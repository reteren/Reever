//! Подсказка в стиле скина (skin/tooltip.js + .tip): маленький ЭЛТ-экран под элементом
//! (или над ним, если снизу не помещается), появляется через 380 мс наведения, а если прошлая
//! погасла меньше 700 мс назад — сразу. Рисуется внутри окна, как и веб-подсказка.

use std::time::{Duration, Instant};

use super::anim::{Tween, E_OUT};
use super::{fill_round, lcd, push_round_layer, pop_layer, stroke_round, Glow, Scene, ACC_3, ACC_HOT, GLASS, GLASS_3};
use crate::native::{Color, Rect, RenderCx, TextStyle};

const DELAY: Duration = Duration::from_millis(380);
const WARM: Duration = Duration::from_millis(700);
const GAP: f32 = 8.0;
const MARGIN: f32 = 6.0;
const MAX_W: f32 = 280.0;

#[derive(Debug, Clone)]
pub struct Tooltip {
    target: Option<(u64, Rect, String)>,
    show_at: Option<Instant>,
    shown: bool,
    hidden_at: Option<Instant>,
    fade: Tween,
}

impl Default for Tooltip {
    fn default() -> Self {
        Self { target: None, show_at: None, shown: false, hidden_at: None, fade: Tween::new(0.0) }
    }
}

impl Tooltip {
    /// Указатель над элементом `key` (прямоугольник, текст) или ни над чем. true — перерисовать.
    pub fn hover(&mut self, over: Option<(u64, Rect, &str)>) -> bool {
        let now = Instant::now();
        let same = match (&self.target, &over) {
            (Some((a, _, _)), Some((b, _, _))) => a == b,
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        let redraw = self.hide();
        if let Some((key, r, text)) = over.filter(|o| !o.2.is_empty()) {
            let warm = self.hidden_at.is_some_and(|t| now.duration_since(t) < WARM);
            self.target = Some((key, r, text.to_string()));
            self.show_at = Some(if warm { now } else { now + DELAY });
        }
        redraw
    }

    /// Скрыть (нажатие, клавиша, колесо, уход указателя). true — была видна.
    pub fn hide(&mut self) -> bool {
        let was = self.shown;
        if was {
            self.hidden_at = Some(Instant::now());
        }
        self.target = None;
        self.show_at = None;
        self.shown = false;
        self.fade.jump(0.0);
        was
    }

    /// Пора показать — true, если только что показана.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.shown && self.target.is_some() && self.show_at.is_some_and(|t| now >= t) {
            self.shown = true;
            self.fade.set(now, 1.0, 0.18, E_OUT);
            return true;
        }
        false
    }

    pub fn wake_at(&self) -> Option<Instant> {
        if self.shown { None } else { self.show_at }
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.fade.running(now)
    }

    fn style() -> TextStyle {
        TextStyle { wrap: true, break_words: true, line_height: Some(1.15), ..lcd(15.0, 500, 0.07) }
    }

    /// Нарисовать поверх всего окна размером `win` (DIP).
    pub fn draw(&self, scene: &mut Scene, cx: &mut RenderCx<'_>, win: (f32, f32)) {
        let Some((_, target, text)) = self.target.as_ref().filter(|_| self.shown) else { return };
        let t = self.fade.value(cx.now);
        let Some(l) = scene.cache.get(cx.text(), &text.to_uppercase(), &Self::style(), MAX_W - 20.0 - 2.0) else { return };
        let (w, h) = (l.width.ceil() + 20.0 + 2.0, l.height + 11.0 + 2.0);
        let mut y = target.y + target.h + GAP;
        let mut below = true;
        if y + h > win.1 - MARGIN && target.y - GAP - h >= MARGIN {
            y = target.y - GAP - h;
            below = false;
        }
        let x = (target.x + target.w / 2.0 - w / 2.0).max(MARGIN).min(win.0 - w - MARGIN).round();
        // появление: opacity .14s, сдвиг 3px .18s
        let shift = (1.0 - t) * if below { -3.0 } else { 3.0 };
        let r = Rect::new(x, y.round() + shift, w, h);
        scene.shadow(cx, Rect::new(r.x, r.y + 6.0, r.w, r.h), 6.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.55 * t }, 18.0);
        scene.shadow(cx, r, 6.0, Color::rgba(39, 230, 170, (56.0 * t) as u8), 14.0);
        stroke_round(cx, Rect::new(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0), 7.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.7 * t }, 1.0, false);
        if push_round_layer(cx, r, 6.0, t) {
            fill_round(cx, r, 6.0, GLASS);
            fill_round(cx, r, 6.0, Color { a: 0.6, ..GLASS_3 });
            stroke_round(cx, r, 6.0, ACC_3, 1.0, false);
            scene.text(&l, r.x + 11.0, r.y + 7.0, ACC_HOT, Glow::G2);
            scene.flush(cx);
            // сканлайны .3
            let mut yy = r.y + r.h;
            while yy > r.y {
                cx.fill_rect(Rect::new(r.x, (yy - 1.0).max(r.y), r.w, 1.0), Color { r: 0.0, g: 0.0, b: 0.0, a: 0.3 });
                yy -= 3.0;
            }
            pop_layer(cx);
        }
    }
}
