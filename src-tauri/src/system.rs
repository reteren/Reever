//! Системные и медиа-команды.
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK,
    VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Опасные команды требуют подтверждения.
pub fn is_dangerous(op: &str) -> bool {
    matches!(op, "shutdown" | "restart")
}

pub fn run_system(op: &str) -> bool {
    match op {
        "shutdown" => run("shutdown", &["/s", "/t", "0"]),
        "restart" => run("shutdown", &["/r", "/t", "0"]),
        "sleep" => run("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
        "lock" => run("rundll32.exe", &["user32.dll,LockWorkStation"]),
        "volume_up" => tap(VK_VOLUME_UP),
        "volume_down" => tap(VK_VOLUME_DOWN),
        "mute" => tap(VK_VOLUME_MUTE),
        "media_play_pause" => tap(VK_MEDIA_PLAY_PAUSE),
        "media_next" => tap(VK_MEDIA_NEXT_TRACK),
        "media_prev" => tap(VK_MEDIA_PREV_TRACK),
        _ => false,
    }
}

fn run(exe: &str, args: &[&str]) -> bool {
    Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok()
}

/// Эмуляция нажатия медиа/громкость-клавиши.
fn tap(vk: VIRTUAL_KEY) -> bool {
    unsafe {
        keybd_event(vk.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(vk.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    }
    true
}
