# Закрытие и завершение программ голосом — контракт

Правила общей работы — как в `docs/voice-pipeline.md` («Правила для всех»): одно дерево,
правь только свои файлы, `cargo check --all-targets` зелёный всегда, не коммить, вопросы —
`orca orchestration ask`, на «СТОП» от координатора — немедленно прекратить правки.

## Статус (2026-09-25)

Реализовано, инсталлятор 0.1.6. Замер (`examples/eval.rs`, 150 фраз): без шума 150/150,
подтверждение да/нет/yes/no 8/8; шум −30 дБFS 131/150. Живые программы на машине
пользователя сопоставляются верно (Steam → `steam.exe`, Discord, Spotify через действие, Opera GX).

Отступления, найденные замером и живой проверкой:
- Английское «yes» русская модель слышит как «яс» или пустоту — ответ на подтверждение
  дополнительно слушает вторая модель (`Engine::second_says_yes`; «да» от любой модели).
- Окно Steam принадлежит `steamwebhelper.exe`; программой считается корневой предок из той же
  папки установки (`steam.exe`) — иначе «заверши стим» убивал бы только помощника.

## Требования пользователя (2026-09-25)

- **«ривер закрой X»** — закрыть открытую программу X (как крестик: программа может спросить
  про сохранение).
- **«ривер заверши X»** — завершить X принудительно, как «Снять задачу» в диспетчере.
- **Галочка в настройках** «Подтверждать закрытие программ» (по умолчанию выключена): после
  команды звучит сигнал ожидания, нужно ответить «да/нет» или «yes/no»; любой другой ответ или
  тишина — отмена.
- Ключевые фразы «закрой»/«заверши» — часть редактируемой грамматики (docs/grammar.md).

## Что уже сделал координатор (фаза 0)

- `grammar.rs`: `IntentKind::Close`, `IntentKind::Kill`; в `defaults()` намерения `close`
  («закрой») и `kill` («заверши»).
- `dispatch.rs`: `Route::Close(Vec<String>)`, `Route::Kill(Vec<String>)` (кандидаты названия:
  цель `text`, затем `alt` — русский и английский варианты); `route()` уже их выдаёт.
  `DispatchResult::Confirm(PendingAction)`, `PendingAction { System(op), Close(RunningApp),
  Kill(RunningApp) }`, `execute_pending(app, &pending)`; в `dispatch()` для Close/Kill —
  заглушка.
- `procs.rs`: типы `RunningApp`, `AppMatch` и сигнатуры `running_apps`, `match_app`,
  `close_app`, `kill_app`, `is_protected` — заглушки.
- `config.rs`: `Settings.confirm_close: bool` (serde default false, `restore!`).
- `textnorm::is_yes`: английские и транслитерированные «yes/yeah/ok/йес/ес/окей».
- glue (`recognizer.rs`): подтверждение любого `PendingAction`, сигнал ожидания — `confirm.wav`
  пака или `start.wav`, «да» → `execute_pending`.
- `Cargo.toml`: фичи `windows` — `Win32_System_Diagnostics_ToolHelp`,
  `Win32_System_ProcessStatus`, `Win32_Storage_FileSystem`, `Win32_Graphics_Dwm` (плюс уже
  были `WindowsAndMessaging`, `Threading`, `Shell`, `Foundation`).

## Поведение

1. `route` → `Route::Close(cands)` / `Route::Kill(cands)`.
2. `dispatch`: `procs::running_apps()` → `procs::match_app(cands, apps, actions, порог)`;
   не нашлось → лог «∅ не нашёл открытую программу «X»», `Done(false)`. Нашлось:
   - защищённая (`is_protected`) и Kill → отказ в лог, `Done(false)`;
   - `settings.confirm_close` → `DispatchResult::Confirm(PendingAction::Close/Kill(app))`;
   - иначе сразу `execute_pending`.
3. Сопоставление названия (`match_app`, чистая функция): кандидаты сравниваются с
   `exe_name`, `description`, заголовками окон (заголовок — длинная строка: «YouTube — Google
   Chrome», совпадение по словам/подстроке) и с действиями реестра, чей target (exe или .lnk →
   exe) совпадает с `exe_path` программы — тогда работают фразы и калибровка действия. Формы
   сравнения — `textnorm::comparison_forms` (транслитерация: «хром» = «chrome»). Порог —
   `match_threshold` из настроек. При равенстве предпочтение: действие > exe/описание > заголовок.
4. `close_app`: `WM_CLOSE` (`PostMessageW`) каждому видимому окну программы.
   `kill_app`: все процессы с тем же `exe_path` + их дочерние (Toolhelp snapshot),
   `TerminateProcess`; перед этим `is_protected` → ошибка.
5. UWP-окна (хост `ApplicationFrameHost.exe`): реальная программа — процесс дочернего окна
   `Windows.UI.Core.CoreWindow`; `running_apps` должен отдавать её, а не хост.

## Владение файлами

| Воркер | Файлы |
|---|---|
| `procs` | `src/procs.rs`, `examples/procs_probe.rs` |
| `dispatch` | `src/dispatch.rs`, `src/grammar.rs` (миграция), `src/commands.rs` |
| `ui` | `src/config.rs`, `src/commands_api.rs`, `frontend/**`, `resources/settings.json` |
| `corpus` | `scripts/gen-corpus.ps1`, `testdata/**`, `examples/eval.rs` |
| координатор | `speech.rs`, `lib.rs`, `main.rs`, `recognizer.rs`, `engine.rs`, `bilingual.rs`, `textnorm.rs`, `Cargo.toml`, `docs/` |
