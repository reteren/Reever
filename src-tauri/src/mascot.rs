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
//! Место — `mascot_position`: в настройках осьминога выводят на экран, ставят мышью и
//! закрепляют; дальше он каждый раз выходит там. Без него — правый нижний угол.
//!
//! Размер — `mascot_scale`, 100..500 % от исходного. Осьминога в сфере рисует главное окно
//! (skin/shell.js) по событию `mascot://orb`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{
    webview::WebviewWindowBuilder, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    WebviewWindow,
};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindow, GetWindowLongW, IsWindowVisible, SetWindowPos, GWL_EXSTYLE, GW_HWNDPREV, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, WS_EX_TOPMOST,
};

use crate::bubble;
use crate::config::LiveConfig;
use crate::events::{self, ui_log};
use crate::mascot_native::{self, Msg};

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
/// Пока модели грузятся (состояние «loading») или фраза после «ривер» ещё звучит, срок
/// `WAKE_GRACE` не истекает — но не дольше этого: под нагрузкой загрузка и распознавание после
/// «ривер» идут дольше 8 с, и маскот уходил посреди команды.
const WAKE_GRACE_MAX: Duration = Duration::from_secs(60);

/// Смены «показать / уйти / спрятать» идут из разных потоков (аудио, отложенные уходы,
/// команды окна). Проверка поколения и действие по ней — под одним замком: иначе новое
/// «ривер» между проверкой и действием оставляло маскота спрятанным на всю команду.
static TRANSITION: Mutex<()> = Mutex::new(());

fn transition() -> std::sync::MutexGuard<'static, ()> {
    TRANSITION.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
/// Знакомство первого запуска (`mascot_intro`): осьминог на экране и говорит в пузыре.
static INTRO: AtomicBool = AtomicBool::new(false);
/// Реплики знакомства; пузырь забирает их один раз (`bubble_intro`).
static INTRO_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Закреплённое место (`Settings::mascot_position`).
static PINNED: Mutex<Option<[i32; 2]>> = Mutex::new(None);
/// Настройки вывели осьминога, чтобы его поставили: он стоит, пока не нажмут «Закрепить».
static PLACING: AtomicBool = AtomicBool::new(false);
/// Как часто проверять, что окно маскота не задвинуто под обычные окна (`keep_on_top`),
/// в тиках `watch_cursor` по 40 мс.
const TOPMOST_CHECK_TICKS: u32 = 25;
/// Окно маскота нативное (`native_mascot`), а не веб (mascot.html).
static NATIVE: AtomicBool = AtomicBool::new(false);
/// Для нативного окна: перетаскивание начинается из потока окон, где AppHandle нет под рукой.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Окно маскота: веб (frontend/mascot.html) или нативное (mascot_native). Вся логика показа
/// здесь общая — различается только «транспорт»: как передать событие и сдвинуть окно.
#[derive(Clone)]
enum Win {
    Web(WebviewWindow),
    /// AppHandle и hwnd окна: после пересоздания окна старый hwnd считается исчезнувшим.
    Native(AppHandle, isize),
}

/// Событие окну маскота (mascot://…).
enum Event {
    Enter(bool),
    Leave,
    Hidden,
    Drop,
}

impl Win {
    fn app_handle(&self) -> &AppHandle {
        match self {
            Win::Web(w) => w.app_handle(),
            Win::Native(app, _) => app,
        }
    }

    /// Нативное окно ещё то же самое (не убрано и не пересоздано).
    fn native_alive(hwnd: isize) -> bool {
        mascot_native::hwnd() == Some(hwnd)
    }

    fn emit(&self, event: Event) {
        match self {
            Win::Web(w) => {
                let _ = match event {
                    Event::Enter(fresh) => w.emit("mascot://enter", fresh),
                    Event::Leave => w.emit("mascot://leave", ()),
                    Event::Hidden => w.emit("mascot://hidden", ()),
                    Event::Drop => w.emit("mascot://drop", ()),
                };
            }
            Win::Native(_, h) if Win::native_alive(*h) => mascot_native::send(match event {
                Event::Enter(fresh) => Msg::Enter(fresh),
                Event::Leave => Msg::Leave,
                Event::Hidden => Msg::Hidden,
                Event::Drop => Msg::Drop,
            }),
            Win::Native(..) => {}
        }
    }

    fn outer_position(&self) -> Option<PhysicalPosition<i32>> {
        match self {
            Win::Web(w) => w.outer_position().ok(),
            Win::Native(_, h) => mascot_native::rect()
                .filter(|_| Win::native_alive(*h))
                .map(|(x, y, _, _)| PhysicalPosition::new(x, y)),
        }
    }

    fn outer_size(&self) -> Option<PhysicalSize<u32>> {
        match self {
            Win::Web(w) => w.outer_size().ok(),
            Win::Native(_, h) => mascot_native::rect()
                .filter(|_| Win::native_alive(*h))
                .map(|(_, _, w, h)| PhysicalSize::new(w, h)),
        }
    }

    fn set_position(&self, x: i32, y: i32) {
        match self {
            Win::Web(w) => {
                let _ = w.set_position(PhysicalPosition::new(x, y));
            }
            Win::Native(_, h) => unsafe {
                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
                let _ = SetWindowPos(HWND(*h as *mut _), None, x, y, 0, 0, flags);
            },
        }
    }

    /// Размер в логических пикселях (DIP монитора, на котором окно).
    fn set_size(&self, width: f64, height: f64) -> bool {
        match self {
            Win::Web(w) => w.set_size(LogicalSize::new(width, height)).is_ok(),
            Win::Native(_, h) => {
                let k = self.scale_factor();
                let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
                let (pw, ph) = ((width * k).round() as i32, (height * k).round() as i32);
                unsafe { SetWindowPos(HWND(*h as *mut _), None, 0, 0, pw, ph, flags) }.is_ok()
            }
        }
    }

    fn hwnd(&self) -> Option<HWND> {
        match self {
            Win::Web(w) => w.hwnd().ok().map(|h| HWND(h.0 as _)),
            Win::Native(_, h) => Some(HWND(*h as *mut _)),
        }
    }

    /// Пропускать мышь сквозь окно. false — окна больше нет.
    fn set_ignore_cursor_events(&self, ignore: bool) -> bool {
        match self {
            Win::Web(w) => w.set_ignore_cursor_events(ignore).is_ok(),
            Win::Native(_, h) => {
                if !Win::native_alive(*h) {
                    return false;
                }
                crate::native::set_click_through(*h, ignore);
                true
            }
        }
    }

    fn destroy(&self) {
        match self {
            Win::Web(w) => {
                let _ = w.destroy();
            }
            Win::Native(_, h) => {
                if Win::native_alive(*h) {
                    mascot_native::destroy();
                }
            }
        }
    }

    fn scale_factor(&self) -> f64 {
        match self {
            Win::Web(w) => w.scale_factor().unwrap_or(1.0),
            Win::Native(_, h) => unsafe { GetDpiForWindow(HWND(*h as *mut _)) }.max(96) as f64 / 96.0,
        }
    }

    fn current_monitor(&self) -> Option<tauri::Monitor> {
        match self {
            Win::Web(w) => w.current_monitor().ok().flatten(),
            Win::Native(app, _) => {
                let (p, s) = (self.outer_position()?, self.outer_size()?);
                let (cx, cy) = (p.x as f64 + s.width as f64 / 2.0, p.y as f64 + s.height as f64 / 2.0);
                app.monitor_from_point(cx, cy).ok().flatten()
            }
        }
    }
}

/// Окно маскота, какое бы оно ни было сейчас.
fn win(app: &AppHandle) -> Option<Win> {
    if let Some(h) = mascot_native::hwnd() {
        return Some(Win::Native(app.clone(), h));
    }
    app.get_webview_window("mascot").map(Win::Web)
}

/// Для пузыря (bubble.rs): где окно маскота, его масштаб и монитор.
pub(crate) fn geometry(app: &AppHandle) -> Option<(PhysicalPosition<i32>, PhysicalSize<u32>, f64, Option<tauri::Monitor>)> {
    let w = win(app)?;
    Some((w.outer_position()?, w.outer_size()?, w.scale_factor(), w.current_monitor()))
}

/// Окно маскота сейчас нативное (настройка `native_mascot`).
pub fn native_enabled() -> bool {
    NATIVE.load(Ordering::Relaxed)
}

/// Включить или выключить нативное окно маскота на лету: текущее окно убирается, новое
/// создаётся в том же режиме (на экране маскот выходит заново из портала).
pub fn set_native(app: &AppHandle, on: bool) {
    let _transition = transition();
    if NATIVE.swap(on, Ordering::Relaxed) == on {
        return;
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if let Some(window) = win(app) {
        if SHOWN.swap(false, Ordering::Relaxed) {
            events::set_mascot_visible(false);
        }
        RESTING.store(false, Ordering::Relaxed);
        LEAVING.store(false, Ordering::Relaxed);
        window.destroy();
    }
    ui_log(app, format!("mascot: {} window", if on { "native" } else { "web" }));
    if mode() != "never" {
        if on {
            mascot_native::prewarm(app);
        }
        create(app);
    }
    bubble::sync(app);
}

/// Состояние распознавания, уровень микрофона и «услышал» — нативному окну (веб-окно
/// получает их событиями reever://… само).
fn native_event(msg: Msg) {
    if mascot_native::exists() {
        mascot_native::send(msg);
    }
}

/// reever://level для нативного окна.
pub fn level(rms: f32) {
    native_event(Msg::Level(rms));
}

/// reever://heard для нативного окна.
pub fn heard(text: &str) {
    if !text.trim().is_empty() {
        native_event(Msg::Heard);
    }
}

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

pub(crate) fn intro_active() -> bool {
    INTRO.load(Ordering::Relaxed)
}

/// Осьминогу сейчас положено быть на экране и в режиме `commands`: идёт команда или его ставят.
fn command_active() -> bool {
    PLACING.load(Ordering::Relaxed) || matches!(events::current_state().as_str(), "listening" | "busy")
}

fn pinned() -> Option<[i32; 2]> {
    PINNED.lock().ok().and_then(|pinned| *pinned)
}

/// Правый нижний угол видимой части осьминога (физические пиксели).
fn anchor_of(window: &Win) -> Option<[i32; 2]> {
    let (position, size) = (window.outer_position()?, window.outer_size()?);
    let corner = visible_corner(BIG.load(Ordering::Relaxed) as f64 / 1000.0);
    Some([
        position.x + (size.width as f64 * corner).round() as i32,
        position.y + (size.height as f64 * corner).round() as i32,
    ])
}

/// Место по умолчанию: правый нижний угол рабочей области основного монитора.
fn default_anchor(app: &AppHandle) -> Option<[i32; 2]> {
    let monitor = app.primary_monitor().ok()??;
    let area = monitor.work_area();
    let margin = (MARGIN * monitor.scale_factor()).round() as i32;
    Some([
        area.position.x + area.size.width as i32 - margin,
        area.position.y + area.size.height as i32 - margin,
    ])
}

/// Закреплённое место, если оно всё ещё на каком-то мониторе (монитор могли отключить).
fn pinned_on_screen(app: &AppHandle) -> Option<[i32; 2]> {
    let [x, y] = pinned()?;
    let monitors = app.available_monitors().ok()?;
    monitors
        .iter()
        .any(|monitor| {
            let (position, size) = (monitor.position(), monitor.size());
            x > position.x
                && y > position.y
                && x <= position.x + size.width as i32
                && y <= position.y + size.height as i32
        })
        .then_some([x, y])
}

fn move_to(window: &Win, [x, y]: [i32; 2]) {
    let Some(size) = window.outer_size() else { return };
    let corner = visible_corner(BIG.load(Ordering::Relaxed) as f64 / 1000.0);
    window.set_position(
        x - (size.width as f64 * corner).round() as i32,
        y - (size.height as f64 * corner).round() as i32,
    );
    bubble::place(window.app_handle());
}

/// Новое закреплённое место из настроек (`None` — угол по умолчанию).
pub fn set_pinned(position: Option<[i32; 2]>) {
    if let Ok(mut pinned) = PINNED.lock() {
        *pinned = position;
    }
}

/// Настройки: вывести осьминога на экран, чтобы его перетащили, куда нужно. Он стоит,
/// пока не вызван `finish_placing`. `false` — маскота нет (режим `never`).
pub fn begin_placing(app: &AppHandle) -> bool {
    if mode() == "never" {
        return false;
    }
    let Some(window) = win(app) else { return false };
    let _transition = transition();
    PLACING.store(true, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if IN_ORB.load(Ordering::Relaxed) {
        orb(app, "absent");
    }
    show(&window);
    true
}

/// Конец расстановки: `save` — закрепить место, где осьминог стоит сейчас (его и вернуть).
/// В режиме `commands` он после этого уходит, как после команды.
pub fn finish_placing(app: &AppHandle, save: bool) -> Option<[i32; 2]> {
    let was_placing = PLACING.swap(false, Ordering::Relaxed);
    let window = win(app)?;
    let anchor = anchor_of(&window);
    if save {
        set_pinned(anchor);
    }
    if was_placing && mode() == "commands" && !command_active() {
        hide_later(window, GENERATION.fetch_add(1, Ordering::Relaxed) + 1);
    }
    anchor.filter(|_| save)
}

/// Открепить: осьминог снова выходит в правом нижнем углу.
pub fn unpin(app: &AppHandle) {
    set_pinned(None);
    if let Some(window) = win(app) {
        if let Some(anchor) = default_anchor(app) {
            move_to(&window, anchor);
        }
    }
}

/// Первый показ при запуске — по сохранённым настройкам.
pub fn spawn(app: &AppHandle) {
    let settings = app.state::<LiveConfig>().get().settings.clone();
    let _ = APP.set(app.clone());
    NATIVE.store(settings.native_mascot, Ordering::Relaxed);
    if settings.native_mascot && settings.effective_mascot_mode() != "never" {
        // поток окон, устройство и шейдер линзы — фоном, пока создаётся остальное
        mascot_native::prewarm(app);
    }
    set_pinned(settings.mascot_position);
    apply(app, settings.effective_mascot_mode(), settings.mascot_scale);
    bubble::configure(app, settings.mascot_bubble);
}

/// Применяет режим и размер: создаёт, показывает, прячет или уничтожает окно.
pub fn apply(app: &AppHandle, mode: &str, scale: u32) {
    let mode = normalize_mode(mode);
    let scale = clamp_scale(scale);
    let _transition = transition();
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

    let window = win(app);
    match (mode, window) {
        ("never", Some(window)) => {
            events::set_mascot_visible(false);
            SHOWN.store(false, Ordering::Relaxed);
            window.destroy();
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
    native_event(Msg::State(state.to_string()));
    let _transition = transition();
    on_state_locked(app, state);
}

/// `on_state` под уже взятым `transition()`.
fn on_state_locked(app: &AppHandle, state: &str) {
    // «loading» бывает и посреди команды (модели догружаются после «ривер») — не повод уходить
    if mode() != "commands" || state == "loading" || intro_active() || PLACING.load(Ordering::Relaxed) {
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
    let Some(window) = win(app) else { return };
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    if matches!(state, "listening" | "busy") {
        if !SHOWN.load(Ordering::Relaxed) && IN_ORB.load(Ordering::Relaxed) && events::main_visible() {
            // телепорт: в сфере уходит в портал, на экране чуть позже выходит из портала
            orb(app, "depart");
            std::thread::spawn(move || {
                std::thread::sleep(TELEPORT);
                let _transition = transition();
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
    native_event(Msg::Wake);
    if mode() != "commands" {
        return;
    }
    let generation = {
        let _transition = transition();
        on_state_locked(app, "listening");
        WAKE_PENDING.store(true, Ordering::Relaxed);
        GENERATION.load(Ordering::Relaxed)
    };
    let Some(window) = win(app) else { return };
    std::thread::spawn(move || {
        std::thread::sleep(WAKE_GRACE);
        let waited = std::time::Instant::now();
        // ждём, пока грузятся модели и пока фраза после «ривер» ещё звучит: «ривер напиши …»
        // одним духом под нагрузкой может распознаться позже срока, а уходить посреди фразы нельзя
        while (events::current_state() == "loading" || events::wake_phrase_open())
            && waited.elapsed() < WAKE_GRACE_MAX.saturating_sub(WAKE_GRACE)
            && GENERATION.load(Ordering::Relaxed) == generation
        {
            std::thread::sleep(Duration::from_millis(250));
        }
        let _transition = transition();
        if GENERATION.load(Ordering::Relaxed) == generation {
            ui_log(window.app_handle(), "mascot: the wake word was not confirmed in time, leaving");
            WAKE_PENDING.store(false, Ordering::Relaxed);
            hide_later(window, generation);
        }
    });
}

/// Большая модель не нашла «ривер» во фразе после срабатывания детектора — уйти, не
/// дожидаясь `WAKE_GRACE`.
pub fn on_wake_rejected(app: &AppHandle) {
    native_event(Msg::WakeRejected);
    let _transition = transition();
    if WAKE_PENDING.swap(false, Ordering::Relaxed) {
        on_state_locked(app, "idle");
    }
}

/// Режим `commands`: через секунду после команды уйти в портал на экране и выйти в сфере.
fn hide_later(window: Win, generation: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(LINGER);
        let app = window.app_handle().clone();
        let to_orb = {
            let _transition = transition();
            // пока маскота держат в руке, он никуда не уходит — уйдёт после отпускания
            if GENERATION.load(Ordering::Relaxed) != generation
                || DRAGGING.load(Ordering::Relaxed)
                || PLACING.load(Ordering::Relaxed)
                || mode() != "commands"
            {
                return;
            }
            window.emit(Event::Leave);
            LEAVING.store(true, Ordering::Relaxed);
            bubble::notify(&app);
            events::main_visible()
        };
        if to_orb {
            std::thread::sleep(TELEPORT);
            let _transition = transition();
            if GENERATION.load(Ordering::Relaxed) != generation {
                return;
            }
            orb(&app, "arrive");
        }
        std::thread::sleep(if to_orb { LEAVE.saturating_sub(TELEPORT) } else { LEAVE });
        let _transition = transition();
        if GENERATION.load(Ordering::Relaxed) == generation && !DRAGGING.load(Ordering::Relaxed) {
            hide(&window);
            if !to_orb {
                orb(&app, "present");
            }
        }
    });
}

fn show(window: &Win) {
    // уже на экране (возможно, посреди ухода) — вернуть без повторного появления
    let fresh = !SHOWN.swap(true, Ordering::Relaxed);
    if fresh {
        ui_log(window.app_handle(), "mascot: shown");
        // закреплённое место: выходит там, даже если в прошлый раз его утащили
        if let Some(anchor) = pinned_on_screen(window.app_handle()) {
            move_to(window, anchor);
        }
        events::set_mascot_visible(true);
    }
    LEAVING.store(false, Ordering::Relaxed);
    window.emit(Event::Enter(fresh));
    bubble::notify(window.app_handle());
}

fn hide(window: &Win) {
    if SHOWN.swap(false, Ordering::Relaxed) {
        ui_log(window.app_handle(), "mascot: hidden");
        events::set_mascot_visible(false);
    }
    RESTING.store(false, Ordering::Relaxed);
    LEAVING.store(false, Ordering::Relaxed);
    window.emit(Event::Hidden);
    bubble::notify(window.app_handle());
}

/// Первый запуск (app.js): осьминог выходит на экран и представляется — реплики `lines`
/// печатает пузырь, потом зовёт `mascot_intro_done`. Пузырь показывается и тогда, когда
/// он выключен в настройках. `false` — маскота нет (режим `never`, лёгкий интерфейс).
#[tauri::command]
pub fn mascot_intro(app: AppHandle, lines: Vec<String>) -> bool {
    if mode() == "never" || lines.is_empty() {
        return false;
    }
    if let Ok(mut current) = INTRO_LINES.lock() {
        *current = lines;
    }
    INTRO.store(true, Ordering::Relaxed);
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    bubble::sync(&app);
    // окно ещё создаётся (первые секунды после запуска) — выйдет, когда будет готово
    let Some(window) = win(&app) else { return true };
    let _transition = transition();
    if !SHOWN.load(Ordering::Relaxed) && IN_ORB.load(Ordering::Relaxed) && events::main_visible() {
        orb(&app, "depart");
        std::thread::spawn(move || {
            std::thread::sleep(TELEPORT);
            let _transition = transition();
            if GENERATION.load(Ordering::Relaxed) == generation {
                show(&window);
            }
        });
    } else {
        if IN_ORB.load(Ordering::Relaxed) {
            orb(&app, "absent");
        }
        show(&window);
    }
    true
}

/// bubble.html: реплики знакомства (пусто — знакомства нет или они уже отданы).
#[tauri::command]
pub fn bubble_intro() -> Vec<String> {
    INTRO_LINES.lock().map(|mut lines| std::mem::take(&mut *lines)).unwrap_or_default()
}

/// bubble.html договорил знакомство: в режиме `commands` осьминог уходит в сферу.
#[tauri::command]
pub fn mascot_intro_done(app: AppHandle) {
    if !INTRO.swap(false, Ordering::Relaxed) {
        return;
    }
    let _ = app.emit_to("main", "reever://mascot-intro-done", ());
    bubble::sync(&app);
    if mode() == "commands" && !command_active() {
        if let Some(window) = win(&app) {
            hide_later(window.clone(), GENERATION.fetch_add(1, Ordering::Relaxed) + 1);
        }
    }
}

/// Настройки → Интерфейс: нативное окно маскота вкл/выкл — сразу, без перезапуска.
#[tauri::command]
pub fn set_native_mascot(app: AppHandle, config: tauri::State<'_, LiveConfig>, enabled: bool) -> Result<(), String> {
    let current = config.get();
    crate::commands_api::persist_value(&current, "native_mascot", serde_json::Value::Bool(enabled))?;
    let mut fresh = (*current).clone();
    fresh.settings.native_mascot = enabled;
    config.set(std::sync::Arc::new(fresh));
    set_native(&app, enabled);
    Ok(())
}

/// skin/fps.js: «Плавность» сменилась (или веб-окно загрузилось с ней) — для нативных окон.
#[tauri::command]
pub fn set_fps_limit(config: tauri::State<'_, LiveConfig>, limit: u32) -> Result<(), String> {
    let limit = if crate::native::FPS_LIMITS.contains(&limit) { limit } else { 0 };
    let current = config.get();
    mascot_native::set_fps_limit(limit);
    if current.settings.fps_limit == limit {
        return Ok(());
    }
    crate::commands_api::persist_value(&current, "fps_limit", serde_json::Value::from(limit))?;
    let mut fresh = (*current).clone();
    fresh.settings.fps_limit = limit;
    config.set(std::sync::Arc::new(fresh));
    Ok(())
}

/// mascot.html: осьминог приземлился (true) или взлетел/исчез (false).
#[tauri::command]
pub fn mascot_rest(rest: bool) {
    RESTING.store(rest, Ordering::Relaxed);
}

/// Окно больше осьминога и прозрачно почти целиком, но прозрачные пиксели WebView2 всё
/// равно ловят клики. Поэтому окно пропускает мышь сквозь себя везде, кроме видимой части
/// с осьминогом в покое: поток следит за курсором и переключает это на границе.
fn watch_cursor(window: Win) {
    std::thread::spawn(move || {
        let mut catching: Option<bool> = None;
        let mut tick = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(40));
            tick = tick.wrapping_add(1);
            if tick % TOPMOST_CHECK_TICKS == 0 && SHOWN.load(Ordering::Relaxed) && keep_on_top(&window) {
                // пузырь стоит над маскотом и должен остаться над ним
                bubble::raise(window.app_handle());
            }
            let (Some(position), Some(size)) = (window.outer_position(), window.outer_size()) else { return };
            let native = matches!(window, Win::Native(..));
            let over = DRAGGING.load(Ordering::Relaxed)
                || (SHOWN.load(Ordering::Relaxed)
                    && RESTING.load(Ordering::Relaxed)
                    // нативное окно ловит мышь только над непрозрачными пикселями осьминога;
                    // веб-окно — над всей видимой частью (прозрачные пиксели WebView2 ловят клики)
                    && cursor().is_some_and(|p| if native { mascot_native::over_opaque(p) } else {
                        let big = BIG.load(Ordering::Relaxed) as f64 / 1000.0;
                        let (half_w, half_h) = (size.width as f64 / big / 2.0, size.height as f64 / big / 2.0);
                        let (cx, cy) = (position.x as f64 + size.width as f64 / 2.0, position.y as f64 + size.height as f64 / 2.0);
                        (p.x as f64 - cx).abs() <= half_w && (p.y as f64 - cy).abs() <= half_h
                    }));
            if catching != Some(over) {
                if !window.set_ignore_cursor_events(!over) {
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
fn keep_on_top(window: &Win) -> bool {
    let Some(hwnd) = window.hwnd() else { return false };
    let mut above = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
    while let Ok(other) = above {
        if other.0.is_null() {
            break;
        }
        let topmost = unsafe { GetWindowLongW(other, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
        if !topmost && unsafe { IsWindowVisible(other) }.as_bool() && !cloaked(other) {
            raise_topmost(hwnd);
            return true;
        }
        above = unsafe { GetWindow(other, GW_HWNDPREV) };
    }
    false
}

fn raise_topmost(hwnd: HWND) {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
    let _ = unsafe { SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags) };
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
    let scale = SCALE.load(Ordering::Relaxed);
    let factor = scale as f64 / 100.0;
    let big = big_for(app.primary_monitor().ok().flatten(), scale);
    BIG.store((big * 1000.0).round() as u32, Ordering::Relaxed);
    if NATIVE.load(Ordering::Relaxed) {
        create_native(app, scale, VISIBLE_W * big * factor, VISIBLE_H * big * factor);
        return;
    }
    let Some(window_config) = app.config().app.windows.iter().find(|window| window.label == "mascot").cloned() else {
        ui_log(app, crate::msg!("log.window.noConfig", window = "mascot"));
        return;
    };
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
                ui_log(&app, crate::msg!("log.window.failed", window = "mascot", error = error));
                return;
            }
        };
        // Окно показывается один раз и навсегда (там же, за экраном); дальше маскот
        // появляется и прячется только рисованием (см. SHOWN), мышь — сквозь окно.
        let _ = window.set_ignore_cursor_events(true);
        let _ = window.show();
        let window = Win::Web(window);
        if NATIVE.load(Ordering::Relaxed) {
            // пока окно создавалось, включили нативное — это веб-окно уже лишнее
            window.destroy();
            return;
        }
        created(&app, window, scale);
    });
}

/// Нативное окно: создаётся в своём потоке (поток окон мог ещё не прогреться), дальше — как
/// веб-окно. Прозрачное окно без рамки на DirectComposition белой рамкой при показе не мелькает,
/// поэтому создаётся сразу у угла, где будет стоять, — с DPI нужного монитора.
fn create_native(app: &AppHandle, scale: u32, width: f64, height: f64) {
    let app = app.clone();
    std::thread::spawn(move || {
        let anchor = pinned_on_screen(&app).or_else(|| default_anchor(&app)).unwrap_or([0, 0]);
        let fps = app.state::<LiveConfig>().get().settings.fps_limit;
        let Some(hwnd) = mascot_native::create(&app, anchor[0] - 1, anchor[1] - 1, width, height, fps) else { return };
        let window = Win::Native(app.clone(), hwnd);
        if !NATIVE.load(Ordering::Relaxed) {
            window.destroy();
            return;
        }
        created(&app, window, scale);
    });
}

/// Окно создано (веб или нативное): слежение за курсором, место, и всё, что могло смениться,
/// пока оно создавалось.
fn created(app: &AppHandle, window: Win, scale: u32) {
    watch_cursor(window.clone());
    // Закреплённое место или угол рабочей области (не всего монитора: иначе маскот
    // уедет под панель задач).
    if let Some(anchor) = pinned_on_screen(app).or_else(|| default_anchor(app)) {
        move_to(&window, anchor);
    }
    // режим, размер или состояние могли смениться, пока окно создавалось
    let _transition = transition();
    let mode = mode();
    let current = SCALE.load(Ordering::Relaxed);
    if current != scale {
        resize_keep_corner(&window, current);
    }
    if mode == "never" {
        window.destroy();
    } else if mode == "always" || command_active() || intro_active() {
        if IN_ORB.load(Ordering::Relaxed) {
            orb(app, "absent");
        }
        show(&window);
    }
}

/// Новый размер растёт вверх и влево: правый нижний угол видимой части остаётся на месте.
fn resize_keep_corner(window: &Win, scale: u32) {
    let (Some(position), Some(size)) = (window.outer_position(), window.outer_size()) else { return };
    let factor = scale as f64 / 100.0;
    let old_corner = visible_corner(BIG.load(Ordering::Relaxed) as f64 / 1000.0);
    let big = big_for(window.current_monitor(), scale);
    BIG.store((big * 1000.0).round() as u32, Ordering::Relaxed);
    if !window.set_size(VISIBLE_W * big * factor, VISIBLE_H * big * factor) {
        return;
    }
    if let Some(new_size) = window.outer_size() {
        let corner = visible_corner(big);
        let x = position.x + (size.width as f64 * old_corner - new_size.width as f64 * corner).round() as i32;
        let y = position.y + (size.height as f64 * old_corner - new_size.height as f64 * corner).round() as i32;
        window.set_position(x, y);
    }
    // у веб-окна раскладку пересчитывает resize страницы, нативному — сказать
    if matches!(window, Win::Native(..)) {
        window_relayout();
    }
    bubble::place(window.app_handle());
}

fn window_relayout() {
    if mascot_native::exists() {
        mascot_native::send(Msg::Relayout);
    }
}

/// Перетаскивание за голову. Захват указателя в WebView2 теряется, как только окно
/// поехало под курсором, поэтому окно ведёт этот поток по системному курсору, пока
/// зажата левая кнопка, а об отпускании страница узнаёт событием `mascot://drop`.
#[tauri::command]
pub fn mascot_drag(window: WebviewWindow) {
    drag(Win::Web(window));
}

/// Нативное окно: голову взяли (поток окон) — то же перетаскивание, что у веб-окна.
pub(crate) fn drag_native() {
    let (Some(app), Some(hwnd)) = (APP.get(), mascot_native::hwnd()) else { return };
    drag(Win::Native(app.clone(), hwnd));
}

fn drag(window: Win) {
    std::thread::spawn(move || {
        let (Some(start), Some(origin)) = (cursor(), window.outer_position()) else {
            window.emit(Event::Drop);
            return;
        };
        DRAGGING.store(true, Ordering::Relaxed);
        let mut last = start;
        while left_button_down() {
            if let Some(p) = cursor() {
                if p.x != last.x || p.y != last.y {
                    window.set_position(origin.x + p.x - start.x, origin.y + p.y - start.y);
                    bubble::place(window.app_handle());
                    last = p;
                }
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        DRAGGING.store(false, Ordering::Relaxed);
        window.emit(Event::Drop);
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
