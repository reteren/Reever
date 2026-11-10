//! Вкладка «Индекс»: поиск по своему индексу дисков прямо в главном окне — поле, «Всё / Папки /
//! Файлы» и список найденного. Ищет на каждую букву (`find::search_typed`, как поле окна
//! выбора), выбор — щелчком или стрелками, открыть — двойным щелчком или Enter. Голосом
//! («ривер индекс», «ривер найди …») открывается отдельное окно выбора (picker.rs), а не она.

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_UP};

use crate::find::{FindItem, FindScope};
use crate::native::controls::button::ButtonKind;
use crate::native::controls::form::{Form, FormEvent, FormResult, Item};
use crate::native::controls::{fill_round, inside, lcd, ui, Glow, Scene, ACC_2, ACC_3, ACC_4, ACC_5, ACC_HOT};
use crate::native::{KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx, TextStyle};
use crate::ui_text::t;

const ROW_H: f32 = 44.0;
const SCOPES: [(FindScope, &str, &str); 3] = [
    (FindScope::Any, "scope:any", "picker.scopeAll"),
    (FindScope::Folder, "scope:folder", "picker.scopeFolders"),
    (FindScope::File, "scope:file", "picker.scopeFiles"),
];

/// Что вкладка просит у программы.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexOut {
    /// Искать набранное; ответ — `IndexTab::results` с тем же `seq`.
    Search { seq: u64, query: String, scope: FindScope },
    /// Открыть найденное: папку — в Проводнике, файл — программой по умолчанию.
    Open(FindItem),
}

pub struct IndexTab {
    pub form: Form,
    locale: String,
    rtl: bool,
    scope: FindScope,
    query: String,
    items: Vec<FindItem>,
    /// ошибка поиска (индекс выключен или строится)
    note: String,
    seq: u64,
    searching: bool,
    selected: usize,
    hover: Option<usize>,
    /// строки списка на экране (с учётом прокрутки) — для мыши и диктора
    pub rows: Vec<Rect>,
    reveal: bool,
    scroll: f32,
    max_scroll: f32,
    view: Rect,
    revealed: Option<(i32, i32)>,
}

impl IndexTab {
    pub fn new(locale: &str, rtl: bool, hwnd: isize) -> Self {
        let mut tab = Self {
            form: Form::new(hwnd, rtl),
            locale: locale.to_string(),
            rtl,
            scope: FindScope::Any,
            query: String::new(),
            items: Vec::new(),
            note: String::new(),
            seq: 0,
            searching: false,
            selected: 0,
            hover: None,
            rows: Vec::new(),
            reveal: false,
            scroll: 0.0,
            max_scroll: 0.0,
            view: Rect::new(0.0, 0.0, 0.0, 0.0),
            revealed: None,
        };
        tab.rebuild();
        tab
    }

    pub fn set_hwnd(&mut self, hwnd: isize) {
        self.form = Form::new(hwnd, self.rtl);
        self.rebuild();
        self.form.set_value("q", &self.query);
    }

    fn t(&self, key: &str) -> String {
        t(&self.locale, key, &[])
    }

    /// Вкладку открыли — курсор сразу в поле поиска.
    pub fn focus(&mut self) {
        self.form.focus_input("q");
    }

    fn rebuild(&mut self) {
        let buttons = SCOPES
            .iter()
            .map(|(scope, id, key)| (id.to_string(), self.t(key), if *scope == self.scope { ButtonKind::Default } else { ButtonKind::Plain }))
            .collect();
        self.form.set_items(vec![
            Item::Input { id: "q".into(), placeholder: self.t("picker.searchPlaceholder"), value: self.query.clone() },
            Item::Buttons { buttons, top: 8.0, bottom: 0.0 },
        ]);
    }

    fn search(&mut self) -> Option<IndexOut> {
        self.seq += 1;
        if self.query.trim().is_empty() {
            self.searching = false;
            self.items.clear();
            self.note.clear();
            self.selected = 0;
            return None;
        }
        self.searching = true;
        Some(IndexOut::Search { seq: self.seq, query: self.query.clone(), scope: self.scope })
    }

    /// Ответ на поиск. Устаревший (набрали дальше) не трогает список.
    pub fn results(&mut self, seq: u64, result: Result<Vec<FindItem>, String>) {
        if seq != self.seq {
            return;
        }
        self.searching = false;
        match result {
            Ok(items) => {
                self.items = items;
                self.note.clear();
            }
            Err(e) => {
                self.items.clear();
                self.note = if e.contains("index-off") { self.t("picker.indexOff") } else { e };
            }
        }
        self.selected = 0;
        self.hover = None;
        self.scroll = 0.0;
    }

    fn status(&self) -> String {
        if !self.note.is_empty() {
            return self.note.clone();
        }
        if self.query.trim().is_empty() {
            return self.t("picker.typeToSearch");
        }
        if self.items.is_empty() {
            return if self.searching { "…".into() } else { self.t("picker.nothing") };
        }
        t(&self.locale, "picker.count", &[("count", &self.items.len().to_string())])
    }

    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>, body: Rect) {
        let clip = Rect::new(body.x - 18.0, body.y - 14.0, body.w + 36.0, body.h + 14.0 + 18.0);
        self.view = clip;
        scene.flush(cx);
        scene.set_clip(Some(clip));
        unsafe { cx.dc().PushAxisAlignedClip(&clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        let mut y = body.y - self.scroll;
        let title = self.t("nav.index").to_uppercase();
        if let Some(l) = scene.cache.get(cx.text(), &title, &lcd(22.0, 600, 0.08), body.w) {
            scene.text(&l, body.x, y + 2.0, ACC_HOT, Glow::G1);
            y += 2.0 + l.height + 12.0;
        }
        y += self.form.draw(scene, cx, body.x, y, body.w);
        if let Some(r) = self.form.focus_rect() {
            let key = (r.x as i32, (r.y + self.scroll) as i32);
            if self.revealed != Some(key) {
                self.revealed = Some(key);
                self.scroll = (self.scroll + super::reveal_delta(r, body)).max(0.0);
            }
        }
        y += 14.0;
        let status = self.status();
        if let Some(l) = scene.cache.get(cx.text(), &status, &ui(12.0, 400, 1.3), body.w) {
            scene.text(&l, body.x, y, ACC_3, Glow::G1);
            y += l.height + 10.0;
        }
        self.rows.clear();
        let name_style = TextStyle { wrap: false, ..ui(14.0, 500, 1.2) };
        let path_style = TextStyle { wrap: false, ..ui(11.0, 400, 1.2) };
        for (i, item) in self.items.iter().enumerate() {
            let row = Rect::new(body.x, y, body.w - 10.0, ROW_H - 4.0);
            self.rows.push(row);
            if i == self.selected || self.hover == Some(i) {
                fill_round(cx, row, 6.0, ACC_5);
            }
            if i == self.selected {
                crate::native::controls::stroke_round(cx, row, 6.0, ACC_4, 1.0, false);
                if self.reveal {
                    self.reveal = false;
                    self.scroll = (self.scroll + super::reveal_delta(row, body)).max(0.0);
                }
            }
            let on = i == self.selected;
            let icon = Rect::new(row.x + 8.0, row.y + (row.h - 20.0) / 2.0, 20.0, 20.0);
            scene.icon(if item.folder { "folder" } else { "file" }, icon, 1.5, if on { ACC_HOT } else { ACC_2 }, false, Glow::G1);
            // длинные имена и пути обрезаются по строке
            let text_r = Rect::new(row.x + 36.0, row.y, row.w - 44.0, row.h);
            scene.flush(cx);
            unsafe { cx.dc().PushAxisAlignedClip(&text_r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
            if let Some(l) = scene.cache.get(cx.text(), &item.name, &name_style, 10_000.0) {
                scene.text(&l, text_r.x, row.y + 4.0, if on { ACC_HOT } else { ACC_2 }, Glow::G1);
            }
            if let Some(l) = scene.cache.get(cx.text(), &item.path, &path_style, 10_000.0) {
                scene.text(&l, text_r.x, row.y + 22.0, ACC_3, Glow::G1);
            }
            scene.flush(cx);
            unsafe { cx.dc().PopAxisAlignedClip() };
            y += ROW_H;
        }
        y += 18.0;
        self.max_scroll = (y + self.scroll - body.y - body.h).max(0.0);
        self.scroll = self.scroll.min(self.max_scroll);
        scene.flush(cx);
        scene.set_clip(None);
        unsafe { cx.dc().PopAxisAlignedClip() };
        if self.max_scroll > 0.0 {
            let track = crate::native::controls::mirror(clip, Rect::new(clip.x + clip.w - 7.0, body.y, 3.0, body.h), self.rtl);
            let total = body.h + self.max_scroll;
            let th = (track.h * body.h / total).max(24.0);
            let ty = track.y + (track.h - th) * self.scroll / self.max_scroll;
            fill_round(cx, Rect::new(track.x, ty, track.w, th), 1.5, ACC_4);
        }
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta: f32) -> bool {
        if !inside(self.view, x, y) || self.max_scroll <= 0.0 {
            return false;
        }
        let next = (self.scroll - delta * 48.0).clamp(0.0, self.max_scroll);
        let moved = next != self.scroll;
        self.scroll = next;
        moved
    }

    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if !inside(self.view, x, y) {
            return None;
        }
        self.rows.iter().position(|r| inside(*r, x, y))
    }

    pub fn mouse(&mut self, ev: MouseEvent) -> (bool, Option<IndexOut>) {
        let r = self.form.mouse(ev);
        let (mut redraw, out) = self.form_result(r);
        if out.is_some() {
            return (true, out);
        }
        let row = match ev.kind {
            MouseKind::Leave => None,
            _ => self.row_at(ev.x, ev.y),
        };
        match ev.kind {
            MouseKind::Move | MouseKind::Leave => {
                if self.hover != row {
                    self.hover = row;
                    redraw = true;
                }
            }
            MouseKind::Down(MouseButton::Left) => {
                if let Some(i) = row {
                    self.selected = i;
                    redraw = true;
                }
            }
            MouseKind::DoubleClick => {
                if let Some(i) = row {
                    self.selected = i;
                    return (true, self.items.get(i).cloned().map(IndexOut::Open));
                }
            }
            _ => {}
        }
        (redraw, None)
    }

    /// Рука над строкой списка и кнопками, текстовый курсор над полем.
    pub fn cursor(&self, x: f32, y: f32) -> Option<bool> {
        if self.row_at(x, y).is_some() {
            return Some(true);
        }
        self.form.hover_kind(x, y)
    }

    pub fn a11y_action(&mut self, base: u64, req: &crate::native::a11y::accesskit::ActionRequest) -> Option<(bool, Option<IndexOut>)> {
        let r = self.form.a11y_action(base, req)?;
        Some(self.form_result(r))
    }

    /// Диктор нажал строку списка: открыть её.
    pub fn open_row(&mut self, i: usize) -> Option<IndexOut> {
        let item = self.items.get(i)?.clone();
        self.selected = i;
        Some(IndexOut::Open(item))
    }

    /// Найденное для диктора: имя, путь, выбрана ли.
    pub fn a11y_rows(&self) -> Vec<(String, String, bool)> {
        self.items.iter().enumerate().map(|(i, it)| (it.name.clone(), it.path.clone(), i == self.selected)).collect()
    }

    pub fn key(&mut self, ev: KeyEvent) -> (bool, Option<IndexOut>) {
        let k = ev.key as u16;
        let n = self.items.len();
        if n > 0 && !ev.ctrl && !ev.alt {
            let page = ((self.view.h / ROW_H) as usize).max(1);
            let next = if k == VK_DOWN.0 {
                Some((self.selected + 1).min(n - 1))
            } else if k == VK_UP.0 {
                Some(self.selected.saturating_sub(1))
            } else if k == VK_NEXT.0 {
                Some((self.selected + page).min(n - 1))
            } else if k == VK_PRIOR.0 {
                Some(self.selected.saturating_sub(page))
            } else if (k == VK_HOME.0 || k == VK_END.0) && ev.shift {
                // Home/End без Shift остаются полю ввода
                Some(if k == VK_HOME.0 { 0 } else { n - 1 })
            } else {
                None
            };
            if let Some(i) = next {
                self.selected = i;
                self.reveal = true;
                return (true, None);
            }
            if k == VK_RETURN.0 && self.form.value("q").is_some() {
                // Enter в поле, пока ответ на набранное не пришёл, не открывает старый список
                if self.searching {
                    return (false, None);
                }
                return (true, self.items.get(self.selected).cloned().map(IndexOut::Open));
            }
        }
        let r = self.form.key(ev);
        self.form_result(r)
    }

    pub fn char_input(&mut self, c: char) -> (bool, Option<IndexOut>) {
        let r = self.form.char_input(c);
        self.form_result(r)
    }

    fn form_result(&mut self, r: FormResult) -> (bool, Option<IndexOut>) {
        let redraw = r.redraw;
        let Some(ev) = r.event else { return (redraw, None) };
        (true, self.on_event(ev))
    }

    fn on_event(&mut self, ev: FormEvent) -> Option<IndexOut> {
        match ev {
            FormEvent::Edited(id, text) if id == "q" => {
                self.query = text;
                self.search()
            }
            FormEvent::Click(id) => {
                let (scope, ..) = SCOPES.iter().find(|(_, sid, _)| *sid == id)?;
                if *scope == self.scope {
                    return None;
                }
                self.scope = *scope;
                self.rebuild();
                self.search()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> FindItem {
        FindItem { name: name.into(), path: format!("C:\\{name}"), folder: false, frequent: false, score: 0.0 }
    }

    #[test]
    fn typing_searches_and_stale_answers_are_ignored() {
        let mut t = IndexTab::new("en", false, 0);
        let out = t.on_event(FormEvent::Edited("q".into(), "rep".into()));
        assert_eq!(out, Some(IndexOut::Search { seq: 1, query: "rep".into(), scope: FindScope::Any }));
        let out = t.on_event(FormEvent::Edited("q".into(), "repo".into()));
        assert!(matches!(out, Some(IndexOut::Search { seq: 2, .. })));
        t.results(1, Ok(vec![item("old")]));
        assert!(t.items.is_empty(), "ответ на устаревший запрос не показывается");
        t.results(2, Ok(vec![item("report"), item("repo")]));
        assert_eq!(t.items.len(), 2);
        // пустое поле — список очищается без поиска
        assert_eq!(t.on_event(FormEvent::Edited("q".into(), " ".into())), None);
        assert!(t.items.is_empty());
    }

    #[test]
    fn scope_button_repeats_the_search_in_that_scope() {
        let mut t = IndexTab::new("en", false, 0);
        t.on_event(FormEvent::Edited("q".into(), "doc".into()));
        let out = t.on_event(FormEvent::Click("scope:folder".into()));
        assert_eq!(out, Some(IndexOut::Search { seq: 2, query: "doc".into(), scope: FindScope::Folder }));
        assert_eq!(t.on_event(FormEvent::Click("scope:folder".into())), None);
    }

    #[test]
    fn arrows_move_selection_and_enter_opens_it() {
        let mut t = IndexTab::new("en", false, 0);
        t.on_event(FormEvent::Edited("q".into(), "a".into()));
        t.results(1, Ok(vec![item("a1"), item("a2"), item("a3")]));
        let key = |k: u16| KeyEvent { key: k as u32, ctrl: false, shift: false, alt: false, repeat: false };
        t.key(key(VK_DOWN.0));
        t.key(key(VK_DOWN.0));
        t.key(key(VK_DOWN.0));
        assert_eq!(t.selected, 2);
        t.key(key(VK_UP.0));
        assert_eq!(t.key(key(VK_RETURN.0)).1, Some(IndexOut::Open(item("a2"))));
    }
}
