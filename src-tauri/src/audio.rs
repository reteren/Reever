use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::mpsc::SyncSender;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
use rubato::{FftFixedInOut, Resampler};

const TARGET_RATE: usize = 16_000; // Vosk обучена на 16 кГц mono

/// Потоковый ресемплер -> i16 @ 16 кГц mono. С фильтрацией (rubato FFT),
/// чтобы не было алиасинга, который убивает точность ASR.
struct StreamResampler {
    inner: Option<FftFixedInOut<f32>>,
    in_buf: Vec<f32>,
    need: usize,
    passthrough: bool,
}

impl StreamResampler {
    fn new(in_rate: usize) -> Self {
        if in_rate == TARGET_RATE {
            return Self { inner: None, in_buf: Vec::new(), need: 0, passthrough: true };
        }
        let inner = FftFixedInOut::<f32>::new(in_rate, TARGET_RATE, 1024, 1)
            .expect("failed to build resampler");
        let need = inner.input_frames_next();
        Self { inner: Some(inner), in_buf: Vec::new(), need, passthrough: false }
    }

    fn push(&mut self, mono: &[f32], out: &mut Vec<i16>) {
        if self.passthrough {
            out.extend(mono.iter().map(|&s| f32_to_i16(s)));
            return;
        }
        let r = self.inner.as_mut().unwrap();
        self.in_buf.extend_from_slice(mono);
        while self.in_buf.len() >= self.need {
            let block: Vec<f32> = self.in_buf.drain(..self.need).collect();
            if let Ok(res) = r.process(&[block], None) {
                out.extend(res[0].iter().map(|&s| f32_to_i16(s)));
            }
        }
    }
}

#[inline]
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Сведение в mono усреднением каналов.
fn to_mono(data: &[f32], channels: usize, scratch: &mut Vec<f32>) {
    scratch.clear();
    if channels <= 1 {
        scratch.extend_from_slice(data);
        return;
    }
    for frame in data.chunks(channels) {
        scratch.push(frame.iter().sum::<f32>() / channels as f32);
    }
}

fn on_stream_error(e: cpal::StreamError) {
    eprintln!("[reever] cpal stream error: {e}");
}

/// Запускает захват микрофона. Возвращённый Stream НЕЛЬЗЯ дропать (иначе захват
/// остановится). Stream !Send -> держим его в worker-потоке.
/// Сэмплы 16 кГц, выброшенные из-за полной очереди (с последнего чтения).
pub static DROPPED_SAMPLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn start_capture(tx: SyncSender<Vec<i16>>) -> anyhow::Result<Stream> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("нет устройства ввода (микрофона)"))?;
    let supported = device.default_input_config()?;
    let in_rate = supported.sample_rate().0 as usize;
    let channels = supported.channels() as usize;
    let fmt = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();

    let mut resampler = StreamResampler::new(in_rate);
    let mut mono: Vec<f32> = Vec::new();
    let mut out16: Vec<i16> = Vec::new();

    // realtime-callback: только конвертация + неблокирующий send. Никакой тяжёлой работы.
    // Очередь полна (распознавание не успевает) — кусок теряется; считаем, чтобы это было
    // видно в журнале, а не догадками.
    macro_rules! build {
        ($t:ty, $to_f32:expr) => {
            device.build_input_stream(
                &config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    let f32buf: Vec<f32> = data.iter().map($to_f32).collect();
                    to_mono(&f32buf, channels, &mut mono);
                    out16.clear();
                    resampler.push(&mono, &mut out16);
                    if !out16.is_empty() && tx.try_send(out16.clone()).is_err() {
                        DROPPED_SAMPLES.fetch_add(out16.len(), std::sync::atomic::Ordering::Relaxed);
                    }
                },
                on_stream_error,
                None,
            )?
        };
    }

    let stream = match fmt {
        SampleFormat::F32 => build!(f32, |&s| s),
        SampleFormat::I16 => build!(i16, |&s| s as f32 / 32768.0),
        SampleFormat::U16 => build!(u16, |&s| (s as f32 - 32768.0) / 32768.0),
        other => return Err(anyhow::anyhow!("неподдерживаемый формат сэмплов: {other:?}")),
    };
    stream.play()?;
    Ok(stream)
}

/// Аудио-плеер. OutputStream держим живым весь рантайм (иначе щелчки/латентность).
/// OutputStream !Send -> создаётся и живёт в worker-потоке.
pub struct Player {
    _stream: OutputStream,
    handle: OutputStreamHandle,
}

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        let (_stream, handle) =
            OutputStream::try_default().map_err(|e| anyhow::anyhow!("audio out: {e}"))?;
        Ok(Self { _stream, handle })
    }

    /// НЕблокирующее воспроизведение: звук играет в фоне, воркер сразу продолжает
    /// (иначе длинный звук задерживал бы реакцию — Reever ждал бы конца проигрывания).
    pub fn play(&self, path: &Path) {
        let file = match File::open(path) {
            Ok(f) => BufReader::new(f),
            Err(e) => {
                eprintln!("[reever] звук не найден {}: {e}", path.display());
                return;
            }
        };
        match Decoder::new(file) {
            Ok(source) => {
                if let Ok(sink) = Sink::try_new(&self.handle) {
                    sink.append(source);
                    sink.detach(); // играет до конца в фоне, не блокирует поток
                }
            }
            Err(e) => eprintln!("[reever] decode error {}: {e}", path.display()),
        }
    }
}
