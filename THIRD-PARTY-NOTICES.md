# Сторонние компоненты

Reever распространяется вместе со сторонними компонентами, перечисленными ниже. Файлы загружаются скриптом `scripts/fetch-deps.ps1` и попадают в инсталлятор, поэтому их лицензии распространяются на дистрибутив.

## Vosk API

- **Компонент**: `libvosk.dll`, `libvosk.lib`, `vosk_api.h`
- **Лицензия**: Apache License 2.0 — указана в заголовке `vendor/vosk/vosk_api.h`
- **Источник**: https://github.com/alphacep/vosk-api (релиз v0.3.45)

## Модели распознавания Vosk

- **Компоненты**: `vosk-model-small-ru-0.22`, `vosk-model-small-en-us-0.15`
- **Лицензия**: Apache License 2.0 — согласно таблице моделей на https://alphacephei.com/vosk/models
- **Источник**: https://alphacephei.com/vosk/models

## Рантайм MinGW-w64

`libvosk.dll` собран MinGW, поэтому вместе с ним поставляются три библиотеки рантайма:

- `libstdc++-6.dll`, `libgcc_s_seh-1.dll` — GCC Runtime Library Exception поверх GPLv3. Исключение прямо разрешает распространение в составе несвободного ПО при условии сборки штатным GCC.
- `libwinpthread-1.dll` — проект MinGW-w64, лицензии в стиле BSD/ZPL.

> Эти три файла входят в архив `vosk-win64-0.3.45.zip` и поставляются в том виде, в каком их выпустил upstream. Точные тексты лицензий — в исходниках MinGW-w64 и GCC. **Перед коммерческим распространением стоит подтвердить формулировки с юристом**: здесь зафиксировано то, что следует из общедоступных лицензий компонентов, а не юридическое заключение.

## Rust-зависимости

Полный список с версиями — `src-tauri/Cargo.lock`. Ключевые: `tauri` (Apache-2.0 / MIT), `cpal`, `rodio`, `rubato`, `strsim`, `serde`, `windows`, `lnk`. Проверить лицензии всего дерева разом:

```bash
cargo install cargo-license
cargo license --manifest-path src-tauri/Cargo.toml
```

## Шрифт Quicksand

Подключается интерфейсом с Google Fonts по сети (`frontend/index.html:7-9`), в дистрибутив не входит. Лицензия — SIL Open Font License 1.1. Если шрифт будет инлайниться локально (см. планы в `Reever-memory/ROADMAP.md`), его файл и текст OFL нужно будет добавить в репозиторий.

## Everything

Reever общается с [Everything](https://www.voidtools.com/) по HTTP, но **не распространяет** его. Утилита устанавливается пользователем отдельно.

Файл `src-tauri/resources/es.exe` (CLI-клиент Everything) лежит в ресурсах, но кодом не вызывается — весь обмен идёт по HTTP. Его назначение не выяснено; если он не нужен, его следует удалить, поскольку он попадает в дистрибутив и подпадает под лицензию voidtools.
