//! Исследование распознавания вырезанного английского термина из русской диктовки.
//!
//! Запуск из src-tauri/: $env:CARGO_BUILD_JOBS=4; cargo run --release --example segment_probe

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{ensure, Context};
use reever_lib::{
    asr::{Asr, AsrConfig},
    speech::{Timed, Transcriber, SAMPLE_RATE},
};
use serde::Deserialize;

const PADS_MS: [u32; 4] = [0, 100, 200, 300];
const FILTER_THRESHOLDS: [f64; 6] = [0.70, 0.76, 0.80, 0.84, 0.88, 0.92];
const EXTRA_SILENCE_MS: usize = 300;
const MODEL_THREADS: i32 = 4;

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    scenario: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    expected_word: Option<String>,
    #[serde(default)]
    expected_terms: Vec<String>,
    #[serde(default)]
    phrase: Option<String>,
    #[serde(default)]
    expected_text: Option<String>,
    file: String,
    #[serde(default)]
    parts: Vec<ManifestPart>,
}

#[derive(Debug, Deserialize)]
struct ManifestPart {
    #[serde(default)]
    text: String,
    #[serde(default)]
    start_ms: u32,
    #[serde(default)]
    end_ms: u32,
}

struct InputPhrase {
    id: String,
    source: String,
    kind: String,
    expected_terms: Vec<String>,
    phrase: String,
    file: PathBuf,
    english_intervals: Vec<(f32, f32)>,
}

#[derive(Clone)]
struct TokenInfo {
    text: String,
    start: f32,
    end: f32,
    ru_dictionary: Option<bool>,
}

struct TargetAssignment {
    expected_word: String,
    token_index: Option<usize>,
    similarity: f64,
}

struct PreparedPhrase {
    input: InputPhrase,
    pcm: Vec<i16>,
    duration: f32,
    last_signal: f32,
    timed: Timed,
    tokens: Vec<TokenInfo>,
    target_terms: Vec<TargetAssignment>,
    skipped_known_english: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum WindowMode {
    Word,
    Neighbors,
}

impl WindowMode {
    fn name(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Neighbors => "word+neighbors",
        }
    }
}

#[derive(Clone)]
struct EnglishWord {
    text: String,
    dictionary: Option<bool>,
}

struct Record {
    phrase_index: usize,
    token_index: usize,
    model_index: usize,
    pad_ms: u32,
    window: WindowMode,
    extra_silence: bool,
    text: String,
    inference_error: Option<String>,
    english_words: Vec<EnglishWord>,
    elapsed_ms: f64,
}

#[derive(Clone, Copy)]
struct Condition {
    model_index: usize,
    pad_ms: u32,
    window: WindowMode,
    extra_silence: bool,
    threshold: f64,
}

#[derive(Default, Clone)]
struct Stats {
    target_exact_raw: usize,
    target_dictionary_any: usize,
    target_true_positive: usize,
    accepted_false_slang: usize,
    accepted_false_dictation: usize,
    accepted_false_mix: usize,
    accepted_false_mix2: usize,
    slang_windows: usize,
    dictation_windows: usize,
    mix_windows: usize,
    mix_dictionary_any: usize,
    mix_nonword_windows: usize,
    mix_nonword_dictionary_any: usize,
    slang_phrases_hit: HashSet<String>,
    dictation_phrases_hit: HashSet<String>,
    mix2_phrases_hit: HashSet<String>,
}

#[derive(Clone)]
struct BestCondition {
    condition: Condition,
    stats: Stats,
    f1: f64,
    precision: f64,
    recall: f64,
}

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let model_root = root.join("resources/model");
    let mut inputs = load_mix_inputs(&testdata)?;
    let dictation_manifest = testdata.join("dictation-manifest.json");
    let dictation_entries: Vec<ManifestEntry> = serde_json::from_slice(
        &std::fs::read(&dictation_manifest)
            .with_context(|| format!("не удалось прочитать {}", dictation_manifest.display()))?,
    )?;
    for entry in dictation_entries {
        let english_intervals = entry
            .parts
            .iter()
            .filter(|part| contains_ascii_word(&part.text))
            .map(|part| (part.start_ms as f32 / 1000.0, part.end_ms as f32 / 1000.0))
            .collect();
        inputs.push(InputPhrase {
            id: entry
                .scenario
                .or(entry.id)
                .unwrap_or_else(|| entry.file.clone()),
            source: "dictation".into(),
            kind: "russian_control".into(),
            expected_terms: Vec::new(),
            phrase: entry.expected_text.unwrap_or_default(),
            file: testdata.join("corpus/dictation").join(entry.file),
            english_intervals,
        });
    }

    let ru_available = reever_lib::spellcheck::is_available("ru-RU");
    let en_available = reever_lib::spellcheck::is_available("en-US");
    eprintln!("Windows spellcheck: ru-RU={ru_available}, en-US={en_available}");
    let mut dictionary_cache: HashMap<String, Option<bool>> = HashMap::new();

    // Сначала строим временную сетку GigaAM для всего корпуса; модель освобождается
    // до загрузки английских моделей, чтобы не держать несколько больших ASR в памяти.
    let mut ru_asr = Asr::new(&AsrConfig {
        model_dir: model_root.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16"),
        num_threads: MODEL_THREADS,
    })?;
    let mut phrases = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.into_iter().enumerate() {
        let pcm = read_wav(&input.file)?;
        let duration = pcm.len() as f32 / SAMPLE_RATE as f32;
        let last_signal = last_nonzero_time(&pcm);
        let timed = ru_asr.transcribe_timed(&pcm)?;
        let mut tokens = Vec::with_capacity(timed.words.len());
        for word in &timed.words {
            let key = word_key(&word.text);
            let ru_dictionary = dictionary_word("ru-RU", &key, &mut dictionary_cache);
            tokens.push(TokenInfo {
                text: word.text.clone(),
                start: word.start,
                end: word.end,
                ru_dictionary,
            });
        }
        let target_terms = target_assignments(&tokens, &input.expected_terms);
        let skipped_known_english = if input.source == "dictation" {
            tokens
                .iter()
                .filter(|token| {
                    input
                        .english_intervals
                        .iter()
                        .any(|(start, end)| token.start >= *start && token.start < *end)
                })
                .count()
        } else {
            0
        };
        eprintln!(
            "[GigaAM {}/{}] {}: {} words, targets={}, last-end Δ={:+.0} ms",
            index + 1,
            phrases.capacity(),
            input.id,
            tokens.len(),
            target_terms
                .iter()
                .map(|target| {
                    format!(
                        "{}←{} ({:.2})",
                        target.expected_word,
                        target
                            .token_index
                            .map(|i| tokens[i].text.as_str())
                            .unwrap_or("—"),
                        target.similarity
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            tokens
                .last()
                .map_or(0.0, |word| (word.end - last_signal) * 1000.0),
        );
        phrases.push(PreparedPhrase {
            input,
            pcm,
            duration,
            last_signal,
            timed,
            tokens,
            target_terms,
            skipped_known_english,
        });
    }
    drop(ru_asr);

    if std::env::var_os("SEGMENT_PROBE_GIGAAM_ONLY").is_some() {
        for source in ["mix", "mix2", "dictation"] {
            let source_phrases: Vec<&PreparedPhrase> = phrases
                .iter()
                .filter(|phrase| phrase.input.source == source)
                .collect();
            let tokens: Vec<(usize, &TokenInfo)> = source_phrases
                .iter()
                .flat_map(|phrase| phrase.tokens.iter().map(move |token| (0, token)))
                .collect();
            let nonword_tokens = source_phrases
                .iter()
                .flat_map(|phrase| phrase.tokens.iter())
                .filter(|token| token.ru_dictionary == Some(false))
                .count();
            let eligible_crops = source_phrases
                .iter()
                .enumerate()
                .flat_map(|(phrase_index, phrase)| {
                    phrase
                        .tokens
                        .iter()
                        .enumerate()
                        .filter_map(move |(token_index, token)| {
                            if token.ru_dictionary != Some(false) {
                                return None;
                            }
                            Some((phrase_index, token_index, phrase, token))
                        })
                })
                .flat_map(|(_, token_index, phrase, _)| {
                    [WindowMode::Word, WindowMode::Neighbors]
                        .into_iter()
                        .flat_map(move |window| {
                            PADS_MS.into_iter().filter_map(move |pad_ms| {
                                let (start, end) =
                                    crop_interval(phrase, token_index, window, pad_ms)?;
                                let start = start as f32 / SAMPLE_RATE as f32;
                                let end = end as f32 / SAMPLE_RATE as f32;
                                if source == "dictation"
                                    && overlaps_known_english_part(phrase, start, end)
                                {
                                    None
                                } else {
                                    Some(())
                                }
                            })
                        })
                })
                .count();
            eprintln!(
                "[GigaAM-only] {source}: {} phrases, {} tokens, {nonword_tokens} ru-RU nonwords, {eligible_crops} eligible token/window/pad crops",
                source_phrases.len(),
                tokens.len()
            );
        }
        if std::env::var_os("SEGMENT_PROBE_DUMP_TIMINGS").is_some() {
            let timings = gigaam_timing_table(&phrases);
            let path = std::env::temp_dir().join("reever_segment_gigaam_timings.md");
            std::fs::write(&path, timings)?;
            eprintln!("[GigaAM-only] timing table: {}", path.display());
        }
        return Ok(());
    }

    let models = [
        ("Parakeet v2", "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"),
        ("Parakeet v3", "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"),
        ("Qwen3-ASR", "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25"),
        ("Whisper small (en)", "sherpa-onnx-whisper-small"),
    ];
    let mut records = Vec::new();
    for (model_index, (model_name, model_dir_name)) in models.iter().enumerate() {
        let original_dir = model_root.join(model_dir_name);
        let whisper_alias = if model_index == 3 {
            Some(create_whisper_english_alias(&original_dir)?)
        } else {
            None
        };
        let effective_dir = whisper_alias.as_deref().unwrap_or(&original_dir);
        eprintln!(
            "[model] loading {model_name} from {}",
            effective_dir.display()
        );
        let mut asr = Asr::new(&AsrConfig {
            model_dir: effective_dir.to_path_buf(),
            num_threads: MODEL_THREADS,
        })?;
        let mut inference_errors = 0usize;
        for (phrase_index, phrase) in phrases.iter().enumerate() {
            for token_index in 0..phrase.tokens.len() {
                let token = &phrase.tokens[token_index];
                // Для dictation negatives достаточно несловарных RU-кандидатов:
                // словарное RU-слово всё равно отклонит заданный фильтр.
                if phrase.input.source == "dictation" && token.ru_dictionary != Some(false) {
                    continue;
                }
                for window in [WindowMode::Word, WindowMode::Neighbors] {
                    if token.ru_dictionary != Some(false) && window == WindowMode::Neighbors {
                        continue;
                    }
                    for pad_ms in PADS_MS {
                        let Some((start, end)) = crop_interval(phrase, token_index, window, pad_ms)
                        else {
                            continue;
                        };
                        let crop_start = start as f32 / SAMPLE_RATE as f32;
                        let crop_end = end as f32 / SAMPLE_RATE as f32;
                        if phrase.input.source == "dictation"
                            && overlaps_known_english_part(phrase, crop_start, crop_end)
                        {
                            continue;
                        }
                        let crop = phrase.pcm[start..end].to_vec();
                        for extra_silence in [false, true] {
                            if token.ru_dictionary != Some(false) && extra_silence {
                                continue;
                            }
                            let input = if extra_silence {
                                let silence =
                                    vec![0_i16; EXTRA_SILENCE_MS * SAMPLE_RATE as usize / 1000];
                                let mut augmented =
                                    Vec::with_capacity(crop.len() + silence.len() * 2);
                                augmented.extend_from_slice(&silence);
                                augmented.extend_from_slice(&crop);
                                augmented.extend_from_slice(&silence);
                                augmented
                            } else {
                                crop.clone()
                            };
                            let started = Instant::now();
                            let (text, inference_error) = match asr.transcribe_verbatim(&input) {
                                Ok(text) => (text, None),
                                Err(error) => {
                                    inference_errors += 1;
                                    if inference_errors <= 5 {
                                        eprintln!(
                                            "[{model_name} no-result] {} / {} / {} / pad={} / silence={}: {error:#}",
                                            phrase.input.id,
                                            token.text,
                                            window.name(),
                                            pad_ms,
                                            extra_silence
                                        );
                                    }
                                    (String::new(), Some(format!("{error:#}")))
                                }
                            };
                            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                            records.push(Record {
                                phrase_index,
                                token_index,
                                model_index,
                                pad_ms,
                                window,
                                extra_silence,
                                text,
                                inference_error,
                                english_words: Vec::new(),
                                elapsed_ms,
                            });
                        }
                    }
                }
            }
            if (phrase_index + 1) % 4 == 0 || phrase_index + 1 == phrases.len() {
                eprintln!(
                    "[{model_name} {}/{}] {} candidates decoded",
                    phrase_index + 1,
                    phrases.len(),
                    records
                        .iter()
                        .filter(|r| r.model_index == model_index)
                        .count()
                );
            }
        }
        drop(asr);
        eprintln!("[{model_name}] inference errors treated as empty: {inference_errors}");
        if let Some(alias) = whisper_alias {
            std::fs::remove_dir_all(&alias)
                .with_context(|| format!("не удалить временный alias {}", alias.display()))?;
        }
    }

    // English spellcheck на уникальных output-токенах, кэшируя COM-вызовы.
    for record in &mut records {
        record.english_words = split_words(&record.text)
            .into_iter()
            .map(|word| {
                let dictionary = dictionary_word("en-US", &word, &mut dictionary_cache);
                EnglishWord {
                    text: word,
                    dictionary,
                }
            })
            .collect();
    }

    let report = render_report(&phrases, &records, &models, ru_available, en_available);
    let report_path = testdata.join("segment-report.md");
    std::fs::write(&report_path, report)?;
    println!("report: {}", report_path.display());
    Ok(())
}

fn load_mix_inputs(testdata: &Path) -> anyhow::Result<Vec<InputPhrase>> {
    let mut result = Vec::new();
    let path = testdata.join("mix-manifest.json");
    let entries: Vec<ManifestEntry> = serde_json::from_slice(&std::fs::read(&path)?)?;
    result.extend(
        entries
            .into_iter()
            .map(|entry| input_from_mix(entry, testdata, "mix")),
    );

    // Worker corpus может появиться уже после старта исследования.
    let mix2_manifest = testdata.join("mix2-manifest.json");
    let mix2_corpus = testdata.join("corpus/mix2");
    if mix2_manifest.is_file() {
        let entries: Vec<ManifestEntry> = serde_json::from_slice(&std::fs::read(&mix2_manifest)?)?;
        for entry in entries {
            let mut input = input_from_mix(entry, testdata, "mix2");
            let file_name = input.file.file_name().unwrap_or_default();
            let nested_path = mix2_corpus.join(file_name);
            let root_path = testdata.join("corpus").join(file_name);
            input.file = if nested_path.is_file() {
                nested_path
            } else {
                root_path
            };
            result.push(input);
        }
    }
    Ok(result)
}

fn input_from_mix(entry: ManifestEntry, testdata: &Path, source: &str) -> InputPhrase {
    let kind = entry.kind.unwrap_or_else(|| "mixed".into());
    let mut expected_terms = if kind == "mixed" {
        entry.expected_terms
    } else {
        Vec::new()
    };
    if expected_terms.is_empty() && kind == "mixed" {
        if let Some(expected_word) = entry.expected_word {
            expected_terms.push(expected_word);
        }
    }
    let id = entry
        .id
        .or(entry.scenario)
        .unwrap_or_else(|| entry.file.clone());
    InputPhrase {
        id,
        source: source.into(),
        kind,
        expected_terms,
        phrase: entry.phrase.or(entry.expected_text).unwrap_or_default(),
        file: testdata.join(format!("corpus/{source}")).join(entry.file),
        english_intervals: Vec::new(),
    }
}

fn target_assignments(tokens: &[TokenInfo], expected_words: &[String]) -> Vec<TargetAssignment> {
    let mut assigned = HashSet::new();
    expected_words
        .iter()
        .filter_map(|expected_word| {
            let expected_key = english_sound_key(expected_word);
            if expected_key.is_empty() {
                return None;
            }
            let best = tokens
                .iter()
                .enumerate()
                .filter(|(index, _)| !assigned.contains(index))
                .map(|(index, token)| {
                    (
                        index,
                        strsim::jaro_winkler(&russian_sound_key(&token.text), &expected_key),
                    )
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let (token_index, similarity) = best.unwrap_or((usize::MAX, 0.0));
            if token_index == usize::MAX {
                return Some(TargetAssignment {
                    expected_word: word_key(expected_word),
                    token_index: None,
                    similarity,
                });
            }
            assigned.insert(token_index);
            Some(TargetAssignment {
                expected_word: word_key(expected_word),
                token_index: Some(token_index),
                similarity,
            })
        })
        .collect()
}

fn dictionary_word(
    language: &str,
    word: &str,
    cache: &mut HashMap<String, Option<bool>>,
) -> Option<bool> {
    let key = word_key(word);
    if key.is_empty() {
        return None;
    }
    let cache_key = format!("{language}\0{key}");
    cache
        .entry(cache_key)
        .or_insert_with(|| {
            reever_lib::spellcheck::misspelled_ratio(language, &key)
                .map(|(ratio, count)| count == 1 && ratio == 0.0)
        })
        .to_owned()
}

fn crop_interval(
    phrase: &PreparedPhrase,
    token_index: usize,
    window: WindowMode,
    pad_ms: u32,
) -> Option<(usize, usize)> {
    let (start, end) = match window {
        WindowMode::Word => {
            let token = phrase.tokens.get(token_index)?;
            (token.start, token.end)
        }
        WindowMode::Neighbors => {
            let left = token_index.saturating_sub(1);
            let right = (token_index + 1).min(phrase.tokens.len().saturating_sub(1));
            (
                phrase.tokens.get(left)?.start,
                phrase.tokens.get(right)?.end,
            )
        }
    };
    let pad = pad_ms as f32 / 1000.0;
    let start = ((start - pad).max(0.0) * SAMPLE_RATE as f32).floor() as usize;
    let end = ((end + pad).min(phrase.duration) * SAMPLE_RATE as f32).ceil() as usize;
    let start = start.min(phrase.pcm.len());
    let end = end.min(phrase.pcm.len());
    (end > start).then_some((start, end))
}

fn overlaps_known_english_part(phrase: &PreparedPhrase, start: f32, end: f32) -> bool {
    phrase
        .input
        .english_intervals
        .iter()
        .any(|(english_start, english_end)| start < *english_end && end > *english_start)
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path).with_context(|| format!("прочитать {}", path.display()))?;
    ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "{} не WAV RIFF",
        path.display()
    );
    let mut offset = 12;
    let mut format = None;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let begin = offset + 8;
        let end = begin.saturating_add(size).min(bytes.len());
        if id == b"fmt " && size >= 16 && end >= begin + 16 {
            format = Some((
                u16::from_le_bytes(bytes[begin..begin + 2].try_into()?),
                u16::from_le_bytes(bytes[begin + 2..begin + 4].try_into()?),
                u32::from_le_bytes(bytes[begin + 4..begin + 8].try_into()?),
                u16::from_le_bytes(bytes[begin + 14..begin + 16].try_into()?),
            ));
        } else if id == b"data" {
            data = Some(&bytes[begin..end]);
            break;
        }
        offset = begin.saturating_add(size).saturating_add(size & 1);
    }
    let (encoding, channels, sample_rate, bits) = format.context("нет fmt chunk")?;
    ensure!(encoding == 1 && channels == 1 && sample_rate == SAMPLE_RATE as u32 && bits == 16,
        "{}: ожидается PCM16 mono {} Гц, получено encoding={encoding}, channels={channels}, rate={sample_rate}, bits={bits}",
        path.display(), SAMPLE_RATE);
    let data = data.context("нет WAV data chunk")?;
    Ok(data
        .chunks_exact(2)
        .map(|part| i16::from_le_bytes([part[0], part[1]]))
        .collect())
}

fn last_nonzero_time(pcm: &[i16]) -> f32 {
    pcm.iter()
        .rposition(|sample| *sample != 0)
        .map_or(0.0, |index| (index + 1) as f32 / SAMPLE_RATE as f32)
}

fn contains_ascii_word(text: &str) -> bool {
    text.split_whitespace().any(|part| {
        part.chars()
            .any(|character| character.is_ascii_alphabetic())
    })
}

fn split_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(word_key)
        .filter(|word| !word.is_empty())
        .collect()
}

fn word_key(word: &str) -> String {
    word.chars()
        .filter(|character| character.is_alphanumeric() || *character == '\'' || *character == '-')
        .flat_map(char::to_lowercase)
        .collect()
}

// Те же фонетические ключи и правки, что применяются текущим dictation mixer.
fn russian_sound_key(word: &str) -> String {
    let chars: Vec<char> = word_key(word).chars().collect();
    let mut output = String::new();
    let mut index = 0;
    while index < chars.len() {
        let pair: String = chars[index..(index + 2).min(chars.len())].iter().collect();
        if pair == "дж" {
            output.push('j');
            index += 2;
            continue;
        }
        if pair == "кс" {
            output.push('x');
            index += 2;
            continue;
        }
        let mapped = match chars[index] {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'э' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ы' => "i",
            'ю' => "yu",
            'я' => "ya",
            'ъ' | 'ь' => "",
            other if other.is_ascii_alphanumeric() => {
                output.push(other);
                index += 1;
                continue;
            }
            _ => "",
        };
        output.push_str(mapped);
        index += 1;
    }
    output
}

fn english_sound_key(word: &str) -> String {
    word_key(word)
        .replace("ough", "uff")
        .replace("ph", "f")
        .replace("qu", "kw")
        .replace("ck", "k")
        .replace("ge", "j")
        .chars()
        .filter(char::is_ascii_alphabetic)
        .collect()
}

fn create_whisper_english_alias(model_dir: &Path) -> anyhow::Result<PathBuf> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let alias = std::env::temp_dir().join(format!(
        "reever-segment-probe-{}-{stamp}-whisper-small.en",
        std::process::id()
    ));
    std::fs::create_dir(&alias)
        .with_context(|| format!("создать временный каталог {}", alias.display()))?;
    for name in [
        "small-encoder.int8.onnx",
        "small-decoder.int8.onnx",
        "small-tokens.txt",
    ] {
        let source = model_dir.join(name);
        let target = alias.join(name);
        if let Err(error) = std::fs::hard_link(&source, &target) {
            let _ = std::fs::remove_dir_all(&alias);
            return Err(error).with_context(|| {
                format!(
                    "не создать hard link {} → {}",
                    source.display(),
                    target.display()
                )
            });
        }
    }
    Ok(alias)
}

fn render_report(
    phrases: &[PreparedPhrase],
    records: &[Record],
    models: &[(&str, &str); 4],
    ru_available: bool,
    en_available: bool,
) -> String {
    let positive_count: usize = phrases.iter().map(|phrase| phrase.target_terms.len()).sum();
    let total_mix_tokens: usize = phrases
        .iter()
        .filter(|phrase| phrase.input.source == "mix" || phrase.input.source == "mix2")
        .map(|phrase| phrase.tokens.len())
        .sum();
    let nonword_mix_tokens: usize = phrases
        .iter()
        .filter(|phrase| phrase.input.source == "mix" || phrase.input.source == "mix2")
        .flat_map(|phrase| phrase.tokens.iter())
        .filter(|token| token.ru_dictionary == Some(false))
        .count();
    let skipped_english: usize = phrases
        .iter()
        .map(|phrase| phrase.skipped_known_english)
        .sum();
    let dictation_tokens: usize = phrases
        .iter()
        .filter(|phrase| phrase.input.source == "dictation")
        .map(|phrase| phrase.tokens.len())
        .sum();
    let dictation_nonword_tokens: usize = phrases
        .iter()
        .filter(|phrase| phrase.input.source == "dictation")
        .flat_map(|phrase| phrase.tokens.iter())
        .filter(|token| token.ru_dictionary == Some(false))
        .count();
    let dictation_candidate_crops = records
        .iter()
        .filter(|record| phrases[record.phrase_index].input.source == "dictation")
        .count();
    let mut output = String::new();
    output.push_str("# Выборочное распознавание вырезанного слова\n\n");
    output.push_str(&format!(
        "Корпус: `mix-manifest` — {} фраз ({} целевых EN-терминов и {} сленговых), `dictation-manifest` — {} контрольных длинных записей{}; GigaAM нашла {} токенов в микс-корпусе и {} из них не прошли ru-RU spellcheck. Windows spellcheck: ru-RU={}, en-US={}.\n\n",
        phrases.iter().filter(|p| p.input.source == "mix").count(),
        phrases
            .iter()
            .filter(|p| p.input.source == "mix")
            .map(|p| p.target_terms.len())
            .sum::<usize>(),
        phrases.iter().filter(|p| p.input.source == "mix" && p.input.kind == "russian_slang").count(),
        phrases.iter().filter(|p| p.input.source == "dictation").count(),
        if phrases.iter().any(|p| p.input.source == "mix2") { format!("; mix2 — {} фраз / {} целевых терминов", phrases.iter().filter(|p| p.input.source == "mix2").count(), phrases.iter().filter(|p| p.input.source == "mix2").map(|p| p.target_terms.len()).sum::<usize>()) } else { String::new() },
        total_mix_tokens, nonword_mix_tokens, ru_available, en_available
    ));
    output.push_str(&format!(
        "Все mix/mix2 токены получили baseline word-crops при каждом pad без дополнительной тишины; несловарные RU-кандидаты получили полную сетку (word/neighbors × тишина off/on), потому что только они могут пройти фильтр. В диктовке обрабатывались только несловарные RU-токены: словарные слова фильтр всё равно запрещает; {} GigaAM-токенов начинаются внутри эталонных ASCII-English `parts`; все crops, пересекающие эти интервалы, исключены из false-positive контроля как заведомо настоящая английская речь.\n\n",
        skipped_english
    ));
    output.push_str(&format!(
        "В диктовке GigaAM выдала {dictation_tokens} токенов, из них ru-RU несловарных — {dictation_nonword_tokens}; после исключения окон с эталонным английским выполнено {dictation_candidate_crops} отрицательных crops. Поэтому `FP dictation=0` здесь означает отсутствие подходящих кандидатов, а не подтверждённую нулевую частоту ложных срабатываний.\n\n"
    ));
    output.push_str("Сегменты: `[start−pad, end+pad]`, `pad=0/100/200/300 мс`; `word+neighbors` расширяет окно на один соседний GigaAM-токен с каждой стороны. Во второй серии к каждому crop добавлялось 300 мс цифровой тишины до и после. Вызовы шли последовательно, по одной модели в памяти, `num_threads=4`; RTF/latency ниже — **под нагрузкой** от параллельных воркеров.\n\n");
    output.push_str("ASR: Parakeet v2, Parakeet v3, Qwen3-ASR, Whisper small (English forced). Для Whisper small временный каталог `*.en` собран hard-link-ами в `%TEMP%`, чтобы `reever_lib::asr` установил `language=en`; каталог удалён после прогона.\n\n");
    output.push_str("`transcribe_verbatim` errors на отдельных crops учитывались как пустой output, чтобы отсутствие гипотезы не обрывало весь корпус; их число показано ниже.\n\n");
    for (model_index, (model_name, _)) in models.iter().enumerate() {
        let total = records
            .iter()
            .filter(|record| record.model_index == model_index)
            .count();
        let errors = records
            .iter()
            .filter(|record| record.model_index == model_index && record.inference_error.is_some())
            .count();
        output.push_str(&format!(
            "- {model_name}: inference errors {errors}/{total}.\n"
        ));
    }
    output.push('\n');
    output.push_str(&format!(
        "Фильтр проверялся при `JW = {}`: нормализованный русский токен должен быть несловарным в ru-RU; распознанное слово — словарным в en-US; `JW(russian_sound_key(RU), english_sound_key(EN)) ≥ порога`. Ключи совпадают с текущим mixer в `dictation.rs`. Порог sweep: `{}`. Для каждого положительного микса целевой RU-токен — токен GigaAM с максимальным фонетическим JW к эталонному EN-термину; это исследовательская привязка без forced alignment. Precision/recall считаются по целевым окнам; каждый принятый нецелевой кандидат считается false accept.\n\n",
        "russian_sound_key", FILTER_THRESHOLDS.map(|t| format!("{t:.2}")).join(", ")
    ));

    output.push_str("## Тайминги GigaAM\n\n");
    output.push_str("В `src-tauri/src/asr.rs::timed_words` слово собирается из token timestamps: если `durations[index]` содержит конечную положительную длительность, конец токена = timestamp + duration; иначе конец = timestamp следующего токена (включая special/EOS token, если он следующий). Только если следующего timestamp вообще нет, используется `audio_duration`; времена ограничиваются длительностью аудио. Конец слова — конец последнего BPE-токена. У GigaAM transducer длительности отсутствуют, поэтому внутренние границы слов приближённые и иногда доходят до timestamp следующего токена; приведённая ниже проверка лишь сопоставляет конец последнего слова с концом речевого сигнала, не измеряет внутренние границы. WAV содержит тишину после речи, а при последнем token без следующего timestamp fallback может захватить её.\n\n");
    output.push_str("| Источник | WAV с транскрипцией | |Δ| median | |Δ| p90 | в ±150 мс | end ≈ конец WAV | max |\n|---|---:|---:|---:|---:|---:|---:|\n");
    for source in ["mix", "mix2", "dictation"] {
        let rows = timing_errors(phrases, source);
        if !rows.is_empty() {
            output.push_str(&format!(
                    "| {source} | {} | {:.0} ms | {:.0} ms | {}/{} | {}/{} | {:.0} ms |\n",
                    rows.len(),
                    median_abs(&rows),
                    percentile_abs(&rows, 0.90),
                    rows.iter().filter(|error| error.abs() <= 0.150).count(),
                    rows.len(),
                    phrases
                        .iter()
                        .filter(|phrase| phrase.input.source == source
                            && phrase
                                .timed
                                .words
                                .last()
                                .is_some_and(|word| (word.end - phrase.duration).abs() <= 0.01))
                        .count(),
                    phrases
                        .iter()
                        .filter(|phrase| phrase.input.source == source
                            && !phrase.timed.words.is_empty())
                        .count(),
                    rows.iter().map(|error| error.abs()).fold(0.0_f32, f32::max) * 1000.0,
                ));
        }
    }
    output.push_str("\n`|Δ| = end последнего слова − последний ненулевой сэмпл по модулю. Это измеряет хвостовой конец, а не точность границы каждого внутреннего слова: у sherpa нет длительностей GigaAM token-ов, внутренние концы только ставятся на следующий token timestamp.\n\n");

    output.push_str("## Модель × pad: слово, без дополнительной тишины, порог 0.80\n\n");
    output.push_str("| Модель | pad | exact raw | принятый exact | recall | precision | FP slang | FP dictation | EN-dict any/all | EN-dict any/nonword | EN-dict target | мс/слово |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for model_index in 0..models.len() {
        for pad_ms in PADS_MS {
            let condition = Condition {
                model_index,
                pad_ms,
                window: WindowMode::Word,
                extra_silence: false,
                threshold: 0.80,
            };
            let stats = summarize(phrases, records, condition);
            let (precision, recall) = precision_recall(&stats, positive_count);
            let exact_raw = target_raw_exact_count(phrases, records, condition);
            let en_dict = stats.target_dictionary_any;
            let latency = average_latency(records, condition);
            output.push_str(&format!(
                "| {} | {} ms | {}/{} | {}/{} | {:.1}% | {:.1}% | {} | {} | {}/{} | {}/{} | {}/{} | {:.1} |\n",
                models[model_index].0,
                pad_ms,
                exact_raw,
                positive_count,
                stats.target_true_positive,
                positive_count,
                recall * 100.0,
                precision * 100.0,
                stats.accepted_false_slang,
                stats.accepted_false_dictation,
                stats.mix_dictionary_any,
                stats.mix_windows,
                stats.mix_nonword_dictionary_any,
                stats.mix_nonword_windows,
                en_dict,
                positive_count,
                latency
            ));
        }
    }
    output.push_str("\nRaw exact — слово встретилось в output целевого GigaAM-кандидата без фильтра; accepted exact дополнительно прошло оба словаря и JW-порог. Precision учитывает все принятые нецелевые crops из mix, mix2, сленга и диктовки; столбцы FP slang/FP dictation показывают отдельно только два контрольных набора. Для этой диктовки подходящих отрицательных crops нет (см. число выше). `мс/слово` — время одного `transcribe_verbatim` crop, включая модельную обработку, без чтения WAV и spellcheck.\n\n");

    let best_by_model: Vec<BestCondition> = (0..models.len())
        .filter_map(|model_index| best_condition(phrases, records, model_index, positive_count))
        .collect();
    let best_global = best_by_model.iter().cloned().max_by(compare_best);
    output.push_str("## Лучшая конфигурация на этой выборке\n\n");
    output.push_str("Оптимизатор перебрал 4 pad × 2 окна × 2 варианта тишины × 6 JW-порогов; качество ранжировано по F1, затем precision и recall. Это настройка и оценка на одном малом synthetic наборе, поэтому ожидаемо optimistic.\n\n");
    output.push_str("| Модель | pad | окно | +тишина | JW | exact/целей | recall | precision | FP slang | FP dictation | FP прочих mix | F1 | мс/слово |\n|---|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for (model_index, best) in best_by_model.iter().enumerate() {
        append_best_row(
            &mut output,
            models[model_index].0,
            best,
            positive_count,
            records,
        );
    }
    if let Some(best) = &best_global {
        output.push_str("\n**Выбранная общая комбинация:** ");
        output.push_str(&format_condition(&best.condition, models));
        output.push_str(&format!(
            "; recall {:.1}%, precision {:.1}%, F1 {:.3}.\n\n",
            best.recall * 100.0,
            best.precision * 100.0,
            best.f1
        ));
    }

    output.push_str(&gigaam_timing_table(phrases));

    output.push_str("## Пофразовая таблица\n\n");
    output.push_str("Для каждой модели здесь показан output в её собственной выбранной комбинации из предыдущей таблицы. Значок `✓` означает точный целевой термин на выделенном кандидате и прохождение фильтра; `·` — иначе.\n\n");
    output.push_str("| ID | Эталон | RU-кандидат (JW) | Parakeet v2 | Parakeet v3 | Qwen3-ASR | Whisper small |\n|---|---|---|---|---|---|---|\n");
    for (phrase_index, phrase) in phrases.iter().enumerate().filter(|(_, p)| {
        (p.input.source == "mix" || p.input.source == "mix2") && !p.target_terms.is_empty()
    }) {
        let target_labels = phrase
            .target_terms
            .iter()
            .map(|target| {
                let token = target
                    .token_index
                    .map(|index| phrase.tokens[index].text.as_str())
                    .unwrap_or("—");
                format!(
                    "{}←{} ({:.2})",
                    target.expected_word, token, target.similarity
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let mut cells = Vec::new();
        for best in &best_by_model {
            let result = phrase
                .target_terms
                .iter()
                .filter_map(|target| {
                    let token_index = target.token_index?;
                    let record = find_record(records, phrase_index, token_index, best.condition)?;
                    let token = &phrase.tokens[token_index];
                    let matched = accepted_word(record, token, best.condition.threshold)
                        .map(|(word, _)| word);
                    let exact = matched.as_deref() == Some(target.expected_word.as_str());
                    Some(format!(
                        "{} {}={}",
                        if exact { "✓" } else { "·" },
                        target.expected_word,
                        abbreviate(&record.text, 36)
                    ))
                })
                .collect::<Vec<_>>()
                .join("; ");
            cells.push(if result.is_empty() {
                "—".to_string()
            } else {
                result
            });
        }
        output.push_str(&format!(
            "| {} | `{}` | `{}` | {} | {} | {} | {} |\n",
            md(&phrase.input.id),
            md(&phrase.input.phrase),
            md(&target_labels),
            cells.get(0).map(String::as_str).unwrap_or("—"),
            cells.get(1).map(String::as_str).unwrap_or("—"),
            cells.get(2).map(String::as_str).unwrap_or("—"),
            cells.get(3).map(String::as_str).unwrap_or("—")
        ));
    }

    if let Some(best) = &best_global {
        output.push_str("\n## Примеры ложных срабатываний общей конфигурации\n\n");
        output.push_str("| Корпус / фраза | RU-токен | EN output | JW |\n|---|---|---|---:|\n");
        let false_examples = false_accept_examples(phrases, records, best.condition, 20);
        if false_examples.is_empty() {
            output.push_str("| — | — | на этой выборке нет принятых false positives | — |\n");
        } else {
            for (phrase_index, record, word, score) in false_examples {
                let phrase = &phrases[phrase_index];
                output.push_str(&format!(
                    "| {}/{} | `{}` | `{}` | {:.2} |\n",
                    phrase.input.source,
                    md(&phrase.input.id),
                    md(&phrase.tokens[record.token_index].text),
                    md(&word),
                    score
                ));
            }
        }
    }

    output.push_str("\n## Ограничения\n\n");
    output.push_str("- Термин из `mix` привязан к GigaAM-токену по максимальному phonetic JW, а не независимому forced alignment; если GigaAM слила слово с соседом, целевой crop может быть смещён.\n");
    output.push_str("- Диктовка синтезирована; ложные срабатывания оцениваются только на несловарных RU-кандидатах. Из окон, пересекающих эталонные ASCII-English `parts`, кандидаты исключены, чтобы настоящую английскую речь не засчитать как false positive.\n");
    output.push_str("- `whisper-small (en)` — multilingual `small`, загруженный с принудительным `language=en`; доступной отдельной small.en модели в папке ресурсов нет.\n");
    output.push_str("- Словарный результат зависит от установленных языковых пакетов Windows. Коллизии, имена брендов и русский сленг могут быть помечены не так, как ожидает пользователь.\n");
    output.push_str("- Латентность снята во время параллельных прогонов в общем дереве. Для оценки CPU бюджета нужен повтор лучшей конфигурации в тишине и на живой речи.\n");
    output
}

fn gigaam_timing_table(phrases: &[PreparedPhrase]) -> String {
    let mut output = String::new();
    output.push_str("## GigaAM token timings по фразам\n\n");
    output.push_str("Для каждой mix/mix2 фразы перечислены слова `текст@start–end`, времена в секундах от начала WAV. Это интервалы, которые реально возвращает `transcribe_timed`; внутренние концы GigaAM приблизительны (см. аудит выше).\n\n");
    output.push_str("| ID | Фраза | GigaAM слова `текст@start–end, s` |\n|---|---|---|\n");
    for phrase in phrases
        .iter()
        .filter(|phrase| phrase.input.source == "mix" || phrase.input.source == "mix2")
    {
        let timed_words = phrase
            .tokens
            .iter()
            .map(|token| format!("{}@{:.2}–{:.2}", md(&token.text), token.start, token.end))
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!(
            "| {} | `{}` | `{}` |\n",
            md(&phrase.input.id),
            md(&phrase.input.phrase),
            timed_words
        ));
    }
    output.push('\n');
    output
}

fn timing_errors(phrases: &[PreparedPhrase], source: &str) -> Vec<f32> {
    phrases
        .iter()
        .filter(|phrase| phrase.input.source == source)
        .filter_map(|phrase| {
            phrase
                .timed
                .words
                .last()
                .map(|word| word.end - phrase.last_signal)
        })
        .collect()
}

fn median_abs(values: &[f32]) -> f32 {
    let mut values: Vec<f32> = values.iter().map(|value| value.abs()).collect();
    values.sort_by(f32::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0) * 1000.0
}

fn percentile_abs(values: &[f32], percentile: f32) -> f32 {
    let mut values: Vec<f32> = values.iter().map(|value| value.abs()).collect();
    values.sort_by(f32::total_cmp);
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() - 1) as f32 * percentile).round() as usize;
    values[index] * 1000.0
}

fn summarize(phrases: &[PreparedPhrase], records: &[Record], condition: Condition) -> Stats {
    let mut stats = Stats::default();
    for record in records
        .iter()
        .filter(|record| same_geometry(record, condition))
    {
        let phrase = &phrases[record.phrase_index];
        let token = &phrase.tokens[record.token_index];
        let is_mix = phrase.input.source == "mix" || phrase.input.source == "mix2";
        if is_mix {
            stats.mix_windows += 1;
            let dictionary_output = record
                .english_words
                .iter()
                .any(|word| word.dictionary == Some(true));
            if dictionary_output {
                stats.mix_dictionary_any += 1;
            }
            if token.ru_dictionary == Some(false) {
                stats.mix_nonword_windows += 1;
                if dictionary_output {
                    stats.mix_nonword_dictionary_any += 1;
                }
            }
        }
        match phrase.input.kind.as_str() {
            "russian_slang" => stats.slang_windows += 1,
            _ if phrase.input.source == "dictation" => stats.dictation_windows += 1,
            _ => {}
        }

        let target_terms: Vec<&TargetAssignment> = phrase
            .target_terms
            .iter()
            .filter(|target| target.token_index == Some(record.token_index))
            .collect();
        let any_dictionary = record
            .english_words
            .iter()
            .any(|word| word.dictionary == Some(true));
        for target in &target_terms {
            if record
                .english_words
                .iter()
                .any(|word| word.text == target.expected_word)
            {
                stats.target_exact_raw += 1;
            }
            if any_dictionary {
                stats.target_dictionary_any += 1;
            }
        }

        let accepted = accepted_word(record, token, condition.threshold);
        let Some((accepted_text, _score)) = accepted else {
            continue;
        };
        let is_true_positive = target_terms
            .iter()
            .any(|target| target.expected_word == accepted_text);
        if is_true_positive {
            stats.target_true_positive += 1;
        } else if phrase.input.source == "dictation" {
            stats.accepted_false_dictation += 1;
            stats.dictation_phrases_hit.insert(phrase.input.id.clone());
        } else if phrase.input.kind == "russian_slang" {
            stats.accepted_false_slang += 1;
            stats.slang_phrases_hit.insert(phrase.input.id.clone());
        } else if phrase.input.source == "mix2" {
            stats.accepted_false_mix2 += 1;
            stats.mix2_phrases_hit.insert(phrase.input.id.clone());
        } else {
            stats.accepted_false_mix += 1;
        }
    }
    stats
}

fn accepted_word(record: &Record, token: &TokenInfo, threshold: f64) -> Option<(String, f64)> {
    if token.ru_dictionary != Some(false) {
        return None;
    }
    let russian_key = russian_sound_key(&token.text);
    record
        .english_words
        .iter()
        .filter(|word| word.dictionary == Some(true))
        .map(|word| {
            (
                word.text.clone(),
                strsim::jaro_winkler(&russian_key, &english_sound_key(&word.text)),
            )
        })
        .filter(|(_, score)| *score >= threshold)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn same_geometry(record: &Record, condition: Condition) -> bool {
    record.model_index == condition.model_index
        && record.pad_ms == condition.pad_ms
        && record.window == condition.window
        && record.extra_silence == condition.extra_silence
}

fn precision_recall(stats: &Stats, positive_count: usize) -> (f64, f64) {
    let false_positives = stats.accepted_false_slang
        + stats.accepted_false_dictation
        + stats.accepted_false_mix
        + stats.accepted_false_mix2;
    let accepted = stats.target_true_positive + false_positives;
    let precision = if accepted == 0 {
        0.0
    } else {
        stats.target_true_positive as f64 / accepted as f64
    };
    let recall = if positive_count == 0 {
        0.0
    } else {
        stats.target_true_positive as f64 / positive_count as f64
    };
    (precision, recall)
}

fn target_raw_exact_count(
    phrases: &[PreparedPhrase],
    records: &[Record],
    condition: Condition,
) -> usize {
    records
        .iter()
        .filter(|record| same_geometry(record, condition))
        .map(|record| {
            let phrase = &phrases[record.phrase_index];
            phrase
                .target_terms
                .iter()
                .filter(|target| target.token_index == Some(record.token_index))
                .filter(|target| {
                    record
                        .english_words
                        .iter()
                        .any(|word| word.text == target.expected_word)
                })
                .count()
        })
        .sum()
}

fn best_condition(
    phrases: &[PreparedPhrase],
    records: &[Record],
    model_index: usize,
    positive_count: usize,
) -> Option<BestCondition> {
    let mut best: Option<BestCondition> = None;
    for pad_ms in PADS_MS {
        for window in [WindowMode::Word, WindowMode::Neighbors] {
            for extra_silence in [false, true] {
                for threshold in FILTER_THRESHOLDS {
                    let condition = Condition {
                        model_index,
                        pad_ms,
                        window,
                        extra_silence,
                        threshold,
                    };
                    let stats = summarize(phrases, records, condition);
                    let (precision, recall) = precision_recall(&stats, positive_count);
                    let f1 = if precision + recall == 0.0 {
                        0.0
                    } else {
                        2.0 * precision * recall / (precision + recall)
                    };
                    let candidate = BestCondition {
                        condition,
                        stats,
                        f1,
                        precision,
                        recall,
                    };
                    if best.as_ref().is_none_or(|current| {
                        compare_best(&candidate, current) == std::cmp::Ordering::Greater
                    }) {
                        best = Some(candidate);
                    }
                }
            }
        }
    }
    best
}

fn compare_best(a: &BestCondition, b: &BestCondition) -> std::cmp::Ordering {
    a.f1.total_cmp(&b.f1)
        .then_with(|| a.precision.total_cmp(&b.precision))
        .then_with(|| a.recall.total_cmp(&b.recall))
        .then_with(|| b.condition.pad_ms.cmp(&a.condition.pad_ms))
        .then_with(|| {
            (a.condition.window == WindowMode::Word).cmp(&(b.condition.window == WindowMode::Word))
        })
        .then_with(|| b.condition.extra_silence.cmp(&a.condition.extra_silence))
        .then_with(|| a.condition.threshold.total_cmp(&b.condition.threshold))
}

fn average_latency(records: &[Record], condition: Condition) -> f64 {
    let mut total = 0.0;
    let mut count = 0usize;
    for record in records
        .iter()
        .filter(|record| same_geometry(record, condition))
    {
        total += record.elapsed_ms;
        count += 1;
    }
    if count == 0 {
        0.0
    } else {
        total / count as f64
    }
}

fn append_best_row(
    output: &mut String,
    model_name: &str,
    best: &BestCondition,
    positive_count: usize,
    records: &[Record],
) {
    let c = best.condition;
    let latency = average_latency(records, c);
    output.push_str(&format!(
        "| {model_name} | {} ms | {} | {} | {:.2} | {}/{} | {:.1}% | {:.1}% | {} | {} | {} | {:.3} | {:.1} |\n",
        c.pad_ms, c.window.name(), if c.extra_silence { "yes" } else { "no" }, c.threshold,
        best.stats.target_true_positive, positive_count, best.recall * 100.0, best.precision * 100.0,
        best.stats.accepted_false_slang, best.stats.accepted_false_dictation,
        best.stats.accepted_false_mix + best.stats.accepted_false_mix2,
        best.f1, latency
    ));
}

fn find_record(
    records: &[Record],
    phrase_index: usize,
    token_index: usize,
    condition: Condition,
) -> Option<&Record> {
    records.iter().find(|record| {
        record.phrase_index == phrase_index
            && record.token_index == token_index
            && same_geometry(record, condition)
    })
}

fn false_accept_examples<'a>(
    phrases: &'a [PreparedPhrase],
    records: &'a [Record],
    condition: Condition,
    limit: usize,
) -> Vec<(usize, &'a Record, String, f64)> {
    let mut examples: Vec<_> = records
        .iter()
        .filter(|record| same_geometry(record, condition))
        .filter_map(|record| {
            let phrase = &phrases[record.phrase_index];
            let (word, score) = accepted_word(
                record,
                &phrase.tokens[record.token_index],
                condition.threshold,
            )?;
            let true_positive = phrase.target_terms.iter().any(|target| {
                target.token_index == Some(record.token_index) && target.expected_word == word
            });
            if true_positive {
                return None;
            }
            Some((record.phrase_index, record, word, score))
        })
        .collect();
    examples.sort_by(|a, b| b.3.total_cmp(&a.3));
    examples.truncate(limit);
    examples
}

fn format_condition(condition: &Condition, models: &[(&str, &str); 4]) -> String {
    format!(
        "{} / pad={} ms / {} / silence={} / JW≥{:.2}",
        models[condition.model_index].0,
        condition.pad_ms,
        condition.window.name(),
        if condition.extra_silence {
            "300 ms"
        } else {
            "off"
        },
        condition.threshold
    )
}

fn abbreviate(text: &str, max_chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max_chars {
        return md(&text);
    }
    format!("{}…", md(&text.chars().take(max_chars).collect::<String>()))
}

fn md(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}
