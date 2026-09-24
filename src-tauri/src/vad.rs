//! Детекторы речи. Владелец: воркер `vad`.

use std::path::Path;

use anyhow::{bail, Context};
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector as SherpaVad};

use crate::speech::VoiceDetector;

const SAMPLE_RATE: i32 = 16_000;
const SILERO_WINDOW: usize = 512;
const ENERGY_FRAME: usize = 320;

/// Silero VAD через sherpa-onnx — рабочий детектор приложения.
pub struct SileroVad {
    detector: SherpaVad,
    float_frame: Vec<f32>,
}

impl SileroVad {
    /// `model` — путь к silero_vad.onnx. `threshold` — порог вероятности речи (0..1).
    pub fn new(model: &Path, threshold: f32) -> anyhow::Result<Self> {
        if !model.is_file() {
            bail!("модель Silero VAD не найдена: {}", model.display());
        }
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            bail!("порог Silero VAD должен быть конечным числом от 0 до 1");
        }

        let mut silero = SileroVadModelConfig::default();
        silero.model = Some(model.to_string_lossy().into_owned());
        silero.threshold = threshold;
        // sherpa использует эти длительности при ведении своей очереди сегментов;
        // границы фразы и минимальную речь отдельно определяет Segmenter.
        silero.min_silence_duration = 0.02;
        silero.min_speech_duration = 0.02;
        silero.window_size = SILERO_WINDOW as i32;
        silero.max_speech_duration = 15.0;

        let config = VadModelConfig {
            silero_vad: silero,
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            provider: Some("cpu".to_owned()),
            ..VadModelConfig::default()
        };
        let detector = SherpaVad::create(&config, 16.0)
            .context("не удалось создать sherpa-onnx Silero VAD")?;

        Ok(Self {
            detector,
            float_frame: vec![0.0; SILERO_WINDOW],
        })
    }
}

impl VoiceDetector for SileroVad {
    fn frame_len(&self) -> usize {
        SILERO_WINDOW
    }

    fn is_speech(&mut self, frame: &[i16]) -> bool {
        if frame.len() != SILERO_WINDOW {
            return false;
        }

        for (dst, src) in self.float_frame.iter_mut().zip(frame) {
            *dst = *src as f32 / 32_768.0;
        }
        self.detector.accept_waveform(&self.float_frame);

        // В sherpa detected() отражает start_ != -1 сразу после AcceptWaveform:
        // это покадровый флаг, тогда как очередь содержит уже завершённые фразы.
        let detected = self.detector.detected();
        // Удаляем готовые сегменты на каждом кадре: они нужны sherpa для своих
        // примеров обработки файлов, но endpointing и аудиобуфер принадлежат нам.
        self.detector.clear();
        detected
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.detector.clear();
        self.float_frame.fill(0.0);
    }
}

/// Энергетический детектор без моделей — для тестов и как запасной вариант.
pub struct EnergyVad {
    noise_rms: f64,
}

impl EnergyVad {
    pub fn new() -> Self {
        Self { noise_rms: 80.0 }
    }
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceDetector for EnergyVad {
    fn frame_len(&self) -> usize {
        ENERGY_FRAME
    }

    fn is_speech(&mut self, frame: &[i16]) -> bool {
        if frame.is_empty() {
            return false;
        }

        let mean_square = frame
            .iter()
            .map(|sample| {
                let sample = *sample as f64;
                sample * sample
            })
            .sum::<f64>()
            / frame.len() as f64;
        let rms = mean_square.sqrt();
        // Запас относительно тихих кадров позволяет пережить обычный фон, а
        // абсолютный минимум не даёт цифровому нулю стать слишком чувствительным.
        let threshold = (self.noise_rms * 3.0).max(220.0);
        let speech = rms > threshold;

        if !speech {
            // Обновляем фон только кадрами ниже текущего порога: громкая речь не
            // поднимает оценку шума и не маскирует следующие слоги.
            self.noise_rms = (self.noise_rms * 0.95 + rms * 0.05).clamp(20.0, 4_000.0);
        }
        speech
    }

    fn reset(&mut self) {
        self.noise_rms = 80.0;
    }
}

#[cfg(test)]
mod tests {
    use super::{EnergyVad, SileroVad, ENERGY_FRAME, SILERO_WINDOW};
    use crate::speech::VoiceDetector;
    use std::{f64::consts::TAU, path::PathBuf};

    fn sine_frame(len: usize, amplitude: f64, frequency_hz: f64) -> Vec<i16> {
        (0..len)
            .map(|n| {
                let phase = TAU * frequency_hz * n as f64 / 16_000.0;
                (phase.sin() * amplitude) as i16
            })
            .collect()
    }

    fn noise_frame(len: usize, amplitude: i32, seed: &mut u32) -> Vec<i16> {
        (0..len)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                ((*seed % (2 * amplitude as u32 + 1)) as i32 - amplitude) as i16
            })
            .collect()
    }

    #[test]
    fn energy_vad_separates_synthetic_tone_noise_and_silence() {
        let mut vad = EnergyVad::new();
        assert_eq!(vad.frame_len(), ENERGY_FRAME);
        let silence = vec![0; ENERGY_FRAME];
        let tone = sine_frame(ENERGY_FRAME, 2_000.0, 440.0);
        let mut seed = 0x52A1_199D;
        let noise = noise_frame(ENERGY_FRAME, 1_500, &mut seed);

        for _ in 0..10 {
            assert!(!vad.is_speech(&silence));
        }
        assert!(vad.is_speech(&tone));
        assert!(vad.is_speech(&noise));
        assert!(!vad.is_speech(&silence));
    }

    #[test]
    fn energy_vad_tracks_quiet_background_without_losing_speech() {
        let mut vad = EnergyVad::new();
        let mut seed = 0xBEEF_1023;
        let quiet_noise = noise_frame(ENERGY_FRAME, 120, &mut seed);
        let speech = sine_frame(ENERGY_FRAME, 1_200.0, 220.0);

        for _ in 0..100 {
            assert!(!vad.is_speech(&quiet_noise));
        }
        assert!(vad.is_speech(&speech));
    }

    fn read_pcm16_mono_wav(path: &std::path::Path) -> anyhow::Result<Vec<i16>> {
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(bytes.len() >= 12, "слишком короткий WAV");
        anyhow::ensure!(
            &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
            "не RIFF/WAVE"
        );

        let mut encoding = None;
        let mut channels = None;
        let mut sample_rate = None;
        let mut bits_per_sample = None;
        let mut pcm = None;
        let mut offset = 12;
        while offset + 8 <= bytes.len() {
            let id = &bytes[offset..offset + 4];
            let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
            let start = offset + 8;
            let end = start
                .checked_add(len)
                .ok_or_else(|| anyhow::anyhow!("WAV chunk переполнен"))?;
            anyhow::ensure!(end <= bytes.len(), "WAV chunk обрезан");
            let chunk = &bytes[start..end];

            if id == b"fmt " {
                anyhow::ensure!(chunk.len() >= 16, "неполный WAV fmt chunk");
                encoding = Some(u16::from_le_bytes(chunk[0..2].try_into()?));
                channels = Some(u16::from_le_bytes(chunk[2..4].try_into()?));
                sample_rate = Some(u32::from_le_bytes(chunk[4..8].try_into()?));
                bits_per_sample = Some(u16::from_le_bytes(chunk[14..16].try_into()?));
            } else if id == b"data" {
                pcm = Some(chunk.to_vec());
            }
            offset = end + (len & 1);
        }

        anyhow::ensure!(encoding == Some(1), "WAV должен содержать PCM без сжатия");
        anyhow::ensure!(channels == Some(1), "WAV должен быть mono");
        anyhow::ensure!(
            sample_rate == Some(16_000),
            "WAV должен иметь частоту 16 кГц"
        );
        anyhow::ensure!(bits_per_sample == Some(16), "WAV должен быть 16-bit PCM");
        let pcm = pcm.ok_or_else(|| anyhow::anyhow!("WAV не содержит data chunk"))?;
        anyhow::ensure!(pcm.len() % 2 == 0, "нечётный размер PCM data");
        Ok(pcm
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect())
    }

    fn test_speech_wav() -> anyhow::Result<PathBuf> {
        if let Some(path) = std::env::var_os("REEVER_VAD_TEST_WAV") {
            return Ok(PathBuf::from(path));
        }

        let root = PathBuf::from("resources/model/test_wavs");
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                {
                    return Ok(path);
                }
            }
        }

        anyhow::bail!(
            "укажите REEVER_VAD_TEST_WAV или положите тестовую речь в resources/model/test_wavs"
        )
    }

    #[test]
    #[ignore = "требуется локальная модель Silero и реальный WAV: REEVER_VAD_TEST_WAV"]
    fn silero_detects_speech_in_real_wav() -> anyhow::Result<()> {
        let model = PathBuf::from("resources/model/silero_vad.onnx");
        let wav = test_speech_wav()?;
        let pcm = read_pcm16_mono_wav(&wav)?;
        let mut vad = SileroVad::new(&model, 0.5)?;

        assert_eq!(vad.frame_len(), SILERO_WINDOW);
        let detected_frames = pcm
            .chunks_exact(SILERO_WINDOW)
            .filter(|frame| vad.is_speech(frame))
            .count();
        assert!(
            detected_frames > 0,
            "Silero не обнаружил речь в {}",
            wav.display()
        );
        Ok(())
    }
}
