//! Меню значка в трее в стиле программы (frontend/traymenu.html) вместо системного меню
//! Windows, которое не перекрасить. Окно `traymenu` создаётся по правому клику у курсора
//! и уничтожается после выбора, Esc или щелчка мимо: в покое ни WebView, ни памяти.
//!
//! Закрытие по щелчку мимо — опросом мыши, а не по потере фокуса: окно, созданное из
//! фонового процесса, Windows то отдаёт фокус, то забирает (при создании и через долю
//! секунды после показа), и меню по `Focused(false)` закрывалось, не успев появиться.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{
    webview::WebviewWindowBuilder, AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow,
    WindowEvent,
};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::events::ui_log;
use crate::actions::SharedState;
use crate::{uilang, winutil};

static CREATE_PENDING: AtomicBool = AtomicBool::new(false);
/// Идёт ли опрос мыши (`watch_outside_clicks`) — один на открытое меню.
static WATCHING: AtomicBool = AtomicBool::new(false);
/// Где кликнули по значку, физические пиксели.
static CURSOR: Mutex<Option<PhysicalPosition<f64>>> = Mutex::new(None);

/// Показать меню у курсора (правый клик по значку).
pub fn show(app: &AppHandle, cursor: PhysicalPosition<f64>) {
    if let Ok(mut slot) = CURSOR.lock() {
        *slot = Some(cursor);
    }
    if let Some(window) = app.get_webview_window("traymenu") {
        publish(app, &window);
        return;
    }
    if CREATE_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(config) = app.config().app.windows.iter().find(|window| window.label == "traymenu").cloned() else {
        CREATE_PENDING.store(false, Ordering::SeqCst);
        ui_log(app, "⚠ tray menu window config not found");
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let window = match WebviewWindowBuilder::from_config(&app, &config).and_then(|builder| builder.build()) {
            Ok(window) => window,
            Err(error) => {
                CREATE_PENDING.store(false, Ordering::SeqCst);
                ui_log(&app, format!("⚠ tray menu window was not created: {error}"));
                return;
            }
        };
        window.on_window_event(move |event| {
            if let WindowEvent::Destroyed = event {
                CREATE_PENDING.store(false, Ordering::SeqCst);
            }
        });
        CREATE_PENDING.store(false, Ordering::SeqCst);
    });
}

fn close(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("traymenu") {
        let _ = window.destroy();
    }
}

fn publish(app: &AppHandle, window: &WebviewWindow) {
    let locale = *uilang::CURRENT.lock().unwrap();
    let paused = app
        .try_state::<Arc<SharedState>>()
        .is_some_and(|state| state.paused.load(Ordering::Relaxed));
    let _ = app.emit_to(
        "traymenu",
        "reever://traymenu",
        json!({
            "labels": {
                "show": uilang::text(locale, "tray.show"),
                "pause": uilang::text(locale, "tray.pause"),
                "quit": uilang::text(locale, "tray.quit"),
            },
            "paused": paused,
        }),
    );
    place(app, window);
    let _ = window.show();
    watch_outside_clicks(app);
    let _ = window.set_focus();
    if let Ok(hwnd) = window.hwnd() {
        winutil::bring_to_front_raw(hwnd.0 as isize);
    }
}

/// Пока меню открыто — раз в 30 мс смотреть кнопки мыши: нажатие вне меню закрывает его.
fn watch_outside_clicks(app: &AppHandle) {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let pressed = || {
            [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
                .iter()
                .any(|key| unsafe { GetAsyncKeyState(key.0 as i32) } as u16 & 0x8000 != 0)
        };
        let mut was_pressed = pressed();
        while let Some(window) = app.get_webview_window("traymenu") {
            std::thread::sleep(Duration::from_millis(30));
            let now = pressed();
            if now && !was_pressed && !cursor_inside(&window) {
                let _ = window.destroy();
                break;
            }
            was_pressed = now;
        }
        WATCHING.store(false, Ordering::SeqCst);
    });
}

fn cursor_inside(window: &WebviewWindow) -> bool {
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return true;
    }
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return true;
    };
    point.x >= position.x
        && point.y >= position.y
        && point.x < position.x + size.width as i32
        && point.y < position.y + size.height as i32
}

/// Как у системного меню: правым нижним углом к курсору (панель задач внизу), в пределах
/// рабочей области монитора — у панели сверху или сбоку меню само сдвинется.
fn place(app: &AppHandle, window: &WebviewWindow) {
    let Some(cursor) = CURSOR.lock().ok().and_then(|slot| *slot) else { return };
    let Ok(size) = window.outer_size() else { return };
    let (width, height) = (size.width as i32, size.height as i32);
    let (x, y) = (cursor.x as i32, cursor.y as i32);
    let (mut left, mut top) = (x - width, y - height);
    if let Ok(Some(monitor)) = app.monitor_from_point(cursor.x, cursor.y) {
        let area = monitor.work_area();
        let (area_left, area_top) = (area.position.x, area.position.y);
        let (area_right, area_bottom) = (area_left + area.size.width as i32, area_top + area.size.height as i32);
        if top < area_top {
            top = y;
        }
        if left < area_left {
            left = x;
        }
        left = left.clamp(area_left, (area_right - width).max(area_left));
        top = top.clamp(area_top, (area_bottom - height).max(area_top));
    }
    let _ = window.set_position(PhysicalPosition::new(left, top));
}

/// traymenu.html загрузилась и подписалась на подписи пунктов.
#[tauri::command]
pub fn traymenu_ready(app: AppHandle) {
    if let Some(window) = app.get_webview_window("traymenu") {
        publish(&app, &window);
    }
}

/// Пункт меню: `show`, `pause`, `quit` или `close` (Esc).
#[tauri::command]
pub fn traymenu_action(app: AppHandle, action: String) {
    close(&app);
    match action.as_str() {
        "show" => crate::show_main(&app),
        "pause" => crate::toggle_pause(&app),
        "quit" => crate::quit(&app),
        _ => {}
    }
}
