//! Throwaway probe: what WASAPI itself offers here beyond what cpal uses.
//! Opens a device in exclusive mode only when its name is given as an
//! argument, and plays silence.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

const FORMATS: [(u16, u16, bool, &str); 5] = [
    (32, 32, true, "f32"),
    (32, 32, false, "i32"),
    (32, 24, false, "i24in32"),
    (24, 24, false, "i24"),
    (16, 16, false, "i16"),
];

fn extensible(rate: u32, bits: u16, valid: u16, float: bool) -> WAVEFORMATEXTENSIBLE {
    let block = 2 * bits / 8;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: valid },
        dwChannelMask: 3,
        SubFormat: if float { KSDATAFORMAT_SUBTYPE_IEEE_FLOAT } else { KSDATAFORMAT_SUBTYPE_PCM },
    }
}

fn main() -> windows::core::Result<()> {
    let exclusive_on: Vec<String> = std::env::args().skip(1).collect();
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for index in 0..devices.GetCount()? {
            let device = devices.Item(index)?;
            let name = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?.to_string();
            println!("\n{name}");
            let client: IAudioClient3 = device.Activate(CLSCTX_ALL, None)?;
            let mix = client.GetMixFormat()?;
            let m = *mix;
            let (rate, channels, bits) = (m.nSamplesPerSec, m.nChannels, m.wBitsPerSample);
            println!("  mix format: {rate} Hz, {channels} ch, {bits} bit");
            let (mut default, mut fundamental, mut min, mut max) = (0u32, 0u32, 0u32, 0u32);
            match client.GetSharedModeEnginePeriod(mix, &mut default, &mut fundamental, &mut min, &mut max) {
                Ok(()) => println!(
                    "  shared engine period: default {default}, fundamental {fundamental}, min {min}, max {max} frames"
                ),
                Err(e) => println!("  GetSharedModeEnginePeriod failed: {e}"),
            }
            let (mut def_period, mut min_period) = (0i64, 0i64);
            client.GetDevicePeriod(Some(&mut def_period), Some(&mut min_period))?;
            println!("  device period: default {} ms, minimum {} ms", def_period as f64 / 1e4, min_period as f64 / 1e4);

            // Low-latency shared mode: harmless to others.
            if min > 0 && min < default {
                match client.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min, mix, None) {
                    Ok(()) => {
                        let buffer = client.GetBufferSize()?;
                        let latency = client.GetStreamLatency()?;
                        println!(
                            "  low-latency shared: period {min} frames, buffer {buffer} frames, stream latency {} ms",
                            latency as f64 / 1e4
                        );
                    }
                    Err(e) => println!("  low-latency shared refused: {e}"),
                }
            }

            // Which formats exclusive mode would take (asking opens nothing).
            let mut usable = Vec::new();
            for rate in [44_100u32, 48_000, 88_200, 96_000] {
                for (bits, valid, float, label) in FORMATS {
                    let format = extensible(rate, bits, valid, float);
                    let fresh: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                    if fresh.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &format.Format, None).is_ok() {
                        usable.push(format!("{rate}/{label}"));
                    }
                }
            }
            println!("  exclusive formats: {}", usable.join(" "));

            if exclusive_on.iter().any(|wanted| name.contains(wanted.as_str())) {
                for (bits, valid, float, label) in FORMATS {
                    let format = extensible(rate, bits, valid, float);
                    let fresh: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                    if fresh.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &format.Format, None).is_err() {
                        continue;
                    }
                    for period in [min_period, 30_000, 50_000, 100_000] {
                        let fresh: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                        let opened = fresh.Initialize(
                            AUDCLNT_SHAREMODE_EXCLUSIVE,
                            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                            period,
                            period,
                            &format.Format,
                            None,
                        );
                        match opened {
                            Ok(()) => {
                                let buffer = fresh.GetBufferSize()?;
                                let latency = fresh.GetStreamLatency()?;
                                let event: HANDLE = CreateEventW(None, false, false, None)?;
                                fresh.SetEventHandle(event)?;
                                let render: IAudioRenderClient = fresh.GetService()?;
                                let bytes = buffer as usize * format.Format.nBlockAlign as usize;
                                let data = render.GetBuffer(buffer)?;
                                std::ptr::write_bytes(data, 0, bytes);
                                render.ReleaseBuffer(buffer, 0)?;
                                fresh.Start()?;
                                let started = std::time::Instant::now();
                                let mut wakes = 0;
                                while started.elapsed().as_millis() < 500 {
                                    if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                                        break;
                                    }
                                    let data = render.GetBuffer(buffer)?;
                                    std::ptr::write_bytes(data, 0, bytes);
                                    render.ReleaseBuffer(buffer, 0)?;
                                    wakes += 1;
                                }
                                fresh.Stop()?;
                                println!(
                                    "  exclusive {label} period {} ms: buffer {buffer} frames ({:.2} ms), stream latency {} ms, {wakes} wakes in 500 ms",
                                    period as f64 / 1e4,
                                    buffer as f64 * 1000.0 / rate as f64,
                                    latency as f64 / 1e4
                                );
                            }
                            Err(e) => println!("  exclusive {label} period {} ms refused: {e}", period as f64 / 1e4),
                        }
                    }
                    break;
                }
            }
        }
    }
    Ok(())
}
