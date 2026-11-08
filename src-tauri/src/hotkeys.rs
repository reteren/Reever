//! Глобальные клавиши Windows: отдельный поток с message-only окном.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use crate::app::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW, TranslateMessage,
    HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_HOTKEY,
};

const RECONFIGURE: u32 = WM_APP + 1;
const STOP: u32 = WM_APP + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    modifiers: u32,
    vk: u32,
}

impl Shortcut {
    pub fn parse(text: &str) -> Result<Self, String> {
        let tokens: Vec<_> = text.split('+').map(str::trim).collect();
        if tokens.is_empty() || tokens.iter().any(|t| t.is_empty()) {
            return Err(format!("некорректная клавиша «{text}»"));
        }
        let mut modifiers = 0;
        for token in &tokens[..tokens.len() - 1] {
            let bit = match token.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" | "CMDORCTRL" | "CMDORCONTROL" | "COMMANDORCTRL"
                | "COMMANDORCONTROL" => MOD_CONTROL.0,
                "ALT" | "OPTION" => MOD_ALT.0,
                "SHIFT" => MOD_SHIFT.0,
                "SUPER" | "META" | "WIN" | "CMD" | "COMMAND" => MOD_WIN.0,
                _ => return Err(format!("неизвестный модификатор «{token}»")),
            };
            modifiers |= bit;
        }
        let key = tokens[tokens.len() - 1].to_ascii_uppercase();
        let vk = key_code(&key).ok_or_else(|| format!("неизвестная клавиша «{key}»"))?;
        Ok(Self { modifiers, vk })
    }
}

/// Имя клавиши по коду VK — для записи сочетания в настройках; `Shortcut::parse` понимает его
/// обратно. None — клавиша, которую зарегистрировать нельзя (модификаторы, Esc и т. п.).
pub fn key_name(vk: u32) -> Option<String> {
    Some(match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk)?.to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        _ => match vk {
            0x08 => "Backspace",
            0x09 => "Tab",
            0x0D => "Enter",
            0x13 => "Pause",
            0x14 => "CapsLock",
            0x20 => "Space",
            0x21 => "PageUp",
            0x22 => "PageDown",
            0x23 => "End",
            0x24 => "Home",
            0x25 => "Left",
            0x26 => "Up",
            0x27 => "Right",
            0x28 => "Down",
            0x2C => "PrintScreen",
            0x2D => "Insert",
            0x2E => "Delete",
            0x6A => "NumpadMultiply",
            0x6B => "NumpadAdd",
            0x6D => "NumpadSubtract",
            0x6E => "NumpadDecimal",
            0x6F => "NumpadDivide",
            0x90 => "NumLock",
            0x91 => "ScrollLock",
            0xAD => "VolumeMute",
            0xAE => "VolumeDown",
            0xAF => "VolumeUp",
            0xB0 => "MediaTrackNext",
            0xB1 => "MediaTrackPrev",
            0xB2 => "MediaStop",
            0xB3 => "MediaPlayPause",
            0xBA => ";",
            0xBB => "=",
            0xBC => ",",
            0xBD => "-",
            0xBE => ".",
            0xBF => "/",
            0xC0 => "`",
            0xDB => "[",
            0xDC => "\\",
            0xDD => "]",
            0xDE => "'",
            _ => return None,
        }
        .to_string(),
    })
}

fn key_code(key: &str) -> Option<u32> {
    let key = key
        .strip_prefix("KEY")
        .filter(|v| v.len() == 1)
        .unwrap_or(key);
    if key.len() == 1 {
        let c = key.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(digit) = key.strip_prefix("DIGIT") {
        if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() {
            return Some(digit.as_bytes()[0] as u32);
        }
    }
    if let Some(number) = key.strip_prefix('F').and_then(|s| s.parse::<u32>().ok()) {
        if (1..=24).contains(&number) {
            return Some(0x70 + number - 1);
        }
    }
    let numpad = key
        .strip_prefix("NUMPAD")
        .or_else(|| key.strip_prefix("NUM"));
    if let Some(number) = numpad.and_then(|s| s.parse::<u32>().ok()) {
        if number <= 9 {
            return Some(0x60 + number);
        }
    }
    Some(match key {
        "BACKQUOTE" | "`" => 0xC0,
        "BACKSLASH" | "\\" => 0xDC,
        "BRACKETLEFT" | "[" => 0xDB,
        "BRACKETRIGHT" | "]" => 0xDD,
        "COMMA" | "," => 0xBC,
        "EQUAL" | "=" => 0xBB,
        "MINUS" | "-" => 0xBD,
        "PERIOD" | "." => 0xBE,
        "QUOTE" | "'" => 0xDE,
        "SEMICOLON" | ";" => 0xBA,
        "SLASH" | "/" => 0xBF,
        "BACKSPACE" => 0x08,
        "TAB" => 0x09,
        "ENTER" | "NUMPADENTER" | "NUMENTER" => 0x0D,
        "SPACE" => 0x20,
        "CAPSLOCK" => 0x14,
        "ESCAPE" | "ESC" => 0x1B,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" | "ARROWLEFT" => 0x25,
        "UP" | "ARROWUP" => 0x26,
        "RIGHT" | "ARROWRIGHT" => 0x27,
        "DOWN" | "ARROWDOWN" => 0x28,
        "PRINTSCREEN" => 0x2C,
        "INSERT" => 0x2D,
        "DELETE" => 0x2E,
        "NUMLOCK" => 0x90,
        "SCROLLLOCK" => 0x91,
        "PAUSE" | "PAUSEBREAK" => 0x13,
        "NUMPADADD" | "NUMADD" | "NUMPADPLUS" | "NUMPLUS" => 0x6B,
        "NUMPADDECIMAL" | "NUMDECIMAL" => 0x6E,
        "NUMPADDIVIDE" | "NUMDIVIDE" => 0x6F,
        "NUMPADEQUAL" | "NUMEQUAL" => 0x45,
        "NUMPADMULTIPLY" | "NUMMULTIPLY" => 0x6A,
        "NUMPADSUBTRACT" | "NUMSUBTRACT" => 0x6D,
        "AUDIOVOLUMEDOWN" | "VOLUMEDOWN" => 0xAE,
        "AUDIOVOLUMEUP" | "VOLUMEUP" => 0xAF,
        "AUDIOVOLUMEMUTE" | "VOLUMEMUTE" => 0xAD,
        "MEDIAPLAY" => 0xFA,
        "MEDIAPAUSE" => 0xB3,
        "MEDIAPLAYPAUSE" => 0xB3,
        "MEDIASTOP" => 0xB2,
        "MEDIATRACKNEXT" => 0xB0,
        "MEDIATRACKPREV" | "MEDIATRACKPREVIOUS" => 0xB1,
        _ => return None,
    })
}

struct Request {
    shortcuts: [Option<Shortcut>; 2],
    reply: Sender<Result<(), String>>,
}

pub struct Hotkeys {
    hwnd: isize,
    requests: Sender<Request>,
}

impl Hotkeys {
    pub fn start(app: AppHandle) -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (requests, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("reever-hotkeys".into())
            .spawn(move || run_thread(app, receiver, ready_tx))
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("поток горячих клавиш не запустился: {e}"))??;
        Ok(Self { hwnd, requests })
    }

    pub fn configure(&self, mode: &str, main: &str, mute: &str) -> Result<(), String> {
        let parse = |text: &str| {
            if text.trim().is_empty() {
                Ok(None)
            } else {
                Shortcut::parse(text).map(Some)
            }
        };
        let shortcuts = [
            if mode == "wake" { None } else { parse(main)? },
            parse(mute)?,
        ];
        let (reply, result) = mpsc::channel();
        self.requests
            .send(Request { shortcuts, reply })
            .map_err(|e| e.to_string())?;
        unsafe { PostMessageW(HWND(self.hwnd as *mut _), RECONFIGURE, WPARAM(0), LPARAM(0)) }
            .map_err(|e| e.to_string())?;
        result
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        let _ = unsafe { PostMessageW(HWND(self.hwnd as *mut _), STOP, WPARAM(0), LPARAM(0)) };
    }
}

fn apply(
    hwnd: HWND,
    current: &mut [Option<Shortcut>; 2],
    next: [Option<Shortcut>; 2],
) -> Result<(), String> {
    for id in 0..2 {
        if current[id] != next[id] && current[id].is_some() {
            unsafe { UnregisterHotKey(hwnd, id as i32 + 1) }.map_err(|e| e.to_string())?;
        }
    }
    for id in 0..2 {
        if current[id] != next[id] {
            if let Some(shortcut) = next[id] {
                if let Err(error) = unsafe {
                    RegisterHotKey(
                        hwnd,
                        id as i32 + 1,
                        HOT_KEY_MODIFIERS(shortcut.modifiers | MOD_NOREPEAT.0),
                        shortcut.vk,
                    )
                } {
                    for rollback in 0..2 {
                        if current[rollback] != next[rollback] {
                            if next[rollback].is_some() {
                                let _ = unsafe { UnregisterHotKey(hwnd, rollback as i32 + 1) };
                            }
                            if let Some(old) = current[rollback] {
                                let _ = unsafe {
                                    RegisterHotKey(
                                        hwnd,
                                        rollback as i32 + 1,
                                        HOT_KEY_MODIFIERS(old.modifiers | MOD_NOREPEAT.0),
                                        old.vk,
                                    )
                                };
                            }
                        }
                    }
                    return Err(format!("клавиша занята или недоступна: {error}"));
                }
            }
        }
    }
    *current = next;
    Ok(())
}

fn run_thread(
    app: AppHandle,
    requests: Receiver<Request>,
    ready: mpsc::SyncSender<Result<isize, String>>,
) {
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Reever hotkeys"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            HINSTANCE(std::ptr::null_mut()),
            None,
        )
    };
    let Ok(hwnd) = hwnd else {
        let _ = ready.send(Err(hwnd.unwrap_err().to_string()));
        return;
    };
    let _ = ready.send(Ok(hwnd.0 as isize));
    let mut current = [None, None];
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        match msg.message {
            RECONFIGURE => {
                while let Ok(request) = requests.try_recv() {
                    let _ = request
                        .reply
                        .send(apply(hwnd, &mut current, request.shortcuts));
                }
            }
            WM_HOTKEY => {
                let Some(state) = app.try_state::<std::sync::Arc<crate::actions::SharedState>>()
                else {
                    continue;
                };
                if msg.wParam.0 == 2 {
                    let now = !state.paused.load(std::sync::atomic::Ordering::Relaxed);
                    state
                        .paused
                        .store(now, std::sync::atomic::Ordering::Relaxed);
                    crate::events::set_state(&app, if now { "paused" } else { "idle" });
                    crate::events::ui_log(
                        &app,
                        if now {
                            crate::msg!("log.mic.mutedHotkey")
                        } else {
                            crate::msg!("log.mic.onHotkey")
                        },
                    );
                } else if msg.wParam.0 == 1 {
                    crate::show_main(&app);
                    state
                        .force_listen
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            STOP => break,
            _ => unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            },
        }
    }
    for id in 1..=2 {
        if current[id - 1].is_some() {
            let _ = unsafe { UnregisterHotKey(hwnd, id as i32) };
        }
    }
    let _ = unsafe { DestroyWindow(hwnd) };
}

#[cfg(test)]
mod tests {
    use super::Shortcut;
    #[test]
    fn parses_settings_and_picker_keys() {
        assert_eq!(Shortcut::parse("Ctrl+Shift+G").unwrap().vk, b'G' as u32);
        for key in [
            "A", "9", "F1", "F12", "F24", "Space", "Enter", "Tab", "Up", "Down", "Left", "Right",
        ] {
            Shortcut::parse(&format!("Ctrl+{key}")).unwrap();
        }
        assert_eq!(Shortcut::parse("Super+Alt+ArrowLeft").unwrap().vk, 0x25);
        assert_eq!(Shortcut::parse("CmdOrCtrl+Digit3").unwrap().vk, b'3' as u32);
        assert!(Shortcut::parse("Ctrl+").is_err());
        assert!(Shortcut::parse("Ctrl+G+Shift").is_err());
        assert!(Shortcut::parse("Ctrl+F25").is_err());
    }

    #[test]
    #[ignore = "регистрирует клавиши в системе на миг"]
    fn single_keys_register_without_a_modifier() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
        for (i, name) in ["PageDown", "Home", "Pause", "F9"].iter().enumerate() {
            let s = Shortcut::parse(name).unwrap();
            let id = 0x7000 + i as i32;
            unsafe { RegisterHotKey(None, id, HOT_KEY_MODIFIERS(s.modifiers | MOD_NOREPEAT.0), s.vk) }.unwrap_or_else(|e| panic!("{name}: {e}"));
            unsafe { UnregisterHotKey(None, id) }.unwrap();
        }
    }

    #[test]
    fn every_recordable_key_parses_back_without_a_modifier() {
        let mut n = 0;
        for vk in 0..=0xFF {
            if let Some(name) = super::key_name(vk) {
                let parsed = Shortcut::parse(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(parsed.vk, vk, "{name}");
                assert_eq!(parsed.modifiers, 0, "{name}");
                n += 1;
            }
        }
        assert!(n > 90, "{n}");
        for name in ["PageUp", "PageDown", "Home", "End", "Insert", "Delete", "Numpad5"] {
            assert!(Shortcut::parse(name).is_ok(), "{name}");
        }
        assert_eq!(Shortcut::parse("Shift+PageDown").unwrap().vk, 0x22);
    }
}
