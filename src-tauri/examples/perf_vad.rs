//! Measures the existing Silero VAD on silent 512-sample frames without starting audio capture.

use std::{path::PathBuf, time::Instant};

use reever_lib::{speech::VoiceDetector, vad::SileroVad};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let model = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("resources/model/silero_vad.onnx"));
    let frames = args
        .get(2)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(20_000usize);

    let mut vad = SileroVad::new(&model, 0.5)?;
    let silence = [0i16; 512];
    for _ in 0..32 {
        std::hint::black_box(vad.is_speech(&silence));
    }

    let started = Instant::now();
    let mut detected = 0usize;
    for _ in 0..frames {
        detected += usize::from(vad.is_speech(std::hint::black_box(&silence)));
    }
    let elapsed = started.elapsed();
    let frame_ms = elapsed.as_secs_f64() * 1_000.0 / frames as f64;
    let idle_budget_pct = frame_ms / 32.0 * 100.0;

    println!("model={}", model.display());
    println!("model_bytes={}", std::fs::metadata(&model)?.len());
    println!("frames={frames}; samples_per_frame=512; sample_rate_hz=16000");
    println!("elapsed_ms={:.3}; ms_per_frame={frame_ms:.5}", elapsed.as_secs_f64() * 1_000.0);
    println!("frames_per_second={:.1}", frames as f64 / elapsed.as_secs_f64());
    println!("estimated_single_core_realtime_budget_pct={idle_budget_pct:.3}");
    println!("speech_frames_on_silence={detected}");
    Ok(())
}
