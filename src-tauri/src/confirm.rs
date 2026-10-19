//! Плашка подтверждения опасной команды («ривер выключи компьютер», «закрой …») при
//! включённом «спрашивать» (`confirm_dangerous`): окно `confirm` в центре экрана
//! (frontend/confirm.html) с кнопками «Да» / «Нет». Голосовой ответ работает как раньше;
//! клик кладёт ответ сюда, аудио-поток забирает его (`take_answer`) и перестаёт ждать голос.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

use serde_json::{json, Value};
use tauri::{webview::WebviewWindowBuilder, AppHandle, Emitter, Manager, WebviewWindow, WindowEvent};

use crate::dispatch::PendingAction;
use crate::events::ui_log;
use crate::winutil;

static CREATE_PENDING: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
/// Что показать: окно создаётся по требованию, вопрос уходит после `confirm_ready`.
static PENDING: Mutex<Option<Value>> = Mutex::new(None);
/// Номер вопроса: клик по старой плашке не ответит на новый вопрос.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// Ответ кликом: (номер вопроса, да/нет).
static ANSWER: Mutex<Option<(u64, bool)>> = Mutex::new(None);

/// Показать вопрос; `seconds` — сколько ждать ответа голосом (потом отмена).
/// Возвращает номер вопроса для `take_answer`.
pub fn show(app: &AppHandle, pending: &PendingAction, seconds: u32) -> u64 {
    let session = SESSION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(mut answer) = ANSWER.lock() {
        *answer = None;
    }
    let (kind, target) = match pending {
        PendingAction::System(op) => (op.as_str(), String::new()),
        PendingAction::Close(target) => ("close", crate::dispatch::app_title(target)),
        PendingAction::Kill(target) => ("kill", crate::dispatch::app_title(target)),
    };
    if let Ok(mut slot) = PENDING.lock() {
        *slot = Some(json!({ "session": session, "kind": kind, "app": target, "seconds": seconds }));
    }
    ensure_window(app);
    session
}

/// Ответ кликом на вопрос `session`, если он был (забирается один раз).
pub fn take_answer(session: u64) -> Option<bool> {
    let mut answer = ANSWER.lock().ok()?;
    match *answer {
        Some((answered, yes)) if answered == session => {
            *answer = None;
            Some(yes)
        }
        _ => None,
    }
}

/// Убрать плашку: ответ получен голосом, время вышло или ожидание прервано.
pub fn hide(app: &AppHandle) {
    if let Ok(mut slot) = PENDING.lock() {
        *slot = None;
    }
    READY.store(false, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("confirm") {
        let _ = window.destroy();
    }
}

fn answer(app: &AppHandle, session: u64, yes: bool) {
    if session == SESSION.load(Ordering::SeqCst) {
        if let Ok(mut answer) = ANSWER.lock() {
            *answer = Some((session, yes));
        }
    }
    hide(app);
}

fn ensure_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("confirm") {
        if READY.load(Ordering::SeqCst) {
            publish(app, &window);
        }
        return;
    }
    if CREATE_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    READY.store(false, Ordering::SeqCst);
    let Some(window_config) = app.config().app.windows.iter().find(|window| window.label == "confirm").cloned() else {
        CREATE_PENDING.store(false, Ordering::SeqCst);
        ui_log(app, "⚠ confirm window config not found");
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let window = match WebviewWindowBuilder::from_config(&app, &window_config).and_then(|builder| builder.build()) {
            Ok(window) => window,
            Err(error) => {
                CREATE_PENDING.store(false, Ordering::SeqCst);
                ui_log(&app, format!("⚠ confirm window was not created: {error}"));
                return;
            }
        };
        let handle = app.clone();
        window.on_window_event(move |event| match event {
            // Alt+F4 по плашке — то же, что «Нет».
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                answer(&handle, SESSION.load(Ordering::SeqCst), false);
            }
            WindowEvent::Destroyed => {
                READY.store(false, Ordering::SeqCst);
                CREATE_PENDING.store(false, Ordering::SeqCst);
                if PENDING.lock().map(|pending| pending.is_some()).unwrap_or(false) {
                    ensure_window(&handle);
                }
            }
            _ => {}
        });
        CREATE_PENDING.store(false, Ordering::SeqCst);
        // Ответ мог прийти голосом, пока WebView создавался.
        if PENDING.lock().map(|pending| pending.is_none()).unwrap_or(true) {
            let _ = window.destroy();
        }
    });
}

fn publish(app: &AppHandle, window: &WebviewWindow) {
    let Some(payload) = PENDING.lock().ok().and_then(|pending| pending.clone()) else {
        let _ = window.destroy();
        return;
    };
    let _ = app.emit_to("confirm", "reever://confirm", payload);
    let _ = window.center();
    let _ = window.show();
    let _ = window.set_focus();
    // Вопрос пришёл голосом, без клика — Windows не даёт фокус фоновой программе.
    if let Ok(hwnd) = window.hwnd() {
        winutil::bring_to_front_raw(hwnd.0 as isize);
    }
}

/// confirm.html загрузилась и подписалась на вопрос.
#[tauri::command]
pub fn confirm_ready(app: AppHandle) {
    let Some(window) = app.get_webview_window("confirm") else { return };
    READY.store(true, Ordering::SeqCst);
    publish(&app, &window);
}

/// Клик «Да» / «Нет» (или Esc) на плашке вопроса `session`.
#[tauri::command]
pub fn confirm_answer(app: AppHandle, session: u64, yes: bool) {
    answer(&app, session, yes);
}
