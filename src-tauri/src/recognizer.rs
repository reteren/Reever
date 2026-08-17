use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use strsim::jaro_winkler;
use tauri::{AppHandle, Emitter};
use vosk::{CompleteResult, DecodingState, Model, Recognizer};

use crate::actions::{Action, SharedState};
use crate::audio::{start_capture, Player};
use crate::commands::{parse, ReeverCommand};
use crate::config::ReeverConfig;
use crate::everything::{self, SearchOutcome};
use crate::{launcher, system, winutil};

#[derive(Clone)]
enum Mode {
    Idle,
    Command,
    Calibrating(String),
    Confirm(String), // op системной команды, ждём «да»/«отмена»
}

enum DispatchResult {
    Done(bool),
    Confirm(String),
}

const WAKE_WORD: &str = "ривер";

fn ui_log(app: &AppHandle, msg: impl AsRef<str>) {
    let m = msg.as_ref();
    // stdout — только в dev; в релизе ничего не печатаем (нет утечки в консоль).
    #[cfg(debug_assertions)]
    println!("[reever] {m}");
    let _ = app.emit("reever://log", m.to_string());
}

/// Затирает аудио-буфер нулями перед очисткой — сырой голос не остаётся в RAM.
fn wipe(buf: &mut Vec<i16>) {
    for s in buf.iter_mut() {
        *s = 0;
    }
    buf.clear();
}

fn set_state(app: &AppHandle, s: &str) {
    let _ = app.emit("reever://state", s);
}

pub fn run_worker(app: AppHandle, cfg: Arc<ReeverConfig>, shared: Arc<SharedState>) {
    if let Err(e) = run_inner(&app, &cfg, &shared) {
        eprintln!("[reever] worker fatal: {e:#}");
        let _ = app.emit("reever://state", "error");
        let _ = app.emit("reever://log", format!("✖ FATAL: {e}"));
    }
}

fn exists(p: &std::path::Path) -> &'static str {
    if p.exists() { "ok" } else { "ОТСУТСТВУЕТ" }
}

fn run_inner(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>) -> anyhow::Result<()> {
    let player = Player::new()?;

    ui_log(app, format!("RU model: {} [{}]", cfg.model_dir.display(), exists(&cfg.model_dir)));
    ui_log(app, format!("EN model: {} [{}]", cfg.en_model_dir.display(), exists(&cfg.en_model_dir)));
    let host = cfg.settings.everything_host.clone();
    let port = cfg.settings.everything_port;
    let wake_words = &cfg.settings.wake_words;
    let wake_enabled = cfg.settings.activation_mode != "hotkey";
    ui_log(app, format!("режим активации: {}", cfg.settings.activation_mode));
    if cfg.settings.offline_mode {
        ui_log(app, "🔒 офлайн-режим: веб-поиск и Everything HTTP отключены (нулевой сетевой трафик)");
    } else {
        ui_log(app, format!("everything: http://{host}:{port}"));
        let mut http_ok = false;
        for _ in 0..5 {
            if everything::ping(&host, port).is_ok() {
                http_ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
        ui_log(app, if http_ok {
            "✓ Everything HTTP доступен"
        } else {
            "⚠ Everything HTTP НЕ доступен — для «открой папку» поставь Everything 1.5a + HTTP-плагин"
        });
    }

    let ru_model = Model::new(cfg.model_dir.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не удалось загрузить RU-модель"))?;
    let en_model = if cfg.settings.english_enabled && cfg.en_model_dir.exists() {
        Model::new(cfg.en_model_dir.to_string_lossy().as_ref())
    } else {
        None
    };
    ui_log(app, if en_model.is_some() { "✓ EN-модель загружена (on-demand)" } else { "EN-модель выкл." });

    let mut wake_rec = Recognizer::new(&ru_model, 16_000.0)
        .ok_or_else(|| anyhow::anyhow!("wake recognizer init failed"))?;
    wake_rec.set_words(false);
    let mut cmd_rec = Recognizer::new(&ru_model, 16_000.0)
        .ok_or_else(|| anyhow::anyhow!("command recognizer init failed"))?;
    cmd_rec.set_words(false);
    let mut en_rec = en_model.as_ref().and_then(|m| Recognizer::new(m, 16_000.0));
    if let Some(r) = en_rec.as_mut() {
        r.set_words(false);
    }

    let (tx, rx) = sync_channel::<Vec<i16>>(32);
    // Поток микрофона держим в Option: на паузе/мьюте реально закрываем его
    // (дроп Stream останавливает захват → системный индикатор микрофона гаснет).
    let mut stream = Some(start_capture(tx.clone())?);

    let mut mode = Mode::Idle;
    let mut buffer: Vec<i16> = Vec::new();
    let mut endp = Endpointer::new();
    // быстрый порог для команды; для одиночного «ривер» — длиннее (не режем слитную фразу)
    let quiet_cmd = Duration::from_millis(cfg.settings.endpoint_ms.max(80));
    let quiet_wake = quiet_cmd + Duration::from_millis(300);
    set_state(app, "idle");
    ui_log(app, format!("✓ готов. «{WAKE_WORD}» / хоткей + команда. «отмена» — стоп."));

    loop {
        // Пауза/мьют: освобождаем микрофон, чистим состояние, ждём снятия.
        if shared.paused.load(Ordering::Relaxed) {
            if stream.is_some() {
                stream = None; // дроп Stream -> захват остановлен
                wipe(&mut buffer);
                wake_rec.reset();
                cmd_rec.reset();
                mode = Mode::Idle;
                set_state(app, "paused");
                ui_log(app, "🔇 микрофон отключён (поток закрыт)");
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
            continue;
        }
        if stream.is_none() {
            match start_capture(tx.clone()) {
                Ok(s) => {
                    stream = Some(s);
                    set_state(app, "idle");
                    ui_log(app, "🔊 микрофон включён");
                }
                Err(e) => {
                    ui_log(app, format!("⚠ микрофон недоступен: {e}"));
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    continue;
                }
            }
        }

        // recv с таймаутом — чтобы реагировать на паузу даже без аудио.
        let chunk = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(c) => c,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if matches!(mode, Mode::Idle) {
            // калибровка
            let pending = shared.calibrate.lock().ok().and_then(|g| g.clone());
            if let Some(id) = pending {
                set_state(app, "listening");
                ui_log(app, "● калибровка — скажи фразу для действия…");
                player.play(&cfg.sound("start.wav"));
                cmd_rec.reset();
                wipe(&mut buffer);
                mode = Mode::Calibrating(id);
                continue;
            }
            // push-to-talk по хоткею
            if shared.force_listen.swap(false, Ordering::Relaxed) {
                set_state(app, "listening");
                ui_log(app, "● хоткей — слушаю команду…");
                player.play(&cfg.sound("start.wav"));
                cmd_rec.reset();
                wipe(&mut buffer);
                mode = Mode::Command;
                continue;
            }
        }

        match mode.clone() {
            Mode::Idle => {
                if !wake_enabled {
                    // режим только-хоткей: эфир не слушаем (активация — через force_listen)
                    wake_rec.reset();
                    endp.reset();
                } else {
                    // буферим фразу целиком, чтобы «ривер открой X» одним духом не терялось
                    buffer.extend_from_slice(&chunk);
                    if let Some(text) =
                        feed_wake(&mut wake_rec, &chunk, &mut endp, quiet_cmd, quiet_wake, wake_words)
                    {
                        #[cfg(debug_assertions)]
                        if !text.is_empty() && !cfg.settings.private_log {
                            println!("[wake] heard: {text:?}");
                        }
                        match strip_wake(&text, wake_words) {
                            // «ривер <команда>» одним духом — выполняем сразу
                            Some(cmd) if !cmd.trim().is_empty() => {
                                let en = decode_en(en_rec.as_mut(), &buffer)
                                    .map(|e| strip_first_word(&e));
                                let next = process_command(
                                    app, cfg, shared, &player, cmd.trim(), en.as_deref(),
                                );
                                cmd_rec.reset();
                                wipe(&mut buffer);
                                wake_rec.reset();
                                if let Mode::Confirm(op) = next {
                                    mode = Mode::Confirm(op);
                                } else {
                                    mode = Mode::Idle;
                                    set_state(app, "idle");
                                }
                            }
                            // сказали только «ривер» — ждём команду отдельной фразой
                            Some(_) => {
                                set_state(app, "listening");
                                ui_log(app, "● слушаю команду…");
                                player.play(&cfg.sound("start.wav"));
                                cmd_rec.reset();
                                wipe(&mut buffer);
                                mode = Mode::Command;
                            }
                            // wake не услышан — сброс
                            None => {
                                wake_rec.reset();
                                wipe(&mut buffer);
                            }
                        }
                    } else if buffer.len() > 16_000 * 12 {
                        // защита от разрастания при долгом непрерывном шуме
                        wake_rec.reset();
                        wipe(&mut buffer);
                        endp.reset();
                    }
                }
            }

            Mode::Command => {
                buffer.extend_from_slice(&chunk);
                if let Some(ru) = feed_until_end(&mut cmd_rec, &chunk, &mut endp, quiet_cmd) {
                    let en = decode_en(en_rec.as_mut(), &buffer);
                    let next = process_command(app, cfg, shared, &player, &ru, en.as_deref());
                    cmd_rec.reset();
                    wipe(&mut buffer);
                    wake_rec.reset();
                    if let Mode::Confirm(op) = next {
                        mode = Mode::Confirm(op);
                    } else {
                        mode = Mode::Idle;
                        set_state(app, "idle");
                    }
                }
            }

            Mode::Confirm(op) => {
                buffer.extend_from_slice(&chunk);
                if let Some(ru) = feed_until_end(&mut cmd_rec, &chunk, &mut endp, quiet_cmd) {
                    if cfg.settings.private_log {
                        ui_log(app, "📝 подтверждение получено (приватный режим)");
                    } else {
                        ui_log(app, format!("📝 подтверждение: «{ru}»"));
                    }
                    let yes = ["да", "ага", "точно", "давай", "подтверждаю", "подтверди"]
                        .iter()
                        .any(|w| ru.split_whitespace().any(|x| x == *w));
                    if yes {
                        ui_log(app, format!("⚙ выполняю: {op}"));
                        let ok = system::run_system(&op);
                        player.play(&cfg.sound(if ok { "success.wav" } else { "error.wav" }));
                    } else {
                        ui_log(app, "↩ отменено");
                        player.play(&cfg.sound("error.wav"));
                    }
                    wipe(&mut buffer);
                    wake_rec.reset();
                    mode = Mode::Idle;
                    set_state(app, "idle");
                }
            }

            Mode::Calibrating(id) => {
                // завершено из UI («Готово» -> cancel_calibration сбросил флаг)?
                let active = shared.calibrate.lock().ok().and_then(|g| g.clone());
                if active.as_deref() != Some(id.as_str()) {
                    ui_log(app, "✓ калибровка завершена");
                    wipe(&mut buffer);
                    wake_rec.reset();
                    endp.reset();
                    mode = Mode::Idle;
                    set_state(app, "idle");
                } else {
                    buffer.extend_from_slice(&chunk);
                    if let Some(ru) = feed_until_end(&mut cmd_rec, &chunk, &mut endp, quiet_cmd) {
                        let en = decode_en(en_rec.as_mut(), &buffer);
                        capture_calibration(app, shared, &id, &ru, en.as_deref());
                        if !ru.is_empty() || en.is_some() {
                            player.play(&cfg.sound("success.wav"));
                        }
                        cmd_rec.reset();
                        wipe(&mut buffer);
                        // остаёмся в калибровке — можно говорить фразы подряд
                    }
                }
            }
        }
    }
    Ok(())
}

fn feed(rec: &mut Recognizer, chunk: &[i16]) -> DecodingState {
    rec.accept_waveform(chunk).unwrap_or(DecodingState::Failed)
}

/// Быстрый детектор конца фразы: если partial не меняется `quiet` — финализируем
/// раньше внутреннего endpoint Vosk (короче пауза → быстрее реакция).
struct Endpointer {
    last: String,
    since: Instant,
}
impl Endpointer {
    fn new() -> Self {
        Self { last: String::new(), since: Instant::now() }
    }
    fn reset(&mut self) {
        self.last.clear();
        self.since = Instant::now();
    }
    fn stable(&mut self, partial: &str, quiet: Duration) -> bool {
        if partial != self.last {
            self.last = partial.to_string();
            self.since = Instant::now();
            return false;
        }
        !partial.is_empty() && self.since.elapsed() >= quiet
    }
}

/// Скармливает чанк и возвращает Some(текст), когда фраза закончилась — либо Vosk
/// финализировал сам, либо partial стабилен `quiet` (наш быстрый endpoint).
fn feed_until_end(
    rec: &mut Recognizer,
    chunk: &[i16],
    endp: &mut Endpointer,
    quiet: Duration,
) -> Option<String> {
    if matches!(feed(rec, chunk), DecodingState::Finalized) {
        endp.reset();
        return Some(result_text(&rec.result()));
    }
    let partial = rec.partial_result().partial.trim().to_lowercase();
    if endp.stable(&partial, quiet) {
        endp.reset();
        Some(result_text(&rec.final_result()))
    } else {
        None
    }
}

/// Wake-фаза с адаптивным порогом: пока после «ривер» нет команды — ждём дольше
/// (`quiet_wake`), чтобы слитное «ривер открой X» не разрезалось на микропаузе после
/// wake-слова; как только команда в partial появилась — быстрый порог `quiet_cmd`.
fn feed_wake(
    rec: &mut Recognizer,
    chunk: &[i16],
    endp: &mut Endpointer,
    quiet_cmd: Duration,
    quiet_wake: Duration,
    wake_words: &[String],
) -> Option<String> {
    if matches!(feed(rec, chunk), DecodingState::Finalized) {
        endp.reset();
        return Some(result_text(&rec.result()));
    }
    let partial = rec.partial_result().partial.trim().to_lowercase();
    let has_cmd = strip_wake(&partial, wake_words).is_some_and(|c| !c.trim().is_empty());
    let quiet = if has_cmd { quiet_cmd } else { quiet_wake };
    if endp.stable(&partial, quiet) {
        endp.reset();
        Some(result_text(&rec.final_result()))
    } else {
        None
    }
}

fn result_text(res: &CompleteResult) -> String {
    match res {
        CompleteResult::Single(s) => s.text.to_string(),
        CompleteResult::Multiple(m) => m
            .alternatives
            .first()
            .map(|a| a.text.to_string())
            .unwrap_or_default(),
    }
    .trim()
    .to_lowercase()
}

fn decode_en(en_rec: Option<&mut Recognizer>, buffer: &[i16]) -> Option<String> {
    let rec = en_rec?;
    rec.reset();
    let _ = rec.accept_waveform(buffer);
    let text = result_text(&rec.final_result());
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Находит wake-слово в тексте и возвращает ВСЁ после него как команду.
/// `Some("")` — сказали только «ривер» (ждём команду отдельной фразой);
/// `Some("открой X")` — «ривер открой X» одним духом; `None` — wake не услышан.
fn strip_wake(text: &str, wake_words: &[String]) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if wake_words.iter().any(|ww| ww == w) {
            return Some(words[i + 1..].join(" "));
        }
    }
    None
}

/// Убирает первое слово (эквивалент wake-слова) из EN-транскрипта, чтобы
/// кандидат для матчинга совпадал с командной частью (как в двухфазном режиме).
fn strip_first_word(s: &str) -> String {
    s.split_whitespace().skip(1).collect::<Vec<_>>().join(" ")
}

/// Обработка распознанной команды: отмена / диспатч / запрос подтверждения.
/// Возвращает следующий режим воркера (Idle или Confirm).
fn process_command(
    app: &AppHandle,
    cfg: &ReeverConfig,
    shared: &Arc<SharedState>,
    player: &Player,
    ru: &str,
    en: Option<&str>,
) -> Mode {
    if cfg.settings.private_log {
        ui_log(app, "📝 распознано (приватный режим)");
    } else {
        ui_log(app, format!("📝 RU: «{ru}»"));
        if let Some(en) = en {
            ui_log(app, format!("📝 EN: «{en}»"));
        }
    }
    if is_cancel(ru, &cfg.settings.cancel_words) {
        ui_log(app, "↩ отменено");
        player.play(&cfg.sound("error.wav"));
        return Mode::Idle;
    }
    player.play(&cfg.sound("received.wav"));
    set_state(app, "busy");
    match dispatch(app, cfg, shared, ru, en) {
        DispatchResult::Done(ok) => {
            player.play(&cfg.sound(if ok { "success.wav" } else { "error.wav" }));
            Mode::Idle
        }
        DispatchResult::Confirm(op) => {
            player.play(&cfg.sound("received.wav"));
            set_state(app, "listening");
            Mode::Confirm(op)
        }
    }
}

fn is_cancel(text: &str, cancel_words: &[String]) -> bool {
    text.split_whitespace().any(|w| cancel_words.iter().any(|c| c == w))
}

fn capture_calibration(app: &AppHandle, shared: &Arc<SharedState>, id: &str, ru: &str, en: Option<&str>) {
    let mut captured: Vec<String> = Vec::new();
    if !ru.trim().is_empty() {
        captured.push(ru.trim().to_lowercase());
    }
    if let Some(en) = en {
        let en = en.trim().to_lowercase();
        if !en.is_empty() && !captured.contains(&en) {
            captured.push(en);
        }
    }
    if captured.is_empty() {
        ui_log(app, "⚠ калибровка: ничего не распознано, повтори");
        let _ = app.emit("reever://calibrated", id.to_string());
        return;
    }
    {
        let mut actions = shared.actions.lock().unwrap();
        if let Some(a) = actions.iter_mut().find(|a| a.id == id) {
            for c in &captured {
                if !a.phrases.iter().any(|p| p.eq_ignore_ascii_case(c)) {
                    a.phrases.push(c.clone());
                }
            }
        }
    }
    shared.persist();
    ui_log(app, format!("✓ калибровка «{id}»: +{captured:?}"));
    let _ = app.emit("reever://calibrated", id.to_string());
}

// ------- диспетчер -------

fn dispatch(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>, ru_text: &str, en_text: Option<&str>) -> DispatchResult {
    // Явная команда «запусти/открой/найди X» — намерение запустить ДЕЙСТВИЕ, а не
    // систему. Системные матчим ТОЛЬКО без глагола-запуска (выключи/пауза/плей/…),
    // иначе «запусти дота два» ложно ловится фаззи на «плей»/«пауза».
    let explicit_launch = parse(ru_text).is_some();
    // 1) системные команды (только если нет глагола-запуска)
    if !explicit_launch {
        if let Some((action, score)) = match_kind(shared, cfg, "system", ru_text, en_text) {
            let op = action.target.clone();
            if system::is_dangerous(&op) && cfg.settings.confirm_dangerous {
                ui_log(app, format!("⚙ {} — подтверди голосом: «да» / «отмена»", action.label));
                return DispatchResult::Confirm(op);
            }
            ui_log(app, format!("⚙ система: {} (score {score:.2})", action.label));
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
fn try_search(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>, ru_text: &str) -> Option<bool> {
    if cfg.settings.offline_mode {
        return None; // офлайн: веб-поиск отключён
    }
    let words: Vec<&str> = ru_text.split_whitespace().collect();
    if words.len() < 2 {
        return None; // нужен триггер + хотя бы 1 слово запроса
    }
    let found: Option<(String, String)> = {
        let actions = shared.actions.lock().unwrap();
        let mut res = None;
        'outer: for a in actions.iter().filter(|a| a.kind == "search") {
            for p in &a.phrases {
                let ph = p.trim().to_lowercase().replace(' ', "");
                if ph.is_empty() {
                    continue;
                }
                let pw = p.split_whitespace().count().max(1);
                let max_take = (pw + 1).min(words.len() - 1);
                for take in 1..=max_take {
                    // склеиваем первые `take` слов без пробелов и fuzzy-сравниваем с фразой
                    let prefix = words[..take].concat().to_lowercase();
                    if jaro_winkler(&prefix, &ph) >= 0.84 {
                        let query = words[take..].join(" ");
                        if !query.trim().is_empty() {
                            res = Some((a.target.replace("{q}", &urlencode(query.trim())), a.label.clone()));
                            break 'outer;
                        }
                    }
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
    Some(launch_with_raise(cfg, &label, || launcher::shell_open(&url)))
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Матч по действиям заданного типа (full-text fuzzy) — для system.
fn match_kind(shared: &Arc<SharedState>, cfg: &ReeverConfig, kind: &str, ru_text: &str, en_text: Option<&str>) -> Option<(Action, f64)> {
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

fn dispatch_verbs(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>, ru_text: &str, en_text: Option<&str>) -> bool {
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
        Some("папку") | Some("папка") | Some("папке") => Some(it.collect::<Vec<_>>().join(" ")),
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
fn launch_intent(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>, target: &str, en_text: Option<&str>) -> bool {
    if let Some((action, score)) = match_action(shared, cfg, target, en_text) {
        ui_log(app, format!("↪ действие «{}» (score {score:.2})", action.label));
        return run_action(app, cfg, &action);
    }
    if cfg.settings.launch_everything_fallback && !cfg.settings.offline_mode {
        let q = cfg.aliases.app(target).unwrap_or_else(|| target.to_string());
        if cfg.settings.private_log {
            ui_log(app, "🔎 (fallback) поиск программ…");
        } else {
            ui_log(app, format!("🔎 (fallback) поиск программ: «{q}»"));
        }
        let o = everything::find_executables(&cfg.settings.everything_host, cfg.settings.everything_port, &q);
        handle_launch(app, cfg, o, &q)
    } else {
        ui_log(app, "∅ нет такого действия (добавь и откалибруй)");
        false
    }
}

/// Матч по личным действиям (исключая системные/поисковые).
fn match_action(shared: &Arc<SharedState>, cfg: &ReeverConfig, target: &str, en_text: Option<&str>) -> Option<(Action, f64)> {
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
    let hint = if action.label.trim().is_empty() { target.clone() } else { action.label.clone() };
    match action.kind.as_str() {
        "folder" => {
            ui_log(app, format!("📁 открываю: {target}"));
            launch_with_raise(cfg, &hint, || everything::open_folders(&[PathBuf::from(&target)]) > 0)
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

fn open_via_everything(app: &AppHandle, cfg: &ReeverConfig, ru_target: &str, en_text: Option<&str>) -> bool {
    if cfg.settings.offline_mode {
        ui_log(app, "🔒 офлайн-режим: поиск папок (Everything) отключён");
        return false;
    }
    let host = &cfg.settings.everything_host;
    let port = cfg.settings.everything_port;

    let q = cfg.aliases.folder(ru_target).unwrap_or_else(|| ru_target.to_string());
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
    let base = best.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();

    let sim = jaro_winkler(&probe.to_lowercase(), &base);
    if sim < cfg.settings.open_min_similarity {
        ui_log(app, format!("∅ не уверен ({sim:.2} < {:.2}) — пропускаю «{}»", cfg.settings.open_min_similarity, best.display()));
        return false;
    }

    ui_log(app, format!("📁 {}", best.display()));
    if o.paths.len() > 1 {
        ui_log(app, format!("(+{} др. — открываю лучшее)", o.paths.len() - 1));
    }
    let hint = base;
    launch_with_raise(cfg, &hint, || everything::open_folders(std::slice::from_ref(&best)) > 0)
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
