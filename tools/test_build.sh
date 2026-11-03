#!/usr/bin/env bash
# Тестовый Reever без установщика: собирает release и кладёт reever.exe с ресурсами
# установщика в test-build/. Настройки, модели и журнал — общие с установленной копией (%APPDATA%).
# При запуске тестовый exe сам закрывает установленный Reever и работает вместо него
# (lib.rs, replace_other_instances_if_test_build).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
powershell -NoProfile -Command "Get-Process reever -ErrorAction SilentlyContinue | Where-Object { \$_.Path -like '$(cygpath -w "$root")\*' } | Stop-Process -Force" || true
python "$root/tools/build_installer.py" --stage "$root/test-build"
