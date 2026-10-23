#!/usr/bin/env bash
# Тестовый Reever без установщика: собирает release и кладёт reever.exe с ресурсами в
# test-build/. Настройки, модели и журнал — общие с установленной копией (%APPDATA%).
# При запуске тестовый exe сам закрывает установленный Reever и работает вместо него
# (lib.rs, replace_other_instances_if_test_build).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/test-build"
powershell -NoProfile -Command "Get-Process reever -ErrorAction SilentlyContinue | Where-Object { \$_.Path -like '$(cygpath -w "$root")\*' } | Stop-Process -Force" || true
# Как `tauri build`: фронтенд вшит в exe (custom-protocol), а не читается из папки.
(cd "$root/src-tauri" && cargo build --release --features tauri/custom-protocol)
mkdir -p "$out"
cp "$root/src-tauri/target/release/reever.exe" "$out/"
rm -rf "$out/resources"
cp -r "$root/src-tauri/target/release/resources" "$out/"
echo "готово: $(cygpath -w "$out")\reever.exe"
