//! Audio hosts: where the engine's sound goes, and where recorded sound
//! comes from.
//!
//! The engine does not talk to the operating system's audio itself. It
//! asks an [`AudioHost`] what devices there are and to open one, and hands
//! over a [`Renderer`]: the host's audio thread calls it for every buffer
//! the device wants filled. Three hosts ship:
//!
//! * [`SystemHost`], the machine's real devices;
//! * [`NullHost`], no device at all: the engine still runs in real time, so
//!   transport and meters behave as usual. Used headless and in tests;
//! * [`scripted::ScriptedHost`], devices that exist only in a test, which
//!   can be made to vanish, refuse a format or stop answering, and whose
//!   audio thread is the test itself.

pub mod scripted;
mod system;
#[cfg(windows)]
mod wasapi;

pub use crate::clock::now_ns;
pub use crate::engine::Renderer;
pub use system::SystemHost;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The sample rates offered for choosing, where a device takes them.
pub const COMMON_SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// The buffer sizes offered for choosing, in frames.
pub const COMMON_BUFFER_SIZES: [u32; 7] = [32, 64, 128, 256, 512, 1024, 2048];

/// An audio device as a host lists it. The name is what identifies it: in
/// the settings, in [`AudioConfig`] and over MCP. A host that finds two
/// devices of one name numbers the later ones (`"USB Audio (2)"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    /// Whether the system uses it when no device is chosen.
    pub is_default: bool,
    /// The rate the system runs it at.
    pub default_sample_rate: u32,
    /// The rates it can be opened at, of [`COMMON_SAMPLE_RATES`].
    pub sample_rates: Vec<u32>,
    pub channels: u16,
    /// Whether it can be taken for this program alone, for the shortest
    /// latency; see [`AudioConfig::exclusive`].
    pub exclusive: bool,
}

/// The devices of a host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioDevices {
    pub outputs: Vec<AudioDevice>,
    pub inputs: Vec<AudioDevice>,
}

/// What the user asks of the audio hardware. `None` means "whatever the
/// system offers". This is a wish: what was obtained is in
/// [`StreamInfo`](crate::StreamInfo).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioConfig {
    /// Name of the output device; `None` for the system's default, which is
    /// then followed when the system's choice changes.
    pub output: Option<String>,
    /// Name of the device audio is recorded from; `None` for the system's
    /// default input.
    pub input: Option<String>,
    /// Sample rate in hertz.
    pub sample_rate: Option<u32>,
    /// Buffer size in frames. Smaller is less latency and more risk of
    /// drop-outs. The device may round it, or not offer the choice at all:
    /// a device shared with other programs runs at the period the system
    /// mixes at.
    pub buffer_size: Option<u32>,
    /// Take the output device for this program alone. Other programs lose
    /// it while Trill runs, and in return the buffer size is really what
    /// was asked for. Where the system cannot do that the device is opened
    /// shared, and [`StreamInfo::warning`](crate::StreamInfo) says so.
    pub exclusive: bool,
}

/// One attempt to open a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRequest {
    /// `None` for the system's default device.
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    pub exclusive: bool,
}

/// What a stream turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamFormat {
    /// The device's name, as [`AudioDevice::name`] has it.
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// How many frames the device asks for at a time.
    pub buffer_frames: u32,
    /// How long after a frame is rendered it is heard, as far as the host
    /// can tell when the stream opens. A running stream reports the figure
    /// again with every buffer ([`Timing::latency_frames`]).
    pub latency_frames: u32,
    pub exclusive: bool,
}

/// Why a device could not be opened.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    #[error("there is no audio device called '{0}'")]
    NotFound(String),
    #[error("the system has no audio output device")]
    NoDevice,
    /// The device is there, but not in the way that was asked for.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

/// What went wrong with a stream that was running.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StreamFailure {
    /// The device is gone: unplugged, switched off, taken away by the
    /// system.
    DeviceLost,
    /// The device reported an error it does not recover from.
    Failed,
    /// The device has stopped asking for audio without saying why.
    Stalled,
}

impl StreamFailure {
    pub(crate) fn code(self) -> u8 {
        match self {
            StreamFailure::DeviceLost => 1,
            StreamFailure::Failed => 2,
            StreamFailure::Stalled => 3,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(StreamFailure::DeviceLost),
            2 => Some(StreamFailure::Failed),
            3 => Some(StreamFailure::Stalled),
            _ => None,
        }
    }
}

/// How a host tells the engine that a running stream has failed. It may be
/// called from any thread, the audio thread included, so it does no more
/// than set a flag: the engine acts on it at its next round of chores.
pub type FailureCallback = Arc<dyn Fn(StreamFailure) + Send + Sync>;

/// What a host knows about the buffer it is asking for.
#[derive(Copy, Clone, Debug)]
pub struct Timing {
    /// The time of the call on the clock live input is stamped with
    /// ([`now_ns`]).
    pub now_ns: u64,
    /// How many frames lie between this buffer's first frame and the
    /// speaker: what is queued ahead of it plus what the device itself
    /// adds.
    pub latency_frames: u32,
}

/// A running stream. Dropping it stops the stream; the host's audio thread
/// has finished its last call into the [`Renderer`] by the time the drop
/// returns.
pub type StreamHandle = Box<dyn Send>;

/// A stream a host has opened.
pub struct OpenStream {
    pub format: StreamFormat,
    pub handle: StreamHandle,
}

/// Something that plays the engine's audio.
pub trait AudioHost: Send + Sync {
    /// The devices that can be opened right now. May take a moment: it asks
    /// the system.
    fn devices(&self) -> AudioDevices;

    /// Opens an output device and starts it. From then on the host's audio
    /// thread calls `renderer` for every buffer; `on_failure` is called if
    /// the stream stops working.
    fn open_output(
        &self,
        request: &StreamRequest,
        renderer: Renderer,
        on_failure: FailureCallback,
    ) -> Result<OpenStream, OpenError>;

    /// Called while the engine waits for the audio thread to do something.
    /// A real host has an audio thread of its own and does nothing here; a
    /// host driven by hand renders a buffer, so that waiting ends.
    fn pump(&self) {}

    /// Whether the audio thread is the host's own, running by itself. The
    /// engine watches such a stream for stalling.
    fn runs_by_itself(&self) -> bool {
        true
    }

    /// The time on the clock this host stamps its buffers with
    /// ([`Timing::now_ns`]): the one live input must be stamped with to
    /// land where it was played. Every host but a scripted one reads
    /// [`now_ns`].
    fn now_ns(&self) -> u64 {
        now_ns()
    }
}

/// No audio hardware. The engine runs at real-time pace all the same.
pub struct NullHost {
    sample_rate: u32,
}

/// The name [`NullHost`]'s stream reports as its device.
pub(crate) const NULL_DEVICE: &str = "none";
const NULL_BLOCK_FRAMES: usize = 512;

impl NullHost {
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate: sample_rate.max(1) }
    }
}

impl AudioHost for NullHost {
    fn devices(&self) -> AudioDevices {
        AudioDevices::default()
    }

    fn open_output(
        &self,
        request: &StreamRequest,
        mut renderer: Renderer,
        _on_failure: FailureCallback,
    ) -> Result<OpenStream, OpenError> {
        let sample_rate = request.sample_rate.unwrap_or(self.sample_rate).max(1);
        let quit = Arc::new(AtomicBool::new(false));
        let stop = quit.clone();
        let thread = std::thread::Builder::new()
            .name("audio-null".into())
            .spawn(move || {
                let period = Duration::from_secs_f64(NULL_BLOCK_FRAMES as f64 / sample_rate as f64);
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let timing = Timing { now_ns: now_ns(), latency_frames: 0 };
                    renderer.render(NULL_BLOCK_FRAMES, timing, &mut |_, _| {});
                    next += period;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        next = now;
                    }
                }
            })
            .map_err(|error| OpenError::Failed(format!("cannot start a thread: {error}")))?;
        let format = StreamFormat {
            device: NULL_DEVICE.into(),
            sample_rate,
            channels: 2,
            buffer_frames: NULL_BLOCK_FRAMES as u32,
            latency_frames: 0,
            exclusive: false,
        };
        Ok(OpenStream { format, handle: Box::new(NullStream { quit, thread: Some(thread) }) })
    }
}

struct NullStream {
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for NullStream {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Gives every device a name of its own: the second and later devices of
/// one name get a number.
pub(crate) fn number_duplicates(names: &mut [String]) {
    for index in 0..names.len() {
        let earlier = names[..index].iter().filter(|name| base_name(name) == names[index]).count();
        if earlier > 0 {
            names[index] = format!("{} ({})", names[index], earlier + 1);
        }
    }
}

/// A name without the number [`number_duplicates`] may have given it.
fn base_name(name: &str) -> &str {
    match name.rsplit_once(" (") {
        Some((base, number))
            if number.strip_suffix(')').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            base
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_of_one_name_are_numbered() {
        let mut names: Vec<String> = ["USB Audio", "Speakers", "USB Audio", "USB Audio"].map(String::from).to_vec();
        number_duplicates(&mut names);
        assert_eq!(names, ["USB Audio", "Speakers", "USB Audio (2)", "USB Audio (3)"]);
    }

    #[test]
    fn failures_survive_the_trip_through_a_flag() {
        for failure in [StreamFailure::DeviceLost, StreamFailure::Failed, StreamFailure::Stalled] {
            assert_eq!(StreamFailure::from_code(failure.code()), Some(failure));
        }
        assert_eq!(StreamFailure::from_code(0), None);
    }
}
