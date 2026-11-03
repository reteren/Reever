//! Аудио-воркер: склейка Engine (чистый автомат) с Tauri — микрофон, звуки,
//! события интерфейса, диспетчер. Вся логика «когда фраза закончилась и что в ней
//! сказано» — в engine.rs; здесь только побочные эффекты.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use crate::app::AppHandle;

use crate::actions::SharedState;
use crate::anglicism::Anglicisms;
use crate::asr::AsrSet;
use crate::audio::{audio_channel, start_capture, Player};
use crate::bilingual::LangMode;
use crate::config::ReeverConfig;
use crate::dictation::Formatter;
use crate::dispatch::{dispatch, execute_pending, DispatchResult, PendingAction};
use crate::engine::{DictationEndReason, Engine, EngineConfig, EngineEvent};
use crate::events::{set_state, ui_log};
use crate::speech::{Timed, Transcriber};
use crate::vad::SileroVad;
use crate::{textnorm, typing};

/// Очередь от realtime-колбэка микрофона. Распознавание идёт синхронно в этом
/// потоке, поэтому запас ~5 с: длинная фраза в простое не должна терять аудио,
/// пока модель её разбирает (колбэк при переполнении молча выбрасывает чанки).
const AUDIO_QUEUE: usize = 512;
const WAKE_PREROLL_SAMPLES: usize = 16_000;
const LOADING_AUDIO_LIMIT_SAMPLES: usize = 60 * 16_000;
/// Сколько перезапуск распознавания ждёт конца идущей команды (диктовка бывает долгой).
const RELOAD_WAIT_MAX: Duration = Duration::from_secs(60);

/// Main ASR в lazy-режиме остаётся типом Engine, но сам объект хранится отдельно:
/// worker может установить или забрать его, не пересоздавая VAD и wake-spotter.
#[derive(Clone, Default)]
struct LazyAsr {
    inner: Arc<Mutex<Option<AsrSet>>>,
}

impl LazyAsr {
    fn loaded(asr: AsrSet) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(asr))),
        }
    }

    fn install(&self, asr: AsrSet) {
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

/// Модели, оставшиеся от прошлого запуска потока. Настройки сохраняются на каждое изменение,
/// и перезапуск ради порога VAD или паузы не должен заново грузить сотни мегабайт: если план
/// моделей и число потоков те же, новый запуск забирает их отсюда.
static KEPT_ASR: Mutex<Option<(crate::models::ModelPlan, i32, AsrSet)>> = Mutex::new(None);

fn keep_asr(cfg: &ReeverConfig, asr: &LazyAsr) {
    let Some(models) = asr.inner.lock().ok().and_then(|mut slot| slot.take()) else { return };
    if let Ok(mut kept) = KEPT_ASR.lock() {
        *kept = Some((cfg.models.clone(), cfg.settings.asr_threads.max(1) as i32, models));
    }
}

/// Оставленные модели, если они подходят; неподходящие освобождаются до загрузки новых.
fn take_kept_asr(cfg: &ReeverConfig) -> Option<AsrSet> {
    let (plan, threads, models) = KEPT_ASR.lock().ok()?.take()?;
    (plan == cfg.models && threads == cfg.settings.asr_threads.max(1) as i32).then_some(models)
}

/// Загруженные модели и ошибка определителя языка, если он не загрузился.
/// Модели распознавания, ошибка определителя языка и словарь англицизмов (если он нужен):
/// словарь грузится вместе с моделями и живёт столько же.
type LoadedModels = anyhow::Result<(AsrSet, Option<String>, Option<anyhow::Result<Anglicisms>>)>;

#[derive(Clone, Copy)]
enum LoadReason {
    Wake,
    Hotkey,
}

/// Поток распознавания. «Сохранить» в настройках поднимает `shared.reload`: поток
/// закрывает микрофон и модели и стартует заново с новыми настройками. После ошибки
/// старта (неверная модель) ждёт исправленных настроек, а не умирает насовсем.
pub fn run_worker(app: AppHandle, shared: Arc<SharedState>) {
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

/// Сводка для плиток Главной (вместо разбора текста лога во фронтенде); каждый emit шлёт
/// полный снимок.
#[derive(Clone)]
struct Status {
    asr: &'static str,
    model: String,
}

fn emit_status(app: &AppHandle, status: &Mutex<Status>) {
    let Ok(st) = status.lock() else { return };
    let payload = json!({ "asr": st.asr, "model": st.model });
    crate::events::remember_ui_status(&payload);
    let _ = app.emit("reever://status", payload);
}

fn set_asr_status(app: &AppHandle, status: &Mutex<Status>, asr: &'static str) {
    if let Ok(mut st) = status.lock() {
        st.asr = asr;
    }
    emit_status(app, status);
}

fn run_inner(app: &AppHandle, cfg: &ReeverConfig, shared: &Arc<SharedState>) -> anyhow::Result<()> {
    let s = &cfg.settings;
    let player = Player::new();
    let status = Arc::new(Mutex::new(Status {
        asr: "loading",
        model: model_names(cfg),
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
    }

    // По умолчанию ASR не выгружается и остаётся независимым текстовым wake-каналом.
    // Экономию памяти включает ползунок выгрузки.
    let lazy_loading = s.models_unload_after_s <= crate::config::MODELS_UNLOAD_MAX_S;
    let kept = take_kept_asr(cfg);
    let asr = if lazy_loading {
        ui_log(
            app,
            crate::msg!("log.models.lazy"),
        );
        LazyAsr::default()
    } else if let Some(models) = kept {
        LazyAsr::loaded(models)
    } else {
        ui_log(app, crate::msg!("log.models.loading", path = model_names(cfg)));
        let (models, lid_error) = AsrSet::load(&cfg.models, s.asr_threads.max(1) as i32)
            .inspect_err(|_| set_asr_status(app, &status, "error"))?;
        if let Some(error) = lid_error {
            ui_log(app, crate::msg!("log.lid.unavailable", error = error));
        }
        LazyAsr::loaded(models)
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
            confirm_wait_ms: CONFIRM_WAIT_MS,
            choice_wait_ms: 15_000,
            dictation_pause_ms: s.dictation_pause_ms.clamp(300, 5_000),
            dictation_idle_ms: s.dictation_idle_ms.clamp(2_000, 600_000),
            dictation_live: s.dictation_live,
        },
        asr.clone(),
        vad,
    );
    engine.set_strict_spotted_wake(lazy_loading);
    crate::voice_adapt::set_passive_enabled(s.voice_passive);
    // Каждая модель сама пишет и родной язык, и английские вставки латиницей: второй
    // модели для цели команды нет. Ru = «как услышала модель». «Ривер» модель может
    // записать английским словом — это и разрешает english_primary.
    engine.set_bilingual(None, None, LangMode::Ru);
    engine.set_english_primary(true);
    log_languages(app, cfg);
    crate::grammar::set_active_languages(&cfg.models.languages());
    // окно прячет встроенные фразы действий на выключенных языках — пусть пересчитает
    let _ = app.emit("reever://active-languages", ());
    if !lazy_loading {
        install_anglicisms(app, cfg, &mut engine, load_anglicisms(cfg));
    }
    enable_wake_spotter(app, cfg, &mut engine);
    // Один раз подготовить оптимизированные графы моделей (ortopt.rs): загрузка большой
    // модели после этого быстрее. Не сразу — пусть сначала запустится всё остальное.
    #[cfg(windows)]
    {
        let dirs: Vec<std::path::PathBuf> = cfg.models.models.iter().map(|model| model.dir.clone()).collect();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            crate::ortopt::prepare(&dirs);
        });
    }
    set_asr_status(app, &status, "ready");

    let (tx, rx) = audio_channel(AUDIO_QUEUE);
    // Поток микрофона держим в Option: на паузе/мьюте реально закрываем его
    // (дроп Stream останавливает захват → системный индикатор микрофона гаснет).
    // Микрофона нет при запуске — не ошибка воркера: цикл ниже ждёт его и подхватит.
    let mut mic = MicLink::default();
    let mut stream = match start_capture(tx.clone()) {
        Ok(capture) => {
            ui_log(
                app,
                if capture.is_raw() { crate::msg!("log.mic.raw") } else { crate::msg!("log.mic.standard") },
            );
            mic.opened();
            Some(capture)
        }
        Err(error) => {
            mic.lost(app, &error);
            None
        }
    };
    let mut worker = Worker {
        app,
        cfg,
        shared,
        player: &player,
        confirm_op: None,
        confirm_session: 0,
        calibrating: None,
        cancel_words: cfg.settings.cancel_words.clone(),
        grammar_version: None,
        targets_fingerprint: None,
        targets_checked: None,
        dictation: None,
        last_wake_sound: None,
        last_spot_sound: None,
        choice_session: None,
        blackbox: cfg.settings.wake_blackbox.then(|| {
            let dir = app.path().app_config_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            crate::blackbox::BlackBox::new(dir.join("blackbox"))
        }),
    };
    set_state(app, "idle");
    ui_log(
        app,
        crate::msg!(
            "log.ready",
            wake = crate::uilang::spoken(&s.wake_words, "ривер", "reever"),
            cancel = crate::uilang::spoken(&cfg.settings.cancel_words, "отмена", "cancel")
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
    let mut load_rx: Option<Receiver<LoadedModels>> = None;
    let mut loading_audio: Vec<i16> = Vec::new();
    let mut loading_audio_truncated = false;
    let mut load_reason: Option<LoadReason> = None;
    let mut recent_chunks: VecDeque<Vec<i16>> = VecDeque::new();
    let mut recent_samples = 0usize;
    let mut last_chunk = std::time::Instant::now();
    let mut stalled = false;
    let mut voice_recording = false;
    let mut picker_muted = false;
    let mut voice_progress = std::time::Instant::now();
    let mut reload_waiting: Option<std::time::Instant> = None;
    loop {
        // Перезапуск (сохранены настройки, применилась тихая подстройка) ждёт конца команды:
        // посреди неё он обрывал команду, сигнал и уводил маскота. Но не дольше RELOAD_WAIT_MAX.
        let reload = shared.reload.load(Ordering::Relaxed);
        if !reload {
            reload_waiting = None;
        }
        let busy = matches!(crate::events::current_state().as_str(), "listening" | "busy");
        if reload && busy && reload_waiting.get_or_insert_with(std::time::Instant::now).elapsed() < RELOAD_WAIT_MAX {
            // команда ещё идёт — перезапуск после неё
        } else if reload {
            loading_audio.fill(0);
            clear_recent_audio(&mut recent_chunks, &mut recent_samples);
            keep_asr(cfg, &asr);
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
            if let Some(name) = player.follow_default_device() {
                ui_log(app, format!("sound: output switched to «{name}»"));
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
                clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                if worker.confirm_op.take().is_some() {
                    crate::confirm::hide(app);
                }
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
        // Поток умер (ошибка cpal/WASAPI) или звука нет 3 с — переоткрыть микрофон, иначе
        // Reever молча глохнет до перезапуска (переключили улучшения звука в Windows).
        if stream.is_some() && (crate::audio::STREAM_FAILED.load(Ordering::SeqCst) || stalled) {
            stream = None;
            stalled = false;
            ui_log(app, crate::msg!("log.mic.reopen"));
            std::thread::sleep(Duration::from_millis(300));
        }
        // Windows сменила микрофон по умолчанию — перейти на него.
        if stream.is_some() && mic.default_changed(app) {
            stream = None;
        }
        if stream.is_none() {
            match start_capture(tx.clone()) {
                Ok(st) => {
                    stream = Some(st);
                    last_chunk = std::time::Instant::now();
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
                    mic.opened();
                    mic.back(app);
                }
                Err(e) => {
                    mic.lost(app, &e);
                    std::thread::sleep(MIC_RETRY);
                    continue;
                }
            }
        }

        // recv с таймаутом — чтобы реагировать на паузу даже без аудио.
        let waiting = std::time::Instant::now();
        let mut chunk = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => {
                last_chunk = std::time::Instant::now();
                // Ждал звук — очередь была пуста, воркер не отстаёт.
                engine.set_audio_caught_up(last_chunk - waiting >= Duration::from_millis(2));
                c
            }
            Err(RecvTimeoutError::Timeout) => {
                // Только по пустой очереди: долгое распознавание фразы — не обрыв.
                stalled = last_chunk.elapsed() >= Duration::from_secs(3);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };

        meter.push(app, &chunk);
        if let Some(saved) = worker.blackbox.as_mut().and_then(|bb| bb.push(&chunk)) {
            ui_log(app, format!("blackbox: saved {}", saved.display()));
        }
        // Подстройка под голос: «ривер» подряд — запись, а не команды.
        if let Some(words) = record_voice(shared, &chunk, &mut voice_progress) {
            if !voice_recording {
                voice_recording = true;
                engine.reset();
                set_state(app, "idle");
            }
            if let Some(words) = words {
                let _ = app.emit("reever://voice-adapt", json!({ "words": words }));
            }
            chunk.fill(0);
            continue;
        }
        voice_recording = false;
        // Окно выбора («найди папку …») открыто — голос не слушаем: в нём печатают и
        // выбирают мышью, а разговор рядом не должен стать командой.
        if crate::picker::is_open() {
            if !picker_muted {
                picker_muted = true;
                worker.choice_session = None;
                engine.reset();
                set_state(app, "idle");
            }
            chunk.fill(0);
            continue;
        }
        picker_muted = false;
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
                    Err(error) | Ok(Err(error)) => {
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
                    Ok(Ok((models, lid_error, anglicisms))) => {
                        load_rx.take();
                        let reason = load_reason.take().unwrap_or(LoadReason::Wake);
                        asr.install(models);
                        install_anglicisms(app, cfg, &mut engine, anglicisms);
                        if let Some(error) = lid_error {
                            ui_log(app, crate::msg!("log.lid.unavailable", error = error));
                        }
                        set_asr_status(app, &status, "ready");
                        worker.sync_grammar(&mut engine);
                        worker.sync_targets(&mut engine);
                        if matches!(reason, LoadReason::Wake) {
                            engine.reset();
                            engine.mark_spotted_wake();
                            set_state(app, "idle");
                        }
                        let mut replay = std::mem::take(&mut loading_audio);
                        for event in engine.push_audio(&replay) {
                            worker.handle(&mut engine, event);
                        }
                        crate::events::set_wake_phrase(engine.wake_phrase_open());
                        replay.fill(0);
                        clear_recent_audio(&mut recent_chunks, &mut recent_samples);
                        loading_audio_truncated = false;
                        last_activity = std::time::Instant::now();
                    }
                }
            }
            chunk.fill(0);
            continue;
        }

        worker.poll_triggers(&mut engine);
        let started = std::time::Instant::now();
        let events = engine.push_audio(&chunk);
        let spent = started.elapsed();
        let activity = events.iter().any(is_activity_event);
        for event in events {
            worker.handle(&mut engine, event);
        }
        crate::events::set_wake_phrase(engine.wake_phrase_open());
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
            && last_activity.elapsed() >= Duration::from_secs(s.models_unload_after_s as u64)
            && engine.can_keep_warm()
            && worker.confirm_op.is_none()
            && worker.calibrating.is_none()
            && worker.dictation.is_none()
            && worker.choice_session.is_none()
        {
            if asr.unload() {
                engine.set_anglicisms(None, Vec::new());
                ui_log(app, crate::msg!("log.models.unloaded"));
                set_asr_status(app, &status, "ready");
                last_activity = std::time::Instant::now();
            }
        }
        chunk.fill(0);
    }
    Ok(())
}

/// Словарь англицизмов для диктовки (anglicism.rs): только по галочке и только когда
/// распознаётся кириллический язык, иначе словарь даже не загружается (`None`). В режиме
/// с выгрузкой моделей он грузится вместе с ними и выгружается вместе с ними (~43 МБ).
fn load_anglicisms(cfg: &ReeverConfig) -> Option<anyhow::Result<Anglicisms>> {
    let s = &cfg.settings;
    if !s.dictation_english || cfg.cyrillic_spellcheck().is_empty() {
        return None;
    }
    Some(Anglicisms::load(&cfg.anglicisms_path, s.dictation_english_level))
}

/// Нет файла — не фатально: диктовка пишет английские слова так, как их услышала модель.
fn install_anglicisms<T, V>(
    app: &AppHandle,
    cfg: &ReeverConfig,
    engine: &mut Engine<T, V>,
    anglicisms: Option<anyhow::Result<Anglicisms>>,
) where
    T: crate::speech::Transcriber,
    V: crate::speech::VoiceDetector,
{
    match anglicisms {
        Some(Ok(anglicisms)) => engine.set_anglicisms(Some(anglicisms), cfg.cyrillic_spellcheck()),
        Some(Err(error)) => ui_log(app, crate::msg!("log.anglicismsFailed", error = format!("{error:#}"))),
        None => engine.set_anglicisms(None, Vec::new()),
    }
}

/// Фоновый детектор wake-слова: акустическая сеть RKW2 (wake.rs, docs/voice-pipeline.md).
/// Весов нет или выключен — не фатально: «ривер» ловит только большая модель.
fn enable_wake_spotter<T, V>(app: &AppHandle, cfg: &ReeverConfig, engine: &mut Engine<T, V>)
where
    T: crate::speech::Transcriber,
    V: crate::speech::VoiceDetector,
{
    let s = &cfg.settings;
    if s.activation_mode == "hotkey" {
        return;
    }
    let (name, mut weights) = spotter_weights(app, cfg);
    // Подстройка под голос (voice_adapt.rs) — вместо общих весов, если сделана для них.
    let personal = app.path().app_config_dir().ok().and_then(|dir| {
        let base = std::fs::read(&weights).ok()?;
        crate::wake::personal_for(&dir, name, &base)
    });
    if let Some((path, _)) = &personal {
        weights = path.clone();
    }
    // Вторая ступень проверяет кандидатов первой — если её веса есть.
    let verifier = crate::wake::verifier_for(name)
        .map(|verify| (verify, crate::config::resource(app, verify)))
        .filter(|(_, path)| path.is_file());
    let spotter = crate::wake::AcousticWakeSpotter::new(&weights).and_then(|spotter| match &verifier {
        Some((_, path)) => spotter.with_verifier(path),
        None => Ok(spotter),
    });
    match spotter {
        Ok(spotter) => {
            let file = name.rsplit('/').next().unwrap_or(name);
            let file = if personal.is_some() { format!("{file} (personal)") } else { file.to_string() };
            let model = match &verifier {
                Some((verify, _)) => format!("{file} + {} (RKW2)", verify.rsplit('/').next().unwrap_or(verify)),
                None => format!("{file} (RKW2)"),
            };
            ui_log(app, crate::msg!("log.spotter.ok", model = model));
            engine.set_wake_spotter(Some(Box::new(spotter)));
        }
        Err(e) => ui_log(app, crate::msg!("log.spotter.failed", error = format!("{e:#}"))),
    }
}

/// Запись для подстройки под голос: `None` — её нет, `Some(Some(слов))` — пора показать,
/// сколько «ривер» уже слышно (раз в полсекунды).
fn record_voice(shared: &SharedState, chunk: &[i16], progress: &mut std::time::Instant) -> Option<Option<usize>> {
    let mut record = shared.voice_record.lock().ok()?;
    let buffer = record.as_mut()?;
    if buffer.len() < crate::voice_adapt::MAX_RECORD {
        buffer.extend(chunk.iter().map(|&v| v as f32 / 32768.0));
    }
    if progress.elapsed() < Duration::from_millis(500) {
        return Some(None);
    }
    *progress = std::time::Instant::now();
    Some(Some(crate::voice_adapt::words(buffer).len()))
}

/// Общие веса детектора под включённые языки: имя ресурса и путь.
pub fn spotter_weights(app: &AppHandle, cfg: &ReeverConfig) -> (&'static str, std::path::PathBuf) {
    let name = crate::wake::weights_for(&cfg.models.languages());
    let weights = crate::config::resource(app, name);
    if weights.is_file() {
        (name, weights)
    } else {
        ("wake/river.bin", crate::config::resource(app, "wake/river.bin"))
    }
}

fn spawn_model_loader(cfg: &ReeverConfig) -> Receiver<LoadedModels> {
    let cfg = cfg.clone();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        // Словарь (~0,2 с) грузится параллельно с моделями (~1 с) и не задерживает их.
        let anglicisms = std::thread::spawn({
            let cfg = cfg.clone();
            move || load_anglicisms(&cfg)
        });
        let loaded = AsrSet::load(&cfg.models, cfg.settings.asr_threads.max(1) as i32);
        let anglicisms = anglicisms.join().unwrap_or(None);
        let _ = tx.send(loaded.map(|(models, lid_error)| (models, lid_error, anglicisms)));
    });
    rx
}

/// Папки моделей для плитки Главной и журнала.
fn model_names(cfg: &ReeverConfig) -> String {
    cfg.models
        .models
        .iter()
        .map(|model| model.name.as_str())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Какие языки распознаются и почему остальные — нет (models.rs).
fn log_languages(app: &AppHandle, cfg: &ReeverConfig) {
    let names = |codes: &[&str]| {
        codes
            .iter()
            .map(|code| crate::models::language_name(code))
            .collect::<Vec<_>>()
            .join(", ")
    };
    ui_log(
        app,
        crate::msg!("log.languages", languages = names(&cfg.models.languages())),
    );
    if !cfg.models.missing.is_empty() {
        ui_log(
            app,
            crate::msg!("log.languages.missing", languages = names(&cfg.models.missing)),
        );
    }
    if !cfg.models.need_lid.is_empty() {
        ui_log(
            app,
            crate::msg!("log.languages.needLid", languages = names(&cfg.models.need_lid)),
        );
    }
}

fn take_loader_event(
    receiver: &mut Option<Receiver<LoadedModels>>,
) -> Option<anyhow::Result<LoadedModels>> {
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

/// Сколько ждать ответа на подтверждение опасной команды (голосом или на плашке).
const CONFIRM_WAIT_MS: u32 = 8_000;

/// Побочные эффекты событий Engine. Отдельная структура, чтобы не таскать пять
/// параметров через каждую функцию.
struct Worker<'a> {
    app: &'a AppHandle,
    cfg: &'a ReeverConfig,
    shared: &'a Arc<SharedState>,
    player: &'a Player,
    /// Опасная системная операция, ждущая «да» голосом или кликом по плашке.
    confirm_op: Option<PendingAction>,
    /// Номер вопроса на плашке подтверждения (confirm.rs).
    confirm_session: u64,
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
    /// Сигнал на срабатывание детектора: подтверждение большой моделью его не повторяет.
    last_spot_sound: Option<std::time::Instant>,
    /// Звук вокруг срабатываний детектора — в WAV (Настройки → «Записывать срабатывания»).
    blackbox: Option<crate::blackbox::BlackBox>,
}

/// Как часто пробовать открыть пропавший микрофон.
const MIC_RETRY: Duration = Duration::from_millis(500);
/// Как часто сверять микрофон по умолчанию с открытым.
const MIC_DEVICE_CHECK: Duration = Duration::from_secs(1);

/// Какой микрофон открыт и с каких пор его нет. Пропал — журнал пишет об этом один раз,
/// а не на каждую попытку (раз в 0,5 с), и отмечает, когда микрофон вернулся.
struct MicLink {
    /// Идентификатор микрофона по умолчанию, который был при открытии.
    device: Option<String>,
    checked: std::time::Instant,
    missing_since: Option<std::time::Instant>,
}

impl Default for MicLink {
    fn default() -> Self {
        Self {
            device: None,
            checked: std::time::Instant::now(),
            missing_since: None,
        }
    }
}

impl MicLink {
    /// Захват открыт: запомнить, какое устройство сейчас по умолчанию.
    fn opened(&mut self) {
        self.device = crate::micwatch::default_mic().map(|(id, _)| id);
        self.checked = std::time::Instant::now();
    }

    /// Открыть не вышло: в журнал — только первая неудача.
    fn lost(&mut self, app: &AppHandle, error: &anyhow::Error) {
        if self.missing_since.is_none() {
            self.missing_since = Some(std::time::Instant::now());
            ui_log(app, crate::msg!("log.mic.unavailable", error = error));
            ui_log(app, crate::msg!("log.mic.waiting"));
        }
    }

    /// Микрофон снова открыт после пропажи.
    fn back(&mut self, app: &AppHandle) {
        if let Some(since) = self.missing_since.take() {
            ui_log(
                app,
                crate::msg!("log.mic.backAfter", seconds = format!("{:.1}", since.elapsed().as_secs_f32())),
            );
        }
    }

    /// Раз в `MIC_DEVICE_CHECK`: Windows сменила микрофон по умолчанию? Открытый поток
    /// при этом не обрывается и слушал бы прежнее устройство.
    fn default_changed(&mut self, app: &AppHandle) -> bool {
        if self.checked.elapsed() < MIC_DEVICE_CHECK {
            return false;
        }
        self.checked = std::time::Instant::now();
        match self.switch_to(crate::micwatch::default_mic()) {
            Some(name) => {
                ui_log(app, crate::msg!("log.mic.defaultChanged", name = name));
                true
            }
            None => false,
        }
    }

    /// Решение по текущему микрофону по умолчанию: имя нового, если надо переходить.
    /// Микрофона по умолчанию нет — не переходим: оборвётся поток, его и переоткроем.
    fn switch_to(&mut self, current: Option<(String, String)>) -> Option<String> {
        let (id, name) = current?;
        match &self.device {
            Some(device) if *device != id => Some(if name.is_empty() { id } else { name }),
            Some(_) => None,
            None => {
                self.device = Some(id);
                None
            }
        }
    }
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
    /// Напечатанное по ходу речи для текущего куска — актуально после `sync_live`.
    live: typing::LiveField,
    /// Печать по ходу речи в потоке ввода (воркер её не ждёт).
    typist: typing::LiveTypist,
    /// Последняя цель печати по ходу речи — к ней придёт поле.
    live_goal: String,
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

/// Детектор может сработать на одно «ривер» дважды подряд — второй сигнал в этих пределах
/// не звучит. Больше нельзя: два настоящих «ривер» подряд бывают через 3 с (в журнале 2,99 с),
/// и второй оставался без звука.
const WAKE_SOUND_REPEAT: Duration = Duration::from_millis(1500);

/// GigaAM подтверждает «ривер» детектора примерно через секунду. Сигнал детектора засчитывается
/// за это подтверждение только в этих пределах — иначе «ривер» посреди речи через 10 с после
/// прошлой команды молчал, потому что «сигнал уже был».
const SPOT_CONFIRM_WINDOW: Duration = Duration::from_secs(4);

impl Worker<'_> {
    fn sound(&self, name: &str) {
        self.play(&self.cfg.sound(name));
    }

    /// Сбой звука виден в reever.log: раньше он уходил в консоль, которой в релизе нет.
    fn play(&self, path: &std::path::Path) {
        let name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
        match self.player.play(path) {
            Ok(()) => ui_log(self.app, format!("sound: {name}")),
            Err(error) => ui_log(self.app, format!("sound: {name} not played: {error:#}")),
        }
    }

    /// Сигнал «услышал ривер»: сразу от детектора, а если его нет — когда фраза разобрана.
    /// Повтор того же «ривер» в пределах `WAKE_SOUND_REPEAT` не звучит.
    /// «Ривер» — индекс файлов (в покое он только на диске) подгружается заранее, пока
    /// распознаётся команда: «ривер найди …» не ждёт чтения снимка.
    fn prefetch_index(&self) {
        if let Some(index) = crate::app::AppHandle::try_state::<crate::local_index::IndexManager>(self.app) {
            index.prefetch();
        }
    }

    fn blackbox_mark(&mut self, tag: &'static str) {
        if let Some(bb) = self.blackbox.as_mut() {
            bb.mark(tag);
        }
    }

    fn wake_sound(&mut self) {
        let recent = self
            .last_wake_sound
            .is_some_and(|at| at.elapsed() < WAKE_SOUND_REPEAT);
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
        let active = crate::grammar::active_languages();
        active.hash(&mut hasher);
        for action in actions.iter() {
            let phrases = action.phrases.iter().filter(|p| action.phrase_active(p, active.as_deref()));
            for target in phrases.chain(std::iter::once(&action.label)) {
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
                    .filter(|p| action.phrase_active(p, active.as_deref()))
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
        // Ответ кликом по плашке подтверждения — голос больше не ждём.
        if self.confirm_op.is_some() {
            if let Some(yes) = crate::confirm::take_answer(self.confirm_session) {
                engine.stop_confirm();
                self.confirm(yes);
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

    fn handle<T, V>(&mut self, engine: &mut Engine<T, V>, ev: EngineEvent)
    where
        T: crate::speech::Transcriber,
        V: crate::speech::VoiceDetector,
    {
        match ev {
            EngineEvent::State(s) => set_state(self.app, s),
            EngineEvent::WakeHeard => {
                self.blackbox_mark("heard");
                if let Some(clip) = engine.take_wake_clip() {
                    crate::voice_adapt::offer_wake_clip(clip);
                }
                ui_log(self.app, crate::msg!("log.listen"));
                self.prefetch_index();
                // Сигнал уже прозвучал на детекторе — большая модель «ривер» лишь подтвердила.
                let spotted = self
                    .last_spot_sound
                    .take()
                    .is_some_and(|at| at.elapsed() < SPOT_CONFIRM_WINDOW);
                if !spotted {
                    self.wake_sound();
                }
            }
            EngineEvent::WakeSpotted => {
                self.blackbox_mark("spot");
                // Сразу, не дожидаясь загрузки большой модели (~1,7 с у Parakeet v3): ложное
                // срабатывание детектора даст лишний сигнал, зато настоящее «ривер» слышно сразу.
                self.wake_sound();
                self.last_spot_sound = Some(std::time::Instant::now());
                crate::mascot::on_wake(self.app);
                // линза лёгкого интерфейса и глаз маскота наливаются сразу по детектору
                // (frontend/skin/shell.js, frontend/skin/mascot.js)
                let _ = self.app.emit("reever://wake", ());
                self.prefetch_index();
            }
            EngineEvent::WakeRejected => {
                // сигнал ложного срабатывания не должен глушить следующее настоящее «ривер»
                self.last_spot_sound = None;
                self.last_wake_sound = None;
                crate::mascot::on_wake_rejected(self.app);
                let _ = self.app.emit("reever://wake-rejected", ());
            }
            EngineEvent::Partial(t) => {
                if !self.private() {
                    let _ = self.app.emit("reever://partial", t);
                }
            }
            EngineEvent::Command { text, alt } => {
                // команда закончила это «ривер»: следующее — уже новое и звучит своим сигналом
                self.last_spot_sound = None;
                self.last_wake_sound = None;
                // «ривер открой …» одной фразой — без WakeHeard, но «ривер» в ней подтверждён.
                if let Some(clip) = engine.take_wake_clip() {
                    crate::voice_adapt::offer_wake_clip(clip);
                }
                self.command(engine, &text, alt.as_deref())
            }
            EngineEvent::Confirm(yes) => self.confirm(yes),
            EngineEvent::Choice(text) => self.choice(&text),
            EngineEvent::Calibration { text, alt } => self.calibration(&text, alt.as_deref()),
            EngineEvent::Timeout => {
                if self.confirm_op.take().is_some() {
                    crate::confirm::hide(self.app);
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
            EngineEvent::DictationPartial(text, settled) => self.dictation_partial(&text, settled),
            EngineEvent::DictationEnd(reason) => self.dictation_end(reason),
            EngineEvent::Log(m) => {
                let text = m.render("en");
                if text.contains("did not confirm") {
                    self.blackbox_mark("unconfirmed");
                } else if text.starts_with("wake: detector candidate") {
                    // Отброшенные кандидаты — главный материал для дообучения детектора.
                    self.blackbox_mark("rejected");
                }
                ui_log(self.app, m)
            }
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
                wake = crate::uilang::spoken(&self.cfg.settings.wake_words, "ривер", "reever"),
                stop = if crate::uilang::current() == "ru" && self.cfg.russian_enabled() { "стоп" } else { "stop" },
                cancel = crate::uilang::spoken(&self.cancel_words, "отмена", "cancel")
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
    /// совпавшие в двух вариантах подряд (после речи пауза — все), и почти без стирания;
    /// окончательный текст выровняет `dictation_text`. Печатает поток ввода, воркер не ждёт.
    fn dictation_partial(&mut self, raw: &str, settled: bool) {
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
        let Some(goal) = typing::partial_goal(&session.live_goal, &previous, &preview, settled)
        else {
            return;
        };
        let Some(target) = self.dictation_target(Duration::ZERO) else {
            return;
        };
        let session = self.dictation.get_or_insert_with(Dictation::default);
        if session.typist.post(target, goal.clone()) {
            session.live_goal = goal;
        } else {
            session.live_broken = true;
        }
    }

    /// Дождаться печати по ходу речи и забрать поле: готовый кусок, конец и отмена
    /// работают с тем, что действительно напечатано.
    fn sync_live(&mut self) {
        let Some(session) = self.dictation.as_mut() else {
            return;
        };
        let (field, failed) = session.typist.finish();
        session.live = field;
        if failed {
            session.live_broken = true;
        }
    }

    /// Готовый кусок диктовки: в поле с курсором (напечатанное по ходу речи остаётся,
    /// допечатывается продолжение). Если поля нет, кусок ждёт его вместе с прежними
    /// ненапечатанными — так текст не разбивается между полем и буфером обмена.
    fn dictation_text(&mut self, raw: &str) {
        let _ = self.app.emit("reever://partial", String::new());
        self.sync_live();
        let private = self.private();
        let session = self.dictation.get_or_insert_with(Dictation::default);
        let pieces = session.formatter.format(raw);
        let wanted = typing::pieces_to_text(&pieces);
        if wanted.is_empty() && session.live.typed.is_empty() {
            return;
        }
        // готовый кусок диктовки — пузырю над маскотом (frontend/bubble.html)
        if !private && !wanted.trim().is_empty() {
            let _ = self.app.emit("reever://said", wanted.clone());
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
        session.live_goal.clear();
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
        self.sync_live();
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
        self.sync_live();
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
            crate::mascot::heard(text);
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
                self.confirm_session = crate::confirm::show(self.app, &pending, CONFIRM_WAIT_MS / 1000);
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
            self.play(&path);
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
        crate::confirm::hide(self.app);
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

#[cfg(test)]
mod mic_link_tests {
    use super::MicLink;

    #[test]
    fn microphone_follows_the_windows_default() {
        let mic = |id: &str, name: &str| Some((id.to_string(), name.to_string()));
        let mut link = MicLink::default();
        // Устройство при открытии не прочиталось — запоминаем первое увиденное.
        assert_eq!(link.switch_to(mic("usb", "HyperX")), None);
        assert_eq!(link.switch_to(mic("usb", "HyperX")), None);
        // Микрофона по умолчанию нет — ждём обрыва потока, не дёргаемся.
        assert_eq!(link.switch_to(None), None);
        // Windows перешла на другой — переходим и мы.
        assert_eq!(link.switch_to(mic("webcam", "Webcam Mic")), Some("Webcam Mic".into()));
        assert_eq!(link.switch_to(mic("webcam", "")), Some("webcam".into()));
    }
}
