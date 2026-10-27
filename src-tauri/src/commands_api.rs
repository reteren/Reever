use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::actions::{Action, SharedState};
use crate::apps::{self, AppEntry};
use crate::appscan;
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

/// Встроенные фразы команд на языках, которые сейчас не распознаются, — редактор их прячет.
#[tauri::command]
pub fn inactive_triggers(state: State<'_, Arc<SharedState>>) -> Vec<String> {
    state
        .grammar
        .lock()
        .map(|grammar| grammar.inactive_triggers())
        .unwrap_or_default()
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

/// Сканер программ (кнопка «Сканировать» во вкладке «Действия»): программы из меню
/// «Пуск» с готовыми фразами на включённых языках распознавания.
#[tauri::command]
pub async fn scan_apps(
    state: State<'_, Arc<SharedState>>,
    config: State<'_, LiveConfig>,
) -> Result<Vec<appscan::ScannedApp>, String> {
    let config = config.get();
    let languages = config.settings.recognition_languages();
    let actions = state.actions.lock().map(|a| a.clone()).unwrap_or_default();
    let reserved: Vec<String> = state
        .grammar
        .lock()
        .map(|grammar| {
            let mut words = grammar.wake_words.clone();
            words.extend(grammar.cancel_words.iter().cloned());
            words.extend(grammar.dictation_stop.iter().cloned());
            words.extend(grammar.intents.iter().flat_map(|intent| intent.triggers.iter().cloned()));
            words
        })
        .unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let mut found = appscan::scan(apps::list_start_apps(), &languages, &actions, &reserved);
        // Часто используемые — сверху, по истории запусков Windows.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        appscan::order_by_usage(&mut found, &appscan::user_assist(), now);
        found
    })
    .await
    .map_err(|e| e.to_string())
}

/// Иконки программ из списка сканера (data-URL по AppID) — отдельно, пачками: список
/// показывается сразу, иконки подгружаются следом.
#[tauri::command]
pub async fn scan_app_icons(ids: Vec<String>) -> Result<std::collections::HashMap<String, String>, String> {
    tauri::async_runtime::spawn_blocking(move || appscan::icons(&ids))
        .await
        .map_err(|e| e.to_string())
}

/// Добавить отмеченные в сканере программы как действия. Возвращает, сколько добавлено.
#[tauri::command]
pub fn add_scanned_apps(
    state: State<'_, Arc<SharedState>>,
    apps: Vec<appscan::ChosenApp>,
) -> Result<usize, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let added = {
        let mut actions = state.actions.lock().map_err(|e| e.to_string())?;
        let mut new = appscan::to_actions(apps, &actions, stamp);
        for action in &mut new {
            action.target = universalize(&action.target, &action.kind);
        }
        let count = new.len();
        actions.extend(new);
        count
    };
    state.persist();
    Ok(added)
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
        packs.push(crate::config::SOUND_PACK.into());
    }
    packs.sort();
    packs
}

/// Языковые пакеты для экрана «Языки распознавания»: какой пакет нужен каждому языку и
/// какие скачаны (models.rs). Скачивает их установщик.
#[tauri::command]
pub fn language_models(app: AppHandle) -> serde_json::Value {
    use crate::models::{ModelDirs, PACKS};
    let dirs = ModelDirs::new(crate::config::resource(&app, "model"));
    let languages: serde_json::Map<String, serde_json::Value> = crate::config::RECOGNITION_LANGUAGES
        .iter()
        .filter_map(|code| {
            crate::models::family_for(code).map(|family| (code.to_string(), json!(family)))
        })
        .collect();
    // Семьи, у которых установлен хоть один вариант.
    let mut installed: Vec<&str> = PACKS
        .iter()
        .filter(|pack| dirs.installed(pack))
        .map(|pack| pack.family)
        .collect();
    installed.dedup();
    json!({ "languages": languages, "installed": installed })
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

/// Подстройка под голос (voice_adapt.rs): начать запись «ривер».
#[tauri::command]
pub fn start_voice_adapt(state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    *state.voice_record.lock().map_err(|e| e.to_string())? = Some(Vec::new());
    Ok(())
}

/// Бросить запись подстройки, ничего не меняя.
#[tauri::command]
pub fn cancel_voice_adapt(state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    if let Some(mut pcm) = state.voice_record.lock().map_err(|e| e.to_string())?.take() {
        pcm.fill(0.0);
    }
    Ok(())
}

/// Закончить запись и подстроить детектор под слова в ней. Err `tooFew:<слов>` — «ривер»
/// мало, остальное — текст ошибки. Подстроено — поток распознавания перезапускается с ней.
#[tauri::command]
pub async fn finish_voice_adapt(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    config: State<'_, LiveConfig>,
) -> Result<crate::voice_adapt::Report, String> {
    let mut pcm = state.voice_record.lock().map_err(|e| e.to_string())?.take().unwrap_or_default();
    let config = config.get();
    let (name, weights) = crate::recognizer::spotter_weights(&app, &config);
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        let result = adapt_and_save(&pcm, name, &weights, &dir);
        if result.is_ok() {
            crate::voice_adapt::save_enrollment(&dir.join("voice"), &pcm);
        }
        pcm.fill(0.0);
        result
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.as_ref().is_ok_and(|report| report.adapted) {
        state.reload.store(true, Ordering::Relaxed);
    }
    result
}

fn adapt_and_save(
    pcm: &[f32],
    name: &str,
    weights: &std::path::Path,
    dir: &std::path::Path,
) -> Result<crate::voice_adapt::Report, String> {
    let words = crate::voice_adapt::words(pcm).len();
    if words < crate::voice_adapt::MIN_WORDS {
        return Err(format!("tooFew:{words}"));
    }
    let bytes = std::fs::read(weights).map_err(|e| e.to_string())?;
    let base = crate::kws::TcnModel::from_bytes(&bytes).map_err(|e| format!("{e:#}"))?;
    let bank = crate::voice_adapt::Bank::load(&weights.with_extension("bank"), &bytes).map_err(|e| format!("{e:#}"))?;
    let (model, report) = crate::voice_adapt::adapt(&base, pcm, &bank).map_err(|e| format!("{e:#}"))?;
    if let Some(model) = model {
        let (bin, meta) = crate::wake::personal_paths(dir, name);
        std::fs::create_dir_all(bin.parent().unwrap_or(dir)).map_err(|e| e.to_string())?;
        std::fs::write(&bin, model.to_bytes()).map_err(|e| e.to_string())?;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let info = json!({ "base": format!("{:016x}", crate::voice_adapt::fnv1a(&bytes)), "report": report, "at": at });
        std::fs::write(&meta, info.to_string()).map_err(|e| e.to_string())?;
    }
    Ok(report)
}

/// Тренер тихой подстройки (voice_adapt::start_passive): учит голову для весов, что сейчас
/// работают, и при успехе перезапускает поток распознавания с ней.
pub fn passive_trainer(app: AppHandle, shared: Arc<SharedState>) -> crate::voice_adapt::Trainer {
    Box::new(move |pcm| {
        let config = app.state::<LiveConfig>().get();
        let (name, weights) = crate::recognizer::spotter_weights(&app, &config);
        let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
        let result = adapt_and_save(pcm, name, &weights, &dir);
        let line = match &result {
            Ok(report) => format!(
                "voice: quiet adaptation on {} wake words — detector heard {} before, {} after{}",
                report.words,
                report.heard_before,
                report.heard_after,
                if report.adapted { "; applied" } else { "; no gain, kept as is" }
            ),
            Err(error) => format!("voice: quiet adaptation skipped: {error}"),
        };
        crate::events::ui_log(&app, line);
        if result.as_ref().is_ok_and(|report| report.adapted) {
            shared.reload.store(true, Ordering::Relaxed);
        }
        result
    })
}

/// Подстройка для весов, что сейчас работают (`null` — её нет), и сколько обращений
/// собрала тихая подстройка.
#[tauri::command]
pub fn voice_adapt_status(app: AppHandle, config: State<'_, LiveConfig>) -> Option<serde_json::Value> {
    let config = config.get();
    let (name, weights) = crate::recognizer::spotter_weights(&app, &config);
    let dir = app.path().app_config_dir().ok()?;
    let passive = crate::voice_adapt::PassiveState::load(&dir.join("voice"));
    let base = std::fs::read(weights).ok();
    let personal = base.and_then(|base| crate::wake::personal_for(&dir, name, &base).map(|(_, meta)| meta));
    Some(json!({
        "report": personal.as_ref().and_then(|meta| meta.get("report")).cloned(),
        "passive": { "collected": passive.collected, "goal": crate::voice_adapt::PASSIVE_GOAL },
    }))
}

/// Калибровку «скажите „ривер“ 10 раз» требует свежая установка (config::voice_intro_pending).
#[tauri::command]
pub fn voice_intro_pending() -> bool {
    crate::config::voice_intro_pending()
}

/// Обязательная калибровка закончена — до следующей установки не спрашивать.
#[tauri::command]
pub fn voice_intro_done() {
    crate::config::clear_voice_intro_pending();
}

/// Убрать подстройку для всех весов — снова общий детектор, и тихая подстройка
/// начинается заново.
#[tauri::command]
pub fn reset_voice_adapt(app: AppHandle, state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    for name in ["wake/river.bin", "wake/river-ru-en.bin"] {
        let (bin, meta) = crate::wake::personal_paths(&dir, name);
        let _ = std::fs::remove_file(bin);
        let _ = std::fs::remove_file(meta);
    }
    crate::voice_adapt::reset_passive(&dir.join("voice"));
    state.reload.store(true, Ordering::Relaxed);
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
    let local_index = s.local_index;
    config.set(Arc::new(fresh));
    if let Some(index) = app.try_state::<crate::local_index::IndexManager>() {
        index.set_enabled(local_index);
    }
    state.reload.store(true, Ordering::Relaxed);
    Ok(())
}

/// Состояние встроенного индекса для плитки Главной и UI настроек.
#[tauri::command]
pub fn index_status(index: State<'_, crate::local_index::IndexManager>) -> crate::local_index::IndexStatus {
    index.status()
}

/// Язык интерфейса: сохраняет выбор; меню трея (traymenu.rs) подхватывает его при открытии. Возвращает
/// итоговый код языка (`auto` раскрывается в язык Windows).
#[tauri::command]
pub fn set_ui_language(
    config: State<'_, LiveConfig>,
    language: String,
) -> Result<String, String> {
    let config = config.get();
    let locale =
        crate::uilang::resolve_enabled(&language, &config.settings.interface_languages());
    *crate::uilang::CURRENT.lock().map_err(|e| e.to_string())? = locale;
    persist_hotkey(&config, "ui_language", &language)?;
    Ok(locale.into())
}

/// Маскот: режим показа и размер применяются сразу, без «Сохранить» и без
/// перезапуска распознавания.
#[tauri::command]
pub fn set_mascot(
    app: AppHandle,
    config: State<'_, LiveConfig>,
    mode: String,
    scale: u32,
    bubble: bool,
) -> Result<(), String> {
    let mode = crate::mascot::normalize_mode(&mode).to_string();
    let scale = crate::mascot::clamp_scale(scale);
    let current = config.get();
    persist_value(&current, "mascot_mode", serde_json::Value::String(mode.clone()))?;
    persist_value(&current, "mascot_scale", serde_json::Value::from(scale))?;
    persist_value(&current, "mascot_bubble", serde_json::Value::Bool(bubble))?;
    let mut fresh = (*current).clone();
    fresh.settings.mascot_mode = mode.clone();
    fresh.settings.mascot_scale = scale;
    fresh.settings.mascot_bubble = bubble;
    let effective = fresh.settings.effective_mascot_mode().to_string();
    config.set(Arc::new(fresh));
    crate::mascot::apply(&app, &effective, scale);
    crate::bubble::configure(&app, bubble);
    Ok(())
}

/// Лёгкий интерфейс вкл/выкл: сохраняет выбор, убирает или возвращает маскота и
/// пересоздаёт главное окно в другом виде (вид задаётся при создании окна).
#[tauri::command]
pub fn set_lite_ui(app: AppHandle, config: State<'_, LiveConfig>, lite: bool) -> Result<(), String> {
    let current = config.get();
    persist_value(&current, "lite_ui", serde_json::Value::Bool(lite))?;
    let mut fresh = (*current).clone();
    fresh.settings.lite_ui = lite;
    let (mode, scale) = (fresh.settings.effective_mascot_mode().to_string(), fresh.settings.mascot_scale);
    config.set(Arc::new(fresh));
    crate::config::write_installer_ui_style(lite);
    crate::mascot::apply(&app, &mode, scale);
    crate::rebuild_main(&app);
    Ok(())
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
    persist_value(config, key, serde_json::Value::String(hotkey.into()))
}

/// Записывает одно поле в settings.json, не трогая остальные.
fn persist_value(config: &ReeverConfig, key: &str, value: serde_json::Value) -> Result<(), String> {
    let mut settings = std::fs::read_to_string(&config.settings_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or(serde_json::to_value(&config.settings).map_err(|e| e.to_string())?);
    let object = settings
        .as_object_mut()
        .ok_or_else(|| "settings.json должен содержать объект".to_string())?;
    object.insert(key.into(), value);
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
