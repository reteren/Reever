//! Нативные детекторы wake-слова «ривер».
//!
//! - RKW1 — tiny DS-CNN по окну в 60 кадров (tools/kws/train.py), только для offline probe;
//! - RKW2 — причинная DS-TCN многоязычного детектора (tools/kws/train_multilang.py):
//!   каждый кадр log-mel считается один раз, прошлое хранится в кольцах блоков.

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
const PRE_OUT: usize = 16;
const POINT_OUT: usize = 24;
const HEAD_INPUT: usize = POINT_OUT * POOL_BINS;
const SCORE_WINDOW: usize = 2;
const SCORE_QUORUM: usize = 2;
const RELEASE_FRAMES: usize = 10;
const RELEASE_RATIO: f32 = 0.6;

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
            std::fs::read(path).with_context(|| format!("reading weights {}", path.display()))?;
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
                "bad RKW1 format or weight size: {} (expected {expected})",
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
            bail!("KWS weights contain NaN or infinity");
        }
        Ok(model)
    }

    /// Вход — 60 кадров log-mel, расположенных от старого к новому.
    pub fn score(&self, frames: &[[f32; MEL_BINS]]) -> anyhow::Result<f32> {
        if frames.len() != CONTEXT_FRAMES {
            bail!(
                "the model needs {CONTEXT_FRAMES} frames, got {}",
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
    gate: ScoreGate,
    since_frame: usize,
}

struct ScoreGate {
    scores: VecDeque<f32>,
    threshold: f32,
    fired: bool,
    quiet_frames: usize,
}

impl ScoreGate {
    fn new(threshold: f32) -> Self {
        Self {
            scores: VecDeque::with_capacity(SCORE_WINDOW),
            threshold: threshold.clamp(0.01, 0.99),
            fired: false,
            quiet_frames: 0,
        }
    }

    fn push(&mut self, score: f32) -> Option<f32> {
        self.scores.push_back(score);
        if self.scores.len() > SCORE_WINDOW {
            self.scores.pop_front();
        }
        if self.fired {
            if score < self.threshold * RELEASE_RATIO {
                self.quiet_frames += 1;
                if self.quiet_frames >= RELEASE_FRAMES {
                    self.fired = false;
                    self.quiet_frames = 0;
                    self.scores.clear();
                }
            } else {
                self.quiet_frames = 0;
            }
            return None;
        }
        if self.scores.len() == SCORE_WINDOW
            && self
                .scores
                .iter()
                .filter(|&&value| value >= self.threshold)
                .count()
                >= SCORE_QUORUM
        {
            self.fired = true;
            return Some(score);
        }
        None
    }
}

impl StreamingKws {
    pub fn new(model: TinyDsCnn, threshold: f32) -> Self {
        Self {
            model,
            frontend: MelFrontend::new(),
            samples: VecDeque::with_capacity(WINDOW_SAMPLES + HOP_SAMPLES),
            frames: VecDeque::with_capacity(CONTEXT_FRAMES),
            gate: ScoreGate::new(threshold),
            since_frame: 0,
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
                    if let Some(hit) = self.gate.push(score) {
                        event = Some(hit);
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

// ── RKW2: причинная DS-TCN многоязычного детектора (tools/kws/train_multilang.py) ──

const TCN_MAGIC: &[u8; 4] = b"RKW2";
/// Повторное срабатывание — после стольких кадров ниже 0,6 порога (как `events` в обучении).
const TCN_RELEASE_FRAMES: usize = 30;

#[derive(Clone)]
struct TcnBlock {
    dilation: usize,
    depth_weight: Vec<f32>,
    depth_bias: Vec<f32>,
    point_weight: Vec<f32>,
    point_bias: Vec<f32>,
}

/// Сеть смотрит только в прошлое: на каждый кадр log-mel — одна оценка, и прошлые кадры
/// не пересчитываются. BatchNorm вшита в свёртки при экспорте.
#[derive(Clone)]
pub struct TcnModel {
    channels: usize,
    kernel: usize,
    pre_weight: Vec<f32>,
    pre_bias: Vec<f32>,
    blocks: Vec<TcnBlock>,
    head_weight: Vec<f32>,
    head_bias: f32,
}

impl TcnModel {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading weights {}", path.display()))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.get(..4) != Some(TCN_MAGIC.as_slice()) {
            bail!("not an RKW2 model");
        }
        let mut offset = 4;
        let mut word = || -> anyhow::Result<usize> {
            let raw: [u8; 4] = bytes
                .get(offset..offset + 4)
                .context("RKW2 is truncated")?
                .try_into()?;
            offset += 4;
            Ok(u32::from_le_bytes(raw) as usize)
        };
        let (mels, channels, kernel, count) = (word()?, word()?, word()?, word()?);
        if mels != MEL_BINS
            || !(1..=512).contains(&channels)
            || !(1..=16).contains(&kernel)
            || !(1..=32).contains(&count)
        {
            bail!("unsupported RKW2 shape: mels {mels}, channels {channels}, kernel {kernel}, blocks {count}");
        }
        let dilations = (0..count).map(|_| word()).collect::<anyhow::Result<Vec<_>>>()?;
        if dilations.iter().any(|d| !(1..=64).contains(d)) {
            bail!("unsupported RKW2 dilation");
        }
        let floats = &bytes[offset..];
        let expected = channels * MEL_BINS
            + channels
            + count * (channels * kernel + channels + channels * channels + channels)
            + channels
            + 1;
        if floats.len() != expected * 4 {
            bail!("RKW2 weight size {} (expected {})", floats.len(), expected * 4);
        }
        let all: Vec<f32> = floats
            .chunks_exact(4)
            .map(|raw| f32::from_le_bytes(raw.try_into().unwrap()))
            .collect();
        if all.iter().any(|value| !value.is_finite()) {
            bail!("KWS weights contain NaN or infinity");
        }
        let mut rest = all.as_slice();
        let mut take = |n: usize| -> Vec<f32> {
            let (head, tail) = rest.split_at(n);
            rest = tail;
            head.to_vec()
        };
        let pre_weight = take(channels * MEL_BINS);
        let pre_bias = take(channels);
        let blocks = dilations
            .iter()
            .map(|&dilation| TcnBlock {
                dilation,
                depth_weight: take(channels * kernel),
                depth_bias: take(channels),
                point_weight: take(channels * channels),
                point_bias: take(channels),
            })
            .collect();
        let head_weight = take(channels);
        let head_bias = take(1)[0];
        Ok(Self { channels, kernel, pre_weight, pre_bias, blocks, head_weight, head_bias })
    }

    /// Кадров прошлого, которые видит сеть.
    pub fn receptive_field(&self) -> usize {
        1 + (self.kernel - 1) * self.blocks.iter().map(|block| block.dilation).sum::<usize>()
    }

    /// Голова сети: веса по каналам и сдвиг (логит = сдвиг + Σ вес·признак).
    pub fn head(&self) -> (&[f32], f32) {
        (&self.head_weight, self.head_bias)
    }

    /// Та же сеть с другой головой — подстройка под голос (voice_adapt.rs).
    pub fn with_head(&self, weight: Vec<f32>, bias: f32) -> Self {
        assert_eq!(weight.len(), self.channels);
        Self { head_weight: weight, head_bias: bias, ..self.clone() }
    }

    /// Признаки перед головой на каждый кадр звука (сэмплы 16 кГц, с начала потока).
    pub fn features(&self, samples: &[f32]) -> Vec<Vec<f32>> {
        let frontend = MelFrontend::new();
        let mut state = TcnState::new(self);
        let mut out = Vec::new();
        let mut start = 0;
        while start + WINDOW_SAMPLES <= samples.len() {
            let frame: [f32; WINDOW_SAMPLES] = std::array::from_fn(|i| samples[start + i].clamp(-1.0, 1.0));
            out.push(state.forward(self, &frontend.compute(&frame)).to_vec());
            start += HOP_SAMPLES;
        }
        out
    }

    /// RKW2 — тот же формат, что пишет tools/kws/train_multilang.py (`export`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = TCN_MAGIC.to_vec();
        let header = [MEL_BINS, self.channels, self.kernel, self.blocks.len()];
        let dilations = self.blocks.iter().map(|block| block.dilation);
        for word in header.into_iter().chain(dilations) {
            out.extend_from_slice(&(word as u32).to_le_bytes());
        }
        let blocks = self.blocks.iter().flat_map(|block| {
            [&block.depth_weight, &block.depth_bias, &block.point_weight, &block.point_bias]
        });
        let tensors = [&self.pre_weight, &self.pre_bias].into_iter().chain(blocks).chain([&self.head_weight]);
        for value in tensors.flatten().chain([&self.head_bias]) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }
}

/// Состояние сети в потоке: на каждый блок — кольцо его входов за (ядро − 1)·дилатация кадров.
struct TcnState {
    history: Vec<Vec<f32>>,
    lengths: Vec<usize>,
    cursor: usize,
    x: Vec<f32>,
    y: Vec<f32>,
}

impl TcnState {
    fn new(model: &TcnModel) -> Self {
        let lengths: Vec<usize> = model
            .blocks
            .iter()
            .map(|block| (model.kernel - 1) * block.dilation + 1)
            .collect();
        Self {
            history: lengths.iter().map(|&len| vec![0.0; len * model.channels]).collect(),
            lengths,
            cursor: 0,
            x: vec![0.0; model.channels],
            y: vec![0.0; model.channels],
        }
    }

    /// Оценка 0..1 для нового кадра.
    fn step(&mut self, model: &TcnModel, mel: &[f32; MEL_BINS]) -> f32 {
        self.forward(model, mel);
        let logit = model.head_bias + model.head_weight.iter().zip(&self.x).map(|(w, v)| w * v).sum::<f32>();
        1.0 / (1.0 + (-logit.clamp(-80.0, 80.0)).exp())
    }

    /// Новый кадр через все блоки; признаки перед головой.
    fn forward(&mut self, model: &TcnModel, mel: &[f32; MEL_BINS]) -> &[f32] {
        let c = model.channels;
        for out in 0..c {
            let row = &model.pre_weight[out * MEL_BINS..(out + 1) * MEL_BINS];
            let value = model.pre_bias[out] + row.iter().zip(mel).map(|(w, v)| w * v).sum::<f32>();
            self.x[out] = value.max(0.0);
        }
        for (index, block) in model.blocks.iter().enumerate() {
            let len = self.lengths[index];
            let history = &mut self.history[index];
            let slot = self.cursor % len;
            history[slot * c..(slot + 1) * c].copy_from_slice(&self.x);
            // Свёртка с паддингом слева: выход в t = Σ_k w[k]·x[t − (K − 1 − k)·d].
            for channel in 0..c {
                let mut value = block.depth_bias[channel];
                for k in 0..model.kernel {
                    let back = (model.kernel - 1 - k) * block.dilation;
                    let source = (slot + len - back) % len;
                    value += block.depth_weight[channel * model.kernel + k] * history[source * c + channel];
                }
                self.y[channel] = value.max(0.0);
            }
            for out in 0..c {
                let row = &block.point_weight[out * c..(out + 1) * c];
                let value = block.point_bias[out] + row.iter().zip(&self.y).map(|(w, v)| w * v).sum::<f32>();
                self.x[out] = (self.x[out] + value).max(0.0);
            }
        }
        self.cursor = self.cursor.wrapping_add(1);
        &self.x
    }
}

/// Многоязычный детектор в потоке: сэмплы 16 кГц → кадры log-mel → оценка на каждые 10 мс.
/// Срабатывание — два кадра подряд выше порога; повторное — после тишины в оценках.
pub struct StreamingTcn {
    model: TcnModel,
    state: TcnState,
    frontend: MelFrontend,
    samples: VecDeque<f32>,
    threshold: f32,
    run: usize,
    fired: bool,
    quiet: usize,
}

impl StreamingTcn {
    pub fn new(model: TcnModel, threshold: f32) -> Self {
        Self {
            state: TcnState::new(&model),
            model,
            frontend: MelFrontend::new(),
            samples: VecDeque::with_capacity(WINDOW_SAMPLES + HOP_SAMPLES),
            threshold: threshold.clamp(0.01, 0.99),
            run: 0,
            fired: false,
            quiet: 0,
        }
    }

    pub fn from_path(path: &Path, threshold: f32) -> anyhow::Result<Self> {
        Ok(Self::new(TcnModel::load(path)?, threshold))
    }

    /// `Some(score)` — услышано wake-слово.
    pub fn feed(&mut self, samples: &[f32]) -> Option<f32> {
        self.process(samples, None)
    }

    /// Для замеров: оценка каждого нового кадра.
    pub fn feed_scores(&mut self, samples: &[f32], scores: &mut Vec<f32>) -> Option<f32> {
        self.process(samples, Some(scores))
    }

    /// Забыть прошлое (новая запись в замерах).
    pub fn reset(&mut self) {
        self.state = TcnState::new(&self.model);
        self.samples.clear();
        self.run = 0;
        self.fired = false;
        self.quiet = 0;
    }

    fn process(&mut self, samples: &[f32], mut scores: Option<&mut Vec<f32>>) -> Option<f32> {
        let mut event = None;
        for &sample in samples {
            self.samples.push_back(sample.clamp(-1.0, 1.0));
            // Первый кадр — как только набралось окно, дальше — каждые HOP сэмплов.
            if self.samples.len() < WINDOW_SAMPLES {
                continue;
            }
            let frame: [f32; WINDOW_SAMPLES] = std::array::from_fn(|i| self.samples[i]);
            let mel = self.frontend.compute(&frame);
            for _ in 0..HOP_SAMPLES {
                self.samples.pop_front();
            }
            let score = self.state.step(&self.model, &mel);
            if let Some(scores) = scores.as_deref_mut() {
                scores.push(score);
            }
            if self.fired {
                self.quiet = if score < self.threshold * RELEASE_RATIO { self.quiet + 1 } else { 0 };
                if self.quiet >= TCN_RELEASE_FRAMES {
                    self.fired = false;
                    self.quiet = 0;
                }
                continue;
            }
            self.run = if score >= self.threshold { self.run + 1 } else { 0 };
            if self.run >= 2 {
                self.fired = true;
                self.run = 0;
                event = Some(score);
            }
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcn_weights_round_trip_and_features_match_streaming_scores() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin");
        let bytes = std::fs::read(&path).unwrap();
        let model = TcnModel::from_bytes(&bytes).unwrap();
        assert_eq!(model.to_bytes(), bytes);
        // Оценка по признакам и голове — та же, что считает поток.
        let audio: Vec<f32> = (0..16_000).map(|i| ((i as f32) * 0.05).sin() * 0.3).collect();
        let mut scores = Vec::new();
        StreamingTcn::new(model.clone(), 0.9).feed_scores(&audio, &mut scores);
        let features = model.features(&audio);
        assert_eq!(features.len(), scores.len());
        let (weight, bias) = model.head();
        for (frame, score) in features.iter().zip(&scores) {
            let logit = bias + weight.iter().zip(frame).map(|(w, v)| w * v).sum::<f32>();
            assert!((1.0 / (1.0 + (-logit).exp()) - score).abs() < 1e-5);
        }
    }

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
    fn score_gate_requires_two_consecutive_hits_and_ten_quiet_frames_to_rearm() {
        let mut gate = ScoreGate::new(0.5);
        assert_eq!(gate.push(0.9), None);
        assert_eq!(gate.push(0.9), Some(0.9));
        for _ in 0..9 {
            assert_eq!(gate.push(0.0), None);
        }
        assert_eq!(gate.push(0.9), None);
        for _ in 0..10 {
            assert_eq!(gate.push(0.0), None);
        }
        assert_eq!(gate.push(0.9), None);
        assert_eq!(gate.push(0.9), Some(0.9));
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

    /// RKW2 из одних нулей: все оценки 0,5, кадры идут каждые 160 сэмплов независимо от
    /// нарезки на чанки, неверный размер весов не загружается.
    #[test]
    fn rkw2_streams_frames_independently_of_chunking() {
        let (channels, kernel, dilations) = (4usize, 3usize, [1u32, 2]);
        let mut bytes = b"RKW2".to_vec();
        for value in [MEL_BINS as u32, channels as u32, kernel as u32, dilations.len() as u32] {
            bytes.extend(value.to_le_bytes());
        }
        for d in dilations {
            bytes.extend(d.to_le_bytes());
        }
        let floats = channels * MEL_BINS + channels
            + dilations.len() * (channels * kernel + channels + channels * channels + channels)
            + channels + 1;
        bytes.extend(vec![0u8; floats * 4]);
        let model = TcnModel::from_bytes(&bytes).unwrap();
        assert_eq!(model.receptive_field(), 1 + 2 * 3);
        assert!(TcnModel::from_bytes(&bytes[..bytes.len() - 4]).is_err());

        let audio = vec![0.0f32; 4000];
        let mut whole = StreamingTcn::new(TcnModel::from_bytes(&bytes).unwrap(), 0.9);
        let mut whole_scores = Vec::new();
        whole.feed_scores(&audio, &mut whole_scores);
        let mut chunked = StreamingTcn::new(model, 0.9);
        let mut chunked_scores = Vec::new();
        for chunk in audio.chunks(137) {
            chunked.feed_scores(chunk, &mut chunked_scores);
        }
        assert_eq!(whole_scores.len(), (4000 - WINDOW_SAMPLES) / HOP_SAMPLES + 1);
        assert_eq!(whole_scores, chunked_scores);
        assert!(whole_scores.iter().all(|score| (score - 0.5).abs() < 1e-6));
    }
}
