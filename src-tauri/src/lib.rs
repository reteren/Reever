// Reever — голосовой лаунчер. Библиотечная часть: всё приложение, кроме точки входа.
// Вынесено из main.rs, чтобы модули (распознавание, разбор команд) были доступны
// тестам и инструментам в examples/ без запуска окон.

pub mod actions;
pub mod app;
pub mod anglicism;
pub mod apps;
pub mod appscan;
pub mod asr;
pub mod audio;
pub mod autostart;
pub mod bilingual;
pub mod blackbox;
pub mod calc;
pub mod calcapp;
pub mod commands;
pub mod commands_api;
pub mod config;
pub mod confirm;
pub mod dialog_demo;
pub mod dialogs_native;
pub mod dictation;
pub mod dispatch;
pub mod engine;
pub mod events;
pub mod find;
pub mod grammar;
pub mod hotkeys;
pub mod input_thread;
pub mod keys;
pub mod kws;
pub mod langid;
pub mod launcher;
pub mod listen;
pub mod local_index;
pub mod loopback;
pub mod micwatch;
pub mod native;
pub mod mascot_demo;
pub mod mascot_native;
pub mod main_native;
pub mod main_demo;
pub mod models;
#[cfg(windows)]
pub mod ortopt;
pub mod bubble;
pub mod bubble_native;
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
pub mod transcribe;
pub mod traymenu;
pub mod typing;
pub mod uilang;
pub mod update;
pub mod ui_text;
pub mod vad;
pub mod voice_adapt;
pub mod wake;
pub mod webchat;
pub mod winutil;

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;

use actions::SharedState;
pub use app::AppHandle;
use config::{LiveConfig, ReeverConfig};

static INSTANCE_EVENT_HANDLE: AtomicIsize = AtomicIsize::new(0);

fn launched_hidden() -> bool {
    std::env::args().any(|a| a == "--hidden")
}

/// Пауза микрофона и обратно (меню трея).
pub(crate) fn toggle_pause(app: &AppHandle) {
    if let Some(state) = app.try_state::<Arc<SharedState>>() {
        let now = !state.paused.load(Ordering::Relaxed);
        state.paused.store(now, Ordering::Relaxed);
        events::set_state(app, if now { "paused" } else { "idle" });
        events::ui_log(app, if now { crate::msg!("log.paused") } else { crate::msg!("log.resumed") });
    }
}

fn quit(app: &AppHandle) {
    traymenu::remove_icon();
    app.exit(0);
}

/// Открыть главное окно (или поднять открытое).
pub(crate) fn show_main(app: &AppHandle) {
    main_native::show(app);
}

/// Главное окно — в трей (окно уничтожается, остаются значок и распознавание).
pub(crate) fn destroy_main(app: &AppHandle) {
    events::set_main_visible(false);
    main_native::destroy(app);
}

/// Пересоздать открытое главное окно (сменился вид интерфейса). Закрытое — не трогаем:
/// новый вид появится при следующем открытии.
pub(crate) fn rebuild_main(app: &AppHandle) {
    if main_native::is_open() {
        main_native::destroy(app);
        main_native::show(app);
    }
}

fn signal_existing_instance_or_register() -> bool {
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::{CreateEventW, SetEvent};
    use windows::core::PCWSTR;

    // Демонстрация поиска работает без аудио и не должна будить установленный экземпляр.
    let event_name = if std::env::var_os("REEVER_FIND_DEMO").is_some() {
        "Local\\com.troid.reever.find-demo.show-main"
    } else if mascot_demo::active() {
        "Local\\com.troid.reever.mascot-demo.show-main"
    } else if dialog_demo::active() {
        "Local\\com.troid.reever.dialog-demo.show-main"
    } else if main_demo::active() {
        "Local\\com.troid.reever.main-demo.show-main"
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

fn listen_for_second_instance(app: AppHandle) {
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

/// Запущен ли exe из папки test-build (tools/test_build.sh).
fn is_test_build() -> bool {
    std::env::current_exe().ok().is_some_and(|own| {
        own.parent()
            .and_then(|dir| dir.file_name())
            .is_some_and(|name| name.eq_ignore_ascii_case("test-build"))
    })
}

/// Автозапуск с Windows включён по умолчанию: при первом запуске установленной программы он
/// включается один раз, дальше решает пользователь (галочка в настройках). Отметка о том, что
/// умолчание уже применено, — `autostart-default.txt` рядом с настройками. Dev- и тестовая
/// сборка автозапуск не трогают: в Run попал бы путь к временному exe.
fn enable_autostart_by_default(app: &AppHandle) {
    if cfg!(debug_assertions) || is_test_build() {
        return;
    }
    let Ok(dir) = app.path().app_config_dir() else { return };
    let marker = dir.join("autostart-default.txt");
    if marker.exists() {
        return;
    }
    match autostart::set(app, true) {
        Ok(()) => {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&marker, "1");
        }
        Err(e) => eprintln!("[reever] autostart: {e}"),
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

    if !is_test_build() {
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
    let app = AppHandle::new();
    if let Err(e) = setup(&app) {
        eprintln!("[reever] start failed: {e}");
        return;
    }
    // главный поток: задачи из других потоков (меню трея), до выхода
    app.run();
    traymenu::remove_icon();
}

fn setup(app: &AppHandle) -> Result<(), String> {
    let cfg = Arc::new(ReeverConfig::resolve(app).map_err(|e| e.to_string())?);
    if let Some(dir) = cfg.settings_path.parent() {
        events::init_log_file(dir.join("reever.log"));
    }

    // Все конфиги полностью редактируемы пользователем (release -> %APPDATA%).
    let actions_path = config::user_config_path(app, "actions.json");
    let grammar_path = config::user_config_path(app, "grammar.json");
    let shared = Arc::new(SharedState::new(actions_path, grammar_path));

    app.manage(LiveConfig::new(cfg.clone()));
    app.manage(shared.clone());
    app.manage(hotkeys::Hotkeys::start(app.clone())?);
    let index_dir = app.path().app_config_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let index = local_index::IndexManager::new(index_dir.join("index-v1.bin"));
    index.set_enabled(cfg.settings.local_index);
    app.manage(index);
    enable_autostart_by_default(app);

    // --- язык интерфейса и трей ---
    let locale = uilang::resolve_enabled(&cfg.settings.ui_language, &cfg.settings.interface_languages());
    *uilang::CURRENT.lock().unwrap() = locale;
    // Значок и меню по правому клику — свои (traymenu.rs): Shell_NotifyIcon и системное
    // меню Windows, нарисованное в стиле Reever.
    traymenu::init(app);

    // Второй запуск поднимает этот экземпляр, не создавая ещё один аудиопоток.
    listen_for_second_instance(app.clone());
    // Автозапуск --hidden оставляет только трей и аудио; обычный запуск создаёт окно.
    if !launched_hidden() {
        show_main(app);
    }
    // нативное главное окно слушает распознавание с самого старта (main_native::init)
    main_native::init(app);
    mascot::spawn(app);
    // проверка маскота без микрофона и горячих клавиш: REEVER_MASCOT_DEMO=1 (mascot_demo.rs)
    if mascot_demo::active() {
        mascot_demo::start(app);
        return Ok(());
    }
    // проверка нативного главного окна: REEVER_MAIN_DEMO=1 (main_demo.rs)
    if main_demo::active() {
        main_demo::start(app);
        return Ok(());
    }
    // проверка окон выбора и подтверждения: REEVER_DIALOG_DEMO=1 (dialog_demo.rs)
    if dialog_demo::active() {
        dialog_demo::start(app);
        return Ok(());
    }

    // --- глобальные хоткеи: push-to-talk + mute ---
    // источник правды для пере-регистрации на лету (set_hotkey / set_mute_hotkey)
    if let Ok(mut g) = shared.ptt_hotkey.lock() { *g = cfg.settings.hotkey.clone(); }
    if let Ok(mut g) = shared.mute_hotkey.lock() { *g = cfg.settings.mute_hotkey.clone(); }
    let hotkeys = app.state::<hotkeys::Hotkeys>();
    let mut main_hotkey = cfg.settings.hotkey.as_str();
    if let Err(e) = hotkeys.configure(&cfg.settings.activation_mode, main_hotkey, "") {
        let message = format!("горячая клавиша недоступна ({}): {e}", cfg.settings.hotkey);
        eprintln!("[reever] {message}");
        events::ui_log(&app, message);
        main_hotkey = "";
    }
    // Занятый основной хоткей не мешает регистрации mute.
    if let Err(e) = hotkeys.configure(&cfg.settings.activation_mode, main_hotkey, &cfg.settings.mute_hotkey) {
        let message = format!("клавиша mute недоступна ({}): {e}", cfg.settings.mute_hotkey);
        eprintln!("[reever] {message}");
        events::ui_log(&app, message);
    }

    // --- проверка окна выбора без микрофона: REEVER_FIND_DEMO="запрос" ---
    // Reever не слушает эфир, а когда готов индекс, показывает окно выбора для запроса. Нужно
    // для живой проверки окна рядом с основной установкой Reever.
    if let Ok(query) = std::env::var("REEVER_FIND_DEMO") {
        let handle = app.clone();
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

    // --- обновления с GitHub (только установленная release-копия) ---
    update::spawn(app);

    // --- тихая подстройка под голос и аудио-воркер ---
    let handle = app.clone();
    voice_adapt::start_passive(
        index_dir.join("voice"),
        commands_api::passive_trainer(handle.clone(), shared.clone()),
    );
    std::thread::Builder::new()
        .name("reever-audio".into())
        .spawn(move || recognizer::run_worker(handle, shared))
        .expect("failed to spawn audio worker");

    Ok(())
}
