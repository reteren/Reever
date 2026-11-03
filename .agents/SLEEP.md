# Sleep checkpoint
Статус: DONE (2026-11-03 ~14:30): этап 5 закончен — 9bfb2cb, 5250bcc (установщик 0.7.3), 892a92e, 5fefc2a
Сохранено: 2026-11-03 02:57 (локальное время машины)
Проект / cwd: C:\Reever (cargo — из C:\Reever\src-tauri)
Ветка / HEAD: main / 392e9a7 «Own tray icon via Shell_NotifyIcon…»; всё ниже — НЕ закоммичено (≈380 изменённых путей, частично staged через git mv)

## Задача и ограничения
Этап 5 docs/native-ui-roadmap.md: убрать Tauri и веб-интерфейс, оставить только нативные окна.
Пользователь ответил «продолжай» на вопрос «делаем нативные окна единственными и убираем веб?» — принято как «да».
Правила: коммит каждого законченного изменения, сообщения на английском, без атрибуции ИИ; push делает пользователь (AGENTS.md).
Установщику — следующая уникальная версия (последняя выпущенная 0.7.2 → следующая 0.7.3).

## Сделано и сохранено
Закоммичено: a6a84e9 (рамка фокуса веба), 9b9b817 (0.7.2), 0d79fd3 (арабская зеркальная раскладка), 392e9a7 (свой значок трея).
На диске, не закоммичено (компилируется, `cargo test --lib` 355 ok, debug-сборка работает):
- src-tauri/src/app.rs — свой каркас вместо Tauri (AppHandle: state/manage/emit/listen_any/unlisten/path/asset_resolver/run_on_main_thread/exit/run, мониторы, PhysicalPosition/Size, вшитые ресурсы).
- src-tauri/build.rs — таблица assets (fonts, mascot, skin) + embed-resource: windows/reever.rc, windows/reever.exe.manifest (новые файлы).
- Cargo.toml: tauri и tauri-build удалены, добавлен embed-resource = "3".
- Ресурсы перенесены git mv: frontend/{fonts,mascot,skin/*.webp,i18n/locales} → src-tauri/assets/{fonts,mascot,skin,locales}; пути в native/text.rs, ui_text.rs, examples/native_demo.rs, tools/installer_art.py обновлены.
- lib.rs переписан (setup + app.run(), без веб-окна, без invoke_handler); skin.rs оставлен только с apply_region.
- Убраны веб-варианты: picker.rs, confirm.rs, bubble.rs, mascot.rs (только нативные окна); events.rs; настройки native_mascot/native_dialogs/native_main удалены из config.rs, settings_tab.rs, main_native, dialog_demo, main_demo, mascot_demo; Act::OpenWeb и draw_not_ported удалены.
- commands_api: #[tauri::command] сняты, async-команды стали синхронными (app::blocking).
- dialogs_native/picker.rs: исправлено падение (индекс за пределами items после выбора/закрытия).

## Точка остановки
Делал установщик без Tauri-бандлера. План:
1. Взять отрендеренный Tauri скрипт src-tauri/target/release/nsis/x64/installer.nsi (+ utils.nsh, FileAssociation.nsh, языковые *.nsh) → положить в src-tauri/windows/ (installer.nsi заменить, lang/*.nsh дополнить); абсолютные пути C:\Reever\src-tauri\ → ${SRC}; VERSION/VERSIONWITHBUILD/OUTFILE/MAINBINARYSRCPATH/ESTIMATEDSIZE через /D.
2. Удалить Section WebView2, defines WEBVIEW2*, !addplugindir.
3. Заменить nsis_tauri_utils: StrReplace → ${WordReplace}; FindProcess/KillProcess → nsExec tasklist/taskkill с фильтром USERNAME; SemverCompare → ${VersionCompare}; RunAsUser → Exec (установка currentUser).
4. tools/build_installer.py: cargo build --release, найти makensis (PATH, Program Files (x86)\NSIS, %LOCALAPPDATA%\tauri\NSIS), собрать в target\release\bundle\nsis\Reever_<v>_x64-setup.exe.
Ещё не сделано: удалить frontend/ (html/js/css, skin/*.js, i18n.js, logo.png), tauri.conf.json, capabilities/, gen/, .taurignore, package.json/package-lock.json; обновить .github/workflows/build.yml, tools/test_build.sh, scripts/fetch-deps.ps1, README/CONTRIBUTING/docs со ссылками на frontend/ и npm run build.

## Проверки
- `cargo check` / `cargo build` (debug) — ок; `cargo test --lib` — 355 passed.
- `cargo check --all-targets`: ошибки в examples/lang_probe.rs и examples/eval_find.rs — были и до этой работы.
- REEVER_MAIN_DEMO (23 снимка), REEVER_DIALOG_DEMO, REEVER_MASCOT_DEMO — работают без Tauri.
- Release-сборка и установщик без Tauri — не проверялись.

## Продолжить
1. `cd C:\Reever\src-tauri && cargo build` — убедиться, что состояние собирается (ожидается: ок).
2. Закоммитить замену Tauri одним коммитом («Replace Tauri with own app shell; native windows only»), после проверки staged diff на секреты.
3. Сделать установщик по плану из «Точки остановки», поднять версию до 0.7.3, собрать, закоммитить.
4. Удалить frontend/ и конфиги Tauri/npm, обновить CI, скрипты и документы; отметить этап 5 в roadmap.

## Команда для завтра
«Продолжи задачу по C:\Reever\.agents\SLEEP.md»

## Итог возобновления (2026-11-03)
Все 4 пункта «Продолжить» сделаны: замена Tauri закоммичена; установщик без tauri-bundler
(windows/installer.nsi + tools/build_installer.py), собран Reever_0.7.3_x64-setup.exe (122 МБ);
frontend/ и конфиги Tauri/npm удалены, CI/доки/скрипты обновлены, этап 5 отмечен в roadmap.
После сборки 0.7.3 исправлены строки установщика (шведский LANG id, немецкие older/unknown) —
попадут в 0.7.4. Не проверено вживую: установка 0.7.3 поверх 0.7.2, деинсталлятор, первый запуск.
