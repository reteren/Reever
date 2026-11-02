//! Однострочное поле ввода скина (input в skin.css): каретка, выделение мышью и
//! Shift+стрелками, слова по Ctrl, Home/End, Backspace/Delete, буфер обмена (Ctrl+A/C/V/X),
//! отмена и повтор (Ctrl+Z / Ctrl+Y, Ctrl+Shift+Z), ввод иероглифов через IME — окно набора
//! ставится у каретки (`ime_caret`). Длинный текст прокручивается так, чтобы каретку было видно.

use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_A, VK_BACK, VK_C, VK_DELETE, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_RIGHT, VK_V, VK_X, VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime;

use super::{data, fill_round, push_round_layer, pop_layer, stroke_round, with_alpha, Glow, Scene, ACC, ACC_2, ACC_4, ACC_HOT, GLASS};
use crate::native::{Color, KeyEvent, Rect, RenderCx, TextLayout, TextStyle};

const CF_UNICODETEXT: u32 = 13;

#[derive(Clone, Debug, PartialEq)]
struct Snap {
    text: Vec<char>,
    caret: usize,
    anchor: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Edit {
    Type,
    Delete,
    Other,
}

/// Что изменилось после события поля.
#[derive(Debug, Default, Clone, Copy)]
pub struct InputResult {
    /// клавиша или символ обработаны полем (дальше их не передавать)
    pub handled: bool,
    /// текст изменился (окну — запустить поиск и т.п.)
    pub changed: bool,
    pub redraw: bool,
}

#[derive(Clone)]
pub struct TextInput {
    pub rect: Rect,
    pub placeholder: String,
    pub style: TextStyle,
    /// отступы текста внутри поля (слева у поиска — место под значок)
    pub pad_left: f32,
    pub pad_right: f32,
    pub focused: bool,
    pub hwnd: isize,
    text: Vec<char>,
    caret: usize,
    anchor: usize,
    scroll_x: f32,
    blink_from: Instant,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    last_edit: Option<(Edit, Instant)>,
    dragging: bool,
    layout: Option<Rc<TextLayout>>,
}

impl Default for TextInput {
    fn default() -> Self {
        Self {
            rect: Rect::new(0.0, 0.0, 0.0, 28.0),
            placeholder: String::new(),
            style: data(13.0, 1.0),
            pad_left: 9.0,
            pad_right: 9.0,
            focused: false,
            hwnd: 0,
            text: Vec::new(),
            caret: 0,
            anchor: 0,
            scroll_x: 0.0,
            blink_from: Instant::now(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            dragging: false,
            layout: None,
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextInput {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// Новый текст без истории правок (как присвоение value из кода).
    pub fn set_text(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.undo.clear();
        self.redo.clear();
        self.last_edit = None;
        self.scroll_x = 0.0;
        self.blink_from = Instant::now();
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
        self.blink_from = Instant::now();
    }

    pub fn set_focus(&mut self, on: bool) {
        self.focused = on;
        self.blink_from = Instant::now();
        if !on {
            self.dragging = false;
        }
    }

    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    fn snap(&self) -> Snap {
        Snap { text: self.text.clone(), caret: self.caret, anchor: self.anchor }
    }

    /// Запомнить состояние перед правкой; набор подряд склеивается в одну отмену.
    fn remember(&mut self, kind: Edit) {
        let now = Instant::now();
        let merge = matches!(self.last_edit, Some((k, t)) if k == kind && kind != Edit::Other && now.duration_since(t) < Duration::from_secs(1));
        if !merge {
            self.undo.push(self.snap());
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = Some((kind, now));
    }

    fn restore(&mut self, s: Snap) {
        self.text = s.text;
        self.caret = s.caret.min(self.text.len());
        self.anchor = s.anchor.min(self.text.len());
        self.last_edit = None;
    }

    fn replace_selection(&mut self, insert: &[char]) {
        let (a, b) = self.selection();
        self.text.splice(a..b, insert.iter().copied());
        self.caret = a + insert.len();
        self.anchor = self.caret;
    }

    fn word_left(&self, from: usize) -> usize {
        let mut i = from;
        while i > 0 && !is_word(self.text[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(self.text[i - 1]) {
            i -= 1;
        }
        i
    }

    fn word_right(&self, from: usize) -> usize {
        let n = self.text.len();
        let mut i = from;
        while i < n && is_word(self.text[i]) {
            i += 1;
        }
        while i < n && !is_word(self.text[i]) {
            i += 1;
        }
        i
    }

    fn move_to(&mut self, to: usize, extend: bool) {
        // каретку передвинули — следующий набор уже новая отмена
        self.last_edit = None;
        self.caret = to.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
        self.blink_from = Instant::now();
    }

    /// Клавиша (поле в фокусе).
    pub fn key(&mut self, ev: KeyEvent) -> InputResult {
        let mut r = InputResult { handled: true, changed: false, redraw: true };
        let (a, b) = self.selection();
        let has_sel = a != b;
        let k = ev.key as u16;
        match k {
            x if x == VK_LEFT.0 => {
                let to = if ev.ctrl {
                    self.word_left(self.caret)
                } else if has_sel && !ev.shift {
                    a
                } else {
                    self.caret.saturating_sub(1)
                };
                self.move_to(to, ev.shift);
            }
            x if x == VK_RIGHT.0 => {
                let to = if ev.ctrl {
                    self.word_right(self.caret)
                } else if has_sel && !ev.shift {
                    b
                } else {
                    (self.caret + 1).min(self.text.len())
                };
                self.move_to(to, ev.shift);
            }
            x if x == VK_HOME.0 => self.move_to(0, ev.shift),
            x if x == VK_END.0 => self.move_to(self.text.len(), ev.shift),
            x if x == VK_BACK.0 => {
                if has_sel || self.caret > 0 {
                    self.remember(Edit::Delete);
                    if !has_sel {
                        self.anchor = if ev.ctrl { self.word_left(self.caret) } else { self.caret - 1 };
                    }
                    self.replace_selection(&[]);
                    r.changed = true;
                }
            }
            x if x == VK_DELETE.0 && !ev.shift => {
                if has_sel || self.caret < self.text.len() {
                    self.remember(Edit::Delete);
                    if !has_sel {
                        self.anchor = if ev.ctrl { self.word_right(self.caret) } else { self.caret + 1 };
                    }
                    self.replace_selection(&[]);
                    r.changed = true;
                }
            }
            x if ev.ctrl && x == VK_A.0 => self.select_all(),
            x if (ev.ctrl && x == VK_C.0) || (ev.ctrl && x == VK_INSERT.0) => {
                if has_sel {
                    clipboard_set(self.hwnd, &self.text[a..b].iter().collect::<String>());
                }
            }
            x if (ev.ctrl && x == VK_X.0) || (ev.shift && x == VK_DELETE.0) => {
                if has_sel {
                    clipboard_set(self.hwnd, &self.text[a..b].iter().collect::<String>());
                    self.remember(Edit::Other);
                    self.replace_selection(&[]);
                    r.changed = true;
                }
            }
            x if (ev.ctrl && x == VK_V.0) || (ev.shift && x == VK_INSERT.0) => {
                if let Some(s) = clipboard_get(self.hwnd) {
                    // одна строка: переводы строк — пробелами
                    let chars: Vec<char> = s.chars().map(|c| if c == '\r' || c == '\n' || c == '\t' { ' ' } else { c }).collect();
                    self.remember(Edit::Other);
                    self.replace_selection(&chars);
                    r.changed = true;
                }
            }
            x if ev.ctrl && ((x == VK_Z.0 && !ev.shift) || false) => {
                if let Some(s) = self.undo.pop() {
                    self.redo.push(self.snap());
                    self.restore(s);
                    r.changed = true;
                }
            }
            x if ev.ctrl && (x == VK_Y.0 || (x == VK_Z.0 && ev.shift)) => {
                if let Some(s) = self.redo.pop() {
                    self.undo.push(self.snap());
                    self.restore(s);
                    r.changed = true;
                }
            }
            _ => r = InputResult::default(),
        }
        if r.handled {
            self.blink_from = Instant::now();
        }
        r
    }

    /// Введённый символ (WM_CHAR, в том числе итог IME).
    pub fn char_input(&mut self, c: char) -> InputResult {
        if (c as u32) < 0x20 || c as u32 == 0x7F {
            return InputResult::default();
        }
        // замена выделения — отдельная отмена, как в браузере
        let (a, b) = self.selection();
        self.remember(if a != b { Edit::Other } else { Edit::Type });
        self.replace_selection(&[c]);
        self.blink_from = Instant::now();
        InputResult { handled: true, changed: true, redraw: true }
    }

    fn text_left(&self) -> f32 {
        self.rect.x + self.pad_left - self.scroll_x
    }

    /// Символ под точкой окна (по последней разметке).
    fn index_at(&self, x: f32) -> usize {
        let Some(l) = &self.layout else { return 0 };
        let (mut trailing, mut inside) = (false.into(), false.into());
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            if l.layout.HitTestPoint(x - self.text_left(), 1.0, &mut trailing, &mut inside, &mut m).is_err() {
                return 0;
            }
        }
        let utf16 = m.textPosition as usize + if trailing.as_bool() { m.length as usize } else { 0 };
        utf16_to_char(&self.text, utf16)
    }

    pub fn mouse_down(&mut self, x: f32, shift: bool) {
        let i = self.index_at(x);
        self.move_to(i, shift);
        self.dragging = true;
    }

    pub fn mouse_move(&mut self, x: f32) -> bool {
        if !self.dragging {
            return false;
        }
        let i = self.index_at(x);
        if i != self.caret {
            self.caret = i;
            return true;
        }
        false
    }

    pub fn mouse_up(&mut self) {
        self.dragging = false;
    }

    /// Двойной щелчок — слово под курсором.
    pub fn double_click(&mut self, x: f32) {
        let i = self.index_at(x).min(self.text.len());
        let mut a = i;
        while a > 0 && is_word(self.text[a - 1]) {
            a -= 1;
        }
        let mut b = i;
        while b < self.text.len() && is_word(self.text[b]) {
            b += 1;
        }
        self.anchor = a;
        self.caret = b;
    }

    fn blink_ms() -> u64 {
        let t = unsafe { GetCaretBlinkTime() };
        if t == 0 || t == u32::MAX { 530 } else { t as u64 }
    }

    /// Каретка видна сейчас (мигает с системным периодом).
    fn caret_on(&self, now: Instant) -> bool {
        let p = Self::blink_ms() as u128;
        (now.saturating_duration_since(self.blink_from).as_millis() / p) % 2 == 0
    }

    /// Когда каретка мигнёт в следующий раз (для перерисовки без постоянных кадров).
    pub fn next_blink(&self, now: Instant) -> Option<Instant> {
        if !self.focused {
            return None;
        }
        let p = Self::blink_ms() as u128;
        let n = now.saturating_duration_since(self.blink_from).as_millis() / p + 1;
        Some(self.blink_from + Duration::from_millis((n * p) as u64))
    }

    /// Где каретка (DIP окна: x, верх, высота) — для окна набора IME.
    pub fn ime_caret(&self) -> Option<(f32, f32, f32)> {
        let l = self.layout.as_ref()?;
        let (x, _) = caret_pos(l, char_to_utf16(&self.text, self.caret));
        let y = self.rect.y + (self.rect.h - l.height) / 2.0;
        Some((self.text_left() + x, y, l.height))
    }

    pub fn draw(&mut self, scene: &mut Scene, cx: &mut RenderCx<'_>) {
        let r = self.rect;
        if self.focused {
            // box-shadow: 0 0 0 1px acc-4, 0 0 12px rgba(acc, .15)
            scene.shadow(cx, r, 5.0, Color::rgba(39, 230, 170, 38), 12.0);
            stroke_round(cx, Rect::new(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0), 6.0, ACC_4, 1.0, false);
        }
        fill_round(cx, r, 5.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 });
        stroke_round(cx, r, 5.0, if self.focused { ACC_2 } else { ACC_4 }, 1.0, false);
        let s: String = self.text.iter().collect();
        let inner = Rect::new(r.x + self.pad_left, r.y + 1.0, r.w - self.pad_left - self.pad_right, r.h - 2.0);
        let wide = TextStyle { wrap: false, ..self.style.clone() };
        let shown = if s.is_empty() { self.placeholder.clone() } else { s.clone() };
        let Some(layout) = scene.cache.get(cx.text(), if shown.is_empty() { " " } else { &shown }, &wide, 100_000.0) else { return };
        if !s.is_empty() {
            self.layout = Some(layout.clone());
        } else {
            self.layout = scene.cache.get(cx.text(), " ", &wide, 100_000.0);
        }
        // прокрутка: каретка всегда видна
        let caret_x = if s.is_empty() { 0.0 } else { caret_pos(&layout, char_to_utf16(&self.text, self.caret)).0 };
        if caret_x - self.scroll_x > inner.w - 1.0 {
            self.scroll_x = caret_x - inner.w + 1.0;
        } else if caret_x < self.scroll_x {
            self.scroll_x = caret_x;
        }
        if s.is_empty() || layout.width <= inner.w {
            self.scroll_x = self.scroll_x.min((layout.width - inner.w).max(0.0)).max(0.0);
        }
        let ty = r.y + (r.h - layout.height) / 2.0;
        let tx = self.text_left();
        let (a, b) = self.selection();
        if push_round_layer(cx, inner, 0.0, 1.0) {
            if s.is_empty() {
                scene.set_clip(Some(inner));
                scene.text(&layout, tx, ty, ACC_4, Glow::G1);
                scene.flush(cx);
                scene.set_clip(None);
            } else {
                // ::selection { background: acc; color: glass; text-shadow: none }
                if self.focused && a != b {
                    for rect in range_rects(&layout, char_to_utf16(&self.text, a), char_to_utf16(&self.text, b)) {
                        fill_round(cx, Rect::new(tx + rect.left, ty + rect.top, rect.right - rect.left, rect.bottom - rect.top), 0.0, ACC);
                    }
                }
                scene.set_clip(Some(inner));
                scene.text(&layout, tx, ty, ACC_HOT, Glow::G1);
                scene.flush(cx);
                scene.set_clip(None);
                if self.focused && a != b {
                    // выделенные буквы — цветом стекла поверх подсветки
                    for rect in range_rects(&layout, char_to_utf16(&self.text, a), char_to_utf16(&self.text, b)) {
                        let clip = D2D_RECT_F { left: tx + rect.left, top: ty + rect.top, right: tx + rect.right, bottom: ty + rect.bottom };
                        unsafe {
                            cx.dc().PushAxisAlignedClip(&clip, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
                        }
                        cx.draw_text(&layout, tx, ty, GLASS);
                        unsafe { cx.dc().PopAxisAlignedClip() };
                    }
                }
            }
            if self.focused && self.caret_on(cx.now) {
                // caret-color: acc
                let x = (tx + caret_x).round();
                cx.fill_rect(Rect::new(x, ty, 1.0, layout.height), ACC);
            }
            pop_layer(cx);
        }
        let _ = with_alpha;
    }
}

fn caret_pos(l: &TextLayout, utf16: usize) -> (f32, f32) {
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        let _ = l.layout.HitTestTextPosition(utf16 as u32, false, &mut x, &mut y, &mut m);
    }
    (x, y)
}

fn range_rects(l: &TextLayout, from: usize, to: usize) -> Vec<D2D_RECT_F> {
    let mut count = 0u32;
    unsafe {
        let _ = l.layout.HitTestTextRange(from as u32, (to - from) as u32, 0.0, 0.0, None, &mut count);
        let mut m = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
        if l.layout.HitTestTextRange(from as u32, (to - from) as u32, 0.0, 0.0, Some(&mut m), &mut count).is_err() {
            return Vec::new();
        }
        m.iter().map(|h| D2D_RECT_F { left: h.left, top: h.top, right: h.left + h.width, bottom: h.top + h.height }).collect()
    }
}

fn char_to_utf16(text: &[char], i: usize) -> usize {
    text[..i.min(text.len())].iter().map(|c| c.len_utf16()).sum()
}

fn utf16_to_char(text: &[char], u: usize) -> usize {
    let mut acc = 0;
    for (i, c) in text.iter().enumerate() {
        if acc >= u {
            return i;
        }
        acc += c.len_utf16();
    }
    text.len()
}

/// Текст в буфер обмена (владелец — окно поля: с пустым владельцем SetClipboardData ненадёжен).
pub fn clipboard_set(hwnd: isize, s: &str) {
    let wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(HWND(hwnd as *mut _)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                let _ = GlobalUnlock(mem);
                let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(mem.0));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Текст из буфера обмена.
pub fn clipboard_get(hwnd: isize) -> Option<String> {
    unsafe {
        OpenClipboard(HWND(hwnd as *mut _)).ok()?;
        let out = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let mem = HGLOBAL(h.0);
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(mem);
            Some(s)
        })();
        let _ = CloseClipboard();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: u16, ctrl: bool, shift: bool) -> KeyEvent {
        KeyEvent { key: k as u32, ctrl, shift, alt: false, repeat: false }
    }

    #[test]
    fn typing_selection_and_undo() {
        let mut t = TextInput::default();
        for c in "привет мир".chars() {
            t.char_input(c);
        }
        assert_eq!(t.text(), "привет мир");
        t.key(key(VK_LEFT.0, true, true)); // выделить «мир»
        t.char_input('Ж');
        assert_eq!(t.text(), "привет Ж");
        t.key(key(VK_Z.0, true, false));
        assert_eq!(t.text(), "привет мир");
        t.key(key(VK_Z.0, true, false));
        assert_eq!(t.text(), "");
        t.key(key(VK_Y.0, true, false));
        assert_eq!(t.text(), "привет мир");
    }

    #[test]
    fn word_delete_and_home_end() {
        let mut t = TextInput::default();
        t.set_text("one two three");
        t.key(key(VK_BACK.0, true, false));
        assert_eq!(t.text(), "one two ");
        t.key(key(VK_HOME.0, false, true));
        t.char_input('x');
        assert_eq!(t.text(), "x");
    }

    #[test]
    fn utf16_mapping_with_surrogates() {
        let text: Vec<char> = "a😀b".chars().collect();
        assert_eq!(char_to_utf16(&text, 2), 3);
        assert_eq!(utf16_to_char(&text, 3), 2);
    }
}
