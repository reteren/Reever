//! Поиск «ривер найди …» на живом Everything (только чтение): что попадёт в окно выбора.
//!
//! cargo run --example find_probe -- any|folder|file "кандидат 1" ["кандидат 2"…]

use reever_lib::find::{self, FindScope, Usage};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scope = match args.first().map(String::as_str) {
        Some("folder") => FindScope::Folder,
        Some("file") => FindScope::File,
        _ => FindScope::Any,
    };
    // «… формат X» — как в dispatch: имя до слова «формат», формат из словаря.
    let mut candidates = Vec::new();
    let mut formats = Vec::new();
    for arg in &args[1..] {
        let (name, format) = find::split_format(arg);
        if !name.is_empty() {
            candidates.push(name);
        }
        if let Some(format) = format {
            formats.push(format);
        }
    }
    let extension = find::resolve_extension(&formats);
    println!("варианты: {:?} · формат: {:?}", find::query_variants(&candidates), extension);
    let recent = find::windows_recent();
    println!("недавних Windows: {}", recent.len());
    match find::search_with_extension(
        "127.0.0.1",
        8011,
        &candidates,
        scope,
        extension.as_deref(),
        &Usage::default(),
        &recent,
    ) {
        Ok(items) => {
            for (i, item) in items.iter().take(12).enumerate() {
                println!(
                    "{:>2}. {}{} {:.2}  {}",
                    i + 1,
                    if item.frequent { "★ " } else { "  " },
                    if item.folder { "📁" } else { "📄" },
                    item.score,
                    item.path
                );
            }
            println!("всего показано: {}", items.len());
        }
        Err(error) => println!("ошибка: {error}"),
    }
}
