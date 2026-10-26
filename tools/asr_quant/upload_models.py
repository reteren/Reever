"""Выложить архивы моделей Reever (pack_models.py) в релиз открытого репозитория GitHub, откуда
их качает установщик. Адреса в src-tauri/src/models.rs и windows/models.nsh указывают на
`BASE_URL` из pack_models.py: `https://github.com/<владелец>/<репозиторий>/releases/download/<тег>`.
Новые архивы — новый тег: уже выложенные файлы установщики старых версий проверяют по SHA-256.

Нужен GitHub CLI с входом в аккаунт владельца (`gh auth login`):

    ..\\kws\\.venv\\Scripts\\python upload_models.py
"""

from __future__ import annotations

import json
import subprocess

from pack_models import BASE_URL, RELEASE

REPO, TAG = BASE_URL.split("github.com/", 1)[1].split("/releases/download/", 1)


def main() -> None:
    manifest = json.loads((RELEASE / "models.json").read_text(encoding="utf-8"))
    archives = [str(RELEASE / f"{folder}.tar.bz2") for folder in manifest]
    exists = subprocess.run(["gh", "release", "view", TAG, "-R", REPO], capture_output=True).returncode == 0
    if exists:
        command = ["gh", "release", "upload", TAG, "-R", REPO, "--clobber"]
    else:
        command = ["gh", "release", "create", TAG, "-R", REPO, "--title", f"Языковые пакеты {TAG}", "--notes", ""]
    subprocess.run([*command, *archives, str(RELEASE / "models.json")], check=True)
    print(f"Готово: https://github.com/{REPO}/releases/tag/{TAG}")


if __name__ == "__main__":
    main()
