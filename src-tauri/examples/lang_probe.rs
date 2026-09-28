//! Диагностика выбора языка диктовки: для каждого WAV (16 кГц mono PCM16) печатает текст
//! обеих моделей «как сказано», ответ определителя языка и итог `dictation::choose_language`.
//!
//! cargo run --release --example lang_probe -- <файлы.wav…>

use std::path::{Path, PathBuf};

use reever_lib::{
    asr::{Asr, AsrConfig},
    bilingual::LangMode,
    dictation::choose_language,
    engine::{Engine, EngineConfig, EngineEvent},
    grammar::Grammar,
    langid::SpeechLid,
    speech::{samples, LanguageId, Transcriber},
    vad::SileroVad,
};

fn main() -> anyhow::Result<()> {
    let model = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/model");
    let mut ru = Asr::new(&AsrConfig {
        model_dir: model.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16"),
        num_threads: 4,
    })?;
    let mut en = Asr::new(&AsrConfig {
        model_dir: model.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"),
        num_threads: 4,
    })?;
    let mut lid = SpeechLid::new(&model.join("sherpa-onnx-whisper-base"), 2)?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--engine") {
        // Склеить файлы с паузой 1,2 с и прогнать через настоящий Engine (VAD + диктовка).
        let mut pcm = Vec::new();
        for path in &args[1..] {
            pcm.extend(read_wav(Path::new(path))?);
            pcm.extend(std::iter::repeat(0i16).take(samples(1_200)));
        }
        pcm.extend(std::iter::repeat(0i16).take(samples(11_000)));
        let vad = SileroVad::new(&model.join("silero_vad.onnx"), 0.5)?;
        let mut engine = Engine::new(EngineConfig::default(), ru, vad);
        engine.set_grammar(Grammar::defaults());
        engine.set_bilingual(Some(Box::new(en)), Some(Box::new(lid)), LangMode::Auto);
        // LANG_PROBE_CONFIRM: после команды — как recognizer при confirm_close — ждать «да».
        let confirm = std::env::var_os("LANG_PROBE_CONFIRM").is_some();
        for chunk in pcm.chunks(samples(20)) {
            for event in engine.push_audio(chunk) {
                match event {
                    EngineEvent::Partial(_) | EngineEvent::State(_) => {}
                    EngineEvent::Command { .. } if confirm => {
                        println!("{event:?} → listen_confirm");
                        engine.listen_confirm();
                    }
                    other => println!("{other:?}"),
                }
            }
        }
        return Ok(());
    }

    for path in args {
        let pcm = read_wav(Path::new(&path))?;
        let ru_text = ru.transcribe_verbatim(&pcm)?;
        let en_text = en.transcribe_verbatim(&pcm)?;
        let lid_lang = lid.detect(&pcm)?;
        let choice = choose_language(&ru_text, &en_text, Some(lid_lang));
        if std::env::var_os("LANG_PROBE_TIMING").is_some() {
            let timed = ru.transcribe_timed(&pcm)?;
            println!("  тайминг: {:?}", timed.words);
            println!("  длительность: {:.2} с", pcm.len() as f32 / 16_000.0);
        }
        println!(
            "{}\n  RU: {ru_text}\n  EN: {en_text}\n  LID: {lid_lang:?}\n  выбор: {choice:?}\n",
            Path::new(&path).file_name().unwrap_or_default().to_string_lossy()
        );
    }
    Ok(())
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path)?;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        if id == b"data" {
            let data = &bytes[offset + 8..(offset + 8 + len).min(bytes.len())];
            return Ok(data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect());
        }
        offset += 8 + len + (len & 1);
    }
    anyhow::bail!("в {} нет блока data", path.display())
}
