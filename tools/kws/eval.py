"""Оценивает экспортированные веса на отдельном CSV WAV-манифесте."""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import torch

from train import MELS, POINT_CHANNELS, POOL_BINS, PRE_CHANNELS, TinyDsCnn, evaluate_files, load_rows


def load_weights(model: TinyDsCnn, path: Path) -> None:
    payload = path.read_bytes()
    if payload[:4] != b"RKW1":
        raise ValueError("ожидался заголовок RKW1")
    shapes = {
        "pre.weight": (PRE_CHANNELS, MELS, 1), "pre.bias": (PRE_CHANNELS,),
        "depth.weight": (PRE_CHANNELS, 1, 5), "depth.bias": (PRE_CHANNELS,),
        "point.weight": (POINT_CHANNELS, PRE_CHANNELS, 1), "point.bias": (POINT_CHANNELS,),
        "head.weight": (2, POINT_CHANNELS * POOL_BINS), "head.bias": (2,),
    }
    offset = 4
    state = {}
    for name, shape in shapes.items():
        size = int(np.prod(shape)) * 4
        values = np.frombuffer(payload[offset:offset + size], dtype="<f4").copy().reshape(shape)
        state[name] = torch.from_numpy(values)
        offset += size
    if offset != len(payload):
        raise ValueError(f"размер RKW1 неверен: {len(payload)}, ожидалось {offset}")
    model.load_state_dict(state)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--weights", type=Path, required=True)
    p.add_argument("--manifest", type=Path, required=True)
    args = p.parse_args()
    model = TinyDsCnn()
    load_weights(model, args.weights)
    print(evaluate_files(model, load_rows(args.manifest), args.manifest.parent, torch.device("cpu")))


if __name__ == "__main__":
    main()
