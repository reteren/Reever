use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use strsim::jaro_winkler;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const MAX_HITS: usize = 9; // ХАРД-ЛИМИТ: не больше 9 окон одновременно

/// Результат поиска: ранжированные пути + опциональная ошибка связи.
pub struct SearchOutcome {
    pub paths: Vec<PathBuf>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct EvtResponse {
    #[serde(default)]
    results: Vec<EvtItem>,
}

#[derive(Deserialize)]
struct EvtItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String, // родительский каталог
    /// "file" | "folder"
    #[serde(default, rename = "type")]
    kind: String,
}

/// Корректная склейка родитель+имя. ВАЖНО: `PathBuf::join("E:", x)` на Windows даёт
/// drive-relative `E:x` (без слэша) — Everything для корня отдаёт `path="E:"`.
fn join_path(parent: &str, name: &str) -> PathBuf {
    if parent.is_empty() {
        return PathBuf::from(name);
    }
    let mut s = String::with_capacity(parent.len() + 1 + name.len());
    s.push_str(parent);
    if !parent.ends_with('\\') && !parent.ends_with('/') {
        s.push('\\');
    }
    s.push_str(name);
    PathBuf::from(s)
}

/// Percent-encoding для query (кириллица -> UTF-8 %XX).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Сырой HTTP/1.0 GET к localhost-серверу Everything. Без внешних зависимостей.
fn http_get(host: &str, port: u16, path: &str) -> Result<String, String> {
    let addr = format!("{host}:{port}");
    let mut stream =
        TcpStream::connect(&addr).map_err(|e| format!("нет связи с {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}:{port}\r\nUser-Agent: reever\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("send: {e}"))?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|e| format!("recv: {e}"))?;
    Ok(buf.splitn(2, "\r\n\r\n").nth(1).unwrap_or("").to_string())
}

/// Запрос к Everything HTTP -> список полных путей.
fn search_raw(host: &str, port: u16, query: &str, count: usize) -> Result<Vec<PathBuf>, String> {
    let path = format!(
        "/?search={}&json=1&path_column=1&count={}",
        url_encode(query),
        count
    );
    println!("[everything] GET http://{host}:{port}{path}");

    let body = http_get(host, port, &path)?;
    let resp: EvtResponse = serde_json::from_str(&body)
        .map_err(|e| format!("не разобрал ответ ({e}); тело: {:.160}", body))?;

    let paths: Vec<PathBuf> = resp
        .results
        .into_iter()
        .map(|i| join_path(&i.path, &i.name))
        .collect();

    println!("[everything] results={}", paths.len());
    Ok(paths)
}

/// Поиск для «найди …»: полные пути и признак папки. Пунктуация и пробелы в именах не
/// учитываются (`ignore_punctuation`, `ignore_whitespace` плагина HTTP Server 1.5):
/// «project gst» находит «-PROJECT.GST-».
pub fn search_items(
    host: &str,
    port: u16,
    query: &str,
    count: usize,
) -> Result<Vec<(PathBuf, bool)>, String> {
    let path = format!(
        "/?search={}&json=1&path_column=1&count={}&ignore_punctuation=1&ignore_whitespace=1",
        url_encode(query),
        count
    );
    let body = http_get(host, port, &path)?;
    let resp: EvtResponse = serde_json::from_str(&body)
        .map_err(|e| format!("не разобрал ответ ({e}); тело: {:.160}", body))?;
    Ok(resp
        .results
        .into_iter()
        .map(|item| (join_path(&item.path, &item.name), item.kind == "folder"))
        .collect())
}

/// Ослабляем каждое слово до префикса (отбрасываем вероятное окончание) —
/// чтобы «собирательное» сматчилось с «Собирательная». Подстроки, НЕ wildcard
/// (Everything со `*` якорит на начало имени).
fn loose_terms(query: &str) -> String {
    query
        .split_whitespace()
        .map(|w| {
            let chars: Vec<char> = w.chars().collect();
            let keep = match chars.len() {
                0..=3 => chars.len(),
                4..=5 => chars.len() - 1,
                _ => chars.len() - 2,
            };
            chars[..keep].iter().collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Двухпроходный поиск: сначала точная фраза, при 0 результатов — ослабленные префиксы.
fn retrieve(
    host: &str,
    port: u16,
    name: &str,
    mk_query: impl Fn(&str) -> String,
) -> Result<Vec<PathBuf>, String> {
    let mut items = search_raw(host, port, &mk_query(name), 64)?;
    if items.is_empty() {
        let loose = loose_terms(name);
        if loose.trim() != name.trim() && !loose.trim().is_empty() {
            println!("[everything] loose retry: {loose:?}");
            items = search_raw(host, port, &mk_query(&loose), 64)?;
        }
    }
    Ok(items)
}

/// Ключ ранжирования (меньше — лучше): похожесть имени -> тип -> локация -> длина.
fn rank_key(full: &Path, query: &str, is_exe: bool) -> (u32, u8, u8, usize) {
    let q = query.trim().to_lowercase();
    let stem = full.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    let name = full.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    let path_l = full.to_string_lossy().to_lowercase();

    // 1) fuzzy-похожесть сказанного на имя (точное совпадение -> 1.0 -> ранг 0)
    let sim = jaro_winkler(&q, &stem).max(jaro_winkler(&q, &name));
    let sim_rank = ((1.0 - sim) * 10_000.0) as u32;

    // 2) для запуска: ярлык > url > exe
    let type_rank = if is_exe {
        match full.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("lnk") => 0,
            Some("url") => 1,
            Some("exe") => 2,
            _ => 3,
        }
    } else {
        0
    };

    // 3) локация: программы — меню «Пуск»; папки — профиль пользователя
    let loc_rank = if is_exe {
        if path_l.contains("start menu") || path_l.contains("главное меню") { 0 } else { 1 }
    } else {
        match std::env::var("USERPROFILE") {
            Ok(up) if path_l.starts_with(&up.to_lowercase()) => 0,
            _ => 1,
        }
    };

    (sim_rank, type_rank, loc_rank, path_l.len())
}

fn ranked(mut paths: Vec<PathBuf>, query: &str, is_exe: bool) -> Vec<PathBuf> {
    paths.sort_by_key(|p| rank_key(p, query, is_exe));
    for (i, p) in paths.iter().take(5).enumerate() {
        println!("[everything]   #{i} -> {}", p.display());
    }
    paths
}

/// Лёгкая проверка доступности HTTP-сервера Everything.
pub fn ping(host: &str, port: u16) -> Result<(), String> {
    search_raw(host, port, "ping", 1).map(|_| ())
}

/// "Открой": папки (серверный фильтр `folder:`) + двухпроходный поиск + fuzzy-ранг.
pub fn find_folders(host: &str, port: u16, name: &str) -> SearchOutcome {
    match retrieve(host, port, name, |q| format!("{q} folder:")) {
        Ok(paths) => SearchOutcome { paths: ranked(paths, name, false), error: None },
        Err(e) => SearchOutcome { paths: Vec::new(), error: Some(e) },
    }
}

/// "Запусти": исполняемые / ярлыки (фильтр `ext:`) + двухпроходный поиск + fuzzy-ранг.
pub fn find_executables(host: &str, port: u16, name: &str) -> SearchOutcome {
    match retrieve(host, port, name, |q| format!("ext:exe;lnk;url {q}")) {
        Ok(paths) => SearchOutcome { paths: ranked(paths, name, true), error: None },
        Err(e) => SearchOutcome { paths: Vec::new(), error: Some(e) },
    }
}

/// Открыть указанные папки в проводнике. Жёсткий лимит 9 окон.
pub fn open_folders(folders: &[PathBuf]) -> usize {
    let mut opened = 0;
    for dir in folders.iter().take(MAX_HITS) {
        let ok = Command::new("explorer")
            .arg(dir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok();
        if ok {
            opened += 1;
        }
    }
    opened
}
