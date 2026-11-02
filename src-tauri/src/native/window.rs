// Окно на DirectComposition: прозрачное с попиксельной альфой, без рамки, поверх всех, без
// активации. Содержимое — swap chain для композиции (premultiplied alpha), DWM смешивает его
// с рабочим столом сам: окну не нужен ни слой (WS_EX_LAYERED + UpdateLayeredWindow), ни
// перерисовка системой (WS_EX_NOREDIRECTIONBITMAP — у окна нет своей поверхности GDI).
//
// Оконная процедура не трогает состояние потока окон: она складывает события в очередь
// потока, а цикл разбирает их после DispatchMessage. Так нет повторного входа в Ui, когда
// SetWindowPos из задачи синхронно присылает WM_SIZE. Синхронный ответ нужен только
// WM_NCHITTEST — его даёт проверка попадания, лежащая в данных самого окна (WndShared).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1SolidColorBrush, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISurface, IDXGISwapChain1, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, ScreenToClient, ValidateRect, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, RegisterClassExW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, GWLP_USERDATA, HTCLIENT,
    HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SIZE,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use super::device::Gfx;
use super::image::Images;
use super::text::Text;
use super::{log, MouseButton, MouseEvent, MouseKind, RenderCx, WindowHandler, WindowId, WindowSpec};

const CLASS_NAME: &str = "ReeverNativeWindow";

#[derive(Debug)]
pub(crate) enum WinEventKind {
    Resized(u32, u32),
    Dpi(u32),
    Mouse(MouseEvent),
    Paint,
    Destroyed,
    DisplayChange,
}

#[derive(Debug)]
pub(crate) struct WinEvent {
    pub hwnd: isize,
    pub kind: WinEventKind,
}

thread_local! {
    static EVENTS: RefCell<VecDeque<WinEvent>> = const { RefCell::new(VecDeque::new()) };
    static CLASS_REGISTERED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn take_events() -> VecDeque<WinEvent> {
    EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

fn push(hwnd: HWND, kind: WinEventKind) {
    EVENTS.with(|e| e.borrow_mut().push_back(WinEvent { hwnd: hwnd.0 as isize, kind }));
}

/// Проверка попадания: (x, y) в DIP внутри окна → попал ли указатель в видимую часть.
pub type HitTest = Box<dyn Fn(f32, f32) -> bool>;

/// Данные окна, доступные оконной процедуре (лежат в GWLP_USERDATA).
struct WndShared {
    hit_test: RefCell<Option<HitTest>>,
    scale: Cell<f32>,
    tracking: Cell<bool>,
}

fn ensure_class() -> Result<()> {
    if CLASS_REGISTERED.with(|c| c.get()) {
        return Ok(());
    }
    let hinstance = unsafe { GetModuleHandleW(None)? };
    let name = HSTRING::from(CLASS_NAME);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance.into(),
        lpszClassName: windows::core::PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    // класс уже зарегистрирован прошлым потоком окон того же процесса — тоже годится
    unsafe { RegisterClassExW(&wc) };
    CLASS_REGISTERED.with(|c| c.set(true));
    Ok(())
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

fn hiword(v: usize) -> u32 {
    ((v >> 16) & 0xFFFF) as u32
}

fn signed_xy(lp: LPARAM) -> (i32, i32) {
    let v = lp.0 as u32;
    ((v & 0xFFFF) as i16 as i32, (v >> 16) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WndShared;
    let shared = shared.as_ref();
    let scale = shared.map_or(1.0, |s| s.scale.get());
    let mouse = |kind: MouseKind| {
        let (x, y) = signed_xy(lp);
        push(hwnd, WinEventKind::Mouse(MouseEvent { kind, x: x as f32 / scale, y: y as f32 / scale }));
    };
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            if let Some(s) = shared {
                if let Some(hit) = s.hit_test.borrow().as_ref() {
                    let (sx, sy) = signed_xy(lp);
                    let mut pt = POINT { x: sx, y: sy };
                    let _ = ScreenToClient(hwnd, &mut pt);
                    if !hit(pt.x as f32 / scale, pt.y as f32 / scale) {
                        return LRESULT(HTTRANSPARENT as isize);
                    }
                }
            }
            return LRESULT(HTCLIENT as isize);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(hwnd, None);
            push(hwnd, WinEventKind::Paint);
            return LRESULT(0);
        }
        WM_SIZE => push(hwnd, WinEventKind::Resized(loword(lp.0 as usize), hiword(lp.0 as usize))),
        WM_DPICHANGED => {
            let dpi = hiword(wp.0);
            if let Some(s) = shared {
                s.scale.set(dpi as f32 / 96.0);
            }
            // система предлагает прямоугольник под новый масштаб — принимаем его как есть,
            // иначе окно на границе мониторов «прыгает» туда-обратно
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            push(hwnd, WinEventKind::Dpi(dpi));
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            if let Some(s) = shared {
                if !s.tracking.get() {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    if TrackMouseEvent(&mut t).is_ok() {
                        s.tracking.set(true);
                    }
                }
            }
            mouse(MouseKind::Move);
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            if let Some(s) = shared {
                s.tracking.set(false);
            }
            push(hwnd, WinEventKind::Mouse(MouseEvent { kind: MouseKind::Leave, x: -1.0, y: -1.0 }));
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            SetCapture(hwnd);
            let b = if msg == WM_LBUTTONDOWN { MouseButton::Left } else { MouseButton::Right };
            mouse(MouseKind::Down(b));
            return LRESULT(0);
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let _ = ReleaseCapture();
            let b = if msg == WM_LBUTTONUP { MouseButton::Left } else { MouseButton::Right };
            mouse(MouseKind::Up(b));
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => push(hwnd, WinEventKind::DisplayChange),
        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WndShared;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !p.is_null() {
                drop(Box::from_raw(p));
            }
            push(hwnd, WinEventKind::Destroyed);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Ресурсы окна на устройстве: swap chain, цель отрисовки и узел DirectComposition.
struct Surface {
    swap: IDXGISwapChain1,
    target: Option<ID2D1Bitmap1>,
    _dtarget: IDCompositionTarget,
    _visual: IDCompositionVisual,
    generation: u64,
}

pub(crate) enum RenderResult {
    Ok,
    DeviceLost,
}

pub(crate) struct NativeWindow {
    pub id: WindowId,
    pub hwnd: HWND,
    shared: *const WndShared,
    pub dpi: u32,
    pub size_px: (u32, u32),
    pub handler: Box<dyn WindowHandler>,
    pub dirty: bool,
    pub visible: bool,
    surface: Option<Surface>,
    pub created_at: Instant,
    pub first_frame: Option<Duration>,
}

pub(crate) fn dip_to_px(dip: f32, dpi: u32) -> u32 {
    ((dip * dpi as f32 / 96.0).round() as i64).max(1) as u32
}

impl NativeWindow {
    pub fn create(id: WindowId, spec: &WindowSpec, handler: Box<dyn WindowHandler>) -> Result<Self> {
        ensure_class()?;
        let created_at = Instant::now();
        // масштаб монитора, на котором окно появится, — чтобы сразу создать его нужного размера
        let monitor = unsafe { MonitorFromPoint(POINT { x: spec.x, y: spec.y }, MONITOR_DEFAULTTONEAREST) };
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
        let mut ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW;
        if spec.topmost {
            ex |= WS_EX_TOPMOST;
        }
        if spec.no_activate {
            ex |= WS_EX_NOACTIVATE;
        }
        let shared = Box::into_raw(Box::new(WndShared {
            hit_test: RefCell::new(None),
            scale: Cell::new(dx as f32 / 96.0),
            tracking: Cell::new(false),
        }));
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(ex.0),
                &HSTRING::from(CLASS_NAME),
                &HSTRING::from(spec.title.as_str()),
                WS_POPUP,
                spec.x,
                spec.y,
                dip_to_px(spec.width, dx) as i32,
                dip_to_px(spec.height, dx) as i32,
                None,
                None,
                GetModuleHandleW(None)?,
                Some(shared as *const _),
            )
        };
        let hwnd = match hwnd {
            Ok(h) => h,
            Err(e) => {
                // WM_NCDESTROY не пришёл — данные окна освобождаем сами
                unsafe { drop(Box::from_raw(shared)) };
                return Err(e);
            }
        };
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let size_px = (dip_to_px(spec.width, dpi), dip_to_px(spec.height, dpi));
        unsafe {
            (*shared).scale.set(dpi as f32 / 96.0);
            if dpi != dx {
                let _ = SetWindowPos(hwnd, None, 0, 0, size_px.0 as i32, size_px.1 as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            if spec.visible {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        Ok(Self {
            id,
            hwnd,
            shared,
            dpi,
            size_px,
            handler,
            dirty: true,
            visible: spec.visible,
            surface: None,
            created_at,
            first_frame: None,
        })
    }

    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn size_dip(&self) -> (f32, f32) {
        (self.size_px.0 as f32 / self.scale(), self.size_px.1 as f32 / self.scale())
    }

    pub fn set_hit_test(&self, hit: Option<HitTest>) {
        unsafe { *(*self.shared).hit_test.borrow_mut() = hit };
    }

    pub fn show(&mut self, visible: bool) {
        self.visible = visible;
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            if visible {
                // поверх всех, без активации — после показа z-порядок мог сбиться
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        self.dirty = true;
    }

    pub fn set_position(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn set_size_dip(&self, w: f32, h: f32) {
        let (pw, ph) = (dip_to_px(w, self.dpi), dip_to_px(h, self.dpi));
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, 0, 0, pw as i32, ph as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn on_resized(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w, h) == self.size_px {
            return;
        }
        self.size_px = (w, h);
        if let Some(s) = self.surface.as_mut() {
            s.target = None; // ссылка на буфер мешает ResizeBuffers
            let r = unsafe { s.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)) };
            if let Err(e) = r {
                log(&format!("native: ResizeBuffers {w}x{h}: {e}"));
                self.surface = None;
            }
        }
        self.dirty = true;
    }

    pub fn on_dpi(&mut self, dpi: u32) {
        self.dpi = dpi.max(96);
        if let Some(s) = self.surface.as_mut() {
            s.target = None; // DPI зашит в битмап цели
        }
        let scale = self.scale();
        self.handler.dpi_changed(scale);
        self.dirty = true;
    }

    /// Ресурсы устройства больше не годятся (устройство пересоздаётся).
    pub fn drop_surface(&mut self) {
        self.surface = None;
        self.dirty = true;
    }

    fn ensure_surface(&mut self, gfx: &Gfx) -> Result<()> {
        if self.surface.as_ref().is_some_and(|s| s.generation == gfx.generation) {
            return Ok(());
        }
        self.surface = None; // старая цель DirectComposition должна уйти до создания новой
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: self.size_px.0,
            Height: self.size_px.1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        unsafe {
            let swap = gfx.dxgi_factory.CreateSwapChainForComposition(&gfx.d3d, &desc, None)?;
            let dtarget = gfx.dcomp.CreateTargetForHwnd(self.hwnd, true)?;
            let visual = gfx.dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            dtarget.SetRoot(&visual)?;
            gfx.dcomp.Commit()?;
            self.surface = Some(Surface { swap, target: None, _dtarget: dtarget, _visual: visual, generation: gfx.generation });
        }
        Ok(())
    }

    pub fn render(
        &mut self,
        gfx: &Gfx,
        images: &mut Images,
        text: &Text,
        brush: &ID2D1SolidColorBrush,
        now: Instant,
    ) -> Result<RenderResult> {
        self.ensure_surface(gfx)?;
        let dpi = self.dpi as f32;
        let scale = self.scale();
        let size = self.size_dip();
        let dc = &gfx.dc;
        let surface = self.surface.as_mut().expect("surface");
        if surface.target.is_none() {
            let buffer: IDXGISurface = unsafe { surface.swap.GetBuffer(0)? };
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi,
                dpiY: dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            surface.target = Some(unsafe { dc.CreateBitmapFromDxgiSurface(&buffer, Some(&props))? });
        }
        let end = unsafe {
            if let Some(t) = &surface.target {
                dc.SetTarget(t);
            }
            dc.SetDpi(dpi, dpi);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE); // ClearType на прозрачном не работает
            dc.SetTransform(&windows::Foundation::Numerics::Matrix3x2::identity());
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let mut cx = RenderCx { dc, images, text, brush, scale, size, now };
            self.handler.render(&mut cx);
            let end = dc.EndDraw(None, None);
            dc.SetTarget(None);
            end
        };
        if let Err(e) = end {
            if e.code() == windows::Win32::Foundation::D2DERR_RECREATE_TARGET {
                return Ok(RenderResult::DeviceLost);
            }
            return Err(e);
        }
        let hr = unsafe { surface.swap.Present(1, DXGI_PRESENT(0)) };
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            return Ok(RenderResult::DeviceLost);
        }
        hr.ok()?;
        self.dirty = false;
        if self.first_frame.is_none() {
            self.first_frame = Some(self.created_at.elapsed());
        }
        Ok(RenderResult::Ok)
    }

    pub fn destroy(mut self) {
        self.surface = None;
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
