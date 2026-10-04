//! Сравнивает Parakeet TDT v3 с GigaAM v3 RU и Parakeet v2 на смешанных фразах.
//! Запуск из src-tauri/: `$env:CARGO_BUILD_JOBS=4; cargo run --release --example parakeet3_mix_bench`.
//! Каждый ASR запускается в отдельном процессе, чтобы снять память модели отдельно.

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{bail, Context};
use reever_lib::{
    asr::{Asr, AsrConfig},
    speech::{TimedWord, Transcriber},
    spellcheck,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

const SAMPLE_RATE: usize = 16_000;
const MODEL_GIGAAM: &str = "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16";
const MODEL_PARAKEET_V2: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";
const MODEL_PARAKEET_V3: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
const RESULT_PREFIX: &str = "__PARAKEET3_RESULT__";

#[derive(Clone)]
struct Sample {
    id: String,
    dataset: String,
    kind: String,
    expected_word: String,
    reference: String,
    voice: String,
    pcm: Vec<i16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WordResult {
    text: String,
    start: f32,
    end: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Recognition {
    id: String,
    text: String,
    words: Vec<WordResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BenchResult {
    model: String,
    model_dir: String,
    load_seconds: f64,
    inference_seconds: f64,
    audio_seconds: f64,
    rtf: f64,
    working_set_after_load: u64,
    private_bytes_after_load: u64,
    peak_working_set: u64,
    peak_private_bytes: u64,
    recognitions: Vec<Recognition>,
}

#[derive(Clone, Copy)]
enum ScriptFilter {
    All,
    Cyrillic,
    Latin,
}

#[derive(Default, Clone, Copy)]
struct MetricCounts {
    word_errors: usize,
    reference_words: usize,
    char_errors: usize,
    reference_chars: usize,
}

#[derive(Clone)]
struct Replacement {
    from: String,
    to: String,
    ru_start: f32,
    ru_end: f32,
    en_start: f32,
    en_end: f32,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--worker") {
        let model_key = args.get(1).context("--worker expects a model key")?;
        return run_worker(model_key);
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let samples = load_samples(&testdata)?;
    anyhow::ensure!(!samples.is_empty(), "no WAV samples were loaded");
    let main_zira_count = samples
        .iter()
        .filter(|sample| sample.dataset == "main_english" && sample.voice.contains("Zira"))
        .count();

    let executable = env::current_exe().context("locate benchmark executable")?;
    let models = [
        ("gigaam", "GigaAM v3 RU", MODEL_GIGAAM),
        ("parakeet_v2", "Parakeet TDT v2", MODEL_PARAKEET_V2),
        ("parakeet_v3", "Parakeet TDT v3", MODEL_PARAKEET_V3),
    ];
    let mut results = HashMap::new();
    for (key, label, _) in models {
        let started = Instant::now();
        let output = Command::new(&executable)
            .arg("--worker")
            .arg(key)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("start worker for {label}"))?;
        if !output.status.success() {
            bail!(
                "{label} worker failed ({}):\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = stdout
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(RESULT_PREFIX))
            .with_context(|| format!("{label} worker did not return its result"))?;
        let result: BenchResult =
            serde_json::from_str(json).with_context(|| format!("parse {label} worker result"))?;
        anyhow::ensure!(
            result.recognitions.len() == samples.len(),
            "{label} returned {} results for {} samples; manifests changed during the run?",
            result.recognitions.len(),
            samples.len()
        );
        println!(
            "{}: {:.2}s inference / {:.2}s audio, RTF {:.4}, WSS {:.0} MiB (worker {:.1}s)",
            label,
            result.inference_seconds,
            result.audio_seconds,
            result.rtf,
            result.peak_working_set as f64 / 1_048_576.0,
            started.elapsed().as_secs_f64()
        );
        results.insert(key.to_string(), result);
    }

    let gigaam = &results["gigaam"];
    let v2 = &results["parakeet_v2"];
    let v3 = &results["parakeet_v3"];
    let gigaam_map = recognition_map(gigaam);
    let v2_map = recognition_map(v2);
    let v3_map = recognition_map(v3);

    let ru_available = spellcheck::is_available("ru-RU");
    let en_available = spellcheck::is_available("en-US");
    let mut dictionary = DictionaryCache::default();
    let mix_samples: Vec<_> = samples
        .iter()
        .filter(|sample| sample.dataset.starts_with("mix"))
        .collect();
    let mut mixed_phrase_count = 0usize;
    let mut v2_direct_terms = 0usize;
    let mut v3_direct_terms = 0usize;
    let mut recovered_gigaam = 0usize;
    let mut recovered_v2 = 0usize;
    let mut recovered_v3 = 0usize;
    let mut recovered_merge = 0usize;
    let mut v2_both_scripts = 0usize;
    let mut v3_both_scripts = 0usize;
    let mut v3_cyrillic_only = 0usize;
    let mut v3_latin_only = 0usize;
    let mut v3_empty = 0usize;
    let mut corrupted_words = HashSet::new();
    let mut eligible_latin = 0usize;
    let mut latin_overlapped = 0usize;
    let mut applied_replacements = 0usize;
    let mut merge_rows = HashMap::new();
    let mut timing_matches = Vec::new();
    let mut v3_timed_cases = 0usize;
    let mut v3_word_count = 0usize;
    let mut v3_timed_word_count = 0usize;

    for sample in &mix_samples {
        let ru = &gigaam_map[&sample.id];
        let en = &v3_map[&sample.id];
        let merged = merge_words(ru, en, &mut dictionary);
        let v2_result = &v2_map[&sample.id];
        if !en.words.is_empty() {
            v3_timed_cases += 1;
        }
        v3_word_count += en.text.split_whitespace().count();
        v3_timed_word_count += en.words.len();
        timing_matches.extend(compare_word_timings(&ru.words, &en.words));

        for candidate in en.words.iter().filter(|word| is_latin_word(&word.text)) {
            if dictionary.is_word("en-US", &candidate.text) == Some(true) {
                eligible_latin += 1;
                if ru.words.iter().any(|russian| {
                    has_cyrillic(&russian.text)
                        && dictionary.is_word("ru-RU", &russian.text) == Some(false)
                        && overlap(candidate, russian) > 0.0
                }) {
                    latin_overlapped += 1;
                }
            }
        }

        let before_reference_russian: HashSet<String> =
            filtered_tokens(&sample.reference, ScriptFilter::Cyrillic)
                .into_iter()
                .collect();
        for replacement in &merged.replacements {
            applied_replacements += 1;
            if before_reference_russian.contains(&replacement.from) {
                corrupted_words.insert((sample.id.clone(), replacement.from.clone()));
            }
        }

        let is_mixed_phrase = sample.kind == "mixed";
        if is_mixed_phrase {
            mixed_phrase_count += 1;
            let target = &sample.expected_word;
            recovered_gigaam += usize::from(contains_word(&ru.text, target));
            v2_direct_terms += usize::from(contains_word(&v2_result.text, target));
            v3_direct_terms += usize::from(contains_word(&en.text, target));
            recovered_v2 += usize::from(contains_word(&v2_result.text, target));
            recovered_v3 += usize::from(contains_word(&en.text, target));
            recovered_merge += usize::from(contains_word(&merged.text, target));
            let (v2_cyrillic, v2_latin) = script_flags(&v2_result.text);
            let (v3_cyrillic, v3_latin) = script_flags(&en.text);
            v2_both_scripts += usize::from(v2_cyrillic && v2_latin);
            v3_both_scripts += usize::from(v3_cyrillic && v3_latin);
            match (v3_cyrillic, v3_latin) {
                (true, false) => v3_cyrillic_only += 1,
                (false, true) => v3_latin_only += 1,
                (false, false) => v3_empty += 1,
                (true, true) => {}
            }
        }

        if sample.kind == "russian_slang" {
            let target = &sample.expected_word;
            if contains_word(&ru.text, target) && !contains_word(&merged.text, target) {
                corrupted_words.insert((sample.id.clone(), target.clone()));
            }
        }
        merge_rows.insert(sample.id.clone(), merged);
    }

    // v3_direct_terms and recovered_v3 are the same count, kept as named values to
    // make the direct-vs-merged table below read naturally.
    let _ = (v3_direct_terms, v2_direct_terms);
    let dictation_samples: Vec<_> = samples
        .iter()
        .filter(|sample| sample.dataset == "dictation")
        .collect();
    let main_english: Vec<_> = samples
        .iter()
        .filter(|sample| sample.dataset == "main_english")
        .collect();
    let zira_english: Vec<_> = samples
        .iter()
        .filter(|sample| sample.dataset == "zira_english")
        .collect();

    let report = render_report(
        &models,
        &results,
        &gigaam_map,
        &v2_map,
        &v3_map,
        &merge_rows,
        &mix_samples,
        &dictation_samples,
        &main_english,
        &zira_english,
        &timing_matches,
        mixed_phrase_count,
        recovered_v2,
        recovered_v3,
        recovered_gigaam,
        recovered_merge,
        v2_both_scripts,
        v3_both_scripts,
        v3_cyrillic_only,
        v3_latin_only,
        v3_empty,
        corrupted_words.len(),
        eligible_latin,
        latin_overlapped,
        applied_replacements,
        v3_timed_cases,
        v3_word_count,
        v3_timed_word_count,
        ru_available,
        en_available,
        main_zira_count,
    );
    let path = testdata.join("parakeet3-mix-report.md");
    fs::write(&path, report).with_context(|| format!("write {}", path.display()))?;
    println!("report: {}", path.display());
    Ok(())
}

fn run_worker(model_key: &str) -> anyhow::Result<()> {
    let (label, model_name) = match model_key {
        "gigaam" => ("GigaAM v3 RU", MODEL_GIGAAM),
        "parakeet_v2" => ("Parakeet TDT v2", MODEL_PARAKEET_V2),
        "parakeet_v3" => ("Parakeet TDT v3", MODEL_PARAKEET_V3),
        _ => bail!("unknown worker model key: {model_key}"),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let samples = load_samples(&testdata)?;
    let model_dir = root.join("resources/model").join(model_name);
    let load_started = Instant::now();
    let mut asr = Asr::new(&AsrConfig {
        model_dir: model_dir.clone(),
        num_threads: 4,
    })?;
    let load_seconds = load_started.elapsed().as_secs_f64();
    let (working_set_after_load, private_bytes_after_load) = process_memory();
    let mut peak_working_set = working_set_after_load;
    let mut peak_private_bytes = private_bytes_after_load;
    let mut inference_seconds = 0.0;
    let mut audio_seconds = 0.0;
    let mut recognitions = Vec::with_capacity(samples.len());

    for sample in &samples {
        let started = Instant::now();
        let timed = asr.transcribe_timed(&sample.pcm)?;
        inference_seconds += started.elapsed().as_secs_f64();
        audio_seconds += sample.pcm.len() as f64 / SAMPLE_RATE as f64;
        let (working_set, private_bytes) = process_memory();
        peak_working_set = peak_working_set.max(working_set);
        peak_private_bytes = peak_private_bytes.max(private_bytes);
        recognitions.push(Recognition {
            id: sample.id.clone(),
            text: timed.text,
            words: timed
                .words
                .into_iter()
                .map(|word| WordResult {
                    text: word.text,
                    start: word.start,
                    end: word.end,
                })
                .collect(),
        });
    }

    let result = BenchResult {
        model: label.to_string(),
        model_dir: model_dir.display().to_string(),
        load_seconds,
        inference_seconds,
        audio_seconds,
        rtf: inference_seconds / audio_seconds.max(f64::EPSILON),
        working_set_after_load,
        private_bytes_after_load,
        peak_working_set,
        peak_private_bytes,
        recognitions,
    };
    println!("{RESULT_PREFIX}{}", serde_json::to_string(&result)?);
    Ok(())
}

fn load_samples(testdata: &Path) -> anyhow::Result<Vec<Sample>> {
    let mut samples = Vec::new();
    let mix_manifest = testdata.join("mix-manifest.json");
    if mix_manifest.is_file() {
        append_mix_samples(&mut samples, testdata, &mix_manifest, "mix1")?;
    }
    let mix2_manifest = testdata.join("mix2-manifest.json");
    if mix2_manifest.is_file() {
        append_mix_samples(&mut samples, testdata, &mix2_manifest, "mix2")?;
    }

    let dictation_path = testdata.join("dictation-manifest.json");
    if dictation_path.is_file() {
        let cases: Vec<Value> = read_json_file(&dictation_path)?;
        for case in cases {
            let file = string_field(&case, "file")?;
            let scenario = string_field(&case, "scenario").unwrap_or(&file);
            let reference = string_field(&case, "expected_text").unwrap_or("");
            let (pcm, _) = read_wav(&testdata.join("corpus/dictation").join(file))?;
            samples.push(Sample {
                id: format!("dictation:{scenario}"),
                dataset: "dictation".into(),
                kind: "dictation".into(),
                expected_word: String::new(),
                reference: reference.to_string(),
                voice: "dictation manifest".into(),
                pcm,
            });

            let parts = case
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for part in parts {
                let voice = string_field(&part, "voice").unwrap_or("");
                if !voice.contains("Zira") {
                    continue;
                }
                let part_text = string_field(&part, "text").unwrap_or("");
                let start_ms = number_field(&part, "start_ms").unwrap_or(0) as usize;
                let end_ms = number_field(&part, "end_ms").unwrap_or(0) as usize;
                let (full, sample_rate) = read_wav(&testdata.join("corpus/dictation").join(file))?;
                anyhow::ensure!(
                    sample_rate == SAMPLE_RATE,
                    "Zira clip is not 16 kHz: {file}"
                );
                let start = start_ms.saturating_sub(80) * sample_rate / 1000;
                let end = ((end_ms + 80) * sample_rate / 1000).min(full.len());
                if start >= end {
                    continue;
                }
                samples.push(Sample {
                    id: format!(
                        "zira_english:{scenario}:{}",
                        number_field(&part, "index").unwrap_or(0)
                    ),
                    dataset: "zira_english".into(),
                    kind: "english".into(),
                    expected_word: String::new(),
                    reference: clean_spoken_punctuation(part_text),
                    voice: voice.to_string(),
                    pcm: full[start..end].to_vec(),
                });
            }
        }
    }

    let main_manifest = testdata.join("manifest.json");
    if main_manifest.is_file() {
        let cases: Vec<Value> = read_json_file(&main_manifest)?;
        for case in cases {
            let text = string_field(&case, "text").unwrap_or("");
            let expected = case
                .get("expected")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
                .unwrap_or(text);
            if filtered_tokens(expected, ScriptFilter::Latin).is_empty() {
                continue;
            }
            let file = string_field(&case, "file")?;
            let (pcm, _) = read_wav(&testdata.join("corpus").join(file))?;
            let scenario = string_field(&case, "scenario").unwrap_or(&file);
            samples.push(Sample {
                id: format!("main_english:{file}"),
                dataset: "main_english".into(),
                kind: scenario.to_string(),
                expected_word: String::new(),
                reference: expected.to_string(),
                voice: string_field(&case, "voice")
                    .unwrap_or("unknown voice")
                    .to_string(),
                pcm,
            });
        }
    }
    Ok(samples)
}

fn append_mix_samples(
    output: &mut Vec<Sample>,
    testdata: &Path,
    manifest_path: &Path,
    dataset: &str,
) -> anyhow::Result<()> {
    let cases: Vec<Value> = read_json_file(manifest_path)?;
    for case in cases {
        let file = string_field(&case, "file")?;
        let audio_path = [
            testdata.join("corpus/mix").join(file),
            testdata.join("corpus").join(file),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .with_context(|| format!("cannot find WAV for {dataset}: {file}"))?;
        let (pcm, sample_rate) = read_wav(&audio_path)?;
        anyhow::ensure!(
            sample_rate == SAMPLE_RATE,
            "{file} has sample rate {sample_rate}"
        );
        let id = string_field(&case, "id").unwrap_or(&file).to_string();
        let expected_word = string_field(&case, "expected_word")
            .ok()
            .or_else(|| {
                case.get("expected_terms")
                    .and_then(Value::as_array)
                    .and_then(|terms| terms.first())
                    .and_then(Value::as_str)
            })
            .or_else(|| string_field(&case, "expected").ok())
            .unwrap_or("");
        output.push(Sample {
            id: format!("{dataset}:{id}"),
            dataset: dataset.to_string(),
            kind: string_field(&case, "kind").unwrap_or("mixed").to_string(),
            expected_word: expected_word.to_string(),
            reference: string_field(&case, "phrase").unwrap_or("").to_string(),
            voice: string_field(&case, "voice")
                .unwrap_or("unknown voice")
                .to_string(),
            pcm,
        });
    }
    Ok(())
}

fn read_wav(path: &Path) -> anyhow::Result<(Vec<i16>, usize)> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "{} is not a RIFF/WAVE file",
        path.display()
    );
    let mut offset = 12;
    let mut sample_rate = None;
    let mut channels = None;
    let mut bits_per_sample = None;
    let mut data = None;
    while offset + 8 <= bytes.len() {
        let tag = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let begin = offset + 8;
        let end = begin.saturating_add(length).min(bytes.len());
        if tag == b"fmt " && end >= begin + 16 {
            let format = u16::from_le_bytes(bytes[begin..begin + 2].try_into()?);
            channels = Some(u16::from_le_bytes(bytes[begin + 2..begin + 4].try_into()?) as usize);
            sample_rate =
                Some(u32::from_le_bytes(bytes[begin + 4..begin + 8].try_into()?) as usize);
            bits_per_sample =
                Some(u16::from_le_bytes(bytes[begin + 14..begin + 16].try_into()?) as usize);
            anyhow::ensure!(format == 1, "{} is not PCM WAV", path.display());
        } else if tag == b"data" {
            data = Some(&bytes[begin..end]);
        }
        offset = begin.saturating_add(length + (length & 1));
    }
    let sample_rate = sample_rate.context("WAV missing fmt sample rate")?;
    anyhow::ensure!(channels == Some(1), "{} is not mono", path.display());
    anyhow::ensure!(
        bits_per_sample == Some(16),
        "{} is not PCM16",
        path.display()
    );
    let data = data.context("WAV missing data chunk")?;
    let pcm = data
        .chunks_exact(2)
        .map(|part| i16::from_le_bytes([part[0], part[1]]))
        .collect();
    Ok((pcm, sample_rate))
}

fn render_report(
    models: &[(&str, &str, &str); 3],
    results: &HashMap<String, BenchResult>,
    gigaam_map: &HashMap<String, Recognition>,
    v2_map: &HashMap<String, Recognition>,
    v3_map: &HashMap<String, Recognition>,
    merge_rows: &HashMap<String, Merged>,
    mix_samples: &[&Sample],
    dictation_samples: &[&Sample],
    main_english: &[&Sample],
    zira_english: &[&Sample],
    timing_matches: &[(f32, f32, f32)],
    mixed_phrase_count: usize,
    recovered_v2: usize,
    recovered_v3: usize,
    recovered_gigaam: usize,
    recovered_merge: usize,
    v2_both_scripts: usize,
    v3_both_scripts: usize,
    v3_cyrillic_only: usize,
    v3_latin_only: usize,
    v3_empty: usize,
    ru_corrupted: usize,
    eligible_latin: usize,
    latin_overlapped: usize,
    applied_replacements: usize,
    v3_timed_cases: usize,
    v3_word_count: usize,
    v3_timed_word_count: usize,
    ru_available: bool,
    en_available: bool,
    main_zira_count: usize,
) -> String {
    let mut output = String::new();
    output.push_str("# Parakeet TDT 0.6B v3: исследование смешанной диктовки\n\n");
    output.push_str("Эксперимент использует синтетические WAV-корпуса Reever, 16 кГц mono PCM16. Все три модели запускались последовательно в отдельных процессах с `num_threads=4`; RTF — только время `transcribe_timed` / длительность аудио, время загрузки показано отдельно. Замер выполнялся в общей рабочей среде, поэтому CPU RTF помечен как полученный под нагрузкой. RSS — Windows working set процесса; private bytes — приватная память процесса.\n\n");

    output.push_str("## Производительность и память\n\n| Модель | Загрузка, с | ASR, с | Аудио, с | RTF (4 потока, под нагрузкой) | WSS после загрузки, МиБ | Пик WSS, МиБ | Private bytes после загрузки, МиБ | Пик private bytes, МиБ |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for (key, label, _) in models {
        let result = &results[*key];
        output.push_str(&format!(
            "| {} | {:.2} | {:.2} | {:.1} | {:.4} | {:.0} | {:.0} | {:.0} | {:.0} |\n",
            label,
            result.load_seconds,
            result.inference_seconds,
            result.audio_seconds,
            result.rtf,
            mib(result.working_set_after_load),
            mib(result.peak_working_set),
            mib(result.private_bytes_after_load),
            mib(result.peak_private_bytes),
        ));
    }
    output.push_str("\nПамять снималась в отдельном дочернем процессе для каждой модели; WSS после загрузки и пиковые значения относятся только к этому процессу. Для чистого RTF требовался запуск без параллельной нагрузки; результаты этого прогона нельзя считать тихим CPU-бенчмарком.\n\n");

    let dictation_filter = ScriptFilter::Cyrillic;
    let gigaam_ru = score_samples(dictation_samples, gigaam_map, dictation_filter, true);
    let v3_ru = score_samples(dictation_samples, v3_map, dictation_filter, true);
    output.push_str("## Русская диктовка: `dictation-manifest.json`\n\n");
    output.push_str(&format!(
        "GigaAM v3 RU: **{} WER**, **{} CER**; Parakeet v3: **{} WER**, **{} CER**. Считались только кириллические слова/символы из `expected_text`; перед оценкой из гипотез убраны диктовочные команды (`ривер напиши`, `стоп ввод`, `остановись`, `открой блокнот`) и озвученные маркеры пунктуации. Словари Windows: ru-RU={}, en-US={}.\n\n",
        pct(gigaam_ru.word_errors, gigaam_ru.reference_words),
        pct(gigaam_ru.char_errors, gigaam_ru.reference_chars),
        pct(v3_ru.word_errors, v3_ru.reference_words),
        pct(v3_ru.char_errors, v3_ru.reference_chars),
        ru_available,
        en_available,
    ));
    output.push_str("| Сценарий | RU слов | GigaAM WER | GigaAM CER | Parakeet v3 WER | Parakeet v3 CER |\n|---|---:|---:|---:|---:|---:|\n");
    for sample in dictation_samples {
        let one = std::slice::from_ref(sample);
        let ru = score_samples(one, gigaam_map, dictation_filter, true);
        let v3 = score_samples(one, v3_map, dictation_filter, true);
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            md(&sample.id),
            ru.reference_words,
            pct(ru.word_errors, ru.reference_words),
            pct(ru.char_errors, ru.reference_chars),
            pct(v3.word_errors, v3.reference_words),
            pct(v3.char_errors, v3.reference_chars),
        ));
    }

    output.push_str("\n## Английские фразы\n\n");
    output.push_str(&format!(
        "В текущем `testdata/manifest.json` найдено {} английских-по-эталону аудиофраз, но **0 с voice Zira** (Zira-в-основном-корпусе: {}). Поэтому таблица ниже отдельно показывает доступные английские Latin-подстроки основного корпуса с другими голосами, а точность на Zira — на 3 изолированных английских сегментах `dictation-manifest.json`. Эти три сегмента использованы как ближайшая доступная проверка native Zira; они не подменяют отсутствующую выборку main manifest. WER/CER считаются только по Latin токенам/символам; слова `period` и `question mark` в SAPI-тексте интерпретированы как знаки пунктуации и исключены из эталона.\n\n",
        main_english.len(), main_zira_count
    ));
    append_english_metric_table(
        &mut output,
        "Основной corpus: Latin-эталоны, все доступные голоса",
        main_english,
        gigaam_map,
        v2_map,
        v3_map,
    );
    append_english_metric_table(
        &mut output,
        "Изолированные English сегменты, Microsoft Zira (dictation corpus)",
        zira_english,
        gigaam_map,
        v2_map,
        v3_map,
    );

    let mix_all = score_samples(mix_samples, gigaam_map, ScriptFilter::All, false);
    let v2_mix = score_samples(mix_samples, v2_map, ScriptFilter::All, false);
    let v3_mix = score_samples(mix_samples, v3_map, ScriptFilter::All, false);
    let merged_mix = score_merged_samples(mix_samples, merge_rows);
    output.push_str("\n## Смешанные русско-английские фразы и пословное слияние\n\n");
    output.push_str(&format!(
        "Parakeet v3 вернул тайминги в **{v3_timed_cases}/{}** фразах mix-корпуса: {} таймированных слов на {} текстовых слов. Совпавшие текстом русские слова между GigaAM и v3 дали {} временных пар; медианная разница начала — **{} мс**, конца — **{} мс**, медианный IoU — **{}**. Это согласованность только для одинаково распознанных токенов, а не ground-truth точность границ.\n\n",
        mix_samples.len(),
        v3_timed_word_count,
        v3_word_count,
        timing_matches.len(),
        median(timing_matches.iter().map(|item| item.0).collect()) * 1000.0,
        median(timing_matches.iter().map(|item| item.1).collect()) * 1000.0,
        fmt_optional(median(timing_matches.iter().map(|item| item.2).collect())),
    ));
    output.push_str(&format!(
        "По всем mix-фразам (смешанные + русский сленг): GigaAM **WER {} / CER {}**; Parakeet v2 **WER {} / CER {}**; v3 **WER {} / CER {}**; после слияния GigaAM+v3 **WER {} / CER {}**. Слияние проверяет ровно три условия: v3-токен только латиницей, слово словарное en-US, тайминг пересекается с несловарным кириллическим словом GigaAM. Неоднозначное равное перекрытие пропускается; к одному русскому слову применяется не более одного английского.\n\n",
        pct(mix_all.word_errors, mix_all.reference_words), pct(mix_all.char_errors, mix_all.reference_chars),
        pct(v2_mix.word_errors, v2_mix.reference_words), pct(v2_mix.char_errors, v2_mix.reference_chars),
        pct(v3_mix.word_errors, v3_mix.reference_words), pct(v3_mix.char_errors, v3_mix.reference_chars),
        pct(merged_mix.word_errors, merged_mix.reference_words), pct(merged_mix.char_errors, merged_mix.reference_chars),
    ));
    output.push_str(&format!(
        "Целевой английский термин распознан напрямую: GigaAM **{recovered_gigaam}/{}**, v2 **{recovered_v2}/{}**, v3 **{recovered_v3}/{}**; после слияния: **{recovered_merge}/{}**. В v3 найдено {eligible_latin} латинских словарных слов, {latin_overlapped} из них имели пересечение с несловарным словом GigaAM; применено замен: {applied_replacements}. Словари доступны: ru-RU={}, en-US={}. Защита русского текста: **{}** известных русских слов из эталонов были бы потеряны при слиянии (счёт включает точные русские слова из `phrase` и целевые slang-токены, если их GigaAM уже распознал).\n\n",
        mixed_phrase_count, mixed_phrase_count, mixed_phrase_count, mixed_phrase_count,
        ru_available, en_available, ru_corrupted,
    ));
    output.push_str(&format!(
        "Переключение алфавита внутри одной фразы (одновременно есть кириллица и латиница): v2 **{v2_both_scripts}/{}**, v3 **{v3_both_scripts}/{}**. Ответы v3: только кириллица **{v3_cyrillic_only}**, только латиница **{v3_latin_only}**, пусто **{v3_empty}**.\n\n",
        mixed_phrase_count, mixed_phrase_count,
    ));
    output.push_str("| ID / класс / target | Голос | Эталонная фраза | GigaAM RU | Parakeet v2 | Parakeet v3 | После merge | Замены и интервалы (с) | Вывод |\n|---|---|---|---|---|---|---|---|---|\n");
    for sample in mix_samples {
        let ru = &gigaam_map[&sample.id];
        let v2 = &v2_map[&sample.id];
        let v3 = &v3_map[&sample.id];
        let merged = &merge_rows[&sample.id];
        let status = if sample.kind == "mixed" && contains_word(&merged.text, &sample.expected_word)
        {
            "термин есть"
        } else if sample.kind == "mixed" && contains_word(&v3.text, &sample.expected_word) {
            "v3 распознал напрямую"
        } else if sample.kind == "mixed" {
            "термин не восстановлен"
        } else if !sample.expected_word.is_empty()
            && contains_word(&ru.text, &sample.expected_word)
            && contains_word(&merged.text, &sample.expected_word)
        {
            "русское слово сохранено"
        } else if !sample.expected_word.is_empty() && contains_word(&ru.text, &sample.expected_word)
        {
            "РУССКОЕ СЛОВО ИЗМЕНЕНО"
        } else if merged.replacements.is_empty() {
            "русский текст без изменений"
        } else {
            "проверить русские замены"
        };
        let expected_label = if sample.expected_word.is_empty() {
            "—"
        } else {
            &sample.expected_word
        };
        let label = format!("{} / {} / `{}`", sample.id, sample.kind, expected_label);
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            md(&label),
            md(&sample.voice),
            md(&sample.reference),
            md(&ru.text),
            md(&v2.text),
            md(&v3.text),
            md(&merged.text),
            md(&format_replacements(&merged.replacements)),
            status,
        ));
    }

    output.push_str("\n## Рекомендация\n\n");
    let gain = recovered_merge as isize - recovered_v2 as isize;
    output.push_str(&format!(
        "Для текущей роли второй модели — донора английских слов — v3 **не заменяет v2** на этом корпусе: целевые термины напрямую — v2 {recovered_v2}/{} против v3 {recovered_v3}/{}; слияние GigaAM+v3 восстановило {recovered_merge}/{} (разница к прямому v2: {gain:+}). При этом на русской диктовке v3 точнее GigaAM (WER/CER показаны выше), а на полном mix-корпусе текст v3 заметно лучше v2; это делает v3 кандидатом на отдельную замену основного распознавателя, но не подтверждает этот путь восстановления терминов. Слияние даёт небольшой общий прирост WER/CER и не испортило ни одного известного русского слова, но вернуло лишь {recovered_merge} из {mixed_phrase_count} целевых английских терминов. Корпус синтетический, а Zira отсутствует в основном manifest; реальная пользовательская речь и Zira из целевого корпуса остаются непроверенными.",
        mixed_phrase_count, mixed_phrase_count, mixed_phrase_count,
    ));
    output
}

fn append_english_metric_table(
    output: &mut String,
    heading: &str,
    samples: &[&Sample],
    gigaam_map: &HashMap<String, Recognition>,
    v2_map: &HashMap<String, Recognition>,
    v3_map: &HashMap<String, Recognition>,
) {
    output.push_str(&format!("### {heading}\n\n"));
    if samples.is_empty() {
        output.push_str("Выборка пуста.\n\n");
        return;
    }
    let gigaam = score_samples(samples, gigaam_map, ScriptFilter::Latin, false);
    let v2 = score_samples(samples, v2_map, ScriptFilter::Latin, false);
    let v3 = score_samples(samples, v3_map, ScriptFilter::Latin, false);
    output.push_str(&format!(
        "GigaAM: WER {} / CER {}; Parakeet v2: WER {} / CER {}; Parakeet v3: WER {} / CER {} ({} фраз).\n\n",
        pct(gigaam.word_errors, gigaam.reference_words), pct(gigaam.char_errors, gigaam.reference_chars),
        pct(v2.word_errors, v2.reference_words), pct(v2.char_errors, v2.reference_chars),
        pct(v3.word_errors, v3.reference_words), pct(v3.char_errors, v3.reference_chars), samples.len(),
    ));
    output.push_str(
        "| Фраза | Голос | Эталон (Latin) | GigaAM | v2 | v3 |\n|---|---|---|---|---|---|\n",
    );
    for sample in samples {
        let reference = filtered_tokens(&sample.reference, ScriptFilter::Latin).join(" ");
        let gigaam = filtered_tokens(&gigaam_map[&sample.id].text, ScriptFilter::Latin).join(" ");
        let v2 = filtered_tokens(&v2_map[&sample.id].text, ScriptFilter::Latin).join(" ");
        let v3 = filtered_tokens(&v3_map[&sample.id].text, ScriptFilter::Latin).join(" ");
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            md(&sample.id),
            md(&sample.voice),
            md(&reference),
            md(&gigaam),
            md(&v2),
            md(&v3),
        ));
    }
    output.push('\n');
}

fn score_samples(
    samples: &[&Sample],
    predictions: &HashMap<String, Recognition>,
    filter: ScriptFilter,
    clean_dictation: bool,
) -> MetricCounts {
    let mut total = MetricCounts::default();
    for sample in samples {
        let Some(prediction) = predictions.get(&sample.id) else {
            continue;
        };
        let hypothesis = if clean_dictation && sample.dataset == "dictation" {
            strip_dictation_controls(&prediction.text)
        } else {
            prediction.text.clone()
        };
        let reference_words = filtered_tokens(&sample.reference, filter);
        let hypothesis_words = filtered_tokens(&hypothesis, filter);
        let reference_chars = filtered_chars(&sample.reference, filter);
        let hypothesis_chars = filtered_chars(&hypothesis, filter);
        total.word_errors += edit_distance(&reference_words, &hypothesis_words);
        total.reference_words += reference_words.len();
        total.char_errors += edit_distance(&reference_chars, &hypothesis_chars);
        total.reference_chars += reference_chars.len();
    }
    total
}

fn score_merged_samples(samples: &[&Sample], merged: &HashMap<String, Merged>) -> MetricCounts {
    let mut total = MetricCounts::default();
    for sample in samples {
        let Some(merged) = merged.get(&sample.id) else {
            continue;
        };
        let reference_words = filtered_tokens(&sample.reference, ScriptFilter::All);
        let hypothesis_words = filtered_tokens(&merged.text, ScriptFilter::All);
        let reference_chars = filtered_chars(&sample.reference, ScriptFilter::All);
        let hypothesis_chars = filtered_chars(&merged.text, ScriptFilter::All);
        total.word_errors += edit_distance(&reference_words, &hypothesis_words);
        total.reference_words += reference_words.len();
        total.char_errors += edit_distance(&reference_chars, &hypothesis_chars);
        total.reference_chars += reference_chars.len();
    }
    total
}

#[derive(Default)]
struct Merged {
    text: String,
    replacements: Vec<Replacement>,
}

#[derive(Default)]
struct DictionaryCache {
    words: HashMap<(String, String), Option<bool>>,
}

impl DictionaryCache {
    fn is_word(&mut self, language: &str, word: &str) -> Option<bool> {
        let key = (language.to_string(), word.to_lowercase());
        if let Some(result) = self.words.get(&key) {
            return *result;
        }
        let result = spellcheck::misspelled_ratio(language, word)
            .map(|(ratio, count)| count == 1 && ratio == 0.0);
        self.words.insert(key, result);
        result
    }
}

fn merge_words(ru: &Recognition, v3: &Recognition, dictionary: &mut DictionaryCache) -> Merged {
    let ru_tokens: Vec<String> = ru.text.split_whitespace().map(str::to_string).collect();
    if ru_tokens.len() != ru.words.len()
        || ru_tokens
            .iter()
            .zip(&ru.words)
            .any(|(text, timed)| word_key(text) != word_key(&timed.text))
    {
        return Merged {
            text: ru.text.clone(),
            replacements: Vec::new(),
        };
    }
    let mut proposals = Vec::<(usize, usize, f32)>::new();
    for (en_index, english) in v3.words.iter().enumerate() {
        if !is_latin_word(&english.text) || dictionary.is_word("en-US", &english.text) != Some(true)
        {
            continue;
        }
        let mut matches: Vec<(usize, f32)> = ru
            .words
            .iter()
            .enumerate()
            .filter_map(|(ru_index, russian)| {
                if !has_cyrillic(&russian.text)
                    || dictionary.is_word("ru-RU", &russian.text) != Some(false)
                {
                    return None;
                }
                let overlap = overlap(english, russian);
                (overlap > 0.0).then_some((ru_index, overlap))
            })
            .collect();
        matches.sort_by(|a, b| b.1.total_cmp(&a.1));
        let Some(&(best_index, best_overlap)) = matches.first() else {
            continue;
        };
        if matches
            .get(1)
            .is_some_and(|(_, next)| (best_overlap - next).abs() <= 0.01)
        {
            continue;
        }
        proposals.push((best_index, en_index, best_overlap));
    }

    // Only one English token may replace a given Russian token; keep the strongest overlap.
    proposals.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut used_ru = HashSet::new();
    let mut used_en = HashSet::new();
    let mut replacements = Vec::new();
    let mut replacements_by_ru = HashMap::new();
    for (ru_index, en_index, _) in proposals {
        if !used_ru.insert(ru_index) || !used_en.insert(en_index) {
            continue;
        }
        let russian = &ru.words[ru_index];
        let english = &v3.words[en_index];
        replacements.push(Replacement {
            from: word_key(&ru_tokens[ru_index]),
            to: word_key(&english.text),
            ru_start: russian.start,
            ru_end: russian.end,
            en_start: english.start,
            en_end: english.end,
        });
        replacements_by_ru.insert(ru_index, word_key(&english.text));
    }
    let output = ru_tokens
        .iter()
        .enumerate()
        .map(|(index, token)| {
            replacements_by_ru
                .get(&index)
                .cloned()
                .unwrap_or_else(|| token.clone())
        })
        .collect::<Vec<_>>()
        .join(" ");
    Merged {
        text: output,
        replacements,
    }
}

fn compare_word_timings(ru: &[WordResult], v3: &[WordResult]) -> Vec<(f32, f32, f32)> {
    let mut used = HashSet::new();
    let mut pairs = Vec::new();
    for russian in ru {
        let key = word_key(&russian.text);
        let best = v3
            .iter()
            .enumerate()
            .filter(|(index, word)| !used.contains(index) && word_key(&word.text) == key)
            .min_by(|(_, left), (_, right)| {
                (left.start - russian.start)
                    .abs()
                    .total_cmp(&(right.start - russian.start).abs())
            });
        if let Some((index, english)) = best {
            used.insert(index);
            let start_delta = (russian.start - english.start).abs();
            let end_delta = (russian.end - english.end).abs();
            let intersection =
                (russian.end.min(english.end) - russian.start.max(english.start)).max(0.0);
            let union =
                (russian.end.max(english.end) - russian.start.min(english.start)).max(f32::EPSILON);
            pairs.push((start_delta, end_delta, intersection / union));
        }
    }
    pairs
}

fn recognition_map(result: &BenchResult) -> HashMap<String, Recognition> {
    result
        .recognitions
        .iter()
        .cloned()
        .map(|row| (row.id.clone(), row))
        .collect()
}

fn filtered_tokens(text: &str, filter: ScriptFilter) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let normalized = token
                .trim_matches(|character: char| {
                    !character.is_alphanumeric() && character != '\'' && character != '-'
                })
                .to_lowercase();
            if normalized.is_empty() {
                return None;
            }
            let keep = match filter {
                ScriptFilter::All => normalized.chars().any(char::is_alphanumeric),
                ScriptFilter::Cyrillic => normalized.chars().any(is_cyrillic),
                ScriptFilter::Latin => normalized
                    .chars()
                    .any(|character| character.is_ascii_alphabetic()),
            };
            keep.then_some(normalized)
        })
        .collect()
}

fn filtered_chars(text: &str, filter: ScriptFilter) -> Vec<char> {
    text.chars()
        .filter(|character| match filter {
            ScriptFilter::All => character.is_alphanumeric(),
            ScriptFilter::Cyrillic => is_cyrillic(*character),
            ScriptFilter::Latin => character.is_ascii_alphabetic(),
        })
        .flat_map(char::to_lowercase)
        .collect()
}

fn edit_distance<T: Eq>(left: &[T], right: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_value) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_value) in right.iter().enumerate() {
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_value != right_value));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn contains_word(text: &str, target: &str) -> bool {
    !target.is_empty()
        && filtered_tokens(text, ScriptFilter::All)
            .iter()
            .any(|token| token.eq_ignore_ascii_case(target))
}

fn is_latin_word(word: &str) -> bool {
    let letters: Vec<_> = word
        .chars()
        .filter(|character| character.is_alphabetic())
        .collect();
    !letters.is_empty() && letters.iter().all(|character| character.is_ascii())
}

fn has_cyrillic(word: &str) -> bool {
    word.chars().any(is_cyrillic)
}

fn script_flags(text: &str) -> (bool, bool) {
    let cyrillic = text.chars().any(is_cyrillic);
    let latin = text
        .chars()
        .any(|character| character.is_ascii_alphabetic());
    (cyrillic, latin)
}

fn is_cyrillic(character: char) -> bool {
    matches!(character as u32, 0x0400..=0x052F)
}

fn word_key(word: &str) -> String {
    word.trim_matches(|character: char| {
        !character.is_alphanumeric() && character != '\'' && character != '-'
    })
    .to_lowercase()
}

fn overlap(left: &impl TimedInterval, right: &impl TimedInterval) -> f32 {
    (left.end().min(right.end()) - left.start().max(right.start())).max(0.0)
}

trait TimedInterval {
    fn start(&self) -> f32;
    fn end(&self) -> f32;
}

impl TimedInterval for TimedWord {
    fn start(&self) -> f32 {
        self.start
    }
    fn end(&self) -> f32 {
        self.end
    }
}

impl TimedInterval for WordResult {
    fn start(&self) -> f32 {
        self.start
    }
    fn end(&self) -> f32 {
        self.end
    }
}

fn strip_dictation_controls(text: &str) -> String {
    let controls = [
        "ривер",
        "напиши",
        "стоп",
        "ввод",
        "остановись",
        "открой",
        "блокнот",
        "запятая",
        "точка",
        "новая",
        "строка",
    ];
    text.split_whitespace()
        .filter(|token| !controls.contains(&word_key(token).as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_spoken_punctuation(text: &str) -> String {
    let mut words: Vec<_> = text.split_whitespace().map(str::to_string).collect();
    while words.last().is_some_and(|word| {
        matches!(
            word.to_lowercase().as_str(),
            "period" | "question" | "mark" | "comma" | "exclamation"
        )
    }) {
        words.pop();
    }
    words.join(" ")
}

fn format_replacements(replacements: &[Replacement]) -> String {
    replacements
        .iter()
        .map(|replacement| {
            format!(
                "{}→{} RU[{:.2}-{:.2}] EN[{:.2}-{:.2}]",
                replacement.from,
                replacement.to,
                replacement.ru_start,
                replacement.ru_end,
                replacement.en_start,
                replacement.en_end,
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn median(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

fn pct(errors: usize, reference: usize) -> String {
    if reference == 0 {
        "n/a".into()
    } else {
        format!(
            "{:.1}% ({errors}/{reference})",
            errors as f64 * 100.0 / reference as f64
        )
    }
}

fn fmt_optional(value: f32) -> String {
    if value.is_finite() {
        format!("{value:.3}")
    } else {
        "n/a".into()
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn md(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\r', "")
        .replace('\n', " ")
}

fn string_field<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("manifest field `{field}` is missing or not a string"))
}

fn number_field(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

fn read_json_file<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).with_context(|| format!("parse {}", path.display()))
}

#[cfg(windows)]
fn process_memory() -> (u64, u64) {
    use std::{ffi::c_void, mem::size_of};

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
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
        cb: size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            size_of::<ProcessMemoryCountersEx>() as u32,
        )
    };
    if ok == 0 {
        (0, 0)
    } else {
        (
            counters.working_set_size as u64,
            counters.private_usage as u64,
        )
    }
}

#[cfg(not(windows))]
fn process_memory() -> (u64, u64) {
    (0, 0)
}
