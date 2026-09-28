//! Диктовка без побочных эффектов: оформление распознанного текста (заглавные буквы,
//! произнесённая пунктуация, перевод строки) и поиск фразы остановки в конце куска.
//! Ввод в окно и буфер обмена — `typing.rs`; когда диктовка идёт — `engine.rs`.

use strsim::normalized_levenshtein;

use crate::speech::Lang;
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
    "кто", "что", "где", "куда", "откуда", "когда", "как", "какой", "какая", "какое", "какие",
    "какого", "какую", "каким", "чей", "чья", "чьё", "чье", "чьи", "почему", "зачем",
    "кого", "кому", "кем", "чего", "чему", "чем", "сколько",
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

/// Фраза целиком — переключение языка диктовки («по-английски», «русский»).
pub fn language_switch(norm: &str) -> Option<Lang> {
    let phrase = norm.trim();
    const EN: &[&str] = &[
        "по английски", "на английском", "английский", "english", "инглиш", "in english",
    ];
    const RU: &[&str] = &["по русски", "на русском", "русский", "russian", "рашн", "in russian"];
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
        assert_eq!(text(&f.format("copy for review? question mark так")), "Copy for review? Так");
    }

    #[test]
    fn particles_get_their_hyphen_back() {
        let mut f = Formatter::new();
        assert_eq!(text(&f.format("если ктото задержится скажи что нибудь")), "Если кто-то задержится скажи что-нибудь");
        let mut f = Formatter::new();
        assert_eq!(text(&f.format("как то так а то нет")), "Как-то так а то нет");
    }

    #[test]
    fn model_period_at_chunk_end_is_dropped_but_spoken_one_kept() {
        let mut f = Formatter::new();
        assert_eq!(text(&f.format("I think we should move it.")), "I think we should move it");
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
        assert_eq!(leading_language_switch("по английски hello world"), Some((Lang::En, 2)));
        assert_eq!(leading_language_switch("english hello"), Some((Lang::En, 1)));
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
