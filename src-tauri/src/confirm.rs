//! Плашка подтверждения опасной команды («ривер выключи компьютер», «закрой …») при
//! включённом «спрашивать» (`confirm_dangerous`): нативная плашка в центре экрана
//! (dialogs_native/confirm.rs) с кнопками «Да» / «Нет». Голосовой ответ работает как раньше;
//! клик кладёт ответ сюда, аудио-поток забирает его (`take_answer`) и перестаёт ждать голос.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use crate::app::AppHandle;
use crate::dispatch::PendingAction;
/// Номер вопроса: клик по старой плашке не ответит на новый вопрос.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// Ответ кликом: (номер вопроса, да/нет).
static ANSWER: Mutex<Option<(u64, bool)>> = Mutex::new(None);

/// Показать вопрос; `seconds` — сколько ждать ответа голосом (потом отмена).
/// Возвращает номер вопроса для `take_answer`.
pub fn show(app: &AppHandle, pending: &PendingAction, seconds: u32) -> u64 {
    crate::dialog_demo::asked("confirm");
    let session = SESSION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(mut answer) = ANSWER.lock() {
        *answer = None;
    }
    let (kind, target) = match pending {
        PendingAction::System(op) => (op.as_str(), String::new()),
        PendingAction::Close(target) => ("close", crate::dispatch::app_title(target)),
        PendingAction::Kill(target) => ("kill", crate::dispatch::app_title(target)),
    };
    let q = crate::dialogs_native::confirm::Question { session, kind: kind.to_string(), app: target, seconds };
    crate::dialogs_native::confirm::show(app, std::sync::Arc::new(NativeHost { app: app.clone() }), q);
    session
}

/// Ответ кнопкой плашки (или Esc — «нет»).
struct NativeHost {
    app: AppHandle,
}

impl crate::dialogs_native::confirm::Host for NativeHost {
    fn answer(&self, session: u64, yes: bool) {
        answer(&self.app, session, yes);
    }
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
    crate::dialogs_native::confirm::hide(app);
}

fn answer(app: &AppHandle, session: u64, yes: bool) {
    if session == SESSION.load(Ordering::SeqCst) {
        if let Ok(mut answer) = ANSWER.lock() {
            *answer = Some((session, yes));
        }
    }
    hide(app);
}
