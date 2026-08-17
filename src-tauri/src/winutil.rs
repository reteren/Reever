//! Оконные утилиты + разворот env-переменных (WinAPI).
use core::ffi::c_void;

use strsim::jaro_winkler;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// Развернуть %ENV% в строке (полная поддержка через WinAPI).
pub fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return collapse_double_root(s.to_string());
    }
    let src: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let expanded = unsafe {
        let needed = ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), None);
        if needed == 0 {
            return collapse_double_root(s.to_string());
        }
        let mut buf = vec![0u16; needed as usize];
        let written = ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf));
        if written == 0 {
            return collapse_double_root(s.to_string());
        }
        let len = (written as usize).saturating_sub(1);
        String::from_utf16_lossy(&buf[..len])
    };
    collapse_double_root(expanded)
}

/// Чинит задвоенный корень пути: если после раскрытия в СЕРЕДИНЕ строки оказался
/// ещё один абсолютный корень `X:\` (напр. `C:\Users\%USERPROFILE%\…` ->
/// `C:\Users\C:\Users\me\…`), берём путь от последнего такого корня.
fn collapse_double_root(p: String) -> String {
    let b = p.as_bytes();
    let mut last = 0usize;
    let mut i = 0usize;
    while i + 2 < b.len() {
        // корень диска `X:\` — одна буква, перед ней начало строки или разделитель
        // (так `steam://`, `https://` не считаются: перед ':' там буква, не '\').
        let boundary = i == 0 || b[i - 1] == b'\\' || b[i - 1] == b'/';
        if boundary
            && b[i].is_ascii_alphabetic()
            && b[i + 1] == b':'
            && (b[i + 2] == b'\\' || b[i + 2] == b'/')
        {
            if i > 0 {
                last = i; // встретили вложенный корень — запоминаем его начало
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    if last > 0 {
        p[last..].to_string()
    } else {
        p
    }
}

fn hwnd_from(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

unsafe extern "system" fn collect_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
        let v = &mut *(lparam.0 as *mut Vec<isize>);
        v.push(hwnd.0 as isize);
    }
    TRUE
}

/// Список видимых top-level окон с заголовком.
pub fn snapshot() -> Vec<isize> {
    let mut v: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_proc), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Поднять на передний план окно, появившееся после snapshot. `hint` — для выбора
/// нужного среди нескольких новых (фаззи по заголовку). Best-effort.
pub fn raise_new(before: &[isize], hint: &str) {
    let after = snapshot();
    let news: Vec<isize> = after.into_iter().filter(|h| !before.contains(h)).collect();
    if news.is_empty() {
        return;
    }
    let hint_l = hint.to_lowercase();
    let pick = news
        .iter()
        .copied()
        .max_by(|&a, &b| {
            let sa = jaro_winkler(&hint_l, &window_title(hwnd_from(a)).to_lowercase());
            let sb = jaro_winkler(&hint_l, &window_title(hwnd_from(b)).to_lowercase());
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or_else(|| news[news.len() - 1]);
    bring_to_front(hwnd_from(pick));
}

fn bring_to_front(hwnd: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let cur = GetCurrentThreadId();
        let _ = AttachThreadInput(cur, fg_thread, TRUE);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = AttachThreadInput(cur, fg_thread, BOOL(0));
    }
}
