//! Английские команды в обоих режимах языков: «русский + английский» (GigaAM основная,
//! Parakeet для цели) и «только английский» (Parakeet основная, Настройки → Языки).
//!
//!   cargo run --release --example english_probe -- <папка с wav 16 кГц> [--english-only] [--trace]

use std::path::PathBuf;

use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::bilingual::LangMode;
use reever_lib::dispatch::route;
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::Grammar;
use reever_lib::langid::SpeechLid;
use reever_lib::speech::samples;
use reever_lib::vad::SileroVad;

const CHUNK: usize = 320;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).expect("папка с wav"));
    let english_only = args.iter().any(|a| a == "--english-only");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let ru = models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16");
    let en = models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8");
    let grammar = Grammar::defaults();
    let primary = if english_only { en.clone() } else { ru };
    let mut engine = Engine::new(
        EngineConfig {
            wake_words: grammar.wake_words.clone(),
            partial_interval_ms: 0,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig { model_dir: primary, num_threads: 4 })?,
        SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
    );
    if english_only {
        engine.set_bilingual(None, None, LangMode::Ru);
    } else {
        engine.set_bilingual(
            Some(Box::new(Asr::new(&AsrConfig { model_dir: en, num_threads: 4 })?)),
            Some(Box::new(SpeechLid::new(&models.join("sherpa-onnx-whisper-tiny"), 4)?)),
            LangMode::Auto,
        );
    }
    engine.set_grammar(grammar.clone());
    let actions = reever_lib::actions::load_actions(&root.join("resources/actions.json"));
    // --trace: что слышит основная модель во всей фразе (почему не сработал wake).
    let mut trace = args
        .iter()
        .any(|a| a == "--trace")
        .then(|| Asr::new(&AsrConfig { model_dir: models.join(if english_only { "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8" } else { "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16" }), num_threads: 4 }))
        .transpose()?;

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
        .collect();
    files.sort();
    for file in files {
        let wave = sherpa_onnx::Wave::read(file.to_string_lossy().as_ref())
            .ok_or_else(|| anyhow::anyhow!("не прочитан {}", file.display()))?;
        let speech: Vec<i16> = wave.samples().iter().map(|s| (s.clamp(-1.0, 1.0) * 32_767.0) as i16).collect();
        engine.reset();
        let mut audio = vec![0i16; samples(500)];
        audio.extend_from_slice(&speech);
        audio.extend(std::iter::repeat_n(0, samples(3_000)));
        let mut result = String::from("нет команды");
        for chunk in audio.chunks(CHUNK) {
            for event in engine.push_audio(chunk) {
                match event {
                    EngineEvent::Command { text, alt } => {
                        let routed = route(&text, alt.as_deref(), &grammar, &actions, 0.7);
                        result = format!("{text:?} alt {alt:?} → {routed:?}");
                    }
                    EngineEvent::DictationStart => result = "диктовка".into(),
                    _ => {}
                }
            }
        }
        if let Some(asr) = trace.as_mut() {
            use reever_lib::speech::Transcriber;
            result = format!("{result} | слышно: {:?}", asr.transcribe(&speech).unwrap_or_default());
        }
        let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
        let result: String = result.chars().take(260).collect();
        println!("{name}: {result}");
    }
    Ok(())
}
