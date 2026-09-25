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
$RhvoicePreferred = @('Alexandr (Russian)', 'Ekaterina (Russian)', 'Dmitriy (Russian)',
                      'Vika (Russian)', 'Roman (Russian)')

# System.Speech (.NET) с голосами RHVoice пишет только первые ~0,1 с фразы —
# поэтому синтез идёт напрямую через COM SAPI, где RHVoice работает целиком.
$voice = New-Object -ComObject SAPI.SpVoice
# Через COM описание голоса длиннее, чем в .NET («Microsoft Irina Desktop - Russian»),
# поэтому сопоставляем по префиксу; «Alexandr (Erzya)» при этом не подходит под «Alexandr (Russian)».
$tokens = @{}
foreach ($name in ($Preferred + @('Microsoft Zira Desktop'))) {
    foreach ($t in $voice.GetVoices()) {
        if ($t.GetDescription() -like "$name*") { $tokens[$name] = $t; break }
    }
}
$pool = @($Preferred | Where-Object { $tokens.ContainsKey($_) })
if ($pool.Count -eq 0) { throw 'не найдено ни одного русского голоса SAPI' }
$rhvoicePool = @($RhvoicePreferred | Where-Object { $tokens.ContainsKey($_) })
if ($rhvoicePool.Count -eq 0) { throw 'не найдено ни одного русского голоса RHVoice SAPI' }
if (-not $tokens.ContainsKey('Microsoft Zira Desktop')) { throw 'не найден голос Microsoft Zira Desktop SAPI' }
Write-Host "голоса: $($pool -join ', ')"
Write-Host "RHVoice для английской цели: $($rhvoicePool -join ', ')"

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
function Add-Case($scenario, $parts, $pauses, $expected, $wakeHeard, $expectedLang = 'ru', $expectedPrefix = '', $expectedPayload = '') {
    $cases.Add([pscustomobject]@{ scenario = $scenario; parts = $parts; pauses = $pauses;
                                  expected = $expected; expected_lang = $expectedLang;
                                  expected_prefix = $expectedPrefix; expected_payload = $expectedPayload;
                                  wake_heard = $wakeHeard })
}

function New-Part([string]$text, [string]$voiceName = '') {
    [pscustomobject]@{ text = $text; voice = $voiceName }
}

# Раскладка по умолчанию (docs/grammar.md): программа — без глагола, папка — «папка» /
# «открой папку», Google — «загугли», YouTube — «поиск ютуб». Старые формы с глаголом
# («открой фотошоп», «запусти доту два») тоже должны работать — они в списке.
$commands = @('фотошоп', 'дискорд', 'стим', 'спотифай', 'юнити хаб', 'дота два',
              'открой фотошоп', 'запусти доту два',
              'папка загрузки', 'открой папку документы', 'папка рабочий стол',
              'загугли погода в москве', 'поиск ютуб смешные коты',
              'выключи звук', 'следующий трек', 'пауза', 'плей')
foreach ($c in $commands) { Add-Case 'continuous' @("ривер $c") @() @($c) $false }
foreach ($c in @('фотошоп', 'папка загрузки', 'загугли погода в москве', 'поиск ютуб смешные коты', 'выключи звук', 'дискорд')) {
    Add-Case 'wake_pause' @('ривер', $c) @(1300) @($c) $true
}
# Ключевое слово без цели + пауза: Engine должен дослушать продолжение.
Add-Case 'verb_pause' @('ривер папка', 'загрузки') @(900) @('папка загрузки') $true
Add-Case 'verb_pause' @('ривер поиск ютуб', 'смешные коты') @(900) @('поиск ютуб смешные коты') $true
Add-Case 'verb_pause' @('ривер загугли', 'рецепт блинов') @(900) @('загугли рецепт блинов') $true
Add-Case 'verb_pause' @('ривер открой', 'фотошоп') @(900) @('открой фотошоп') $true
Add-Case 'inner_pause' @('ривер открой папку', 'документы') @(300) @('открой папку документы') $false
Add-Case 'inner_pause' @('ривер загугли', 'рецепт блинов') @(300) @('загугли рецепт блинов') $false
$negatives = @('я сегодня ходил в магазин за хлебом', 'включи реверс на гитаре',
               'ривьера это курорт на море', 'поставь чайник пожалуйста',
               'открой окно в комнате', 'где мой телефон', 'мотор ревёт за окном',
               'давай поиграем вечером')
foreach ($n in $negatives) { Add-Case 'negative' @($n) @() @() $false }

# Английскую цель читаем Zira, сохраняя русскую фразу-триггер в русском голосе.
$englishPayloads = @(
    [pscustomobject]@{ prefix = 'ривер поиск ютуб'; trigger = 'поиск ютуб'; payload = "sinksaiko i'm sloth" },
    [pscustomobject]@{ prefix = 'ривер поиск ютуб'; trigger = 'поиск ютуб'; payload = 'never gonna give you up' },
    [pscustomobject]@{ prefix = 'ривер поиск ютуб'; trigger = 'поиск ютуб'; payload = 'lofi hip hop radio' },
    [pscustomobject]@{ prefix = 'ривер загугли'; trigger = 'загугли'; payload = 'how to cook pasta' },
    [pscustomobject]@{ prefix = 'ривер загугли'; trigger = 'загугли'; payload = 'rust borrow checker' },
    [pscustomobject]@{ prefix = 'ривер папка'; trigger = 'папка'; payload = 'downloads' },
    [pscustomobject]@{ prefix = 'ривер папка'; trigger = 'папка'; payload = 'documents' },
    [pscustomobject]@{ prefix = 'ривер папка'; trigger = 'папка'; payload = 'screenshots' }
)
foreach ($c in $englishPayloads) {
    $expected = "$($c.trigger) $($c.payload)"
    Add-Case 'en_payload' @((New-Part $c.prefix), (New-Part $c.payload 'Microsoft Zira Desktop')) `
        @(900) @($expected) $true 'en' $c.trigger $c.payload
    Add-Case 'en_accent' @((New-Part $c.prefix), (New-Part $c.payload '__rhvoice__')) `
        @(900) @($expected) $true 'en' $c.trigger $c.payload
}

# Bare-launch: после «ривер» ключевая фраза не нужна, цель произносит Zira.
foreach ($app in @('minecraft', 'discord', 'photoshop')) {
    Add-Case 'en_bare' @((New-Part 'ривер'), (New-Part $app 'Microsoft Zira Desktop')) `
        @(900) @($app) $true 'en' '' $app
}

# Русский контроль: второй язык не должен переключать цель на английский.
$russianControls = @(
    [pscustomobject]@{ prefix = 'ривер поиск ютуб'; trigger = 'поиск ютуб'; payload = 'смешные коты' },
    [pscustomobject]@{ prefix = 'ривер загугли'; trigger = 'загугли'; payload = 'погода в москве' },
    [pscustomobject]@{ prefix = 'ривер папка'; trigger = 'папка'; payload = 'загрузки' }
)
foreach ($c in $russianControls) {
    $expected = "$($c.trigger) $($c.payload)"
    Add-Case 'ru_payload_control' @((New-Part $c.prefix), (New-Part $c.payload)) `
        @(900) @($expected) $true 'ru' $c.trigger $c.payload
}

$rows = New-Object System.Collections.Generic.List[object]
$i = 0
foreach ($case in $cases) {
    for ($v = 0; $v -lt [Math]::Min($Voices, $pool.Count); $v++) {
        $voiceName = $pool[($i + $v) % $pool.Count]
        $file = '{0:D3}_{1}.wav' -f $rows.Count, $case.scenario
        $pcm = New-Object System.Collections.Generic.List[byte]
        for ($p = 0; $p -lt $case.parts.Count; $p++) {
            if ($p -gt 0) { $pcm.AddRange([byte[]]::new(32 * $case.pauses[$p - 1])) }
            $part = $case.parts[$p]
            if ($part -is [string]) {
                $partText = $part
                $partVoice = $voiceName
            } else {
                $partText = [string]$part.text
                $partVoice = [string]$part.voice
                if ([string]::IsNullOrWhiteSpace($partVoice)) { $partVoice = $voiceName }
                elseif ($partVoice -eq '__rhvoice__') {
                    $partVoice = $rhvoicePool[$rows.Count % $rhvoicePool.Count]
                }
            }
            $pcm.AddRange((Get-Pcm $partText $partVoice ($rows.Count % 3)))
        }
        Write-Wav (Join-Path $OutDir $file) $pcm.ToArray()
        $spokenParts = @($case.parts | ForEach-Object { if ($_ -is [string]) { $_ } else { $_.text } })
        $rows.Add([pscustomobject]@{ file = $file; scenario = $case.scenario; voice = $voiceName;
                                     text = ($spokenParts -join ' / '); expected = @($case.expected);
                                     expected_lang = $case.expected_lang; expected_prefix = $case.expected_prefix;
                                     expected_payload = $case.expected_payload;
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
