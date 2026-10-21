<#
.SYNOPSIS
    Загружает модели распознавания Reever, которые не хранятся в git.

.DESCRIPTION
    Ставит в src-tauri/resources/model/ то же, что вшито в установщик: английскую
    Parakeet 110m и Silero VAD (детектор «ривер» — свои веса в resources/wake). Сама
    библиотека sherpa-onnx линкуется статически и скачивается сборкой cargo.

    Языковые пакеты (src-tauri/src/models.rs) установщик скачивает у пользователя сам.
    Для разработки и замеров их можно поставить сюда же ключом -Packs: european
    (Parakeet v3), asian (SenseVoice), lid (Whisper base).

    Скрипт идемпотентен: уже установленное пропускается, если не задан -Force.

.PARAMETER Force
    Переустановить всё, даже если файлы уже на месте.

.PARAMETER SkipModels
    Не трогать модели (полезно в CI, где они закешированы).

.PARAMETER Packs
    Языковые пакеты для разработки: european, asian, lid. По умолчанию — ни одного.

.EXAMPLE
    pwsh scripts/fetch-deps.ps1
    pwsh scripts/fetch-deps.ps1 -Packs european,asian,lid
    pwsh scripts/fetch-deps.ps1 -Force
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$SkipModels,
    [string[]]$Packs = @()
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # иначе Invoke-WebRequest в разы медленнее

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$ModelDir  = Join-Path $RepoRoot 'src-tauri/resources/model'
$SherpaReleaseUrl = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models'
$SileroName = 'silero_vad.onnx'

# Модели установщика (tauri.conf.json, bundle.resources) и пакеты (models.rs). Files —
# что должно лежать в папке модели после распаковки.
$BaseModels = @(
    @{ Name = 'sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8'
       Files = @('encoder.int8.onnx', 'decoder.int8.onnx', 'joiner.int8.onnx', 'tokens.txt') }
)
$PackModels = @{
    european = @{ Name = 'sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8'
                  Files = @('encoder.int8.onnx', 'decoder.int8.onnx', 'joiner.int8.onnx', 'tokens.txt') }
    asian    = @{ Name = 'sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09'
                  Files = @('model.int8.onnx', 'tokens.txt') }
    lid      = @{ Name = 'sherpa-onnx-whisper-base'
                  Files = @('base-encoder.int8.onnx', 'base-decoder.int8.onnx', 'base-tokens.txt') }
}

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

function Test-Model {
    param([hashtable]$Model)
    $dir = Join-Path $ModelDir $Model.Name
    foreach ($file in $Model.Files) {
        if (-not (Test-Path (Join-Path $dir $file) -PathType Leaf)) { return $false }
    }
    return $true
}

function Install-Model {
    param([hashtable]$Model)
    if ((Test-Model $Model) -and -not $Force) {
        Write-Skip $Model.Name
        return
    }
    $archive = Join-Path ([System.IO.Path]::GetTempPath()) "$($Model.Name).tar.bz2"
    try {
        Get-Archive -Url "$SherpaReleaseUrl/$($Model.Name).tar.bz2" -Destination $archive
        $target = Join-Path $ModelDir $Model.Name
        if (Test-Path $target) { Remove-Item -LiteralPath $target -Recurse -Force }
        # Только нужные файлы: в архивах ещё полноразмерные модели и примеры.
        $members = $Model.Files | ForEach-Object { "$($Model.Name)/$_" }
        & tar.exe -xjf $archive -C $ModelDir @members
        if ($LASTEXITCODE -ne 0) { throw "tar.exe завершился с кодом $LASTEXITCODE при распаковке $($Model.Name)" }
        if (-not (Test-Model $Model)) { throw "после распаковки $($Model.Name) не хватает файлов" }
        Write-Ok $Model.Name
    } finally {
        if (Test-Path $archive) { Remove-Item -LiteralPath $archive -Force }
    }
}

# `-File` передаёт «european,asian» одной строкой — делим сами.
$Packs = @($Packs | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
foreach ($pack in $Packs) {
    if (-not $PackModels.ContainsKey($pack)) { throw "-Packs: неизвестный пакет $pack (есть european, asian, lid)" }
}
$Wanted = @($BaseModels) + @($Packs | ForEach-Object { $PackModels[$_] })

# --- 1. модели ---------------------------------------------------------------
if (-not $SkipModels) {
    New-Item -ItemType Directory -Force -Path $ModelDir | Out-Null
    Write-Step "Модели sherpa-onnx ($($Wanted.Count) + Silero VAD)"
    $sileroPath = Join-Path $ModelDir $SileroName
    if ((Test-Path $sileroPath -PathType Leaf) -and -not $Force) {
        Write-Skip $SileroName
    } else {
        $tmpSilero = Join-Path ([System.IO.Path]::GetTempPath()) $SileroName
        try {
            Get-Archive -Url "$SherpaReleaseUrl/$SileroName" -Destination $tmpSilero
            Copy-Item -LiteralPath $tmpSilero -Destination $sileroPath -Force
            Write-Ok $SileroName
        } finally {
            if (Test-Path $tmpSilero) { Remove-Item -LiteralPath $tmpSilero -Force }
        }
    }
    foreach ($model in $Wanted) { Install-Model $model }
} else {
    Write-Step 'Модели — пропущено (-SkipModels)'
}

# --- 2. проверка -------------------------------------------------------------
Write-Step 'Проверка'
$missing = @()
if (-not $SkipModels) {
    if (-not (Test-Path (Join-Path $ModelDir $SileroName) -PathType Leaf)) {
        $missing += "resources/model/$SileroName"
    }
    foreach ($model in $Wanted) {
        if (-not (Test-Model $model)) { $missing += "resources/model/$($model.Name)" }
    }
}

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host 'НЕ ХВАТАЕТ:' -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    Write-Host ''
    Write-Host 'Сборка пройдёт, но приложение не сможет загрузить модель распознавания.' -ForegroundColor Yellow
    exit 1
}

Write-Ok 'все зависимости на месте'
Write-Host ''
Write-Host 'Дальше: npm install, затем npm run dev' -ForegroundColor Cyan
