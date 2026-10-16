"""Сверяет потоковые score Python и Rust на одном и том же WAV."""

from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path

import numpy as np
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
    result = subprocess.run(
        [str(args.rust_exe.resolve()), "--model", str(args.weights.resolve()),
         "--wav", str(args.wav.resolve()), "--all-scores"],
        cwd=args.cwd.resolve(), capture_output=True, text=True, check=True,
    )
    rust_scores = np.asarray(
        [float(value) for value in re.findall(r"^score=([0-9.eE+-]+)$", result.stdout, re.MULTILINE)],
        dtype=np.float32,
    )
    if len(python_scores) != len(rust_scores):
        raise RuntimeError(f"разное число кадров: Python={len(python_scores)}, Rust={len(rust_scores)}")
    if not len(python_scores):
        raise RuntimeError("WAV короче 600 мс, сравнивать нечего")
    deltas = np.abs(python_scores - rust_scores)
    maximum = float(deltas.max())
    rms = float(np.sqrt(np.mean(deltas * deltas)))
    print(f"frames={len(deltas)} max_abs_delta={maximum:.8g} rms_delta={rms:.8g}")
    if maximum > 1e-3:
        raise SystemExit("numeric comparison failed: tolerance max abs 1e-3")


if __name__ == "__main__":
    main()
