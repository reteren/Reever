//! Безопасный прогон чужих голосов через детектор и Engine без выполнения действий.
//! cargo run --release --example wake_reliability_probe -- --detector-only > probe.jsonl
//! cargo run --release --example wake_reliability_probe -- --model <папка> > probe.jsonl

use std::path::{Path, PathBuf};

use reever_lib::actions::load_actions;
use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::dispatch::route;
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::{set_active_languages, Grammar};
use reever_lib::kws::StreamingTcn;
use reever_lib::speech::{Timed, Transcriber, WakeSpotter};
use reever_lib::vad::SileroVad;
use reever_lib::wake::AcousticWakeSpotter;
use serde::Deserialize;
use serde_json::json;

const CHUNK: usize = 320;

#[derive(Deserialize)]
struct Case {
    speaker: String,
    text: String,
    file: String,
    wake_start: f32,
    wake_end: f32,
}

struct RecordingAsr {
    inner: Asr,
    heard: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

struct SilentAsr;

impl Transcriber for SilentAsr {
    fn transcribe(&mut self, _pcm: &[i16]) -> anyhow::Result<String> {
        Ok(String::new())
    }
}

impl Transcriber for RecordingAsr {
    fn transcribe(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        let text = self.inner.transcribe(pcm)?;
        self.heard.lock().unwrap().push(text.clone());
        Ok(text)
    }

    fn transcribe_timed(&mut self, pcm: &[i16]) -> anyhow::Result<Timed> {
        let timed = self.inner.transcribe_timed(pcm)?;
        self.heard.lock().unwrap().push(timed.text.clone());
        Ok(timed)
    }

    fn transcribe_verbatim(&mut self, pcm: &[i16]) -> anyhow::Result<String> {
        let text = self.inner.transcribe_verbatim(pcm)?;
        self.heard.lock().unwrap().push(text.clone());
        Ok(text)
    }
}

fn wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("cannot read {}", path.display()))?;
    anyhow::ensure!(
        wave.sample_rate() == 16_000,
        "expected 16 kHz: {}",
        path.display()
    );
    Ok(wave
        .samples()
        .iter()
        .map(|&x| (x.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect())
}

fn dbfs(pcm: &[i16]) -> f64 {
    if pcm.is_empty() {
        return -120.0;
    }
    let mean = pcm
        .iter()
        .map(|&x| (x as f64 / 32768.0).powi(2))
        .sum::<f64>()
        / pcm.len() as f64;
    10.0 * mean.max(1e-12).log10()
}

fn detector(
    weights: &Path,
    verify: Option<&Path>,
    pcm: &[i16],
    idle_ms: usize,
) -> anyhow::Result<(Option<usize>, Vec<String>)> {
    let mut spotter = AcousticWakeSpotter::new(weights)?;
    if let Some(verify) = verify {
        spotter = spotter.with_verifier(verify)?;
    }
    let silence = vec![0i16; idle_ms * 16];
    for part in silence.chunks(CHUNK) {
        spotter.feed(part);
        spotter.take_note();
    }
    let mut fired = None;
    let mut notes = Vec::new();
    for (i, part) in pcm.chunks(CHUNK).enumerate() {
        if spotter.feed(part) && fired.is_none() {
            fired = Some((i + 1) * CHUNK);
        }
        if let Some(note) = spotter.take_note() {
            notes.push(note);
        }
    }
    Ok((fired, notes))
}

fn repeat_detector(
    weights: &Path,
    verify: Option<&Path>,
    pcm: &[i16],
    idle_ms: usize,
    repeats: usize,
) -> anyhow::Result<Vec<bool>> {
    let mut spotter = AcousticWakeSpotter::new(weights)?;
    if let Some(verify) = verify {
        spotter = spotter.with_verifier(verify)?;
    }
    for part in vec![0i16; idle_ms * 16].chunks(CHUNK) {
        spotter.feed(part);
    }
    // Одинаковая фаза 10-мс кадров на каждом повторе: иначе длина WAV сама сдвигает кадры.
    let mut aligned = pcm.to_vec();
    aligned.resize(aligned.len().div_ceil(CHUNK) * CHUNK, 0);
    let mut found = Vec::new();
    for _ in 0..repeats {
        let mut fired = false;
        for part in aligned.chunks(CHUNK) {
            fired |= spotter.feed(part);
        }
        found.push(fired);
    }
    Ok(found)
}

fn engine_spot(
    root: &Path,
    weights: &Path,
    verify: Option<&Path>,
    pcm: &[i16],
    strict: bool,
) -> anyhow::Result<(Option<usize>, Vec<String>)> {
    let vad = SileroVad::new(&root.join("resources/model/silero_vad.onnx"), 0.5)?;
    let mut engine = Engine::new(
        EngineConfig {
            partial_interval_ms: 0,
            ..EngineConfig::default()
        },
        SilentAsr,
        vad,
    );
    engine.set_strict_spotted_wake(strict);
    let mut spotter = AcousticWakeSpotter::new(weights)?;
    if let Some(verify) = verify {
        spotter = spotter.with_verifier(verify)?;
    }
    engine.set_wake_spotter(Some(Box::new(spotter)));
    let mut spotted = None;
    let mut logs = Vec::new();
    for (i, part) in pcm.chunks(CHUNK).enumerate() {
        for event in engine.push_audio(part) {
            match event {
                EngineEvent::WakeSpotted if spotted.is_none() => spotted = Some((i + 1) * CHUNK),
                EngineEvent::Log(text) => logs.push(text.render("en")),
                _ => {}
            }
        }
    }
    Ok((spotted, logs))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| {
        args.iter()
            .position(|v| v == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let detector_only = args.iter().any(|v| v == "--detector-only");
    let commands_only = args.iter().any(|v| v == "--commands-only");
    let english_mode = args.iter().any(|v| v == "--english");
    let selected_speaker = arg("--speaker");
    let idle_ms: usize = arg("--idle-ms")
        .map(|x| x.parse())
        .transpose()?
        .unwrap_or(1_000);
    let gain: f64 = arg("--gain").map(|x| x.parse()).transpose()?.unwrap_or(1.0);
    let repeats: usize = arg("--repeat").map(|x| x.parse()).transpose()?.unwrap_or(0);
    let data_root = arg("--data-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../tools/kws/data"));
    let model =
        arg("--model").unwrap_or_else(|| "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8".into());
    let grammar = Grammar::defaults();
    let actions = load_actions(&root.join("resources/actions.json"));
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut engine = if detector_only {
        None
    } else {
        let asr = Asr::new(&AsrConfig {
            model_dir: root.join("resources/model").join(&model),
            num_threads: 2,
        })?;
        let vad = SileroVad::new(&root.join("resources/model/silero_vad.onnx"), 0.5)?;
        let mut engine = Engine::new(
            EngineConfig {
                partial_interval_ms: 0,
                ..EngineConfig::default()
            },
            RecordingAsr {
                inner: asr,
                heard: heard.clone(),
            },
            vad,
        );
        engine.set_grammar(grammar.clone());
        engine.set_strict_spotted_wake(true);
        // recognizer.rs enables this even when Parakeet v3 also covers Russian/French.
        engine.set_english_primary(true);
        Some(engine)
    };
    for folder in ["friend-ru-uk", "friend-ru2", "friend-fr"] {
        let dir = data_root.join(folder);
        for line in std::fs::read_to_string(dir.join("clips.jsonl"))?.lines() {
            let case: Case = serde_json::from_str(line)?;
            if commands_only && !case.file.contains("-cmd-") {
                continue;
            }
            if selected_speaker
                .as_deref()
                .is_some_and(|s| s != case.speaker)
            {
                continue;
            }
            let languages = if english_mode {
                &["en"][..]
            } else if folder == "friend-fr" {
                &["en", "fr"][..]
            } else if case.speaker == "friend-uk" {
                &["en", "uk"][..]
            } else {
                &["en", "ru"][..]
            };
            set_active_languages(languages);
            heard.lock().unwrap().clear();
            let path = dir.join(&case.file);
            let mut pcm = wav(&path)?;
            let level = dbfs(&pcm);
            let lo = (case.wake_start * 16_000.0) as usize;
            let hi = ((case.wake_end * 16_000.0) as usize).min(pcm.len());
            let wake_dbfs = dbfs(pcm.get(lo.min(hi)..hi).unwrap_or(&[]));
            for x in &mut pcm {
                *x = (*x as f64 * gain).clamp(-32768.0, 32767.0) as i16;
            }
            let mut stream = vec![0i16; 16_000];
            stream.extend_from_slice(&pcm);
            stream.extend(vec![0i16; 16_000 * 3]);
            let weights = if folder == "friend-fr" {
                "river.bin"
            } else {
                "river-ru-en.bin"
            };
            let weights = root.join("resources/wake").join(weights);
            let verify =
                (folder != "friend-fr").then(|| root.join("resources/wake/verify-ru-en.bin"));
            let mut scores = Vec::new();
            let mut raw = StreamingTcn::from_path(&weights, 0.99)?;
            raw.feed_scores(
                &stream
                    .iter()
                    .map(|&s| s as f32 / 32768.0)
                    .collect::<Vec<_>>(),
                &mut scores,
            );
            let peak = scores.iter().copied().fold(0.0_f32, f32::max);
            let (mut run, mut longest) = (0, 0);
            for &score in &scores {
                run = if score >= reever_lib::wake::THRESHOLD {
                    run + 1
                } else {
                    0
                };
                longest = longest.max(run);
            }
            let (first, notes_first) = detector(&weights, None, &stream, idle_ms)?;
            let (verified, notes_verify) = detector(&weights, verify.as_deref(), &stream, idle_ms)?;
            let repeat_first = if repeats > 0 {
                repeat_detector(&weights, None, &stream, idle_ms, repeats)?
            } else {
                Vec::new()
            };
            let repeat_verify = if repeats > 0 {
                repeat_detector(&weights, verify.as_deref(), &stream, idle_ms, repeats)?
            } else {
                Vec::new()
            };
            let (spotted_at, spot_logs) =
                engine_spot(&root, &weights, verify.as_deref(), &stream, true)?;
            let (warm_spotted_at, _) =
                engine_spot(&root, &weights, verify.as_deref(), &stream, false)?;
            let (mut wake_heard, mut rejected, mut commands, mut logs) =
                (false, false, Vec::new(), Vec::new());
            let mut dictation = false;
            if let (Some(engine), Some(at)) = (engine.as_mut(), spotted_at) {
                engine.reset();
                engine.set_wake_spotter(None);
                engine.mark_spotted_wake();
                // recognizer.rs хранит примерно секунду до триггера и весь звук за время загрузки.
                // Здесь модель уже загружена, но буфер воспроизводится тем же вызовом Engine.
                let from = at.saturating_sub(16_000);
                for part in stream[from..].chunks(CHUNK) {
                    for event in engine.push_audio(part) {
                        match event {
                            EngineEvent::WakeSpotted => {}
                            EngineEvent::WakeHeard => wake_heard = true,
                            EngineEvent::WakeRejected => rejected = true,
                            EngineEvent::Command { text, alt } => commands.push((text, alt)),
                            EngineEvent::DictationStart => dictation = true,
                            EngineEvent::Log(text) => logs.push(text.render("en")),
                            _ => {}
                        }
                    }
                }
            }
            let cold_asr = heard.lock().unwrap().clone();
            let routes: Vec<String> = commands
                .iter()
                .map(|(text, alt)| {
                    format!("{:?}", route(text, alt.as_deref(), &grammar, &actions, 0.7))
                })
                .collect();
            let executable = routes.iter().any(|r| {
                !r.starts_with("Unknown") && !r.starts_with("NoAction") && !r.starts_with("Dictate")
            });
            let (mut warm_wake, mut warm_rejected, mut warm_commands, mut warm_dictation) =
                (false, false, Vec::new(), false);
            let mut warm_asr = Vec::new();
            if let Some(engine) = engine.as_mut() {
                engine.reset();
                // Модель держится в памяти (keep_asr_loaded по умолчанию): recognizer.rs
                // выключает строгое подтверждение, «ривер» ищется в тексте каждой фразы.
                engine.set_strict_spotted_wake(false);
                let mut spotter = AcousticWakeSpotter::new(&weights)?;
                if let Some(verify) = verify.as_deref() {
                    spotter = spotter.with_verifier(verify)?;
                }
                engine.set_wake_spotter(Some(Box::new(spotter)));
                heard.lock().unwrap().clear();
                for part in stream.chunks(CHUNK) {
                    for event in engine.push_audio(part) {
                        match event {
                            EngineEvent::WakeHeard => warm_wake = true,
                            EngineEvent::WakeRejected => warm_rejected = true,
                            EngineEvent::Command { text, .. } => warm_commands.push(text),
                            EngineEvent::DictationStart => warm_dictation = true,
                            _ => {}
                        }
                    }
                }
                warm_asr = heard.lock().unwrap().clone();
                engine.set_strict_spotted_wake(true);
            }
            let text = json!({
                "speaker":case.speaker, "file":format!("{folder}/{}",case.file), "reference":case.text,
                "kind":if case.file.contains("-cmd-") { "command" } else { "wake" },
                "dbfs":level, "wake_dbfs":wake_dbfs, "gain":gain,
                "peak_score":peak, "longest_run":longest,
                "stage1":first.is_some(), "stage1_ms":first.map(|n| n*1000/16000),
                "verified":verified.is_some(), "verified_ms":verified.map(|n| n*1000/16000),
                "stage1_notes":notes_first, "verify_notes":notes_verify,
                "repeat_stage1":repeat_first, "repeat_verify":repeat_verify,
                "engine_spotted":spotted_at.is_some(), "engine_spotted_ms":spotted_at.map(|n| n*1000/16000),
                "warm_spotted":warm_spotted_at.is_some(), "spot_logs":spot_logs,
                "wake_heard":wake_heard, "wake_rejected":rejected,
                "asr":cold_asr,
                "commands":commands, "dictation":dictation,
                "routes":routes, "routable":executable, "logs":logs
                ,"warm_model_wake":warm_wake,"warm_model_rejected":warm_rejected,
                "warm_model_commands":warm_commands,"warm_model_dictation":warm_dictation,"warm_model_asr":warm_asr
            });
            println!("{text}");
        }
    }
    Ok(())
}
