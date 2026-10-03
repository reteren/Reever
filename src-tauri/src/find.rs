//! «Ривер найди …» / «найди папку …» / «найди файл …»: поиск через Everything с выбором
//! в окне Reever. Запросы идут для каждого варианта распознавания (русский, английский,
//! транслит), без учёта пунктуации и пробелов — «project gst» находит «-PROJECT.GST-».
//! Наверх поднимается то, что открывают чаще: история открытий через Reever
//! (`usage.json`) и недавние элементы Windows (`%APPDATA%\Microsoft\Windows\Recent`).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use strsim::jaro_winkler;

use crate::everything;

/// Что искать.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FindScope {
    Any,
    Folder,
    File,
}

/// Найденный элемент — для окна выбора и для открытия.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FindItem {
    pub name: String,
    /// Полный путь.
    pub path: String,
    pub folder: bool,
    /// Часто/недавно открывался — показывается в верхнем разделе.
    pub frequent: bool,
    #[serde(skip)]
    pub score: f64,
}

/// Сколько показывать в окне выбора.
pub const MAX_SHOWN: usize = 40;
const PER_QUERY: usize = 120;
/// Нечёткий проход: сколько имён с тем же началом смотреть и какое сходство считать.
const FUZZY_PER_QUERY: usize = 500;
const FUZZY_MIN: f64 = 0.6 * 0.82;
/// Ранг совпадения, найденного пословным запросом (все слова есть в имени).
const ALL_WORDS_MATCHED: f64 = 0.85;
/// Сколько всего можно потратить на запросы к Everything (локально — миллисекунды).
const SEARCH_BUDGET_MS: u64 = 2_500;
/// Недавние Windows перечитываются не чаще раза в минуту.
const RECENT_TTL_SECS: u64 = 60;

// ---------------------------------------------------------------- варианты запроса

/// Варианты запроса из кандидатов распознавания: как есть, без пробелов, транслит
/// кириллицы в латиницу и буквы, сказанные по-русски («джи эс ти» → «gst»).
pub fn query_variants(candidates: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |value: String| {
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if value.chars().filter(|c| c.is_alphanumeric()).count() >= 2 && !out.contains(&value) {
            out.push(value);
        }
    };
    for candidate in candidates {
        let base = drop_spoken_punctuation(&candidate.trim().to_lowercase());
        if base.is_empty() {
            continue;
        }
        push(base.clone());
        if base.chars().any(is_cyrillic) {
            // Сначала «английское» прочтение (проджект → project), потом буквальный транслит.
            push(latin_letters_and(&base, english_translit));
            push(latin_letters_and(&base, translit));
        }
    }
    out
}

/// Пословный запрос для смешанных имён («деловые партнеры фотошоп» → папка «Деловые
/// партнеры Photoshop»): каждое русское слово — «само | английский транслит | транслит»,
/// длинное — по основе («длительности» найдёт «длительностей»). В Everything `|` — «ИЛИ»
/// внутри слова, пробел — «И» между словами. `None` — в запросе нет русских слов или он из
/// одного слова (тогда хватает обычных вариантов).
pub fn mixed_query(candidate: &str) -> Option<String> {
    let base = drop_spoken_punctuation(&candidate.trim().to_lowercase());
    let words: Vec<&str> = base.split_whitespace().collect();
    if words.len() < 2 || !base.chars().any(is_cyrillic) {
        return None;
    }
    let mut terms = Vec::new();
    let mut letters = String::new();
    for word in words {
        if let Some(spelled) = spelled_letters(word) {
            letters.push_str(&spelled);
            continue;
        }
        if !letters.is_empty() {
            terms.push(std::mem::take(&mut letters));
        }
        if !word.chars().any(is_cyrillic) {
            terms.push(word.to_string());
            continue;
        }
        // Основа — только для русского написания (русские слова склоняются); латиница — от
        // целого слова: английские имена не склоняются. «ф» часто «ph» (фотошоп → photoshop).
        let mut options = vec![russian_stem(word)];
        let english = english_translit(word);
        for latin in [english.clone(), translit(word), english.replace('f', "ph")] {
            if latin.len() >= 2 && !options.contains(&latin) {
                options.push(latin);
            }
        }
        terms.push(options.join("|"));
    }
    if !letters.is_empty() {
        terms.push(letters);
    }
    Some(terms.join(" "))
}

/// Основа русского слова для поиска подстрокой: длинное слово без двух последних букв
/// (окончание), короткое — как есть.
fn russian_stem(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let keep = match chars.len() {
        0..=4 => chars.len(),
        5..=6 => chars.len() - 1,
        _ => chars.len() - 2,
    };
    chars[..keep].iter().collect()
}

/// Произнесённая пунктуация в имени («проджект точка джи эс ти», «voice pipeline точка md»)
/// не нужна: Everything и так игнорирует пунктуацию в именах, а слово «точка» в запросе
/// стало бы лишним условием.
fn drop_spoken_punctuation(text: &str) -> String {
    const SPOKEN: &[&str] = &[
        "точка", "дефис", "тире", "подчеркивание", "подчёркивание", "нижнее", "слэш", "слеш",
        "dot", "dash", "hyphen", "underscore", "slash",
    ];
    text.split_whitespace()
        .filter(|word| !SPOKEN.contains(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_cyrillic(c: char) -> bool {
    matches!(c, 'а'..='я' | 'ё')
}

/// Названия латинских букв по-русски → буквы, остальные слова — через `convert`. Буквы,
/// сказанные подряд, склеиваются: «джи эс ти» и склеенное ASR «джиэсти» → «gst»
/// (в Everything пробел — это «И», раздельные буквы не нашлись бы).
fn latin_letters_and(text: &str, convert: fn(&str) -> String) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut letters = String::new();
    for word in text.split_whitespace() {
        if let Some(spelled) = spelled_letters(word) {
            letters.push_str(&spelled);
            continue;
        }
        if !letters.is_empty() {
            words.push(std::mem::take(&mut letters));
        }
        words.push(convert(word));
    }
    if !letters.is_empty() {
        words.push(letters);
    }
    words.join(" ")
}

/// Слово целиком из названий букв: «джи» → «g», «джиэсти» → «gst». Самое длинное
/// название — первым, чтобы «джей» не разобралось как «дж» + «ей».
fn spelled_letters(word: &str) -> Option<String> {
    if let Some(letter) = latin_letter(word) {
        return Some(letter.to_string());
    }
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let found = (2..=4.min(chars.len() - i)).rev().find_map(|len| {
            let piece: String = chars[i..i + len].iter().collect();
            latin_letter(&piece).map(|letter| (len, letter))
        });
        let (len, letter) = found?;
        out.push_str(letter);
        i += len;
    }
    (out.len() >= 2).then_some(out)
}

/// Как русскоязычный читает английское слово, наоборот: «дж» → j, «кт» → ct,
/// «кс» → x («проджект» → «project», «экспорт» → «export»).
fn english_translit(word: &str) -> String {
    let replaced = word
        .replace("дж", "j")
        .replace("кс", "x")
        .replace("кт", "ct")
        .replace("ай", "i")
        .replace("ей", "ey");
    translit(&replaced)
}

fn latin_letter(word: &str) -> Option<&'static str> {
    Some(match word {
        "эй" => "a",
        "би" => "b",
        "си" => "c",
        "ди" => "d",
        "эф" => "f",
        "джи" => "g",
        "эйч" => "h",
        "ай" => "i",
        "джей" => "j",
        "кей" => "k",
        "эл" => "l",
        "эм" => "m",
        "эн" => "n",
        "оу" => "o",
        "пи" => "p",
        "кью" => "q",
        "ар" => "r",
        "эс" => "s",
        "ти" => "t",
        "ю" => "u",
        "ви" => "v",
        "экс" => "x",
        "уай" => "y",
        "зед" | "зи" => "z",
        _ => return None,
    })
}

fn translit(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        let mapped = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' | 'э' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' | 'й' | 'ы' => "i",
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
            'щ' => "sch",
            'ю' => "yu",
            'я' => "ya",
            _ => "",
        };
        if !mapped.is_empty() {
            out.push_str(mapped);
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------- ранжирование

/// Только буквы и цифры в нижнем регистре: «-PROJECT.GST-» → «projectgst».
pub fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'ё' { 'е' } else { c })
        .collect()
}

/// Насколько имя похоже на запрос (0..1): совпало целиком, начинается, содержит, похоже.
pub fn name_score(name: &str, query: &str) -> f64 {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let (name, stem, query) = (squash(name), squash(&stem), squash(query));
    if query.is_empty() || name.is_empty() {
        return 0.0;
    }
    if name == query || stem == query {
        1.0
    } else if stem.starts_with(&query) || name.starts_with(&query) {
        0.9 - 0.1 * (1.0 - query.len() as f64 / stem.len().max(1) as f64)
    } else if name.contains(&query) {
        0.7
    } else {
        0.6 * jaro_winkler(&query, &stem)
    }
}

/// Шум, который почти никогда не ищут голосом: системные и служебные каталоги, сборка.
fn noise_penalty(path: &str) -> f64 {
    let lower = path.to_lowercase();
    const NOISE: &[&str] = &[
        "\\appdata\\",
        "\\windows\\",
        "\\$recycle.bin",
        "\\node_modules\\",
        "\\.git\\",
        "\\target\\",
        "\\obj\\",
        "\\bin\\debug",
        "\\bin\\release",
        "\\programdata\\",
        "\\temp\\",
        "\\.cache\\",
    ];
    // Установленные программы и ресурсы игр голосом ищут редко, но бывает.
    const SOFT_NOISE: &[&str] = &["\\program files", "\\steamapps\\", "\\.vs\\", "\\.gemini\\"];
    if NOISE.iter().any(|part| lower.contains(part)) {
        0.35
    } else if SOFT_NOISE.iter().any(|part| lower.contains(part)) {
        0.2
    } else {
        0.0
    }
}

// ---------------------------------------------------------------- частота открытий

/// История открытий через Reever: путь → (сколько раз, когда последний раз).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub items: HashMap<String, UsageEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageEntry {
    pub count: u32,
    pub last: u64,
}

impl Usage {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Отметить открытие и сохранить (старые редкие записи вычищаются).
    pub fn record(path: &Path, opened: &str) {
        let mut usage = Self::load(path);
        let entry = usage.items.entry(opened.to_lowercase()).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.last = now_secs();
        if usage.items.len() > 2_000 {
            let mut entries: Vec<_> = usage.items.into_iter().collect();
            entries.sort_by_key(|(_, e)| std::cmp::Reverse((e.count, e.last)));
            entries.truncate(1_500);
            usage.items = entries.into_iter().collect();
        }
        // Через временный файл: обрыв посреди записи не портит историю.
        if let Ok(json) = serde_json::to_string(&usage) {
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, json).is_ok() {
                let _ = std::fs::rename(&temp, path);
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Недавние элементы Windows с кэшем на минуту: разбор сотен ярлыков на каждую команду
/// не нужен.
pub fn windows_recent() -> HashMap<String, u64> {
    static CACHE: std::sync::Mutex<Option<(u64, HashMap<String, u64>)>> =
        std::sync::Mutex::new(None);
    let now = now_secs();
    if let Ok(cache) = CACHE.lock() {
        if let Some((at, map)) = cache.as_ref() {
            if now.saturating_sub(*at) < RECENT_TTL_SECS {
                return map.clone();
            }
        }
    }
    let fresh = read_windows_recent();
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((now, fresh.clone()));
    }
    fresh
}

/// Не больше стольких ярлыков из «Недавних» (самые свежие обычно и нужны).
const RECENT_MAX: usize = 400;

fn read_windows_recent() -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return out;
    };
    let dir = PathBuf::from(appdata).join("Microsoft\\Windows\\Recent");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let now = SystemTime::now();
    for entry in entries.filter_map(Result::ok).take(RECENT_MAX * 2) {
        if out.len() >= RECENT_MAX {
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lnk") {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        let Ok(link) = lnk::ShellLink::open(&path) else {
            continue;
        };
        let target = link.link_info().as_ref().and_then(|info| {
            info.local_base_path_unicode()
                .clone()
                .or_else(|| info.local_base_path().clone())
        });
        if let Some(target) = target {
            let key = target.trim_end_matches('\\').to_lowercase();
            let slot = out.entry(key).or_insert(age);
            *slot = (*slot).min(age);
        }
    }
    out
}

/// Добавка к рангу за частоту и свежесть: история Reever и недавние Windows.
fn usage_boost(path: &str, usage: &Usage, recent: &HashMap<String, u64>) -> f64 {
    let key = path.trim_end_matches('\\').to_lowercase();
    let mut boost = 0.0;
    if let Some(entry) = usage.items.get(&key) {
        boost += 0.25 * ((entry.count as f64) + 1.0).ln();
        let days = now_secs().saturating_sub(entry.last) as f64 / 86_400.0;
        boost += 0.25 / (1.0 + days / 7.0);
    }
    if let Some(age) = recent.get(&key) {
        let days = *age as f64 / 86_400.0;
        boost += 0.35 / (1.0 + days / 3.0);
    }
    boost
}

// ---------------------------------------------------------------- поиск

/// Найти элементы: по запросу на каждый вариант, слить, ранжировать. Ошибка — нет связи
/// с Everything.
pub fn search(
    host: &str,
    port: u16,
    candidates: &[String],
    scope: FindScope,
    usage: &Usage,
    recent: &HashMap<String, u64>,
) -> Result<Vec<FindItem>, String> {
    let variants = query_variants(candidates);
    let prefix = match scope {
        FindScope::Any => "",
        FindScope::Folder => "folder: ",
        FindScope::File => "file: ",
    };
    let mut found: HashMap<String, FindItem> = HashMap::new();
    let mut last_error = None;
    let mut any_ok = false;
    // Общий срок: поиск идёт в аудио-потоке, медленный Everything не должен его держать.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(SEARCH_BUDGET_MS);
    let run = |found: &mut HashMap<String, FindItem>,
               any_ok: &mut bool,
               last_error: &mut Option<String>,
               query: &str,
               count: usize,
               min_similarity: f64| {
        if std::time::Instant::now() >= deadline && *any_ok {
            return;
        }
        match everything::search_items(host, port, query, count) {
            Ok(items) => {
                *any_ok = true;
                for (full, folder) in items {
                    let full = full.to_string_lossy().into_owned();
                    let name = Path::new(&full)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| full.clone());
                    let mut similarity = variants
                        .iter()
                        .map(|v| name_score(&name, v))
                        .fold(0.0, f64::max);
                    // Отрицательный порог — пословный запрос: все слова уже совпали.
                    if min_similarity < 0.0 {
                        similarity = similarity.max(ALL_WORDS_MATCHED);
                    } else if similarity < min_similarity {
                        continue;
                    }
                    let boost = usage_boost(&full, usage, recent);
                    let item = FindItem {
                        name,
                        path: full.clone(),
                        folder,
                        frequent: boost >= 0.15,
                        score: similarity + boost - noise_penalty(&full),
                    };
                    found
                        .entry(full.to_lowercase())
                        .and_modify(|existing| {
                            if item.score > existing.score {
                                *existing = item.clone();
                            }
                        })
                        .or_insert(item);
                }
            }
            Err(error) => *last_error = Some(error),
        }
    };

    // Everything отдаёт первые N совпадений без учёта похожести, поэтому на каждый вариант
    // три запроса: имя целиком (`wfn:`), начало имени (`startwith:`) и вхождение слов.
    for variant in variants.iter().take(4) {
        let squashed = squash(variant);
        run(&mut found, &mut any_ok, &mut last_error, &format!("{prefix}wfn:{squashed}"), PER_QUERY, 0.0);
        run(&mut found, &mut any_ok, &mut last_error, &format!("{prefix}startwith:{squashed}"), PER_QUERY, 0.0);
        // Слова — раздельные условия (AND), пунктуация и пробелы в имени не мешают.
        run(&mut found, &mut any_ok, &mut last_error, &format!("{prefix}{variant}"), PER_QUERY, 0.0);
    }

    // Смешанные имена: пословный запрос «слово | латиница». Всё, что он нашёл, совпало по
    // всем словам — поэтому ранг не ниже 0,85.
    for candidate in candidates {
        if let Some(query) = mixed_query(candidate) {
            run(&mut found, &mut any_ok, &mut last_error, &format!("{prefix}{query}"), PER_QUERY, -1.0);
        }
    }

    // Мало хороших совпадений — нечёткий проход: имена с теми же первыми буквами, отбор по
    // сходству строк («меморволд» → Memory_Vault, «билингол» → bilingual). Everything ищет
    // только подстроки, поэтому похожесть считаем сами.
    let good = found.values().filter(|item| item.score >= 0.7).count();
    if good < 3 {
        for variant in variants.iter().take(4) {
            let squashed = squash(variant);
            let len = squashed.chars().count();
            if len < 4 {
                continue;
            }
            let head: String = squashed.chars().take(if len >= 6 { 4 } else { 3 }).collect();
            run(
                &mut found,
                &mut any_ok,
                &mut last_error,
                &format!("{prefix}startwith:{head}"),
                FUZZY_PER_QUERY,
                FUZZY_MIN,
            );
        }
    }

    if !any_ok {
        return Err(last_error.unwrap_or_else(|| "нет запроса".into()));
    }
    let mut items: Vec<FindItem> = found.into_values().collect();
    // Частые — первым разделом, внутри каждого — по рангу; при равенстве короче путь.
    items.sort_by(|a, b| {
        b.frequent
            .cmp(&a.frequent)
            .then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.path.len().cmp(&b.path.len()))
    });
    items.truncate(MAX_SHOWN);
    Ok(items)
}

// ---------------------------------------------------------------- выбор голосом

/// Номер варианта из сказанного: «третий», «номер 3», «3», «три» → Some(3).
pub fn parse_choice(text: &str) -> Option<usize> {
    const ORDINALS: &[(&str, usize)] = &[
        ("перв", 1),
        ("втор", 2),
        ("трет", 3),
        ("четверт", 4),
        ("пят", 5),
        ("шест", 6),
        ("седьм", 7),
        ("восьм", 8),
        ("девят", 9),
        ("десят", 10),
        ("first", 1),
        ("second", 2),
        ("third", 3),
        ("fourth", 4),
        ("fifth", 5),
    ];
    let normalized = crate::textnorm::normalize(text);
    for word in normalized.split_whitespace() {
        if let Ok(number) = word.parse::<usize>() {
            if (1..=MAX_SHOWN).contains(&number) {
                return Some(number);
            }
        }
        if let Some((_, number)) = ORDINALS.iter().find(|(stem, _)| word.starts_with(stem)) {
            return Some(*number);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_and_case_do_not_matter() {
        assert_eq!(squash("-PROJECT.GST-"), "projectgst");
        assert_eq!(name_score("-PROJECT.GST-", "project gst"), 1.0);
        assert!(name_score("ProjectGST.sln", "project gst") > 0.95);
        assert!(name_score("projects", "project gst") < 0.7);
    }

    #[test]
    fn russian_letter_names_and_translit_become_latin() {
        let variants = query_variants(&["проджект джи эс ти".into()]);
        assert!(variants.contains(&"project gst".to_string()), "{variants:?}");
        assert!(variants.contains(&"prodzhekt gst".to_string()), "{variants:?}");
        let glued = query_variants(&["проджект джиэсти".into()]);
        assert!(glued.contains(&"project gst".to_string()), "{glued:?}");
        assert_eq!(query_variants(&["Загрузки".into(), "downloads".into()]), [
            "загрузки",
            "zagruzki",
            "downloads"
        ]);
        // однобуквенные слова не разбираются как буквы: «и» остаётся словом
        assert!(spelled_letters("пиво").is_none());
    }

    #[test]
    fn spoken_punctuation_is_dropped_and_near_names_score() {
        let variants = query_variants(&["проджект точка джиэсти".into()]);
        assert!(variants.contains(&"project gst".to_string()), "{variants:?}");
        assert!(name_score("Memory_Vault", "memorvold") >= FUZZY_MIN, "{}", name_score("Memory_Vault", "memorvold"));
        assert!(name_score("bilingual.md", "bilingol") >= FUZZY_MIN);
        assert!(name_score("Downloads", "memorvold") < FUZZY_MIN);
    }

    #[test]
    fn mixed_names_get_per_word_alternatives() {
        assert_eq!(
            mixed_query("деловые партнеры фотошоп").as_deref(),
            Some("делов|delovie партне|partneri фотош|fotoshop|photoshop")
        );
        assert_eq!(
            mixed_query("нет длительности точка эм ди").as_deref(),
            Some("нет|net длительнос|dlitelnosti md")
        );
        assert_eq!(mixed_query("project gst"), None);
        assert_eq!(mixed_query("загрузки"), None);
    }

    #[test]
    fn spoken_choice_numbers() {
        assert_eq!(parse_choice("третий"), Some(3));
        assert_eq!(parse_choice("номер два"), Some(2));
        assert_eq!(parse_choice("открой первый"), Some(1));
        assert_eq!(parse_choice("5"), Some(5));
        assert_eq!(parse_choice("second one"), Some(2));
        assert_eq!(parse_choice("отмена"), None);
    }
}
