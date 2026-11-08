//! «ривер слушай»: звук компьютера (loopback.rs) — в текст во вкладке «Текст».
//! Кончается само, если `listen_silence_s` секунд не было нового текста (больше 10 — только
//! командой), по «ривер стоп» (текст остаётся и уходит в буфер обмена) и «ривер отмена»
//! (текст отброшен). Модели — свои, под языки распознавания (transcribe.rs).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::AppHandle;
use crate::transcribe::{publish, JobEnd, JobKind, Joiner, Recognizer, TextJob};

/// До первой речи в звуке компьютера ждать не меньше этого: команду сказали, видео ещё не
/// включили.
const FIRST_SPEECH_WAIT: Duration = Duration::from_secs(10);

/// Секунд без текста больше этого — слушать до команды.
pub const SILENCE_UNTIL_COMMAND: u32 = 11;

const RUN: u8 = 0;
const STOP: u8 = 1;
const CANCEL: u8 = 2;

static SESSION: Mutex<Option<Arc<AtomicU8>>> = Mutex::new(None);

/// Идёт прослушивание.
pub fn active() -> bool {
    SESSION.lock().map(|s| s.is_some()).unwrap_or(false)
}

/// Закончить: `cancel` — отбросить текст.
pub fn stop(cancel: bool) -> bool {
    let Some(flag) = SESSION.lock().ok().and_then(|s| s.clone()) else { return false };
    flag.store(if cancel { CANCEL } else { STOP }, Ordering::SeqCst);
    true
}

/// Сколько молчать до конца (None — до команды): настройка 3–10 с, больше — до команды.
pub fn silence_limit(seconds: u32) -> Option<Duration> {
    (seconds < SILENCE_UNTIL_COMMAND).then(|| Duration::from_secs(seconds.clamp(3, 10) as u64))
}

/// Начать слушать звук компьютера. `false` — уже слушает.
pub fn start(app: &AppHandle) -> bool {
    let flag = Arc::new(AtomicU8::new(RUN));
    {
        let Ok(mut slot) = SESSION.lock() else { return false };
        if slot.is_some() {
            return false;
        }
        *slot = Some(flag.clone());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut job = TextJob { kind: JobKind::Listen, running: true, ..TextJob::default() };
        publish(&job);
        let end = run(&app, &flag, &mut job);
        job.running = false;
        job.end = Some(end);
        match end {
            JobEnd::Cancelled => {
                job.text.clear();
                crate::events::ui_log(&app, crate::msg!("log.computer.cancelled"));
            }
            JobEnd::Error => crate::events::ui_log(&app, crate::msg!("log.computer.failed", error = &job.error)),
            _ => {
                job.copied = !job.text.is_empty() && crate::typing::set_clipboard(&job.text).is_ok();
                crate::events::ui_log(&app, crate::msg!("log.computer.done", chars = job.text.chars().count()));
            }
        }
        if let Ok(mut slot) = SESSION.lock() {
            *slot = None;
        }
        publish(&job);
    });
    true
}

fn run(app: &AppHandle, flag: &AtomicU8, job: &mut TextJob) -> JobEnd {
    let cfg = app.state::<crate::config::LiveConfig>().get();
    let s = &cfg.settings;
    let languages: Vec<String> = s.recognition_languages().iter().map(|c| c.to_string()).collect();
    let mut recognizer = match Recognizer::load(&languages, &cfg.model_dirs, &cfg.vad_model_path, s.vad_threshold, s.asr_threads as i32) {
        Ok(r) => r,
        Err(e) => {
            job.error = format!("{e:#}");
            return JobEnd::Error;
        }
    };
    let (capture, rx) = match crate::loopback::start() {
        Ok(c) => c,
        Err(e) => {
            job.error = format!("{e:#}");
            return JobEnd::Error;
        }
    };
    let limit = silence_limit(s.listen_silence_s);
    crate::events::ui_log(
        app,
        match limit {
            Some(l) => crate::msg!("log.computer.start", seconds = l.as_secs()),
            None => crate::msg!("log.computer.startUntilCommand"),
        },
    );
    let mut segmenter = Recognizer::segmenter();
    let mut joiner = Joiner::new(s.dictation_punctuation);
    let mut phrases = Vec::new();
    // тишина считается по звуку: пока в нём речь (фраза ещё не кончилась — текста нет),
    // прослушивание идёт; до первой речи — не меньше FIRST_SPEECH_WAIT (успеть включить видео)
    let started = Instant::now();
    let mut quiet_since = Instant::now();
    let mut heard = false;
    // открытое окно — на вкладку «Текст»: там идёт текст
    crate::main_native::send(crate::main_native::Msg::Tab("text".into()));
    let end = loop {
        match flag.load(Ordering::SeqCst) {
            STOP => break JobEnd::Stopped,
            CANCEL => break JobEnd::Cancelled,
            _ => {}
        }
        if let Ok(packet) = rx.recv_timeout(Duration::from_millis(100)) {
            recognizer.push(&mut segmenter, &packet, &mut phrases);
        }
        if segmenter.in_speech() {
            quiet_since = Instant::now();
            heard = true;
        }
        for phrase in phrases.drain(..) {
            match recognizer.phrase(&phrase) {
                Ok(text) if !text.is_empty() => {
                    joiner.push(&text, false);
                    job.text = joiner.text.clone();
                    quiet_since = Instant::now();
                    heard = true;
                    publish(job);
                    // фраза — в бегущую строку окна и пузырь маскота
                    let _ = app.emit("reever://partial", format!("🎧 {text}"));
                    let _ = app.emit("reever://said", text.clone());
                }
                Ok(_) => {}
                Err(e) => {
                    job.error = format!("{e:#}");
                    break;
                }
            }
        }
        if !job.error.is_empty() {
            break JobEnd::Error;
        }
        if let Some(l) = limit {
            let waited = if heard { quiet_since.elapsed() } else { started.elapsed().saturating_sub(FIRST_SPEECH_WAIT.saturating_sub(l)) };
            if waited >= l && !segmenter.in_speech() {
                break JobEnd::Silence;
            }
        }
    };
    drop(capture);
    let _ = app.emit("reever://partial", String::new());
    // «стоп» посреди фразы — дослушанное тоже в текст
    if matches!(end, JobEnd::Stopped) {
        if let Some(rest) = segmenter.finish_now() {
            if let Ok(text) = recognizer.phrase(&rest) {
                joiner.push(&text, false);
                job.text = joiner.text.clone();
            }
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_limit_is_three_to_ten_seconds_or_until_command() {
        assert_eq!(silence_limit(5), Some(Duration::from_secs(5)));
        assert_eq!(silence_limit(1), Some(Duration::from_secs(3)));
        assert_eq!(silence_limit(10), Some(Duration::from_secs(10)));
        assert_eq!(silence_limit(11), None);
        assert_eq!(silence_limit(60), None);
    }
}
