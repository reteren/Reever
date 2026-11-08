//! Диспетчер: чистый разбор команды и исполнение выбранного маршрута.

use std::path::Path;
use std::sync::Arc;

use crate::app::AppHandle;

use crate::actions::{Action, SharedState};
use crate::config::ReeverConfig;
use crate::events::ui_log;
use crate::grammar::{Grammar, IntentKind};
use crate::textnorm::{join_cjk, normalize, strip_leading_fillers};
use crate::{calc, calcapp, launcher, system, winutil};

pub enum DispatchResult {
    Done(bool),
    /// Выполнить только после голосового «да» (docs/app-control.md).
    Confirm(PendingAction),
    /// Показано окно выбора с N вариантами — ждать номер голосом (docs/find.md).
    Choose(usize),
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
    pub fn describe(&self) -> crate::uilang::LogText {
        match self {
            PendingAction::System(op) => op.clone().into(),
            PendingAction::Close(app) => crate::msg!("log.pending.close", app = app_title(app)).into(),
            PendingAction::Kill(app) => crate::msg!("log.pending.kill", app = app_title(app)).into(),
        }
    }
}

pub(crate) fn app_title(app: &crate::procs::RunningApp) -> String {
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
                ui_log(app, crate::msg!("log.closing", app = app_title(target), windows = n));
                n > 0
            }
            Err(e) => {
                ui_log(
                    app,
                    crate::msg!("log.closeFailed", app = app_title(target), error = format!("{e:#}")),
                );
                false
            }
        },
        PendingAction::Kill(target) => match crate::procs::kill_app(target) {
            Ok(n) => {
                ui_log(
                    app,
                    crate::msg!("log.killed", app = app_title(target), processes = n),
                );
                n > 0
            }
            Err(e) => {
                ui_log(
                    app,
                    crate::msg!("log.killFailed", app = app_title(target), error = format!("{e:#}")),
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
    /// Ни одно действие не подошло: текст команды для журнала и замеров.
    NoAction(String),
    /// Поиск папки по своему индексу дисков.
    Folder(Vec<String>),
    /// Веб-поиск: готовый URL и подпись намерения.
    Search { label: String, url: String },
    /// Закрыть запущенную программу: кандидаты названия по порядку предпочтения.
    Close(Vec<String>),
    /// Завершить запущенную программу: кандидаты названия по порядку предпочтения.
    Kill(Vec<String>),
    /// Посчитать в калькуляторе: кандидаты выражения (русский и английский варианты).
    Calc(Vec<String>),
    /// Нажать клавишу или сочетание: кандидаты названия (русский и английский варианты).
    Key(Vec<String>),
    /// «Слушай»: звук компьютера в текст (listen.rs).
    Listen,
    /// Диктовка с первым куском текста. Обычно её перехватывает Engine раньше `route`
    /// (он сам печатает текст); сюда она доходит только из eval и тестов.
    Dictate(String),
    /// Найти в своём индексе и дать выбрать: кандидаты запроса (русский, английский).
    Find {
        scope: crate::find::FindScope,
        candidates: Vec<String>,
    },
    /// Веб-чат с ИИ: адрес, подпись намерения и вопрос.
    WebChat {
        label: String,
        url: String,
        text: String,
    },
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
        let alt_payload = alt
            .map(|alt| strip_leading_fillers(&normalize(alt)))
            .and_then(|alt| {
                grammar
                    .match_trigger(&alt)
                    .filter(|matched| matched.intent.id == trigger.intent.id)
                    .map(|matched| matched.payload.trim().to_string())
            })
            .filter(|payload| !payload.is_empty());
        if matches!(trigger.intent.kind, IntentKind::Search | IntentKind::WebChat) {
            // Поиск и чат уходят одним текстом — язык решает словарь (docs/bilingual.md).
            // Выбор `bilingual` меняется только при явном перевесе словарных слов.
            if let Some(alt_payload) = alt_payload {
                let lang = |t: &str| {
                    if t.chars().any(|c| matches!(c, 'а'..='я' | 'ё')) {
                        "ru-RU"
                    } else {
                        "en-US"
                    }
                };
                let (chosen_lang, other_lang) = (lang(&payloads[0]), lang(&alt_payload));
                if chosen_lang != other_lang
                    && crate::dictation::payload_switch_language(
                        &payloads[0],
                        chosen_lang,
                        &alt_payload,
                        other_lang,
                    )
                {
                    payloads[0] = alt_payload;
                }
            }
        } else if let Some(alt_payload) = alt_payload {
            push_unique(&mut payloads, &alt_payload);
        }
        // «слушай» — без цели: что сказано после глагола, не важно
        if trigger.intent.kind == IntentKind::Listen {
            return Route::Listen;
        }
        if trigger.intent.kind == IntentKind::Dictate {
            return Route::Dictate(payloads[0].clone());
        }
        if payloads[0].is_empty() {
            return Route::Unknown;
        }

        return match trigger.intent.kind {
            // Имена папок и файлов, запросы и текст — обратно без пробелов между
            // иероглифами (normalize разбил «报告» на «报 告»).
            IntentKind::Folder => Route::Folder(payloads.iter().map(|p| join_cjk(p)).collect()),
            IntentKind::Close => Route::Close(payloads),
            IntentKind::Kill => Route::Kill(payloads),
            IntentKind::Calc => Route::Calc(payloads),
            IntentKind::Key => Route::Key(payloads),
            IntentKind::Listen => Route::Listen,
            IntentKind::Dictate => Route::Dictate(join_cjk(&payloads[0])),
            IntentKind::Find | IntentKind::FindFolder | IntentKind::FindFile => Route::Find {
                scope: match trigger.intent.kind {
                    IntentKind::FindFolder => crate::find::FindScope::Folder,
                    IntentKind::FindFile => crate::find::FindScope::File,
                    _ => crate::find::FindScope::Any,
                },
                candidates: payloads.iter().map(|p| join_cjk(p)).collect(),
            },
            IntentKind::WebChat => Route::WebChat {
                label: trigger.intent.label.clone(),
                url: trigger.intent.url.trim().to_string(),
                text: capitalize_first(&join_cjk(&payloads[0])),
            },
            IntentKind::Search => Route::Search {
                label: trigger.intent.label.clone(),
                url: trigger.intent.url.replace("{q}", &urlencode(&join_cjk(&payloads[0]))),
            },
            IntentKind::Launch => {
                let matched = match_launch_targets(&payloads, actions, match_threshold);
                if matched.is_empty() {
                    Route::NoAction(payloads[0].clone())
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

        Route::NoAction(text)
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
    // Копируем снапшоты под короткими локами; поиск и запуск приложений идут
    // уже без удержания общих mutex, чтобы не задерживать обновления грамматики/UI.
    // «открой ривер» / «открой настройки ривера» — окно самого Reever, раньше действий и папок.
    if let Some(page) = [Some(text), alt]
        .into_iter()
        .flatten()
        .find_map(|t| crate::commands::opens_self(t, &cfg.settings.wake_words))
    {
        ui_log(app, crate::msg!("log.showSelf"));
        match page {
            crate::commands::SelfPage::Window => crate::show_main(app),
            crate::commands::SelfPage::Settings => crate::main_native::show_tab(app, "settings"),
        }
        return DispatchResult::Done(true);
    }
    let grammar = shared.grammar.lock().unwrap().clone();
    let actions = shared.actions.lock().unwrap().clone();
    let route = route(text, alt, &grammar, &actions, cfg.settings.match_threshold);

    match route {
        Route::System(action) => {
            let op = action.target.clone();
            if system::is_dangerous(&op) && cfg.settings.confirm_dangerous {
                ui_log(
                    app,
                    crate::msg!("log.confirmSystem", action = &action.label),
                );
                return DispatchResult::Confirm(PendingAction::System(op));
            }
            let normalized = strip_leading_fillers(&normalize(text));
            let score = action.score(&[normalized]);
            ui_log(
                app,
                crate::msg!("log.system", action = &action.label, score = format!("{score:.2}")),
            );
            DispatchResult::Done(system::run_system(&op))
        }
        Route::Launch(actions) => {
            let mut any = false;
            for action in &actions {
                let score = launch_action_score(action, &[text, alt.unwrap_or("")], &grammar);
                ui_log(
                    app,
                    crate::msg!("log.action", action = &action.label, score = format!("{score:.2}")),
                );
                if run_action(app, cfg, action) {
                    any = true;
                }
            }
            DispatchResult::Done(any)
        }
        Route::NoAction(_) => {
            ui_log(app, crate::msg!("log.noAction"));
            DispatchResult::Done(false)
        }
        // «папка X» / «открой папку X» — тот же поиск с окном выбора, что и «найди папку X»;
        // алиасы из aliases.json («загрузки» → Downloads) — дополнительные варианты запроса.
        Route::Folder(targets) => {
            let mut candidates = targets.clone();
            for target in &targets {
                if let Some(alias) = cfg.aliases.folder(target) {
                    push_unique(&mut candidates, &alias);
                }
            }
            dispatch_find(
                app,
                cfg,
                shared,
                crate::find::FindScope::Folder,
                &candidates,
            )
        }
        Route::Dictate(_) => {
            // Диктовку ведёт Engine; сюда попадает только команда, пришедшая в обход него.
            ui_log(
                app,
                crate::msg!(
                    "log.dictateByVoice",
                    wake = crate::uilang::spoken(&cfg.settings.wake_words, "ривер", "reever")
                ),
            );
            DispatchResult::Done(false)
        }
        Route::Listen => {
            let started = crate::listen::start(app);
            if !started {
                ui_log(app, crate::msg!("log.computer.already"));
            }
            DispatchResult::Done(started)
        }
        Route::Key(candidates) => {
            let parsed: Vec<_> = candidates.iter().map(|candidate| crate::keys::parse(candidate)).collect();
            let keys = match parsed.iter().find_map(|result| result.as_ref().ok()) {
                Some(keys) => keys.clone(),
                None => {
                    let unknown = parsed.into_iter().find_map(Result::err).unwrap_or_default();
                    ui_log(app, crate::msg!("log.key.unknown", name = unknown));
                    return DispatchResult::Done(false);
                }
            };
            let name = crate::keys::describe(&keys);
            match crate::keys::press(&keys) {
                Ok(()) => {
                    ui_log(app, crate::msg!("log.key.pressed", keys = name));
                    DispatchResult::Done(true)
                }
                Err(error) => {
                    ui_log(app, crate::msg!("log.key.failed", keys = name, error = format!("{error:#}")));
                    DispatchResult::Done(false)
                }
            }
        }
        Route::Calc(candidates) => {
            let expression = candidates
                .iter()
                .find_map(|candidate| calc::parse_expression(candidate));
            let Some(expression) = expression else {
                if cfg.settings.private_log {
                    ui_log(app, crate::msg!("log.calc.badExpression.private"));
                } else {
                    ui_log(
                        app,
                        crate::msg!("log.calc.badExpression", expression = candidates.join(" / ")),
                    );
                }
                return DispatchResult::Done(false);
            };

            match calcapp::open_and_type(&expression.keys) {
                Ok(()) => {
                    if cfg.settings.private_log {
                        ui_log(app, crate::msg!("log.calc.private"));
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
                            crate::msg!("log.calc.typeFailed.private"),
                        );
                    } else {
                        ui_log(
                            app,
                            crate::msg!("log.calc.typeFailed", error = format!("{error:#}")),
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
                ui_log(app, crate::msg!("log.offline.webSearch"));
                return DispatchResult::Done(false);
            }
            if cfg.settings.private_log {
                ui_log(app, format!("🔎 {label}"));
            } else {
                ui_log(app, format!("🔎 {label}: {url}"));
            }
            DispatchResult::Done(open_url(cfg, &label, &url, || launcher::shell_open(&url)))
        }
        Route::Find { scope, candidates } => dispatch_find(app, cfg, shared, scope, &candidates),
        Route::WebChat { label, url, text } => {
            if cfg.settings.offline_mode {
                ui_log(app, crate::msg!("log.offline.chat"));
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
                        crate::msg!("log.chat.clipboard"),
                    );
                    DispatchResult::Done(false)
                }
                Err(error) => {
                    ui_log(app, crate::msg!("log.chat.failed", error = format!("{error:#}")));
                    DispatchResult::Done(false)
                }
            }
        }
        Route::Unknown => {
            ui_log(app, crate::msg!("log.unknownCommand"));
            DispatchResult::Done(false)
        }
    }
}

/// «Найди …» и «папка …»: поиск по своему индексу; одно совпадение — открыть, несколько —
/// окно выбора (docs/find.md).
fn dispatch_find(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    scope: crate::find::FindScope,
    candidates: &[String],
) -> DispatchResult {
    if !cfg.settings.local_index {
        ui_log(app, crate::msg!("log.find.indexOff"));
        return DispatchResult::Done(false);
    }
    let query = candidates.join(" / ");
    let line = match (scope, cfg.settings.private_log) {
        (crate::find::FindScope::Folder, true) => crate::msg!("log.find.folder.private"),
        (crate::find::FindScope::Folder, false) => crate::msg!("log.find.folder", query = &query),
        (crate::find::FindScope::File, true) => crate::msg!("log.find.file.private"),
        (crate::find::FindScope::File, false) => crate::msg!("log.find.file", query = &query),
        (crate::find::FindScope::Any, true) => crate::msg!("log.find.any.private"),
        (crate::find::FindScope::Any, false) => crate::msg!("log.find.any", query = &query),
    };
    ui_log(app, line);
    // «… формат X»: имя — до слова «формат», формат — из закрытого словаря (find::FORMATS).
    let mut names: Vec<String> = Vec::new();
    let mut spoken_formats: Vec<String> = Vec::new();
    for candidate in candidates {
        let (name, format) = crate::find::split_format(candidate);
        if !name.trim().is_empty() && !names.contains(&name) {
            names.push(name);
        }
        if let Some(format) = format.filter(|f| !f.trim().is_empty()) {
            spoken_formats.push(format);
        }
    }
    let extension = crate::find::resolve_extension(&spoken_formats);
    if !spoken_formats.is_empty() {
        match &extension {
            Some(ext) => ui_log(app, crate::msg!("log.find.format", ext = ext)),
            None => {
                if cfg.settings.private_log {
                    ui_log(app, crate::msg!("log.find.badFormat.private"));
                } else {
                    ui_log(app, crate::msg!("log.find.badFormat", format = spoken_formats.join(" / ")));
                }
                return DispatchResult::Done(false);
            }
        }
    }
    if names.is_empty() && extension.is_none() {
        ui_log(app, crate::msg!("log.find.empty"));
        return DispatchResult::Done(false);
    }
    let usage = crate::find::Usage::load(&shared.usage_path);
    let recent = crate::find::windows_recent();
    let manager = app.try_state::<crate::local_index::IndexManager>();
    let Some(snapshot) = manager.as_ref().and_then(|manager| manager.snapshot()) else {
        ui_log(app, crate::msg!("log.find.indexBuilding"));
        return DispatchResult::Done(false);
    };
    let items = crate::find::search(
        &snapshot,
        &names,
        scope,
        extension.as_deref(),
        &usage,
        &recent,
    );
    match items.len() {
        0 => {
            ui_log(app, crate::msg!("log.find.nothing"));
            DispatchResult::Done(false)
        }
        1 => DispatchResult::Done(crate::picker::open(app, shared, cfg, &items[0])),
        n => {
            let title = match (&extension, names.first()) {
                (Some(ext), Some(name)) => format!("{name} · .{ext}"),
                (Some(ext), None) => format!(".{ext}"),
                (None, Some(name)) => name.clone(),
                (None, None) => String::new(),
            };
            // Поле поиска в окне начинается с того, что искали голосом, в том же синтаксисе.
            let query = match (&extension, names.first()) {
                (Some(ext), Some(name)) => format!("{name} ext:{ext}"),
                (Some(ext), None) => format!("ext:{ext}"),
                (None, Some(name)) => name.clone(),
                (None, None) => String::new(),
            };
            let _ = crate::picker::show(app, shared, &title, &query, scope, &items);
            DispatchResult::Choose(n)
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
            ui_log(app, crate::msg!("log.proc.notFound.private"));
        } else {
            ui_log(
                app,
                crate::msg!("log.proc.notFound", name = candidates.join(" / ")),
            );
        }
        return DispatchResult::Done(false);
    };

    let title = app_title(&found.app);
    if cfg.settings.private_log {
        ui_log(
            app,
            crate::msg!(
                "log.proc.found.private",
                by = crate::uilang::text(crate::uilang::current(), found.matched_by.key()),
                score = format!("{:.2}", found.score)
            ),
        );
    } else {
        ui_log(
            app,
            crate::msg!(
                "log.proc.found",
                app = &title,
                by = found.matched_by.describe(crate::uilang::current()),
                score = format!("{:.2}", found.score)
            ),
        );
    }

    if kill && crate::procs::is_protected(&found.app.exe_name) {
        if cfg.settings.private_log {
            ui_log(
                app,
                crate::msg!("log.proc.protected.private"),
            );
        } else {
            ui_log(
                app,
                crate::msg!("log.proc.protected", app = &title),
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
                crate::msg!("log.proc.confirm.private"),
            );
        } else {
            ui_log(
                app,
                crate::msg!(
                    "log.proc.confirm",
                    action = pending.describe().render(crate::uilang::current())
                ),
            );
        }
        DispatchResult::Confirm(pending)
    } else {
        DispatchResult::Done(execute_pending(app, &pending))
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
            ui_log(app, crate::msg!("log.openFolder", target = &target));
            launch_with_raise(cfg, &hint, || {
                launcher::open_folder(Path::new(&target))
            })
        }
        "uwp" => {
            ui_log(app, format!("▶ UWP: {target}"));
            launch_with_raise(cfg, &hint, || launcher::launch_uwp(&target))
        }
        _ if winutil::is_web_url(&target) => {
            ui_log(app, crate::msg!("log.openUrl", target = &target));
            open_url(cfg, &hint, &target, || launcher::shell_open(&target))
        }
        _ => {
            ui_log(app, crate::msg!("log.launch", target = &target));
            launch_with_raise(cfg, &hint, || launcher::shell_open(&target))
        }
    }
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
    fn listen_routes_from_the_grammar() {
        let grammar = Grammar::defaults();
        let actions = actions();
        for text in ["слушай", "послушай звук компьютера", "listen"] {
            assert!(matches!(route(text, &grammar, &actions, THRESHOLD), Route::Listen), "{text}");
        }
        // «слушай, открой …» — вводное слово, команда — открыть
        assert!(!matches!(route("слушай открой фотошоп", &grammar, &actions, THRESHOLD), Route::Listen));
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
            Route::NoAction("абракадабра".into())
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
            Route::NoAction("абракадабра".into())
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
            (
                "гемини какая погода будет завтра в москве",
                "Какая погода будет завтра в москве",
            ),
            (
                "гимини какая погода будет завтра в москве",
                "Какая погода будет завтра в москве",
            ),
            (
                "джимини обясни что такое квантовый компьютер",
                "Обясни что такое квантовый компьютер",
            ),
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
            assert!(
                grammar.is_incomplete(heard),
                "{heard} — ждать вопрос после паузы"
            );
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

    #[test]
    fn find_phrases_pick_the_scope_by_the_longest_trigger() {
        let grammar = Grammar::defaults();
        let actions = Vec::new();
        use crate::find::FindScope;
        for (heard, scope, query) in [
            (
                "найди папку проджект джи эс ти",
                FindScope::Folder,
                "проджект джи эс ти",
            ),
            ("найди project gst", FindScope::Any, "project gst"),
            ("найди файл отчет", FindScope::File, "отчет"),
            ("поищи папку загрузки", FindScope::Folder, "загрузки"),
        ] {
            match route(heard, &grammar, &actions, THRESHOLD) {
                Route::Find {
                    scope: got,
                    candidates,
                } => {
                    assert_eq!(got, scope, "{heard}");
                    assert_eq!(candidates[0], query, "{heard}");
                }
                other => panic!("{heard}: {other:?}"),
            }
        }
    }

    #[test]
    fn search_payload_language_is_chosen_by_dictionary() {
        let grammar = Grammar::defaults();
        let actions = Vec::new();
        let url = |text: &str, alt: &str| match super::route(text, Some(alt), &grammar, &actions, THRESHOLD) {
            Route::Search { url, .. } => url,
            other => panic!("{other:?}"),
        };
        // английский с акцентом: русская модель дала транслит
        assert!(url("загугли плиз ревью зе файнал скеджул", "загугли please review the final schedule")
            .contains("please+review"));
        // русская фраза: английская модель выдумала связную фразу — остаётся русский
        assert!(!url("загугли погода в москве", "загугли i think it s a good idea").contains("think"));
    }
}
