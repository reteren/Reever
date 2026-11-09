// Рисовальщик нативного пузыря — перенос frontend/bubble.html: та же логика (что показывать,
// когда гаснуть, печать реплик знакомства) и то же оформление (чёрное стекло с радиальным
// градиентом, сканлайны поверх текста, люминофорное свечение текста, мигающая каретка,
// хвостик над головой маскота) с теми же длительностями и кривыми переходов CSS.
// Числа совпадают с bubble.html намеренно — при правке одного правьте и другое.
//
// Отличия, вынужденные платформой:
// - каретка держится за последним словом (в CSS inline-block может уйти на новую строку
//   одна, здесь — вместе со словом);
// - свечение text-shadow — размытие Direct2D с σ = радиус / 2 (как у Chromium);
// - запасные шрифты для иероглифов и арабского — системный подбор DirectWrite по языку
//   интерфейса, а не цепочка Segoe UI → YaHei → Yu Gothic из CSS.

use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_GRADIENT_STOP, D2D_POINT_2F, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, CLSID_D2D1Opacity, ID2D1CommandList, ID2D1Effect,
    ID2D1PathGeometry, ID2D1RadialGradientBrush, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1,
    D2D1_PROPERTY_CACHED, D2D1_PROPERTY_TYPE_BOOL, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout1, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_TEXT_RANGE};

use crate::native::{Color, Direction, Font, GpuCx, RenderCx, TextLayout, TextStyle, WindowHandler};

// ── размеры окна и пузыря (bubble.html, tauri.conf.json) ───────────────
pub const WIN_W: f32 = 340.0;
pub const WIN_H: f32 = 400.0;
const SIDE: f32 = 20.0; // left/right
const BOTTOM: f32 = 9.0;
const MAX_H: f32 = 380.0;
const PAD_TOP: f32 = 9.0;
const PAD_X: f32 = 13.0;
const PAD_BOTTOM: f32 = 10.0;
const RADIUS: f32 = 10.0;
const FONT_SIZE: f32 = 19.0;
const LINE_HEIGHT: f32 = 1.22;
const LETTER: f32 = 0.03 * FONT_SIZE;
const PROMPT_GAP: f32 = 0.35 * FONT_SIZE;
const CARET_W: f32 = 0.5 * FONT_SIZE;
const CARET_H: f32 = 0.95 * FONT_SIZE;
const CARET_ML: f32 = 0.12 * FONT_SIZE;
const CARET_DROP: f32 = 0.1 * FONT_SIZE;
const TAIL_HALF: f32 = 7.0;
const TAIL_H: f32 = 9.0;
const HOLD: Duration = Duration::from_millis(1600);

const ACC: Color = Color::rgb(0x27, 0xE6, 0xAA);
const ACC_HOT: Color = Color::rgb(0xC8, 0xFF, 0xEC);
const ACC_3: Color = Color::rgba(39, 230, 170, 117); // .46
const GLASS: Color = Color::rgb(0x02, 0x0A, 0x08);
const GLASS_2: Color = Color::rgb(0x05, 0x15, 0x0F);

/// Каретка — нулевой ширины символ, которому разрядка даёт ширину каретки: место в строке
/// под неё держит сама разметка.
const CARET_CHAR: char = '\u{2060}';

#[derive(Debug, Clone)]
pub enum Msg {
    State(String),
    Partial(String),
    Said(String),
    Heard(String),
    /// можно ли показываться и где хвостик (DIP окна пузыря)
    Layout { allowed: bool, ax: f32 },
    /// реплики знакомства (bubble_intro)
    Intro(Vec<String>),
    /// «ривер слушай» начался (`true`: текст с чистого листа, пузырь открыт, пока идёт) или
    /// кончился (`false`: текст ещё чуть держится и пузырь закрывается).
    Session(bool),
}

// ── cubic-bezier, как в CSS ─────────────────────────────────────────────
#[derive(Clone, Copy)]
struct Bez(f32, f32, f32, f32);

const EASE_IN: Bez = Bez(0.42, 0.0, 1.0, 1.0);
const EASE_OUT: Bez = Bez(0.0, 0.0, 0.58, 1.0);

impl Bez {
    fn at(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let Bez(x1, y1, x2, y2) = self;
        let curve = |t: f32, a: f32, b: f32| 3.0 * (1.0 - t) * (1.0 - t) * t * a + 3.0 * (1.0 - t) * t * t * b + t * t * t;
        // t по x — делением отрезка пополам (монотонно)
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if curve(mid, x1, x2) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        curve((lo + hi) / 2.0, y1, y2)
    }
}

/// Переход CSS: от текущего значения к цели, с задержкой, длительностью и кривой.
#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    delay: f32,
    dur: f32,
    ease: Bez,
}

impl Tween {
    fn still(v: f32) -> Self {
        Self { from: v, to: v, start: Instant::now(), delay: 0.0, dur: 0.0, ease: EASE_OUT }
    }

    fn value(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32() - self.delay;
        if t <= 0.0 {
            return self.from;
        }
        if self.dur <= 0.0 || t >= self.dur {
            return self.to;
        }
        self.from + (self.to - self.from) * self.ease.at(t / self.dur)
    }

    fn running(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32() < self.delay + self.dur && self.from != self.to
    }

    fn retarget(&mut self, now: Instant, to: f32, delay: f32, dur: f32, ease: Bez) {
        *self = Self { from: self.value(now), to, start: now, delay, dur, ease };
    }
}

/// Печать реплик знакомства (playIntro в bubble.html).
enum Intro {
    /// ждём, пока осьминог выйдет на экран (не дольше 10 с)
    WaitAllowed { since: Instant },
    /// дать ему приземлиться
    Settle { until: Instant },
    Typing { line: usize, chars: usize, next: Instant },
    Pause { line: usize, until: Instant },
    Done { until: Instant },
}

struct Fx {
    generation: u64,
    near: ID2D1Effect,
    wide: ID2D1Effect,
    near_op: ID2D1Effect,
    wide_op: ID2D1Effect,
    caret_blur: ID2D1Effect,
    caret_op: ID2D1Effect,
    glass: ID2D1RadialGradientBrush,
    prompt: ID2D1SolidColorBrush,
    text_list: Option<ID2D1CommandList>,
    caret_list: Option<ID2D1CommandList>,
    tail: ID2D1PathGeometry,
}

pub struct BubbleView {
    // логика bubble.html
    allowed: bool,
    ax: f32,
    active: bool,
    committed: String,
    live: String,
    hold_until: Option<Instant>,
    /// идёт «ривер слушай»: пузырь открыт, пока не кончится
    session: bool,
    /// Reever слушает или занят (State)
    busy: bool,
    intro_lines: Vec<String>,
    intro: Option<Intro>,
    intro_text: Option<String>,
    shown: String,
    on: bool,
    caret_off: bool,
    blink_from: Instant,
    // переходы
    box_op: Tween,
    box_hide: Tween,
    text_op: Tween,
    tail_op: Tween,
    tail_scale: Tween,
    // разметка текущего текста и где каретка
    layout: Option<TextLayout>,
    layout_for: String,
    caret: Option<D2D_RECT_F>,
    locale: String,
    fx: Option<Fx>,
    lists_for: (String, u64),
    on_intro_done: Box<dyn Fn()>,
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (false, false) => format!("{a} {b}"),
        (false, true) => a.to_string(),
        _ => b.to_string(),
    }
}

impl BubbleView {
    pub fn new(locale: &str, on_intro_done: Box<dyn Fn()>) -> Self {
        Self {
            allowed: false,
            ax: 170.0,
            active: false,
            committed: String::new(),
            live: String::new(),
            hold_until: None,
            session: false,
            busy: false,
            intro_lines: Vec::new(),
            intro: None,
            intro_text: None,
            shown: String::new(),
            on: false,
            caret_off: true,
            blink_from: Instant::now(),
            box_op: Tween::still(0.0),
            box_hide: Tween::still(1.0),
            text_op: Tween::still(0.0),
            tail_op: Tween::still(0.0),
            tail_scale: Tween::still(0.2),
            layout: None,
            layout_for: String::new(),
            caret: None,
            locale: locale.to_string(),
            fx: None,
            lists_for: (String::new(), 0),
            on_intro_done,
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        let now = Instant::now();
        match msg {
            Msg::State(s) => {
                self.busy = s == "listening" || s == "busy" || s == "loading";
                if s == "listening" || s == "busy" {
                    if !self.active || self.hold_until.is_some() {
                        if !self.active {
                            self.committed.clear();
                            self.live.clear();
                        }
                        self.active = true;
                    }
                    self.hold_until = None;
                } else if s != "loading" && self.active && self.hold_until.is_none() && !self.session {
                    // текст остаётся до следующей команды: пузырь закрывается вместе с ним
                    self.hold_until = Some(now + HOLD);
                }
            }
            Msg::Partial(v) => {
                self.live = v.trim().to_string();
                if !self.live.is_empty() && !self.active {
                    self.active = true;
                    self.committed.clear();
                }
                // текст пришёл, когда Reever ни слушает, ни занят, — закрыться самому: иначе
                // пузырь, открытый таким текстом, висел бы до следующей команды
                if self.active && !self.busy && !self.session {
                    self.hold_until = Some(now + HOLD);
                }
            }
            Msg::Session(on) => {
                self.session = on;
                if on {
                    self.committed.clear();
                    self.live.clear();
                    self.active = true;
                    self.hold_until = None;
                } else {
                    self.live.clear();
                    self.hold_until = Some(now + HOLD);
                }
            }
            Msg::Said(v) => {
                self.committed = join(&self.committed, v.trim());
                self.live.clear();
            }
            Msg::Heard(v) => {
                let v = v.trim();
                if !v.is_empty() {
                    self.committed = v.to_string();
                    self.live.clear();
                }
            }
            Msg::Layout { allowed, ax } => {
                self.allowed = allowed;
                self.ax = ax;
            }
            Msg::Intro(lines) => {
                if self.intro.is_none() && !lines.is_empty() {
                    self.intro_lines = lines;
                    self.intro = Some(Intro::WaitAllowed { since: now });
                    self.intro_text = Some(String::new());
                }
            }
        }
        self.update(now);
    }

    /// Таймеры (удержание, печать знакомства) и пересчёт показа — render() в bubble.html.
    fn update(&mut self, now: Instant) {
        if self.hold_until.is_some_and(|t| now >= t) {
            self.hold_until = None;
            self.active = false;
        }
        self.step_intro(now);
        let text = match &self.intro_text {
            Some(t) => t.clone(),
            None => join(&self.committed, &self.live).trim().to_string(),
        };
        if !text.is_empty() {
            self.shown = text.clone();
        }
        let caret_off = self.intro_text.is_none() && (!self.active || self.hold_until.is_some());
        if caret_off != self.caret_off {
            self.caret_off = caret_off;
            self.blink_from = now; // анимация мигания начинается заново, когда каретку показали
        }
        let on = self.allowed && (self.intro_text.is_some() || self.active) && !text.is_empty();
        if on != self.on {
            self.on = on;
            if on {
                self.box_op.retarget(now, 1.0, 0.0, 0.16, EASE_OUT);
                self.box_hide.retarget(now, 0.0, 0.0, 0.3, Bez(0.16, 1.0, 0.3, 1.0));
                self.text_op.retarget(now, 1.0, 0.0, 0.16, EASE_IN);
                self.tail_op.retarget(now, 1.0, 0.0, 0.12, EASE_OUT);
                self.tail_scale.retarget(now, 1.0, 0.0, 0.2, Bez(0.16, 1.0, 0.3, 1.0));
            } else {
                self.box_op.retarget(now, 0.0, 0.08, 0.34, Bez(0.4, 0.0, 0.9, 0.6));
                self.box_hide.retarget(now, 1.0, 0.06, 0.42, Bez(0.55, 0.0, 0.7, 0.2));
                self.text_op.retarget(now, 0.0, 0.0, 0.16, EASE_IN);
                self.tail_op.retarget(now, 0.0, 0.3, 0.2, EASE_IN);
                self.tail_scale.retarget(now, 0.2, 0.28, 0.24, Bez(0.55, 0.0, 0.7, 0.2));
            }
        }
    }

    fn step_intro(&mut self, now: Instant) {
        loop {
            let next = match self.intro.take() {
                None => return,
                Some(Intro::WaitAllowed { since }) => {
                    if self.allowed || now.saturating_duration_since(since) >= Duration::from_secs(10) {
                        Intro::Settle { until: now + Duration::from_millis(900) }
                    } else {
                        self.intro = Some(Intro::WaitAllowed { since });
                        return;
                    }
                }
                Some(Intro::Settle { until }) if now < until => {
                    self.intro = Some(Intro::Settle { until });
                    return;
                }
                Some(Intro::Settle { until }) => Intro::Typing { line: 0, chars: 0, next: until },
                Some(Intro::Typing { line, chars, next }) => {
                    let Some(text) = self.intro_lines.get(line) else {
                        self.intro = Some(Intro::Done { until: now + Duration::from_millis(500) });
                        self.intro_text = None;
                        continue;
                    };
                    if now < next {
                        self.intro = Some(Intro::Typing { line, chars, next });
                        return;
                    }
                    let total = text.chars().count();
                    let chars = chars + 1;
                    self.intro_text = Some(text.chars().take(chars).collect());
                    if chars >= total {
                        // 1400 мс + 40 мс на букву, считая от последней напечатанной
                        let wait = Duration::from_millis(1400 + 40 * total as u64);
                        Intro::Pause { line, until: next + Duration::from_millis(34) + wait }
                    } else {
                        Intro::Typing { line, chars, next: next + Duration::from_millis(34) }
                    }
                }
                Some(Intro::Pause { line, until }) if now < until => {
                    self.intro = Some(Intro::Pause { line, until });
                    return;
                }
                Some(Intro::Pause { line, until }) => {
                    if line + 1 < self.intro_lines.len() {
                        Intro::Typing { line: line + 1, chars: 0, next: until }
                    } else {
                        self.intro_text = None;
                        Intro::Done { until: until + Duration::from_millis(500) }
                    }
                }
                Some(Intro::Done { until }) if now < until => {
                    self.intro = Some(Intro::Done { until });
                    return;
                }
                Some(Intro::Done { .. }) => {
                    self.intro_lines.clear();
                    (self.on_intro_done)();
                    return;
                }
            };
            self.intro = Some(next);
        }
    }

    fn blink_visible(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.blink_from).as_secs_f32().rem_euclid(1.0) < 0.5
    }

    fn style(&self) -> TextStyle {
        TextStyle {
            font: Font::Handjet,
            size: FONT_SIZE,
            weight: 400,
            direction: Direction::Ltr, // как у страницы: абзац слева направо, bidi внутри
            locale: self.locale.clone(),
            axes: vec![(*b"ELSH", 2.0), (*b"ELGR", 1.0)],
            line_height: Some(LINE_HEIGHT),
            letter_spacing: LETTER,
            break_words: true,
            // --f-lcd: 'Handjet', 'Segoe UI', 'Microsoft YaHei UI', 'Yu Gothic UI', sans-serif
            fallback: vec!["Segoe UI".into(), "Microsoft YaHei UI".into(), "Yu Gothic UI".into()],
            ..Default::default()
        }
    }

    fn box_height(&self) -> f32 {
        let th = self.layout.as_ref().map_or(0.0, |l| l.height);
        (th + PAD_TOP + PAD_BOTTOM).min(MAX_H)
    }

    fn ensure_fx(&mut self, gpu: &GpuCx<'_>) -> windows::core::Result<()> {
        if self.fx.as_ref().is_some_and(|f| f.generation == gpu.generation) {
            return Ok(());
        }
        self.fx = None;
        self.lists_for = (String::new(), 0);
        let dc = gpu.dc;
        unsafe {
            let blur = |sigma: f32| -> windows::core::Result<ID2D1Effect> {
                let e = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
                e.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, as_bytes(&sigma))?;
                e.SetValue(D2D1_PROPERTY_CACHED.0 as u32, D2D1_PROPERTY_TYPE_BOOL, as_bytes(&BOOL(1)))?;
                Ok(e)
            };
            let opacity = |input: &ID2D1Effect, v: f32| -> windows::core::Result<ID2D1Effect> {
                let e = dc.CreateEffect(&CLSID_D2D1Opacity)?;
                e.SetInput(0, &input.GetOutput()?, true);
                e.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, as_bytes(&v))?;
                Ok(e)
            };
            // text-shadow: 0 0 1px rgba(acc, .9), 0 0 7px rgba(acc, .45); box-shadow каретки 0 0 6px
            let near = blur(0.5)?;
            let wide = blur(3.5)?;
            let caret_blur = blur(3.0)?;
            let near_op = opacity(&near, 0.9)?;
            let wide_op = opacity(&wide, 0.45)?;
            let caret_op = opacity(&caret_blur, 1.0)?;
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: GLASS_2.d2d() },
                D2D1_GRADIENT_STOP { position: 0.7, color: GLASS.d2d() },
            ];
            let rt: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = dc;
            let stops = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let glass = dc.CreateRadialGradientBrush(
                &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: D2D_POINT_2F::default(), gradientOriginOffset: D2D_POINT_2F::default(), radiusX: 1.0, radiusY: 1.0 },
                None,
                &stops,
            )?;
            let prompt = dc.CreateSolidColorBrush(&ACC_3.d2d(), None)?;
            let factory = dc.GetFactory()?;
            let tail = factory.CreatePathGeometry()?;
            let sink = tail.Open()?;
            // хвостик: остриё в (0, 0), основание выше на TAIL_H — ставится переносом
            sink.BeginFigure(D2D_POINT_2F { x: -TAIL_HALF, y: -TAIL_H }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&[D2D_POINT_2F { x: TAIL_HALF, y: -TAIL_H }, D2D_POINT_2F { x: 0.0, y: 0.0 }]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            self.fx = Some(Fx {
                generation: gpu.generation,
                near,
                wide,
                near_op,
                wide_op,
                caret_blur,
                caret_op,
                glass,
                prompt,
                text_list: None,
                caret_list: None,
                tail,
            });
        }
        Ok(())
    }

    /// Разметка текста «› текст⁠» и место каретки.
    fn relayout(&mut self, gpu: &GpuCx<'_>) {
        if self.layout.is_some() && self.layout_for == self.shown {
            return;
        }
        let full = format!("›{}{CARET_CHAR}", self.shown);
        let width = WIN_W - 2.0 * SIDE - 2.0 * PAD_X;
        let Ok(layout) = gpu.text.layout(&full, &self.style(), width) else { return };
        let n = full.encode_utf16().count() as u32;
        unsafe {
            if let Ok(l1) = layout.layout.cast::<IDWriteTextLayout1>() {
                // отступ после «›» (margin-right .35em) и ширина каретки с её отступом слева
                let _ = l1.SetCharacterSpacing(0.0, LETTER + PROMPT_GAP, 0.0, DWRITE_TEXT_RANGE { startPosition: 0, length: 1 });
                let _ = l1.SetCharacterSpacing(CARET_ML, 0.0, CARET_W, DWRITE_TEXT_RANGE { startPosition: n - 1, length: 1 });
            }
        }
        let mut layout = layout;
        unsafe {
            let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
            if layout.layout.GetMetrics(&mut m).is_ok() {
                layout.height = m.height;
            }
            // каретка: левый край символа-заглушки + отступ, низ — на .1em ниже базовой линии
            let (mut x, mut y) = (0.0f32, 0.0f32);
            let mut hit = DWRITE_HIT_TEST_METRICS::default();
            self.caret = None;
            if layout.layout.HitTestTextPosition(n - 1, false, &mut x, &mut y, &mut hit).is_ok() {
                let mut count = 0u32;
                let _ = layout.layout.GetLineMetrics(None, &mut count);
                let mut lines = vec![DWRITE_LINE_METRICS::default(); count as usize];
                if layout.layout.GetLineMetrics(Some(&mut lines), &mut count).is_ok() {
                    let (mut pos, mut top) = (0u32, 0.0f32);
                    for line in &lines {
                        if n - 1 < pos + line.length {
                            let bottom = top + line.baseline + CARET_DROP;
                            let left = x + CARET_ML;
                            self.caret = Some(D2D_RECT_F { left, top: bottom - CARET_H, right: left + CARET_W, bottom });
                            break;
                        }
                        pos += line.length;
                        top += line.height;
                    }
                }
            }
        }
        self.layout = Some(layout);
        self.layout_for = self.shown.clone();
    }

    /// Списки команд: текст и каретка цветом свечения — входы размытия.
    fn record(&mut self, gpu: &GpuCx<'_>) {
        let key = (self.shown.clone(), gpu.generation);
        if self.lists_for == key {
            return;
        }
        let (Some(fx), Some(layout)) = (self.fx.as_mut(), self.layout.as_ref()) else { return };
        let dc = gpu.dc;
        unsafe {
            let Ok(brush) = dc.CreateSolidColorBrush(&ACC.d2d(), None) else { return };
            let old = dc.GetTarget().ok();
            let rec = |draw: &dyn Fn()| -> Option<ID2D1CommandList> {
                let list = dc.CreateCommandList().ok()?;
                dc.SetTarget(&list);
                dc.BeginDraw();
                dc.SetTransform(&Matrix3x2::identity());
                draw();
                let _ = dc.EndDraw(None, None);
                list.Close().ok()?;
                Some(list)
            };
            fx.text_list = rec(&|| dc.DrawTextLayout(D2D_POINT_2F::default(), &layout.layout, &brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT));
            let caret = self.caret;
            fx.caret_list = caret.and_then(|r| rec(&|| dc.FillRectangle(&r, &brush)));
            if let Some(t) = old {
                dc.SetTarget(&t);
            } else {
                dc.SetTarget(None);
            }
            if let Some(l) = &fx.text_list {
                fx.near.SetInput(0, l, true);
                fx.wide.SetInput(0, l, true);
            }
            if let Some(l) = &fx.caret_list {
                fx.caret_blur.SetInput(0, l, true);
            }
        }
        self.lists_for = key;
    }
}

fn as_bytes<T>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

fn translate(x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: x, M32: y }
}

fn scale(sx: f32, sy: f32) -> Matrix3x2 {
    Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: 0.0, M32: 0.0 }
}

impl WindowHandler for BubbleView {
    fn animating(&self) -> bool {
        let now = Instant::now();
        [&self.box_op, &self.box_hide, &self.text_op, &self.tail_op, &self.tail_scale].iter().any(|t| t.running(now))
    }

    fn wake_at(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut at: Vec<Instant> = Vec::new();
        if let Some(t) = self.hold_until {
            at.push(t);
        }
        match &self.intro {
            Some(Intro::WaitAllowed { since }) => at.push(*since + Duration::from_secs(10)),
            Some(Intro::Settle { until } | Intro::Pause { until, .. } | Intro::Done { until }) => at.push(*until),
            Some(Intro::Typing { next, .. }) => at.push(*next),
            None => {}
        }
        // мигание каретки: следующая смена через полсекунды от начала мигания
        if self.on && !self.caret_off {
            let t = now.saturating_duration_since(self.blink_from).as_secs_f32();
            let next = ((t / 0.5).floor() + 1.0) * 0.5;
            at.push(self.blink_from + Duration::from_secs_f32(next));
        }
        at.into_iter().min()
    }

    fn prepare(&mut self, gpu: &GpuCx<'_>) {
        self.update(gpu.now);
        if let Err(e) = self.ensure_fx(gpu) {
            eprintln!("[reever] bubble fx: {e}");
            return;
        }
        if self.shown.is_empty() {
            return;
        }
        self.relayout(gpu);
        self.record(gpu);
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        let now = cx.now;
        let (box_op, hide, text_op, tail_op, tail_sc) = (
            self.box_op.value(now),
            self.box_hide.value(now),
            self.text_op.value(now),
            self.tail_op.value(now),
            self.tail_scale.value(now),
        );
        let (Some(fx), Some(layout)) = (self.fx.as_ref(), self.layout.as_ref()) else { return };
        let dc = cx.dc().clone();
        let bottom = WIN_H - BOTTOM;
        let h = self.box_height();
        let rect = D2D_RECT_F { left: SIDE, top: bottom - h, right: WIN_W - SIDE, bottom };
        unsafe {
            if box_op > 0.001 {
                // translateY(3px) scale(.5, .12) от хвостика — в долях скрытости
                // мониторчик (ax < 0) раскрывается от середины
                let (o_x, o_y) = (if self.ax < 0.0 { WIN_W / 2.0 } else { self.ax }, bottom);
                let m = translate(-o_x, -o_y) * scale(1.0 - 0.5 * hide, 1.0 - 0.88 * hide) * translate(o_x, o_y + 3.0 * hide);
                dc.SetTransform(&m);
                let factory = dc.GetFactory();
                let shape = factory.and_then(|f| f.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect, radiusX: RADIUS, radiusY: RADIUS }));
                if let Ok(shape) = shape {
                    let params = D2D1_LAYER_PARAMETERS1 {
                        contentBounds: rect,
                        geometricMask: std::mem::ManuallyDrop::new(Some(shape.cast().expect("geometry"))),
                        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                        maskTransform: Matrix3x2::identity(),
                        opacity: box_op,
                        opacityBrush: std::mem::ManuallyDrop::new(None),
                        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                    };
                    dc.PushLayer(&params, None);
                    std::mem::ManuallyDrop::into_inner(params.geometricMask);
                    // стекло: radial-gradient(120% 90% at 50% 40%, glass-2 0%, glass 70%)
                    let (w, hh) = (rect.right - rect.left, rect.bottom - rect.top);
                    fx.glass.SetCenter(D2D_POINT_2F { x: rect.left + w * 0.5, y: rect.top + hh * 0.4 });
                    fx.glass.SetRadiusX(w * 1.2);
                    fx.glass.SetRadiusY(hh * 0.9);
                    dc.FillRectangle(&rect, &fx.glass);
                    // текст прижат к низу (flex-end); что выше пузыря — обрезано, видны последние строки
                    let tx = rect.left + PAD_X;
                    let ty = bottom - PAD_BOTTOM - layout.height;
                    if text_op > 0.001 {
                        let text_layer = D2D1_LAYER_PARAMETERS1 {
                            contentBounds: rect,
                            geometricMask: std::mem::ManuallyDrop::new(None),
                            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                            maskTransform: Matrix3x2::identity(),
                            opacity: text_op,
                            opacityBrush: std::mem::ManuallyDrop::new(None),
                            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                        };
                        dc.PushLayer(&text_layer, None);
                        let off = D2D_POINT_2F { x: tx, y: ty };
                        for e in [&fx.wide_op, &fx.near_op] {
                            if let Ok(img) = e.GetOutput() {
                                dc.DrawImage(&img, Some(&off), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                            }
                        }
                        // «›» — цветом acc-3 (эффект рисования на первый символ), остальное — acc-hot
                        let _ = layout.layout.SetDrawingEffect(&fx.prompt, DWRITE_TEXT_RANGE { startPosition: 0, length: 1 });
                        dc.DrawTextLayout(off, &layout.layout, cx.brush(ACC_HOT), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                        if !self.caret_off && self.blink_visible(now) {
                            if let (Some(c), Ok(img)) = (self.caret, fx.caret_op.GetOutput()) {
                                dc.DrawImage(&img, Some(&off), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                                let r = D2D_RECT_F { left: c.left + tx, top: c.top + ty, right: c.right + tx, bottom: c.bottom + ty };
                                dc.FillRectangle(&r, cx.brush(ACC));
                            }
                        }
                        dc.PopLayer();
                    }
                    // сканлайны ЭЛТ поверх текста: снизу вверх полоса 1 px через 3 px
                    let dark = cx.brush(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.28 });
                    let mut y = bottom;
                    while y > rect.top {
                        dc.FillRectangle(&D2D_RECT_F { left: rect.left, top: (y - 1.0).max(rect.top), right: rect.right, bottom: y }, dark);
                        y -= 3.0;
                    }
                    dc.PopLayer();
                }
            }
            // у мониторчика лёгкого вида хвостика нет: маскота, на которого он указывал бы, нет
            if tail_op > 0.001 && self.ax >= 0.0 {
                let x = self.ax.clamp(34.0, WIN_W - 34.0);
                let m = scale(tail_sc, tail_sc) * translate(x, WIN_H);
                dc.SetTransform(&m);
                let mut c = GLASS;
                c.a = tail_op;
                dc.FillGeometry(&fx.tail, cx.brush(c), None);
            }
            dc.SetTransform(&Matrix3x2::identity());
        }
    }
}
