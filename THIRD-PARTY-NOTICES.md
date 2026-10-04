# Сторонние компоненты

Reever распространяется вместе со сторонними компонентами, перечисленными ниже. Файлы загружаются скриптом `scripts/fetch-deps.ps1` и попадают в инсталлятор, поэтому их лицензии распространяются на дистрибутив.

## sherpa-onnx

- **Компонент**: Rust-крейт `sherpa-onnx` и его нативная библиотека (вместе с ONNX Runtime, MIT), статически линкуются в `reever.exe`
- **Лицензия**: Apache License 2.0
- **Источник**: https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE

## GigaAM v3

- **Компонент**: sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16
- **Лицензия**: MIT (файл LICENSE в архиве модели)
- **Авторское право**: Copyright (c) 2024 GigaChat Team; архив LICENSE входит в папку модели.
- **Источник**: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models

## Silero VAD

- **Компонент**: silero_vad.onnx
- **Лицензия**: MIT
- **Авторское право**: Copyright (c) 2020-present Silero Team.
- **Источник**: https://github.com/snakers4/silero-vad/blob/master/LICENSE

## NVIDIA Parakeet TDT 0.6B v2

- **Компонент**: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8` (ONNX-конверсия NVIDIA `parakeet-tdt-0.6b-v2`)
- **Лицензия**: Creative Commons Attribution 4.0 International (CC BY 4.0), согласно карточке модели NVIDIA.
- **Источник**: https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2
- **ONNX-пакет**: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models

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

## Everything

Reever общается с [Everything](https://www.voidtools.com/) по HTTP, но **не распространяет** его. Утилита устанавливается пользователем отдельно.

Файл `src-tauri/resources/es.exe` (CLI-клиент Everything) лежит в ресурсах, но кодом не вызывается — весь обмен идёт по HTTP. Его назначение не выяснено; если он не нужен, его следует удалить, поскольку он попадает в дистрибутив и подпадает под лицензию voidtools.
