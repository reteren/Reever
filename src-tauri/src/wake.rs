//! Фоновый детектор wake-слова на маленькой потоковой модели (Vosk small-ru, zipformer,
//! sherpa-onnx). Слушает эфир непрерывно, без ожидания паузы, и срабатывает, как только
//! в промежуточном тексте появилось «ривер». Большая модель (GigaAM) по-прежнему
//! распознаёт команду; детектор лишь подтверждает, что wake-слово было, когда GigaAM
//! расслышала его неверно, и даёт мгновенный сигнал «услышал».

use std::path::Path;

use anyhow::{bail, Context};
use sherpa_onnx::{
    OnlineRecognizer, OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig,
};

use crate::asr::CpuProviderConfig;
use crate::speech::{WakeSpotter, SAMPLE_RATE};
use crate::textnorm::normalize;

pub struct StreamingWakeSpotter {
    recognizer: OnlineRecognizer,
    stream: OnlineStream,
    wake_words: Vec<String>,
    /// Уже сработали на текущей фразе: повторное «ривер» в ней же не считается.
    fired: bool,
    buffer: Vec<f32>,
}

impl StreamingWakeSpotter {
    /// `model_dir` — sherpa-onnx-streaming-zipformer-*: encoder/decoder/joiner + tokens.txt.
    pub fn new(model_dir: &Path, wake_words: &[String]) -> anyhow::Result<Self> {
        if !model_dir.is_dir() {
            bail!(
                "wake word detector model not found: {}",
                model_dir.display()
            );
        }
        let file = |stem: &str| -> anyhow::Result<String> {
            let mut candidates: Vec<_> = std::fs::read_dir(model_dir)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension().is_some_and(|ext| ext == "onnx")
                        && path
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with(stem))
                })
                .collect();
            // int8 быстрее; полная точность — если int8 нет.
            candidates.sort_by_key(|path| !path.to_string_lossy().contains("int8"));
            candidates
                .into_iter()
                .next()
                .map(|path| path.to_string_lossy().into_owned())
                .with_context(|| format!("no {stem}*.onnx in {}", model_dir.display()))
        };

        let mut config = OnlineRecognizerConfig::default();
        let cpu_provider = CpuProviderConfig::new()?;
        config.model_config.transducer = OnlineTransducerModelConfig {
            encoder: Some(file("encoder")?),
            decoder: Some(file("decoder")?),
            joiner: Some(file("joiner")?),
        };
        config.model_config.tokens =
            Some(model_dir.join("tokens.txt").to_string_lossy().into_owned());
        config.model_config.num_threads = 1;
        config.model_config.provider = Some(cpu_provider.provider());
        config.decoding_method = Some("greedy_search".into());
        // Конец фразы — чтобы текст не копился бесконечно и wake снова мог сработать.
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = 1.5;
        config.rule2_min_trailing_silence = 0.6;
        config.rule3_min_utterance_length = 12.0;
        let recognizer = OnlineRecognizer::create(&config)
            .context("sherpa-onnx failed to load the wake word detector model")?;
        let stream = recognizer.create_stream();
        Ok(Self {
            recognizer,
            stream,
            wake_words: wake_words.to_vec(),
            fired: false,
            buffer: Vec::new(),
        })
    }
}

/// Есть ли в тексте wake-слово: точное слово или вариант из списка, одна опечатка при
/// той же длине («ривир»), склейка с основным wake («риверпли»). Недописанное последнее
/// слово («приве») не проходит: нечёткой склейки «п + ривер», как в `strip_wake`, здесь нет.
/// Похожие слова в середине фразы («включи реверс») отсекает Engine: засчитывается только
/// срабатывание в начале речи.
pub fn contains_wake(text: &str, wake_words: &[String]) -> bool {
    let wakes: Vec<String> = wake_words.iter().map(|word| normalize(word)).collect();
    normalize(text).split_whitespace().any(|word| {
        wakes.iter().enumerate().any(|(index, wake)| {
            !wake.is_empty()
                && (word == wake
                    || (index == 0 && word.starts_with(wake.as_str()))
                    || (word.chars().count() == wake.chars().count()
                        && strsim::normalized_levenshtein(word, wake) >= 0.8))
        })
    })
}

impl WakeSpotter for StreamingWakeSpotter {
    fn feed(&mut self, pcm: &[i16]) -> bool {
        if pcm.is_empty() {
            return false;
        }
        self.buffer.clear();
        self.buffer
            .extend(pcm.iter().map(|sample| *sample as f32 / 32_768.0));
        self.stream
            .accept_waveform(SAMPLE_RATE as i32, &self.buffer);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let mut hit = false;
        if !self.fired {
            if let Some(result) = self.recognizer.get_result(&self.stream) {
                if std::env::var_os("REEVER_WAKE_DEBUG").is_some() && !result.text.is_empty() {
                    eprintln!("[wake] {}", result.text);
                }
                if contains_wake(&result.text, &self.wake_words) {
                    self.fired = true;
                    hit = true;
                }
            }
        }
        if self.recognizer.is_endpoint(&self.stream) {
            self.reset();
        }
        hit
    }

    fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
        self.fired = false;
    }

    fn set_wake_words(&mut self, wake_words: &[String]) {
        self.wake_words = wake_words.to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::contains_wake;

    #[test]
    fn wake_match_is_strict_about_partial_words() {
        let wake = vec!["ривер".to_string(), "ревер".into()];
        assert!(contains_wake("ривер открой", &wake));
        assert!(contains_wake("ну ривер", &wake));
        assert!(contains_wake("риверпли", &wake));
        assert!(contains_wake("ривир", &wake));
        assert!(contains_wake("ревер", &wake));
        assert!(!contains_wake("приве", &wake));
        assert!(!contains_wake("привет", &wake));
        assert!(!contains_wake("реверс", &wake));
        assert!(!contains_wake("отправь отчёт", &wake));
    }
}
