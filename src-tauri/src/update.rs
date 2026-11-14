//! Автообновление через релизы GitHub.
//!
//! При каждом запуске (через `FIRST_CHECK`) и дальше раз в `CHECK_EVERY` Reever спрашивает
//! у GitHub последний релиз (`releases/latest`: без черновиков и пре-релизов). Если его версия
//! новее, установщик `Reever_<версия>_x64-setup.exe` скачивается в
//! `%LOCALAPPDATA%\com.troid.reever\update` (с докачкой) и сверяется с размером и SHA-256,
//! которые отдаёт GitHub. Затем в углу экрана выезжает уведомление (dialogs_native/update.rs):
//! «Обновить» ставит сразу, а с галочкой «Обновлять автоматически» версия ставится и сама,
//! когда Reever ничем не занят: окно закрыто, распознавание в покое, клавиатурой и мышью
//! давно не пользовались. Установщик (`/S /UPDATE /R`) сам закрывает Reever, не трогает
//! ярлыки и языковые пакеты и запускает новую версию.
//!
//! Сеть — через `curl.exe` из Windows, как и в установщике. Офлайн-режим, dev- и тестовая
//! сборка, копия без `uninstall.exe` рядом — без обновлений. `REEVER_DIALOG_DEMO=1
//! REEVER_UPDATE_DEMO=1` — показать уведомление с выдуманной версией рядом с установленным
//! Reever (снимок в `REEVER_UPDATE_SHOTS`, `REEVER_UPDATE_DEMO_MANUAL=1` — без автообновления).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::app::AppHandle;
use crate::config::LiveConfig;
use crate::events::ui_log;

const LATEST: &str = "https://api.github.com/repos/reteren/Reever/releases/latest";
/// Первая проверка после запуска: сеть после входа в Windows поднимается не сразу.
const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Как часто смотреть, не пора ли поставить уже скачанное.
const IDLE_POLL: Duration = Duration::from_secs(30);
/// Сколько без клавиатуры и мыши считается «не пользуются».
const USER_IDLE: Duration = Duration::from_secs(5 * 60);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>`; у старых загрузок GitHub его может не быть.
    #[serde(default)]
    digest: Option<String>,
}

/// Скачанный и проверенный установщик новой версии.
#[derive(Clone)]
struct Staged {
    version: String,
    path: PathBuf,
}

static STAGED: Mutex<Option<Staged>> = Mutex::new(None);

pub fn spawn(app: &AppHandle) {
    if std::env::var_os("REEVER_UPDATE_DEMO").is_some() {
        demo(app);
        return;
    }
    if cfg!(debug_assertions) || !installed_copy() {
        return;
    }
    let Ok(dir) = app.path().app_local_data_dir().map(|d| d.join("update")) else {
        return;
    };
    remove_stale(&dir);
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("reever-update".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK);
            let mut next_check = Instant::now();
            loop {
                let (online, auto) = settings(&app);
                if online && staged().is_none() && Instant::now() >= next_check {
                    next_check = Instant::now() + CHECK_EVERY;
                    match fetch_newer(&dir) {
                        Ok(Some(ready)) => {
                            ui_log(
                                &app,
                                crate::msg!("log.update.ready", version = ready.version),
                            );
                            let version = ready.version.clone();
                            if let Ok(mut s) = STAGED.lock() {
                                *s = Some(ready);
                            }
                            notify(&app, version, auto);
                        }
                        Ok(None) => {}
                        Err(error) => ui_log(&app, crate::msg!("log.update.failed", error = error)),
                    }
                }
                if online && auto && staged().is_some() && quiet() {
                    install_staged(&app);
                }
                std::thread::sleep(IDLE_POLL);
            }
        });
}

fn staged() -> Option<Staged> {
    STAGED.lock().ok().and_then(|s| s.clone())
}

/// Уведомление в углу: «Обновить» — поставить сейчас.
fn notify(app: &AppHandle, version: String, auto: bool) {
    let handle = app.clone();
    crate::dialogs_native::update::show(
        app,
        version,
        auto,
        Arc::new(move |now| {
            crate::dialogs_native::update::hide(&handle);
            if now {
                install_staged(&handle);
            }
        }),
    );
}

/// Поставить скачанную версию: установщик закроет Reever и запустит новую.
fn install_staged(app: &AppHandle) {
    let Some(ready) = STAGED.lock().ok().and_then(|mut s| s.take()) else {
        return;
    };
    ui_log(
        app,
        crate::msg!("log.update.installing", version = ready.version),
    );
    match install(&ready.path, !crate::events::main_visible()) {
        Ok(()) => {
            crate::traymenu::remove_icon();
            app.exit(0);
        }
        Err(error) => ui_log(app, crate::msg!("log.update.failed", error = error)),
    }
}

/// Проверка уведомления без сети: выдуманная версия, снимок окна, ответ — в stderr.
fn demo(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let auto = std::env::var_os("REEVER_UPDATE_DEMO_MANUAL").is_none();
        crate::dialogs_native::update::show(
            &app,
            "0.9.0".into(),
            auto,
            Arc::new(|now| {
                eprintln!(
                    "[update-demo] answer: {}",
                    if now { "now" } else { "later" }
                )
            }),
        );
        std::thread::sleep(Duration::from_millis(1500));
        let Some(dir) = std::env::var_os("REEVER_UPDATE_SHOTS").map(PathBuf::from) else {
            return;
        };
        let Some((w, h, rgba)) = crate::dialogs_native::update::snapshot(&app) else {
            eprintln!("[update-demo] no shot");
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let Ok(file) = std::fs::File::create(dir.join("update.png")) else {
            return;
        };
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut wr) = enc.write_header() {
            let _ = wr.write_image_data(&rgba);
        }
        eprintln!("[update-demo] shot");
    });
}

/// Установленная копия: установщик кладёт `uninstall.exe` рядом с `reever.exe`.
fn installed_copy() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("uninstall.exe").is_file()))
        .unwrap_or(false)
}

/// (можно в сеть, ставить само).
fn settings(app: &AppHandle) -> (bool, bool) {
    app.try_state::<LiveConfig>()
        .map_or((false, false), |config| {
            let settings = &config.get().settings;
            (!settings.offline_mode, settings.auto_update)
        })
}

/// Reever ничем не занят, и за компьютером, похоже, никого: перезапуск никому не помешает.
fn quiet() -> bool {
    crate::events::current_state() == "idle"
        && !crate::events::main_visible()
        && user_idle() >= USER_IDLE
}

fn user_idle() -> Duration {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    let now = unsafe { GetTickCount() };
    Duration::from_millis(now.wrapping_sub(info.dwTime) as u64)
}

/// Новее текущей версии есть релиз → скачать (или докачать) и проверить установщик.
fn fetch_newer(dir: &Path) -> Result<Option<Staged>, String> {
    // 404 — у репозитория ещё нет ни одного релиза: обновляться не на что.
    let body = match curl(&["-H", "Accept: application/vnd.github+json", LATEST]) {
        Err(error) if error.contains("404") => return Ok(None),
        other => other?,
    };
    let release: Release = serde_json::from_slice(&body).map_err(|e| format!("GitHub: {e}"))?;
    let version = release.tag_name.trim_start_matches(['v', 'V']).to_string();
    if !newer(&version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let name = format!("Reever_{version}_x64-setup.exe");
    let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
        return Ok(None);
    };
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(&name);
    if !(path.is_file() && verify(&path, asset).is_ok()) {
        let part = dir.join(format!("{name}.part"));
        let part_arg = part.to_string_lossy().into_owned();
        curl(&["-C", "-", "-o", &part_arg, &asset.browser_download_url])?;
        if let Err(error) = verify(&part, asset) {
            let _ = std::fs::remove_file(&part);
            return Err(error);
        }
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    }
    Ok(Some(Staged { version, path }))
}

fn verify(path: &Path, asset: &Asset) -> Result<(), String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size != asset.size {
        return Err(format!("{}: {size} / {} bytes", asset.name, asset.size));
    }
    if let Some(expected) = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
    {
        let actual = sha256(path).map_err(|e| e.to_string())?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("{}: SHA-256 mismatch", asset.name));
        }
    }
    Ok(())
}

fn sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    use std::os::windows::process::CommandExt;
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let output = Command::new(system.join("System32").join("curl.exe"))
        .args([
            "-L",
            "--fail",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--connect-timeout",
            "30",
            "--ssl-revoke-best-effort",
            "-A",
            concat!("Reever/", env!("CARGO_PKG_VERSION")),
        ])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    // 33 — докачивать нечего: файл уже целый, его проверит размер и сумма.
    if output.status.success() || output.status.code() == Some(33) {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Тихая установка поверх: установщик закроет Reever и запустит новый в трее.
fn install(path: &Path, hidden: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut args = vec!["/S", "/UPDATE", "/R"];
    if hidden {
        args.extend(["/ARGS", "--hidden"]);
    }
    Command::new(path)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Установщики уже стоящей (или более старой) версии и недокачанные хвосты от них.
fn remove_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let version = name
            .strip_prefix("Reever_")
            .and_then(|rest| rest.split('_').next())
            .unwrap_or_default();
        if !newer(version, env!("CARGO_PKG_VERSION")) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `candidate` новее `current` (числа через точку; нечисловое — не новее).
fn newer(candidate: &str, current: &str) -> bool {
    fn parse(version: &str) -> Option<Vec<u64>> {
        version.split('.').map(|part| part.parse().ok()).collect()
    }
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.8.8", "0.8.7"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.8.7", "0.8.7"));
        assert!(!newer("0.8.6", "0.8.7"));
        assert!(!newer("", "0.8.7"));
        assert!(!newer("0.9.0-beta", "0.8.7"));
    }

    #[test]
    fn release_json_parses() {
        let json = r#"{"tag_name":"v0.8.8","assets":[{"name":"Reever_0.8.8_x64-setup.exe",
            "browser_download_url":"https://example.com/a.exe","size":5,
            "digest":"sha256:abc"}]}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v0.8.8");
        assert_eq!(release.assets[0].size, 5);
        assert_eq!(release.assets[0].digest.as_deref(), Some("sha256:abc"));
    }

    #[test]
    fn verify_checks_size_and_digest() {
        let path = std::env::temp_dir().join("reever-update-verify.bin");
        std::fs::write(&path, b"abc").unwrap();
        let mut asset = Asset {
            name: "x".into(),
            browser_download_url: String::new(),
            size: 3,
            digest: Some(
                "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            ),
        };
        assert!(verify(&path, &asset).is_ok());
        asset.digest = Some("sha256:00".into());
        assert!(verify(&path, &asset).is_err());
        asset.digest = None;
        asset.size = 4;
        assert!(verify(&path, &asset).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
