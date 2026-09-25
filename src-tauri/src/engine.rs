//! Автомат прослушивания без Tauri: wake → команда / подтверждение / калибровка.
//! Владелец: воркер `engine`.

use crate::bilingual::{self, LangMode, Resolved};
use crate::commands::is_bare_verb;
use crate::grammar::Grammar;
use crate::segmenter::{SegmentEvent, Segmenter, SegmenterConfig};
use crate::speech::{samples, LanguageId, Timed, Transcriber, VoiceDetector};
use crate::textnorm::{is_yes, starts_with_wake, strip_wake};

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
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    /// "idle" | "listening" | "busy" — для `reever://state`.
    State(&'static str),
    /// Сказали только wake-слово (или wake + голый глагол): играть сигнал «слушаю».
    WakeHeard,
    /// Промежуточный текст текущей фразы — для `reever://partial`.
    Partial(String),
    /// Готовая команда без wake-слова (нормализованная).
    Command { text: String, alt: Option<String> },
    /// Фраза, сказанная в режиме калибровки (без wake-слова).
    Calibration { text: String, alt: Option<String> },
    /// Ответ на подтверждение: true — «да».
    Confirm(bool),
    /// После wake/хоткея команды не дождались.
    Timeout,
    /// Диагностика для лога (без распознанного текста — приватность решает glue).
    Log(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenKind {
    Command,
    Confirm,
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
enum Mode {
    Idle,
    Listening(ListenState),
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
}

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

    /// Главный вход: очередной кусок 16 кГц mono из микрофона.
    pub fn push_audio(&mut self, pcm: &[i16]) -> Vec<EngineEvent> {
        let mut out = std::mem::take(&mut self.pending_events);

        // В hotkey-only простое — настоящий простой: не зовём ни VAD, ни ASR и не храним звук.
        if pcm.is_empty() || (matches!(self.mode, Mode::Idle) && !self.cfg.wake_enabled) {
            return out;
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
        }
        self.grammar = Some(grammar);
    }

    /// Хоткей push-to-talk: сразу ждать команду, без wake-слова.
    pub fn listen_command(&mut self) {
        self.begin_listening(ListenKind::Command, String::new());
        self.pending_events.push(EngineEvent::State("listening"));
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
            wait_remaining: samples(self.cfg.command_wait_ms),
            speech_started: false,
            partial_elapsed: 0,
            empty_segments: 0,
        });
        self.mute_remaining = samples(self.cfg.signal_mute_ms);
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
    }

    fn prepare_segmenter(&mut self, end_silence_ms: u32) {
        self.segmenter.reset();
        self.vad.reset();
        self.segmenter.set_end_silence_ms(end_silence_ms);
    }

    fn wait_remaining(&self) -> Option<usize> {
        match &self.mode {
            Mode::Listening(state) if !state.speech_started => Some(state.wait_remaining),
            _ => None,
        }
    }

    fn is_speech_started(&self) -> bool {
        matches!(&self.mode, Mode::Listening(state) if state.speech_started)
    }

    fn partial_samples_until_next(&self) -> Option<usize> {
        let interval = samples(self.cfg.partial_interval_ms);
        match &self.mode {
            Mode::Listening(state) if state.speech_started && interval > 0 => {
                Some(interval.saturating_sub(state.partial_elapsed))
            }
            _ => None,
        }
    }

    fn advance_wait(&mut self, count: usize, out: &mut Vec<EngineEvent>) {
        if let Mode::Listening(state) = &mut self.mode {
            if !state.speech_started {
                state.wait_remaining = state.wait_remaining.saturating_sub(count);
                if state.wait_remaining == 0 {
                    self.expire_listening(out);
                }
            }
        }
    }

    fn expire_listening(&mut self, out: &mut Vec<EngineEvent>) {
        let kind = match &self.mode {
            Mode::Listening(state) if !state.speech_started => state.kind,
            _ => return,
        };

        match kind {
            ListenKind::Command => out.push(EngineEvent::Timeout),
            ListenKind::Confirm => out.push(EngineEvent::Confirm(false)),
        }
        self.return_to_idle(true, out);
    }

    fn advance_partial(&mut self, count: usize, out: &mut Vec<EngineEvent>) {
        let interval = samples(self.cfg.partial_interval_ms);
        if interval == 0 {
            return;
        }

        let should_transcribe = if let Mode::Listening(state) = &mut self.mode {
            if state.speech_started {
                state.partial_elapsed = state.partial_elapsed.saturating_add(count);
                if state.partial_elapsed >= interval {
                    state.partial_elapsed %= interval;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if !should_transcribe || self.segmenter.current_audio().is_empty() {
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
                self.return_to_idle(true, out);
            }
        }
    }

    fn handle_segment_event(&mut self, event: SegmentEvent, out: &mut Vec<EngineEvent>) {
        match event {
            SegmentEvent::SpeechStart => {
                if let Mode::Listening(state) = &mut self.mode {
                    state.speech_started = true;
                    state.partial_elapsed = 0;
                }
            }
            SegmentEvent::SpeechEnd(mut audio) => {
                // Финализированный аудиосегмент принадлежит только одному вызову ASR.
                let mode = std::mem::replace(&mut self.mode, Mode::Idle);
                match mode {
                    Mode::Idle => {
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
                            ListenKind::Command => self.asr.transcribe_timed(&audio).map(|timed| {
                                self.second_language_only(&audio, timed)
                            }),
                            ListenKind::Confirm => self.asr.transcribe(&audio).map(|text| Timed {
                                text,
                                words: Vec::new(),
                            }),
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
                                    wait_remaining: samples(self.cfg.command_wait_ms),
                                    speech_started: false,
                                    partial_elapsed: 0,
                                    empty_segments: state.empty_segments + 1,
                                    ..state
                                });
                                out.push(EngineEvent::State("listening"));
                            }
                            Ok(timed) => match state.kind {
                                ListenKind::Command => {
                                    let text = self.strip_repeated_wake(&timed.text);
                                    if text.trim().is_empty() {
                                        out.push(EngineEvent::Timeout);
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
                                    out.push(EngineEvent::Confirm(is_yes(&text)));
                                    self.return_to_idle(true, out);
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

        let Some(rest) = strip_initial_wake(&timed.text, &self.cfg.wake_words) else {
            return;
        };
        let rest = rest.trim();
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
            Ok(text) if !text.trim().is_empty() => Timed { text, words: Vec::new() },
            _ => timed,
        }
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
        cfg.command_wait_ms = 300;
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
}
