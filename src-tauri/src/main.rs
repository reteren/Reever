// Reever — голосовой интерфейс для утилиты Everything.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod apps;
mod audio;
mod commands;
mod commands_api;
mod config;
mod everything;
mod launcher;
mod recognizer;
mod system;
mod winutil;

use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use actions::SharedState;
use config::ReeverConfig;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Защита от DLL-hijacking: убираем текущий каталог и PATH из путей поиска DLL,
/// оставляя только System32 + каталог приложения. Подменённая DLL в папке запуска
/// (например, в Downloads) больше не подхватится. Вызывать первым делом в main().
#[cfg(windows)]
fn harden_dll_search() {
    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    };
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    }
}

fn main() {
    #[cfg(windows)]
    harden_dll_search();

    tauri::Builder::default()
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
                        let _ = app.emit("reever://state", if now { "paused" } else { "idle" });
                        let _ = app.emit("reever://log", if now { "🔇 микрофон заглушён (хоткей)" } else { "🔊 микрофон включён (хоткей)" });
                    } else {
                        // иначе — push-to-talk
                        state.force_listen.store(true, Ordering::Relaxed);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let cfg = Arc::new(ReeverConfig::resolve(app.handle())?);

            // Все конфиги полностью редактируемы пользователем (release -> %APPDATA%).
            let actions_path = config::user_config_path(app.handle(), "actions.json");
            let shared = Arc::new(SharedState::new(actions_path));

            app.manage(cfg.clone());
            app.manage(shared.clone());

            // --- трей ---
            let show = MenuItemBuilder::with_id("show", "Показать настройки").build(app)?;
            let pause = MenuItemBuilder::with_id("pause", "Пауза / Продолжить микрофон").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Выход").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &pause, &quit]).build()?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Reever")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main(app),
                    "pause" => {
                        if let Some(state) = app.try_state::<Arc<SharedState>>() {
                            let now = !state.paused.load(Ordering::Relaxed);
                            state.paused.store(now, Ordering::Relaxed);
                            let _ = app.emit("reever://state", if now { "paused" } else { "idle" });
                            let _ = app.emit("reever://log", if now { "⏸ микрофон на паузе" } else { "▶ микрофон активен" });
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // --- закрытие окна -> прячем в трей ---
            if let Some(w) = app.get_webview_window("main") {
                let wc = w.clone();
                w.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = wc.hide();
                    }
                });
                // Автозапуск (--hidden) -> стартуем в трей; обычный запуск -> показываем окно.
                let hidden = std::env::args().any(|a| a == "--hidden");
                if !hidden {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

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

            // --- аудио-воркер ---
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("reever-audio".into())
                .spawn(move || recognizer::run_worker(handle, cfg, shared))
                .expect("failed to spawn audio worker");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands_api::list_actions,
            commands_api::list_apps,
            commands_api::list_sound_packs,
            commands_api::save_action,
            commands_api::delete_action,
            commands_api::add_phrase,
            commands_api::remove_phrase,
            commands_api::start_calibration,
            commands_api::cancel_calibration,
            commands_api::get_settings,
            commands_api::save_settings,
            commands_api::set_paused,
            commands_api::get_paused,
            commands_api::set_autostart,
            commands_api::get_autostart,
            commands_api::set_hotkey,
            commands_api::set_mute_hotkey,
            commands_api::get_wallpaper,
            commands_api::check_everything,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
