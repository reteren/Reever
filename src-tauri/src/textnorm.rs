//! Нормализация распознанного текста и wake/отмена/согласие. Владелец: воркер `text`.

use strsim::jaro_winkler;

const WAKE_FUZZY_THRESHOLD: f64 = 0.86;

/// Общая форма распознанного текста: нижний регистр, `ё`→`е`, пунктуация → пробел,
/// числа словами → числа цифрами, одиночные пробелы и `trim`.
pub fn normalize(text: &str) -> String {
    let cleaned: String = text
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    normalize_number_words(&words)
}

/// Ищет wake-слово только среди первых двух слов и возвращает нормализованный хвост.
/// Fuzzy-порог пропускает типичные ошибки ASR, но требует похожую длину слова, чтобы
/// не принимать самостоятельные слова вроде «реверс» или «ривьера».
pub fn strip_wake(text: &str, wake_words: &[String]) -> Option<String> {
    let normalized = normalize(text);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    for (i, word) in words.iter().take(2).enumerate() {
        let exact = wake_words.iter().any(|wake| wake_exact(word, wake));
        // Список wake_words хранит допустимые варианты, но fuzzy-якорь один: первое
        // слово (каноническое «ривер»). Иначе «ревёт» становится похожим на вариант
        // «ревер», хотя сам по себе wake-словом не является.
        let fuzzy = wake_words
            .first()
            .is_some_and(|wake| wake_fuzzy_matches(word, wake));
        if exact || fuzzy {
            return Some(words[i + 1..].join(" "));
        }
        // ASR склеивает короткое «ривер плей» в «риверпли», а шум перед фразой даёт
        // лишнюю букву: «тривер». Только для канонического wake — иначе «реверсивный»
        // стал бы вариантом «ревер».
        if let Some(wake) = wake_words.first() {
            if let Some(glued) = glued_wake_tail(word, wake) {
                let mut rest = vec![glued];
                rest.extend(words[i + 1..].iter().map(|w| w.to_string()));
                return Some(rest.join(" ").trim().to_string());
            }
        }
    }
    None
}

/// `Some(хвост)`, если слово — wake с приклеенным продолжением («риверпли» → «пли»,
/// хвост от 2 букв, чтобы не ловить «ривера») или с 1–2 лишними буквами спереди
/// («тривер» → «»).
fn glued_wake_tail(word: &str, wake: &str) -> Option<String> {
    let wake = normalize(wake);
    if wake.chars().count() < 4 {
        return None;
    }
    if let Some(tail) = word.strip_prefix(wake.as_str()) {
        if tail.chars().count() >= 2 {
            return Some(tail.to_string());
        }
    }
    let lead = word.strip_suffix(wake.as_str())?;
    (1..=2).contains(&lead.chars().count()).then(String::new)
}

/// Есть ли в фразе слово отмены, с учётом частых русских словоформ.
pub fn is_cancel(text: &str, cancel_words: &[String]) -> bool {
    let words: Vec<String> = normalize(text)
        .split_whitespace()
        .map(stem_russian_word)
        .collect();
    cancel_words.iter().any(|cancel| {
        normalize(cancel)
            .split_whitespace()
            .map(stem_russian_word)
            .any(|stem| words.iter().any(|word| word == &stem))
    })
}

/// Согласие на подтверждение опасной команды («да», «ага», …), включая словоформы.
pub fn is_yes(text: &str) -> bool {
    normalize(text).split_whitespace().any(|word| {
        matches!(
            word,
            "да" | "ага" | "угу" | "точно" | "давай" | "давайте" | "подтверждаю" | "подтверди"
        ) || word.starts_with("дава")
            || word.starts_with("подтвержд")
            || word.starts_with("подтверд")
            || word.starts_with("соглас")
            || word.starts_with("конеч")
    })
}

/// Удаляет вводные слова только в начале фразы. Используется для сравнения и разбора
/// вежливых команд, чтобы слова внутри названия действия не пропадали.
pub(crate) fn strip_leading_fillers(text: &str) -> String {
    let fillers = ["пожалуйста", "а", "ну", "давай", "слушай"];
    let mut words = normalize(text);
    loop {
        let Some((first, rest)) = words.split_once(' ') else {
            if fillers.contains(&words.as_str()) {
                words.clear();
            }
            break;
        };
        if fillers.contains(&first) {
            words = rest.to_string();
        } else {
            break;
        }
    }
    words
}

/// Варианты одной фразы для сравнения калибровки Vosk с транскриптом GigaAM:
/// исходный текст, приближённая транслитерация и канонизация произнесённых букв.
pub(crate) fn comparison_forms(text: &str) -> Vec<String> {
    let normalized = normalize(text);
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut forms = Vec::new();
    push_unique(&mut forms, normalized.clone());
    push_unique(&mut forms, canonicalize_letter_names(&normalized));

    let transliterated = normalize(&latin_to_cyrillic(&normalized));
    push_unique(&mut forms, transliterated.clone());
    push_unique(&mut forms, canonicalize_letter_names(&transliterated));

    let without_fillers = strip_leading_fillers(&normalized);
    if !without_fillers.is_empty() && without_fillers != normalized {
        push_unique(&mut forms, without_fillers.clone());
        let transliterated = normalize(&latin_to_cyrillic(&without_fillers));
        push_unique(&mut forms, transliterated.clone());
        push_unique(&mut forms, canonicalize_letter_names(&transliterated));
    }
    forms
}

fn wake_exact(word: &str, wake: &str) -> bool {
    let word_forms = comparison_forms(word);
    let wake_forms = comparison_forms(wake);
    for word_form in &word_forms {
        for wake_form in &wake_forms {
            if word_form == wake_form {
                return true;
            }
        }
    }
    false
}

fn wake_fuzzy_matches(word: &str, wake: &str) -> bool {
    let word_forms = comparison_forms(word);
    let wake_forms = comparison_forms(wake);
    for word_form in &word_forms {
        for wake_form in &wake_forms {
            if word_form == wake_form {
                continue;
            }
            let word_len = word_form.chars().count();
            let wake_len = wake_form.chars().count();
            if word_len < 3 || wake_len < 3 || word_len > wake_len || wake_len - word_len > 1 {
                continue;
            }
            if jaro_winkler(word_form, wake_form) >= WAKE_FUZZY_THRESHOLD {
                return true;
            }
        }
    }
    false
}

fn normalize_number_words(words: &[&str]) -> String {
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let Some((value, kind)) = number_word(words[i]) else {
            out.push(words[i].to_string());
            i += 1;
            continue;
        };

        // Составное числительное начинается с десятка/сотни/тысячи либо с единицы,
        // за которой сразу следует разряд. Обычную последовательность «два три»
        // оставляем двумя числами.
        let has_following_scale = words
            .get(i + 1)
            .and_then(|word| number_word(word))
            .is_some_and(|(_, next_kind)| next_kind == NumberKind::Scale);
        if kind == NumberKind::Scale || has_following_scale || value >= 20 {
            let (combined, next) = read_compound_number(words, i);
            out.push(combined.to_string());
            i = next;
        } else {
            out.push(value.to_string());
            i += 1;
        }
    }
    out.join(" ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberKind {
    Unit,
    Scale,
}

fn number_word(word: &str) -> Option<(u32, NumberKind)> {
    let value = match word {
        "ноль" | "нуль" | "zero" => 0,
        "один" | "одна" | "одно" | "одну" | "одного" | "одной" | "одному" | "одним" | "one" => {
            1
        }
        "два" | "две" | "двух" | "двум" | "двумя" | "two" => 2,
        "три" | "трех" | "трем" | "тремя" | "three" => 3,
        "четыре" | "четырех" | "четырем" | "четырьмя" | "four" => 4,
        "пять" | "пяти" | "пятью" | "five" => 5,
        "шесть" | "шести" | "шестью" | "six" => 6,
        "семь" | "семи" | "семью" | "seven" => 7,
        "восемь" | "восьми" | "восемью" | "eight" => 8,
        "девять" | "девяти" | "девятью" | "nine" => 9,
        "десять" | "ten" => 10,
        "одиннадцать" | "eleven" => 11,
        "двенадцать" | "twelve" => 12,
        "тринадцать" | "thirteen" => 13,
        "четырнадцать" | "fourteen" => 14,
        "пятнадцать" | "fifteen" => 15,
        "шестнадцать" | "sixteen" => 16,
        "семнадцать" | "seventeen" => 17,
        "восемнадцать" | "eighteen" => 18,
        "девятнадцать" | "nineteen" => 19,
        "двадцать" | "twenty" => 20,
        "тридцать" | "thirty" => 30,
        "сорок" | "forty" => 40,
        "пятьдесят" | "fifty" => 50,
        "шестьдесят" | "sixty" => 60,
        "семьдесят" | "seventy" => 70,
        "восемьдесят" | "eighty" => 80,
        "девяносто" | "ninety" => 90,
        "сто" | "hundred" => 100,
        "двести" => 200,
        "триста" => 300,
        "четыреста" => 400,
        "пятьсот" => 500,
        "шестьсот" => 600,
        "семьсот" => 700,
        "восемьсот" => 800,
        "девятьсот" => 900,
        "тысяча" | "тысячи" | "тысяч" | "thousand" => 1000,
        _ => return None,
    };
    let kind = if value >= 20 {
        NumberKind::Scale
    } else {
        NumberKind::Unit
    };
    Some((value, kind))
}

fn read_compound_number(words: &[&str], start: usize) -> (u32, usize) {
    let mut total = 0u32;
    let mut current = 0u32;
    let mut i = start;
    let mut consumed = false;

    while let Some((value, kind)) = words.get(i).and_then(|word| number_word(word)) {
        if i > start && kind == NumberKind::Unit && value < 10 {
            let previous_is_scale = words
                .get(i - 1)
                .and_then(|word| number_word(word))
                .is_some_and(|(previous, _)| previous >= 20);
            if !previous_is_scale {
                break;
            }
        }
        if value == 1000 {
            total += current.max(1) * value;
            current = 0;
        } else if value >= 100 {
            current = current.max(1) * value;
        } else {
            current += value;
        }
        consumed = true;
        i += 1;
    }
    (total + current, if consumed { i } else { start + 1 })
}

fn stem_russian_word(word: &str) -> String {
    if matches!(word, "стопни" | "стопните") {
        return "стоп".into();
    }
    const SUFFIXES: [&str; 25] = [
        "ировать",
        "оваться",
        "ироваться",
        "ениями",
        "аниями",
        "ениями",
        "иями",
        "ями",
        "ами",
        "ией",
        "иями",
        "остью",
        "остью",
        "овать",
        "ывать",
        "ивать",
        "иться",
        "иться",
        "ить",
        "ать",
        "ять",
        "еть",
        "уть",
        "ись",
        "яй",
    ];
    for suffix in SUFFIXES {
        if word.len() > suffix.len() + 2 && word.ends_with(suffix) {
            return word[..word.len() - suffix.len()].to_string();
        }
    }
    for suffix in [
        "ейте",
        "айте",
        "яйте",
        "уйте",
        "ите",
        "ете",
        "ает",
        "яет",
        "ает",
        "яла",
        "яли",
        "ала",
        "али",
        "ешь",
        "ешься",
        "ьте",
        "нись",
        "ами",
        "ями",
        "ах",
        "ях",
        "ов",
        "ев",
        "ом",
        "ем",
        "ой",
        "ый",
        "ая",
        "яя",
        "ое",
        "ее",
        "ы",
        "и",
        "а",
        "я",
        "у",
        "ю",
        "е",
        "о",
        "ь",
        "й",
    ] {
        if word.len() > suffix.len() + 2 && word.ends_with(suffix) {
            return word[..word.len() - suffix.len()].to_string();
        }
    }
    word.to_string()
}

fn latin_to_cyrillic(text: &str) -> String {
    text.split_whitespace()
        .map(transliterate_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn transliterate_word(word: &str) -> String {
    match word {
        "youtube" => return "ютуб".into(),
        "tiktok" => return "тик ток".into(),
        _ => {}
    }

    let chars: Vec<char> = word.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_alphabetic() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let remaining: String = chars[i..].iter().collect();
        if let Some((sequence, value)) = [
            ("shch", "щ"),
            ("sch", "щ"),
            ("you", "ю"),
            ("yo", "е"),
            ("yu", "ю"),
            ("ya", "я"),
            ("zh", "ж"),
            ("kh", "х"),
            ("ch", "ч"),
            ("sh", "ш"),
            ("ts", "ц"),
            ("ph", "ф"),
            ("th", "т"),
            ("ck", "к"),
            ("oo", "у"),
            ("ee", "и"),
            ("ea", "и"),
            ("ou", "у"),
        ]
        .iter()
        .find(|(sequence, _)| remaining.starts_with(sequence))
        {
            out.push_str(value);
            i += sequence.chars().count();
            continue;
        }

        let next = chars.get(i + 1).copied().unwrap_or('\0');
        let mapped = match chars[i] {
            'a' => "а",
            'b' => "б",
            'c' if matches!(next, 'e' | 'i' | 'y') => "с",
            'c' => "к",
            'd' => "д",
            'e' => {
                if i + 1 == chars.len() {
                    ""
                } else {
                    "е"
                }
            }
            'f' => "ф",
            'g' => "г",
            'h' => "х",
            'i' => "и",
            'j' => "дж",
            'k' => "к",
            'l' => "л",
            'm' => "м",
            'n' => "н",
            'o' => "о",
            'p' => "п",
            'q' => "к",
            'r' => "р",
            's' => "с",
            't' => "т",
            'u' => "у",
            'v' => "в",
            'w' => "в",
            'x' => "кс",
            'y' => "й",
            'z' => "з",
            other => {
                out.push(other);
                i += 1;
                continue;
            }
        };
        out.push_str(mapped);
        i += 1;
    }
    out
}

fn canonicalize_letter_names(text: &str) -> String {
    fn letter(word: &str) -> Option<&'static str> {
        match word {
            "эф" | "ф" => Some("ф"),
            "эл" | "л" => Some("л"),
            "эйч" | "х" => Some("х"),
            "эй" => Some("а"),
            "би" => Some("б"),
            "си" => Some("с"),
            "ди" => Some("д"),
            "джи" => Some("г"),
            "ай" => Some("и"),
            "джей" => Some("дж"),
            "кей" | "кью" | "ку" => Some("к"),
            "эм" => Some("м"),
            "эн" => Some("н"),
            "пи" => Some("п"),
            "ар" => Some("р"),
            "эс" => Some("с"),
            "ти" => Some("т"),
            "ю" => Some("ю"),
            "ви" => Some("в"),
            "экс" => Some("кс"),
            "уай" => Some("й"),
            "зед" => Some("з"),
            _ => None,
        }
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if let Some(first) = letter(words[i]) {
            let mut run = String::new();
            let mut end = i;
            while end < words.len() {
                let Some(ch) = letter(words[end]) else { break };
                run.push_str(ch);
                end += 1;
            }
            if end - i >= 2 {
                out.push(run);
                i = end;
                continue;
            }
            out.push(first.to_string());
            i += 1;
        } else {
            out.push(words[i].to_string());
            i += 1;
        }
    }
    out.join(" ")
}

fn push_unique(forms: &mut Vec<String>, value: String) {
    if !value.is_empty() && !forms.contains(&value) {
        forms.push(value);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn glued_and_prefixed_wake_is_recognized() {
        let wake = vec!["ривер".to_string(), "ревер".to_string()];
        assert_eq!(super::strip_wake("риверпли", &wake), Some("пли".into()));
        assert_eq!(super::strip_wake("ривероткрой загрузки", &wake), Some("открой загрузки".into()));
        assert_eq!(super::strip_wake("тривер", &wake), Some(String::new()));
        assert_eq!(super::strip_wake("тривер открой загрузки", &wake), Some("открой загрузки".into()));
        // не wake: одна буква хвоста, чужой вариант, длинная приставка
        assert_eq!(super::strip_wake("ривера нет", &wake), None);
        assert_eq!(super::strip_wake("реверсивный ход", &wake), None);
        assert_eq!(super::strip_wake("сквививер", &wake), None);
    }

    use super::{is_cancel, is_yes, normalize, strip_wake};

    fn wakes() -> Vec<String> {
        vec![
            "ривер".into(),
            "river".into(),
            "ревер".into(),
            "ривэр".into(),
            "ривир".into(),
            "ривьер".into(),
        ]
    }

    #[test]
    fn normalize_canonicalizes_punctuation_numbers_and_casing() {
        assert_eq!(normalize("Ёж, Дота два!"), "еж дота 2");
        assert_eq!(normalize("ДОТА 2"), "дота 2");
        assert_eq!(normalize("двадцать две"), "22");
        assert_eq!(normalize("Dota two"), "dota 2");
        assert_eq!(normalize("два три"), "2 3");
    }

    #[test]
    fn strip_wake_accepts_separate_and_joined_styles() {
        assert_eq!(strip_wake("ривер", &wakes()), Some(String::new()));
        assert_eq!(
            strip_wake("ривер открой загрузки", &wakes()),
            Some("открой загрузки".into())
        );
        assert_eq!(
            strip_wake("открой ривер загрузки", &wakes()),
            Some("загрузки".into())
        );
        assert_eq!(
            strip_wake("пожалуйста ривер открой загрузки", &wakes()),
            Some("открой загрузки".into())
        );
    }

    #[test]
    fn strip_wake_handles_common_asr_misspellings_and_latin() {
        for wake in ["ривер", "ревер", "river", "ривэр", "ривир", "риве"] {
            assert_eq!(
                strip_wake(&format!("{wake} открой браузер"), &wakes()),
                Some("открой браузер".into()),
                "{wake}"
            );
        }
    }

    #[test]
    fn strip_wake_ignores_similar_words_and_only_checks_first_two_words() {
        for phrase in [
            "реверс открой загрузки",
            "ривьера открой загрузки",
            "ревёт открой загрузки",
        ] {
            assert_eq!(strip_wake(phrase, &wakes()), None, "{phrase}");
        }
        assert_eq!(strip_wake("пожалуйста, ну river фотошоп", &wakes()), None);
        assert_eq!(strip_wake("музыка потом ривер открой", &wakes()), None);
    }

    #[test]
    fn cancel_and_yes_accept_inflected_forms() {
        assert_eq!(super::stem_russian_word("отмени"), "отмен");
        assert_eq!(super::stem_russian_word("отмена"), "отмен");
        let cancels = vec![
            "отмена".into(),
            "стоп".into(),
            "отбой".into(),
            "забудь".into(),
        ];
        assert!(is_cancel("отмени, пожалуйста", &cancels));
        assert!(is_cancel("остановись", &["остановить".into()]));
        assert!(is_cancel("стопни музыку", &cancels));
        assert!(is_cancel("забудьте", &cancels));
        assert!(!is_cancel("стопка", &cancels));

        assert!(is_yes("да"));
        assert!(is_yes("давайте"));
        assert!(is_yes("подтвердить"));
        assert!(is_yes("согласна"));
        assert!(is_yes("угу"));
        assert!(!is_yes("далеко"));
    }
}
