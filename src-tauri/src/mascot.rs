//! Маскот: хромовый осьминог в правом нижнем углу экрана (окно `mascot`,
//! frontend/mascot.html). За голову его можно взять и перетащить; анимации — пререндер
//! в frontend/mascot/.
//!
//! Режимы (`mascot_mode` в настройках): `always` — окно видно всегда; `commands` — окно
//! создано заранее, но показывается, только пока Reever слушает и выполняет команду;
//! `never` — окна нет вовсе (WebView2 не держится в памяти). Размер — `mascot_scale`,
//! 100..500 % от исходного.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{
    webview::WebviewWindowBuilder, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewWindow,
};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::config::LiveConfig;
use crate::events::{self, ui_log};

/// Отступ окна от краёв рабочей области, в логических пикселях. Сам осьминог стоит
/// ещё глубже: поля окна отданы под свечение (frontend/mascot.html, PAD).
const MARGIN: f64 = 0.0;
/// Видимая часть окна при 100 %: спрайт 120×160 и поля под свечение. Само окно больше
/// в `BIG` раз (до двух), чтобы в полёте ничего не обрезалось; лишнее в покое отрезано
/// регионом окна (mascot.html). Видимая часть — по центру окна.
const VISIBLE_W: f64 = 164.0;
const VISIBLE_H: f64 = 204.0;
const MAX_BIG: f64 = 2.0;
/// Сколько маскот ещё виден после команды в режиме `commands`.
const LINGER: Duration = Duration::from_millis(1000);
/// Длина анимации ухода в mascot.html (58 кадров при 48 к/с, и полёт, и портал) — окно
/// прячется после неё.
const LEAVE: Duration = Duration::from_millis(1260);

static MODE: Mutex<String> = Mutex::new(String::new());
/// Как маскот появляется и уходит: `flight` или `portal` (кадры и частицы — в mascot.html).
static ENTRANCE: Mutex<String> = Mutex::new(String::new());
static SCALE: AtomicU32 = AtomicU32::new(100);
/// Во сколько раз окно больше видимой части, в тысячных. Windows не даёт окну быть больше
/// экрана, поэтому на крупных размерах запас под полёт меньше двукратного.
static BIG: AtomicU32 = AtomicU32::new(2000);
/// Поколение показа: отложенное скрытие срабатывает, только если с тех пор ничего не менялось.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
/// Маскот на экране. Само окно после создания не прячется и не обрезается регионом
/// никогда: и показ скрытого прозрачного окна без рамки, и смена его региона Windows
/// сопровождает кадром со стандартной белой рамкой. Спрятанный маскот — пустой холст
/// (mascot.html), а клики окно пропускает сквозь себя (см. `watch_cursor`).
static SHOWN: AtomicBool = AtomicBool::new(false);
/// Осьминог стоит на месте (не летит) — только тогда окно ловит мышь, и только над ним.
static RESTING: AtomicBool = AtomicBool::new(false);

pub fn normalize_mode(mode: &str) -> &'static str {
    match mode.trim() {
        "commands" => "commands",
        "never" => "never",
        _ => "always",
    }
}

pub fn normalize_entrance(entrance: &str) -> &'static str {
    match entrance.trim() {
        "portal" => "portal",
        _ => "flight",
    }
}

pub fn set_entrance(app: &AppHandle, entrance: &str) {
    if let Ok(mut current) = ENTRANCE.lock() {
        *current = normalize_entrance(entrance).to_string();
    }
    if let Some(window) = app.get_webview_window("mascot") {
        let _ = window.emit("mascot://layout", ());
    }
}

pub fn clamp_scale(scale: u32) -> u32 {
    scale.clamp(100, 500)
}

/// Запас под полёт для размера `scale`: вдвое, но так, чтобы окно влезло в монитор.
fn big_for(monitor: Option<tauri::Monitor>, scale: u32) -> f64 {
    let factor = scale as f64 / 100.0;
    let Some(monitor) = monitor else { return MAX_BIG };
    let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
    let fit = (0.97 * size.width / (VISIBLE_W * factor)).min(0.97 * size.height / (VISIBLE_H * factor));
    fit.clamp(1.0, MAX_BIG)
}

/// Правый нижний угол видимой части — в этой доле окна.
fn visible_corner(big: f64) -> f64 {
    0.5 + 0.5 / big
}

/// Раскладка для mascot.html: размер в процентах, запас окна под полёт, виден ли маскот
/// и как он появляется.
#[tauri::command]
pub fn mascot_layout() -> (u32, f64, bool, String) {
    let entrance = ENTRANCE.lock().map(|entrance| entrance.clone()).unwrap_or_default();
    (
        SCALE.load(Ordering::Relaxed),
        BIG.load(Ordering::Relaxed) as f64 / 1000.0,
        SHOWN.load(Ordering::Relaxed),
        normalize_entrance(&entrance).to_string(),
    )
}

fn mode() -> String {
    MODE.lock().map(|mode| mode.clone()).unwrap_or_default()
}

fn command_active() -> bool {
    matches!(events::current_state().as_str(), "listening" | "busy")
}

/// Первый показ при запуске — по сохранённым настройкам.
pub fn spawn(app: &AppHandle) {
    let settings = app.state::<LiveConfig>().get().settings.clone();
    set_entrance(app, &settings.mascot_entrance);
    apply(app, &settings.mascot_mode, settings.mascot_scale);
}

/// Применяет режим и размер: создаёт, показывает, прячет или уничтожает окно.
pub fn apply(app: &AppHandle, mode: &str, scale: u32) {
    let mode = normalize_mode(mode);
    let scale = clamp_scale(scale);
    if let Ok(mut current) = MODE.lock() {
        *current = mode.to_string();
    }
    let old_scale = SCALE.swap(scale, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);

    let window = app.get_webview_window("mascot");
    match (mode, window) {
        ("never", Some(window)) => {
            events::set_mascot_visible(false);
            SHOWN.store(false, Ordering::Relaxed);
            let _ = window.destroy();
        }
        ("never", None) => {}
        (_, None) => create(app),
        (_, Some(window)) => {
            if old_scale != scale {
                resize_keep_corner(&window, scale);
            }
            if mode == "always" || command_active() {
                show(&window);
            } else {
                hide(&window);
            }
        }
    }
}

/// Режим `commands`: появиться, когда Reever начал слушать, и уйти через секунду
/// после того, как команда отработала.
pub fn on_state(app: &AppHandle, state: &str) {
    // «loading» бывает и посреди команды (модели догружаются после «ривер») — не повод уходить
    if mode() != "commands" || state == "loading" {
        return;
    }
    let Some(window) = app.get_webview_window("mascot") else { return };
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    if matches!(state, "listening" | "busy") {
        show(&window);
    } else {
        hide_later(window, generation);
    }
}

fn hide_later(window: WebviewWindow, generation: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(LINGER);
        // пока маскота держат в руке, он никуда не уходит — уйдёт после отпускания
        if GENERATION.load(Ordering::Relaxed) != generation || DRAGGING.load(Ordering::Relaxed) {
            return;
        }
        let _ = window.emit("mascot://leave", ());
        std::thread::sleep(LEAVE);
        if GENERATION.load(Ordering::Relaxed) == generation && !DRAGGING.load(Ordering::Relaxed) {
            hide(&window);
        }
    });
}

fn show(window: &WebviewWindow) {
    // уже на экране (возможно, посреди ухода) — вернуть без повторного появления
    let fresh = !SHOWN.swap(true, Ordering::Relaxed);
    if fresh {
        events::set_mascot_visible(true);
    }
    let _ = window.emit("mascot://enter", fresh);
}

fn hide(window: &WebviewWindow) {
    if SHOWN.swap(false, Ordering::Relaxed) {
        events::set_mascot_visible(false);
    }
    RESTING.store(false, Ordering::Relaxed);
    let _ = window.emit("mascot://hidden", ());
}

/// mascot.html: осьминог приземлился (true) или взлетел/исчез (false).
#[tauri::command]
pub fn mascot_rest(rest: bool) {
    RESTING.store(rest, Ordering::Relaxed);
}

/// Окно больше осьминога и прозрачно почти целиком, но прозрачные пиксели WebView2 всё
/// равно ловят клики. Поэтому окно пропускает мышь сквозь себя везде, кроме видимой части
/// с осьминогом в покое: поток следит за курсором и переключает это на границе.
fn watch_cursor(window: WebviewWindow) {
    std::thread::spawn(move || {
        let mut catching: Option<bool> = None;
        loop {
            std::thread::sleep(Duration::from_millis(40));
            let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else { return };
            let over = DRAGGING.load(Ordering::Relaxed)
                || (SHOWN.load(Ordering::Relaxed)
                    && RESTING.load(Ordering::Relaxed)
                    && cursor().is_some_and(|p| {
                        let big = BIG.load(Ordering::Relaxed) as f64 / 1000.0;
                        let (half_w, half_h) = (size.width as f64 / big / 2.0, size.height as f64 / big / 2.0);
                        let (cx, cy) = (position.x as f64 + size.width as f64 / 2.0, position.y as f64 + size.height as f64 / 2.0);
                        (p.x as f64 - cx).abs() <= half_w && (p.y as f64 - cy).abs() <= half_h
                    }));
            if catching != Some(over) {
                if window.set_ignore_cursor_events(!over).is_err() {
                    return;
                }
                catching = Some(over);
            }
        }
    });
}

fn create(app: &AppHandle) {
    let Some(window_config) = app.config().app.windows.iter().find(|window| window.label == "mascot").cloned() else {
        ui_log(app, "⚠ не найден конфиг окна маскота");
        return;
    };
    let scale = SCALE.load(Ordering::Relaxed);
    let factor = scale as f64 / 100.0;
    let big = big_for(app.primary_monitor().ok().flatten(), scale);
    BIG.store((big * 1000.0).round() as u32, Ordering::Relaxed);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Создаётся далеко за экраном: первый показ окна без рамки Windows сопровождает
        // кадром со стандартной рамкой, и пусть он мелькнёт там, где его никто не видит.
        let built = WebviewWindowBuilder::from_config(&app, &window_config)
            .map(|builder| builder.inner_size(VISIBLE_W * big * factor, VISIBLE_H * big * factor).position(-30000.0, -30000.0))
            .and_then(|builder| builder.build());
        let window = match built {
            Ok(window) => window,
            Err(error) => {
                ui_log(&app, format!("⚠ окно маскота не создано: {error}"));
                return;
            }
        };
        // Окно показывается один раз и навсегда (там же, за экраном); дальше маскот
        // появляется и прячется только рисованием (см. SHOWN), мышь — сквозь окно.
        let _ = window.set_ignore_cursor_events(true);
        let _ = window.show();
        watch_cursor(window.clone());
        // Рабочая область, а не весь монитор: иначе маскот уедет под панель задач.
        if let (Ok(Some(monitor)), Ok(size)) = (window.primary_monitor(), window.outer_size()) {
            let area = monitor.work_area();
            let margin = (MARGIN * monitor.scale_factor()).round() as i32;
            let corner = visible_corner(big);
            let x = area.position.x + area.size.width as i32 - (size.width as f64 * corner).round() as i32 - margin;
            let y = area.position.y + area.size.height as i32 - (size.height as f64 * corner).round() as i32 - margin;
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        // режим, размер или состояние могли смениться, пока окно создавалось
        let mode = mode();
        let current = SCALE.load(Ordering::Relaxed);
        if current != scale {
            resize_keep_corner(&window, current);
        }
        if mode == "never" {
            let _ = window.destroy();
        } else if mode == "always" || command_active() {
            show(&window);
        }
    });
}

/// Новый размер растёт вверх и влево: правый нижний угол видимой части остаётся на месте.
fn resize_keep_corner(window: &WebviewWindow, scale: u32) {
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else { return };
    let factor = scale as f64 / 100.0;
    let old_corner = visible_corner(BIG.load(Ordering::Relaxed) as f64 / 1000.0);
    let big = big_for(window.current_monitor().ok().flatten(), scale);
    BIG.store((big * 1000.0).round() as u32, Ordering::Relaxed);
    if window.set_size(LogicalSize::new(VISIBLE_W * big * factor, VISIBLE_H * big * factor)).is_err() {
        return;
    }
    if let Ok(new_size) = window.outer_size() {
        let corner = visible_corner(big);
        let x = position.x + (size.width as f64 * old_corner - new_size.width as f64 * corner).round() as i32;
        let y = position.y + (size.height as f64 * old_corner - new_size.height as f64 * corner).round() as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
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
        DRAGGING.store(true, Ordering::Relaxed);
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
        DRAGGING.store(false, Ordering::Relaxed);
        let _ = window.emit("mascot://drop", ());
        // команда закончилась, пока маскота держали, — теперь можно уходить
        if mode() == "commands" && !command_active() {
            let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
            hide_later(window, generation);
        }
    });
}

fn cursor() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok().map(|_| p)
}

fn left_button_down() -> bool {
    (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
}
