"""Ложные срабатывания из «чёрного ящика» программы — в отрицательные примеры детектора.

Чёрный ящик (src-tauri/src/blackbox.rs, Настройки → «Записывать срабатывания») пишет 15 с
звука вокруг каждого срабатывания и отброшенного кандидата: микрофон, комната, игры и видео
самого пользователя — лучшие отрицательные примеры, какие бывают.

Разметка без человека: первая ступень детектора (examples/wake_gate_probe, без проверки
паузы — сеть ошибается чаще, чем срабатывает программа) находит кандидатов, Parakeet v3
распознаёт всю запись. Кандидат, рядом с которым (−1,5…+0,5 с) нет ничего похожего на
«ривер», — ложный: 2 с звука до него идут в отрицательные примеры. Ещё кандидат — само
событие записи (за 3 с до конца файла: `blackbox::AFTER`) у файлов `rejected` и
`unconfirmed`: там первая ступень программы точно сработала, даже если зонд не повторил. Где большая модель
слышит «ривер» — кандидат пропускается, даже если это был промах.

Записи делятся по времени: ранние 70 % — обучение (`clips.jsonl` для
train_multilang.py --real-neg), поздние 30 % — замер (`test/` и распознанный текст
`test/asr.json`). `--score` считает на замере срабатывания детектора программы (с паузой,
со второй ступенью из VERIFY): рядом с «ривер» — попадания, остальные — ложные.

    .venv\\Scripts\\python blackbox_negatives.py [--source %APPDATA%\\com.troid.reever\\blackbox] <веса.bin>
    .venv\\Scripts\\python blackbox_negatives.py --score <веса.bin> [<веса.bin> ...]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

import numpy as np
import soundfile as sf

from asr_check import recognizer, words

ROOT = Path(__file__).resolve().parents[2]
PROBE = ROOT / "src-tauri" / "target" / "release" / "examples" / "wake_gate_probe.exe"
SOURCE = Path(os.environ.get("APPDATA", "")) / "com.troid.reever" / "blackbox"
OUT = Path(__file__).resolve().parent / "data" / "blackbox"
RATE = 16_000
SEG = 2.0
# Всё, что большая модель может написать вместо «ривер», — щедро: лучше потерять ложный
# пример, чем научить сеть не слышать настоящее слово.
WAKE_LIKE = re.compile(r"^(riv|rev|reev|rib|riw|рив|рев|рів|рэв|ріў|diver|driver|ariba|riva|ривь)", re.IGNORECASE)


def probe(folder: Path, weights: Path, no_pause: bool) -> dict[str, list[int]]:
    """Срабатывания детектора программы по файлам папки, мс; вторая ступень — VERIFY из окружения."""
    env = dict(os.environ, SPOTTER="1", TIMES="1", WEIGHTS=str(weights.resolve()))
    if no_pause:
        env["NO_PAUSE"] = "1"
        env.pop("VERIFY", None)
    out = subprocess.run([str(PROBE), f"bb={folder}"], env=env, capture_output=True, text=True,
                         encoding="utf-8", check=True).stdout
    fires: dict[str, list[int]] = {}
    for line in out.splitlines():
        if line.startswith("fire,"):
            path, ms = line[len("fire,"):].rsplit(",", 1)
            fires.setdefault(Path(path).name, []).append(int(ms))
    return fires


def near_wake(t: float, wake_at: list) -> bool:
    return any(s - 0.5 <= t <= e + 1.5 for s, e in wake_at)


def score(test_dir: Path, weights: list[Path]) -> None:
    asr = json.loads((test_dir / "asr.json").read_text(encoding="utf-8"))
    total_wake = sum(len(v) for v in asr.values())
    for w in weights:
        fires = probe(test_dir, w, no_pause=False)
        hit = false = 0
        for name, wake_at in asr.items():
            own = fires.get(name, [])
            hit += sum(any(s - 0.5 <= ms / 1000 <= e + 1.5 for ms in own) for s, e in wake_at)
            false += sum(not near_wake(ms / 1000, wake_at) for ms in own)
        print(f"{w}: «ривер» {hit}/{total_wake}, ложных {false} на {len(asr)} записях")


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("weights", type=Path, nargs="+", help="первая ступень, чьи ошибки собираем (с --score — несколько)")
    p.add_argument("--score", action="store_true", help="только замер на test/")
    p.add_argument("--source", type=Path, default=SOURCE)
    p.add_argument("--out", type=Path, default=OUT)
    p.add_argument("--split", type=float, default=0.7, help="доля ранних записей в обучение")
    args = p.parse_args()
    if args.score:
        score(args.out / "test", args.weights)
        return
    files = sorted(args.source.glob("*.wav"))
    border = int(len(files) * args.split)
    clips_dir, test_dir = args.out / "clips", args.out / "test"
    for d in (clips_dir, test_dir):
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)
    for f in files[border:]:
        shutil.copy2(f, test_dir / f.name)
    train = files[:border]
    work = args.out / "train-src"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir()
    for f in train:
        shutil.copy2(f, work / f.name)

    fires = probe(work, args.weights[0], no_pause=True)
    rec = recognizer()
    asr = {}
    for f in files[border:]:
        audio, _ = sf.read(f, dtype="float32")
        asr[f.name] = [(s, e) for w, s, e in words(rec, audio) if WAKE_LIKE.match(w)]
    (test_dir / "asr.json").write_text(json.dumps(asr), encoding="utf-8")
    rows, kept, skipped = [], 0, 0
    for f in train:
        audio, rate = sf.read(f, dtype="float32")
        assert rate == RATE, f
        wake_at = [(s, e) for w, s, e in words(rec, audio) if WAKE_LIKE.match(w)]
        own = list(fires.get(f.name, []))
        if "rejected" in f.stem or "unconfirmed" in f.stem:
            event = int((len(audio) / RATE - 3.0) * 1000)
            if not any(abs(event - ms) < 600 for ms in own):
                own.append(event)
        for ms in own:
            t = ms / 1000
            if near_wake(t, wake_at):
                skipped += 1
                continue
            end = min(len(audio), int((t + 0.15) * RATE))
            piece = audio[max(0, end - int(SEG * RATE)):end]
            piece = np.concatenate([np.zeros(int(SEG * RATE) - len(piece), np.float32), piece])
            name = f"{f.stem}-{ms}.wav"
            sf.write(clips_dir / name, piece, RATE, subtype="PCM_16")
            rows.append({"file": f"clips/{name}", "source": f.name, "at_ms": ms})
            kept += 1
    (args.out / "clips.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
    shutil.rmtree(work)
    print(f"записей: обучение {len(train)}, замер {len(files) - border}; "
          f"кандидатов: ложных {kept}, рядом с «ривер» {skipped}")


if __name__ == "__main__":
    main()
