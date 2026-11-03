//! Меню значка в трее — системное меню Windows (`TrackPopupMenu`), пункты которого
//! рисует сам Reever (owner-draw): чёрное стекло, люминофорный акцент, шапка со
//! статусом микрофона и значки из шрифта значков Windows.
//!
//! Раньше здесь было своё окно с WebView (frontend/traymenu.html): на каждый правый клик
//! создавался WebView2, грузил скин и шрифты и только потом показывался. Из фонового
//! процесса это было тяжело и ненадёжно — меню то появлялось с задержкой, то не появлялось
//! вовсе. Системное меню показывает сама Windows: мгновенно, закрывается по Esc и щелчку
//! мимо, ходит стрелками, а памяти в покое не держит — от нас только краски.
//!
//! Меню живёт в своём потоке со скрытым окном-владельцем: модальный цикл
//! `TrackPopupMenu` не встаёт посреди цикла событий Tauri, а выбранный пункт уходит
//! в главный поток. Это же окно держит и сам значок (`Shell_NotifyIconW`): щелчки по нему
//! приходят сюда сообщением `WM_TRAY`, а после перезапуска Проводника значок добавляется
//! заново (`TaskbarCreated`).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock};

use tauri::{AppHandle, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{
    AddFontMemResourceEx, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, FillRect,
    GetDC, GetStockObject, GetTextExtentPoint32W, MonitorFromPoint, ReleaseDC, RoundRect, SelectObject,
    SetBkMode, SetTextCharacterExtra, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DRAW_TEXT_FORMAT, FW_NORMAL, FW_SEMIBOLD, HDC, HFONT, MONITOR_DEFAULTTONEAREST, NULL_PEN,
    OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED, ODT_MENU};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, InsertMenuItemW, PostMessageW, RegisterClassW, SetForegroundWindow,
    SetMenuInfo, TrackPopupMenu, TranslateMessage, HMENU, MENUINFO, MENUITEMINFOW, MFS_ENABLED,
    MFT_OWNERDRAW, MFT_SEPARATOR, MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIM_BACKGROUND, MSG,
    SM_MENUDROPALIGNMENT, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN,
    TPM_RIGHTBUTTON, WM_APP, WM_DRAWITEM, WM_ENTERIDLE, WM_MEASUREITEM, WM_NULL, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::actions::SharedState;
use crate::uilang;

/// Окно-владелец меню (поток `traymenu`); 0 — ещё не создано.
static OWNER: AtomicIsize = AtomicIsize::new(0);
static APP: OnceLock<AppHandle> = OnceLock::new();
/// Шрифт шапки (Handjet из ресурсов интерфейса) зарегистрирован в процессе.
static BRAND_FONT: OnceLock<bool> = OnceLock::new();

const WM_SHOW_MENU: u32 = WM_APP + 1;
/// Щелчок по значку: в `lParam` — сообщение мыши.
const WM_TRAY: u32 = WM_APP + 2;
/// Номер значка у окна-владельца.
const TRAY_ID: u32 = 1;
/// Значок приложения (icons/icon.ico): размер выбирает Windows под DPI трея.
static ICON_ICO: &[u8] = include_bytes!("../icons/icon.ico");
/// `RegisterWindowMessageW("TaskbarCreated")`: Проводник перезапустился, значки пропали.
static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// `WM_ENTERIDLE` от меню: `wParam == MSGF_MENU`, в `lParam` — окно самого меню.
const MSGF_MENU: usize = 2;

const ID_SHOW: u32 = 1;
const ID_PAUSE: u32 = 2;
const ID_QUIT: u32 = 3;

/// Что за строка меню (`dwItemData`). Шапка и линии — разделители: на них не встаёт
/// ни мышь, ни стрелки.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Header = 1,
    Line = 2,
    Show = 3,
    Pause = 4,
    Quit = 5,
}

impl Row {
    fn from(data: usize) -> Row {
        match data {
            1 => Row::Header,
            3 => Row::Show,
            4 => Row::Pause,
            5 => Row::Quit,
            _ => Row::Line,
        }
    }
}

// Палитра скина (frontend/skin.css): чёрное стекло и люминофор --acc, его яркости
// уже смешаны с фоном — GDI не умеет прозрачность.
const BACK: COLORREF = rgb(0x05, 0x10, 0x0D);
const HOVER: COLORREF = rgb(0x0B, 0x29, 0x1F);
const LINE: COLORREF = rgb(0x10, 0x30, 0x26);
const BORDER: COLORREF = rgb(0x17, 0x5C, 0x47);
const TEXT: COLORREF = rgb(0xC9, 0xEC, 0xDF);
const TEXT_HOT: COLORREF = rgb(0xC8, 0xFF, 0xEC);
const ACC: COLORREF = rgb(0x27, 0xE6, 0xAA);
const DIM: COLORREF = rgb(0x4F, 0x7A, 0x6C);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

// Значки: Segoe Fluent Icons (Windows 11) или Segoe MDL2 Assets — коды у них общие.
const ICON_SETTINGS: char = '\u{E713}';
const ICON_PAUSE: char = '\u{E769}';
const ICON_MIC: char = '\u{E720}';
const ICON_POWER: char = '\u{E7E8}';

/// Всё, что нужно на время одного показа меню: подписи, состояние и шрифты под DPI
/// монитора, где щёлкнули.
struct Session {
    scale: f32,
    paused: bool,
    show: String,
    pause: String,
    quit: String,
    status: String,
    text: HFONT,
    icons: HFONT,
    brand: HFONT,
    small: HFONT,
    framed: bool,
}

impl Session {
    fn px(&self, value: f32) -> i32 {
        (value * self.scale).round() as i32
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for font in [self.text, self.icons, self.brand, self.small] {
            unsafe {
                let _ = DeleteObject(font);
            }
        }
    }
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Запустить поток меню. Зовётся один раз при создании значка в трее.
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let fonts = app.asset_resolver().get("fonts/Handjet.ttf".into()).map(|asset| asset.bytes);
    let _ = std::thread::Builder::new().name("traymenu".into()).spawn(move || {
        if let Some(bytes) = fonts {
            let mut count = 0u32;
            let handle = unsafe {
                AddFontMemResourceEx(bytes.as_ptr() as *const c_void, bytes.len() as u32, None, &mut count)
            };
            let _ = BRAND_FONT.set(!handle.is_invalid() && count > 0);
        }
        run();
    });
}

/// Показать меню у курсора (правый клик по значку).
pub fn show() {
    let owner = OWNER.load(Ordering::Acquire);
    if owner != 0 {
        unsafe {
            let _ = PostMessageW(HWND(owner as *mut c_void), WM_SHOW_MENU, WPARAM(0), LPARAM(0));
        }
    }
}

fn run() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: w!("ReeverTrayMenu"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let Ok(owner) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("ReeverTrayMenu"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            module,
            None,
        ) else {
            return;
        };
        OWNER.store(owner.0 as isize, Ordering::Release);
        TASKBAR_CREATED.store(windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Release);
        add_icon(owner);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Иконка из .ico нужного размера (16 px × масштаб трея): образ не меньше нужного, самый
/// маленький из таких; Windows уменьшит его сама.
unsafe fn load_icon(size: i32) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};
    let (offset, len) = ico_entry(size as u32)?;
    CreateIconFromResourceEx(&ICON_ICO[offset..offset + len], true, 0x0003_0000, size, size, LR_DEFAULTCOLOR).ok()
}

/// Смещение и длина образа в .ico: каталог ICONDIR, записи по 16 байт (ширина 0 — 256).
fn ico_entry(want: u32) -> Option<(usize, usize)> {
    let count = u16::from_le_bytes([*ICON_ICO.get(4)?, *ICON_ICO.get(5)?]) as usize;
    let mut best: Option<(u32, usize, usize)> = None;
    for i in 0..count {
        let e = 6 + i * 16;
        let w = match *ICON_ICO.get(e)? as u32 {
            0 => 256,
            w => w,
        };
        let len = u32::from_le_bytes(ICON_ICO.get(e + 8..e + 12)?.try_into().ok()?) as usize;
        let off = u32::from_le_bytes(ICON_ICO.get(e + 12..e + 16)?.try_into().ok()?) as usize;
        if off + len > ICON_ICO.len() {
            continue;
        }
        // не меньше нужного и поменьше; если все меньше — самый большой
        let better = match best {
            None => true,
            Some((bw, _, _)) => (w >= want && (bw < want || w < bw)) || (bw < want && w > bw),
        };
        if better {
            best = Some((w, off, len));
        }
    }
    best.map(|(_, off, len)| (off, len))
}

unsafe fn icon_data(owner: HWND) -> windows::Win32::UI::Shell::NOTIFYICONDATAW {
    use windows::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NOTIFYICONDATAW};
    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(owner).max(96);
    let size = (16 * dpi / 96) as i32;
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: owner,
        uID: TRAY_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    if let Some(icon) = load_icon(size) {
        data.hIcon = icon;
    }
    for (dst, src) in data.szTip.iter_mut().zip("Reever".encode_utf16()) {
        *dst = src;
    }
    data
}

/// Добавить значок в трей (при старте и после перезапуска Проводника).
unsafe fn add_icon(owner: HWND) {
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_ADD, NIM_MODIFY};
    let data = icon_data(owner);
    if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() && !Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() {
        eprintln!("[reever] tray icon: Shell_NotifyIcon failed");
    } else if data.hIcon.is_invalid() {
        eprintln!("[reever] tray icon: no image in icon.ico");
    }
}

/// Убрать значок (выход): иначе он висит в трее, пока над ним не проведут мышью.
pub fn remove_icon() {
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
    let owner = OWNER.load(Ordering::Acquire);
    if owner == 0 {
        return;
    }
    let data = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: HWND(owner as *mut c_void), uID: TRAY_ID, ..Default::default() };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// Действие на главном потоке приложения.
fn run_action(action: fn(&AppHandle)) {
    let Some(app) = APP.get() else { return };
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || action(&handle));
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{WM_LBUTTONUP, WM_RBUTTONUP};
    let taskbar = TASKBAR_CREATED.load(Ordering::Acquire);
    if taskbar != 0 && msg == taskbar {
        add_icon(hwnd);
        return LRESULT(0);
    }
    match msg {
        WM_SHOW_MENU => {
            popup(hwnd);
            LRESULT(0)
        }
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => run_action(crate::show_main),
                WM_RBUTTONUP => popup(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_MEASUREITEM => {
            let item = &mut *(lparam.0 as *mut MEASUREITEMSTRUCT);
            if item.CtlType != ODT_MENU {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            measure(hwnd, item);
            LRESULT(1)
        }
        WM_DRAWITEM => {
            let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if item.CtlType != ODT_MENU {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            draw(item);
            LRESULT(1)
        }
        WM_ENTERIDLE if wparam.0 == MSGF_MENU => {
            frame(HWND(lparam.0 as *mut c_void));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn popup(owner: HWND) {
    let Some(app) = APP.get() else { return };
    let mut at = POINT::default();
    let _ = GetCursorPos(&mut at);
    let locale = *uilang::CURRENT.lock().unwrap();
    let paused = paused(app);
    SESSION.with(|cell| *cell.borrow_mut() = Some(session(at, locale, paused)));

    let Ok(menu) = CreatePopupMenu() else { return };
    let back = CreateSolidBrush(BACK);
    let info = MENUINFO {
        cbSize: size_of::<MENUINFO>() as u32,
        fMask: MIM_BACKGROUND,
        hbrBack: back,
        ..Default::default()
    };
    let _ = SetMenuInfo(menu, &info);
    let rows = [
        (Row::Header, 0),
        (Row::Line, 0),
        (Row::Show, ID_SHOW),
        (Row::Pause, ID_PAUSE),
        (Row::Line, 0),
        (Row::Quit, ID_QUIT),
    ];
    for (position, (row, id)) in rows.into_iter().enumerate() {
        insert(menu, position as u32, row, id);
    }

    // Без этого меню не закрывается щелчком мимо (известная особенность меню трея).
    let _ = SetForegroundWindow(owner);
    let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | align,
        at.x,
        at.y,
        0,
        owner,
        None,
    );
    let _ = PostMessageW(owner, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    let _ = DeleteObject(back);
    SESSION.with(|cell| cell.borrow_mut().take());

    let action: fn(&AppHandle) = match chosen.0 as u32 {
        ID_SHOW => crate::show_main,
        ID_PAUSE => crate::toggle_pause,
        ID_QUIT => crate::quit,
        _ => return,
    };
    run_action(action);
}

unsafe fn insert(menu: HMENU, position: u32, row: Row, id: u32) {
    let separator = matches!(row, Row::Header | Row::Line);
    let item = MENUITEMINFOW {
        cbSize: size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE | MIIM_ID | MIIM_DATA | MIIM_STATE,
        fType: if separator { MFT_OWNERDRAW | MFT_SEPARATOR } else { MFT_OWNERDRAW },
        fState: MFS_ENABLED,
        wID: id,
        dwItemData: row as usize,
        ..Default::default()
    };
    let _ = InsertMenuItemW(menu, position, true, &item);
}

unsafe fn session(at: POINT, locale: &'static str, paused: bool) -> Session {
    let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
    let (mut dpi, mut _y) = (96u32, 96u32);
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut _y);
    let scale = dpi as f32 / 96.0 * MENU_SCALE;
    let text_face = if system_font("SegUIVar.ttf") { w!("Segoe UI Variable Text") } else { w!("Segoe UI") };
    let icon_face = if system_font("SegoeIcons.ttf") { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") };
    let brand_face = if BRAND_FONT.get() == Some(&true) { w!("Handjet") } else { text_face };
    let px = |value: f32| (value * scale).round() as i32;
    Session {
        scale,
        paused,
        show: uilang::text(locale, "tray.show").to_string(),
        pause: uilang::text(locale, if paused { "tray.mic.resume" } else { "tray.mic.pause" }).to_string(),
        quit: uilang::text(locale, "tray.quit").to_string(),
        status: uilang::text(locale, if paused { "tray.status.paused" } else { "tray.status.listening" }).to_string(),
        text: font(px(14.0), FW_NORMAL.0 as i32, text_face),
        icons: font(px(15.0), FW_NORMAL.0 as i32, icon_face),
        brand: font(px(22.0), FW_SEMIBOLD.0 as i32, brand_face),
        small: font(px(12.0), FW_NORMAL.0 as i32, text_face),
        framed: false,
    }
}

fn system_font(file: &str) -> bool {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    std::path::Path::new(&windir).join("Fonts").join(file).exists()
}

unsafe fn font(height: i32, weight: i32, face: PCWSTR) -> HFONT {
    CreateFontW(
        -height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        0,
        face,
    )
}

/// Общий масштаб меню: размеры ниже заданы крупно, а меню трея должно быть компактным.
const MENU_SCALE: f32 = 0.7;

// Размеры в логических пикселях (при 100 % и MENU_SCALE = 1).
const PAD: f32 = 10.0;
const ICON_BOX: f32 = 22.0;
const GAP: f32 = 10.0;
const ROW_H: f32 = 34.0;
const HEADER_H: f32 = 40.0;
const LINE_H: f32 = 9.0;
const MIN_W: f32 = 230.0;

unsafe fn measure(owner: HWND, item: &mut MEASUREITEMSTRUCT) {
    SESSION.with(|cell| {
        let cell = cell.borrow();
        let Some(s) = cell.as_ref() else { return };
        let hdc = GetDC(owner);
        let (width, height) = match Row::from(item.itemData) {
            Row::Line => (0, s.px(LINE_H)),
            Row::Header => {
                let brand = text_width(hdc, s.brand, "REEVER") + s.px(2.0) * 5;
                let status = text_width(hdc, s.small, &s.status);
                (brand + status + s.px(PAD * 2.0 + 14.0 + 28.0), s.px(HEADER_H))
            }
            row => {
                let label = text_width(hdc, s.text, label(s, row));
                (label + s.px(PAD * 2.0 + ICON_BOX + GAP + 18.0), s.px(ROW_H))
            }
        };
        ReleaseDC(owner, hdc);
        item.itemWidth = width.max(s.px(MIN_W)) as u32;
        item.itemHeight = height as u32;
    });
}

fn label(s: &Session, row: Row) -> &str {
    match row {
        Row::Show => &s.show,
        Row::Pause => &s.pause,
        Row::Quit => &s.quit,
        _ => "",
    }
}

unsafe fn draw(item: &DRAWITEMSTRUCT) {
    SESSION.with(|cell| {
        let cell = cell.borrow();
        let Some(s) = cell.as_ref() else { return };
        let hdc = item.hDC;
        let r = item.rcItem;
        fill(hdc, r, BACK);
        SetBkMode(hdc, TRANSPARENT);
        match Row::from(item.itemData) {
            Row::Line => {
                let y = (r.top + r.bottom) / 2;
                let line = RECT { left: r.left + s.px(PAD), top: y, right: r.right - s.px(PAD), bottom: y + s.px(1.0).max(1) };
                fill(hdc, line, LINE);
            }
            Row::Header => draw_header(hdc, s, r),
            row => {
                let hot = item.itemState.0 & ODS_SELECTED.0 != 0;
                if hot {
                    let inset = RECT { left: r.left + s.px(4.0), top: r.top + s.px(1.0), right: r.right - s.px(4.0), bottom: r.bottom - s.px(1.0) };
                    round(hdc, inset, s.px(6.0), HOVER);
                    // Светящаяся черта слева — как зажжённый сегмент на экранах скина.
                    let mid = (r.top + r.bottom) / 2;
                    let bar = RECT { left: inset.left + s.px(3.0), top: mid - s.px(7.0), right: inset.left + s.px(5.0), bottom: mid + s.px(7.0) };
                    round(hdc, bar, s.px(2.0), ACC);
                }
                let glyph = match row {
                    Row::Show => ICON_SETTINGS,
                    Row::Pause if s.paused => ICON_MIC,
                    Row::Pause => ICON_PAUSE,
                    _ => ICON_POWER,
                };
                let icon = RECT { left: r.left + s.px(PAD + 4.0), top: r.top, right: r.left + s.px(PAD + 4.0 + ICON_BOX), bottom: r.bottom };
                text(hdc, s.icons, &glyph.to_string(), icon, if hot { TEXT_HOT } else { ACC }, DT_LEFT);
                let rest = RECT { left: icon.right + s.px(GAP), right: r.right - s.px(PAD), ..r };
                text(hdc, s.text, label(s, row), rest, if hot { TEXT_HOT } else { TEXT }, DT_LEFT | DT_END_ELLIPSIS);
            }
        }
    });
}

unsafe fn draw_header(hdc: HDC, s: &Session, r: RECT) {
    let left = RECT { left: r.left + s.px(PAD + 4.0), top: r.top + s.px(2.0), ..r };
    SetTextCharacterExtra(hdc, s.px(2.0));
    text(hdc, s.brand, "REEVER", left, ACC, DT_LEFT);
    SetTextCharacterExtra(hdc, 0);

    let colour = if s.paused { DIM } else { ACC };
    let status = RECT { right: r.right - s.px(PAD + 4.0), ..r };
    text(hdc, s.small, &s.status, status, colour, DT_RIGHT);
    // Точка-индикатор перед статусом: горит, пока Reever слушает.
    let width = text_width(hdc, s.small, &s.status);
    let (d, mid) = (s.px(7.0), (r.top + r.bottom) / 2);
    let x = status.right - width - s.px(8.0) - d;
    let dot = RECT { left: x, top: mid - d / 2, right: x + d, bottom: mid - d / 2 + d };
    round(hdc, dot, d, colour);
}

unsafe fn text(hdc: HDC, font: HFONT, value: &str, mut rect: RECT, colour: COLORREF, align: DRAW_TEXT_FORMAT) {
    let old = SelectObject(hdc, font);
    SetTextColor(hdc, colour);
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    DrawTextW(hdc, &mut wide, &mut rect, align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    SelectObject(hdc, old);
}

unsafe fn text_width(hdc: HDC, font: HFONT, value: &str) -> i32 {
    let old = SelectObject(hdc, font);
    let wide: Vec<u16> = value.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
    SelectObject(hdc, old);
    size.cx
}

unsafe fn fill(hdc: HDC, rect: RECT, colour: COLORREF) {
    let brush = CreateSolidBrush(colour);
    FillRect(hdc, &rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn round(hdc: HDC, rect: RECT, radius: i32, colour: COLORREF) {
    let brush = CreateSolidBrush(colour);
    let old_brush = SelectObject(hdc, brush);
    let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    // Без пера RoundRect/Ellipse не дорисовывают правый и нижний пиксель — добавляем.
    if radius >= rect.bottom - rect.top {
        let _ = Ellipse(hdc, rect.left, rect.top, rect.right + 1, rect.bottom + 1);
    } else {
        let _ = RoundRect(hdc, rect.left, rect.top, rect.right + 1, rect.bottom + 1, radius, radius);
    }
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
    let _ = DeleteObject(brush);
}

/// Рамка окна меню в тон (Windows 11; на Windows 10 вызов просто ничего не делает).
unsafe fn frame(menu_window: HWND) {
    let fresh = SESSION.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(s) = cell.as_mut() else { return false };
        !std::mem::replace(&mut s.framed, true)
    });
    if fresh {
        let _ = DwmSetWindowAttribute(
            menu_window,
            DWMWA_BORDER_COLOR,
            &BORDER as *const COLORREF as *const c_void,
            size_of::<COLORREF>() as u32,
        );
    }
}

fn paused(app: &AppHandle) -> bool {
    app.try_state::<Arc<SharedState>>()
        .is_some_and(|state| state.paused.load(Ordering::Relaxed))
}
