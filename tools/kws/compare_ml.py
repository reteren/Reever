"""Сверка RKW2: оценки по кадрам из обучения (PyTorch, .pt) и из kws.rs (kws_ml_probe).

    .venv\\Scripts\\python compare_ml.py data/kws-ml.pt <файл.wav>
Нужен собранный пример: cargo build --release --example kws_ml_probe (из src-tauri).
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
import torch

from train_multilang import CausalTcn, LogMel, export, read_wav

ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    checkpoint, wav = Path(sys.argv[1]), Path(sys.argv[2])
    state = torch.load(checkpoint, map_location="cpu")
    model = CausalTcn(channels=state["pre.weight"].shape[0])
    model.load_state_dict(state)
    model.eval()
    with torch.no_grad():
        python = torch.sigmoid(model(LogMel()(torch.from_numpy(read_wav(wav))[None])))[0].numpy()
    with tempfile.TemporaryDirectory() as tmp:
        weights = Path(tmp) / "kws.bin"
        export(model, weights)
        probe = ROOT / "src-tauri" / "target" / "release" / "examples" / "kws_ml_probe.exe"
        out = subprocess.run([str(probe), str(weights), str(wav)], capture_output=True, text=True, encoding="utf-8", errors="replace", check=True)
    rust = np.array([float(line) for line in out.stdout.split()])
    n = min(len(python), len(rust))
    diff = np.abs(python[:n] - rust[:n])
    print(f"кадров: python {len(python)}, rust {len(rust)}; макс. расхождение {diff.max():.2e}, среднее {diff.mean():.2e}")
    if len(python) != len(rust) or diff.max() > 1e-3:
        raise SystemExit("РАСХОЖДЕНИЕ")


if __name__ == "__main__":
    main()
