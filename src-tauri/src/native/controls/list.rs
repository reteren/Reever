//! Прокручиваемый список строк разной высоты (заголовки разделов и пункты) и его полоса
//! прокрутки (::-webkit-scrollbar скина: 6px, бегунок acc-4, при наведении acc-2 со свечением).
//! Что рисовать в строке, решает окно; список знает высоты, прокрутку и где что под мышью.

use std::time::Instant;

use super::anim::{Bez, Tween};
use super::{fill_round, inside, Scene, ACC, ACC_2, ACC_4};
use crate::native::{Color, Rect, RenderCx};

/// Деление колеса — 100 px, как у Chromium; доезжает плавно за 150 мс.
const WHEEL_STEP: f32 = 100.0;
const SMOOTH: f32 = 0.15;
const BAR_W: f32 = 6.0;

#[derive(Debug, Clone)]
pub struct ListView {
    /// видимая область списка (прокручиваемый элемент целиком, с полосой)
    pub viewport: Rect,
    /// отступы содержимого сверху и снизу (padding)
    pub pad_top: f32,
    pub pad_bottom: f32,
    heights: Vec<f32>,
    tops: Vec<f32>,
    scroll: Tween,
    pub hover: Option<usize>,
    thumb_hover: bool,
    thumb_t: Tween,
    drag: Option<(f32, f32)>,
}

impl Default for ListView {
    fn default() -> Self {
        Self {
            viewport: Rect::new(0.0, 0.0, 0.0, 0.0),
            pad_top: 0.0,
            pad_bottom: 0.0,
            heights: Vec::new(),
            tops: Vec::new(),
            scroll: Tween::new(0.0),
            hover: None,
            thumb_hover: false,
            thumb_t: Tween::new(0.0),
            drag: None,
        }
    }
}

impl ListView {
    /// Новые строки; прокрутка — в начало.
    pub fn set_rows(&mut self, heights: Vec<f32>) {
        let mut y = self.pad_top;
        self.tops = heights
            .iter()
            .map(|h| {
                let t = y;
                y += h;
                t
            })
            .collect();
        self.heights = heights;
        self.scroll.jump(0.0);
        self.hover = None;
    }

    pub fn len(&self) -> usize {
        self.heights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    pub fn content_height(&self) -> f32 {
        self.tops.last().zip(self.heights.last()).map_or(self.pad_top, |(t, h)| t + h) + self.pad_bottom
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_height() - self.viewport.h).max(0.0)
    }

    pub fn scroll(&self, now: Instant) -> f32 {
        self.scroll.value(now).clamp(0.0, self.max_scroll())
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.scroll.running(now) || self.thumb_t.running(now)
    }

    /// Верх строки `i` в координатах содержимого.
    pub fn top(&self, i: usize) -> f32 {
        self.tops.get(i).copied().unwrap_or(0.0)
    }

    pub fn height(&self, i: usize) -> f32 {
        self.heights.get(i).copied().unwrap_or(0.0)
    }

    /// Строка на экране (с учётом прокрутки).
    pub fn row_rect(&self, i: usize, now: Instant) -> Rect {
        Rect::new(self.viewport.x, self.viewport.y + self.top(i) - self.scroll(now), self.viewport.w, self.height(i))
    }

    /// Строка под точкой окна.
    pub fn row_at(&self, x: f32, y: f32, now: Instant) -> Option<usize> {
        if !inside(self.viewport, x, y) || self.over_bar(x) {
            return None;
        }
        let cy = y - self.viewport.y + self.scroll(now);
        (0..self.heights.len()).find(|&i| cy >= self.tops[i] && cy < self.tops[i] + self.heights[i])
    }

    fn set_scroll(&mut self, to: f32, smooth: bool) {
        let to = to.clamp(0.0, self.max_scroll());
        if smooth {
            self.scroll.set(Instant::now(), to, SMOOTH, Bez(0.25, 0.1, 0.25, 1.0));
        } else {
            self.scroll.jump(to);
        }
    }

    /// scrollIntoView({ block: 'nearest' }): строка `i` целиком в видимой области.
    /// `cover_top` — сколько сверху закрыто липким заголовком раздела.
    pub fn ensure_visible(&mut self, i: usize, cover_top: f32) {
        let now = Instant::now();
        let s = self.scroll.target();
        let (t, h) = (self.top(i), self.height(i));
        let _ = now;
        if t - cover_top < s {
            self.set_scroll(t - cover_top, false);
        } else if t + h > s + self.viewport.h {
            self.set_scroll(t + h - self.viewport.h, false);
        }
    }

    /// Колесо: true — прокрутилось.
    pub fn wheel(&mut self, delta: f32) -> bool {
        let before = self.scroll.target();
        self.set_scroll(before - delta * WHEEL_STEP, true);
        (self.scroll.target() - before).abs() > 0.01
    }

    fn over_bar(&self, x: f32) -> bool {
        self.max_scroll() > 0.0 && x >= self.viewport.x + self.viewport.w - BAR_W
    }

    /// Бегунок полосы (если есть что прокручивать).
    pub fn thumb(&self, now: Instant) -> Option<Rect> {
        let max = self.max_scroll();
        if max <= 0.0 {
            return None;
        }
        let v = self.viewport;
        let h = (v.h * v.h / self.content_height()).max(18.0);
        let y = v.y + (v.h - h) * (self.scroll(now) / max);
        Some(Rect::new(v.x + v.w - BAR_W, y, BAR_W, h))
    }

    /// Мышь двигается: наведение на строку и бегунок, перетаскивание бегунка. true — перерисовать.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let now = Instant::now();
        if let Some((y0, s0)) = self.drag {
            let v = self.viewport;
            if let Some(t) = self.thumb(now) {
                let track = (v.h - t.h).max(1.0);
                self.set_scroll(s0 + (y - y0) / track * self.max_scroll(), false);
            }
            return true;
        }
        let over_thumb = self.thumb(now).is_some_and(|t| inside(t, x, y));
        let mut changed = false;
        if over_thumb != self.thumb_hover {
            self.thumb_hover = over_thumb;
            self.thumb_t.set(now, if over_thumb { 1.0 } else { 0.0 }, 0.12, Bez(0.25, 0.1, 0.25, 1.0));
            changed = true;
        }
        let hover = self.row_at(x, y, now);
        if hover != self.hover {
            self.hover = hover;
            changed = true;
        }
        changed
    }

    pub fn mouse_leave(&mut self) -> bool {
        let changed = self.hover.is_some() || self.thumb_hover;
        self.hover = None;
        if self.thumb_hover && self.drag.is_none() {
            self.thumb_hover = false;
            self.thumb_t.set(Instant::now(), 0.0, 0.12, Bez(0.25, 0.1, 0.25, 1.0));
        }
        changed
    }

    /// Нажатие на полосе: бегунок — тащить, дорожка — страница. true — нажатие забрано полосой.
    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        let now = Instant::now();
        if !inside(self.viewport, x, y) || !self.over_bar(x) {
            return false;
        }
        match self.thumb(now) {
            Some(t) if inside(t, x, y) => self.drag = Some((y, self.scroll(now))),
            Some(t) => {
                let page = self.viewport.h * 0.875;
                let s = self.scroll(now);
                self.set_scroll(if y < t.y { s - page } else { s + page }, true);
            }
            None => {}
        }
        true
    }

    pub fn mouse_up(&mut self) -> bool {
        self.drag.take().is_some()
    }

    /// Полоса прокрутки: дорожка прозрачная, бегунок acc-4 → acc-2 со свечением при наведении.
    pub fn draw_scrollbar(&self, scene: &mut Scene, cx: &mut RenderCx<'_>) {
        let Some(t) = self.thumb(cx.now) else { return };
        let h = if self.drag.is_some() { 1.0 } else { self.thumb_t.value(cx.now) };
        if h > 0.01 {
            scene.shadow(cx, t, 3.0, Color { a: h, ..ACC }, 6.0);
        }
        fill_round(cx, t, 3.0, super::mix(ACC_4, ACC_2, h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> ListView {
        let mut l = ListView { pad_top: 4.0, pad_bottom: 6.0, ..Default::default() };
        l.viewport = Rect::new(0.0, 100.0, 300.0, 100.0);
        l.set_rows(vec![28.0, 43.0, 43.0, 43.0, 43.0]);
        l
    }

    #[test]
    fn geometry_and_hit() {
        let l = list();
        assert_eq!(l.content_height(), 4.0 + 28.0 + 43.0 * 4.0 + 6.0);
        assert_eq!(l.row_at(10.0, 100.0 + 4.0 + 28.0 + 1.0, Instant::now()), Some(1));
        assert_eq!(l.row_at(10.0, 99.0, Instant::now()), None);
    }

    #[test]
    fn nearest_scrolling() {
        let mut l = list();
        l.ensure_visible(4, 0.0);
        let s = l.scroll(Instant::now());
        assert!((s - (l.top(4) + 43.0 - 100.0)).abs() < 0.01);
        l.ensure_visible(1, 0.0);
        assert!((l.scroll(Instant::now()) - l.top(1)).abs() < 0.01);
    }
}
