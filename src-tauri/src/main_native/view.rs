//! Вид нативного главного окна: перенос frontend/index.html + skin/shell.js (каркас, экраны,
//! линза, эквалайзер, вкладки «Главная» и «Лог»). Всё рисуется в координатах скина 1280×880
//! (как CSS-пиксели #skin): масштаб окна под маленький экран (`--s` в CSS) — множитель DPI
//! контекста на время кадра.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1Image, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use super::frame::{FrameLight, FramePixels, FrameShader};
use super::fx::{Fx, FxState, Mode};
use super::Act;
use crate::mascot_native::lens::{Lens, LensParams};
use crate::mascot_native::{Msg as MascotMsg, MascotView};
use crate::native::controls::button::{Button, ButtonKind, Lunka, Press};
use crate::native::controls::anim::{Bez, E_OUT};
use crate::native::controls::form::Form;
use crate::native::controls::tooltip::Tooltip;
use crate::native::controls::{
    data, dotted_hline, fill_round, lcd, lcd_background, lcd_overlay, ui, with_alpha, Glow, Scene, ACC, ACC_2, ACC_3,
    ACC_4, ACC_5, ACC_HOT,
};
use crate::native::{
    self, set_cursor, Align, Color, Cursor, Direction, GpuCx, ImageId, Images, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect,
    RenderCx, TextStyle, WindowHandler,
};

pub const SKIN_W: f32 = 1280.0;
pub const SKIN_H: f32 = 880.0;

/// ТЕСТ производительности, как FRAME_STATIC в shell.js (выбор пользователя): рамка без живого
/// света — ни блика под курсором, ни света линзы по голосу, ни подсветки лунок. Шейдер рамки
/// рисует один кадр и стоит. Вернуть живой свет — false.
const FRAME_STATIC: bool = true;

// Геометрия скина — та же, что в design/reever-frame.blend и skin.css.
const PANEL: (f32, f32, f32, f32, f32) = (130.0, 100.0, 1150.0, 780.0, 46.0);
const ORB: (f32, f32, f32) = (310.0, 352.0, 100.0);
const CORE: Rect = Rect::new(204.0, 246.0, 212.0, 212.0);
const TITLE: Rect = Rect::new(178.0, 132.0, 212.0, 30.0);
const STATUS: Rect = Rect::new(190.0, 510.0, 242.0, 158.0);
const RAIL: Rect = Rect::new(1045.0, 174.0, 69.0, 506.0);
const SCREEN: Rect = Rect::new(465.0, 174.0, 560.0, 506.0);
const EQ: Rect = Rect::new(190.0, 700.0, 924.0, 46.0);
const TABS: [&str; 4] = ["home", "actions", "settings", "logs"];
const TAB_ICONS: [&str; 4] = ["orb", "stack", "tune", "term"];
const TAB_KEYS: [&str; 4] = ["nav.home", "nav.actions", "nav.settings", "nav.log"];
const LOG_LIMIT: usize = 250;

/// Сообщения окну из распознавания и Rust.
#[derive(Debug, Clone)]
pub enum Msg {
    State(String),
    Partial(String),
    Heard(String),
    Log(String),
    Level(f32),
    Wake(bool),
    Status(serde_json::Value),
    Info(Info),
    /// mascot://orb — осьминог в сфере (пока не рисуется, следующий шаг)
    Orb(String),
    /// Открыть вкладку (проверка main_demo.rs)
    Tab(String),
    /// Грамматика сохранена (None) или ошибка сохранения.
    GrammarDone(Option<String>),
    /// Раскрыть редактор «Что можно сказать» (id команды, `cancel`, `general`; проверка)
    EditGrammar(String),
    /// Итог изменения настройки — строкой под формой.
    SettingsMsg(String),
    /// Открыть окно поверх для проверки: `calibration`, `voice`, `apps`, `close`.
    TestOverlay(String),
    /// Сканер: найденные программы и их иконки (id → PNG).
    ScanApps(Vec<super::overlays::ScanApp>),
    ScanIcons(std::collections::HashMap<String, std::sync::Arc<[u8]>>),
    /// Программы Windows для выбора (list_apps).
    Apps(Vec<(String, String)>),
    /// Подстройка под голос: сколько слов услышано.
    VoiceWords(u32),
    /// Подстройка под голос закончилась: итог строкой; `true` — запись начата заново.
    VoiceResult(String, bool),
    /// Первый запуск: обязательная подстройка под голос.
    VoiceIntro,
    /// Расстановка маскота не началась (он выключен).
    SettingsPlacing(bool),
    /// Прокрутить вкладку «Настройки» (проверка)
    ScrollSettings(f32),
    /// Выбрать действие на вкладке «Действия» (проверка)
    SelectAction(String),
}

/// Сведения для экранов, которые не приходят событиями.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub paused: bool,
    pub actions: usize,
    pub index: String,
    pub mode: String,
    pub hotkey: String,
    pub version: String,
    pub say: super::say::SayList,
    /// грамматика для редактора и встроенные фразы языков, которые не распознаются
    pub grammar: crate::grammar::Grammar,
    pub hidden_triggers: Vec<String>,
    /// действия и их встроенные фразы языков, которые не распознаются
    pub action_list: Vec<crate::actions::Action>,
    pub hidden_phrases: std::collections::HashMap<String, Vec<String>>,
    pub settings: super::settings_tab::SettingsInfo,
}

/// Картинки скина.
pub struct Assets {
    frame_color: ImageId,
    frame_normal: ImageId,
    btn: [(ImageId, ImageId); 3],
}

impl Assets {
    pub fn load(app: &AppHandle, images: &mut Images) -> Self {
        let l = |p: &str| crate::dialogs_native::loader(app, p);
        let mut add = |p: &str| images.add_loader(l(p));
        Self {
            frame_color: add("skin/frame_color.webp"),
            frame_normal: add("skin/frame_normal.webp"),
            btn: [
                (add("skin/btn_min_off.webp"), add("skin/btn_min_on.webp")),
                (add("skin/btn_shade_off.webp"), add("skin/btn_shade_on.webp")),
                (add("skin/btn_close_off.webp"), add("skin/btn_close_on.webp")),
            ],
        }
    }

    /// Форма окна по альфе рамки (windowRects в shell.js): пустоты между шипами пропускают
    /// клики; прямоугольники в координатах скина.
    pub fn shape(&self, images: &Images) -> Vec<[f64; 4]> {
        let (w, h) = (SKIN_W as usize, SKIN_H as usize);
        let Ok(mask) = images.alpha_mask(self.frame_color, w as u32, h as u32) else { return Vec::new() };
        shape_rects(&mask.alpha, w, h)
    }
}

/// Прямоугольники формы окна по альфе w×h: сама рамка, вся плита (отверстия под экраны — тоже
/// окно), +1 пиксель вокруг, кроме верхней полосы (там Windows обводит полупрозрачное светлым).
pub fn shape_rects(alpha: &[u8], w: usize, h: usize) -> Vec<[f64; 4]> {
    let mut on = vec![false; w * h];
    for (i, a) in alpha.iter().enumerate().take(w * h) {
        on[i] = *a > 14;
    }
    let (x0, y0, x1, y1, r) = PANEL;
    for y in y0 as usize..(y1 as usize).min(h) {
        let yf = y as f32;
        let dy = (y0 + r - yf).max(yf - (y1 - r)).max(0.0);
        let inset = if dy > 0.0 { r - (r * r - dy * dy).max(0.0).sqrt() } else { 0.0 };
        let (xa, xb) = ((x0 + inset).ceil() as usize, (x1 - inset).max(0.0) as usize);
        for x in xa..xb.min(w) {
            on[y * w + x] = true;
        }
    }
    const TOP: usize = 34;
    let mut grown = vec![false; w * h];
    for y in 0..TOP.min(h) {
        for x in 0..w {
            grown[y * w + x] = alpha[y * w + x] > 230;
        }
    }
    for y in TOP..h {
        for x in 0..w {
            if !on[y * w + x] {
                continue;
            }
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                if yy < TOP {
                    continue;
                }
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    grown[yy * w + xx] = true;
                }
            }
        }
    }
    // строки → отрезки → прямоугольники (одинаковые отрезки подряд склеиваются)
    let mut rects = Vec::new();
    let mut open: std::collections::HashMap<(usize, usize), usize> = std::collections::HashMap::new();
    for y in 0..=h {
        let mut spans = Vec::new();
        if y < h {
            let mut x = 0;
            while x < w {
                if grown[y * w + x] {
                    let s = x;
                    while x < w && grown[y * w + x] {
                        x += 1;
                    }
                    spans.push((s, x));
                } else {
                    x += 1;
                }
            }
        }
        let mut next = std::collections::HashMap::new();
        for key in spans {
            let start = open.remove(&key).unwrap_or(y);
            next.insert(key, start);
        }
        for ((a, b), start) in open.drain() {
            rects.push([a as f64, start as f64, b as f64, y as f64]);
        }
        open = next;
    }
    rects
}

/// Пружина shell.js: недодемпфированная — свет «взлетает» с перехлёстом.
#[derive(Debug, Clone, Copy)]
struct Spring {
    v: f32,
    t: f32,
    vel: f32,
    k: f32,
    c: f32,
}

impl Spring {
    const fn new(v: f32, k: f32, c: f32) -> Self {
        Self { v, t: v, vel: 0.0, k, c }
    }
    fn step(&mut self, dt: f32) {
        let a = self.k * (self.t - self.v) - self.c * self.vel;
        self.vel += a * dt;
        self.v += self.vel * dt;
    }
    /// Ещё движется (иначе — защёлкнуть в цель).
    fn moving(&mut self) -> bool {
        if (self.t - self.v).abs() > 0.012 || self.vel.abs() > 0.025 {
            true
        } else {
            self.v = self.t;
            self.vel = 0.0;
            false
        }
    }
}

/// Цели пружин по состоянию — STATES в shell.js.
struct StateTargets {
    fill: f32,
    energy: f32,
    wave: f32,
    swirl: f32,
    dark: f32,
    glitch: f32,
    orb_light: f32,
}

fn targets(state: &str) -> StateTargets {
    let t = |fill, energy, wave, swirl, dark, glitch, orb_light| StateTargets { fill, energy, wave, swirl, dark, glitch, orb_light };
    match state {
        "listening" => t(0.72, 1.0, 0.32, 0.0, 0.0, 0.0, 0.95),
        "busy" => t(0.9, 1.12, 0.22, 1.0, 0.0, 0.0, 1.15),
        "loading" => t(0.3, 0.6, 0.1, 0.3, 0.0, 0.0, 0.3),
        "paused" => t(0.0, 0.12, 0.0, 0.0, 1.0, 0.0, 0.0),
        "error" => t(0.22, 0.5, 0.1, 0.0, 0.3, 1.0, 0.3),
        _ => t(0.17, 0.55, 0.14, 0.0, 0.0, 0.0, 0.35),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Lunka(usize),
    Tab(usize),
    Pause,
    Orb,
    Web,
    /// строка «Что можно сказать» с редактором
    Row(usize),
}

struct Springs {
    fill: Spring,
    energy: Spring,
    wave: Spring,
    swirl: Spring,
    dark: Spring,
    glitch: Spring,
    orb_light: Spring,
    mouse_on: Spring,
    mx: Spring,
    my: Spring,
    lunki: [Spring; 3],
}

impl Springs {
    fn all(&mut self) -> [&mut Spring; 13] {
        let [l0, l1, l2] = &mut self.lunki;
        [
            &mut self.fill, &mut self.energy, &mut self.wave, &mut self.swirl, &mut self.dark, &mut self.glitch,
            &mut self.orb_light, &mut self.mouse_on, &mut self.mx, &mut self.my, l0, l1, l2,
        ]
    }
}

pub struct MainView {
    assets: Assets,
    host: Arc<dyn Fn(Act) + Send + Sync>,
    hwnd: isize,
    /// масштаб скина в окне (fitScale)
    k: f32,
    locale: String,
    rtl: bool,
    shape: Vec<[f64; 4]>,
    scene: Scene,
    tooltip: Tooltip,
    // шейдеры
    frame: Option<FrameShader>,
    frame_px: Option<FramePixels>,
    frame_failed: bool,
    lens: Option<Lens>,
    lens_failed: bool,
    // состояние
    state: String,
    tab: usize,
    partial: String,
    heard: Option<String>,
    log: Vec<String>,
    log_scroll: f32,
    /// прокрутка «Главной» (подсказка и список «Что можно сказать»), DIP
    home_scroll: f32,
    home_max_scroll: f32,
    /// строки списка на экране (с учётом прокрутки) и строка под курсором
    say_rects: Vec<Rect>,
    say_hover: Option<usize>,
    /// редактор «Что можно сказать» и его форма; срок следующего мигания каретки
    editor: Option<super::grammar_edit::GrammarEditor>,
    form: Form,
    form_blink: Option<Instant>,
    actions_tab: super::actions_tab::ActionsTab,
    settings_tab: super::settings_tab::SettingsTab,
    /// окно поверх (калибровка, голос, выбор программы)
    overlay: Option<super::overlays::Overlay>,
    /// компактный режим (полоска) и её содержимое
    shade: Option<super::shade::Shade>,
    shaded: bool,
    shade_pressed: Option<super::shade::ShadeHit>,
    // анимации экранов: включение (crtOn), смена канала (chOut/chIn), вспышка «принято»
    power_from: Instant,
    power_quick: bool,
    channel: Option<(Instant, usize)>,
    invert_until: Instant,
    /// бегущая строка: текст не влезает — рисуется в живом слое и едет
    ticker_over: f32,
    ticker_from: Instant,
    caret_phase: i64,
    /// раскрытую форму прокрутить в поле зрения (scrollIntoView nearest); где она в содержимом
    reveal: bool,
    editor_span: (f32, f32),
    status: serde_json::Value,
    info: Info,
    woke_until: Option<Instant>,
    level: f32,
    level_raw: f32,
    flash: f32,
    burst: f32,
    fx: Fx,
    /// угли в покое — только первые 1,1 с после открытия (idleEmbersUntil в shell.js)
    embers_until: Instant,
    eq: Vec<f32>,
    eq_acc: f32,
    t0: Instant,
    springs: Springs,
    // управление
    lunki: [Lunka; 3],
    pause: Button,
    web: Button,
    tab_hover: Option<usize>,
    orb_hover: bool,
    pressed: Option<Target>,
    // статичный слой (экраны, текст, рамка) — перерисовывается только когда что-то изменилось
    static_layer: Option<(ID2D1Bitmap1, (u32, u32), u64)>,
    static_dirty: bool,
    /// пока идут переходы наведения кнопок — слой обновляется каждый кадр
    static_live_until: Instant,
    gpu_generation: u64,
    /// фаза мигания точки состояния в прошлом кадре
    blink_phase: bool,
    // осьминог в сфере: свой вид маскота, рисуется в битмап линзы не чаще 30 раз в секунду
    orb: Option<MascotView>,
    orb_layer: Option<(ID2D1Bitmap1, (u32, u32), u64)>,
    orb_dirty: bool,
    orb_last: Instant,
}

impl MainView {
    pub fn new(
        assets: Assets,
        k: f32,
        locale: &str,
        snapshot: crate::events::UiSnapshot,
        info: Info,
        host: Arc<dyn Fn(Act) + Send + Sync>,
    ) -> Self {
        let lunka = |i: usize, c: (f32, f32), dome: f32, icon: &'static str, tip: &str| Lunka {
            center: c,
            box_size: 44.0,
            dome,
            icon,
            off: assets.btn[i].0,
            on: assets.btn[i].1,
            press: Press::default(),
            tip: crate::ui_text::t(locale, tip, &[]),
        };
        let lunki = [
            lunka(0, (965.0, 100.0), 22.0, "min", "win.minimize"),
            lunka(1, (1015.0, 100.0), 28.0, "shade", "win.shade"),
            lunka(2, (1065.0, 100.0), 30.0, "tray", "win.toTray"),
        ];
        let tab = TABS.iter().position(|t| *t == snapshot.tab).unwrap_or(0);
        let (list0, hidden0, settings0) = (info.action_list.clone(), info.hidden_phrases.clone(), info.settings.clone());
        let mut v = Self {
            host,
            hwnd: 0,
            k,
            locale: locale.to_string(),
            rtl: crate::ui_text::direction(locale) == "rtl",
            shape: Vec::new(),
            scene: Scene::default(),
            tooltip: Tooltip::default(),
            frame: None,
            frame_px: None,
            frame_failed: false,
            lens: None,
            lens_failed: false,
            state: "idle".into(),
            tab,
            partial: String::new(),
            heard: None,
            log: snapshot.log.into_iter().rev().take(LOG_LIMIT).rev().collect(),
            log_scroll: 0.0,
            home_scroll: 0.0,
            home_max_scroll: 0.0,
            say_rects: Vec::new(),
            say_hover: None,
            editor: None,
            form: Form::new(0, crate::ui_text::direction(locale) == "rtl"),
            form_blink: None,
            actions_tab: super::actions_tab::ActionsTab::new(locale, crate::ui_text::direction(locale) == "rtl", 0),
            settings_tab: super::settings_tab::SettingsTab::new(locale, crate::ui_text::direction(locale) == "rtl", 0),
            overlay: None,
            shade: None,
            shaded: false,
            shade_pressed: None,
            power_from: Instant::now(),
            power_quick: false,
            channel: None,
            invert_until: Instant::now(),
            ticker_over: 0.0,
            ticker_from: Instant::now(),
            caret_phase: 0,
            reveal: false,
            editor_span: (0.0, 0.0),
            status: snapshot.status.unwrap_or_default(),
            info,
            woke_until: None,
            level: 0.0,
            level_raw: 0.0,
            flash: 0.0,
            burst: 0.0,
            fx: Fx::new((ORB.0, ORB.1), ORB.2),
            embers_until: Instant::now() + Duration::from_millis(1100),
            eq: vec![0.0; ((EQ.w - 16.0) / 9.0) as usize],
            eq_acc: 0.0,
            t0: Instant::now(),
            springs: Springs {
                fill: Spring::new(0.0, 46.0, 7.5),
                energy: Spring::new(0.0, 30.0, 10.0),
                wave: Spring::new(0.0, 40.0, 12.0),
                swirl: Spring::new(0.0, 10.0, 6.0),
                dark: Spring::new(0.0, 18.0, 9.0),
                glitch: Spring::new(0.0, 60.0, 14.0),
                orb_light: Spring::new(0.0, 26.0, 9.0),
                mouse_on: Spring::new(0.0, 20.0, 9.0),
                mx: Spring::new(640.0, 90.0, 19.0),
                my: Spring::new(200.0, 90.0, 19.0),
                lunki: [Spring::new(0.0, 60.0, 13.0); 3],
            },
            lunki,
            pause: Button::new("", ButtonKind::Plain),
            web: Button::new("", ButtonKind::Default),
            tab_hover: None,
            orb_hover: false,
            pressed: None,
            static_layer: None,
            static_dirty: true,
            static_live_until: Instant::now(),
            gpu_generation: 0,
            blink_phase: true,
            orb: None,
            orb_layer: None,
            orb_dirty: true,
            orb_last: Instant::now(),
            assets,
        };
        // кнопка в вебе сжата flex-колонкой до ~17 px и стоит под строкой «Хоткей»
        v.pause.rect = Rect::new(STATUS.x + 12.0, STATUS.y + 137.0, STATUS.w - 24.0, 17.0);
        v.pause.font_size = 15.0;
        v.web.label = v.t("main.openWeb");
        v.set_state(&snapshot.state);
        v.actions_tab.sync(list0, hidden0);
        v.settings_tab.sync(settings0);
        v
    }

    fn t(&self, key: &str) -> String {
        crate::ui_text::t(&self.locale, key, &[])
    }

    /// Осьминог в сфере (вид маскота с раскладкой линзы); `inside` — сидит ли он там сейчас.
    pub fn set_orb_mascot(&mut self, mut view: MascotView, inside: bool) {
        view.orb_layout(CORE.w);
        view.orb_action(if inside { "present" } else { "absent" });
        self.orb = Some(view);
        self.orb_dirty = true;
    }

    /// Полоска компактного режима (создаётся вместе с окном: нужны картинки и AppHandle).
    pub fn set_shade_view(&mut self, s: super::shade::Shade) {
        self.shade = Some(s);
    }

    /// Картинки лунок «шторка» и «в трей» — для полоски.
    pub fn shade_buttons(&self) -> [(ImageId, ImageId); 2] {
        [self.assets.btn[1], self.assets.btn[2]]
    }

    /// Чья альфа задаёт форму окна: рамки полоски или главной рамки.
    pub fn shape_source(&self, shaded: bool) -> (ImageId, u32, u32) {
        match (&self.shade, shaded) {
            (Some(s), true) => s.shape_source(),
            _ => (self.assets.frame_color, SKIN_W as u32, SKIN_H as u32),
        }
    }

    /// Масштаб скина в окне.
    pub fn k(&self) -> f32 {
        self.k
    }

    pub fn shaded(&self) -> bool {
        self.shaded
    }

    /// Включить или выключить полоску (окно уже переразмерено, форма — `shape`).
    pub fn set_shaded(&mut self, on: bool, shape: Vec<[f64; 4]>) {
        self.shaded = on;
        self.shape = shape;
        if !on {
            self.power_on(true);
        }
        self.tooltip.hide();
        self.overlay = None;
        self.static_dirty = true;
    }

    pub fn set_shape(&mut self, shape: Vec<[f64; 4]>) {
        self.shape = shape;
    }

    fn set_state(&mut self, name: &str) {
        let was = std::mem::replace(&mut self.state, name.to_string());
        if matches!(name, "idle" | "paused") {
            self.embers_until = Instant::now();
        }
        if matches!(name, "listening" | "busy") {
            self.woke_until = None;
        } else if name == "paused" || matches!(was.as_str(), "listening" | "busy") {
            self.woke_until = None;
        }
        // уровень приходит только пока слушаем: после команды — тишина, иначе эквалайзер горит
        if !matches!(name, "listening" | "busy") {
            self.level_raw = 0.0;
        }
        let t = targets(name);
        let s = &mut self.springs;
        s.fill.t = t.fill;
        s.energy.t = t.energy;
        s.wave.t = t.wave;
        s.swirl.t = t.swirl;
        s.dark.t = t.dark;
        s.glitch.t = t.glitch;
        s.orb_light.t = t.orb_light;
    }

    pub fn on_msg(&mut self, msg: Msg) {
        // уровень и wake меняют только живой слой (линза, рой, эквалайзер)
        if !matches!(msg, Msg::Level(_) | Msg::Wake(_)) {
            self.static_dirty = true;
        }
        if let Some(orb) = self.orb.as_mut() {
            let m = match &msg {
                Msg::State(s) => Some(MascotMsg::State(s.clone())),
                Msg::Wake(true) => Some(MascotMsg::Wake),
                Msg::Wake(false) => Some(MascotMsg::WakeRejected),
                Msg::Level(v) => Some(MascotMsg::Level(*v)),
                Msg::Heard(h) if !h.trim().is_empty() => Some(MascotMsg::Heard),
                _ => None,
            };
            if let Some(m) = m {
                orb.on_msg(m);
            }
            if let Msg::Orb(action) = &msg {
                orb.orb_action(action);
                self.orb_dirty = true;
            }
        }
        match msg {
            Msg::State(s) => self.set_state(&s),
            Msg::Partial(p) => {
                if p != self.partial {
                    self.ticker_from = Instant::now();
                }
                self.partial = p;
            }
            Msg::Heard(h) => {
                let h = h.trim().to_string();
                if !h.is_empty() {
                    self.heard = Some(h);
                    self.flash = 1.0;
                    self.burst = 1.0;
                    self.invert_until = Instant::now() + Duration::from_millis(95);
                }
            }
            Msg::Log(line) => {
                self.log.push(line);
                if self.log.len() > LOG_LIMIT {
                    let extra = self.log.len() - LOG_LIMIT;
                    self.log.drain(..extra);
                }
            }
            Msg::Level(rms) => {
                self.level_raw = if matches!(self.state.as_str(), "listening" | "busy") {
                    ((20.0 * (rms + 1e-6).log10() + 54.0) / 40.0).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
            Msg::Wake(on) => self.woke_until = on.then(|| Instant::now() + Duration::from_secs(8)),
            Msg::Status(s) => {
                if s.is_object() {
                    self.status = s;
                }
            }
            Msg::Info(i) => {
                self.actions_tab.sync(i.action_list.clone(), i.hidden_phrases.clone());
                if let Some(o) = self.overlay.as_mut() {
                    if let super::overlays::Kind::Calibration { id, label, count, last } = &mut o.kind {
                        if let Some((l, phrases)) = self.actions_tab.shown_phrases(id) {
                            *label = l;
                            *count = phrases.len();
                            *last = phrases.last().cloned();
                        }
                    }
                }
                self.settings_tab.sync(i.settings.clone());
                self.info = i;
            }
            Msg::Orb(_) => {}
            Msg::GrammarDone(err) => match err {
                None => self.close_editor(),
                Some(e) => {
                    if let Some(ed) = self.editor.as_mut() {
                        ed.errors = e.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
                    }
                    self.rebuild_form();
                }
            },
            Msg::SettingsMsg(m) => self.settings_tab.msg = m,
            Msg::TestOverlay(which) => match which.as_str() {
                "calibration" => {
                    let id = self.info.action_list.first().map(|a| a.id.clone()).unwrap_or_default();
                    let (label, phrases) = self.actions_tab.shown_phrases(&id).unwrap_or_default();
                    self.open_overlay(super::overlays::Kind::Calibration { id, label, count: phrases.len(), last: phrases.last().cloned() });
                }
                "voice" => self.open_overlay(super::overlays::Kind::Voice { words: 6, result: String::new(), recording: true, wake: "reever".into(), intro: false }),
                "apps" => {
                    self.open_overlay(super::overlays::Kind::Apps { list: None });
                    (self.host)(Act::ListApps);
                }
                "scan" => {
                    self.open_overlay(super::overlays::Kind::Scan { apps: None });
                    (self.host)(Act::Scan);
                }
                _ => self.overlay = None,
            },
            Msg::ScanApps(apps) => {
                if let Some(o) = self.overlay.as_mut() {
                    o.set_scan(apps);
                }
            }
            Msg::ScanIcons(icons) => {
                if let Some(o) = self.overlay.as_mut() {
                    o.set_icons(&icons);
                }
            }
            Msg::Apps(list) => {
                if let Some(o) = self.overlay.as_mut() {
                    if let super::overlays::Kind::Apps { list: l } = &mut o.kind {
                        *l = Some(list);
                    }
                }
            }
            Msg::VoiceWords(n) => {
                if let Some(o) = self.overlay.as_mut() {
                    if let super::overlays::Kind::Voice { words, recording: true, .. } = &mut o.kind {
                        if n > *words {
                            self.flash = 0.8; // вспышка шарика: слово засчитано (Skin.pulse)
                        }
                        *words = n;
                    }
                }
            }
            Msg::VoiceResult(text, restart) => {
                if let Some(o) = self.overlay.as_mut() {
                    o.voice_result(text, restart);
                }
                if restart {
                    self.set_state("listening");
                }
            }
            Msg::VoiceIntro => {
                if self.overlay.is_none() {
                    let wake = if self.info.say.ru { "ривер" } else { "reever" };
                    self.open_overlay(super::overlays::Kind::Voice { words: 0, result: String::new(), recording: true, wake: wake.into(), intro: true });
                    self.set_state("listening");
                    (self.host)(Act::VoiceStart);
                }
            }
            Msg::SettingsPlacing(on) => self.settings_tab.set_placing(on),
            Msg::ScrollSettings(d) => {
                self.settings_tab.scroll_by(d);
            }
            Msg::SelectAction(id) => {
                self.tab = 1;
                let id = if id.is_empty() { self.info.action_list.first().map(|a| a.id.clone()).unwrap_or_default() } else { id };
                self.actions_tab.open(&id);
            }
            Msg::EditGrammar(key) => {
                self.tab = 0;
                self.open_editor(&key);
            }
            Msg::Tab(t) => {
                if let Some(i) = TABS.iter().position(|x| *x == t) {
                    self.tab = i;
                    self.log_scroll = 0.0;
                    self.home_scroll = 0.0;
                }
            }
        }
    }

    fn active(&self) -> bool {
        matches!(self.state.as_str(), "listening" | "busy" | "loading") || self.orb_hover || self.pressed.is_some()
    }

    // ── Свет рамки ────────────────────────────────────────────────────
    fn light(&self) -> FrameLight {
        let lunk = |i: usize, on: f32| [self.lunki[i].center.0, self.lunki[i].center.1, on];
        if FRAME_STATIC {
            return FrameLight {
                mouse: (0.0, 0.0),
                mouse_on: 0.0,
                orb: (ORB.0, ORB.1),
                orb_light: targets("idle").orb_light,
                sweep_x: -1.0,
                sweep_i: 0.0,
                reveal: 1800.0,
                lunki: [lunk(0, 0.0), lunk(1, 0.0), lunk(2, 0.0)],
            };
        }
        let s = &self.springs;
        FrameLight {
            mouse: (s.mx.v, s.my.v),
            mouse_on: s.mouse_on.v,
            orb: (ORB.0, ORB.1),
            orb_light: (s.orb_light.v + self.flash * 1.5).max(0.0),
            sweep_x: -1.0,
            sweep_i: 0.0,
            reveal: 1800.0,
            lunki: [lunk(0, s.lunki[0].v), lunk(1, s.lunki[1].v), lunk(2, s.lunki[2].v)],
        }
    }

    // ── Рисование ─────────────────────────────────────────────────────
    fn draw_frame(&mut self, cx: &mut RenderCx<'_>) {
        let r = Rect::new(0.0, 0.0, SKIN_W, SKIN_H);
        if let Some(f) = &self.frame {
            unsafe {
                cx.dc().DrawBitmap(f.bitmap(), Some(&r.d2d()), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        } else {
            // шейдера ещё нет (первый кадр) или он не собрался — рамка просто картинкой
            cx.draw_image(self.assets.frame_color, r, 1.0);
        }
    }

    fn text(&mut self, cx: &mut RenderCx<'_>, s: &str, style: &TextStyle, width: f32) -> Option<Rc<native::TextLayout>> {
        self.scene.cache.get(cx.text(), s, style, width)
    }

    fn draw_title(&mut self, cx: &mut RenderCx<'_>) {
        let r = TITLE;
        lcd_background(cx, r, 8.0);
        let logo = TextStyle { ..lcd(21.0, 700, 0.14) };
        let mut x = r.x + 10.0;
        if let Some(l) = self.text(cx, "REEVER", &logo, 400.0) {
            self.scene.text(&l, x, r.y + (r.h - l.height) / 2.0, ACC_HOT, Glow::G3);
            x += l.width + 8.0;
        }
        let ver = format!("v{}", self.info.version);
        if let Some(l) = self.text(cx, &ver, &lcd(13.0, 400, 0.06), 200.0) {
            self.scene.text(&l, x, r.y + (r.h - l.height) / 2.0, ACC_3, Glow::G1);
        }
        // лампочки: индекс готов, идёт запись
        let net = self.info.index == "ready";
        let rec = matches!(self.state.as_str(), "listening" | "busy");
        for (i, on) in [net, rec].into_iter().enumerate() {
            let c = Rect::new(r.x + r.w - 10.0 - 6.0 - (1 - i) as f32 * 11.0, r.y + 12.0, 6.0, 6.0);
            if on {
                self.scene.shadow(cx, c, 3.0, ACC, 6.0);
            }
            self.scene.flush(cx);
            fill_round(cx, c, 3.0, if on { ACC } else { Color::rgba(39, 230, 170, 18) });
        }
        self.scene.flush(cx);
        lcd_overlay(cx, r, 8.0);
    }

    /// Точка состояния: мигает при «слушаю»/«выполняю».
    fn draw_dot(&mut self, cx: &mut RenderCx<'_>) {
        let r = STATUS;
        let now = Instant::now();
        let blink = |period: f32| ((now.duration_since(self.t0).as_secs_f32() / (period / 2.0)) as i64) % 2 == 0;
        let dot = Rect::new(r.x + 12.0, r.y + 20.0 + 7.0, 10.0, 10.0);
        let (dot_c, glow_r, show) = match self.state.as_str() {
            "listening" => (ACC, 8.0, blink(1.0)),
            "busy" => (ACC_HOT, 10.0, blink(0.35)),
            _ => (ACC_4, 4.0, true),
        };
        if show {
            self.scene.shadow(cx, dot, 5.0, dot_c, glow_r);
            self.scene.flush(cx);
            fill_round(cx, dot, 5.0, dot_c);
        }
    }

    fn pill(&self, key: &str) -> String {
        self.t(key)
    }

    fn draw_status(&mut self, cx: &mut RenderCx<'_>) {
        let r = STATUS;
        lcd_background(cx, r, 12.0);
        let (x0, x1) = (r.x + 12.0, r.x + r.w - 12.0);
        // точка состояния: мигает — слой обновляется на каждом переключении (advance)
        self.draw_dot(cx);
        let key = match self.state.as_str() {
            s @ ("listening" | "busy" | "loading" | "paused" | "error") => format!("mic.{s}"),
            _ => "mic.idle".into(),
        };
        let label = self.t(&key).to_uppercase();
        let st = TextStyle { ellipsis: true, ..lcd(24.0, 600, 0.06) };
        if let Some(l) = self.text(cx, &label, &st, x1 - x0 - 19.0) {
            self.scene.text(&l, x0 + 19.0, r.y + 20.0 + (24.0 - l.height) / 2.0, ACC_HOT, Glow::G3);
        }
        // показания
        let paused = self.info.paused;
        let asr = self.status.get("asr").and_then(|v| v.as_str()).unwrap_or("");
        let model = self.status.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("-").to_string();
        let index = match self.info.index.as_str() {
            "ready" => ("pill.indexReady", 1),
            "scanning" => ("pill.indexing", 0),
            "off" => ("pill.off", 0),
            _ => ("pill.unchecked", 0),
        };
        let mode_key = format!("mode.{}", self.info.mode);
        // нет перевода — t() отдаёт сам ключ: тогда как есть (I18N.has в app.js)
        let mode = Some(self.t(&mode_key)).filter(|m| *m != mode_key).unwrap_or_else(|| self.info.mode.clone());
        let hotkey = if self.info.mode == "wake" || self.info.hotkey.is_empty() { "-".to_string() } else { self.info.hotkey.clone() };
        // (подпись, значение, 1 — ok / -1 — плохо / 0 — обычное)
        let rows: [(&str, String, i32); 6] = [
            ("tile.mic", if paused { self.pill("pill.paused") } else { self.pill("pill.active") }, if paused { -1 } else { 1 }),
            (
                "tile.asr",
                match asr {
                    "ready" => self.pill("pill.ready"),
                    "error" => self.pill("pill.error"),
                    "loading" => self.pill("pill.loading"),
                    _ => "-".into(),
                },
                match asr {
                    "ready" => 1,
                    "error" => -1,
                    _ => 0,
                },
            ),
            ("tile.model", model, 0),
            ("tile.index", self.pill(index.0), index.1),
            ("tile.activation", mode, 0),
            ("tile.hotkey", hotkey, 0),
        ];
        let dt = ui(11.0, 400, 15.0 / 11.0);
        let dd = TextStyle { ellipsis: true, align: Align::End, letter_spacing: -0.2, ..data(10.0, 1.5) };
        let mut y = r.y + 20.0 + 24.0 + 4.0;
        for (label, value, kind) in rows {
            let label = self.t(label);
            let mut lw = 0.0;
            if let Some(l) = self.text(cx, &label, &dt, 200.0) {
                self.scene.text(&l, x0, y + (15.0 - l.height) / 2.0, ACC_3, Glow::G1);
                lw = l.width;
            }
            let vw = x1 - x0 - lw - 10.0;
            if let Some(l) = self.text(cx, &value, &dd, vw.max(10.0)) {
                let (c, g, a) = match kind {
                    1 => (ACC_HOT, Glow::G3, 1.0),
                    -1 => (ACC, Glow::G2, 0.8),
                    _ => (ACC, Glow::G2, 1.0),
                };
                let lx = x1 - vw.max(10.0);
                let ty = y + (15.0 - l.height) / 2.0;
                self.scene.text_alpha(&l, lx, ty, c, g, a);
                if kind == -1 {
                    // .pill.bad — зачёркнуто
                    let line_x = x1 - l.width.min(vw);
                    self.scene.flush(cx);
                    cx.fill_rect(Rect::new(line_x, ty + l.height / 2.0, l.width.min(vw), 1.0), with_alpha(ACC, 0.8));
                }
            }
            y += 15.0;
        }
        self.pause.label = self.t(if paused { "mic.resume" } else { "mic.pause" });
        self.scene.flush(cx);
        self.pause.draw(&mut self.scene, cx);
        self.scene.flush(cx);
        lcd_overlay(cx, r, 12.0);
    }

    fn tab_rect(i: usize) -> Rect {
        // рельс: 4 клавиши поровну, внизу — счётчик действий (~52 px)
        let inner = Rect::new(RAIL.x + 6.0, RAIL.y + 8.0, RAIL.w - 12.0, RAIL.h - 16.0);
        let counter = 52.0;
        let h = (inner.h - counter - 4.0 * 4.0) / 4.0;
        Rect::new(inner.x, inner.y + i as f32 * (h + 4.0), inner.w, h)
    }

    fn draw_rail(&mut self, cx: &mut RenderCx<'_>) {
        let r = RAIL;
        lcd_background(cx, r, 12.0);
        for i in 0..4 {
            let tr = Self::tab_rect(i);
            let on = i == self.tab;
            let hover = self.tab_hover == Some(i) && !on;
            if on || hover {
                // .navi.on: полоса фона по горизонтали прозрачная к краям
                let bg = ACC_5;
                let third = tr.w * 0.3;
                fill_round(cx, Rect::new(tr.x + third, tr.y, tr.w - 2.0 * third, tr.h), 0.0, bg);
                for k in 0..6 {
                    let a = (k as f32 + 0.5) / 6.0;
                    let w = third / 6.0;
                    let c = with_alpha(bg, bg.a * a);
                    fill_round(cx, Rect::new(tr.x + k as f32 * w, tr.y, w, tr.h), 0.0, c);
                    fill_round(cx, Rect::new(tr.x + tr.w - (k as f32 + 1.0) * w, tr.y, w, tr.h), 0.0, c);
                }
            }
            if on {
                let bar = Rect::new(r.x, tr.y + tr.h * 0.18, 2.0, tr.h * 0.64);
                self.scene.shadow(cx, bar, 1.0, ACC, 8.0);
                self.scene.flush(cx);
                fill_round(cx, bar, 1.0, ACC);
            }
            let (c, g) = if on { (ACC_HOT, Glow::G3) } else if hover { (ACC_2, Glow::G1) } else { (ACC_3, Glow::G1) };
            let label = self.t(TAB_KEYS[i]).to_uppercase();
            // кегль уменьшается, пока слово не влезет (fitRail): 14 → 10
            let mut layout = None;
            for fs in (10..=14).rev() {
                let st = lcd(fs as f32, 500, 0.04);
                if let Some(l) = self.text(cx, &label, &st, 1000.0) {
                    if l.width <= tr.w || fs == 10 {
                        layout = Some(l);
                        break;
                    }
                }
            }
            let lh = layout.as_ref().map_or(0.0, |l| l.height);
            let block = 24.0 + 9.0 + lh;
            let top = tr.y + (tr.h - block) / 2.0;
            self.scene.icon(TAB_ICONS[i], Rect::new(tr.x + (tr.w - 24.0) / 2.0, top, 24.0, 24.0), 1.6, c, false, g);
            if let Some(l) = layout {
                self.scene.text(&l, tr.x + (tr.w - l.width.min(tr.w)) / 2.0, top + 33.0, c, g);
            }
        }
        // счётчик действий
        let last = Self::tab_rect(3);
        let cy = last.y + last.h + 4.0;
        self.scene.flush(cx);
        dotted_hline(cx, last.x, cy, last.w, ACC_4);
        let n = self.info.actions.to_string();
        if let Some(l) = self.text(cx, &n, &lcd(30.0, 700, 0.0), 200.0) {
            self.scene.text(&l, last.x + (last.w - l.width) / 2.0, cy + 6.0, ACC_HOT, Glow::G3);
        }
        let cap = self.t("tile.actions").to_uppercase();
        let st = TextStyle { align: Align::Center, wrap: true, ..lcd(11.0, 400, 0.08) };
        if let Some(l) = self.text(cx, &cap, &st, last.w) {
            self.scene.text(&l, last.x, cy + 38.0, ACC_3, Glow::G1);
        }
        self.scene.flush(cx);
        lcd_overlay(cx, r, 12.0);
    }

    fn draw_screen(&mut self, cx: &mut RenderCx<'_>) {
        let r = SCREEN;
        lcd_background(cx, r, 14.0);
        // бегущая строка
        let tk = Rect::new(r.x, r.y, r.w, 34.0);
        fill_round(cx, tk, 0.0, with_alpha(ACC_5, ACC_5.a * 0.5));
        cx.fill_rect(Rect::new(r.x, r.y + 33.0, r.w, 1.0), ACC_5);
        self.scene.icon("play", Rect::new(r.x + 14.0, r.y + 10.5, 13.0, 13.0), 0.0, ACC, true, Glow::G2);
        let partial = if self.partial.trim().is_empty() { "…".to_string() } else { self.partial.to_uppercase() };
        let tape_w = r.w - 14.0 - 13.0 - 8.0 - 3.0 - 14.0;
        let wide = TextStyle { direction: self.dir(), ..lcd(21.0, 500, 0.05) };
        let over = self.text(cx, &partial, &wide, 100_000.0).map_or(0.0, |l| l.width - tape_w);
        self.ticker_over = over;
        if over <= 4.0 {
            if let Some(l) = self.text(cx, &partial, &wide, tape_w) {
                self.scene.text(&l, r.x + 14.0 + 13.0 + 8.0 + 3.0, r.y + (34.0 - l.height) / 2.0, ACC_HOT, Glow::G3);
            }
        }
        // содержимое вкладки
        let body = Rect::new(r.x + 18.0, r.y + 34.0 + 14.0, r.w - 36.0, r.h - 34.0 - 14.0 - 18.0);
        self.web.rect = Rect::new(0.0, 0.0, 0.0, 0.0);
        // смена канала: старое схлопывается в строку (0,14 с), новое раскрывается (0,32 с)
        let now = cx.now;
        let mut shown = self.tab;
        let mut ch: Option<(f32, f32, f32)> = None; // scale x, scale y, яркость
        if let Some((t, old)) = self.channel {
            let e = now.duration_since(t).as_secs_f32();
            if e < 0.14 {
                shown = old;
                let k = e / 0.14;
                let k = Bez(0.7, 0.0, 1.0, 0.5).at(k);
                // 0 → 70%: сжатие по высоте, 70 → 100%: по ширине
                let (sx, sy): (f32, f32) = if k < 0.7 { (1.0, 1.0 - (1.0 - 0.012) * k / 0.7) } else { (1.0 - (k - 0.7) / 0.3, 0.012) };
                ch = Some((sx.max(0.0), sy, 1.0 + 3.0 * k));
            } else if e < 0.46 {
                let k = E_OUT.at((e - 0.14) / 0.32);
                let (sx, sy) = if k < 0.35 { (0.02 + 0.98 * k / 0.35, 0.012) } else { (1.0, 0.012 + 0.988 * (k - 0.35) / 0.65) };
                ch = Some((sx, sy, 4.0 - 3.0 * k));
            } else {
                self.channel = None;
            }
        }
        let mut old_transform = windows::Foundation::Numerics::Matrix3x2::default();
        if let Some((sx, sy, _)) = ch {
            self.scene.flush(cx);
            let (cxm, cym) = (body.x + body.w / 2.0, body.y + body.h / 2.0);
            unsafe {
                cx.dc().GetTransform(&mut old_transform);
                let m = windows::Foundation::Numerics::Matrix3x2 { M11: sx.max(0.0001), M12: 0.0, M21: 0.0, M22: sy, M31: cxm * (1.0 - sx), M32: cym * (1.0 - sy) };
                cx.dc().SetTransform(&(m * old_transform));
            }
        }
        let real_tab = self.tab;
        self.tab = shown;
        match TABS[self.tab] {
            "home" => self.draw_home(cx, body),
            "actions" => {
                self.actions_tab.draw(&mut self.scene, cx, body);
                self.form_blink = self.actions_tab.next_blink(cx.now);
            }
            "settings" => {
                self.settings_tab.draw(&mut self.scene, cx, body);
                self.form_blink = self.settings_tab.form.next_blink(cx.now);
            }
            "logs" => self.draw_logs(cx, body),
            _ => self.draw_not_ported(cx, body),
        }
        self.tab = real_tab;
        self.scene.flush(cx);
        if let Some((_, _, bright)) = ch {
            unsafe { cx.dc().SetTransform(&old_transform) };
            if bright > 1.0 {
                let line = Rect::new(body.x, body.y + body.h / 2.0 - 3.0, body.w, 6.0);
                unsafe { cx.dc().SetPrimitiveBlend(windows::Win32::Graphics::Direct2D::D2D1_PRIMITIVE_BLEND_ADD) };
                cx.fill_rect(line, Color { a: ((bright - 1.0) * 0.12).min(0.5), ..ACC_HOT });
                unsafe { cx.dc().SetPrimitiveBlend(windows::Win32::Graphics::Direct2D::D2D1_PRIMITIVE_BLEND_SOURCE_OVER) };
            }
        }
        // «команда принята»: экран на миг вспыхивает (invert 95 мс)
        if now < self.invert_until {
            fill_round(cx, Rect::new(r.x, r.y, r.w, r.h), 14.0, Color { a: 0.82, ..ACC });
        }
        self.scene.flush(cx);
        lcd_overlay(cx, r, 14.0);
    }

    fn dir(&self) -> Direction {
        if self.rtl { Direction::Rtl } else { Direction::Ltr }
    }

    /// «Главная» (#p-home): услышанная команда или подсказка, «Что можно сказать» — список
    /// примеров (renderGrammarExamples). Всё вместе прокручивается колесом, как .panel.
    fn draw_home(&mut self, cx: &mut RenderCx<'_>, body: Rect) {
        // панель прокручивается в своих границах: падинги экрана — внутри прокрутки
        let clip = Rect::new(body.x - 18.0, body.y - 14.0, body.w + 36.0, body.h + 14.0 + 18.0);
        self.scene.flush(cx);
        self.scene.set_clip(Some(clip));
        unsafe {
            cx.dc().PushAxisAlignedClip(&clip.d2d(), windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
        }
        let top = body.y - self.home_scroll;
        // .heard: последняя услышанная команда или подсказка
        let (text, style, color, glow) = match &self.heard {
            Some(h) => (h.clone(), TextStyle { wrap: true, direction: self.dir(), line_height: Some(1.05), ..lcd(30.0, 500, 0.03) }, ACC_HOT, Glow::G3),
            None => (
                self.info.say.hint.clone(),
                TextStyle { wrap: true, direction: self.dir(), ..ui(15.0, 400, 1.45) },
                ACC_2,
                Glow::G1,
            ),
        };
        let mut h = 64.0f32;
        // .heard::after: блок-курсор после текста, мигает (1,06 с, через раз — тусклее)
        let caret_w = style.size * 0.55;
        if let Some(l) = self.text(cx, &text, &style, body.w - caret_w - 4.0) {
            self.scene.text(&l, body.x, top + 4.0, color, glow);
            h = h.max(4.0 + l.height + 14.0);
            let (ex, ey, eh) = l.end_point(text.encode_utf16().count() as u32);
            let on = (cx.now.duration_since(self.t0).as_secs_f32() / 0.53) as i64 % 2 == 0;
            let a = if on { 1.0 } else { 0.25 };
            let block = Rect::new(body.x + ex + 4.0, top + 4.0 + ey + (eh - style.size) / 2.0 + style.size * 0.12, caret_w, style.size);
            self.scene.shadow(cx, block, 0.0, Color { a: 0.8 * a, ..ACC }, 8.0);
            cx.fill_rect(block, Color { a, ..ACC });
        }
        self.scene.flush(cx);
        dotted_hline(cx, body.x, top + h, body.w, ACC_4);
        // .pack-warn: включён язык без скачанного пакета (renderPackWarning)
        if let Some(text) = self.pack_warning() {
            let st = TextStyle { wrap: true, direction: self.dir(), ..ui(13.0, 400, 1.4) };
            if let Some(l) = self.text(cx, &text, &st, body.w - 20.0) {
                let r = Rect::new(body.x, top + h + 12.0 - 4.0, body.w, l.height + 12.0);
                let mut dx = r.x;
                while dx < r.x + r.w {
                    cx.fill_rect(Rect::new(dx, r.y, 1.0, 1.0), ACC_4);
                    cx.fill_rect(Rect::new(dx, r.y + r.h - 1.0, 1.0, 1.0), ACC_4);
                    dx += 2.0;
                }
                let mut dy = r.y;
                while dy < r.y + r.h {
                    cx.fill_rect(Rect::new(r.x, dy, 1.0, 1.0), ACC_4);
                    cx.fill_rect(Rect::new(r.x + r.w - 1.0, dy, 1.0, 1.0), ACC_4);
                    dy += 2.0;
                }
                self.scene.text(&l, r.x + 10.0, r.y + 6.0, ACC_HOT, Glow::None);
                h += r.h - 4.0 + 12.0;
            }
        }
        // .grammar-heading: заголовок слева, «Изменить» справа (редактор пока в веб-версии)
        let mut y = top + h + 12.0;
        let general = self.editor.as_ref().is_some_and(|e| e.open == "general");
        self.web.label = self.t(if general { "grammar.closeEditor" } else { "grammar.edit" });
        let edit_w = self.web.natural_width(&mut self.scene, cx);
        self.web.rect = Rect::new(body.x + body.w - edit_w, y, edit_w, 28.0);
        let h2 = self.t("grammar.heading").to_uppercase();
        if let Some(l) = self.text(cx, &h2, &lcd(18.0, 600, 0.08), body.w - edit_w - 12.0) {
            self.scene.text(&l, body.x, y + (28.0 - l.height) / 2.0, ACC_HOT, Glow::G3);
        }
        self.scene.flush(cx);
        self.web.draw(&mut self.scene, cx);
        y += 28.0 + 6.0;
        if general {
            let start = y;
            y = self.draw_editor(cx, body, y);
            self.editor_span = (start - top, y - top);
        }
        // строки: номер, команда, что произойдёт — сетка 26px | auto | 1fr, по базовой линии
        let rows = self.info.say.rows.clone();
        self.say_rects.clear();
        let num_st = lcd(15.0, 500, 0.0);
        let code_st = TextStyle { line_height: Some(1.25), ..lcd(17.0, 500, 0.04) };
        let desc_st = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
        for (i, row) in rows.iter().enumerate() {
            let x0 = body.x + 6.0;
            let num = self.text(cx, &format!("{:02}.", i + 1), &num_st, 26.0);
            let code = self.text(cx, &row.command, &code_st, 10_000.0);
            let code_w = code.as_ref().map_or(0.0, |l| l.width);
            let desc_x = x0 + 26.0 + 8.0 + code_w + 8.0;
            let desc = self.text(cx, &row.desc, &desc_st, (body.x + body.w - 6.0 - desc_x).max(60.0));
            let items = [num.as_ref(), code.as_ref(), desc.as_ref()];
            let line_base = items.iter().flatten().map(|l| l.baseline()).fold(0.0f32, f32::max);
            let bottom = items.iter().flatten().map(|l| line_base - l.baseline() + l.height).fold(0.0f32, f32::max);
            let rh = bottom + 6.0;
            let rect = Rect::new(body.x, y, body.w, rh);
            let open = row.key.is_some() && self.editor.as_ref().is_some_and(|e| Some(&e.open) == row.key.as_ref());
            let hover = self.say_hover == Some(i) || open;
            if hover {
                fill_round(cx, rect, 4.0, ACC_5);
            }
            let alpha = if row.off { 0.45 } else { 1.0 };
            let ty = |l: &Rc<native::TextLayout>| y + 3.0 + line_base - l.baseline();
            if let Some(l) = &num {
                self.scene.text(l, x0, ty(l), ACC_3, Glow::G1);
            }
            if let Some(l) = &code {
                self.scene.text_alpha(l, x0 + 34.0, ty(l), ACC_HOT, Glow::G2, alpha);
            }
            if let Some(l) = &desc {
                self.scene.text_alpha(l, desc_x, ty(l), if hover { ACC_2 } else { ACC_3 }, Glow::G1, alpha);
            }
            self.say_rects.push(rect);
            let row_top = y;
            y += rh;
            if open {
                y = self.draw_editor(cx, body, y);
                self.editor_span = (row_top - top, y - top);
            }
        }
        self.home_max_scroll = (y + self.home_scroll - body.y + 4.0 - body.h).max(0.0);
        self.home_scroll = self.home_scroll.min(self.home_max_scroll);
        if std::mem::take(&mut self.reveal) && self.editor.is_some() {
            let (a, b) = self.editor_span;
            let mut s = self.home_scroll;
            if b - a > body.h || a < s {
                s = a;
            } else if b > s + body.h {
                s = b - body.h;
            }
            let s = s.clamp(0.0, self.home_max_scroll);
            if s != self.home_scroll {
                self.home_scroll = s;
                self.static_dirty = true; // ещё кадр с новой прокруткой
            }
        }
        self.scene.flush(cx);
        self.scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
        // полоса прокрутки, когда список не влезает
        if self.home_max_scroll > 0.0 {
            let track = Rect::new(SCREEN.x + SCREEN.w - 7.0, body.y, 3.0, body.h);
            let total = body.h + self.home_max_scroll;
            let th = (track.h * body.h / total).max(24.0);
            let ty = track.y + (track.h - th) * self.home_scroll / self.home_max_scroll;
            fill_round(cx, Rect::new(track.x, ty, track.w, th), 1.5, ACC_4);
        }
    }

    /// Языки без скачанного пакета: строка для «Главной» или None.
    fn pack_warning(&self) -> Option<String> {
        let s = &self.info.settings;
        let packs = s.models.get("languages")?;
        let installed: Vec<&str> = s.models.get("installed")?.as_array()?.iter().filter_map(|v| v.as_str()).collect();
        let chosen = s.settings.get("languages")?.as_array()?;
        let mut pack = None;
        let missing: Vec<&str> = chosen
            .iter()
            .filter_map(|c| c.as_str())
            .filter(|c| {
                let p = packs.get(*c).and_then(|p| p.as_str());
                let miss = p.is_some_and(|p| !installed.contains(&p));
                if miss && pack.is_none() {
                    pack = p.map(String::from);
                }
                miss
            })
            .collect();
        if missing.is_empty() {
            return None;
        }
        let names: Vec<&str> = missing.iter().map(|c| super::settings_tab::language_name(c)).collect();
        let pack = match pack.as_deref() {
            Some("european") => "Parakeet v3".to_string(),
            Some("asian") => "SenseVoice".to_string(),
            Some(p) => p.to_string(),
            None => String::new(),
        };
        Some(crate::ui_text::t(&self.locale, "home.packMissing", &[("languages", &names.join(", ")), ("pack", &pack)]))
    }

    /// Форма редактора под строкой (.grammar-editor: отступ 34, черта слева, 10 до полей).
    fn draw_editor(&mut self, cx: &mut RenderCx<'_>, body: Rect, y: f32) -> f32 {
        let x = body.x + 34.0;
        let top = y + 4.0;
        let h = self.form.draw(&mut self.scene, cx, x + 11.0, top, body.w - 34.0 - 11.0);
        cx.fill_rect(Rect::new(x, top, 1.0, h), ACC_4);
        self.form_blink = self.form.next_blink(cx.now);
        top + h + 10.0
    }

    fn open_editor(&mut self, key: &str) {
        if self.editor.as_ref().is_some_and(|e| e.open == key) {
            self.close_editor();
            return;
        }
        let say = &self.info.say;
        self.editor = Some(super::grammar_edit::GrammarEditor::new(
            key,
            &self.info.grammar,
            &self.info.hidden_triggers,
            &self.locale,
            &say.wake,
            say.ru,
        ));
        self.form.set_items(Vec::new());
        self.rebuild_form();
        self.reveal = true;
    }

    fn close_editor(&mut self) {
        self.editor = None;
        self.form.set_items(Vec::new());
        self.form_blink = None;
        native::set_ime_caret(self.hwnd, None);
        self.static_dirty = true;
    }

    fn rebuild_form(&mut self) {
        if let Some(ed) = &self.editor {
            self.form.set_items(ed.items());
        }
        self.touch_static();
    }

    fn editor_event(&mut self, r: native::controls::form::FormResult) -> bool {
        use super::grammar_edit::EditorOut;
        let redraw = r.redraw;
        if let (Some(ev), Some(ed)) = (r.event, self.editor.as_mut()) {
            match ed.on_event(ev) {
                EditorOut::None => {}
                EditorOut::Rebuild => self.rebuild_form(),
                EditorOut::Save(g) => (self.host)(Act::SaveGrammar(g)),
                EditorOut::Close => self.close_editor(),
                EditorOut::Reset => (self.host)(Act::ResetGrammar),
            }
            self.touch_static();
            return true;
        }
        if redraw {
            self.touch_static();
        }
        redraw
    }

    fn shade_mouse(&mut self, kind: MouseKind, x: f32, y: f32) -> bool {
        use super::shade::ShadeHit;
        let Some(s) = self.shade.as_mut() else { return false };
        let hit = s.hit(x, y);
        match kind {
            MouseKind::Move => {
                let mut r = false;
                r |= s.lunki[0].set_hover(hit == Some(ShadeHit::Expand));
                r |= s.lunki[1].set_hover(hit == Some(ShadeHit::Tray));
                let orb = hit == Some(ShadeHit::Orb);
                if orb != s.orb_hover {
                    s.orb_hover = orb;
                    self.orb_hover = orb;
                    r = true;
                }
                let tip = match hit {
                    Some(ShadeHit::Expand) => Some((11, s.lunki[0].rect(), s.lunki[0].tip.as_str())),
                    Some(ShadeHit::Tray) => Some((12, s.lunki[1].rect(), s.lunki[1].tip.as_str())),
                    _ => None,
                };
                r |= self.tooltip.hover(tip);
                set_cursor(self.hwnd, if hit.is_some() { Cursor::Hand } else { Cursor::Arrow });
                r
            }
            MouseKind::Leave => {
                let r = s.lunki[0].set_hover(false) | s.lunki[1].set_hover(false);
                s.orb_hover = false;
                self.orb_hover = false;
                r | self.tooltip.hide()
            }
            MouseKind::Down(MouseButton::Left) => {
                self.tooltip.hide();
                match hit {
                    Some(ShadeHit::Expand) => s.lunki[0].press.pressed = true,
                    Some(ShadeHit::Tray) => s.lunki[1].press.pressed = true,
                    Some(ShadeHit::Orb) => {}
                    // за плиту полоску таскают (#shadeDrag)
                    None => native::begin_drag(self.hwnd),
                }
                self.shade_pressed = hit;
                true
            }
            MouseKind::Up(MouseButton::Left) => {
                s.lunki[0].press.pressed = false;
                s.lunki[1].press.pressed = false;
                let p = self.shade_pressed.take();
                if p.is_some() && p == hit {
                    match p {
                        Some(ShadeHit::Expand) => (self.host)(Act::Shade(false)),
                        Some(ShadeHit::Tray) => (self.host)(Act::Tray),
                        Some(ShadeHit::Orb) => {
                            self.flash = 0.6;
                            self.info.paused = !self.info.paused;
                            (self.host)(Act::TogglePause);
                        }
                        None => {}
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn open_overlay(&mut self, kind: super::overlays::Kind) {
        self.overlay = Some(super::overlays::Overlay::new(kind, &self.locale, self.hwnd));
        self.touch_static();
    }

    fn overlay_out(&mut self, out: super::overlays::OverlayOut) {
        use super::overlays::{Kind, OverlayOut};
        match out {
            OverlayOut::Close => {
                match self.overlay.as_ref().map(|o| &o.kind) {
                    Some(Kind::Calibration { .. }) => (self.host)(Act::CancelCalibration),
                    Some(Kind::Voice { recording: true, .. }) => {
                        (self.host)(Act::VoiceCancel);
                        self.set_state("idle");
                    }
                    _ => {}
                }
                self.overlay = None;
                native::set_ime_caret(self.hwnd, None);
            }
            OverlayOut::VoiceDone => {
                let mut intro = false;
                if let Some(o) = self.overlay.as_mut() {
                    if let Kind::Voice { recording, result, intro: i, .. } = &mut o.kind {
                        *recording = false;
                        *result = crate::ui_text::t(&self.locale, "voiceAdapt.working", &[]);
                        intro = *i;
                    }
                }
                self.set_state("idle");
                (self.host)(Act::VoiceFinish(intro));
            }
            OverlayOut::AddScanned(apps) => {
                (self.host)(Act::AddScanned(apps));
                self.overlay = None;
            }
            OverlayOut::App(name, aumid) => {
                self.actions_tab.apply_app(&name, &aumid);
                self.overlay = None;
            }
        }
        self.touch_static();
    }

    fn settings_out(&mut self, out: super::settings_tab::SettingsOut) {
        if out == super::settings_tab::SettingsOut::VoiceStart {
            let wake = if self.info.say.ru { "ривер" } else { "reever" };
            self.open_overlay(super::overlays::Kind::Voice { words: 0, result: String::new(), recording: true, wake: wake.into(), intro: false });
            // пока окно открыто, линза слушает (Skin.setState('listening'))
            self.set_state("listening");
            (self.host)(Act::VoiceStart);
            return;
        }
        if out == super::settings_tab::SettingsOut::OpenGrammar {
            self.tab = 0;
            (self.host)(Act::Tab("home".into()));
            if !self.editor.as_ref().is_some_and(|e| e.open == "general") {
                self.open_editor("general");
            }
            self.static_dirty = true;
            return;
        }
        (self.host)(Act::Settings(out));
    }

    fn actions_out(&mut self, out: super::actions_tab::ActionsOut) {
        use super::actions_tab::ActionsOut;
        match out {
            ActionsOut::Save(a) => (self.host)(Act::SaveAction(a)),
            ActionsOut::Delete(id) => (self.host)(Act::DeleteAction(id)),
            ActionsOut::Calibrate(a) => {
                let phrases = self.actions_tab.shown_phrases(&a.id).map(|(_, p)| p).unwrap_or_else(|| a.phrases.clone());
                self.open_overlay(super::overlays::Kind::Calibration {
                    id: a.id.clone(),
                    label: a.label.clone(),
                    count: phrases.len(),
                    last: phrases.last().cloned(),
                });
                (self.host)(Act::Calibrate(a));
            }
            ActionsOut::Scan => {
                self.open_overlay(super::overlays::Kind::Scan { apps: None });
                (self.host)(Act::Scan);
            }
            ActionsOut::PickApp => {
                self.open_overlay(super::overlays::Kind::Apps { list: None });
                (self.host)(Act::ListApps);
            }
        }
    }

    /// Точка над видимой частью «Главной» (ниже бегущей строки).
    fn over_home(&self, x: f32, y: f32) -> bool {
        TABS[self.tab] == "home" && native::controls::inside(SCREEN, x, y) && y >= SCREEN.y + 34.0
    }

    /// Строка «Что можно сказать» под курсором (только видимая часть «Главной»).
    fn say_row_at(&self, x: f32, y: f32) -> Option<usize> {
        if TABS[self.tab] != "home" || !native::controls::inside(SCREEN, x, y) || y < SCREEN.y + 34.0 {
            return None;
        }
        self.say_rects.iter().position(|r| native::controls::inside(*r, x, y))
    }

    fn web_button(&mut self, cx: &mut RenderCx<'_>, x: f32, y: f32) {
        let label = self.web.label.to_uppercase();
        let w = self.text(cx, &label, &lcd(15.0, 500, 0.07), 1000.0).map_or(160.0, |l| l.width + 24.0);
        self.web.rect = Rect::new(x, y, w, 28.0);
        self.scene.flush(cx);
        self.web.draw(&mut self.scene, cx);
    }

    fn draw_not_ported(&mut self, cx: &mut RenderCx<'_>, body: Rect) {
        self.web.label = self.t("main.openWeb");
        let title = self.t(TAB_KEYS[self.tab]).to_uppercase();
        let mut y = body.y + 2.0;
        if let Some(l) = self.text(cx, &title, &lcd(22.0, 600, 0.08), body.w) {
            self.scene.text(&l, body.x, y, ACC_HOT, Glow::G3);
            y += l.height + 12.0;
        }
        let note = self.t("main.notPorted");
        let st = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
        if let Some(l) = self.text(cx, &note, &st, body.w) {
            self.scene.text(&l, body.x, y, ACC_3, Glow::G1);
            y += l.height + 14.0;
        }
        self.web_button(cx, body.x, y);
    }

    fn draw_logs(&mut self, cx: &mut RenderCx<'_>, body: Rect) {
        let title = self.t("nav.log").to_uppercase();
        let mut top = body.y + 2.0;
        if let Some(l) = self.text(cx, &title, &lcd(22.0, 600, 0.08), body.w) {
            self.scene.text(&l, body.x, top, ACC_HOT, Glow::G3);
            top += l.height + 12.0;
        }
        // строки снизу вверх: последняя — у нижнего края, прокрутка колесом
        let view = Rect::new(body.x, top, body.w, body.y + body.h - top);
        let st = TextStyle { wrap: true, break_words: true, letter_spacing: -0.22, ..data(11.0, 1.6) };
        self.scene.set_clip(Some(view));
        let mut y = view.y + view.h + self.log_scroll;
        let n = self.log.len();
        for i in (0..n).rev() {
            if y < view.y {
                break;
            }
            let line = self.log[i].clone();
            let Some(l) = self.text(cx, &line, &st, view.w) else { continue };
            let lh = l.height + 3.0;
            y -= lh;
            if y + lh > view.y + view.h + 1.0 {
                continue;
            }
            let last = i + 1 == n;
            let (c, g) = if last { (ACC_HOT, Glow::G2) } else { (ACC_2, Glow::G1) };
            self.scene.text(&l, view.x, y + 1.0, c, g);
            cx.fill_rect(Rect::new(view.x, y + lh - 1.0, view.w, 1.0), Color::rgba(39, 230, 170, 10));
        }
        self.scene.flush(cx);
        self.scene.set_clip(None);
    }

    /// Эквалайзер: лента громкости меняется ~22 раза в секунду (шаг 45 мс), поэтому он в
    /// статичном слое, который обновляется на каждом шаге ленты, а не каждый кадр.
    fn draw_eq(&mut self, cx: &mut RenderCx<'_>) {
        let r = EQ;
        lcd_background(cx, r, 10.0);
        let cols = self.eq.len();
        let rows = 7usize;
        let top = 7.0;
        let ph = (r.h - 14.0) / (rows - 1) as f32;
        for (i, v) in self.eq.iter().enumerate() {
            let lit = (v * rows as f32).round() as usize;
            let x = r.x + 8.0 + i as f32 * 9.0 + 4.5;
            let recent = i as f32 / cols as f32;
            for row in 0..rows {
                let y = r.y + r.h - top - row as f32 * ph;
                let c = if row < lit {
                    if row >= rows - 2 {
                        Color::rgba(200, 255, 236, ((0.55 + 0.45 * recent) * 255.0) as u8)
                    } else {
                        Color::rgba(39, 230, 170, ((0.35 + 0.65 * recent) * 255.0) as u8)
                    }
                } else {
                    Color::rgba(39, 230, 170, 19)
                };
                cx.fill_rect(Rect::new(x - 3.0, y - 1.6, 6.0, 3.2), c);
            }
        }
        lcd_overlay(cx, r, 10.0);
    }

    fn draw_core(&mut self, cx: &mut RenderCx<'_>) {
        if let Some(lens) = &self.lens {
            unsafe {
                cx.dc().DrawBitmap(lens.bitmap(), Some(&CORE.d2d()), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        }
    }

    fn lens_params(&self) -> LensParams {
        let s = &self.springs;
        LensParams {
            time: self.t0.elapsed().as_secs_f32(),
            fill: s.fill.v.max(0.0),
            wave: s.wave.v,
            energy: s.energy.v.max(0.0),
            swirl: s.swirl.v,
            flash: self.flash,
            dark: s.dark.v.max(0.0),
            glitch: s.glitch.v.max(0.0),
        }
    }

    /// Осьминог в сфере: вид маскота рисуется в свой битмап размером с линзу (как в своё окно),
    /// битмап кладётся в круг линзы — как холст #orbMascot с clip по кругу.
    fn draw_orb_mascot(&mut self, cx: &mut RenderCx<'_>, dpi: (f32, f32)) {
        let Some(orb) = self.orb.as_ref() else { return };
        if !orb.visible() {
            return;
        }
        let size = ((CORE.w * cx.scale).round().max(1.0) as u32, (CORE.h * cx.scale).round().max(1.0) as u32);
        let fits = self.orb_layer.as_ref().is_some_and(|(_, sz, gen)| *sz == size && *gen == self.gpu_generation);
        if !fits {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: dpi.0,
                dpiY: dpi.1,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            match unsafe { cx.dc().CreateBitmap(D2D_SIZE_U { width: size.0, height: size.1 }, None, 0, &props) } {
                Ok(b) => {
                    self.orb_layer = Some((b, size, self.gpu_generation));
                    self.orb_dirty = true;
                }
                Err(e) => {
                    native::log(&format!("main: orb mascot layer: {e}"));
                    self.orb = None;
                    return;
                }
            }
        }
        let Some((bmp, _, _)) = self.orb_layer.clone() else { return };
        if self.orb_dirty {
            self.orb_dirty = false;
            let dc = cx.dc().clone();
            unsafe {
                let old: Option<ID2D1Image> = dc.GetTarget().ok();
                dc.SetTarget(&bmp);
                dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                {
                    let mut sub = RenderCx {
                        dc: cx.dc,
                        images: &mut *cx.images,
                        text: cx.text,
                        brush: cx.brush,
                        scale: cx.scale,
                        size: (CORE.w, CORE.h),
                        now: cx.now,
                    };
                    if let Some(orb) = self.orb.as_mut() {
                        orb.render(&mut sub);
                    }
                }
                // вид маскота ставит свою матрицу и DPI — вернуть кадру главного окна
                dc.SetTransform(&windows::Foundation::Numerics::Matrix3x2::identity());
                dc.SetDpi(dpi.0, dpi.1);
                dc.SetPrimitiveBlend(windows::Win32::Graphics::Direct2D::D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
                if let Some(old) = old {
                    dc.SetTarget(&old);
                }
            }
        }
        // круг линзы: r = 100 вокруг центра сферы
        let clip = Rect::new(ORB.0 - ORB.2, ORB.1 - ORB.2, ORB.2 * 2.0, ORB.2 * 2.0);
        if native::controls::push_round_layer(cx, clip, ORB.2, 1.0) {
            unsafe {
                cx.dc().DrawBitmap(&bmp, Some(&CORE.d2d()), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
            native::controls::pop_layer(cx);
        }
    }

    /// Статичная часть окна: экраны с текстом и рамка (в статическом режиме света).
    fn draw_static(&mut self, cx: &mut RenderCx<'_>) {
        let now = cx.now;
        for (i, r) in [TITLE, STATUS, RAIL, SCREEN, EQ].into_iter().enumerate() {
            let p = self.power(i, now);
            if p <= 0.0 {
                continue; // экран ещё не включился
            }
            let anim = p < 1.0;
            let (clip, bright) = if anim { crt_on(r, p) } else { (r, 0.0) };
            if anim {
                self.scene.flush(cx);
                self.scene.set_clip(Some(clip));
                unsafe { cx.dc().PushAxisAlignedClip(&clip.d2d(), windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED) };
            }
            match i {
                0 => self.draw_title(cx),
                1 => self.draw_status(cx),
                2 => self.draw_rail(cx),
                3 => self.draw_screen(cx),
                _ => self.draw_eq(cx),
            }
            if anim {
                self.scene.flush(cx);
                // filter: brightness(5 → 1) — светлой добавкой поверх
                if bright > 0.0 {
                    unsafe { cx.dc().SetPrimitiveBlend(windows::Win32::Graphics::Direct2D::D2D1_PRIMITIVE_BLEND_ADD) };
                    cx.fill_rect(clip, Color { a: (bright * 0.18).min(0.9), ..ACC_HOT });
                    unsafe { cx.dc().SetPrimitiveBlend(windows::Win32::Graphics::Direct2D::D2D1_PRIMITIVE_BLEND_SOURCE_OVER) };
                }
                self.scene.set_clip(None);
                unsafe { cx.dc().PopAxisAlignedClip() };
            }
        }
        self.draw_frame(cx);
        // лунки в покое не меняются; наведение и нажатие обновляют слой (touch_static)
        for l in self.lunki.iter() {
            l.draw(&mut self.scene, cx);
        }
        self.scene.flush(cx);
        if let Some(o) = self.overlay.as_mut() {
            o.draw(&mut self.scene, cx, (SKIN_W, SKIN_H), cx.now);
            self.scene.flush(cx);
            self.form_blink = o.next_blink(cx.now);
        }
    }

    /// Бегущая строка, когда текст не влезает (#partial.scroll): едет в живом слое, чтобы не
    /// перерисовывать весь статичный слой 30 раз в секунду.
    fn draw_ticker_live(&mut self, cx: &mut RenderCx<'_>) {
        if self.ticker_over <= 4.0 || self.power(3, cx.now) < 1.0 || self.overlay.is_some() {
            return;
        }
        let r = SCREEN;
        let tape = Rect::new(r.x + 14.0 + 13.0 + 8.0, r.y, r.w - 14.0 - 13.0 - 8.0 - 14.0, 34.0);
        let partial = self.partial.to_uppercase();
        let wide = TextStyle { direction: self.dir(), ..lcd(21.0, 500, 0.05) };
        let Some(l) = self.text(cx, &partial, &wide, 100_000.0) else { return };
        // marquee: 8% на месте, потом уезжает на переполнение + 30, по кругу
        let dist = self.ticker_over + 30.0;
        let dur = (dist / 55.0).max(4.0);
        let k = (cx.now.duration_since(self.ticker_from).as_secs_f32() % dur) / dur;
        let shift = if k < 0.08 { 0.0 } else { dist * (k - 0.08) / 0.92 };
        self.scene.set_clip(Some(tape));
        unsafe { cx.dc().PushAxisAlignedClip(&tape.d2d(), windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED) };
        self.scene.text(&l, tape.x + 3.0 - shift, r.y + (34.0 - l.height) / 2.0, ACC_HOT, Glow::G3);
        self.scene.flush(cx);
        self.scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
    }

    /// Включение экрана `i` (powerOn в shell.js): 0 — ещё тёмный, 1 — включён.
    fn power(&self, i: usize, now: Instant) -> f32 {
        let delay = if self.power_quick { i as f32 * 0.05 } else { 0.15 + i as f32 * 0.09 };
        let t = now.saturating_duration_since(self.power_from).as_secs_f32() - delay;
        if t <= 0.0 {
            return 0.0;
        }
        (t / 0.62).min(1.0)
    }

    /// Идёт ли какая-то анимация экранов (статичный слой тогда обновляется каждый кадр).
    fn screens_animating(&self, now: Instant) -> bool {
        (0..5).any(|i| self.power(i, now) < 1.0)
            || self.channel.is_some_and(|(t, _)| now.duration_since(t) < Duration::from_millis(140 + 320))
            || now < self.invert_until + Duration::from_millis(20)
    }

    /// Включить экраны заново (развернули полоску — быстро).
    pub fn power_on(&mut self, quick: bool) {
        self.power_from = Instant::now();
        self.power_quick = quick;
        self.static_dirty = true;
    }

    /// Обновить статичный слой, если нужно. false — слоя нет (рисовать напрямую).
    fn render_static(&mut self, cx: &mut RenderCx<'_>, dpi: (f32, f32)) -> bool {
        let size = ((SKIN_W * cx.scale).round().max(1.0) as u32, (SKIN_H * cx.scale).round().max(1.0) as u32);
        let fits = self.static_layer.as_ref().is_some_and(|(_, sz, gen)| *sz == size && *gen == self.gpu_generation);
        if !fits {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: dpi.0,
                dpiY: dpi.1,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let made = unsafe { cx.dc().CreateBitmap(D2D_SIZE_U { width: size.0, height: size.1 }, None, 0, &props) };
            match made {
                Ok(b) => {
                    self.static_layer = Some((b, size, self.gpu_generation));
                    self.static_dirty = true;
                }
                Err(e) => {
                    native::log(&format!("main: static layer: {e}"));
                    self.static_layer = None;
                    return false;
                }
            }
        }
        // рамка с живым светом меняется каждый кадр — слой тоже; пока шейдера рамки нет — тоже
        let live = !FRAME_STATIC || Instant::now() < self.static_live_until || self.frame.is_none();
        if self.static_dirty || live {
            if std::env::var_os("REEVER_MAIN_PROFILE").is_some() {
                PROFILE.with(|p| {
                    let mut p = p.borrow_mut();
                    *p.entry(if live { "redraw_live" } else { "redraw_dirty" }).or_insert(0.0) += 1.0;
                });
            }
            let Some((bmp, _, _)) = self.static_layer.clone() else { return false };
            let dc = cx.dc().clone();
            unsafe {
                let old: Option<ID2D1Image> = dc.GetTarget().ok();
                dc.SetTarget(&bmp);
                dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                self.draw_static(cx);
                if let Some(old) = old {
                    dc.SetTarget(&old);
                }
            }
            self.static_dirty = false;
            let k = self.k;
            let caret = if let Some(o) = &self.overlay { o.ime_caret() } else { match TABS[self.tab] {
                "actions" => self.actions_tab.ime_caret(),
                "settings" => self.settings_tab.form.ime_caret(),
                _ => self.form.ime_caret(),
            } };
            native::set_ime_caret(self.hwnd, caret.map(|(x, y, h)| (x * k, y * k, h * k)));
        }
        true
    }

    /// Наведение или нажатие на текстовую кнопку/вкладку: их переходы — в статичном слое.
    fn touch_static(&mut self) {
        self.static_dirty = true;
        self.static_live_until = Instant::now() + Duration::from_millis(260);
    }

    fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        for (i, l) in self.lunki.iter().enumerate() {
            if l.hit(x, y) {
                return Some(Target::Lunka(i));
            }
        }
        for i in 0..4 {
            if native::controls::inside(Self::tab_rect(i), x, y) {
                return Some(Target::Tab(i));
            }
        }
        if self.pause.hit(x, y) {
            return Some(Target::Pause);
        }
        if self.tab != 3 && self.web.rect.w > 0.0 && self.web.hit(x, y) {
            return Some(Target::Web);
        }
        let (dx, dy) = (x - ORB.0, y - ORB.1);
        if dx * dx + dy * dy <= ORB.2 * ORB.2 {
            return Some(Target::Orb);
        }
        if let Some(i) = self.say_row_at(x, y) {
            if self.info.say.rows.get(i).is_some_and(|r| r.key.is_some()) {
                return Some(Target::Row(i));
            }
        }
        None
    }

    fn over_screen(x: f32, y: f32) -> bool {
        [TITLE, STATUS, RAIL, SCREEN, EQ].iter().any(|r| native::controls::inside(*r, x, y))
    }

    fn activate(&mut self, t: Target) {
        match t {
            Target::Lunka(0) => (self.host)(Act::Minimize),
            Target::Lunka(2) => (self.host)(Act::Tray),
            Target::Lunka(_) => (self.host)(Act::Shade(true)),
            Target::Tab(i) => {
                if self.tab != i {
                    self.channel = Some((Instant::now(), self.tab));
                    if self.settings_tab.placing {
                        self.settings_tab.set_placing(false);
                        (self.host)(Act::Settings(super::settings_tab::SettingsOut::MascotPin(false)));
                    }
                    self.tab = i;
                    self.log_scroll = 0.0;
                    self.home_scroll = 0.0;
                    self.say_hover = None;
                    if self.editor.is_some() {
                        self.close_editor();
                    }
                    self.static_dirty = true;
                    (self.host)(Act::Tab(TABS[i].to_string()));
                }
            }
            Target::Pause | Target::Orb => {
                self.flash = 0.6;
                self.info.paused = !self.info.paused;
                self.static_dirty = true;
                (self.host)(Act::TogglePause);
            }
            Target::Web if TABS[self.tab] == "home" => {
                let general = self.editor.as_ref().is_some_and(|e| e.open == "general");
                if general {
                    self.close_editor();
                } else {
                    self.open_editor("general");
                }
            }
            Target::Web => (self.host)(Act::OpenWeb),
            Target::Row(i) => {
                if let Some(key) = self.info.say.rows.get(i).and_then(|r| r.key.clone()) {
                    self.open_editor(&key);
                }
            }
        }
    }
}

impl WindowHandler for MainView {
    fn attached(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
        self.form = Form::new(hwnd, self.rtl);
        self.actions_tab.set_hwnd(hwnd);
        self.settings_tab.set_hwnd(hwnd);
    }

    fn prepare(&mut self, gpu: &GpuCx<'_>) {
        if self.overlay.as_ref().is_some_and(|o| o.animating()) || self.screens_animating(gpu.now) {
            self.static_dirty = true;
        }
        if TABS[self.tab] == "home" && !self.shaded {
            let phase = (gpu.now.duration_since(self.t0).as_secs_f32() / 0.53) as i64;
            if phase != self.caret_phase {
                self.caret_phase = phase;
                self.static_dirty = true;
            }
        }
        if let Some(out) = self.settings_tab.take_due(gpu.now) {
            (self.host)(Act::Settings(out));
        }
        if self.form_blink.is_some_and(|b| gpu.now >= b) {
            self.form_blink = None;
            self.static_dirty = true;
        }
        // шаг осьминога — по часам, а не в advance: кадр по wake_at (покой окна) рисуется без
        // advance, и срок пробуждения застревал бы в прошлом — окно крутилось бы на частоте монитора
        if let Some(orb) = self.orb.as_mut() {
            let elapsed = gpu.now.saturating_duration_since(self.orb_last);
            if orb.visible() && elapsed >= Duration::from_secs_f32(1.0 / 30.0) {
                orb.advance(elapsed.min(Duration::from_millis(100)));
                self.orb_dirty = true;
                self.orb_last = gpu.now;
            }
        }
        if gpu.generation != self.gpu_generation {
            self.gpu_generation = gpu.generation;
            self.static_layer = None;
            if let Some(s) = self.shade.as_mut() {
                s.reset_gpu();
            }
        }
        if self.shaded {
            let p = self.lens_params();
            let orb_light = (self.springs.orb_light.v + self.flash * 1.5).max(0.0);
            if let Some(s) = self.shade.as_mut() {
                s.prepare(gpu, self.k, &p, orb_light);
            }
            return;
        }
        // рамка: шейдер по картинкам, подготовленным в прошлом кадре (render)
        if !self.frame_failed {
            if let Some(px) = self.frame_px.take() {
                match FrameShader::new(gpu, &px) {
                    Ok(Some(f)) => self.frame = Some(f),
                    Ok(None) => self.frame_failed = true,
                    Err(e) => {
                        native::log(&format!("main: frame shader: {e}"));
                        self.frame_failed = true;
                    }
                }
            }
            let light = self.light();
            if let Some(f) = &mut self.frame {
                if f.fits(gpu, frame_size(self.k, gpu.scale)) {
                    f.render(gpu, (SKIN_W, SKIN_H), &light);
                }
            }
        }
        if self.orb_dirty {
            if let Some(orb) = self.orb.as_mut() {
                if orb.visible() {
                    orb.prepare(gpu);
                }
            }
        }
        // линза: та же, что глаз маскота
        if !self.lens_failed {
            let size = (CORE.w * self.k * gpu.scale).round().max(2.0) as u32;
            if self.lens.as_ref().map_or(true, |l| !l.fits(gpu, size)) {
                match Lens::new(gpu, size) {
                    Ok(Some(l)) => self.lens = Some(l),
                    Ok(None) => self.lens_failed = true,
                    Err(e) => {
                        native::log(&format!("main: lens: {e}"));
                        self.lens_failed = true;
                    }
                }
            }
            let p = self.lens_params();
            if let Some(l) = &self.lens {
                l.render(gpu, &p);
            }
        }
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        // всё — в координатах скина: DPI контекста и масштаб умножены на k
        let (mut dpi_x, mut dpi_y) = (96.0f32, 96.0f32);
        unsafe { cx.dc().GetDpi(&mut dpi_x, &mut dpi_y) };
        unsafe { cx.dc().SetDpi(dpi_x * self.k, dpi_y * self.k) };
        let base_scale = cx.scale;
        cx.scale = base_scale * self.k;
        self.scene.begin_frame(cx);
        if self.shaded && self.shade.is_some() {
            // syncShadeText: что слышно сейчас, иначе последняя команда, иначе состояние
            let text = if !self.partial.trim().is_empty() {
                self.partial.clone()
            } else if let Some(h) = &self.heard {
                h.clone()
            } else {
                let key = match self.state.as_str() {
                    s @ ("listening" | "busy" | "loading" | "paused" | "error") => format!("mic.{s}"),
                    _ => "mic.idle".into(),
                };
                self.t(&key)
            };
            let (k, scene) = (self.k, &mut self.scene);
            if let Some(s) = self.shade.as_mut() {
                s.set_text(&text);
                s.render(scene, cx, k, base_scale);
            }
            self.tooltip.draw(&mut self.scene, cx, (super::shade::SHADE_W, super::shade::SHADE_H));
            self.scene.flush(cx);
            cx.scale = base_scale;
            unsafe { cx.dc().SetDpi(dpi_x, dpi_y) };
            return;
        }

        // картинки для шейдера рамки — один раз на размер (дальше prepare)
        let size = frame_size(self.k, base_scale);
        let need = !self.frame_failed && self.frame_px.is_none() && self.frame.as_ref().map_or(true, |_| false);
        if need {
            let images = cx.images();
            if let (Ok(color), Ok(normal)) = (
                images.pixels(self.assets.frame_color, size.0, size.1),
                images.pixels(self.assets.frame_normal, size.0, size.1),
            ) {
                self.frame_px = Some(FramePixels { w: size.0, h: size.1, color, normal });
            } else {
                self.frame_failed = true;
            }
        }

        let prof = std::env::var_os("REEVER_MAIN_PROFILE").is_some();
        let mut marks: Vec<(&str, Instant)> = vec![("start", Instant::now())];
        let mut mark = |name: &'static str, cx: &RenderCx<'_>| {
            if prof {
                unsafe {
                    let _ = cx.dc().Flush(None, None);
                }
                marks.push((name, Instant::now()));
            }
        };
        self.draw_core(cx);
        mark("core", cx);
        self.draw_orb_mascot(cx, (dpi_x * self.k, dpi_y * self.k));
        mark("orb", cx);
        if self.render_static(cx, (dpi_x * self.k, dpi_y * self.k)) {
            if let Some((bmp, _, _)) = &self.static_layer {
                unsafe {
                    cx.dc().DrawBitmap(bmp, Some(&Rect::new(0.0, 0.0, SKIN_W, SKIN_H).d2d()), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                }
            }
        } else {
            // без слоя (не создался) — всё прямо в кадр
            self.draw_static(cx);
        }
        mark("static", cx);
        self.draw_ticker_live(cx);
        self.fx.draw(cx);
        mark("fx", cx);
        if prof {
            let mut acc = PROFILE.with(|p| p.take());
            for w in marks.windows(2) {
                let e = acc.entry(w[1].0).or_insert(0.0);
                *e += w[1].1.duration_since(w[0].1).as_secs_f64() * 1000.0;
            }
            *acc.entry("frames").or_insert(0.0) += 1.0;
            if acc["frames"] >= 300.0 {
                let n = acc["frames"];
                let line: Vec<String> = acc.iter().filter(|(k, _)| **k != "frames").map(|(k, v)| format!("{k} {:.3} ms", v / n)).collect();
                eprintln!("[main-profile] per frame: {}", line.join(", "));
                acc.clear();
            }
            PROFILE.with(|p| p.replace(acc));
        }
        self.tooltip.draw(&mut self.scene, cx, (SKIN_W, SKIN_H));
        self.scene.flush(cx);

        cx.scale = base_scale;
        unsafe { cx.dc().SetDpi(dpi_x, dpi_y) };
    }

    fn animating(&self) -> bool {
        let springs_moving = {
            let s = &self.springs;
            [s.fill, s.energy, s.wave, s.swirl, s.dark, s.glitch, s.orb_light, s.mouse_on, s.mx, s.my, s.lunki[0], s.lunki[1], s.lunki[2]]
                .iter()
                .any(|s| (s.t - s.v).abs() > 0.012 || s.vel.abs() > 0.025)
        };
        self.frame_px.is_some()
            || self.fx.active()
            || self.burst > 0.01
            || springs_moving
            || self.flash > 0.01
            || (self.level_raw - self.level).abs() > 0.005
            || self.eq.iter().any(|v| *v > 0.001)
            || self.active()
            || self.tooltip.animating(Instant::now())
            || self.overlay.as_ref().is_some_and(|o| o.animating())
            || self.screens_animating(Instant::now())
            || Instant::now() < self.static_live_until
    }

    fn wake_at(&self) -> Option<Instant> {
        let mut at = self.tooltip.wake_at();
        // мигание курсора на «Главной» (2 раза в секунду), бегущая строка (до 30 кадров)
        if !self.shaded && TABS[self.tab] == "home" {
            let next = self.t0 + Duration::from_secs_f32(0.53 * (self.caret_phase + 1) as f32);
            at = Some(at.map_or(next, |a| a.min(next)));
        }
        if !self.shaded && self.ticker_over > 4.0 {
            let next = Instant::now() + Duration::from_millis(33);
            at = Some(at.map_or(next, |a| a.min(next)));
        }
        if self.shaded {
            if let Some(w) = self.shade.as_ref().and_then(|s| s.wake_at()) {
                at = Some(at.map_or(w, |a| a.min(w)));
            }
        }
        if let Some(b) = self.form_blink {
            at = Some(at.map_or(b, |a| a.min(b)));
        }
        if let Some(b) = self.settings_tab.wake_at() {
            at = Some(at.map_or(b, |a| a.min(b)));
        }
        if self.orb.as_ref().is_some_and(|o| o.visible()) {
            let next = self.orb_last + Duration::from_millis(34);
            at = Some(at.map_or(next, |a| a.min(next)));
        }
        if let Some(w) = self.woke_until {
            at = Some(at.map_or(w, |a| a.min(w)));
        }
        at
    }

    fn advance(&mut self, dt: Duration) {
        let dt = dt.as_secs_f32().min(0.05);
        let now = Instant::now();
        if self.woke_until.is_some_and(|w| now >= w) {
            self.woke_until = None;
        }
        let time = now.duration_since(self.t0).as_secs_f32();
        // уровень: быстрая атака, медленный спад (доли — за кадр 60 Гц)
        let k = if self.level_raw > self.level { 0.45f32 } else { 0.88 };
        self.level += (self.level_raw - self.level) * (1.0 - k.powf(dt * 60.0));
        let t = targets(&self.state);
        let lv = match self.state.as_str() {
            "listening" => self.level,
            "busy" => self.level * 0.4,
            _ => 0.0,
        };
        let s = &mut self.springs;
        s.fill.t = t.fill + lv * 0.2 + if self.state == "loading" { (time * 2.2).sin() * 0.08 } else { 0.0 };
        s.wave.t = t.wave + lv * 2.4;
        s.orb_light.t = t.orb_light + lv * 1.3 + if self.orb_hover { 0.35 } else { 0.0 };
        s.energy.t = t.energy + lv * 0.5;
        for (i, l) in self.lunki.iter().enumerate() {
            s.lunki[i].t = if l.press.pressed { 2.2 } else if l.press.hover { 1.2 } else { 0.0 };
        }
        for sp in s.all() {
            sp.step(dt);
            sp.moving();
        }
        self.flash = (self.flash - dt * 2.6).max(0.0);
        self.burst = (self.burst - dt * 3.0).max(0.0);
        // рой у линзы: режим по состоянию; в покое угли — только сразу после открытия
        let mode = match self.state.as_str() {
            "listening" => Mode::Orbit,
            "busy" => Mode::Collapse,
            "paused" => Mode::None,
            "idle" if now >= self.embers_until => Mode::None,
            _ => Mode::Ember,
        };
        let fx = FxState {
            mode,
            level: lv,
            energy: self.springs.energy.v.max(0.0),
            threads: matches!(self.state.as_str(), "listening" | "busy"),
            burst: self.burst,
        };
        self.fx.step(&fx, dt);
        if self.shaded {
            if let Some(s) = self.shade.as_mut() {
                // в полоске без нитей (threads && !shaded)
                s.fx.step(&FxState { threads: false, ..fx }, dt);
            }
        }
        // эквалайзер: лента громкости, справа — сейчас
        self.eq_acc += dt;
        if self.eq_acc >= 0.045 {
            self.eq_acc = 0.0;
            self.eq.rotate_left(1);
            let v = if self.state == "paused" { 0.0 } else { (self.level * 1.15).min(1.0) };
            if let Some(last) = self.eq.last_mut() {
                *last = v;
            }
            if self.eq.iter().any(|v| *v > 0.001) || v > 0.0 {
                self.static_dirty = true;
            }
        }
        let phase = match self.state.as_str() {
            "listening" => ((time / 0.5) as i64) % 2 == 0,
            "busy" => ((time / 0.175) as i64) % 2 == 0,
            _ => true,
        };
        if phase != self.blink_phase {
            self.blink_phase = phase;
            self.static_dirty = true;
        }
        self.tooltip.tick(now);
    }

    fn mouse(&mut self, ev: MouseEvent) -> bool {
        let (x, y) = (ev.x / self.k, ev.y / self.k);
        if self.shaded && self.shade.is_some() {
            return self.shade_mouse(ev.kind, x, y);
        }
        if self.overlay.is_some() {
            let (redraw, out) = self.overlay.as_mut().map(|o| o.mouse(MouseEvent { kind: ev.kind, x, y })).unwrap_or_default();
            if let Some(out) = out {
                self.overlay_out(out);
            }
            if redraw {
                self.touch_static();
            }
            if ev.kind == MouseKind::Move {
                set_cursor(self.hwnd, Cursor::Arrow);
            }
            return redraw;
        }
        if TABS[self.tab] == "settings" {
            let over = native::controls::inside(SCREEN, x, y) && y >= SCREEN.y + 34.0;
            let skin = MouseEvent { kind: ev.kind, x, y };
            let busy = self.settings_tab.form.popup_open() || self.settings_tab.form.sliding();
            let mine = match ev.kind {
                MouseKind::Down(_) | MouseKind::DoubleClick => over || busy,
                MouseKind::Up(_) => (over && self.pressed.is_none()) || busy,
                _ => busy,
            };
            if matches!(ev.kind, MouseKind::Move | MouseKind::Leave | MouseKind::Up(_)) || mine {
                let (redraw, out) = self.settings_tab.mouse(skin);
                if let Some(out) = out {
                    self.settings_out(out);
                }
                if redraw {
                    self.touch_static();
                }
                if mine {
                    return true;
                }
                if ev.kind == MouseKind::Move && over {
                    let c = match self.settings_tab.cursor(x, y) {
                        Some(true) => Cursor::Hand,
                        Some(false) => Cursor::Text,
                        None => Cursor::Arrow,
                    };
                    set_cursor(self.hwnd, c);
                    return redraw;
                }
            }
        }
        if TABS[self.tab] == "actions" {
            let over = native::controls::inside(SCREEN, x, y) && y >= SCREEN.y + 34.0;
            let skin = MouseEvent { kind: ev.kind, x, y };
            let mine = match ev.kind {
                MouseKind::Down(_) | MouseKind::DoubleClick => over || self.actions_tab.form.popup_open(),
                MouseKind::Up(_) => over && self.pressed.is_none(),
                _ => false,
            };
            if matches!(ev.kind, MouseKind::Move | MouseKind::Leave | MouseKind::Up(_)) || mine {
                let (redraw, out) = self.actions_tab.mouse(skin);
                if let Some(out) = out {
                    self.actions_out(out);
                }
                if redraw {
                    self.touch_static();
                }
                if mine {
                    return true;
                }
                if ev.kind == MouseKind::Move && over {
                    let c = match self.actions_tab.cursor(x, y) {
                        Some(true) => Cursor::Hand,
                        Some(false) => Cursor::Text,
                        None => Cursor::Arrow,
                    };
                    set_cursor(self.hwnd, c);
                    return redraw;
                }
            }
        }
        match ev.kind {
            MouseKind::Move => {
                if !FRAME_STATIC {
                    self.springs.mx.t = x;
                    self.springs.my.t = y;
                    self.springs.mouse_on.t = 1.0;
                }
                let t = self.target_at(x, y);
                let mut redraw = false;
                let mut lunka_hover = false;
                for (i, l) in self.lunki.iter_mut().enumerate() {
                    lunka_hover |= l.set_hover(t == Some(Target::Lunka(i)));
                }
                if lunka_hover {
                    self.touch_static();
                    redraw = true;
                }
                let tab = match t {
                    Some(Target::Tab(i)) => Some(i),
                    _ => None,
                };
                let mut touched = false;
                if tab != self.tab_hover {
                    self.tab_hover = tab;
                    touched = true;
                }
                touched |= self.pause.press.set_hover(t == Some(Target::Pause), 0.12);
                let row = if t.is_none() || matches!(t, Some(Target::Row(_))) { self.say_row_at(x, y) } else { None };
                if row != self.say_hover {
                    self.say_hover = row;
                    touched = true;
                }
                touched |= self.web.press.set_hover(t == Some(Target::Web), 0.12);
                if touched {
                    self.touch_static();
                    redraw = true;
                }
                let orb = t == Some(Target::Orb);
                if orb != self.orb_hover {
                    self.orb_hover = orb;
                    redraw = true;
                }
                let tip = match t {
                    Some(Target::Lunka(i)) => Some((i as u64 + 1, self.lunki[i].rect(), self.lunki[i].tip.as_str())),
                    Some(Target::Orb) => None,
                    _ => None,
                };
                redraw |= self.tooltip.hover(tip);
                let mut cursor = if t.is_some() { Cursor::Hand } else { Cursor::Arrow };
                if self.editor.is_some() {
                    let over = self.over_home(x, y);
                    let r = self.form.mouse(MouseEvent { kind: MouseKind::Move, x, y });
                    redraw |= self.editor_event(r);
                    if over {
                        match self.form.hover_kind(x, y) {
                            Some(true) => cursor = Cursor::Hand,
                            Some(false) => cursor = Cursor::Text,
                            None => {}
                        }
                    }
                }
                set_cursor(self.hwnd, cursor);
                redraw
            }
            MouseKind::Leave => {
                self.springs.mouse_on.t = 0.0;
                let mut r = false;
                for l in self.lunki.iter_mut() {
                    r |= l.set_hover(false);
                }
                self.tab_hover = None;
                self.orb_hover = false;
                self.say_hover = None;
                self.touch_static();
                r | self.pause.press.set_hover(false, 0.12) | self.web.press.set_hover(false, 0.12) | self.tooltip.hide() | true
            }
            MouseKind::Down(MouseButton::Left) | MouseKind::DoubleClick if self.editor.is_some() && self.over_home(x, y) && self.form.hover_kind(x, y).is_some() => {
                self.tooltip.hide();
                let r = self.form.mouse(MouseEvent { kind: ev.kind, x, y });
                self.editor_event(r);
                true
            }
            MouseKind::Down(MouseButton::Left) => {
                self.tooltip.hide();
                self.touch_static();
                if self.editor.is_some() {
                    // щелчок мимо полей снимает их фокус
                    let r = self.form.mouse(MouseEvent { kind: MouseKind::Down(MouseButton::Left), x: -1.0, y: -1.0 });
                    self.editor_event(r);
                }
                let t = self.target_at(x, y);
                self.pressed = t;
                match t {
                    Some(Target::Lunka(i)) => self.lunki[i].press.pressed = true,
                    Some(Target::Pause) => self.pause.press.pressed = true,
                    Some(Target::Web) => self.web.press.pressed = true,
                    Some(_) => {}
                    // за пустое место (плита, хром, шипы) окно таскают; экраны — нет, как в вебе
                    None if !Self::over_screen(x, y) => {
                        self.pressed = None;
                        native::begin_drag(self.hwnd);
                    }
                    None => {}
                }
                true
            }
            MouseKind::Up(MouseButton::Left) => {
                self.touch_static();
                if self.editor.is_some() {
                    let r = self.form.mouse(MouseEvent { kind: MouseKind::Up(MouseButton::Left), x, y });
                    if r.event.is_some() {
                        self.pressed = None;
                        self.editor_event(r);
                        return true;
                    }
                }
                let t = self.target_at(x, y);
                let p = self.pressed.take();
                for l in self.lunki.iter_mut() {
                    l.press.pressed = false;
                }
                self.pause.press.pressed = false;
                self.web.press.pressed = false;
                if let Some(p) = p.filter(|p| Some(*p) == t) {
                    self.activate(p);
                }
                true
            }
            _ => false,
        }
    }

    fn key(&mut self, ev: KeyEvent) -> bool {
        if let Some(o) = self.overlay.as_mut() {
            let (redraw, out) = o.key(ev);
            if let Some(out) = out {
                self.overlay_out(out);
            }
            if redraw {
                self.touch_static();
            }
            return redraw;
        }
        if TABS[self.tab] == "settings" {
            let (redraw, out) = self.settings_tab.key(ev);
            if let Some(out) = out {
                self.settings_out(out);
            }
            if redraw {
                self.touch_static();
            }
            return redraw;
        }
        if TABS[self.tab] == "actions" {
            let (redraw, out) = self.actions_tab.key(ev);
            if let Some(out) = out {
                self.actions_out(out);
            }
            if redraw {
                self.touch_static();
            }
            return redraw;
        }
        if self.editor.is_none() {
            return false;
        }
        let r = self.form.key(ev);
        self.editor_event(r)
    }

    fn char_input(&mut self, c: char) -> bool {
        if let Some(o) = self.overlay.as_mut() {
            let r = o.char_input(c);
            if r {
                self.touch_static();
            }
            return r;
        }
        if TABS[self.tab] == "settings" {
            let (redraw, out) = self.settings_tab.char_input(c);
            if let Some(out) = out {
                self.settings_out(out);
            }
            if redraw {
                self.touch_static();
            }
            return redraw;
        }
        if TABS[self.tab] == "actions" {
            let (redraw, out) = self.actions_tab.char_input(c);
            if let Some(out) = out {
                self.actions_out(out);
            }
            if redraw {
                self.touch_static();
            }
            return redraw;
        }
        if self.editor.is_none() {
            return false;
        }
        let r = self.form.char_input(c);
        self.editor_event(r)
    }

    fn focus(&mut self, focused: bool) -> bool {
        self.form.window_focus(focused);
        self.actions_tab.window_focus(focused);
        self.settings_tab.form.window_focus(focused);
        if let Some(o) = self.overlay.as_mut() {
            o.window_focus(focused);
        }
        self.static_dirty = true;
        true
    }

    fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        let (x, y) = (x / self.k, y / self.k);
        if let Some(o) = self.overlay.as_mut() {
            let r = o.wheel(x, y, delta);
            if r {
                self.static_dirty = true;
            }
            return r;
        }
        if TABS[self.tab] == "settings" && self.settings_tab.wheel(x, y, delta) {
            self.static_dirty = true;
            return true;
        }
        if TABS[self.tab] == "actions" && self.actions_tab.wheel(x, y, delta) {
            self.static_dirty = true;
            return true;
        }
        if TABS[self.tab] == "logs" && native::controls::inside(SCREEN, x, y) {
            self.log_scroll = (self.log_scroll + delta * 48.0).max(0.0);
            self.static_dirty = true;
            return true;
        }
        if TABS[self.tab] == "home" && native::controls::inside(SCREEN, x, y) && self.home_max_scroll > 0.0 {
            let next = (self.home_scroll - delta * 48.0).clamp(0.0, self.home_max_scroll);
            if next != self.home_scroll {
                self.home_scroll = next;
                self.say_hover = self.say_row_at(x, y);
                self.static_dirty = true;
                return true;
            }
        }
        false
    }

    fn dpi_changed(&mut self, scale: f32) {
        // форма окна — в пикселях: пересчитать под новый масштаб монитора
        let scaled: Vec<[f64; 4]> = self.shape.iter().map(|r| r.map(|v| v * self.k as f64)).collect();
        if let Err(e) = crate::skin::apply_region(self.hwnd, &scaled, scale as f64) {
            native::log(&format!("main: window shape: {e}"));
        }
        self.frame = None;
        self.frame_px = None;
        self.static_layer = None;
        self.orb_layer = None;
    }
}

/// Размер текстуры рамки в пикселях: как resize() в frame.js — не выше ×1,5 к DIP.
fn frame_size(k: f32, scale: f32) -> (u32, u32) {
    let d = scale.min(1.5);
    ((SKIN_W * k * d).round().max(2.0) as u32, (SKIN_H * k * d).round().max(2.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_covers_the_panel_and_skips_empty_corners() {
        let (w, h) = (SKIN_W as usize, SKIN_H as usize);
        let alpha = vec![0u8; w * h];
        let rects = shape_rects(&alpha, w, h);
        let inside = |x: f64, y: f64| rects.iter().any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
        assert!(inside(640.0, 440.0), "center of the panel is part of the window");
        assert!(!inside(5.0, 5.0), "empty corner lets clicks through");
        assert!(!inside(131.0, 101.0), "rounded panel corner is cut");
    }

    #[test]
    fn spring_settles_on_its_target() {
        let mut s = Spring::new(0.0, 46.0, 7.5);
        s.t = 1.0;
        for _ in 0..600 {
            s.step(1.0 / 60.0);
            s.moving();
        }
        assert_eq!(s.v, 1.0);
    }
}

thread_local! {
    static PROFILE: std::cell::RefCell<std::collections::BTreeMap<&'static str, f64>> = std::cell::RefCell::new(Default::default());
}

/// crtOn: щель по центру → полоса во всю ширину → весь экран; яркость 5 → 4 → 1.
/// Возвращает видимую часть экрана и добавку яркости (0 — как обычно).
fn crt_on(r: Rect, p: f32) -> (Rect, f32) {
    let k = E_OUT.at(p);
    let (ix, iy, b) = if k < 0.3 {
        let t = k / 0.3;
        (0.49 * (1.0 - t), 0.5 - 0.006 * t, 5.0 - t)
    } else {
        let t = (k - 0.3) / 0.7;
        (0.0, 0.494 * (1.0 - t), 4.0 - 3.0 * t)
    };
    (Rect::new(r.x + r.w * ix, r.y + r.h * iy, (r.w * (1.0 - 2.0 * ix)).max(1.0), (r.h * (1.0 - 2.0 * iy)).max(1.0)), b - 1.0)
}
