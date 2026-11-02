//! Строки нативного интерфейса из тех же JSON, которые читают веб-окна.
//! Ключи и тексты живут только в `frontend/i18n/locales`.

use intl_pluralrules::{PluralCategory, PluralRuleType, PluralRules};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use unic_langid::LanguageIdentifier;

#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Text(String),
    Plural(HashMap<String, String>),
}

type Dictionary = HashMap<String, Entry>;

const SOURCES: [(&str, &str); 30] = [
    ("en", include_str!("../../frontend/i18n/locales/en.json")),
    ("ru", include_str!("../../frontend/i18n/locales/ru.json")),
    ("de", include_str!("../../frontend/i18n/locales/de.json")),
    ("es", include_str!("../../frontend/i18n/locales/es.json")),
    ("pt", include_str!("../../frontend/i18n/locales/pt.json")),
    ("it", include_str!("../../frontend/i18n/locales/it.json")),
    ("fr", include_str!("../../frontend/i18n/locales/fr.json")),
    ("zh", include_str!("../../frontend/i18n/locales/zh.json")),
    ("ja", include_str!("../../frontend/i18n/locales/ja.json")),
    ("ar", include_str!("../../frontend/i18n/locales/ar.json")),
    ("bg", include_str!("../../frontend/i18n/locales/bg.json")),
    ("hr", include_str!("../../frontend/i18n/locales/hr.json")),
    ("cs", include_str!("../../frontend/i18n/locales/cs.json")),
    ("da", include_str!("../../frontend/i18n/locales/da.json")),
    ("nl", include_str!("../../frontend/i18n/locales/nl.json")),
    ("et", include_str!("../../frontend/i18n/locales/et.json")),
    ("fi", include_str!("../../frontend/i18n/locales/fi.json")),
    ("el", include_str!("../../frontend/i18n/locales/el.json")),
    ("hu", include_str!("../../frontend/i18n/locales/hu.json")),
    ("lv", include_str!("../../frontend/i18n/locales/lv.json")),
    ("lt", include_str!("../../frontend/i18n/locales/lt.json")),
    ("mt", include_str!("../../frontend/i18n/locales/mt.json")),
    ("pl", include_str!("../../frontend/i18n/locales/pl.json")),
    ("ro", include_str!("../../frontend/i18n/locales/ro.json")),
    ("sk", include_str!("../../frontend/i18n/locales/sk.json")),
    ("sl", include_str!("../../frontend/i18n/locales/sl.json")),
    ("sv", include_str!("../../frontend/i18n/locales/sv.json")),
    ("uk", include_str!("../../frontend/i18n/locales/uk.json")),
    ("ko", include_str!("../../frontend/i18n/locales/ko.json")),
    ("yue", include_str!("../../frontend/i18n/locales/yue.json")),
];

fn dictionaries() -> &'static HashMap<&'static str, Dictionary> {
    static DICTIONARIES: OnceLock<HashMap<&'static str, Dictionary>> = OnceLock::new();
    DICTIONARIES.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(code, json)| {
                (
                    *code,
                    serde_json::from_str(json).expect("словарь интерфейса"),
                )
            })
            .collect()
    })
}

fn normalize(locale: &str) -> &str {
    let code = locale.trim().split(['-', '_']).next().unwrap_or_default();
    SOURCES
        .iter()
        .find(|(supported, _)| supported.eq_ignore_ascii_case(code))
        .map(|(supported, _)| *supported)
        .unwrap_or("en")
}

fn rules() -> &'static HashMap<&'static str, PluralRules> {
    static RULES: OnceLock<HashMap<&'static str, PluralRules>> = OnceLock::new();
    RULES.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(code, _)| {
                // В CLDR кантонский имеет только `other`, как и китайский.
                let rule_code = if *code == "yue" { "zh" } else { code };
                let language: LanguageIdentifier = rule_code.parse().expect("код языка");
                let rule = PluralRules::create(language, PluralRuleType::CARDINAL)
                    .expect("правила множественного числа для языка интерфейса");
                (*code, rule)
            })
            .collect()
    })
}

fn plural_category(locale: &str, count: &str) -> &'static str {
    let selected = rules()[locale]
        .select(count)
        .unwrap_or(PluralCategory::OTHER);
    match selected {
        PluralCategory::ZERO => "zero",
        PluralCategory::ONE => "one",
        PluralCategory::TWO => "two",
        PluralCategory::FEW => "few",
        PluralCategory::MANY => "many",
        PluralCategory::OTHER => "other",
    }
}

/// Строка без подстановок. Нужна журналу и трею, где вызывающие места
/// используют заимствованный текст; для ключа с формами числа возвращает ключ.
pub fn text<'a>(locale: &str, key: &'a str) -> &'a str {
    let locale = normalize(locale);
    let dictionaries = dictionaries();
    match dictionaries[locale]
        .get(key)
        .or_else(|| dictionaries["en"].get(key))
    {
        Some(Entry::Text(value)) => value,
        _ => key,
    }
}

/// Направление письма для `<код>` или системного кода вроде `ar-SA`.
pub fn direction(locale: &str) -> &'static str {
    if normalize(locale) == "ar" {
        "rtl"
    } else {
        "ltr"
    }
}

/// Перевод ключа. Параметры задаются парами `("count", "5")`; неизвестные
/// подстановки остаются в тексте. Для форм числа используется кардинальное правило CLDR.
pub fn t(locale: &str, key: &str, params: &[(&str, &str)]) -> String {
    let locale = normalize(locale);
    let dictionaries = dictionaries();
    let entry = dictionaries[locale]
        .get(key)
        .or_else(|| dictionaries["en"].get(key));
    let Some(entry) = entry else {
        return key.to_owned();
    };
    let template = match entry {
        Entry::Text(text) => text.as_str(),
        Entry::Plural(forms) => {
            let count = params
                .iter()
                .find(|(name, _)| *name == "count")
                .map(|(_, value)| *value)
                .unwrap_or("0");
            let category = plural_category(locale, count);
            forms
                .get(category)
                .or_else(|| forms.get("other"))
                .map(String::as_str)
                .unwrap_or(key)
        }
    };
    let text = replace_params(template, params);
    if locale == "ar" {
        isolate_quotes(&text)
    } else {
        text
    }
}

fn replace_params(template: &str, params: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('}') {
            let name = &after[..end];
            if !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                if let Some((_, value)) = params.iter().find(|(param, _)| *param == name) {
                    out.push_str(value);
                    rest = &after[end + 1..];
                    continue;
                }
            }
        }
        out.push('{');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn isolate_quotes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('«') {
        out.push_str(&rest[..start]);
        let after = &rest[start + '«'.len_utf8()..];
        let Some(end) = after.find('»') else {
            rest = &rest[start..];
            break;
        };
        out.push_str("«\u{2068}");
        out.push_str(&after[..end]);
        out.push_str("\u{2069}»");
        rest = &after[end + '»'.len_utf8()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_english_keys_exist_in_every_language() {
        let dictionaries = dictionaries();
        let english = &dictionaries["en"];
        assert_eq!(SOURCES.len(), 30);
        for (code, _) in SOURCES {
            let dictionary = &dictionaries[code];
            for key in english.keys() {
                assert!(dictionary.contains_key(key), "{code}: {key}");
            }
        }
    }

    #[test]
    fn plural_rules_match_cardinal_examples() {
        for (count, category) in [("1", "one"), ("2", "few"), ("5", "many"), ("21", "one")] {
            assert_eq!(plural_category("ru", count), category, "ru: {count}");
        }
        for (count, category) in [
            ("0", "zero"),
            ("1", "one"),
            ("2", "two"),
            ("3", "few"),
            ("11", "many"),
            ("100", "other"),
        ] {
            assert_eq!(plural_category("ar", count), category, "ar: {count}");
        }
        assert_eq!(plural_category("en", "1"), "one");
        assert_eq!(plural_category("en", "2"), "other");
        assert_eq!(plural_category("ja", "1"), "other");
        assert_eq!(plural_category("ja", "5"), "other");
        assert_eq!(t("ru", "picker.count", &[("count", "21")]), "21 вариант");
        assert_eq!(t("en", "picker.count", &[("count", "2")]), "2 options");
    }

    #[test]
    fn substitution_fallback_and_direction() {
        assert_eq!(
            t("en", "settings.error", &[("error", "oops")]),
            "Error: oops"
        );
        assert_eq!(t("en", "settings.error", &[]), "Error: {error}");
        assert_eq!(t("en", "missing.key", &[]), "missing.key");
        assert_eq!(t("xx", "win.minimize", &[]), "Minimize");
        assert_eq!(direction("ar-SA"), "rtl");
        assert_eq!(direction("ru"), "ltr");
        assert_eq!(text("en", "tray.show"), "Show settings");
        assert_eq!(text("zz", "tray.show"), "Show settings");
        assert_eq!(text("en", "missing.key"), "missing.key");
    }
}
