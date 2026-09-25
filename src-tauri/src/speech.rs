//! Общие контракты голосового конвейера. Файл принадлежит координатору:
//! реализации (asr.rs, vad.rs) и потребители (segmenter.rs, engine.rs) меняются
//! независимо, пока держатся этих сигнатур. Менять — только через координатора.

/// Весь конвейер работает на 16 кГц mono i16 (так отдаёт audio.rs).
pub const SAMPLE_RATE: usize = 16_000;

/// Сэмплов в `ms` миллисекундах при SAMPLE_RATE.
pub const fn samples(ms: u32) -> usize {
    SAMPLE_RATE * ms as usize / 1000
}

/// Слово распознанного текста с положением в аудио (секунды от начала `pcm`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimedWord {
    /// Нормализованное слово (как в `Timed::text`).
    pub text: String,
    pub start: f32,
    pub end: f32,
}

/// Распознанный текст с таймингом слов. `words` может быть пустым, если модель не
/// отдаёт таймстемпы; тогда потребитель делит аудио пропорционально числу букв.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timed {
    /// Нормализованный текст — тот же, что вернул бы `transcribe`.
    pub text: String,
    pub words: Vec<TimedWord>,
}

/// Офлайн-распознавание законченного фрагмента речи.
pub trait Transcriber {
    /// `pcm` — 16 кГц mono. Возвращает НОРМАЛИЗОВАННЫЙ текст: нижний регистр,
    /// `ё`→`е`, без пунктуации, одиночные пробелы (см. `textnorm::normalize`).
    /// Пустая строка — речи не распознано (это не ошибка).
    fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String>;

    /// То же с таймингом слов — по нему [`crate::bilingual`] вырезает из фразы цель
    /// команды для второго языка. По умолчанию — без тайминга.
    fn transcribe_timed(&mut self, pcm: &[i16]) -> anyhow::Result<Timed> {
        Ok(Timed { text: self.transcribe(pcm)?, words: Vec::new() })
    }
}

/// Язык фрагмента речи.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ru,
    En,
}

/// Определение языка фрагмента речи (для цели команды: «поиск ютуб <это>»).
pub trait LanguageId {
    /// Язык и уверенность 0..1. Фрагмент 16 кГц mono, обычно 0,3–5 с.
    fn detect(&mut self, pcm: &[i16]) -> anyhow::Result<(Lang, f32)>;
}

/// Покадровый детектор речи.
pub trait VoiceDetector {
    /// Длина кадра в сэмплах; `is_speech` всегда получает ровно столько.
    fn frame_len(&self) -> usize;
    /// Есть ли речь в кадре. Детектор может держать внутреннее состояние
    /// (сглаживание), но НЕ должен сам решать про конец фразы — это segmenter.
    fn is_speech(&mut self, frame: &[i16]) -> bool;
    fn reset(&mut self);
}
