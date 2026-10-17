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
    /// Устарело (до 0.3.6): английский теперь включён всегда, см. `languages`.
    #[serde(default = "default_true")]
    pub english_enabled: bool,
    /// Языки распознавания (`RECOGNITION_LANGUAGES`). Английский включён всегда, даже если
    /// его нет в списке; без русского основной моделью становится английская.
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
    /// Папка sherpa-onnx модели относительно resources/model/.
    #[serde(default = "default_asr_model")]
    pub asr_model: String,
    /// Папка второй модели для распознавания цели команды (английская/мультиязычная).
    #[serde(default = "default_second_asr_model")]
    pub second_asr_model: String,
    /// Папка модели LID; пустая строка отключает модель и оставляет текстовые эвристики.
    #[serde(default = "default_lid_model")]
    pub lid_model: String,
    /// Язык цели команды по умолчанию: `auto`, `ru` или `en`.
    #[serde(default = "default_payload_language")]
    pub payload_language: String,
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
    /// Спрашивать «да/нет» перед закрытием и завершением программ голосом.
    #[serde(default)]
    pub confirm_close: bool,
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
    /// Диктовка: пауза (мс), после которой сказанный кусок печатается; диктовка идёт дальше.
    #[serde(default = "default_dictation_pause_ms")]
    pub dictation_pause_ms: u32,
    /// Диктовка заканчивается, если столько миллисекунд не было речи.
    #[serde(default = "default_dictation_idle_ms")]
    pub dictation_idle_ms: u32,
    /// Нет поля с курсором — продиктованный текст идёт в буфер обмена.
    #[serde(default = "default_true")]
    pub dictation_clipboard: bool,
    /// Печатать диктовку по ходу речи (по словам), а не после паузы.
    #[serde(default = "default_true")]
    pub dictation_live: bool,
    /// Английские слова в русской диктовке латиницей по словарю англицизмов
    /// («харднес» → hardness, см. anglicism.rs).
    #[serde(default = "default_true")]
    pub dictation_english: bool,
    /// Свои английские слова для диктовки: `opacity` — предпочитать это слово,
    /// `апасити=opacity` — всегда менять именно так.
    #[serde(default)]
    pub dictation_terms: Vec<String>,
    /// «ривер гемини …»: сразу отправлять вопрос (Enter), а не только вводить.
    #[serde(default = "default_true")]
    pub webchat_send: bool,
    /// Фоновый детектор wake-слова на маленькой потоковой модели.
    #[serde(default = "default_true")]
    pub wake_spotter: bool,
    /// Папка модели детектора в resources/model.
    #[serde(default = "default_wake_spotter_model")]
    pub wake_spotter_model: String,
    /// Сигнал сразу, как только услышано wake-слово.
    #[serde(default = "default_true")]
    pub wake_sound: bool,
    /// Язык интерфейса: `auto` (язык Windows) или код из `uilang::SUPPORTED`.
    #[serde(default = "default_ui_language")]
    pub ui_language: String,
    /// Через сколько секунд простоя выгружать тяжёлые модели распознавания (в покое
    /// остаётся только детектор «ривер»); 0 — не выгружать никогда.
    #[serde(default = "default_models_unload_s")]
    pub models_unload_s: u32,
    /// Откуда искать файлы и папки: `everything` (Everything HTTP), `local` (свой
    /// индекс дисков Reever), `auto` (Everything, если доступен, иначе свой индекс).
    #[serde(default = "default_find_source")]
    pub find_source: String,
    /// Модель режима «только английский» (основная, когда русский выключен).
    #[serde(default = "default_english_only_model")]
    pub english_only_model: String,
    /// Когда показывать маскота: `always`, `commands` (пока слушает и выполняет
    /// команду) или `never`.
    #[serde(default = "default_mascot_mode")]
    pub mascot_mode: String,
    /// Размер маскота в процентах от исходного, 100..500.
    #[serde(default = "default_mascot_scale")]
    pub mascot_scale: u32,
    /// Как маскот появляется и уходит: `flight` (мёртвая петля) или `portal`.
    #[serde(default = "default_mascot_entrance")]
    pub mascot_entrance: String,
}

fn default_models_unload_s() -> u32 {
    60
}
fn default_mascot_mode() -> String {
    "always".into()
}
fn default_mascot_scale() -> u32 {
    100
}
fn default_mascot_entrance() -> String {
    "flight".into()
}
fn default_find_source() -> String {
    "everything".into()
}
fn default_english_only_model() -> String {
    "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8".into()
}

/// Языки, которые Reever умеет распознавать, по порядку показа. Английский — первый
/// и обязательный; новый язык — новая строка здесь и модель для него.
pub const RECOGNITION_LANGUAGES: &[&str] = &["en", "ru"];

fn default_languages() -> Vec<String> {
    vec!["ru".into(), "en".into()]
}

impl Settings {
    /// Включённые языки распознавания в порядке `RECOGNITION_LANGUAGES`: английский
    /// всегда, неизвестные коды отброшены.
    pub fn recognition_languages(&self) -> Vec<&'static str> {
        RECOGNITION_LANGUAGES
            .iter()
            .copied()
            .filter(|code| {
                *code == "en"
                    || self
                        .languages
                        .iter()
                        .any(|language| language.trim().eq_ignore_ascii_case(code))
            })
            .collect()
    }

    /// Русский включён: основная модель — русская, английская распознаёт цели команд.
    pub fn russian_enabled(&self) -> bool {
        self.recognition_languages().contains(&"ru")
    }

    /// Папка основной модели: русская, а без русского — отдельная модель английского режима.
    pub fn primary_asr_model(&self) -> &str {
        if self.russian_enabled() {
            &self.asr_model
        } else {
            &self.english_only_model
        }
    }
}

fn default_ui_language() -> String {
    "auto".into()
}

fn default_wake_spotter_model() -> String {
    "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16".into()
}

fn default_dictation_pause_ms() -> u32 {
    800
}
fn default_dictation_idle_ms() -> u32 {
    3_000
}

fn default_endpoint_ms() -> u64 {
    250
}

fn default_asr_model() -> String {
    "sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16".into()
}
fn default_second_asr_model() -> String {
    "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8".into()
}
fn default_lid_model() -> String {
    "sherpa-onnx-whisper-base".into()
}
fn default_payload_language() -> String {
    "auto".into()
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
    restore!("second_asr_model", second_asr_model, String);
    restore!("lid_model", lid_model, String);
    restore!("payload_language", payload_language, String);
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
    restore!("confirm_close", confirm_close, bool);
    restore!("sound_pack", sound_pack, String);
    restore!("private_log", private_log, bool);
    restore!("offline_mode", offline_mode, bool);
    restore!("mute_hotkey", mute_hotkey, String);
    restore!("endpoint_ms", endpoint_ms, u64);
    restore!("dictation_pause_ms", dictation_pause_ms, u32);
    restore!("dictation_idle_ms", dictation_idle_ms, u32);
    restore!("models_unload_s", models_unload_s, u32);
    restore!("find_source", find_source, String);
    restore!("english_only_model", english_only_model, String);
    restore!("dictation_clipboard", dictation_clipboard, bool);
    restore!("dictation_live", dictation_live, bool);
    restore!("dictation_english", dictation_english, bool);
    restore!("dictation_terms", dictation_terms, Vec<String>);
    restore!("webchat_send", webchat_send, bool);
    restore!("wake_spotter", wake_spotter, bool);
    restore!("wake_spotter_model", wake_spotter_model, String);
    restore!("wake_sound", wake_sound, bool);
    restore!("ui_language", ui_language, String);
    restore!("languages", languages, Vec<String>);
    restore!("mascot_mode", mascot_mode, String);
    restore!("mascot_scale", mascot_scale, u32);
    restore!("mascot_entrance", mascot_entrance, String);

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
            second_asr_model: default_second_asr_model(),
            lid_model: default_lid_model(),
            payload_language: default_payload_language(),
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
            confirm_close: false,
            sound_pack: default_sound_pack(),
            private_log: false,
            offline_mode: false,
            mute_hotkey: String::new(),
            endpoint_ms: default_endpoint_ms(),
            dictation_pause_ms: default_dictation_pause_ms(),
            dictation_idle_ms: default_dictation_idle_ms(),
            dictation_clipboard: true,
            dictation_live: true,
            dictation_english: true,
            dictation_terms: Vec::new(),
            webchat_send: true,
            wake_spotter: true,
            wake_spotter_model: default_wake_spotter_model(),
            wake_sound: true,
            ui_language: default_ui_language(),
            languages: default_languages(),
            models_unload_s: default_models_unload_s(),
            find_source: default_find_source(),
            english_only_model: default_english_only_model(),
            mascot_mode: default_mascot_mode(),
            mascot_scale: default_mascot_scale(),
            mascot_entrance: default_mascot_entrance(),
        }
    }
}

/// Пути ко всем внешним ресурсам + загруженные настройки/алиасы.
#[derive(Clone)]
pub struct ReeverConfig {
    pub asr_model_dir: PathBuf,
    pub second_model_dir: PathBuf,
    /// `None` означает режим LID только по текстовым эвристикам.
    pub lid_model_dir: Option<PathBuf>,
    pub vad_model_path: PathBuf,
    /// Словарь англицизмов для диктовки (resources/anglicisms.tsv).
    pub anglicisms_path: PathBuf,
    pub sounds_dir: PathBuf,
    pub settings_path: PathBuf,
    pub aliases: Aliases,
    pub settings: Settings,
}

/// Действующие настройки. «Сохранить» подменяет их на лету, а поток распознавания
/// перезапускается с новыми — без перезапуска всего Reever.
pub struct LiveConfig(std::sync::RwLock<std::sync::Arc<ReeverConfig>>);

impl LiveConfig {
    pub fn new(config: std::sync::Arc<ReeverConfig>) -> Self {
        Self(std::sync::RwLock::new(config))
    }

    pub fn get(&self) -> std::sync::Arc<ReeverConfig> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, config: std::sync::Arc<ReeverConfig>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = config;
    }
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

        let lid_model_dir = if settings.lid_model.trim().is_empty() {
            None
        } else {
            Some(resource(app, &format!("model/{}", settings.lid_model)))
        };

        Ok(Self {
            asr_model_dir: resource(app, &format!("model/{}", settings.primary_asr_model())),
            second_model_dir: resource(app, &format!("model/{}", settings.second_asr_model)),
            lid_model_dir,
            vad_model_path: resource(app, "model/silero_vad.onnx"),
            anglicisms_path: resource(app, "anglicisms.tsv"),
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

#[cfg(test)]
mod tests {
    use super::parse_settings;

    #[test]
    fn english_is_always_a_recognition_language() {
        let both = parse_settings(r#"{"english_enabled": false}"#);
        assert_eq!(both.recognition_languages(), ["en", "ru"]);
        assert_eq!(both.primary_asr_model(), both.asr_model);

        let english = parse_settings(r#"{"languages": ["en"]}"#);
        assert_eq!(english.recognition_languages(), ["en"]);
        assert!(!english.russian_enabled());
        assert_eq!(english.primary_asr_model(), english.english_only_model);
        assert_eq!(
            english.primary_asr_model(),
            "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8"
        );

        // Пустой список и неизвестные коды — всё равно английский.
        let odd = parse_settings(r#"{"languages": ["xx", " RU "]}"#);
        assert_eq!(odd.recognition_languages(), ["en", "ru"]);
        assert_eq!(
            parse_settings(r#"{"languages": []}"#).recognition_languages(),
            ["en"]
        );
    }

    #[test]
    fn ru_en_uses_secondary_english_model_and_english_only_uses_its_own_model() {
        let both = parse_settings(
            r#"{"languages":["ru","en"],"second_asr_model":"parakeet-600m","english_only_model":"parakeet-110m"}"#,
        );
        assert_eq!(both.primary_asr_model(), both.asr_model);
        assert_eq!(both.second_asr_model, "parakeet-600m");

        let english = parse_settings(
            r#"{"languages":["en"],"second_asr_model":"parakeet-600m","english_only_model":"parakeet-110m"}"#,
        );
        assert_eq!(english.primary_asr_model(), "parakeet-110m");
        assert_eq!(english.second_asr_model, "parakeet-600m");
    }
}
