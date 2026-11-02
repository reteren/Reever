//! Вкладка «Действия» (#p-actions, renderRows/renderEditor в frontend/app.js): слева поиск,
//! «Создать», фильтры и список действий; справа редактор выбранного (имя и тип, цель, фразы,
//! сохранить/удалить). Подтверждения (несохранённое при переходе, удаление) — строкой с
//! кнопками в редакторе, а не модальным окном. Выбор программы Windows, калибровка и сканер
//! пока не перенесены.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_TAB};

use crate::actions::Action;
use crate::native::controls::button::{Button, ButtonKind};
use crate::native::controls::form::{Form, FormEvent, FormResult, Item};
use crate::native::controls::text_input::TextInput;
use crate::native::controls::{
    data, fill_round, inside, lcd, stroke_round, ui, Glow, Scene, ACC, ACC_2, ACC_3, ACC_4, ACC_5, GLASS,
};
use crate::native::{Align, Color, Direction, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx, TextLayout, TextStyle};
use crate::ui_text::t;

const KINDS: [&str; 6] = ["shell", "url", "uwp", "folder", "system", "search"];
const FILTERS: [(&str, &str); 5] = [("all", "filter.all"), ("apps", "filter.apps"), ("folder", "filter.folder"), ("web", "filter.web"), ("system", "filter.system")];
const LIST_W: f32 = 200.0;

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "url" => "link",
        "uwp" => "grid",
        "folder" => "folder",
        "system" => "power",
        "search" => "lens",
        _ => "play",
    }
}

fn kind_group(kind: &str) -> &'static str {
    match kind {
        "url" | "search" => "web",
        "folder" => "folder",
        "system" => "system",
        _ => "apps",
    }
}

/// Что вкладка просит у программы.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionsOut {
    Save(Action),
    Delete(String),
    /// сохранить и начать калибровку
    Calibrate(Action),
    /// открыть выбор программы Windows
    PickApp,
    /// открыть сканер программ
    Scan,
}

#[derive(Debug, Clone, PartialEq)]
enum Pending {
    /// перейти к другому действию (None — к новому), бросив правки
    Switch(Option<String>),
    Delete,
}

#[derive(Debug, Clone, PartialEq)]
enum Hover {
    Row(String),
    Filter(usize),
    Create,
    Scan,
    /// «Сканировать» в подсказке над пустым списком
    ScanTip,
}

pub struct ActionsTab {
    actions: Vec<Action>,
    hidden: HashMap<String, Vec<String>>,
    sel: Option<String>,
    draft: Option<Action>,
    dirty: bool,
    filter: usize,
    search: TextInput,
    search_focus: bool,
    create: Button,
    scan: Button,
    scan_tip: Button,
    scroll: f32,
    max_scroll: f32,
    edit_scroll: f32,
    edit_max: f32,
    rows: Vec<(Rect, String)>,
    filters: Vec<Rect>,
    hover: Option<Hover>,
    pressed: Option<Hover>,
    pub form: Form,
    notice: Vec<String>,
    pending: Option<Pending>,
    locale: String,
    rtl: bool,
    edit_rect: Rect,
    list_rect: Rect,
}

impl ActionsTab {
    pub fn new(locale: &str, rtl: bool, hwnd: isize) -> Self {
        let mut search = TextInput::default();
        search.style = ui(13.0, 400, 1.0);
        search.placeholder = t(locale, "actions.search", &[]);
        search.hwnd = hwnd;
        search.rtl = rtl;
        Self {
            actions: Vec::new(),
            hidden: HashMap::new(),
            sel: None,
            draft: None,
            dirty: false,
            filter: 0,
            search,
            search_focus: false,
            create: Button::new(&t(locale, "actions.create", &[]), ButtonKind::Plain),
            scan: Button::new(&t(locale, "actions.scan", &[]), ButtonKind::Plain),
            scan_tip: Button::new(&t(locale, "actions.scan", &[]), ButtonKind::Plain),
            scroll: 0.0,
            max_scroll: 0.0,
            edit_scroll: 0.0,
            edit_max: 0.0,
            rows: Vec::new(),
            filters: Vec::new(),
            hover: None,
            pressed: None,
            form: Form::new(hwnd, rtl),
            notice: Vec::new(),
            pending: None,
            locale: locale.to_string(),
            rtl,
            edit_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            list_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    pub fn set_hwnd(&mut self, hwnd: isize) {
        self.search.hwnd = hwnd;
        self.form = Form::new(hwnd, self.rtl);
        self.rebuild();
    }

    fn t(&self, key: &str) -> String {
        t(&self.locale, key, &[])
    }

    fn shown(&self, a: &Action) -> Vec<String> {
        let hidden = self.hidden.get(&a.id);
        a.phrases.iter().filter(|p| !hidden.is_some_and(|h| h.contains(p))).cloned().collect()
    }

    /// Свежий список из программы. Несохранённые правки остаются; иначе черновик — из списка.
    pub fn sync(&mut self, actions: Vec<Action>, hidden: HashMap<String, Vec<String>>) {
        let same = actions == self.actions && hidden == self.hidden;
        self.actions = actions;
        self.hidden = hidden;
        if same {
            return;
        }
        if !self.dirty {
            self.draft = self.sel.as_ref().and_then(|id| self.actions.iter().find(|a| &a.id == id).cloned());
            if self.draft.is_none() {
                self.sel = None;
            }
            self.load_form();
        }
    }

    fn visible(&self, a: &Action) -> bool {
        let f = FILTERS[self.filter].0;
        if f != "all" && kind_group(&a.kind) != f {
            return false;
        }
        let q = self.search.text().to_lowercase();
        q.is_empty() || a.label.to_lowercase().contains(&q) || self.shown(a).iter().any(|p| p.contains(&q))
    }

    /// Поля формы — заново из черновика (выбор другого действия, сохранение).
    fn load_form(&mut self) {
        self.form.set_items(Vec::new());
        self.edit_scroll = 0.0;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let Some(a) = &self.draft else {
            self.form.set_items(Vec::new());
            return;
        };
        let options: Vec<(String, String)> = KINDS.iter().map(|k| (k.to_string(), self.t(&format!("actionKind.{k}")))).collect();
        let kind = if a.kind.is_empty() { "shell".to_string() } else { a.kind.clone() };
        let shown = self.shown(a);
        let count = shown.len().to_string();
        let mut items = vec![
            Item::InputSelect {
                id: "name".into(),
                placeholder: self.t("actions.namePlaceholder"),
                value: a.label.clone(),
                select: "kind".into(),
                options,
                selected: kind,
            },
            Item::Label(self.t("actions.target")),
            Item::Input { id: "target".into(), placeholder: "steam://rungameid/570".into(), value: a.target.clone() },
            Item::Note(self.t("actions.searchHint")),
            Item::Buttons { buttons: vec![("pickapp".into(), self.t("actions.pickApp"), ButtonKind::Plain)], top: 6.0, bottom: 0.0 },
            Item::Label(t(&self.locale, "actions.phrases", &[("count", &count)])),
            Item::Chips { id: "phrases".into(), values: shown, primary_first: false },
            Item::AddRow { id: "phrases+".into(), placeholder: self.t("actions.phrasePlaceholder"), button: self.t("common.add") },
        ];
        match &self.pending {
            Some(p) => {
                let (text, ok, kind) = match p {
                    Pending::Delete => ("actions.confirmDelete", "common.delete", ButtonKind::Danger),
                    Pending::Switch(_) => ("actions.confirmDiscard", "common.ok", ButtonKind::Default),
                };
                items.push(Item::Errors(vec![self.t(text)]));
                items.push(Item::Buttons {
                    buttons: vec![("confirm".into(), self.t(ok), kind), ("unconfirm".into(), self.t("common.cancel"), ButtonKind::Plain)],
                    top: 4.0,
                    bottom: 4.0,
                });
            }
            None => {
                items.push(Item::Buttons {
                    buttons: vec![
                        ("calibrate".into(), self.t("actions.calibrate"), ButtonKind::Default),
                        ("save".into(), self.t("common.save"), ButtonKind::Default),
                        ("delete".into(), self.t("common.delete"), ButtonKind::Danger),
                    ],
                    top: 16.0,
                    bottom: 4.0,
                });
                items.push(Item::Errors(self.notice.clone()));
            }
        }
        self.form.set_items(items);
    }

    fn select(&mut self, id: Option<String>) {
        self.sel = id.clone();
        self.dirty = false;
        self.notice.clear();
        self.pending = None;
        self.draft = match id {
            Some(id) => self.actions.iter().find(|a| a.id == id).cloned(),
            None => {
                let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
                let a = Action { id: format!("act_{n:x}"), label: String::new(), kind: "shell".into(), target: String::new(), verbs: Vec::new(), phrases: Vec::new() };
                self.sel = Some(a.id.clone());
                self.dirty = true;
                Some(a)
            }
        };
        self.load_form();
    }

    /// Выбрана программа Windows: тип uwp, цель — AUMID, имя — если пустое (как openAppPicker).
    pub fn apply_app(&mut self, name: &str, aumid: &str) {
        if let Some(d) = self.draft.as_mut() {
            d.kind = "uwp".into();
            d.target = aumid.to_string();
            if d.label.trim().is_empty() {
                d.label = name.to_string();
            }
            self.dirty = true;
            self.load_form();
        }
    }

    /// Фразы действия, что видны (для окна калибровки).
    pub fn shown_phrases(&self, id: &str) -> Option<(String, Vec<String>)> {
        let a = self.actions.iter().find(|a| a.id == id)?;
        Some((a.label.clone(), self.shown(a)))
    }

    /// Открыть действие по id (проверка).
    pub fn open(&mut self, id: &str) {
        self.go(Some(id.to_string()));
    }

    /// Перейти к действию (или к новому); с несохранёнными правками — сначала спросить.
    fn go(&mut self, id: Option<String>) {
        if id.is_some() && id == self.sel {
            return;
        }
        if self.dirty {
            self.pending = Some(Pending::Switch(id));
            self.rebuild();
            return;
        }
        self.select(id);
    }

    // ---------- рисование ----------

    fn text(scene: &mut Scene, cx: &RenderCx<'_>, s: &str, st: &TextStyle, w: f32) -> Option<Rc<TextLayout>> {
        scene.cache.get(cx.text(), s, st, w)
    }

    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, body: Rect) {
        let lx = body.x;
        let mut y = body.y;
        // поиск
        self.search.rect = Rect::new(lx, y, LIST_W, 28.0);
        y += 28.0 + 8.0;
        // .list-keys: «Создать» и «Сканировать» поровну
        let half = (LIST_W - 8.0) / 2.0;
        self.create.rect = Rect::new(lx, y, half, 28.0);
        self.scan.rect = Rect::new(lx + half + 8.0, y, half, 28.0);
        y += 28.0 + 8.0;
        // фильтры
        self.filters.clear();
        let st = lcd(14.0, 500, 0.06);
        let mut px = lx;
        for (i, (_, key)) in FILTERS.iter().enumerate() {
            let label = self.t(key).to_uppercase();
            let Some(l) = Self::text(scene, cx, &label, &st, 1000.0) else { continue };
            let w = l.width + 14.0 + 2.0;
            let h = l.height + 6.0 + 2.0;
            if px > lx && px + w > lx + LIST_W {
                px = lx;
                y += h + 4.0;
            }
            let r = Rect::new(px, y, w, h);
            let on = i == self.filter;
            let hot = self.hover == Some(Hover::Filter(i));
            if on {
                scene.shadow(cx, r, 4.0, Color::rgba(39, 230, 170, 89), 10.0);
                fill_round(cx, r, 4.0, ACC);
            } else if hot {
                stroke_round(cx, r, 4.0, ACC_5, 1.0, false);
            }
            let (c, g) = if on { (GLASS, Glow::None) } else if hot { (ACC_2, Glow::G1) } else { (ACC_3, Glow::G1) };
            scene.text(&l, r.x + 8.0, r.y + 4.0, c, g);
            self.filters.push(r);
            px += w + 4.0;
            if i + 1 == FILTERS.len() {
                y += h;
            }
        }
        y += 8.0;
        // строки
        let list = Rect::new(lx - 4.0, y, LIST_W + 8.0, body.y + body.h - y);
        self.list_rect = list;
        scene.flush(cx);
        scene.set_clip(Some(list));
        unsafe { cx.dc().PushAxisAlignedClip(&list.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        self.rows.clear();
        let shown: Vec<Action> = self.actions.iter().filter(|a| self.visible(a)).cloned().collect();
        let mut ry = list.y - self.scroll;
        // своих программ ещё нет — подсказать сканер (.scan-tip)
        self.scan_tip.rect = Rect::new(0.0, 0.0, 0.0, 0.0);
        if self.filter == 0 && self.search.text().is_empty() && !self.actions.iter().any(|a| a.kind != "system") {
            let hs = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
            let tip = Rect::new(list.x + 4.0, ry + 2.0, list.w - 8.0, 0.0);
            let l = Self::text(scene, cx, &self.t("actions.scanTip"), &hs, tip.w - 16.0);
            let lh = l.as_ref().map_or(0.0, |l| l.height);
            let bw = self.scan_tip.natural_width(scene, cx);
            let r = Rect::new(tip.x, tip.y, tip.w, 8.0 + lh + 7.0 + 28.0 + 8.0);
            stroke_round(cx, r, 5.0, ACC_4, 1.0, true);
            if let Some(l) = l {
                scene.text(&l, r.x + 8.0, r.y + 8.0, ACC_3, Glow::G1);
            }
            self.scan_tip.rect = Rect::new(r.x + 8.0, r.y + 8.0 + lh + 7.0, bw, 28.0);
            ry += r.h + 2.0 + 8.0;
        }
        if shown.is_empty() {
            let hs = TextStyle { wrap: true, direction: self.dir(), ..ui(12.0, 400, 1.45) };
            if let Some(l) = Self::text(scene, cx, &self.t("actions.empty"), &hs, list.w - 24.0) {
                scene.text(&l, list.x + 12.0, ry + 12.0, ACC_3, Glow::G1);
            }
        }
        let name_st = TextStyle { ellipsis: true, direction: self.dir(), ..ui(13.0, 400, 1.2) };
        let count_st = data(10.0, 1.0);
        for a in &shown {
            let rh = 5.0 + 17.0 + 5.0;
            let r = Rect::new(list.x, ry, list.w, rh);
            let sel = self.sel.as_deref() == Some(a.id.as_str());
            let hot = self.hover == Some(Hover::Row(a.id.clone()));
            if sel {
                fill_round(cx, r, 4.0, ACC);
            } else if hot {
                fill_round(cx, r, 4.0, ACC_5);
            }
            let (ci, cn, cc, g) = if sel { (GLASS, GLASS, GLASS, Glow::None) } else { (ACC_3, ACC_2, ACC_3, Glow::G1) };
            scene.icon(kind_icon(&a.kind), Rect::new(r.x + 6.0 + 1.5, r.y + (rh - 15.0) / 2.0, 15.0, 15.0), 1.6, ci, false, g);
            let count = t(&self.locale, "actions.phraseCount", &[("count", &self.shown(a).len().to_string())]);
            let cw = Self::text(scene, cx, &count, &count_st, 1000.0).map(|l| {
                scene.text(&l, r.x + r.w - 6.0 - l.width, r.y + (rh - l.height) / 2.0, cc, g);
                l.width
            });
            let label = if a.label.trim().is_empty() { self.t("actions.unnamed") } else { a.label.clone() };
            let nx = r.x + 6.0 + 18.0 + 8.0;
            let nw = (r.x + r.w - 6.0 - cw.unwrap_or(0.0) - 8.0 - nx).max(20.0);
            if let Some(l) = Self::text(scene, cx, &label, &name_st, nw) {
                scene.text(&l, nx, r.y + (rh - l.height) / 2.0, cn, g);
            }
            self.rows.push((r, a.id.clone()));
            ry += rh;
        }
        self.max_scroll = (ry + self.scroll - list.y - list.h).max(0.0);
        self.scroll = self.scroll.min(self.max_scroll);
        scene.flush(cx);
        scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
        // поле поиска и кнопка — поверх
        self.search.draw(scene, cx);
        scene.flush(cx);
        self.create.draw(scene, cx);
        self.scan.draw(scene, cx);
        if self.scan_tip.rect.w > 0.0 && inside(list, self.scan_tip.rect.x, self.scan_tip.rect.y + 27.0) {
            self.scan_tip.draw(scene, cx);
        }
        scene.flush(cx);

        // редактор справа: пунктир слева, отступ 14
        let ex = body.x + LIST_W + 14.0;
        let mut dy = body.y;
        while dy < body.y + body.h {
            cx.fill_rect(Rect::new(ex, dy, 1.0, 1.0), ACC_4);
            dy += 2.0;
        }
        let edit = Rect::new(ex + 15.0, body.y, body.x + body.w - ex - 15.0, body.h);
        self.edit_rect = edit;
        if self.draft.is_none() {
            let st = TextStyle { align: Align::Center, wrap: true, line_height: Some(1.3), ..lcd(17.0, 500, 0.06) };
            let pick = self.t("actions.pick").replace("<br />", "\n").replace("<br>", "\n").to_uppercase();
            if let Some(l) = Self::text(scene, cx, &pick, &st, edit.w) {
                scene.text(&l, edit.x, edit.y + (edit.h - l.height) / 2.0, ACC_3, Glow::G1);
            }
            scene.flush(cx);
            return;
        }
        // поля могут не влезть по высоте — прокрутка колесом; выпадающему списку — запас снизу
        let clip = Rect::new(edit.x - 2.0, edit.y - 2.0, edit.w + 4.0, edit.h + 4.0);
        scene.set_clip(Some(clip));
        unsafe { cx.dc().PushAxisAlignedClip(&clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        let h = self.form.draw(scene, cx, edit.x, edit.y - self.edit_scroll, edit.w);
        self.edit_max = (h - edit.h).max(0.0);
        scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
    }

    fn dir(&self) -> Direction {
        if self.rtl { Direction::Rtl } else { Direction::Ltr }
    }

    pub fn next_blink(&self, now: Instant) -> Option<Instant> {
        if self.search_focus {
            self.search.next_blink(now)
        } else {
            self.form.next_blink(now)
        }
    }

    pub fn ime_caret(&self) -> Option<(f32, f32, f32)> {
        if self.search_focus {
            self.search.ime_caret()
        } else {
            self.form.ime_caret()
        }
    }

    pub fn window_focus(&mut self, on: bool) {
        self.search.set_focus(on && self.search_focus);
        self.form.window_focus(on);
    }

    // ---------- события ----------

    fn hover_at(&self, x: f32, y: f32) -> Option<Hover> {
        if inside(self.create.rect, x, y) {
            return Some(Hover::Create);
        }
        if inside(self.scan.rect, x, y) {
            return Some(Hover::Scan);
        }
        if self.scan_tip.rect.w > 0.0 && inside(self.scan_tip.rect, x, y) && inside(self.list_rect, x, y) {
            return Some(Hover::ScanTip);
        }
        if let Some(i) = self.filters.iter().position(|r| inside(*r, x, y)) {
            return Some(Hover::Filter(i));
        }
        if inside(self.list_rect, x, y) {
            return self.rows.iter().find(|(r, _)| inside(*r, x, y)).map(|(_, id)| Hover::Row(id.clone()));
        }
        None
    }

    /// Кликабельное (рука), поле (текстовый курсор) или ничего.
    pub fn cursor(&self, x: f32, y: f32) -> Option<bool> {
        if self.form.popup_open() {
            return Some(true);
        }
        if inside(self.search.rect, x, y) {
            return Some(false);
        }
        if self.hover_at(x, y).is_some() {
            return Some(true);
        }
        if inside(self.edit_rect, x, y) {
            return self.form.hover_kind(x, y);
        }
        None
    }

    fn over_form(&self, x: f32, y: f32) -> bool {
        self.draft.is_some() && (self.form.popup_open() || inside(self.edit_rect, x, y))
    }

    pub fn mouse(&mut self, ev: MouseEvent) -> (bool, Option<ActionsOut>) {
        let (x, y) = (ev.x, ev.y);
        match ev.kind {
            MouseKind::Move => {
                let mut redraw = self.search.mouse_move(x);
                let h = self.hover_at(x, y);
                if h != self.hover {
                    self.hover = h.clone();
                    redraw = true;
                }
                redraw |= self.create.press.set_hover(h == Some(Hover::Create), 0.12);
                redraw |= self.scan.press.set_hover(h == Some(Hover::Scan), 0.12);
                redraw |= self.scan_tip.press.set_hover(h == Some(Hover::ScanTip), 0.12);
                let r = self.form.mouse(ev);
                let (r2, out) = self.form_result(r);
                (redraw || r2, out)
            }
            MouseKind::Leave => {
                self.hover = None;
                self.create.press.set_hover(false, 0.12);
                let r = self.form.mouse(ev);
                self.form_result(r);
                (true, None)
            }
            MouseKind::Down(MouseButton::Left) | MouseKind::DoubleClick => {
                if self.over_form(x, y) {
                    self.set_search_focus(false);
                    let r = self.form.mouse(ev);
                    return self.form_result(r);
                }
                // щелчок вне формы снимает фокус её полей
                let r = self.form.mouse(MouseEvent { kind: MouseKind::Down(MouseButton::Left), x: -1e5, y: -1e5 });
                self.form_result(r);
                if inside(self.search.rect, x, y) {
                    self.set_search_focus(true);
                    if ev.kind == MouseKind::DoubleClick {
                        self.search.double_click(x);
                    } else {
                        self.search.mouse_down(x, false);
                    }
                    return (true, None);
                }
                self.set_search_focus(false);
                let h = self.hover_at(x, y);
                match h {
                    Some(Hover::Create) => self.create.press.pressed = true,
                    Some(Hover::Scan) => self.scan.press.pressed = true,
                    Some(Hover::ScanTip) => self.scan_tip.press.pressed = true,
                    _ => {}
                }
                self.pressed = h;
                (true, None)
            }
            MouseKind::Up(MouseButton::Left) => {
                self.search.mouse_up();
                self.create.press.pressed = false;
                self.scan.press.pressed = false;
                self.scan_tip.press.pressed = false;
                let r = self.form.mouse(ev);
                let (redraw, out) = self.form_result(r);
                if out.is_some() {
                    self.pressed = None;
                    return (true, out);
                }
                let p = self.pressed.take();
                if p.is_some() && p == self.hover_at(x, y) {
                    match p.unwrap() {
                        Hover::Create => self.go(None),
                        Hover::Scan | Hover::ScanTip => return (true, Some(ActionsOut::Scan)),
                        Hover::Filter(i) => {
                            self.filter = i;
                            self.scroll = 0.0;
                        }
                        Hover::Row(id) => self.go(Some(id)),
                    }
                    return (true, None);
                }
                (redraw || p.is_some(), None)
            }
            _ => (false, None),
        }
    }

    fn set_search_focus(&mut self, on: bool) {
        self.search_focus = on;
        self.search.set_focus(on);
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        if inside(self.list_rect, x, y) && self.max_scroll > 0.0 {
            self.scroll = (self.scroll - delta * 48.0).clamp(0.0, self.max_scroll);
            return true;
        }
        if inside(self.edit_rect, x, y) && self.edit_max > 0.0 {
            self.edit_scroll = (self.edit_scroll - delta * 48.0).clamp(0.0, self.edit_max);
            return true;
        }
        false
    }

    pub fn key(&mut self, ev: KeyEvent) -> (bool, Option<ActionsOut>) {
        let k = ev.key as u16;
        if self.search_focus {
            if k == VK_TAB.0 || k == VK_ESCAPE.0 {
                self.set_search_focus(false);
                return (true, None);
            }
            let r = self.search.key(ev);
            if r.changed {
                self.scroll = 0.0;
            }
            return (r.redraw || r.handled, None);
        }
        let r = self.form.key(ev);
        self.form_result(r)
    }

    pub fn char_input(&mut self, c: char) -> (bool, Option<ActionsOut>) {
        if self.search_focus {
            let r = self.search.char_input(c);
            if r.changed {
                self.scroll = 0.0;
            }
            return (r.redraw, None);
        }
        let r = self.form.char_input(c);
        self.form_result(r)
    }

    fn form_result(&mut self, r: FormResult) -> (bool, Option<ActionsOut>) {
        let redraw = r.redraw;
        let Some(ev) = r.event else { return (redraw, None) };
        let out = self.on_event(ev);
        (true, out)
    }

    fn on_event(&mut self, ev: FormEvent) -> Option<ActionsOut> {
        let draft = self.draft.as_mut()?;
        match ev {
            FormEvent::Edited(id, v) => {
                match id.as_str() {
                    "name" => draft.label = v,
                    "target" => draft.target = v,
                    _ => return None,
                }
                self.dirty = true;
                None
            }
            FormEvent::Select(id, v) if id == "kind" => {
                draft.kind = v;
                self.dirty = true;
                self.rebuild();
                None
            }
            FormEvent::Remove(id, i) if id == "phrases" => {
                // фраза убирается сразу (как remove_phrase в вебе); заодно сохраняются правки
                let shown = {
                    let a = draft.clone();
                    self.shown(&a)
                };
                let draft = self.draft.as_mut()?;
                if let Some(p) = shown.get(i) {
                    if let Some(pos) = draft.phrases.iter().position(|x| x == p) {
                        draft.phrases.remove(pos);
                    }
                }
                self.commit()
            }
            FormEvent::Add(id, v) if id == "phrases+" => {
                // как add_phrase: строчными, без повторов; действие сохраняется
                let p = v.trim().to_lowercase();
                if !p.is_empty() && !draft.phrases.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                    draft.phrases.push(p);
                }
                self.commit()
            }
            FormEvent::Click(id) => match id.as_str() {
                "save" => {
                    if draft.label.trim().is_empty() {
                        self.notice = vec![self.t("actions.needName")];
                        self.rebuild();
                        return None;
                    }
                    self.commit()
                }
                "pickapp" => Some(ActionsOut::PickApp),
                "calibrate" => {
                    let a = draft.clone();
                    self.commit();
                    Some(ActionsOut::Calibrate(a))
                }
                "delete" => {
                    self.pending = Some(Pending::Delete);
                    self.rebuild();
                    None
                }
                "unconfirm" => {
                    self.pending = None;
                    self.rebuild();
                    None
                }
                "confirm" => match self.pending.take() {
                    Some(Pending::Switch(next)) => {
                        self.dirty = false;
                        self.select(next);
                        None
                    }
                    Some(Pending::Delete) => {
                        let id = draft.id.clone();
                        let saved = self.actions.iter().any(|a| a.id == id);
                        self.dirty = false;
                        self.sel = None;
                        self.draft = None;
                        self.load_form();
                        saved.then_some(ActionsOut::Delete(id))
                    }
                    None => None,
                },
                _ => None,
            },
            FormEvent::Escape => {
                if self.pending.take().is_some() {
                    self.rebuild();
                }
                None
            }
            _ => None,
        }
    }

    /// Сохранить черновик: правки считаются сохранёнными, список обновит программа.
    fn commit(&mut self) -> Option<ActionsOut> {
        let draft = self.draft.clone()?;
        self.dirty = false;
        self.notice.clear();
        self.pending = None;
        if let Some(a) = self.actions.iter_mut().find(|a| a.id == draft.id) {
            *a = draft.clone();
        } else {
            self.actions.push(draft.clone());
        }
        self.rebuild();
        Some(ActionsOut::Save(draft))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> ActionsTab {
        let mut t = ActionsTab::new("en", false, 0);
        let a = |id: &str, kind: &str, label: &str, phrases: &[&str]| Action {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            target: String::new(),
            verbs: vec![],
            phrases: phrases.iter().map(|s| s.to_string()).collect(),
        };
        t.sync(
            vec![a("a", "shell", "Notepad", &["notepad", "блокнот"]), a("b", "url", "Site", &["site"]), a("c", "folder", "Docs", &["docs"])],
            HashMap::from([("a".to_string(), vec!["блокнот".to_string()])]),
        );
        t
    }

    #[test]
    fn filter_and_search_match_like_the_web_list() {
        let mut t = tab();
        assert_eq!(t.actions.iter().filter(|a| t.visible(a)).count(), 3);
        t.filter = 3; // web
        let v: Vec<_> = t.actions.iter().filter(|a| t.visible(a)).map(|a| a.id.clone()).collect();
        assert_eq!(v, ["b"]);
        t.filter = 0;
        t.search.set_text("блокнот"); // скрытая фраза не ищется
        assert_eq!(t.actions.iter().filter(|a| t.visible(a)).count(), 0);
    }

    #[test]
    fn switching_with_unsaved_edits_asks_first() {
        let mut t = tab();
        t.go(Some("a".into()));
        t.on_event(FormEvent::Edited("name".into(), "Editor".into()));
        t.go(Some("b".into()));
        assert_eq!(t.sel.as_deref(), Some("a"));
        assert!(matches!(t.pending, Some(Pending::Switch(_))));
        t.on_event(FormEvent::Click("confirm".into()));
        assert_eq!(t.sel.as_deref(), Some("b"));
        assert!(!t.dirty);
    }

    #[test]
    fn removing_a_phrase_keeps_hidden_ones_and_saves() {
        let mut t = tab();
        t.go(Some("a".into()));
        let out = t.on_event(FormEvent::Remove("phrases".into(), 0));
        match out {
            Some(ActionsOut::Save(a)) => assert_eq!(a.phrases, ["блокнот"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn new_action_needs_a_name_and_unsaved_new_one_is_dropped_on_delete() {
        let mut t = tab();
        t.go(None);
        assert_eq!(t.on_event(FormEvent::Click("save".into())), None);
        assert!(!t.notice.is_empty());
        t.on_event(FormEvent::Click("delete".into()));
        assert_eq!(t.on_event(FormEvent::Click("confirm".into())), None);
        assert!(t.draft.is_none());
    }
}
