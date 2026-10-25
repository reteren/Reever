//! Окно выбора «ривер найди …»: несколько совпадений из индекса — список в центре
//! экрана (окно `picker`, frontend/picker.html). Выбор — кликом, клавишами или голосом
//! («третий»); открытие записывается в историю для ранжирования (find::Usage).
//! В окне можно и печатать: поле поиска ищет по индексу на каждую букву, как Everything
//! (`picker_search`, `find::search_typed`).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tauri::{webview::WebviewWindowBuilder, AppHandle, Emitter, Manager, State, WebviewWindow, WindowEvent};

use crate::actions::SharedState;
use crate::config::ReeverConfig;
use crate::events::ui_log;
use crate::find::{FindItem, FindScope, Usage};
use crate::{launcher, winutil};

static PICKER_CREATE_PENDING: AtomicBool = AtomicBool::new(false);
static PICKER_READY: AtomicBool = AtomicBool::new(false);
static PENDING_PICKER: Mutex<Option<Value>> = Mutex::new(None);
/// Номер последнего набранного запроса: ответ на устаревший не трогает список.
static SEARCH_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Недавние Windows (`find::windows_recent` читает папку ярлыков) — на время набора.
static RECENT: Mutex<Option<(Instant, Arc<HashMap<String, u64>>)>> = Mutex::new(None);
const RECENT_FOR: Duration = Duration::from_secs(30);

/// Показать варианты. `title` — что искали (для заголовка окна); `query` и `scope` —
/// с чего начинается поле поиска в окне. Возвращает номер окна — по нему аудио-поток
/// узнаёт, что закрыли именно его.
pub fn show(
    app: &AppHandle,
    shared: &SharedState,
    title: &str,
    query: &str,
    scope: FindScope,
    items: &[FindItem],
) -> u64 {
    let session = shared.picker_session.fetch_add(1, Ordering::SeqCst) + 1;
    SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut picker) = shared.picker.lock() {
        *picker = items.to_vec();
    }
    if let Ok(mut pending) = PENDING_PICKER.lock() {
        *pending = Some(json!({ "title": title, "query": query, "scope": scope, "items": items }));
    }
    ensure_picker(app);
    session
}

/// Создать окно по требованию. Событие с вариантами отправит picker_ready после подписки страницы.
fn ensure_picker(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("picker") {
        if PICKER_READY.load(Ordering::SeqCst) {
            publish_picker(app, &window);
        }
        return;
    }
    if PICKER_CREATE_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    PICKER_READY.store(false, Ordering::SeqCst);

    let Some(window_config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "picker")
        .cloned()
    else {
        PICKER_CREATE_PENDING.store(false, Ordering::SeqCst);
        ui_log(app, "⚠ picker window config not found");
        mark_current_session_closed(app);
        return;
    };

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let window = WebviewWindowBuilder::from_config(&app, &window_config)
            .and_then(|builder| builder.build());
        let window = match window {
            Ok(window) => window,
            Err(error) => {
                PICKER_CREATE_PENDING.store(false, Ordering::SeqCst);
                ui_log(&app, format!("⚠ picker window was not created: {error}"));
                mark_current_session_closed(&app);
                return;
            }
        };

        attach_picker_window(&app, &window);
        PICKER_CREATE_PENDING.store(false, Ordering::SeqCst);
        // Закрытие могло прийти, пока WebView создавался. Не оставляем его скрытым в фоне.
        if PENDING_PICKER.lock().map(|pending| pending.is_none()).unwrap_or(true) {
            let _ = window.destroy();
        }
    });
}

fn attach_picker_window(app: &AppHandle, window: &WebviewWindow) {
    let handle = app.clone();
    let shared = app.state::<Arc<SharedState>>().inner().clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            mark_closed(&shared);
            hide(&handle, &shared);
        }
        WindowEvent::Destroyed => {
            PICKER_READY.store(false, Ordering::SeqCst);
            PICKER_CREATE_PENDING.store(false, Ordering::SeqCst);
            if PENDING_PICKER.lock().map(|pending| pending.is_some()).unwrap_or(false) {
                ensure_picker(&handle);
            }
        }
        _ => {}
    });
}

/// Окно выбора на экране: пока оно открыто, аудио-поток не слушает голос.
pub fn is_open() -> bool {
    PENDING_PICKER.lock().map(|pending| pending.is_some()).unwrap_or(false)
}

/// Вызывается из picker.html после загрузки страницы и завершения подписки на событие.
#[tauri::command]
pub fn picker_ready(app: AppHandle) {
    let Some(window) = app.get_webview_window("picker") else { return };
    PICKER_READY.store(true, Ordering::SeqCst);
    publish_picker(&app, &window);
}

fn publish_picker(app: &AppHandle, window: &WebviewWindow) {
    let pending = PENDING_PICKER.lock().ok().and_then(|value| value.clone());
    let Some(payload) = pending else {
        let _ = window.destroy();
        return;
    };

    let _ = app.emit_to("picker", "reever://picker", payload);
    let _ = window.center();
    let _ = window.show();
    let _ = window.set_focus();
    // Команда пришла голосом, без клика — Windows не даёт фокус фоновой программе;
    // тот же обход, что и при подъёме найденного окна.
    if let Ok(hwnd) = window.hwnd() {
        winutil::bring_to_front_raw(hwnd.0 as isize);
    }
}

fn mark_current_session_closed(app: &AppHandle) {
    if let Some(shared) = app.try_state::<Arc<SharedState>>() {
        mark_closed(&shared);
    }
}

/// Окно закрыто из интерфейса: отметить его номер, чтобы аудио-поток перестал ждать номер.
pub fn mark_closed(shared: &SharedState) {
    let session = shared.picker_session.load(Ordering::SeqCst);
    shared.picker_closed_session.store(session, Ordering::SeqCst);
}

/// Спрятать окно и забыть варианты.
pub fn hide(app: &AppHandle, shared: &SharedState) {
    // Поиск, набранный в окне и ещё идущий, не вернёт список закрытому окну.
    SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut picker) = shared.picker.lock() {
        picker.clear();
    }
    if let Ok(mut pending) = PENDING_PICKER.lock() {
        *pending = None;
    }
    PICKER_READY.store(false, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("picker") {
        let _ = window.destroy();
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
        ui_log(app, crate::msg!("log.choice.missing", number = index + 1));
        return false;
    };
    hide(app, shared);
    open(app, shared, cfg, &item)
}

/// Открыть найденное: папку — в Проводнике, файл — программой по умолчанию.
pub fn open(app: &AppHandle, shared: &SharedState, cfg: &ReeverConfig, item: &FindItem) -> bool {
    if cfg.settings.private_log {
        ui_log(app, if item.folder { crate::msg!("log.openingFolder") } else { crate::msg!("log.openingFile") });
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
    cfg: State<'_, crate::config::LiveConfig>,
    index: usize,
) -> Result<bool, String> {
    // Отметить до открытия: оно может занять время, а за это время может открыться новое окно.
    mark_closed(&shared);
    Ok(choose(&app, &shared, &cfg.get(), index))
}

fn windows_recent() -> Arc<HashMap<String, u64>> {
    let mut cached = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    match cached.as_ref() {
        Some((at, recent)) if at.elapsed() < RECENT_FOR => recent.clone(),
        _ => {
            let recent = Arc::new(crate::find::windows_recent());
            *cached = Some((Instant::now(), recent.clone()));
            recent
        }
    }
}

/// Поиск, набранный в окне: варианты по индексу дисков (`find::search_typed`). Найденное
/// становится списком окна — его номера выбирают и голосом, и клавишами. `None` в ответе —
/// пришёл более новый запрос, этот ответ устарел. Индекс выключен — ошибка `index-off`.
#[tauri::command]
pub async fn picker_search(
    app: AppHandle,
    shared: State<'_, Arc<SharedState>>,
    query: String,
    scope: FindScope,
) -> Result<Option<Vec<FindItem>>, String> {
    let generation = SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let shared = shared.inner().clone();
    let manager = app
        .try_state::<crate::local_index::IndexManager>()
        .map(|manager| manager.inner().clone());
    tauri::async_runtime::spawn_blocking(move || {
        let Some(index) = manager.and_then(|manager| manager.snapshot()) else {
            return Err("index-off".to_string());
        };
        if SEARCH_GENERATION.load(Ordering::SeqCst) != generation {
            return Ok(None);
        }
        let usage = Usage::load(&shared.usage_path);
        let items = crate::find::search_typed(&index, &query, scope, &usage, &windows_recent());
        let mut picker = shared.picker.lock().unwrap_or_else(|e| e.into_inner());
        if SEARCH_GENERATION.load(Ordering::SeqCst) != generation {
            return Ok(None);
        }
        *picker = items.clone();
        Ok(Some(items))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn picker_close(app: AppHandle, shared: State<'_, Arc<SharedState>>) -> Result<(), String> {
    mark_closed(&shared);
    hide(&app, &shared);
    ui_log(&app, crate::msg!("log.choice.closed"));
    Ok(())
}
