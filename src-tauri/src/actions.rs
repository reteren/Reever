use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use strsim::jaro_winkler;

use crate::commands::ReeverCommand;
use crate::textnorm::{comparison_forms, normalize};

/// Одно пользовательское действие (программа/папка-шорткат), которое юзер
/// определяет сам и калибрует голосом.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// "shell" — ShellExecute по target (exe/.lnk/.url/протокол вроде steam://…)
    /// "folder" — открыть папку target в проводнике
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub target: String,
    /// (устар.) На какие глаголы реагирует — больше не используется матчингом.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// Триггер-фразы: то, что записала калибровка (RU+EN транскрипты) + ручные.
    #[serde(default)]
    pub phrases: Vec<String>,
}

fn default_kind() -> String {
    "shell".into()
}

impl Action {
    /// Лучшая похожесть фраз действия на любого из кандидатов (0..1).
    pub fn score(&self, candidates: &[String]) -> f64 {
        let active = crate::grammar::active_languages();
        let mut best = 0.0_f64;
        for p in self.phrases.iter().filter(|p| self.phrase_active(p, active.as_deref())) {
            let phrase_forms = scoring_forms(p);
            for c in candidates {
                let candidate_forms = scoring_forms(c);
                best = best.max(pair_score(&phrase_forms, &candidate_forms));
            }
        }
        best
    }

    /// Сравнивается ли фраза: своя фраза пользователя — всегда, встроенная фраза встроенного
    /// действия или сайта — пока её язык распознаётся (`active`; `None` — все языки).
    pub fn phrase_active(&self, phrase: &str, active: Option<&[String]>) -> bool {
        let Some(active) = active else {
            return true;
        };
        builtin_phrase_languages(&self.id, phrase)
            .is_none_or(|languages| languages.iter().any(|language| active.iter().any(|a| a == language)))
    }

    /// Встроенные фразы этого действия на языках, которые сейчас не распознаются.
    pub fn inactive_phrases(&self) -> Vec<String> {
        let active = crate::grammar::active_languages();
        self.phrases
            .iter()
            .filter(|phrase| !self.phrase_active(phrase, active.as_deref()))
            .cloned()
            .collect()
    }
}

/// Языки встроенной фразы встроенного действия (`None` — фраза не встроенная, а своя).
/// Русские и английские фразы действий и сайтов размечаются по письму: кириллица — русский.
fn builtin_phrase_languages(id: &str, phrase: &str) -> Option<&'static [&'static str]> {
    use std::collections::HashMap;
    static BY_PHRASE: std::sync::OnceLock<HashMap<(String, String), Vec<&'static str>>> =
        std::sync::OnceLock::new();
    let map = BY_PHRASE.get_or_init(|| {
        let mut map: HashMap<(String, String), Vec<&'static str>> = HashMap::new();
        let mut add = |id: &str, phrase: &str, language: &'static str| {
            let languages = map.entry((id.to_string(), normalize(phrase))).or_default();
            if !languages.contains(&language) {
                languages.push(language);
            }
        };
        for action in default_actions().into_iter().chain(default_sites()) {
            for phrase in &action.phrases {
                let cyrillic = phrase.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
                add(&action.id, phrase, if cyrillic { "ru" } else { "en" });
            }
        }
        for (id, phrases) in crate::grammar::ENGLISH_SYSTEM_PHRASES {
            for phrase in *phrases {
                add(id, phrase, "en");
            }
        }
        for (id, phrases) in crate::grammar::LANGUAGE_SYSTEM_PHRASES {
            for (languages, phrase) in *phrases {
                for language in languages.split(' ') {
                    add(id, phrase, language);
                }
            }
        }
        map
    });
    map.get(&(id.to_string(), normalize(phrase))).map(Vec::as_slice)
}

/// Добавляет цель, если в фразе явно произнесён глагол команды. Так score работает и
/// при полном транскрипте, и после того, как dispatch уже снял глагол.
fn scoring_forms(text: &str) -> Vec<String> {
    let mut forms = comparison_forms(text);
    if let Some(command) = crate::commands::parse(text) {
        let target = match command {
            ReeverCommand::Open(target) | ReeverCommand::Launch(target) => target,
        };
        forms.extend(comparison_forms(&target));
        forms.sort();
        forms.dedup();
    }
    forms
}

/// Похожесть слова на слово, ниже которой слова считаются разными.
const WORD_MIN_SIMILARITY: f64 = 0.82;
/// Склейку сравниваем только у строк близкой длины: это те же звуки, разбитые на слова
/// иначе («маркноут» / «марк ноут»), а не короткое слово в начале длинного («плей» / «плейлист»).
const GLUED_MIN_LENGTH_RATIO: f64 = 0.75;
/// Слово с словом — тоже только близкой длины: Jaro-Winkler у короткого слова почти целиком
/// входящего в длинное высокий — «час» (фраза калибровки «chess») ~ «чанский» 0,85, и
/// бред открывал chess.com (живой журнал 2026-10-23).
const WORD_MIN_LENGTH_RATIO: f64 = 0.6;

/// Jaro по строке целиком здесь нет намеренно: он засчитывает общие буквы поперёк границ
/// слов, и бред проходил порог — «отмена» ~ «дота» 0.75, «at me no» ~ «shut down
/// computer» 0.75 (живой журнал 2026-10-16). Каждое слово должно быть похоже на слово.
/// Пробовали оставлять его при хотя бы одном совпавшем слове — опорой становятся
/// короткие и служебные слова («no» ~ «note», «that»), и бред снова запускает действия.
fn pair_score(left: &[String], right: &[String]) -> f64 {
    let mut best = 0.0_f64;
    for a in left {
        for b in right {
            best = best.max(word_overlap_score(a, b));
            best = best.max(glued_score(a, b));
        }
    }
    best
}

/// Фразы без пробелов: GigaAM и калибровка делят на слова одни и те же звуки по-разному.
fn glued_score(left: &str, right: &str) -> f64 {
    let left: String = left.split_whitespace().collect();
    let right: String = right.split_whitespace().collect();
    let (shorter, longer) = {
        let (l, r) = (left.chars().count(), right.chars().count());
        (l.min(r), l.max(r))
    };
    if shorter == 0 || (shorter as f64) < longer as f64 * GLUED_MIN_LENGTH_RATIO {
        return 0.0;
    }
    // Склейка — те же звуки, разбитые иначе, значит и начало то же. Без этого Jaro
    // засчитывал общие буквы вразнобой: «поиск» ~ «спотик» (фраза калибровки Spotify
    // «с по тик») 0,82 — и «ривер поиск» запускал Spotify (живой журнал 2026-10-23).
    if left.chars().next() != right.chars().next() {
        return 0.0;
    }
    let similarity = jaro_winkler(&left, &right);
    if similarity >= WORD_MIN_SIMILARITY {
        similarity
    } else {
        0.0
    }
}

/// Похожесть по словам: каждое слово совпадает не более чем с одним словом второй фразы
/// и приносит свою похожесть, поэтому общий короткий фрагмент не даёт score=1, а лишние
/// слова с любой стороны снижают оценку.
fn word_overlap_score(left: &str, right: &str) -> f64 {
    let left_words: Vec<&str> = left.split_whitespace().collect();
    let right_words: Vec<&str> = right.split_whitespace().collect();
    if left_words.is_empty() || right_words.is_empty() {
        return 0.0;
    }

    let mut used = vec![false; right_words.len()];
    let mut matched = 0.0_f64;
    for word in &left_words {
        let best = right_words
            .iter()
            .enumerate()
            .filter(|(index, other)| !used[*index] && similar_length(word, other))
            .map(|(index, other)| (index, jaro_winkler(word, other)))
            .filter(|(_, similarity)| *similarity >= WORD_MIN_SIMILARITY)
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((index, similarity)) = best {
            used[index] = true;
            matched += similarity;
        }
    }
    2.0 * matched / (left_words.len() + right_words.len()) as f64
}

fn similar_length(left: &str, right: &str) -> bool {
    let (l, r) = (left.chars().count(), right.chars().count());
    l.min(r) as f64 >= l.max(r) as f64 * WORD_MIN_LENGTH_RATIO
}

pub fn load_actions(path: &Path) -> Vec<Action> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_actions(path: &Path, actions: &[Action]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(actions).unwrap_or_else(|_| "[]".to_string());
    std::fs::write(path, json)
}

fn act(id: &str, label: &str, kind: &str, target: &str, phrases: &[&str]) -> Action {
    Action {
        id: id.into(),
        label: label.into(),
        kind: kind.into(),
        target: target.into(),
        verbs: Vec::new(),
        phrases: phrases.iter().map(|s| s.to_string()).collect(),
    }
}

/// Дефолтные системные действия. Поиск хранится в grammar.json.
fn default_actions() -> Vec<Action> {
    vec![
        act(
            "sys_shutdown",
            "Выключить ПК",
            "system",
            "shutdown",
            &[
                "выключи компьютер",
                "выключи комп",
                "выключи пк",
                "оффни пк",
                "оффни комп",
                "shut down computer",
                "turn off computer",
            ],
        ),
        act(
            "sys_restart",
            "Перезагрузка",
            "system",
            "restart",
            &[
                "перезагрузи компьютер",
                "перезагрузи пк",
                "ребутни пк",
                "рестарт",
                "restart computer",
                "reboot computer",
            ],
        ),
        act(
            "sys_mute",
            "Без звука",
            "system",
            "mute",
            &["выключи звук", "без звука", "мьют", "mute sound"],
        ),
        act(
            "sys_play",
            "Пауза/Плей",
            "system",
            "media_play_pause",
            &["пауза", "плей", "останови музыку", "продолжи музыку", "pause music", "play music"],
        ),
        act(
            "sys_next",
            "Следующий трек",
            "system",
            "media_next",
            &["следующий трек", "переключи трек", "дальше трек", "next track"],
        ),
        act(
            "sys_prev",
            "Предыдущий трек",
            "system",
            "media_prev",
            &["предыдущий трек", "назад трек", "previous track"],
        ),
    ]
}

/// Сайты по умолчанию (одобрены пользователем 2026-11-01). Программы человек добавляет сам,
/// а сайты одинаковы у всех — их Reever знает сразу.
fn default_sites() -> Vec<Action> {
    vec![
        act("site_twitter", "X (Twitter)", "url", "https://x.com", &["твиттер", "твитер", "икс твиттер", "twitter"]),
        act("site_reddit", "Reddit", "url", "https://www.reddit.com", &["реддит", "редит", "reddit"]),
        act("site_twitch", "Twitch", "url", "https://www.twitch.tv", &["твич", "twitch"]),
        act("site_netflix", "Netflix", "url", "https://www.netflix.com", &["нетфликс", "netflix"]),
        act("site_outlook", "Outlook", "url", "https://outlook.live.com/mail/", &["аутлук", "outlook"]),
        act("site_drive", "Google Drive", "url", "https://drive.google.com", &["гугл диск", "google drive"]),
        act("site_docs", "Google Docs", "url", "https://docs.google.com/document/", &["гугл документы", "гугл доки", "google docs"]),
        act("site_sheets", "Google Sheets", "url", "https://docs.google.com/spreadsheets/", &["гугл таблицы", "таблицы", "google sheets"]),
        act("site_calendar", "Google Calendar", "url", "https://calendar.google.com", &["календарь", "гугл календарь", "google calendar", "calendar"]),
        act("site_translate", "Google Translate", "url", "https://translate.google.com", &["переводчик", "гугл переводчик", "google translate", "translator"]),
        act("site_github", "GitHub", "url", "https://github.com", &["гитхаб", "гит хаб", "github"]),
        act("site_maps", "Google Maps", "url", "https://www.google.com/maps", &["карты", "гугл карты", "google maps", "maps"]),
        act("site_wikipedia", "Wikipedia", "url", "https://www.wikipedia.org", &["википедия", "wikipedia"]),
        act("site_aliexpress", "AliExpress", "url", "https://www.aliexpress.com", &["алиэкспресс", "али экспресс", "aliexpress"]),
        act("site_pinterest", "Pinterest", "url", "https://www.pinterest.com", &["пинтерест", "pinterest"]),
        act("site_chess", "Chess.com", "url", "https://www.chess.com", &["шахматы", "chess"]),
        act("site_temu", "Temu", "url", "https://www.temu.com", &["тему", "тэму", "temu"]),
    ]
}

/// Хост адреса без `www.` — сайт, который у человека уже есть, второй раз не добавляется.
fn url_host(target: &str) -> Option<String> {
    let rest = target.trim().split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    (!host.is_empty()).then_some(host)
}

/// Сайты по умолчанию добавляются по одному разу: какие уже предлагались, помнит `seen_path`
/// (рядом с actions.json). Удалённый человеком сайт после перезапуска не возвращается, а сайт
/// из новой версии появится и у того, кто ставит обновление поверх. Сайт с тем же адресом
/// (тот же хост и путь), что уже есть в действиях, не дублируется.
fn seed_default_sites(actions: &mut Vec<Action>, seen_path: &Path) -> bool {
    let mut seen: Vec<String> = std::fs::read_to_string(seen_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let key = |target: &str| {
        url_host(target).map(|host| {
            let path = target.split_once("://").map_or("", |(_, rest)| rest);
            let path = path.split_once('/').map_or("", |(_, path)| path).trim_end_matches('/');
            format!("{host}/{path}")
        })
    };
    let mut added = false;
    let mut seen_changed = false;
    for site in default_sites() {
        if seen.contains(&site.id) {
            continue;
        }
        seen.push(site.id.clone());
        seen_changed = true;
        let site_key = key(&site.target);
        let exists = actions
            .iter()
            .any(|action| action.id == site.id || (site_key.is_some() && key(&action.target) == site_key));
        if !exists {
            actions.push(site);
            added = true;
        }
    }
    if seen_changed {
        if let Ok(json) = serde_json::to_string_pretty(&seen) {
            if let Err(e) = std::fs::write(seen_path, json) {
                eprintln!("[reever] не удалось сохранить список сайтов по умолчанию: {e}");
            }
        }
    }
    added
}

/// Файл со списком уже предложенных сайтов по умолчанию — рядом с actions.json.
fn seen_sites_path(actions_path: &Path) -> PathBuf {
    actions_path.with_extension("sites.json")
}

/// Добавить дефолтные действия, которых ещё нет (по id). Возвращает true если добавили.
fn merge_defaults(actions: &mut Vec<Action>) -> bool {
    let mut added = false;
    for d in default_actions() {
        if !actions.iter().any(|a| a.id == d.id) {
            actions.push(d);
            added = true;
        }
    }
    added
}

/// Снести устаревшие дефолтные команды, которые мы убрали из набора.
fn remove_obsolete(actions: &mut Vec<Action>) -> bool {
    const OBSOLETE: [&str; 4] = ["sys_vol_up", "sys_vol_down", "sys_lock", "sys_sleep"];
    let before = actions.len();
    actions.retain(|a| !OBSOLETE.contains(&a.id.as_str()));
    actions.len() != before
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{word_overlap_score, Action, SharedState};
    use crate::config::Settings;

    fn actions() -> Vec<Action> {
        serde_json::from_str(include_str!("../resources/actions.json"))
            .expect("valid read-only fixture")
    }

    fn score(id: &str, transcript: &str) -> f64 {
        let action = actions()
            .into_iter()
            .find(|action| action.id == id)
            .expect("action in actions.json");
        action.score(&[transcript.to_string()])
    }

    #[test]
    fn real_vosk_phrases_match_gigaam_transcripts() {
        assert!(score("act_mqlp3ixy", "дота 2") >= 0.7);
        assert!(score("act_mqlp3ixy", "дота два") >= 0.7);
        assert!(score("act_mqlp3ixy", "daughter") >= 0.7);
        assert!(score("act_mqlpc0cc", "фл студио") >= 0.7);
        assert!(score("act_mqlpc0cc", "эф эл студио") >= 0.7);
        assert!(score("act_mqlpel49", "фотошоп") >= 0.7);
        assert!(score("act_mqlpwj9y", "дискорд") >= 0.7);
    }

    #[test]
    fn action_score_accepts_polite_and_reverse_command_phrases() {
        assert!(score("act_mqlpel49", "запусти пожалуйста фотошоп") >= 0.7);
        assert!(score("act_mqlpel49", "фотошоп открой") >= 0.7);
    }

    #[test]
    fn word_overlap_contributes_without_making_one_shared_word_a_full_match() {
        assert!(score("act_mqlpc0cc", "студио файл") >= 0.7);
        assert!(word_overlap_score("файл студио", "студио") < 0.7);
    }

    #[test]
    fn a_short_phrase_does_not_match_a_long_word_it_resembles() {
        // Живой журнал 2026-10-23: фраза калибровки «час» (chess) и «чанский» — 0,85.
        let chess = Action {
            id: "chess".into(),
            label: "chess".into(),
            kind: "url".into(),
            target: "https://www.chess.com".into(),
            verbs: Vec::new(),
            phrases: vec!["шахматы".into(), "час".into(), "chess".into()],
        };
        assert!(chess.score(&["чанский".into()]) < 0.7);
        let spotify = Action {
            phrases: vec!["спати фай".into(), "spotify".into(), "с по тик".into()],
            ..chess.clone()
        };
        assert!(spotify.score(&["поиск".into()]) < 0.7);
        assert!(spotify.score(&["спотифай".into()]) >= 0.7);
        assert!(chess.score(&["шахматы".into()]) >= 0.9);
        assert!(chess.score(&["шахмат".into()]) >= 0.7);
    }

    #[test]
    fn letters_shared_across_word_boundaries_do_not_match() {
        // Живой журнал 2026-10-16: английская модель на «ривер отмена».
        assert!(score("act_mqlp3ixy", "отмена") < 0.7);
        let shutdown = super::default_actions()
            .into_iter()
            .find(|action| action.id == "sys_shutdown")
            .unwrap();
        for heard in ["at me no", "let me or no", "at me a no"] {
            assert!(shutdown.score(&[heard.to_string()]) < 0.7, "{heard}");
        }
    }

    #[test]
    fn builtin_phrases_of_disabled_languages_are_hidden_and_not_matched() {
        let mut restart = super::default_actions()
            .into_iter()
            .find(|action| action.id == "sys_restart")
            .unwrap();
        crate::grammar::append_language_system_phrases_for_test(std::slice::from_mut(&mut restart));
        restart.phrases.push("моя фраза".into());
        let english = vec!["en".to_string()];
        let russian = vec!["en".to_string(), "ru".to_string()];
        let slovak = vec!["en".to_string(), "sk".to_string()];
        assert!(restart.phrase_active("restart computer", Some(&english)));
        assert!(!restart.phrase_active("перезагрузи пк", Some(&english)));
        assert!(restart.phrase_active("перезагрузи пк", Some(&russian)));
        assert!(!restart.phrase_active("redémarre l'ordinateur", Some(&russian)));
        assert!(!restart.phrase_active("перезавантаж комп'ютер", Some(&russian)));
        assert!(restart.phrase_active("reštartuj počítač", Some(&slovak)));
        assert!(restart.phrase_active("моя фраза", Some(&english)), "своя фраза видна всегда");
        assert!(restart.phrase_active("redémarre l'ordinateur", None));
        let shutdown = super::act("sys_shutdown", "", "system", "", &["vypni počítač"]);
        assert!(shutdown.phrase_active("vypni počítač", Some(&slovak)), "общая фраза чешского и словацкого");
        let chess = super::default_sites().into_iter().find(|site| site.id == "site_chess").unwrap();
        assert!(!chess.phrase_active("шахматы", Some(&english)));
        assert!(chess.phrase_active("chess", Some(&english)));
        // та же фраза у своего действия человека — его собственная
        let own = super::act("act_x", "", "url", "", &["шахматы"]);
        assert!(own.phrase_active("шахматы", Some(&english)));
    }

    #[test]
    fn same_sounds_split_into_words_differently_still_match() {
        let marknote = super::act("x", "MarkNote", "shell", "", &["марк ноут"]);
        assert!(marknote.score(&["маркноут".to_string()]) >= 0.7);
        let playlist = super::act("y", "Playlist", "shell", "", &["плей лист"]);
        assert!(playlist.score(&["плей".to_string()]) < 0.7);
    }

    #[test]
    fn default_sites_are_offered_once_and_do_not_duplicate_existing_ones() {
        let seen = std::env::temp_dir().join(format!("reever-sites-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&seen);
        let mut actions = vec![super::act("mine", "YT", "url", "https://chess.com/", &["шахматы"])];
        assert!(super::seed_default_sites(&mut actions, &seen));
        let ids: Vec<_> = actions.iter().map(|action| action.id.as_str()).collect();
        assert!(ids.contains(&"site_github") && ids.contains(&"site_temu"));
        // chess.com уже был у человека — второй не появился
        assert!(!ids.contains(&"site_chess"));
        // удалённый сайт после перезапуска не возвращается
        actions.retain(|action| action.id != "site_github");
        assert!(!super::seed_default_sites(&mut actions, &seen));
        assert!(!actions.iter().any(|action| action.id == "site_github"));
        let _ = std::fs::remove_file(&seen);
    }

    #[test]
    fn default_sites_answer_their_names() {
        let sites = super::default_sites();
        let site = |id: &str| sites.iter().find(|site| site.id == id).unwrap();
        assert!(site("site_pinterest").score(&["пинтерест".into()]) >= 0.9);
        assert!(site("site_sheets").score(&["гугл таблицы".into()]) >= 0.9);
        assert!(site("site_maps").score(&["карты".into()]) >= 0.9);
        assert!(site("site_drive").score(&["гугл документы".into()]) < 0.9);
        assert!(site("site_chess").score(&["шахматы".into()]) >= 0.9);
    }

    #[test]
    fn search_actions_are_not_merged_as_defaults() {
        let mut actions = Vec::new();
        assert!(super::merge_defaults(&mut actions));
        assert!(actions.iter().all(|action| action.kind != "search"));
        assert!(!actions
            .iter()
            .any(|action| { action.id == "web_google" || action.id == "web_youtube" }));
    }

    #[test]
    fn shared_state_creates_persists_and_versions_grammar() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "reever-actions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir();
        let actions_path = root.join(format!("{name}-actions.json"));
        let grammar_path = root.join(format!("{name}-grammar.json"));

        let state = SharedState::new(
            actions_path.clone(),
            grammar_path.clone(),
            &Settings::default(),
        );
        assert!(grammar_path.exists());
        state.bump_grammar();
        assert_eq!(state.grammar_version.load(Ordering::SeqCst), 1);

        let expected = {
            let mut grammar = state.grammar.lock().unwrap();
            grammar.wake_words[0] = "новое слово".into();
            grammar.clone()
        };
        state.persist_grammar().unwrap();
        let persisted: crate::grammar::Grammar =
            serde_json::from_str(&std::fs::read_to_string(&grammar_path).unwrap()).unwrap();
        assert_eq!(persisted, expected);

        drop(state);
        let _ = std::fs::remove_file(super::seen_sites_path(&actions_path));
        let _ = std::fs::remove_file(actions_path);
        let _ = std::fs::remove_file(grammar_path);
    }
}

/// Состояние, разделяемое между Tauri-командами (UI) и аудио-воркером.
pub struct SharedState {
    pub actions: Mutex<Vec<Action>>,
    /// Если Some(id) — воркер в режиме калибровки: следующую фразу пишет в это действие.
    pub calibrate: Mutex<Option<String>>,
    /// Запись «ривер» для подстройки под голос (voice_adapt.rs): пока Some — звук копится
    /// здесь, а не идёт в распознавание. Только в памяти.
    pub voice_record: Mutex<Option<Vec<f32>>>,
    pub actions_path: PathBuf,
    /// Пауза микрофона (из трея / mute-хоткея). При паузе поток cpal закрывается.
    pub paused: AtomicBool,
    /// Триггер прослушки по хоткею (push-to-talk).
    pub force_listen: AtomicBool,
    /// Настройки сохранены: поток распознавания перезапускается с новыми.
    pub reload: AtomicBool,
    /// Текущий push-to-talk хоткей (источник правды для пере-регистрации).
    pub ptt_hotkey: Mutex<String>,
    /// Текущий mute-хоткей (panic-mute). Пусто — выкл.
    pub mute_hotkey: Mutex<String>,
    /// Грамматика голосовых команд (редактор «Что можно сказать»).
    pub grammar: Mutex<crate::grammar::Grammar>,
    pub grammar_path: PathBuf,
    /// Растёт при каждом сохранении грамматики — аудио-поток сверяет и перечитывает.
    pub grammar_version: std::sync::atomic::AtomicU64,
    /// Варианты в окне выбора «ривер найди …» (пусто — окно закрыто).
    pub picker: Mutex<Vec<crate::find::FindItem>>,
    /// Номер текущего окна выбора: растёт при каждом показе.
    pub picker_session: std::sync::atomic::AtomicU64,
    /// Номер окна, закрытого из интерфейса (клик, Esc, Alt+F4) — аудио-поток сверяет его со
    /// своим и перестаёт ждать номер голосом. Так закрытие старого окна не обрывает новое.
    pub picker_closed_session: std::sync::atomic::AtomicU64,
    /// История открытий для ранжирования поиска (`usage.json` рядом с actions.json).
    pub usage_path: PathBuf,
}

impl SharedState {
    /// `grammar_path` — `grammar.json`; `settings` нужны только для переноса старых
    /// `wake_words`/`cancel_words` при первом создании грамматики.
    pub fn new(
        actions_path: PathBuf,
        grammar_path: PathBuf,
        settings: &crate::config::Settings,
    ) -> Self {
        let grammar_is_new = !grammar_path.exists();
        let mut actions = load_actions(&actions_path);
        let removed = remove_obsolete(&mut actions);
        let added = merge_defaults(&mut actions);
        let seeded = seed_default_sites(&mut actions, &seen_sites_path(&actions_path));
        let (grammar, migrated_actions) =
            crate::grammar::Grammar::load_or_migrate(&grammar_path, settings, &mut actions);
        let usage_path = actions_path.with_file_name("usage.json");
        let state = Self {
            usage_path,
            picker: Mutex::new(Vec::new()),
            picker_session: std::sync::atomic::AtomicU64::new(0),
            picker_closed_session: std::sync::atomic::AtomicU64::new(0),
            actions: Mutex::new(actions),
            calibrate: Mutex::new(None),
            voice_record: Mutex::new(None),
            actions_path,
            paused: AtomicBool::new(false),
            force_listen: AtomicBool::new(false),
            reload: AtomicBool::new(false),
            ptt_hotkey: Mutex::new(String::new()),
            mute_hotkey: Mutex::new(String::new()),
            grammar: Mutex::new(grammar),
            grammar_path,
            grammar_version: std::sync::atomic::AtomicU64::new(0),
        };
        if grammar_is_new {
            if let Err(e) = state.persist_grammar() {
                eprintln!("[reever] не удалось сохранить grammar.json: {e}");
            }
        }
        if removed || added || seeded || migrated_actions {
            state.persist();
        }
        state
    }

    /// Сохранить текущий реестр на диск (вызывать под локом снято).
    pub fn persist(&self) {
        if let Ok(actions) = self.actions.lock() {
            if let Err(e) = save_actions(&self.actions_path, &actions) {
                eprintln!("[reever] не удалось сохранить actions.json: {e}");
            }
        }
    }

    /// Сохраняет текущую грамматику в файл редактора.
    pub fn persist_grammar(&self) -> std::io::Result<()> {
        let grammar = self
            .grammar
            .lock()
            .map_err(|_| std::io::Error::other("блокировка грамматики повреждена"))?;
        grammar.save(&self.grammar_path)
    }

    /// Уведомляет аудио-поток, что грамматика изменилась.
    pub fn bump_grammar(&self) {
        self.grammar_version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
