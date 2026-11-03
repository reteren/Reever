//! Пузырь над маскотом: терминал без рамки, куда по ходу речи пишется сказанное
//! (bubble_native). Настройка `mascot_bubble`; работает, пока маскот на экране.
//!
//! Окно, как и окно маскота, создаётся один раз и больше не прячется: показ и скрытие
//! прозрачного окна без рамки Windows сопровождает кадром со стандартной белой рамкой.
//! Пустой пузырь — пустой прозрачный холст, а мышь окно пропускает сквозь себя всегда.
//! Окно не масштабируется вместе с маскотом и ходит за ним: хвостик пузыря смотрит
//! на голову осьминога.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::app::AppHandle;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
};

use crate::bubble_native::{self, Msg};
use crate::mascot;

/// Настройка включена.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Где хвостик пузыря по горизонтали, в десятых логического пикселя окна пузыря.
static ARROW_X: AtomicI32 = AtomicI32::new(1700);
/// Голова осьминога в покое — на столько логических пикселей (при 100 %) выше центра
/// его видимой части; хвостик пузыря касается её чуть сверху.
const HEAD_ABOVE_CENTER: f64 = 64.0;
const TIP_GAP: f64 = 2.0;

/// Окно пузыря (bubble_native).
struct Win(isize);

impl Win {
    fn destroy(&self) {
        bubble_native::destroy();
    }

    /// (ширина, высота) в физических пикселях.
    fn outer_size(&self) -> Option<(f64, f64)> {
        bubble_native::rect().map(|(_, _, w, h)| (w as f64, h as f64))
    }

    fn set_position(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(HWND(self.0 as *mut _), None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    fn scale_factor(&self) -> Option<f64> {
        Some(unsafe { GetDpiForWindow(HWND(self.0 as *mut _)) }.max(96) as f64 / 96.0)
    }

    fn hwnd(&self) -> Option<HWND> {
        Some(HWND(self.0 as *mut _))
    }
}

fn win(_app: &AppHandle) -> Option<Win> {
    bubble_native::hwnd().map(Win)
}

/// Настройка включена — или маскот как раз представляется (`mascot::mascot_intro`).
fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) || mascot::intro_active()
}

pub fn configure(app: &AppHandle, on: bool) {
    // подписка нативного пузыря на события — сразу при старте (mascot::spawn), см. init
    bubble_native::init(app);
    ENABLED.store(on, Ordering::Relaxed);
    sync(app);
}

/// Окно пузыря есть, только пока он может понадобиться: настройка включена и маскот
/// вообще бывает на экране (не режим `never`).
pub fn sync(app: &AppHandle) {
    let want = enabled() && mascot::mode() != "never";
    match (want, win(app)) {
        (true, None) => create(app),
        (false, Some(window)) => window.destroy(),
        (true, Some(_)) => notify(app),
        (false, None) => {}
    }
}

/// Пузырь заново узнаёт, можно ли ему показываться и где хвостик.
pub fn notify(app: &AppHandle) {
    if win(app).is_some() {
        let (allowed, ax) = bubble_layout();
        bubble_native::send(Msg::Layout { allowed, ax: ax as f32 });
        // реплики знакомства маскота
        let lines = mascot::bubble_intro();
        if !lines.is_empty() {
            bubble_native::send(Msg::Intro(lines));
        }
    }
}

/// Поднять пузырь наверх z-порядка (над маскотом).
pub(crate) fn raise(app: &AppHandle) {
    if let Some(hwnd) = win(app).and_then(|w| w.hwnd()) {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        let _ = unsafe { SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags) };
    }
}

/// Можно ли сейчас показывать пузырь и где хвостик (логические px окна).
pub fn bubble_layout() -> (bool, f64) {
    (
        enabled() && mascot::on_screen(),
        ARROW_X.load(Ordering::Relaxed) as f64 / 10.0,
    )
}

/// Поставить пузырь над головой маскота (в пределах рабочей области его монитора).
pub fn place(app: &AppHandle) {
    let (Some(bubble), Some((position, size, scale_factor, monitor))) = (win(app), mascot::geometry(app)) else {
        return;
    };
    let Some((bw, bh)) = bubble.outer_size() else {
        return;
    };
    let k = mascot::scale() as f64 / 100.0;
    let cx = position.x as f64 + size.width as f64 / 2.0;
    let cy = position.y as f64 + size.height as f64 / 2.0;
    let tip = cy - (HEAD_ABOVE_CENTER * k + TIP_GAP) * scale_factor;
    let mut x = cx - bw / 2.0;
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let (left, right) = (area.position.x as f64, area.position.x as f64 + area.size.width as f64);
        x = x.min(right - bw).max(left);
    }
    bubble.set_position(x.round() as i32, (tip - bh).round() as i32);
    let arrow = (cx - x) / bubble.scale_factor().unwrap_or(scale_factor);
    if ARROW_X.swap((arrow * 10.0).round() as i32, Ordering::Relaxed) != (arrow * 10.0).round() as i32 {
        notify(app);
    }
}

/// Нативный пузырь уже создаётся (окна ещё нет, а sync могут позвать снова).
static CREATING: AtomicBool = AtomicBool::new(false);

fn create(app: &AppHandle) {
    if CREATING.swap(true, Ordering::SeqCst) {
        return;
    }
    // окно — в своём потоке: поток окон мог ещё не прогреться
    let app = app.clone();
    std::thread::spawn(move || {
        // у маскота — чтобы окно сразу получило DPI его монитора
        let (x, y) = mascot::geometry(&app).map_or((0, 0), |(p, _, _, _)| (p.x, p.y));
        let made = bubble_native::create(&app, x, y);
        CREATING.store(false, Ordering::SeqCst);
        if made.is_none() {
            return;
        }
        place(&app);
        notify(&app);
    });
}
