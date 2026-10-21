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
    if args.first().map(String::as_str) == Some("--spot") {
        // Где фоновый детектор слышит wake-слово (секунды от начала файла), по 20 мс.
        for path in &args[1..] {
            let mut spotter = reever_lib::wake::AcousticWakeSpotter::new(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin"))?;
            let mut pcm = read_wav(Path::new(path))?;
            pcm.extend(std::iter::repeat(0i16).take(samples(1_500)));
            let mut hits = Vec::new();
            for (index, chunk) in pcm.chunks(samples(20)).enumerate() {
                use reever_lib::speech::WakeSpotter;
                if spotter.feed(chunk) {
                    hits.push(format!("{:.2}", index as f32 * 0.02));
                }
            }
            println!(
                "{:<40} {}",
                Path::new(path).file_name().unwrap_or_default().to_string_lossy(),
                if hits.is_empty() { "—".into() } else { hits.join(", ") }
            );
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--spot-engine") {
        // Корпус через Engine (VAD + детектор + GigaAM): по файлу — сигналы детектора и
        // команды. Файлы разделены 2 с тишины, движок один на весь прогон.
        let vad = SileroVad::new(&model.join("silero_vad.onnx"), 0.5)?;
        let mut engine = Engine::new(EngineConfig::default(), ru, vad);
        engine.set_grammar(Grammar::defaults());
        let with_spotter = std::env::var_os("LANG_PROBE_NO_SPOTTER").is_none();
        if with_spotter {
            let spotter = reever_lib::wake::AcousticWakeSpotter::new(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin"))?;
            engine.set_wake_spotter(Some(Box::new(spotter)));
        }
        for path in &args[1..] {
            let mut pcm = read_wav(Path::new(path))?;
            pcm.extend(std::iter::repeat(0i16).take(samples(7_000)));
            let (mut spotted, mut woke) = (0, Vec::new());
            // LANG_PROBE_BEEP=<wav 16 кГц>: сигнал «услышал» подмешивается в микрофон с момента
            // срабатывания детектора (40 % громкости) — как динамики в реальной комнате.
            let beep = std::env::var_os("LANG_PROBE_BEEP")
                .map(|path| read_wav(Path::new(&path)))
                .transpose()?;
            let mut beep_at: Option<usize> = None;
            let chunk_len = samples(20);
            for index in 0..pcm.len().div_ceil(chunk_len) {
                let start = index * chunk_len;
                let end = (start + chunk_len).min(pcm.len());
                if let (Some(beep), Some(at)) = (&beep, beep_at) {
                    for (i, sample) in pcm[start..end].iter_mut().enumerate() {
                        if let Some(b) = beep.get(start + i - at) {
                            *sample = (*sample as i32 + (*b as i32 * 2 / 5)).clamp(-32768, 32767) as i16;
                        }
                    }
                }
                for event in engine.push_audio(&pcm[start..end]) {
                    match event {
                        EngineEvent::WakeSpotted => {
                            spotted += 1;
                            beep_at.get_or_insert(end);
                        }
                        EngineEvent::Command { text, .. } => woke.push(text),
                        EngineEvent::WakeHeard => woke.push("<wake>".into()),
                        EngineEvent::Log(message) if message.starts_with("wake:") => woke.push("<починка>".into()),
                        _ => {}
                    }
                }
            }
            engine.reset();
            println!(
                "{}	{spotted}	{}",
                Path::new(path).file_name().unwrap_or_default().to_string_lossy(),
                woke.join(" | ")
            );
        }
        return Ok(());
    }
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
        if std::env::var_os("LANG_PROBE_SPOTTER").is_some() {
            let spotter = reever_lib::wake::AcousticWakeSpotter::new(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin"))?;
            engine.set_wake_spotter(Some(Box::new(spotter)));
        }
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
