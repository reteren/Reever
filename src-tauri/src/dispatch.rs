//! Диспетчер: что делать с уже распознанным текстом команды.
//! Приоритет `system → search → verbs` — контракт, а не деталь (см. `dispatch`).

use std::path::PathBuf;
use std::sync::Arc;

use strsim::jaro_winkler;
use tauri::AppHandle;

use crate::actions::{Action, SharedState};
use crate::commands::{parse, ReeverCommand};
use crate::config::ReeverConfig;
use crate::events::ui_log;
use crate::everything::{self, SearchOutcome};
use crate::textnorm::{comparison_forms, normalize, strip_leading_fillers};
use crate::{launcher, system, winutil};

pub enum DispatchResult {
    Done(bool),
    Confirm(String),
}

pub fn dispatch(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    ru_text: &str,
    en_text: Option<&str>,
) -> DispatchResult {
    // Явная команда «запусти/открой/найди X» — намерение запустить ДЕЙСТВИЕ, а не
    // систему. Системные матчим ТОЛЬКО без глагола-запуска (выключи/пауза/плей/…),
    // иначе «запусти дота два» ложно ловится фаззи на «плей»/«пауза».
    let explicit_launch = has_explicit_verb(ru_text);
    // 1) системные команды (только если нет глагола-запуска)
    if !explicit_launch {
        if let Some((action, score)) = match_kind(shared, cfg, "system", ru_text, en_text) {
            let op = action.target.clone();
            if system::is_dangerous(&op) && cfg.settings.confirm_dangerous {
                ui_log(
                    app,
                    format!("⚙ {} — подтверди голосом: «да» / «отмена»", action.label),
                );
                return DispatchResult::Confirm(op);
            }
            ui_log(
                app,
                format!("⚙ система: {} (score {score:.2})", action.label),
            );
            return DispatchResult::Done(system::run_system(&op));
        }
    }
    // 2) поиск/параметрические (префиксный матч)
    if let Some(done) = try_search(app, cfg, shared, ru_text) {
        return DispatchResult::Done(done);
    }
    // 3) глаголы (открой/запусти/найди)
    DispatchResult::Done(dispatch_verbs(app, cfg, shared, ru_text, en_text))
}

/// Поисковые/параметрические действия: фраза-триггер в начале, остаток = запрос.
fn try_search(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    ru_text: &str,
) -> Option<bool> {
    if cfg.settings.offline_mode {
        return None; // офлайн: веб-поиск отключён
    }
    let normalized = strip_leading_fillers(&normalize(ru_text));
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.len() < 2 {
        return None; // нужен триггер + хотя бы 1 слово запроса
    }
    let found: Option<(String, String)> = {
        let actions = shared.actions.lock().unwrap();
        let mut res = None;
        'outer: for a in actions.iter().filter(|a| a.kind == "search") {
            if let Some((_take, query)) = find_search_prefix(&normalized, &a.phrases) {
                if !query.is_empty() {
                    res = Some((a.target.replace("{q}", &urlencode(&query)), a.label.clone()));
                    break 'outer;
                }
            }
        }
        res
    };
    let (url, label) = found?;
    if cfg.settings.private_log {
        ui_log(app, format!("🔎 {label}"));
    } else {
        ui_log(app, format!("🔎 {label}: {url}"));
    }
    Some(launch_with_raise(cfg, &label, || {
        launcher::shell_open(&url)
    }))
}

/// Возвращает сколько слов занимает триггер и нормализованный остаток-запрос.
/// Склеивание форм позволяет сопоставить «за гугле» с «загугли» после пунктуации ASR.
fn find_search_prefix(text: &str, phrases: &[String]) -> Option<(usize, String)> {
    let normalized = strip_leading_fillers(&normalize(text));
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }

    for phrase in phrases {
        let trigger = normalize(phrase);
        if trigger.is_empty() {
            continue;
        }
        let trigger_words = trigger.split_whitespace().count().max(1);
        let max_take = (trigger_words + 1).min(words.len() - 1);
        for take in 1..=max_take {
            let prefix = words[..take].join(" ");
            if search_trigger_score(&prefix, &trigger) >= 0.84 {
                let query = words[take..].join(" ");
                if !query.is_empty() {
                    return Some((take, query));
                }
            }
        }
    }
    None
}

fn search_trigger_score(prefix: &str, trigger: &str) -> f64 {
    let prefix_forms = comparison_forms(prefix);
    let trigger_forms = comparison_forms(trigger);
    let mut best = 0.0_f64;
    for prefix in &prefix_forms {
        for trigger in &trigger_forms {
            best = best.max(jaro_winkler(prefix, trigger));
            best = best.max(jaro_winkler(
                &prefix.replace(' ', ""),
                &trigger.replace(' ', ""),
            ));
        }
    }
    best
}

fn has_explicit_verb(text: &str) -> bool {
    parse(text).is_some()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Матч по действиям заданного типа (full-text fuzzy) — для system.
fn match_kind(
    shared: &Arc<SharedState>,
    cfg: &ReeverConfig,
    kind: &str,
    ru_text: &str,
    en_text: Option<&str>,
) -> Option<(Action, f64)> {
    let mut candidates = vec![ru_text.to_lowercase()];
    if let Some(en) = en_text {
        candidates.push(en.to_lowercase());
    }
    let actions = shared.actions.lock().unwrap();
    actions
        .iter()
        .filter(|a| a.kind == kind)
        .map(|a| (a, a.score(&candidates)))
        .filter(|(_, s)| *s >= cfg.settings.match_threshold)
        .max_by(|(_, s1), (_, s2)| s1.partial_cmp(s2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(a, s)| (a.clone(), s))
}

fn dispatch_verbs(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    ru_text: &str,
    en_text: Option<&str>,
) -> bool {
    let spoken = match parse(ru_text) {
        Some(ReeverCommand::Launch(t)) | Some(ReeverCommand::Open(t)) => t,
        None => {
            ui_log(app, "⚠ не распознал команду");
            return false;
        }
    };
    let (folder_search, raw_target) = match strip_folder_word(&spoken) {
        Some(rest) => (true, rest),
        None => (false, spoken),
    };

    let parts = split_targets(&raw_target);
    let multi = parts.len() > 1;
    if multi {
        ui_log(app, format!("мульти: {parts:?}"));
    }
    let targets = if multi { parts } else { vec![raw_target] };

    let mut any = false;
    for t in &targets {
        let en = if multi { None } else { en_text };
        let ok = if folder_search {
            open_via_everything(app, cfg, t, en)
        } else {
            launch_intent(app, cfg, shared, t, en)
        };
        if ok {
            any = true;
        }
    }
    any
}

/// «открой папку X» -> Some("X"); иначе None.
fn strip_folder_word(t: &str) -> Option<String> {
    let mut it = t.split_whitespace();
    match it.next() {
        Some("папку") | Some("папка") | Some("папке") => {
            Some(it.collect::<Vec<_>>().join(" "))
        }
        _ => None,
    }
}

/// Разбить цель по «и»/«and»/«,» (мультизапуск). Не дробим, если часть короче
/// 2 символов — это обычно мисрекогн одного слова («с и фай» = спотифай).
fn split_targets(s: &str) -> Vec<String> {
    let parts: Vec<String> = s
        .replace(" и ", ",")
        .replace(" and ", ",")
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() > 1 && parts.iter().all(|p| p.chars().count() >= 2) {
        parts
    } else {
        vec![s.trim().to_string()]
    }
}

/// «открой X» / «запусти X» / «найди X» -> личные действия (любые, кроме system/search).
fn launch_intent(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    target: &str,
    en_text: Option<&str>,
) -> bool {
    if let Some((action, score)) = match_action(shared, cfg, target, en_text) {
        ui_log(
            app,
            format!("↪ действие «{}» (score {score:.2})", action.label),
        );
        return run_action(app, cfg, &action);
    }
    if cfg.settings.launch_everything_fallback && !cfg.settings.offline_mode {
        let q = cfg
            .aliases
            .app(target)
            .unwrap_or_else(|| target.to_string());
        if cfg.settings.private_log {
            ui_log(app, "🔎 (fallback) поиск программ…");
        } else {
            ui_log(app, format!("🔎 (fallback) поиск программ: «{q}»"));
        }
        let o = everything::find_executables(
            &cfg.settings.everything_host,
            cfg.settings.everything_port,
            &q,
        );
        handle_launch(app, cfg, o, &q)
    } else {
        ui_log(app, "∅ нет такого действия (добавь и откалибруй)");
        false
    }
}

/// Матч по личным действиям (исключая системные/поисковые).
fn match_action(
    shared: &Arc<SharedState>,
    cfg: &ReeverConfig,
    target: &str,
    en_text: Option<&str>,
) -> Option<(Action, f64)> {
    let mut candidates = vec![target.to_lowercase()];
    if let Some(en) = en_text {
        candidates.push(en.to_lowercase());
    }
    let actions = shared.actions.lock().unwrap();
    actions
        .iter()
        .filter(|a| a.kind != "system" && a.kind != "search")
        .map(|a| (a, a.score(&candidates)))
        .filter(|(_, s)| *s >= cfg.settings.match_threshold)
        .max_by(|(_, s1), (_, s2)| s1.partial_cmp(s2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(a, s)| (a.clone(), s))
}

fn run_action(app: &AppHandle, cfg: &ReeverConfig, action: &Action) -> bool {
    let target = winutil::expand_env(&action.target);
    let hint = if action.label.trim().is_empty() {
        target.clone()
    } else {
        action.label.clone()
    };
    match action.kind.as_str() {
        "folder" => {
            ui_log(app, format!("📁 открываю: {target}"));
            launch_with_raise(cfg, &hint, || {
                everything::open_folders(&[PathBuf::from(&target)]) > 0
            })
        }
        "uwp" => {
            ui_log(app, format!("▶ UWP: {target}"));
            launch_with_raise(cfg, &hint, || launcher::launch_uwp(&target))
        }
        _ => {
            ui_log(app, format!("▶ запуск: {target}"));
            launch_with_raise(cfg, &hint, || launcher::shell_open(&target))
        }
    }
}

fn en_target(en: &str) -> String {
    let words: Vec<&str> = en.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }
    let n = words.len().min(2);
    words[words.len() - n..].join(" ")
}

fn open_via_everything(
    app: &AppHandle,
    cfg: &ReeverConfig,
    ru_target: &str,
    en_text: Option<&str>,
) -> bool {
    if cfg.settings.offline_mode {
        ui_log(app, "🔒 офлайн-режим: поиск папок (Everything) отключён");
        return false;
    }
    let host = &cfg.settings.everything_host;
    let port = cfg.settings.everything_port;

    let q = cfg
        .aliases
        .folder(ru_target)
        .unwrap_or_else(|| ru_target.to_string());
    if cfg.settings.private_log {
        ui_log(app, "🔎 поиск папок…");
    } else {
        ui_log(app, format!("🔎 поиск папок: «{q}»"));
    }
    let mut outcome = everything::find_folders(host, port, &q);
    let mut probe = q;

    if outcome.error.is_none() && outcome.paths.is_empty() {
        if let Some(en) = en_text {
            let en_q = en_target(en);
            if !en_q.is_empty() {
                if !cfg.settings.private_log {
                    ui_log(app, format!("🔎 (EN) поиск папок: «{en_q}»"));
                }
                outcome = everything::find_folders(host, port, &en_q);
                probe = en_q;
            }
        }
    }
    handle_open(app, cfg, outcome, &probe)
}

fn handle_open(app: &AppHandle, cfg: &ReeverConfig, o: SearchOutcome, probe: &str) -> bool {
    if let Some(e) = o.error {
        ui_log(app, format!("⚠ {e}"));
        return false;
    }
    if o.paths.is_empty() {
        ui_log(app, "∅ папки не найдены");
        return false;
    }
    let best = o.paths[0].clone();
    let base = best
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let sim = jaro_winkler(&probe.to_lowercase(), &base);
    if sim < cfg.settings.open_min_similarity {
        ui_log(
            app,
            format!(
                "∅ не уверен ({sim:.2} < {:.2}) — пропускаю «{}»",
                cfg.settings.open_min_similarity,
                best.display()
            ),
        );
        return false;
    }

    ui_log(app, format!("📁 {}", best.display()));
    if o.paths.len() > 1 {
        ui_log(
            app,
            format!("(+{} др. — открываю лучшее)", o.paths.len() - 1),
        );
    }
    let hint = base;
    launch_with_raise(cfg, &hint, || {
        everything::open_folders(std::slice::from_ref(&best)) > 0
    })
}

fn handle_launch(app: &AppHandle, cfg: &ReeverConfig, o: SearchOutcome, hint: &str) -> bool {
    if let Some(e) = o.error {
        ui_log(app, format!("⚠ {e}"));
        return false;
    }
    if o.paths.is_empty() {
        ui_log(app, "∅ программа не найдена");
        return false;
    }
    let target = o.paths[0].clone();
    ui_log(app, format!("▶ запуск: {}", target.display()));
    let h = hint.to_string();
    launch_with_raise(cfg, &h, || launcher::launch(&target))
}

fn launch_with_raise(cfg: &ReeverConfig, hint: &str, f: impl FnOnce() -> bool) -> bool {
    let before = if cfg.settings.raise_result {
        winutil::snapshot()
    } else {
        Vec::new()
    };
    let ok = f();
    if ok && cfg.settings.raise_result {
        std::thread::sleep(std::time::Duration::from_millis(600));
        winutil::raise_new(&before, hint);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::{find_search_prefix, has_explicit_verb};
    use crate::actions::Action;

    fn actions() -> Vec<Action> {
        serde_json::from_str(include_str!("../resources/actions.json"))
            .expect("valid read-only fixture")
    }

    #[test]
    fn search_triggers_match_normalized_gigaam_phrases() {
        let actions = actions();
        let google = actions
            .iter()
            .find(|action| action.id == "web_google")
            .unwrap();
        let youtube = actions
            .iter()
            .find(|action| action.id == "web_youtube")
            .unwrap();
        assert_eq!(
            find_search_prefix("пожалуйста, загугли погоду", &google.phrases),
            Some((1, "погоду".into()))
        );
        assert_eq!(
            find_search_prefix("ну за гугле погоду", &google.phrases),
            Some((2, "погоду".into()))
        );
        assert_eq!(
            find_search_prefix("на ютубе коты", &youtube.phrases),
            Some((2, "коты".into()))
        );
        assert_eq!(
            find_search_prefix("youtube cats", &youtube.phrases),
            Some((1, "cats".into()))
        );
    }

    #[test]
    fn explicit_launch_keeps_system_actions_from_stealing_the_command() {
        assert!(has_explicit_verb("запусти пожалуйста фотошоп"));
        assert!(has_explicit_verb("плей открой"));
        assert!(!has_explicit_verb("плей"));
    }
}
