//! Benchmark small alternate sherpa-onnx families via its Rust API.
//!
//! This is separate from `Asr::new`: it bypasses Reever's model-family detection.
//!   cargo run --release --example en_moonshine_bench -- --model <dir> --refs <tsv> <wav>...

use std::{collections::HashMap, env, fs, path::PathBuf, time::Instant};

use anyhow::{bail, Context};
use sherpa_onnx::{
    OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig, Wave,
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut model = None;
    let mut refs = None;
    let mut wavs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--model" => {
                index += 1;
                model = Some(PathBuf::from(args.get(index).context("--model <каталог>")?));
            }
            "--refs" => {
                index += 1;
                refs = Some(PathBuf::from(args.get(index).context("--refs <tsv>")?));
            }
            arg if arg.starts_with('-') => bail!("неизвестный аргумент: {arg}"),
            arg => wavs.push(PathBuf::from(arg)),
        }
        index += 1;
    }
    let model = model.context("нужен --model <каталог>")?;
    let refs = refs.context("нужен --refs <tsv>")?;
    anyhow::ensure!(!wavs.is_empty(), "нужен хотя бы один WAV");

    let tokens = model.join("tokens.txt");
    anyhow::ensure!(tokens.is_file(), "нет {}", tokens.display());
    let mut model_config = sherpa_onnx::OfflineModelConfig::default();
    model_config.tokens = Some(tokens.to_string_lossy().into_owned());
    model_config.num_threads = 4;
    if model.join("decoder_model_merged.ort").is_file() {
        model_config.moonshine = OfflineMoonshineModelConfig {
            encoder: Some(path(&model, "encoder_model.ort")),
            merged_decoder: Some(path(&model, "decoder_model_merged.ort")),
            ..Default::default()
        };
    } else if model.join("preprocess.onnx").is_file() {
        model_config.moonshine = OfflineMoonshineModelConfig {
            preprocessor: Some(path(&model, "preprocess.onnx")),
            encoder: Some(path(&model, "encode.int8.onnx")),
            uncached_decoder: Some(path(&model, "uncached_decode.int8.onnx")),
            cached_decoder: Some(path(&model, "cached_decode.int8.onnx")),
            ..Default::default()
        };
    } else {
        let find = |prefix: &str| -> anyhow::Result<String> {
            let mut candidates: Vec<PathBuf> = fs::read_dir(&model)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|entry| {
                    entry.extension().is_some_and(|extension| extension == "onnx")
                        && entry
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
                })
                .collect();
            candidates.sort_by_key(|entry| (!entry.to_string_lossy().contains("int8"), entry.clone()));
            candidates
                .first()
                .map(|entry| entry.to_string_lossy().into_owned())
                .with_context(|| format!("не найден ONNX с префиксом {prefix}"))
        };
        model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some(find("encoder-")?),
            decoder: Some(find("decoder-")?),
            joiner: Some(find("joiner-")?),
        };
        // Для Zipformer sherpa выбирает совместимый transducer runtime по модели.
        model_config.model_type = None;
    }
    let mut config = OfflineRecognizerConfig::default();
    config.model_config = model_config;

    let model_bytes = directory_size(&model)?;
    let load_started = Instant::now();
    let recognizer = OfflineRecognizer::create(&config)
        .with_context(|| format!("Moonshine не загрузился из {}", model.display()))?;
    let load_seconds = load_started.elapsed().as_secs_f64();
    let references = read_references(refs)?;

    let mut total_elapsed = 0.0;
    let mut total_audio = 0.0;
    let mut total_errors = 0;
    let mut total_words = 0;
    let mut zira_errors = 0;
    let mut zira_words = 0;
    let mut accented_errors = 0;
    let mut accented_words = 0;
    println!(
        "MODEL {} | package {:.1} MiB | load {:.2}s | threads 4",
        model.display(),
        model_bytes as f64 / (1024.0 * 1024.0),
        load_seconds
    );

    for wav in &wavs {
        let key = wav
            .file_name()
            .context("непонятное имя WAV")?
            .to_string_lossy()
            .into_owned();
        let reference = references
            .get(&key)
            .with_context(|| format!("нет эталона для {key}"))?;
        let wave = Wave::read(wav.to_string_lossy().as_ref())
            .with_context(|| format!("не удалось прочитать WAV {}", wav.display()))?;
        anyhow::ensure!(wave.sample_rate() == 16_000, "нужен WAV 16 кГц: {}", wav.display());
        let duration = wave.num_samples() as f64 / f64::from(wave.sample_rate());
        let started = Instant::now();
        let stream = recognizer.create_stream();
        stream.accept_waveform(wave.sample_rate(), wave.samples());
        recognizer.decode(&stream);
        let elapsed = started.elapsed().as_secs_f64();
        let result = stream.get_result().context("нет результата ASR")?;
        let ref_words: Vec<String> = normalize(reference).split_whitespace().map(str::to_owned).collect();
        let hyp_words: Vec<String> = normalize(&result.text).split_whitespace().map(str::to_owned).collect();
        let errors = edit_distance(&ref_words, &hyp_words);
        total_errors += errors;
        total_words += ref_words.len();
        if key.contains("_zira") {
            zira_errors += errors;
            zira_words += ref_words.len();
        } else {
            accented_errors += errors;
            accented_words += ref_words.len();
        }
        total_elapsed += elapsed;
        total_audio += duration;
        println!(
            "{} | {:.3}s | audio {:.3}s | RTF {:.3} | ref: {} | hyp: {}",
            key,
            elapsed,
            duration,
            if duration > 0.0 { elapsed / duration } else { 0.0 },
            normalize(reference),
            normalize(&result.text)
        );
    }
    println!(
        "TOTAL elapsed={total_elapsed:.3}s audio={total_audio:.3}s RTF={:.3} WER={:.3} ZIRA-WER={:.3} RH-EN-WER={:.3}",
        if total_audio > 0.0 { total_elapsed / total_audio } else { 0.0 },
        ratio(total_errors, total_words),
        ratio(zira_errors, zira_words),
        ratio(accented_errors, accented_words)
    );
    Ok(())
}

fn path(model: &std::path::Path, name: &str) -> String {
    model.join(name).to_string_lossy().into_owned()
}

fn normalize(text: &str) -> String {
    reever_lib::textnorm::normalize(text)
}

fn read_references(path: PathBuf) -> anyhow::Result<HashMap<String, String>> {
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("не удалось прочитать эталоны {}", path.display()))?;
    let mut refs = HashMap::new();
    for (line, value) in contents.lines().enumerate() {
        if value.trim().is_empty() || value.starts_with('#') {
            continue;
        }
        let Some((file, reference)) = value.split_once('\t') else {
            bail!("{}:{}: ожидаются имя WAV, табуляция, текст", path.display(), line + 1);
        };
        refs.insert(file.trim().trim_start_matches('\u{feff}').to_string(), reference.trim().to_string());
    }
    Ok(refs)
}

fn directory_size(path: &std::path::Path) -> anyhow::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += directory_size(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

fn edit_distance(reference: &[String], hypothesis: &[String]) -> usize {
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

fn ratio(errors: usize, words: usize) -> f64 {
    if words == 0 {
        0.0
    } else {
        errors as f64 / words as f64
    }
}
