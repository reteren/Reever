//! Проверка нативного главного окна без микрофона: `REEVER_MAIN_DEMO=1` (запускать с `--hidden`,
//! чтобы не открылось веб-окно). Reever запускается рядом с установленным (своё событие
//! единственного экземпляра), держит конфиги во временной папке, не слушает эфир и не
//! регистрирует горячие клавиши. Окно не забирает фокус (человек за компьютером может печатать),
//! снимки — только своего окна вне экрана (`REEVER_MAIN_SHOTS=<папка>`), экран не захватывается.
//! Сценарий — те же события, что шлёт распознавание: состояния, уровень, бегущая строка,
//! «услышал», журнал, статус; плюс вкладки «Лог» и «Настройки». Шаги, время первого кадра,
//! память и процессор печатаются в stderr (`[main-demo] …`). `REEVER_MAIN_PROFILE=1` — ещё и время
//! частей кадра (линза, статичный слой, рой) в среднем за 300 кадров (`[main-profile] …`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::AppHandle;

use crate::events;

pub fn active() -> bool {
    std::env::var_os("REEVER_MAIN_DEMO").is_some()
}

fn step(started: Instant, name: &str) {
    eprintln!("[main-demo] {:>6} ms {name}", started.elapsed().as_millis());
}

fn shoot(app: &AppHandle, name: &str) {
    let Some(dir) = std::env::var_os("REEVER_MAIN_SHOTS").map(std::path::PathBuf::from) else { return };
    let Some((w, h, rgba)) = crate::main_native::snapshot(app) else {
        eprintln!("[main-demo] no shot {name}");
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
    eprintln!("[main-demo] shot {name}");
}

/// Видимых верхних окон этого процесса.
fn visible_windows() -> usize {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() && IsWindowVisible(hwnd).as_bool() {
            *(data.0 as *mut usize) += 1;
        }
        true.into()
    }
    let mut n = 0usize;
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut n as *mut usize as isize));
    }
    n
}

fn memory(started: Instant, what: &str) {
    let (ws, private) = crate::native::stats::memory();
    eprintln!(
        "[main-demo] {:>6} ms memory {what}: working set {:.1} MB, private {:.1} MB",
        started.elapsed().as_millis(),
        ws as f64 / 1048576.0,
        private as f64 / 1048576.0
    );
}

pub fn start(app: &AppHandle) {
    // вид окна — только в памяти, конфиг во временной папке и так свой
    if let Some(config) = app.try_state::<crate::config::LiveConfig>() {
        let mut fresh = (*config.get()).clone();
        // REEVER_MAIN_DEMO_LITE=1 — лёгкий вид (lite.css)
        fresh.settings.lite_ui = std::env::var_os("REEVER_MAIN_DEMO_LITE").is_some();
        // REEVER_MAIN_DEMO_NO_MONITOR=1 — без мониторчика лёгкого вида (замер его цены)
        fresh.settings.lite_monitor = std::env::var_os("REEVER_MAIN_DEMO_NO_MONITOR").is_none();
        // REEVER_MAIN_DEMO_LOCALE=ar — язык интерфейса (арабский: зеркальная раскладка)
        if let Ok(code) = std::env::var("REEVER_MAIN_DEMO_LOCALE") {
            let resolved = crate::uilang::resolve(&code);
            if let Ok(mut current) = crate::uilang::CURRENT.lock() {
                *current = resolved;
            }
            fresh.settings.ui_language = code;
        }
        // REEVER_MAIN_DEMO_FPS=60|30 — «Плавность» (по умолчанию без лимита, частота монитора)
        if let Some(fps) = std::env::var("REEVER_MAIN_DEMO_FPS").ok().and_then(|v| v.parse().ok()) {
            fresh.settings.fps_limit = fps;
        }
        config.set(Arc::new(fresh));
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        // фон программы без окна — чтобы отличить цену окна от прочей работы (индекс и т.п.)
        std::thread::sleep(Duration::from_secs(2));
        memory(started, "before window");
        let cpu = crate::native::stats::CpuMeter::start();
        std::thread::sleep(Duration::from_secs(3));
        eprintln!("[main-demo] cpu without window 3 s: {:?}", cpu.finish());
        step(started, "show");
        let before = visible_windows();
        // двойной щелчок по значку в трее — два show подряд: окно должно быть одно
        crate::main_native::show(&app);
        crate::main_native::show(&app);
        for _ in 0..400 {
            if let Some(ms) = crate::main_native::first_frame(&app) {
                eprintln!("[main-demo] first frame {} ms after the window was created", ms);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(120));
        eprintln!("[main-demo] windows added by a double show: {}", visible_windows() - before);
        shoot(&app, "power-on");
        memory(started, "power-on");
        step(started, "window shown");
        // REEVER_MAIN_DEMO_SOAK=<с> — столько «слушаю» с анимацией, память каждые 5 с
        // (растёт ли с каждым кадром или выходит на полку)
        if let Some(soak) = std::env::var("REEVER_MAIN_DEMO_SOAK").ok().and_then(|v| v.parse::<u64>().ok()) {
            let nostate = std::env::var_os("REEVER_MAIN_DEMO_SOAK_NOSTATE").is_some();
            if !nostate {
                events::set_state(&app, "listening");
            }
            let t = Instant::now();
            let mut next = 0u64;
            while t.elapsed() < Duration::from_secs(soak) {
                let s = t.elapsed().as_secs_f32();
                let _ = app.emit("reever://level", 0.02 + 0.08 * (s * 7.0).sin().abs());
                if t.elapsed().as_secs() >= next {
                    memory(started, &format!("soak {next} s, frames {:?}", crate::main_native::frames(&app)));
                    next += 5;
                }
                std::thread::sleep(Duration::from_millis(33));
            }
            events::set_state(&app, "idle");
            std::thread::sleep(Duration::from_secs(3));
            memory(started, "soak idle 3 s");
        }
        let _ = app.emit("reever://status", serde_json::json!({ "asr": "ready", "model": "parakeet-v3" }));
        events::ui_log(&app, "[main-demo] журнал: первая строка");
        events::ui_log(&app, "[main-demo] a second line that is long enough to wrap across the log screen of the native main window");
        std::thread::sleep(Duration::from_millis(1500));
        shoot(&app, "home-idle");
        memory(started, "home-idle");
        memory(started, "idle, window open");
        let images0 = crate::main_native::image_stats(&app);
        let frames0 = crate::main_native::frames(&app);
        let cpu = crate::native::stats::CpuMeter::start();
        std::thread::sleep(Duration::from_secs(3));
        eprintln!("[main-demo] cpu idle 3 s: {:?}", cpu.finish());
        eprintln!(
            "[main-demo] idle 3 s: frames {:?}, images (count, bytes, decodes) {:?} -> {:?}",
            crate::main_native::frames(&app).zip(frames0).map(|(a, b)| a - b),
            images0,
            crate::main_native::image_stats(&app)
        );
        // кадры осьминога в сфере декодируются при первом проходе (~4 с) — покой после прогрева
        std::thread::sleep(Duration::from_secs(2));
        let images0 = crate::main_native::image_stats(&app);
        let cpu = crate::native::stats::CpuMeter::start();
        std::thread::sleep(Duration::from_secs(3));
        eprintln!("[main-demo] cpu idle after warm-up 3 s: {:?}", cpu.finish());
        eprintln!("[main-demo] images {:?} -> {:?}", images0, crate::main_native::image_stats(&app));

        // веб-окно маскота при загрузке шлёт свою «Плавность» (skin/fps.js) — лимит проверки здесь
        if let Some(fps) = std::env::var("REEVER_MAIN_DEMO_FPS").ok().and_then(|v| v.parse().ok()) {
            crate::mascot_native::set_fps_limit(fps);
        }
        step(started, "listening");
        events::set_state(&app, "listening");
        let _ = app.emit("reever://partial", "открой блокнот");
        let frames0 = crate::main_native::frames(&app);
        let cpu = crate::native::stats::CpuMeter::start();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            let s = t.elapsed().as_secs_f32();
            let rms = 0.02 + 0.08 * (s * 7.0).sin().abs();
            let _ = app.emit("reever://level", rms);
            std::thread::sleep(Duration::from_millis(33));
        }
        shoot(&app, "home-listening");
        memory(started, "home-listening");
        eprintln!("[main-demo] cpu listening 3 s: {:?}", cpu.finish());
        eprintln!(
            "[main-demo] listening 3 s: frames {:?}, fps limit {:?}",
            crate::main_native::frames(&app).zip(frames0).map(|(a, b)| a - b),
            crate::mascot_native::ui(&app).and_then(|ui| ui.call(|ui| ui.fps_limit()))
        );
        let _ = app.emit("reever://heard", "открой блокнот");
        events::set_state(&app, "busy");
        std::thread::sleep(Duration::from_millis(900));
        shoot(&app, "home-heard");
        memory(started, "home-heard");
        events::set_state(&app, "idle");
        let _ = app.emit("reever://partial", "");
        std::thread::sleep(Duration::from_millis(1200));

        step(started, "logs tab");
        crate::main_native::send(crate::main_native::Msg::Tab("logs".into()));
        std::thread::sleep(Duration::from_millis(500));
        shoot(&app, "logs");
        memory(started, "logs");
        step(started, "settings tab");
        crate::main_native::send(crate::main_native::Msg::Tab("settings".into()));
        std::thread::sleep(Duration::from_millis(500));
        shoot(&app, "settings");
        memory(started, "settings");
        for (i, d) in [900.0f32, 650.0, 650.0, 300.0, 5000.0].iter().enumerate() {
            crate::main_native::send(crate::main_native::Msg::ScrollSettings(*d));
            std::thread::sleep(Duration::from_millis(300));
            shoot(&app, &format!("settings-{}", i + 1));
        }
        // вкладка «Текст»: пустая и с распознанным файлом (без моделей — готовый итог)
        crate::main_native::send(crate::main_native::Msg::Tab("text".into()));
        std::thread::sleep(Duration::from_millis(500));
        shoot(&app, "text");
        memory(started, "text");
        crate::transcribe::publish(&crate::transcribe::TextJob {
            kind: crate::transcribe::JobKind::File,
            name: "lecture.m4a".into(),
            running: true,
            progress: Some(0.42),
            text: "Сегодня поговорим о том, как устроен звук. Сначала - немного истории.".into(),
            ..Default::default()
        });
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "text-running");
        memory(started, "text-running");
        crate::transcribe::publish(&crate::transcribe::TextJob {
            kind: crate::transcribe::JobKind::Listen,
            text: "Welcome back to the channel. Today we are testing speech to text.\nВторая часть - по-русски.".into(),
            end: Some(crate::transcribe::JobEnd::Silence),
            copied: true,
            ..Default::default()
        });
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "text-done");
        memory(started, "text-done");
        // вкладка «Индекс»: пустая и со списком (индекс во временной папке выключен — готовый ответ;
        // номер 0 — ещё ничего не набирали)
        crate::main_native::send(crate::main_native::Msg::Tab("index".into()));
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "index");
        let item = |name: &str, path: &str, folder: bool| crate::find::FindItem { name: name.into(), path: path.into(), folder, frequent: false, score: 0.0 };
        let items = vec![
            item("Downloads", r"C:\Users\user\Downloads", true),
            item("report-2026.docx", r"C:\Users\user\Documents\Work\Very long folder name to check clipping\report-2026.docx", false),
            item("Отчёт за октябрь.xlsx", r"D:\Документы\Отчёт за октябрь.xlsx", false),
        ];
        crate::main_native::send(crate::main_native::Msg::IndexResults(0, Ok(items)));
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "index-results");
        crate::main_native::send(crate::main_native::Msg::Tab("actions".into()));
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "actions");
        memory(started, "actions");
        crate::main_native::send(crate::main_native::Msg::SelectAction(String::new()));
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "actions-edit");
        memory(started, "actions-edit");
        for which in ["calibration", "voice", "apps", "scan"] {
            crate::main_native::send(crate::main_native::Msg::TestOverlay(which.into()));
            std::thread::sleep(Duration::from_millis(match which {
                "apps" => 1500,
                "scan" => 8000,
                _ => 400,
            }));
            shoot(&app, &format!("overlay-{which}"));
        }
        crate::main_native::send(crate::main_native::Msg::TestOverlay("close".into()));
        // клавиатура: Tab по окну (без настоящего ввода — окно фокус не берёт)
        let key = |k: u32, shift: bool| crate::main_native::send(crate::main_native::Msg::Key(k, shift));
        crate::main_native::send(crate::main_native::Msg::Tab("home".into()));
        std::thread::sleep(Duration::from_millis(500));
        // Tab: вкладки рельса (с текущей), «Пауза», «Изменить», строки
        for _ in 0..6 {
            key(0x09, false);
        }
        std::thread::sleep(Duration::from_millis(300));
        shoot(&app, "kbd-home");
        memory(started, "kbd-home");
        key(0x09, false); // первая строка «Что можно сказать»
        key(0x28, false); // ↓ — вторая
        key(0x0D, false); // Enter — раскрыть её редактор
        key(0x09, false); // в редактор: флажок «Включено»
        key(0x09, false); // поле подписи
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "kbd-editor");
        memory(started, "kbd-editor");
        key(0x1B, false);
        crate::main_native::send(crate::main_native::Msg::GrammarDone(None));
        crate::main_native::send(crate::main_native::Msg::Tab("settings".into()));
        std::thread::sleep(Duration::from_millis(500));
        for _ in 0..6 {
            key(0x09, false);
        }
        std::thread::sleep(Duration::from_millis(300));
        shoot(&app, "kbd-settings");
        memory(started, "kbd-settings");
        crate::main_native::send(crate::main_native::Msg::Tab("home".into()));
        // компактный режим: полоска, бегущая строка, обратно
        crate::main_native::shade_for_test(&app, true);
        let _ = app.emit("reever://partial", "открой папку загрузки и найди там файл отчёта");
        std::thread::sleep(Duration::from_millis(700));
        shoot(&app, "shade");
        memory(started, "shade");
        let _ = app.emit("reever://partial", "");
        crate::main_native::shade_for_test(&app, false);
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "unshaded");
        memory(started, "unshaded");
        // редактор «Что можно сказать»: общий (wake-слова) и под строкой диктовки
        crate::main_native::send(crate::main_native::Msg::EditGrammar("general".into()));
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "grammar-general");
        memory(started, "grammar-general");
        crate::main_native::send(crate::main_native::Msg::EditGrammar("dictate".into()));
        std::thread::sleep(Duration::from_millis(400));
        shoot(&app, "grammar-dictate");
        memory(started, "grammar-dictate");
        crate::main_native::send(crate::main_native::Msg::GrammarDone(None));
        step(started, "paused");
        events::set_state(&app, "paused");
        std::thread::sleep(Duration::from_millis(1200));
        shoot(&app, "home-paused");
        memory(started, "home-paused");
        events::set_state(&app, "idle");
        std::thread::sleep(Duration::from_millis(800));
        memory(started, "end, window open");
        step(started, "to tray");
        crate::main_native::destroy(&app);
        std::thread::sleep(Duration::from_millis(1500));
        memory(started, "window destroyed");
        step(started, "done");
    });
}
