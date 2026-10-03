//! Грамматика голосовых команд, редактируемая пользователем: wake-слова, слова отмены и
//! «намерения» — что сказать после wake-слова, чтобы открыть программу, папку или
//! поиск. Живёт в `grammar.json` рядом с `actions.json` и меняется на лету.
//! Владелец: воркер `grammar`. Типы и `defaults()` — контракт (docs/grammar.md).

use std::path::Path;

use serde::{Deserialize, Serialize};
use strsim::{jaro_winkler, levenshtein};

use crate::actions::Action;
use crate::config::Settings;
use crate::textnorm::{comparison_forms, normalize, strip_leading_fillers};

const GRAMMAR_VERSION: u32 = 8;
/// Версионированные дефолты дописываются старым пользователям, но не восстанавливаются
/// после явного удаления намерения из грамматики актуальной версии.
const ADDED_DEFAULT_INTENTS: &[(u32, &[&str])] = &[
    (2, &["close", "kill"]),
    (3, &["calc"]),
    (4, &["dictate"]),
    (6, &["chat_gemini"]),
    (7, &["chat_gpt", "chat_claude"]),
    (8, &["find", "find_folder", "find_file"]),
];
/// Новые слова отмены: (версия, слово). Дописываются один раз, если их ещё нет.
const ADDED_CANCEL_WORDS: &[(u32, &str)] =
    &[(7, "хватит"), (7, "остановись"), (7, "stop")];
/// Новые фразы у существующих намерений: (версия, id намерения, фраза). Дописываются
/// один раз при миграции, если фраза не занята другим намерением.
const ADDED_DEFAULT_TRIGGERS: &[(u32, &str, &str)] = &[(5, "dictate", "запиши")];

fn legacy_version() -> u32 {
    1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    /// Запуск пользовательского действия (программа, ссылка, папка-шорткат, UWP).
    Launch,
    /// Динамический поиск папки через Everything.
    Folder,
    /// Веб-поиск по шаблону `url` с `{q}`.
    Search,
    /// Мягко закрыть запущенную программу (как крестик).
    Close,
    /// Завершить программу принудительно (как «Снять задачу»).
    Kill,
    /// Посчитать в калькуляторе Windows: «посчитай 33 умножить на 78».
    Calc,
    /// Диктовка: «напиши …» — дальше речь печатается в поле с курсором (или в буфер).
    Dictate,
    /// Веб-чат с ИИ по адресу `url`: открыть и ввести туда сказанное («ривер гемини …»).
    WebChat,
    /// «Найди …» — файл или папка через Everything, выбор в окне Reever.
    Find,
    /// «Найди папку …» — только папки.
    FindFolder,
    /// «Найди файл …» — только файлы.
    FindFile,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Intent {
    /// Стабильный id: `launch`, `folder`, `web_google`, `web_youtube`, пользовательские `search_*`.
    pub id: String,
    pub kind: IntentKind,
    /// Подпись в меню «Что можно сказать».
    pub label: String,
    /// Ключевые фразы после wake-слова. Пустая строка `""` — «без ключевого слова»:
    /// имеет смысл только у `Launch` («ривер фотошоп»).
    pub triggers: Vec<String>,
    /// Только `Search`: шаблон URL, `{q}` заменяется запросом.
    #[serde(default)]
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Язык цели команды для этого намерения: `auto` — решает определитель языка
    /// (и глобальная настройка `payload_language`), `ru`/`en` — всегда этот язык.
    #[serde(default)]
    pub lang: crate::bilingual::LangMode,
}

impl Default for Intent {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: IntentKind::Launch,
            label: String::new(),
            triggers: Vec::new(),
            url: String::new(),
            enabled: true,
            lang: crate::bilingual::LangMode::Auto,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grammar {
    /// Первое слово — каноническое: к нему применяется нечёткое сравнение
    /// (`textnorm::strip_wake`), остальные — точные варианты произношения.
    pub wake_words: Vec<String>,
    pub cancel_words: Vec<String>,
    /// Фразы, завершающие диктовку («стоп ввод», «ривер остановись»). Сравниваются
    /// с фразой целиком — посреди текста они остаются текстом.
    pub dictation_stop: Vec<String>,
    /// Версия схемы для однократного добавления новых дефолтных намерений.
    #[serde(default = "legacy_version")]
    pub version: u32,
    pub intents: Vec<Intent>,
}

impl Default for Grammar {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Что нашлось в начале команды.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerMatch<'a> {
    pub intent: &'a Intent,
    /// Ключевая фраза в том виде, как она записана в грамматике.
    pub trigger: String,
    /// Остаток команды после ключевой фразы, нормализованный; может быть пустым.
    pub payload: String,
}

fn intent(id: &str, kind: IntentKind, label: &str, triggers: &[&str], url: &str) -> Intent {
    Intent {
        id: id.into(),
        kind,
        label: label.into(),
        triggers: triggers.iter().map(|s| s.to_string()).collect(),
        url: url.into(),
        enabled: true,
        lang: crate::bilingual::LangMode::Auto,
    }
}

impl Grammar {
    /// Раскладка по умолчанию (утверждена пользователем 2026-09-24):
    /// «ривер X» — программа/ссылка без глагола; «ривер папка X» / «ривер открой папку X» —
    /// папка; «ривер загугли X» — Google; «ривер поиск ютуб X» — YouTube.
    /// Глаголы «открой»/«запусти» для программ приняты дополнительно, но не обязательны.
    pub fn defaults() -> Self {
        Self {
            version: GRAMMAR_VERSION,
            wake_words: vec![
                "ривер".into(),
                "ревер".into(),
                "ривьер".into(),
                "ливер".into(),
            ],
            cancel_words: vec![
                "отмена".into(),
                "стоп".into(),
                "отбой".into(),
                "забудь".into(),
                "хватит".into(),
                "остановись".into(),
                "stop".into(),
            ],
            dictation_stop: vec!["стоп ввод".into(), "ривер остановись".into()],
            intents: vec![
                intent(
                    "launch",
                    IntentKind::Launch,
                    "открыть программу или ссылку",
                    &["", "открой", "запусти"],
                    "",
                ),
                intent(
                    "folder",
                    IntentKind::Folder,
                    "открыть папку на компьютере (Everything)",
                    &["папка", "открой папку"],
                    "",
                ),
                intent(
                    "web_google",
                    IntentKind::Search,
                    "поиск в Google",
                    &["загугли"],
                    "https://www.google.com/search?q={q}",
                ),
                intent(
                    "web_youtube",
                    IntentKind::Search,
                    "поиск на YouTube",
                    &["поиск ютуб"],
                    "https://www.youtube.com/results?search_query={q}",
                ),
                intent(
                    "close",
                    IntentKind::Close,
                    "закрыть открытую программу",
                    &["закрой"],
                    "",
                ),
                intent(
                    "calc",
                    IntentKind::Calc,
                    "посчитать в калькуляторе",
                    &["посчитай", "сколько будет"],
                    "",
                ),
                intent(
                    "dictate",
                    IntentKind::Dictate,
                    "диктовка: ввести текст в поле с курсором",
                    &["напиши", "запиши", "диктовка"],
                    "",
                ),
                intent(
                    "chat_gemini",
                    IntentKind::WebChat,
                    "спросить Gemini (чат в браузере)",
                    &["гемини", "гимини", "гимине", "джемини", "джимини", "gemini"],
                    "https://gemini.google.com/app",
                ),
                intent(
                    "chat_gpt",
                    IntentKind::WebChat,
                    "спросить ChatGPT (чат в браузере)",
                    &["чат гпт", "чатгпт", "чат джипити", "чат джи пити", "чат жпт", "chatgpt", "chat gpt"],
                    "https://chatgpt.com/",
                ),
                intent(
                    "chat_claude",
                    IntentKind::WebChat,
                    "спросить Claude (чат в браузере)",
                    &["клауд", "клод", "клауде", "клоуд", "claude"],
                    "https://claude.ai/new",
                ),
                intent(
                    "find",
                    IntentKind::Find,
                    "найти файл или папку (окно выбора)",
                    &["найди", "найти", "поищи", "find"],
                    "",
                ),
                intent(
                    "find_folder",
                    IntentKind::FindFolder,
                    "найти папку (окно выбора)",
                    &["найди папку", "найти папку", "поищи папку", "find folder"],
                    "",
                ),
                intent(
                    "find_file",
                    IntentKind::FindFile,
                    "найти файл (окно выбора)",
                    &["найди файл", "найти файл", "поищи файл", "find file"],
                    "",
                ),
                intent(
                    "kill",
                    IntentKind::Kill,
                    "завершить программу (как в диспетчере задач)",
                    &["заверши"],
                    "",
                ),
            ],
        }
    }

    /// Читает `grammar.json`. Файла нет → `defaults()` с `wake_words`/`cancel_words`,
    /// перенесёнными из старых настроек, и поисковыми действиями, перенесёнными из
    /// реестра (они удаляются из `actions`; вернуть `true`, если `actions` изменён).
    /// Битый файл → defaults + предупреждение в stderr, файл не перезаписывается молча.
    pub fn load_or_migrate(
        path: &Path,
        settings: &Settings,
        actions: &mut Vec<Action>,
    ) -> (Self, bool) {
        if path.exists() {
            let loaded = std::fs::read_to_string(path)
                .ok()
                .and_then(|contents| serde_json::from_str::<Self>(&contents).ok());
            if let Some(mut grammar) = loaded {
                grammar
                    .wake_words
                    .retain(|word| !normalize(word).is_empty());
                if grammar.wake_words.is_empty() {
                    grammar.wake_words = Self::defaults().wake_words;
                }
                if grammar.version < GRAMMAR_VERSION {
                    let old_version = grammar.version;
                    append_versioned_defaults(&mut grammar);
                    grammar.version = GRAMMAR_VERSION;
                    if let Err(error) = grammar.save(path) {
                        eprintln!(
                            "[reever] не удалось сохранить миграцию grammar.json v{old_version} → v{GRAMMAR_VERSION}: {error}"
                        );
                    }
                }
                return (grammar, false);
            }

            eprintln!(
                "[reever] не удалось прочитать grammar.json; используются настройки по умолчанию, исходный файл сохранён"
            );
            return (Self::defaults(), false);
        }

        let mut grammar = Self::defaults();
        let configured_wake_words: Vec<String> = settings
            .wake_words
            .iter()
            .filter(|word| !normalize(word).is_empty())
            .cloned()
            .collect();
        if !configured_wake_words.is_empty() {
            grammar.wake_words = configured_wake_words;
            if let Some(river) = grammar
                .wake_words
                .iter()
                .position(|word| normalize(word) == "ривер")
            {
                let canonical = grammar.wake_words.remove(river);
                grammar.wake_words.insert(0, canonical);
            }
        }
        let configured_cancel_words: Vec<String> = settings
            .cancel_words
            .iter()
            .filter(|word| !normalize(word).is_empty())
            .cloned()
            .collect();
        if !configured_cancel_words.is_empty() {
            grammar.cancel_words = configured_cancel_words;
        }

        let mut migrated = false;
        let mut keep = Vec::with_capacity(actions.len());
        for action in actions.drain(..) {
            if action.kind != "search" {
                keep.push(action);
                continue;
            }
            migrated = true;
            if action.id == "web_google" || action.id == "web_youtube" {
                let old_defaults: &[&str] = if action.id == "web_google" {
                    &["загугли", "за гугле", "гугл", "поищи"]
                } else {
                    &["ютуб", "на ютубе"]
                };
                if let Some(intent) = grammar
                    .intents
                    .iter_mut()
                    .find(|intent| intent.id == action.id)
                {
                    for phrase in action.phrases {
                        let normalized = normalize(&phrase);
                        if normalized.is_empty()
                            || old_defaults.iter().any(|old| normalize(old) == normalized)
                        {
                            continue;
                        }
                        if !intent
                            .triggers
                            .iter()
                            .any(|trigger| normalize(trigger) == normalized)
                        {
                            intent.triggers.push(phrase);
                        }
                    }
                }
            } else {
                grammar.intents.push(Intent {
                    id: action.id,
                    kind: IntentKind::Search,
                    label: action.label,
                    triggers: action.phrases,
                    url: action.target,
                    enabled: true,
                    lang: crate::bilingual::LangMode::Auto,
                });
            }
        }
        *actions = keep;
        (grammar, migrated)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(path, json)
    }

    /// Ошибки, из-за которых грамматику нельзя сохранить (тексты — для UI, по-русски):
    /// нет ни одного wake-слова; одинаковая ключевая фраза у двух намерений; `""` не у
    /// `Launch`; `Search` без `{q}` в `url`; пустой id / дубликат id.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self
            .wake_words
            .iter()
            .all(|word| normalize(word).is_empty())
        {
            errors.push("Добавьте хотя бы одно слово активации.".into());
        }

        let mut ids = std::collections::HashSet::new();
        let mut phrases: Vec<(String, usize, String)> = Vec::new();
        for (intent_index, intent) in self.intents.iter().enumerate() {
            if intent.id.trim().is_empty() {
                errors.push(format!(
                    "У намерения «{}» не задан идентификатор.",
                    display_intent(intent)
                ));
            } else if !ids.insert(intent.id.clone()) {
                errors.push(format!(
                    "Идентификатор «{}» используется в нескольких намерениях.",
                    intent.id
                ));
            }
            if intent.kind == IntentKind::WebChat
                && !(intent.url.trim().starts_with("http://")
                    || intent.url.trim().starts_with("https://"))
            {
                errors.push(format!(
                    "Адрес чата «{}» должен начинаться с https://.",
                    display_intent(intent)
                ));
            }
            if intent.kind == IntentKind::Search && !intent.url.contains("{q}") {
                errors.push(format!(
                    "В шаблоне поиска «{}» должен быть маркер {{q}}.",
                    display_intent(intent)
                ));
            }

            for trigger in &intent.triggers {
                let normalized = normalize(trigger);
                if normalized.is_empty() && intent.kind != IntentKind::Launch {
                    errors.push(format!(
                        "Пустая ключевая фраза разрешена только для запуска программы («{}»).",
                        display_intent(intent)
                    ));
                }

                let duplicate = phrases.iter().find(|(phrase, previous_index, _)| {
                    phrase == &normalized && *previous_index != intent_index
                });
                if let Some((_, _, previous_label)) = duplicate {
                    let phrase = if normalized.is_empty() {
                        "без ключевой фразы"
                    } else {
                        normalized.as_str()
                    };
                    errors.push(format!(
                        "Фраза «{}» используется и в «{}», и в «{}».",
                        phrase,
                        previous_label,
                        display_intent(intent)
                    ));
                } else if !phrases.iter().any(|(phrase, previous_index, _)| {
                    phrase == &normalized && *previous_index == intent_index
                }) {
                    phrases.push((normalized, intent_index, display_intent(intent).to_string()));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Самое длинное совпадение ключевой фразы в начале `text` (текст уже без wake).
    /// Только непустые фразы включённых намерений; сравнение нечёткое, как у поисковых
    /// триггеров раньше («за гугле» = «загугли»), и целыми словами.
    pub fn match_trigger(&self, text: &str) -> Option<TriggerMatch<'_>> {
        let normalized = strip_leading_fillers(&normalize(text));
        let words: Vec<&str> = normalized.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }

        let mut best: Option<(&Intent, &str, usize, usize, f64)> = None;
        for intent in self.intents.iter().filter(|intent| intent.enabled) {
            for trigger in &intent.triggers {
                let trigger_words = normalize(trigger);
                let trigger_count = trigger_words.split_whitespace().count();
                if trigger_count == 0 {
                    continue;
                }
                let min_take = trigger_count.saturating_sub(1).max(1);
                let max_take = (trigger_count + 1).min(words.len());
                for take in min_take..=max_take {
                    let score = trigger_score(&words[..take].join(" "), &trigger_words);
                    if score < TRIGGER_THRESHOLD {
                        continue;
                    }
                    let replace =
                        best.as_ref()
                            .map_or(true, |(_, _, best_len, best_take, best_score)| {
                                trigger_count > *best_len
                                    || (trigger_count == *best_len && score > *best_score)
                                    || (trigger_count == *best_len
                                        && score == *best_score
                                        && take < *best_take)
                            });
                    if replace {
                        best = Some((intent, trigger, trigger_count, take, score));
                    }
                }
            }
        }

        best.map(|(intent, trigger, _, consumed, _)| TriggerMatch {
            intent,
            trigger: trigger.to_string(),
            payload: words[consumed..].join(" "),
        })
    }

    /// Включён ли запуск без ключевого слова («ривер фотошоп»).
    pub fn allows_bare_launch(&self) -> bool {
        self.intents.iter().any(|i| {
            i.enabled
                && i.kind == IntentKind::Launch
                && i.triggers
                    .iter()
                    .any(|trigger| normalize(trigger).is_empty())
        })
    }

    /// Фраза — только ключевое слово без цели («папка», «загугли», «поиск ютуб»,
    /// «открой»): Engine должен дослушать продолжение, а не выполнять обрубок.
    pub fn is_incomplete(&self, text: &str) -> bool {
        self.match_trigger(text)
            .is_some_and(|matched| matched.payload.is_empty())
    }
}

fn append_versioned_defaults(grammar: &mut Grammar) {
    for (introduced_version, word) in ADDED_CANCEL_WORDS {
        if grammar.version < *introduced_version
            && !grammar
                .cancel_words
                .iter()
                .any(|existing| normalize(existing) == normalize(word))
        {
            grammar.cancel_words.push((*word).to_string());
        }
    }
    for (introduced_version, id, phrase) in ADDED_DEFAULT_TRIGGERS {
        if grammar.version >= *introduced_version {
            continue;
        }
        let wanted = normalize(phrase);
        let taken = grammar
            .intents
            .iter()
            .any(|intent| intent.triggers.iter().any(|trigger| normalize(trigger) == wanted));
        if taken {
            continue;
        }
        if let Some(intent) = grammar.intents.iter_mut().find(|intent| intent.id == *id) {
            intent.triggers.push((*phrase).to_string());
        }
    }
    let defaults = Grammar::defaults();
    for (introduced_version, ids) in ADDED_DEFAULT_INTENTS {
        if grammar.version >= *introduced_version {
            continue;
        }
        for id in *ids {
            if grammar.intents.iter().any(|existing| existing.id == *id) {
                continue;
            }
            let Some(mut addition) = defaults
                .intents
                .iter()
                .find(|intent| intent.id == *id)
                .cloned()
            else {
                continue;
            };

            let mut conflicts = Vec::new();
            addition.triggers.retain(|trigger| {
                let phrase = normalize(trigger);
                let conflict = grammar.intents.iter().any(|existing| {
                    existing
                        .triggers
                        .iter()
                        .any(|existing_trigger| normalize(existing_trigger) == phrase)
                });
                if conflict {
                    conflicts.push(phrase);
                    false
                } else {
                    true
                }
            });
            if !conflicts.is_empty() {
                eprintln!(
                    "[reever] не добавляю конфликтующие фразы {:?} для нового намерения {}; его нужно настроить вручную",
                    conflicts, addition.id
                );
            }
            if addition.triggers.is_empty() || !conflicts.is_empty() {
                addition.enabled = false;
            }
            grammar.intents.push(addition);
        }
    }
}

const TRIGGER_THRESHOLD: f64 = 0.84;

fn display_intent(intent: &Intent) -> &str {
    if intent.label.trim().is_empty() {
        &intent.id
    } else {
        &intent.label
    }
}

/// Сопоставляет целые токены, но допускает словоформы и разное склеивание ASR.
fn trigger_score(candidate: &str, trigger: &str) -> f64 {
    let candidate = normalize(candidate);
    let trigger = normalize(trigger);
    if candidate.is_empty() || trigger.is_empty() {
        return 0.0;
    }
    if morphology_form(&candidate) == morphology_form(&trigger) {
        return 1.0;
    }

    let mut best = 0.0_f64;
    let candidate_forms = comparison_forms(&candidate);
    let trigger_forms = comparison_forms(&trigger);
    for left in &candidate_forms {
        for right in &trigger_forms {
            best = best.max(aligned_phrase_score(left, right));

            let left_words: Vec<&str> = left.split_whitespace().collect();
            let right_words: Vec<&str> = right.split_whitespace().collect();
            if left_words.len().abs_diff(right_words.len()) == 1 {
                // В распознанной фразе часто появляется короткое «в» перед площадкой:
                // «поиск в ютубе» и «поиск ютуб» обозначают один триггер.
                for optional in ["в", "на"] {
                    if left_words.len() > right_words.len() {
                        if let Some(reduced) = remove_word(&left_words, optional) {
                            best = best.max(aligned_phrase_score(&reduced, &right_words.join(" ")));
                        }
                    } else if let Some(reduced) = remove_word(&right_words, optional) {
                        best = best.max(aligned_phrase_score(&left_words.join(" "), &reduced));
                    }
                }

                // Допускаем склейку соседних слов ASR («за гугле»), но сравниваем всю
                // склейку с порогом и ограничением на число исправленных букв.
                let joined_left = left.replace(' ', "");
                let joined_right = right.replace(' ', "");
                best = best.max(safe_word_similarity(&joined_left, &joined_right));
            }
        }
    }
    best
}

fn aligned_phrase_score(left: &str, right: &str) -> f64 {
    let left_words: Vec<&str> = left.split_whitespace().collect();
    let right_words: Vec<&str> = right.split_whitespace().collect();
    if left_words.is_empty() || left_words.len() != right_words.len() {
        return 0.0;
    }
    let mut total = 0.0;
    for (left, right) in left_words.iter().zip(right_words.iter()) {
        let score = safe_word_similarity(left, right);
        if score < TRIGGER_THRESHOLD {
            return 0.0;
        }
        total += score;
    }
    total / left_words.len() as f64
}

fn safe_word_similarity(left: &str, right: &str) -> f64 {
    if morphology_form(left) == morphology_form(right) {
        return 1.0;
    }
    let similarity = jaro_winkler(left, right);
    let max_len = left.chars().count().max(right.chars().count());
    let allowed_edits = (max_len / 8).max(1);
    if similarity >= TRIGGER_THRESHOLD && levenshtein(left, right) <= allowed_edits {
        similarity
    } else {
        0.0
    }
}

fn remove_word(words: &[&str], optional: &str) -> Option<String> {
    let index = words.iter().position(|word| *word == optional)?;
    let mut remaining = words.to_vec();
    remaining.remove(index);
    Some(remaining.join(" "))
}

fn morphology_form(text: &str) -> String {
    text.split_whitespace()
        .map(stem_word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Ограниченное снятие русских окончаний: различает «папку/папке/папки» от «папаша».
fn stem_word(word: &str) -> String {
    const ENDINGS: [&str; 12] = [
        "иями", "ями", "ами", "ов", "ев", "ом", "ем", "ой", "ый", "ая", "яя", "ое",
    ];
    if !word
        .chars()
        .all(|ch| ('а'..='я').contains(&ch) || ch == 'ё')
    {
        return word.to_string();
    }
    for ending in ENDINGS
        .into_iter()
        .chain(["ее", "ы", "и", "а", "у", "ю", "е", "я", "ь", "й"])
    {
        if word.ends_with(ending) && word.chars().count() > ending.chars().count() + 2 {
            return word[..word.len() - ending.len()].to_string();
        }
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Grammar, Intent, IntentKind};
    use crate::actions::Action;
    use crate::config::Settings;

    fn intent_mut<'a>(grammar: &'a mut Grammar, id: &str) -> &'a mut Intent {
        grammar
            .intents
            .iter_mut()
            .find(|intent| intent.id == id)
            .unwrap()
    }

    fn temporary_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "reever-grammar-{label}-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn action(id: &str, kind: &str, target: &str, phrases: &[&str]) -> Action {
        Action {
            id: id.into(),
            label: id.into(),
            kind: kind.into(),
            target: target.into(),
            verbs: Vec::new(),
            phrases: phrases.iter().map(|phrase| phrase.to_string()).collect(),
        }
    }

    #[test]
    fn defaults_are_valid() {
        assert_eq!(Grammar::defaults().validate(), Ok(()));
    }

    #[test]
    fn matches_longest_prefix_and_keeps_normalized_payload() {
        let grammar = Grammar::defaults();
        for (text, intent_id, payload) in [
            ("папка документы", "folder", "документы"),
            ("открой папку документы", "folder", "документы"),
            ("загугли погода", "web_google", "погода"),
            ("за гугле погода", "web_google", "погода"),
            ("поиск ютуб коты", "web_youtube", "коты"),
            ("поиск в ютубе коты", "web_youtube", "коты"),
            ("открой фотошоп", "launch", "фотошоп"),
        ] {
            let matched = grammar
                .match_trigger(text)
                .unwrap_or_else(|| panic!("{text}"));
            assert_eq!(matched.intent.id, intent_id, "{text}");
            assert_eq!(matched.payload, payload, "{text}");
        }
        assert!(grammar.match_trigger("фотошоп").is_none());
    }

    #[test]
    fn matching_respects_enabled_state_and_russian_noun_forms() {
        let mut grammar = Grammar::defaults();
        for form in ["папку", "папке", "папки", "папками"] {
            assert_eq!(
                grammar
                    .match_trigger(&format!("{form} загрузки"))
                    .unwrap()
                    .intent
                    .id,
                "folder"
            );
        }
        assert!(grammar.match_trigger("папаша документы").is_none());

        intent_mut(&mut grammar, "folder").enabled = false;
        assert!(grammar.match_trigger("папка документы").is_none());
    }

    #[test]
    fn incomplete_is_only_a_trigger_without_a_payload() {
        let grammar = Grammar::defaults();
        for phrase in [
            "папка",
            "поиск ютуб",
            "загугли",
            "посчитай",
            "сколько будет",
            "открой",
            "закрой",
            "заверши",
        ] {
            assert!(grammar.is_incomplete(phrase), "{phrase}");
        }
        for phrase in [
            "фотошоп",
            "папка документы",
            "загугли погода",
            "закрой хром",
            "заверши телеграм",
            "посчитай 2 плюс 2",
        ] {
            assert!(!grammar.is_incomplete(phrase), "{phrase}");
        }
    }

    #[test]
    fn validate_reports_each_contract_violation_in_russian() {
        let mut grammar = Grammar::defaults();
        grammar.wake_words = vec!["".into(), "!!!".into()];
        intent_mut(&mut grammar, "web_google")
            .triggers
            .push(String::new());
        intent_mut(&mut grammar, "web_google")
            .triggers
            .push(" ПАПКА! ".into());
        intent_mut(&mut grammar, "web_google").url = "https://google.com".into();
        grammar.intents.push(Intent {
            id: String::new(),
            kind: IntentKind::Launch,
            label: "Без id".into(),
            triggers: vec!["новая фраза".into()],
            url: String::new(),
            enabled: true,
            lang: crate::bilingual::LangMode::Auto,
        });
        grammar.intents.push(Intent {
            id: "launch".into(),
            kind: IntentKind::Launch,
            label: "Дубль id".into(),
            triggers: vec!["другая фраза".into()],
            url: String::new(),
            enabled: true,
            lang: crate::bilingual::LangMode::Auto,
        });

        let errors = grammar.validate().unwrap_err().join("\n");
        for expected in [
            "слово активации",
            "папка",
            "Пустая ключевая фраза",
            "маркер {q}",
            "не задан идентификатор",
            "Идентификатор «launch»",
        ] {
            assert!(
                errors.contains(expected),
                "missing {expected:?} in {errors}"
            );
        }
        assert!(errors.contains("и в «открыть папку"), "{errors}");
        assert!(errors.contains("поиск в Google"), "{errors}");
    }

    #[test]
    fn close_and_kill_migration_adds_defaults_once_for_version_one() {
        let path = temporary_path("v1-control");
        let mut legacy = Grammar::defaults();
        legacy.version = 1;
        legacy
            .intents
            .retain(|intent| intent.id != "close" && intent.id != "kill" && intent.id != "calc");
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let mut actions = Vec::new();
        let (migrated, actions_changed) =
            Grammar::load_or_migrate(&path, &Settings::default(), &mut actions);
        assert!(!actions_changed);
        assert_eq!(migrated.version, super::GRAMMAR_VERSION);
        assert_eq!(
            migrated
                .intents
                .iter()
                .filter(|intent| intent.id == "close")
                .count(),
            1
        );
        assert_eq!(
            migrated
                .intents
                .iter()
                .filter(|intent| intent.id == "kill")
                .count(),
            1
        );
        assert_eq!(
            migrated
                .intents
                .iter()
                .filter(|intent| intent.id == "calc")
                .count(),
            1
        );
        assert!(migrated.intents.iter().any(|intent| {
            intent.id == "close"
                && intent.triggers.len() == 1
                && intent.triggers[0] == "закрой"
                && intent.enabled
        }));
        assert!(migrated.intents.iter().any(|intent| {
            intent.id == "kill"
                && intent.triggers.len() == 1
                && intent.triggers[0] == "заверши"
                && intent.enabled
        }));
        assert!(migrated.intents.iter().any(|intent| {
            intent.id == "calc"
                && intent.triggers == ["посчитай", "сколько будет"]
                && intent.enabled
        }));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(&format!("\"version\": {}", super::GRAMMAR_VERSION)));

        let (reloaded, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(reloaded, migrated);
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "close")
                .count(),
            1
        );
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "kill")
                .count(),
            1
        );
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "calc")
                .count(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_disables_a_default_intent_when_its_trigger_conflicts() {
        let path = temporary_path("v1-conflict");
        let mut legacy = Grammar::defaults();
        legacy.version = 1;
        legacy
            .intents
            .retain(|intent| intent.id != "close" && intent.id != "kill");
        intent_mut(&mut legacy, "launch")
            .triggers
            .push("закрой".into());
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let (migrated, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        let close = migrated
            .intents
            .iter()
            .find(|intent| intent.id == "close")
            .unwrap();
        assert!(!close.enabled);
        assert!(close.triggers.is_empty());
        assert!(migrated
            .intents
            .iter()
            .any(|intent| intent.id == "kill" && intent.enabled));
        assert!(migrated.validate().is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_two_migration_adds_calc_and_disables_it_on_phrase_conflict() {
        let path = temporary_path("v2-calc-conflict");
        let mut legacy = Grammar::defaults();
        legacy.version = 2;
        legacy.intents.retain(|intent| intent.id != "calc");
        intent_mut(&mut legacy, "launch")
            .triggers
            .push("посчитай".into());
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let (migrated, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(migrated.version, super::GRAMMAR_VERSION);
        let calc = migrated
            .intents
            .iter()
            .find(|intent| intent.id == "calc")
            .unwrap();
        assert!(!calc.enabled);
        assert_eq!(calc.triggers, ["сколько будет"]);
        assert!(migrated.validate().is_ok());
        let (reloaded, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(reloaded, migrated);
        assert_eq!(
            reloaded
                .intents
                .iter()
                .filter(|intent| intent.id == "calc")
                .count(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn current_version_does_not_restore_intents_the_user_removed() {
        let path = temporary_path("v3-deleted-control");
        let mut current = Grammar::defaults();
        current
            .intents
            .retain(|intent| intent.id != "close" && intent.id != "kill" && intent.id != "calc");
        std::fs::write(&path, serde_json::to_string_pretty(&current).unwrap()).unwrap();

        let (loaded, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(loaded.version, super::GRAMMAR_VERSION);
        assert!(!loaded
            .intents
            .iter()
            .any(|intent| intent.id == "close" || intent.id == "kill" || intent.id == "calc"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn close_and_kill_reject_empty_key_phrases() {
        let mut grammar = Grammar::defaults();
        intent_mut(&mut grammar, "close").triggers = vec![String::new()];
        intent_mut(&mut grammar, "kill").triggers = vec!["   ".into()];
        let errors = grammar.validate().unwrap_err().join("\n");
        assert!(errors.contains("закрыть открытую программу"));
        assert!(errors.contains("завершить программу"));
    }

    #[test]
    fn calc_rejects_empty_key_phrases() {
        let mut grammar = Grammar::defaults();
        intent_mut(&mut grammar, "calc").triggers = vec![String::new()];
        let errors = grammar.validate().unwrap_err().join("\n");
        assert!(errors.contains("посчитать в калькуляторе"));
    }

    #[test]
    fn existing_json_uses_defaults_for_missing_fields_and_empty_wake_list() {
        assert_eq!(Grammar::defaults().version, 8);
        assert_eq!(
            serde_json::from_str::<Grammar>(r#"{"intents":[]}"#)
                .unwrap()
                .version,
            1
        );
        let path = temporary_path("partial");
        std::fs::write(&path, r#"{"intents":[]}"#).unwrap();
        let (loaded, migrated) =
            Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert!(!migrated);
        assert_eq!(loaded.wake_words, Grammar::defaults().wake_words);
        assert_eq!(loaded.cancel_words, Grammar::defaults().cancel_words);
        std::fs::write(&path, r#"{"wake_words":[],"cancel_words":[],"intents":[]}"#).unwrap();
        let (loaded, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(loaded.wake_words, Grammar::defaults().wake_words);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_replaces_legacy_search_triggers_and_keeps_custom_phrases() {
        let path = temporary_path("migrate");
        let mut settings = Settings::default();
        settings.wake_words = vec!["речной".into(), "ривер".into(), "река".into()];
        settings.cancel_words = vec!["не надо".into()];
        let mut actions = vec![
            action(
                "web_google",
                "search",
                "https://old.google/?q={q}",
                &["за гугле", "гугл", "гугли новости"],
            ),
            action(
                "web_youtube",
                "search",
                "https://old.youtube/?q={q}",
                &["ютуб", "на ютубе", "найди ролики"],
            ),
            action(
                "search_custom",
                "search",
                "https://example.com/?q={q}",
                &["найди рецепт"],
            ),
            action("app", "shell", "app.exe", &["приложение"]),
        ];

        let (grammar, changed) = Grammar::load_or_migrate(&path, &settings, &mut actions);
        assert!(changed);
        assert_eq!(grammar.wake_words[0], "ривер");
        assert_eq!(grammar.cancel_words, vec!["не надо"]);
        assert_eq!(
            actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec!["app"]
        );
        let google = grammar
            .intents
            .iter()
            .find(|intent| intent.id == "web_google")
            .unwrap();
        assert_eq!(google.triggers, vec!["загугли", "гугли новости"]);
        assert_eq!(google.url, "https://www.google.com/search?q={q}");
        let youtube = grammar
            .intents
            .iter()
            .find(|intent| intent.id == "web_youtube")
            .unwrap();
        assert_eq!(youtube.triggers, vec!["поиск ютуб", "найди ролики"]);
        let custom = grammar
            .intents
            .iter()
            .find(|intent| intent.id == "search_custom")
            .unwrap();
        assert_eq!(custom.label, "search_custom");
        assert_eq!(custom.url, "https://example.com/?q={q}");
        assert_eq!(custom.triggers, vec!["найди рецепт"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corrupt_json_returns_defaults_without_rewriting_the_file() {
        let path = temporary_path("corrupt");
        let corrupt = "not valid json";
        std::fs::write(&path, corrupt).unwrap();
        let mut actions = vec![action(
            "web_google",
            "search",
            "https://google/?q={q}",
            &["загугли"],
        )];
        let (grammar, changed) =
            Grammar::load_or_migrate(&path, &Settings::default(), &mut actions);
        assert!(!changed);
        assert_eq!(grammar, Grammar::defaults());
        assert_eq!(
            actions.len(),
            1,
            "битый файл не должен запускать миграцию действий"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_four_migration_adds_zapishi_to_dictation_once() {
        let path = temporary_path("v4-zapishi");
        let mut legacy = Grammar::defaults();
        legacy.version = 4;
        intent_mut(&mut legacy, "dictate").triggers = vec!["напиши".into(), "диктовка".into()];
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let (migrated, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(
            migrated.intents.iter().find(|i| i.id == "dictate").unwrap().triggers,
            ["напиши", "диктовка", "запиши"]
        );
        let (reloaded, _) = Grammar::load_or_migrate(&path, &Settings::default(), &mut Vec::new());
        assert_eq!(reloaded, migrated);
        let _ = std::fs::remove_file(path);
    }
}
