//! Каналы в интерфейс: лог и состояние. Общие для аудио-воркера и диспетчера.

use tauri::{AppHandle, Emitter};

pub fn ui_log(app: &AppHandle, msg: impl AsRef<str>) {
    let m = msg.as_ref();
    // stdout — только в dev; в релизе ничего не печатаем (нет утечки в консоль).
    #[cfg(debug_assertions)]
    println!("[reever] {m}");
    let _ = app.emit("reever://log", m.to_string());
}

pub fn set_state(app: &AppHandle, s: &str) {
    let _ = app.emit("reever://state", s);
}
