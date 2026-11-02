//! Кнопки скина: текстовая (button.sm / .default / .danger), «фишка» переключателя (.chip)
//! и стеклянный купол-«лунка» (.lunka) — кнопка окна с картинками off/on.

use std::time::Instant;

use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D_POINT_2F};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Shadow, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_VECTOR4,
};

use super::anim::{Tween, E_OUT, EASE};
use super::{
    as_bytes, fill_round, inside, lcd, mix, stroke_round, with_alpha, Glow, Scene, ACC, ACC_2, ACC_3, ACC_4, ACC_5,
    ACC_HOT, GLASS,
};
use crate::native::{Color, ImageId, Rect, RenderCx, TextStyle};

/// Общее состояние нажимаемого элемента: наведение, нажатие, фокус и переход наведения.
#[derive(Debug, Clone)]
pub struct Press {
    pub hover: bool,
    pub pressed: bool,
    pub focused: bool,
    /// фокус показан рамкой (:focus-visible) — после клавиатуры, не после щелчка
    pub focus_visible: bool,
    pub hover_t: Tween,
}

impl Default for Press {
    fn default() -> Self {
        Self { hover: false, pressed: false, focused: false, focus_visible: false, hover_t: Tween::new(0.0) }
    }
}

impl Press {
    /// Наведение изменилось — true, если нужна перерисовка.
    pub fn set_hover(&mut self, on: bool, dur: f32) -> bool {
        if self.hover == on {
            return false;
        }
        self.hover = on;
        self.hover_t.set(Instant::now(), if on { 1.0 } else { 0.0 }, dur, EASE);
        true
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.hover_t.running(now)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// button.sm
    Plain,
    /// button.default — главное действие
    Default,
    /// button.danger — опасное действие, рамка штрихами
    Danger,
}

/// Текстовая кнопка: высота, шрифт и отступ — как у button.sm, крупнее — через поля.
#[derive(Debug, Clone)]
pub struct Button {
    pub rect: Rect,
    pub label: String,
    pub kind: ButtonKind,
    pub press: Press,
    pub font_size: f32,
    pub letter_em: f32,
    pub radius: f32,
    /// рамка фокуса: (толщина, отступ, цвет) — у .answers button 2px dashed acc-hot, отступ 4
    pub focus_ring: (f32, f32, Color),
}

impl Button {
    pub fn new(label: &str, kind: ButtonKind) -> Self {
        Self {
            rect: Rect::new(0.0, 0.0, 0.0, 28.0),
            label: label.to_string(),
            kind,
            press: Press::default(),
            font_size: 15.0,
            letter_em: 0.07,
            radius: 5.0,
            focus_ring: (1.0, 2.0, ACC_2),
        }
    }

    fn style(&self) -> TextStyle {
        lcd(self.font_size, 500, self.letter_em)
    }

    /// Ширина по тексту (padding 0 12px, рамка 1px).
    pub fn natural_width(&self, scene: &mut Scene, cx: &RenderCx<'_>) -> f32 {
        let l = scene.cache.get(cx.text(), &self.label.to_uppercase(), &self.style(), 10_000.0);
        l.map_or(40.0, |l| l.width) + 24.0 + 2.0
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        inside(self.rect, x, y)
    }

    pub fn draw(&self, scene: &mut Scene, cx: &mut RenderCx<'_>) {
        let now = cx.now;
        let h = self.press.hover_t.value(now);
        let active = self.press.pressed && self.press.hover;
        let mut r = self.rect;
        if active {
            r.y += 1.0; // translateY(1px)
        }
        let (bg0, border0, text0) = match self.kind {
            ButtonKind::Default => (ACC_4, ACC_2, ACC_HOT),
            _ => (ACC_5, ACC_4, ACC),
        };
        let (mut bg, mut border, mut text) = (mix(bg0, ACC_4, h), mix(border0, ACC_3, h), mix(text0, ACC_HOT, h));
        let mut glow = if h > 0.5 { Glow::G3 } else { Glow::G2 };
        if active {
            bg = ACC;
            text = GLASS;
            border = mix(border0, ACC_3, h);
            glow = Glow::None;
        }
        if h > 0.01 && !active {
            scene.shadow(cx, r, self.radius, with_alpha(Color::rgba(39, 230, 170, 46), h), 14.0);
        }
        fill_round(cx, r, self.radius, bg);
        stroke_round(cx, r, self.radius, border, 1.0, self.kind == ButtonKind::Danger);
        let label = self.label.to_uppercase();
        if let Some(l) = scene.cache.get(cx.text(), &label, &self.style(), 10_000.0) {
            let x = r.x + (r.w - l.width) / 2.0;
            let y = r.y + (r.h - l.height) / 2.0;
            scene.text(&l, x, y, text, glow);
        }
        if self.press.focused && self.press.focus_visible {
            let (w, off, c) = self.focus_ring;
            let ring = Rect::new(r.x - off - w, r.y - off - w, r.w + 2.0 * (off + w), r.h + 2.0 * (off + w));
            stroke_round(cx, ring, self.radius + off + w, c, w, true);
        }
    }
}

/// «Фишка» переключателя области поиска (.chip в окне выбора).
#[derive(Debug, Clone)]
pub struct Chip {
    pub rect: Rect,
    pub label: String,
    pub on: bool,
    pub press: Press,
}

impl Chip {
    pub fn new(label: &str) -> Self {
        Self { rect: Rect::new(0.0, 0.0, 0.0, 0.0), label: label.to_string(), on: false, press: Press::default() }
    }

    /// .chip: font 500 14px/1 var(--f-lcd) — без осей ELSH/ELGR (у фишки они не заданы).
    fn style() -> TextStyle {
        TextStyle { axes: Vec::new(), ..lcd(14.0, 500, 0.06) }
    }

    /// Размер по тексту при padding 5px 7px 4px и рамке 1px.
    pub fn natural_size(&self, scene: &mut Scene, cx: &RenderCx<'_>) -> (f32, f32) {
        let l = scene.cache.get(cx.text(), &self.label.to_uppercase(), &Self::style(), 10_000.0);
        let (w, h) = l.map_or((30.0, 14.0), |l| (l.width, l.height));
        (w + 14.0 + 2.0, h + 9.0 + 2.0)
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        inside(self.rect, x, y)
    }

    pub fn draw(&self, scene: &mut Scene, cx: &mut RenderCx<'_>) {
        let h = self.press.hover_t.value(cx.now);
        let r = self.rect;
        let (text, glow) = if self.on {
            scene.shadow(cx, r, 4.0, Color::rgba(39, 230, 170, 89), 10.0);
            fill_round(cx, r, 4.0, ACC);
            (GLASS, Glow::None)
        } else {
            if h > 0.01 {
                stroke_round(cx, r, 4.0, with_alpha(ACC_5, h), 1.0, false);
            }
            (mix(ACC_3, ACC_2, h), Glow::G2)
        };
        if let Some(l) = scene.cache.get(cx.text(), &self.label.to_uppercase(), &Self::style(), 10_000.0) {
            scene.text(&l, r.x + (r.w - l.width) / 2.0, r.y + 5.0 + 1.0, text, glow);
        }
        if self.press.focused && self.press.focus_visible {
            stroke_round(cx, Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0), 7.0, ACC_2, 1.0, true);
        }
    }
}

/// Стеклянный купол кнопки окна (.lunka): тёмный купол, при наведении проявляется горящий,
/// при нажатии продавливается. Центр — `center`, спрайт — `box_size` (44), купол — `dome` (--d).
#[derive(Debug, Clone)]
pub struct Lunka {
    pub center: (f32, f32),
    pub box_size: f32,
    pub dome: f32,
    pub icon: &'static str,
    pub off: ImageId,
    pub on: ImageId,
    pub press: Press,
    pub tip: String,
}

impl Lunka {
    pub fn hit(&self, x: f32, y: f32) -> bool {
        let (dx, dy) = (x - self.center.0, y - self.center.1);
        dx * dx + dy * dy <= (self.box_size / 2.0) * (self.box_size / 2.0)
    }

    pub fn rect(&self) -> Rect {
        let b = self.box_size;
        Rect::new(self.center.0 - b / 2.0, self.center.1 - b / 2.0, b, b)
    }

    pub fn set_hover(&mut self, on: bool) -> bool {
        // transition: opacity .22s var(--e-out) у горящего купола
        if self.press.hover == on {
            return false;
        }
        self.press.hover = on;
        self.press.hover_t.set(Instant::now(), if on { 1.0 } else { 0.0 }, 0.22, E_OUT);
        true
    }

    pub fn draw(&self, scene: &mut Scene, cx: &mut RenderCx<'_>) {
        let h = self.press.hover_t.value(cx.now);
        let active = self.press.pressed && self.press.hover;
        let dc = cx.dc().clone();
        let base = {
            let mut m = Matrix3x2::default();
            unsafe { dc.GetTransform(&mut m) };
            m
        };
        let (cxp, cyp) = self.center;
        if active {
            // translateY(1.5px) scale(.9)
            let m = Matrix3x2 { M11: 0.9, M12: 0.0, M21: 0.0, M22: 0.9, M31: cxp * 0.1, M32: cyp * 0.1 + 1.5 };
            unsafe { dc.SetTransform(&(m * base)) };
        }
        let r = self.rect();
        cx.draw_image(self.off, r, 1.0);
        if h > 0.001 {
            // горящий купол с ореолом: drop-shadow(0 0 3px acc .85) drop-shadow(0 0 10px acc .5)
            let pw = (r.w * cx.scale).round() as u32;
            if let Some(bmp) = cx.bitmap(self.on, pw, pw) {
                unsafe {
                    for (sigma, a) in [(5.0f32, 0.5f32), (1.5, 0.85)] {
                        if let Ok(e) = dc.CreateEffect(&CLSID_D2D1Shadow) {
                            e.SetInput(0, &bmp, BOOL(1));
                            let s = sigma * cx.scale;
                            let _ = e.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, as_bytes(&s));
                            let color = [ACC.r, ACC.g, ACC.b, a * h];
                            let _ = e.SetValue(1, D2D1_PROPERTY_TYPE_VECTOR4, as_bytes(&color));
                            if let Ok(img) = e.GetOutput() {
                                // битмап 96 DPI: в пикселях — DPI контекста на время 96
                                let dpi = 96.0 * cx.scale;
                                dc.SetDpi(96.0, 96.0);
                                let t = Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 };
                                let cur = {
                                    let mut m = Matrix3x2::default();
                                    dc.GetTransform(&mut m);
                                    m
                                };
                                let sc = Matrix3x2 { M11: 1.0 / cx.scale, M12: 0.0, M21: 0.0, M22: 1.0 / cx.scale, M31: r.x, M32: r.y };
                                let px = Matrix3x2 { M11: cx.scale, M12: 0.0, M21: 0.0, M22: cx.scale, M31: 0.0, M32: 0.0 };
                                dc.SetTransform(&(t * sc * cur * px));
                                dc.DrawImage(&img, Some(&D2D_POINT_2F::default()), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                                dc.SetTransform(&cur);
                                dc.SetDpi(dpi, dpi);
                            }
                        }
                    }
                }
            }
            cx.draw_image(self.on, r, h);
        }
        // значок: rgba(acc, .78) со свечением; при наведении — тёмный #032619 с бликом снизу
        let s = self.dome * 0.52;
        let ir = Rect::new(cxp - s / 2.0, cyp - s / 2.0, s, s);
        let idle = Color::rgba(39, 230, 170, 199);
        let dark = Color::rgb(0x03, 0x26, 0x19);
        if h > 0.5 {
            scene.icon(self.icon, Rect::new(ir.x, ir.y + 1.0, ir.w, ir.h), 2.6, Color::rgba(200, 255, 236, 140), false, Glow::None);
            scene.icon(self.icon, ir, 2.6, mix(idle, dark, h), false, Glow::None);
        } else {
            scene.icon(self.icon, ir, 2.6, mix(idle, dark, h), false, Glow::G2);
        }
        scene.flush(cx);
        unsafe { dc.SetTransform(&base) };
        if self.press.focused && self.press.focus_visible {
            stroke_round(cx, r, r.w / 2.0 + 2.0, ACC_2, 1.0, true);
        }
    }
}
