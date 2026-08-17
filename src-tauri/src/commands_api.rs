use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::actions::{Action, SharedState};
use crate::apps::{self, AppEntry};
use crate::config::ReeverConfig;

/// Список всех действий (для UI).
#[tauri::command]
pub fn list_actions(state: State<'_, Arc<SharedState>>) -> Vec<Action> {
    state.actions.lock().map(|a| a.clone()).unwrap_or_default()
}

/// Установленные приложения (вкл. UWP) для пикера.
#[tauri::command]
pub fn list_apps() -> Vec<AppEntry> {
    apps::list_start_apps()
}

/// Доступные наборы звуков (подпапки sounds/).
#[tauri::command]
pub fn list_sound_packs(config: State<'_, Arc<ReeverConfig>>) -> Vec<String> {
    let mut packs: Vec<String> = std::fs::read_dir(&config.sounds_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    if packs.is_empty() {
        packs.push("default".into());
    }
    packs.sort();
    packs
}

/// Универсализация пути: C:\Users\me\… -> %USERPROFILE%\… (переносимость).
fn universalize(target: &str, kind: &str) -> String {
    if kind == "uwp" || target.contains("://") {
        return target.to_string();
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA", "APPDATA", "USERPROFILE"] {
        if let Ok(val) = std::env::var(var) {
            if !val.is_empty() && target.to_lowercase().starts_with(&val.to_lowercase()) {
                return format!("%{}%{}", var, &target[val.len()..]);
            }
        }
    }
    target.to_string()
}

/// Создать/обновить действие (upsert по id).
#[tauri::command]
pub fn save_action(state: State<'_, Arc<SharedState>>, mut action: Action) -> Result<(), String> {
    action.target = universalize(&action.target, &action.kind);
    {
        let mut actions = state.actions.lock().map_err(|e| e.to_string())?;
        if let Some(existing) = actions.iter_mut().find(|a| a.id == action.id) {
            *existing = action;
        } else {
            actions.push(action);
        }
    }
    state.persist();
    Ok(())
}

/// Удалить действие.
#[tauri::command]
pub fn delete_action(state: State<'_, Arc<SharedState>>, id: String) -> Result<(), String> {
    {
        let mut actions = state.actions.lock().map_err(|e| e.to_string())?;
        actions.retain(|a| a.id != id);
    }
    state.persist();
    Ok(())
}

/// Добавить триггер-фразу вручную.
#[tauri::command]
pub fn add_phrase(
    state: State<'_, Arc<SharedState>>,
    id: String,
    phrase: String,
) -> Result<(), String> {
    let phrase = phrase.trim().to_lowercase();
    if phrase.is_empty() {
        return Ok(());
    }
    {
        let mut actions = state.actions.lock().map_err(|e| e.to_string())?;
        if let Some(a) = actions.iter_mut().find(|a| a.id == id) {
            if !a.phrases.iter().any(|p| p.eq_ignore_ascii_case(&phrase)) {
                a.phrases.push(phrase);
            }
        }
    }
    state.persist();
    Ok(())
}

/// Удалить одну триггер-фразу у действия.
#[tauri::command]
pub fn remove_phrase(
    state: State<'_, Arc<SharedState>>,
    id: String,
    phrase: String,
) -> Result<(), String> {
    {
        let mut actions = state.actions.lock().map_err(|e| e.to_string())?;
        if let Some(a) = actions.iter_mut().find(|a| a.id == id) {
            a.phrases.retain(|p| p != &phrase);
        }
    }
    state.persist();
    Ok(())
}

/// Включить режим калибровки для действия: следующая сказанная фраза уйдёт в него.
#[tauri::command]
pub fn start_calibration(state: State<'_, Arc<SharedState>>, id: String) -> Result<(), String> {
    *state.calibrate.lock().map_err(|e| e.to_string())? = Some(id);
    Ok(())
}

/// Отменить калибровку.
#[tauri::command]
pub fn cancel_calibration(state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    *state.calibrate.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}

/// Прочитать settings.json (для UI).
#[tauri::command]
pub fn get_settings(config: State<'_, Arc<ReeverConfig>>) -> Result<serde_json::Value, String> {
    let txt = std::fs::read_to_string(&config.settings_path).map_err(|e| e.to_string())?;
    serde_json::from_str(&txt).map_err(|e| e.to_string())
}

/// Сохранить settings.json (применяется при перезапуске).
#[tauri::command]
pub fn save_settings(
    config: State<'_, Arc<ReeverConfig>>,
    settings: serde_json::Value,
) -> Result<(), String> {
    let txt = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&config.settings_path, txt).map_err(|e| e.to_string())
}

/// Пауза/возобновление микрофона.
#[tauri::command]
pub fn set_paused(state: State<'_, Arc<SharedState>>, paused: bool) {
    state.paused.store(paused, Ordering::Relaxed);
}

#[tauri::command]
pub fn get_paused(state: State<'_, Arc<SharedState>>) -> bool {
    state.paused.load(Ordering::Relaxed)
}

/// Автозапуск с Windows.
#[tauri::command]
pub fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    let m = app.autolaunch();
    if on {
        m.enable().map_err(|e| e.to_string())
    } else {
        m.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Активная проверка доступности Everything HTTP (для плитки UI, по запросу).
#[tauri::command]
pub fn check_everything(config: State<'_, Arc<ReeverConfig>>) -> bool {
    if config.settings.offline_mode {
        return false; // офлайн-режим: вообще не трогаем сеть
    }
    crate::everything::ping(
        &config.settings.everything_host,
        config.settings.everything_port,
    )
    .is_ok()
}

/// Обои Frutiger Aero как data-URL (фон вебвью). Пользователь может заменить
/// %APPDATA%/com.troid.reever/wallpaper.png. Пусто → фронт рисует CSS-небо.
#[tauri::command]
pub fn get_wallpaper(app: AppHandle) -> Option<String> {
    let path = crate::config::user_config_path(&app, "wallpaper.png");
    let bytes = std::fs::read(&path).ok()?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };
    Some(format!("data:{};base64,{}", mime, b64(&bytes)))
}

/// Минимальный base64 без внешних зависимостей.
fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Пере-регистрация ОБОИХ хоткеев (ptt + mute) разом — иначе unregister_all
/// сбрасывает и второй. Источник правды — поля SharedState.
fn reapply_hotkeys(app: &AppHandle, mode: &str, ptt: &str, mute: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if mode != "wake" && !ptt.trim().is_empty() {
        gs.register(ptt).map_err(|e| e.to_string())?;
    }
    if !mute.trim().is_empty() {
        gs.register(mute).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Перерегистрировать push-to-talk хоткей на лету (применяется сразу, без рестарта).
#[tauri::command]
pub fn set_hotkey(
    app: AppHandle,
    config: State<'_, Arc<ReeverConfig>>,
    state: State<'_, Arc<SharedState>>,
    hotkey: String,
) -> Result<(), String> {
    *state.ptt_hotkey.lock().map_err(|e| e.to_string())? = hotkey.clone();
    let mute = state.mute_hotkey.lock().map_err(|e| e.to_string())?.clone();
    reapply_hotkeys(&app, &config.settings.activation_mode, &hotkey, &mute)
}

/// Перерегистрировать mute-хоткей (panic-mute) на лету.
#[tauri::command]
pub fn set_mute_hotkey(
    app: AppHandle,
    config: State<'_, Arc<ReeverConfig>>,
    state: State<'_, Arc<SharedState>>,
    hotkey: String,
) -> Result<(), String> {
    *state.mute_hotkey.lock().map_err(|e| e.to_string())? = hotkey.clone();
    let ptt = state.ptt_hotkey.lock().map_err(|e| e.to_string())?.clone();
    reapply_hotkeys(&app, &config.settings.activation_mode, &ptt, &hotkey)
}
