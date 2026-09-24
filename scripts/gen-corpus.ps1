<#
.SYNOPSIS
    Синтезирует тестовый корпус голосовых команд Reever (WAV + manifest.json).

.DESCRIPTION
    Фразы озвучиваются установленными русскими голосами SAPI (Microsoft Irina
    и голоса RHVoice) сразу в 16 кГц mono 16-bit — в том же формате, что отдаёт
    микрофонный конвейер. Части фразы синтезируются отдельно и склеиваются с
    настоящей цифровой тишиной, чтобы проверять оба стиля речи: «ривер» + пауза +
    команда и слитно.

    WAV кладутся в testdata/corpus/ (в .gitignore), манифест — в
    testdata/manifest.json (в git). Прогон: cargo run --release --example eval.

.PARAMETER Voices
    Сколько голосов использовать на каждую фразу (по кругу из доступных).
#>
[CmdletBinding()]
param([int]$Voices = 2)

$ErrorActionPreference = 'Stop'

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$OutDir    = Join-Path $RepoRoot 'testdata/corpus'
$Manifest  = Join-Path $RepoRoot 'testdata/manifest.json'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Get-ChildItem -LiteralPath $OutDir -Filter '*.wav' | Remove-Item -Force

# Предпочтительные голоса: мужские и женские, разные движки.
$Preferred = @('Microsoft Irina Desktop', 'Alexandr (Russian)', 'Ekaterina (Russian)',
               'Dmitriy (Russian)', 'Vika (Russian)', 'Roman (Russian)')

# System.Speech (.NET) с голосами RHVoice пишет только первые ~0,1 с фразы —
# поэтому синтез идёт напрямую через COM SAPI, где RHVoice работает целиком.
$voice = New-Object -ComObject SAPI.SpVoice
# Через COM описание голоса длиннее, чем в .NET («Microsoft Irina Desktop - Russian»),
# поэтому сопоставляем по префиксу; «Alexandr (Erzya)» при этом не подходит под «Alexandr (Russian)».
$tokens = @{}
foreach ($name in $Preferred) {
    foreach ($t in $voice.GetVoices()) {
        if ($t.GetDescription() -like "$name*") { $tokens[$name] = $t; break }
    }
}
$pool = @($Preferred | Where-Object { $tokens.ContainsKey($_) })
if ($pool.Count -eq 0) { throw 'не найдено ни одного русского голоса SAPI' }
Write-Host "голоса: $($pool -join ', ')"

$SAFT16kHz16BitMono = 18
$SSFMCreateForWrite = 3
$TmpWav = Join-Path ([IO.Path]::GetTempPath()) 'reever-corpus-part.wav'

# PCM 16 кГц mono 16-bit одной фразы (без WAV-заголовка).
function Get-Pcm([string]$text, [string]$voiceName, [int]$rateIdx) {
    $fmt = New-Object -ComObject SAPI.SpAudioFormat
    $fmt.Type = $SAFT16kHz16BitMono
    $stream = New-Object -ComObject SAPI.SpFileStream
    $stream.Format = $fmt
    $stream.Open($TmpWav, $SSFMCreateForWrite, $false)
    $voice.Voice = $tokens[$voiceName]
    $voice.Rate = @(0, -1, 1)[$rateIdx]
    $voice.AudioOutputStream = $stream
    [void]$voice.Speak($text)
    $stream.Close()
    $bytes = [IO.File]::ReadAllBytes($TmpWav)
    # Заголовок SAPI бывает разной длины — ищем чанк data.
    for ($o = 12; $o -lt $bytes.Length - 8; ) {
        $id = [Text.Encoding]::ASCII.GetString($bytes, $o, 4)
        $size = [BitConverter]::ToInt32($bytes, $o + 4)
        if ($id -eq 'data') {
            $size = [Math]::Min($size, $bytes.Length - $o - 8)
            $pcm = New-Object byte[] $size
            [Array]::Copy($bytes, $o + 8, $pcm, 0, $size)
            return ,$pcm
        }
        $o += 8 + $size + ($size % 2)
    }
    throw "в WAV от SAPI нет чанка data ($voiceName)"
}

function Write-Wav([string]$path, [byte[]]$pcm) {
    $ms = New-Object IO.MemoryStream
    $w = New-Object IO.BinaryWriter($ms)
    $w.Write([Text.Encoding]::ASCII.GetBytes('RIFF')); $w.Write([int](36 + $pcm.Length))
    $w.Write([Text.Encoding]::ASCII.GetBytes('WAVEfmt ')); $w.Write([int]16)
    $w.Write([int16]1); $w.Write([int16]1); $w.Write([int]16000); $w.Write([int]32000)
    $w.Write([int16]2); $w.Write([int16]16)
    $w.Write([Text.Encoding]::ASCII.GetBytes('data')); $w.Write([int]$pcm.Length); $w.Write($pcm)
    [IO.File]::WriteAllBytes($path, $ms.ToArray())
}

# Сценарий: части фразы и паузы между ними (мс); expected — ожидаемые команды
# Engine (текст без wake-слова); wake_heard — ждём ли сигнал «слушаю».
$cases = New-Object System.Collections.Generic.List[object]
function Add-Case($scenario, $parts, $pauses, $expected, $wakeHeard) {
    $cases.Add([pscustomobject]@{ scenario = $scenario; parts = $parts; pauses = $pauses;
                                  expected = $expected; wake_heard = $wakeHeard })
}

$commands = @('открой загрузки', 'запусти фотошоп', 'загугли погода в москве', 'выключи звук',
              'открой папку документы', 'запусти доту два', 'следующий трек', 'пауза',
              'ютуб смешные коты', 'запусти дискорд', 'открой рабочий стол', 'запусти стим',
              'запусти спотифай', 'запусти юнити хаб', 'плей')
foreach ($c in $commands) { Add-Case 'continuous' @("ривер $c") @() @($c) $false }
foreach ($c in $commands[0..5]) { Add-Case 'wake_pause' @('ривер', $c) @(1300) @($c) $true }
Add-Case 'verb_pause' @('ривер открой', 'загрузки') @(900) @('открой загрузки') $true
Add-Case 'verb_pause' @('ривер запусти', 'фотошоп') @(900) @('запусти фотошоп') $true
Add-Case 'inner_pause' @('ривер открой', 'папку документы') @(300) @('открой папку документы') $false
Add-Case 'inner_pause' @('ривер загугли', 'рецепт блинов') @(300) @('загугли рецепт блинов') $false
$negatives = @('я сегодня ходил в магазин за хлебом', 'включи реверс на гитаре',
               'ривьера это курорт на море', 'поставь чайник пожалуйста',
               'открой окно в комнате', 'где мой телефон', 'мотор ревёт за окном',
               'давай поиграем вечером')
foreach ($n in $negatives) { Add-Case 'negative' @($n) @() @() $false }

$rows = New-Object System.Collections.Generic.List[object]
$i = 0
foreach ($case in $cases) {
    for ($v = 0; $v -lt [Math]::Min($Voices, $pool.Count); $v++) {
        $voiceName = $pool[($i + $v) % $pool.Count]
        $file = '{0:D3}_{1}.wav' -f $rows.Count, $case.scenario
        $pcm = New-Object System.Collections.Generic.List[byte]
        for ($p = 0; $p -lt $case.parts.Count; $p++) {
            if ($p -gt 0) { $pcm.AddRange([byte[]]::new(32 * $case.pauses[$p - 1])) }
            $pcm.AddRange((Get-Pcm $case.parts[$p] $voiceName ($rows.Count % 3)))
        }
        Write-Wav (Join-Path $OutDir $file) $pcm.ToArray()
        $rows.Add([pscustomobject]@{ file = $file; scenario = $case.scenario; voice = $voiceName;
                                     text = ($case.parts -join ' / '); expected = @($case.expected);
                                     wake_heard = $case.wake_heard })
    }
    $i++
}
Remove-Item -LiteralPath $TmpWav -Force -ErrorAction SilentlyContinue

# ConvertTo-Json в Windows PowerShell 5.1 пишет UTF-16 при Out-File — пишем явно UTF-8 без BOM.
$json = ConvertTo-Json -InputObject $rows.ToArray() -Depth 4
$utf8 = New-Object System.Text.UTF8Encoding -ArgumentList $false
[IO.File]::WriteAllText($Manifest, $json, $utf8)
Write-Host "готово: $($rows.Count) файлов -> $OutDir"
