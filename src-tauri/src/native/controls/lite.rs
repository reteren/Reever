//! Лёгкий вид (frontend/lite.css, настройка `lite_ui`): тот же пульт — чёрное стекло и
//! люминофор, — но без хромовой отливки, шейдеров, частиц и маскота. Корпус — шлифованная
//! сталь со светлой верхней кромкой, экраны и кнопки — утопленные в металл гнёзда, линза —
//! стеклянная сфера в стальном ободе, жидкость в которой поднимается по «ривер».

use windows::Win32::Graphics::Direct2D::Common::{D2D1_GRADIENT_STOP, D2D_POINT_2F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1RenderTarget, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
};

use super::{fill_round, pop_layer, push_round_layer, radial_brush, rounded, stroke_round, Glow, Scene, ACC, ACC_3, ACC_5, ACC_HOT, GLASS, GLASS_2};
use crate::native::{Color, Rect, RenderCx};

const METAL: [(f32, Color); 4] = [
    (0.0, Color::rgb(0x74, 0x81, 0x7D)),
    (0.08, Color::rgb(0x5A, 0x66, 0x62)),
    (0.45, Color::rgb(0x4A, 0x55, 0x51)),
    (1.0, Color::rgb(0x3C, 0x46, 0x43)),
];
const EDGE: Color = Color::rgb(0x07, 0x0A, 0x0A);

/// Залить прямоугольник (со скруглением) линейным градиентом между двумя точками.
fn fill_linear(cx: &RenderCx<'_>, r: Rect, radius: f32, from: (f32, f32), to: (f32, f32), stops: &[(f32, Color)]) {
    let dc = cx.dc();
    let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: c.d2d() }).collect();
    unsafe {
        let rt: &ID2D1RenderTarget = dc;
        let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
        let Ok(b) = dc.CreateLinearGradientBrush(
            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: D2D_POINT_2F { x: from.0, y: from.1 }, endPoint: D2D_POINT_2F { x: to.0, y: to.1 } },
            None,
            &coll,
        ) else {
            return;
        };
        dc.FillRoundedRectangle(&rounded(r, radius), &b);
    }
}

/// Корпус: шлифованная сталь (--metal), фаска (--bevel), тёмный шов по краю.
pub fn metal_panel(cx: &mut RenderCx<'_>, r: Rect, radius: f32) {
    fill_linear(cx, r, radius, (r.x, r.y), (r.x, r.y + r.h), &METAL);
    if push_round_layer(cx, r, radius, 1.0) {
        // шлифовка: светлый штрих, пауза, тёмный штрих — шаг 3 px
        let mut y = r.y;
        while y < r.y + r.h {
            cx.fill_rect(Rect::new(r.x, y, r.w, 1.0), Color::rgba(255, 255, 255, 9));
            cx.fill_rect(Rect::new(r.x, y + 2.0, r.w, 1.0), Color::rgba(0, 0, 0, 13));
            y += 3.0;
        }
        // фаска: блик сверху, тень снизу, едва заметные бока
        cx.fill_rect(Rect::new(r.x, r.y + 1.0, r.w, 1.0), Color::rgba(235, 255, 248, 97));
        cx.fill_rect(Rect::new(r.x, r.y + r.h - 2.0, r.w, 1.0), Color::rgba(0, 0, 0, 140));
        cx.fill_rect(Rect::new(r.x + 1.0, r.y, 1.0, r.h), Color::rgba(235, 255, 248, 15));
        cx.fill_rect(Rect::new(r.x + r.w - 2.0, r.y, 1.0, r.h), Color::rgba(0, 0, 0, 77));
        pop_layer(cx);
    }
    stroke_round(cx, Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0), radius, EDGE, 1.0, false);
    stroke_round(cx, Rect::new(r.x + 2.5, r.y + 2.5, r.w - 5.0, r.h - 5.0), (radius - 2.0).max(0.0), Color::rgba(235, 255, 248, 8), 1.0, false);
}

/// Утопленное гнездо (--recess) вокруг экрана или кнопки: тёмный шов, блик на нижней губе
/// выреза, внутренняя тень сверху. Рисовать поверх содержимого гнезда.
pub fn recess(cx: &mut RenderCx<'_>, r: Rect, radius: f32, deep: bool) {
    // блик нижней губы — на 1 px ниже шва
    stroke_round(cx, Rect::new(r.x - 0.5, r.y + 0.5, r.w + 1.0, r.h + 1.0), radius + 1.0, Color::rgba(235, 255, 248, 26), 1.0, false);
    stroke_round(cx, Rect::new(r.x - 0.5, r.y - 0.5, r.w + 1.0, r.h + 1.0), radius + 0.5, Color::rgba(0, 0, 0, 191), 1.0, false);
    if push_round_layer(cx, r, radius, 1.0) {
        let depth = if deep { 10.0 } else { 8.0 };
        let dark = if deep { 0.95 } else { 0.8 };
        fill_linear(cx, Rect::new(r.x, r.y, r.w, depth), 0.0, (r.x, r.y), (r.x, r.y + depth), &[
            (0.0, Color { r: 0.0, g: 0.0, b: 0.0, a: dark * 0.7 }),
            (1.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
        ]);
        pop_layer(cx);
    }
}

/// Кнопка окна в лёгком виде (.lunka): гнездо со стеклом и люминофорным значком.
/// `hover` 0..1, `pressed` — нажата.
pub fn key(scene: &mut Scene, cx: &mut RenderCx<'_>, r: Rect, icon: &str, hover: f32, pressed: bool) {
    let radius = 9.0;
    fill_round(cx, r, radius, GLASS);
    if let Some(b) = radial_brush(cx, (r.x + r.w * 0.5, r.y + r.h * 0.3), (r.w * 1.2 * 0.5, r.h * 0.5), &[(0.0, GLASS_2), (0.75, GLASS)]) {
        unsafe { cx.dc().FillRoundedRectangle(&rounded(r, radius), &b) };
    }
    // изнутри: тонкая рамка люминофора, при наведении — ярче и с подсветкой
    let ring = Color { a: ACC_5.a + (ACC_3.a - ACC_5.a) * hover, ..ACC };
    stroke_round(cx, Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0), radius - 0.5, ring, 1.0, false);
    if hover > 0.01 && !pressed {
        if push_round_layer(cx, r, radius, hover) {
            if let Some(b) = radial_brush(cx, (r.x + r.w / 2.0, r.y + r.h / 2.0), (r.w * 0.6, r.h * 0.6), &[(0.0, Color::rgba(39, 230, 170, 56)), (1.0, Color::rgba(39, 230, 170, 0))]) {
                unsafe { cx.dc().FillRectangle(&r.d2d(), &b) };
            }
            pop_layer(cx);
        }
    }
    let color = if pressed { ACC } else { super::mix(ACC_3, ACC_HOT, hover) };
    let dy = if pressed { 1.0 } else { 0.0 };
    let s = 17.0;
    scene.icon(icon, Rect::new(r.x + (r.w - s) / 2.0, r.y + (r.h - s) / 2.0 + dy, s, s), 2.0, color, false, Glow::G1);
    scene.flush(cx);
    recess(cx, r, radius, pressed);
}

/// Состояние сферы.
#[derive(Debug, Clone, Copy)]
pub struct Orb {
    /// уровень жидкости 0 (пусто) … 1 (до краёв)
    pub fill: f32,
    /// громкость голоса 0..1 — подкачивает ореол
    pub level: f32,
    /// ореол «активна» 0..1
    pub glow: f32,
    pub hover: bool,
    pub paused: bool,
    /// вспышка «команда принята» 0..1
    pub flash: f32,
}

/// Сфера лёгкого вида (#orbHit): обод `ring` px, внутри — тёмное стекло и жидкость.
pub fn orb(cx: &mut RenderCx<'_>, center: (f32, f32), size: f32, ring: f32, o: &Orb) {
    let (x, y) = center;
    let rad = size / 2.0;
    let r = Rect::new(x - rad, y - rad, size, size);
    let dim = if o.paused { 0.6 } else { 1.0 };
    // ореол: 0 0 (18 + 26·lv)px (2 + 4·lv)px rgba(acc, .55); наведение — 16px .3
    let halo = (o.glow * 0.55).max(if o.hover { 0.3 } else { 0.0 });
    if halo > 0.01 && !o.paused {
        let blur = if o.glow > 0.01 { 18.0 + 26.0 * o.level } else { 16.0 };
        let spread = if o.glow > 0.01 { 2.0 + 4.0 * o.level } else { 0.0 };
        if let Some(b) = radial_brush(cx, center, (rad + spread + blur, rad + spread + blur), &[
            (0.0, Color { a: halo, ..ACC }),
            ((rad / (rad + spread + blur)).min(0.99), Color { a: halo, ..ACC }),
            (1.0, Color { a: 0.0, ..ACC }),
        ]) {
            unsafe {
                cx.dc().FillEllipse(&D2D1_ELLIPSE { point: D2D_POINT_2F { x, y }, radiusX: rad + spread + blur, radiusY: rad + spread + blur }, &b);
            }
        }
    }
    // обод: сталь, светлая сверху-слева (conic-gradient в CSS)
    fill_linear(cx, r, rad, (x - rad * 0.7, y - rad * 0.7), (x + rad * 0.7, y + rad * 0.7), &[
        (0.0, Color::rgb(0xC9, 0xD6, 0xD2)),
        (0.3, Color::rgb(0x7F, 0x8D, 0x89)),
        (0.55, Color::rgb(0x2E, 0x38, 0x36)),
        (0.8, Color::rgb(0x9A, 0xA8, 0xA4)),
        (1.0, Color::rgb(0x26, 0x2F, 0x2D)),
    ]);
    stroke_round(cx, Rect::new(r.x - 0.5, r.y - 0.5, r.w + 1.0, r.h + 1.0), rad + 0.5, EDGE, 1.0, false);
    let inner = Rect::new(r.x + ring, r.y + ring, size - 2.0 * ring, size - 2.0 * ring);
    let ir = inner.w / 2.0;
    if !push_round_layer(cx, inner, ir, dim) {
        return;
    }
    if let Some(b) = radial_brush(cx, (x, y + ir * 0.1), (ir * 1.44, ir * 1.44), &[(0.0, Color::rgb(0x06, 0x19, 0x0F)), (0.72, Color::rgb(0x02, 0x0A, 0x08))]) {
        unsafe { cx.dc().FillRectangle(&inner.d2d(), &b) };
    }
    // жидкость: поверхность — светлая кромка, ниже толща; уровень — сдвигом
    if o.fill > 0.001 {
        let top = inner.y + inner.h * (1.0 - o.fill.clamp(0.0, 1.0));
        let liquid = Rect::new(inner.x, top, inner.w, inner.h);
        let h = inner.h;
        let tint = if o.paused { 0.0 } else { 1.0 };
        let stops = [
            (0.0, Color { a: 1.0 * tint, ..ACC_HOT }),
            (2.0 / h, Color { a: 1.0 * tint, ..ACC_HOT }),
            (3.0 / h, Color::rgba(39, 230, 170, (230.0 * tint) as u8)),
            (0.3, Color::rgba(39, 230, 170, (153.0 * tint) as u8)),
            (1.0, Color::rgba(4, 70, 50, (242.0 * tint) as u8)),
        ];
        fill_linear(cx, liquid, 0.0, (x, top), (x, top + h), &stops);
    }
    // блик стекла: radial at 34% 26%
    if let Some(b) = radial_brush(cx, (inner.x + inner.w * 0.34, inner.y + inner.h * 0.26), (ir * 0.55, ir * 0.55), &[(0.0, Color::rgba(200, 255, 236, 36)), (1.0, Color::rgba(200, 255, 236, 0))]) {
        unsafe { cx.dc().FillRectangle(&inner.d2d(), &b) };
    }
    // тень снизу внутри: inset 0 -16px 30px rgba(0,0,0,.55)
    fill_linear(cx, Rect::new(inner.x, inner.y + inner.h - 30.0, inner.w, 30.0), 0.0, (x, inner.y + inner.h - 30.0), (x, inner.y + inner.h), &[
        (0.0, Color::rgba(0, 0, 0, 0)),
        (1.0, Color::rgba(0, 0, 0, 120)),
    ]);
    if o.flash > 0.01 {
        cx.fill_rect(inner, Color { a: 0.5 * o.flash, ..ACC_HOT });
    }
    pop_layer(cx);
    stroke_round(cx, Rect::new(inner.x + 0.5, inner.y + 0.5, inner.w - 1.0, inner.h - 1.0), ir, Color::rgba(0, 0, 0, 204), 1.0, false);
}

/// Шкала вокруг сферы (#skin::after): насечки через 10°, кольцо `size` и толщина 6 px.
pub fn scale_ring(cx: &mut RenderCx<'_>, center: (f32, f32), size: f32) {
    let r1 = size / 2.0;
    let r0 = r1 - 6.0;
    let c = Color::rgba(39, 230, 170, 97);
    for i in 0..36 {
        let a = (i as f32 * 10.0 - 1.0 + 1.0).to_radians() - std::f32::consts::FRAC_PI_2;
        let (s, co) = a.sin_cos();
        let p0 = D2D_POINT_2F { x: center.0 + co * r0, y: center.1 + s * r0 };
        let p1 = D2D_POINT_2F { x: center.0 + co * r1, y: center.1 + s * r1 };
        // дуга 2° на радиусе ~104 px — около 3,6 px шириной
        let w = (2f32.to_radians() * (r0 + r1) / 2.0).max(1.0);
        unsafe { cx.dc().DrawLine(p0, p1, cx.brush(c), w, None) };
    }
}
