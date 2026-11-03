//! Компактный режим (#shade в index.html, setShade в shell.js): окно сжимается в хромовую
//! полоску 640×170 поверх всех окон — своя рамка (shade_color/shade_normal) с тем же шейдером
//! света, маленькая линза, частицы без нитей, бегущая строка и две лунки: развернуть и в трей.

use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_ALIASED, D2D1_INTERPOLATION_MODE_LINEAR};

use super::frame::{FrameLight, FramePixels, FrameShader};
use super::fx::Fx;
use crate::mascot_native::lens::{Lens, LensParams};
use crate::native::controls::button::{Lunka, Press};
use crate::native::controls::{lcd, lcd_background, lcd_overlay, Glow, Scene, ACC, ACC_HOT};
use crate::native::{self, GpuCx, ImageId, Images, Rect, RenderCx, TextLayout, TextStyle};

/// Размер полоски: полный вид 640×170, лёгкий — 520×64 (lite.css #shade).
pub fn shade_w() -> f32 {
    if lite() { 520.0 } else { 640.0 }
}
pub fn shade_h() -> f32 {
    if lite() { 64.0 } else { 170.0 }
}

fn lite() -> bool {
    super::view::lite()
}

/// Лёгкий вид: мини-сфера 44 (10, 10), строка между ней и клавишами, клавиши 34×34.
const LITE_ORB: (f32, f32, f32) = (32.0, 32.0, 22.0);
const LITE_TICKER: Rect = Rect { x: 62.0, y: 10.0, w: 362.0, h: 44.0 };
/// центр и радиус сферы, рамка линзы (left 79, top 52, 66×66)
const ORB: (f32, f32, f32) = (112.0, 85.0, 30.0);
const CORE: Rect = Rect { x: 79.0, y: 52.0, w: 66.0, h: 66.0 };
const TICKER: Rect = Rect { x: 168.0, y: 63.0, w: 306.0, h: 44.0 };
/// скорость бегущей строки, DIP/с (как --dur: (over + 24) / 50 с)
const MARQUEE: f32 = 50.0;

/// Что нажали в полоске.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShadeHit {
    Expand,
    Tray,
    Orb,
}

pub struct Shade {
    color: ImageId,
    normal: ImageId,
    frame: Option<FrameShader>,
    frame_px: Option<FramePixels>,
    frame_failed: bool,
    lens: Option<Lens>,
    lens_failed: bool,
    pub fx: Fx,
    pub lunki: [Lunka; 2],
    pub orb_hover: bool,
    /// текст строки и с какого момента он бежит
    text: String,
    marquee_from: Instant,
    overflow: f32,
    last: Instant,
}

impl Shade {
    pub fn new(images: &mut Images, app: &crate::app::AppHandle, btn: [(ImageId, ImageId); 2], tips: [String; 2]) -> Self {
        let l = |p: &str| crate::dialogs_native::loader(app, p);
        let color = images.add_loader(l("skin/shade_color.webp"));
        let normal = images.add_loader(l("skin/shade_normal.webp"));
        let [t0, t1] = tips;
        let lunka = |c: (f32, f32), icon: &'static str, (off, on): (ImageId, ImageId), tip: String| Lunka {
            center: c,
            box_size: 36.0,
            dome: 23.0,
            icon,
            off,
            on,
            press: Press::default(),
            tip,
        };
        Self {
            color,
            normal,
            frame: None,
            frame_px: None,
            frame_failed: false,
            lens: None,
            lens_failed: false,
            fx: Fx::new((ORB.0, ORB.1), ORB.2),
            lunki: if lite() {
                let mut l = [lunka((449.0, 32.0), "expand", btn[0], t0), lunka((491.0, 32.0), "tray", btn[1], t1)];
                for k in l.iter_mut() {
                    k.box_size = 34.0;
                }
                l
            } else {
                [lunka((510.0, 85.0), "expand", btn[0], t0), lunka((548.0, 85.0), "tray", btn[1], t1)]
            },
            orb_hover: false,
            text: String::new(),
            marquee_from: Instant::now(),
            overflow: 0.0,
            last: Instant::now(),
        }
    }

    /// Форма окна по альфе рамки полоски (прямоугольники в координатах полоски).
    /// None — лёгкий вид: форма — скруглённый прямоугольник (`view::rounded_rects`).
    pub fn shape_source(&self) -> Option<(ImageId, u32, u32)> {
        (!lite()).then_some((self.color, shade_w() as u32, shade_h() as u32))
    }

    /// Устройство пересоздано или масштаб сменился — текстуры заново.
    pub fn reset_gpu(&mut self) {
        self.frame = None;
        self.frame_px = None;
        self.frame_failed = false;
        self.lens = None;
    }

    fn light(&self, orb_light: f32) -> FrameLight {
        let l = |i: usize| [self.lunki[i].center.0, self.lunki[i].center.1, 0.0];
        FrameLight {
            mouse: (0.0, 0.0),
            mouse_on: 0.0,
            orb: (ORB.0, ORB.1),
            orb_light,
            sweep_x: -1.0,
            sweep_i: 0.0,
            reveal: 1800.0,
            lunki: [l(0), l(1), [-999.0, -999.0, 0.0]],
        }
    }

    fn frame_size(k: f32, scale: f32) -> (u32, u32) {
        let d = scale.min(1.5);
        ((shade_w() * k * d).round().max(2.0) as u32, (shade_h() * k * d).round().max(2.0) as u32)
    }

    pub fn prepare(&mut self, gpu: &GpuCx<'_>, k: f32, lens: &LensParams, orb_light: f32) {
        if lite() {
            return;
        }
        if !self.frame_failed {
            if let Some(px) = self.frame_px.take() {
                match FrameShader::new(gpu, &px) {
                    Ok(Some(f)) => self.frame = Some(f),
                    Ok(None) => self.frame_failed = true,
                    Err(e) => {
                        native::log(&format!("main: shade frame: {e}"));
                        self.frame_failed = true;
                    }
                }
            }
            let light = self.light(orb_light);
            if let Some(f) = &mut self.frame {
                if f.fits(gpu, Self::frame_size(k, gpu.scale)) {
                    f.render(gpu, (shade_w(), shade_h()), &light);
                }
            }
        }
        if !self.lens_failed {
            let size = (CORE.w * k * gpu.scale).round().max(2.0) as u32;
            if self.lens.as_ref().map_or(true, |l| !l.fits(gpu, size)) {
                match Lens::new(gpu, size) {
                    Ok(Some(l)) => self.lens = Some(l),
                    Ok(None) => self.lens_failed = true,
                    Err(e) => {
                        native::log(&format!("main: shade lens: {e}"));
                        self.lens_failed = true;
                    }
                }
            }
            if let Some(l) = &self.lens {
                l.render(gpu, lens);
            }
        }
    }

    /// Строка полоски (syncShadeText): что слышно сейчас, иначе последняя команда, иначе состояние.
    pub fn set_text(&mut self, text: &str) {
        let t = text.to_uppercase();
        if t != self.text {
            self.text = t;
            self.marquee_from = Instant::now();
        }
    }

    /// Строка не влезает — бежит (кадры нужны, но не чаще 30 в секунду).
    pub fn wake_at(&self) -> Option<Instant> {
        (self.overflow > 4.0).then(|| self.last + Duration::from_millis(33))
    }

    pub fn render(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, k: f32, base_scale: f32, orb: &crate::native::controls::lite::Orb) {
        self.last = cx.now;
        if lite() {
            self.render_lite(scene, cx, orb);
            return;
        }
        // картинки для шейдера рамки — один раз на размер
        if !self.frame_failed && self.frame_px.is_none() && self.frame.is_none() {
            let size = Self::frame_size(k, base_scale);
            let images = cx.images();
            match (images.pixels(self.color, size.0, size.1), images.pixels(self.normal, size.0, size.1)) {
                (Ok(color), Ok(normal)) => self.frame_px = Some(FramePixels { w: size.0, h: size.1, color, normal }),
                _ => self.frame_failed = true,
            }
        }
        // линза под рамкой
        if let Some(l) = &self.lens {
            unsafe {
                cx.dc().DrawBitmap(l.bitmap(), Some(&CORE.d2d()), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        }
        // бегущая строка
        self.draw_ticker(scene, cx, TICKER, 24.0);
        // рамка
        let r = Rect::new(0.0, 0.0, shade_w(), shade_h());
        match &self.frame {
            Some(f) => unsafe {
                cx.dc().DrawBitmap(f.bitmap(), Some(&r.d2d()), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            },
            None => cx.draw_image(self.color, r, 1.0),
        }
        for l in self.lunki.iter() {
            l.draw(scene, cx);
        }
        scene.flush(cx);
        self.fx.draw(cx);
    }

    /// Лёгкая полоска: сталь, мини-сфера, строка, две клавиши.
    fn render_lite(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, orb: &crate::native::controls::lite::Orb) {
        use crate::native::controls::lite;
        lite::metal_panel(cx, Rect::new(0.0, 0.0, shade_w(), shade_h()), 16.0);
        lite::orb(cx, (LITE_ORB.0, LITE_ORB.1), LITE_ORB.2 * 2.0, 3.0, orb);
        self.draw_ticker(scene, cx, LITE_TICKER, 22.0);
        lite::recess(cx, LITE_TICKER, 8.0, false);
        for l in self.lunki.iter() {
            let r = Rect::new(l.center.0 - 17.0, l.center.1 - 17.0, 34.0, 34.0);
            lite::key(scene, cx, r, l.icon, l.press.hover_t.value(cx.now), l.press.pressed && l.press.hover);
        }
        scene.flush(cx);
    }

    /// Табло строки (#shadeTicker): значок, текст (бежит, если не влезает), стекло.
    fn draw_ticker(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, t: Rect, size: f32) {
        lcd_background(cx, t, 8.0);
        scene.icon("play", Rect::new(t.x + 12.0, t.y + 16.0, 12.0, 12.0), 0.0, ACC, true, Glow::G2);
        let inner = Rect::new(t.x + 12.0 + 12.0 + 8.0, t.y, t.w - 12.0 - 12.0 - 8.0 - 12.0, t.h);
        let st: TextStyle = lcd(size, 500, 0.05);
        let text = if self.text.is_empty() { "REEVER".to_string() } else { self.text.clone() };
        if let Some(l) = scene.cache.get(cx.text(), &text, &st, 100_000.0) {
            let l: Rc<TextLayout> = l;
            self.overflow = l.width - inner.w;
            // арабская строка стоит у правого края и уезжает вправо
            let mut x = if l.rtl { inner.x + inner.w - l.width } else { inner.x };
            if self.overflow > 4.0 {
                // marquee: уезжает на ширину переполнения + 24, по кругу
                let dist = self.overflow + 24.0 + 30.0;
                let dur = (dist / MARQUEE).max(4.0);
                let t = cx.now.duration_since(self.marquee_from).as_secs_f32() % dur;
                x += if l.rtl { dist * t / dur } else { -dist * t / dur };
            }
            scene.flush(cx);
            scene.set_clip(Some(inner));
            unsafe { cx.dc().PushAxisAlignedClip(&inner.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
            scene.text(&l, x, inner.y + (inner.h - l.height) / 2.0, ACC_HOT, Glow::G3);
            scene.flush(cx);
            scene.set_clip(None);
            unsafe { cx.dc().PopAxisAlignedClip() };
        }
        lcd_overlay(cx, t, 8.0);
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<ShadeHit> {
        if self.lunki[0].hit(x, y) {
            return Some(ShadeHit::Expand);
        }
        if self.lunki[1].hit(x, y) {
            return Some(ShadeHit::Tray);
        }
        let o = if lite() { LITE_ORB } else { ORB };
        let (dx, dy) = (x - o.0, y - o.1);
        (dx * dx + dy * dy <= o.2 * o.2).then_some(ShadeHit::Orb)
    }
}
