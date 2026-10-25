//! Маскот: хромовый осьминог в правом нижнем углу экрана (окно `mascot`,
//! frontend/mascot.html). За голову его можно взять и перетащить; анимации — пререндер
//! в frontend/mascot/.
//!
//! Режимы (`mascot_mode` в настройках):
//! - `always` — осьминог всегда на экране;
//! - `commands` — в покое он сидит в сфере главного окна; пока Reever слушает и выполняет
//!   команду, телепортируется на экран: уходит в портал в сфере и выходит из портала в углу
//!   экрана, а после команды — обратно;
//! - `never` — окна нет вовсе (WebView2 не держится в памяти), осьминог всегда в сфере.
//!
//! Размер — `mascot_scale`, 100..500 % от исходного. Осьминога в сфере рисует главное окно
//! (skin/shell.js) по событию `mascot://orb`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{
    webview::WebviewWindowBuilder, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewWindow,
};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindow, GetWindowLongW, IsWindowVisible, SetWindowPos, GWL_EXSTYLE, GW_HWNDPREV, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, WS_EX_TOPMOST,
};

use crate::bubble;
use crate::config::LiveConfig;
use crate::events::{self, ui_log};

/// Отступ окна от краёв рабочей области, в логических пикселях. Сам осьминог стоит
/// ещё глубже: поля окна отданы под свечение (frontend/mascot.html, PAD).
const MARGIN: f64 = 0.0;
/// Видимая часть окна при 100 %: спрайт 120×160 и поля под свечение. Само окно больше
/// в `BIG` раз (до двух), чтобы портал не обрезался; клики вне осьминога окно пропускает
/// сквозь себя. Видимая часть — по центру окна.
const VISIBLE_W: f64 = 164.0;
const VISIBLE_H: f64 = 204.0;
const MAX_BIG: f64 = 2.0;
/// Сколько маскот ещё виден после команды в режиме `commands`.
const LINGER: Duration = Duration::from_millis(1000);
/// Длина ухода в портал (58 кадров при 48 к/с) — окно прячется после неё.
const LEAVE: Duration = Duration::from_millis(1260);
/// Телепорт между сферой и экраном: второй портал открывается с этой задержкой, чтобы
/// осьминог успел скрыться в первом. Только когда главное окно видно — иначе сферы не видно
/// и ждать незачем.
const TELEPORT: Duration = Duration::from_millis(450);
/// Сколько маскот ждёт после срабатывания детектора, пока большая модель подтвердит
/// «ривер» (загрузка и распознавание — 1–2 с, на медленной машине дольше). Не подтвердила
/// и ничего не случилось — ложное срабатывание, маскот уходит.
const WAKE_GRACE: Duration = Duration::from_secs(8);

static MODE: Mutex<String> = Mutex::new(String::new());
/// Осьминог сидит в сфере главного окна.
static IN_ORB: AtomicBool = AtomicBool::new(false);
static SCALE: AtomicU32 = AtomicU32::new(100);
/// Во сколько раз окно больше видимой части, в тысячных. Windows не даёт окну быть больше
/// экрана, поэтому на крупных размерах запас под полёт меньше двукратного.
static BIG: AtomicU32 = AtomicU32::new(2000);
/// Поколение показа: отложенное скрытие срабатывает, только если с тех пор ничего не менялось.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
/// Маскот вышел по детектору (`on_wake`), а большая модель «ривер» ещё не подтвердила.
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
/// Маскот на экране. Само окно после создания не прячется и не обрезается регионом
/// никогда: и показ скрытого прозрачного окна без рамки, и смена его региона Windows
/// сопровождает кадром со стандартной белой рамкой. Спрятанный маскот — пустой холст
/// (mascot.html), а клики окно пропускает сквозь себя (см. `watch_cursor`).
static SHOWN: AtomicBool = AtomicBool::new(false);
/// Осьминог стоит на месте (не летит) — только тогда окно ловит мышь, и только над ним.
static RESTING: AtomicBool = AtomicBool::new(false);
/// Маскот уже уходит в портал (пузырь над ним гаснет сразу, не дожидаясь конца ухода).
static LEAVING: AtomicBool = AtomicBool::new(false);
/// Как часто проверять, что окно маскота не задвинуто под обычные окна (`keep_on_top`),
/// в тиках `watch_cursor` по 40 мс.
const TOPMOST_CHECK_TICKS: u32 = 25;

pub fn normalize_mode(mode: &str) -> &'static str {
    match mode.trim() {
        "commands" => "commands",
        "never" => "never",
        _ => "always",
    }
}

pub fn clamp_scale(scale: u32) -> u32 {
    scale.clamp(100, 500)
}

/// Запас под портал для размера `scale`: вдвое, но так, чтобы окно влезло в монитор.
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

/// Раскладка для mascot.html: размер в процентах, запас окна под портал и виден ли маскот.
#[tauri::command]
pub fn mascot_layout() -> (u32, f64, bool) {
    (SCALE.load(Ordering::Relaxed), BIG.load(Ordering::Relaxed) as f64 / 1000.0, SHOWN.load(Ordering::Relaxed))
}

/// Сидит ли осьминог в сфере — главное окно спрашивает при создании.
#[tauri::command]
pub fn mascot_orb() -> bool {
    IN_ORB.load(Ordering::Relaxed)
}

/// Осьминог в сфере главного окна: `arrive` — выйти из портала, `depart` — уйти в портал,
/// `present` / `absent` — сразу сидеть / сразу исчезнуть.
fn orb(app: &AppHandle, action: &str) {
    IN_ORB.store(matches!(action, "arrive" | "present"), Ordering::Relaxed);
    let _ = app.emit_to("main", "mascot://orb", action);
}

pub(crate) fn mode() -> String {
    MODE.lock().map(|mode| mode.clone()).unwrap_or_default()
}

pub(crate) fn scale() -> u32 {
    SCALE.load(Ordering::Relaxed)
}

/// Маскот на экране и не уходит — над ним можно показывать пузырь.
pub(crate) fn on_screen() -> bool {
    SHOWN.load(Ordering::Relaxed) && !LEAVING.load(Ordering::Relaxed)
}

fn command_active() -> bool {
    matches!(events::current_state().as_str(), "listening" | "busy")
}

/// Первый показ при запуске — по сохранённым настройкам.
pub fn spawn(app: &AppHandle) {
    let settings = app.state::<LiveConfig>().get().settings.clone();
    apply(app, settings.effective_mascot_mode(), settings.mascot_scale);
    bubble::configure(app, settings.mascot_bubble);
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

    // смена режима — без телепорта: осьминог сразу там, где ему положено быть
    let in_orb = !(mode == "always" || (mode == "commands" && command_active()));
    if in_orb != IN_ORB.load(Ordering::Relaxed) {
        orb(app, if in_orb { "present" } else { "absent" });
    }

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
    bubble::sync(app);
}

/// Режим `commands`: появиться, когда Reever начал слушать, и уйти через секунду
/// после того, как команда отработала.
pub fn on_state(app: &AppHandle, state: &str) {
    // «loading» бывает и посреди команды (модели догружаются после «ривер») — не повод уходить
    if mode() != "commands" || state == "loading" {
        return;
    }
    // После `on_wake` «idle» ещё ничего не значит: его ставят и по окончании загрузки
    // моделей, пока человек договаривает команду. Решают «слушаю»/«выполняю» или отказ.
    if state == "idle" && WAKE_PENDING.load(Ordering::Relaxed) {
        return;
    }
    if matches!(state, "listening" | "busy") {
        WAKE_PENDING.store(false, Ordering::Relaxed);
    }
    let Some(window) = app.get_webview_window("mascot") else { return };
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    if matches!(state, "listening" | "busy") {
        if !SHOWN.load(Ordering::Relaxed) && IN_ORB.load(Ordering::Relaxed) && events::main_visible() {
            // телепорт: в сфере уходит в портал, на экране чуть позже выходит из портала
            orb(app, "depart");
            std::thread::spawn(move || {
                std::thread::sleep(TELEPORT);
                if GENERATION.load(Ordering::Relaxed) == generation {
                    show(&window);
                }
            });
        } else {
            // уже на экране (возможно, посреди ухода) или сферы не видно — сразу
            if IN_ORB.load(Ordering::Relaxed) {
                orb(app, "absent");
            }
            show(&window);
        }
    } else {
        hide_later(window, generation);
    }
}

/// Режим `commands`: детектор услышал «ривер» — выйти сразу, вместе с мгновенным сигналом,
/// а не когда большая модель подтвердит слово. Любая смена состояния после этого (слушаю,
/// команда, отказ) ведёт маскота как обычно; если её так и не было — уйти через `WAKE_GRACE`.
pub fn on_wake(app: &AppHandle) {
    if mode() != "commands" {
        return;
    }
    on_state(app, "listening");
    WAKE_PENDING.store(true, Ordering::Relaxed);
    let Some(window) = app.get_webview_window("mascot") else { return };
    let generation = GENERATION.load(Ordering::Relaxed);
    std::thread::spawn(move || {
        std::thread::sleep(WAKE_GRACE);
        if GENERATION.load(Ordering::Relaxed) == generation {
            WAKE_PENDING.store(false, Ordering::Relaxed);
            hide_later(window, generation);
        }
    });
}

/// Большая модель не нашла «ривер» во фразе после срабатывания детектора — уйти, не
/// дожидаясь `WAKE_GRACE`.
pub fn on_wake_rejected(app: &AppHandle) {
    if WAKE_PENDING.swap(false, Ordering::Relaxed) {
        on_state(app, "idle");
    }
}

/// Режим `commands`: через секунду после команды уйти в портал на экране и выйти в сфере.
fn hide_later(window: WebviewWindow, generation: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(LINGER);
        // пока маскота держат в руке, он никуда не уходит — уйдёт после отпускания
        if GENERATION.load(Ordering::Relaxed) != generation || DRAGGING.load(Ordering::Relaxed) || mode() != "commands" {
            return;
        }
        let _ = window.emit("mascot://leave", ());
        let app = window.app_handle().clone();
        LEAVING.store(true, Ordering::Relaxed);
        bubble::notify(&app);
        let to_orb = events::main_visible();
        if to_orb {
            std::thread::sleep(TELEPORT);
            if GENERATION.load(Ordering::Relaxed) != generation {
                return;
            }
            orb(&app, "arrive");
            std::thread::sleep(LEAVE.saturating_sub(TELEPORT));
        } else {
            std::thread::sleep(LEAVE);
        }
        if GENERATION.load(Ordering::Relaxed) == generation && !DRAGGING.load(Ordering::Relaxed) {
            hide(&window);
            if !to_orb {
                orb(&app, "present");
            }
        }
    });
}

fn show(window: &WebviewWindow) {
    // уже на экране (возможно, посреди ухода) — вернуть без повторного появления
    let fresh = !SHOWN.swap(true, Ordering::Relaxed);
    if fresh {
        events::set_mascot_visible(true);
    }
    LEAVING.store(false, Ordering::Relaxed);
    let _ = window.emit("mascot://enter", fresh);
    bubble::notify(window.app_handle());
}

fn hide(window: &WebviewWindow) {
    if SHOWN.swap(false, Ordering::Relaxed) {
        events::set_mascot_visible(false);
    }
    RESTING.store(false, Ordering::Relaxed);
    LEAVING.store(false, Ordering::Relaxed);
    let _ = window.emit("mascot://hidden", ());
    bubble::notify(window.app_handle());
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
        let mut tick = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(40));
            tick = tick.wrapping_add(1);
            if tick % TOPMOST_CHECK_TICKS == 0 && SHOWN.load(Ordering::Relaxed) && keep_on_top(&window) {
                // пузырь стоит над маскотом и должен остаться над ним
                if let Some(bubble) = window.app_handle().get_webview_window("bubble") {
                    raise_topmost(&bubble);
                }
            }
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

/// Windows иногда задвигает окно «поверх всех» под обычные окна: флаг WS_EX_TOPMOST
/// остаётся, а места наверху z-порядка нет, и маскот пропадает за ними, пока окно не
/// пересоздать (замечено 2026-10-19: маскот под Telegram и редактором). Если над окном
/// есть видимое обычное окно — вернуть его наверх. Возвращает, поднимали ли окно.
fn keep_on_top(window: &WebviewWindow) -> bool {
    let Ok(raw) = window.hwnd() else { return false };
    let hwnd = HWND(raw.0 as _);
    let mut above = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
    while let Ok(other) = above {
        if other.0.is_null() {
            break;
        }
        let topmost = unsafe { GetWindowLongW(other, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
        if !topmost && unsafe { IsWindowVisible(other) }.as_bool() && !cloaked(other) {
            raise_topmost(window);
            return true;
        }
        above = unsafe { GetWindow(other, GW_HWNDPREV) };
    }
    false
}

fn raise_topmost(window: &WebviewWindow) {
    if let Ok(raw) = window.hwnd() {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        let _ = unsafe { SetWindowPos(HWND(raw.0 as _), HWND_TOPMOST, 0, 0, 0, 0, flags) };
    }
}

/// Скрытое системой окно (UWP в фоне): «видимо», но на экране его нет.
fn cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, std::mem::size_of::<u32>() as u32)
    }
    .is_ok()
        && cloaked != 0
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
        bubble::place(&app);
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
    bubble::place(window.app_handle());
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
                    bubble::place(window.app_handle());
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
