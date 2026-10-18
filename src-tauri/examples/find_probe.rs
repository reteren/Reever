//! Поиск «ривер найди …» по индексу дисков (только чтение): что попадёт в окно выбора.
//! Берёт снимок индекса установленного Reever, без него — обходит диски заново.
//!
//! cargo run --example find_probe -- any|folder|file "кандидат 1" ["кандидат 2"…]

use reever_lib::find::{self, FindScope, Usage};
use reever_lib::local_index::DiskIndex;

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
    let index = DiskIndex::load_or_scan(&installed_snapshot());
    println!("записей в индексе: {}", index.entries());
    let items = find::search(
        &index,
        &candidates,
        scope,
        extension.as_deref(),
        &Usage::default(),
        &recent,
    );
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

/// Снимок индекса установленного Reever (`app_config_dir` Tauri).
fn installed_snapshot() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default())
        .join("com.troid.reever")
        .join("index-v1.bin")
}
