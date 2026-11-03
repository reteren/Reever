//! Сборка без Tauri: таблица вшитых ресурсов интерфейса (assets/ → OUT_DIR/assets.rs, читает
//! `app::AssetResolver`) и ресурсы exe — значок, сведения о версии, манифест (reever.rc).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Папки assets/, которые нужны окнам во время работы (шрифты и строки вшиты отдельно).
const EMBED: &[&str] = &["fonts", "mascot", "skin"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets = root.join("assets");
    let mut files = Vec::new();
    for d in EMBED {
        walk(&assets.join(d), &mut files);
        println!("cargo:rerun-if-changed=assets/{d}");
    }
    let mut table: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|p| (p.strip_prefix(&assets).unwrap().to_string_lossy().replace('\\', "/"), p))
        .collect();
    table.sort();
    let mut src = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (name, path) in &table {
        let _ = writeln!(src, "    ({name:?}, include_bytes!({:?})),", path.to_string_lossy());
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("assets.rs");
    std::fs::write(out, src).unwrap();

    // значок, версия, манифест (DPI, Common Controls 6) — только в exe
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = version.split('.').collect();
    let comma = format!("{},{},{},0", parts[0], parts.get(1).unwrap_or(&"0"), parts.get(2).unwrap_or(&"0"));
    println!("cargo:rerun-if-changed=windows/reever.rc");
    println!("cargo:rerun-if-changed=windows/reever.exe.manifest");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    embed_resource::compile("windows/reever.rc", [format!("VERSION_COMMA={comma}"), format!("VERSION_STR=\"{version}\"")])
        .manifest_required()
        .unwrap();
}
