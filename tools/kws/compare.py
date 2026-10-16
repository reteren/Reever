"""Сверяет Python и Rust max-score на одном и том же WAV."""

from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path

import torch

from eval import load_weights
from train import TinyDsCnn, log_mel, read_wav, stream_scores


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--weights", type=Path, required=True)
    p.add_argument("--wav", type=Path, required=True)
    p.add_argument("--rust-exe", type=Path, required=True)
    p.add_argument("--cwd", type=Path, required=True, help="рабочая папка src-tauri для бинарника")
    args = p.parse_args()
    model = TinyDsCnn()
    load_weights(model, args.weights)
    python_scores = stream_scores(model, log_mel(read_wav(args.wav)), torch.device("cpu"))
    python_max = float(python_scores.max()) if len(python_scores) else 0.0
    result = subprocess.run([str(args.rust_exe.resolve()), "--model", str(args.weights.resolve()),
                             "--wav", str(args.wav.resolve())], cwd=args.cwd.resolve(),
                            capture_output=True, text=True, check=True)
    match = re.search(r"max_score=([0-9.]+)", result.stdout)
    if not match:
        raise RuntimeError(f"Rust probe не напечатал max_score: {result.stdout} {result.stderr}")
    rust_max = float(match.group(1))
    delta = abs(python_max - rust_max)
    print(f"python_max={python_max:.8f} rust_max={rust_max:.8f} abs_delta={delta:.8f}")
    if delta > 1e-3:
        raise SystemExit("numeric comparison failed: tolerance 1e-3")


if __name__ == "__main__":
    main()
