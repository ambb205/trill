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
//! can change while the program runs (`Engine::apply`), and a device can
//! disappear (`Engine::check`). Through all of it the processor, with the
//! graph and every running device, stays where it is, in a `Seat` that one
//! stream after another renders from.

use crate::graph::{AudioData, Graph};
use crate::host::priority::AudioPriority;
use crate::host::{
    AudioConfig, AudioDevices, AudioHost, FailureCallback, NULL_DEVICE, NullHost, OpenError, OpenStream, StreamFailure,
    StreamHandle, StreamRequest, SystemHost, Timing,
};
use crate::live::{Captured, LiveInput, LiveSender, LiveTarget, TrackSet};
use crate::processor::{Processor, Taken};
use crate::queue::Queue;
use crate::spread::Workers;
use crate::team::{Team, TeamStats, Watch};
use std::cell::UnsafeCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use trill_core::DeviceId;

const MESSAGE_CAPACITY: usize = 1024;
/// How long a message waits for room in a queue the audio thread is
/// emptying, and in one it shows no sign of emptying.
const SEND_WAIT: Duration = Duration::from_secs(2);
const SEND_WAIT_AWAY: Duration = Duration::from_millis(250);
/// A key that reaches the audio thread this long after it went down (the
/// stream was away) is not sounded late.
const STALE_SECONDS: f64 = 0.5;
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
/// The variable that says how many threads a song that plays may be made
/// on, for finding out what that does: 1 is the audio callback alone.
pub const PLAY_THREADS_ENV: &str = "TRILL_PLAY_THREADS";

/// How many threads a song that plays may be made on unless somebody says
/// otherwise: half the machine's processors, the audio callback's own
/// among them, and eight at the most. Half, because the threads are at
/// the priority the system keeps for audio, and what is left has to draw
/// the window and run whatever else the machine is doing; and because the
/// second half of the processors a machine counts are mostly the first
/// half again. [`PLAY_THREADS_ENV`] in the environment says another
/// number.
pub fn usual_play_threads() -> usize {
    let asked = std::env::var(PLAY_THREADS_ENV).ok().and_then(|text| text.trim().parse::<usize>().ok());
    let usual = || std::thread::available_parallelism().map_or(1, |count| count.get() / 2).clamp(1, 8);
    asked.unwrap_or_else(usual).max(1)
}

/// How the audio thread has kept up since that was last asked
/// ([`Engine::take_load`]).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Load {
    /// How many buffers the device asked for.
    pub callbacks: u64,
    /// How many of them took longer to make than they last: each is a
    /// gap in what is heard.
    pub late: u64,
    /// The share of the time there was that making them took, over all of
    /// them and for the worst one: 1 is all of it.
    pub mean: f32,
    pub worst: f32,
    /// How many blocks were made on more than one thread.
    pub spread_blocks: u64,
    /// How many threads make the blocks now, the callback's among them.
    pub threads: usize,
    /// How many the song could use at the most, and how many it may have.
    pub allowed: usize,
    /// What the threads beside the callback's did, where there are any.
    pub team: Option<TeamStats>,
    /// How many frames the device asked for in all, and the fewest and the
    /// most at a time. A device that is shared with other programs does
    /// not always ask for the same.
    pub frames: u64,
    pub smallest_frames: u32,
    pub largest_frames: u32,
    /// The longest the device let pass between asking for one buffer and
    /// asking for the next, in nanoseconds.
    pub longest_gap_ns: u64,
    /// Whether the system has the callback's thread in its audio class
    /// ([`AudioPriority`](crate::host::priority::AudioPriority)): `None`
    /// where it was never asked for the thread (a host driven by hand).
    pub audio_class: Option<bool>,
    /// The priority the system says that thread has, by its own numbers,
    /// and the one it had before Trill asked for the audio class.
    pub thread_level: Option<i32>,
    pub thread_level_before: Option<i32>,
}

impl Load {
    /// Adds what `later` counted to this, for keeping count over longer
    /// than the engine was asked at: what is counted adds up, what is a
    /// most or a least stays one, and what says how things stand (the
    /// threads, the thread's class) is as `later` has it.
    pub fn add(&mut self, later: &Load) {
        let frames = self.frames + later.frames;
        if frames > 0 {
            // (The time a buffer had is its frames: the mean of two means
            // is theirs weighed by that.)
            let weighed = f64::from(self.mean) * self.frames as f64 + f64::from(later.mean) * later.frames as f64;
            self.mean = (weighed / frames as f64) as f32;
        }
        self.frames = frames;
        self.callbacks += later.callbacks;
        self.late += later.late;
        self.worst = self.worst.max(later.worst);
        self.spread_blocks += later.spread_blocks;
        (self.threads, self.allowed) = (later.threads, later.allowed);
        self.team = match (self.team, later.team) {
            (Some(before), Some(after)) => Some(before.plus(&after)),
            (before, after) => after.or(before),
        };
        self.smallest_frames = match (self.smallest_frames, later.smallest_frames) {
            (0, least) | (least, 0) => least,
            (one, other) => one.min(other),
        };
        self.largest_frames = self.largest_frames.max(later.largest_frames);
        self.longest_gap_ns = self.longest_gap_ns.max(later.longest_gap_ns);
        self.audio_class = later.audio_class.or(self.audio_class);
        self.thread_level = later.thread_level.or(self.thread_level);
        self.thread_level_before = later.thread_level_before.or(self.thread_level_before);
    }
}

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
        round: Option<crate::monitor::Round>,
    },
    StopAudition,
    Metronome(bool),
    /// The tracks a keyboard plays from now on.
    LiveTargets(TrackSet),
    /// Lets go of every note a keyboard is holding: its port has gone.
    ReleaseLive,
    /// Lets go of every note that sounds, played by hand or by the song.
    ReleaseEverything,
    /// The threads the song that plays may be made on beside the
    /// callback's, or none.
    Team(Option<Box<Team>>),
    /// The share of a block's time above which a song that plays is
    /// spread: see [`Processor::spread_from`].
    SpreadFrom(f64),
    /// Count the cycles threads run over each track's block, or stop.
    CountCycles(bool),
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
    /// Threads the song is no longer made on: sent home, and waited for,
    /// by whoever collects this.
    Team(Box<Team>),
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
    /// New notes that were not sounded: there was no room, or they came
    /// too late.
    live_skipped: AtomicU32,
    /// The song that plays would be spread over more threads if it had
    /// any: see [`Processor::wants_threads`].
    wants_threads: AtomicBool,
    /// How many threads make the blocks now.
    threads: AtomicU32,
    /// Since [`Engine::take_load`] was last asked: the buffers counted,
    /// those that took longer than they last, the time making them took
    /// and the time they last (nanoseconds), the worst share (the bits of
    /// an `f32`) and the blocks that were spread.
    counted: AtomicU64,
    late: AtomicU64,
    busy_ns: AtomicU64,
    given_ns: AtomicU64,
    worst: AtomicU32,
    spread: AtomicU64,
    /// Since then too: the frames asked for, the fewest and the most a
    /// buffer had, and the longest time between two buffers.
    frames: AtomicU64,
    smallest: AtomicU32,
    largest: AtomicU32,
    longest_gap_ns: AtomicU64,
    /// What the callback's thread is, as of the first buffer since: 0
    /// where its audio class was never asked for, 1 where the system
    /// refused it, 2 where it granted it; and the thread's priority by the
    /// system's numbers, offset so that 0 says none was read.
    audio_class: AtomicU32,
    thread_level: AtomicU32,
    thread_level_before: AtomicU32,
}

/// What is added to a thread's priority to keep it in [`Shared`], where 0
/// says that none was read.
const LEVEL_OFFSET: i32 = 1 << 16;

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
    /// When the device last asked for a buffer.
    asked: Option<Instant>,
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
                Message::Audition { audio, gain, round } => {
                    if let Some(previous) = self.processor.start_audition(audio, gain, round) {
                        let _ = self.garbage.push(Garbage::Audio(previous));
                    }
                }
                Message::StopAudition => self.processor.stop_audition(),
                Message::Metronome(on) => self.processor.set_metronome(on),
                Message::LiveTargets(tracks) => self.processor.set_live_targets(tracks),
                Message::ReleaseLive => self.processor.release_live(),
                Message::ReleaseEverything => self.processor.release_everything(),
                Message::Team(team) => {
                    // The ones it had are sent home elsewhere: that waits.
                    if let Some(before) = self.processor.play_over(team) {
                        let _ = self.garbage.push(Garbage::Team(before));
                    }
                }
                Message::SpreadFrom(share) => self.processor.spread_from(share),
                Message::CountCycles(on) => self.processor.count_cycles(on),
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
        let stale = STALE_SECONDS * self.processor.sample_rate();
        let mut lost = self.live_overflowed.swap(false, Ordering::Relaxed);
        while let Some(input) = self.live.pop() {
            let age = timing.now_ns.saturating_sub(input.time_ns) as f64 * frames_per_ns;
            // A key that went down while there was no stream to sound it
            // is not sounded now, long after. Its coming up, the pedal and
            // the wheels still count: they say how things stand.
            if age > stale && matches!(input.kind, trill_core::device::EventKind::NoteOn { .. }) {
                self.shared.live_skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let offset = (frames as f64 - age).round().clamp(0.0, frames.saturating_sub(1) as f64);
            // How long before the frame it is placed at the event happened.
            let early = offset + age;
            let taken = match input.target {
                LiveTarget::Routed => self.processor.live_routed(input.kind, offset as u32, early),
                LiveTarget::Track(track) => self.processor.live_on(track, input.kind, offset as u32, early),
            };
            match taken {
                Taken::Kept => {}
                Taken::Skipped => {
                    self.shared.live_skipped.fetch_add(1, Ordering::Relaxed);
                }
                Taken::Lost => lost = true,
            }
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
            processor.after_fault();
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
        {
            let shared = &self.shared;
            let frames = total.min(u32::MAX as usize) as u32;
            shared.frames.fetch_add(total as u64, Ordering::Relaxed);
            shared.largest.fetch_max(frames, Ordering::Relaxed);
            // (Nothing asked for yet is 0, which no buffer is smaller than.)
            let smallest = shared.smallest.load(Ordering::Relaxed);
            if frames > 0 && (smallest == 0 || frames < smallest) {
                shared.smallest.store(frames, Ordering::Relaxed);
            }
            if let Some(before) = self.asked.replace(started) {
                let gap = started.saturating_duration_since(before).as_nanos() as u64;
                shared.longest_gap_ns.fetch_max(gap, Ordering::Relaxed);
            }
            // What this thread is: a look at a thread-local with every
            // buffer and, with the first since that was last asked, one
            // call that asks the system about the calling thread.
            let class = match AudioPriority::of_this_thread() {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            };
            shared.audio_class.store(class, Ordering::Relaxed);
            if shared.counted.load(Ordering::Relaxed) == 0 {
                let level = AudioPriority::level_of_this_thread().map_or(0, |level| (level + LEVEL_OFFSET) as u32);
                shared.thread_level.store(level, Ordering::Relaxed);
                let before = AudioPriority::level_before().map_or(0, |level| (level + LEVEL_OFFSET) as u32);
                shared.thread_level_before.store(before, Ordering::Relaxed);
            }
        }
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
        shared.wants_threads.store(self.processor.wants_threads(), Ordering::Relaxed);
        shared.threads.store(self.processor.threads() as u32, Ordering::Relaxed);
        shared.spread.fetch_add(self.processor.take_spread_blocks(), Ordering::Relaxed);
        let available = total as f64 / self.processor.sample_rate();
        if available > 0.0 {
            let took = started.elapsed().as_secs_f64();
            let load = (took / available) as f32;
            shared.load.store(load.to_bits(), Ordering::Relaxed);
            shared.counted.fetch_add(1, Ordering::Relaxed);
            shared.busy_ns.fetch_add((took * 1e9) as u64, Ordering::Relaxed);
            shared.given_ns.fetch_add((available * 1e9) as u64, Ordering::Relaxed);
            // The bits of a float that is not negative are in its order.
            shared.worst.fetch_max(load.max(0.0).to_bits(), Ordering::Relaxed);
            if load > 1.0 {
                shared.late.fetch_add(1, Ordering::Relaxed);
            }
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
    /// Who takes the graphs the audio thread retires, if not whoever
    /// collects them: see [`Engine::retire_through`].
    retire: Option<Box<dyn Fn(Box<Graph>) + Send>>,
    /// Set when a message could not be sent; see [`Engine::send`].
    messages_lost: bool,
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
    /// How many threads a song that plays may be made on, the callback's
    /// among them; how many the graph that plays can use; and the ones
    /// that were made for it, as seen from here.
    play_threads: usize,
    can_use: usize,
    team: Option<(usize, Watch)>,
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
            asked: None,
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
            retire: None,
            messages_lost: false,
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
            play_threads: usual_play_threads(),
            can_use: 1,
            team: None,
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

    /// How the audio thread has kept up since this was last asked: see
    /// [`Load`].
    pub fn take_load(&mut self) -> Load {
        let shared = &self.shared;
        let take = |count: &AtomicU64| count.swap(0, Ordering::Relaxed);
        let (busy, given) = (take(&shared.busy_ns), take(&shared.given_ns));
        Load {
            callbacks: take(&shared.counted),
            late: take(&shared.late),
            mean: if given == 0 { 0.0 } else { (busy as f64 / given as f64) as f32 },
            worst: f32::from_bits(shared.worst.swap(0, Ordering::Relaxed)),
            spread_blocks: take(&shared.spread),
            threads: shared.threads.load(Ordering::Relaxed).max(1) as usize,
            allowed: self.play_threads.min(self.can_use).max(1),
            team: self.team.as_ref().map(|(_, watch)| watch.take()),
            frames: take(&shared.frames),
            smallest_frames: shared.smallest.swap(0, Ordering::Relaxed),
            largest_frames: shared.largest.swap(0, Ordering::Relaxed),
            longest_gap_ns: take(&shared.longest_gap_ns),
            audio_class: match shared.audio_class.load(Ordering::Relaxed) {
                0 => None,
                class => Some(class == 2),
            },
            thread_level: match shared.thread_level.load(Ordering::Relaxed) {
                0 => None,
                level => Some(level as i32 - LEVEL_OFFSET),
            },
            thread_level_before: match shared.thread_level_before.load(Ordering::Relaxed) {
                0 => None,
                level => Some(level as i32 - LEVEL_OFFSET),
            },
        }
    }

    /// How many threads a song that plays may be made on, the audio
    /// callback's among them.
    pub fn play_threads(&self) -> usize {
        self.play_threads
    }

    /// Says how many threads a song that plays may be made on, the audio
    /// callback's among them: [`usual_play_threads`] where nobody says.
    /// One has the callback make everything, as it did before there were
    /// others. More are started when a song first needs them
    /// ([`Engine::tend_threads`]), and no sooner.
    pub(crate) fn set_play_threads(&mut self, threads: usize) {
        let threads = threads.max(1);
        if threads == self.play_threads {
            return;
        }
        self.play_threads = threads;
        // The ones there are go home; the right number comes when it is
        // next needed.
        if self.team.take().is_some() {
            self.send(Message::Team(None));
        }
    }

    /// Says how many threads the graph that was last handed over can use:
    /// see [`Graph::spread`].
    pub(crate) fn can_use(&mut self, threads: usize) {
        self.can_use = threads.max(1);
    }

    /// Gives the song that plays its threads, where it may have them and
    /// can use them. Cheap while there is nothing to do: it is called with
    /// every graph that is handed over and a few times a second. Threads
    /// are started here and nowhere else, never by the audio thread.
    ///
    /// A stream whose own thread is an audio thread (a device's, which
    /// asked the system for its audio class) has them as soon as its song
    /// has anything to make at once, before a note of it is heard: what a
    /// song costs is only known once it has played, and one that turns out
    /// to need them has no buffer to spare for their being made. A stream
    /// that is driven by hand, or runs with no device, has them when its
    /// song asks ([`Processor::wants_threads`]).
    pub(crate) fn tend_threads(&mut self) {
        // All of them at once: the ones a song cannot use yet sleep.
        let wanted = if self.can_use >= 2 { self.play_threads } else { 1 };
        let has = self.team.as_ref().map_or(0, |(threads, _)| *threads);
        let asked = self.shared.wants_threads.load(Ordering::Relaxed);
        let at_once = self.shared.audio_class.load(Ordering::Relaxed) != 0;
        if !(asked || at_once || has > 0) || wanted < 2 || has >= wanted - 1 {
            return;
        }
        let team = Box::new(Team::new(wanted - 1));
        let (made, watch) = (team.len(), team.watch());
        if made > 0 && self.send(Message::Team(Some(team))) {
            log::info!("the song is made on {} threads from here on", made + 1);
            self.team = Some((made, watch));
        }
    }

    /// Sets the share of a block's time above which a song that plays is
    /// spread over its threads: see [`Processor::spread_from`].
    pub(crate) fn spread_from(&mut self, share: f64) {
        self.send(Message::SpreadFrom(share));
    }

    /// Has the cycles threads run over each track's block counted, or
    /// stops that: see [`Processor::count_cycles`].
    pub(crate) fn count_cycles(&mut self, on: bool) {
        self.send(Message::CountCycles(on));
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

    /// How many new notes were not sounded: more came at once than there
    /// was room for, or they reached the audio thread long after they were
    /// played because the stream was away. Nothing hangs for these.
    pub fn live_skipped(&self) -> u32 {
        self.shared.live_skipped.load(Ordering::Relaxed)
    }

    /// Whether messages to the audio thread were lost because it was not
    /// taking any, now that it is taking them again. Asked once it is
    /// `true`, it is `false` until more are lost: whoever asks then brings
    /// the audio thread back in line with everything it missed.
    pub(crate) fn take_lost_messages(&mut self) -> bool {
        let recovered = self.messages_lost && self.messages.slots() >= MESSAGE_CAPACITY / 2;
        if recovered {
            self.messages_lost = false;
        }
        recovered
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

    /// Has the graphs the audio thread retires handed to `take` from here
    /// on, in place of being freed by whoever collects them: a graph holds
    /// the devices that went with it, and a plugin among them is taken
    /// down on its own thread, which is waited for.
    pub(crate) fn retire_through(&mut self, take: Box<dyn Fn(Box<Graph>) + Send>) {
        self.retire = Some(take);
    }

    /// Frees graphs the audio thread has retired. Cheap; call it often.
    pub fn collect_garbage(&mut self) {
        // Dropping here, on the caller's thread and not the audio thread's,
        // is the whole point; or handing on to whoever takes them
        // ([`Engine::retire_through`]).
        while let Ok(garbage) = self.garbage.pop() {
            match garbage {
                Garbage::Graph(graph) => {
                    self.swaps_in_flight = self.swaps_in_flight.saturating_sub(1);
                    match &self.retire {
                        Some(take) => take(graph),
                        None => drop(graph),
                    }
                }
                Garbage::Audio(audio) => drop(audio),
                // Its threads are told to go home and waited for.
                Garbage::Team(team) => drop(team),
            }
        }
    }

    pub(crate) fn swap(&mut self, graph: Box<Graph>) {
        self.collect_garbage();
        // Never have more graphs outstanding than the return queue holds, so
        // the audio thread can always hand the old one back.
        let deadline = Instant::now() + SEND_WAIT;
        while self.swaps_in_flight >= GARBAGE_CAPACITY - 1 && !self.messages_lost && Instant::now() < deadline {
            self.wait_a_moment();
            self.collect_garbage();
        }
        self.swaps_in_flight += 1;
        if !self.send(Message::Swap(graph)) {
            // It never left: there is nothing to come back.
            self.swaps_in_flight -= 1;
        }
    }

    /// Sends a message to the audio thread and says whether it went.
    ///
    /// The queue is long, so there is room unless the audio thread has
    /// stopped taking messages: the device is being changed, or has
    /// stalled. Then this waits, but not for long and not twice: two
    /// seconds for an audio thread that is still rendering, a quarter of a
    /// second for one that shows no sign of life, and not at all once a
    /// message has been lost and the queue is still full. A caller is never
    /// held up for seconds on end, time after time, by a device that has
    /// gone away.
    ///
    /// A message that is lost is not forgotten: [`Engine::take_lost_messages`]
    /// says so once the audio thread is back, and the session then tells
    /// it again everything it needs to know.
    pub(crate) fn send(&mut self, message: Message) -> bool {
        let mut message = message;
        let started = Instant::now();
        let rendered = self.shared.callbacks.load(Ordering::Relaxed);
        loop {
            match self.messages.push(message) {
                Ok(()) => return true,
                Err(rtrb::PushError::Full(returned)) => {
                    let alive = self.shared.callbacks.load(Ordering::Relaxed) != rendered;
                    let patience = match (self.messages_lost, alive) {
                        (true, _) => Duration::ZERO,
                        (false, true) => SEND_WAIT,
                        (false, false) => SEND_WAIT_AWAY,
                    };
                    if started.elapsed() >= patience {
                        if !self.messages_lost {
                            log::error!("the audio thread is not taking messages; they are told again when it is back");
                        }
                        self.messages_lost = true;
                        return false;
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
        // The threads it was made on stay with what is stuck.
        self.team = None;
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
                    // A device that is not there says so, by name. Anything
                    // else is the refusal of a request, and it is the
                    // request that needs describing.
                    refusals.push(match &error {
                        OpenError::NotFound(_) | OpenError::NoDevice => error.to_string(),
                        _ => format!("{}: {error}", describe(&request)),
                    });
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
            // The old processor goes here, on the controller's thread. Its
            // graph, with every device in it, goes to whoever takes what is
            // retired: plugins are let go of on a thread of their own, and
            // waited for.
            let mut processor = Processor::new(format.sample_rate as f64);
            processor.set_capture(realtime.processor.set_capture(None));
            let old = realtime.processor.swap_graph(Box::new(Graph::empty()));
            match &self.retire {
                Some(take) => take(old),
                None => drop(old),
            }
            realtime.processor = processor;
            reset = true;
        }
        self.seat.active.store(self.generation, Ordering::Release);
        drop(claim);
        self.stream = Some(stream.handle);

        // Whether anything is heard: not through the stand-in, and not
        // through a host that is itself no device.
        let audible = real && format.device != NULL_DEVICE;
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
            output: if audible { describe_format(&format) } else { "none (null backend)".into() },
            warning,
            device: audible.then(|| format.device.clone()),
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
