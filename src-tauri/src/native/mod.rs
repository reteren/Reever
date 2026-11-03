//! Нативное ядро окон без WebView2: Direct2D + DirectWrite + WIC + DirectComposition
//! (этап 0a docs/native-ui-roadmap.md).
//!
//! Как пользоваться:
//! ```ignore
//! let ui = native::UiThread::spawn()?;                 // свой поток с циклом сообщений
//! let id = ui.call(|ui| {
//!     let frame = ui.images().add_file("mascot/idle_0001.webp");
//!     ui.create_window(WindowSpec { width: 160.0, height: 213.0, ..Default::default() },
//!                      Box::new(MyView { frame }))
//! })??;
//! ui.post(move |ui| { ui.handler_mut::<MyView>(id).unwrap().state = 2; ui.invalidate(id); });
//! ```
//! Окно рисует `WindowHandler::render`; перерисовка только когда окно «грязное»
//! (`Ui::invalidate`) или `animating()` возвращает true — тогда кадры идут с лимитом
//! `Ui::set_fps_limit` (0/60/30) и стоят во время `Ui::pause_animations`.
//! Tauri живёт рядом в своём потоке: этот модуль его цикл не трогает.

pub mod a11y;
pub mod controls;
mod device;
mod frame;
mod image;
pub mod stats;
mod text;
mod ui;
mod window;

use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_POINT_2F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, ID2D1SolidColorBrush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP};

pub use frame::{FrameClock, FPS_LIMITS};
pub use image::{AlphaMask, ImageId, Images, Loader, DEFAULT_CACHE_BUDGET};
pub use text::{is_rtl, Align, Direction, Font, Text, TextLayout, TextStyle};
pub use ui::Ui;
pub use window::{activate, begin_drag, set_click_through, set_cursor, set_ime_caret, Cursor, HitTest};

/// Сообщение-будильник: в очереди задач что-то есть.
const WM_JOB: u32 = WM_APP + 0x3A1;

type Job = Box<dyn FnOnce(&mut Ui) + Send>;

static LOGGER: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();

/// Куда писать журнал модуля (один раз, до первого окна). По умолчанию — stderr.
pub fn set_logger(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = LOGGER.set(Box::new(f));
}

pub(crate) fn log(msg: &str) {
    match LOGGER.get() {
        Some(f) => f(msg),
        None => eprintln!("{msg}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub(crate) u32);

/// Как создать окно.
#[derive(Debug, Clone)]
pub struct WindowSpec {
    pub title: String,
    /// Левый верхний угол, экранные пиксели.
    pub x: i32,
    pub y: i32,
    /// Размер в DIP (в пикселях — умножить на масштаб монитора).
    pub width: f32,
    pub height: f32,
    pub topmost: bool,
    /// Не забирать фокус ни при показе, ни при клике (маскот, пузырь).
    pub no_activate: bool,
    pub visible: bool,
    /// Обычное окно приложения: кнопка на панели задач, сворачивается и разворачивается ею.
    /// Без флага — окно-инструмент (маскот, пузырь, диалоги): на панели задач его нет.
    pub taskbar: bool,
}

impl Default for WindowSpec {
    fn default() -> Self {
        Self {
            title: "Reever".into(),
            x: 100,
            y: 100,
            width: 200.0,
            height: 200.0,
            topmost: true,
            no_activate: true,
            visible: true,
            taskbar: false,
        }
    }
}

/// Сведения об окне.
#[derive(Debug, Clone, Copy)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub dpi: u32,
    pub scale: f32,
    pub size_px: (u32, u32),
    /// Время от создания окна до первого показанного кадра.
    pub first_frame: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Move,
    Down(MouseButton),
    Up(MouseButton),
    /// Указатель ушёл из окна (координаты не заданы).
    Leave,
    /// Второе нажатие левой кнопки подряд (вместо второго Down).
    DoubleClick,
}

/// Нажатие клавиши: виртуальный код (VK_*) и модификаторы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: u32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// автоповтор зажатой клавиши
    pub repeat: bool,
}

/// Событие мыши; координаты в DIP внутри окна.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub x: f32,
    pub y: f32,
}

/// Доступ к D3D11 перед кадром (вне BeginDraw/EndDraw Direct2D): свои шейдеры рисуют в свои
/// текстуры, а потом эти текстуры рисуются в кадре как битмапы Direct2D.
pub struct GpuCx<'a> {
    pub d3d: &'a windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub context: &'a windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub dc: &'a ID2D1DeviceContext,
    /// Меняется при пересоздании устройства: ресурсы прошлого поколения выбросить.
    pub generation: u64,
    pub scale: f32,
    pub now: Instant,
    /// DirectWrite — разметить текст до кадра (например, записать его в список команд для эффектов).
    pub text: &'a Text,
}

/// Содержимое окна. Живёт в потоке окон.
pub trait WindowHandler: std::any::Any {
    /// Окно создано (hwnd — для `set_cursor` и т.п. из потока окон).
    fn attached(&mut self, _hwnd: isize) {}
    /// Перед кадром, до рисования Direct2D: работа с D3D11 (свои шейдеры).
    fn prepare(&mut self, _gpu: &GpuCx<'_>) {}
    /// Нарисовать кадр (фон уже прозрачный).
    fn render(&mut self, cx: &mut RenderCx<'_>);
    /// Окно анимируется: кадры идут сами, с лимитом частоты.
    fn animating(&self) -> bool {
        false
    }
    /// Перерисоваться к этому сроку, даже если не анимируется (мигание каретки, задержки):
    /// поток окон спит до него без кадров в промежутке.
    fn wake_at(&self) -> Option<Instant> {
        None
    }
    /// Шаг анимации перед кадром (`dt` — время с прошлого шага, не больше 100 мс).
    fn advance(&mut self, _dt: Duration) {}
    /// Мышь; true — нужна перерисовка.
    fn mouse(&mut self, _ev: MouseEvent) -> bool {
        false
    }
    /// Масштаб монитора сменился (окно перенесли); размеры в DIP остаются прежними.
    fn dpi_changed(&mut self, _scale: f32) {}
    /// Клавиша; true — нужна перерисовка.
    fn key(&mut self, _ev: KeyEvent) -> bool {
        false
    }
    /// Введённый символ (после раскладки и IME); true — нужна перерисовка.
    fn char_input(&mut self, _c: char) -> bool {
        false
    }
    /// Колесо мыши в точке (DIP); `delta` — деления (+ от себя).
    fn wheel(&mut self, _x: f32, _y: f32, _delta: f32) -> bool {
        false
    }
    /// Окно получило или потеряло фокус клавиатуры.
    fn focus(&mut self, _focused: bool) -> bool {
        false
    }
    /// Окно просят закрыть (Alt+F4) — решает обработчик.
    fn close_requested(&mut self) {}
    /// Дерево элементов для экранного диктора (UI Automation); None — окно без него.
    /// Зовётся после каждого кадра; границы — в физических пикселях окна (`a11y::bounds`).
    fn a11y_tree(&mut self, _scale: f32) -> Option<a11y::accesskit::TreeUpdate> {
        None
    }
    /// Действие диктора (нажать, перевести фокус); true — нужна перерисовка.
    fn a11y_action(&mut self, _req: a11y::accesskit::ActionRequest) -> bool {
        false
    }
}

/// Цвет без предумножения альфы, 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    pub fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

/// Прямоугольник в DIP.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn d2d(self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

/// Всё для рисования одного кадра окна. Координаты — DIP.
pub struct RenderCx<'a> {
    pub(crate) dc: &'a ID2D1DeviceContext,
    pub(crate) images: &'a mut Images,
    pub(crate) text: &'a Text,
    pub(crate) brush: &'a ID2D1SolidColorBrush,
    /// Масштаб монитора (1.0 = 96 DPI).
    pub scale: f32,
    /// Размер окна в DIP.
    pub size: (f32, f32),
    pub now: Instant,
}

impl RenderCx<'_> {
    /// Контекст Direct2D — для того, чего нет в помощниках (эффекты, геометрия).
    pub fn dc(&self) -> &ID2D1DeviceContext {
        self.dc
    }

    pub fn text(&self) -> &Text {
        self.text
    }

    pub fn images(&mut self) -> &mut Images {
        self.images
    }

    /// Битмап картинки ровно `w`×`h` пикселей (для эффектов Direct2D). DPI битмапа — 96.
    pub fn bitmap(&mut self, id: ImageId, w: u32, h: u32) -> Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap1> {
        match self.images.bitmap(self.dc, id, w, h) {
            Ok(b) => Some(b),
            Err(e) => {
                log(&format!("native: картинка {}: {e}", id.0));
                None
            }
        }
    }

    /// Кисть одного цвета (общая на кадр — цвет ставится перед каждым использованием).
    pub fn brush(&self, color: Color) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color.d2d()) };
        self.brush
    }

    /// Выровнять DIP-координату по пиксельной сетке монитора.
    pub fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    pub fn fill_rect(&mut self, r: Rect, color: Color) {
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.dc.FillRectangle(&r.d2d(), self.brush);
        }
    }

    pub fn fill_rounded(&mut self, r: Rect, radius: f32, color: Color) {
        unsafe {
            self.brush.SetColor(&color.d2d());
            let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            self.dc.FillRoundedRectangle(&rr, self.brush);
        }
    }

    /// Нарисовать картинку в прямоугольник. Битмап берётся из кэша ровно под пиксельный размер
    /// прямоугольника на этом мониторе, а угол выравнивается по пикселям — без размытия.
    pub fn draw_image(&mut self, id: ImageId, r: Rect, opacity: f32) {
        let pw = (r.w * self.scale).round().max(1.0) as u32;
        let ph = (r.h * self.scale).round().max(1.0) as u32;
        let bitmap = match self.images.bitmap(self.dc, id, pw, ph) {
            Ok(b) => b,
            Err(e) => {
                log(&format!("native: картинка {}: {e}", id.0));
                return;
            }
        };
        let (x, y) = (self.snap(r.x), self.snap(r.y));
        let dest = Rect::new(x, y, pw as f32 / self.scale, ph as f32 / self.scale).d2d();
        unsafe {
            self.dc.DrawBitmap(&bitmap, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        }
    }

    /// Нарисовать размеченный текст; (x, y) — левый верхний угол прямоугольника разметки.
    pub fn draw_text(&mut self, layout: &TextLayout, x: f32, y: f32, color: Color) {
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.dc.DrawTextLayout(
                D2D_POINT_2F { x: self.snap(x), y: self.snap(y) },
                &layout.layout,
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
        }
    }
}

/// Поток окон. Объекты Direct2D живут в нём; сюда присылаются задачи.
/// Drop завершает поток и закрывает его окна.
pub struct UiThread {
    tid: u32,
    tx: Option<mpsc::Sender<Job>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl UiThread {
    pub fn spawn() -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let join = std::thread::Builder::new().name("reever-native-ui".into()).spawn(move || unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            // per-monitor v2 для окон этого потока — независимо от манифеста процесса
            let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            // создать очередь сообщений потока до того, как в неё начнут слать будильники
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            match Ui::new(rx) {
                Ok(ui) => {
                    let _ = ready_tx.send(Ok(GetCurrentThreadId()));
                    ui.run();
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            }
            if com {
                CoUninitialize();
            }
        })?;
        let tid = ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("поток окон завершился при запуске"))?
            .map_err(|e| anyhow::anyhow!("поток окон: {e}"))?;
        Ok(Self { tid, tx: Some(tx), join: Some(join) })
    }

    /// Выполнить задачу в потоке окон, не дожидаясь. false — поток уже завершён.
    pub fn post(&self, f: impl FnOnce(&mut Ui) + Send + 'static) -> bool {
        let Some(tx) = &self.tx else { return false };
        if tx.send(Box::new(f)).is_err() {
            return false;
        }
        unsafe { PostThreadMessageW(self.tid, WM_JOB, None, None) }.is_ok()
    }

    /// Выполнить задачу в потоке окон и дождаться результата. None — поток завершён.
    /// Не звать из самого потока окон (зависнет).
    pub fn call<R: Send + 'static>(&self, f: impl FnOnce(&mut Ui) -> R + Send + 'static) -> Option<R> {
        let (tx, rx) = mpsc::channel();
        if !self.post(move |ui| {
            let _ = tx.send(f(ui));
        }) {
            return None;
        }
        rx.recv().ok()
    }

    pub fn is_running(&self) -> bool {
        self.join.as_ref().is_some_and(|j| !j.is_finished())
    }
}

impl Drop for UiThread {
    fn drop(&mut self) {
        self.post(|ui| ui.quit());
        self.tx = None;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
