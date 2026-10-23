use std::cell::UnsafeCell;
use std::fs::File;
use std::io::BufReader;
use std::ops::{Deref, DerefMut};
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

// Флаг `occupied` передаёт слот между callback-производителем и worker-потребителем.
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
                // SAFETY: успешный переход состояния даёт этому writer исключительный доступ.
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

impl DerefMut for AudioPacket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let slot = &self.pool.slots[self.index];
        // SAFETY: слот занят только этим пакетом, производитель его не трогает до Drop;
        // изменяемый доступ нужен, чтобы занулить звук после распознавания.
        unsafe { (&mut *slot.samples.get()).as_mut_slice() }
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
        // SAFETY: этот writer единолично владеет занятым слотом.
        unsafe { (&*slot.samples.get()).as_slice() }
    }

    fn samples_mut(&mut self) -> &mut Vec<i16> {
        let slot = &self.pool.as_ref().unwrap().slots[self.index];
        // SAFETY: этот writer единолично владеет занятым слотом.
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
            if self
                .writer
                .as_ref()
                .is_some_and(|writer| writer.samples().len() == OUTPUT_BUFFER_SAMPLES)
            {
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
            writer.samples_mut().extend(
                samples[offset..offset + count]
                    .iter()
                    .map(|&sample| f32_to_i16(sample)),
            );
            offset += count;
        }
    }

    fn append_iter<I: Iterator<Item = f32>>(&mut self, mut samples: I) {
        loop {
            if self
                .writer
                .as_ref()
                .is_some_and(|writer| writer.samples().len() == OUTPUT_BUFFER_SAMPLES)
            {
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
        let Some(writer) = self.writer.take() else {
            return;
        };
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
            self.in_block[self.in_len..self.in_len + copied].copy_from_slice(&mono[..copied]);
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

    fn push_converted<I: Iterator<Item = f32>>(&mut self, mono: I, out: &mut CallbackOutput<'_>) {
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
            let mut outputs = [&mut *output];
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

/// Поток микрофона умер: Windows пересоздаёт устройство, когда меняют его улучшения звука
/// (эквалайзер, «Голосовая ясность»), формат или устройство по умолчанию. Callback после
/// этого не приходит; recognizer.rs по этому флагу (или по тишине без callback'ов)
/// открывает микрофон заново.
pub static STREAM_FAILED: AtomicBool = AtomicBool::new(false);

fn on_stream_error(e: cpal::StreamError) {
    eprintln!("[reever] cpal stream error: {e}");
    STREAM_FAILED.store(true, Ordering::SeqCst);
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
                frame
                    .iter()
                    .map(|&sample| sample as f32 / 32768.0)
                    .sum::<f32>()
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
    STREAM_FAILED.store(false, Ordering::SeqCst);
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no input device (microphone)"))?;
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
        other => {
            return Err(anyhow::anyhow!(
                "unsupported sample format: {other:?}"
            ))
        }
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
                eprintln!("[reever] sound not found {}: {e}", path.display());
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

#[cfg(test)]
mod tests {
    use super::{audio_channel, CaptureProcessor, DROPPED_SAMPLES, TARGET_RATE};
    use rubato::{FftFixedInOut, Resampler};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Receiver;

    fn make_test_wav(sample_rate: usize, channels: usize, pcm: &[i16]) -> Vec<u8> {
        let data_len = (pcm.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&(channels as u16).to_le_bytes());
        wav.extend_from_slice(&(sample_rate as u32).to_le_bytes());
        wav.extend_from_slice(&((sample_rate * channels * 2) as u32).to_le_bytes());
        wav.extend_from_slice(&((channels * 2) as u16).to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in pcm {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    fn decode_test_wav(wav: &[u8]) -> (usize, usize, Vec<i16>) {
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1);
        let channels = u16::from_le_bytes([wav[22], wav[23]]) as usize;
        let sample_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap()) as usize;
        let bits_per_sample = u16::from_le_bytes([wav[34], wav[35]]);
        assert_eq!(bits_per_sample, 16);
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        let pcm = wav[44..44 + data_len]
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        (sample_rate, channels, pcm)
    }

    fn old_mono<T: Copy>(data: &[T], channels: usize, convert: impl Fn(T) -> f32) -> Vec<f32> {
        data.chunks(channels)
            .map(|frame| frame.iter().copied().map(&convert).sum::<f32>() / channels as f32)
            .collect()
    }

    fn old_resampler(sample_rate: usize, mono: &[f32]) -> Vec<i16> {
        if sample_rate == TARGET_RATE {
            return mono
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
        }

        let mut resampler = FftFixedInOut::<f32>::new(sample_rate, TARGET_RATE, 1024, 1).unwrap();
        let need = resampler.input_frames_next();
        let mut in_buf = Vec::new();
        let mut output = Vec::new();
        in_buf.extend_from_slice(mono);
        while in_buf.len() >= need {
            let block: Vec<f32> = in_buf.drain(..need).collect();
            let result = resampler.process(&[block], None).unwrap();
            output.extend(
                result[0]
                    .iter()
                    .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16),
            );
        }
        output
    }

    fn collect_packets(rx: &Receiver<super::AudioPacket>, output: &mut Vec<i16>) {
        while let Ok(packet) = rx.try_recv() {
            output.extend_from_slice(&packet);
        }
    }

    fn run_capture<T: Copy>(
        sample_rate: usize,
        channels: usize,
        input: &[T],
        push: fn(&mut CaptureProcessor, &[T]),
    ) -> Vec<i16> {
        let (sender, rx) = audio_channel(128);
        let mut capture = CaptureProcessor::new(sample_rate, channels, sender);
        let frame_count = input.len() / channels;
        let callback_frames = [480, 257, 701, 1_024];
        let mut output = Vec::new();
        let mut offset = 0;
        let mut callback_index = 0;
        while offset < frame_count {
            let frames =
                callback_frames[callback_index % callback_frames.len()].min(frame_count - offset);
            let start = offset * channels;
            let end = (offset + frames) * channels;
            push(&mut capture, &input[start..end]);
            collect_packets(&rx, &mut output);
            offset += frames;
            callback_index += 1;
        }
        output
    }

    #[test]
    fn resampler_output_matches_previous_path_for_test_wavs() {
        for sample_rate in [44_100, 48_000] {
            for channels in [1, 2] {
                let mut source_pcm = Vec::with_capacity(5_000 * channels);
                for frame in 0..5_000i32 {
                    let left = ((frame * 97 % 30_001) - 15_000) as i16;
                    source_pcm.push(left);
                    if channels == 2 {
                        source_pcm.push(-left / 2);
                    }
                }
                let (wav_rate, wav_channels, pcm) =
                    decode_test_wav(&make_test_wav(sample_rate, channels, &source_pcm));
                assert_eq!((wav_rate, wav_channels), (sample_rate, channels));

                let float_input: Vec<f32> =
                    pcm.iter().map(|&sample| sample as f32 / 32768.0).collect();
                let expected = old_resampler(
                    sample_rate,
                    &old_mono(&float_input, channels, |sample| sample),
                );
                assert_eq!(
                    run_capture(
                        sample_rate,
                        channels,
                        &float_input,
                        CaptureProcessor::push_f32
                    ),
                    expected,
                    "F32, {sample_rate} Hz, {channels} channels"
                );

                let expected = old_resampler(
                    sample_rate,
                    &old_mono(&pcm, channels, |sample| sample as f32 / 32768.0),
                );
                assert_eq!(
                    run_capture(sample_rate, channels, &pcm, CaptureProcessor::push_i16),
                    expected,
                    "I16, {sample_rate} Hz, {channels} channels"
                );

                let unsigned_pcm: Vec<u16> = pcm
                    .iter()
                    .map(|&sample| (sample as i32 + 32_768) as u16)
                    .collect();
                let expected = old_resampler(
                    sample_rate,
                    &old_mono(&unsigned_pcm, channels, |sample| {
                        (sample as f32 - 32_768.0) / 32_768.0
                    }),
                );
                assert_eq!(
                    run_capture(
                        sample_rate,
                        channels,
                        &unsigned_pcm,
                        CaptureProcessor::push_u16
                    ),
                    expected,
                    "U16, {sample_rate} Hz, {channels} channels"
                );
            }
        }
    }

    #[test]
    fn full_queue_drops_samples_and_updates_the_existing_counter() {
        let (sender, _rx) = audio_channel(0);
        let mut capture = CaptureProcessor::new(TARGET_RATE, 1, sender);
        let input = vec![0.25; 1_024];
        let before = DROPPED_SAMPLES.load(Ordering::Relaxed);
        capture.push_f32(&input);
        let after = DROPPED_SAMPLES.load(Ordering::Relaxed);
        assert_eq!(after - before, input.len());
    }
}
