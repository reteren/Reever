//! Нормализация распознанного текста и wake/отмена/согласие. Владелец: воркер `text`.

use strsim::jaro_winkler;

const WAKE_FUZZY_THRESHOLD: f64 = 0.86;

/// Общая форма распознанного текста: нижний регистр, `ё`→`е`, пунктуация → пробел,
/// числа словами → числа цифрами, одиночные пробелы и `trim`. Иероглифы и кана — каждый
/// знак отдельным словом: китайский и японский пишут без пробелов, а фразы команд и
/// действий сравниваются по словам («打开 记事本» = «打 开 记 事 本»).
pub fn normalize(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for c in text.to_lowercase().replace('ё', "е").chars() {
        if is_cjk(c) {
            cleaned.push(' ');
            cleaned.push(c);
            cleaned.push(' ');
        } else if c.is_alphanumeric() {
            cleaned.push(c);
        } else {
            cleaned.push(' ');
        }
    }
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    normalize_number_words(&words)
}

/// Иероглиф или кана (корейский хангыль пишется с пробелами между словами — не сюда).
pub fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
    )
}

/// Обратно к письму без пробелов: «记 事 本» → «记事本», «打开 notepad» остаётся
/// с пробелом перед латиницей. Для запроса в поиск, показа цели и поиска в тексте слов
/// вроде «はい» или «不是», которые `normalize` разбил на знаки.
pub fn join_cjk(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|(index, c)| {
            !(c.is_whitespace()
                && chars[..*index].iter().rev().find(|p| !p.is_whitespace()).is_some_and(|p| is_cjk(*p))
                && chars[index + 1..].iter().find(|n| !n.is_whitespace()).is_some_and(|n| is_cjk(*n)))
        })
        .map(|(_, c)| *c)
        .collect()
}

/// Ищет wake-слово только среди первых двух слов и возвращает нормализованный хвост.
/// Fuzzy-порог пропускает типичные ошибки ASR, но требует похожую длину слова, чтобы
/// не принимать самостоятельные слова вроде «реверс» или «ривьера».
pub fn strip_wake(text: &str, wake_words: &[String]) -> Option<String> {
    strip_wake_within(text, wake_words, 2)
}

/// Стоит ли wake-слово в самом начале фразы (в том числе со сдвинутой границей:
/// «риверс потифай»). Нужно Engine, чтобы отличать «ривер X» от «открой ривер X».
pub fn starts_with_wake(text: &str, wake_words: &[String]) -> bool {
    strip_wake_within(text, wake_words, 1).is_some()
}

fn strip_wake_within(text: &str, wake_words: &[String], positions: usize) -> Option<String> {
    let normalized = normalize(text);
    // «リバー» normalize разбил на знаки — сравниваем начало фразы целиком.
    for wake in wake_words.iter().filter(|wake| wake.chars().any(is_cjk)) {
        let wake = normalize(wake);
        if let Some(rest) = normalized.strip_prefix(&wake) {
            if rest.is_empty() || rest.starts_with(' ') {
                return Some(rest.trim().to_string());
            }
        }
    }
    let words: Vec<&str> = normalized.split_whitespace().collect();
    for (i, word) in words.iter().take(positions).enumerate() {
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
            // Сдвинутая граница слова: «риверс потифай» = «ривер спотифай». Одна лишняя
            // **согласная** переносится в следующее слово, только если оно есть: гласная
            // на конце — падежное окончание («ривера нет», «риверу»), это не wake.
            if let Some(next) = words.get(i + 1) {
                let canonical = normalize(wake);
                if let Some(tail) = word.strip_prefix(canonical.as_str()) {
                    let consonant = tail.chars().count() == 1
                        && tail.chars().all(|c| "бвгджзйклмнпрстфхцчшщ".contains(c));
                    if canonical.chars().count() >= 4 && consonant {
                        let mut rest = vec![format!("{tail}{next}")];
                        rest.extend(words[i + 2..].iter().map(|w| w.to_string()));
                        return Some(rest.join(" "));
                    }
                }
            }
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
    let normalized = normalize(text);
    let words: Vec<String> = normalized
        .split_whitespace()
        .map(stem_russian_word)
        .collect();
    let compact = join_cjk(&normalized);
    cancel_words.iter().any(|cancel| {
        // «取消», «キャンセル»: normalize разбил их на знаки, а отдельный «消» в «打开消息»
        // отменой не считается — нужно всё слово целиком.
        if cancel.chars().any(is_cjk) {
            return compact.contains(&join_cjk(&normalize(cancel)));
        }
        normalize(cancel)
            .split_whitespace()
            .map(stem_russian_word)
            .any(|stem| words.iter().any(|word| word == &stem))
    })
}

/// Согласие на подтверждение опасной команды («да», «ага», …), включая словоформы.
pub fn is_yes(text: &str) -> bool {
    let normalized = normalize(text);
    let compact = join_cjk(&normalized);
    // «不是», «不好», «唔係» — отказ, хотя в них есть «是», «好», «係».
    if compact.contains('不') || compact.contains('没') || compact.contains('唔') {
        return false;
    }
    const CJK_YES: [&str; 9] = ["是", "好", "对", "係", "啱", "對", "はい", "ええ", "うん"];
    CJK_YES.iter().any(|yes| compact.split_whitespace().any(|word| word.contains(yes)))
        || normalized.split_whitespace().any(|word| {
            matches!(
                word,
                "да" | "ага" | "угу" | "точно" | "давай" | "давайте" | "подтверждаю" | "подтверди"
                    | "yes" | "yeah" | "yep" | "yup" | "sure" | "ok" | "okay"
                    | "йес" | "ес" | "есс" | "еес" | "яс" | "йеа" | "окей" | "ок"
                    // Многоязычная модель на коротком «да» путает язык: «da»; остальное —
                    // «да» языков из языковых пакетов (models.rs).
                    | "da" | "ja" | "jawohl" | "oui" | "si" | "sí" | "sì" | "sim" | "claro"
                    | "certo" | "d'accord"
                    | "так" | "tak" | "ano" | "áno" | "jo" | "joo" | "kyllä" | "jah" | "jaa" | "jā"
                    | "taip" | "ναι" | "igen" | "iva" | "네" | "예" | "응" | "그래"
            ) || word.starts_with("дава")
                || word.starts_with("подтвержд")
                || word.starts_with("подтверд")
                || word.starts_with("соглас")
                || word.starts_with("конеч")
        })
}

/// Явный отказ на подтверждение («нет», «отмена», «no»).
pub fn is_no(text: &str) -> bool {
    let normalized = normalize(text);
    let compact = join_cjk(&normalized);
    const CJK_NO: [&str; 6] = ["不", "没", "唔", "取消", "いいえ", "いや"];
    CJK_NO.iter().any(|no| compact.split_whitespace().any(|word| word.contains(no)))
        || normalized.split_whitespace().any(|word| {
            matches!(
                word,
                "нет" | "не" | "неа" | "отмена" | "отмени" | "отменить" | "стоп" | "не надо" | "no"
                    | "nope" | "cancel" | "stop" | "ноу" | "ноуп"
                    // «нет» других языков; «não» многоязычная модель пишет и на русское «no».
                    | "net" | "nein" | "non" | "não" | "nao" | "annuler" | "abbrechen"
                    | "cancelar" | "annulla"
                    | "ні" | "nie" | "ne" | "nee" | "nej" | "ei" | "nē" | "όχι" | "nem" | "nu" | "le"
                    | "아니" | "아니요" | "아니오"
            ) || word.starts_with("отмен")
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

/// Числа словами → цифры в тексте «как сказано» (диктовка): «восемьдесят восемь» → «88».
/// Остальные слова, регистр, «ё» и пунктуация не трогаются. Только кириллица — английский
/// текст модель пишет сама, а «one of them» цифрой стать не должно. Одиночные формы
/// единицы («один раз», «одна») остаются словами.
pub fn digits_in_text(raw: &str) -> String {
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let keys: Vec<String> = tokens
        .iter()
        .map(|token| {
            token
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
                .replace('ё', "е")
        })
        .collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let cyrillic = |key: &str| key.chars().next().is_some_and(|c| ('а'..='я').contains(&c));

    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let Some((value, kind)) = number_word(key_refs[i]).filter(|_| cyrillic(key_refs[i])) else {
            out.push(tokens[i].to_string());
            i += 1;
            continue;
        };
        let has_following_scale = key_refs
            .get(i + 1)
            .and_then(|word| number_word(word))
            .is_some_and(|(_, next_kind)| next_kind == NumberKind::Scale);
        let (number, next) = if kind == NumberKind::Scale || has_following_scale || value >= 20 {
            read_compound_number(&key_refs, i)
        } else {
            (value, i + 1)
        };
        // «двадцать шестом» — порядковое: целиком словами, иначе вышло бы «20 шестом».
        let ordinal_follows = key_refs.get(next).is_some_and(|word| is_ordinal_word(word));
        if (number == 1 && next == i + 1) || ordinal_follows {
            out.extend(tokens[i..next].iter().map(|token| token.to_string()));
            i = next;
            continue;
        }
        // Пунктуация после последнего слова числа («двадцать,») остаётся на месте.
        let last = tokens[next - 1];
        let tail: String = last
            .chars()
            .rev()
            .take_while(|c| !c.is_alphanumeric())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        out.push(format!("{number}{tail}"));
        i = next;
    }
    out.join(" ")
}

/// Порядковое числительное: «первый», «шестом», «двадцатого», «сотый».
fn is_ordinal_word(word: &str) -> bool {
    const ROOTS: &[&str] = &[
        "перв", "втор", "трет", "четверт", "пят", "шест", "седьм", "восьм", "девят", "десят",
        "двадцат", "тридцат", "сороков", "сот", "тысячн",
    ];
    const ENDINGS: &[&str] = &[
        "ый", "ой", "ий", "ая", "ое", "ье", "ые", "ого", "его", "ому", "ему", "ом", "ем", "ую",
        "ых", "их", "ым", "им", "ыми", "ими", "ья", "ьи", "ьего", "ьему", "ьим", "ьем", "ью",
    ];
    ROOTS.iter().any(|root| {
        word.strip_prefix(root)
            .is_some_and(|rest| ENDINGS.contains(&rest))
    })
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
        "десять" | "десяти" | "десятью" | "ten" => 10,
        "одиннадцать" | "одиннадцати" | "eleven" => 11,
        "двенадцать" | "двенадцати" | "twelve" => 12,
        "тринадцать" | "тринадцати" | "thirteen" => 13,
        "четырнадцать" | "четырнадцати" | "fourteen" => 14,
        "пятнадцать" | "пятнадцати" | "fifteen" => 15,
        "шестнадцать" | "шестнадцати" | "sixteen" => 16,
        "семнадцать" | "семнадцати" | "seventeen" => 17,
        "восемнадцать" | "восемнадцати" | "eighteen" => 18,
        "девятнадцать" | "девятнадцати" | "nineteen" => 19,
        "двадцать" | "двадцати" | "twenty" => 20,
        "тридцать" | "тридцати" | "thirty" => 30,
        "сорок" | "сорока" | "forty" => 40,
        "пятьдесят" | "пятидесяти" | "fifty" => 50,
        "шестьдесят" | "шестидесяти" | "sixty" => 60,
        "семьдесят" | "семидесяти" | "seventy" => 70,
        "восемьдесят" | "восьмидесяти" | "eighty" => 80,
        "девяносто" | "девяноста" | "ninety" => 90,
        "сто" | "ста" | "hundred" => 100,
        "двести" | "двухсот" | "двумстам" => 200,
        "триста" | "трехсот" | "тремстам" => 300,
        "четыреста" | "четырехсот" | "четыремстам" => 400,
        "пятьсот" | "пятисот" | "пятистам" => 500,
        "шестьсот" | "шестисот" | "шестистам" => 600,
        "семьсот" | "семисот" | "семистам" => 700,
        "восемьсот" | "восьмисот" | "восьмистам" => 800,
        "девятьсот" | "девятисот" | "девятистам" => 900,
        "тысяча" | "тысячи" | "тысяч" | "тысячу" | "тысяче" | "тысячей" | "thousand" => 1000,
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

/// Произношение английских слов из фраз команд (grammar.rs, версия 9).
const ENGLISH_COMMAND_WORDS: &[(&str, &str)] = &[
    ("open", "оупен"),
    ("close", "клоуз"),
    ("quit", "квит"),
    ("launch", "лонч"),
    ("run", "ран"),
    ("folder", "фолдер"),
    ("search", "серч"),
    ("google", "гугл"),
    ("type", "тайп"),
    ("write", "райт"),
    ("dictate", "диктейт"),
    ("calculate", "калькулейт"),
    ("kill", "кил"),
    ("task", "таск"),
    ("find", "файнд"),
    ("file", "файл"),
    ("cancel", "кэнсел"),
    ("stop", "стоп"),
    ("typing", "тайпинг"),
    ("next", "некст"),
    ("previous", "привиус"),
    ("track", "трек"),
    ("music", "мьюзик"),
    ("computer", "компьютер"),
    ("shut", "шат"),
    ("down", "даун"),
    ("turn", "тёрн"),
    ("off", "офф"),
    ("restart", "рестарт"),
    ("reboot", "ребут"),
    ("mute", "мьют"),
    ("sound", "саунд"),
    ("pause", "поуз"),
    ("play", "плей"),
];

pub(crate) fn transliterate_word(word: &str) -> String {
    match word {
        "youtube" => return "ютуб".into(),
        "tiktok" => return "тик ток".into(),
        _ => {}
    }
    // Английские слова команд — как их произносят и как их пишет русская модель:
    // побуквенно «close» дал бы «клосе», а сказанное «клоуз» ушло бы к «клауд».
    if let Some(spoken) = ENGLISH_COMMAND_WORDS
        .iter()
        .find(|(english, _)| *english == word)
        .map(|(_, spoken)| *spoken)
    {
        return spoken.into();
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
    fn oblique_case_numbers_become_digits() {
        // «пятнадцать процентов от двухсот», «разделить на пятьдесят» и т.п. (калькулятор)
        assert_eq!(super::normalize("пятнадцать процентов от двухсот"), "15 процентов от 200");
        assert_eq!(super::normalize("от трехсот сорока пяти"), "от 345");
        assert_eq!(super::normalize("из тысячи двухсот"), "из 1200");
        assert_eq!(super::normalize("двадцати пяти"), "25");
    }

    #[test]
    fn shifted_word_boundary_moves_letter_to_next_word() {
        let wake = vec!["ривер".to_string(), "ревер".to_string()];
        assert_eq!(super::strip_wake("риверс потифай", &wake), Some("спотифай".into()));
        assert_eq!(super::strip_wake("риверс потифай пожалуйста", &wake), Some("спотифай пожалуйста".into()));
        // без следующего слова и у вариантов — не wake
        assert_eq!(super::strip_wake("риверы", &wake), None);
        assert_eq!(super::strip_wake("реверс гитары", &wake), None);
    }

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
        assert!(is_cancel("取消", &["取消".into()]));
        assert!(!is_cancel("打开消息", &["取消".into()]));

        assert!(is_yes("да"));
        assert!(is_yes("давайте"));
        assert!(is_yes("подтвердить"));
        assert!(is_yes("согласна"));
        assert!(is_yes("угу"));
        assert!(!is_yes("далеко"));

        // Ответы на других языках и то, как их путает многоязычная модель.
        assert!(is_yes("Da"));
        assert!(is_yes("Ja."));
        assert!(is_yes("Sí"));
        assert!(is_yes("是的"));
        assert!(is_yes("はい"));
        assert!(!is_yes("不是"));
        assert!(super::is_no("não"));
        assert!(super::is_no("Nein"));
        assert!(super::is_no("不要"));
        assert!(super::is_no("いいえ"));
        assert!(is_yes("Tak"));
        assert!(is_yes("Ναι"));
        assert!(is_yes("네"));
        assert!(!is_yes("唔係"));
        assert!(super::is_no("Nie"));
        assert!(super::is_no("아니요"));
    }

    #[test]
    fn chinese_and_japanese_split_into_characters_and_join_back() {
        assert_eq!(normalize("打开记事本。"), "打 开 记 事 本");
        assert_eq!(normalize("打开Notepad"), "打 开 notepad");
        assert_eq!(normalize("メモ帳を開いて"), "メ モ 帳 を 開 い て");
        assert_eq!(super::join_cjk("打 开 记 事 本"), "打开记事本");
        assert_eq!(super::join_cjk("搜索 猫 的 视 频 youtube"), "搜索猫的视频 youtube");
        assert_eq!(super::join_cjk("open notepad"), "open notepad");
        let wake = ["ривер".to_string(), "リバー".to_string()];
        assert_eq!(strip_wake("リバー、メモ帳を開いて", &wake), Some("メ モ 帳 を 開 い て".into()));
        assert_eq!(strip_wake("リンゴを買って", &wake), None);
    }

    #[test]
    fn dictation_numbers_become_digits_but_text_stays_as_spoken() {
        assert_eq!(
            super::digits_in_text("Едят сосиски восемьдесят восемь штук"),
            "Едят сосиски 88 штук"
        );
        assert_eq!(
            super::digits_in_text("в две тысячи двадцать шестом году,"),
            "в две тысячи двадцать шестом году,"
        );
        assert_eq!(super::digits_in_text("в две тысячи двадцать шестой"), "в две тысячи двадцать шестой");
        assert_eq!(super::digits_in_text("один раз и ещё двадцать,"), "один раз и ещё 20,");
        assert_eq!(super::digits_in_text("one of them"), "one of them");
        assert_eq!(super::digits_in_text("пять минут"), "5 минут");
    }
}
