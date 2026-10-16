//! Офлайн-ASR: GigaAM v3 (русский) через sherpa-onnx. Владелец: воркер `asr`.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{bail, Context};
use sherpa_onnx::{
    OfflineNemoEncDecCtcModelConfig, OfflineQwen3ASRModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, OfflineTransducerModelConfig, OfflineWhisperModelConfig,
};

use crate::{
    speech::{Timed, TimedWord, Transcriber},
    textnorm,
};

const MIN_AUDIO_SAMPLES: usize = 3_200; // 0,2 секунды при 16 кГц

/// sherpa-onnx 1.13.8 принимает параметры CPU arena только через `cpu:<файл>`.
/// Файл живёт лишь до создания сессии и удаляется RAII-объектом.
pub(crate) struct CpuProviderConfig {
    path: PathBuf,
}

impl CpuProviderConfig {
    pub(crate) fn new() -> anyhow::Result<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("reever-ort-cpu-{}-{id}.conf", std::process::id()));
            let file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "не удалось создать конфигурацию ONNX Runtime {}",
                            path.display()
                        )
                    });
                }
            };
            let config = Self { path };
            let mut file = file;
            file.write_all(b"EnableCpuMemArena=0\n").with_context(|| {
                format!(
                    "не удалось записать конфигурацию ONNX Runtime {}",
                    config.path.display()
                )
            })?;
            return Ok(config);
        }
        bail!("не удалось создать уникальную конфигурацию ONNX Runtime")
    }

    pub(crate) fn provider(&self) -> String {
        format!("cpu:{}", self.path.to_string_lossy())
    }
}

impl Drop for CpuProviderConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub struct AsrConfig {
    /// Папка модели sherpa-onnx (например, resources/model/<asr_model>).
    pub model_dir: PathBuf,
    pub num_threads: i32,
}

pub struct Asr {
    recognizer: OfflineRecognizer,
}

impl Asr {
    /// Загружает поддерживаемую sherpa-onnx модель по её файлам.
    pub fn new(cfg: &AsrConfig) -> anyhow::Result<Self> {
        let model_dir = &cfg.model_dir;
        if !model_dir.is_dir() {
            bail!("каталог ASR-модели не найден: {}", model_dir.display());
        }

        let tokens = model_dir.join("tokens.txt");
        let encoder = best_onnx(model_dir, "encoder")?;
        let decoder = best_onnx(model_dir, "decoder")?;
        let joiner = best_onnx(model_dir, "joiner")?;
        let ctc_model = best_onnx(model_dir, "model")?;
        let qwen_frontend = model_dir.join("conv_frontend.onnx");
        let qwen_tokenizer = model_dir.join("tokenizer");
        let whisper_encoder = best_onnx_component(model_dir, "-encoder")?;
        let whisper_decoder = best_onnx_component(model_dir, "-decoder")?;
        let whisper_tokens = if tokens.is_file() {
            Some(tokens.clone())
        } else {
            best_file_with_suffix(model_dir, "-tokens.txt")?
        };

        let mut recognizer_config = OfflineRecognizerConfig::default();
        recognizer_config.model_config.tokens = Some(path_string(&tokens));
        recognizer_config.model_config.num_threads = cfg.num_threads.max(1);
        let cpu_provider = CpuProviderConfig::new()?;
        recognizer_config.model_config.provider = Some(cpu_provider.provider());

        if qwen_frontend.is_file()
            && qwen_tokenizer.is_dir()
            && encoder.is_some()
            && decoder.is_some()
        {
            recognizer_config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
                conv_frontend: Some(path_string(&qwen_frontend)),
                encoder: encoder.map(|path| path_string(&path)),
                decoder: decoder.map(|path| path_string(&path)),
                tokenizer: Some(path_string(&qwen_tokenizer)),
                ..Default::default()
            };
            // Qwen хранит токенизатор отдельно и ожидает пустой общий путь tokens.
            recognizer_config.model_config.tokens = Some(String::new());
        } else if let (Some(encoder), Some(decoder), Some(joiner)) = (encoder, decoder, joiner) {
            if !tokens.is_file() {
                bail!(
                    "не найден tokens.txt для NeMo transducer-модели в {}",
                    model_dir.display()
                );
            }
            recognizer_config.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(path_string(&encoder)),
                decoder: Some(path_string(&decoder)),
                joiner: Some(path_string(&joiner)),
            };
            recognizer_config.model_config.model_type = Some("nemo_transducer".into());
        } else if let Some(model) = ctc_model {
            if !tokens.is_file() {
                bail!(
                    "не найден tokens.txt для NeMo CTC-модели в {}",
                    model_dir.display()
                );
            }
            recognizer_config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                model: Some(path_string(&model)),
            };
            recognizer_config.model_config.model_type = Some("nemo_ctc".into());
        } else if let (Some(encoder), Some(decoder)) = (whisper_encoder, whisper_decoder) {
            let Some(whisper_tokens) = whisper_tokens else {
                bail!(
                    "не найден tokens.txt для Whisper-модели в {}",
                    model_dir.display()
                );
            };
            recognizer_config.model_config.tokens = Some(path_string(&whisper_tokens));
            let model_name = model_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let language = (model_name.ends_with(".en") || model_name.contains("-en-"))
                .then(|| "en".to_string());
            recognizer_config.model_config.whisper = OfflineWhisperModelConfig {
                encoder: Some(path_string(&encoder)),
                decoder: Some(path_string(&decoder)),
                language,
                task: Some("transcribe".into()),
                enable_token_timestamps: true,
                ..Default::default()
            };
            recognizer_config.model_config.model_type = Some("whisper".into());
        } else {
            let found = onnx_files(model_dir)?;
            bail!(
                "в {} не найдена поддерживаемая sherpa-onnx модель; нужны encoder*.onnx + decoder*.onnx + joiner*.onnx + tokens.txt (NeMo transducer), model*.onnx + tokens.txt (CTC), Whisper *-encoder*.onnx + *-decoder*.onnx + tokens.txt либо Qwen3 conv_frontend.onnx + encoder*.onnx + decoder*.onnx + tokenizer/. ONNX-файлы: {}{}",
                model_dir.display(),
                if found.is_empty() { "нет".into() } else { found.join(", ") },
                if tokens.is_file() { "" } else { "; также отсутствует tokens.txt" }
            );
        }

        let recognizer = OfflineRecognizer::create(&recognizer_config).with_context(|| {
            format!(
                "sherpa-onnx не смог загрузить модель из {}",
                model_dir.display()
            )
        })?;

        Ok(Self { recognizer })
    }

    /// Возвращает исходный вывод sherpa-onnx без нормализации; используется бенчмарком.
    pub fn transcribe_raw(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        Ok(self
            .decode_result(pcm)?
            .map(|result| result.text)
            .unwrap_or_default())
    }

    fn decode_result(
        &mut self,
        pcm: &[i16],
    ) -> anyhow::Result<Option<sherpa_onnx::OfflineRecognizerResult>> {
        if pcm.is_empty() {
            return Ok(None);
        }

        let mut waveform: Vec<f32> = pcm
            .iter()
            .map(|sample| f32::from(*sample) / 32_768.0)
            .collect();
        // Короткие сегменты дополняем тишиной: фронтенду модели нужны хотя бы
        // несколько акустических кадров, а сегментер может отдать меньше 0,2 с.
        waveform.resize(waveform.len().max(MIN_AUDIO_SAMPLES), 0.0);

        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, &waveform);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .context("sherpa-onnx не вернул результат распознавания")?;
        Ok(Some(result))
    }
}

impl Transcriber for Asr {
    fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        Ok(textnorm::normalize(&self.transcribe_raw(pcm)?))
    }

    fn transcribe_verbatim(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        Ok(self
            .transcribe_raw(pcm)?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "))
    }

    fn transcribe_timed(&mut self, pcm: &[i16]) -> anyhow::Result<Timed> {
        let Some(result) = self.decode_result(pcm)? else {
            return Ok(Timed::default());
        };
        let text = textnorm::normalize(&result.text);
        let duration = pcm.len() as f32 / 16_000.0;
        let words = timed_words(
            &result.tokens,
            result.timestamps.as_deref(),
            result.durations.as_deref(),
            duration,
        );
        Ok(Timed { text, words })
    }
}

fn timed_words(
    tokens: &[String],
    timestamps: Option<&[f32]>,
    durations: Option<&[f32]>,
    audio_duration: f32,
) -> Vec<TimedWord> {
    let Some(timestamps) = timestamps else {
        return Vec::new();
    };
    let count = tokens.len().min(timestamps.len());
    if count == 0 {
        return Vec::new();
    }

    let mut words = Vec::new();
    let mut current = String::new();
    let mut word_start = 0.0_f32;
    let mut word_end = 0.0_f32;
    let mut previous_end = 0.0_f32;

    for index in 0..count {
        let token = &tokens[index];
        let bpe_start = token.starts_with('▁');
        let piece = token.strip_prefix('▁').unwrap_or(token);
        if bpe_start && !current.is_empty() {
            flush_timed_word(
                &mut words,
                &mut current,
                word_start,
                word_end,
                &mut previous_end,
            );
        }
        if piece.is_empty() || is_special_token(piece) {
            continue;
        }

        let timestamp = finite_time(timestamps[index]).clamp(0.0, audio_duration.max(0.0));
        let next_timestamp = timestamps
            .get(index + 1)
            .copied()
            .map(finite_time)
            .unwrap_or(audio_duration)
            .clamp(timestamp, audio_duration.max(timestamp));
        let duration_end = durations
            .and_then(|items| items.get(index))
            .copied()
            .filter(|duration| duration.is_finite() && *duration > 0.0)
            .map(|duration| timestamp + duration);
        let token_end = duration_end
            .unwrap_or(next_timestamp)
            .clamp(timestamp, audio_duration.max(timestamp));

        for character in piece.chars() {
            if character.is_whitespace() {
                flush_timed_word(
                    &mut words,
                    &mut current,
                    word_start,
                    word_end,
                    &mut previous_end,
                );
                continue;
            }
            if current.is_empty() {
                word_start = timestamp;
            }
            current.push(character);
            word_end = token_end.max(timestamp);
        }
    }
    flush_timed_word(
        &mut words,
        &mut current,
        word_start,
        word_end,
        &mut previous_end,
    );
    words
}

fn flush_timed_word(
    words: &mut Vec<TimedWord>,
    text: &mut String,
    start: f32,
    end: f32,
    previous_end: &mut f32,
) {
    if text.is_empty() {
        return;
    }
    let normalized = textnorm::normalize(text);
    text.clear();
    let start = start.max(*previous_end);
    let end = end.max(start);
    for word in normalized.split_whitespace() {
        words.push(TimedWord {
            text: word.to_string(),
            start,
            end,
        });
    }
    *previous_end = end;
}

fn finite_time(time: f32) -> f32 {
    if time.is_finite() {
        time.max(0.0)
    } else {
        0.0
    }
}

fn is_special_token(token: &str) -> bool {
    token.starts_with('<') && token.ends_with('>')
}

fn best_onnx(dir: &Path, prefix: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать каталог модели {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "onnx")
                && path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().starts_with(prefix))
        })
        .collect();

    candidates.sort_by(|left, right| {
        let left_name = left
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let right_name = right
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let left_int8 = left_name.ends_with(".int8.onnx");
        let right_int8 = right_name.ends_with(".int8.onnx");
        right_int8
            .cmp(&left_int8)
            .then_with(|| left_name.cmp(&right_name))
    });

    Ok(candidates.into_iter().next())
}

fn best_onnx_component(dir: &Path, marker: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать каталог модели {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "onnx")
                && path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().contains(marker))
        })
        .collect();

    candidates.sort_by(|left, right| {
        let left_name = left
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let right_name = right
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let left_int8 = left_name.ends_with(".int8.onnx");
        let right_int8 = right_name.ends_with(".int8.onnx");
        right_int8
            .cmp(&left_int8)
            .then_with(|| left_name.cmp(&right_name))
    });

    Ok(candidates.into_iter().next())
}

fn best_file_with_suffix(dir: &Path, suffix: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать каталог модели {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(suffix))
        })
        .collect();
    candidates.sort();
    Ok(candidates.into_iter().next())
}

fn onnx_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut files: Vec<String> = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать каталог модели {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "onnx"))
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    files.sort();
    Ok(files)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::{env, path::PathBuf};

    use sherpa_onnx::Wave;

    use super::{timed_words, Asr, AsrConfig, CpuProviderConfig};
    use crate::speech::Transcriber;

    #[test]
    fn cpu_provider_config_disables_arena_and_removes_its_file() {
        let config = CpuProviderConfig::new().unwrap();
        let path = config.path.clone();

        assert!(config.provider().starts_with("cpu:"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "EnableCpuMemArena=0\n"
        );

        drop(config);
        assert!(!path.exists());
    }

    #[test]
    fn timed_words_group_character_and_sentencepiece_tokens() {
        let characters = ["п", "а", "п", "к", "а", " ", "д", "о", "м"].map(str::to_string);
        let times = [0.0, 0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40];
        let durations = [0.05; 9];
        let words = timed_words(&characters, Some(&times), Some(&durations), 1.0);
        assert_eq!(
            words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            ["папка", "дом"]
        );
        assert!(words[0].start < words[1].start);

        let pieces = ["▁open", "ing", "▁down", "loads"].map(str::to_string);
        let times = [0.2, 0.3, 0.5, 0.6];
        let words = timed_words(&pieces, Some(&times), None, 1.0);
        assert_eq!(
            words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            ["opening", "downloads"]
        );
        assert!(words[0].end <= words[1].start);
    }

    #[test]
    fn timed_words_are_empty_without_token_timestamps() {
        let tokens = ["▁hello".to_string()];
        assert!(timed_words(&tokens, None, None, 1.0).is_empty());
        assert!(timed_words(&tokens, Some(&[]), None, 1.0).is_empty());
    }

    #[test]
    #[ignore = "нужны скачанная GigaAM-модель и путь WAV в ASR_TEST_WAV"]
    fn recognizes_one_wav() {
        let model_dir = env::var_os("ASR_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                    "resources/model/sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16",
                )
            });
        let wav_path = PathBuf::from(env::var_os("ASR_TEST_WAV").expect("задайте ASR_TEST_WAV"));
        let wave = Wave::read(&wav_path.to_string_lossy()).expect("прочитать тестовый WAV");
        assert_eq!(wave.sample_rate(), 16_000, "WAV должен быть 16 кГц");

        let pcm: Vec<i16> = wave
            .samples()
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
            .collect();
        let mut asr = Asr::new(&AsrConfig {
            model_dir,
            num_threads: 4,
        })
        .expect("загрузить модель");
        let text = asr.transcribe(&pcm).expect("распознать WAV");
        assert!(!text.is_empty(), "модель вернула пустой текст");
        println!("{text}");
    }

    #[test]
    #[ignore = "нужны скачанная GigaAM-модель и путь WAV в ASR_TEST_WAV"]
    fn transcribe_timed_returns_monotonic_words_over_the_speech() {
        let model_dir = env::var_os("ASR_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                    "resources/model/sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16",
                )
            });
        let wav_path = PathBuf::from(env::var_os("ASR_TEST_WAV").expect("задайте ASR_TEST_WAV"));
        let wave = Wave::read(&wav_path.to_string_lossy()).expect("прочитать тестовый WAV");
        assert_eq!(wave.sample_rate(), 16_000, "WAV должен быть 16 кГц");
        let pcm: Vec<i16> = wave
            .samples()
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
            .collect();
        let audio_duration = pcm.len() as f32 / 16_000.0;
        let mut asr = Asr::new(&AsrConfig {
            model_dir,
            num_threads: 4,
        })
        .expect("загрузить модель");
        let result = asr
            .transcribe_timed(&pcm)
            .expect("распознать WAV с таймингами");
        println!("timed result: {result:?}");
        assert!(!result.text.is_empty(), "модель вернула пустой текст");
        assert!(
            !result.words.is_empty(),
            "модель не вернула таймстемпы токенов"
        );
        let mut previous_start = 0.0;
        let mut previous_end = 0.0;
        for word in &result.words {
            assert!(
                word.start >= previous_start,
                "слова не монотонны: {result:?}"
            );
            assert!(word.end >= word.start, "пустой интервал: {word:?}");
            assert!(word.end <= audio_duration + 0.05, "время вне WAV: {word:?}");
            assert!(
                word.end >= previous_end,
                "концы слов не монотонны: {result:?}"
            );
            previous_start = word.start;
            previous_end = word.end;
        }
        assert!(result.words.last().unwrap().end > result.words[0].start);
    }
}
