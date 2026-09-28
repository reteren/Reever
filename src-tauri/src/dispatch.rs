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
use crate::{calc, calcapp, launcher, system, winutil};

pub enum DispatchResult {
    Done(bool),
    /// Выполнить только после голосового «да» (docs/app-control.md).
    Confirm(PendingAction),
}

/// Действие, которое ждёт подтверждения голосом.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingAction {
    /// Опасная системная операция (`system::run_system`).
    System(String),
    /// Мягко закрыть программу.
    Close(crate::procs::RunningApp),
    /// Завершить программу принудительно.
    Kill(crate::procs::RunningApp),
}

impl PendingAction {
    /// Что сказать в лог при вопросе «подтверди».
    pub fn describe(&self) -> String {
        match self {
            PendingAction::System(op) => op.clone(),
            PendingAction::Close(app) => format!("закрыть {}", app_title(app)),
            PendingAction::Kill(app) => format!("завершить {}", app_title(app)),
        }
    }
}

fn app_title(app: &crate::procs::RunningApp) -> String {
    if app.description.trim().is_empty() {
        app.exe_name.clone()
    } else {
        app.description.clone()
    }
}

/// Выполнить подтверждённое действие. Возвращает успех.
pub fn execute_pending(app: &AppHandle, pending: &PendingAction) -> bool {
    match pending {
        PendingAction::System(op) => system::run_system(op),
        PendingAction::Close(target) => match crate::procs::close_app(target) {
            Ok(n) => {
                ui_log(app, format!("✕ закрываю {} (окон: {n})", app_title(target)));
                n > 0
            }
            Err(e) => {
                ui_log(
                    app,
                    format!("⚠ не удалось закрыть {}: {e:#}", app_title(target)),
                );
                false
            }
        },
        PendingAction::Kill(target) => match crate::procs::kill_app(target) {
            Ok(n) => {
                ui_log(
                    app,
                    format!("☠ завершено {} (процессов: {n})", app_title(target)),
                );
                n > 0
            }
            Err(e) => {
                ui_log(
                    app,
                    format!("⚠ не удалось завершить {}: {e:#}", app_title(target)),
                );
                false
            }
        },
    }
}

/// Первая буква заглавной: нормализованный текст приходит в нижнем регистре.
fn capitalize_first(text: &str) -> String {
    let mut chars = text.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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
    Folder(Vec<String>),
    /// Веб-поиск: готовый URL и подпись намерения.
    Search { label: String, url: String },
    /// Закрыть запущенную программу: кандидаты названия по порядку предпочтения.
    Close(Vec<String>),
    /// Завершить запущенную программу: кандидаты названия по порядку предпочтения.
    Kill(Vec<String>),
    /// Посчитать в калькуляторе: кандидаты выражения (русский и английский варианты).
    Calc(Vec<String>),
    /// Диктовка с первым куском текста. Обычно её перехватывает Engine раньше `route`
    /// (он сам печатает текст); сюда она доходит только из eval и тестов.
    Dictate(String),
    /// Веб-чат с ИИ: адрес, подпись намерения и вопрос.
    WebChat { label: String, url: String, text: String },
    /// Ничего не подошло.
    Unknown,
}

/// Разбирает нормализованный текст после wake; ведущие слова-паразиты снимаются здесь,
/// чтобы одинаково обрабатывались вызовы из движка и прямые route-тесты.
pub fn route(
    text: &str,
    alt: Option<&str>,
    grammar: &Grammar,
    actions: &[Action],
    match_threshold: f64,
) -> Route {
    let text = strip_leading_fillers(&normalize(text));
    if text.is_empty() {
        return Route::Unknown;
    }

    if let Some(trigger) = grammar.match_trigger(&text) {
        let mut payloads = vec![trigger.payload.trim().to_string()];
        if trigger.intent.kind != IntentKind::Search {
            if let Some(alt) = alt.map(|alt| strip_leading_fillers(&normalize(alt))) {
                if let Some(alt_trigger) = grammar
                    .match_trigger(&alt)
                    .filter(|matched| matched.intent.id == trigger.intent.id)
                {
                    push_unique(&mut payloads, alt_trigger.payload.trim());
                }
            }
        }
        if trigger.intent.kind == IntentKind::Dictate {
            return Route::Dictate(payloads[0].clone());
        }
        if payloads[0].is_empty() {
            return Route::Unknown;
        }

        return match trigger.intent.kind {
            IntentKind::Folder => Route::Folder(payloads),
            IntentKind::Close => Route::Close(payloads),
            IntentKind::Kill => Route::Kill(payloads),
            IntentKind::Calc => Route::Calc(payloads),
            IntentKind::Dictate => Route::Dictate(payloads[0].clone()),
            IntentKind::WebChat => Route::WebChat {
                label: trigger.intent.label.clone(),
                url: trigger.intent.url.trim().to_string(),
                text: capitalize_first(&payloads[0]),
            },
            IntentKind::Search => Route::Search {
                label: trigger.intent.label.clone(),
                url: trigger.intent.url.replace("{q}", &urlencode(&payloads[0])),
            },
            IntentKind::Launch => {
                let matched = match_launch_targets(&payloads, actions, match_threshold);
                if matched.is_empty() {
                    Route::LaunchFallback(payloads[0].clone())
                } else {
                    Route::Launch(matched)
                }
            }
        };
    }

    if grammar.allows_bare_launch() {
        let candidates = command_candidates(&text, alt);
        if let Some((action, _score)) =
            best_action(&candidates, actions, match_threshold, |action| {
                action.kind != "search"
            })
        {
            return if action.kind == "system" {
                Route::System(action.clone())
            } else {
                Route::Launch(vec![action.clone()])
            };
        }

        // При bare-launch разрешены и короткие имена нескольких действий; сначала
        // проверяем, что каждый фрагмент разрешается в пользовательское действие.
        for candidate in &candidates {
            let targets = split_targets(candidate);
            if targets.len() > 1 {
                let mut matched = Vec::with_capacity(targets.len());
                for target in &targets {
                    let Some((action, _score)) = best_action(
                        std::slice::from_ref(target),
                        actions,
                        match_threshold,
                        |action| action.kind != "system" && action.kind != "search",
                    ) else {
                        matched.clear();
                        break;
                    };
                    if !matched
                        .iter()
                        .any(|previous: &Action| previous.id == action.id)
                    {
                        matched.push(action.clone());
                    }
                }
                if !matched.is_empty() {
                    return Route::Launch(matched);
                }
            }
        }

        Route::LaunchFallback(text)
    } else {
        best_action(
            std::slice::from_ref(&text),
            actions,
            match_threshold,
            |action| action.kind == "system",
        )
        .map(|(action, _score)| Route::System(action.clone()))
        .unwrap_or(Route::Unknown)
    }
}

fn match_launch_targets(payloads: &[String], actions: &[Action], threshold: f64) -> Vec<Action> {
    let mut matched = Vec::new();
    for payload in payloads {
        for target in split_targets(payload) {
            if let Some((action, _score)) = best_action(
                std::slice::from_ref(&target),
                actions,
                threshold,
                |action| action.kind != "system" && action.kind != "search",
            ) {
                if !matched
                    .iter()
                    .any(|previous: &Action| previous.id == action.id)
                {
                    matched.push(action.clone());
                }
            }
        }
    }
    matched
}

fn best_action<'a>(
    candidates: &[String],
    actions: &'a [Action],
    threshold: f64,
    eligible: impl Fn(&Action) -> bool,
) -> Option<(&'a Action, f64)> {
    actions
        .iter()
        .filter(|action| eligible(action))
        .map(|action| (action, action.score(candidates)))
        .filter(|(_, score)| *score >= threshold)
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn launch_action_score(action: &Action, texts: &[&str], grammar: &Grammar) -> f64 {
    texts
        .iter()
        .flat_map(|text| {
            let normalized = strip_leading_fillers(&normalize(text));
            let payload = grammar
                .match_trigger(&normalized)
                .filter(|matched| matched.intent.kind == IntentKind::Launch)
                .map(|matched| matched.payload)
                .unwrap_or(normalized);
            split_targets(&payload)
        })
        .map(|target| action.score(std::slice::from_ref(&target)))
        .fold(0.0, f64::max)
}

fn command_candidates(text: &str, alt: Option<&str>) -> Vec<String> {
    let mut candidates = vec![text.to_string()];
    if let Some(alt) = alt {
        push_unique(
            &mut candidates,
            strip_leading_fillers(&normalize(alt)).trim(),
        );
    }
    candidates
}

fn push_unique(candidates: &mut Vec<String>, candidate: &str) {
    let candidate = candidate.trim();
    if !candidate.is_empty()
        && !candidates
            .iter()
            .any(|existing| normalize(existing) == normalize(candidate))
    {
        candidates.push(candidate.to_string());
    }
}

pub fn dispatch(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    text: &str,
    alt: Option<&str>,
) -> DispatchResult {
    // Копируем снапшоты под короткими локами; Everything и запуск приложений идут
    // уже без удержания общих mutex, чтобы не задерживать обновления грамматики/UI.
    let grammar = shared.grammar.lock().unwrap().clone();
    let actions = shared.actions.lock().unwrap().clone();
    let route = route(text, alt, &grammar, &actions, cfg.settings.match_threshold);

    match route {
        Route::System(action) => {
            let op = action.target.clone();
            if system::is_dangerous(&op) && cfg.settings.confirm_dangerous {
                ui_log(
                    app,
                    format!("⚙ {} — подтверди голосом: «да» / «отмена»", action.label),
                );
                return DispatchResult::Confirm(PendingAction::System(op));
            }
            let normalized = strip_leading_fillers(&normalize(text));
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
                let score = launch_action_score(action, &[text, alt.unwrap_or("")], &grammar);
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
        Route::Folder(targets) => DispatchResult::Done(open_via_everything(app, cfg, &targets)),
        Route::Dictate(_) => {
            // Диктовку ведёт Engine; сюда попадает только команда, пришедшая в обход него.
            ui_log(app, "⚠ диктовка запускается голосом: «ривер напиши …»");
            DispatchResult::Done(false)
        }
        Route::Calc(candidates) => {
            let expression = candidates
                .iter()
                .find_map(|candidate| calc::parse_expression(candidate));
            let Some(expression) = expression else {
                if cfg.settings.private_log {
                    ui_log(app, "⚠ не понял выражение (частный режим)");
                } else {
                    ui_log(
                        app,
                        format!("⚠ не понял выражение «{}»", candidates.join(" / ")),
                    );
                }
                return DispatchResult::Done(false);
            };

            match calcapp::open_and_type(&expression.keys) {
                Ok(()) => {
                    if cfg.settings.private_log {
                        ui_log(app, "🧮 считаю");
                    } else if let Some(value) = expression.value {
                        ui_log(app, format!("🧮 {} = {value}", expression.pretty));
                    } else {
                        ui_log(app, format!("🧮 {}", expression.pretty));
                    }
                    DispatchResult::Done(true)
                }
                Err(error) => {
                    if cfg.settings.private_log {
                        ui_log(
                            app,
                            "⚠ не удалось ввести выражение в калькулятор (частный режим)",
                        );
                    } else {
                        ui_log(
                            app,
                            format!("⚠ не удалось ввести выражение в калькулятор: {error:#}"),
                        );
                    }
                    DispatchResult::Done(false)
                }
            }
        }
        Route::Close(candidates) => {
            dispatch_process_control(app, cfg, &actions, &candidates, false)
        }
        Route::Kill(candidates) => dispatch_process_control(app, cfg, &actions, &candidates, true),
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
            DispatchResult::Done(open_url(cfg, &label, &url, || {
                launcher::shell_open(&url)
            }))
        }
        Route::WebChat { label, url, text } => {
            if cfg.settings.offline_mode {
                ui_log(app, "🔒 офлайн-режим: чат в браузере отключён");
                return DispatchResult::Done(false);
            }
            if cfg.settings.private_log {
                ui_log(app, format!("💬 {label}"));
            } else {
                ui_log(app, format!("💬 {label}: «{text}»"));
            }
            match crate::webchat::open_and_send(&url, &text, cfg.settings.webchat_send) {
                Ok(crate::webchat::ChatOutcome::Typed) => DispatchResult::Done(true),
                Ok(crate::webchat::ChatOutcome::Clipboard) => {
                    ui_log(
                        app,
                        "📋 поле ввода чата не нашлось — вопрос в буфере обмена, вставь Ctrl+V",
                    );
                    DispatchResult::Done(false)
                }
                Err(error) => {
                    ui_log(app, format!("⚠ чат: {error:#}"));
                    DispatchResult::Done(false)
                }
            }
        }
        Route::Unknown => {
            ui_log(app, "⚠ не распознал команду");
            DispatchResult::Done(false)
        }
    }
}

fn dispatch_process_control(
    app: &AppHandle,
    cfg: &ReeverConfig,
    actions: &[Action],
    candidates: &[String],
    kill: bool,
) -> DispatchResult {
    let apps = crate::procs::running_apps();
    let Some(found) =
        crate::procs::match_app(candidates, &apps, actions, cfg.settings.match_threshold)
    else {
        if cfg.settings.private_log {
            ui_log(app, "∅ не нашёл открытую программу (частный режим)");
        } else {
            ui_log(
                app,
                format!("∅ не нашёл открытую программу «{}»", candidates.join(" / ")),
            );
        }
        return DispatchResult::Done(false);
    };

    let title = app_title(&found.app);
    if cfg.settings.private_log {
        ui_log(
            app,
            format!(
                "🔎 найдена открытая программа (совпадение: {}, score {:.2})",
                private_match_source(&found.matched_by),
                found.score
            ),
        );
    } else {
        ui_log(
            app,
            format!(
                "🔎 найдена открытая программа «{title}» по {} (score {:.2})",
                found.matched_by, found.score
            ),
        );
    }

    if kill && crate::procs::is_protected(&found.app.exe_name) {
        if cfg.settings.private_log {
            ui_log(
                app,
                "⚠ отказ: нельзя завершить защищённый системный процесс",
            );
        } else {
            ui_log(
                app,
                format!("⚠ отказ: «{title}» — защищённый системный процесс"),
            );
        }
        return DispatchResult::Done(false);
    }

    let pending = if kill {
        PendingAction::Kill(found.app)
    } else {
        PendingAction::Close(found.app)
    };
    if cfg.settings.confirm_close {
        if cfg.settings.private_log {
            ui_log(
                app,
                "❔ подтверди закрытие найденной программы голосом: «да» / «нет»",
            );
        } else {
            ui_log(
                app,
                format!(
                    "❔ {} — подтверди голосом: «да» / «нет»",
                    pending.describe()
                ),
            );
        }
        DispatchResult::Confirm(pending)
    } else {
        DispatchResult::Done(execute_pending(app, &pending))
    }
}

fn private_match_source(matched_by: &str) -> &'static str {
    if matched_by.starts_with("действие") {
        "действие"
    } else if matched_by.starts_with("заголовок окна") {
        "заголовок окна"
    } else if matched_by.starts_with("описание") {
        "описание"
    } else if matched_by.starts_with("exe") {
        "имя exe"
    } else {
        "название программы"
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
        _ if winutil::is_web_url(&target) => {
            ui_log(app, format!("🌐 открываю: {target}"));
            open_url(cfg, &hint, &target, || launcher::shell_open(&target))
        }
        _ => {
            ui_log(app, format!("▶ запуск: {target}"));
            launch_with_raise(cfg, &hint, || launcher::shell_open(&target))
        }
    }
}

fn open_via_everything(app: &AppHandle, cfg: &ReeverConfig, targets: &[String]) -> bool {
    if cfg.settings.offline_mode {
        ui_log(app, "🔒 офлайн-режим: поиск папок (Everything) отключён");
        return false;
    }
    let host = &cfg.settings.everything_host;
    let port = cfg.settings.everything_port;

    let mut queries = Vec::new();
    for target in targets {
        let query = cfg
            .aliases
            .folder(target)
            .unwrap_or_else(|| target.to_string());
        push_unique(&mut queries, &query);
    }
    if queries.is_empty() {
        ui_log(app, "∅ пустой запрос папки");
        return false;
    }
    if cfg.settings.private_log {
        ui_log(
            app,
            format!("🔎 поиск папок по {} вариантам…", queries.len()),
        );
    } else {
        ui_log(
            app,
            format!(
                "🔎 поиск папок: {}",
                queries
                    .iter()
                    .map(|q| format!("«{q}»"))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        );
    }

    let mut errors = Vec::new();
    let mut best: Option<(std::path::PathBuf, String, usize, f64)> = None;
    for (index, query) in queries.iter().enumerate() {
        let outcome = everything::find_folders(host, port, query);
        if let Some(error) = outcome.error {
            errors.push(error);
        }
        for path in outcome.paths {
            let base = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let similarity = jaro_winkler(&normalize(query), &normalize(base));
            if best
                .as_ref()
                .is_none_or(|(_, _, _, score)| similarity > *score)
            {
                best = Some((path, query.clone(), index, similarity));
            }
        }
    }
    let Some((best, query, variant_index, similarity)) = best else {
        if let Some(error) = errors.first() {
            ui_log(app, format!("⚠ {error}"));
        } else {
            ui_log(app, "∅ папки не найдены");
        }
        return false;
    };
    let base = best
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();

    if similarity < cfg.settings.open_min_similarity {
        ui_log(
            app,
            format!(
                "∅ вариант {} не уверен ({similarity:.2} < {:.2}) — пропускаю «{}»",
                variant_index + 1,
                cfg.settings.open_min_similarity,
                best.display()
            ),
        );
        return false;
    }

    if cfg.settings.private_log {
        ui_log(
            app,
            format!(
                "📁 победил вариант {} (похожесть {similarity:.2})",
                variant_index + 1
            ),
        );
    } else {
        ui_log(
            app,
            format!(
                "📁 победил вариант {} «{query}» (похожесть {similarity:.2}): {}",
                variant_index + 1,
                best.display()
            ),
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

/// Открыть ссылку и вывести браузер на передний план — в том числе свёрнутый: вкладка
/// создаётся в уже открытом окне, и `raise_new` (только новые окна) его не поднимет.
/// Браузер обрабатывает ссылку асинхронно, поэтому окно ищется несколько раз.
fn open_url(cfg: &ReeverConfig, hint: &str, url: &str, f: impl FnOnce() -> bool) -> bool {
    let _ = url;
    if !cfg.settings.raise_result {
        return f();
    }
    let before = winutil::snapshot();
    let ok = f();
    if !ok {
        return false;
    }
    for delay in [250, 400, 600] {
        std::thread::sleep(std::time::Duration::from_millis(delay));
        if winutil::raise_default_browser() {
            return true;
        }
    }
    // Браузер по умолчанию не определился — хотя бы новое окно (браузер был закрыт).
    winutil::raise_new(&before, hint);
    true
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
    use super::{route as route_with_alt, Route};
    use crate::actions::Action;
    use crate::grammar::Grammar;

    const THRESHOLD: f64 = 0.7;

    fn route(text: &str, grammar: &Grammar, actions: &[Action], threshold: f64) -> Route {
        route_with_alt(text, None, grammar, actions, threshold)
    }

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
            Route::Folder(vec!["загрузки".into()])
        );
        assert_eq!(
            route("открой папку загрузки", &grammar, &actions, THRESHOLD),
            Route::Folder(vec!["загрузки".into()])
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

    #[test]
    fn search_uses_primary_text_and_ignores_alt() {
        let grammar = Grammar::defaults();
        let actions = actions();
        let result = route_with_alt(
            "поиск ютуб sinksaiko i'm sloth",
            Some("поиск ютуб смешные коты"),
            &grammar,
            &actions,
            THRESHOLD,
        );
        assert_eq!(
            result,
            Route::Search {
                label: "поиск на YouTube".into(),
                url: format!(
                    "https://www.youtube.com/results?search_query={}",
                    super::urlencode(&crate::textnorm::normalize("sinksaiko i'm sloth"))
                ),
            }
        );
    }

    #[test]
    fn folder_route_keeps_distinct_primary_and_english_targets() {
        let grammar = Grammar::defaults();
        let actions = actions();
        assert_eq!(
            route_with_alt(
                "открой папку загрузки",
                Some("открой папку downloads"),
                &grammar,
                &actions,
                THRESHOLD,
            ),
            Route::Folder(vec!["загрузки".into(), "downloads".into()])
        );
    }

    #[test]
    fn explicit_launch_can_match_an_action_found_only_in_alt() {
        let grammar = Grammar::defaults();
        let actions = vec![Action {
            id: "fl-studio".into(),
            label: "FL Studio".into(),
            kind: "shell".into(),
            target: "fl-studio.exe".into(),
            verbs: Vec::new(),
            phrases: vec!["fl studio".into()],
        }];
        assert_eq!(
            route_with_alt(
                "открой файл студио",
                Some("открой fl studio"),
                &grammar,
                &actions,
                THRESHOLD,
            ),
            Route::Launch(vec![actions[0].clone()])
        );
    }

    #[test]
    fn bare_launch_can_match_an_action_found_only_in_alt() {
        let grammar = Grammar::defaults();
        let actions = vec![Action {
            id: "fl-studio".into(),
            label: "FL Studio".into(),
            kind: "shell".into(),
            target: "fl-studio.exe".into(),
            verbs: Vec::new(),
            phrases: vec!["fl studio".into()],
        }];
        assert_eq!(
            route_with_alt(
                "файл студио",
                Some("fl studio"),
                &grammar,
                &actions,
                THRESHOLD,
            ),
            Route::Launch(vec![actions[0].clone()])
        );
    }

    #[test]
    fn close_and_kill_routes_keep_unique_language_candidates() {
        let grammar = Grammar::defaults();
        let actions = actions();

        assert_eq!(
            route("закрой хром", &grammar, &actions, THRESHOLD),
            Route::Close(vec!["хром".into()])
        );
        assert_eq!(
            route_with_alt(
                "закрой хром",
                Some("закрой chrome"),
                &grammar,
                &actions,
                THRESHOLD,
            ),
            Route::Close(vec!["хром".into(), "chrome".into()])
        );
        assert_eq!(
            route_with_alt(
                "закрой хром",
                Some("закрой хром"),
                &grammar,
                &actions,
                THRESHOLD,
            ),
            Route::Close(vec!["хром".into()])
        );
        assert_eq!(
            route("заверши телеграм", &grammar, &actions, THRESHOLD),
            Route::Kill(vec!["телеграм".into()])
        );
    }

    #[test]
    fn close_trigger_precedes_bare_launch_and_folder_trigger() {
        let grammar = Grammar::defaults();
        let actions = actions();

        assert_eq!(
            route("закрой папку загрузки", &grammar, &actions, THRESHOLD),
            Route::Close(vec!["папку загрузки".into()])
        );
        assert_eq!(
            route("закрой", &grammar, &actions, THRESHOLD),
            Route::Unknown
        );
        assert!(grammar.is_incomplete("закрой"));
    }

    #[test]
    fn calculator_triggers_precede_bare_launch_and_keep_expression_candidates() {
        let grammar = Grammar::defaults();
        let actions = actions();

        assert_eq!(
            route("посчитай 33 умножить на 78", &grammar, &actions, THRESHOLD),
            Route::Calc(vec!["33 умножить на 78".into()])
        );
        assert_eq!(
            route("сколько будет 2 плюс 2", &grammar, &actions, THRESHOLD),
            Route::Calc(vec!["2 плюс 2".into()])
        );
        assert_eq!(
            route("посчитай minecraft", &grammar, &actions, THRESHOLD),
            Route::Calc(vec!["minecraft".into()])
        );
        assert_eq!(
            route("посчитай", &grammar, &actions, THRESHOLD),
            Route::Unknown
        );
        assert!(grammar.is_incomplete("посчитай"));
    }

    #[test]
    fn gemini_variants_route_to_web_chat() {
        let grammar = Grammar::defaults();
        let actions = Vec::new();
        for (heard, question) in [
            ("гемини какая погода будет завтра в москве", "Какая погода будет завтра в москве"),
            ("гимини какая погода будет завтра в москве", "Какая погода будет завтра в москве"),
            ("джимини обясни что такое квантовый компьютер", "Обясни что такое квантовый компьютер"),
            ("gemini what is rust", "What is rust"),
        ] {
            match route(heard, &grammar, &actions, THRESHOLD) {
                Route::WebChat { url, text, .. } => {
                    assert_eq!(url, "https://gemini.google.com/app", "{heard}");
                    assert_eq!(text, question, "{heard}");
                }
                other => panic!("{heard}: {other:?}"),
            }
        }
        for heard in ["гемини", "гимине", "джимини"] {
            assert!(grammar.is_incomplete(heard), "{heard} — ждать вопрос после паузы");
        }
    }

    #[test]
    fn chatgpt_and_claude_route_to_their_chats() {
        let grammar = Grammar::defaults();
        let actions = Vec::new();
        for (heard, url) in [
            ("чат гпт как сварить борщ", "https://chatgpt.com/"),
            ("чатгпт как сварить борщ", "https://chatgpt.com/"),
            ("клауд как сварить борщ", "https://claude.ai/new"),
            ("клод как сварить борщ", "https://claude.ai/new"),
        ] {
            match route(heard, &grammar, &actions, THRESHOLD) {
                Route::WebChat { url: got, text, .. } => {
                    assert_eq!(got, url, "{heard}");
                    assert_eq!(text, "Как сварить борщ", "{heard}");
                }
                other => panic!("{heard}: {other:?}"),
            }
        }
    }
}
