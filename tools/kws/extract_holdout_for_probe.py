"""Extract the untouched final 30% of friend WAVs for wake_reliability_probe.

python tools/kws/extract_holdout_for_probe.py --out <temporary-directory>
The source WAVs and test.json files are read only. Output is disposable.
"""

from __future__ import annotations

import argparse
import json
import wave
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "tools" / "kws" / "data"
SOURCE = ROOT / "testdata" / "wake" / "friend"
SOURCES = {
    "friend-ru-uk": "ru-uk",
    "friend-ru2": "ru2",
    "friend-fr": "fr",
}


def speaker(folder: str, name: str) -> str:
    if folder == "friend-fr":
        return "friend-fr"
    if folder == "friend-ru2":
        return "friend2-ru"
    if name.startswith("any-"):
        return "friend-any"
    if name.startswith("ruuk-"):
        return "friend-ruuk"
    return "friend-uk"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    for folder, source_folder in SOURCES.items():
        pieces = json.loads((DATA / folder / "test.json").read_text(encoding="utf-8"))
        destination = args.out / folder
        clips = destination / "clips"
        clips.mkdir(parents=True, exist_ok=True)
        cache: dict[str, tuple[bytes, int]] = {}
        with (destination / "clips.jsonl").open("w", encoding="utf-8") as manifest:
            for index, piece in enumerate(pieces):
                name = piece["file"]
                if name not in cache:
                    with wave.open(str(SOURCE / source_folder / name), "rb") as source:
                        if (source.getframerate(), source.getnchannels(), source.getsampwidth()) != (16_000, 1, 2):
                            raise ValueError(f"unexpected WAV format: {name}")
                        cache[name] = (source.readframes(source.getnframes()), source.getnframes())
                samples, frames = cache[name]
                first = max(0, (piece["start_ms"] - 300) * 16)
                last = min(frames, (piece["end_ms"] + 300) * 16)
                stem = Path(name).stem
                filename = f"{stem}-holdout-{index:03d}.wav"
                with wave.open(str(clips / filename), "wb") as output:
                    output.setparams((1, 2, 16_000, 0, "NONE", "not compressed"))
                    output.writeframes(samples[first * 2 : last * 2])
                wake_start = (piece["start_ms"] * 16 - first) / 16_000
                # End of wake is unannotated in test.json; this window is used only for
                # approximate level statistics, not for scoring the detector.
                wake_end = min((last - first) / 16_000, wake_start + 0.5)
                row = {
                    "speaker": speaker(folder, name),
                    "text": f"holdout segment from {name}",
                    "file": f"clips/{filename}",
                    "wake_start": wake_start,
                    "wake_end": wake_end,
                    "source_file": name,
                    "start_ms": piece["start_ms"],
                    "end_ms": piece["end_ms"],
                }
                manifest.write(json.dumps(row, ensure_ascii=False) + "\n")
        print(f"{folder}: {len(pieces)} holdout regions")


if __name__ == "__main__":
    main()
