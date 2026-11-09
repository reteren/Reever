//! Элементы управления нативных окон в стиле скина «Пульт с ЭЛТ» (frontend/skin.css):
//! кнопки, купол-«лунка», список с выделением и прокруткой, поле ввода, подсказка, значки.
//! Первые нужны окнам выбора и подтверждения (этап 3), дальше — главному окну (этап 4).
//!
//! Устройство простое: элемент — структура с состоянием (наведение, нажатие, фокус, переходы)
//! и методами `hit`/`draw`; раскладку (прямоугольники) задаёт окно. Текст рисуется через
//! [`Scene`]: в кадре тексты копятся, а в конце под ними рисуется свечение (CSS text-shadow
//! уровней g1/g2/g3) — размытые списки команд Direct2D, по одному на проход свечения.

pub mod anim;
pub mod button;
pub mod form;
pub mod icons;
pub mod lite;
pub mod list;
pub mod text_input;
pub mod tooltip;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use windows::core::Interface;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_GRADIENT_STOP, D2D_POINT_2F, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, ID2D1DeviceContext, ID2D1Effect, ID2D1Geometry, ID2D1RenderTarget,
    ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_CAP_STYLE_FLAT, D2D1_DASH_STYLE_CUSTOM,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_PROPERTY_CACHED,
    D2D1_PROPERTY_TYPE_BOOL, D2D1_PROPERTY_TYPE_FLOAT, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES,
};

use super::{Color, Font, Rect, RenderCx, Text, TextLayout, TextStyle};

// ── палитра скина (skin.css :root) ─────────────────────────────────────
pub const ACC: Color = Color::rgb(0x27, 0xE6, 0xAA);
pub const ACC_HOT: Color = Color::rgb(0xC8, 0xFF, 0xEC);
pub const ACC_2: Color = Color::rgba(39, 230, 170, 184); // .72
pub const ACC_3: Color = Color::rgba(39, 230, 170, 117); // .46
pub const ACC_4: Color = Color::rgba(39, 230, 170, 56); // .22
pub const ACC_5: Color = Color::rgba(39, 230, 170, 26); // .10
pub const GLASS: Color = Color::rgb(0x02, 0x0A, 0x08);
pub const GLASS_2: Color = Color::rgb(0x05, 0x15, 0x0F);
pub const GLASS_3: Color = Color::rgb(0x0A, 0x22, 0x19);

pub fn with_alpha(c: Color, a: f32) -> Color {
    Color { a: c.a * a, ..c }
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

/// Запасные шрифты скина: 'Segoe UI', 'Microsoft YaHei UI', 'Yu Gothic UI'.
fn fallback() -> Vec<String> {
    vec!["Segoe UI".into(), "Microsoft YaHei UI".into(), "Yu Gothic UI".into()]
}

/// --f-lcd: Handjet с осями ELSH 2, ELGR 1 (как font-variation-settings у экранов).
pub fn lcd(size: f32, weight: u16, letter_em: f32) -> TextStyle {
    TextStyle {
        font: Font::Handjet,
        size,
        weight,
        axes: vec![(*b"ELSH", 2.0), (*b"ELGR", 1.0)],
        letter_spacing: letter_em * size,
        line_height: Some(1.0),
        wrap: false,
        fallback: fallback(),
        ..Default::default()
    }
}

/// --f-ui: Geologica (SHRP 60 — font-variation-settings у body).
pub fn ui(size: f32, weight: u16, line_height: f32) -> TextStyle {
    TextStyle {
        font: Font::Geologica,
        size,
        weight,
        axes: vec![(*b"SHRP", 60.0)],
        line_height: Some(line_height),
        wrap: false,
        fallback: fallback(),
        ..Default::default()
    }
}

/// --f-data: Martian Mono.
pub fn data(size: f32, line_height: f32) -> TextStyle {
    TextStyle {
        font: Font::MartianMono,
        size,
        weight: 400,
        line_height: Some(line_height),
        wrap: false,
        fallback: vec!["Consolas".into(), "Microsoft YaHei UI".into()],
        ..Default::default()
    }
}

/// Кэш разметок текста: окно рисует одни и те же надписи из кадра в кадр.
#[derive(Default)]
pub struct TextCache {
    map: HashMap<u64, Rc<TextLayout>>,
}

fn style_key(text: &str, style: &TextStyle, width: f32, extra: u8, h: &mut std::collections::hash_map::DefaultHasher) {
    text.hash(h);
    format!("{style:?}").hash(h);
    width.to_bits().hash(h);
    extra.hash(h);
}

impl TextCache {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Разметка текста по ширине (кэшируется).
    pub fn get(&mut self, text: &Text, s: &str, style: &TextStyle, width: f32) -> Option<Rc<TextLayout>> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        style_key(s, style, width, 0, &mut h);
        let key = h.finish();
        if let Some(l) = self.map.get(&key) {
            return Some(l.clone());
        }
        // разметка длинной строки журнала — десятки КБ; 2000 штук держали десятки МБ
        if self.map.len() > 500 {
            self.map.clear();
        }
        let l = Rc::new(text.layout(s, style, width).ok()?);
        self.map.insert(key, l.clone());
        Some(l)
    }

    /// Одна строка, не влезающее — многоточие в НАЧАЛЕ: виден конец (CSS direction: rtl +
    /// text-overflow: ellipsis у пути к файлу — самое различимое у пути в конце).
    pub fn get_tail(&mut self, text: &Text, s: &str, style: &TextStyle, width: f32) -> Option<Rc<TextLayout>> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        style_key(s, style, width, 1, &mut h);
        let key = h.finish();
        if let Some(l) = self.map.get(&key) {
            return Some(l.clone());
        }
        let wide = TextStyle { wrap: false, ellipsis: false, ..style.clone() };
        let full = text.layout(s, &wide, 100_000.0).ok()?;
        let chars: Vec<char> = s.chars().collect();
        let fitted = if full.width <= width + 0.5 {
            full
        } else {
            // наибольший хвост, который с «…» влезает в ширину — делением пополам
            let (mut lo, mut hi) = (0usize, chars.len());
            let mut best = None;
            while lo < hi {
                let mid = (lo + hi + 1) / 2;
                let tail: String = std::iter::once('…').chain(chars[chars.len() - mid..].iter().copied()).collect();
                let l = text.layout(&tail, &wide, 100_000.0).ok()?;
                if l.width <= width + 0.5 {
                    best = Some(l);
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            match best {
                Some(l) => l,
                None => text.layout("…", &wide, 100_000.0).ok()?,
            }
        };
        let l = Rc::new(fitted);
        self.map.insert(key, l.clone());
        Some(l)
    }
}

/// Свечение текста — уровни из skin.css.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Glow {
    None,
    /// --g1: 0 0 1px rgba(acc, .45)
    G1,
    /// --g2: 0 0 1px rgba(acc, .9), 0 0 7px rgba(acc, .45)
    G2,
    /// --g3: 0 0 1px acc-hot, 0 0 5px acc, 0 0 16px rgba(acc, .55)
    G3,
}

/// Проходы свечения: цвет (с альфой) и σ размытия (радиус CSS / 2).
fn passes(g: Glow) -> &'static [(u8, f32, f32)] {
    // (0 — acc, 1 — acc-hot; альфа; σ)
    match g {
        Glow::None => &[],
        Glow::G1 => &[(0, 0.45, 0.5)],
        Glow::G2 => &[(0, 0.9, 0.5), (0, 0.45, 3.5)],
        Glow::G3 => &[(1, 1.0, 0.5), (0, 1.0, 2.5), (0, 0.55, 8.0)],
    }
}

enum Item {
    Text { layout: Rc<TextLayout>, x: f32, y: f32, color: Color, opacity: f32 },
    Icon { geometry: ID2D1Geometry, m: Matrix3x2, stroke: f32, color: Color, fill: bool, opacity: f32 },
}

struct Queued {
    item: Item,
    glow: Glow,
    clip: Option<D2D_RECT_F>,
}

/// Кадр окна: тексты и значки копятся, `flush` рисует под ними свечение и их самих.
#[derive(Default)]
pub struct Scene {
    queue: Vec<Queued>,
    blurs: HashMap<u32, ID2D1Effect>,
    generation: u64,
    clip: Option<D2D_RECT_F>,
    pub cache: TextCache,
}

impl Scene {
    /// Начать кадр окна: фабрика геометрии — от контекста устройства, очередь пуста.
    pub fn begin_frame(&mut self, cx: &RenderCx<'_>) {
        icons::use_factory_of(cx.dc());
        self.begin(self.generation);
    }

    /// Начать кадр (эффекты прошлого устройства выбрасываются).
    pub fn begin(&mut self, generation: u64) {
        if generation != self.generation {
            self.blurs.clear();
            self.generation = generation;
        }
        self.queue.clear();
        self.clip = None;
    }

    /// Обрезка для следующих текстов (overflow: hidden у прокручиваемого списка).
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip.map(|r| r.d2d());
    }

    pub fn text(&mut self, layout: &Rc<TextLayout>, x: f32, y: f32, color: Color, glow: Glow) {
        self.text_alpha(layout, x, y, color, glow, 1.0);
    }

    pub fn text_alpha(&mut self, layout: &Rc<TextLayout>, x: f32, y: f32, color: Color, glow: Glow, opacity: f32) {
        self.queue.push(Queued {
            item: Item::Text { layout: layout.clone(), x, y, color, opacity },
            glow,
            clip: self.clip,
        });
    }

    /// Значок (штрих или заливка) в квадрате `r`; толщина штриха — в единицах viewBox 24×24,
    /// как stroke-width у SVG.
    pub fn icon(&mut self, name: &str, r: Rect, stroke: f32, color: Color, fill: bool, glow: Glow) {
        let Some(g) = icons::geometry(name) else { return };
        let k = r.w / 24.0;
        let m = Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: r.x, M32: r.y };
        self.queue.push(Queued { item: Item::Icon { geometry: g, m, stroke, color, fill, opacity: 1.0 }, glow, clip: self.clip });
    }

    fn blur(&mut self, dc: &ID2D1DeviceContext, sigma: f32) -> Option<ID2D1Effect> {
        let key = (sigma * 100.0) as u32;
        if let Some(e) = self.blurs.get(&key) {
            return Some(e.clone());
        }
        unsafe {
            let e = dc.CreateEffect(&CLSID_D2D1GaussianBlur).ok()?;
            e.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, as_bytes(&sigma)).ok()?;
            e.SetValue(D2D1_PROPERTY_CACHED.0 as u32, D2D1_PROPERTY_TYPE_BOOL, as_bytes(&BOOL(0))).ok()?;
            self.blurs.insert(key, e.clone());
            Some(e)
        }
    }

    /// Ореол вокруг прямоугольника (CSS box-shadow: 0 0 <blur> <color>) — рисуется сразу.
    pub fn shadow(&mut self, cx: &RenderCx<'_>, r: Rect, radius: f32, color: Color, blur: f32) {
        if color.a <= 0.003 {
            return;
        }
        let dc = cx.dc().clone();
        unsafe {
            let base = transform(&dc);
            let Ok(list) = dc.CreateCommandList() else { return };
            let old = dc.GetTarget().ok();
            dc.SetTarget(&list);
            dc.FillRoundedRectangle(&rounded(r, radius), cx.brush(color));
            if let Some(t) = old {
                dc.SetTarget(&t);
            }
            if list.Close().is_err() {
                return;
            }
            if let Some(b) = self.blur(&dc, blur / 2.0) {
                b.SetInput(0, &list, true);
                if let Ok(img) = b.GetOutput() {
                    dc.SetTransform(&Matrix3x2::identity());
                    dc.DrawImage(&img, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                    dc.SetTransform(&base);
                }
            }
        }
    }

    /// Нарисовать накопленное: сначала свечение всех уровней, потом сами тексты и значки.
    pub fn flush(&mut self, cx: &mut RenderCx<'_>) {
        let dc = cx.dc().clone();
        let queue = std::mem::take(&mut self.queue);
        unsafe {
            let base = transform(&dc);
            // проходы свечения: (цвет, альфа, σ) → список команд из всех элементов прохода
            let mut keys: Vec<(u8, u32, u32)> = Vec::new();
            for q in &queue {
                for &(c, a, s) in passes(q.glow) {
                    let k = (c, (a * 100.0) as u32, (s * 100.0) as u32);
                    if !keys.contains(&k) {
                        keys.push(k);
                    }
                }
            }
            for (c, a, s) in keys {
                let color = with_alpha(if c == 1 { ACC_HOT } else { ACC }, a as f32 / 100.0);
                let Ok(list) = dc.CreateCommandList() else { continue };
                let old = dc.GetTarget().ok();
                dc.SetTarget(&list);
                for q in queue.iter().filter(|q| passes(q.glow).iter().any(|p| (p.0, (p.1 * 100.0) as u32, (p.2 * 100.0) as u32) == (c, a, s))) {
                    draw_item(&dc, cx, &q.item, Some(color), base, q.clip);
                }
                if let Some(t) = old {
                    dc.SetTarget(&t);
                }
                if list.Close().is_err() {
                    continue;
                }
                if let Some(blur) = self.blur(&dc, s as f32 / 100.0) {
                    blur.SetInput(0, &list, true);
                    if let Ok(img) = blur.GetOutput() {
                        dc.SetTransform(&Matrix3x2::identity());
                        dc.DrawImage(&img, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                        dc.SetTransform(&base);
                    }
                }
            }
            for q in &queue {
                draw_item(&dc, cx, &q.item, None, base, q.clip);
            }
            dc.SetTransform(&base);
        }
    }
}

fn transform(dc: &ID2D1DeviceContext) -> Matrix3x2 {
    let mut m = Matrix3x2::default();
    unsafe { dc.GetTransform(&mut m) };
    m
}

/// Элемент очереди; `glow_color` — рисуется в список свечения этим цветом (без прозрачности
/// самого текста: тень CSS — по форме глифов).
unsafe fn draw_item(dc: &ID2D1DeviceContext, cx: &RenderCx<'_>, item: &Item, glow_color: Option<Color>, base: Matrix3x2, clip: Option<D2D_RECT_F>) {
    // в списке команд перенос рисуется вместе с элементом (список потом кладётся без переноса)
    if glow_color.is_some() {
        dc.SetTransform(&base);
    }
    if let Some(c) = clip {
        dc.PushAxisAlignedClip(&c, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    match item {
        Item::Text { layout, x, y, color, opacity } => {
            let c = glow_color.unwrap_or(with_alpha(*color, *opacity));
            dc.DrawTextLayout(D2D_POINT_2F { x: *x, y: *y }, &layout.layout, cx.brush(c), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        Item::Icon { geometry, m, stroke, color, fill, opacity } => {
            let c = glow_color.unwrap_or(with_alpha(*color, *opacity));
            dc.SetTransform(&(*m * base));
            if *fill {
                dc.FillGeometry(geometry, cx.brush(c), None);
            } else {
                dc.DrawGeometry(geometry, cx.brush(c), *stroke, &icons::round_stroke());
            }
            dc.SetTransform(&base);
        }
    }
    if clip.is_some() {
        dc.PopAxisAlignedClip();
    }
}

pub fn as_bytes<T>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

pub fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius }
}

pub fn fill_round(cx: &RenderCx<'_>, r: Rect, radius: f32, color: Color) {
    unsafe { cx.dc().FillRoundedRectangle(&rounded(r, radius), cx.brush(color)) };
}

/// Рамка 1px внутри прямоугольника (CSS border), сплошная или штрихами (dashed).
pub fn stroke_round(cx: &RenderCx<'_>, r: Rect, radius: f32, color: Color, width: f32, dashed: bool) {
    let inset = Rect::new(r.x + width / 2.0, r.y + width / 2.0, r.w - width, r.h - width);
    let style = if dashed { dash_style(3.0 * width, 3.0 * width) } else { None };
    unsafe { cx.dc().DrawRoundedRectangle(&rounded(inset, (radius - width / 2.0).max(0.0)), cx.brush(color), width, style.as_ref()) };
}

/// Горизонтальная линия 1px «dotted» (border-bottom: 1px dotted).
pub fn dotted_hline(cx: &RenderCx<'_>, x: f32, y: f32, w: f32, color: Color) {
    let style = dash_style(1.0, 1.0);
    unsafe {
        cx.dc().DrawLine(D2D_POINT_2F { x, y: y + 0.5 }, D2D_POINT_2F { x: x + w, y: y + 0.5 }, cx.brush(color), 1.0, style.as_ref());
    }
}

pub(crate) fn reset_dashes() {
    DASHES.with(|d| d.borrow_mut().clear());
}

thread_local! {
    static DASHES: std::cell::RefCell<HashMap<(u32, u32), ID2D1StrokeStyle>> = std::cell::RefCell::new(HashMap::new());
}

/// Штрих «длина/пропуск» в долях толщины линии (как dashes у Direct2D).
fn dash_style(on: f32, off: f32) -> Option<ID2D1StrokeStyle> {
    let key = ((on * 10.0) as u32, (off * 10.0) as u32);
    if let Some(s) = DASHES.with(|d| d.borrow().get(&key).cloned()) {
        return Some(s);
    }
    let factory = icons::factory()?;
    let props = D2D1_STROKE_STYLE_PROPERTIES {
        startCap: D2D1_CAP_STYLE_FLAT,
        endCap: D2D1_CAP_STYLE_FLAT,
        dashCap: D2D1_CAP_STYLE_FLAT,
        dashStyle: D2D1_DASH_STYLE_CUSTOM,
        ..Default::default()
    };
    let f0: &windows::Win32::Graphics::Direct2D::ID2D1Factory = &factory;
    let s = unsafe { f0.CreateStrokeStyle(&props, Some(&[on, off])).ok()? };
    DASHES.with(|d| d.borrow_mut().insert(key, s.clone()));
    Some(s)
}

/// Слой с обрезкой по скруглённому прямоугольнику и прозрачностью. Закрыть `pop_layer`.
pub fn push_round_layer(cx: &RenderCx<'_>, r: Rect, radius: f32, opacity: f32) -> bool {
    let Some(factory) = icons::factory() else { return false };
    unsafe {
        let Ok(shape) = factory.CreateRoundedRectangleGeometry(&rounded(r, radius)) else { return false };
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: r.d2d(),
            geometricMask: std::mem::ManuallyDrop::new(Some(shape.cast().expect("geometry"))),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        cx.dc().PushLayer(&params, None);
        std::mem::ManuallyDrop::into_inner(params.geometricMask);
    }
    true
}

pub fn pop_layer(cx: &RenderCx<'_>) {
    unsafe { cx.dc().PopLayer() };
}

/// Радиальный градиент (для лёгкого вида и экранов).
pub(crate) fn radial_brush(cx: &RenderCx<'_>, center: (f32, f32), radius: (f32, f32), stops: &[(f32, Color)]) -> Option<windows::Win32::Graphics::Direct2D::ID2D1RadialGradientBrush> {
    radial(cx, center, radius, stops)
}

fn radial(cx: &RenderCx<'_>, center: (f32, f32), radius: (f32, f32), stops: &[(f32, Color)]) -> Option<windows::Win32::Graphics::Direct2D::ID2D1RadialGradientBrush> {
    let dc = cx.dc();
    let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: c.d2d() }).collect();
    unsafe {
        let rt: &ID2D1RenderTarget = dc;
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
        dc.CreateRadialGradientBrush(
            &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: D2D_POINT_2F { x: center.0, y: center.1 },
                gradientOriginOffset: D2D_POINT_2F::default(),
                radiusX: radius.0,
                radiusY: radius.1,
            },
            None,
            &coll,
        )
        .ok()
    }
}

/// Фон экрана .lcd: radial-gradient(120% 90% at 50% 40%, glass-2 0%, glass 70%), glass.
pub fn lcd_background(cx: &RenderCx<'_>, r: Rect, radius: f32) {
    fill_round(cx, r, radius, GLASS);
    if let Some(b) = radial(cx, (r.x + r.w * 0.5, r.y + r.h * 0.4), (r.w * 1.2, r.h * 0.9), &[(0.0, GLASS_2), (0.7, GLASS)]) {
        unsafe { cx.dc().FillRoundedRectangle(&rounded(r, radius), &b) };
    }
}

/// Стекло поверх содержимого экрана (.lcd::after): сканлайны, виньетка, блик сверху.
pub fn lcd_overlay(cx: &RenderCx<'_>, r: Rect, radius: f32) {
    if !push_round_layer(cx, r, radius, 1.0) {
        return;
    }
    let dc = cx.dc();
    unsafe {
        // repeating-linear-gradient(0deg, rgba(0,0,0,.28) 0 1px, transparent 1px 3px) — снизу вверх
        let dark = cx.brush(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.28 });
        let mut y = r.y + r.h;
        while y > r.y {
            dc.FillRectangle(&D2D_RECT_F { left: r.x, top: (y - 1.0).max(r.y), right: r.x + r.w, bottom: y }, dark);
            y -= 3.0;
        }
        // radial-gradient(140% 100% at 50% 50%, transparent 55%, rgba(0,0,0,.55) 100%)
        let clear = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
        if let Some(b) = radial(cx, (r.x + r.w / 2.0, r.y + r.h / 2.0), (r.w * 1.4 * 0.5 * std::f32::consts::SQRT_2, r.h * 0.5 * std::f32::consts::SQRT_2), &[(0.55, clear), (1.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.55 })]) {
            dc.FillRectangle(&r.d2d(), &b);
        }
        // linear-gradient(170deg, rgba(200,255,236,.07) 0%, transparent 28%)
        let rt: &ID2D1RenderTarget = dc;
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: Color::rgba(200, 255, 236, 18).d2d() },
            D2D1_GRADIENT_STOP { position: 0.28, color: Color::rgba(200, 255, 236, 0).d2d() },
        ];
        if let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
            // направление 170°: почти сверху вниз, чуть влево; длина — проекция прямоугольника
            let a = 170f32.to_radians();
            let (dx, dy) = (a.sin(), -a.cos());
            let half = (r.w * dx.abs() + r.h * dy.abs()) / 2.0;
            let (cxm, cym) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            if let Ok(b) = dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: D2D_POINT_2F { x: cxm - dx * half, y: cym - dy * half },
                    endPoint: D2D_POINT_2F { x: cxm + dx * half, y: cym + dy * half },
                },
                None,
                &coll,
            ) {
                dc.FillRectangle(&r.d2d(), &b);
            }
        }
    }
    pop_layer(cx);
}

/// Отразить прямоугольник внутри контейнера по горизонтали (раскладка dir="rtl").
pub fn mirror(container: Rect, r: Rect, rtl: bool) -> Rect {
    if rtl {
        Rect::new(container.x + container.w - (r.x - container.x) - r.w, r.y, r.w, r.h)
    } else {
        r
    }
}

/// Точка внутри прямоугольника.
pub fn inside(r: Rect, x: f32, y: f32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h
}
