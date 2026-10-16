[CmdletBinding()]
param(
    [int[]]$Rates = @(-3, 0, 3),
    [int]$MaxVoices = 0,
    [switch]$WithOpus = $true
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$data = Join-Path $PSScriptRoot 'data\generated'
$manifest = Join-Path $data 'manifest.csv'
New-Item -ItemType Directory -Force -Path $data | Out-Null

$voice = New-Object -ComObject SAPI.SpVoice
$voices = @($voice.GetVoices())
if ($MaxVoices -gt 0) { $voices = @($voices | Select-Object -First $MaxVoices) }
$format = New-Object -ComObject SAPI.SpAudioFormat
$format.Type = 18 # SAFT16kHz16BitMono
$tmp = Join-Path $env:TEMP 'reever-kws-sapi.wav'
$rows = New-Object System.Collections.Generic.List[object]
$index = 0

function Get-Pcm([string]$Text, $Token, [int]$Rate) {
    $stream = New-Object -ComObject SAPI.SpFileStream
    $stream.Format = $format
    $stream.Open($tmp, 3, $false)
    $voice.Voice = $Token
    $voice.Rate = $Rate
    $voice.AudioOutputStream = $stream
    [void]$voice.Speak($Text)
    $stream.Close()
    $bytes = [IO.File]::ReadAllBytes($tmp)
    for ($offset = 12; $offset + 8 -le $bytes.Length;) {
        $chunk = [Text.Encoding]::ASCII.GetString($bytes, $offset, 4)
        $size = [BitConverter]::ToInt32($bytes, $offset + 4)
        if ($chunk -eq 'data') { $pcm = New-Object byte[] $size; [Array]::Copy($bytes, $offset + 8, $pcm, 0, $size); return ,$pcm }
        $offset += 8 + $size + ($size % 2)
    }
    return ,([byte[]]@())
}

function Write-Wav([string]$Path, [byte[]]$Pcm) {
    $s = New-Object IO.MemoryStream; $w = New-Object IO.BinaryWriter($s)
    $w.Write([Text.Encoding]::ASCII.GetBytes('RIFF')); $w.Write([int](36 + $Pcm.Length))
    $w.Write([Text.Encoding]::ASCII.GetBytes('WAVEfmt ')); $w.Write([int]16); $w.Write([int16]1); $w.Write([int16]1)
    $w.Write([int]16000); $w.Write([int]32000); $w.Write([int16]2); $w.Write([int16]16)
    $w.Write([Text.Encoding]::ASCII.GetBytes('data')); $w.Write([int]$Pcm.Length); $w.Write($Pcm)
    [IO.File]::WriteAllBytes($Path, $s.ToArray())
}

$ffmpeg = Get-Command ffmpeg -ErrorAction SilentlyContinue
$pitchValues = if ($ffmpeg) { @(-8, 0, 8) } else { Write-Warning 'ffmpeg не найден: pitch augmentation отключена'; @(0) }
function Get-WavDataBytes([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    for ($offset = 12; $offset + 8 -le $bytes.Length;) {
        $chunk = [Text.Encoding]::ASCII.GetString($bytes, $offset, 4)
        $size = [BitConverter]::ToInt32($bytes, $offset + 4)
        if ($chunk -eq 'data') { return $size }
        $offset += 8 + $size + ($size % 2)
    }
    throw "В WAV не найден data chunk: $Path"
}
function Add-OpusVariant([string]$Source, $Row) {
    # Одного 24-кбит/с примера из четырёх хватает как канального augmentation;
    # каждый wav отдельно гонять через два процесса ffmpeg слишком накладно.
    if (-not $WithOpus -or -not $ffmpeg -or ($index % 4) -ne 0) { return }
    $stem = [IO.Path]::GetFileNameWithoutExtension($Source)
    $ogg = Join-Path $data ($stem + '-opus24k.ogg')
    $dest = Join-Path $data ($stem + '-opus24k.wav')
    & $ffmpeg.Source -v error -y -i $Source -ac 1 -ar 16000 -c:a libopus -b:a 24k -application voip -vbr off $ogg
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg не смог закодировать $Source" }
    & $ffmpeg.Source -v error -y -i $ogg -ac 1 -ar 16000 -c:a pcm_s16le $dest
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg не смог декодировать $ogg" }
    Remove-Item -LiteralPath $ogg -Force
    $copy = [pscustomobject]@{ file=[IO.Path]::GetFileName($dest); label=$Row.label; voice=$Row.voice; text=$Row.text; keyword_start_s=$Row.keyword_start_s; keyword_end_s=$Row.keyword_end_s; rate=$Row.rate; pitch=$Row.pitch; position=$Row.position; augmentation='opus24k' }
    $rows.Add($copy)
}

function Apply-Pitch([string]$Path, [int]$Semitones) {
    if ($Semitones -eq 0 -or -not $ffmpeg) { return 1.0 }
    $originalBytes = Get-WavDataBytes $Path
    $factor = [Math]::Pow(2.0, $Semitones / 12.0)
    $factorText = $factor.ToString('0.000000', [Globalization.CultureInfo]::InvariantCulture)
    $tempoText = (1.0 / $factor).ToString('0.000000', [Globalization.CultureInfo]::InvariantCulture)
    $tempPath = [IO.Path]::ChangeExtension($Path, '.pitched.wav')
    & $ffmpeg.Source -v error -y -i $Path -af "asetrate=16000*$factorText,aresample=16000,atempo=$tempoText" -ac 1 -ar 16000 -c:a pcm_s16le $tempPath
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg не смог изменить высоту: $Path" }
    $pitchScale = (Get-WavDataBytes $tempPath) / [double]$originalBytes
    Move-Item -LiteralPath $tempPath -Destination $Path -Force
    return $pitchScale
}

# Слова произносятся отдельным сегментом: манифест хранит точную позицию для обучения.
$wake = @(
    @{ word='ривер'; label='1' }, @{ word='ревер'; label='1' }, @{ word='ривьер'; label='1' },
    @{ word='river'; label='1' }, @{ word='reever'; label='1' }
)
$negatives = @(
    'включи реверс на гитаре', 'мотор ревёт за окном', 'мы вышли к ривьере', 'на обед будет печёночный ливер',
    'я приверженец простого интерфейса', 'говори без обращения к помощнику', 'сегодня обсудим планы и новости',
    'проверим документы и отправим результат команде', 'включи запись и поделись экраном', 'это короткий разговор о работе'
)
$prefixes = @('', 'слушай ', 'давай уточним, ', 'у меня вопрос, ')
$suffixes = @(', открой заметки', ', подскажи погоду', ', посмотри это', ', расскажи подробнее', '')

$voiceIndex = 0
foreach ($token in $voices) {
    $voiceName = [string]$token.GetDescription()
    $languageTags = ([string]$token.GetAttribute('Language')).Split(';', [StringSplitOptions]::RemoveEmptyEntries)
    $isEnglish = $languageTags | Where-Object { $_.Trim() -match '^(409|0409)$' } | Select-Object -First 1
    $prefixesForVoice = if ($isEnglish) { @('', 'hey ', 'hello ', 'please ') } else { $prefixes }
    $suffixesForVoice = if ($isEnglish) { @(', open notes', ', check this', ', tell me more', ', look it up', '') } else { $suffixes }
    $negativePhrases = if ($isEnglish) {
        @('reverse the guitar direction', 'the engine roars outside', 'we went to the riviera', 'liver for lunch',
          'I prefer a simple interface', 'let us discuss work and plans', 'check the documents and send the result',
          'open the meeting and share the screen', 'this is a short conversation', 'drive near the bridge')
    } else { $negatives }
    $positiveWords = @($wake | Where-Object {
        ($_.word -in @('river', 'reever')) -eq [bool]$isEnglish
    })
    $rateIndex = 0
    foreach ($rate in $Rates) {
        $pitchIndex = 0
        foreach ($pitch in $pitchValues) {
            # Девять комбинаций на голос покрывают все варианты слова, все три
            # позиции во фразе и каждую скорость/высоту без лишнего дублирования.
            $item = $positiveWords[($voiceIndex + $pitchIndex) % $positiveWords.Count]
            $position = @('begin', 'middle', 'end')[$rateIndex % 3]
            $prefix = ''
            $suffix = ''
            if ($position -ne 'begin') {
                $prefix = $prefixesForVoice[1 + (($voiceIndex + $pitchIndex) % ($prefixesForVoice.Count - 1))]
            }
            if ($position -eq 'middle') {
                $suffix = $suffixesForVoice[($voiceIndex + $rateIndex + $pitchIndex) % ($suffixesForVoice.Count - 1)]
            } elseif ($position -eq 'begin') {
                $suffix = $suffixesForVoice[($voiceIndex + $rateIndex + $pitchIndex) % $suffixesForVoice.Count]
            }
            $prefixPcm = if ($prefix) { Get-Pcm $prefix $token $rate } else { [byte[]]@() }
            $wordPcm = Get-Pcm $item.word $token $rate
            $suffixPcm = if ($suffix) { Get-Pcm $suffix $token $rate } else { [byte[]]@() }
            if (($prefix -and -not $prefixPcm.Length) -or -not $wordPcm.Length -or ($suffix -and -not $suffixPcm.Length)) {
                $pitchIndex++
                continue
            }
            $pcm = New-Object System.Collections.Generic.List[byte]
            if ($prefixPcm.Length) { $pcm.AddRange($prefixPcm); $pcm.AddRange([byte[]]::new(3200)) }
            $kwStart = $pcm.Count / 32000.0
            $pcm.AddRange($wordPcm)
            $kwEnd = $pcm.Count / 32000.0
            if ($suffixPcm.Length) { $pcm.AddRange([byte[]]::new(3200)); $pcm.AddRange($suffixPcm) }
            $phraseDuration = $pcm.Count / 32000.0
            $name = 'p-{0:D6}.wav' -f $index
            $path = Join-Path $data $name
            Write-Wav $path $pcm.ToArray()
            $pitchScale = Apply-Pitch $path $pitch
            $kwStart *= $pitchScale
            $kwEnd *= $pitchScale
            $phraseDuration *= $pitchScale
            $position = if ($kwStart -le 0.01) { 'begin' } elseif ($kwEnd -ge $phraseDuration - 0.01) { 'end' } else { 'middle' }
            $row = [pscustomobject]@{ file=$name; label=1; voice=$voiceName; text=($prefix + $item.word + $suffix); keyword_start_s=$kwStart.ToString('0.000000', [Globalization.CultureInfo]::InvariantCulture); keyword_end_s=$kwEnd.ToString('0.000000', [Globalization.CultureInfo]::InvariantCulture); rate=$rate; pitch=$pitch; position=$position; augmentation='pcm' }
            $rows.Add($row)
            Add-OpusVariant $path $row
            $index++

            # Для каждого SAPI-голоса сохраняем hard negatives при нейтральной
            # скорости: полный speed/pitch cross-product нужен позитивному классу.
            if ($rate -eq 0 -and $pitch -eq 0) {
                foreach ($text in ($negativePhrases | Select-Object -First 5)) {
                    $pcm = Get-Pcm $text $token $rate
                    if (-not $pcm.Length) { continue }
                    $name = 'n-{0:D6}.wav' -f $index
                    $path = Join-Path $data $name
                    Write-Wav $path $pcm
                    $row = [pscustomobject]@{ file=$name; label=0; voice=$voiceName; text=$text; keyword_start_s=0; keyword_end_s=0; rate=$rate; pitch=$pitch; augmentation='pcm' }
                    $rows.Add($row)
                    Add-OpusVariant $path $row
                    $index++
                }
            }
            $pitchIndex++
        }
        $rateIndex++
    }
    $voiceIndex++
}

$rows | Export-Csv -NoTypeInformation -Encoding UTF8 -Path $manifest
Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
Write-Host "Готово: $($rows.Count) WAV -> $data; manifest=$manifest"
& python (Join-Path $PSScriptRoot 'generate_noise.py') --manifest $manifest
if ($LASTEXITCODE -ne 0) { throw 'generate_noise.py failed' }
& (Join-Path $PSScriptRoot 'kws-general-negatives.ps1')
& python (Join-Path $PSScriptRoot 'make_external.py') --repo-root $root --out (Join-Path $PSScriptRoot 'data\external-testdata.csv') --live-root (Join-Path $PSScriptRoot 'data\no-live')
if ($LASTEXITCODE -ne 0) { throw 'make_external.py failed' }
