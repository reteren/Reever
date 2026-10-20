"""Обучение многоязычного детектора «ривер»: причинная DS-TCN по кадрам log-mel.

Примеры склеиваются на лету из пулов gen_multilang.py: речь или тишина, пауза, wake-слово,
пауза, команда — граница слова известна, поэтому цель задаётся по кадрам: «да» — сразу
после конца слова, «нет» — на обычной речи, созвучных словах и шуме. Аугментации — на GPU:
громкость, высота/темп, шум, эхо комнаты, полоса телефона/Discord.

Языки из --holdout-langs (по умолчанию нидерландский, венгерский, корейский) в обучение не
попадают — на них проверяется, слышит ли детектор «river» на незнакомом языке.

С --fleurs в отрицательные примеры и контекст вокруг слова идёт живая речь носителей
(Google FLEURS, CC-BY 4.0: data/fleurs/<язык>/dev.tsv + dev/*.wav). Часть фраз каждого
языка отложена для замера ложных срабатываний в час на речи, которую сеть не слышала. На
шаге --mine-at сеть прогоняется по живой речи обучения: места ложных срабатываний
становятся трудными отрицательными примерами (их доля — --hard-share).

С --real-wake (data/globe/clips.jsonl, globe_river.py) в пул wake-слова идут живые «river»
дикторов Common Voice с разными акцентами; часть дикторов отложена — это замер на живых
людях по группам акцентов (globe-de, globe-zh, …).

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


# Метка акцента Common Voice → короткая группа для отчёта.
ACCENT_GROUPS = [
    ("india", "india"), ("united states", "us"), ("england", "uk"), ("canad", "us"), ("austral", "uk"),
    ("scot", "uk"), ("irish", "uk"), ("ireland", "uk"), ("new zealand", "uk"), ("wales", "uk"),
    ("german", "de"), ("french", "fr"), ("france", "fr"), ("chin", "zh"), ("mandarin", "zh"),
    ("hong kong", "zh"), ("cantonese", "zh"), ("taiwan", "zh"), ("russia", "ru"), ("ukrain", "ru"),
    ("spanish", "es"), ("spain", "es"), ("mexic", "es"), ("latin", "es"), ("argentin", "es"),
    ("dutch", "nl"), ("netherland", "nl"), ("belgi", "nl"), ("polish", "pl"), ("poland", "pl"),
    ("italian", "it"), ("italy", "it"), ("japan", "ja"), ("korea", "ko"), ("portug", "pt"),
    ("brazil", "pt"), ("swed", "nordic"), ("norw", "nordic"), ("danish", "nordic"), ("finn", "nordic"),
    ("czech", "slavic"), ("slovak", "slavic"), ("croat", "slavic"), ("serb", "slavic"), ("bulgar", "slavic"),
    ("hungar", "hu"), ("greek", "el"), ("roman", "ro"), ("african", "africa"), ("nigeria", "africa"),
    ("kenya", "africa"), ("filipin", "asia"), ("philippin", "asia"), ("malay", "asia"), ("singapore", "asia"),
    ("vietnam", "asia"), ("indonesia", "asia"), ("pakistan", "india"), ("bangla", "india"), ("arab", "arab"),
]


def accent_group(accent: str) -> str:
    accent = (accent or "").lower()
    return next((group for key, group in ACCENT_GROUPS if key in accent), "other")


def globe_rows(path: Path) -> list[dict]:
    """Клипы globe_river.py как строки манифеста: wake-слово, диктор = голос."""
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        item = json.loads(line)
        if item.get("file"):
            rows.append({"file": str((path.parent / item["file"]).resolve()), "kind": "wake",
                         "lang": "globe-" + accent_group(item.get("accent", "")),
                         "voice": "globe:" + item["speaker"], "sid": 0, "text": "river",
                         # Метка начала слова у Parakeet запаздывает (~0,24 с, замер на 30
                         # клипах), конец почти точный.
                         "wake_start": max(0.0, item["wake_start"] - 0.24), "wake_end": item["wake_end"] + 0.04})
    return rows


class Pools:
    def __init__(self, rows: list[dict]):
        self.wake = [(r, read_wav(DATA / r["file"])) for r in rows if r["kind"] == "wake"]
        self.speech = [(r, read_wav(DATA / r["file"])) for r in rows if r["kind"] == "speech"]
        self.confusable = [(r, read_wav(DATA / r["file"])) for r in rows if r["kind"] == "confusable"]
        # Речь того же голоса — чтобы контекст вокруг wake-слова звучал как один человек.
        self.speech_by_voice: dict[tuple, list[np.ndarray]] = defaultdict(list)
        for r, a in self.speech:
            self.speech_by_voice[(r["voice"], r["sid"])].append(a)
        # Живая речь (FLEURS) и трудные отрицательные примеры — int16.
        # Живые окна с «river» внутри (GLOBE): отдельно, их речь вокруг слова — своя.
        self.wake_windows = [(r, a) for r, a in self.wake if r.get("wake_end") is not None]
        self.wake = [(r, a) for r, a in self.wake if r.get("wake_end") is None]
        self.window_share = 0.3
        self.real: list[np.ndarray] = []
        self.hard: list[np.ndarray] = []
        self.hard_share = 0.0


def resample(audio: np.ndarray, factor: float) -> np.ndarray:
    """Меняет высоту и темп сразу (как другой голос), factor > 1 — выше и быстрее."""
    n = max(1, int(len(audio) / factor))
    return np.interp(np.arange(n) * factor, np.arange(len(audio)), audio).astype(np.float32)


def level(audio: np.ndarray) -> np.ndarray:
    peak = float(np.abs(audio).max()) if len(audio) else 0.0
    return audio / peak * 0.5 if peak > 1e-4 else audio


# Фразы с самим словом или созвучием на любом языке — не отрицательный пример.
WAKE_LIKE = ("river", "reever", "ривер", "рівер", "ривър", "riviera", "rivier", "rivière", "rivers")


def load_fleurs(root: Path, per_lang: int, test_per_lang: int, seed: int) -> tuple[list[np.ndarray], list[np.ndarray]]:
    """Живая речь: (обучение, тест) — int16, чтобы десятки часов поместились в память."""
    rng = random.Random(seed)
    train, test = [], []
    for lang_dir in sorted(d for d in root.iterdir() if (d / "dev.tsv").is_file()):
        rows = []
        for line in (lang_dir / "dev.tsv").read_text(encoding="utf-8").splitlines():
            cols = line.split("\t")
            if len(cols) < 3:
                continue
            text = cols[2].lower()
            path = lang_dir / "dev" / cols[1]
            if path.is_file() and not any(word in text for word in WAKE_LIKE):
                rows.append(path)
        rows = sorted(set(rows))
        rng.shuffle(rows)
        for path in rows[:test_per_lang]:
            test.append(read_wav_i16(path))
        for path in rows[test_per_lang:test_per_lang + per_lang]:
            train.append(read_wav_i16(path))
    return train, test


def read_wav_i16(path: Path) -> np.ndarray:
    """Любой WAV (в FLEURS — float32) → int16 16 кГц mono."""
    import soundfile

    audio, rate = soundfile.read(str(path), dtype="float32", always_2d=True)
    audio = audio.mean(axis=1)
    if rate != RATE:
        audio = resample(audio, rate / RATE)
    return (np.clip(audio, -1.0, 1.0) * 32767).astype("<i2")


def real_piece(real: list[np.ndarray], rng: random.Random, seconds: float) -> np.ndarray:
    clip = rng.choice(real)
    n = min(len(clip), int(seconds * RATE))
    start = rng.randrange(0, max(1, len(clip) - n))
    return level(clip[start:start + n].astype(np.float32) / 32768.0)


def compose(pools: Pools, rng: random.Random, positive: bool) -> tuple[np.ndarray, int, int]:
    """Пример длиной SEG и (начало, конец) wake-слова в сэмплах; для отрицательного — (-1, -1)."""
    total = int(SEG * RATE)
    parts: list[np.ndarray] = []
    start = end = -1
    if positive and pools.wake_windows and rng.random() < pools.window_share:
        # Живая фраза с «river» внутри: речь вокруг слова уже настоящая.
        row, clip = rng.choice(pools.wake_windows)
        factor = rng.uniform(0.92, 1.08)
        clip = level(resample(clip, factor))
        pre = np.zeros(int(rng.uniform(0.0, 0.6) * RATE), np.float32)
        audio = np.concatenate([pre, clip, np.zeros(int(0.5 * RATE), np.float32)])
        start = len(pre) + int(row["wake_start"] * RATE / factor)
        end = len(pre) + int(row["wake_end"] * RATE / factor)
        want_end = int(rng.uniform(0.7, SEG - 0.3) * RATE)
        shift = end - want_end
        audio = np.concatenate([np.zeros(-shift, np.float32), audio]) if shift < 0 else audio[shift:]
        start, end = start - shift, end - shift
        parts = [audio]
    elif positive:
        row, wake = rng.choice(pools.wake)
        wake = level(resample(wake, rng.uniform(0.88, 1.12)))
        own = pools.speech_by_voice.get((row["voice"], row["sid"])) or [a for _, a in rng.sample(pools.speech, 1)]
        # До слова: тишина или конец чужой фразы (обращение посреди разговора).
        if rng.random() < 0.45:
            pre = np.zeros(int(rng.uniform(0.1, 1.4) * RATE), np.float32)
        else:
            speech = real_piece(pools.real, rng, 1.5) if pools.real and rng.random() < 0.5 else level(rng.choice(own))
            pre = np.concatenate([speech[-int(rng.uniform(0.3, 1.3) * RATE):],
                                  np.zeros(int(rng.uniform(0.0, 0.4) * RATE), np.float32)])
        post_gap = np.zeros(int(rng.uniform(0.04, 0.45) * RATE), np.float32)
        if rng.random() < 0.4:
            post = np.zeros(int(0.8 * RATE), np.float32)
        elif pools.real and rng.random() < 0.5:
            post = real_piece(pools.real, rng, 1.5)
        else:
            post = level(rng.choice(own))
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
    elif pools.hard and rng.random() < pools.hard_share:
        # Место, где сеть уже ошиблась на живой речи.
        audio = level(rng.choice(pools.hard).astype(np.float32) / 32768.0)
    elif pools.real and rng.random() < 0.55:
        # Живая речь: подряд куски разных носителей, иногда с созвучным словом внутри.
        while sum(len(p) for p in parts) < total + RATE:
            parts.append(real_piece(pools.real, rng, rng.uniform(1.0, 3.0)))
            if rng.random() < 0.2:
                parts.append(level(resample(rng.choice(pools.confusable)[1], rng.uniform(0.88, 1.12))))
            parts.append(np.zeros(int(rng.uniform(0.0, 0.3) * RATE), np.float32))
        audio = np.concatenate(parts)
        audio = audio[rng.randrange(0, max(1, len(audio) - total)):]
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
        for row, clip in pools.wake_windows:
            audio = np.concatenate([np.zeros(int(0.6 * RATE), np.float32), level(clip), np.zeros(int(0.5 * RATE), np.float32)])
            feats = mel(torch.from_numpy(audio).to(device)[None])
            scores = torch.sigmoid(model(feats))[0].cpu().numpy()
            s = (int((0.6 + row["wake_start"]) * RATE) - WIN // 2) // HOP
            e = (int((0.6 + row["wake_end"]) * RATE) - WIN // 2) // HOP
            by_lang[row["lang"]].append((scores, s, e))
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


def real_false(model: CausalTcn, mel: LogMel, real: list[np.ndarray], device: torch.device, thresholds) -> dict:
    """Ложные срабатывания в час на сплошном потоке живой речи."""
    model.eval()
    stream = np.concatenate([np.concatenate([c.astype(np.float32) / 32768.0, np.zeros(int(0.2 * RATE), np.float32)]) for c in real])
    scores = []
    with torch.no_grad():
        for chunk in range(0, len(stream), RATE * 60):
            piece = stream[max(0, chunk - RATE):chunk + RATE * 60]
            sc = torch.sigmoid(model(mel(torch.from_numpy(piece).to(device)[None])))[0].cpu().numpy()
            scores.append(sc[(RATE // HOP if chunk else 0):])
    scores = np.concatenate(scores)
    hours = len(stream) / RATE / 3600
    model.train()
    return {"real_hours": round(hours, 2), **{f"real_false_per_hour@{th}": round(len(events(scores, th)) / hours, 1) for th in thresholds}}


def mine_hard(model: CausalTcn, mel: LogMel, real: list[np.ndarray], device: torch.device, threshold: float = 0.5) -> list[np.ndarray]:
    """Куски живой речи по SEG секунд, которые кончаются на ложном срабатывании."""
    model.eval()
    total = int(SEG * RATE)
    hard = []
    with torch.no_grad():
        for clip in real:
            audio = clip.astype(np.float32) / 32768.0
            if len(audio) < WIN:
                continue
            scores = torch.sigmoid(model(mel(torch.from_numpy(audio).to(device)[None])))[0].cpu().numpy()
            for frame in events(scores, threshold, release=60):
                end = min(len(audio), frame * HOP + WIN + int(0.15 * RATE))
                piece = audio[max(0, end - total):end]
                piece = np.concatenate([np.zeros(total - len(piece), np.float32), piece])
                hard.append((piece * 32767).astype("<i2"))
    model.train()
    return hard


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
    p.add_argument("--fleurs", type=Path, help="data/fleurs: живая речь носителей")
    p.add_argument("--fleurs-per-lang", type=int, default=150)
    p.add_argument("--fleurs-test-per-lang", type=int, default=40)
    p.add_argument("--mine-at", default="", help="шаги добора трудных примеров, через запятую")
    p.add_argument("--hard-share", type=float, default=0.25)
    p.add_argument("--real-wake", type=Path, help="data/globe/clips.jsonl: живые «river»")
    p.add_argument("--eval-only", type=Path, help="только замер готовых весов (.pt) на том же разбиении")
    p.add_argument("--real-wake-test", type=float, default=0.15, help="доля дикторов GLOBE в тест")
    p.add_argument("--real-wake-repeat", type=int, default=3, help="во сколько раз чаще брать живые клипы")
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
    if args.real_wake:
        real_rows = globe_rows(args.real_wake)
        speakers = sorted({r["voice"] for r in real_rows})
        held_globe = set(rng.sample(speakers, int(len(speakers) * args.real_wake_test)))
        for r in real_rows:
            if r["voice"] in held_globe:
                held_sids.add((r["voice"], r["sid"]))
        # Живых клипов меньше синтетических — в обучении их берут чаще.
        train_real = [r for r in real_rows if r["voice"] not in held_globe]
        rows += [r for r in real_rows if r["voice"] in held_globe] + train_real * args.real_wake_repeat
        print(f"живые «river»: {len(real_rows)} клипов, {len(speakers)} дикторов, в тест {len(held_globe)}")
    is_test = lambda r: r["lang"] in holdout_langs or (r["voice"], r["sid"]) in held_sids
    train_pools = Pools([r for r in rows if not is_test(r)])
    test_pools = Pools([r for r in rows if is_test(r)])
    print(f"train: wake {len(train_pools.wake)}, speech {len(train_pools.speech)}, confusable {len(train_pools.confusable)}")
    print(f"test:  wake {len(test_pools.wake)}, speech {len(test_pools.speech)}, confusable {len(test_pools.confusable)}; языки без обучения: {sorted(holdout_langs)}")
    real_test: list[np.ndarray] = []
    if args.fleurs:
        train_pools.real, real_test = load_fleurs(args.fleurs, args.fleurs_per_lang, args.fleurs_test_per_lang, args.seed)
        hours = lambda clips: sum(len(c) for c in clips) / RATE / 3600
        print(f"живая речь: обучение {len(train_pools.real)} фраз ({hours(train_pools.real):.1f} ч), тест {len(real_test)} ({hours(real_test):.1f} ч)")
    mine_steps = {int(x) for x in args.mine_at.split(",") if x}

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    mel, augment = LogMel().to(device), Augment().to(device)
    model = CausalTcn(args.channels).to(device)
    print(f"параметров {sum(p.numel() for p in model.parameters())}, контекст {model.receptive_field()} кадров")
    if args.eval_only:
        model.load_state_dict(torch.load(args.eval_only, map_location=device))
        report = evaluate(model, mel, test_pools, device, (0.85, 0.93, 0.97))
        if real_test:
            report.update(real_false(model, mel, real_test, device, (0.85, 0.93, 0.97)))
        print(json.dumps(report, ensure_ascii=False))
        return
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
        if step in mine_steps and train_pools.real:
            found = mine_hard(model, mel, train_pools.real, device)
            train_pools.hard = (train_pools.hard + found)[-6000:]
            train_pools.hard_share = args.hard_share
            print(f"шаг {step}: трудных примеров +{len(found)}, всего {len(train_pools.hard)}", flush=True)
        if step % args.eval_every == 0 or step == args.steps:
            report = evaluate(model, mel, test_pools, device)
            if real_test:
                report.update(real_false(model, mel, real_test, device, (0.5, 0.7, 0.85, 0.93, 0.97)))
            print(json.dumps(report, ensure_ascii=False), flush=True)
            torch.save(model.state_dict(), args.output.with_suffix(".pt"))
    export(model, args.output)
    print(f"веса: {args.output} ({args.output.stat().st_size} байт)")


if __name__ == "__main__":
    main()
