// Reever — голосовой лаунчер. Библиотечная часть: всё приложение, кроме точки входа.
// Вынесено из main.rs, чтобы модули (распознавание, разбор команд) были доступны
// тестам и инструментам в examples/ без запуска Tauri.

pub mod actions;
pub mod anglicism;
pub mod apps;
pub mod appscan;
pub mod asr;
pub mod audio;
pub mod bilingual;
pub mod blackbox;
pub mod calc;
pub mod calcapp;
pub mod commands;
pub mod commands_api;
pub mod config;
pub mod confirm;
pub mod dictation;
pub mod dispatch;
pub mod engine;
pub mod events;
pub mod find;
pub mod grammar;
pub mod input_thread;
pub mod kws;
pub mod langid;
pub mod launcher;
pub mod local_index;
pub mod micwatch;
pub mod models;
#[cfg(windows)]
pub mod ortopt;
pub mod bubble;
pub mod mascot;
pub mod picker;
pub mod procs;
pub mod rawmic;
pub mod recognizer;
pub mod segmenter;
pub mod skin;
pub mod speech;
pub mod spellcheck;
pub mod system;
pub mod textnorm;
pub mod traymenu;
pub mod typing;
pub mod uilang;
pub mod vad;
pub mod voice_adapt;
pub mod wake;
pub mod webchat;
pub mod winutil;

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::Arc;

use actions::SharedState;
use config::{LiveConfig, ReeverConfig};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::WebviewWindowBuilder;
use tauri::{Manager, RunEvent, WebviewWindow, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// Показывать главное окно только после отрисовки рамки; счётчик сбрасывается при пересоздании.
static WINDOW_SHOWN: AtomicBool = AtomicBool::new(false);
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static MAIN_CREATE_PENDING: AtomicBool = AtomicBool::new(false);
static MAIN_WINDOW_GENERATION: AtomicU64 = AtomicU64::new(0);
static INSTANCE_EVENT_HANDLE: AtomicIsize = AtomicIsize::new(0);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

fn launched_hidden() -> bool {
    std::env::args().any(|a| a == "--hidden")
}

fn reveal_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        events::set_main_visible(true);
    }
}

/// Интерфейс готов: рамка нарисована и форма применена. Вызывается при каждой загрузке.
#[tauri::command]
fn window_ready(app: tauri::AppHandle) {
    events::set_state(&app, &events::current_state());
    if SHOW_REQUESTED.load(Ordering::SeqCst) && !WINDOW_SHOWN.swap(true, Ordering::SeqCst) {
        reveal_main(&app);
    }
}

/// Пауза микрофона и обратно (меню трея).
fn toggle_pause(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<Arc<SharedState>>() {
        let now = !state.paused.load(Ordering::Relaxed);
        state.paused.store(now, Ordering::Relaxed);
        events::set_state(app, if now { "paused" } else { "idle" });
        events::ui_log(app, if now { crate::msg!("log.paused") } else { crate::msg!("log.resumed") });
    }
}

fn quit(app: &tauri::AppHandle) {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
    app.exit(0);
}

fn show_main(app: &tauri::AppHandle) {
    SHOW_REQUESTED.store(true, Ordering::SeqCst);
    if app.get_webview_window("main").is_some() {
        if WINDOW_SHOWN.load(Ordering::SeqCst) {
            reveal_main(app);
        }
        return;
    }

    if MAIN_CREATE_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }

    let Some(window_config) = app.config().app.windows.iter().find(|window| window.label == "main").cloned() else {
        MAIN_CREATE_PENDING.store(false, Ordering::SeqCst);
        events::ui_log(app, "⚠ main window config not found");
        return;
    };

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Создание WebView не блокирует обработчик tray/hotkey/IPC.
        // Вид окна (полный скин или лёгкий) страница узнаёт до первого кадра.
        let window = WebviewWindowBuilder::from_config(&app, &window_config)
            .map(|builder| builder.initialization_script(lite_script(&app)))
            .and_then(|builder| builder.build());
        let window = match window {
            Ok(window) => window,
            Err(error) => {
                MAIN_CREATE_PENDING.store(false, Ordering::SeqCst);
                events::ui_log(&app, format!("⚠ main window was not created: {error}"));
                return;
            }
        };

        let generation = MAIN_WINDOW_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        WINDOW_SHOWN.store(false, Ordering::SeqCst);
        attach_main_window(&app, &window);
        MAIN_CREATE_PENDING.store(false, Ordering::SeqCst);

        // Если JS упал до window_ready, оставляем запасной путь показа, как раньше.
        let handle = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if MAIN_WINDOW_GENERATION.load(Ordering::SeqCst) == generation
                && SHOW_REQUESTED.load(Ordering::SeqCst)
                && !WINDOW_SHOWN.swap(true, Ordering::SeqCst)
            {
                reveal_main(&handle);
            }
        });
    });
}

fn attach_main_window(app: &tauri::AppHandle, window: &WebviewWindow) {
    let handle = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move { destroy_main(&handle); });
        }
        WindowEvent::Destroyed => {
            events::set_main_visible(false);
            WINDOW_SHOWN.store(false, Ordering::SeqCst);
            MAIN_CREATE_PENDING.store(false, Ordering::SeqCst);
            MAIN_WINDOW_GENERATION.fetch_add(1, Ordering::SeqCst);
            // Если показ запросили во время уничтожения старого WebView, повторяем его
            // после освобождения метки `main`.
            if SHOW_REQUESTED.load(Ordering::SeqCst) && !EXIT_REQUESTED.load(Ordering::SeqCst) {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move { show_main(&handle); });
            }
        }
        _ => {}
    });
}

fn destroy_main(app: &tauri::AppHandle) {
    SHOW_REQUESTED.store(false, Ordering::SeqCst);
    WINDOW_SHOWN.store(false, Ordering::SeqCst);
    events::set_main_visible(false);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.destroy();
    }
}

/// Флаг лёгкого интерфейса для страницы окна: index.html, picker.html и confirm.html
/// читают его до первого кадра и подключают lite.css.
pub(crate) fn lite_script(app: &tauri::AppHandle) -> String {
    let lite = app.state::<LiveConfig>().get().settings.lite_ui;
    format!("window.__REEVER_LITE__ = {lite};")
}

/// Пересоздать открытое главное окно (сменился вид интерфейса). Закрытое — не трогаем:
/// новый вид появится при следующем открытии.
pub(crate) fn rebuild_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Destroyed увидит SHOW_REQUESTED и создаст окно заново.
        SHOW_REQUESTED.store(true, Ordering::SeqCst);
        let _ = window.destroy();
    }
}

#[tauri::command]
async fn send_to_tray(app: tauri::AppHandle) {
    destroy_main(&app);
}

#[tauri::command]
fn set_compact_mode(compact: bool) {
    events::set_compact(compact);
}

fn signal_existing_instance_or_register() -> bool {
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::{CreateEventW, SetEvent};
    use windows::core::PCWSTR;

    // Демонстрация поиска работает без аудио и не должна будить установленный экземпляр.
    let event_name = if std::env::var_os("REEVER_FIND_DEMO").is_some() {
        "Local\\com.troid.reever.find-demo.show-main"
    } else {
        "Local\\com.troid.reever.show-main"
    };
    let name: Vec<u16> = event_name.encode_utf16().chain(Some(0)).collect();
    let Ok(handle) = (unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) }) else {
        return false;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = SetEvent(handle);
            let _ = CloseHandle(handle);
        }
        true
    } else {
        INSTANCE_EVENT_HANDLE.store(handle.0 as isize, Ordering::SeqCst);
        false
    }
}

fn listen_for_second_instance(app: tauri::AppHandle) {
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};

    let raw = INSTANCE_EVENT_HANDLE.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    std::thread::spawn(move || {
        let event = HANDLE(raw as *mut std::ffi::c_void);
        loop {
            if unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                show_main(&app);
            }
        }
    });
}

/// Защита от DLL-hijacking: убираем текущий каталог и PATH из путей поиска DLL,
/// оставляя только System32 + каталог приложения. Подменённая DLL в папке запуска
/// (например, в Downloads) больше не подхватится. Вызывать первым делом в main().
#[cfg(windows)]
pub fn harden_dll_search() {
    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    };
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    }
}

/// Тестовая сборка (tools/test_build.sh, папка test-build) при запуске закрывает другие
/// экземпляры Reever, например установленный: иначе она лишь подняла бы окно установленного
/// и вышла, а проверялась бы старая версия. Два слушающих Reever сразу — двойные команды.
fn replace_other_instances_if_test_build() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    let Ok(own) = std::env::current_exe() else { return };
    let is_test_build = own
        .parent()
        .and_then(|dir| dir.file_name())
        .is_some_and(|name| name.eq_ignore_ascii_case("test-build"));
    if !is_test_build {
        return;
    }
    let own_pid = std::process::id();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if entry.th32ProcessID != own_pid && name.eq_ignore_ascii_case("reever.exe") {
                if let Ok(process) = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, entry.th32ProcessID) {
                    if TerminateProcess(process, 0).is_ok() {
                        let _ = WaitForSingleObject(process, 3_000);
                    }
                    let _ = CloseHandle(process);
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
}

pub fn run() {
    replace_other_instances_if_test_build();
    // Второй запуск отправляет сигнал первому экземпляру и не запускает второй аудиопоток.
    if signal_existing_instance_or_register() {
        return;
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let Some(state) = app.try_state::<Arc<SharedState>>() else {
                        return;
                    };
                    // Mute-хоткей? — мгновенно глушим/включаем микрофон (panic-mute).
                    let mute = state.mute_hotkey.lock().ok().map(|g| g.clone()).unwrap_or_default();
                    let is_mute = !mute.trim().is_empty()
                        && Shortcut::from_str(&mute).map(|m| &m == shortcut).unwrap_or(false);
                    if is_mute {
                        let now = !state.paused.load(Ordering::Relaxed);
                        state.paused.store(now, Ordering::Relaxed);
                        events::set_state(app, if now { "paused" } else { "idle" });
                        events::ui_log(app, if now { crate::msg!("log.mic.mutedHotkey") } else { crate::msg!("log.mic.onHotkey") });
                    } else {
                        // иначе — push-to-talk
                        show_main(app);
                        state.force_listen.store(true, Ordering::Relaxed);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let cfg = Arc::new(ReeverConfig::resolve(app.handle())?);
            if let Some(dir) = cfg.settings_path.parent() {
                events::init_log_file(dir.join("reever.log"));
            }

            // Все конфиги полностью редактируемы пользователем (release -> %APPDATA%).
            let actions_path = config::user_config_path(app.handle(), "actions.json");
            let grammar_path = config::user_config_path(app.handle(), "grammar.json");
            let shared = Arc::new(SharedState::new(actions_path, grammar_path, &cfg.settings));

            app.manage(LiveConfig::new(cfg.clone()));
            app.manage(shared.clone());
            let index_dir = app.path().app_config_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let index = local_index::IndexManager::new(index_dir.join("index-v1.bin"));
            index.set_enabled(cfg.settings.local_index);
            app.manage(index);

            // --- трей ---
            let locale = uilang::resolve_enabled(
                &cfg.settings.ui_language,
                &cfg.settings.interface_languages(),
            );
            *uilang::CURRENT.lock().unwrap() = locale;
            // Меню по правому клику — системное меню Windows, нарисованное в стиле Reever (traymenu.rs).
            traymenu::init(app.handle());
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Reever")
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => show_main(tray.app_handle()),
                    TrayIconEvent::Click {
                        button: MouseButton::Right,
                        button_state: MouseButtonState::Up,
                        ..
                    } => traymenu::show(),
                    _ => {}
                })
                .build(app)?;

            // Второй запуск поднимает этот экземпляр, не создавая ещё один аудиопоток.
            listen_for_second_instance(app.handle().clone());
            // Автозапуск --hidden оставляет только трей и аудио; обычный запуск создаёт окно.
            if !launched_hidden() {
                show_main(app.handle());
            }
            mascot::spawn(app.handle());

            // --- глобальные хоткеи: push-to-talk + mute ---
            // источник правды для пере-регистрации на лету (set_hotkey / set_mute_hotkey)
            if let Ok(mut g) = shared.ptt_hotkey.lock() { *g = cfg.settings.hotkey.clone(); }
            if let Ok(mut g) = shared.mute_hotkey.lock() { *g = cfg.settings.mute_hotkey.clone(); }
            let gs = app.global_shortcut();
            if cfg.settings.activation_mode != "wake" && !cfg.settings.hotkey.trim().is_empty() {
                if let Err(e) = gs.register(cfg.settings.hotkey.as_str()) {
                    eprintln!("[reever] hotkey register failed ({}): {e}", cfg.settings.hotkey);
                }
            }
            if !cfg.settings.mute_hotkey.trim().is_empty() {
                if let Err(e) = gs.register(cfg.settings.mute_hotkey.as_str()) {
                    eprintln!("[reever] mute hotkey register failed ({}): {e}", cfg.settings.mute_hotkey);
                }
            }

            // --- проверка окна выбора без микрофона: REEVER_FIND_DEMO="запрос" ---
            // Reever не слушает эфир, а когда готов индекс, показывает окно выбора для запроса. Нужно
            // для живой проверки окна рядом с основной установкой Reever.
            if let Ok(query) = std::env::var("REEVER_FIND_DEMO") {
                let handle = app.handle().clone();
                let shared = shared.clone();
                let index = app.state::<local_index::IndexManager>().inner().clone();
                std::thread::spawn(move || {
                    // Ждём первый снимок индекса: при холодном старте обход дисков идёт секунды.
                    let mut snapshot = None;
                    for _ in 0..120 {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        snapshot = index.snapshot();
                        if snapshot.is_some() {
                            break;
                        }
                    }
                    let items = snapshot
                        .map(|snapshot| {
                            find::search(
                                &snapshot,
                                &[query.clone()],
                                find::FindScope::Any,
                                None,
                                &find::Usage::load(&shared.usage_path),
                                &find::windows_recent(),
                            )
                        })
                        .unwrap_or_default();
                    let _ = picker::show(&handle, &shared, &query, &query, find::FindScope::Any, &items);
                });
                return Ok(());
            }

            // --- тихая подстройка под голос и аудио-воркер ---
            let handle = app.handle().clone();
            voice_adapt::start_passive(
                index_dir.join("voice"),
                commands_api::passive_trainer(handle.clone(), shared.clone()),
            );
            std::thread::Builder::new()
                .name("reever-audio".into())
                .spawn(move || recognizer::run_worker(handle, shared))
                .expect("failed to spawn audio worker");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands_api::list_actions,
            commands_api::list_apps,
            commands_api::scan_apps,
            commands_api::scan_app_icons,
            commands_api::add_scanned_apps,
            commands_api::list_sound_packs,
            commands_api::language_models,
            commands_api::get_grammar,
            commands_api::inactive_triggers,
            commands_api::save_grammar,
            commands_api::reset_grammar,
            commands_api::save_action,
            commands_api::delete_action,
            commands_api::add_phrase,
            commands_api::remove_phrase,
            commands_api::start_calibration,
            commands_api::start_voice_adapt,
            commands_api::cancel_voice_adapt,
            commands_api::finish_voice_adapt,
            commands_api::voice_adapt_status,
            commands_api::reset_voice_adapt,
            commands_api::cancel_calibration,
            commands_api::get_settings,
            commands_api::save_settings,
            commands_api::index_status,
            window_ready,
            send_to_tray,
            set_compact_mode,
            events::get_ui_snapshot,
            events::set_ui_tab,
            events::cache_ui_status,
            commands_api::set_paused,
            commands_api::get_paused,
            commands_api::set_autostart,
            commands_api::get_autostart,
            commands_api::set_hotkey,
            commands_api::set_mute_hotkey,
            commands_api::get_wallpaper,
            commands_api::set_ui_language,
            mascot::mascot_drag,
            mascot::mascot_layout,
            mascot::mascot_rest,
            mascot::mascot_orb,
            bubble::bubble_layout,
            commands_api::set_mascot,
            commands_api::set_lite_ui,
            picker::picker_choose,
            picker::picker_close,
            picker::picker_search,
            picker::picker_ready,
            confirm::confirm_ready,
            confirm::confirm_answer,
            skin::set_window_shape,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app, event| {
        match event {
            RunEvent::ExitRequested { api, code: None, .. } => {
                // Окна уничтожаются в трейном режиме, но процесс оставляет живыми трей и аудио.
                api.prevent_exit();
            }
            RunEvent::ExitRequested { .. } => EXIT_REQUESTED.store(true, Ordering::SeqCst),
            _ => {}
        }
    });
}
