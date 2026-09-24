//! Офлайн-ASR: GigaAM v3 (русский) через sherpa-onnx. Владелец: воркер `asr`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use sherpa_onnx::{
    OfflineNemoEncDecCtcModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig,
};

use crate::{speech::Transcriber, textnorm};

const MIN_AUDIO_SAMPLES: usize = 3_200; // 0,2 секунды при 16 кГц

pub struct AsrConfig {
    /// Папка модели sherpa-onnx (например, resources/model/<asr_model>).
    pub model_dir: PathBuf,
    pub num_threads: i32,
}

pub struct Asr {
    recognizer: OfflineRecognizer,
}

impl Asr {
    /// Загружает NeMo transducer или CTC по файлам в каталоге модели.
    pub fn new(cfg: &AsrConfig) -> anyhow::Result<Self> {
        let model_dir = &cfg.model_dir;
        if !model_dir.is_dir() {
            bail!("каталог ASR-модели не найден: {}", model_dir.display());
        }

        let tokens = model_dir.join("tokens.txt");
        let encoder = best_onnx(model_dir, "encoder")?;
        let decoder = model_dir.join("decoder.onnx");
        let joiner = model_dir.join("joiner.onnx");
        let ctc_model = best_onnx(model_dir, "model")?;

        let mut recognizer_config = OfflineRecognizerConfig::default();
        recognizer_config.model_config.tokens = Some(path_string(&tokens));
        recognizer_config.model_config.num_threads = cfg.num_threads.max(1);

        if let (Some(encoder), true, true) = (encoder, decoder.is_file(), joiner.is_file()) {
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
        } else {
            let found = onnx_files(model_dir)?;
            bail!(
                "в {} не найдена поддерживаемая sherpa-onnx NeMo-модель; нужны encoder*.onnx + decoder.onnx + joiner.onnx + tokens.txt (transducer) либо model*.onnx + tokens.txt (CTC). ONNX-файлы: {}{}",
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
        if pcm.is_empty() {
            return Ok(String::new());
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
        Ok(result.text)
    }
}

impl Transcriber for Asr {
    fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        Ok(textnorm::normalize(&self.transcribe_raw(pcm)?))
    }
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

    use super::{Asr, AsrConfig};
    use crate::speech::Transcriber;

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
}
