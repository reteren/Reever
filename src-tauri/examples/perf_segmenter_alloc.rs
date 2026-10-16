//! Counts allocator calls while feeding quiet packets through the production segmenter.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use reever_lib::{segmenter::{Segmenter, SegmenterConfig}, speech::VoiceDetector};

static ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(new_size, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct FixedVad(bool);

impl VoiceDetector for FixedVad {
    fn frame_len(&self) -> usize {
        512
    }

    fn is_speech(&mut self, _frame: &[i16]) -> bool {
        self.0
    }

    fn reset(&mut self) {}
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let iterations = args
        .get(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000usize);
    let packet_samples = args
        .get(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(160usize);
    let speech_mode = args.get(3).is_some_and(|value| value == "speech");
    let packet = vec![if speech_mode { 2_000i16 } else { 0 }; packet_samples];
    let mut vad = FixedVad(speech_mode);
    let mut segmenter = Segmenter::new(SegmenterConfig::default());

    for _ in 0..1_000 {
        std::hint::black_box(segmenter.push(&packet, &mut vad));
    }
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);

    let started = Instant::now();
    let mut events_seen = 0usize;
    for _ in 0..iterations {
        let events = segmenter.push(&packet, &mut vad);
        events_seen += events.len();
        std::hint::black_box(events);
    }
    let elapsed = started.elapsed();
    let calls = ALLOC_CALLS.load(Ordering::Relaxed);
    let bytes = ALLOC_BYTES.load(Ordering::Relaxed);

    println!("mode={}; iterations={iterations}; packet_samples={packet_samples}; packet_ms={:.3}; vad_frame_samples=512; events_seen={events_seen}", if speech_mode { "synthetic_speech" } else { "quiet" }, packet_samples as f64 * 1_000.0 / 16_000.0);
    println!("elapsed_ms={:.3}; ns_per_packet={:.1}", elapsed.as_secs_f64() * 1_000.0, elapsed.as_nanos() as f64 / iterations as f64);
    println!("allocator_calls={calls}; calls_per_packet={:.3}", calls as f64 / iterations as f64);
    println!("requested_bytes={bytes}; requested_bytes_per_packet={:.1}", bytes as f64 / iterations as f64);
    println!("note=global allocator counts alloc+realloc requests in this single-threaded probe");
}
