[CmdletBinding()]
param(
    [string[]]$Exclude = @('Irina', 'Alexandr', 'Ekaterina', 'Dmitriy', 'Vika', 'Roman')
)
# Слитные команды «ривер открой фотошоп» голосами Windows (как gen_phrases.py для piper):
# data/sapi/<голос>/phrase-*.wav и phrases.tsv; gen_sapi_import.py добавит их в корпус.

$ErrorActionPreference = 'Stop'
$out = Join-Path $PSScriptRoot 'data\sapi'
$voice = New-Object -ComObject SAPI.SpVoice
$format = New-Object -ComObject SAPI.SpAudioFormat
$format.Type = 18
$commands = @('открой фотошоп', 'открой телеграм', 'закрой хром', 'запусти доту', 'поставь паузу', 'найди файл отчёт',
    'открой папку загрузки', 'выключи звук', 'сделай громче', 'напиши привет', 'открой ютуб', 'закрой окно',
    'сверни всё', 'открой калькулятор', 'стоп', 'следующий трек', 'включи музыку', 'открой браузер',
    'открой дискорд', 'фотошоп', 'телеграм', 'пауза', 'дота два', 'открой блокнот')
$rates = @(-2, 0, 2, 4)
$k = 0
foreach ($token in $voice.GetVoices()) {
    $name = $token.GetDescription()
    $speaker = ($name -replace '^Microsoft\s+', '').Split(' ')[0]
    if ($token.GetAttribute('Language') -notmatch '(^|;)419($|;)' -or $Exclude -contains $speaker) { continue }
    $dir = Join-Path $out (($name -replace '[^\w]+', '_').Trim('_'))
    # Только дикторы, уже синтезированные gen_sapi.ps1 (один вариант на диктора).
    if (-not (Test-Path (Join-Path $dir 'list.tsv')) -or (Test-Path (Join-Path $dir 'phrases.tsv'))) { continue }
    $rows = New-Object System.Collections.Generic.List[string]
    $i = 0
    foreach ($command in $commands) {
        $text = "ривер $command"
        $rate = $rates[($k++) % $rates.Count]
        $file = Join-Path $dir ("phrase-{0:d3}.wav" -f $i++)
        $stream = New-Object -ComObject SAPI.SpFileStream
        $stream.Format = $format
        $stream.Open($file, 3, $false)
        $voice.Voice = $token
        $voice.Rate = $rate
        $voice.AudioOutputStream = $stream
        [void]$voice.Speak($text)
        $stream.Close()
        $rows.Add(("{0}`t{1}`t{2}`t{3}`t{4}" -f $name, 'wake_phrase', $text, $rate, $file))
    }
    [IO.File]::WriteAllLines((Join-Path $dir 'phrases.tsv'), $rows, [Text.UTF8Encoding]::new($false))
    Write-Host "${name}: $($rows.Count)"
}
