"""Сборка установщика Reever без Tauri: release-exe и NSIS-скрипт src-tauri/windows/installer.nsi.

    python tools/build_installer.py            # cargo build --release, затем makensis
    python tools/build_installer.py --no-cargo # только makensis по уже собранному exe
    python tools/build_installer.py --stage test-build  # exe и ресурсы установщика в папку, без NSIS

Результат: src-tauri/target/release/bundle/nsis/Reever_<версия>_x64-setup.exe.
Версия берётся из src-tauri/Cargo.toml — она же вшивается в exe (build.rs).

makensis ищется в PATH, затем в Program Files (x86)/NSIS, затем в %LOCALAPPDATA%/tauri/NSIS
(туда его ставил прежний сборщик Tauri). Нужен NSIS 3 с плагинами nsDialogs, nsExec, System.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src-tauri"
NSI = SRC / "windows" / "installer.nsi"


def version() -> str:
    text = (SRC / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^\[package\][^\[]*?^version\s*=\s*"([^"]+)"', text, re.M | re.S)
    if not m:
        sys.exit("version not found in src-tauri/Cargo.toml")
    return m.group(1)


def find_makensis() -> Path:
    found = shutil.which("makensis")
    if found:
        return Path(found)
    candidates = [
        Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "NSIS" / "makensis.exe",
        Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "NSIS" / "makensis.exe",
        Path(os.environ.get("LOCALAPPDATA", "")) / "tauri" / "NSIS" / "makensis.exe",
    ]
    for c in candidates:
        if c.is_file():
            return c
    sys.exit("makensis not found: install NSIS 3 (https://nsis.sourceforge.io) or add it to PATH")


def bundled_files(exe: Path) -> list[Path]:
    """Файлы, которые кладёт установщик: exe и строки File ... "${SRC}\\..." из скрипта."""
    files = [exe]
    for m in re.finditer(r'^\s*File /a "/oname=[^"]+" "\$\{SRC\}\\([^"]+)"', NSI.read_text(encoding="utf-8-sig"), re.M):
        files.append(SRC / m.group(1).replace("\\", "/"))
    return files


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--no-cargo", action="store_true", help="не пересобирать exe")
    ap.add_argument("--stage", type=Path, help="разложить exe и ресурсы в папку вместо установщика")
    args = ap.parse_args()

    ver = version()
    exe = SRC / "target" / "release" / "reever.exe"
    if not args.no_cargo:
        subprocess.run(["cargo", "build", "--release"], cwd=SRC, check=True)
    if not exe.is_file():
        sys.exit(f"{exe} not found: build it first (cargo build --release)")

    files = bundled_files(exe)
    missing = [str(f) for f in files if not f.is_file()]
    if missing:
        sys.exit("missing files (run scripts/fetch-deps.ps1?):\n  " + "\n  ".join(missing))
    if args.stage:
        stage = args.stage.resolve()
        shutil.rmtree(stage / "resources", ignore_errors=True)
        for f in files:
            dst = stage / (f.name if f == exe else f.relative_to(SRC))
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(f, dst)
        print(stage / exe.name)
        return

    size_kb = sum(f.stat().st_size for f in files) // 1024

    out_dir = SRC / "target" / "release" / "bundle" / "nsis"
    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / f"Reever_{ver}_x64-setup.exe"

    cmd = [
        str(find_makensis()),
        "/V3",
        "/INPUTCHARSET", "UTF8",
        f"/DSRC={SRC}",
        f"/DVERSION={ver}",
        f"/DVERSIONWITHBUILD={ver}.0",
        f"/DMAINBINARYSRCPATH={exe}",
        f"/DOUTFILE={out}",
        f"/DESTIMATEDSIZE={size_kb}",
        str(NSI),
    ]
    subprocess.run(cmd, check=True)
    print(f"{out} ({out.stat().st_size} bytes)")
    print(stage_release(out, ver))


def stage_release(installer: Path, ver: str) -> Path:
    """Всё, что выкладывается релизом GitHub, — в одну папку release/<версия>/ (не в git):
    установщик, SHA256SUMS.txt и RELEASE_NOTES.md (заготовка; написанное руками не трогается).
    Автообновлению из этого нужен только установщик: версию оно берёт из тега, сумму — у GitHub."""
    folder = ROOT / "release" / ver
    folder.mkdir(parents=True, exist_ok=True)
    target = folder / installer.name
    shutil.copy2(installer, target)
    digest = hashlib.sha256(target.read_bytes()).hexdigest()
    (folder / "SHA256SUMS.txt").write_text(f"{digest}  {target.name}\n", encoding="utf-8")
    notes = folder / "RELEASE_NOTES.md"
    if not notes.exists():
        notes.write_text(f"Reever {ver}\n", encoding="utf-8")
    return folder


if __name__ == "__main__":
    main()
