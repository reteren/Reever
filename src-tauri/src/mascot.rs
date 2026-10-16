//! Маскот: хромовый осьминог в правом нижнем углу экрана (окно `mascot`,
//! frontend/mascot.html). Пока тестовый режим — окно есть всегда; за голову его можно
//! взять и перетащить, анимация подхвата и покачивания — пререндер в frontend/mascot/.

use std::time::Duration;

use tauri::{webview::WebviewWindowBuilder, AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::events::ui_log;

/// Отступ от краёв рабочей области, в логических пикселях.
const MARGIN: f64 = 16.0;

pub fn spawn(app: &AppHandle) {
    if app.get_webview_window("mascot").is_some() {
        return;
    }
    let Some(window_config) = app.config().app.windows.iter().find(|window| window.label == "mascot").cloned() else {
        ui_log(app, "⚠ не найден конфиг окна маскота");
        return;
    };

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let window = match WebviewWindowBuilder::from_config(&app, &window_config).and_then(|builder| builder.build()) {
            Ok(window) => window,
            Err(error) => {
                ui_log(&app, format!("⚠ окно маскота не создано: {error}"));
                return;
            }
        };
        // Рабочая область, а не весь монитор: иначе маскот уедет под панель задач.
        if let (Ok(Some(monitor)), Ok(size)) = (window.primary_monitor(), window.outer_size()) {
            let area = monitor.work_area();
            let margin = (MARGIN * monitor.scale_factor()).round() as i32;
            let x = area.position.x + area.size.width as i32 - size.width as i32 - margin;
            let y = area.position.y + area.size.height as i32 - size.height as i32 - margin;
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        let _ = window.show();
    });
}

/// Перетаскивание за голову. Захват указателя в WebView2 теряется, как только окно
/// поехало под курсором, поэтому окно ведёт этот поток по системному курсору, пока
/// зажата левая кнопка, а об отпускании страница узнаёт событием `mascot://drop`.
#[tauri::command]
pub fn mascot_drag(window: WebviewWindow) {
    std::thread::spawn(move || {
        let (Some(start), Ok(origin)) = (cursor(), window.outer_position()) else {
            let _ = window.emit("mascot://drop", ());
            return;
        };
        let mut last = start;
        while left_button_down() {
            if let Some(p) = cursor() {
                if p.x != last.x || p.y != last.y {
                    let _ = window.set_position(PhysicalPosition::new(origin.x + p.x - start.x, origin.y + p.y - start.y));
                    last = p;
                }
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        let _ = window.emit("mascot://drop", ());
    });
}

fn cursor() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok().map(|_| p)
}

fn left_button_down() -> bool {
    (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
}
