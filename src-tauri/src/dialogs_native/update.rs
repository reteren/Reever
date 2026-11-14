//! Уведомление об обновлении: маленький экран ЭЛТ в правом нижнем углу над треем.
//! Выезжает снизу, фокус не забирает. «Обновить» ставит скачанную версию сразу, «Позже» и
//! крестик убирают окно (с автообновлением версия поставится сама, когда Reever будет свободен).
//! При автообновлении окно само гаснет через `AUTO_HIDE`, полоска внизу показывает остаток.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};

use super::{dir, locale, t, tp};
use crate::app::AppHandle;
use crate::native::controls::anim::{Tween, EASE_IN, E_OUT};
use crate::native::controls::button::{Button, ButtonKind};
use crate::native::controls::{
    fill_round, inside, lcd, lcd_background, lcd_overlay, pop_layer, push_round_layer,
    stroke_round, ui, Glow, Scene, ACC, ACC_3, ACC_4, ACC_5, ACC_HOT,
};
use crate::native::{
    set_cursor, Align, Cursor, KeyEvent, MouseButton, MouseEvent, MouseKind, Rect, RenderCx,
    TextStyle, WindowHandler, WindowId, WindowSpec,
};

static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);

/// Окно с полями под тень; экран — `CARD`.
const WIN_W: f32 = 372.0;
const WIN_H: f32 = 188.0;
const PAD: f32 = 16.0;
const CARD: Rect = Rect::new(PAD, PAD, WIN_W - 2.0 * PAD, WIN_H - 2.0 * PAD);
const RADIUS: f32 = 12.0;
const AUTO_HIDE: Duration = Duration::from_secs(30);
const SHOW: f32 = 0.38;
const HIDE: f32 = 0.18;

/// Что выбрали: `true` — обновить сейчас.
pub type Answer = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Close,
    Now,
    Later,
}

pub struct UpdateView {
    hwnd: isize,
    version: String,
    /// С автообновлением окно гаснет само и обещает поставить версию позже.
    auto: bool,
    shown_at: Instant,
    appear: Tween,
    answered: bool,
    on_answer: Answer,
    now: Button,
    later: Button,
    close: Rect,
    close_hover: Tween,
    pressed: Option<Target>,
    scene: Scene,
}

impl UpdateView {
    fn new(version: String, auto: bool, on_answer: Answer) -> Self {
        let button = |key: &str, kind| Button {
            font_size: 13.0,
            letter_em: 0.1,
            radius: 6.0,
            ..Button::new(&t(key), kind)
        };
        let mut appear = Tween::new(0.0);
        appear.set(Instant::now(), 1.0, SHOW, E_OUT);
        Self {
            hwnd: 0,
            version,
            auto,
            shown_at: Instant::now(),
            appear,
            answered: false,
            on_answer,
            now: button("update.now", ButtonKind::Default),
            later: button("update.later", ButtonKind::Plain),
            close: Rect::new(CARD.x + CARD.w - 34.0, CARD.y + 8.0, 26.0, 26.0),
            close_hover: Tween::new(0.0),
            pressed: None,
            scene: Scene::default(),
        }
    }

    fn answer(&mut self, now: bool) {
        if self.answered {
            return;
        }
        self.answered = true;
        self.appear.set(Instant::now(), 0.0, HIDE, EASE_IN);
        let on_answer = self.on_answer.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f32(HIDE + 0.03));
            on_answer(now);
        });
    }

    fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        if inside(self.close, x, y) {
            Some(Target::Close)
        } else if self.now.hit(x, y) {
            Some(Target::Now)
        } else if self.later.hit(x, y) {
            Some(Target::Later)
        } else {
            None
        }
    }

    fn left(&self, now: Instant) -> Option<f32> {
        self.auto.then(|| {
            1.0 - now.saturating_duration_since(self.shown_at).as_secs_f32()
                / AUTO_HIDE.as_secs_f32()
        })
    }
}

impl WindowHandler for UpdateView {
    fn attached(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
    }

    fn animating(&self) -> bool {
        let now = Instant::now();
        self.appear.running(now)
            || self.close_hover.running(now)
            || self.now.press.animating(now)
            || self.later.press.animating(now)
            || (!self.answered && self.auto)
    }

    fn prepare(&mut self, gpu: &crate::native::GpuCx<'_>) {
        if self.left(gpu.now).is_some_and(|k| k <= 0.0) {
            self.answer(false);
        }
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        let now = cx.now;
        self.scene.begin_frame(cx);
        let k = self.appear.value(now);
        if k <= 0.001 {
            return;
        }
        let (_, rtl) = locale();
        let dc = cx.dc().clone();
        unsafe {
            dc.SetTransform(&Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: 0.0,
                M32: (1.0 - k) * 18.0,
            })
        };
        self.scene.shadow(
            cx,
            CARD,
            RADIUS,
            crate::native::Color::rgba(0, 0, 0, (150.0 * k) as u8),
            16.0,
        );
        if push_round_layer(cx, Rect::new(0.0, 0.0, WIN_W, WIN_H), 0.0, k) {
            let c = CARD;
            lcd_background(cx, c, RADIUS);
            let x = c.x + 18.0;
            let w = c.w - 36.0;
            // заголовок: значок и «ОБНОВЛЕНИЕ» люминофором
            self.scene.icon(
                "tray",
                Rect::new(x, c.y + 15.0, 15.0, 15.0),
                2.0,
                ACC_HOT,
                false,
                Glow::G2,
            );
            let title = TextStyle {
                direction: dir(rtl),
                ..lcd(15.0, 500, 0.08)
            };
            if let Some(l) = self.scene.cache.get(
                cx.text(),
                &t("update.title").to_uppercase(),
                &title,
                w - 60.0,
            ) {
                self.scene
                    .text(&l, x + 23.0, c.y + 22.5 - l.height / 2.0, ACC_HOT, Glow::G3);
            }
            // крестик — «позже»
            let h = self.close_hover.value(now);
            if h > 0.01 {
                fill_round(
                    cx,
                    self.close,
                    6.0,
                    crate::native::Color {
                        a: ACC_5.a * h * 1.6,
                        ..ACC
                    },
                );
            }
            let ci = Rect::new(self.close.x + 7.0, self.close.y + 7.0, 12.0, 12.0);
            self.scene.icon(
                "x",
                ci,
                2.0,
                crate::native::controls::mix(ACC_3, ACC_HOT, h),
                false,
                Glow::G1,
            );
            // версия и подсказка
            let head = TextStyle {
                wrap: true,
                direction: dir(rtl),
                align: Align::Start,
                ..ui(16.0, 600, 1.25)
            };
            let mut y = c.y + 44.0;
            if let Some(l) = self.scene.cache.get(
                cx.text(),
                &tp("update.ready", &[("version", &self.version)]),
                &head,
                w,
            ) {
                self.scene.text(&l, x, y, ACC_HOT, Glow::G2);
                y += l.height + 4.0;
            }
            let hint_key = if self.auto {
                "update.hint.auto"
            } else {
                "update.hint.manual"
            };
            let hint = TextStyle {
                wrap: true,
                direction: dir(rtl),
                align: Align::Start,
                ..ui(12.5, 400, 1.35)
            };
            if let Some(l) = self.scene.cache.get(cx.text(), &t(hint_key), &hint, w) {
                self.scene.text(&l, x, y, ACC_3, Glow::None);
            }
            // кнопки справа внизу (в арабском — слева)
            let by = c.y + c.h - 18.0 - 30.0;
            let wn = self.now.natural_width(&mut self.scene, cx).max(96.0);
            let wl = self.later.natural_width(&mut self.scene, cx).max(84.0);
            let right = c.x + c.w - 18.0;
            if rtl {
                self.now.rect = Rect::new(x, by, wn, 30.0);
                self.later.rect = Rect::new(x + wn + 10.0, by, wl, 30.0);
            } else {
                self.now.rect = Rect::new(right - wn, by, wn, 30.0);
                self.later.rect = Rect::new(right - wn - 10.0 - wl, by, wl, 30.0);
            }
            self.later.draw(&mut self.scene, cx);
            self.now.draw(&mut self.scene, cx);
            // остаток до автоскрытия — тонкая полоска по нижнему краю экрана
            if let Some(left) = self.left(now) {
                let bar = Rect::new(
                    c.x + RADIUS,
                    c.y + c.h - 8.0,
                    (c.w - 2.0 * RADIUS) * left.clamp(0.0, 1.0),
                    2.0,
                );
                fill_round(cx, bar, 1.0, ACC_3);
            }
            self.scene.flush(cx);
            lcd_overlay(cx, c, RADIUS);
            stroke_round(
                cx,
                Rect::new(c.x + 0.5, c.y + 0.5, c.w - 1.0, c.h - 1.0),
                RADIUS,
                ACC_4,
                1.0,
                false,
            );
            pop_layer(cx);
        }
        unsafe { dc.SetTransform(&Matrix3x2::identity()) };
    }

    fn mouse(&mut self, ev: MouseEvent) -> bool {
        if self.answered {
            return false;
        }
        let now = Instant::now();
        match ev.kind {
            MouseKind::Move => {
                let t = self.target_at(ev.x, ev.y);
                self.close_hover.set(
                    now,
                    if t == Some(Target::Close) { 1.0 } else { 0.0 },
                    0.12,
                    E_OUT,
                );
                self.now.press.set_hover(t == Some(Target::Now), 0.12);
                self.later.press.set_hover(t == Some(Target::Later), 0.12);
                set_cursor(
                    self.hwnd,
                    if t.is_some() {
                        Cursor::Hand
                    } else {
                        Cursor::Arrow
                    },
                );
                true
            }
            MouseKind::Leave => {
                self.close_hover.set(now, 0.0, 0.12, E_OUT);
                self.now.press.set_hover(false, 0.12) | self.later.press.set_hover(false, 0.12)
            }
            MouseKind::Down(MouseButton::Left) => {
                let t = self.target_at(ev.x, ev.y);
                self.pressed = t;
                match t {
                    Some(Target::Now) => self.now.press.pressed = true,
                    Some(Target::Later) => self.later.press.pressed = true,
                    _ => {}
                }
                true
            }
            MouseKind::Up(MouseButton::Left) => {
                let t = self.target_at(ev.x, ev.y);
                self.now.press.pressed = false;
                self.later.press.pressed = false;
                match self.pressed.take().filter(|p| Some(*p) == t) {
                    Some(Target::Now) => self.answer(true),
                    Some(Target::Later | Target::Close) => self.answer(false),
                    None => {}
                }
                true
            }
            _ => false,
        }
    }

    fn key(&mut self, ev: KeyEvent) -> bool {
        let k = ev.key as u16;
        if k == VK_ESCAPE.0 {
            self.answer(false);
        } else if k == VK_RETURN.0 {
            self.answer(true);
        } else {
            return false;
        }
        true
    }

    fn close_requested(&mut self) {
        self.answer(false);
    }

    fn a11y_tree(&mut self, scale: f32) -> Option<crate::native::a11y::accesskit::TreeUpdate> {
        use crate::native::a11y::accesskit::{
            Action, Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate,
        };
        use crate::native::a11y::bounds;
        let b = |r: Rect| bounds(r.x, r.y, r.w, r.h, scale);
        let mut root = Node::new(Role::Alert);
        root.set_label(t("update.title"));
        root.set_description(tp("update.ready", &[("version", &self.version)]));
        root.set_bounds(bounds(0.0, 0.0, WIN_W, WIN_H, scale));
        root.set_children(vec![NodeId(2), NodeId(3), NodeId(4)]);
        let mut nodes = Vec::new();
        for (id, label, rect) in [
            (2u64, self.now.label.clone(), self.now.rect),
            (3, self.later.label.clone(), self.later.rect),
            (4, t("update.later"), self.close),
        ] {
            let mut n = Node::new(Role::Button);
            n.set_label(label);
            n.set_bounds(b(rect));
            n.add_action(Action::Click);
            nodes.push((NodeId(id), n));
        }
        nodes.push((NodeId(1), root));
        Some(TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(2),
        })
    }

    fn a11y_action(&mut self, req: crate::native::a11y::accesskit::ActionRequest) -> bool {
        use crate::native::a11y::accesskit::Action;
        if req.action != Action::Click {
            return false;
        }
        match req.target_node.0 {
            2 => self.answer(true),
            3 | 4 => self.answer(false),
            _ => return false,
        }
        true
    }
}

/// Левый верхний угол окна у правого нижнего угла рабочей области основного монитора.
fn corner_position() -> (i32, i32) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let k = dx as f32 / 96.0;
        let r = info.rcWork;
        let (w, h) = ((WIN_W * k).round() as i32, (WIN_H * k).round() as i32);
        let margin = (8.0 * k).round() as i32;
        (r.right - w - margin, r.bottom - h - margin)
    }
}

/// Показать уведомление о версии `version` (если оно уже на экране — заменить).
pub fn show(app: &AppHandle, version: String, auto: bool, on_answer: Answer) {
    hide(app);
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(ui) = crate::mascot_native::ui(&app) else {
            return;
        };
        let (x, y) = corner_position();
        let made = ui.call(move |ui| {
            let view = UpdateView::new(version, auto, on_answer);
            let spec = WindowSpec {
                title: "Reever".into(),
                x,
                y,
                width: WIN_W,
                height: WIN_H,
                topmost: true,
                no_activate: true,
                visible: true,
                taskbar: false,
            };
            let id = ui.create_window(spec, Box::new(view))?;
            Ok::<_, windows::core::Error>((id, ui.info(id).map(|i| i.hwnd).unwrap_or(0)))
        });
        match made {
            Some(Ok(win)) => {
                if let Ok(mut w) = WIN.lock() {
                    *w = Some(win);
                }
            }
            Some(Err(e)) => {
                crate::events::ui_log(&app, format!("update: native window failed: {e}"))
            }
            None => {}
        }
    });
}

pub fn hide(app: &AppHandle) {
    let Some((id, _)) = WIN.lock().ok().and_then(|mut w| w.take()) else {
        return;
    };
    if let Some(ui) = crate::mascot_native::ui(app) {
        ui.post(move |ui| ui.close_window(id));
        super::release_later(ui.clone());
    }
}

/// Снимок окна вне экрана (RGBA) — для проверок без захвата экрана.
pub fn snapshot(app: &AppHandle) -> Option<(u32, u32, Vec<u8>)> {
    let (id, _) = WIN.lock().ok().and_then(|w| *w)?;
    let ui = crate::mascot_native::ui(app)?;
    ui.call(move |ui| ui.snapshot(id)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    fn view(auto: bool) -> (UpdateView, Arc<AtomicU8>) {
        // 0 — нет ответа, 1 — позже, 2 — обновить
        let got = Arc::new(AtomicU8::new(0));
        let g = got.clone();
        let v = UpdateView::new(
            "0.9.0".into(),
            auto,
            Arc::new(move |now| g.store(if now { 2 } else { 1 }, Ordering::SeqCst)),
        );
        (v, got)
    }

    fn wait(got: &AtomicU8) -> u8 {
        for _ in 0..100 {
            let v = got.load(Ordering::SeqCst);
            if v != 0 {
                return v;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        0
    }

    fn key(k: u16) -> KeyEvent {
        KeyEvent {
            key: k as u32,
            ctrl: false,
            shift: false,
            alt: false,
            repeat: false,
        }
    }

    #[test]
    fn enter_updates_now_and_answers_once() {
        let (mut v, got) = view(false);
        v.key(key(VK_RETURN.0));
        v.key(key(VK_ESCAPE.0));
        assert_eq!(wait(&got), 2);
    }

    #[test]
    fn escape_is_later() {
        let (mut v, got) = view(true);
        v.key(key(VK_ESCAPE.0));
        assert_eq!(wait(&got), 1);
    }

    #[test]
    fn only_auto_mode_counts_down() {
        assert!(view(true).0.left(Instant::now()).is_some());
        assert!(view(false).0.left(Instant::now()).is_none());
    }
}
