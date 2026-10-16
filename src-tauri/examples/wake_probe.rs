//! Исследовательская проба фонового wake-детектора без запуска Reever и доступа к микрофону.
//!
//! Из src-tauri:
//!   cargo run --release --example wake_probe
//!   cargo run --release --example wake_probe -- --idle-seconds 20
//!
//! Первая команда повторно прогоняет WAV из testdata/manifest.json, кусками по 20 мс.
//! Вторая держит только StreamingWakeSpotter на синтетическом потоке тишины; READY
//! печатается после загрузки модели, чтобы снаружи снять память/CPU процесса.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reever_lib::speech::WakeSpotter;
use reever_lib::wake::StreamingWakeSpotter;
use serde::Deserialize;

const CHUNK: usize = 320; // 20 ms at 16 kHz
const MODEL: &str = "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16";
const DEFAULT_WAKE_WORDS: &[&str] = &["ривер", "ревер", "ривьер"];

#[derive(Deserialize)]
struct Case {
    file: String,
    scenario: String,
    text: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri parent")
        .to_path_buf()
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не прочитан {}", path.display()))?;
    anyhow::ensure!(
        wave.sample_rate() == 16_000,
        "{}: нужен 16 кГц",
        path.display()
    );
    Ok(wave
        .samples()
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
        .collect())
}

fn load_spotter(args: &[String], root: &Path) -> Result<StreamingWakeSpotter> {
    let model_dir = arg_value(args, "--model")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("src-tauri/resources/model").join(MODEL));
    println!("model={}", model_dir.display());
    let wake_words: Vec<String> = arg_value(args, "--wake-words")
        .map(|words| {
            words
                .split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| {
            DEFAULT_WAKE_WORDS
                .iter()
                .map(|word| (*word).to_string())
                .collect()
        });
    StreamingWakeSpotter::new(&model_dir, &wake_words)
}

fn run_idle(args: &[String], spotter: &mut StreamingWakeSpotter) -> Result<()> {
    let seconds = arg_value(args, "--idle-seconds")
        .context("после --idle-seconds укажите количество секунд")?
        .parse::<u64>()?;
    println!("READY pid={} seconds={seconds}", std::process::id());
    std::io::stdout().flush()?;

    let silence = [0i16; CHUNK];
    let start = Instant::now();
    let mut chunks = 0u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        let chunk_start = Instant::now();
        let _ = spotter.feed(&silence);
        chunks += 1;
        if let Some(left) = Duration::from_millis(20).checked_sub(chunk_start.elapsed()) {
            std::thread::sleep(left);
        }
    }
    println!(
        "IDLE elapsed_ms={} chunks={} fed_audio_s={:.2}",
        start.elapsed().as_millis(),
        chunks,
        chunks as f64 * 0.02
    );
    Ok(())
}

fn run_corpus(args: &[String], root: &Path, spotter: &mut StreamingWakeSpotter) -> Result<()> {
    let corpus = arg_value(args, "--corpus")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("testdata/corpus"));
    let manifest_path = arg_value(args, "--manifest")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("testdata/manifest.json"));
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .with_context(|| format!("манифест {}", manifest_path.display()))?;

    let mut positive = 0usize;
    let mut positive_hits = 0usize;
    let mut negative = 0usize;
    let mut false_hits = 0usize;
    let mut audio_seconds = 0f64;
    let start = Instant::now();

    for case in &cases {
        let audio = read_wav(&corpus.join(&case.file))?;
        let is_positive = case.scenario != "negative";
        if is_positive {
            positive += 1;
        } else {
            negative += 1;
        }
        audio_seconds += audio.len() as f64 / 16_000.0;

        spotter.reset();
        let mut detected = false;
        for chunk in audio.chunks(CHUNK) {
            detected |= spotter.feed(chunk);
        }
        match (is_positive, detected) {
            (true, true) => positive_hits += 1,
            (false, true) => false_hits += 1,
            _ => {}
        }
        if args.iter().any(|arg| arg == "--verbose") && is_positive != detected {
            println!(
                "{} {} text={}",
                if detected { "FALSE" } else { "MISS" },
                case.file,
                case.text
            );
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let rtf = elapsed / audio_seconds.max(f64::EPSILON);
    println!("cases={}", cases.len());
    println!(
        "positive={positive} hits={positive_hits} misses={}",
        positive - positive_hits
    );
    println!("negative={negative} false_hits={false_hits}");
    println!(
        "audio_s={audio_seconds:.2} replay_wall_s={elapsed:.3} rtf={rtf:.5} one_core_percent={:.2}",
        rtf * 100.0
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();
    let mut spotter = load_spotter(&args, &root)?;
    if args.iter().any(|arg| arg == "--idle-seconds") {
        run_idle(&args, &mut spotter)
    } else {
        run_corpus(&args, &root, &mut spotter)
    }
}
