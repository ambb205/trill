//! The machine's own audio devices.
//!
//! Devices are listed and, shared with other programs, opened through
//! cpal. Taking a device for this program alone is something cpal does not
//! offer on Windows, where it matters most (a shared device there plays
//! through the system's mixer, tens of milliseconds behind): that is done
//! by [`super::wasapi`].

use super::{
    AudioDevice, AudioDevices, AudioHost, COMMON_SAMPLE_RATES, FailureCallback, InputFormat, InputRequest, OpenError,
    OpenInput, OpenStream, Renderer, StreamFailure, StreamFormat, StreamRequest, Timing, now_ns, number_duplicates,
};
use crate::capture::{Missing, Said, Sink};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long cpal may take to bring a stream up.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a new stream is listened to before its latency is reported:
/// the figure settles once the device's buffer has filled.
const SETTLE_TIME: Duration = Duration::from_millis(250);
const SETTLE_CALLBACKS: u32 = 12;

/// The machine's audio devices.
#[derive(Default)]
pub struct SystemHost {
    /// See [`SystemHost::unheard`].
    unheard: bool,
}

impl SystemHost {
    pub fn new() -> Self {
        Self { unheard: false }
    }

    /// The machine's devices, played silence: every buffer a device asks
    /// for is made as it would be, and nothing of it is handed on. For
    /// measuring whether a song is played in time on the device its user
    /// plays through, without their hearing it. Such a host opens a device
    /// shared with the other programs and never takes it from them.
    pub fn unheard() -> Self {
        Self { unheard: true }
    }
}

/// A device with the name it is listed under.
struct Named {
    device: cpal::Device,
    name: String,
    is_default: bool,
}

fn name_of(device: &cpal::Device) -> String {
    device.description().map(|description| description.name().to_string()).unwrap_or_else(|_| "Unknown device".into())
}

fn named(devices: impl Iterator<Item = cpal::Device>, default: Option<cpal::Device>) -> Vec<Named> {
    let default = default.and_then(|device| device.id().ok());
    let devices: Vec<cpal::Device> = devices.collect();
    let mut names: Vec<String> = devices.iter().map(name_of).collect();
    number_duplicates(&mut names);
    devices
        .into_iter()
        .zip(names)
        .map(|(device, name)| {
            let is_default = default.is_some() && device.id().ok() == default;
            Named { device, name, is_default }
        })
        .collect()
}

fn outputs(host: &cpal::Host) -> Vec<Named> {
    match host.output_devices() {
        Ok(devices) => named(devices, host.default_output_device()),
        Err(error) => {
            log::warn!("cannot list the audio outputs: {error}");
            Vec::new()
        }
    }
}

fn inputs(host: &cpal::Host) -> Vec<Named> {
    match host.input_devices() {
        Ok(devices) => named(devices, host.default_input_device()),
        Err(error) => {
            log::warn!("cannot list the audio inputs: {error}");
            Vec::new()
        }
    }
}

/// What a device is listed as. `output` says which side of it to describe.
fn describe(named: &Named, output: bool) -> Option<AudioDevice> {
    let device = &named.device;
    let default = if output { device.default_output_config() } else { device.default_input_config() }.ok()?;
    let ranges: Vec<(u32, u32)> = if output {
        device
            .supported_output_configs()
            .map(|configs| configs.map(|c| (c.min_sample_rate(), c.max_sample_rate())).collect())
    } else {
        device
            .supported_input_configs()
            .map(|configs| configs.map(|c| (c.min_sample_rate(), c.max_sample_rate())).collect())
    }
    .unwrap_or_default();
    let mut sample_rates: Vec<u32> = COMMON_SAMPLE_RATES
        .into_iter()
        .filter(|rate| ranges.iter().any(|(min, max)| (*min..=*max).contains(rate)))
        .collect();
    if !sample_rates.contains(&default.sample_rate()) {
        sample_rates.push(default.sample_rate());
        sample_rates.sort_unstable();
    }
    Some(AudioDevice {
        name: named.name.clone(),
        is_default: named.is_default,
        default_sample_rate: default.sample_rate(),
        sample_rates,
        channels: default.channels(),
        exclusive: output && cfg!(windows),
    })
}

impl AudioHost for SystemHost {
    fn devices(&self) -> AudioDevices {
        let host = cpal::default_host();
        AudioDevices {
            outputs: outputs(&host).iter().filter_map(|named| describe(named, true)).collect(),
            inputs: inputs(&host).iter().filter_map(|named| describe(named, false)).collect(),
        }
    }

    fn open_output(
        &self,
        request: &StreamRequest,
        renderer: Renderer,
        on_failure: FailureCallback,
    ) -> Result<OpenStream, OpenError> {
        let host = cpal::default_host();
        let found = match &request.device {
            Some(name) => outputs(&host)
                .into_iter()
                .find(|named| named.name == *name)
                .ok_or_else(|| OpenError::NotFound(name.clone()))?,
            None => {
                // The device as the system hands it out for "the default":
                // a stream built from this one is told when the system's
                // choice moves on, which one built from the list is not.
                let device = host.default_output_device().ok_or(OpenError::NoDevice)?;
                // Under the name it is listed by, which may carry a number.
                let id = device.id().ok();
                let listed = outputs(&host).into_iter().find(|named| id.is_some() && named.device.id().ok() == id);
                let name = listed.map_or_else(|| name_of(&device), |listed| listed.name);
                Named { name, device, is_default: true }
            }
        };
        if request.exclusive && self.unheard {
            return Err(OpenError::Refused(format!(
                "'{}' is only measured on here, and is not taken from the programs that play through it",
                found.name
            )));
        }
        if request.exclusive {
            return open_exclusive(&found, request, renderer, on_failure);
        }
        open_shared(&found, request, renderer, on_failure, self.unheard)
    }

    fn open_input(&self, request: &InputRequest, on_failure: FailureCallback) -> Result<OpenInput, OpenError> {
        if self.unheard {
            // A host that only measures hears nobody's room.
            return Err(OpenError::Refused(
                "the machine's devices are only measured on here: no input of theirs is opened".to_string(),
            ));
        }
        let host = cpal::default_host();
        let found = match &request.device {
            Some(name) => inputs(&host)
                .into_iter()
                .find(|named| named.name == *name)
                .ok_or_else(|| OpenError::NotFound(name.clone()))?,
            None => {
                let device = host.default_input_device().ok_or(OpenError::NoInput)?;
                let id = device.id().ok();
                let listed = inputs(&host).into_iter().find(|named| id.is_some() && named.device.id().ok() == id);
                let name = listed.map_or_else(|| name_of(&device), |listed| listed.name);
                Named { name, device, is_default: true }
            }
        };
        // An input and an output of one piece of hardware count time by
        // one clock.
        let shares_clock = request.output.as_ref().is_some_and(|played| {
            let output = outputs(&host).into_iter().find(|named| named.name == *played);
            output.is_some_and(|output| same_hardware(&found.device, &output.device))
        });
        let mut open = match request.exclusive {
            true => open_exclusive_input(&found, request, on_failure)?,
            false => open_shared_input(&found, on_failure)?,
        };
        open.format.shares_clock = shares_clock;
        Ok(open)
    }
}

#[cfg(windows)]
fn same_hardware(one: &cpal::Device, other: &cpal::Device) -> bool {
    let endpoint = |device: &cpal::Device| device.id().ok().map(|id| id.to_string());
    let (Some(one), Some(other)) = (endpoint(one), endpoint(other)) else { return false };
    let plain = |id: &str| id.strip_prefix("wasapi:").unwrap_or(id).to_string();
    super::wasapi::same_hardware(&plain(&one), &plain(&other))
}

#[cfg(not(windows))]
fn same_hardware(_one: &cpal::Device, _other: &cpal::Device) -> bool {
    false
}

#[cfg(windows)]
fn open_exclusive_input(
    found: &Named,
    request: &InputRequest,
    on_failure: FailureCallback,
) -> Result<OpenInput, OpenError> {
    let id = found.device.id().map(|id| id.to_string()).map_err(|error| OpenError::Failed(error.to_string()))?;
    let endpoint = id.strip_prefix("wasapi:").unwrap_or(&id);
    super::wasapi::open_exclusive_input(endpoint, &found.name, request, on_failure)
}

#[cfg(not(windows))]
fn open_exclusive_input(
    found: &Named,
    _request: &InputRequest,
    _on_failure: FailureCallback,
) -> Result<OpenInput, OpenError> {
    Err(OpenError::Refused(format!("'{}' cannot be taken exclusively on this system", found.name)))
}

/// Opens an input as the system has it, shared with other programs: at
/// the rate and with the buffers it runs at, whatever the engine's are.
fn open_shared_input(found: &Named, on_failure: FailureCallback) -> Result<OpenInput, OpenError> {
    let device = &found.device;
    let failed = |error: cpal::Error| match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable => OpenError::NotFound(found.name.clone()),
        cpal::ErrorKind::UnsupportedConfig => {
            OpenError::Refused(format!("'{}' refused the format: {error}", found.name))
        }
        _ => OpenError::Failed(format!("'{}' could not be opened: {error}", found.name)),
    };
    let supported = device.default_input_config().map_err(failed)?;
    let (channels, rate, format) = (supported.channels() as usize, supported.sample_rate(), supported.sample_format());
    let config: cpal::StreamConfig = supported.config();
    let (sink, link) = crate::capture::link(rate as f64, channels);
    let measured = Arc::new(Measured::default());
    let broke = Arc::new(AtomicBool::new(false));
    let state = Taking {
        sink,
        channels: channels.max(1),
        frames_per_second: rate as f64,
        age: None,
        measured: measured.clone(),
        broke: broke.clone(),
    };
    let lost = on_failure.clone();
    let on_error = move |error: cpal::Error| match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::HostUnavailable => {
            log::warn!("audio input lost: {error}");
            lost(StreamFailure::DeviceLost);
        }
        // This leaves the stream running. It is the device's own word
        // that the buffer it hands over next does not follow the one
        // before, said on its thread just before that buffer: the frames
        // that are missing are silence where they would have been. (It
        // does not say how many: as many as that buffer's stamp says.)
        cpal::ErrorKind::Xrun => broke.store(true, Ordering::Relaxed),
        cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => log::warn!("audio input: {error}"),
        _ => {
            log::error!("audio input error: {error}");
            lost(StreamFailure::Failed);
        }
    };
    let timeout = Some(ACTIVATION_TIMEOUT);
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(config, state.taking::<f32>(), on_error, timeout),
        cpal::SampleFormat::I16 => device.build_input_stream(config, state.taking::<i16>(), on_error, timeout),
        cpal::SampleFormat::U16 => device.build_input_stream(config, state.taking::<u16>(), on_error, timeout),
        cpal::SampleFormat::I32 => device.build_input_stream(config, state.taking::<i32>(), on_error, timeout),
        cpal::SampleFormat::I8 => device.build_input_stream(config, state.taking::<i8>(), on_error, timeout),
        cpal::SampleFormat::U8 => device.build_input_stream(config, state.taking::<u8>(), on_error, timeout),
        cpal::SampleFormat::F64 => device.build_input_stream(config, state.taking::<f64>(), on_error, timeout),
        other => {
            return Err(OpenError::Refused(format!(
                "'{}' gives samples as {other:?}, which Trill does not read",
                found.name
            )));
        }
    }
    .map_err(failed)?;
    stream.play().map_err(failed)?;

    // How many frames it hands over at a time, and how old they are when
    // it does, are known once a few buffers have come.
    let deadline = Instant::now() + SETTLE_TIME;
    while measured.callbacks.load(Ordering::Relaxed) < SETTLE_CALLBACKS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let buffer_frames = match measured.period.load(Ordering::Relaxed) {
        // Nothing has come yet: a hundredth of a second, as the system
        // hands a shared device's sound over.
        0 => rate / 100,
        frames => frames,
    };
    let format = InputFormat {
        device: found.name.clone(),
        sample_rate: rate,
        channels: channels as u16,
        buffer_frames,
        latency_frames: measured.latency.load(Ordering::Relaxed),
        exclusive: false,
        shares_clock: false,
    };
    Ok(OpenInput { format, handle: Box::new(stream), link })
}

/// What an input stream's callback owns.
struct Taking {
    sink: Sink,
    channels: usize,
    frames_per_second: f64,
    /// How old a buffer's first frame is when it is handed over, smoothed.
    age: Option<f64>,
    measured: Arc<Measured>,
    /// Set when the device has said that the buffer it hands over next
    /// does not follow the one before.
    broke: Arc<AtomicBool>,
}

/// A buffer whose first frame is said to be older than this, in seconds,
/// when it is handed over has no stamp: a device does not keep that much,
/// and the zero of the system's counter (which a driver without a stamp
/// gives) is the day the machine was started.
const OLDEST_STAMP: f64 = 2.0;

impl Taking {
    /// The data callback for a stream of `T` samples.
    fn taking<T>(mut self) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        move |data, info| self.take(data, info)
    }

    fn take<T>(&mut self, data: &[T], info: &cpal::InputCallbackInfo)
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        // An input's thread is an audio thread like the output's.
        super::priority::AudioPriority::raise_for_good();
        let channels = self.channels;
        let frames = data.len() / channels;
        // When the device heard the buffer's first frame, by its own
        // account: so long before this call. A stamp that has it heard
        // after this call, or longer ago than any buffer is old, is none.
        let stamp = info.timestamp();
        let age = stamp.callback.checked_duration_since(stamp.capture).map(|age| age.as_secs_f64());
        let age = age.filter(|age| *age < OLDEST_STAMP);
        let heard_ns = age.map(|age| now_ns().saturating_sub((age * 1e9) as u64));
        let missing = match self.broke.swap(false, Ordering::Relaxed) {
            true => Missing::Uncounted,
            false => Missing::Nothing,
        };
        let said = Said { heard_ns, missing };
        self.sink.push_said(said, frames, |frame, channel| data[frame * channels + channel].to_sample::<f32>());
        let Some(age) = age else { return };

        // What lies between the microphone and a buffer beyond the
        // buffer's own length is the device's latency.
        let beyond = (age * self.frames_per_second - frames as f64).max(0.0);
        let smoothed = match self.age {
            Some(before) => before + (beyond - before) * 0.1,
            None => beyond,
        };
        self.age = Some(smoothed);
        let measured = &self.measured;
        measured.latency.store(smoothed.round() as u32, Ordering::Relaxed);
        let period = measured.period.load(Ordering::Relaxed);
        if period == 0 || (frames as u32) > period {
            // (The most it hands over at once: what has to have gone by
            // before the newest of it is there.)
            measured.period.store(frames as u32, Ordering::Relaxed);
        }
        measured.callbacks.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(windows)]
fn open_exclusive(
    found: &Named,
    request: &StreamRequest,
    renderer: Renderer,
    on_failure: FailureCallback,
) -> Result<OpenStream, OpenError> {
    // cpal's id for a WASAPI device is the endpoint's own, behind a prefix.
    let id = found.device.id().map(|id| id.to_string()).map_err(|error| OpenError::Failed(error.to_string()))?;
    let endpoint = id.strip_prefix("wasapi:").unwrap_or(&id);
    super::wasapi::open_exclusive(endpoint, &found.name, request, renderer, on_failure)
}

#[cfg(not(windows))]
fn open_exclusive(
    found: &Named,
    _request: &StreamRequest,
    _renderer: Renderer,
    _on_failure: FailureCallback,
) -> Result<OpenStream, OpenError> {
    Err(OpenError::Refused(format!(
        "'{}' cannot be taken exclusively on this system; a shared device takes the buffer size asked for here",
        found.name
    )))
}

fn open_shared(
    found: &Named,
    request: &StreamRequest,
    renderer: Renderer,
    on_failure: FailureCallback,
    unheard: bool,
) -> Result<OpenStream, OpenError> {
    let device = &found.device;
    let failed = |error: cpal::Error| match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable => OpenError::NotFound(found.name.clone()),
        cpal::ErrorKind::UnsupportedConfig => {
            OpenError::Refused(format!("'{}' refused the format: {error}", found.name))
        }
        _ => OpenError::Failed(format!("'{}' could not be opened: {error}", found.name)),
    };
    let default = device.default_output_config().map_err(failed)?;
    let rate = request.sample_rate.unwrap_or(default.sample_rate());
    let supported = if rate == default.sample_rate() {
        default
    } else {
        // The format nearest the device's own that runs at the rate asked
        // for: the same channels and sample type if that exists.
        let ranges: Vec<cpal::SupportedStreamConfigRange> =
            device.supported_output_configs().map_err(failed)?.collect();
        let fits = |range: &&cpal::SupportedStreamConfigRange| {
            (range.min_sample_rate()..=range.max_sample_rate()).contains(&rate)
        };
        let nearest = ranges.iter().filter(fits).max_by_key(|range| {
            (range.channels() == default.channels(), range.sample_format() == default.sample_format())
        });
        match nearest {
            Some(range) => range.with_sample_rate(rate),
            None => {
                return Err(OpenError::Refused(format!(
                    "'{}' does not run at {rate} Hz while it is shared with other programs (the system runs it at {} Hz)",
                    found.name,
                    default.sample_rate()
                )));
            }
        }
    };

    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let mut config: cpal::StreamConfig = supported.config();
    config.buffer_size = request.buffer_size.map_or(cpal::BufferSize::Default, cpal::BufferSize::Fixed);

    let measured = Arc::new(Measured::default());
    let state = Callback {
        renderer,
        channels: channels.max(1),
        frames_per_second: rate as f64,
        latency: None,
        measured: measured.clone(),
        unheard,
    };
    let lost = on_failure.clone();
    let on_error = move |error: cpal::Error| match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::HostUnavailable => {
            log::warn!("audio stream lost: {error}");
            lost(StreamFailure::DeviceLost);
        }
        // These leave the stream running.
        cpal::ErrorKind::Xrun => log::debug!("audio drop-out: {error}"),
        cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => log::warn!("audio stream: {error}"),
        _ => {
            log::error!("audio stream error: {error}");
            lost(StreamFailure::Failed);
        }
    };
    let timeout = Some(ACTIVATION_TIMEOUT);
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(config, state.filling::<f32>(), on_error, timeout),
        cpal::SampleFormat::I16 => device.build_output_stream(config, state.filling::<i16>(), on_error, timeout),
        cpal::SampleFormat::U16 => device.build_output_stream(config, state.filling::<u16>(), on_error, timeout),
        cpal::SampleFormat::I32 => device.build_output_stream(config, state.filling::<i32>(), on_error, timeout),
        cpal::SampleFormat::I8 => device.build_output_stream(config, state.filling::<i8>(), on_error, timeout),
        cpal::SampleFormat::U8 => device.build_output_stream(config, state.filling::<u8>(), on_error, timeout),
        cpal::SampleFormat::F64 => device.build_output_stream(config, state.filling::<f64>(), on_error, timeout),
        other => {
            return Err(OpenError::Refused(format!(
                "'{}' wants samples as {other:?}, which Trill does not write",
                found.name
            )));
        }
    }
    .map_err(failed)?;
    stream.play().map_err(failed)?;

    // The latency a shared device settles at is known only once its buffer
    // has filled, a few callbacks in.
    let deadline = Instant::now() + SETTLE_TIME;
    while measured.callbacks.load(Ordering::Relaxed) < SETTLE_CALLBACKS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let buffer_frames = match stream.buffer_size() {
        Ok(frames) if frames > 0 => frames,
        _ => measured.period.load(Ordering::Relaxed),
    };
    let format = StreamFormat {
        device: found.name.clone(),
        sample_rate: rate,
        channels: channels as u16,
        buffer_frames,
        latency_frames: measured.latency.load(Ordering::Relaxed),
        exclusive: false,
    };
    Ok(OpenStream { format, handle: Box::new(stream) })
}

/// What a stream's first callbacks tell whoever opened it.
#[derive(Default)]
struct Measured {
    callbacks: AtomicU32,
    /// The latency as of the last callback, in frames.
    latency: AtomicU32,
    /// The smallest buffer the device has asked for, in frames: its period.
    /// (The first buffer of a shared stream is larger: it fills the queue.)
    period: AtomicU32,
}

/// What a stream's callback owns.
struct Callback {
    renderer: Renderer,
    channels: usize,
    frames_per_second: f64,
    /// The latency, smoothed: the device reports it to the granularity of
    /// its own clock, and a take must not wobble with that.
    latency: Option<f64>,
    measured: Arc<Measured>,
    /// What is made is not handed on: the device is given silence.
    unheard: bool,
}

impl Callback {
    /// The data callback for a stream of `T` samples.
    fn filling<T>(mut self) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        move |data, info| self.fill(data, info)
    }

    fn fill<T>(&mut self, data: &mut [T], info: &cpal::OutputCallbackInfo)
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        // The library that calls this leaves its thread at the priority of
        // any other. The first buffer puts it where audio belongs; from
        // then on this is a look at a thread-local.
        super::priority::AudioPriority::raise_for_good();
        let channels = self.channels;
        let frames = data.len() / channels;
        let stamp = info.timestamp();
        let ahead = stamp.playback.duration_since(stamp.callback).as_secs_f64() * self.frames_per_second;
        let latency = match self.latency {
            Some(latency) => latency + (ahead - latency) * 0.1,
            None => ahead,
        };
        self.latency = Some(latency);
        let latency_frames = latency.round().clamp(0.0, u32::MAX as f64) as u32;
        let measured = &self.measured;
        measured.latency.store(latency_frames, Ordering::Relaxed);
        let period = measured.period.load(Ordering::Relaxed);
        if period == 0 || (frames as u32) < period {
            measured.period.store(frames as u32, Ordering::Relaxed);
        }
        measured.callbacks.fetch_add(1, Ordering::Relaxed);

        let timing = Timing { now_ns: now_ns(), latency_frames };
        let mut written = 0;
        let unheard = self.unheard;
        self.renderer.render(frames, timing, &mut |left, right| {
            let out = &mut data[written * channels..(written + left.len()) * channels];
            for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
                if unheard {
                    frame.fill(T::from_sample(0.0));
                } else if channels == 1 {
                    frame[0] = T::from_sample((left[index] + right[index]) * 0.5);
                } else {
                    frame[0] = T::from_sample(left[index]);
                    frame[1] = T::from_sample(right[index]);
                    for extra in &mut frame[2..] {
                        *extra = T::from_sample(0.0);
                    }
                }
            }
            written += left.len();
        });
    }
}
