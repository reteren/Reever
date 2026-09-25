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
