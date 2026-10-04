//! Реальный прогон диктовки через Silero VAD, Segmenter и Engine.
//! Корпус: `scripts/gen-corpus.ps1 -Dictation`; отчёт: `testdata/dictation-report.md`.
//! Запуск из `src-tauri/`: `cargo run --release --example eval_dictation`.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use reever_lib::textnorm::normalize;
use reever_lib::{
    asr::{Asr, AsrConfig},
    bilingual::LangMode,
    dictation::{pieces_to_string, Formatter},
    engine::{DictationEndReason, Engine, EngineConfig, EngineEvent},
    grammar::Grammar,
    langid::SpeechLid,
    speech::{LanguageId, Transcriber},
    vad::SileroVad,
};
use serde::Deserialize;

const CHUNK_SAMPLES: usize = 320; // 20 мс при 16 кГц.
const DEFAULT_ASR_MODEL: &str = "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16";
const DEFAULT_SECOND_MODEL: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";
const DEFAULT_LID_MODEL: &str = "sherpa-onnx-whisper-base";

#[derive(Debug, Deserialize)]
struct Scenario {
    file: String,
    scenario: String,
    start_mode: String,
    expected_text: String,
    expected_end: String,
    expected_command: String,
    #[serde(default)]
    pause_check_ms: u32,
    #[serde(default)]
    soft_pause_after_part: i32,
    parts: Vec<Part>,
    pauses: Vec<Pause>,
    speech_ms: u32,
    pause_ms: u32,
    audio_ms: u32,
}

#[derive(Debug, Deserialize)]
struct Part {
    text: String,
    start_ms: u32,
    end_ms: u32,
}

#[derive(Debug, Deserialize)]
struct Pause {
    start_ms: u32,
    end_ms: u32,
    duration_ms: u32,
    after_part: usize,
}

#[derive(Default)]
struct Run {
    starts_ms: Vec<u32>,
    dictation: Vec<DictationChunk>,
    ends: Vec<EndEvent>,
    commands: Vec<CommandEvent>,
    timeouts_ms: Vec<u32>,
    logs: Vec<String>,
    formatted: String,
}

struct DictationChunk {
    at_ms: u32,
    raw: String,
}

struct EndEvent {
    at_ms: u32,
    reason: DictationEndReason,
}

struct CommandEvent {
    at_ms: u32,
    text: String,
    alt: Option<String>,
}

struct Evaluation {
    scenario: Scenario,
    run: Run,
    word_errors: usize,
    reference_words: usize,
    char_errors: usize,
    reference_chars: usize,
    start_ok: bool,
    end_ok: bool,
    command_ok: bool,
    pause_ok: Option<bool>,
    soft_limit_ok: Option<bool>,
    formatted_exact: bool,
    split_fragments: Vec<String>,
    issues: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let testdata = root.join("../testdata");
    let model = arg("--model").unwrap_or_else(|| DEFAULT_ASR_MODEL.into());
    let second_model = arg("--second-model").unwrap_or_else(|| DEFAULT_SECOND_MODEL.into());
    let lid_model = arg("--lid-model").unwrap_or_else(|| DEFAULT_LID_MODEL.into());
    let model_dir = model_path(&models, &model);
    let second_dir = model_path(&models, &second_model);
    let lid_dir = model_path(&models, &lid_model);

    let manifest: Vec<Scenario> = serde_json::from_str(
        &std::fs::read_to_string(testdata.join("dictation-manifest.json")).context(
            "прочитать testdata/dictation-manifest.json; запустите gen-corpus.ps1 -Dictation",
        )?,
    )?;
    anyhow::ensure!(!manifest.is_empty(), "manifest диктовки пуст");

    let model_started = Instant::now();
    let mut engine = Engine::new(
        EngineConfig {
            wake_words: ["ривер", "ревер", "ривьер", "ливер"]
                .iter()
                .map(|word| word.to_string())
                .collect(),
            partial_interval_ms: 0,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig {
            model_dir: model_dir.clone(),
            num_threads: 4,
        })?,
        SileroVad::new(&models.join("silero_vad.onnx"), 0.5)?,
    );

    let second: Option<Box<dyn Transcriber + Send>> = if second_dir.is_dir() {
        match Asr::new(&AsrConfig {
            model_dir: second_dir.clone(),
            num_threads: 4,
        }) {
            Ok(asr) => Some(Box::new(asr)),
            Err(error) => {
                eprintln!("предупреждение: вторая ASR-модель не загрузилась: {error}");
                None
            }
        }
    } else {
        eprintln!(
            "предупреждение: вторая ASR-модель не найдена: {}",
            second_dir.display()
        );
        None
    };
    let lid: Option<Box<dyn LanguageId + Send>> = if second.is_some() && lid_dir.is_dir() {
        match SpeechLid::new(&lid_dir, 4) {
            Ok(lid) => Some(Box::new(lid)),
            Err(error) => {
                eprintln!("предупреждение: LID-модель не загрузилась: {error}");
                None
            }
        }
    } else {
        if second.is_some() {
            eprintln!(
                "предупреждение: LID-модель не найдена: {}",
                lid_dir.display()
            );
        }
        None
    };
    let has_second = second.is_some();
    let has_lid = lid.is_some();
    engine.set_bilingual(second, lid, LangMode::Auto);
    engine.set_grammar(Grammar::defaults());

    let mut evaluations = Vec::with_capacity(manifest.len());
    let mut audio_samples = 0usize;
    let asr_started = Instant::now();
    let trace = std::env::var_os("EVAL_TRACE").is_some();
    for scenario in manifest {
        let path = testdata.join("corpus/dictation").join(&scenario.file);
        let audio = read_wav(&path)?;
        audio_samples += audio.len();
        if trace {
            eprintln!(
                "{} [{}; {} с речи + {} с пауз]",
                scenario.scenario,
                scenario.start_mode,
                scenario.speech_ms as f64 / 1000.0,
                scenario.pause_ms as f64 / 1000.0
            );
        }
        let run = run_scenario(&mut engine, &audio, trace);
        let evaluation = evaluate(scenario, run);
        if trace {
            eprintln!("  -> {}", evaluation.issues.join("; "));
        }
        evaluations.push(evaluation);
    }

    let audio_seconds = audio_samples as f64 / 16_000.0;
    let processing_seconds = asr_started.elapsed().as_secs_f64();
    let model_load_seconds = model_started.elapsed().as_secs_f64();
    // Instant's full-span elapsed value can be unreliable on this Windows
    // runner; the independently measured setup and evaluation spans add up
    // to the Engine run time (excluding Cargo's build/link step).
    let total_seconds = model_load_seconds + processing_seconds;
    let report = render_report(
        &evaluations,
        &model,
        &second_model,
        has_second,
        &lid_model,
        has_lid,
        audio_seconds,
        processing_seconds,
        model_load_seconds,
        total_seconds,
    );
    let report_path = testdata.join("dictation-report.md");
    std::fs::write(&report_path, &report)?;
    println!("{report}");
    eprintln!("отчёт записан: {}", report_path.display());
    Ok(())
}

fn model_path(models: &Path, name: &str) -> PathBuf {
    let path = PathBuf::from(name);
    if path.is_absolute() {
        path
    } else {
        models.join(path)
    }
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не прочитан {}", path.display()))?;
    anyhow::ensure!(
        wave.sample_rate() == 16_000,
        "{}: нужен 16 кГц",
        path.display()
    );
    Ok(wave
        .samples()
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
        .collect())
}

fn run_scenario(engine: &mut Engine<Asr, SileroVad>, audio: &[i16], trace: bool) -> Run {
    engine.reset();
    let mut run = Run::default();
    let mut formatter = Formatter::new();
    let mut formatted_pieces = Vec::new();

    for (index, chunk) in audio.chunks(CHUNK_SAMPLES).enumerate() {
        let at_ms = ((index + 1) * CHUNK_SAMPLES * 1000 / 16_000) as u32;
        for event in engine.push_audio(chunk) {
            if trace {
                eprintln!("  {at_ms:>6} мс  {event:?}");
            }
            match event {
                EngineEvent::DictationStart => run.starts_ms.push(at_ms),
                EngineEvent::Dictation(text) => {
                    formatted_pieces.extend(formatter.format(&text));
                    run.dictation.push(DictationChunk { at_ms, raw: text });
                }
                EngineEvent::DictationEnd(reason) => run.ends.push(EndEvent { at_ms, reason }),
                EngineEvent::Command { text, alt } => {
                    run.commands.push(CommandEvent { at_ms, text, alt });
                }
                EngineEvent::Timeout => run.timeouts_ms.push(at_ms),
                EngineEvent::Log(message) => run.logs.push(message),
                _ => {}
            }
        }
    }
    run.formatted = pieces_to_string(&formatted_pieces);
    run
}

fn evaluate(scenario: Scenario, run: Run) -> Evaluation {
    let expected_words: Vec<String> = normalize(&scenario.expected_text)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let actual_words: Vec<String> = normalize(&run.formatted)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let word_errors = edit_distance(&expected_words, &actual_words);
    let expected_chars: Vec<char> = normalize(&scenario.expected_text).chars().collect();
    let actual_chars: Vec<char> = normalize(&run.formatted).chars().collect();
    let char_errors = edit_distance(&expected_chars, &actual_chars);

    let start_ok = run.starts_ms.len() == 1;
    let end_ok = run.ends.len() == 1
        && run
            .ends
            .first()
            .is_some_and(|end| end_reason_name(end.reason) == scenario.expected_end);
    let command_ok = if scenario.expected_command.is_empty() {
        run.commands.is_empty()
    } else {
        run.commands.len() == 1
            && normalize(&run.commands[0].text) == normalize(&scenario.expected_command)
    };
    let pause_ok = check_long_pause(&scenario, &run);
    let soft_limit_ok = check_soft_limit(&scenario, &run);
    let formatted_exact = format_key(&scenario.expected_text) == format_key(&run.formatted);
    let split_fragments = find_split_fragments(&scenario.expected_text, &run.dictation);

    let mut issues = Vec::new();
    if !start_ok {
        issues.push(format!(
            "ожидался один DictationStart, получено {}",
            run.starts_ms.len()
        ));
    }
    if !end_ok {
        let actual = run
            .ends
            .iter()
            .map(|end| format!("{}@{}мс", end_reason_name(end.reason), end.at_ms))
            .collect::<Vec<_>>();
        issues.push(format!(
            "конец: ожидался {}, получено {:?}",
            scenario.expected_end, actual
        ));
    }
    if !command_ok {
        issues.push(format!(
            "команда: ожидалась {:?}, получено {:?}",
            scenario.expected_command,
            run.commands
                .iter()
                .map(|command| &command.text)
                .collect::<Vec<_>>()
        ));
    }
    if !run.timeouts_ms.is_empty() {
        issues.push(format!(
            "неожиданные EngineEvent::Timeout в {} мс",
            run.timeouts_ms
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if pause_ok == Some(false) {
        issues.push(format!(
            "не продолжилась после паузы {} мс",
            scenario.pause_check_ms
        ));
    }
    if soft_limit_ok == Some(false) {
        issues.push("граница мягких 12 с не проявилась в паузе 300 мс".into());
    }
    if !split_fragments.is_empty() {
        issues.push(format!(
            "похожие на разрезанные слова стыки: {}",
            split_fragments.join(", ")
        ));
    }
    if !formatted_exact {
        issues.push(format!(
            "Formatter не совпал с эталоном (WER {:.1}%, CER {:.1}%; сравнение эталона и результата ниже)",
            percent(word_errors, expected_words.len()),
            percent(char_errors, expected_chars.len())
        ));
    }
    if !run.logs.is_empty() {
        issues.push(format!(
            "Engine записал {} ошибок/предупреждений",
            run.logs.len()
        ));
    }

    Evaluation {
        reference_words: expected_words.len(),
        reference_chars: expected_chars.len(),
        scenario,
        run,
        word_errors,
        char_errors,
        start_ok,
        end_ok,
        command_ok,
        pause_ok,
        soft_limit_ok,
        formatted_exact,
        split_fragments,
        issues,
    }
}

fn check_long_pause(scenario: &Scenario, run: &Run) -> Option<bool> {
    if scenario.pause_check_ms == 0 {
        return None;
    }
    let pause = scenario
        .pauses
        .iter()
        .find(|pause| pause.duration_ms == scenario.pause_check_ms)?;
    let next_part = scenario.parts.get(pause.after_part + 1)?;
    let ended_before_continuation = run
        .ends
        .iter()
        .any(|end| end.at_ms >= pause.start_ms && end.at_ms < next_part.end_ms);
    let resumed = run
        .dictation
        .iter()
        .any(|chunk| chunk.at_ms >= pause.end_ms && chunk.at_ms >= next_part.start_ms);
    Some(!ended_before_continuation && resumed)
}

fn check_soft_limit(scenario: &Scenario, run: &Run) -> Option<bool> {
    if scenario.soft_pause_after_part < 0 {
        return None;
    }
    let part_index = scenario.soft_pause_after_part as usize;
    let part = scenario.parts.get(part_index)?;
    let next_part = scenario.parts.get(part_index + 1)?;
    if part.end_ms.saturating_sub(part.start_ms) < 12_000 {
        return Some(false);
    }
    let event = run.dictation.iter().find(|chunk| {
        chunk.at_ms >= part.start_ms.saturating_add(12_000)
            && chunk.at_ms <= next_part.start_ms.saturating_add(250)
    });
    let intact = event.is_some_and(|chunk| normalize(&chunk.raw) == normalize(&part.text));
    Some(event.is_some() && intact)
}

fn find_split_fragments(expected: &str, chunks: &[DictationChunk]) -> Vec<String> {
    let expected_words: Vec<String> = normalize(expected)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    chunks
        .windows(2)
        .filter_map(|pair| {
            let last = normalize(&pair[0].raw)
                .split_whitespace()
                .last()?
                .to_owned();
            let first = normalize(&pair[1].raw)
                .split_whitespace()
                .next()?
                .to_owned();
            let joined = format!("{last}{first}");
            expected_words
                .iter()
                .any(|word| word == &joined && word != &last && word != &first)
                .then(|| format!("{last}|{first} ({} → {} мс)", pair[0].at_ms, pair[1].at_ms))
        })
        .collect()
}

fn end_reason_name(reason: DictationEndReason) -> &'static str {
    match reason {
        DictationEndReason::Silence => "Silence",
        DictationEndReason::Stopped => "Stopped",
        DictationEndReason::Command => "Command",
        DictationEndReason::Error => "Error",
        DictationEndReason::Cancelled => "Cancelled",
    }
}

fn format_key(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_lowercase()
}

fn edit_distance<T: PartialEq>(left: &[T], right: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_value) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_value) in right.iter().enumerate() {
            current[right_index + 1] = if left_value == right_value {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    evaluations: &[Evaluation],
    model: &str,
    second_model: &str,
    has_second: bool,
    lid_model: &str,
    has_lid: bool,
    audio_seconds: f64,
    processing_seconds: f64,
    model_load_seconds: f64,
    total_seconds: f64,
) -> String {
    let total_word_errors: usize = evaluations.iter().map(|row| row.word_errors).sum();
    let total_words: usize = evaluations.iter().map(|row| row.reference_words).sum();
    let total_char_errors: usize = evaluations.iter().map(|row| row.char_errors).sum();
    let total_chars: usize = evaluations.iter().map(|row| row.reference_chars).sum();
    let starts_ok = evaluations.iter().filter(|row| row.start_ok).count();
    let ends_ok = evaluations.iter().filter(|row| row.end_ok).count();
    let commands_expected = evaluations
        .iter()
        .filter(|row| !row.scenario.expected_command.is_empty())
        .collect::<Vec<_>>();
    let commands_ok = commands_expected
        .iter()
        .filter(|row| row.command_ok)
        .count();
    let pauses = evaluations
        .iter()
        .filter(|row| row.pause_ok.is_some())
        .collect::<Vec<_>>();
    let pauses_ok = pauses
        .iter()
        .filter(|row| row.pause_ok == Some(true))
        .count();
    let soft_checks = evaluations
        .iter()
        .filter(|row| row.soft_limit_ok.is_some())
        .collect::<Vec<_>>();
    let soft_ok = soft_checks
        .iter()
        .filter(|row| row.soft_limit_ok == Some(true))
        .count();
    let formatted_ok = evaluations.iter().filter(|row| row.formatted_exact).count();
    let split_count: usize = evaluations
        .iter()
        .map(|row| row.split_fragments.len())
        .sum();
    let overall_pass = evaluations
        .iter()
        .filter(|row| row.issues.is_empty())
        .count();
    let rtf = processing_seconds / audio_seconds.max(f64::EPSILON);

    let mut report = format!(
        "# Длинная диктовка — прогон реального Engine\n\nДата: {}\n\n\
         Сценариев: {}; пройдено полностью: {overall_pass}/{}. Модель RU: `{model}`; вторая ASR: `{}`; LID: `{}`.\n\n\
         WAV обработано: {:.1} с аудио за {:.1} с вычислений (RTF {:.3}); загрузка моделей {:.1} с, весь прогон {:.1} с.\n\n\
         События: старт `DictationStart` {starts_ok}/{}; ожидаемый конец `DictationEnd` {ends_ok}/{}; команда посреди диктовки {commands_ok}/{}; проверка продолжения после паузы 3–6 с {pauses_ok}/{}; мягкий предел 12 с вокруг паузы 300 мс с сохранением целого текста {soft_ok}/{}.\n\n\
         Суммарный WER: {:.1}% ({total_word_errors}/{total_words} замен/вставок/удалений); CER: {:.1}% ({total_char_errors}/{total_chars}); точное совпадение оформленного текста: {formatted_ok}/{}; подозрительных фрагментов слова на границах событий: {split_count}.\n\n\
         WER/CER рассчитаны после `textnorm::normalize` (регистр и пунктуация не влияют на ошибки текста); `Formatter` проверяется отдельно точным сравнением знаков и переводов строк. Длительность сообщения — синтезированная речь плюс вставленные паузы, без lead-in и конечной тишины.\n\n\
         Английские фрагменты проходят через вторую модель `{}` и LID `{}`.\n\n",
        chrono_like_date(),
        evaluations.len(),
        evaluations.len(),
        if has_second { second_model } else { "отключена (модель не загрузилась)" },
        if has_lid { lid_model } else { "отключён (модель не загрузилась)" },
        audio_seconds,
        processing_seconds,
        rtf,
        model_load_seconds,
        total_seconds,
        evaluations.len(),
        evaluations.len(),
        commands_expected.len(),
        pauses.len(),
        soft_checks.len(),
        percent(total_word_errors, total_words),
        percent(total_char_errors, total_chars),
        evaluations.len(),
        if has_second { "доступны" } else { "недоступны" },
        if has_lid { "доступен" } else { "недоступен" },
    );

    report.push_str("## Сводка по сценариям\n\n");
    report.push_str("| Сценарий | Речь + паузы | Старт | Конец (ожид. → факт) | Command | WER | CER | Длинная пауза | Soft 12 с | Стык слова | Вердикт |\n");
    report.push_str("|---|---:|---|---|---|---:|---:|---|---|---|---|\n");
    for row in evaluations {
        let reason = row
            .run
            .ends
            .last()
            .map(|end| format!("{} @ {} мс", end_reason_name(end.reason), end.at_ms))
            .unwrap_or_else(|| "нет".into());
        let long_pause = row
            .pause_ok
            .map(|ok| if ok { "OK" } else { "FAIL" }.to_string())
            .unwrap_or_else(|| "—".into());
        let soft = row
            .soft_limit_ok
            .map(|ok| if ok { "OK" } else { "FAIL" }.to_string())
            .unwrap_or_else(|| "—".into());
        let boundary = if row.split_fragments.is_empty() {
            "нет"
        } else {
            "подозрение"
        };
        let verdict = if row.issues.is_empty() {
            "PASS"
        } else {
            "FAIL"
        };
        let command_status = if row.scenario.expected_command.is_empty() {
            "—"
        } else if row.command_ok {
            "OK"
        } else {
            "FAIL"
        };
        report.push_str(&format!(
            "| `{}` | {:.1} с | {} | {} → {} | {} | {:.1}% | {:.1}% | {} | {} | {} | {} |\n",
            row.scenario.scenario,
            (row.scenario.speech_ms + row.scenario.pause_ms) as f64 / 1000.0,
            if row.start_ok { "OK" } else { "FAIL" },
            row.scenario.expected_end,
            reason,
            command_status,
            percent(row.word_errors, row.reference_words),
            percent(row.char_errors, row.reference_chars),
            long_pause,
            soft,
            boundary,
            verdict,
        ));
    }

    report.push_str("\n## Детали и провалы\n\n");
    for row in evaluations {
        report.push_str(&format!("### {}\n\n", row.scenario.scenario));
        report.push_str(&format!(
            "- Стиль начала: `{}`; аудио: {:.1} с.\n- DictationStart: {:?}.\n- DictationEnd: {}.\n",
            row.scenario.start_mode,
            row.scenario.audio_ms as f64 / 1000.0,
            row.run.starts_ms,
            row.run
                .ends
                .iter()
                .map(|end| format!("{}@{}мс", end_reason_name(end.reason), end.at_ms))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !row.run.commands.is_empty() {
            report.push_str("- Command: ");
            for command in &row.run.commands {
                report.push_str(&format!(
                    "`{}` at {} мс{}; ",
                    command.text,
                    command.at_ms,
                    command
                        .alt
                        .as_deref()
                        .map(|alt| format!(", alt `{alt}`"))
                        .unwrap_or_default()
                ));
            }
            report.push('\n');
        }
        report.push_str(&format!(
            "- Эталон: `{}`\n- Formatter: `{}`\n- WER {:.1}% ({}/{}); CER {:.1}% ({}/{}).\n",
            inline(&row.scenario.expected_text),
            inline(&row.run.formatted),
            percent(row.word_errors, row.reference_words),
            row.word_errors,
            row.reference_words,
            percent(row.char_errors, row.reference_chars),
            row.char_errors,
            row.reference_chars,
        ));
        if !row.run.dictation.is_empty() {
            report.push_str("- Куски Engine: ");
            report.push_str(
                &row.run
                    .dictation
                    .iter()
                    .map(|chunk| format!("{}мс `{}`", chunk.at_ms, inline(&chunk.raw)))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            report.push_str(".\n");
        }
        if row.scenario.pause_check_ms != 0 {
            report.push_str(&format!(
                "- Продолжение после паузы {} мс: {}.\n",
                row.scenario.pause_check_ms,
                row.pause_ok.map_or("нельзя оценить", |ok| if ok {
                    "OK"
                } else {
                    "FAIL"
                })
            ));
        }
        if row.soft_limit_ok.is_some() {
            let soft_part = row
                .scenario
                .parts
                .get(row.scenario.soft_pause_after_part as usize);
            let soft_event = soft_part.and_then(|part| {
                let next_part = row
                    .scenario
                    .parts
                    .get(row.scenario.soft_pause_after_part as usize + 1)?;
                row.run.dictation.iter().find(|chunk| {
                    chunk.at_ms >= part.start_ms.saturating_add(12_000)
                        && chunk.at_ms <= next_part.start_ms.saturating_add(250)
                })
            });
            report.push_str(&format!(
                "- Soft limit: {} (фрагмент {:.1} с, короткая пауза {}–{} мс, событие @ {} мс; текст целый: {}).\n",
                if row.soft_limit_ok == Some(true) {
                    "OK"
                } else {
                    "FAIL"
                },
                soft_part
                    .map(|part| (part.end_ms.saturating_sub(part.start_ms)) as f64 / 1000.0)
                    .unwrap_or_default(),
                soft_part.map(|part| part.end_ms).unwrap_or_default(),
                row.scenario
                    .parts
                    .get(row.scenario.soft_pause_after_part as usize + 1)
                    .map(|part| part.start_ms)
                    .unwrap_or_default(),
                soft_event.map(|event| event.at_ms).unwrap_or_default(),
                soft_part
                    .zip(soft_event)
                    .is_some_and(|(part, event)| normalize(&part.text) == normalize(&event.raw))
                    .then_some("да")
                    .unwrap_or("нет")
            ));
        }
        if row.split_fragments.is_empty() {
            report.push_str("- Стык событий: явного склеивания двух фрагментов в одно эталонное слово не найдено.\n");
        } else {
            report.push_str(&format!(
                "- Возможное разрезанное слово на границе событий: {}.\n",
                row.split_fragments.join(", ")
            ));
        }
        if row.issues.is_empty() {
            report.push_str("- Провалы: нет.\n\n");
        } else {
            report.push_str("- Провалы/причины: ");
            report.push_str(&row.issues.join("; "));
            if !row.run.logs.is_empty() {
                report.push_str(&format!("; Engine logs: {}", row.run.logs.join(" | ")));
            }
            report.push_str(".\n\n");
        }
    }
    report
}

fn percent(errors: usize, reference: usize) -> f64 {
    if reference == 0 {
        0.0
    } else {
        errors as f64 * 100.0 / reference as f64
    }
}

fn inline(text: &str) -> String {
    text.replace('\n', " ↵ ").replace('\r', "")
}

fn chrono_like_date() -> String {
    // Keep the example dependency-free while making the report reproducible.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| format!("Unix {}", duration.as_secs()))
        .unwrap_or_else(|_| "unknown".into())
}
