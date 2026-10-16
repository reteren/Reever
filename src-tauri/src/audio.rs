use std::fs::File;
use std::io::BufReader;
use std::cell::UnsafeCell;
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
use rubato::{FftFixedInOut, Resampler};

const TARGET_RATE: usize = 16_000; // Vosk обучена на 16 кГц mono
const OUTPUT_BUFFER_SAMPLES: usize = 512;

static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
static CALLBACK_INPUT_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Сводка аудиоколбэков с момента последнего чтения. Звуковые данные не сохраняются.
#[derive(Clone, Copy, Debug, Default)]
pub struct CallbackStats {
    pub callbacks: u64,
    pub input_frames: u64,
}

pub fn take_callback_stats() -> CallbackStats {
    CallbackStats {
        callbacks: CALLBACK_COUNT.swap(0, Ordering::Relaxed),
        input_frames: CALLBACK_INPUT_FRAMES.swap(0, Ordering::Relaxed),
    }
}

struct AudioSlot {
    occupied: AtomicBool,
    samples: UnsafeCell<Vec<i16>>,
}

// `occupied` передаёт слот между единственным callback-производителем и worker-потребителем.
// Пока слот занят, его Vec доступен только владельцу PacketWriter или AudioPacket.
unsafe impl Sync for AudioSlot {}
unsafe impl Send for AudioSlot {}

struct AudioPool {
    slots: Box<[AudioSlot]>,
    next_slot: AtomicUsize,
}

impl AudioPool {
    fn new(queue_capacity: usize) -> Self {
        let slot_count = queue_capacity.saturating_add(1).max(2);
        let slots = (0..slot_count)
            .map(|_| AudioSlot {
                occupied: AtomicBool::new(false),
                samples: UnsafeCell::new(Vec::with_capacity(OUTPUT_BUFFER_SAMPLES)),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            next_slot: AtomicUsize::new(0),
        }
    }

    fn acquire(self: &Arc<Self>) -> Option<PacketWriter> {
        let len = self.slots.len();
        let first = self.next_slot.fetch_add(1, Ordering::Relaxed) % len;
        for offset in 0..len {
            let index = (first + offset) % len;
            let slot = &self.slots[index];
            if slot
                .occupied
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                // SAFETY: the successful state transition grants this writer exclusive access.
                unsafe { (&mut *slot.samples.get()).clear() };
                return Some(PacketWriter {
                    pool: Some(self.clone()),
                    index,
                });
            }
        }
        None
    }

    fn release(&self, index: usize) {
        self.slots[index].occupied.store(false, Ordering::Release);
    }
}

/// Один пакет в очереди микрофона. Drop возвращает заранее выделенный буфер в пул.
pub struct AudioPacket {
    pool: Arc<AudioPool>,
    index: usize,
}

impl Deref for AudioPacket {
    type Target = [i16];

    fn deref(&self) -> &Self::Target {
        let slot = &self.pool.slots[self.index];
        // SAFETY: occupied остаётся true, пока жив пакет, поэтому производитель не меняет Vec.
        unsafe { (&*slot.samples.get()).as_slice() }
    }
}

impl Drop for AudioPacket {
    fn drop(&mut self) {
        self.pool.release(self.index);
    }
}

struct PacketWriter {
    pool: Option<Arc<AudioPool>>,
    index: usize,
}

impl PacketWriter {
    fn samples(&self) -> &[i16] {
        let slot = &self.pool.as_ref().unwrap().slots[self.index];
        // SAFETY: this writer exclusively owns the occupied slot.
        unsafe { (&*slot.samples.get()).as_slice() }
    }

    fn samples_mut(&mut self) -> &mut Vec<i16> {
        let slot = &self.pool.as_ref().unwrap().slots[self.index];
        // SAFETY: this writer exclusively owns the occupied slot.
        unsafe { &mut *slot.samples.get() }
    }

    fn into_packet(mut self) -> AudioPacket {
        AudioPacket {
            pool: self.pool.take().unwrap(),
            index: self.index,
        }
    }
}

impl Drop for PacketWriter {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            pool.release(self.index);
        }
    }
}

/// Канал аудио с заранее выделенным пулом пакетов: callback не выделяет Vec на каждый чанк.
#[derive(Clone)]
pub struct AudioSender {
    tx: SyncSender<AudioPacket>,
    pool: Arc<AudioPool>,
}

pub fn audio_channel(queue_capacity: usize) -> (AudioSender, Receiver<AudioPacket>) {
    let (tx, rx) = sync_channel(queue_capacity);
    let pool = Arc::new(AudioPool::new(queue_capacity));
    (AudioSender { tx, pool }, rx)
}

impl AudioSender {
    fn acquire(&self) -> Option<PacketWriter> {
        self.pool.acquire()
    }

    fn try_send(&self, writer: PacketWriter) -> Result<(), usize> {
        let samples = writer.samples().len();
        match self.tx.try_send(writer.into_packet()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(packet) | TrySendError::Disconnected(packet)) => {
                drop(packet);
                Err(samples)
            }
        }
    }
}

struct CallbackOutput<'a> {
    sender: &'a AudioSender,
    writer: &'a mut Option<PacketWriter>,
    dropped: &'a mut usize,
}

impl CallbackOutput<'_> {
    fn append_f32(&mut self, samples: &[f32]) {
        let mut offset = 0;
        while offset < samples.len() {
            if self.writer.as_ref().is_some_and(|writer| writer.samples().len() == OUTPUT_BUFFER_SAMPLES) {
                self.flush();
            }
            if self.writer.is_none() {
                *self.writer = self.sender.acquire();
            }
            let Some(writer) = self.writer.as_mut() else {
                *self.dropped = self.dropped.saturating_add(samples.len() - offset);
                return;
            };
            let used = writer.samples().len();
            let count = (OUTPUT_BUFFER_SAMPLES - used).min(samples.len() - offset);
            writer
                .samples_mut()
                .extend(samples[offset..offset + count].iter().map(|&sample| f32_to_i16(sample)));
            offset += count;
        }
    }

    fn append_iter<I: Iterator<Item = f32>>(&mut self, mut samples: I) {
        loop {
            if self.writer.as_ref().is_some_and(|writer| writer.samples().len() == OUTPUT_BUFFER_SAMPLES) {
                self.flush();
            }
            if self.writer.is_none() {
                *self.writer = self.sender.acquire();
            }
            let Some(writer) = self.writer.as_mut() else {
                *self.dropped = self.dropped.saturating_add(samples.count());
                return;
            };
            let room = OUTPUT_BUFFER_SAMPLES - writer.samples().len();
            let mut written = 0;
            while written < room {
                let Some(sample) = samples.next() else { return };
                writer.samples_mut().push(f32_to_i16(sample));
                written += 1;
            }
        }
    }

    fn flush(&mut self) {
        let Some(writer) = self.writer.take() else { return };
        if !writer.samples().is_empty() {
            if let Err(samples) = self.sender.try_send(writer) {
                *self.dropped = self.dropped.saturating_add(samples);
            }
        }
    }
}

/// Потоковый ресемплер -> i16 @ 16 кГц mono. С фильтрацией (rubato FFT),
/// чтобы не было алиасинга, который убивает точность ASR.
struct StreamResampler {
    inner: Option<FftFixedInOut<f32>>,
    in_block: Vec<f32>,
    in_len: usize,
    out_block: Vec<f32>,
    need: usize,
    passthrough: bool,
}

impl StreamResampler {
    fn new(in_rate: usize) -> Self {
        if in_rate == TARGET_RATE {
            return Self {
                inner: None,
                in_block: Vec::new(),
                in_len: 0,
                out_block: Vec::new(),
                need: 0,
                passthrough: true,
            };
        }
        let inner = FftFixedInOut::<f32>::new(in_rate, TARGET_RATE, 1024, 1)
            .expect("failed to build resampler");
        let need = inner.input_frames_next();
        let output_len = inner.output_frames_max();
        Self {
            inner: Some(inner),
            in_block: vec![0.0; need],
            in_len: 0,
            out_block: vec![0.0; output_len],
            need,
            passthrough: false,
        }
    }

    fn push_f32_mono(&mut self, mono: &[f32], out: &mut CallbackOutput<'_>) {
        if self.passthrough {
            out.append_f32(mono);
            return;
        }

        let mut offset = 0;
        if self.in_len > 0 {
            let copied = (self.need - self.in_len).min(mono.len());
            self.in_block[self.in_len..self.in_len + copied]
                .copy_from_slice(&mono[..copied]);
            self.in_len += copied;
            offset = copied;
            if self.in_len == self.need {
                Self::process_block(
                    self.inner.as_mut().unwrap(),
                    &self.in_block,
                    &mut self.out_block,
                    out,
                );
                self.in_len = 0;
            }
        }

        while mono.len() - offset >= self.need {
            Self::process_block(
                self.inner.as_mut().unwrap(),
                &mono[offset..offset + self.need],
                &mut self.out_block,
                out,
            );
            offset += self.need;
        }
        let tail = &mono[offset..];
        if !tail.is_empty() {
            self.in_block[..tail.len()].copy_from_slice(tail);
            self.in_len = tail.len();
        }
    }

    fn push_converted<I: Iterator<Item = f32>>(
        &mut self,
        mono: I,
        out: &mut CallbackOutput<'_>,
    ) {
        if self.passthrough {
            out.append_iter(mono);
            return;
        }
        for sample in mono {
            self.in_block[self.in_len] = sample;
            self.in_len += 1;
            if self.in_len == self.need {
                Self::process_block(
                    self.inner.as_mut().unwrap(),
                    &self.in_block,
                    &mut self.out_block,
                    out,
                );
                self.in_len = 0;
            }
        }
    }

    fn process_block(
        resampler: &mut FftFixedInOut<f32>,
        input: &[f32],
        output: &mut [f32],
        out: &mut CallbackOutput<'_>,
    ) {
        let output_len = {
            let inputs = [input];
            let mut outputs = [output];
            resampler
                .process_into_buffer(&inputs, &mut outputs, None)
                .expect("preallocated resampler buffers have the expected size")
                .1
        };
        out.append_f32(&output[..output_len]);
    }
}

#[inline]
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

fn on_stream_error(e: cpal::StreamError) {
    eprintln!("[reever] cpal stream error: {e}");
}

/// Запускает захват микрофона. Возвращённый Stream НЕЛЬЗЯ дропать (иначе захват
/// остановится). Stream !Send -> держим его в worker-потоке.
/// Сэмплы 16 кГц, выброшенные из-за полной очереди (с последнего чтения).
pub static DROPPED_SAMPLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Небольшая оболочка над входным устройством; те же методы использует synthetic perf-проба.
#[doc(hidden)]
pub struct CaptureProcessor {
    resampler: StreamResampler,
    channels: usize,
    sender: AudioSender,
    writer: Option<PacketWriter>,
    dropped_samples: usize,
}

impl CaptureProcessor {
    #[doc(hidden)]
    pub fn new(input_rate: usize, channels: usize, sender: AudioSender) -> Self {
        Self {
            resampler: StreamResampler::new(input_rate),
            channels: channels.max(1),
            sender,
            writer: None,
            dropped_samples: 0,
        }
    }

    pub fn push_f32(&mut self, data: &[f32]) {
        self.begin_callback(data.len() / self.channels);
        let mut output = CallbackOutput {
            sender: &self.sender,
            writer: &mut self.writer,
            dropped: &mut self.dropped_samples,
        };
        if self.channels == 1 {
            self.resampler.push_f32_mono(data, &mut output);
        } else {
            let channels = self.channels;
            self.resampler.push_converted(
                data.chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32),
                &mut output,
            );
        }
        output.flush();
        drop(output);
        self.record_dropped_samples();
    }

    pub fn push_i16(&mut self, data: &[i16]) {
        self.begin_callback(data.len() / self.channels);
        let channels = self.channels;
        let mut output = CallbackOutput {
            sender: &self.sender,
            writer: &mut self.writer,
            dropped: &mut self.dropped_samples,
        };
        self.resampler.push_converted(
            data.chunks(channels).map(|frame| {
                frame.iter().map(|&sample| sample as f32 / 32768.0).sum::<f32>()
                    / channels as f32
            }),
            &mut output,
        );
        output.flush();
        drop(output);
        self.record_dropped_samples();
    }

    pub fn push_u16(&mut self, data: &[u16]) {
        self.begin_callback(data.len() / self.channels);
        let channels = self.channels;
        let mut output = CallbackOutput {
            sender: &self.sender,
            writer: &mut self.writer,
            dropped: &mut self.dropped_samples,
        };
        self.resampler.push_converted(
            data.chunks(channels).map(|frame| {
                frame
                    .iter()
                    .map(|&sample| (sample as f32 - 32768.0) / 32768.0)
                    .sum::<f32>()
                    / channels as f32
            }),
            &mut output,
        );
        output.flush();
        drop(output);
        self.record_dropped_samples();
    }

    fn begin_callback(&mut self, input_frames: usize) {
        CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
        CALLBACK_INPUT_FRAMES.fetch_add(input_frames as u64, Ordering::Relaxed);
        self.dropped_samples = 0;
    }

    fn record_dropped_samples(&self) {
        if self.dropped_samples > 0 {
            DROPPED_SAMPLES.fetch_add(self.dropped_samples, Ordering::Relaxed);
        }
    }
}

pub fn start_capture(tx: AudioSender) -> anyhow::Result<Stream> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("нет устройства ввода (микрофона)"))?;
    let supported = device.default_input_config()?;
    let in_rate = supported.sample_rate().0 as usize;
    let channels = supported.channels() as usize;
    let fmt = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();

    let mut processor = CaptureProcessor::new(in_rate, channels, tx);

    // realtime-callback: только конвертация + неблокирующий send. Никакой тяжёлой работы.
    // Очередь полна (распознавание не успевает) — кусок теряется; считаем, чтобы это было
    // видно в журнале, а не догадками.
    macro_rules! build {
        ($t:ty, $method:ident) => {
            device.build_input_stream(
                &config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    processor.$method(data);
                },
                on_stream_error,
                None,
            )?
        };
    }

    let stream = match fmt {
        SampleFormat::F32 => build!(f32, push_f32),
        SampleFormat::I16 => build!(i16, push_i16),
        SampleFormat::U16 => build!(u16, push_u16),
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
