"""Обучение и оценка небольшого DS-CNN для wake-слова «ривер»."""

from __future__ import annotations

import argparse
import csv
import math
import random
import wave
from pathlib import Path

import numpy as np
import torch
from torch import nn
from torch.nn import functional as F
from torch.utils.data import DataLoader, Dataset

RATE = 16_000
WIN = 400
HOP = 160
FFT = 512
MELS = 40
FRAMES = 60
POOL_BINS = 6
MAGIC = b"RKW1"


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


MEL_BANK = mel_filters()
HANN = np.hanning(WIN).astype(np.float32)


def log_mel(samples: np.ndarray) -> np.ndarray:
    """Та же Hann/FFT/мел-фильтрация реализована в kws.rs без дополнительных crate."""
    frames: list[np.ndarray] = []
    for start in range(0, max(0, len(samples) - WIN + 1), HOP):
        frame = np.zeros(FFT, dtype=np.float32)
        frame[:WIN] = samples[start:start + WIN] * HANN
        spectrum = np.fft.rfft(frame)
        power = (spectrum.real * spectrum.real + spectrum.imag * spectrum.imag).astype(np.float32)
        energy = MEL_BANK @ power
        frames.append(np.log(energy + 1e-10).astype(np.float32))
    if not frames:
        return np.zeros((0, MELS), dtype=np.float32)
    return np.stack(frames)


def read_wav(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as wav:
        channels, rate, width = wav.getnchannels(), wav.getframerate(), wav.getsampwidth()
        raw = wav.readframes(wav.getnframes())
    if width != 2:
        raise ValueError(f"Ожидался PCM16 WAV: {path}")
    audio = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    if channels > 1:
        audio = audio.reshape(-1, channels).mean(axis=1)
    if rate != RATE:
        new_len = round(len(audio) * RATE / rate)
        audio = np.interp(np.arange(new_len) * rate / RATE, np.arange(len(audio)), audio).astype(np.float32)
    return audio


def augment(audio: np.ndarray, rng: np.random.Generator) -> np.ndarray:
    audio = audio.copy()
    audio *= float(rng.uniform(0.35, 1.7))
    if rng.random() < 0.85:
        # Шум с SNR 5..30 dB. Тишина получает небольшой ненулевой фон.
        rms = float(np.sqrt(np.mean(audio * audio) + 1e-10))
        snr = float(rng.uniform(5.0, 30.0))
        noise_rms = max(rms, 0.003) / (10.0 ** (snr / 20.0))
        audio += rng.normal(0.0, noise_rms, size=len(audio)).astype(np.float32)
    if rng.random() < 0.35 and len(audio) > 1:
        # Три отражения на 20/50/100 мс как простой room impulse response.
        tail = np.zeros_like(audio)
        for delay, gain in ((320, 0.22), (800, 0.12), (1600, 0.06)):
            if len(audio) > delay:
                tail[delay:] += audio[:-delay] * gain
        audio += tail
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    if peak > 0.98:
        audio *= 0.98 / peak
    return audio.astype(np.float32)


class AudioWindows(Dataset):
    def __init__(self, rows: list[dict[str, str]], root: Path, train: bool):
        self.rows, self.root, self.train = rows, root, train
        self.rng = np.random.default_rng(731 if train else 191)

    def __len__(self) -> int:
        return len(self.rows)

    def __getitem__(self, index: int) -> tuple[torch.Tensor, torch.Tensor]:
        row = self.rows[index]
        rel = row["file"]
        audio = read_wav(self.root / rel)
        if self.train:
            audio = augment(audio, self.rng)
        feats = log_mel(audio)
        label = int(row["label"])
        n = len(feats)
        if label == 1:
            kw_start = max(0, int(float(row.get("keyword_start_s", "0")) * RATE / HOP))
            kw_end = max(kw_start + 1, int(float(row.get("keyword_end_s", "0.35")) * RATE / HOP))
            center = (kw_start + kw_end) // 2
            # Слово перемещается по временному окну, как при обращении посреди фразы.
            place = int(self.rng.integers(8, FRAMES - 2)) if self.train else FRAMES // 2
            start = center - place
        else:
            start = int(self.rng.integers(0, max(1, n - FRAMES + 1))) if self.train else 0
        clip = np.full((FRAMES, MELS), np.log(1e-10), dtype=np.float32)
        src_begin, src_end = max(0, start), min(n, start + FRAMES)
        if src_end > src_begin:
            clip[src_begin - start:src_end - start] = feats[src_begin:src_end]
        return torch.from_numpy(clip.T.copy()), torch.tensor(label, dtype=torch.long)


class TinyDsCnn(nn.Module):
    """Framewise projection, depthwise temporal conv, pointwise conv, pooled head."""

    def __init__(self):
        super().__init__()
        self.pre = nn.Conv1d(MELS, 8, kernel_size=1)
        self.depth = nn.Conv1d(8, 8, kernel_size=5, padding=2, groups=8)
        self.point = nn.Conv1d(8, 12, kernel_size=1)
        self.head = nn.Linear(12 * POOL_BINS, 2)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        x = F.relu(self.pre(x))
        x = F.relu(self.point(F.relu(self.depth(x))))
        x = F.adaptive_avg_pool1d(x, POOL_BINS).flatten(1)
        return self.head(x)


def serialize(model: TinyDsCnn, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as f:
        f.write(MAGIC)
        for name in ("pre.weight", "pre.bias", "depth.weight", "depth.bias", "point.weight", "point.bias", "head.weight", "head.bias"):
            values = model.state_dict()[name].detach().cpu().numpy().astype("<f4").ravel()
            f.write(values.tobytes())


def load_rows(manifest: Path) -> list[dict[str, str]]:
    with manifest.open(encoding="utf-8-sig", newline="") as f:
        return list(csv.DictReader(f))


def stream_scores(model: TinyDsCnn, feats: np.ndarray, device: torch.device) -> np.ndarray:
    model.eval()
    if len(feats) < FRAMES:
        return np.zeros(0, dtype=np.float32)
    windows = np.stack([feats[start:start + FRAMES].T for start in range(len(feats) - FRAMES + 1)])
    scores: list[np.ndarray] = []
    with torch.no_grad():
        for start in range(0, len(windows), 256):
            batch = torch.from_numpy(windows[start:start + 256]).to(device)
            scores.append(model(batch).softmax(dim=1)[:, 1].cpu().numpy())
    return np.concatenate(scores)


def first_smoothed(scores: np.ndarray, threshold: float) -> int | None:
    for i in range(2, len(scores)):
        if sum(float(value) >= threshold for value in scores[i - 2:i + 1]) >= 2:
            return i
    return None


def evaluate_files(model: TinyDsCnn, rows: list[dict[str, str]], root: Path, device: torch.device) -> dict[str, object]:
    results = []
    # В реальном фоновом потоке до вызова уже есть 600 мс контекста.
    pre_roll = np.zeros(round(0.6 * RATE), dtype=np.float32)
    for row in rows:
        audio = read_wav(root / row["file"])
        scores = stream_scores(model, log_mel(np.concatenate((pre_roll, audio))), device)
        results.append((row, scores, len(audio) / RATE))
    thresholds = {}
    for threshold in (0.35, 0.5, 0.65, 0.8):
        positives = [item for item in results if int(item[0]["label"]) == 1]
        negatives = [item for item in results if int(item[0]["label"]) == 0]
        detected, quick_detected, latencies = 0, 0, []
        false_files = 0
        for row, scores, _duration in positives:
            kw_start = float(row.get("keyword_start_s", "0"))
            kw_end = float(row.get("keyword_end_s", "0"))
            frame = first_smoothed(scores, threshold)
            if frame is not None:
                # score-index 0 covers audio through 0.615 s; each next score is 10 ms later.
                detected_time = 0.615 + frame * HOP / RATE - 0.6
                delay = detected_time - kw_end
                if detected_time >= kw_start - 0.03 and detected_time <= kw_end + 1.0:
                    detected += 1
                    latencies.append(delay * 1000)
                    if delay <= 0.30 and delay >= -0.03:
                        quick_detected += 1
        negative_hours = sum(duration for _row, _scores, duration in negatives) / 3600.0
        for _row, scores, _duration in negatives:
            if first_smoothed(scores, threshold) is not None:
                false_files += 1
        metrics = {
            "positive_files": len(positives),
            "detected": detected,
            "recall": detected / len(positives) if positives else None,
            "detected_within_300ms": quick_detected,
            "recall_within_300ms": quick_detected / len(positives) if positives else None,
            "false_files": false_files,
            "negative_files": len(negatives),
            "false_alarms_per_hour": false_files / negative_hours if negative_hours else None,
            "median_latency_ms": float(np.median(latencies)) if latencies else None,
            "p95_latency_ms": float(np.percentile(latencies, 95)) if latencies else None,
        }
        for position in ("begin", "middle", "end"):
            group = [item for item in positives if item[0].get("position") == position]
            hits, quick_hits = 0, 0
            for row, scores, _duration in group:
                frame = first_smoothed(scores, threshold)
                if frame is not None:
                    kw_start = float(row.get("keyword_start_s", "0"))
                    kw_end = float(row.get("keyword_end_s", "0"))
                    detected_time = 0.615 + frame * HOP / RATE - 0.6
                    delay = detected_time - kw_end
                    if kw_start - 0.03 <= detected_time <= kw_end + 1.0:
                        hits += 1
                        if -0.03 <= delay <= 0.30:
                            quick_hits += 1
            metrics[f"{position}_recall"] = hits / len(group) if group else None
            metrics[f"{position}_recall_within_300ms"] = quick_hits / len(group) if group else None
            metrics[f"{position}_n"] = len(group)
        thresholds[str(threshold)] = metrics
    return thresholds


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--output", type=Path, default=Path("data/kws.bin"))
    p.add_argument("--epochs", type=int, default=20)
    p.add_argument("--batch-size", type=int, default=64)
    p.add_argument("--seed", type=int, default=7)
    p.add_argument("--user-dir", type=Path)
    args = p.parse_args()
    random.seed(args.seed); np.random.seed(args.seed); torch.manual_seed(args.seed)
    rows = load_rows(args.manifest)
    if args.user_dir and args.user_dir.exists():
        for path in sorted(args.user_dir.glob("*.wav")):
            rows.append({"file": str(path.resolve()), "label": "1", "voice": "user",
                         "keyword_start_s": "0.05", "keyword_end_s": "0.45", "position": "begin"})
    if not rows:
        raise SystemExit("В манифесте нет аудиофайлов")
    root = args.manifest.parent
    voices = sorted({row.get("voice", "") for row in rows if row.get("label") == "1" and row.get("voice") != "user"})
    held = set(voices[-max(1, len(voices) // 5):]) if len(voices) > 1 else set()
    train_rows = [row for row in rows if row.get("voice", "") not in held or row.get("voice") == "user"]
    test_rows = [row for row in rows if row.get("voice", "") in held]
    if not test_rows:
        train_rows, test_rows = rows[:-1], rows[-1:]
    train_ds = AudioWindows(train_rows, root, True)
    loader = DataLoader(train_ds, batch_size=args.batch_size, shuffle=True, num_workers=0)
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    torch.set_num_threads(min(4, torch.get_num_threads()))
    model = TinyDsCnn().to(device)
    optim = torch.optim.AdamW(model.parameters(), lr=0.002, weight_decay=1e-4)
    for epoch in range(args.epochs):
        model.train(); loss_sum = 0.0
        for x, y in loader:
            x, y = x.to(device), y.to(device)
            loss = F.cross_entropy(model(x), y)
            optim.zero_grad(); loss.backward(); optim.step()
            loss_sum += float(loss.detach()) * len(y)
        print(f"epoch={epoch + 1}/{args.epochs} loss={loss_sum / len(train_ds):.4f}")
    serialize(model.cpu(), args.output)
    print("heldout", evaluate_files(model, test_rows, root, torch.device("cpu")))
    print(f"weights={args.output} bytes={args.output.stat().st_size} train={len(train_rows)} holdout={len(test_rows)} voices_holdout={sorted(held)}")


if __name__ == "__main__":
    main()
