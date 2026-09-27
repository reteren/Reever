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

.PARAMETER Dictation
    Вместо корпуса команд синтезировать длинные сценарии диктовки в
    testdata/corpus/dictation/ и записать testdata/dictation-manifest.json.
#>
[CmdletBinding()]
param([int]$Voices = 2, [switch]$Dictation)

$ErrorActionPreference = 'Stop'

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$CorpusDir = Join-Path $RepoRoot 'testdata/corpus'
$OutDir    = if ($Dictation) { Join-Path $CorpusDir 'dictation' } else { $CorpusDir }
$Manifest  = if ($Dictation) { Join-Path $RepoRoot 'testdata/dictation-manifest.json' } else { Join-Path $RepoRoot 'testdata/manifest.json' }
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

function New-Dictation-Part([string]$text, [string]$voiceName = '') {
    [pscustomobject]@{ text = $text; voice = $voiceName }
}

if ($Dictation) {
    $dictationCases = @(
        [pscustomobject]@{
            id = '01_joined_natural'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши пожалуйста подготовь подробный ответ для участников проекта чтобы каждый знал какие задачи уже завершены и что еще нужно проверить до пятницы')
                (New-Dictation-Part 'сначала соберем замечания от команды затем распределим их по разделам и назначим ответственных за исправление каждой найденной проблемы')
                (New-Dictation-Part 'после проверки я отправлю короткое резюме с датами ссылками и следующими шагами чтобы никому не пришлось искать решения в старых сообщениях')
            )
            pauses_ms = @(1200, 500)
            expected_text = 'Пожалуйста подготовь подробный ответ для участников проекта чтобы каждый знал какие задачи уже завершены и что еще нужно проверить до пятницы сначала соберем замечания от команды затем распределим их по разделам и назначим ответственных за исправление каждой найденной проблемы после проверки я отправлю короткое резюме с датами ссылками и следующими шагами чтобы никому не пришлось искать решения в старых сообщениях'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '02_wake_pause_natural'; start_mode = 'wake_pause'
            parts = @(
                (New-Dictation-Part 'ривер')
                (New-Dictation-Part 'напиши на следующей неделе мы обсудим план поездки заранее забронируем билеты проверим расписание поездов и подготовим список вещей для каждого участника')
                (New-Dictation-Part 'отдельно уточним время сбора адрес гостиницы и порядок связи если кто-то задержится в дороге')
                (New-Dictation-Part 'все важные сведения я внесу в общее письмо и отправлю его команде вечером')
            )
            pauses_ms = @(950, 1800, 600)
            expected_text = 'На следующей неделе мы обсудим план поездки заранее забронируем билеты проверим расписание поездов и подготовим список вещей для каждого участника отдельно уточним время сбора адрес гостиницы и порядок связи если кто-то задержится в дороге все важные сведения я внесу в общее письмо и отправлю его команде вечером'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '03_soft_limit_12s'; start_mode = 'joined_trigger_only'
            parts = @(
                (New-Dictation-Part 'ривер напиши')
                (New-Dictation-Part 'перед началом работы мы внимательно проверим исходные требования уточним порядок согласования распределим подготовительные задачи между участниками и заранее соберем материалы которые понадобятся для первого этапа затем сверим сроки с календарем отметим возможные зависимости и составим общий план проверки чтобы каждая часть результата имела понятного владельца и четкий критерий готовности')
                (New-Dictation-Part 'после этого мы зафиксируем договоренности и поделимся ими со всей командой')
            )
            pauses_ms = @(900, 300)
            expected_text = 'Перед началом работы мы внимательно проверим исходные требования уточним порядок согласования распределим подготовительные задачи между участниками и заранее соберем материалы которые понадобятся для первого этапа затем сверим сроки с календарем отметим возможные зависимости и составим общий план проверки чтобы каждая часть результата имела понятного владельца и четкий критерий готовности после этого мы зафиксируем договоренности и поделимся ими со всей командой'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = 1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '04_pause_3s'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши сегодня я отправлю команде короткое описание результата и список вопросов которые требуют общего решения')
                (New-Dictation-Part 'перед публикацией мы сравним текущую версию с утвержденными требованиями проверим ссылки на документы и убедимся что нужные люди получили уведомление')
                (New-Dictation-Part 'после этого внесем небольшие исправления подготовим чистую сборку и назначим время для совместной проверки всех основных сценариев')
            )
            pauses_ms = @(1200, 3000)
            expected_text = 'Сегодня я отправлю команде короткое описание результата и список вопросов которые требуют общего решения перед публикацией мы сравним текущую версию с утвержденными требованиями проверим ссылки на документы и убедимся что нужные люди получили уведомление после этого внесем небольшие исправления подготовим чистую сборку и назначим время для совместной проверки всех основных сценариев'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 3000; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '05_pause_6s'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши в начале дня мы соберем все текущие замечания и подготовим единый список задач для команды')
                (New-Dictation-Part 'затем проверим что каждый пункт понятен и что для его выполнения хватает времени и необходимых материалов')
                (New-Dictation-Part 'после перерыва обсудим результаты распределим оставшуюся работу и заранее сообщим коллегам об изменениях в расписании')
            )
            pauses_ms = @(600, 6000)
            expected_text = 'В начале дня мы соберем все текущие замечания и подготовим единый список задач для команды затем проверим что каждый пункт понятен и что для его выполнения хватает времени и необходимых материалов после перерыва обсудим результаты распределим оставшуюся работу и заранее сообщим коллегам об изменениях в расписании'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 6000; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '06_stop_standalone'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши подготовь сообщение о готовности новой версии расскажи что основные проверки завершены и приложи короткий список известных ограничений')
                (New-Dictation-Part 'перед отправкой мы еще раз сверим номера сборок проверим установщик и убедимся что инструкция соответствует фактическому поведению программы')
                (New-Dictation-Part 'стоп ввод')
            )
            pauses_ms = @(1300, 1100)
            expected_text = 'Подготовь сообщение о готовности новой версии расскажи что основные проверки завершены и приложи короткий список известных ограничений перед отправкой мы еще раз сверим номера сборок проверим установщик и убедимся что инструкция соответствует фактическому поведению программы'
            expected_end = 'Stopped'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 1800
        }
        [pscustomobject]@{
            id = '07_stop_at_chunk_end'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши пожалуйста обнови план работ укажи какие пункты уже проверены и перечисли оставшиеся вопросы для следующей встречи')
                (New-Dictation-Part 'добавь ссылки на исходные документы и попроси участников прислать замечания сегодня до конца рабочего дня стоп ввод')
            )
            pauses_ms = @(1200)
            expected_text = 'Пожалуйста обнови план работ укажи какие пункты уже проверены и перечисли оставшиеся вопросы для следующей встречи добавь ссылки на исходные документы и попроси участников прислать замечания сегодня до конца рабочего дня'
            expected_end = 'Stopped'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 1800
        }
        [pscustomobject]@{
            id = '08_wake_stop_standalone'; start_mode = 'wake_pause'
            parts = @(
                (New-Dictation-Part 'ривер')
                (New-Dictation-Part 'напиши нам нужно подготовить к обсуждению полный список изменений описать причины каждого решения и собрать отзывы от команды')
                (New-Dictation-Part 'затем проверим насколько новая версия удобна в повседневной работе и какие инструкции потребуется дополнить')
                (New-Dictation-Part 'ривер остановись')
            )
            pauses_ms = @(950, 1200, 1400)
            expected_text = 'Нам нужно подготовить к обсуждению полный список изменений описать причины каждого решения и собрать отзывы от команды затем проверим насколько новая версия удобна в повседневной работе и какие инструкции потребуется дополнить'
            expected_end = 'Stopped'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 1800
        }
        [pscustomobject]@{
            id = '09_wake_stop_at_chunk_end'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши после проверки отправь участникам краткий отчет укажи дату запуска и перечисли документы которые надо сохранить')
                (New-Dictation-Part 'также напомни что новые замечания можно добавить в общий список и обсудить на ближайшей встрече ривер остановись')
            )
            pauses_ms = @(900)
            expected_text = 'После проверки отправь участникам краткий отчет укажи дату запуска и перечисли документы которые надо сохранить также напомни что новые замечания можно добавить в общий список и обсудить на ближайшей встрече'
            expected_end = 'Stopped'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 1800
        }
        [pscustomobject]@{
            id = '10_command_mid_dictation'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши сначала собери основные результаты тестирования затем подготовь понятное сообщение с перечислением найденных проблем и предложенных способов исправления')
                (New-Dictation-Part 'перед тем как отправить текст проверь заголовок и добавь дату следующего обсуждения')
                (New-Dictation-Part 'ривер открой блокнот')
            )
            pauses_ms = @(1200, 1200)
            expected_text = 'Сначала собери основные результаты тестирования затем подготовь понятное сообщение с перечислением найденных проблем и предложенных способов исправления перед тем как отправить текст проверь заголовок и добавь дату следующего обсуждения'
            expected_end = 'Command'; expected_command = 'открой блокнот'; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 1800
        }
        [pscustomobject]@{
            id = '11_spoken_punctuation'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши первый пункт запятая сроки согласуем завтра точка')
                (New-Dictation-Part 'новая строка второй пункт запятая отчет отправим утром точка')
                (New-Dictation-Part 'новая строка третье проверьте источники сравните итоговые цифры и сохраните копию документа точка')
            )
            pauses_ms = @(1200, 900)
            expected_text = "Первый пункт, сроки согласуем завтра.`r`nВторой пункт, отчет отправим утром.`r`nТретье проверьте источники сравните итоговые цифры и сохраните копию документа."
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '12_english_review'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши коллеги мы уже собрали замечания по отчету и подготовили новую редакцию документа с учетом последних предложений команды')
                (New-Dictation-Part 'please review the final schedule and send your comments before the meeting tomorrow period' 'Microsoft Zira Desktop')
                (New-Dictation-Part 'после этого я объединю ответы проверю все даты и отправлю участникам согласованную версию вместе с кратким описанием изменений')
            )
            pauses_ms = @(1400, 1600)
            expected_text = 'Коллеги мы уже собрали замечания по отчету и подготовили новую редакцию документа с учетом последних предложений команды Please review the final schedule and send your comments before the meeting tomorrow. После этого я объединю ответы проверю все даты и отправлю участникам согласованную версию вместе с кратким описанием изменений'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '13_english_followup'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши сначала сохраните исходную таблицу и создайте отдельную копию чтобы изменения можно было проверить без риска потерять предыдущие значения')
                (New-Dictation-Part 'I will send the updated document after the team confirms the delivery date period' 'Microsoft Zira Desktop')
                (New-Dictation-Part 'если срок изменится сообщите об этом заранее и приложите ссылку на новую версию чтобы все участники открывали один и тот же файл')
            )
            pauses_ms = @(900, 2200)
            expected_text = 'Сначала сохраните исходную таблицу и создайте отдельную копию чтобы изменения можно было проверить без риска потерять предыдущие значения I will send the updated document after the team confirms the delivery date. Если срок изменится сообщите об этом заранее и приложите ссылку на новую версию чтобы все участники открывали один и тот же файл'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '14_english_question'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши когда подготовка будет закончена отправьте сообщение ответственному и добавьте туда окончательный список материалов для проверки')
                (New-Dictation-Part 'can we keep the original version and create a separate copy for review question mark' 'Microsoft Zira Desktop')
                (New-Dictation-Part 'так мы сможем сравнить старый и новый варианты и быстро найти место где появились неожиданные изменения')
            )
            pauses_ms = @(1000, 900)
            expected_text = 'Когда подготовка будет закончена отправьте сообщение ответственному и добавьте туда окончательный список материалов для проверки Can we keep the original version and create a separate copy for review? Так мы сможем сравнить старый и новый варианты и быстро найти место где появились неожиданные изменения'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '15_natural_short_and_long_pauses'; start_mode = 'wake_pause'
            parts = @(
                (New-Dictation-Part 'ривер')
                (New-Dictation-Part 'напиши сегодня нужно внимательно проверить список участников и убедиться что все получили приглашение на встречу')
                (New-Dictation-Part 'затем подготовить материалы и разослать каждому краткий порядок обсуждения')
                (New-Dictation-Part 'в конце дня мы проверим ответы и внесем изменения в общий календарь чтобы никому не пришлось искать актуальное время отдельно')
            )
            pauses_ms = @(900, 300, 2500)
            expected_text = 'Сегодня нужно внимательно проверить список участников и убедиться что все получили приглашение на встречу затем подготовить материалы и разослать каждому краткий порядок обсуждения в конце дня мы проверим ответы и внесем изменения в общий календарь чтобы никому не пришлось искать актуальное время отдельно'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '16_multiple_2500ms_pauses'; start_mode = 'joined'
            parts = @(
                (New-Dictation-Part 'ривер напиши перед началом встречи подготовь краткий обзор выполненной работы и перечисли важные изменения которые появились после прошлой проверки')
                (New-Dictation-Part 'отдельно запиши вопросы которые пока остаются открытыми и укажи какие данные нужны чтобы принять решение')
                (New-Dictation-Part 'после обсуждения распределим задачи между участниками договоримся о сроках и сохраним общий план в доступном месте')
            )
            pauses_ms = @(2500, 2500)
            expected_text = 'Перед началом встречи подготовь краткий обзор выполненной работы и перечисли важные изменения которые появились после прошлой проверки отдельно запиши вопросы которые пока остаются открытыми и укажи какие данные нужны чтобы принять решение после обсуждения распределим задачи между участниками договоримся о сроках и сохраним общий план в доступном месте'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '17_three_second_pause_after_wake'; start_mode = 'wake_pause'
            parts = @(
                (New-Dictation-Part 'ривер')
                (New-Dictation-Part 'напиши я сначала сверю все пункты инструкции и подготовлю понятное описание действий которые нужно выполнить перед запуском')
                (New-Dictation-Part 'потом проверю варианты на чистой установке сравню сообщения об ошибках и отмечу шаги которые требуют дополнительного пояснения')
                (New-Dictation-Part 'в завершение отправлю команде итоговый документ и назначу время для повторной проверки')
            )
            pauses_ms = @(950, 3000, 500)
            expected_text = 'Я сначала сверю все пункты инструкции и подготовлю понятное описание действий которые нужно выполнить перед запуском потом проверю варианты на чистой установке сравню сообщения об ошибках и отмечу шаги которые требуют дополнительного пояснения в завершение отправлю команде итоговый документ и назначу время для повторной проверки'
            expected_end = 'Silence'; expected_command = ''; pause_check_ms = 3000; soft_pause_after_part = -1; final_silence_ms = 11500
        }
        [pscustomobject]@{
            id = '18_empty_dictation_timeout'; start_mode = 'joined_trigger_only'
            parts = @((New-Dictation-Part 'ривер напиши'))
            pauses_ms = @()
            expected_text = ''; expected_end = 'Silence'; expected_command = ''; pause_check_ms = 0; soft_pause_after_part = -1; final_silence_ms = 11500
        }
    )

    $rows = New-Object System.Collections.Generic.List[object]
    $caseIndex = 0
    foreach ($case in $dictationCases) {
        $pcm = New-Object System.Collections.Generic.List[byte]
        $partRows = New-Object System.Collections.Generic.List[object]
        $pauseRows = New-Object System.Collections.Generic.List[object]
        $leadMs = 1000
        $elapsedBytes = 32 * $leadMs
        $spokenBytes = 0

        for ($p = 0; $p -lt @($case.parts).Count; $p++) {
            if ($p -gt 0) {
                $pauseMs = [int]$case.pauses_ms[$p - 1]
                $pauseRows.Add([pscustomobject]@{
                    start_ms = [int][Math]::Round($elapsedBytes / 32.0)
                    end_ms = [int][Math]::Round(($elapsedBytes + 32 * $pauseMs) / 32.0)
                    duration_ms = $pauseMs
                    after_part = $p - 1
                })
                $pauseBytes = [byte[]]::new(32 * $pauseMs)
                $pcm.AddRange($pauseBytes)
                $elapsedBytes += $pauseBytes.Length
            }

            $part = $case.parts[$p]
            $partVoice = [string]$part.voice
            if ([string]::IsNullOrWhiteSpace($partVoice)) {
                $partVoice = $pool[$caseIndex % $pool.Count]
            }
            $startMs = [int][Math]::Round($elapsedBytes / 32.0)
            $partPcm = Get-Pcm ([string]$part.text) $partVoice ($caseIndex % 3)
            $pcm.AddRange([byte[]]$partPcm)
            $spokenBytes += $partPcm.Length
            $elapsedBytes += $partPcm.Length
            $partRows.Add([pscustomobject]@{
                index = $p; text = [string]$part.text; voice = $partVoice
                start_ms = $startMs
                end_ms = [int][Math]::Round($elapsedBytes / 32.0)
                duration_ms = [int][Math]::Round($partPcm.Length / 32.0)
            })
        }

        $leading = [byte[]]::new(32 * $leadMs)
        $trailing = [byte[]]::new(32 * [int]$case.final_silence_ms)
        $audio = New-Object System.Collections.Generic.List[byte]
        $audio.AddRange($leading)
        $audio.AddRange($pcm.ToArray())
        $audio.AddRange($trailing)
        $file = '{0:D2}_{1}.wav' -f ($caseIndex + 1), $case.id
        Write-Wav (Join-Path $OutDir $file) $audio.ToArray()
        $audioMs = [int][Math]::Round($audio.Count / 32.0)
        $speechMs = [int][Math]::Round($spokenBytes / 32.0)
        $pauseMsTotal = ($case.pauses_ms | Measure-Object -Sum).Sum
        if ($null -eq $pauseMsTotal) { $pauseMsTotal = 0 }
        $rows.Add([pscustomobject]@{
            file = $file; scenario = $case.id; start_mode = $case.start_mode
            expected_text = $case.expected_text; expected_end = $case.expected_end
            expected_command = $case.expected_command
            pause_check_ms = $case.pause_check_ms
            soft_pause_after_part = $case.soft_pause_after_part
            parts = $partRows.ToArray(); pauses = $pauseRows.ToArray()
            speech_ms = $speechMs; pause_ms = [int]$pauseMsTotal
            audio_ms = $audioMs; final_silence_ms = [int]$case.final_silence_ms
        })
        Write-Host ('{0}: речь {1:N1} с, паузы {2:N1} с, всего {3:N1} с' -f `
            $case.id, ($speechMs / 1000.0), ($pauseMsTotal / 1000.0), ($audioMs / 1000.0))
        $caseIndex++
    }

    Remove-Item -LiteralPath $TmpWav -Force -ErrorAction SilentlyContinue
    $json = ConvertTo-Json -InputObject $rows.ToArray() -Depth 6
    $utf8 = New-Object System.Text.UTF8Encoding -ArgumentList $false
    [IO.File]::WriteAllText($Manifest, $json, $utf8)
    Write-Host "готово: $($rows.Count) сценариев -> $OutDir; manifest -> $Manifest"
    return
}

# Сценарий: части фразы и паузы между ними (мс); expected — ожидаемые команды
# Engine (текст без wake-слова); wake_heard — ждём ли сигнал «слушаю».
$cases = New-Object System.Collections.Generic.List[object]
function Add-Case($scenario, $parts, $pauses, $expected, $wakeHeard, $expectedLang = 'ru', $expectedPrefix = '', $expectedPayload = '', $expectedApp = '', $expectedConfirm = $null, $commandEndParts = 0, $expectedValue = $null) {
    $cases.Add([pscustomobject]@{ scenario = $scenario; parts = $parts; pauses = $pauses;
                                  expected = $expected; expected_lang = $expectedLang;
                                  expected_prefix = $expectedPrefix; expected_payload = $expectedPayload;
                                  expected_app = $expectedApp; expected_confirm = $expectedConfirm;
                                  value = $expectedValue;
                                  command_end_parts = $commandEndParts;
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

# Закрытие и принудительное завершение программ: для проверки маршрута ожидаемая цель
# хранится отдельно, а eval сопоставляет кандидатов только с подставными RunningApp.
$closeApps = @(
    [pscustomobject]@{ spoken = 'хром'; app = 'chrome' },
    [pscustomobject]@{ spoken = 'телеграм'; app = 'telegram' },
    [pscustomobject]@{ spoken = 'дискорд'; app = 'discord' },
    [pscustomobject]@{ spoken = 'блокнот'; app = 'notepad' },
    [pscustomobject]@{ spoken = 'стим'; app = 'steam' }
)
foreach ($app in $closeApps) {
    $command = "закрой $($app.spoken)"
    Add-Case 'app_close' @("ривер $command") @() @($command) $false 'ru' '' '' $app.app
}

foreach ($app in $closeApps | Where-Object { $_.app -in @('chrome', 'telegram', 'discord') }) {
    $command = "заверши $($app.spoken)"
    Add-Case 'app_kill' @("ривер $command") @() @($command) $false 'ru' '' '' $app.app
}

# «Закрой» без цели должно открыть окно дослушивания; английские цели произносит Zira.
Add-Case 'app_pause' @('ривер закрой', 'хром') @(900) @('закрой хром') $true 'ru' '' '' 'chrome'
foreach ($app in @(
    [pscustomobject]@{ spoken = 'chrome'; app = 'chrome' },
    [pscustomobject]@{ spoken = 'discord'; app = 'discord' },
    [pscustomobject]@{ spoken = 'telegram'; app = 'telegram' }
)) {
    $command = "закрой $($app.spoken)"
    Add-Case 'app_en' @((New-Part 'ривер закрой'), (New-Part $app.spoken 'Microsoft Zira Desktop')) `
        @(900) @($command) $true 'en' 'закрой' $app.spoken $app.app
}

# Ответ произносится после паузы; eval включает listen_confirm по событию Command и
# отдельно проверяет результат без обращения к настоящим процессам машины.
foreach ($answer in @(
    [pscustomobject]@{ text = 'да'; voice = ''; expected = $true },
    [pscustomobject]@{ text = 'нет'; voice = ''; expected = $false },
    [pscustomobject]@{ text = 'yes'; voice = 'Microsoft Zira Desktop'; expected = $true },
    [pscustomobject]@{ text = 'no'; voice = 'Microsoft Zira Desktop'; expected = $false }
)) {
    Add-Case 'app_confirm' @((New-Part 'ривер закрой хром'), (New-Part $answer.text $answer.voice)) `
        @(1500) @('закрой хром') $false 'ru' '' '' 'chrome' $answer.expected 1
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

# Арифметика: value — эталон parse_expression; expected — команда Engine до маршрутизации.
$calcCases = @(
    [pscustomobject]@{ spoken = 'ривер посчитай тридцать три умножить на семьдесят восемь'; expected = 'посчитай тридцать три умножить на семьдесят восемь'; value = 2574 },
    [pscustomobject]@{ spoken = 'ривер сколько будет два плюс два'; expected = 'сколько будет два плюс два'; value = 4 },
    [pscustomobject]@{ spoken = 'ривер посчитай сто двадцать пять разделить на пять'; expected = 'посчитай сто двадцать пять разделить на пять'; value = 25 },
    [pscustomobject]@{ spoken = 'ривер посчитай пятнадцать процентов от двухсот'; expected = 'посчитай пятнадцать процентов от двухсот'; value = 30 },
    [pscustomobject]@{ spoken = 'ривер посчитай тысяча минус триста сорок'; expected = 'посчитай тысяча минус триста сорок'; value = 660 },
    [pscustomobject]@{ spoken = 'ривер посчитай семь в квадрате'; expected = 'посчитай семь в квадрате'; value = 49 }
)
foreach ($c in $calcCases) {
    Add-Case 'calc' @($c.spoken) @() @($c.expected) $false 'ru' '' '' '' $null 0 $c.value
}
Add-Case 'calc_pause' @('ривер посчитай', 'двенадцать умножить на двенадцать') `
    @(900) @('посчитай двенадцать умножить на двенадцать') $true 'ru' '' '' '' $null 0 144
Add-Case 'calc_en' @((New-Part 'ривер посчитай'), (New-Part 'thirty three times seventy eight' 'Microsoft Zira Desktop')) `
    @(900) @('посчитай thirty three times seventy eight') $true 'en' '' '' '' $null 0 2574

$rows = New-Object System.Collections.Generic.List[object]
$i = 0
foreach ($case in $cases) {
    for ($v = 0; $v -lt [Math]::Min($Voices, $pool.Count); $v++) {
        $voiceName = $pool[($i + $v) % $pool.Count]
        $file = '{0:D3}_{1}.wav' -f $rows.Count, $case.scenario
        $pcm = New-Object System.Collections.Generic.List[byte]
        $commandEndBytes = 0
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
            if ($case.command_end_parts -gt 0 -and ($p + 1) -eq $case.command_end_parts) {
                $commandEndBytes = $pcm.Count
            }
        }
        Write-Wav (Join-Path $OutDir $file) $pcm.ToArray()
        $spokenParts = @($case.parts | ForEach-Object { if ($_ -is [string]) { $_ } else { $_.text } })
        $rows.Add([pscustomobject]@{ file = $file; scenario = $case.scenario; voice = $voiceName;
                                     text = ($spokenParts -join ' / '); expected = @($case.expected);
                                     expected_lang = $case.expected_lang; expected_prefix = $case.expected_prefix;
                                     expected_payload = $case.expected_payload; expected_app = $case.expected_app;
                                     expected_confirm = $case.expected_confirm;
                                     value = $case.value;
                                     command_end_bytes = $commandEndBytes;
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
