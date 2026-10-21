[CmdletBinding()]
param(
    # restore — вернуть исходный энкодер int8 из резервной копии.
    [ValidateSet('install', 'restore')][string]$Mode = 'install'
)
# Parakeet v3 «точный» (энкодер: MatMul в 4 битах блоками по 128, остальное без сжатия;
# quantize_v3.py) вместо исходного int8 в папке моделей Reever. Программа выбирает файлы по
# имени, поэтому энкодер просто подменяется под тем же именем encoder.int8.onnx; исходный
# лежит в models\backup-v3-int8\. Кэш оптимизированного графа (ortopt.rs) сверяет размер и
# дату файлов и пересобирается сам.

$ErrorActionPreference = 'Stop'
$models = Join-Path $env:LOCALAPPDATA 'com.troid.reever\models'
$v3 = Join-Path $models 'sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8'
$backup = Join-Path $models 'backup-v3-int8'
$accurate = Join-Path $PSScriptRoot 'data\v3-int4-b128\encoder.int8.onnx'
$encoder = Join-Path $v3 'encoder.int8.onnx'

if (Get-Process reever -ErrorAction SilentlyContinue) { throw 'Сначала закройте Reever.' }

if ($Mode -eq 'install') {
    if (-not (Test-Path $accurate)) { throw "Нет точного энкодера: $accurate (quantize_v3.py)" }
    New-Item -ItemType Directory -Force -Path $backup | Out-Null
    if (-not (Test-Path (Join-Path $backup 'encoder.int8.onnx'))) {
        Copy-Item $encoder (Join-Path $backup 'encoder.int8.onnx')
    }
    Copy-Item $accurate $encoder -Force
    Write-Host "Поставлен точный энкодер; исходный — в $backup"
} else {
    $saved = Join-Path $backup 'encoder.int8.onnx'
    if (-not (Test-Path $saved)) { throw "Нет резервной копии: $saved" }
    Copy-Item $saved $encoder -Force
    Write-Host 'Возвращён исходный энкодер int8.'
}
(Get-Item $encoder).Length / 1MB | ForEach-Object { Write-Host ("Энкодер: {0:N0} МБ" -f $_) }
