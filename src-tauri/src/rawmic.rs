//! Микрофон в «сыром» режиме WASAPI (AUDCLNT_STREAMOPTIONS_RAW): звук до улучшений
//! Windows — эквалайзера, «Голосовой ясности», шумодава драйвера. Эти эффекты включены
//! почти у всех и ломают детектор «ривер»: шумодав глушит паузы в цифровой ноль и
//! съедает начало слова после тишины, команды распознаются хуже. Discord и звонки
//! по-прежнему получают обработанный звук — сырой режим только у Reever.
//!
//! cpal сырого режима не умеет, поэтому захват свой: поток с COM, событийный WASAPI в общем
//! режиме, формат микшера. Устройство без сырого режима или ошибка — `start` возвращает
//! ошибку, и audio.rs берёт обычный захват через cpal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::thread::JoinHandle;

use windows::core::GUID;
use windows::Win32::Foundation::{CloseHandle, BOOL, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, AudioCategory_Other, AudioClientProperties, IAudioCaptureClient,
    IAudioClient2, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMOPTIONS_RAW, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::audio::{AudioSender, CaptureProcessor, STREAM_FAILED};

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 2;
/// Буфер WASAPI, 100 нс: 100 мс.
const BUFFER_HNS: i64 = 1_000_000;

#[derive(Clone, Copy)]
enum Sample {
    F32,
    I16,
}

/// Идущий захват; дроп останавливает его (индикатор микрофона Windows гаснет).
pub struct RawCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for RawCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Микрофон по умолчанию в сыром режиме.
pub fn start(tx: AudioSender) -> anyhow::Result<RawCapture> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = sync_channel::<anyhow::Result<()>>(1);
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("reever-rawmic".into())
        .spawn(move || {
            if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
                let _ = ready_tx.send(Err(error.into()));
                return;
            }
            let result = unsafe { capture(tx, &thread_stop, &ready_tx) };
            if let Err(error) = result {
                // До запуска — ошибка уходит в start (ready_tx ещё ждёт); после — поток умер.
                if ready_tx.try_send(Err(error)).is_err() {
                    STREAM_FAILED.store(true, Ordering::SeqCst);
                }
            }
            unsafe { CoUninitialize() };
        })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(RawCapture { stop, thread: Some(thread) }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => Err(anyhow::anyhow!("raw microphone thread exited")),
    }
}

unsafe fn capture(
    tx: AudioSender,
    stop: &AtomicBool,
    ready: &std::sync::mpsc::SyncSender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?;
    let client: IAudioClient2 = device.Activate(CLSCTX_ALL, None)?;
    let properties = AudioClientProperties {
        cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
        bIsOffload: BOOL(0),
        eCategory: AudioCategory_Other,
        Options: AUDCLNT_STREAMOPTIONS_RAW,
    };
    client.SetClientProperties(&properties)?;

    let format = client.GetMixFormat()?;
    let parsed = parse_format(format);
    let initialized = parsed.as_ref().ok().map(|_| {
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, BUFFER_HNS, 0, format, None)
    });
    CoTaskMemFree(Some(format as *const _));
    let (rate, channels, sample) = parsed?;
    initialized.expect("format parsed")?;

    let event = CreateEventW(None, false, false, None)?;
    let result = (|| -> anyhow::Result<()> {
        client.SetEventHandle(event)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let _ = ready.send(Ok(()));
        let mut processor = CaptureProcessor::new(rate, channels, tx);
        let mut silence: Vec<f32> = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                continue;
            }
            loop {
                if capture.GetNextPacketSize()? == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let samples = frames as usize * channels;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                    silence.clear();
                    silence.resize(samples, 0.0);
                    processor.push_f32(&silence);
                } else {
                    match sample {
                        Sample::F32 => processor.push_f32(std::slice::from_raw_parts(data as *const f32, samples)),
                        Sample::I16 => processor.push_i16(std::slice::from_raw_parts(data as *const i16, samples)),
                    }
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
        let _ = client.Stop();
        Ok(())
    })();
    let _ = CloseHandle(event);
    result
}

/// Частота, каналы и тип сэмплов формата микшера (float32 или int16).
unsafe fn parse_format(format: *const WAVEFORMATEX) -> anyhow::Result<(usize, usize, Sample)> {
    let base = std::ptr::read_unaligned(format);
    let (tag, bits) = (base.wFormatTag, base.wBitsPerSample);
    let sample = match tag {
        WAVE_FORMAT_IEEE_FLOAT if bits == 32 => Sample::F32,
        WAVE_FORMAT_PCM if bits == 16 => Sample::I16,
        WAVE_FORMAT_EXTENSIBLE => {
            let extensible = std::ptr::read_unaligned(format as *const WAVEFORMATEXTENSIBLE);
            let subtype = extensible.SubFormat;
            match (subtype, bits) {
                (SUBTYPE_FLOAT, 32) => Sample::F32,
                (SUBTYPE_PCM, 16) => Sample::I16,
                _ => anyhow::bail!("unsupported mix format: subtype {subtype:?}, {bits} bit"),
            }
        }
        _ => anyhow::bail!("unsupported mix format: tag {tag}, {bits} bit"),
    };
    Ok((base.nSamplesPerSec as usize, base.nChannels as usize, sample))
}
