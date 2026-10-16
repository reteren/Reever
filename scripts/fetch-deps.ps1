<#
.SYNOPSIS
    Загружает бинарные зависимости Reever, которые не хранятся в git.

.DESCRIPTION
    Ставит модель распознавания речи GigaAM v3 и Silero VAD (обе — через
    sherpa-onnx) в src-tauri/resources/model/. Сама библиотека sherpa-onnx
    линкуется статически и скачивается сборкой cargo, здесь её нет.

    Загружает русскую GigaAM v3, английскую Parakeet и Silero VAD.
    Скрипт идемпотентен: уже установленное пропускается, если не задан -Force.

.PARAMETER Force
    Переустановить всё, даже если файлы уже на месте.

.PARAMETER SkipModels
    Не трогать модели (полезно в CI, где они закешированы).

.PARAMETER AsrModel
    Имя sherpa-onnx ASR-модели из GitHub release asr-models.
    По умолчанию sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16.

.PARAMETER SecondModel
    Имя дополнительной sherpa-onnx ASR-модели из GitHub release asr-models.
    По умолчанию sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.

.PARAMETER EnglishOnlyModel
    Основная sherpa-onnx модель для режима без русского языка.
    По умолчанию sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8.

.EXAMPLE
    pwsh scripts/fetch-deps.ps1
    pwsh scripts/fetch-deps.ps1 -Force
    pwsh scripts/fetch-deps.ps1 -AsrModel sherpa-onnx-nemo-ctc-giga-am-v3-russian-2025-12-16
    pwsh scripts/fetch-deps.ps1 -SecondModel sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8
    pwsh scripts/fetch-deps.ps1 -EnglishOnlyModel sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$SkipModels,
    [string]$AsrModel = 'sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16',
    [string]$SecondModel = 'sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8',
    [string]$EnglishOnlyModel = 'sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8',
    [string]$LidModel = 'sherpa-onnx-whisper-base',
    # Фоновый детектор wake-слова (docs/voice-pipeline.md); пустое имя — без него.
    [string]$WakeModel = 'sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16'
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
if ([IO.Path]::GetFileName($SecondModel) -ne $SecondModel -or [string]::IsNullOrWhiteSpace($SecondModel)) {
    throw "-SecondModel должен быть именем модели без пути: $SecondModel"
}
if ([IO.Path]::GetFileName($EnglishOnlyModel) -ne $EnglishOnlyModel -or [string]::IsNullOrWhiteSpace($EnglishOnlyModel)) {
    throw "-EnglishOnlyModel должен быть именем модели без пути: $EnglishOnlyModel"
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
    $hasEncoder = @($onnx | Where-Object { $_.Name -like 'encoder*.onnx' }).Count -gt 0
    $hasDecoder = @($onnx | Where-Object { $_.Name -like 'decoder*.onnx' }).Count -gt 0
    $hasJoiner = @($onnx | Where-Object { $_.Name -like 'joiner*.onnx' }).Count -gt 0
    $hasTransducer = $hasEncoder -and $hasDecoder -and $hasJoiner
    $hasCtc = ($onnx.Name -like 'model*.onnx')
    return ($hasTransducer -or $hasCtc)
}

# Whisper-модель определителя языка: <имя>-encoder*.onnx, <имя>-decoder*.onnx, <имя>-tokens.txt.
function Test-WhisperModel {
    param([string]$Path)
    $files = @(Get-ChildItem -LiteralPath $Path -File -ErrorAction SilentlyContinue)
    $hasEncoder = @($files | Where-Object { $_.Name -like '*-encoder*.onnx' }).Count -gt 0
    $hasDecoder = @($files | Where-Object { $_.Name -like '*-decoder*.onnx' }).Count -gt 0
    $hasTokens = @($files | Where-Object { $_.Name -like '*tokens.txt' }).Count -gt 0
    return ($hasEncoder -and $hasDecoder -and $hasTokens)
}

function Install-WhisperModel {
    param([string]$Name)
    $target = Join-Path $ModelDir $Name
    if ((Test-WhisperModel $target) -and -not $Force) {
        Write-Skip $Name
        return
    }
    $archive = Join-Path ([System.IO.Path]::GetTempPath()) "$Name.tar.bz2"
    try {
        Get-Archive -Url "$SherpaReleaseUrl/$Name.tar.bz2" -Destination $archive
        if (Test-Path $target) { Remove-Item -LiteralPath $target -Recurse -Force }
        & tar.exe -xjf $archive -C $ModelDir
        if ($LASTEXITCODE -ne 0) { throw "tar.exe завершился с кодом $LASTEXITCODE при распаковке $Name" }
        if (-not (Test-WhisperModel $target)) { throw "после распаковки $Name не найдены encoder/decoder/tokens Whisper" }
        Write-Ok $Name
    } finally {
        if (Test-Path $archive) { Remove-Item -LiteralPath $archive -Force }
    }
}

function Install-SherpaModel {
    param([string]$Name)
    $target = Join-Path $ModelDir $Name
    if ((Test-SherpaModel $target) -and -not $Force) {
        Write-Skip $Name
        return
    }

    $archive = Join-Path ([System.IO.Path]::GetTempPath()) "$Name.tar.bz2"
    try {
        Get-Archive -Url "$SherpaReleaseUrl/$Name.tar.bz2" -Destination $archive
        if (Test-Path $target) { Remove-Item -LiteralPath $target -Recurse -Force }
        & tar.exe -xjf $archive -C $ModelDir
        if ($LASTEXITCODE -ne 0) { throw "tar.exe завершился с кодом $LASTEXITCODE при распаковке $Name" }
        if (-not (Test-SherpaModel $target)) {
            throw "после распаковки $Name не найдены tokens.txt и ожидаемые ONNX-файлы"
        }
        Write-Ok $Name
    } finally {
        if (Test-Path $archive) { Remove-Item -LiteralPath $archive -Force }
    }
}

# --- 1. модели ---------------------------------------------------------------
if (-not $SkipModels) {
    New-Item -ItemType Directory -Force -Path $ModelDir | Out-Null
    Write-Step "Модели sherpa-onnx (GigaAM v3 + $SecondModel + $EnglishOnlyModel + $LidModel + Silero VAD)"
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

    Install-SherpaModel -Name $AsrModel
    if ($SecondModel -ne $AsrModel) {
        Install-SherpaModel -Name $SecondModel
    } else {
        Write-Skip "$SecondModel (совпадает с -AsrModel)"
    }
    if ($EnglishOnlyModel -ne $AsrModel -and $EnglishOnlyModel -ne $SecondModel) {
        Install-SherpaModel -Name $EnglishOnlyModel
    } else {
        Write-Skip "$EnglishOnlyModel (уже совпадает с другой ASR-моделью)"
    }
    # Определитель языка цели команды (docs/bilingual.md); пустое имя — без него.
    if (-not [string]::IsNullOrWhiteSpace($LidModel)) {
        Install-WhisperModel -Name $LidModel
    }
    if (-not [string]::IsNullOrWhiteSpace($WakeModel)) {
        Install-SherpaModel -Name $WakeModel
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
    if ($SecondModel -ne $AsrModel -and -not (Test-SherpaModel (Join-Path $ModelDir $SecondModel))) {
        $missing += "resources/model/$SecondModel (tokens.txt и encoder/decoder/joiner либо model*.onnx)"
    }
    if ($EnglishOnlyModel -ne $AsrModel -and $EnglishOnlyModel -ne $SecondModel -and
        -not (Test-SherpaModel (Join-Path $ModelDir $EnglishOnlyModel))) {
        $missing += "resources/model/$EnglishOnlyModel (tokens.txt и encoder/decoder/joiner либо model*.onnx)"
    }
    if (-not [string]::IsNullOrWhiteSpace($LidModel) -and -not (Test-WhisperModel (Join-Path $ModelDir $LidModel))) {
        $missing += "resources/model/$LidModel (Whisper encoder/decoder/tokens)"
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
