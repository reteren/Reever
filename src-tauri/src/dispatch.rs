//! Диспетчер: чистый разбор команды и исполнение выбранного маршрута.

use std::path::PathBuf;
use std::sync::Arc;

use strsim::jaro_winkler;
use tauri::AppHandle;

use crate::actions::{Action, SharedState};
use crate::config::ReeverConfig;
use crate::events::ui_log;
use crate::everything::{self, SearchOutcome};
use crate::grammar::{Grammar, IntentKind};
use crate::textnorm::{normalize, strip_leading_fillers};
use crate::{launcher, system, winutil};

pub enum DispatchResult {
    Done(bool),
    Confirm(String),
}

/// Куда ведёт команда — чистое решение без побочных эффектов (тестируется без Tauri,
/// используется `examples/eval.rs`). Контракт: docs/grammar.md.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// Системное действие (выключение, медиа…); опасные подтверждает `dispatch`.
    System(Action),
    /// Одно или несколько пользовательских действий (мультизапуск «X и Y»).
    Launch(Vec<Action>),
    /// Ни одно действие не подошло — искать программу через Everything (если включено).
    LaunchFallback(String),
    /// Динамический поиск папки через Everything.
    Folder(String),
    /// Веб-поиск: готовый URL и подпись намерения.
    Search { label: String, url: String },
    /// Ничего не подошло.
    Unknown,
}

/// Разбирает нормализованный текст после wake; ведущие слова-паразиты снимаются здесь,
/// чтобы одинаково обрабатывались вызовы из движка и прямые route-тесты.
pub fn route(text: &str, grammar: &Grammar, actions: &[Action], match_threshold: f64) -> Route {
    let text = strip_leading_fillers(&normalize(text));
    if text.is_empty() {
        return Route::Unknown;
    }

    if let Some(trigger) = grammar.match_trigger(&text) {
        let payload = trigger.payload.trim();
        if payload.is_empty() {
            return Route::Unknown;
        }

        return match trigger.intent.kind {
            IntentKind::Folder => Route::Folder(payload.to_owned()),
            IntentKind::Search => Route::Search {
                label: trigger.intent.label.clone(),
                url: trigger.intent.url.replace("{q}", &urlencode(payload)),
            },
            IntentKind::Launch => {
                let matched = match_launch_targets(payload, actions, match_threshold);
                if matched.is_empty() {
                    Route::LaunchFallback(payload.to_owned())
                } else {
                    Route::Launch(matched)
                }
            }
        };
    }

    if grammar.allows_bare_launch() {
        if let Some((action, _score)) = best_action(&text, actions, match_threshold, |action| {
            action.kind != "search"
        }) {
            return if action.kind == "system" {
                Route::System(action.clone())
            } else {
                Route::Launch(vec![action.clone()])
            };
        }

        // При bare-launch разрешены и короткие имена нескольких действий; сначала
        // проверяем, что каждый фрагмент разрешается в пользовательское действие.
        let targets = split_targets(&text);
        if targets.len() > 1 {
            let mut matched = Vec::with_capacity(targets.len());
            for target in &targets {
                let Some((action, _score)) =
                    best_action(target, actions, match_threshold, |action| {
                        action.kind != "system" && action.kind != "search"
                    })
                else {
                    matched.clear();
                    break;
                };
                matched.push(action.clone());
            }
            if !matched.is_empty() {
                return Route::Launch(matched);
            }
        }

        Route::LaunchFallback(text)
    } else {
        best_action(&text, actions, match_threshold, |action| {
            action.kind == "system"
        })
        .map(|(action, _score)| Route::System(action.clone()))
        .unwrap_or(Route::Unknown)
    }
}

fn match_launch_targets(payload: &str, actions: &[Action], threshold: f64) -> Vec<Action> {
    split_targets(payload)
        .iter()
        .filter_map(|target| {
            best_action(target, actions, threshold, |action| {
                action.kind != "system" && action.kind != "search"
            })
            .map(|(action, _score)| action.clone())
        })
        .collect()
}

fn best_action<'a>(
    text: &str,
    actions: &'a [Action],
    threshold: f64,
    eligible: impl Fn(&Action) -> bool,
) -> Option<(&'a Action, f64)> {
    let candidate = text.to_owned();
    actions
        .iter()
        .filter(|action| eligible(action))
        .map(|action| (action, action.score(std::slice::from_ref(&candidate))))
        .filter(|(_, score)| *score >= threshold)
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn launch_action_score(action: &Action, text: &str, grammar: &Grammar) -> f64 {
    let normalized = strip_leading_fillers(&normalize(text));
    let payload = grammar
        .match_trigger(&normalized)
        .filter(|matched| matched.intent.kind == IntentKind::Launch)
        .map(|matched| matched.payload)
        .unwrap_or(normalized);
    split_targets(&payload)
        .iter()
        .map(|target| action.score(std::slice::from_ref(target)))
        .fold(0.0, f64::max)
}

pub fn dispatch(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    ru_text: &str,
    en_text: Option<&str>,
) -> DispatchResult {
    // Копируем снапшоты под короткими локами; Everything и запуск приложений идут
    // уже без удержания общих mutex, чтобы не задерживать обновления грамматики/UI.
    let grammar = shared.grammar.lock().unwrap().clone();
    let actions = shared.actions.lock().unwrap().clone();
    let _ = en_text; // EN-транскрипция больше не используется для папок и запуска.
    let route = route(ru_text, &grammar, &actions, cfg.settings.match_threshold);

    match route {
        Route::System(action) => {
            let op = action.target.clone();
            if system::is_dangerous(&op) && cfg.settings.confirm_dangerous {
                ui_log(
                    app,
                    format!("⚙ {} — подтверди голосом: «да» / «отмена»", action.label),
                );
                return DispatchResult::Confirm(op);
            }
            let normalized = strip_leading_fillers(&normalize(ru_text));
            let score = action.score(&[normalized]);
            ui_log(
                app,
                format!("⚙ система: {} (score {score:.2})", action.label),
            );
            DispatchResult::Done(system::run_system(&op))
        }
        Route::Launch(actions) => {
            let mut any = false;
            for action in &actions {
                let score = launch_action_score(action, ru_text, &grammar);
                ui_log(
                    app,
                    format!("↪ действие «{}» (score {score:.2})", action.label),
                );
                if run_action(app, cfg, action) {
                    any = true;
                }
            }
            DispatchResult::Done(any)
        }
        Route::LaunchFallback(target) => {
            if cfg.settings.offline_mode {
                ui_log(app, "🔒 офлайн-режим: поиск программ (Everything) отключён");
                return DispatchResult::Done(false);
            }
            if !cfg.settings.launch_everything_fallback {
                ui_log(app, "∅ нет такого действия (добавь и откалибруй)");
                return DispatchResult::Done(false);
            }

            let query = cfg.aliases.app(&target).unwrap_or_else(|| target.clone());
            if cfg.settings.private_log {
                ui_log(app, "🔎 (fallback) поиск программ…");
            } else {
                ui_log(app, format!("🔎 (fallback) поиск программ: «{query}»"));
            }
            let outcome = everything::find_executables(
                &cfg.settings.everything_host,
                cfg.settings.everything_port,
                &query,
            );
            DispatchResult::Done(handle_launch(app, cfg, outcome, &query))
        }
        Route::Folder(target) => DispatchResult::Done(open_via_everything(app, cfg, &target)),
        Route::Search { label, url } => {
            if cfg.settings.offline_mode {
                ui_log(app, "🔒 офлайн-режим: веб-поиск отключён");
                return DispatchResult::Done(false);
            }
            if cfg.settings.private_log {
                ui_log(app, format!("🔎 {label}"));
            } else {
                ui_log(app, format!("🔎 {label}: {url}"));
            }
            DispatchResult::Done(launch_with_raise(cfg, &label, || {
                launcher::shell_open(&url)
            }))
        }
        Route::Unknown => {
            ui_log(app, "⚠ не распознал команду");
            DispatchResult::Done(false)
        }
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
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

/// Разбить цель по «и»/«and»/«,» (мультизапуск). Не дробим, если часть короче
/// 2 символов — это обычно мисрекогн одного слова («с и фай» = спотифай).
fn split_targets(s: &str) -> Vec<String> {
    let parts: Vec<String> = s
        .replace(" и ", ",")
        .replace(" and ", ",")
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() > 1 && parts.iter().all(|part| part.chars().count() >= 2) {
        parts
    } else {
        vec![s.trim().to_string()]
    }
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

fn open_via_everything(app: &AppHandle, cfg: &ReeverConfig, target: &str) -> bool {
    if cfg.settings.offline_mode {
        ui_log(app, "🔒 офлайн-режим: поиск папок (Everything) отключён");
        return false;
    }
    let host = &cfg.settings.everything_host;
    let port = cfg.settings.everything_port;

    let query = cfg
        .aliases
        .folder(target)
        .unwrap_or_else(|| target.to_string());
    if cfg.settings.private_log {
        ui_log(app, "🔎 поиск папок…");
    } else {
        ui_log(app, format!("🔎 поиск папок: «{query}»"));
    }
    let outcome = everything::find_folders(host, port, &query);
    handle_open(app, cfg, outcome, &query)
}

fn handle_open(app: &AppHandle, cfg: &ReeverConfig, outcome: SearchOutcome, probe: &str) -> bool {
    if let Some(error) = outcome.error {
        ui_log(app, format!("⚠ {error}"));
        return false;
    }
    if outcome.paths.is_empty() {
        ui_log(app, "∅ папки не найдены");
        return false;
    }
    let best = outcome.paths[0].clone();
    let base = best
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();

    let similarity = jaro_winkler(&probe.to_lowercase(), &base);
    if similarity < cfg.settings.open_min_similarity {
        ui_log(
            app,
            format!(
                "∅ не уверен ({similarity:.2} < {:.2}) — пропускаю «{}»",
                cfg.settings.open_min_similarity,
                best.display()
            ),
        );
        return false;
    }

    ui_log(app, format!("📁 {}", best.display()));
    if outcome.paths.len() > 1 {
        ui_log(
            app,
            format!("(+{} др. — открываю лучшее)", outcome.paths.len() - 1),
        );
    }
    let hint = base;
    launch_with_raise(cfg, &hint, || {
        everything::open_folders(std::slice::from_ref(&best)) > 0
    })
}

fn handle_launch(app: &AppHandle, cfg: &ReeverConfig, outcome: SearchOutcome, hint: &str) -> bool {
    if let Some(error) = outcome.error {
        ui_log(app, format!("⚠ {error}"));
        return false;
    }
    if outcome.paths.is_empty() {
        ui_log(app, "∅ программа не найдена");
        return false;
    }
    let target = outcome.paths[0].clone();
    ui_log(app, format!("▶ запуск: {}", target.display()));
    let hint = hint.to_string();
    launch_with_raise(cfg, &hint, || launcher::launch(&target))
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
    use super::{route, Route};
    use crate::actions::Action;
    use crate::grammar::Grammar;

    const THRESHOLD: f64 = 0.7;

    fn actions() -> Vec<Action> {
        serde_json::from_str(include_str!("../resources/actions.json"))
            .expect("resources/actions.json is valid")
    }

    fn action_id(route: Route) -> String {
        match route {
            Route::System(action) => action.id,
            Route::Launch(actions) if actions.len() == 1 => actions[0].id.clone(),
            other => panic!("ожидалось одно действие, получено {other:?}"),
        }
    }

    fn launch_ids(route: Route) -> Vec<String> {
        match route {
            Route::Launch(actions) => actions.into_iter().map(|action| action.id).collect(),
            other => panic!("ожидался Route::Launch, получено {other:?}"),
        }
    }

    #[test]
    fn bare_launch_and_system_routes_use_the_real_action_registry() {
        let grammar = Grammar::defaults();
        let actions = actions();

        assert_eq!(
            action_id(route("фотошоп", &grammar, &actions, THRESHOLD)),
            "act_mqlpel49"
        );
        assert_eq!(
            action_id(route("дота 2", &grammar, &actions, THRESHOLD)),
            "act_mqlp3ixy"
        );
        assert_eq!(
            action_id(route("дискорд", &grammar, &actions, THRESHOLD)),
            "act_mqlpwj9y"
        );
        assert_eq!(
            action_id(route("пауза", &grammar, &actions, THRESHOLD)),
            "sys_play"
        );
        assert_eq!(
            action_id(route("выключи комп", &grammar, &actions, THRESHOLD)),
            "sys_shutdown"
        );
        assert_eq!(
            action_id(route("следующий трек", &grammar, &actions, THRESHOLD)),
            "sys_next"
        );
        assert_eq!(
            route("ну пожалуйста фотошоп", &grammar, &actions, THRESHOLD),
            Route::Launch(vec![actions
                .iter()
                .find(|action| action.id == "act_mqlpel49")
                .unwrap()
                .clone()])
        );
        assert_eq!(
            route("абракадабра", &grammar, &actions, THRESHOLD),
            Route::LaunchFallback("абракадабра".into())
        );
    }

    #[test]
    fn bare_multi_launch_splits_real_photoshop_and_discord_actions() {
        let grammar = Grammar::defaults();
        let actions = actions();
        assert_eq!(
            launch_ids(route("фотошоп и дискорд", &grammar, &actions, THRESHOLD)),
            ["act_mqlpel49", "act_mqlpwj9y"]
        );
    }

    #[test]
    fn removing_bare_launch_keeps_only_system_fallback_and_unknown() {
        let mut grammar = Grammar::defaults();
        let launch = grammar
            .intents
            .iter_mut()
            .find(|intent| intent.id == "launch")
            .unwrap();
        launch.triggers.retain(|trigger| !trigger.trim().is_empty());
        let actions = actions();

        assert_eq!(
            route("фотошоп", &grammar, &actions, THRESHOLD),
            Route::Unknown
        );
        assert_eq!(
            action_id(route("выключи комп", &grammar, &actions, THRESHOLD)),
            "sys_shutdown"
        );
    }

    #[test]
    fn explicit_launch_and_folder_intents_take_precedence() {
        let grammar = Grammar::defaults();
        let actions = actions();

        assert_eq!(
            action_id(route("открой фотошоп", &grammar, &actions, THRESHOLD)),
            "act_mqlpel49"
        );
        assert_eq!(
            launch_ids(route(
                "открой фотошоп и дискорд",
                &grammar,
                &actions,
                THRESHOLD
            )),
            ["act_mqlpel49", "act_mqlpwj9y"]
        );
        assert_eq!(
            route("папка загрузки", &grammar, &actions, THRESHOLD),
            Route::Folder("загрузки".into())
        );
        assert_eq!(
            route("открой папку загрузки", &grammar, &actions, THRESHOLD),
            Route::Folder("загрузки".into())
        );
        assert_eq!(
            action_id(route("запусти доту два", &grammar, &actions, THRESHOLD)),
            "act_mqlp3ixy"
        );
        assert_eq!(
            route("открой абракадабра", &grammar, &actions, THRESHOLD),
            Route::LaunchFallback("абракадабра".into())
        );
    }

    #[test]
    fn search_intents_build_encoded_google_and_youtube_urls() {
        let grammar = Grammar::defaults();
        let actions = actions();

        assert_eq!(
            route("загугли погода в москве", &grammar, &actions, THRESHOLD),
            Route::Search {
                label: "поиск в Google".into(),
                url: "https://www.google.com/search?q=%D0%BF%D0%BE%D0%B3%D0%BE%D0%B4%D0%B0+%D0%B2+%D0%BC%D0%BE%D1%81%D0%BA%D0%B2%D0%B5".into(),
            }
        );
        assert_eq!(
            route("поиск ютуб смешные коты", &grammar, &actions, THRESHOLD),
            Route::Search {
                label: "поиск на YouTube".into(),
                url: "https://www.youtube.com/results?search_query=%D1%81%D0%BC%D0%B5%D1%88%D0%BD%D1%8B%D0%B5+%D0%BA%D0%BE%D1%82%D1%8B".into(),
            }
        );
    }

    #[test]
    fn explicit_launch_still_works_when_bare_launch_is_disabled() {
        let mut grammar = Grammar::defaults();
        let launch = grammar
            .intents
            .iter_mut()
            .find(|intent| intent.id == "launch")
            .unwrap();
        launch.triggers.retain(|trigger| !trigger.trim().is_empty());
        let actions = actions();

        assert_eq!(
            action_id(route("открой фотошоп", &grammar, &actions, THRESHOLD)),
            "act_mqlpel49"
        );
    }
}
