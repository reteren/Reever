//! Автозапуск Windows с тем же значением реестра, которое создавал auto-launch.

use tauri::AppHandle;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_SZ,
};

const RUN: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
const ENABLED: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(
    path: &str,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<HKEY, String> {
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wide(path).as_ptr()),
            0,
            access,
            &mut key,
        )
    }
    .ok()
    .map_err(|e| format!("не удалось открыть реестр автозапуска: {e}"))?;
    Ok(key)
}

/// Запись совместима с tauri-plugin-autostart/auto-launch: имя пакета и путь без кавычек.
pub fn set(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let command = if enabled {
        let path = std::env::current_exe().map_err(|e| e.to_string())?;
        Some(format!("{} --hidden", path.display()))
    } else {
        None
    };
    set_named(&app.package_info().name, command.as_deref())
}

fn set_named(name: &str, command: Option<&str>) -> Result<(), String> {
    let value = wide(name);
    let command = command.map(wide);
    let key = open(RUN, KEY_SET_VALUE)?;
    let result = if let Some(command) = command.as_ref() {
        let bytes =
            unsafe { std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2) };
        unsafe { RegSetValueExW(key, PCWSTR(value.as_ptr()), 0, REG_SZ, Some(bytes)) }
    } else {
        unsafe { RegDeleteValueW(key, PCWSTR(value.as_ptr())) }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
        .ok()
        .map_err(|e| format!("не удалось изменить автозапуск: {e}"))?;

    // auto-launch сбрасывает отметку отключения в диспетчере задач при включении.
    if command.is_some() {
        if let Ok(key) = open(APPROVED, KEY_SET_VALUE) {
            let result = unsafe {
                RegSetValueExW(key, PCWSTR(value.as_ptr()), 0, REG_BINARY, Some(&ENABLED))
            };
            unsafe {
                let _ = RegCloseKey(key);
            }
            result
                .ok()
                .map_err(|e| format!("не удалось разрешить автозапуск: {e}"))?;
        }
    }
    Ok(())
}

pub fn is_enabled(app: &AppHandle) -> bool {
    is_enabled_named(&app.package_info().name)
}

fn is_enabled_named(name: &str) -> bool {
    let value = wide(name);
    let Ok(key) = open(RUN, KEY_QUERY_VALUE) else {
        return false;
    };
    let exists =
        unsafe { RegQueryValueExW(key, PCWSTR(value.as_ptr()), None, None, None, None) }.is_ok();
    unsafe {
        let _ = RegCloseKey(key);
    }
    if !exists {
        return false;
    }

    let Ok(key) = open(APPROVED, KEY_QUERY_VALUE) else {
        return true;
    };
    let mut bytes = [0u8; 256];
    let mut size = bytes.len() as u32;
    let result = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value.as_ptr()),
            None,
            None,
            Some(bytes.as_mut_ptr()),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if result == ERROR_FILE_NOT_FOUND || !result.is_ok() {
        return true;
    }
    size >= 8 && bytes[..size as usize].iter().rev().take(8).all(|&v| v == 0)
}

#[cfg(test)]
mod tests {
    use super::{is_enabled_named, set_named};

    /// Отдельная живая проверка: пишет реальный Run и возвращает его в исходное состояние.
    #[test]
    #[ignore]
    fn registry_round_trip() {
        const NAME: &str = "Reever";
        assert!(
            !is_enabled_named(NAME),
            "значение Reever уже существует; проверку нельзя запускать"
        );
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = set_named("Reever", None);
            }
        }
        let _restore = Restore;
        let command = format!("{} --hidden", std::env::current_exe().unwrap().display());
        set_named(NAME, Some(&command)).unwrap();
        let query = std::process::Command::new("reg")
            .args([
                "query",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                NAME,
            ])
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&query.stdout);
        println!("{output}");
        assert!(query.status.success());
        assert!(output.contains(&command));
        assert!(is_enabled_named(NAME));
        set_named(NAME, None).unwrap();
        assert!(!is_enabled_named(NAME));
    }
}
