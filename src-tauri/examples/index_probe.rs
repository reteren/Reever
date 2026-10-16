//! Измерительная проба собственного индекса Windows без повышенных прав.
//!
//! Запуск из src-tauri: cargo run --release --example index_probe -- --query=downloads
//! По умолчанию измеряет полный и исключающий проход. Имена файлов не печатает.

use std::collections::VecDeque;
use std::ffi::{c_void, OsString};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use strsim::jaro_winkler;

const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;
const FIND_EX_INFO_BASIC: i32 = 1;
const FIND_EX_SEARCH_NAME_MATCH: i32 = 0;
const FIND_FIRST_EX_LARGE_FETCH: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const ROOT_PARENT: u64 = u64::MAX;
const FUZZY_MIN: f64 = 0.492; // тот же 0.6 * 0.82, что и в find.rs

#[repr(C)]
#[derive(Clone, Copy)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
struct FindDataW {
    attributes: u32,
    creation_time: FileTime,
    access_time: FileTime,
    write_time: FileTime,
    size_high: u32,
    size_low: u32,
    reserved0: u32,
    reserved1: u32,
    name: [u16; 260],
    alternate_name: [u16; 14],
}

#[repr(C)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLogicalDriveStringsW(length: u32, buffer: *mut u16) -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn FindFirstFileExW(
        file_name: *const u16,
        info_level: i32,
        find_data: *mut FindDataW,
        search_op: i32,
        search_filter: *mut c_void,
        additional_flags: u32,
    ) -> *mut c_void;
    fn FindNextFileW(find_handle: *mut c_void, find_data: *mut FindDataW) -> i32;
    fn FindClose(find_handle: *mut c_void) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}

#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(
        process: *mut c_void,
        counters: *mut ProcessMemoryCountersEx,
        size: u32,
    ) -> i32;
}

/// Финальная компактная запись: ссылка на родителя, диапазон имени в арене, флаг типа.
#[repr(C)]
#[derive(Clone, Copy)]
struct IndexRecord {
    parent: u32,
    name_offset: u32,
    name_len: u16,
    flags: u8,
    _reserved: u8,
}

/// Во время параллельного обхода каждый поток пишет в свой shard без общей блокировки.
/// parent_handle = (номер shard << 32) | локальный индекс родителя.
#[repr(C)]
#[derive(Clone, Copy)]
struct ShardRecord {
    parent_handle: u64,
    name_offset: u32,
    name_len: u16,
    flags: u8,
    _reserved: u8,
}

#[derive(Default)]
struct Shard {
    names: Vec<u8>,
    records: Vec<ShardRecord>,
}

impl Shard {
    fn push(&mut self, shard_id: usize, parent: u64, name: &str, folder: bool) -> u64 {
        let offset = self.names.len() as u32;
        let bytes = name.as_bytes();
        self.names.extend_from_slice(bytes);
        let local_index = self.records.len() as u32;
        self.records.push(ShardRecord {
            parent_handle: parent,
            name_offset: offset,
            name_len: bytes.len() as u16,
            flags: u8::from(folder),
            _reserved: 0,
        });
        ((shard_id as u64) << 32) | u64::from(local_index)
    }
}

struct Index {
    names: Vec<u8>,
    records: Vec<IndexRecord>,
}

impl Index {
    fn name(&self, index: usize) -> &str {
        let record = &self.records[index];
        let begin = record.name_offset as usize;
        let end = begin + record.name_len as usize;
        // All bytes come from String::from_utf16_lossy(...).as_bytes().
        unsafe { std::str::from_utf8_unchecked(&self.names[begin..end]) }
    }

    #[allow(dead_code)]
    fn full_path(&self, index: u32) -> String {
        let mut parts = Vec::new();
        let mut current = index;
        loop {
            let record = self.records[current as usize];
            parts.push(self.name(current as usize));
            if record.parent == u32::MAX {
                break;
            }
            current = record.parent;
        }
        parts.reverse();
        let mut path = String::new();
        for part in parts {
            if path.ends_with(':') {
                path.push('\\');
            } else if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(part);
        }
        path
    }

    fn search_substring(&self, query: &str) -> (u64, Duration) {
        let needle = squash(query);
        let started = Instant::now();
        let mut count = 0;
        for i in 0..self.records.len() {
            if squash(self.name(i)).contains(&needle) {
                count += 1;
            }
        }
        (count, started.elapsed())
    }

    fn search_fuzzy(&self, query: &str) -> (u64, f64, Duration) {
        let needle = squash(query);
        let started = Instant::now();
        let mut count = 0;
        let mut best = 0.0f64;
        for i in 0..self.records.len() {
            let name = squash(self.name(i));
            let stem = name.rsplit_once('.').map_or(name.as_str(), |pair| pair.0);
            // find.rs умножает Jaro-Winkler на 0.6 и принимает fuzzy от 0.492.
            let score = 0.6 * jaro_winkler(&needle, stem).max(jaro_winkler(&needle, &name));
            if score >= FUZZY_MIN {
                count += 1;
            }
            best = best.max(score);
        }
        (count, best, started.elapsed())
    }
}

#[derive(Clone)]
struct Task {
    directory: PathBuf,
    parent_handle: u64,
}

#[derive(Default)]
struct QueueState {
    queue: VecDeque<Task>,
    active: usize,
    done: bool,
}

struct WorkQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct Counters {
    records: AtomicU64,
    directories: AtomicU64,
    errors: AtomicU64,
    excluded_directories: AtomicU64,
    reparse_directories: AtomicU64,
}

struct ScanResult {
    index: Index,
    elapsed: Duration,
    counters: Counters,
}

fn main() {
    let mut query = "downloads".to_string();
    let mut worker_count = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
        .max(1);
    let mut passes = "both".to_string();
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--query=") {
            query = value.to_string();
        } else if let Some(value) = arg.strip_prefix("--threads=") {
            worker_count = value.parse::<usize>().unwrap_or(worker_count).clamp(1, 64);
        } else if let Some(value) = arg.strip_prefix("--passes=") {
            passes = value.to_string();
        }
    }

    if size_of::<IndexRecord>() != 12 {
        eprintln!(
            "Ожидалась запись индекса 12 байт, фактически {}",
            size_of::<IndexRecord>()
        );
        std::process::exit(2);
    }
    let roots = fixed_roots();
    println!(
        "Фиксированные диски: {}; потоки: {worker_count}; запрос: {query:?}",
        roots.join(", ")
    );
    println!(
        "Запись индекса: {} байт; FFI-запись Windows: {} байт",
        size_of::<IndexRecord>(),
        size_of::<FindDataW>()
    );
    if roots.is_empty() {
        eprintln!("Фиксированные диски не найдены.");
        std::process::exit(1);
    }

    let do_full = passes == "both" || passes == "full";
    let do_filtered = passes == "both" || passes == "filtered";
    for (filtered, label) in [(false, "полный"), (true, "с исключениями")] {
        if (filtered && !do_filtered) || (!filtered && !do_full) {
            continue;
        }
        println!("\n=== Проход: {label} ===");
        let result = scan_index(&roots, worker_count, filtered);
        let current_private = private_bytes().unwrap_or(0);
        let records_bytes = result.index.records.len() * size_of::<IndexRecord>();
        println!(
            "Обход: {:.2?}; записей: {}; ошибок доступа/перечисления: {}",
            result.elapsed,
            result.index.records.len(),
            result.counters.errors.load(Ordering::Relaxed)
        );
        println!(
            "Директории: {}; пропущено исключённых: {}; записей-переходов (не обходились): {}",
            result.counters.directories.load(Ordering::Relaxed),
            result.counters.excluded_directories.load(Ordering::Relaxed),
            result.counters.reparse_directories.load(Ordering::Relaxed)
        );
        println!("Память: private bytes процесса={} MiB; арена имен={} MiB; записи={} MiB; суммарно полезных данных={} MiB",
            mib_u64(current_private), mib(result.index.names.len()), mib(records_bytes),
            mib(result.index.names.len() + records_bytes));

        let (substring_count, substring_time) = result.index.search_substring(&query);
        println!(
            "Подстрока по всем именам: совпадений={substring_count}; время={substring_time:.2?}"
        );
        let (fuzzy_count, fuzzy_best, fuzzy_time) = result.index.search_fuzzy(&query);
        println!("Fuzzy Jaro-Winkler по всем именам: совпадений (score >= {FUZZY_MIN:.3})={fuzzy_count}; max={fuzzy_best:.3}; время={fuzzy_time:.2?}");
        println!(
            "Private bytes после поисковых проходов: {} MiB",
            mib_u64(private_bytes().unwrap_or(0))
        );

        drop(result.index);
        println!(
            "Private bytes после освобождения индекса: {} MiB",
            mib_u64(private_bytes().unwrap_or(0))
        );
    }
}

fn fixed_roots() -> Vec<String> {
    let mut buffer = vec![0u16; 1024];
    let length =
        unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) } as usize;
    if length == 0 || length >= buffer.len() {
        return Vec::new();
    }
    let mut roots = Vec::new();
    for part in buffer[..length]
        .split(|c| *c == 0)
        .filter(|part| !part.is_empty())
    {
        let drive: Vec<u16> = part.iter().copied().chain([0]).collect();
        if unsafe { GetDriveTypeW(drive.as_ptr()) } == DRIVE_FIXED {
            roots.push(String::from_utf16_lossy(part));
        }
    }
    roots
}

fn scan_index(root_names: &[String], worker_count: usize, filtered: bool) -> ScanResult {
    let started = Instant::now();
    let queue = Arc::new(WorkQueue {
        state: Mutex::new(QueueState::default()),
        ready: Condvar::new(),
    });
    let counters = Arc::new(Counters::default());
    let mut shards: Vec<Shard> = (0..worker_count).map(|_| Shard::default()).collect();
    let mut root_paths = Vec::new();

    for root_name in root_names {
        let path = extended_root(root_name);
        let root_label = root_name.trim_end_matches('\\');
        let root_handle = shards[0].push(0, ROOT_PARENT, root_label, true);
        root_paths.push(root_name.clone());
        queue.state.lock().unwrap().queue.push_back(Task {
            directory: path,
            parent_handle: root_handle,
        });
        counters.records.fetch_add(1, Ordering::Relaxed);
    }
    let initial_roots = root_names.len();
    println!(
        "Корни: {}; стартовая запись на том: {initial_roots}",
        root_paths.join(", ")
    );

    let mut joins = Vec::with_capacity(worker_count);
    for (worker_id, shard) in shards.into_iter().enumerate() {
        let queue = queue.clone();
        let counters = counters.clone();
        joins.push(
            thread::Builder::new()
                .name(format!("index-{worker_id}"))
                .spawn(move || worker(worker_id, shard, queue, counters, filtered))
                .expect("создание потока обхода"),
        );
    }

    loop {
        let state = queue.state.lock().unwrap();
        if state.done {
            break;
        }
        let (state, timeout) = queue
            .ready
            .wait_timeout(state, Duration::from_secs(10))
            .unwrap();
        if timeout.timed_out() && !state.done {
            println!(
                "Прогресс: {} записей, {} каталогов, {} ошибок",
                counters.records.load(Ordering::Relaxed),
                counters.directories.load(Ordering::Relaxed),
                counters.errors.load(Ordering::Relaxed)
            );
        }
    }

    let mut shards = Vec::with_capacity(worker_count);
    for join in joins {
        shards.push(join.join().expect("поток обхода завершился с panic"));
    }
    let index = flatten_shards(shards);
    ScanResult {
        index,
        elapsed: started.elapsed(),
        counters: Arc::try_unwrap(counters).unwrap_or_else(|_| panic!("счётчики ещё используются")),
    }
}

fn worker(
    worker_id: usize,
    mut shard: Shard,
    queue: Arc<WorkQueue>,
    counters: Arc<Counters>,
    filtered: bool,
) -> Shard {
    loop {
        let task = {
            let mut state = queue.state.lock().unwrap();
            loop {
                if let Some(task) = state.queue.pop_front() {
                    state.active += 1;
                    break Some(task);
                }
                if state.active == 0 {
                    state.done = true;
                    queue.ready.notify_all();
                    break None;
                }
                state = queue.ready.wait(state).unwrap();
            }
        };
        let Some(task) = task else { break };
        let children = enumerate_directory(worker_id, &task, &mut shard, &counters, filtered);
        let mut state = queue.state.lock().unwrap();
        for child in children {
            state.queue.push_back(child);
        }
        state.active -= 1;
        if state.active == 0 && state.queue.is_empty() {
            state.done = true;
        }
        queue.ready.notify_all();
    }
    shard
}

fn enumerate_directory(
    worker_id: usize,
    task: &Task,
    shard: &mut Shard,
    counters: &Counters,
    filtered: bool,
) -> Vec<Task> {
    counters.directories.fetch_add(1, Ordering::Relaxed);
    let pattern = enumeration_pattern(&task.directory);
    let mut data: FindDataW = unsafe { zeroed() };
    let handle = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FIND_EX_INFO_BASIC,
            &mut data,
            FIND_EX_SEARCH_NAME_MATCH,
            std::ptr::null_mut(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        counters.errors.fetch_add(1, Ordering::Relaxed);
        return Vec::new();
    }

    let mut pending = Vec::new();
    loop {
        let name_slice = nul_terminated(&data.name);
        if !is_dot_entry(name_slice) {
            let is_dir = data.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
            let is_reparse = data.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            let name = String::from_utf16_lossy(name_slice);
            let mut child_path = task.directory.clone();
            child_path.push(OsString::from_wide(name_slice));

            if !(filtered && is_dir && excluded_directory(&child_path)) {
                let parent = task.parent_handle;
                let record = shard.push(worker_id, parent, &name, is_dir);
                counters.records.fetch_add(1, Ordering::Relaxed);
                if is_dir {
                    if is_reparse {
                        counters.reparse_directories.fetch_add(1, Ordering::Relaxed);
                    } else {
                        pending.push(Task {
                            directory: child_path,
                            parent_handle: record,
                        });
                    }
                }
            } else {
                counters
                    .excluded_directories
                    .fetch_add(1, Ordering::Relaxed);
            }
        }

        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            break;
        }
    }
    unsafe { FindClose(handle) };
    pending
}

fn flatten_shards(mut shards: Vec<Shard>) -> Index {
    let mut bases = Vec::with_capacity(shards.len());
    let mut record_total = 0usize;
    let mut names_total = 0usize;
    for shard in &shards {
        bases.push(record_total as u32);
        record_total += shard.records.len();
        names_total += shard.names.len();
    }
    let mut names = Vec::with_capacity(names_total);
    let mut records = Vec::with_capacity(record_total);

    for (shard_id, shard) in shards.iter_mut().enumerate() {
        let name_base = names.len() as u32;
        for source in &shard.records {
            let parent = if source.parent_handle == ROOT_PARENT {
                u32::MAX
            } else {
                let parent_shard = (source.parent_handle >> 32) as usize;
                let local_index = source.parent_handle as u32;
                bases[parent_shard] + local_index
            };
            records.push(IndexRecord {
                parent,
                name_offset: name_base + source.name_offset,
                name_len: source.name_len,
                flags: source.flags,
                _reserved: 0,
            });
        }
        names.append(&mut shard.names);
        let _ = shard_id;
    }
    Index { names, records }
}

fn extended_root(root: &str) -> PathBuf {
    PathBuf::from(format!(r"\\?\{root}"))
}

fn enumeration_pattern(directory: &Path) -> Vec<u16> {
    let mut pattern: Vec<u16> = directory.as_os_str().encode_wide().collect();
    if !pattern.last().is_some_and(|c| *c == b'\\' as u16) {
        pattern.push(b'\\' as u16);
    }
    pattern.push(b'*' as u16);
    pattern.push(0);
    pattern
}

fn nul_terminated(input: &[u16]) -> &[u16] {
    let len = input.iter().position(|c| *c == 0).unwrap_or(input.len());
    &input[..len]
}

fn is_dot_entry(name: &[u16]) -> bool {
    name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16]
}

fn excluded_directory(path: &Path) -> bool {
    let mut full = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(stripped) = full.strip_prefix(r"\\?\") {
        full = stripped.to_string();
    }
    let normalized = format!("\\{}\\", full.trim_matches('\\'));
    let components: Vec<&str> = normalized.split('\\').filter(|c| !c.is_empty()).collect();
    let last = components.last().copied().unwrap_or_default();
    // Большие системные/восстанавливаемые и генерируемые деревья.
    if (components.first() == Some(&"c:") && components.get(1) == Some(&"windows"))
        || components
            .iter()
            .any(|c| matches!(*c, "$recycle.bin" | "system volume information"))
    {
        return true;
    }
    if matches!(last, "windowsapps" | "node_modules" | "target" | ".git") {
        return true;
    }
    if last == "temp" && normalized.contains(r"\appdata\local\temp\") {
        return true;
    }

    // На любом уровне исключаются типичные каталоги браузерных кэшей.
    matches!(
        last,
        "cache"
            | "cache2"
            | "cachestorage"
            | "code cache"
            | "gpucache"
            | "shadercache"
            | "grshadercache"
            | "service worker"
    )
}

fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'ё' { 'е' } else { c })
        .collect()
}

fn private_bytes() -> Option<u64> {
    let mut counters: ProcessMemoryCountersEx = unsafe { zeroed() };
    counters.cb = size_of::<ProcessMemoryCountersEx>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some(counters.private_usage as u64)
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn mib_u64(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
