"""Собирает CSV для внешней оценки на WAV, уже описанных testdata manifest-ами."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--repo-root", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--live-root", type=Path)
    args = p.parse_args()
    root = args.repo_root.resolve()
    rows: list[dict[str, str]] = []
    manifest = root / "testdata/manifest.json"
    corpus = root / "testdata/corpus"
    if manifest.exists():
        for item in json.loads(manifest.read_text(encoding="utf-8")):
            path = corpus / item["file"]
            if path.is_file():
                positive = item.get("scenario") != "negative"
                rows.append({"file": str(path.resolve()), "label": str(int(positive)),
                             "voice": item.get("voice", "unknown"), "scenario": item.get("scenario", ""),
                             "text": item.get("text", ""), "keyword_start_s": "0",
                             "keyword_end_s": "0.45" if positive else "0", "position": "begin" if positive else ""})
    live_dir = args.live_root or (root / "testdata/live")
    live_manifest = live_dir / "live-manifest.json"
    if live_manifest.exists():
        for item in json.loads(live_manifest.read_text(encoding="utf-8")):
            path = live_dir / item["file"]
            if path.is_file():
                contains_wake = "ривер" in (item.get("phrase", "") + item.get("expected", "")).casefold()
                rows.append({"file": str(path.resolve()), "label": str(int(contains_wake)),
                             "voice": item.get("voice", "live microphone"), "scenario": item.get("kind", "live"),
                             "text": item.get("phrase", ""), "keyword_start_s": "0",
                             "keyword_end_s": "0.45" if contains_wake else "0", "position": "begin" if contains_wake else ""})
    args.out.parent.mkdir(parents=True, exist_ok=True)
    fields = ["file", "label", "voice", "scenario", "text", "keyword_start_s", "keyword_end_s", "position"]
    with args.out.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields); writer.writeheader(); writer.writerows(rows)
    print(f"external_wavs={len(rows)} output={args.out}")


if __name__ == "__main__":
    main()
