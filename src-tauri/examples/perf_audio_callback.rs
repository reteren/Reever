//! Сравнивает старый путь захвата с CaptureProcessor на синтетических callback-буферах.
//! Микрофон и устройство ввода не открываются.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::{Duration, Instant};

use reever_lib::audio::{audio_channel, AudioPacket, CaptureProcessor};
use rubato::{FftFixedInOut, Resampler};

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn count_allocation(bytes: usize) {
    if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
        ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
struct Measurement {
    calls: usize,
    bytes: usize,
    elapsed: Duration,
    audio_seconds: f64,
    iterations: usize,
}

impl Measurement {
    fn print(&self, label: &str, sample_rate: usize, frames: usize) {
        let callback_rate = self.iterations as f64 / self.audio_seconds;
        println!(
            "{label}; rate={sample_rate}; callback_frames={frames}; simulated_callback_hz={callback_rate:.1}; alloc_calls_per_callback={:.3}; alloc_bytes_per_audio_second={:.0}; alloc_calls_per_audio_second={:.1}; cpu_ns_per_callback={:.1}; elapsed_ms={:.1}",
            self.calls as f64 / self.iterations as f64,
            self.bytes as f64 / self.audio_seconds,
            self.calls as f64 / self.audio_seconds,
            self.elapsed.as_nanos() as f64 / self.iterations as f64,
            self.elapsed.as_secs_f64() * 1_000.0,
        );
    }
}

fn begin_measurement() {
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
}

fn end_measurement(
    started: Instant,
    iterations: usize,
    sample_rate: usize,
    frames: usize,
) -> Measurement {
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    Measurement {
        calls: ALLOC_CALLS.load(Ordering::Relaxed),
        bytes: ALLOC_BYTES.load(Ordering::Relaxed),
        elapsed: started.elapsed(),
        audio_seconds: iterations as f64 * frames as f64 / sample_rate as f64,
        iterations,
    }
}

struct OldResampler {
    inner: Option<FftFixedInOut<f32>>,
    in_buf: Vec<f32>,
    need: usize,
    passthrough: bool,
}

impl OldResampler {
    fn new(sample_rate: usize) -> Self {
        if sample_rate == 16_000 {
            return Self {
                inner: None,
                in_buf: Vec::new(),
                need: 0,
                passthrough: true,
            };
        }
        let inner = FftFixedInOut::<f32>::new(sample_rate, 16_000, 1024, 1).unwrap();
        let need = inner.input_frames_next();
        Self {
            inner: Some(inner),
            in_buf: Vec::new(),
            need,
            passthrough: false,
        }
    }

    fn push(&mut self, mono: &[f32], output: &mut Vec<i16>) {
        if self.passthrough {
            output.extend(mono.iter().map(|&sample| f32_to_i16(sample)));
            return;
        }
        self.in_buf.extend_from_slice(mono);
        let resampler = self.inner.as_mut().unwrap();
        while self.in_buf.len() >= self.need {
            let block: Vec<f32> = self.in_buf.drain(..self.need).collect();
            if let Ok(result) = resampler.process(&[block], None) {
                output.extend(result[0].iter().map(|&sample| f32_to_i16(sample)));
            }
        }
    }
}

struct OldCapture {
    channels: usize,
    mono: Vec<f32>,
    output: Vec<i16>,
    resampler: OldResampler,
    tx: SyncSender<Vec<i16>>,
    rx: Receiver<Vec<i16>>,
}

impl OldCapture {
    fn new(sample_rate: usize, channels: usize) -> Self {
        let (tx, rx) = sync_channel(32);
        Self {
            channels,
            mono: Vec::new(),
            output: Vec::new(),
            resampler: OldResampler::new(sample_rate),
            tx,
            rx,
        }
    }

    fn push_f32(&mut self, data: &[f32]) {
        let f32buf: Vec<f32> = data.iter().map(|&sample| sample).collect();
        self.mono.clear();
        if self.channels <= 1 {
            self.mono.extend_from_slice(&f32buf);
        } else {
            for frame in f32buf.chunks(self.channels) {
                self.mono
                    .push(frame.iter().sum::<f32>() / self.channels as f32);
            }
        }
        self.output.clear();
        self.resampler.push(&self.mono, &mut self.output);
        if !self.output.is_empty() {
            let _ = self.tx.try_send(self.output.clone());
        }
        while self.rx.try_recv().is_ok() {}
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

fn drain_packets(rx: &Receiver<AudioPacket>) {
    while let Ok(packet) = rx.try_recv() {
        drop(packet);
    }
}

fn measure_new<T: Copy>(
    sample_rate: usize,
    channels: usize,
    frames: usize,
    iterations: usize,
    input: &[T],
    push: fn(&mut CaptureProcessor, &[T]),
) -> Measurement {
    let (sender, rx) = audio_channel(32);
    let mut capture = CaptureProcessor::new(sample_rate, channels, sender);
    for _ in 0..1_000 {
        push(&mut capture, input);
        drain_packets(&rx);
    }
    begin_measurement();
    let started = Instant::now();
    for _ in 0..iterations {
        push(&mut capture, input);
        drain_packets(&rx);
    }
    end_measurement(started, iterations, sample_rate, frames)
}

fn measure_old(
    sample_rate: usize,
    channels: usize,
    frames: usize,
    iterations: usize,
) -> Measurement {
    let mut capture = OldCapture::new(sample_rate, channels);
    let input: Vec<f32> = (0..frames * channels)
        .map(|index| {
            let signed = (index as i32 * 97 % 30_001) - 15_000;
            signed as f32 / 32_768.0
        })
        .collect();
    for _ in 0..1_000 {
        capture.push_f32(&input);
    }
    begin_measurement();
    let started = Instant::now();
    for _ in 0..iterations {
        capture.push_f32(&input);
    }
    end_measurement(started, iterations, sample_rate, frames)
}

fn main() {
    let iterations = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000usize);

    println!(
        "input=synthetic; microphone_opened=false; callbacks={iterations}; warmup_callbacks=1000"
    );
    let old_rate = 48_000;
    let old_frames = 480;
    measure_old(old_rate, 2, old_frames, iterations).print("path=before", old_rate, old_frames);

    for sample_rate in [44_100, 48_000] {
        let frames = sample_rate / 100;
        for channels in [1, 2] {
            let floats: Vec<f32> = (0..frames * channels)
                .map(|index| {
                    let signed = (index as i32 * 97 % 30_001) - 15_000;
                    signed as f32 / 32_768.0
                })
                .collect();
            let signed: Vec<i16> = floats
                .iter()
                .map(|&sample| (sample * 32_768.0) as i16)
                .collect();
            let unsigned: Vec<u16> = signed
                .iter()
                .map(|&sample| (sample as i32 + 32_768) as u16)
                .collect();

            measure_new(
                sample_rate,
                channels,
                frames,
                iterations,
                &floats,
                CaptureProcessor::push_f32,
            )
            .print(
                &format!("path=after; format=F32; channels={channels}"),
                sample_rate,
                frames,
            );
            measure_new(
                sample_rate,
                channels,
                frames,
                iterations,
                &signed,
                CaptureProcessor::push_i16,
            )
            .print(
                &format!("path=after; format=I16; channels={channels}"),
                sample_rate,
                frames,
            );
            measure_new(
                sample_rate,
                channels,
                frames,
                iterations,
                &unsigned,
                CaptureProcessor::push_u16,
            )
            .print(
                &format!("path=after; format=U16; channels={channels}"),
                sample_rate,
                frames,
            );
        }
    }
}
