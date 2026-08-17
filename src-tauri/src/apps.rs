//! Перечисление установленных приложений (вкл. UWP/Store) через Get-StartApps.
use std::os::windows::process::CommandExt;
use std::process::Command;

use serde::{Deserialize, Serialize};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Serialize)]
pub struct AppEntry {
    pub name: String,
    pub aumid: String,
}

#[derive(Deserialize)]
struct RawApp {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "AppID", default)]
    app_id: String,
}

/// Список приложений из меню «Пуск» (десктоп + UWP): имя + AUMID/AppID.
pub fn list_start_apps() -> Vec<AppEntry> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Get-StartApps | ConvertTo-Json -Compress",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let Ok(out) = out else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);

    // Get-StartApps -> массив; для одного элемента PowerShell вернёт объект.
    let raws: Vec<RawApp> = serde_json::from_str::<Vec<RawApp>>(&text)
        .or_else(|_| serde_json::from_str::<RawApp>(&text).map(|r| vec![r]))
        .unwrap_or_default();

    raws.into_iter()
        .filter(|r| !r.app_id.is_empty())
        .map(|r| AppEntry { name: r.name, aumid: r.app_id })
        .collect()
}
