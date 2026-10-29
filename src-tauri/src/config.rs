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
    /// Как модель «слышит» wake-word. Активация, если любое слово фразы совпало.
    #[serde(default = "default_wake_words")]
    pub wake_words: Vec<String>,
    /// Порог fuzzy-совпадения фразы с действием (0..1). Выше — строже.
    #[serde(default = "default_match_threshold")]
    pub match_threshold: f64,
    /// Устарело (до 0.3.6): английский теперь включён всегда, см. `languages`.
    #[serde(default = "default_true")]
    pub english_enabled: bool,
    /// Языки распознавания (`RECOGNITION_LANGUAGES`). Английский включён всегда, даже если
    /// его нет в списке; язык без скачанного пакета модели пропускается (models.rs).
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
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
    /// Офлайн-режим: без веб-поиска и веб-чатов (нулевой сетевой трафик).
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
    /// Английские слова в кириллической диктовке латиницей по словарю англицизмов
    /// («харднес» → hardness, см. anglicism.rs). Включено по умолчанию; без кириллического
    /// языка распознавания не действует.
    #[serde(default = "default_true")]
    pub dictation_english: bool,
    /// Сила замены англицизмов, 1–5: 1 — только очевидные английские слова, 5 — всё,
    /// что похоже на английское и не нашлось в русском словаре (anglicism.rs).
    #[serde(default = "default_dictation_english_level")]
    pub dictation_english_level: u8,
    /// «ривер гемини …»: сразу отправлять вопрос (Enter), а не только вводить.
    #[serde(default = "default_true")]
    pub webchat_send: bool,
    /// «Чёрный ящик» (blackbox.rs): звук вокруг срабатываний детектора — в WAV, для отладки
    /// и дообучения. Пишет микрофон на диск — по умолчанию выключен, включает разработчик.
    #[serde(default)]
    pub wake_blackbox: bool,
    /// Язык интерфейса: `auto` (язык Windows) или код из `uilang::SUPPORTED`.
    #[serde(default = "default_ui_language")]
    pub ui_language: String,
    /// Языки интерфейса, между которыми можно переключаться; английский — всегда.
    /// Отмечаются в установщике (`take_installer_ui_languages`) и в Настройках.
    #[serde(default = "default_ui_languages")]
    pub ui_languages: Vec<String>,
    /// Через сколько секунд простоя выгружать тяжёлые модели распознавания (в покое
    /// остаётся только детектор «ривер»): 0 — сразу после команды, больше
    /// `MODELS_UNLOAD_MAX_S` — никогда (по умолчанию: модель в памяти ещё и ловит
    /// «ривер», пропущенный детектором). Прежний ключ `models_unload_s`, где 0 значило
    /// «никогда», не читается.
    #[serde(default = "default_models_unload_after_s")]
    pub models_unload_after_s: u32,
    /// Тихая подстройка детектора: первые 40 подтверждённых «ривер» сохраняются в папке
    /// настроек и доучивают детектор, потом стираются (voice_adapt.rs).
    #[serde(default = "default_true")]
    pub voice_passive: bool,
    /// Свой индекс дисков для «найди …» и «папка …». Выключен — ни снимка в памяти, ни
    /// фоновых потоков, но и искать файлы голосом нечем.
    #[serde(default = "default_true")]
    pub local_index: bool,
    /// Когда показывать маскота: `always`, `commands` (пока слушает и выполняет
    /// команду) или `never`.
    #[serde(default = "default_mascot_mode")]
    pub mascot_mode: String,
    /// Размер маскота в процентах от исходного, 100..500.
    #[serde(default = "default_mascot_scale")]
    pub mascot_scale: u32,
    /// Пузырь-терминал над маскотом со сказанным по ходу речи.
    #[serde(default = "default_true")]
    pub mascot_bubble: bool,
    /// Лёгкий интерфейс: плоская панель без 3D-рамки, жидкой линзы и маскота. Пока он
    /// включён, маскота нет при любом `mascot_mode` (сам выбор сохраняется на потом).
    #[serde(default)]
    pub lite_ui: bool,
}

fn default_dictation_english_level() -> u8 {
    crate::anglicism::DEFAULT_STRENGTH
}
/// Наибольшая задержка выгрузки; всё, что больше, — «не выгружать».
pub const MODELS_UNLOAD_MAX_S: u32 = 120;

fn default_models_unload_after_s() -> u32 {
    MODELS_UNLOAD_MAX_S + 1
}
fn default_mascot_mode() -> String {
    "commands".into()
}
fn default_mascot_scale() -> u32 {
    100
}

/// Языки, которые Reever умеет распознавать, по порядку показа. Английский — первый
/// и обязательный; новый язык — строка здесь, пакет в `models::PACKS` и фразы команд
/// в грамматике (`grammar::LANGUAGE_TRIGGERS`).
pub const RECOGNITION_LANGUAGES: &[&str] = &[
    "en", "ru", "uk", "de", "fr", "es", "it", "pt", "pl", "cs", "sk", "bg", "hr", "sl", "nl", "da", "sv", "fi", "et", "lv", "lt", "el", "hu", "ro", "mt", "zh", "yue", "ja", "ko",
];

/// Английский и язык Windows, если Reever его распознаёт: пакет для него обычно и
/// скачивают при установке.
fn default_languages() -> Vec<String> {
    let mut languages = vec!["en".to_string()];
    let system = crate::uilang::resolve("auto");
    if system != "en" && RECOGNITION_LANGUAGES.contains(&system) {
        languages.push(system.to_string());
    }
    languages
}

impl Settings {
    /// Включённые языки распознавания в порядке `RECOGNITION_LANGUAGES`: английский
    /// всегда, неизвестные коды отброшены.
    /// Режим маскота с учётом лёгкого интерфейса: в нём маскота нет.
    pub fn effective_mascot_mode(&self) -> &str {
        if self.lite_ui {
            "never"
        } else {
            &self.mascot_mode
        }
    }

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

}

fn default_ui_language() -> String {
    "auto".into()
}

/// Английский и язык Windows, если Reever на нём переведён.
fn default_ui_languages() -> Vec<String> {
    let mut languages = vec!["en".to_string()];
    let system = crate::uilang::resolve("auto");
    if system != "en" {
        languages.push(system.to_string());
    }
    languages
}

impl Settings {
    /// Включённые языки интерфейса по порядку `uilang::SUPPORTED`: английский всегда,
    /// незнакомые коды отброшены.
    pub fn interface_languages(&self) -> Vec<&'static str> {
        crate::uilang::SUPPORTED
            .iter()
            .copied()
            .filter(|code| *code == "en" || self.ui_languages.iter().any(|l| l.trim() == *code))
            .collect()
    }
}

/// Папка, через которую установщик передаёт выбор языков интерфейса
/// (`src-tauri/windows/models.nsh`): `ui-languages-pending.txt` — новый выбор,
/// `ui-languages.txt` — текущие языки, чтобы установщик отметил их при повторном запуске.
fn installer_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("com.troid.reever"))
}

/// ВРЕМЕННО, ДО ФИНАЛЬНОГО РЕЛИЗА (docs/release-todo.md): установщик при каждой установке
/// кладёт `voice-intro-pending.txt`, и программа заново требует 10 «ривер» на шарике, даже
/// если данные не чистили, — чтобы проверять первый запуск глазами нового пользователя.
/// Файл стирается, когда калибровка закончена.
pub fn voice_intro_pending() -> bool {
    installer_dir().is_some_and(|dir| dir.join("voice-intro-pending.txt").is_file())
}

pub fn clear_voice_intro_pending() {
    if let Some(dir) = installer_dir() {
        let _ = std::fs::remove_file(dir.join("voice-intro-pending.txt"));
    }
}

/// Языки интерфейса, отмеченные в установщике после прошлого запуска; файл удаляется.
fn take_installer_ui_languages() -> Option<Vec<String>> {
    let path = installer_dir()?.join("ui-languages-pending.txt");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let languages: Vec<String> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|code| crate::uilang::SUPPORTED.contains(code))
        .map(str::to_string)
        .collect();
    (!languages.is_empty()).then_some(languages)
}

/// Выбор из установщика — в settings.json, текущие языки — для следующего запуска установщика.
fn sync_installer_ui_languages(settings: &mut Settings, settings_path: &std::path::Path) {
    if let Some(languages) = take_installer_ui_languages() {
        settings.ui_languages = languages;
        let mut value = std::fs::read_to_string(settings_path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert("ui_languages".into(), serde_json::json!(settings.ui_languages));
            if let Ok(text) = serde_json::to_string_pretty(&value) {
                let _ = std::fs::write(settings_path, text);
            }
        }
    }
    if let Some(dir) = installer_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("ui-languages.txt"), settings.interface_languages().join(","));
    }
}


/// Галочка «лёгкий интерфейс» из установщика: `ui-style-pending.txt` (`lite` или `full`)
/// переносится в settings.json и удаляется; текущий вид — в `ui-style.txt`, чтобы
/// установщик при повторном запуске показал галочку как есть.
fn sync_installer_ui_style(settings: &mut Settings, settings_path: &std::path::Path) {
    let Some(dir) = installer_dir() else { return };
    let pending = dir.join("ui-style-pending.txt");
    if let Ok(text) = std::fs::read_to_string(&pending) {
        let _ = std::fs::remove_file(&pending);
        let lite = match text.trim() {
            "lite" => Some(true),
            "full" => Some(false),
            _ => None,
        };
        if let Some(lite) = lite.filter(|lite| *lite != settings.lite_ui) {
            settings.lite_ui = lite;
            let mut value = std::fs::read_to_string(settings_path)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("lite_ui".into(), serde_json::json!(lite));
                if let Ok(text) = serde_json::to_string_pretty(&value) {
                    let _ = std::fs::write(settings_path, text);
                }
            }
        }
    }
    write_installer_ui_style(settings.lite_ui);
}

/// Текущий вид интерфейса — для галочки в установщике.
pub fn write_installer_ui_style(lite: bool) {
    if let Some(dir) = installer_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("ui-style.txt"), if lite { "lite" } else { "full" });
    }
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
fn default_activation() -> String {
    "both".into()
}
fn default_hotkey() -> String {
    "Ctrl+Shift+G".into()
}
/// Единственный встроенный набор звуков.
pub const SOUND_PACK: &str = "classic";
/// Прежние встроенные наборы: из %APPDATA% удаляются при запуске, свои наборы остаются.
const RETIRED_SOUND_PACKS: [&str; 3] = ["default", "doresearchedition", "hloppupok"];

fn default_sound_pack() -> String {
    SOUND_PACK.into()
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

    restore!("wake_words", wake_words, Vec<String>);
    restore!("match_threshold", match_threshold, f64);
    restore!("english_enabled", english_enabled, bool);
    restore!("asr_threads", asr_threads, u32);
    restore!("vad_threshold", vad_threshold, f32);
    restore!("wake_pause_ms", wake_pause_ms, u32);
    restore!("command_pause_ms", command_pause_ms, u32);
    restore!("command_wait_ms", command_wait_ms, u32);
    restore!("partial_results", partial_results, bool);
    restore!("cancel_words", cancel_words, Vec<String>);
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
    restore!("models_unload_after_s", models_unload_after_s, u32);
    // Была галочка «Держать модель речи в памяти»: включённая перекрывала ползунок.
    if object.get("keep_asr_loaded").and_then(|v| v.as_bool()) == Some(true) {
        settings.models_unload_after_s = MODELS_UNLOAD_MAX_S + 1;
    }
    restore!("voice_passive", voice_passive, bool);
    restore!("local_index", local_index, bool);
    restore!("dictation_clipboard", dictation_clipboard, bool);
    restore!("dictation_live", dictation_live, bool);
    restore!("dictation_english", dictation_english, bool);
    restore!("dictation_english_level", dictation_english_level, u8);
    restore!("webchat_send", webchat_send, bool);
    restore!("wake_blackbox", wake_blackbox, bool);
    restore!("ui_language", ui_language, String);
    restore!("ui_languages", ui_languages, Vec<String>);
    restore!("languages", languages, Vec<String>);
    restore!("mascot_mode", mascot_mode, String);
    restore!("mascot_scale", mascot_scale, u32);
    restore!("mascot_bubble", mascot_bubble, bool);
    restore!("lite_ui", lite_ui, bool);

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
            wake_words: default_wake_words(),
            match_threshold: default_match_threshold(),
            english_enabled: true,
            asr_threads: default_asr_threads(),
            vad_threshold: default_vad_threshold(),
            wake_pause_ms: default_wake_pause_ms(),
            command_pause_ms: default_command_pause_ms(),
            command_wait_ms: default_command_wait_ms(),
            partial_results: true,
            cancel_words: default_cancel_words(),
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
            dictation_english_level: default_dictation_english_level(),
            webchat_send: true,
            wake_blackbox: false,
            ui_language: default_ui_language(),
            ui_languages: default_ui_languages(),
            languages: default_languages(),
            models_unload_after_s: default_models_unload_after_s(),
            voice_passive: true,
            local_index: true,
            mascot_mode: default_mascot_mode(),
            mascot_scale: default_mascot_scale(),
            mascot_bubble: true,
            lite_ui: false,
        }
    }
}

/// Пути ко всем внешним ресурсам + загруженные настройки/алиасы.
#[derive(Clone)]
pub struct ReeverConfig {
    /// Модели распознавания под включённые языки (models.rs).
    pub models: crate::models::ModelPlan,
    pub model_dirs: crate::models::ModelDirs,
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
        let mut settings = std::fs::read_to_string(&settings_path)
            .ok()
            .map(|s| parse_settings(&s))
            .unwrap_or_default();
        sync_installer_ui_languages(&mut settings, &settings_path);
        sync_installer_ui_style(&mut settings, &settings_path);

        let model_dirs = crate::models::ModelDirs::new(resource(app, "model"));
        let models = crate::models::plan(&settings.recognition_languages(), &model_dirs);

        Ok(Self {
            models,
            model_dirs,
            vad_model_path: resource(app, "model/silero_vad.onnx"),
            anglicisms_path: resource(app, "anglicisms.tsv"),
            sounds_dir: sounds_dir(app),
            settings_path,
            aliases,
            settings,
        })
    }

    /// Русский распознаётся: включён в настройках и его пакет скачан.
    pub fn russian_enabled(&self) -> bool {
        self.models.has("ru")
    }

    /// Словари Windows кириллических языков, которые сейчас распознаются: замене
    /// англицизмов нужен хотя бы один (anglicism.rs).
    pub fn cyrillic_spellcheck(&self) -> Vec<&'static str> {
        crate::anglicism::CYRILLIC_LANGUAGES
            .iter()
            .filter(|(code, _)| self.models.has(code))
            .map(|&(_, tag)| tag)
            .collect()
    }

    /// Путь к звуку активного пака; если в паке файла нет — берём из classic.
    pub fn sound(&self, name: &str) -> PathBuf {
        let pack = if self.settings.sound_pack.trim().is_empty() {
            SOUND_PACK
        } else {
            self.settings.sound_pack.as_str()
        };
        let p = self.sounds_dir.join(pack).join(name);
        if p.exists() {
            p
        } else {
            self.sounds_dir.join(SOUND_PACK).join(name)
        }
    }
}

/// Папка наборов звуков без прежних встроенных наборов; classic досеивается, если его нет.
fn sounds_dir(app: &AppHandle) -> PathBuf {
    let dir = user_config_dir(app, "sounds");
    if cfg!(debug_assertions) {
        return dir;
    }
    for pack in RETIRED_SOUND_PACKS {
        let _ = std::fs::remove_dir_all(dir.join(pack));
    }
    if !dir.join(SOUND_PACK).is_dir() {
        if let Some(seed) = bundled(app, "sounds") {
            let _ = copy_dir_recursive(&seed.join(SOUND_PACK), &dir.join(SOUND_PACK));
        }
    }
    dir
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
        let english = parse_settings(r#"{"languages": ["en"]}"#);
        assert_eq!(english.recognition_languages(), ["en"]);

        // Порядок — как в RECOGNITION_LANGUAGES; пустой список и неизвестные коды — всё
        // равно английский.
        let odd = parse_settings(r#"{"languages": ["zh", "xx", " RU "]}"#);
        assert_eq!(odd.recognition_languages(), ["en", "ru", "zh"]);
        assert_eq!(
            parse_settings(r#"{"languages": []}"#).recognition_languages(),
            ["en"]
        );
    }

    #[test]
    fn old_model_fields_are_ignored() {
        let old = parse_settings(
            r#"{"languages":["ru","en"],"asr_model":"giga-am","second_asr_model":"parakeet-600m","payload_language":"en"}"#,
        );
        assert_eq!(old.recognition_languages(), ["en", "ru"]);
    }
}
