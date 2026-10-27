//! Подстройка детектора «ривер» под голос пользователя (Настройки → «Подстроить под мой
//! голос»). Человек говорит «ривер» раз десять с паузами; голова сети (64 веса и сдвиг)
//! дообучается на этих словах прямо здесь, за доли секунды, с притяжением к исходной голове.
//! Остальная сеть не меняется. Опыт и замеры — tools/kws/adapt_voice.py,
//! docs/research/wake-detector.md: по 32 словам второго диктора — как полное переобучение.
//!
//! Чтобы голова не начала срабатывать на всё подряд, отрицательные кадры — запись вне слов
//! (комната и голос человека) и банк признаков чужой речи и шума (resources/wake/*.bank,
//! tools/kws/make_bank.py). Из нескольких сил притяжения берётся самая слабая, при которой
//! ложных на банке не больше чем вдвое против исходной головы; не помогло — подстройки нет.

use std::path::Path;

use anyhow::{bail, Context};

use crate::kws::{TcnModel, HOP_SAMPLES};
use crate::wake::THRESHOLD;

/// Кто услышал «ривер» первым: детектор или большая модель, когда детектор промолчал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeSource {
    Kws,
    AsrOnly,
}

/// Подтверждённое большой моделью «ривер»: само слово с запасом ~0,25 с до и 0,3 с после,
/// PCM mono 16 кГц.
pub struct WakeClip {
    pub samples: Vec<i16>,
    pub source: WakeSource,
}

// ── Тихая подстройка ────────────────────────────────────────────────────
// Первые PASSIVE_GOAL подтверждённых «ривер» копятся в `<папка настроек>/voice/clips.pcm`
// (через полсекунды тишины, как в записи из Настроек) и на шагах PASSIVE_STEPS доучивают
// голову сети на всём накопленном вместе с записью «скажите „ривер“», если она была. Самые
// ценные — AsrOnly: их детектор пропустил. На последнем шаге записи стираются; остаётся
// подстроенная голова. Счётчик — в `passive.json`.

/// Сколько обращений слушать.
pub const PASSIVE_GOAL: u32 = 40;
const PASSIVE_STEPS: [u32; 3] = [10, 20, 40];
const GAP: usize = RATE / 2;

/// Обучить и сохранить подстройку по записи (склейке слов через тишину).
pub type Trainer = Box<dyn Fn(&[f32]) -> Result<Report, String> + Send>;

static PASSIVE_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PASSIVE: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<WakeClip>>> =
    std::sync::OnceLock::new();

/// Сколько обращений уже собрано и закончена ли тихая подстройка.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PassiveState {
    pub collected: u32,
    #[serde(default)]
    pub sources: [u32; 2],
}

impl PassiveState {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("passive.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        if let Ok(text) = serde_json::to_string(self) {
            let _ = std::fs::write(dir.join("passive.json"), text);
        }
    }

    pub fn done(&self) -> bool {
        self.collected >= PASSIVE_GOAL
    }
}

/// Запустить поток тихой подстройки (один раз на процесс). `dir` — `<папка настроек>/voice`.
pub fn start_passive(dir: std::path::PathBuf, train: Trainer) {
    let (tx, rx) = std::sync::mpsc::channel::<WakeClip>();
    if PASSIVE.set(std::sync::Mutex::new(tx)).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("reever-voice-adapt".into())
        .spawn(move || {
            for clip in rx {
                passive_step(&dir, clip, &train);
            }
        });
}

/// Настройка «подстраиваться под мой голос»; выключена — новые обращения не сохраняются.
pub fn set_passive_enabled(on: bool) {
    PASSIVE_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Отдать подтверждённое обращение потоку подстройки; не ждёт и ничего не делает, когда
/// подстройка выключена или уже закончена.
pub fn offer_wake_clip(clip: WakeClip) {
    if !PASSIVE_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(tx) = PASSIVE.get().and_then(|tx| tx.lock().ok()) {
        let _ = tx.send(clip);
    }
}

/// Запись «скажите „ривер“» из Настроек или первого запуска — пусть тихая подстройка
/// учится и на ней. Ничего не пишет, если подстройка выключена или закончена.
pub fn save_enrollment(dir: &Path, pcm: &[f32]) {
    if !PASSIVE_ON.load(std::sync::atomic::Ordering::Relaxed) || PassiveState::load(dir).done() {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let bytes: Vec<u8> = pcm
        .iter()
        .flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    let _ = std::fs::write(dir.join("enrollment.pcm"), bytes);
}

/// Начать тихую подстройку заново: стереть записи и счётчик.
pub fn reset_passive(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn passive_step(dir: &Path, clip: WakeClip, train: &Trainer) {
    let mut state = PassiveState::load(dir);
    if state.done() || !PASSIVE_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let mut bytes = Vec::with_capacity((clip.samples.len() + GAP) * 2);
    bytes.extend(std::iter::repeat_n(0u8, GAP * 2));
    bytes.extend(clip.samples.iter().flat_map(|s| s.to_le_bytes()));
    use std::io::Write;
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("clips.pcm"))
        .and_then(|mut file| file.write_all(&bytes));
    if appended.is_err() {
        return;
    }
    state.collected += 1;
    state.sources[usize::from(clip.source == WakeSource::AsrOnly)] += 1;
    state.save(dir);
    if !PASSIVE_STEPS.contains(&state.collected) {
        return;
    }
    let read = |name: &str| -> Vec<f32> {
        std::fs::read(dir.join(name))
            .unwrap_or_default()
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect()
    };
    let mut pcm = read("enrollment.pcm");
    pcm.extend(std::iter::repeat_n(0.0, GAP));
    pcm.extend(read("clips.pcm"));
    let _ = train(&pcm);
    pcm.fill(0.0);
    if state.done() {
        let _ = std::fs::remove_file(dir.join("clips.pcm"));
        let _ = std::fs::remove_file(dir.join("enrollment.pcm"));
    }
}

const RATE: usize = 16_000;
/// Дольше запись не копится (настройки забыли закрыть).
pub const MAX_RECORD: usize = 60 * RATE;
const FRAME: usize = HOP_SAMPLES;
const WINDOW: usize = 400;
/// Слово — кусок громче порога длиной 0,2–1,6 с; паузы короче 200 мс внутри слова.
const MIN_WORD_FRAMES: usize = 20;
const MAX_WORD_FRAMES: usize = 160;
const JOIN_FRAMES: usize = 20;
/// Сколько слов нужно, чтобы подстройка имела смысл.
pub const MIN_WORDS: usize = 5;
/// Доля слова в потере (adapt_voice.py --pos-weight): меньше — осторожнее.
const POS_WEIGHT: f32 = 0.1;
/// Силы притяжения к исходной голове, от слабой к сильной.
const L2_STEPS: [f32; 4] = [1.0, 3.0, 10.0, 30.0];
const ITERATIONS: usize = 300;
/// Искажённых копий записи: громкость и шум.
const COPIES: usize = 3;

/// Куски речи в записи, сэмплы [начало, конец).
pub fn words(pcm: &[f32]) -> Vec<(usize, usize)> {
    let db: Vec<f32> = pcm
        .chunks_exact(FRAME)
        .map(|frame| {
            10.0 * (frame.iter().map(|v| v * v).sum::<f32>() / FRAME as f32 + 1e-12).log10()
        })
        .collect();
    if db.is_empty() {
        return Vec::new();
    }
    // Порог от фона комнаты: тихий микрофон и шумная комната — одно и то же.
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    let loud: Vec<bool> = db.iter().map(|&v| v > (floor + 12.0).max(-55.0)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < loud.len() {
        if !loud[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < loud.len()
            && loud[j..(j + JOIN_FRAMES).min(loud.len())]
                .iter()
                .any(|&l| l)
        {
            j += 1;
        }
        // Хвост окна склейки — тишина после слова.
        let end = (i..j).rev().find(|&k| loud[k]).map_or(j, |k| k + 1);
        if (MIN_WORD_FRAMES..=MAX_WORD_FRAMES).contains(&(end - i)) {
            out.push((i * FRAME, end * FRAME));
        }
        i = j;
    }
    out
}

/// Признаки чужой речи и шума для этих весов (формат RKB1, tools/kws/make_bank.py).
pub struct Bank {
    frames: Vec<Vec<f32>>,
}

impl Bank {
    pub fn load(path: &Path, weights: &[u8]) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.get(..4) != Some(b"RKB1".as_slice()) || bytes.len() < 20 {
            bail!("not an RKB1 bank");
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let (channels, count) = (u32_at(4), u32_at(8));
        let hash = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        if hash != fnv1a(weights) {
            bail!("bank {} was built for other weights", path.display());
        }
        let scales_end = 20 + channels * 4;
        if bytes.len() != scales_end + channels * count {
            bail!("RKB1 bank is truncated");
        }
        let scales: Vec<f32> = bytes[20..scales_end]
            .chunks_exact(4)
            .map(|raw| f32::from_le_bytes(raw.try_into().unwrap()))
            .collect();
        let frames = bytes[scales_end..]
            .chunks_exact(channels)
            .map(|row| {
                row.iter()
                    .zip(&scales)
                    .map(|(&q, s)| q as f32 * s)
                    .collect()
            })
            .collect();
        Ok(Self { frames })
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Что вышло: сколько слов нашлось и сколько из них сеть слышит до и после.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    pub words: usize,
    pub heard_before: usize,
    pub heard_after: usize,
    /// Доля кадров банка выше порога, ‰.
    pub false_before: f32,
    pub false_after: f32,
    /// Подстройка применена (иначе — не помогла или не нужна).
    pub adapted: bool,
}

/// Подстроить голову `base` под «ривер» из записи. `None` в модели — оставить как есть.
pub fn adapt(
    base: &TcnModel,
    pcm: &[f32],
    bank: &Bank,
) -> anyhow::Result<(Option<TcnModel>, Report)> {
    let found = words(pcm);
    if found.len() < MIN_WORDS {
        bail!("heard {} words, need at least {MIN_WORDS}", found.len());
    }
    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut clean_features = Vec::new();
    for copy in 0..=COPIES {
        let audio = if copy == 0 {
            pcm.to_vec()
        } else {
            distort(pcm, &mut seed)
        };
        let features = base.features(&audio);
        let labels = frame_labels(&found, features.len());
        for (frame, label) in features.iter().zip(&labels) {
            match label {
                Some(true) => positives.push(frame.clone()),
                Some(false) => negatives.push(frame.clone()),
                None => {}
            }
        }
        if copy == 0 {
            clean_features = features;
        }
    }
    negatives.extend(bank.frames.iter().cloned());
    let (w0, b0) = base.head();
    let heard_before = heard(&clean_features, &found, w0, b0);
    let false_before = false_rate(&bank.frames, w0, b0);
    let mut report = Report {
        words: found.len(),
        heard_before,
        heard_after: heard_before,
        false_before,
        false_after: false_before,
        adapted: false,
    };
    for l2 in L2_STEPS {
        let (w, b) = fit(&positives, &negatives, w0, b0, l2);
        let false_after = false_rate(&bank.frames, &w, b);
        if false_after > false_before * 2.0 + 0.5 {
            continue;
        }
        let heard_after = heard(&clean_features, &found, &w, b);
        if heard_after <= heard_before {
            break;
        }
        report.heard_after = heard_after;
        report.false_after = false_after;
        report.adapted = true;
        return Ok((Some(base.with_head(w, b)), report));
    }
    Ok((None, report))
}

/// Метка кадра: слово только что кончилось — да, обычный звук — нет, внутри слова и сразу
/// за окном срабатывания — неважно (как `targets` в train_multilang.py).
fn frame_labels(found: &[(usize, usize)], frames: usize) -> Vec<Option<bool>> {
    let mut labels = vec![Some(false); frames];
    let frame_of = |sample: usize| sample.saturating_sub(WINDOW / 2) / FRAME;
    for &(start, end) in found {
        let (s, e) = (frame_of(start), frame_of(end));
        let mut set = |from: usize, to: usize, value: Option<bool>| {
            for label in &mut labels[from.min(frames)..to.min(frames)] {
                *label = value;
            }
        };
        set(s, e.saturating_sub(4), None);
        set(e.saturating_sub(4), e + 18, Some(true));
        set(e + 18, e + 60, None);
    }
    labels
}

/// Логистическая регрессия головы: Adam, вес слова `POS_WEIGHT`, L2 к исходной голове.
fn fit(
    positives: &[Vec<f32>],
    negatives: &[Vec<f32>],
    w0: &[f32],
    b0: f32,
    l2: f32,
) -> (Vec<f32>, f32) {
    let c = w0.len();
    let mut w = w0.to_vec();
    let mut b = b0;
    let (mut mw, mut vw) = (vec![0.0f32; c + 1], vec![0.0f32; c + 1]);
    let (pos_scale, neg_scale) = (
        POS_WEIGHT / positives.len() as f32,
        (1.0 - POS_WEIGHT) / negatives.len() as f32,
    );
    let mut grad = vec![0.0f32; c + 1];
    for step in 1..=ITERATIONS {
        grad.iter_mut().for_each(|g| *g = 0.0);
        for (set, target, scale) in [(positives, 1.0, pos_scale), (negatives, 0.0, neg_scale)] {
            for frame in set {
                let p = sigmoid(b + dot(&w, frame));
                let d = (p - target) * scale;
                for (g, v) in grad[..c].iter_mut().zip(frame) {
                    *g += d * v;
                }
                grad[c] += d;
            }
        }
        for i in 0..c {
            grad[i] += 2.0 * l2 * (w[i] - w0[i]) / c as f32;
        }
        let (beta1, beta2, lr) = (0.9f32, 0.999f32, 0.01f32);
        for i in 0..=c {
            mw[i] = beta1 * mw[i] + (1.0 - beta1) * grad[i];
            vw[i] = beta2 * vw[i] + (1.0 - beta2) * grad[i] * grad[i];
            let m = mw[i] / (1.0 - beta1.powi(step as i32));
            let v = vw[i] / (1.0 - beta2.powi(step as i32));
            let delta = lr * m / (v.sqrt() + 1e-8);
            if i < c {
                w[i] -= delta;
            } else {
                b -= delta;
            }
        }
    }
    (w, b)
}

/// Сколько слов сеть слышит: два кадра подряд выше порога сразу после слова.
fn heard(features: &[Vec<f32>], found: &[(usize, usize)], w: &[f32], b: f32) -> usize {
    let frame_of = |sample: usize| sample.saturating_sub(WINDOW / 2) / FRAME;
    found
        .iter()
        .filter(|&&(start, end)| {
            let from = frame_of(start);
            let to = (frame_of(end) + 40).min(features.len());
            let mut run = 0;
            features[from.min(to)..to].iter().any(|frame| {
                run = if sigmoid(b + dot(w, frame)) >= THRESHOLD {
                    run + 1
                } else {
                    0
                };
                run >= 2
            })
        })
        .count()
}

/// Кадров банка выше порога, ‰.
fn false_rate(frames: &[Vec<f32>], w: &[f32], b: f32) -> f32 {
    let above = frames
        .iter()
        .filter(|frame| sigmoid(b + dot(w, frame)) >= THRESHOLD)
        .count();
    above as f32 * 1000.0 / frames.len().max(1) as f32
}

/// Копия записи тише или громче и с белым шумом 10–30 дБ ниже речи.
fn distort(pcm: &[f32], seed: &mut u64) -> Vec<f32> {
    let mut next = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 40) as f32 / (1u64 << 24) as f32
    };
    let gain = 0.3 + next() * 1.2;
    let rms = (pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len().max(1) as f32)
        .sqrt()
        .max(1e-4);
    let noise = rms / 10f32.powf((10.0 + next() * 20.0) / 20.0);
    pcm.iter()
        .map(|&v| {
            // Сумма четырёх равномерных — почти нормальный шум.
            let n = (next() + next() + next() + next() - 2.0) * 1.7;
            ((v + n * noise) * gain).clamp(-1.0, 1.0)
        })
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-80.0, 80.0)).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| (i as f32 * 0.07).sin() * 0.3)
            .collect()
    }

    #[test]
    fn words_are_found_between_pauses_and_long_sounds_are_skipped() {
        let mut pcm = vec![0.0005f32; RATE / 2];
        for len in [0.5, 0.7, 2.5, 0.4] {
            pcm.extend(tone(len));
            pcm.extend(vec![0.0005f32; RATE / 2]);
        }
        let found = words(&pcm);
        assert_eq!(found.len(), 3, "{found:?}");
        let (start, end) = found[0];
        assert!((start as i64 - RATE as i64 / 2).abs() <= FRAME as i64);
        assert!((end as i64 - RATE as i64).abs() <= FRAME as i64);
    }

    #[test]
    fn labels_mark_frames_right_after_the_word() {
        let labels = frame_labels(&[(16_000, 24_000)], 300);
        let e = (24_000 - 200) / 160;
        assert_eq!(labels[e], Some(true));
        assert_eq!(labels[e - 10], None);
        assert_eq!(labels[e + 30], None);
        assert_eq!(labels[e + 70], Some(false));
        assert_eq!(labels[10], Some(false));
    }

    #[test]
    fn fit_moves_the_head_toward_positives_and_l2_holds_it_back() {
        let positives = vec![vec![1.0, 0.0]; 20];
        let negatives = vec![vec![0.0, 1.0]; 200];
        let (weak, _) = fit(&positives, &negatives, &[0.0, 0.0], 0.0, 0.1);
        let (strong, _) = fit(&positives, &negatives, &[0.0, 0.0], 0.0, 100.0);
        assert!(weak[0] > strong[0] && strong[0] > 0.0);
        assert!(weak[1] < 0.0);
    }

    #[test]
    fn passive_trains_on_steps_and_forgets_the_audio_at_the_goal() {
        let dir = std::env::temp_dir().join(format!("reever-passive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_passive_enabled(true);
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let train: Trainer = Box::new(move |pcm| {
            seen.lock().unwrap().push(pcm.len());
            Err("test".into())
        });
        for _ in 0..PASSIVE_GOAL + 3 {
            let clip = WakeClip { samples: vec![1000; RATE / 2], source: WakeSource::AsrOnly };
            passive_step(&dir, clip, &train);
        }
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), PASSIVE_STEPS.len());
        // Каждый шаг учится на всём накопленном: клипы с тишиной между ними.
        assert_eq!(calls[0], GAP + 10 * (GAP + RATE / 2));
        let state = PassiveState::load(&dir);
        assert_eq!((state.collected, state.sources), (PASSIVE_GOAL, [0, PASSIVE_GOAL]));
        assert!(!dir.join("clips.pcm").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bank_must_match_its_weights() {
        let dir = std::env::temp_dir().join(format!("reever-bank-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.bank");
        let weights = b"weights";
        let mut bytes = b"RKB1".to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(fnv1a(weights).to_le_bytes());
        bytes.extend(0.5f32.to_le_bytes());
        bytes.extend(1.0f32.to_le_bytes());
        bytes.extend([2u8, 3]);
        std::fs::write(&path, &bytes).unwrap();
        let bank = Bank::load(&path, weights).unwrap();
        assert_eq!(bank.frames, vec![vec![1.0, 3.0]]);
        assert!(Bank::load(&path, b"other").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
