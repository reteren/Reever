//! Пузырь над маскотом: терминал без рамки, куда по ходу речи пишется сказанное
//! (frontend/bubble.html). Настройка `mascot_bubble`; работает, пока маскот на экране.
//!
//! Окно, как и окно маскота, создаётся один раз и больше не прячется: показ и скрытие
//! прозрачного окна без рамки Windows сопровождает кадром со стандартной белой рамкой.
//! Пустой пузырь — пустой прозрачный холст, а мышь окно пропускает сквозь себя всегда.
//! Окно не масштабируется вместе с маскотом и ходит за ним: хвостик пузыря смотрит
//! на голову осьминога.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use tauri::{webview::WebviewWindowBuilder, AppHandle, Emitter, Manager, PhysicalPosition};

use crate::events::ui_log;
use crate::mascot;

/// Настройка включена.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Где хвостик пузыря по горизонтали, в десятых логического пикселя окна пузыря.
static ARROW_X: AtomicI32 = AtomicI32::new(1700);
/// Голова осьминога в покое — на столько логических пикселей (при 100 %) выше центра
/// его видимой части; хвостик пузыря касается её чуть сверху.
const HEAD_ABOVE_CENTER: f64 = 64.0;
const TIP_GAP: f64 = 2.0;

/// Настройка включена — или маскот как раз представляется (`mascot::mascot_intro`).
fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) || mascot::intro_active()
}

pub fn configure(app: &AppHandle, on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    sync(app);
}

/// Окно пузыря есть, только пока он может понадобиться: настройка включена и маскот
/// вообще бывает на экране (не режим `never`).
pub fn sync(app: &AppHandle) {
    let want = enabled() && mascot::mode() != "never";
    match (want, app.get_webview_window("bubble")) {
        (true, None) => create(app),
        (false, Some(window)) => {
            let _ = window.destroy();
        }
        (true, Some(_)) => notify(app),
        (false, None) => {}
    }
}

/// Пузырь заново спрашивает, можно ли ему показываться и где хвостик.
pub fn notify(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("bubble") {
        let _ = window.emit("bubble://layout", ());
    }
}

/// Для bubble.html: можно ли сейчас показывать пузырь и где хвостик (логические px окна).
#[tauri::command]
pub fn bubble_layout() -> (bool, f64) {
    (
        enabled() && mascot::on_screen(),
        ARROW_X.load(Ordering::Relaxed) as f64 / 10.0,
    )
}

/// Поставить пузырь над головой маскота (в пределах рабочей области его монитора).
pub fn place(app: &AppHandle) {
    // окно маскота — веб или нативное (mascot::geometry)
    let (Some(bubble), Some((position, size, scale_factor, monitor))) = (app.get_webview_window("bubble"), mascot::geometry(app)) else {
        return;
    };
    let Ok(bubble_size) = bubble.outer_size() else {
        return;
    };
    let k = mascot::scale() as f64 / 100.0;
    let cx = position.x as f64 + size.width as f64 / 2.0;
    let cy = position.y as f64 + size.height as f64 / 2.0;
    let tip = cy - (HEAD_ABOVE_CENTER * k + TIP_GAP) * scale_factor;
    let (bw, bh) = (bubble_size.width as f64, bubble_size.height as f64);
    let mut x = cx - bw / 2.0;
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let (left, right) = (area.position.x as f64, area.position.x as f64 + area.size.width as f64);
        x = x.min(right - bw).max(left);
    }
    let _ = bubble.set_position(PhysicalPosition::new(x.round() as i32, (tip - bh).round() as i32));
    let arrow = (cx - x) / bubble.scale_factor().unwrap_or(scale_factor);
    if ARROW_X.swap((arrow * 10.0).round() as i32, Ordering::Relaxed) != (arrow * 10.0).round() as i32 {
        notify(app);
    }
}

fn create(app: &AppHandle) {
    let Some(window_config) = app.config().app.windows.iter().find(|window| window.label == "bubble").cloned() else {
        ui_log(app, crate::msg!("log.window.noConfig", window = "bubble"));
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // за экраном: первый показ окна без рамки мелькает стандартной рамкой там, где не видно
        let built = WebviewWindowBuilder::from_config(&app, &window_config)
            .map(|builder| builder.position(-30000.0, -30000.0))
            .and_then(|builder| builder.build());
        let window = match built {
            Ok(window) => window,
            Err(error) => {
                ui_log(&app, crate::msg!("log.window.failed", window = "bubble", error = error));
                return;
            }
        };
        let _ = window.set_ignore_cursor_events(true);
        let _ = window.show();
        place(&app);
        notify(&app);
    });
}
