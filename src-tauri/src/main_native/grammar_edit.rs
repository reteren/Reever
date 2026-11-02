//! Редактор «Что можно сказать» (renderGrammarEditor в frontend/app.js): черновик грамматики,
//! форма под раскрытой строкой списка (или над списком — «Изменить»: wake-слова, новые поиски
//! и чаты, сброс), проверка на языке интерфейса до сохранения (grammarErrors).

use std::collections::{HashMap, HashSet};

use crate::grammar::{Grammar, Intent, IntentKind};
use crate::native::controls::button::ButtonKind;
use crate::native::controls::form::{FormEvent, Item};
use crate::ui_text::t;

use super::say::{intent_label, is_default_label};

/// Что редактор просит у окна.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorOut {
    None,
    /// Форму надо пересобрать (черновик изменился).
    Rebuild,
    Save(Grammar),
    Close,
    Reset,
}

pub struct GrammarEditor {
    /// id команды, `cancel` или `general`
    pub open: String,
    pub draft: Grammar,
    saved_ids: HashSet<String>,
    /// встроенные фразы языков, которые не распознаются: не видны, но не удаляются
    hidden: HashSet<String>,
    /// «оставьте хотя бы одно…» — вместо подсказки группы, пока не добавят новое
    notes: HashMap<String, String>,
    pub errors: Vec<String>,
    confirm_reset: bool,
    locale: String,
    wake: String,
    ru: bool,
}

fn kind_name(k: IntentKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

fn kind_icon(k: IntentKind) -> &'static str {
    match k {
        IntentKind::Launch => "play",
        IntentKind::Folder | IntentKind::FindFolder => "folder",
        IntentKind::Search | IntentKind::Find => "lens",
        IntentKind::Close => "x",
        IntentKind::Kill => "stop",
        IntentKind::Calc => "sum",
        IntentKind::Dictate => "pen",
        IntentKind::Key => "term",
        IntentKind::FindFile => "file",
        IntentKind::WebChat => "spark",
    }
}

/// Сколько фраз нужно оставить у команды (как minimumTriggers в app.js).
fn minimum_triggers(k: IntentKind) -> usize {
    match k {
        IntentKind::Close
        | IntentKind::Kill
        | IntentKind::Calc
        | IntentKind::Dictate
        | IntentKind::Key
        | IntentKind::WebChat
        | IntentKind::Find
        | IntentKind::FindFolder
        | IntentKind::FindFile => 1,
        _ => 0,
    }
}

fn deletable(i: &Intent) -> bool {
    (i.kind == IntentKind::Search && i.id.starts_with("search_")) || (i.kind == IntentKind::WebChat && i.id.starts_with("chat_custom_"))
}

/// Проверка, как Grammar::validate, но на языке интерфейса (grammarErrors в app.js).
pub fn errors(g: &Grammar, locale: &str) -> Vec<String> {
    let tr = |k: &str, p: &[(&str, &str)]| t(locale, k, p);
    let mut out = Vec::new();
    if !g.wake_words.iter().any(|w| !w.trim().is_empty()) {
        out.push(tr("validation.wake", &[]));
    }
    let mut ids = HashSet::new();
    let mut phrases: HashMap<String, String> = HashMap::new();
    for intent in &g.intents {
        let name = intent_label(locale, intent);
        if intent.id.trim().is_empty() {
            out.push(tr("validation.id", &[]));
        } else if !ids.insert(intent.id.clone()) {
            out.push(tr("validation.duplicateId", &[("id", &intent.id)]));
        }
        if intent.kind == IntentKind::Search && !intent.url.contains("{q}") {
            out.push(tr("validation.searchQ", &[("name", &name)]));
        }
        if intent.kind == IntentKind::WebChat {
            let u = intent.url.trim();
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                out.push(tr("validation.chatUrl", &[("name", &name)]));
            }
        }
        if matches!(intent.kind, IntentKind::Calc | IntentKind::Dictate | IntentKind::Key) && !intent.triggers.iter().any(|p| !p.trim().is_empty()) {
            out.push(tr("validation.needPhrase", &[("name", &name)]));
        }
        for phrase in &intent.triggers {
            let key = phrase.trim().to_lowercase();
            if key.is_empty() && intent.kind != IntentKind::Launch {
                out.push(tr("validation.emptyPhrase", &[("name", &name)]));
            }
            if key.is_empty() {
                continue;
            }
            if let Some(other) = phrases.get(&key) {
                out.push(tr("validation.duplicatePhrase", &[("phrase", phrase), ("other", other)]));
            } else {
                phrases.insert(key, name.clone());
            }
        }
    }
    out
}

impl GrammarEditor {
    pub fn new(open: &str, grammar: &Grammar, hidden: &[String], locale: &str, wake: &str, ru: bool) -> Self {
        Self {
            open: open.to_string(),
            draft: grammar.clone(),
            saved_ids: grammar.intents.iter().map(|i| i.id.clone()).collect(),
            hidden: hidden.iter().cloned().collect(),
            notes: HashMap::new(),
            errors: Vec::new(),
            confirm_reset: false,
            locale: locale.to_string(),
            wake: wake.to_string(),
            ru,
        }
    }

    fn t(&self, key: &str) -> String {
        t(&self.locale, key, &[])
    }

    fn visible_triggers(&self, i: &Intent) -> Vec<String> {
        i.triggers.iter().filter(|v| !v.trim().is_empty() && !self.hidden.contains(*v)).cloned().collect()
    }

    fn words_group(&self, items: &mut Vec<Item>, legend: &str, hint: &str, id: &str, values: Vec<String>, add: &str) {
        items.push(Item::GroupBegin { legend: self.t(legend), icon: None });
        items.push(Item::Note(self.notes.get(id).cloned().unwrap_or_else(|| self.t(hint))));
        items.push(Item::Chips { id: id.into(), values, primary_first: id == "wake" });
        items.push(Item::AddRow { id: format!("{id}+"), placeholder: self.t(add), button: self.t("common.add") });
        items.push(Item::GroupEnd);
    }

    fn intent_group(&self, items: &mut Vec<Item>, i: &Intent) {
        let kind = format!("kind.{}", kind_name(i.kind));
        let legend = if t(&self.locale, &kind, &[]) == kind { self.t("grammar.intent") } else { self.t(&kind) };
        items.push(Item::GroupBegin { legend, icon: Some(kind_icon(i.kind)) });
        items.push(Item::Check { id: format!("en:{}", i.id), label: self.t("grammar.enabled"), on: i.enabled });
        items.push(Item::Label(self.t("grammar.label")));
        let label = if is_default_label(i) { intent_label(&self.locale, i) } else { i.label.clone() };
        items.push(Item::Input { id: format!("label:{}", i.id), placeholder: String::new(), value: label });
        if deletable(i) {
            let key = if i.kind == IntentKind::WebChat { "grammar.deleteChat" } else { "grammar.deleteSearch" };
            items.push(Item::Buttons { buttons: vec![(format!("del:{}", i.id), self.t(key), ButtonKind::Danger)], top: 8.0, bottom: 0.0 });
        }
        items.push(Item::Label(self.t("grammar.phrases")));
        let chips = format!("trig:{}", i.id);
        if minimum_triggers(i.kind) > 0 {
            items.push(Item::Note(self.notes.get(&chips).cloned().unwrap_or_default()));
        }
        items.push(Item::Chips { id: chips.clone(), values: self.visible_triggers(i), primary_first: false });
        items.push(Item::AddRow { id: format!("{chips}+"), placeholder: self.t("grammar.addPhrase"), button: self.t("common.add") });
        if i.kind == IntentKind::Launch {
            let app = if self.ru { "фотошоп" } else { "photoshop" };
            items.push(Item::Check {
                id: format!("bare:{}", i.id),
                label: t(&self.locale, "grammar.bareLaunch", &[("wake", &self.wake), ("app", app)]),
                on: i.triggers.iter().any(|v| v.trim().is_empty()),
            });
        }
        if i.kind == IntentKind::WebChat {
            items.push(Item::Label(self.t("grammar.chatUrl")));
            items.push(Item::Input { id: format!("url:{}", i.id), placeholder: "https://gemini.google.com/app".into(), value: i.url.clone() });
        }
        if i.kind == IntentKind::Search {
            items.push(Item::Label(self.t("grammar.searchUrl")));
            items.push(Item::Input { id: format!("url:{}", i.id), placeholder: "https://example.com/?q={q}".into(), value: i.url.clone() });
        }
        items.push(Item::GroupEnd);
    }

    /// Форма для текущего черновика.
    pub fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        match self.open.as_str() {
            "general" => {
                self.words_group(&mut items, "grammar.wakeLegend", "grammar.wakeHint", "wake", self.draft.wake_words.clone(), "grammar.addWake");
                // новые, ещё не сохранённые поиски и чаты правятся здесь же
                for i in self.draft.intents.iter().filter(|i| !self.saved_ids.contains(&i.id)) {
                    self.intent_group(&mut items, i);
                }
                items.push(Item::Buttons {
                    buttons: vec![
                        ("addsearch".into(), self.t("grammar.addSearch"), ButtonKind::Plain),
                        ("addchat".into(), self.t("grammar.addChat"), ButtonKind::Plain),
                    ],
                    top: 0.0,
                    bottom: 12.0,
                });
            }
            "cancel" => {
                self.words_group(&mut items, "grammar.cancelLegend", "grammar.cancelHint", "cancel", self.draft.cancel_words.clone(), "grammar.addCancel");
            }
            id => {
                if let Some(i) = self.draft.intents.iter().find(|i| i.id == id) {
                    self.intent_group(&mut items, i);
                    if i.kind == IntentKind::Dictate {
                        self.words_group(&mut items, "grammar.stopLegend", "grammar.stopHint", "stop", self.draft.dictation_stop.clone(), "grammar.addStop");
                    }
                }
            }
        }
        let mut buttons = vec![
            ("save".to_string(), self.t("common.save"), ButtonKind::Default),
            ("close".to_string(), self.t("common.cancel"), ButtonKind::Plain),
        ];
        if self.open == "general" {
            buttons.push(("reset".into(), self.t("grammar.reset"), ButtonKind::Danger));
        }
        items.push(Item::Buttons { buttons, top: 12.0, bottom: 4.0 });
        items.push(Item::Errors(self.errors.clone()));
        items
    }

    fn intent_mut(&mut self, id: &str) -> Option<&mut Intent> {
        self.draft.intents.iter_mut().find(|i| i.id == id)
    }

    /// Значения фишек по id группы.
    fn chips(&self, id: &str) -> Option<(Vec<String>, usize, &'static str)> {
        Some(match id {
            "wake" => (self.draft.wake_words.clone(), 1, "grammar.keepOneWake"),
            "cancel" => (self.draft.cancel_words.clone(), 0, "grammar.keepOnePhrase"),
            "stop" => (self.draft.dictation_stop.clone(), 1, "grammar.keepOnePhrase"),
            _ => {
                let iid = id.strip_prefix("trig:")?;
                let i = self.draft.intents.iter().find(|i| i.id == iid)?;
                (self.visible_triggers(i), minimum_triggers(i.kind), "grammar.keepOnePhrase")
            }
        })
    }

    fn set_chips(&mut self, id: &str, values: Vec<String>) {
        match id {
            "wake" => self.draft.wake_words = values,
            "cancel" => self.draft.cancel_words = values,
            "stop" => self.draft.dictation_stop = values,
            _ => {
                let Some(iid) = id.strip_prefix("trig:") else { return };
                let hidden = self.hidden.clone();
                if let Some(i) = self.intent_mut(iid) {
                    let bare = i.kind == IntentKind::Launch && i.triggers.iter().any(|v| v.trim().is_empty());
                    let kept: Vec<String> = i.triggers.iter().filter(|v| hidden.contains(*v)).cloned().collect();
                    let mut next = Vec::new();
                    if bare {
                        next.push(String::new());
                    }
                    next.extend(values);
                    next.extend(kept);
                    i.triggers = next;
                }
            }
        }
    }

    fn new_id(&self, prefix: &str) -> String {
        let mut n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        while self.draft.intents.iter().any(|i| i.id == format!("{prefix}{n}")) {
            n += 1;
        }
        format!("{prefix}{n}")
    }

    pub fn on_event(&mut self, ev: FormEvent) -> EditorOut {
        if !matches!(ev, FormEvent::Click(ref c) if c == "reset") {
            self.confirm_reset = false;
        }
        match ev {
            FormEvent::Escape => EditorOut::Close,
            FormEvent::Toggle(id) => {
                if let Some(iid) = id.strip_prefix("en:") {
                    if let Some(i) = self.intent_mut(iid) {
                        i.enabled = !i.enabled;
                    }
                } else if let Some(iid) = id.strip_prefix("bare:") {
                    if let Some(i) = self.intent_mut(iid) {
                        let rest: Vec<String> = i.triggers.iter().filter(|v| !v.trim().is_empty()).cloned().collect();
                        let bare = i.triggers.len() != rest.len();
                        i.triggers = if bare { rest } else { std::iter::once(String::new()).chain(rest).collect() };
                    }
                }
                EditorOut::Rebuild
            }
            FormEvent::Remove(id, index) => {
                let Some((values, minimum, keep)) = self.chips(&id) else { return EditorOut::None };
                let next: Vec<String> = values.into_iter().enumerate().filter(|(i, _)| *i != index).map(|(_, v)| v).collect();
                if next.len() < minimum {
                    self.notes.insert(id, self.t(keep));
                } else {
                    self.set_chips(&id, next);
                }
                EditorOut::Rebuild
            }
            FormEvent::Add(field, value) => {
                let id = field.trim_end_matches('+').to_string();
                let Some((mut values, _, _)) = self.chips(&id) else { return EditorOut::None };
                values.push(value);
                self.set_chips(&id, values);
                self.notes.remove(&id);
                EditorOut::Rebuild
            }
            FormEvent::Edited(id, value) => {
                if let Some(iid) = id.strip_prefix("label:") {
                    if let Some(i) = self.intent_mut(iid) {
                        i.label = value;
                    }
                } else if let Some(iid) = id.strip_prefix("url:") {
                    if let Some(i) = self.intent_mut(iid) {
                        i.url = value;
                    }
                }
                EditorOut::None
            }
            FormEvent::Click(id) => match id.as_str() {
                "save" => {
                    let problems = errors(&self.draft, &self.locale);
                    if problems.is_empty() {
                        self.errors.clear();
                        EditorOut::Save(self.draft.clone())
                    } else {
                        self.errors = problems;
                        EditorOut::Rebuild
                    }
                }
                "close" => EditorOut::Close,
                "reset" => {
                    if self.confirm_reset {
                        EditorOut::Reset
                    } else {
                        self.confirm_reset = true;
                        self.errors = vec![self.t("grammar.confirmReset")];
                        EditorOut::Rebuild
                    }
                }
                "addsearch" | "addchat" => {
                    let search = id == "addsearch";
                    let iid = self.new_id(if search { "search_" } else { "chat_custom_" });
                    self.draft.intents.push(Intent {
                        id: iid,
                        kind: if search { IntentKind::Search } else { IntentKind::WebChat },
                        label: self.t(if search { "grammar.newSearch" } else { "grammar.newChat" }),
                        triggers: vec![if search { "поиск" } else { "чат" }.into()],
                        url: if search { "https://example.com/?q={q}" } else { "https://chatgpt.com/" }.into(),
                        ..Default::default()
                    });
                    EditorOut::Rebuild
                }
                other => {
                    if let Some(iid) = other.strip_prefix("del:") {
                        self.draft.intents.retain(|i| i.id != iid);
                        if self.open == iid {
                            self.open = "general".into();
                        }
                        return EditorOut::Rebuild;
                    }
                    EditorOut::None
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(open: &str) -> GrammarEditor {
        GrammarEditor::new(open, &Grammar::defaults(), &[], "en", "reever", false)
    }

    #[test]
    fn the_last_wake_word_cannot_be_removed() {
        let mut e = ed("general");
        e.draft.wake_words = vec!["reever".into()];
        assert_eq!(e.on_event(FormEvent::Remove("wake".into(), 0)), EditorOut::Rebuild);
        assert_eq!(e.draft.wake_words, ["reever"]);
        assert!(e.items().iter().any(|i| matches!(i, Item::Note(n) if n.contains("at least one"))));
    }

    #[test]
    fn hidden_triggers_survive_edits_and_bare_launch_toggles() {
        let mut g = Grammar::defaults();
        let launch = g.intents.iter_mut().find(|i| i.kind == IntentKind::Launch).unwrap();
        launch.triggers = vec!["".into(), "open".into(), "öffne".into()];
        let id = launch.id.clone();
        let mut e = GrammarEditor::new(&id, &g, &["öffne".to_string()], "en", "reever", false);
        e.on_event(FormEvent::Add(format!("trig:{id}+"), "launch".into()));
        let t = &e.draft.intents.iter().find(|i| i.id == id).unwrap().triggers;
        assert_eq!(t, &["", "open", "launch", "öffne"]);
        e.on_event(FormEvent::Toggle(format!("bare:{id}")));
        let t = &e.draft.intents.iter().find(|i| i.id == id).unwrap().triggers;
        assert_eq!(t, &["open", "launch", "öffne"]);
    }

    #[test]
    fn save_reports_localized_errors_and_new_search_needs_q() {
        let mut e = ed("general");
        e.on_event(FormEvent::Click("addsearch".into()));
        let id = e.draft.intents.last().unwrap().id.clone();
        e.on_event(FormEvent::Edited(format!("url:{id}"), "https://example.com/".into()));
        assert_eq!(e.on_event(FormEvent::Click("save".into())), EditorOut::Rebuild);
        assert!(e.errors.iter().any(|m| m.contains("{q}")), "{:?}", e.errors);
        e.on_event(FormEvent::Edited(format!("url:{id}"), "https://example.com/?q={q}".into()));
        // фраза «поиск» новой команды не должна совпадать с другой
        let r = e.on_event(FormEvent::Click("save".into()));
        assert!(matches!(r, EditorOut::Save(_)) || !e.errors.iter().any(|m| m.contains("{q}")));
    }

    #[test]
    fn reset_asks_first() {
        let mut e = ed("general");
        assert_eq!(e.on_event(FormEvent::Click("reset".into())), EditorOut::Rebuild);
        assert_eq!(e.on_event(FormEvent::Click("reset".into())), EditorOut::Reset);
    }

    #[test]
    fn default_grammar_has_no_errors() {
        assert!(errors(&Grammar::defaults(), "en").is_empty(), "{:?}", errors(&Grammar::defaults(), "en"));
    }
}
