//! Печать по ходу речи на корпусе диктовки: когда слово появилось в пузыре над маскотом
//! (промежуточный текст) и когда оно попало в поле. Engine идёт в реальном времени: часы —
//! время звука, а то, что воркер считает или печатает дольше звука, сдвигает их вперёд.
//!
//!   cargo run --release --example dictation_live_probe [-- --char-ms 10] [--blocking] [--limit 6]
//!
//! `--char-ms` — сколько длится печать одного символа (`typing::CHAR_PAUSE`). `--blocking` —
//! воркер ждёт печати (так было до `typing::LiveTypist`), иначе печать идёт своим потоком.
//! Модели — как у пользователя по умолчанию: GigaAM, Parakeet v2, whisper-base, англицизмы.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use reever_lib::{
    anglicism::Anglicisms,
    asr::{Asr, AsrConfig},
    bilingual::LangMode,
    dictation::Formatter,
    engine::{Engine, EngineConfig, EngineEvent},
    grammar::Grammar,
    langid::SpeechLid,
    speech::{LanguageId, Transcriber},
    typing,
    vad::SileroVad,
};
use serde::Deserialize;

const CHUNK_SAMPLES: usize = 320; // 20 мс
const ASR_MODEL: &str = "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16";
const SECOND_MODEL: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";
const LID_MODEL: &str = "sherpa-onnx-whisper-base";

#[derive(Deserialize)]
struct Scenario {
    file: String,
    scenario: String,
}

/// Поле и пузырь одной диктовки — как в `recognizer.rs`, только без окна.
#[derive(Default)]
struct Live {
    formatter: Formatter,
    typed: String,
    last_preview: String,
    /// Слова готовых кусков: в поле и в пузыре.
    done_words: usize,
    /// Моменты (мс), когда в пузыре / в поле стало `i + 1` слов.
    bubble_at: Vec<f64>,
    field_at: Vec<f64>,
    /// Моменты обновлений поля и сколько символов за раз.
    updates: Vec<(f64, usize)>,
    rewrites: usize,
    /// Сколько раз по ходу речи стиралось напечатанное (каждое — заметное «исправление»).
    corrections: usize,
    /// Когда поток ввода освободится (асинхронная печать).
    typist_free: f64,
}

impl Live {
    fn mark(times: &mut Vec<f64>, words: usize, at: f64) {
        while times.len() < words {
            times.push(at);
        }
    }

    fn words(text: &str) -> usize {
        text.split_whitespace().count()
    }

    /// Привести поле к `goal`, начиная в `at`; вернуть, сколько символов ушло в печать
    /// (стирание + ввод). Слово попадает в поле, когда допечатан его последний символ.
    fn reconcile(&mut self, goal: &str, at: f64, char_ms: f64, blocking: bool) -> usize {
        let prefix = self
            .typed
            .chars()
            .zip(goal.chars())
            .take_while(|(a, b)| a == b)
            .count();
        let erased = self.typed.chars().count() - prefix;
        let added = goal.chars().count() - prefix;
        if erased + added == 0 {
            return 0;
        }
        if erased > 0 {
            self.rewrites += erased;
            self.corrections += 1;
        }
        let start = if blocking {
            at
        } else {
            self.typist_free.max(at)
        };
        let mut chars = erased;
        let kept: String = goal.chars().take(prefix).collect();
        let mut text = kept.clone();
        for character in goal.chars().skip(prefix) {
            text.push(character);
            chars += 1;
            let words = Live::words(&text);
            let word_done =
                !character.is_whitespace() && text.chars().count() == goal.chars().count();
            let before_space = character.is_whitespace() && words > 0;
            if word_done || before_space {
                Live::mark(
                    &mut self.field_at,
                    self.done_words + words,
                    start + chars as f64 * char_ms,
                );
            }
        }
        let end = start + chars as f64 * char_ms;
        self.typist_free = end;
        self.typed = goal.to_string();
        self.updates.push((end, erased + added));
        Live::mark(
            &mut self.field_at,
            self.done_words + Live::words(&self.typed),
            end,
        );
        erased + added
    }

    fn partial(
        &mut self,
        raw: &str,
        settled: bool,
        at: f64,
        char_ms: f64,
        blocking: bool,
    ) -> usize {
        let preview = typing::pieces_to_text(&self.formatter.clone().format(raw));
        Live::mark(
            &mut self.bubble_at,
            self.done_words + Live::words(&preview),
            at,
        );
        let previous = std::mem::replace(&mut self.last_preview, preview.clone());
        match typing::partial_goal(&self.typed, &previous, &preview, settled) {
            Some(goal) => self.reconcile(&goal, at, char_ms, blocking),
            None => 0,
        }
    }

    fn chunk(&mut self, raw: &str, at: f64, char_ms: f64, blocking: bool) {
        let wanted = typing::pieces_to_text(&self.formatter.format(raw));
        Live::mark(
            &mut self.bubble_at,
            self.done_words + Live::words(&wanted),
            at,
        );
        let goal = typing::settle_goal(&self.typed, &wanted);
        self.reconcile(&goal, at, char_ms, blocking);
        self.done_words += Live::words(&goal);
        self.typed.clear();
        self.last_preview.clear();
    }
}

#[derive(Default)]
struct Totals {
    lags: Vec<f64>,
    gaps: Vec<f64>,
    batch_chars: Vec<usize>,
    partial_ms: Vec<f64>,
    chunk_ms: Vec<f64>,
    rewrites: usize,
    corrections: usize,
    words: usize,
    worker_behind_ms: f64,
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let char_ms: f64 = arg("--char-ms").map_or(Ok(10.0), |v| v.parse())?;
    let blocking = args.iter().any(|a| a == "--blocking");
    let limit: usize = arg("--limit").map_or(Ok(usize::MAX), |v| v.parse())?;
    let partial_ms: Option<u32> = arg("--partial-ms").map(|v| v.parse()).transpose()?;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let testdata = root.join("../testdata");
    let manifest: Vec<Scenario> = serde_json::from_str(
        &std::fs::read_to_string(testdata.join("dictation-manifest.json"))
            .context("testdata/dictation-manifest.json")?,
    )?;

    let mut engine = Engine::new(
        EngineConfig {
            wake_words: ["ривер", "ревер", "ривьер", "ливер"]
                .iter()
                .map(|word| word.to_string())
                .collect(),
            partial_interval_ms: partial_ms.unwrap_or(400),
            dictation_live: true,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig {
            model_dir: models.join(ASR_MODEL),
            num_threads: 4,
        })?,
        SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
    );
    let second: Option<Box<dyn Transcriber + Send>> = Asr::new(&AsrConfig {
        model_dir: models.join(SECOND_MODEL),
        num_threads: 4,
    })
    .ok()
    .map(|asr| Box::new(asr) as _);
    let lid: Option<Box<dyn LanguageId + Send>> = SpeechLid::new(&models.join(LID_MODEL), 4)
        .ok()
        .map(|lid| Box::new(lid) as _);
    eprintln!(
        "вторая модель: {}, LID: {}",
        second.is_some(),
        lid.is_some()
    );
    engine.set_bilingual(second, lid, LangMode::Auto);
    engine.set_grammar(Grammar::defaults());
    let anglicisms = Anglicisms::load(&root.join("resources/anglicisms.tsv"), 3)?;
    engine.set_anglicisms(Some(anglicisms), vec!["ru-RU"]);

    let mut totals = Totals::default();
    for scenario in manifest.iter().take(limit) {
        let audio = read_wav(&testdata.join("corpus/dictation").join(&scenario.file))?;
        engine.reset();
        let mut live = Live::default();
        // Часы воркера (мс): не раньше звука, но позже, если воркер не успевал.
        let mut clock = 0.0f64;
        for (index, chunk) in audio.chunks(CHUNK_SAMPLES).enumerate() {
            let audio_ms = ((index + 1) * CHUNK_SAMPLES) as f64 / 16.0;
            clock = clock.max(audio_ms);
            let started = Instant::now();
            let events = engine.push_audio(chunk);
            let spent = started.elapsed().as_secs_f64() * 1000.0;
            clock += spent;
            for event in events {
                match event {
                    EngineEvent::DictationPartial(raw, settled) => {
                        totals.partial_ms.push(spent);
                        let chars = live.partial(&raw, settled, clock, char_ms, blocking);
                        if blocking {
                            clock += chars as f64 * char_ms;
                        }
                    }
                    EngineEvent::Dictation(raw) => {
                        totals.chunk_ms.push(spent);
                        // Готовый кусок воркер допечатывает сам: ждёт печать по ходу речи
                        // (`sync_live`), затем `settle`.
                        clock = clock.max(live.typist_free);
                        live.chunk(&raw, clock, char_ms, true);
                        clock = clock.max(live.typist_free);
                    }
                    _ => {}
                }
            }
            totals.worker_behind_ms = totals.worker_behind_ms.max(clock - audio_ms);
        }
        let lags: Vec<f64> = live
            .field_at
            .iter()
            .zip(&live.bubble_at)
            .map(|(field, bubble)| field - bubble)
            .collect();
        let gaps: Vec<f64> = live.updates.windows(2).map(|w| w[1].0 - w[0].0).collect();
        eprintln!(
            "{:<40} слов {:>3}  отставание поля от пузыря: ср {:>5.0} мс, p90 {:>5.0}  паузы между вводами p90 {:>5.0} мс",
            scenario.scenario,
            live.field_at.len(),
            mean(&lags),
            percentile(&lags, 0.9),
            percentile(&gaps, 0.9)
        );
        totals.lags.extend(lags);
        totals.gaps.extend(gaps);
        totals
            .batch_chars
            .extend(live.updates.iter().map(|&(_, chars)| chars));
        totals.rewrites += live.rewrites;
        totals.corrections += live.corrections;
        totals.words += live.field_at.len();
    }

    let batches: Vec<f64> = totals.batch_chars.iter().map(|&c| c as f64).collect();
    println!(
        "печать символа: {char_ms} мс, воркер {}",
        if blocking {
            "ждёт печати"
        } else {
            "не ждёт печати по ходу речи"
        }
    );
    println!(
        "отставание поля от пузыря, мс: ср {:.0}, медиана {:.0}, p90 {:.0}, макс {:.0}",
        mean(&totals.lags),
        percentile(&totals.lags, 0.5),
        percentile(&totals.lags, 0.9),
        percentile(&totals.lags, 1.0)
    );
    println!(
        "паузы между вводами в поле, мс: медиана {:.0}, p90 {:.0}; символов за ввод: медиана {:.0}, p90 {:.0}",
        percentile(&totals.gaps, 0.5),
        percentile(&totals.gaps, 0.9),
        percentile(&batches, 0.5),
        percentile(&batches, 0.9)
    );
    println!(
        "расчёт промежуточного текста, мс: ср {:.0}, p90 {:.0}; готового куска: ср {:.0}, p90 {:.0}",
        mean(&totals.partial_ms),
        percentile(&totals.partial_ms, 0.9),
        mean(&totals.chunk_ms),
        percentile(&totals.chunk_ms, 0.9)
    );
    println!(
        "исправлений в поле: {} на {} слов (стёрто символов: {}); воркер отставал от звука максимум на {:.0} мс",
        totals.corrections, totals.words, totals.rewrites, totals.worker_behind_ms
    );
    Ok(())
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не прочитан {}", path.display()))?;
    anyhow::ensure!(
        wave.sample_rate() == 16_000,
        "{}: нужен 16 кГц",
        path.display()
    );
    Ok(wave
        .samples()
        .iter()
        .map(|&sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect())
}
