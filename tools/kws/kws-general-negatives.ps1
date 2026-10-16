[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$data = Join-Path $PSScriptRoot 'data\generated'
$manifest = Join-Path $data 'manifest.csv'
if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
    throw "Сначала соберите основной корпус: $manifest не найден"
}

$voice = New-Object -ComObject SAPI.SpVoice
$voices = @($voice.GetVoices())
$format = New-Object -ComObject SAPI.SpAudioFormat
$format.Type = 18 # SAFT16kHz16BitMono
$tmp = Join-Path $env:TEMP 'reever-kws-general-negative.wav'

function Get-Pcm([string]$Text, $Token) {
    $stream = New-Object -ComObject SAPI.SpFileStream
    $stream.Format = $format
    $stream.Open($tmp, 3, $false)
    $voice.Voice = $Token
    $voice.Rate = 0
    $voice.AudioOutputStream = $stream
    [void]$voice.Speak($Text)
    $stream.Close()
    $bytes = [IO.File]::ReadAllBytes($tmp)
    for ($offset = 12; $offset + 8 -le $bytes.Length;) {
        $chunk = [Text.Encoding]::ASCII.GetString($bytes, $offset, 4)
        $size = [BitConverter]::ToInt32($bytes, $offset + 4)
        if ($chunk -eq 'data') {
            $pcm = New-Object byte[] $size
            [Array]::Copy($bytes, $offset + 8, $pcm, 0, $size)
            return ,$pcm
        }
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

$existing = @(Import-Csv -LiteralPath $manifest -Encoding UTF8)
$knownFiles = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($row in $existing) { [void]$knownFiles.Add([string]$row.file) }
$added = New-Object System.Collections.Generic.List[object]
$voiceIndex = 0
foreach ($token in $voices) {
    $voiceName = [string]$token.GetDescription()
    $languageTags = ([string]$token.GetAttribute('Language')).Split(';', [StringSplitOptions]::RemoveEmptyEntries)
    $isEnglish = [bool]($languageTags | Where-Object { $_.Trim() -match '^(409|0409)$' } | Select-Object -First 1)
    $phrases = if ($isEnglish) {
        @('let us discuss work and plans', 'check the documents and send the result',
          'open the meeting and share the screen', 'this is a short conversation', 'drive near the bridge')
    } else {
        @('говори без обращения к помощнику', 'сегодня обсудим планы и новости',
          'проверим документы и отправим результат команде', 'включи запись и поделись экраном',
          'это короткий разговор о работе')
    }
    $phraseIndex = $voiceIndex % $phrases.Count
    $name = 'n-general-v{0:D3}.wav' -f $voiceIndex
    if (-not $knownFiles.Contains($name)) {
        $pcm = Get-Pcm $phrases[$phraseIndex] $token
        if ($pcm.Length) {
            Write-Wav (Join-Path $data $name) $pcm
            $added.Add([pscustomobject][ordered]@{
                augmentation='pcm'; file=$name; keyword_end_s='0'; keyword_start_s='0'; label='0'
                pitch='0'; position=''; rate='0'; text=$phrases[$phraseIndex]; voice=$voiceName
            })
        }
    }
    $voiceIndex++
}

if ($added.Count -gt 0) {
    $all = New-Object System.Collections.Generic.List[object]
    $all.AddRange([object[]]$existing)
    $all.AddRange($added.ToArray())
    $all | Export-Csv -NoTypeInformation -Encoding UTF8 -Path $manifest
}
if (Test-Path -LiteralPath $tmp -PathType Leaf) { [IO.File]::Delete($tmp) }
Write-Host "Добавлено общих SAPI-негативов: $($added.Count); голосов=$voiceIndex; manifest=$manifest"
