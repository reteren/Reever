//! Клавиатура и экранный диктор главного окна (дочерний модуль `view`: видит его поля).
//!
//! Tab и Shift+Tab ходят по окну: три лунки, вкладки рельса, «Пауза», дальше — содержимое
//! вкладки («Изменить», строки «Что можно сказать» и раскрытый редактор на Главной; поиск,
//! кнопки, фильтры, список и редактор в «Действиях»; форма «Настроек»). Enter и пробел
//! нажимают, стрелки ходят по вкладкам и строкам. Рамка фокуса — только после клавиатуры.
//!
//! Дерево UI Automation собирается, только когда что-то поменялось (статичный слой
//! перерисовался или сменился фокус), а не на каждом из 180 кадров в секунду.

use accesskit::{Action, Node, NodeId, Role, Toggled, TreeId, TreeInfo, TreeUpdate};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP};

use super::*;

/// Где фокус клавиатуры окна.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) enum KFocus {
    #[default]
    None,
    Lunka(usize),
    Tab(usize),
    Pause,
    /// «Изменить» на Главной
    Web,
    /// строка «Что можно сказать»
    Row(usize),
    /// раскрытый редактор фраз (форма)
    Editor,
    /// содержимое «Действий» или «Настроек»
    Content,
}

const HOME_EDITOR: u64 = 1000;
const ACTIONS: u64 = 2000;
const SETTINGS: u64 = 6000;

impl MainView {
    /// Порядок Tab по окну.
    fn view_order(&self) -> Vec<KFocus> {
        let mut o: Vec<KFocus> = (0..3).map(KFocus::Lunka).chain((0..4).map(KFocus::Tab)).collect();
        o.push(KFocus::Pause);
        match TABS[self.tab] {
            "home" => {
                o.push(KFocus::Web);
                if self.editor.as_ref().is_some_and(|e| e.open == "general") {
                    o.push(KFocus::Editor);
                }
                for (i, row) in self.info.say.rows.iter().enumerate() {
                    if row.key.is_some() {
                        o.push(KFocus::Row(i));
                        if self.editor.as_ref().is_some_and(|e| Some(&e.open) == row.key.as_ref()) {
                            o.push(KFocus::Editor);
                        }
                    }
                }
            }
            "actions" | "settings" => o.push(KFocus::Content),
            _ => {}
        }
        o
    }

    /// Фокус пришёл в элемент: формы и вкладки ставят его на свой первый/последний элемент.
    /// false — внутри пусто (элемент пропускается).
    fn enter_focus(&mut self, f: KFocus, back: bool) -> bool {
        match f {
            KFocus::Editor => self.form.focus_edge(back),
            KFocus::Content if TABS[self.tab] == "actions" => {
                self.actions_tab.kbd_enter(back);
                true
            }
            KFocus::Content => self.settings_tab.form.focus_edge(back),
            _ => true,
        }
    }

    fn leave_focus(&mut self) {
        match self.kfocus {
            KFocus::Editor => self.form.blur(),
            KFocus::Content => {
                self.actions_tab.kbd_leave();
                self.settings_tab.form.blur();
            }
            _ => {}
        }
    }

    /// Сдвинуть фокус по окну на шаг.
    fn move_focus(&mut self, back: bool) {
        let o = self.view_order();
        let n = o.len();
        let mut i = match o.iter().position(|f| *f == self.kfocus) {
            Some(i) => i,
            // первый Tab — на текущую вкладку рельса
            None => {
                let start = o.iter().position(|f| *f == KFocus::Tab(self.tab)).unwrap_or(0);
                self.leave_focus();
                self.kfocus = o[start];
                self.kvisible = true;
                self.touch_static();
                return;
            }
        };
        self.leave_focus();
        for _ in 0..n {
            i = if back { (i + n - 1) % n } else { (i + 1) % n };
            if self.enter_focus(o[i], back) {
                self.kfocus = o[i];
                break;
            }
        }
        self.kvisible = true;
        self.touch_static();
    }

    /// Фокус клавиатуры уже внутри формы или поиска (поставлен мышью) — учесть его.
    fn adopt_focus(&mut self) {
        if !matches!(self.kfocus, KFocus::None) {
            return;
        }
        match TABS[self.tab] {
            "home" if self.form.has_focus() => self.kfocus = KFocus::Editor,
            "actions" if self.actions_tab.kbd_active() => self.kfocus = KFocus::Content,
            "settings" if self.settings_tab.form.has_focus() => self.kfocus = KFocus::Content,
            _ => {}
        }
    }

    /// Клавиша на уровне окна. None — пусть её разберёт вкладка (поля ввода и т. п.).
    pub(super) fn kbd(&mut self, ev: KeyEvent) -> Option<bool> {
        let k = ev.key as u16;
        if ev.alt || (ev.ctrl && k != VK_TAB.0) {
            return None;
        }
        if TABS[self.tab] == "settings" && self.settings_tab.recording() {
            return None;
        }
        self.adopt_focus();
        let tab = k == VK_TAB.0 && !ev.ctrl;
        match self.kfocus {
            KFocus::Content if TABS[self.tab] == "actions" => {
                return match self.actions_tab.kbd_key(ev) {
                    crate::main_native::actions_tab::Kbd::Leave(back) => {
                        self.move_focus(back);
                        Some(true)
                    }
                    crate::main_native::actions_tab::Kbd::Handled(redraw, out) => {
                        if let Some(out) = out {
                            self.actions_out(out);
                        }
                        if redraw {
                            self.touch_static();
                        }
                        self.a11y_dirty = true;
                        Some(redraw)
                    }
                };
            }
            KFocus::Content | KFocus::Editor => {
                let form = if self.kfocus == KFocus::Editor { &self.form } else { &self.settings_tab.form };
                if tab && form.at_edge(!ev.shift) {
                    self.move_focus(ev.shift);
                    return Some(true);
                }
                self.a11y_dirty = true;
                return None; // форма разберёт сама (вкладка шлёт ей клавиши)
            }
            _ => {}
        }
        if tab {
            self.move_focus(ev.shift);
            return Some(true);
        }
        if self.kfocus == KFocus::None {
            return None;
        }
        if k == VK_ESCAPE.0 {
            self.kfocus = KFocus::None;
            self.kvisible = false;
            self.touch_static();
            return Some(true);
        }
        if k == VK_RETURN.0 || k == VK_SPACE.0 {
            self.kswallow = k == VK_SPACE.0;
            let t = match self.kfocus {
                KFocus::Lunka(i) => Some(Target::Lunka(i)),
                KFocus::Tab(i) => Some(Target::Tab(i)),
                KFocus::Pause => Some(Target::Pause),
                KFocus::Web => Some(Target::Web),
                KFocus::Row(i) => Some(Target::Row(i)),
                _ => None,
            };
            if let Some(t) = t {
                self.activate(t);
                self.a11y_dirty = true;
                self.touch_static();
                return Some(true);
            }
            return None;
        }
        let step: i32 = if k == VK_DOWN.0 || k == VK_RIGHT.0 {
            1
        } else if k == VK_UP.0 || k == VK_LEFT.0 {
            -1
        } else {
            0
        };
        if step != 0 {
            match self.kfocus {
                KFocus::Tab(i) => {
                    self.kfocus = KFocus::Tab((i as i32 + step).rem_euclid(4) as usize);
                }
                KFocus::Row(i) => {
                    let keyed: Vec<usize> = self.info.say.rows.iter().enumerate().filter(|(_, r)| r.key.is_some()).map(|(i, _)| i).collect();
                    if let Some(p) = keyed.iter().position(|x| *x == i) {
                        let q = (p as i32 + step).clamp(0, keyed.len() as i32 - 1) as usize;
                        self.kfocus = KFocus::Row(keyed[q]);
                    }
                }
                _ => return None,
            }
            self.touch_static();
            self.a11y_dirty = true;
            return Some(true);
        }
        None
    }

    /// Рамка фокуса элемента окна (:focus-visible).
    pub(super) fn draw_focus_ring(&mut self, cx: &mut RenderCx<'_>) {
        if !self.kvisible || self.overlay.is_some() {
            return;
        }
        let r = match self.kfocus {
            KFocus::Lunka(i) => Some(self.lunki[i].rect()),
            KFocus::Tab(i) => Some(Self::tab_rect(i)),
            KFocus::Pause => Some(self.pause.rect),
            KFocus::Web if TABS[self.tab] == "home" => Some(self.web.rect),
            KFocus::Row(i) => self.say_rects.get(i).copied(),
            _ => None,
        };
        if let Some(r) = r {
            native::controls::stroke_round(cx, Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0), 6.0, ACC_2, 1.0, true);
        }
    }

    /// Дерево для диктора; None — ничего не менялось.
    pub(super) fn build_a11y(&mut self, scale: f32) -> Option<TreeUpdate> {
        if !self.a11y_dirty {
            return None;
        }
        self.a11y_dirty = false;
        let k = self.k;
        let s = scale * k; // координаты скина → пиксели окна
        let b = |r: Rect| native::a11y::bounds(r.x, r.y, r.w, r.h, s);
        let mut nodes: Vec<(NodeId, Node)> = Vec::new();
        let mut kids = Vec::new();
        // лунки
        for (i, l) in self.lunki.iter().enumerate() {
            let id = NodeId(2 + i as u64);
            let mut n = Node::new(Role::Button);
            n.set_label(l.tip.clone());
            n.set_bounds(b(l.rect()));
            n.add_action(Action::Click);
            n.add_action(Action::Focus);
            nodes.push((id, n));
            kids.push(id);
        }
        // рельс вкладок
        let mut tabs = Node::new(Role::TabList);
        tabs.set_label(self.t("nav.sections"));
        tabs.set_bounds(b(RAIL));
        let mut tab_ids = Vec::new();
        for i in 0..4 {
            let id = NodeId(6 + i as u64);
            let mut n = Node::new(Role::Tab);
            n.set_label(self.t(TAB_KEYS[i]));
            n.set_selected(i == self.tab);
            n.set_bounds(b(Self::tab_rect(i)));
            n.add_action(Action::Click);
            n.add_action(Action::Focus);
            nodes.push((id, n));
            tab_ids.push(id);
        }
        tabs.set_children(tab_ids);
        nodes.push((NodeId(5), tabs));
        kids.push(NodeId(5));
        // состояние, бегущая строка, услышанное
        let state_key = match self.state.as_str() {
            s @ ("listening" | "busy" | "loading" | "paused" | "error") => format!("mic.{s}"),
            _ => "mic.idle".into(),
        };
        for (id, text, r) in [
            (10u64, self.t(&state_key), STATUS),
            (12, self.partial.clone(), SCREEN),
            (11, self.heard.clone().unwrap_or_else(|| self.info.say.hint.clone()), SCREEN),
        ] {
            if text.trim().is_empty() {
                continue;
            }
            let mut n = Node::new(Role::Label);
            n.set_value(text);
            n.set_bounds(b(r));
            nodes.push((NodeId(id), n));
            kids.push(NodeId(id));
        }
        let mut pause = Node::new(Role::Button);
        pause.set_label(self.pause.label.clone());
        pause.set_bounds(b(self.pause.rect));
        pause.add_action(Action::Click);
        pause.add_action(Action::Focus);
        nodes.push((NodeId(13), pause));
        kids.push(NodeId(13));
        // содержимое вкладки
        let mut content = Node::new(Role::Group);
        content.set_label(self.t(TAB_KEYS[self.tab]));
        content.set_bounds(b(SCREEN));
        let mut ckids = Vec::new();
        let mut focus = None;
        match TABS[self.tab] {
            "home" => {
                let mut web = Node::new(Role::Button);
                web.set_label(self.web.label.clone());
                web.set_bounds(b(self.web.rect));
                web.add_action(Action::Click);
                web.add_action(Action::Focus);
                nodes.push((NodeId(21), web));
                ckids.push(NodeId(21));
                let mut list = Node::new(Role::List);
                list.set_label(self.t("grammar.heading"));
                let mut rows = Vec::new();
                let (fnodes, fkids, ffocus) = if self.editor.is_some() { self.form.a11y(HOME_EDITOR, s) } else { (Vec::new(), Vec::new(), None) };
                for (i, row) in self.info.say.rows.iter().enumerate() {
                    let id = NodeId(100 + i as u64);
                    let mut n = Node::new(if row.key.is_some() { Role::Button } else { Role::ListItem });
                    n.set_label(format!("{}: {}", row.command, row.desc));
                    if let Some(r) = self.say_rects.get(i) {
                        n.set_bounds(b(*r));
                    }
                    if row.key.is_some() {
                        let open = self.editor.as_ref().is_some_and(|e| Some(&e.open) == row.key.as_ref());
                        n.set_expanded(open);
                        n.add_action(Action::Click);
                        n.add_action(Action::Focus);
                    }
                    if row.off {
                        n.set_disabled();
                    }
                    nodes.push((id, n));
                    rows.push(id);
                }
                list.set_children(rows);
                nodes.push((NodeId(22), list));
                ckids.push(NodeId(22));
                nodes.extend(fnodes);
                ckids.extend(fkids);
                if self.kfocus == KFocus::Editor {
                    focus = ffocus;
                }
            }
            "logs" => {
                let mut list = Node::new(Role::List);
                list.set_label(self.t("nav.log"));
                let mut lines = Vec::new();
                let n0 = self.log.len().saturating_sub(40);
                for (i, line) in self.log.iter().skip(n0).enumerate() {
                    let id = NodeId(300 + i as u64);
                    let mut n = Node::new(Role::ListItem);
                    n.set_label(line.clone());
                    nodes.push((id, n));
                    lines.push(id);
                }
                list.set_children(lines);
                nodes.push((NodeId(23), list));
                ckids.push(NodeId(23));
            }
            "actions" => {
                let (anodes, akids, afocus) = self.actions_tab.a11y(ACTIONS, s);
                nodes.extend(anodes);
                ckids.extend(akids);
                if self.kfocus == KFocus::Content {
                    focus = afocus;
                }
            }
            _ => {
                let (fnodes, fkids, ffocus) = self.settings_tab.form.a11y(SETTINGS, s);
                nodes.extend(fnodes);
                ckids.extend(fkids);
                if self.kfocus == KFocus::Content {
                    focus = ffocus;
                }
            }
        }
        content.set_children(ckids);
        nodes.push((NodeId(20), content));
        kids.push(NodeId(20));
        // окно поверх — диалог с заголовком (его кнопки — мышью и Enter/Esc)
        if let Some(o) = &self.overlay {
            let mut d = Node::new(Role::Dialog);
            d.set_label(o.title());
            d.set_modal();
            nodes.push((NodeId(30), d));
            kids.push(NodeId(30));
        }
        let mut root = Node::new(Role::Window);
        root.set_label("Reever".to_string());
        root.set_children(kids);
        root.set_bounds(native::a11y::bounds(0.0, 0.0, SKIN_W, SKIN_H, s));
        nodes.push((NodeId(1), root));
        let focus = match self.kfocus {
            KFocus::Lunka(i) => NodeId(2 + i as u64),
            KFocus::Tab(i) => NodeId(6 + i as u64),
            KFocus::Pause => NodeId(13),
            KFocus::Web => NodeId(21),
            KFocus::Row(i) => NodeId(100 + i as u64),
            _ => focus.unwrap_or(if self.overlay.is_some() { NodeId(30) } else { NodeId(1) }),
        };
        let _ = Toggled::False;
        Some(TreeUpdate { nodes, tree: Some(TreeInfo::new(NodeId(1))), tree_id: TreeId::ROOT, focus })
    }

    /// Действие диктора (нажать, перевести фокус, ввести текст).
    pub(super) fn a11y_do(&mut self, req: accesskit::ActionRequest) -> bool {
        let id = req.target_node.0;
        self.a11y_dirty = true;
        self.kvisible = true;
        let click = req.action == Action::Click;
        let focus = req.action == Action::Focus;
        let target = match id {
            2..=4 => Some((KFocus::Lunka((id - 2) as usize), Target::Lunka((id - 2) as usize))),
            6..=9 => Some((KFocus::Tab((id - 6) as usize), Target::Tab((id - 6) as usize))),
            13 => Some((KFocus::Pause, Target::Pause)),
            21 => Some((KFocus::Web, Target::Web)),
            100..=299 => Some((KFocus::Row((id - 100) as usize), Target::Row((id - 100) as usize))),
            _ => None,
        };
        if let Some((f, t)) = target {
            self.leave_focus();
            self.kfocus = f;
            if click {
                self.activate(t);
            }
            self.touch_static();
            return click || focus;
        }
        if (HOME_EDITOR..ACTIONS).contains(&id) {
            if let Some(r) = self.form.a11y_action(HOME_EDITOR, &req) {
                self.kfocus = KFocus::Editor;
                return self.editor_event(r) || true;
            }
        } else if (ACTIONS..SETTINGS).contains(&id) {
            if let Some((redraw, out)) = self.actions_tab.a11y_action(ACTIONS, &req) {
                self.kfocus = KFocus::Content;
                if let Some(out) = out {
                    self.actions_out(out);
                }
                self.touch_static();
                return redraw;
            }
        } else if id >= SETTINGS {
            if let Some((redraw, out)) = self.settings_tab.a11y_action(SETTINGS, &req) {
                self.kfocus = KFocus::Content;
                if let Some(out) = out {
                    self.settings_out(out);
                }
                self.touch_static();
                return redraw;
            }
        }
        false
    }
}
