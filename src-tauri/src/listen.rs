//! «ривер слушай»: звук компьютера (loopback.rs) — в текст. Тихо: ни маскота, ни главного
//! окна; распознанное по ходу речи видно в пузыре-мониторчике справа внизу (bubble.rs), в конце
//! текст молча уходит в буфер обмена и остаётся во вкладке «Текст» (скопировать ещё раз).
//! Кончается само, если `listen_silence_s` секунд в звуке не было речи (больше 10 — только
//! командой), по «ривер стоп» (текст остаётся) и «ривер отмена» (текст отброшен). Новое
//! «слушай» во время прослушивания начинает запись заново. Модели — свои, под языки
//! распознавания (transcribe.rs).

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

/// Промежуточный текст фразы в пузыре — не чаще этого (и не чаще, чем вдвое дольше его счёта).
const PARTIAL_EVERY: Duration = Duration::from_millis(700);

/// Начать слушать звук компьютера. Уже слушает — та запись отбрасывается и начинается новая.
pub fn start(app: &AppHandle) -> bool {
    let flag = Arc::new(AtomicU8::new(RUN));
    let previous = SESSION.lock().ok().and_then(|s| s.clone());
    if let Some(old) = &previous {
        old.store(CANCEL, Ordering::SeqCst);
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // прежняя запись отпускает захват и модели — дождаться её конца
        for _ in 0..200 {
            match SESSION.lock() {
                Ok(mut slot) if slot.is_none() => {
                    *slot = Some(flag.clone());
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(25)),
            }
        }
        if !SESSION.lock().is_ok_and(|s| s.as_ref().is_some_and(|f| Arc::ptr_eq(f, &flag))) {
            return;
        }
        crate::bubble::set_listening(&app, true);
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
        crate::bubble::set_listening(&app, false);
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
    let (mut partial_at, mut partial_cost) = (Instant::now(), Duration::ZERO);
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
                    // готовая фраза — в пузырь (дописывается к прежним)
                    crate::bubble_native::send(crate::bubble_native::Msg::Said(text.clone()));
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
        // недоговорённая фраза — в пузырь по ходу речи, как при диктовке
        let audio = segmenter.current_audio();
        if audio.len() > crate::speech::samples(500) && partial_at.elapsed() >= PARTIAL_EVERY.max(partial_cost * 2) {
            let t = Instant::now();
            if let Ok(text) = recognizer.phrase(audio) {
                if !text.is_empty() {
                    crate::bubble_native::send(crate::bubble_native::Msg::Partial(text));
                }
            }
            partial_cost = t.elapsed();
            partial_at = Instant::now();
        }
        if let Some(l) = limit {
            let waited = if heard { quiet_since.elapsed() } else { started.elapsed().saturating_sub(FIRST_SPEECH_WAIT.saturating_sub(l)) };
            if waited >= l && !segmenter.in_speech() {
                break JobEnd::Silence;
            }
        }
    };
    drop(capture);
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
