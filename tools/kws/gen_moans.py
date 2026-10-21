"""Мычание, стоны и протяжные гласные — отрицательные примеры для детектора «ривер».

Детектор срабатывал на протяжное «и-а», на стон и мычание: в обучении были китайские
«ривер» (瑞沃 «жуй-во», 里沃 «ли-во») — по сути скольжение гласных без «р». Здесь
синтез источник-фильтр: голосовой источник (импульсы с наклоном спектра, дрожание высоты,
придыхание) через резонаторы формант, которые плавно переходят между гласными («и→а»,
«у→а», «а-а-а», «м-м-м»). Клипы — data/ml/synth-moan/, вид `confusable` в
data/ml/manifest.jsonl (train_multilang.py берёт их в отрицательные примеры).

    .venv\\Scripts\\python gen_moans.py [--count 1500]
"""

from __future__ import annotations

import argparse
import json
import math
import random
import wave
from pathlib import Path

import numpy as np

OUT = Path(__file__).resolve().parent / "data" / "ml"
VOICE = "synth-moan"
RATE = 16_000

# Форманты F1–F4 (Гц), мужской голос; женский/детский — умножением.
VOWELS = {
    "i": (280, 2250, 2900, 3500),
    "y": (300, 1600, 2400, 3300),  # «ы»
    "e": (450, 1900, 2550, 3400),
    "a": (750, 1250, 2500, 3400),
    "o": (480, 850, 2450, 3300),
    "u": (320, 750, 2350, 3200),
    "m": (250, 1100, 2300, 3200),  # закрытый рот: носовой гул
}
# Сценарии: последовательность гласных («и-а», «ии-у-о», «м-м-а»…).
PATHS = [
    "ia", "iaa", "ia", "iia", "ea", "ua", "uo", "iuo", "io", "ie", "ai", "ay",
    "a", "aa", "o", "u", "i", "e", "y", "m", "mm", "ma", "mi", "am", "aia",
    "iai", "oa", "ae", "eia", "aua", "imi", "iam", "uai",
]


def resonator(x: np.ndarray, freq: np.ndarray, bw: float) -> np.ndarray:
    """Двухполюсный резонатор с меняющейся частотой (по сэмплам)."""
    y = np.zeros_like(x)
    r = math.exp(-math.pi * bw / RATE)
    y1 = y2 = 0.0
    c2 = -r * r
    gain = 1 - r
    for n in range(len(x)):
        c1 = 2 * r * math.cos(2 * math.pi * freq[n] / RATE)
        v = gain * x[n] + c1 * y1 + c2 * y2
        y[n] = v
        y2, y1 = y1, v
    return y


def moan(rng: random.Random) -> tuple[np.ndarray, str]:
    path = rng.choice(PATHS)
    seconds = rng.uniform(0.35, 1.8)
    n = int(seconds * RATE)
    t = np.arange(n) / RATE
    # Высота: стон — скольжение, вздох — вниз, вопрос — вверх; дрожание и вибрато.
    f0a = rng.uniform(75, 320)
    f0b = f0a * rng.uniform(0.6, 1.6)
    f0 = f0a + (f0b - f0a) * (t / seconds) ** rng.uniform(0.5, 2.0)
    f0 *= 1 + rng.uniform(0, 0.03) * np.sin(2 * math.pi * rng.uniform(4, 7) * t)
    f0 *= 1 + np.cumsum(np.random.default_rng(rng.randrange(1 << 30)).normal(0, 0.0015, n))
    f0 = np.clip(f0, 60, 450)
    phase = np.cumsum(f0 / RATE)
    # Источник: пила с наклоном (мягкий голос) + придыхание.
    saw = 2 * (phase % 1.0) - 1
    source = np.diff(np.concatenate([[0.0], saw])) * -1
    source = np.convolve(source, np.ones(rng.choice((2, 3, 4))) / 3, mode="same")
    breath = rng.uniform(0.0, 0.35)
    source = source * (1 - breath) + np.random.default_rng(rng.randrange(1 << 30)).normal(0, 0.05, n) * breath
    # Траектории формант: равные куски на гласную, сглаженный переход.
    scale = rng.uniform(0.95, 1.3)
    points = [VOWELS[v] for v in path]
    seg = np.linspace(0, len(points) - 1, n)
    idx = np.floor(seg).astype(int)
    frac = seg - idx
    frac = frac * frac * (3 - 2 * frac)
    nxt = np.minimum(idx + 1, len(points) - 1)
    out = np.zeros(n)
    for k, bw in zip(range(4), (90, 110, 160, 220)):
        a = np.array([p[k] for p in points])
        freq = (a[idx] * (1 - frac) + a[nxt] * frac) * scale
        out += resonator(source, freq, bw) / (k + 1)
    if path.startswith("m") or path.endswith("m"):
        # Носовой гул: срезаем верх там, где рот закрыт.
        mask = np.where(np.array([p == VOWELS["m"] for p in points])[idx], 0.35, 1.0)
        out *= mask
    # Огибающая: мягкое начало и затухание.
    env = np.minimum(1, t / rng.uniform(0.03, 0.2)) * np.minimum(1, (seconds - t) / rng.uniform(0.05, 0.4))
    out *= env
    out /= max(1e-6, np.abs(out).max())
    return (out * 0.5).astype(np.float32), path


def write_wav(path: Path, samples: np.ndarray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    pcm = (np.clip(samples, -1, 1) * 32767).astype(np.int16)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm.tobytes())


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--count", type=int, default=1500)
    args = p.parse_args()
    manifest = OUT / "manifest.jsonl"
    done = sum(1 for line in manifest.read_text(encoding="utf-8").splitlines() if f'"{VOICE}/' in line)
    rng = random.Random(20261021 + done)
    with manifest.open("a", encoding="utf-8") as log:
        for i in range(done, args.count):
            audio, path = moan(rng)
            rel = f"{VOICE}/confusable-{i:05d}.wav"
            write_wav(OUT / rel, audio)
            log.write(json.dumps({"file": rel, "kind": "confusable", "lang": "synth", "voice": VOICE,
                                  "sid": 0, "text": path, "speed": 1.0,
                                  "seconds": round(len(audio) / RATE, 3)}, ensure_ascii=False) + "\n")
    print(f"клипов: {args.count} (новых {max(0, args.count - done)})")


if __name__ == "__main__":
    main()
