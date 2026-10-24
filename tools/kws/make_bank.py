"""Банк отрицательных примеров для подстройки под голос в программе (voice_adapt.rs).

Подстройка учит голову сети на «ривер» человека; чтобы она не начала срабатывать на всё
подряд, ей нужны и отрицательные кадры — чужая речь и шум. Звук возить с программой
тяжело, поэтому банк — готовые признаки перед головой (выход блоков) этих самых весов:
фразы FLEURS (CC-BY 4.0), их искажённые копии и синтетический шум (как в adapt_voice.py).
Признаки зависят от весов: банк помнит хэш своего .bin и при замене весов собирается заново.

Формат RKB1: магия, u32 каналов, u32 кадров, u64 FNV-1a весов, f32 масштаб на канал,
u8 кадры × каналы (значение = u8 · масштаб; признаки после ReLU, неотрицательные).

    .venv\\Scripts\\python make_bank.py data/kws-ruen4.pt ../../src-tauri/resources/wake/river-ru-en.bin
"""

from __future__ import annotations

import argparse
import random
import struct
import tempfile
from pathlib import Path

import numpy as np
import torch
from torch.nn import functional as F

import train_multilang as t

FLEURS = Path(__file__).resolve().parent / "data" / "fleurs"


def fnv1a(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for byte in data:
        h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("pt", type=Path, help="веса обучения (.pt) того же .bin")
    p.add_argument("bin", type=Path, help="веса программы; банк ляжет рядом: <имя>.bank")
    p.add_argument("--channels", type=int, default=64)
    p.add_argument("--dilations", default="1,2,4,8,1,2")
    p.add_argument("--phrases", type=int, default=300)
    p.add_argument("--noise", type=int, default=300)
    p.add_argument("--per-clip", type=int, default=20, help="кадров с каждого примера")
    args = p.parse_args()

    model = t.CausalTcn(args.channels, dilations=tuple(int(d) for d in args.dilations.split(",")))
    model.load_state_dict(torch.load(args.pt, map_location="cpu"))
    model.eval()
    weights = args.bin.read_bytes()
    with tempfile.TemporaryDirectory() as tmp:
        t.export(model, Path(tmp) / "check.bin")
        if (Path(tmp) / "check.bin").read_bytes() != weights:
            raise SystemExit(f"{args.pt} — не те веса, что {args.bin}")
    model.eval()
    mel, aug = t.LogMel(), t.Augment(True)
    torch.manual_seed(5)
    np.random.seed(5)
    rng = random.Random(3)

    def feats(audio: np.ndarray) -> np.ndarray:
        with torch.no_grad():
            x = F.relu(model.bn(model.pre(model.norm(mel(torch.from_numpy(audio.astype(np.float32))[None])))))
            for block in model.blocks:
                x = block(x)
        f = x[0].T.numpy()
        return f[rng.sample(range(len(f)), min(args.per_clip, len(f)))]

    def distorted(audio: np.ndarray) -> np.ndarray:
        with torch.no_grad():
            return aug(torch.from_numpy(audio)[None])[0].numpy()

    frames = []
    train, _ = t.load_fleurs(FLEURS, 150, 40, 11)
    for clip in rng.sample(train, args.phrases):
        audio = t.level(clip[:6 * t.RATE])
        frames += [feats(audio), feats(distorted(audio))]
    for _ in range(args.noise):
        frames.append(feats(distorted(np.random.randn(2 * t.RATE).astype(np.float32) * 0.01)))
    bank = np.concatenate(frames)
    scale = np.maximum(bank.max(axis=0), 1e-6) / 255.0
    quantized = np.clip(np.round(bank / scale), 0, 255).astype(np.uint8)
    out = args.bin.with_suffix(".bank")
    with out.open("wb") as f:
        f.write(b"RKB1")
        f.write(struct.pack("<IIQ", bank.shape[1], bank.shape[0], fnv1a(weights)))
        f.write(scale.astype("<f4").tobytes())
        f.write(quantized.tobytes())
    print(f"{out}: {bank.shape[0]} кадров × {bank.shape[1]}, {out.stat().st_size // 1024} КБ")


if __name__ == "__main__":
    main()
