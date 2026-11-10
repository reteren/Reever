//! Нативное главное окно (этап 4 docs/native-ui-roadmap.md): перенос прежних frontend/index.html +
//! skin/shell.js на ядро `native/` в общем с маскотом и диалогами потоке окон: каркас (рамка с
//! шейдером, форма окна, лунки, перетаскивание), экраны состояния, рельс вкладок, бегущая строка,
//! эквалайзер, жидкая линза и все вкладки.
//!
//! Жизненный цикл: «в трей» окно уничтожается целиком, показ —
//! создаёт его заново по снимку состояния (`events::get_ui_snapshot`). События распознавания
//! окно слушает само через `AppHandle::listen_any`, подписка — при старте программы (`init`).

mod actions_tab;
mod frame;
mod fx;
mod grammar_edit;
mod overlays;
mod say;
mod settings_tab;
mod shade;
mod text_tab;
mod view;

use std::sync::{Arc, Mutex, Once, OnceLock};

use crate::app::AppHandle;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::native::{self, WindowId, WindowSpec};
pub use view::Msg;
use view::{skin_h, skin_w, Info, MainView};

static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);
/// Окно создаётся (между `show` и записью в `WIN`): второй `show` в это время — двойной щелчок
/// по значку в трее — создал бы второе окно, которого программа не знает (его кнопки и
/// «Закрыть окно» ничего не делали бы).
static CREATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LISTEN: Once = Once::new();
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Что окно просит сделать (кнопки, вкладки) — выполняется вне потока окон.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Minimize,
    Tray,
    TogglePause,
    Tab(String),
    SaveGrammar(crate::grammar::Grammar),
    ResetGrammar,
    SaveAction(crate::actions::Action),
    DeleteAction(String),
    Settings(settings_tab::SettingsOut),
    Calibrate(crate::actions::Action),
    CancelCalibration,
    ListApps,
    VoiceStart,
    VoiceCancel,
    /// `intro` — обязательная подстройка первого запуска
    VoiceFinish(bool),
    Scan,
    AddScanned(Vec<overlays::ScanApp>),
    /// компактный режим: полоска поверх всех окон
    Shade(bool),
    /// вкладка «Текст»
    Text(text_tab::TextOut),
    /// кнопка «Индекс»: окно поиска по индексу с пустым полем
    Index,
}

pub fn is_open() -> bool {
    WIN.lock().ok().is_some_and(|w| w.is_some())
}

/// Подписаться на события распознавания. Звать при старте программы, до первых событий
/// (см. bubble_native::init: поздняя подписка получает событие не по порядку).
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    LISTEN.call_once(|| {
        let text = |payload: &str| serde_json::from_str::<String>(payload).unwrap_or_default();
        app.listen_any("reever://state", move |e| send(Msg::State(text(e.payload()))));
        app.listen_any("reever://partial", move |e| send(Msg::Partial(text(e.payload()))));
        app.listen_any("reever://heard", move |e| send(Msg::Heard(text(e.payload()))));
        app.listen_any("reever://log", move |e| send(Msg::Log(text(e.payload()))));
        app.listen_any("reever://wake", |_| send(Msg::Wake(true)));
        app.listen_any("reever://wake-rejected", |_| send(Msg::Wake(false)));
        app.listen_any("reever://level", |e| {
            if let Ok(v) = e.payload().parse::<f32>() {
                send(Msg::Level(v));
            }
        });
        app.listen_any("reever://status", |e| {
            let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap_or_default();
            // статус бывает строкой с JSON внутри — как в app.js applyStatus
            let v = match v {
                serde_json::Value::String(s) => serde_json::from_str(&s).unwrap_or_default(),
                v => v,
            };
            send(Msg::Status(v));
        });
        app.listen_any("mascot://orb", move |e| send(Msg::Orb(text(e.payload()))));
        // калибровка записала фразу — список действий (и счёт в окне калибровки) заново
        let app_c = app.clone();
        app.listen_any("reever://calibrated", move |_| refresh(&app_c));
        app.listen_any("reever://voice-adapt", |e| {
            let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap_or_default();
            send(Msg::VoiceWords(v.get("words").and_then(|w| w.as_u64()).unwrap_or(0) as u32));
        });
    });
}

/// Сообщение открытому окну (из любого потока).
pub fn send(msg: Msg) {
    let Some((id, _)) = WIN.lock().ok().and_then(|w| *w) else { return };
    let Some(app) = APP.get() else { return };
    if let Some(ui) = crate::mascot_native::ui(app) {
        ui.post(move |ui| {
            if let Some(v) = ui.handler_mut::<MainView>(id) {
                v.on_msg(msg);
            }
            ui.invalidate(id);
        });
    }
}

/// Сведения для экранов: пауза, индекс, активация, число действий, версия.
fn info(app: &AppHandle) -> Info {
    let shared = app.try_state::<Arc<crate::actions::SharedState>>();
    let paused = shared.as_ref().is_some_and(|s| s.paused.load(std::sync::atomic::Ordering::Relaxed));
    let actions = shared.as_ref().and_then(|s| s.actions.lock().ok().map(|a| a.len())).unwrap_or(0);
    let index = app
        .try_state::<crate::local_index::IndexManager>()
        .map(|i| i.status().state.to_string())
        .unwrap_or_default();
    let (mode, hotkey) = app
        .try_state::<crate::config::LiveConfig>()
        .map(|c| {
            let s = &c.get().settings;
            (s.activation_mode.clone(), s.hotkey.clone())
        })
        .unwrap_or_default();
    // «Что можно сказать»: примеры на языке интерфейса, если он распознаётся
    // языки загруженных моделей, как у веб-версии (inactive_*); до запуска распознавания — настройки
    let langs: Vec<String> = crate::grammar::active_languages().unwrap_or_else(|| {
        app.try_state::<crate::config::LiveConfig>()
            .map(|c| c.get().settings.recognition_languages().iter().map(|l| l.to_string()).collect())
            .unwrap_or_else(|| vec!["en".into()])
    });
    let langs: Vec<&str> = langs.iter().map(String::as_str).collect();
    let grammar = shared
        .as_ref()
        .and_then(|s| s.grammar.lock().ok().map(|g| g.clone()))
        .unwrap_or_else(crate::grammar::Grammar::defaults);
    let say = say::build(&grammar, &crate::uilang::current().to_string(), &langs);
    let action_list = shared.as_ref().and_then(|s| s.actions.lock().ok().map(|a| a.clone())).unwrap_or_default();
    let hidden_phrases = app
        .try_state::<Arc<crate::actions::SharedState>>()
        .map(crate::commands_api::inactive_action_phrases)
        .unwrap_or_default();
    let lang_list: Vec<String> = langs.iter().map(|l| l.to_string()).collect();
    // встроенные фразы, слова отмены и конца диктовки языков, которые не распознаются
    let mut hidden_triggers = grammar.inactive_triggers_for(Some(&lang_list));
    hidden_triggers.extend(grammar.inactive_words_for(&lang_list));
    Info { paused, actions, index, mode, hotkey, version: crate::app::VERSION.to_string(), say, grammar, hidden_triggers, action_list, hidden_phrases, settings: settings_info(app) }
}

/// Настройки для вкладки: settings.json (как get_settings), автозапуск, наборы звуков, языки.
fn settings_info(app: &AppHandle) -> settings_tab::SettingsInfo {
    let Some(config) = app.try_state::<crate::config::LiveConfig>() else { return Default::default() };
    let settings = crate::commands_api::get_settings(app.state()).unwrap_or_else(|_| serde_json::to_value(&config.get().settings).unwrap_or_default());
    let enabled = config.get().settings.interface_languages();
    settings_tab::SettingsInfo {
        settings,
        autostart: crate::commands_api::get_autostart(app.clone()),
        sound_packs: crate::commands_api::list_sound_packs(app.state()),
        ui_languages: enabled.iter().map(|c| c.to_string()).collect(),
        auto_language: crate::uilang::resolve_enabled("auto", &enabled).to_string(),
        models: crate::commands_api::language_models(app.clone()),
        voice: crate::commands_api::voice_adapt_status(app.clone(), app.state()).unwrap_or_default(),
    }
}

/// Изменение на вкладке «Настройки» — теми же командами, что у веб-окна; итог — строкой под формой.
/// Вкладка «Текст»: файл, звук компьютера, буфер обмена, языки файла.
fn text_act(app: &AppHandle, out: text_tab::TextOut) {
    use text_tab::TextOut;
    match out {
        TextOut::PickFile(languages) => {
            let owner = WIN.lock().ok().and_then(|w| *w).map(|(_, h)| h).unwrap_or(0);
            if let Some(path) = pick_audio_file(owner) {
                crate::transcribe::start_file(app, path, languages);
            }
        }
        TextOut::File(path, languages) => crate::transcribe::start_file(app, path, languages),
        TextOut::CancelFile => crate::transcribe::cancel_file(),
        TextOut::Listen => {
            crate::listen::start(app);
        }
        TextOut::ListenStop(cancel) => {
            crate::listen::stop(cancel);
        }
        TextOut::Copy(text) => {
            let _ = crate::typing::set_clipboard(&text);
        }
        TextOut::Clear => crate::transcribe::clear(),
        TextOut::Languages(codes) => {
            let mut patch = serde_json::Map::new();
            patch.insert("transcribe_languages".into(), serde_json::json!(codes));
            apply_setting(app, settings_tab::SettingsOut::Patch(patch));
        }
    }
}

/// Окно «Открыть» Windows для аудио и видео; None — закрыли.
fn pick_audio_file(owner: isize) -> Option<std::path::PathBuf> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> windows::core::Result<std::path::PathBuf> {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let locale = crate::uilang::current().to_string();
            let media = windows::core::HSTRING::from(crate::ui_text::t(&locale, "text.file.types", &[]));
            let filters = [
                COMDLG_FILTERSPEC { pszName: PCWSTR(media.as_ptr()), pszSpec: w!("*.mp3;*.m4a;*.aac;*.wav;*.wma;*.flac;*.ogg;*.opus;*.mp4;*.m4v;*.mov;*.avi;*.mkv;*.wmv;*.webm;*.3gp;*.amr") },
                COMDLG_FILTERSPEC { pszName: w!("*.*"), pszSpec: w!("*.*") },
            ];
            dialog.SetFileTypes(&filters)?;
            dialog.Show(HWND(owner as *mut _))?;
            let item = dialog.GetResult()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = name.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
            Ok(std::path::PathBuf::from(path))
        })();
        if com {
            CoUninitialize();
        }
        result.ok()
    }
}

fn apply_setting(app: &AppHandle, out: settings_tab::SettingsOut) {
    use settings_tab::SettingsOut;
    let locale = crate::uilang::current().to_string();
    let tr = |k: &str, p: &[(&str, &str)]| crate::ui_text::t(&locale, k, p);
    let err = |e: String| tr("settings.error", &[("error", &e)]);
    let saved = tr("settings.saved", &[]);
    let msg = match out {
        SettingsOut::Patch(map) => {
            let monitor = map.contains_key("lite_monitor");
            let r = crate::commands_api::save_settings(app.clone(), app.state(), app.state(), serde_json::Value::Object(map));
            if monitor {
                crate::bubble::configure_monitor(app);
            }
            r.map_or_else(err, |_| saved.clone())
        }
        SettingsOut::LiteUi(on) => crate::commands_api::set_lite_ui(app.clone(), app.state(), on).map_or_else(err, |_| saved.clone()),
        SettingsOut::Autostart(on) => match crate::commands_api::set_autostart(app.clone(), on) {
            Ok(()) => tr("settings.autostartState", &[("state", &tr(if on { "common.on" } else { "common.off" }, &[]))]),
            Err(e) => err(e),
        },
        SettingsOut::Hotkey { mute, combo, mode } => {
            let r = if mute {
                crate::commands_api::set_mute_hotkey(app.clone(), app.state(), app.state(), combo.clone(), mode)
            } else {
                crate::commands_api::set_hotkey(app.clone(), app.state(), app.state(), combo.clone(), mode)
            };
            match r {
                Ok(()) => tr("hotkey.applied", &[("combo", &combo)]),
                Err(e) => tr("hotkey.error", &[("error", &e)]),
            }
        }
        SettingsOut::Mascot { mode, scale, bubble } => {
            crate::commands_api::set_mascot(app.clone(), app.state(), mode, scale, bubble).map_or_else(err, |_| saved.clone())
        }
        SettingsOut::UiLanguage(code) => match crate::commands_api::set_ui_language(app.state(), code) {
            // окно рисует подписи на своём языке — пересоздать его на новом
            Ok(_) => {
                crate::rebuild_main(app);
                return;
            }
            Err(e) => err(e),
        },
        SettingsOut::UiLanguages { codes, language } => {
            let mut patch = serde_json::Map::new();
            patch.insert("ui_languages".into(), serde_json::Value::from(codes));
            if let Err(e) = crate::commands_api::save_settings(app.clone(), app.state(), app.state(), serde_json::Value::Object(patch)) {
                err(e)
            } else {
                // язык окна сменился (выбранный убрали, «как в Windows» раскрылось иначе) — пересоздать
                let before = crate::uilang::current();
                match crate::commands_api::set_ui_language(app.state(), language) {
                    Ok(code) if code != before => {
                        crate::rebuild_main(app);
                        return;
                    }
                    Ok(_) => saved.clone(),
                    Err(e) => err(e),
                }
            }
        }
        SettingsOut::MascotPlace => {
            if crate::commands_api::mascot_place(app.clone()) {
                saved.clone()
            } else {
                send(Msg::SettingsPlacing(false));
                tr("settings.mascot_position.unavailable", &[])
            }
        }
        SettingsOut::MascotPin(save) => crate::commands_api::mascot_pin(app.clone(), app.state(), save).map_or_else(err, |_| saved.clone()),
        SettingsOut::MascotUnpin => crate::commands_api::mascot_unpin(app.clone(), app.state()).map_or_else(err, |_| saved.clone()),
        SettingsOut::VoiceReset => crate::commands_api::reset_voice_adapt(app.clone(), app.state()).map_or_else(err, |_| saved.clone()),
        SettingsOut::Fps(n) => crate::mascot::set_fps_limit(app.state(), n).map_or_else(err, |_| saved.clone()),
        SettingsOut::OpenGrammar | SettingsOut::VoiceStart => return,
    };
    refresh(app);
    send(Msg::SettingsMsg(msg));
}

/// Полоска (setShade в shell.js): регион и размер окна, «поверх всех», флаг для снимка.
fn set_shade(app: &AppHandle, on: bool) {
    let Some((id, hwnd)) = WIN.lock().ok().and_then(|w| *w) else { return };
    let Some(ui) = crate::mascot_native::ui(app) else { return };
    ui.call(move |ui| {
        let Some(v) = ui.handler_mut::<MainView>(id) else { return };
        if v.shaded() == on {
            return;
        }
        let (src, w, h) = v.shape_source(on);
        let k = v.k();
        let shape = match src {
            Some(src) => ui.images().alpha_mask(src, w, h).map(|m| view::shape_rects(&m.alpha, w as usize, h as usize)).unwrap_or_default(),
            // лёгкий вид: скруглённый прямоугольник (16 у полоски, 18 у окна)
            None => view::rounded_rects(w as f32, h as f32, if on { 16.0 } else { 18.0 }),
        };
        let scale = ui.info(id).map_or(1.0, |i| i.scale);
        let scaled: Vec<[f64; 4]> = shape.iter().map(|r| r.map(|x| x * k as f64)).collect();
        // сначала регион, потом размер: иначе на кадр мелькнёт старая форма в новом размере
        if on {
            let _ = crate::skin::apply_region(hwnd, &scaled, scale as f64);
        }
        ui.set_size(id, w as f32 * k, h as f32 * k);
        if !on {
            let _ = crate::skin::apply_region(hwnd, &scaled, scale as f64);
        }
        crate::winutil::set_topmost_raw(hwnd, on);
        if let Some(v) = ui.handler_mut::<MainView>(id) {
            v.set_shaded(on, shape);
        }
        ui.invalidate(id);
    });
    crate::events::set_compact(on);
}

/// Отметка «знакомство показано» (у веб-окна — localStorage): файл в папке настроек.
fn intro_flag(app: &AppHandle, what: &str) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(format!("native-intro-{what}.txt")))
}

fn mark_intro(app: &AppHandle, what: &str) {
    if let Some(p) = intro_flag(app, what) {
        let _ = std::fs::write(p, "1");
    }
}

/// Первый запуск (maybeMascotIntro / maybeVoiceIntro в app.js): маскот рассказывает о себе,
/// потом — обязательная подстройка под голос. У кого подстройка уже есть, тому — ни того, ни
/// другого. Звать в своём потоке: ждёт конца рассказа маскота.
fn first_run(app: &AppHandle) {
    if crate::main_demo::active() {
        return;
    }
    let status = app
        .try_state::<crate::config::LiveConfig>()
        .and_then(|_| crate::commands_api::voice_adapt_status(app.clone(), app.state()))
        .unwrap_or_default();
    let adapted = status.get("report").is_some_and(|r| !r.is_null());
    let collected = status.get("passive").and_then(|p| p.get("collected")).and_then(|c| c.as_u64()).unwrap_or(0) > 0;
    let seen = |what: &str| intro_flag(app, what).is_some_and(|p| p.is_file());
    if !seen("mascot") && !adapted && !collected {
        mark_intro(app, "mascot");
        let locale = crate::uilang::current().to_string();
        let ru = locale == "ru";
        let params = [
            ("wake", if ru { "ривер" } else { "reever" }.to_string()),
            ("settings", crate::ui_text::t(&locale, "nav.settings", &[])),
            ("section", crate::ui_text::t(&locale, "settings.group.mascot", &[])),
        ];
        let p: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let lines: Vec<String> = (1..=4).map(|n| crate::ui_text::t(&locale, &format!("mascotIntro.{n}"), &p)).collect();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let id = app.listen_any("reever://mascot-intro-done", move |_| {
            let _ = tx.send(());
        });
        if crate::mascot::mascot_intro(app.clone(), lines) {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(45));
        }
        app.unlisten(id);
    }
    // калибровка — один раз на пользователя: переустановка и обновление её не повторяют
    if !seen("voice") && !adapted {
        send(Msg::VoiceIntro);
    }
}

/// Обновить экраны из Rust (пауза, индекс, действия, настройки) — после действий и раз в пару секунд.
pub fn refresh(app: &AppHandle) {
    if is_open() {
        send(Msg::Info(info(app)));
    }
}

fn act(app: &AppHandle, a: Act) {
    match a {
        Act::Minimize => {
            if let Some((_, hwnd)) = WIN.lock().ok().and_then(|w| *w) {
                crate::winutil::minimize_raw(hwnd);
            }
        }
        Act::Tray => crate::destroy_main(app),
        Act::TogglePause => {
            crate::toggle_pause(app);
            refresh(app);
        }
        Act::Tab(tab) => {
            let _ = crate::events::set_ui_tab(tab);
        }
        Act::SaveGrammar(g) => {
            let r = match app.try_state::<Arc<crate::actions::SharedState>>() {
                Some(state) => crate::commands_api::save_grammar_in(&state, g),
                None => Err(crate::ui_text::t(&crate::uilang::current().to_string(), "grammar.saveFailed", &[])),
            };
            refresh(app);
            send(Msg::GrammarDone(r.err()));
        }
        Act::SaveAction(a) => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                if let Err(e) = crate::commands_api::save_action(state, a) {
                    crate::events::ui_log(app, format!("[reever] save action: {e}"));
                }
            }
            refresh(app);
        }
        Act::DeleteAction(id) => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                if let Err(e) = crate::commands_api::delete_action(state, id) {
                    crate::events::ui_log(app, format!("[reever] delete action: {e}"));
                }
            }
            refresh(app);
        }
        Act::Settings(out) => apply_setting(app, out),
        Act::Text(out) => text_act(app, out),
        Act::Index => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                crate::picker::show_index(app, &state, "");
            }
        }
        Act::Shade(on) => set_shade(app, on),
        Act::Calibrate(a) => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                let id = a.id.clone();
                let _ = crate::commands_api::save_action(state.clone(), a);
                let _ = crate::commands_api::start_calibration(state, id);
            }
            refresh(app);
        }
        Act::CancelCalibration => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                let _ = crate::commands_api::cancel_calibration(state);
            }
            refresh(app);
        }
        Act::ListApps => {
            let apps = crate::commands_api::list_apps().into_iter().map(|a| (a.name, a.aumid)).collect();
            send(Msg::Apps(apps));
        }
        Act::VoiceStart => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                let _ = crate::commands_api::start_voice_adapt(state);
            }
        }
        Act::VoiceCancel => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                let _ = crate::commands_api::cancel_voice_adapt(state);
            }
        }
        Act::Scan => {
            let found = match (app.try_state::<Arc<crate::actions::SharedState>>(), app.try_state::<crate::config::LiveConfig>()) {
                (Some(state), Some(config)) => crate::commands_api::scan_apps(state, config).unwrap_or_default(),
                _ => Vec::new(),
            };
            let apps: Vec<overlays::ScanApp> = found
                .into_iter()
                .map(|a| {
                    let v = serde_json::to_value(&a).unwrap_or_default();
                    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
                    overlays::ScanApp {
                        id: a.id,
                        name: a.name,
                        kind: a.kind,
                        target: a.target,
                        phrases: a.phrases,
                        checked: a.checked,
                        note: s("note"),
                        group: s("group").unwrap_or_else(|| "other".into()),
                        focus_minutes: a.focus_minutes,
                        icon_png: None,
                        icon: None,
                    }
                })
                .collect();
            let ids: Vec<String> = apps.iter().map(|a| a.id.clone()).collect();
            send(Msg::ScanApps(apps));
            // иконки догружаются после списка, чтобы он не ждал их
            if !ids.is_empty() {
                use base64::Engine;
                let icons = crate::commands_api::scan_app_icons(ids).unwrap_or_default();
                let decoded = icons
                    .into_iter()
                    .filter_map(|(id, url)| {
                        let b64 = url.split_once(",").map(|(_, b)| b.to_string())?;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
                        Some((id, std::sync::Arc::<[u8]>::from(bytes)))
                    })
                    .collect();
                send(Msg::ScanIcons(decoded));
            }
        }
        Act::AddScanned(apps) => {
            let chosen: Vec<crate::appscan::ChosenApp> = apps
                .into_iter()
                .map(|a| crate::appscan::ChosenApp { name: a.name, kind: a.kind, target: a.target, phrases: a.phrases })
                .collect();
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                match crate::commands_api::add_scanned_apps(state, chosen) {
                    Ok(count) => crate::events::ui_log(
                        app,
                        crate::ui_text::t(&crate::uilang::current().to_string(), "scan.added", &[("count", &count.to_string())]),
                    ),
                    Err(e) => crate::events::ui_log(app, format!("[reever] scan: {e}")),
                }
            }
            refresh(app);
        }
        Act::VoiceFinish(intro) => {
            let locale = crate::uilang::current().to_string();
            let wake = if locale == "ru" { "ривер" } else { "reever" };
            let r = crate::commands_api::finish_voice_adapt(app.clone(), app.state(), app.state());
            let n = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0).to_string();
            let text = match r.map(|rep| serde_json::to_value(rep).unwrap_or_default()) {
                Ok(v) if v.get("adapted").and_then(|a| a.as_bool()) == Some(true) => crate::ui_text::t(
                    &locale,
                    "voiceAdapt.adapted",
                    &[("heard", &n(&v, "heard_after")), ("words", &n(&v, "words")), ("before", &n(&v, "heard_before"))],
                ),
                Ok(v) => crate::ui_text::t(&locale, "voiceAdapt.noGain", &[("heard", &n(&v, "heard_before")), ("words", &n(&v, "words"))]),
                Err(e) => match e.strip_prefix("tooFew:") {
                    Some(count) => {
                        let text = crate::ui_text::t(&locale, "voiceAdapt.tooFew", &[("count", count), ("min", "5"), ("wake", wake)]);
                        // первая подстройка не кончается, пока слова не разобраны: запись — заново
                        if intro {
                            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                                let _ = crate::commands_api::start_voice_adapt(state);
                            }
                            send(Msg::VoiceResult(text, true));
                            return;
                        }
                        text
                    }
                    None => crate::ui_text::t(&locale, "voiceAdapt.failed", &[("error", &e)]),
                },
            };
            // подстройка не применилась из-за ошибки — всё равно отпускаем, иначе программа заперта
            if intro {
                mark_intro(app, "voice");
            }
            refresh(app);
            send(Msg::VoiceResult(text, false));
        }
        Act::ResetGrammar => {
            if let Some(state) = app.try_state::<Arc<crate::actions::SharedState>>() {
                crate::commands_api::reset_grammar_in(&state);
            }
            refresh(app);
            send(Msg::GrammarDone(None));
        }
    }
}

/// Левый верхний угол и масштаб скина: окно 1280×880 DIP целиком в рабочей области основного
/// монитора (как fitScale в shell.js — с запасом 24 DIP), по центру.
fn placement() -> (i32, i32, f32) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let dpi = dx as f32 / 96.0;
        let r = info.rcWork;
        let (aw, ah) = ((r.right - r.left) as f32 / dpi - 24.0, (r.bottom - r.top) as f32 / dpi - 24.0);
        let k = (aw / skin_w()).min(ah / skin_h()).min(1.0).max(0.3);
        let (w, h) = ((skin_w() * k * dpi).round() as i32, (skin_h() * k * dpi).round() as i32);
        (r.left + (r.right - r.left - w) / 2, r.top + (r.bottom - r.top - h) / 2, k)
    }
}

/// Показать окно на вкладке `tab` («открой настройки ривера»): окно создаётся в потоке окон,
/// вкладка переключается, когда оно появилось.
pub fn show_tab(app: &AppHandle, tab: &'static str) {
    show(app);
    std::thread::spawn(move || {
        for _ in 0..100 {
            if WIN.lock().ok().is_some_and(|w| w.is_some()) {
                send(Msg::Tab(tab.into()));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
}

/// Показать окно (создать, если его нет) и отдать ему фокус.
pub fn show(app: &AppHandle) {
    init(app);
    // знакомство первого запуска (ждёт, пока окно появится: поток окон рисует его раньше)
    static FIRST_RUN: Once = Once::new();
    let app_fr = app.clone();
    FIRST_RUN.call_once(move || {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            first_run(&app_fr);
        });
    });
    // в режиме проверки окно не забирает фокус: человек за компьютером может печатать
    let demo = crate::main_demo::active();
    if let Some((_, hwnd)) = WIN.lock().ok().and_then(|w| *w) {
        if !demo {
            crate::winutil::bring_to_front_raw(hwnd);
            native::activate(hwnd);
        }
        return;
    }
    if CREATING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // сброс — когда окно записано в WIN (или не создалось), как бы ни кончилась ветка
        struct Created;
        impl Drop for Created {
            fn drop(&mut self) {
                CREATING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _created = Created;
        let Some(ui) = crate::mascot_native::ui(&app) else { return };
        let lite = app.try_state::<crate::config::LiveConfig>().is_some_and(|c| c.get().settings.lite_ui);
        view::set_lite(lite);
        let (x, y, k) = placement();
        let snapshot = crate::events::get_ui_snapshot();
        let info = info(&app);
        let locale = crate::uilang::current().to_string();
        let app2 = app.clone();
        let host: Arc<dyn Fn(Act) + Send + Sync> = Arc::new(move |a| {
            let app = app2.clone();
            std::thread::spawn(move || act(&app, a));
        });
        let app3 = app.clone();
        let made = ui.call(move |ui| {
            let assets = view::Assets::load(&app3, ui.images());
            // лёгкий вид — скруглённый прямоугольник во всё окно, полный — по альфе рамки
            let shape = if lite { view::rounded_rects(skin_w(), skin_h(), 18.0) } else { assets.shape(ui.images()) };
            let view = MainView::new(assets, k, &locale, snapshot, info, host);
            let spec = WindowSpec {
                title: "Reever".into(),
                x,
                y,
                width: skin_w() * k,
                height: skin_h() * k,
                topmost: false,
                no_activate: demo,
                visible: true,
                // кнопка на панели задач: свёрнутое окно возвращается ею, а не только из трея
                taskbar: !demo,
            };
            let id = ui.create_window(spec, Box::new(view))?;
            let (hwnd, scale) = ui.info(id).map(|i| (i.hwnd, i.scale)).unwrap_or((0, 1.0));
            // осьминог в сфере: свой вид маскота (кадры общие с окном маскота через кэш картинок)
            let orb = (!lite).then(|| crate::mascot_native::load_assets(&app3, ui.images())).flatten()
                .map(|a| crate::mascot_native::MascotView::new(a, &crate::events::current_state()));
            // полоска компактного режима: своя рамка, те же картинки лунок
            let btn = ui.handler_mut::<MainView>(id).map(|v| v.shade_buttons());
            let shade_view = btn.map(|btn| {
                let tip = |k: &str| crate::ui_text::t(&crate::uilang::current().to_string(), k, &[]);
                shade::Shade::new(ui.images(), &app3, btn, [tip("win.expand"), tip("win.toTray")])
            });
            if let Some(v) = ui.handler_mut::<MainView>(id) {
                if let Some(s) = shade_view {
                    v.set_shade_view(s);
                }
                v.set_shape(shape.clone());
                if let Some(orb) = orb {
                    v.set_orb_mascot(orb, crate::mascot::mascot_orb());
                }
            }
            let scaled: Vec<[f64; 4]> = shape.iter().map(|r| r.map(|v| v * k as f64)).collect();
            if let Err(e) = crate::skin::apply_region(hwnd, &scaled, scale as f64) {
                native::log(&format!("main: window shape: {e}"));
            }
            Ok::<_, windows::core::Error>((id, hwnd))
        });
        match made {
            Some(Ok((id, hwnd))) => {
                if let Ok(mut w) = WIN.lock() {
                    *w = Some((id, hwnd));
                }
                crate::events::set_main_visible(true);
                // окно закрывали полоской — открывается полоской (snapshot.compact)
                if crate::events::get_ui_snapshot().compact {
                    set_shade(&app, true);
                }
                if !demo {
                    crate::winutil::bring_to_front_raw(hwnd);
                    native::activate(hwnd);
                }
                // экраны, которые не приходят событиями (пауза, индекс, число действий)
                std::thread::spawn(move || {
                    while is_open() {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        if let Some(app) = APP.get() {
                            refresh(app);
                        }
                    }
                });
            }
            Some(Err(e)) => crate::events::ui_log(&app, format!("main: native window failed: {e}")),
            None => {}
        }
    });
}

/// Снимок окна вне экрана (проверка без захвата экрана, main_demo.rs).
pub fn snapshot(app: &AppHandle) -> Option<(u32, u32, Vec<u8>)> {
    let (id, _) = WIN.lock().ok().and_then(|w| *w)?;
    let ui = crate::mascot_native::ui(app)?;
    ui.call(move |ui| ui.snapshot(id)).flatten()
}

/// Время от создания окна до первого показанного кадра, мс (main_demo.rs).
pub fn first_frame(app: &AppHandle) -> Option<u128> {
    let (id, _) = WIN.lock().ok().and_then(|w| *w)?;
    let ui = crate::mascot_native::ui(app)?;
    ui.call(move |ui| ui.info(id).and_then(|i| i.first_frame)).flatten().map(|d| d.as_millis())
}

/// Уничтожить окно («в трей»): без окон поток окон отпускает графическое устройство.
pub fn destroy(app: &AppHandle) {
    let Some((id, _)) = WIN.lock().ok().and_then(|mut w| w.take()) else { return };
    crate::events::set_main_visible(false);
    if let Some(ui) = crate::mascot_native::ui(app) {
        ui.post(move |ui| {
            ui.close_window(id);
            ui.release_device();
        });
    }
}

/// Кэш картинок потока окон: (картинок, байт, декодирований всего) — для проверок.
pub fn image_stats(app: &AppHandle) -> Option<(usize, usize, u64)> {
    let ui = crate::mascot_native::ui(app)?;
    ui.call(|ui| ui.images().stats())
}

/// Кадров потока окон всего — для проверок.
pub fn frames(app: &AppHandle) -> Option<u64> {
    let ui = crate::mascot_native::ui(app)?;
    ui.call(|ui| ui.frames())
}

/// Полоска вкл/выкл (проверка main_demo.rs).
pub fn shade_for_test(app: &AppHandle, on: bool) {
    set_shade(app, on);
}

/// Сдвиг прокрутки, чтобы `r` (уже со сдвигом) попал в `view` с полями 12 DIP.
pub(crate) fn reveal_delta(r: crate::native::Rect, view: crate::native::Rect) -> f32 {
    let pad = 12.0;
    if r.y < view.y + pad {
        r.y - view.y - pad
    } else if r.y + r.h > view.y + view.h - pad {
        (r.y + r.h - (view.y + view.h - pad)).min(r.y - view.y - pad)
    } else {
        0.0
    }
}
