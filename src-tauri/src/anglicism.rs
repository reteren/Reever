//! Английские слова в кириллической диктовке (русский, украинский, болгарский) по словарю
//! их русских фонетических записей.
//!
//! Модель иногда пишет английский термин кириллицей так, как его слышит: «апасити»,
//! «харднес», «эскейп». Включается галочкой в настройках и только если среди языков
//! распознавания есть кириллический (`CYRILLIC_LANGUAGES`); латиницу и другие
//! алфавиты не трогает. Вторая (английская) модель на русской фразе термин почти не слышит
//! (замеры в testdata/*-mix-report.md: 4–15 из 98), а словарю вторая модель не нужна:
//! транслит сравнивается с вариантами записи английских слов (CMUdict → кириллица,
//! testdata/lexicon/build_lexicon.py → resources/anglicisms.tsv).
//!
//! Меняется только слово, которого нет в словарях Windows включённых кириллических языков; формы с русским
//! окончанием («скейлится», «майлстоунов») остаются кириллицей — гибрид «scaleится» хуже.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use strsim::normalized_levenshtein;

/// Сила замены из настроек: 1 — только очевидные английские слова, 5 — всё, что похоже
/// на английское и не нашлось в русском словаре.
pub const MIN_STRENGTH: u8 = 1;
pub const MAX_STRENGTH: u8 = 5;
pub const DEFAULT_STRENGTH: u8 = 2;

/// Кириллические языки распознавания и их словари Windows: только с ними замена работает.
/// Записи в словаре русские; украинские и болгарские слова сводятся к ним в `normalize`.
pub const CYRILLIC_LANGUAGES: &[(&str, &str)] = &[("ru", "ru-RU"), ("uk", "uk-UA"), ("bg", "bg-BG")];

/// Пороги одной ступени силы.
#[derive(Clone, Copy, Debug)]
struct Strength {
    /// Нижняя граница фонетического сходства для слова из общего словаря. На ступени 3
    /// это 0,925: ниже начинается случайная омонимия («бинды» / bandy 0,93 уже ошибка).
    general: f64,
    /// Для доменных терминов порог мягче: ошибка там дешевле, промах дороже.
    domain: f64,
    /// Лучший кандидат должен оторваться от второго, иначе слово остаётся как было
    /// («сайс»: size или sighs — решает приоритет термина, а не случай).
    margin: f64,
    /// Общее слово короче — не меняется; ровно такой длины — только точная запись слова
    /// с частотой не ниже `short_weight`. Среди коротких созвучий слишком много («стро» /
    /// straw, «зом» / them). Термины («тул», «пнг») — можно на любой ступени.
    short_chars: usize,
    short_weight: f64,
}

fn strength(level: u8) -> Strength {
    match level.clamp(MIN_STRENGTH, MAX_STRENGTH) {
        1 => Strength { general: 0.99, domain: 0.95, margin: 0.05, short_chars: 6, short_weight: 0.001 },
        2 => Strength { general: 0.96, domain: 0.86, margin: 0.05, short_chars: 5, short_weight: 0.001 },
        3 => Strength { general: 0.925, domain: 0.78, margin: 0.05, short_chars: 4, short_weight: 0.001 },
        4 => Strength { general: 0.88, domain: 0.74, margin: 0.03, short_chars: 4, short_weight: 0.0 },
        _ => Strength { general: 0.80, domain: 0.70, margin: 0.01, short_chars: 4, short_weight: 0.0 },
    }
}

const FREQUENCY_BONUS: f64 = 0.015;
const DOMAIN_BONUS: f64 = 0.06;

/// Короче — слишком много совпадений с обрывками русских слов.
const MIN_TOKEN_CHARS: usize = 3;
const MAX_TOKEN_CHARS: usize = 24;

/// Разговорные слова, которые словарь Windows не знает, а созвучное английское найдётся
/// («вайб» / vibe). Неоднозначный токен безопаснее оставить по-русски.
const SLANG_GUARD: &[&str] = &[
    "блин", "капец", "кек", "кринж", "имба", "вайб", "зашквар", "жесть", "пиздец", "пипец",
    "хрень", "треш", "трэш", "чилл", "чилить", "хайп", "лол", "рофл", "угар", "фигня",
    "фигово", "офигеть", "задолбал", "норм", "спс", "пж", "ок", "окей", "кста", "нихера",
    "нифига", "немно",
    // Названия, которые пишут кириллицей, а словарь Windows знает только с заглавной
    // или не знает вовсе: ближайшее английское слово неверно («ютуб» / tube). Проверять
    // всё подряд с заглавной нельзя — там же имена «Джейсон», «Эдит» (JSON, edit).
    "ютуб", "ютуба", "ютубе", "ютубу", "ютубом", "христос",
];

/// Записи, которые словарь Windows считает русскими словами, но в речи о программах это
/// почти всегда английский термин: «хедер» (религиозная школа) — header, «сейв» — save.
/// Привычные заимствования («таймер», «слайдер», «файл») сюда не входят: их пишут по-русски.
const OVER_RUSSIAN: &[(&str, &str)] = &[
    ("хедер", "header"),
    ("хэдер", "header"),
    ("футер", "footer"),
    ("сейв", "save"),
];

/// Окончания, после которых слово — русская форма, а не английское слово
/// («лагать», «рандомный»): английский термин так не звучит.
const RUSSIAN_ENDINGS: &[&str] = &[
    "ться", "тся", "ть", "ешь", "ишь", "ете", "ите", "ый", "ий", "ая", "яя", "ое", "ее", "ые",
    "ие", "ого", "его", "ому", "ему", "ыми", "ими", "ую", "юю",
    // Глагол настоящего времени («лагает», «юзают»).
    "ает", "яет", "ует", "ают", "яют", "уют",
    // Английское слово на «ы» не кончается: это русское множественное («бинды»).
    "ы",
];

/// Окончания словоизменения: «майлстоунов», «резолюшена». Если без окончания слово
/// совпадает с английским лучше, чем целиком, — это русская форма англицизма.
const INFLECTIONS: &[&str] = &[
    "ами", "ями", "ов", "ев", "ом", "ем", "ах", "ях", "ам", "ям", "ой", "ей", "ою", "ею",
    "нул", "нула", "нули", "ил", "ила", "или", "ал", "ала", "али", "а", "я", "ы", "и", "у",
    "ю", "е",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Priority {
    General,
    Domain,
}

struct Entry {
    display: String,
    weight: f64,
    priority: Priority,
}

/// Словарь англицизмов: индекс вариантов записи по согласному «скелету».
pub struct Anglicisms {
    entries: Vec<Entry>,
    /// Варианты: (запись, скелет, индекс слова).
    variants: Vec<(String, String, usize)>,
    by_skeleton: HashMap<String, Vec<usize>>,
    by_gram: HashMap<String, Vec<usize>>,
    /// Точные подмены мимо скоринга (`OVER_RUSSIAN`).
    exact: HashMap<String, String>,
    strength: Strength,
}

#[derive(Clone, Debug)]
struct Candidate {
    entry: usize,
    similarity: f64,
    score: f64,
}

impl Anglicisms {
    /// Загружает resources/anglicisms.tsv; `level` — сила замены 1–5.
    pub fn load(path: &Path, level: u8) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("словарь англицизмов {}", path.display()))?;
        Ok(Self::parse(&text, level))
    }

    /// Формат строки: `display<TAB>weight<TAB>domain<TAB>вариант,вариант`.
    pub fn parse(text: &str, level: u8) -> Self {
        let mut lexicon = Self {
            entries: Vec::new(),
            variants: Vec::new(),
            by_skeleton: HashMap::new(),
            by_gram: HashMap::new(),
            exact: HashMap::new(),
            strength: strength(level),
        };
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let (Some(display), Some(weight), Some(domain), Some(variants)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let priority = if domain == "1" {
                Priority::Domain
            } else {
                Priority::General
            };
            let mut variants: Vec<String> = variants.split(',').map(str::to_string).collect();
            if priority == Priority::Domain {
                for spoken in variants.iter().map(|v| akanye(v)).collect::<Vec<_>>() {
                    if !variants.contains(&spoken) {
                        variants.push(spoken);
                    }
                }
            }
            lexicon.push(
                display,
                weight.parse().unwrap_or(0.0),
                priority,
                variants.iter().map(String::as_str),
            );
        }
        for (heard, english) in OVER_RUSSIAN {
            lexicon.exact.insert((*heard).to_string(), (*english).to_string());
        }
        lexicon
    }

    fn push<'a>(
        &mut self,
        display: &str,
        weight: f64,
        priority: Priority,
        variants: impl Iterator<Item = &'a str>,
    ) {
        let entry = self.entries.len();
        self.entries.push(Entry {
            display: display.to_string(),
            weight,
            priority,
        });
        for variant in variants {
            let variant = normalize(variant);
            let skeleton = skeleton(&variant);
            if skeleton.is_empty() {
                continue;
            }
            let index = self.variants.len();
            for gram in grams(&skeleton) {
                self.by_gram.entry(gram).or_default().push(index);
            }
            self.by_skeleton.entry(skeleton.clone()).or_default().push(index);
            self.variants.push((variant, skeleton, entry));
        }
    }

    /// Два лучших английских слова для кириллического токена.
    fn best(&self, token: &str) -> (Option<Candidate>, Option<Candidate>) {
        let query = normalize(token);
        let length = query.chars().count();
        if !(MIN_TOKEN_CHARS..=MAX_TOKEN_CHARS).contains(&length) {
            return (None, None);
        }
        let query_skeleton = skeleton(&query);
        if query_skeleton.is_empty() {
            return (None, None);
        }
        let mut pool: HashSet<usize> = HashSet::new();
        if let Some(hits) = self.by_skeleton.get(&query_skeleton) {
            pool.extend(hits);
        }
        if query_skeleton.chars().count() >= 3 {
            for gram in grams(&query_skeleton) {
                if let Some(hits) = self.by_gram.get(&gram) {
                    pool.extend(hits);
                }
            }
        }

        let slack = ((length as f64) * 0.25).ceil() as usize;
        let mut per_entry: HashMap<usize, f64> = HashMap::new();
        for index in pool {
            let (variant, variant_skeleton, entry) = &self.variants[index];
            if variant.chars().count().abs_diff(length) > slack {
                continue;
            }
            let similarity = 0.65 * normalized_levenshtein(&query_skeleton, variant_skeleton)
                + 0.35 * normalized_levenshtein(&query, variant);
            let slot = per_entry.entry(*entry).or_insert(0.0);
            if similarity > *slot {
                *slot = similarity;
            }
        }

        let mut ranked: Vec<Candidate> = per_entry
            .into_iter()
            .map(|(entry, similarity)| {
                let item = &self.entries[entry];
                let bonus = match item.priority {
                    Priority::General => 0.0,
                    Priority::Domain => DOMAIN_BONUS,
                };
                Candidate {
                    entry,
                    similarity,
                    score: similarity + bonus + FREQUENCY_BONUS * item.weight.sqrt(),
                }
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| self.entries[a.entry].display.cmp(&self.entries[b.entry].display))
        });
        let mut ranked = ranked.into_iter();
        (ranked.next(), ranked.next())
    }

    /// Два лучших кандидата с похожестью и итоговым баллом — для замеров и журнала.
    pub fn explain(&self, token: &str) -> Vec<(String, f64, f64)> {
        let (best, runner_up) = self.best(token);
        [best, runner_up]
            .into_iter()
            .flatten()
            .map(|c| (self.entries[c.entry].display.clone(), c.similarity, c.score))
            .collect()
    }

    /// Уверенная подмена для одного токена: английское слово или `None`.
    fn replacement(&self, token: &str) -> Option<String> {
        let (best, runner_up) = self.best(token);
        let best = best?;
        let entry = &self.entries[best.entry];
        let strength = self.strength;
        let threshold = if entry.priority == Priority::General {
            strength.general
        } else {
            strength.domain
        };
        let margin = best.score - runner_up.map_or(0.0, |r| r.score);
        let length = token.chars().count();
        let short_general = entry.priority == Priority::General
            && (length < strength.short_chars
                || (length == strength.short_chars
                    && (best.similarity < 1.0 || entry.weight < strength.short_weight)));
        (best.similarity >= threshold && margin >= strength.margin && !short_general)
            .then(|| entry.display.clone())
    }

    /// Русская форма англицизма («майлстоунов»): основа без окончания — точная запись
/// английского слова, а целиком слово точно не совпадает. Нестрогое совпадение основы
/// здесь вредно: «апасити» = «апасит» + «и» ≈ opposite.
    fn is_inflected(&self, token: &str) -> bool {
        let (full, _) = self.best(token);
        let full = full.map_or(0.0, |c| c.similarity);
        INFLECTIONS.iter().any(|ending| {
            let Some(root) = token.strip_suffix(ending) else {
                return false;
            };
            // «трея» — «трей» + «я»: перед окончанием на гласную пропадает «й».
            let restored = format!("{root}й");
            let roots = if "яюеи".contains(ending.chars().next().unwrap_or(' ')) {
                vec![root, restored.as_str()]
            } else {
                vec![root]
            };
            roots.into_iter().any(|root| {
                root.chars().count() >= MIN_TOKEN_CHARS
                    && self.best(root).0.is_some_and(|c| c.similarity >= 0.99 && full < 0.99)
            })
        })
    }

    /// Подменяет в тексте диктовки транслит английских слов латиницей.
    ///
    /// `is_russian(word)` — есть ли слово (в нижнем регистре, как написано) в словаре языка
    /// (`None` — проверка недоступна, тогда текст не меняется: без словаря нельзя отличить
    /// «харднес» от «хорошо»).
    pub fn restore(&self, text: &str, is_russian: &dyn Fn(&str) -> Option<bool>) -> String {
        let mut result = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end) in word_spans(text) {
            result.push_str(&text[cursor..start]);
            let token = &text[start..end];
            match self.word_replacement(token, is_russian) {
                Some(english) => result.push_str(&match_case(token, &english)),
                None => result.push_str(token),
            }
            cursor = end;
        }
        result.push_str(&text[cursor..]);
        result
    }

    fn word_replacement(
        &self,
        token: &str,
        is_russian: &dyn Fn(&str) -> Option<bool>,
    ) -> Option<String> {
        let key = normalize(token);
        if !key.chars().all(is_cyrillic) || key.chars().count() < MIN_TOKEN_CHARS {
            return None;
        }
        if let Some(english) = self.exact.get(&key) {
            return Some(english.clone());
        }
        if SLANG_GUARD.contains(&key.as_str())
            || RUSSIAN_ENDINGS.iter().any(|ending| key.ends_with(ending))
            || is_russian(&token.to_lowercase()) != Some(false)
            || self.is_inflected(&key)
        {
            return None;
        }
        self.replacement(&key)
    }
}

/// Границы слов: буквы и дефис внутри слова; пунктуация и пробелы остаются на месте.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        if character.is_alphabetic() {
            start.get_or_insert(index);
        } else if let Some(word_start) = start.take() {
            spans.push((word_start, index));
        }
    }
    if let Some(word_start) = start {
        spans.push((word_start, text.len()));
    }
    spans
}

/// Регистр подмены: аббревиатуры (PNG) и имена собственные (GitHub) как в словаре,
/// обычное слово — с заглавной только там, где заглавной была русская запись.
fn match_case(original: &str, english: &str) -> String {
    if english.chars().any(char::is_uppercase) {
        return english.to_string();
    }
    let capitalized = original.chars().next().is_some_and(char::is_uppercase);
    if capitalized {
        let mut chars = english.chars();
        return chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
    }
    english.to_string()
}

/// Нижний регистр и русские буквы вместо украинских: «апасіті» ищется как «апасити».
fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for character in text
        .trim()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
    {
        match character {
            'ё' | 'є' => normalized.push('е'),
            'і' => normalized.push('и'),
            'ї' => normalized.push_str("йи"),
            'ґ' => normalized.push('г'),
            other => normalized.push(other),
        }
    }
    normalized
}

fn is_cyrillic(character: char) -> bool {
    ('\u{0400}'..='\u{04ff}').contains(&character)
}

/// Согласный скелет: гласные в транслите плавают сильнее всего («апасити» / «опэсати»).
fn skeleton(word: &str) -> String {
    word.chars()
        .filter(|character| !"аоуыэеияюёьъй".contains(*character))
        .collect()
}

/// Безударное «о» русская модель пишет как «а» («апасити» / «опэсати»), «э» — как «е».
/// Только для терминов: в общем словаре такие варианты дают лишние созвучия.
fn akanye(word: &str) -> String {
    word.chars()
        .map(|character| match character {
            'о' => 'а',
            'э' => 'е',
            other => other,
        })
        .collect()
}

fn grams(skeleton: &str) -> Vec<String> {
    let chars: Vec<char> = skeleton.chars().collect();
    let size = if chars.len() > 6 { 3 } else { 2 };
    chars.windows(size).map(|window| window.iter().collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEXICON: &str = "\
# display\tweight\tdomain\tvariants
life\t0.0121\t0\tлайф,лэйф
size\t0.0014\t1\tсайз,зайз,сэйз,сайс
sighs\t0.00009\t0\tсайз,зайз,сэйз,сайс
escape\t0.0014\t1\tискэйп,ыскэйп,ескэйп,искейп
scape\t0.00001\t0\tскэйп,скейп
opposite\t0.0013\t0\tапазат,опазат,апасат,апзат
opacity\t0.0000066\t1\tоупэсати,опэсати,оупасати,опасити
hardness\t0.00008\t1\tхарднас,харднес,харднис
legate\t0.00003\t0\tлэгат,легат
throb\t0.00002\t0\tсроб,фроб
milestone\t0.00002\t0\tмайлстоун,майлстон
PNG\t0.08\t1\tпнг,пиэнджи
";

    /// Ступень 3 «сбалансированно»: на ней проверены общие слова (на 2 «лайф» не меняется).
    fn lexicon() -> Anglicisms {
        Anglicisms::parse(LEXICON, 3)
    }

    /// Словарь Windows в тестах: всё, чего нет в списке, — не русское слово.
    fn russian(word: &str) -> Option<bool> {
        const WORDS: &[&str] = &[
            "можно", "крутить", "ластику", "нельзя", "нажать", "просто", "кисти", "уменьши",
            "картинку", "стер", "лагать", "рас", "раз", "открой", "запусти", "нет", "это", "вот",
            "но", "потом",
        ];
        Some(WORDS.contains(&word))
    }

    #[test]
    fn restores_terms_from_the_user_log() {
        let words = lexicon();
        assert_eq!(
            words.restore("можно крутить харднес но апасити ластику нельзя", &russian),
            "можно крутить hardness но opacity ластику нельзя"
        );
        assert_eq!(words.restore("нажать просто эскейп.", &russian), "нажать просто escape.");
        assert_eq!(words.restore("стер пнг картинку", &russian), "стер PNG картинку");
        assert_eq!(words.restore("уменьши сайс кисти", &russian), "уменьши size кисти");
    }

    #[test]
    fn keeps_russian_words_slang_and_russian_forms() {
        let words = lexicon();
        for text in ["можно лагать", "вот кринж", "это зашквар", "можно раз"] {
            assert_eq!(words.restore(text, &russian), text);
        }
        // Русская форма англицизма остаётся кириллицей: «milestoneов» хуже.
        assert_eq!(words.restore("нет майлстоунов", &russian), "нет майлстоунов");
        assert_eq!(words.restore("это сроба", &russian), "это сроба");
    }

    #[test]
    fn general_words_need_a_close_match() {
        let words = lexicon();
        assert_eq!(words.restore("это лайф", &russian), "это life");
        assert_eq!(words.restore("это майлстоун", &russian), "это milestone");
    }

    #[test]
    fn without_a_dictionary_nothing_changes() {
        let words = lexicon();
        assert_eq!(words.restore("харднес", &|_| None), "харднес");
    }

    #[test]
    fn keeps_case_and_punctuation() {
        let words = lexicon();
        assert_eq!(words.restore("Харднес, а потом пнг!", &russian), "Hardness, а потом PNG!");
    }

    #[test]
    fn ukrainian_letters_map_to_the_russian_spelling() {
        let words = lexicon();
        let ukrainian = |word: &str| Some(["зменш", "пензля"].contains(&word));
        assert_eq!(words.restore("зменш апасіті пензля", &ukrainian), "зменш opacity пензля");
        // Словарю языка уходит слово как написано, а не сведённое к русскому.
        let seen = std::cell::RefCell::new(Vec::new());
        words.restore("апасіті", &|word| {
            seen.borrow_mut().push(word.to_string());
            Some(false)
        });
        assert_eq!(seen.borrow().as_slice(), ["апасіті"]);
    }

    #[test]
    fn strength_one_keeps_only_obvious_words() {
        let words = Anglicisms::parse(LEXICON, 1);
        // Термин с точной записью — меняется и на самой слабой ступени.
        assert_eq!(words.restore("уменьши сайз кисти", &russian), "уменьши size кисти");
        // Общие слова короче шести букв — нет, хотя на ступени 3 они меняются.
        assert_eq!(words.restore("это лайф", &russian), "это лайф");
        assert_eq!(words.restore("это легат", &russian), "это легат");
        assert_eq!(lexicon().restore("это легат", &russian), "это legate");
    }

    #[test]
    fn strength_five_takes_loose_matches_but_not_russian_words() {
        let words = Anglicisms::parse(LEXICON, 5);
        assert_eq!(words.restore("это лайф", &russian), "это life");
        // Русские слова, сленг и русские формы не трогает ни одна ступень.
        for text in ["можно лагать", "вот кринж", "нет майлстоунов", "открой картинку"] {
            assert_eq!(words.restore(text, &russian), text);
        }
    }

    #[test]
    fn out_of_range_strength_is_clamped() {
        assert_eq!(strength(0).general, strength(1).general);
        assert_eq!(strength(9).general, strength(5).general);
    }
}
