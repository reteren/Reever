//! Тихая подстройка (voice_adapt::passive_step) на записях посторонних дикторов: учится на
//! вырезанных «ривер» из обучающих 70 % (tools/kws/data/friend-*/clips.jsonl, до 40 слов, как
//! в программе), проверяется на отложенных 30 % (tools/kws/extract_holdout_for_probe.py):
//! сколько обращений детектор ловит общими весами и подстроенными.
//! cargo run --release --example voice_passive_probe -- --holdout %TEMP%\reever-wake-holdout

use std::path::{Path, PathBuf};

use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::kws::TcnModel;
use reever_lib::speech::Transcriber;
use reever_lib::vad::SileroVad;
use reever_lib::voice_adapt::{adapt, Bank, PASSIVE_GOAL};
use reever_lib::wake::AcousticWakeSpotter;
use serde::Deserialize;

const RATE: usize = 16_000;

#[derive(Deserialize)]
struct Case {
    file: String,
    wake_start: f32,
    wake_end: f32,
}

struct SilentAsr;

impl Transcriber for SilentAsr {
    fn transcribe(&mut self, _pcm: &[i16]) -> anyhow::Result<String> {
        Ok(String::new())
    }
}

fn wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("cannot read {}", path.display()))?;
    anyhow::ensure!(wave.sample_rate() == RATE as i32, "expected 16 kHz: {}", path.display());
    Ok(wave.samples().to_vec())
}

fn cases(dir: &Path) -> anyhow::Result<Vec<Case>> {
    std::fs::read_to_string(dir.join("clips.jsonl"))?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

/// Ловит ли детектор (со всеми фильтрами Engine) обращение в записи.
fn spotted(root: &Path, weights: &Path, pcm: &[f32]) -> anyhow::Result<bool> {
    let vad = SileroVad::new(&root.join("resources/model/silero_vad.onnx"), 0.5)?;
    let mut engine = Engine::new(EngineConfig { partial_interval_ms: 0, ..EngineConfig::default() }, SilentAsr, vad);
    engine.set_strict_spotted_wake(true);
    engine.set_wake_spotter(Some(Box::new(AcousticWakeSpotter::new(weights)?)));
    let mut stream: Vec<i16> = vec![0; RATE];
    stream.extend(pcm.iter().map(|&x| (x.clamp(-1.0, 1.0) * 32767.0) as i16));
    stream.extend(vec![0; 3 * RATE]);
    for part in stream.chunks(320) {
        if engine.push_audio(part).iter().any(|e| matches!(e, EngineEvent::WakeSpotted)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.iter().position(|v| v == key).and_then(|i| args.get(i + 1)).cloned();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let train_root = root.join("../tools/kws/data");
    let holdout = PathBuf::from(arg("--holdout").ok_or_else(|| anyhow::anyhow!("--holdout <dir>"))?);
    let tmp = std::env::temp_dir().join("reever-passive-probe");
    std::fs::create_dir_all(&tmp)?;
    for (folder, weights) in [
        ("friend-ru-uk", "wake/river-ru-en.bin"),
        ("friend-ru2", "wake/river-ru-en.bin"),
        ("friend-fr", "wake/river.bin"),
    ] {
        let weights = root.join("resources").join(weights);
        let bytes = std::fs::read(&weights)?;
        let base = TcnModel::from_bytes(&bytes)?;
        let bank = Bank::load(&weights.with_extension("bank"), &bytes)?;
        // Как passive_step: полсекунды тишины, слово с запасом 0,25 с до и 0,3 с после.
        let mut pcm = Vec::new();
        let mut used = 0;
        for case in cases(&train_root.join(folder))?.into_iter().take(PASSIVE_GOAL as usize) {
            let audio = wav(&train_root.join(folder).join(&case.file))?;
            let at = |s: f32| ((s.max(0.0) * RATE as f32) as usize).min(audio.len());
            let (from, to) = (at(case.wake_start - 0.25), at(case.wake_end + 0.3));
            pcm.extend(std::iter::repeat_n(0.0, RATE / 2));
            pcm.extend_from_slice(&audio[from..to]);
            used += 1;
        }
        let started = std::time::Instant::now();
        let (model, report) = adapt(&base, &pcm, &bank)?;
        let took = started.elapsed().as_secs_f32();
        let adapted = tmp.join(format!("{folder}.bin"));
        std::fs::write(&adapted, model.unwrap_or(base).to_bytes())?;
        let (mut total, mut before, mut after) = (0, 0, 0);
        for case in cases(&holdout.join(folder))? {
            let audio = wav(&holdout.join(folder).join(&case.file))?;
            total += 1;
            before += usize::from(spotted(&root, &weights, &audio)?);
            after += usize::from(spotted(&root, &adapted, &audio)?);
        }
        println!(
            "{folder}: учился на {used} словах за {took:.1} с — {report:?}; отложенные: {before}/{total} → {after}/{total}"
        );
    }
    Ok(())
}
