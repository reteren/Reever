//! Захват звука компьютера (loopback.rs) на `SECONDS` (по умолчанию 3) с: сколько звука пришло
//! и его громкость — проверка, что поток идёт и паузы досыпаются тишиной.
//!   cargo run --release --example loopback_probe

use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::var("SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let (capture, rx) = reever_lib::loopback::start()?;
    let t = Instant::now();
    let (mut n, mut peak, mut sum) = (0usize, 0i32, 0f64);
    while t.elapsed() < Duration::from_secs(seconds) {
        if let Ok(p) = rx.recv_timeout(Duration::from_millis(100)) {
            for &s in p.iter() {
                peak = peak.max((s as i32).abs());
                sum += (s as f64) * (s as f64);
            }
            n += p.len();
        }
    }
    drop(capture);
    let secs = t.elapsed().as_secs_f64();
    let rms = if n > 0 { (sum / n as f64).sqrt() } else { 0.0 };
    println!("{n} samples in {secs:.1} s = {:.0} per s (16000 expected), peak {peak}, rms {rms:.0}", n as f64 / secs);
    Ok(())
}
