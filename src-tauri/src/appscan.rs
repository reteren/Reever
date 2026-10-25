//! Сканер программ для вкладки «Действия»: новый пользователь нажимает «Сканировать» и
//! получает список своих программ с готовыми фразами вместо того, чтобы заводить каждое
//! действие руками.
//!
//! «Нормальные программы» — то, что установщики сами кладут в меню «Пуск» (Get-StartApps,
//! вместе с приложениями Store и играми Steam), а не каждый exe на диске. Из него убираются
//! удаление, справка, ссылки на сайты, документы и папки, дубли версий; системные утилиты
//! и программы с общим названием («Камера», «Paint») остаются в списке, но без галочки.
//!
//! Фразы: название без версии и мусора («DaVinci Resolve 19» → «davinci resolve»), короткие
//! формы («davinci», «resolve davinci») и, если среди языков распознавания есть кириллический,
//! то же кириллицей — по словарю произношений, а для незнакомых слов по правилам английского
//! чтения («давинчи резолв»).
//!
//! Порядок: сверху часто используемые, затем запускавшиеся на этой неделе, затем остальные
//! по алфавиту — по истории запусков, которую ведёт сама Windows (UserAssist, `order_by_usage`).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::actions::Action;
use crate::apps::AppEntry;

/// Почему программа в списке без галочки (или недоступна).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Note {
    /// Уже есть действие для этой программы.
    Exists,
    /// Название — обычное слово, фраза будет срабатывать в речи.
    Generic,
    /// Служебная утилита Windows или инструмент разработчика.
    System,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScannedApp {
    /// AppID из меню «Пуск» — по нему же берётся иконка.
    pub id: String,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub phrases: Vec<String>,
    pub checked: bool,
    pub note: Option<Note>,
    /// Раздел списка по тому, как часто программой пользуются (`order_by_usage`).
    pub group: Group,
    /// Сколько минут программа была в фокусе и когда запускалась (Unix, секунды) — для подсказки.
    pub focus_minutes: u32,
    pub last_used: Option<u64>,
}

/// Раздел списка сканера: часто используемые сверху, затем недавние, затем по алфавиту.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Frequent,
    Recent,
    Other,
}

/// Выбранная в списке программа — из неё делается действие.
#[derive(Clone, Debug, Deserialize)]
pub struct ChosenApp {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub phrases: Vec<String>,
}

/// `languages` — включённые языки распознавания, `reserved` — фразы команд и wake-слова:
/// их программе не даём, иначе «ривер поиск» открывал бы программу «Поиск».
pub fn scan(entries: Vec<AppEntry>, languages: &[&str], actions: &[Action], reserved: &[String]) -> Vec<ScannedApp> {
    let cyrillic = languages.iter().any(|code| matches!(*code, "ru" | "uk" | "bg"));
    let reserved: HashSet<String> = reserved.iter().map(|phrase| phrase.trim().to_lowercase()).collect();
    let existing = Existing::new(actions);

    // Одна программа — одна строка: из версий («FL Studio 2024» и «2025») берётся новейшая.
    let mut best: HashMap<String, Candidate> = HashMap::new();
    for entry in entries {
        if is_junk(&entry.name, &entry.aumid) {
            continue;
        }
        let candidate = Candidate::new(entry);
        let key = candidate.core.join(" ");
        if key.is_empty() {
            continue;
        }
        match best.get(&key) {
            Some(current) if current.rank() >= candidate.rank() => {}
            _ => {
                best.insert(key, candidate);
            }
        }
    }
    let mut candidates: Vec<Candidate> = best.into_values().collect();
    candidates.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));

    let mut apps: Vec<ScannedApp> = candidates
        .into_iter()
        .map(|candidate| {
            let mut phrases: Vec<String> = generate_phrases(&candidate.words, &candidate.core, cyrillic)
                .into_iter()
                .filter(|phrase| !reserved.contains(phrase))
                .collect();
            phrases.truncate(MAX_PHRASES);
            let note = if existing.covers(&candidate, &phrases) {
                Some(Note::Exists)
            } else if is_system(&candidate.entry.name, &candidate.entry.aumid) {
                Some(Note::System)
            } else if candidate.core.iter().all(|word| GENERIC.contains(&word.as_str())) {
                Some(Note::Generic)
            } else {
                None
            };
            let (kind, target) = target_of(&candidate.entry.aumid);
            ScannedApp {
                id: candidate.entry.aumid.clone(),
                name: candidate.display.clone(),
                kind,
                target,
                checked: note.is_none() && !phrases.is_empty(),
                note,
                phrases,
                group: Group::Other,
                focus_minutes: 0,
                last_used: None,
            }
        })
        .collect();
    drop_shared_phrases(&mut apps);
    apps
}

// ── Как часто пользуются ─────────────────────────────────────────────────

/// Что Windows помнит о запусках программы (UserAssist).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AppUsage {
    pub runs: u32,
    pub focus_count: u32,
    pub focus_ms: u32,
    /// Последний запуск, Unix-время в секундах.
    pub last: Option<u64>,
}

/// Недавним считается запуск не старше этого.
const RECENT_DAYS: f64 = 7.0;

/// Часто: долго в фокусе или открывали несколько раз. Счётчик запусков в Windows 10/11
/// почти всегда 0 — опора на фокус, он ведётся.
fn is_frequent(usage: &AppUsage) -> bool {
    usage.focus_ms >= 5 * 60_000 || usage.focus_count >= 3 || usage.runs >= 3
}

/// Ранг внутри «часто»: время в фокусе, число открытий, свежесть.
fn usage_score(usage: &AppUsage, now: u64) -> f64 {
    let minutes = f64::from(usage.focus_ms) / 60_000.0;
    let recency = usage
        .last
        .map(|last| 1.0 / (1.0 + now.saturating_sub(last) as f64 / 86_400.0 / RECENT_DAYS))
        .unwrap_or(0.0);
    minutes.ln_1p() + 0.6 * f64::from(usage.focus_count).ln_1p() + 0.6 * f64::from(usage.runs).ln_1p() + recency
}

/// Разложить список по разделам: часто используемые (по `usage_score`), недавние (свежие
/// выше), остальные — по алфавиту, как пришли из `scan`. `usage` — по AppID в нижнем регистре.
pub fn order_by_usage(apps: &mut [ScannedApp], usage: &HashMap<String, AppUsage>, now: u64) {
    let mut scores = HashMap::new();
    for app in apps.iter_mut() {
        let Some(found) = usage.get(&app.id.to_lowercase()) else { continue };
        app.focus_minutes = found.focus_ms / 60_000;
        app.last_used = found.last;
        let recent = found
            .last
            .is_some_and(|last| (now.saturating_sub(last) as f64) < RECENT_DAYS * 86_400.0);
        app.group = if is_frequent(found) {
            Group::Frequent
        } else if recent {
            Group::Recent
        } else {
            Group::Other
        };
        scores.insert(app.id.clone(), usage_score(found, now));
    }
    // Сортировка устойчивая: внутри «остальных» сохраняется алфавит.
    apps.sort_by(|a, b| {
        a.group.cmp(&b.group).then_with(|| match a.group {
            Group::Frequent => scores[&b.id].total_cmp(&scores[&a.id]),
            Group::Recent => b.last_used.cmp(&a.last_used),
            Group::Other => std::cmp::Ordering::Equal,
        })
    });
}

/// UserAssist: Windows ведёт его для каждого пользователя — что запускали из «Пуска»,
/// Проводника и панели задач. Имена записей — AppID меню «Пуск» (или путь с GUID известной
/// папки) в ROT13, данные — 72 байта: запуски (4), фокус: раз (8) и мс (12), последний
/// запуск FILETIME (60). Без прав администратора; данных нет (Windows не ведёт историю
/// запусков) — пустой ответ, список остаётся по алфавиту.
pub fn user_assist() -> HashMap<String, AppUsage> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };
    const ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist";
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let mut out: HashMap<String, AppUsage> = HashMap::new();
    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(wide(ROOT).as_ptr()), 0, KEY_READ, &mut root) != ERROR_SUCCESS {
            return out;
        }
        let mut guid = [0u16; 256];
        for index in 0.. {
            let mut len = guid.len() as u32;
            if RegEnumKeyExW(root, index, PWSTR(guid.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None)
                != ERROR_SUCCESS
            {
                break;
            }
            let path = format!(r"{}\Count", String::from_utf16_lossy(&guid[..len as usize]));
            let mut count = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(wide(&path).as_ptr()), 0, KEY_READ, &mut count) != ERROR_SUCCESS {
                continue;
            }
            let mut name = vec![0u16; 2048];
            let mut data = [0u8; 128];
            for value in 0.. {
                let mut name_len = name.len() as u32;
                let mut data_len = data.len() as u32;
                let status = RegEnumValueW(
                    count,
                    value,
                    PWSTR(name.as_mut_ptr()),
                    &mut name_len,
                    None,
                    None,
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                );
                // Чужое длинное значение — пропустить его, а не бросать раздел.
                if status == ERROR_MORE_DATA {
                    continue;
                }
                if status != ERROR_SUCCESS {
                    break;
                }
                let Some(usage) = parse_user_assist(&data[..data_len as usize]) else { continue };
                let key = rot13(&String::from_utf16_lossy(&name[..name_len as usize])).to_lowercase();
                // Одна программа бывает в двух разделах (exe и ярлык) — берём больший фокус.
                let slot = out.entry(key).or_default();
                let last = slot.last.max(usage.last);
                if usage.focus_ms >= slot.focus_ms {
                    *slot = usage;
                }
                slot.last = last;
            }
            let _ = RegCloseKey(count);
        }
        let _ = RegCloseKey(root);
    }
    out
}

fn parse_user_assist(data: &[u8]) -> Option<AppUsage> {
    if data.len() < 68 {
        return None;
    }
    let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    let filetime = u64::from_le_bytes(data[60..68].try_into().unwrap());
    // FILETIME — сотни наносекунд с 1601 года; до 1970 (ноль и мусор) — даты нет.
    const UNIX_EPOCH_SECS_SINCE_1601: u64 = 11_644_473_600;
    let last = (filetime / 10_000_000)
        .checked_sub(UNIX_EPOCH_SECS_SINCE_1601)
        .filter(|&secs| secs > 0);
    Some(AppUsage { runs: u32_at(4), focus_count: u32_at(8), focus_ms: u32_at(12), last })
}

fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            _ => c,
        })
        .collect()
}

/// Выбранные программы → новые действия. Программу, для которой действие уже есть
/// (тот же target), второй раз не добавляем.
pub fn to_actions(chosen: Vec<ChosenApp>, actions: &[Action], stamp: u128) -> Vec<Action> {
    let mut taken: HashSet<String> = actions.iter().map(|action| action.target.to_lowercase()).collect();
    let mut ids: HashSet<String> = actions.iter().map(|action| action.id.clone()).collect();
    let mut out = Vec::new();
    for (index, app) in chosen.into_iter().enumerate() {
        let phrases: Vec<String> = dedup(app.phrases.iter().map(|phrase| phrase.trim().to_lowercase()).filter(|phrase| !phrase.is_empty()));
        if app.target.trim().is_empty() || phrases.is_empty() || !taken.insert(app.target.to_lowercase()) {
            continue;
        }
        let mut id = format!("app_{:x}{index:x}", stamp);
        while !ids.insert(id.clone()) {
            id.push('x');
        }
        out.push(Action {
            id,
            label: app.name,
            kind: app.kind,
            target: app.target,
            verbs: Vec::new(),
            phrases,
        });
    }
    out
}

const MAX_PHRASES: usize = 6;

struct Candidate {
    entry: AppEntry,
    /// Название для списка и подпись действия: без версии и разрядности.
    display: String,
    /// Слова названия в нижнем регистре.
    words: Vec<String>,
    /// Слова без производителя и мусора («launcher», «desktop») — главная фраза.
    core: Vec<String>,
}

impl Candidate {
    fn new(entry: AppEntry) -> Self {
        let display = display_name(&entry.name);
        let words = words_of(&display);
        let core = core_words(&words);
        Self { entry, display, words, core }
    }

    /// Какую из одноимённых записей оставить: 64-битную, потом новейшую по названию и пути.
    fn rank(&self) -> (bool, String, String) {
        let name = self.entry.name.to_lowercase();
        let narrow = ["x86", "32-bit", "32bit", "(wow)"].iter().any(|mark| name.contains(mark));
        (!narrow, name, self.entry.aumid.to_lowercase())
    }
}

/// Ярлыки, которые не программы: удаление, справка, сайты, документы, папки.
fn is_junk(name: &str, app_id: &str) -> bool {
    const NAME_MARKS: &[&str] = &[
        "uninstall", "uninst", "деинсталл", "удалить", "удаление", "help", "справка", "manual", "documentation",
        "руководство", "readme", "read me", "release notes", "what's new", "whats new", "новости в последней",
        "website", "web site", "faq", "user guide", "quick guide", "troubleshooting", "report a problem",
        "error reporter", "crash reporter", "bug report", "updater", "телеметр", "private browsing",
        "приватн", "compatibility mode", "support center", "version history", "revision history",
        "learn more", "узнать больше", "more...", "example scripts", "sample", "license", "лиценз",
        "module docs", "online docs", "install additional tools",
    ];
    const DOCUMENTS: &[&str] = &[
        "chm", "pdf", "txt", "htm", "html", "url", "md", "rtf", "ico", "doc", "docx", "xml", "ini", "log", "hlp",
    ];
    const EXE_MARKS: &[&str] = &["unins", "uninst", "updater", "errorreporter", "bugreporter", "crashreporter", "crashpad"];

    let name = name.to_lowercase();
    let id = app_id.trim().to_lowercase();
    if name.trim().is_empty() || id.is_empty() || id == "com.troid.reever" {
        return true;
    }
    if NAME_MARKS.iter().any(|mark| name.contains(mark)) {
        return true;
    }
    if let Some((scheme, _)) = id.split_once("://") {
        // steam://, com.epicgames.launcher:// и т. п. — игры; веб-ссылки — сайты.
        return matches!(scheme, "http" | "https" | "ftp" | "mailto" | "file");
    }
    if is_path(&id) {
        let file = id.rsplit('\\').next().unwrap_or_default();
        let Some((stem, extension)) = file.rsplit_once('.') else {
            return true; // папка
        };
        return DOCUMENTS.contains(&extension) || EXE_MARKS.iter().any(|mark| stem.contains(mark));
    }
    false
}

/// Путь к файлу: `C:\…` или `{GUID известной папки}\…`.
fn is_path(app_id: &str) -> bool {
    let bytes = app_id.as_bytes();
    (bytes.len() > 2 && bytes[1] == b':' && bytes[2] == b'\\') || (app_id.starts_with('{') && app_id.contains("}\\"))
}

/// Служебное Windows и инструменты разработчика — показываем, но без галочки.
fn is_system(name: &str, app_id: &str) -> bool {
    const SYSTEM_FOLDERS: &[&str] = &[
        "{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}", // System32
        "{d65231b0-b2f1-4857-a4ce-a8e7c6ea7d27}", // SysWOW64
        "{f38bf404-1d43-42f2-9305-67de0b28fc23}", // Windows
    ];
    const SYSTEM_IDS: &[&str] = &["microsoft.windows.", "windows.immersivecontrolpanel", "microsoftwindows.client", "microsoft.office.setlang"];
    const SYSTEM_NAMES: &[&str] = &[
        "command prompt", "developer powershell", "odbc", "native tools", "cross tools", "app cert kit",
        "software development kit", "application verifier", "package manager", "client configuration",
        "task manager", "диспетчер задач", "security policy", "политика безопасности", "resource monitor",
        "монитор ресурсов", "task scheduler", "планировщик задач", "event viewer", "просмотр событий",
        "performance monitor", "системный монитор", "computer management", "управление компьютером", "app recovery",
    ];
    let id = app_id.to_lowercase();
    let name = name.to_lowercase();
    SYSTEM_FOLDERS.iter().any(|folder| id.starts_with(folder))
        // Microsoft.Windows.Photos_… — Store-приложение, а не служебное: у пакетов есть «_».
        || SYSTEM_IDS.iter().any(|prefix| id.starts_with(prefix) && !(*prefix == "microsoft.windows." && id.contains('_')))
        || SYSTEM_NAMES.iter().any(|mark| name.contains(mark))
}

/// Как запускать: ссылки (steam://…) и полные пути — напрямую, остальное — через
/// shell:AppsFolder, как сам «Пуск» (Store-приложения, ярлыки с аргументами).
fn target_of(app_id: &str) -> (String, String) {
    let bytes = app_id.as_bytes();
    let absolute = bytes.len() > 2 && bytes[1] == b':' && bytes[2] == b'\\';
    if app_id.contains("://") || absolute {
        ("shell".into(), app_id.into())
    } else {
        ("uwp".into(), app_id.into())
    }
}

/// Уже заведённые действия: по цели (AppID, путь, имя exe), подписи и фразам.
struct Existing {
    targets: HashSet<String>,
    files: HashSet<String>,
    labels: HashSet<String>,
    phrases: HashSet<String>,
}

impl Existing {
    fn new(actions: &[Action]) -> Self {
        let mut existing = Self { targets: HashSet::new(), files: HashSet::new(), labels: HashSet::new(), phrases: HashSet::new() };
        for action in actions.iter().filter(|action| action.kind != "system") {
            let target = action.target.trim().to_lowercase();
            if let Some(file) = exe_name(&target) {
                existing.files.insert(file);
            }
            existing.targets.insert(target);
            existing.labels.insert(display_name(&action.label).to_lowercase());
            existing.phrases.extend(action.phrases.iter().map(|phrase| phrase.trim().to_lowercase()));
        }
        existing
    }

    fn covers(&self, candidate: &Candidate, phrases: &[String]) -> bool {
        let id = candidate.entry.aumid.to_lowercase();
        self.targets.contains(&id)
            || exe_name(&id).is_some_and(|file| self.files.contains(&file))
            || self.labels.contains(&candidate.display.to_lowercase())
            || phrases.first().is_some_and(|phrase| self.phrases.contains(phrase))
    }
}

fn exe_name(target: &str) -> Option<String> {
    let file = target.rsplit(['\\', '/']).next()?.to_lowercase();
    file.ends_with(".exe").then_some(file)
}

/// Фраза, которую сгенерировали двум программам («unity» у Unity и Unity Hub), остаётся
/// только той, чьё это главное название, — иначе одна фраза открывала бы что попало.
fn drop_shared_phrases(apps: &mut [ScannedApp]) {
    let mut owners: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, app) in apps.iter().enumerate() {
        for phrase in &app.phrases {
            owners.entry(phrase.clone()).or_default().push(index);
        }
    }
    for (phrase, indexes) in owners.into_iter().filter(|(_, indexes)| indexes.len() > 1) {
        let words = |index: usize| apps[index].phrases.first().map_or(usize::MAX, |main| main.split_whitespace().count());
        let shortest = indexes.iter().map(|&index| words(index)).min().unwrap_or(usize::MAX);
        let owners: Vec<usize> = indexes.iter().copied().filter(|&index| words(index) == shortest).collect();
        let main = (owners.len() == 1).then(|| owners[0]);
        for index in indexes {
            if Some(index) != main {
                apps[index].phrases.retain(|own| *own != phrase);
            }
        }
    }
    for app in apps.iter_mut().filter(|app| app.phrases.is_empty()) {
        app.checked = false;
    }
}

// ── Названия и фразы ─────────────────────────────────────────────────────

/// Слова, которые в названии ничего не различают: без них фраза короче и её так и говорят.
const NOISE: &[&str] = &[
    "beta", "preview", "alpha", "insider", "insiders", "portable", "edition", "launcher", "desktop", "app",
    "browser", "браузер", "client", "for", "windows", "workplace", "and", "the", "steam", "demo", "playtest",
];
/// Производитель перед названием («Adobe Photoshop» → «photoshop»).
const VENDORS: &[&str] = &[
    "microsoft", "adobe", "google", "mozilla", "autodesk", "jetbrains", "corel", "apple", "amazon",
];
/// Общие слова: программа только из них — без галочки, первым словом фразу не делаем.
const GENERIC: &[&str] = &[
    "calculator", "camera", "clock", "alarms", "notepad", "paint", "photos", "weather", "terminal", "editor",
    "dashboard", "bridge", "project", "word", "publisher", "settings", "store", "news", "maps", "mail", "music",
    "tips", "player", "media", "sound", "recorder", "game", "bar", "help", "get", "hub", "feedback", "phone",
    "link", "quick", "assist", "sticky", "notes", "snipping", "tool", "tools", "file", "converter", "power",
    "automate", "to", "do", "manager", "control", "panel", "visual", "studio", "home", "video", "audio",
    "people", "movies", "tv", "solitaire", "casual", "games", "security", "console", "remote", "monitor",
    "speed", "test", "configuration", "multi", "instance", "services", "notifications", "proxy", "generator",
    "click", "button", "simple", "hard", "lost", "color", "command", "upload", "legends", "new", "cloud",
    "plugins", "compare", "utility", "legacy", "kit", "software", "development",
    "настройки", "камера", "часы", "калькулятор", "блокнот", "проводник", "выполнить", "справка", "почта",
    "погода", "фотографии", "музыка", "кино",
];

/// Название без версии, разрядности и скобок: «OBS Studio (64bit)» → «OBS Studio».
fn display_name(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '®' | '™' | '©' => {}
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let kept: Vec<&str> = out.split_whitespace().filter(|word| !is_version(word)).collect();
    if kept.is_empty() {
        name.trim().to_string()
    } else {
        kept.join(" ")
    }
}

/// «19», «2025», «22.0», «v1.2», «1.5a», «x64», «64-bit».
fn is_version(word: &str) -> bool {
    let lower = word.to_lowercase();
    if matches!(lower.as_str(), "x64" | "x86" | "64bit" | "32bit" | "64-bit" | "32-bit" | "arm64") {
        return true;
    }
    let rest = lower.strip_prefix('v').unwrap_or(&lower);
    let mut chars = rest.chars().peekable();
    if !chars.peek().is_some_and(char::is_ascii_digit) {
        return false;
    }
    let body: String = rest.trim_end_matches(|c: char| c.is_ascii_alphabetic()).to_string();
    let suffix = rest.len() - body.len();
    suffix <= 2 && !body.is_empty() && body.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
}

/// Слова названия для фраз: нижний регистр, без апострофов, разделители — пробелы.
fn words_of(display: &str) -> Vec<String> {
    let lower = display.to_lowercase().replace(['\'', '’', '`'], "");
    lower
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.' | '/' | '+' | ':' | ',' | '!' | '|'))
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_string)
        .collect()
}

fn core_words(words: &[String]) -> Vec<String> {
    let mut core: Vec<String> = words.iter().filter(|word| !NOISE.contains(&word.as_str())).cloned().collect();
    if core.is_empty() {
        core = words.to_vec();
    }
    if core.len() > 1 && VENDORS.contains(&core[0].as_str()) {
        core.remove(0);
    }
    core
}

/// Фразы программы: главная (core) первой — её же ищет `drop_shared_phrases`.
fn generate_phrases(words: &[String], core: &[String], cyrillic: bool) -> Vec<String> {
    if let Some(spoken) = whole_name(&core.join(" ")) {
        let mut phrases: Vec<String> = spoken.latin.iter().map(|phrase| phrase.to_string()).collect();
        if cyrillic {
            phrases.extend(spoken.cyrillic.iter().map(|phrase| phrase.to_string()));
        }
        return dedup(phrases.into_iter());
    }
    // Порядок — по важности: лишнее отрежет MAX_PHRASES. Главное и короткое название,
    // обратный порядок слов, они же кириллицей, и только потом полное название.
    // Только латиница: «диспетчер» из «Диспетчер задач» или «задач диспетчер» так не говорят.
    // Короткое слово отдельно легко совпадает с речью — «dome», «lost».
    let distinctive = |word: &String| {
        word.chars().count() >= 5
            && word.chars().all(|c| c.is_ascii_alphabetic())
            && !GENERIC.contains(&word.as_str())
            && !VENDORS.contains(&word.as_str())
    };
    let mut spoken: Vec<Vec<String>> = vec![core.to_vec()];
    if core.len() >= 2 && distinctive(&core[0]) {
        spoken.push(vec![core[0].clone()]);
    }
    let reversed = (core.len() == 2 && core.iter().all(distinctive)).then(|| vec![core[1].clone(), core[0].clone()]);
    let to_cyrillic = |phrase: &Vec<String>| {
        phrase.iter().any(|word| word.chars().any(|c| c.is_ascii_alphabetic())).then(|| phrase.iter().map(|word| cyrillic_word(word)).collect::<Vec<_>>())
    };

    let mut phrases: Vec<Vec<String>> = spoken.clone();
    phrases.extend(reversed);
    if cyrillic {
        phrases.extend(spoken.iter().filter_map(to_cyrillic));
    }
    phrases.push(words.to_vec());
    if cyrillic {
        phrases.extend(to_cyrillic(&words.to_vec()));
    }
    dedup(phrases.into_iter().map(|phrase| phrase.join(" ")).filter(|phrase| phrase.chars().filter(|c| c.is_alphanumeric()).count() >= 3))
}

fn dedup(phrases: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    phrases
        .map(|phrase| phrase.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|phrase| !phrase.is_empty() && seen.insert(phrase.clone()))
        .collect()
}

struct Spoken {
    latin: &'static [&'static str],
    cyrillic: &'static [&'static str],
}

/// Названия, которые не собрать из слов: цифры, сокращения, устоявшиеся русские имена.
fn whole_name(core: &str) -> Option<Spoken> {
    let (latin, cyrillic): (&[&str], &[&str]) = match core {
        "7 zip file manager" | "7 zip" => (&["7-zip", "seven zip"], &["севен зип", "семь зип"]),
        "s&box" => (&["s&box", "sandbox"], &["сэндбокс", "эс бокс"]),
        "paint net" => (&["paint net", "paint dot net"], &["пейнт нет", "пейнт дот нет"]),
        "league of legends" => (&["league of legends"], &["лига легенд", "лиг оф леджендс"]),
        "garrys mod" => (&["garrys mod", "gmod"], &["гаррис мод", "гмод"]),
        "obs studio" => (&["obs studio", "obs"], &["обс студио", "обс"]),
        "mp3tag" => (&["mp3tag"], &["эм пи три тег", "мп3 тег"]),
        "chatgpt" => (&["chatgpt", "chat gpt"], &["чат джи пи ти", "чатгпт"]),
        "visual studio code" => (&["visual studio code", "vs code", "vscode"], &["вижуал студио код", "вс код", "ви эс код"]),
        "edge" => (&["edge", "microsoft edge"], &["эдж", "майкрософт эдж"]),
        "dota" => (&["dota", "dota 2"], &["дота", "дота два"]),
        "counter strike" => (&["counter strike", "cs"], &["контр страйк", "кс", "каунтер страйк"]),
        "fl studio" => (&["fl studio", "fruity loops"], &["фл студио", "эф эл студио", "фрути лупс"]),
        "ldplayer" => (&["ldplayer", "ld player"], &["эл ди плеер", "лд плеер"]),
        "ea" => (&["ea app", "electronic arts"], &["иа апп", "электроник артс"]),
        "qbittorrent" => (&["qbittorrent", "torrent"], &["кьюбиторрент", "торрент"]),
        "capcut" => (&["capcut", "cap cut"], &["капкут"]),
        "whatsapp" => (&["whatsapp", "whats app"], &["ватсап", "вотсап"]),
        "tmodloader" => (&["tmodloader", "t mod loader"], &["ти мод лоадер", "тмодлоадер"]),
        "minecraft" => (&["minecraft"], &["майнкрафт"]),
        "photoshop" => (&["photoshop"], &["фотошоп"]),
        "after effects" => (&["after effects"], &["афтер эффектс", "афтер эффект", "афтер"]),
        "premiere pro" => (&["premiere pro", "premiere"], &["премьер про", "премьер"]),
        _ => return None,
    };
    Some(Spoken { latin, cyrillic })
}

/// Как русскоязычный произносит слово из названия программы.
fn cyrillic_word(word: &str) -> String {
    if !word.chars().any(|c| c.is_ascii_alphabetic()) {
        return word.to_string();
    }
    if let Some((_, spoken)) = SPOKEN_WORDS.iter().find(|(latin, _)| *latin == word) {
        return (*spoken).to_string();
    }
    // Короткое без гласных — аббревиатура, читается по буквам: «vlc» → «вэ эл си».
    let letters: Vec<char> = word.chars().collect();
    if letters.len() <= 4 && letters.iter().all(|c| c.is_ascii_alphabetic() && !"aeiouy".contains(*c)) {
        return letters.iter().map(|c| letter_name(*c)).collect::<Vec<_>>().join(" ");
    }
    crate::textnorm::transliterate_word(&english_reading(word))
}

/// Английские сочетания, которые побуквенная транслитерация читает неверно.
fn english_reading(word: &str) -> String {
    let mut out = word.to_string();
    for (from, to) in [
        ("tion", "шн"), ("igh", "ай"), ("iew", "ью"), ("ay", "ей"), ("oy", "ой"), ("ey", "ей"), ("ai", "эй"),
        ("aw", "о"), ("ou", "ау"), ("wh", "в"), ("qu", "кв"), ("sch", "ск"),
    ] {
        out = out.replace(from, to);
    }
    if let Some(stem) = out.strip_suffix("ew") {
        out = format!("{stem}ью");
    }
    // «y» между согласными и в конце — гласная: «msys» → «мсис», «legacy» → «легаси».
    // Латинская «i», а не «и»: так «c» перед ней читается как «с».
    let chars: Vec<char> = out.chars().collect();
    let vowel = |c: Option<&char>| c.is_some_and(|c| "aeiouаеиоуыэюяё".contains(*c));
    out = chars
        .iter()
        .enumerate()
        .map(|(index, &c)| {
            let consonant_around = !vowel(index.checked_sub(1).and_then(|before| chars.get(before))) && !vowel(chars.get(index + 1));
            if c == 'y' && index > 0 && consonant_around { 'i' } else { c }
        })
        .collect();
    // «e» в начале слова — «э»: «editor» → «эдитор», а не «едитор».
    if let Some(rest) = out.strip_prefix('e') {
        out = format!("э{rest}");
    }
    out
}

fn letter_name(c: char) -> &'static str {
    match c {
        'a' => "эй", 'b' => "би", 'c' => "си", 'd' => "ди", 'e' => "и", 'f' => "эф", 'g' => "джи", 'h' => "эйч",
        'i' => "ай", 'j' => "джей", 'k' => "кей", 'l' => "эл", 'm' => "эм", 'n' => "эн", 'o' => "оу", 'p' => "пи",
        'q' => "кью", 'r' => "ар", 's' => "эс", 't' => "ти", 'u' => "ю", 'v' => "ви", 'w' => "дабл ю", 'x' => "икс",
        'y' => "уай", 'z' => "зед", _ => "",
    }
}

/// Произношение частых слов в названиях программ: по буквам «resolve» — «ресолв»,
/// а говорят «резолв».
const SPOKEN_WORDS: &[(&str, &str)] = &[
    ("davinci", "давинчи"), ("resolve", "резолв"), ("discord", "дискорд"), ("spotify", "спотифай"),
    ("blender", "блендер"), ("opera", "опера"), ("gx", "джи икс"), ("brave", "брейв"), ("orca", "орка"),
    ("telegram", "телеграм"), ("chrome", "хром"), ("google", "гугл"), ("firefox", "файрфокс"), ("edge", "эдж"),
    ("steam", "стим"), ("illustrator", "иллюстратор"), ("premiere", "премьер"), ("audition", "аудишн"),
    ("lightroom", "лайтрум"), ("adobe", "адоб"), ("substance", "сабстенс"), ("painter", "пейнтер"),
    ("designer", "дизайнер"), ("sampler", "сэмплер"), ("excel", "эксель"), ("powerpoint", "пауэрпоинт"),
    ("outlook", "аутлук"), ("teams", "тимс"), ("onenote", "ваннот"), ("onedrive", "ван драйв"), ("word", "ворд"),
    ("visio", "визио"), ("access", "аксесс"), ("copilot", "копайлот"), ("clipchamp", "клипчамп"), ("visual", "вижуал"),
    ("studio", "студио"), ("code", "код"), ("notepad", "ноутпад"), ("paint", "пейнт"), ("vegas", "вегас"),
    ("pro", "про"), ("unity", "юнити"), ("hub", "хаб"), ("unreal", "анриал"), ("engine", "энджин"), ("epic", "эпик"),
    ("games", "геймс"), ("game", "гейм"), ("roblox", "роблокс"), ("player", "плеер"), ("valorant", "валорант"),
    ("riot", "райот"), ("viber", "вайбер"), ("zoom", "зум"), ("skype", "скайп"), ("slack", "слак"), ("notion", "ноушн"),
    ("obsidian", "обсидиан"), ("figma", "фигма"), ("krita", "крита"), ("gimp", "гимп"), ("audacity", "одасити"),
    ("winrar", "винрар"), ("torrent", "торрент"), ("nvidia", "нвидиа"), ("geforce", "джифорс"),
    ("experience", "экспириенс"), ("battle", "батл"), ("net", "нет"), ("wallpaper", "воллпейпер"),
    ("terminal", "терминал"), ("powershell", "пауэршелл"), ("python", "питон"), ("git", "гит"), ("bash", "баш"),
    ("github", "гитхаб"), ("gitlab", "гитлаб"), ("microsoft", "майкрософт"), ("store", "стор"), ("xbox", "иксбокс"),
    ("ableton", "аблетон"), ("live", "лайв"), ("cubase", "кьюбейс"), ("reaper", "рипер"), ("everything", "эвритинг"),
    ("lightshot", "лайтшот"), ("sharex", "шарекс"), ("upscayl", "апскейл"), ("topaz", "топаз"), ("video", "видео"),
    ("ai", "эй ай"), ("proton", "протон"), ("vpn", "впн"), ("radmin", "радмин"), ("ollama", "оллама"),
    ("claude", "клод"), ("codex", "кодекс"), ("cursor", "курсор"), ("antigravity", "антигравити"),
    ("rider", "райдер"), ("pycharm", "пайчарм"), ("intellij", "интеллидж"), ("idea", "айдиа"), ("android", "андроид"),
    ("bluestacks", "блюстакс"), ("media", "медиа"), ("music", "мьюзик"), ("factorio", "факторио"),
    ("palworld", "палворлд"), ("rimworld", "римворлд"), ("terraria", "террария"), ("maxon", "максон"),
    ("cinema", "синема"), ("houdini", "гудини"), ("maya", "майя"), ("zbrush", "зибраш"), ("keyshot", "кейшот"),
    ("autocad", "автокад"), ("sketchup", "скетчап"), ("revit", "ревит"), ("nexus", "нексус"), ("vital", "витал"),
    ("serum", "серум"), ("splice", "сплайс"), ("tunnelbear", "таннел бир"), ("windhawk", "виндхок"),
    ("powertoys", "пауэр тойс"), ("translucenttb", "транслюсент ти би"), ("eartrumpet", "ир трампет"),
    ("hive", "хайв"), ("marknote", "маркноут"), ("bloody", "блади"), ("blackmagic", "блэкмэджик"), ("purity", "пьюрити"), ("sandisk", "сандиск"), ("dashboard", "дашборд"),
    ("crystaldiskinfo", "кристал диск инфо"), ("arena", "арена"), ("breakout", "брейкаут"), ("infinite", "инфинит"),
    ("isaac", "айзек"), ("binding", "байндинг"), ("rebirth", "ребёрс"), ("chivalry", "шивалри"),
    ("geoguessr", "геогессер"), ("brotato", "бротато"), ("necesse", "несесси"), ("barony", "барони"),
    ("maneater", "мэнитер"), ("bodycam", "бодикам"), ("soundpad", "саундпад"), ("lossless", "лослесс"),
    ("scaling", "скейлинг"), ("plague", "плейг"), ("evolved", "эволвд"), ("kingdom", "кингдом"), ("rush", "раш"),
    ("bloons", "блунс"), ("dome", "доум"), ("keeper", "кипер"), ("teamfight", "тимфайт"), ("tactics", "тактикс"),
    ("legends", "леджендс"), ("whatsapp", "ватсап"), ("calculator", "калькулятор"), ("camera", "камера"),
    ("photos", "фотос"), ("weather", "погода"), ("clock", "часы"), ("todo", "ту ду"), ("phone", "фон"),
    ("link", "линк"), ("snipping", "сниппинг"), ("tool", "тул"), ("sticky", "стики"), ("notes", "ноутс"),
    ("security", "секьюрити"), ("settings", "сеттингс"), ("explorer", "эксплорер"), ("manager", "менеджер"),
    ("control", "контрол"), ("panel", "панель"), ("app", "апп"), ("one", "ван"), ("box", "бокс"), ("ea", "иа"),
    ("bsg", "би эс джи"), ("battlestate", "батлстейт"), ("fl", "эф эл"), ("obs", "обс"), ("vlc", "вэ эл си"),
    ("mpc", "эм пи си"), ("qbittorrent", "кьюбиторрент"), ("utorrent", "юторрент"), ("winamp", "винамп"),
    ("aimp", "аимп"), ("foobar", "фубар"), ("anydesk", "энидеск"), ("teamviewer", "тимвьюер"),
    ("parsec", "парсек"), ("yandex", "яндекс"), ("vivaldi", "вивальди"), ("tor", "тор"), ("thunderbird", "тандерберд"),
    ("filezilla", "файлзилла"), ("putty", "путти"), ("docker", "докер"), ("postman", "постман"),
    ("dbeaver", "дибивер"), ("godot", "годот"), ("aseprite", "асепрайт"), ("inkscape", "инкскейп"),
    ("darktable", "дарктейбл"), ("handbrake", "хэндбрейк"), ("kdenlive", "кденлайв"), ("shotcut", "шоткат"),
    ("audiolens", "аудиоленс"), ("repix", "репикс"), ("reshot", "ришот"), ("rebuffer", "рибаффер"),
    ("the", "зе"), ("of", "оф"), ("and", "энд"), ("to", "ту"), ("in", "ин"), ("for", "фор"), ("how", "хау"),
    ("download", "даунлоад"), ("downloader", "даунлоадер"), ("upload", "аплоад"), ("button", "баттон"),
    ("hunters", "хантерс"), ("dust", "даст"), ("plugins", "плагинс"), ("cloud", "клауд"), ("s&box", "сэндбокс"),
    ("windows", "виндовс"), ("shoebox", "шубокс"), ("viewer", "вьюер"), ("utility", "ютилити"),
    ("installer", "инсталлер"), ("editor", "эдитор"), ("builder", "билдер"), ("effect", "эффект"),
];

// ── Иконки ───────────────────────────────────────────────────────────────

/// Иконки программ 32×32 как data-URL PNG по AppID; не нашлась — программы нет в ответе.
pub fn icons(ids: &[String]) -> HashMap<String, String> {
    // В 4 потока: 270 иконок по ~12 мс — это 3 с подряд.
    const THREADS: usize = 4;
    let chunk = ids.len().div_ceil(THREADS).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = ids.chunks(chunk).map(|part| scope.spawn(move || icons_in_thread(part))).collect();
        workers.into_iter().flat_map(|worker| worker.join().unwrap_or_default()).collect()
    })
}

fn icons_in_thread(ids: &[String]) -> HashMap<String, String> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    let mut out = HashMap::new();
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    for id in ids {
        if let Some(url) = unsafe { icon(id) } {
            out.insert(id.clone(), url);
        }
    }
    if initialized {
        unsafe { CoUninitialize() };
    }
    out
}

const ICON_SIZE: i32 = 32;

unsafe fn icon(app_id: &str) -> Option<String> {
    use base64::Engine;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{HWND, SIZE};
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::System::Com::IBindCtx;
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY};

    let path = HSTRING::from(format!("shell:AppsFolder\\{app_id}"));
    let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&path, None::<&IBindCtx>).ok()?;
    let bitmap = factory.GetImage(SIZE { cx: ICON_SIZE, cy: ICON_SIZE }, SIIGBF_ICONONLY).ok()?;
    let object = HGDIOBJ(bitmap.0);
    let mut header = BITMAP::default();
    let read = GetObjectW(object, std::mem::size_of::<BITMAP>() as i32, Some(&mut header as *mut _ as *mut _));
    let (width, height) = (header.bmWidth, header.bmHeight.abs());
    let mut pixels = vec![0u8; (width.max(0) * height * 4) as usize];
    let mut lines = 0;
    if read > 0 && width > 0 && height > 0 {
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // сверху вниз
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = GetDC(HWND::default());
        lines = GetDIBits(dc, bitmap, 0, height as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(HWND::default(), dc);
    }
    let _ = DeleteObject(object);
    if lines <= 0 {
        return None;
    }

    // BGRA с premultiplied alpha → RGBA. Без альфы вовсе (старые иконки) — непрозрачные.
    let opaque = pixels.chunks_exact(4).all(|pixel| pixel[3] == 0);
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = if opaque { 255 } else { pixel[3] };
        let unpremultiply = |value: u8| if alpha == 0 { 0 } else { ((value as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8 };
        let (blue, green, red) = (pixel[0], pixel[1], pixel[2]);
        pixel[0] = if opaque { red } else { unpremultiply(red) };
        pixel[1] = if opaque { green } else { unpremultiply(green) };
        pixel[2] = if opaque { blue } else { unpremultiply(blue) };
        pixel[3] = alpha;
    }
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&pixels).ok()?;
    }
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png_bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, id: &str) -> AppEntry {
        AppEntry { name: name.into(), aumid: id.into() }
    }

    fn scan_names(entries: &[(&str, &str)], languages: &[&str]) -> Vec<ScannedApp> {
        scan(entries.iter().map(|(name, id)| entry(name, id)).collect(), languages, &[], &[])
    }

    #[test]
    fn davinci_gets_latin_and_cyrillic_phrases() {
        let apps = scan_names(&[("DaVinci Resolve", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Blackmagic Design\DaVinci Resolve\Resolve.exe")], &["en", "ru"]);
        let phrases = &apps[0].phrases;
        for expected in ["davinci resolve", "davinci", "resolve davinci", "давинчи резолв", "давинчи"] {
            assert!(phrases.contains(&expected.to_string()), "{expected} not in {phrases:?}");
        }
        assert!(apps[0].checked);
        assert_eq!(apps[0].kind, "uwp");
    }

    #[test]
    fn without_a_cyrillic_language_phrases_stay_latin() {
        let apps = scan_names(&[("Discord", "com.squirrel.Discord.Discord")], &["en", "de"]);
        assert_eq!(apps[0].phrases, vec!["discord"]);
    }

    #[test]
    fn junk_shortcuts_are_dropped() {
        let apps = scan_names(
            &[
                ("Uninstall Vital", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Vital\unins000.exe"),
                ("7-Zip Help", r"{6D809377-6AF0-444B-8957-A3773F02200E}\7-Zip\7-zip.chm"),
                ("Steam Support Center", "http://support.steampowered.com/"),
                ("Manual", r"C:\ProgramData\oeksound\soothe2\soothe2_ManualFAQ.pdf"),
                ("Synplant Patches", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Sonic Charge\Synplant Patches"),
                ("EA Updater", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Electronic Arts\EA Desktop\EA Desktop\EAUpdater.exe"),
                ("Деинсталлировать Telegram", r"C:\Users\me\AppData\Roaming\Telegram Desktop\unins000.exe"),
                ("Nightly Private Browsing", "CA062AFCB9D4F69B;PrivateBrowsingAUMID"),
                ("Reever", "com.troid.reever"),
                ("Vital", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Vital\Vital.exe"),
            ],
            &["en"],
        );
        let names: Vec<&str> = apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, vec!["Vital"]);
    }

    #[test]
    fn versions_collapse_to_the_newest() {
        let apps = scan_names(
            &[
                ("FL Studio 2024", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Image-Line\FL Studio 2024\FL64.exe"),
                ("FL Studio 2025", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Image-Line\FL Studio 2025\FL64.exe"),
                ("Windows PowerShell", r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe"),
                ("Windows PowerShell (x86)", r"{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}\WindowsPowerShell\v1.0\powershell.exe"),
            ],
            &["en"],
        );
        assert_eq!(apps.len(), 2);
        let fl = apps.iter().find(|app| app.name == "FL Studio").unwrap();
        assert!(fl.target.contains("2025"));
        let shell = apps.iter().find(|app| app.name == "Windows PowerShell").unwrap();
        assert!(shell.target.contains("1AC14E77"));
        assert_eq!(shell.note, Some(Note::System));
        assert!(!shell.checked);
    }

    #[test]
    fn generic_names_are_listed_unchecked() {
        let apps = scan_names(&[("Camera", "Microsoft.WindowsCamera_8wekyb3d8bbwe!App"), ("Media Player", "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic")], &["en"]);
        assert!(apps.iter().all(|app| app.note == Some(Note::Generic) && !app.checked));
    }

    #[test]
    fn vendor_and_noise_words_are_dropped_from_the_main_phrase() {
        let apps = scan_names(
            &[
                ("Adobe Photoshop 2025", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Adobe\Adobe Photoshop 2025\Photoshop.exe"),
                ("Minecraft Launcher", "Microsoft.4297127D64EC6_8wekyb3d8bbwe!Minecraft"),
                ("OBS Studio (64bit)", r"{6D809377-6AF0-444B-8957-A3773F02200E}\obs-studio\bin\64bit\obs64.exe"),
            ],
            &["en", "ru"],
        );
        let find = |name: &str| apps.iter().find(|app| app.name == name).unwrap();
        assert_eq!(find("Adobe Photoshop").phrases[0], "photoshop");
        assert!(find("Adobe Photoshop").phrases.contains(&"фотошоп".to_string()));
        assert_eq!(find("Minecraft Launcher").phrases[0], "minecraft");
        assert!(find("OBS Studio").phrases.contains(&"обс".to_string()));
    }

    #[test]
    fn steam_games_and_paths_launch_directly() {
        let apps = scan_names(&[("Dota 2", "steam://rungameid/570"), ("Spotify", r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe")], &["en", "ru"]);
        let dota = apps.iter().find(|app| app.name == "Dota").unwrap();
        assert_eq!((dota.kind.as_str(), dota.target.as_str()), ("shell", "steam://rungameid/570"));
        assert!(dota.phrases.contains(&"дота".to_string()));
        let spotify = apps.iter().find(|app| app.name == "Spotify").unwrap();
        assert_eq!(spotify.kind, "shell");
        assert!(spotify.phrases.contains(&"спотифай".to_string()));
    }

    #[test]
    fn a_phrase_shared_by_two_apps_stays_with_its_owner() {
        let apps = scan_names(&[("Unity", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Unity\Hub\Editor\6000.3.9f1\Editor\Unity.exe"), ("Unity Hub", "com.unity3d.unityhub")], &["en", "ru"]);
        let hub = apps.iter().find(|app| app.name == "Unity Hub").unwrap();
        let unity = apps.iter().find(|app| app.name == "Unity").unwrap();
        assert!(unity.phrases.contains(&"unity".to_string()));
        assert!(!hub.phrases.contains(&"unity".to_string()));
        assert!(unity.phrases.contains(&"юнити".to_string()));
        assert!(!hub.phrases.contains(&"юнити".to_string()));
        assert!(hub.phrases.contains(&"unity hub".to_string()));
    }

    #[test]
    fn existing_actions_and_reserved_phrases_are_respected() {
        let spotify = Action {
            id: "a".into(),
            label: "Spotify".into(),
            kind: "shell".into(),
            target: r"%APPDATA%\Spotify\Spotify.exe".into(),
            verbs: Vec::new(),
            phrases: vec!["спотифай".into()],
        };
        let apps = scan(
            vec![entry("Spotify", r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe"), entry("Search", "Search.App")],
            &["en", "ru"],
            &[spotify],
            &["search".to_string()],
        );
        let find = |name: &str| apps.iter().find(|app| app.name == name).unwrap();
        assert_eq!(find("Spotify").note, Some(Note::Exists));
        assert!(!find("Spotify").checked);
        assert!(find("Search").phrases.iter().all(|phrase| phrase != "search"));
    }

    #[test]
    fn unknown_words_are_read_the_english_way() {
        assert_eq!(cyrillic_word("vlc"), "вэ эл си");
        assert_eq!(cyrillic_word("nora"), "нора");
        assert!(cyrillic_word("redoku").starts_with("редоку"));
        assert_eq!(cyrillic_word("editor"), "эдитор");
        assert_eq!(cyrillic_word("legacy"), "легаси");
        assert_eq!(cyrillic_word("msys"), "мсис");
        assert_eq!(cyrillic_word("rerounder"), "рераундер");
    }

    #[test]
    fn chosen_apps_become_actions_once() {
        let chosen = vec![
            ChosenApp { name: "Discord".into(), kind: "uwp".into(), target: "com.squirrel.Discord.Discord".into(), phrases: vec!["Discord".into(), "дискорд".into()] },
            ChosenApp { name: "Discord".into(), kind: "uwp".into(), target: "com.squirrel.Discord.Discord".into(), phrases: vec!["discord".into()] },
            ChosenApp { name: "Empty".into(), kind: "uwp".into(), target: "x".into(), phrases: vec![] },
        ];
        let actions = to_actions(chosen, &[], 7);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].phrases, vec!["discord", "дискорд"]);
        assert!(actions[0].id.starts_with("app_"));
    }

    #[test]
    fn frequently_used_apps_go_first_then_recent_then_alphabet() {
        const DAY: u64 = 86_400;
        let now = 100 * DAY;
        let mut apps = scan_names(
            &[("Alpha", "a.app"), ("Beta", "b.app"), ("Discord", "d.app"), ("Opera", "o.app"), ("Zeta", "z.app")],
            &["en"],
        );
        let used = |focus_min: u32, focus_count: u32, days_ago: u64| AppUsage {
            runs: 0,
            focus_count,
            focus_ms: focus_min * 60_000,
            last: Some(now - days_ago * DAY),
        };
        let usage: HashMap<String, AppUsage> = [
            ("o.app".to_string(), used(155, 18, 2)),
            ("d.app".to_string(), used(73, 16, 7)),
            ("z.app".to_string(), used(0, 1, 1)),
            ("b.app".to_string(), used(0, 0, 30)),
        ]
        .into();
        order_by_usage(&mut apps, &usage, now);
        let order: Vec<(&str, Group)> = apps.iter().map(|app| (app.name.as_str(), app.group)).collect();
        assert_eq!(
            order,
            vec![
                ("Opera", Group::Frequent),
                ("Discord", Group::Frequent),
                ("Zeta", Group::Recent),
                ("Alpha", Group::Other),
                ("Beta", Group::Other),
            ]
        );
        assert_eq!(apps[0].focus_minutes, 155);
        assert_eq!(apps[0].last_used, Some(now - 2 * DAY));
    }

    #[test]
    fn user_assist_records_are_decoded() {
        assert_eq!(rot13("pbz.fdhveery.Qvfpbeq.Qvfpbeq"), "com.squirrel.Discord.Discord");
        let mut data = vec![0u8; 72];
        data[8..12].copy_from_slice(&16u32.to_le_bytes());
        data[12..16].copy_from_slice(&(73u32 * 60_000).to_le_bytes());
        // 2026-10-18 11:03:01 UTC в FILETIME
        let filetime: u64 = (1_792_234_981 + 11_644_473_600) * 10_000_000;
        data[60..68].copy_from_slice(&filetime.to_le_bytes());
        let usage = parse_user_assist(&data).unwrap();
        assert_eq!((usage.focus_count, usage.focus_ms / 60_000, usage.last), (16, 73, Some(1_792_234_981)));
        // Нулевая дата (записи без запусков) — без даты, короткие данные — не запись.
        assert_eq!(parse_user_assist(&[0u8; 72]).unwrap().last, None);
        assert_eq!(parse_user_assist(&[0u8; 16]), None);
    }
}
