//! Файл в текст тем же кодом, что вкладка «Текст» (transcribe.rs):
//!   cargo run --release --example transcribe_file -- <файл> [en,ru,…]
//! Печатает длительность, время декодирования и распознавания и текст.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use reever_lib::models::ModelDirs;
use reever_lib::transcribe::{decode, transcribe, Recognizer};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("файл");
    let languages: Vec<String> = args.next().unwrap_or_else(|| "en".into()).split(',').map(str::to_string).collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let t = Instant::now();
    let pcm = decode(Path::new(&file))?;
    eprintln!("audio {:.1} s, decoded in {} ms", pcm.len() as f32 / 16_000.0, t.elapsed().as_millis());
    let t = Instant::now();
    let dirs = ModelDirs::new(root.join("resources/model"));
    let mut r = Recognizer::load(&languages, &dirs, &root.join("resources/model/silero_vad.onnx"), 0.5, 4)?;
    eprintln!("models {:?} loaded in {} ms", r.languages, t.elapsed().as_millis());
    let t = Instant::now();
    let text = transcribe(&mut r, &pcm, true, &AtomicBool::new(false), |_, _| {})?;
    eprintln!("transcribed in {} ms, {} chars", t.elapsed().as_millis(), text.chars().count());
    println!("{text}");
    Ok(())
}
