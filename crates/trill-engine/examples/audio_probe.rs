//! Throwaway probe: what cpal offers on this machine. Plays silence only.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn main() {
    println!("hosts available: {:?}", cpal::available_hosts());
    let host = cpal::default_host();
    println!("default host: {:?}", host.id());
    let default_out = host.default_output_device().and_then(|d| d.id().ok()).map(|id| id.to_string());
    let default_in = host.default_input_device().and_then(|d| d.id().ok()).map(|id| id.to_string());
    println!("default output id: {default_out:?}\ndefault input id: {default_in:?}");

    for device in host.devices().unwrap() {
        let id = device.id().map(|id| id.to_string()).unwrap_or_default();
        let description = device.description();
        println!("\ndevice {id}\n  {description:?}");
        println!("  out={} in={}", device.supports_output(), device.supports_input());
        if device.supports_output() {
            println!("  default out: {:?}", device.default_output_config());
            if let Ok(configs) = device.supported_output_configs() {
                for config in configs {
                    println!("    {config:?}");
                }
            }
        }
        if device.supports_input() {
            println!("  default in: {:?}", device.default_input_config());
        }
    }

    let Some(device) = host.default_output_device() else { return };
    let supported = device.default_output_config().unwrap();
    println!("\nopening the default output with different buffer sizes (silence)");
    for request in [None, Some(64u32), Some(128), Some(256), Some(480), Some(512), Some(1024), Some(2048)] {
        let mut config: cpal::StreamConfig = supported.into();
        config.buffer_size = request.map_or(cpal::BufferSize::Default, cpal::BufferSize::Fixed);
        let callbacks = Arc::new(AtomicU64::new(0));
        let frames = Arc::new(AtomicU64::new(0));
        let largest = Arc::new(AtomicU64::new(0));
        let latency = Arc::new(AtomicU64::new(0));
        let (c, f, l, lat) = (callbacks.clone(), frames.clone(), largest.clone(), latency.clone());
        let channels = config.channels as u64;
        let stream = device.build_output_stream(
            config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
                let n = data.len() as u64 / channels;
                c.fetch_add(1, Ordering::Relaxed);
                f.fetch_add(n, Ordering::Relaxed);
                l.fetch_max(n, Ordering::Relaxed);
                let stamp = info.timestamp();
                let ahead = stamp.playback.duration_since(stamp.callback);
                lat.store(ahead.as_micros() as u64, Ordering::Relaxed);
            },
            |error| eprintln!("    stream error: {error} ({:?})", error.kind()),
            Some(Duration::from_secs(3)),
        );
        match stream {
            Err(error) => println!("  request {request:?}: refused: {error} ({:?})", error.kind()),
            Ok(stream) => {
                stream.play().unwrap();
                std::thread::sleep(Duration::from_millis(700));
                let (c, f) = (callbacks.load(Ordering::Relaxed), frames.load(Ordering::Relaxed));
                println!(
                    "  request {request:?}: buffer_size()={:?} callbacks={c} mean frames={:.1} largest={} playback-callback={:.2} ms",
                    stream.buffer_size(),
                    f as f64 / c.max(1) as f64,
                    largest.load(Ordering::Relaxed),
                    latency.load(Ordering::Relaxed) as f64 / 1000.0
                );
            }
        }
    }
}
