//! Вкладка «Настройки» (#p-settings, GROUPS/fieldEl/renderSettings в frontend/app.js).
//! «Сохранить» нет: каждое изменение применяется сразу; правки подряд склеиваются в одно
//! сохранение (числа — после паузы в наборе: «0.» посреди набора не должно стать порогом).
//! Запись голоса для подстройки (окно поверх) пока не перенесена: здесь — её состояние и сброс.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;

use crate::native::controls::button::ButtonKind;
use crate::native::controls::form::{Form, FormEvent, FormResult, Item};
use crate::native::controls::{fill_round, inside, lcd, ui, Glow, Scene, ACC_4, ACC_HOT};
use crate::native::{Direction, KeyEvent, MouseEvent, Rect, RenderCx, TextStyle};
use crate::ui_text::t;

/// Группы и поля — как GROUPS в app.js (без ещё не перенесённых).
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "interface",
        &[
            ("lite_ui", "lite_ui"),
            ("fps_limit", "fps"),
            ("ui_language", "ui_language"),
            ("ui_languages", "ui_languages"),
        ],
    ),
    ("mascot", &[("mascot_mode", "mascot_mode"), ("mascot_scale", "mascot_scale"), ("mascot_position", "mascot_position"), ("mascot_bubble", "mascot_bubble")]),
    ("activation", &[("activation_mode", "select"), ("voice_adapt", "voice_adapt"), ("voice_passive", "bool"), ("hotkey", "hotkey"), ("mute_hotkey", "hotkey")]),
    ("privacy", &[("private_log", "bool"), ("offline_mode", "bool")]),
    ("languages", &[("languages", "languages")]),
    (
        "recognition",
        &[
            ("asr_threads", "number"),
            ("models_unload_after_s", "unload"),
            ("vad_threshold", "number"),
            ("wake_pause_ms", "number"),
            ("command_pause_ms", "number"),
            ("command_wait_ms", "number"),
            ("partial_results", "bool"),
            ("match_threshold", "number"),
        ],
    ),
    (
        "dictation",
        &[
            ("dictation_clipboard", "bool"),
            ("dictation_live", "bool"),
            ("dictation_english", "bool"),
            ("dictation_english_level", "strength"),
            ("dictation_idle_ms", "number"),
            ("dictation_pause_ms", "number"),
            ("webchat_send", "bool"),
        ],
    ),
    ("search", &[("local_index", "bool")]),
    ("system", &[("confirm_dangerous", "bool"), ("confirm_close", "bool"), ("raise_result", "bool")]),
];

const ACTIVATION: [&str; 3] = ["both", "wake", "hotkey"];
const MASCOT_MODES: [&str; 3] = ["always", "commands", "never"];
const NEVER_UNLOAD: f32 = 121.0;
const CYRILLIC: [&str; 3] = ["ru", "uk", "bg"];

pub fn language_name(code: &str) -> &'static str {
    match code {
        "en" => "English",
        "ru" => "Русский",
        "de" => "Deutsch",
        "es" => "Español",
        "pt" => "Português",
        "it" => "Italiano",
        "fr" => "Français",
        "zh" => "中文",
        "ja" => "日本語",
        "ar" => "العربية",
        "bg" => "Български",
        "hr" => "Hrvatski",
        "cs" => "Čeština",
        "da" => "Dansk",
        "nl" => "Nederlands",
        "et" => "Eesti",
        "fi" => "Suomi",
        "el" => "Ελληνικά",
        "hu" => "Magyar",
        "lv" => "Latviešu",
        "lt" => "Lietuvių",
        "mt" => "Malti",
        "pl" => "Polski",
        "ro" => "Română",
        "sk" => "Slovenčina",
        "sl" => "Slovenščina",
        "sv" => "Svenska",
        "uk" => "Українська",
        "ko" => "한국어",
        "yue" => "粵語",
        _ => "",
    }
}

/// Что вкладка просит у программы.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsOut {
    /// Изменённые ключи settings.json (save_settings).
    Patch(serde_json::Map<String, Value>),
    LiteUi(bool),
    Autostart(bool),
    Hotkey { mute: bool, combo: String, mode: String },
    Mascot { mode: String, scale: u32, bubble: bool },
    UiLanguage(String),
    /// Языки интерфейса (ui_languages); `language` — выбранный язык, если его убрали — `auto`.
    UiLanguages { codes: Vec<String>, language: String },
    Fps(u32),
    MascotPlace,
    MascotPin(bool),
    MascotUnpin,
    VoiceReset,
    /// открыть окно записи голоса
    VoiceStart,
    /// «Что можно сказать» → Изменить
    OpenGrammar,
}

/// Сведения для вкладки, которые даёт программа.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsInfo {
    pub settings: Value,
    pub autostart: bool,
    pub sound_packs: Vec<String>,
    /// языки интерфейса, из которых можно выбрать (английский всегда)
    pub ui_languages: Vec<String>,
    /// во что раскрывается «как в Windows»
    pub auto_language: String,
    /// language_models: { languages: код → пакет, installed: [пакеты] }
    pub models: Value,
    /// voice_adapt_status
    pub voice: Value,
}

pub struct SettingsTab {
    info: SettingsInfo,
    /// значения, ещё не пришедшие обратно в `info` (чтобы форма не моргала старым)
    local: BTreeMap<String, Value>,
    patch: serde_json::Map<String, Value>,
    flush_at: Option<Instant>,
    recording: Option<String>,
    hotkey_note: Option<(String, String)>,
    /// раскрытое меню «+ Добавить язык» (`rl` — распознавания, `ul` — интерфейса)
    menu: Option<&'static str>,
    /// маскот расставляется мышью
    pub placing: bool,
    pub msg: String,
    pub form: Form,
    scroll: f32,
    max_scroll: f32,
    view: Rect,
    /// элемент в фокусе, к которому уже прокрутили
    revealed: Option<(i32, i32)>,
    locale: String,
    rtl: bool,
}

impl SettingsTab {
    pub fn new(locale: &str, rtl: bool, hwnd: isize) -> Self {
        Self {
            info: SettingsInfo::default(),
            local: BTreeMap::new(),
            patch: serde_json::Map::new(),
            flush_at: None,
            recording: None,
            hotkey_note: None,
            menu: None,
            placing: false,
            msg: String::new(),
            form: Form::new(hwnd, rtl),
            scroll: 0.0,
            max_scroll: 0.0,
            view: Rect::new(0.0, 0.0, 0.0, 0.0),
            revealed: None,
            locale: locale.to_string(),
            rtl,
        }
    }

    pub fn set_hwnd(&mut self, hwnd: isize) {
        self.form = Form::new(hwnd, self.rtl);
        self.rebuild();
    }

    fn t(&self, key: &str) -> String {
        t(&self.locale, key, &[])
    }

    fn has(&self, key: &str) -> bool {
        self.t(key) != key
    }

    pub fn sync(&mut self, info: SettingsInfo) {
        if info == self.info {
            return;
        }
        // пришедшее совпало с локальным — локальное больше не нужно
        if let Some(obj) = info.settings.as_object() {
            self.local.retain(|k, v| obj.get(k) != Some(v));
        }
        self.info = info;
        self.rebuild();
    }

    fn get(&self, key: &str) -> Value {
        self.local.get(key).cloned().or_else(|| self.info.settings.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn flag(&self, key: &str) -> bool {
        match self.get(key) {
            Value::Bool(b) => b,
            Value::Null => key == "mascot_bubble",
            _ => false,
        }
    }

    fn num(&self, key: &str) -> Option<f64> {
        self.get(key).as_f64()
    }

    fn text_of(&self, key: &str) -> String {
        match self.get(key) {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    }

    fn set_local(&mut self, key: &str, v: Value) {
        self.local.insert(key.to_string(), v);
    }

    /// Сохранить ключ: склеивается с соседними правками, уходит через `delay`.
    fn save(&mut self, key: &str, v: Value, delay: Duration) {
        self.set_local(key, v.clone());
        self.patch.insert(key.to_string(), v);
        let at = Instant::now() + delay;
        self.flush_at = Some(self.flush_at.map_or(at, |old| old.max(at)));
    }

    pub fn wake_at(&self) -> Option<Instant> {
        self.flush_at
    }

    /// Пора отправить накопленное.
    pub fn take_due(&mut self, now: Instant) -> Option<SettingsOut> {
        if self.flush_at.is_some_and(|t| now >= t) {
            self.flush_at = None;
            let patch = std::mem::take(&mut self.patch);
            if !patch.is_empty() {
                return Some(SettingsOut::Patch(patch));
            }
        }
        None
    }

    fn recognition_languages(&self) -> Vec<String> {
        let mut out = vec!["en".to_string()];
        if let Some(a) = self.get("languages").as_array() {
            out.extend(a.iter().filter_map(|v| v.as_str()).map(|s| s.trim().to_lowercase()));
        }
        out
    }

    fn label(&self, key: &str) -> String {
        self.t(&format!("settings.{key}"))
    }

    fn help(&self, key: &str) -> Option<String> {
        let k = format!("settings.{key}.help");
        self.has(&k).then(|| self.t(&k))
    }

    fn unload_text(&self, v: f32) -> String {
        if v >= NEVER_UNLOAD {
            self.t("settings.models_unload_after_s.never")
        } else if v <= 0.0 {
            self.t("settings.models_unload_after_s.now")
        } else {
            t(&self.locale, "settings.models_unload_after_s.seconds", &[("n", &(v as i64).to_string())])
        }
    }

    fn field(&self, items: &mut Vec<Item>, key: &str, kind: &str) {
        let help = self.help(key);
        match kind {
            "bool" | "lite_ui" | "mascot_bubble" => {
                items.push(Item::Check { id: format!("cb:{key}"), label: self.label(key), on: self.flag(key) });
                if let Some(h) = &help {
                    items.push(Item::Help { text: h.clone(), indent: true });
                }
                if key == "dictation_english" && !self.recognition_languages().iter().any(|l| CYRILLIC.contains(&l.as_str())) {
                    items.push(Item::Help { text: self.t("settings.dictation_english.inactive"), indent: true });
                }
                return;
            }
            _ => {}
        }
        items.push(Item::Label(self.label(key)));
        match kind {
            "fps" => {
                let cur = self.num(key).unwrap_or(0.0) as u32;
                let options = [0, 60, 30].iter().map(|v| (v.to_string(), self.t(&format!("settings.fps_limit.{v}")))).collect();
                items.push(Item::Select { id: key.into(), options, selected: cur.to_string() });
            }
            "ui_language" => {
                let cur = self.text_of(key);
                let auto = format!("{} ({})", self.t("settings.ui_language.auto"), language_name(&self.info.auto_language));
                let mut options = vec![("auto".to_string(), auto)];
                options.extend(self.info.ui_languages.iter().map(|c| (c.clone(), language_name(c).to_string())));
                let selected = if self.info.ui_languages.contains(&cur) { cur } else { "auto".into() };
                items.push(Item::Select { id: key.into(), options, selected });
            }
            "mascot_mode" => {
                let cur = self.text_of(key);
                let options = MASCOT_MODES.iter().map(|m| (m.to_string(), self.t(&format!("settings.mascot_mode.{m}")))).collect();
                items.push(Item::Select { id: key.into(), options, selected: if cur.is_empty() { "always".into() } else { cur } });
            }
            "select" => {
                let cur = self.text_of(key);
                let options = ACTIVATION
                    .iter()
                    .map(|o| {
                        let k = format!("settings.{key}.{o}");
                        let label = if self.has(&k) { self.t(&k) } else if self.has(&format!("mode.{o}")) { self.t(&format!("mode.{o}")) } else { o.to_string() };
                        (o.to_string(), label)
                    })
                    .collect();
                items.push(Item::Select { id: key.into(), options, selected: if cur.is_empty() { "both".into() } else { cur } });
            }
            "mascot_scale" => {
                let v = self.num(key).unwrap_or(100.0).clamp(100.0, 500.0) as f32;
                items.push(Item::Slider { id: key.into(), min: 100.0, max: 500.0, step: 25.0, value: v, text: format!("{}%", v as i64) });
            }
            "strength" => {
                let v = self.num(key).unwrap_or(3.0).round().clamp(1.0, 5.0) as f32;
                items.push(Item::Slider { id: key.into(), min: 1.0, max: 5.0, step: 1.0, value: v, text: format!("{} / 5", v as i64) });
                items.push(Item::Help { text: self.t(&format!("settings.{key}.{}", v as i64)), indent: false });
                return;
            }
            "unload" => {
                let v = self.num(key).map_or(NEVER_UNLOAD, |n| n.round() as f32).clamp(0.0, NEVER_UNLOAD);
                items.push(Item::Slider { id: key.into(), min: 0.0, max: NEVER_UNLOAD, step: 1.0, value: v, text: self.unload_text(v) });
            }
            "languages" => {
                self.languages_field(items);
                if let Some(h) = help {
                    items.push(Item::Help { text: h, indent: false });
                }
                return;
            }
            "ui_languages" => {
                self.ui_languages_field(items);
                return;
            }
            "mascot_position" => {
                let pinned = !self.get(key).is_null();
                let mut buttons = vec![(
                    "mpplace".to_string(),
                    self.t(if self.placing { "settings.mascot_position.save" } else { "settings.mascot_position.place" }),
                    ButtonKind::Plain,
                )];
                if !self.placing && pinned {
                    buttons.push(("mpreset".into(), self.t("settings.mascot_position.reset"), ButtonKind::Plain));
                }
                items.push(Item::Buttons { buttons, top: 0.0, bottom: 0.0 });
                let about = if self.placing {
                    "settings.mascot_position.placing"
                } else if pinned {
                    "settings.mascot_position.pinned"
                } else {
                    "settings.mascot_position.corner"
                };
                items.push(Item::Help { text: self.t(about), indent: false });
                return;
            }
            "voice_adapt" => {
                let v = &self.info.voice;
                let report = v.get("report").filter(|r| !r.is_null());
                let passive = v.get("passive").filter(|p| !p.is_null());
                let n = |x: Option<&Value>, k: &str| x.and_then(|x| x.get(k)).and_then(Value::as_i64).unwrap_or(0).to_string();
                let mut text = match report {
                    Some(r) => t(
                        &self.locale,
                        "settings.voice_adapt.on",
                        &[("heard", &n(Some(r), "heard_after")), ("words", &n(Some(r), "words")), ("before", &n(Some(r), "heard_before"))],
                    ),
                    None => self.t("settings.voice_adapt.off"),
                };
                if passive.is_some() && self.get("voice_passive") != Value::Bool(false) {
                    let (count, goal) = (n(passive, "collected"), n(passive, "goal"));
                    let done = count.parse::<i64>().unwrap_or(0) >= goal.parse::<i64>().unwrap_or(1);
                    text.push(' ');
                    text.push_str(&if done {
                        t(&self.locale, "settings.voice_passive.done", &[("goal", &goal)])
                    } else {
                        t(&self.locale, "settings.voice_passive.progress", &[("count", &count), ("goal", &goal)])
                    });
                }
                let can_reset = report.is_some() || n(passive, "collected") != "0";
                let id = if can_reset { "vareset" } else { "!vareset" };
                items.push(Item::Buttons {
                    buttons: vec![("vastart".into(), self.t("settings.voice_adapt.start"), ButtonKind::Plain), (id.into(), self.t("settings.voice_adapt.reset"), ButtonKind::Plain)],
                    top: 4.0,
                    bottom: 0.0,
                });
                items.push(Item::Help { text, indent: false });
                if let Some(h) = help {
                    items.push(Item::Help { text: h, indent: false });
                }
                return;
            }
            "hotkey" => {
                let rec = self.recording.as_deref() == Some(key);
                let label = match (&self.hotkey_note, rec) {
                    (Some((k, note)), true) if k == key => note.clone(),
                    (_, true) => self.t("hotkey.press"),
                    _ => {
                        let v = self.text_of(key);
                        if v.is_empty() { self.t("hotkey.none") } else { v }
                    }
                };
                let kind = if rec { ButtonKind::Default } else { ButtonKind::Plain };
                items.push(Item::Buttons { buttons: vec![(format!("hk:{key}"), label, kind)], top: 0.0, bottom: 0.0 });
            }
            _ => {
                items.push(Item::Input { id: format!("in:{key}"), placeholder: String::new(), value: self.text_of(key) });
            }
        }
        if let Some(h) = help {
            items.push(Item::Help { text: h, indent: false });
        }
    }

    fn pack_name(pack: &str) -> &str {
        match pack {
            "european" => "Parakeet v3",
            "asian" => "SenseVoice",
            p => p,
        }
    }

    /// Языки распознавания (languagesEditor): строка на язык с его пакетом, «+ Добавить язык».
    fn languages_field(&self, items: &mut Vec<Item>) {
        let enabled = self.recognition_enabled();
        let packs = self.info.models.get("languages");
        let installed: Vec<&str> = self.info.models.get("installed").and_then(Value::as_array).map_or(Vec::new(), |a| a.iter().filter_map(Value::as_str).collect());
        let pack_of = |c: &str| packs.and_then(|p| p.get(c)).and_then(Value::as_str).unwrap_or("").to_string();
        let ready = |c: &str| installed.contains(&pack_of(c).as_str());
        for code in &enabled {
            let (note, missing, removable) = if code == "en" {
                (self.t("settings.languages.always"), false, false)
            } else if ready(code) {
                (Self::pack_name(&pack_of(code)).to_string(), false, true)
            } else {
                (self.t("settings.languages.notDownloaded"), true, true)
            };
            items.push(Item::LangRow { id: format!("rl:{code}"), name: language_name(code).into(), note, missing, removable });
        }
        let addable: Vec<&str> = crate::config::RECOGNITION_LANGUAGES.iter().copied().filter(|c| !enabled.iter().any(|e| e == c)).collect();
        let add = format!("+ {}", self.t("settings.languages.add"));
        items.push(Item::Buttons { buttons: vec![(if addable.is_empty() { "!addrl" } else { "addrl" }.into(), add, ButtonKind::Plain)], top: 0.0, bottom: 0.0 });
        if self.menu == Some("rl") {
            let buttons = addable
                .iter()
                .map(|c| {
                    let ok = ready(c);
                    let label = if ok { language_name(c).to_string() } else { format!("{} - {}", language_name(c), self.t("settings.languages.notDownloaded")) };
                    (format!("{}rl+:{c}", if ok { "" } else { "!" }), label, ButtonKind::Plain)
                })
                .collect();
            items.push(Item::Buttons { buttons, top: 6.0, bottom: 0.0 });
        }
        if addable.is_empty() {
            items.push(Item::Help { text: self.t("settings.languages.noMore"), indent: false });
        }
        let mut used: Vec<String> = enabled.iter().filter(|c| ready(c)).map(|c| pack_of(c)).collect();
        used.sort();
        used.dedup();
        if used.len() > 1 && !installed.contains(&"lid") {
            items.push(Item::Help { text: self.t("settings.languages.needLid"), indent: false });
        }
        if addable.iter().any(|c| !ready(c)) {
            items.push(Item::Help { text: self.t("settings.languages.installer"), indent: false });
        }
    }

    /// Включённые языки распознавания по порядку RECOGNITION_LANGUAGES, английский — всегда.
    fn recognition_enabled(&self) -> Vec<String> {
        let chosen = self.recognition_languages();
        crate::config::RECOGNITION_LANGUAGES.iter().filter(|c| **c == "en" || chosen.iter().any(|x| x == *c)).map(|c| c.to_string()).collect()
    }

    /// Языки интерфейса (uiLanguagesEditor).
    fn ui_languages_field(&self, items: &mut Vec<Item>) {
        let enabled = &self.info.ui_languages;
        for code in enabled {
            let en = code == "en";
            items.push(Item::LangRow {
                id: format!("ul:{code}"),
                name: language_name(code).into(),
                note: if en { self.t("settings.languages.always") } else { String::new() },
                missing: false,
                removable: !en,
            });
        }
        let addable: Vec<&str> = crate::uilang::SUPPORTED.iter().copied().filter(|c| !enabled.iter().any(|e| e == c)).collect();
        let add = format!("+ {}", self.t("settings.languages.add"));
        items.push(Item::Buttons { buttons: vec![(if addable.is_empty() { "!addul" } else { "addul" }.into(), add, ButtonKind::Plain)], top: 0.0, bottom: 0.0 });
        if self.menu == Some("ul") {
            let buttons = addable.iter().map(|c| (format!("ul+:{c}"), language_name(c).to_string(), ButtonKind::Plain)).collect();
            items.push(Item::Buttons { buttons, top: 6.0, bottom: 0.0 });
        }
    }

    fn set_ui_languages(&mut self, codes: Vec<String>) -> Option<SettingsOut> {
        let current = {
            let c = self.text_of("ui_language");
            if c.is_empty() { "auto".to_string() } else { c }
        };
        let language = if current != "auto" && !codes.contains(&current) { "auto".to_string() } else { current };
        self.info.ui_languages = crate::uilang::SUPPORTED.iter().filter(|c| **c == "en" || codes.iter().any(|x| x == *c)).map(|c| c.to_string()).collect();
        self.rebuild();
        Some(SettingsOut::UiLanguages { codes, language })
    }

    fn rebuild(&mut self) {
        let mut items = vec![Item::Link {
            id: "grammar".into(),
            prefix: self.t("settings.grammarLinkPrefix"),
            link: self.t("settings.grammarLink"),
            suffix: self.t("settings.grammarLinkSuffix"),
        }];
        items.push(Item::Gap(14.0));
        for (group, fields) in GROUPS {
            items.push(Item::GroupBegin { legend: self.t(&format!("settings.group.{group}")), icon: None });
            for (key, kind) in fields.iter() {
                self.field(&mut items, key, kind);
            }
            items.push(Item::GroupEnd);
        }
        items.push(Item::GroupBegin { legend: self.t("settings.group.os"), icon: None });
        items.push(Item::Check { id: "autostart".into(), label: self.label("autostart"), on: self.info.autostart });
        items.push(Item::Label(self.label("sound_pack")));
        let cur = self.text_of("sound_pack");
        let options = self.info.sound_packs.iter().map(|p| (p.clone(), p.clone())).collect();
        items.push(Item::Select { id: "sound_pack".into(), options, selected: if cur.is_empty() { "classic".into() } else { cur } });
        items.push(Item::GroupEnd);
        self.form.set_items(items);
    }

    fn dir(&self) -> Direction {
        if self.rtl { Direction::Rtl } else { Direction::Ltr }
    }

    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, body: Rect) {
        let clip = Rect::new(body.x - 18.0, body.y - 14.0, body.w + 36.0, body.h + 14.0 + 18.0);
        self.view = clip;
        scene.flush(cx);
        scene.set_clip(Some(clip));
        unsafe { cx.dc().PushAxisAlignedClip(&clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        let mut y = body.y - self.scroll;
        // .h1: margin 2 0 12
        let title = self.t("nav.settings").to_uppercase();
        if let Some(l) = scene.cache.get(cx.text(), &title, &lcd(22.0, 600, 0.08), body.w) {
            scene.text(&l, body.x, y + 2.0, ACC_HOT, Glow::G3);
            y += 2.0 + l.height + 12.0;
        }
        let h = self.form.draw(scene, cx, body.x, y, body.w);
        y += h;
        // фокус ушёл на другой элемент (Tab) — прокрутить к нему
        if let Some(r) = self.form.focus_rect() {
            let key = ((r.x) as i32, (r.y + self.scroll) as i32);
            if self.revealed != Some(key) {
                self.revealed = Some(key);
                self.scroll = (self.scroll + super::reveal_delta(r, body)).max(0.0);
            }
        }
        // .msg: min-height 18, margin-top 8
        y += 8.0;
        if !self.msg.is_empty() {
            let st = TextStyle { wrap: true, direction: self.dir(), ..ui(13.0, 400, 1.45) };
            if let Some(l) = scene.cache.get(cx.text(), &self.msg, &st, body.w) {
                scene.text(&l, body.x, y, ACC_HOT, Glow::G2);
            }
        }
        y += 18.0;
        self.max_scroll = (y + self.scroll - body.y - body.h).max(0.0);
        self.scroll = self.scroll.min(self.max_scroll);
        scene.flush(cx);
        scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
        if self.max_scroll > 0.0 {
            // полоса прокрутки — у конца строки (слева в dir="rtl", как в Chromium)
            let track = crate::native::controls::mirror(clip, Rect::new(clip.x + clip.w - 7.0, body.y, 3.0, body.h), self.rtl);
            let total = body.h + self.max_scroll;
            let th = (track.h * body.h / total).max(24.0);
            let ty = track.y + (track.h - th) * self.scroll / self.max_scroll;
            fill_round(cx, Rect::new(track.x, ty, track.w, th), 1.5, ACC_4);
        }
    }

    pub fn set_placing(&mut self, on: bool) {
        self.placing = on;
        self.rebuild();
    }

    /// Прокрутить на `d` DIP (проверка; граница — после следующего кадра).
    pub fn scroll_by(&mut self, d: f32) {
        self.scroll = (self.scroll + d).max(0.0);
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        if !inside(self.view, x, y) || self.max_scroll <= 0.0 {
            return false;
        }
        let next = (self.scroll - delta * 48.0).clamp(0.0, self.max_scroll);
        let moved = next != self.scroll;
        self.scroll = next;
        moved
    }

    pub fn mouse(&mut self, ev: MouseEvent) -> (bool, Option<SettingsOut>) {
        let r = self.form.mouse(ev);
        self.form_result(r)
    }

    /// Кликабельное (рука), поле (текстовый курсор) или ничего.
    pub fn cursor(&self, x: f32, y: f32) -> Option<bool> {
        self.form.hover_kind(x, y)
    }

    /// Действие диктора над элементом формы настроек.
    pub fn a11y_action(&mut self, base: u64, req: &accesskit::ActionRequest) -> Option<(bool, Option<SettingsOut>)> {
        let r = self.form.a11y_action(base, req)?;
        Some(self.form_result(r))
    }

    pub fn recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn key(&mut self, ev: KeyEvent) -> (bool, Option<SettingsOut>) {
        if let Some(key) = self.recording.clone() {
            return (true, self.record(&key, ev));
        }
        let r = self.form.key(ev);
        self.form_result(r)
    }

    pub fn char_input(&mut self, c: char) -> (bool, Option<SettingsOut>) {
        if self.recording.is_some() {
            return (false, None);
        }
        let r = self.form.char_input(c);
        self.form_result(r)
    }

    /// Запись сочетания (как onKey в app.js): Esc — отмена, нужен модификатор, кроме F-клавиш.
    fn record(&mut self, key: &str, ev: KeyEvent) -> Option<SettingsOut> {
        let k = ev.key;
        if k == VK_ESCAPE.0 as u32 {
            self.recording = None;
            self.hotkey_note = None;
            self.rebuild();
            return None;
        }
        // одни модификаторы — ждём основную клавишу
        if matches!(k, 0x10 | 0x11 | 0x12 | 0xA0..=0xA5 | 0x5B | 0x5C) {
            return None;
        }
        let main = match k {
            0x41..=0x5A => char::from_u32(k).map(|c| c.to_string()),
            0x30..=0x39 => char::from_u32(k).map(|c| c.to_string()),
            0x70..=0x87 => Some(format!("F{}", k - 0x6F)),
            0x20 => Some("Space".into()),
            0x0D => Some("Enter".into()),
            0x09 => Some("Tab".into()),
            0x25 => Some("Left".into()),
            0x26 => Some("Up".into()),
            0x27 => Some("Right".into()),
            0x28 => Some("Down".into()),
            _ => None,
        }?;
        let mut mods = Vec::new();
        if ev.ctrl {
            mods.push("Ctrl");
        }
        if ev.alt {
            mods.push("Alt");
        }
        if ev.shift {
            mods.push("Shift");
        }
        if mods.is_empty() && !main.starts_with('F') {
            self.hotkey_note = Some((key.to_string(), self.t("hotkey.needModifier")));
            self.rebuild();
            return None;
        }
        let combo = mods.into_iter().map(String::from).chain(std::iter::once(main)).collect::<Vec<_>>().join("+");
        self.recording = None;
        self.hotkey_note = None;
        self.set_local(key, Value::String(combo.clone()));
        self.rebuild();
        let mode = {
            let m = self.text_of("activation_mode");
            if m.is_empty() { "both".to_string() } else { m }
        };
        Some(SettingsOut::Hotkey { mute: key == "mute_hotkey", combo, mode })
    }

    fn form_result(&mut self, r: FormResult) -> (bool, Option<SettingsOut>) {
        let redraw = r.redraw;
        let Some(ev) = r.event else { return (redraw, None) };
        let out = self.on_event(ev);
        (true, out)
    }

    fn mascot_out(&self) -> SettingsOut {
        let mode = self.text_of("mascot_mode");
        SettingsOut::Mascot {
            mode: if mode.is_empty() { "always".into() } else { mode },
            scale: self.num("mascot_scale").unwrap_or(100.0) as u32,
            bubble: self.flag("mascot_bubble"),
        }
    }

    fn on_event(&mut self, ev: FormEvent) -> Option<SettingsOut> {
        match ev {
            FormEvent::Click(id) if id == "grammar" => Some(SettingsOut::OpenGrammar),
            FormEvent::Click(id) if id == "addrl" || id == "addul" => {
                let m = if id == "addrl" { "rl" } else { "ul" };
                self.menu = if self.menu == Some(m) { None } else { Some(m) };
                self.rebuild();
                None
            }
            FormEvent::Click(id) if id.starts_with("rl+:") => {
                let mut codes = self.recognition_enabled();
                codes.push(id[4..].to_string());
                self.menu = None;
                self.save("languages", Value::from(codes), Duration::from_millis(300));
                self.rebuild();
                None
            }
            FormEvent::Click(id) if id.starts_with("ul+:") => {
                let mut codes = self.info.ui_languages.clone();
                codes.push(id[4..].to_string());
                self.menu = None;
                self.set_ui_languages(codes)
            }
            FormEvent::Click(id) if id == "mpplace" => {
                let out = if self.placing { SettingsOut::MascotPin(true) } else { SettingsOut::MascotPlace };
                self.placing = !self.placing;
                self.rebuild();
                Some(out)
            }
            FormEvent::Click(id) if id == "mpreset" => {
                self.set_local("mascot_position", Value::Null);
                self.rebuild();
                Some(SettingsOut::MascotUnpin)
            }
            FormEvent::Click(id) if id == "vareset" => Some(SettingsOut::VoiceReset),
            FormEvent::Click(id) if id == "vastart" => Some(SettingsOut::VoiceStart),
            FormEvent::Remove(id, _) if id.starts_with("rl:") => {
                let code = &id[3..];
                let codes: Vec<String> = self.recognition_enabled().into_iter().filter(|c| c != code).collect();
                self.save("languages", Value::from(codes), Duration::from_millis(300));
                self.rebuild();
                None
            }
            FormEvent::Remove(id, _) if id.starts_with("ul:") => {
                let code = &id[3..];
                let codes: Vec<String> = self.info.ui_languages.iter().filter(|c| *c != code).cloned().collect();
                self.set_ui_languages(codes)
            }
            FormEvent::Click(id) => {
                let key = id.strip_prefix("hk:")?.to_string();
                self.recording = if self.recording.as_deref() == Some(key.as_str()) { None } else { Some(key) };
                self.hotkey_note = None;
                self.rebuild();
                None
            }
            FormEvent::Toggle(id) if id == "autostart" => {
                self.info.autostart = !self.info.autostart;
                self.rebuild();
                Some(SettingsOut::Autostart(self.info.autostart))
            }
            FormEvent::Toggle(id) => {
                let key = id.strip_prefix("cb:")?.to_string();
                let on = !self.flag(&key);
                self.set_local(&key, Value::Bool(on));
                self.rebuild();
                match key.as_str() {
                    "lite_ui" => Some(SettingsOut::LiteUi(on)),
                    "mascot_bubble" => Some(self.mascot_out()),
                    _ => {
                        self.save(&key, Value::Bool(on), Duration::from_millis(300));
                        None
                    }
                }
            }
            FormEvent::Select(id, v) => {
                self.set_local(&id, Value::String(v.clone()));
                let out = match id.as_str() {
                    "fps_limit" => {
                        let n: u32 = v.parse().unwrap_or(0);
                        self.set_local(&id, Value::from(n));
                        Some(SettingsOut::Fps(n))
                    }
                    "ui_language" => Some(SettingsOut::UiLanguage(v)),
                    "mascot_mode" => Some(self.mascot_out()),
                    _ => {
                        self.save(&id, Value::String(v), Duration::from_millis(300));
                        None
                    }
                };
                self.rebuild();
                out
            }
            FormEvent::Slide(id, v) => {
                // подпись меняется на ходу, сохраняется — когда отпустили
                self.set_local(&id, Value::from(v as i64));
                self.rebuild();
                None
            }
            FormEvent::SlideEnd(id, v) => self.slide_end(&id, v),
            FormEvent::Edited(id, text) => {
                let key = id.strip_prefix("in:")?.to_string();
                let s = text.trim();
                // как onchange в вебе: пустое и недописанное число не сохраняется
                if s.is_empty() || s.ends_with('.') || s.ends_with('-') {
                    return None;
                }
                let n: f64 = s.parse().ok().filter(|n: &f64| n.is_finite())?;
                let v = if n.fract() == 0.0 && !s.contains('.') { Value::from(n as i64) } else { Value::from(n) };
                self.save(&key, v, Duration::from_millis(900));
                None
            }
            _ => None,
        }
    }

    /// Отпустили ползунок: сохранить (размер маскота — через set_mascot).
    pub fn slide_end(&mut self, id: &str, v: f32) -> Option<SettingsOut> {
        self.set_local(id, Value::from(v as i64));
        self.rebuild();
        if id == "mascot_scale" {
            return Some(self.mascot_out());
        }
        self.save(id, Value::from(v as i64), Duration::from_millis(100));
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> SettingsTab {
        let mut t = SettingsTab::new("en", false, 0);
        t.sync(SettingsInfo {
            settings: serde_json::json!({ "vad_threshold": 0.5, "hotkey": "Ctrl+Shift+G", "activation_mode": "both", "languages": ["en"] }),
            autostart: false,
            sound_packs: vec!["classic".into()],
            ui_languages: vec!["en".into(), "ru".into()],
            auto_language: "en".into(),
            ..Default::default()
        });
        t
    }

    fn key(k: u32, ctrl: bool) -> KeyEvent {
        KeyEvent { key: k, ctrl, shift: false, alt: false, repeat: false }
    }

    #[test]
    fn number_fields_save_after_typing_and_skip_unfinished_values() {
        let mut t = tab();
        assert_eq!(t.on_event(FormEvent::Edited("in:vad_threshold".into(), "0.".into())), None);
        assert!(t.patch.is_empty());
        t.on_event(FormEvent::Edited("in:vad_threshold".into(), "0.35".into()));
        assert_eq!(t.patch.get("vad_threshold"), Some(&serde_json::json!(0.35)));
        assert_eq!(t.take_due(Instant::now()), None);
        let out = t.take_due(Instant::now() + Duration::from_secs(2));
        assert!(matches!(out, Some(SettingsOut::Patch(p)) if p.len() == 1));
    }

    #[test]
    fn hotkey_recording_needs_a_modifier_and_escape_cancels() {
        let mut t = tab();
        t.on_event(FormEvent::Click("hk:hotkey".into()));
        assert!(t.recording());
        assert_eq!(t.record("hotkey", key(0x41, false)), None);
        assert!(t.recording());
        let out = t.record("hotkey", key(0x41, true));
        assert_eq!(out, Some(SettingsOut::Hotkey { mute: false, combo: "Ctrl+A".into(), mode: "both".into() }));
        t.on_event(FormEvent::Click("hk:mute_hotkey".into()));
        t.record("mute_hotkey", key(VK_ESCAPE.0 as u32, false));
        assert!(!t.recording());
    }

    #[test]
    fn unload_slider_ends_with_never() {
        let t = tab();
        assert_eq!(t.unload_text(121.0), t.t("settings.models_unload_after_s.never"));
        assert_eq!(t.unload_text(0.0), t.t("settings.models_unload_after_s.now"));
    }

    #[test]
    fn recognition_language_without_a_pack_cannot_be_added() {
        let mut t = tab();
        t.info.models = serde_json::json!({ "languages": { "ru": "european", "ja": "asian" }, "installed": ["european"] });
        t.on_event(FormEvent::Click("addrl".into()));
        t.rebuild();
        t.on_event(FormEvent::Click("rl+:ru".into()));
        assert_eq!(t.patch.get("languages"), Some(&serde_json::json!(["en", "ru"])));
        t.on_event(FormEvent::Remove("rl:ru".into(), 0));
        assert_eq!(t.patch.get("languages"), Some(&serde_json::json!(["en"])));
    }

    #[test]
    fn removing_the_current_interface_language_falls_back_to_auto() {
        let mut t = tab();
        t.set_local("ui_language", Value::String("ru".into()));
        let out = t.on_event(FormEvent::Remove("ul:ru".into(), 0));
        assert_eq!(out, Some(SettingsOut::UiLanguages { codes: vec!["en".into()], language: "auto".into() }));
    }

    #[test]
    fn switches_go_to_their_commands() {
        let mut t = tab();
        assert_eq!(t.on_event(FormEvent::Toggle("cb:lite_ui".into())), Some(SettingsOut::LiteUi(true)));
        assert_eq!(t.on_event(FormEvent::Toggle("cb:private_log".into())), None);
        assert!(t.patch.contains_key("private_log"));
    }
}
