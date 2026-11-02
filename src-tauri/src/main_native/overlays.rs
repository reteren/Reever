//! Окна поверх главного (.overlay > .modal в index.html): калибровка действия, подстройка под
//! голос, выбор программы Windows. Модальны: пока окно открыто, мышь и клавиатура — его.

use std::time::Instant;

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};

use crate::native::controls::button::{Button, ButtonKind};
use crate::native::controls::text_input::TextInput;
use crate::native::controls::{
    fill_round, inside, lcd, lcd_background, lcd_overlay, stroke_round, ui, Glow, Scene, ACC, ACC_3, ACC_HOT, GLASS,
};
use crate::native::{Align, Color, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx, TextStyle};
use crate::ui_text::t;

/// Слов для подстройки под голос (VOICE_GOAL в app.js); «Готово» — от пяти.
pub const VOICE_GOAL: u32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// калибровка действия: id, подпись, сколько фраз и последняя
    Calibration { id: String, label: String, count: usize, last: Option<String> },
    /// подстройка: слов услышано, итог (после «Готово»), идёт ли запись
    Voice { words: u32, result: String, recording: bool, wake: String },
    /// программы Windows (имя, AUMID); None — ещё грузятся
    Apps { list: Option<Vec<(String, String)>> },
}

/// Что окно просит.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayOut {
    /// закрыть (калибровку — отменить её, запись — бросить)
    Close,
    VoiceDone,
    /// выбрана программа: имя, AUMID
    App(String, String),
}

pub struct Overlay {
    pub kind: Kind,
    rect: Rect,
    ok: Button,
    cancel: Button,
    search: TextInput,
    list_rect: Rect,
    rows: Vec<(Rect, usize)>,
    hover: Option<usize>,
    scroll: f32,
    max_scroll: f32,
    pressed: Option<u8>,
    locale: String,
    t0: Instant,
}

impl Overlay {
    pub fn new(kind: Kind, locale: &str, hwnd: isize) -> Self {
        let tr = |k: &str| t(locale, k, &[]);
        let (ok, cancel) = match &kind {
            Kind::Calibration { .. } => (Button::new(&tr("common.done"), ButtonKind::Default), Button::new("", ButtonKind::Plain)),
            Kind::Voice { .. } => (Button::new(&tr("common.done"), ButtonKind::Default), Button::new(&tr("common.cancel"), ButtonKind::Plain)),
            Kind::Apps { .. } => (Button::new("", ButtonKind::Default), Button::new(&tr("common.close"), ButtonKind::Plain)),
        };
        let mut search = TextInput::default();
        search.style = ui(13.0, 400, 1.0);
        search.placeholder = tr("appPicker.search");
        search.hwnd = hwnd;
        search.set_focus(matches!(kind, Kind::Apps { .. }));
        Self {
            kind,
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            ok,
            cancel,
            search,
            list_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            rows: Vec::new(),
            hover: None,
            scroll: 0.0,
            max_scroll: 0.0,
            pressed: None,
            locale: locale.to_string(),
            t0: Instant::now(),
        }
    }

    fn t(&self, key: &str, p: &[(&str, &str)]) -> String {
        t(&self.locale, key, p)
    }

    /// «Готово» доступно (подстройка — от пяти слов, пока идёт запись).
    fn ok_enabled(&self) -> bool {
        match &self.kind {
            Kind::Voice { words, recording, .. } => *recording && *words >= 5,
            Kind::Calibration { .. } => true,
            Kind::Apps { .. } => false,
        }
    }

    /// Шарик калибровки мигает (animation: blink 1.2s) — кадры нужны всё время.
    pub fn animating(&self) -> bool {
        matches!(self.kind, Kind::Calibration { .. }) || matches!(self.kind, Kind::Voice { recording: true, .. })
    }

    pub fn next_blink(&self, now: Instant) -> Option<Instant> {
        matches!(self.kind, Kind::Apps { .. }).then(|| self.search.next_blink(now)).flatten()
    }

    pub fn ime_caret(&self) -> Option<(f32, f32, f32)> {
        matches!(self.kind, Kind::Apps { .. }).then(|| self.search.ime_caret()).flatten()
    }

    fn filtered(&self) -> Vec<(usize, String)> {
        let Kind::Apps { list: Some(list) } = &self.kind else { return Vec::new() };
        let q = self.search.text().to_lowercase();
        list.iter().enumerate().filter(|(_, (n, _))| q.is_empty() || n.to_lowercase().contains(&q)).map(|(i, (n, _))| (i, n.clone())).collect()
    }

    /// Нарисовать поверх окна размером `skin` (затемнения в вебе нет — .overlay прозрачный).
    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, skin: (f32, f32), now: Instant) {
        let w = 420.0;
        let center = |h: f32| Rect::new((skin.0 - w) / 2.0, (skin.1 - h) / 2.0, w, h);
        // высота по содержимому: сначала тело, потом подвал (кнопки 28 + 16 снизу)
        let body_h = match &self.kind {
            Kind::Apps { .. } => 18.0 + 28.0 + 8.0 + (880.0 * 0.44) + 18.0,
            _ => 18.0 + 230.0 + 18.0,
        };
        let r = center(body_h + 28.0 + 16.0);
        self.rect = r;
        // .modal: тень, экран, рамка acc-3
        scene.shadow(cx, r, 12.0, Color::rgba(0, 0, 0, 178), 60.0);
        scene.shadow(cx, r, 12.0, Color::rgba(39, 230, 170, 38), 30.0);
        lcd_background(cx, r, 12.0);
        stroke_round(cx, r, 12.0, ACC_3, 1.0, false);
        let inner = Rect::new(r.x + 18.0, r.y + 18.0, r.w - 36.0, body_h - 36.0);
        match self.kind.clone() {
            Kind::Calibration { label, count, last, .. } => {
                self.draw_big(scene, cx, inner, now, &self.t("calibration.heading", &[("label", &label)]), &self.t("calibration.hint", &[]), &self.t("calibration.count", &[("count", &count.to_string())]), &last.map(|p| self.t("calibration.last", &[("phrase", &p)])).unwrap_or_default());
            }
            Kind::Voice { words, result, wake, .. } => {
                let goal = VOICE_GOAL.to_string();
                let hint = self.t("voiceAdapt.hint", &[("wake", &wake), ("goal", &goal)]);
                let count = self.t("voiceAdapt.count", &[("count", &words.to_string()), ("goal", &goal)]);
                self.draw_big(scene, cx, inner, now, &self.t("voiceAdapt.title", &[]), &hint, &count, &result);
            }
            Kind::Apps { list } => {
                self.search.rect = Rect::new(inner.x, inner.y, inner.w, 28.0);
                let lr = Rect::new(inner.x, inner.y + 28.0 + 8.0, inner.w, inner.h - 36.0);
                self.list_rect = lr;
                scene.flush(cx);
                scene.set_clip(Some(lr));
                unsafe { cx.dc().PushAxisAlignedClip(&lr.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
                self.rows.clear();
                let st = TextStyle { ellipsis: true, ..ui(13.0, 400, 1.45) };
                let mut y = lr.y - self.scroll;
                if list.is_none() {
                    if let Some(l) = scene.cache.get(cx.text(), &self.t("common.loading", &[]), &ui(12.0, 400, 1.45), lr.w) {
                        scene.text(&l, lr.x, y, ACC_3, Glow::G1);
                    }
                } else {
                    let shown = self.filtered();
                    if shown.is_empty() {
                        if let Some(l) = scene.cache.get(cx.text(), &self.t("common.nothingFound", &[]), &ui(12.0, 400, 1.45), lr.w) {
                            scene.text(&l, lr.x, y, ACC_3, Glow::G1);
                        }
                    }
                    for (i, name) in shown {
                        let rh = 5.0 + 18.9 + 5.0;
                        let row = Rect::new(lr.x, y, lr.w, rh);
                        let hot = self.hover == Some(i);
                        if hot {
                            fill_round(cx, row, 4.0, ACC);
                        }
                        if let Some(l) = scene.cache.get(cx.text(), &name, &st, lr.w - 16.0) {
                            scene.text(&l, row.x + 8.0, row.y + (rh - l.height) / 2.0, if hot { GLASS } else { ACC }, if hot { Glow::None } else { Glow::G1 });
                        }
                        self.rows.push((row, i));
                        y += rh;
                    }
                }
                self.max_scroll = (y + self.scroll - lr.y - lr.h).max(0.0);
                scene.flush(cx);
                scene.set_clip(None);
                unsafe { cx.dc().PopAxisAlignedClip() };
                self.search.draw(scene, cx);
            }
        }
        // подвал: кнопки справа
        let fy = r.y + body_h;
        let mut x = r.x + r.w - 18.0;
        scene.flush(cx);
        for which in [0u8, 1] {
            let show_ok = !matches!(self.kind, Kind::Apps { .. });
            let show_cancel = !matches!(self.kind, Kind::Calibration { .. });
            let (b, show) = if which == 0 { (&mut self.ok, show_ok) } else { (&mut self.cancel, show_cancel) };
            if !show {
                b.rect = Rect::new(0.0, 0.0, 0.0, 0.0);
                continue;
            }
            let bw = b.natural_width(scene, cx);
            x -= bw;
            b.rect = Rect::new(x, fy, bw, 28.0);
            x -= 8.0;
        }
        let ok_on = self.ok_enabled();
        if self.ok.rect.w > 0.0 {
            if ok_on {
                self.ok.draw(scene, cx);
            } else if crate::native::controls::push_round_layer(cx, self.ok.rect, 0.0, 0.35) {
                self.ok.draw(scene, cx);
                scene.flush(cx);
                crate::native::controls::pop_layer(cx);
            }
        }
        if self.cancel.rect.w > 0.0 {
            self.cancel.draw(scene, cx);
        }
        scene.flush(cx);
        lcd_overlay(cx, r, 12.0);
    }

    /// .calbig: мигающий шарик, заголовок, подсказка, крупный счёт, строка ниже.
    #[allow(clippy::too_many_arguments)]
    fn draw_big(&self, scene: &mut Scene, cx: &mut RenderCx<'_>, inner: Rect, now: Instant, title: &str, hint: &str, count: &str, last: &str) {
        let mut y = inner.y;
        // шарик 64 px: radial-gradient + ореол, мигает за 1,2 с
        let phase = (now.duration_since(self.t0).as_secs_f32() / 1.2 * std::f32::consts::TAU).cos() * 0.5 + 0.5;
        let a = 0.35 + 0.65 * phase;
        let orb = Rect::new(inner.x + (inner.w - 64.0) / 2.0, y, 64.0, 64.0);
        scene.shadow(cx, orb, 32.0, Color { a: 0.46 * a, ..ACC }, 30.0);
        fill_round(cx, Rect::new(orb.x + 10.0, orb.y + 10.0, 44.0, 44.0), 22.0, Color { a: 0.25 * a, ..ACC });
        fill_round(cx, Rect::new(orb.x + 20.0, orb.y + 26.0, 24.0, 24.0), 12.0, Color { a: 0.85 * a, ..ACC });
        y += 64.0 + 8.0;
        let center = |s: TextStyle| TextStyle { align: Align::Center, wrap: true, ..s };
        let parts: [(&str, TextStyle, Color, Glow); 4] = [
            (title, center(lcd(18.0, 600, 0.08)), ACC_HOT, Glow::G3),
            (hint, center(ui(12.0, 400, 1.45)), ACC_3, Glow::G1),
            (count, center(lcd(22.0, 500, 0.04)), ACC_HOT, Glow::G3),
            (last, center(ui(12.0, 400, 1.45)), ACC_3, Glow::G1),
        ];
        for (i, (text, st, c, g)) in parts.into_iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            let text = if i == 0 { text.to_uppercase() } else { text.to_string() };
            if let Some(l) = scene.cache.get(cx.text(), &text, &st, inner.w) {
                scene.text(&l, inner.x, y, c, g);
                y += l.height + 8.0;
            }
        }
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        if inside(self.list_rect, x, y) && self.max_scroll > 0.0 {
            self.scroll = (self.scroll - delta * 48.0).clamp(0.0, self.max_scroll);
            return true;
        }
        false
    }

    pub fn mouse(&mut self, ev: MouseEvent) -> (bool, Option<OverlayOut>) {
        let (x, y) = (ev.x, ev.y);
        let row_at = |o: &Self| o.rows.iter().find(|(r, _)| inside(*r, x, y) && inside(o.list_rect, x, y)).map(|(_, i)| *i);
        match ev.kind {
            MouseKind::Move => {
                let mut redraw = self.search.mouse_move(x);
                let h = row_at(self);
                if h != self.hover {
                    self.hover = h;
                    redraw = true;
                }
                redraw |= self.ok.press.set_hover(inside(self.ok.rect, x, y) && self.ok_enabled(), 0.12);
                redraw |= self.cancel.press.set_hover(inside(self.cancel.rect, x, y), 0.12);
                (redraw, None)
            }
            MouseKind::Down(MouseButton::Left) | MouseKind::DoubleClick => {
                if inside(self.search.rect, x, y) && matches!(self.kind, Kind::Apps { .. }) {
                    self.search.set_focus(true);
                    if ev.kind == MouseKind::DoubleClick {
                        self.search.double_click(x);
                    } else {
                        self.search.mouse_down(x, false);
                    }
                    return (true, None);
                }
                self.pressed = if inside(self.ok.rect, x, y) && self.ok_enabled() {
                    self.ok.press.pressed = true;
                    Some(0)
                } else if inside(self.cancel.rect, x, y) {
                    self.cancel.press.pressed = true;
                    Some(1)
                } else if row_at(self).is_some() {
                    Some(2)
                } else {
                    None
                };
                (true, None)
            }
            MouseKind::Up(MouseButton::Left) => {
                self.search.mouse_up();
                self.ok.press.pressed = false;
                self.cancel.press.pressed = false;
                let out = match self.pressed.take() {
                    Some(0) if inside(self.ok.rect, x, y) => self.ok_out(),
                    Some(1) if inside(self.cancel.rect, x, y) => Some(OverlayOut::Close),
                    Some(2) => row_at(self).and_then(|i| self.pick(i)),
                    _ => None,
                };
                (true, out)
            }
            _ => (false, None),
        }
    }

    fn ok_out(&self) -> Option<OverlayOut> {
        match self.kind {
            Kind::Calibration { .. } => Some(OverlayOut::Close),
            Kind::Voice { .. } => Some(OverlayOut::VoiceDone),
            Kind::Apps { .. } => None,
        }
    }

    fn pick(&self, i: usize) -> Option<OverlayOut> {
        let Kind::Apps { list: Some(list) } = &self.kind else { return None };
        list.get(i).map(|(n, a)| OverlayOut::App(n.clone(), a.clone()))
    }

    pub fn key(&mut self, ev: KeyEvent) -> (bool, Option<OverlayOut>) {
        let k = ev.key as u16;
        if k == VK_ESCAPE.0 {
            // Esc закрывает окно (как keydown в app.js); калибровку — «Готово»
            return (true, Some(OverlayOut::Close));
        }
        if matches!(self.kind, Kind::Apps { .. }) {
            if k == VK_RETURN.0 {
                let first = self.filtered().first().map(|(i, _)| *i);
                return (true, first.and_then(|i| self.pick(i)));
            }
            let r = self.search.key(ev);
            if r.changed {
                self.scroll = 0.0;
            }
            return (r.redraw || r.handled, None);
        }
        (false, None)
    }

    pub fn char_input(&mut self, c: char) -> bool {
        if matches!(self.kind, Kind::Apps { .. }) {
            let r = self.search.char_input(c);
            if r.changed {
                self.scroll = 0.0;
            }
            return r.redraw;
        }
        false
    }

    pub fn window_focus(&mut self, on: bool) {
        self.search.set_focus(on && matches!(self.kind, Kind::Apps { .. }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_done_needs_five_words_while_recording() {
        let mut o = Overlay::new(Kind::Voice { words: 4, result: String::new(), recording: true, wake: "reever".into() }, "en", 0);
        assert!(!o.ok_enabled());
        o.kind = Kind::Voice { words: 5, result: String::new(), recording: true, wake: "reever".into() };
        assert!(o.ok_enabled());
        o.kind = Kind::Voice { words: 9, result: "done".into(), recording: false, wake: "reever".into() };
        assert!(!o.ok_enabled());
    }

    #[test]
    fn app_search_filters_and_enter_picks_the_first() {
        let list = vec![("Calculator".to_string(), "calc".to_string()), ("Camera".to_string(), "cam".to_string())];
        let mut o = Overlay::new(Kind::Apps { list: Some(list) }, "en", 0);
        o.search.set_text("cam");
        let (_, out) = o.key(KeyEvent { key: VK_RETURN.0 as u32, ctrl: false, shift: false, alt: false, repeat: false });
        assert_eq!(out, Some(OverlayOut::App("Camera".into(), "cam".into())));
    }
}
