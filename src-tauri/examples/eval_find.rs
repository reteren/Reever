//! End-to-end evaluation for «ривер найди …»: real Engine → dispatch::route →
//! Everything find::search, with a fresh empty history for every synthesized utterance.
//!
//! Run from src-tauri/: cargo run --release --example eval_find

use std::path::{Path, PathBuf};

use reever_lib::{
    actions::{load_actions, Action},
    asr::{Asr, AsrConfig},
    bilingual::LangMode,
    dispatch::{route, Route},
    engine::{Engine, EngineConfig, EngineEvent},
    everything,
    find::{self, FindScope, Usage},
    grammar::Grammar,
    langid::SpeechLid,
    speech::samples,
    vad::SileroVad,
};
use serde::Deserialize;

const CHUNK: usize = 320;
const MATCH_THRESHOLD: f64 = 0.70;
const HOST: &str = "127.0.0.1";
const PORT: u16 = 8011;

#[derive(Deserialize)]
struct Case {
    file: String,
    target_id: String,
    target_name: String,
    target_path: String,
    target_kind: String,
    language: String,
    phrase: String,
    style: String,
    voice: String,
    utterance: String,
}

struct ResultRow {
    case: Case,
    heard: Vec<(String, Option<String>)>,
    route: String,
    scope: Option<FindScope>,
    candidates: Vec<String>,
    items: Vec<find::FindItem>,
    rank: Option<usize>,
    matching_queries: Vec<String>,
    error: String,
}

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("../testdata");
    let model = root.join("resources/model");
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(testdata.join("find-manifest.json"))?)?;
    anyhow::ensure!(!cases.is_empty(), "find-manifest.json is empty; run scripts/gen-corpus.ps1 -Find first");

    let ru = Asr::new(&AsrConfig {
        model_dir: model.join("sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16"),
        num_threads: 4,
    })?;
    let en = Asr::new(&AsrConfig {
        model_dir: model.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"),
        num_threads: 4,
    })?;
    let lid = SpeechLid::new(&model.join("sherpa-onnx-whisper-base"), 2)?;
    let grammar = Grammar::defaults();
    let actions_path = root.join("resources/actions.json");
    let actions: Vec<Action> = load_actions(&actions_path);
    let wake_words = vec!["ривер".into(), "ревер".into(), "ривьер".into(), "ливер".into()];
    let mut engine = Engine::new(
        EngineConfig { wake_words, partial_interval_ms: 0, ..EngineConfig::default() },
        ru,
        SileroVad::new(&model.join("silero_vad.onnx"), 0.5)?,
    );
    engine.set_grammar(grammar.clone());
    engine.set_bilingual(Some(Box::new(en)), Some(Box::new(lid)), LangMode::Auto);

    let mut rows = Vec::with_capacity(cases.len());
    for (index, case) in cases.into_iter().enumerate() {
        let path = testdata.join("corpus/find-benchmark").join(&case.file);
        let mut pcm = read_wav(&path)?;
        // Corpus WAV already has 1 s lead, a 650 ms internal pause where applicable,
        // and 3.5 s tail; a little extra silence guarantees VAD closure on every build.
        pcm.extend(std::iter::repeat(0i16).take(samples(1_000)));
        let mut heard = Vec::new();
        for chunk in pcm.chunks(CHUNK) {
            for event in engine.push_audio(chunk) {
                if let EngineEvent::Command { text, alt } = event {
                    heard.push((text, alt));
                }
            }
        }
        engine.reset();

        let mut route_name = "no command".to_string();
        let mut scope = None;
        let mut candidates = Vec::new();
        let mut items = Vec::new();
        let mut matching_queries = Vec::new();
        let mut error = String::new();
        for (text, alt) in &heard {
            match route(text, alt.as_deref(), &grammar, &actions, MATCH_THRESHOLD) {
                Route::Find { scope: found_scope, candidates: found_candidates } => {
                    route_name = "find".into();
                    scope = Some(found_scope);
                    candidates = found_candidates;
                    match find::search(HOST, PORT, &candidates, found_scope, &Usage::default(), &Default::default()) {
                        Ok(found) => items = found,
                        Err(e) => error = e,
                    }
                    matching_queries = variants_that_find(&candidates, found_scope, &case.target_path);
                    break;
                }
                other => route_name = route_label(&other).into(),
            }
        }
        let rank = items.iter().position(|item| same_path(&item.path, &case.target_path)).map(|i| i + 1);
        println!(
            "[{}/{}] {} {} {} -> {} rank={:?}", index + 1, rows.capacity(), case.target_id,
            case.style, case.language, route_name, rank
        );
        rows.push(ResultRow { case, heard, route: route_name, scope, candidates, items, rank, matching_queries, error });
    }

    let report = render_report(&rows);
    std::fs::write(testdata.join("find-report.md"), &report)?;
    println!("\n{}", aggregate_summary(&rows));
    println!("report: {}", testdata.join("find-report.md").display());
    Ok(())
}

fn variants_that_find(candidates: &[String], scope: FindScope, target: &str) -> Vec<String> {
    let prefix = match scope {
        FindScope::Any => "",
        FindScope::Folder => "folder: ",
        FindScope::File => "file: ",
    };
    let mut matches = Vec::new();
    'candidate: for variant in find::query_variants(candidates).iter().take(4) {
        let squashed = find::squash(variant);
        for (label, query) in [
            ("wfn", format!("{prefix}wfn:{squashed}")),
            ("startwith", format!("{prefix}startwith:{squashed}")),
            ("words", format!("{prefix}{variant}")),
        ] {
            if everything::search_items(HOST, PORT, &query, 120)
                .ok()
                .is_some_and(|found| found.iter().any(|(path, _)| same_path(&path.to_string_lossy(), target)))
            {
                matches.push(format!("{variant} ({label})"));
                // Keep searching later variants: Russian vs. English/translit ambiguity is
                // part of the question this benchmark is intended to answer.
                if matches.len() >= 6 { break 'candidate; }
                break;
            }
        }
    }
    matches
}

fn route_label(route: &Route) -> &'static str {
    match route {
        Route::Find { .. } => "find",
        Route::Unknown => "unknown",
        Route::Launch(_) => "launch",
        Route::LaunchFallback(_) => "launch fallback",
        Route::Folder(_) => "folder",
        Route::Search { .. } => "search",
        Route::System(_) => "system",
        Route::Close(_) => "close",
        Route::Kill(_) => "kill",
        Route::Calc(_) => "calc",
        Route::Dictate(_) => "dictate",
        Route::WebChat { .. } => "web chat",
    }
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path)?;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        if id == b"data" {
            return Ok(bytes[offset + 8..(offset + 8 + length).min(bytes.len())]
                .chunks_exact(2).map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]])).collect());
        }
        offset += 8 + length + (length & 1);
    }
    anyhow::bail!("{} has no WAV data chunk", path.display())
}

fn markdown_cell(text: &str) -> String { text.replace('|', "\\|").replace('\n', " ") }

fn render_report(rows: &[ResultRow]) -> String {
    let mut out = String::from("# Find benchmark: русские, английские и смешанные цели\n\n");
    out.push_str("Engine: GigaAM v3 RU + Parakeet TDT v2 EN + Whisper Base LID + Silero VAD; затем `dispatch::route` и `find::search` на Everything HTTP 127.0.0.1:8011. История `Usage` и список recent пустые для каждого запроса. WAV синтезированы COM SAPI голосами Microsoft Irina и Alexandr (RHVoice), слитно и с цифровой паузой 650 мс после «найди».\n\n");
    out.push_str(&aggregate_summary(rows));
    out.push_str("\n\n`Engine` печатает выбранную команду и альтернативу; оба поля включены ниже. `variant (wfn/startwith/words)` означает первый проверенный Everything HTTP-запрос этого кандидата, вернувший точный путь.\n\n");
    out.push_str("| Цель | Язык | Форма | Режим | Голос | Engine text / alt | Route/scope | Кандидаты | Rank | Кандидат → запрос, нашедший цель | Диагностика |\n|---|---|---|---|---|---|---|---|---:|---|---|\n");
    for row in rows {
        let outputs = if row.heard.is_empty() { "—".into() } else {
            row.heard.iter().map(|(text, alt)| format!("{} / {}", text, alt.as_deref().unwrap_or("—"))).collect::<Vec<_>>().join("<br>")
        };
        let rank = row.rank.map(|r| r.to_string()).unwrap_or_else(|| "miss".into());
        let scope = row.scope.map(|s| format!("{} / {:?}", row.route, s)).unwrap_or_else(|| row.route.clone());
        let mut diagnostics = Vec::new();
        if row.rank.is_none() { diagnostics.push("target absent from top 40".to_string()); }
        if row.route != "find" { diagnostics.push(format!("unexpected route: {}", row.route)); }
        if !row.error.is_empty() { diagnostics.push(row.error.clone()); }
        if row.heard.is_empty() { diagnostics.push("Engine emitted no Command".into()); }
        let top = row.items.iter().take(3).map(|i| format!("{} ({})", i.name, i.path)).collect::<Vec<_>>().join("; ");
        if row.rank.is_none() && !top.is_empty() { diagnostics.push(format!("top results: {top}")); }
        out.push_str(&format!(
            "| {} `{}` [{}] | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&row.case.target_name), markdown_cell(&row.case.target_id), row.case.target_kind,
            row.case.language, markdown_cell(&format!("{} · {}", row.case.phrase, row.case.utterance)),
            row.case.style, row.case.voice, markdown_cell(&outputs), markdown_cell(&scope),
            markdown_cell(&row.candidates.join("; ")), rank,
            markdown_cell(&row.matching_queries.join("; ")),
            markdown_cell(&diagnostics.join("; "))
        ));
    }
    out
}

fn aggregate_summary(rows: &[ResultRow]) -> String {
    let mut out = String::from("## Метрики\n\n| Язык цели | Стиль | n | top-1 | top-3 | в top-40 | route=find |\n|---|---|---:|---:|---:|---:|---:|\n");
    for language in ["en", "ru", "mixed"] {
        for style in ["joined", "pause_after_find"] {
            let group: Vec<_> = rows.iter().filter(|r| r.case.language == language && r.case.style == style).collect();
            if group.is_empty() { continue; }
            let top1 = group.iter().filter(|r| r.rank == Some(1)).count();
            let top3 = group.iter().filter(|r| r.rank.is_some_and(|rank| rank <= 3)).count();
            let listed = group.iter().filter(|r| r.rank.is_some()).count();
            let routed = group.iter().filter(|r| r.route == "find").count();
            out.push_str(&format!("| {language} | {style} | {} | {top1} | {top3} | {listed} | {routed} |\n", group.len()));
        }
    }
    let top1 = rows.iter().filter(|r| r.rank == Some(1)).count();
    let top3 = rows.iter().filter(|r| r.rank.is_some_and(|rank| rank <= 3)).count();
    let listed = rows.iter().filter(|r| r.rank.is_some()).count();
    let routed = rows.iter().filter(|r| r.route == "find").count();
    out.push_str(&format!("\nВсего: **{}/{} top-1**, **{}/{} top-3**, **{}/{} в top-40**, **{}/{} корректно направлено в find**.\n", top1, rows.len(), top3, rows.len(), listed, rows.len(), routed, rows.len()));
    out
}
