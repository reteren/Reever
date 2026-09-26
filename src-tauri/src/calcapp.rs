//! Windows Calculator launcher and guarded keyboard input.
//! For parenthesized expressions it sends Alt+2 and checks that the input was
//! delivered while Calculator kept focus; it does not inspect the selected view
//! through UI Automation.

use std::{
    ffi::c_void,
    mem::size_of,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use windows::Win32::{
    Foundation::{BOOL, HWND, TRUE},
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::{
        Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_ESCAPE, VK_MENU,
        },
        WindowsAndMessaging::{
            BringWindowToTop, GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
            IsWindow, SetForegroundWindow, ShowWindow, GA_ROOT, SW_RESTORE,
        },
    },
};

use crate::{launcher, procs};

const CALCULATOR_AUMID: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
const WINDOW_WAIT: Duration = Duration::from_secs(3);
const KEY_DELAY: Duration = Duration::from_millis(20);

/// Открыть калькулятор, сбросить прежний ввод и набрать `keys` (`33*78=`).
/// Ошибка — если окно калькулятора не появилось или не удалось сделать его активным:
/// вводить нажатия в чужое окно нельзя ни при каких условиях.
pub fn open_and_type(keys: &str) -> anyhow::Result<()> {
    let window = match find_calculator_window() {
        Some(window) => window,
        None => {
            if !launcher::launch_uwp(CALCULATOR_AUMID) && !launcher::shell_open("calc.exe") {
                bail!("не удалось запустить калькулятор Windows");
            }
            wait_for_calculator_window().context("окно калькулятора не появилось за 3 секунды")?
        }
    };

    activate_calculator(window)?;

    // Alt+2 is the Calculator shortcut for its engineering/scientific view.
    // Leave the user's current view untouched for ordinary arithmetic input.
    if keys.contains('(') || keys.contains(')') {
        ensure_calculator_foreground(window)?;
        send_engineering_shortcut()?;
        sleep(Duration::from_millis(80));
        ensure_calculator_foreground(window)?;
    }

    ensure_calculator_foreground(window)?;
    send_virtual_key(VK_ESCAPE).context("не удалось сбросить ввод калькулятора")?;
    sleep(KEY_DELAY);

    for ch in keys.chars() {
        ensure_calculator_foreground(window)
            .with_context(|| format!("фокус покинул калькулятор перед вводом {ch:?}"))?;

        // Enter is the reliable equivalent of '=' across calculator layouts.
        if ch == '=' {
            send_virtual_key(windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN)
                .context("не удалось отправить Enter в калькулятор")?;
        } else {
            send_unicode_char(ch).with_context(|| format!("не удалось ввести символ {ch:?}"))?;
        }
        sleep(KEY_DELAY);
    }

    ensure_calculator_foreground(window)?;
    Ok(())
}

fn find_calculator_window() -> Option<HWND> {
    procs::running_apps()
        .into_iter()
        .find(is_calculator)
        .and_then(|app| app.windows.into_iter().find(|raw| *raw != 0))
        .map(hwnd_from)
}

fn is_calculator(app: &procs::RunningApp) -> bool {
    let exe = app.exe_name.trim().to_ascii_lowercase();
    if matches!(exe.as_str(), "calculatorapp" | "calculator" | "calc") {
        return true;
    }

    is_calculator_label(&app.description)
        || app.titles.iter().any(|title| is_calculator_label(title))
}

fn is_calculator_label(label: &str) -> bool {
    let label = label.trim();
    label.eq_ignore_ascii_case("calculator")
        || label.eq_ignore_ascii_case("калькулятор")
        || label.to_ascii_lowercase().contains("windows calculator")
}

fn wait_for_calculator_window() -> anyhow::Result<HWND> {
    let deadline = Instant::now() + WINDOW_WAIT;
    loop {
        if let Some(window) = find_calculator_window() {
            return Ok(window);
        }
        if Instant::now() >= deadline {
            bail!("калькулятор не создал видимое окно за 3 секунды");
        }
        sleep(Duration::from_millis(100));
    }
}

fn activate_calculator(window: HWND) -> anyhow::Result<()> {
    if !unsafe { IsWindow(window) }.as_bool() {
        bail!("окно калькулятора уже закрыто");
    }

    for _ in 0..8 {
        unsafe {
            if IsIconic(window).as_bool() {
                let _ = ShowWindow(window, SW_RESTORE);
            }

            let foreground = GetForegroundWindow();
            let foreground_thread = GetWindowThreadProcessId(foreground, None);
            let current_thread = GetCurrentThreadId();
            let attached = foreground_thread != 0
                && foreground_thread != current_thread
                && AttachThreadInput(current_thread, foreground_thread, TRUE).as_bool();

            let _ = BringWindowToTop(window);
            let _ = SetForegroundWindow(window);

            if attached {
                let _ = AttachThreadInput(current_thread, foreground_thread, BOOL(0));
            }
        }

        sleep(Duration::from_millis(25));
        if ensure_calculator_foreground(window).is_ok() {
            return Ok(());
        }
    }

    bail!("Windows не передал фокус калькулятору; ввод отменён")
}

fn ensure_calculator_foreground(window: HWND) -> anyhow::Result<()> {
    let foreground = unsafe { GetForegroundWindow() };
    let root = unsafe { GetAncestor(foreground, GA_ROOT) };
    if foreground == window || root == window {
        Ok(())
    } else {
        bail!("активное окно изменилось; ввод в калькулятор остановлен")
    }
}

fn send_engineering_shortcut() -> anyhow::Result<()> {
    let two = VIRTUAL_KEY(b'2' as u16);
    let inputs = [
        keyboard_input(VK_MENU, 0, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(two, 0, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(two, 0, KEYEVENTF_KEYUP),
        keyboard_input(VK_MENU, 0, KEYEVENTF_KEYUP),
    ];
    send_inputs(&inputs).context("не удалось переключить калькулятор в инженерный режим")
}

fn send_virtual_key(key: VIRTUAL_KEY) -> anyhow::Result<()> {
    let inputs = [
        keyboard_input(key, 0, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(key, 0, KEYEVENTF_KEYUP),
    ];
    send_inputs(&inputs)
}

fn send_unicode_char(ch: char) -> anyhow::Result<()> {
    let mut inputs = Vec::with_capacity(ch.len_utf16() * 2);
    for unit in ch.encode_utf16(&mut [0; 2]).iter().copied() {
        inputs.push(keyboard_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
        inputs.push(keyboard_input(
            VIRTUAL_KEY(0),
            unit,
            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
        ));
    }
    send_inputs(&inputs)
}

fn keyboard_input(key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> anyhow::Result<()> {
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        bail!("SendInput передал {sent} из {} нажатий", inputs.len());
    }
    Ok(())
}

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}
