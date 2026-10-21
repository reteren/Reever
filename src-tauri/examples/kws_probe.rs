//! Offline probe детектора RKW1 (tiny DS-CNN) без доступа к микрофону.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use reever_lib::kws::{StreamingKws, HOP_SAMPLES, SAMPLE_RATE};

fn read_pcm16_mono(path: &Path) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("чтение {}", path.display()))?;
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        bail!("ожидался RIFF/WAVE: {}", path.display());
    }
    let mut offset = 12;
    let mut channels = 0u16;
    let mut rate = 0u32;
    let mut bits = 0u16;
    let mut format = 0u16;
    let mut audio = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let begin = offset + 8;
        let end = begin.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end >= begin + 16 {
            format = u16::from_le_bytes(bytes[begin..begin + 2].try_into().unwrap());
            channels = u16::from_le_bytes(bytes[begin + 2..begin + 4].try_into().unwrap());
            rate = u32::from_le_bytes(bytes[begin + 4..begin + 8].try_into().unwrap());
            bits = u16::from_le_bytes(bytes[begin + 14..begin + 16].try_into().unwrap());
        } else if id == b"data" {
            audio = Some(&bytes[begin..end]);
        }
        offset = begin + size + size % 2;
    }
    if format != 1 || bits != 16 || channels != 1 || rate != SAMPLE_RATE as u32 {
        bail!("нужен PCM16 mono 16 кГц; найден format={format} bits={bits} channels={channels} rate={rate}");
    }
    let data = audio.context("WAV не содержит data chunk")?;
    Ok(data
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0)
        .collect())
}

fn main() -> anyhow::Result<()> {
    let mut model_path = None::<PathBuf>;
    let mut wavs = Vec::<PathBuf>::new();
    let mut threshold = 0.65f32;
    let mut dump_scores = false;
    let mut idle_seconds = 0u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model_path = args.next().map(PathBuf::from),
            "--wav" => wavs.push(
                args.next()
                    .map(PathBuf::from)
                    .context("после --wav нужен путь")?,
            ),
            "--threshold" => {
                threshold = args
                    .next()
                    .context("после --threshold нужен score")?
                    .parse()?
            }
            "--all-scores" => dump_scores = true,
            "--idle-seconds" => {
                idle_seconds = args
                    .next()
                    .context("после --idle-seconds нужны секунды")?
                    .parse()?
            }
            "--help" | "-h" => {
                println!("kws_probe --model <data/kws.bin> [--wav <file.wav> ...] [--threshold 0.5] [--all-scores] [--idle-seconds N]");
                return Ok(());
            }
            other => bail!("неизвестный аргумент: {other}"),
        }
    }
    let model_path = model_path.context("укажите --model")?;
    for path in wavs {
        let audio = read_pcm16_mono(&path)?;
        let mut detector = StreamingKws::from_path(&model_path, threshold)?;
        let mut max_score = 0.0f32;
        let mut frames = 0usize;
        let mut hits = 0usize;
        let start = Instant::now();
        for chunk in audio.chunks(HOP_SAMPLES) {
            let (scores, hit) = detector.feed_scores(chunk);
            if let Some(score) = hit {
                hits += 1;
                let _ = score;
            }
            for score in scores {
                max_score = max_score.max(score);
                if dump_scores {
                    println!("score={score:.8}");
                }
                frames += 1;
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let duration = audio.len() as f64 / SAMPLE_RATE as f64;
        println!("file={} duration_s={duration:.3} frames={frames} max_score={max_score:.8} events={hits} elapsed_s={elapsed:.6} rtf={:.5}", path.display(), elapsed / duration.max(0.001));
    }
    if idle_seconds > 0 {
        let zeros = vec![0.0f32; HOP_SAMPLES];
        let mut detector = StreamingKws::from_path(&model_path, threshold)?;
        let start = Instant::now();
        let mut hits = 0usize;
        for _ in 0..idle_seconds as usize * SAMPLE_RATE / HOP_SAMPLES {
            if detector.feed(&zeros).is_some() {
                hits += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!(
            "idle_seconds={idle_seconds} hits={hits} elapsed_s={:.6}",
            start.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
