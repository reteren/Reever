//! Нативная плашка подтверждения опасной команды — перенос frontend/confirm.html: вопрос
//! крупно (мигает три раза), подсказка про голос, кнопки «Да» (штрихами — опасно) и «Нет»
//! (в фокусе по умолчанию: случайный Enter ничего не выключит), полоска и «отмена через N с».
//! Esc и купол закрытия — «Нет»; стрелки влево/вправо переводят фокус между кнопками.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT, DWRITE_TEXT_RANGE};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB};

use super::{close_button, dir, draw_frame, draw_title, load_skin, locale, t, tp, Skin, SCREEN, WIN_H, WIN_W};
use crate::native::controls::button::{Button, ButtonKind, Lunka};
use crate::native::controls::tooltip::Tooltip;
use crate::native::controls::{fill_round, lcd, lcd_background, lcd_overlay, stroke_round, ui, Glow, Scene, ACC, ACC_2, ACC_3, ACC_4, ACC_5, ACC_HOT};
use crate::native::{
    self, set_cursor, Align, Cursor, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx, TextStyle,
    WindowHandler, WindowId, WindowSpec,
};

static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);

/// Вопрос (как payload reever://confirm).
#[derive(Clone, Debug)]
pub struct Question {
    pub session: u64,
    pub kind: String,
    pub app: String,
    pub seconds: u32,
}

/// Ответ уходит в confirm.rs (вызывается не из потока окон).
pub trait Host: Send + Sync {
    fn answer(&self, session: u64, yes: bool);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Close,
    Yes,
    No,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Close,
    Yes,
    No,
}

pub struct ConfirmView {
    host: std::sync::Arc<dyn Host>,
    hwnd: isize,
    skin: Skin,
    question: Option<Question>,
    shown_at: Instant,
    yes: Button,
    no: Button,
    close: Lunka,
    focus: Focus,
    pressed: Option<Target>,
    tooltip: Tooltip,
    scene: Scene,
    swallow_char: bool,
}

impl ConfirmView {
    fn new(host: std::sync::Arc<dyn Host>, skin: Skin) -> Self {
        let big = |label: &str, kind| Button {
            font_size: 34.0,
            letter_em: 0.12,
            radius: 8.0,
            focus_ring: (2.0, 4.0, ACC_HOT),
            ..Button::new(label, kind)
        };
        Self {
            host,
            hwnd: 0,
            close: close_button(&skin, "x", &t("confirm.noTitle")),
            skin,
            question: None,
            shown_at: Instant::now(),
            yes: big(&t("confirm.yes"), ButtonKind::Danger),
            no: big(&t("confirm.no"), ButtonKind::Default),
            focus: Focus::No,
            pressed: None,
            tooltip: Tooltip::default(),
            scene: Scene::default(),
            swallow_char: false,
        }
    }

    /// render(): новый вопрос, фокус — на «Нет», отсчёт заново.
    fn apply(&mut self, q: Question) {
        self.yes.label = t("confirm.yes");
        self.no.label = t("confirm.no");
        self.close.tip = t("confirm.noTitle");
        self.question = Some(q);
        self.shown_at = Instant::now();
        // фокус из скрипта при показе — с видимой рамкой (:focus-visible)
        self.set_focus(Focus::No, true);
    }

    fn set_focus(&mut self, f: Focus, visible: bool) {
        self.focus = f;
        self.yes.press.focused = f == Focus::Yes;
        self.no.press.focused = f == Focus::No;
        self.close.press.focused = f == Focus::Close;
        for p in [&mut self.yes.press, &mut self.no.press, &mut self.close.press] {
            p.focus_visible = visible;
        }
    }

    fn answer(&mut self, yes: bool) {
        let Some(q) = self.question.take() else { return };
        let host = self.host.clone();
        std::thread::spawn(move || host.answer(q.session, yes));
    }

    fn total(&self) -> Duration {
        Duration::from_secs(self.question.as_ref().map_or(8, |q| q.seconds.max(1)) as u64)
    }

    fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        if self.close.hit(x, y) {
            Some(Target::Close)
        } else if self.yes.hit(x, y) {
            Some(Target::Yes)
        } else if self.no.hit(x, y) {
            Some(Target::No)
        } else {
            None
        }
    }

    fn activate(&mut self, t: Target) {
        match t {
            Target::Yes => self.answer(true),
            Target::No | Target::Close => self.answer(false),
        }
    }

    fn question_text(&self) -> String {
        let Some(q) = &self.question else { return String::new() };
        let key = if ["shutdown", "restart", "close", "kill"].contains(&q.kind.as_str()) { format!("confirm.{}", q.kind) } else { "confirm.generic".into() };
        tp(&key, &[("app", &q.app), ("action", &q.kind)])
    }

    fn draw_screen(&mut self, cx: &mut RenderCx<'_>) {
        let s = SCREEN;
        let now = cx.now;
        let (_, rtl) = locale();
        lcd_background(cx, s, 13.0);
        // колонка по центру: padding 24px 28px, gap 22px
        let inner_w = s.w - 56.0;
        let q_style = TextStyle {
            wrap: true,
            break_words: true,
            align: Align::Center,
            direction: dir(rtl),
            line_height: Some(1.05),
            ..lcd(38.0, 500, 0.05)
        };
        let qtext = self.question_text().to_uppercase();
        let ql = self.scene.cache.get(cx.text(), &qtext, &q_style, inner_w);
        // «Голосом: скажите …» — <strong> жирнее и ярче
        let (voice_plain, strong) = strip_strong(&t("confirm.voice"));
        let v_style = TextStyle { wrap: true, align: Align::Center, direction: dir(rtl), ..ui(14.0, 400, 1.45) };
        let vl = cx.text().layout(&voice_plain, &v_style, inner_w).ok().map(std::rc::Rc::new);
        if let (Some(vl), Some((a, b))) = (&vl, strong) {
            unsafe {
                // <strong>: у переменного шрифта жирность — ось wght (SetFontWeight её не трогает)
                let range = DWRITE_TEXT_RANGE { startPosition: a as u32, length: (b - a) as u32 };
                if let Ok(l4) = windows::core::Interface::cast::<windows::Win32::Graphics::DirectWrite::IDWriteTextLayout4>(&vl.layout) {
                    let axes = [
                        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_AXIS_VALUE { axisTag: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_AXIS_TAG(u32::from_le_bytes(*b"wght")), value: 600.0 },
                        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_AXIS_VALUE { axisTag: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_AXIS_TAG(u32::from_le_bytes(*b"SHRP")), value: 60.0 },
                    ];
                    let _ = l4.SetFontAxisValues(&axes, range);
                }
                let _ = vl.layout.SetFontWeight(DWRITE_FONT_WEIGHT(600), range);
            }
        }
        let left = self.total().saturating_sub(now.saturating_duration_since(self.shown_at));
        let secs = (left.as_millis() as f32 / 1000.0).ceil() as u64;
        let left_text = tp("confirm.left", &[("seconds", &secs.to_string())]).to_uppercase();
        let l_style = TextStyle { direction: dir(rtl), ..lcd(16.0, 500, 0.06) };
        let ll = self.scene.cache.get(cx.text(), &left_text, &l_style, 10_000.0);
        let qh = ql.as_ref().map_or(0.0, |l| l.height);
        let vh = vl.as_ref().map_or(0.0, |l| l.height);
        let lh = ll.as_ref().map_or(16.0, |l| l.height);
        let timer_h = 6.0 + 7.0 + lh;
        let total_h = qh + 22.0 + vh + 22.0 + 70.0 + 22.0 + timer_h;
        let mut y = s.y + (s.h - total_h) / 2.0;
        // вопрос: мигает 1,2 с × 3 (steps: 1 → .25 → 1)
        if let Some(l) = &ql {
            let t_ = now.saturating_duration_since(self.shown_at).as_secs_f32();
            let phase = t_ % 1.2;
            let op = if t_ < 3.6 && (0.3..0.9).contains(&phase) { 0.25 } else { 1.0 };
            self.scene.text_alpha(l, s.x + 28.0 + (inner_w - l.width.min(inner_w)) * 0.0, y, ACC_HOT, Glow::G3, op);
        }
        y += qh + 22.0;
        if let Some(l) = &vl {
            if let Some((a, b)) = strong {
                unsafe {
                    // своя кисть: общая кисть кадра перекрашивается следующими рисованиями
                    if let Ok(brush) = cx.dc().CreateSolidColorBrush(&ACC_2.d2d(), None) {
                        let _ = l.layout.SetDrawingEffect(&brush, DWRITE_TEXT_RANGE { startPosition: a as u32, length: (b - a) as u32 });
                    }
                }
            }
            self.scene.text(l, s.x + 28.0, y, ACC_3, Glow::G1);
        }
        y += vh + 22.0;
        // кнопки 170×70, gap 26px, по центру
        let bx = s.x + (s.w - (170.0 * 2.0 + 26.0)) / 2.0;
        let (first, second) = if rtl { (&mut self.no, &mut self.yes) } else { (&mut self.yes, &mut self.no) };
        first.rect = Rect::new(bx, y, 170.0, 70.0);
        second.rect = Rect::new(bx + 170.0 + 26.0, y, 170.0, 70.0);
        self.yes.draw(&mut self.scene, cx);
        self.no.draw(&mut self.scene, cx);
        y += 70.0 + 22.0;
        // полоска: 6px, рамка acc-4; заливка уменьшается слева направо к началу (origin: left)
        let bar = Rect::new(s.x + 28.0, y, inner_w, 6.0);
        fill_round(cx, bar, 3.0, ACC_5);
        stroke_round(cx, bar, 3.0, ACC_4, 1.0, false);
        let k = left.as_secs_f32() / self.total().as_secs_f32();
        if k > 0.0 {
            let fill = Rect::new(bar.x + 1.0, bar.y + 1.0, (bar.w - 2.0) * k, bar.h - 2.0);
            self.scene.shadow(cx, fill, 0.0, ACC, 10.0);
            if crate::native::controls::push_round_layer(cx, Rect::new(bar.x + 1.0, bar.y + 1.0, bar.w - 2.0, bar.h - 2.0), 2.0, 1.0) {
                cx.fill_rect(fill, ACC);
                crate::native::controls::pop_layer(cx);
            }
        }
        if let Some(l) = &ll {
            self.scene.text(l, s.x + (s.w - l.width) / 2.0, y + 6.0 + 7.0, ACC_3, Glow::G2);
        }
        self.scene.flush(cx);
        lcd_overlay(cx, s, 13.0);
    }
}

/// «<strong>Голосом:</strong> скажите …» → текст без тегов и границы жирного куска (в UTF-16).
fn strip_strong(html: &str) -> (String, Option<(usize, usize)>) {
    let (Some(a), Some(b)) = (html.find("<strong>"), html.find("</strong>")) else {
        return (html.to_string(), None);
    };
    let before = &html[..a];
    let inner = &html[a + 8..b];
    let after = &html[b + 9..];
    let start = before.encode_utf16().count();
    let end = start + inner.encode_utf16().count();
    (format!("{before}{inner}{after}"), Some((start, end)))
}

impl WindowHandler for ConfirmView {
    fn attached(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
    }

    fn animating(&self) -> bool {
        let now = Instant::now();
        // мигание вопроса — первые 3,6 с; полоска — пока идёт отсчёт
        self.question.is_some() && now.saturating_duration_since(self.shown_at) < self.total() + Duration::from_millis(100)
            || self.yes.press.animating(now)
            || self.no.press.animating(now)
            || self.close.press.animating(now)
            || self.tooltip.animating(now)
    }

    fn wake_at(&self) -> Option<Instant> {
        self.tooltip.wake_at()
    }

    fn prepare(&mut self, gpu: &crate::native::GpuCx<'_>) {
        self.tooltip.tick(gpu.now);
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        self.scene.begin_frame(cx);
        let (_, rtl) = locale();
        draw_title(&mut self.scene, cx, "warn", false, &t("confirm.title"), rtl);
        self.draw_screen(cx);
        draw_frame(cx, &self.skin);
        self.close.draw(&mut self.scene, cx);
        self.tooltip.draw(&mut self.scene, cx, (WIN_W, WIN_H));
    }

    fn mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Move => {
                let t = self.target_at(ev.x, ev.y);
                let mut r = self.close.set_hover(t == Some(Target::Close));
                r |= self.yes.press.set_hover(t == Some(Target::Yes), 0.12);
                r |= self.no.press.set_hover(t == Some(Target::No), 0.12);
                let tip = if t == Some(Target::Close) { Some((1u64, self.close.rect(), self.close.tip.as_str())) } else { None };
                r |= self.tooltip.hover(tip);
                set_cursor(self.hwnd, if t.is_some() { Cursor::Hand } else { Cursor::Arrow });
                r
            }
            MouseKind::Leave => {
                self.close.set_hover(false) | self.yes.press.set_hover(false, 0.12) | self.no.press.set_hover(false, 0.12) | self.tooltip.hide()
            }
            MouseKind::Down(MouseButton::Left) => {
                self.tooltip.hide();
                let t = self.target_at(ev.x, ev.y);
                self.pressed = t;
                match t {
                    Some(Target::Yes) => {
                        self.yes.press.pressed = true;
                        self.set_focus(Focus::Yes, false);
                    }
                    Some(Target::No) => {
                        self.no.press.pressed = true;
                        self.set_focus(Focus::No, false);
                    }
                    Some(Target::Close) => self.close.press.pressed = true,
                    None => native::begin_drag(self.hwnd),
                }
                true
            }
            MouseKind::Up(MouseButton::Left) => {
                let t = self.target_at(ev.x, ev.y);
                let p = self.pressed.take();
                self.yes.press.pressed = false;
                self.no.press.pressed = false;
                self.close.press.pressed = false;
                if let Some(p) = p.filter(|p| Some(*p) == t) {
                    self.activate(p);
                }
                true
            }
            _ => false,
        }
    }

    fn key(&mut self, ev: KeyEvent) -> bool {
        self.tooltip.hide();
        if self.question.is_none() || ev.alt {
            return false;
        }
        let k = ev.key as u16;
        if k == VK_ESCAPE.0 {
            self.answer(false);
        } else if k == VK_LEFT.0 || k == VK_RIGHT.0 {
            self.set_focus(if self.focus == Focus::Yes { Focus::No } else { Focus::Yes }, true);
        } else if k == VK_TAB.0 {
            let order = [Focus::Close, Focus::Yes, Focus::No];
            let i = order.iter().position(|f| *f == self.focus).unwrap_or(2);
            let next = if ev.shift { order[(i + 2) % 3] } else { order[(i + 1) % 3] };
            self.set_focus(next, true);
        } else if k == VK_RETURN.0 || k == VK_SPACE.0 {
            // Enter и пробел нажимают кнопку в фокусе
            self.swallow_char = true;
            match self.focus {
                Focus::Yes => self.answer(true),
                Focus::No | Focus::Close => self.answer(false),
            }
        } else {
            return false;
        }
        true
    }

    fn char_input(&mut self, _c: char) -> bool {
        self.swallow_char = false;
        false
    }

    fn close_requested(&mut self) {
        // Alt+F4 по плашке — то же, что «Нет»
        self.answer(false);
    }

    fn a11y_tree(&mut self, scale: f32) -> Option<crate::native::a11y::accesskit::TreeUpdate> {
        use crate::native::a11y::accesskit::{Action, Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};
        use crate::native::a11y::bounds;
        let b = |r: Rect| bounds(r.x, r.y, r.w, r.h, scale);
        let mut nodes = Vec::new();
        let mut root = Node::new(Role::AlertDialog);
        root.set_label(t("confirm.title"));
        root.set_description(self.question_text());
        root.set_bounds(bounds(0.0, 0.0, WIN_W, WIN_H, scale));
        root.set_children(vec![NodeId(2), NodeId(5), NodeId(6), NodeId(3), NodeId(4), NodeId(7)]);
        let mut close = Node::new(Role::Button);
        close.set_label(t("confirm.no"));
        close.set_description(self.close.tip.clone());
        close.set_bounds(b(self.close.rect()));
        close.add_action(Action::Click);
        close.add_action(Action::Focus);
        nodes.push((NodeId(2), close));
        for (id, btn) in [(3u64, &self.yes), (4, &self.no)] {
            let mut n = Node::new(Role::Button);
            n.set_label(btn.label.clone());
            n.set_bounds(b(btn.rect));
            n.add_action(Action::Click);
            n.add_action(Action::Focus);
            nodes.push((NodeId(id), n));
        }
        let mut q = Node::new(Role::Label);
        q.set_value(self.question_text());
        nodes.push((NodeId(5), q));
        let mut voice = Node::new(Role::Label);
        voice.set_value(strip_strong(&t("confirm.voice")).0);
        nodes.push((NodeId(6), voice));
        let left = self.total().saturating_sub(Instant::now().saturating_duration_since(self.shown_at));
        let mut bar = Node::new(Role::ProgressIndicator);
        bar.set_label(tp("confirm.left", &[("seconds", &((left.as_millis() as f32 / 1000.0).ceil() as u64).to_string())]));
        bar.set_numeric_value(left.as_secs_f64());
        bar.set_min_numeric_value(0.0);
        bar.set_max_numeric_value(self.total().as_secs_f64());
        nodes.push((NodeId(7), bar));
        nodes.push((NodeId(1), root));
        let focus = match self.focus {
            Focus::Close => NodeId(2),
            Focus::Yes => NodeId(3),
            Focus::No => NodeId(4),
        };
        Some(TreeUpdate { nodes, tree: Some(TreeInfo::new(NodeId(1))), tree_id: TreeId::ROOT, focus })
    }

    fn a11y_action(&mut self, req: crate::native::a11y::accesskit::ActionRequest) -> bool {
        use crate::native::a11y::accesskit::Action;
        let target = match req.target_node.0 {
            2 => (Focus::Close, Target::Close),
            3 => (Focus::Yes, Target::Yes),
            4 => (Focus::No, Target::No),
            _ => return false,
        };
        match req.action {
            Action::Focus => self.set_focus(target.0, true),
            Action::Click => self.activate(target.1),
            _ => return false,
        }
        true
    }
}

/// Показать вопрос (создать окно, если его нет) и отдать ему фокус.
pub fn show(app: &AppHandle, host: std::sync::Arc<dyn Host>, q: Question) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(ui) = crate::mascot_native::ui(&app) else { return };
        let existing = WIN.lock().ok().and_then(|w| *w);
        if let Some((id, hwnd)) = existing {
            ui.post(move |ui| {
                if let Some(v) = ui.handler_mut::<ConfirmView>(id) {
                    v.apply(q);
                }
                ui.invalidate(id);
            });
            if !crate::dialog_demo::active() {
                crate::winutil::bring_to_front_raw(hwnd);
            }
            return;
        }
        let (x, y) = super::centered_position();
        let app2 = app.clone();
        let made = ui.call(move |ui| {
            let skin = load_skin(&app2, ui.images());
            let mut view = ConfirmView::new(host, skin);
            view.apply(q);
            let spec = WindowSpec { title: "Reever".into(), x, y, width: WIN_W, height: WIN_H, topmost: true, no_activate: crate::dialog_demo::active(), visible: true };
            let id = ui.create_window(spec, Box::new(view))?;
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
                    crate::dialog_demo::shown("confirm");
                }
            }
            Some(Err(e)) => crate::events::ui_log(&app, format!("confirm: native window failed: {e}")),
            None => {}
        }
    });
}

pub fn hide(app: &AppHandle) {
    let Some((id, _)) = WIN.lock().ok().and_then(|mut w| w.take()) else { return };
    if let Some(ui) = crate::mascot_native::ui(app) {
        ui.post(move |ui| {
            ui.close_window(id);
        });
        // устройство — не сразу: следующее окно (вопрос после выбора, новый поиск) в течение
        // минуты покажется без пересоздания D3D11 (~200 мс)
        super::release_later(ui.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::ImageId;
    use std::sync::Arc;

    #[derive(Default)]
    struct Mock {
        answers: Mutex<Vec<(u64, bool)>>,
    }

    impl Host for Mock {
        fn answer(&self, session: u64, yes: bool) {
            self.answers.lock().unwrap().push((session, yes));
        }
    }

    fn answers(m: &Mock) -> Vec<(u64, bool)> {
        for _ in 0..100 {
            if !m.answers.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(20));
        m.answers.lock().unwrap().clone()
    }

    fn view() -> (ConfirmView, Arc<Mock>) {
        let mock = Arc::new(Mock::default());
        let skin = Skin { frame: ImageId(0), close_off: ImageId(1), close_on: ImageId(2) };
        let mut v = ConfirmView::new(mock.clone(), skin);
        v.apply(Question { session: 7, kind: "shutdown".into(), app: String::new(), seconds: 8 });
        (v, mock)
    }

    fn key(k: u16, shift: bool) -> KeyEvent {
        KeyEvent { key: k as u32, ctrl: false, shift, alt: false, repeat: false }
    }

    #[test]
    fn no_is_focused_and_enter_answers_no() {
        let (mut v, mock) = view();
        assert_eq!(v.focus, Focus::No);
        v.key(key(VK_RETURN.0, false));
        assert_eq!(answers(&mock), vec![(7, false)]);
    }

    #[test]
    fn arrows_switch_to_yes_and_space_answers_yes() {
        let (mut v, mock) = view();
        v.key(key(VK_LEFT.0, false));
        assert_eq!(v.focus, Focus::Yes);
        v.key(key(VK_RIGHT.0, false));
        assert_eq!(v.focus, Focus::No);
        v.key(key(VK_RIGHT.0, false));
        v.key(key(VK_SPACE.0, false));
        assert_eq!(answers(&mock), vec![(7, true)]);
    }

    #[test]
    fn escape_and_alt_f4_answer_no_once() {
        let (mut v, mock) = view();
        v.key(key(VK_ESCAPE.0, false));
        v.close_requested();
        assert_eq!(answers(&mock), vec![(7, false)]);
    }

    #[test]
    fn tab_cycles_close_yes_no() {
        let (mut v, _) = view();
        v.key(key(VK_TAB.0, false));
        assert_eq!(v.focus, Focus::Close);
        v.key(key(VK_TAB.0, false));
        assert_eq!(v.focus, Focus::Yes);
        v.key(key(VK_TAB.0, true));
        assert_eq!(v.focus, Focus::Close);
    }

    #[test]
    fn strong_is_stripped_with_utf16_range() {
        let (s, r) = strip_strong("<strong>Голосом:</strong> скажите «да».");
        assert_eq!(s, "Голосом: скажите «да».");
        assert_eq!(r, Some((0, 8)));
        assert_eq!(strip_strong("plain").1, None);
    }
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
