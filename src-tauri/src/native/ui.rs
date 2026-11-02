// Поток окон: цикл сообщений, задачи из других потоков, кадры по таймеру.
//
// Всё, что касается Direct2D/DirectComposition, живёт в этом потоке (объекты не Send):
// остальной код присылает сюда замыкания через UiThread::post/call. Цикл:
//   1) разобрать сообщения Windows (оконные процедуры складывают события в очередь);
//   2) выполнить присланные задачи;
//   3) разобрать события окон (размер, DPI, мышь);
//   4) если есть «грязные» или анимированные окна и подошёл срок кадра — нарисовать;
//   5) ждать: сообщения + таймер высокого разрешения до следующего кадра. Если рисовать нечего,
//      поток спит без таймера и не тратит CPU вовсе.
// Таймер высокого разрешения (CREATE_WAITABLE_TIMER_HIGH_RESOLUTION) нужен потому, что тайм-аут
// MsgWaitForMultipleObjects идёт шагами системного таймера (15,6 мс) и 60 кадров не дал бы.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::{ID2D1Factory1, ID2D1SolidColorBrush};
use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
use windows::Win32::System::Threading::{
    CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimer,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use super::device::{create_d2d_factory, Gfx};
use super::frame::FrameClock;
use super::image::Images;
use super::text::Text;
use super::window::{take_events, HitTest, NativeWindow, RenderResult, WinEventKind};
use super::{log, Job, WindowHandler, WindowId, WindowInfo, WindowSpec};

/// Самый длинный шаг анимации: после долгой паузы анимация не перескакивает вперёд рывком.
const MAX_ANIM_STEP: Duration = Duration::from_millis(100);

/// Состояние потока окон. Доступно только внутри задач, присланных в поток.
pub struct Ui {
    factory: ID2D1Factory1,
    gfx: Option<Gfx>,
    generation: u64,
    text: Text,
    images: Images,
    windows: Vec<NativeWindow>,
    next_id: u32,
    clock: FrameClock,
    brush: Option<(u64, ID2D1SolidColorBrush)>,
    jobs: Receiver<Job>,
    timer: HANDLE,
    last_advance: Option<Instant>,
    frames: u64,
    quit: bool,
}

impl Ui {
    pub(crate) fn new(jobs: Receiver<Job>) -> Result<Self> {
        let timer = unsafe {
            CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)?
        };
        let mut clock = FrameClock::new();
        clock.set_refresh_hz(refresh_hz());
        Ok(Self {
            factory: create_d2d_factory()?,
            gfx: None,
            generation: 0,
            text: Text::new()?,
            images: Images::new()?,
            windows: Vec::new(),
            next_id: 1,
            clock,
            brush: None,
            jobs,
            timer,
            last_advance: None,
            frames: 0,
            quit: false,
        })
    }

    // ---------- API для задач ----------

    /// Создать окно. Размер в DIP, позиция — экранные пиксели левого верхнего угла.
    pub fn create_window(&mut self, spec: WindowSpec, handler: Box<dyn WindowHandler>) -> Result<WindowId> {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        let mut w = NativeWindow::create(id, &spec, handler)?;
        let hwnd = w.hwnd.0 as isize;
        w.handler.attached(hwnd);
        self.windows.push(w);
        Ok(id)
    }

    pub fn close_window(&mut self, id: WindowId) {
        if let Some(i) = self.windows.iter().position(|w| w.id == id) {
            self.windows.remove(i).destroy();
        }
    }

    /// Окно нужно перерисовать (что-то поменялось в его состоянии).
    pub fn invalidate(&mut self, id: WindowId) {
        if let Some(w) = self.window_mut(id) {
            w.dirty = true;
        }
    }

    /// Обработчик окна нужного типа — чтобы поменять его состояние из задачи.
    pub fn handler_mut<H: WindowHandler>(&mut self, id: WindowId) -> Option<&mut H> {
        let w = self.windows.iter_mut().find(|w| w.id == id)?;
        let any: &mut dyn std::any::Any = w.handler.as_mut();
        any.downcast_mut::<H>()
    }

    pub fn show(&mut self, id: WindowId, visible: bool) {
        if let Some(w) = self.window_mut(id) {
            w.show(visible);
        }
    }

    /// Сдвинуть окно (экранные пиксели левого верхнего угла).
    pub fn set_position(&mut self, id: WindowId, x: i32, y: i32) {
        if let Some(w) = self.window(id) {
            w.set_position(x, y);
        }
    }

    pub fn set_size(&mut self, id: WindowId, width: f32, height: f32) {
        if let Some(w) = self.window(id) {
            w.set_size_dip(width, height);
        }
    }

    /// Проверка попадания указателя (DIP внутри окна). None — всё окно ловит мышь.
    /// Мимо — HTTRANSPARENT: клик уходит окну ниже, но только окнам этого же потока
    /// (ограничение Windows); сквозной клик в чужие окна — регион окна (этап 1).
    pub fn set_hit_test(&mut self, id: WindowId, hit: Option<HitTest>) {
        if let Some(w) = self.window(id) {
            w.set_hit_test(hit);
        }
    }

    pub fn info(&self, id: WindowId) -> Option<WindowInfo> {
        self.window(id).map(|w| WindowInfo {
            hwnd: w.hwnd.0 as isize,
            dpi: w.dpi,
            scale: w.scale(),
            size_px: w.size_px,
            first_frame: w.first_frame,
        })
    }

    /// Лимит кадров: 0 (как монитор) / 60 / 30.
    pub fn set_fps_limit(&mut self, limit: u32) {
        self.clock.set_limit(limit);
    }

    pub fn fps_limit(&self) -> u32 {
        self.clock.limit()
    }

    /// Остановить анимации на `dur` (окно таскают), как ReeverFps.pause в fps.js.
    pub fn pause_animations(&mut self, dur: Duration) {
        self.clock.pause(Instant::now(), dur);
    }

    pub fn images(&mut self) -> &mut Images {
        &mut self.images
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    /// Сколько кадров нарисовано с запуска потока (все окна за один проход — один кадр).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Устройство работает программно (WARP).
    pub fn software_rendering(&self) -> Option<bool> {
        self.gfx.as_ref().map(|g| g.software)
    }

    /// Создать графическое устройство и загрузить кодеки картинок заранее (иначе — в первом
    /// кадре первого окна).
    /// Создание D3D11 + Direct2D + DirectComposition стоит десятки миллисекунд; если позвать это
    /// при старте программы, окно маскота покажется без этой задержки.
    pub fn prewarm(&mut self) -> Result<()> {
        if self.gfx.is_none() {
            self.images.prewarm_codecs();
            self.generation += 1;
            self.gfx = Some(Gfx::new(&self.factory, self.generation)?);
        }
        Ok(())
    }

    /// Отпустить графическое устройство, если окон не осталось (D3D11, Direct2D, кэш картинок):
    /// поток окон без окон не держит память видеокарты и драйвера. Следующее окно создаст его заново.
    pub fn release_device(&mut self) {
        if self.windows.is_empty() {
            self.brush = None;
            self.images.clear_device();
            self.gfx = None;
        }
    }

    /// Проверка восстановления: пройти тот же путь, что при настоящей потере устройства.
    pub fn simulate_device_loss(&mut self) {
        self.recover_device("проверка");
    }

    /// Завершить поток окон (окна закрываются).
    pub fn quit(&mut self) {
        self.quit = true;
    }

    // ---------- цикл ----------

    pub(crate) fn run(mut self) {
        while !self.quit {
            self.pump_messages();
            self.run_jobs();
            self.process_events();
            if self.quit {
                break;
            }
            self.frame();
            self.wait();
        }
        for w in std::mem::take(&mut self.windows) {
            w.destroy();
        }
        // последние WM_NCDESTROY уже разобраны DestroyWindow; очередь событий больше не нужна
        let _ = take_events();
        unsafe {
            let _ = CloseHandle(self.timer);
        }
    }

    fn window(&self, id: WindowId) -> Option<&NativeWindow> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut NativeWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn pump_messages(&mut self) {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    self.quit = true;
                    return;
                }
                // сообщение-будильник задачи (без окна) разбирать не нужно — задачи берутся ниже
                if msg.hwnd.0.is_null() {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn run_jobs(&mut self) {
        while let Ok(job) = self.jobs.try_recv() {
            job(self);
            if self.quit {
                return;
            }
        }
    }

    fn process_events(&mut self) {
        for ev in take_events() {
            if let WinEventKind::DisplayChange = ev.kind {
                self.clock.set_refresh_hz(refresh_hz());
                if self.gfx.as_ref().is_some_and(|g| g.is_lost()) {
                    self.recover_device("смена дисплея");
                }
                continue;
            }
            let Some(i) = self.windows.iter().position(|w| w.hwnd.0 as isize == ev.hwnd) else { continue };
            let w = &mut self.windows[i];
            match ev.kind {
                WinEventKind::Resized(x, y) => w.on_resized(x, y),
                WinEventKind::Dpi(d) => w.on_dpi(d),
                WinEventKind::Mouse(m) => {
                    if w.handler.mouse(m) {
                        w.dirty = true;
                    }
                }
                WinEventKind::Paint => w.dirty = true,
                WinEventKind::Destroyed => {
                    // окно закрыла система (или его владелец) — забываем без второго DestroyWindow
                    self.windows.remove(i);
                }
                WinEventKind::DisplayChange => {}
            }
        }
    }

    /// Есть ли что рисовать: (нужен кадр сейчас, есть анимации на паузе).
    /// Ближайший срок `wake_at` среди видимых окон.
    fn next_wake(&self) -> Option<Instant> {
        self.windows.iter().filter(|w| w.visible).filter_map(|w| w.handler.wake_at()).min()
    }

    fn pending(&mut self, now: Instant) -> (bool, bool) {
        for w in self.windows.iter_mut().filter(|w| w.visible) {
            if w.handler.wake_at().is_some_and(|t| t <= now) {
                w.dirty = true;
            }
        }
        let paused = self.clock.is_paused(now);
        let mut need = false;
        let mut paused_anim = false;
        for w in self.windows.iter().filter(|w| w.visible) {
            let anim = w.handler.animating();
            if w.dirty || (anim && !paused) {
                need = true;
            }
            if anim && paused {
                paused_anim = true;
            }
        }
        (need, paused_anim)
    }

    fn frame(&mut self) {
        let now = Instant::now();
        let (need, _) = self.pending(now);
        if !need {
            self.clock.idle(now);
            self.last_advance = None;
            return;
        }
        if !self.clock.due(now) {
            return;
        }
        if let Err(e) = self.prewarm() {
            log(&format!("native: не удалось создать устройство: {e}"));
            // не крутиться вхолостую: следующая попытка через кадр
            self.clock.frame_done(now);
            return;
        }
        let paused = self.clock.is_paused(now);
        let dt = self
            .last_advance
            .map_or(self.clock.interval(), |t| now.saturating_duration_since(t))
            .min(MAX_ANIM_STEP);
        if paused {
            self.last_advance = None;
        } else {
            self.last_advance = Some(now);
        }
        let mut lost = false;
        {
            let gfx = self.gfx.as_ref().expect("gfx");
            let brush = match &self.brush {
                Some((g, b)) if *g == gfx.generation => b.clone(),
                _ => match unsafe { gfx.dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None) } {
                    Ok(b) => {
                        self.brush = Some((gfx.generation, b.clone()));
                        b
                    }
                    Err(e) => {
                        log(&format!("native: кисть: {e}"));
                        lost = true;
                        self.brush = None;
                        // устройство, скорее всего, потеряно — восстановление ниже
                        return self.after_frame(now, lost);
                    }
                },
            };
            for w in self.windows.iter_mut().filter(|w| w.visible) {
                let anim = w.handler.animating() && !paused;
                if !(w.dirty || anim) {
                    continue;
                }
                if anim {
                    w.handler.advance(dt);
                }
                match w.render(gfx, &mut self.images, &self.text, &brush, now) {
                    Ok(RenderResult::Ok) => {}
                    Ok(RenderResult::DeviceLost) => {
                        lost = true;
                        break;
                    }
                    Err(e) => {
                        log(&format!("native: кадр окна {}: {e}", w.id.0));
                        if gfx.is_lost() {
                            lost = true;
                            break;
                        }
                        w.dirty = false; // не повторять сломанный кадр бесконечно
                    }
                }
            }
        }
        self.after_frame(now, lost);
    }

    fn after_frame(&mut self, now: Instant, lost: bool) {
        if lost {
            self.recover_device("устройство потеряно");
        }
        self.frames += 1;
        self.clock.frame_done(now);
    }

    /// Пересоздать устройство и всё, что от него зависит; окна перерисуются со следующего кадра.
    fn recover_device(&mut self, why: &str) {
        log(&format!("native: пересоздание устройства ({why})"));
        for w in &mut self.windows {
            w.drop_surface();
        }
        self.brush = None;
        self.images.clear_device();
        self.gfx = None;
        self.generation += 1;
        match Gfx::new(&self.factory, self.generation) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => log(&format!("native: устройство не создано, повтор на следующем кадре: {e}")),
        }
    }

    fn wait(&mut self) {
        let now = Instant::now();
        let (need, paused_anim) = self.pending(now);
        let deadline = if need {
            match self.clock.next_due() {
                Some(d) if d > now => Some(d),
                _ => return, // кадр уже пора — не ждём
            }
        } else if paused_anim {
            self.clock.paused_until()
        } else {
            None
        };
        let deadline = match (deadline, self.next_wake().filter(|_| !need)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        unsafe {
            match deadline {
                Some(d) => {
                    // отрицательное значение — относительный срок в единицах по 100 нс
                    let due = -((d.saturating_duration_since(now).as_nanos() / 100).max(1) as i64);
                    let _ = SetWaitableTimer(self.timer, &due, 0, None, None, false);
                    MsgWaitForMultipleObjectsEx(Some(&[self.timer]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                }
                None => {
                    let _ = CancelWaitableTimer(self.timer);
                    MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                }
            }
        }
    }
}

/// Частота обновления экрана по данным DWM (для лимита «как монитор»).
fn refresh_hz() -> f64 {
    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    if unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.is_ok() && info.rateRefresh.uiDenominator != 0 {
        info.rateRefresh.uiNumerator as f64 / info.rateRefresh.uiDenominator as f64
    } else {
        60.0
    }
}
