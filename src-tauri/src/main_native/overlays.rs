//! Окна поверх главного (.overlay > .modal в index.html): калибровка действия, подстройка под
//! голос, выбор программы Windows, сканер программ. Модальны: пока окно открыто, мышь и
//! клавиатура — его.

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
    Voice { words: u32, result: String, recording: bool, wake: String, intro: bool },
    /// программы Windows (имя, AUMID); None — ещё грузятся
    Apps { list: Option<Vec<(String, String)>> },
    /// сканер: найденные программы (None — ищутся)
    Scan { apps: Option<Vec<ScanApp>> },
}

/// Программа из сканера (appscan::ScannedApp) и её иконка.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanApp {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub phrases: Vec<String>,
    pub checked: bool,
    /// exists / generic / system
    pub note: Option<String>,
    /// frequent / recent / other
    pub group: String,
    pub focus_minutes: u32,
    /// PNG иконки и её картинка в кэше окон (заводится при первом рисовании)
    pub icon_png: Option<std::sync::Arc<[u8]>>,
    pub icon: Option<crate::native::ImageId>,
}

impl ScanApp {
    /// Можно отметить: ещё не добавлена и есть фразы.
    pub fn selectable(&self) -> bool {
        self.note.as_deref() != Some("exists") && !self.phrases.is_empty()
    }
}

/// Что окно просит.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayOut {
    /// закрыть (калибровку — отменить её, запись — бросить)
    Close,
    VoiceDone,
    /// выбрана программа: имя, AUMID
    App(String, String),
    /// добавить отмеченные программы как действия
    AddScanned(Vec<ScanApp>),
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
    /// dir="rtl": раскладка отражена
    rtl: bool,
    t0: Instant,
    all: Button,
    none: Button,
    tint: Option<(u64, windows::Win32::Graphics::Direct2D::ID2D1Effect)>,
}

impl Overlay {
    pub fn new(kind: Kind, locale: &str, hwnd: isize) -> Self {
        let tr = |k: &str| t(locale, k, &[]);
        let (ok, cancel) = match &kind {
            Kind::Calibration { .. } => (Button::new(&tr("common.done"), ButtonKind::Default), Button::new("", ButtonKind::Plain)),
            Kind::Voice { .. } => (Button::new(&tr("common.done"), ButtonKind::Default), Button::new(&tr("common.cancel"), ButtonKind::Plain)),
            Kind::Apps { .. } => (Button::new("", ButtonKind::Default), Button::new(&tr("common.close"), ButtonKind::Plain)),
            Kind::Scan { .. } => (Button::new("", ButtonKind::Default), Button::new(&tr("common.cancel"), ButtonKind::Plain)),
        };
        let typing = matches!(kind, Kind::Apps { .. } | Kind::Scan { .. });
        let mut search = TextInput::default();
        search.style = ui(13.0, 400, 1.0);
        search.placeholder = tr(if matches!(kind, Kind::Scan { .. }) { "scan.search" } else { "appPicker.search" });
        search.hwnd = hwnd;
        let rtl = crate::ui_text::direction(locale) == "rtl";
        search.rtl = rtl;
        search.set_focus(typing);
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
            rtl,
            t0: Instant::now(),
            all: Button::new(&tr("scan.all"), ButtonKind::Plain),
            none: Button::new(&tr("scan.none"), ButtonKind::Plain),
            tint: None,
        }
    }

    fn dir(&self) -> crate::native::Direction {
        if self.rtl { crate::native::Direction::Rtl } else { crate::native::Direction::Ltr }
    }

    /// Заголовок окна (для диктора).
    pub fn title(&self) -> String {
        let key = match self.kind {
            Kind::Calibration { .. } => "calibration.title",
            Kind::Voice { .. } => "voiceAdapt.title",
            Kind::Apps { .. } => "appPicker.title",
            Kind::Scan { .. } => "scan.title",
        };
        self.t(key, &[])
    }

    fn typing(&self) -> bool {
        matches!(self.kind, Kind::Apps { .. } | Kind::Scan { .. })
    }

    fn scan_visible(&self, a: &ScanApp) -> bool {
        let q = self.search.text().trim().to_lowercase();
        q.is_empty() || a.name.to_lowercase().contains(&q) || a.phrases.iter().any(|p| p.contains(&q))
    }

    /// Отметить все видимые (или снять).
    fn check_visible(&mut self, on: bool) {
        let q = self.search.text().trim().to_lowercase();
        if let Kind::Scan { apps: Some(apps) } = &mut self.kind {
            for a in apps.iter_mut() {
                let vis = q.is_empty() || a.name.to_lowercase().contains(&q) || a.phrases.iter().any(|p| p.contains(&q));
                if a.selectable() && vis {
                    a.checked = on;
                }
            }
        }
    }

    fn scan_counts(&self) -> (usize, usize) {
        match &self.kind {
            Kind::Scan { apps: Some(apps) } => (apps.iter().filter(|a| a.checked).count(), apps.iter().filter(|a| a.selectable()).count()),
            _ => (0, 0),
        }
    }

    /// Зелёный тон иконки по яркости (filter: grayscale sepia saturate hue-rotate в вебе).
    fn tint(&mut self, cx: &RenderCx<'_>, generation: u64) -> Option<windows::Win32::Graphics::Direct2D::ID2D1Effect> {
        use windows::Win32::Graphics::Direct2D::Common::{D2D_MATRIX_5X4_F, D2D_MATRIX_5X4_F_0, D2D_MATRIX_5X4_F_0_0};
        use windows::Win32::Graphics::Direct2D::{CLSID_D2D1ColorMatrix, D2D1_PROPERTY_TYPE_MATRIX_5X4};
        if let Some((g, e)) = &self.tint {
            if *g == generation {
                return Some(e.clone());
            }
        }
        let (r, g, b) = (0.42f32, 1.12f32, 0.86f32);
        let l = [0.299f32, 0.587, 0.114];
        let m = D2D_MATRIX_5X4_F {
            Anonymous: D2D_MATRIX_5X4_F_0 {
                Anonymous: D2D_MATRIX_5X4_F_0_0 {
                    _11: l[0] * r, _12: l[0] * g, _13: l[0] * b, _14: 0.0,
                    _21: l[1] * r, _22: l[1] * g, _23: l[1] * b, _24: 0.0,
                    _31: l[2] * r, _32: l[2] * g, _33: l[2] * b, _34: 0.0,
                    _41: 0.0, _42: 0.0, _43: 0.0, _44: 1.0,
                    _51: 0.0, _52: 0.0, _53: 0.0, _54: 0.0,
                },
            },
        };
        let e = unsafe {
            let e = cx.dc().CreateEffect(&CLSID_D2D1ColorMatrix).ok()?;
            let bytes = std::slice::from_raw_parts(&m as *const _ as *const u8, std::mem::size_of::<D2D_MATRIX_5X4_F>());
            e.SetValue(0, D2D1_PROPERTY_TYPE_MATRIX_5X4, bytes).ok()?;
            e
        };
        self.tint = Some((generation, e.clone()));
        Some(e)
    }

    fn t(&self, key: &str, p: &[(&str, &str)]) -> String {
        t(&self.locale, key, p)
    }

    /// «Готово» доступно (подстройка — от пяти слов, пока идёт запись).
    fn ok_enabled(&self) -> bool {
        match &self.kind {
            Kind::Scan { .. } => self.scan_counts().0 > 0,
            // обязательная подстройка первого запуска — только после всех слов
            Kind::Voice { words, recording, intro, .. } => *recording && *words >= if *intro { VOICE_GOAL } else { 5 },
            Kind::Calibration { .. } => true,
            Kind::Apps { .. } => false,
        }
    }

    /// Шарик калибровки мигает (animation: blink 1.2s) — кадры нужны всё время.
    pub fn animating(&self) -> bool {
        matches!(self.kind, Kind::Calibration { .. } | Kind::Scan { apps: None }) || matches!(self.kind, Kind::Voice { recording: true, .. })
    }

    pub fn next_blink(&self, now: Instant) -> Option<Instant> {
        self.typing().then(|| self.search.next_blink(now)).flatten()
    }

    pub fn ime_caret(&self) -> Option<(f32, f32, f32)> {
        self.typing().then(|| self.search.ime_caret()).flatten()
    }

    fn filtered(&self) -> Vec<(usize, String)> {
        let Kind::Apps { list: Some(list) } = &self.kind else { return Vec::new() };
        let q = self.search.text().to_lowercase();
        list.iter().enumerate().filter(|(_, (n, _))| q.is_empty() || n.to_lowercase().contains(&q)).map(|(i, (n, _))| (i, n.clone())).collect()
    }

    /// Нарисовать поверх окна размером `skin` (затемнения в вебе нет — .overlay прозрачный).
    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, skin: (f32, f32), now: Instant) {
        let w = if matches!(self.kind, Kind::Scan { .. }) { 640.0 } else { 420.0 };
        let center = |h: f32| Rect::new((skin.0 - w) / 2.0, (skin.1 - h) / 2.0, w, h);
        // высота по содержимому: сначала тело, потом подвал (кнопки 28 + 16 снизу)
        let body_h = match &self.kind {
            Kind::Apps { .. } => 18.0 + 28.0 + 8.0 + (880.0 * 0.44) + 18.0,
            Kind::Scan { .. } => 18.0 + 24.0 + 6.0 + 36.0 + 12.0 + 28.0 + 6.0 + (880.0 * 0.52) + 18.0,
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
            Kind::Voice { words, result, wake, intro, .. } => {
                let goal = VOICE_GOAL.to_string();
                let hint = self.t(if intro { "voiceAdapt.introHint" } else { "voiceAdapt.hint" }, &[("wake", &wake), ("goal", &goal)]);
                let count = self.t("voiceAdapt.count", &[("count", &words.to_string()), ("goal", &goal)]);
                self.draw_big(scene, cx, inner, now, &self.t("voiceAdapt.title", &[]), &hint, &count, &result);
            }
            Kind::Scan { apps } => self.draw_scan(scene, cx, inner, now, apps),
            Kind::Apps { list } => {
                self.search.rect = Rect::new(inner.x, inner.y, inner.w, 28.0);
                let lr = Rect::new(inner.x, inner.y + 28.0 + 8.0, inner.w, inner.h - 36.0);
                self.list_rect = lr;
                scene.flush(cx);
                scene.set_clip(Some(lr));
                unsafe { cx.dc().PushAxisAlignedClip(&lr.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
                self.rows.clear();
                let st = TextStyle { ellipsis: true, direction: self.dir(), ..ui(13.0, 400, 1.45) };
                let note_st = TextStyle { direction: self.dir(), ..ui(12.0, 400, 1.45) };
                let mut y = lr.y - self.scroll;
                if list.is_none() {
                    if let Some(l) = scene.cache.get(cx.text(), &self.t("common.loading", &[]), &note_st, lr.w) {
                        scene.text(&l, lr.x, y, ACC_3, Glow::G1);
                    }
                } else {
                    let shown = self.filtered();
                    if shown.is_empty() {
                        if let Some(l) = scene.cache.get(cx.text(), &self.t("common.nothingFound", &[]), &note_st, lr.w) {
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
        if matches!(self.kind, Kind::Scan { .. }) {
            self.ok.label = self.t("scan.add", &[("count", &self.scan_counts().0.to_string())]);
        }
        for which in [0u8, 1] {
            let show_ok = !matches!(self.kind, Kind::Apps { .. });
            // первая подстройка: ни отмены, ни пропуска, пока идёт запись
            let show_cancel = !matches!(self.kind, Kind::Calibration { .. } | Kind::Voice { intro: true, recording: true, .. });
            let (b, show) = if which == 0 { (&mut self.ok, show_ok) } else { (&mut self.cancel, show_cancel) };
            if !show {
                b.rect = Rect::new(0.0, 0.0, 0.0, 0.0);
                continue;
            }
            let bw = b.natural_width(scene, cx);
            x -= bw;
            b.rect = crate::native::controls::mirror(r, Rect::new(x, fy, bw, 28.0), self.rtl);
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

    /// Сканер: заголовок со счётом, подсказка, фильтр и «Все»/«Ничего», список с разделами.
    fn draw_scan(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, inner: Rect, now: Instant, apps: Option<Vec<ScanApp>>) {
        let mut y = inner.y;
        let title = self.t("scan.title", &[]).to_uppercase();
        let rtl = self.rtl;
        let m = move |c: Rect, r: Rect| crate::native::controls::mirror(c, r, rtl);
        let dir = self.dir();
        if let Some(l) = scene.cache.get(cx.text(), &title, &TextStyle { direction: dir, ..lcd(18.0, 600, 0.08) }, inner.w) {
            scene.text(&l, inner.x, y, ACC_HOT, Glow::G3);
        }
        let (checked, total) = self.scan_counts();
        if apps.as_ref().is_some_and(|a| !a.is_empty()) {
            let c = self.t("scan.count", &[("checked", &checked.to_string()), ("total", &total.to_string())]);
            if let Some(l) = scene.cache.get(cx.text(), &c, &crate::native::controls::data(11.0, 1.0), 1000.0) {
                scene.text(&l, m(inner, Rect::new(inner.x + inner.w - l.width, y, l.width, 0.0)).x, y + 4.0, crate::native::controls::ACC_2, Glow::G1);
            }
        }
        y += 24.0;
        if let Some(l) = scene.cache.get(cx.text(), &self.t("scan.hint", &[]), &TextStyle { wrap: true, direction: dir, ..ui(12.0, 400, 1.45) }, inner.w) {
            scene.text(&l, inner.x, y, ACC_3, Glow::G1);
        }
        y += 36.0 + 12.0;
        // фильтр и кнопки
        let nw = self.none.natural_width(scene, cx);
        let aw = self.all.natural_width(scene, cx);
        let none_r = Rect::new(inner.x + inner.w - nw, y, nw, 28.0);
        let all_r = Rect::new(none_r.x - 8.0 - aw, y, aw, 28.0);
        self.search.rect = m(inner, Rect::new(inner.x, y, all_r.x - 8.0 - inner.x, 28.0));
        self.none.rect = m(inner, none_r);
        self.all.rect = m(inner, all_r);
        y += 28.0 + 6.0;
        let lr = Rect::new(inner.x - 6.0, y, inner.w + 12.0, inner.y + inner.h - y);
        self.list_rect = lr;
        scene.flush(cx);
        scene.set_clip(Some(lr));
        unsafe { cx.dc().PushAxisAlignedClip(&lr.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        self.rows.clear();
        let mut ry = lr.y - self.scroll;
        let hint = |scene: &mut Scene, cx: &mut RenderCx<'_>, text: &str, a: f32| {
            if let Some(l) = scene.cache.get(cx.text(), text, &TextStyle { direction: dir, ..ui(12.0, 400, 1.45) }, lr.w - 12.0) {
                scene.text_alpha(&l, lr.x + 6.0, lr.y + 18.0, ACC_3, Glow::G1, a);
            }
        };
        match &apps {
            None => {
                let phase = (now.duration_since(self.t0).as_secs_f32() / 1.2 * std::f32::consts::TAU).cos() * 0.5 + 0.5;
                hint(scene, cx, &self.t("scan.loading", &[]), 0.35 + 0.65 * phase);
            }
            Some(list) => {
                let shown: Vec<(usize, &ScanApp)> = list.iter().enumerate().filter(|(_, a)| self.scan_visible(a)).collect();
                if shown.is_empty() {
                    hint(scene, cx, &self.t(if list.is_empty() { "scan.empty" } else { "common.nothingFound" }, &[]), 1.0);
                }
                let grouped = shown.iter().any(|(_, a)| a.group != "other");
                let mut group: Option<&str> = None;
                let generation = cx_generation(cx);
                for (i, a) in shown {
                    if grouped && group != Some(a.group.as_str()) {
                        group = Some(a.group.as_str());
                        ry += if self.rows.is_empty() { 0.0 } else { 6.0 };
                        let gt = self.t(&format!("scan.group.{}", a.group), &[]).to_uppercase();
                        if let Some(l) = scene.cache.get(cx.text(), &gt, &TextStyle { direction: dir, ..lcd(14.0, 500, 0.08) }, lr.w - 12.0) {
                            scene.text(&l, lr.x + 6.0, ry + 6.0, ACC_3, Glow::G1);
                        }
                        crate::native::controls::dotted_hline(cx, lr.x, ry + 25.0, lr.w, crate::native::controls::ACC_4);
                        ry += 25.0 + 2.0;
                    }
                    let rh = 6.0 + 17.0 + 3.0 + 16.1 + 6.0;
                    let row = Rect::new(lr.x, ry, lr.w, rh);
                    let off = !a.selectable();
                    let alpha = if off { 0.45 } else { 1.0 };
                    if self.hover == Some(i) && !off {
                        fill_round(cx, row, 5.0, crate::native::controls::ACC_5);
                    }
                    // флажок
                    let cb0 = Rect::new(row.x + 6.0, row.y + (rh - 16.0) / 2.0, 16.0, 16.0);
                    let cb = m(row, cb0);
                    fill_round(cx, cb, 3.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 * alpha });
                    stroke_round(cx, cb, 3.0, Color { a: ACC_3.a * alpha, ..ACC_3 }, 1.0, false);
                    if a.checked {
                        let ir = Rect::new(cb.x + 4.0, cb.y + 4.0, 8.0, 8.0);
                        scene.shadow(cx, ir, 1.0, ACC, 6.0);
                        fill_round(cx, ir, 1.0, ACC);
                    }
                    // иконка
                    let ic0 = Rect::new(cb0.x + 16.0 + 10.0, row.y + (rh - 22.0) / 2.0, 22.0, 22.0);
                    let ic = m(row, ic0);
                    let mut drew = false;
                    if let Some(png) = &a.icon_png {
                        let id = match a.icon {
                            Some(id) => id,
                            None => {
                                let id = cx.images().add_bytes(png.clone());
                                if let Kind::Scan { apps: Some(list) } = &mut self.kind {
                                    if let Some(x) = list.get_mut(i) {
                                        x.icon = Some(id);
                                    }
                                }
                                id
                            }
                        };
                        let px = (22.0 * cx.scale).round().max(1.0) as u32;
                        if let (Some(bmp), Some(fx)) = (cx.bitmap(id, px, px), self.tint(cx, generation)) {
                            unsafe {
                                fx.SetInput(0, &bmp, true);
                                if let Ok(out) = fx.GetOutput() {
                                    let dc = cx.dc().clone();
                                    let mut old = windows::Foundation::Numerics::Matrix3x2::default();
                                    dc.GetTransform(&mut old);
                                    // эффект рисуется в пикселях битмапа: масштаб — до DIP иконки
                                    let k = 22.0 / px as f32;
                                    let m = windows::Foundation::Numerics::Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: ic.x, M32: ic.y };
                                    let (mut dx, mut dy) = (96.0f32, 96.0f32);
                                    dc.GetDpi(&mut dx, &mut dy);
                                    dc.SetDpi(96.0, 96.0);
                                    let scale = dx / 96.0;
                                    let ms = windows::Foundation::Numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 };
                                    dc.SetTransform(&(m * ms * old));
                                    dc.DrawImage(&out, None, None, windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR, windows::Win32::Graphics::Direct2D::Common::D2D1_COMPOSITE_MODE_SOURCE_OVER);
                                    dc.SetDpi(dx, dy);
                                    dc.SetTransform(&old);
                                }
                            }
                            drew = true;
                        }
                    }
                    if !drew {
                        let icon = if a.target.contains("://") { "play" } else { "grid" };
                        scene.icon(icon, Rect::new(ic.x + 3.0, ic.y + 3.0, 16.0, 16.0), 1.6, Color { a: ACC_3.a * alpha, ..ACC_3 }, false, Glow::G1);
                    }
                    // имя, отметки
                    let bx = ic0.x + 22.0 + 10.0;
                    let bw = row.x + row.w - 6.0 - bx;
                    let mut nx = bx;
                    let name_st = TextStyle { ellipsis: true, direction: dir, ..ui(13.0, 400, 1.3) };
                    let mark = match a.group.as_str() {
                        "frequent" | "recent" => Some((self.t(&format!("scan.mark.{}", a.group), &[]).to_uppercase(), a.group == "frequent")),
                        _ => None,
                    };
                    let note = a.note.as_ref().map(|n| self.t(&format!("scan.note.{n}"), &[]).to_uppercase());
                    let tag_st = lcd(13.0, 500, 0.06);
                    let tags_w: f32 = [mark.as_ref().map(|m| m.0.clone()), note.clone()]
                        .iter()
                        .flatten()
                        .filter_map(|s| scene.cache.get(cx.text(), s, &tag_st, 1000.0).map(|l| l.width + 12.0 + 8.0))
                        .sum();
                    if let Some(l) = scene.cache.get(cx.text(), &a.name, &name_st, (bw - tags_w).max(40.0)) {
                        scene.text_alpha(&l, m(row, Rect::new(nx, row.y, (bw - tags_w).max(40.0), 0.0)).x, row.y + 6.0, ACC_HOT, Glow::G1, alpha);
                        nx += l.width + 8.0;
                    }
                    for (text, solid, dashed) in [mark.map(|(t, f)| (t, f, false)), note.map(|n| (n, false, true))].into_iter().flatten() {
                        if let Some(l) = scene.cache.get(cx.text(), &text, &tag_st, 1000.0) {
                            let tr = m(row, Rect::new(nx, row.y + 6.0 + 1.0, l.width + 10.0, l.height + 2.0));
                            if solid {
                                fill_round(cx, tr, 3.0, Color { a: crate::native::controls::ACC_2.a * alpha, ..crate::native::controls::ACC_2 });
                            } else {
                                stroke_round(cx, tr, 3.0, Color { a: crate::native::controls::ACC_4.a * alpha, ..crate::native::controls::ACC_4 }, 1.0, dashed);
                            }
                            let c = if solid { GLASS } else if dashed { ACC_3 } else { crate::native::controls::ACC_2 };
                            scene.text_alpha(&l, tr.x + 5.0, tr.y + 1.0, c, if solid { Glow::None } else { Glow::G1 }, alpha);
                            nx += tr.w + 8.0;
                        }
                    }
                    let phr = if a.phrases.is_empty() { "-".to_string() } else { a.phrases.join(" · ") };
                    let phr_st = TextStyle { ellipsis: true, direction: dir, ..lcd(14.0, 500, 0.04) };
                    if let Some(l) = scene.cache.get(cx.text(), &phr, &phr_st, bw) {
                        let c = if a.checked { crate::native::controls::ACC_2 } else { ACC_3 };
                        scene.text_alpha(&l, m(row, Rect::new(bx, row.y, bw, 0.0)).x, row.y + 6.0 + 17.0 + 3.0, c, Glow::G1, alpha);
                    }
                    self.rows.push((row, i));
                    ry += rh;
                }
            }
        }
        self.max_scroll = (ry + self.scroll - lr.y - lr.h).max(0.0);
        self.scroll = self.scroll.min(self.max_scroll);
        scene.flush(cx);
        scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
        self.search.draw(scene, cx);
        scene.flush(cx);
        self.all.draw(scene, cx);
        self.none.draw(scene, cx);
        scene.flush(cx);
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
        let dir = self.dir();
        let center = |s: TextStyle| TextStyle { align: Align::Center, wrap: true, direction: dir, ..s };
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
        let scan = matches!(self.kind, Kind::Scan { .. });
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
                if scan {
                    redraw |= self.all.press.set_hover(inside(self.all.rect, x, y), 0.12);
                    redraw |= self.none.press.set_hover(inside(self.none.rect, x, y), 0.12);
                }
                (redraw, None)
            }
            MouseKind::Down(MouseButton::Left) | MouseKind::DoubleClick => {
                if inside(self.search.rect, x, y) && self.typing() {
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
                } else if scan && inside(self.all.rect, x, y) {
                    self.all.press.pressed = true;
                    Some(3)
                } else if scan && inside(self.none.rect, x, y) {
                    self.none.press.pressed = true;
                    Some(4)
                } else {
                    None
                };
                (true, None)
            }
            MouseKind::Up(MouseButton::Left) => {
                self.search.mouse_up();
                self.ok.press.pressed = false;
                self.cancel.press.pressed = false;
                self.all.press.pressed = false;
                self.none.press.pressed = false;
                let out = match self.pressed.take() {
                    Some(0) if inside(self.ok.rect, x, y) => self.ok_out(),
                    Some(1) if inside(self.cancel.rect, x, y) => Some(OverlayOut::Close),
                    Some(2) if scan => {
                        if let Some(i) = row_at(self) {
                            if let Kind::Scan { apps: Some(apps) } = &mut self.kind {
                                if let Some(a) = apps.get_mut(i).filter(|a| a.selectable()) {
                                    a.checked = !a.checked;
                                }
                            }
                        }
                        None
                    }
                    Some(2) => row_at(self).and_then(|i| self.pick(i)),
                    Some(3) if inside(self.all.rect, x, y) => {
                        self.check_visible(true);
                        None
                    }
                    Some(4) if inside(self.none.rect, x, y) => {
                        self.check_visible(false);
                        None
                    }
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
            Kind::Scan { apps: Some(ref apps) } => Some(OverlayOut::AddScanned(apps.iter().filter(|a| a.checked).cloned().collect())),
            Kind::Scan { apps: None } => None,
        }
    }

    fn pick(&self, i: usize) -> Option<OverlayOut> {
        let Kind::Apps { list: Some(list) } = &self.kind else { return None };
        list.get(i).map(|(n, a)| OverlayOut::App(n.clone(), a.clone()))
    }

    pub fn key(&mut self, ev: KeyEvent) -> (bool, Option<OverlayOut>) {
        let k = ev.key as u16;
        if k == VK_ESCAPE.0 && matches!(self.kind, Kind::Voice { intro: true, recording: true, .. }) {
            return (false, None);
        }
        if k == VK_ESCAPE.0 {
            // Esc закрывает окно (как keydown в app.js); калибровку — «Готово»
            return (true, Some(OverlayOut::Close));
        }
        if matches!(self.kind, Kind::Scan { .. }) {
            let r = self.search.key(ev);
            if r.changed {
                self.scroll = 0.0;
            }
            return (r.redraw || r.handled, None);
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
        if self.typing() {
            let r = self.search.char_input(c);
            if r.changed {
                self.scroll = 0.0;
            }
            return r.redraw;
        }
        false
    }

    pub fn window_focus(&mut self, on: bool) {
        self.search.set_focus(on && self.typing());
    }

    /// Подстройка закончилась: итог; кнопка отмены становится «Закрыть».
    pub fn voice_result(&mut self, text: String, restart: bool) {
        if let Kind::Voice { result, recording, words, .. } = &mut self.kind {
            *result = text;
            if restart {
                *recording = true;
                *words = 0;
            }
        }
        if !restart {
            self.cancel.label = t(&self.locale, "common.close", &[]);
        }
    }

    /// Найденные программы пришли (сканер ещё открыт).
    pub fn set_scan(&mut self, apps: Vec<ScanApp>) {
        if let Kind::Scan { apps: a } = &mut self.kind {
            *a = Some(apps);
        }
    }

    /// Иконки пришли: id → PNG.
    pub fn set_icons(&mut self, icons: &std::collections::HashMap<String, std::sync::Arc<[u8]>>) {
        if let Kind::Scan { apps: Some(apps) } = &mut self.kind {
            for a in apps.iter_mut() {
                if let Some(png) = icons.get(&a.id) {
                    a.icon_png = Some(png.clone());
                    a.icon = None;
                }
            }
        }
    }
}

/// Поколение устройства для кэша эффекта: битмап цели меняется вместе с устройством.
fn cx_generation(cx: &RenderCx<'_>) -> u64 {
    use windows::core::Interface;
    cx.dc().as_raw() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_done_needs_five_words_while_recording() {
        let mut o = Overlay::new(Kind::Voice { words: 4, result: String::new(), recording: true, wake: "reever".into(), intro: false }, "en", 0);
        assert!(!o.ok_enabled());
        o.kind = Kind::Voice { words: 5, result: String::new(), recording: true, wake: "reever".into(), intro: false };
        assert!(o.ok_enabled());
        o.kind = Kind::Voice { words: 9, result: "done".into(), recording: false, wake: "reever".into(), intro: false };
        assert!(!o.ok_enabled());
    }

    #[test]
    fn scan_all_checks_only_selectable_visible_apps() {
        let app = |id: &str, note: Option<&str>, phrases: &[&str]| ScanApp {
            id: id.into(),
            name: id.into(),
            kind: "shell".into(),
            target: String::new(),
            phrases: phrases.iter().map(|p| p.to_string()).collect(),
            checked: false,
            note: note.map(String::from),
            group: "other".into(),
            focus_minutes: 0,
            icon_png: None,
            icon: None,
        };
        let mut o = Overlay::new(Kind::Scan { apps: None }, "en", 0);
        o.set_scan(vec![app("obs", None, &["obs"]), app("notepad", Some("exists"), &["notepad"]), app("blank", None, &[])]);
        o.check_visible(true);
        assert_eq!(o.scan_counts(), (1, 1));
        assert!(o.ok_enabled());
        match o.ok_out() {
            Some(OverlayOut::AddScanned(list)) => assert_eq!(list.len(), 1),
            other => panic!("{other:?}"),
        }
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
