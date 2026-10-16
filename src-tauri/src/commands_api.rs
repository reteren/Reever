use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::actions::{Action, SharedState};
use crate::apps::{self, AppEntry};
use crate::config::{LiveConfig, ReeverConfig};
use crate::grammar::Grammar;

/// Список всех действий (для UI).
#[tauri::command]
pub fn list_actions(state: State<'_, Arc<SharedState>>) -> Vec<Action> {
    state.actions.lock().map(|a| a.clone()).unwrap_or_default()
}

/// Вернуть текущую редактируемую грамматику для раздела «Что можно сказать».
#[tauri::command]
pub fn get_grammar(state: State<'_, Arc<SharedState>>) -> Grammar {
    state
        .grammar
        .lock()
        .map(|grammar| grammar.clone())
        .unwrap_or_else(|_| Grammar::defaults())
}

/// Проверить и применить грамматику сразу, затем сохранить её для следующего запуска.
#[tauri::command]
pub fn save_grammar(state: State<'_, Arc<SharedState>>, grammar: Grammar) -> Result<(), String> {
    grammar.validate().map_err(|errors| errors.join("\n"))?;
    let previous = {
        let mut current = state.grammar.lock().map_err(|error| error.to_string())?;
        let previous = current.clone();
        *current = grammar.clone();
        previous
    };
    if let Err(error) = state.persist_grammar() {
        if let Ok(mut current) = state.grammar.lock() {
            if *current == grammar {
                *current = previous;
            }
        }
        return Err(format!("Не удалось сохранить грамматику: {error}"));
    }
    state.bump_grammar();
    Ok(())
}

/// Восстановить встроенные значения, сохранить их и уведомить аудио-воркер о версии.
#[tauri::command]
pub fn reset_grammar(state: State<'_, Arc<SharedState>>) -> Grammar {
    let grammar = Grammar::defaults();
    match state.grammar.lock() {
        Ok(mut current) => {
            *current = grammar.clone();
            drop(current);
            state.bump_grammar();
            if let Err(error) = state.persist_grammar() {
                eprintln!("[reever] не удалось сохранить грамматику по умолчанию: {error}");
            }
        }
        Err(error) => eprintln!("[reever] не удалось сбросить грамматику: {error}"),
    }
    grammar
}

/// Установленные приложения (вкл. UWP) для пикера.
#[tauri::command]
pub fn list_apps() -> Vec<AppEntry> {
    apps::list_start_apps()
}

/// Доступные наборы звуков (подпапки sounds/).
#[tauri::command]
pub fn list_sound_packs(config: State<'_, LiveConfig>) -> Vec<String> {
    let config = config.get();
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

/// Доступные папки моделей распознавания в resources/model/.
#[tauri::command]
pub fn list_asr_models(app: AppHandle) -> Vec<String> {
    let model_root = crate::config::resource(&app, "model");
    let mut models: Vec<String> = std::fs::read_dir(model_root)
        .map(|rd| {
            rd.flatten()
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    models
}

/// Универсализация пути: C:\Users\me\… -> %USERPROFILE%\… (переносимость).
fn universalize(target: &str, kind: &str) -> String {
    if kind == "uwp" || target.contains("://") {
        return target.to_string();
    }
    for var in [
        "ProgramFiles(x86)",
        "ProgramFiles",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
    ] {
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
pub fn get_settings(config: State<'_, LiveConfig>) -> Result<serde_json::Value, String> {
    let config = config.get();
    let txt = std::fs::read_to_string(&config.settings_path).map_err(|e| e.to_string())?;
    let mut settings = match serde_json::from_str::<serde_json::Value>(&txt) {
        Ok(serde_json::Value::Object(object)) => serde_json::Value::Object(object),
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    let normalized = serde_json::to_value(&config.settings).map_err(|e| e.to_string())?;
    if let (Some(object), Some(defaults)) = (settings.as_object_mut(), normalized.as_object()) {
        // Keep comments and future/unknown keys, while returning recovered known fields
        // so one malformed value cannot break the settings form.
        for (key, value) in defaults {
            // Legacy compatibility fields stay absent in newly written settings files,
            // but existing values remain available to older installations.
            if (key == "endpoint_ms" || key == "english_enabled") && !object.contains_key(key) {
                continue;
            }
            object.insert(key.clone(), value.clone());
        }
    }
    Ok(settings)
}

/// Сохранить settings.json и применить сразу: настройки перечитываются, хоткеи
/// перерегистрируются, поток распознавания перезапускается с новыми моделями и паузами.
#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    config: State<'_, LiveConfig>,
    state: State<'_, Arc<SharedState>>,
    settings: serde_json::Value,
) -> Result<(), String> {
    let path = config.get().settings_path.clone();
    let txt = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, txt).map_err(|e| e.to_string())?;
    let fresh = ReeverConfig::resolve(&app).map_err(|e| e.to_string())?;
    let s = &fresh.settings;
    reapply_hotkeys(&app, &s.activation_mode, &s.hotkey, &s.mute_hotkey)?;
    *state.ptt_hotkey.lock().map_err(|e| e.to_string())? = s.hotkey.clone();
    *state.mute_hotkey.lock().map_err(|e| e.to_string())? = s.mute_hotkey.clone();
    config.set(Arc::new(fresh));
    state.reload.store(true, Ordering::Relaxed);
    Ok(())
}

/// Язык интерфейса: сохраняет выбор и сразу переводит меню трея. Возвращает
/// итоговый код языка (`auto` раскрывается в язык Windows).
#[tauri::command]
pub fn set_ui_language(
    config: State<'_, LiveConfig>,
    tray: State<'_, crate::uilang::TrayItems>,
    language: String,
) -> Result<String, String> {
    let config = config.get();
    let locale = crate::uilang::resolve(&language);
    crate::uilang::apply_tray(&tray, locale);
    *crate::uilang::CURRENT.lock().map_err(|e| e.to_string())? = locale;
    persist_hotkey(&config, "ui_language", &language)?;
    Ok(locale.into())
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
pub fn check_everything(config: State<'_, LiveConfig>) -> bool {
    let config = config.get();
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
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
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

fn persist_hotkey(config: &ReeverConfig, key: &str, hotkey: &str) -> Result<(), String> {
    let mut settings = std::fs::read_to_string(&config.settings_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or(serde_json::to_value(&config.settings).map_err(|e| e.to_string())?);
    let object = settings
        .as_object_mut()
        .ok_or_else(|| "settings.json должен содержать объект".to_string())?;
    object.insert(key.into(), serde_json::Value::String(hotkey.into()));
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&config.settings_path, text).map_err(|e| e.to_string())
}

/// Перерегистрировать push-to-talk хоткей на лету (применяется сразу, без рестарта).
#[tauri::command(rename_all = "camelCase")]
pub fn set_hotkey(
    app: AppHandle,
    config: State<'_, LiveConfig>,
    state: State<'_, Arc<SharedState>>,
    hotkey: String,
    activation_mode: String,
) -> Result<(), String> {
    let config = config.get();
    let mute = state.mute_hotkey.lock().map_err(|e| e.to_string())?.clone();
    reapply_hotkeys(&app, &activation_mode, &hotkey, &mute)?;
    *state.ptt_hotkey.lock().map_err(|e| e.to_string())? = hotkey.clone();
    persist_hotkey(&config, "hotkey", &hotkey)
}

/// Перерегистрировать mute-хоткей (panic-mute) на лету.
#[tauri::command(rename_all = "camelCase")]
pub fn set_mute_hotkey(
    app: AppHandle,
    config: State<'_, LiveConfig>,
    state: State<'_, Arc<SharedState>>,
    hotkey: String,
    activation_mode: String,
) -> Result<(), String> {
    let config = config.get();
    let ptt = state.ptt_hotkey.lock().map_err(|e| e.to_string())?.clone();
    reapply_hotkeys(&app, &activation_mode, &ptt, &hotkey)?;
    *state.mute_hotkey.lock().map_err(|e| e.to_string())? = hotkey.clone();
    persist_hotkey(&config, "mute_hotkey", &hotkey)
}
