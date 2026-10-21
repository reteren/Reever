[CmdletBinding()]
param(
    # Дикторы корпуса testdata/manifest.json — не для обучения: на них проверяется русский.
    # Сравнение по имени диктора: у RHVoice один голос бывает под несколькими языками
    # («Ekaterina (Chuvash)» — та же Екатерина).
    [string[]]$Exclude = @('Irina', 'Alexandr', 'Ekaterina', 'Dmitriy', 'Vika', 'Roman'),
    [int[]]$Rates = @(-4, -1, 2, 5)
)
# Русские (и украинские) голоса Windows (SAPI, RHVoice) читают «Ривер», созвучия и обычную
# речь — в tools/kws/data/sapi/<голос>/ со списком list.tsv; готовые голоса при повторном
# запуске пропускаются. gen_sapi_import.py обрезает тишину и дописывает клипы в корпус.

$ErrorActionPreference = 'Stop'
$out = Join-Path $PSScriptRoot 'data\sapi'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$voice = New-Object -ComObject SAPI.SpVoice
$format = New-Object -ComObject SAPI.SpAudioFormat
$format.Type = 18 # SAFT16kHz16BitMono
$wakes = @('Ривер.', 'Ривер!', 'Ривер?', 'Ривер, открой телеграм.', 'Ривер, пауза.', 'Ривер, напиши привет.', 'Ривер стоп.', 'Ревер.', 'Ривьер.', 'River.')
$confusable = @('Реверс.', 'Ревёт.', 'Привет.', 'Ривьера.', 'Вера.', 'Ливер.', 'Ровер.', 'Верно.', 'Привезли.', 'Ирина.', 'Рыбы.', 'Ребята.')
$speech = @(
    'Который сейчас час?', 'Я схожу в магазин за хлебом.', 'Пришли мне отчёт завтра утром.',
    'Открой, пожалуйста, папку с документами.', 'Сегодня на улице довольно холодно.', 'Давай созвонимся вечером после работы.',
    'Поставь музыку погромче.', 'Мне нужно доделать презентацию к пятнице.', 'Где лежат ключи от машины?',
    'Включи свет на кухне.', 'Я не успеваю на встречу, начинайте без меня.', 'Посмотри, что пишут в новостях.'
)
$n = 0
foreach ($token in $voice.GetVoices()) {
    $name = $token.GetDescription()
    $lang = $token.GetAttribute('Language')
    # 419 — русский, 422 — украинский (шестнадцатеричный LCID).
    $speaker = ($name -replace '^Microsoft\s+', '').Split(' ')[0]
    if ($lang -notmatch '(^|;)(419|422)($|;)' -or $Exclude -contains $speaker) { continue }
    $dir = Join-Path $out (($name -replace '[^\w]+', '_').Trim('_'))
    if (Test-Path (Join-Path $dir 'list.tsv')) { continue }
    # Один диктор под разными языками — один тембр: берём первый вариант.
    if (Get-ChildItem $out -Directory -Filter "$($speaker)_*" | Where-Object { Test-Path (Join-Path $_.FullName 'list.tsv') }) { continue }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $rows = New-Object System.Collections.Generic.List[string]
    $jobs = @()
    foreach ($text in $wakes) { foreach ($rate in $Rates) { $jobs += ,@('wake', $text, $rate) } }
    foreach ($text in $confusable) { $jobs += ,@('confusable', $text, $Rates[($n++) % $Rates.Count]) }
    foreach ($text in $speech) { $jobs += ,@('speech', $text, $Rates[($n++) % $Rates.Count]) }
    $i = 0
    foreach ($job in $jobs) {
        $file = Join-Path $dir ("{0}-{1:d3}.wav" -f $job[0], $i++)
        $stream = New-Object -ComObject SAPI.SpFileStream
        $stream.Format = $format
        $stream.Open($file, 3, $false)
        $voice.Voice = $token
        $voice.Rate = $job[2]
        $voice.AudioOutputStream = $stream
        [void]$voice.Speak($job[1])
        $stream.Close()
        $rows.Add(("{0}`t{1}`t{2}`t{3}`t{4}" -f $name, $job[0], $job[1], $job[2], $file))
    }
    [IO.File]::WriteAllLines((Join-Path $dir 'list.tsv'), $rows, [Text.UTF8Encoding]::new($false))
    Write-Host "$name ($lang): $($jobs.Count) клипов"
}
