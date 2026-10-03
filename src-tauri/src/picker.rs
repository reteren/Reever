//! Окно выбора «ривер найди …»: несколько совпадений из Everything — список в центре
//! экрана (окно `picker`, frontend/picker.html). Выбор — кликом, клавишами или голосом
//! («третий»); открытие записывается в историю для ранжирования (find::Usage).

use std::sync::{atomic::Ordering, Arc};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::actions::SharedState;
use crate::config::ReeverConfig;
use crate::events::ui_log;
use crate::find::{FindItem, Usage};
use crate::{launcher, winutil};

/// Показать варианты. `title` — что искали (для заголовка окна).
/// Возвращает номер окна — по нему аудио-поток узнаёт, что закрыли именно его.
pub fn show(app: &AppHandle, shared: &SharedState, title: &str, items: &[FindItem]) -> u64 {
    let session = shared.picker_session.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(mut picker) = shared.picker.lock() {
        *picker = items.to_vec();
    }
    let _ = app.emit_to("picker", "reever://picker", json!({ "title": title, "items": items }));
    if let Some(window) = app.get_webview_window("picker") {
        let _ = window.center();
        let _ = window.show();
        let _ = window.set_focus();
        // Команда пришла голосом, без клика — Windows не даёт фокус фоновой программе;
        // тот же обход, что и при подъёме найденного окна.
        if let Ok(hwnd) = window.hwnd() {
            winutil::bring_to_front_raw(hwnd.0 as isize);
        }
    }
    session
}

/// Окно закрыто из интерфейса: отметить его номер, чтобы аудио-поток перестал ждать номер.
pub fn mark_closed(shared: &SharedState) {
    let session = shared.picker_session.load(Ordering::SeqCst);
    shared.picker_closed_session.store(session, Ordering::SeqCst);
}

/// Спрятать окно и забыть варианты.
pub fn hide(app: &AppHandle, shared: &SharedState) {
    if let Ok(mut picker) = shared.picker.lock() {
        picker.clear();
    }
    if let Some(window) = app.get_webview_window("picker") {
        let _ = window.hide();
    }
}

/// Открыть вариант номер `index` (с нуля). `false` — номера нет или открыть не удалось.
pub fn choose(app: &AppHandle, shared: &SharedState, cfg: &ReeverConfig, index: usize) -> bool {
    // Забрать вариант и очистить список под одной блокировкой: выбор мышью и голосом
    // почти одновременно не откроет два результата.
    let item = shared.picker.lock().ok().and_then(|mut picker| {
        let item = picker.get(index).cloned();
        if item.is_some() {
            picker.clear();
        }
        item
    });
    let Some(item) = item else {
        ui_log(app, format!("⚠ варианта №{} нет в списке", index + 1));
        return false;
    };
    hide(app, shared);
    open(app, shared, cfg, &item)
}

/// Открыть найденное: папку — в Проводнике, файл — программой по умолчанию.
pub fn open(app: &AppHandle, shared: &SharedState, cfg: &ReeverConfig, item: &FindItem) -> bool {
    if cfg.settings.private_log {
        ui_log(app, if item.folder { "📂 открываю папку" } else { "📄 открываю файл" });
    } else {
        ui_log(app, format!("{} {}", if item.folder { "📂" } else { "📄" }, item.path));
    }
    let before = winutil::snapshot();
    let ok = launcher::shell_open(&item.path);
    if ok {
        Usage::record(&shared.usage_path, &item.path);
        if cfg.settings.raise_result {
            std::thread::sleep(std::time::Duration::from_millis(400));
            winutil::raise_new(&before, &item.name);
        }
    }
    ok
}

#[tauri::command]
pub fn picker_choose(
    app: AppHandle,
    shared: State<'_, Arc<SharedState>>,
    cfg: State<'_, Arc<ReeverConfig>>,
    index: usize,
) -> Result<bool, String> {
    // Отметить до открытия: оно может занять время, а за это время может открыться новое окно.
    mark_closed(&shared);
    Ok(choose(&app, &shared, &cfg, index))
}

#[tauri::command]
pub fn picker_close(app: AppHandle, shared: State<'_, Arc<SharedState>>) -> Result<(), String> {
    mark_closed(&shared);
    hide(&app, &shared);
    ui_log(&app, "↩ выбор закрыт");
    Ok(())
}
