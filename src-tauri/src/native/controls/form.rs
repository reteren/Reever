//! Форма скина сверху вниз: группы-рамки с легендой (fieldset.grp), подсказки (.hint),
//! подписи полей (.fld), фишки с крестиком (.grammar-chip), поле с кнопкой «Добавить»
//! (.editrow), поля ввода, флажки (.cbrow), ряды кнопок и список ошибок. Окно описывает форму
//! списком [`Item`] (заново после каждой правки), форма раскладывает и рисует его в заданной
//! колонке и превращает мышь и клавиатуру в [`FormEvent`]. Состояние полей ввода (каретка,
//! выделение, отмена) живёт в форме по id и переживает пересборку списка.
//!
//! Клавиатура: Tab и Shift+Tab ходят по всем элементам по порядку (поля, кнопки, флажки,
//! списки, ползунки, крестики фишек, ссылки), Enter и пробел нажимают, стрелки листают список
//! и двигают ползунок; рамка фокуса — пунктиром, как :focus-visible. Для экранного диктора
//! форма отдаёт свои элементы узлами UI Automation (`a11y`) и принимает его действия.
//!
//! Первым пользуется редактор «Что можно сказать» главного окна; дальше — «Действия» и
//! «Настройки».

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP};

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// все элементы по порядку для Tab и диктора, их подписи
    nav: Vec<Hit>,
    labels: HashMap<Hit, String>,
    /// фокус клавиатуры не на поле (кнопка, флажок, список…) и видна ли рамка фокуса
    kfocus: Option<Hit>,
    focus_visible: bool,
    /// пробел нажал кнопку — следующий символ (тот же пробел) не печатать
    swallow_char: bool,
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
        let (nav, labels) = Self::navigation(&items);
        // фокус на исчезнувшем (убрали фишку) — на соседа слева, иначе снять
        if let Some(k) = self.kfocus.clone() {
            if !nav.contains(&k) {
                self.kfocus = match k {
                    Hit::Remove(id, i) if i > 0 && nav.contains(&Hit::Remove(id.clone(), i - 1)) => Some(Hit::Remove(id, i - 1)),
                    _ => None,
                };
            }
        }
        self.nav = nav;
        self.labels = labels;
        self.items = items;
    }

    /// Порядок элементов для Tab и их подписи (для диктора).
    fn navigation(items: &[Item]) -> (Vec<Hit>, HashMap<Hit, String>) {
        let mut nav = Vec::new();
        let mut labels = HashMap::new();
        let mut last = String::new();
        for it in items {
            match it {
                Item::Label(t) => last = t.clone(),
                Item::Chips { id, values, .. } => {
                    for (i, v) in values.iter().enumerate() {
                        let h = Hit::Remove(id.clone(), i);
                        labels.insert(h.clone(), format!("✕ {v}"));
                        nav.push(h);
                    }
                }
                Item::AddRow { id, placeholder, button } => {
                    let h = Hit::Input(id.clone());
                    labels.insert(h.clone(), placeholder.clone());
                    nav.push(h);
                    let b = Hit::Button(format!("add:{id}"));
                    labels.insert(b.clone(), button.clone());
                    nav.push(b);
                }
                Item::Input { id, placeholder, .. } => {
                    let h = Hit::Input(id.clone());
                    labels.insert(h.clone(), if last.is_empty() { placeholder.clone() } else { last.clone() });
                    nav.push(h);
                }
                Item::InputSelect { id, placeholder, select, .. } => {
                    let h = Hit::Input(id.clone());
                    labels.insert(h.clone(), placeholder.clone());
                    nav.push(h);
                    let sh = Hit::Select(select.clone());
                    labels.insert(sh.clone(), placeholder.clone());
                    nav.push(sh);
                }
                Item::Check { id, label, .. } => {
                    let h = Hit::Check(id.clone());
                    labels.insert(h.clone(), label.clone());
                    nav.push(h);
                }
                Item::Buttons { buttons, .. } => {
                    for (id, label, _) in buttons.iter().filter(|(id, _, _)| !id.starts_with('!')) {
                        let h = Hit::Button(id.clone());
                        labels.insert(h.clone(), label.clone());
                        nav.push(h);
                    }
                }
                Item::Select { id, .. } => {
                    let h = Hit::Select(id.clone());
                    labels.insert(h.clone(), last.clone());
                    nav.push(h);
                }
                Item::Slider { id, .. } => {
                    let h = Hit::Slider(id.clone());
                    labels.insert(h.clone(), last.clone());
                    nav.push(h);
                }
                Item::Link { id, link, .. } => {
                    let h = Hit::Link(id.clone());
                    labels.insert(h.clone(), link.clone());
                    nav.push(h);
                }
                Item::LangRow { id, name, removable: true, .. } => {
                    let h = Hit::Remove(id.clone(), 0);
                    labels.insert(h.clone(), format!("✕ {name}"));
                    nav.push(h);
                }
                _ => {}
            }
        }
        (nav, labels)
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
        self.focus.is_some() || self.kfocus.is_some()
    }

    /// Снять фокус клавиатуры (Tab ушёл из формы дальше по окну).
    pub fn blur(&mut self) {
        self.set_focus(None);
        self.kfocus = None;
        self.open_select = None;
    }

    /// Поставить фокус на первый (или последний — `last`) элемент формы с клавиатуры.
    pub fn focus_edge(&mut self, last: bool) -> bool {
        let Some(h) = (if last { self.nav.last() } else { self.nav.first() }).cloned() else { return false };
        self.focus_visible = true;
        self.focus_hit(h);
        true
    }

    /// Где элемент в фокусе (координаты последнего рисования) — чтобы прокрутить к нему.
    pub fn focus_rect(&self) -> Option<Rect> {
        let cur = self.current()?;
        self.hits.iter().find(|(_, h)| *h == cur).map(|(r, _)| *r)
    }

    /// Фокус на последнем (`last`) или первом элементе — Tab дальше уходит из формы.
    pub fn at_edge(&self, last: bool) -> bool {
        let Some(cur) = self.current() else { return true };
        let edge = if last { self.nav.last() } else { self.nav.first() };
        edge == Some(&cur)
    }

    fn focus_hit(&mut self, h: Hit) {
        if let Hit::Input(id) = &h {
            let id = id.clone();
            self.kfocus = None;
            self.set_focus(Some(id.clone()));
            if let Some(i) = self.inputs.get_mut(&id) {
                i.select_all();
            }
        } else {
            self.set_focus(None);
            self.kfocus = Some(h);
        }
    }

    fn current(&self) -> Option<Hit> {
        self.focus.clone().map(Hit::Input).or_else(|| self.kfocus.clone())
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
        // :focus-visible — пунктир вокруг элемента в фокусе (у полей своя рамка)
        if self.focus_visible && self.window_focused {
            if let Some(k) = self.kfocus.clone() {
                if let Some((r, _)) = self.hits.iter().find(|(_, h)| *h == k) {
                    let ring = Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
                    stroke_round(cx, ring, 6.0, ACC_2, 1.0, true);
                }
            }
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
                self.focus_visible = false;
                self.kfocus = None;
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
            if self.nav.is_empty() {
                return FormResult::default();
            }
            self.open_select = None;
            self.focus_visible = true;
            let n = self.nav.len();
            let cur = self.current().and_then(|f| self.nav.iter().position(|o| *o == f));
            let next = match (cur, ev.shift) {
                (None, false) => 0,
                (None, true) => n - 1,
                (Some(i), false) => (i + 1) % n,
                (Some(i), true) => (i + n - 1) % n,
            };
            let h = self.nav[next].clone();
            self.focus_hit(h);
            return FormResult::redraw();
        }
        // открытый список: стрелки по вариантам, Enter — выбрать
        if let Some(sel) = self.open_select.clone() {
            let opts = self.options(&sel);
            let cur = match &self.hover {
                Some(Hit::Option(_, v)) => opts.iter().position(|o| o == v),
                _ => None,
            };
            if k == VK_DOWN.0 || k == VK_UP.0 {
                let n = opts.len().max(1);
                let i = match (cur, k == VK_DOWN.0) {
                    (None, _) => 0,
                    (Some(i), true) => (i + 1) % n,
                    (Some(i), false) => (i + n - 1) % n,
                };
                if let Some(v) = opts.get(i) {
                    self.hover = Some(Hit::Option(sel, v.clone()));
                }
                return FormResult::redraw();
            }
            if k == VK_RETURN.0 || k == VK_SPACE.0 {
                self.swallow_char = k == VK_SPACE.0;
                self.open_select = None;
                if let (Some(i), true) = (cur, true) {
                    return FormResult::event(FormEvent::Select(sel, opts[i].clone()));
                }
                return FormResult::redraw();
            }
        }
        if let Some(h) = self.kfocus.clone() {
            return self.key_on(h, ev);
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
        if std::mem::take(&mut self.swallow_char) {
            return FormResult::default();
        }
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

    /// Значения вариантов выпадающего списка `id`.
    fn options(&self, id: &str) -> Vec<String> {
        for it in &self.items {
            match it {
                Item::Select { id: i, options, .. } | Item::InputSelect { select: i, options, .. } if i == id => {
                    return options.iter().map(|(v, _)| v.clone()).collect();
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn selected_of(&self, id: &str) -> Option<String> {
        self.items.iter().find_map(|it| match it {
            Item::Select { id: i, selected, .. } | Item::InputSelect { select: i, selected, .. } if i == id => Some(selected.clone()),
            _ => None,
        })
    }

    fn slider_of(&self, id: &str) -> Option<(f32, f32, f32, f32)> {
        self.items.iter().find_map(|it| match it {
            Item::Slider { id: i, min, max, step, value, .. } if i == id => Some((*min, *max, *step, *value)),
            _ => None,
        })
    }

    /// Нажать элемент (Enter, пробел, диктор): то же, что щелчок.
    fn activate(&mut self, h: Hit) -> FormResult {
        match h {
            Hit::Button(id) => {
                if let Some(field) = id.strip_prefix("add:") {
                    return self.add(field);
                }
                FormResult::event(FormEvent::Click(id))
            }
            Hit::Remove(id, i) => FormResult::event(FormEvent::Remove(id, i)),
            Hit::Check(id) => FormResult::event(FormEvent::Toggle(id)),
            Hit::Link(id) => FormResult::event(FormEvent::Click(id)),
            Hit::Select(id) => {
                let sel = self.selected_of(&id);
                self.hover = sel.map(|v| Hit::Option(id.clone(), v));
                self.open_select = Some(id);
                FormResult::redraw()
            }
            Hit::Input(id) => {
                self.focus_hit(Hit::Input(id));
                FormResult::redraw()
            }
            _ => FormResult::default(),
        }
    }

    /// Клавиша на элементе, который не поле ввода.
    fn key_on(&mut self, h: Hit, ev: KeyEvent) -> FormResult {
        let k = ev.key as u16;
        if k == VK_RETURN.0 || k == VK_SPACE.0 {
            self.swallow_char = k == VK_SPACE.0;
            return self.activate(h);
        }
        let dir: i32 = if k == VK_DOWN.0 || k == VK_RIGHT.0 {
            1
        } else if k == VK_UP.0 || k == VK_LEFT.0 {
            -1
        } else {
            0
        };
        match h {
            // закрытый список: стрелки сразу меняют выбор
            Hit::Select(id) if dir != 0 => {
                let opts = self.options(&id);
                let cur = self.selected_of(&id).and_then(|v| opts.iter().position(|o| *o == v)).unwrap_or(0);
                let next = (cur as i32 + dir).clamp(0, opts.len() as i32 - 1) as usize;
                match opts.get(next) {
                    Some(v) if next != cur => FormResult::event(FormEvent::Select(id, v.clone())),
                    _ => FormResult::default(),
                }
            }
            Hit::Slider(id) => {
                let Some((min, max, step, value)) = self.slider_of(&id) else { return FormResult::default() };
                let v = if k == VK_HOME.0 {
                    min
                } else if k == VK_END.0 {
                    max
                } else if dir != 0 {
                    (value + step.max(1e-3) * dir as f32).clamp(min, max)
                } else {
                    return FormResult::default();
                };
                FormResult::event(FormEvent::SlideEnd(id, v))
            }
            _ => FormResult::default(),
        }
    }

    /// Элементы формы для экранного диктора: узлы с id от `base` (по порядку Tab), подписи и
    /// тексты (подсказки, ошибки) — от `base + 5000`. Возвращает узлы, детей по порядку и узел
    /// в фокусе.
    pub fn a11y(&self, base: u64, scale: f32) -> (Vec<(accesskit::NodeId, accesskit::Node)>, Vec<accesskit::NodeId>, Option<accesskit::NodeId>) {
        use accesskit::{Action, Node, NodeId, Role, Toggled};
        let b = |r: Rect| crate::native::a11y::bounds(r.x, r.y, r.w, r.h, scale);
        let rect_of = |h: &Hit| self.hits.iter().find(|(_, x)| x == h).map(|(r, _)| *r);
        let mut nodes = Vec::new();
        let mut kids = Vec::new();
        let mut text_id = base + 5000;
        let mut push_text = |nodes: &mut Vec<(NodeId, Node)>, kids: &mut Vec<NodeId>, t: &str| {
            if t.trim().is_empty() {
                return;
            }
            let mut n = Node::new(Role::Label);
            n.set_value(t.to_string());
            nodes.push((NodeId(text_id), n));
            kids.push(NodeId(text_id));
            text_id += 1;
        };
        let mut nav_i = 0usize;
        for it in &self.items {
            match it {
                Item::GroupBegin { legend, .. } => push_text(&mut nodes, &mut kids, legend),
                Item::Note(t) | Item::Help { text: t, .. } => push_text(&mut nodes, &mut kids, t),
                Item::Errors(list) => {
                    for e in list {
                        push_text(&mut nodes, &mut kids, e);
                    }
                }
                _ => {}
            }
            // элементы этого пункта — следующие в nav
            let count = Self::navigation(std::slice::from_ref(it)).0.len();
            for h in self.nav.iter().skip(nav_i).take(count) {
                let id = NodeId(base + nav_i as u64);
                nav_i += 1;
                let label = self.labels.get(h).cloned().unwrap_or_default();
                let mut n = match h {
                    Hit::Input(fid) => {
                        let mut n = Node::new(Role::TextInput);
                        n.set_value(self.value(fid).unwrap_or_default());
                        n.add_action(Action::SetValue);
                        n
                    }
                    Hit::Check(cid) => {
                        let on = self.items.iter().any(|it| matches!(it, Item::Check { id, on: true, .. } if id == cid));
                        let mut n = Node::new(Role::CheckBox);
                        n.set_toggled(if on { Toggled::True } else { Toggled::False });
                        n.add_action(Action::Click);
                        n
                    }
                    Hit::Select(sid) => {
                        let mut n = Node::new(Role::ComboBox);
                        let sel = self.selected_of(sid).unwrap_or_default();
                        let shown = self.items.iter().find_map(|it| match it {
                            Item::Select { id, options, .. } | Item::InputSelect { select: id, options, .. } if id == sid => {
                                options.iter().find(|(v, _)| *v == sel).map(|(_, l)| l.clone())
                            }
                            _ => None,
                        });
                        n.set_value(shown.unwrap_or(sel));
                        n.add_action(Action::Click);
                        n
                    }
                    Hit::Slider(sid) => {
                        let mut n = Node::new(Role::Slider);
                        if let Some((min, max, step, value)) = self.slider_of(sid) {
                            n.set_numeric_value(value as f64);
                            n.set_min_numeric_value(min as f64);
                            n.set_max_numeric_value(max as f64);
                            n.set_numeric_value_step(step as f64);
                        }
                        let text = self.items.iter().find_map(|it| match it {
                            Item::Slider { id, text, .. } if id == sid => Some(text.clone()),
                            _ => None,
                        });
                        if let Some(t) = text {
                            n.set_value(t);
                        }
                        n.add_action(Action::Increment);
                        n.add_action(Action::Decrement);
                        n
                    }
                    Hit::Link(_) => {
                        let mut n = Node::new(Role::Link);
                        n.add_action(Action::Click);
                        n
                    }
                    _ => {
                        let mut n = Node::new(Role::Button);
                        n.add_action(Action::Click);
                        n
                    }
                };
                n.set_label(label);
                n.add_action(Action::Focus);
                if let Some(r) = rect_of(h) {
                    n.set_bounds(b(r));
                }
                nodes.push((id, n));
                kids.push(id);
            }
        }
        let focus = self.current().and_then(|f| self.nav.iter().position(|h| *h == f)).map(|i| NodeId(base + i as u64));
        (nodes, kids, focus)
    }

    /// Действие диктора над элементом формы (id из `a11y`). None — узел не формы.
    pub fn a11y_action(&mut self, base: u64, req: &accesskit::ActionRequest) -> Option<FormResult> {
        use accesskit::{Action, ActionData};
        let i = req.target_node.0.checked_sub(base)? as usize;
        let h = self.nav.get(i)?.clone();
        Some(match req.action {
            Action::Focus => {
                self.focus_visible = true;
                self.focus_hit(h);
                FormResult::redraw()
            }
            Action::Click => self.activate(h),
            Action::SetValue => match (&h, &req.data) {
                (Hit::Input(id), Some(ActionData::Value(v))) => {
                    let id = id.clone();
                    self.set_value(&id, v);
                    if self.is_add_row(&id) {
                        FormResult::redraw()
                    } else {
                        FormResult::event(FormEvent::Edited(id, v.to_string()))
                    }
                }
                _ => FormResult::default(),
            },
            Action::Increment | Action::Decrement => {
                let key = if req.action == Action::Increment { VK_RIGHT.0 } else { VK_LEFT.0 };
                self.key_on(h, KeyEvent { key: key as u32, ctrl: false, shift: false, alt: false, repeat: false })
            }
            _ => FormResult::default(),
        })
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

    fn key(k: u16, shift: bool) -> KeyEvent {
        KeyEvent { key: k as u32, ctrl: false, shift, alt: false, repeat: false }
    }

    #[test]
    fn tab_walks_every_control_and_space_presses_without_typing() {
        let mut f = Form::new(0, false);
        f.set_items(vec![
            Item::Input { id: "a".into(), placeholder: String::new(), value: String::new() },
            Item::Check { id: "c".into(), label: "C".into(), on: false },
            Item::Buttons { buttons: vec![("ok".into(), "OK".into(), ButtonKind::Default)], top: 0.0, bottom: 0.0 },
        ]);
        f.key(key(VK_TAB.0, false));
        assert_eq!(f.current(), Some(Hit::Input("a".into())));
        f.key(key(VK_TAB.0, false));
        let r = f.key(key(VK_SPACE.0, false));
        assert_eq!(r.event, Some(FormEvent::Toggle("c".into())));
        assert!(f.char_input(' ').event.is_none());
        f.key(key(VK_TAB.0, false));
        assert_eq!(f.key(key(VK_RETURN.0, false)).event, Some(FormEvent::Click("ok".into())));
        f.key(key(VK_TAB.0, true));
        assert_eq!(f.current(), Some(Hit::Check("c".into())));
    }

    #[test]
    fn arrows_change_a_closed_select_and_step_a_slider() {
        let mut f = Form::new(0, false);
        let opts = vec![("0".to_string(), "Zero".to_string()), ("60".to_string(), "Sixty".to_string())];
        f.set_items(vec![
            Item::Select { id: "fps".into(), options: opts, selected: "0".into() },
            Item::Slider { id: "s".into(), min: 1.0, max: 5.0, step: 1.0, value: 3.0, text: "3 / 5".into() },
        ]);
        f.key(key(VK_TAB.0, false));
        assert_eq!(f.key(key(VK_DOWN.0, false)).event, Some(FormEvent::Select("fps".into(), "60".into())));
        f.key(key(VK_TAB.0, false));
        assert_eq!(f.key(key(VK_RIGHT.0, false)).event, Some(FormEvent::SlideEnd("s".into(), 4.0)));
        assert_eq!(f.key(key(VK_HOME.0, false)).event, Some(FormEvent::SlideEnd("s".into(), 1.0)));
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
