//! Автомат прослушивания без Tauri: wake → команда / подтверждение / калибровка.
//! Владелец: воркер `engine`.

use crate::anglicism::Anglicisms;
use crate::bilingual::{self, LangMode, Resolved, MIN_LID_DURATION_MS};
use crate::commands::is_bare_verb;
use crate::dictation::{
    choose_language, language_switch, leading_language_switch, trailing_stop_words, trim_words,
};
use crate::grammar::{Grammar, IntentKind};
use crate::segmenter::{SegmentEvent, Segmenter, SegmenterConfig};
use crate::speech::{
    samples, Lang, LanguageId, Timed, TimedWord, Transcriber, VoiceDetector, WakeSpotter, SAMPLE_RATE,
};
use crate::textnorm::{is_no, is_yes, normalize, starts_with_wake, strip_wake};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub wake_words: Vec<String>,
    /// false — режим «только хоткей»: эфир в простое не распознаётся вообще.
    pub wake_enabled: bool,
    /// Тишина, завершающая фразу в простое (там же ловится wake).
    pub wake_pause_ms: u32,
    /// Тишина, завершающая команду после сигнала «слушаю».
    pub command_pause_ms: u32,
    /// Сколько ждать начала команды после wake/хоткея, прежде чем сдаться.
    pub command_wait_ms: u32,
    /// Сколько аудио глушить после сигнала активации режима.
    pub signal_mute_ms: u32,
    /// Промежуточные тексты во время команды; 0 — выключено.
    pub partial_interval_ms: u32,
    /// Сколько ждать ответа «да»/«нет» на подтверждение.
    pub confirm_wait_ms: u32,
    /// Сколько ждать номер варианта в окне выбора.
    pub choice_wait_ms: u32,
    /// Диктовка: пауза, после которой сказанный кусок печатается (диктовка идёт дальше).
    pub dictation_pause_ms: u32,
    /// Диктовка заканчивается, если столько времени не было речи.
    pub dictation_idle_ms: u32,
    /// Печатать диктовку по ходу речи (промежуточный текст), а не после паузы.
    pub dictation_live: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            wake_words: vec!["ривер".into()],
            wake_enabled: true,
            wake_pause_ms: 600,
            command_pause_ms: 900,
            command_wait_ms: 5_000,
            // 0: аудио из очереди записано ДО сигнала, глушение съедало бы начало
            // команды («ривер открой… папку»). Эхо сигнала гасит пропуск пустых фраз.
            signal_mute_ms: 0,
            partial_interval_ms: 400,
            confirm_wait_ms: 8_000,
            choice_wait_ms: 15_000,
            dictation_pause_ms: 800,
            dictation_idle_ms: 5_000,
            dictation_live: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    /// "idle" | "listening" | "busy" — для `reever://state`.
    State(&'static str),
    /// Сказали только wake-слово (или wake + голый глагол): играть сигнал «слушаю».
    WakeHeard,
    /// Фоновый детектор услышал wake-слово прямо по ходу речи (сигнал «услышал»).
    WakeSpotted,
    /// Промежуточный текст текущей фразы — для `reever://partial`.
    Partial(String),
    /// Готовая команда без wake-слова (нормализованная).
    Command { text: String, alt: Option<String> },
    /// Фраза, сказанная в режиме калибровки (без wake-слова).
    Calibration { text: String, alt: Option<String> },
    /// Ответ на подтверждение: true — «да».
    Confirm(bool),
    /// Сказанное в окне выбора (номер варианта); пусто — не дождались.
    Choice(String),
    /// После wake/хоткея команды не дождались.
    Timeout,
    /// Началась диктовка («ривер напиши»): дальше речь — текст, а не команды.
    DictationStart,
    /// Очередной продиктованный кусок «как сказано» (без нормализации).
    Dictation(String),
    /// Промежуточный текст текущего куска «как сказано» — для печати по ходу речи.
    DictationPartial(String),
    /// Диктовка закончилась.
    DictationEnd(DictationEndReason),
    /// Диагностика для лога (без распознанного текста — приватность решает glue).
    Log(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationEndReason {
    /// Долго не было речи (`dictation_idle_ms`).
    Silence,
    /// Сказали фразу остановки («стоп ввод», «ривер остановись»).
    Stopped,
    /// «Ривер отмена» / «ривер стоп» — диктовка отменена, продиктованное стирается.
    Cancelled,
    /// Сказали новую команду с wake-словом — она выполняется следом.
    Command,
    /// Ошибка распознавания.
    Error,
}

/// Pre-roll и пределы длины фразы: короткие команды и длинная диктовка.
const COMMAND_LIMITS: (u32, u32, u32) = (300, 0, 15_000);
const DICTATION_LIMITS: (u32, u32, u32) = (500, 12_000, 24_000);
/// Как часто пересчитывать текст при живой диктовке.
const LIVE_PARTIAL_MS: u32 = 400;
/// Длиннее этого промежуточный текст диктовки не пересчитывается: дорого, а кусок
/// всё равно скоро закончится на мягком пределе.
const DICTATION_PARTIAL_MAX_MS: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenKind {
    Command,
    Confirm,
    /// Номер варианта в окне выбора («ривер найди …»).
    Choice,
}

#[derive(Clone, Debug)]
struct ListenState {
    kind: ListenKind,
    prefix: String,
    wait_remaining: usize,
    speech_started: bool,
    partial_elapsed: usize,
    /// Сколько подряд фраз распознались пустыми (сигнал «слушаю» в динамиках,
    /// кашель, щелчок) — их не считаем концом ожидания, но не бесконечно.
    empty_segments: u8,
}

/// Пустых фраз подряд, после которых ожидание команды всё же завершается.
const MAX_EMPTY_SEGMENTS: u8 = 2;

#[derive(Clone, Debug)]
struct DictationState {
    /// Сколько ещё ждать речи до конца диктовки (сэмплы).
    idle_remaining: usize,
    speech_started: bool,
    partial_elapsed: usize,
}

#[derive(Clone, Debug)]
enum Mode {
    Idle,
    Listening(ListenState),
    Dictating(DictationState),
    Calibrating,
}

struct TrackingVoiceDetector<V: VoiceDetector> {
    inner: V,
    saw_speech: bool,
}

impl<V: VoiceDetector> VoiceDetector for TrackingVoiceDetector<V> {
    fn frame_len(&self) -> usize {
        self.inner.frame_len()
    }

    fn is_speech(&mut self, frame: &[i16]) -> bool {
        let speech = self.inner.is_speech(frame);
        self.saw_speech |= speech;
        speech
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.saw_speech = false;
    }
}

pub struct Engine<T: Transcriber, V: VoiceDetector> {
    cfg: EngineConfig,
    asr: T,
    vad: TrackingVoiceDetector<V>,
    segmenter: Segmenter,
    mode: Mode,
    /// Отсчёт mute отдельно от ожидания: оба таймера продвигаются по сэмплам.
    mute_remaining: usize,
    /// Публичные методы режимов не возвращают события, поэтому выдаём их при следующем push.
    pending_events: Vec<EngineEvent>,
    /// Отличает режим, в котором началась обработка чанка, от режима после транскрипции.
    mode_epoch: u64,
    /// Пользовательская грамматика: какие фразы — «незаконченная команда». Пока её нет
    /// (тесты, старт) — встроенный список глаголов `commands::is_bare_verb`.
    grammar: Option<Grammar>,
    second: Option<Box<dyn Transcriber + Send>>,
    lid: Option<Box<dyn LanguageId + Send>>,
    lang_mode: LangMode,
    /// Язык диктовки, выбранный голосом («по-английски»); `None` — автоматически.
    dictation_lang: Option<Lang>,
    spotter: Option<Box<dyn WakeSpotter + Send>>,
    /// Сколько сэмплов прошло с последнего срабатывания детектора wake-слова.
    spot_age: Option<usize>,
    /// Словарь англицизмов: «харднес» в русской диктовке → hardness.
    anglicisms: Option<Anglicisms>,
    /// Сэмплов с начала речи текущей фразы простоя (`None` — речи сейчас нет).
    idle_speech_age: Option<usize>,
}

/// Срабатывание детектора позже этого от начала речи — слово в середине фразы
/// («включи реверс»), а не обращение: wake говорят первым.
const SPOT_WINDOW_MS: u32 = 1_000;

/// Срабатывание детектора засчитывается фразе, если было не раньше этого до её конца
/// сверх длины самой фразы.
const SPOT_GRACE_MS: u32 = 1_500;

impl<T: Transcriber, V: VoiceDetector> Engine<T, V> {
    pub fn new(cfg: EngineConfig, asr: T, vad: V) -> Self {
        let mut segmenter = Segmenter::new(SegmenterConfig {
            end_silence_ms: cfg.wake_pause_ms,
            ..SegmenterConfig::default()
        });
        segmenter.set_end_silence_ms(cfg.wake_pause_ms);

        Self {
            cfg,
            asr,
            vad: TrackingVoiceDetector {
                inner: vad,
                saw_speech: false,
            },
            segmenter,
            mode: Mode::Idle,
            mute_remaining: 0,
            pending_events: Vec::new(),
            mode_epoch: 0,
            grammar: None,
            second: None,
            lid: None,
            lang_mode: LangMode::Auto,
            dictation_lang: None,
            spotter: None,
            spot_age: None,
            idle_speech_age: None,
            anglicisms: None,
        }
    }

    /// Подключает второй ASR и необязательный определитель языка цели.
    pub fn set_bilingual(
        &mut self,
        second: Option<Box<dyn Transcriber + Send>>,
        lid: Option<Box<dyn LanguageId + Send>>,
        mode: LangMode,
    ) {
        self.second = second;
        self.lid = lid;
        self.lang_mode = mode;
    }

    /// Словарь англицизмов для русской диктовки; `None` — текст как услышала модель.
    pub fn set_anglicisms(&mut self, anglicisms: Option<Anglicisms>) {
        self.anglicisms = anglicisms;
    }

    /// Фоновый детектор wake-слова (маленькая потоковая модель); `None` — только GigaAM.
    pub fn set_wake_spotter(&mut self, mut spotter: Option<Box<dyn WakeSpotter + Send>>) {
        if let Some(spotter) = spotter.as_deref_mut() {
            spotter.set_wake_words(&self.cfg.wake_words);
        }
        self.spotter = spotter;
        self.spot_age = None;
    }

    /// Главный вход: очередной кусок 16 кГц mono из микрофона.
    pub fn push_audio(&mut self, pcm: &[i16]) -> Vec<EngineEvent> {
        let mut out = std::mem::take(&mut self.pending_events);

        // В hotkey-only простое — настоящий простой: не зовём ни VAD, ни ASR и не храним звук.
        if pcm.is_empty() || (matches!(self.mode, Mode::Idle) && !self.cfg.wake_enabled) {
            return out;
        }

        // Детектор слушает только простой: там решается, было ли «ривер».
        if matches!(self.mode, Mode::Idle) {
            if let Some(age) = self.idle_speech_age.as_mut() {
                *age = age.saturating_add(pcm.len());
            }
            if let Some(spotter) = self.spotter.as_deref_mut() {
                let at_phrase_start = self
                    .idle_speech_age
                    .map_or(true, |age| age <= samples(SPOT_WINDOW_MS));
                if spotter.feed(pcm) && at_phrase_start {
                    self.spot_age = Some(0);
                    out.push(EngineEvent::WakeSpotted);
                } else if let Some(age) = self.spot_age.as_mut() {
                    *age = age.saturating_add(pcm.len());
                }
            }
        }

        let frame_len = self.vad.frame_len().max(1);
        let mut offset = 0;

        while offset < pcm.len() {
            if matches!(self.mode, Mode::Idle) && !self.cfg.wake_enabled {
                break;
            }

            let epoch = self.mode_epoch;
            let wait_at_chunk_start = self.wait_remaining();
            let is_muted = self.mute_remaining > 0;
            let mut count = (pcm.len() - offset).min(frame_len);

            if is_muted {
                count = count.min(self.mute_remaining);
            }
            if let Some(wait) = wait_at_chunk_start {
                if wait == 0 {
                    self.expire_listening(&mut out);
                    continue;
                }
                count = count.min(wait);
            }
            if let Some(partial_left) = self.partial_samples_until_next() {
                count = count.min(partial_left.max(1));
            }

            // Пока звучит сигнал активации, VAD видит нули и реальный звук не попадает в сегмент.
            let zeroed;
            let input = if is_muted {
                zeroed = vec![0; count];
                zeroed.as_slice()
            } else {
                &pcm[offset..offset + count]
            };
            self.vad.saw_speech = false;
            let segment_events = self.segmenter.push(input, &mut self.vad);
            let vad_saw_speech = self.vad.saw_speech;
            offset += count;

            if is_muted {
                self.mute_remaining = self.mute_remaining.saturating_sub(count);
            }

            let speech_start = segment_events
                .iter()
                .any(|event| matches!(event, SegmentEvent::SpeechStart));
            for event in segment_events {
                self.handle_segment_event(event, &mut out);
            }

            // Таймаут тратит только время до SpeechStart. При старте речи сегментатор выигрывает
            // у границы таймаута в этом же кадре.
            if self.mode_epoch == epoch
                && wait_at_chunk_start.is_some()
                && !speech_start
                && !vad_saw_speech
                && self.wait_remaining().is_some()
            {
                self.advance_wait(count, &mut out);
            }

            if self.mode_epoch == epoch && self.is_speech_started() {
                self.advance_partial(count, &mut out);
                // Ошибка partial распознавания уже вернула автомат в Idle: остаток этого входного
                // чанка не должен попасть в новый сегмент простоя.
                if self.mode_epoch != epoch {
                    break;
                }
            }
        }

        out
    }

    /// Грамматика из редактора: wake-слова и незаконченные фразы («ривер папка» →
    /// дослушать). Применяется сразу, в том числе посреди ожидания команды.
    pub fn set_grammar(&mut self, grammar: Grammar) {
        if !grammar.wake_words.is_empty() {
            self.cfg.wake_words = grammar.wake_words.clone();
            if let Some(spotter) = self.spotter.as_deref_mut() {
                spotter.set_wake_words(&self.cfg.wake_words);
            }
        }
        self.grammar = Some(grammar);
    }

    /// Хоткей push-to-talk: сразу ждать команду, без wake-слова.
    pub fn listen_command(&mut self) {
        self.begin_listening(ListenKind::Command, String::new());
        self.pending_events.push(EngineEvent::State("listening"));
    }

    /// Ждать номер варианта в окне выбора («третий», «номер 2»).
    pub fn listen_choice(&mut self) {
        self.begin_listening(ListenKind::Choice, String::new());
        self.pending_events.push(EngineEvent::State("listening"));
    }

    /// Окно выбора закрыто мышью/клавиатурой — номер больше не ждём.
    pub fn stop_choice(&mut self) {
        if matches!(&self.mode, Mode::Listening(state) if state.kind == ListenKind::Choice) {
            self.enter_idle();
            self.pending_events.push(EngineEvent::State("idle"));
        }
    }

    /// Ждать «да»/«отмена» на подтверждение опасной команды.
    pub fn listen_confirm(&mut self) {
        self.begin_listening(ListenKind::Confirm, String::new());
        self.pending_events.push(EngineEvent::State("listening"));
    }

    /// Режим калибровки: каждая фраза → `Calibration`, пока не `stop_calibration`.
    pub fn start_calibration(&mut self) {
        self.prepare_segmenter(self.cfg.command_pause_ms);
        self.mode = Mode::Calibrating;
        self.mute_remaining = samples(self.cfg.signal_mute_ms);
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
        self.pending_events.push(EngineEvent::State("listening"));
    }

    pub fn stop_calibration(&mut self) {
        self.enter_idle();
        self.pending_events.push(EngineEvent::State("idle"));
    }

    /// Пауза микрофона: вернуться в простой, затереть все буферы нулями.
    pub fn reset(&mut self) {
        if let Some(spotter) = self.spotter.as_deref_mut() {
            spotter.reset();
        }
        self.spot_age = None;
        self.idle_speech_age = None;
        self.segmenter.reset();
        self.vad.reset();
        self.segmenter.set_end_silence_ms(self.cfg.wake_pause_ms);
        self.mode = Mode::Idle;
        self.mute_remaining = 0;
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
        self.pending_events.clear();
        self.pending_events.push(EngineEvent::State("idle"));
    }

    fn begin_listening(&mut self, kind: ListenKind, prefix: String) {
        self.prepare_segmenter(self.cfg.command_pause_ms);
        self.mode = Mode::Listening(ListenState {
            kind,
            prefix,
            wait_remaining: samples(self.wait_ms(kind)),
            speech_started: false,
            partial_elapsed: 0,
            empty_segments: 0,
        });
        self.mute_remaining = samples(self.cfg.signal_mute_ms);
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
    }

    /// Сколько ждать начала речи: на подтверждение — своё время (по умолчанию 8 с), ведь
    /// сначала нужно услышать вопрос и сигнал.
    fn wait_ms(&self, kind: ListenKind) -> u32 {
        match kind {
            ListenKind::Command => self.cfg.command_wait_ms,
            ListenKind::Confirm => self.cfg.confirm_wait_ms,
            ListenKind::Choice => self.cfg.choice_wait_ms,
        }
    }

    fn prepare_segmenter(&mut self, end_silence_ms: u32) {
        self.segmenter.reset();
        self.vad.reset();
        let (pre_roll, soft_max, max) = COMMAND_LIMITS;
        self.segmenter.set_limits(pre_roll, soft_max, max);
        self.segmenter.set_end_silence_ms(end_silence_ms);
    }

    fn wait_remaining(&self) -> Option<usize> {
        match &self.mode {
            Mode::Listening(state) if !state.speech_started => Some(state.wait_remaining),
            Mode::Dictating(state) if !state.speech_started => Some(state.idle_remaining),
            _ => None,
        }
    }

    fn is_speech_started(&self) -> bool {
        match &self.mode {
            Mode::Listening(state) => state.speech_started,
            Mode::Dictating(state) => state.speech_started,
            _ => false,
        }
    }

    /// (началась ли речь, сколько сэмплов с последнего промежуточного текста)
    fn partial_progress(&mut self) -> Option<(&mut bool, &mut usize)> {
        match &mut self.mode {
            Mode::Listening(state) => Some((&mut state.speech_started, &mut state.partial_elapsed)),
            Mode::Dictating(state) => Some((&mut state.speech_started, &mut state.partial_elapsed)),
            _ => None,
        }
    }

    /// Интервал промежуточного текста: у живой диктовки он есть всегда.
    fn partial_interval(&self) -> usize {
        if matches!(self.mode, Mode::Dictating(_)) && self.cfg.dictation_live {
            samples(LIVE_PARTIAL_MS)
        } else {
            samples(self.cfg.partial_interval_ms)
        }
    }

    fn partial_samples_until_next(&self) -> Option<usize> {
        let interval = self.partial_interval();
        match &self.mode {
            Mode::Listening(state) if state.speech_started && interval > 0 => {
                Some(interval.saturating_sub(state.partial_elapsed))
            }
            Mode::Dictating(state) if state.speech_started && interval > 0 => {
                Some(interval.saturating_sub(state.partial_elapsed))
            }
            _ => None,
        }
    }

    fn advance_wait(&mut self, count: usize, out: &mut Vec<EngineEvent>) {
        let remaining = match &mut self.mode {
            Mode::Listening(state) if !state.speech_started => &mut state.wait_remaining,
            Mode::Dictating(state) if !state.speech_started => &mut state.idle_remaining,
            _ => return,
        };
        *remaining = remaining.saturating_sub(count);
        if *remaining == 0 {
            self.expire_listening(out);
        }
    }

    fn expire_listening(&mut self, out: &mut Vec<EngineEvent>) {
        match &self.mode {
            Mode::Listening(state) if !state.speech_started => match state.kind {
                ListenKind::Command => out.push(EngineEvent::Timeout),
                ListenKind::Choice => out.push(EngineEvent::Choice(String::new())),
                ListenKind::Confirm => {
                    out.push(EngineEvent::Log(format!(
                        "подтверждение: ответа не было {} с — отмена",
                        self.wait_ms(ListenKind::Confirm) / 1000
                    )));
                    out.push(EngineEvent::Confirm(false));
                }
            },
            Mode::Dictating(state) if !state.speech_started => {
                out.push(EngineEvent::DictationEnd(DictationEndReason::Silence))
            }
            _ => return,
        }
        self.return_to_idle(true, out);
    }

    fn advance_partial(&mut self, count: usize, out: &mut Vec<EngineEvent>) {
        let interval = self.partial_interval();
        if interval == 0 {
            return;
        }

        let should_transcribe = match self.partial_progress() {
            Some((speech_started, partial_elapsed)) if *speech_started => {
                *partial_elapsed = partial_elapsed.saturating_add(count);
                if *partial_elapsed >= interval {
                    *partial_elapsed %= interval;
                    true
                } else {
                    false
                }
            }
            _ => false,
        };

        if !should_transcribe || self.segmenter.current_audio().is_empty() {
            return;
        }
        if matches!(self.mode, Mode::Dictating(_))
            && self.segmenter.current_audio().len() > samples(DICTATION_PARTIAL_MAX_MS)
        {
            return;
        }

        // Живая диктовка: промежуточный текст «как сказано» — его печатают по ходу речи.
        if matches!(self.mode, Mode::Dictating(_)) && self.cfg.dictation_live {
            let result = {
                let audio = self.segmenter.current_audio();
                self.asr.transcribe_verbatim(audio)
            };
            match result {
                Ok(raw) if !raw.trim().is_empty() => {
                    let text = if self.dictation_lang == Some(Lang::En) {
                        raw
                    } else {
                        self.english_words(raw)
                    };
                    out.push(EngineEvent::DictationPartial(text));
                }
                Ok(_) => {}
                Err(error) => {
                    out.push(EngineEvent::Log(format!("Ошибка распознавания: {error}")));
                    out.push(EngineEvent::DictationEnd(DictationEndReason::Error));
                    self.return_to_idle(true, out);
                }
            }
            return;
        }

        let result = {
            let audio = self.segmenter.current_audio();
            self.asr.transcribe(audio)
        };
        match result {
            Ok(text) => {
                let text = self.strip_repeated_wake(&text);
                let prefix = match &self.mode {
                    Mode::Listening(state) => state.prefix.as_str(),
                    _ => "",
                };
                let partial = join_command(prefix, &text);
                if !partial.is_empty() {
                    out.push(EngineEvent::Partial(partial));
                }
            }
            Err(error) => {
                out.push(EngineEvent::Log(format!("Ошибка распознавания: {error}")));
                if matches!(self.mode, Mode::Dictating(_)) {
                    out.push(EngineEvent::DictationEnd(DictationEndReason::Error));
                }
                self.return_to_idle(true, out);
            }
        }
    }

    fn handle_segment_event(&mut self, event: SegmentEvent, out: &mut Vec<EngineEvent>) {
        match event {
            SegmentEvent::SpeechStart => {
                if matches!(self.mode, Mode::Idle) {
                    // SpeechStart приходит после min_speech_ms — речь уже идёт столько.
                    self.idle_speech_age = Some(samples(SegmenterConfig::default().min_speech_ms));
                }
                if let Some((speech_started, partial_elapsed)) = self.partial_progress() {
                    *speech_started = true;
                    *partial_elapsed = 0;
                }
            }
            SegmentEvent::SpeechEnd(mut audio) => {
                // Финализированный аудиосегмент принадлежит только одному вызову ASR.
                let mode = std::mem::replace(&mut self.mode, Mode::Idle);
                match mode {
                    Mode::Idle => {
                        self.idle_speech_age = None;
                        let result = self.asr.transcribe_timed(&audio);
                        self.prepare_segmenter(self.cfg.wake_pause_ms);
                        if self.cfg.wake_enabled {
                            self.handle_idle_transcript(result, &audio, out);
                        }
                        audio.fill(0);
                    }
                    Mode::Listening(state) => {
                        out.push(EngineEvent::State("busy"));
                        let result = match state.kind {
                            ListenKind::Command | ListenKind::Choice => self
                                .asr
                                .transcribe_timed(&audio)
                                .map(|timed| self.second_language_only(&audio, timed)),
                            // Английское «yes/no» русская модель не слышит вовсе (пустой
                            // текст) — тогда ответ слушает вторая модель, как и английскую цель.
                            ListenKind::Confirm => self
                                .asr
                                .transcribe(&audio)
                                .map(|text| Timed {
                                    text,
                                    words: Vec::new(),
                                })
                                .map(|timed| self.second_language_only(&audio, timed)),
                        };
                        match result {
                            // Пустая фраза — не ответ, а шум: ждём дальше с новым окном
                            // ожидания. Так сигнал «слушаю», пойманный микрофоном, не
                            // обрывает команду, и глушить аудио после сигнала не нужно.
                            Ok(timed)
                                if self.strip_repeated_wake(&timed.text).trim().is_empty()
                                    && state.empty_segments < MAX_EMPTY_SEGMENTS =>
                            {
                                self.prepare_segmenter(self.cfg.command_pause_ms);
                                self.mode = Mode::Listening(ListenState {
                                    wait_remaining: samples(self.wait_ms(state.kind)),
                                    speech_started: false,
                                    partial_elapsed: 0,
                                    empty_segments: state.empty_segments + 1,
                                    ..state
                                });
                                out.push(EngineEvent::State("listening"));
                            }
                            Ok(timed) => match state.kind {
                                ListenKind::Choice => {
                                    // «ривер …» вместо номера — новая команда, окно выбора
                                    // остаётся для мыши.
                                    if strip_initial_wake(&timed.text, &self.cfg.wake_words)
                                        .is_some()
                                    {
                                        out.push(EngineEvent::Choice(String::new()));
                                        self.enter_idle();
                                        self.handle_idle_transcript(Ok(timed), &audio, out);
                                        if matches!(self.mode, Mode::Idle) {
                                            out.push(EngineEvent::State("idle"));
                                        }
                                    } else {
                                        out.push(EngineEvent::Choice(timed.text));
                                        self.return_to_idle(true, out);
                                    }
                                }
                                ListenKind::Command => {
                                    let text = self.strip_repeated_wake(&timed.text);
                                    if text.trim().is_empty() {
                                        out.push(EngineEvent::Timeout);
                                    } else if state.prefix.trim().is_empty()
                                        && self.try_start_dictation(&audio, &timed, out)
                                    {
                                        audio.fill(0);
                                        return;
                                    } else {
                                        let resolved =
                                            self.resolve_command(&audio, &timed, &state.prefix);
                                        if resolved.text.trim().is_empty() {
                                            out.push(EngineEvent::Timeout);
                                        } else {
                                            out.push(EngineEvent::Command {
                                                text: resolved.text,
                                                alt: resolved.alt,
                                            });
                                        }
                                    }
                                    self.return_to_idle(true, out);
                                }
                                ListenKind::Confirm => {
                                    let text = self.strip_repeated_wake(&timed.text);
                                    let yes = is_yes(&text) || self.second_says_yes(&audio);
                                    // Ни «да», ни «нет»: это эхо сигнала подтверждения или шум
                                    // (английская модель выдумывает на нём «mm hmm»). Отказом это
                                    // не считаем — ждём настоящий ответ, но не бесконечно.
                                    let no = is_no(&text);
                                    if !yes && !no && state.empty_segments < MAX_EMPTY_SEGMENTS {
                                        out.push(EngineEvent::Log(
                                            "подтверждение: ни «да», ни «нет» — жду ответ".into(),
                                        ));
                                        self.prepare_segmenter(self.cfg.command_pause_ms);
                                        self.mode = Mode::Listening(ListenState {
                                            wait_remaining: samples(self.wait_ms(state.kind)),
                                            speech_started: false,
                                            partial_elapsed: 0,
                                            empty_segments: state.empty_segments + 1,
                                            ..state
                                        });
                                        out.push(EngineEvent::State("listening"));
                                    } else {
                                        if !yes {
                                            out.push(EngineEvent::Log(
                                                if no {
                                                    "подтверждение: услышан отказ"
                                                } else {
                                                    "подтверждение: ответ не разобран — отмена"
                                                }
                                                .into(),
                                            ));
                                        }
                                        out.push(EngineEvent::Confirm(yes));
                                        self.return_to_idle(true, out);
                                    }
                                }
                            },
                            Err(error) => {
                                out.push(EngineEvent::Log(format!(
                                    "Ошибка распознавания: {error}"
                                )));
                                self.return_to_idle(true, out);
                            }
                        }
                        audio.fill(0);
                    }
                    Mode::Dictating(_) => {
                        self.dictation_segment(&audio, out);
                        audio.fill(0);
                    }
                    Mode::Calibrating => {
                        let result = self.asr.transcribe_timed(&audio);
                        self.prepare_segmenter(self.cfg.command_pause_ms);
                        match result {
                            Ok(timed) => {
                                let resolved = self.resolve_command(&audio, &timed, "");
                                out.push(EngineEvent::Calibration {
                                    text: resolved.text,
                                    alt: resolved.alt,
                                });
                                self.mode = Mode::Calibrating;
                            }
                            Err(error) => {
                                out.push(EngineEvent::Log(format!(
                                    "Ошибка распознавания: {error}"
                                )));
                                self.return_to_idle(true, out);
                            }
                        }
                        audio.fill(0);
                    }
                }
            }
        }
    }

    fn handle_idle_transcript(
        &mut self,
        result: anyhow::Result<Timed>,
        audio: &[i16],
        out: &mut Vec<EngineEvent>,
    ) {
        let timed = match result {
            Ok(timed) => timed,
            Err(error) => {
                out.push(EngineEvent::Log(format!("Ошибка распознавания: {error}")));
                return;
            }
        };

        // Детектор слышал «ривер» в этой фразе, а GigaAM его не расслышала («река открой…»,
        // пустота) — чиним текст: похожее слово в начале становится wake-словом.
        let spotted = self
            .spot_age
            .is_some_and(|age| age <= audio.len() + samples(SPOT_GRACE_MS));
        let timed = if spotted && strip_initial_wake(&timed.text, &self.cfg.wake_words).is_none() {
            match repair_wake(&timed, &self.cfg.wake_words, audio.len(), &is_russian_word) {
                Some(repaired) => {
                    out.push(EngineEvent::Log("wake: «ривер» подтвердил фоновый детектор".into()));
                    repaired
                }
                None => {
                    out.push(EngineEvent::Log(
                        "wake: детектор слышал «ривер», большая модель — нет; фраза не принята".into(),
                    ));
                    timed
                }
            }
        } else {
            timed
        };
        let Some(rest) = strip_initial_wake(&timed.text, &self.cfg.wake_words) else {
            return;
        };
        self.spot_age = None;
        let rest = rest.trim();
        if self.try_start_dictation(audio, &timed, out) {
            return;
        }
        if rest.is_empty() || self.is_incomplete(rest) {
            out.push(EngineEvent::WakeHeard);
            self.begin_listening(ListenKind::Command, rest.to_string());
            out.push(EngineEvent::State("listening"));
        } else {
            out.push(EngineEvent::State("busy"));
            let resolved = self.resolve_command(audio, &timed, "");
            out.push(EngineEvent::Command {
                text: resolved.text,
                alt: resolved.alt,
            });
            out.push(EngineEvent::State("idle"));
        }
    }

    /// «напиши …» (намерение `Dictate`) — начать диктовку. `heard` — нормализованный текст
    /// этого аудио целиком (возможно, с wake-словом): по нему считается, сколько слов
    /// «как сказано» отрезать, чтобы осталась только цель — первый кусок текста.
    fn try_start_dictation(
        &mut self,
        audio: &[i16],
        timed: &Timed,
        out: &mut Vec<EngineEvent>,
    ) -> bool {
        let heard = timed.text.as_str();
        let body = self.strip_repeated_wake(heard);
        let payload = match self
            .grammar
            .as_ref()
            .and_then(|grammar| grammar.match_trigger(&body))
        {
            Some(matched) if matched.intent.kind == IntentKind::Dictate => matched.payload,
            _ => return false,
        };

        out.push(EngineEvent::DictationStart);
        out.push(EngineEvent::State("listening"));
        self.dictation_lang = None;
        let heard_words = heard.split_whitespace().count();
        let mut skip = heard_words.saturating_sub(payload.split_whitespace().count());
        if let Some((lang, words)) = leading_language_switch(&payload) {
            self.dictation_lang = Some(lang);
            skip += words;
        }
        // Даже при пустой цели звук после «напиши» может быть: английскую фразу русская
        // модель часто не слышит вовсе («ривер напиши hello…» → «ривер напиши»).
        if skip < heard_words || !timed.words.is_empty() {
            let first = self.first_dictation_chunk(audio, timed, skip);
            if !first.trim().is_empty() {
                out.push(EngineEvent::Dictation(first));
            }
        }
        self.begin_dictation();
        true
    }

    /// Текст после «напиши» в той же фразе. По таймингу слов вырезается звук цели, и он
    /// распознаётся заново обеими моделями — так «ривер напиши hello» даёт английский текст.
    /// Без тайминга — русский вариант всей фразы без первых `skip` слов.
    fn first_dictation_chunk(&mut self, audio: &[i16], timed: &Timed, skip: usize) -> String {
        let heard_words = timed.text.split_whitespace().count();
        if timed.words.len() == heard_words && skip > 0 && skip <= timed.words.len() {
            let start_s = match timed.words.get(skip) {
                Some(word) => (word.start - 0.15).max(timed.words[skip - 1].end),
                // Цели русская модель не услышала: берём всё после триггера. Конец последнего
                // слова в тайминге растянут до конца записи, поэтому он оценивается по длине
                // слова (~0,1 с на букву).
                None => {
                    let last = &timed.words[skip - 1];
                    let spoken = 0.15 + 0.1 * last.text.chars().count() as f32;
                    last.end.min(last.start + spoken)
                }
            };
            let start = ((start_s.max(0.0) * SAMPLE_RATE as f32) as usize).min(audio.len());
            let cropped = &audio[start..];
            let min_len = if skip < timed.words.len() { samples(200) } else { samples(600) };
            if cropped.len() >= min_len {
                let ru = self.asr.transcribe_verbatim(cropped).unwrap_or_default();
                return self.dictation_text(cropped, ru);
            }
            if skip >= timed.words.len() {
                return String::new();
            }
        }
        if skip >= heard_words {
            return String::new();
        }
        match self.asr.transcribe_verbatim(audio) {
            Ok(raw) => trim_words(&raw, skip, 0),
            Err(_) => String::new(),
        }
    }

    fn begin_dictation(&mut self) {
        let (pre_roll, soft_max, max) = DICTATION_LIMITS;
        self.prepare_segmenter(self.cfg.dictation_pause_ms);
        self.segmenter.set_limits(pre_roll, soft_max, max);
        self.mode = Mode::Dictating(DictationState {
            idle_remaining: samples(self.cfg.dictation_idle_ms.max(1_000)),
            speech_started: false,
            partial_elapsed: 0,
        });
        self.mute_remaining = 0;
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
    }

    /// Законченный кусок диктовки: фраза остановки, новая команда с wake-словом или текст.
    fn dictation_segment(&mut self, audio: &[i16], out: &mut Vec<EngineEvent>) {
        let raw = match self.asr.transcribe_verbatim(audio) {
            Ok(raw) => raw,
            Err(error) => {
                out.push(EngineEvent::Log(format!("Ошибка распознавания: {error}")));
                out.push(EngineEvent::DictationEnd(DictationEndReason::Error));
                self.return_to_idle(true, out);
                return;
            }
        };
        let norm = normalize(&raw);
        let stop_phrases = self
            .grammar
            .as_ref()
            .map(|grammar| grammar.dictation_stop.clone())
            .unwrap_or_else(|| Grammar::defaults().dictation_stop);

        if let Some(stop_words) = trailing_stop_words(&norm, &stop_phrases, &self.cfg.wake_words) {
            let text = trim_words(&raw, 0, stop_words);
            if !text.is_empty() {
                out.push(EngineEvent::Dictation(text));
            }
            out.push(EngineEvent::DictationEnd(DictationEndReason::Stopped));
            self.return_to_idle(true, out);
            return;
        }

        if let Some(lang) = language_switch(&norm) {
            self.dictation_lang = Some(lang);
            out.push(EngineEvent::Log(match lang {
                Lang::En => "✎ диктовка: английский".into(),
                Lang::Ru => "✎ диктовка: русский".into(),
            }));
            self.begin_dictation();
            out.push(EngineEvent::State("listening"));
            return;
        }

        let cancel_words = self
            .grammar
            .as_ref()
            .map(|grammar| grammar.cancel_words.clone())
            .unwrap_or_else(|| Grammar::defaults().cancel_words);
        // «ривер отмена» / «ривер стоп» — в начале куска или в конце («…и всё ривер отмена»):
        // диктовка отменяется, продиктованное стирается. Не команда «стоп»: нечёткий поиск
        // однажды запустил на неё Spotify.
        let cancel_at_start = strip_initial_wake(&norm, &self.cfg.wake_words).is_some_and(|rest| {
            !rest.trim().is_empty() && crate::textnorm::is_cancel(&rest, &cancel_words)
        });
        let words: Vec<&str> = norm.split_whitespace().collect();
        let cancel_at_end = words.len() >= 2
            && crate::textnorm::is_cancel(words[words.len() - 1], &cancel_words)
            && strip_initial_wake(words[words.len() - 2], &self.cfg.wake_words)
                .is_some_and(|rest| rest.trim().is_empty());
        if cancel_at_start || cancel_at_end {
            out.push(EngineEvent::DictationEnd(DictationEndReason::Cancelled));
            self.return_to_idle(true, out);
            return;
        }

        if strip_initial_wake(&norm, &self.cfg.wake_words).is_some() {
            // «ривер …» посреди диктовки — это снова команда: диктовка заканчивается,
            // а фраза разбирается так же, как в простое.
            out.push(EngineEvent::DictationEnd(DictationEndReason::Command));
            self.enter_idle();
            let timed = self.asr.transcribe_timed(audio);
            self.handle_idle_transcript(timed, audio, out);
            if matches!(self.mode, Mode::Idle) {
                out.push(EngineEvent::State("idle"));
            }
            return;
        }

        let text = self.dictation_text(audio, raw);
        if !text.trim().is_empty() {
            out.push(EngineEvent::Dictation(text));
        }
        self.begin_dictation();
        out.push(EngineEvent::State("listening"));
    }

    /// Язык куска диктовки. Русская модель — основная; вторая включается, если язык
    /// намерения «английский» или определитель уверенно слышит английский, а русский
    /// вариант не состоит из явно русских слов (то же вето, что у цели команды).
    fn dictation_text(&mut self, audio: &[i16], ru_raw: String) -> String {
        let mode = match self.dictation_lang {
            Some(Lang::Ru) => LangMode::Ru,
            Some(Lang::En) => LangMode::En,
            None => {
                let intent_lang = self
                    .grammar
                    .as_ref()
                    .and_then(|grammar| {
                        grammar
                            .intents
                            .iter()
                            .find(|i| i.kind == IntentKind::Dictate)
                    })
                    .map(|intent| intent.lang)
                    .unwrap_or(LangMode::Auto);
                if intent_lang == LangMode::Auto {
                    self.lang_mode
                } else {
                    intent_lang
                }
            }
        };
        if mode == LangMode::Ru {
            return self.english_words(ru_raw);
        }
        let Some(second) = self.second.as_deref_mut() else {
            return self.english_words(ru_raw);
        };
        // Английскую модель слушаем всегда: определитель языка на английском с русским
        // акцентом уверенно говорит «ru», а русская модель слышит транслит или пустоту.
        let en_raw = second.transcribe_verbatim(audio).unwrap_or_default();
        if mode == LangMode::En {
            return if en_raw.trim().is_empty() { ru_raw } else { en_raw };
        }
        let long_enough =
            audio.len() as f32 * 1000.0 / SAMPLE_RATE as f32 >= MIN_LID_DURATION_MS;
        let lid = match self.lid.as_deref_mut() {
            Some(lid) if long_enough => lid.detect(audio).ok(),
            _ => None,
        };
        match choose_language(&ru_raw, &en_raw, lid) {
            Lang::En => en_raw,
            Lang::Ru => {
                let mixed = self.mix_english_words(audio, ru_raw);
                self.english_words(mixed)
            }
        }
    }

    /// Транслит английских слов в русском куске — латиницей по словарю англицизмов
    /// (второй модели не нужно: на русской фразе она термин почти не слышит).
    fn english_words(&self, text: String) -> String {
        let Some(anglicisms) = &self.anglicisms else {
            return text;
        };
        let is_russian = |word: &str| {
            crate::spellcheck::misspelled_ratio("ru-RU", word)
                .map(|(ratio, count)| count == 1 && ratio == 0.0)
        };
        anglicisms.restore(&text, &is_russian)
    }

    /// Русский кусок с английскими словами внутри («настрой харднес» → «настрой hardness»):
    /// пословная подмена по таймингу слов обеих моделей (`dictation::mix_english_words` —
    /// только несловарное русское слово на словарное английское и похожее по звучанию).
    fn mix_english_words(&mut self, audio: &[i16], ru_raw: String) -> String {
        let Ok(ru_timed) = self.asr.transcribe_timed(audio) else {
            return ru_raw;
        };
        let Some(second) = self.second.as_deref_mut() else {
            return ru_raw;
        };
        let Ok(en_timed) = second.transcribe_timed(audio) else {
            return ru_raw;
        };
        let is_word = |lang: &str, word: &str| {
            crate::spellcheck::misspelled_ratio(lang, word).map(|(ratio, _)| ratio < 0.5)
        };
        crate::dictation::mix_english_words(&ru_raw, &ru_timed.words, &en_timed.words, &is_word)
    }

    /// Русская модель на чисто английском сегменте («ривер поиск ютуб» + пауза +
    /// «never gonna give you up») часто возвращает пустоту — и пустая фраза была бы принята
    /// за эхо сигнала. Прежде чем так решить, сегмент слушает вторая модель; её текст
    /// становится результатом без тайминга, и `resolve` берёт цель целиком.
    fn second_language_only(&mut self, audio: &[i16], timed: Timed) -> Timed {
        if !self.strip_repeated_wake(&timed.text).trim().is_empty() {
            return timed;
        }
        let Some(second) = self.second.as_deref_mut() else {
            return timed;
        };
        match second.transcribe(audio) {
            Ok(text) if !text.trim().is_empty() => Timed {
                text,
                words: Vec::new(),
            },
            _ => timed,
        }
    }

    /// Английское «yes» русская модель слышит как «яс»/«ес»/пустоту, а вторая — чётко.
    /// Согласие засчитывается, если «да» услышала любая из моделей; «нет» и всё прочее —
    /// отказ, так что лишняя проверка не может превратить отказ в согласие по ошибке ASR
    /// одной модели сильнее, чем это уже делает is_yes.
    fn second_says_yes(&mut self, audio: &[i16]) -> bool {
        let Some(second) = self.second.as_deref_mut() else {
            return false;
        };
        second
            .transcribe(audio)
            .map(|text| is_yes(&text))
            .unwrap_or(false)
    }

    fn resolve_command(&mut self, audio: &[i16], timed: &Timed, prefix: &str) -> Resolved {
        let fallback_grammar = Grammar::default();
        let grammar = self.grammar.as_ref().unwrap_or(&fallback_grammar).clone();
        bilingual::resolve(
            audio,
            timed,
            prefix,
            &self.cfg.wake_words,
            &grammar,
            self.second
                .as_deref_mut()
                .map(|transcriber| transcriber as &mut dyn Transcriber),
            self.lid
                .as_deref_mut()
                .map(|language_id| language_id as &mut dyn LanguageId),
            self.lang_mode,
        )
    }

    fn strip_repeated_wake(&self, text: &str) -> String {
        strip_initial_wake(text, &self.cfg.wake_words).unwrap_or_else(|| text.trim().to_string())
    }

    fn return_to_idle(&mut self, emit_state: bool, out: &mut Vec<EngineEvent>) {
        self.enter_idle();
        if emit_state {
            out.push(EngineEvent::State("idle"));
        }
    }

    fn enter_idle(&mut self) {
        if let Some(spotter) = self.spotter.as_deref_mut() {
            spotter.reset();
        }
        self.spot_age = None;
        self.idle_speech_age = None;
        self.mode = Mode::Idle;
        self.mute_remaining = 0;
        self.prepare_segmenter(self.cfg.wake_pause_ms);
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
    }
}

impl<T: Transcriber, V: VoiceDetector> Engine<T, V> {
    fn is_incomplete(&self, text: &str) -> bool {
        match &self.grammar {
            Some(g) => g.is_incomplete(text),
            None => is_bare_verb(text),
        }
    }
}

fn join_command(prefix: &str, text: &str) -> String {
    match (prefix.trim(), text.trim()) {
        ("", text) => text.to_string(),
        (prefix, "") => prefix.to_string(),
        (prefix, text) => format!("{prefix} {text}"),
    }
}

/// Фраза, где фоновый детектор слышал wake-слово, а GigaAM — нет. Чиним осторожно:
/// на корпусе без этого ограничения «мотор ревёт за окном» становилось командой.
/// - Короткий обрывок (до 1,5 с, не больше одного слова) — это одно «ривер».
/// - Иначе искажённое wake — слова в начале фразы (по таймингу: первые `WAKE_SPAN_MS`
///   после pre-roll, не больше двух; без тайминга — первое слово), и каждое из них должно
///   быть НЕ словарным русским словом («рибер», «ривир»), а не «река» / «ревёт».
///   `is_real_word` — проверка словаря; `None` (словаря нет) — не чиним.
fn repair_wake(
    timed: &Timed,
    wake_words: &[String],
    audio_len: usize,
    is_real_word: &dyn Fn(&str) -> Option<bool>,
) -> Option<Timed> {
    const WAKE_SPAN_MS: f32 = 500.0;
    let canonical = wake_words.first()?.clone();
    let words: Vec<&str> = timed.text.split_whitespace().collect();
    if words.len() <= 1 && audio_len <= samples(1_500) {
        return Some(Timed {
            text: canonical,
            words: Vec::new(),
        });
    }
    let aligned = !timed.words.is_empty() && timed.words.len() == words.len();
    let dropped = if aligned {
        let limit = (COMMAND_LIMITS.0 as f32 + WAKE_SPAN_MS) / 1000.0;
        timed
            .words
            .iter()
            .take(2)
            .take_while(|word| word.start < limit)
            .count()
    } else {
        1
    };
    if dropped == 0 || dropped >= words.len() {
        return None;
    }
    let all_garbled = words[..dropped]
        .iter()
        .all(|word| is_real_word(word) == Some(false));
    if !all_garbled {
        return None;
    }

    let mut text_words = vec![canonical.clone()];
    text_words.extend(words.iter().skip(dropped).map(|word| word.to_string()));
    let mut timed_words = Vec::new();
    if aligned {
        let first = timed.words[0].start;
        timed_words.push(TimedWord {
            text: canonical,
            start: first,
            end: timed.words[dropped].start,
        });
        timed_words.extend(timed.words.iter().skip(dropped).cloned());
    }
    Some(Timed {
        text: text_words.join(" "),
        words: timed_words,
    })
}

/// Словарное ли русское слово (проверка орфографии Windows); `None` — словаря нет.
fn is_russian_word(word: &str) -> Option<bool> {
    crate::spellcheck::misspelled_ratio("ru-RU", word).map(|(ratio, _)| ratio < 0.5)
}

/// Обращения, которые люди ставят перед именем: «эй ривер», «слушай ривер открой …».
const WAKE_LEAD_INS: [&str; 8] = ["эй", "ну", "а", "слушай", "окей", "ок", "так", "хей"];

fn strip_initial_wake(text: &str, wake_words: &[String]) -> Option<String> {
    // Нечёткость (опечатки ASR) решает textnorm::strip_wake, но он ищет wake в первых двух
    // словах. Вторым словом wake допустим только после обращения — иначе «открой ривер
    // загрузки» из обычной речи ложно становилось бы командой.
    let rest = strip_wake(text, wake_words)?;
    let mut words = text.split_whitespace();
    let first = words.next()?;
    let wake_first = starts_with_wake(text, wake_words);
    let lead_in = WAKE_LEAD_INS.contains(&first) && words.next().is_some();
    (wake_first || lead_in).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockTranscriber {
        texts: VecDeque<String>,
        calls: Rc<Cell<usize>>,
        inputs: Rc<RefCell<Vec<Vec<i16>>>>,
    }

    impl MockTranscriber {
        fn new(texts: &[&str]) -> (Self, Rc<Cell<usize>>, Rc<RefCell<Vec<Vec<i16>>>>) {
            let calls = Rc::new(Cell::new(0));
            let inputs = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    texts: texts.iter().map(|text| (*text).to_string()).collect(),
                    calls: calls.clone(),
                    inputs: inputs.clone(),
                },
                calls,
                inputs,
            )
        }
    }

    impl Transcriber for MockTranscriber {
        fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
            self.calls.set(self.calls.get() + 1);
            self.inputs.borrow_mut().push(pcm.to_vec());
            Ok(self.texts.pop_front().unwrap_or_default())
        }
    }

    struct FixedTranscriber {
        texts: VecDeque<String>,
    }

    impl FixedTranscriber {
        fn new(texts: &[&str]) -> Self {
            Self {
                texts: texts.iter().map(|text| (*text).to_string()).collect(),
            }
        }
    }

    impl Transcriber for FixedTranscriber {
        fn transcribe(&mut self, _pcm: &[i16]) -> anyhow::Result<String> {
            Ok(self.texts.pop_front().unwrap_or_default())
        }
    }

    struct CountingTranscriber(Arc<AtomicUsize>);

    impl Transcriber for CountingTranscriber {
        fn transcribe(&mut self, _pcm: &[i16]) -> anyhow::Result<String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok("unused".into())
        }
    }

    struct MockVoiceDetector {
        calls: Rc<Cell<usize>>,
    }

    impl MockVoiceDetector {
        fn new() -> (Self, Rc<Cell<usize>>) {
            let calls = Rc::new(Cell::new(0));
            (
                Self {
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    impl VoiceDetector for MockVoiceDetector {
        fn frame_len(&self) -> usize {
            160
        }

        fn is_speech(&mut self, frame: &[i16]) -> bool {
            self.calls.set(self.calls.get() + 1);
            frame.iter().any(|sample| *sample != 0)
        }

        fn reset(&mut self) {}
    }

    fn test_config() -> EngineConfig {
        EngineConfig {
            wake_pause_ms: 120,
            command_pause_ms: 120,
            command_wait_ms: 1_000,
            signal_mute_ms: 0,
            partial_interval_ms: 0,
            ..EngineConfig::default()
        }
    }

    fn engine(
        cfg: EngineConfig,
        texts: &[&str],
    ) -> (
        Engine<MockTranscriber, MockVoiceDetector>,
        Rc<Cell<usize>>,
        Rc<Cell<usize>>,
        Rc<RefCell<Vec<Vec<i16>>>>,
    ) {
        let (asr, asr_calls, inputs) = MockTranscriber::new(texts);
        let (vad, vad_calls) = MockVoiceDetector::new();
        (Engine::new(cfg, asr, vad), asr_calls, vad_calls, inputs)
    }

    fn phrase(marker: i16, end_silence_ms: u32) -> Vec<i16> {
        // 240 мс речи проходят min_speech_ms, после чего идёт конец фразы.
        let mut pcm = vec![marker; samples(240)];
        pcm.resize(samples(240 + end_silence_ms + 100), 0);
        pcm
    }

    fn contains(events: &[EngineEvent], wanted: &EngineEvent) -> bool {
        events.iter().any(|event| event == wanted)
    }

    #[test]
    fn bare_verb_detection_is_exact() {
        assert!(is_bare_verb("открой"));
        assert!(is_bare_verb("запусти"));
        assert!(!is_bare_verb("открой загрузки"));
        assert!(!is_bare_verb("переоткрой"));
    }

    #[test]
    fn grammar_trigger_without_target_waits_for_continuation() {
        // «ривер папка» + пауза + «загрузки»: ключевое слово из грамматики без цели —
        // сигнал и дослушивание, как раньше с голым глаголом.
        let (mut engine, _, _, _) = engine(test_config(), &["ривер папка", "загрузки"]);
        engine.set_grammar(crate::grammar::Grammar::defaults());
        let mut events = engine.push_audio(&phrase(123, 700));
        assert!(contains(&events, &EngineEvent::WakeHeard));
        events.extend(engine.push_audio(&phrase(123, 1_000)));
        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "папка загрузки".into(),
                alt: None
            }
        ));
    }

    #[test]
    fn wake_is_only_stripped_at_the_start_of_a_phrase() {
        let wake_words = vec!["ривер".to_string()];
        assert_eq!(
            strip_initial_wake("ривер открой загрузки", &wake_words),
            Some("открой загрузки".to_string())
        );
        assert_eq!(
            strip_initial_wake("открой ривер загрузки", &wake_words),
            None
        );
        // опечатка ASR в самом wake-слове и обращение перед ним
        assert_eq!(
            strip_initial_wake("ривир открой загрузки", &wake_words),
            Some("открой загрузки".to_string())
        );
        assert_eq!(
            strip_initial_wake("эй ривер открой загрузки", &wake_words),
            Some("открой загрузки".to_string())
        );
    }

    #[test]
    fn wake_disabled_idle_does_not_call_vad_or_asr() {
        let mut cfg = test_config();
        cfg.wake_enabled = false;
        let (mut engine, asr_calls, vad_calls, _) = engine(cfg, &["не использовать"]);

        let events = engine.push_audio(&vec![123; samples(300)]);

        assert!(events.is_empty());
        assert_eq!(asr_calls.get(), 0);
        assert_eq!(vad_calls.get(), 0);
    }

    #[test]
    fn contiguous_wake_and_command_is_dispatched_once() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой загрузки"]);
        let events = engine.push_audio(&phrase(123, 120));

        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(!contains(&events, &EngineEvent::WakeHeard));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, EngineEvent::Command { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn wake_pause_then_command_emits_one_wake_heard() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер", "открой загрузки"]);
        let wake = engine.push_audio(&phrase(123, 120));
        assert!(contains(&wake, &EngineEvent::WakeHeard));
        assert!(contains(&wake, &EngineEvent::State("listening")));

        let command = engine.push_audio(&phrase(123, 120));
        assert!(contains(
            &command,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert_eq!(
            wake.iter()
                .chain(command.iter())
                .filter(|event| matches!(event, EngineEvent::WakeHeard))
                .count(),
            1
        );
    }

    #[test]
    fn wake_with_bare_verb_is_joined_to_following_phrase() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой", "загрузки"]);
        let prefix = engine.push_audio(&phrase(123, 120));
        assert!(contains(&prefix, &EngineEvent::WakeHeard));
        assert!(!prefix
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));

        let continuation = engine.push_audio(&phrase(123, 120));
        assert!(contains(
            &continuation,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
    }

    #[test]
    fn repeated_wake_at_command_start_is_removed() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер", "ривер открой загрузки"]);
        let wake = engine.push_audio(&phrase(123, 120));
        assert!(contains(&wake, &EngineEvent::WakeHeard));

        let command = engine.push_audio(&phrase(123, 120));
        assert!(contains(
            &command,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
    }

    #[test]
    fn wake_times_out_when_no_command_speech_starts() {
        let mut cfg = test_config();
        cfg.command_wait_ms = 300;
        let (mut engine, _, _, _) = engine(cfg, &["ривер"]);
        let wake = engine.push_audio(&phrase(123, 120));
        assert!(contains(&wake, &EngineEvent::WakeHeard));

        let timeout = engine.push_audio(&vec![0; samples(300)]);
        assert!(contains(&timeout, &EngineEvent::Timeout));
        assert!(contains(&timeout, &EngineEvent::State("idle")));
    }

    #[test]
    fn command_wait_stops_counting_as_soon_as_speech_starts() {
        let mut cfg = test_config();
        cfg.command_wait_ms = 300;
        let (mut engine, _, _, _) = engine(cfg, &["открой загрузки"]);
        engine.listen_command();

        assert!(!contains(
            &engine.push_audio(&vec![0; samples(250)]),
            &EngineEvent::Timeout
        ));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(!contains(&events, &EngineEvent::Timeout));
    }

    #[test]
    fn hotkey_listen_command_accepts_a_command_without_wake() {
        let (mut engine, _, _, _) = engine(test_config(), &["открой загрузки"]);
        engine.listen_command();
        let events = engine.push_audio(&phrase(123, 120));

        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(contains(&events, &EngineEvent::State("listening")));
    }

    #[test]
    fn empty_segments_are_skipped_then_command_is_heard() {
        // Эхо сигнала «слушаю» и кашель распознаются пустыми — команда после них не теряется.
        let (mut engine, _, _, _) = engine(test_config(), &["", "", "открой загрузки"]);
        engine.listen_command();
        let mut events = engine.push_audio(&phrase(123, 120));
        events.extend(engine.push_audio(&phrase(123, 120)));
        assert!(!contains(&events, &EngineEvent::Timeout));
        events.extend(engine.push_audio(&phrase(123, 120)));

        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(!contains(&events, &EngineEvent::Timeout));
    }

    #[test]
    fn too_many_empty_segments_end_with_timeout() {
        let (mut engine, _, _, _) = engine(test_config(), &["", "", ""]);
        engine.listen_command();
        let mut events = Vec::new();
        for _ in 0..3 {
            events.extend(engine.push_audio(&phrase(123, 120)));
        }

        assert!(contains(&events, &EngineEvent::Timeout));
        assert!(contains(&events, &EngineEvent::State("idle")));
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
    }

    #[test]
    fn confirm_yes_no_and_timeout() {
        for (text, expected) in [("да", true), ("нет", false)] {
            let (mut engine, _, _, _) = engine(test_config(), &[text]);
            engine.listen_confirm();
            let events = engine.push_audio(&phrase(123, 120));
            assert!(contains(&events, &EngineEvent::Confirm(expected)));
        }

        let mut cfg = test_config();
        cfg.confirm_wait_ms = 300;
        let (mut engine, _, _, _) = engine(cfg, &[]);
        engine.listen_confirm();
        let events = engine.push_audio(&vec![0; samples(300)]);
        assert!(contains(&events, &EngineEvent::Confirm(false)));
        assert!(!contains(&events, &EngineEvent::Timeout));
    }

    #[test]
    fn calibration_emits_multiple_phrases_without_wake_words() {
        let (mut engine, _, _, _) =
            engine(test_config(), &["ривер открой загрузки", "файл студио"]);
        engine.start_calibration();

        let first = engine.push_audio(&phrase(123, 120));
        let second = engine.push_audio(&phrase(123, 120));

        assert!(contains(
            &first,
            &EngineEvent::Calibration {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(contains(
            &second,
            &EngineEvent::Calibration {
                text: "файл студио".into(),
                alt: None
            }
        ));
        engine.stop_calibration();
        assert!(contains(
            &engine.push_audio(&[]),
            &EngineEvent::State("idle")
        ));
    }

    #[test]
    fn reset_mid_command_discards_the_prefix_and_audio() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой", "загрузки"]);
        let wake = engine.push_audio(&phrase(123, 120));
        assert!(contains(&wake, &EngineEvent::WakeHeard));

        engine.reset();
        let reset_state = engine.push_audio(&[]);
        assert!(contains(&reset_state, &EngineEvent::State("idle")));
        let ignored = engine.push_audio(&phrase(123, 120));
        assert!(!ignored
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
    }

    #[test]
    fn phrase_without_wake_is_ignored() {
        let (mut engine, _, _, _) = engine(
            test_config(),
            &["открой загрузки", "пожалуйста ривер открой загрузки"],
        );
        let mut events = engine.push_audio(&phrase(123, 120));
        events.extend(engine.push_audio(&phrase(123, 120)));

        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::WakeHeard)));
    }

    #[test]
    fn partial_is_emitted_during_a_long_command() {
        let mut cfg = test_config();
        cfg.partial_interval_ms = 100;
        let (mut engine, _, _, _) = engine(
            cfg,
            &["открой загрузки", "открой загрузки", "открой загрузки"],
        );
        engine.listen_command();
        let events = engine.push_audio(&phrase(123, 120));

        assert!(events
            .iter()
            .any(|event| matches!(event, EngineEvent::Partial(text) if !text.is_empty())));
        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
    }

    #[test]
    fn signal_mute_replaces_audio_with_zeros_and_does_not_keep_it() {
        let mut cfg = test_config();
        cfg.signal_mute_ms = 250;
        let (mut engine, _, _, inputs) = engine(cfg, &["ривер", "открой загрузки"]);
        let wake = engine.push_audio(&phrase(123, 120));
        assert!(contains(&wake, &EngineEvent::WakeHeard));

        // 100 мс тишины из хвоста первой push-фразы уже прошли в mute-режиме.
        let muted = vec![111; samples(100)];
        let muted_events = engine.push_audio(&muted);
        assert!(!muted_events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
        let command = engine.push_audio(&phrase(222, 120));
        assert!(contains(
            &command,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(!inputs.borrow()[1].contains(&111));
    }

    #[test]
    fn signal_mute_is_applied_to_hotkey_confirm_and_calibration() {
        let mut cfg = test_config();
        cfg.signal_mute_ms = 100;

        let (mut command_engine, _, _, command_inputs) = engine(cfg.clone(), &["открой загрузки"]);
        command_engine.listen_command();
        let muted_events = command_engine.push_audio(&vec![111; samples(100)]);
        assert!(!muted_events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. } | EngineEvent::Timeout)));
        assert!(contains(
            &command_engine.push_audio(&phrase(222, 120)),
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(!command_inputs.borrow()[0].contains(&111));

        let (mut confirm_engine, _, _, confirm_inputs) = engine(cfg.clone(), &["да"]);
        confirm_engine.listen_confirm();
        let muted_events = confirm_engine.push_audio(&vec![111; samples(100)]);
        assert!(!muted_events
            .iter()
            .any(|event| matches!(event, EngineEvent::Confirm(_))));
        assert!(contains(
            &confirm_engine.push_audio(&phrase(222, 120)),
            &EngineEvent::Confirm(true)
        ));
        assert!(!confirm_inputs.borrow()[0].contains(&111));

        let (mut calibration_engine, _, _, calibration_inputs) = engine(cfg, &["файл студио"]);
        calibration_engine.start_calibration();
        let muted_events = calibration_engine.push_audio(&vec![111; samples(100)]);
        assert!(!muted_events
            .iter()
            .any(|event| matches!(event, EngineEvent::Calibration { .. })));
        assert!(contains(
            &calibration_engine.push_audio(&phrase(222, 120)),
            &EngineEvent::Calibration {
                text: "файл студио".into(),
                alt: None
            }
        ));
        assert!(!calibration_inputs.borrow()[0].contains(&111));
    }

    #[test]
    fn transcribe_error_logs_and_returns_to_idle() {
        struct ErrorTranscriber;
        impl Transcriber for ErrorTranscriber {
            fn transcribe(&mut self, _pcm: &[i16]) -> anyhow::Result<String> {
                anyhow::bail!("mock failure")
            }
        }

        let (vad, _) = MockVoiceDetector::new();
        let mut engine = Engine::new(test_config(), ErrorTranscriber, vad);
        engine.listen_command();
        let events = engine.push_audio(&phrase(123, 120));

        assert!(events.iter().any(
            |event| matches!(event, EngineEvent::Log(message) if message.contains("mock failure"))
        ));
        assert!(contains(&events, &EngineEvent::State("idle")));
        assert!(!contains(&events, &EngineEvent::Timeout));
    }

    #[test]
    fn second_transcriber_populates_alt_for_command() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой загрузки"]);
        engine.set_bilingual(
            Some(Box::new(FixedTranscriber::new(&["downloads"]))),
            None,
            LangMode::En,
        );

        let events = engine.push_audio(&phrase(123, 120));
        assert!(events.iter().any(|event| matches!(event,
            EngineEvent::Command { text, alt: Some(alt) }
                if text == "открой downloads" && alt == "открой загрузки"
        )));
    }

    #[test]
    fn second_transcriber_populates_alt_for_hotkey_command() {
        let (mut engine, _, _, _) = engine(test_config(), &["открой загрузки"]);
        engine.set_bilingual(
            Some(Box::new(FixedTranscriber::new(&["downloads"]))),
            None,
            LangMode::En,
        );
        engine.listen_command();

        let events = engine.push_audio(&phrase(123, 120));
        assert!(events.iter().any(|event| matches!(event,
            EngineEvent::Command { text, alt: Some(alt) }
                if text == "открой downloads" && alt == "открой загрузки"
        )));
    }

    #[test]
    fn missing_second_transcriber_leaves_alt_empty() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой загрузки"]);
        engine.set_bilingual(None, None, LangMode::En);

        let events = engine.push_audio(&phrase(123, 120));
        assert!(events.iter().any(|event| matches!(event,
            EngineEvent::Command { text, alt: None } if text == "открой загрузки"
        )));
    }

    #[test]
    fn calibration_also_emits_the_resolved_alternate() {
        let (mut engine, _, _, _) = engine(test_config(), &["открой загрузки"]);
        engine.start_calibration();
        engine.set_bilingual(
            Some(Box::new(FixedTranscriber::new(&["downloads"]))),
            None,
            LangMode::En,
        );

        let events = engine.push_audio(&phrase(123, 120));
        assert!(events.iter().any(|event| matches!(event,
            EngineEvent::Calibration { text, alt: Some(alt) }
                if text == "открой downloads" && alt == "открой загрузки"
        )));
    }

    #[test]
    fn confirm_never_calls_the_second_transcriber() {
        let (mut engine, _, _, _) = engine(test_config(), &["да"]);
        let calls = Arc::new(AtomicUsize::new(0));
        engine.set_bilingual(
            Some(Box::new(CountingTranscriber(calls.clone()))),
            None,
            LangMode::En,
        );
        engine.listen_confirm();

        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(&events, &EngineEvent::Confirm(true)));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    fn dictation_engine(texts: &[&str]) -> Engine<MockTranscriber, MockVoiceDetector> {
        let mut cfg = test_config();
        cfg.dictation_pause_ms = 120;
        cfg.dictation_idle_ms = 1_000;
        let (mut engine, _, _, _) = engine(cfg, texts);
        engine.set_grammar(crate::grammar::Grammar::defaults());
        engine
    }

    fn dictated(events: &[EngineEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::Dictation(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dictation_in_one_breath_types_the_payload_and_keeps_listening() {
        // Mock: транскрипция фразы простоя, затем её же «как сказано».
        let mut engine = dictation_engine(&[
            "ривер напиши привет мир",
            "ривер напиши привет мир",
            "как дела",
            "стоп ввод",
        ]);
        let mut events = engine.push_audio(&phrase(123, 700));
        assert!(contains(&events, &EngineEvent::DictationStart));
        assert!(!contains(&events, &EngineEvent::WakeHeard));
        assert_eq!(dictated(&events), ["привет мир"]);

        // короткая пауза (с хвостом первой фразы — меньше тайм-аута 1 с): диктовка идёт
        events = engine.push_audio(&vec![0; samples(200)]);
        events.extend(engine.push_audio(&phrase(123, 200)));
        assert_eq!(dictated(&events), ["как дела"]);

        events = engine.push_audio(&phrase(123, 200));
        assert!(dictated(&events).is_empty());
        assert!(contains(
            &events,
            &EngineEvent::DictationEnd(DictationEndReason::Stopped)
        ));
        assert!(contains(&events, &EngineEvent::State("idle")));
    }

    #[test]
    fn dictation_after_a_pause_starts_without_the_wake_signal() {
        let mut engine = dictation_engine(&["ривер напиши", "длинный текст письма"]);
        let mut events = engine.push_audio(&phrase(123, 700));
        assert!(contains(&events, &EngineEvent::DictationStart));
        assert!(!contains(&events, &EngineEvent::WakeHeard));
        events.extend(engine.push_audio(&phrase(123, 200)));
        assert_eq!(dictated(&events), ["длинный текст письма"]);
    }

    #[test]
    fn dictation_ends_after_idle_silence() {
        let mut engine = dictation_engine(&["ривер напиши"]);
        let events = engine.push_audio(&phrase(123, 700));
        assert!(contains(&events, &EngineEvent::DictationStart));
        let events = engine.push_audio(&vec![0; samples(1_100)]);
        assert!(contains(
            &events,
            &EngineEvent::DictationEnd(DictationEndReason::Silence)
        ));
        assert!(contains(&events, &EngineEvent::State("idle")));
    }

    #[test]
    fn stop_phrase_at_the_end_keeps_the_text_before_it() {
        let mut engine = dictation_engine(&["ривер напиши", "вот и всё стоп ввод"]);
        engine.push_audio(&phrase(123, 700));
        let events = engine.push_audio(&phrase(123, 200));
        assert_eq!(dictated(&events), ["вот и всё"]);
        assert!(contains(
            &events,
            &EngineEvent::DictationEnd(DictationEndReason::Stopped)
        ));
    }

    #[test]
    fn river_stop_phrase_ends_dictation() {
        let mut engine = dictation_engine(&["ривер напиши", "ривер остановись"]);
        engine.push_audio(&phrase(123, 700));
        let events = engine.push_audio(&phrase(123, 200));
        assert!(dictated(&events).is_empty());
        assert!(contains(
            &events,
            &EngineEvent::DictationEnd(DictationEndReason::Stopped)
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
    }

    #[test]
    fn wake_command_during_dictation_ends_it_and_runs_the_command() {
        let mut engine = dictation_engine(&[
            "ривер напиши",
            "ривер открой загрузки",
            "ривер открой загрузки",
        ]);
        engine.push_audio(&phrase(123, 700));
        let events = engine.push_audio(&phrase(123, 200));
        assert!(contains(
            &events,
            &EngineEvent::DictationEnd(DictationEndReason::Command)
        ));
        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
        assert!(dictated(&events).is_empty());
    }

    #[test]
    fn confirm_ignores_echo_that_is_neither_yes_nor_no() {
        // эхо сигнала, распознанное как «мм», не отменяет команду — ждём ответа
        let (mut engine, _, _, _) = engine(test_config(), &["мм", "да"]);
        engine.listen_confirm();
        let mut events = engine.push_audio(&phrase(123, 120));
        assert!(!events.iter().any(|event| matches!(event, EngineEvent::Confirm(_))));
        events.extend(engine.push_audio(&phrase(123, 120)));
        assert!(contains(&events, &EngineEvent::Confirm(true)));
    }

    struct ScriptedSpotter(VecDeque<bool>);

    impl WakeSpotter for ScriptedSpotter {
        fn feed(&mut self, _pcm: &[i16]) -> bool {
            self.0.pop_front().unwrap_or(false)
        }
        fn reset(&mut self) {}
        fn set_wake_words(&mut self, _wake_words: &[String]) {}
    }

    #[test]
    fn spotted_wake_repairs_a_misheard_wake_word() {
        // GigaAM услышала «рибер открой загрузки», детектор — «ривер».
        let (mut engine, _, _, _) = engine(test_config(), &["рибер открой загрузки"]);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([true])))));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(&events, &EngineEvent::WakeSpotted), "{events:?}");
        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ), "{events:?}");
    }

    #[test]
    fn without_a_spot_misheard_wake_is_ignored() {
        let (mut engine, _, _, _) = engine(test_config(), &["река открой загрузки"]);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::new()))));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(!events.iter().any(|event| matches!(event, EngineEvent::Command { .. })));
    }

    #[test]
    fn repair_needs_a_garbled_non_word_at_the_start() {
        let wake = vec!["ривер".to_string()];
        let dict = |word: &str| Some(matches!(word, "мотор" | "ревет" | "река" | "за" | "окном"));
        let timed = |text: &str| Timed {
            text: text.into(),
            words: Vec::new(),
        };
        let long = samples(3_000);
        assert_eq!(
            repair_wake(&timed("рибер открой хром"), &wake, long, &dict).map(|t| t.text),
            Some("ривер открой хром".into())
        );
        assert_eq!(repair_wake(&timed("мотор ревет за окном"), &wake, long, &dict), None);
        assert_eq!(repair_wake(&timed("река открой хром"), &wake, long, &dict), None);
        assert_eq!(
            repair_wake(&timed("ре"), &wake, samples(800), &dict).map(|t| t.text),
            Some("ривер".into())
        );
        let no_dictionary = |_: &str| None;
        assert_eq!(repair_wake(&timed("рибер открой хром"), &wake, long, &no_dictionary), None);
    }

    #[test]
    fn spotted_bare_wake_with_empty_transcript_starts_listening() {
        let (mut engine, _, _, _) = engine(test_config(), &[""]);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([true])))));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(&events, &EngineEvent::WakeHeard));
    }

    #[test]
    fn river_cancel_word_during_dictation_just_stops_it() {
        for word in ["стоп", "stop", "хватит"] {
            let heard = format!("ривер {word}");
            let mut engine = dictation_engine(&["ривер напиши", &heard]);
            engine.push_audio(&phrase(123, 700));
            let events = engine.push_audio(&phrase(123, 200));
            assert!(
                contains(&events, &EngineEvent::DictationEnd(DictationEndReason::Cancelled)),
                "{word}: {events:?}"
            );
            assert!(!events.iter().any(|event| matches!(event, EngineEvent::Command { .. })));
        }
    }

    #[test]
    fn choice_mode_returns_the_spoken_number_or_times_out() {
        let (mut spoken, _, _, _) = engine(test_config(), &["третий"]);
        spoken.listen_choice();
        let events = spoken.push_audio(&phrase(123, 120));
        assert!(contains(&events, &EngineEvent::Choice("третий".into())));

        let mut cfg = test_config();
        cfg.choice_wait_ms = 300;
        let (mut silent, _, _, _) = engine(cfg, &[]);
        silent.listen_choice();
        let events = silent.push_audio(&vec![0; samples(300)]);
        assert!(contains(&events, &EngineEvent::Choice(String::new())));
    }

    #[test]
    fn wake_command_during_choice_is_a_new_command() {
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой загрузки"]);
        engine.listen_choice();
        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(
            &events,
            &EngineEvent::Command {
                text: "открой загрузки".into(),
                alt: None
            }
        ));
    }

    #[test]
    fn river_cancel_at_the_end_of_a_chunk_cancels_the_dictation() {
        let mut engine = dictation_engine(&["ривер напиши", "и вот это всё ривер отмена"]);
        engine.push_audio(&phrase(123, 700));
        let events = engine.push_audio(&phrase(123, 200));
        assert!(contains(&events, &EngineEvent::DictationEnd(DictationEndReason::Cancelled)), "{events:?}");
        assert!(dictated(&events).is_empty());
    }

    #[test]
    fn live_dictation_emits_verbatim_partials() {
        let mut cfg = test_config();
        cfg.dictation_pause_ms = 120;
        cfg.dictation_live = true;
        let (mut engine, _, _, _) = engine(cfg, &["ривер напиши", "привет", "привет мир", "привет мир"]);
        engine.set_grammar(crate::grammar::Grammar::defaults());
        engine.push_audio(&phrase(123, 700));
        // длинная фраза: 900 мс речи — успевают промежуточные распознавания
        let mut pcm = vec![123; samples(900)];
        pcm.resize(samples(900 + 300), 0);
        let events = engine.push_audio(&pcm);
        assert!(
            events.iter().any(|e| matches!(e, EngineEvent::DictationPartial(t) if t.starts_with("привет"))),
            "{events:?}"
        );
    }
}
