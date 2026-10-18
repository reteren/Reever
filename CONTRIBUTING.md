# Участие в разработке

## Быстрый старт

```bash
git clone <url> && cd Reever
npm install
powershell -ExecutionPolicy Bypass -File scripts/fetch-deps.ps1
npm run dev
```

Второй шаг обязателен. Модели распознавания **не хранятся в git** — скрипт скачивает из релизов sherpa-onnx то, что вшито в установщик (английская Parakeet 110m, детектор «ривер», Silero VAD). Языковые пакеты (`src-tauri/src/models.rs`) у пользователя качает установщик; для разработки — `-Packs european,asian,lid`. Идемпотентен: повторный запуск ничего не перекачивает, `-Force` переустанавливает. Сама библиотека sherpa-onnx линкуется статически: её prebuilt скачивает `cargo` при первой сборке.

Что нужно на машине: [Rust](https://rustup.rs/), [Node.js](https://nodejs.org/), Windows 10/11. Проект собирается только под Windows — вся оболочка построена на WinAPI.

## Проверка, что всё встало

`scripts/fetch-deps.ps1` в конце сам проверяет наличие моделей и завершается с кодом 1, если чего-то нет. Это важно: без моделей сборка пройдёт зелёной, а приложение не сможет распознавать речь.

## Сборка

```bash
npm run dev      # разработка
npm run build    # релизный инсталлятор -> src-tauri/target/release/bundle/nsis/
```

## О чём стоит знать до первого коммита

**`npm run dev` меняет файлы репозитория.** В dev-режиме конфиги читаются и пишутся в `src-tauri/resources/`, а не в `%APPDATA%` (см. `src-tauri/src/config.rs:196-251`). Калибровка голосом дописывает фразы прямо в `src-tauri/resources/actions.json`. Проверяйте `git diff` перед коммитом — легко случайно отправить свои личные фразы.

**Скрипты PowerShell сохраняются в UTF-8 с BOM.** Windows PowerShell 5.1 читает `.ps1` без BOM как ANSI и ломает кириллицу в сообщениях. Это закреплено в `.gitattributes`; не «чините» кодировку на UTF-8 без BOM.

## Тесты и замер качества

```bash
cd src-tauri
cargo test --lib                                   # юнит-тесты: разбор, матчинг, сегментация, автомат
cargo test --lib -- --ignored                      # то же на реальных моделях (нужен fetch-deps)
powershell -File ../scripts/gen-corpus.ps1         # синтетический корпус голосами SAPI
cargo run --release --example eval                 # весь конвейер на корпусе -> testdata/eval-report.md
cargo run --release --example eval -- --noise-db -30   # то же с фоновым шумом
```

Меняете паузы, порог VAD, wake-слово или матчинг — прогоните `eval` до и после и сравните отчёты. Корпус озвучивается голосами RHVoice; через .NET System.Speech они пишут лишь первые 0,1 с, поэтому скрипт работает через COM SAPI — не «упрощайте» это.

## Структура

| Где | Что |
|---|---|
| `src-tauri/src/recognizer.rs` | аудио-воркер: склейка Engine с Tauri — микрофон, звуки, события UI |
| `src-tauri/src/engine.rs` | автомат прослушивания (wake → команда / подтверждение / калибровка), без Tauri |
| `src-tauri/src/{asr,vad,segmenter,speech}.rs` | модели sherpa-onnx и набор моделей под языки (`AsrSet`), Silero VAD, нарезка на фразы, общие трейты |
| `src-tauri/src/models.rs`, `src-tauri/windows/` | каталог языковых пакетов; страница установщика, которая их скачивает |
| `src-tauri/src/{textnorm,commands,dispatch}.rs` | нормализация текста и wake, грамматика команд, диспетчер |
| `src-tauri/src/config.rs` | настройки, алиасы, резолв путей dev↔release |
| `src-tauri/src/commands_api.rs` | команды для интерфейса |
| `src-tauri/src/{find,local_index}.rs` | «найди …»: запросы и ранжирование, свой индекс дисков |
| `src-tauri/src/{launcher,apps,system,winutil}.rs` | запуск, перечисление приложений, системные команды, окна |
| `frontend/` | интерфейс без сборки: `index.html`, `picker.html`, `skin.css`, скин в `skin/` |

Контракт голосового конвейера — `docs/voice-pipeline.md`. Подробная архитектурная документация живёт в отдельном репозитории `Reever-memory`: диаграмма потока данных, карточка на каждую подсистему, зафиксированные решения и список известных проблем.

## Стиль

- Комментарии и сообщения — по-русски, как в существующем коде.
- Комментарий объясняет **почему**, а не **что**. В проекте это соблюдается: все обходы крайних случаев Windows (`\\?\` в путях к моделям, drive-relative `PathBuf::join`, задвоенный корень после `expand_env`, `AttachThreadInput`) подписаны причиной прямо в коде. Не теряйте эту привычку.
- `cargo fmt` перед коммитом. Конфига `rustfmt.toml` нет — берутся дефолты.

## Известные проблемы

Список открытых расхождений — в `Reever-memory/MEMORY/BUGS.md`. Прежде чем чинить что-то «странное», загляните туда: возможно, это уже описано вместе с причиной.
