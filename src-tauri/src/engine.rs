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
    samples, Lang, LanguageId, Timed, TimedWord, Transcriber, VoiceDetector, WakeSpotter,
    SAMPLE_RATE,
};
use crate::textnorm::{
    is_no, is_yes, normalize, starts_with_wake, strip_leading_fillers, strip_wake,
};

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
            dictation_idle_ms: 3_000,
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
    /// Большая модель расслышала фразу после `WakeSpotted`, но wake-слова в ней нет.
    WakeRejected,
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
    Log(crate::uilang::LogText),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationEndReason {
    /// Долго не было речи (`dictation_idle_ms`).
    Silence,
    /// Сказали фразу остановки («стоп ввод», «ривер остановись»).
    Stopped,
    /// «Ривер отмена» — диктовка отменена, продиктованное стирается («ривер стоп» — `Stopped`).
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
/// Длиннее этого кусок пересчитывается реже: распознавание всего куска дорожает.
const DICTATION_PARTIAL_SLOW_MS: u32 = 10_000;
/// Интервал промежуточного текста у длинного куска.
const LIVE_PARTIAL_SLOW_MS: u32 = 1_000;

/// Пауза перед «ривер» посреди фразы, после которой это обращение, а не слово в разговоре
/// («я ривер потестил» — без паузы, «…ну короче. Ривер, открой ютуб» — с паузой).
const MID_WAKE_PAUSE_MS: usize = 250;

/// Самая длинная тишина (мс), которая заканчивается у `at` (± слово начинается чуть раньше
/// или позже отметки модели). Тишина — кадры 10 мс в 10 раз тише речи этой фразы.
fn pause_before(audio: &[i16], at: usize) -> usize {
    const FRAME: usize = 160;
    let energies: Vec<f64> = audio
        .chunks(FRAME)
        .map(|frame| {
            (frame.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / frame.len() as f64).sqrt()
        })
        .collect();
    if energies.is_empty() {
        return 0;
    }
    let mut sorted = energies.clone();
    sorted.sort_by(f64::total_cmp);
    let speech = sorted[(sorted.len() * 9 / 10).min(sorted.len() - 1)];
    let quiet = speech * 0.1;
    let at_frame = (at / FRAME).min(energies.len());
    // Слово могло начаться до отметки модели на ~150 мс: ищем тишину, кончающуюся рядом.
    let mut best = 0;
    for end in at_frame.saturating_sub(15)..=at_frame.min(energies.len()) {
        let run = energies[..end]
            .iter()
            .rev()
            .take_while(|&&energy| energy < quiet)
            .count();
        best = best.max(run);
    }
    best * FRAME * 1000 / SAMPLE_RATE as usize
}

/// Начало «ривер» посреди фразы (сэмпл, с небольшим запасом), если перед ним пауза —
/// или если сразу за ним идёт команда (`is_command`: «закрой калькулятор», «калькулятор»). В звонке
/// обращаются на одном дыхании («нет вот он ривер закрой калькулятор», пауза 240 мс),
/// а «он не сработал на ривер» без команды после — просто разговор.
fn mid_phrase_wake(
    timed: &Timed,
    audio: &[i16],
    wake_words: &[String],
    is_command: &dyn Fn(&str) -> bool,
) -> Option<usize> {
    timed
        .words
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, word)| {
            let is_wake = strip_initial_wake(&word.text, wake_words)
                .is_some_and(|rest| rest.trim().is_empty());
            if !is_wake {
                return None;
            }
            let at = ((word.start.max(0.0) * SAMPLE_RATE as f32) as usize).min(audio.len());
            let after: Vec<&str> = timed.words[index + 1..]
                .iter()
                .map(|w| w.text.as_str())
                .collect();
            let commanded = !after.is_empty() && is_command(&after.join(" "));
            (commanded || pause_before(audio, at) >= MID_WAKE_PAUSE_MS)
                .then(|| at.saturating_sub(samples(80)))
        })
}

/// Слова, которые в диктовке после «ривер» заканчивают её, оставляя текст («ривер стоп»);
/// остальные слова отмены («ривер отмена») стирают продиктованное.
fn is_dictation_stop(text: &str) -> bool {
    let words: Vec<String> = normalize(text)
        .split_whitespace()
        .map(String::from)
        .collect();
    !words.is_empty()
        && words.len() <= 2
        && words.iter().any(|word| {
            matches!(
                word.as_str(),
                "стоп" | "stop" | "хватит" | "стопни" | "все" | "всё"
            ) || word.starts_with("останов")
                || word.starts_with("законч")
        })
}

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
    /// Текущий кусок начался словами «ривер напиши …» (диктовка включилась посреди фразы):
    /// команду в начале надо отрезать.
    opening: bool,
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
    /// Lazy-режим требует подтверждения найденного wake основной ASR-моделью.
    strict_spotted_wake: bool,
    /// Сколько сэмплов прошло с последнего срабатывания детектора wake-слова.
    spot_age: Option<usize>,
    /// Детектор сработал в начале фразы, а не посреди речи: такое «ривер» можно чинить
    /// по `repair_wake`, как без ленивой загрузки.
    spot_at_start: bool,
    /// Кандидат детектора ждёт подтверждения VAD, что это речь, а не шум: сэмплов с
    /// срабатывания, «в начале фразы», описание для журнала.
    spot_pending: Option<(usize, bool, Option<String>)>,
    /// Речь ли по VAD в последних кусках простоя: (сэмплов, речь); всего не больше
    /// `SPOT_VAD_WINDOW_MS`.
    vad_history: std::collections::VecDeque<(usize, bool)>,
    /// Основная модель английская (режим «только английский»): «ривер» она пишет
    /// английскими словами — см. `strip_english_wake`.
    english_primary: bool,
    /// Диагностика (`REEVER_TRACE=1`): в журнал идёт каждая фраза простоя и решение по
    /// «ривер» — видно, почему обращение не принято.
    trace: bool,
    /// Словарь англицизмов: «харднес» в кириллической диктовке → hardness.
    anglicisms: Option<Anglicisms>,
    /// Словари Windows включённых кириллических языков (`ru-RU`, `uk-UA`, `bg-BG`):
    /// слово, которое знает хоть один, — не англицизм.
    anglicism_spellcheck: Vec<&'static str>,
    /// Сэмплов с начала речи текущей фразы простоя (`None` — речи сейчас нет).
    idle_speech_age: Option<usize>,
    /// Сэмплов с последней проверки «ривер напиши …» посреди фразы простоя.
    idle_partial_elapsed: usize,
    /// Цели действий (фразы и названия, нормализованные): «открой <цель>» с такой
    /// целью — законченная команда, её не нужно дослушивать полную паузу.
    /// Цель → действие (пустая строка — у цели несколько действий).
    known_targets: std::collections::HashMap<String, String>,
    /// Эта пауза фразы уже проверена на законченную команду.
    fast_checked: bool,
    /// Распознанный текст фразы, законченной досрочно, — чтобы не считать его второй раз.
    fast_timed: Option<Timed>,
}

/// Пауза, после которой фраза проверяется на законченную команду («ривер открой
/// маркноут» → запуск через 0,3 с тишины вместо полной паузы конца фразы).
const FAST_END_MS: u32 = 300;

/// Длиннее этого фраза — не короткая команда (разговор, звонок): досрочно не проверяем.
const FAST_END_MAX_PHRASE_MS: usize = 5_000;

/// Старый eager-режим принимает детектор только в начале речи; lazy-режим принимает
/// срабатывание в любом месте и проверяет его основной ASR.
const SPOT_WINDOW_MS: u32 = 1_000;

/// Срабатывание детектора засчитывается фразе, если было не раньше этого до её конца
/// сверх длины самой фразы.
const SPOT_GRACE_MS: u32 = 1_500;
/// VAD-проверка кандидата детектора: речь хотя бы 100 мс из последних 800 мс, ждать
/// подтверждения не дольше 400 мс (`judge_pending_spot`).
const SPOT_VAD_MIN_MS: u32 = 100;
const SPOT_VAD_WINDOW_MS: u32 = 800;
const SPOT_VAD_WAIT_MS: u32 = 400;

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
            strict_spotted_wake: false,
            spot_age: None,
            spot_at_start: false,
            spot_pending: None,
            vad_history: std::collections::VecDeque::new(),
            english_primary: false,
            trace: std::env::var_os("REEVER_TRACE").is_some(),
            idle_speech_age: None,
            idle_partial_elapsed: 0,
            anglicisms: None,
            anglicism_spellcheck: Vec::new(),
            known_targets: std::collections::HashMap::new(),
            fast_checked: false,
            fast_timed: None,
        }
    }

    /// Цели действий (фразы калибровки и названия): по ним команда «открой X»
    /// считается законченной сразу, и для точной русской цели не нужна вторая модель.
    /// Пары (цель, id действия).
    pub fn set_known_targets<I: IntoIterator<Item = (String, String)>>(&mut self, targets: I) {
        self.known_targets.clear();
        for (target, action) in targets {
            let target = normalize(&target);
            if target.is_empty() {
                continue;
            }
            self.known_targets
                .entry(target)
                .and_modify(|owner| {
                    if *owner != action {
                        owner.clear();
                    }
                })
                .or_insert(action);
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

    /// Словарь англицизмов для кириллической диктовки и словари Windows её языков;
    /// `None` — текст как услышала модель.
    pub fn set_anglicisms(&mut self, anglicisms: Option<Anglicisms>, spellcheck: Vec<&'static str>) {
        self.anglicisms = anglicisms;
        self.anglicism_spellcheck = spellcheck;
    }

    /// Фоновый детектор wake-слова (маленькая потоковая модель); `None` — только GigaAM.
    pub fn set_wake_spotter(&mut self, mut spotter: Option<Box<dyn WakeSpotter + Send>>) {
        if let Some(spotter) = spotter.as_deref_mut() {
            spotter.set_wake_words(&self.cfg.wake_words);
        }
        self.spotter = spotter;
        self.spot_age = None;
    }

    pub fn set_strict_spotted_wake(&mut self, strict: bool) {
        self.strict_spotted_wake = strict;
    }

    pub fn set_english_primary(&mut self, english: bool) {
        self.english_primary = english;
    }

    /// После ленивой загрузки аудио повторно подаётся с pre-roll. Сохраняем факт,
    /// что этот буфер начался от подтверждённого триггера, даже если spotter уже сбросил endpoint.
    pub fn mark_spotted_wake(&mut self) {
        self.spot_age = Some(0);
        // Буфер начинается за секунду до срабатывания — «ривер» в его начале.
        self.spot_at_start = true;
    }

    /// Главный вход: очередной кусок 16 кГц mono из микрофона.
    /// Можно ли сейчас подогреть модели: простой и речи нет (идущую фразу не задерживаем).
    pub fn can_keep_warm(&self) -> bool {
        matches!(self.mode, Mode::Idle) && self.idle_speech_age.is_none()
    }

    /// Прогон моделей на полсекунде тихого шума. После долгой тишины Windows вытесняет
    /// память моделей (сотни МБ) на диск, и первая команда тормозила: звук копился в
    /// очереди и терялся — «разогрев» на первых командах. Результат не используется.
    pub fn keep_warm(&mut self) {
        let mut seed = 0x2545_f491u32;
        let pcm: Vec<i16> = (0..samples(500))
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                ((seed % 401) as i16) - 200
            })
            .collect();
        let _ = self.asr.transcribe(&pcm);
        if let Some(second) = self.second.as_deref_mut() {
            let _ = second.transcribe(&pcm);
        }
        if let Some(lid) = self.lid.as_deref_mut() {
            let _ = lid.detect(&pcm);
        }
    }

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
                let fired = spotter.feed(pcm);
                let note = spotter.take_note();
                if fired && (at_phrase_start || self.strict_spotted_wake) {
                    // Решение — когда VAD скажет, речь ли это (ниже, в цикле по кадрам).
                    self.spot_pending = Some((0, at_phrase_start, note));
                } else if let (false, Some(note)) = (fired, note.as_ref()) {
                    out.push(EngineEvent::Log(format!("wake: detector candidate {note}").into()));
                } else if fired && self.trace {
                    let ms = self.idle_speech_age.unwrap_or(0) * 1000 / SAMPLE_RATE as usize;
                    out.push(EngineEvent::Log(format!("trace: the detector heard the wake word {ms} ms into the phrase — not at its start, skipped").into()));
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
            self.judge_pending_spot(count, vad_saw_speech, &mut out);

            if is_muted {
                self.mute_remaining = self.mute_remaining.saturating_sub(count);
            }

            let speech_start = segment_events
                .iter()
                .any(|event| matches!(event, SegmentEvent::SpeechStart));
            for event in segment_events {
                self.handle_segment_event(event, &mut out);
            }
            if self.mode_epoch == epoch {
                self.try_fast_end(&mut out);
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

            // Детектор слышал «ривер» в начале фразы: проверяем, не «напиши» ли это, чтобы
            // печатать по ходу речи с первой фразы, а не после паузы.
            if self.mode_epoch == epoch
                && matches!(self.mode, Mode::Idle)
                && self.cfg.dictation_live
                && self.idle_speech_age.is_some()
                && self.spot_age.is_some()
            {
                self.idle_partial_elapsed = self.idle_partial_elapsed.saturating_add(count);
                if self.idle_partial_elapsed >= samples(LIVE_PARTIAL_MS) {
                    self.idle_partial_elapsed = 0;
                    self.try_open_dictation(true, &mut out);
                }
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

    /// Кандидат детектора принимается, если VAD слышал речь хотя бы `SPOT_VAD_MIN_MS` из
    /// последних `SPOT_VAD_WINDOW_MS`; ждём этого не дольше `SPOT_VAD_WAIT_MS`. Сирена,
    /// лай, дыхание — не речь: так отсекается ~80 % ложных срабатываний на шум, а живых
    /// «river» теряется 0,1 % (examples/wake_gate_probe.rs).
    fn judge_pending_spot(&mut self, count: usize, speech: bool, out: &mut Vec<EngineEvent>) {
        self.vad_history.push_back((count, speech));
        let mut total: usize = self.vad_history.iter().map(|(n, _)| n).sum();
        while total > samples(SPOT_VAD_WINDOW_MS) {
            let Some((n, _)) = self.vad_history.pop_front() else { break };
            total -= n;
        }
        let Some((age, at_start, _)) = self.spot_pending.as_mut() else {
            return;
        };
        *age += count;
        let (age, at_start) = (*age, *at_start);
        if !matches!(self.mode, Mode::Idle) {
            self.spot_pending = None;
            return;
        }
        let speech_samples: usize = self
            .vad_history
            .iter()
            .filter(|(_, speech)| *speech)
            .map(|(n, _)| n)
            .sum();
        let speech_ms = speech_samples * 1000 / SAMPLE_RATE as usize;
        let accepted = speech_samples >= samples(SPOT_VAD_MIN_MS);
        if !accepted && age < samples(SPOT_VAD_WAIT_MS) {
            return;
        }
        let note = self.spot_pending.take().and_then(|(_, _, note)| note);
        let note = note.map(|n| format!(" {n},")).unwrap_or_default();
        if accepted {
            out.push(EngineEvent::Log(
                format!("wake: detector heard the wake word —{note} speech {speech_ms} ms").into(),
            ));
            self.spot_age = Some(age);
            self.spot_at_start = at_start;
            out.push(EngineEvent::WakeSpotted);
        } else {
            out.push(EngineEvent::Log(
                format!("wake: detector candidate{note} — rejected: not speech (speech {speech_ms} ms)").into(),
            ));
        }
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

    /// Ответ пришёл кликом по плашке подтверждения — голос больше не ждём.
    pub fn stop_confirm(&mut self) {
        if matches!(&self.mode, Mode::Listening(state) if state.kind == ListenKind::Confirm) {
            self.enter_idle();
            self.pending_events.push(EngineEvent::State("idle"));
        }
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
        self.spot_pending = None;
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

    /// Интервал промежуточного текста: у живой диктовки он есть всегда (у длинного куска реже),
    /// а в ожидании команды — чтобы «напиши …» начинало печатать по ходу речи.
    fn partial_interval(&self) -> usize {
        if self.cfg.dictation_live
            && self.cfg.partial_interval_ms == 0
            && matches!(&self.mode, Mode::Listening(state) if state.kind == ListenKind::Command)
        {
            return samples(LIVE_PARTIAL_MS);
        }
        if matches!(self.mode, Mode::Dictating(_)) && self.cfg.dictation_live {
            if self.segmenter.current_audio().len() > samples(DICTATION_PARTIAL_SLOW_MS) {
                samples(LIVE_PARTIAL_SLOW_MS)
            } else {
                samples(LIVE_PARTIAL_MS)
            }
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
                    out.push(EngineEvent::Log(
                        crate::msg!("log.confirm.noAnswer", seconds = self.wait_ms(ListenKind::Confirm) / 1000).into(),
                    ));
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

        // «ривер» — пауза — «напиши …»: живая диктовка с этой же фразы.
        if self.cfg.dictation_live
            && matches!(&self.mode, Mode::Listening(state)
                if state.kind == ListenKind::Command && state.prefix.trim().is_empty())
            && self.try_open_dictation(false, out)
        {
            return;
        }

        // Живая диктовка: промежуточный текст «как сказано» — его печатают по ходу речи.
        if matches!(self.mode, Mode::Dictating(_)) && self.cfg.dictation_live {
            let result = {
                let audio = self.segmenter.current_audio();
                self.asr.transcribe_verbatim(audio)
            };
            let opening = matches!(&self.mode, Mode::Dictating(state) if state.opening);
            let result = result.map(|raw| {
                if opening {
                    match self.opening_skip(&raw) {
                        Some(skip) => self.before_wake_word(&trim_words(&raw, skip, 0)),
                        None => String::new(),
                    }
                } else {
                    self.before_wake_word(&raw)
                }
            });
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
                    out.push(EngineEvent::Log(crate::msg!("log.asrError", error = error).into()));
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
                out.push(EngineEvent::Log(crate::msg!("log.asrError", error = error).into()));
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
                    self.idle_partial_elapsed = 0;
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
                        let result = match self.fast_timed.take() {
                            Some(timed) => Ok(timed),
                            None => self.asr.transcribe_timed(&audio),
                        };
                        self.prepare_segmenter(self.cfg.wake_pause_ms);
                        if self.cfg.wake_enabled {
                            self.handle_idle_transcript(result, &audio, out);
                        }
                        audio.fill(0);
                    }
                    Mode::Listening(state) => {
                        out.push(EngineEvent::State("busy"));
                        let result = match state.kind {
                            ListenKind::Command | ListenKind::Choice => {
                                match self.fast_timed.take() {
                                    Some(timed) => Ok(timed),
                                    None => self.asr.transcribe_timed(&audio),
                                }
                                .map(|timed| self.second_language_only(&audio, timed))
                                .map(|timed| self.drop_english_wake(timed, state.kind))
                            }
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
                                            crate::msg!("log.confirm.unclearWaiting").into(),
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
                                                    crate::msg!("log.confirm.no")
                                                } else {
                                                    crate::msg!("log.confirm.unclear")
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
                                out.push(EngineEvent::Log(
                                    crate::msg!("log.asrError", error = error).into(),
                                ));
                                self.return_to_idle(true, out);
                            }
                        }
                        audio.fill(0);
                    }
                    Mode::Dictating(state) => {
                        if state.opening {
                            self.opening_segment(&audio, out);
                        } else {
                            self.dictation_segment(&audio, out);
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
                                out.push(EngineEvent::Log(
                                    crate::msg!("log.asrError", error = error).into(),
                                ));
                                self.return_to_idle(true, out);
                            }
                        }
                        audio.fill(0);
                    }
                }
            }
        }
    }

    /// Короткая пауза после законченной команды с известной целью — выполняем сразу,
    /// не дожидаясь полной паузы конца фразы. Проверка одна на паузу: если команда
    /// не узнана, фраза заканчивается как обычно.
    fn try_fast_end(&mut self, out: &mut Vec<EngineEvent>) {
        if self.known_targets.is_empty() {
            return;
        }
        let silence = self.segmenter.trailing_silence();
        if silence == 0 {
            self.fast_checked = false;
            return;
        }
        if self.fast_checked || silence < samples(FAST_END_MS) {
            return;
        }
        let applies = match &self.mode {
            Mode::Idle => self.cfg.wake_enabled,
            Mode::Listening(state) => state.kind == ListenKind::Command,
            _ => false,
        };
        let audio_len = self.segmenter.current_audio().len();
        if !applies || audio_len == 0 || audio_len > samples(FAST_END_MAX_PHRASE_MS as u32) {
            return;
        }
        self.fast_checked = true;
        let Ok(timed) = self.asr.transcribe_timed(self.segmenter.current_audio()) else {
            return;
        };
        // Журнал подтверждения попадает в out, только если команда выполняется досрочно,
        // иначе его напишет разбор полной фразы.
        let mut notes = Vec::new();
        let timed = if matches!(self.mode, Mode::Idle) && self.strict_spotted_wake {
            match self.spot_age {
                Some(age) if age <= audio_len + samples(SPOT_GRACE_MS) => {
                    let Some(confirmed) = self.confirm_wake(&timed, audio_len, &mut notes) else {
                        return;
                    };
                    confirmed
                }
                Some(_) => return,
                None => timed,
            }
        } else {
            timed
        };
        let command = match &self.mode {
            Mode::Idle => match strip_initial_wake(&timed.text, &self.cfg.wake_words) {
                Some(rest) => rest,
                None => return,
            },
            Mode::Listening(state) => {
                join_command(&state.prefix, &self.strip_repeated_wake(&timed.text))
            }
            _ => return,
        };
        if !self.is_known_command(&command) {
            return;
        }
        out.append(&mut notes);
        if let Some(audio) = self.segmenter.finish_now() {
            self.fast_timed = Some(timed);
            self.handle_segment_event(SegmentEvent::SpeechEnd(audio), out);
            self.fast_timed = None;
        }
    }

    /// «открой/закрой/заверши <известная цель>» или просто «<цель>», если запуск без
    /// ключевого слова разрешён. Поиск, чат, диктовка и прочее со свободным текстом —
    /// никогда: там конец фразы знает только пауза.
    fn is_known_command(&self, command: &str) -> bool {
        let command = strip_leading_fillers(&normalize(command));
        if command.is_empty() {
            return false;
        }
        let fallback = Grammar::default();
        let grammar = self.grammar.as_ref().unwrap_or(&fallback);
        let target = match grammar.match_trigger(&command) {
            Some(found)
                if matches!(
                    found.intent.kind,
                    IntentKind::Launch | IntentKind::Close | IntentKind::Kill
                ) =>
            {
                normalize(&found.payload)
            }
            Some(_) => return false,
            None if grammar.allows_bare_launch() => command,
            None => return false,
        };
        // «анти гравити» — начало «анти гравити идея» другого действия: короткая пауза
        // посреди названия не должна открыть другую программу, такую цель дослушиваем.
        // «маркноут» и «маркноут маркноут» у одного действия — не помеха.
        let Some(owner) = self
            .known_targets
            .get(&target)
            .filter(|owner| !owner.is_empty())
        else {
            return false;
        };
        let longer = format!("{target} ");
        !self
            .known_targets
            .iter()
            .any(|(other, other_owner)| other.starts_with(&longer) && other_owner != owner)
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
                out.push(EngineEvent::Log(crate::msg!("log.asrError", error = error).into()));
                return;
            }
        };
        if self.trace {
            let detector = match self.spot_age {
                Some(age) if age <= audio.len() + samples(SPOT_GRACE_MS) => "heard the wake word",
                Some(_) => "heard it long ago",
                None => "silent",
            };
            out.push(EngineEvent::Log(
                format!(
                    "trace: idle phrase {:.1} s «{}» · detector {detector}",
                    audio.len() as f32 / SAMPLE_RATE as f32,
                    timed.text
                )
                .into(),
            ));
        }

        // Детектор слышал «ривер» в этой фразе, а GigaAM его не расслышала («река открой…»,
        // пустота) — чиним текст: похожее слово в начале становится wake-словом.
        let spotted = self
            .spot_age
            .is_some_and(|age| age <= audio.len() + samples(SPOT_GRACE_MS));
        let timed = if spotted && self.strict_spotted_wake {
            match self.confirm_wake(&timed, audio.len(), out) {
                Some(confirmed) => confirmed,
                None => {
                    // Пустой текст — модели ещё грузятся (фраза будет проиграна заново
                    // после загрузки) или один шум: подтверждать нечего, в журнал не пишем.
                    if !timed.text.trim().is_empty() {
                        out.push(EngineEvent::Log(
                            format!(
                                "wake: the detector fired, but the main model did not confirm the wake word; phrase ignored (heard «{}»)",
                                timed.text.trim()
                            )
                            .into(),
                        ));
                        out.push(EngineEvent::WakeRejected);
                    }
                    self.spot_age = None;
                    return;
                }
            }
        } else if spotted && strip_initial_wake(&timed.text, &self.cfg.wake_words).is_none() {
            match repair_wake(&timed, &self.cfg.wake_words, audio.len(), &is_russian_word) {
                Some(repaired) => {
                    out.push(EngineEvent::Log(
                        format!(
                            "wake: the wake word was confirmed by the background detector (heard «{}»)",
                            timed.text.trim()
                        )
                        .into(),
                    ));
                    repaired
                }
                None => {
                    out.push(EngineEvent::Log(
                        "wake: the detector heard the wake word, the main model did not; phrase ignored"
                            .into(),
                    ));
                    timed
                }
            }
        } else {
            timed
        };
        let Some(rest) = strip_initial_wake(&timed.text, &self.cfg.wake_words) else {
            // В разговоре (звонок) «ривер» звучит посреди речи: фраза началась раньше, и
            // wake не в её начале. После паузы — это обращение: разбираем хвост с «ривер».
            if self.trace {
                for word in timed.words.iter().skip(1) {
                    if strip_initial_wake(&word.text, &self.cfg.wake_words)
                        .is_some_and(|r| r.trim().is_empty())
                    {
                        let at =
                            ((word.start.max(0.0) * SAMPLE_RATE as f32) as usize).min(audio.len());
                        out.push(EngineEvent::Log(
                            format!(
                                "trace: «{}» mid-phrase, pause before it {} ms (need ≥ {MID_WAKE_PAUSE_MS})",
                                word.text,
                                pause_before(audio, at)
                            )
                            .into(),
                        ));
                    }
                }
            }
            // Команда после «ривер»: глагол грамматики с целью («закрой калькулятор») или
            // точное название действия («калькулятор»).
            let grammar = self.grammar.clone();
            let targets: std::collections::HashSet<String> =
                self.known_targets.keys().cloned().collect();
            let is_command = |text: &str| {
                let normalized = normalize(text);
                (normalized.split_whitespace().count() >= 2
                    && grammar
                        .as_ref()
                        .is_some_and(|g| g.match_trigger(text).is_some()))
                    || targets.contains(&normalized)
            };
            if let Some(start) = mid_phrase_wake(&timed, audio, &self.cfg.wake_words, &is_command) {
                out.push(EngineEvent::Log(
                    "wake: the wake word after a pause mid-speech".into(),
                ));
                let tail = &audio[start..];
                let result = self.asr.transcribe_timed(tail);
                if result.as_ref().is_ok_and(|tail_timed| {
                    strip_initial_wake(&tail_timed.text, &self.cfg.wake_words).is_some()
                }) {
                    self.handle_idle_transcript(result, tail, out);
                }
            }
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
            if spotted && self.strict_spotted_wake {
                out.push(EngineEvent::WakeHeard);
            }
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
        self.first_dictation_text(audio, timed, &payload, out);
        true
    }

    /// Первый кусок после «напиши» (`payload` — цель по грамматике) и переход к диктовке.
    fn first_dictation_text(
        &mut self,
        audio: &[i16],
        timed: &Timed,
        payload: &str,
        out: &mut Vec<EngineEvent>,
    ) {
        let heard = timed.text.as_str();
        self.dictation_lang = None;
        let heard_words = heard.split_whitespace().count();
        let mut skip = heard_words.saturating_sub(payload.split_whitespace().count());
        if let Some((lang, words)) = leading_language_switch(payload) {
            self.dictation_lang = Some(lang);
            skip += words;
        }
        // «ривер напиши … ривер отмена» одним духом: длинная первая фраза тоже может
        // заканчиваться стопом или отменой.
        let control = self.trailing_control(&normalize(heard));
        if let Some((DictationEndReason::Cancelled, _)) = control {
            out.push(EngineEvent::DictationEnd(DictationEndReason::Cancelled));
            self.return_to_idle(true, out);
            return;
        }
        // Даже при пустой цели звук после «напиши» может быть: английскую фразу русская
        // модель часто не слышит вовсе («ривер напиши hello…» → «ривер напиши»).
        if skip < heard_words || !timed.words.is_empty() {
            let first = self.first_dictation_chunk(audio, timed, skip);
            let first = match control {
                Some((_, drop)) => trim_words(&first, 0, drop),
                None => first,
            };
            if !first.trim().is_empty() {
                out.push(EngineEvent::Dictation(first));
            }
        }
        if let Some((reason, _)) = control {
            out.push(EngineEvent::DictationEnd(reason));
            self.return_to_idle(true, out);
            return;
        }
        self.begin_dictation();
    }

    /// Цель «напиши …» в тексте фразы: сколько первых слов («ривер напиши», «по-английски»)
    /// отрезать. `None` — фраза не начинает диктовку.
    fn opening_skip(&self, raw: &str) -> Option<usize> {
        let norm = normalize(raw);
        let body = self.strip_repeated_wake(&norm);
        let matched = self.grammar.as_ref()?.match_trigger(&body)?;
        if matched.intent.kind != IntentKind::Dictate {
            return None;
        }
        let payload = matched.payload;
        let mut skip = norm
            .split_whitespace()
            .count()
            .checked_sub(payload.split_whitespace().count())?;
        if let Some((_, words)) = leading_language_switch(&payload) {
            skip += words;
        }
        Some(skip)
    }

    /// Посреди фразы слышно «ривер напиши …» и уже хотя бы слово цели — диктовка начинается
    /// сразу, чтобы текст печатался по ходу речи, а не одним блоком после паузы. `idle` —
    /// фраза простоя (тогда она обязана начинаться с wake-слова). Конец фразы разберёт
    /// `opening_segment` так же, как обычный старт диктовки.
    fn try_open_dictation(&mut self, idle: bool, out: &mut Vec<EngineEvent>) -> bool {
        // В lazy-режиме частичный transcript ещё не даёт надёжно подтвердить wake.
        // Диктовка стартует после полной фразы через handle_idle_transcript.
        if idle && self.strict_spotted_wake {
            return false;
        }
        let Ok(raw) = self.asr.transcribe_verbatim(self.segmenter.current_audio()) else {
            return false;
        };
        if idle && strip_initial_wake(&normalize(&raw), &self.cfg.wake_words).is_none() {
            return false;
        }
        let Some(skip) = self.opening_skip(&raw) else {
            return false;
        };
        if raw.split_whitespace().count() <= skip {
            return false;
        }
        out.push(EngineEvent::DictationStart);
        out.push(EngineEvent::State("listening"));
        self.dictation_lang = None;
        self.spot_age = None;
        self.spot_pending = None;
        self.idle_speech_age = None;
        let (pre_roll, soft_max, max) = DICTATION_LIMITS;
        self.segmenter.set_limits(pre_roll, soft_max, max);
        self.segmenter
            .set_end_silence_ms(self.cfg.dictation_pause_ms);
        self.mode = Mode::Dictating(DictationState {
            idle_remaining: samples(self.cfg.dictation_idle_ms.max(1_000)),
            speech_started: true,
            partial_elapsed: 0,
            opening: true,
        });
        let text = self.before_wake_word(&trim_words(&raw, skip, 0));
        if !text.trim().is_empty() {
            let text = if self.dictation_lang == Some(Lang::En) {
                text
            } else {
                self.english_words(text)
            };
            out.push(EngineEvent::DictationPartial(text));
        }
        true
    }

    /// Конец фразы, с которой диктовка началась посреди речи: первый кусок, как при обычном
    /// старте. Если окончательный текст оказался не «напиши …», фраза разбирается как кусок
    /// диктовки (в том числе как новая команда).
    fn opening_segment(&mut self, audio: &[i16], out: &mut Vec<EngineEvent>) {
        let timed = match self.asr.transcribe_timed(audio) {
            Ok(timed) => timed,
            Err(error) => {
                out.push(EngineEvent::Log(crate::msg!("log.asrError", error = error).into()));
                out.push(EngineEvent::DictationEnd(DictationEndReason::Error));
                self.return_to_idle(true, out);
                return;
            }
        };
        let body = self.strip_repeated_wake(&timed.text);
        let payload = self
            .grammar
            .as_ref()
            .and_then(|grammar| grammar.match_trigger(&body))
            .filter(|matched| matched.intent.kind == IntentKind::Dictate)
            .map(|matched| matched.payload);
        match payload {
            Some(payload) => {
                self.first_dictation_text(audio, &timed, &payload, out);
                if matches!(self.mode, Mode::Dictating(_)) {
                    out.push(EngineEvent::State("listening"));
                }
            }
            None => self.dictation_segment(audio, out),
        }
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
            let min_len = if skip < timed.words.len() {
                samples(200)
            } else {
                samples(600)
            };
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
            opening: false,
        });
        self.mute_remaining = 0;
        self.mode_epoch = self.mode_epoch.wrapping_add(1);
    }

    /// Законченный кусок диктовки: фраза остановки, новая команда с wake-словом или текст.
    fn dictation_segment(&mut self, audio: &[i16], out: &mut Vec<EngineEvent>) {
        let raw = match self.asr.transcribe_verbatim(audio) {
            Ok(raw) => raw,
            Err(error) => {
                out.push(EngineEvent::Log(crate::msg!("log.asrError", error = error).into()));
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
                Lang::En => crate::msg!("log.dictation.english").into(),
                Lang::Ru => crate::msg!("log.dictation.russian").into(),
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
        // «ривер стоп» / «ривер отмена» — в начале куска или в конце («…и всё ривер стоп»).
        // «Стоп» (хватит, остановись) заканчивает диктовку, напечатанное остаётся; «отмена»
        // (отбой, забудь) стирает продиктованное. Не команда «стоп»: нечёткий поиск однажды
        // запустил на неё Spotify.
        let words: Vec<&str> = norm.split_whitespace().collect();
        let (control, keep_words) = match strip_initial_wake(&norm, &self.cfg.wake_words) {
            Some(rest) if !rest.trim().is_empty() => (Some(rest), 0),
            _ if words.len() >= 2
                && strip_initial_wake(words[words.len() - 2], &self.cfg.wake_words)
                    .is_some_and(|rest| rest.trim().is_empty()) =>
            {
                (Some(words[words.len() - 1].to_string()), words.len() - 2)
            }
            _ => (None, 0),
        };
        if let Some(control) = control {
            if is_dictation_stop(&control) {
                let text = trim_words(&raw, 0, words.len() - keep_words);
                if !text.is_empty() {
                    out.push(EngineEvent::Dictation(text));
                }
                out.push(EngineEvent::DictationEnd(DictationEndReason::Stopped));
                self.return_to_idle(true, out);
                return;
            }
            if crate::textnorm::is_cancel(&control, &cancel_words) {
                out.push(EngineEvent::DictationEnd(DictationEndReason::Cancelled));
                self.return_to_idle(true, out);
                return;
            }
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
            return if en_raw.trim().is_empty() {
                ru_raw
            } else {
                en_raw
            };
        }
        let long_enough = audio.len() as f32 * 1000.0 / SAMPLE_RATE as f32 >= MIN_LID_DURATION_MS;
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

    /// Транслит английских слов в кириллическом куске — латиницей по словарю англицизмов
    /// (второй модели не нужно: на русской фразе она термин почти не слышит).
    fn english_words(&self, text: String) -> String {
        let Some(anglicisms) = &self.anglicisms else {
            return text;
        };
        // «ревер» — запись wake-слова, а не river: иначе «ревер стоп» перестаёт быть командой.
        let is_russian = |word: &str| {
            if strip_initial_wake(word, &self.cfg.wake_words)
                .is_some_and(|rest| rest.trim().is_empty())
            {
                return Some(true);
            }
            // Знает хоть один словарь — слово родное. Словаря языка может не быть в Windows
            // (украинский и болгарский ставятся вместе с языком в «Параметрах»): такой
            // пропускаем, иначе замена выключилась бы и для русского. Нет ни одного — `None`.
            let mut verdict = None;
            for &language in &self.anglicism_spellcheck {
                match crate::spellcheck::misspelled_ratio(language, word) {
                    Some((ratio, count)) if count == 1 && ratio == 0.0 => return Some(true),
                    Some(_) => verdict = Some(false),
                    None => {}
                }
            }
            verdict
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
        let known = |target: &str| self.known_targets.contains_key(&normalize(target));
        bilingual::resolve(
            audio,
            timed,
            prefix,
            &self.cfg.wake_words,
            &grammar,
            &known,
            self.second
                .as_deref_mut()
                .map(|transcriber| transcriber as &mut dyn Transcriber),
            self.lid
                .as_deref_mut()
                .map(|language_id| language_id as &mut dyn LanguageId),
            self.lang_mode,
        )
    }

    /// «…ривер стоп» / «…ривер отмена» / «…стоп ввод» в конце фразы: чем закончить диктовку
    /// и сколько последних слов (нормализованных) отрезать от текста.
    fn trailing_control(&self, norm: &str) -> Option<(DictationEndReason, usize)> {
        let stop_phrases = self
            .grammar
            .as_ref()
            .map(|grammar| grammar.dictation_stop.clone())
            .unwrap_or_else(|| Grammar::defaults().dictation_stop);
        if let Some(words) = trailing_stop_words(norm, &stop_phrases, &self.cfg.wake_words) {
            return Some((DictationEndReason::Stopped, words));
        }
        let words: Vec<&str> = norm.split_whitespace().collect();
        if words.len() < 2
            || !strip_initial_wake(words[words.len() - 2], &self.cfg.wake_words)
                .is_some_and(|rest| rest.trim().is_empty())
        {
            return None;
        }
        let control = words[words.len() - 1];
        let cancel_words = self
            .grammar
            .as_ref()
            .map(|grammar| grammar.cancel_words.clone())
            .unwrap_or_else(|| Grammar::defaults().cancel_words);
        if is_dictation_stop(control) {
            Some((DictationEndReason::Stopped, 2))
        } else if crate::textnorm::is_cancel(control, &cancel_words) {
            Some((DictationEndReason::Cancelled, 2))
        } else {
            None
        }
    }

    /// Промежуточный текст до «ривер …» посреди куска: команду («ривер стоп», «ривер
    /// открой») не печатаем по ходу речи, чтобы потом не стирать.
    fn before_wake_word(&self, raw: &str) -> String {
        let words: Vec<&str> = raw.split_whitespace().collect();
        let normalized: Vec<String> = words.iter().map(|word| normalize(word)).collect();
        match normalized.iter().position(|word| {
            !word.is_empty()
                && strip_initial_wake(word, &self.cfg.wake_words)
                    .is_some_and(|rest| rest.trim().is_empty())
        }) {
            Some(index) => words[..index].join(" "),
            None => raw.to_string(),
        }
    }

    /// Подтверждение «ривер», услышанного детектором, основной моделью (ленивый режим).
    /// Wake в любом месте фразы по `confirm_spotted_wake`; если детектор сработал в начале
    /// фразы, а модель записала слово неузнаваемо, — английские записи (`strip_english_wake`)
    /// и осторожная починка `repair_wake`, как без ленивой загрузки. Посреди речи —
    /// только подтверждение, иначе обычный разговор запускал бы команды.
    fn confirm_wake(
        &self,
        timed: &Timed,
        audio_len: usize,
        out: &mut Vec<EngineEvent>,
    ) -> Option<Timed> {
        if let Some((index, confirmed)) = confirm_spotted_wake(timed, &self.cfg.wake_words) {
            if index > 0 {
                out.push(EngineEvent::Log(
                    "wake: the main model confirmed the wake word mid-phrase".into(),
                ));
            }
            return Some(confirmed);
        }
        if !self.spot_at_start || timed.text.trim().is_empty() {
            return None;
        }
        if self.english_primary {
            if let Some(rest) = strip_english_wake(&timed.text) {
                out.push(EngineEvent::Log(
                    "wake: the English model wrote the wake word as an English word; confirmed by the background detector".into(),
                ));
                let canonical = normalize(self.cfg.wake_words.first()?);
                return Some(Timed {
                    text: format!("{canonical} {rest}").trim().to_string(),
                    words: Vec::new(),
                });
            }
        }
        let fixed = repair_wake(timed, &self.cfg.wake_words, audio_len, &is_russian_word)?;
        out.push(EngineEvent::Log(
            format!(
                "wake: the wake word was confirmed by the background detector (heard «{}»)",
                timed.text.trim()
            )
            .into(),
        ));
        Some(fixed)
    }

    /// Ответ на сигнал «слушаю» в режиме «только английский»: «ривер», повторённое перед
    /// командой, английская модель пишет как «right …» — убираем. Только для команды:
    /// в диктовке «Every day…» — это текст.
    fn drop_english_wake(&self, timed: Timed, kind: ListenKind) -> Timed {
        if !self.english_primary || kind != ListenKind::Command {
            return timed;
        }
        match strip_english_wake(&timed.text) {
            Some(rest) => Timed {
                text: rest,
                words: Vec::new(),
            },
            None => timed,
        }
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
        self.spot_pending = None;
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
/// - И в обоих случаях искажённое должно звучать похоже на wake (`sounds_like_wake`):
///   Parakeet пишет «ривер» как «Diver», «Ariba», «Рвер», а чужая речь после ложного
///   срабатывания детектора («вот такое») командой не становится.
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
        // Детектор акустический и иногда ошибается: настоящее слово («да», «окей») — не wake.
        // Пустой текст — модель не расслышала ничего: верим детектору (в ленивом режиме
        // пустое отсекает `confirm_wake` раньше).
        if words.first().is_some_and(|word| is_real_word(word) == Some(true))
            || (!words.is_empty() && !sounds_like_wake(&words, wake_words))
        {
            return None;
        }
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
    if !all_garbled || !sounds_like_wake(&words[..dropped], wake_words) {
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

/// Похоже ли на звук wake-слова: первое слово или первые два вместе («Re well») по
/// звуковому ключу (dictation.rs). Порог по записям пользователя: «Diver» 0,87, «Ariba»
/// 0,78, «Report» 0,76 — да; «вот» 0, «голубые» 0, «real» 0,67 — нет.
fn sounds_like_wake(words: &[&str], wake_words: &[String]) -> bool {
    const MIN_SIMILARITY: f64 = 0.75;
    let key = |word: &str| {
        let word = word.to_lowercase();
        if word.chars().any(|c| c.is_ascii_alphabetic()) {
            crate::dictation::english_sound_key(&word)
        } else {
            crate::dictation::russian_sound_key(&word)
        }
    };
    let mut targets: Vec<String> = wake_words.iter().map(|wake| key(wake)).collect();
    targets.extend(["river", "rever", "riva"].map(String::from));
    let one = words.first().map(|word| key(word)).unwrap_or_default();
    let two: String = words.iter().take(2).map(|word| key(word)).collect();
    [one, two].iter().filter(|candidate| !candidate.is_empty()).any(|candidate| {
        targets
            .iter()
            .any(|target| strsim::jaro_winkler(candidate, target) >= MIN_SIMILARITY)
    })
}

/// Lazy-режим не полагается на fuzzy `strip_wake` для ложных KWS-совпадений вроде
/// «ревёт» и «реверс». Принимает точное wake-слово, склейку wake+хвост или односимвольную
/// ошибку ASR, только если она не является словарным русским словом.
fn confirm_spotted_wake(timed: &Timed, wake_words: &[String]) -> Option<(usize, Timed)> {
    // В начале фразы — те же правила, что без ленивой загрузки: шум перед речью даёт
    // «тривер», граница слова съезжает — «риверс потифай». Без этого первое «ривер»
    // после долгой тишины отвергалось, хотя детектор его слышал. Строгий поиск ниже —
    // для wake посреди фразы.
    if strip_initial_wake(&timed.text, wake_words).is_some() {
        return Some((0, timed.clone()));
    }
    let words: Vec<&str> = timed.text.split_whitespace().collect();
    let canonical = normalize(wake_words.first()?);
    if canonical.is_empty() {
        return None;
    }
    let index = words.iter().position(|word| {
        let word = normalize(word);
        if wake_words.iter().any(|wake| normalize(wake) == word) {
            return true;
        }
        if let Some(tail) = word.strip_prefix(&canonical) {
            if tail.chars().count() >= 2 {
                return true;
            }
        }
        let word_len = word.chars().count();
        let wake_len = canonical.chars().count();
        word_len <= wake_len
            && wake_len.saturating_sub(word_len) <= 1
            && strsim::levenshtein(&word, &canonical) == 1
            && is_russian_word(&word) == Some(false)
    })?;

    let recognized = normalize(words[index]);
    let exact = wake_words.iter().any(|wake| normalize(wake) == recognized)
        || recognized.starts_with(&canonical)
            && recognized
                .chars()
                .count()
                .saturating_sub(canonical.chars().count())
                >= 2;
    let mut confirmed_words = Vec::with_capacity(words.len() - index);
    confirmed_words.push(if exact { recognized } else { canonical.clone() });
    confirmed_words.extend(words.iter().skip(index + 1).map(|word| normalize(word)));

    let mut confirmed = timed.clone();
    confirmed.text = confirmed_words.join(" ");
    if timed.words.len() == words.len() {
        confirmed.words = timed.words.iter().skip(index).cloned().collect();
        if !exact {
            if let Some(first) = confirmed.words.first_mut() {
                first.text = canonical;
            }
        }
    } else {
        confirmed.words.clear();
    }
    Some((index, confirmed))
}

/// Словарное ли русское слово (проверка орфографии Windows); `None` — словаря нет.
fn is_russian_word(word: &str) -> Option<bool> {
    crate::spellcheck::misspelled_ratio("ru-RU", word).map(|(ratio, _)| ratio < 0.5)
}

/// Обращения, которые люди ставят перед именем: «эй ривер», «слушай ривер открой …».
const WAKE_LEAD_INS: [&str; 8] = ["эй", "ну", "а", "слушай", "окей", "ок", "так", "хей"];

/// Как английская модель (Parakeet, режим «только английский») пишет русское «ривер»:
/// по журналу — «right», по корпусу — «every», «ever», «ere», «a rever», «a really»,
/// «a liver», «eriver», «rivier», «revers», склеенное с командой «revacroidge». Это обычные
/// английские слова, поэтому вызывающий засчитывает их только вместе с детектором в начале
/// фразы или в ответе на сигнал. `Some(хвост)` — первое слово (после «a/i/uh …») — wake.
fn strip_english_wake(text: &str) -> Option<String> {
    const LEADS: [&str; 7] = ["a", "i", "uh", "um", "oh", "ah", "the"];
    const SPELLINGS: [&str; 16] = [
        "right", "really", "ever", "every", "ere", "river", "rivers", "liver", "reever", "rever",
        "revere", "revers", "reverse", "rivier", "riva", "eriver",
    ];
    let normalized = normalize(text);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let skip = usize::from(words.len() > 1 && LEADS.contains(&words[0]));
    let word = *words.get(skip)?;
    let spelled = SPELLINGS.contains(&word) || word.starts_with("riv") || word.starts_with("rev");
    spelled.then(|| words[skip + 1..].join(" "))
}

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
            |event| matches!(event, EngineEvent::Log(message) if message.render("en").contains("mock failure"))
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
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Confirm(_))));
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
    fn spotted_noise_without_speech_is_rejected() {
        // Детектор сработал на шум, VAD речи не слышит: ни сигнала, ни загрузки моделей.
        let (mut engine, _, _, _) = engine(test_config(), &["ривер открой загрузки"]);
        engine.set_strict_spotted_wake(true);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([true])))));
        let events = engine.push_audio(&vec![0; samples(600)]);
        assert!(!contains(&events, &EngineEvent::WakeSpotted), "{events:?}");
        assert!(
            events.iter().any(|event| matches!(event, EngineEvent::Log(message) if message.render("en").contains("not speech"))),
            "{events:?}"
        );
    }

    #[test]
    fn spotted_wake_repairs_a_misheard_wake_word() {
        // GigaAM услышала «рибер открой загрузки», детектор — «ривер».
        let (mut engine, _, _, _) = engine(test_config(), &["рибер открой загрузки"]);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([true])))));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(&events, &EngineEvent::WakeSpotted), "{events:?}");
        assert!(
            contains(
                &events,
                &EngineEvent::Command {
                    text: "открой загрузки".into(),
                    alt: None
                }
            ),
            "{events:?}"
        );
    }

    #[test]
    fn without_a_spot_misheard_wake_is_ignored() {
        let (mut engine, _, _, _) = engine(test_config(), &["река открой загрузки"]);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::new()))));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
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
        assert_eq!(
            repair_wake(&timed("мотор ревет за окном"), &wake, long, &dict),
            None
        );
        assert_eq!(
            repair_wake(&timed("река открой хром"), &wake, long, &dict),
            None
        );
        // После ложного срабатывания детектора: не похоже на «ривер» — не команда.
        assert_eq!(
            repair_wake(&timed("вот такое"), &wake, long, &dict),
            None
        );
        assert_eq!(
            repair_wake(&timed("diver открой хром"), &wake, long, &dict).map(|t| t.text),
            Some("ривер открой хром".into())
        );
        assert_eq!(
            repair_wake(&timed("ре"), &wake, samples(800), &dict).map(|t| t.text),
            Some("ривер".into())
        );
        let no_dictionary = |_: &str| None;
        assert_eq!(
            repair_wake(&timed("рибер открой хром"), &wake, long, &no_dictionary),
            None
        );
    }

    #[test]
    fn strict_spotter_confirmation_finds_wake_mid_phrase_and_rejects_similar_words() {
        let wake = vec!["ривер".to_string(), "ревер".to_string()];
        let timed = Timed {
            text: "разговор ривер открой хром".into(),
            words: [
                ("разговор", 0.0, 0.4),
                ("ривер", 0.5, 0.8),
                ("открой", 0.9, 1.2),
                ("хром", 1.3, 1.5),
            ]
            .into_iter()
            .map(|(text, start, end)| TimedWord {
                text: text.into(),
                start,
                end,
            })
            .collect(),
        };
        let (index, confirmed) = confirm_spotted_wake(&timed, &wake).unwrap();
        assert_eq!(index, 1);
        assert_eq!(confirmed.text, "ривер открой хром");
        assert_eq!(confirmed.words[0].text, "ривер");

        for false_wake in ["мотор ревет за окном", "включи реверс на гитаре"]
        {
            assert!(
                confirm_spotted_wake(
                    &Timed {
                        text: false_wake.into(),
                        words: Vec::new()
                    },
                    &wake
                )
                .is_none(),
                "ложный wake принят: {false_wake}"
            );
        }
    }

    #[test]
    fn lazy_spotter_confirms_a_wake_mid_phrase_before_dispatch() {
        let mut cfg = test_config();
        cfg.wake_words = vec!["ривер".into()];
        let (mut engine, _, _, _) = engine(cfg, &["разговор ривер открой загрузки"]);
        engine.set_strict_spotted_wake(true);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([
            false, true,
        ])))));

        engine.push_audio(&vec![123; samples(600)]);
        let mut pcm = vec![123; samples(240)];
        pcm.resize(samples(240 + 250), 0);
        let events = engine.push_audio(&pcm);

        assert!(contains(&events, &EngineEvent::WakeSpotted), "{events:?}");
        assert!(
            contains(
                &events,
                &EngineEvent::Command {
                    text: "открой загрузки".into(),
                    alt: None,
                }
            ),
            "{events:?}"
        );
    }

    #[test]
    fn lazy_spotter_confirms_a_wake_garbled_at_phrase_start() {
        // Первое «ривер» после тишины: шум перед речью — «тривер», съехавшая граница —
        // «риверс потифай». Без ленивой загрузки это команды, с ней — тоже.
        for (heard, command) in [
            ("тривер открой загрузки", "открой загрузки"),
            ("риверс потифай", "спотифай"),
        ] {
            let mut cfg = test_config();
            cfg.wake_words = vec!["ривер".into()];
            let (mut engine, _, _, _) = engine(cfg, &[heard]);
            engine.set_strict_spotted_wake(true);
            engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([
                false, true,
            ])))));

            engine.push_audio(&vec![123; samples(600)]);
            let mut pcm = vec![123; samples(240)];
            pcm.resize(samples(240 + 250), 0);
            let events = engine.push_audio(&pcm);

            assert!(
                contains(
                    &events,
                    &EngineEvent::Command {
                        text: command.into(),
                        alt: None,
                    }
                ),
                "«{heard}»: {events:?}"
            );
        }
    }

    #[test]
    fn lazy_spotter_repairs_a_lone_garbled_wake_at_phrase_start() {
        // «Ривер» одно, после долгой тишины, GigaAM написала обрывком — детектор слышал его
        // в начале фразы: ждём команду, как без ленивой загрузки.
        let mut cfg = test_config();
        cfg.wake_words = vec!["ривер".into()];
        let (mut engine, _, _, _) = engine(cfg, &["рибе"]);
        engine.set_strict_spotted_wake(true);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([
            false, true,
        ])))));

        engine.push_audio(&vec![123; samples(300)]);
        let mut pcm = vec![123; samples(240)];
        pcm.resize(samples(240 + 250), 0);
        let events = engine.push_audio(&pcm);

        assert!(contains(&events, &EngineEvent::WakeHeard), "{events:?}");
    }

    #[test]
    fn english_model_spellings_of_the_wake_word() {
        for (heard, rest) in [
            ("Right, open notepad.", "open notepad"),
            ("a rever is a crow", "is a crow"),
            ("I really will push", "will push"),
            ("every zachroi telegram", "zachroi telegram"),
            ("right", ""),
        ] {
            assert_eq!(strip_english_wake(heard).as_deref(), Some(rest), "«{heard}»");
        }
        for heard in ["open notepad", "all of it right now", "a cat"] {
            assert_eq!(strip_english_wake(heard), None, "«{heard}»");
        }
    }

    #[test]
    fn english_only_mode_takes_right_as_the_wake_word_only_with_the_detector() {
        let lazy_engine = |heard: &str, detector: VecDeque<bool>| {
            let mut cfg = test_config();
            cfg.wake_words = vec!["ривер".into()];
            let (mut engine, _, _, _) = engine(cfg, &[heard]);
            engine.set_strict_spotted_wake(true);
            engine.set_english_primary(true);
            engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(detector))));
            engine.push_audio(&vec![123; samples(600)]);
            let mut pcm = vec![123; samples(240)];
            pcm.resize(samples(240 + 250), 0);
            engine.push_audio(&pcm)
        };

        let events = lazy_engine("right open notepad", VecDeque::from([false, true]));
        assert!(
            contains(
                &events,
                &EngineEvent::Command {
                    text: "open notepad".into(),
                    alt: None,
                }
            ),
            "{events:?}"
        );

        // Без срабатывания детектора «right» — обычное слово.
        let events = lazy_engine("right open notepad", VecDeque::from([false, false]));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EngineEvent::Command { .. })),
            "{events:?}"
        );
    }

    #[test]
    fn lazy_spotter_false_alarm_does_not_dispatch_a_similar_word() {
        let mut cfg = test_config();
        cfg.wake_words = vec!["ривер".into()];
        let (mut engine, _, _, _) = engine(cfg, &["включи реверс на гитаре"]);
        engine.set_strict_spotted_wake(true);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([
            false, true,
        ])))));

        engine.push_audio(&vec![123; samples(600)]);
        let mut pcm = vec![123; samples(240)];
        pcm.resize(samples(240 + 250), 0);
        let events = engine.push_audio(&pcm);

        assert!(contains(&events, &EngineEvent::WakeSpotted), "{events:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EngineEvent::Command { .. })),
            "ложный wake выполнил команду: {events:?}"
        );
    }

    #[test]
    fn lazy_idle_does_not_open_dictation_from_a_partial_wake() {
        let (mut engine, asr_calls, _, _) = engine(test_config(), &["ривер напиши привет"]);
        engine.set_strict_spotted_wake(true);
        engine.mark_spotted_wake();
        let mut events = Vec::new();

        assert!(!engine.try_open_dictation(true, &mut events));
        assert!(events.is_empty());
        assert_eq!(asr_calls.get(), 0);
    }

    #[test]
    fn lazy_spotter_accepts_a_hit_after_the_start_of_a_phrase() {
        let (mut engine, _, _, _) = engine(test_config(), &[""]);
        engine.set_strict_spotted_wake(true);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([
            false, false, false, false, true,
        ])))));
        for _ in 0..4 {
            engine.push_audio(&vec![123; samples(300)]);
        }
        let events = engine.push_audio(&vec![123; samples(300)]);
        assert!(contains(&events, &EngineEvent::WakeSpotted), "{events:?}");
    }

    #[test]
    fn spotted_bare_wake_with_empty_transcript_starts_listening() {
        let (mut engine, _, _, _) = engine(test_config(), &[""]);
        engine.set_wake_spotter(Some(Box::new(ScriptedSpotter(VecDeque::from([true])))));
        let events = engine.push_audio(&phrase(123, 120));
        assert!(contains(&events, &EngineEvent::WakeHeard));
    }

    #[test]
    fn river_stop_ends_dictation_and_river_cancel_erases_it() {
        for (word, reason) in [
            ("стоп", DictationEndReason::Stopped),
            ("stop", DictationEndReason::Stopped),
            ("хватит", DictationEndReason::Stopped),
            ("отмена", DictationEndReason::Cancelled),
            ("забудь", DictationEndReason::Cancelled),
        ] {
            let heard = format!("ривер {word}");
            let mut engine = dictation_engine(&["ривер напиши", &heard]);
            engine.push_audio(&phrase(123, 700));
            let events = engine.push_audio(&phrase(123, 200));
            assert!(
                contains(&events, &EngineEvent::DictationEnd(reason)),
                "{word}: {events:?}"
            );
            assert!(!events
                .iter()
                .any(|event| matches!(event, EngineEvent::Command { .. })));
        }
    }

    #[test]
    fn river_stop_at_the_end_keeps_the_text_before_it() {
        let mut engine = dictation_engine(&["ривер напиши", "вот и всё ривер стоп"]);
        engine.push_audio(&phrase(123, 700));
        let events = engine.push_audio(&phrase(123, 200));
        assert_eq!(dictated(&events), vec!["вот и всё".to_string()]);
        assert!(contains(
            &events,
            &EngineEvent::DictationEnd(DictationEndReason::Stopped)
        ));
    }

    #[test]
    fn dictation_opens_mid_phrase_and_types_live_from_the_first_words() {
        let mut cfg = test_config();
        cfg.dictation_pause_ms = 120;
        cfg.dictation_live = true;
        let texts = ["напиши привет мир"; 12];
        let (mut engine, _, _, _) = engine(cfg, &texts);
        engine.set_grammar(crate::grammar::Grammar::defaults());
        engine.listen_command();
        let mut pcm = vec![123; samples(1_200)];
        pcm.resize(samples(1_200 + 300), 0);
        let events = engine.push_audio(&pcm);
        assert!(
            contains(&events, &EngineEvent::DictationStart),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EngineEvent::DictationPartial(t) if t == "привет мир")),
            "{events:?}"
        );
        assert_eq!(dictated(&events), vec!["привет мир".to_string()]);
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Command { .. })));
    }

    #[test]
    fn one_breath_dictation_honours_river_cancel_and_stop_at_the_end() {
        let mut engine = dictation_engine(&["ривер напиши итак я хочу ривер отмена"]);
        let events = engine.push_audio(&phrase(123, 700));
        assert!(
            contains(
                &events,
                &EngineEvent::DictationEnd(DictationEndReason::Cancelled)
            ),
            "{events:?}"
        );
        assert!(dictated(&events).is_empty(), "{events:?}");

        let mut engine = dictation_engine(&["ривер напиши итак я хочу ривер стоп"; 2]);
        let events = engine.push_audio(&phrase(123, 700));
        assert!(
            contains(
                &events,
                &EngineEvent::DictationEnd(DictationEndReason::Stopped)
            ),
            "{events:?}"
        );
        assert_eq!(dictated(&events), vec!["итак я хочу".to_string()]);
    }

    #[test]
    fn pause_before_measures_silence_ending_at_the_word() {
        let mut audio = vec![3000i16; samples(1_000)];
        audio.extend(vec![0i16; samples(400)]);
        audio.extend(vec![3000i16; samples(600)]);
        let at = samples(1_400);
        let pause = pause_before(&audio, at);
        assert!((380..=420).contains(&pause), "{pause}");
        assert_eq!(
            pause_before(&vec![3000i16; samples(1_000)], samples(500)),
            0
        );
    }

    #[test]
    fn wake_after_a_pause_mid_phrase_starts_a_command() {
        let word = |text: &str, start: f32| crate::speech::TimedWord {
            text: text.into(),
            start,
            end: start + 0.3,
        };
        let timed = Timed {
            text: "ну короче ривер открой".into(),
            words: vec![
                word("ну", 0.0),
                word("короче", 0.3),
                word("ривер", 1.4),
                word("открой", 1.8),
            ],
        };
        let wake = vec!["ривер".to_string()];
        let mut audio = vec![3000i16; samples(1_000)];
        audio.extend(vec![0i16; samples(400)]);
        audio.extend(vec![3000i16; samples(800)]);
        let never = |_: &str| false;
        assert_eq!(
            mid_phrase_wake(&timed, &audio, &wake, &never),
            Some(samples(1_400) - samples(80))
        );
        // Без паузы — слово в разговоре.
        let solid = vec![3000i16; samples(2_200)];
        assert_eq!(mid_phrase_wake(&timed, &solid, &wake, &never), None);
    }

    #[test]
    fn wake_without_a_pause_counts_when_a_command_follows() {
        let word = |text: &str, start: f32| crate::speech::TimedWord {
            text: text.into(),
            start,
            end: start + 0.3,
        };
        let wake = vec!["ривер".to_string()];
        let grammar = Grammar::default();
        let is_command = |text: &str| {
            text.split_whitespace().count() >= 2 && grammar.match_trigger(text).is_some()
                || text == "калькулятор"
        };
        let solid = vec![3000i16; samples(3_000)];
        // «нет вот он ривер закрой калькулятор» на одном дыхании — обращение.
        let timed = Timed {
            text: "нет вот он ривер закрой калькулятор".into(),
            words: vec![
                word("нет", 0.0),
                word("вот", 0.3),
                word("он", 0.6),
                word("ривер", 0.9),
                word("закрой", 1.3),
                word("калькулятор", 1.7),
            ],
        };
        assert_eq!(
            mid_phrase_wake(&timed, &solid, &wake, &is_command),
            Some(samples(900) - samples(80))
        );
        // «он не сработал на ривер» — команды после нет, это разговор.
        let talk = Timed {
            text: "он не сработал на ривер".into(),
            words: vec![
                word("он", 0.0),
                word("не", 0.2),
                word("сработал", 0.4),
                word("на", 0.9),
                word("ривер", 1.1),
            ],
        };
        assert_eq!(mid_phrase_wake(&talk, &solid, &wake, &is_command), None);
        // «…ривер это крутая штука» — не команда.
        let chat = Timed {
            text: "короче ривер это крутая штука".into(),
            words: vec![
                word("короче", 0.0),
                word("ривер", 0.4),
                word("это", 0.8),
                word("крутая", 1.0),
                word("штука", 1.4),
            ],
        };
        assert_eq!(mid_phrase_wake(&chat, &solid, &wake, &is_command), None);
        // «…с ривером ривер калькулятор» — за «ривер» название действия.
        let named = Timed {
            text: "че там с ривером ривер калькулятор".into(),
            words: vec![
                word("че", 0.0),
                word("там", 0.2),
                word("с", 0.4),
                word("ривером", 0.5),
                word("ривер", 1.0),
                word("калькулятор", 1.4),
            ],
        };
        assert_eq!(
            mid_phrase_wake(&named, &solid, &wake, &is_command),
            Some(samples(1_000) - samples(80))
        );
    }

    fn fast_engine(texts: &[&str]) -> Engine<MockTranscriber, MockVoiceDetector> {
        let (mut engine, _, _, _) = engine(
            EngineConfig {
                wake_pause_ms: 900,
                command_pause_ms: 900,
                ..test_config()
            },
            texts,
        );
        engine.set_grammar(Grammar::defaults());
        engine.set_known_targets(
            [
                ("маркноут", "mark"),
                ("маркноут маркноут", "mark"),
                ("анти гравити", "ag"),
                ("анти гравити идея", "ide"),
            ]
            .map(|(target, action)| (target.to_string(), action.to_string())),
        );
        engine
    }

    /// Мс тишины после речи, когда пришла команда (`None` — не пришла).
    fn command_after(
        engine: &mut Engine<MockTranscriber, MockVoiceDetector>,
    ) -> Option<(u32, String)> {
        let mut pcm = vec![123i16; samples(240)];
        pcm.resize(samples(240 + 1_500), 0);
        for (index, chunk) in pcm.chunks(160).enumerate() {
            for event in engine.push_audio(chunk) {
                if let EngineEvent::Command { text, .. } = event {
                    let elapsed = ((index + 1) * 10) as u32;
                    return Some((elapsed.saturating_sub(240), text));
                }
            }
        }
        None
    }

    #[test]
    fn known_target_ends_the_command_after_a_short_pause() {
        let mut engine = fast_engine(&["ривер открой маркноут"]);
        let (silence, text) = command_after(&mut engine).expect("команда");
        assert_eq!(text, "открой маркноут");
        assert!(silence < 400, "команда через {silence} мс тишины");
    }

    #[test]
    fn unknown_or_ambiguous_targets_wait_for_the_full_pause() {
        // Свободный текст (поиск) и цель, с которой начинается другая, — полная пауза.
        for said in [
            "ривер загугли маркноут",
            "ривер открой анти гравити",
            "ривер открой фотошоп",
        ] {
            let mut engine = fast_engine(&[said, said]);
            let (silence, _) = command_after(&mut engine).expect("команда");
            assert!(silence >= 900, "{said}: команда через {silence} мс тишины");
        }
        let mut engine = fast_engine(&["ривер открой анти гравити идея"]);
        assert!(command_after(&mut engine).expect("команда").0 < 400);
    }

    #[test]
    fn live_partial_stops_before_a_wake_word() {
        let mut engine = dictation_engine(&[]);
        engine.cfg.wake_words = vec!["ривер".into()];
        assert_eq!(engine.before_wake_word("и всё Ривер стоп"), "и всё");
        assert_eq!(engine.before_wake_word("обычный текст"), "обычный текст");
        assert!(is_dictation_stop("стоп"));
        assert!(is_dictation_stop("остановись"));
        assert!(!is_dictation_stop("отмена"));
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
        assert!(
            contains(
                &events,
                &EngineEvent::DictationEnd(DictationEndReason::Cancelled)
            ),
            "{events:?}"
        );
        assert!(dictated(&events).is_empty());
    }

    #[test]
    fn live_dictation_emits_verbatim_partials() {
        let mut cfg = test_config();
        cfg.dictation_pause_ms = 120;
        cfg.dictation_live = true;
        let (mut engine, _, _, _) =
            engine(cfg, &["ривер напиши", "привет", "привет мир", "привет мир"]);
        engine.set_grammar(crate::grammar::Grammar::defaults());
        engine.push_audio(&phrase(123, 700));
        // длинная фраза: 900 мс речи — успевают промежуточные распознавания
        let mut pcm = vec![123; samples(900)];
        pcm.resize(samples(900 + 300), 0);
        let events = engine.push_audio(&pcm);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EngineEvent::DictationPartial(t) if t.starts_with("привет"))),
            "{events:?}"
        );
    }
}
