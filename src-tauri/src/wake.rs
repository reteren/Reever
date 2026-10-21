//! Фоновый детектор wake-слова: многоязычная акустическая сеть RKW2 (kws.rs, обучение —
//! tools/kws/train_multilang.py, веса — resources/wake/river.bin, 117 КБ). Слушает эфир
//! непрерывно и узнаёт сам звук «river / ривер» на любом языке, а не текст: десятые доли
//! процента одного ядра и треть мегабайта памяти. Большая модель распознаёт команду и
//! подтверждает, что wake-слово было (engine.rs, `confirm_wake`): ложное срабатывание
//! детектора лишь будит её.
//!
//! Детектор знает только «river»: свои wake-слова из настроек он не слышит (их по-прежнему
//! находит большая модель, пока она загружена).

use std::path::Path;

use crate::kws::StreamingTcn;
use crate::speech::WakeSpotter;

/// Порог оценки сети. 0,9 — компромисс (docs/research/wake-detector.md): ~98 % «river» у
/// живых дикторов английского, 112/150 русского корпуса на голосах Windows, около 8 ложных
/// срабатываний в час сплошной живой речи. Выше порог — меньше ложных и больше пропусков.
pub const THRESHOLD: f32 = 0.9;

pub struct AcousticWakeSpotter {
    kws: StreamingTcn,
    buffer: Vec<f32>,
}

impl AcousticWakeSpotter {
    /// `weights` — файл RKW2 (resources/wake/river.bin).
    pub fn new(weights: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            kws: StreamingTcn::from_path(weights, THRESHOLD)?,
            buffer: Vec::new(),
        })
    }
}

impl WakeSpotter for AcousticWakeSpotter {
    fn feed(&mut self, pcm: &[i16]) -> bool {
        self.buffer.clear();
        self.buffer
            .extend(pcm.iter().map(|sample| *sample as f32 / 32_768.0));
        self.kws.feed(&self.buffer).is_some()
    }

    fn reset(&mut self) {
        self.kws.reset();
    }

    fn set_wake_words(&mut self, _wake_words: &[String]) {}
}
