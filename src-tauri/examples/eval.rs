//! Прогон синтетического корпуса через весь конвейер: Silero VAD → Segmenter → Engine.
//! Аудио подаётся кусками по 20 мс с тишиной до и после фразы.
//!
//! Корпус: scripts/gen-corpus.ps1. Запуск из src-tauri/:
//!   cargo run --release --example eval [-- --noise-db -25] [--model <папка>]
//!     [--second-model <папка>] [--lid-model <папка>]
//! Отчёт: testdata/eval-report*.md. EVAL_TRACE=1 печатает события по каждому файлу.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use reever_lib::actions::{load_actions, Action};
use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::bilingual::LangMode;
use reever_lib::dispatch::{route, Route};
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::grammar::Grammar;
use reever_lib::langid::SpeechLid;
use reever_lib::speech::{samples, Lang, LanguageId, Transcriber};
use reever_lib::textnorm::normalize;
use reever_lib::vad::SileroVad;
use serde::Deserialize;
use strsim::jaro_winkler;

const CHUNK: usize = 320; // 20 мс
const LEAD_MS: u32 = 1_000;
const TAIL_MS: u32 = 2_000;
const DEFAULT_SECOND_MODEL: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";
const DEFAULT_LID_MODEL: &str = "sherpa-onnx-whisper-base";

#[derive(Deserialize)]
struct Case {
    file: String,
    scenario: String,
    voice: String,
    text: String,
    expected: Vec<String>,
    wake_heard: bool,
    #[serde(default = "default_expected_lang")]
    expected_lang: String,
    #[serde(default)]
    expected_prefix: String,
    #[serde(default)]
    expected_payload: String,
}

fn default_expected_lang() -> String {
    "ru".into()
}

#[derive(Clone)]
struct Candidate {
    text: String,
    alt: Option<String>,
}

struct Outcome {
    commands: Vec<Candidate>,
    wake_heard: usize,
    /// Задержка последней команды после конца аудиофразы, мс аудио-времени.
    latency_ms: Option<f64>,
}

struct Row<'a> {
    case: &'a Case,
    out: Outcome,
    /// exact — точный текст; close — тот же маршрут / достаточно похожая команда.
    verdict: &'static str,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let noise_db: Option<f32> = arg("--noise-db").map(|value| value.parse()).transpose()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("resources/model");
    let testdata = root.join("../testdata");
    let model = arg("--model")
        .unwrap_or_else(|| "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16".into());
    let model_dir = model_path(&models, &model);
    let second_model = arg("--second-model")
        .unwrap_or_else(|| DEFAULT_SECOND_MODEL.into());
    let lid_model = arg("--lid-model").unwrap_or_else(|| DEFAULT_LID_MODEL.into());
    let second_dir = model_path(&models, &second_model);
    let lid_dir = model_path(&models, &lid_model);

    let cases: Vec<Case> =
        serde_json::from_str(&std::fs::read_to_string(testdata.join("manifest.json"))?)?;
    let wake_words: Vec<String> =
        ["ривер", "ревер", "ривьер", "ливер"].iter().map(|word| word.to_string()).collect();

    let asr_started = Instant::now();
    let mut engine = Engine::new(
        EngineConfig {
            wake_words,
            partial_interval_ms: 0,
            ..EngineConfig::default()
        },
        Asr::new(&AsrConfig {
            model_dir,
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
                eprintln!(
                    "предупреждение: вторая ASR-модель {} не загрузилась ({error}); запуск без второго языка",
                    second_dir.display()
                );
                None
            }
        }
    } else {
        eprintln!(
            "предупреждение: вторая ASR-модель не найдена: {}; запуск без второго языка",
            second_dir.display()
        );
        None
    };
    let lid: Option<Box<dyn LanguageId + Send>> = if second.is_some() {
        if lid_model.trim().is_empty() {
            eprintln!("предупреждение: LID не задан; язык цели выберет текстовая эвристика");
            None
        } else if lid_dir.is_dir() {
            match SpeechLid::new(&lid_dir, 4) {
                Ok(lid) => Some(Box::new(lid)),
                Err(error) => {
                    eprintln!(
                        "предупреждение: LID-модель {} не загрузилась ({error}); язык цели выберет текстовая эвристика",
                        lid_dir.display()
                    );
                    None
                }
            }
        } else {
            eprintln!(
                "предупреждение: LID-модель не найдена: {}; язык цели выберет текстовая эвристика",
                lid_dir.display()
            );
            None
        }
    } else {
        None
    };
    let has_second = second.is_some();
    let has_lid = lid.is_some();
    engine.set_bilingual(second, lid, LangMode::Auto);
    eprintln!(
        "модели загружены за {:.1} с (вторая ASR: {}, LID: {})",
        asr_started.elapsed().as_secs_f64(),
        if has_second { "да" } else { "нет" },
        if has_lid { "да" } else { "нет" }
    );

    // Раскладка по умолчанию из редактора: «ривер папка» / «ривер загугли» без цели —
    // незаконченная команда, Engine дослушивает.
    let grammar = Grammar::defaults();
    engine.set_grammar(grammar.clone());
    let actions: Vec<Action> = load_actions(&root.join("resources/actions.json"))
        .into_iter()
        .filter(|action| action.kind != "search")
        .collect();
    let routing = Routing { grammar, actions };

    let trace = std::env::var_os("EVAL_TRACE").is_some();
    let mut rows = Vec::new();
    let mut compute = 0.0_f64;
    let mut audio_s = 0.0_f64;
    for (index, case) in cases.iter().enumerate() {
        let speech = read_wav(&testdata.join("corpus").join(&case.file))?;
        audio_s += speech.len() as f64 / 16_000.0;
        if trace {
            eprintln!("{} [{}] {}", case.file, case.voice, case.text);
        }
        let started = Instant::now();
        let out = run_case(&mut engine, &speech, noise_db, index as u64, trace);
        compute += started.elapsed().as_secs_f64();
        rows.push(judge(case, out, &routing));
    }

    let model_label = format!(
        "{} + {}{}",
        model,
        if has_second { second_model.as_str() } else { "без второго языка" },
        if has_lid { " + LID" } else { "" }
    );
    let report = render(&rows, &model_label, noise_db, compute, audio_s);
    let name = match noise_db {
        Some(db) => format!("eval-report-noise{}.md", db.abs()),
        None => "eval-report.md".into(),
    };
    std::fs::write(testdata.join(&name), &report)?;
    println!("{report}");
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
    anyhow::ensure!(wave.sample_rate() == 16_000, "{}: нужен 16 кГц", path.display());
    Ok(wave
        .samples()
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
        .collect())
}

fn run_case(
    engine: &mut Engine<Asr, SileroVad>,
    speech: &[i16],
    noise_db: Option<f32>,
    seed: u64,
    trace: bool,
) -> Outcome {
    engine.reset();
    let lead = samples(LEAD_MS);
    let mut audio = vec![0i16; lead];
    audio.extend_from_slice(speech);
    audio.extend(std::iter::repeat_n(0, samples(TAIL_MS)));
    if let Some(db) = noise_db {
        add_noise(&mut audio, db, seed);
    }
    let speech_end = lead + speech.len();

    let mut out = Outcome {
        commands: Vec::new(),
        wake_heard: 0,
        latency_ms: None,
    };
    for (index, chunk) in audio.chunks(CHUNK).enumerate() {
        for event in engine.push_audio(chunk) {
            if trace {
                eprintln!("  {:>6} мс  {event:?}", (index + 1) * CHUNK / 16);
            }
            match event {
                EngineEvent::Command { text, alt } => {
                    let at = (index + 1) * CHUNK;
                    out.latency_ms = Some((at as f64 - speech_end as f64) / 16.0);
                    out.commands.push(Candidate { text, alt });
                }
                EngineEvent::WakeHeard => out.wake_heard += 1,
                _ => {}
            }
        }
    }
    out
}

/// Белый шум заданного уровня относительно полной шкалы (детерминированный LCG).
fn add_noise(audio: &mut [i16], db: f32, seed: u64) {
    let amp = 32_767.0 * 10f32.powf(db / 20.0) * 3f32.sqrt();
    let mut value = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    for sample in audio.iter_mut() {
        value = value
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let unit = ((value >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
        *sample = (*sample as f32 + unit * amp).clamp(-32_768.0, 32_767.0) as i16;
    }
}

/// Грамматика и реестр для функционального вердикта через dispatch::route.
struct Routing {
    grammar: Grammar,
    actions: Vec<Action>,
}

impl Routing {
    fn route(&self, text: &str, alt: Option<&str>) -> Route {
        route(text, alt, &self.grammar, &self.actions, 0.7)
    }
}

fn judge<'a>(case: &'a Case, out: Outcome, routing: &Routing) -> Row<'a> {
    let got: Vec<String> = out
        .commands
        .iter()
        .map(|candidate| normalize(&candidate.text))
        .collect();
    let want: Vec<String> = case
        .expected
        .iter()
        .map(|command| normalize(command))
        .collect();
    let wake_ok = !case.wake_heard || out.wake_heard == 1;
    let verdict = if got.len() != want.len() || !wake_ok {
        "FAIL"
    } else if got == want {
        "exact"
    } else if out.commands.iter().zip(&want).all(|(candidate, expected)| {
        same_command(&candidate.text, candidate.alt.as_deref(), expected, routing)
    }) {
        "close"
    } else {
        "FAIL"
    };
    Row { case, out, verdict }
}

/// Та же ли команда с точки зрения приложения: одинаковый route или похожий текст.
fn same_command(got: &str, alt: Option<&str>, want: &str, routing: &Routing) -> bool {
    let wanted = routing.route(want, None);
    if wanted != Route::Unknown
        && !matches!(wanted, Route::LaunchFallback(_))
        && routing.route(got, alt) == wanted
    {
        return true;
    }
    let action = Action {
        id: String::new(),
        label: String::new(),
        kind: "shell".into(),
        target: String::new(),
        verbs: Vec::new(),
        phrases: vec![want.to_string()],
    };
    action.score(&[normalize(got)]) >= 0.7 || jaro_winkler(&normalize(got), &normalize(want)) >= 0.9
}

fn target_payload(text: &str, prefix: &str) -> String {
    let text = normalize(text);
    let prefix = normalize(prefix);
    if prefix.is_empty() {
        return text;
    }
    text.strip_prefix(&prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
        .unwrap_or(&text)
        .trim()
        .to_string()
}

fn output_language(payload: &str) -> Option<Lang> {
    let (mut cyrillic, mut latin) = (0, 0);
    for character in payload.chars() {
        if ('а'..='я').contains(&character) || character == 'ё' {
            cyrillic += 1;
        } else if character.is_ascii_alphabetic() {
            latin += 1;
        }
    }
    match (cyrillic, latin) {
        (0, 0) => None,
        (cyrillic, latin) if cyrillic >= latin => Some(Lang::Ru),
        _ => Some(Lang::En),
    }
}

fn cer(expected: &str, got: &str) -> f64 {
    let expected = normalize(expected);
    let got = normalize(got);
    let denominator = expected.chars().count().max(1);
    strsim::levenshtein(&expected, &got) as f64 / denominator as f64
}

fn render(rows: &[Row], model: &str, noise_db: Option<f32>, compute: f64, audio_s: f64) -> String {
    let mut report = format!("# Reever eval — {model}\n\n");
    report += &format!(
        "Шум: {}. Файлов: {}. RTF конвейера: {:.3}.\n\n",
        noise_db.map_or("нет".into(), |db| format!("{db} дБFS")),
        rows.len(),
        compute / audio_s.max(1e-9)
    );
    report += "| сценарий | язык цели | всего | exact | close | fail | язык верен | средний CER en | alt точнее text по CER | задержка, мс (сред.) |\n";
    report += "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n";

    let mut groups: BTreeMap<(&str, &str), Vec<&Row>> = BTreeMap::new();
    for row in rows {
        groups
            .entry((row.case.scenario.as_str(), row.case.expected_lang.as_str()))
            .or_default()
            .push(row);
    }

    let mut totals = (0, 0, 0);
    let (mut language_hits, mut language_total) = (0, 0);
    let (mut english_cer, mut alt_cer, mut alt_better, mut english_total) =
        (0.0, 0.0, 0, 0);
    for ((scenario, language), group) in groups {
        let exact = group.iter().filter(|row| row.verdict == "exact").count();
        let close = group.iter().filter(|row| row.verdict == "close").count();
        let fail = group.iter().filter(|row| row.verdict == "FAIL").count();
        totals.0 += exact;
        totals.1 += close;
        totals.2 += fail;

        let target_rows: Vec<&Row> = group
            .iter()
            .copied()
            .filter(|row| !row.case.expected_payload.is_empty())
            .collect();
        let target_lang_hits = target_rows
            .iter()
            .filter(|row| {
                row.out
                    .commands
                    .first()
                    .and_then(|candidate| {
                        output_language(&target_payload(
                            &candidate.text,
                            &row.case.expected_prefix,
                        ))
                    })
                    .is_some_and(|detected| {
                        (language == "en" && detected == Lang::En)
                            || (language == "ru" && detected == Lang::Ru)
                    })
            })
            .count();
        language_hits += target_lang_hits;
        language_total += target_rows.len();

        let latency: Vec<f64> = group
            .iter()
            .filter_map(|row| row.out.latency_ms)
            .collect();
        let average_latency = average(&latency).map_or("—".into(), |value| format!("{value:.0}"));
        let lang_cell = if target_rows.is_empty() {
            "—".into()
        } else {
            format!("{target_lang_hits}/{}", target_rows.len())
        };
        let mut cer_cell = "—".to_string();
        let mut alt_cell = "—".to_string();

        if language == "en" && !target_rows.is_empty() {
            let mut main_sum = 0.0;
            let mut alt_sum = 0.0;
            let mut better_count = 0;
            for row in &target_rows {
                let expected = normalize(&row.case.expected_payload);
                let selected = row
                    .out
                    .commands
                    .first()
                    .map(|candidate| {
                        target_payload(&candidate.text, &row.case.expected_prefix)
                    })
                    .unwrap_or_default();
                let selected_cer = cer(&expected, &selected);
                main_sum += selected_cer;
                let alternate = row.out.commands.first().and_then(|candidate| {
                    candidate.alt.as_deref().map(|text| {
                        target_payload(text, &row.case.expected_prefix)
                    })
                });
                let alternate_cer = alternate
                    .as_deref()
                    .map(|text| cer(&expected, text))
                    .unwrap_or(1.0);
                alt_sum += alternate_cer;
                if alternate.is_some() && alternate_cer < selected_cer {
                    better_count += 1;
                }
            }
            let count = target_rows.len();
            english_cer += main_sum;
            alt_cer += alt_sum;
            alt_better += better_count;
            english_total += count;
            cer_cell = format!("{:.3}", main_sum / count as f64);
            alt_cell = format!("{better_count}/{count}");
        }
        report += &format!(
            "| {scenario} | {language} | {} | {exact} | {close} | {fail} | {lang_cell} | {cer_cell} | {alt_cell} | {average_latency} |\n",
            group.len()
        );
    }

    let accepted = totals.0 + totals.1;
    report += &format!(
        "\n**Итого: {accepted}/{} ({:.0}%)** — exact {}, close {}, fail {}.\n",
        rows.len(),
        100.0 * accepted as f64 / rows.len().max(1) as f64,
        totals.0,
        totals.1,
        totals.2
    );
    if language_total > 0 {
        report += &format!(
            "\nТочность выбора языка цели: {language_hits}/{language_total} ({:.1}%).\n",
            100.0 * language_hits as f64 / language_total as f64
        );
    }
    if english_total > 0 {
        report += &format!(
            "Средний CER английской цели: {:.3}; средний CER варианта alt: {:.3}; alt точнее основного: {alt_better}/{english_total} ({:.1}%).\n",
            english_cer / english_total as f64,
            alt_cer / english_total as f64,
            100.0 * alt_better as f64 / english_total as f64
        );
    }
    let latency: Vec<f64> = rows
        .iter()
        .filter_map(|row| row.out.latency_ms)
        .collect();
    if let Some(value) = average(&latency) {
        report += &format!("Средняя задержка: {value:.0} мс.\n");
    }

    report += "\n## Неточные и провалы\n\n| файл | сценарий | язык цели | голос | сказано | ожидалось | text | alt | wake |\n|---|---|---|---|---|---|---|---|---|\n";
    for row in rows.iter().filter(|row| row.verdict != "exact") {
        let commands: Vec<(&str, &str)> = row
            .out
            .commands
            .iter()
            .map(|candidate| (candidate.text.as_str(), candidate.alt.as_deref().unwrap_or("")))
            .collect();
        report += &format!(
            "| {} {} | {} | {} | {} | {} | {:?} | {:?} | {:?} | {} |\n",
            row.case.file,
            row.verdict,
            row.case.scenario,
            row.case.expected_lang,
            row.case.voice,
            row.case.text,
            row.case.expected,
            commands.iter().map(|(text, _)| *text).collect::<Vec<_>>(),
            commands.iter().map(|(_, alt)| *alt).collect::<Vec<_>>(),
            row.out.wake_heard
        );
    }
    report
}

fn average(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}
