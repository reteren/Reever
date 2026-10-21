//! Проба «первого ривер после простоя»: повторяет ленивый путь recognizer.rs, когда
//! модели выгружены. Детектор слушает поток, после срабатывания в буфер идёт только
//! pre-roll недавнего звука, пока «грузятся» модели копится остальное, затем всё
//! проигрывается в Engine с mark_spotted_wake. Сравнивается с тёплым путём (модели уже
//! в памяти) и с разной длиной pre-roll.
//!
//! Из src-tauri:
//!   cargo run --release --example cold_wake_probe
//!   cargo run --release --example cold_wake_probe -- --load-ms 3000 --preroll-ms 1000,2000,3000
//!   cargo run --release --example cold_wake_probe -- --english --asr sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8

use std::path::{Path, PathBuf};

use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::Grammar;
use reever_lib::speech::{samples, WakeSpotter};
use reever_lib::vad::SileroVad;
use reever_lib::wake::AcousticWakeSpotter;
use serde::Deserialize;

const CHUNK: usize = 320; // 20 мс
const LEAD_MS: u32 = 1_000;
const TAIL_MS: u32 = 2_500;
const ASR: &str = "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16";

#[derive(Deserialize)]
struct Case {
    file: String,
    scenario: String,
    text: String,
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не прочитан {}", path.display()))?;
    anyhow::ensure!(wave.sample_rate() == 16_000, "{}: нужен 16 кГц", path.display());
    Ok(wave
        .samples()
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
        .collect())
}

#[derive(Default)]
struct Outcome {
    command: Option<String>,
    rejected: bool,
    /// Сигнал «слушаю» или команда — на негативной фразе это ложное срабатывание.
    woke: bool,
    /// Что услышала основная модель в фразах простоя (нужен REEVER_TRACE=1).
    heard: Vec<String>,
}

fn collect(events: Vec<EngineEvent>, out: &mut Outcome) {
    for event in events {
        match event {
            EngineEvent::WakeHeard => out.woke = true,
            EngineEvent::Command { text, .. } => {
                out.woke = true;
                if out.command.is_none() {
                    out.command = Some(text);
                }
            }
            EngineEvent::Log(line) => {
                let line = line.render("en");
                if line.contains("did not confirm") {
                    out.rejected = true;
                }
                if let Some(heard) = line.split('«').nth(1).and_then(|rest| rest.split('»').next()) {
                    if line.starts_with("trace: idle phrase") {
                        out.heard.push(heard.to_string());
                    }
                }
            }
            _ => {}
        }
    }
}

fn feed(engine: &mut Engine<Asr, SileroVad>, audio: &[i16], out: &mut Outcome) {
    for chunk in audio.chunks(CHUNK) {
        let events = engine.push_audio(chunk);
        collect(events, out);
    }
}

/// Тёплый путь: модели в памяти, Engine слышит всё с начала.
fn warm(engine: &mut Engine<Asr, SileroVad>, stream: &[i16]) -> Outcome {
    engine.reset();
    let mut out = Outcome::default();
    feed(engine, stream, &mut out);
    out
}

/// Холодный путь: детектор отдельно, в Engine — pre-roll + звук за время загрузки.
/// None — детектор не сработал вовсе.
fn cold(
    engine: &mut Engine<Asr, SileroVad>,
    spotter: &mut AcousticWakeSpotter,
    stream: &[i16],
    preroll: usize,
    load: usize,
) -> Option<(Outcome, usize)> {
    spotter.reset();
    let mut fired_at = None;
    for (index, chunk) in stream.chunks(CHUNK).enumerate() {
        if spotter.feed(chunk) {
            fired_at = Some((index + 1) * CHUNK);
            break;
        }
    }
    let fired_at = fired_at?.min(stream.len());
    let replay_end = (fired_at + load).min(stream.len());
    let replay = &stream[fired_at.saturating_sub(preroll)..replay_end];

    engine.reset();
    engine.mark_spotted_wake();
    let mut out = Outcome::default();
    collect(engine.push_audio(replay), &mut out);
    feed(engine, &stream[replay_end..], &mut out);
    Some((out, fired_at))
}

fn verdict(out: &Outcome) -> &'static str {
    match (&out.command, out.rejected) {
        (Some(_), _) => "ok",
        (None, true) => "REJECTED",
        (None, false) => "missed",
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let load_ms: u32 = arg("--load-ms").map(|v| v.parse()).transpose()?.unwrap_or(1_000);
    let prerolls: Vec<u32> = arg("--preroll-ms")
        .unwrap_or_else(|| "1000,2000,3000".into())
        .split(',')
        .map(|v| v.trim().parse())
        .collect::<Result<_, _>>()?;
    let verbose = args.iter().any(|value| value == "--verbose");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let testdata = root.join("../testdata");
    let cases: Vec<Case> =
        serde_json::from_str(&std::fs::read_to_string(testdata.join("manifest.json"))?)?;
    let wake_words: Vec<String> = ["ривер", "ревер", "ривьер"].iter().map(|w| w.to_string()).collect();

    let mut engine = Engine::new(
        EngineConfig {
            wake_words: wake_words.clone(),
            partial_interval_ms: 0,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig {
            model_dir: models.join(arg("--asr").unwrap_or_else(|| ASR.into())),
            num_threads: 4,
        })?,
        SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
    );
    engine.set_grammar(Grammar::defaults());
    engine.set_strict_spotted_wake(true);
    // Режим «только английский»: основная модель английская (--asr <Parakeet en>).
    engine.set_english_primary(args.iter().any(|value| value == "--english"));
    engine.set_wake_spotter(Some(Box::new(AcousticWakeSpotter::new(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin"))?)));
    let mut spotter = AcousticWakeSpotter::new(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/wake/river.bin"))?;

    // Живая запись: весь журнал Engine (с REEVER_TRACE=1 — и что услышала GigaAM).
    if let Some(path) = arg("--wav") {
        let mut stream = vec![0i16; samples(LEAD_MS)];
        stream.extend_from_slice(&read_wav(Path::new(&path))?);
        stream.extend(std::iter::repeat(0).take(samples(TAIL_MS)));
        engine.reset();
        for (index, chunk) in stream.chunks(CHUNK).enumerate() {
            for event in engine.push_audio(chunk) {
                let at = (index * CHUNK) as f32 / 16_000.0;
                match event {
                    EngineEvent::Log(line) => println!("{at:6.2}s {}", line.render("en")),
                    EngineEvent::Command { text, alt } => {
                        println!("{at:6.2}s COMMAND «{text}» alt={alt:?}")
                    }
                    EngineEvent::WakeSpotted => println!("{at:6.2}s detector fired"),
                    EngineEvent::WakeHeard => println!("{at:6.2}s WAKE HEARD"),
                    _ => {}
                }
            }
        }
        return Ok(());
    }

    let negative: Vec<&Case> = cases.iter().filter(|case| case.scenario == "negative").collect();
    let positive: Vec<&Case> = cases.iter().filter(|case| case.scenario != "negative").collect();
    let mut warm_ok = 0;
    let mut spotted = 0;
    let mut cold_ok = vec![0usize; prerolls.len()];
    let mut late_ms = Vec::new();
    for case in &positive {
        let speech = read_wav(&testdata.join("corpus").join(&case.file))?;
        let mut stream = vec![0i16; samples(LEAD_MS)];
        stream.extend_from_slice(&speech);
        stream.extend(std::iter::repeat(0).take(samples(TAIL_MS)));

        let warm_out = warm(&mut engine, &stream);
        warm_ok += usize::from(warm_out.command.is_some());

        let mut line = format!("{:<26} warm={:<8}", case.file, verdict(&warm_out));
        for (slot, preroll_ms) in prerolls.iter().enumerate() {
            let Some((out, fired_at)) =
                cold(&mut engine, &mut spotter, &stream, samples(*preroll_ms), samples(load_ms))
            else {
                line.push_str(" detector=silent");
                break;
            };
            if slot == 0 {
                spotted += 1;
                let speech_start_ms = LEAD_MS as usize;
                late_ms.push(fired_at * 1000 / 16_000 - speech_start_ms.min(fired_at * 1000 / 16_000));
                line.push_str(&format!(" fired@{}ms", fired_at * 1000 / 16_000 - LEAD_MS as usize));
            }
            cold_ok[slot] += usize::from(out.command.is_some());
            if slot == 0 && out.command.is_none() && !out.heard.is_empty() {
                line.push_str(&format!(" heard={:?}", out.heard));
            }
            line.push_str(&format!(" pre{}={}", preroll_ms, verdict(&out)));
        }
        if verbose {
            println!("{line}  «{}»", case.text);
        } else {
            println!("{line}");
        }
    }

    // Речь без «ривер»: ни тёплый, ни холодный путь не должны просыпаться.
    let (mut warm_false, mut cold_false, mut cold_spotted) = (0, 0, 0);
    for case in &negative {
        let speech = read_wav(&testdata.join("corpus").join(&case.file))?;
        let mut stream = vec![0i16; samples(LEAD_MS)];
        stream.extend_from_slice(&speech);
        stream.extend(std::iter::repeat(0).take(samples(TAIL_MS)));
        let warm_out = warm(&mut engine, &stream);
        warm_false += usize::from(warm_out.woke);
        let preroll = samples(*prerolls.first().unwrap_or(&1_000));
        if let Some((out, _)) = cold(&mut engine, &mut spotter, &stream, preroll, samples(load_ms)) {
            cold_spotted += 1;
            cold_false += usize::from(out.woke);
            if out.woke || warm_out.woke {
                println!("FALSE WAKE {} warm={} cold={}", case.file, warm_out.woke, out.woke);
            }
        }
    }

    late_ms.sort_unstable();
    println!();
    println!("cases={} detector_fired={spotted} load_ms={load_ms}", positive.len());
    println!("warm: {warm_ok}/{}", positive.len());
    println!(
        "negative={} warm_false={warm_false} detector_fired={cold_spotted} cold_false={cold_false}",
        negative.len()
    );
    for (slot, preroll_ms) in prerolls.iter().enumerate() {
        println!("cold preroll {preroll_ms} ms: {}/{spotted}", cold_ok[slot]);
    }
    if !late_ms.is_empty() {
        let pick = |p: usize| late_ms[(late_ms.len() - 1) * p / 100];
        println!(
            "detector fires after speech start: p50={} ms p90={} ms max={} ms",
            pick(50),
            pick(90),
            pick(100)
        );
    }
    Ok(())
}
