//! Read-only audio-device probe. Queries the default input format without opening a stream.

use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no default input device")?;
    let supported = device.default_input_config()?;
    let config: cpal::StreamConfig = supported.config();

    println!("device={}", device.name()?);
    println!("sample_rate_hz={}", supported.sample_rate().0);
    println!("channels={}", supported.channels());
    println!("sample_format={:?}", supported.sample_format());
    println!("stream_config={config:?}");
    println!("stream_started=false (no capture opened)");
    Ok(())
}
