<#
.SYNOPSIS
    Создаёт расширенный SAPI-корпус русской речи с английскими терминами.
.DESCRIPTION
    Запуск из корня Reever: .\scripts\gen-mix2-corpus.ps1
    Манифест записывается до синтеза, чтобы его можно было читать во время генерации.
#>
[CmdletBinding()]
param(
    [switch]$ManifestOnly
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$OutDir = Join-Path $RepoRoot 'testdata/corpus'
$Manifest = Join-Path $RepoRoot 'testdata/mix2-manifest.json'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$voiceBases = @(
    'Microsoft Irina Desktop',
    'Alexandr (Russian)',
    'Ekaterina (Russian)',
    'Dmitriy (Russian)',
    'Vika (Russian)',
    'Roman (Russian)'
)
$voice = New-Object -ComObject SAPI.SpVoice
$installedVoices = @($voice.GetVoices())
$voices = New-Object System.Collections.Generic.List[object]
foreach ($base in $voiceBases) {
    $token = $null
    foreach ($candidate in $installedVoices) {
        if ($candidate.GetDescription() -like "$base*") { $token = $candidate; break }
    }
    if ($null -eq $token) { throw "Не найден русский голос SAPI: $base" }
    $voices.Add([pscustomobject]@{ Name = $token.GetDescription(); Token = $token })
}

$termCases = @(
    @{ term='hardness'; phrases=@('Для камня выставь hardness на сорок процентов'; 'hardness у этой кисти слишком высокая') },
    @{ term='opacity'; phrases=@('Opacity слоя поставь на семьдесят процентов'; 'Снизь opacity фона до двадцати') },
    @{ term='roughness'; phrases=@('Добавь roughness поверхности металла'; 'Для пластика roughness убавь до нуля') },
    @{ term='escape'; phrases=@('Нажми escape, чтобы закрыть окно'; 'escape нажми после отмены') },
    @{ term='eraser'; phrases=@('Выбери eraser для удаления лишнего'; 'eraser на панели находится слева') },
    @{ term='tool'; phrases=@('tool для обрезки находится справа'; 'Сначала выбери нужный tool') },
    @{ term='PNG'; phrases=@('Сохрани иллюстрацию как PNG'; 'PNG для этой картинки будет достаточно') },
    @{ term='JPEG'; phrases=@('Экспортируй снимок в JPEG'; 'JPEG подойдёт для отправки по почте') },
    @{ term='layer'; phrases=@('Поставь layer над фоном'; 'layer с текстом заблокируй') },
    @{ term='brush'; phrases=@('Выбери brush помягче'; 'brush для волос сделай меньше') },
    @{ term='canvas'; phrases=@('Увеличь canvas по ширине'; 'canvas оставь белым') },
    @{ term='scale'; phrases=@('Поставь scale на пятьдесят процентов'; 'scale изображения измени перед экспортом') },
    @{ term='size'; phrases=@('Уменьши size кисти до двенадцати'; 'size окна можно увеличить') },
    @{ term='preview'; phrases=@('Покажи preview перед отправкой'; 'preview результата открой справа') },
    @{ term='render'; phrases=@('Запусти render этой сцены'; 'render лучше сделать ночью') },
    @{ term='shader'; phrases=@('Проверь shader на новой модели'; 'shader для воды выглядит слишком ярким') },
    @{ term='commit'; phrases=@('Сделай commit после проверки'; 'commit с этим исправлением уже готов') },
    @{ term='merge'; phrases=@('Перед merge проверь изменения'; 'merge этой ветки запланирован на вечер') },
    @{ term='branch'; phrases=@('Создай новую branch для задачи'; 'branch с исправлением уже опубликована') },
    @{ term='deploy'; phrases=@('Можно deploy на тестовый сервер'; 'deploy приложения начнётся после обеда') },
    @{ term='pull request'; phrases=@('Открой pull request для ревью'; 'pull request я отправил утром') },
    @{ term='Ctrl'; phrases=@('Зажми Ctrl и нажми букву эс'; 'Ctrl используй для быстрого выделения') },
    @{ term='Alt'; phrases=@('Нажми Alt перед выбором пункта'; 'Alt зажми на секунду') },
    @{ term='Shift'; phrases=@('Удерживай Shift при масштабировании'; 'Shift добавь к этому сочетанию') },
    @{ term='Enter'; phrases=@('Нажми Enter, чтобы подтвердить'; 'Enter отправит сообщение в чат') },
    @{ term='Tab'; phrases=@('Перейди к следующему полю клавишей Tab'; 'Tab нажми два раза') },
    @{ term='Photoshop'; phrases=@('Открой Photoshop и загрузи проект'; 'Photoshop сегодня запускается медленно') },
    @{ term='Figma'; phrases=@('Найди макет в Figma'; 'Figma сохранила изменения автоматически') },
    @{ term='Blender'; phrases=@('Добавь камеру в Blender'; 'Blender открой на втором экране') },
    @{ term='Unity'; phrases=@('Собери сцену в Unity'; 'Unity скачала обновление движка') },
    @{ term='GitHub'; phrases=@('Загрузи ветку на GitHub'; 'GitHub прислал уведомление о проверке') },
    @{ term='Discord'; phrases=@('Отправь ссылку в Discord'; 'Discord открой после встречи') },
    @{ term='undo'; phrases=@('Сделай undo последнего действия'; 'undo вернёт предыдущий вариант') },
    @{ term='redo'; phrases=@('Выполни redo после отмены'; 'redo вернул правильный цвет') },
    @{ term='save'; phrases=@('Не забудь save перед закрытием'; 'save проекта уже завершился') },
    @{ term='export'; phrases=@('Сделай export в отдельную папку'; 'export модели займёт пару минут') },
    @{ term='import'; phrases=@('Запусти import текстур'; 'import файла прошёл без ошибок') },
    @{ term='crop'; phrases=@('Выбери crop для обрезки фотографии'; 'crop кадра оставь по центру') },
    @{ term='zoom'; phrases=@('Увеличь zoom для проверки деталей'; 'zoom поставь на сто процентов') },
    @{ term='screenshot'; phrases=@('Сделай screenshot открытого окна'; 'screenshot экрана сохрани на рабочий стол') }
)

$russianPhrases = @(
    'Мама достала из шкатулки серебряную брошь и приколола её к воротнику',
    'Ориентиром для поворота будет высокая водонапорная башня',
    'На праздник дети надели бумажные маски и стали танцевать',
    'На нижний слой краски нанесли прозрачный лак',
    'Поезд из Москвы прибыл в Тулу без опоздания',
    'Раз от раза погода становилась всё теплее',
    'Сайт библиотеки работает даже во время выходных',
    'Файл с фотографиями лежит в папке на рабочем столе',
    'Папка с документами осталась у секретаря',
    'Сервер перезагрузили после обновления программы',
    'Вчерашний разговор неожиданно затянулся до полуночи',
    'Купи свежий хлеб и молоко по дороге домой',
    'Старый мост закрыли на ремонт до конца месяца',
    'Сосед посадил возле дома три молодых яблони',
    'Пожалуйста, поставь чайник и достань большие чашки',
    'За окном весь день шёл мелкий осенний дождь',
    'На полке стоят книги о путешествиях по северу',
    'Новый автобус остановился возле главного входа',
    'Утром бабушка испекла пирог с яблоками и корицей',
    'На станции объявили задержку вечернего поезда',
    'Ветер быстро разогнал тёмные облака над рекой',
    'Положи ключи на деревянную тумбочку у двери',
    'Кошка свернулась клубком на тёплом подоконнике',
    'Рабочий стол нужно протереть перед началом встречи',
    'Завтра мы поедем в гости к старым друзьям',
    'В корзине осталось несколько спелых груш',
    'Участники собрались возле входа ровно к восьми',
    'После прогулки дети выпили горячего какао',
    'В библиотеке открыли небольшую выставку старых карт',
    'Поставь цветы поближе к солнечному окну',
    'На соседней улице построили новую детскую площадку',
    'Повар добавил в суп немного свежего укропа',
    'Летом река здесь становится мелкой и прозрачной',
    'Мастер починил расшатавшуюся ножку кухонного стула',
    'Сегодня на рынке продают сладкую клубнику',
    'Перед прогулкой проверь, закрыты ли окна',
    'Утренний туман медленно рассеялся над полем',
    'В коробке нашли старые письма и семейные фотографии',
    'На полке рядом с книгами лежит деревянная линейка',
    'По выходным мы обычно гуляем в городском парке'
)

$rows = New-Object System.Collections.Generic.List[object]
for ($termIndex = 0; $termIndex -lt $termCases.Count; $termIndex++) {
    $item = $termCases[$termIndex]
    for ($variant = 0; $variant -lt 2; $variant++) {
        $index = $rows.Count
        $voiceIndex = ($termIndex + $variant) % $voices.Count
        $file = 'mix2-{0:D3}-m2-{1}-{2}.wav' -f ($index + 1), $item.term.ToLowerInvariant().Replace(' ', '-'), ($variant + 1)
        $phrase = $item.phrases[$variant]
        $rows.Add([pscustomobject]@{
            id = ('m2-{0}-{1}' -f $item.term.ToLowerInvariant().Replace(' ', '-'), ($variant + 1))
            kind = 'mixed'
            phrase = $phrase
            expected = $phrase
            expected_terms = @($item.term)
            voice = $voices[$voiceIndex].Name
            file = $file
        })
    }
}
for ($index = 0; $index -lt $russianPhrases.Count; $index++) {
    $phrase = $russianPhrases[$index]
    $voiceIndex = ($termCases.Count + $index) % $voices.Count
    $file = 'mix2-{0:D3}-ru-{1:D2}.wav' -f ($rows.Count + 1), ($index + 1)
    $rows.Add([pscustomobject]@{
        id = ('ru-m2-{0:D2}' -f ($index + 1))
        kind = 'russian'
        phrase = $phrase
        expected = $phrase
        expected_terms = @()
        voice = $voices[$voiceIndex].Name
        file = $file
    })
}

$json = ConvertTo-Json -InputObject $rows.ToArray() -Depth 5
$json = [Text.RegularExpressions.Regex]::Replace($json, "`r?`n", "`n")
[IO.File]::WriteAllText($Manifest, $json, (New-Object System.Text.UTF8Encoding -ArgumentList $false))
Write-Host "Манифест записан: $($rows.Count) фраз ($($termCases.Count * 2) смешанных + $($russianPhrases.Count) русских), $Manifest"
if ($ManifestOnly) { return }

$SAFT16kHz16BitMono = 18
$SSFMCreateForWrite = 3
$tmpWav = Join-Path ([IO.Path]::GetTempPath()) ('reever-mix2-{0}.wav' -f [Guid]::NewGuid().ToString('N'))
function Get-Pcm([string]$text, [string]$voiceName) {
    $format = New-Object -ComObject SAPI.SpAudioFormat
    $format.Type = $SAFT16kHz16BitMono
    $stream = New-Object -ComObject SAPI.SpFileStream
    $stream.Format = $format
    $stream.Open($tmpWav, $SSFMCreateForWrite, $false)
    foreach ($candidateVoice in $voices) {
        if ($candidateVoice.Name -eq $voiceName) { $voice.Voice = $candidateVoice.Token; break }
    }
    $voice.Rate = 0
    $voice.AudioOutputStream = $stream
    [void]$voice.Speak($text)
    $stream.Close()
    $bytes = [IO.File]::ReadAllBytes($tmpWav)
    for ($offset = 12; $offset -lt $bytes.Length - 8;) {
        $chunk = [Text.Encoding]::ASCII.GetString($bytes, $offset, 4)
        $size = [Math]::Min([BitConverter]::ToInt32($bytes, $offset + 4), $bytes.Length - $offset - 8)
        if ($chunk -eq 'data') {
            $pcm = New-Object byte[] $size
            [Array]::Copy($bytes, $offset + 8, $pcm, 0, $size)
            return ,$pcm
        }
        $offset += 8 + $size + ($size % 2)
    }
    throw "SAPI WAV без data chunk: $voiceName"
}
function Write-Wav([string]$path, [byte[]]$pcm) {
    $stream = New-Object IO.MemoryStream
    $writer = New-Object IO.BinaryWriter($stream)
    $writer.Write([Text.Encoding]::ASCII.GetBytes('RIFF')); $writer.Write([int](36 + $pcm.Length))
    $writer.Write([Text.Encoding]::ASCII.GetBytes('WAVEfmt ')); $writer.Write([int]16)
    $writer.Write([int16]1); $writer.Write([int16]1); $writer.Write([int]16000); $writer.Write([int]32000)
    $writer.Write([int16]2); $writer.Write([int16]16)
    $writer.Write([Text.Encoding]::ASCII.GetBytes('data')); $writer.Write([int]$pcm.Length); $writer.Write($pcm)
    [IO.File]::WriteAllBytes($path, $stream.ToArray())
    $writer.Dispose()
    $stream.Dispose()
}

for ($index = 0; $index -lt $rows.Count; $index++) {
    $item = $rows[$index]
    $pcm = [byte[]](Get-Pcm $item.phrase $item.voice)
    $audio = New-Object System.Collections.Generic.List[byte]
    $audio.AddRange([byte[]]::new(8000)) # 250 мс цифровой тишины до речи
    $audio.AddRange($pcm)
    $audio.AddRange([byte[]]::new(8000)) # 250 мс цифровой тишины после речи
    Write-Wav (Join-Path $OutDir $item.file) $audio.ToArray()
    if (($index + 1) % 10 -eq 0 -or $index -eq $rows.Count - 1) {
        Write-Host ("[{0}/{1}] {2} ({3})" -f ($index + 1), $rows.Count, $item.id, $item.voice)
    }
}
Remove-Item -LiteralPath $tmpWav -Force -ErrorAction SilentlyContinue
Write-Host "Готово: $($rows.Count) WAV в $OutDir"
