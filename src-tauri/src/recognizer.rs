//! Аудио-воркер: склейка Engine (чистый автомат) с Tauri — микрофон, звуки,
//! события интерфейса, диспетчер. Вся логика «когда фраза закончилась и что в ней
//! сказано» — в engine.rs; здесь только побочные эффекты.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::actions::SharedState;
use crate::asr::{Asr, AsrConfig};
use crate::audio::{start_capture, Player};
use crate::bilingual::LangMode;
use crate::config::ReeverConfig;
use crate::dictation::{pieces_to_string, Formatter};
use crate::dispatch::{dispatch, execute_pending, DispatchResult, PendingAction};
use crate::engine::{DictationEndReason, Engine, EngineConfig, EngineEvent};
use crate::events::{set_state, ui_log};
use crate::langid::SpeechLid;
use crate::speech::LanguageId;
use crate::vad::SileroVad;
use crate::{everything, textnorm, typing};

/// Очередь от realtime-колбэка микрофона. Распознавание идёт синхронно в этом
/// потоке, поэтому запас ~5 с: длинная фраза в простое не должна терять аудио,
/// пока модель её разбирает (колбэк при переполнении молча выбрасывает чанки).
const AUDIO_QUEUE: usize = 512;

pub fn run_worker(app: AppHandle, cfg: Arc<ReeverConfig>, shared: Arc<SharedState>) {
    if let Err(e) = run_inner(&app, &cfg, &shared) {
        eprintln!("[reever] worker fatal: {e:#}");
        set_state(&app, "error");
        ui_log(&app, format!("✖ FATAL: {e}"));
    }
}

/// Сводка для плиток Главной (вместо разбора текста лога во фронтенде). Общая для
/// воркера и фоновой проверки Everything, поэтому каждый emit шлёт полный снимок.
#[derive(Clone)]
struct Status {
    asr: &'static str,
    model: String,
    everything: Option<bool>,
}

fn emit_status(app: &AppHandle, status: &Mutex<Status>) {
    let Ok(st) = status.lock() else { return };
    let _ = app.emit(
        "reever://status",
        json!({ "asr": st.asr, "model": st.model, "everything": st.everything }),
    );
}

fn set_asr_status(app: &AppHandle, status: &Mutex<Status>, asr: &'static str) {
    if let Ok(mut st) = status.lock() {
        st.asr = asr;
    }
    emit_status(app, status);
}

/// Проверка Everything в фоне: без него до 4 с ретраев, и ждать их, прежде чем
/// грузить модель и слушать микрофон, незачем.
fn spawn_everything_probe(app: &AppHandle, cfg: &ReeverConfig, status: &Arc<Mutex<Status>>) {
    let (app, status) = (app.clone(), status.clone());
    let (host, port) = (
        cfg.settings.everything_host.clone(),
        cfg.settings.everything_port,
    );
    ui_log(&app, format!("everything: http://{host}:{port}"));
    std::thread::spawn(move || {
        let ok = (0..5).any(|i| {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(800));
            }
            everything::ping(&host, port).is_ok()
        });
        if let Ok(mut st) = status.lock() {
            st.everything = Some(ok);
        }
        emit_status(&app, &status);
        ui_log(
            &app,
            if ok {
                "✓ Everything HTTP доступен"
            } else {
                "⚠ Everything HTTP НЕ доступен — для «открой папку» поставь Everything 1.5a + HTTP-сервер"
            },
        );
    });
}

fn run_inner(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>) -> anyhow::Result<()> {
    let s = &cfg.settings;
    let player = Player::new()?;
    let status = Arc::new(Mutex::new(Status {
        asr: "loading",
        model: s.asr_model.clone(),
        everything: None,
    }));
    set_state(app, "loading");
    emit_status(app, &status);
    ui_log(app, format!("режим активации: {}", s.activation_mode));

    if s.offline_mode {
        ui_log(
            app,
            "🔒 офлайн-режим: веб-поиск и Everything HTTP отключены (нулевой сетевой трафик)",
        );
    } else {
        spawn_everything_probe(app, cfg, &status);
    }

    ui_log(
        app,
        format!(
            "загрузка модели распознавания: {}",
            cfg.asr_model_dir.display()
        ),
    );
    let asr = Asr::new(&AsrConfig {
        model_dir: cfg.asr_model_dir.clone(),
        num_threads: s.asr_threads.max(1) as i32,
    })
    .inspect_err(|_| set_asr_status(app, &status, "error"))?;
    let vad = SileroVad::new(&cfg.vad_model_path, s.vad_threshold)
        .inspect_err(|_| set_asr_status(app, &status, "error"))?;
    let mut engine = Engine::new(
        EngineConfig {
            wake_words: s.wake_words.clone(),
            wake_enabled: s.activation_mode != "hotkey",
            wake_pause_ms: s.wake_pause_ms,
            command_pause_ms: s.command_pause_ms,
            command_wait_ms: s.command_wait_ms,
            signal_mute_ms: 0,
            partial_interval_ms: if s.partial_results { 400 } else { 0 },
            dictation_pause_ms: s.dictation_pause_ms.clamp(300, 5_000),
            dictation_idle_ms: s.dictation_idle_ms.clamp(2_000, 600_000),
        },
        asr,
        vad,
    );
    enable_second_language(app, cfg, &mut engine);
    set_asr_status(app, &status, "ready");

    let (tx, rx) = sync_channel::<Vec<i16>>(AUDIO_QUEUE);
    // Поток микрофона держим в Option: на паузе/мьюте реально закрываем его
    // (дроп Stream останавливает захват → системный индикатор микрофона гаснет).
    let mut stream = Some(start_capture(tx.clone())?);
    let mut worker = Worker {
        app,
        cfg,
        shared,
        player: &player,
        confirm_op: None,
        calibrating: None,
        cancel_words: cfg.settings.cancel_words.clone(),
        grammar_version: None,
        dictation: None,
    };
    set_state(app, "idle");
    ui_log(app, "✓ готов. «ривер» — и команду сразу или после сигнала. Хоткей — то же без «ривер». «отмена» — стоп.");

    loop {
        if shared.paused.load(Ordering::Relaxed) {
            if stream.is_some() {
                stream = None;
                engine.reset();
                worker.confirm_op = None;
                worker.calibrating = None;
                worker.dictation = None;
                // Накопившееся в очереди — тоже голос; выбрасываем, не распознавая.
                while rx.try_recv().is_ok() {}
                set_state(app, "paused");
                ui_log(app, "🔇 микрофон отключён (поток закрыт)");
            }
            std::thread::sleep(Duration::from_millis(150));
            continue;
        }
        if stream.is_none() {
            match start_capture(tx.clone()) {
                Ok(st) => {
                    stream = Some(st);
                    set_state(app, "idle");
                    ui_log(app, "🔊 микрофон включён");
                }
                Err(e) => {
                    ui_log(app, format!("⚠ микрофон недоступен: {e}"));
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }

        // recv с таймаутом — чтобы реагировать на паузу даже без аудио.
        let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        worker.sync_grammar(&mut engine);
        worker.poll_triggers(&mut engine);
        for ev in engine.push_audio(&chunk) {
            worker.handle(&mut engine, ev);
        }
    }
    Ok(())
}

/// Второй язык цели команды («ривер поиск ютуб never gonna give you up»): вторая модель
/// распознаёт вырезанную цель, определитель языка выбирает вариант (docs/bilingual.md).
/// Нет модели на диске — не фатально: работаем только по-русски и пишем об этом в лог.
fn enable_second_language<T, V>(app: &AppHandle, cfg: &ReeverConfig, engine: &mut Engine<T, V>)
where
    T: crate::speech::Transcriber,
    V: crate::speech::VoiceDetector,
{
    let s = &cfg.settings;
    if !s.english_enabled {
        ui_log(app, "второй язык выключен — только русский");
        return;
    }
    let second = match Asr::new(&AsrConfig {
        model_dir: cfg.second_model_dir.clone(),
        num_threads: s.asr_threads.max(1) as i32,
    }) {
        Ok(asr) => asr,
        Err(e) => {
            ui_log(app, format!("⚠ второй язык недоступен: {e:#}"));
            return;
        }
    };
    let lid: Option<Box<dyn LanguageId + Send>> = match &cfg.lid_model_dir {
        Some(dir) => match SpeechLid::new(dir, 1) {
            Ok(lid) => Some(Box::new(lid)),
            Err(e) => {
                ui_log(
                    app,
                    format!("⚠ определитель языка недоступен ({e:#}) — только эвристика"),
                );
                None
            }
        },
        None => None,
    };
    let mode = match s.payload_language.as_str() {
        "ru" => LangMode::Ru,
        "en" => LangMode::En,
        _ => LangMode::Auto,
    };
    ui_log(
        app,
        format!(
            "✓ второй язык: {} · определитель: {} · язык цели: {}",
            s.second_asr_model,
            if lid.is_some() {
                s.lid_model.as_str()
            } else {
                "эвристика"
            },
            s.payload_language
        ),
    );
    engine.set_bilingual(Some(Box::new(second)), lid, mode);
}

/// Побочные эффекты событий Engine. Отдельная структура, чтобы не таскать пять
/// параметров через каждую функцию.
struct Worker<'a> {
    app: &'a AppHandle,
    cfg: &'a ReeverConfig,
    shared: &'a Arc<SharedState>,
    player: &'a Player,
    /// Опасная системная операция, ждущая голосового «да».
    confirm_op: Option<PendingAction>,
    /// id действия, которое сейчас калибруется.
    calibrating: Option<String>,
    /// Слова отмены из грамматики (редактор меняет их на лету).
    cancel_words: Vec<String>,
    /// Версия грамматики, уже переданная в Engine; `None` — ещё ни разу.
    grammar_version: Option<u64>,
    /// Идёт диктовка («ривер напиши»).
    dictation: Option<Dictation>,
}

/// Состояние одной диктовки: оформление текста и то, что ушло в буфер обмена.
#[derive(Default)]
struct Dictation {
    formatter: Formatter,
    /// Текст, для которого не нашлось поля ввода; лежит в буфере обмена целиком.
    clipboard: String,
    typed_chars: usize,
}

impl Worker<'_> {
    fn sound(&self, name: &str) {
        self.player.play(&self.cfg.sound(name));
    }

    fn private(&self) -> bool {
        self.cfg.settings.private_log
    }

    /// Грамматику редактирует UI-поток; сверка атомика на каждом чанке дешёвая, а клон
    /// под мьютексом — только когда версия действительно сменилась.
    fn sync_grammar<T, V>(&mut self, engine: &mut Engine<T, V>)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        let version = self.shared.grammar_version.load(Ordering::Relaxed);
        if self.grammar_version == Some(version) {
            return;
        }
        let Some(grammar) = self.shared.grammar.lock().ok().map(|g| g.clone()) else {
            return;
        };
        if !grammar.cancel_words.is_empty() {
            self.cancel_words = grammar.cancel_words.clone();
        }
        if self.grammar_version.is_some() {
            ui_log(self.app, "✓ фразы команд обновлены");
        }
        engine.set_grammar(grammar);
        self.grammar_version = Some(version);
    }

    /// Внешние триггеры из UI/хоткеев: калибровка и push-to-talk.
    fn poll_triggers<T, V>(&mut self, engine: &mut Engine<T, V>)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        let pending = self.shared.calibrate.lock().ok().and_then(|g| g.clone());
        match (&self.calibrating, pending) {
            (None, Some(id)) => {
                ui_log(
                    self.app,
                    "● калибровка — говори фразу для действия (можно несколько подряд)…",
                );
                self.sound("start.wav");
                engine.start_calibration();
                self.calibrating = Some(id);
            }
            // «Готово» в UI (cancel_calibration сбросил флаг) или калибруют другое действие.
            (Some(cur), p) if p.as_deref() != Some(cur.as_str()) => {
                engine.stop_calibration();
                self.calibrating = None;
                ui_log(self.app, "✓ калибровка завершена");
                set_state(self.app, "idle");
            }
            _ => {}
        }
        // Хоткей в калибровке/подтверждении не перебивает текущий режим.
        if self.shared.force_listen.swap(false, Ordering::Relaxed)
            && self.calibrating.is_none()
            && self.confirm_op.is_none()
        {
            ui_log(self.app, "● хоткей — слушаю команду…");
            self.sound("start.wav");
            self.dictation = None;
            engine.listen_command();
        }
    }

    fn handle<T, V>(&mut self, engine: &mut Engine<T, V>, ev: EngineEvent)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        match ev {
            EngineEvent::State(s) => set_state(self.app, s),
            EngineEvent::WakeHeard => {
                ui_log(self.app, "● слушаю команду…");
                self.sound("start.wav");
            }
            EngineEvent::Partial(t) => {
                if !self.private() {
                    let _ = self.app.emit("reever://partial", t);
                }
            }
            EngineEvent::Command { text, alt } => self.command(engine, &text, alt.as_deref()),
            EngineEvent::Confirm(yes) => self.confirm(yes),
            EngineEvent::Calibration { text, alt } => self.calibration(&text, alt.as_deref()),
            EngineEvent::Timeout => {
                if self.confirm_op.take().is_some() {
                    ui_log(self.app, "↩ не дождался подтверждения — отменено");
                    self.sound("error.wav");
                } else {
                    ui_log(self.app, "⌛ команды не было");
                }
                let _ = self.app.emit("reever://partial", String::new());
                set_state(self.app, "idle");
            }
            EngineEvent::DictationStart => self.dictation_start(),
            EngineEvent::Dictation(text) => self.dictation_text(&text),
            EngineEvent::DictationEnd(reason) => self.dictation_end(reason),
            EngineEvent::Log(m) => ui_log(self.app, m),
        }
    }

    fn dictation_start(&mut self) {
        let _ = self.app.emit("reever://partial", String::new());
        let idle_s = self.cfg.settings.dictation_idle_ms.clamp(2_000, 600_000) / 1000;
        ui_log(
            self.app,
            format!("✎ диктовка: говори — текст печатается в поле с курсором; конец — «стоп ввод», «ривер остановись» или {idle_s} с тишины"),
        );
        self.sound("start.wav");
        self.dictation = Some(Dictation::default());
    }

    /// Кусок диктовки: в поле с курсором активного окна, иначе (если включено) в буфер.
    fn dictation_text(&mut self, raw: &str) {
        let _ = self.app.emit("reever://partial", String::new());
        let private = self.private();
        let clipboard_enabled = self.cfg.settings.dictation_clipboard;
        let session = self.dictation.get_or_insert_with(Dictation::default);
        let pieces = session.formatter.format(raw);
        if pieces.is_empty() {
            return;
        }
        let progress = match typing::text_target() {
            Some(target) => typing::type_pieces(target, &pieces),
            None => typing::TypeProgress {
                typed: Vec::new(),
                remaining: pieces.clone(),
            },
        };
        let text = pieces_to_string(&pieces);
        session.typed_chars += pieces_to_string(&progress.typed).chars().count();
        let rest = &progress.remaining;
        let mut note = "";
        if !rest.is_empty() {
            if clipboard_enabled {
                let rest = pieces_to_string(rest);
                let rest = if session.typed_chars == 0 && session.clipboard.is_empty() {
                    rest.trim_start()
                } else {
                    rest.as_str()
                };
                session.clipboard.push_str(rest);
                note = match typing::set_clipboard(&session.clipboard) {
                    Ok(()) => " → 📋 буфер",
                    Err(_) => " → ⚠ буфер занят, текст потерян",
                };
            } else {
                note = " → ⚠ нет поля ввода, пропущено";
            }
        }
        if private {
            ui_log(self.app, format!("✎ продиктовано{note}"));
        } else {
            ui_log(self.app, format!("✎ «{}»{note}", text.trim()));
        }
    }

    fn dictation_end(&mut self, reason: DictationEndReason) {
        let _ = self.app.emit("reever://partial", String::new());
        let session = self.dictation.take().unwrap_or_default();
        let why = match reason {
            DictationEndReason::Silence => "тишина",
            DictationEndReason::Stopped => "стоп",
            DictationEndReason::Command => "новая команда",
            DictationEndReason::Error => "ошибка распознавания",
        };
        let mut summary = format!(
            "✓ диктовка завершена ({why}): напечатано символов — {}",
            session.typed_chars
        );
        if !session.clipboard.is_empty() {
            summary.push_str(&format!(
                "; в буфере обмена — {} (вставь Ctrl+V)",
                session.clipboard.chars().count()
            ));
        }
        ui_log(self.app, summary);
        match reason {
            DictationEndReason::Command => {}
            DictationEndReason::Error => self.sound("error.wav"),
            _ => self.sound("success.wav"),
        }
    }

    fn command<T, V>(&mut self, engine: &mut Engine<T, V>, text: &str, alt: Option<&str>)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        let _ = self.app.emit("reever://partial", String::new());
        if self.private() {
            ui_log(self.app, "📝 распознано (приватный режим)");
        } else {
            ui_log(self.app, format!("📝 «{text}»"));
            let _ = self.app.emit("reever://heard", text.to_string());
        }
        if textnorm::is_cancel(text, &self.cancel_words) {
            ui_log(self.app, "↩ отменено");
            self.sound("error.wav");
            set_state(self.app, "idle");
            return;
        }
        self.sound("received.wav");
        set_state(self.app, "busy");
        match dispatch(self.app, self.cfg, self.shared, text, alt) {
            DispatchResult::Done(ok) => {
                self.sound(if ok { "success.wav" } else { "error.wav" });
                set_state(self.app, "idle");
            }
            DispatchResult::Confirm(pending) => {
                ui_log(
                    self.app,
                    format!(
                        "❓ {} — подтверди голосом: «да» / «нет» (yes / no)",
                        pending.describe()
                    ),
                );
                self.confirm_sound();
                self.confirm_op = Some(pending);
                engine.listen_confirm();
            }
        }
    }

    /// Сигнал «жду подтверждения»: `confirm.wav` пака, если он есть, иначе `start.wav` —
    /// тот же звук «слушаю», что после wake-слова.
    fn confirm_sound(&self) {
        let path = self.cfg.sound("confirm.wav");
        if path.is_file() {
            self.player.play(&path);
        } else {
            self.sound("start.wav");
        }
    }

    fn confirm(&mut self, yes: bool) {
        let Some(pending) = self.confirm_op.take() else {
            return;
        };
        if yes {
            ui_log(self.app, format!("⚙ выполняю: {}", pending.describe()));
            let ok = execute_pending(self.app, &pending);
            self.sound(if ok { "success.wav" } else { "error.wav" });
        } else {
            ui_log(self.app, "↩ отменено");
            self.sound("error.wav");
        }
        set_state(self.app, "idle");
    }

    fn calibration(&mut self, text: &str, alt: Option<&str>) {
        let Some(id) = self.calibrating.clone() else {
            return;
        };
        let phrase = text.trim();
        let mut candidates = vec![phrase.to_string()];
        if let Some(alt) = alt.map(str::trim).filter(|alt| !alt.is_empty()) {
            candidates.push(alt.to_string());
        }
        candidates.retain(|candidate| !candidate.is_empty());
        if candidates.is_empty() {
            ui_log(self.app, "⚠ калибровка: ничего не распознано, повтори");
            let _ = self.app.emit("reever://calibrated", id);
            return;
        }
        let added = {
            let mut actions = self.shared.actions.lock().unwrap();
            match actions.iter_mut().find(|a| a.id == id) {
                Some(action) => {
                    let mut added = false;
                    for candidate in &candidates {
                        let normalized = textnorm::normalize(candidate);
                        if !action
                            .phrases
                            .iter()
                            .any(|existing| textnorm::normalize(existing) == normalized)
                        {
                            action.phrases.push(candidate.clone());
                            added = true;
                        }
                    }
                    added
                }
                _ => false,
            }
        };
        if added {
            self.shared.persist();
        }
        if self.private() {
            ui_log(self.app, format!("✓ калибровка «{id}»: фраза записана"));
        } else {
            ui_log(
                self.app,
                format!(
                    "✓ калибровка «{id}»: «{phrase}»{}",
                    if added { "" } else { " (уже есть)" }
                ),
            );
        }
        self.sound("success.wav");
        let _ = self.app.emit("reever://calibrated", id);
    }
}
