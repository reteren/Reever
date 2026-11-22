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
use std::sync::atomic::{AtomicBool, Ordering};
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
/// Настройки → Немора → «Музыкальные команды Неморы». Выключено — команды скрыты, как будто
/// Неморы нет.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Включить или выключить команды Неморы (при запуске и после сохранения настроек).
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
    refresh();
}

/// Где найден nemora-mcp (для настроек; `None` — Немора не установлена).
pub fn installed_path() -> Option<PathBuf> {
    mcp_path()
}

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
    crate::grammar::set_nemora_installed(installed && ENABLED.load(Ordering::Relaxed));
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

/// Как ведут себя команды Неморы — Настройки → Немора (`Settings::nemora_*`).
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// После «включи трек X»: `smart`, `keep`, `artist`, `repeat`, `single`.
    pub after_song: String,
    /// «Включи исполнителя X»: `shuffle`, `plays`, `album`.
    pub artist_order: String,
    /// Альбомы и плейлисты вперемешку (иначе по порядку).
    pub collection_shuffle: bool,
    /// «Включи музыку», когда ничего не на паузе: `smart`, `favorites`, `plays`, `added`.
    pub play_music: String,
    /// Шаг «громче» / «тише», проценты.
    pub volume_step: u32,
    /// Запускать закрытую Немору для команд, которые включают музыку.
    pub start_app: bool,
    /// Только уверенные совпадения названия.
    pub strict: bool,
    /// «Выключи музыку» без играющей Неморы — медиа-клавиша для другого плеера.
    pub pause_fallback: bool,
    /// Не писать названия в журнал.
    pub private: bool,
}

impl Options {
    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        Self {
            after_song: settings.nemora_after_song.clone(),
            artist_order: settings.nemora_artist_order.clone(),
            collection_shuffle: settings.nemora_collection_order == "shuffle",
            play_music: settings.nemora_play_music.clone(),
            volume_step: settings.nemora_volume_step,
            start_app: settings.nemora_start_app,
            strict: settings.nemora_strict,
            pause_fallback: settings.nemora_pause_fallback,
            private: settings.private_log,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::from_settings(&crate::config::Settings::default())
    }
}

/// Выполнить команду Неморы в своём потоке. `queries` — названное после ключевой фразы
/// (варианты по порядку предпочтения).
pub fn start(app: &AppHandle, op: MusicOp, queries: Vec<String>, options: Options) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("reever-nemora".into())
        .spawn(move || {
            let ok = run(&app, op, &queries, &options);
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

/// Системные «предыдущий трек» (`media_prev`) и «заново» (`media_restart`), когда Немора
/// установлена: играет она — точно через неё, иначе медиа-клавиша (другой плеер).
pub fn start_media(app: &AppHandle, op: &str) {
    let (app, owned) = (app.clone(), op.to_string());
    let spawned = std::thread::Builder::new()
        .name("reever-nemora".into())
        .spawn(move || {
            let ok = match media_in_nemora(&owned) {
                Ok(true) => true,
                Ok(false) => crate::system::run_system(&owned),
                Err(error) => {
                    ui_log(&app, crate::msg!("log.music.failed", error = error));
                    crate::system::run_system(&owned)
                }
            };
            OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()).push(ok);
        });
    if spawned.is_err() {
        let ok = crate::system::run_system(op);
        OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()).push(ok);
    }
}

/// `true` — сделано в Неморе; `false` — она не играет, нужна медиа-клавиша. app_status не
/// запускает закрытую Немору.
fn media_in_nemora(op: &str) -> Result<bool, CallError> {
    let status = call("app_status", json!({}), CALL_TIMEOUT)?;
    let running = status
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let allowed = !matches!(
        status.get("access").and_then(Value::as_str),
        Some("off" | "read")
    );
    if !running || !allowed {
        return Ok(false);
    }
    let now = call("now_playing", json!({}), CALL_TIMEOUT)?;
    if now.get("playing").and_then(Value::as_bool) != Some(true) {
        return Ok(false);
    }
    if op == "media_restart" {
        call("seek", json!({ "seconds": 0 }), CALL_TIMEOUT)?;
        return Ok(true);
    }
    // previous_song после первых 5 с только перематывает в начало — тогда ещё раз
    let before = now.get("songId").cloned();
    call("previous_song", json!({}), CALL_TIMEOUT)?;
    let after = call("now_playing", json!({}), CALL_TIMEOUT)?;
    if before.is_some() && after.get("songId").cloned() == before {
        call("previous_song", json!({}), CALL_TIMEOUT)?;
    }
    Ok(true)
}

struct Run<'a> {
    app: &'a AppHandle,
    options: &'a Options,
    /// Немора была открыта, когда пришла команда.
    running: bool,
}

impl Run<'_> {
    /// Название для журнала: в приватном журнале — «…».
    fn name(&self, text: &str) -> String {
        if self.options.private {
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

fn run(app: &AppHandle, op: MusicOp, queries: &[String], options: &Options) -> bool {
    let failed = Run {
        app,
        options,
        running: false,
    };
    // app_status не запускает Немору: узнать, открыта ли она и что разрешено агентам
    let status = match call("app_status", json!({}), CALL_TIMEOUT) {
        Ok(status) => status,
        Err(error) => return failed.failed(&error),
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
    let run = Run {
        app,
        options,
        running,
    };
    let starts = matches!(
        op,
        MusicOp::Song | MusicOp::Artist | MusicOp::Album | MusicOp::Playlist | MusicOp::Play
    );
    let result = match op {
        // запускать закрытую Немору выключено в настройках
        _ if starts && !running && !options.start_app => not_running(&run),
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
        MusicOp::Volume => volume(&run, queries, running),
        MusicOp::Louder => volume_step(&run, true),
        MusicOp::Quieter => volume_step(&run, false),
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

/// Номер части словами — цифрами: «part two», «парт ту» и «Pt. 2» должны совпасть, иначе
/// первая и вторая части для сравнения неразличимы. Только после `NUMBERED`: в остальных
/// местах «ту» — это «to» («бэк ту блэк»), а «Two Weeks» говорят «ту уикс».
const NUMBER_WORDS: &[(&str, &str)] = &[
    ("one", "1"),
    ("two", "2"),
    ("too", "2"),
    ("to", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("ван", "1"),
    ("ту", "2"),
    ("фри", "3"),
    ("сри", "3"),
    ("фор", "4"),
    ("файв", "5"),
    ("один", "1"),
    ("два", "2"),
    ("три", "3"),
    ("четыре", "4"),
    ("пять", "5"),
];
/// После них — номер части (ASR пишет и «part too»).
const NUMBERED: &[&str] = &[
    "part",
    "pt",
    "парт",
    "часть",
    "vol",
    "volume",
    "chapter",
    "глава",
];

/// Название к общему виду: нормализация, «pt» → «part», «vol» → «volume», номер части цифрой.
fn canonical(text: &str) -> String {
    let words: Vec<String> = normalize(text)
        .split_whitespace()
        .map(String::from)
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for word in words {
        // «pt2», «vol3» слитно — как «pt 2»
        if let Some((prefix, digits)) = glued_number(&word) {
            out.push(prefix.to_string());
            out.push(digits.to_string());
            continue;
        }
        let after_numbered = out
            .last()
            .is_some_and(|last| NUMBERED.contains(&last.as_str()));
        let word = match word.as_str() {
            "pt" => "part".to_string(),
            "vol" => "volume".to_string(),
            other if after_numbered => NUMBER_WORDS
                .iter()
                .find(|(spoken, _)| *spoken == other)
                .map_or_else(|| other.to_string(), |(_, digit)| digit.to_string()),
            other => other.to_string(),
        };
        out.push(word);
    }
    out.join(" ")
}

/// «pt2» → («part», «2»), «vol3» → («volume», «3»).
fn glued_number(word: &str) -> Option<(&'static str, &str)> {
    [
        ("part", "part"),
        ("pt", "part"),
        ("volume", "volume"),
        ("vol", "volume"),
    ]
    .iter()
    .find_map(|(prefix, full)| {
        let digits = word.strip_prefix(prefix)?;
        (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            .then_some((*full, digits))
    })
}

/// Варианты названия: целиком и без пометок в скобках («Nevermind (Deluxe Edition)» —
/// говорят «невермайнд»).
fn name_variants(name: &str) -> Vec<String> {
    let mut plain = String::with_capacity(name.len());
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let mut variants = vec![name.to_string()];
    if !plain.trim().is_empty() && normalize(&plain) != normalize(name) {
        variants.push(plain);
    }
    variants
}

/// Варианты для сравнения: общий вид названия и его транслитерация латиницы в кириллицу
/// («goat» = «гоат», «Hate Music» = «хат мьюзик») — русская модель пишет английские
/// названия по-русски.
fn forms(text: &str) -> Vec<String> {
    crate::textnorm::comparison_forms(&canonical(text))
}

/// То же название (любой вариант без скобок, с транслитерацией).
fn same(name: &str, query: &str) -> bool {
    let query = forms(query);
    name_variants(name)
        .iter()
        .flat_map(|variant| forms(variant))
        .any(|form| !form.is_empty() && query.contains(&form))
}

/// Кандидаты по каждому варианту запроса: (запрос, [(название, что вернуть)]) — выдача
/// Неморы по порядку её ранжирования.
type Found<T> = Vec<(String, Vec<(String, T)>)>;

/// Похожесть ниже этой — уже не «то самое»: тогда решает ранжирование Неморы.
const MIN_SIMILAR: f64 = 0.6;
/// Латинский запрос и такой похожий результат поиска («hate musik» → «Hate Music») —
/// библиотеку не листаем: опечатки поиск Неморы понимает сам.
const SEARCH_CONFIDENT: f64 = 0.8;

/// Варианты запроса (русский, английский) без пустых и повторов.
fn distinct(queries: &[String]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for query in queries.iter().map(|q| q.trim()).filter(|q| !q.is_empty()) {
        if !out.iter().any(|seen| normalize(seen) == normalize(query)) {
            out.push(query);
        }
    }
    out
}

/// Фонетический ключ варианта: без пробелов (слитное «DEADMATRIX» = «дэд матрикс»), гласные,
/// которые транслит и модель путают, сведены (е э и ы й → и, ю → у, я → а, о → а), повторы
/// букв схлопнуты. Сравнивает звучание, а не написание.
fn skeleton(form: &str) -> String {
    let mut out = String::with_capacity(form.len());
    for c in form.chars().filter(|c| !c.is_whitespace()) {
        let c = match c {
            'е' | 'э' | 'ы' | 'й' | 'ё' => 'и',
            'ю' => 'у',
            'я' | 'о' => 'а',
            other => other,
        };
        if !out.ends_with(c) {
            out.push(c);
        }
    }
    out
}

/// Ключи текста: двойные латинские буквы схлопнуты до транслитерации («Fluxxwave» —
/// «флуксвав», а не «флуксксвав»), потом `skeleton` каждого варианта.
fn phonetic_keys(text: &str) -> Vec<String> {
    let mut single = String::with_capacity(text.len());
    for c in canonical(text).chars() {
        if !(c.is_ascii_alphabetic() && single.ends_with(c)) {
            single.push(c);
        }
    }
    crate::textnorm::comparison_forms(&single)
        .iter()
        .map(|form| skeleton(form))
        .filter(|key| !key.is_empty())
        .collect()
}

/// Запрос внутри названия: 0,7 и больше — чем больше названия он покрывает, тем ближе к 1.
/// Название внутри запроса — без поблажки, иначе «I Really Like You» обходил бы
/// «I Really Like You Part 2».
fn contained(name: &str, query: &str, min_chars: usize) -> Option<f64> {
    let (name_len, query_len) = (name.chars().count(), query.chars().count());
    (query_len >= min_chars && name.contains(query))
        .then(|| 0.7 + 0.3 * query_len as f64 / name_len.max(1) as f64)
}

/// Насколько название похоже на запрос, 0…1: лучшее из сравнения вариантов по Левенштейну
/// («goat» и «гоат» в одной азбуке) и сравнения фонетических ключей («флюкс вэйв спидап» и
/// «Fluxxwave (Sped Up)»). Запрос, целиком входящий в название, — не меньше 0,7.
fn similarity(name: &str, query: &str) -> f64 {
    let variants = name_variants(name);
    let names: Vec<String> = variants.iter().flat_map(|variant| forms(variant)).collect();
    let queries = forms(query);
    let mut best = 0.0_f64;
    for name in &names {
        for query in &queries {
            let score = strsim::normalized_levenshtein(name, query);
            best = best.max(contained(name, query, 3).map_or(score, |c| c.max(score)));
        }
    }
    let name_keys: Vec<String> = variants
        .iter()
        .flat_map(|variant| phonetic_keys(variant))
        .collect();
    let query_keys = phonetic_keys(query);
    for name in &name_keys {
        for query in &query_keys {
            let score = strsim::normalized_levenshtein(name, query);
            best = best.max(contained(name, query, 5).map_or(score, |c| c.max(score)));
        }
    }
    best
}

/// Похожесть первого результата поиска, ниже которой он — не то: на транслите поиск Неморы
/// отдаёт случайное («дэд матрикс» → «i fvcked up my happiness…», 0,12).
const MIN_FALLBACK: f64 = 0.3;

/// Настройка «только уверенные совпадения»: похожесть не ниже этой, без запасного первого
/// результата поиска.
const STRICT_SIMILAR: f64 = 0.8;

/// Лучшее совпадение по всем вариантам запроса и его оценка: точное название — 1, иначе
/// самое похожее, если не меньше `min`. При равенстве — раньше в выдаче и раньше вариант.
fn best_similar<T: Clone>(found: &Found<T>, min: f64) -> Option<(f64, T)> {
    for (query, items) in found {
        if let Some((_, item)) = items.iter().find(|(name, _)| same(name, query)) {
            return Some((1.0, item.clone()));
        }
    }
    let mut best: Option<(f64, &T)> = None;
    for (query, items) in found {
        for (name, item) in items {
            let score = similarity(name, query);
            if best.is_none_or(|(top, _)| score > top) {
                best = Some((score, item));
            }
        }
    }
    best.filter(|(score, _)| *score >= min)
        .map(|(score, item)| (score, item.clone()))
}

/// Первый результат первого варианта, по которому что-то нашлось (выдача Неморы ранжирована),
/// — если он хоть немного похож (`MIN_FALLBACK`) или его не сравнить по буквам (японское,
/// китайское название: кириллические чтения японского Немора понимает сама).
fn first_result<T: Clone>(found: &Found<T>) -> Option<T> {
    found.iter().find_map(|(query, items)| {
        let (name, item) = items.first()?;
        let comparable =
            name.chars().any(|c| c.is_alphabetic()) && !name.chars().any(crate::textnorm::is_cjk);
        (!comparable || similarity(name, query) >= MIN_FALLBACK).then(|| item.clone())
    })
}

/// Лучший кандидат: `best_similar`, иначе `first_result` (`with_library` без библиотеки).
#[cfg(test)]
fn choose<T: Clone>(found: &Found<T>) -> Option<T> {
    best_similar(found, MIN_SIMILAR)
        .map(|(_, item)| item)
        .or_else(|| first_result(found))
}

fn cyrillic(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё' | 'і' | 'ї' | 'є' | 'ґ'))
}

/// Выдача поиска не дала точного совпадения — сравнить со всей библиотекой, если название
/// сказано кириллицей («хейт мьюзик»: транслит английского поиск Неморы не понимает, а
/// транслитерация Reever фонетическая — «Hate Music» = «хат мьюзик») или выдача ничего
/// похожего не дала («i really like you part too» при «… pt2»). Берётся то, что похожее:
/// лучшее из выдачи или из библиотеки; ничего похожего — первый результат поиска.
fn with_library<T: Clone>(
    found: &Found<T>,
    queries: &[&str],
    strict: bool,
    library: impl FnOnce() -> Result<Vec<(String, T)>, CallError>,
) -> Result<Option<T>, CallError> {
    let min = if strict { STRICT_SIMILAR } else { MIN_SIMILAR };
    // запасной первый результат — только без строгого выбора
    let fallback = |found: &Found<T>| if strict { None } else { first_result(found) };
    let best = best_similar(found, min);
    let confident = best.as_ref().map_or(0.0, |(score, _)| *score) >= SEARCH_CONFIDENT;
    let spoken: Vec<&str> = queries
        .iter()
        .copied()
        .filter(|q| cyrillic(q) || !confident)
        .collect();
    if best.as_ref().is_some_and(|(score, _)| *score >= 1.0) || spoken.is_empty() {
        return Ok(best.map(|(_, item)| item).or_else(|| fallback(found)));
    }
    let library = library()?;
    let everything: Found<T> = spoken
        .iter()
        .map(|query| (query.to_string(), library.clone()))
        .collect();
    let best = match (best, best_similar(&everything, min)) {
        (Some(search), Some(whole)) => Some(if whole.0 > search.0 { whole } else { search }),
        (search, whole) => search.or(whole),
    };
    Ok(best.map(|(_, item)| item).or_else(|| fallback(found)))
}

/// Все строки списка Неморы (`items_key` — массив в ответе) по страницам.
fn list_pages<T>(
    tool: &str,
    items_key: &str,
    page: usize,
    mut row: impl FnMut(&Value) -> Option<T>,
) -> Result<Vec<T>, CallError> {
    let mut rows = Vec::new();
    let mut offset = 0;
    loop {
        let reply = call(
            tool,
            json!({ "limit": page, "offset": offset }),
            CALL_TIMEOUT,
        )?;
        let items: Vec<&Value> = reply
            .get(items_key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        let total = reply.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        let got = items.len();
        offset += got;
        rows.extend(items.into_iter().filter_map(&mut row));
        if got == 0 || offset >= total.min(LIBRARY_LIMIT) {
            return Ok(rows);
        }
    }
}

/// Все треки библиотеки для сравнения по названию — и «исполнитель название», если сказали
/// и исполнителя («включи трек нирвана смэлз лайк тин спирит»).
fn library_songs() -> Result<Vec<(String, Value)>, CallError> {
    let songs = list_pages("list_songs", "songs", PAGE, |song| Some(song.clone()))?;
    let mut rows = Vec::with_capacity(songs.len() * 2);
    for (title, song) in song_candidates(&json!({ "songs": songs })) {
        let artists: Vec<&str> = song
            .get("artists")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !artists.is_empty() {
            rows.push((format!("{} {title}", artists.join(" ")), song.clone()));
        }
        rows.push((title, song));
    }
    Ok(rows)
}

/// Треки из выдачи поиска. Версии (slowed, reverb — строки с `version` или своим `viewId`)
/// — только если ничего, кроме них, не нашлось.
fn song_candidates(found: &Value) -> Vec<(String, Value)> {
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
    let plain_songs: Vec<&Value> = songs.iter().copied().filter(plain).collect();
    let chosen = if plain_songs.is_empty() {
        songs
    } else {
        plain_songs
    };
    chosen
        .into_iter()
        .map(|song| {
            let title = song
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (title.to_string(), song.clone())
        })
        .collect()
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

/// Все песни области (`None` — вся библиотека) по страницам list_songs; `sort` — порядок
/// Неморы (`plays` — сначала популярные, `album`, `added`…), `None` — её порядок по умолчанию.
fn list_all(scope: Option<&str>, sort: Option<&str>) -> Result<Vec<String>, CallError> {
    let mut ids = Vec::new();
    loop {
        let mut arguments = json!({ "limit": PAGE, "offset": ids.len() });
        if let Some(scope) = scope {
            arguments["scope"] = json!(scope);
        }
        if let Some(sort) = sort {
            arguments["sort"] = json!(sort);
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
fn find_song(queries: &[String], strict: bool) -> Result<Option<Value>, CallError> {
    let queries = distinct(queries);
    let mut found = Vec::new();
    for query in &queries {
        found.push((
            query.to_string(),
            song_candidates(&search(query, "songs", 10)?),
        ));
    }
    with_library(&found, &queries, strict, library_songs)
}

/// Режим повтора, каким он был до «повторять этот трек» / «только этот трек»: Reever сам
/// поставил «один» или «выкл» и возвращает прежний на следующей команде воспроизведения —
/// иначе следующий альбом крутил бы по кругу свою первую песню.
static REPEAT_BEFORE: Mutex<Option<String>> = Mutex::new(None);

/// Поставить режим повтора, запомнив прежний (если он ещё не запомнен).
fn set_repeat_remembering(mode: &str) -> Result<(), CallError> {
    let before = call("now_playing", json!({}), CALL_TIMEOUT)
        .ok()
        .and_then(|now| now.get("repeat").and_then(Value::as_str).map(String::from));
    call("set_repeat", json!({ "mode": mode }), CALL_TIMEOUT)?;
    let mut saved = REPEAT_BEFORE.lock().unwrap_or_else(|e| e.into_inner());
    if saved.is_none() {
        *saved = before.filter(|before| before != mode);
    }
    Ok(())
}

/// Вернуть режим повтора, который Reever поменял для одного трека.
fn restore_repeat() {
    let before = REPEAT_BEFORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(mode) = before {
        let _ = call("set_repeat", json!({ "mode": mode }), CALL_TIMEOUT);
    }
}

/// Порядок песен исполнителя для list_songs; `None` — вперемешку (play с shuffle).
fn artist_sort(order: &str) -> Option<&'static str> {
    match order {
        "plays" => Some("plays"),
        "album" => Some("album"),
        _ => None,
    }
}

fn play_song(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some(song) = find_song(queries, run.options.strict)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let Some(id) = song.get("songId").and_then(Value::as_str).map(String::from) else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let mode = run.options.after_song.as_str();
    if matches!(mode, "repeat" | "single") {
        // очередь — только этот трек; повтор «один» или «выкл» (при «всё» трек крутился бы)
        call("play", json!({ "song_ids": [id] }), START_TIMEOUT)?;
        run.playing(&song_line(&song));
        set_repeat_remembering(if mode == "repeat" { "one" } else { "off" })?;
        return Ok(true);
    }
    restore_repeat();
    // перемешивание до команды: play без shuffle выключает оба режима
    let shuffle_before = if run.running && mode == "keep" {
        call("now_playing", json!({}), CALL_TIMEOUT)
            .ok()
            .and_then(|now| shuffle_mode(&now).map(String::from))
    } else {
        None
    };
    let mut queue = if mode == "artist" {
        let artist = song
            .get("artists")
            .and_then(Value::as_array)
            .and_then(|artists| artists.first())
            .and_then(Value::as_str)
            .map(String::from);
        let sort = artist_sort(&run.options.artist_order);
        artist
            .map(|artist| list_all(Some(&format!("artist:{artist}")), sort).unwrap_or_default())
            .unwrap_or_default()
    } else {
        list_all(None, None).unwrap_or_default()
    };
    queue.retain(|other| *other != id);
    queue.insert(0, id);
    let rest = queue.len() > 1;
    call(
        "play",
        json!({ "song_ids": queue, "start_index": 0 }),
        START_TIMEOUT,
    )?;
    run.playing(&song_line(&song));
    if !rest {
        return Ok(true);
    }
    // перемешивание переставляет очередь, играющий трек остаётся первым
    match mode {
        "keep" => match shuffle_before.as_deref() {
            Some("smart") => {
                enable_smart()?;
            }
            Some("shuffle") => {
                call("set_shuffle", json!({ "mode": "shuffle" }), CALL_TIMEOUT)?;
            }
            _ => {}
        },
        "artist" => {
            if artist_sort(&run.options.artist_order).is_none() {
                call("set_shuffle", json!({ "mode": "shuffle" }), CALL_TIMEOUT)?;
            }
        }
        _ => {
            enable_smart()?;
        }
    }
    Ok(true)
}

/// «Включи исполнителя X»: все его песни вперемешку.
fn find_artist(queries: &[String], strict: bool) -> Result<Option<String>, CallError> {
    let queries = distinct(queries);
    let mut found = Vec::new();
    for query in &queries {
        let result = search(query, "artists", 10)?;
        let names: Vec<(String, String)> = result
            .get("artists")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|name| (name.to_string(), name.to_string()))
            .collect();
        found.push((query.to_string(), names));
    }
    with_library(&found, &queries, strict, || {
        list_pages("list_artists", "items", 500, |artist| {
            let name = artist.get("name")?.as_str()?.to_string();
            Some((name.clone(), name))
        })
    })
}

fn play_artist(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some(artist) = find_artist(queries, run.options.strict)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let sort = artist_sort(&run.options.artist_order);
    let ids = list_all(Some(&format!("artist:{artist}")), sort)?;
    if ids.is_empty() {
        return Ok(run.not_found(&artist));
    }
    restore_repeat();
    call(
        "play",
        json!({ "song_ids": ids, "shuffle": sort.is_none() }),
        START_TIMEOUT,
    )?;
    run.playing(&artist);
    Ok(true)
}

/// «Включи альбом X»: альбом в порядке треков.
/// Альбом: (ключ для list_songs, название).
fn find_album(queries: &[String], strict: bool) -> Result<Option<(String, String)>, CallError> {
    let queries = distinct(queries);
    let mut found = Vec::new();
    for query in &queries {
        let result = search(query, "albums", 10)?;
        let albums: Vec<(String, (String, String))> = result
            .get("albums")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|album| {
                let key = album.get("key")?.as_str()?.to_string();
                let title = album
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Some((title.clone(), (key, title)))
            })
            .collect();
        found.push((query.to_string(), albums));
    }
    with_library(&found, &queries, strict, || {
        list_pages("list_albums", "albums", 500, |album| {
            let key = album.get("key")?.as_str()?.to_string();
            let title = album
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some((title.clone(), (key, title)))
        })
    })
}

fn play_album(run: &Run, queries: &[String]) -> Result<bool, CallError> {
    let Some((key, title)) = find_album(queries, run.options.strict)? else {
        return Ok(run.not_found(queries.first().map_or("", String::as_str)));
    };
    let ids = list_all(Some(&format!("album:{key}")), None)?;
    if ids.is_empty() {
        return Ok(run.not_found(&title));
    }
    restore_repeat();
    call(
        "play",
        json!({ "song_ids": ids, "shuffle": run.options.collection_shuffle }),
        START_TIMEOUT,
    )?;
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
    restore_repeat();
    call(
        "play",
        json!({ "song_ids": ids, "shuffle": run.options.collection_shuffle }),
        START_TIMEOUT,
    )?;
    run.playing(&name);
    Ok(true)
}

/// Одна страница list_songs в порядке `sort` (самые слушаемые, недавно добавленные).
fn top_songs(sort: &str) -> Result<Vec<String>, CallError> {
    let page = call(
        "list_songs",
        json!({ "limit": PAGE, "sort": sort }),
        CALL_TIMEOUT,
    )?;
    Ok(song_ids(&page))
}

/// «Включи музыку»: остановленное — продолжить; играет — ничего не трогать; иначе — что
/// выбрано в настройках (по умолчанию вся библиотека в Smart Shuffle).
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
    restore_repeat();
    // избранное, самые слушаемые, недавно добавленные — если там что-то есть
    let chosen = match run.options.play_music.as_str() {
        "favorites" => song_ids(&call(
            "get_playlist",
            json!({ "playlist_id": "Favorites" }),
            CALL_TIMEOUT,
        )?),
        "plays" => top_songs("plays")?,
        "added" => top_songs("added")?,
        _ => Vec::new(),
    };
    if !chosen.is_empty() {
        let shuffle = run.options.play_music != "added";
        call(
            "play",
            json!({ "song_ids": chosen, "shuffle": shuffle }),
            START_TIMEOUT,
        )?;
        let now = call("now_playing", json!({}), CALL_TIMEOUT).unwrap_or_default();
        run.playing(&song_line(&now));
        return Ok(true);
    }
    let ids = list_all(None, None)?;
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
    if !run.options.pause_fallback {
        ui_log(run.app, crate::msg!("log.music.nothing"));
        return Ok(false);
    }
    ui_log(run.app, crate::msg!("log.music.mediaKey"));
    Ok(crate::system::run_system("media_play_pause"))
}

/// Громкость из сказанного: «50», «пятьдесят», «семьдесят пять процентов», «fifty five»,
/// «максимум». Больше 100 или не число — `None`.
fn volume_percent(text: &str) -> Option<u8> {
    let digits = crate::textnorm::digits_in_text(text);
    let words: Vec<String> = normalize(&digits)
        .split_whitespace()
        .map(String::from)
        .collect();
    if words.iter().any(|w| {
        matches!(
            w.as_str(),
            "максимум" | "максимальная" | "max" | "maximum" | "full"
        )
    }) {
        return Some(100);
    }
    if words.iter().any(|w| {
        matches!(
            w.as_str(),
            "минимум" | "ноль" | "нуль" | "zero" | "min" | "minimum"
        )
    }) {
        return Some(0);
    }
    if let Some(number) = words.iter().find_map(|w| w.parse::<u32>().ok()) {
        return u8::try_from(number).ok().filter(|n| *n <= 100);
    }
    // английские числительные: «fifty five», «one hundred»
    let mut total = 0u32;
    let mut seen = false;
    for word in &words {
        let value = match word.as_str() {
            "one" => 1,
            "two" => 2,
            "three" => 3,
            "four" => 4,
            "five" => 5,
            "six" => 6,
            "seven" => 7,
            "eight" => 8,
            "nine" => 9,
            "ten" => 10,
            "eleven" => 11,
            "twelve" => 12,
            "thirteen" => 13,
            "fourteen" => 14,
            "fifteen" => 15,
            "sixteen" => 16,
            "seventeen" => 17,
            "eighteen" => 18,
            "nineteen" => 19,
            "twenty" => 20,
            "thirty" => 30,
            "forty" => 40,
            "fifty" => 50,
            "sixty" => 60,
            "seventy" => 70,
            "eighty" => 80,
            "ninety" => 90,
            "hundred" => {
                total = total.max(1) * 100;
                seen = true;
                continue;
            }
            _ => continue,
        };
        total += value;
        seen = true;
    }
    (seen && total <= 100).then_some(total as u8)
}

/// «Громкость 50»: громкость самой Неморы (её ползунок, не Windows).
fn volume(run: &Run, queries: &[String], running: bool) -> Result<bool, CallError> {
    let Some(percent) = queries.iter().find_map(|query| volume_percent(query)) else {
        ui_log(run.app, crate::msg!("log.music.badVolume"));
        return Ok(false);
    };
    if !running {
        return not_running(run);
    }
    call("set_volume", json!({ "percent": percent }), CALL_TIMEOUT)?;
    ui_log(run.app, crate::msg!("log.music.volume", percent = percent));
    Ok(true)
}

/// «Громче» / «тише»: громкость Неморы на шаг из настроек.
fn volume_step(run: &Run, up: bool) -> Result<bool, CallError> {
    if !run.running {
        return not_running(run);
    }
    let now = call("now_playing", json!({}), CALL_TIMEOUT)?;
    let current = now
        .get("volumePercent")
        .and_then(Value::as_f64)
        .unwrap_or(50.0);
    let step = f64::from(run.options.volume_step.clamp(1, 50));
    let next = (current + if up { step } else { -step })
        .clamp(0.0, 100.0)
        .round();
    call("set_volume", json!({ "percent": next }), CALL_TIMEOUT)?;
    ui_log(
        run.app,
        crate::msg!("log.music.volume", percent = next as u8),
    );
    Ok(true)
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
        MusicOp::Song => find_song(queries, false).map(|song| song.map(|song| song_line(&song))),
        MusicOp::Artist => find_artist(queries, false),
        MusicOp::Album => find_album(queries, false).map(|album| album.map(|(_, title)| title)),
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

    fn pick(found: &Value, query: &str) -> Option<String> {
        let found: Found<Value> = vec![(query.to_string(), song_candidates(found))];
        choose(&found).map(|song| song["songId"].as_str().unwrap_or_default().to_string())
    }

    #[test]
    fn exact_title_wins_and_versions_are_skipped() {
        let found = json!({ "songs": [
            { "songId": "a", "title": "Teen Spirit Cover", "artists": ["X"] },
            { "songId": "b", "viewId": "b:v1", "version": "Slowed", "title": "Smells Like Teen Spirit" },
            { "songId": "c", "title": "Smells Like Teen Spirit", "artists": ["Nirvana"] },
        ]});
        assert_eq!(
            pick(&found, "smells like teen spirit").as_deref(),
            Some("c")
        );
        // ничего похожего — не включать случайное: «не нашлось»
        assert_eq!(pick(&found, "совсем другое"), None);
        let only_versions = json!({ "songs": [{ "songId": "b", "viewId": "b:v1", "version": "Slowed", "title": "T" }] });
        assert_eq!(pick(&only_versions, "t").as_deref(), Some("b"));
        assert_eq!(pick(&json!({ "songs": [] }), "t"), None);
    }

    #[test]
    fn english_variant_finds_what_the_transliteration_did_not() {
        // «включи трек ай рили лайк ю парт ту»: русская модель — транслит, английская — слова
        let found: Found<&str> = vec![
            ("ай рили лайк ю парт ту".into(), vec![]),
            (
                "i really like you part two".into(),
                vec![
                    ("I Really Like You".into(), "short"),
                    ("I Really Like You Part 2".into(), "part2"),
                ],
            ),
        ];
        assert_eq!(choose(&found), Some("part2"));
        // русское название — точное совпадение русского варианта важнее похожего английского
        let found: Found<&str> = vec![
            ("группа крови".into(), vec![("Группа крови".into(), "kino")]),
            (
                "group of blood".into(),
                vec![("Blood Group".into(), "other")],
            ),
        ];
        assert_eq!(choose(&found), Some("kino"));
    }

    #[test]
    fn cyrillic_title_is_found_in_the_whole_library() {
        let library = || {
            Ok(vec![
                ("Antidepressant".to_string(), "ad"),
                ("Hate Music".to_string(), "hm"),
            ])
        };
        // поиск Неморы транслит не нашёл — сравнение со всей библиотекой
        let nothing: Found<&str> = vec![("хейт мьюзик".into(), vec![])];
        assert_eq!(
            with_library(&nothing, &["хейт мьюзик"], false, library),
            Ok(Some("hm"))
        );
        // точное совпадение в выдаче — библиотеку не листаем
        let exact: Found<&str> = vec![("hate music".into(), vec![("Hate Music".into(), "search")])];
        let untouched = || -> Result<Vec<(String, &str)>, CallError> { panic!("library listed") };
        assert_eq!(
            with_library(&exact, &["hate music"], false, untouched),
            Ok(Some("search"))
        );
        // латиница, по которой поиск ничего похожего не дал, — листаем
        let parts = || {
            Ok(vec![
                ("I Really Like You, Pt. 1".to_string(), "pt1"),
                ("I Really Like You pt2".to_string(), "pt2"),
            ])
        };
        let none: Found<&str> = vec![("i really like you part too".into(), vec![])];
        assert_eq!(
            with_library(&none, &["i really like you part too"], false, parts),
            Ok(Some("pt2"))
        );
        // латиница с похожим результатом — не листаем: опечатки поиск Неморы понимает сам
        let latin: Found<&str> = vec![("hate musik".into(), vec![("Hate Music".into(), "search")])];
        assert_eq!(
            with_library(&latin, &["hate musik"], false, untouched),
            Ok(Some("search"))
        );
        // в библиотеке ничего похожего — первый результат поиска, как раньше
        let junk: Found<&str> = vec![("хейт мьюзик".into(), vec![("Хейтеры".into(), "ru")])];
        let unrelated = || Ok(vec![("Antidepressant".to_string(), "ad")]);
        assert_eq!(
            with_library(&junk, &["хейт мьюзик"], false, unrelated),
            Ok(Some("ru"))
        );
    }

    #[test]
    fn parts_numbers_and_brackets_do_not_hide_the_title() {
        let parts = |query: &str| -> Found<&'static str> {
            vec![(
                query.to_string(),
                vec![
                    ("I Really Like You, Pt. 1".into(), "pt1"),
                    ("I Really Like You, Pt. 2".into(), "pt2"),
                ],
            )]
        };
        assert_eq!(choose(&parts("ай рили лайк ю парт ту")), Some("pt2"));
        assert_eq!(choose(&parts("i really like you part too")), Some("pt2"));
        assert_eq!(choose(&parts("i really like you part one")), Some("pt1"));
        assert_eq!(canonical("Vol. 3 — part two"), "volume 3 part 2");
        assert_eq!(
            canonical("I Really Like You pt2"),
            "i really like you part 2"
        );
        let glued: Found<&str> = vec![(
            "ай рили лайк ю парт ту".into(),
            vec![
                ("I Really Like You, Pt. 1".into(), "pt1"),
                ("I Really Like You pt2".into(), "pt2"),
            ],
        )];
        assert_eq!(choose(&glued), Some("pt2"));
        // «ту» вне номера части — «to»: «бэк ту блэк» = «Back to Black»
        assert_eq!(canonical("бэк ту блэк"), "бэк ту блэк");
        assert!(
            same("Back to Black", "бэк ту блэк")
                || similarity("Back to Black", "бэк ту блэк") >= MIN_SIMILAR
        );
        let albums: Found<&str> = vec![(
            "невермайнд".into(),
            vec![
                ("never seen".into(), "wrong"),
                ("Nevermind (Deluxe Edition)".into(), "right"),
            ],
        )];
        assert_eq!(choose(&albums), Some("right"));
        assert_eq!(
            name_variants("Song [Live] (2011)"),
            vec!["Song [Live] (2011)".to_string(), "Song  ".to_string()]
        );
    }

    #[test]
    fn volume_is_read_from_digits_and_words() {
        for (text, want) in [
            ("50", Some(50)),
            ("пятьдесят", Some(50)),
            ("семьдесят пять", Some(75)),
            ("на 30 процентов", Some(30)),
            ("сто", Some(100)),
            ("ноль", Some(0)),
            ("0", Some(0)),
            ("максимум", Some(100)),
            ("fifty five", Some(55)),
            ("one hundred", Some(100)),
            ("150", None),
            ("погромче", None),
        ] {
            assert_eq!(volume_percent(text), want, "{text}");
        }
    }

    #[test]
    fn glued_and_doubled_titles_are_heard_by_sound() {
        // «включи трек дэд матрикс» → «DEADMATRIX.exe», а не случайный первый результат
        let found: Found<&str> = vec![(
            "дэд матрикс".into(),
            vec![(
                "i fvcked up my happiness playing a nintendo emulator".into(),
                "junk",
            )],
        )];
        assert_eq!(choose(&found), None);
        let library: Found<&str> = vec![(
            "дэд матрикс".into(),
            vec![
                (
                    "i fvcked up my happiness playing a nintendo emulator".into(),
                    "junk",
                ),
                ("DEADMATRIX.exe".into(), "deadmatrix"),
            ],
        )];
        assert_eq!(choose(&library), Some("deadmatrix"));
        // «флюкс вэйв спидап» — «Fluxxwave (Sped Up)», не «Fluxxwave (Lay with Me)»
        let fluxx: Found<&str> = vec![(
            "флюкс вэйв спидап".into(),
            vec![
                ("Fluxxwave (Lay with Me)".into(), "lay"),
                ("Fluxxwave (Sped Up)".into(), "sped"),
            ],
        )];
        assert_eq!(choose(&fluxx), Some("sped"));
        // японское название по буквам не сравнить — ранжирование Неморы остаётся
        let japanese: Found<&str> = vec![("мушоку тенсей".into(), vec![("無職転生".into(), "jp")])];
        assert_eq!(choose(&japanese), Some("jp"));
    }

    #[test]
    fn names_match_exactly_or_fall_back_to_the_first() {
        let names = |query: &str, list: &[&'static str]| -> Found<&'static str> {
            vec![(
                query.to_string(),
                list.iter().map(|name| (name.to_string(), *name)).collect(),
            )]
        };
        assert_eq!(
            choose(&names("нирвана", &["Nirvanna Tribute", "Nirvana"])),
            Some("Nirvana")
        );
        assert_eq!(
            choose(&names("Nirvana", &["Nirvanna Tribute", "Nirvana"])),
            Some("Nirvana")
        );
        assert_eq!(choose(&names("x", &[])), None);
        assert_eq!(
            distinct(&["a".into(), " A ".into(), "".into(), "b".into()]),
            vec!["a", "b"]
        );
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
        let first = list_all(None, None).expect("list_songs");
        println!("library: {} songs", first.len());
        assert!(!first.is_empty());
        let playlists = call("list_playlists", json!({}), CALL_TIMEOUT).expect("list_playlists");
        println!("favorites: {:?}", pick_playlist(&playlists, "избранное"));
        shutdown();
    }
}
