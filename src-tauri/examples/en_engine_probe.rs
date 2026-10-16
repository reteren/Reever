//! English command probe for Parakeet variants through Reever's real Engine.
//!
//!   cargo run --release --example en_engine_probe -- <wav-dir> --model <model-dir> [--english-only]

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
    let english_only = args.iter().any(|arg| arg == "--english-only");
    let model_arg = args
        .iter()
        .position(|arg| arg == "--model")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
        .expect("--model <папка модели>");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let en = if model_arg.is_absolute() {
        model_arg
    } else {
        models.join(model_arg)
    };
    anyhow::ensure!(en.is_dir(), "нет каталога модели: {}", en.display());

    let grammar = Grammar::defaults();
    let primary = if english_only {
        en.clone()
    } else {
        models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16")
    };
    let mut engine = Engine::new(
        EngineConfig {
            wake_words: grammar.wake_words.clone(),
            partial_interval_ms: 0,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig {
            model_dir: primary,
            num_threads: 4,
        })?,
        SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
    );
    if english_only {
        engine.set_bilingual(None, None, LangMode::Ru);
    } else {
        engine.set_bilingual(
            Some(Box::new(Asr::new(&AsrConfig {
                model_dir: en.clone(),
                num_threads: 4,
            })?)),
            Some(Box::new(SpeechLid::new(
                &models.join("sherpa-onnx-whisper-tiny"),
                4,
            )?)),
            LangMode::Auto,
        );
    }
    engine.set_grammar(grammar.clone());
    let actions = reever_lib::actions::load_actions(&root.join("resources/actions.json"));

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "wav"))
        .collect();
    files.sort();
    println!(
        "MODE {} | MODEL {} | FILES {}",
        if english_only { "en-only" } else { "ru+en" },
        en.display(),
        files.len()
    );

    for file in files {
        let wave = sherpa_onnx::Wave::read(file.to_string_lossy().as_ref())
            .ok_or_else(|| anyhow::anyhow!("не прочитан {}", file.display()))?;
        anyhow::ensure!(wave.sample_rate() == 16_000, "нужен WAV 16 кГц: {}", file.display());
        let speech: Vec<i16> = wave
            .samples()
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
            .collect();
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
                        result = format!("{text:?} alt {alt:?} -> {routed:?}");
                    }
                    EngineEvent::DictationStart => result = "диктовка началась".into(),
                    _ => {}
                }
            }
        }
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        let result: String = result.chars().take(260).collect();
        println!("{name}: {result}");
    }
    Ok(())
}
