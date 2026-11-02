//! Нативное окно выбора «ривер найди …» — перенос frontend/picker.html: заголовок с запросом,
//! поле поиска (ищет на каждую букву с задержкой 120 мс) и фишки области, подсказка клавиш,
//! список вариантов с разделами «часто» / «все», счётчик и кнопки «Отмена» / «Открыть».
//! Клавиши — как в скрипте страницы: стрелки, PageUp/PageDown, Home/End, Enter, Esc, 1–9,
//! буква из списка переводит в поле. Что делать с выбором, решает picker.rs.

use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SPACE, VK_TAB, VK_UP,
};

use super::{close_button, dir, draw_frame, draw_title, load_skin, locale, t, tp, Skin, SCREEN, WIN_H, WIN_W};
use crate::find::{FindItem, FindScope};
use crate::native::controls::button::{Button, ButtonKind, Chip, Lunka};
use crate::native::controls::list::ListView;
use crate::native::controls::text_input::TextInput;
use crate::native::controls::tooltip::Tooltip;
use crate::native::controls::{
    data, dotted_hline, fill_round, inside, lcd, lcd_background, lcd_overlay, mirror, ui, Glow, Scene, ACC, ACC_2,
    ACC_3, ACC_4, ACC_5, ACC_HOT, GLASS,
};
use crate::native::{
    self, set_cursor, Align, Color, Cursor, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx,
    TextLayout, TextStyle, UiThread, WindowHandler, WindowId, WindowSpec,
};

static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);

/// Что показать (как payload события reever://picker).
#[derive(Clone, Debug)]
pub struct Payload {
    pub title: String,
    pub query: String,
    pub scope: FindScope,
    pub items: Vec<FindItem>,
}

/// Что окно просит у picker.rs (вызывается не из потока окон).
pub trait Host: Send + Sync {
    fn choose(&self, index: usize);
    fn close(&self);
    /// Поиск набранного; Ok(None) — устарел, Err("index-off") — индекс выключен.
    fn search(&self, query: &str, scope: FindScope) -> Result<Option<Vec<FindItem>>, String>;
}

const SCOPES: [FindScope; 3] = [FindScope::Any, FindScope::Folder, FindScope::File];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Close,
    Query,
    Chip(usize),
    List,
    Cancel,
    Open,
}

const ORDER: [Focus; 7] = [Focus::Close, Focus::Query, Focus::Chip(0), Focus::Chip(1), Focus::Chip(2), Focus::List, Focus::Cancel];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Close,
    Chip(usize),
    Cancel,
    Open,
    Query,
    Bar,
    Row(usize),
}

enum Entry {
    Section(String),
    Item(usize),
}

pub struct PickerView {
    host: std::sync::Arc<dyn Host>,
    ui: Option<std::sync::Arc<UiThread>>,
    id: Option<WindowId>,
    hwnd: isize,
    skin: Skin,
    rtl: bool,
    title: String,
    items: Vec<FindItem>,
    note: String,
    selected: usize,
    scope: FindScope,
    query: TextInput,
    chips: [Chip; 3],
    cancel: Button,
    open: Button,
    close: Lunka,
    list: ListView,
    entries: Vec<Entry>,
    focus: Focus,
    tooltip: Tooltip,
    scene: Scene,
    search_due: Option<Instant>,
    search_seq: u64,
    searching: bool,
    pending_choose: Option<usize>,
    pressed: Option<Target>,
    swallow_char: bool,
    relayout: bool,
    head_bottom: f32,
    foot_top: f32,
    hints: Vec<(Rect, Rc<TextLayout>, bool)>,
}

impl PickerView {
    fn new(host: std::sync::Arc<dyn Host>, ui: Option<std::sync::Arc<UiThread>>, skin: Skin) -> Self {
        let (_, rtl) = locale();
        let mut query = TextInput::default();
        query.pad_left = 28.0;
        query.placeholder = t("picker.searchPlaceholder");
        Self {
            host,
            ui,
            id: None,
            hwnd: 0,
            close: close_button(&skin, "tray", &t("picker.closeTitle")),
            skin,
            rtl,
            title: String::new(),
            items: Vec::new(),
            note: String::new(),
            selected: 0,
            scope: FindScope::Any,
            query,
            chips: [Chip::new(&t("picker.scopeAll")), Chip::new(&t("picker.scopeFolders")), Chip::new(&t("picker.scopeFiles"))],
            cancel: Button::new(&t("common.cancel"), ButtonKind::Plain),
            open: Button::new(&t("picker.openButton"), ButtonKind::Default),
            list: {
                let mut l = ListView::default();
                l.pad_top = 4.0;
                l.pad_bottom = 6.0;
                l
            },
            entries: Vec::new(),
            focus: Focus::Query,
            tooltip: Tooltip::default(),
            scene: Scene::default(),
            search_due: None,
            search_seq: 0,
            searching: false,
            pending_choose: None,
            pressed: None,
            swallow_char: false,
            relayout: true,
            head_bottom: 0.0,
            foot_top: 0.0,
            hints: Vec::new(),
        }
    }

    /// reever://picker: новый список, запрос и область; фокус в поле, запрос выделен.
    fn apply(&mut self, p: Payload) {
        let (_, rtl) = locale();
        self.rtl = rtl;
        self.query.placeholder = t("picker.searchPlaceholder");
        self.items = p.items;
        self.note.clear();
        self.search_seq += 1;
        self.search_due = None;
        self.searching = false;
        self.pending_choose = None;
        self.query.set_text(if p.query.is_empty() { &p.title } else { &p.query });
        self.set_scope(p.scope);
        self.render_list(p.title.trim());
        self.set_focus(Focus::Query, false);
        self.query.select_all();
    }

    fn set_scope(&mut self, scope: FindScope) {
        self.scope = scope;
        for (i, c) in self.chips.iter_mut().enumerate() {
            c.on = SCOPES[i] == scope;
        }
    }

    /// render(title) из страницы: заголовок, счётчик, разделы и пункты; выбор — первый.
    fn render_list(&mut self, title: &str) {
        self.title = if title.is_empty() { t("picker.found") } else { tp("picker.foundFor", &[("title", title)]) };
        self.entries.clear();
        let has_frequent = self.items.iter().any(|i| i.frequent);
        let mut section: Option<String> = None;
        for (i, item) in self.items.iter().enumerate() {
            let name = t(if item.frequent { "picker.frequent" } else { "picker.all" });
            if has_frequent && section.as_deref() != Some(&name) {
                section = Some(name.clone());
                self.entries.push(Entry::Section(name));
            }
            self.entries.push(Entry::Item(i));
        }
        self.selected = 0;
        self.relayout = true;
    }

    fn entry_of(&self, item: usize) -> Option<usize> {
        self.entries.iter().position(|e| matches!(e, Entry::Item(i) if *i == item))
    }

    fn mark(&mut self) {
        if let Some(e) = self.entry_of(self.selected) {
            self.list.ensure_visible(e, 0.0);
        }
    }

    fn set_focus(&mut self, f: Focus, visible: bool) {
        self.focus = f;
        self.query.set_focus(f == Focus::Query);
        for (i, c) in self.chips.iter_mut().enumerate() {
            c.press.focused = f == Focus::Chip(i);
            c.press.focus_visible = visible;
        }
        self.cancel.press.focused = f == Focus::Cancel;
        self.cancel.press.focus_visible = visible;
        self.open.press.focused = f == Focus::Open;
        self.open.press.focus_visible = visible;
        self.close.press.focused = f == Focus::Close;
        self.close.press.focus_visible = visible;
        native::set_ime_caret(self.hwnd, if f == Focus::Query { self.query.ime_caret() } else { None });
    }

    fn cycle_focus(&mut self, back: bool) {
        let mut order = ORDER.to_vec();
        order.push(Focus::Open);
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        let next = order[if back { (i + n - 1) % n } else { (i + 1) % n }];
        self.set_focus(next, true);
    }

    fn schedule_search(&mut self, delay: Duration) {
        self.search_due = Some(Instant::now() + delay);
    }

    /// Запрос поиска уходит в picker.rs в своём потоке; ответ возвращается задачей в поток окон.
    fn fire_search(&mut self) {
        self.search_due = None;
        self.search_seq += 1;
        self.searching = true;
        let (seq, text, scope) = (self.search_seq, self.query.text(), self.scope);
        let (host, ui, id) = (self.host.clone(), self.ui.clone(), self.id);
        std::thread::spawn(move || {
            let result = host.search(&text, scope);
            let (Some(id), Some(ui)) = (id, ui) else { return };
            ui.post(move |ui| {
                if let Some(v) = ui.handler_mut::<PickerView>(id) {
                    v.search_done(seq, &text, result);
                }
                ui.invalidate(id);
            });
        });
    }

    fn search_done(&mut self, seq: u64, text: &str, result: Result<Option<Vec<FindItem>>, String>) {
        if seq != self.search_seq {
            return;
        }
        self.searching = false;
        match result {
            Err(e) => {
                self.items.clear();
                self.note = if e.contains("index-off") { t("picker.indexOff") } else { e };
            }
            Ok(None) => return,
            Ok(Some(found)) => {
                self.items = found;
                self.note.clear();
            }
        }
        self.render_list(text.trim());
        if let Some(i) = self.pending_choose.take() {
            self.choose(i);
        }
    }

    /// Enter сразу после набора: сначала дождаться списка по набранному.
    fn choose_fresh(&mut self, index: usize) {
        if self.search_due.is_some() {
            self.fire_search();
        }
        if self.searching {
            self.pending_choose = Some(index);
        } else {
            self.choose(index);
        }
    }

    fn choose(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        self.items.clear();
        let host = self.host.clone();
        std::thread::spawn(move || host.choose(index));
    }

    fn close_window(&mut self) {
        self.items.clear();
        let host = self.host.clone();
        std::thread::spawn(move || host.close());
    }

    fn move_selection(&mut self, to: isize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = to.clamp(0, self.items.len() as isize - 1) as usize;
        self.mark();
    }

    fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        let now = Instant::now();
        if self.close.hit(x, y) {
            return Some(Target::Close);
        }
        for (i, c) in self.chips.iter().enumerate() {
            if c.hit(x, y) {
                return Some(Target::Chip(i));
            }
        }
        if self.cancel.hit(x, y) {
            return Some(Target::Cancel);
        }
        if self.open.hit(x, y) {
            return Some(Target::Open);
        }
        if inside(self.query.rect, x, y) {
            return Some(Target::Query);
        }
        if inside(self.list.viewport, x, y) {
            if self.list.thumb(now).is_some() && x >= self.list.viewport.x + self.list.viewport.w - 6.0 {
                return Some(Target::Bar);
            }
            if let Some(e) = self.list.row_at(x, y, now) {
                if let Some(Entry::Item(i)) = self.entries.get(e) {
                    return Some(Target::Row(*i));
                }
            }
        }
        None
    }

    fn activate(&mut self, t: Target) {
        match t {
            Target::Close | Target::Cancel => self.close_window(),
            Target::Open => self.choose_fresh(self.selected),
            Target::Chip(i) => {
                self.set_scope(SCOPES[i]);
                self.set_focus(Focus::Query, false);
                self.schedule_search(Duration::ZERO);
            }
            _ => {}
        }
    }

    // ── раскладка ──────────────────────────────────────────────────────
    fn layout(&mut self, cx: &mut RenderCx<'_>) {
        let s = SCREEN;
        let rtl = self.rtl;
        // .head: padding 11px 16px 9px; строка поиска: поле (flex 1) + фишки (gap 2px)
        let head = Rect::new(s.x + 16.0, s.y + 11.0, s.w - 32.0, 0.0);
        let mut chip_w = 0.0;
        let sizes: Vec<(f32, f32)> = self.chips.iter().map(|c| c.natural_size(&mut self.scene, cx)).collect();
        for (w, _) in &sizes {
            chip_w += w;
        }
        chip_w += 2.0 * 2.0;
        let row_h = 28.0f32;
        let input_w = head.w - chip_w - 8.0;
        self.query.rect = mirror(head, Rect::new(head.x, head.y, input_w, row_h), rtl);
        self.query.rtl = rtl;
        let mut x = head.x + input_w + 8.0;
        for (i, c) in self.chips.iter_mut().enumerate() {
            let (w, h) = sizes[i];
            c.rect = mirror(head, Rect::new(x, head.y + (row_h - h) / 2.0, w, h), rtl);
            x += w + 2.0;
        }
        // .help: подсказки клавиш в строку с переносом (gap 5px 14px), 12px Geologica
        self.hints.clear();
        let help_top = head.y + row_h + 9.0;
        let hint_style = TextStyle { direction: dir(rtl), ..ui(12.0, 400, 1.45) };
        let kbd_style = lcd(13.0, 500, 0.04);
        let parts: Vec<(Vec<&str>, String)> = vec![
            (vec!["↑", "↓"], t("picker.move")),
            (vec!["Enter"], t("picker.open")),
            (vec!["1–9"], t("picker.select")),
            (vec!["Esc"], t("picker.close")),
            (vec![], t("picker.doubleClick")),
        ];
        let line_h = 18.0f32;
        let (mut cx_, mut cy_) = (0.0f32, 0.0f32);
        let mut lines = 1;
        for (keys, label) in parts {
            // ширина подсказки: клавиши (min 18, padding 4) через пробел и подпись
            let mut items: Vec<(f32, Rc<TextLayout>, bool)> = Vec::new();
            for k in keys {
                if let Some(l) = self.scene.cache.get(cx.text(), k, &kbd_style, 1000.0) {
                    items.push(((l.width + 8.0 + 2.0).max(18.0 + 2.0), l, true));
                }
            }
            if let Some(l) = self.scene.cache.get(cx.text(), &label, &hint_style, 1000.0) {
                items.push((l.width, l, false));
            }
            let space = 3.5f32;
            let w: f32 = items.iter().map(|i| i.0).sum::<f32>() + space * (items.len().saturating_sub(1)) as f32;
            if cx_ > 0.0 && cx_ + w > head.w {
                cx_ = 0.0;
                cy_ += line_h + 5.0;
                lines += 1;
            }
            let mut ix = cx_;
            for (iw, l, kbd) in items {
                let r = mirror(head, Rect::new(head.x + ix, help_top + cy_, iw, line_h), rtl);
                self.hints.push((r, l, kbd));
                ix += iw + space;
            }
            cx_ += w + 14.0;
        }
        let help_h = lines as f32 * line_h + (lines - 1) as f32 * 5.0;
        self.head_bottom = help_top + help_h + 9.0;
        // .foot: padding 9px 14px, кнопки 28px, рамка сверху 1px
        self.foot_top = s.y + s.h - (9.0 + 28.0 + 9.0) - 1.0;
        let foot = Rect::new(s.x + 14.0, self.foot_top + 1.0 + 9.0, s.w - 28.0, 28.0);
        let ow = self.open.natural_width(&mut self.scene, cx);
        let cw = self.cancel.natural_width(&mut self.scene, cx);
        self.open.rect = mirror(foot, Rect::new(foot.x + foot.w - ow, foot.y, ow, 28.0), rtl);
        self.cancel.rect = mirror(foot, Rect::new(foot.x + foot.w - ow - 8.0 - cw, foot.y, cw, 28.0), rtl);
        // #list: от шапки до подвала
        self.list.viewport = Rect::new(s.x, self.head_bottom + 1.0, s.w, self.foot_top - self.head_bottom - 1.0);
        let heights: Vec<f32> = self.entries.iter().map(|e| if matches!(e, Entry::Section(_)) { 28.0 } else { 43.15 }).collect();
        self.list.set_rows(heights);
        self.mark();
        self.relayout = false;
    }

    // ── рисование ─────────────────────────────────────────────────────
    fn draw_screen(&mut self, cx: &mut RenderCx<'_>) {
        let s = SCREEN;
        let now = cx.now;
        let rtl = self.rtl;
        lcd_background(cx, s, 13.0);
        // значок поиска в поле: left 8px, top 7px, 14px, acc-3
        self.query.draw(&mut self.scene, cx);
        let qi = Rect::new(self.query.rect.x + 8.0, self.query.rect.y + 7.0, 14.0, 14.0);
        self.scene.icon("search", qi, 1.6, ACC_3, false, Glow::G1);
        for c in &self.chips {
            c.draw(&mut self.scene, cx);
        }
        for (r, l, kbd) in self.hints.clone() {
            if kbd {
                fill_round(cx, r, 3.0, ACC_5);
                crate::native::controls::stroke_round(cx, Rect::new(r.x, r.y + 1.0, r.w, 17.0), 3.0, ACC_4, 1.0, false);
                self.scene.text(&l, r.x + (r.w - l.width) / 2.0, r.y + 2.0 + 1.0, ACC_2, Glow::G1);
            } else {
                let x = if rtl { r.x + r.w - l.width } else { r.x };
                self.scene.text(&l, x, r.y + (r.h - l.height) / 2.0, ACC_3, Glow::G1);
            }
        }
        self.scene.flush(cx);
        dotted_hline(cx, s.x, self.head_bottom, s.w, ACC_4);

        // список
        let v = self.list.viewport;
        let bar = if self.list.thumb(now).is_some() { 6.0 } else { 0.0 };
        if crate::native::controls::push_round_layer(cx, v, 0.0, 1.0) {
            self.scene.set_clip(Some(v));
            if self.items.is_empty() {
                let empty = if !self.note.is_empty() {
                    self.note.clone()
                } else if self.query.text().trim().is_empty() {
                    t("picker.typeToSearch")
                } else {
                    t("picker.nothing")
                };
                let st = TextStyle { align: Align::Center, wrap: true, direction: dir(rtl), ..ui(13.0, 400, 1.45) };
                if let Some(l) = self.scene.cache.get(cx.text(), &empty, &st, v.w - 12.0 - 24.0) {
                    self.scene.text(&l, v.x + 6.0 + 12.0, v.y + 4.0 + 18.0, ACC_3, Glow::G1);
                }
            }
            let row_w = v.w - 12.0 - bar;
            let mut sections = Vec::new();
            for (e, entry) in self.entries.iter().enumerate() {
                let rr = self.list.row_rect(e, now);
                if rr.y > v.y + v.h || rr.y + rr.h < v.y {
                    if matches!(entry, Entry::Section(_)) {
                        sections.push(e);
                    }
                    continue;
                }
                match entry {
                    Entry::Section(_) => sections.push(e),
                    Entry::Item(i) => {
                        let row = Rect::new(v.x + 6.0, rr.y, row_w, rr.h);
                        let sel = *i == self.selected;
                        let hover = self.list.hover == Some(e);
                        if sel {
                            self.scene.shadow(cx, row, 5.0, Color::rgba(39, 230, 170, 89), 14.0);
                            fill_round(cx, row, 5.0, ACC);
                        } else if hover {
                            fill_round(cx, row, 5.0, ACC_5);
                        }
                        let item = &self.items[*i];
                        let inner = Rect::new(row.x + 10.0, row.y + 5.0, row.w - 20.0, row.h - 10.0);
                        let (num_c, kind_c, name_c, path_c, glow) =
                            if sel { (GLASS, GLASS, GLASS, GLASS, Glow::None) } else { (ACC_3, ACC_2, ACC_HOT, ACC_3, Glow::G2) };
                        // номер: колонка 26px, по правому краю
                        let num = mirror(inner, Rect::new(inner.x, inner.y, 26.0, inner.h), rtl);
                        if let Some(l) = self.scene.cache.get(cx.text(), &(i + 1).to_string(), &lcd(17.0, 500, 0.0), 1000.0) {
                            self.scene.text(&l, num.x + num.w - l.width, num.y + (num.h - l.height) / 2.0, num_c, glow);
                        }
                        let kind = mirror(inner, Rect::new(inner.x + 26.0 + 9.0, inner.y + (inner.h - 18.0) / 2.0, 18.0, 18.0), rtl);
                        self.scene.icon(if item.folder { "folder" } else { "file" }, kind, 1.6, kind_c, false, glow);
                        let text = mirror(inner, Rect::new(inner.x + 26.0 + 9.0 + 18.0 + 9.0, inner.y, inner.w - 62.0, inner.h), rtl);
                        // направление абзаца — как у страницы (dir), выравнивание «к началу»
                        let name_st = TextStyle { ellipsis: true, direction: dir(rtl), align: Align::Start, ..ui(13.0, 600, 1.45) };
                        if let Some(l) = self.scene.cache.get(cx.text(), &item.name, &name_st, text.w) {
                            self.scene.text(&l, text.x, text.y, name_c, glow);
                        }
                        let path_st = data(11.0, 1.3);
                        if let Some(l) = self.scene.cache.get_tail(cx.text(), &item.path, &path_st, text.w) {
                            self.scene.text(&l, text.x, text.y + 18.85, path_c, Glow::None);
                        }
                    }
                }
            }
            self.scene.flush(cx);
            // липкие заголовки разделов: прилипают к верху (top: -4px), следующий ложится поверх
            for e in sections {
                let Entry::Section(name) = &self.entries[e] else { continue };
                let rr = self.list.row_rect(e, now);
                let y = rr.y.max(v.y - 4.0);
                if y > v.y + v.h {
                    continue;
                }
                let hr = Rect::new(v.x, y, v.w - bar, 28.0);
                cx.fill_rect(hr, GLASS);
                dotted_hline(cx, hr.x, hr.y + 27.0, hr.w, ACC_4);
                let st = TextStyle { direction: dir(rtl), ..lcd(14.0, 500, 0.08) };
                if let Some(l) = self.scene.cache.get(cx.text(), &name.to_uppercase(), &st, 10_000.0) {
                    let x = if rtl { hr.x + hr.w - 10.0 - l.width } else { hr.x + 10.0 };
                    self.scene.text(&l, x, hr.y + 8.0, ACC_3, Glow::G2);
                }
                self.scene.flush(cx);
            }
            self.scene.set_clip(None);
            crate::native::controls::pop_layer(cx);
        }
        self.list.draw_scrollbar(&mut self.scene, cx);

        // подвал: счётчик, «Отмена», «Открыть»
        dotted_hline(cx, s.x, self.foot_top, s.w, ACC_4);
        let count = tp("picker.count", &[("count", &self.items.len().to_string())]);
        let st = TextStyle { direction: dir(rtl), ..lcd(15.0, 500, 0.06) };
        if let Some(l) = self.scene.cache.get(cx.text(), &count.to_uppercase(), &st, 10_000.0) {
            let foot = Rect::new(s.x + 14.0, self.foot_top + 10.0, s.w - 28.0, 28.0);
            let x = if rtl { foot.x + foot.w - l.width } else { foot.x };
            self.scene.text(&l, x, foot.y + (28.0 - l.height) / 2.0, ACC_3, Glow::G2);
        }
        self.cancel.draw(&mut self.scene, cx);
        self.open.draw(&mut self.scene, cx);
        self.scene.flush(cx);
        lcd_overlay(cx, s, 13.0);
        native::set_ime_caret(self.hwnd, if self.focus == Focus::Query { self.query.ime_caret() } else { None });
    }

    fn hover_at(&mut self, x: f32, y: f32) -> bool {
        let t = self.target_at(x, y);
        let mut redraw = self.close.set_hover(t == Some(Target::Close));
        for (i, c) in self.chips.iter_mut().enumerate() {
            redraw |= c.press.set_hover(t == Some(Target::Chip(i)), 0.15);
        }
        redraw |= self.cancel.press.set_hover(t == Some(Target::Cancel), 0.12);
        redraw |= self.open.press.set_hover(t == Some(Target::Open), 0.12);
        redraw |= self.list.mouse_move(x, y);
        redraw |= self.query.mouse_move(x);
        let tip = if t == Some(Target::Close) { Some((1u64, self.close.rect(), self.close.tip.as_str())) } else { None };
        redraw |= self.tooltip.hover(tip);
        set_cursor(
            self.hwnd,
            match t {
                Some(Target::Query) => Cursor::Text,
                Some(Target::Close | Target::Chip(_) | Target::Cancel | Target::Open | Target::Row(_)) => Cursor::Hand,
                _ => Cursor::Arrow,
            },
        );
        redraw
    }
}

impl WindowHandler for PickerView {
    fn attached(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
        self.query.hwnd = hwnd;
    }

    fn animating(&self) -> bool {
        let now = Instant::now();
        self.list.animating(now)
            || self.tooltip.animating(now)
            || self.close.press.animating(now)
            || self.cancel.press.animating(now)
            || self.open.press.animating(now)
            || self.chips.iter().any(|c| c.press.animating(now))
    }

    fn wake_at(&self) -> Option<Instant> {
        let now = Instant::now();
        [self.search_due, self.tooltip.wake_at(), self.query.next_blink(now)].into_iter().flatten().min()
    }

    fn prepare(&mut self, gpu: &crate::native::GpuCx<'_>) {
        if self.search_due.is_some_and(|t| gpu.now >= t) {
            self.fire_search();
        }
        self.tooltip.tick(gpu.now);
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        self.scene.begin_frame(cx);
        if self.relayout {
            self.layout(cx);
        }
        let (_, rtl) = locale();
        let title = self.title.clone();
        draw_title(&mut self.scene, cx, "play", true, &title, rtl);
        self.draw_screen(cx);
        draw_frame(cx, &self.skin);
        self.close.draw(&mut self.scene, cx);
        self.tooltip.draw(&mut self.scene, cx, (WIN_W, WIN_H));
    }

    fn mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Move => self.hover_at(ev.x, ev.y),
            MouseKind::Leave => {
                let mut r = self.list.mouse_leave() | self.tooltip.hide() | self.close.set_hover(false);
                for c in &mut self.chips {
                    r |= c.press.set_hover(false, 0.15);
                }
                r |= self.cancel.press.set_hover(false, 0.12) | self.open.press.set_hover(false, 0.12);
                r
            }
            MouseKind::Down(MouseButton::Left) => {
                self.tooltip.hide();
                let t = self.target_at(ev.x, ev.y);
                self.pressed = t;
                match t {
                    Some(Target::Close) => {
                        self.close.press.pressed = true;
                    }
                    Some(Target::Chip(i)) => {
                        self.chips[i].press.pressed = true;
                        self.set_focus(Focus::Chip(i), false);
                    }
                    Some(Target::Cancel) => {
                        self.cancel.press.pressed = true;
                        self.set_focus(Focus::Cancel, false);
                    }
                    Some(Target::Open) => {
                        self.open.press.pressed = true;
                        self.set_focus(Focus::Open, false);
                    }
                    Some(Target::Query) => {
                        self.set_focus(Focus::Query, false);
                        let shift = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10) } < 0;
                        self.query.mouse_down(ev.x, shift);
                    }
                    Some(Target::Bar) => {
                        self.list.mouse_down(ev.x, ev.y);
                    }
                    Some(Target::Row(i)) => {
                        self.selected = i;
                        self.set_focus(Focus::List, false);
                        self.mark();
                    }
                    None => native::begin_drag(self.hwnd),
                }
                true
            }
            MouseKind::Up(MouseButton::Left) => {
                let t = self.target_at(ev.x, ev.y);
                let pressed = self.pressed.take();
                self.close.press.pressed = false;
                for c in &mut self.chips {
                    c.press.pressed = false;
                }
                self.cancel.press.pressed = false;
                self.open.press.pressed = false;
                self.list.mouse_up();
                self.query.mouse_up();
                if let Some(p) = pressed.filter(|p| Some(*p) == t) {
                    self.activate(p);
                }
                true
            }
            MouseKind::DoubleClick => {
                match self.target_at(ev.x, ev.y) {
                    Some(Target::Row(i)) => self.choose(i),
                    Some(Target::Query) => self.query.double_click(ev.x),
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        self.tooltip.hide();
        if inside(self.list.viewport, x, y) {
            let r = self.list.wheel(delta);
            self.hover_at(x, y);
            return r;
        }
        false
    }

    fn key(&mut self, ev: KeyEvent) -> bool {
        self.tooltip.hide();
        if ev.alt {
            return false;
        }
        let k = ev.key as u16;
        let typing = self.focus == Focus::Query;
        let plain = !ev.ctrl && !ev.alt;
        if k == VK_ESCAPE.0 {
            self.close_window();
            return true;
        }
        if k == VK_RETURN.0 {
            self.choose_fresh(self.selected);
            return true;
        }
        if k == VK_TAB.0 {
            self.cycle_focus(ev.shift);
            return true;
        }
        if k == VK_SPACE.0 && !typing {
            let t = match self.focus {
                Focus::Close => Some(Target::Close),
                Focus::Chip(i) => Some(Target::Chip(i)),
                Focus::Cancel => Some(Target::Cancel),
                Focus::Open => Some(Target::Open),
                _ => None,
            };
            if let Some(t) = t {
                self.swallow_char = true;
                self.activate(t);
                return true;
            }
        }
        let is_char_key = (0x30..=0x5A).contains(&k) || (0xBA..=0xE2).contains(&k) || k == VK_SPACE.0;
        if self.items.is_empty() {
            if !typing && plain && (is_char_key || k == VK_BACK.0) {
                self.set_focus(Focus::Query, false);
                if k == VK_BACK.0 {
                    let r = self.query.key(ev);
                    if r.changed {
                        self.schedule_search(Duration::from_millis(120));
                    }
                }
                return true;
            }
            if typing {
                let r = self.query.key(ev);
                if r.changed {
                    self.schedule_search(Duration::from_millis(120));
                }
                return r.redraw;
            }
            return false;
        }
        let page = 8isize;
        let sel = self.selected as isize;
        if k == VK_DOWN.0 {
            self.move_selection(sel + 1);
        } else if k == VK_UP.0 {
            self.move_selection(sel - 1);
        } else if k == VK_NEXT.0 {
            self.move_selection(sel + page);
        } else if k == VK_PRIOR.0 {
            self.move_selection(sel - page);
        } else if typing {
            let r = self.query.key(ev);
            if r.changed {
                self.schedule_search(Duration::from_millis(120));
            }
            return r.redraw;
        } else if k == VK_HOME.0 {
            self.move_selection(0);
        } else if k == VK_END.0 {
            self.move_selection(self.items.len() as isize - 1);
        } else if plain && !ev.shift && ((0x31..=0x39).contains(&k) || (0x61..=0x69).contains(&k)) {
            let d = if k >= 0x61 { k - 0x61 } else { k - 0x31 } as usize;
            self.swallow_char = true;
            self.choose(d);
        } else if plain && (is_char_key || k == VK_BACK.0) {
            // буква из списка — в поле поиска, как в Everything
            self.set_focus(Focus::Query, false);
            if k == VK_BACK.0 {
                let r = self.query.key(ev);
                if r.changed {
                    self.schedule_search(Duration::from_millis(120));
                }
            }
        } else {
            return false;
        }
        true
    }

    fn char_input(&mut self, c: char) -> bool {
        if std::mem::take(&mut self.swallow_char) {
            return false;
        }
        if self.focus != Focus::Query {
            return false;
        }
        let r = self.query.char_input(c);
        if r.changed {
            self.schedule_search(Duration::from_millis(120));
        }
        r.redraw
    }

    fn focus(&mut self, focused: bool) -> bool {
        self.query.set_focus(focused && self.focus == Focus::Query);
        true
    }

    fn close_requested(&mut self) {
        self.close_window();
    }

    fn a11y_tree(&mut self, scale: f32) -> Option<crate::native::a11y::accesskit::TreeUpdate> {
        use crate::native::a11y::accesskit::{Action, Node, NodeId, Role, Toggled, TreeId, TreeInfo, TreeUpdate};
        use crate::native::a11y::bounds;
        let b = |r: Rect| bounds(r.x, r.y, r.w, r.h, scale);
        let mut nodes = Vec::new();
        let mut root = Node::new(Role::Window);
        root.set_label(self.title.clone());
        let mut kids = vec![NodeId(2), NodeId(3), NodeId(4), NodeId(5), NodeId(6), NodeId(7), NodeId(8), NodeId(9)];
        let mut close = Node::new(Role::Button);
        close.set_label(t("common.close"));
        close.set_description(self.close.tip.clone());
        close.set_bounds(b(self.close.rect()));
        close.add_action(Action::Click);
        close.add_action(Action::Focus);
        nodes.push((NodeId(2), close));
        let mut q = Node::new(Role::SearchInput);
        q.set_label(t("picker.searchLabel"));
        q.set_value(self.query.text());
        q.set_placeholder(self.query.placeholder.clone());
        q.set_bounds(b(self.query.rect));
        q.add_action(Action::Focus);
        q.add_action(Action::SetValue);
        nodes.push((NodeId(3), q));
        for (i, c) in self.chips.iter().enumerate() {
            let mut n = Node::new(Role::RadioButton);
            n.set_label(c.label.clone());
            n.set_toggled(if c.on { Toggled::True } else { Toggled::False });
            n.set_bounds(b(c.rect));
            n.add_action(Action::Click);
            n.add_action(Action::Focus);
            nodes.push((NodeId(4 + i as u64), n));
        }
        let mut list = Node::new(Role::ListBox);
        list.set_label(t("picker.list"));
        list.set_bounds(b(self.list.viewport));
        list.set_size_of_set(self.items.len());
        let now = Instant::now();
        let mut items = Vec::new();
        for (e, entry) in self.entries.iter().enumerate() {
            if let Entry::Item(i) = entry {
                let id = NodeId(100 + *i as u64);
                let mut n = Node::new(Role::ListBoxOption);
                n.set_label(format!("{} — {}", self.items[*i].name, self.items[*i].path));
                n.set_selected(*i == self.selected);
                n.set_position_in_set(*i + 1);
                n.set_size_of_set(self.items.len());
                n.set_bounds(b(self.list.row_rect(e, now)));
                n.add_action(Action::Click);
                n.add_action(Action::Focus);
                nodes.push((id, n));
                items.push(id);
            }
        }
        list.set_children(items);
        nodes.push((NodeId(7), list));
        for (id, btn) in [(8u64, &self.cancel), (9, &self.open)] {
            let mut n = Node::new(Role::Button);
            n.set_label(btn.label.clone());
            n.set_bounds(b(btn.rect));
            n.add_action(Action::Click);
            n.add_action(Action::Focus);
            nodes.push((NodeId(id), n));
        }
        let mut count = Node::new(Role::Label);
        count.set_value(tp("picker.count", &[("count", &self.items.len().to_string())]));
        nodes.push((NodeId(10), count));
        kids.push(NodeId(10));
        root.set_children(kids);
        root.set_bounds(bounds(0.0, 0.0, WIN_W, WIN_H, scale));
        nodes.push((NodeId(1), root));
        let focus = match self.focus {
            Focus::Close => NodeId(2),
            Focus::Query => NodeId(3),
            Focus::Chip(i) => NodeId(4 + i as u64),
            Focus::List if !self.items.is_empty() => NodeId(100 + self.selected as u64),
            Focus::List => NodeId(7),
            Focus::Cancel => NodeId(8),
            Focus::Open => NodeId(9),
        };
        Some(TreeUpdate { nodes, tree: Some(TreeInfo::new(NodeId(1))), tree_id: TreeId::ROOT, focus })
    }

    fn a11y_action(&mut self, req: crate::native::a11y::accesskit::ActionRequest) -> bool {
        use crate::native::a11y::accesskit::{Action, ActionData};
        let id = req.target_node.0;
        let focus = match id {
            2 => Some(Focus::Close),
            3 => Some(Focus::Query),
            4..=6 => Some(Focus::Chip((id - 4) as usize)),
            7 => Some(Focus::List),
            8 => Some(Focus::Cancel),
            9 => Some(Focus::Open),
            n if n >= 100 => Some(Focus::List),
            _ => None,
        };
        match req.action {
            Action::Focus => {
                if let Some(f) = focus {
                    if id >= 100 {
                        self.selected = (id - 100) as usize;
                        self.mark();
                    }
                    self.set_focus(f, true);
                }
            }
            Action::Click => match id {
                2 => self.activate(Target::Close),
                4..=6 => self.activate(Target::Chip((id - 4) as usize)),
                8 => self.activate(Target::Cancel),
                9 => self.activate(Target::Open),
                n if n >= 100 => self.choose((n - 100) as usize),
                _ => {}
            },
            Action::SetValue if id == 3 => {
                if let Some(ActionData::Value(v)) = req.data {
                    self.query.set_text(&v);
                    self.schedule_search(Duration::from_millis(120));
                }
            }
            _ => return false,
        }
        true
    }
}

fn ui_thread(app: &AppHandle) -> Option<std::sync::Arc<UiThread>> {
    crate::mascot_native::ui(app)
}

/// Показать окно с вариантами (создать, если его нет) и отдать ему фокус.
pub fn show(app: &AppHandle, host: std::sync::Arc<dyn Host>, payload: Payload) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(ui) = ui_thread(&app) else { return };
        let existing = WIN.lock().ok().and_then(|w| *w);
        if let Some((id, hwnd)) = existing {
            ui.post(move |ui| {
                if let Some(v) = ui.handler_mut::<PickerView>(id) {
                    v.apply(payload);
                }
                ui.invalidate(id);
            });
            if !crate::dialog_demo::active() {
                crate::winutil::bring_to_front_raw(hwnd);
            }
            return;
        }
        let (x, y) = super::centered_position();
        let (app2, ui2) = (app.clone(), ui.clone());
        let made = ui.call(move |ui| {
            let skin = load_skin(&app2, ui.images());
            let mut view = PickerView::new(host, Some(ui2), skin);
            view.apply(payload);
            let spec = WindowSpec { title: "Reever".into(), x, y, width: WIN_W, height: WIN_H, topmost: true, no_activate: crate::dialog_demo::active(), visible: true };
            let id = ui.create_window(spec, Box::new(view))?;
            if let Some(v) = ui.handler_mut::<PickerView>(id) {
                v.id = Some(id);
            }
            Ok::<_, windows::core::Error>((id, ui.info(id).map(|i| i.hwnd).unwrap_or(0)))
        });
        match made {
            Some(Ok((id, hwnd))) => {
                if let Ok(mut w) = WIN.lock() {
                    *w = Some((id, hwnd));
                }
                // в режиме проверки фокус не забирается: человек за компьютером может печатать
                if !crate::dialog_demo::active() {
                    crate::winutil::bring_to_front_raw(hwnd);
                    native::activate(hwnd);
                }
                if crate::dialog_demo::active() {
                    // замер: дождаться первого кадра
                    for _ in 0..200 {
                        if ui.call(move |ui| ui.info(id).and_then(|i| i.first_frame)).flatten().is_some() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    crate::dialog_demo::shown("picker");
                }
            }
            Some(Err(e)) => crate::events::ui_log(&app, format!("picker: native window failed: {e}")),
            None => {}
        }
    });
}

/// Закрыть окно (выбор сделан, окно закрыли или ожидание прервано).
pub fn hide(app: &AppHandle) {
    let Some((id, _)) = WIN.lock().ok().and_then(|mut w| w.take()) else { return };
    if let Some(ui) = ui_thread(app) {
        ui.post(move |ui| {
            ui.close_window(id);
        });
        // устройство — не сразу: следующее окно (вопрос после выбора, новый поиск) в течение
        // минуты покажется без пересоздания D3D11 (~200 мс)
        super::release_later(ui.clone());
    }
}

pub fn hwnd() -> Option<isize> {
    WIN.lock().ok().and_then(|w| w.map(|(_, h)| h))
}


/// Снимок окна вне экрана (RGBA) — для проверок без захвата экрана.
pub fn snapshot(app: &AppHandle) -> Option<(u32, u32, Vec<u8>)> {
    let (id, _) = WIN.lock().ok().and_then(|w| *w)?;
    let ui = crate::mascot_native::ui(app)?;
    ui.call(move |ui| ui.snapshot(id)).flatten()
}

/// Прислать окну WM_DPICHANGED (проверка масштаба без второго монитора).
pub fn fake_dpi(dpi: u32) {
    let Some((_, hwnd)) = WIN.lock().ok().and_then(|w| *w) else { return };
    crate::dialogs_native::send_dpi(hwnd, dpi);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::ImageId;
    use std::sync::Arc;
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_3;

    #[derive(Default)]
    struct Mock {
        calls: Mutex<Vec<String>>,
    }

    impl Host for Mock {
        fn choose(&self, index: usize) {
            self.calls.lock().unwrap().push(format!("choose {index}"));
        }
        fn close(&self) {
            self.calls.lock().unwrap().push("close".into());
        }
        fn search(&self, query: &str, _scope: FindScope) -> Result<Option<Vec<FindItem>>, String> {
            self.calls.lock().unwrap().push(format!("search {query}"));
            Err("index-off".into())
        }
    }

    fn calls(m: &Mock) -> Vec<String> {
        // вызовы хоста идут в своих потоках — подождать
        for _ in 0..100 {
            if !m.calls.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(20));
        m.calls.lock().unwrap().clone()
    }

    fn view(n: usize) -> (PickerView, Arc<Mock>) {
        let mock = Arc::new(Mock::default());
        let skin = Skin { frame: ImageId(0), close_off: ImageId(1), close_on: ImageId(2) };
        let mut v = PickerView::new(mock.clone(), None, skin);
        let items = (0..n)
            .map(|i| FindItem { name: format!("item {i}"), path: format!("C:\\x\\{i}"), folder: i % 2 == 0, frequent: i < 2, score: 0.0 })
            .collect();
        v.apply(Payload { title: "q".into(), query: "q".into(), scope: FindScope::Any, items });
        (v, mock)
    }

    fn key(k: u16) -> KeyEvent {
        KeyEvent { key: k as u32, ctrl: false, shift: false, alt: false, repeat: false }
    }

    #[test]
    fn arrows_and_pages_move_selection_from_the_search_field() {
        let (mut v, _) = view(20);
        assert_eq!(v.focus, Focus::Query);
        v.key(key(VK_DOWN.0));
        v.key(key(VK_DOWN.0));
        assert_eq!(v.selected, 2);
        v.key(key(VK_UP.0));
        assert_eq!(v.selected, 1);
        v.key(key(VK_NEXT.0));
        assert_eq!(v.selected, 9);
        v.key(key(VK_PRIOR.0));
        v.key(key(VK_PRIOR.0));
        assert_eq!(v.selected, 0);
        // Home из поля — каретке в поле, выбор не меняется
        v.key(key(VK_DOWN.0));
        v.key(key(VK_HOME.0));
        assert_eq!(v.selected, 1);
    }

    #[test]
    fn home_end_and_digits_work_in_the_list() {
        let (mut v, mock) = view(12);
        v.set_focus(Focus::List, true);
        v.key(key(VK_END.0));
        assert_eq!(v.selected, 11);
        v.key(key(VK_HOME.0));
        assert_eq!(v.selected, 0);
        v.key(key(VK_3.0));
        assert!(!v.char_input('3'), "цифра выбора не печатается");
        assert_eq!(calls(&mock), vec!["choose 2"]);
    }

    #[test]
    fn letter_in_list_goes_to_search_field() {
        let (mut v, _) = view(5);
        v.set_focus(Focus::List, false);
        v.key(key(0x41));
        assert_eq!(v.focus, Focus::Query);
        v.query.set_text("");
        v.char_input('a');
        assert_eq!(v.query.text(), "a");
        assert!(v.search_due.is_some(), "набор запускает поиск с задержкой");
    }

    #[test]
    fn enter_opens_selection_and_escape_closes() {
        let (mut v, mock) = view(5);
        v.key(key(VK_DOWN.0));
        v.key(key(VK_RETURN.0));
        assert_eq!(calls(&mock), vec!["choose 1"]);
        let (mut v, mock) = view(5);
        v.key(key(VK_ESCAPE.0));
        assert_eq!(calls(&mock), vec!["close"]);
    }

    #[test]
    fn tab_cycles_focus_and_space_presses_chip() {
        let (mut v, _) = view(3);
        v.key(key(VK_TAB.0));
        assert_eq!(v.focus, Focus::Chip(0));
        v.key(key(VK_TAB.0));
        assert_eq!(v.focus, Focus::Chip(1));
        v.key(key(VK_SPACE.0));
        assert_eq!(v.scope, FindScope::Folder);
        assert_eq!(v.focus, Focus::Query);
        let back = KeyEvent { shift: true, ..key(VK_TAB.0) };
        v.key(back);
        assert_eq!(v.focus, Focus::Close);
        v.key(back);
        assert_eq!(v.focus, Focus::Open);
    }

    #[test]
    fn sections_frequent_then_all() {
        let (v, _) = view(4);
        assert!(matches!(v.entries[0], Entry::Section(_)));
        assert!(matches!(v.entries[1], Entry::Item(0)));
        assert!(matches!(v.entries[3], Entry::Section(_)));
        assert_eq!(v.entries.len(), 6);
    }
}
