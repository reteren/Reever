"""Добавляет WAV тишины, шумов и искусственной музыки в CSV-манифест."""

from __future__ import annotations

import argparse
import csv
import math
import random
import struct
import wave
from pathlib import Path

RATE = 16_000


def write_wav(path: Path, samples: list[float]) -> None:
    pcm = b"".join(struct.pack("<h", max(-32768, min(32767, round(x * 32767)))) for x in samples)
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1); out.setsampwidth(2); out.setframerate(RATE); out.writeframes(pcm)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--manifest", required=True, type=Path)
    p.add_argument("--count", type=int, default=12)
    args = p.parse_args()
    folder = args.manifest.parent
    rng = random.Random(156)
    rows: list[dict[str, str]] = []
    with args.manifest.open(encoding="utf-8-sig", newline="") as f:
        rows.extend(csv.DictReader(f))
    for kind in ("silence", "white_noise", "room_noise", "music_tones"):
        for i in range(args.count):
            samples = []
            phase = rng.random() * math.tau
            for n in range(RATE * 2):
                t = n / RATE
                if kind == "silence":
                    value = rng.uniform(-0.0002, 0.0002)
                elif kind == "white_noise":
                    value = rng.uniform(-0.04, 0.04)
                elif kind == "room_noise":
                    value = (0.018 * math.sin(math.tau * 60 * t + phase)
                             + rng.uniform(-0.012, 0.012))
                else:
                    # Устойчивые аккорды и медленная огибающая; это шумовой контроль, не музыка из лицензированного файла.
                    envelope = 0.45 + 0.35 * math.sin(math.tau * 0.35 * t + phase)
                    value = envelope * (0.018 * math.sin(math.tau * 220 * t)
                                        + 0.012 * math.sin(math.tau * 277.18 * t)
                                        + 0.008 * math.sin(math.tau * 329.63 * t))
                samples.append(value)
            name = f"bg-{kind}-{i:03}.wav"
            write_wav(folder / name, samples)
            rows.append({"file": name, "label": "0", "voice": kind, "text": kind,
                         "keyword_start_s": "0", "keyword_end_s": "0", "rate": "0", "pitch": "0",
                         "augmentation": "synthetic_background"})
    fields = sorted({key for row in rows for key in row})
    with args.manifest.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields); writer.writeheader(); writer.writerows(rows)
    print(f"added_background_wavs={args.count * 4} manifest_rows={len(rows)}")


if __name__ == "__main__":
    main()
