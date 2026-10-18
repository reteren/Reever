use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Запуск UWP/Store-приложения по AUMID через shell:AppsFolder.
pub fn launch_uwp(aumid: &str) -> bool {
    Command::new("explorer")
        .arg(format!("shell:AppsFolder\\{aumid}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok()
}

/// УНИВЕРСАЛЬНЫЙ запуск. ShellExecuteW("open") сам разруливает:
///  - .exe              → запуск с корректным рабочим каталогом
///  - .lnk              → резолвит цель ярлыка и запускает её (как двойной клик)
///  - .url              → открывает (в т.ч. кастомные протоколы)
///  - steam://rungameid/570, ms-settings:, http(s):// и пр.
/// Возвращает true при успехе (HINSTANCE > 32 — легаси-контракт WinAPI).
pub fn shell_open(target: &str) -> bool {
    let verb = HSTRING::from("open");
    let file = HSTRING::from(target);
    let result = unsafe {
        ShellExecuteW(
            None,
            &verb,
            &file,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    result.0 as isize > 32
}

/// Извлечь цель из бинарного .lnk (через крейт `lnk`). Нужно когда цель надо
/// ПРОИНСПЕКТИРОВАТЬ/преобразовать. Для запуска достаточно shell_open() по .lnk.
#[allow(dead_code)] // публичный helper для инспекции ярлыков; запуск идёт через shell_open
pub fn parse_lnk_target(path: &Path) -> Option<String> {
    let link = lnk::ShellLink::open(path).ok()?;
    if let Some(info) = link.link_info() {
        if let Some(base) = info.local_base_path() {
            return Some(base.clone());
        }
    }
    link.relative_path().clone()
}

/// Открыть папку в проводнике.
pub fn open_folder(dir: &Path) -> bool {
    Command::new("explorer")
        .arg(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok()
}
