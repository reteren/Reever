"""Обучение многоязычного детектора «ривер»: причинная DS-TCN по кадрам log-mel.

Примеры склеиваются на лету из пулов gen_multilang.py: речь или тишина, пауза, wake-слово,
пауза, команда — граница слова известна, поэтому цель задаётся по кадрам: «да» — сразу
после конца слова, «нет» — на обычной речи, созвучных словах и шуме. Аугментации — на GPU:
громкость, высота/темп, шум, эхо комнаты, полоса телефона/Discord.

Языки из --holdout-langs (по умолчанию нидерландский, венгерский, корейский) в обучение не
попадают — на них проверяется, слышит ли детектор «river» на незнакомом языке.

Признаки совпадают с src-tauri/src/kws.rs: окно Ханна 400, шаг 160, FFT 512, 40 мел, log.

    .venv\\Scripts\\python train_multilang.py --steps 30000 --output data/kws-ml.bin
"""

from __future__ import annotations

import argparse
import json
import math
import random
import struct
import time
import wave
from collections import defaultdict
from pathlib import Path

import numpy as np
import torch
from torch import nn
from torch.nn import functional as F

RATE = 16_000
WIN, HOP, FFT, MELS = 400, 160, 512, 40
SEG = 2.0  # секунд в обучающем примере
MAGIC = b"RKW2"
DATA = Path(__file__).resolve().parent / "data" / "ml"


def mel_filters() -> np.ndarray:
    def hz_to_mel(hz: float) -> float:
        return 2595.0 * math.log10(1.0 + hz / 700.0)

    def mel_to_hz(mel: float) -> float:
        return 700.0 * (10.0 ** (mel / 2595.0) - 1.0)

    points = np.linspace(hz_to_mel(20), hz_to_mel(7600), MELS + 2)
    bins_hz = np.arange(FFT // 2 + 1, dtype=np.float64) * RATE / FFT
    edges = [mel_to_hz(float(point)) for point in points]
    bank = np.zeros((MELS, FFT // 2 + 1), dtype=np.float32)
    for m in range(MELS):
        left, center, right = edges[m:m + 3]
        bank[m] = np.maximum(0.0, np.minimum((bins_hz - left) / (center - left), (right - bins_hz) / (right - center)))
    return bank


class LogMel(nn.Module):
    """Как kws.rs: симметричное окно Ханна (np.hanning), нули до 512, |rfft|², мел, log."""

    def __init__(self):
        super().__init__()
        self.register_buffer("window", torch.from_numpy(np.hanning(WIN).astype(np.float32)))
        self.register_buffer("bank", torch.from_numpy(mel_filters()))

    def forward(self, audio: torch.Tensor) -> torch.Tensor:  # [N, S] -> [N, MELS, T]
        frames = audio.unfold(1, WIN, HOP) * self.window
        spectrum = torch.fft.rfft(frames, n=FFT)
        power = spectrum.real ** 2 + spectrum.imag ** 2
        return torch.log(power @ self.bank.T + 1e-10).transpose(1, 2)


class Block(nn.Module):
    def __init__(self, channels: int, kernel: int, dilation: int):
        super().__init__()
        self.pad = (kernel - 1) * dilation
        self.depth = nn.Conv1d(channels, channels, kernel, dilation=dilation, groups=channels)
        self.bn1 = nn.BatchNorm1d(channels)
        self.point = nn.Conv1d(channels, channels, 1)
        self.bn2 = nn.BatchNorm1d(channels)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        y = F.relu(self.bn1(self.depth(F.pad(x, (self.pad, 0)))))
        return F.relu(x + self.bn2(self.point(y)))


class CausalTcn(nn.Module):
    """Только прошлые кадры: в потоке каждый новый кадр считается за O(параметров)."""

    def __init__(self, channels: int = 64, kernel: int = 5, dilations=(1, 2, 4, 8, 1, 2)):
        super().__init__()
        self.channels, self.kernel, self.dilations = channels, kernel, tuple(dilations)
        self.norm = nn.BatchNorm1d(MELS)
        self.pre = nn.Conv1d(MELS, channels, 1)
        self.bn = nn.BatchNorm1d(channels)
        self.blocks = nn.ModuleList(Block(channels, kernel, d) for d in dilations)
        self.head = nn.Conv1d(channels, 1, 1)

    def forward(self, feats: torch.Tensor) -> torch.Tensor:  # [N, MELS, T] -> [N, T] логиты
        x = F.relu(self.bn(self.pre(self.norm(feats))))
        for block in self.blocks:
            x = block(x)
        return self.head(x).squeeze(1)

    def receptive_field(self) -> int:
        return 1 + (self.kernel - 1) * sum(self.dilations)


# ── Данные ──────────────────────────────────────────────────────────────

def read_wav(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as wav:
        raw = wav.readframes(wav.getnframes())
    return np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0


class Pools:
    def __init__(self, rows: list[dict]):
        self.wake = [(r, read_wav(DATA / r["file"])) for r in rows if r["kind"] == "wake"]
        self.speech = [(r, read_wav(DATA / r["file"])) for r in rows if r["kind"] == "speech"]
        self.confusable = [(r, read_wav(DATA / r["file"])) for r in rows if r["kind"] == "confusable"]
        # Речь того же голоса — чтобы контекст вокруг wake-слова звучал как один человек.
        self.speech_by_voice: dict[tuple, list[np.ndarray]] = defaultdict(list)
        for r, a in self.speech:
            self.speech_by_voice[(r["voice"], r["sid"])].append(a)


def resample(audio: np.ndarray, factor: float) -> np.ndarray:
    """Меняет высоту и темп сразу (как другой голос), factor > 1 — выше и быстрее."""
    n = max(1, int(len(audio) / factor))
    return np.interp(np.arange(n) * factor, np.arange(len(audio)), audio).astype(np.float32)


def level(audio: np.ndarray) -> np.ndarray:
    peak = float(np.abs(audio).max()) if len(audio) else 0.0
    return audio / peak * 0.5 if peak > 1e-4 else audio


def compose(pools: Pools, rng: random.Random, positive: bool) -> tuple[np.ndarray, int, int]:
    """Пример длиной SEG и (начало, конец) wake-слова в сэмплах; для отрицательного — (-1, -1)."""
    total = int(SEG * RATE)
    parts: list[np.ndarray] = []
    start = end = -1
    if positive:
        row, wake = rng.choice(pools.wake)
        wake = level(resample(wake, rng.uniform(0.88, 1.12)))
        own = pools.speech_by_voice.get((row["voice"], row["sid"])) or [a for _, a in rng.sample(pools.speech, 1)]
        # До слова: тишина или конец чужой фразы (обращение посреди разговора).
        if rng.random() < 0.55:
            pre = np.zeros(int(rng.uniform(0.1, 1.4) * RATE), np.float32)
        else:
            speech = level(rng.choice(own))
            pre = np.concatenate([speech[-int(rng.uniform(0.3, 1.3) * RATE):],
                                  np.zeros(int(rng.uniform(0.0, 0.4) * RATE), np.float32)])
        post_gap = np.zeros(int(rng.uniform(0.04, 0.45) * RATE), np.float32)
        post = level(rng.choice(own)) if rng.random() < 0.6 else np.zeros(int(0.8 * RATE), np.float32)
        audio = np.concatenate([pre, wake, post_gap, post])
        start, end = len(pre), len(pre) + len(wake)
        # Конец слова — в пределах [0,7 c; SEG − 0,3 c] примера.
        want_end = int(rng.uniform(0.7, SEG - 0.3) * RATE)
        shift = end - want_end
        if shift < 0:
            audio = np.concatenate([np.zeros(-shift, np.float32), audio])
        else:
            audio = audio[shift:]
        start, end = start - shift, end - shift
        parts = [audio]
    else:
        while sum(len(p) for p in parts) < total:
            pick = rng.random()
            if pick < 0.6:
                parts.append(level(resample(rng.choice(pools.speech)[1], rng.uniform(0.9, 1.1))))
            elif pick < 0.85:
                parts.append(level(resample(rng.choice(pools.confusable)[1], rng.uniform(0.88, 1.12))))
            else:
                parts.append(np.zeros(int(rng.uniform(0.1, 0.8) * RATE), np.float32))
            parts.append(np.zeros(int(rng.uniform(0.0, 0.35) * RATE), np.float32))
        audio = np.concatenate(parts)
        offset = rng.randrange(0, max(1, len(audio) - total))
        audio = audio[offset:]
    audio = audio[:total]
    if len(audio) < total:
        audio = np.concatenate([audio, np.zeros(total - len(audio), np.float32)])
    return audio, start, end


def targets(start: int, end: int, frames: int) -> tuple[np.ndarray, np.ndarray]:
    """Цель и маска по кадрам: 1 сразу после конца слова, неважно — внутри слова и сразу за
    окном срабатывания, 0 — везде ещё."""
    target = np.zeros(frames, np.float32)
    mask = np.ones(frames, np.float32)
    if end >= 0:
        frame_of = lambda sample: (sample - WIN // 2) // HOP
        s, e = frame_of(start), frame_of(end)
        mask[max(0, s):max(0, e - 4)] = 0
        target[max(0, e - 4):max(0, e + 18)] = 1
        mask[max(0, e + 18):max(0, e + 60)] = 0
    return target, mask


class Augment(nn.Module):
    """На GPU: громкость, шум с SNR, эхо, полоса пропускания (микрофон, телефон, Discord)."""

    def forward(self, audio: torch.Tensor) -> torch.Tensor:
        n, s = audio.shape
        dev = audio.device
        audio = audio * torch.empty(n, 1, device=dev).uniform_(0.05, 1.6)
        # Эхо: экспоненциально затухающий шум как импульс комнаты.
        rir_len = 4000
        decay = torch.empty(n, 1, device=dev).uniform_(0.01, 0.12)
        t = torch.arange(rir_len, device=dev) / RATE
        rir = torch.randn(n, rir_len, device=dev) * torch.exp(-t / decay)
        rir[:, 0] = 1.0
        rir = rir / rir.norm(dim=1, keepdim=True)
        wet = torch.fft.irfft(torch.fft.rfft(audio, n=s + rir_len) * torch.fft.rfft(rir, n=s + rir_len), n=s + rir_len)[:, :s]
        use_rir = (torch.rand(n, 1, device=dev) < 0.45).float()
        audio = use_rir * wet * (audio.norm(dim=1, keepdim=True) / (wet.norm(dim=1, keepdim=True) + 1e-6)) + (1 - use_rir) * audio
        # Полоса: случайный срез низов и верхов в частотной области.
        spec = torch.fft.rfft(audio)
        freqs = torch.fft.rfftfreq(s, 1 / RATE).to(dev)
        low = torch.where(torch.rand(n, 1, device=dev) < 0.4, torch.empty(n, 1, device=dev).uniform_(80, 400), torch.zeros(n, 1, device=dev))
        high = torch.where(torch.rand(n, 1, device=dev) < 0.4, torch.empty(n, 1, device=dev).uniform_(3200, 7000), torch.full((n, 1), 8000.0, device=dev))
        audio = torch.fft.irfft(spec * ((freqs >= low) & (freqs <= high)).float(), n=s)
        # Шум: белый или розоватый, SNR 0–35 дБ, иногда без шума.
        rms = audio.pow(2).mean(dim=1, keepdim=True).sqrt().clamp_min(3e-4)
        snr = torch.empty(n, 1, device=dev).uniform_(0, 35)
        noise = torch.randn(n, s, device=dev)
        pink = torch.rand(n, 1, device=dev) < 0.5
        noise = torch.where(pink, torch.cumsum(noise, dim=1) * 0.02, noise)
        noise = noise - noise.mean(dim=1, keepdim=True)
        noise = noise / noise.pow(2).mean(dim=1, keepdim=True).sqrt().clamp_min(1e-6)
        keep = (torch.rand(n, 1, device=dev) < 0.85).float()
        audio = audio + keep * noise * rms / (10 ** (snr / 20))
        return audio.clamp(-1, 1)


# ── Оценка ──────────────────────────────────────────────────────────────

def events(scores: np.ndarray, threshold: float, release: int = 30) -> list[int]:
    """Срабатывание — 2 кадра подряд выше порога; снова — после `release` кадров ниже 0,6·порога."""
    out, fired, quiet, run = [], False, 0, 0
    for i, score in enumerate(scores):
        if fired:
            quiet = quiet + 1 if score < threshold * 0.6 else 0
            if quiet >= release:
                fired, quiet = False, 0
            continue
        run = run + 1 if score >= threshold else 0
        if run >= 2:
            out.append(i)
            fired, run = True, 0
    return out


def evaluate(model: CausalTcn, mel: LogMel, pools: Pools, device: torch.device, thresholds=(0.5, 0.7, 0.85, 0.93)) -> dict:
    model.eval()
    rng = random.Random(5)
    by_lang: dict[str, list] = defaultdict(list)
    with torch.no_grad():
        for row, wake in pools.wake:
            own = pools.speech_by_voice.get((row["voice"], row["sid"]))
            for with_command in (False, True):
                pre = np.zeros(int(0.6 * RATE), np.float32)
                post = level(rng.choice(own)) if with_command and own else np.zeros(int(1.0 * RATE), np.float32)
                audio = np.concatenate([pre, level(wake), np.zeros(int(0.2 * RATE), np.float32), post])
                feats = mel(torch.from_numpy(audio).to(device)[None])
                scores = torch.sigmoid(model(feats))[0].cpu().numpy()
                e = (len(pre) + len(wake) - WIN // 2) // HOP
                s = (len(pre) - WIN // 2) // HOP
                by_lang[row["lang"]].append((scores, s, e))
        stream = np.concatenate([np.concatenate([level(a), np.zeros(int(0.25 * RATE), np.float32)])
                                 for _, a in pools.speech + pools.confusable])
        neg_scores = []
        for chunk in range(0, len(stream), RATE * 60):
            piece = stream[max(0, chunk - RATE):chunk + RATE * 60]
            feats = mel(torch.from_numpy(piece).to(device)[None])
            sc = torch.sigmoid(model(feats))[0].cpu().numpy()
            neg_scores.append(sc[(RATE // HOP if chunk else 0):])
        neg_scores = np.concatenate(neg_scores)
    hours = len(stream) / RATE / 3600
    result = {"negative_hours": round(hours, 3)}
    for th in thresholds:
        per_lang = {}
        for lang, items in sorted(by_lang.items()):
            hit = sum(any(s - 2 <= f <= e + 40 for f in events(sc, th)) for sc, s, e in items)
            per_lang[lang] = f"{hit}/{len(items)}"
        result[f"th={th}"] = {"recall": per_lang, "false_per_hour": round(len(events(neg_scores, th)) / hours, 1)}
    model.train()
    return result


# ── Экспорт ─────────────────────────────────────────────────────────────

def fold(conv_w: torch.Tensor, conv_b: torch.Tensor, bn: nn.BatchNorm1d) -> tuple[torch.Tensor, torch.Tensor]:
    scale = bn.weight / torch.sqrt(bn.running_var + bn.eps)
    w = conv_w * scale.view(-1, *([1] * (conv_w.dim() - 1)))
    b = (conv_b - bn.running_mean) * scale + bn.bias
    return w, b


def export(model: CausalTcn, path: Path) -> None:
    """RKW2: заголовок (каналы, ядро, число блоков, дилатации) и веса f32 LE с BN, вшитой в
    свёртки. Порядок совпадает с kws.rs: norm∘pre, затем блоки (depth, point), head."""
    model = model.cpu().eval()
    with torch.no_grad():
        # Нормализация входа (BN по мелам) вшивается в pre: pre(norm(x)).
        n = model.norm
        scale = n.weight / torch.sqrt(n.running_var + n.eps)
        shift = n.bias - n.running_mean * scale
        pre_w = model.pre.weight[:, :, 0] * scale[None, :]
        pre_b = model.pre.bias + model.pre.weight[:, :, 0] @ shift
        pre_w, pre_b = fold(pre_w, pre_b, model.bn)
        tensors = [pre_w, pre_b]
        for block in model.blocks:
            dw, db = fold(block.depth.weight[:, 0, :], block.depth.bias, block.bn1)
            pw, pb = fold(block.point.weight[:, :, 0], block.point.bias, block.bn2)
            tensors += [dw, db, pw, pb]
        tensors += [model.head.weight[0, :, 0], model.head.bias]
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<IIII", MELS, model.channels, model.kernel, len(model.dilations)))
        f.write(struct.pack(f"<{len(model.dilations)}I", *model.dilations))
        for tensor in tensors:
            f.write(tensor.detach().contiguous().numpy().astype("<f4").tobytes())


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--steps", type=int, default=30000)
    p.add_argument("--batch", type=int, default=256)
    p.add_argument("--channels", type=int, default=64)
    p.add_argument("--holdout-langs", default="nl,hu,ko")
    p.add_argument("--holdout-share", type=float, default=0.15, help="доля дикторов многоголосых моделей в тест")
    p.add_argument("--output", type=Path, default=Path("data/kws-ml.bin"))
    p.add_argument("--eval-every", type=int, default=5000)
    p.add_argument("--seed", type=int, default=11)
    args = p.parse_args()
    random.seed(args.seed); np.random.seed(args.seed); torch.manual_seed(args.seed)

    rows = [json.loads(line) for line in (DATA / "manifest.jsonl").read_text(encoding="utf-8").splitlines()]
    holdout_langs = set(filter(None, args.holdout_langs.split(",")))
    rng = random.Random(args.seed)
    multi = defaultdict(set)
    for r in rows:
        multi[r["voice"]].add(r["sid"])
    held_sids = {(v, sid) for v, sids in multi.items() if len(sids) > 3
                 for sid in rng.sample(sorted(sids), max(1, int(len(sids) * args.holdout_share)))}
    is_test = lambda r: r["lang"] in holdout_langs or (r["voice"], r["sid"]) in held_sids
    train_pools = Pools([r for r in rows if not is_test(r)])
    test_pools = Pools([r for r in rows if is_test(r)])
    print(f"train: wake {len(train_pools.wake)}, speech {len(train_pools.speech)}, confusable {len(train_pools.confusable)}")
    print(f"test:  wake {len(test_pools.wake)}, speech {len(test_pools.speech)}, confusable {len(test_pools.confusable)}; языки без обучения: {sorted(holdout_langs)}")

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    mel, augment = LogMel().to(device), Augment().to(device)
    model = CausalTcn(args.channels).to(device)
    print(f"параметров {sum(p.numel() for p in model.parameters())}, контекст {model.receptive_field()} кадров")
    optim = torch.optim.AdamW(model.parameters(), lr=3e-3, weight_decay=1e-4)
    sched = torch.optim.lr_scheduler.OneCycleLR(optim, max_lr=3e-3, total_steps=args.steps, pct_start=0.05)
    frames = (int(SEG * RATE) - WIN) // HOP + 1
    started = time.time()
    for step in range(1, args.steps + 1):
        batch, tgt, msk = [], [], []
        for i in range(args.batch):
            audio, s, e = compose(train_pools, rng, positive=i % 2 == 0)
            t, m = targets(s, e, frames)
            batch.append(audio); tgt.append(t); msk.append(m)
        audio = torch.from_numpy(np.stack(batch)).to(device)
        target = torch.from_numpy(np.stack(tgt)).to(device)
        mask = torch.from_numpy(np.stack(msk)).to(device)
        with torch.no_grad():
            feats = mel(augment(audio))
        logits = model(feats)
        loss = (F.binary_cross_entropy_with_logits(logits, target, reduction="none", pos_weight=torch.tensor(4.0, device=device)) * mask).sum() / mask.sum()
        optim.zero_grad(); loss.backward(); optim.step(); sched.step()
        if step % 200 == 0:
            print(f"step {step}/{args.steps} loss {loss.item():.4f} {time.time() - started:.0f} c", flush=True)
        if step % args.eval_every == 0 or step == args.steps:
            print(json.dumps(evaluate(model, mel, test_pools, device), ensure_ascii=False), flush=True)
            torch.save(model.state_dict(), args.output.with_suffix(".pt"))
    export(model, args.output)
    print(f"веса: {args.output} ({args.output.stat().st_size} байт)")


if __name__ == "__main__":
    main()
