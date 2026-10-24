"""Подстройка детектора под голос одного человека по нескольким его «ривер».

Опыт перед подстройкой в самой программе: обучается не вся сеть, а только голова
(64 веса и сдвиг) и, с --point, pointwise-свёртка последнего блока — с L2 к исходным
весам, чтобы сеть не забыла остальное. Положительные кадры — сразу после слова в клипах
диктора (и их искажённые копии, --aug), отрицательные — те же клипы вне слова, фразы
FLEURS (--bank) и синтетический шум (--noise).

    .venv\\Scripts\\python adapt_voice.py data/kws-ruen3.pt data/friend-ru-uk/clips.jsonl out.bin \\
        --bank 300 --aug 4 --noise 300 --pos-weight 0.1 --l2 3

Замер — как у остальных весов: score_user_wake.py, bench.sh (docs/research/wake-detector.md).
"""

from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

import numpy as np
import torch
from torch.nn import functional as F

import train_multilang as t

FLEURS = Path(__file__).resolve().parent / "data" / "fleurs"


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("base", type=Path, help="веса обучения (.pt)")
    p.add_argument("clips", type=Path, help="clips.jsonl диктора (user_wake.py)")
    p.add_argument("out", type=Path)
    p.add_argument("--channels", type=int, default=64)
    p.add_argument("--dilations", default="1,2,4,8,1,2")
    p.add_argument("--bank", type=int, default=0, help="фраз FLEURS в отрицательные")
    p.add_argument("--aug", type=int, default=0, help="искажённых копий каждого клипа")
    p.add_argument("--noise", type=int, default=0, help="примеров синтетического шума")
    p.add_argument("--pos-weight", type=float, default=0.1, help="доля кадров слова в потере")
    p.add_argument("--l2", type=float, default=3.0, help="притяжение к исходным весам")
    p.add_argument("--point", action="store_true", help="подстраивать ещё pointwise последнего блока")
    args = p.parse_args()

    model = t.CausalTcn(args.channels, dilations=tuple(int(d) for d in args.dilations.split(",")))
    model.load_state_dict(torch.load(args.base, map_location="cpu"))
    model.eval()
    mel, aug = t.LogMel(), t.Augment(True)
    last = model.blocks[-1]
    torch.manual_seed(5)

    def feats(audio: np.ndarray) -> torch.Tensor:
        """[T, 2C]: вход последнего блока и вход его pointwise — всё, что дальше обучается."""
        with torch.no_grad():
            x = F.relu(model.bn(model.pre(model.norm(mel(torch.from_numpy(audio.astype(np.float32))[None])))))
            for block in model.blocks[:-1]:
                x = block(x)
            y = F.relu(last.bn1(last.depth(F.pad(x, (last.pad, 0)))))
        return torch.cat([x[0].T, y[0].T], dim=1)

    def variants(audio: np.ndarray, k: int) -> list[np.ndarray]:
        with torch.no_grad():
            return [audio] + [aug(torch.from_numpy(audio)[None])[0].numpy() for _ in range(k)]

    pos, neg = [], []
    rows = [json.loads(line) for line in args.clips.read_text(encoding="utf-8").splitlines()]
    pre = np.zeros(int(0.6 * t.RATE), np.float32)
    for r in rows:
        clip = t.level(t.read_wav(args.clips.parent / r["file"]))
        for audio in variants(np.concatenate([pre, clip, np.zeros(int(0.8 * t.RATE), np.float32)]), args.aug):
            f = feats(audio)
            target, mask = t.targets(int((0.6 + r["wake_start"]) * t.RATE), int((0.6 + r["wake_end"]) * t.RATE), len(f))
            target, mask = torch.from_numpy(target), torch.from_numpy(mask) == 1
            pos.append(f[(target == 1) & mask])
            neg.append(f[(target == 0) & mask])
    for _ in range(args.noise):
        noise = np.random.randn(2 * t.RATE).astype(np.float32) * 0.01
        f = feats(variants(noise, 1)[1])
        neg.append(f[torch.randperm(len(f))[:60]])
    if args.bank:
        train, _ = t.load_fleurs(FLEURS, 150, 40, 11)
        for clip in random.Random(3).sample(train, args.bank):
            for audio in variants(t.level(clip[:6 * t.RATE]), min(args.aug, 1)):
                f = feats(audio)
                neg.append(f[torch.randperm(len(f))[:60]])
    P, N = torch.cat(pos), torch.cat(neg)
    print(f"клипов {len(rows)}: кадров слова {len(P)}, вне слова {len(N)}")

    C = args.channels
    bn = last.bn2
    bn_scale = (bn.weight / torch.sqrt(bn.running_var + bn.eps)).detach()
    bn_shift = (bn.bias - bn.running_mean * bn_scale).detach()
    w0, b0 = model.head.weight[0, :, 0].detach().clone(), model.head.bias.detach().clone()
    pw0, pb0 = last.point.weight[:, :, 0].detach().clone(), last.point.bias.detach().clone()
    w, b = w0.clone().requires_grad_(), b0.clone().requires_grad_()
    pw, pb = pw0.clone().requires_grad_(args.point), pb0.clone().requires_grad_(args.point)
    X = torch.cat([P, N])
    Y = torch.cat([torch.ones(len(P)), torch.zeros(len(N))])
    weight = torch.cat([torch.full((len(P),), args.pos_weight / len(P)), torch.full((len(N),), (1 - args.pos_weight) / len(N))])

    def logits(w, b, pw, pb) -> torch.Tensor:
        return torch.relu(X[:, :C] + (X[:, C:] @ pw.T + pb) * bn_scale + bn_shift) @ w + b

    with torch.no_grad():
        before = torch.sigmoid(logits(w0, b0, pw0, pb0))
    optim = torch.optim.Adam([w, b] + ([pw, pb] if args.point else []), lr=0.01)
    for _ in range(300):
        loss = (F.binary_cross_entropy_with_logits(logits(w, b, pw, pb), Y, reduction="none") * weight).sum() \
            + args.l2 * (((w - w0) ** 2).sum() / len(w) + ((pw - pw0) ** 2).sum() / C + ((pb - pb0) ** 2).sum() / C)
        optim.zero_grad(); loss.backward(); optim.step()
    with torch.no_grad():
        after = torch.sigmoid(logits(w, b, pw, pb))
        print(f"кадры слова ≥0,9: {(before[:len(P)] >= 0.9).float().mean():.2f} → {(after[:len(P)] >= 0.9).float().mean():.2f}; "
              f"вне слова: {(before[len(P):] >= 0.9).float().mean():.3f} → {(after[len(P):] >= 0.9).float().mean():.3f}")
        model.head.weight[0, :, 0] = w
        model.head.bias[:] = b
        last.point.weight[:, :, 0] = pw
        last.point.bias[:] = pb
    t.export(model, args.out)


if __name__ == "__main__":
    main()
