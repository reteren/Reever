//! Проверка окон выбора и подтверждения без микрофона: `REEVER_DIALOG_DEMO=1`. Reever
//! запускается рядом с установленным (своё событие единственного экземпляра), не слушает эфир,
//! не регистрирует горячие клавиши и держит конфиги во временной папке, а сам показывает окно
//! выбора с примером списка, длинный список и вопрос «Выключить компьютер?». Шаги и время
//! показа окна печатаются в stderr (`[dialog-demo] …`). `REEVER_DIALOG_DEMO_WARM=1` — первое окно
//! после прогрева в начале команды (как при «ривер найди …»), `REEVER_DIALOG_DEMO_LITE=1` —
//! лёгкий вид (в памяти, без записи в settings.json).

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::app::AppHandle;

use crate::actions::SharedState;
use crate::find::{FindItem, FindScope};

pub fn active() -> bool {
    std::env::var_os("REEVER_DIALOG_DEMO").is_some()
}

/// Когда попросили показать окно (для замера «до показа»).
static ASKED: Mutex<Option<(&'static str, Instant)>> = Mutex::new(None);

/// Окно попросили показать — засечь время (только в режиме проверки).
pub fn asked(what: &'static str) {
    if active() {
        if let Ok(mut a) = ASKED.lock() {
            *a = Some((what, Instant::now()));
        }
    }
}

/// Окно показалось с содержимым — напечатать, сколько прошло с просьбы.
pub fn shown(what: &str) {
    if !active() {
        return;
    }
    if let Some((w, at)) = ASKED.lock().ok().and_then(|mut a| a.take()) {
        if w == what {
            eprintln!("[dialog-demo] shown {what} in {} ms", at.elapsed().as_millis());
        }
    }
}

fn step(name: &str, started: Instant) {
    eprintln!("[dialog-demo] {:>6} ms {name}", started.elapsed().as_millis());
}

fn item(name: &str, path: &str, folder: bool, frequent: bool) -> FindItem {
    FindItem { name: name.into(), path: path.into(), folder, frequent, score: 0.0 }
}

/// Тот же пример, что у picker.html?shot.
fn sample() -> Vec<FindItem> {
    vec![
        item("-PROJECT.GST-", "C:\\-PROJECT.GST-", true, true),
        item("ProjectGST", "C:\\OProjectodoGST\\ProjectGST", true, false),
        item("ProjectGST", "C:\\Memory_Vault\\PROJECTS\\ProjectGST", true, false),
        item("PROJECT GST.md", "C:\\-PROJECT.GST-\\obsidian\\PROJECT GST.md", false, false),
        item("ProjectGST_Data", "C:\\OProjectodoGST\\ProjectGST\\bt\\ProjectGST_Data", true, false),
        item("OProjectodoGST", "C:\\OProjectodoGST", true, false),
        item("ProjectGST_BurstDebugInformation_DoNotShip", "C:\\OProjectodoGST\\ProjectGST\\bt\\ProjectGST_BurstDebugInformation_DoNotShip", true, false),
    ]
}

fn long() -> Vec<FindItem> {
    (1..=30)
        .map(|i| item(&format!("Отчёт {i:02}.docx"), &format!("C:\\Users\\demo\\Documents\\Отчёты\\2026\\Отчёт {i:02}.docx"), false, i <= 3))
        .collect()
}

/// Сохранить снимок окна `what` («picker» / «confirm») в REEVER_DIALOG_SHOTS/<name>.png.
fn shoot(app: &AppHandle, what: &str, name: &str) {
    let Some(dir) = std::env::var_os("REEVER_DIALOG_SHOTS").map(std::path::PathBuf::from) else { return };
    let shot = match what {
        "picker" => crate::dialogs_native::picker::snapshot(app),
        _ => crate::dialogs_native::confirm::snapshot(app),
    };
    let Some((w, h, rgba)) = shot else {
        eprintln!("[dialog-demo] no shot {name}");
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) = std::fs::File::create(dir.join(format!("{name}.png"))) else { return };
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut wr) = enc.write_header() {
        let _ = wr.write_image_data(&rgba);
    }
    eprintln!("[dialog-demo] shot {name}");
}

/// Нативное окно: снимок при 144 DPI (WM_DPICHANGED) и обратно.
fn shoot_dpi(app: &AppHandle, what: &str, name: &str) {
    let set = |dpi| match what {
        "picker" => crate::dialogs_native::picker::fake_dpi(dpi),
        _ => crate::dialogs_native::confirm::fake_dpi(dpi),
    };
    set(144);
    std::thread::sleep(Duration::from_millis(600));
    shoot(app, what, name);
    set(96);
    std::thread::sleep(Duration::from_millis(300));
}

pub fn start(app: &AppHandle) {
    // REEVER_DIALOG_DEMO_LITE=1 — лёгкий вид (только в памяти, settings.json проверки не меняется)
    if let Some(config) = app.try_state::<crate::config::LiveConfig>() {
        let mut fresh = (*config.get()).clone();
        if std::env::var_os("REEVER_DIALOG_DEMO_LITE").is_some() {
            fresh.settings.lite_ui = true;
        }
        config.set(Arc::new(fresh));
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let shared = app.state::<Arc<SharedState>>().inner().clone();
        // REEVER_DIALOG_DEMO_WARM=1: команда начинается как настоящая — состояние «слушаю»
        // (events::set_state прогревает графическое устройство), окно — через 1 с, как после
        // распознавания. Без переменной первое окно открывается холодным.
        if std::env::var_os("REEVER_DIALOG_DEMO_WARM").is_some() {
            std::thread::sleep(Duration::from_secs(2));
            step("listening (warm-up)", started);
            crate::events::set_state(&app, "listening");
            std::thread::sleep(Duration::from_secs(1));
            crate::events::set_state(&app, "idle");
        } else {
            std::thread::sleep(Duration::from_secs(3));
        }
        step("picker", started);
        asked("picker");
        crate::picker::show(&app, &shared, "project vsta", "project vsta", FindScope::Any, &sample());
        std::thread::sleep(Duration::from_millis(1500));
        shoot(&app, "picker", "picker");
        shoot_dpi(&app, "picker", "picker_144");
        std::thread::sleep(Duration::from_secs(6));
        step("picker-long", started);
        asked("picker");
        crate::picker::show(&app, &shared, "отчёт", "отчёт", FindScope::File, &long());
        std::thread::sleep(Duration::from_millis(1500));
        shoot(&app, "picker", "picker_long");
        std::thread::sleep(Duration::from_secs(5));
        crate::picker::hide(&app, &shared);
        std::thread::sleep(Duration::from_secs(2));
        step("confirm", started);
        asked("confirm");
        crate::confirm::show(&app, &crate::dispatch::PendingAction::System("shutdown".into()), 8);
        // мимо фазы мигания вопроса (0,3–0,9 с от показа — приглушён)
        std::thread::sleep(Duration::from_millis(1250));
        shoot(&app, "confirm", "confirm");
        shoot_dpi(&app, "confirm", "confirm_144");
        std::thread::sleep(Duration::from_secs(7));
        crate::confirm::hide(&app);
        std::thread::sleep(Duration::from_secs(1));
        step("done", started);
    });
}
