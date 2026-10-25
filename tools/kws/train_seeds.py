"""Несколько обучений детектора с разными seed — и замер всех, чтобы взять лучшее.

Разброс между seed у этой сети заметный (одни и те же данные дают ±5 «ривер» из 91 и
вдвое разное число ложных на шумах), поэтому одно обучение мало что доказывает. Запускает
train_multilang.py с одинаковыми аргументами и --seed из списка (по `--parallel` сразу,
каждое с --resume: после обрыва повторный запуск продолжит), затем меряет каждые веса:
голоса из --voices (score_user_wake.py), шумы ESC-50, чёрный ящик (blackbox_negatives.py
--score). Таблица — в <prefix>-seeds.txt.

    .venv\\Scripts\\python train_seeds.py --prefix data/kws-ml9 --seeds 11,12,13 -- \\
        --steps 24000 --fleurs data/fleurs --mine-at 8000,15000 --aug-extra ...
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PY = sys.executable
PROBE = HERE.parents[1] / "src-tauri" / "target" / "release" / "examples" / "wake_gate_probe.exe"
VOICES = {
    "user": [],
    "friend-ru-uk": ["--data", "data/friend-ru-uk", "--source", "../../testdata/wake/friend/ru-uk"],
    "friend-fr": ["--data", "data/friend-fr", "--source", "../../testdata/wake/friend/fr"],
    "friend-ru2": ["--data", "data/friend-ru2", "--source", "../../testdata/wake/friend/ru2"],
}


def run(cmd: list[str], env: dict | None = None) -> str:
    return subprocess.run(cmd, cwd=HERE, env=env, capture_output=True, text=True, encoding="utf-8").stdout


def measure(weights: Path, verify: Path | None) -> dict[str, str]:
    env = dict(os.environ, PYTHONIOENCODING="utf-8")
    if verify:
        env["VERIFY"] = str(verify.resolve())
    row = {}
    for name, extra in VOICES.items():
        out = run([PY, "-X", "utf8", "score_user_wake.py", *extra, str(weights)], env)
        m = re.search(r"всего (\d+/\d+)", out)
        row[name] = m.group(1) if m else "?"
    noise = run([str(PROBE), f"noise={HERE / 'data' / 'esc50' / 'streams'}"],
                dict(env, SPOTTER="1", WEIGHTS=str(weights.resolve())))
    m = re.search(r"срабатываний (\d+)", noise)
    row["noise"] = m.group(1) if m else "?"
    bb = run([PY, "-X", "utf8", "blackbox_negatives.py", "--score", str(weights)], env)
    m = re.search(r"«ривер» (\d+/\d+), ложных (\d+)", bb)
    row["blackbox"] = f"{m.group(1)}, ложных {m.group(2)}" if m else "?"
    return row


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--prefix", type=Path, required=True, help="data/kws-ml9 → data/kws-ml9-s11.bin, …")
    p.add_argument("--seeds", default="11,12,13")
    p.add_argument("--parallel", type=int, default=2, help="обучений сразу (каждое ~4 ГБ памяти)")
    p.add_argument("--verify", type=Path, help="вторая ступень для замера")
    p.add_argument("--compare", type=Path, action="append", default=[], help="готовые веса в ту же таблицу")
    p.add_argument("train_args", nargs=argparse.REMAINDER)
    args = p.parse_args()
    train_args = [a for a in args.train_args if a != "--"]
    seeds = [int(s) for s in args.seeds.split(",")]
    outputs = {s: args.prefix.with_name(f"{args.prefix.name}-s{s}.bin") for s in seeds}
    queue, running = [s for s in seeds if not outputs[s].is_file()], {}
    env = dict(os.environ, PYTHONIOENCODING="utf-8")
    while queue or running:
        while queue and len(running) < args.parallel:
            s = queue.pop(0)
            log = outputs[s].with_suffix(".log").open("a", encoding="utf-8")
            running[s] = subprocess.Popen([PY, "train_multilang.py", *train_args, "--seed", str(s), "--resume",
                                           "--output", str(outputs[s])], cwd=HERE, env=env, stdout=log, stderr=log)
            print(f"seed {s}: обучение", flush=True)
        for s, proc in list(running.items()):
            if proc.poll() is not None:
                del running[s]
                print(f"seed {s}: {'готово' if proc.returncode == 0 else f'ошибка {proc.returncode}'}", flush=True)
        time.sleep(10)
    table = [f"вторая ступень: {args.verify or 'нет'}", "веса | " + " | ".join([*VOICES, "шумы ESC-50", "чёрный ящик"])]
    for weights in [*args.compare, *(outputs[s] for s in seeds if outputs[s].is_file())]:
        row = measure(weights, args.verify)
        table.append(f"{weights.name} | " + " | ".join(row.values()))
        print(table[-1], flush=True)
    args.prefix.with_name(f"{args.prefix.name}-seeds.txt").write_text("\n".join(table) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
