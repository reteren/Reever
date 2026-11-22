//! Анимации для README: `REEVER_README_DEMO=<папка>`. Reever запускается как проверка главного
//! окна (main_demo.rs: рядом с установленным, конфиги во временной папке, без микрофона и
//! горячих клавиш), переключает интерфейс на английский и прогоняет сценарии — те же события,
//! что шлёт распознавание. Кадры окон снимаются вне экрана (RGBA с прозрачностью, экран не
//! захватывается) в `<папка>/<сценарий>/<окно>/NNNN.png`; в `frames.txt` рядом — время кадра
//! от начала сценария (мс) и место окна на экране. GIF из кадров собирает
//! `tools/readme_gifs.py`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::app::AppHandle;
use crate::events;
use crate::find::{FindItem, FindScope};

pub fn active() -> bool {
    std::env::var_os("REEVER_README_DEMO").is_some()
}

fn out_dir() -> PathBuf {
    std::env::var_os("REEVER_README_DEMO").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("readme-demo"))
}

/// Кадров в секунду при записи; GIF потом прореживается.
const FPS: u64 = 25;

/// Кадр окна: размер, RGBA и левый верхний угол на экране.
type Frame = (u32, u32, Vec<u8>, i32, i32);

fn window_pos(hwnd: Option<isize>) -> Option<(i32, i32)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindowVisible};
    let hwnd = HWND(hwnd? as *mut _);
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return None;
    }
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some((r.left, r.top))
}

fn grab_main(app: &AppHandle) -> Option<Frame> {
    let (w, h, rgba) = crate::main_native::snapshot(app)?;
    Some((w, h, rgba, 0, 0))
}

fn grab_mascot(app: &AppHandle) -> Option<Frame> {
    let (x, y) = window_pos(crate::mascot_native::hwnd())?;
    let (w, h, rgba) = crate::mascot_native::snapshot(app)?;
    Some((w, h, rgba, x, y))
}

fn grab_bubble(app: &AppHandle) -> Option<Frame> {
    let (x, y) = window_pos(crate::bubble_native::hwnd())?;
    let (w, h, rgba) = crate::bubble_native::snapshot(app)?;
    Some((w, h, rgba, x, y))
}

fn grab_picker(app: &AppHandle) -> Option<Frame> {
    let (w, h, rgba) = crate::dialogs_native::picker::snapshot(app)?;
    Some((w, h, rgba, 0, 0))
}

fn grab_confirm(app: &AppHandle) -> Option<Frame> {
    let (w, h, rgba) = crate::dialogs_native::confirm::snapshot(app)?;
    Some((w, h, rgba, 0, 0))
}

/// Запись одного окна: поток снимков с частотой FPS и поток записи PNG.
struct Recorder {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Recorder {
    fn start(app: &AppHandle, scene: &str, window: &str, started: Instant, grab: fn(&AppHandle) -> Option<Frame>) -> Self {
        let dir = out_dir().join(scene).join(window);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let app = app.clone();
        let thread = std::thread::spawn(move || {
            // очередь короткая: если PNG пишутся медленнее, замедляются и снимки — время в
            // frames.txt всё равно настоящее
            let (tx, rx) = mpsc::sync_channel::<(Frame, u128)>(4);
            let writer_dir = dir.clone();
            let writer = std::thread::spawn(move || {
                let Ok(mut list) = std::fs::File::create(writer_dir.join("frames.txt")) else { return };
                for (i, ((w, h, rgba, x, y), ms)) in rx.into_iter().enumerate() {
                    let name = format!("{i:04}.png");
                    let Ok(file) = std::fs::File::create(writer_dir.join(&name)) else { continue };
                    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    enc.set_compression(png::Compression::Fast);
                    if let Ok(mut wr) = enc.write_header() {
                        let _ = wr.write_image_data(&rgba);
                    }
                    let _ = writeln!(list, "{name} {ms} {x} {y}");
                }
            });
            let tick = Duration::from_millis(1000 / FPS);
            let mut next = Instant::now();
            while !stop2.load(Ordering::Relaxed) {
                if let Some(frame) = grab(&app) {
                    if tx.send((frame, started.elapsed().as_millis())).is_err() {
                        break;
                    }
                }
                next += tick;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
            drop(tx);
            let _ = writer.join();
        });
        Self { stop, thread }
    }

    fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

fn wait(secs: f32) {
    std::thread::sleep(Duration::from_secs_f32(secs));
}

fn step(started: Instant, name: &str) {
    eprintln!("[readme-demo] {:>6} ms {name}", started.elapsed().as_millis());
}

/// Голос: уровень микрофона синусом около −30…−10 дБFS, `secs` секунд; главному окну
/// событием, маскоту вызовом (как events.rs).
fn voice(app: &AppHandle, secs: f32) {
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f32() < secs {
        let t = t0.elapsed().as_secs_f32();
        let db = -20.0 + 10.0 * (t * std::f32::consts::TAU * 2.0).sin() * (t * 5.3).cos().abs();
        let rms = 10f32.powf(db / 20.0);
        let _ = app.emit("reever://level", rms);
        crate::mascot::level(rms);
        std::thread::sleep(Duration::from_millis(40));
    }
    let _ = app.emit("reever://level", 0.0f32);
    crate::mascot::level(0.0);
}

/// Фраза по словам в `reever://partial`, как растёт распознавание по ходу речи.
fn speak(app: &AppHandle, phrase: &str, per_word: f32) {
    let words: Vec<&str> = phrase.split(' ').collect();
    for n in 1..=words.len() {
        let _ = app.emit("reever://partial", words[..n].join(" "));
        voice(app, per_word);
    }
}

fn item(name: &str, path: &str, folder: bool, frequent: bool) -> FindItem {
    FindItem { name: name.into(), path: path.into(), folder, frequent, score: 0.0 }
}

/// Главное окно: «reever, open notepad» — слушаю, текст растёт, услышал, выполнил.
fn scene_command(app: &AppHandle) {
    let started = Instant::now();
    step(started, "command");
    let rec = Recorder::start(app, "command", "main", started, grab_main);
    wait(1.2);
    let _ = app.emit("reever://wake", ());
    events::set_state(app, "listening");
    voice(app, 0.5);
    speak(app, "open notepad", 0.45);
    voice(app, 0.3);
    let _ = app.emit("reever://heard", "open notepad");
    events::set_state(app, "busy");
    wait(1.0);
    events::ui_log(app, "opened: Notepad");
    events::set_state(app, "idle");
    let _ = app.emit("reever://partial", "");
    wait(1.6);
    rec.finish();
}

/// Главное окно: вкладки по очереди — действия, настройки, текст из файла, индекс.
fn scene_tour(app: &AppHandle) {
    use crate::main_native::{send, Msg};
    let started = Instant::now();
    step(started, "tour");
    let rec = Recorder::start(app, "tour", "main", started, grab_main);
    wait(1.0);
    send(Msg::Tab("actions".into()));
    wait(1.8);
    send(Msg::Tab("settings".into()));
    wait(1.2);
    for _ in 0..4 {
        send(Msg::ScrollSettings(260.0));
        wait(0.45);
    }
    wait(0.6);
    send(Msg::Tab("text".into()));
    wait(0.8);
    let text = "Welcome back to the channel. Today we are testing speech to text: \
                drop any audio or video file and Reever writes down what was said, \
                fully offline, right on this computer.";
    let words: Vec<&str> = text.split(' ').collect();
    let steps = 8;
    for i in 1..=steps {
        let n = words.len() * i / steps;
        crate::transcribe::publish(&crate::transcribe::TextJob {
            kind: crate::transcribe::JobKind::File,
            name: "interview.m4a".into(),
            running: true,
            progress: Some(i as f32 / steps as f32),
            text: words[..n].join(" "),
            ..Default::default()
        });
        wait(0.3);
    }
    crate::transcribe::publish(&crate::transcribe::TextJob {
        kind: crate::transcribe::JobKind::File,
        name: "interview.m4a".into(),
        text: text.into(),
        copied: true,
        ..Default::default()
    });
    wait(1.4);
    send(Msg::Tab("index".into()));
    wait(0.6);
    let items = vec![
        item("Projects", r"C:\Users\demo\Projects", true, true),
        item("project-plan.docx", r"C:\Users\demo\Documents\Work\project-plan.docx", false, false),
        item("Project photos", r"D:\Photos\2026\Project photos", true, false),
        item("projector-manual.pdf", r"C:\Users\demo\Downloads\projector-manual.pdf", false, false),
    ];
    send(Msg::IndexResults(0, Ok(items)));
    wait(1.8);
    send(Msg::Tab("home".into()));
    wait(1.2);
    rec.finish();
}

/// Маскот: осьминог выходит из сферы к углу экрана, пузырь показывает, что слышно.
fn scene_mascot(app: &AppHandle) {
    let started = Instant::now();
    step(started, "mascot");
    let mascot = Recorder::start(app, "mascot", "mascot", started, grab_mascot);
    let bubble = Recorder::start(app, "mascot", "bubble", started, grab_bubble);
    wait(0.6);
    crate::mascot::on_wake(app);
    let _ = app.emit("reever://wake", ());
    wait(1.6);
    events::set_state(app, "listening");
    voice(app, 0.4);
    speak(app, "search youtube lofi hip hop radio", 0.4);
    voice(app, 0.3);
    let _ = app.emit("reever://heard", "search youtube lofi hip hop radio");
    crate::mascot::heard("search youtube lofi hip hop radio");
    wait(0.3);
    events::set_state(app, "busy");
    wait(1.2);
    events::set_state(app, "idle");
    let _ = app.emit("reever://partial", "");
    wait(3.5);
    mascot.finish();
    bubble.finish();
}

/// «reever listen»: звук компьютера в текст, мониторчик в углу.
fn scene_listen(app: &AppHandle) {
    let started = Instant::now();
    step(started, "listen");
    let rec = Recorder::start(app, "listen", "bubble", started, grab_bubble);
    crate::bubble::set_listening(app, true);
    wait(1.0);
    let lines = [
        "Hi everyone, and welcome to the stream.",
        "Today we are going to build a small voice launcher in Rust.",
    ];
    for line in lines {
        let words: Vec<&str> = line.split(' ').collect();
        for n in 1..=words.len() {
            crate::bubble_native::send(crate::bubble_native::Msg::Partial(words[..n].join(" ")));
            wait(0.22);
        }
        // готовая фраза дописывается к прежним (как listen.rs)
        crate::bubble_native::send(crate::bubble_native::Msg::Said(line.into()));
        wait(0.5);
    }
    wait(1.0);
    crate::bubble::set_listening(app, false);
    wait(3.0);
    rec.finish();
}

/// Окно выбора: «reever folder project» — найденное, выбор стрелками.
fn scene_picker(app: &AppHandle) {
    let started = Instant::now();
    step(started, "picker");
    let shared = app.state::<Arc<crate::actions::SharedState>>().inner().clone();
    let rec = Recorder::start(app, "picker", "picker", started, grab_picker);
    let items = vec![
        item("Projects", r"C:\Users\demo\Projects", true, true),
        item("Project photos", r"D:\Photos\2026\Project photos", true, false),
        item("project-plan.docx", r"C:\Users\demo\Documents\Work\project-plan.docx", false, false),
        item("Project archive", r"E:\Backup\Project archive", true, false),
        item("projector-manual.pdf", r"C:\Users\demo\Downloads\projector-manual.pdf", false, false),
    ];
    crate::picker::show(app, &shared, "project", "project", FindScope::Any, &items);
    wait(1.5);
    for _ in 0..2 {
        key(crate::dialogs_native::picker::hwnd(), 0x28);
        wait(0.7);
    }
    key(crate::dialogs_native::picker::hwnd(), 0x26);
    wait(1.2);
    rec.finish();
    crate::picker::hide(app, &shared);
}

/// Вопрос перед опасной командой: «reever shut down» — ждёт «yes» голосом.
fn scene_confirm(app: &AppHandle) {
    let started = Instant::now();
    step(started, "confirm");
    let rec = Recorder::start(app, "confirm", "confirm", started, grab_confirm);
    crate::confirm::show(app, &crate::dispatch::PendingAction::System("shutdown".into()), 8);
    wait(4.5);
    rec.finish();
    crate::confirm::hide(app);
}

/// Клавиша окну без фокуса (окно разбирает WM_KEYDOWN само).
fn key(hwnd: Option<isize>, vk: u32) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP};
    let Some(hwnd) = hwnd else { return };
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(1));
        let _ = PostMessageW(hwnd, WM_KEYUP, WPARAM(vk as usize), LPARAM(0xC000_0001u32 as isize));
    }
}

pub fn start(app: &AppHandle) {
    // английский интерфейс и полный вид — только в памяти, конфиг и так во временной папке
    if let Some(config) = app.try_state::<crate::config::LiveConfig>() {
        let mut fresh = (*config.get()).clone();
        fresh.settings.lite_ui = false;
        fresh.settings.ui_language = "en".into();
        fresh.settings.mascot_mode = "commands".into();
        fresh.settings.mascot_bubble = true;
        config.set(Arc::new(fresh));
    }
    if let Ok(mut current) = crate::uilang::CURRENT.lock() {
        *current = crate::uilang::resolve("en");
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let only = std::env::var("REEVER_README_SCENES").ok();
        let wanted = |name: &str| only.as_deref().is_none_or(|o| o.split(',').any(|s| s.trim() == name));
        let _ = app.emit("reever://status", serde_json::json!({ "asr": "ready", "model": "parakeet-v3" }));
        crate::main_native::show(&app);
        // кадры осьминога в сфере декодируются при первом проходе (~4 с)
        wait(6.0);
        // обход вкладок — первым: начинается с чистой «Главной» и на ней кончается, так что и
        // команда после него начинается с чистого листа
        if wanted("tour") {
            scene_tour(&app);
        }
        if wanted("command") {
            scene_command(&app);
        }
        if wanted("mascot") {
            scene_mascot(&app);
        }
        if wanted("listen") {
            scene_listen(&app);
        }
        if wanted("picker") {
            scene_picker(&app);
            wait(1.0);
        }
        if wanted("confirm") {
            scene_confirm(&app);
        }
        eprintln!("[readme-demo] done");
        app.exit(0);
    });
}
