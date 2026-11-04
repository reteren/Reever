/// Строгая грамматика голосовых команд Reever.
use crate::textnorm::{normalize, strip_leading_fillers};

#[derive(Debug, PartialEq, Eq)]
pub enum ReeverCommand {
    Open(String),   // «открой <X>» → папки или личные действия
    Launch(String), // «запусти <X>» → программы/игры
}

const OPEN_VERBS: &[&str] = &[
    "перейди в",
    "зайди в",
    "открывайте",
    "открывай",
    "откройте",
    "открыть",
    "покажите",
    "покажи",
    "найдите",
    "найти",
    "найди",
    "открой",
    "открои",
    "открыть",
];

const LAUNCH_VERBS: &[&str] = &[
    "запускайте",
    "запускай",
    "запустите",
    "запустить",
    "включайте",
    "включай",
    "включите",
    "включить",
    "запусти",
    "включи",
    "вруби",
    "стартани",
];

/// Разбирает нормализованный GigaAM-текст; пробелы в глаголах значимы.
pub fn parse(text: &str) -> Option<ReeverCommand> {
    let command = clean_command_text(text);
    if command.is_empty() {
        return None;
    }

    if let Some(rest) = strip_verb(&command, OPEN_VERBS) {
        if !rest.is_empty() {
            return Some(ReeverCommand::Open(rest));
        }
    }
    if let Some(rest) = strip_verb(&command, LAUNCH_VERBS) {
        if !rest.is_empty() {
            return Some(ReeverCommand::Launch(rest));
        }
    }

    // В разговоре глагол часто ставится после названия: «фотошоп открой».
    if let Some(target) = strip_trailing_verb(&command, OPEN_VERBS) {
        if !target.is_empty() {
            return Some(ReeverCommand::Open(clean_target(&target)));
        }
    }
    if let Some(target) = strip_trailing_verb(&command, LAUNCH_VERBS) {
        if !target.is_empty() {
            return Some(ReeverCommand::Launch(clean_target(&target)));
        }
    }
    None
}

/// Поисковые триггеры, после которых обязательно идёт запрос. Дублируют фразы
/// дефолтных search-действий: Engine решает «команда ещё не договорена» без реестра.
const SEARCH_TRIGGERS: &[&str] = &[
    "загугли",
    "погугли",
    "гугл",
    "поищи",
    "найди в интернете",
    "ютуб",
    "на ютубе",
    "в ютубе",
];

/// `true`, если фраза — начало команды без цели: голый глагол («открой»), глагол +
/// «папку» («открой папку»), поисковый триггер без запроса («загугли») или калькулятор
/// без выражения («посчитай»). После такого Engine дослушивает продолжение.
pub fn is_bare_verb(text: &str) -> bool {
    let command = clean_command_text(text);
    let is_verb = |t: &str| {
        OPEN_VERBS
            .iter()
            .chain(LAUNCH_VERBS)
            .any(|verb| t == normalize(verb))
    };
    const CALC_TRIGGERS: &[&str] = &["посчитай", "сколько будет"];
    if is_verb(&command)
        || SEARCH_TRIGGERS.iter().any(|t| command == normalize(t))
        || CALC_TRIGGERS.iter().any(|t| command == normalize(t))
    {
        return true;
    }
    // «открой папку» — цель ещё впереди
    command
        .rsplit_once(' ')
        .is_some_and(|(verb, last)| matches!(last, "папку" | "папка" | "папке") && is_verb(verb))
}

/// Срезает только целые слова в начале, включая составные глаголы.
fn strip_verb(text: &str, verbs: &[&str]) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for verb in verbs {
        let verb_words: Vec<&str> = verb.split_whitespace().collect();
        if words.starts_with(&verb_words) {
            return Some(clean_target(&words[verb_words.len()..].join(" ")));
        }
    }
    None
}

fn strip_trailing_verb(text: &str, verbs: &[&str]) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for verb in verbs {
        let verb_words: Vec<&str> = verb.split_whitespace().collect();
        if words.ends_with(&verb_words) {
            return Some(words[..words.len() - verb_words.len()].join(" "));
        }
    }
    None
}

fn clean_command_text(text: &str) -> String {
    let normalized = strip_leading_fillers(&normalize(text));
    let mut words: Vec<&str> = normalized.split_whitespace().collect();
    while words.first().is_some_and(|word| {
        matches!(
            *word,
            "мне" | "пожалуйста" | "а" | "ну" | "давай" | "слушай"
        )
    }) {
        words.remove(0);
    }
    while words
        .last()
        .is_some_and(|word| matches!(*word, "мне" | "пожалуйста"))
    {
        words.pop();
    }
    words.join(" ")
}

fn clean_target(text: &str) -> String {
    clean_command_text(text)
}

/// Что открыть у самого Reever по «открой ривер» / «открой настройки ривера».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfPage {
    /// Главное окно (как щелчок по значку в трее).
    Window,
    /// Главное окно на вкладке «Настройки».
    Settings,
}

const SELF_VERBS: &[&str] = &[
    "открой", "открои", "откройте", "открыть", "покажи", "покажите", "разверни", "верни", "open", "show",
];
const SELF_SETTINGS: &[&str] = &["настройки", "настроики", "настройку", "settings", "setting"];
const SELF_WINDOW: &[&str] = &["окно", "меню", "менюшку", "window", "menu", "app"];

/// «открой ривер», «покажи окно ривера», «открой настройки ривера», «open reever settings»:
/// команда самому Reever. `wake_words` — как в настройках (первое — каноническое «ривер»).
/// Обращение в начале фразы Engine уже снял: сюда приходит «открой ривер» из «ривер, открой
/// ривер».
pub fn opens_self(text: &str, wake_words: &[String]) -> Option<SelfPage> {
    let normalized = strip_leading_fillers(&normalize(text));
    let mut words: Vec<&str> = normalized.split_whitespace().collect();
    if words.is_empty() || !SELF_VERBS.contains(&words[0]) {
        return None;
    }
    words.remove(0);
    let mut page = SelfPage::Window;
    words.retain(|w| {
        if SELF_SETTINGS.contains(w) {
            page = SelfPage::Settings;
            false
        } else {
            !SELF_WINDOW.contains(w) && !matches!(*w, "мне" | "пожалуйста" | "please" | "the" | "my")
        }
    });
    // осталось одно слово — само имя: «ривер», падежом «ривера» / «риверу», латиницей
    let [name] = words.as_slice() else { return None };
    let is_wake = |w: &str| crate::textnorm::strip_wake(w, wake_words).is_some_and(|rest| rest.is_empty());
    let stem = name.strip_suffix(['а', 'у']).unwrap_or(name);
    (is_wake(name) || (stem.chars().count() >= 4 && is_wake(stem))).then_some(page)
}

#[cfg(test)]
mod tests {
    use super::{is_bare_verb, opens_self, parse, ReeverCommand, SelfPage};

    #[test]
    fn open_self_window_and_settings() {
        let wake = vec!["ривер".to_string(), "reever".to_string(), "river".to_string()];
        assert_eq!(opens_self("открой ривер", &wake), Some(SelfPage::Window));
        assert_eq!(opens_self("Покажи окно ривера", &wake), Some(SelfPage::Window));
        assert_eq!(opens_self("открой меню ривера", &wake), Some(SelfPage::Window));
        assert_eq!(opens_self("открой настройки ривера", &wake), Some(SelfPage::Settings));
        assert_eq!(opens_self("open reever", &wake), Some(SelfPage::Window));
        assert_eq!(opens_self("open river settings", &wake), Some(SelfPage::Settings));
        assert_eq!(opens_self("открой фотошоп", &wake), None);
        assert_eq!(opens_self("открой ривер загрузки", &wake), None);
        assert_eq!(opens_self("открой реверс", &wake), None);
        assert_eq!(opens_self("открой", &wake), None);
    }

    #[test]
    fn parse_matches_whole_verb_words_and_polite_forms() {
        assert_eq!(
            parse("откройте загрузки"),
            Some(ReeverCommand::Open("загрузки".into()))
        );
        assert_eq!(
            parse("пожалуйста откройте мне загрузки"),
            Some(ReeverCommand::Open("загрузки".into()))
        );
        assert_eq!(
            parse("открой пожалуйста фотошоп"),
            Some(ReeverCommand::Open("фотошоп".into()))
        );
        assert_eq!(
            parse("запусти пожалуйста фотошоп"),
            Some(ReeverCommand::Launch("фотошоп".into()))
        );
        assert_eq!(
            parse("запустите дота два"),
            Some(ReeverCommand::Launch("дота 2".into()))
        );
        assert_eq!(
            parse("включите дискорд"),
            Some(ReeverCommand::Launch("дискорд".into()))
        );
    }

    #[test]
    fn parse_supports_search_and_reverse_verb_grammar() {
        assert_eq!(
            parse("найди настройки"),
            Some(ReeverCommand::Open("настройки".into()))
        );
        assert_eq!(
            parse("покажи загрузки"),
            Some(ReeverCommand::Open("загрузки".into()))
        );
        assert_eq!(
            parse("перейди в загрузки"),
            Some(ReeverCommand::Open("загрузки".into()))
        );
        assert_eq!(
            parse("зайди в загрузки"),
            Some(ReeverCommand::Open("загрузки".into()))
        );
        assert_eq!(
            parse("фотошоп открой"),
            Some(ReeverCommand::Open("фотошоп".into()))
        );
        assert_eq!(
            parse("дискорд запусти"),
            Some(ReeverCommand::Launch("дискорд".into()))
        );
    }

    #[test]
    fn parse_does_not_cut_a_verb_from_the_middle_of_a_word() {
        assert_eq!(
            parse("откройте загрузки"),
            Some(ReeverCommand::Open("загрузки".into()))
        );
        assert_eq!(parse("переоткройте загрузки"), None);
        assert_eq!(
            parse("найди открытую папку"),
            Some(ReeverCommand::Open("открытую папку".into()))
        );
        assert_eq!(parse("открой"), None);
    }

    #[test]
    fn bare_verb_allows_only_polite_fillers() {
        for text in [
            "открой",
            "откройте",
            "пожалуйста открой",
            "открой мне",
            "ну, запустите, пожалуйста",
        ] {
            assert!(is_bare_verb(text), "{text}");
        }
        assert!(!is_bare_verb("открой фотошоп"));
        assert!(!is_bare_verb("пожалуйста, открой фотошоп"));
        assert!(!is_bare_verb("открытая загрузка"));
        // незаконченные поиск и папка тоже ждут продолжения
        assert!(is_bare_verb("загугли"));
        assert!(is_bare_verb("ютуб"));
        assert!(is_bare_verb("посчитай"));
        assert!(is_bare_verb("сколько будет"));
        assert!(!is_bare_verb("посчитай 33 умножить на 78"));
        assert!(is_bare_verb("открой папку"));
        assert!(!is_bare_verb("загугли рецепт блинов"));
        assert!(!is_bare_verb("открой папку документы"));
    }
}
