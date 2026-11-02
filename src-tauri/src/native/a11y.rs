//! Специальные возможности нативных окон: дерево элементов для UI Automation (экранные дикторы,
//! Narrator, NVDA) через accesskit + accesskit_windows на сыром HWND. Окно отдаёт дерево
//! (`WindowHandler::a11y_tree`), адаптер отвечает на WM_GETOBJECT, а действия диктора
//! (нажать, перевести фокус) приходят обработчику окна (`WindowHandler::a11y_action`).

use std::collections::VecDeque;
use std::sync::Mutex;

pub use accesskit;
use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Сообщение-будильник: для окна пришли действия диктора.
pub(crate) const WM_A11Y_ACTION: u32 = WM_APP + 0x3A2;

/// Действия диктора приходят из потоков UI Automation — копятся здесь до потока окон.
static ACTIONS: Mutex<VecDeque<(isize, ActionRequest)>> = Mutex::new(VecDeque::new());

pub(crate) fn take_actions(hwnd: isize) -> Vec<ActionRequest> {
    let Ok(mut q) = ACTIONS.lock() else { return Vec::new() };
    let mut out = Vec::new();
    q.retain(|(h, r)| {
        if *h == hwnd {
            out.push(r.clone());
            false
        } else {
            true
        }
    });
    out
}

struct Sink {
    hwnd: isize,
}

impl ActionHandler for Sink {
    fn do_action(&mut self, request: ActionRequest) {
        if let Ok(mut q) = ACTIONS.lock() {
            q.push_back((self.hwnd, request));
        }
        unsafe {
            let _ = PostMessageW(HWND(self.hwnd as *mut _), WM_A11Y_ACTION, WPARAM(0), LPARAM(0));
        }
    }
}

struct Initial(Option<TreeUpdate>);

impl ActivationHandler for Initial {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0.take()
    }
}

/// Адаптер окна и последнее полное дерево (его отдаём при первом обращении диктора).
pub(crate) struct A11y {
    adapter: accesskit_windows::Adapter,
    tree: Option<TreeUpdate>,
}

impl A11y {
    pub fn new(hwnd: isize, tree: TreeUpdate) -> Self {
        let adapter = accesskit_windows::Adapter::new(accesskit_windows::HWND(hwnd as *mut _), false, Sink { hwnd });
        Self { adapter, tree: Some(tree) }
    }

    /// Ответ на WM_GETOBJECT (None — не наш запрос, отдать DefWindowProc).
    pub fn get_object(&mut self, wp: usize, lp: isize) -> Option<isize> {
        let mut initial = Initial(self.tree.clone());
        let r = self.adapter.handle_wm_getobject(accesskit_windows::WPARAM(wp), accesskit_windows::LPARAM(lp), &mut initial)?;
        let r: accesskit_windows::LRESULT = r.into();
        Some(r.0)
    }

    /// Новое дерево; события для диктора поднимаются после выхода из заимствования.
    pub fn update(&mut self, tree: TreeUpdate) -> Option<accesskit_windows::QueuedEvents> {
        self.tree = Some(tree.clone());
        self.adapter.update_if_active(|| tree)
    }

    pub fn focus(&mut self, focused: bool) -> Option<accesskit_windows::QueuedEvents> {
        self.adapter.update_window_focus_state(focused)
    }
}

/// Узел-прямоугольник в физических пикселях окна (DIP × масштаб).
pub fn bounds(x: f32, y: f32, w: f32, h: f32, scale: f32) -> accesskit::Rect {
    let k = scale as f64;
    accesskit::Rect { x0: x as f64 * k, y0: y as f64 * k, x1: (x + w) as f64 * k, y1: (y + h) as f64 * k }
}
