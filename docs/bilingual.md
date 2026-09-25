# Английский и смешанная речь — контракт

Правила общей работы — как в `docs/voice-pipeline.md` («Правила для всех»): одно дерево,
правь только свои файлы, `cargo check --all-targets` зелёный всегда, не коммить, вопросы —
`orca orchestration ask`, на «СТОП» от координатора — немедленно прекратить правки.

## Статус (2026-09-24)

Реализовано. Модели по бенчмаркам воркеров: вторая — `sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8`
(английский, WER/CER 0.65/0.26 на трудном корпусе, RTF 0.076, 631 МБ); определитель —
`sherpa-onnx-whisper-base` (92% на фрагментах, 85 мс; бандлятся только int8-файлы, ~154 МБ).
Порог уверенности 0.55, минимальная длина цели для определителя 0.7 с. Инсталлятор 0.1.5 — 668 МБ.

Замер (`examples/eval.rs`, 118 фраз): без шума 118/118, язык цели верен 35/44; английская цель
после русской ключевой фразы (Zira) — язык 16/16, CER 0.026; «ривер minecraft» — 6/6.
Шум −30 дБFS 104/118, −25 дБFS 78/118. Английский, прочитанный русскими голосами RHVoice,
— 7/16 по языку: RHVoice читает латиницу почти по буквам, это артефакт синтеза, а не акцент.

Отступления, найденные замером:
- Русская модель на чисто английском сегменте возвращает пустоту — раньше это считалось
  эхом сигнала и пропускалось; теперь пустой сегмент в режиме команды сначала слушает вторая
  модель (`Engine::second_language_only`).
- Вето текста: определитель говорит «английский», но `text_hint` уверенно видит русские слова →
  русский. В шуме Parakeet выдумывает связные английские фразы поверх русской речи.

## Проблема (пользователь, 2026-09-24)

«ривер поиск ютуб sinksaiko i'm sloth» распознаётся только по-русски (GigaAM — русская
модель): «поиск ютуб синксайко айм слот». Английский в целом воспринимается плохо. Нужно,
чтобы английская цель команды распознавалась по-английски, а при необходимости язык
переключался (например, папку ищут по английскому имени).

## Архитектура

1. **GigaAM (русская) остаётся главной**: wake-слово, ключевые фразы грамматики, русские цели.
2. Распознавание команды — **с таймингом слов** (`Transcriber::transcribe_timed`, sherpa-onnx
   отдаёт `timestamps` токенов).
3. `bilingual::resolve` находит ключевую фразу (`grammar.match_trigger`), вырезает **аудио
   цели** (всё после ключевой фразы; при bare-launch — всё после wake) с запасом ~150 мс и
   распознаёт его **второй моделью** (английская или мультиязычная — выбирает бенчмарк).
4. **Язык цели**: `Intent.lang` (ru/en/auto) → глобальная `payload_language` → для `auto`
   определитель языка (`LanguageId`) и текстовые эвристики; при сомнении — русский.
5. Выход: `Resolved { text, alt, payload_lang }` — команда с целью на выбранном языке и
   запасной вариант на другом. Дальше используются **оба**, где это дёшево:
   - `Folder` — Everything ищет по обоим, открывается лучший по похожести;
   - `Launch` — `Action::score` по обоим кандидатам (фразы действий бывают и латиницей);
   - `Search` — только `text` (один запрос), поэтому выбор языка здесь критичен.
6. Калибровка записывает в действие обе формы (русскую и английскую) — как раньше делал Vosk EN.

## Типы (зафиксированы, менять только через координатора)

- `speech.rs`: `TimedWord { text, start, end }` (секунды), `Timed { text, words }`,
  `Transcriber::transcribe_timed` (дефолт — без слов), `Lang { Ru, En }`,
  `trait LanguageId { fn detect(&mut self, pcm) -> Result<(Lang, f32)> }`.
- `bilingual.rs`: `LangMode { Auto, Ru, En }` (serde lowercase), `Resolved`, `resolve(...)`
  — сигнатура и doc-комментарий в файле.
- `grammar.rs`: `Intent.lang: LangMode` (serde default `auto`).
- `langid.rs`: `SpeechLid::new(model_dir, threads)`, `impl LanguageId`, `text_hint(ru, en)`.
- **Engine** (воркер `engine`): `EngineEvent::Command { text: String, alt: Option<String> }`,
  `EngineEvent::Calibration { text: String, alt: Option<String> }`;
  `Engine::set_bilingual(second: Option<Box<dyn Transcriber + Send>>, lid: Option<Box<dyn LanguageId + Send>>, mode: LangMode)`.
- **route** (воркер `engine`): `route(text, alt: Option<&str>, grammar, actions, threshold)`,
  `Route::Folder(Vec<String>)` — кандидаты запроса по порядку предпочтения.

## Настройки (воркер `ui`, `config.rs`)

| Ключ | Тип | Дефолт | Смысл |
|---|---|---|---|
| `english_enabled` | bool | true | второй язык включён (поле было, снова используется) |
| `second_asr_model` | string | `sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8` (уточнит бенчмарк) | папка второй модели в resources/model |
| `lid_model` | string | `sherpa-onnx-whisper-tiny` (уточнит бенчмарк) | папка определителя языка; пусто — без модели, только эвристики |
| `payload_language` | string | `auto` | `auto` / `ru` / `en` — язык цели по умолчанию |

`ReeverConfig`: `second_model_dir`, `lid_model_dir` — та же логика путей, что у `asr_model_dir`.

## Владение файлами

| Воркер | Файлы |
|---|---|
| `en-asr` | `src/asr.rs`, `scripts/fetch-deps.ps1`, `tauri.conf.json`, `THIRD-PARTY-NOTICES.md`, `.github/workflows/build.yml`, `examples/asr_bench.rs` |
| `langid` | `src/langid.rs`, `examples/lid_bench.rs` |
| `bilingual` | `src/bilingual.rs` |
| `engine` | `src/engine.rs`, `src/dispatch.rs`, `src/commands.rs` |
| `corpus` | `scripts/gen-corpus.ps1`, `testdata/**`, `examples/eval.rs`, корневой `.gitignore` |
| `ui` | `src/config.rs`, `src/commands_api.rs`, `frontend/**`, `resources/settings.json` |
| координатор | `speech.rs`, `grammar.rs`, `lib.rs`, `main.rs`, `recognizer.rs`, `Cargo.toml`, `docs/` |

Модели скачиваются в `src-tauri/resources/model/` (в `.gitignore`). В `fetch-deps.ps1` их
добавляет только `en-asr`; модель определителя языка `langid` для бенчмарка качает вручную
и сообщает имя — в скрипт её добавит координатор или `en-asr` по просьбе координатора.

## Голоса для тестов

Английский SAPI-голос: `Microsoft Zira Desktop` (en-US). Русские: Irina и RHVoice
(Alexandr, Ekaterina, Dmitriy, Vika, Roman). Синтез — только через COM SAPI
(`SAPI.SpVoice` + `SpFileStream`), см. `scripts/gen-corpus.ps1`: .NET System.Speech с RHVoice
пишет лишь 0,1 с. Русский голос, читающий латиницу, даёт реалистичный «английский с акцентом».
