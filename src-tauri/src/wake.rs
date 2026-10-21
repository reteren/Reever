//! Фоновый детектор wake-слова: многоязычная акустическая сеть RKW2 (kws.rs, обучение —
//! tools/kws/train_multilang.py, веса — resources/wake/river.bin, 117 КБ). Слушает эфир
//! непрерывно и узнаёт сам звук «river / ривер» на любом языке, а не текст: десятые доли
//! процента одного ядра и треть мегабайта памяти. Большая модель распознаёт команду и
//! подтверждает, что wake-слово было (engine.rs, `confirm_wake`): ложное срабатывание
//! детектора лишь будит её.
//!
//! Сеть одна срабатывает и на шум (сирена, лай, плач, дыхание), поэтому кандидат проходит
//! фильтры, не зависящие от языка (замеры — examples/wake_gate_probe.rs, docs/research/
//! wake-detector.md): слово заметно громче фона и всплеск уверенности не случайный.
//! Третий фильтр — речь ли это по Silero VAD — в engine.rs, где VAD уже работает.
//!
//! Детектор знает только «river»: свои wake-слова из настроек он не слышит (их по-прежнему
//! находит большая модель, пока она загружена).

use std::collections::VecDeque;
use std::path::Path;

use crate::kws::{StreamingTcn, HOP_SAMPLES};
use crate::speech::WakeSpotter;

/// Порог оценки сети. 0,9 — компромисс (docs/research/wake-detector.md): ~98 % «river» у
/// живых дикторов английского, 112/150 русского корпуса на голосах Windows, около 8 ложных
/// срабатываний в час сплошной живой речи. Выше порог — меньше ложных и больше пропусков.
pub const THRESHOLD: f32 = 0.9;
/// Всплеск короче трёх кадров (30 мс) засчитывается, только если сеть уверена хотя бы
/// на столько: короткие всплески — в основном шум и чужая речь.
pub const SHORT_RUN_PEAK: f32 = 0.95;
/// Насколько слово должно быть громче фона, дБ: у живых «river» 99 % выше 10 дБ.
pub const MIN_SNR_DB: f32 = 10.0;
/// После срабатывания — ждать, пока оценка не продержится ниже 0,6 порога 300 мс.
const RELEASE_FRAMES: usize = 30;
/// Громкость слова — пик за последние 700 мс, фон — 10-й перцентиль за 3 с.
const WORD_FRAMES: usize = 70;
const FLOOR_FRAMES: usize = 300;

pub struct AcousticWakeSpotter {
    kws: StreamingTcn,
    buffer: Vec<f32>,
    scores: Vec<f32>,
    /// Звук, ещё не сложившийся в кадр громкости.
    pending: Vec<f32>,
    /// Громкость кадров 10 мс, дБ от полной шкалы.
    levels: VecDeque<f32>,
    run: usize,
    peak: f32,
    fired: bool,
    quiet: usize,
    note: Option<String>,
}

impl AcousticWakeSpotter {
    /// `weights` — файл RKW2 (resources/wake/river.bin).
    pub fn new(weights: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            // Срабатывания решает сам: StreamingTcn только считает оценки.
            kws: StreamingTcn::from_path(weights, 0.99)?,
            buffer: Vec::new(),
            scores: Vec::new(),
            pending: Vec::with_capacity(HOP_SAMPLES),
            levels: VecDeque::with_capacity(FLOOR_FRAMES + 1),
            run: 0,
            peak: 0.0,
            fired: false,
            quiet: 0,
            note: None,
        })
    }

    fn push_levels(&mut self) {
        for &sample in &self.buffer {
            self.pending.push(sample);
            if self.pending.len() == HOP_SAMPLES {
                let energy = self.pending.iter().map(|x| x * x).sum::<f32>() / HOP_SAMPLES as f32;
                if self.levels.len() == FLOOR_FRAMES {
                    self.levels.pop_front();
                }
                self.levels.push_back(10.0 * energy.max(1e-12).log10());
                self.pending.clear();
            }
        }
    }

    /// Громкость слова и фона, дБ.
    fn loudness(&self) -> (f32, f32) {
        let word = self
            .levels
            .iter()
            .rev()
            .take(WORD_FRAMES)
            .copied()
            .fold(-120.0, f32::max);
        let mut all: Vec<f32> = self.levels.iter().copied().collect();
        if all.is_empty() {
            return (word, -120.0);
        }
        let nth = all.len() / 10;
        let (_, floor, _) = all.select_nth_unstable_by(nth, f32::total_cmp);
        (word, *floor)
    }

    /// Кандидат: оценка выше порога, всплеск не случайный. Возвращает, принят ли он.
    fn judge(&mut self) -> bool {
        let (word, floor) = self.loudness();
        let snr = word - floor;
        let verdict = if snr < MIN_SNR_DB {
            Some(format!("rejected: only {snr:+.0} dB over the background"))
        } else {
            None
        };
        self.note = Some(format!(
            "score {:.2}, {} frames, level {word:.0} dB, background {floor:.0} dB ({snr:+.0}){}",
            self.peak,
            self.run,
            verdict.as_ref().map(|v| format!(" — {v}")).unwrap_or_default()
        ));
        verdict.is_none()
    }
}

impl WakeSpotter for AcousticWakeSpotter {
    fn feed(&mut self, pcm: &[i16]) -> bool {
        self.buffer.clear();
        self.buffer
            .extend(pcm.iter().map(|sample| *sample as f32 / 32_768.0));
        self.push_levels();
        self.scores.clear();
        self.kws.feed_scores(&self.buffer, &mut self.scores);
        let mut spotted = false;
        for i in 0..self.scores.len() {
            let score = self.scores[i];
            if self.fired {
                self.quiet = if score < THRESHOLD * 0.6 { self.quiet + 1 } else { 0 };
                if self.quiet >= RELEASE_FRAMES {
                    self.fired = false;
                    self.quiet = 0;
                }
                continue;
            }
            if score >= THRESHOLD {
                self.run += 1;
                self.peak = self.peak.max(score);
            } else {
                self.run = 0;
                self.peak = 0.0;
            }
            if self.run >= 3 || (self.run == 2 && self.peak >= SHORT_RUN_PEAK) {
                // Срабатывание решено: дальше этот всплеск не рассматривается, как бы он
                // ни кончился, — повторное только после затишья.
                self.fired = true;
                spotted |= self.judge();
                self.run = 0;
                self.peak = 0.0;
            }
        }
        spotted
    }

    fn reset(&mut self) {
        self.kws.reset();
        self.run = 0;
        self.peak = 0.0;
        self.fired = false;
        self.quiet = 0;
    }

    fn set_wake_words(&mut self, _wake_words: &[String]) {}

    fn take_note(&mut self) -> Option<String> {
        self.note.take()
    }
}
