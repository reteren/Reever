# Сторонние компоненты

Reever распространяется вместе со сторонними компонентами, перечисленными ниже. Файлы загружаются скриптом `scripts/fetch-deps.ps1`; часть моделей попадает в инсталлятор, языковые пакеты инсталлятор скачивает сам по выбору пользователя.

## sherpa-onnx

- **Компонент**: Rust-крейт `sherpa-onnx` и его нативная библиотека (вместе с ONNX Runtime, MIT), статически линкуются в `reever.exe`
- **Лицензия**: Apache License 2.0
- **Источник**: https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE

## Модели в установщике

### NVIDIA Parakeet TDT 110M (английский)

- **Компонент**: `sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8`
- **Лицензия**: Creative Commons Attribution 4.0 International (CC BY 4.0), согласно карточке модели NVIDIA.
- **ONNX-пакет**: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models

### Streaming Zipformer small-ru (детектор «ривер»)

- **Компонент**: `sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16`
- **Лицензия**: Apache License 2.0 (модели Vosk / alphacep).
- **ONNX-пакет**: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models

### Silero VAD

- **Компонент**: silero_vad.onnx
- **Лицензия**: MIT
- **Авторское право**: Copyright (c) 2020-present Silero Team.
- **Источник**: https://github.com/snakers4/silero-vad/blob/master/LICENSE

### Банк признаков для подстройки под голос

- **Компонент**: `resources/wake/river.bank`, `resources/wake/river-ru-en.bank` — признаки
  детектора «ривер» на фразах FLEURS и синтетическом шуме (`tools/kws/make_bank.py`); самих
  записей в файлах нет.
- **Данные**: Google FLEURS, Creative Commons Attribution 4.0 International (CC BY 4.0).
- **Источник**: https://huggingface.co/datasets/google/fleurs

## Модели, которые скачивает установщик

Эти модели в установщик не входят: установщик скачивает их по галочке в
`%LOCALAPPDATA%\com.troid.reever\models` (`src-tauri/windows/models.nsh`). Parakeet v3 и
SenseVoice — 4-битные архивы Reever из релиза https://github.com/reteren/reever-models;
Whisper base — с релиза sherpa-onnx.

### NVIDIA Parakeet TDT 0.6B v3 (европейские языки)

- **Компонент**: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` (ONNX-конверсия NVIDIA `parakeet-tdt-0.6b-v3`)
- **Лицензия**: Creative Commons Attribution 4.0 International (CC BY 4.0), согласно карточке модели NVIDIA.
- **Источник**: https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3

### SenseVoice Small (китайский, японский, корейский)

- **Компонент**: `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17`
- **Лицензия**: код SenseVoice — MIT (Copyright (c) 2025 FunASR); веса модели — по лицензии модели
  FunAudioLLM («model-license» в карточке модели).
- **Источник**: https://huggingface.co/FunAudioLLM/SenseVoiceSmall, https://github.com/FunAudioLLM/SenseVoice

### Whisper base (определитель языка)

- **Компонент**: `sherpa-onnx-whisper-base` (int8-файлы)
- **Лицензия**: MIT, Copyright (c) 2022 OpenAI.
- **Источник**: https://github.com/openai/whisper/blob/main/LICENSE

## Словарь англицизмов (`resources/anglicisms.tsv`)

Русские фонетические записи английских слов для диктовки сгенерированы
`testdata/lexicon/build_lexicon.py` и `export_tsv.py` из двух источников.

### CMU Pronouncing Dictionary

- **Лицензия**: BSD-2-Clause (текст ниже)
- **Источник**: https://github.com/cmusphinx/cmudict (коммит 74790861f652b15e4ac49015a90074ad62a27690)

```
Copyright (C) 1993-2015 Carnegie Mellon University. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.
   The contents of this file are deemed to be source code.

2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in
   the documentation and/or other materials provided with the
   distribution.

This work was supported in part by funding from the Defense Advanced
Research Projects Agency, the Office of Naval Research and the National
Science Foundation of the United States of America, and by member
companies of the Carnegie Mellon Sphinx Speech Consortium. We acknowledge
the contributions of many volunteers to the expansion and improvement of
this dictionary.

THIS SOFTWARE IS PROVIDED BY CARNEGIE MELLON UNIVERSITY ``AS IS'' AND
ANY EXPRESSED OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL CARNEGIE MELLON UNIVERSITY
NOR ITS EMPLOYEES BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

### English-word-frequencies

- **Компонент**: частоты английских слов (ранжирование кандидатов)
- **Лицензия**: Apache License 2.0
- **Источник**: https://github.com/brekker23/English-word-frequencies (коммит 815665f95ddab30520dff6df260ed3b6244b9638)

## Rust-зависимости

Полный список с версиями — `src-tauri/Cargo.lock`. Ключевые: `tauri` (Apache-2.0 / MIT), `sherpa-onnx`, `cpal`, `rodio`, `rubato`, `strsim`, `serde`, `windows`, `lnk`. Проверить лицензии всего дерева разом:

```bash
cargo install cargo-license
cargo license --manifest-path src-tauri/Cargo.toml
```

## Шрифт

Интерфейс использует системный Segoe UI и ничего не загружает из сети; шрифтов в дистрибутиве нет.
