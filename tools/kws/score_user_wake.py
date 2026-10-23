"""Замер детектора на голосе пользователя: проверочная часть записей user_wake.py.

Прогоняет детектор программы (examples/wake_gate_probe, SPOTTER=1 TIMES=1) по записям
testdata/wake/ и считает, сколько кусков проверочной части (data/user/test.json) он поймал
и сколько раз сработал там мимо них.

    .venv\\Scripts\\python score_user_wake.py <веса.bin> [<веса.bin> ...]
    .venv\\Scripts\\python score_user_wake.py --data data/friend-fr --source testdata/wake/friend/fr <веса.bin>
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
from collections import defaultdict
from pathlib import Path

import soundfile as sf

from user_wake import SOURCE, load

ROOT = Path(__file__).resolve().parents[2]
PROBE = ROOT / "src-tauri" / "target" / "release" / "examples" / "wake_gate_probe.exe"
TEST = Path(__file__).resolve().parent / "data" / "user" / "test.json"


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("weights", nargs="+")
    p.add_argument("--data", type=Path, default=TEST.parent, help="выход user_wake.py --out (там test.json)")
    p.add_argument("--source", type=Path, default=SOURCE, help="папка с записями, как у user_wake.py --source")
    args = p.parse_args()
    pieces = json.loads((args.data / "test.json").read_text(encoding="utf-8"))
    files = sorted({p["file"] for p in pieces})
    with tempfile.TemporaryDirectory() as tmp:
        # Зонд читает WAV через sherpa-onnx: float-стерео FL Studio — в 16 кГц PCM.
        for name in files:
            sf.write(Path(tmp) / name, load(args.source / name), 16_000, subtype="PCM_16")
        for weights in args.weights:
            env = dict(os.environ, SPOTTER="1", TIMES="1", WEIGHTS=str(Path(weights).resolve()))
            out = subprocess.run([str(PROBE), f"user={tmp}"], env=env, capture_output=True,
                                 text=True, encoding="utf-8", check=True).stdout
            fires = defaultdict(list)
            for line in out.splitlines():
                if line.startswith("fire,"):
                    path, ms = line[len("fire,"):].rsplit(",", 1)
                    fires[Path(path).name].append(int(ms))
            print(f"== {weights}")
            total = caught = 0
            for name in files:
                own = [p for p in pieces if p["file"] == name]
                border = min(p["start_ms"] for p in own)
                used, hit = set(), 0
                for p in own:
                    match = [t for t in fires[name] if p["start_ms"] - 100 <= t <= p["end_ms"] + 400]
                    hit += bool(match)
                    used.update(match)
                stray = len([t for t in fires[name] if t >= border and t not in used])
                total, caught = total + len(own), caught + hit
                print(f"  {name:14} поймано {hit:2}/{len(own):2}  мимо {stray}")
            print(f"  всего {caught}/{total} ({100 * caught / total:.0f}%)")


if __name__ == "__main__":
    main()
