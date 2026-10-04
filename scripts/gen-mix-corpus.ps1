<#
.SYNOPSIS
    Создаёт SAPI-корпус для пословного смешивания русского и английского ASR.
.DESCRIPTION
    Запуск из корня Reever: .\scripts\gen-mix-corpus.ps1
    WAV идут в игнорируемый testdata/corpus/mix, манифест — в testdata/mix-manifest.json.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$OutDir = Join-Path $RepoRoot 'testdata/corpus/mix'
$Manifest = Join-Path $RepoRoot 'testdata/mix-manifest.json'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Get-ChildItem -LiteralPath $OutDir -Filter 'mix-*.wav' -File | Remove-Item -Force

$phrases = @(
    @{ id='mixed_hardness'; kind='mixed'; expected_word='hardness'; phrase='Настрой hardness поверхности на высокий уровень' },
    @{ id='mixed_roughness'; kind='mixed'; expected_word='roughness'; phrase='Добавь roughness к материалу' },
    @{ id='mixed_opacity'; kind='mixed'; expected_word='opacity'; phrase='Уменьши opacity этого слоя' },
    @{ id='mixed_layer'; kind='mixed'; expected_word='layer'; phrase='Выбери layer с маской' },
    @{ id='mixed_brush'; kind='mixed'; expected_word='brush'; phrase='Возьми brush помягче' },
    @{ id='mixed_render'; kind='mixed'; expected_word='render'; phrase='Запусти render этой сцены' },
    @{ id='mixed_commit'; kind='mixed'; expected_word='commit'; phrase='Сделай commit после проверки' },
    @{ id='mixed_merge'; kind='mixed'; expected_word='merge'; phrase='Перед merge проверь изменения' },
    @{ id='mixed_deploy'; kind='mixed'; expected_word='deploy'; phrase='Можно deploy на тестовый сервер' },
    @{ id='mixed_branch'; kind='mixed'; expected_word='branch'; phrase='Создай новую branch для задачи' },
    @{ id='mixed_shader'; kind='mixed'; expected_word='shader'; phrase='Проверь shader для этой модели' },
    @{ id='mixed_texture'; kind='mixed'; expected_word='texture'; phrase='Замени texture на более чёткую' },
    @{ id='mixed_material'; kind='mixed'; expected_word='material'; phrase='Выбери material для волос' },
    @{ id='mixed_mask'; kind='mixed'; expected_word='mask'; phrase='Добавь mask на фотографию' },
    @{ id='mixed_canvas'; kind='mixed'; expected_word='canvas'; phrase='Увеличь размер canvas' },
    @{ id='mixed_preview'; kind='mixed'; expected_word='preview'; phrase='Покажи preview перед отправкой' },
    @{ id='mixed_package'; kind='mixed'; expected_word='package'; phrase='Собери package для установки' },
    @{ id='mixed_pipeline'; kind='mixed'; expected_word='pipeline'; phrase='Проверь pipeline и сохрани проект' },
    @{ id='ru_pizdecz'; kind='russian_slang'; expected_word='пиздец'; phrase='Это просто пиздец какой-то' },
    @{ id='ru_kapets'; kind='russian_slang'; expected_word='капец'; phrase='Ну это полный капец' },
    @{ id='ru_cringe'; kind='russian_slang'; expected_word='кринж'; phrase='Вчера был лютый кринж' },
    @{ id='ru_zashkvar'; kind='russian_slang'; expected_word='зашквар'; phrase='Такой зашквар нельзя выкладывать' },
    @{ id='ru_imba'; kind='russian_slang'; expected_word='имба'; phrase='Да ладно, это настоящая имба' },
    @{ id='ru_chill'; kind='russian_slang'; expected_word='чилить'; phrase='После работы хочется чилить' },
    @{ id='ru_ugar'; kind='russian_slang'; expected_word='угар'; phrase='Вчера весь вечер был угар' },
    @{ id='ru_blin'; kind='russian_slang'; expected_word='блин'; phrase='Блин, опять всё сломалось' },
    @{ id='ru_zhest'; kind='russian_slang'; expected_word='жесть'; phrase='Вот это жесть конечно' },
    @{ id='ru_vibe'; kind='russian_slang'; expected_word='вайб'; phrase='У этого места особенный вайб' }
)

$voice = New-Object -ComObject SAPI.SpVoice
$voiceNames = @('Microsoft Irina Desktop', 'Alexandr (Russian)')
$tokens = @{}
foreach ($wanted in $voiceNames) {
    foreach ($token in $voice.GetVoices()) {
        if ($token.GetDescription() -like "$wanted*") { $tokens[$wanted] = $token; break }
    }
    if (-not $tokens.ContainsKey($wanted)) { throw "Не найден голос SAPI: $wanted" }
}

$SAFT16kHz16BitMono = 18
$SSFMCreateForWrite = 3
$TmpWav = Join-Path ([IO.Path]::GetTempPath()) 'reever-mix-part.wav'
function Get-Pcm([string]$text, [string]$voiceName) {
    $format = New-Object -ComObject SAPI.SpAudioFormat
    $format.Type = $SAFT16kHz16BitMono
    $stream = New-Object -ComObject SAPI.SpFileStream
    $stream.Format = $format
    $stream.Open($TmpWav, $SSFMCreateForWrite, $false)
    $voice.Voice = $tokens[$voiceName]
    $voice.Rate = 0
    $voice.AudioOutputStream = $stream
    [void]$voice.Speak($text)
    $stream.Close()
    $bytes = [IO.File]::ReadAllBytes($TmpWav)
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
}

$rows = New-Object System.Collections.Generic.List[object]
for ($index = 0; $index -lt $phrases.Count; $index++) {
    $item = $phrases[$index]
    $voiceName = $voiceNames[$index % $voiceNames.Count]
    $pcm = [byte[]](Get-Pcm $item.phrase $voiceName)
    $audio = New-Object System.Collections.Generic.List[byte]
    $audio.AddRange([byte[]]::new(32000)) # 1 с цифровой тишины до фразы
    $audio.AddRange($pcm)
    $audio.AddRange([byte[]]::new(96000)) # 3 с после фразы
    $file = 'mix-{0:D2}-{1}.wav' -f ($index + 1), $item.id
    Write-Wav (Join-Path $OutDir $file) $audio.ToArray()
    $rows.Add([pscustomobject]@{
        id = $item.id; kind = $item.kind; expected_word = $item.expected_word
        phrase = $item.phrase; voice = $voiceName; file = $file
    })
    Write-Host ("{0}: {1} ({2})" -f $item.id, $item.phrase, $voiceName)
}
Remove-Item -LiteralPath $TmpWav -Force -ErrorAction SilentlyContinue
$json = ConvertTo-Json -InputObject $rows.ToArray() -Depth 5
[IO.File]::WriteAllText($Manifest, $json, (New-Object System.Text.UTF8Encoding -ArgumentList $false))
Write-Host "Готово: $($rows.Count) фраз; манифест $Manifest"
