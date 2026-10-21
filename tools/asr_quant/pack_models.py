"""Архивы моделей Reever для установщика: те же tar.bz2 «папка/файлы», что у sherpa-onnx,
поэтому установщик (models.nsh, ReeverFetch) качает и распаковывает их как раньше.

Источники — собранные варианты в data/ (quantize_v3.py, quantize_sensevoice.py); результат —
data/release/<папка>.tar.bz2 и data/release/models.json (адрес, SHA-256, размер, файлы) —
его значения переносятся в src-tauri/src/models.rs и src-tauri/windows/models.nsh.

    ..\\kws\\.venv\\Scripts\\python pack_models.py
"""

from __future__ import annotations

import hashlib
import json
import tarfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE / "data"
RELEASE = DATA / "release"
# Открытое хранилище архивов; меняется в одном месте вместе с models.rs/models.nsh.
BASE_URL = "https://huggingface.co/Nikenmar/reever-models/resolve/main"

# Папка в архиве → (источник, {имя в архиве: имя в источнике}).
MODELS = {
    "reever-parakeet-v3-int4-accurate": (DATA / "v3-int4-b128", {
        "encoder.onnx": "encoder.int8.onnx", "decoder.onnx": "decoder.int8.onnx",
        "joiner.onnx": "joiner.int8.onnx", "tokens.txt": "tokens.txt"}),
    "reever-parakeet-v3-int4-light": (DATA / "v3-hybrid-b128-conv8u", {
        "encoder.onnx": "encoder.int8.onnx", "decoder.onnx": "decoder.int8.onnx",
        "joiner.onnx": "joiner.int8.onnx", "tokens.txt": "tokens.txt"}),
    "reever-sense-voice-int4-accurate": (DATA / "sensevoice" / "reever-sense-voice-int4-accurate", {
        "model.onnx": "model.onnx", "tokens.txt": "tokens.txt"}),
    "reever-sense-voice-int4-light": (DATA / "sensevoice" / "reever-sense-voice-int4-light", {
        "model.onnx": "model.onnx", "tokens.txt": "tokens.txt"}),
}


def main() -> None:
    RELEASE.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for folder, (source, files) in MODELS.items():
        archive = RELEASE / f"{folder}.tar.bz2"
        if not archive.is_file():
            with tarfile.open(archive, "w:bz2", compresslevel=9) as tar:
                for name, original in files.items():
                    tar.add(source / original, arcname=f"{folder}/{name}")
        digest = hashlib.sha256()
        with archive.open("rb") as f:
            for block in iter(lambda: f.read(1 << 20), b""):
                digest.update(block)
        manifest[folder] = {
            "url": f"{BASE_URL}/{archive.name}",
            "sha256": digest.hexdigest(),
            "bytes": archive.stat().st_size,
            "mb": round(archive.stat().st_size / 2**20),
            "files": list(files),
        }
        print(folder, manifest[folder]["mb"], "МБ", manifest[folder]["sha256"], flush=True)
    (RELEASE / "models.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
