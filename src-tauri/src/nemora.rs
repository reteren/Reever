//! Немора — музыкальный плеер того же автора. Если она установлена, у Reever есть музыкальные
//! команды («ривер включи трек …», «лайкни», «что играет»); нет — их не видно и они не
//! сравниваются. Ни кнопок, ни настроек: установку замечаем сами (`refresh`).
//!
//! Связь — её MCP-сервер `nemora-mcp.exe` (тот же, через который её видят ИИ-агенты): дочерний
//! процесс, JSON-RPC строками по stdin/stdout. Он сам читает библиотеку без запущенной Неморы,
//! сам запускает её для `play` и проверяет уровень доступа агентов (Немора → Настройки →
//! AI agents). Внутренний канал Неморы (именованный pipe) не трогаем: это её дело, а MCP —
//! описанный и стабильный контракт. Контракт команд: docs/nemora.md.
//!
//! Команда выполняется в своём потоке: запуск Неморы ждёт до 40 с, а аудио-поток всё это
//! время должен слушать. Звук успеха или ошибки воркер играет, когда команда закончится
//! (`take_outcomes`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::AppHandle;
use crate::events::ui_log;
use crate::grammar::MusicOp;
use crate::textnorm::normalize;

/// Путь к `nemora-mcp.exe` вместо установленного (сборка Неморы из исходников).
pub const MCP_ENV: &str = "REEVER_NEMORA_MCP";
const MCP_EXE: &str = "nemora-mcp.exe";
/// Запись установщика Неморы (NSIS): `InstallLocation` — папка программы, в кавычках.
const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Nemora";

/// Протокол MCP, на котором говорим. Немора (rmcp) принимает и его, и новее.
const PROTOCOL: &str = "2025-06-18";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// `play` и `resume` запускают Немору, если она закрыта: nemora-mcp ждёт её до 40 с.
const START_TIMEOUT: Duration = Duration::from_secs(70);
/// Сколько держать nemora-mcp без дела: следующая команда не платит за запуск процесса.
const IDLE: Duration = Duration::from_secs(5 * 60);
/// Как часто заново смотреть, установлена ли Немора (реестр и один файл — дёшево).
const RECHECK: Duration = Duration::from_secs(5);
/// list_songs отдаёт не больше 200 строк за раз.
const PAGE: usize = 200;
/// Предел для «вся библиотека»: длиннее очередь не нужна, а запрос растёт.
const LIBRARY_LIMIT: usize = 20_000;

// ─── Где Немора ──────────────────────────────────────────────────────────────

/// `nemora-mcp.exe` установленной Неморы: переменная `REEVER_NEMORA_MCP`, папка из записи
/// установщика (HKCU, затем HKLM), `%LOCALAPPDATA%\Nemora`.
pub fn find_mcp() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(MCP_ENV).map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    install_dirs()
        .into_iter()
        .map(|dir| dir.join(MCP_EXE))
        .find(|path| path.is_file())
}

fn install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            if let Some(dir) = registry_string(root, UNINSTALL_KEY, "InstallLocation")
                .and_then(|raw| clean_install_location(&raw))
            {
                dirs.push(dir);
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Nemora"));
    }
    dirs
}

/// `"C:\Users\…\Nemora"` из реестра: установщик пишет путь в кавычках, бывает и `%ENV%`.
pub(crate) fn clean_install_location(raw: &str) -> Option<PathBuf> {
    let cleaned = raw
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .trim_matches('"')
        .trim();
    if cleaned.is_empty() {
        return None;
    }
    #[cfg(windows)]
    let cleaned = crate::winutil::expand_env(cleaned);
    Some(PathBuf::from(cleaned.to_string()))
}

#[cfg(windows)]
fn registry_string(
    root: windows::Win32::System::Registry::HKEY,
    path: &str,
    name: &str,
) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_QUERY_VALUE, REG_EXPAND_SZ, REG_SZ,
        REG_VALUE_TYPE,
    };
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (path, name) = (wide(path), wide(name));
    let mut key = HKEY::default();
    if !unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), 0, KEY_QUERY_VALUE, &mut key) }.is_ok()
    {
        return None;
    }
    let mut buffer = vec![0u16; 2048];
    let mut size = (buffer.len() * 2) as u32;
    let mut kind = REG_VALUE_TYPE::default();
    let result = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if !result.is_ok() || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
        return None;
    }
    let len = (size as usize / 2).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Последняя проверка: когда и что нашли.
static FOUND: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);

/// Найти Немору заново (не чаще раза в `RECHECK`) и включить или спрятать её команды.
/// Возвращает `Some(установлена)`, если с прошлой проверки это изменилось.
pub fn refresh() -> Option<bool> {
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((checked, _)) = found.as_ref() {
        if checked.elapsed() < RECHECK {
            return None;
        }
    }
    let path = find_mcp();
    let installed = path.is_some();
    let changed = found
        .as_ref()
        .is_none_or(|(_, before)| before.is_some() != installed);
    *found = Some((Instant::now(), path));
    crate::grammar::set_nemora_installed(installed);
    changed.then_some(installed)
}

/// Путь к nemora-mcp по последней проверке (проверить, если её ещё не было).
fn mcp_path() -> Option<PathBuf> {
    refresh();
    FOUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|(_, path)| path.clone())
}

/// Раз в несколько секунд из аудио-потока: заметить установку или удаление Неморы и закрыть
/// nemora-mcp, который давно без дела.
pub fn tick(app: &AppHandle) {
    let gone = match refresh() {
        Some(true) => {
            ui_log(app, crate::msg!("log.music.on"));
            false
        }
        Some(false) => {
            ui_log(app, crate::msg!("log.music.off"));
            true
        }
        None => false,
    };
    // try_lock: идёт команда (play ждёт запуска Неморы до 40 с) — аудио-поток не ждёт её
    if let Ok(mut client) = CLIENT.try_lock() {
        if gone || client.as_ref().is_some_and(|c| c.used.elapsed() >= IDLE) {
            *client = None;
        }
    }
}

/// Закрыть nemora-mcp при выходе из Reever (если не занят командой — тогда он закроется
/// сам вместе с процессом Reever: закрытый stdin — и nemora-mcp выходит).
pub fn shutdown() {
    if let Ok(mut client) = CLIENT.try_lock() {
        *client = None;
    }
}

// ─── MCP-клиент ──────────────────────────────────────────────────────────────

/// Ошибка вызова: связь (процесс умер, молчит) или ответ самой Неморы (`isError`).
#[derive(Debug, Clone, PartialEq)]
enum CallError {
    Transport(String),
    Tool(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Transport(text) | CallError::Tool(text) => f.write_str(text),
        }
    }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    used: Instant,
    path: PathBuf,
}

impl Client {
    fn start(path: &Path) -> Result<Self, CallError> {
        let mut command = Command::new(path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // консольная программа: без флага над экраном мелькнуло бы чёрное окно
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|e| CallError::Transport(format!("{}: {e}", path.display())))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(CallError::Transport("nemora-mcp: no stdio".into()));
        };
        let (sender, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name("reever-nemora-mcp".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| CallError::Transport(e.to_string()))?;
        let mut client = Self {
            child,
            stdin,
            lines,
            next_id: 1,
            used: Instant::now(),
            path: path.to_path_buf(),
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                // Немора подписывает этим именем свои уведомления и журнал агентов
                "clientInfo": { "name": "reever", "title": "Reever", "version": env!("CARGO_PKG_VERSION") },
            }),
            HANDSHAKE_TIMEOUT,
        )?;
        client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(client)
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn send(&mut self, message: &Value) -> Result<(), CallError> {
        writeln!(self.stdin, "{message}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| CallError::Transport(format!("nemora-mcp: {e}")))
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let id = self.next_id;
        self.next_id += 1;
        self.used = Instant::now();
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(CallError::Transport(format!(
                        "nemora-mcp: no answer to {method}"
                    )))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(CallError::Transport("nemora-mcp exited".into()))
                }
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            // сервер может сам спросить (ping) — ответить, чтобы он не ждал
            if let (Some(server_id), Some(server_method)) =
                (message.get("id"), message.get("method"))
            {
                let reply = if server_method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": server_id, "result": {} })
                } else {
                    json!({ "jsonrpc": "2.0", "id": server_id, "error": { "code": -32601, "message": "not supported" } })
                };
                self.send(&reply)?;
                continue;
            }
            if let Some(answer) = response_for(&message, id) {
                self.used = Instant::now();
                return answer.map_err(CallError::Tool);
            }
        }
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            timeout,
        )?;
        tool_payload(&result).map_err(CallError::Tool)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // закрытый stdin — nemora-mcp выходит сам; kill — если завис
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ответ на запрос `id`: `result` или текст `error`. `None` — сообщение не про него.
fn response_for(message: &Value, id: u64) -> Option<Result<Value, String>> {
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error");
        return Some(Err(text.to_string()));
    }
    Some(Ok(message.get("result").cloned().unwrap_or(Value::Null)))
}

/// Что вернул инструмент: `structuredContent`, иначе текст (JSON, если он JSON). `isError` —
/// человеческий текст Неморы («Nemora is not running…»).
fn tool_payload(result: &Value) -> Result<Value, String> {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(if text.trim().is_empty() {
            "Nemora: error".into()
        } else {
            text
        });
    }
    if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(structured.clone());
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

static CLIENT: Mutex<Option<Client>> = Mutex::new(None);

/// Вызвать инструмент Неморы. Процесс nemora-mcp запускается при первом вызове и живёт
/// `IDLE`; умер или не отвечает — один перезапуск.
fn call(tool: &str, arguments: Value, timeout: Duration) -> Result<Value, CallError> {
    let path = mcp_path().ok_or_else(|| CallError::Transport("Nemora is not installed".into()))?;
    let mut slot = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    for attempt in 0..2 {
        let fresh = !slot
            .as_mut()
            .is_some_and(|client| client.path == path && client.alive());
        if fresh {
            *slot = None;
            *slot = Some(Client::start(&path)?);
        }
        let client = slot.as_mut().expect("client started above");
        match client.call_tool(tool, arguments.clone(), timeout) {
            Err(CallError::Transport(error)) => {
                *slot = None;
                // запрос мог дойти (play запускает Немору) — повторяем, только если процесс был старый
                if fresh || attempt == 1 {
                    return Err(CallError::Transport(error));
                }
            }
            other => return other,
        }
    }
    Err(CallError::Transport("nemora-mcp".into()))
}

// ─── Команды ─────────────────────────────────────────────────────────────────

/// Итоги команд, которые закончились в фоне: воркер играет по ним звук.
static OUTCOMES: Mutex<Vec<bool>> = Mutex::new(Vec::new());

/// Забрать итоги закончившихся команд (успех / неудача) — для звуков.
pub fn take_outcomes() -> Vec<bool> {
    std::mem::take(&mut *OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Выполнить команду Неморы в своём потоке. `queries` — названное после ключевой фразы
/// (варианты по порядку предпочтения); `private` — не писать названия в журнал.
pub fn start(app: &AppHandle, op: MusicOp, queries: Vec<String>, private: bool) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("reever-nemora".into())
        .spawn(move || {
            let ok = run(&app, op, &queries, private);
            OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()).push(ok);
        });
    if let Err(error) = spawned {
        OUTCOMES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(false);
        eprintln!("[reever] nemora: thread: {error}");
    }
}

struct Run<'a> {
    app: &'a AppHandle,
    private: bool,
}

impl Run<'_> {
    /// Название для журнала: в приватном журнале — «…».
    fn name(&self, text: &str) -> String {
        if self.private {
            "…".into()
        } else {
            text.to_string()
        }
    }

    fn failed(&self, error: &CallError) -> bool {
        ui_log(self.app, crate::msg!("log.music.failed", error = error));
        false
    }

    fn not_found(&self, query: &str) -> bool {
        ui_log(
            self.app,
            crate::msg!("log.music.notFound", query = self.name(query)),
        );
        false
    }

    fn playing(&self, what: &str) {
        ui_log(
            self.app,
            crate::msg!("log.music.play", what = self.name(what)),
        );
    }
}

fn run(app: &AppHandle, op: MusicOp, queries: &[String], private: bool) -> bool {
    let run = Run { app, private };
    // app_status не запускает Немору: узнать, открыта ли она и что разрешено агентам
    let status = match call("app_status", json!({}), CALL_TIMEOUT) {
        Ok(status) => status,
        Err(error) => return run.failed(&error),
    };
    if matches!(
        status.get("access").and_then(Value::as_str),
        Some("off" | "read")
    ) {
        ui_log(app, crate::msg!("log.music.noAccess"));
        return false;
    }
    let running = status
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let result = match op {
        MusicOp::Song => play_song(&run, queries),
        MusicOp::Artist => play_artist(&run, queries),
        MusicOp::Album => play_album(&run, queries),
        MusicOp::Playlist => play_playlist(&run, queries),
        MusicOp::Play => play_music(&run, running),
        MusicOp::Pause => pause(&run, running),
        MusicOp::SmartShuffle | MusicOp::Shuffle | MusicOp::ShuffleOff if !running => {
            not_running(&run)
        }
        MusicOp::SmartShuffle => smart_shuffle(&run),
        MusicOp::Shuffle => set_shuffle(&run, "shuffle", "log.music.shuffleOn"),
        MusicOp::ShuffleOff => set_shuffle(&run, "off", "log.music.shuffleOff"),
        // set_favorite запустил бы закрытую Немору — лайкать нечего, если она не играет
        MusicOp::Like if !running => not_running(&run),
        MusicOp::Like => like(&run),
        MusicOp::NowPlaying if !running => not_running(&run),
        MusicOp::NowPlaying => now_playing(&run),
        MusicOp::None => Ok(false),
    };
    result.unwrap_or_else(|error| run.failed(&error))
}

fn not_running(run: &Run) -> Result<bool, CallError> {
    ui_log(run.app, crate::msg!("log.music.notRunning"));
    Ok(false)
}

/// Запрос для поиска Неморы: «;» она делит на несколько запросов.
fn search_query(query: &str) -> String {
    query
        .replace(';', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn search(query: &str, filter: &str, limit: u32) -> Result<Value, CallError> {
    call(
        "search",
        json!({ "query": search_query(query), "filter": filter, "limit": limit }),
        CALL_TIMEOUT,
    )
}

/// Первый вариант запроса, по которому что-то нашлось (`pick` вернул `Some`).
fn first_found<T>(
    queries: &[String],
    mut find: impl FnMut(&str) -> Result<Option<T>, CallError>,
) -> Result<Option<T>, CallError> {
    for query in queries.iter().map(|q| q.trim()).filter(|q| !q.is_empty()) {
        if let Some(found) = find(query)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Варианты названия для сравнения: как есть и транслитерацией латиницы в кириллицу
/// («goat» = «гоат») — русская модель пишет английские названия по-русски.
fn forms(text: &str) -> Vec<String> {
    crate::textnorm::comparison_forms(text)
}

fn same(a: &str, b: &str) -> bool {
    let (a, b) = (forms(a), forms(b));
    a.iter().any(|form| !form.is_empty() && b.contains(form))
}

/// Трек из выдачи поиска: точное совпадение названия, иначе первый (выдача ранжирована).
/// Версии (slowed, reverb — строки с `version` или своим `viewId`) — только если их назвали.
fn pick_song(found: &Value, query: &str) -> Option<Value> {
    let songs: Vec<&Value> = found
        .get("songs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let plain = |song: &&Value| {
        let version = song.get("version").is_some_and(|v| !v.is_null());
        let view = song.get("viewId").and_then(Value::as_str);
        !version && view.is_none_or(|view| Some(view) == song.get("songId").and_then(Value::as_str))
    };
    let candidates: Vec<&Value> = {
        let plain: Vec<&Value> = songs.iter().copied().filter(plain).collect();
        if plain.is_empty() {
            songs
        } else {
            plain
        }
    };
    candidates
        .iter()
        .find(|song| {
            song.get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| same(title, query))
        })
        .or(candidates.first())
        .map(|song| (*song).clone())
}

/// Имя из списка (исполнители, альбомы, плейлисты): точное, иначе первое — или `None`.
fn pick_name<'a>(names: &[&'a str], query: &str) -> Option<&'a str> {
    names
        .iter()
        .copied()
        .find(|name| same(name, query))
        .or(names.first().copied())
}

/// «Исполнитель — название» для журнала.
fn song_line(song: &Value) -> String {
    let title = song
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let artists: Vec<&str> = song
        .get("artists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    match (artists.is_empty(), title.is_empty()) {
        (true, _) => title.to_string(),
        (false, true) => artists.join(", "),
        (false, false) => format!("{} — {title}", artists.join(", ")),
    }
}

fn song_ids(list: &Value) -> Vec<String> {
    list.get("songs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|song| song.get("songId").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

/// Все песни области (`None` — вся библиотека) по страницам list_songs.
fn list_all(scope: Option<&str>) -> Result<Vec<String>, CallError> {
    let mut ids = Vec::new();
    loop {
        let mut arguments = json!({ "limit": PAGE, "offset": ids.len() });
        if let Some(scope) = scope {
            arguments["scope"] = json!(scope);
        }
        let page = call("list_songs", arguments, CALL_TIMEOUT)?;
        let rows = song_ids(&page);
        let total = page.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        let got = rows.len();
        ids.extend(rows);
        if got == 0 || ids.len() >= total.min(LIBRARY_LIMIT) {
            return Ok(ids);
        }
    }
}

fn shuffle_mode(state: &Value) -> Option<&str> {
    state.get("shuffle").and_then(Value::as_str)
}

/// Включить Smart Shuffle. Немора не включает его без тир-листа, влияющего на шаффл, а веса
/// грузит не сразу — поэтому проверить, и если не вышло, обычное перемешивание.
/// `true` — включился Smart.
fn enable_smart() -> Result<bool, CallError> {
    let state = call("set_shuffle", json!({ "mode": "smart" }), CALL_TIMEOUT)?;
    if shuffle_mode(&state) == Some("smart") {
        return Ok(true);
    }
    std::thread::sleep(Duration::from_millis(1500));
    let now = call("now_playing", json!({}), CALL_TIMEOUT)?;
    if shuffle_mode(&now) == Some("smart") {
        return Ok(true);
    }
    if shuffle_mode(&now) != Some("shuffle") {
        call("set_shuffle", json!({ "mode": "shuffle" }), CALL_TIMEOUT)?;
    }
    Ok(false)
}

/// «Включи трек X»: найденный трек, за ним вся библиотека в Smart Shuffle — музыка не
/// кончается на одном треке (play без shuffle делает очередь ровно из переданного).
fn find_song(queries: &[String]) -> Result<Option<Value>, CallError> {
    first_found(queries, |query| {
        Ok(pick_song(&search(query, "songs", 10)?, query))
    })
}

fn play_song(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some(song) = find_song(queries)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let Some(id) = song.get("songId").and_then(Value::as_str).map(String::from) else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let mut queue = list_all(None).unwrap_or_default();
    queue.retain(|other| *other != id);
    queue.insert(0, id);
    let whole = queue.len() > 1;
    call(
        "play",
        json!({ "song_ids": queue, "start_index": 0 }),
        START_TIMEOUT,
    )?;
    run.playing(&song_line(&song));
    if whole {
        // smart переставляет очередь, играющий трек остаётся первым
        enable_smart()?;
    }
    Ok(true)
}

/// «Включи исполнителя X»: все его песни вперемешку.
fn find_artist(queries: &[String]) -> Result<Option<String>, CallError> {
    first_found(queries, |query| {
        let found = search(query, "artists", 10)?;
        let names: Vec<&str> = found
            .get("artists")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        Ok(pick_name(&names, query).map(String::from))
    })
}

fn play_artist(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some(artist) = find_artist(queries)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let ids = list_all(Some(&format!("artist:{artist}")))?;
    if ids.is_empty() {
        return Ok(run.not_found(&artist));
    }
    call(
        "play",
        json!({ "song_ids": ids, "shuffle": true }),
        START_TIMEOUT,
    )?;
    run.playing(&artist);
    Ok(true)
}

/// «Включи альбом X»: альбом в порядке треков.
/// Альбом: (ключ для list_songs, название).
fn find_album(queries: &[String]) -> Result<Option<(String, String)>, CallError> {
    first_found(queries, |query| {
        let found = search(query, "albums", 10)?;
        let albums: Vec<(&str, &str)> = found
            .get("albums")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|album| {
                Some((
                    album.get("key")?.as_str()?,
                    album
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ))
            })
            .collect();
        let titles: Vec<&str> = albums.iter().map(|(_, title)| *title).collect();
        Ok(pick_name(&titles, query)
            .and_then(|title| albums.iter().find(|(_, t)| *t == title))
            .map(|(key, title)| (key.to_string(), title.to_string())))
    })
}

fn play_album(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some((key, title)) = find_album(queries)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let ids = list_all(Some(&format!("album:{key}")))?;
    if ids.is_empty() {
        return Ok(run.not_found(&title));
    }
    call("play", json!({ "song_ids": ids }), START_TIMEOUT)?;
    run.playing(&title);
    Ok(true)
}

/// Слова «избранное» — плейлист `Favorites`.
const FAVORITES_WORDS: &[&str] = &[
    "избранное",
    "избранные",
    "любимое",
    "любимые",
    "лайкнутые",
    "favorites",
    "favourites",
    "liked",
];

/// Плейлист по названию: id из list_playlists (поиск Неморы отдаёт только имена).
fn pick_playlist(list: &Value, query: &str) -> Option<(String, String)> {
    let playlists: Vec<(&str, &str)> = list
        .get("playlists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some((
                p.get("playlistId")?.as_str()?,
                p.get("name").and_then(Value::as_str).unwrap_or_default(),
            ))
        })
        .collect();
    let wanted = normalize(query);
    let wanted_forms = forms(query);
    if FAVORITES_WORDS.iter().any(|word| normalize(word) == wanted) {
        return playlists
            .iter()
            .find(|(id, _)| *id == "Favorites")
            .map(|(id, name)| (id.to_string(), name.to_string()));
    }
    let score = |name: &str| {
        let mut best = 0.0_f64;
        for name in forms(name) {
            for wanted in &wanted_forms {
                let score = if name == *wanted {
                    2.0
                } else if !name.is_empty()
                    && !wanted.is_empty()
                    && (name.contains(wanted.as_str()) || wanted.contains(&name))
                {
                    1.5
                } else {
                    strsim::jaro_winkler(&name, wanted)
                };
                best = best.max(score);
            }
        }
        best
    };
    playlists
        .iter()
        .map(|(id, name)| (score(name), id, name))
        .filter(|(score, _, _)| *score >= 0.85)
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, id, name)| (id.to_string(), name.to_string()))
}

/// «Включи плейлист X»: в его порядке.
/// Плейлист: (id, название).
fn find_playlist(queries: &[String]) -> Result<Option<(String, String)>, CallError> {
    let list = call("list_playlists", json!({}), CALL_TIMEOUT)?;
    first_found(queries, |query| Ok(pick_playlist(&list, query)))
}

fn play_playlist(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some((id, name)) = find_playlist(queries)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let ids = song_ids(&call(
        "get_playlist",
        json!({ "playlist_id": id }),
        CALL_TIMEOUT,
    )?);
    if ids.is_empty() {
        return Ok(run.not_found(&name));
    }
    call("play", json!({ "song_ids": ids }), START_TIMEOUT)?;
    run.playing(&name);
    Ok(true)
}

/// «Включи музыку»: остановленное — продолжить; играет — ничего не трогать; иначе вся
/// библиотека в Smart Shuffle.
fn play_music(run: &Run, running: bool) -> Result<bool, CallError> {
    let now = if running {
        Some(call("now_playing", json!({}), CALL_TIMEOUT)?)
    } else {
        None
    };
    if let Some(now) = now
        .as_ref()
        .filter(|now| now.get("songId").is_some_and(|id| !id.is_null()))
    {
        if now.get("playing").and_then(Value::as_bool) != Some(true) {
            let resumed = call("resume", json!({}), START_TIMEOUT)?;
            run.playing(&song_line(if resumed.get("title").is_some() {
                &resumed
            } else {
                now
            }));
        } else {
            run.playing(&song_line(now));
        }
        return Ok(true);
    }
    let ids = list_all(None)?;
    if ids.is_empty() {
        ui_log(run.app, crate::msg!("log.music.empty"));
        return Ok(false);
    }
    let smart_before = now.as_ref().and_then(shuffle_mode) == Some("smart");
    let played = call(
        "play",
        json!({ "song_ids": ids, "shuffle": true }),
        START_TIMEOUT,
    )?;
    if !smart_before {
        enable_smart()?;
    }
    let now = call("now_playing", json!({}), CALL_TIMEOUT).unwrap_or(played);
    run.playing(&song_line(&now));
    Ok(true)
}

/// «Выключи музыку»: играет Немора — её пауза (не переключатель: повтор не включит обратно);
/// не играет — медиа-клавиша, как «пауза» (может играть другой плеер).
fn pause(run: &Run, running: bool) -> Result<bool, CallError> {
    if running {
        let now = call("now_playing", json!({}), CALL_TIMEOUT)?;
        if now.get("playing").and_then(Value::as_bool) == Some(true) {
            call("pause", json!({}), CALL_TIMEOUT)?;
            ui_log(run.app, crate::msg!("log.music.paused"));
            return Ok(true);
        }
    }
    ui_log(run.app, crate::msg!("log.music.mediaKey"));
    Ok(crate::system::run_system("media_play_pause"))
}

fn smart_shuffle(run: &Run) -> Result<bool, CallError> {
    if enable_smart()? {
        ui_log(run.app, crate::msg!("log.music.smart"));
    } else {
        ui_log(run.app, crate::msg!("log.music.smartFallback"));
    }
    Ok(true)
}

fn set_shuffle(run: &Run, mode: &str, done: &'static str) -> Result<bool, CallError> {
    call("set_shuffle", json!({ "mode": mode }), CALL_TIMEOUT)?;
    ui_log(
        run.app,
        crate::uilang::Msg {
            key: done,
            args: Vec::new(),
        },
    );
    Ok(true)
}

fn like(run: &Run) -> Result<bool, CallError> {
    let now = call("now_playing", json!({}), CALL_TIMEOUT)?;
    let Some(id) = now.get("songId").and_then(Value::as_str) else {
        ui_log(run.app, crate::msg!("log.music.nothing"));
        return Ok(false);
    };
    call(
        "set_favorite",
        json!({ "song_ids": [id], "favorite": true }),
        CALL_TIMEOUT,
    )?;
    ui_log(
        run.app,
        crate::msg!("log.music.liked", what = run.name(&song_line(&now))),
    );
    Ok(true)
}

fn now_playing(run: &Run) -> Result<bool, CallError> {
    let now = call("now_playing", json!({}), CALL_TIMEOUT)?;
    if now.get("songId").is_none_or(Value::is_null) {
        ui_log(run.app, crate::msg!("log.music.nothing"));
        return Ok(false);
    }
    // название — ответ на вопрос, его показываем и в приватном журнале
    ui_log(
        run.app,
        crate::msg!("log.music.now", what = song_line(&now)),
    );
    Ok(true)
}

/// Что нашлось бы по команде с целью — только чтение, воспроизведение не трогает
/// (examples/nemora_probe.rs: проверить разбор на своей библиотеке).
pub fn preview(op: MusicOp, queries: &[String]) -> Result<Option<String>, String> {
    let found = match op {
        MusicOp::Song => find_song(queries).map(|song| song.map(|song| song_line(&song))),
        MusicOp::Artist => find_artist(queries),
        MusicOp::Album => find_album(queries).map(|album| album.map(|(_, title)| title)),
        MusicOp::Playlist => find_playlist(queries).map(|playlist| playlist.map(|(_, name)| name)),
        _ => return Ok(None),
    };
    found.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_location_loses_quotes_and_padding() {
        assert_eq!(
            clean_install_location("\"C:\\Users\\me\\AppData\\Local\\Nemora\"\0"),
            Some(PathBuf::from("C:\\Users\\me\\AppData\\Local\\Nemora"))
        );
        assert_eq!(clean_install_location("  \"\"  "), None);
    }

    #[test]
    fn response_is_matched_by_id() {
        let ok = json!({ "jsonrpc": "2.0", "id": 3, "result": { "x": 1 } });
        assert_eq!(response_for(&ok, 3), Some(Ok(json!({ "x": 1 }))));
        assert_eq!(response_for(&ok, 4), None);
        let failed =
            json!({ "jsonrpc": "2.0", "id": 4, "error": { "code": -1, "message": "boom" } });
        assert_eq!(response_for(&failed, 4), Some(Err("boom".into())));
        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/progress" });
        assert_eq!(response_for(&notification, 4), None);
    }

    #[test]
    fn tool_payload_prefers_structured_then_json_text_and_reports_errors() {
        let structured = json!({ "content": [{ "type": "text", "text": "{}" }], "structuredContent": { "a": 1 } });
        assert_eq!(tool_payload(&structured), Ok(json!({ "a": 1 })));
        let text = json!({ "content": [{ "type": "text", "text": "{\"running\":true}" }] });
        assert_eq!(tool_payload(&text), Ok(json!({ "running": true })));
        let plain = json!({ "content": [{ "type": "text", "text": "done" }] });
        assert_eq!(tool_payload(&plain), Ok(json!("done")));
        let error = json!({ "content": [{ "type": "text", "text": "Nemora is not running." }], "isError": true });
        assert_eq!(tool_payload(&error), Err("Nemora is not running.".into()));
    }

    #[test]
    fn exact_title_wins_and_versions_are_skipped() {
        let found = json!({ "songs": [
            { "songId": "a", "title": "Teen Spirit Cover", "artists": ["X"] },
            { "songId": "b", "viewId": "b:v1", "version": "Slowed", "title": "Smells Like Teen Spirit" },
            { "songId": "c", "title": "Smells Like Teen Spirit", "artists": ["Nirvana"] },
        ]});
        assert_eq!(
            pick_song(&found, "smells like teen spirit").unwrap()["songId"],
            "c"
        );
        assert_eq!(pick_song(&found, "something else").unwrap()["songId"], "a");
        let only_versions = json!({ "songs": [{ "songId": "b", "viewId": "b:v1", "version": "Slowed", "title": "T" }] });
        assert_eq!(pick_song(&only_versions, "t").unwrap()["songId"], "b");
        assert!(pick_song(&json!({ "songs": [] }), "t").is_none());
    }

    #[test]
    fn names_match_exactly_or_fall_back_to_the_first() {
        assert_eq!(
            pick_name(&["Nirvanna Tribute", "Nirvana"], "нирвана"),
            Some("Nirvana")
        );
        assert_eq!(
            pick_name(&["Nirvanna Tribute", "Nirvana"], "Nirvana"),
            Some("Nirvana")
        );
        assert_eq!(pick_name(&[], "x"), None);
    }

    #[test]
    fn playlist_by_name_and_favorites_by_word() {
        let list = json!({ "playlists": [
            { "name": "Favorites", "playlistId": "Favorites", "songCount": 0 },
            { "name": "GOAT", "playlistId": "p1", "songCount": 278 },
            { "name": "straftat vibes jungle / breakcore", "playlistId": "p2", "songCount": 17 },
        ]});
        assert_eq!(
            pick_playlist(&list, "goat"),
            Some(("p1".into(), "GOAT".into()))
        );
        assert_eq!(pick_playlist(&list, "гоат").map(|p| p.0), Some("p1".into()));
        assert_eq!(
            pick_playlist(&list, "избранное").map(|p| p.0),
            Some("Favorites".into())
        );
        assert_eq!(
            pick_playlist(&list, "straftat vibes").map(|p| p.0),
            Some("p2".into())
        );
        assert_eq!(pick_playlist(&list, "совсем другое"), None);
    }

    #[test]
    fn song_line_and_query_cleanup() {
        assert_eq!(
            song_line(&json!({ "title": "Song", "artists": ["A", "B"] })),
            "A, B — Song"
        );
        assert_eq!(
            song_line(&json!({ "title": "Song", "artists": [] })),
            "Song"
        );
        assert_eq!(search_query("ac;dc  back in black"), "ac dc back in black");
    }

    /// Живая проверка на установленной Неморе: только чтение (статус, поиск, список песен).
    #[test]
    #[ignore]
    fn live_read_only_calls() {
        let path = find_mcp().expect("Nemora is not installed");
        println!("nemora-mcp: {}", path.display());
        let status = call("app_status", json!({}), CALL_TIMEOUT).expect("app_status");
        println!("status: {status}");
        assert!(status.get("access").is_some());
        let found = search("a", "songs", 3).expect("search");
        println!("search: {found}");
        let first = list_all(None).expect("list_songs");
        println!("library: {} songs", first.len());
        assert!(!first.is_empty());
        let playlists = call("list_playlists", json!({}), CALL_TIMEOUT).expect("list_playlists");
        println!("favorites: {:?}", pick_playlist(&playlists, "избранное"));
        shutdown();
    }
}
