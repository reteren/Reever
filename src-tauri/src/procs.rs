//! Запущенные программы: найти по названию и закрыть («ривер закрой хром») или
//! завершить как «Снять задачу» («ривер заверши телеграм»). Владелец: воркер `procs`.

use std::os::windows::ffi::OsStrExt;
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    mem::size_of,
    path::{Path, PathBuf},
    ptr, slice,
};

use anyhow::Context;
use strsim::jaro_winkler;
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, BOOL, HWND, LPARAM, WPARAM},
        Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
            },
        },
        UI::WindowsAndMessaging::{
            EnumChildWindows, EnumWindows, GetClassNameW, GetWindow, GetWindowLongW,
            GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
            IsWindowVisible, PostMessageW, GWL_EXSTYLE, GW_OWNER, WM_CLOSE, WS_EX_TOOLWINDOW,
        },
    },
};

use crate::{actions::Action, textnorm, winutil};

/// Программа с видимыми окнами. Группировка — по пути exe, а не по pid: у Chrome,
/// Discord, Telegram десятки процессов, а для пользователя это одна программа.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunningApp {
    /// Полный путь к exe (группирующий ключ).
    pub exe_path: PathBuf,
    /// Имя exe без расширения в нижнем регистре: `chrome`, `telegram`.
    pub exe_name: String,
    /// `FileDescription` из ресурсов версии exe: «Google Chrome»; пусто, если нет.
    pub description: String,
    /// Заголовки видимых окон программы.
    pub titles: Vec<String>,
    /// Все pid процессов с этим exe (для завершения).
    pub pids: Vec<u32>,
    /// Видимые окна верхнего уровня (сырые HWND) — для мягкого закрытия.
    pub windows: Vec<isize>,
}

/// Результат поиска программы по сказанному названию.
#[derive(Clone, Debug, PartialEq)]
pub struct AppMatch {
    pub app: RunningApp,
    /// Похожесть 0..1.
    pub score: f64,
    /// По чему совпало — для лога: «exe», «описание», «заголовок окна», действие.
    pub matched_by: String,
}

#[derive(Clone, Debug)]
struct ProcessInfo {
    pid: u32,
    parent_pid: u32,
    exe_name: String,
    image_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct WindowInfo {
    hwnd: isize,
    title: String,
    pid: u32,
}

/// Программы с видимыми окнами. Окна Проводника оставляем для закрытия, но explorer
/// отдельно защищён от принудительного завершения.
pub fn running_apps() -> Vec<RunningApp> {
    let processes = match snapshot_processes() {
        Ok(processes) => processes,
        Err(_) => return Vec::new(),
    };
    let by_pid: HashMap<u32, &ProcessInfo> = processes.iter().map(|p| (p.pid, p)).collect();
    let process_paths: Vec<(u32, u32, PathBuf)> = processes
        .iter()
        .filter_map(|process| {
            process
                .image_path
                .as_ref()
                .map(|path| (process.pid, process.parent_pid, path.to_path_buf()))
        })
        .collect();
    let mut children_by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in &processes {
        if process.parent_pid != 0 {
            children_by_parent
                .entry(process.parent_pid)
                .or_default()
                .push(process.pid);
        }
    }
    let own_pid = std::process::id();
    let own_path = std::env::current_exe().ok().map(|path| path_key(&path));
    let mut windows: Vec<WindowInfo> = Vec::new();

    unsafe {
        let _ = EnumWindows(
            Some(collect_top_level_window),
            LPARAM(&mut windows as *mut Vec<WindowInfo> as isize),
        );
    }

    let mut grouped: HashMap<String, RunningApp> = HashMap::new();
    let mut roots_by_group: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut visible_windows: Vec<(String, WindowInfo)> = Vec::new();
    for window in windows {
        let Some(owner_process) = by_pid.get(&window.pid).copied() else {
            continue;
        };
        let process = if owner_process.exe_name == "applicationframehost" {
            // UWP рисует содержимое в дочернем CoreWindow. Хостовый exe не является
            // приложением пользователя, поэтому группируем по процессу дочернего окна.
            let Some(core_pid) = find_core_window_process(hwnd_from(window.hwnd)) else {
                continue;
            };
            let Some(process) = by_pid.get(&core_pid).copied() else {
                continue;
            };
            process
        } else {
            owner_process
        };
        if process.pid == own_pid || process.exe_name == "reever" {
            continue;
        }
        let Some(exe_path) = process.image_path.as_ref() else {
            continue;
        };

        let root_pid = root_process_pid(process.pid, &process_paths);
        let Some(root_process) = by_pid.get(&root_pid).copied() else {
            continue;
        };
        let Some(root_path) = root_process.image_path.as_ref() else {
            continue;
        };
        if root_process.pid == own_pid
            || root_process.exe_name == "reever"
            || own_path
                .as_ref()
                .is_some_and(|own| own == &path_key(root_path))
        {
            continue;
        }

        let key = path_key(root_path);
        let app = grouped.entry(key.clone()).or_insert_with(|| RunningApp {
            exe_path: root_path.clone(),
            exe_name: path_exe_name(root_path),
            description: if root_process.exe_name == "explorer" {
                "Проводник".into()
            } else {
                file_description(root_path).unwrap_or_default()
            },
            ..RunningApp::default()
        });
        if app.description.trim().is_empty() {
            app.description = file_description(exe_path).unwrap_or_default();
        }
        roots_by_group
            .entry(key.clone())
            .or_default()
            .insert(root_pid);
        visible_windows.push((
            key,
            WindowInfo {
                pid: process.pid,
                ..window
            },
        ));
    }

    let mut members_by_group: HashMap<String, HashSet<u32>> = HashMap::new();
    for (key, roots) in &roots_by_group {
        let Some(root_path) = grouped.get(key).map(|app| app.exe_path.as_path()) else {
            continue;
        };
        let protected_root = grouped
            .get(key)
            .is_some_and(|app| is_known_protected_name(&app.exe_name));
        let members = members_by_group.entry(key.clone()).or_default();
        let mut pending: Vec<u32> = roots.iter().copied().collect();
        while let Some(pid) = pending.pop() {
            if !members.insert(pid) {
                continue;
            }
            if let Some(children) = children_by_parent.get(&pid) {
                pending.extend(children.iter().copied().filter(|child_pid| {
                    by_pid
                        .get(child_pid)
                        .and_then(|process| process.image_path.as_deref())
                        .is_some_and(|child_path| {
                            if protected_root {
                                path_key(child_path) == path_key(root_path)
                            } else {
                                is_in_installation(root_path, child_path)
                            }
                        })
                }));
            }
        }
    }

    for (key, app) in grouped.iter_mut() {
        if let Some(members) = members_by_group.get(key) {
            app.pids = members.iter().copied().collect();
            app.pids.sort_unstable();
        }
    }
    let group_depth: HashMap<String, usize> = grouped
        .iter()
        .map(|(key, app)| {
            (
                key.clone(),
                app.exe_path
                    .parent()
                    .map(path_key)
                    .unwrap_or_default()
                    .len(),
            )
        })
        .collect();
    for (original_key, window) in visible_windows {
        // Если тот же процесс лежит в дереве нескольких оконных корней, выбираем
        // корень самой установки (путь покороче), а при равенстве — прямой матч.
        let selected_key = members_by_group
            .iter()
            .filter(|(_, members)| members.contains(&window.pid))
            .map(|(key, _)| key)
            .min_by_key(|key| {
                (
                    group_depth.get(*key).copied().unwrap_or(usize::MAX),
                    *key != &original_key,
                )
            })
            .cloned();
        let Some(key) = selected_key else {
            continue;
        };
        let Some(app) = grouped.get_mut(&key) else {
            continue;
        };
        if !app.titles.contains(&window.title) {
            app.titles.push(window.title);
        }
        app.windows.push(window.hwnd);
    }

    let mut apps: Vec<RunningApp> = grouped
        .into_values()
        .filter(|app| !app.windows.is_empty())
        .collect();
    apps.sort_by(|left, right| {
        left.exe_name
            .cmp(&right.exe_name)
            .then_with(|| left.exe_path.cmp(&right.exe_path))
    });
    apps
}

/// Чистое сопоставление без перечисления процессов: сначала учитываются действия,
/// затем имя/описание exe и в последнюю очередь заголовок окна.
pub fn match_app(
    candidates: &[String],
    apps: &[RunningApp],
    actions: &[Action],
    threshold: f64,
) -> Option<AppMatch> {
    if candidates.is_empty() || !threshold.is_finite() {
        return None;
    }

    // Разбираем ярлыки один раз. Внутри самого сопоставления остаётся только сравнение
    // строк с уже известными приложениями и путями.
    let action_targets: Vec<(usize, String, f64)> = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| action.kind.eq_ignore_ascii_case("shell"))
        .filter_map(|(index, action)| {
            let target = resolve_action_exe(&action.target)?;
            let target_key = path_key(&target);
            let phrase_score = action.score(candidates);
            let label_score = text_score(candidates, &[action.label.as_str()]);
            Some((index, target_key, phrase_score.max(label_score)))
        })
        .collect();

    let mut best: Option<(usize, f64, u8, String)> = None;
    for (app_index, app) in apps.iter().enumerate() {
        let app_key = path_key(&app.exe_path);
        for (action_index, target_key, score) in &action_targets {
            if target_key == &app_key {
                let action = &actions[*action_index];
                consider_match(
                    &mut best,
                    app_index,
                    *score,
                    4,
                    format!(
                        "действие «{}»",
                        if action.label.is_empty() {
                            action.id.as_str()
                        } else {
                            action.label.as_str()
                        }
                    ),
                    threshold,
                );
            }
        }

        consider_match(
            &mut best,
            app_index,
            text_score(candidates, &[app.exe_name.as_str()]),
            3,
            "exe".into(),
            threshold,
        );
        consider_match(
            &mut best,
            app_index,
            text_score(candidates, &[app.description.as_str()]),
            3,
            "описание".into(),
            threshold,
        );
        consider_match(
            &mut best,
            app_index,
            text_score(
                candidates,
                &app.titles.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
            1,
            "заголовок окна".into(),
            threshold,
        );
    }

    best.map(|(app_index, score, _, matched_by)| AppMatch {
        app: apps[app_index].clone(),
        score,
        matched_by,
    })
}

/// Мягко закрыть: WM_CLOSE всем окнам, которые ещё существуют.
pub fn close_app(app: &RunningApp) -> anyhow::Result<usize> {
    let mut sent = 0;
    for raw_hwnd in &app.windows {
        let hwnd = hwnd_from(*raw_hwnd);
        unsafe {
            if IsWindow(hwnd).as_bool()
                && PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok()
            {
                sent += 1;
            }
        }
    }
    Ok(sent)
}

/// Завершить только процессы с путём программы и их потомков. Сначала проверяем весь
/// набор на системные процессы, чтобы не оборвать часть дерева перед отказом.
pub fn kill_app(app: &RunningApp) -> anyhow::Result<usize> {
    if is_protected(&app.exe_name) {
        anyhow::bail!(
            "завершение системной программы «{}» запрещено",
            app.exe_name
        );
    }

    let processes = snapshot_processes().context("не удалось получить снимок процессов")?;
    let by_pid: HashMap<u32, &ProcessInfo> = processes.iter().map(|p| (p.pid, p)).collect();
    let wanted_path = path_key(&app.exe_path);
    let mut targets = Vec::new();
    let mut seen = HashSet::new();

    // Проверяем путь повторно: RunningApp мог устареть, а pid — перейти другому exe.
    for pid in &app.pids {
        if let Some(process) = by_pid.get(pid) {
            if process
                .image_path
                .as_ref()
                .is_some_and(|path| path_key(path) == wanted_path)
                && seen.insert(*pid)
            {
                targets.push(*pid);
            }
        }
    }
    if targets.is_empty() {
        anyhow::bail!("процессы {} уже не запущены", app.exe_path.display());
    }

    let mut cursor = 0;
    while cursor < targets.len() {
        let parent = targets[cursor];
        cursor += 1;
        for process in &processes {
            if process.parent_pid == parent && seen.insert(process.pid) {
                targets.push(process.pid);
            }
        }
    }

    for pid in &targets {
        if let Some(process) = by_pid.get(pid) {
            if is_protected(&process.exe_name) {
                anyhow::bail!(
                    "дерево {} содержит защищённый процесс {} (pid {})",
                    app.exe_name,
                    process.exe_name,
                    pid
                );
            }
        }
    }

    let mut terminated = 0;
    let mut failures = Vec::new();
    // Потомков завершаем раньше родителей: это сокращает возможность появления
    // новых дочерних процессов после обхода снимка.
    for pid in targets.iter().rev() {
        let handle = match unsafe { OpenProcess(PROCESS_TERMINATE, BOOL(0), *pid) } {
            Ok(handle) => handle,
            Err(error) => {
                failures.push(format!("pid {pid}: {error}"));
                continue;
            }
        };
        let result = unsafe { TerminateProcess(handle, 1) };
        let _ = unsafe { CloseHandle(handle) };
        match result {
            Ok(()) => terminated += 1,
            Err(error) => failures.push(format!("pid {pid}: {error}")),
        }
    }

    if terminated == 0 {
        anyhow::bail!(
            "не удалось завершить процессы {}: {}",
            app.exe_name,
            if failures.is_empty() {
                "нет доступных процессов".into()
            } else {
                failures.join("; ")
            }
        );
    }
    Ok(terminated)
}

/// Защищённые имена: explorer, winlogon, csrss, smss, services, lsass, svchost,
/// dwm, wininit, fontdrvhost, sihost, taskhostw, system, registry, MsMpEng,
/// audiodg, ctfmon, conhost и Reever. Запрет относится к kill; Explorer всё ещё
/// можно закрыть через WM_CLOSE, чтобы закрыть конкретное окно Проводника.
pub fn is_protected(exe_name: &str) -> bool {
    let name = normalize_exe_name(exe_name);
    if is_known_protected_name(&name) {
        return true;
    }
    std::env::current_exe()
        .ok()
        .as_deref()
        .map(path_exe_name)
        .is_some_and(|current| current == name)
}

const PROTECTED_PROCESS_NAMES: &[&str] = &[
    "explorer",
    "winlogon",
    "csrss",
    "smss",
    "services",
    "lsass",
    "svchost",
    "dwm",
    "wininit",
    "fontdrvhost",
    "sihost",
    "taskhostw",
    "system",
    "registry",
    "msmpeng",
    "audiodg",
    "ctfmon",
    "conhost",
    "reever",
];

fn is_known_protected_name(exe_name: &str) -> bool {
    let name = normalize_exe_name(exe_name);
    PROTECTED_PROCESS_NAMES.contains(&name.as_str())
}

#[derive(Clone, Copy)]
struct Translation {
    language: u16,
    codepage: u16,
}

unsafe extern "system" fn collect_top_level_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    let extended_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if extended_style & WS_EX_TOOLWINDOW.0 != 0 {
        return BOOL(1);
    }
    if GetWindow(hwnd, GW_OWNER)
        .map(|owner| !owner.0.is_null())
        .unwrap_or(false)
    {
        return BOOL(1);
    }

    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        size_of::<u32>() as u32,
    )
    .is_err()
        || cloaked != 0
    {
        return BOOL(1);
    }

    let title = window_title(hwnd);
    if title.is_empty() {
        return BOOL(1);
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return BOOL(1);
    }
    let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);
    windows.push(WindowInfo {
        hwnd: hwnd.0 as isize,
        title,
        pid,
    });
    BOOL(1)
}

unsafe extern "system" fn find_core_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let mut class_name = [0u16; 256];
    let length = GetClassNameW(hwnd, &mut class_name);
    if length <= 0 {
        return BOOL(1);
    }
    let class_name = String::from_utf16_lossy(&class_name[..length as usize]);
    if !class_name.eq_ignore_ascii_case("Windows.UI.Core.CoreWindow") {
        return BOOL(1);
    }

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return BOOL(1);
    }
    *(lparam.0 as *mut Option<u32>) = Some(pid);
    BOOL(0)
}

fn find_core_window_process(hwnd: HWND) -> Option<u32> {
    let mut pid = None;
    unsafe {
        let _ = EnumChildWindows(
            hwnd,
            Some(find_core_window),
            LPARAM(&mut pid as *mut Option<u32> as isize),
        );
    }
    pid
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; (length as usize).saturating_add(1)];
        let copied = GetWindowTextW(hwnd, &mut buffer);
        if copied <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buffer[..copied as usize])
            .trim()
            .to_string()
    }
}

fn snapshot_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .context("CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS) завершился ошибкой")?;
    let mut process_entry = PROCESSENTRY32W::default();
    process_entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut processes = Vec::new();

    unsafe {
        if Process32FirstW(snapshot, &mut process_entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &process_entry.szExeFile[..nul_terminated_len(&process_entry.szExeFile)],
                );
                let image_path = query_process_path(process_entry.th32ProcessID);
                let exe_name = image_path
                    .as_deref()
                    .map(path_exe_name)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| normalize_exe_name(&name));
                processes.push(ProcessInfo {
                    pid: process_entry.th32ProcessID,
                    parent_pid: process_entry.th32ParentProcessID,
                    exe_name,
                    image_path,
                });

                process_entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
                if Process32NextW(snapshot, &mut process_entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(processes)
}

fn query_process_path(pid: u32) -> Option<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL(0), pid).ok()? };
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    if result.is_err() || length == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(
        &buffer[..length as usize],
    )))
}

fn file_description(path: &Path) -> Option<String> {
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path_wide.as_ptr()), None) };
    if size == 0 {
        return None;
    }
    let mut version_info = vec![0u8; size as usize];
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(path_wide.as_ptr()),
            0,
            size,
            version_info.as_mut_ptr() as *mut c_void,
        )
        .ok()?;
    }

    let translations_query: Vec<u16> = "\\VarFileInfo\\Translation\0".encode_utf16().collect();
    let mut translations_ptr = ptr::null_mut();
    let mut translations_len = 0u32;
    let has_translations = unsafe {
        VerQueryValueW(
            version_info.as_ptr() as *const c_void,
            PCWSTR(translations_query.as_ptr()),
            &mut translations_ptr,
            &mut translations_len,
        )
        .as_bool()
    };
    if !has_translations || translations_ptr.is_null() {
        return None;
    }

    let translation_count = (translations_len as usize / size_of::<Translation>()).min(32);
    let translations =
        unsafe { slice::from_raw_parts(translations_ptr as *const Translation, translation_count) };
    for translation in translations {
        let query = format!(
            "\\StringFileInfo\\{:04x}{:04x}\\FileDescription\0",
            translation.language, translation.codepage
        );
        let query_wide: Vec<u16> = query.encode_utf16().collect();
        let mut value_ptr = ptr::null_mut();
        let mut value_len = 0u32;
        let found = unsafe {
            VerQueryValueW(
                version_info.as_ptr() as *const c_void,
                PCWSTR(query_wide.as_ptr()),
                &mut value_ptr,
                &mut value_len,
            )
            .as_bool()
        };
        if !found || value_ptr.is_null() || value_len == 0 {
            continue;
        }
        let value = unsafe { slice::from_raw_parts(value_ptr as *const u16, value_len as usize) };
        let value_len = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        let description = String::from_utf16_lossy(&value[..value_len])
            .trim()
            .to_string();
        if !description.is_empty() {
            return Some(description);
        }
    }
    None
}

fn resolve_action_exe(target: &str) -> Option<PathBuf> {
    let target = target.trim().trim_matches('"');
    if target.is_empty() {
        return None;
    }
    let expanded = winutil::expand_env(target);
    let path = PathBuf::from(expanded);
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
    {
        resolve_shortcut_target(&path)
    } else if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        Some(path)
    } else {
        None
    }
}

fn resolve_shortcut_target(shortcut: &Path) -> Option<PathBuf> {
    let link = lnk::ShellLink::open(shortcut).ok()?;
    if let Some(link_info) = link.link_info() {
        let base = link_info
            .local_base_path_unicode()
            .as_deref()
            .filter(|path| !path.is_empty())
            .or_else(|| {
                link_info
                    .local_base_path()
                    .as_deref()
                    .filter(|path| !path.is_empty())
            });
        if let Some(base) = base {
            let base = winutil::expand_env(base);
            let suffix = link_info
                .common_path_suffix_unicode()
                .as_deref()
                .filter(|path| !path.is_empty())
                .unwrap_or_else(|| link_info.common_path_suffix());
            let suffix = winutil::expand_env(suffix);
            let target = if Path::new(&suffix).is_absolute() {
                PathBuf::from(suffix)
            } else if suffix.is_empty() {
                PathBuf::from(base)
            } else {
                let mut combined = base.trim_end_matches(['\\', '/']).to_string();
                combined.push('\\');
                combined.push_str(suffix.trim_start_matches(['\\', '/']));
                PathBuf::from(combined)
            };
            if target
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            {
                return Some(target);
            }
        }
    }

    let relative = link.relative_path().as_deref()?;
    let parent = shortcut.parent()?;
    let expanded = winutil::expand_env(relative);
    let target = parent.join(expanded);
    target
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        .then_some(target)
}

fn consider_match(
    best: &mut Option<(usize, f64, u8, String)>,
    app_index: usize,
    score: f64,
    priority: u8,
    matched_by: String,
    threshold: f64,
) {
    if score < threshold {
        return;
    }
    let should_replace = best
        .as_ref()
        .map_or(true, |(_, best_score, best_priority, _)| {
            score > *best_score || (score == *best_score && priority > *best_priority)
        });
    if should_replace {
        *best = Some((app_index, score, priority, matched_by));
    }
}

fn text_score(candidates: &[String], values: &[&str]) -> f64 {
    let candidate_forms: Vec<Vec<String>> = candidates.iter().map(|c| match_forms(c)).collect();
    let value_forms: Vec<Vec<String>> = values.iter().map(|v| match_forms(v)).collect();
    let mut best = 0.0f64;
    for candidates in &candidate_forms {
        for candidate in candidates {
            for values in &value_forms {
                for value in values {
                    best = best.max(form_score(candidate, value));
                }
            }
        }
    }
    best
}

fn form_score(candidate: &str, value: &str) -> f64 {
    if candidate.is_empty() || value.is_empty() {
        return 0.0;
    }
    if candidate == value || contains_word_phrase(value, candidate) {
        return 1.0;
    }

    let candidate_words: Vec<&str> = candidate.split_whitespace().collect();
    let value_words: Vec<&str> = value.split_whitespace().collect();
    let mut best = jaro_winkler(candidate, value);
    if candidate_words.len() == 1 {
        for value_word in value_words {
            best = best.max(jaro_winkler(candidate, value_word));
        }
    } else if candidate_words.len() <= value_words.len() {
        for window in value_words.windows(candidate_words.len()) {
            best = best.max(jaro_winkler(candidate, &window.join(" ")));
        }
    }
    best
}

fn contains_word_phrase(haystack: &str, needle: &str) -> bool {
    let haystack: Vec<&str> = haystack.split_whitespace().collect();
    let needle: Vec<&str> = needle.split_whitespace().collect();
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle.as_slice())
}

fn match_forms(value: &str) -> Vec<String> {
    let normalized = textnorm::normalize(value);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut forms = Vec::new();
    for form in textnorm::comparison_forms(&normalized) {
        push_form(&mut forms, &form);
        push_form(&mut forms, &cyrillic_to_latin(&form));
        for alias in app_aliases(&form) {
            push_form(&mut forms, alias);
        }
    }
    forms
}

fn push_form(forms: &mut Vec<String>, form: &str) {
    let form = textnorm::normalize(form);
    if !form.is_empty() && !forms.contains(&form) {
        forms.push(form);
    }
}

fn cyrillic_to_latin(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        result.push_str(match character {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "kh",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' | 'ь' => "",
            'ы' => "y",
            'э' => "e",
            'ю' => "yu",
            'я' => "ya",
            _ => {
                result.push(character);
                continue;
            }
        });
    }
    result
}

fn app_aliases(value: &str) -> &'static [&'static str] {
    match value {
        "хром" | "chrome" => &["хром", "chrome"],
        "телеграм" | "telegram" => &["телеграм", "telegram"],
        "дискорд" | "discord" => &["дискорд", "discord"],
        "блокнот" | "notepad" => &["блокнот", "notepad"],
        "ютуб" | "youtube" => &["ютуб", "youtube"],
        "фотошоп" | "photoshop" => &["фотошоп", "photoshop"],
        "проводник" | "explorer" => &["проводник", "explorer"],
        _ => &[],
    }
}

fn hwnd_from(value: isize) -> HWND {
    HWND(value as *mut c_void)
}

fn path_exe_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .map(normalize_exe_name)
        .unwrap_or_default()
}

fn normalize_exe_name(value: &str) -> String {
    let trimmed = value.trim().trim_matches('"');
    let basename = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    let basename = basename.to_lowercase();
    basename
        .strip_suffix(".exe")
        .unwrap_or(&basename)
        .to_string()
}

/// Поднимается к родителю только если его exe лежит в каталоге-предке текущего
/// exe: так вспомогательные процессы одной установки становятся приложением-корнем,
/// а общие оболочки вроде Explorer не поглощают запущенные ими программы.
fn root_process_pid(pid: u32, processes: &[(u32, u32, PathBuf)]) -> u32 {
    let by_pid: HashMap<u32, &(u32, u32, PathBuf)> = processes
        .iter()
        .map(|process| (process.0, process))
        .collect();
    let mut root_pid = pid;
    let mut visited = HashSet::new();
    visited.insert(root_pid);

    loop {
        let Some((_, parent_pid, child_exe)) = by_pid.get(&root_pid).copied() else {
            break;
        };
        if *parent_pid == 0 || !visited.insert(*parent_pid) {
            break;
        }
        let Some((_, _, parent_exe)) = by_pid.get(parent_pid).copied() else {
            break;
        };
        if !is_installation_parent(parent_exe, child_exe) {
            break;
        }
        root_pid = *parent_pid;
    }

    root_pid
}

fn is_installation_parent(parent_exe: &Path, child_exe: &Path) -> bool {
    if is_known_protected_name(&path_exe_name(parent_exe)) {
        return false;
    }
    let (Some(parent_dir), Some(child_dir)) = (parent_exe.parent(), child_exe.parent()) else {
        return false;
    };
    let parent_dir = path_key(parent_dir);
    let child_dir = path_key(child_dir);
    child_dir
        .strip_prefix(&parent_dir)
        .is_some_and(|suffix| suffix.starts_with('\\') && suffix.len() > 1)
}

fn is_in_installation(root_exe: &Path, child_exe: &Path) -> bool {
    let (Some(root_dir), Some(child_dir)) = (root_exe.parent(), child_exe.parent()) else {
        return false;
    };
    let root_dir = path_key(root_dir);
    let child_dir = path_key(child_dir);
    child_dir == root_dir
        || child_dir
            .strip_prefix(&root_dir)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn nul_terminated_len(value: &[u16]) -> usize {
    value.iter().position(|c| *c == 0).unwrap_or(value.len())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::SystemTime};

    use super::{is_protected, match_app, root_process_pid, RunningApp};
    use crate::actions::Action;

    fn app(exe_name: &str, description: &str, titles: &[&str]) -> RunningApp {
        RunningApp {
            exe_path: PathBuf::from(format!("C:\\Apps\\{exe_name}.exe")),
            exe_name: exe_name.to_lowercase(),
            description: description.into(),
            titles: titles.iter().map(|title| (*title).into()).collect(),
            ..RunningApp::default()
        }
    }

    fn match_name(query: &str, apps: &[RunningApp]) -> Option<super::AppMatch> {
        match_app(&[query.into()], apps, &[], 0.84)
    }

    #[test]
    fn matches_common_russian_and_english_app_names() {
        let apps = vec![
            app("chrome", "Google Chrome", &["New Tab - Google Chrome"]),
            app("Telegram", "Telegram Desktop", &["Telegram"]),
            app("Discord", "Discord", &["Friends - Discord"]),
            app("notepad", "Блокнот", &["Безымянный — Блокнот"]),
        ];
        for query in ["хром", "chrome"] {
            assert_eq!(match_name(query, &apps).unwrap().app.exe_name, "chrome");
        }
        assert_eq!(
            match_name("телеграм", &apps).unwrap().app.exe_name,
            "telegram"
        );
        assert_eq!(
            match_name("дискорд", &apps).unwrap().app.exe_name,
            "discord"
        );
        assert_eq!(
            match_name("блокнот", &apps).unwrap().app.exe_name,
            "notepad"
        );
        assert_eq!(
            match_name("notepad", &apps).unwrap().app.exe_name,
            "notepad"
        );
    }

    #[test]
    fn window_title_matches_youtube_but_exact_exe_wins_ties() {
        let chrome = app("chrome", "Google Chrome", &["YouTube — Google Chrome"]);
        assert_eq!(
            match_name("ютуб", &[chrome]).unwrap().app.exe_name,
            "chrome"
        );

        let chrome = app("chrome", "Google Chrome", &["YouTube — Google Chrome"]);
        let youtube = app("youtube", "YouTube", &["YouTube"]);
        let found = match_name("youtube", &[chrome, youtube]).unwrap();
        assert_eq!(found.app.exe_name, "youtube");
        assert_eq!(found.matched_by, "exe");
    }

    #[test]
    fn unmatched_app_name_returns_none_and_threshold_is_respected() {
        let apps = vec![app("chrome", "Google Chrome", &["New Tab - Google Chrome"])];
        assert!(match_name("unrelated application", &apps).is_none());
        assert!(match_app(&["хром".into()], &apps, &[], 1.01).is_none());
    }

    #[test]
    fn photoshop_shortcut_action_matches_expanded_user_profile_target() {
        let temp_dir = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let shortcut_path = temp_dir.join(format!("reever-procs-{unique}.lnk"));
        write_test_shortcut(&shortcut_path, r"%USERPROFILE%\Programs\", "Photoshop.exe");

        let user_profile = std::env::var("USERPROFILE").unwrap();
        let app = RunningApp {
            exe_path: PathBuf::from(user_profile).join("Programs/Photoshop.exe"),
            exe_name: "photoshop".into(),
            description: "Adobe Creative Tools".into(),
            ..RunningApp::default()
        };
        let action = Action {
            id: "photoshop".into(),
            label: "Photoshop".into(),
            kind: "shell".into(),
            target: shortcut_path.to_string_lossy().into_owned(),
            verbs: Vec::new(),
            phrases: vec!["фотошоп".into()],
        };
        let found = match_app(&["фотошоп".into()], &[app], &[action], 0.84).unwrap();
        assert_eq!(found.matched_by, "действие «Photoshop»");
        assert_eq!(found.score, 1.0);

        let _ = fs::remove_file(shortcut_path);
    }

    #[test]
    fn protected_process_list_is_case_and_extension_insensitive() {
        for process in [
            "explorer.exe",
            "WINLOGON",
            "csrss.exe",
            "smss",
            "services.exe",
            "lsass",
            "svchost.exe",
            "dwm",
            "wininit.exe",
            "fontdrvhost.exe",
            "sihost",
            "taskhostw.exe",
            "System",
            "Registry.exe",
            "MsMpEng.EXE",
            "audiodg.exe",
            "ctfmon",
            "conhost.exe",
            "reever.exe",
        ] {
            assert!(is_protected(process), "{process}");
        }
        assert!(!is_protected("notepad.exe"));
    }

    #[test]
    fn helper_process_resolves_to_ancestor_exe_in_same_installation() {
        let processes = vec![
            (
                10,
                0,
                PathBuf::from(r"C:\Program Files (x86)\Steam\steam.exe"),
            ),
            (
                11,
                10,
                PathBuf::from(r"C:\Program Files (x86)\Steam\bin\steamwebhelper.exe"),
            ),
            (
                12,
                11,
                PathBuf::from(r"C:\Program Files (x86)\Steam\bin\cef\win64\steamwebhelper.exe"),
            ),
        ];

        assert_eq!(root_process_pid(12, &processes), 10);
    }

    #[test]
    fn root_resolution_stops_at_shell_and_directory_boundaries() {
        let processes = vec![
            (20, 0, PathBuf::from(r"C:\Windows\explorer.exe")),
            (21, 20, PathBuf::from(r"C:\Windows\System32\user-app.exe")),
            (30, 0, PathBuf::from(r"C:\Apps\Steam\launcher.exe")),
            (31, 30, PathBuf::from(r"C:\Apps\Steamling\helper.exe")),
        ];

        assert_eq!(root_process_pid(21, &processes), 21);
        assert_eq!(root_process_pid(31, &processes), 31);
    }

    fn write_test_shortcut(path: &std::path::Path, base: &str, suffix: &str) {
        let base = base.as_bytes();
        let suffix = suffix.as_bytes();
        let volume_id_size = 17u32;
        let base_offset = 28 + volume_id_size;
        let suffix_offset = base_offset + base.len() as u32 + 1;
        let link_info_size = suffix_offset as usize + suffix.len() + 1;
        let mut bytes = vec![0u8; 0x4c];
        bytes[0..4].copy_from_slice(&(0x4cu32).to_le_bytes());
        bytes[4..20].copy_from_slice(&[
            0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x46,
        ]);
        bytes[20..24].copy_from_slice(&2u32.to_le_bytes()); // HAS_LINK_INFO
        bytes[60..64].copy_from_slice(&1u32.to_le_bytes()); // ShowNormal
        let info_start = bytes.len();
        bytes.extend_from_slice(&(link_info_size as u32).to_le_bytes());
        bytes.extend_from_slice(&28u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
        bytes.extend_from_slice(&28u32.to_le_bytes());
        bytes.extend_from_slice(&base_offset.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&suffix_offset.to_le_bytes());
        bytes.extend_from_slice(&volume_id_size.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes()); // DriveFixed
        bytes.extend_from_slice(&0u32.to_le_bytes()); // serial number
        bytes.extend_from_slice(&16u32.to_le_bytes()); // label offset
        bytes.push(0);
        bytes.extend_from_slice(base);
        bytes.push(0);
        bytes.extend_from_slice(suffix);
        bytes.push(0);
        debug_assert_eq!(bytes.len() - info_start, link_info_size);
        bytes.extend_from_slice(&0u32.to_le_bytes()); // terminal ExtraData block
        fs::write(path, bytes).expect("записать тестовый ярлык");
    }
}
