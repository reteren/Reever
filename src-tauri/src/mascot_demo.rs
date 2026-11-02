//! Проверка маскота без микрофона: `REEVER_MASCOT_DEMO=1`. Reever запускается рядом с
//! установленным (своё имя события единственного экземпляра), не слушает эфир и не
//! регистрирует горячие клавиши, а прогоняет маскота по сценарию — те же вызовы, что делает
//! распознавание: wake, состояния, уровень микрофона, «услышал», отказ wake, знакомство.
//! Шаги печатаются в stderr (`[mascot-demo] шаг`), чтобы успеть снять кадры. Работает и с
//! веб-, и с нативным окном (`native_mascot`) — так их и сравнивают.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::{events, mascot};

pub fn active() -> bool {
    std::env::var_os("REEVER_MASCOT_DEMO").is_some()
}

fn step(app: &AppHandle, started: Instant, name: &str) {
    let line = format!("[mascot-demo] {:>6} ms {name}", started.elapsed().as_millis());
    eprintln!("{line}");
    events::ui_log(app, line);
}

/// reever://level — веб-окнам событием, нативному вызовом (как events.rs).
fn level(app: &AppHandle, rms: f32) {
    let _ = app.emit_to("main", "reever://level", rms);
    let _ = app.emit_to("mascot", "reever://level", rms);
    mascot::level(rms);
}

fn wait(d: f32) {
    std::thread::sleep(Duration::from_secs_f32(d));
}

/// reever://partial — текст по ходу речи (пузырь, веб или нативный, слушает событие сам).
fn partial(app: &AppHandle, text: &str) {
    let _ = app.emit("reever://partial", text);
}

/// Голос: громкость синусом 2 Гц около −30…−10 дБFS, `secs` секунд.
fn voice(app: &AppHandle, secs: f32) {
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f32() < secs {
        let t = t0.elapsed().as_secs_f32();
        let db = -20.0 + 10.0 * (t * std::f32::consts::TAU * 2.0).sin();
        level(app, 10f32.powf(db / 20.0));
        std::thread::sleep(Duration::from_millis(50));
    }
    level(app, 0.0);
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        events::set_state(&app, "idle");
        // окно маскота создаётся асинхронно
        for _ in 0..100 {
            if mascot::geometry(&app).is_some() {
                break;
            }
            wait(0.1);
        }
        wait(3.0);
        step(&app, started, "idle");
        wait(4.0);
        step(&app, started, "wake");
        mascot::on_wake(&app);
        let _ = app.emit("reever://wake", ());
        wait(4.0);
        step(&app, started, "listening");
        events::set_state(&app, "listening");
        voice(&app, 1.5);
        // пузырь: короткая и длинная фраза, арабский (справа налево) и японский
        for (name, text) in [
            ("bubble-short", "открой браузер"),
            ("bubble-long", "ривер напиши письмо Ане что я задержусь на полчаса и приду к семи вечера, пусть не ждёт с ужином"),
            ("bubble-ar", "مرحبا بالعالم، افتح المتصفح من فضلك"),
            ("bubble-ja", "こんにちは世界、ブラウザを開いてください"),
        ] {
            step(&app, started, name);
            partial(&app, text);
            voice(&app, 2.5);
        }
        step(&app, started, "heard");
        let _ = app.emit("reever://heard", "ривер открой браузер");
        mascot::heard("ривер открой браузер");
        wait(0.3);
        step(&app, started, "busy");
        events::set_state(&app, "busy");
        voice(&app, 3.0);
        wait(1.0);
        step(&app, started, "idle-after-command");
        events::set_state(&app, "idle");
        wait(4.0);
        step(&app, started, "paused");
        events::set_state(&app, "paused");
        wait(4.0);
        step(&app, started, "idle-again");
        events::set_state(&app, "idle");
        wait(3.0);
        step(&app, started, "wake-then-rejected");
        mascot::on_wake(&app);
        let _ = app.emit("reever://wake", ());
        wait(2.5);
        mascot::on_wake_rejected(&app);
        let _ = app.emit("reever://wake-rejected", ());
        step(&app, started, "rejected");
        wait(5.0);
        step(&app, started, "intro");
        let lines = vec!["Привет! Я Ривер.".to_string(), "Скажите «ривер» — и я слушаю.".to_string()];
        mascot::mascot_intro(app.clone(), lines);
        wait(12.0);
        // переключение окна на лету, как из настроек (set_native_mascot)
        let native = mascot::native_enabled();
        step(&app, started, if native { "switch-to-web" } else { "switch-to-native" });
        mascot::set_native(&app, !native);
        wait(6.0);
        step(&app, started, "switch-back");
        mascot::set_native(&app, native);
        wait(6.0);
        step(&app, started, "done");
    });
}
