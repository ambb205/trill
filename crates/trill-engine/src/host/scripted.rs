//! Audio devices that exist only in a test.
//!
//! A [`ScriptedHost`] lists the devices a test gives it and opens them
//! like a real host, but has no audio thread: the test is the audio
//! thread, and [`ScriptedHost::advance`] is the device asking for its next
//! buffers. Time stands still in between, so what a test plays and where
//! it lands are exact, on any machine and under any load. Devices can be
//! unplugged, made to refuse a format, or made to stop answering.

use super::{
    AudioDevice, AudioDevices, AudioHost, FailureCallback, OpenError, OpenStream, Renderer, StreamFailure,
    StreamFormat, StreamRequest, Timing,
};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

/// A device of a [`ScriptedHost`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptedDevice {
    pub name: String,
    /// The rates it opens at; the first is the one it runs at unasked.
    pub sample_rates: Vec<u32>,
    /// The buffer it asks for when shared, whatever size is asked of it:
    /// the way a device shared through a system mixer behaves.
    pub shared_buffer: u32,
    /// Frames between a buffer and the speaker when shared.
    pub shared_latency: u32,
    /// Whether it can be taken exclusively. It then gives the buffer size
    /// asked for (or `shared_buffer`), and its latency is that buffer.
    pub exclusive: bool,
}

impl ScriptedDevice {
    /// A device called `name` that runs at 48 kHz (and opens at 44.1 kHz),
    /// asks for 480 frames at a time when shared and plays them 1440
    /// frames later, and can be taken exclusively.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            sample_rates: vec![48_000, 44_100],
            shared_buffer: 480,
            shared_latency: 1440,
            exclusive: true,
        }
    }
}

struct Running {
    renderer: Renderer,
    on_failure: FailureCallback,
    format: StreamFormat,
    /// Which stream of the host this is, so that a handle closes only its
    /// own.
    serial: u64,
}

#[derive(Default)]
struct State {
    devices: Vec<ScriptedDevice>,
    /// The name of the default device; the first device when `None`.
    default: Option<String>,
    running: Option<Running>,
    serial: u64,
    /// Every attempt to open a device, in order.
    requests: Vec<StreamRequest>,
    /// The host's clock, in nanoseconds: it moves only as audio is
    /// rendered.
    now_ns: u64,
    /// What was rendered since the last [`ScriptedHost::take_output`].
    left: Vec<f32>,
    right: Vec<f32>,
    keep_output: bool,
    /// How long opening a device takes.
    open_delay: Duration,
    watched: bool,
    /// Whether waiting on the audio thread renders a buffer.
    pump: bool,
}

/// See the module documentation.
#[derive(Clone)]
pub struct ScriptedHost {
    state: Arc<Mutex<State>>,
}

impl Default for ScriptedHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedHost {
    /// A host with no devices.
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(State { pump: true, ..State::default() })) }
    }

    /// A host with one device, `"Speakers"`; see [`ScriptedDevice::new`].
    pub fn with_speakers() -> Self {
        let host = Self::new();
        host.add_device(ScriptedDevice::new("Speakers"));
        host
    }

    /// Plugs a device in.
    pub fn add_device(&self, device: ScriptedDevice) {
        self.state.lock().devices.push(device);
    }

    /// Unplugs a device. A stream running on it fails as a real one does:
    /// it stops asking for audio and says the device is lost.
    pub fn remove_device(&self, name: &str) {
        let lost = {
            let mut state = self.state.lock();
            state.devices.retain(|device| device.name != name);
            if state.default.as_deref() == Some(name) {
                state.default = None;
            }
            match &state.running {
                Some(running) if running.format.device == name => state.running.take(),
                _ => None,
            }
        };
        if let Some(running) = lost {
            (running.on_failure)(StreamFailure::DeviceLost);
        }
    }

    /// Makes `name` the system's default device.
    pub fn set_default(&self, name: &str) {
        self.state.lock().default = Some(name.into());
    }

    /// Makes the running stream report `failure` and stop.
    pub fn fail_stream(&self, failure: StreamFailure) {
        let running = self.state.lock().running.take();
        if let Some(running) = running {
            (running.on_failure)(failure);
        }
    }

    /// Makes every attempt to open a device take `delay`: a driver that is
    /// slow to answer, or never does.
    pub fn set_open_delay(&self, delay: Duration) {
        self.state.lock().open_delay = delay;
    }

    /// Whether the engine should watch this host's streams for stalling,
    /// as it does a real device's. Off unless asked for: a stream that only
    /// moves when a test says so looks stalled to a clock.
    pub fn set_watched(&self, watched: bool) {
        self.state.lock().watched = watched;
    }

    /// Whether the engine waiting for the audio thread renders a buffer
    /// (the default), or finds the audio thread unresponsive.
    pub fn set_pump(&self, pump: bool) {
        self.state.lock().pump = pump;
    }

    /// Keeps what is rendered from now on for [`ScriptedHost::take_output`].
    pub fn keep_output(&self) {
        self.state.lock().keep_output = true;
    }

    /// What was rendered since the last call, left and right.
    pub fn take_output(&self) -> (Vec<f32>, Vec<f32>) {
        let mut state = self.state.lock();
        (std::mem::take(&mut state.left), std::mem::take(&mut state.right))
    }

    /// Every attempt to open a device so far, in order.
    pub fn requests(&self) -> Vec<StreamRequest> {
        self.state.lock().requests.clone()
    }

    /// The format of the running stream, if one is running.
    pub fn running(&self) -> Option<StreamFormat> {
        self.state.lock().running.as_ref().map(|running| running.format.clone())
    }

    /// The host's clock: nanoseconds of audio rendered so far. Live input
    /// for a test is stamped with it.
    pub fn now_ns(&self) -> u64 {
        self.state.lock().now_ns
    }

    /// The time on the host's clock `frames` frames from now, at the
    /// running stream's rate.
    pub fn ns_after(&self, frames: f64) -> u64 {
        let state = self.state.lock();
        let rate = state.running.as_ref().map_or(48_000, |running| running.format.sample_rate);
        state.now_ns + (frames * 1e9 / rate as f64).round() as u64
    }

    /// The device asks for `frames` more frames, a buffer at a time (the
    /// last buffer may be short). Returns how many were rendered: none
    /// when no stream is running.
    pub fn advance(&self, frames: usize) -> usize {
        let mut state = self.state.lock();
        let mut done = 0;
        while done < frames {
            let buffer = state.running.as_ref().map_or(0, |running| running.format.buffer_frames as usize);
            if buffer == 0 {
                break;
            }
            let size = buffer.min(frames - done);
            state.render(size);
            done += size;
        }
        done
    }

    /// Advances by whole buffers until `done` says so, or `limit` frames
    /// have gone by. Returns whether `done` did.
    pub fn advance_until(&self, limit: usize, mut done: impl FnMut() -> bool) -> bool {
        let mut rendered = 0;
        while !done() {
            let buffer = self.running().map_or(0, |format| format.buffer_frames as usize);
            if buffer == 0 || rendered >= limit {
                return false;
            }
            rendered += self.advance(buffer);
        }
        true
    }
}

impl State {
    fn render(&mut self, frames: usize) {
        let Some(running) = &mut self.running else { return };
        let timing = Timing { now_ns: self.now_ns, latency_frames: running.format.latency_frames };
        let (left, right, keep) = (&mut self.left, &mut self.right, self.keep_output);
        running.renderer.render(frames, timing, &mut |l, r| {
            if keep {
                left.extend_from_slice(l);
                right.extend_from_slice(r);
            }
        });
        self.now_ns += (frames as f64 * 1e9 / running.format.sample_rate as f64).round() as u64;
    }

    fn listed(&self, device: &ScriptedDevice) -> AudioDevice {
        let default = self.default.as_deref().or(self.devices.first().map(|first| first.name.as_str()));
        AudioDevice {
            name: device.name.clone(),
            is_default: default == Some(device.name.as_str()),
            default_sample_rate: device.sample_rates.first().copied().unwrap_or(48_000),
            sample_rates: {
                let mut rates = device.sample_rates.clone();
                rates.sort_unstable();
                rates
            },
            channels: 2,
            exclusive: device.exclusive,
        }
    }
}

impl AudioHost for ScriptedHost {
    fn devices(&self) -> AudioDevices {
        let state = self.state.lock();
        AudioDevices { outputs: state.devices.iter().map(|device| state.listed(device)).collect(), inputs: Vec::new() }
    }

    fn open_output(
        &self,
        request: &StreamRequest,
        renderer: Renderer,
        on_failure: FailureCallback,
    ) -> Result<OpenStream, OpenError> {
        let delay = {
            let mut state = self.state.lock();
            state.requests.push(request.clone());
            state.open_delay
        };
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }

        let mut state = self.state.lock();
        let device = match &request.device {
            Some(name) => state
                .devices
                .iter()
                .find(|device| device.name == *name)
                .ok_or_else(|| OpenError::NotFound(name.clone()))?,
            None => {
                let default = state.default.clone();
                let named = default.and_then(|name| state.devices.iter().find(|device| device.name == name));
                named.or(state.devices.first()).ok_or(OpenError::NoDevice)?
            }
        }
        .clone();
        let own_rate = device.sample_rates.first().copied().unwrap_or(48_000);
        let sample_rate = request.sample_rate.unwrap_or(own_rate);
        if !device.sample_rates.contains(&sample_rate) {
            return Err(OpenError::Refused(format!("'{}' does not run at {sample_rate} Hz", device.name)));
        }
        if request.exclusive && !device.exclusive {
            return Err(OpenError::Refused(format!("'{}' cannot be taken exclusively", device.name)));
        }
        let buffer_frames =
            if request.exclusive { request.buffer_size.unwrap_or(device.shared_buffer) } else { device.shared_buffer };
        let format = StreamFormat {
            device: device.name.clone(),
            sample_rate,
            channels: 2,
            buffer_frames,
            latency_frames: if request.exclusive { buffer_frames } else { device.shared_latency },
            exclusive: request.exclusive,
        };
        state.serial += 1;
        let serial = state.serial;
        state.running = Some(Running { renderer, on_failure, format: format.clone(), serial });
        Ok(OpenStream { format, handle: Box::new(Handle { state: self.state.clone(), serial }) })
    }

    fn pump(&self) {
        let mut state = self.state.lock();
        if state.pump
            && let Some(buffer) = state.running.as_ref().map(|running| running.format.buffer_frames as usize)
        {
            state.render(buffer);
        }
    }

    fn runs_by_itself(&self) -> bool {
        self.state.lock().watched
    }
}

/// Closes its stream when dropped, if that stream is still the one running.
struct Handle {
    state: Arc<Mutex<State>>,
    serial: u64,
}

impl Drop for Handle {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        if state.running.as_ref().is_some_and(|running| running.serial == self.serial) {
            state.running = None;
        }
    }
}
