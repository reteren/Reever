//! Форма скина сверху вниз: группы-рамки с легендой (fieldset.grp), подсказки (.hint),
//! подписи полей (.fld), фишки с крестиком (.grammar-chip), поле с кнопкой «Добавить»
//! (.editrow), поля ввода, флажки (.cbrow), ряды кнопок и список ошибок. Окно описывает форму
//! списком [`Item`] (заново после каждой правки), форма раскладывает и рисует его в заданной
//! колонке и превращает мышь и клавиатуру в [`FormEvent`]. Состояние полей ввода (каретка,
//! выделение, отмена) живёт в форме по id и переживает пересборку списка.
//!
//! Первым пользуется редактор «Что можно сказать» главного окна; дальше — «Действия» и
//! «Настройки».

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN, VK_TAB};

use super::button::{Button, ButtonKind};
use super::text_input::TextInput;
use super::{fill_round, inside, lcd, mix, stroke_round, ui, with_alpha, Glow, Scene, ACC, ACC_2, ACC_3, ACC_4, ACC_5, ACC_HOT};
use crate::native::{Color, Direction, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx, TextStyle};

/// Элемент формы.
#[derive(Debug, Clone)]
pub enum Item {
    /// Начало рамки с легендой (значок — имя из `icons`).
    GroupBegin { legend: String, icon: Option<&'static str> },
    GroupEnd,
    /// Мелкая подсказка (.hint).
    Note(String),
    /// Подпись поля (.fld).
    Label(String),
    /// Фишки с крестиком; `primary_first` — первая выделена (главное wake-слово).
    Chips { id: String, values: Vec<String>, primary_first: bool },
    /// Поле и кнопка «Добавить» рядом: Enter или кнопка — [`FormEvent::Add`].
    AddRow { id: String, placeholder: String, button: String },
    /// Поле ввода; `value` — начальный текст, если поля с этим id ещё не было.
    Input { id: String, placeholder: String, value: String },
    Check { id: String, label: String, on: bool },
    /// Ряд кнопок с отступами сверху и снизу.
    Buttons { buttons: Vec<(String, String, ButtonKind)>, top: f32, bottom: f32 },
    /// Ошибки — строками с маркером.
    Errors(Vec<String>),
}

/// Что сделал человек.
#[derive(Debug, Clone, PartialEq)]
pub enum FormEvent {
    Click(String),
    Toggle(String),
    Remove(String, usize),
    Add(String, String),
    Edited(String, String),
    Escape,
}

#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Button(String),
    Remove(String, usize),
    Check(String),
    Input(String),
}

/// Итог события: перерисовать ли и что произошло.
#[derive(Debug, Default)]
pub struct FormResult {
    pub redraw: bool,
    pub event: Option<FormEvent>,
}

impl FormResult {
    fn redraw() -> Self {
        Self { redraw: true, event: None }
    }
    fn event(e: FormEvent) -> Self {
        Self { redraw: true, event: Some(e) }
    }
}

#[derive(Default)]
pub struct Form {
    items: Vec<Item>,
    inputs: HashMap<String, TextInput>,
    buttons: HashMap<String, Button>,
    hits: Vec<(Rect, Hit)>,
    /// порядок полей для Tab
    order: Vec<String>,
    focus: Option<String>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    dragging: Option<String>,
    hwnd: isize,
    pub rtl: bool,
    /// окно в фокусе клавиатуры (каретка мигает только тогда)
    window_focused: bool,
}

fn input_style() -> TextStyle {
    ui(13.0, 400, 1.0)
}

impl Form {
    pub fn new(hwnd: isize, rtl: bool) -> Self {
        Self { hwnd, rtl, window_focused: true, ..Default::default() }
    }

    /// Новый список элементов. Поля с теми же id сохраняют текст и каретку; новые получают
    /// `value`; исчезнувшие забываются.
    pub fn set_items(&mut self, items: Vec<Item>) {
        let mut keep = std::collections::HashSet::new();
        self.order.clear();
        for it in &items {
            let (id, placeholder, value) = match it {
                Item::AddRow { id, placeholder, .. } => (id, placeholder, ""),
                Item::Input { id, placeholder, value } => (id, placeholder, value.as_str()),
                _ => continue,
            };
            keep.insert(id.clone());
            self.order.push(id.clone());
            let hwnd = self.hwnd;
            let rtl = self.rtl;
            let input = self.inputs.entry(id.clone()).or_insert_with(|| {
                let mut i = TextInput::default();
                i.style = input_style();
                i.hwnd = hwnd;
                i.rtl = rtl;
                i.set_text(value);
                i
            });
            input.placeholder = placeholder.clone();
        }
        self.inputs.retain(|k, _| keep.contains(k));
        if self.focus.as_ref().is_some_and(|f| !keep.contains(f)) {
            self.focus = None;
        }
        self.items = items;
    }

    pub fn value(&self, id: &str) -> Option<String> {
        self.inputs.get(id).map(|i| i.text())
    }

    pub fn set_value(&mut self, id: &str, v: &str) {
        if let Some(i) = self.inputs.get_mut(id) {
            i.set_text(v);
        }
    }

    pub fn focus_input(&mut self, id: &str) {
        if self.inputs.contains_key(id) {
            self.set_focus(Some(id.to_string()));
        }
    }

    pub fn has_focus(&self) -> bool {
        self.focus.is_some()
    }

    fn set_focus(&mut self, id: Option<String>) {
        if let Some(old) = self.focus.take() {
            if let Some(i) = self.inputs.get_mut(&old) {
                i.set_focus(false);
            }
        }
        if let Some(id) = &id {
            if let Some(i) = self.inputs.get_mut(id) {
                i.set_focus(self.window_focused);
            }
        }
        self.focus = id;
    }

    pub fn window_focus(&mut self, on: bool) {
        self.window_focused = on;
        if let Some(f) = self.focus.clone() {
            if let Some(i) = self.inputs.get_mut(&f) {
                i.set_focus(on);
            }
        }
    }

    /// Следующее мигание каретки.
    pub fn next_blink(&self, now: Instant) -> Option<Instant> {
        let f = self.focus.as_ref()?;
        self.inputs.get(f)?.next_blink(now)
    }

    /// Каретка для окна набора IME (координаты формы).
    pub fn ime_caret(&self) -> Option<(f32, f32, f32)> {
        let f = self.focus.as_ref()?;
        self.inputs.get(f)?.ime_caret()
    }

    fn button(&mut self, id: &str, label: &str, kind: ButtonKind) -> &mut Button {
        let b = self.buttons.entry(id.to_string()).or_insert_with(|| Button::new(label, kind));
        b.label = label.to_string();
        b.kind = kind;
        b
    }

    fn dir(&self) -> Direction {
        if self.rtl { Direction::Rtl } else { Direction::Ltr }
    }

    /// Нарисовать в колонке (x, y, ширина w); вернуть высоту.
    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, x: f32, y0: f32, w: f32) -> f32 {
        self.hits.clear();
        let items = self.items.clone();
        let mut y = y0;
        // открытые рамки: (верх, легенда: x0, x1, высота)
        let mut groups: Vec<(f32, f32, f32, f32)> = Vec::new();
        let (mut cx0, mut cw) = (x, w);
        for it in &items {
            match it {
                Item::GroupBegin { legend, icon } => {
                    // legend: padding 0 6px, gap 6, значок 15 px, текст 16 px
                    let st = lcd(16.0, 600, 0.08);
                    let text = legend.to_uppercase();
                    let l = scene.cache.get(cx.text(), &text, &st, 10_000.0);
                    let lh = l.as_ref().map_or(16.0, |l| l.height).max(16.0);
                    let lx0 = cx0 + 14.0 - 2.0;
                    let mut tx = lx0 + 6.0;
                    if let Some(name) = icon {
                        scene.icon(name, Rect::new(tx, y + (lh - 15.0) / 2.0, 15.0, 15.0), 1.6, ACC_HOT, false, Glow::G2);
                        tx += 15.0 + 6.0;
                    }
                    if let Some(l) = &l {
                        scene.text(l, tx, y, ACC_HOT, Glow::G2);
                        tx += l.width;
                    }
                    groups.push((y + lh / 2.0, lx0, tx + 6.0, lh));
                    y += lh + 4.0;
                    cx0 += 15.0;
                    cw -= 30.0;
                }
                Item::GroupEnd => {
                    let Some((top, lx0, lx1, _)) = groups.pop() else { continue };
                    cx0 -= 15.0;
                    cw += 30.0;
                    y += 12.0;
                    let r = Rect::new(cx0 + 0.5, top, cw - 1.0, y - top);
                    scene.flush(cx);
                    // рамка с разрывом под легендой: три непересекающиеся области обрезки
                    let parts = [
                        Rect::new(r.x - 2.0, r.y - 2.0, lx0 - r.x + 2.0, r.h + 4.0),
                        Rect::new(lx1, r.y - 2.0, r.x + r.w + 2.0 - lx1, r.h + 4.0),
                        Rect::new(lx0, r.y + 2.0, lx1 - lx0, r.h + 2.0),
                    ];
                    for p in parts {
                        if p.w <= 0.0 {
                            continue;
                        }
                        unsafe { cx.dc().PushAxisAlignedClip(&p.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
                        stroke_round(cx, r, 8.0, ACC_4, 1.0, false);
                        unsafe { cx.dc().PopAxisAlignedClip() };
                    }
                    y += 12.0;
                }
                Item::Note(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    let st = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
                    if let Some(l) = scene.cache.get(cx.text(), text, &st, cw) {
                        scene.text(&l, cx0, y + 2.0, ACC_3, Glow::G1);
                        y += l.height + 4.0;
                    }
                }
                Item::Label(text) => {
                    y += 12.0;
                    let st = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
                    if let Some(l) = scene.cache.get(cx.text(), text, &st, cw) {
                        scene.text(&l, cx0, y, ACC_2, Glow::G1);
                        y += l.height;
                    }
                    y += 5.0;
                }
                Item::Chips { id, values, primary_first } => {
                    y += 6.0;
                    let st = lcd(15.0, 500, 0.04);
                    let (mut px, mut row_h) = (cx0, 0.0f32);
                    let mut any = false;
                    for (i, v) in values.iter().enumerate() {
                        let Some(l) = scene.cache.get(cx.text(), v, &st, 10_000.0) else { continue };
                        // padding 3 4 3 9, крестик 18×18 через gap 6, рамка 1
                        let cw_ = 1.0 + 9.0 + l.width + 6.0 + 18.0 + 4.0 + 1.0;
                        let ch = 1.0 + 3.0 + l.height.max(18.0) + 3.0 + 1.0;
                        if px > cx0 && px + cw_ > cx0 + cw {
                            px = cx0;
                            y += row_h + 5.0;
                            row_h = 0.0;
                        }
                        let r = Rect::new(px, y, cw_, ch);
                        let primary = *primary_first && i == 0;
                        if primary {
                            scene.shadow(cx, r, 4.0, Color::rgba(39, 230, 170, 51), 8.0);
                        }
                        fill_round(cx, r, 4.0, ACC_5);
                        stroke_round(cx, r, 4.0, if primary { ACC_2 } else { ACC_4 }, 1.0, false);
                        scene.text(&l, r.x + 10.0, r.y + (ch - l.height) / 2.0, ACC_HOT, Glow::None);
                        let xr = Rect::new(r.x + r.w - 1.0 - 4.0 - 18.0, r.y + (ch - 18.0) / 2.0, 18.0, 18.0);
                        let hit = Hit::Remove(id.clone(), i);
                        let hot = self.hover.as_ref() == Some(&hit);
                        scene.icon("x", Rect::new(xr.x + 2.0, xr.y + 2.0, 14.0, 14.0), 1.6, if hot { ACC_HOT } else { ACC }, false, if hot { Glow::G3 } else { Glow::G2 });
                        self.hits.push((xr, hit));
                        px += cw_ + 5.0;
                        row_h = row_h.max(ch);
                        any = true;
                    }
                    if any {
                        y += row_h;
                    }
                    y += 6.0;
                }
                Item::AddRow { id, button, .. } => {
                    let bid = format!("add:{id}");
                    let b = self.button(&bid, button, ButtonKind::Plain);
                    let bw = b.natural_width(scene, cx);
                    b.rect = Rect::new(cx0 + cw - bw, y, bw, 28.0);
                    let br = b.rect;
                    if let Some(inp) = self.inputs.get_mut(id) {
                        inp.rect = Rect::new(cx0, y, (cw - bw - 8.0).max(40.0), 28.0);
                        self.hits.push((inp.rect, Hit::Input(id.clone())));
                    }
                    self.hits.push((br, Hit::Button(bid)));
                    y += 28.0;
                }
                Item::Input { id, .. } => {
                    if let Some(inp) = self.inputs.get_mut(id) {
                        inp.rect = Rect::new(cx0, y, cw, 28.0);
                        self.hits.push((inp.rect, Hit::Input(id.clone())));
                    }
                    y += 28.0;
                }
                Item::Check { id, label, on } => {
                    y += 9.0;
                    let st = TextStyle { wrap: true, direction: self.dir(), ..ui(13.0, 400, 1.45) };
                    let l = scene.cache.get(cx.text(), label, &st, (cw - 25.0).max(40.0));
                    let h = l.as_ref().map_or(16.0, |l| l.height).max(16.0);
                    let b = Rect::new(cx0, y + (h - 16.0) / 2.0, 16.0, 16.0);
                    fill_round(cx, b, 3.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 });
                    let hit = Hit::Check(id.clone());
                    let hot = self.hover.as_ref() == Some(&hit);
                    stroke_round(cx, b, 3.0, if hot { ACC_2 } else { ACC_3 }, 1.0, false);
                    if *on {
                        let i = Rect::new(b.x + 4.0, b.y + 4.0, 8.0, 8.0);
                        scene.shadow(cx, i, 1.0, ACC, 6.0);
                        fill_round(cx, i, 1.0, ACC);
                    }
                    let mut tw = 0.0;
                    if let Some(l) = &l {
                        scene.text(l, cx0 + 25.0, y + (h - l.height) / 2.0, ACC, Glow::G1);
                        tw = l.width;
                    }
                    self.hits.push((Rect::new(cx0, y, 25.0 + tw, h), hit));
                    y += h + 9.0;
                }
                Item::Buttons { buttons, top, bottom } => {
                    y += top;
                    let mut px = cx0;
                    let mut row_h = 0.0f32;
                    for (id, label, kind) in buttons {
                        let b = self.button(id, label, *kind);
                        let bw = b.natural_width(scene, cx);
                        if px > cx0 && px + bw > cx0 + cw {
                            px = cx0;
                            y += row_h + 8.0;
                        }
                        b.rect = Rect::new(px, y, bw, 28.0);
                        let br = b.rect;
                        self.hits.push((br, Hit::Button(id.clone())));
                        px += bw + 8.0;
                        row_h = 28.0;
                    }
                    y += row_h + bottom;
                }
                Item::Errors(list) => {
                    if list.is_empty() {
                        continue;
                    }
                    y += 6.0;
                    let st = TextStyle { wrap: true, direction: self.dir(), ..ui(13.0, 400, 1.45) };
                    for e in list {
                        if let Some(l) = scene.cache.get(cx.text(), e, &st, (cw - 18.0).max(40.0)) {
                            fill_round(cx, Rect::new(cx0 + 6.0, y + 8.0, 4.0, 4.0), 2.0, ACC_HOT);
                            scene.text(&l, cx0 + 18.0, y, ACC_HOT, Glow::G1);
                            y += l.height;
                        }
                    }
                    y += 6.0;
                }
            }
        }
        // поля и кнопки — поверх подложки, после текстов рамок
        scene.flush(cx);
        let ids: Vec<String> = self.order.clone();
        for id in ids {
            if let Some(inp) = self.inputs.get_mut(&id) {
                inp.draw(scene, cx);
            }
        }
        scene.flush(cx);
        let shown: Vec<String> = self
            .hits
            .iter()
            .filter_map(|(_, h)| if let Hit::Button(b) = h { Some(b.clone()) } else { None })
            .collect();
        for id in shown {
            if let Some(b) = self.buttons.get(&id) {
                b.draw(scene, cx);
            }
        }
        scene.flush(cx);
        let _ = (mix, with_alpha, inside);
        y - y0
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|(_, h)| h.clone())
    }

    /// Курсор над чем-то кликабельным (рука) или над полем (I-образный — решает окно).
    pub fn hover_kind(&self, x: f32, y: f32) -> Option<bool> {
        self.hit_at(x, y).map(|h| !matches!(h, Hit::Input(_)))
    }

    /// Мышь в координатах формы. `None` от `hit` — щелчок мимо формы снимает фокус поля.
    pub fn mouse(&mut self, ev: MouseEvent) -> FormResult {
        let (x, y) = (ev.x, ev.y);
        match ev.kind {
            MouseKind::Move => {
                if let Some(id) = self.dragging.clone() {
                    if let Some(i) = self.inputs.get_mut(&id) {
                        return FormResult { redraw: i.mouse_move(x), event: None };
                    }
                }
                let h = self.hit_at(x, y).filter(|h| !matches!(h, Hit::Input(_)));
                if h != self.hover {
                    let mut redraw = true;
                    for (id, b) in self.buttons.iter_mut() {
                        redraw |= b.press.set_hover(h == Some(Hit::Button(id.clone())), 0.12);
                    }
                    self.hover = h;
                    return FormResult { redraw, event: None };
                }
                FormResult::default()
            }
            MouseKind::Leave => {
                for b in self.buttons.values_mut() {
                    b.press.set_hover(false, 0.12);
                }
                self.hover = None;
                FormResult::redraw()
            }
            MouseKind::Down(MouseButton::Left) | MouseKind::DoubleClick => {
                let h = self.hit_at(x, y);
                match &h {
                    Some(Hit::Input(id)) => {
                        let id = id.clone();
                        self.set_focus(Some(id.clone()));
                        if let Some(i) = self.inputs.get_mut(&id) {
                            if ev.kind == MouseKind::DoubleClick {
                                i.double_click(x);
                            } else {
                                i.mouse_down(x, false);
                                self.dragging = Some(id);
                            }
                        }
                    }
                    Some(Hit::Button(id)) => {
                        if let Some(b) = self.buttons.get_mut(id) {
                            b.press.pressed = true;
                        }
                    }
                    _ => {}
                }
                if !matches!(h, Some(Hit::Input(_))) {
                    self.set_focus(None);
                }
                self.pressed = h;
                FormResult::redraw()
            }
            MouseKind::Up(MouseButton::Left) => {
                if let Some(id) = self.dragging.take() {
                    if let Some(i) = self.inputs.get_mut(&id) {
                        i.mouse_up();
                    }
                }
                for b in self.buttons.values_mut() {
                    b.press.pressed = false;
                }
                let p = self.pressed.take();
                let h = self.hit_at(x, y);
                if p.is_none() || p != h {
                    return FormResult::redraw();
                }
                match p.unwrap() {
                    Hit::Button(id) => {
                        if let Some(field) = id.strip_prefix("add:") {
                            return self.add(field);
                        }
                        FormResult::event(FormEvent::Click(id))
                    }
                    Hit::Remove(id, i) => FormResult::event(FormEvent::Remove(id, i)),
                    Hit::Check(id) => FormResult::event(FormEvent::Toggle(id)),
                    Hit::Input(_) => FormResult::redraw(),
                }
            }
            _ => FormResult::default(),
        }
    }

    fn add(&mut self, field: &str) -> FormResult {
        let Some(i) = self.inputs.get_mut(field) else { return FormResult::default() };
        let v = i.text().trim().to_string();
        if v.is_empty() {
            return FormResult::redraw();
        }
        i.set_text("");
        FormResult::event(FormEvent::Add(field.to_string(), v))
    }

    fn is_add_row(&self, id: &str) -> bool {
        self.items.iter().any(|it| matches!(it, Item::AddRow { id: i, .. } if i == id))
    }

    pub fn key(&mut self, ev: KeyEvent) -> FormResult {
        let k = ev.key as u16;
        if k == VK_ESCAPE.0 {
            return FormResult::event(FormEvent::Escape);
        }
        if k == VK_TAB.0 && !ev.ctrl && !ev.alt {
            if self.order.is_empty() {
                return FormResult::default();
            }
            let n = self.order.len();
            let cur = self.focus.as_ref().and_then(|f| self.order.iter().position(|o| o == f));
            let next = match (cur, ev.shift) {
                (None, false) => 0,
                (None, true) => n - 1,
                (Some(i), false) => (i + 1) % n,
                (Some(i), true) => (i + n - 1) % n,
            };
            let id = self.order[next].clone();
            self.set_focus(Some(id.clone()));
            if let Some(i) = self.inputs.get_mut(&id) {
                i.select_all();
            }
            return FormResult::redraw();
        }
        let Some(f) = self.focus.clone() else { return FormResult::default() };
        if k == VK_RETURN.0 {
            if self.is_add_row(&f) {
                return self.add(&f);
            }
            return FormResult::default();
        }
        let Some(i) = self.inputs.get_mut(&f) else { return FormResult::default() };
        let r = i.key(ev);
        let text = i.text();
        let event = (r.changed && !self.is_add_row(&f)).then(|| FormEvent::Edited(f, text));
        FormResult { redraw: r.redraw || r.handled, event }
    }

    pub fn char_input(&mut self, c: char) -> FormResult {
        let Some(f) = self.focus.clone() else { return FormResult::default() };
        if c == '\r' || c == '\t' || c == '\u{1b}' {
            return FormResult::default();
        }
        let Some(i) = self.inputs.get_mut(&f) else { return FormResult::default() };
        let r = i.char_input(c);
        let text = i.text();
        let event = (r.changed && !self.is_add_row(&f)).then(|| FormEvent::Edited(f, text));
        FormResult { redraw: r.redraw, event }
    }

    /// Нажатая кнопка формы ещё анимируется (переход наведения).
    pub fn animating(&self, now: Instant) -> bool {
        self.buttons.values().any(|b| b.press.animating(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_keep_their_text_across_rebuilds_and_new_ones_take_the_value() {
        let mut f = Form::new(0, false);
        f.set_items(vec![Item::Input { id: "a".into(), placeholder: String::new(), value: "one".into() }]);
        f.set_value("a", "edited");
        f.set_items(vec![
            Item::Input { id: "a".into(), placeholder: String::new(), value: "one".into() },
            Item::Input { id: "b".into(), placeholder: String::new(), value: "two".into() },
        ]);
        assert_eq!(f.value("a").as_deref(), Some("edited"));
        assert_eq!(f.value("b").as_deref(), Some("two"));
        f.set_items(vec![]);
        assert_eq!(f.value("a"), None);
    }

    #[test]
    fn enter_in_an_add_row_adds_the_trimmed_text_and_clears_the_field() {
        let mut f = Form::new(0, false);
        f.set_items(vec![Item::AddRow { id: "w".into(), placeholder: String::new(), button: "+".into() }]);
        f.focus_input("w");
        for c in " hi ".chars() {
            f.char_input(c);
        }
        let r = f.key(KeyEvent { key: VK_RETURN.0 as u32, ctrl: false, shift: false, alt: false, repeat: false });
        assert_eq!(r.event, Some(FormEvent::Add("w".into(), "hi".into())));
        assert_eq!(f.value("w").as_deref(), Some(""));
    }

    #[test]
    fn typing_in_a_plain_input_reports_the_edit() {
        let mut f = Form::new(0, false);
        f.set_items(vec![Item::Input { id: "u".into(), placeholder: String::new(), value: "ab".into() }]);
        f.focus_input("u");
        let r = f.char_input('c');
        assert_eq!(r.event, Some(FormEvent::Edited("u".into(), "abc".into())));
    }
}
