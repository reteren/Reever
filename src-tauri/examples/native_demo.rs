// Демо нативного ядра окон (этап 0a docs/native-ui-roadmap.md): прозрачное окно поверх всех с
// анимированными кадрами маскота и строками на русском, арабском, японском, корейском и
// китайском. Печатает время до первого кадра, рабочий набор процесса и загрузку CPU.
//
//   cargo run --release --example native_demo -- [--fps 0|60|30] [--seconds 10]
//       [--warmup 1] [--screenshot out.png] [--device-loss] [--static] [--cold] [--hold]
//
// --static      — без анимации (замер покоя: окно на экране, кадров нет)
// --device-loss — на 3-й секунде пройти путь восстановления после потери устройства
// --screenshot  — снимок экрана под окном (то, что реально показал DWM) в PNG
// --click-through — сделать окно сквозным для мыши (WS_EX_LAYERED + WS_EX_TRANSPARENT) и
//                 проверить WindowFromPoint: под центром окна должно быть чужое окно
// --fake-dpi N  — прислать окну WM_DPICHANGED с DPI N (проверка смены масштаба)
// --cold        — не создавать устройство заранее (Ui::prewarm), мерить холодный первый кадр
// --hold        — не закрываться после замера (посмотреть глазами; закрыть Ctrl+C)

use std::path::PathBuf;
use std::time::{Duration, Instant};

use reever_lib::native::{
    stats, Align, Color, Font, ImageId, Rect, RenderCx, TextLayout, TextStyle, UiThread,
    WindowHandler, WindowSpec,
};

const SPRITE_FPS: f32 = 24.0;
const WIN_W: f32 = 380.0;
const WIN_H: f32 = 600.0;

struct Line {
    text: &'static str,
    style: TextStyle,
    color: Color,
}

struct Demo {
    frames: Vec<ImageId>,
    t: f32,
    animate: bool,
    lines: Vec<Line>,
    layouts: Vec<TextLayout>,
    no_sprite: bool,
}

impl WindowHandler for Demo {
    fn animating(&self) -> bool {
        self.animate
    }

    fn advance(&mut self, dt: Duration) {
        self.t += dt.as_secs_f32();
    }

    fn render(&mut self, cx: &mut RenderCx<'_>) {
        let (w, h) = cx.size;
        let pad = 16.0;
        if self.layouts.is_empty() {
            for l in &self.lines {
                match cx.text().layout(l.text, &l.style, w - pad * 2.0) {
                    Ok(t) => self.layouts.push(t),
                    Err(e) => eprintln!("разметка «{}»: {e}", l.text),
                }
            }
        }
        // полупрозрачная подложка — видно, что альфа попиксельная
        cx.fill_rounded(Rect::new(0.0, 0.0, w, h), 18.0, Color::rgba(14, 22, 30, 170));
        cx.fill_rounded(Rect::new(6.0, 6.0, w - 12.0, 4.0), 2.0, Color::rgba(90, 220, 255, 200));

        let frame = ((self.t * SPRITE_FPS) as usize) % self.frames.len().max(1);
        if let Some(&id) = self.frames.get(frame).filter(|_| !self.no_sprite) {
            // кадр 384×512 в 180×240 DIP: кэш держит копию ровно под пиксели монитора
            cx.draw_image(id, Rect::new((w - 180.0) / 2.0, 14.0, 180.0, 240.0), 1.0);
        }
        let mut y = 262.0;
        for (layout, line) in self.layouts.iter().zip(&self.lines) {
            cx.draw_text(layout, pad, y, line.color);
            y += layout.height + 6.0;
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let fps: u32 = arg_value(&args, "--fps").and_then(|v| v.parse().ok()).unwrap_or(30);
    let seconds: u64 = arg_value(&args, "--seconds").and_then(|v| v.parse().ok()).unwrap_or(10);
    let warmup: f64 = arg_value(&args, "--warmup").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let screenshot = arg_value(&args, "--screenshot").map(PathBuf::from);
    let device_loss = args.iter().any(|a| a == "--device-loss");
    let animate = !args.iter().any(|a| a == "--static");
    let hold = args.iter().any(|a| a == "--hold");
    // для разбора затрат: без текста / без спрайта
    let no_text = args.iter().any(|a| a == "--no-text");
    let no_sprite = args.iter().any(|a| a == "--no-sprite");

    let (ws0, _) = stats::memory();
    let mascot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend/mascot");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&mascot_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("idle_") && n.ends_with(".webp")))
        .collect();
    files.sort();
    anyhow::ensure!(!files.is_empty(), "нет кадров idle_*.webp в {}", mascot_dir.display());

    let t_spawn = Instant::now();
    let ui = UiThread::spawn()?;
    let spawn_ms = t_spawn.elapsed().as_secs_f64() * 1000.0;
    // --cold: устройство создаётся при первом кадре окна (как без предпрогрева)
    let cold = args.iter().any(|a| a == "--cold");
    let prewarm_ms = if cold {
        None
    } else {
        let t = Instant::now();
        ui.call(|ui| ui.prewarm()).ok_or_else(|| anyhow::anyhow!("поток окон не отвечает"))??;
        Some(t.elapsed().as_secs_f64() * 1000.0)
    };

    let lines = vec![
        Line { text: "Привет! Я Ривер — скажите «ривер, открой браузер».",
            style: TextStyle { font: Font::Geologica, size: 17.0, weight: 600, ..Default::default() },
            color: Color::rgb(235, 245, 255) },
        Line { text: "СЛУШАЮ · 12:45",
            style: TextStyle { font: Font::Handjet, size: 24.0, weight: 600,
                axes: vec![(*b"ELSH", 2.0), (*b"ELGR", 1.0)], ..Default::default() },
            color: Color::rgb(90, 220, 255) },
        Line { text: "martian mono 0123456789",
            style: TextStyle { font: Font::MartianMono, size: 12.0, ..Default::default() },
            color: Color::rgb(170, 190, 205) },
        Line { text: "مرحبا بالعالم — هذا نص عربي يلتف على عدة أسطر داخل النافذة",
            style: TextStyle { font: Font::Geologica, size: 18.0, locale: "ar-SA".into(), ..Default::default() },
            color: Color::rgb(255, 225, 160) },
        Line { text: "こんにちは世界",
            style: TextStyle { font: Font::Geologica, size: 20.0, locale: "ja-JP".into(), ..Default::default() },
            color: Color::rgb(255, 190, 210) },
        Line { text: "안녕하세요 세계",
            style: TextStyle { font: Font::Geologica, size: 20.0, locale: "ko-KR".into(), ..Default::default() },
            color: Color::rgb(190, 255, 200) },
        Line { text: "你好，世界 · 中文",
            style: TextStyle { font: Font::Geologica, size: 18.0, locale: "zh-CN".into(), align: Align::Center, ..Default::default() },
            color: Color::rgb(220, 220, 255) },
    ];

    let lines = if no_text { Vec::new() } else { lines };
    let t_create = Instant::now();
    let id = ui
        .call(move |ui| {
            ui.set_fps_limit(fps);
            let frames = files.iter().map(|p| ui.images().add_file(p)).collect();
            let spec = WindowSpec { title: "Reever native demo".into(), x: 200, y: 160, width: WIN_W, height: WIN_H, ..Default::default() };
            ui.create_window(spec, Box::new(Demo { frames, t: 0.0, animate, lines, layouts: Vec::new(), no_sprite }))
        })
        .ok_or_else(|| anyhow::anyhow!("поток окон не отвечает"))??;

    // время до первого кадра (от вызова create_window в потоке окон и от запроса извне)
    let first = loop {
        let info = ui.call(move |ui| ui.info(id)).flatten();
        if let Some(f) = info.and_then(|i| i.first_frame) {
            break (f, info.unwrap());
        }
        if t_create.elapsed() > Duration::from_secs(10) {
            anyhow::bail!("первый кадр не появился за 10 с");
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let (first_frame, info) = first;
    let request_to_frame = t_create.elapsed();
    println!("поток окон запущен за {spawn_ms:.1} мс");
    match prewarm_ms {
        Some(ms) => println!("предпрогрев устройства: {ms:.1} мс"),
        None => println!("без предпрогрева (--cold): устройство создаётся в первом кадре"),
    }
    println!(
        "первый кадр: {:.1} мс от CreateWindow, ≤{:.1} мс от запроса; DPI {} (×{:.2}), окно {}×{} px",
        first_frame.as_secs_f64() * 1000.0,
        request_to_frame.as_secs_f64() * 1000.0,
        info.dpi,
        info.scale,
        info.size_px.0,
        info.size_px.1
    );
    let sw = ui.call(|ui| ui.software_rendering()).flatten();
    println!("устройство: {}", if sw == Some(true) { "WARP (программное)" } else { "видеокарта" });

    // дать прогреться, потом мерить. Первый круг анимации декодирует кадры в кэш (96 кадров
    // по 24 в секунду — 4 с), поэтому установившийся режим — с --warmup 5 и больше.
    std::thread::sleep(Duration::from_secs_f64(warmup));
    if let Some(dpi) = arg_value(&args, "--fake-dpi").and_then(|v| v.parse::<u32>().ok()) {
        fake_dpi_change(info.hwnd, dpi);
        std::thread::sleep(Duration::from_millis(500));
        let i = ui.call(move |ui| ui.info(id)).flatten().expect("окно");
        println!("после WM_DPICHANGED {dpi}: DPI {} (×{:.2}), окно {}×{} px", i.dpi, i.scale, i.size_px.0, i.size_px.1);
    }
    if args.iter().any(|a| a == "--click-through") {
        use windows::Win32::Foundation::{HWND, POINT};
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let h = HWND(info.hwnd as *mut _);
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            SetWindowLongW(h, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as i32);
            std::thread::sleep(Duration::from_millis(300));
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(h, &mut r);
            let under = WindowFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 });
            println!("сквозной клик: окно под центром {:?}, наше {:?}", under.0, h.0);
        }
    }
    if let Some(path) = &screenshot {
        capture(info.hwnd, path)?;
        println!("снимок: {}", path.display());
    }
    let frames0 = ui.call(|ui| ui.frames()).unwrap_or(0);
    let meter = stats::CpuMeter::start();
    let start = Instant::now();
    let mut lost_done = false;
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(100));
        if device_loss && !lost_done && start.elapsed() > Duration::from_secs(3) {
            lost_done = true;
            ui.post(|ui| ui.simulate_device_loss());
            println!("потеря устройства смоделирована");
        }
    }
    let cpu = meter.finish();
    let elapsed = start.elapsed().as_secs_f64();
    let frames = ui.call(|ui| ui.frames()).unwrap_or(0) - frames0;
    let (ws, private) = stats::memory();
    let (cached, cache_bytes, decodes) = ui.call(|ui| ui.images().stats()).unwrap_or_default();
    println!(
        "лимит {}: {:.1} кадр/с за {:.0} с; CPU {:.2} % ядра ({:.2} % всего ЦП, {} лог. ядер)",
        if fps == 0 { "как монитор".to_string() } else { fps.to_string() },
        frames as f64 / elapsed,
        elapsed,
        cpu.of_one_core,
        cpu.of_total,
        stats::logical_cpus()
    );
    println!(
        "RAM: рабочий набор {:.1} МБ (до окна {:.1} МБ), частные {:.1} МБ; кэш кадров {} шт / {:.1} МБ, декодирований {}",
        mb(ws), mb(ws0), mb(private), cached, mb(cache_bytes), decodes
    );
    if let Some(path) = &screenshot {
        if device_loss {
            let after = path.with_file_name(format!(
                "{}-after-loss.png",
                path.file_stem().and_then(|s| s.to_str()).unwrap_or("shot")
            ));
            capture(info.hwnd, &after)?;
            println!("снимок после восстановления: {}", after.display());
        }
    }
    if hold {
        println!("окно открыто, Ctrl+C — выход");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    drop(ui);
    Ok(())
}

/// Прислать окну WM_DPICHANGED, как при переносе на монитор с другим масштабом (мониторов с
/// разным масштабом может не оказаться под рукой; путь обработки в окне тот же).
fn fake_dpi_change(hwnd: isize, dpi: u32) {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SendMessageW, WM_DPICHANGED};
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (w, h) = (
            (WIN_W * dpi as f32 / 96.0).round() as i32,
            (WIN_H * dpi as f32 / 96.0).round() as i32,
        );
        let suggested = RECT { left: r.left, top: r.top, right: r.left + w, bottom: r.top + h };
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            WPARAM((dpi | (dpi << 16)) as usize),
            LPARAM(&suggested as *const RECT as isize),
        );
    }
}

/// Снимок того, что DWM показал на месте окна (BitBlt с экрана: содержимое окна на
/// DirectComposition через PrintWindow не достаётся — у окна нет своей поверхности).
fn capture(hwnd: isize, path: &std::path::Path) -> anyhow::Result<()> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut r = RECT::default();
        GetWindowRect(HWND(hwnd as *mut _), &mut r)?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        BitBlt(mem, 0, 0, w, h, screen, r.left, r.top, SRCCOPY | CAPTUREBLT)?;
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = png::Encoder::new(file, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&px)?;
    }
    Ok(())
}
