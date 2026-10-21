"""Клипы gen_sapi.ps1 (голоса Windows) → корпус data/ml/manifest.jsonl: обрезка тишины по
краям, сломанные (пустые) пропускаются. Повторный запуск не дублирует строки."""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import soundfile

from gen_multilang import OUT, RATE, trim

SAPI = Path(__file__).resolve().parent / "data" / "sapi"


def main() -> None:
    manifest = OUT / "manifest.jsonl"
    done = {json.loads(l)["file"] for l in manifest.read_text(encoding="utf-8").splitlines()}
    added = 0
    with manifest.open("a", encoding="utf-8") as log:
        lines = [l for f in sorted((list(SAPI.glob("*/list.tsv")) + list(SAPI.glob("*/phrases.tsv")))) for l in f.read_text(encoding="utf-8-sig").splitlines()]
        for line in lines:
            voice, kind, text, rate, path = line.split("\t")
            if path in done or not Path(path).is_file():
                continue
            audio, sr = soundfile.read(path, dtype="float32")
            if audio.ndim > 1:
                audio = audio.mean(axis=1)
            if sr != RATE:
                audio = np.interp(np.arange(int(len(audio) * RATE / sr)) * sr / RATE, np.arange(len(audio)), audio).astype(np.float32)
            audio = trim(audio)
            if len(audio) < RATE * (0.18 if kind != "speech" else 0.6):
                continue
            # «Ривер, открой …» — граница слова неизвестна; обучение такой вид не берёт.
            if kind == "wake" and ("," in text or " " in text.strip(".!?")):
                kind = "wake_phrase"
            soundfile.write(path, audio, RATE, subtype="PCM_16")
            lang = "uk" if "Ukrainian" in voice else "ru"
            log.write(json.dumps({"file": path, "kind": kind, "lang": lang, "voice": "sapi:" + voice, "sid": 0,
                                  "text": text, "speed": int(rate), "seconds": round(len(audio) / RATE, 3)},
                                 ensure_ascii=False) + "\n")
            added += 1
    print(f"добавлено {added}")


if __name__ == "__main__":
    main()
