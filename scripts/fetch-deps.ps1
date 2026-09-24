<#
.SYNOPSIS
    Загружает бинарные зависимости Reever, которые не хранятся в git.

.DESCRIPTION
    Ставит модель распознавания речи GigaAM v3 и Silero VAD (обе — через
    sherpa-onnx) в src-tauri/resources/model/. Сама библиотека sherpa-onnx
    линкуется статически и скачивается сборкой cargo, здесь её нет.

    Около 225 МБ на диске (~200 МБ загрузки).
    Скрипт идемпотентен: уже установленное пропускается, если не задан -Force.

.PARAMETER Force
    Переустановить всё, даже если файлы уже на месте.

.PARAMETER SkipModels
    Не трогать модели (полезно в CI, где они закешированы).

.PARAMETER AsrModel
    Имя sherpa-onnx ASR-модели из GitHub release asr-models.
    По умолчанию sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16.

.EXAMPLE
    pwsh scripts/fetch-deps.ps1
    pwsh scripts/fetch-deps.ps1 -Force
    pwsh scripts/fetch-deps.ps1 -AsrModel sherpa-onnx-nemo-ctc-giga-am-v3-russian-2025-12-16
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$SkipModels,
    [string]$AsrModel = 'sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # иначе Invoke-WebRequest в разы медленнее

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$ModelDir  = Join-Path $RepoRoot 'src-tauri/resources/model'
$SherpaReleaseUrl = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models'
$SileroName = 'silero_vad.onnx'

if ([IO.Path]::GetFileName($AsrModel) -ne $AsrModel -or [string]::IsNullOrWhiteSpace($AsrModel)) {
    throw "-AsrModel должен быть именем модели без пути: $AsrModel"
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

function Test-SherpaModel {
    param([string]$Path)
    if (-not (Test-Path (Join-Path $Path 'tokens.txt') -PathType Leaf)) { return $false }

    $onnx = @(Get-ChildItem -LiteralPath $Path -Filter '*.onnx' -File -ErrorAction SilentlyContinue)
    $hasTransducer = ($onnx.Name -like 'encoder*.onnx') -and
        (Test-Path (Join-Path $Path 'decoder.onnx') -PathType Leaf) -and
        (Test-Path (Join-Path $Path 'joiner.onnx') -PathType Leaf)
    $hasCtc = ($onnx.Name -like 'model*.onnx')
    return ($hasTransducer -or $hasCtc)
}

# --- 1. модели ---------------------------------------------------------------
if (-not $SkipModels) {
    New-Item -ItemType Directory -Force -Path $ModelDir | Out-Null
    Write-Step 'Модели sherpa-onnx (GigaAM v3 + Silero VAD)'
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

    $asrTarget = Join-Path $ModelDir $AsrModel
    if ((Test-SherpaModel $asrTarget) -and -not $Force) {
        Write-Skip $AsrModel
    } else {
        $tmpAsr = Join-Path ([System.IO.Path]::GetTempPath()) "$AsrModel.tar.bz2"
        try {
            Get-Archive -Url "$SherpaReleaseUrl/$AsrModel.tar.bz2" -Destination $tmpAsr
            if (Test-Path $asrTarget) { Remove-Item -LiteralPath $asrTarget -Recurse -Force }
            & tar.exe -xjf $tmpAsr -C $ModelDir
            if ($LASTEXITCODE -ne 0) { throw "tar.exe завершился с кодом $LASTEXITCODE при распаковке $AsrModel" }
            if (-not (Test-SherpaModel $asrTarget)) {
                throw "после распаковки $AsrModel не найдены tokens.txt и ожидаемые ONNX-файлы"
            }
            Write-Ok $AsrModel
        } finally {
            if (Test-Path $tmpAsr) { Remove-Item -LiteralPath $tmpAsr -Force }
        }
    }
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
    if (-not (Test-SherpaModel (Join-Path $ModelDir $AsrModel))) {
        $missing += "resources/model/$AsrModel (tokens.txt и encoder/decoder/joiner либо model*.onnx)"
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
