//! Qwen3-ASR versus GigaAM for Russian dictation with embedded English terms.
//!
//! Run from src-tauri with CARGO_BUILD_JOBS=4:
//!   cargo run --release --example qwen_mix_bench
//!   cargo run --release --example qwen_mix_bench -- --mix2-only
//!   cargo run --release --example qwen_mix_bench -- --speed-only
//!
//! Writes only ../testdata/qwen-mix-report.md. The Qwen model is configured
//! directly so this probe can evaluate the sherpa-onnx Qwen hotwords option.

use std::{
    collections::HashMap,
    ffi::c_void,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{ensure, Context};
use reever_lib::{
    asr::{Asr, AsrConfig},
    speech::Transcriber,
    textnorm,
};
use serde::Deserialize;
use sherpa_onnx::{OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig, Wave};

const SAMPLE_RATE: i32 = 16_000;
const THREADS: i32 = 4;
const QWEN_MODEL: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";
const GIGA_MODEL: &str = "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16";

#[derive(Clone, Deserialize)]
struct MixCase {
    id: String,
    kind: String,
    expected_word: String,
    phrase: String,
    voice: String,
    file: String,
}

#[derive(Deserialize)]
struct Mix2Case {
    id: String,
    kind: String,
    expected: String,
    expected_terms: Vec<String>,
    voice: String,
    file: String,
}

#[derive(Deserialize)]
struct DictationCase {
    file: String,
    scenario: String,
    expected_text: String,
}

#[derive(Deserialize)]
struct MainCase {
    file: String,
    scenario: String,
    text: String,
    expected_lang: String,
}

#[derive(Clone)]
struct Decode {
    text: String,
    seconds: f64,
}

struct MixRow {
    case: MixCase,
    qwen: Decode,
    hinted: Decode,
    giga: Decode,
}

struct Mix2Row {
    case: Mix2Case,
    qwen: Decode,
    hinted: Decode,
    giga: Decode,
}

#[derive(Clone)]
struct AccuracyRow {
    file: String,
    scenario: String,
    reference: String,
    seconds: f64,
    qwen: Decode,
    giga: Decode,
}

#[derive(Default, Clone, Copy)]
struct Metric {
    word_edits: usize,
    ref_words: usize,
    char_edits: usize,
    ref_chars: usize,
    exact: usize,
    cases: usize,
}

struct MemorySample {
    working: usize,
    peak_working: usize,
    private: usize,
}

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--mix2-only") {
        return run_mix2_only(&root);
    }
    if args.iter().any(|arg| arg == "--speed-only") {
        return run_speed_only(&root);
    }
    let corpus = testdata.join("corpus");
    let model_root = root.join("resources/model");
    let mix: Vec<MixCase> = read_json(&testdata.join("mix-manifest.json"))?;
    let dictation: Vec<DictationCase> = read_json(&testdata.join("dictation-manifest.json"))?;
    let main: Vec<MainCase> = read_json(&testdata.join("manifest.json"))?;
    ensure!(!mix.is_empty(), "mix-manifest.json is empty");
    ensure!(!dictation.is_empty(), "dictation-manifest.json is empty");
    ensure!(!main.is_empty(), "manifest.json is empty");

    let hotwords = glossary(&mix);
    println!("threads={THREADS}; Qwen hotwords={hotwords}");

    let qwen_path = model_root.join(QWEN_MODEL);
    let load_started = Instant::now();
    let qwen = create_qwen(&qwen_path, THREADS)?;
    let qwen_load_seconds = load_started.elapsed().as_secs_f64();
    let qwen_memory_loaded = memory_sample();
    println!(
        "Qwen loaded in {qwen_load_seconds:.2}s; memory={}",
        memory_text(&qwen_memory_loaded)
    );

    let mut mix_rows = Vec::with_capacity(mix.len());
    for (index, case) in mix.iter().enumerate() {
        let audio = read_pcm(&corpus.join("mix").join(&case.file))?;
        let (qwen_text, qwen_seconds) = qwen_decode(&qwen, &audio, None)?;
        let (hinted_text, hinted_seconds) = qwen_decode(&qwen, &audio, Some(&hotwords))?;
        mix_rows.push(MixRow {
            case: case.clone(),
            qwen: Decode {
                text: qwen_text,
                seconds: qwen_seconds,
            },
            hinted: Decode {
                text: hinted_text,
                seconds: hinted_seconds,
            },
            giga: Decode {
                text: String::new(),
                seconds: 0.0,
            },
        });
        println!("Qwen mix {}/{} {}", index + 1, mix.len(), case.id);
    }

    let mut dict_rows = Vec::with_capacity(dictation.len());
    for (index, case) in dictation.iter().enumerate() {
        let audio = read_pcm(&corpus.join("dictation").join(&case.file))?;
        let duration = audio.len() as f64 / f64::from(SAMPLE_RATE);
        let (text, seconds) = qwen_decode(&qwen, &audio, None)?;
        dict_rows.push(AccuracyRow {
            file: case.file.clone(),
            scenario: case.scenario.clone(),
            reference: textnorm::normalize(&case.expected_text),
            seconds: duration,
            qwen: Decode { text, seconds },
            giga: Decode {
                text: String::new(),
                seconds: 0.0,
            },
        });
        println!(
            "Qwen dictation {}/{} {}",
            index + 1,
            dictation.len(),
            case.scenario
        );
    }

    let mut main_rows = Vec::with_capacity(main.len());
    for (index, case) in main.iter().enumerate() {
        if case.expected_lang != "ru" {
            continue;
        }
        let audio = read_pcm(&corpus.join(&case.file))?;
        let duration = audio.len() as f64 / f64::from(SAMPLE_RATE);
        let (text, seconds) = qwen_decode(&qwen, &audio, None)?;
        main_rows.push(AccuracyRow {
            file: case.file.clone(),
            scenario: case.scenario.clone(),
            reference: textnorm::normalize(&case.text),
            seconds: duration,
            qwen: Decode { text, seconds },
            giga: Decode {
                text: String::new(),
                seconds: 0.0,
            },
        });
        println!("Qwen main {}/{} {}", index + 1, main.len(), case.file);
    }

    let speed_audio = read_pcm(&corpus.join("dictation").join(&dictation[0].file))?;
    let mut speed_rows = Vec::new();
    for seconds in [2_usize, 4, 6, 8, 10, 12] {
        let start = 2 * SAMPLE_RATE as usize;
        let end = start + seconds * SAMPLE_RATE as usize;
        ensure!(
            end <= speed_audio.len(),
            "dictation clip too short for {seconds}s benchmark"
        );
        let chunk = &speed_audio[start..end];
        let mut timings = Vec::with_capacity(3);
        let mut last_text = String::new();
        for _ in 0..3 {
            let (text, elapsed) = qwen_decode(&qwen, chunk, None)?;
            timings.push(elapsed);
            last_text = text;
        }
        timings.sort_by(f64::total_cmp);
        speed_rows.push((seconds, timings[1], timings[1] / seconds as f64, last_text));
    }

    let mut noise_rows = Vec::new();
    for seconds in [2_usize, 5, 10] {
        let silence = vec![0_i16; seconds * SAMPLE_RATE as usize];
        let (plain, _) = qwen_decode(&qwen, &silence, None)?;
        let (hinted, _) = qwen_decode(&qwen, &silence, Some(&hotwords))?;
        noise_rows.push(NoiseRow::new(
            format!("тишина {seconds} с"),
            silence,
            plain,
            hinted,
        ));
    }
    for dbfs in [-45_i32, -30] {
        let noise = make_noise(5, dbfs);
        let (plain, _) = qwen_decode(&qwen, &noise, None)?;
        let (hinted, _) = qwen_decode(&qwen, &noise, Some(&hotwords))?;
        noise_rows.push(NoiseRow::new(
            format!("белый шум 5 с, RMS {dbfs} dBFS"),
            noise,
            plain,
            hinted,
        ));
    }
    let qwen_memory_after = memory_sample();
    drop(qwen);

    let giga_path = model_root.join(GIGA_MODEL);
    let giga_load_started = Instant::now();
    let mut giga = Asr::new(&AsrConfig {
        model_dir: giga_path,
        num_threads: THREADS,
    })?;
    let giga_load_seconds = giga_load_started.elapsed().as_secs_f64();
    let giga_memory_loaded = memory_sample();
    println!(
        "GigaAM loaded in {giga_load_seconds:.2}s; memory={}",
        memory_text(&giga_memory_loaded)
    );

    for row in &mut mix_rows {
        let audio = read_pcm(&corpus.join("mix").join(&row.case.file))?;
        row.giga = giga_decode(&mut giga, &audio)?;
    }
    for row in &mut dict_rows {
        let audio = read_pcm(&corpus.join("dictation").join(&row.file))?;
        row.giga = giga_decode(&mut giga, &audio)?;
    }
    for row in &mut main_rows {
        let audio = read_pcm(&corpus.join(&row.file))?;
        row.giga = giga_decode(&mut giga, &audio)?;
    }
    for row in &mut noise_rows {
        row.giga = giga_decode(&mut giga, &row.pcm)?.text;
    }
    let giga_memory_after = memory_sample();
    drop(giga);

    let report = render_report(
        &mix_rows,
        &dict_rows,
        &main_rows,
        main.len(),
        &speed_rows,
        &noise_rows,
        &hotwords,
        qwen_load_seconds,
        giga_load_seconds,
        &qwen_memory_loaded,
        &qwen_memory_after,
        &giga_memory_loaded,
        &giga_memory_after,
    );
    let report_path = testdata.join("qwen-mix-report.md");
    fs::write(&report_path, report)?;
    println!("report: {}", report_path.display());
    if !args.iter().any(|arg| arg == "--skip-mix2")
        && testdata.join("mix2-manifest.json").is_file()
        && corpus.join("mix2-001-m2-hardness-1.wav").is_file()
    {
        run_mix2_only(&root)?;
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

fn create_qwen(model_dir: &Path, threads: i32) -> anyhow::Result<OfflineRecognizer> {
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = threads.max(1);
    config.model_config.tokens = Some(String::new());
    config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
        conv_frontend: Some(
            model_dir
                .join("conv_frontend.onnx")
                .to_string_lossy()
                .into_owned(),
        ),
        encoder: Some(
            model_dir
                .join("encoder.int8.onnx")
                .to_string_lossy()
                .into_owned(),
        ),
        decoder: Some(
            model_dir
                .join("decoder.int8.onnx")
                .to_string_lossy()
                .into_owned(),
        ),
        tokenizer: Some(model_dir.join("tokenizer").to_string_lossy().into_owned()),
        ..Default::default()
    };
    OfflineRecognizer::create(&config).context("sherpa-onnx failed to create Qwen3-ASR recognizer")
}

fn qwen_decode(
    recognizer: &OfflineRecognizer,
    pcm: &[i16],
    hotwords: Option<&str>,
) -> anyhow::Result<(String, f64)> {
    let started = Instant::now();
    let samples: Vec<f32> = pcm
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect();
    let stream = recognizer.create_stream();
    if let Some(hotwords) = hotwords {
        stream.set_option("hotwords", hotwords);
    }
    stream.accept_waveform(SAMPLE_RATE, &samples);
    recognizer.decode(&stream);
    let result = stream
        .get_result()
        .context("Qwen stream returned no result")?;
    Ok((
        textnorm::normalize(&result.text),
        started.elapsed().as_secs_f64(),
    ))
}

fn giga_decode(recognizer: &mut Asr, pcm: &[i16]) -> anyhow::Result<Decode> {
    let started = Instant::now();
    let text = recognizer.transcribe(pcm)?;
    Ok(Decode {
        text,
        seconds: started.elapsed().as_secs_f64(),
    })
}

fn read_pcm(path: &Path) -> anyhow::Result<Vec<i16>> {
    let name = path.to_string_lossy();
    let wave = Wave::read(&name).with_context(|| format!("read WAV {}", path.display()))?;
    ensure!(
        wave.sample_rate() == SAMPLE_RATE,
        "{}: expected 16 kHz, got {}",
        path.display(),
        wave.sample_rate()
    );
    Ok(wave
        .samples()
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
        .collect())
}

fn glossary(cases: &[MixCase]) -> String {
    let mut words: Vec<String> = [
        "hardness",
        "opacity",
        "roughness",
        "escape",
        "PNG",
        "eraser",
        "tool",
        "scale",
        "high",
    ]
    .into_iter()
    .map(str::to_string)
    .chain(
        cases
            .iter()
            .filter(|case| case.kind == "mixed")
            .map(|case| case.expected_word.clone()),
    )
    .collect();
    words.sort_by_key(|word| word.to_lowercase());
    words.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    words.join(",")
}

fn mix2_glossary(cases: &[Mix2Case]) -> String {
    let mut words: Vec<String> = [
        "hardness",
        "opacity",
        "roughness",
        "escape",
        "PNG",
        "eraser",
        "tool",
        "scale",
        "high",
    ]
    .into_iter()
    .map(str::to_string)
    .chain(
        cases
            .iter()
            .filter(|case| case.kind == "mixed")
            .flat_map(|case| case.expected_terms.iter().cloned()),
    )
    .collect();
    words.sort_by_key(|word| word.to_lowercase());
    words.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    words.join(",")
}

fn run_mix2_only(root: &Path) -> anyhow::Result<()> {
    let testdata = root.join("../testdata");
    let corpus = testdata.join("corpus");
    let model_root = root.join("resources/model");
    let cases: Vec<Mix2Case> = read_json(&testdata.join("mix2-manifest.json"))?;
    ensure!(!cases.is_empty(), "mix2-manifest.json is empty");
    for case in &cases {
        ensure!(
            corpus.join(&case.file).is_file(),
            "missing mix2 audio: {}",
            case.file
        );
    }
    let mixed_count = cases.iter().filter(|case| case.kind == "mixed").count();
    let russian_count = cases.iter().filter(|case| case.kind == "russian").count();
    let hotwords = mix2_glossary(&cases);

    let qwen_path = model_root.join(QWEN_MODEL);
    let load_started = Instant::now();
    let qwen = create_qwen(&qwen_path, THREADS)?;
    let qwen_load_seconds = load_started.elapsed().as_secs_f64();
    let qwen_memory_loaded = memory_sample();
    println!(
        "mix2: {} cases ({} mixed, {} Russian); Qwen load {:.2}s; glossary={}",
        cases.len(),
        mixed_count,
        russian_count,
        qwen_load_seconds,
        hotwords
    );

    let mut rows = Vec::with_capacity(cases.len());
    for (index, case) in cases.into_iter().enumerate() {
        let audio = read_pcm(&corpus.join(&case.file))?;
        let (qwen_text, qwen_seconds) = qwen_decode(&qwen, &audio, None)?;
        let (hinted_text, hinted_seconds) = qwen_decode(&qwen, &audio, Some(&hotwords))?;
        rows.push(Mix2Row {
            case,
            qwen: Decode {
                text: qwen_text,
                seconds: qwen_seconds,
            },
            hinted: Decode {
                text: hinted_text,
                seconds: hinted_seconds,
            },
            giga: Decode {
                text: String::new(),
                seconds: 0.0,
            },
        });
        if (index + 1) % 10 == 0 || index + 1 == rows.capacity() {
            println!("Qwen mix2 {}/{}", index + 1, rows.capacity());
        }
    }
    let qwen_memory_after = memory_sample();
    drop(qwen);

    let giga_started = Instant::now();
    let mut giga = Asr::new(&AsrConfig {
        model_dir: model_root.join(GIGA_MODEL),
        num_threads: THREADS,
    })?;
    let giga_load_seconds = giga_started.elapsed().as_secs_f64();
    let giga_memory_loaded = memory_sample();
    for row in &mut rows {
        let audio = read_pcm(&corpus.join(&row.case.file))?;
        row.giga = giga_decode(&mut giga, &audio)?;
    }
    let giga_memory_after = memory_sample();
    drop(giga);

    let term_count: usize = rows
        .iter()
        .filter(|row| row.case.kind == "mixed")
        .map(|row| row.case.expected_terms.len())
        .sum();
    let term_hits = |mode: usize| -> usize {
        rows.iter()
            .filter(|row| row.case.kind == "mixed")
            .flat_map(|row| {
                row.case.expected_terms.iter().map(move |term| {
                    let transcript = match mode {
                        0 => &row.qwen.text,
                        1 => &row.hinted.text,
                        _ => &row.giga.text,
                    };
                    usize::from(contains_word(transcript, term))
                })
            })
            .sum()
    };
    let phrase_hits = |mode: usize| -> usize {
        rows.iter()
            .filter(|row| row.case.kind == "mixed")
            .filter(|row| {
                !row.case.expected_terms.is_empty()
                    && row.case.expected_terms.iter().all(|term| {
                        let transcript = match mode {
                            0 => &row.qwen.text,
                            1 => &row.hinted.text,
                            _ => &row.giga.text,
                        };
                        contains_word(transcript, term)
                    })
            })
            .count()
    };
    let qwen_terms = term_hits(0);
    let hinted_terms = term_hits(1);
    let giga_terms = term_hits(2);
    let qwen_phrases = phrase_hits(0);
    let hinted_phrases = phrase_hits(1);
    let giga_phrases = phrase_hits(2);

    let russian: Vec<_> = rows
        .iter()
        .filter(|row| row.case.kind == "russian")
        .collect();
    let mut ru_qwen = Metric::default();
    let mut ru_hinted = Metric::default();
    let mut ru_giga = Metric::default();
    for row in &russian {
        add_metric(&mut ru_qwen, &row.case.expected, &row.qwen.text);
        add_metric(&mut ru_hinted, &row.case.expected, &row.hinted.text);
        add_metric(&mut ru_giga, &row.case.expected, &row.giga.text);
    }
    let false_insertions: Vec<_> = russian
        .iter()
        .filter_map(|row| {
            let terms = contains_any_word(&row.hinted.text, &hotwords);
            (!terms.is_empty()).then_some((*row, terms))
        })
        .collect();
    let changed_russian_tokens: usize = russian
        .iter()
        .map(|row| changed_tokens(&row.qwen.text, &row.hinted.text))
        .sum();
    let qwen_decode_total: f64 = rows.iter().map(|row| row.qwen.seconds).sum();
    let hinted_decode_total: f64 = rows.iter().map(|row| row.hinted.seconds).sum();
    let giga_decode_total: f64 = rows.iter().map(|row| row.giga.seconds).sum();

    let mut report = format!(
        "\n\n## Расширенный mix2-корпус (появился во время прогона)\n\n\
         Манифест: `mix2-manifest.json`; WAV: `testdata/corpus/`; {} случаев: {} mixed с {} целевыми терминами и {} обычных русских фраз для проверки коллизий. Все три транскрипции в таблице. Hotwords включают список из задачи и целевые термины манифеста.\n\
         Модели — CPU, 4 потока; замер выполнен под параллельной нагрузкой. Qwen: первая загрузка {:.2} с, после загрузки {}, после прогона {}. GigaAM: загрузка {:.2} с, после загрузки {}, после прогона {} (текущая WS/private память; процессный peak унаследован от Qwen).\n\n\
         | Метрика | Qwen | Qwen + hotwords | GigaAM |\n|---|---:|---:|---:|\n\
         | Mixed фразы со всеми целевыми терминами в точном латинском написании | {}/{} | {}/{} | {}/{} |\n\
         | Всего точных терминов | {}/{} | {}/{} | {}/{} |\n\
         | WER на {} обычных русских фразах | {:.2}% ({}/{}) | {:.2}% ({}/{}) | {:.2}% ({}/{}) |\n\
         | CER на обычных русских фразах | {:.2}% ({}/{}) | {:.2}% ({}/{}) | {:.2}% ({}/{}) |\n\
         | Glossary false insertion в русских фразах | — | {}/{} | — |\n\n\
         Время decode-вызовов на 120 WAV: Qwen baseline {:.1} с, Qwen+hotwords {:.1} с, GigaAM {:.1} с. Hotwords удалили/изменили {} транскрибированных токенов на русском поднаборе.\n\n\
         ### Транскрипции по всем mix2-фразам\n\n\
         | ID / kind / целевые термины | Эталонная фраза | Голос | GigaAM | Qwen | Qwen + hotwords |\n|---|---|---|---|---|---|\n",
        rows.len(), mixed_count, term_count, russian_count,
        qwen_load_seconds, memory_text(&qwen_memory_loaded), memory_text(&qwen_memory_after),
        giga_load_seconds, memory_current_text(&giga_memory_loaded), memory_current_text(&giga_memory_after),
        qwen_phrases, mixed_count, hinted_phrases, mixed_count, giga_phrases, mixed_count,
        qwen_terms, term_count, hinted_terms, term_count, giga_terms, term_count,
        russian_count,
        pct(ru_qwen.word_edits, ru_qwen.ref_words), ru_qwen.word_edits, ru_qwen.ref_words,
        pct(ru_hinted.word_edits, ru_hinted.ref_words), ru_hinted.word_edits, ru_hinted.ref_words,
        pct(ru_giga.word_edits, ru_giga.ref_words), ru_giga.word_edits, ru_giga.ref_words,
        pct(ru_qwen.char_edits, ru_qwen.ref_chars), ru_qwen.char_edits, ru_qwen.ref_chars,
        pct(ru_hinted.char_edits, ru_hinted.ref_chars), ru_hinted.char_edits, ru_hinted.ref_chars,
        pct(ru_giga.char_edits, ru_giga.ref_chars), ru_giga.char_edits, ru_giga.ref_chars,
        false_insertions.len(), russian_count,
        qwen_decode_total, hinted_decode_total, giga_decode_total, changed_russian_tokens,
    );

    for row in &rows {
        let targets = row.case.expected_terms.join(", ");
        let target_status = if row.case.kind == "mixed" {
            format!(
                "term hit: Qwen {}/{}, hint {}/{}, GigaAM {}/{}",
                row.case
                    .expected_terms
                    .iter()
                    .filter(|term| contains_word(&row.qwen.text, term))
                    .count(),
                row.case.expected_terms.len(),
                row.case
                    .expected_terms
                    .iter()
                    .filter(|term| contains_word(&row.hinted.text, term))
                    .count(),
                row.case.expected_terms.len(),
                row.case
                    .expected_terms
                    .iter()
                    .filter(|term| contains_word(&row.giga.text, term))
                    .count(),
                row.case.expected_terms.len(),
            )
        } else {
            let inserted = contains_any_word(&row.hinted.text, &hotwords);
            if inserted.is_empty() {
                "ordinary Russian: no glossary term inserted".to_string()
            } else {
                format!("FALSE INSERTION: {}", inserted.join(", "))
            }
        };
        report.push_str(&format!(
            "| `{}` / {} / `{}` — {} | {} | {} | {} | {} | {} |\n",
            md(&row.case.id),
            md(&row.case.kind),
            md(&targets),
            md(&target_status),
            md(&row.case.expected),
            md(&row.case.voice),
            md(&row.giga.text),
            md(&row.qwen.text),
            md(&row.hinted.text),
        ));
    }
    report.push_str(&format!(
        "\nGlossary на обычных русских фразах добавил один из целевых английских терминов в **{}/{}** случаев: {}. При этом WER Qwen изменился {:.2}% → {:.2}%, GigaAM {:.2}%; exact-case hits сравнивают всю последовательность токенов термина без учёта регистра.\n",
        false_insertions.len(), russian_count,
        if false_insertions.is_empty() { "ни в одной".to_string() } else { false_insertions.iter().map(|(row, terms)| format!("{} ({})", row.case.id, terms.join(","))).collect::<Vec<_>>().join("; ") },
        pct(ru_qwen.word_edits, ru_qwen.ref_words), pct(ru_hinted.word_edits, ru_hinted.ref_words), pct(ru_giga.word_edits, ru_giga.ref_words),
    ));
    let report_path = testdata.join("qwen-mix-report.md");
    let current_report = fs::read_to_string(&report_path)
        .with_context(|| format!("read report {} before mix2 refresh", report_path.display()))?;
    if let Some((base_report, _)) = current_report.split_once("\n\n## Расширенный mix2-корпус")
    {
        fs::write(&report_path, base_report)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(false)
        .append(true)
        .open(&report_path)
        .with_context(|| format!("open report {} for mix2 append", report_path.display()))?;
    file.write_all(report.as_bytes())?;
    println!("appended mix2 report: {}", report_path.display());
    Ok(())
}

fn run_speed_only(root: &Path) -> anyhow::Result<()> {
    let testdata = root.join("../testdata");
    let model_root = root.join("resources/model");
    let audio = read_pcm(&testdata.join("corpus/dictation/01_01_joined_natural.wav"))?;
    let active_before = active_asr_processes();
    let qwen_started = Instant::now();
    let qwen = create_qwen(&model_root.join(QWEN_MODEL), THREADS)?;
    let load_seconds = qwen_started.elapsed().as_secs_f64();
    let memory_loaded = memory_sample();
    let start = 2 * SAMPLE_RATE as usize;
    let warm = &audio[start..start + 6 * SAMPLE_RATE as usize];
    let _ = qwen_decode(&qwen, warm, None)?;

    let mut results = Vec::new();
    for seconds in [2_usize, 4, 6, 8, 10, 12] {
        let end = start + seconds * SAMPLE_RATE as usize;
        ensure!(
            end <= audio.len(),
            "dictation WAV too short for {seconds}s chunk"
        );
        let chunk = &audio[start..end];
        let mut samples = Vec::with_capacity(3);
        for _ in 0..3 {
            let (_, elapsed) = qwen_decode(&qwen, chunk, None)?;
            samples.push(elapsed);
        }
        samples.sort_by(f64::total_cmp);
        results.push((seconds, samples[1], samples[1] / seconds as f64));
    }
    let memory_after = memory_sample();
    let active_after = active_asr_processes();
    let load_text = format!(
        "прочие ASR-процессы до замера: {}; после: {}",
        process_list_text(&active_before),
        process_list_text(&active_after),
    );
    let mut report = format!(
        "\n\n## Отдельный повтор RTF\n\nДата: {}; {}. Qwen CPU, 4 потока; модель загружена за {:.2} с (WS/private после загрузки: {}), post-run: {}. Источник audio: `01_01_joined_natural.wav`, offset 2 с; один warm-up, median трёх распознаваний на длину.\n\n| Длительность | Median inference | RTF |\n|---:|---:|---:|\n",
        chrono_now(), load_text, load_seconds, memory_current_text(&memory_loaded), memory_text(&memory_after),
    );
    for (seconds, elapsed, rtf) in results {
        report.push_str(&format!("| {seconds} с | {elapsed:.2} с | {rtf:.3} |\n"));
    }
    let report_path = testdata.join("qwen-mix-report.md");
    let mut file = fs::OpenOptions::new()
        .create(false)
        .append(true)
        .open(&report_path)
        .with_context(|| format!("open report {} for speed append", report_path.display()))?;
    file.write_all(report.as_bytes())?;
    println!("appended quiet speed run: {}", report_path.display());
    Ok(())
}

fn make_noise(seconds: usize, dbfs: i32) -> Vec<i16> {
    let rms = 10_f64.powf(f64::from(dbfs) / 20.0);
    let scale = rms * 3.0_f64.sqrt() * 32_767.0;
    let mut state = 0x6d2b_79f5_u32;
    (0..seconds * SAMPLE_RATE as usize)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let unit = (state as f64 / u32::MAX as f64) * 2.0 - 1.0;
            (unit * scale).round().clamp(-32_768.0, 32_767.0) as i16
        })
        .collect()
}

fn contains_word(text: &str, wanted: &str) -> bool {
    let text_words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect();
    let wanted_words: Vec<String> = wanted
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect();
    !wanted_words.is_empty()
        && text_words
            .windows(wanted_words.len())
            .any(|window| window == wanted_words)
}

fn contains_any_word(text: &str, words_csv: &str) -> Vec<String> {
    words_csv
        .split(',')
        .filter(|word| contains_word(text, word))
        .map(str::to_string)
        .collect()
}

fn changed_tokens(before: &str, after: &str) -> usize {
    let mut available = HashMap::<String, usize>::new();
    for word in after.split_whitespace().map(str::to_lowercase) {
        *available.entry(word).or_default() += 1;
    }
    before
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|word| match available.get_mut(word) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .count()
}

fn edit_distance<T: Eq>(left: &[T], right: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut current = vec![i + 1; right.len() + 1];
        for (j, b) in right.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(a != b));
        }
        previous = current;
    }
    previous[right.len()]
}

fn score(reference: &str, hypothesis: &str) -> (usize, usize, usize, usize, bool) {
    let reference = textnorm::normalize(reference);
    let hypothesis = textnorm::normalize(hypothesis);
    let reference_words: Vec<&str> = reference.split_whitespace().collect();
    let hypothesis_words: Vec<&str> = hypothesis.split_whitespace().collect();
    let reference_chars: Vec<char> = reference.chars().collect();
    let hypothesis_chars: Vec<char> = hypothesis.chars().collect();
    (
        edit_distance(&reference_words, &hypothesis_words),
        reference_words.len(),
        edit_distance(&reference_chars, &hypothesis_chars),
        reference_chars.len(),
        reference == hypothesis,
    )
}

fn add_metric(metric: &mut Metric, reference: &str, hypothesis: &str) {
    let (word_edits, ref_words, char_edits, ref_chars, exact) = score(reference, hypothesis);
    metric.word_edits += word_edits;
    metric.ref_words += ref_words;
    metric.char_edits += char_edits;
    metric.ref_chars += ref_chars;
    metric.exact += usize::from(exact);
    metric.cases += 1;
}

fn pct(errors: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        errors as f64 * 100.0 / total as f64
    }
}

fn fmt_metric(metric: Metric) -> String {
    format!(
        "WER {:.2}% ({}/{}), CER {:.2}% ({}/{}), exact {}/{}",
        pct(metric.word_edits, metric.ref_words),
        metric.word_edits,
        metric.ref_words,
        pct(metric.char_edits, metric.ref_chars),
        metric.char_edits,
        metric.ref_chars,
        metric.exact,
        metric.cases,
    )
}

fn md(value: &str) -> String {
    if value.trim().is_empty() {
        return "∅".into();
    }
    value
        .replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', " ")
}

fn fmt_edit_rate(errors: usize, reference_units: usize) -> String {
    if reference_units == 0 {
        format!("n/a ({errors} insertions; empty reference)")
    } else {
        format!(
            "{:.2}% ({errors}/{reference_units})",
            pct(errors, reference_units)
        )
    }
}

fn ratio(errors: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        errors as f64 / total as f64
    }
}

fn render_report(
    mix: &[MixRow],
    dictation: &[AccuracyRow],
    main: &[AccuracyRow],
    main_manifest_total: usize,
    speed: &[(usize, f64, f64, String)],
    noise: &[NoiseRow],
    hotwords: &str,
    qwen_load: f64,
    giga_load: f64,
    qwen_loaded: &MemorySample,
    qwen_after: &MemorySample,
    giga_loaded: &MemorySample,
    giga_after: &MemorySample,
) -> String {
    let mixed: Vec<&MixRow> = mix.iter().filter(|row| row.case.kind == "mixed").collect();
    let slang: Vec<&MixRow> = mix
        .iter()
        .filter(|row| row.case.kind == "russian_slang")
        .collect();
    let qwen_terms = mixed
        .iter()
        .filter(|row| contains_word(&row.qwen.text, &row.case.expected_word))
        .count();
    let hint_terms = mixed
        .iter()
        .filter(|row| contains_word(&row.hinted.text, &row.case.expected_word))
        .count();
    let giga_terms = mixed
        .iter()
        .filter(|row| contains_word(&row.giga.text, &row.case.expected_word))
        .count();
    let qwen_slang = slang
        .iter()
        .filter(|row| contains_word(&row.qwen.text, &row.case.expected_word))
        .count();
    let hint_slang = slang
        .iter()
        .filter(|row| contains_word(&row.hinted.text, &row.case.expected_word))
        .count();
    let giga_slang = slang
        .iter()
        .filter(|row| contains_word(&row.giga.text, &row.case.expected_word))
        .count();
    let false_terms: Vec<_> = slang
        .iter()
        .filter_map(|row| {
            let inserted = contains_any_word(&row.hinted.text, hotwords);
            (!inserted.is_empty()).then_some((row, inserted))
        })
        .collect();
    let changed_slang_tokens: usize = slang
        .iter()
        .map(|row| changed_tokens(&row.qwen.text, &row.hinted.text))
        .sum();

    let dict_qwen = sum_metrics(dictation, false);
    let dict_giga = sum_metrics(dictation, true);
    let dict_ru: Vec<_> = dictation
        .iter()
        .filter(|row| !row.reference.chars().any(|c| c.is_ascii_alphabetic()))
        .cloned()
        .collect();
    let dict_ru_qwen = sum_metrics(&dict_ru, false);
    let dict_ru_giga = sum_metrics(&dict_ru, true);
    let main_qwen = sum_metrics(main, false);
    let main_giga = sum_metrics(main, true);
    let qwen_decode_seconds: f64 = dictation
        .iter()
        .chain(main)
        .map(|row| row.qwen.seconds)
        .sum();
    let giga_decode_seconds: f64 = dictation
        .iter()
        .chain(main)
        .map(|row| row.giga.seconds)
        .sum();

    let mut output = format!(
        "# Qwen3-ASR-0.6B как модель диктовки для смешанной речи\n\n\
         Дата прогона: {}; CPU: {}; corpus WAV: 16 kHz mono PCM16; CPU provider; 4 потока для обеих моделей.\n\
         Замеры выполнялись при параллельных задачах на общей машине; скорость помечена как «под нагрузкой».\n\n\
         ## Краткий результат\n\n\
         - В mix-корпусе Qwen без подсказки точно написал целевой английский термин в **{qwen_terms}/{}** фразах, Qwen с hotwords — **{hint_terms}/{}**, GigaAM — **{giga_terms}/{}**.\n\
         - Из русских сленговых целевых слов сохранено: Qwen **{qwen_slang}/{}**, Qwen+hotwords **{hint_slang}/{}**, GigaAM **{giga_slang}/{}**; hotwords вставили термин из глоссария в **{}/{}** сленговых фраз, изменив/удалив **{}** исходных токенов суммарно.\n\
         - На длинной диктовке: Qwen {}; GigaAM {}.\n\
         - На `manifest.json` (русскоязычные эталоны): Qwen {}; GigaAM {}.\n\
         - Рекомендация: **{}**.\n\n\
         ## Конфигурация Qwen и ресурсы\n\n\
         Локальный `OfflineQwen3ASRModelConfig` из sherpa-onnx 1.13.8 задаёт `conv_frontend`, `encoder`, `decoder`, `tokenizer`, `max_total_len`, `max_new_tokens`, `temperature`, `top_p`, `seed` и `hotwords`. Отдельных `system_prompt`, `user_prompt`, `context` или `language` полей в этой структуре нет. Проверен per-stream `hotwords` через `OfflineStream::set_option(\"hotwords\", ...)`; список передавался CSV: `{}`.\n\
         Первая загрузка Qwen: {:.2} с; после загрузки: {}; после corpus/decode: {}. Память Qwen после полного прогона — его собственный high-water mark. GigaAM: загрузка {:.2} с; после загрузки: {}; после corpus/decode: {}. Для GigaAM показаны текущие working set/private bytes; peak рабочей памяти процесса нельзя отделить от предыдущего Qwen-прогона.\n\n\
         ## Точность русского текста\n\n\
         | Corpus | Qwen | GigaAM |\n|---|---|---|\n\
         | `dictation-manifest.json` (все {} сценариев) | {} | {} |\n\
         | Только эталоны без латиницы ({} сценариев) | {} | {} |\n\
         | `manifest.json` (`expected_lang=ru`, {} из {} записей) | {} | {} |\n\n\
         CER/WER посчитаны по `textnorm::normalize`: WER — word-level edit distance / число слов эталона, CER — character-level distance / число символов. `manifest.json` сравнивается с полем `text` входного эталона; для `dictation-manifest.json` — с `expected_text`.\n\n\
         Сумма времени вызовов распознавания на этих двух наборах (не время WAV I/O): Qwen {:.1} с, GigaAM {:.1} с.\n\n\
         ### Длинная диктовка по сценарию\n\n\
         | Сценарий | WAV | Audio | Qwen WER | Qwen CER | GigaAM WER | GigaAM CER |\n|---|---|---:|---:|---:|---:|---:|\n",
        chrono_now(), cpu_name(), mixed.len(), mixed.len(), mixed.len(), slang.len(), slang.len(), slang.len(),
        false_terms.len(), slang.len(), changed_slang_tokens,
        fmt_metric(dict_qwen), fmt_metric(dict_giga), fmt_metric(main_qwen), fmt_metric(main_giga),
        recommendation(qwen_terms, hint_terms, mixed.len(), qwen_slang, hint_slang, slang.len(), false_terms.len()),
        md(hotwords), qwen_load, memory_text(qwen_loaded), memory_text(qwen_after), giga_load,
        memory_current_text(giga_loaded), memory_current_text(giga_after),
        dictation.len(), fmt_metric(dict_qwen), fmt_metric(dict_giga), dict_ru.len(), fmt_metric(dict_ru_qwen), fmt_metric(dict_ru_giga),
        main.len(), main_manifest_total, fmt_metric(main_qwen), fmt_metric(main_giga), qwen_decode_seconds, giga_decode_seconds,
    );

    for row in dictation {
        let (qw, qn, qc, qchars, _) = score(&row.reference, &row.qwen.text);
        let (gw, gn, gc, gchars, _) = score(&row.reference, &row.giga.text);
        output.push_str(&format!(
            "| `{}` | `{}` | {:.1} с | {} | {} | {} | {} |\n",
            md(&row.scenario),
            md(&row.file),
            row.seconds,
            fmt_edit_rate(qw, qn),
            fmt_edit_rate(qc, qchars),
            fmt_edit_rate(gw, gn),
            fmt_edit_rate(gc, gchars),
        ));
    }

    output.push_str("\n## Mix-корпус: термин в русской фразе и сленг\n\n| ID / тип / эталон | Фраза | Голос | GigaAM | Qwen | Qwen + glossary | Статус |\n|---|---|---|---|---|---|---|\n");
    for row in mix {
        let target = contains_word(&row.qwen.text, &row.case.expected_word);
        let hint_target = contains_word(&row.hinted.text, &row.case.expected_word);
        let giga_target = contains_word(&row.giga.text, &row.case.expected_word);
        let status = if row.case.kind == "mixed" {
            format!(
                "точный термин: GigaAM {}, Qwen {}, hint {}",
                yesno(giga_target),
                yesno(target),
                yesno(hint_target)
            )
        } else {
            let inserted = contains_any_word(&row.hinted.text, hotwords);
            format!(
                "сленг: GigaAM {}, Qwen {}, hint {}; вставлено: {}",
                yesno(giga_target),
                yesno(target),
                yesno(hint_target),
                if inserted.is_empty() {
                    "—".into()
                } else {
                    inserted.join(", ")
                }
            )
        };
        output.push_str(&format!(
            "| `{}` / {} / `{}` | {} | {} | {} | {} | {} | {} |\n",
            md(&row.case.id),
            md(&row.case.kind),
            md(&row.case.expected_word),
            md(&row.case.phrase),
            md(&row.case.voice),
            md(&row.giga.text),
            md(&row.qwen.text),
            md(&row.hinted.text),
            md(&status),
        ));
    }

    output.push_str(&format!("\nГлоссарий точно распознан Qwen: **{qwen_terms}/{}**; с подсказкой: **{hint_terms}/{}**; GigaAM: **{giga_terms}/{}**. Целевые сленговые слова сохранены: Qwen **{qwen_slang}/{}**, Qwen+hotwords **{hint_slang}/{}**, GigaAM **{giga_slang}/{}**. На сленговых фразах glossary-термин появился без соответствующего mixed-высказывания в **{}/{}** случаях: {}. Относительно Qwen без подсказки hotwords удалили/изменили **{}** исходных токенов сленга.\n",
        mixed.len(), mixed.len(), mixed.len(), slang.len(), slang.len(), slang.len(), false_terms.len(), slang.len(),
        if false_terms.is_empty() { "ни в одной".to_string() } else { false_terms.iter().map(|(row, terms)| format!("{} ({})", row.case.id, terms.join(","))).collect::<Vec<_>>().join("; ") },
        changed_slang_tokens,
    ));

    output.push_str("\n## Основной manifest: худшие Qwen-примеры\n\n| Файл / сценарий | Эталон | Qwen | GigaAM | Qwen WER | GigaAM WER |\n|---|---|---|---|---:|---:|\n");
    let mut worst: Vec<_> = main.iter().collect();
    worst.sort_by(|a, b| {
        let ae = score(&a.reference, &a.qwen.text).0;
        let be = score(&b.reference, &b.qwen.text).0;
        ratio(be, score(&b.reference, &b.qwen.text).1)
            .total_cmp(&ratio(ae, score(&a.reference, &a.qwen.text).1))
    });
    for row in worst.into_iter().take(10) {
        let (qw, qn, _, _, _) = score(&row.reference, &row.qwen.text);
        let (gw, gn, _, _, _) = score(&row.reference, &row.giga.text);
        output.push_str(&format!(
            "| `{}` / `{}` | {} | {} | {} | {:.1}% | {:.1}% |\n",
            md(&row.file),
            md(&row.scenario),
            md(&clip(&row.reference, 140)),
            md(&clip(&row.qwen.text, 140)),
            md(&clip(&row.giga.text, 140)),
            pct(qw, qn),
            pct(gw, gn),
        ));
    }

    output.push_str("\n## Скорость Qwen на чанках 2–12 секунд\n\nRTF — median трёх измерений после прогрева; chunks взяты из речи `dictation-manifest` с offset 2 с. Все значения — под параллельной нагрузкой.\n\n| Длительность | Median inference | RTF | Последний транскрипт |\n|---:|---:|---:|---|\n");
    for (seconds, elapsed, rtf, text) in speed {
        output.push_str(&format!(
            "| {seconds} с | {elapsed:.2} с | {rtf:.3} | {} |\n",
            md(text)
        ));
    }

    output.push_str("\n## Тишина и шум\n\nШум сгенерирован детерминированно как uniform white noise с указанным RMS. Проверен и Qwen, и glossary-вариант; GigaAM прогнан на тех же PCM данных. Пустая транскрипция обозначена `∅`.\n\n| Сигнал | Qwen | Qwen + glossary | GigaAM | Вставлены glossary термины |\n|---|---|---|---|---|\n");
    for row in noise {
        let inserted = contains_any_word(&row.hinted, hotwords);
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            md(&row.label),
            md(&row.plain),
            md(&row.hinted),
            md(&row.giga),
            if inserted.is_empty() {
                "нет".into()
            } else {
                inserted.join(", ")
            }
        ));
    }

    output.push_str("\n## Итоговая рекомендация\n\n");
    output.push_str(&recommendation_long(
        qwen_terms,
        hint_terms,
        mixed.len(),
        qwen_slang,
        hint_slang,
        slang.len(),
        false_terms.len(),
        dict_qwen,
        dict_giga,
        main_qwen,
        main_giga,
    ));
    output.push_str("\n\n## Методика и ограничения\n\n- Qwen и GigaAM запускались последовательно, по 4 CPU threads, чтобы снять память каждой модели отдельно. Для Qwen baseline и glossary использовался один offline recognizer; glossary передавался per stream через CSV `hotwords`.\n- Список подсказки включал `hardness`, `opacity`, `roughness`, `escape`, `PNG`, `eraser`, `tool`, `scale`, `high` и все mixed-термины из manifest. Сленговые кейсы не участвовали в построении списка.\n- Тихий повтор вынесен в режим `--speed-only`; запускайте его после завершения других ASR-бенчмарков. Режим записывает список процессов до и после измерения.\n- `max_total_len` по умолчанию равен 512; на длинных диктовках sherpa-onnx сообщил о превышении и обрезал audio placeholders (в логе `audio_token_len=642 -> keep_audio=497`). Это ограничение может объяснять часть провала на длинных сценариях.\n- Точное совпадение термина требует совпадения всей последовательности токенов термина после удаления пунктуации, без учёта регистра; транслитерация не засчитывается.\n- WAV-выборка синтетическая SAPI; результаты не заменяют проверку живого микрофона.\n- Точность на основном `manifest.json` оценивает распознавание всего русского эталона `text`, включая wake-слово; dictation manifest сравнивает полный эталон `expected_text`.\n");
    output
}

fn sum_metrics(rows: &[AccuracyRow], giga: bool) -> Metric {
    let mut metric = Metric::default();
    for row in rows {
        add_metric(
            &mut metric,
            &row.reference,
            if giga { &row.giga.text } else { &row.qwen.text },
        );
    }
    metric
}

fn recommendation(
    qwen_terms: usize,
    hinted_terms: usize,
    mixed_count: usize,
    qwen_slang: usize,
    hinted_slang: usize,
    slang_count: usize,
    false_insertions: usize,
) -> &'static str {
    if qwen_terms * 2 >= mixed_count && qwen_slang * 5 >= slang_count * 4 && false_insertions == 0 {
        "Qwen как вторая модель для mixed-кластеров: baseline достигает приемлемого recall терминов без потери сленга"
    } else if hinted_terms * 2 >= mixed_count
        && hinted_slang * 5 >= slang_count * 4
        && false_insertions == 0
    {
        "Qwen+hotwords как вторая модель для mixed-кластеров: подсказка повышает recall минимум до половины фраз без потери сленга"
    } else {
        "не годится как основная модель или автоматическая вторая модель: точных mixed-терминов слишком мало; пока только экспериментальная ручная проверка отдельного фрагмента"
    }
}

fn recommendation_long(
    qwen_terms: usize,
    hinted_terms: usize,
    mixed_count: usize,
    qwen_slang: usize,
    hinted_slang: usize,
    slang_count: usize,
    false_insertions: usize,
    dict_qwen: Metric,
    dict_giga: Metric,
    main_qwen: Metric,
    main_giga: Metric,
) -> String {
    let mut result = recommendation(
        qwen_terms,
        hinted_terms,
        mixed_count,
        qwen_slang,
        hinted_slang,
        slang_count,
        false_insertions,
    )
    .to_string();
    if ratio(dict_qwen.word_edits, dict_qwen.ref_words)
        > ratio(dict_giga.word_edits, dict_giga.ref_words) + 0.05
        || ratio(main_qwen.word_edits, main_qwen.ref_words)
            > ratio(main_giga.word_edits, main_giga.ref_words) + 0.05
    {
        result.push_str("; русская точность уступает GigaAM более чем на 5 процентных пунктов, поэтому переводить основную диктовку на Qwen нельзя");
    }
    result
}

fn yesno(value: bool) -> &'static str {
    if value {
        "да"
    } else {
        "нет"
    }
}

fn clip(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn chrono_now() -> String {
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz'",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn cpu_name() -> String {
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| std::env::consts::ARCH.to_string())
}

fn active_asr_processes() -> Vec<String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Process | Where-Object { $_.ProcessName -match 'bench|eval|qwen|whisper|parakeet|sherpa' -and $_.ProcessName -ne 'qwen_mix_bench' } | Select-Object -ExpandProperty ProcessName",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

fn process_list_text(names: &[String]) -> String {
    if names.is_empty() {
        "none detected".to_string()
    } else {
        names.join(", ")
    }
}

fn memory_text(sample: &MemorySample) -> String {
    format!(
        "WS {:.0} MiB / peak {:.0} MiB / private {:.0} MiB",
        mib(sample.working),
        mib(sample.peak_working),
        mib(sample.private)
    )
}

fn memory_current_text(sample: &MemorySample) -> String {
    format!(
        "WS {:.0} MiB / private {:.0} MiB",
        mib(sample.working),
        mib(sample.private)
    )
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(windows)]
fn memory_sample() -> MemorySample {
    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_nonpaged_pool_usage: usize,
        quota_nonpaged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCountersEx {
        cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_nonpaged_pool_usage: 0,
        quota_nonpaged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    let cb = counters.cb;
    let success = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, cb) };
    if success == 0 {
        return MemorySample {
            working: 0,
            peak_working: 0,
            private: 0,
        };
    }
    MemorySample {
        working: counters.working_set_size,
        peak_working: counters.peak_working_set_size,
        private: counters.private_usage,
    }
}

#[cfg(not(windows))]
fn memory_sample() -> MemorySample {
    MemorySample {
        working: 0,
        peak_working: 0,
        private: 0,
    }
}

struct NoiseRow {
    label: String,
    pcm: Vec<i16>,
    plain: String,
    hinted: String,
    giga: String,
}

impl NoiseRow {
    fn new(label: String, pcm: Vec<i16>, plain: String, hinted: String) -> Self {
        Self {
            label,
            pcm,
            plain,
            hinted,
            giga: String::new(),
        }
    }
}
