//! «Что можно сказать» на «Главной»: строки списка и подсказка над ним — то же, что
//! renderGrammarExamples / renderHomeHint в frontend/app.js (пример команды на языке интерфейса:
//! русский, если интерфейс русский и русский распознаётся; иначе английский).

use crate::grammar::{Grammar, Intent, IntentKind};
use crate::ui_text::t;

/// Строка списка: что сказать, что произойдёт; `key` — что раскрывает клик (id команды или
/// `cancel`), `off` — команда выключена (строка приглушена).
#[derive(Debug, Clone, PartialEq)]
pub struct SayRow {
    pub command: String,
    pub desc: String,
    pub key: Option<String>,
    pub off: bool,
}

/// Список для экрана: подсказка над ним (пока ничего не услышано) и строки.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SayList {
    pub hint: String,
    pub rows: Vec<SayRow>,
}

fn is_cyrillic(s: &str) -> bool {
    s.chars().any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

/// Английская фраза — только латиница ASCII: «не кириллица» ловит и «リバー», и «öffne».
fn is_english(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic()) && s.chars().all(|c| (' '..='~').contains(&c))
}

struct Ctx<'a> {
    locale: &'a str,
    ru: bool,
    /// языки распознавания (английский — всегда)
    langs: &'a [&'a str],
}

impl Ctx<'_> {
    fn t(&self, key: &str, params: &[(&str, &str)]) -> String {
        t(self.locale, key, params)
    }

    /// Слова отмены языков, которые распознаются: «abbrechen» без немецкого не показывать.
    fn heard_cancel_words(&self, words: &[String]) -> Vec<String> {
        words.iter().filter(|w| crate::grammar::cancel_word_active(w, self.langs)).cloned().collect()
    }

    fn pick(&self, values: &[String], fallback: &str) -> String {
        let list: Vec<&str> = values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()).collect();
        let fits = |v: &&&str| if self.ru { is_cyrillic(v) } else { is_english(v) };
        list.iter().find(fits).or(list.first()).map_or(fallback.to_string(), |v| v.to_string())
    }

    /// Wake-слово в примерах: «ривер» английская модель слышит как «river».
    fn wake(&self, g: &Grammar) -> String {
        let words: Vec<&str> = g.wake_words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
        if self.ru {
            return words.first().unwrap_or(&"ривер").to_string();
        }
        if let Some(latin) = words.iter().find(|w| is_english(w)) {
            return latin.to_string();
        }
        match words.first() {
            None => "reever".into(),
            Some(w) if w.to_lowercase() == "ривер" => "reever".into(),
            Some(w) => w.to_string(),
        }
    }

    /// Подпись команды: перевод, если пользователь её не менял (как intentLabel в app.js).
    fn label(&self, intent: &Intent) -> String {
        if is_default_label(intent) {
            return self.t(&format!("intent.{}", intent.id), &[]);
        }
        let label = intent.label.trim();
        if label.is_empty() { intent.id.clone() } else { label.to_string() }
    }

    fn example(&self, g: &Grammar, intent: &Intent, wake: &str) -> Option<(String, String)> {
        let triggers: Vec<String> = intent.triggers.iter().map(|v| v.trim().to_string()).collect();
        let trigger = self.pick(&triggers, "");
        let ru = self.ru;
        if intent.kind == IntentKind::Launch && triggers.iter().any(|v| v.is_empty()) {
            let label = self.label(intent);
            return Some((format!("{wake} X"), self.t("example.launch", &[("label", &label)])));
        }
        if trigger.is_empty() {
            return None;
        }
        let x = format!("{wake} {trigger} X");
        Some(match intent.kind {
            IntentKind::Close => (x, self.t("example.close", &[])),
            IntentKind::Kill => (x, self.t("example.kill", &[])),
            IntentKind::Calc => (x, self.t("example.calc", &[("expr", if ru { "33 умножить на 78" } else { "33 times 78" })])),
            IntentKind::Key => {
                let keys = if ru { "энтер, контрол эс, виндоус дэ" } else { "enter, ctrl s, windows d" };
                (x, self.t("example.key", &[("keys", keys)]))
            }
            IntentKind::Find => (x, self.t("example.findBoth", &[])),
            IntentKind::FindFolder => (x, self.t("example.findFolders", &[])),
            IntentKind::FindFile => (x, self.t("example.findFiles", &[])),
            IntentKind::WebChat => {
                let label = self.label(intent);
                let label = if label.is_empty() { self.t("example.chatDefault", &[]) } else { label };
                (x, self.t("example.chat", &[("label", &label)]))
            }
            IntentKind::Dictate => {
                let stop = self.pick(&g.dictation_stop, if ru { "стоп ввод" } else { "stop typing" });
                // «{wake} стоп» заканчивает диктовку, «{wake} отмена» стирает (engine.rs)
                let end = if ru { "стоп" } else { "stop" };
                let cancel_words: Vec<String> = self
                    .heard_cancel_words(&g.cancel_words)
                    .iter()
                    .filter(|w| !["стоп", "stop", "хватит", "остановись"].contains(&w.trim().to_lowercase().as_str()))
                    .cloned()
                    .collect();
                let cancel = self.pick(&cancel_words, if ru { "отмена" } else { "cancel" });
                let desc = self.t("example.dictate", &[("wake", wake), ("stop", &stop), ("cancel", &cancel), ("end", end)]);
                (format!("{wake} {trigger} …"), desc)
            }
            _ => {
                let label = self.label(intent);
                (x, if label.is_empty() { self.t("example.command", &[]) } else { label })
            }
        })
    }
}

/// Подпись по умолчанию (или старая подпись по умолчанию из прежних версий grammar.json).
fn is_default_label(intent: &Intent) -> bool {
    let defaults = Grammar::defaults();
    let Some(builtin) = defaults.intents.iter().find(|i| i.id == intent.id).map(|i| i.label.clone()) else {
        return false;
    };
    let label = intent.label.trim();
    label.is_empty()
        || label == builtin
        || (intent.id == "calc" && label == "калькулятор")
        || (intent.id == "folder" && label == "открыть папку на компьютере (Everything)")
}

/// Список «Что можно сказать» для языка интерфейса `locale`; `langs` — языки распознавания
/// (английский распознаётся всегда).
pub fn build(g: &Grammar, locale: &str, langs: &[&str]) -> SayList {
    let cx = Ctx { locale, ru: locale == "ru" && langs.contains(&"ru"), langs };
    let wake = cx.wake(g);
    let launch = g.intents.iter().find(|i| i.kind == IntentKind::Launch && i.enabled);
    let open = cx.pick(launch.map_or(&[][..], |i| &i.triggers[..]), if cx.ru { "открой" } else { "open" });
    let hint = cx.t("home.hint", &[("wake", &wake), ("open", &open)]);
    let mut rows = Vec::new();
    for intent in &g.intents {
        if let Some((command, desc)) = cx.example(g, intent, &wake) {
            rows.push(SayRow { command, desc, key: Some(intent.id.clone()), off: !intent.enabled });
        }
    }
    let shutdown = if cx.ru { "выключи комп" } else { "shut down computer" };
    rows.push(SayRow { command: format!("{wake} {shutdown}"), desc: cx.t("example.shutdown", &[]), key: None, off: false });
    let cancel = cx.pick(&cx.heard_cancel_words(&g.cancel_words), if cx.ru { "отмена" } else { "cancel" });
    rows.push(SayRow { command: format!("{wake} {cancel}"), desc: cx.t("example.cancel", &[]), key: Some("cancel".into()), off: false });
    SayList { hint, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_examples_only_when_russian_is_heard() {
        let g = Grammar::defaults();
        let ru = build(&g, "ru", &["en", "ru"]);
        assert!(ru.rows.iter().all(|r| r.command.starts_with("ривер ")), "{:?}", ru.rows);
        assert_eq!(ru.rows.last().unwrap().key.as_deref(), Some("cancel"));
        // русский интерфейс без русской модели — английские примеры
        let en = build(&g, "ru", &["en"]);
        assert!(en.rows.iter().all(|r| r.command.starts_with("reever ")), "{:?}", en.rows);
        assert!(en.rows.iter().all(|r| is_english(&r.command.replace('…', "x"))), "{:?}", en.rows);
    }

    #[test]
    fn every_enabled_intent_has_a_row_plus_shutdown_and_cancel() {
        let g = Grammar::defaults();
        let list = build(&g, "en", &["en"]);
        assert_eq!(list.rows.len(), g.intents.len() + 2);
        assert!(list.rows.iter().all(|r| !r.desc.is_empty() && !r.desc.starts_with("example.")));
        assert!(!list.hint.is_empty() && !list.hint.starts_with("home."));
    }

    #[test]
    fn disabled_intent_is_muted_and_custom_label_is_kept() {
        let mut g = Grammar::defaults();
        let calc = g.intents.iter_mut().find(|i| i.id == "calc").unwrap();
        calc.enabled = false;
        let list = build(&g, "en", &["en"]);
        assert!(list.rows.iter().find(|r| r.key.as_deref() == Some("calc")).unwrap().off);
        let folder = g.intents.iter_mut().find(|i| i.id == "folder").unwrap();
        folder.label = "my folders".into();
        let list = build(&g, "en", &["en"]);
        assert_eq!(list.rows.iter().find(|r| r.key.as_deref() == Some("folder")).unwrap().desc, "my folders");
    }

    #[test]
    fn cancel_words_of_languages_not_heard_are_not_shown() {
        // порядок, как у грамматики после миграций: слова пакетов раньше английских
        let mut g = Grammar::defaults();
        g.cancel_words = ["отмена", "стоп", "abbrechen", "annuler", "stop", "cancel"].map(String::from).to_vec();
        let list = build(&g, "en", &["en"]);
        assert_eq!(list.rows.last().unwrap().command, "reever stop");
        let dictate = list.rows.iter().find(|r| r.key.as_deref() == Some("dictate")).unwrap();
        assert!(!dictate.desc.contains("abbrechen"), "{}", dictate.desc);
        let de = build(&g, "en", &["en", "de"]);
        assert_eq!(de.rows.last().unwrap().command, "reever abbrechen");
    }
}
