//! Каналы в интерфейс: лог и состояние. Общие для аудио-воркера и диспетчера.

use std::{io::Write, path::PathBuf, sync::Mutex};

use tauri::{AppHandle, Emitter};

/// Журнал в файл (`reever.log` в папке настроек) — чтобы разбирать сбои по факту, а не
/// по скриншотам. Пишется то же, что видно в UI: при приватном режиме текст команд туда
/// не попадает и так. Больше `LOG_LIMIT` — старый журнал уходит в `reever.old.log`.
static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
const LOG_LIMIT: u64 = 1024 * 1024;

pub fn init_log_file(path: PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = LOG_FILE.lock() {
        *file = Some(path);
    }
}

fn append_log(message: &str) {
    let Ok(guard) = LOG_FILE.lock() else { return };
    let Some(path) = guard.as_ref() else { return };
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
        let _ = std::fs::rename(path, path.with_extension("old.log"));
    }
    let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {message}",
            time.wYear,
            time.wMonth,
            time.wDay,
            time.wHour,
            time.wMinute,
            time.wSecond,
            time.wMilliseconds
        );
    }
}

pub fn ui_log(app: &AppHandle, msg: impl AsRef<str>) {
    let m = msg.as_ref();
    // stdout — только в dev; в релизе ничего не печатаем (нет утечки в консоль).
    #[cfg(debug_assertions)]
    println!("[reever] {m}");
    append_log(m);
    let _ = app.emit("reever://log", m.to_string());
}

pub fn set_state(app: &AppHandle, s: &str) {
    let _ = app.emit("reever://state", s);
}
