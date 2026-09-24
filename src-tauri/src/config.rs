use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

/// Таблицы алиасов из resources/aliases.json (редактируется пользователем).
#[derive(Clone, Default, Deserialize)]
pub struct Aliases {
    #[serde(default)]
    pub folders: HashMap<String, String>,
    #[serde(default)]
    pub apps: HashMap<String, String>,
}

impl Aliases {
    fn lookup(map: &HashMap<String, String>, q: &str) -> Option<String> {
        let q = q.trim().to_lowercase();
        map.get(&q)
            .cloned()
            .or_else(|| map.get(&q.replace(' ', "")).cloned())
    }
    pub fn folder(&self, q: &str) -> Option<String> {
        Self::lookup(&self.folders, q)
    }
    pub fn app(&self, q: &str) -> Option<String> {
        Self::lookup(&self.apps, q)
    }
}

/// Настройки из resources/settings.json.
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_host")]
    pub everything_host: String,
    #[serde(default = "default_port")]
    pub everything_port: u16,
    /// Как модель «слышит» wake-word. Активация, если любое слово фразы совпало.
    #[serde(default = "default_wake_words")]
    pub wake_words: Vec<String>,
    /// «Запусти»: если ни одно действие не совпало — искать программу в Everything.
    #[serde(default)]
    pub launch_everything_fallback: bool,
    /// Порог fuzzy-совпадения фразы с действием (0..1). Выше — строже.
    #[serde(default = "default_match_threshold")]
    pub match_threshold: f64,
    /// Включить английскую модель (декод командного буфера on-demand).
    #[serde(default = "default_true")]
    pub english_enabled: bool,
    /// Папка sherpa-onnx модели относительно resources/model/.
    #[serde(default = "default_asr_model")]
    pub asr_model: String,
    /// Число потоков ONNX Runtime для распознавания.
    #[serde(default = "default_asr_threads")]
    pub asr_threads: u32,
    /// Порог Silero VAD (0..1).
    #[serde(default = "default_vad_threshold")]
    pub vad_threshold: f32,
    /// Тишина для конца фразы в режиме ожидания wake-слова.
    #[serde(default = "default_wake_pause_ms")]
    pub wake_pause_ms: u32,
    /// Тишина для конца команды после wake-слова.
    #[serde(default = "default_command_pause_ms")]
    pub command_pause_ms: u32,
    /// Сколько ждать начала речи после wake-слова.
    #[serde(default = "default_command_wait_ms")]
    pub command_wait_ms: u32,
    /// Показывать распознанный текст команды до её завершения.
    #[serde(default = "default_true")]
    pub partial_results: bool,
    /// Слова отмены команды (стоп/передумал).
    #[serde(default = "default_cancel_words")]
    pub cancel_words: Vec<String>,
    /// Минимальная похожесть для «открой» (динамика). Ниже — «не понял».
    #[serde(default = "default_open_min_sim")]
    pub open_min_similarity: f64,
    /// Поднимать открытое окно на передний план (best-effort).
    #[serde(default = "default_true")]
    pub raise_result: bool,
    /// Режим активации: "wake" | "hotkey" | "both".
    #[serde(default = "default_activation")]
    pub activation_mode: String,
    /// Глобальный хоткей (push-to-talk), напр. "CmdOrCtrl+Shift+Space".
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    /// Подтверждать опасные системные команды (shutdown/restart) голосом.
    #[serde(default = "default_true")]
    pub confirm_dangerous: bool,
    /// Активный набор звуков (папка в sounds/).
    #[serde(default = "default_sound_pack")]
    pub sound_pack: String,
    /// Приватный лог: не показывать распознанный текст/запросы в UI и stdout.
    #[serde(default)]
    pub private_log: bool,
    /// Офлайн-режим: без веб-поиска и Everything-HTTP (нулевой сетевой трафик).
    #[serde(default)]
    pub offline_mode: bool,
    /// Хоткей мгновенного «заглушить микрофон» (panic-mute). Пусто — выкл.
    #[serde(default)]
    pub mute_hotkey: String,
    /// Пауза конца фразы (мс): сколько тишины ждать перед финализацией. Меньше —
    /// быстрее реакция, но слишком мало рискует обрезать фразу между словами.
    #[serde(default = "default_endpoint_ms")]
    pub endpoint_ms: u64,
}

fn default_endpoint_ms() -> u64 {
    250
}

fn default_asr_model() -> String {
    "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16".into()
}
fn default_asr_threads() -> u32 {
    4
}
fn default_vad_threshold() -> f32 {
    0.5
}
fn default_wake_pause_ms() -> u32 {
    600
}
fn default_command_pause_ms() -> u32 {
    900
}
fn default_command_wait_ms() -> u32 {
    5000
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8011
}
fn default_wake_words() -> Vec<String> {
    vec!["ривер".into(), "ревер".into(), "ривьер".into()]
}
fn default_match_threshold() -> f64 {
    0.7
}
fn default_true() -> bool {
    true
}
fn default_cancel_words() -> Vec<String> {
    vec![
        "отмена".into(),
        "стоп".into(),
        "отбой".into(),
        "забудь".into(),
    ]
}
fn default_open_min_sim() -> f64 {
    0.55
}
fn default_activation() -> String {
    "both".into()
}
fn default_hotkey() -> String {
    "Ctrl+Shift+G".into()
}
fn default_sound_pack() -> String {
    "default".into()
}

/// Десериализует настройки по одному полю: ошибка одного значения не сбрасывает
/// остальные настройки пользователя.
fn parse_settings(text: &str) -> Settings {
    let value = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => value,
        Err(error) => {
            eprintln!(
                "[WARN] settings.json не читается ({error}); применены значения по умолчанию"
            );
            return Settings::default();
        }
    };
    let Some(object) = value.as_object() else {
        eprintln!("[WARN] settings.json должен содержать объект; применены значения по умолчанию");
        return Settings::default();
    };

    let mut settings = Settings::default();
    let mut invalid = Vec::new();
    macro_rules! restore {
        ($key:literal, $field:ident, $ty:ty) => {
            if let Some(value) = object.get($key) {
                match serde_json::from_value::<$ty>(value.clone()) {
                    Ok(parsed) => settings.$field = parsed,
                    Err(error) => invalid.push(format!("{} ({error})", $key)),
                }
            }
        };
    }

    restore!("everything_host", everything_host, String);
    restore!("everything_port", everything_port, u16);
    restore!("wake_words", wake_words, Vec<String>);
    restore!(
        "launch_everything_fallback",
        launch_everything_fallback,
        bool
    );
    restore!("match_threshold", match_threshold, f64);
    restore!("english_enabled", english_enabled, bool);
    restore!("asr_model", asr_model, String);
    restore!("asr_threads", asr_threads, u32);
    restore!("vad_threshold", vad_threshold, f32);
    restore!("wake_pause_ms", wake_pause_ms, u32);
    restore!("command_pause_ms", command_pause_ms, u32);
    restore!("command_wait_ms", command_wait_ms, u32);
    restore!("partial_results", partial_results, bool);
    restore!("cancel_words", cancel_words, Vec<String>);
    restore!("open_min_similarity", open_min_similarity, f64);
    restore!("raise_result", raise_result, bool);
    restore!("activation_mode", activation_mode, String);
    restore!("hotkey", hotkey, String);
    restore!("confirm_dangerous", confirm_dangerous, bool);
    restore!("sound_pack", sound_pack, String);
    restore!("private_log", private_log, bool);
    restore!("offline_mode", offline_mode, bool);
    restore!("mute_hotkey", mute_hotkey, String);
    restore!("endpoint_ms", endpoint_ms, u64);

    if !invalid.is_empty() {
        eprintln!(
            "[WARN] неверные поля settings.json восстановлены по умолчанию: {}",
            invalid.join(", ")
        );
    }
    settings
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            everything_host: default_host(),
            everything_port: default_port(),
            wake_words: default_wake_words(),
            launch_everything_fallback: false,
            match_threshold: default_match_threshold(),
            english_enabled: true,
            asr_model: default_asr_model(),
            asr_threads: default_asr_threads(),
            vad_threshold: default_vad_threshold(),
            wake_pause_ms: default_wake_pause_ms(),
            command_pause_ms: default_command_pause_ms(),
            command_wait_ms: default_command_wait_ms(),
            partial_results: true,
            cancel_words: default_cancel_words(),
            open_min_similarity: default_open_min_sim(),
            raise_result: true,
            activation_mode: default_activation(),
            hotkey: default_hotkey(),
            confirm_dangerous: true,
            sound_pack: default_sound_pack(),
            private_log: false,
            offline_mode: false,
            mute_hotkey: String::new(),
            endpoint_ms: default_endpoint_ms(),
        }
    }
}

/// Пути ко всем внешним ресурсам + загруженные настройки/алиасы.
#[derive(Clone)]
pub struct ReeverConfig {
    pub asr_model_dir: PathBuf,
    pub vad_model_path: PathBuf,
    pub sounds_dir: PathBuf,
    pub settings_path: PathBuf,
    pub aliases: Aliases,
    pub settings: Settings,
}

impl ReeverConfig {
    pub fn resolve(app: &AppHandle) -> tauri::Result<Self> {
        let aliases = std::fs::read_to_string(user_config_path(app, "aliases.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Aliases>(&s).ok())
            .unwrap_or_default();

        let settings_path = user_config_path(app, "settings.json");
        let settings = std::fs::read_to_string(&settings_path)
            .ok()
            .map(|s| parse_settings(&s))
            .unwrap_or_default();

        Ok(Self {
            asr_model_dir: resource(app, &format!("model/{}", settings.asr_model)),
            vad_model_path: resource(app, "model/silero_vad.onnx"),
            sounds_dir: user_config_dir(app, "sounds"),
            settings_path,
            aliases,
            settings,
        })
    }

    /// Путь к звуку активного пака; если в паке файла нет — берём из default.
    pub fn sound(&self, name: &str) -> PathBuf {
        let pack = if self.settings.sound_pack.trim().is_empty() {
            "default"
        } else {
            self.settings.sound_pack.as_str()
        };
        let p = self.sounds_dir.join(pack).join(name);
        if p.exists() {
            p
        } else {
            self.sounds_dir.join("default").join(name)
        }
    }
}

/// Пользовательская ДИРЕКТОРИЯ (звуки-паки): dev — src-tauri/resources/<name>;
/// release — %APPDATA%/<id>/<name>, сид рекурсивно из бандла при первом запуске.
pub fn user_config_dir(app: &AppHandle, name: &str) -> PathBuf {
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(name);
    }
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(name);
    if !dir.exists() {
        if let Some(seed) = bundled(app, name) {
            let _ = copy_dir_recursive(&seed, &dir);
        }
    }
    dir
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Путь к ПОЛЬЗОВАТЕЛЬСКОМУ конфигу (полностью редактируемому):
/// dev — `src-tauri/resources/<name>` (бандлится в инсталлер при сборке);
/// release — `%APPDATA%/<identifier>/<name>` (writable), сид из бандла при первом запуске.
pub fn user_config_path(app: &AppHandle, name: &str) -> PathBuf {
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(name);
    }
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(name);
    if !p.exists() {
        if let Some(seed) = bundled(app, name) {
            let _ = std::fs::copy(&seed, &p);
        }
    }
    p
}

/// Ищет бандлированный ресурс по всем вероятным раскладкам, чтобы установленная
/// версия точно нашла файл независимо от того, куда Tauri его положил:
///   <resource_dir>/<rel>, <resource_dir>/resources/<rel>,
///   <exe_dir>/<rel>, <exe_dir>/resources/<rel>.
fn bundled(app: &AppHandle, rel: &str) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = app.path().resolve(rel, BaseDirectory::Resource) {
        cands.push(p);
    }
    if let Ok(p) = app
        .path()
        .resolve(&format!("resources/{rel}"), BaseDirectory::Resource)
    {
        cands.push(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join(rel));
            cands.push(dir.join("resources").join(rel));
        }
    }
    cands.into_iter().find(|p| p.exists()).map(strip_verbatim)
}

/// Срезает `\\?\` (extended-length prefix). Rust его понимает, а нативные
/// C-библиотеки (sherpa-onnx/ONNX Runtime) — не всегда: путь с `\\?\` для них может
/// оказаться несуществующим.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC\\") {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Резолвит read-only ресурс (модели/звуки): bundled, в dev — src-tauri/resources.
pub(crate) fn resource(app: &AppHandle, rel: &str) -> PathBuf {
    bundled(app, rel).unwrap_or_else(|| {
        strip_verbatim(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .join(rel),
        )
    })
}
