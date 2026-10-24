//! Отдельный поток для всего, что диктовка делает через COM и окна: UI Automation,
//! проверка орфографии, владелец буфера обмена, SendInput.
//!
//! Раньше это выполнялось прямо в аудио-воркере, который же запускает и закрывает
//! программы. `CoInitializeEx(MULTITHREADED)` из диктовки переводил воркер в MTA, а
//! ShellExecute (ярлыки, UWP) рассчитан на STA; вдобавок окно-владелец буфера жило в
//! потоке без цикла сообщений. Здесь — свой STA-поток, который качает сообщения между
//! заданиями, а воркер просто ждёт результат.

use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        mpsc::{self, RecvTimeoutError},
        Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

use windows::Win32::{
    System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
    UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE},
};

type Job = Box<dyn FnOnce() + Send>;

/// Как часто поток без заданий обрабатывает оконные сообщения (буфер обмена, UIA).
const PUMP_INTERVAL: Duration = Duration::from_millis(30);
/// Дольше задание не ждём: ввод длинного куска укладывается с большим запасом.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

static SENDER: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();

fn sender() -> Option<mpsc::Sender<Job>> {
    SENDER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let spawned = thread::Builder::new()
                .name("reever-input".into())
                .spawn(move || run(rx));
            if let Err(error) = spawned {
                eprintln!("[reever] input thread did not start: {error}");
            }
            Mutex::new(tx)
        })
        .lock()
        .ok()
        .map(|tx| tx.clone())
}

fn run(rx: mpsc::Receiver<Job>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    loop {
        pump();
        match rx.recv_timeout(PUMP_INTERVAL) {
            // Паника в задании не должна убить поток: следующие вызовы продолжат работать.
            Ok(job) => {
                let _ = catch_unwind(AssertUnwindSafe(job));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn pump() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Выполнить `f` в потоке ввода и дождаться результата. `None` — поток недоступен,
/// задание упало или не уложилось в тайм-аут.
pub fn call<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    let (tx, rx) = mpsc::sync_channel(1);
    sender()?
        .send(Box::new(move || {
            let _ = tx.send(f());
        }))
        .ok()?;
    rx.recv_timeout(CALL_TIMEOUT).ok()
}

/// Поставить `f` в очередь потока ввода, не дожидаясь (печать по ходу речи не держит
/// аудио-воркер). `false` — поток недоступен.
pub fn post(f: impl FnOnce() + Send + 'static) -> bool {
    sender().is_some_and(|tx| tx.send(Box::new(f)).is_ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn runs_jobs_on_a_separate_thread_and_survives_panics() {
        let caller = std::thread::current().id();
        let worker = super::call(|| std::thread::current().id()).unwrap();
        assert_ne!(caller, worker);
        assert_eq!(super::call(|| -> u8 { panic!("boom") }), None);
        assert_eq!(super::call(|| 7), Some(7));
    }
}
