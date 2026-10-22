//! Диктовка без побочных эффектов: оформление распознанного текста (заглавные буквы,
//! произнесённая пунктуация, перевод строки) и поиск фразы остановки в конце куска.
//! Ввод в окно и буфер обмена — `typing.rs`; когда диктовка идёт — `engine.rs`.

use strsim::normalized_levenshtein;

use crate::speech::{Lang, TimedWord};
use crate::textnorm::{normalize, starts_with_wake, strip_wake};

/// Кусок оформленного текста для ввода.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    /// Перевод строки: печатается Shift+Enter, чтобы в чатах не отправить сообщение.
    NewLine,
}

/// Собрать куски в обычный текст (для буфера обмена и лога).
pub fn pieces_to_string(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Text(text) => text.as_str(),
            Piece::NewLine => "\r\n",
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spoken {
    Mark(&'static str),
    Dash,
    NewLine,
}

/// Произнесённые знаки: длинные фразы раньше коротких («точка с запятой» до «точка»).
const SPOKEN: &[(&[&str], Spoken)] = &[
    (&["точка", "с", "запятой"], Spoken::Mark(";")),
    (&["вопросительный", "знак"], Spoken::Mark("?")),
    (&["восклицательный", "знак"], Spoken::Mark("!")),
    (&["с", "новой", "строки"], Spoken::NewLine),
    (&["новая", "строка"], Spoken::NewLine),
    (&["новый", "абзац"], Spoken::NewLine),
    (&["question", "mark"], Spoken::Mark("?")),
    (&["exclamation", "mark"], Spoken::Mark("!")),
    (&["full", "stop"], Spoken::Mark(".")),
    (&["new", "line"], Spoken::NewLine),
    (&["new", "paragraph"], Spoken::NewLine),
    (&["многоточие"], Spoken::Mark("…")),
    (&["точка"], Spoken::Mark(".")),
    (&["запятая"], Spoken::Mark(",")),
    (&["двоеточие"], Spoken::Mark(":")),
    (&["тире"], Spoken::Dash),
    (&["comma"], Spoken::Mark(",")),
    (&["period"], Spoken::Mark(".")),
];

/// Оформитель одной диктовки: помнит, начало ли предложения и нужен ли пробел, так что
/// куски, распознанные по паузам, склеиваются в связный текст.
#[derive(Clone, Debug)]
pub struct Formatter {
    sentence_start: bool,
    need_space: bool,
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new()
    }
}

impl Formatter {
    pub fn new() -> Self {
        Self {
            sentence_start: true,
            need_space: false,
        }
    }

    /// Оформить очередной распознанный кусок.
    pub fn format(&mut self, raw: &str) -> Vec<Piece> {
        let raw = crate::textnorm::digits_in_text(raw);
        let hyphenated = hyphenate_particles(&raw);
        let tokens: Vec<&str> = hyphenated.iter().map(String::as_str).collect();
        let keys: Vec<String> = tokens.iter().map(|token| spoken_key(token)).collect();
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut i = 0;
        let mut spoken_last = false;
        while i < tokens.len() {
            if let Some((len, spoken)) = match_spoken(&keys[i..]) {
                spoken_last = true;
                match spoken {
                    Spoken::Mark(mark) => {
                        // «запятая» первым словом куска — знак к концу прошлого куска.
                        // Модель с пунктуацией могла уже поставить этот знак («review?» +
                        // «question mark») — второй не нужен.
                        if !text.ends_with(mark) {
                            text.push_str(mark);
                        }
                        self.need_space = true;
                        if matches!(mark, "." | "!" | "?" | "…") {
                            self.sentence_start = true;
                        }
                    }
                    Spoken::Dash => {
                        self.push_word(&mut text, "—", false);
                    }
                    Spoken::NewLine => {
                        if !text.is_empty() {
                            pieces.push(Piece::Text(std::mem::take(&mut text)));
                        }
                        pieces.push(Piece::NewLine);
                        self.need_space = false;
                        self.sentence_start = true;
                    }
                }
                i += len;
                continue;
            }
            self.push_word(&mut text, tokens[i], true);
            spoken_last = false;
            i += 1;
        }
        // Точку в конце куска ставит модель с пунктуацией (английская), а не человек:
        // кусок кончается паузой, а не обязательно предложением. Сказанную «точку» оставляем.
        if !spoken_last && text.ends_with('.') && !text.ends_with("..") {
            text.pop();
            self.sentence_start = false;
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        pieces
    }

    fn push_word(&mut self, text: &mut String, word: &str, capitalize: bool) {
        if self.need_space {
            text.push(' ');
        }
        if capitalize && self.sentence_start {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                text.extend(first.to_uppercase());
                text.push_str(chars.as_str());
            }
            self.sentence_start = false;
        } else {
            text.push_str(word);
        }
        self.need_space = true;
        // Модели с пунктуацией (английская) сами ставят точку в конце предложения.
        if word.ends_with(['.', '!', '?', '…']) {
            self.sentence_start = true;
        }
    }
}

/// Местоимения, к которым пишутся через дефис частицы -то, -либо, -нибудь.
const PARTICLE_ROOTS: &[&str] = &[
    "кто",
    "что",
    "где",
    "куда",
    "откуда",
    "когда",
    "как",
    "какой",
    "какая",
    "какое",
    "какие",
    "какого",
    "какую",
    "каким",
    "чей",
    "чья",
    "чьё",
    "чье",
    "чьи",
    "почему",
    "зачем",
    "кого",
    "кому",
    "кем",
    "чего",
    "чему",
    "чем",
    "сколько",
];
const PARTICLES: &[&str] = &["то", "либо", "нибудь"];

/// ASR пишет «кто-то» слитно («ктото») или раздельно («кто то») — возвращаем дефис.
fn hyphenate_particles(raw: &str) -> Vec<String> {
    let words: Vec<&str> = raw.split_whitespace().collect();
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        let lower = word.to_lowercase();
        if PARTICLE_ROOTS.contains(&lower.as_str()) {
            if let Some(next) = words.get(i + 1) {
                if PARTICLES.contains(&next.to_lowercase().as_str()) {
                    out.push(format!("{word}-{next}"));
                    i += 2;
                    continue;
                }
            }
        }
        let glued = PARTICLES.iter().find_map(|particle| {
            let root = lower.strip_suffix(particle)?;
            PARTICLE_ROOTS
                .contains(&root)
                .then(|| format!("{}-{particle}", &word[..root.len()]))
        });
        out.push(glued.unwrap_or_else(|| word.to_string()));
        i += 1;
    }
    out
}

/// Слово для сравнения с произнесённым знаком: без пунктуации модели, в нижнем регистре.
fn spoken_key(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
        .replace('ё', "е")
}

fn match_spoken(keys: &[String]) -> Option<(usize, Spoken)> {
    SPOKEN.iter().find_map(|(words, spoken)| {
        (keys.len() >= words.len() && keys.iter().zip(words.iter()).all(|(key, word)| key == word))
            .then_some((words.len(), *spoken))
    })
}

/// Язык куска диктовки по двум распознаваниям одного звука: `ru` — русской моделью,
/// `en` — английской. На чужом языке модель выдаёт транслит или обрывки слов, поэтому
/// решает доля несловарных слов (проверка орфографии Windows); голос определителя языка
/// (`lid`) лишь сдвигает порог. Без словарей — только определитель, по умолчанию русский.
pub fn choose_language(ru: &str, en: &str, lid: Option<(Lang, f32)>) -> Lang {
    let ru_empty = ru.trim().is_empty();
    let en_empty = !en.chars().any(char::is_alphabetic);
    if en_empty {
        return Lang::Ru;
    }
    let en_score = crate::spellcheck::misspelled_ratio("en-US", en);
    if ru_empty {
        // Русская модель ничего не услышала: английский, только если это настоящие слова,
        // а не выдумка английской модели на шуме.
        return match en_score {
            Some((ratio, _)) if ratio <= 0.25 => Lang::En,
            Some(_) => Lang::Ru,
            None => match lid {
                Some((Lang::En, confidence)) if confidence >= 0.55 => Lang::En,
                _ => Lang::Ru,
            },
        };
    }
    let ru_score = crate::spellcheck::misspelled_ratio("ru-RU", ru);
    match (ru_score, en_score) {
        (Some((ru_ratio, ru_words)), Some((en_ratio, en_words))) => {
            // Сколько словарных слов услышала каждая модель в одном и том же звуке. На
            // английской речи русская модель выдаёт транслит или пару коротких слов
            // («у них»), на русской английская — обрывки с редкими настоящими словами.
            let mut ru_valid = ru_words as f32 * (1.0 - ru_ratio);
            let mut en_valid = en_words as f32 * (1.0 - en_ratio);
            match lid {
                Some((Lang::En, confidence)) if confidence >= 0.55 => en_valid *= 1.25,
                Some((Lang::Ru, confidence)) if confidence >= 0.55 => ru_valid *= 1.25,
                _ => {}
            }
            if en_ratio <= 0.35 && en_valid > ru_valid * 1.5 + 0.5 {
                Lang::En
            } else {
                Lang::Ru
            }
        }
        _ => match lid {
            Some((Lang::En, confidence)) if confidence >= 0.55 => Lang::En,
            _ => Lang::Ru,
        },
    }
}

/// Нижняя граница фонетического сходства для пословной подстановки.
/// Калибровка на примерах: «рафнес» / roughness ≈ 0,84, но «маск» / mosque ≈ 0,78.
/// Порог оставляет запас от ложной английской омонимии и сохраняет roughness.
pub const MIX_ENGLISH_MIN_SIMILARITY: f64 = 0.80;

/// Частые разговорные слова не считаем ASR-транслитом, даже если второй распознаватель
/// угадал созвучное английское словарное слово («вайб» / vibe). Это консервативный список:
/// неоднозначный токен безопаснее оставить по-русски, чем менять текст диктовки.
const RUSSIAN_SLANG_GUARD: &[&str] = &[
    "блин",
    "капец",
    "кек",
    "кринж",
    "имба",
    "вайб",
    "зашквар",
    "жесть",
    "пиздец",
    "пипец",
    "хрень",
    "треш",
    "трэш",
    "чилл",
    "чилить",
    "хайп",
    "лол",
    "рофл",
    "угар",
    "фигня",
    "фигово",
    "офигеть",
    "задолбал",
];

/// Подменить фонетически записанные английские слова в русском ASR-тексте словами Parakeet.
///
/// Подстановка выполняется только при точном выравнивании русских слов с их таймингами,
/// словарной проверке обоих языков и достаточном фонетическом сходстве. Тайминги английских
/// слов назначаются максимум одному русскому слову, поэтому широкий интервал не дублирует
/// одну английскую гипотезу по обе стороны границы. Если данных недостаточно, `ru_raw`
/// возвращается без изменений.
pub fn mix_english_words(
    ru_raw: &str,
    ru_words: &[TimedWord],
    en_words: &[TimedWord],
    is_word: &dyn Fn(&str, &str) -> Option<bool>,
) -> String {
    let spans = raw_word_spans(ru_raw);
    if spans.is_empty() || spans.len() != ru_words.len() || en_words.is_empty() {
        return ru_raw.to_string();
    }

    let ru_keys: Vec<String> = spans
        .iter()
        .map(|span| word_key(&ru_raw[span.start..span.end]))
        .collect();
    // Не применяем тайминги к чужой строке: точное пословное соответствие — важнее покрытия.
    if ru_keys
        .iter()
        .zip(ru_words)
        .any(|(key, timed)| key.is_empty() || *key != word_key(&timed.text))
    {
        return ru_raw.to_string();
    }

    let mut assigned: Vec<Vec<usize>> = vec![Vec::new(); ru_words.len()];
    for (en_index, english) in en_words.iter().enumerate() {
        let mut best: Option<(usize, f32)> = None;
        let mut ambiguous = false;
        for (ru_index, russian) in ru_words.iter().enumerate() {
            let overlap = time_overlap(russian, english);
            if overlap <= 0.0 {
                continue;
            }
            match best {
                None => {
                    best = Some((ru_index, overlap));
                    ambiguous = false;
                }
                Some((_, previous)) if overlap > previous + 0.01 => {
                    best = Some((ru_index, overlap));
                    ambiguous = false;
                }
                Some((_, previous)) if (overlap - previous).abs() <= 0.01 => ambiguous = true,
                _ => {}
            }
        }
        if !ambiguous {
            if let Some((ru_index, _)) = best {
                assigned[ru_index].push(en_index);
            }
        }
    }

    let mut replacements: Vec<Option<String>> = vec![None; ru_words.len()];
    for ru_index in 0..ru_words.len() {
        let ru_key = &ru_keys[ru_index];
        if RUSSIAN_SLANG_GUARD.contains(&ru_key.as_str())
            || is_word("ru-RU", ru_key) != Some(false)
            || assigned[ru_index].is_empty()
        {
            continue;
        }

        let mut indexes = assigned[ru_index].clone();
        indexes.sort_by(|a, b| {
            en_words[*a]
                .start
                .partial_cmp(&en_words[*b].start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut en_keys = Vec::with_capacity(indexes.len());
        let mut en_text = Vec::with_capacity(indexes.len());
        let mut all_english_words = true;
        for index in indexes {
            // Обычно TimedWord уже одно слово; split поддерживает нормализованные BPE-чанки.
            for part in en_words[index].text.split_whitespace() {
                let key = word_key(part);
                if key.is_empty() || is_word("en-US", &key) != Some(true) {
                    all_english_words = false;
                    break;
                }
                en_keys.push(english_sound_key(&key));
                en_text.push(part);
            }
            if !all_english_words {
                break;
            }
        }
        if !all_english_words || en_keys.is_empty() {
            continue;
        }

        let russian_sound = russian_sound_key(ru_key);
        let english_sound = en_keys.concat();
        if !russian_sound.is_empty()
            && strsim::jaro_winkler(&russian_sound, &english_sound) >= MIX_ENGLISH_MIN_SIMILARITY
        {
            replacements[ru_index] = Some(en_text.join(" "));
        }
    }

    if replacements.iter().all(Option::is_none) {
        return ru_raw.to_string();
    }
    let mut result = String::with_capacity(ru_raw.len());
    let mut cursor = 0;
    for (index, span) in spans.iter().enumerate() {
        result.push_str(&ru_raw[cursor..span.start]);
        if let Some(replacement) = &replacements[index] {
            let token = &ru_raw[span.start..span.end];
            let (prefix_end, suffix_start) = word_core_bounds(token);
            result.push_str(&token[..prefix_end]);
            result.push_str(replacement);
            result.push_str(&token[suffix_start..]);
        } else {
            result.push_str(&ru_raw[span.start..span.end]);
        }
        cursor = span.end;
    }
    result.push_str(&ru_raw[cursor..]);
    result
}

#[derive(Clone, Copy)]
struct WordSpan {
    start: usize,
    end: usize,
}

fn raw_word_spans(text: &str) -> Vec<WordSpan> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some(word_start) = start.take() {
                spans.push(WordSpan {
                    start: word_start,
                    end: index,
                });
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(word_start) = start {
        spans.push(WordSpan {
            start: word_start,
            end: text.len(),
        });
    }
    spans
}

fn word_key(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric() || *character == '\'')
        .flat_map(char::to_lowercase)
        .map(|character| if character == 'ё' { 'е' } else { character })
        .collect()
}

fn word_core_bounds(token: &str) -> (usize, usize) {
    let first = token.char_indices().find(|(_, c)| c.is_alphanumeric());
    let last = token
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric());
    match (first, last) {
        (Some((start, _)), Some((end, character))) => (start, end + character.len_utf8()),
        _ => (0, token.len()),
    }
}

fn time_overlap(left: &TimedWord, right: &TimedWord) -> f32 {
    if !left.start.is_finite()
        || !left.end.is_finite()
        || !right.start.is_finite()
        || !right.end.is_finite()
        || left.end <= left.start
        || right.end <= right.start
    {
        return 0.0;
    }
    (left.end.min(right.end) - left.start.max(right.start)).max(0.0)
}

pub(crate) fn russian_sound_key(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    while index < chars.len() {
        let pair: String = chars[index..(index + 2).min(chars.len())].iter().collect();
        if pair == "дж" {
            output.push('j');
            index += 2;
            continue;
        }
        if pair == "кс" {
            output.push('x');
            index += 2;
            continue;
        }
        let mapped = match chars[index] {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'э' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ы' => "i",
            'ю' => "yu",
            'я' => "ya",
            'ъ' | 'ь' => "",
            other if other.is_ascii_alphanumeric() => {
                output.push(other);
                index += 1;
                continue;
            }
            _ => "",
        };
        output.push_str(mapped);
        index += 1;
    }
    output
}

pub(crate) fn english_sound_key(word: &str) -> String {
    word.to_lowercase()
        .replace("ough", "uff")
        .replace("ph", "f")
        .replace("qu", "kw")
        .replace("ck", "k")
        .replace("ge", "j")
        .chars()
        .filter(char::is_ascii_alphabetic)
        .collect()
}

/// Цель поиска или вопроса в чат, услышанная обеими моделями: заменить ли выбранный
/// `bilingual` вариант (`chosen`) другим (`other`). Только при явном перевесе словарных слов
/// (втрое с запасом): без определителя языка английская модель на русской речи выдумывает
/// связные фразы («погода в москве» → «i think it's a good idea»), а словарь считает
/// ошибкой имена со строчной буквы («москве»). На английском с акцентом русская модель
/// даёт транслит («плиз ревью зе файнал»), словарных слов там почти нет — замена проходит.
pub fn payload_switch_language(
    chosen: &str,
    chosen_lang: &str,
    other: &str,
    other_lang: &str,
) -> bool {
    // Словарные слова — целым числом: дробная доля давала 1,9999998 вместо 2 на пороге.
    let valid = |text: &str, lang: &str| {
        crate::spellcheck::misspelled_ratio(lang, text).map(|(ratio, words)| {
            let bad = (ratio * words as f32).round() as usize;
            (ratio, words.saturating_sub(bad))
        })
    };
    let Some((other_ratio, other_valid)) = valid(other, other_lang) else {
        return false;
    };
    let chosen_valid = valid(chosen, chosen_lang).map(|(_, v)| v).unwrap_or(0);
    other_ratio <= 0.25 && other_valid >= 2 && other_valid > chosen_valid * 3 + 1
}

/// Фраза целиком — переключение языка диктовки («по-английски», «русский»).
pub fn language_switch(norm: &str) -> Option<Lang> {
    let phrase = norm.trim();
    const EN: &[&str] = &[
        "по английски",
        "на английском",
        "английский",
        "english",
        "инглиш",
        "in english",
    ];
    const RU: &[&str] = &[
        "по русски",
        "на русском",
        "русский",
        "russian",
        "рашн",
        "in russian",
    ];
    if EN.contains(&phrase) {
        Some(Lang::En)
    } else if RU.contains(&phrase) {
        Some(Lang::Ru)
    } else {
        None
    }
}

/// Переключение языка в начале цели «напиши по-английски …»: язык и число слов фразы.
pub fn leading_language_switch(norm: &str) -> Option<(Lang, usize)> {
    let words: Vec<&str> = norm.split_whitespace().collect();
    (1..=2.min(words.len()))
        .rev()
        .find_map(|take| language_switch(&words[..take].join(" ")).map(|lang| (lang, take)))
}

/// Сколько последних слов нормализованного куска `norm` — фраза остановки диктовки.
/// Фраза с wake-словом («ривер остановись») требует wake перед собой; фраза без него
/// («стоп ввод») принимается и с wake впереди. Сравнение нечёткое (ошибки ASR).
pub fn trailing_stop_words(
    norm: &str,
    stop_phrases: &[String],
    wake_words: &[String],
) -> Option<usize> {
    let words: Vec<&str> = norm.split_whitespace().collect();
    let mut best: Option<usize> = None;
    for stop in stop_phrases {
        let phrase = normalize(stop);
        if phrase.is_empty() {
            continue;
        }
        let wake_first = starts_with_wake(&phrase, wake_words);
        let body = if wake_first {
            strip_wake(&phrase, wake_words).unwrap_or_default()
        } else {
            phrase.clone()
        };
        let body_words: Vec<&str> = body.split_whitespace().collect();
        if body_words.is_empty() || words.len() < body_words.len() {
            continue;
        }
        let tail = &words[words.len() - body_words.len()..];
        if !similar(&tail.concat(), &body_words.concat()) {
            continue;
        }
        let mut count = body_words.len();
        let wake_before =
            words.len() > count && is_wake_word(words[words.len() - count - 1], wake_words);
        if wake_first && !wake_before {
            continue;
        }
        if wake_before {
            count += 1;
        }
        best = Some(best.map_or(count, |previous| previous.max(count)));
    }
    best
}

fn is_wake_word(word: &str, wake_words: &[String]) -> bool {
    strip_wake(word, wake_words).is_some_and(|rest| rest.trim().is_empty())
}

fn similar(heard: &str, expected: &str) -> bool {
    if heard == expected {
        return true;
    }
    // Короткие фразы — только точно: иначе «стоп» ловил бы «стол».
    expected.chars().count() >= 6 && normalized_levenshtein(heard, expected) >= 0.75
}

/// Первые `drop_front` и последние `drop_back` слов долой (слова wake/триггера/стоп-фразы
/// в тексте «как сказано»; в них нет чисел, поэтому счёт слов совпадает с нормализованным).
pub fn trim_words(raw: &str, drop_front: usize, drop_back: usize) -> String {
    let words: Vec<&str> = raw.split_whitespace().collect();
    if drop_front + drop_back >= words.len() {
        return String::new();
    }
    words[drop_front..words.len() - drop_back].join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(pieces: &[Piece]) -> String {
        pieces_to_string(pieces)
    }

    fn wake() -> Vec<String> {
        vec!["ривер".into(), "ревер".into()]
    }

    fn stops() -> Vec<String> {
        vec!["стоп ввод".into(), "ривер остановись".into()]
    }

    fn timed_word(text: &str, start: f32, end: f32) -> TimedWord {
        TimedWord {
            text: text.into(),
            start,
            end,
        }
    }

    fn mock_word(language: &str, word: &str) -> Option<bool> {
        match language {
            "ru-RU" => Some(matches!(
                word,
                "измерь" | "затем" | "слой" | "в" | "на" | "потом" | "это"
            )),
            "en-US" => Some(matches!(
                word,
                "measure"
                    | "hardness"
                    | "roughness"
                    | "layer"
                    | "hard"
                    | "ness"
                    | "pizza"
                    | "cringe"
                    | "vibe"
            )),
            _ => None,
        }
    }

    #[test]
    fn restores_phonetically_transcribed_english_terms() {
        let ru_words = [
            timed_word("измерь", 0.00, 0.25),
            timed_word("харднес", 0.28, 0.58),
            timed_word("рафнес", 0.62, 0.92),
        ];
        let en_words = [
            timed_word("measure", 0.02, 0.22),
            timed_word("hardness", 0.30, 0.56),
            timed_word("roughness", 0.64, 0.90),
        ];
        assert_eq!(
            mix_english_words("измерь харднес рафнес", &ru_words, &en_words, &mock_word),
            "измерь hardness roughness"
        );
    }

    #[test]
    fn leaves_dictionary_russian_and_preserves_spacing_and_punctuation() {
        let ru_words = [
            timed_word("затем", 0.1, 0.3),
            timed_word("харднес", 0.4, 0.8),
        ];
        let en_words = [timed_word("hardness", 0.45, 0.75)];
        assert_eq!(
            mix_english_words("  Затем, харднес!  ", &ru_words, &en_words, &mock_word),
            "  Затем, hardness!  "
        );

        let ru_words = [timed_word("слой", 0.1, 0.5)];
        let en_words = [timed_word("layer", 0.12, 0.48)];
        assert_eq!(
            mix_english_words("слой", &ru_words, &en_words, &mock_word),
            "слой"
        );
    }

    #[test]
    fn protects_slang_and_profanity_from_accidental_english_matches() {
        for (ru, en) in [("пиздец", "pizza"), ("вайб", "vibe"), ("кринж", "cringe")]
        {
            let ru_words = [timed_word(ru, 0.1, 0.6)];
            let en_words = [timed_word(en, 0.12, 0.58)];
            assert_eq!(mix_english_words(ru, &ru_words, &en_words, &mock_word), ru);
        }
    }

    #[test]
    fn similarity_cutoff_separates_roughness_from_near_homophones() {
        let roughness = strsim::jaro_winkler(
            &russian_sound_key("рафнес"),
            &english_sound_key("roughness"),
        );
        let pizza = strsim::jaro_winkler(&russian_sound_key("пиздец"), &english_sound_key("pizza"));
        let mosque = strsim::jaro_winkler(&russian_sound_key("маск"), &english_sound_key("mosque"));
        assert!(
            (roughness - 0.8375).abs() < 0.01,
            "roughness score={roughness:.4}"
        );
        assert!((mosque - 0.775).abs() < 0.01, "mosque score={mosque:.4}");
        assert!(
            roughness >= MIX_ENGLISH_MIN_SIMILARITY,
            "roughness similarity={roughness:.3}"
        );
        assert!(
            pizza < MIX_ENGLISH_MIN_SIMILARITY,
            "pizza similarity={pizza:.3}"
        );
        assert!(
            mosque < MIX_ENGLISH_MIN_SIMILARITY,
            "mosque similarity={mosque:.3}"
        );
    }

    #[test]
    fn requires_both_dictionary_answers_and_real_time_overlap() {
        let ru_words = [timed_word("харднес", 0.2, 0.5)];
        let no_dictionary = |_: &str, _: &str| None;
        assert_eq!(
            mix_english_words(
                "харднес",
                &ru_words,
                &[timed_word("hardness", 0.21, 0.49)],
                &no_dictionary
            ),
            "харднес"
        );
        let unknown_english = |lang: &str, word: &str| {
            if lang == "ru-RU" {
                Some(false)
            } else if lang == "en-US" && word == "hardness" {
                Some(false)
            } else {
                None
            }
        };
        assert_eq!(
            mix_english_words(
                "харднес",
                &ru_words,
                &[timed_word("hardness", 0.21, 0.49)],
                &unknown_english
            ),
            "харднес"
        );
        assert_eq!(
            mix_english_words(
                "харднес",
                &ru_words,
                &[timed_word("hardness", 0.5, 0.8)],
                &mock_word
            ),
            "харднес"
        );
    }

    #[test]
    fn can_join_overlapping_english_words_for_one_russian_word() {
        let ru_words = [timed_word("харднес", 0.1, 0.8)];
        let en_words = [
            timed_word("hard", 0.12, 0.42),
            timed_word("ness", 0.44, 0.76),
        ];
        assert_eq!(
            mix_english_words("харднес", &ru_words, &en_words, &mock_word),
            "hard ness"
        );
    }

    #[test]
    fn refuses_ambiguous_timings_and_mismatched_russian_text() {
        let ru_words = [
            timed_word("затем", 0.0, 0.5),
            timed_word("харднес", 0.5, 1.0),
        ];
        let en_words = [timed_word("hardness", 0.4, 0.6)];
        assert_eq!(
            mix_english_words("затем харднес", &ru_words, &en_words, &mock_word),
            "затем харднес"
        );
        assert_eq!(
            mix_english_words("что-то другое", &ru_words, &en_words, &mock_word),
            "что-то другое"
        );
    }

    #[test]
    fn capitalizes_sentences_and_joins_chunks() {
        let mut f = Formatter::new();
        assert_eq!(text(&f.format("привет как дела")), "Привет как дела");
        assert_eq!(
            text(&f.format("у меня всё хорошо точка")),
            " у меня всё хорошо."
        );
        assert_eq!(text(&f.format("завтра увидимся")), " Завтра увидимся");
    }

    #[test]
    fn spoken_punctuation_and_newline() {
        let mut f = Formatter::new();
        let pieces =
            f.format("во первых запятая это вопросительный знак новая строка дальше тире конец");
        assert_eq!(
            pieces,
            vec![
                Piece::Text("Во первых, это?".into()),
                Piece::NewLine,
                Piece::Text("Дальше — конец".into()),
            ]
        );
        let mut f = Formatter::new();
        assert_eq!(text(&f.format("раз точка с запятой два")), "Раз; 2");
    }

    #[test]
    fn spoken_mark_after_the_same_model_mark_is_not_doubled() {
        let mut f = Formatter::new();
        assert_eq!(
            text(&f.format("copy for review? question mark так")),
            "Copy for review? Так"
        );
    }

    #[test]
    fn particles_get_their_hyphen_back() {
        let mut f = Formatter::new();
        assert_eq!(
            text(&f.format("если ктото задержится скажи что нибудь")),
            "Если кто-то задержится скажи что-нибудь"
        );
        let mut f = Formatter::new();
        assert_eq!(
            text(&f.format("как то так а то нет")),
            "Как-то так а то нет"
        );
    }

    #[test]
    fn model_period_at_chunk_end_is_dropped_but_spoken_one_kept() {
        let mut f = Formatter::new();
        assert_eq!(
            text(&f.format("I think we should move it.")),
            "I think we should move it"
        );
        assert_eq!(text(&f.format("готово точка")), " готово.");
        assert_eq!(text(&f.format("One. Two.")), " One. Two");
    }

    #[test]
    fn keeps_model_punctuation_and_case() {
        let mut f = Formatter::new();
        assert_eq!(
            text(&f.format("hello, world. how are you?")),
            "Hello, world. How are you?"
        );
    }

    #[test]
    fn stop_phrases_at_the_end_of_a_chunk() {
        let (w, s) = (wake(), stops());
        assert_eq!(trailing_stop_words("стоп ввод", &s, &w), Some(2));
        assert_eq!(trailing_stop_words("стоп вод", &s, &w), Some(2));
        assert_eq!(trailing_stop_words("и это всё стоп ввод", &s, &w), Some(2));
        assert_eq!(trailing_stop_words("ривер стоп ввод", &s, &w), Some(3));
        assert_eq!(trailing_stop_words("ривер остановись", &s, &w), Some(2));
        assert_eq!(trailing_stop_words("ревер остановитесь", &s, &w), Some(2));
        // без wake-слова «остановись» — просто текст
        assert_eq!(trailing_stop_words("остановись", &s, &w), None);
        assert_eq!(trailing_stop_words("стоп вводить данные", &s, &w), None);
        assert_eq!(trailing_stop_words("обычный текст", &s, &w), None);
    }

    #[test]
    fn language_switch_phrases() {
        assert_eq!(language_switch("по английски"), Some(Lang::En));
        assert_eq!(language_switch("english"), Some(Lang::En));
        assert_eq!(language_switch("по русски"), Some(Lang::Ru));
        assert_eq!(language_switch("английский язык сложный"), None);
        assert_eq!(
            leading_language_switch("по английски hello world"),
            Some((Lang::En, 2))
        );
        assert_eq!(
            leading_language_switch("english hello"),
            Some((Lang::En, 1))
        );
        assert_eq!(leading_language_switch("привет мир"), None);
        // так приходит из ASR после textnorm::normalize
        assert_eq!(language_switch(&normalize("По-английски")), Some(Lang::En));
        assert_eq!(language_switch(&normalize("по-русски")), Some(Lang::Ru));
    }

    #[test]
    fn trims_words_from_both_ends() {
        assert_eq!(trim_words("ривер напиши Привет мир", 2, 0), "Привет мир");
        assert_eq!(trim_words("текст стоп ввод", 0, 2), "текст");
        assert_eq!(trim_words("стоп ввод", 0, 2), "");
    }
}
