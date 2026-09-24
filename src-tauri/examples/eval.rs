//! Прогон синтетического корпуса через весь конвейер: Silero VAD → Segmenter →
//! Engine → GigaAM. Аудио подаётся кусками по 20 мс, как из микрофона, с тишиной до
//! и после фразы — так проверяются и паузы, и распознавание разом.
//!
//! Корпус: `scripts/gen-corpus.ps1`. Запуск из src-tauri/:
//!   cargo run --release --example eval [-- --noise-db -25] [--model <папка>]
//! Отчёт: testdata/eval-report.md (или eval-report-noise<дБ>.md).
//! EVAL_TRACE=1 печатает события Engine по каждому файлу.

use std::path::{Path, PathBuf};
use std::time::Instant;

use reever_lib::actions::{load_actions, Action};
use reever_lib::dispatch::{route, Route};
use reever_lib::grammar::Grammar;
use reever_lib::asr::{Asr, AsrConfig};
use reever_lib::engine::{Engine, EngineConfig, EngineEvent};
use reever_lib::speech::samples;
use reever_lib::textnorm::normalize;
use reever_lib::vad::SileroVad;
use serde::Deserialize;
use strsim::jaro_winkler;

const CHUNK: usize = 320; // 20 мс
const LEAD_MS: u32 = 1_000;
const TAIL_MS: u32 = 2_000;

#[derive(Deserialize)]
struct Case {
    file: String,
    scenario: String,
    voice: String,
    text: String,
    expected: Vec<String>,
    wake_heard: bool,
}

struct Outcome {
    commands: Vec<String>,
    wake_heard: usize,
    /// Задержка последней команды после конца аудио фразы, мс аудио-времени.
    latency_ms: Option<f64>,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let noise_db: Option<f32> = arg("--noise-db").map(|v| v.parse()).transpose()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = arg("--model")
        .unwrap_or_else(|| "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16".into());
    let model_dir = root.join("resources/model").join(&model);
    let testdata = root.join("../testdata");

    let cases: Vec<Case> =
        serde_json::from_str(&std::fs::read_to_string(testdata.join("manifest.json"))?)?;
    let wake_words: Vec<String> =
        ["ривер", "ревер", "ривьер", "ливер"].iter().map(|s| s.to_string()).collect();

    let asr_started = Instant::now();
    let mut engine = Engine::new(
        EngineConfig { wake_words, partial_interval_ms: 0, ..EngineConfig::default() },
        Asr::new(&AsrConfig { model_dir, num_threads: 4 })?,
        SileroVad::new(&root.join("resources/model/silero_vad.onnx"), 0.5)?,
    );
    eprintln!("модель загружена за {:.1} с", asr_started.elapsed().as_secs_f64());
    // Раскладка по умолчанию из редактора: «ривер папка» / «ривер загугли» без цели —
    // незаконченная команда, Engine дослушивает.
    let grammar = Grammar::defaults();
    engine.set_grammar(grammar.clone());
    // Поиск живёт в грамматике; старые search-действия из бандла сюда не берём.
    let actions: Vec<Action> = load_actions(&root.join("resources/actions.json"))
        .into_iter()
        .filter(|a| a.kind != "search")
        .collect();
    let routing = Routing { grammar, actions };

    let trace = std::env::var_os("EVAL_TRACE").is_some();
    let mut rows = Vec::new();
    let mut compute = 0.0_f64;
    let mut audio_s = 0.0_f64;
    for (i, case) in cases.iter().enumerate() {
        let speech = read_wav(&testdata.join("corpus").join(&case.file))?;
        audio_s += speech.len() as f64 / 16_000.0;
        if trace {
            eprintln!("{} [{}] {}", case.file, case.voice, case.text);
        }
        let started = Instant::now();
        let out = run_case(&mut engine, &speech, noise_db, i as u64, trace);
        compute += started.elapsed().as_secs_f64();
        rows.push(judge(case, out, &routing));
    }

    let report = render(&rows, &model, noise_db, compute, audio_s);
    let name = match noise_db {
        Some(db) => format!("eval-report-noise{}.md", db.abs()),
        None => "eval-report.md".into(),
    };
    std::fs::write(testdata.join(&name), &report)?;
    println!("{report}");
    Ok(())
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let wave = sherpa_onnx::Wave::read(path.to_string_lossy().as_ref())
        .ok_or_else(|| anyhow::anyhow!("не прочитан {}", path.display()))?;
    anyhow::ensure!(wave.sample_rate() == 16_000, "{}: нужен 16 кГц", path.display());
    Ok(wave.samples().iter().map(|s| (s.clamp(-1.0, 1.0) * 32_767.0) as i16).collect())
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

    let mut out = Outcome { commands: Vec::new(), wake_heard: 0, latency_ms: None };
    for (n, chunk) in audio.chunks(CHUNK).enumerate() {
        for ev in engine.push_audio(chunk) {
            if trace {
                eprintln!("  {:>6} мс  {ev:?}", (n + 1) * CHUNK / 16);
            }
            match ev {
                EngineEvent::Command(t) => {
                    let at = (n + 1) * CHUNK;
                    out.latency_ms = Some((at as f64 - speech_end as f64) / 16.0);
                    out.commands.push(t);
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
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    for s in audio.iter_mut() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let u = ((x >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
        *s = (*s as f32 + u * amp).clamp(-32_768.0, 32_767.0) as i16;
    }
}

struct Row<'a> {
    case: &'a Case,
    out: Outcome,
    /// exact — точный текст; close — текст отличается, но матчинг действий приложения
    /// (Action::score, порог 0.7 по умолчанию) считает его той же командой.
    verdict: &'static str,
}

/// Грамматика и реестр для функционального вердикта через `dispatch::route`.
struct Routing {
    grammar: Grammar,
    actions: Vec<Action>,
}

impl Routing {
    fn route(&self, text: &str) -> Route {
        route(text, &self.grammar, &self.actions, 0.7)
    }
}

fn judge<'a>(case: &'a Case, out: Outcome, routing: &Routing) -> Row<'a> {
    let got: Vec<String> = out.commands.iter().map(|c| normalize(c)).collect();
    let want: Vec<String> = case.expected.iter().map(|c| normalize(c)).collect();
    let wake_ok = !case.wake_heard || out.wake_heard == 1;
    let verdict = if got.len() != want.len() || !wake_ok {
        "FAIL"
    } else if got == want {
        "exact"
    } else if got.iter().zip(&want).all(|(g, w)| same_command(g, w, routing)) {
        "close"
    } else {
        "FAIL"
    };
    Row { case, out, verdict }
}

/// Та же ли команда с точки зрения приложения: одинаковый маршрут `dispatch::route`
/// (то же действие, та же папка, тот же поиск) или похожий текст («дискард» ≈ «дискорд»).
fn same_command(got: &str, want: &str, routing: &Routing) -> bool {
    let wanted = routing.route(want);
    if wanted != Route::Unknown && !matches!(wanted, Route::LaunchFallback(_)) && routing.route(got) == wanted {
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
    action.score(&[got.to_string()]) >= 0.7 || jaro_winkler(got, want) >= 0.9
}

fn render(rows: &[Row], model: &str, noise_db: Option<f32>, compute: f64, audio_s: f64) -> String {
    let mut s = String::new();
    s += &format!("# Reever eval — {model}\n\n");
    s += &format!(
        "Шум: {}. Файлов: {}. RTF конвейера: {:.3}.\n\n",
        noise_db.map_or("нет".into(), |d| format!("{d} дБFS")),
        rows.len(),
        compute / audio_s.max(1e-9)
    );
    s += "| сценарий | всего | exact | close | fail | задержка, мс (сред.) |\n|---|---|---|---|---|---|\n";
    let mut scenarios: Vec<&str> = rows.iter().map(|r| r.case.scenario.as_str()).collect();
    scenarios.dedup();
    let mut total = (0, 0, 0);
    for sc in scenarios {
        let rs: Vec<&Row> = rows.iter().filter(|r| r.case.scenario == sc).collect();
        let n = |v: &str| rs.iter().filter(|r| r.verdict == v).count();
        let lat: Vec<f64> = rs.iter().filter_map(|r| r.out.latency_ms).collect();
        let avg = if lat.is_empty() {
            "—".into()
        } else {
            format!("{:.0}", lat.iter().sum::<f64>() / lat.len() as f64)
        };
        s += &format!("| {sc} | {} | {} | {} | {} | {avg} |\n", rs.len(), n("exact"), n("close"), n("FAIL"));
        total = (total.0 + n("exact"), total.1 + n("close"), total.2 + n("FAIL"));
    }
    let ok = total.0 + total.1;
    s += &format!(
        "\n**Итого: {ok}/{} ({:.0}%)** — exact {}, close {}, fail {}.\n\n",
        rows.len(),
        100.0 * ok as f64 / rows.len() as f64,
        total.0,
        total.1,
        total.2
    );
    s += "## Неточные и провалы\n\n| файл | голос | сказано | ожидалось | получено | wake |\n|---|---|---|---|---|---|\n";
    for r in rows.iter().filter(|r| r.verdict != "exact") {
        s += &format!(
            "| {} {} | {} | {} | {:?} | {:?} | {} |\n",
            r.case.file, r.verdict, r.case.voice, r.case.text, r.case.expected, r.out.commands, r.out.wake_heard
        );
    }
    s
}
