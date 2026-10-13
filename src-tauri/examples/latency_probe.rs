//! Задержка «ривер открой X» от конца речи до команды: сколько ждём тишину (аудио-время)
//! и сколько считают модели (настоящее время), каждая отдельно.
//!
//!   cargo run --release --example latency_probe [-- --scenario continuous|wake_pause] [--limit 12]
//!
//! Модели и паузы — как в настройках пользователя по умолчанию (GigaAM, Parakeet,
//! whisper-tiny, wake 600 мс, команда 900 мс).

use std::path::{Path, PathBuf};
use std::time::Instant;

use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::bilingual::LangMode;
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::Grammar;
use reever_lib::langid::SpeechLid;
use reever_lib::speech::{samples, LanguageId, Transcriber};
use reever_lib::vad::SileroVad;
use serde::Deserialize;

const CHUNK: usize = 320;

#[derive(Deserialize)]
struct Case {
    file: String,
    scenario: String,
    text: String,
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не прочитан {}", path.display()))?;
    Ok(wave.samples().iter().map(|s| (s.clamp(-1.0, 1.0) * 32_767.0) as i16).collect())
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|v| v == name).and_then(|i| args.get(i + 1)).cloned();
    let scenario = arg("--scenario").unwrap_or_else(|| "continuous".into());
    let limit: usize = arg("--limit").and_then(|v| v.parse().ok()).unwrap_or(12);
    let wake_pause: u32 = arg("--wake-pause").and_then(|v| v.parse().ok()).unwrap_or(600);
    let command_pause: u32 = arg("--command-pause").and_then(|v| v.parse().ok()).unwrap_or(900);
    let threads: u32 = arg("--threads").and_then(|v| v.parse().ok()).unwrap_or(4);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let testdata = root.join("../testdata");
    let ru = models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16");
    let en = models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8");
    let lid_dir = models.join("sherpa-onnx-whisper-tiny");

    let mut engine = Engine::new(
        EngineConfig {
            wake_words: ["ривер", "ревер", "ривьер", "ливер"].iter().map(|w| w.to_string()).collect(),
            partial_interval_ms: 0,
            wake_pause_ms: wake_pause,
            command_pause_ms: command_pause,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig { model_dir: ru.clone(), num_threads: threads as i32 })?,
        SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
    );
    engine.set_bilingual(
        Some(Box::new(Asr::new(&AsrConfig { model_dir: en.clone(), num_threads: threads as i32 })?)),
        Some(Box::new(SpeechLid::new(&lid_dir, threads as i32)?)),
        LangMode::Auto,
    );
    engine.set_grammar(Grammar::defaults());
    // --no-fast — как было до досрочного конца команды (без целей действий).
    if !args.iter().any(|a| a == "--no-fast") {
        let actions = reever_lib::actions::load_actions(&root.join("resources/actions.json"));
        engine.set_known_targets(
            actions.iter().flat_map(|a| a.phrases.iter().chain(std::iter::once(&a.label)).map(|target| (target.clone(), a.id.clone()))),
        );
    }

    // Отдельные экземпляры — мерить каждую модель на той же фразе.
    let mut ru_asr = Asr::new(&AsrConfig { model_dir: ru, num_threads: threads as i32 })?;
    let mut en_asr = Asr::new(&AsrConfig { model_dir: en, num_threads: threads as i32 })?;
    let mut lid = SpeechLid::new(&lid_dir, threads as i32)?;

    let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(testdata.join("manifest.json"))?)?;
    let picked: Vec<&Case> = cases
        .iter()
        .filter(|c| c.scenario == scenario && c.text.contains("открой"))
        .take(limit)
        .collect();

    println!("файл | текст | тишина до команды, мс | счёт в вызове с командой, мс | GigaAM | Parakeet | LID");
    let (mut sum_wait, mut sum_compute, mut sum_ru, mut sum_en, mut sum_lid, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for case in picked {
        let speech = read_wav(&testdata.join("corpus").join(&case.file))?;
        // Прогрев не нужен: модели уже прогреты прошлыми файлами (первый — с запасом).
        engine.reset();
        let lead = samples(500);
        let mut audio = vec![0i16; lead];
        audio.extend_from_slice(&speech);
        audio.extend(std::iter::repeat_n(0, samples(2_000)));
        let speech_end = lead + speech.len();
        let mut found = None;
        for (index, chunk) in audio.chunks(CHUNK).enumerate() {
            let started = Instant::now();
            let events = engine.push_audio(chunk);
            let compute = ms(started);
            if events.iter().any(|e| matches!(e, EngineEvent::Command { .. })) {
                let at = (index + 1) * CHUNK;
                found = Some(((at as f64 - speech_end as f64) / 16.0, compute));
                break;
            }
        }
        let started = Instant::now();
        let _ = ru_asr.transcribe_timed(&speech);
        let t_ru = ms(started);
        let started = Instant::now();
        let _ = en_asr.transcribe(&speech);
        let t_en = ms(started);
        let started = Instant::now();
        let _ = lid.detect(&speech);
        let t_lid = ms(started);
        match found {
            Some((wait, compute)) => {
                println!("{} | {} | {wait:.0} | {compute:.0} | {t_ru:.0} | {t_en:.0} | {t_lid:.0}", case.file, case.text);
                sum_wait += wait;
                sum_compute += compute;
                sum_ru += t_ru;
                sum_en += t_en;
                sum_lid += t_lid;
                n += 1.0;
            }
            None => println!("{} | {} | нет команды", case.file, case.text),
        }
    }
    if n > 0.0 {
        println!(
            "среднее | | {:.0} | {:.0} | {:.0} | {:.0} | {:.0}",
            sum_wait / n, sum_compute / n, sum_ru / n, sum_en / n, sum_lid / n
        );
    }
    Ok(())
}
