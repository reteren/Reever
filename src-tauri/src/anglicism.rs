//! Английские слова в русской диктовке по словарю их русских фонетических записей.
//!
//! GigaAM — русская модель и пишет английский термин так, как его слышит: «апасити»,
//! «харднес», «эскейп». Вторая (английская) модель на русской фразе термин почти не слышит
//! (замеры в testdata/*-mix-report.md: 4–15 из 98), а словарю вторая модель не нужна:
//! транслит сравнивается с вариантами записи английских слов (CMUdict → кириллица,
//! testdata/lexicon/build_lexicon.py → resources/anglicisms.tsv).
//!
//! Меняется только слово, которого нет в русском словаре Windows; формы с русским
//! окончанием («скейлится», «майлстоунов») остаются кириллицей — гибрид «scaleится» хуже.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use strsim::normalized_levenshtein;

/// Нижняя граница фонетического сходства для слова из общего словаря. Ниже начинается
/// случайная омонимия: «бинды» / bandy 0,93 уже ошибка, «редит» / radiate 0,90.
const MIN_SIMILARITY: f64 = 0.925;
/// Короткое общее слово («лайф», «плей») — только точная запись частого слова: среди
/// четырёх букв созвучий слишком много («стро» / straw, «эрас» / heiress); из трёх букв
/// общие слова не берутся совсем («зом» / them). Термины («тул», «пнг») — можно.
const SHORT_TOKEN_CHARS: usize = 4;
const SHORT_MIN_WEIGHT: f64 = 0.001;
/// Для доменных терминов и слов пользователя порог мягче: ошибка там дешевле, промах дороже.
const MIN_SIMILARITY_PRIORITY: f64 = 0.78;
/// Лучший кандидат должен оторваться от второго, иначе слово остаётся как было
/// («сайс»: size или sighs — решает приоритет термина, а не случай).
const MIN_MARGIN: f64 = 0.05;
const FREQUENCY_BONUS: f64 = 0.015;
const DOMAIN_BONUS: f64 = 0.06;
const USER_BONUS: f64 = 0.12;

/// Короче — слишком много совпадений с обрывками русских слов.
const MIN_TOKEN_CHARS: usize = 3;
const MAX_TOKEN_CHARS: usize = 24;

/// Разговорные слова, которые словарь Windows не знает, а созвучное английское найдётся
/// («вайб» / vibe). Неоднозначный токен безопаснее оставить по-русски.
const SLANG_GUARD: &[&str] = &[
    "блин", "капец", "кек", "кринж", "имба", "вайб", "зашквар", "жесть", "пиздец", "пипец",
    "хрень", "треш", "трэш", "чилл", "чилить", "хайп", "лол", "рофл", "угар", "фигня",
    "фигово", "офигеть", "задолбал", "норм", "спс", "пж", "ок", "окей", "кста",
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
    User,
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
    /// Явные пары пользователя «апасити=opacity» — точное совпадение, мимо скоринга.
    exact: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct Candidate {
    entry: usize,
    similarity: f64,
    score: f64,
}

impl Anglicisms {
    /// Загружает resources/anglicisms.tsv и добавляет слова пользователя.
    pub fn load(path: &Path, user_terms: &[String]) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("словарь англицизмов {}", path.display()))?;
        Ok(Self::parse(&text, user_terms))
    }

    /// Формат строки: `display<TAB>weight<TAB>domain<TAB>вариант,вариант`.
    pub fn parse(text: &str, user_terms: &[String]) -> Self {
        let mut lexicon = Self {
            entries: Vec::new(),
            variants: Vec::new(),
            by_skeleton: HashMap::new(),
            by_gram: HashMap::new(),
            exact: HashMap::new(),
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
        lexicon.add_user_terms(user_terms);
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

    /// Слова пользователя из настроек: `opacity` — поднять приоритет (или добавить с
    /// записью по буквам), `апасити=opacity` — точная подмена.
    fn add_user_terms(&mut self, user_terms: &[String]) {
        for term in user_terms {
            let term = term.trim();
            if term.is_empty() {
                continue;
            }
            if let Some((heard, english)) = term.split_once('=') {
                let (heard, english) = (normalize(heard), english.trim());
                if !heard.is_empty() && !english.is_empty() {
                    self.exact.insert(heard, english.to_string());
                }
                continue;
            }
            let key = term.to_lowercase();
            let mut found = false;
            for entry in &mut self.entries {
                if entry.display.to_lowercase() == key {
                    entry.priority = Priority::User;
                    found = true;
                }
            }
            if !found {
                let spelled = spell_in_cyrillic(&key);
                if !spelled.is_empty() {
                    self.push(term, 0.0, Priority::User, std::iter::once(spelled.as_str()));
                }
            }
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
                    Priority::User => USER_BONUS,
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
        let threshold = if entry.priority == Priority::General {
            MIN_SIMILARITY
        } else {
            MIN_SIMILARITY_PRIORITY
        };
        let margin = best.score - runner_up.map_or(0.0, |r| r.score);
        let length = token.chars().count();
        let short_general = entry.priority == Priority::General
            && (length < SHORT_TOKEN_CHARS
                || (length == SHORT_TOKEN_CHARS
                    && (best.similarity < 1.0 || entry.weight < SHORT_MIN_WEIGHT)));
        (best.similarity >= threshold && margin >= MIN_MARGIN && !short_general)
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
    /// `is_russian(word)` — есть ли слово в русском словаре (`None` — проверка недоступна,
    /// тогда текст не меняется: без словаря нельзя отличить «харднес» от «хорошо»).
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
            || is_russian(&key) != Some(false)
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

fn normalize(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|character| if character == 'ё' { 'е' } else { character })
        .collect()
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

/// Запись английского слова «по буквам» для слов пользователя, которых нет в словаре.
fn spell_in_cyrillic(word: &str) -> String {
    let mut output = String::new();
    let chars: Vec<char> = word.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let pair: String = chars[index..(index + 2).min(chars.len())].iter().collect();
        let mapped = match pair.as_str() {
            "sh" => Some("ш"),
            "ch" => Some("ч"),
            "th" => Some("с"),
            "ph" => Some("ф"),
            "ck" => Some("к"),
            "ee" => Some("и"),
            "oo" => Some("у"),
            _ => None,
        };
        if let Some(mapped) = mapped {
            output.push_str(mapped);
            index += 2;
            continue;
        }
        output.push_str(match chars[index] {
            'a' => "а",
            'b' => "б",
            'c' => "к",
            'd' => "д",
            'e' => "е",
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
            'u' => "а",
            'v' => "в",
            'w' => "в",
            'x' => "кс",
            'y' => "и",
            'z' => "з",
            _ => "",
        });
        index += 1;
    }
    output
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

    fn lexicon(user: &[&str]) -> Anglicisms {
        let user: Vec<String> = user.iter().map(|s| s.to_string()).collect();
        Anglicisms::parse(LEXICON, &user)
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
        let words = lexicon(&[]);
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
        let words = lexicon(&[]);
        for text in ["можно лагать", "вот кринж", "это зашквар", "можно раз"] {
            assert_eq!(words.restore(text, &russian), text);
        }
        // Русская форма англицизма остаётся кириллицей: «milestoneов» хуже.
        assert_eq!(words.restore("нет майлстоунов", &russian), "нет майлстоунов");
        assert_eq!(words.restore("это сроба", &russian), "это сроба");
    }

    #[test]
    fn general_words_need_a_close_match() {
        let words = lexicon(&[]);
        assert_eq!(words.restore("это лайф", &russian), "это life");
        assert_eq!(words.restore("это майлстоун", &russian), "это milestone");
    }

    #[test]
    fn without_a_dictionary_nothing_changes() {
        let words = lexicon(&[]);
        assert_eq!(words.restore("харднес", &|_| None), "харднес");
    }

    #[test]
    fn keeps_case_and_punctuation() {
        let words = lexicon(&[]);
        assert_eq!(words.restore("Харднес, а потом пнг!", &russian), "Hardness, а потом PNG!");
    }

    #[test]
    fn user_terms_win_and_exact_pairs_apply() {
        let words = lexicon(&["тулирас=eraser tool", "Krita"]);
        assert_eq!(words.restore("открой тулирас", &russian), "открой eraser tool");
        assert_eq!(words.restore("запусти крита", &russian), "запусти Krita");
    }
}
