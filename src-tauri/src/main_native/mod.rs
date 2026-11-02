//! Нативное главное окно (этап 4 docs/native-ui-roadmap.md): то же, что frontend/index.html +
//! skin/shell.js, на ядре `native/` в общем с маскотом и диалогами потоке окон. Включается
//! настройкой `native_main` (в лёгком интерфейсе пока веб). Перенесены каркас (рамка с шейдером,
//! форма окна, лунки, перетаскивание), экраны состояния, рельс вкладок, бегущая строка,
//! эквалайзер, жидкая линза, «Главная» и «Лог»; «Действия» и «Настройки» пока открывают
//! веб-версию кнопкой.
//!
//! Жизненный цикл тот же, что у веб-окна в lib.rs: «в трей» окно уничтожается целиком, показ —
//! создаёт его заново по снимку состояния (`events::get_ui_snapshot`). События распознавания
//! окно слушает само через `AppHandle::listen_any`, подписка — при старте программы (`init`).

mod actions_tab;
mod frame;
mod fx;
mod grammar_edit;
mod say;
mod settings_tab;
mod view;

use std::sync::{Arc, Mutex, Once, OnceLock};

use tauri::{AppHandle, Listener, Manager};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::native::{self, WindowId, WindowSpec};
pub use view::Msg;
use view::{Info, MainView, SKIN_H, SKIN_W};

static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);
static LISTEN: Once = Once::new();
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Что окно просит сделать (кнопки, вкладки) — выполняется вне потока окон.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Minimize,
    Tray,
    TogglePause,
    OpenWeb,
    Tab(String),
    SaveGrammar(crate::grammar::Grammar),
    ResetGrammar,
    SaveAction(crate::actions::Action),
    DeleteAction(String),
    Settings(settings_tab::SettingsOut),
}

/// Включено ли нативное главное окно (настройка `native_main`; в лёгком интерфейсе — веб).
pub fn enabled(app: &AppHandle) -> bool {
    app.try_state::<crate::config::LiveConfig>().is_some_and(|c| {
        let s = &c.get().settings;
        s.native_main && !s.lite_ui
    })
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
    let hidden_triggers = grammar.inactive_triggers_for(Some(&langs.iter().map(|l| l.to_string()).collect::<Vec<_>>()));
    Info { paused, actions, index, mode, hotkey, version: app.package_info().version.to_string(), say, grammar, hidden_triggers, action_list, hidden_phrases, settings: settings_info(app) }
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
fn apply_setting(app: &AppHandle, out: settings_tab::SettingsOut) {
    use settings_tab::SettingsOut;
    let locale = crate::uilang::current().to_string();
    let tr = |k: &str, p: &[(&str, &str)]| crate::ui_text::t(&locale, k, p);
    let err = |e: String| tr("settings.error", &[("error", &e)]);
    let saved = tr("settings.saved", &[]);
    let msg = match out {
        SettingsOut::Patch(map) => crate::commands_api::save_settings(app.clone(), app.state(), app.state(), serde_json::Value::Object(map))
            .map_or_else(err, |_| saved.clone()),
        SettingsOut::Native(key, on) => {
            let r = match key {
                "native_mascot" => crate::mascot::set_native_mascot(app.clone(), app.state(), on),
                "native_dialogs" => crate::dialogs_native::set_native_dialogs(app.state(), on),
                _ => set_native_main(app.clone(), app.state(), on),
            };
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
                crate::switch_main(app);
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
                        crate::switch_main(app);
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
        SettingsOut::OpenGrammar => return,
    };
    refresh(app);
    send(Msg::SettingsMsg(msg));
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
        Act::OpenWeb => {
            let config = app.state::<crate::config::LiveConfig>();
            let _ = set_native_main(app.clone(), config, false);
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
        let k = (aw / SKIN_W).min(ah / SKIN_H).min(1.0).max(0.3);
        let (w, h) = ((SKIN_W * k * dpi).round() as i32, (SKIN_H * k * dpi).round() as i32);
        (r.left + (r.right - r.left - w) / 2, r.top + (r.bottom - r.top - h) / 2, k)
    }
}

/// Показать окно (создать, если его нет) и отдать ему фокус.
pub fn show(app: &AppHandle) {
    init(app);
    // в режиме проверки окно не забирает фокус: человек за компьютером может печатать
    let demo = crate::main_demo::active();
    if let Some((_, hwnd)) = WIN.lock().ok().and_then(|w| *w) {
        if !demo {
            crate::winutil::bring_to_front_raw(hwnd);
            native::activate(hwnd);
        }
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(ui) = crate::mascot_native::ui(&app) else { return };
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
            let shape = assets.shape(ui.images());
            let view = MainView::new(assets, k, &locale, snapshot, info, host);
            let spec = WindowSpec {
                title: "Reever".into(),
                x,
                y,
                width: SKIN_W * k,
                height: SKIN_H * k,
                topmost: false,
                no_activate: demo,
                visible: true,
            };
            let id = ui.create_window(spec, Box::new(view))?;
            let (hwnd, scale) = ui.info(id).map(|i| (i.hwnd, i.scale)).unwrap_or((0, 1.0));
            // осьминог в сфере: свой вид маскота (кадры общие с окном маскота через кэш картинок)
            let orb = crate::mascot_native::load_assets(&app3, ui.images())
                .map(|a| crate::mascot_native::MascotView::new(a, &crate::events::current_state()));
            if let Some(v) = ui.handler_mut::<MainView>(id) {
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

/// Настройки → Интерфейс: нативное главное окно вкл/выкл — сразу: открытое окно (веб или
/// нативное) пересоздаётся в новом виде.
#[tauri::command]
pub fn set_native_main(app: AppHandle, config: tauri::State<'_, crate::config::LiveConfig>, enabled: bool) -> Result<(), String> {
    let current = config.get();
    crate::commands_api::persist_value(&current, "native_main", serde_json::Value::Bool(enabled))?;
    let mut fresh = (*current).clone();
    fresh.settings.native_main = enabled;
    config.set(Arc::new(fresh));
    let open = is_open() || app.get_webview_window("main").is_some();
    if open {
        let app = app.clone();
        std::thread::spawn(move || crate::switch_main(&app));
    }
    Ok(())
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
