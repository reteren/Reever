"""Выложить собранный установщик релизом GitHub, откуда его берёт автообновление (update.rs).

    python tools/release.py                    # релиз v<версия> с установщиком этой версии
    python tools/release.py --notes "что нового"

Версия — из src-tauri/Cargo.toml, установщик — src-tauri/target/release/bundle/nsis/
Reever_<версия>_x64-setup.exe (сначала python tools/build_installer.py). Нужен gh, вошедший
в аккаунт с правом записи в репозиторий. Тег ставится на текущий коммит, поэтому он должен
быть уже запушен: иначе скрипт останавливается.

Установленные копии Reever смотрят только «последний» релиз (без черновиков и пре-релизов)
и качают из него файл с точным именем Reever_<версия>_x64-setup.exe.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_installer import ROOT, SRC, version  # noqa: E402


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--notes", default="", help="текст релиза")
    args = ap.parse_args()

    ver = version()
    installer = SRC / "target" / "release" / "bundle" / "nsis" / f"Reever_{ver}_x64-setup.exe"
    if not installer.is_file():
        sys.exit(f"{installer} not found: run python tools/build_installer.py first")

    head = git("rev-parse", "HEAD")
    git("fetch", "--quiet", "origin")
    if subprocess.run(["git", "merge-base", "--is-ancestor", head, "@{u}"], cwd=ROOT).returncode != 0:
        sys.exit("HEAD is not pushed yet: push first, the release tag points at this commit")

    subprocess.run(
        [
            "gh", "release", "create", f"v{ver}", str(installer),
            "--target", head,
            "--title", f"Reever {ver}",
            "--notes", args.notes or f"Reever {ver}",
        ],
        cwd=ROOT,
        check=True,
    )


if __name__ == "__main__":
    main()
