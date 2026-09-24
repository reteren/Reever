//! Быстрый прогон GigaAM по WAV: cargo run --release --example asr_bench -- <model-dir> [--threads N] <wav>...

use std::time::Instant;
use std::{env, path::PathBuf, process};

use anyhow::{bail, Context};
use reever_lib::{
    asr::{Asr, AsrConfig},
    textnorm,
};
use sherpa_onnx::Wave;

fn main() -> anyhow::Result<()> {
    let mut args = env::args_os().skip(1);
    let model_dir = args.next().map(PathBuf::from).unwrap_or_else(|| usage());

    let mut num_threads = 4;
    let mut wav_paths = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--threads" {
            let value = args.next().unwrap_or_else(|| usage());
            num_threads = value
                .to_string_lossy()
                .parse::<i32>()
                .context("--threads должен быть целым числом")?;
            if num_threads < 1 {
                bail!("--threads должен быть не меньше 1");
            }
        } else {
            wav_paths.push(PathBuf::from(arg));
        }
    }
    if wav_paths.is_empty() {
        usage();
    }

    let mut asr = Asr::new(&AsrConfig {
        model_dir: model_dir.clone(),
        num_threads,
    })?;
    println!("model={} threads={num_threads}", model_dir.display());

    let mut total_elapsed = 0.0_f64;
    let mut total_audio = 0.0_f64;
    for wav_path in wav_paths {
        let wav_name = wav_path.to_string_lossy();
        let wave = Wave::read(&wav_name)
            .with_context(|| format!("не удалось прочитать mono WAV {}", wav_path.display()))?;
        let sample_rate = wave.sample_rate();
        if sample_rate != 16_000 {
            bail!(
                "{}: частота {} Гц; бенчмарк ожидает mono 16 кГц",
                wav_path.display(),
                sample_rate
            );
        }
        let audio_seconds = wave.num_samples() as f64 / f64::from(sample_rate);
        let pcm: Vec<i16> = wave
            .samples()
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
            .collect();

        let started = Instant::now();
        let text = asr.transcribe_raw(&pcm)?;
        let normalized = textnorm::normalize(&text);
        let elapsed = started.elapsed().as_secs_f64();
        let rtf = if audio_seconds > 0.0 {
            elapsed / audio_seconds
        } else {
            0.0
        };
        println!(
            "{} | {:.3}s | audio {:.3}s | RTF {:.3} | raw: {:?} | normalized: {}",
            wav_path.display(),
            elapsed,
            audio_seconds,
            rtf,
            text,
            normalized
        );
        total_elapsed += elapsed;
        total_audio += audio_seconds;
    }

    if total_audio > 0.0 {
        println!(
            "TOTAL elapsed={total_elapsed:.3}s audio={total_audio:.3}s RTF={:.3}",
            total_elapsed / total_audio
        );
    }
    Ok(())
}

fn usage() -> ! {
    eprintln!("usage: asr_bench <model-dir> [--threads N] <mono-16khz.wav>...");
    process::exit(2);
}
