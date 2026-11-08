//! Звук компьютера («ривер слушай», listen.rs): петлевой захват WASAPI с устройства вывода
//! по умолчанию — то, что слышно в колонках или наушниках, без микрофона.
//!
//! Пока ничего не играет, Windows пакетов не присылает; нарезке на фразы нужна и тишина
//! (иначе последняя фраза не кончится), поэтому паузы досыпаются нулями по часам.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

use crate::audio::{audio_channel, AudioPacket, CaptureProcessor};
use crate::rawmic::{parse_format, Sample};

const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 2;
/// Буфер WASAPI, 100 нс: 200 мс.
const BUFFER_HNS: i64 = 2_000_000;
/// Пакетов нет дольше — это тишина, досыпать нулями.
const GAP: Duration = Duration::from_millis(60);

/// Идущий захват; дроп останавливает его.
pub struct Loopback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Начать захват: звук 16 кГц mono приходит пакетами в возвращённый приёмник.
pub fn start() -> anyhow::Result<(Loopback, Receiver<AudioPacket>)> {
    let (tx, rx) = audio_channel(256);
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = sync_channel::<anyhow::Result<()>>(1);
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new().name("reever-loopback".into()).spawn(move || {
        if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
            let _ = ready_tx.send(Err(error.into()));
            return;
        }
        let result = unsafe { capture(tx, &thread_stop, &ready_tx) };
        if let Err(error) = result {
            let _ = ready_tx.try_send(Err(error));
        }
        unsafe { CoUninitialize() };
    })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok((Loopback { stop, thread: Some(thread) }, rx)),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => Err(anyhow::anyhow!("loopback thread exited")),
    }
}

unsafe fn capture(
    tx: crate::audio::AudioSender,
    stop: &AtomicBool,
    ready: &std::sync::mpsc::SyncSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let parsed = parse_format(format);
    let initialized = parsed
        .as_ref()
        .ok()
        .map(|_| client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, BUFFER_HNS, 0, format, None));
    CoTaskMemFree(Some(format as *const _));
    let (rate, channels, sample) = parsed?;
    initialized.expect("format parsed")?;
    let capture: IAudioCaptureClient = client.GetService()?;
    client.Start()?;
    let _ = ready.send(Ok(()));

    let mut processor = CaptureProcessor::new(rate, channels, tx);
    let mut zeros: Vec<f32> = Vec::new();
    // до какого момента звук уже отдан (пакетами или нулями)
    let mut covered = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
        let mut got = false;
        while capture.GetNextPacketSize()? > 0 {
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let n = frames as usize * channels;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                zeros.clear();
                zeros.resize(n, 0.0);
                processor.push_f32(&zeros);
            } else {
                match sample {
                    Sample::F32 => processor.push_f32(std::slice::from_raw_parts(data as *const f32, n)),
                    Sample::I16 => processor.push_i16(std::slice::from_raw_parts(data as *const i16, n)),
                }
            }
            capture.ReleaseBuffer(frames)?;
            got = true;
        }
        let now = Instant::now();
        if got {
            covered = now;
        } else if now.duration_since(covered) >= GAP {
            // ничего не играет: столько же тишины, сколько прошло
            let frames = (now.duration_since(covered).as_secs_f64() * rate as f64) as usize;
            zeros.clear();
            zeros.resize(frames * channels, 0.0);
            processor.push_f32(&zeros);
            covered = now;
        }
    }
    let _ = client.Stop();
    Ok(())
}
