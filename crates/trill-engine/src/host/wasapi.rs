//! A Windows output device taken for this program alone ("exclusive mode").
//!
//! A device shared with other programs plays through the system's mixer,
//! which works in periods of 10 ms and keeps a buffer or two ahead: some
//! 30 ms between a key and its sound, whatever buffer size is asked for.
//! Taken exclusively, the device plays this program's buffers directly, at
//! the period asked for, down to about 3 ms on ordinary hardware. The price
//! is that no other program can use the device meanwhile.
//!
//! One thread does everything for a stream: it opens the device, runs the
//! render loop at the system's "Pro Audio" priority, and closes the device
//! when the stream is dropped. Nothing of the stream is ever touched from
//! another thread.
//!
//! An input is taken the same way ([`open_exclusive_input`]): where the
//! output plays this program's buffers directly, the input should hand
//! its own over as directly, or what is saved on the way out is spent on
//! the way in.

use super::{
    FailureCallback, InputFormat, InputRequest, OpenError, OpenInput, OpenStream, Renderer, StreamFailure,
    StreamFormat, StreamRequest, Timing, now_ns,
};
use crate::capture::{Missing, Said, Sink};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, S_OK, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
    AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_EXCLUSIVE,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioCaptureClient, IAudioClient, IAudioClock, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::HSTRING;

/// The buffer asked for when the user names none: about 5 ms, short enough
/// to play on and long enough for an ordinary machine to keep up with.
const DEFAULT_BUFFER_SECONDS: f64 = 0.005;
/// A device that has not asked for a buffer for this long has stalled.
const STALL_MS: u32 = 2_000;
/// How long the thread may take to open the device.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// How samples are laid out in the device's buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Layout {
    Float32,
    Int32,
    /// 24 bits at the top of 32.
    Int24In32,
    /// 24 bits in three bytes.
    Int24,
    Int16,
}

impl Layout {
    /// Tried in this order: the device's native resolution matters less
    /// than getting one it takes at all.
    const ALL: [Layout; 5] = [Layout::Float32, Layout::Int32, Layout::Int24In32, Layout::Int24, Layout::Int16];

    /// Container bits, valid bits.
    fn bits(self) -> (u16, u16) {
        match self {
            Layout::Float32 | Layout::Int32 => (32, 32),
            Layout::Int24In32 => (32, 24),
            Layout::Int24 => (24, 24),
            Layout::Int16 => (16, 16),
        }
    }

    fn describe(self, rate: u32, channels: u16, mask: u32) -> WAVEFORMATEXTENSIBLE {
        let (container, valid) = self.bits();
        let block = channels * container / 8;
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: channels,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * block as u32,
                nBlockAlign: block,
                wBitsPerSample: container,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: valid },
            dwChannelMask: mask,
            SubFormat: if self == Layout::Float32 { KSDATAFORMAT_SUBTYPE_IEEE_FLOAT } else { KSDATAFORMAT_SUBTYPE_PCM },
        }
    }

    /// Writes one sample at `out`, which has room for a container.
    ///
    /// # Safety
    ///
    /// `out` must be valid for writing the layout's container size.
    #[inline]
    unsafe fn write(self, out: *mut u8, sample: f32) {
        // The engine hands over samples within -1..=1. To the nearest
        // step, not the one towards nothing: cut off, a sample is half a
        // step out on average and always the same way, which is twice the
        // noise of rounding and bends what is quiet. (Measured through a
        // cable of 16 bits: what was played came back 67.3 dB clean cut
        // off, and to the bit rounded, where it lay on the cable's steps.)
        unsafe {
            match self {
                Layout::Float32 => out.cast::<f32>().write_unaligned(sample),
                Layout::Int32 => out.cast::<i32>().write_unaligned((sample as f64 * 2_147_483_647.0).round() as i32),
                Layout::Int24In32 => out.cast::<i32>().write_unaligned(((sample * 8_388_607.0).round() as i32) << 8),
                Layout::Int24 => {
                    let value = (sample * 8_388_607.0).round() as i32;
                    out.copy_from_nonoverlapping(value.to_le_bytes().as_ptr(), 3);
                }
                Layout::Int16 => out.cast::<i16>().write_unaligned((sample * 32_767.0).round() as i16),
            }
        }
    }

    /// Reads one sample at `at`, as a device that records hands it over.
    ///
    /// # Safety
    ///
    /// `at` must be valid for reading the layout's container size.
    #[inline]
    unsafe fn read(self, at: *const u8) -> f32 {
        unsafe {
            match self {
                Layout::Float32 => at.cast::<f32>().read_unaligned(),
                Layout::Int32 => (at.cast::<i32>().read_unaligned() as f64 / 2_147_483_648.0) as f32,
                Layout::Int24In32 => (at.cast::<i32>().read_unaligned() >> 8) as f32 / 8_388_608.0,
                Layout::Int24 => {
                    let mut bytes = [0u8; 4];
                    // Into the top three bytes, so that the sign comes down
                    // with the shift.
                    at.copy_to_nonoverlapping(bytes.as_mut_ptr().add(1), 3);
                    (i32::from_le_bytes(bytes) >> 8) as f32 / 8_388_608.0
                }
                Layout::Int16 => f32::from(at.cast::<i16>().read_unaligned()) / 32_768.0,
            }
        }
    }
}

/// Initializes COM on this thread for as long as it lives.
struct Com;

impl Com {
    fn new() -> Self {
        // SAFETY: paired with the `CoUninitialize` in `drop`. A thread that
        // already has COM in another mode keeps it; the call then fails
        // and the pairing still holds, as COM documents.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: see `new`.
        unsafe { CoUninitialize() };
    }
}

/// An open device, ready to start.
struct Device {
    client: IAudioClient,
    render: IAudioRenderClient,
    clock: Option<(IAudioClock, u64)>,
    event: HANDLE,
    layout: Layout,
    channels: usize,
    block: usize,
    sample_rate: u32,
    buffer_frames: u32,
    stream_latency_frames: f64,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the client and the event are this struct's own, live
        // until here.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

fn refused(name: &str, error: &windows::core::Error) -> OpenError {
    let code = error.code();
    if code == AUDCLNT_E_DEVICE_IN_USE {
        OpenError::Refused(format!("'{name}' is in use by another program, so it cannot be taken exclusively"))
    } else if code == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
        OpenError::Refused(format!(
            "Windows does not let programs take '{name}' exclusively (Sound settings, the device's properties, Advanced)"
        ))
    } else if code == AUDCLNT_E_UNSUPPORTED_FORMAT {
        OpenError::Refused(format!("'{name}' refused the format"))
    } else if code == AUDCLNT_E_DEVICE_INVALIDATED {
        OpenError::NotFound(name.to_string())
    } else {
        OpenError::Failed(format!("'{name}' could not be opened exclusively: {error}"))
    }
}

/// Opens the endpoint `endpoint` exclusively. Runs on the stream's thread.
fn open(endpoint: &str, name: &str, request: &StreamRequest) -> Result<Device, OpenError> {
    let failed = |error: windows::core::Error| refused(name, &error);
    // SAFETY: plain COM calls on objects this function creates and owns;
    // every pointer handed over points to a live local.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(failed)?;
        let device: IMMDevice =
            enumerator.GetDevice(&HSTRING::from(endpoint)).map_err(|_| OpenError::NotFound(name.to_string()))?;
        let activate = || device.Activate::<IAudioClient>(CLSCTX_ALL, None).map_err(failed);
        let mut client = activate()?;

        // What the system runs the device at when it is shared: the rate to
        // use unasked, and how its channels are laid out.
        let mix = client.GetMixFormat().map_err(failed)?;
        let (mix_rate, mix_channels) = ((*mix).nSamplesPerSec, (*mix).nChannels);
        let mix_mask = if (*mix).wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16 && (*mix).cbSize >= 22 {
            (*mix.cast::<WAVEFORMATEXTENSIBLE>()).dwChannelMask
        } else {
            0
        };
        CoTaskMemFree(Some(mix as *const _));
        let sample_rate = request.sample_rate.unwrap_or(mix_rate);

        // Stereo if the device takes it, else all the channels it has.
        let mut shapes = vec![(2u16, 3u32)];
        if mix_channels > 2 {
            let mask = if mix_mask != 0 { mix_mask } else { (1u32 << mix_channels.min(31)) - 1 };
            shapes.push((mix_channels, mask));
        }
        let accepted = shapes
            .iter()
            .flat_map(|&(channels, mask)| Layout::ALL.into_iter().map(move |layout| (layout, channels, mask)))
            .find(|&(layout, channels, mask)| {
                let format = layout.describe(sample_rate, channels, mask);
                client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &format.Format, None) == S_OK
            });
        let Some((layout, channels, mask)) = accepted else {
            return Err(OpenError::Refused(format!(
                "'{name}' takes no format Trill writes at {sample_rate} Hz when it is opened exclusively"
            )));
        };
        let format = layout.describe(sample_rate, channels, mask);

        let (mut default_period, mut minimum_period) = (0i64, 0i64);
        client.GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period)).map_err(failed)?;
        let wanted = match request.buffer_size {
            Some(frames) => frames as f64,
            // A power of two, as the sizes offered for choosing are.
            None => (DEFAULT_BUFFER_SECONDS * sample_rate as f64).max(1.0).log2().ceil().exp2(),
        };
        let to_period = |frames: f64| (frames * 1e7 / sample_rate as f64).round() as i64;
        let mut period = to_period(wanted).max(minimum_period);

        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        let mut opened = client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &format.Format, None);
        if let Err(error) = &opened
            && error.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED
        {
            // The device wants a buffer of whole hardware packets and says
            // how many frames that makes; a client can be set up only once.
            let aligned = client.GetBufferSize().map_err(failed)?;
            period = (1e7 * aligned as f64 / sample_rate as f64 + 0.5) as i64;
            client = activate()?;
            opened = client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &format.Format, None);
        }
        opened.map_err(failed)?;

        let event = CreateEventW(None, false, false, None).map_err(failed)?;
        // From here the event is closed by `Device`'s drop on every path.
        let render = match client.SetEventHandle(event).and_then(|()| client.GetService::<IAudioRenderClient>()) {
            Ok(render) => render,
            Err(error) => {
                let _ = CloseHandle(event);
                return Err(refused(name, &error));
            }
        };
        let clock = client.GetService::<IAudioClock>().ok().and_then(|clock| {
            let frequency = clock.GetFrequency().ok().filter(|frequency| *frequency > 0)?;
            Some((clock, frequency))
        });
        let mut device = Device {
            client,
            render,
            clock,
            event,
            layout,
            channels: channels as usize,
            block: format.Format.nBlockAlign as usize,
            sample_rate,
            buffer_frames: 0,
            stream_latency_frames: 0.0,
        };
        device.buffer_frames = device.client.GetBufferSize().map_err(failed)?;
        let latency = device.client.GetStreamLatency().unwrap_or(0).max(0);
        device.stream_latency_frames = latency as f64 * sample_rate as f64 / 1e7;
        Ok(device)
    }
}

/// Fills the device's buffers until told to stop or the device fails.
fn run(device: &Device, mut renderer: Renderer, on_failure: &FailureCallback, quit: &AtomicBool) {
    let frames = device.buffer_frames;
    let (layout, channels, block) = (device.layout, device.channels, device.block);
    let container = layout.bits().0 as usize / 8;
    let fail = |error: windows::core::Error| {
        let lost = error.code() == AUDCLNT_E_DEVICE_INVALIDATED;
        log::warn!("exclusive audio stream ended: {error}");
        on_failure(if lost { StreamFailure::DeviceLost } else { StreamFailure::Failed });
    };
    // The latency, smoothed; see `system::Callback`.
    let mut latency: Option<f64> = None;
    let mut written: u64 = 0;

    // SAFETY: the device's objects are live for the whole call and used on
    // this thread only. `GetBuffer(frames)` yields `frames * block` bytes,
    // all of which are written (first with silence, then with samples)
    // before `ReleaseBuffer`.
    unsafe {
        // The device starts by playing a buffer, so it gets one of silence.
        match device.render.GetBuffer(frames) {
            Ok(data) => {
                std::ptr::write_bytes(data, 0, frames as usize * block);
                let _ = device.render.ReleaseBuffer(frames, 0);
                written += frames as u64;
            }
            Err(error) => return fail(error),
        }
        if let Err(error) = device.client.Start() {
            return fail(error);
        }

        while !quit.load(Ordering::Relaxed) {
            match WaitForSingleObject(device.event, STALL_MS) {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => {
                    log::warn!("the exclusive audio device has stopped asking for audio");
                    on_failure(StreamFailure::Stalled);
                    return;
                }
                _ => {
                    on_failure(StreamFailure::Failed);
                    return;
                }
            }
            if quit.load(Ordering::Relaxed) {
                break;
            }
            let data = match device.render.GetBuffer(frames) {
                Ok(data) => data,
                Err(error) => return fail(error),
            };

            // What is queued ahead of this buffer, plus what the device
            // says it adds itself.
            let mut ahead = frames as f64 + device.stream_latency_frames;
            if let Some((clock, frequency)) = &device.clock {
                let mut position = 0u64;
                if clock.GetPosition(&mut position, None).is_ok() {
                    let played = position as f64 * device.sample_rate as f64 / *frequency as f64;
                    ahead = (written as f64 - played).max(0.0) + device.stream_latency_frames;
                }
            }
            let smoothed = match latency {
                Some(latency) => latency + (ahead - latency) * 0.1,
                None => ahead,
            };
            latency = Some(smoothed);
            let timing = Timing { now_ns: now_ns(), latency_frames: smoothed.round().max(0.0) as u32 };

            std::ptr::write_bytes(data, 0, frames as usize * block);
            let mut done = 0usize;
            renderer.render(frames as usize, timing, &mut |left, right| {
                let mut out = data.add(done * block);
                for (l, r) in left.iter().zip(right) {
                    if channels == 1 {
                        layout.write(out, (l + r) * 0.5);
                    } else {
                        layout.write(out, *l);
                        layout.write(out.add(container), *r);
                    }
                    out = out.add(block);
                }
                done += left.len();
            });
            if let Err(error) = device.render.ReleaseBuffer(frames, 0) {
                return fail(error);
            }
            written += frames as u64;
        }
    }
}

/// Opens the output with the endpoint id `endpoint` for this program alone
/// and starts playing through `renderer`. `name` is what the device is
/// listed as.
pub(super) fn open_exclusive(
    endpoint: &str,
    name: &str,
    request: &StreamRequest,
    renderer: Renderer,
    on_failure: FailureCallback,
) -> Result<OpenStream, OpenError> {
    let quit = Arc::new(AtomicBool::new(false));
    let (opened, result) = mpsc::channel();
    let (endpoint, device_name, request, stop) =
        (endpoint.to_string(), name.to_string(), request.clone(), quit.clone());
    let thread = std::thread::Builder::new()
        .name("audio-exclusive".into())
        .spawn(move || {
            let _com = Com::new();
            let device = match open(&endpoint, &device_name, &request) {
                Ok(device) => device,
                Err(error) => {
                    let _ = opened.send(Err(error));
                    return;
                }
            };
            let format = StreamFormat {
                device: device_name,
                sample_rate: device.sample_rate,
                channels: device.channels as u16,
                buffer_frames: device.buffer_frames,
                latency_frames: (device.buffer_frames as f64 + device.stream_latency_frames).round() as u32,
                exclusive: true,
            };
            if opened.send(Ok(format)).is_err() {
                return;
            }
            // (Given up when this thread ends.)
            if !super::priority::AudioPriority::raise_for_good() {
                log::warn!("the audio thread was not given audio priority; expect drop-outs under load");
            }
            run(&device, renderer, &on_failure, &stop);
        })
        .map_err(|error| OpenError::Failed(format!("cannot start a thread: {error}")))?;

    match result.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok(format)) => Ok(OpenStream { format, handle: Box::new(Stream { quit, thread: Some(thread) }) }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        // The thread is left to itself: if the device ever answers, the
        // thread finds nobody listening and closes it again. Its answer
        // may also have got through at the very moment this gave up on
        // it: then it finds it has been told to stop before it plays.
        Err(_) => {
            quit.store(true, Ordering::Relaxed);
            Err(OpenError::Failed(format!("'{name}' did not answer")))
        }
    }
}

/// The handle to an exclusive stream: dropping it stops the thread, which
/// closes the device on its way out.
struct Stream {
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The thread looks at the flag every time the device asks for a
            // buffer, and at the latest when it gives up on a stalled one.
            let _ = thread.join();
        }
    }
}

/// Whether the endpoints with the ids `one` and `other` are of one piece
/// of hardware: the system keeps, with every endpoint, which device it
/// belongs to. Two that cannot be asked are taken to be two devices.
pub(super) fn same_hardware(one: &str, other: &str) -> bool {
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::STGM_READ;
    use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
    use windows::Win32::System::Variant::VT_CLSID;
    // The device an endpoint belongs to, by the system's name for it.
    const CONTAINER: PROPERTYKEY =
        PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c), pid: 2 };
    let _com = Com::new();
    let container = |endpoint: &str| -> Option<u128> {
        // SAFETY: plain COM calls on objects this closure creates and
        // owns; the value read is copied out before it is cleared.
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let device: IMMDevice = enumerator.GetDevice(&HSTRING::from(endpoint)).ok()?;
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let mut value = store.GetValue(&CONTAINER).ok()?;
            let inner = &value.Anonymous.Anonymous;
            let found =
                (inner.vt == VT_CLSID && !inner.Anonymous.puuid.is_null()).then(|| (*inner.Anonymous.puuid).to_u128());
            let _ = PropVariantClear(&mut value);
            found
        }
    };
    // The system gives every device that is built in, or that it knows
    // no container for, one and the same id: two endpoints that both have
    // it are any two devices, and that is no evidence of one clock.
    let says_something = |container: &u128| !is_no_container(*container);
    let (one, other) = (container(one).filter(says_something), container(other).filter(says_something));
    matches!((one, other), (Some(one), Some(other)) if one == other)
}

/// Whether a container id is the one that stands for "no container":
/// `{00000000-0000-0000-FFFF-FFFFFFFFFFFF}`, or nothing at all.
fn is_no_container(container: u128) -> bool {
    container == 0 || container == 0x0000_0000_0000_0000_ffff_ffff_ffff_ffff
}

/// An open input device, ready to start.
struct Capture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    layout: Layout,
    channels: usize,
    block: usize,
    sample_rate: u32,
    buffer_frames: u32,
    stream_latency_frames: f64,
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: the client and the event are this struct's own, live
        // until here.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Opens the input endpoint `endpoint` exclusively, with all the channels
/// it has: at the rate asked for if it takes that, at its own otherwise
/// (what comes in is brought to the engine's rate whatever it is). Runs on
/// the stream's thread.
fn open_capture(endpoint: &str, name: &str, request: &InputRequest) -> Result<Capture, OpenError> {
    let failed = |error: windows::core::Error| refused(name, &error);
    // SAFETY: plain COM calls on objects this function creates and owns;
    // every pointer handed over points to a live local.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(failed)?;
        let device: IMMDevice =
            enumerator.GetDevice(&HSTRING::from(endpoint)).map_err(|_| OpenError::NotFound(name.to_string()))?;
        let activate = || device.Activate::<IAudioClient>(CLSCTX_ALL, None).map_err(failed);
        let mut client = activate()?;

        let mix = client.GetMixFormat().map_err(failed)?;
        let (mix_rate, mix_channels) = ((*mix).nSamplesPerSec, (*mix).nChannels);
        let mix_mask = if (*mix).wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16 && (*mix).cbSize >= 22 {
            (*mix.cast::<WAVEFORMATEXTENSIBLE>()).dwChannelMask
        } else {
            0
        };
        CoTaskMemFree(Some(mix as *const _));

        // Every channel the device has, as it lays them out; and failing
        // that two, or one.
        let all = if mix_mask != 0 { mix_mask } else { (1u32 << mix_channels.clamp(1, 31)) - 1 };
        let mut shapes = vec![(mix_channels.max(1), all)];
        for shape in [(2u16, 3u32), (1, 4)] {
            if !shapes.iter().any(|(channels, _)| *channels == shape.0) {
                shapes.push(shape);
            }
        }
        let mut rates = vec![request.sample_rate.unwrap_or(mix_rate)];
        if !rates.contains(&mix_rate) {
            rates.push(mix_rate);
        }
        let tries = rates.iter().flat_map(|&rate| {
            shapes.iter().flat_map(move |&(channels, mask)| {
                Layout::ALL.into_iter().map(move |layout| (rate, layout, channels, mask))
            })
        });
        let accepted = tries.into_iter().find(|&(rate, layout, channels, mask)| {
            let format = layout.describe(rate, channels, mask);
            client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &format.Format, None) == S_OK
        });
        let Some((sample_rate, layout, channels, mask)) = accepted else {
            return Err(OpenError::Refused(format!(
                "'{name}' gives no format Trill reads when it is opened exclusively"
            )));
        };
        let format = layout.describe(sample_rate, channels, mask);

        let (mut default_period, mut minimum_period) = (0i64, 0i64);
        client.GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period)).map_err(failed)?;
        let wanted = match request.buffer_size {
            // As long as the output's buffer lasts, in this device's frames.
            Some(frames) => frames as f64 * sample_rate as f64 / request.sample_rate.unwrap_or(sample_rate) as f64,
            None => (DEFAULT_BUFFER_SECONDS * sample_rate as f64).max(1.0).log2().ceil().exp2(),
        };
        let mut period = ((wanted * 1e7 / sample_rate as f64).round() as i64).max(minimum_period);
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        let mut opened = client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &format.Format, None);
        if let Err(error) = &opened
            && error.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED
        {
            let aligned = client.GetBufferSize().map_err(failed)?;
            period = (1e7 * aligned as f64 / sample_rate as f64 + 0.5) as i64;
            client = activate()?;
            opened = client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &format.Format, None);
        }
        opened.map_err(failed)?;

        let event = CreateEventW(None, false, false, None).map_err(failed)?;
        // From here the event is closed by `Capture`'s drop on every path.
        let capture = match client.SetEventHandle(event).and_then(|()| client.GetService::<IAudioCaptureClient>()) {
            Ok(capture) => capture,
            Err(error) => {
                let _ = CloseHandle(event);
                return Err(refused(name, &error));
            }
        };
        let mut device = Capture {
            client,
            capture,
            event,
            layout,
            channels: channels as usize,
            block: format.Format.nBlockAlign as usize,
            sample_rate,
            buffer_frames: 0,
            stream_latency_frames: 0.0,
        };
        device.buffer_frames = device.client.GetBufferSize().map_err(failed)?;
        let latency = device.client.GetStreamLatency().unwrap_or(0).max(0);
        device.stream_latency_frames = latency as f64 * sample_rate as f64 / 1e7;
        Ok(device)
    }
}

/// Hands the device's buffers to `sink` until told to stop or the device
/// fails.
fn run_capture(device: &Capture, mut sink: Sink, on_failure: &FailureCallback, quit: &AtomicBool) {
    let (layout, channels, block) = (device.layout, device.channels.min(sink.channels()), device.block);
    let container = layout.bits().0 as usize / 8;
    let latency_ns = (device.stream_latency_frames * 1e9 / device.sample_rate as f64) as u64;
    let fail = |error: windows::core::Error| {
        let lost = error.code() == AUDCLNT_E_DEVICE_INVALIDATED;
        log::warn!("exclusive audio input ended: {error}");
        on_failure(if lost { StreamFailure::DeviceLost } else { StreamFailure::Failed });
    };
    // SAFETY: the device's objects are live for the whole call and used on
    // this thread only. `GetBuffer` yields `frames * block` bytes that stay
    // valid until `ReleaseBuffer`, and nothing is read past them.
    unsafe {
        let mut frequency = 0i64;
        let _ = QueryPerformanceFrequency(&mut frequency);
        if let Err(error) = device.client.Start() {
            return fail(error);
        }
        // Where the device's own count of frames will be at the buffer
        // after the one last handed over, if it hands them all over.
        let mut follows: Option<u64> = None;
        while !quit.load(Ordering::Relaxed) {
            match WaitForSingleObject(device.event, STALL_MS) {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => {
                    log::warn!("the exclusive audio input has stopped handing over audio");
                    on_failure(StreamFailure::Stalled);
                    return;
                }
                _ => {
                    on_failure(StreamFailure::Failed);
                    return;
                }
            }
            if quit.load(Ordering::Relaxed) {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags, mut position, mut stamp) = (0u32, 0u32, 0u64, 0u64);
            let got =
                device.capture.GetBuffer(&mut data, &mut frames, &mut flags, Some(&mut position), Some(&mut stamp));
            if let Err(error) = got {
                return fail(error);
            }
            if frames == 0 || data.is_null() {
                continue;
            }
            let flagged = |flag: u32| flags & flag != 0;
            // Frames are missing before this buffer when the device says
            // it does not follow the one before (of the first buffer of
            // all the flag says nothing); how many, by its own count of
            // the frames it has heard, where that has gone on further
            // than what it handed over.
            let breaks = flagged(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32);
            let missing = match follows {
                Some(follows) if breaks => match position.checked_sub(follows) {
                    Some(gone) if gone > 0 => Missing::Frames(gone),
                    _ => Missing::Uncounted,
                },
                _ => Missing::Nothing,
            };
            follows = Some(position.saturating_add(u64::from(frames)));
            // The buffer's first frame was heard when the counter stood
            // at `stamp` (in ten-millionths of a second), less what the
            // device says lies before that: so long before now. A stamp
            // the device says is wrong, the counter's zero and a moment
            // that has not come are no stamp.
            let mut counter = 0i64;
            let _ = QueryPerformanceCounter(&mut counter);
            let now = counter.max(0) as u128 * 10_000_000 / frequency.max(1) as u128;
            let stands = !flagged(AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) && stamp != 0 && stamp as u128 <= now;
            let heard_ns = stands.then(|| {
                let age_ns = now.saturating_sub(stamp as u128) as u64 * 100 + latency_ns;
                now_ns().saturating_sub(age_ns)
            });
            let silent = flagged(AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
            sink.push_said(Said { heard_ns, missing }, frames as usize, |frame, channel| {
                if silent || channel >= channels {
                    return 0.0;
                }
                layout.read(data.add(frame * block + channel * container))
            });
            if let Err(error) = device.capture.ReleaseBuffer(frames) {
                return fail(error);
            }
        }
    }
}

/// Opens the input with the endpoint id `endpoint` for this program alone
/// and starts taking what it hears. `name` is what the device is listed
/// as.
pub(super) fn open_exclusive_input(
    endpoint: &str,
    name: &str,
    request: &InputRequest,
    on_failure: FailureCallback,
) -> Result<OpenInput, OpenError> {
    let quit = Arc::new(AtomicBool::new(false));
    let (opened, result) = mpsc::channel();
    let (endpoint, device_name, request, stop) =
        (endpoint.to_string(), name.to_string(), request.clone(), quit.clone());
    let thread = std::thread::Builder::new()
        .name("audio-input-exclusive".into())
        .spawn(move || {
            let _com = Com::new();
            let device = match open_capture(&endpoint, &device_name, &request) {
                Ok(device) => device,
                Err(error) => {
                    let _ = opened.send(Err(error));
                    return;
                }
            };
            let format = InputFormat {
                device: device_name,
                sample_rate: device.sample_rate,
                channels: device.channels as u16,
                buffer_frames: device.buffer_frames,
                latency_frames: device.stream_latency_frames.round() as u32,
                exclusive: true,
                shares_clock: false,
            };
            let (sink, link) = crate::capture::link(device.sample_rate as f64, device.channels);
            if opened.send(Ok((format, link))).is_err() {
                return;
            }
            if !super::priority::AudioPriority::raise_for_good() {
                log::warn!("the input's thread was not given audio priority; expect drop-outs under load");
            }
            run_capture(&device, sink, &on_failure, &stop);
        })
        .map_err(|error| OpenError::Failed(format!("cannot start a thread: {error}")))?;

    match result.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok((format, link))) => {
            Ok(OpenInput { format, handle: Box::new(Stream { quit, thread: Some(thread) }), link })
        }
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        // An answer that got through at the very moment this gave up on it
        // would leave the thread taking what the device hears with nobody
        // to tell it to stop, and the device open until the program ends:
        // it is told now, and closes the device the moment it begins.
        Err(_) => {
            quit.store(true, Ordering::Relaxed);
            Err(OpenError::Failed(format!("'{name}' did not answer")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_every_built_in_device_has_says_nothing_of_two_endpoints_being_one_device() {
        // `{00000000-0000-0000-FFFF-FFFFFFFFFFFF}`: what the system gives
        // a device it knows no container for.
        let none = windows::core::GUID::from_values(0, 0, 0, [0xff; 8]).to_u128();
        assert!(is_no_container(none) && is_no_container(0));
        let some = windows::core::GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c).to_u128();
        assert!(!is_no_container(some));
    }

    fn read_back(layout: Layout, sample: f32) -> f32 {
        let mut bytes = [0u8; 4];
        // SAFETY: four bytes hold any container.
        unsafe {
            layout.write(bytes.as_mut_ptr(), sample);
            layout.read(bytes.as_ptr())
        }
    }

    #[test]
    fn samples_are_read_in_each_layout_a_device_may_hand_over() {
        for layout in Layout::ALL {
            for sample in [0.0f32, 0.5, -0.5, 0.25, -1.0, 0.999] {
                let back = read_back(layout, sample);
                let step = match layout.bits().1 {
                    16 => 2.0 / 32_768.0,
                    _ => 1e-6,
                };
                assert!((back - sample).abs() <= step, "{layout:?}: {sample} came back as {back}");
            }
        }
    }

    fn written(layout: Layout, sample: f32) -> Vec<u8> {
        let mut bytes = [0u8; 4];
        // SAFETY: four bytes hold any container.
        unsafe { layout.write(bytes.as_mut_ptr(), sample) };
        bytes[..layout.bits().0 as usize / 8].to_vec()
    }

    #[test]
    fn samples_are_written_in_each_layout_the_device_may_want() {
        assert_eq!(written(Layout::Float32, 0.5), 0.5f32.to_le_bytes());
        assert_eq!(written(Layout::Int16, 1.0), 32_767i16.to_le_bytes());
        assert_eq!(written(Layout::Int16, -1.0), (-32_767i16).to_le_bytes());
        assert_eq!(written(Layout::Int24, 1.0), [0xFF, 0xFF, 0x7F]);
        assert_eq!(written(Layout::Int24, -1.0), [0x01, 0x00, 0x80]);
        assert_eq!(written(Layout::Int24In32, 1.0), [0x00, 0xFF, 0xFF, 0x7F]);
        assert_eq!(written(Layout::Int32, 1.0), i32::MAX.to_le_bytes());
        assert_eq!(written(Layout::Int32, -1.0), (-i32::MAX).to_le_bytes());
        for layout in Layout::ALL {
            assert!(written(layout, 0.0).iter().all(|byte| *byte == 0), "{layout:?}");
        }
    }

    #[test]
    fn formats_are_described_as_the_device_expects() {
        let format = Layout::Int24In32.describe(48_000, 2, 3);
        let (block, bits, valid) =
            (format.Format.nBlockAlign, format.Format.wBitsPerSample, unsafe { format.Samples.wValidBitsPerSample });
        assert_eq!((block, bits, valid), (8, 32, 24));
        let packed = Layout::Int24.describe(44_100, 2, 3);
        let (block, bytes) = (packed.Format.nBlockAlign, packed.Format.nAvgBytesPerSec);
        assert_eq!((block, bytes), (6, 44_100 * 6));
    }
}
