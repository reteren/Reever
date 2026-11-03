//! Каркас приложения без Tauri (этап 5 docs/native-ui-roadmap.md): то немногое, что Reever брал
//! у Tauri, — общее состояние, события между модулями, папки, вшитые ресурсы интерфейса,
//! мониторы и главный поток. Имена методов те же, что были у Tauri (`state`, `emit`,
//! `listen_any`, `path().app_config_dir()`…), чтобы перенос был механическим.
//!
//! Главный поток крутит цикл сообщений Windows (`run`): в него приходят задачи
//! `run_on_main_thread`, по выходу он возвращается. Окна интерфейса живут в своём потоке
//! (`native::UiThread`), значок трея — в потоке `traymenu`.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use windows::Win32::Foundation::{LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONULL,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_APP, WM_QUIT,
};

/// Идентификатор приложения: папка настроек %APPDATA%\com.troid.reever (как было у Tauri).
pub const IDENTIFIER: &str = "com.troid.reever";
/// Версия из Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

type Listener = Arc<dyn Fn(Event) + Send + Sync>;
type Task = Box<dyn FnOnce() + Send>;

struct Inner {
    states: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    listeners: RwLock<Vec<(EventId, String, Listener)>>,
    next_listener: AtomicU64,
    main_thread: AtomicU32,
    tasks: Mutex<Vec<Task>>,
}

/// Ручка приложения: дёшево клонируется, живёт во всех потоках.
#[derive(Clone)]
pub struct AppHandle(Arc<Inner>);

/// Событие для подписчиков: полезная нагрузка — JSON, как было у Tauri.
pub struct Event {
    payload: String,
}

impl Event {
    pub fn payload(&self) -> &str {
        &self.payload
    }
}

pub type EventId = u64;

/// Ссылка на общее состояние (`app.state::<T>()`).
pub struct State<'a, T: Send + Sync + 'static> {
    value: Arc<dyn Any + Send + Sync>,
    _life: std::marker::PhantomData<&'a T>,
}

impl<T: Send + Sync + 'static> State<'_, T> {
    pub fn inner(&self) -> &T {
        self.value.downcast_ref::<T>().expect("state type")
    }
}

impl<T: Send + Sync + 'static> std::ops::Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.inner()
    }
}

impl<T: Send + Sync + 'static> Clone for State<'_, T> {
    fn clone(&self) -> Self {
        Self { value: self.value.clone(), _life: std::marker::PhantomData }
    }
}

/// Сообщение главному потоку: пора выполнить задачи из очереди.
const WM_TASKS: u32 = WM_APP + 0x51;

impl AppHandle {
    /// Новое приложение; главный поток — тот, что потом вызовет `run`.
    pub fn new() -> Self {
        Self(Arc::new(Inner {
            states: RwLock::new(HashMap::new()),
            listeners: RwLock::new(Vec::new()),
            next_listener: AtomicU64::new(1),
            main_thread: AtomicU32::new(0),
            tasks: Mutex::new(Vec::new()),
        }))
    }

    /// Положить общее состояние (одно значение на тип). Повторный вызов не заменяет.
    pub fn manage<T: Send + Sync + 'static>(&self, value: T) -> bool {
        let mut states = self.0.states.write().unwrap();
        if states.contains_key(&TypeId::of::<T>()) {
            return false;
        }
        states.insert(TypeId::of::<T>(), Arc::new(value));
        true
    }

    pub fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>> {
        let value = self.0.states.read().unwrap().get(&TypeId::of::<T>())?.clone();
        Some(State { value, _life: std::marker::PhantomData })
    }

    /// Общее состояние; паника, если его не положили (как у Tauri).
    pub fn state<T: Send + Sync + 'static>(&self) -> State<'_, T> {
        self.try_state::<T>().unwrap_or_else(|| panic!("state {} is not managed", std::any::type_name::<T>()))
    }

    /// Разослать событие подписчикам; нагрузка уходит JSON-строкой.
    pub fn emit<S: serde::Serialize>(&self, event: &str, payload: S) -> Result<(), String> {
        let payload = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        let targets: Vec<Listener> =
            self.0.listeners.read().unwrap().iter().filter(|(_, name, _)| name == event).map(|(_, _, f)| f.clone()).collect();
        for f in targets {
            f(Event { payload: payload.clone() });
        }
        Ok(())
    }

    pub fn listen_any<F: Fn(Event) + Send + Sync + 'static>(&self, event: &str, handler: F) -> EventId {
        let id = self.0.next_listener.fetch_add(1, Ordering::Relaxed);
        self.0.listeners.write().unwrap().push((id, event.to_string(), Arc::new(handler)));
        id
    }

    pub fn unlisten(&self, id: EventId) {
        self.0.listeners.write().unwrap().retain(|(i, _, _)| *i != id);
    }

    pub fn path(&self) -> PathResolver {
        PathResolver
    }

    pub fn asset_resolver(&self) -> AssetResolver {
        AssetResolver
    }

    /// Выполнить на главном потоке (из любого потока).
    pub fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<(), String> {
        self.0.tasks.lock().unwrap().push(Box::new(f));
        let thread = self.0.main_thread.load(Ordering::Acquire);
        if thread != 0 {
            unsafe { PostThreadMessageW(thread, WM_TASKS, WPARAM(0), LPARAM(0)) }.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Завершить цикл главного потока; `run` вернётся, процесс выйдет.
    pub fn exit(&self, code: i32) {
        let thread = self.0.main_thread.load(Ordering::Acquire);
        if thread == 0 {
            std::process::exit(code);
        }
        unsafe {
            let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(code as usize), LPARAM(0));
        }
    }

    /// Цикл сообщений главного потока: задачи `run_on_main_thread`, до `exit`.
    pub fn run(&self) -> i32 {
        let id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        self.0.main_thread.store(id, Ordering::Release);
        // задачи, поставленные до запуска цикла
        self.drain();
        let mut msg = MSG::default();
        unsafe {
            loop {
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == 0 || r.0 == -1 {
                    break;
                }
                if msg.hwnd.0.is_null() && msg.message == WM_TASKS {
                    self.drain();
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        msg.wParam.0 as i32
    }

    fn drain(&self) {
        let tasks = std::mem::take(&mut *self.0.tasks.lock().unwrap());
        for t in tasks {
            t();
        }
    }

    pub fn primary_monitor(&self) -> Result<Option<Monitor>, String> {
        Ok(monitor_of(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }))
    }

    /// Монитор под точкой (физические пиксели) или None.
    pub fn monitor_from_point(&self, x: f64, y: f64) -> Result<Option<Monitor>, String> {
        let h = unsafe { MonitorFromPoint(POINT { x: x as i32, y: y as i32 }, MONITOR_DEFAULTTONULL) };
        Ok(if h.is_invalid() { None } else { monitor_of(h) })
    }

    pub fn available_monitors(&self) -> Result<Vec<Monitor>, String> {
        unsafe extern "system" fn each(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::Win32::Foundation::BOOL {
            let list = &mut *(data.0 as *mut Vec<HMONITOR>);
            list.push(h);
            true.into()
        }
        let mut list: Vec<HMONITOR> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut _ as isize));
        }
        Ok(list.into_iter().filter_map(monitor_of).collect())
    }
}

impl Default for AppHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Точка в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalPosition<T> {
    pub x: T,
    pub y: T,
}

impl<T> PhysicalPosition<T> {
    pub fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

/// Размер в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalSize<T> {
    pub width: T,
    pub height: T,
}

impl<T> PhysicalSize<T> {
    pub fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

/// Размер в логических пикселях (DIP).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize<T> {
    pub width: T,
    pub height: T,
}

impl PhysicalSize<u32> {
    pub fn to_logical<T: From<f64>>(&self, scale: f64) -> LogicalSize<T> {
        LogicalSize { width: T::from(self.width as f64 / scale), height: T::from(self.height as f64 / scale) }
    }
}

/// Монитор: прямоугольник в физических пикселях и масштаб.
#[derive(Debug, Clone)]
pub struct Monitor {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    work: Rect,
    scale: f64,
}

/// Прямоугольник в физических пикселях.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub position: PhysicalPosition<i32>,
    pub size: PhysicalSize<u32>,
}

impl Monitor {
    pub fn position(&self) -> &PhysicalPosition<i32> {
        &self.position
    }
    pub fn size(&self) -> &PhysicalSize<u32> {
        &self.size
    }
    pub fn scale_factor(&self) -> f64 {
        self.scale
    }
    /// Рабочая область (без панели задач).
    pub fn work_area(&self) -> &Rect {
        &self.work
    }
}

fn monitor_of(h: HMONITOR) -> Option<Monitor> {
    if h.is_invalid() {
        return None;
    }
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        if !GetMonitorInfoW(h, &mut info).as_bool() {
            return None;
        }
    }
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    let r = info.rcMonitor;
    let w = info.rcWork;
    Some(Monitor {
        position: PhysicalPosition::new(r.left, r.top),
        size: PhysicalSize::new((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32),
        work: Rect {
            position: PhysicalPosition::new(w.left, w.top),
            size: PhysicalSize::new((w.right - w.left).max(0) as u32, (w.bottom - w.top).max(0) as u32),
        },
        scale: dx.max(96) as f64 / 96.0,
    })
}

/// Тяжёлая работа «в фоне» у прежних async-команд: теперь их зовут из рабочих потоков,
/// поэтому выполняется на месте.
pub fn blocking<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    Ok(f())
}

/// Папки приложения.
pub struct PathResolver;

/// Откуда брать ресурс (`resolve`).
pub enum BaseDirectory {
    Resource,
}

impl PathResolver {
    /// %APPDATA%\com.troid.reever — настройки, действия, индекс.
    pub fn app_config_dir(&self) -> Result<PathBuf, String> {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(IDENTIFIER)).ok_or_else(|| "APPDATA is not set".to_string())
    }

    pub fn app_data_dir(&self) -> Result<PathBuf, String> {
        self.app_config_dir()
    }

    /// %LOCALAPPDATA%\com.troid.reever.
    pub fn app_local_data_dir(&self) -> Result<PathBuf, String> {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(IDENTIFIER)).ok_or_else(|| "LOCALAPPDATA is not set".to_string())
    }

    /// Папка ресурсов: рядом с exe (установщик кладёт `resources\` туда же).
    pub fn resource_dir(&self) -> Result<PathBuf, String> {
        std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).ok_or_else(|| "no exe dir".to_string())
    }

    pub fn resolve(&self, rel: &str, base: BaseDirectory) -> Result<PathBuf, String> {
        match base {
            BaseDirectory::Resource => Ok(self.resource_dir()?.join(rel)),
        }
    }
}

/// Вшитые в exe ресурсы интерфейса (шрифты, кадры маскота, картинки рамки) — `assets/`,
/// таблицу собирает build.rs.
pub struct AssetResolver;

pub struct Asset {
    pub bytes: Vec<u8>,
}

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

impl AssetResolver {
    pub fn get(&self, path: String) -> Option<Asset> {
        let path = path.trim_start_matches('/');
        embedded::ASSETS.binary_search_by(|(p, _)| (*p).cmp(path)).ok().map(|i| Asset { bytes: embedded::ASSETS[i].1.to_vec() })
    }
}

/// Байты вшитого ресурса без копии.
pub fn asset(path: &str) -> Option<&'static [u8]> {
    let path = path.trim_start_matches('/');
    embedded::ASSETS.binary_search_by(|(p, _)| (*p).cmp(path)).ok().map(|i| embedded::ASSETS[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_reach_listeners_as_json_and_unlisten_stops_them() {
        let app = AppHandle::new();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let id = app.listen_any("x", move |e| g.lock().unwrap().push(e.payload().to_string()));
        app.emit("x", "hi").unwrap();
        app.emit("y", 1).unwrap();
        app.unlisten(id);
        app.emit("x", 2).unwrap();
        assert_eq!(*got.lock().unwrap(), vec!["\"hi\"".to_string()]);
    }

    #[test]
    fn state_is_per_type() {
        let app = AppHandle::new();
        assert!(app.try_state::<u32>().is_none());
        app.manage(7u32);
        assert!(!app.manage(8u32));
        assert_eq!(*app.state::<u32>(), 7);
    }

    #[test]
    fn assets_include_the_mascot_and_fonts() {
        assert!(asset("mascot/anim.json").is_some());
        assert!(asset("fonts/Handjet.ttf").is_some());
        assert!(asset("skin/frame_color.webp").is_some());
    }
}
