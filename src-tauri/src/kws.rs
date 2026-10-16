//! Нативный tiny DS-CNN для wake-слова «ривер».
//!
//! Архитектура и формат весов совпадают с tools/kws/train.py. Модуль пока не
//! подключён к production-коду: его использует только offline probe.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{bail, Context};

pub const SAMPLE_RATE: usize = 16_000;
pub const WINDOW_SAMPLES: usize = 400; // 25 мс
pub const HOP_SAMPLES: usize = 160; // 10 мс
pub const FFT_SIZE: usize = 512;
pub const MEL_BINS: usize = 40;
pub const CONTEXT_FRAMES: usize = 60; // 600 мс
pub const POOL_BINS: usize = 6;
const MAGIC: &[u8; 4] = b"RKW1";
const PRE_OUT: usize = 8;
const POINT_OUT: usize = 12;
const HEAD_INPUT: usize = POINT_OUT * POOL_BINS;

#[derive(Clone)]
pub struct TinyDsCnn {
    pre_weight: [f32; PRE_OUT * MEL_BINS],
    pre_bias: [f32; PRE_OUT],
    depth_weight: [f32; PRE_OUT * 5],
    depth_bias: [f32; PRE_OUT],
    point_weight: [f32; POINT_OUT * PRE_OUT],
    point_bias: [f32; POINT_OUT],
    head_weight: [f32; 2 * HEAD_INPUT],
    head_bias: [f32; 2],
}

impl TinyDsCnn {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("чтение весов {}", path.display()))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        let count = PRE_OUT * MEL_BINS
            + PRE_OUT
            + PRE_OUT * 5
            + PRE_OUT
            + POINT_OUT * PRE_OUT
            + POINT_OUT
            + 2 * HEAD_INPUT
            + 2;
        let expected = MAGIC.len() + count * 4;
        if bytes.len() != expected || bytes.get(..4) != Some(MAGIC.as_slice()) {
            bail!(
                "неверный формат RKW1 или размер весов: {} (ожидалось {expected})",
                bytes.len()
            );
        }
        let mut offset = MAGIC.len();
        fn read<const N: usize>(bytes: &[u8], offset: &mut usize) -> [f32; N] {
            let mut out = [0.0; N];
            for value in &mut out {
                let raw: [u8; 4] = bytes[*offset..*offset + 4].try_into().unwrap();
                *value = f32::from_le_bytes(raw);
                *offset += 4;
            }
            out
        }
        let model = Self {
            pre_weight: read(bytes, &mut offset),
            pre_bias: read(bytes, &mut offset),
            depth_weight: read(bytes, &mut offset),
            depth_bias: read(bytes, &mut offset),
            point_weight: read(bytes, &mut offset),
            point_bias: read(bytes, &mut offset),
            head_weight: read(bytes, &mut offset),
            head_bias: read(bytes, &mut offset),
        };
        if model
            .pre_weight
            .iter()
            .chain(model.pre_bias.iter())
            .chain(model.depth_weight.iter())
            .chain(model.depth_bias.iter())
            .chain(model.point_weight.iter())
            .chain(model.point_bias.iter())
            .chain(model.head_weight.iter())
            .chain(model.head_bias.iter())
            .any(|x| !x.is_finite())
        {
            bail!("веса KWS содержат NaN или бесконечность");
        }
        Ok(model)
    }

    /// Вход — 60 кадров log-mel, расположенных от старого к новому.
    pub fn score(&self, frames: &[[f32; MEL_BINS]]) -> anyhow::Result<f32> {
        if frames.len() != CONTEXT_FRAMES {
            bail!(
                "модели нужны {CONTEXT_FRAMES} кадров, получено {}",
                frames.len()
            );
        }
        let mut pre = [[0.0f32; CONTEXT_FRAMES]; PRE_OUT];
        for channel in 0..PRE_OUT {
            for time in 0..CONTEXT_FRAMES {
                let mut value = self.pre_bias[channel];
                for mel in 0..MEL_BINS {
                    value += self.pre_weight[channel * MEL_BINS + mel] * frames[time][mel];
                }
                pre[channel][time] = value.max(0.0);
            }
        }

        let mut depth = [[0.0f32; CONTEXT_FRAMES]; PRE_OUT];
        for channel in 0..PRE_OUT {
            for time in 0..CONTEXT_FRAMES {
                let mut value = self.depth_bias[channel];
                for k in 0..5 {
                    let source_time = time as isize + k as isize - 2;
                    if (0..CONTEXT_FRAMES as isize).contains(&source_time) {
                        value +=
                            self.depth_weight[channel * 5 + k] * pre[channel][source_time as usize];
                    }
                }
                depth[channel][time] = value.max(0.0);
            }
        }

        let mut point = [[0.0f32; CONTEXT_FRAMES]; POINT_OUT];
        for channel in 0..POINT_OUT {
            for time in 0..CONTEXT_FRAMES {
                let mut value = self.point_bias[channel];
                for input in 0..PRE_OUT {
                    value += self.point_weight[channel * PRE_OUT + input] * depth[input][time];
                }
                point[channel][time] = value.max(0.0);
            }
        }

        let mut pooled = [0.0f32; HEAD_INPUT];
        let frames_per_bin = CONTEXT_FRAMES / POOL_BINS;
        for channel in 0..POINT_OUT {
            for bin in 0..POOL_BINS {
                let begin = bin * frames_per_bin;
                let sum: f32 = (begin..begin + frames_per_bin)
                    .map(|t| point[channel][t])
                    .sum();
                pooled[channel * POOL_BINS + bin] = sum / frames_per_bin as f32;
            }
        }

        let mut logits = [0.0f32; 2];
        for class in 0..2 {
            logits[class] = self.head_bias[class]
                + (0..HEAD_INPUT)
                    .map(|i| self.head_weight[class * HEAD_INPUT + i] * pooled[i])
                    .sum::<f32>();
        }
        let difference = (logits[0] - logits[1]).clamp(-80.0, 80.0);
        Ok(1.0 / (1.0 + difference.exp()))
    }
}

struct MelFrontend {
    window: [f32; WINDOW_SAMPLES],
    filters: [[f32; FFT_SIZE / 2 + 1]; MEL_BINS],
}

impl MelFrontend {
    fn new() -> Self {
        fn hz_to_mel(hz: f64) -> f64 {
            2595.0 * (1.0 + hz / 700.0).log10()
        }
        fn mel_to_hz(mel: f64) -> f64 {
            700.0 * (10.0f64.powf(mel / 2595.0) - 1.0)
        }
        let low = hz_to_mel(20.0);
        let high = hz_to_mel(7600.0);
        let edges: Vec<f64> = (0..MEL_BINS + 2)
            .map(|i| mel_to_hz(low + (high - low) * i as f64 / (MEL_BINS + 1) as f64))
            .collect();
        let mut filters = [[0.0; FFT_SIZE / 2 + 1]; MEL_BINS];
        for mel in 0..MEL_BINS {
            let (left, center, right) = (edges[mel], edges[mel + 1], edges[mel + 2]);
            for bin in 0..=FFT_SIZE / 2 {
                let hz = bin as f64 * SAMPLE_RATE as f64 / FFT_SIZE as f64;
                let value = ((hz - left) / (center - left)).min((right - hz) / (right - center));
                filters[mel][bin] = value.max(0.0) as f32;
            }
        }
        let window = std::array::from_fn(|i| {
            (0.5 - 0.5
                * (2.0 * std::f32::consts::PI * i as f32 / (WINDOW_SAMPLES - 1) as f32).cos())
                as f32
        });
        Self { window, filters }
    }

    fn compute(&self, samples: &[f32; WINDOW_SAMPLES]) -> [f32; MEL_BINS] {
        let mut real = [0.0f32; FFT_SIZE];
        let mut imag = [0.0f32; FFT_SIZE];
        for i in 0..WINDOW_SAMPLES {
            real[i] = samples[i] * self.window[i];
        }
        fft(&mut real, &mut imag);
        let mut out = [0.0; MEL_BINS];
        for mel in 0..MEL_BINS {
            let mut energy = 0.0;
            for bin in 0..=FFT_SIZE / 2 {
                energy += self.filters[mel][bin] * (real[bin] * real[bin] + imag[bin] * imag[bin]);
            }
            out[mel] = (energy + 1e-10).ln();
        }
        out
    }
}

fn fft(real: &mut [f32; FFT_SIZE], imag: &mut [f32; FFT_SIZE]) {
    let n = FFT_SIZE;
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            real.swap(i, j);
            imag.swap(i, j);
        }
    }
    let mut length = 2;
    while length <= n {
        let angle = -2.0 * std::f32::consts::PI / length as f32;
        let step_re = angle.cos();
        let step_im = angle.sin();
        for start in (0..n).step_by(length) {
            let mut twiddle_re = 1.0;
            let mut twiddle_im = 0.0;
            for k in 0..length / 2 {
                let even = start + k;
                let odd = even + length / 2;
                let odd_re = real[odd] * twiddle_re - imag[odd] * twiddle_im;
                let odd_im = real[odd] * twiddle_im + imag[odd] * twiddle_re;
                real[odd] = real[even] - odd_re;
                imag[odd] = imag[even] - odd_im;
                real[even] += odd_re;
                imag[even] += odd_im;
                let next_re = twiddle_re * step_re - twiddle_im * step_im;
                twiddle_im = twiddle_re * step_im + twiddle_im * step_re;
                twiddle_re = next_re;
            }
        }
        length *= 2;
    }
}

/// Потоковый wrapper: новые аудиосэмплы -> новые log-mel кадры -> score.
pub struct StreamingKws {
    model: TinyDsCnn,
    frontend: MelFrontend,
    samples: VecDeque<f32>,
    frames: VecDeque<[f32; MEL_BINS]>,
    scores: VecDeque<f32>,
    since_frame: usize,
    threshold: f32,
    fired: bool,
    quiet_frames: usize,
}

impl StreamingKws {
    pub fn new(model: TinyDsCnn, threshold: f32) -> Self {
        Self {
            model,
            frontend: MelFrontend::new(),
            samples: VecDeque::with_capacity(WINDOW_SAMPLES + HOP_SAMPLES),
            frames: VecDeque::with_capacity(CONTEXT_FRAMES),
            scores: VecDeque::with_capacity(3),
            since_frame: 0,
            threshold: threshold.clamp(0.01, 0.99),
            fired: false,
            quiet_frames: 0,
        }
    }

    /// Возвращает Some(score) при подтверждённом 2-из-3 пересечении порога.
    /// После срабатывания новый hit разрешается после 20 кадров ниже 60% порога.
    pub fn feed(&mut self, samples: &[f32]) -> Option<f32> {
        self.process(samples, false).1
    }

    /// Вариант для offline probe: возвращает score каждого нового 10-мс кадра.
    pub fn feed_scores(&mut self, samples: &[f32]) -> (Vec<f32>, Option<f32>) {
        self.process(samples, true)
    }

    fn process(&mut self, samples: &[f32], collect_scores: bool) -> (Vec<f32>, Option<f32>) {
        let mut scores_out = Vec::new();
        let mut event = None;
        for &sample in samples {
            self.samples.push_back(sample.clamp(-1.0, 1.0));
            self.since_frame += 1;
            if self.samples.len() < WINDOW_SAMPLES || self.since_frame < HOP_SAMPLES {
                continue;
            }
            let frame: [f32; WINDOW_SAMPLES] = std::array::from_fn(|i| self.samples[i]);
            self.frames.push_back(self.frontend.compute(&frame));
            for _ in 0..HOP_SAMPLES {
                self.samples.pop_front();
            }
            self.since_frame = 0;
            if self.frames.len() > CONTEXT_FRAMES {
                self.frames.pop_front();
            }
            if self.frames.len() == CONTEXT_FRAMES {
                let stack: [[f32; MEL_BINS]; CONTEXT_FRAMES] =
                    std::array::from_fn(|index| self.frames[index]);
                if let Ok(score) = self.model.score(&stack) {
                    if collect_scores {
                        scores_out.push(score);
                    }
                    self.scores.push_back(score);
                    if self.scores.len() > 3 {
                        self.scores.pop_front();
                    }
                    if self.fired {
                        if score < self.threshold * 0.6 {
                            self.quiet_frames += 1;
                            if self.quiet_frames >= 20 {
                                self.fired = false;
                                self.quiet_frames = 0;
                                self.scores.clear();
                            }
                        } else {
                            self.quiet_frames = 0;
                        }
                    } else if self.scores.len() == 3
                        && self.scores.iter().filter(|&&s| s >= self.threshold).count() >= 2
                    {
                        self.fired = true;
                        event = Some(score);
                    }
                }
            }
        }
        (scores_out, event)
    }

    pub fn from_path(path: &Path, threshold: f32) -> anyhow::Result<Self> {
        Ok(Self::new(TinyDsCnn::load(path)?, threshold))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_model_header_and_shape_are_checked() {
        let count = PRE_OUT * MEL_BINS
            + PRE_OUT
            + PRE_OUT * 5
            + PRE_OUT
            + POINT_OUT * PRE_OUT
            + POINT_OUT
            + 2 * HEAD_INPUT
            + 2;
        let mut bytes = vec![0u8; 4 + count * 4];
        bytes[..4].copy_from_slice(MAGIC);
        assert!(TinyDsCnn::from_bytes(&bytes).is_ok());
        bytes[0] = b'X';
        assert!(TinyDsCnn::from_bytes(&bytes).is_err());
    }

    #[test]
    fn mel_frontend_returns_finite_silence_features() {
        let feature = MelFrontend::new().compute(&[0.0; WINDOW_SAMPLES]);
        assert!(feature.iter().all(|value| value.is_finite()));
        assert!(feature
            .iter()
            .all(|value| (*value - (1e-10f32).ln()).abs() < 1e-5));
    }

    #[test]
    fn forward_rejects_wrong_context_and_scores_valid_context() {
        let count = PRE_OUT * MEL_BINS
            + PRE_OUT
            + PRE_OUT * 5
            + PRE_OUT
            + POINT_OUT * PRE_OUT
            + POINT_OUT
            + 2 * HEAD_INPUT
            + 2;
        let mut bytes = vec![0u8; 4 + count * 4];
        bytes[..4].copy_from_slice(MAGIC);
        let model = TinyDsCnn::from_bytes(&bytes).unwrap();
        assert!(model.score(&[[0.0; MEL_BINS]; CONTEXT_FRAMES - 1]).is_err());
        assert!((model.score(&[[0.0; MEL_BINS]; CONTEXT_FRAMES]).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn streaming_window_count_and_hysteresis_ignore_chunk_boundaries() {
        let count = PRE_OUT * MEL_BINS
            + PRE_OUT
            + PRE_OUT * 5
            + PRE_OUT
            + POINT_OUT * PRE_OUT
            + POINT_OUT
            + 2 * HEAD_INPUT
            + 2;
        let mut bytes = vec![0u8; 4 + count * 4];
        bytes[..4].copy_from_slice(MAGIC);
        let model = TinyDsCnn::from_bytes(&bytes).unwrap();
        let audio = vec![0.0f32; SAMPLE_RATE * 4 / 5];
        let mut whole = StreamingKws::new(model.clone(), 0.5);
        let (whole_scores, whole_event) = whole.feed_scores(&audio);
        let mut chunked = StreamingKws::new(model, 0.5);
        let mut chunked_scores = Vec::new();
        let mut chunked_event = None;
        for chunk in audio.chunks(233) {
            let (scores, event) = chunked.feed_scores(chunk);
            chunked_scores.extend(scores);
            chunked_event = chunked_event.or(event);
        }
        assert_eq!(whole_scores.len(), 19);
        assert_eq!(whole_scores.len(), chunked_scores.len());
        assert!(whole_event.is_some());
        assert!(chunked_event.is_some());
        for (a, b) in whole_scores.iter().zip(chunked_scores) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
