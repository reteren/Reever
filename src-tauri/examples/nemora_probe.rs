//! Команды Неморы на своей библиотеке без голоса и без воспроизведения: фраза → маршрут →
//! что нашлось бы. Только чтение (поиск, списки), очередь и плеер не трогаются.
//!
//!   cargo run --example nemora_probe -- "включи трек smells like teen spirit" "play playlist goat"

use reever_lib::dispatch::{route_with, Route};
use reever_lib::grammar::Grammar;

fn main() {
    let phrases: Vec<String> = std::env::args().skip(1).collect();
    if phrases.is_empty() {
        eprintln!("usage: nemora_probe \"включи трек …\" [\"включи альбом …\" …]");
        std::process::exit(2);
    }
    match reever_lib::nemora::find_mcp() {
        Some(path) => println!("nemora-mcp: {}", path.display()),
        None => {
            eprintln!(
                "Nemora is not installed (or set {})",
                reever_lib::nemora::MCP_ENV
            );
            std::process::exit(1);
        }
    }
    let grammar = Grammar::defaults();
    for phrase in &phrases {
        match route_with(phrase, None, &grammar, &[], 0.7, true) {
            Route::Music { op, queries } => match reever_lib::nemora::preview(op, &queries) {
                Ok(Some(found)) => println!("«{phrase}» → {op:?} {queries:?} → {found}"),
                Ok(None) if op.needs_target() => {
                    println!("«{phrase}» → {op:?} {queries:?} → not found")
                }
                Ok(None) => println!("«{phrase}» → {op:?} (no search)"),
                Err(error) => println!("«{phrase}» → {op:?} {queries:?} → error: {error}"),
            },
            other => println!("«{phrase}» → not a Nemora command: {other:?}"),
        }
    }
    reever_lib::nemora::shutdown();
}
