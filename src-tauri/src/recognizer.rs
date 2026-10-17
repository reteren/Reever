//! Аудио-воркер: склейка Engine (чистый автомат) с Tauri — микрофон, звуки,
//! события интерфейса, диспетчер. Вся логика «когда фраза закончилась и что в ней
//! сказано» — в engine.rs; здесь только побочные эффекты.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::actions::SharedState;
use crate::anglicism::Anglicisms;
use crate::asr::{Asr, AsrConfig};
use crate::audio::{audio_channel, start_capture, Player};
use crate::bilingual::LangMode;
use crate::config::ReeverConfig;
use crate::dictation::Formatter;
use crate::dispatch::{dispatch, execute_pending, DispatchResult, PendingAction};
use crate::engine::{DictationEndReason, Engine, EngineConfig, EngineEvent};
use crate::events::{set_state, ui_log};
use crate::uilang::Msg;
use crate::grammar::{Grammar, IntentKind};
use crate::langid::SpeechLid;
use crate::speech::{LanguageId, Timed, Transcriber};
use crate::vad::SileroVad;
use crate::{everything, textnorm, typing};

/// Очередь от realtime-колбэка микрофона. Распознавание идёт синхронно в этом
/// потоке, поэтому запас ~5 с: длинная фраза в простое не должна терять аудио,
/// пока модель её разбирает (колбэк при переполнении молча выбрасывает чанки).
const AUDIO_QUEUE: usize = 512;
const WAKE_PREROLL_SAMPLES: usize = 16_000;
const LOADING_AUDIO_LIMIT_SAMPLES: usize = 60 * 16_000;

/// Main ASR в lazy-режиме остаётся типом Engine, но сам объект хранится отдельно:
/// worker может установить или забрать его, не пересоздавая VAD и wake-spotter.
#[derive(Clone, Default)]
struct LazyAsr {
    inner: Arc<Mutex<Option<Asr>>>,
}

impl LazyAsr {
    fn loaded(asr: Asr) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(asr))),
        }
    }

    fn install(&self, asr: Asr) {
        if let Ok(mut slot) = self.inner.lock() {
            *slot = Some(asr);
        }
    }

    fn unload(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .is_some()
    }

    fn is_loaded(&self) -> bool {
        self.inner.lock().is_ok_and(|slot| slot.is_some())
    }
}

impl Transcriber for LazyAsr {
    fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        match self
            .inner
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().map(|asr| asr.transcribe(pcm)))
        {
            Some(result) => result,
            None => Ok(String::new()),
        }
    }

    fn transcribe_timed(&mut self, pcm: &[i16]) -> anyhow::Result<Timed> {
        match self
            .inner
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().map(|asr| asr.transcribe_timed(pcm)))
        {
            Some(result) => result,
            None => Ok(Timed::default()),
        }
    }

    fn transcribe_verbatim(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        match self
            .inner
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().map(|asr| asr.transcribe_verbatim(pcm)))
        {
            Some(result) => result,
            None => Ok(String::new()),
        }
    }
}

enum ModelLoadEvent {
    Primary(anyhow::Result<Asr>),
    Secondary {
        second: Option<Asr>,
        lid: Option<SpeechLid>,
        warnings: Vec<Msg>,
    },
}

#[derive(Clone, Copy)]
enum LoadReason {
    Wake,
    Hotkey,
}

/// Поток распознавания. «Сохранить» в настройках поднимает `shared.reload`: поток
/// закрывает микрофон и модели и стартует заново с новыми настройками. После ошибки
/// старта (неверная модель) ждёт исправленных настроек, а не умирает насовсем.
pub fn run_worker(app: AppHandle, shared: Arc<SharedState>) {
    use tauri::Manager;
    loop {
        shared.reload.store(false, Ordering::Relaxed);
        let cfg = app.state::<crate::config::LiveConfig>().get();
        if let Err(e) = run_inner(&app, &cfg, &shared) {
            eprintln!("[reever] worker fatal: {e:#}");
            set_state(&app, "error");
            ui_log(&app, crate::msg!("log.fatal", error = e));
            while !shared.reload.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
        } else if !shared.reload.load(Ordering::Relaxed) {
            return;
        }
        ui_log(&app, crate::msg!("log.restarting"));
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
    let payload = json!({ "asr": st.asr, "model": st.model, "everything": st.everything });
    crate::events::remember_ui_status(&payload);
    let _ = app.emit("reever://status", payload);
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
                crate::msg!("log.everything.ok")
            } else {
                crate::msg!("log.everything.down")
            },
        );
    });
}

fn run_inner(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>) -> anyhow::Result<()> {
    let s = &cfg.settings;
    let player = Player::new()?;
    let status = Arc::new(Mutex::new(Status {
        asr: "loading",
        model: s.primary_asr_model().to_string(),
        everything: None,
    }));
    set_state(app, "loading");
    emit_status(app, &status);
    ui_log(
        app,
        crate::msg!(
            "log.started",
            version = env!("CARGO_PKG_VERSION"),
            mode = crate::uilang::text(
                crate::uilang::current(),
                match s.activation_mode.as_str() {
                    "hotkey" => "activation.hotkey",
                    "wake" => "activation.wake",
                    _ => "activation.both",
                }
            )
        ),
    );

    if s.offline_mode {
        ui_log(
            app,
            crate::msg!("log.offline"),
        );
    } else {
        spawn_everything_probe(app, cfg, &status);
    }

    let lazy_loading = s.models_unload_s > 0;
    let asr = if lazy_loading {
        ui_log(
            app,
            crate::msg!("log.models.lazy"),
        );
        LazyAsr::default()
    } else {
        ui_log(
            app,
            crate::msg!("log.models.loading", path = cfg.asr_model_dir.display()),
        );
        let primary = Asr::new(&AsrConfig {
            model_dir: cfg.asr_model_dir.clone(),
            num_threads: s.asr_threads.max(1) as i32,
        })
        .inspect_err(|_| set_asr_status(app, &status, "error"))?;
        LazyAsr::loaded(primary)
    };
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
            confirm_wait_ms: 8_000,
            choice_wait_ms: 15_000,
            dictation_pause_ms: s.dictation_pause_ms.clamp(300, 5_000),
            dictation_idle_ms: s.dictation_idle_ms.clamp(2_000, 600_000),
            dictation_live: s.dictation_live,
        },
        asr.clone(),
        vad,
    );
    engine.set_strict_spotted_wake(lazy_loading);
    if s.russian_enabled() {
        engine.set_bilingual(None, None, language_mode(s));
        if !lazy_loading {
            let (second, lid, warnings) = load_secondary_models(cfg);
            for warning in warnings {
                ui_log(app, warning);
            }
            engine.set_bilingual(
                second.map(|model| Box::new(model) as Box<dyn Transcriber + Send>),
                lid.map(|model| Box::new(model) as Box<dyn LanguageId + Send>),
                language_mode(s),
            );
        }
        enable_anglicisms(app, cfg, &mut engine);
    } else {
        // Только английский: основная модель уже английская, второй язык и словарь
        // англицизмов (русская диктовка) не нужны. Ru = «как услышала основная модель».
        ui_log(app, crate::msg!("log.englishOnly"));
        engine.set_bilingual(None, None, LangMode::Ru);
    }
    enable_wake_spotter(app, cfg, &mut engine);
    set_asr_status(app, &status, "ready");

    let (tx, rx) = audio_channel(AUDIO_QUEUE);
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
        targets_fingerprint: None,
        targets_checked: None,
        dictation: None,
        last_wake_sound: None,
        choice_session: None,
    };
    set_state(app, "idle");
    ui_log(
        app,
        crate::msg!(
            "log.ready",
            wake = s.wake_words.first().map(String::as_str).unwrap_or("ривер"),
            cancel = cfg.settings.cancel_words.first().map(String::as_str).unwrap_or("отмена")
        ),
    );

    // Громкость микрофона в Windows: её меняют другие программы (Discord в звонке).
    let mut mic_level: Option<crate::micwatch::MicLevel> = None;
    let mut mic_checked = std::time::Instant::now();
    let mut meter = crate::events::LevelMeter::new();
    let trace = std::env::var_os("REEVER_TRACE").is_some();
    let mut last_activity = std::time::Instant::now();
    let mut last_warm = std::time::Instant::now();
    let mut last_audio_trace = std::time::Instant::now();
    let mut load_rx: Option<Receiver<ModelLoadEvent>> = None;
    let mut loading_audio: Vec<i16> = Vec::new();
    let mut loading_audio_truncated = false;
    let mut load_reason: Option<LoadReason> = None;
    let mut pending_command_audio: Option<Vec<i16>> = None;
    let mut pending_command_reason: Option<LoadReason> = None;
    let mut staged_audio: Option<Vec<i16>> = None;
    let mut staged_reason: Option<LoadReason> = None;
    let mut recent_chunks: VecDeque<Vec<i16>> = VecDeque::new();
    let mut recent_samples = 0usize;
    loop {
        if shared.reload.load(Ordering::Relaxed) {
            loading_audio.fill(0);
            if let Some(audio) = pending_command_audio.as_mut() {
                audio.fill(0);
            }
            if let Some(audio) = staged_audio.as_mut() {
                audio.fill(0);
            }
            clear_recent_audio(&mut recent_chunks, &mut recent_samples);
            return Ok(());
        }
        if trace && stream.is_some() && last_audio_trace.elapsed() >= Duration::from_secs(60) {
            let elapsed = last_audio_trace.elapsed().as_secs_f64();
            let stats = crate::audio::take_callback_stats();
            let callback_rate = stats.callbacks as f64 / elapsed;
            let average_frames = if stats.callbacks == 0 {
                0.0
            } else {
                stats.input_frames as f64 / stats.callbacks as f64
            };
            ui_log(
                app,
                format!(
                    "trace: audio callbacks {callback_rate:.1}/s, average chunk {average_frames:.0} frames"
                ),
            );
            last_audio_trace = std::time::Instant::now();
        }
        if stream.is_some() && mic_checked.elapsed() >= Duration::from_secs(2) {
            mic_checked = std::time::Instant::now();
            let dropped = crate::audio::DROPPED_SAMPLES.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                ui_log(
                    app,
                    crate::msg!("log.audioDropped", ms = dropped * 1000 / 16_000),
                );
            }
            let now = crate::micwatch::mic_level();
            if let (Some(before), Some(after)) = (mic_level, now) {
                if let Some(line) = crate::micwatch::describe_change(before, after) {
                    ui_log(app, line);
                }
            }
            if now.is_some() {
                mic_level = now;
            }
        }
        if shared.paused.load(Ordering::Relaxed) {
            if stream.is_some() {
                stream = None;
                engine.reset();
                if !asr.is_loaded() && load_rx.take().is_some() {
                    // Загрузчик завершится сам, но его результат уже некому принять.
                    // Так звук до паузы не попадёт в команду после возобновления.
                    load_reason = None;
                    set_asr_status(app, &status, "ready");
                }
                loading_audio.fill(0);
                loading_audio.clear();
                loading_audio_truncated = false;
                if let Some(audio) = pending_command_audio.as_mut() {
                    audio.fill(0);
                }
                pending_command_audio = None;
                pending_command_reason = None;
                if let Some(audio) = staged_audio.as_mut() {
                    audio.fill(0);
                }
                staged_audio = None;
                staged_reason = None;
                clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                worker.confirm_op = None;
                worker.calibrating = None;
                worker.dictation = None;
                // Накопившееся в очереди — тоже голос; выбрасываем, не распознавая.
                while let Ok(mut queued) = rx.try_recv() {
                    queued.fill(0);
                }
                set_state(app, "paused");
                ui_log(app, crate::msg!("log.mic.closed"));
            }
            std::thread::sleep(Duration::from_millis(150));
            continue;
        }
        if stream.is_none() {
            match start_capture(tx.clone()) {
                Ok(st) => {
                    stream = Some(st);
                    set_state(app, "idle");
                    mic_level = crate::micwatch::mic_level();
                    mic_checked = std::time::Instant::now();
                    match mic_level {
                        Some(level) => ui_log(
                            app,
                            crate::msg!("log.mic.onLevel", percent = level.percent),
                        ),
                        None => ui_log(app, crate::msg!("log.mic.on")),
                    }
                }
                Err(e) => {
                    ui_log(app, crate::msg!("log.mic.unavailable", error = e));
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }

        // recv с таймаутом — чтобы реагировать на паузу даже без аудио.
        let mut chunk = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        meter.push(app, &chunk);
        worker.sync_grammar(&mut engine);
        worker.sync_targets(&mut engine);
        let model_ready = asr.is_loaded();
        let loading = load_rx.is_some();
        if lazy_loading && !model_ready {
            if loading {
                if append_loading_audio(&mut loading_audio, &chunk) && !loading_audio_truncated {
                    ui_log(
                        app,
                        crate::msg!("log.models.bufferFull"),
                    );
                }
                loading_audio_truncated |= loading_audio.len() >= LOADING_AUDIO_LIMIT_SAMPLES;
            } else {
                remember_recent_audio(&mut recent_chunks, &mut recent_samples, &chunk);
                let hotkey_started = worker.poll_triggers(&mut engine);
                let calibration_started = worker.calibrating.is_some();
                if hotkey_started || calibration_started {
                    loading_audio_truncated |= append_loading_audio(&mut loading_audio, &chunk);
                    load_reason = Some(LoadReason::Hotkey);
                    load_rx = Some(spawn_model_loader(cfg));
                    last_activity = std::time::Instant::now();
                    set_asr_status(app, &status, "loading");
                    set_state(app, "loading");
                    ui_log(app, crate::msg!("log.models.loadingNow"));
                } else {
                    let started = std::time::Instant::now();
                    let events = engine.push_audio(&chunk);
                    let spent = started.elapsed();
                    let spotted = events
                        .iter()
                        .any(|event| matches!(event, EngineEvent::WakeSpotted));
                    for ev in events {
                        worker.handle(&mut engine, ev);
                    }
                    if spotted {
                        loading_audio = flatten_chunks(&recent_chunks);
                        clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        loading_audio_truncated = false;
                        load_reason = Some(LoadReason::Wake);
                        load_rx = Some(spawn_model_loader(cfg));
                        last_activity = std::time::Instant::now();
                        set_asr_status(app, &status, "loading");
                        set_state(app, "loading");
                        ui_log(app, crate::msg!("log.models.loadingNow"));
                    }
                    if trace && spent >= Duration::from_millis(250) {
                        ui_log(
                            app,
                            format!("trace: recognition took {} ms", spent.as_millis()),
                        );
                    }
                }
            }

            if let Some(result) = take_loader_event(&mut load_rx) {
                match result {
                    Err(error) | Ok(ModelLoadEvent::Primary(Err(error))) => {
                        load_rx.take();
                        load_reason = None;
                        loading_audio.fill(0);
                        loading_audio.clear();
                        engine.reset();
                        set_asr_status(app, &status, "error");
                        set_state(app, "idle");
                        ui_log(app, crate::msg!("log.models.failed", error = format!("{error:#}")));
                        clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        loading_audio_truncated = false;
                    }
                    Ok(ModelLoadEvent::Primary(Ok(primary))) => {
                        let reason = load_reason.take().unwrap_or(LoadReason::Wake);
                        asr.install(primary);
                        set_asr_status(app, &status, "ready");
                        worker.sync_grammar(&mut engine);
                        worker.sync_targets(&mut engine);
                        if matches!(reason, LoadReason::Wake) {
                            engine.reset();
                            engine.mark_spotted_wake();
                            set_state(app, "idle");
                        }

                        let mut secondary_ready = !s.russian_enabled();
                        if !s.russian_enabled() {
                            load_rx.take();
                        } else if let Some(secondary) = take_loader_event(&mut load_rx) {
                            match secondary {
                                Ok(ModelLoadEvent::Secondary {
                                    second,
                                    lid,
                                    warnings,
                                }) => {
                                    install_secondary_models(
                                        app,
                                        &mut engine,
                                        s,
                                        second,
                                        lid,
                                        warnings,
                                    );
                                    load_rx.take();
                                    secondary_ready = true;
                                }
                                Err(error) => {
                                    ui_log(
                                        app,
                                        crate::msg!("log.models.secondFailed", error = format!("{error:#}")),
                                    );
                                    engine.set_bilingual(None, None, language_mode(s));
                                    secondary_ready = true;
                                }
                                Ok(ModelLoadEvent::Primary(_)) => {}
                            }
                        }

                        if !secondary_ready {
                            staged_audio = Some(loading_audio.clone());
                            staged_reason = Some(reason);
                        }
                        let mut replay = std::mem::take(&mut loading_audio);
                        let events = engine.push_audio(&replay);
                        let deferred =
                            worker.handle_engine_events(&mut engine, events, !secondary_ready);
                        if deferred {
                            pending_command_audio =
                                Some(staged_audio.take().unwrap_or_else(|| replay.clone()));
                            pending_command_reason = staged_reason.take().or(Some(reason));
                            set_state(app, "loading");
                            ui_log(app, crate::msg!("log.models.waitSecond"));
                        }
                        replay.fill(0);
                        clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        loading_audio_truncated = false;
                        last_activity = std::time::Instant::now();
                    }
                    Ok(ModelLoadEvent::Secondary {
                        second,
                        lid,
                        warnings,
                    }) => {
                        install_secondary_models(app, &mut engine, s, second, lid, warnings);
                        load_rx.take();
                    }
                }
            }
            chunk.fill(0);
            continue;
        }

        if lazy_loading && asr.is_loaded() && load_rx.is_some() {
            if pending_command_audio.is_some() {
                if append_loading_audio(&mut loading_audio, &chunk) && !loading_audio_truncated {
                    ui_log(
                        app,
                        crate::msg!("log.models.bufferFull"),
                    );
                }
                loading_audio_truncated |= loading_audio.len() >= LOADING_AUDIO_LIMIT_SAMPLES;
            } else {
                remember_recent_audio(&mut recent_chunks, &mut recent_samples, &chunk);
                if let Some(audio) = staged_audio.as_mut() {
                    if append_loading_audio(audio, &chunk) && !loading_audio_truncated {
                        ui_log(
                            app,
                            crate::msg!("log.models.bufferFull"),
                        );
                    }
                    loading_audio_truncated |= audio.len() >= LOADING_AUDIO_LIMIT_SAMPLES;
                }
            }

            let mut replayed_pending = false;
            if let Some(result) = take_loader_event(&mut load_rx) {
                match result {
                    Ok(ModelLoadEvent::Secondary {
                        second,
                        lid,
                        warnings,
                    }) => {
                        install_secondary_models(app, &mut engine, s, second, lid, warnings);
                        load_rx.take();
                        if let Some(mut replay) = pending_command_audio.take() {
                            let reason = pending_command_reason.take().unwrap_or(LoadReason::Wake);
                            let truncated = append_loading_audio(&mut replay, &loading_audio);
                            if truncated {
                                ui_log(app, crate::msg!("log.models.commandTooLong"));
                            }
                            loading_audio.fill(0);
                            loading_audio.clear();
                            engine.reset();
                            match reason {
                                LoadReason::Wake => engine.mark_spotted_wake(),
                                LoadReason::Hotkey => engine.listen_command(),
                            }
                            let events = engine.push_audio(&replay);
                            for event in events {
                                if matches!(
                                    event,
                                    EngineEvent::WakeHeard | EngineEvent::WakeSpotted
                                ) {
                                    continue;
                                }
                                worker.handle(&mut engine, event);
                            }
                            replay.fill(0);
                            replayed_pending = true;
                            loading_audio_truncated = false;
                            clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                            last_activity = std::time::Instant::now();
                        } else {
                            if let Some(audio) = staged_audio.as_mut() {
                                audio.fill(0);
                            }
                            staged_audio = None;
                            staged_reason = None;
                            clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        }
                    }
                    Err(error) => {
                        ui_log(app, crate::msg!("log.models.secondFailed", error = format!("{error:#}")));
                        engine.set_bilingual(None, None, language_mode(s));
                        load_rx.take();
                        if let Some(mut replay) = pending_command_audio.take() {
                            let reason = pending_command_reason.take().unwrap_or(LoadReason::Wake);
                            let truncated = append_loading_audio(&mut replay, &loading_audio);
                            if truncated {
                                ui_log(app, crate::msg!("log.models.commandTooLong"));
                            }
                            loading_audio.fill(0);
                            loading_audio.clear();
                            engine.reset();
                            match reason {
                                LoadReason::Wake => engine.mark_spotted_wake(),
                                LoadReason::Hotkey => engine.listen_command(),
                            }
                            let events = engine.push_audio(&replay);
                            for event in events {
                                if matches!(
                                    event,
                                    EngineEvent::WakeHeard | EngineEvent::WakeSpotted
                                ) {
                                    continue;
                                }
                                worker.handle(&mut engine, event);
                            }
                            replay.fill(0);
                            replayed_pending = true;
                            loading_audio_truncated = false;
                            clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        } else {
                            if let Some(audio) = staged_audio.as_mut() {
                                audio.fill(0);
                            }
                            staged_audio = None;
                            staged_reason = None;
                            loading_audio.fill(0);
                            loading_audio.clear();
                            loading_audio_truncated = false;
                            clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        }
                    }
                    Ok(ModelLoadEvent::Primary(_)) => {
                        ui_log(app, "⚠ model loader returned stages out of order");
                    }
                }
            }

            if pending_command_audio.is_some() && load_rx.is_some() {
                chunk.fill(0);
                continue;
            }
            if replayed_pending {
                chunk.fill(0);
                continue;
            }
        }

        let hotkey_started = worker.poll_triggers(&mut engine);
        if hotkey_started && lazy_loading && load_rx.is_some() {
            if let Some(audio) = staged_audio.as_mut() {
                audio.fill(0);
            }
            staged_audio = Some(chunk.to_vec());
            staged_reason = Some(LoadReason::Hotkey);
            loading_audio_truncated = false;
        }
        let started = std::time::Instant::now();
        let events = engine.push_audio(&chunk);
        let spent = started.elapsed();
        let activity = events.iter().any(is_activity_event);
        if lazy_loading && load_rx.is_some() {
            if events
                .iter()
                .any(|event| matches!(event, EngineEvent::WakeSpotted))
            {
                if let Some(audio) = staged_audio.as_mut() {
                    audio.fill(0);
                }
                staged_audio = Some(flatten_chunks(&recent_chunks));
                staged_reason = Some(LoadReason::Wake);
                loading_audio_truncated = false;
            }
        }
        let has_command = events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. }));
        let deferred =
            worker.handle_engine_events(&mut engine, events, lazy_loading && load_rx.is_some());
        if deferred {
            pending_command_audio = Some(staged_audio.take().unwrap_or_else(|| chunk.to_vec()));
            pending_command_reason = staged_reason.take().or(Some(LoadReason::Wake));
            set_state(app, "loading");
            ui_log(app, crate::msg!("log.models.waitSecond"));
        } else if has_command {
            if let Some(audio) = staged_audio.as_mut() {
                audio.fill(0);
            }
            staged_audio = None;
            staged_reason = None;
            loading_audio_truncated = false;
        }
        if activity {
            last_activity = std::time::Instant::now();
        }
        if trace && spent >= Duration::from_millis(250) {
            ui_log(
                app,
                format!("trace: recognition took {} ms", spent.as_millis()),
            );
        }

        // 0 — прежний режим с постоянными моделями и периодическим keep-warm.
        if !lazy_loading && last_warm.elapsed() >= Duration::from_secs(60) && engine.can_keep_warm()
        {
            let started = std::time::Instant::now();
            engine.keep_warm();
            last_warm = std::time::Instant::now();
            if trace {
                ui_log(
                    app,
                    format!(
                        "trace: model warm-up {} ms",
                        started.elapsed().as_millis()
                    ),
                );
            }
        }

        // В lazy-режиме выгружаем только в чистом Idle. keep_warm не вызывается для
        // отсутствующих моделей: следующий триггер загрузит их в фоне.
        if lazy_loading
            && asr.is_loaded()
            && load_rx.is_none()
            && s.models_unload_s > 0
            && last_activity.elapsed() >= Duration::from_secs(s.models_unload_s as u64)
            && engine.can_keep_warm()
            && worker.confirm_op.is_none()
            && worker.calibrating.is_none()
            && worker.dictation.is_none()
            && worker.choice_session.is_none()
        {
            engine.set_bilingual(None, None, language_mode(s));
            if asr.unload() {
                ui_log(app, crate::msg!("log.models.unloaded"));
                set_asr_status(app, &status, "ready");
                last_activity = std::time::Instant::now();
            }
        }
        chunk.fill(0);
    }
    Ok(())
}

/// Словарь англицизмов для диктовки (anglicism.rs). Нет файла — не фатально: диктовка
/// пишет английские слова так, как их услышала русская модель.
fn enable_anglicisms<T, V>(app: &AppHandle, cfg: &ReeverConfig, engine: &mut Engine<T, V>)
where
    T: crate::speech::Transcriber,
    V: crate::speech::VoiceDetector,
{
    let s = &cfg.settings;
    if !s.dictation_english {
        return;
    }
    match Anglicisms::load(&cfg.anglicisms_path, &s.dictation_terms) {
        Ok(anglicisms) => engine.set_anglicisms(Some(anglicisms)),
        Err(error) => ui_log(app, crate::msg!("log.anglicismsFailed", error = format!("{error:#}"))),
    }
}

/// Фоновый детектор wake-слова на маленькой потоковой модели (docs/voice-pipeline.md).
/// Модели нет или выключен — не фатально: «ривер» ловит только большая модель.
fn enable_wake_spotter<T, V>(app: &AppHandle, cfg: &ReeverConfig, engine: &mut Engine<T, V>)
where
    T: crate::speech::Transcriber,
    V: crate::speech::VoiceDetector,
{
    let s = &cfg.settings;
    if !s.wake_spotter || s.activation_mode == "hotkey" {
        return;
    }
    let dir = crate::config::resource(app, &format!("model/{}", s.wake_spotter_model));
    match crate::wake::StreamingWakeSpotter::new(&dir, &s.wake_words) {
        Ok(spotter) => {
            ui_log(
                app,
                crate::msg!("log.spotter.ok", model = &s.wake_spotter_model),
            );
            engine.set_wake_spotter(Some(Box::new(spotter)));
        }
        Err(e) => ui_log(app, crate::msg!("log.spotter.failed", error = format!("{e:#}"))),
    }
}

/// Загружает вторую модель и LID после основной. Отсутствие этих моделей не
/// блокирует команды на основном языке: bilingual.rs использует текстовые эвристики.
fn load_secondary_models(cfg: &ReeverConfig) -> (Option<Asr>, Option<SpeechLid>, Vec<Msg>) {
    let s = &cfg.settings;
    let mut warnings = Vec::new();
    let second = match Asr::new(&AsrConfig {
        model_dir: cfg.second_model_dir.clone(),
        num_threads: s.asr_threads.max(1) as i32,
    }) {
        Ok(asr) => Some(asr),
        Err(error) => {
            warnings.push(crate::msg!("log.second.unavailable", error = format!("{error:#}")));
            None
        }
    };
    let lid = if second.is_some() {
        match &cfg.lid_model_dir {
            Some(dir) => match SpeechLid::new(dir, 1) {
                Ok(lid) => Some(lid),
                Err(error) => {
                    warnings.push(crate::msg!("log.lid.unavailable", error = format!("{error:#}")));
                    None
                }
            },
            None => None,
        }
    } else {
        None
    };
    if second.is_some() {
        warnings.push(match &lid {
            Some(_) => crate::msg!(
                "log.second.ok",
                model = &s.second_asr_model,
                lid = &s.lid_model,
                target = &s.payload_language
            ),
            None => crate::msg!(
                "log.second.okHeuristic",
                model = &s.second_asr_model,
                target = &s.payload_language
            ),
        });
    }
    (second, lid, warnings)
}

fn language_mode(settings: &crate::config::Settings) -> LangMode {
    match settings.payload_language.as_str() {
        "ru" => LangMode::Ru,
        "en" => LangMode::En,
        _ => LangMode::Auto,
    }
}

fn install_secondary_models<T, V>(
    app: &AppHandle,
    engine: &mut Engine<T, V>,
    settings: &crate::config::Settings,
    second: Option<Asr>,
    lid: Option<SpeechLid>,
    warnings: Vec<Msg>,
) where
    T: crate::speech::Transcriber,
    V: crate::speech::VoiceDetector,
{
    for warning in warnings {
        ui_log(app, warning);
    }
    engine.set_bilingual(
        second.map(|model| Box::new(model) as Box<dyn Transcriber + Send>),
        lid.map(|model| Box::new(model) as Box<dyn LanguageId + Send>),
        language_mode(settings),
    );
}

fn spawn_model_loader(cfg: &ReeverConfig) -> Receiver<ModelLoadEvent> {
    let cfg = cfg.clone();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let primary = Asr::new(&AsrConfig {
            model_dir: cfg.asr_model_dir.clone(),
            num_threads: cfg.settings.asr_threads.max(1) as i32,
        });
        let primary = match primary {
            Ok(primary) => primary,
            Err(error) => {
                let _ = tx.send(ModelLoadEvent::Primary(Err(error)));
                return;
            }
        };
        if tx.send(ModelLoadEvent::Primary(Ok(primary))).is_err() {
            return;
        }
        if cfg.settings.russian_enabled() {
            let (second, lid, warnings) = load_secondary_models(&cfg);
            let _ = tx.send(ModelLoadEvent::Secondary {
                second,
                lid,
                warnings,
            });
        }
    });
    rx
}

fn take_loader_event(
    receiver: &mut Option<Receiver<ModelLoadEvent>>,
) -> Option<anyhow::Result<ModelLoadEvent>> {
    let rx = receiver.as_ref()?;
    match rx.try_recv() {
        Ok(event) => Some(Ok(event)),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            receiver.take();
            Some(Err(anyhow::anyhow!(
                "the model loader thread ended without a result"
            )))
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
    }
}

fn remember_recent_audio(chunks: &mut VecDeque<Vec<i16>>, sample_count: &mut usize, chunk: &[i16]) {
    chunks.push_back(chunk.to_vec());
    *sample_count = sample_count.saturating_add(chunk.len());
    while *sample_count > WAKE_PREROLL_SAMPLES {
        let Some(mut oldest) = chunks.pop_front() else {
            break;
        };
        *sample_count = sample_count.saturating_sub(oldest.len());
        oldest.fill(0);
    }
}

fn flatten_chunks(chunks: &VecDeque<Vec<i16>>) -> Vec<i16> {
    let sample_count = chunks.iter().map(Vec::len).sum();
    let mut audio = Vec::with_capacity(sample_count);
    for chunk in chunks {
        audio.extend_from_slice(chunk);
    }
    audio
}

fn append_loading_audio(audio: &mut Vec<i16>, chunk: &[i16]) -> bool {
    let remaining = LOADING_AUDIO_LIMIT_SAMPLES.saturating_sub(audio.len());
    let kept = remaining.min(chunk.len());
    audio.extend_from_slice(&chunk[..kept]);
    kept < chunk.len()
}

fn clear_recent_audio(chunks: &mut VecDeque<Vec<i16>>, sample_count: &mut usize) {
    for chunk in chunks.iter_mut() {
        chunk.fill(0);
    }
    chunks.clear();
    *sample_count = 0;
}

fn is_activity_event(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::WakeSpotted
            | EngineEvent::WakeHeard
            | EngineEvent::Command { .. }
            | EngineEvent::Confirm(_)
            | EngineEvent::Choice(_)
            | EngineEvent::Calibration { .. }
            | EngineEvent::DictationStart
            | EngineEvent::DictationEnd(_)
            | EngineEvent::State("listening" | "busy")
    )
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
    /// Отпечаток целей действий, переданных в Engine (см. `sync_targets`).
    targets_fingerprint: Option<u64>,
    targets_checked: Option<std::time::Instant>,
    /// Идёт диктовка («ривер напиши»).
    dictation: Option<Dictation>,
    /// Номер окна выбора, для которого ждём номер голосом.
    choice_session: Option<u64>,
    /// Когда последний раз играл сигнал wake-слова: детектор и GigaAM слышат одно «ривер»,
    /// сигнал должен прозвучать один раз.
    last_wake_sound: Option<std::time::Instant>,
}

/// Состояние одной диктовки: оформление текста и то, что ушло в буфер обмена.
#[derive(Default)]
struct Dictation {
    formatter: Formatter,
    /// Текст, для которого не нашлось поля ввода; лежит в буфере обмена целиком.
    clipboard: String,
    /// Что было в буфере до диктовки (`Some` — диктовка в него писала): вернуть при отмене.
    clipboard_before: Option<Option<String>>,
    /// Сколько символов готовых кусков напечатано в поле (для стирания при отмене).
    typed_chars: usize,
    /// Причина «нет поля ввода» уже записана в журнал в этой диктовке.
    focus_logged: bool,
    /// Окно, выбранное для ввода в начале диктовки.
    target: Option<isize>,
    /// Напечатанное по ходу речи для текущего куска.
    live: typing::LiveField,
    /// Печать по ходу речи в этом куске не удалась — до конца куска только готовый текст.
    live_broken: bool,
    /// Прошлый промежуточный текст куска: по ходу речи печатаются слова, совпавшие дважды.
    last_preview: String,
    /// Готовый текст, который ещё не удалось напечатать (поле не нашлось, окно на миг
    /// сменилось). Ждёт поле, чтобы текст не разбился между полем и буфером; в конце
    /// диктовки остаток уходит в буфер обмена.
    pending: String,
}

/// Сколько готовый кусок ждёт поле ввода: фокус в WebView на миг уходит на контейнер,
/// а активным ненадолго становится всплывающее окно.
const FIELD_WAIT: Duration = Duration::from_millis(700);

impl Worker<'_> {
    fn sound(&self, name: &str) {
        self.player.play(&self.cfg.sound(name));
    }

    /// Сигнал «услышал ривер»: сразу от детектора, а если его нет — когда фраза разобрана.
    /// Повтор того же «ривер» (детектор, потом GigaAM) в пределах 3 с не звучит.
    fn wake_sound(&mut self) {
        if !self.cfg.settings.wake_sound {
            return;
        }
        let recent = self
            .last_wake_sound
            .is_some_and(|at| at.elapsed() < Duration::from_secs(3));
        if !recent {
            self.sound("start.wav");
            self.last_wake_sound = Some(std::time::Instant::now());
        }
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
            ui_log(self.app, crate::msg!("log.grammarUpdated"));
        }
        engine.set_grammar(grammar);
        self.grammar_version = Some(version);
    }

    /// Цели действий для досрочного конца команды («ривер открой маркноут» — запуск без
    /// полной паузы). Действия меняют UI и калибровка; раз в секунду сверяем отпечаток.
    fn sync_targets<T, V>(&mut self, engine: &mut Engine<T, V>)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        if self
            .targets_checked
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.targets_checked = Some(std::time::Instant::now());
        let Some(actions) = self.shared.actions.lock().ok() else {
            return;
        };
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut count = 0usize;
        for action in actions.iter() {
            for target in action.phrases.iter().chain(std::iter::once(&action.label)) {
                target.hash(&mut hasher);
                action.id.hash(&mut hasher);
                count += 1;
            }
        }
        count.hash(&mut hasher);
        let fingerprint = hasher.finish();
        if self.targets_fingerprint == Some(fingerprint) {
            return;
        }
        let targets = actions
            .iter()
            .flat_map(|action| {
                action
                    .phrases
                    .iter()
                    .chain(std::iter::once(&action.label))
                    .map(|target| (target.clone(), action.id.clone()))
            })
            .collect::<Vec<(String, String)>>();
        drop(actions);
        engine.set_known_targets(targets);
        self.targets_fingerprint = Some(fingerprint);
    }

    /// Внешние триггеры из UI/хоткеев: калибровка и push-to-talk.
    fn poll_triggers<T, V>(&mut self, engine: &mut Engine<T, V>) -> bool
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        let pending = self.shared.calibrate.lock().ok().and_then(|g| g.clone());
        match (&self.calibrating, pending) {
            (None, Some(id)) => {
                ui_log(
                    self.app,
                    crate::msg!("log.calibration.start"),
                );
                self.sound("start.wav");
                engine.start_calibration();
                self.calibrating = Some(id);
            }
            // «Готово» в UI (cancel_calibration сбросил флаг) или калибруют другое действие.
            (Some(cur), p) if p.as_deref() != Some(cur.as_str()) => {
                engine.stop_calibration();
                self.calibrating = None;
                ui_log(self.app, crate::msg!("log.calibration.done"));
                set_state(self.app, "idle");
            }
            _ => {}
        }
        // Окно выбора закрыто кликом/клавишей — номер голосом больше не ждём.
        if let Some(session) = self.choice_session {
            if self.shared.picker_closed_session.load(Ordering::SeqCst) >= session {
                self.choice_session = None;
                engine.stop_choice();
            }
        }
        // Хоткей в калибровке/подтверждении не перебивает текущий режим.
        let hotkey_started = self.shared.force_listen.swap(false, Ordering::Relaxed)
            && self.calibrating.is_none()
            && self.confirm_op.is_none();
        if hotkey_started {
            ui_log(self.app, crate::msg!("log.listen.hotkey"));
            self.sound("start.wav");
            self.dictation = None;
            engine.listen_command();
        }
        hotkey_started
    }

    fn command_needs_secondary(&self, text: &str) -> bool {
        if !self.cfg.settings.russian_enabled() {
            return false;
        }
        let grammar = self
            .shared
            .grammar
            .lock()
            .ok()
            .map(|grammar| grammar.clone())
            .unwrap_or_else(Grammar::defaults);
        let command = textnorm::normalize(text);
        let (payload, intent_lang) = if let Some(found) = grammar.match_trigger(&command) {
            (found.payload, found.intent.lang)
        } else if grammar.allows_bare_launch() {
            let lang = grammar
                .intents
                .iter()
                .find(|intent| intent.enabled && intent.kind == IntentKind::Launch)
                .map(|intent| intent.lang)
                .unwrap_or(LangMode::Auto);
            (command, lang)
        } else {
            return false;
        };
        if payload.is_empty() {
            return false;
        }
        let normalized_payload = textnorm::normalize(&payload);
        let known_target = self.shared.actions.lock().ok().is_some_and(|actions| {
            actions.iter().any(|action| {
                textnorm::normalize(&action.label) == normalized_payload
                    || action
                        .phrases
                        .iter()
                        .any(|phrase| textnorm::normalize(phrase) == normalized_payload)
            })
        });
        if known_target {
            return false;
        }
        match intent_lang {
            LangMode::Ru => false,
            LangMode::En => true,
            LangMode::Auto => match self.cfg.settings.payload_language.as_str() {
                "ru" => false,
                "en" => true,
                _ => true,
            },
        }
    }

    fn handle_engine_events<T, V>(
        &mut self,
        engine: &mut Engine<T, V>,
        events: Vec<EngineEvent>,
        secondary_pending: bool,
    ) -> bool
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        let defer_command = secondary_pending
            && events.iter().any(|event| match event {
                EngineEvent::Command { text, .. } => self.command_needs_secondary(text),
                _ => false,
            });
        for event in events {
            if defer_command
                && (matches!(&event, EngineEvent::State("busy"))
                    || matches!(&event, EngineEvent::Command { text, .. } if self.command_needs_secondary(text)))
            {
                continue;
            }
            self.handle(engine, event);
        }
        defer_command
    }

    fn handle<T, V>(&mut self, engine: &mut Engine<T, V>, ev: EngineEvent)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        match ev {
            EngineEvent::State(s) => set_state(self.app, s),
            EngineEvent::WakeHeard => {
                ui_log(self.app, crate::msg!("log.listen"));
                self.wake_sound();
            }
            EngineEvent::WakeSpotted => {
                if self.cfg.settings.models_unload_s == 0 {
                    self.wake_sound();
                }
            }
            EngineEvent::Partial(t) => {
                if !self.private() {
                    let _ = self.app.emit("reever://partial", t);
                }
            }
            EngineEvent::Command { text, alt } => self.command(engine, &text, alt.as_deref()),
            EngineEvent::Confirm(yes) => self.confirm(yes),
            EngineEvent::Choice(text) => self.choice(&text),
            EngineEvent::Calibration { text, alt } => self.calibration(&text, alt.as_deref()),
            EngineEvent::Timeout => {
                if self.confirm_op.take().is_some() {
                    ui_log(self.app, crate::msg!("log.confirm.timeout"));
                    self.sound("error.wav");
                } else {
                    ui_log(self.app, crate::msg!("log.noCommand"));
                }
                let _ = self.app.emit("reever://partial", String::new());
                set_state(self.app, "idle");
            }
            EngineEvent::DictationStart => self.dictation_start(),
            EngineEvent::Dictation(text) => self.dictation_text(&text),
            EngineEvent::DictationPartial(text) => self.dictation_partial(&text),
            EngineEvent::DictationEnd(reason) => self.dictation_end(reason),
            EngineEvent::Log(m) => ui_log(self.app, m),
        }
    }

    fn dictation_start(&mut self) {
        let _ = self.app.emit("reever://partial", String::new());
        let idle_s = self.cfg.settings.dictation_idle_ms.clamp(2_000, 600_000) / 1000;
        ui_log(
            self.app,
            crate::msg!(
                "log.dictation.start",
                seconds = idle_s,
                wake = self.cfg.settings.wake_words.first().map(String::as_str).unwrap_or("ривер"),
                stop = if self.cfg.settings.russian_enabled() { "стоп" } else { "stop" },
                cancel = self.cancel_words.first().map(String::as_str).unwrap_or("отмена")
            ),
        );
        // Цепочка сигналов: «ривер» уже прозвучал (детектор/пауза) — теперь «принял команду».
        self.wake_sound();
        self.sound("received.wav");
        self.dictation = Some(Dictation::default());
    }

    /// Окно, куда печатать: выбранное в начале диктовки, пока оно активно (поле в нём не
    /// перепроверяется на каждом куске — фокус в WebView на миг уходит на соседний элемент);
    /// иначе — поле ввода в новом активном окне. `wait` — сколько ждать, пока поле появится.
    fn dictation_target(&mut self, wait: Duration) -> Option<isize> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let session = self.dictation.get_or_insert_with(Dictation::default);
            if let Some(target) = session.target {
                if typing::is_foreground(target) {
                    return Some(target);
                }
            }
            if let Some(target) = typing::text_target() {
                session.target = Some(target);
                return Some(target);
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        let session = self.dictation.get_or_insert_with(Dictation::default);
        if !wait.is_zero() && !session.focus_logged {
            // Один раз за диктовку — что было в фокусе: по журналу видно, почему поле не нашлось.
            session.focus_logged = true;
            ui_log(
                self.app,
                crate::msg!("log.dictation.noField", focus = typing::describe_focus()),
            );
        }
        None
    }

    /// Напечатать накопленный текст, если поле есть. `true` — накопленного не осталось.
    fn flush_pending(&mut self, target: isize) -> bool {
        let session = self.dictation.get_or_insert_with(Dictation::default);
        if session.pending.is_empty() {
            return true;
        }
        let mut field = typing::LiveField::default();
        let done = field.reconcile(target, &session.pending);
        let typed = field.typed.chars().count();
        session.typed_chars += typed;
        session.pending = session.pending.chars().skip(typed).collect();
        done && session.pending.is_empty()
    }

    /// Промежуточный текст куска — печать по ходу речи (`typing::partial_goal`): только слова,
    /// совпавшие в двух вариантах подряд, и почти без стирания; окончательный текст
    /// выровняет `dictation_text`.
    fn dictation_partial(&mut self, raw: &str) {
        if !self.private() {
            let _ = self.app.emit("reever://partial", raw.to_string());
        }
        let session = self.dictation.get_or_insert_with(Dictation::default);
        let preview = typing::pieces_to_text(&session.formatter.clone().format(raw));
        let previous = std::mem::replace(&mut session.last_preview, preview.clone());
        // Пока готовый текст ждёт поле, печать по ходу речи нарушила бы порядок.
        if session.live_broken || !session.pending.is_empty() {
            return;
        }
        let Some(goal) = typing::partial_goal(&session.live.typed, &previous, &preview) else {
            return;
        };
        let Some(target) = self.dictation_target(Duration::ZERO) else {
            return;
        };
        let session = self.dictation.get_or_insert_with(Dictation::default);
        if !session.live.reconcile(target, &goal) {
            session.live_broken = true;
        }
    }

    /// Готовый кусок диктовки: в поле с курсором (напечатанное по ходу речи остаётся,
    /// допечатывается продолжение). Если поля нет, кусок ждёт его вместе с прежними
    /// ненапечатанными — так текст не разбивается между полем и буфером обмена.
    fn dictation_text(&mut self, raw: &str) {
        let _ = self.app.emit("reever://partial", String::new());
        let private = self.private();
        let session = self.dictation.get_or_insert_with(Dictation::default);
        let pieces = session.formatter.format(raw);
        let wanted = typing::pieces_to_text(&pieces);
        if wanted.is_empty() && session.live.typed.is_empty() {
            return;
        }
        let target = self.dictation_target(FIELD_WAIT);
        let flushed = match target {
            Some(target) => self.flush_pending(target),
            None => false,
        };
        let session = self.dictation.get_or_insert_with(Dictation::default);
        let rest = match target.filter(|_| flushed) {
            Some(target) => session.live.settle(target, &wanted),
            None => {
                // Напечатанное по ходу речи в поле осталось; ждёт только остальное.
                let prefix = session
                    .live
                    .typed
                    .chars()
                    .zip(wanted.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                session.live.typed = session.live.typed.chars().take(prefix).collect();
                wanted.chars().skip(prefix).collect()
            }
        };
        session.typed_chars += session.live.typed.chars().count();
        session.live.typed.clear();
        session.last_preview.clear();
        session.live_broken = false;
        let waiting = !rest.is_empty();
        if waiting {
            session.pending.push_str(&rest);
        }
        if wanted.is_empty() {
            return;
        }
        let line = match (private, waiting) {
            (true, false) => crate::msg!("log.dictation.typed.private"),
            (true, true) => crate::msg!("log.dictation.waiting.private"),
            (false, false) => crate::msg!("log.dictation.typed", text = wanted.trim()),
            (false, true) => crate::msg!("log.dictation.waiting", text = wanted.trim()),
        };
        ui_log(self.app, line);
    }

    /// Конец диктовки: что так и не нашло поле — последняя попытка, затем буфер обмена.
    fn settle_pending(&mut self) {
        let has_pending = self
            .dictation
            .as_ref()
            .is_some_and(|session| !session.pending.is_empty());
        if !has_pending {
            return;
        }
        if let Some(target) = self.dictation_target(FIELD_WAIT) {
            if self.flush_pending(target) {
                return;
            }
        }
        let clipboard_enabled = self.cfg.settings.dictation_clipboard;
        let Some(session) = self.dictation.as_mut() else {
            return;
        };
        let rest = std::mem::take(&mut session.pending);
        if !clipboard_enabled {
            ui_log(self.app, crate::msg!("log.dictation.dropped"));
            return;
        }
        if session.clipboard_before.is_none() {
            session.clipboard_before = Some(typing::get_clipboard_text());
        }
        let rest = rest.replace('\n', "\r\n");
        let rest = if session.typed_chars == 0 {
            rest.trim_start().to_string()
        } else {
            rest
        };
        session.clipboard.push_str(&rest);
        if typing::set_clipboard(&session.clipboard).is_err() {
            ui_log(self.app, crate::msg!("log.dictation.clipboardBusy"));
            session.clipboard.clear();
        }
    }

    fn dictation_end(&mut self, reason: DictationEndReason) {
        let _ = self.app.emit("reever://partial", String::new());
        if reason == DictationEndReason::Cancelled {
            self.cancel_dictation();
            return;
        }
        // Напечатанное по ходу речи для незаконченного куска («ривер открой …», «ривер стоп»)
        // стереть.
        let target = self.dictation.as_ref().and_then(|session| session.target);
        if let Some(session) = self.dictation.as_mut() {
            let mut live = std::mem::take(&mut session.live);
            if let (false, Some(target)) = (live.typed.is_empty(), target) {
                let _ = live.reconcile(target, "");
            }
        }
        self.settle_pending();
        let session = self.dictation.take().unwrap_or_default();
        let why = crate::uilang::text(
            crate::uilang::current(),
            match reason {
                DictationEndReason::Silence => "dictation.end.silence",
                DictationEndReason::Stopped => "dictation.end.stopped",
                DictationEndReason::Command => "dictation.end.command",
                DictationEndReason::Error => "dictation.end.error",
                DictationEndReason::Cancelled => "dictation.end.cancelled",
            },
        );
        let summary = if session.clipboard.is_empty() {
            crate::msg!("log.dictation.end", why = why, typed = session.typed_chars)
        } else {
            crate::msg!(
                "log.dictation.endClipboard",
                why = why,
                typed = session.typed_chars,
                clipboard = session.clipboard.chars().count()
            )
        };
        ui_log(self.app, summary);
        match reason {
            DictationEndReason::Command => {}
            DictationEndReason::Error => self.sound("error.wav"),
            _ => self.sound("success.wav"),
        }
    }

    /// «Ривер отмена» в диктовке: стереть напечатанное (пока окно то же) и вернуть буфер
    /// обмена, каким он был до диктовки.
    fn cancel_dictation(&mut self) {
        let session = self.dictation.take().unwrap_or_default();
        let total = session.typed_chars + session.live.typed.chars().count();
        let mut erased = 0;
        if total > 0 {
            if let Some(target) = session.target.filter(|t| typing::is_foreground(*t)) {
                erased = typing::press_backspace(target, total);
            }
        }
        if let Some(before) = &session.clipboard_before {
            let _ = typing::set_clipboard(before.as_deref().unwrap_or(""));
        }
        let locale = crate::uilang::current();
        let mut summary = if erased < total {
            crate::msg!("log.dictation.cancelledPartly", erased = erased, total = total).render(locale)
        } else {
            crate::msg!("log.dictation.cancelled", erased = erased).render(locale)
        };
        if session.clipboard_before.is_some() {
            summary.push_str(crate::uilang::text(locale, "log.dictation.clipboardRestored"));
        }
        ui_log(self.app, summary);
        self.sound("error.wav");
    }

    fn command<T, V>(&mut self, engine: &mut Engine<T, V>, text: &str, alt: Option<&str>)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        let _ = self.app.emit("reever://partial", String::new());
        if self.private() {
            ui_log(self.app, crate::msg!("log.heard.private"));
        } else {
            // Второй вариант (другой язык) — по журналу видно, что услышала вторая модель.
            match alt.filter(|alt| alt.trim() != text.trim()) {
                Some(alt) => ui_log(self.app, crate::msg!("log.heardAlt", text = text, alt = alt)),
                None => ui_log(self.app, format!("📝 «{text}»")),
            }
            let _ = self.app.emit("reever://heard", text.to_string());
        }
        if textnorm::is_cancel(text, &self.cancel_words) {
            ui_log(self.app, crate::msg!("log.cancelled"));
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
            DispatchResult::Choose(count) => {
                ui_log(
                    self.app,
                    crate::msg!("log.choose", count = count),
                );
                self.choice_session = Some(self.shared.picker_session.load(Ordering::SeqCst));
                engine.listen_choice();
            }
            DispatchResult::Confirm(pending) => {
                ui_log(
                    self.app,
                    crate::msg!(
                        "log.confirm",
                        action = pending.describe().render(crate::uilang::current())
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

    /// Номер варианта, сказанный в окне выбора. Не разобрали — окно остаётся для мыши.
    fn choice(&mut self, text: &str) {
        self.choice_session = None;
        let open = self
            .shared
            .picker
            .lock()
            .map(|picker| !picker.is_empty())
            .unwrap_or(false);
        if !open {
            return;
        }
        if text.trim().is_empty() {
            ui_log(
                self.app,
                crate::msg!("log.choice.notHeard"),
            );
            set_state(self.app, "idle");
            return;
        }
        if textnorm::is_cancel(text, &self.cancel_words) {
            crate::picker::hide(self.app, self.shared);
            ui_log(self.app, crate::msg!("log.choice.cancelled"));
            self.sound("error.wav");
            set_state(self.app, "idle");
            return;
        }
        match crate::find::parse_choice(text) {
            Some(number) => {
                let ok = crate::picker::choose(self.app, self.shared, self.cfg, number - 1);
                self.sound(if ok { "success.wav" } else { "error.wav" });
            }
            None => {
                if self.private() {
                    ui_log(self.app, crate::msg!("log.choice.unclear.private"));
                } else {
                    ui_log(
                        self.app,
                        crate::msg!("log.choice.unclear", text = text),
                    );
                }
            }
        }
        set_state(self.app, "idle");
    }

    fn confirm(&mut self, yes: bool) {
        let Some(pending) = self.confirm_op.take() else {
            return;
        };
        if yes {
            ui_log(
                self.app,
                crate::msg!("log.executing", action = pending.describe().render(crate::uilang::current())),
            );
            let ok = execute_pending(self.app, &pending);
            self.sound(if ok { "success.wav" } else { "error.wav" });
        } else {
            ui_log(self.app, crate::msg!("log.cancelled"));
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
            ui_log(self.app, crate::msg!("log.calibration.nothing"));
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
            ui_log(self.app, crate::msg!("log.calibration.saved.private", action = &id));
        } else {
            ui_log(
                self.app,
                if added {
                    crate::msg!("log.calibration.saved", action = &id, phrase = phrase)
                } else {
                    crate::msg!("log.calibration.exists", action = &id, phrase = phrase)
                },
            );
        }
        self.sound("success.wav");
        let _ = self.app.emit("reever://calibrated", id);
    }
}

#[cfg(test)]
mod audio_buffer_tests {
    use super::{
        append_loading_audio, clear_recent_audio, remember_recent_audio,
        LOADING_AUDIO_LIMIT_SAMPLES, WAKE_PREROLL_SAMPLES,
    };
    use std::collections::VecDeque;

    #[test]
    fn wake_preroll_is_bounded_and_loading_audio_keeps_its_start() {
        let mut recent = VecDeque::new();
        let mut recent_samples = 0;
        remember_recent_audio(&mut recent, &mut recent_samples, &vec![1; 8_000]);
        remember_recent_audio(&mut recent, &mut recent_samples, &vec![2; 8_000]);
        remember_recent_audio(&mut recent, &mut recent_samples, &vec![3; 8_000]);

        assert!(recent_samples <= WAKE_PREROLL_SAMPLES + 8_000);
        assert!(recent.iter().flatten().all(|sample| *sample != 1));

        let mut loading = Vec::new();
        assert!(!append_loading_audio(&mut loading, &[7, 8]));
        let kept = LOADING_AUDIO_LIMIT_SAMPLES - loading.len();
        assert!(append_loading_audio(&mut loading, &vec![9; kept + 1]));
        assert_eq!(loading.len(), LOADING_AUDIO_LIMIT_SAMPLES);
        assert_eq!(&loading[..2], &[7, 8]);

        clear_recent_audio(&mut recent, &mut recent_samples);
        assert!(recent.is_empty());
        assert_eq!(recent_samples, 0);
    }
}
