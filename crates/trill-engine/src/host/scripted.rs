//! Audio devices that exist only in a test.
//!
//! A [`ScriptedHost`] lists the devices a test gives it and opens them
//! like a real host, but has no audio thread: the test is the audio
//! thread, and [`ScriptedHost::advance`] is the device asking for its next
//! buffers. Time stands still in between, so what a test plays and where
//! it lands are exact, on any machine and under any load. Devices can be
//! unplugged, made to refuse a format, or made to stop answering.
//!
//! It has inputs too ([`ScriptedInput`]), each with a clock of its own
//! that a test sets against the host's: what one hears is a function of
//! the host's time ([`Hears`]), or what the output plays, as through a
//! cable. An input hands over its buffers as the host's time reaches
//! them, which is as the output is advanced.

use super::{
    AudioDevice, AudioDevices, AudioHost, FailureCallback, InputFormat, InputRequest, OpenError, OpenInput, OpenStream,
    Renderer, StreamFailure, StreamFormat, StreamRequest, Timing,
};
use crate::capture::Sink;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

/// How much of what the output played is kept for an input that hears it.
const PLAYED_SECONDS: usize = 4;
/// How many frames on either side of a moment between two frames of what
/// was played are read to say what was played at it.
const BETWEEN: i64 = 16;

/// An input device of a [`ScriptedHost`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptedInput {
    pub name: String,
    /// The rate it runs at, whatever is asked of it.
    pub sample_rate: u32,
    pub channels: u16,
    /// How many frames it hands over at a time when shared. Taken
    /// exclusively it hands over as many as are asked for.
    pub buffer: u32,
    /// How many frames it says lie between its microphone and a buffer.
    pub latency: u32,
    /// How many more lie there than it says: what a measurement finds.
    pub unsaid: u32,
    /// How fast its clock runs against the host's, in parts a million.
    pub ppm: f64,
    pub exclusive: bool,
    /// Whether it says it counts time by the output's clock: the input of
    /// the device that is played through. (Its `ppm` should then be
    /// nothing: one that says so and runs apart all the same is how a
    /// test has the session find out.)
    pub shares_clock: bool,
}

impl ScriptedInput {
    /// A device called `name` with two channels that runs at 48 kHz on
    /// the host's own clock, hands over 480 frames at a time and says,
    /// rightly, that they are 240 frames old; it can be taken exclusively.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            sample_rate: 48_000,
            channels: 2,
            buffer: 480,
            latency: 240,
            unsaid: 0,
            ppm: 0.0,
            exclusive: true,
            shares_clock: false,
        }
    }
}

/// What a channel of an input hears at a moment of the host's clock, in
/// seconds.
pub type Sound = Arc<dyn Fn(usize, f64) -> f32 + Send + Sync>;

/// What the inputs of a [`ScriptedHost`] hear.
#[derive(Clone, Default)]
pub enum Hears {
    #[default]
    Nothing,
    /// A sound: see [`Sound`].
    Sound(Sound),
    /// What the output plays, at the moment it is heard and `gain` as
    /// loud: a cable from the output to the input, left to the first
    /// channel and right to the second.
    Output { gain: f32 },
}

/// An input that is open.
struct Capturing {
    sink: Sink,
    on_failure: FailureCallback,
    format: InputFormat,
    device: ScriptedInput,
    serial: u64,
    /// When it was opened on the host's clock, in nanoseconds, and how
    /// many buffers it has handed over or lost since.
    opened: f64,
    handed: u64,
}

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
    /// rendered. (Kept with its fraction, so that a buffer which is not a
    /// whole number of nanoseconds long does not add up to a clock that
    /// is slow.)
    now_ns: u64,
    now: f64,
    inputs: Vec<ScriptedInput>,
    capturing: Option<Capturing>,
    /// Every attempt to open an input, in order.
    input_requests: Vec<InputRequest>,
    hears: Hears,
    /// While set, the open input hands nothing over: what it would have
    /// is lost.
    input_stalled: bool,
    /// What the output has played since it was opened, for an input that
    /// hears it: the last seconds of it, the frame the first of them is,
    /// and how many were played in all; when it was opened; and how many
    /// frames more lie between its buffers and the speaker than it says.
    played: (VecDeque<f32>, VecDeque<f32>),
    played_from: u64,
    played_frames: u64,
    output_opened: f64,
    output_unsaid: u32,
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

    /// Plugs an input device in.
    pub fn add_input(&self, device: ScriptedInput) {
        self.state.lock().inputs.push(device);
    }

    /// Unplugs an input device. A stream running on it says the device is
    /// lost and hands nothing more over.
    pub fn remove_input(&self, name: &str) {
        let lost = {
            let mut state = self.state.lock();
            state.inputs.retain(|device| device.name != name);
            match &state.capturing {
                Some(capturing) if capturing.format.device == name => state.capturing.take(),
                _ => None,
            }
        };
        if let Some(capturing) = lost {
            (capturing.on_failure)(StreamFailure::DeviceLost);
        }
    }

    /// Says what the inputs hear from now on.
    pub fn hear(&self, hears: Hears) {
        self.state.lock().hears = hears;
    }

    /// Has the open input hand nothing over for as long as this is set:
    /// what it would have handed over meanwhile is lost, as it is when a
    /// device is kept from its work.
    pub fn stall_input(&self, stalled: bool) {
        self.state.lock().input_stalled = stalled;
    }

    /// Says how many frames more lie between the output's buffers and its
    /// speaker than it says: what an input that hears the output hears
    /// that much later.
    pub fn set_output_unsaid(&self, frames: u32) {
        self.state.lock().output_unsaid = frames;
    }

    /// The format of the open input, if one is open.
    pub fn capturing(&self) -> Option<InputFormat> {
        self.state.lock().capturing.as_ref().map(|capturing| capturing.format.clone())
    }

    /// Every attempt to open an input so far, in order.
    pub fn input_requests(&self) -> Vec<InputRequest> {
        self.state.lock().input_requests.clone()
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
        // What the input has to hand over by now is there before the
        // output is asked for anything.
        self.hand_over();
        let Some(running) = &mut self.running else { return };
        let timing = Timing { now_ns: self.now_ns, latency_frames: running.format.latency_frames };
        let (left, right, keep) = (&mut self.left, &mut self.right, self.keep_output);
        let played = &mut self.played;
        let listened = matches!(self.hears, Hears::Output { .. });
        running.renderer.render(frames, timing, &mut |l, r| {
            if keep {
                left.extend_from_slice(l);
                right.extend_from_slice(r);
            }
            if listened {
                played.0.extend(l);
                played.1.extend(r);
            }
        });
        let rate = running.format.sample_rate as usize;
        self.played_frames += frames as u64;
        if !listened {
            // Nobody hears it: what was played before is not there to be
            // heard later either.
            self.played.0.clear();
            self.played.1.clear();
            self.played_from = self.played_frames;
        }
        while self.played.0.len() > PLAYED_SECONDS * rate {
            self.played.0.pop_front();
            self.played.1.pop_front();
            self.played_from += 1;
        }
        self.now += frames as f64 * 1e9 / rate as f64;
        self.now_ns = self.now.round() as u64;
    }

    /// Has the open input hand over every buffer the host's time has
    /// reached: one is there once its last frame has been heard and what
    /// lies between the microphone and the buffer has gone by.
    fn hand_over(&mut self) {
        let now = self.now;
        let stalled = self.input_stalled;
        let State { capturing, hears, played, played_from, running, output_opened, output_unsaid, .. } = self;
        let Some(capturing) = capturing else { return };
        let device = &capturing.device;
        let rate = device.sample_rate as f64 * (1.0 + device.ppm / 1e6);
        let buffer = capturing.format.buffer_frames as u64;
        let behind = (device.latency + device.unsaid) as f64;
        // What the speaker gives out at a moment of the host's clock.
        let output = running.as_ref().map(|running| {
            let ahead = (running.format.latency_frames + *output_unsaid) as f64;
            (running.format.sample_rate as f64, ahead)
        });
        let (played_from, output_opened) = (*played_from as i64, *output_opened);
        let speaker = |channel: usize, ns: f64| -> f32 {
            let Some((output_rate, ahead)) = output else { return 0.0 };
            let side = match channel {
                0 => &played.0,
                1 => &played.1,
                _ => return 0.0,
            };
            // The frame that is at the speaker then, which is seldom a
            // whole one: read between frames, and exactly where it is.
            let place = (ns - output_opened) / 1e9 * output_rate - ahead;
            let whole = place.round();
            let at = |frame: i64| match usize::try_from(frame - played_from) {
                Ok(index) => side.get(index).copied().unwrap_or(0.0) as f64,
                Err(_) => 0.0,
            };
            if (place - whole).abs() < 1e-6 {
                return at(whole as i64) as f32;
            }
            let whole = place.floor();
            let fraction = place - whole;
            let mut sum = 0.0;
            for tap in -BETWEEN + 1..=BETWEEN {
                let away = tap as f64 - fraction;
                let angle = std::f64::consts::PI * away;
                let window = 0.5 + 0.5 * (angle / BETWEEN as f64).cos();
                sum += at(whole as i64 + tap) * angle.sin() / angle * window;
            }
            sum as f32
        };
        loop {
            let first = capturing.handed * buffer;
            let due = capturing.opened + ((first + buffer) as f64 + behind) / rate * 1e9;
            if due > now {
                break;
            }
            capturing.handed += 1;
            if stalled {
                continue;
            }
            // It says its frames are as old as it says its latency is: a
            // frame is stamped later than it was heard by what it leaves
            // unsaid.
            let stamp = capturing.opened + (first as f64 + device.unsaid as f64) / rate * 1e9;
            let opened = capturing.opened;
            let heard_ns = |frame: usize| opened + (first + frame as u64) as f64 / rate * 1e9;
            capturing.sink.push(stamp.round() as u64, buffer as usize, |frame, channel| match &*hears {
                Hears::Nothing => 0.0,
                Hears::Sound(sound) => sound(channel, heard_ns(frame) / 1e9),
                Hears::Output { gain } => speaker(channel, heard_ns(frame)) * gain,
            });
        }
    }

    fn listed_input(&self, device: &ScriptedInput) -> AudioDevice {
        AudioDevice {
            name: device.name.clone(),
            is_default: self.inputs.first().is_some_and(|first| first.name == device.name),
            default_sample_rate: device.sample_rate,
            sample_rates: vec![device.sample_rate],
            channels: device.channels,
            exclusive: device.exclusive,
        }
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
        AudioDevices {
            outputs: state.devices.iter().map(|device| state.listed(device)).collect(),
            inputs: state.inputs.iter().map(|device| state.listed_input(device)).collect(),
        }
    }

    fn open_input(&self, request: &InputRequest, on_failure: FailureCallback) -> Result<OpenInput, OpenError> {
        let mut state = self.state.lock();
        state.input_requests.push(request.clone());
        let device = match &request.device {
            Some(name) => state
                .inputs
                .iter()
                .find(|device| device.name == *name)
                .ok_or_else(|| OpenError::NotFound(name.clone()))?,
            None => state.inputs.first().ok_or(OpenError::NoInput)?,
        }
        .clone();
        if request.exclusive && !device.exclusive {
            return Err(OpenError::Refused(format!("'{}' cannot be taken exclusively", device.name)));
        }
        let format = InputFormat {
            device: device.name.clone(),
            sample_rate: device.sample_rate,
            channels: device.channels,
            buffer_frames: match request.exclusive {
                true => request.buffer_size.unwrap_or(device.buffer),
                false => device.buffer,
            },
            latency_frames: device.latency,
            exclusive: request.exclusive,
            shares_clock: device.shares_clock && request.output.is_some(),
        };
        let (sink, link) = crate::capture::link(device.sample_rate as f64, device.channels as usize);
        state.serial += 1;
        let serial = state.serial;
        let opened = state.now;
        state.capturing =
            Some(Capturing { sink, on_failure, format: format.clone(), device, serial, opened, handed: 0 });
        Ok(OpenInput { format, handle: Box::new(InputHandle { state: self.state.clone(), serial }), link })
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
        // What an input hears of the output is this stream's from here.
        state.played = Default::default();
        (state.played_from, state.played_frames, state.output_opened) = (0, 0, state.now);
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

    fn now_ns(&self) -> u64 {
        self.state.lock().now_ns
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

/// Closes its input when dropped, if that input is still the one open.
struct InputHandle {
    state: Arc<Mutex<State>>,
    serial: u64,
}

impl Drop for InputHandle {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        if state.capturing.as_ref().is_some_and(|capturing| capturing.serial == self.serial) {
            state.capturing = None;
        }
    }
}
