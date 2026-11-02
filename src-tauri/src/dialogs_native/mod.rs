//! Нативные окна выбора и подтверждения (этап 3 docs/native-ui-roadmap.md): то же, что
//! frontend/picker.html и confirm.html, на ядре `native/` и его элементах управления. Включаются
//! настройкой `native_dialogs` (в лёгком интерфейсе пока остаются веб-окна). Логику — когда
//! показывать, что делать с выбором и ответом — по-прежнему ведут picker.rs и confirm.rs.
//!
//! Общая обвязка обоих окон — хромовая рамка `skin/picker_frame.webp` поверх двух экранов ЭЛТ
//! (табло заголовка и основной экран) и купол закрытия; окно перетаскивается за любое пустое место.

pub mod confirm;
pub mod picker;

use std::sync::Arc;

use tauri::AppHandle;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::native::controls::button::{Lunka, Press};
use crate::native::controls::{lcd, lcd_background, lcd_overlay, Glow, Scene, ACC, ACC_HOT};
use crate::native::{Align, Direction, ImageId, Images, Loader, Rect, RenderCx, TextStyle};

/// Окно 620×520 DIP — как у веб-окон picker/confirm (tauri.conf.json).
pub const WIN_W: f32 = 620.0;
pub const WIN_H: f32 = 520.0;
pub const TITLE: Rect = Rect::new(40.0, 38.0, 342.0, 38.0);
pub const SCREEN: Rect = Rect::new(40.0, 88.0, 540.0, 394.0);

/// Картинки рамки и купола.
#[derive(Clone, Copy, Debug)]
pub struct Skin {
    pub frame: ImageId,
    pub close_off: ImageId,
    pub close_on: ImageId,
}

pub fn loader(app: &AppHandle, path: &str) -> Loader {
    let app = app.clone();
    let path = path.to_string();
    Arc::new(move || app.asset_resolver().get(path.clone()).map(|a| a.bytes))
}

pub fn load_skin(app: &AppHandle, images: &mut Images) -> Skin {
    Skin {
        frame: images.add_loader(loader(app, "skin/picker_frame.webp")),
        close_off: images.add_loader(loader(app, "skin/btn_close_off.webp")),
        close_on: images.add_loader(loader(app, "skin/btn_close_on.webp")),
    }
}

/// Лёгкий вид (lite_ui): стальная пластина вместо хромового обода (lite.css). Ставится, когда
/// окна включаются (`enabled`).
static LITE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn lite() -> bool {
    LITE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Купол закрытия в (568, 57), --d: 30px; в лёгком виде — клавиша 36×32 в (540, 41).
pub fn close_button(skin: &Skin, icon: &'static str, tip: &str) -> Lunka {
    let (center, box_size) = if lite() { ((558.0, 57.0), 36.0) } else { ((568.0, 57.0), 44.0) };
    Lunka { center, box_size, dome: 30.0, icon, off: skin.close_off, on: skin.close_on, press: Press::default(), tip: tip.to_string() }
}

/// Кнопка закрытия: купол или клавиша лёгкого вида.
pub fn draw_close(scene: &mut Scene, cx: &mut RenderCx<'_>, l: &Lunka) {
    if lite() {
        let h = l.press.hover_t.value(cx.now);
        crate::native::controls::lite::key(scene, cx, Rect::new(540.0, 41.0, 36.0, 32.0), l.icon, h, l.press.pressed && l.press.hover);
    } else {
        l.draw(scene, cx);
    }
}

/// Пластина лёгкого вида под экранами (#pk::before, #cf::before) — рисовать первой.
pub fn draw_plate(cx: &mut RenderCx<'_>) {
    if lite() {
        crate::native::controls::lite::metal_panel(cx, Rect::new(24.0, 22.0, WIN_W - 48.0, WIN_H - 44.0), 18.0);
    }
}

/// Табло заголовка: в лёгком виде шире (#pkTitle { width: 488px }).
fn title_rect() -> Rect {
    if lite() { Rect::new(TITLE.x, TITLE.y, 488.0, TITLE.h) } else { TITLE }
}

/// Язык интерфейса сейчас и направление письма.
pub fn locale() -> (&'static str, bool) {
    let l = crate::uilang::current();
    (l, crate::ui_text::direction(l) == "rtl")
}

pub fn t(key: &str) -> String {
    crate::ui_text::t(crate::uilang::current(), key, &[])
}

pub fn tp(key: &str, params: &[(&str, &str)]) -> String {
    crate::ui_text::t(crate::uilang::current(), key, params)
}

/// Направление строки интерфейса: в арабском — справа налево.
pub fn dir(rtl: bool) -> Direction {
    if rtl { Direction::Rtl } else { Direction::Ltr }
}

/// Табло заголовка (#pkTitle/#cfTitle): значок 14px, текст 21px, обрезка многоточием.
pub fn draw_title(scene: &mut Scene, cx: &mut RenderCx<'_>, icon: &'static str, filled: bool, text: &str, rtl: bool) {
    let r = title_rect();
    lcd_background(cx, r, 11.0);
    let inner = Rect::new(r.x + 14.0, r.y, r.w - 28.0, r.h);
    let icon_r = crate::native::controls::mirror(inner, Rect::new(inner.x, r.y + (r.h - 14.0) / 2.0, 14.0, 14.0), rtl);
    scene.icon(icon, icon_r, 2.0, ACC_HOT, filled, Glow::G2);
    let style = TextStyle { ellipsis: true, direction: dir(rtl), align: Align::Start, ..lcd(21.0, 500, 0.05) };
    let tw = inner.w - 14.0 - 8.0;
    let tx = if rtl { inner.x } else { inner.x + 22.0 };
    if let Some(l) = scene.cache.get(cx.text(), &text.to_uppercase(), &style, tw) {
        scene.text(&l, tx, r.y + (r.h - l.height) / 2.0, ACC_HOT, Glow::G3);
    }
    scene.flush(cx);
    lcd_overlay(cx, r, 11.0);
    let _ = ACC;
}

/// Хромовая рамка поверх экранов (z-index 3); в лёгком виде — гнёзда экранов в металле.
pub fn draw_frame(cx: &mut RenderCx<'_>, skin: &Skin) {
    if lite() {
        crate::native::controls::lite::recess(cx, title_rect(), 11.0, false);
        crate::native::controls::lite::recess(cx, SCREEN, 13.0, false);
        return;
    }
    cx.draw_image(skin.frame, Rect::new(0.0, 0.0, WIN_W, WIN_H), 1.0);
}

/// Левый верхний угол окна по центру рабочей области основного монитора (как window.center()
/// у Tauri: без панели задач).
pub fn centered_position() -> (i32, i32) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let k = dx as f32 / 96.0;
        let r = info.rcWork;
        let (w, h) = ((WIN_W * k).round() as i32, (WIN_H * k).round() as i32);
        (r.left + (r.right - r.left - w) / 2, r.top + (r.bottom - r.top - h) / 2)
    }
}

/// Включены ли нативные окна (настройка `native_dialogs`; в лёгком интерфейсе — веб).
pub fn enabled(app: &AppHandle) -> bool {
    use tauri::Manager;
    app.try_state::<crate::config::LiveConfig>().is_some_and(|c| {
        let s = &c.get().settings;
        LITE.store(s.lite_ui, std::sync::atomic::Ordering::Relaxed);
        s.native_dialogs
    })
}

/// WM_DPICHANGED с прямоугольником под новый масштаб (центр окна на месте).
pub fn send_dpi(hwnd: isize, dpi: u32) {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SendMessageW, WM_DPICHANGED};
    unsafe {
        let h = HWND(hwnd as *mut _);
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        let (w, hh) = ((WIN_W * dpi as f32 / 96.0).round() as i32, (WIN_H * dpi as f32 / 96.0).round() as i32);
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let s = RECT { left: cx - w / 2, top: cy - hh / 2, right: cx - w / 2 + w, bottom: cy - hh / 2 + hh };
        SendMessageW(h, WM_DPICHANGED, WPARAM((dpi | (dpi << 16)) as usize), LPARAM(&s as *const RECT as isize));
    }
}

/// Настройки → Интерфейс: нативные окна выбора и подтверждения вкл/выкл — сразу: следующее
/// окно покажется уже в новом виде (открытое сейчас доживает в прежнем).
#[tauri::command]
pub fn set_native_dialogs(config: tauri::State<'_, crate::config::LiveConfig>, enabled: bool) -> Result<(), String> {
    let current = config.get();
    crate::commands_api::persist_value(&current, "native_dialogs", serde_json::Value::Bool(enabled))?;
    let mut fresh = (*current).clone();
    fresh.settings.native_dialogs = enabled;
    config.set(Arc::new(fresh));
    Ok(())
}

/// Команда началась (слушаю после «ривер» или горячей клавиши): создать графическое
/// устройство заранее, если его нет. Окно выбора открывается только после распознавания
/// команды — к этому времени устройство уже готово, и первое окно не ждёт 0,3 с на D3D11.
/// Неиспользованное устройство отпускается через минуту, как после закрытия окна.
pub fn warm(app: &AppHandle) {
    if !enabled(app) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(ui) = crate::mascot_native::ui(&app) else { return };
        let fresh = ui.call(|ui| {
            let fresh = !ui.has_device();
            fresh && ui.prewarm().is_ok()
        });
        if fresh.unwrap_or(false) {
            release_later(ui);
        }
    });
}

/// Отпустить графическое устройство через минуту, если к тому времени окон не останется.
pub fn release_later(ui: Arc<crate::native::UiThread>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        ui.post(|ui| ui.release_device());
    });
}
