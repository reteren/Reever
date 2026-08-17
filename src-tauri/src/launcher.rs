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

/// Извлечь URL из интернет-ярлыка (.url — это обычный INI-файл):
///   [InternetShortcut]
///   URL=steam://rungameid/570
pub fn parse_url_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("URL=").map(|s| s.trim().to_string()))
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

/// Главная точка запуска для команды "Запусти".
pub fn launch(found: &Path) -> bool {
    let ext = found
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        // .url: достаём URL и запускаем именно его — так корректно ловятся протоколы.
        "url" => match parse_url_file(found) {
            Some(url) => shell_open(&url),
            None => shell_open(&found.to_string_lossy()),
        },
        // .lnk и .exe: ShellExecute по самому пути — Windows сам резолвит ярлык.
        _ => shell_open(&found.to_string_lossy()),
    }
}
