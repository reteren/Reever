//! Язык интерфейса для того, что рисует сам Rust: меню трея и строки журнала.
//! Интерфейс окна и журнал читают общие словари через `ui_text`; здесь — тот же
//! выбор: `ui_language` в settings.json, `auto` — язык Windows.
//!
//! Словари — `frontend/i18n/locales/<код>.json`; журнал и трей используют
//! ключи `log.*` и `tray.*` наряду с ключами окон.

pub const SUPPORTED: [&str; 30] = [
    "en", "ru", "de", "es", "pt", "it", "fr", "zh", "ja", "ar", "bg", "hr", "cs", "da", "nl", "et",
    "fi", "el", "hu", "lv", "lt", "mt", "pl", "ro", "sk", "sl", "sv", "uk", "ko", "yue",
];

/// Текущий язык интерфейса (для строк, которые Rust шлёт в журнал).
pub static CURRENT: std::sync::Mutex<&'static str> = std::sync::Mutex::new("en");

/// Как `resolve`, но среди включённых языков интерфейса (`Settings::ui_languages`):
/// выключенный — английский.
/// `auto` здесь — первый из предпочитаемых языков пользователя (Параметры → Язык), а не
/// региональный формат и не язык самой Windows: его же отдаёт WebView2 как
/// `navigator.language`. Окно после загрузки всё равно сообщает свой язык (`sync_ui_locale`).
pub fn resolve_enabled(preference: &str, enabled: &[&str]) -> &'static str {
    let preference = preference.trim();
    let code = if preference.is_empty() || preference.eq_ignore_ascii_case("auto") {
        normalize(&preferred_language())
    } else {
        normalize(preference)
    };
    if enabled.contains(&code) {
        code
    } else {
        "en"
    }
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
    SUPPORTED
        .iter()
        .copied()
        .find(|code| *code == language)
        .unwrap_or("en")
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

/// Первый предпочитаемый язык пользователя («en-US»): значение Languages в
/// HKCU\Control Panel\International\User Profile (его показывает Get-WinUserLanguageList).
fn preferred_language() -> String {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_MULTI_SZ};
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Control Panel\International\User Profile"),
            w!("Languages"),
            RRF_RT_REG_MULTI_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_ok() {
        let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
        if end > 0 {
            return String::from_utf16_lossy(&buffer[..end]);
        }
    }
    system_ui_language()
}

/// Первый язык интерфейса Windows («ru-RU»).
fn system_ui_language() -> String {
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let (mut count, mut length) = (0u32, 0u32);
    unsafe {
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            windows::core::PWSTR::null(),
            &mut length,
        )
        .is_err()
            || length == 0
        {
            return system_locale();
        }
        let mut buffer = vec![0u16; length as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .is_err()
        {
            return system_locale();
        }
        let end = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }
}

/// Слово для подсказки в журнале на языке интерфейса: в русском — первое из списка,
/// в остальных — первое латиницей (как `displayWake` в app.js), иначе `english`.
pub fn spoken(words: &[String], russian: &str, english: &str) -> String {
    let words = words
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty());
    if current() == "ru" {
        return words.into_iter().next().unwrap_or(russian).to_string();
    }
    let latin = |word: &&str| word.chars().any(|c| c.is_ascii_alphabetic()) && word.is_ascii();
    words.into_iter().find(latin).unwrap_or(english).to_string()
}

pub fn current() -> &'static str {
    CURRENT.lock().map(|locale| *locale).unwrap_or("en")
}

/// Строка на языке `locale` без подстановок.
pub fn text<'a>(locale: &str, key: &'a str) -> &'a str {
    crate::ui_text::text(locale, key)
}

/// Строка на языке `locale` с подстановкой `{name}`; неизвестные `{…}` остаются как есть.
pub fn tr(locale: &str, key: &str, args: &[(&str, String)]) -> String {
    let params: Vec<_> = args
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    crate::ui_text::t(locale, key, &params)
}

/// Сообщение для журнала: ключ словаря и подстановки. Переводится при показе —
/// окно получает его на языке интерфейса, а `reever.log` всегда по-английски.
#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Msg {
    pub fn render(&self, locale: &str) -> String {
        tr(locale, self.key, &self.args)
    }
}

/// `msg!("log.key", name = value, …)` — сообщение журнала; значения через `Display`.
#[macro_export]
macro_rules! msg {
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::uilang::Msg {
            key: $key,
            args: vec![$((stringify!($name), ($value).to_string())),*],
        }
    };
}

/// Строка журнала: переводимое сообщение или текст как есть (услышанная фраза,
/// путь, диагностика `trace:` — у неё нет перевода).
#[derive(Clone, Debug, PartialEq)]
pub enum LogText {
    Msg(Msg),
    Raw(String),
}

impl LogText {
    pub fn render(&self, locale: &str) -> String {
        match self {
            LogText::Msg(msg) => msg.render(locale),
            LogText::Raw(text) => text.clone(),
        }
    }
}

impl From<Msg> for LogText {
    fn from(msg: Msg) -> Self {
        LogText::Msg(msg)
    }
}

impl From<String> for LogText {
    fn from(text: String) -> Self {
        LogText::Raw(text)
    }
}

impl From<&str> for LogText {
    fn from(text: &str) -> Self {
        LogText::Raw(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_languages_and_regions_resolve() {
        assert_eq!(resolve("de"), "de");
        assert_eq!(resolve("pt-BR"), "pt");
        assert_eq!(resolve("zh_CN"), "zh");
        assert_eq!(resolve("ko"), "ko");
        assert_eq!(resolve("tr"), "en");
        assert!(SUPPORTED.contains(&resolve("auto")));
    }

    #[test]
    fn journal_and_tray_use_shared_dictionary() {
        for code in SUPPORTED {
            assert_ne!(text(code, "tray.show"), "tray.show", "{code}");
            assert_ne!(text(code, "log.paused"), "log.paused", "{code}");
        }
    }

    #[test]
    fn placeholders_fill_and_unknown_ones_stay() {
        let msg = crate::msg!("log.launch", target = "C:\\a.exe");
        assert_eq!(msg.render("en"), "▶ launching: C:\\a.exe");
        assert_eq!(tr("en", "missing.key", &[]), "missing.key");
        assert_eq!(tr("en", "log.launch", &[]), "▶ launching: {target}");
    }
}
