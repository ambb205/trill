//! The realtime engine: owns the audio thread and talks to it through
//! lock-free queues.
//!
//! The controller never touches the processor directly. It sends messages
//! (a new graph, transport commands, parameter changes) through a ring
//! buffer; the audio thread applies them at the start of its next callback
//! and sends retired graphs back to be freed off the audio thread.

use crate::graph::{AudioData, Graph};
use crate::processor::Processor;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use trill_core::device::EventKind;
use trill_core::{DeviceId, TrackId};

const MESSAGE_CAPACITY: usize = 1024;
const GARBAGE_CAPACITY: usize = 64;
/// Largest callback the engine renders in one go; bigger ones are chunked.
const CALLBACK_FRAMES: usize = 8192;
const NULL_BLOCK_FRAMES: usize = 512;

/// Where the audio goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AudioBackend {
    /// The system's default output device, or `Null` if there is none.
    Auto,
    /// The system's default output device; fail if there is none.
    Device,
    /// No audio hardware. The engine still runs in real time, so transport
    /// and meters behave normally. Used headless and in tests.
    Null { sample_rate: u32 },
}

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub sample_rate: f64,
    /// Human-readable description of the output, e.g. the device name.
    pub output: String,
    /// Set when `Auto` fell back to the null backend.
    pub warning: Option<String>,
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
    Param { device: DeviceId, index: usize, value: f64 },
    Live { track: TrackId, kind: EventKind },
    Panic,
    Audition { audio: Arc<AudioData>, gain: f32 },
    StopAudition,
    Metronome(bool),
}

/// What the audio thread hands back to be freed elsewhere.
pub(crate) enum Garbage {
    Graph(Box<Graph>),
    Audio(Arc<AudioData>),
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
    quit: AtomicBool,
}

/// Everything the audio callback owns.
struct Realtime {
    processor: Processor,
    messages: rtrb::Consumer<Message>,
    garbage: rtrb::Producer<Garbage>,
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
                Message::Live { track, kind } => self.processor.live_event(track, kind),
                Message::Panic => self.processor.panic(),
                Message::Audition { audio, gain } => {
                    if let Some(previous) = self.processor.start_audition(audio, gain) {
                        let _ = self.garbage.push(Garbage::Audio(previous));
                    }
                }
                Message::StopAudition => self.processor.stop_audition(),
                Message::Metronome(on) => self.processor.set_metronome(on),
            }
        }
        if let Some(finished) = self.processor.take_finished_audition() {
            let _ = self.garbage.push(Garbage::Audio(finished));
        }
    }

    /// Renders `frames` (at most the scratch size) into the scratch buffers.
    fn render(&mut self, frames: usize) {
        self.apply_messages();
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
        self.shared.playing.store(self.processor.is_playing(), Ordering::Relaxed);
        self.shared.auditioning.store(self.processor.is_auditioning(), Ordering::Relaxed);
        self.shared.position.store(self.processor.position().to_bits(), Ordering::Relaxed);
    }

    /// Fills an interleaved device buffer.
    fn fill<T>(&mut self, data: &mut [T], channels: usize)
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let started = Instant::now();
        let total = data.len() / channels.max(1);
        let mut done = 0;
        while done < total {
            let frames = (total - done).min(self.left.len());
            self.render(frames);
            let out = &mut data[done * channels..(done + frames) * channels];
            for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
                let left = sanitize(self.left[i]);
                let right = sanitize(self.right[i]);
                if channels == 1 {
                    frame[0] = T::from_sample((left + right) * 0.5);
                } else {
                    frame[0] = T::from_sample(left);
                    frame[1] = T::from_sample(right);
                    for extra in &mut frame[2..] {
                        *extra = T::from_sample(0.0);
                    }
                }
            }
            done += frames;
        }
        let available = total as f64 / self.processor.sample_rate();
        if available > 0.0 {
            let load = (started.elapsed().as_secs_f64() / available) as f32;
            self.shared.load.store(load.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Last line of defense before the hardware: no NaNs, no over-range samples.
#[inline]
fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 }
}

/// Controller-side handle to the audio thread.
pub struct Engine {
    messages: rtrb::Producer<Message>,
    garbage: rtrb::Consumer<Garbage>,
    shared: Arc<Shared>,
    info: StreamInfo,
    /// Graphs sent to the audio thread and not yet returned.
    swaps_in_flight: usize,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(backend: AudioBackend) -> Result<Self, EngineError> {
        let (message_tx, message_rx) = rtrb::RingBuffer::new(MESSAGE_CAPACITY);
        let (garbage_tx, garbage_rx) = rtrb::RingBuffer::new(GARBAGE_CAPACITY);
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::channel();

        let thread_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("audio-driver".into())
            .spawn(move || {
                // Built on this thread because audio streams are not `Send`.
                let make = move |sample_rate: f64| Realtime {
                    processor: Processor::new(sample_rate),
                    messages: message_rx,
                    garbage: garbage_tx,
                    shared: thread_shared.clone(),
                    left: vec![0.0; CALLBACK_FRAMES],
                    right: vec![0.0; CALLBACK_FRAMES],
                };
                drive(backend, make, ready_tx);
            })
            .map_err(|e| EngineError::NoOutput(e.to_string()))?;

        let info = match ready_rx.recv() {
            Ok(Ok(info)) => info,
            Ok(Err(reason)) => return Err(EngineError::NoOutput(reason)),
            Err(_) => return Err(EngineError::ThreadDied),
        };
        Ok(Self { messages: message_tx, garbage: garbage_rx, shared, info, swaps_in_flight: 0, thread: Some(thread) })
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn sample_rate(&self) -> f64 {
        self.info.sample_rate
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
            thread::sleep(Duration::from_millis(1));
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
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        self.collect_garbage();
    }
}

type Ready = mpsc::Sender<Result<StreamInfo, String>>;

fn drive(backend: AudioBackend, make: impl FnOnce(f64) -> Realtime, ready: Ready) {
    match backend {
        AudioBackend::Null { sample_rate } => drive_null(sample_rate, make, ready, None),
        AudioBackend::Device => {
            if let Err((reason, _, ready)) = drive_device(make, ready) {
                let _ = ready.send(Err(reason));
            }
        }
        AudioBackend::Auto => {
            if let Err((reason, make, ready)) = drive_device(make, ready) {
                let warning = format!("no audio output ({reason}); running without sound");
                drive_null(48_000, make, ready, Some(warning));
            }
        }
    }
}

/// Runs the processor at realtime pace without audio hardware.
fn drive_null(sample_rate: u32, make: impl FnOnce(f64) -> Realtime, ready: Ready, warning: Option<String>) {
    let mut realtime = make(sample_rate as f64);
    let shared = realtime.shared.clone();
    let info = StreamInfo { sample_rate: sample_rate as f64, output: "none (null backend)".into(), warning };
    if ready.send(Ok(info)).is_err() {
        return;
    }
    let period = Duration::from_secs_f64(NULL_BLOCK_FRAMES as f64 / sample_rate as f64);
    let mut next = Instant::now();
    while !shared.quit.load(Ordering::Relaxed) {
        realtime.render(NULL_BLOCK_FRAMES);
        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

/// Opens the default output device and parks until asked to quit. On
/// failure, hands `make` and `ready` back so the caller can fall back.
fn drive_device<F: FnOnce(f64) -> Realtime>(make: F, ready: Ready) -> Result<(), (String, F, Ready)> {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        return Err(("no default output device".into(), make, ready));
    };
    let supported = match device.default_output_config() {
        Ok(config) => config,
        Err(e) => return Err((e.to_string(), make, ready)),
    };
    let sample_rate = supported.sample_rate() as f64;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "default output".into());

    let mut realtime = make(sample_rate);
    let shared = realtime.shared.clone();
    let on_error = |error| log::error!("audio stream error: {error}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| realtime.fill(data, channels),
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| realtime.fill(data, channels),
            on_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| realtime.fill(data, channels),
            on_error,
            None,
        ),
        cpal::SampleFormat::I32 => device.build_output_stream(
            config,
            move |data: &mut [i32], _: &cpal::OutputCallbackInfo| realtime.fill(data, channels),
            on_error,
            None,
        ),
        other => {
            // `realtime` was not moved into a callback on this path, but the
            // closure it came from is spent; report the failure outright.
            let _ = ready.send(Err(format!("unsupported sample format {other:?}")));
            return Ok(());
        }
    };
    let stream = match stream {
        Ok(stream) => stream,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return Ok(());
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready.send(Err(e.to_string()));
        return Ok(());
    }

    let info = StreamInfo { sample_rate, output: format!("{} ({} Hz)", name, sample_rate as u32), warning: None };
    if ready.send(Ok(info)).is_ok() {
        while !shared.quit.load(Ordering::Relaxed) {
            thread::park_timeout(Duration::from_millis(200));
        }
    }
    drop(stream);
    Ok(())
}
