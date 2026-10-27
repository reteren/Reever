//! Ложные «ривер» на обычной речи без обращения: FLEURS dev, по `--per-lang` фраз на язык,
//! через секунду тишины — как в программе с моделью в памяти (нестрогое подтверждение).
//! Каждое WakeHeard, Command или DictationStart — ложное срабатывание; после него Engine
//! сбрасывается, чтобы следующая фраза не стала «командой».
//! cargo run --release --example wake_far_probe -- --fleurs ../tools/kws/data/fleurs --per-lang 150 > far.jsonl

use std::path::{Path, PathBuf};

use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::{set_active_languages, Grammar};
use reever_lib::vad::SileroVad;
use reever_lib::wake::AcousticWakeSpotter;
use serde_json::json;

const CHUNK: usize = 320;
const LANGS: [&str; 5] = ["en_us", "ru_ru", "uk_ua", "fr_fr", "de_de"];

fn wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("cannot read {}", path.display()))?;
    anyhow::ensure!(wave.sample_rate() == 16_000, "expected 16 kHz: {}", path.display());
    Ok(wave.samples().iter().map(|&x| (x.clamp(-1.0, 1.0) * 32767.0) as i16).collect())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.iter().position(|v| v == key).and_then(|i| args.get(i + 1)).cloned();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fleurs = arg("--fleurs").map(PathBuf::from).unwrap_or_else(|| root.join("../tools/kws/data/fleurs"));
    let per_lang: usize = arg("--per-lang").map(|x| x.parse()).transpose()?.unwrap_or(150);
    let model = arg("--model").unwrap_or_else(|| "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8".into());

    set_active_languages(&["en", "ru"]);
    let asr = Asr::new(&AsrConfig { model_dir: root.join("resources/model").join(&model), num_threads: 2 })?;
    let vad = SileroVad::new(&root.join("resources/model/silero_vad.onnx"), 0.5)?;
    let mut engine = Engine::new(EngineConfig { partial_interval_ms: 0, ..EngineConfig::default() }, asr, vad);
    engine.set_grammar(Grammar::defaults());
    engine.set_strict_spotted_wake(false);
    engine.set_english_primary(true);
    let spotter = AcousticWakeSpotter::new(&root.join("resources/wake/river-ru-en.bin"))?
        .with_verifier(&root.join("resources/wake/verify-ru-en.bin"))?;
    engine.set_wake_spotter(Some(Box::new(spotter)));

    let silence = vec![0i16; 16_000];
    let (mut seconds, mut spots, mut wakes, mut commands, mut dictations) = (0f64, 0, 0, 0, 0);
    for lang in LANGS {
        let mut files: Vec<PathBuf> = std::fs::read_dir(fleurs.join(lang).join("dev"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();
        for file in files.into_iter().take(per_lang) {
            let pcm = wav(&file)?;
            seconds += (pcm.len() + silence.len()) as f64 / 16_000.0;
            for part in pcm.chunks(CHUNK).chain(silence.chunks(CHUNK)) {
                let mut fired = None;
                for event in engine.push_audio(part) {
                    match event {
                        EngineEvent::WakeSpotted => spots += 1,
                        EngineEvent::WakeHeard => { wakes += 1; fired = Some("wake".to_string()); }
                        EngineEvent::Command { text, .. } => { commands += 1; fired = Some(format!("command: {text}")); }
                        EngineEvent::DictationStart => { dictations += 1; fired = Some("dictation".into()); }
                        _ => {}
                    }
                }
                if let Some(what) = fired {
                    println!("{}", json!({ "lang": lang, "file": file.file_name().map(|n| n.to_string_lossy().to_string()), "event": what }));
                    engine.reset();
                }
            }
        }
        eprintln!("{lang}: {:.2} h so far, spots {spots}, wakes {wakes}, commands {commands}, dictation {dictations}", seconds / 3600.0);
    }
    let hours = seconds / 3600.0;
    println!("{}", json!({ "hours": hours, "detector_spots": spots, "wake_heard": wakes, "commands": commands,
        "dictation": dictations, "false_per_hour": (wakes + commands + dictations) as f64 / hours }));
    Ok(())
}
