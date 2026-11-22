//! Нативное окно маскота (этап 1 docs/native-ui-roadmap.md): тот же осьминог, что и
//! прежний frontend/mascot.html, но на ядре `native/` — без WebView2. Всю логику показа
//! (режимы, поколения, телепорт, уходы) ведёт
//! mascot.rs, отсюда — только «транспорт»: создать/убрать окно, передать ему событие, узнать,
//! над непрозрачным ли пикселем курсор.
//!
//! Кадры и anim.json берутся из ресурсов, вшитых в exe (assets/mascot, build.rs), по запросу при каждом
//! декодировании: в памяти держится только кэш уменьшенных под масштаб кадров.

pub(crate) mod lens;
mod view;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::AppHandle;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use crate::native::{self, Loader, UiThread, WindowId, WindowSpec};
pub use view::Msg;
pub(crate) use view::MascotView;
use view::{Assets, HitFrame, ALPHA_HIT};

/// Поток окон: создаётся при первом нативном маскоте и живёт до выхода (без окон он спит и
/// устройство отпускает — см. `destroy`).
static UI: Mutex<Option<Arc<UiThread>>> = Mutex::new(None);
/// Окно маскота: (id в потоке окон, hwnd).
static WIN: Mutex<Option<(WindowId, isize)>> = Mutex::new(None);
/// Что нарисовано сейчас — для проверки попадания из потока слежения за курсором.
static HIT: Mutex<Option<HitFrame>> = Mutex::new(None);
/// Курсор над спрайтом: рисовальщику пора готовить альфу текущего кадра.
static MASK_WANTED: AtomicBool = AtomicBool::new(false);

fn log(app: &AppHandle, text: String) {
    crate::events::ui_log(app, text);
}

/// Поток окон — общий для маскота и пузыря (bubble_native).
pub(crate) fn ui(app: &AppHandle) -> Option<Arc<UiThread>> {
    let mut slot = UI.lock().ok()?;
    if let Some(ui) = slot.as_ref().filter(|u| u.is_running()) {
        return Some(ui.clone());
    }
    match UiThread::spawn() {
        Ok(ui) => {
            // «Плавность» — сразу, кто бы ни открыл первое окно (маскот, главное, выбор)
            if let Some(config) = app.try_state::<crate::config::LiveConfig>() {
                let limit = config.get().settings.fps_limit;
                ui.post(move |ui| ui.set_fps_limit(limit));
            }
            let ui = Arc::new(ui);
            *slot = Some(ui.clone());
            Some(ui)
        }
        Err(e) => {
            log(app, format!("mascot: native window thread failed: {e}"));
            None
        }
    }
}

/// Запустить поток окон, создать устройство и скомпилировать шейдер линзы заранее (фоном
/// при старте программы): тогда маскот появляется без задержки на всё это.
pub fn prewarm(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let Some(ui) = ui(&app) else { return };
        let ok = ui.call(|ui| ui.prewarm().is_ok()).unwrap_or(false);
        let lens = lens::warm_up();
        log(&app, format!(
            "mascot: native core ready in {} ms (device {}, lens shader {})",
            started.elapsed().as_millis(),
            if ok { "ok" } else { "failed" },
            if lens { "ok" } else { "failed" }
        ));
    });
}

/// Лимит кадров из настройки «Плавность» (0 / 60 / 30).
pub fn set_fps_limit(limit: u32) {
    if let Some(ui) = UI.lock().ok().and_then(|u| u.clone()) {
        ui.post(move |ui| ui.set_fps_limit(limit));
    }
}

fn asset_loader(app: &AppHandle, path: String) -> Loader {
    let app = app.clone();
    Arc::new(move || app.asset_resolver().get(path.clone()).map(|a| a.bytes))
}

#[derive(serde::Deserialize)]
struct AnimJson {
    eye: EyeJson,
    portal: PortalJson,
}

#[derive(serde::Deserialize)]
struct EyeJson {
    grab: Vec<[f32; 7]>,
    idle: Vec<[f32; 7]>,
    portal_in: Vec<[f32; 7]>,
}

#[derive(serde::Deserialize)]
struct PortalJson {
    portal_in: Vec<[f32; 3]>,
    portal_out: Vec<[f32; 3]>,
}

/// Кадры и разметка маскота (anim.json) — для окна маскота и для осьминога в сфере
/// главного окна. Звать в потоке окон.
pub(crate) fn load_assets(app: &AppHandle, images: &mut native::Images) -> Option<Assets> {
    let anim = app
        .asset_resolver()
        .get("mascot/anim.json".into())
        .and_then(|a| serde_json::from_slice::<AnimJson>(&a.bytes).ok())?;
    let mut set = |name: &str, count: usize| -> Vec<native::ImageId> {
        (1..=count).map(|i| images.add_loader(asset_loader(app, format!("mascot/{name}_{i:04}.webp")))).collect()
    };
    Some(Assets {
        grab: set("grab", 84),
        idle: set("idle", 96),
        portal_in: set("portal_in", 58),
        portal_gate: set("portal_gate", 58),
        eye_grab: anim.eye.grab,
        eye_idle: anim.eye.idle,
        eye_portal_in: anim.eye.portal_in,
        portal_at_in: anim.portal.portal_in,
        portal_at_out: anim.portal.portal_out,
    })
}

/// Создать окно (блокирует до готовности, ~35 мс после prewarm). `x`, `y` — экранные пиксели
/// левого верхнего угла, размер — DIP. Возвращает hwnd.
pub fn create(app: &AppHandle, x: i32, y: i32, width: f64, height: f64, fps_limit: u32) -> Option<isize> {
    let ui = ui(app)?;
    let anim = app
        .asset_resolver()
        .get("mascot/anim.json".into())
        .and_then(|a| serde_json::from_slice::<AnimJson>(&a.bytes).ok());
    let Some(anim) = anim else {
        log(app, "mascot: mascot/anim.json is missing from the embedded assets".into());
        return None;
    };
    let state = crate::events::current_state();
    let app2 = app.clone();
    let made = ui.call(move |ui| {
        ui.set_fps_limit(fps_limit);
        let mut set = |name: &str, count: usize| -> Vec<native::ImageId> {
            (1..=count).map(|i| ui.images().add_loader(asset_loader(&app2, format!("mascot/{name}_{i:04}.webp")))).collect()
        };
        let assets = Assets {
            grab: set("grab", 84),
            idle: set("idle", 96),
            portal_in: set("portal_in", 58),
            portal_gate: set("portal_gate", 58),
            eye_grab: anim.eye.grab,
            eye_idle: anim.eye.idle,
            eye_portal_in: anim.eye.portal_in,
            portal_at_in: anim.portal.portal_in,
            portal_at_out: anim.portal.portal_out,
        };
        // кадры портала при 100 % — 328×408, весь набор маскота при 150 % — около 100 МБ;
        // кэш держит покой и хват, портал при нехватке декодируется заново (раз в показ)
        ui.images().set_budget(64 * 1024 * 1024);
        let spec = WindowSpec {
            title: "Reever".into(),
            x,
            y,
            width: width as f32,
            height: height as f32,
            topmost: true,
            no_activate: true,
            visible: true,
            taskbar: false,
        };
        let id = ui.create_window(spec, Box::new(MascotView::new(assets, &state)))?;
        let hwnd = ui.info(id).map(|i| i.hwnd).unwrap_or(0);
        Ok::<_, windows::core::Error>((id, hwnd))
    })?;
    match made {
        Ok((id, hwnd)) => {
            // мышь — сквозь окно, пока курсор не над осьминогом (mascot.rs, watch_cursor)
            native::set_click_through(hwnd, true);
            if let Ok(mut w) = WIN.lock() {
                *w = Some((id, hwnd));
            }
            Some(hwnd)
        }
        Err(e) => {
            log(app, format!("mascot: native window failed: {e}"));
            None
        }
    }
}

/// Убрать окно; без окон поток окон отпускает графическое устройство.
pub fn destroy() {
    let Some((id, _)) = WIN.lock().ok().and_then(|mut w| w.take()) else { return };
    publish_hit(None);
    if let Some(ui) = UI.lock().ok().and_then(|u| u.clone()) {
        ui.post(move |ui| {
            ui.close_window(id);
            ui.release_device();
        });
    }
}

pub fn exists() -> bool {
    WIN.lock().map(|w| w.is_some()).unwrap_or(false)
}

pub fn hwnd() -> Option<isize> {
    WIN.lock().ok().and_then(|w| w.map(|(_, h)| h))
}

/// Передать окну событие (как emit веб-окну).
pub fn send(msg: Msg) {
    let Some((id, _)) = WIN.lock().ok().and_then(|w| *w) else { return };
    if let Some(ui) = UI.lock().ok().and_then(|u| u.clone()) {
        ui.post(move |ui| {
            if let Some(view) = ui.handler_mut::<MascotView>(id) {
                view.on_msg(msg);
            }
            ui.invalidate(id);
        });
    }
}

/// Окно на экране: (x, y, ширина, высота) в физических пикселях.
pub fn rect() -> Option<(i32, i32, u32, u32)> {
    let hwnd = hwnd()?;
    let mut r = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut r) }.ok()?;
    Some((r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32))
}

pub(crate) fn publish_hit(hit: Option<HitFrame>) {
    if let Ok(mut h) = HIT.lock() {
        *h = hit;
    }
}

pub(crate) fn mask_wanted() -> bool {
    MASK_WANTED.load(Ordering::Relaxed)
}

/// Курсор (экранные пиксели) над непрозрачным пикселем осьминога в покое или в руке.
pub fn over_opaque(cursor: POINT) -> bool {
    let Some((x, y, _, _)) = rect() else { return false };
    let Some(hit) = HIT.lock().ok().and_then(|h| h.clone()) else {
        MASK_WANTED.store(false, Ordering::Relaxed);
        return false;
    };
    let [rx, ry, rw, rh] = hit.rect_px;
    let fx = ((cursor.x - x) as f32 - rx) / rw;
    let fy = ((cursor.y - y) as f32 - ry) / rh;
    let inside = (0.0..1.0).contains(&fx) && (0.0..1.0).contains(&fy);
    MASK_WANTED.store(inside, Ordering::Relaxed);
    inside && hit.mask.is_some_and(|m| m.at(fx, fy) > ALPHA_HIT)
}

/// Снимок окна вне экрана (RGBA) — для проверок и анимаций README без захвата экрана.
pub fn snapshot(app: &AppHandle) -> Option<(u32, u32, Vec<u8>)> {
    let (id, _) = WIN.lock().ok().and_then(|w| *w)?;
    let ui = ui(app)?;
    ui.call(move |ui| ui.snapshot(id)).flatten()
}
