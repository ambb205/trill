//! The realtime engine: owns what the audio thread plays with and talks to
//! it through lock-free queues.
//!
//! The controller never touches the processor directly. It sends messages
//! (a new graph, transport commands, parameter changes) through a ring
//! buffer; the audio thread applies them at the start of its next callback
//! and sends retired graphs back to be freed off the audio thread.
//!
//! The audio thread itself belongs to an [`AudioHost`]: the engine asks the
//! host to open a device and hands it a [`Renderer`]. Which device that is
//! can change while the program runs ([`Engine::apply`]), and a device can
//! disappear ([`Engine::check`]). Through all of it the processor, with the
//! graph and every running device, stays where it is, in a [`Seat`] that
//! one stream after another renders from.

use crate::graph::{AudioData, Graph};
use crate::host::{
    AudioConfig, AudioDevices, AudioHost, FailureCallback, NULL_DEVICE, NullHost, OpenError, OpenStream, StreamFailure,
    StreamHandle, StreamRequest, SystemHost, Timing,
};
use crate::live::{Captured, LiveInput, LiveSender, LiveTarget, TrackSet};
use crate::processor::Processor;
use crate::queue::Queue;
use std::cell::UnsafeCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use trill_core::DeviceId;

const MESSAGE_CAPACITY: usize = 1024;
const GARBAGE_CAPACITY: usize = 64;
/// Recorded events waiting to be collected. They are collected a few times
/// a second; a player does not come near this in that time.
const CAPTURE_CAPACITY: usize = 16_384;
/// Largest stretch the engine renders in one go; longer buffers are filled
/// in pieces.
const CALLBACK_FRAMES: usize = 8192;
/// The rate the engine runs at before a device has had its say, and with no
/// device at all.
const FALLBACK_SAMPLE_RATE: u32 = 48_000;

/// How long the engine waits for things that should not take long, before
/// it stops waiting and does the next best thing. The defaults suit real
/// hardware; tests shorten them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Patience {
    /// How long a device may take to open, to list or to close.
    pub device: Duration,
    /// How long the audio thread may take to finish the buffer it is in
    /// when the stream is changed. Longer than this and a device is stuck
    /// inside its `process`: the engine starts over without it.
    pub audio_thread: Duration,
    /// A stream that has not asked for audio for this long has stalled.
    pub stall: Duration,
    /// How often a device that went missing is looked for again.
    pub retry: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Self {
            device: Duration::from_secs(6),
            audio_thread: Duration::from_secs(2),
            stall: Duration::from_secs(3),
            retry: Duration::from_secs(3),
        }
    }
}

/// Where the audio goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AudioBackend {
    /// The system's audio devices, or `Null` if none can be opened.
    Auto,
    /// The system's audio devices; fail if none can be opened.
    Device,
    /// No audio hardware. The engine still runs in real time, so transport
    /// and meters behave normally. Used headless and in tests.
    Null { sample_rate: u32 },
}

/// Everything needed to start an engine: who plays the audio, what the
/// user wants of it, and what to do when that cannot be had.
#[derive(Clone)]
pub struct AudioSetup {
    pub host: Arc<dyn AudioHost>,
    pub config: AudioConfig,
    /// Whether to run without sound when no device can be opened as the
    /// engine starts, rather than fail. Once it runs, an engine always
    /// carries on without sound when its devices go.
    pub silent_fallback: bool,
    pub patience: Patience,
}

impl AudioSetup {
    /// The setup `backend` stands for, asking for `config`.
    pub fn new(backend: AudioBackend, config: AudioConfig) -> Self {
        match backend {
            AudioBackend::Auto => Self::on(Arc::new(SystemHost::new()), config),
            AudioBackend::Device => Self { silent_fallback: false, ..Self::on(Arc::new(SystemHost::new()), config) },
            AudioBackend::Null { sample_rate } => {
                Self::on(Arc::new(NullHost::new(sample_rate)), AudioConfig::default())
            }
        }
    }

    /// A setup that plays through `host`, asking for `config`, and runs
    /// without sound if no device can be opened.
    pub fn on(host: Arc<dyn AudioHost>, config: AudioConfig) -> Self {
        Self { host, config, silent_fallback: true, patience: Patience::default() }
    }
}

impl From<AudioBackend> for AudioSetup {
    fn from(backend: AudioBackend) -> Self {
        Self::new(backend, AudioConfig::default())
    }
}

/// The audio stream as it is, which is not always as it was asked for
/// ([`AudioConfig`]).
#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    pub sample_rate: f64,
    /// Human-readable description of the output, e.g. the device name.
    pub output: String,
    /// Set when the stream is not what was asked for: the device was
    /// missing or refused the format, and something else was opened. In
    /// words fit to show the user.
    pub warning: Option<String>,
    /// The name of the device in use; `None` when running without sound.
    pub device: Option<String>,
    /// How many frames the device asks for at a time.
    pub buffer_frames: u32,
    /// Whether the device was taken for this program alone.
    pub exclusive: bool,
    /// How long after a frame is rendered it is heard, in frames, as the
    /// device reported when it was opened. [`Engine::output_latency`] has
    /// the figure as it stands now.
    pub latency_frames: u32,
}

impl StreamInfo {
    /// [`StreamInfo::latency_frames`] in milliseconds.
    pub fn latency_ms(&self) -> f64 {
        frames_to_ms(self.latency_frames, self.sample_rate)
    }

    /// The length of one buffer in milliseconds.
    pub fn buffer_ms(&self) -> f64 {
        frames_to_ms(self.buffer_frames, self.sample_rate)
    }
}

fn frames_to_ms(frames: u32, sample_rate: f64) -> f64 {
    if sample_rate > 0.0 { frames as f64 * 1000.0 / sample_rate } else { 0.0 }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("audio output unavailable: {0}")]
    NoOutput(String),
    #[error("the audio thread exited unexpectedly")]
    ThreadDied,
}

pub(crate) enum Message {
    Swap(Box<Graph>),
    Play,
    Stop,
    Seek(f64),
    Param {
        device: DeviceId,
        index: usize,
        value: f64,
    },
    Panic,
    Audition {
        audio: Arc<AudioData>,
        gain: f32,
    },
    StopAudition,
    Metronome(bool),
    /// The tracks a keyboard plays from now on.
    LiveTargets(TrackSet),
    /// Lets go of every note a keyboard is holding: its port has gone.
    ReleaseLive,
    /// Lets go of every note that sounds, played by hand or by the song.
    ReleaseEverything,
    /// Starts the take numbered `take` on `tracks`, after `count_in_beats`
    /// of count-in if the song is not already playing.
    Record {
        take: u32,
        tracks: TrackSet,
        count_in_beats: f64,
    },
}

/// What the audio thread hands back to be freed elsewhere.
pub(crate) enum Garbage {
    Graph(Box<Graph>),
    Audio(Arc<AudioData>),
}

/// Whether a take is being recorded, as the audio thread has it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecordPhase {
    Off = 0,
    CountIn = 1,
    Rolling = 2,
}

#[derive(Default)]
struct Shared {
    playing: AtomicBool,
    position: AtomicU64,
    /// Fraction of the available time the last callback used, as f32 bits.
    load: AtomicU32,
    /// Callbacks that panicked and were replaced by silence.
    faults: AtomicU32,
    auditioning: AtomicBool,
    /// How many buffers have been rendered: a stream that stops asking is
    /// found out by this standing still.
    callbacks: AtomicU64,
    /// The output latency the host last reported, in frames.
    latency_frames: AtomicU32,
    /// A [`StreamFailure`] code, or 0.
    failure: AtomicU8,
    /// A [`RecordPhase`].
    recording: AtomicU8,
    /// Times live input was lost for want of room.
    live_dropped: AtomicU32,
}

/// Everything the audio callback works with.
pub(crate) struct Realtime {
    processor: Processor,
    messages: rtrb::Consumer<Message>,
    garbage: rtrb::Producer<Garbage>,
    live: Arc<Queue<LiveInput>>,
    /// Set by whoever could not queue a live event.
    live_overflowed: Arc<AtomicBool>,
    shared: Arc<Shared>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Realtime {
    fn apply_messages(&mut self) {
        while let Ok(message) = self.messages.pop() {
            match message {
                Message::Swap(graph) => {
                    let old = self.processor.swap_graph(graph);
                    // The controller never has more swaps in flight than the
                    // queue holds, so this push cannot fail. If it somehow
                    // does, freeing here is still correct, only slow.
                    let _ = self.garbage.push(Garbage::Graph(old));
                }
                Message::Play => self.processor.play(),
                Message::Stop => self.processor.stop(),
                Message::Seek(beat) => self.processor.seek(beat),
                Message::Param { device, index, value } => self.processor.set_param(device, index, value),
                Message::Panic => self.processor.panic(),
                Message::Audition { audio, gain } => {
                    if let Some(previous) = self.processor.start_audition(audio, gain) {
                        let _ = self.garbage.push(Garbage::Audio(previous));
                    }
                }
                Message::StopAudition => self.processor.stop_audition(),
                Message::Metronome(on) => self.processor.set_metronome(on),
                Message::LiveTargets(tracks) => self.processor.set_live_targets(tracks),
                Message::ReleaseLive => self.processor.release_live(),
                Message::ReleaseEverything => self.processor.release_everything(),
                Message::Record { take, tracks, count_in_beats } => {
                    self.processor.record(take, tracks, count_in_beats);
                }
            }
        }
        if let Some(finished) = self.processor.take_finished_audition() {
            let _ = self.garbage.push(Garbage::Audio(finished));
        }
    }

    /// Hands what was played since the last buffer to the processor, each
    /// event at the frame of this buffer that its time stamp asks for.
    ///
    /// An event that happened `age` before this call is placed `age`
    /// before the buffer's end. Whenever in a buffer's time a key goes
    /// down, it is heard one buffer and the output latency later: a fixed
    /// delay, where playing everything at the buffer's first frame would
    /// add a random one of up to a buffer's length.
    fn take_live_input(&mut self, frames: usize, timing: Timing) {
        let frames_per_ns = self.processor.sample_rate() / 1e9;
        let mut lost = self.live_overflowed.swap(false, Ordering::Relaxed);
        while let Some(input) = self.live.pop() {
            let age = timing.now_ns.saturating_sub(input.time_ns) as f64 * frames_per_ns;
            let offset = (frames as f64 - age).round().clamp(0.0, frames.saturating_sub(1) as f64);
            // How long before the frame it is placed at the event happened.
            let early = offset + age;
            let taken = match input.target {
                LiveTarget::Routed => self.processor.live_routed(input.kind, offset as u32, early),
                LiveTarget::Track(track) => self.processor.live_on(track, input.kind, offset as u32, early),
            };
            lost |= !taken;
        }
        if lost {
            // What was lost may have been a key coming up: nothing may be
            // left sounding on the strength of an event that never came.
            self.shared.live_dropped.fetch_add(1, Ordering::Relaxed);
            self.processor.release_everything();
        }
    }

    /// Renders `frames` (at most the scratch size) into the scratch buffers.
    fn render(&mut self, frames: usize) {
        let left = &mut self.left[..frames];
        let right = &mut self.right[..frames];
        let processor = &mut self.processor;
        // A bug in a device must not take the audio device (and the app)
        // down with it: a panicking block becomes silence.
        if catch_unwind(AssertUnwindSafe(|| processor.process(left, right))).is_err() {
            left.fill(0.0);
            right.fill(0.0);
            self.shared.faults.fetch_add(1, Ordering::Relaxed);
        }
        // Last line of defense before the hardware: no NaNs, no over-range
        // samples.
        for sample in left.iter_mut().chain(right.iter_mut()) {
            *sample = sanitize(*sample);
        }
    }

    /// Renders `total` frames, handing them to `sink` a stretch at a time.
    fn fill(&mut self, total: usize, timing: Timing, sink: &mut dyn FnMut(&[f32], &[f32])) {
        let started = Instant::now();
        self.apply_messages();
        self.processor.set_output_latency(timing.latency_frames);
        self.take_live_input(total, timing);
        let mut done = 0;
        while done < total {
            let frames = (total - done).min(self.left.len());
            self.render(frames);
            sink(&self.left[..frames], &self.right[..frames]);
            done += frames;
        }

        let shared = &self.shared;
        shared.playing.store(self.processor.is_playing(), Ordering::Relaxed);
        shared.auditioning.store(self.processor.is_auditioning(), Ordering::Relaxed);
        shared.position.store(self.processor.position().to_bits(), Ordering::Relaxed);
        shared.recording.store(self.processor.record_phase() as u8, Ordering::Relaxed);
        shared.latency_frames.store(timing.latency_frames, Ordering::Relaxed);
        shared.callbacks.fetch_add(1, Ordering::Relaxed);
        let available = total as f64 / self.processor.sample_rate();
        if available > 0.0 {
            let load = (started.elapsed().as_secs_f64() / available) as f32;
            shared.load.store(load.to_bits(), Ordering::Relaxed);
        }
    }
}

#[inline]
fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 }
}

/// Where the audio thread's state sits while one stream after another
/// renders from it.
///
/// Two things keep it from being used twice at once without anybody having
/// to wait. `active` names the one renderer that may use it: a stream that
/// was closed, or that an attempt to open gave up waiting for, renders
/// silence from then on. And `claimed` is taken for the length of every
/// use: by the audio thread for one buffer, by the controller while it
/// changes what is in the seat.
pub(crate) struct Seat {
    active: AtomicU64,
    claimed: AtomicBool,
    realtime: UnsafeCell<Realtime>,
}

// SAFETY: `realtime` is reached only through `Seat::try_claim`, which lets
// one thread in at a time.
unsafe impl Sync for Seat {}

impl Seat {
    fn try_claim(&self) -> Option<Claim<'_>> {
        self.claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Claim { seat: self })
    }

    /// Claims the seat for the controller, waiting for the audio thread to
    /// finish the buffer it may be in the middle of. `None` if it never
    /// does: a device is stuck inside its `process`.
    fn claim(&self, timeout: Duration) -> Option<Claim<'_>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(claim) = self.try_claim() {
                return Some(claim);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

struct Claim<'a> {
    seat: &'a Seat,
}

impl Claim<'_> {
    fn realtime(&mut self) -> &mut Realtime {
        // SAFETY: the claim is exclusive for as long as it lives.
        unsafe { &mut *self.seat.realtime.get() }
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.seat.claimed.store(false, Ordering::Release);
    }
}

/// What a host's audio thread calls to have a buffer rendered.
pub struct Renderer {
    seat: Arc<Seat>,
    generation: u64,
    silence: Box<[f32]>,
}

impl Renderer {
    fn new(seat: Arc<Seat>, generation: u64) -> Self {
        Self { seat, generation, silence: vec![0.0; 1024].into_boxed_slice() }
    }

    /// Renders the next `frames` frames and hands them to `sink` as left
    /// and right samples, in order, a stretch at a time (the stretches add
    /// up to `frames`). Realtime-safe: it neither allocates nor waits. A
    /// renderer whose stream has been replaced renders silence.
    pub fn render(&mut self, frames: usize, timing: Timing, sink: &mut dyn FnMut(&[f32], &[f32])) {
        if self.seat.active.load(Ordering::Acquire) == self.generation
            && let Some(mut claim) = self.seat.try_claim()
        {
            claim.realtime().fill(frames, timing, sink);
            return;
        }
        let mut done = 0;
        while done < frames {
            let stretch = (frames - done).min(self.silence.len());
            sink(&self.silence[..stretch], &self.silence[..stretch]);
            done += stretch;
        }
    }
}

/// A thread that talks to the audio devices on the engine's behalf, so
/// that a device which never answers costs a wait with an end, not the
/// program. It lives as long as the engine: on Windows the objects of a
/// stream stay valid only while a thread that knew them is around.
struct Driver {
    jobs: mpsc::Sender<Box<dyn FnOnce() + Send>>,
    thread: Option<JoinHandle<()>>,
}

impl Driver {
    fn start() -> Result<Self, EngineError> {
        let (jobs, queue) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let thread = thread::Builder::new()
            .name("audio-driver".into())
            .spawn(move || {
                for job in queue {
                    job();
                }
            })
            .map_err(|error| EngineError::NoOutput(format!("cannot start a thread: {error}")))?;
        Ok(Self { jobs, thread: Some(thread) })
    }

    /// Runs `job` on the driver thread and waits for it, at most `timeout`.
    /// `None` if it took longer: the thread is then left to itself (its
    /// result, should one ever come, is dropped there) and a new one takes
    /// its place.
    fn call<T: Send + 'static>(&mut self, timeout: Duration, job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (reply, result) = mpsc::channel();
        let sent = self.jobs.send(Box::new(move || {
            let _ = reply.send(job());
        }));
        if sent.is_ok()
            && let Ok(value) = result.recv_timeout(timeout)
        {
            return Some(value);
        }
        log::error!("an audio device did not answer within {} s; leaving it be", timeout.as_secs());
        if let Ok(fresh) = Self::start() {
            // The stuck thread is let go of, not joined: it may never
            // return. If it does, it finds its queue closed and ends.
            let mut stuck = std::mem::replace(self, fresh);
            stuck.thread = None;
        }
        None
    }

    /// Runs `job` on the driver thread without waiting for it.
    fn post(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.jobs.send(Box::new(job));
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Closing the queue ends the thread once it is idle, which it is:
        // a thread that got stuck was replaced, and forgotten, in `call`.
        let (closed, _) = mpsc::channel();
        self.jobs = closed;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What changing the audio stream did to the engine.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The processor was replaced by an empty one (the sample rate changed,
    /// or the old one was stuck): the graph must be compiled anew with
    /// every device created afresh, and what the audio thread had been told
    /// (transport position, metronome, live targets) told again.
    pub reset: bool,
}

/// Whether the device that was asked for is missing, and so worth looking
/// for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Missing {
    No,
    /// It was not there when last looked for: when it is, try it.
    Absent,
    /// It was there and could not be opened: wait until it has gone.
    Unseen,
}

/// What [`Engine::check`] found.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    Good,
    /// The stream has failed.
    Failed(StreamFailure),
    /// The device that was asked for, and was missing, may be back.
    Retry,
}

/// Controller-side handle to the audio thread.
pub struct Engine {
    messages: rtrb::Producer<Message>,
    garbage: rtrb::Consumer<Garbage>,
    captured: rtrb::Consumer<Captured>,
    live: LiveSender,
    shared: Arc<Shared>,
    seat: Arc<Seat>,
    /// The generation of the renderer that may use the seat; each attempt
    /// to open a stream gets a new one.
    generation: u64,
    info: StreamInfo,
    /// Graphs sent to the audio thread and not yet returned.
    swaps_in_flight: usize,
    host: Arc<dyn AudioHost>,
    /// What plays when no device will: nothing, at real-time pace.
    null: Arc<NullHost>,
    config: AudioConfig,
    patience: Patience,
    // Field order is drop order: the stream goes before the driver thread
    // that opened it.
    stream: Option<StreamHandle>,
    driver: Driver,
    /// Whether the stream is one that should be asking for audio by
    /// itself, and so can be caught stalling.
    watched: bool,
    last_callbacks: u64,
    last_progress: Instant,
    /// Set while a device other than the one asked for is in use because
    /// that one was missing: it is looked for again every so often.
    missing: Missing,
    /// Set while the stream is being opened again because the missing
    /// device was seen to be back.
    retrying: bool,
    last_retry: Instant,
    /// The answer, when it comes, to whether that device is back.
    lookup: Option<mpsc::Receiver<bool>>,
}

/// The controller's ends of a fresh set of queues, and the seat holding
/// the audio thread's ends.
struct Core {
    messages: rtrb::Producer<Message>,
    garbage: rtrb::Consumer<Garbage>,
    captured: rtrb::Consumer<Captured>,
    shared: Arc<Shared>,
    seat: Arc<Seat>,
}

impl Core {
    fn new(sample_rate: f64, live: &LiveSender, generation: u64) -> Self {
        let (message_tx, message_rx) = rtrb::RingBuffer::new(MESSAGE_CAPACITY);
        let (garbage_tx, garbage_rx) = rtrb::RingBuffer::new(GARBAGE_CAPACITY);
        let (capture_tx, capture_rx) = rtrb::RingBuffer::new(CAPTURE_CAPACITY);
        let shared = Arc::new(Shared::default());
        let mut processor = Processor::new(sample_rate);
        processor.set_capture(Some(capture_tx));
        let realtime = Realtime {
            processor,
            messages: message_rx,
            garbage: garbage_tx,
            live: live.queue(),
            live_overflowed: live.overflowed(),
            shared: shared.clone(),
            left: vec![0.0; CALLBACK_FRAMES],
            right: vec![0.0; CALLBACK_FRAMES],
        };
        let seat = Arc::new(Seat {
            active: AtomicU64::new(generation),
            claimed: AtomicBool::new(false),
            realtime: UnsafeCell::new(realtime),
        });
        Self { messages: message_tx, garbage: garbage_rx, captured: capture_rx, shared, seat }
    }
}

impl Engine {
    pub fn start(backend: AudioBackend) -> Result<Self, EngineError> {
        Self::start_with(backend.into())
    }

    /// Starts an engine on the first device that can be opened: the one
    /// `setup` asks for, as it asks for it, or failing that something
    /// nearby ([`StreamInfo::warning`] then says what and why).
    pub fn start_with(setup: AudioSetup) -> Result<Self, EngineError> {
        let live = LiveSender::new();
        let core = Core::new(FALLBACK_SAMPLE_RATE as f64, &live, 0);
        let now = Instant::now();
        let mut engine = Self {
            messages: core.messages,
            garbage: core.garbage,
            captured: core.captured,
            live,
            shared: core.shared,
            seat: core.seat,
            generation: 0,
            info: StreamInfo {
                sample_rate: FALLBACK_SAMPLE_RATE as f64,
                output: String::new(),
                warning: None,
                device: None,
                buffer_frames: 0,
                exclusive: false,
                latency_frames: 0,
            },
            swaps_in_flight: 0,
            host: setup.host,
            null: Arc::new(NullHost::new(FALLBACK_SAMPLE_RATE)),
            config: setup.config,
            patience: setup.patience,
            stream: None,
            driver: Driver::start()?,
            watched: false,
            last_callbacks: 0,
            last_progress: now,
            missing: Missing::No,
            retrying: false,
            last_retry: now,
            lookup: None,
        };
        engine.open(setup.silent_fallback)?;
        Ok(engine)
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn sample_rate(&self) -> f64 {
        self.info.sample_rate
    }

    /// What was asked of the audio hardware; [`Engine::info`] is what came
    /// of it.
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }

    /// The devices that could be chosen. Asks the system, which can take a
    /// moment; if it does not answer at all the lists come back empty.
    pub fn devices(&mut self) -> AudioDevices {
        let host = self.host.clone();
        self.driver.call(self.patience.device, move || host.devices()).unwrap_or_default()
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Song position in beats, as of the last audio callback.
    pub fn position(&self) -> f64 {
        f64::from_bits(self.shared.position.load(Ordering::Relaxed))
    }

    /// Fraction of the audio callback's time budget in use (1.0 = overload).
    pub fn load(&self) -> f32 {
        f32::from_bits(self.shared.load.load(Ordering::Relaxed))
    }

    /// Whether a sound is being auditioned, as of the last audio callback.
    pub fn is_auditioning(&self) -> bool {
        self.shared.auditioning.load(Ordering::Relaxed)
    }

    /// Number of audio blocks lost to a panicking device.
    pub fn faults(&self) -> u32 {
        self.shared.faults.load(Ordering::Relaxed)
    }

    /// How long after a frame is rendered it is heard, in frames, as the
    /// device reported with its last buffer.
    pub fn output_latency(&self) -> u32 {
        self.shared.latency_frames.load(Ordering::Relaxed)
    }

    /// How many times live input arrived faster than the engine could take
    /// it and some was lost; every sounding note was let go of each time.
    /// Never, in ordinary playing.
    pub fn live_dropped(&self) -> u32 {
        self.shared.live_dropped.load(Ordering::Relaxed)
    }

    /// The way in for live input.
    pub fn live(&self) -> &LiveSender {
        &self.live
    }

    pub(crate) fn record_phase(&self) -> RecordPhase {
        match self.shared.recording.load(Ordering::Relaxed) {
            1 => RecordPhase::CountIn,
            2 => RecordPhase::Rolling,
            _ => RecordPhase::Off,
        }
    }

    /// The next thing the audio thread reported about the take being
    /// recorded.
    pub(crate) fn next_captured(&mut self) -> Option<Captured> {
        self.captured.pop().ok()
    }

    /// Gives the audio thread a turn if it is one that only runs when told
    /// to; see [`AudioHost::pump`].
    pub(crate) fn pump(&self) {
        self.host.pump();
    }

    /// The time on the clock the audio thread reads; see
    /// [`AudioHost::now_ns`].
    pub(crate) fn now_ns(&self) -> u64 {
        self.host.now_ns()
    }

    /// Frees graphs the audio thread has retired. Cheap; call it often.
    pub fn collect_garbage(&mut self) {
        // Dropping here, on the caller's thread, is the whole point.
        while let Ok(garbage) = self.garbage.pop() {
            match garbage {
                Garbage::Graph(graph) => {
                    self.swaps_in_flight = self.swaps_in_flight.saturating_sub(1);
                    drop(graph);
                }
                Garbage::Audio(audio) => drop(audio),
            }
        }
    }

    pub(crate) fn swap(&mut self, graph: Box<Graph>) {
        self.collect_garbage();
        // Never have more graphs outstanding than the return queue holds, so
        // the audio thread can always hand the old one back.
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.swaps_in_flight >= GARBAGE_CAPACITY - 1 && Instant::now() < deadline {
            self.wait_a_moment();
            self.collect_garbage();
        }
        self.swaps_in_flight += 1;
        self.send(Message::Swap(graph));
    }

    pub(crate) fn send(&mut self, message: Message) {
        let mut message = message;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.messages.push(message) {
                Ok(()) => return,
                Err(rtrb::PushError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        log::error!("audio thread is not consuming messages; dropping one");
                        return;
                    }
                    message = returned;
                    self.wait_a_moment();
                }
            }
        }
    }

    /// Lets the audio thread get on with it for a millisecond.
    fn wait_a_moment(&self) {
        self.host.pump();
        thread::sleep(Duration::from_millis(1));
    }

    /// Asks for a different audio stream and opens it, while the program
    /// runs. The graph and every running device carry over unless the
    /// sample rate changes ([`Applied::reset`]).
    ///
    /// This does not fail: if what was asked for cannot be had, something
    /// nearby is opened, down to no device at all, and
    /// [`StreamInfo::warning`] says what and why. `config` is kept as the
    /// wish either way.
    pub(crate) fn apply(&mut self, config: AudioConfig) -> Applied {
        self.config = config;
        self.reopen()
    }

    /// Opens the stream again as [`Engine::config`] asks: after a failure,
    /// or when a missing device may be back.
    pub(crate) fn reopen(&mut self) -> Applied {
        match self.open(true) {
            Ok(applied) => applied,
            Err(error) => {
                // Not even the silent stand-in could be started. There is
                // no stream; the next round of chores tries again.
                log::error!("{error}");
                self.info.warning = Some(error.to_string());
                self.missing = Missing::Absent;
                Applied { reset: false }
            }
        }
    }

    /// Puts on record, for whoever reads [`Engine::info`], that the stream
    /// in use took over from one that failed, and why.
    pub(crate) fn note_failure(&mut self, reason: &str) {
        let mut text = reason.to_string();
        if let Some(first) = text.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        let now = match &self.info.device {
            Some(_) => format!("Now using {}.", self.info.output),
            None => "Running without sound until an audio device is there.".to_string(),
        };
        self.info.warning = Some(format!("{text}. {now}"));
    }

    /// Looks at how the stream is doing. Cheap; call it a few times a
    /// second.
    pub(crate) fn check(&mut self) -> Health {
        let now = Instant::now();
        if let Some(failure) = StreamFailure::from_code(self.shared.failure.swap(0, Ordering::Relaxed)) {
            return Health::Failed(failure);
        }
        if self.watched {
            let callbacks = self.shared.callbacks.load(Ordering::Relaxed);
            if callbacks != self.last_callbacks {
                self.last_callbacks = callbacks;
                self.last_progress = now;
            } else if now.duration_since(self.last_progress) >= self.patience.stall {
                self.last_progress = now;
                return Health::Failed(StreamFailure::Stalled);
            }
        }
        if let Some(lookup) = &self.lookup {
            // Asking the system for its devices takes a moment, so it is
            // not waited for: the answer is picked up on a later round.
            match lookup.try_recv() {
                Ok(present) => {
                    self.lookup = None;
                    match (self.missing, present) {
                        (Missing::Absent, true) => {
                            self.retrying = true;
                            return Health::Retry;
                        }
                        (Missing::Unseen, false) => self.missing = Missing::Absent,
                        _ => {}
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.lookup = None,
            }
        } else if self.missing != Missing::No && now.duration_since(self.last_retry) >= self.patience.retry {
            self.last_retry = now;
            let (host, wanted) = (self.host.clone(), self.config.output.clone());
            let (answer, lookup) = mpsc::channel();
            self.lookup = Some(lookup);
            self.driver.post(move || {
                let outputs = host.devices().outputs;
                let back = match wanted {
                    Some(name) => outputs.iter().any(|device| device.name == name),
                    None => !outputs.is_empty(),
                };
                let _ = answer.send(back);
            });
        }
        Health::Good
    }

    /// Closes the running stream, waiting for its last callback.
    fn close(&mut self) {
        // From here on the stream renders silence, whatever becomes of it.
        self.generation += 1;
        self.seat.active.store(self.generation, Ordering::Release);
        if let Some(stream) = self.stream.take() {
            self.driver.call(self.patience.device, move || drop(stream));
        }
        self.watched = false;
    }

    /// Replaces the queues and the processor with new ones, for when the
    /// audio thread never came out of the old ones. What is stuck in there
    /// stays there.
    fn start_over(&mut self) {
        log::error!("the audio thread is stuck inside a device; starting over without it");
        let core = Core::new(self.info.sample_rate, &self.live, self.generation);
        self.messages = core.messages;
        self.garbage = core.garbage;
        self.captured = core.captured;
        self.shared = core.shared;
        self.seat = core.seat;
        self.swaps_in_flight = 0;
    }

    /// Closes the stream and opens one as the configuration asks, or the
    /// nearest thing to it. Fails only if nothing at all can be opened and
    /// `silent_fallback` is off.
    fn open(&mut self, silent_fallback: bool) -> Result<Applied, EngineError> {
        self.close();
        let mut reset = false;
        if self.seat.claim(self.patience.audio_thread).is_none() {
            self.start_over();
            reset = true;
        }
        let seat = self.seat.clone();
        // Held until the new stream is ready: its callbacks render silence
        // meanwhile.
        let mut claim = seat.try_claim().ok_or(EngineError::ThreadDied)?;

        let shared = self.shared.clone();
        let on_failure: FailureCallback = Arc::new(move |failure: StreamFailure| {
            shared.failure.store(failure.code(), Ordering::Relaxed);
        });

        let wish = StreamRequest {
            device: self.config.output.clone(),
            sample_rate: self.config.sample_rate,
            buffer_size: self.config.buffer_size,
            exclusive: self.config.exclusive,
        };
        let mut refusals: Vec<String> = Vec::new();
        let mut missing = false;
        let mut opened: Option<(OpenStream, bool)> = None;
        for request in candidates(&wish) {
            self.generation += 1;
            let renderer = Renderer::new(seat.clone(), self.generation);
            let (host, attempt, failed) = (self.host.clone(), request.clone(), on_failure.clone());
            let result = self
                .driver
                .call(self.patience.device, move || host.open_output(&attempt, renderer, failed))
                .unwrap_or_else(|| Err(OpenError::Failed("the device did not answer".into())));
            match result {
                Ok(stream) => {
                    opened = Some((stream, true));
                    break;
                }
                Err(error) => {
                    missing |= matches!(error, OpenError::NotFound(_) | OpenError::NoDevice);
                    refusals.push(format!("{}: {error}", describe(&request)));
                }
            }
        }
        if opened.is_none() {
            if !silent_fallback {
                return Err(EngineError::NoOutput(refusals.join("; ")));
            }
            self.generation += 1;
            let renderer = Renderer::new(seat.clone(), self.generation);
            let request = StreamRequest { device: None, sample_rate: None, buffer_size: None, exclusive: false };
            match self.null.open_output(&request, renderer, on_failure) {
                Ok(stream) => opened = Some((stream, false)),
                Err(error) => return Err(EngineError::NoOutput(error.to_string())),
            }
        }
        let (stream, real) = opened.expect("a stream was opened or the function returned");

        let format = stream.format;
        let realtime = claim.realtime();
        if reset || realtime.processor.sample_rate() != format.sample_rate as f64 {
            // The old processor goes here, on the controller's thread, with
            // its graph and every device in it.
            let mut processor = Processor::new(format.sample_rate as f64);
            processor.set_capture(realtime.processor.set_capture(None));
            realtime.processor = processor;
            reset = true;
        }
        self.seat.active.store(self.generation, Ordering::Release);
        drop(claim);
        self.stream = Some(stream.handle);

        let first = refusals.first().cloned();
        let warning = match (&first, real) {
            (None, _) => None,
            (Some(first), true) => Some(format!("{first}. Using {} instead.", describe_format(&format))),
            (Some(first), false) => Some(format!("no audio output ({first}); running without sound")),
        };
        if let Some(warning) = &warning {
            log::warn!("{warning}");
        }
        self.info = StreamInfo {
            sample_rate: format.sample_rate as f64,
            output: if real { describe_format(&format) } else { "none (null backend)".into() },
            warning,
            device: (real && format.device != NULL_DEVICE).then(|| format.device.clone()),
            buffer_frames: format.buffer_frames,
            exclusive: format.exclusive,
            latency_frames: format.latency_frames,
        };
        self.shared.latency_frames.store(format.latency_frames, Ordering::Relaxed);
        self.shared.failure.store(0, Ordering::Relaxed);
        let now = Instant::now();
        self.watched = real && self.host.runs_by_itself();
        self.last_callbacks = self.shared.callbacks.load(Ordering::Relaxed);
        self.last_progress = now;
        // Worth looking again only where a device was not there: one that
        // is there and refuses a format will refuse it next time too. And
        // if this very attempt was made because the device looked to be
        // back, it must be seen to go before it is tried once more: a
        // device that is listed and cannot be opened would otherwise have
        // the stream opened again and again.
        self.missing = match (missing && first.is_some(), std::mem::take(&mut self.retrying)) {
            (false, _) => Missing::No,
            (true, false) => Missing::Absent,
            (true, true) => Missing::Unseen,
        };
        self.last_retry = now;
        self.lookup = None;
        Ok(Applied { reset })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.close();
        self.collect_garbage();
    }
}

/// What to try, in order, to honour `wish`: the wish itself, then the same
/// device with less asked of it, then the system's default device.
fn candidates(wish: &StreamRequest) -> Vec<StreamRequest> {
    let mut list = vec![wish.clone()];
    let mut push = |request: StreamRequest| {
        if !list.contains(&request) {
            list.push(request);
        }
    };
    push(StreamRequest { exclusive: false, ..wish.clone() });
    push(StreamRequest { sample_rate: None, buffer_size: None, exclusive: false, device: wish.device.clone() });
    push(StreamRequest { device: None, sample_rate: None, buffer_size: None, exclusive: false });
    list
}

/// A request in words, for saying why it was refused.
fn describe(request: &StreamRequest) -> String {
    let mut text = match &request.device {
        Some(name) => format!("'{name}'"),
        None => "the default output".to_string(),
    };
    let mut details = Vec::new();
    if let Some(rate) = request.sample_rate {
        details.push(format!("{rate} Hz"));
    }
    if let Some(frames) = request.buffer_size {
        details.push(format!("{frames} frames"));
    }
    if request.exclusive {
        details.push("exclusive".to_string());
    }
    if !details.is_empty() {
        text.push_str(&format!(" ({})", details.join(", ")));
    }
    text
}

fn describe_format(format: &crate::host::StreamFormat) -> String {
    let mode = if format.exclusive { ", exclusive" } else { "" };
    format!("{} ({} Hz{mode})", format.device, format.sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_tried_goes_from_the_wish_to_the_default_device() {
        let wish = StreamRequest {
            device: Some("DAC".into()),
            sample_rate: Some(96_000),
            buffer_size: Some(64),
            exclusive: true,
        };
        let list = candidates(&wish);
        assert_eq!(list.len(), 4);
        assert_eq!(list[0], wish);
        assert_eq!(list[1], StreamRequest { exclusive: false, ..wish.clone() });
        assert_eq!(
            list[2],
            StreamRequest { device: Some("DAC".into()), sample_rate: None, buffer_size: None, exclusive: false }
        );
        assert_eq!(list[3], StreamRequest { device: None, sample_rate: None, buffer_size: None, exclusive: false });

        // Nothing asked for: one attempt, at the default device.
        let plain = StreamRequest { device: None, sample_rate: None, buffer_size: None, exclusive: false };
        assert_eq!(candidates(&plain), [plain]);
    }

    #[test]
    fn a_request_reads_as_what_was_asked() {
        let request = StreamRequest {
            device: Some("DAC".into()),
            sample_rate: Some(96_000),
            buffer_size: Some(64),
            exclusive: true,
        };
        assert_eq!(describe(&request), "'DAC' (96000 Hz, 64 frames, exclusive)");
        let plain = StreamRequest { device: None, sample_rate: None, buffer_size: None, exclusive: false };
        assert_eq!(describe(&plain), "the default output");
    }
}
