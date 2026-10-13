//! Язык интерфейса для того, что рисует сам Rust: меню трея и строки журнала о паузе.
//! Интерфейс окна переводится в frontend/i18n; здесь — те же десять языков и тот же
//! выбор: `ui_language` в settings.json, `auto` — язык Windows.

use tauri::menu::MenuItem;
use tauri::Wry;

pub const SUPPORTED: [&str; 10] = ["en", "ru", "de", "es", "pt", "it", "fr", "zh", "ja", "ar"];

/// Текущий язык интерфейса (для строк, которые Rust шлёт в журнал).
pub static CURRENT: std::sync::Mutex<&'static str> = std::sync::Mutex::new("en");

/// Пункты меню трея — чтобы сменить им подписи при смене языка без перезапуска.
pub struct TrayItems {
    pub show: MenuItem<Wry>,
    pub pause: MenuItem<Wry>,
    pub quit: MenuItem<Wry>,
}

/// Код языка из настройки или из языка Windows; незнакомый — английский.
pub fn resolve(preference: &str) -> &'static str {
    let preference = preference.trim();
    if preference.is_empty() || preference.eq_ignore_ascii_case("auto") {
        return normalize(&system_locale());
    }
    normalize(preference)
}

fn normalize(value: &str) -> &'static str {
    let language = value.trim().to_lowercase();
    let language = language.split(['-', '_']).next().unwrap_or_default();
    SUPPORTED.iter().copied().find(|code| *code == language).unwrap_or("en")
}

fn system_locale() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buffer = [0u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    if length <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize - 1])
}

/// Строка трея или журнала на языке `locale`.
pub fn text(locale: &str, key: &str) -> &'static str {
    let row: [&'static str; 10] = match key {
        "tray.show" => [
            "Show settings",
            "Показать настройки",
            "Einstellungen anzeigen",
            "Mostrar ajustes",
            "Mostrar configurações",
            "Mostra impostazioni",
            "Afficher les paramètres",
            "显示设置",
            "設定を表示",
            "عرض الإعدادات",
        ],
        "tray.pause" => [
            "Pause / resume microphone",
            "Пауза / Продолжить микрофон",
            "Mikrofon pausieren / fortsetzen",
            "Pausar / reanudar micrófono",
            "Pausar / retomar microfone",
            "Metti in pausa / riprendi microfono",
            "Suspendre / reprendre le micro",
            "暂停 / 恢复麦克风",
            "マイクを一時停止 / 再開",
            "إيقاف الميكروفون مؤقتًا / استئنافه",
        ],
        "tray.quit" => [
            "Quit", "Выход", "Beenden", "Salir", "Sair", "Esci", "Quitter", "退出", "終了", "خروج",
        ],
        "log.paused" => [
            "⏸ microphone paused",
            "⏸ микрофон на паузе",
            "⏸ Mikrofon pausiert",
            "⏸ micrófono en pausa",
            "⏸ microfone em pausa",
            "⏸ microfono in pausa",
            "⏸ micro en pause",
            "⏸ 麦克风已暂停",
            "⏸ マイクを一時停止しました",
            "⏸ الميكروفون متوقف مؤقتًا",
        ],
        "log.resumed" => [
            "▶ microphone active",
            "▶ микрофон активен",
            "▶ Mikrofon aktiv",
            "▶ micrófono activo",
            "▶ microfone ativo",
            "▶ microfono attivo",
            "▶ micro actif",
            "▶ 麦克风已启用",
            "▶ マイクが有効です",
            "▶ الميكروفون نشط",
        ],
        _ => return "",
    };
    let index = SUPPORTED.iter().position(|code| *code == locale).unwrap_or(0);
    row[index]
}

/// Подписи меню трея на языке `locale`.
pub fn apply_tray(items: &TrayItems, locale: &str) {
    let _ = items.show.set_text(text(locale, "tray.show"));
    let _ = items.pause.set_text(text(locale, "tray.pause"));
    let _ = items.quit.set_text(text(locale, "tray.quit"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_languages_and_regions_resolve() {
        assert_eq!(resolve("de"), "de");
        assert_eq!(resolve("pt-BR"), "pt");
        assert_eq!(resolve("zh_CN"), "zh");
        assert_eq!(resolve("ko"), "en");
        assert!(SUPPORTED.contains(&resolve("auto")));
    }

    #[test]
    fn every_language_has_every_tray_string() {
        for key in ["tray.show", "tray.pause", "tray.quit", "log.paused", "log.resumed"] {
            for locale in SUPPORTED {
                assert!(!text(locale, key).is_empty(), "{locale} {key}");
            }
        }
    }
}
