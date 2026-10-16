//! Запись одного 1,5-секундного PCM16 примера. Запускает только пользователь.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

const TARGET_RATE: usize = 16_000;

fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data_size = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(TARGET_RATE as u32).to_le_bytes());
    out.extend_from_slice(&((TARGET_RATE * 2) as u32).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn resample(audio: &[f32], source_rate: usize) -> Vec<i16> {
    if audio.is_empty() {
        return Vec::new();
    }
    let count = (audio.len() as f64 * TARGET_RATE as f64 / source_rate as f64).round() as usize;
    (0..count)
        .map(|i| {
            let position = i as f64 * source_rate as f64 / TARGET_RATE as f64;
            let left = position.floor() as usize;
            let right = (left + 1).min(audio.len() - 1);
            let fraction = (position - left as f64) as f32;
            let value =
                audio[left.min(audio.len() - 1)] * (1.0 - fraction) + audio[right] * fraction;
            (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    print!("Нажмите Enter и сразу произнесите только «ривер» (запись 1,5 с): ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;

    let device = cpal::default_host()
        .default_input_device()
        .context("микрофон не найден")?;
    let supported = device
        .default_input_config()
        .context("не удалось прочитать формат микрофона")?;
    let config: StreamConfig = supported.clone().into();
    let rate = config.sample_rate.0 as usize;
    let channels = config.channels as usize;
    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let output = captured.clone();
    let err = |error| eprintln!("Ошибка аудиопотока: {error}");
    let stream = match supported.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let mut out = output.lock().unwrap();
                for frame in data.chunks(channels) {
                    out.push(frame.iter().sum::<f32>() / channels as f32);
                }
            },
            err,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let mut out = output.lock().unwrap();
                for frame in data.chunks(channels) {
                    out.push(
                        frame.iter().map(|x| *x as f32 / 32768.0).sum::<f32>() / channels as f32,
                    );
                }
            },
            err,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let mut out = output.lock().unwrap();
                for frame in data.chunks(channels) {
                    out.push(
                        frame.iter().map(|x| *x as f32 / 32768.0 - 1.0).sum::<f32>()
                            / channels as f32,
                    );
                }
            },
            err,
            None,
        )?,
        other => bail!("формат микрофона не поддержан: {other:?}"),
    };
    stream.play()?;
    std::thread::sleep(Duration::from_millis(1500));
    drop(stream);
    let audio = captured.lock().unwrap().clone();
    let wav = resample(&audio, rate);
    if wav.is_empty() {
        bail!("микрофон не вернул аудиоданные");
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("корень репозитория не найден")?;
    let dir = root.join("tools/kws/data/user");
    std::fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = dir.join(format!("user-{stamp}.wav"));
    std::fs::write(&path, wav_bytes(&wav)).with_context(|| format!("запись {}", path.display()))?;
    println!(
        "Сохранено: {} ({} сэмплов, 16 кГц)",
        path.display(),
        wav.len()
    );
    Ok(())
}
