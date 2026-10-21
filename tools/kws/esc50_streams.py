"""Потоки «как с микрофона» из ESC-50 для замеров ложных срабатываний детектора «ривер»
(src-tauri/examples/wake_gate_probe.rs): по файлу на категорию шума, клипы подряд поверх
тихого фона комнаты (белый шум около −60 дБ), громкость клипа случайная −12…0 дБ.

    python esc50_streams.py data/esc50/ESC-50-master data/esc50/streams

ESC-50 — CC BY-NC 3.0: только для замеров, в обучение и в программу не идёт.
"""

from __future__ import annotations

import csv
import random
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
import torch.nn.functional as F

RATE = 16_000


def main() -> None:
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(7)
    by_cat: dict[str, list[str]] = defaultdict(list)
    with (src / "meta" / "esc50.csv").open(encoding="utf-8") as f:
        for row in csv.DictReader(f):
            by_cat[row["category"]].append(row["filename"])
    for cat, names in sorted(by_cat.items()):
        parts = []
        for name in names:
            audio, rate = sf.read(src / "audio" / name, dtype="float32")
            if audio.ndim > 1:
                audio = audio.mean(axis=1)
            # Сглаживание перед прореживанием и линейная интерполяция — для шумов хватает.
            t = torch.from_numpy(audio)[None, None]
            k = max(1, round(rate / RATE))
            t = F.avg_pool1d(F.pad(t, (k // 2, k - 1 - k // 2), mode="replicate"), k, stride=1)
            t = F.interpolate(t, size=int(len(audio) * RATE / rate), mode="linear", align_corners=False)
            audio = t[0, 0].numpy().astype(np.float32)
            peak = float(np.abs(audio).max()) or 1.0
            audio *= 10 ** (rng.uniform(-12, 0) / 20) * 0.5 / peak
            parts.append(audio)
            parts.append(np.zeros(RATE // 2, dtype=np.float32))
        stream = np.concatenate(parts)
        stream += np.random.default_rng(1).normal(0, 10 ** (-60 / 20), stream.shape).astype(np.float32)
        sf.write(out / f"{cat}.wav", np.clip(stream, -1, 1), RATE, subtype="PCM_16")
        print(cat, f"{len(stream) / RATE:.0f} s", flush=True)


if __name__ == "__main__":
    main()
