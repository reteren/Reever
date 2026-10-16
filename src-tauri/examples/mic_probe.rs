//! Запись того, что слышит Reever (тот же захват, 16 кГц mono), с громкостью по секундам —
//! сравнить звук в звонке Discord и без него:
//!
//!   cargo run --release --example mic_probe -- 20 testdata/live/probe.wav

use std::time::{Duration, Instant};

fn db(value: f64) -> f64 {
    20.0 * value.max(1e-9).log10()
}

fn write_wav(path: &str, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(20);
    let out = args.get(1).cloned().unwrap_or_else(|| "mic-probe.wav".into());

    let (tx, rx) = reever_lib::audio::audio_channel(256);
    let _stream = reever_lib::audio::start_capture(tx)?;
    println!("громкость микрофона в Windows: {:?}", reever_lib::micwatch::mic_level());
    let started = Instant::now();
    let mut all: Vec<i16> = Vec::new();
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            all.extend_from_slice(&chunk);
        }
    }
    println!("сек  RMS dBFS  пик dBFS");
    for (index, second) in all.chunks(16_000).enumerate() {
        let rms = (second.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / second.len() as f64).sqrt();
        let peak = second.iter().map(|&s| (s as f64 / 32768.0).abs()).fold(0.0, f64::max);
        println!("{index:>3}  {:>8.1}  {:>8.1}", db(rms), db(peak));
    }
    write_wav(&out, &all)?;
    println!("записано: {out} ({:.1} с)", all.len() as f64 / 16_000.0);
    Ok(())
}
