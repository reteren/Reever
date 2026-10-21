"""Фразы «Ривер, открой …» из корпуса (kind = wake_phrase) получают границы слова по меткам
Parakeet v3 (asr_check.py, поправка начала −0,24 с) — и идут в обучение как живые окна:
«ривер» без паузы перед следующим словом. Где модель слово не нашла — строка остаётся без
границ и в обучение не идёт."""

from __future__ import annotations

import json

import soundfile

import asr_check
from gen_multilang import OUT


def main() -> None:
    rec = asr_check.recognizer()
    path = OUT / "manifest.jsonl"
    rows = [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines()]
    found = missed = 0
    for row in rows:
        if row["kind"] != "wake_phrase" or "wake_end" in row:
            continue
        audio, _ = soundfile.read(row["file"] if ":" in row["file"] else str(OUT / row["file"]), dtype="float32")
        span = asr_check.find_wake(rec, audio)
        if span is None:
            missed += 1
            continue
        row["wake_start"] = round(max(0.0, span[0] - 0.24), 3)
        row["wake_end"] = round(span[1] + 0.04, 3)
        found += 1
    path.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
    print(f"границы найдены {found}, не найдены {missed}")


if __name__ == "__main__":
    main()
