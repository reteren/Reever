"""Живые «ривер» из «чёрного ящика» программы — в положительные примеры детектора.

Чёрный ящик (src-tauri/src/blackbox.rs) пишет 15 с звука вокруг каждого срабатывания и
отброшенного кандидата. В этих записях — настоящие «ривер» пользователя через его микрофон,
в его комнате, с его фоном, в том числе те, что детектор пропустил или отбросил: лучших
положительных примеров не бывает.

Разметка без человека: Parakeet v3 распознаёт всю запись, слово из asr_check.WAKE_RE (строже,
чем WAKE_LIKE у blackbox_negatives.py: здесь ошибка учит сеть срабатывать на чужое) — «ривер».
Клип — 1 с до слова и 0,5 с после. Окна записей пересекаются (события чаще 15 с — разные
файлы с одним звуком), поэтому слово берётся один раз по абсолютному времени: имя файла —
момент сохранения, то есть конец окна. Записи замера `data/blackbox/test/` и всё, что
попадает в их окна, в обучение не идут.

    .venv\\Scripts\\python blackbox_wake.py [--source %APPDATA%\\com.troid.reever\\blackbox]
    → data/blackbox-wake/clips.jsonl для train_multilang.py --real-wake
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
from datetime import datetime
from pathlib import Path

import soundfile as sf

from asr_check import WAKE_RE, recognizer, words

HERE = Path(__file__).resolve().parent
SOURCE = Path(os.environ.get("APPDATA", "")) / "com.troid.reever" / "blackbox"
RATE = 16_000
PRE, POST = 1.0, 0.5


def window(f: Path) -> tuple[float, float]:
    """Абсолютное время окна записи, с: имя — момент сохранения, конец окна."""
    end = datetime.strptime(f.name[:15], "%Y%m%d-%H%M%S").timestamp()
    return end - sf.info(f).frames / RATE, end


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--source", type=Path, default=SOURCE)
    p.add_argument("--test", type=Path, default=HERE / "data" / "blackbox" / "test")
    p.add_argument("--out", type=Path, default=HERE / "data" / "blackbox-wake")
    p.add_argument("--repeat", type=int, default=2, help="во сколько раз чаще брать клип (поверх --real-wake-repeat)")
    args = p.parse_args()
    held = [window(f) for f in sorted(args.test.glob("*.wav"))]
    held_names = {f.name for f in args.test.glob("*.wav")}
    files = [f for f in sorted(args.source.glob("*.wav")) if f.name not in held_names]
    clips_dir = args.out / "clips"
    if clips_dir.exists():
        shutil.rmtree(clips_dir)
    clips_dir.mkdir(parents=True)
    rec = recognizer()
    taken: list[float] = []
    rows, dup, in_test = [], 0, 0
    for f in files:
        audio, rate = sf.read(f, dtype="float32")
        assert rate == RATE, f
        start, _ = window(f)
        for word, s, e in words(rec, audio):
            if not WAKE_RE.match(word):
                continue
            at = start + s
            if any(a <= at <= b for a, b in held):
                in_test += 1
                continue
            if any(abs(at - t) < 0.6 for t in taken):
                dup += 1
                continue
            taken.append(at)
            a, b = max(0, int((s - PRE) * RATE)), min(len(audio), int((e + POST) * RATE))
            name = f"{f.stem}-{int(s * 1000)}.wav"
            sf.write(clips_dir / name, audio[a:b], RATE, subtype="PCM_16")
            rows.append({"speaker": "user-bb", "lang": "user-bb", "text": word, "file": f"clips/{name}",
                         "wake_start": round(s - a / RATE, 3), "wake_end": round(e - a / RATE, 3),
                         "source": f.name, "repeat": args.repeat})
    (args.out / "clips.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
    print(f"записей {len(files)} (замер {len(held_names)} отложен); «ривер» {len(rows)}, "
          f"повторов из соседних окон {dup}, в окнах замера {in_test}")


if __name__ == "__main__":
    main()
