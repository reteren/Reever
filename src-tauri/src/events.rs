//! Каналы в интерфейс: лог и состояние. Общие для аудио-воркера и диспетчера.

use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    sync::{atomic::{AtomicBool, Ordering}, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

/// Журнал в файл (`reever.log` в папке настроек) — чтобы разбирать сбои по факту, а не
/// по скриншотам. Пишется то же, что видно в UI: при приватном режиме текст команд туда
/// не попадает и так. Больше `LOG_LIMIT` — старый журнал уходит в `reever.old.log`.
static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
const LOG_LIMIT: u64 = 1024 * 1024;
const UI_LOG_LIMIT: usize = 250;

/// Небольшое состояние процесса переживает уничтожение WebView главного окна.
static UI_HISTORY: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static UI_STATE: Mutex<String> = Mutex::new(String::new());
static UI_TAB: Mutex<String> = Mutex::new(String::new());
static UI_STATUS: Mutex<Option<Value>> = Mutex::new(None);
static UI_COMPACT: AtomicBool = AtomicBool::new(false);
static MAIN_VISIBLE: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
pub struct UiSnapshot {
    state: String,
    log: Vec<String>,
    tab: String,
    status: Option<Value>,
    compact: bool,
}

fn push_ui_log(log: &mut VecDeque<String>, message: &str) {
    while log.len() >= UI_LOG_LIMIT {
        log.pop_front();
    }
    log.push_back(message.to_string());
}

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
    if let Ok(mut log) = UI_HISTORY.lock() {
        push_ui_log(&mut log, m);
    }
    let _ = app.emit("reever://log", m.to_string());
}

pub fn set_state(app: &AppHandle, s: &str) {
    if let Ok(mut state) = UI_STATE.lock() {
        *state = s.to_string();
    }
    let _ = app.emit("reever://state", s);
}

pub fn current_state() -> String {
    UI_STATE
        .lock()
        .map(|state| if state.is_empty() { "idle".into() } else { state.clone() })
        .unwrap_or_else(|_| "idle".into())
}

/// Последний статус распознавателя нужен новому WebView: раннее событие могло уйти до его загрузки.
pub fn remember_ui_status(status: &Value) {
    if let Ok(mut current) = UI_STATUS.lock() {
        *current = Some(status.clone());
    }
}

#[tauri::command]
pub fn cache_ui_status(status: Value) {
    remember_ui_status(&status);
}

pub fn set_main_visible(visible: bool) {
    MAIN_VISIBLE.store(visible, Ordering::Relaxed);
}

pub fn set_compact(compact: bool) {
    UI_COMPACT.store(compact, Ordering::Relaxed);
}

#[tauri::command]
pub fn set_ui_tab(tab: String) -> Result<(), String> {
    if !is_ui_tab(&tab) {
        return Err("неизвестная вкладка интерфейса".into());
    }
    *UI_TAB.lock().map_err(|_| "не удалось сохранить вкладку".to_string())? = tab;
    Ok(())
}

fn is_ui_tab(tab: &str) -> bool {
    matches!(tab, "home" | "actions" | "settings" | "logs")
}

#[tauri::command]
pub fn get_ui_snapshot() -> UiSnapshot {
    UiSnapshot {
        state: current_state(),
        log: UI_HISTORY.lock().map(|log| log.iter().cloned().collect()).unwrap_or_default(),
        tab: UI_TAB
            .lock()
            .map(|tab| if tab.is_empty() { "home".into() } else { tab.clone() })
            .unwrap_or_else(|_| "home".into()),
        status: UI_STATUS.lock().map(|status| status.clone()).unwrap_or(None),
        compact: UI_COMPACT.load(Ordering::Relaxed),
    }
}

/// Уровень микрофона для шара и эквалайзера в окне: RMS 0..1 раз в ~33 мс.
/// Только пока главное окно видно — в трее интерфейс не должен будить ни GPU, ни IPC.
/// Видимость спрашивается раз в полсекунды: это синхронный вызов в поток окна.
pub struct LevelMeter {
    peak: f32,
    last_emit: Instant,
    last_check: Instant,
    visible: bool,
}

impl LevelMeter {
    const EVERY: Duration = Duration::from_millis(33);
    const CHECK: Duration = Duration::from_millis(500);

    pub fn new() -> Self {
        let past = Instant::now() - Self::CHECK;
        Self { peak: 0.0, last_emit: past, last_check: past, visible: false }
    }

    pub fn push(&mut self, app: &AppHandle, pcm: &[i16]) {
        if pcm.is_empty() {
            return;
        }

        // В трее и между окнами не трогаем PCM вообще: событие lifecycle сбрасывает
        // этот флаг синхронно с уничтожением/показом главного окна.
        if !MAIN_VISIBLE.load(Ordering::Relaxed) {
            self.visible = false;
            self.peak = 0.0;
            return;
        }

        // После создания WebView и далее раз в полсекунды уточняем видимость окна.
        // При скрытом окне обнуляем накопленный пик и возвращаемся до расчёта RMS.
        if !self.visible || self.last_check.elapsed() >= Self::CHECK {
            self.last_check = Instant::now();
            self.visible = app
                .get_webview_window("main")
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false);
            if !self.visible {
                MAIN_VISIBLE.store(false, Ordering::Relaxed);
                self.peak = 0.0;
                return;
            }
        }

        let sum: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = ((sum / pcm.len() as f64).sqrt() / 32768.0) as f32;
        self.peak = self.peak.max(rms);
        if self.last_emit.elapsed() < Self::EVERY {
            return;
        }
        let _ = app.emit_to("main", "reever://level", (self.peak * 10000.0).round() / 10000.0);
        self.peak = 0.0;
        self.last_emit = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::{is_ui_tab, push_ui_log, UI_LOG_LIMIT};
    use std::collections::VecDeque;

    #[test]
    fn ui_log_history_keeps_only_the_latest_entries() {
        let mut log = VecDeque::new();
        for i in 0..=UI_LOG_LIMIT {
            push_ui_log(&mut log, &format!("line-{i}"));
        }

        assert_eq!(log.len(), UI_LOG_LIMIT);
        assert_eq!(log.front().map(String::as_str), Some("line-1"));
        assert_eq!(log.back().map(String::as_str), Some("line-250"));
    }

    #[test]
    fn ui_tab_validation_accepts_only_existing_sections() {
        assert!(is_ui_tab("home"));
        assert!(is_ui_tab("actions"));
        assert!(is_ui_tab("settings"));
        assert!(is_ui_tab("logs"));
        assert!(!is_ui_tab("unknown"));
    }
}
