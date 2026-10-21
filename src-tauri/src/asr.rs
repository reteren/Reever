//! Офлайн-ASR через sherpa-onnx: модель по файлам в папке (NeMo transducer/CTC, SenseVoice,
//! Whisper, Qwen3) и набор моделей под включённые языки (`AsrSet`).

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{bail, Context};
use sherpa_onnx::{
    OfflineNemoEncDecCtcModelConfig, OfflineQwen3ASRModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig,
    OfflineWhisperModelConfig,
};

use crate::{
    langid::SpeechLid,
    models::ModelPlan,
    speech::{samples, Timed, TimedWord, Transcriber},
    textnorm,
};

const MIN_AUDIO_SAMPLES: usize = 3_200; // 0,2 секунды при 16 кГц
const SHORT_AUDIO_SAMPLES: usize = samples(1_000);
const SHORT_AUDIO_PAD: usize = samples(700);

/// sherpa-onnx 1.13.8 принимает параметры CPU arena только через `cpu:<файл>`.
/// Файл живёт лишь до создания сессии и удаляется RAII-объектом.
pub(crate) struct CpuProviderConfig {
    path: PathBuf,
}

impl CpuProviderConfig {
    pub(crate) fn new() -> anyhow::Result<Self> {
        Self::with_options("")
    }

    /// `extra` — ещё строки конфига провайдера sherpa-onnx (`ключ=значение\n`).
    pub(crate) fn with_options(extra: &str) -> anyhow::Result<Self> {
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
                            "failed to create the ONNX Runtime config {}",
                            path.display()
                        )
                    });
                }
            };
            let config = Self { path };
            let mut file = file;
            file.write_all(format!("EnableCpuMemArena=0\n{extra}").as_bytes()).with_context(|| {
                format!(
                    "failed to write the ONNX Runtime config {}",
                    config.path.display()
                )
            })?;
            return Ok(config);
        }
        bail!("failed to create a unique ONNX Runtime config")
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
    /// Загружает поддерживаемую sherpa-onnx модель по её файлам. Есть заранее
    /// оптимизированные графы (ortopt.rs) — грузит их, не загрузились — исходные файлы.
    pub fn new(cfg: &AsrConfig) -> anyhow::Result<Self> {
        match Self::build(cfg, true) {
            Ok(Some(asr)) => return Ok(asr),
            Ok(None) => {}
            Err(_) => crate::ortopt::forget(&cfg.model_dir),
        }
        Self::build(cfg, false).map(|asr| asr.expect("без кэша модель собирается всегда"))
    }

    /// `optimized` — только из кэша оптимизированных графов; `Ok(None)` — кэша нет.
    fn build(cfg: &AsrConfig, optimized: bool) -> anyhow::Result<Option<Self>> {
        let model_dir = &cfg.model_dir;
        if !model_dir.is_dir() {
            bail!("ASR model folder not found: {}", model_dir.display());
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
        let cached = |path: &Option<PathBuf>| path.as_deref().and_then(crate::ortopt::cached);
        if optimized
            && [&encoder, &decoder, &joiner, &ctc_model, &whisper_encoder, &whisper_decoder]
                .iter()
                .all(|path| path.is_none() || cached(path).is_none())
        {
            return Ok(None);
        }
        let swap = |path: Option<PathBuf>| -> Option<PathBuf> {
            if optimized {
                path.map(|p| crate::ortopt::cached(&p).unwrap_or(p))
            } else {
                path
            }
        };
        let (encoder, decoder, joiner, ctc_model) = (swap(encoder), swap(decoder), swap(joiner), swap(ctc_model));
        let (whisper_encoder, whisper_decoder) = (swap(whisper_encoder), swap(whisper_decoder));
        let whisper_tokens = if tokens.is_file() {
            Some(tokens.clone())
        } else {
            best_file_with_suffix(model_dir, "-tokens.txt")?
        };

        let mut recognizer_config = OfflineRecognizerConfig::default();
        recognizer_config.model_config.tokens = Some(path_string(&tokens));
        recognizer_config.model_config.num_threads = cfg.num_threads.max(1);
        // Граф уже оптимизирован — второй раз ORT его не трогает: в этом и выигрыш загрузки.
        let cpu_provider = CpuProviderConfig::with_options(if optimized { "GraphOptimizationLevel=0\n" } else { "" })?;
        recognizer_config.model_config.provider = Some(cpu_provider.provider());

        let sense_voice = model_dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("sense-voice"));

        if sense_voice {
            // SenseVoice — тоже model*.onnx + tokens.txt, как NeMo CTC; отличить можно
            // только по имени папки.
            let (Some(model), true) = (ctc_model.as_ref(), tokens.is_file()) else {
                bail!(
                    "SenseVoice needs model*.onnx and tokens.txt in {}",
                    model_dir.display()
                );
            };
            recognizer_config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(path_string(model)),
                // Язык фразы модель определяет сама; ITN пишет числа цифрами.
                language: None,
                use_itn: true,
            };
        } else if qwen_frontend.is_file()
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
                    "tokens.txt for the NeMo transducer model not found in {}",
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
                    "tokens.txt for the NeMo CTC model not found in {}",
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
                    "tokens.txt for the Whisper model not found in {}",
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
                "no supported sherpa-onnx model in {}; expected encoder*.onnx + decoder*.onnx + joiner*.onnx + tokens.txt (NeMo transducer), model*.onnx + tokens.txt (CTC), Whisper *-encoder*.onnx + *-decoder*.onnx + tokens.txt or Qwen3 conv_frontend.onnx + encoder*.onnx + decoder*.onnx + tokenizer/. ONNX files: {}{}",
                model_dir.display(),
                if found.is_empty() { "none".into() } else { found.join(", ") },
                if tokens.is_file() { "" } else { "; tokens.txt is missing too" }
            );
        }

        let recognizer = OfflineRecognizer::create(&recognizer_config).with_context(|| {
            format!(
                "sherpa-onnx failed to load the model from {}",
                model_dir.display()
            )
        })?;

        Ok(Some(Self { recognizer }))
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
        // Многоязычная Parakeet v3 на одном коротком слове (до 1 с) без тишины вокруг теряет язык:
        // «да» → «Da», «D.». С 0,7 с тишины с обеих сторон — «Да.» (testdata/corpus, app_confirm).
        if waveform.len() < SHORT_AUDIO_SAMPLES {
            let mut padded = vec![0.0; SHORT_AUDIO_PAD];
            padded.extend_from_slice(&waveform);
            padded.resize(padded.len() + SHORT_AUDIO_PAD, 0.0);
            waveform = padded;
        }

        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, &waveform);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .context("sherpa-onnx returned no recognition result")?;
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

/// Короче этого определитель языка ошибается: «ривер» один — любая модель справится.
const LID_MIN_SAMPLES: usize = samples(700);
/// Решение по фразе длиной от стольких сэмплов переиспользуется для её продолжения.
const LID_REUSE_SAMPLES: usize = samples(1_500);

/// Модели распознавания под включённые языки (models.rs). Модель одна — фраза идёт ей;
/// несколько — определитель языка решает, какой модели её отдать.
pub struct AsrSet {
    models: Vec<(&'static [&'static str], Asr)>,
    lid: Option<SpeechLid>,
    /// Последнее решение: (отпечаток начала фразы, длина аудио, модель). Частичное
    /// распознавание одной фразы идёт каждые 400 мс — язык не нужно определять заново.
    last: Option<(u64, usize, usize)>,
}

impl AsrSet {
    /// Загружает все модели плана. Определитель языка не загрузился — фразы идут первой
    /// модели, а ошибка возвращается вторым значением для журнала.
    pub fn load(plan: &ModelPlan, num_threads: i32) -> anyhow::Result<(Self, Option<String>)> {
        let mut models = Vec::with_capacity(plan.models.len());
        for model in &plan.models {
            let asr = Asr::new(&AsrConfig {
                model_dir: model.dir.clone(),
                num_threads,
            })?;
            models.push((model.covers, asr));
        }
        let mut lid_error = None;
        let lid = match &plan.lid {
            Some(dir) if models.len() > 1 => match SpeechLid::new(dir, 1) {
                Ok(lid) => Some(lid),
                Err(error) => {
                    lid_error = Some(format!("{error:#}"));
                    None
                }
            },
            _ => None,
        };
        Ok((
            Self {
                models,
                lid,
                last: None,
            },
            lid_error,
        ))
    }

    fn pick(&mut self, pcm: &[i16]) -> usize {
        if self.models.len() < 2 {
            return 0;
        }
        let Some(lid) = self.lid.as_mut() else {
            return 0;
        };
        let fingerprint = fingerprint(pcm);
        if let Some((last_print, last_len, index)) = self.last {
            if last_print == fingerprint && (last_len >= LID_REUSE_SAMPLES || pcm.len() <= last_len)
            {
                return index;
            }
        }
        if pcm.len() < LID_MIN_SAMPLES {
            return 0;
        }
        let index = match lid.detect_code(pcm) {
            Ok(code) => self
                .models
                .iter()
                .position(|(covers, _)| covers.contains(&code.as_str()))
                .unwrap_or(0),
            Err(_) => 0,
        };
        self.last = Some((fingerprint, pcm.len(), index));
        index
    }

    fn asr(&mut self, pcm: &[i16]) -> &mut Asr {
        let index = self.pick(pcm);
        &mut self.models[index].1
    }
}

/// Отпечаток первых 0,5 с: у частичных распознаваний одной фразы он одинаковый.
fn fingerprint(pcm: &[i16]) -> u64 {
    pcm.iter()
        .take(samples(500))
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, sample| {
            (hash ^ u64::from(*sample as u16)).wrapping_mul(0x0100_0000_01b3)
        })
}

impl Transcriber for AsrSet {
    fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        self.asr(pcm).transcribe(pcm)
    }

    fn transcribe_verbatim(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        self.asr(pcm).transcribe_verbatim(pcm)
    }

    fn transcribe_timed(&mut self, pcm: &[i16]) -> anyhow::Result<Timed> {
        self.asr(pcm).transcribe_timed(pcm)
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
        .with_context(|| format!("failed to read the model folder {}", dir.display()))?
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
        .with_context(|| format!("failed to read the model folder {}", dir.display()))?
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
        .with_context(|| format!("failed to read the model folder {}", dir.display()))?
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
        .with_context(|| format!("failed to read the model folder {}", dir.display()))?
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
    #[ignore = "нужны скачанная Parakeet v3 и путь WAV в ASR_TEST_WAV"]
    fn recognizes_one_wav() {
        let model_dir = env::var_os("ASR_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("resources/model")
                    .join(crate::models::EUROPEAN.dir)
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
    #[ignore = "нужны скачанная Parakeet v3 и путь WAV в ASR_TEST_WAV"]
    fn transcribe_timed_returns_monotonic_words_over_the_speech() {
        let model_dir = env::var_os("ASR_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("resources/model")
                    .join(crate::models::EUROPEAN.dir)
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
