//! Ввод продиктованного текста: есть ли у активного окна поле с курсором, печать в него
//! (`SendInput`, Unicode) и запасной путь — буфер обмена.

use std::{ffi::c_void, mem::size_of, thread::sleep, time::Duration};

use anyhow::{bail, Context};
use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HINSTANCE, HWND},
    System::{
        Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED},
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        LibraryLoader::GetModuleHandleW,
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        Threading::GetCurrentProcessId,
    },
    UI::{
        Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_DocumentControlTypeId,
            UIA_EditControlTypeId, UIA_ValuePatternId,
        },
        Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_RETURN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CreateWindowExW, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
            GetWindowLongPtrW, GetWindowThreadProcessId, GA_ROOT, GUITHREADINFO, GWL_STYLE,
            HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
        },
    },
};

use crate::dictation::Piece;

/// Короткая пауза даёт целевому окну обработать символ до следующей проверки фокуса.
const CHAR_PAUSE: Duration = Duration::from_millis(10);
const CF_UNICODETEXT: u32 = 13;
const ES_READONLY_STYLE: u32 = 0x0800;

thread_local! {
    /// `SetClipboardData` требует owner HWND. Message-only окно невидимо и
    /// принадлежит этому потоку, поэтому clipboard ownership не остаётся NULL.
    static CLIPBOARD_OWNER: HWND = create_clipboard_owner();
}

fn create_clipboard_owner() -> HWND {
    let instance = unsafe {
        GetModuleHandleW(windows::core::PCWSTR::null())
            .map(|module| HINSTANCE(module.0))
            .unwrap_or_default()
    };
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            windows::core::w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            instance,
            None,
        )
        .unwrap_or_default()
    }
}

/// Окна оболочки, у которых «фокус» есть, а поля для текста нет.
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// Активное окно, если в нём фокус на поле ввода (виден текстовый курсор или UI Automation
/// говорит, что в фокусе редактируемое поле). Своё окно Reever — не цель.
pub fn text_target() -> Option<isize> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    let thread = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return None;
    }
    if SHELL_CLASSES.contains(&class_name(foreground).as_str()) {
        return None;
    }

    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let has_caret =
        unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok() && !info.hwndCaret.0.is_null();
    if has_caret && native_edit_is_read_only(info.hwndFocus) {
        return None;
    }
    match focused_element_editable(pid) {
        Some(true) => Some(foreground.0 as isize),
        Some(false) => None,
        None => has_caret.then_some(foreground.0 as isize),
    }
}

/// UI Automation может быть недоступна в потоке приложения, поэтому дополнительно
/// проверяем штатный ES_READONLY у нативных EDIT/RichEdit, если там стоит курсор.
fn native_edit_is_read_only(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let class = class_name(hwnd);
    if class != "Edit" && !class.starts_with("RichEdit") {
        return false;
    }
    (unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32) & ES_READONLY_STYLE != 0
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

thread_local! {
    static AUTOMATION: Option<IUIAutomation> = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
    };
}

/// Браузеры и Electron не рисуют системный курсор — у них поле видно только через UIA:
/// в фокусе Edit или Document, и если у него есть ValuePattern, то не только для чтения
/// (так страница без активного поля в браузере — не цель, а документ Word — цель).
fn focused_element_editable(foreground_pid: u32) -> Option<bool> {
    AUTOMATION.with(|automation| {
        let Some(automation) = automation else {
            return None;
        };
        unsafe {
            let Ok(element) = automation.GetFocusedElement() else {
                return None;
            };
            if element.CurrentProcessId().map(|pid| pid as u32).ok()? != foreground_pid {
                return None;
            }
            let control_type = element.CurrentControlType().ok()?;
            if control_type != UIA_EditControlTypeId && control_type != UIA_DocumentControlTypeId {
                return Some(false);
            }
            match element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                Ok(value) => value.CurrentIsReadOnly().ok().map(|ro| !ro.as_bool()),
                Err(_) => Some(true),
            }
        }
    })
}

/// Префикс, который удалось напечатать, и точный остаток для буфера обмена.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeProgress {
    pub typed: Vec<Piece>,
    pub remaining: Vec<Piece>,
}

/// Напечатать куски только пока активно исходное поле. Снимок `hwndFocus` защищает
/// от смены поля даже внутри того же верхнего окна приложения.
pub fn type_pieces(target: isize, pieces: &[Piece]) -> TypeProgress {
    let Ok(focus) = current_focus(target) else {
        return TypeProgress {
            typed: Vec::new(),
            remaining: pieces.to_vec(),
        };
    };
    let mut progress = TypeProgress::default();

    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Text(text) => {
                let typed_chars = type_text(target, focus, text);
                let total_chars = text.chars().count();
                if typed_chars < total_chars {
                    if typed_chars > 0 {
                        let split = text
                            .char_indices()
                            .nth(typed_chars)
                            .map(|(index, _)| index)
                            .unwrap_or(text.len());
                        progress.typed.push(Piece::Text(text[..split].to_string()));
                        if split < text.len() {
                            progress
                                .remaining
                                .push(Piece::Text(text[split..].to_string()));
                        }
                    } else {
                        progress.remaining.push(piece.clone());
                    }
                    progress.remaining.extend_from_slice(&pieces[index + 1..]);
                    return progress;
                }
                progress.typed.push(piece.clone());
            }
            Piece::NewLine => {
                if ensure_target(target, focus).is_err() {
                    progress.remaining.extend_from_slice(&pieces[index..]);
                    return progress;
                }
                let sent = send_inputs(&shift_enter()) as usize;
                if sent < 4 {
                    release_shift_enter();
                }
                if sent >= 2 {
                    progress.typed.push(Piece::NewLine);
                } else {
                    progress.remaining.push(Piece::NewLine);
                }
                if sent < 4 {
                    progress.remaining.extend_from_slice(&pieces[index + 1..]);
                    return progress;
                }
            }
        }
    }
    progress
}

fn type_text(target: isize, focus: HWND, text: &str) -> usize {
    let mut typed_chars = 0;
    for character in text.chars() {
        if ensure_target(target, focus).is_err() {
            return typed_chars;
        }
        // Не разрываем суррогатную пару на границе отдельных проверок фокуса.
        let mut encoded = [0u16; 2];
        let units = character.encode_utf16(&mut encoded);
        let mut inputs = Vec::with_capacity(units.len() * 2);
        for &unit in units.iter() {
            inputs.push(key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
            inputs.push(key(
                VIRTUAL_KEY(0),
                unit,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ));
        }
        let sent = (send_inputs(&inputs) as usize).min(inputs.len());
        typed_chars += chars_after_input_events(units, sent);
        if sent < inputs.len() {
            if sent % 2 == 1 {
                let unit = units[sent / 2];
                let key_up = key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
                let _ = send_inputs(&[key_up]);
            }
            return typed_chars;
        }
        sleep(CHAR_PAUSE);
    }
    typed_chars
}

fn chars_after_input_events(units: &[u16], sent_events: usize) -> usize {
    let mut unit_index = 0;
    let mut chars = 0;
    for decoded in char::decode_utf16(units.iter().copied()) {
        let width = decoded
            .as_ref()
            .map_or(1, |character| character.len_utf16());
        let key_down_count = 2 * (unit_index + width - 1) + 1;
        if sent_events < key_down_count {
            break;
        }
        chars += 1;
        unit_index += width;
    }
    chars
}

fn current_focus(target: isize) -> anyhow::Result<HWND> {
    ensure_foreground_window(target)?;
    let thread = unsafe { GetWindowThreadProcessId(hwnd_from(target), None) };
    if thread == 0 {
        bail!("не удалось определить поток целевого окна");
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.context("не удалось прочитать фокус")?;
    if info.hwndFocus.0.is_null() {
        bail!("в целевом окне нет сфокусированного поля");
    }
    Ok(info.hwndFocus)
}

fn ensure_target(target: isize, expected_focus: HWND) -> anyhow::Result<()> {
    ensure_foreground_window(target)?;
    let thread = unsafe { GetWindowThreadProcessId(hwnd_from(target), None) };
    if thread == 0 {
        bail!("не удалось определить поток целевого окна");
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.context("не удалось проверить поле ввода")?;
    if info.hwndFocus.0 != expected_focus.0 {
        bail!("фокусное поле сменилось — ввод остановлен");
    }
    Ok(())
}

fn ensure_foreground_window(target: isize) -> anyhow::Result<()> {
    let foreground = unsafe { GetForegroundWindow() };
    let root = unsafe { GetAncestor(foreground, GA_ROOT) };
    if foreground.0 as isize == target || root.0 as isize == target {
        Ok(())
    } else {
        bail!("активное окно сменилось — ввод остановлен")
    }
}

fn shift_enter() -> [INPUT; 4] {
    [
        key(VK_SHIFT, 0, KEYBD_EVENT_FLAGS(0)),
        key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)),
        key(VK_RETURN, 0, KEYEVENTF_KEYUP),
        key(VK_SHIFT, 0, KEYEVENTF_KEYUP),
    ]
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn release_shift_enter() {
    let releases = [
        key(VK_RETURN, 0, KEYEVENTF_KEYUP),
        key(VK_SHIFT, 0, KEYEVENTF_KEYUP),
    ];
    let _ = send_inputs(&releases);
}

fn send_inputs(inputs: &[INPUT]) -> u32 {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) }
}

/// Положить текст в буфер обмена (заменяя прежнее содержимое).
pub fn set_clipboard(text: &str) -> anyhow::Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let owner = CLIPBOARD_OWNER.with(|owner| *owner);
    if owner.0.is_null() {
        bail!("не удалось создать владельца буфера обмена");
    }
    unsafe {
        // Буфер бывает занят другой программой — несколько коротких попыток.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(owner).is_ok() {
                opened = true;
                break;
            }
            sleep(Duration::from_millis(20));
        }
        if !opened {
            bail!("буфер обмена занят другой программой");
        }
        let result = (|| -> anyhow::Result<()> {
            EmptyClipboard().context("не удалось очистить буфер обмена")?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)
                .context("не удалось выделить память под буфер обмена")?;
            let ptr = GlobalLock(memory) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(memory);
                bail!("не удалось заблокировать память буфера обмена");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(memory);
            if let Err(error) = SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0 as *mut c_void)) {
                let _ = GlobalFree(memory);
                return Err(error).context("не удалось записать текст в буфер обмена");
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
