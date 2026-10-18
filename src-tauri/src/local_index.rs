//! Компактный локальный индекс фиксированных дисков и управление его фоновым обновлением.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, GetDriveTypeW, GetLogicalDrives, ReadDirectoryChangesW,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, OPEN_EXISTING, WIN32_FIND_DATAW,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, ResetEvent, SetEvent, SetThreadPriority,
    WaitForMultipleObjects, THREAD_PRIORITY_LOWEST,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const MAGIC: &[u8; 8] = b"RVIDX001";
const SNAPSHOT_VERSION: u32 = 1;
const BLOCK_SIZE: usize = 512;
const BLOOM_BITS: usize = 32_768;
const BLOOM_WORDS: usize = BLOOM_BITS / 64;
const DIRECTORY_FLAG: u8 = 1;
const ROOT_PARENT: u32 = u32::MAX;
const ROOT_HANDLE: u64 = u64::MAX;

/// На диске и в памяти запись занимает ровно 12 байт; имена отдельно лежат в UTF-8-арене.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Record {
    parent: u32,
    name_offset: u32,
    name_len: u16,
    flags: u8,
    reserved: u8,
}

#[derive(Clone, Debug, Default)]
struct DriveIndex {
    records: Vec<Record>,
    names: Vec<u8>,
}

/// Фильтры обхода. `user_exclusions` зарезервирован под будущий список в настройках.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub default_exclusions: bool,
    pub user_exclusions: Vec<PathBuf>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            default_exclusions: true,
            user_exclusions: Vec::new(),
        }
    }
}

/// Неизменяемый снимок поиска. Обновления строят новый снимок и атомарно подменяют Arc.
#[derive(Debug)]
pub struct DiskIndex {
    records: Vec<Record>,
    names: Vec<u8>,
    /// Bloom-фильтр по блокам имён: 4 KiB на 512 записей.
    blocks: Vec<u64>,
    /// Отсортированные пары hash имени / ID для точного и префиксного поиска.
    exact_names: Vec<u64>,
    prefix3_names: Vec<u64>,
}

impl DiskIndex {
    fn from_drive_indexes(drives: Vec<DriveIndex>) -> Self {
        let total_records = drives.iter().map(|d| d.records.len()).sum();
        let total_names = drives.iter().map(|d| d.names.len()).sum();
        let mut records = Vec::with_capacity(total_records);
        let mut names = Vec::with_capacity(total_names);
        for drive in drives {
            let base = records.len() as u32;
            let name_base = names.len() as u32;
            names.extend_from_slice(&drive.names);
            records.extend(drive.records.into_iter().map(|mut record| {
                if record.parent != ROOT_PARENT {
                    record.parent = record.parent.saturating_add(base);
                }
                record.name_offset = record.name_offset.saturating_add(name_base);
                record
            }));
        }
        let blocks = build_blocks(&records, &names);
        let (exact_names, prefix3_names) = build_fast_indexes(&records, &names);
        Self {
            records,
            names,
            blocks,
            exact_names,
            prefix3_names,
        }
    }

    /// Для замеров и проб в examples/: снимок установленного Reever, если он есть, иначе
    /// полный обход фиксированных дисков с исключениями по умолчанию.
    pub fn load_or_scan(snapshot_path: &Path) -> Self {
        load_snapshot(snapshot_path).unwrap_or_else(|_| {
            scan_roots_with_progress(
                &fixed_drive_roots(),
                &ScanOptions::default(),
                Arc::new(AtomicBool::new(false)),
                None,
            )
        })
    }

    pub fn entries(&self) -> usize {
        self.records.len()
    }

    /// Ёмкость основных массивов и вспомогательного поиска, без накладных расходов Arc/Vec.
    pub fn memory_bytes(&self) -> usize {
        self.records.capacity() * std::mem::size_of::<Record>()
            + self.names.capacity()
            + self.blocks.capacity() * std::mem::size_of::<u64>()
            + self.exact_names.capacity() * std::mem::size_of::<u64>()
            + self.prefix3_names.capacity() * std::mem::size_of::<u64>()
    }

    fn name(&self, id: u32) -> &str {
        let r = self.records[id as usize];
        let start = r.name_offset as usize;
        let end = start
            .saturating_add(r.name_len as usize)
            .min(self.names.len());
        std::str::from_utf8(&self.names[start..end]).unwrap_or("")
    }

    pub fn path(&self, id: u32) -> String {
        let mut parts = Vec::new();
        let mut current = id;
        loop {
            let Some(record) = self.records.get(current as usize).copied() else {
                return String::new();
            };
            parts.push(self.name(current).to_string());
            if record.parent == ROOT_PARENT {
                break;
            }
            current = record.parent;
        }
        parts.reverse();
        let mut path = parts.first().cloned().unwrap_or_default();
        if !path.ends_with('\\') {
            path.push('\\');
        }
        for part in parts.iter().skip(1) {
            if !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(part);
        }
        path
    }

    /// Компактный синтаксис запросов find.rs: wfn:, startwith:, AND-слова и альтернативы
    /// через |.
    pub fn search_items(
        &self,
        expression: &str,
        scope: crate::find::FindScope,
        extension: Option<&str>,
        count: usize,
    ) -> Vec<(PathBuf, bool)> {
        let mut body = if extension.is_some() {
            expression.strip_prefix("file: ").unwrap_or(expression)
        } else {
            match scope {
                crate::find::FindScope::Folder => {
                    expression.strip_prefix("folder: ").unwrap_or(expression)
                }
                crate::find::FindScope::File => {
                    expression.strip_prefix("file: ").unwrap_or(expression)
                }
                crate::find::FindScope::Any => expression,
            }
        };
        if let Some(ext) = extension {
            let ext_filter = format!("ext:{ext}");
            body = body
                .strip_prefix(&ext_filter)
                .map(str::trim_start)
                .unwrap_or(body);
        }
        let (mode, query) = if let Some(q) = body.strip_prefix("wfn:") {
            (MatchMode::Exact, crate::find::squash(q))
        } else if let Some(q) = body.strip_prefix("startwith:") {
            (MatchMode::Prefix, crate::find::squash(q))
        } else {
            (MatchMode::Words, body.to_string())
        };
        let extension_only = extension.is_some() && body.is_empty();
        let groups = if extension_only {
            Vec::new()
        } else {
            parse_groups(&query)
        };
        if count == 0 {
            return Vec::new();
        }
        if !extension_only && mode == MatchMode::Exact {
            return self.search_exact(&query, scope, extension, count);
        }
        if !extension_only && mode == MatchMode::Prefix && query.chars().count() >= 3 {
            return self.search_prefix(&query, scope, extension, count);
        }
        let mut output = Vec::with_capacity(count.min(64));
        for block in 0..self.block_count() {
            if !self.block_might_match(block, &groups, mode) {
                continue;
            }
            let start = block * BLOCK_SIZE;
            let end = (start + BLOCK_SIZE).min(self.records.len());
            for id in start..end {
                let record = self.records[id];
                let folder = record.flags & DIRECTORY_FLAG != 0;
                if extension.is_none() && scope == crate::find::FindScope::Folder && !folder
                    || extension.is_none() && scope == crate::find::FindScope::File && folder
                {
                    continue;
                }
                if let Some(ext) = extension {
                    if folder
                        || Path::new(self.name(id as u32))
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| !s.eq_ignore_ascii_case(ext))
                            .unwrap_or(true)
                    {
                        continue;
                    }
                }
                let normalized = crate::find::squash(self.name(id as u32));
                let matched = extension_only
                    || match mode {
                        MatchMode::Exact => normalized == query,
                        MatchMode::Prefix => normalized.starts_with(&query),
                        MatchMode::Words => groups
                            .iter()
                            .all(|group| group.iter().any(|term| normalized.contains(&term.text))),
                    };
                if !matched {
                    continue;
                }
                output.push((PathBuf::from(self.path(id as u32)), folder));
                if output.len() >= count {
                    return output;
                }
            }
        }
        output
    }

    fn block_count(&self) -> usize {
        self.records.len().div_ceil(BLOCK_SIZE)
    }

    fn search_exact(
        &self,
        query: &str,
        scope: crate::find::FindScope,
        extension: Option<&str>,
        count: usize,
    ) -> Vec<(PathBuf, bool)> {
        let hash = normalized_name_hash(query);
        let (start, end) = key_range(&self.exact_names, hash);
        let mut output = Vec::with_capacity(count.min(64));
        for key in &self.exact_names[start..end] {
            let id = *key as u32;
            if !self.record_matches_type(id, scope, extension)
                || crate::find::squash(self.name(id)) != query
            {
                continue;
            }
            output.push((
                PathBuf::from(self.path(id)),
                self.records[id as usize].flags & DIRECTORY_FLAG != 0,
            ));
            if output.len() >= count {
                break;
            }
        }
        output
    }

    fn search_prefix(
        &self,
        query: &str,
        scope: crate::find::FindScope,
        extension: Option<&str>,
        count: usize,
    ) -> Vec<(PathBuf, bool)> {
        let chars: Vec<char> = query.chars().take(3).collect();
        let hash = gram_bit_with_tag(&chars, 19) as u32;
        let (start, end) = key_range(&self.prefix3_names, hash);
        let mut output = Vec::with_capacity(count.min(64));
        for key in &self.prefix3_names[start..end] {
            let id = *key as u32;
            if !self.record_matches_type(id, scope, extension)
                || !crate::find::squash(self.name(id)).starts_with(query)
            {
                continue;
            }
            output.push((
                PathBuf::from(self.path(id)),
                self.records[id as usize].flags & DIRECTORY_FLAG != 0,
            ));
            if output.len() >= count {
                break;
            }
        }
        output
    }

    fn record_matches_type(
        &self,
        id: u32,
        scope: crate::find::FindScope,
        extension: Option<&str>,
    ) -> bool {
        let folder = self.records[id as usize].flags & DIRECTORY_FLAG != 0;
        if extension.is_none()
            && ((scope == crate::find::FindScope::Folder && !folder)
                || (scope == crate::find::FindScope::File && folder))
        {
            return false;
        }
        if let Some(extension) = extension {
            if folder
                || Path::new(self.name(id))
                    .extension()
                    .and_then(|part| part.to_str())
                    .map(|part| !part.eq_ignore_ascii_case(extension))
                    .unwrap_or(true)
            {
                return false;
            }
        }
        true
    }

    fn block_might_match(&self, block: usize, groups: &[Vec<SearchTerm>], mode: MatchMode) -> bool {
        if groups.is_empty() || groups.iter().all(|g| g.iter().all(|t| t.text.is_empty())) {
            return true;
        }
        let words = &self.blocks[block * BLOOM_WORDS..(block + 1) * BLOOM_WORDS];
        if mode == MatchMode::Prefix {
            return groups
                .first()
                .into_iter()
                .flatten()
                .flat_map(|term| term.prefix_bits.iter().flatten())
                .any(|&bit| bloom_has(words, bit));
        }
        groups.iter().all(|group| {
            group.iter().any(|term| {
                term.contains_bits.is_empty()
                    || term.contains_bits.iter().all(|&bit| bloom_has(words, bit))
            })
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchMode {
    Exact,
    Prefix,
    Words,
}

#[derive(Clone, Debug)]
struct SearchTerm {
    text: String,
    contains_bits: Vec<usize>,
    prefix_bits: [Option<usize>; 3],
}

fn search_term(text: String) -> SearchTerm {
    let chars: Vec<char> = text.chars().collect();
    let prefix_bits = [2usize, 3, 4].map(|width| {
        (chars.len() >= width).then(|| gram_bit_with_tag(&chars[..width], width as u8 + 16))
    });
    SearchTerm {
        contains_bits: term_bits(&text),
        text,
        prefix_bits,
    }
}

fn parse_groups(query: &str) -> Vec<Vec<SearchTerm>> {
    query
        .split_whitespace()
        .map(|group| {
            group
                .split('|')
                .map(crate::find::squash)
                .filter(|term| !term.is_empty())
                .map(search_term)
                .collect::<Vec<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect()
}

fn term_bits(term: &str) -> Vec<usize> {
    let chars: Vec<char> = term.chars().collect();
    let mut bits = HashSet::new();
    for width in [2usize, 3] {
        for window in chars.windows(width) {
            bits.insert(gram_bit(window));
        }
    }
    bits.into_iter().collect()
}

fn gram_bit(chars: &[char]) -> usize {
    gram_bit_with_tag(chars, chars.len() as u8)
}

fn gram_bit_with_tag(chars: &[char], tag: u8) -> usize {
    let mut hash = 2_166_136_261u32 ^ tag as u32;
    for c in chars {
        for byte in (*c as u32).to_le_bytes() {
            hash ^= byte as u32;
            hash = hash.wrapping_mul(16_777_619);
        }
    }
    hash as usize & (BLOOM_BITS - 1)
}

fn bloom_set(words: &mut [u64], bit: usize) {
    words[bit / 64] |= 1u64 << (bit % 64);
}

fn bloom_has(words: &[u64], bit: usize) -> bool {
    words[bit / 64] & (1u64 << (bit % 64)) != 0
}

fn build_blocks(records: &[Record], names: &[u8]) -> Vec<u64> {
    let block_count = records.len().div_ceil(BLOCK_SIZE);
    let mut blocks = vec![0u64; block_count * BLOOM_WORDS];
    for (id, record) in records.iter().enumerate() {
        let start = record.name_offset as usize;
        let end = start
            .saturating_add(record.name_len as usize)
            .min(names.len());
        let name = std::str::from_utf8(&names[start..end]).unwrap_or("");
        let normalized = crate::find::squash(name);
        let chars: Vec<char> = normalized.chars().collect();
        let words =
            &mut blocks[(id / BLOCK_SIZE) * BLOOM_WORDS..(id / BLOCK_SIZE + 1) * BLOOM_WORDS];
        for width in [2usize, 3] {
            for gram in chars.windows(width) {
                bloom_set(words, gram_bit(gram));
            }
        }
        if chars.len() >= 2 {
            bloom_set(words, gram_bit_with_tag(&chars[..2], 18));
        }
        if chars.len() >= 3 {
            bloom_set(words, gram_bit_with_tag(&chars[..3], 19));
        }
        if chars.len() >= 4 {
            bloom_set(words, gram_bit_with_tag(&chars[..4], 20));
        }
    }
    blocks
}

fn build_fast_indexes(records: &[Record], names: &[u8]) -> (Vec<u64>, Vec<u64>) {
    let mut exact_names = Vec::with_capacity(records.len());
    let mut prefix3_names = Vec::with_capacity(records.len());
    for (id, record) in records.iter().enumerate() {
        let start = record.name_offset as usize;
        let end = start + record.name_len as usize;
        let original = std::str::from_utf8(&names[start..end]).unwrap_or("");
        let normalized = crate::find::squash(original);
        let id = id as u32;
        exact_names.push(((normalized_name_hash(&normalized) as u64) << 32) | u64::from(id));
        let chars: Vec<char> = normalized.chars().collect();
        if chars.len() >= 3 {
            prefix3_names.push(((gram_bit_with_tag(&chars[..3], 19) as u64) << 32) | u64::from(id));
        }
    }
    exact_names.sort_unstable();
    prefix3_names.sort_unstable();
    (exact_names, prefix3_names)
}

fn normalized_name_hash(name: &str) -> u32 {
    name.as_bytes().iter().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
    })
}

fn key_range(keys: &[u64], hash: u32) -> (usize, usize) {
    let hash = u64::from(hash);
    let start = keys.partition_point(|key| key >> 32 < hash);
    let end = keys.partition_point(|key| key >> 32 <= hash);
    (start, end)
}

#[cfg(test)]
fn add_record(index: &mut DriveIndex, parent: u32, name: &std::ffi::OsStr, folder: bool) -> u32 {
    let lossy = name.to_string_lossy();
    let bytes = lossy.as_bytes();
    let offset = index.names.len() as u32;
    index.names.extend_from_slice(bytes);
    let id = index.records.len() as u32;
    index.records.push(Record {
        parent,
        name_offset: offset,
        name_len: bytes.len().min(u16::MAX as usize) as u16,
        flags: if folder { DIRECTORY_FLAG } else { 0 },
        reserved: 0,
    });
    id
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn fixed_drive_roots() -> Vec<PathBuf> {
    let bits = unsafe { GetLogicalDrives() };
    let mut roots = Vec::new();
    for drive in 0..26 {
        if bits & (1 << drive) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + drive as u8) as char);
        let root_w = wide(std::ffi::OsStr::new(&root));
        // DRIVE_FIXED == 3, API value documented by Win32.
        if unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) } == 3 {
            roots.push(PathBuf::from(root));
        }
    }
    roots
}

fn excluded_directory(path: &Path, options: &ScanOptions) -> bool {
    let mut lower = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(path) = lower.strip_prefix(r"\\?\") {
        lower = path.to_string();
    }
    if options.user_exclusions.iter().any(|excluded| {
        let excluded = excluded.to_string_lossy().replace('/', "\\").to_lowercase();
        lower == excluded
            || lower.starts_with(&(excluded.trim_end_matches('\\').to_string() + "\\"))
    }) {
        return true;
    }
    if !options.default_exclusions {
        return false;
    }
    let parts: Vec<&str> = lower.split('\\').filter(|s| !s.is_empty()).collect();
    if parts
        .iter()
        .any(|p| matches!(*p, "node_modules" | "target" | ".git" | "windowsapps"))
    {
        return true;
    }
    if parts
        .iter()
        .any(|p| matches!(*p, "$recycle.bin" | "system volume information"))
    {
        return true;
    }
    if parts.len() >= 2 && parts[1] == "windows" {
        return true;
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let temp = PathBuf::from(local)
            .join("Temp")
            .to_string_lossy()
            .to_lowercase();
        if lower == temp || lower.starts_with(&(temp.trim_end_matches('\\').to_string() + "\\")) {
            return true;
        }
    }
    // Одноимённые полезные каталоги не исключаем вне профилей Chrome/Edge/Firefox.
    let browser = [
        "\\appdata\\local\\google\\chrome\\user data\\",
        "\\appdata\\local\\microsoft\\edge\\user data\\",
        "\\appdata\\local\\mozilla\\firefox\\profiles\\",
    ]
    .iter()
    .any(|prefix| lower.contains(prefix));
    if browser {
        let cache_names = [
            "cache",
            "cache2",
            "cachestorage",
            "code cache",
            "gpucache",
            "shadercache",
            "grshadercache",
            "service worker",
        ];
        if parts.iter().any(|p| cache_names.contains(p)) {
            return true;
        }
    }
    false
}

#[derive(Clone)]
struct ScanTask {
    directory: PathBuf,
    parent_handle: u64,
}

#[derive(Default)]
struct ScanQueueState {
    queue: VecDeque<ScanTask>,
    active: usize,
}

struct ScanQueue {
    state: Mutex<ScanQueueState>,
    ready: Condvar,
}

#[derive(Clone, Copy)]
struct ScanRecord {
    parent_handle: u64,
    name_offset: u32,
    name_len: u16,
    flags: u8,
    reserved: u8,
}

#[derive(Default)]
struct ScanShard {
    records: Vec<ScanRecord>,
    names: Vec<u8>,
}

impl ScanShard {
    fn push(
        &mut self,
        shard_id: usize,
        parent_handle: u64,
        name: &std::ffi::OsStr,
        folder: bool,
    ) -> u64 {
        let name = name.to_string_lossy();
        let offset = self.names.len() as u32;
        self.names.extend_from_slice(name.as_bytes());
        let local_id = self.records.len() as u32;
        self.records.push(ScanRecord {
            parent_handle,
            name_offset: offset,
            name_len: name.len().min(u16::MAX as usize) as u16,
            flags: if folder { DIRECTORY_FLAG } else { 0 },
            reserved: 0,
        });
        ((shard_id as u64) << 32) | u64::from(local_id)
    }
}

#[cfg(test)]
fn scan_roots(roots: &[PathBuf], options: &ScanOptions, stop: Arc<AtomicBool>) -> DiskIndex {
    scan_roots_with_progress(roots, options, stop, None)
}

fn scan_roots_with_progress(
    roots: &[PathBuf],
    options: &ScanOptions,
    stop: Arc<AtomicBool>,
    progress: Option<Arc<AtomicUsize>>,
) -> DiskIndex {
    let worker_count = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .min(8)
        .max(1);
    let queue = Arc::new(ScanQueue {
        state: Mutex::new(ScanQueueState::default()),
        ready: Condvar::new(),
    });
    let mut shards: Vec<ScanShard> = (0..worker_count).map(|_| ScanShard::default()).collect();
    {
        let mut state = queue.state.lock().unwrap();
        for root in roots {
            let label = root.to_string_lossy().trim_end_matches('\\').to_string();
            let parent = shards[0].push(0, ROOT_HANDLE, std::ffi::OsStr::new(&label), true);
            state.queue.push_back(ScanTask {
                directory: extended_root(root),
                parent_handle: parent,
            });
        }
    }
    if let Some(progress) = &progress {
        progress.store(roots.len(), Ordering::Relaxed);
    }

    let mut workers = Vec::with_capacity(worker_count);
    for (worker_id, shard) in shards.into_iter().enumerate() {
        let queue = queue.clone();
        let options = options.clone();
        let stop = stop.clone();
        let progress = progress.clone();
        workers.push(
            thread::Builder::new()
                .name(format!("reever-index-scan-{worker_id}"))
                .spawn(move || scan_worker(worker_id, shard, queue, options, stop, progress))
                .expect("не удалось запустить поток обхода индекса"),
        );
    }
    let shards = workers
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .collect();
    let drive = flatten_scan_shards(shards);
    DiskIndex::from_drive_indexes(vec![drive])
}

fn scan_worker(
    worker_id: usize,
    mut shard: ScanShard,
    queue: Arc<ScanQueue>,
    options: ScanOptions,
    stop: Arc<AtomicBool>,
    progress: Option<Arc<AtomicUsize>>,
) -> ScanShard {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
    loop {
        let task = {
            let mut state = queue.state.lock().unwrap();
            loop {
                if stop.load(Ordering::Relaxed) {
                    state.queue.clear();
                    if state.active == 0 {
                        queue.ready.notify_all();
                    }
                    break None;
                }
                if let Some(task) = state.queue.pop_front() {
                    state.active += 1;
                    break Some(task);
                }
                if state.active == 0 {
                    queue.ready.notify_all();
                    break None;
                }
                state = queue.ready.wait(state).unwrap();
            }
        };
        let Some(task) = task else { break };
        let before = shard.records.len();
        let children = scan_directory(worker_id, &task, &mut shard, &options, &stop);
        if let Some(progress) = &progress {
            progress.fetch_add(shard.records.len() - before, Ordering::Relaxed);
        }
        let mut state = queue.state.lock().unwrap();
        if !stop.load(Ordering::Relaxed) {
            state.queue.extend(children);
        }
        state.active -= 1;
        queue.ready.notify_all();
    }
    shard
}

fn scan_directory(
    worker_id: usize,
    task: &ScanTask,
    shard: &mut ScanShard,
    options: &ScanOptions,
    stop: &AtomicBool,
) -> Vec<ScanTask> {
    let mut pattern = task.directory.clone();
    pattern.push("*");
    let pattern_w = wide(pattern.as_os_str());
    let mut data = WIN32_FIND_DATAW::default();
    let first = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern_w.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let Ok(handle) = first else { return Vec::new() };
    let mut children = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let name_end = data
            .cFileName
            .iter()
            .position(|&character| character == 0)
            .unwrap_or(data.cFileName.len());
        let name = OsString::from_wide(&data.cFileName[..name_end]);
        if name != "." && name != ".." {
            let attributes = data.dwFileAttributes;
            let folder = attributes & 0x10 != 0;
            let reparse = attributes & 0x400 != 0;
            let child_path = if folder {
                Some(task.directory.join(&name))
            } else {
                None
            };
            let excluded = child_path
                .as_ref()
                .map(|path| excluded_directory(path, options))
                .unwrap_or(false);
            if !excluded {
                let id = shard.push(worker_id, task.parent_handle, &name, folder);
                if folder && !reparse {
                    if let Some(directory) = child_path {
                        children.push(ScanTask {
                            directory,
                            parent_handle: id,
                        });
                    }
                }
            }
        }
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(handle);
    }
    children
}

fn flatten_scan_shards(shards: Vec<ScanShard>) -> DriveIndex {
    let mut record_bases = Vec::with_capacity(shards.len());
    let mut record_count = 0usize;
    let mut names_count = 0usize;
    for shard in &shards {
        record_bases.push(record_count as u32);
        record_count += shard.records.len();
        names_count += shard.names.len();
    }
    let mut records = Vec::with_capacity(record_count);
    let mut names = Vec::with_capacity(names_count);
    for (shard_id, shard) in shards.into_iter().enumerate() {
        let names_base = names.len() as u32;
        for source in shard.records {
            let parent = if source.parent_handle == ROOT_HANDLE {
                ROOT_PARENT
            } else {
                let parent_shard = (source.parent_handle >> 32) as usize;
                let parent_local = source.parent_handle as u32;
                record_bases[parent_shard].saturating_add(parent_local)
            };
            records.push(Record {
                parent,
                name_offset: names_base.saturating_add(source.name_offset),
                name_len: source.name_len,
                flags: source.flags,
                reserved: source.reserved,
            });
        }
        names.extend_from_slice(&shard.names);
        let _ = shard_id;
    }
    DriveIndex { records, names }
}

fn extended_root(root: &Path) -> PathBuf {
    PathBuf::from(format!(r"\\?\{}", root.display()))
}

fn save_snapshot(index: &DiskIndex, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.to_path_buf();
    temp.set_extension("bin.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(MAGIC)?;
    file.write_all(&SNAPSHOT_VERSION.to_le_bytes())?;
    file.write_all(&(index.records.len() as u64).to_le_bytes())?;
    file.write_all(&(index.names.len() as u64).to_le_bytes())?;
    let mut chunk = Vec::with_capacity(64 * 1024);
    for record in &index.records {
        chunk.extend_from_slice(&record.parent.to_le_bytes());
        chunk.extend_from_slice(&record.name_offset.to_le_bytes());
        chunk.extend_from_slice(&record.name_len.to_le_bytes());
        chunk.push(record.flags);
        chunk.push(record.reserved);
        if chunk.len() >= 60 * 1024 {
            file.write_all(&chunk)?;
            chunk.clear();
        }
    }
    file.write_all(&chunk)?;
    file.write_all(&index.names)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn load_snapshot(path: &Path) -> std::io::Result<DiskIndex> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "версия снимка",
        ));
    }
    let version = read_u32(&mut file)?;
    if version != SNAPSHOT_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "версия снимка",
        ));
    }
    let count = usize::try_from(read_u64(&mut file)?).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "слишком большой снимок")
    })?;
    let names_len = usize::try_from(read_u64(&mut file)?).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "слишком большой снимок")
    })?;
    if count > u32::MAX as usize || names_len > u32::MAX as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "слишком большой снимок",
        ));
    }
    let expected_len = 28u64
        .checked_add(
            (count as u64)
                .checked_mul(12)
                .ok_or_else(invalid_snapshot)?,
        )
        .and_then(|len| len.checked_add(names_len as u64))
        .ok_or_else(invalid_snapshot)?;
    if file_len != expected_len {
        return Err(invalid_snapshot());
    }
    let mut records = Vec::with_capacity(count);
    let mut raw = [0u8; 12];
    for _ in 0..count {
        file.read_exact(&mut raw)?;
        records.push(Record {
            parent: u32::from_le_bytes(raw[0..4].try_into().unwrap()),
            name_offset: u32::from_le_bytes(raw[4..8].try_into().unwrap()),
            name_len: u16::from_le_bytes(raw[8..10].try_into().unwrap()),
            flags: raw[10],
            reserved: raw[11],
        });
    }
    let mut names = vec![0; names_len];
    file.read_exact(&mut names)?;
    for record in &records {
        let start = record.name_offset as usize;
        let end = start
            .checked_add(record.name_len as usize)
            .ok_or_else(invalid_snapshot)?;
        if end > names.len()
            || std::str::from_utf8(&names[start..end]).is_err()
            || (record.parent != ROOT_PARENT
                && (record.parent as usize >= records.len()
                    || records[record.parent as usize].flags & DIRECTORY_FLAG == 0))
        {
            return Err(invalid_snapshot());
        }
    }
    let mut colors = vec![0u8; records.len()];
    let mut chain = Vec::new();
    for start in 0..records.len() {
        chain.clear();
        let mut current = start;
        loop {
            match colors[current] {
                1 => return Err(invalid_snapshot()),
                2 => break,
                _ => {}
            }
            colors[current] = 1;
            chain.push(current);
            let parent = records[current].parent;
            if parent == ROOT_PARENT {
                break;
            }
            current = parent as usize;
        }
        for &id in &chain {
            colors[id] = 2;
        }
    }
    let blocks = build_blocks(&records, &names);
    let (exact_names, prefix3_names) = build_fast_indexes(&records, &names);
    Ok(DiskIndex {
        records,
        names,
        blocks,
        exact_names,
        prefix3_names,
    })
}

fn invalid_snapshot() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "повреждённый снимок индекса",
    )
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexStatus {
    pub state: &'static str,
    pub entries: usize,
    pub bytes: usize,
}

#[derive(Default)]
struct RuntimeStatus {
    enabled: bool,
    scanning: bool,
    entries: usize,
    bytes: usize,
}

struct ManagerInner {
    snapshot_path: PathBuf,
    index: RwLock<Option<Arc<DiskIndex>>>,
    status: Mutex<RuntimeStatus>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stop: Mutex<Option<Arc<AtomicBool>>>,
    scan_entries: Arc<AtomicUsize>,
    // Win32 HANDLE хранится как usize, чтобы состояние Tauri оставалось Send + Sync.
    stop_event: Mutex<Option<usize>>,
}

/// Переключатель индексатора: выключенный не оставляет ни снимка в памяти, ни рабочих потоков.
#[derive(Clone)]
pub struct IndexManager(Arc<ManagerInner>);

impl IndexManager {
    pub fn new(snapshot_path: PathBuf) -> Self {
        Self(Arc::new(ManagerInner {
            snapshot_path,
            index: RwLock::new(None),
            status: Mutex::new(RuntimeStatus::default()),
            worker: Mutex::new(None),
            stop: Mutex::new(None),
            scan_entries: Arc::new(AtomicUsize::new(0)),
            stop_event: Mutex::new(None),
        }))
    }

    pub fn set_enabled(&self, enabled: bool) {
        if enabled {
            self.enable();
        } else {
            self.disable();
        }
    }

    fn enable(&self) {
        if self.0.status.lock().map(|s| s.enabled).unwrap_or(false) {
            return;
        }
        self.disable();
        let stop = Arc::new(AtomicBool::new(false));
        let event = unsafe { CreateEventW(None, true, false, None) };
        let Ok(event) = event else { return };
        if let Ok(mut state) = self.0.status.lock() {
            state.enabled = true;
            state.scanning = true;
            state.entries = 0;
            state.bytes = 0;
        }
        let _ = self.0.index.write().map(|mut i| *i = None);
        *self.0.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop.clone());
        *self.0.stop_event.lock().unwrap_or_else(|e| e.into_inner()) = Some(event.0 as usize);
        let inner = self.0.clone();
        let event_raw = event.0 as usize;
        let worker = thread::Builder::new()
            .name("reever-index".into())
            .spawn(move || run_manager(inner, stop, event_raw));
        match worker {
            Ok(worker) => *self.0.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker),
            Err(_) => {
                let _ = self.0.stop.lock().map(|mut slot| slot.take());
                let _ = self.0.stop_event.lock().map(|mut slot| slot.take());
                if let Ok(mut state) = self.0.status.lock() {
                    *state = RuntimeStatus::default();
                }
                unsafe {
                    let _ = CloseHandle(event);
                }
            }
        }
    }

    pub fn disable(&self) {
        if let Some(stop) = self.0.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(event) = self
            .0
            .stop_event
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            unsafe {
                let _ = SetEvent(HANDLE(event as _));
            }
            if let Some(worker) = self
                .0
                .worker
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = worker.join();
            }
            unsafe {
                let _ = CloseHandle(HANDLE(event as _));
            }
        }
        let _ = self.0.index.write().map(|mut i| *i = None);
        if let Ok(mut state) = self.0.status.lock() {
            *state = RuntimeStatus::default();
        }
        self.0.scan_entries.store(0, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Option<Arc<DiskIndex>> {
        self.0.index.read().ok().and_then(|i| i.clone())
    }

    pub fn status(&self) -> IndexStatus {
        let state = self.0.status.lock().unwrap_or_else(|e| e.into_inner());
        IndexStatus {
            state: if !state.enabled {
                "off"
            } else if state.scanning {
                "scanning"
            } else {
                "ready"
            },
            entries: if state.scanning {
                self.0.scan_entries.load(Ordering::Relaxed)
            } else {
                state.entries
            },
            bytes: state.bytes,
        }
    }
}

fn publish(inner: &ManagerInner, index: DiskIndex, scanning: bool) {
    let entries = index.entries();
    let bytes = index.memory_bytes();
    let index = Arc::new(index);
    if let Ok(mut slot) = inner.index.write() {
        *slot = Some(index);
    }
    if let Ok(mut state) = inner.status.lock() {
        state.enabled = true;
        state.scanning = scanning;
        state.entries = entries;
        state.bytes = bytes;
    }
}

fn run_manager(inner: Arc<ManagerInner>, stop: Arc<AtomicBool>, stop_event: usize) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
    let roots = fixed_drive_roots();
    let (events_tx, events_rx) = mpsc::sync_channel::<()>(1);
    let mut watchers = Vec::new();
    let (ready_tx, ready_rx) = mpsc::channel();
    for root in &roots {
        let root = root.clone();
        let tx = events_tx.clone();
        let stop = stop.clone();
        let ready = ready_tx.clone();
        let event = stop_event;
        match thread::Builder::new()
            .name("reever-index-watch".into())
            .spawn(move || watch_root(root, tx, stop, event, ready))
        {
            Ok(worker) => watchers.push(worker),
            Err(_) => {
                let _ = ready_tx.send(());
            }
        }
    }
    drop(ready_tx);
    // Дожидаемся первой заявки ReadDirectoryChangesW до чтения снимка. События,
    // пришедшие во время загрузки и сверки, остаются в канале и вызовут перескан.
    for _ in 0..roots.len() {
        if ready_rx.recv_timeout(Duration::from_secs(5)).is_err() {
            break;
        }
    }
    if let Ok(index) = load_snapshot(&inner.snapshot_path) {
        publish(&inner, index, true);
    }
    let options = ScanOptions::default();
    while !stop.load(Ordering::Relaxed) {
        if let Ok(mut status) = inner.status.lock() {
            status.scanning = true;
        }
        inner.scan_entries.store(0, Ordering::Relaxed);
        let started = Instant::now();
        let index = scan_roots_with_progress(
            &roots,
            &options,
            stop.clone(),
            Some(inner.scan_entries.clone()),
        );
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let _ = save_snapshot(&index, &inner.snapshot_path);
        publish(&inner, index, false);
        // Любое изменение и переполнение уведомителя сводятся к debounce + перескану.
        loop {
            match events_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(()) => break,
                Err(mpsc::RecvTimeoutError::Timeout) if !stop.load(Ordering::Relaxed) => continue,
                Err(_) => break,
            }
        }
        while events_rx.recv_timeout(Duration::from_millis(700)).is_ok() {}
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if started.elapsed().as_secs() > 0 {
            if let Ok(mut status) = inner.status.lock() {
                status.scanning = true;
            }
        }
    }
    for watcher in watchers {
        let _ = watcher.join();
    }
}

fn watch_root(
    root: PathBuf,
    changed: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
    stop_event: usize,
    ready: mpsc::Sender<()>,
) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
    let root_w = wide(root.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(root_w.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    };
    let Ok(handle) = handle else {
        let _ = ready.send(());
        return;
    };
    let event = unsafe { CreateEventW(None, true, false, None) };
    let Ok(event) = event else {
        let _ = ready.send(());
        unsafe {
            let _ = CloseHandle(handle);
        }
        return;
    };
    let mut buffer = vec![0u8; 64 * 1024];
    let mut announced_ready = false;
    let mut overlapped = OVERLAPPED::default();
    let mut request_pending = false;
    while !stop.load(Ordering::Relaxed) {
        let _ = unsafe { ResetEvent(event) };
        overlapped.hEvent = event;
        let result = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                None,
                Some(&mut overlapped),
                None,
            )
        };
        if result.is_err() {
            break;
        }
        request_pending = true;
        if !announced_ready {
            let _ = ready.send(());
            announced_ready = true;
        }
        let waited =
            unsafe { WaitForMultipleObjects(&[HANDLE(stop_event as _), event], false, u32::MAX) };
        if waited == WAIT_OBJECT_0 {
            break;
        }
        let mut bytes = 0;
        if unsafe { GetOverlappedResult(handle, &overlapped, &mut bytes, false) }.is_err() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            request_pending = false;
            let _ = changed.try_send(());
            continue;
        }
        request_pending = false;
        // Нулевой размер означает переполнение буфера и требует полной сверки.
        let _overflow = bytes == 0;
        let _ = changed.try_send(());
    }
    if !announced_ready {
        let _ = ready.send(());
    }
    unsafe {
        if request_pending {
            let _ = CancelIoEx(handle, Some(&overlapped));
            let mut ignored = 0;
            let _ = GetOverlappedResult(handle, &overlapped, &mut ignored, true);
        }
        let _ = CloseHandle(event);
        let _ = CloseHandle(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> DiskIndex {
        let mut drive = DriveIndex::default();
        add_record(&mut drive, ROOT_PARENT, std::ffi::OsStr::new("Z:"), true);
        let folder = add_record(&mut drive, 0, std::ffi::OsStr::new("Memory_Vault"), true);
        add_record(
            &mut drive,
            folder,
            std::ffi::OsStr::new("bilingual.md"),
            false,
        );
        add_record(&mut drive, 0, std::ffi::OsStr::new("downloads"), true);
        add_record(&mut drive, 0, std::ffi::OsStr::new("report.pdf"), false);
        DiskIndex::from_drive_indexes(vec![drive])
    }

    #[test]
    fn compact_record_is_twelve_bytes() {
        assert_eq!(std::mem::size_of::<Record>(), 12);
    }

    #[test]
    fn block_filter_preserves_contains_prefix_and_fuzzy_head_matches() {
        let index = fixture();
        assert_eq!(
            index
                .search_items("wfn:memoryvault", crate::find::FindScope::Any, None, 10)
                .len(),
            1
        );
        assert!(index
            .search_items("wfn:memory", crate::find::FindScope::Any, None, 10)
            .is_empty());
        assert_eq!(
            index
                .search_items("memory", crate::find::FindScope::Any, None, 10)
                .len(),
            1
        );
        assert_eq!(
            index
                .search_items("startwith:mem", crate::find::FindScope::Folder, None, 10)
                .len(),
            1
        );
        assert_eq!(
            index
                .search_items("bilingual", crate::find::FindScope::File, None, 10)
                .len(),
            1
        );
    }

    #[test]
    fn extension_queries_keep_the_name_filter_and_support_extension_only() {
        let index = fixture();
        assert_eq!(
            index
                .search_items(
                    "file: ext:pdf wfn:reportpdf",
                    crate::find::FindScope::File,
                    Some("pdf"),
                    10
                )
                .len(),
            1
        );
        assert_eq!(
            index
                .search_items(
                    "file: ext:pdf",
                    crate::find::FindScope::File,
                    Some("pdf"),
                    10
                )
                .len(),
            1
        );
        assert!(index
            .search_items(
                "file: ext:txt wfn:report",
                crate::find::FindScope::File,
                Some("txt"),
                10
            )
            .is_empty());
    }

    #[test]
    fn local_fuzzy_candidates_use_the_shared_find_ranker() {
        let index = fixture();
        let usage = crate::find::Usage::default();
        let recent = std::collections::HashMap::new();
        let items = crate::find::search(
            &index,
            &["memorvold".to_string()],
            crate::find::FindScope::Any,
            None,
            &usage,
            &recent,
        );
        assert_eq!(
            items.first().map(|item| item.name.as_str()),
            Some("Memory_Vault")
        );
    }

    #[test]
    fn disabled_index_leaves_nothing_in_memory() {
        let manager = IndexManager::new(std::env::temp_dir().join("reever-index-disabled.bin"));
        manager.set_enabled(false);
        let status = manager.status();
        assert_eq!(status.state, "off");
        assert_eq!(status.entries, 0);
        assert_eq!(status.bytes, 0);
        assert!(manager.snapshot().is_none());
    }

    #[test]
    fn scanning_status_reports_live_record_progress() {
        let manager = IndexManager::new(std::env::temp_dir().join("reever-index-progress.bin"));
        {
            let mut status = manager.0.status.lock().unwrap();
            status.enabled = true;
            status.scanning = true;
        }
        manager.0.scan_entries.store(42, Ordering::Relaxed);
        let status = manager.status();
        assert_eq!(status.state, "scanning");
        assert_eq!(status.entries, 42);
    }

    #[test]
    fn snapshot_round_trip_and_path_reconstruction() {
        let index = fixture();
        let path =
            std::env::temp_dir().join(format!("reever-index-test-{}.bin", std::process::id()));
        save_snapshot(&index, &path).unwrap();
        let loaded = load_snapshot(&path).unwrap();
        save_snapshot(&loaded, &path).unwrap();
        let loaded = load_snapshot(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.entries(), index.entries());
        assert_eq!(loaded.path(2), "Z:\\Memory_Vault\\bilingual.md");
        assert_eq!(loaded.memory_bytes(), index.memory_bytes());
    }

    #[test]
    fn excluded_paths_are_scoped_and_reparse_boundary_is_not_followed() {
        let options = ScanOptions::default();
        assert!(excluded_directory(
            Path::new("C:\\Windows\\System32"),
            &options
        ));
        assert!(excluded_directory(
            Path::new("C:\\repo\\node_modules\\pkg"),
            &options
        ));
        assert!(excluded_directory(
            Path::new("C:\\Users\\u\\AppData\\Local\\Microsoft\\WindowsApps\\python.exe"),
            &options
        ));
        assert!(excluded_directory(
            Path::new("C:\\Users\\u\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache"),
            &options
        ));
        assert!(!excluded_directory(Path::new("C:\\repo\\Cache"), &options));
        assert!(!excluded_directory(
            Path::new("D:\\Media\\Windows"),
            &options
        ));
        assert!(excluded_directory(
            Path::new(r"\\?\C:\repo\node_modules\pkg"),
            &options
        ));
    }

    #[test]
    fn rescanning_temp_tree_reflects_folder_create_rename_and_delete() {
        let base = std::env::temp_dir().join(format!("reever-index-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("created")).unwrap();
        fs::write(base.join("created").join("before.txt"), "x").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let options = ScanOptions {
            default_exclusions: false,
            user_exclusions: Vec::new(),
        };
        let first = scan_roots(&[base.clone()], &options, stop.clone());
        let snapshot = base.with_extension("index-test.bin");
        save_snapshot(&first, &snapshot).unwrap();
        let loaded = load_snapshot(&snapshot).unwrap();
        assert_eq!(
            loaded
                .search_items("before", crate::find::FindScope::Any, None, 10)
                .len(),
            1
        );
        let _ = fs::remove_file(snapshot);
        assert_eq!(
            first
                .search_items("before", crate::find::FindScope::Any, None, 10)
                .len(),
            1
        );
        assert_eq!(
            first
                .search_items("created", crate::find::FindScope::Folder, None, 10)
                .len(),
            1
        );
        fs::rename(base.join("created"), base.join("renamed")).unwrap();
        fs::rename(
            base.join("renamed").join("before.txt"),
            base.join("renamed").join("after.txt"),
        )
        .unwrap();
        let second = scan_roots(&[base.clone()], &options, stop.clone());
        assert_eq!(
            second
                .search_items("before", crate::find::FindScope::Any, None, 10)
                .len(),
            0
        );
        assert_eq!(
            second
                .search_items("renamed", crate::find::FindScope::Folder, None, 10)
                .len(),
            1
        );
        assert_eq!(
            second
                .search_items("after", crate::find::FindScope::Any, None, 10)
                .len(),
            1
        );
        fs::remove_dir_all(base.join("renamed")).unwrap();
        let third = scan_roots(&[base.clone()], &options, stop.clone());
        assert_eq!(
            third
                .search_items("after", crate::find::FindScope::Any, None, 10)
                .len(),
            0
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    #[ignore = "долгий замер по всем фиксированным дискам; запускать вручную при верификации"]
    fn measure_all_fixed_drives_with_and_without_exclusions() {
        let roots = fixed_drive_roots();
        for options in [
            ScanOptions {
                default_exclusions: false,
                user_exclusions: Vec::new(),
            },
            ScanOptions::default(),
        ] {
            let start = Instant::now();
            let index = scan_roots(&roots, &options, Arc::new(AtomicBool::new(false)));
            let elapsed = start.elapsed();
            println!(
                "scan excluded={} entries={} bytes={} helper_bytes={} elapsed_ms={}",
                options.default_exclusions,
                index.entries(),
                index.memory_bytes(),
                (index.blocks.capacity()
                    + index.exact_names.capacity()
                    + index.prefix3_names.capacity())
                    * 8,
                elapsed.as_millis()
            );
            for (query, typo) in [
                ("downloads", "downloadz"),
                ("memoryvault", "memorvold"),
                ("bilingual", "bilingol"),
                ("projectgst", "projectgxt"),
                ("pictures", "pictuers"),
            ] {
                let usage = crate::find::Usage::default();
                let recent = std::collections::HashMap::new();
                let started = Instant::now();
                let substring = crate::find::search(
                    &index,
                    &[query.to_string()],
                    crate::find::FindScope::Any,
                    None,
                    &usage,
                    &recent,
                )
                .len();
                let substring_ms = started.elapsed().as_millis();
                let started = Instant::now();
                let fuzzy = crate::find::search(
                    &index,
                    &[typo.to_string()],
                    crate::find::FindScope::Any,
                    None,
                    &usage,
                    &recent,
                )
                .len();
                let fuzzy_ms = started.elapsed().as_millis();
                println!("query={query} substring_ms={substring_ms} results={substring} fuzzy_ms={fuzzy_ms} fuzzy_results={fuzzy}");
            }
        }
    }
}
