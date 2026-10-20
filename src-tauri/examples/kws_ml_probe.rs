//! Оценки многоязычного детектора RKW2 (kws.rs) по кадрам для одного WAV — сверка с
//! обучением (tools/kws/compare_ml.py) и ручная проверка записей.
//!   cargo run --release --example kws_ml_probe -- <веса.bin> <файл.wav> [--threshold 0.85]

use std::path::Path;

use reever_lib::kws::StreamingTcn;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(weights), Some(wav)) = (args.first(), args.get(1)) else {
        anyhow::bail!("usage: kws_ml_probe <weights.bin> <file.wav> [--threshold 0.85]");
    };
    let threshold = args
        .iter()
        .position(|a| a == "--threshold")
        .and_then(|i| args.get(i + 1))
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.85);
    let wave = sherpa_onnx::Wave::read(wav).ok_or_else(|| anyhow::anyhow!("не прочитан {wav}"))?;
    anyhow::ensure!(wave.sample_rate() == 16_000, "нужен WAV 16 кГц");
    let mut kws = StreamingTcn::from_path(Path::new(weights), threshold)?;
    let mut scores = Vec::new();
    let mut events = Vec::new();
    for chunk in wave.samples().chunks(320) {
        let before = scores.len();
        if kws.feed_scores(chunk, &mut scores).is_some() {
            events.push(before);
        }
    }
    for score in &scores {
        println!("{score:.6}");
    }
    eprintln!("кадров {}, срабатываний {} (кадры {:?})", scores.len(), events.len(), events);
    Ok(())
}
