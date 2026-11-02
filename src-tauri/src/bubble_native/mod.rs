//! Нативный пузырь над маскотом (этап 2 docs/native-ui-roadmap.md): то же, что
//! frontend/bubble.html, на ядре `native/` в общем с маскотом потоке окон. Включается вместе с
//! нативным маскотом (`native_mascot`): маскот и пузырь — одно целое, и только вместе они
//! убирают WebView2 из трея. Показ, место и хвостик по-прежнему решает bubble.rs.
//!
//! События распознавания (reever://state, partial, said, heard) пузырь слушает сам через
//! `AppHandle::listen` — как веб-окно, без отдельных вызовов из распознавания.

mod view;

use std::sync::{Mutex, Once, OnceLock};

use tauri::{AppHandle, Listener};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use crate::native::{self, WindowId, WindowSpec};
pub use view::Msg;
use view::{BubbleView, WIN_H, WIN_W};

static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);
static LISTEN: Once = Once::new();
static APP: OnceLock<AppHandle> = OnceLock::new();

fn listen(app: &AppHandle) {
    LISTEN.call_once(|| {
        let text = |payload: &str| serde_json::from_str::<String>(payload).unwrap_or_default();
        app.listen_any("reever://state", move |e| send(Msg::State(text(e.payload()))));
        app.listen_any("reever://partial", move |e| send(Msg::Partial(text(e.payload()))));
        app.listen_any("reever://said", move |e| send(Msg::Said(text(e.payload()))));
        app.listen_any("reever://heard", move |e| send(Msg::Heard(text(e.payload()))));
    });
}

/// Создать окно пузыря (блокирует до готовности). `x`, `y` — экранные пиксели.
pub fn create(app: &AppHandle, x: i32, y: i32) -> Option<isize> {
    let _ = APP.set(app.clone());
    listen(app);
    let ui = crate::mascot_native::ui(app)?;
    let locale = crate::uilang::current().to_string();
    let state = crate::events::current_state();
    let made = ui.call(move |ui| {
        let done: Box<dyn Fn()> = Box::new(|| {
            // знакомство договорено — как invoke('mascot_intro_done'); не из потока окон
            if let Some(app) = APP.get().cloned() {
                std::thread::spawn(move || crate::mascot::mascot_intro_done(app));
            }
        });
        let mut view = BubbleView::new(&locale, done);
        view.on_msg(Msg::State(state));
        let spec = WindowSpec {
            title: "Reever".into(),
            x,
            y,
            width: WIN_W,
            height: WIN_H,
            topmost: true,
            no_activate: true,
            visible: true,
        };
        let id = ui.create_window(spec, Box::new(view))?;
        let hwnd = ui.info(id).map(|i| i.hwnd).unwrap_or(0);
        Ok::<_, windows::core::Error>((id, hwnd))
    })?;
    match made {
        Ok((id, hwnd)) => {
            // мышь пузырь не ловит никогда — как set_ignore_cursor_events(true) у веб-окна
            native::set_click_through(hwnd, true);
            if let Ok(mut w) = WIN.lock() {
                *w = Some((id, hwnd));
            }
            Some(hwnd)
        }
        Err(e) => {
            crate::events::ui_log(app, format!("bubble: native window failed: {e}"));
            None
        }
    }
}

pub fn destroy() {
    let Some((id, _)) = WIN.lock().ok().and_then(|mut w| w.take()) else { return };
    if let Some(app) = APP.get() {
        if let Some(ui) = crate::mascot_native::ui(app) {
            ui.post(move |ui| {
                ui.close_window(id);
                ui.release_device();
            });
        }
    }
}

pub fn hwnd() -> Option<isize> {
    WIN.lock().ok().and_then(|w| w.map(|(_, h)| h))
}

pub fn send(msg: Msg) {
    let Some((id, _)) = WIN.lock().ok().and_then(|w| *w) else { return };
    let Some(app) = APP.get() else { return };
    if let Some(ui) = crate::mascot_native::ui(app) {
        ui.post(move |ui| {
            if let Some(view) = ui.handler_mut::<BubbleView>(id) {
                view.on_msg(msg);
            }
            ui.invalidate(id);
        });
    }
}

/// Окно на экране: (x, y, ширина, высота) в физических пикселях.
pub fn rect() -> Option<(i32, i32, u32, u32)> {
    let hwnd = hwnd()?;
    let mut r = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut r) }.ok()?;
    Some((r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32))
}
