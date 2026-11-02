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
use super::{data, fill_round, inside, lcd, mix, stroke_round, ui, with_alpha, Glow, Scene, ACC, ACC_2, ACC_3, ACC_4, ACC_5, ACC_HOT};
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
    /// Поле ввода и выпадающий список справа от него (select шириной по тексту):
    /// выбор — [`FormEvent::Select`].
    InputSelect { id: String, placeholder: String, value: String, select: String, options: Vec<(String, String)>, selected: String },
    /// Выпадающий список на всю ширину колонки.
    Select { id: String, options: Vec<(String, String)>, selected: String },
    /// Пояснение к настройке (.setting-help); `indent` — под флажком (отступ 25, ближе к нему).
    Help { text: String, indent: bool },
    /// Ползунок и подпись значения справа (.mascot-scale): тянут — [`FormEvent::Slide`],
    /// отпустили — [`FormEvent::SlideEnd`].
    Slider { id: String, min: f32, max: f32, step: f32, value: f32, text: String },
    /// Строка текста со ссылкой посередине (.hint с <a>): щелчок — [`FormEvent::Click`].
    Link { id: String, prefix: String, link: String, suffix: String },
    /// Отступ по вертикали.
    Gap(f32),
    /// Строка языка (.lang-row): название шириной 90, пометка (пакет, «всегда»; `missing` —
    /// ярче, как .lang-missing) и крестик — [`FormEvent::Remove`] с индексом 0.
    LangRow { id: String, name: String, note: String, missing: bool, removable: bool },
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
    Select(String, String),
    Slide(String, f32),
    SlideEnd(String, f32),
    Escape,
}

#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Button(String),
    Remove(String, usize),
    Check(String),
    Input(String),
    /// выпадающий список (открыть/закрыть) и его вариант
    Select(String),
    Option(String, String),
    /// ползунок: id, левый край и ширина дорожки, min, max, step
    Slider(String),
    Link(String),
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
    /// открытый выпадающий список
    open_select: Option<String>,
    /// дорожки ползунков (id → x, ширина, min, max, step) и какой тянут
    sliders: HashMap<String, (f32, f32, f32, f32, f32)>,
    sliding: Option<String>,
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
        if self.open_select.as_ref().is_some_and(|o| {
            !items.iter().any(|it| matches!(it, Item::InputSelect { select: id, .. } | Item::Select { id, .. } if id == o))
        }) {
            self.open_select = None;
        }
        let mut keep = std::collections::HashSet::new();
        self.order.clear();
        for it in &items {
            let (id, placeholder, value) = match it {
                Item::AddRow { id, placeholder, .. } => (id, placeholder, ""),
                Item::Input { id, placeholder, value } => (id, placeholder, value.as_str()),
                Item::InputSelect { id, placeholder, value, .. } => (id, placeholder, value.as_str()),
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
        // открытый выпадающий список рисуется поверх всего, после раскладки
        let mut popup: Option<(String, Rect, Vec<(String, String)>, String)> = None;
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
                Item::InputSelect { id, select, options, selected, .. } => {
                    let st = input_style();
                    let label = options.iter().find(|(v, _)| v == selected).map_or(selected.clone(), |(_, l)| l.clone());
                    let tw = scene.cache.get(cx.text(), &label, &st, 10_000.0).map_or(60.0, |l| l.width);
                    let sw = (tw + 9.0 + 26.0 + 2.0).min(cw * 0.6);
                    let sr = Rect::new(cx0 + cw - sw, y, sw, 28.0);
                    if let Some(inp) = self.inputs.get_mut(id) {
                        inp.rect = Rect::new(cx0, y, (cw - sw - 8.0).max(40.0), 28.0);
                        self.hits.push((inp.rect, Hit::Input(id.clone())));
                    }
                    let open = self.open_select.as_deref() == Some(select.as_str());
                    fill_round(cx, sr, 5.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 });
                    stroke_round(cx, sr, 5.0, if open { ACC_2 } else { ACC_4 }, 1.0, false);
                    if let Some(l) = scene.cache.get(cx.text(), &label, &st, sw - 9.0 - 26.0) {
                        scene.text(&l, sr.x + 9.0, sr.y + (28.0 - l.height) / 2.0, ACC_HOT, Glow::G1);
                    }
                    scene.icon("chev", Rect::new(sr.x + sw - 22.0, sr.y + 6.0, 16.0, 16.0), 2.0, ACC, false, Glow::None);
                    self.hits.push((sr, Hit::Select(select.clone())));
                    if open {
                        popup = Some((select.clone(), sr, options.clone(), selected.clone()));
                    }
                    y += 28.0;
                }
                Item::Select { id, options, selected } => {
                    let st = input_style();
                    let label = options.iter().find(|(v, _)| v == selected).map_or(selected.clone(), |(_, l)| l.clone());
                    let sr = Rect::new(cx0, y, cw, 28.0);
                    let open = self.open_select.as_deref() == Some(id.as_str());
                    fill_round(cx, sr, 5.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 });
                    stroke_round(cx, sr, 5.0, if open { ACC_2 } else { ACC_4 }, 1.0, false);
                    if let Some(l) = scene.cache.get(cx.text(), &label, &TextStyle { ellipsis: true, ..st }, cw - 9.0 - 26.0) {
                        scene.text(&l, sr.x + 9.0, sr.y + (28.0 - l.height) / 2.0, ACC_HOT, Glow::G1);
                    }
                    scene.icon("chev", Rect::new(sr.x + cw - 22.0, sr.y + 6.0, 16.0, 16.0), 2.0, ACC, false, Glow::None);
                    self.hits.push((sr, Hit::Select(id.clone())));
                    if open {
                        popup = Some((id.clone(), sr, options.clone(), selected.clone()));
                    }
                    y += 28.0;
                }
                Item::Help { text, indent } => {
                    if text.is_empty() {
                        continue;
                    }
                    let (dx, top) = if *indent { (25.0, -4.0) } else { (0.0, 4.0) };
                    y += top;
                    let st = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
                    if let Some(l) = scene.cache.get(cx.text(), text, &st, (cw - dx).max(40.0)) {
                        scene.text(&l, cx0 + dx, y, ACC_3, Glow::G1);
                        y += l.height;
                    }
                    y += 6.0;
                }
                Item::Slider { id, min, max, step, value, text } => {
                    let st = data(12.0, 1.0);
                    let lw = scene.cache.get(cx.text(), text, &st, 10_000.0).map_or(44.0, |l| l.width).max(44.0);
                    let tw = (cw - 12.0 - lw).max(40.0);
                    // дорожка 4 px, ручка 14×14 (ход ручки — ширина дорожки минус ручка)
                    let track = Rect::new(cx0, y + 12.0, tw, 4.0);
                    fill_round(cx, track, 2.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 });
                    stroke_round(cx, track, 2.0, ACC_4, 1.0, false);
                    let k = if max > min { ((value - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 };
                    let thumb = Rect::new(cx0 + (tw - 14.0) * k, y + 7.0, 14.0, 14.0);
                    scene.shadow(cx, thumb, 3.0, ACC, 8.0);
                    fill_round(cx, thumb, 3.0, ACC);
                    if let Some(l) = scene.cache.get(cx.text(), text, &st, 10_000.0) {
                        scene.text(&l, cx0 + cw - l.width, y + (28.0 - l.height) / 2.0, ACC_HOT, Glow::G1);
                    }
                    let hit = Rect::new(cx0, y, tw, 28.0);
                    self.sliders.insert(id.clone(), (cx0 + 7.0, tw - 14.0, *min, *max, *step));
                    self.hits.push((hit, Hit::Slider(id.clone())));
                    y += 28.0;
                }
                Item::Link { id, prefix, link, suffix } => {
                    let st = ui(12.0, 400, 1.45);
                    let mut px = cx0;
                    let mut h = 17.4f32;
                    for (i, part) in [prefix, link, suffix].into_iter().enumerate() {
                        if part.is_empty() {
                            continue;
                        }
                        let Some(l) = scene.cache.get(cx.text(), part, &st, 10_000.0) else { continue };
                        if i == 1 {
                            let hit = Hit::Link(id.clone());
                            let hot = self.hover.as_ref() == Some(&hit);
                            scene.text(&l, px, y, ACC_HOT, if hot { Glow::G2 } else { Glow::G1 });
                            cx.fill_rect(Rect::new(px, y + l.baseline() + 3.0, l.width, 1.0), if hot { ACC_HOT } else { ACC_3 });
                            self.hits.push((Rect::new(px, y, l.width, l.height), hit));
                        } else {
                            scene.text(&l, px, y, ACC_3, Glow::G1);
                        }
                        px += l.width;
                        h = h.max(l.height);
                    }
                    y += h;
                }
                Item::Gap(g) => y += g,
                Item::LangRow { id, name, note, missing, removable } => {
                    let st = ui(13.0, 400, 1.2);
                    let h = 22.0;
                    if let Some(l) = scene.cache.get(cx.text(), name, &st, 90.0) {
                        scene.text(&l, cx0, y + (h - l.height) / 2.0, ACC, Glow::G1);
                    }
                    let mut px = cx0 + 90.0 + 8.0;
                    if !note.is_empty() {
                        if let Some(l) = scene.cache.get(cx.text(), note, &ui(11.0, 400, 1.2), 10_000.0) {
                            scene.text(&l, px, y + (h - l.height) / 2.0, if *missing { ACC_HOT } else { ACC_3 }, Glow::G1);
                            px += l.width + 8.0;
                        }
                    }
                    if *removable {
                        let xr = Rect::new(px, y + 2.0, 18.0, 18.0);
                        let hit = Hit::Remove(id.clone(), 0);
                        let hot = self.hover.as_ref() == Some(&hit);
                        scene.icon("x", Rect::new(xr.x + 2.0, xr.y + 2.0, 14.0, 14.0), 1.6, if hot { ACC_HOT } else { ACC }, false, if hot { Glow::G3 } else { Glow::G2 });
                        self.hits.push((xr, hit));
                    }
                    y += h + 6.0;
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
                        if !id.starts_with('!') {
                            self.hits.push((br, Hit::Button(id.clone())));
                        }
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
        let mut shown: Vec<String> = self
            .hits
            .iter()
            .filter_map(|(_, h)| if let Hit::Button(b) = h { Some(b.clone()) } else { None })
            .collect();
        // неактивные (button:disabled { opacity: .35 })
        for it in &items {
            if let Item::Buttons { buttons, .. } = it {
                shown.extend(buttons.iter().filter(|(id, _, _)| id.starts_with('!')).map(|(id, _, _)| id.clone()));
            }
        }
        for id in shown {
            if let Some(b) = self.buttons.get(&id) {
                if id.starts_with('!') {
                    scene.flush(cx);
                    if super::push_round_layer(cx, Rect::new(b.rect.x - 2.0, b.rect.y - 2.0, b.rect.w + 4.0, b.rect.h + 4.0), 0.0, 0.35) {
                        b.draw(scene, cx);
                        scene.flush(cx);
                        super::pop_layer(cx);
                    }
                } else {
                    b.draw(scene, cx);
                }
            }
        }
        scene.flush(cx);
        if let Some((id, sr, options, selected)) = popup {
            // ::picker(select): фон #04140F, рамка acc-4, тень; наведённый вариант — заливка acc
            let st = TextStyle { wrap: false, ..ui(13.0, 400, 1.2) };
            let w = options
                .iter()
                .filter_map(|(_, l)| scene.cache.get(cx.text(), l, &st, 10_000.0).map(|l| l.width))
                .fold(sr.w - 8.0, f32::max)
                + 18.0
                + 8.0;
            let row_h = 6.0 + 15.6 + 6.0;
            let pr = Rect::new(sr.x + sr.w - w, sr.y + sr.h + 4.0, w, 8.0 + row_h * options.len() as f32);
            scene.shadow(cx, pr, 6.0, Color::rgba(39, 230, 170, 41), 18.0);
            fill_round(cx, pr, 6.0, Color::rgb(0x04, 0x14, 0x0F));
            stroke_round(cx, pr, 6.0, ACC_4, 1.0, false);
            for (i, (v, l)) in options.iter().enumerate() {
                let r = Rect::new(pr.x + 4.0, pr.y + 4.0 + row_h * i as f32, pr.w - 8.0, row_h);
                let hit = Hit::Option(id.clone(), v.clone());
                let hot = self.hover.as_ref() == Some(&hit);
                if hot {
                    scene.shadow(cx, r, 4.0, Color::rgba(39, 230, 170, 89), 12.0);
                    fill_round(cx, r, 4.0, ACC);
                }
                if let Some(lay) = scene.cache.get(cx.text(), l, &st, 10_000.0) {
                    let (c, g) = if hot {
                        (super::GLASS, Glow::None)
                    } else if *v == selected {
                        (ACC_HOT, Glow::G2)
                    } else {
                        (ACC, Glow::G1)
                    };
                    scene.text(&lay, r.x + 9.0, r.y + (row_h - lay.height) / 2.0, c, g);
                }
                self.hits.push((r, hit));
            }
            scene.flush(cx);
        }
        let _ = (mix, with_alpha, inside);
        y - y0
    }

    /// Открыт выпадающий список (щелчок мимо — закрыть).
    pub fn popup_open(&self) -> bool {
        self.open_select.is_some()
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|(_, h)| h.clone())
    }

    /// Курсор над чем-то кликабельным (рука) или над полем (I-образный — решает окно).
    pub fn hover_kind(&self, x: f32, y: f32) -> Option<bool> {
        if self.open_select.is_some() {
            return Some(true);
        }
        self.hit_at(x, y).map(|h| !matches!(h, Hit::Input(_)))
    }

    /// Мышь в координатах формы. `None` от `hit` — щелчок мимо формы снимает фокус поля.
    pub fn mouse(&mut self, ev: MouseEvent) -> FormResult {
        let (x, y) = (ev.x, ev.y);
        match ev.kind {
            MouseKind::Move if self.sliding.is_some() => {
                let id = self.sliding.clone().unwrap_or_default();
                match self.slider_value(&id, x) {
                    Some(v) => FormResult::event(FormEvent::Slide(id, v)),
                    None => FormResult::default(),
                }
            }
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
            MouseKind::Down(MouseButton::Left) | MouseKind::DoubleClick if self.open_select.is_some() => {
                // открытый список: вариант — выбрать, всё остальное — закрыть
                let h = self.hit_at(x, y);
                let open = self.open_select.take();
                if let (Some(Hit::Option(id, v)), Some(_)) = (h, open) {
                    self.pressed = None;
                    return FormResult::event(FormEvent::Select(id, v));
                }
                self.pressed = Some(Hit::Select(String::new()));
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
                    Some(Hit::Slider(id)) => {
                        let id = id.clone();
                        self.set_focus(None);
                        self.sliding = Some(id.clone());
                        self.pressed = None;
                        if let Some(v) = self.slider_value(&id, x) {
                            return FormResult::event(FormEvent::Slide(id, v));
                        }
                        return FormResult::redraw();
                    }
                    _ => {}
                }
                if !matches!(h, Some(Hit::Input(_))) {
                    self.set_focus(None);
                }
                self.pressed = h;
                FormResult::redraw()
            }
            MouseKind::Up(MouseButton::Left) if self.sliding.is_some() => {
                let id = self.sliding.take().unwrap_or_default();
                match self.slider_value(&id, x) {
                    Some(v) => FormResult::event(FormEvent::SlideEnd(id, v)),
                    None => FormResult::redraw(),
                }
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
                    Hit::Select(id) => {
                        if !id.is_empty() {
                            self.open_select = Some(id);
                        }
                        FormResult::redraw()
                    }
                    Hit::Option(..) | Hit::Slider(_) => FormResult::redraw(),
                    Hit::Link(id) => FormResult::event(FormEvent::Click(id)),
                }
            }
            _ => FormResult::default(),
        }
    }

    /// Значение ползунка под x (с шагом и в пределах).
    fn slider_value(&self, id: &str, x: f32) -> Option<f32> {
        let (x0, w, min, max, step) = *self.sliders.get(id)?;
        let k = if w > 0.0 { ((x - x0) / w).clamp(0.0, 1.0) } else { 0.0 };
        let v = min + (max - min) * k;
        let v = if step > 0.0 { min + ((v - min) / step).round() * step } else { v };
        Some(v.clamp(min, max))
    }

    /// Тянут ползунок (окно держит мышь у себя).
    pub fn sliding(&self) -> bool {
        self.sliding.is_some()
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
            if self.open_select.take().is_some() {
                return FormResult::redraw();
            }
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
