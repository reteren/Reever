//! Сравнение offline-ASR sherpa-onnx по WAV и эталонным транскриптам.
//!
//! Одна модель: `cargo run --release --example asr_bench -- <model-dir> [--threads N] [--refs refs.tsv] <wav>...`
//! Несколько моделей: `cargo run --release --example asr_bench -- --compare [--threads N] [--refs refs.tsv] <model-dir>... -- <wav>...`

use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process,
    time::Instant,
};

use anyhow::{bail, Context};
use reever_lib::{
    asr::{Asr, AsrConfig},
    textnorm,
};
use sherpa_onnx::Wave;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut compare = false;
    let mut after_separator = false;
    let mut num_threads = 4;
    let mut refs_path = None;
    let mut model_dirs = Vec::new();
    let mut wav_paths = Vec::new();
    let mut single_model = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--" => after_separator = true,
            "--compare" => compare = true,
            "--threads" => {
                index += 1;
                let value = args.get(index).context("--threads ожидает число")?;
                num_threads = value
                    .parse::<i32>()
                    .context("--threads должен быть целым числом")?;
                if num_threads < 1 {
                    bail!("--threads должен быть не меньше 1");
                }
            }
            "--refs" => {
                index += 1;
                refs_path = Some(PathBuf::from(
                    args.get(index).context("--refs ожидает TSV-файл")?,
                ));
            }
            arg if arg.starts_with('-') => bail!("неизвестный аргумент: {arg}"),
            arg if compare && !after_separator => model_dirs.push(PathBuf::from(arg)),
            arg if compare => wav_paths.push(PathBuf::from(arg)),
            arg if single_model.is_none() => single_model = Some(PathBuf::from(arg)),
            arg => wav_paths.push(PathBuf::from(arg)),
        }
        index += 1;
    }

    if !compare {
        model_dirs.extend(single_model);
    }
    if model_dirs.is_empty() || wav_paths.is_empty() {
        usage();
    }
    let references = refs_path
        .map(read_references)
        .transpose()?
        .unwrap_or_default();

    println!("threads={num_threads} pid={}", process::id());
    for model_dir in model_dirs {
        let model_bytes = directory_size(&model_dir)?;
        let load_started = Instant::now();
        let mut asr = Asr::new(&AsrConfig {
            model_dir: model_dir.clone(),
            num_threads,
        })?;
        let load_elapsed = load_started.elapsed().as_secs_f64();
        println!(
            "MODEL {} | size {:.1} MiB | load {:.2}s",
            model_dir.display(),
            model_bytes as f64 / (1024.0 * 1024.0),
            load_elapsed
        );

        let mut total_elapsed = 0.0_f64;
        let mut total_audio = 0.0_f64;
        let mut wer_edits = 0usize;
        let mut wer_words = 0usize;
        let mut cer_edits = 0usize;
        let mut cer_chars = 0usize;
        let mut en_errors = (0usize, 0usize, 0usize, 0usize);
        let mut ru_errors = (0usize, 0usize, 0usize, 0usize);
        let mut zira_errors = (0usize, 0usize, 0usize, 0usize);
        let mut rh_en_errors = (0usize, 0usize, 0usize, 0usize);

        for wav_path in &wav_paths {
            let wav_name = wav_path.to_string_lossy();
            let wave = Wave::read(&wav_name)
                .with_context(|| format!("не удалось прочитать WAV {}", wav_path.display()))?;
            let sample_rate = wave.sample_rate();
            if sample_rate != 16_000 {
                bail!(
                    "{}: частота {} Гц; бенчмарк ожидает mono 16 кГц",
                    wav_path.display(),
                    sample_rate
                );
            }
            let audio_seconds = wave.num_samples() as f64 / f64::from(sample_rate);
            let pcm: Vec<i16> = wave
                .samples()
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
                .collect();

            let started = Instant::now();
            let raw = asr.transcribe_raw(&pcm)?;
            let normalized = textnorm::normalize(&raw);
            let elapsed = started.elapsed().as_secs_f64();
            let rtf = if audio_seconds > 0.0 {
                elapsed / audio_seconds
            } else {
                0.0
            };
            let key = wav_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| wav_path.to_string_lossy().into_owned());
            if let Some(reference) = references.get(&key) {
                let reference = textnorm::normalize(reference);
                let reference_words: Vec<&str> = reference.split_whitespace().collect();
                let hypothesis_words: Vec<&str> = normalized.split_whitespace().collect();
                let word_edits = edit_distance(&reference_words, &hypothesis_words);
                let reference_chars: Vec<char> = reference.chars().collect();
                let hypothesis_chars: Vec<char> = normalized.chars().collect();
                let char_edits = edit_distance(&reference_chars, &hypothesis_chars);
                let wer = ratio(word_edits, reference_words.len());
                let cer = ratio(char_edits, reference_chars.len());
                wer_edits += word_edits;
                wer_words += reference_words.len();
                cer_edits += char_edits;
                cer_chars += reference_chars.len();
                let metrics = if key.contains("_ru_") {
                    &mut ru_errors
                } else {
                    &mut en_errors
                };
                metrics.0 += word_edits;
                metrics.1 += reference_words.len();
                metrics.2 += char_edits;
                metrics.3 += reference_chars.len();
                let voice_metrics = if key.starts_with("zira_") {
                    &mut zira_errors
                } else if key.contains("_ru_") {
                    &mut ru_errors
                } else {
                    &mut rh_en_errors
                };
                voice_metrics.0 += word_edits;
                voice_metrics.1 += reference_words.len();
                voice_metrics.2 += char_edits;
                voice_metrics.3 += reference_chars.len();
                println!(
                    "{} | {:.3}s | audio {:.3}s | RTF {:.3} | WER {:.3} | CER {:.3} | ref: {} | raw: {:?} | hyp: {}",
                    wav_path.display(), elapsed, audio_seconds, rtf, wer, cer, reference, raw, normalized
                );
            } else {
                println!(
                    "{} | {:.3}s | audio {:.3}s | RTF {:.3} | raw: {:?} | normalized: {}",
                    wav_path.display(),
                    elapsed,
                    audio_seconds,
                    rtf,
                    raw,
                    normalized
                );
            }
            total_elapsed += elapsed;
            total_audio += audio_seconds;
        }

        if total_audio > 0.0 {
            print!(
                "TOTAL elapsed={total_elapsed:.3}s audio={total_audio:.3}s RTF={:.3}",
                total_elapsed / total_audio
            );
        }
        if wer_words > 0 || cer_chars > 0 {
            print!(
                " WER={:.3} CER={:.3} EN-WER={:.3} EN-CER={:.3} ZIRA-WER={:.3} ZIRA-CER={:.3} RH-EN-WER={:.3} RH-EN-CER={:.3} RU-WER={:.3} RU-CER={:.3}",
                ratio(wer_edits, wer_words),
                ratio(cer_edits, cer_chars),
                ratio(en_errors.0, en_errors.1),
                ratio(en_errors.2, en_errors.3),
                ratio(zira_errors.0, zira_errors.1),
                ratio(zira_errors.2, zira_errors.3),
                ratio(rh_en_errors.0, rh_en_errors.1),
                ratio(rh_en_errors.2, rh_en_errors.3),
                ratio(ru_errors.0, ru_errors.1),
                ratio(ru_errors.2, ru_errors.3)
            );
        }
        println!();
    }
    Ok(())
}

fn read_references(path: PathBuf) -> anyhow::Result<HashMap<String, String>> {
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("не удалось прочитать эталоны {}", path.display()))?;
    let mut references = HashMap::new();
    for (line_index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((file_name, reference)) = line.split_once('\t') else {
            bail!(
                "{}:{}: ожидаются имя WAV, табуляция и эталон",
                path.display(),
                line_index + 1
            );
        };
        references.insert(file_name.trim().to_string(), reference.trim().to_string());
    }
    Ok(references)
}

fn directory_size(path: &Path) -> anyhow::Result<u64> {
    fn walk(path: &Path, total: &mut u64) -> anyhow::Result<()> {
        for entry in fs::read_dir(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?
        {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                walk(&entry.path(), total)?;
            } else {
                *total += metadata.len();
            }
        }
        Ok(())
    }
    let mut total = 0;
    walk(path, &mut total)?;
    Ok(total)
}

fn edit_distance<T: Eq>(reference: &[T], hypothesis: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    for (row, expected) in reference.iter().enumerate() {
        let mut current = vec![row + 1; hypothesis.len() + 1];
        for (column, actual) in hypothesis.iter().enumerate() {
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + usize::from(expected != actual));
        }
        previous = current;
    }
    previous[hypothesis.len()]
}

fn ratio(errors: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        errors as f64 / total as f64
    }
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  asr_bench <model-dir> [--threads N] [--refs refs.tsv] <wav>...\n  asr_bench --compare [--threads N] [--refs refs.tsv] <model-dir>... -- <wav>...\nrefs.tsv: file-name.wav<TAB>reference text"
    );
    process::exit(2);
}
