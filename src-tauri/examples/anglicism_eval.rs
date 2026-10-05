//! Замер `anglicism::Anglicisms` на кейсах из testdata/lexicon (build_lexicon.py):
//! транскрипции GigaAM смешанных фраз, примеры пользователя и все слова журнала диктовки.
//!
//!   cargo run --release --example anglicism_eval [-- --terms opacity,eraser] [--probe хайд,хедер]
//!
//! Пишет testdata/anglicism-report.md. Все подмены на словах журнала перечисляются:
//! там есть и настоящие английские слова пользователя, поэтому их проверяет человек.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use reever_lib::anglicism::Anglicisms;
use serde::Deserialize;

#[derive(Deserialize)]
struct PositiveCase {
    id: String,
    set: String,
    text: String,
    expected_groups: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct NegativeToken {
    token: String,
    occurrences: usize,
}

#[derive(Deserialize)]
struct Negatives {
    tokens: Vec<NegativeToken>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let terms: Vec<String> = args
        .iter()
        .position(|a| a == "--terms")
        .and_then(|i| args.get(i + 1))
        .map(|list| list.split(',').map(str::to_string).collect())
        .unwrap_or_default();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let lexicon_dir = root.join("testdata/lexicon");
    let started = Instant::now();
    let words = Anglicisms::load(&root.join("src-tauri/resources/anglicisms.tsv"), &terms)?;
    let load_ms = started.elapsed().as_millis();

    let mut cache: BTreeMap<String, Option<bool>> = BTreeMap::new();
    let cache_cell = std::cell::RefCell::new(&mut cache);
    let is_russian = |word: &str| -> Option<bool> {
        if let Some(hit) = cache_cell.borrow().get(word) {
            return *hit;
        }
        let result = reever_lib::spellcheck::misspelled_ratio("ru-RU", word)
            .map(|(ratio, count)| count == 1 && ratio == 0.0);
        cache_cell.borrow_mut().insert(word.to_string(), result);
        result
    };

    // --probe хайд,хедер — только показать решения для отдельных слов.
    if let Some(list) = args.iter().position(|a| a == "--probe").and_then(|i| args.get(i + 1)) {
        for word in list.split(',') {
            println!(
                "{word} → {}   ru-RU: {:?}   {:?}",
                words.restore(word, &is_russian),
                is_russian(word),
                words.explain(word)
            );
        }
        return Ok(());
    }

    let positives: Vec<PositiveCase> = serde_json::from_str(
        &std::fs::read_to_string(lexicon_dir.join("positive-cases.json"))
            .context("positive-cases.json")?,
    )?;
    let negatives: Negatives = serde_json::from_str(
        &std::fs::read_to_string(lexicon_dir.join("negative-cases.json"))
            .context("negative-cases.json")?,
    )?;

    let mut report = String::new();
    let mut by_set: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut rows = String::new();
    let mut russian_changed = 0;
    let mut russian_cases = 0;
    let mut timings = Vec::new();
    let mut wrong: Vec<String> = Vec::new();
    for case in &positives {
        let t = Instant::now();
        let out = words.restore(&case.text, &is_russian);
        timings.push(t.elapsed().as_secs_f64() * 1000.0);
        let lower = out.to_lowercase();
        let tokens: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .collect();
        let hits = case
            .expected_groups
            .iter()
            .filter(|group| {
                group.iter().any(|expected| {
                    let parts: Vec<String> =
                        expected.to_lowercase().split_whitespace().map(str::to_string).collect();
                    tokens.windows(parts.len()).any(|w| w.iter().zip(&parts).all(|(a, b)| a == b))
                })
            })
            .count();
        let slot = by_set.entry(case.set.clone()).or_default();
        slot.0 += hits;
        slot.1 += case.expected_groups.len();
        if out != case.text && hits < case.expected_groups.len() {
            wrong.push(format!("{} → {}", case.text, out));
        }
        if case.expected_groups.is_empty() {
            russian_cases += 1;
            if out != case.text {
                russian_changed += 1;
            }
        }
        let _ = writeln!(
            rows,
            "| {} | {} | {} | {}/{} |",
            case.id,
            case.text,
            if out == case.text { "·".to_string() } else { out.clone() },
            hits,
            case.expected_groups.len()
        );
    }

    let mut changes: Vec<(String, String, usize)> = Vec::new();
    for item in &negatives.tokens {
        let out = words.restore(&item.token, &is_russian);
        if out != item.token {
            changes.push((item.token.clone(), out, item.occurrences));
        }
    }

    timings.sort_by(f64::total_cmp);
    let _ = writeln!(report, "# Словарь англицизмов в диктовке — замер\n");
    let _ = writeln!(
        report,
        "Словарь: resources/anglicisms.tsv, загрузка {load_ms} мс. Слова пользователя: {}.\n",
        if terms.is_empty() { "нет".to_string() } else { terms.join(", ") }
    );
    let _ = writeln!(report, "## Восстановлено английских слов\n\n| Набор | Найдено |\n|---|---:|");
    let (mut hit_total, mut all_total) = (0, 0);
    for (set, (hits, total)) in &by_set {
        hit_total += hits;
        all_total += total;
        let _ = writeln!(report, "| {set} | {hits}/{total} |");
    }
    let _ = writeln!(report, "| **всего** | **{hit_total}/{all_total}** |\n");
    let _ = writeln!(
        report,
        "Чисто русские фразы корпуса изменены: **{russian_changed}/{russian_cases}**. \
         Время на фразу: медиана {:.2} мс, максимум {:.2} мс (со словарём Windows).\n",
        timings.get(timings.len() / 2).copied().unwrap_or(0.0),
        timings.last().copied().unwrap_or(0.0)
    );
    let _ = writeln!(
        report,
        "## Подмены на словах журнала диктовки ({} слов, {} употреблений)\n\n\
         Журнал — живая речь пользователя: здесь и верные английские слова, и ошибки.\n\n\
         | Было | Стало | Раз |\n|---|---|---:|",
        changes.len(),
        changes.iter().map(|c| c.2).sum::<usize>()
    );
    for (from, to, count) in &changes {
        let _ = writeln!(report, "| {from} | {to} | {count} |");
    }
    let _ = writeln!(report, "\n## По фразам\n\n| ID | GigaAM | После | Термины |\n|---|---|---|---:|");
    report.push_str(&rows);

    let path = root.join("testdata/anglicism-report.md");
    std::fs::write(&path, &report)?;
    println!("{}", report.lines().take(20).collect::<Vec<_>>().join("\n"));
    println!("\nЛишние подмены в корпусе:");
    for line in &wrong {
        println!("  {line}");
    }
    println!("\nПодмены в журнале:");
    for (from, to, count) in &changes {
        println!("  {from} → {to} ×{count}   {:?}", words.explain(from));
    }
    println!("\nОтчёт: {}", path.display());
    Ok(())
}
