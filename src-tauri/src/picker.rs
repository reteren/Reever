//! Окно выбора «ривер найди …»: несколько совпадений из индекса — список в центре
//! экрана (нативное окно, dialogs_native/picker.rs). Выбор — кликом, клавишами или голосом
//! («третий»); открытие записывается в историю для ранжирования (find::Usage).
//! В окне можно и печатать: поле поиска ищет по индексу на каждую букву, как Everything
//! (`find::search_typed`).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use crate::app::AppHandle;

use crate::actions::SharedState;
use crate::config::ReeverConfig;
use crate::events::ui_log;
use crate::find::{FindItem, FindScope, Usage};
use crate::{launcher, winutil};

/// Окно открыто (варианты показаны).
static OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
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
    show_payload(app, shared, title, query, scope, items, false)
}

/// Окно индекса: то же окно поиска, но без найденного голосом — человек печатает сам.
/// `query` — что сказали после «индекс» («ривер индекс отчёт»), обычно пусто.
pub fn show_index(app: &AppHandle, shared: &SharedState, query: &str) -> u64 {
    show_payload(app, shared, "", query, FindScope::Any, &[], true)
}

fn show_payload(
    app: &AppHandle,
    shared: &SharedState,
    title: &str,
    query: &str,
    scope: FindScope,
    items: &[FindItem],
    browse: bool,
) -> u64 {
    crate::dialog_demo::asked("picker");
    let session = shared.picker_session.fetch_add(1, Ordering::SeqCst) + 1;
    SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut picker) = shared.picker.lock() {
        *picker = items.to_vec();
    }
    OPEN.store(true, Ordering::SeqCst);
    let payload = crate::dialogs_native::picker::Payload {
        title: title.to_string(),
        query: query.to_string(),
        scope,
        items: items.to_vec(),
        browse,
    };
    crate::dialogs_native::picker::show(app, Arc::new(NativeHost { app: app.clone() }), payload);
    session
}

/// Окно выбора просит: выбрать, закрыть, искать набранное.
struct NativeHost {
    app: AppHandle,
}

impl crate::dialogs_native::picker::Host for NativeHost {
    fn choose(&self, index: usize) {
        let shared = self.app.state::<Arc<SharedState>>().inner().clone();
        let cfg = self.app.state::<crate::config::LiveConfig>().get();
        mark_closed(&shared);
        choose(&self.app, &shared, &cfg, index);
    }

    fn close(&self) {
        let shared = self.app.state::<Arc<SharedState>>().inner().clone();
        mark_closed(&shared);
        hide(&self.app, &shared);
        ui_log(&self.app, crate::msg!("log.choice.closed"));
    }

    fn search(&self, query: &str, scope: FindScope) -> Result<Option<Vec<FindItem>>, String> {
        let shared = self.app.state::<Arc<SharedState>>().inner().clone();
        search_blocking(&self.app, &shared, query, scope)
    }
}

/// Окно выбора на экране: пока оно открыто, аудио-поток не слушает голос.
pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
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
    OPEN.store(false, Ordering::SeqCst);
    crate::dialogs_native::picker::hide(app);
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

/// Поиск во вкладке «Индекс» главного окна: то же, что поле окна выбора, но список окна
/// выбора не трогает. Индекс выключен или строится — ошибка `index-off`.
pub fn search_index(app: &AppHandle, shared: &SharedState, query: &str, scope: FindScope) -> Result<Vec<FindItem>, String> {
    let manager = app
        .try_state::<crate::local_index::IndexManager>()
        .map(|manager| manager.inner().clone());
    let Some(index) = manager.and_then(|manager| manager.snapshot()) else {
        return Err("index-off".to_string());
    };
    let usage = Usage::load(&shared.usage_path);
    Ok(crate::find::search_typed(&index, query, scope, &usage, &windows_recent()))
}

/// Поиск, набранный в окне: варианты по индексу дисков (`find::search_typed`). Найденное
/// становится списком окна — его номера выбирают и голосом, и клавишами. `None` в ответе —
/// пришёл более новый запрос, этот ответ устарел. Индекс выключен — ошибка `index-off`.
fn search_blocking(
    app: &AppHandle,
    shared: &Arc<SharedState>,
    query: &str,
    scope: FindScope,
) -> Result<Option<Vec<FindItem>>, String> {
    let generation = SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let manager = app
        .try_state::<crate::local_index::IndexManager>()
        .map(|manager| manager.inner().clone());
    let Some(index) = manager.and_then(|manager| manager.snapshot()) else {
        return Err("index-off".to_string());
    };
    if SEARCH_GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(None);
    }
    let usage = Usage::load(&shared.usage_path);
    let items = crate::find::search_typed(&index, query, scope, &usage, &windows_recent());
    let mut picker = shared.picker.lock().unwrap_or_else(|e| e.into_inner());
    if SEARCH_GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(None);
    }
    *picker = items.clone();
    Ok(Some(items))
}
