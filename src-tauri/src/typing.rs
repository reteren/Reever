//! Ввод продиктованного текста: есть ли у активного окна поле с курсором, печать в него
//! (`SendInput`, Unicode) и запасной путь — буфер обмена.

use std::{
    ffi::c_void,
    mem::size_of,
    sync::{Arc, Mutex},
    thread::sleep,
    time::Duration,
};

use anyhow::{bail, Context};
use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HINSTANCE, HWND},
    System::{
        Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, SetClipboardData,
        },
        LibraryLoader::GetModuleHandleW,
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        Threading::GetCurrentProcessId,
    },
    UI::{
        Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_DocumentControlTypeId,
            UIA_EditControlTypeId, UIA_ValuePatternId,
        },
        Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_RETURN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CreateWindowExW, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
            GetWindowLongPtrW, GetWindowThreadProcessId, GA_ROOT, GA_ROOTOWNER, GUITHREADINFO, GWL_STYLE,
            HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
        },
    },
};

use crate::dictation::Piece;

/// Короткая пауза даёт целевому окну обработать символ до следующей проверки фокуса.
const CHAR_PAUSE: Duration = Duration::from_millis(10);
const CF_UNICODETEXT: u32 = 13;
const ES_READONLY_STYLE: u32 = 0x0800;

thread_local! {
    /// `SetClipboardData` требует owner HWND. Message-only окно невидимо и
    /// принадлежит этому потоку, поэтому clipboard ownership не остаётся NULL.
    static CLIPBOARD_OWNER: HWND = create_clipboard_owner();
}

fn create_clipboard_owner() -> HWND {
    let instance = unsafe {
        GetModuleHandleW(windows::core::PCWSTR::null())
            .map(|module| HINSTANCE(module.0))
            .unwrap_or_default()
    };
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            windows::core::w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            instance,
            None,
        )
        .unwrap_or_default()
    }
}

/// Окна оболочки, у которых «фокус» есть, а поля для текста нет.
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// Активное окно, если в нём фокус на поле ввода (виден текстовый курсор или UI Automation
/// говорит, что в фокусе редактируемое поле). Своё окно Reever — не цель.
fn text_target_local() -> Option<isize> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    let thread = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return None;
    }
    if SHELL_CLASSES.contains(&class_name(foreground).as_str()) {
        return None;
    }

    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let has_caret =
        unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok() && !info.hwndCaret.0.is_null();
    if has_caret && native_edit_is_read_only(info.hwndFocus) {
        return None;
    }
    match focused_element_editable(pid) {
        Some(true) => Some(foreground.0 as isize),
        Some(false) => None,
        None => has_caret.then_some(foreground.0 as isize),
    }
}

/// Что сейчас в фокусе — для журнала, когда поле ввода не нашлось: класс активного окна,
/// его процесс и что видит UI Automation (тип элемента, процесс, только ли для чтения).
fn describe_focus_local() -> String {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return "no active window".into();
    }
    let mut pid = 0u32;
    let thread = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let caret = unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok() && !info.hwndCaret.0.is_null();
    let uia = AUTOMATION.with(|automation| {
        let automation = automation.as_ref()?;
        unsafe {
            let element = automation.GetFocusedElement().ok()?;
            let control = element.CurrentControlType().ok().map(|c| c.0).unwrap_or_default();
            let element_pid = element.CurrentProcessId().unwrap_or_default();
            let readonly = element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|value| value.CurrentIsReadOnly().ok())
                .map(|ro| if ro.as_bool() { "read-only" } else { "editable" })
                .unwrap_or("no ValuePattern");
            Some(format!("UIA type {control}, pid {element_pid}, {readonly}"))
        }
    });
    format!(
        "window class «{}», pid {pid}, system caret: {}, {}",
        class_name(foreground),
        if caret { "yes" } else { "no" },
        uia.unwrap_or_else(|| "UIA: no element".into())
    )
}

/// UI Automation может быть недоступна в потоке приложения, поэтому дополнительно
/// проверяем штатный ES_READONLY у нативных EDIT/RichEdit, если там стоит курсор.
fn native_edit_is_read_only(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let class = class_name(hwnd);
    if class != "Edit" && !class.starts_with("RichEdit") {
        return false;
    }
    (unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32) & ES_READONLY_STYLE != 0
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

thread_local! {
    static AUTOMATION: Option<IUIAutomation> = unsafe {
        // COM инициализирован потоком ввода (STA, см. input_thread.rs).
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
    };
}

/// Браузеры и Electron не рисуют системный курсор — у них поле видно только через UIA:
/// в фокусе Edit или Document, и если у него есть ValuePattern, то не только для чтения
/// (так страница без активного поля в браузере — не цель, а документ Word — цель).
fn focused_element_editable(foreground_pid: u32) -> Option<bool> {
    AUTOMATION.with(|automation| {
        let Some(automation) = automation else {
            return None;
        };
        unsafe {
            let Ok(element) = automation.GetFocusedElement() else {
                return None;
            };
            let element_pid = match element.CurrentProcessId() {
                Ok(pid) => pid as u32,
                Err(_) => return Some(false),
            };
            // Процесс элемента не сверяем строго: у WebView2/Chromium поле живёт в процессе
            // рендерера, который может быть и не потомком окна (журнал пользователя: окно
            // pid 48532, поле pid 49924). Фокус и так в активном окне, а печать идёт туда же.
            let _ = (element_pid, foreground_pid);
            let control_type = element.CurrentControlType().ok()?;
            if control_type != UIA_EditControlTypeId && control_type != UIA_DocumentControlTypeId {
                return Some(false);
            }
            match element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                Ok(value) => value.CurrentIsReadOnly().ok().map(|ro| !ro.as_bool()),
                Err(_) => Some(true),
            }
        }
    })
}


/// Префикс, который удалось напечатать, и точный остаток для буфера обмена.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeProgress {
    pub typed: Vec<Piece>,
    pub remaining: Vec<Piece>,
}

/// Напечатать куски только пока активно исходное поле. Снимок `hwndFocus` защищает
/// от смены поля даже внутри того же верхнего окна приложения.
fn type_pieces_local(target: isize, pieces: &[Piece]) -> TypeProgress {
    let Ok(focus) = current_focus(target) else {
        return TypeProgress {
            typed: Vec::new(),
            remaining: pieces.to_vec(),
        };
    };
    let mut progress = TypeProgress::default();

    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Text(text) => {
                let typed_chars = type_text(target, focus, text);
                let total_chars = text.chars().count();
                if typed_chars < total_chars {
                    if typed_chars > 0 {
                        let split = text
                            .char_indices()
                            .nth(typed_chars)
                            .map(|(index, _)| index)
                            .unwrap_or(text.len());
                        progress.typed.push(Piece::Text(text[..split].to_string()));
                        if split < text.len() {
                            progress
                                .remaining
                                .push(Piece::Text(text[split..].to_string()));
                        }
                    } else {
                        progress.remaining.push(piece.clone());
                    }
                    progress.remaining.extend_from_slice(&pieces[index + 1..]);
                    return progress;
                }
                progress.typed.push(piece.clone());
            }
            Piece::NewLine => {
                if ensure_target(target, focus).is_err() {
                    progress.remaining.extend_from_slice(&pieces[index..]);
                    return progress;
                }
                let sent = send_inputs(&shift_enter()) as usize;
                if sent < 4 {
                    release_shift_enter();
                }
                if sent >= 2 {
                    progress.typed.push(Piece::NewLine);
                } else {
                    progress.remaining.push(Piece::NewLine);
                }
                if sent < 4 {
                    progress.remaining.extend_from_slice(&pieces[index + 1..]);
                    return progress;
                }
            }
        }
    }
    progress
}

fn type_text(target: isize, focus: HWND, text: &str) -> usize {
    let mut typed_chars = 0;
    for character in text.chars() {
        if ensure_target(target, focus).is_err() {
            return typed_chars;
        }
        // Не разрываем суррогатную пару на границе отдельных проверок фокуса.
        let mut encoded = [0u16; 2];
        let units = character.encode_utf16(&mut encoded);
        let mut inputs = Vec::with_capacity(units.len() * 2);
        for &unit in units.iter() {
            inputs.push(key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
            inputs.push(key(
                VIRTUAL_KEY(0),
                unit,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ));
        }
        let sent = (send_inputs(&inputs) as usize).min(inputs.len());
        typed_chars += chars_after_input_events(units, sent);
        if sent < inputs.len() {
            if sent % 2 == 1 {
                let unit = units[sent / 2];
                let key_up = key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
                let _ = send_inputs(&[key_up]);
            }
            return typed_chars;
        }
        sleep(CHAR_PAUSE);
    }
    typed_chars
}

fn chars_after_input_events(units: &[u16], sent_events: usize) -> usize {
    let mut unit_index = 0;
    let mut chars = 0;
    for decoded in char::decode_utf16(units.iter().copied()) {
        let width = decoded
            .as_ref()
            .map_or(1, |character| character.len_utf16());
        let key_down_count = 2 * (unit_index + width - 1) + 1;
        if sent_events < key_down_count {
            break;
        }
        chars += 1;
        unit_index += width;
    }
    chars
}

fn current_focus(target: isize) -> anyhow::Result<HWND> {
    ensure_foreground_window(target)?;
    let thread = unsafe { GetWindowThreadProcessId(hwnd_from(target), None) };
    if thread == 0 {
        bail!("failed to find the target window thread");
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.context("failed to read the focus")?;
    if info.hwndFocus.0.is_null() {
        bail!("the target window has no focused field");
    }
    Ok(info.hwndFocus)
}

fn ensure_target(target: isize, expected_focus: HWND) -> anyhow::Result<()> {
    ensure_foreground_window(target)?;
    let thread = unsafe { GetWindowThreadProcessId(hwnd_from(target), None) };
    if thread == 0 {
        bail!("failed to find the target window thread");
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.context("failed to check the input field")?;
    if info.hwndFocus.0 != expected_focus.0 {
        bail!("the focused field changed - typing stopped");
    }
    Ok(())
}

fn ensure_foreground_window(target: isize) -> anyhow::Result<()> {
    let foreground = unsafe { GetForegroundWindow() };
    let root = unsafe { GetAncestor(foreground, GA_ROOT) };
    // Всплывающее окно самого приложения (подсказка, автодополнение, меню) принадлежит цели.
    let owner = unsafe { GetAncestor(foreground, GA_ROOTOWNER) };
    if foreground.0 as isize == target || root.0 as isize == target || owner.0 as isize == target {
        Ok(())
    } else {
        bail!("the active window changed - typing stopped")
    }
}

fn shift_enter() -> [INPUT; 4] {
    [
        key(VK_SHIFT, 0, KEYBD_EVENT_FLAGS(0)),
        key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)),
        key(VK_RETURN, 0, KEYEVENTF_KEYUP),
        key(VK_SHIFT, 0, KEYEVENTF_KEYUP),
    ]
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn release_shift_enter() {
    let releases = [
        key(VK_RETURN, 0, KEYEVENTF_KEYUP),
        key(VK_SHIFT, 0, KEYEVENTF_KEYUP),
    ];
    let _ = send_inputs(&releases);
}

fn send_inputs(inputs: &[INPUT]) -> u32 {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) }
}

/// Положить текст в буфер обмена (заменяя прежнее содержимое).
fn set_clipboard_local(text: &str) -> anyhow::Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let owner = CLIPBOARD_OWNER.with(|owner| *owner);
    if owner.0.is_null() {
        bail!("failed to create a clipboard owner");
    }
    unsafe {
        // Буфер бывает занят другой программой — несколько коротких попыток.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(owner).is_ok() {
                opened = true;
                break;
            }
            sleep(Duration::from_millis(20));
        }
        if !opened {
            bail!("the clipboard is held by another program");
        }
        let result = (|| -> anyhow::Result<()> {
            EmptyClipboard().context("failed to empty the clipboard")?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)
                .context("failed to allocate clipboard memory")?;
            let ptr = GlobalLock(memory) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(memory);
                bail!("failed to lock clipboard memory");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(memory);
            if let Err(error) = SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0 as *mut c_void)) {
                let _ = GlobalFree(memory);
                return Err(error).context("failed to write text to the clipboard");
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

// Публичный вход — только через поток ввода (input_thread.rs): UIA, SendInput и окно-
// владелец буфера не должны жить в аудио-воркере, который запускает программы.

/// Активное окно, если в нём фокус на поле ввода; см. `text_target_local`.
pub fn text_target() -> Option<isize> {
    crate::input_thread::call(text_target_local).flatten()
}

/// Напечатать куски в поле `target`; см. `type_pieces_local`.
pub fn type_pieces(target: isize, pieces: &[Piece]) -> TypeProgress {
    let owned = pieces.to_vec();
    crate::input_thread::call(move || type_pieces_local(target, &owned)).unwrap_or_else(|| {
        TypeProgress {
            typed: Vec::new(),
            remaining: pieces.to_vec(),
        }
    })
}

/// Нажать Enter в окне `target` (отправить сообщение в чате), если оно всё ещё активно.
pub fn press_enter(target: isize) -> bool {
    crate::input_thread::call(move || {
        ensure_foreground_window(target).is_ok()
            && send_inputs(&[
                key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)),
                key(VK_RETURN, 0, KEYEVENTF_KEYUP),
            ]) == 2
    })
    .unwrap_or(false)
}

/// Активно ли ещё окно `target` (или его дочернее): выбранное в начале диктовки окно
/// остаётся целью, пока пользователь не переключился.
pub fn is_foreground(target: isize) -> bool {
    ensure_foreground_window(target).is_ok()
}

fn press_backspace_local(target: isize, count: usize) -> usize {
    let mut sent = 0;
    while sent < count {
        if ensure_foreground_window(target).is_err() {
            break;
        }
        let batch = (count - sent).min(24);
        let mut inputs = Vec::with_capacity(batch * 2);
        for _ in 0..batch {
            inputs.push(key(VK_BACK, 0, KEYBD_EVENT_FLAGS(0)));
            inputs.push(key(VK_BACK, 0, KEYEVENTF_KEYUP));
        }
        if send_inputs(&inputs) as usize != inputs.len() {
            break;
        }
        sent += batch;
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    sent
}

/// Стереть `count` символов Backspace в окне `target` (пока оно активно). Возвращает,
/// сколько нажатий отправлено.
pub fn press_backspace(target: isize, count: usize) -> usize {
    crate::input_thread::call(move || press_backspace_local(target, count))
    .unwrap_or(0)
}

/// Текст из буфера обмена (`None` — там не текст или буфер занят).
pub fn get_clipboard_text() -> Option<String> {
    crate::input_thread::call(get_clipboard_text_local).flatten()
}

fn get_clipboard_text_local() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        let owner = CLIPBOARD_OWNER.with(|owner| *owner);
        OpenClipboard(owner).ok()?;
        let text = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let memory = windows::Win32::Foundation::HGLOBAL(handle.0);
            let ptr = GlobalLock(memory) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(memory);
            Some(text)
        })();
        let _ = CloseClipboard();
        text
    }
}

/// Описание фокуса для журнала; см. `describe_focus_local`.
pub fn describe_focus() -> String {
    crate::input_thread::call(describe_focus_local).unwrap_or_default()
}

/// Положить текст в буфер обмена; см. `set_clipboard_local`.
pub fn set_clipboard(text: &str) -> anyhow::Result<()> {
    let owned = text.to_string();
    crate::input_thread::call(move || set_clipboard_local(&owned))
        .unwrap_or_else(|| Err(anyhow::anyhow!("the input thread is unavailable")))
}

/// Текст, напечатанный в поле по ходу речи, и его выравнивание с новым вариантом: стереть
/// расходящийся хвост (Backspace) и допечатать остальное. Перевод строки — `\n`
/// (Shift+Enter, одно нажатие Backspace стирает его).
#[derive(Clone, Debug, Default)]
pub struct LiveField {
    /// Что сейчас напечатано (то, что Reever сам ввёл в этот кусок).
    pub typed: String,
}

impl LiveField {
    /// Привести напечатанное к `wanted` в окне `target`. `false` — окно сменилось или ввод не
    /// прошёл; `typed` тогда отражает то, что реально успело напечататься.
    pub fn reconcile(&mut self, target: isize, wanted: &str) -> bool {
        self.reconcile_with(target, wanted, false)
    }

    /// `reconcile`; `local` — вызвано в самом потоке ввода (`LiveTypist`): печатать
    /// напрямую, а не заданием в тот же поток, которое ждало бы само себя.
    fn reconcile_with(&mut self, target: isize, wanted: &str, local: bool) -> bool {
        let typed: Vec<char> = self.typed.chars().collect();
        let wanted_chars: Vec<char> = wanted.chars().collect();
        let prefix = typed
            .iter()
            .zip(wanted_chars.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let erase = typed.len() - prefix;
        if erase > 0 {
            let sent = if local {
                press_backspace_local(target, erase)
            } else {
                press_backspace(target, erase)
            };
            self.typed = typed[..typed.len() - sent].iter().collect();
            if sent < erase {
                return false;
            }
        }
        let rest: String = wanted_chars[prefix..].iter().collect();
        if rest.is_empty() {
            return true;
        }
        let pieces = text_to_pieces(&rest);
        let progress = if local {
            type_pieces_local(target, &pieces)
        } else {
            type_pieces(target, &pieces)
        };
        self.typed.push_str(&pieces_to_text(&progress.typed));
        progress.remaining.is_empty()
    }

    /// Окончательный текст куска (`settle_goal`): напечатанное остаётся, допечатывается
    /// продолжение. Возвращает то, что напечатать не удалось (окно сменилось) — пусто, если всё в поле.
    pub fn settle(&mut self, target: isize, wanted: &str) -> String {
        let goal = settle_goal(&self.typed, wanted);
        if self.reconcile(target, &goal) {
            return String::new();
        }
        let prefix = common_prefix_chars(&self.typed, &goal);
        goal.chars().skip(prefix).collect()
    }
}

/// Печать по ходу речи без ожидания: воркер отдаёт цель и сразу распознаёт дальше, а поток
/// ввода дописывает поле до последней цели по слову за задание. Раньше воркер стоял, пока
/// печатался каждый символ (`CHAR_PAUSE`), звук копился в очереди, и следующий вариант
/// приходил позже — текст в поле шёл рывками. Между словами в потоке ввода успевают
/// проверка орфографии и прочие задания. Промежуточные цели, которые устарели раньше, чем
/// до них дошла печать, пропускаются.
#[derive(Clone, Default)]
pub struct LiveTypist {
    state: Arc<Mutex<TypistState>>,
}

#[derive(Default)]
struct TypistState {
    field: LiveField,
    /// Последняя цель: окно и текст, к которому привести поле.
    goal: Option<(isize, String)>,
    /// Задание печати стоит в очереди потока ввода или выполняется.
    busy: bool,
    /// Окно сменилось или ввод не прошёл — до `finish` не печатаем.
    failed: bool,
}

impl LiveTypist {
    fn lock(&self) -> std::sync::MutexGuard<'_, TypistState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Привести поле к `goal`, не дожидаясь печати. `false` — печать уже не удалась
    /// (окно сменилось); новые цели до `finish` не принимаются.
    pub fn post(&self, target: isize, goal: String) -> bool {
        let mut state = self.lock();
        if state.failed {
            return false;
        }
        state.goal = Some((target, goal));
        if !state.busy {
            state.busy = true;
            let typist = self.clone();
            if !crate::input_thread::post(move || typist.step()) {
                state.busy = false;
                state.failed = true;
                return false;
            }
        }
        true
    }

    /// Одно слово к цели (или стирание расходящегося хвоста) и следующее задание в очередь.
    fn step(&self) {
        let (target, goal, mut field) = {
            let mut state = self.lock();
            match state.goal.clone() {
                Some((target, goal)) if !state.failed && state.field.typed != goal => {
                    (target, goal, state.field.clone())
                }
                _ => {
                    state.goal = None;
                    state.busy = false;
                    return;
                }
            }
        };
        let next = next_live_step(&field.typed, &goal);
        let ok = field.reconcile_with(target, &next, true);
        let mut state = self.lock();
        state.field = field;
        if !ok {
            state.failed = true;
            state.goal = None;
            state.busy = false;
            return;
        }
        let typist = self.clone();
        if !crate::input_thread::post(move || typist.step()) {
            state.failed = true;
            state.busy = false;
        }
    }

    /// Дождаться, пока допечатается последняя цель, и забрать поле: дальше с ним работает
    /// воркер (готовый кусок, конец, отмена). `true` во втором — печать не удалась.
    pub fn finish(&self) -> (LiveField, bool) {
        // Поток ввода выполняет задания по очереди: пустое задание в конце очереди
        // дожидается очередного слова печати. Предел — на случай упавшего задания.
        for _ in 0..10_000 {
            if !self.lock().busy || crate::input_thread::call(|| ()).is_none() {
                break;
            }
        }
        let mut state = self.lock();
        let state = std::mem::take(&mut *state);
        (state.field, state.failed)
    }
}

/// Ближайший шаг от `typed` к `goal`: стереть расходящийся хвост, иначе допечатать одно
/// слово (с пробелами перед ним).
fn next_live_step(typed: &str, goal: &str) -> String {
    let prefix = common_prefix_chars(typed, goal);
    if prefix < typed.chars().count() {
        return goal.chars().take(prefix).collect();
    }
    let rest_start = goal
        .char_indices()
        .nth(prefix)
        .map_or(goal.len(), |(index, _)| index);
    let rest = &goal[rest_start..];
    let word_start = rest.len() - rest.trim_start().len();
    let word_end = rest[word_start..]
        .find(char::is_whitespace)
        .map_or(rest.len(), |end| word_start + end);
    goal[..rest_start + word_end].to_string()
}

/// Сколько символов в конце уже напечатанного можно переписать: исправление последних
/// пары слов незаметно, а стирание целого предложения выглядит как «пишет туда-сюда».
pub const LIVE_REWRITE_CHARS: usize = 24;
/// По ходу речи — только последнее слово-другое: остальное поправит окончательный текст.
pub const PARTIAL_REWRITE_CHARS: usize = 12;

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Байтовые границы слов (непробельных отрезков) строки.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        match (character.is_whitespace(), start) {
            (true, Some(begin)) => {
                spans.push((begin, index));
                start = None;
            }
            (false, None) => start = Some(index),
            _ => {}
        }
    }
    if let Some(begin) = start {
        spans.push((begin, text.len()));
    }
    spans
}

/// Слово без регистра и знаков — для сравнения вариантов распознавания.
fn word_key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'ё' { 'е' } else { c })
        .collect()
}

/// Сколько первых слов `wanted` соответствует напечатанным словам `typed`: префикс `wanted`
/// с наименьшим словарным расстоянием до `typed` (слова могли слиться, разделиться, смениться).
fn aligned_words(typed: &[&str], wanted: &[&str]) -> usize {
    let typed: Vec<String> = typed.iter().map(|w| word_key(w)).collect();
    let wanted: Vec<String> = wanted.iter().map(|w| word_key(w)).collect();
    // row[j] — расстояние между всеми словами typed и первыми j словами wanted.
    let mut row: Vec<usize> = (0..=wanted.len()).collect();
    for (i, typed_word) in typed.iter().enumerate() {
        let mut next = vec![i + 1; wanted.len() + 1];
        for (j, wanted_word) in wanted.iter().enumerate() {
            let cost = usize::from(typed_word != wanted_word);
            next[j + 1] = (row[j] + cost).min(row[j + 1] + 1).min(next[j] + 1);
        }
        row = next;
    }
    // При равенстве — больший префикс: недопечатать слово хуже, чем повторить.
    let best = row.iter().copied().min().unwrap_or(0);
    row.iter().rposition(|&distance| distance == best).unwrap_or(0)
}

/// Во что превратить поле, когда кусок готов. Расхождение в хвосте (до `LIVE_REWRITE_CHARS`
/// напечатанного) исправляется; если модель передумала раньше, напечатанное остаётся,
/// а из окончательного текста допечатывается то, что идёт после совпавших слов.
pub fn settle_goal(typed: &str, wanted: &str) -> String {
    let erase = typed.chars().count() - common_prefix_chars(typed, wanted);
    if erase <= LIVE_REWRITE_CHARS {
        return wanted.to_string();
    }
    let typed_spans = word_spans(typed);
    let wanted_spans = word_spans(wanted);
    let typed_words: Vec<&str> = typed_spans.iter().map(|&(a, b)| &typed[a..b]).collect();
    let wanted_words: Vec<&str> = wanted_spans.iter().map(|&(a, b)| &wanted[a..b]).collect();
    let aligned = aligned_words(&typed_words, &wanted_words);
    if aligned == 0 {
        return wanted.to_string();
    }
    let tail_start = wanted_spans[aligned - 1].1;
    let tail = &wanted[tail_start..];
    let mut goal = typed.trim_end().to_string();
    goal.push_str(tail);
    goal
}

/// Что печатать по ходу речи. `preview` — оформленный промежуточный текст, `previous` —
/// прошлый. Последнее слово ещё меняется, поэтому печатаются только слова, которые
/// совпали в двух вариантах подряд; стирать по ходу речи можно лишь чуть-чуть
/// (`PARTIAL_REWRITE_CHARS`), остальное выровняет окончательный текст. `None` — ничего не делать.
///
/// `settled` — вариант посчитан, когда после речи уже пауза (`Engine`): последнее слово
/// договорено, и печатается всё, не дожидаясь окончательного текста куска. Так же, если
/// вариант не изменился: новых слов не прозвучало, последнее слово закончено.
pub fn partial_goal(typed: &str, previous: &str, preview: &str, settled: bool) -> Option<String> {
    let spans = word_spans(preview);
    let previous_spans = word_spans(previous);
    let agreed = if settled {
        spans.len()
    } else {
        // Без последнего (ещё звучащего) слова, если вариант изменился.
        let stable = if preview == previous {
            spans.len()
        } else {
            spans.len().saturating_sub(1)
        };
        spans[..stable]
            .iter()
            .zip(previous_spans.iter())
            .take_while(|(&(a, b), &(c, d))| preview[a..b] == previous[c..d])
            .count()
    };
    if agreed == 0 {
        return None;
    }
    let goal = &preview[..spans[agreed - 1].1];
    if goal.chars().count() <= typed.chars().count() && typed.starts_with(goal) {
        return None;
    }
    let erase = typed.chars().count() - common_prefix_chars(typed, goal);
    (erase <= PARTIAL_REWRITE_CHARS).then(|| goal.to_string())
}

/// Куски оформленного текста → строка с `\n` на месте перевода строки.
pub fn pieces_to_text(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Text(text) => text.as_str(),
            Piece::NewLine => "\n",
        })
        .collect()
}

/// Обратно: строка с `\n` → куски для печати.
pub fn text_to_pieces(text: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    for (index, part) in text.split('\n').enumerate() {
        if index > 0 {
            pieces.push(Piece::NewLine);
        }
        if !part.is_empty() {
            pieces.push(Piece::Text(part.to_string()));
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_fixes_only_the_tail() {
        assert_eq!(settle_goal("привет как де", "Привет, как дела?"), "Привет, как дела?");
        assert_eq!(settle_goal("как дела", "как делишки"), "как делишки");
    }

    #[test]
    fn settle_keeps_typed_text_when_the_model_changes_its_mind_early() {
        let typed = "я давно уже хотел сделать механику что любую ноту";
        let wanted = "Я давно хотел сделать механику, что любую ноду можно покрасить.";
        assert_eq!(
            settle_goal(typed, wanted),
            "я давно уже хотел сделать механику что любую ноту можно покрасить."
        );
    }

    #[test]
    fn settle_with_nothing_typed_is_the_final_text() {
        assert_eq!(settle_goal("", " новый кусок"), " новый кусок");
    }

    #[test]
    fn partial_types_only_words_seen_twice() {
        assert_eq!(partial_goal("", "", "привет как дела", false), None);
        assert_eq!(
            partial_goal("", "привет как де", "привет как дела у", false),
            Some("привет как".into())
        );
        // Последнее слово сменилось — уже напечатанное не трогаем.
        assert_eq!(partial_goal("привет как", "привет как де", "привет как дела", false), None);
    }

    #[test]
    fn live_steps_go_word_by_word_and_erase_first() {
        assert_eq!(next_live_step("", "Привет, как дела"), "Привет,");
        assert_eq!(next_live_step("Привет,", "Привет, как дела"), "Привет, как");
        assert_eq!(next_live_step("Привет, как", "Привет, как дела"), "Привет, как дела");
        assert_eq!(next_live_step("Привет, как де", "Привет, как дела"), "Привет, как дела");
        assert_eq!(next_live_step("Привет, как", "Привет,\nкак дела"), "Привет,");
        assert_eq!(next_live_step("Привет,", "Привет,\nкак"), "Привет,\nкак");
    }

    #[test]
    fn partial_types_the_last_word_once_it_is_finished() {
        // Вариант не изменился — новых слов нет, последнее договорено.
        assert_eq!(
            partial_goal("привет как", "привет как дела", "привет как дела", false),
            Some("привет как дела".into())
        );
        // После речи пауза — печатается всё сразу, даже первый вариант.
        assert_eq!(
            partial_goal("", "", "привет как дела", true),
            Some("привет как дела".into())
        );
        assert_eq!(partial_goal("привет как дела", "", "привет как дела", true), None);
    }

    #[test]
    fn partial_never_erases_far_back() {
        let typed = "раз два три четыре пять шесть семь";
        assert_eq!(
            partial_goal(typed, "раз дважды три четыре пять шесть семь восемь", "раз дважды три четыре пять шесть семь восемь девять", false),
            None
        );
        assert_eq!(
            partial_goal(typed, "", "раз дважды три четыре пять шесть семь", true),
            None
        );
        assert_eq!(
            partial_goal("раз два три", "раз два тридцать пять", "раз два тридцать пять шесть", false),
            Some("раз два тридцать пять".into())
        );
    }
}
