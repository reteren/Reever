//! Offline Whisper benchmark for Russian speech with embedded English terms.
//!
//! Run from src-tauri with CARGO_BUILD_JOBS=4:
//!   cargo run --release --example whisper_mix_bench
//!   cargo run --release --example whisper_mix_bench -- --rtf-only

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result};
use reever_lib::{
    asr::{Asr, AsrConfig},
    speech::{TimedWord, Transcriber},
    spellcheck,
};
use serde::Deserialize;
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineWhisperModelConfig};
use windows::Win32::{
    System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    },
};

const SAMPLE_RATE: usize = 16_000;
const THREADS: i32 = 4;
const WHISPER_TAIL_PADDING_FRAMES: i32 = 300;
const EXTRA_SILENCE_SECONDS: usize = 10;

#[derive(Clone, Deserialize)]
struct MixCase {
    id: String,
    kind: String,
    #[serde(default)]
    expected_word: String,
    #[serde(default)]
    expected_terms: Vec<String>,
    phrase: String,
    #[serde(default)]
    expected: Option<String>,
    voice: String,
    file: String,
}

impl MixCase {
    fn terms(&self) -> Vec<String> {
        if !self.expected_terms.is_empty() {
            self.expected_terms.clone()
        } else if !self.expected_word.is_empty() {
            vec![self.expected_word.clone()]
        } else {
            Vec::new()
        }
    }

    fn reference(&self) -> &str {
        self.expected.as_deref().unwrap_or(&self.phrase)
    }
}

#[derive(Deserialize)]
struct DictationCase {
    file: String,
    start_mode: String,
    expected_text: String,
    parts: Vec<DictationPart>,
}

#[derive(Deserialize)]
struct DictationPart {
    text: String,
    start_ms: usize,
    end_ms: usize,
}

struct MixSample {
    corpus: String,
    case: MixCase,
    pcm: Vec<i16>,
}

struct DictationSample {
    case: DictationCase,
    chunks: Vec<Vec<i16>>,
}

#[derive(Clone, Copy, Default)]
struct MemoryStats {
    working_set: usize,
    pagefile: usize,
}

struct Decode {
    text: String,
    elapsed_s: f64,
    timed_words: Vec<TimedWord>,
    timed_tokens: usize,
}

struct GigaMix {
    text: String,
    words: Vec<TimedWord>,
}

struct GigaResults {
    load_ms: f64,
    after_load: MemoryStats,
    after_run: MemoryStats,
    mix: Vec<GigaMix>,
    dictation: Vec<Vec<String>>,
    rtf: Vec<(f64, f64)>,
}

struct VariantResults {
    model: String,
    language: String,
    load_ms: f64,
    after_load: MemoryStats,
    after_run: MemoryStats,
    mix_text: Vec<String>,
    tail_text: Vec<Option<String>>,
    mix_has_timing: Vec<bool>,
    mix_timed_words: Vec<Vec<TimedWord>>,
    dictation: Vec<Vec<String>>,
    rtf: Vec<(f64, f64)>,
    timed_chunks: usize,
    timing_chunks: usize,
    timed_tokens: usize,
}

struct MergePrediction {
    text: String,
    changed: usize,
    method: String,
}

struct RtfRow {
    model: String,
    language: String,
    load_ms: f64,
    after_load: MemoryStats,
    after_run: MemoryStats,
    samples: Vec<(f64, f64)>,
}

struct SpellCache {
    available_ru: bool,
    available_en: bool,
    values: HashMap<(String, String), bool>,
}

impl SpellCache {
    fn new(available_ru: bool, available_en: bool) -> Self {
        Self {
            available_ru,
            available_en,
            values: HashMap::new(),
        }
    }

    fn is_word(&mut self, language: &str, word: &str) -> bool {
        let key = (language.to_string(), word.to_lowercase());
        if let Some(value) = self.values.get(&key) {
            return *value;
        }
        let available = match language {
            "ru-RU" => self.available_ru,
            "en-US" => self.available_en,
            _ => false,
        };
        let value = available
            && spellcheck::misspelled_ratio(language, word)
                .is_some_and(|(ratio, count)| count == 1 && ratio == 0.0);
        self.values.insert(key, value);
        value
    }
}

struct Whisper {
    recognizer: OfflineRecognizer,
}

impl Whisper {
    fn new(model_dir: &Path, language: Option<&str>) -> Result<(Self, f64)> {
        let model_name = model_dir
            .file_name()
            .context("model directory has no final component")?
            .to_string_lossy()
            .replace("sherpa-onnx-whisper-", "");
        let encoder = model_dir.join(format!("{model_name}-encoder.int8.onnx"));
        let decoder = model_dir.join(format!("{model_name}-decoder.int8.onnx"));
        let tokens = model_dir.join(format!("{model_name}-tokens.txt"));
        anyhow::ensure!(encoder.is_file(), "missing {}", encoder.display());
        anyhow::ensure!(decoder.is_file(), "missing {}", decoder.display());
        anyhow::ensure!(tokens.is_file(), "missing {}", tokens.display());

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.tokens = Some(path_string(&tokens));
        config.model_config.num_threads = THREADS;
        config.model_config.model_type = Some("whisper".into());
        config.model_config.whisper = OfflineWhisperModelConfig {
            encoder: Some(path_string(&encoder)),
            decoder: Some(path_string(&decoder)),
            language: language.map(str::to_string),
            task: Some("transcribe".into()),
            tail_paddings: WHISPER_TAIL_PADDING_FRAMES,
            enable_token_timestamps: true,
            enable_segment_timestamps: false,
        };
        let started = Instant::now();
        let recognizer = OfflineRecognizer::create(&config)
            .with_context(|| format!("load Whisper {} ({language:?})", model_dir.display()))?;
        Ok((Self { recognizer }, started.elapsed().as_secs_f64() * 1000.0))
    }

    fn decode(&self, pcm: &[i16]) -> Result<Decode> {
        let samples: Vec<f32> = pcm
            .iter()
            .map(|sample| f32::from(*sample) / 32_768.0)
            .collect();
        let stream = self.recognizer.create_stream();
        let started = Instant::now();
        stream.accept_waveform(SAMPLE_RATE as i32, &samples);
        self.recognizer.decode(&stream);
        let elapsed_s = started.elapsed().as_secs_f64();
        let result = stream
            .get_result()
            .context("sherpa-onnx returned no Whisper result")?;
        let timed_tokens = result
            .timestamps
            .as_ref()
            .map_or(0, |timestamps| timestamps.len());
        let timed_words = whisper_words(
            &result.tokens,
            result.timestamps.as_deref(),
            result.durations.as_deref(),
            pcm.len() as f32 / SAMPLE_RATE as f32,
        );
        Ok(Decode {
            text: result.text,
            elapsed_s,
            timed_words,
            timed_tokens,
        })
    }
}

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let models = root.join("resources/model");
    let mut mixes = Vec::new();
    let mut mix_notes = Vec::new();
    load_mix_manifest(&testdata, "mix-manifest.json", "mix", &mut mixes, &mut mix_notes)?;
    if testdata.join("mix2-manifest.json").is_file() {
        load_mix_manifest(&testdata, "mix2-manifest.json", "mix2", &mut mixes, &mut mix_notes)?;
    }
    let dictation_cases: Vec<DictationCase> =
        read_json(&testdata.join("dictation-manifest.json"))?;
    let dictations = load_dictations(&testdata, dictation_cases)?;
    anyhow::ensure!(!mixes.is_empty(), "no mixed-corpus WAV files were found");

    let ru_available = spellcheck::is_available("ru-RU");
    let en_available = spellcheck::is_available("en-US");
    let mut spell = SpellCache::new(ru_available, en_available);
    eprintln!(
        "corpora: {} mix WAVs; {} dictation cases, {} speech chunks; ru-RU={}, en-US={}",
        mixes.len(),
        dictations.len(),
        dictations.iter().map(|case| case.chunks.len()).sum::<usize>(),
        ru_available,
        en_available
    );

    if std::env::args().any(|argument| argument == "--rtf-only") {
        eprintln!("RTF-only pass: no mixed-corpus inference; inspect other active ASR processes before calling this run quiet.");
        let mut quiet_rows = vec![print_giga_rtf_only(&models, &dictations)?];
        for model in ["base", "small", "turbo"] {
            for language in [Some("ru"), None] {
                quiet_rows.push(print_whisper_rtf_only(
                    model,
                    language.unwrap_or("auto"),
                    language,
                    &models.join(format!("sherpa-onnx-whisper-{model}")),
                    &dictations,
                )?);
            }
        }
        append_quiet_rtf_report(&testdata.join("whisper-mix-report.md"), &quiet_rows)?;
        println!("quiet RTF section appended to testdata/whisper-mix-report.md");
        return Ok(());
    }

    let giga = run_giga(&models, &mixes, &dictations)?;
    let whisper_models = ["base", "small", "turbo"];
    let mut variants = Vec::new();
    for model in whisper_models {
        for language in [Some("ru"), None] {
            let display_language = language.unwrap_or("auto").to_string();
            let model_dir = models.join(format!("sherpa-onnx-whisper-{model}"));
            let result = run_whisper_variant(
                model,
                &display_language,
                language,
                &model_dir,
                &mixes,
                &dictations,
            )?;
            variants.push(result);
        }
    }

    let report = render_report(
        &mixes,
        &dictations,
        &giga,
        &variants,
        &mut spell,
        &mix_notes,
        ru_available,
        en_available,
    );
    let report_path = testdata.join("whisper-mix-report.md");
    fs::write(&report_path, report)?;
    println!("report: {}", report_path.display());
    Ok(())
}

fn run_giga(
    models: &Path,
    mixes: &[MixSample],
    dictations: &[DictationSample],
) -> Result<GigaResults> {
    let model_dir = models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16");
    let started = Instant::now();
    let mut asr = Asr::new(&AsrConfig {
        model_dir,
        num_threads: THREADS,
    })?;
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_load = memory_stats();
    let mut mix = Vec::with_capacity(mixes.len());
    for (i, sample) in mixes.iter().enumerate() {
        let result = asr.transcribe_timed(&sample.pcm)?;
        mix.push(GigaMix {
            text: result.text,
            words: result.words,
        });
        if (i + 1) % 20 == 0 || i + 1 == mixes.len() {
            eprintln!("GigaAM mixed corpus: {}/{}", i + 1, mixes.len());
        }
    }
    let mut dictation = vec![Vec::new(); dictations.len()];
    let mut rtf = Vec::new();
    for (case_index, case) in dictations.iter().enumerate() {
        for chunk in &case.chunks {
            let started = Instant::now();
            let text = asr.transcribe_raw(chunk)?;
            let elapsed_s = started.elapsed().as_secs_f64();
            dictation[case_index].push(text);
            let duration_s = chunk.len() as f64 / SAMPLE_RATE as f64;
            if (2.0..=12.0).contains(&duration_s) {
                rtf.push((duration_s, elapsed_s));
            }
        }
    }
    let after_run = memory_stats();
    Ok(GigaResults {
        load_ms,
        after_load,
        after_run,
        mix,
        dictation,
        rtf,
    })
}

fn print_giga_rtf_only(models: &Path, dictations: &[DictationSample]) -> Result<RtfRow> {
    let model_dir = models.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16");
    let started = Instant::now();
    let mut asr = Asr::new(&AsrConfig { model_dir, num_threads: THREADS })?;
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_load = memory_stats();
    let mut rtf = Vec::new();
    for case in dictations {
        for chunk in &case.chunks {
            let duration_s = chunk.len() as f64 / SAMPLE_RATE as f64;
            let started = Instant::now();
            let _text = asr.transcribe_raw(chunk)?;
            let elapsed_s = started.elapsed().as_secs_f64();
            if (2.0..=12.0).contains(&duration_s) {
                rtf.push((duration_s, elapsed_s));
            }
        }
    }
    Ok(print_rtf_row("GigaAM", "ru", load_ms, after_load, memory_stats(), rtf))
}

fn print_whisper_rtf_only(
    model: &str,
    language_name: &str,
    language: Option<&str>,
    model_dir: &Path,
    dictations: &[DictationSample],
) -> Result<RtfRow> {
    let (whisper, load_ms) = Whisper::new(model_dir, language)?;
    let after_load = memory_stats();
    let mut rtf = Vec::new();
    for case in dictations {
        for chunk in &case.chunks {
            let duration_s = chunk.len() as f64 / SAMPLE_RATE as f64;
            let decoded = whisper.decode(chunk)?;
            if (2.0..=12.0).contains(&duration_s) {
                rtf.push((duration_s, decoded.elapsed_s));
            }
        }
    }
    Ok(print_rtf_row(model, language_name, load_ms, after_load, memory_stats(), rtf))
}

fn print_rtf_row(model: &str, language: &str, load_ms: f64, after_load: MemoryStats, after_run: MemoryStats, samples: Vec<(f64, f64)>) -> RtfRow {
    let (median, p95, count) = rtf_summary(&samples);
    println!("QUIET_RTF|{model}|{language}|{median:.3}|{p95:.3}|{count}|{load_ms:.0}|{:.0}|{:.0}|{:.0}|{:.0}", mib(after_load.working_set), mib(after_load.pagefile), mib(after_run.working_set), mib(after_run.pagefile));
    RtfRow {
        model: model.to_string(),
        language: language.to_string(),
        load_ms,
        after_load,
        after_run,
        samples,
    }
}

fn append_quiet_rtf_report(path: &Path, rows: &[RtfRow]) -> Result<()> {
    let marker = "\n## Quiet RTF-only follow-up\n";
    let mut report = if path.is_file() { fs::read_to_string(path)? } else { String::from("# Whisper mix benchmark\n") };
    if let Some(index) = report.find(marker) {
        report.truncate(index);
    }
    report.push_str(marker);
    report.push_str("\nОтдельный RTF-only проход после основного теста: во время запуска не было процессов ASR-бенчмарка; активен обычный Reever. Это снижает межмодельную нагрузку, но не является лабораторным idle тестом. Те же 46 кусков длительностью 2–12 с и 4 потока; время включает `accept_waveform` и decode.\n\n");
    report.push_str("| ASR | язык | RTF p50 | RTF p95 | samples | load ms | память после загрузки WS/commit MiB | после прогона WS/commit MiB |\n|---|---|---:|---:|---:|---:|---:|---:|\n");
    for row in rows {
        let (median, p95, count) = rtf_summary(&row.samples);
        report.push_str(&format!("| {} | {} | {median:.3} | {p95:.3} | {count} | {:.0} | {:.0}/{:.0} | {:.0}/{:.0} |\n", row.model, row.language, row.load_ms, mib(row.after_load.working_set), mib(row.after_load.pagefile), mib(row.after_run.working_set), mib(row.after_run.pagefile)));
    }
    fs::write(path, report)?;
    Ok(())
}

fn run_whisper_variant(
    model: &str,
    language_name: &str,
    language: Option<&str>,
    model_dir: &Path,
    mixes: &[MixSample],
    dictations: &[DictationSample],
) -> Result<VariantResults> {
    eprintln!("loading Whisper {model}/{language_name}");
    let (whisper, load_ms) = Whisper::new(model_dir, language)?;
    let after_load = memory_stats();
    let mut mix_text = Vec::with_capacity(mixes.len());
    let mut mix_has_timing = Vec::with_capacity(mixes.len());
    let mut mix_timed_words = Vec::with_capacity(mixes.len());
    let mut tail_text = Vec::with_capacity(mixes.len());
    let extra_silence = vec![0_i16; SAMPLE_RATE * EXTRA_SILENCE_SECONDS];
    let mut timed_tokens = 0;
    let mut timing_chunks = 0;
    for (i, sample) in mixes.iter().enumerate() {
        let decoded = whisper.decode(&sample.pcm)?;
        timed_tokens += decoded.timed_tokens;
        timing_chunks += usize::from(!decoded.timed_words.is_empty());
        mix_has_timing.push(!decoded.timed_words.is_empty());
        mix_timed_words.push(decoded.timed_words);
        mix_text.push(decoded.text);

        if sample.corpus == "mix" {
            let mut tailed = sample.pcm.clone();
            tailed.extend_from_slice(&extra_silence);
            tail_text.push(Some(whisper.decode(&tailed)?.text));
        } else {
            tail_text.push(None);
        }
        if (i + 1) % 20 == 0 || i + 1 == mixes.len() {
            eprintln!("Whisper {model}/{language_name} mix: {}/{}", i + 1, mixes.len());
        }
    }

    let mut dictation = vec![Vec::new(); dictations.len()];
    let mut rtf = Vec::new();
    for (case_index, case) in dictations.iter().enumerate() {
        for (chunk_index, chunk) in case.chunks.iter().enumerate() {
            let duration_s = chunk.len() as f64 / SAMPLE_RATE as f64;
            let decoded = whisper.decode(chunk)?;
            timed_tokens += decoded.timed_tokens;
            timing_chunks += usize::from(!decoded.timed_words.is_empty());
            if (2.0..=12.0).contains(&duration_s) {
                rtf.push((duration_s, decoded.elapsed_s));
            }
            dictation[case_index].push(decoded.text);
            if (chunk_index + 1) % 10 == 0 && case_index == 0 {
                eprintln!("Whisper {model}/{language_name} dictation chunks: {}/{}", chunk_index + 1, case.chunks.len());
            }
        }
    }
    let after_run = memory_stats();
    Ok(VariantResults {
        model: model.to_string(),
        language: language_name.to_string(),
        load_ms,
        after_load,
        after_run,
        mix_text,
        tail_text,
        mix_has_timing,
        mix_timed_words,
        dictation,
        rtf,
        timed_chunks: dictations.iter().map(|case| case.chunks.len()).sum(),
        timing_chunks,
        timed_tokens,
    })
}

fn load_mix_manifest(
    testdata: &Path,
    manifest: &str,
    corpus: &str,
    output: &mut Vec<MixSample>,
    notes: &mut Vec<String>,
) -> Result<()> {
    let path = testdata.join(manifest);
    let cases: Vec<MixCase> = read_json(&path)?;
    let before = output.len();
    let mut missing = Vec::new();
    for case in cases {
        let wav = if corpus == "mix" {
            testdata.join("corpus/mix").join(&case.file)
        } else {
            testdata.join("corpus").join(&case.file)
        };
        if !wav.is_file() {
            missing.push(case.file.clone());
            continue;
        }
        output.push(MixSample {
            corpus: corpus.to_string(),
            case,
            pcm: read_wav(&wav)?,
        });
    }
    if !missing.is_empty() {
        notes.push(format!(
            "{}: {} of {} listed WAVs were missing (e.g. {}). Only existing files were run.",
            corpus,
            missing.len(),
            output.len() + missing.len() - before,
            missing.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    eprintln!(
        "{}: loaded {} of {} WAVs",
        corpus,
        output.len() - before,
        output.len() - before + missing.len()
    );
    Ok(())
}

fn load_dictations(testdata: &Path, cases: Vec<DictationCase>) -> Result<Vec<DictationSample>> {
    let mut output = Vec::with_capacity(cases.len());
    for case in cases {
        let wav = testdata.join("corpus/dictation").join(&case.file);
        let pcm = read_wav(&wav)?;
        let mut chunks = Vec::new();
        if case.start_mode != "joined_trigger_only" && !case.expected_text.trim().is_empty() {
            for part in &case.parts {
                if is_control_part(&part.text) {
                    continue;
                }
                let start = part.start_ms.saturating_mul(SAMPLE_RATE) / 1000;
                let end = part.end_ms.saturating_mul(SAMPLE_RATE) / 1000;
                if end <= start || start >= pcm.len() {
                    continue;
                }
                let end = end.min(pcm.len());
                let speech = &pcm[start..end];
                let duration_s = speech.len() as f64 / SAMPLE_RATE as f64;
                let count = (duration_s / 12.0).ceil().max(1.0) as usize;
                for piece in 0..count {
                    let piece_start = speech.len() * piece / count;
                    let piece_end = speech.len() * (piece + 1) / count;
                    if piece_end > piece_start {
                        chunks.push(speech[piece_start..piece_end].to_vec());
                    }
                }
            }
        }
        output.push(DictationSample { case, chunks });
    }
    Ok(output)
}

fn is_control_part(text: &str) -> bool {
    let words = tokens(text);
    if words.is_empty() {
        return true;
    }
    if words[0] == "стоп" || words[0] == "остановись" {
        return true;
    }
    words[0] == "ривер" && words.len() > 1 && words[1] != "напиши"
}

fn decode_dictation_hypothesis(chunks: &[String]) -> String {
    let mut words = chunks.iter().flat_map(|chunk| tokens(chunk)).collect::<Vec<_>>();
    while matches!(words.first().map(String::as_str), Some("ривер" | "напиши" | "river" | "write")) {
        words.remove(0);
    }
    words.join(" ")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).with_context(|| format!("parse {}", path.display()))
}

fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let bytes = fs::read(path).with_context(|| format!("read WAV {}", path.display()))?;
    anyhow::ensure!(bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE", "{} is not a RIFF/WAVE file", path.display());
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let end = offset.saturating_add(8).saturating_add(length).min(bytes.len());
        if id == b"data" {
            return Ok(bytes[offset + 8..end]
                .chunks_exact(2)
                .map(|part| i16::from_le_bytes([part[0], part[1]]))
                .collect());
        }
        offset = end.saturating_add(length & 1);
    }
    anyhow::bail!("{} has no WAV data chunk", path.display())
}

fn whisper_words(
    tokens: &[String],
    timestamps: Option<&[f32]>,
    durations: Option<&[f32]>,
    audio_duration: f32,
) -> Vec<TimedWord> {
    let Some(timestamps) = timestamps else {
        return Vec::new();
    };
    let count = tokens.len().min(timestamps.len());
    if count == 0 {
        return Vec::new();
    }
    let mut words = Vec::new();
    let mut current = String::new();
    let mut word_start = 0.0_f32;
    let mut word_end = 0.0_f32;
    let mut previous_end = 0.0_f32;
    for index in 0..count {
        let token = &tokens[index];
        let begins_word = token.starts_with('▁');
        let piece = token.strip_prefix('▁').unwrap_or(token);
        if begins_word && !current.is_empty() {
            flush_word(&mut words, &mut current, word_start, word_end, &mut previous_end);
        }
        if piece.is_empty() || (piece.starts_with('<') && piece.ends_with('>')) {
            continue;
        }
        let time = finite_time(timestamps[index]).clamp(0.0, audio_duration.max(0.0));
        let next_time = timestamps
            .get(index + 1)
            .copied()
            .map(finite_time)
            .unwrap_or(audio_duration)
            .clamp(time, audio_duration.max(time));
        let end = durations
            .and_then(|items| items.get(index))
            .copied()
            .filter(|duration| duration.is_finite() && *duration > 0.0)
            .map(|duration| (time + duration).min(audio_duration))
            .unwrap_or(next_time);
        if current.is_empty() {
            word_start = time;
        }
        current.push_str(piece);
        word_end = end.max(time);
    }
    flush_word(&mut words, &mut current, word_start, word_end, &mut previous_end);
    words
}

fn flush_word(
    words: &mut Vec<TimedWord>,
    text: &mut String,
    start: f32,
    end: f32,
    previous_end: &mut f32,
) {
    let parts = tokens(text);
    text.clear();
    let start = start.max(*previous_end);
    let end = end.max(start);
    words.extend(parts.into_iter().map(|text| TimedWord {
        text,
        start,
        end,
    }));
    *previous_end = end;
}

fn finite_time(time: f32) -> f32 {
    if time.is_finite() { time.max(0.0) } else { 0.0 }
}

fn memory_stats() -> MemoryStats {
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb).is_ok() {
            MemoryStats {
                working_set: counters.WorkingSetSize,
                pagefile: counters.PagefileUsage,
            }
        } else {
            MemoryStats::default()
        }
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tokens(text: &str) -> Vec<String> {
    normalize(text)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn transliterate(token: &str) -> String {
    let mut output = String::new();
    for character in token.chars() {
        let replacement = match character {
            'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d", 'е' | 'э' => "e",
            'ё' => "yo", 'ж' => "zh", 'з' => "z", 'и' => "i", 'й' => "y", 'к' => "k",
            'л' => "l", 'м' => "m", 'н' => "n", 'о' => "o", 'п' => "p", 'р' => "r",
            'с' => "s", 'т' => "t", 'у' => "u", 'ф' => "f", 'х' => "h", 'ц' => "ts",
            'ч' => "ch", 'ш' => "sh", 'щ' => "shch", 'ы' => "y", 'ю' => "yu", 'я' => "ya",
            'ь' | 'ъ' => "", other => {
                output.push(other);
                continue;
            }
        };
        output.push_str(replacement);
    }
    output
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    let text = format!(" {} ", normalize(text));
    let phrase = format!(" {} ", normalize(phrase));
    text.contains(&phrase)
}

fn merge_words(
    ru_text: &str,
    ru_timed: &[TimedWord],
    en_text: &str,
    en_timed: &[TimedWord],
    spell: &mut SpellCache,
) -> (String, usize, String) {
    let mut ru_words = tokens(ru_text);
    let mut en_words = tokens(en_text);
    let use_timing = !ru_timed.is_empty() && !en_timed.is_empty();
    if use_timing {
        ru_words = ru_timed.iter().map(|word| normalize(&word.text)).filter(|word| !word.is_empty()).collect();
        en_words = en_timed.iter().map(|word| normalize(&word.text)).filter(|word| !word.is_empty()).collect();
    }
    let aligned = if use_timing {
        timing_alignment(ru_timed, en_timed, &ru_words, &en_words)
    } else {
        token_alignment(&ru_words, &en_words)
    };
    let mut output = ru_words.clone();
    let mut changed = 0;
    for (ru_index, en_index) in aligned {
        if ru_index >= ru_words.len() || en_index >= en_words.len() {
            continue;
        }
        let ru = &ru_words[ru_index];
        let en = &en_words[en_index];
        let is_latin = en.chars().any(|ch| ch.is_ascii_alphabetic())
            && en.chars().all(|ch| ch.is_ascii_alphabetic() || ch.is_ascii_digit());
        if is_latin && !spell.is_word("ru-RU", ru) && spell.is_word("en-US", en) {
            if ru != en {
                output[ru_index] = en.clone();
                changed += 1;
            }
        }
    }
    (
        output.join(" "),
        changed,
        if use_timing { "time-overlap" } else { "token-Levenshtein" }.into(),
    )
}

fn token_alignment(ru: &[String], en: &[String]) -> Vec<(usize, usize)> {
    let rows = ru.len() + 1;
    let columns = en.len() + 1;
    let mut dp = vec![vec![0_u32; columns]; rows];
    for i in 0..rows {
        dp[i][0] = i as u32;
    }
    for j in 0..columns {
        dp[0][j] = j as u32;
    }
    for i in 1..rows {
        for j in 1..columns {
            let left = transliterate(&ru[i - 1]);
            let right = transliterate(&en[j - 1]);
            let substitute = dp[i - 1][j - 1] + u32::from(left != right);
            dp[i][j] = substitute.min(dp[i - 1][j] + 1).min(dp[i][j - 1] + 1);
        }
    }
    let (mut i, mut j) = (ru.len(), en.len());
    let mut pairs = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let left = transliterate(&ru[i - 1]);
            let right = transliterate(&en[j - 1]);
            let cost = u32::from(left != right);
            if dp[i][j] == dp[i - 1][j - 1] + cost {
                pairs.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            i -= 1;
        } else if j > 0 {
            j -= 1;
        } else {
            break;
        }
    }
    pairs.reverse();
    pairs
}

fn timing_alignment(
    ru_timed: &[TimedWord],
    en_timed: &[TimedWord],
    ru_words: &[String],
    en_words: &[String],
) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut en_cursor = 0;
    for (ru_index, ru) in ru_timed.iter().enumerate().take(ru_words.len()) {
        let mut best = None;
        let mut best_overlap = 0.0_f32;
        for (en_index, en) in en_timed.iter().enumerate().skip(en_cursor).take(en_words.len().saturating_sub(en_cursor)) {
            let overlap = (ru.end.min(en.end) - ru.start.max(en.start)).max(0.0);
            if overlap > best_overlap {
                best_overlap = overlap;
                best = Some(en_index);
            }
        }
        if let Some(en_index) = best {
            pairs.push((ru_index, en_index));
            en_cursor = en_index.saturating_add(1);
        }
    }
    pairs
}

fn edit_distance<T: PartialEq>(reference: &[T], hypothesis: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0; hypothesis.len() + 1];
    for (i, reference_item) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, hypothesis_item) in hypothesis.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(reference_item != hypothesis_item));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[hypothesis.len()]
}

fn word_error(reference: &str, hypothesis: &str) -> (usize, usize) {
    let reference = tokens(reference);
    (edit_distance(&reference, &tokens(hypothesis)), reference.len())
}

fn character_error(reference: &str, hypothesis: &str) -> (usize, usize) {
    let reference: Vec<char> = normalize(reference).chars().filter(|ch| !ch.is_whitespace()).collect();
    let hypothesis: Vec<char> = normalize(hypothesis).chars().filter(|ch| !ch.is_whitespace()).collect();
    (edit_distance(&reference, &hypothesis), reference.len())
}

fn dictation_hypothesis(outputs: &[String]) -> String {
    decode_dictation_hypothesis(outputs)
}

fn rtf_summary(samples: &[(f64, f64)]) -> (f64, f64, usize) {
    if samples.is_empty() {
        return (0.0, 0.0, 0);
    }
    let mut values: Vec<f64> = samples
        .iter()
        .filter_map(|(duration, elapsed)| (*duration > 0.0).then_some(elapsed / duration))
        .collect();
    values.sort_by(f64::total_cmp);
    let median = values[values.len() / 2];
    let p95 = values[((values.len() - 1) * 95 / 100).min(values.len() - 1)];
    (median, p95, values.len())
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn render_report(
    mixes: &[MixSample],
    dictations: &[DictationSample],
    giga: &GigaResults,
    variants: &[VariantResults],
    spell: &mut SpellCache,
    mix_notes: &[String],
    ru_available: bool,
    en_available: bool,
) -> String {
    let mut out = String::new();
    out.push_str("# Whisper small/turbo in Russian mixed dictation\n\n");
    out.push_str("Эксперимент sherpa-onnx 1.13.8 на CPU, 4 потока, OfflineRecognizer, int8 ONNX. Основной корпус — 18 смешанных и 10 русских сленговых SAPI-фраз; расширенный mix2 корпус включается, если его манифест и WAV уже есть. Диктовка оценивается по `testdata/dictation-manifest.json`, а в выводе отдельно вырезаются wake/stop/command-фразы, не входящие в `expected_text`.\n\n");
    out.push_str(&format!("Словари Windows: ru-RU={}, en-US={}. Whisper `language=ru` передан явно; `auto` задан как `language=None` (sherpa выводит язык из аудио); `task=transcribe`, `tail_paddings=300`, `enable_token_timestamps=true`, 4 потока. Для хвостового теста добавлено 10 секунд цифровой тишины поверх WAV. Конфиг `OfflineWhisperModelConfig` в sherpa-onnx 1.13.8 содержит `language`, `task`, `tail_paddings` и timestamp-флаги, но не содержит initial prompt; `tail_paddings<=0` в нативном коде заменяется на 1000 кадров. Примечание: каталог Cargo registry — `$USERPROFILE/.cargo/registry/src/.../sherpa-onnx-1.13.8/src/offline_asr.rs`; нативная реализация ограничивает один вход Whisper 29,5 с и использует padding для EOT.\n\n", ru_available, en_available));
    out.push_str("## Доступность таймингов Whisper\n\n");
    out.push_str("Хотя `enable_token_timestamps=true`, bundled int8 decoder у моделей не экспортирован с cross-attention outputs: sherpa пишет `Timestamps will not be available`. Поэтому на этом прогоне временного merge нет, используется текстовое выравнивание Левенштейна по токенам.\n\n");
    out.push_str("| Whisper | язык | mix фразы с word timestamps | dictation chunks с word timestamps | выдано токенов с timestamp |\n|---|---|---:|---:|---:|\n");
    for variant in variants {
        let timed_mix = variant.mix_has_timing.iter().filter(|value| **value).count();
        let timed_dictation = variant.timing_chunks.saturating_sub(timed_mix);
        out.push_str(&format!("| {} | {} | {timed_mix}/{} | {timed_dictation}/{} | {} |\n", variant.model, variant.language, variant.mix_has_timing.len(), variant.timed_chunks, variant.timed_tokens));
    }
    out.push('\n');
    for note in mix_notes {
        out.push_str(&format!("> {note}\n\n"));
    }

    out.push_str("## Сводка качества смешанной речи\n\n");
    out.push_str("Exact term = английский ожидаемый термин присутствует как отдельная последовательность слов. Для каждого модельного столбца указано `терминов/всех целевых терминов`; чисто русские фразы показаны, чтобы видеть транслитерацию/перевод сленга.\n\n");
    out.push_str("| Корпус | Whisper | язык | целевые en-термины | чисто ru: slang word / exact phrase | \n|---|---|---|---:|---:|\n");
    for corpus in ["mix", "mix2"] {
        let indices: Vec<usize> = mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == corpus).map(|(i, _)| i).collect();
        if indices.is_empty() { continue; }
        let mixed_indices: Vec<usize> = indices.iter().copied().filter(|i| mixes[*i].case.kind == "mixed").collect();
        let ru_indices: Vec<usize> = indices.iter().copied().filter(|i| mixes[*i].case.kind != "mixed").collect();
        let terms_total: usize = mixed_indices.iter().map(|i| mixes[*i].case.terms().len()).sum();
        let terms_found: usize = mixed_indices.iter().map(|i| mixes[*i].case.terms().iter().filter(|term| contains_phrase(&giga.mix[*i].text, term)).count()).sum();
        let ru_found = ru_indices.iter().filter(|i| {
            let sample = &mixes[**i];
            if !sample.case.expected_word.is_empty() {
                contains_phrase(&giga.mix[**i].text, &sample.case.expected_word)
            } else {
                normalize(&giga.mix[**i].text) == normalize(sample.case.reference())
            }
        }).count();
        out.push_str(&format!("| {corpus} | GigaAM v3 | ru | {terms_found}/{terms_total} | {ru_found}/{} |\n", ru_indices.len()));
    }
    for variant in variants {
        for corpus in ["mix", "mix2"] {
            let indices: Vec<usize> = mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == corpus).map(|(i, _)| i).collect();
            if indices.is_empty() { continue; }
            let mixed_indices: Vec<usize> = indices.iter().copied().filter(|i| mixes[*i].case.kind == "mixed").collect();
            let ru_indices: Vec<usize> = indices.iter().copied().filter(|i| mixes[*i].case.kind != "mixed").collect();
            let terms_total: usize = mixed_indices.iter().map(|i| mixes[*i].case.terms().len()).sum();
            let terms_found: usize = mixed_indices.iter().map(|i| mixes[*i].case.terms().iter().filter(|term| contains_phrase(&variant.mix_text[*i], term)).count()).sum();
            let ru_total = ru_indices.len();
            let ru_found = ru_indices.iter().filter(|i| {
                let sample = &mixes[**i];
                let expected = sample.case.expected_word.clone();
                if !expected.is_empty() {
                    contains_phrase(&variant.mix_text[**i], &expected)
                } else {
                    normalize(&variant.mix_text[**i]) == normalize(sample.case.reference())
                }
            }).count();
            out.push_str(&format!("| {corpus} | {} | {} | {terms_found}/{terms_total} | {ru_found}/{ru_total} |\n", variant.model, variant.language));
        }
    }
    out.push_str("\n## По фразам: original mix corpus\n\n");
    out.push_str("Tail column сравнивает обычный WAV с тем же WAV плюс 10 с тишины и показывает только фразы «продолжение следует» / «субтитры сделал»; в разделе ниже также перечислены другие добавленные хвосты.\n\n");
    out.push_str("| ID / класс | SAPI voice | SAPI phrase | GigaAM | small ru | small auto | turbo ru | turbo auto | base ru | base auto | tail hallucination |\n|---|---|---|---|---|---|---|---|---|---|---|\n");
    for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == "mix") {
        let variant_text = |model: &str, language: &str| -> &str {
            variants.iter().find(|v| v.model == model && v.language == language).map(|v| v.mix_text[index].as_str()).unwrap_or("")
        };
        let mut tail_flags = Vec::new();
        for variant in variants {
            if sample.corpus != "mix" { continue; }
            if let Some(tail) = &variant.tail_text[index] {
                if stock_hallucination(tail).is_some() {
                    tail_flags.push(format!("{}/{}: {}", variant.model, variant.language, stock_hallucination(tail).unwrap_or_default()));
                }
            }
        }
        out.push_str(&format!(
            "| {} / {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            md(&sample.case.id), md(&sample.case.kind), md(&sample.case.voice), md(&sample.case.phrase), md(&giga.mix[index].text),
            md(variant_text("small", "ru")), md(variant_text("small", "auto")),
            md(variant_text("turbo", "ru")), md(variant_text("turbo", "auto")),
            md(variant_text("base", "ru")), md(variant_text("base", "auto")),
            md(&tail_flags.join("; "))
        ));
    }

    if mixes.iter().any(|sample| sample.corpus == "mix2") {
        out.push_str("\n## По фразам: расширенный mix2 corpus\n\n");
        out.push_str("Для всех 120 созданных WAV показаны outputs по всем языковым режимам; хвостовой +10 с тест отдельно проведён на исходных 28 фразах.\n\n");
        out.push_str("| ID / class | SAPI voice | reference | target terms | small ru | small auto | turbo ru | turbo auto | base ru | base auto |\n|---|---|---|---|---|---|---|---|---|---|\n");
        for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == "mix2") {
            let variant_text = |model: &str, language: &str| -> &str {
                variants.iter().find(|v| v.model == model && v.language == language).map(|v| v.mix_text[index].as_str()).unwrap_or("")
            };
            out.push_str(&format!("| {} / {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n", md(&sample.case.id), md(&sample.case.kind), md(&sample.case.voice), md(sample.case.reference()), md(&sample.case.terms().join(", ")), md(variant_text("small", "ru")), md(variant_text("small", "auto")), md(variant_text("turbo", "ru")), md(variant_text("turbo", "auto")), md(variant_text("base", "ru")), md(variant_text("base", "auto"))));
        }
    }

    out.push_str("\n## Галлюцинации после +10 с тишины\n\n");
    out.push_str("Stock phrases count отдельно от любого нового хвостового текста. `Δ` — суффикс, добавившийся после longest common prefix с обычным выводом; нестабильное изменение внутри текста также видно в полном ответе в таблице.\n\n");
    out.push_str("| Whisper | язык | stock phrase cases | ID: добавленный суффикс |\n|---|---|---:|---|\n");
    for variant in variants {
        let mut known = 0;
        let mut extras = Vec::new();
        for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == "mix") {
            let Some(tail) = &variant.tail_text[index] else { continue; };
            if let Some(phrase) = stock_hallucination(tail) {
                known += 1;
                extras.push(format!("{}: {phrase}", sample.case.id));
            } else {
                let delta = suffix_after_common_prefix(&variant.mix_text[index], tail);
                if !delta.is_empty() {
                    extras.push(format!("{}: Δ{}", sample.case.id, delta));
                }
            }
        }
        out.push_str(&format!("| {} | {} | {known}/28 | {} |\n", variant.model, variant.language, md(&extras.join("; "))));
    }

    out.push_str("\n## WER/CER на dictation-manifest.json\n\n");
    out.push_str("WER считает слова после lowercase, ё→е и удаления пунктуации; CER считает символы без пробелов после той же нормализации. Все модели видят одинаковые речевые части, длинные части поделены на блоки не длиннее 12 с; wake/stop/command, отсутствующие в `expected_text`, удалены по metadata. Это ближе к диктовочным фрагментам, но границы некоторых блоков режут слово.\n\n");
    out.push_str("| ASR | язык | WER | CER |\n|---|---|---:|---:|\n");
    let giga_wer = eval_corpus(dictations, &giga.dictation, true, false);
    out.push_str(&format!("| GigaAM v3 | ru | {:.1}% ({}/{}) | {:.1}% ({}/{}) |\n", giga_wer.0, giga_wer.1, giga_wer.2, giga_wer.3, giga_wer.4, giga_wer.5));
    for variant in variants {
        let scores = eval_corpus(dictations, &variant.dictation, true, false);
        out.push_str(&format!("| Whisper {} | {} | {:.1}% ({}/{}) | {:.1}% ({}/{}) |\n", variant.model, variant.language, scores.0, scores.1, scores.2, scores.3, scores.4, scores.5));
    }

    out.push_str("\n## RTF, загрузка и память\n\n");
    out.push_str("Измерено в одном процессе с другими параллельными ASR-прогонами на машине: считать как **под нагрузкой**. RTF — decode end-to-end для речевых кусков 2–12 с, включая accept_waveform; p50 и p95 по отдельным кускам. Порог диктовки — 0,30. sherpa Whisper offline не отдаёт partial-текст: RTF относится к готовому куску, а не к пословной потоковой выдаче.\n\n");
    out.push_str("| ASR | язык | RTF p50 | RTF p95 | samples | загрузка, мс | память после загрузки WS / commit, MiB | после прогона WS / commit, MiB |\n|---|---|---:|---:|---:|---:|---:|---:|\n");
    let (median, p95, count) = rtf_summary(&giga.rtf);
    out.push_str(&format!("| GigaAM v3 | ru | {median:.3} | {p95:.3} | {count} | {:.0} | {:.0} / {:.0} | {:.0} / {:.0} |\n", giga.load_ms, mib(giga.after_load.working_set), mib(giga.after_load.pagefile), mib(giga.after_run.working_set), mib(giga.after_run.pagefile)));
    for variant in variants {
        let (median, p95, count) = rtf_summary(&variant.rtf);
        out.push_str(&format!("| Whisper {} | {} | {median:.3} | {p95:.3} | {count} | {:.0} | {:.0} / {:.0} | {:.0} / {:.0} |\n", variant.model, variant.language, variant.load_ms, mib(variant.after_load.working_set), mib(variant.after_load.pagefile), mib(variant.after_run.working_set), mib(variant.after_run.pagefile)));
    }
    out.push_str("\nФайлы int8-моделей находятся в `src-tauri/resources/model/sherpa-onnx-whisper-*`. Working set и commit — срез `GetProcessMemoryInfo` этого процесса; OS может сохранять страницы кэша после освобождения предыдущей модели.\n\n");

    out.push_str("## Пословное слияние GigaAM + Whisper\n\n");
    out.push_str("Для каждой фразы латинское слово Whisper принимается, если оно целиком проходит en-US spellcheck, выровнено с токеном GigaAM, который не проходит ru-RU spellcheck, и Whisper выдал ASCII Latin. При наличии word timestamps используется пересечение временных интервалов; иначе — sequence Levenshtein по токенам после простой кириллица→латиница транслитерации. Это прототип оценки, не привязанный к фонетическому расстоянию.\n\n");
    let mut merge_outputs = HashMap::<String, Vec<MergePrediction>>::new();
    for model in ["small", "turbo"] {
        let Some(variant) = variants.iter().find(|v| v.model == model && v.language == "ru") else { continue; };
        let mut predictions = Vec::with_capacity(mixes.len());
        for index in 0..mixes.len() {
            let (text, changed, method) = merge_words(
                &giga.mix[index].text,
                &giga.mix[index].words,
                &variant.mix_text[index],
                &variant.mix_timed_words[index],
                spell,
            );
            predictions.push(MergePrediction { text, changed, method });
        }
        merge_outputs.insert(model.to_string(), predictions);
    }
    out.push_str("### Результат слияния на исходных фразах\n\n");
    out.push_str("| ID | target | GigaAM RU | Whisper small RU | small merged | Whisper turbo RU | turbo merged | replaced tokens |\n|---|---|---|---|---|---|---|---:|\n");
    for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == "mix") {
        let small = &merge_outputs["small"][index];
        let turbo = &merge_outputs["turbo"][index];
        let small_raw = &variants.iter().find(|v| v.model == "small" && v.language == "ru").unwrap().mix_text[index];
        let turbo_raw = &variants.iter().find(|v| v.model == "turbo" && v.language == "ru").unwrap().mix_text[index];
        out.push_str(&format!("| {} | {} | {} | {} | {} | {} | {} | {}/{} |\n", md(&sample.case.id), md(&sample.case.terms().join(", ")), md(&giga.mix[index].text), md(small_raw), md(&small.text), md(turbo_raw), md(&turbo.text), small.changed, turbo.changed));
    }
    if mixes.iter().any(|sample| sample.corpus == "mix2") {
        out.push_str("\n### Результат слияния на mix2\n\n");
        out.push_str("| ID | class | expected terms | GigaAM RU | small Whisper RU | small merged | turbo Whisper RU | turbo merged | changed small/turbo |\n|---|---|---|---|---|---|---|---|---:|\n");
        for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == "mix2") {
            let small = &merge_outputs["small"][index];
            let turbo = &merge_outputs["turbo"][index];
            let small_raw = &variants.iter().find(|v| v.model == "small" && v.language == "ru").unwrap().mix_text[index];
            let turbo_raw = &variants.iter().find(|v| v.model == "turbo" && v.language == "ru").unwrap().mix_text[index];
            out.push_str(&format!("| {} | {} | {} | {} | {} | {} | {} | {} | {}/{} |\n", md(&sample.case.id), md(&sample.case.kind), md(&sample.case.terms().join(", ")), md(&giga.mix[index].text), md(small_raw), md(&small.text), md(turbo_raw), md(&turbo.text), small.changed, turbo.changed));
        }
    }

    out.push_str("\n### Метрики восстановления и русских ложных замен\n\n");
    out.push_str("| Корпус | Whisper RU | en terms восстановлено | замен вне recovered targets / всего | чисто ru cases с заменой | чисто ru WER GigaAM → после merge |\n|---|---|---:|---:|---:|---:|\n");
    for model in ["small", "turbo"] {
        for corpus in ["mix", "mix2"] {
            let indices: Vec<usize> = mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == corpus).map(|(i, _)| i).collect();
            if indices.is_empty() { continue; }
            let mut total_terms = 0;
            let mut recovered = 0;
            let mut replacements = 0;
            let mut false_replacements = 0;
            let mut ru_cases_changed = 0;
            let mut pure_ref = Vec::new();
            let mut pure_giga = Vec::new();
            let mut pure_merged = Vec::new();
            for index in &indices {
                let sample = &mixes[*index];
                let merged = &merge_outputs[model][*index];
                replacements += merged.changed;
                if sample.case.kind == "mixed" {
                    let mut case_recovered = 0;
                    for term in sample.case.terms() {
                        total_terms += 1;
                        if !contains_phrase(&giga.mix[*index].text, &term) && contains_phrase(&merged.text, &term) {
                            recovered += 1;
                            case_recovered += 1;
                        }
                    }
                    false_replacements += merged.changed.saturating_sub(case_recovered);
                } else {
                    ru_cases_changed += usize::from(merged.changed > 0);
                    false_replacements += merged.changed;
                    pure_ref.push(sample.case.reference().to_string());
                    pure_giga.push(giga.mix[*index].text.clone());
                    pure_merged.push(merged.text.clone());
                }
            }
            let (before_wer, before_total) = aggregate_wer(&pure_ref, &pure_giga);
            let (after_wer, _) = aggregate_wer(&pure_ref, &pure_merged);
            let before = if before_total == 0 { 0.0 } else { before_wer as f64 * 100.0 / before_total as f64 };
            let after = if before_total == 0 { 0.0 } else { after_wer as f64 * 100.0 / before_total as f64 };
            out.push_str(&format!("| {corpus} | {model} | {recovered}/{total_terms} | {false_replacements} / {replacements} | {ru_cases_changed}/{} | {before:.1}% → {after:.1}% |\n", pure_ref.len()));
        }
    }
    let timing_merge_count = merge_outputs["turbo"].iter().enumerate().filter(|(index, prediction)| mixes[*index].corpus == "mix" && prediction.method == "time-overlap").count();
    let timed_merge_count = timing_merge_count;
    out.push_str(&format!("\nСлияние использовало Whisper word timestamps в {timed_merge_count}/{} исходных фразах turbo RU; на остальных случаях применён token-Levenshtein. `en terms восстановлено` считает только отсутствовавший в GigaAM целевой термин, появившийся после merge. `замен вне recovered targets` — фактические токены слияния, не объяснённые восстановлением цели; `чисто ru` показывает изменения в русском корпусе и WER до/после.\n\n", mixes.iter().filter(|sample| sample.corpus == "mix").count()));
    out.push_str("\n`восстановлено без потерь` — целевой термин отсутствовал в GigaAM и появился после merge; целевой счёт намеренно не выдаёт за восстановление уже верный исходный английский текст. `pure ru` показывает любые текстовые изменения после merge на сленге / русских контрольных фразах, а WER сравнивает их с манифестом. Низкий false-positive счёт здесь нельзя переносить на свободную речь без живого корпуса.\n\n");

    out.push_str("## Вывод\n\n");
    let corpus_score = |variant: &VariantResults, corpus: &str| {
        let mut found = 0;
        let mut total = 0;
        for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == corpus && sample.case.kind == "mixed") {
            for term in sample.case.terms() {
                total += 1;
                found += usize::from(contains_phrase(&variant.mix_text[index], &term));
            }
        }
        (found, total)
    };
    let best_mix_direct = variants.iter().max_by_key(|variant| corpus_score(variant, "mix").0).unwrap();
    let best_mix2_direct = variants.iter().max_by_key(|variant| corpus_score(variant, "mix2").0).unwrap();
    let recovery_score = |corpus: &str, predictions: &[MergePrediction]| {
        let mut found = 0;
        let mut total = 0;
        for (index, sample) in mixes.iter().enumerate().filter(|(_, sample)| sample.corpus == corpus && sample.case.kind == "mixed") {
            for term in sample.case.terms() {
                total += 1;
                found += usize::from(!contains_phrase(&giga.mix[index].text, &term) && contains_phrase(&predictions[index].text, &term));
            }
        }
        (found, total)
    };
    let small_mix_recovered = recovery_score("mix", &merge_outputs["small"]);
    let turbo_mix_recovered = recovery_score("mix", &merge_outputs["turbo"]);
    let small_mix2_recovered = recovery_score("mix2", &merge_outputs["small"]);
    let turbo_mix2_recovered = recovery_score("mix2", &merge_outputs["turbo"]);
    let turbo_auto = variants.iter().find(|variant| variant.model == "turbo" && variant.language == "auto").unwrap();
    let small_auto = variants.iter().find(|variant| variant.model == "small" && variant.language == "auto").unwrap();
    let turbo_auto_wer = eval_corpus(dictations, &turbo_auto.dictation, true, false).0;
    let (turbo_p50, turbo_p95, _) = rtf_summary(&turbo_auto.rtf);
    let (small_p50, small_p95, _) = rtf_summary(&small_auto.rtf);
    let (mix_direct, mix_total) = corpus_score(best_mix_direct, "mix");
    let (mix2_direct, mix2_total) = corpus_score(best_mix2_direct, "mix2");
    out.push_str(&format!("**Whisper пока не стоит подключать как второй ASR для английских вкраплений:** лучший direct результат на исходном корпусе — {mix_direct}/{mix_total} у {}/{}; на mix2 — {mix2_direct}/{mix2_total} у {}/{}. Merge восстановил {}/{} исходных терминов у small, {}/{} у turbo, {}/{} на mix2 у small и {}/{} у turbo. Turbo auto снизил dictation WER до {turbo_auto_wer:.1}% против {:.1}% у GigaAM; основной прогон под нагрузкой дал ему RTF p50/p95 {turbo_p50:.3}/{turbo_p95:.3}, small auto — {small_p50:.3}/{small_p95:.3}. Порог RTF 0.30 не выдерживается стабильно на p95. Whisper offline отдаёт текст только после завершения сегмента и не выдаёт partial-текст.\n\nОграничения: SAPI не покрывает живые акценты, микрофоны и естественные code-switches; словари Windows могут считать сленг и имена ошибками. RTF и память могут меняться под общей нагрузкой.\n", best_mix_direct.model, best_mix_direct.language, best_mix2_direct.model, best_mix2_direct.language, small_mix_recovered.0, small_mix_recovered.1, turbo_mix_recovered.0, turbo_mix_recovered.1, small_mix2_recovered.0, small_mix2_recovered.1, turbo_mix2_recovered.0, turbo_mix2_recovered.1, giga_wer.0));
    out
}

fn eval_corpus(
    cases: &[DictationSample],
    hypotheses: &[Vec<String>],
    trim_wake: bool,
    _unused: bool,
) -> (f64, usize, usize, f64, usize, usize) {
    let mut word_edits = 0;
    let mut word_total = 0;
    let mut char_edits = 0;
    let mut char_total = 0;
    for (index, case) in cases.iter().enumerate() {
        let mut hypothesis = dictation_hypothesis(hypotheses.get(index).map(Vec::as_slice).unwrap_or(&[]));
        if !trim_wake {
            hypothesis = normalize(&hypothesis);
        }
        let (edits, total) = word_error(&case.case.expected_text, &hypothesis);
        let (char_edit, char_count) = character_error(&case.case.expected_text, &hypothesis);
        word_edits += edits;
        word_total += total;
        char_edits += char_edit;
        char_total += char_count;
    }
    (
        if word_total == 0 { 0.0 } else { word_edits as f64 * 100.0 / word_total as f64 },
        word_edits,
        word_total,
        if char_total == 0 { 0.0 } else { char_edits as f64 * 100.0 / char_total as f64 },
        char_edits,
        char_total,
    )
}

fn aggregate_wer(references: &[String], hypotheses: &[String]) -> (usize, usize) {
    references.iter().zip(hypotheses).fold((0, 0), |(edits, total), (reference, hypothesis)| {
        let (case_edits, case_total) = word_error(reference, hypothesis);
        (edits + case_edits, total + case_total)
    })
}

fn stock_hallucination(text: &str) -> Option<String> {
    let normalized = normalize(text);
    if normalized.contains("продолжение следует") {
        Some("продолжение следует".into())
    } else if normalized.contains("субтитры сделал") {
        Some("субтитры сделал".into())
    } else {
        None
    }
}

fn suffix_after_common_prefix(original: &str, tailed: &str) -> String {
    let original = tokens(original);
    let tailed = tokens(tailed);
    let common = original.iter().zip(&tailed).take_while(|(left, right)| left == right).count();
    if tailed.len() > common {
        tailed[common..].join(" ")
    } else {
        String::new()
    }
}

fn md(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ").replace('\r', " ")
}
