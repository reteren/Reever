"""Выложить архивы моделей Reever (pack_models.py) в открытый репозиторий Hugging Face, откуда
их качает установщик. Адреса в src-tauri/src/models.rs и windows/models.nsh указывают на
`BASE_URL` из pack_models.py; другой репозиторий — поменять BASE_URL и пересобрать архивы-список.

Нужен токен Hugging Face с правом записи (huggingface.co → Settings → Access Tokens):

    $env:HF_TOKEN = "hf_..."
    ..\\kws\\.venv\\Scripts\\python upload_models.py
"""

from __future__ import annotations

import json
import os

from huggingface_hub import HfApi

from pack_models import BASE_URL, RELEASE

REPO = BASE_URL.split("huggingface.co/", 1)[1].split("/resolve/", 1)[0]


def main() -> None:
    token = os.environ.get("HF_TOKEN")
    if not token:
        raise SystemExit("Нет HF_TOKEN: создайте токен с правом записи на huggingface.co")
    api = HfApi(token=token)
    api.create_repo(REPO, repo_type="model", exist_ok=True, private=False)
    manifest = json.loads((RELEASE / "models.json").read_text(encoding="utf-8"))
    for folder, item in manifest.items():
        archive = RELEASE / f"{folder}.tar.bz2"
        print(f"{archive.name}: {item['mb']} МБ…", flush=True)
        api.upload_file(path_or_fileobj=str(archive), path_in_repo=archive.name, repo_id=REPO, repo_type="model")
    print(f"Готово: https://huggingface.co/{REPO}")


if __name__ == "__main__":
    main()
