<#
.SYNOPSIS
    Загружает бинарные зависимости Reever, которые не хранятся в git.

.DESCRIPTION
    Ставит две вещи:
      1. Модели Vosk (RU + EN) -> src-tauri/resources/model/
      2. Нативную библиотеку Vosk для Windows -> src-tauri/vendor/vosk/

    Суммарно около 238 МБ на диске (~100 МБ загрузки).
    Скрипт идемпотентен: уже установленное пропускается, если не задан -Force.

.PARAMETER Force
    Переустановить всё, даже если файлы уже на месте.

.PARAMETER SkipModels
    Не трогать модели (полезно в CI, где они закешированы).

.EXAMPLE
    pwsh scripts/fetch-deps.ps1
    pwsh scripts/fetch-deps.ps1 -Force
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$SkipModels
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # иначе Invoke-WebRequest в разы медленнее

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$ModelDir  = Join-Path $RepoRoot 'src-tauri/resources/model'
$VendorDir = Join-Path $RepoRoot 'src-tauri/vendor/vosk'

# --- источники ---------------------------------------------------------------
# Версия нативной библиотеки соответствует бинарям, с которыми проект собирался
# изначально (файлы от 2022-12-14 = релиз v0.3.45).
$VoskNativeUrl = 'https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-win64-0.3.45.zip'
$Models = @(
    @{ Name = 'vosk-model-small-ru-0.22';    Url = 'https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip' }
    @{ Name = 'vosk-model-small-en-us-0.15'; Url = 'https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip' }
)

# Файлы, без которых сборка соберётся, но приложение упадёт в рантайме.
# libvosk.dll — MinGW-сборка, поэтому тянет за собой три рантайм-DLL.
$RequiredVendorFiles = @(
    'libvosk.dll',
    'libstdc++-6.dll',
    'libgcc_s_seh-1.dll',
    'libwinpthread-1.dll'
)
# Импортная библиотека для линковщика MSVC. Имя не произвольное: vosk-sys 0.2.0
# объявляет #[cfg_attr(target_os = "windows", link(name = "libvosk"))], поэтому
# линковщик ищет ровно libvosk.lib. (Встречающийся рядом vosk.lib — избыточный
# дубль того же файла, сборке он не нужен.)
$RequiredLib = 'libvosk.lib'

# --- вспомогательное ---------------------------------------------------------
function Write-Step   { param([string]$m) Write-Host "==> $m" -ForegroundColor Cyan }
function Write-Ok     { param([string]$m) Write-Host "    OK  $m" -ForegroundColor Green }
function Write-Skip   { param([string]$m) Write-Host "    --  $m (уже на месте)" -ForegroundColor DarkGray }

function Get-Archive {
    param([string]$Url, [string]$Destination)
    Write-Host "    загрузка $Url"
    Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing
    $mb = [math]::Round((Get-Item $Destination).Length / 1MB, 1)
    Write-Host "    получено $mb МБ"
}

# --- 1. модели ---------------------------------------------------------------
if (-not $SkipModels) {
    Write-Step 'Модели Vosk'
    New-Item -ItemType Directory -Force -Path $ModelDir | Out-Null

    foreach ($model in $Models) {
        $target = Join-Path $ModelDir $model.Name
        if ((Test-Path $target) -and -not $Force) {
            Write-Skip $model.Name
            continue
        }

        $tmp = Join-Path ([System.IO.Path]::GetTempPath()) "$($model.Name).zip"
        try {
            Get-Archive -Url $model.Url -Destination $tmp

            if (Test-Path $target) { Remove-Item -Recurse -Force $target }
            # Архив содержит папку с именем модели на верхнем уровне,
            # поэтому распаковываем в родительский каталог.
            Expand-Archive -Path $tmp -DestinationPath $ModelDir -Force

            if (-not (Test-Path $target)) {
                throw "после распаковки не найден каталог $target — изменилась структура архива"
            }
            Write-Ok $model.Name
        }
        finally {
            if (Test-Path $tmp) { Remove-Item -Force $tmp }
        }
    }
} else {
    Write-Step 'Модели Vosk — пропущено (-SkipModels)'
}

# --- 2. нативная библиотека --------------------------------------------------
Write-Step 'Нативная библиотека Vosk (win64)'
New-Item -ItemType Directory -Force -Path $VendorDir | Out-Null

$haveAllVendor = ($RequiredVendorFiles | ForEach-Object { Test-Path (Join-Path $VendorDir $_) }) -notcontains $false
$haveLib = Test-Path (Join-Path $VendorDir $RequiredLib)

if ($haveAllVendor -and $haveLib -and -not $Force) {
    Write-Skip 'vendor/vosk'
} else {
    $tmpZip     = Join-Path ([System.IO.Path]::GetTempPath()) 'vosk-win64.zip'
    $tmpExtract = Join-Path ([System.IO.Path]::GetTempPath()) 'vosk-win64-extract'
    try {
        Get-Archive -Url $VoskNativeUrl -Destination $tmpZip

        if (Test-Path $tmpExtract) { Remove-Item -Recurse -Force $tmpExtract }
        Expand-Archive -Path $tmpZip -DestinationPath $tmpExtract -Force

        # Раскладка внутри архива между релизами менялась — ищем рекурсивно.
        Get-ChildItem -Path $tmpExtract -Recurse -File | ForEach-Object {
            Copy-Item -Path $_.FullName -Destination (Join-Path $VendorDir $_.Name) -Force
        }
        Write-Ok 'vendor/vosk'
    }
    finally {
        if (Test-Path $tmpZip)     { Remove-Item -Force $tmpZip }
        if (Test-Path $tmpExtract) { Remove-Item -Recurse -Force $tmpExtract }
    }
}

# --- 3. проверка -------------------------------------------------------------
Write-Step 'Проверка'
$missing = @()

foreach ($f in $RequiredVendorFiles) {
    if (-not (Test-Path (Join-Path $VendorDir $f))) { $missing += "vendor/vosk/$f" }
}
if (-not (Test-Path (Join-Path $VendorDir $RequiredLib))) {
    $missing += "vendor/vosk/$RequiredLib"
}
if (-not $SkipModels) {
    foreach ($model in $Models) {
        $amFile = Join-Path $ModelDir "$($model.Name)/am/final.mdl"
        if (-not (Test-Path $amFile)) { $missing += "resources/model/$($model.Name)" }
    }
}

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host 'НЕ ХВАТАЕТ:' -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    Write-Host ''
    Write-Host 'Сборка пройдёт, но приложение упадёт при загрузке Vosk:' -ForegroundColor Yellow
    Write-Host 'build.rs на отсутствующую DLL печатает предупреждение, а не ошибку.' -ForegroundColor Yellow
    exit 1
}

Write-Ok 'все зависимости на месте'
Write-Host ''
Write-Host 'Дальше: npm install, затем npm run dev' -ForegroundColor Cyan
