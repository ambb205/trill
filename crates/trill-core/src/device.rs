//! The device API: what an instrument or effect looks like to the engine.
//!
//! A device has two halves:
//!
//! * a [`DeviceProcessor`], which lives on the audio thread and must be
//!   realtime-safe (no allocation, locks, I/O or panics in `process`), and
//! * an optional [`DeviceController`], which stays off the audio thread and
//!   handles the slow things: saving state, opening an editor window.
//!
//! Built-in devices and hosted plugins (CLAP, VST3) both sit behind this API,
//! so the engine never knows which one it is driving.

use crate::project::Device;
use crate::time::TimeSignature;
use serde::{Deserialize, Serialize};

/// URI scheme of the devices that ship with the DAW, e.g. `builtin:polysynth`.
pub const BUILTIN_SCHEME: &str = "builtin";

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCategory {
    Instrument,
    Effect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Float,
    Int,
    Bool,
    /// The value is an index into these choices.
    Enum(Vec<String>),
}

/// Describes one automatable parameter. Values are always in plain units
/// (Hz, dB, seconds...) between `min` and `max`, never normalized.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    /// Stable key used in the project file and the API.
    pub id: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: String,
    pub kind: ParamKind,
}

impl ParamInfo {
    pub fn clamp(&self, value: f64) -> f64 {
        let v = if value.is_finite() { value } else { self.default };
        let v = v.clamp(self.min, self.max);
        match self.kind {
            ParamKind::Float => v,
            _ => v.round(),
        }
    }
}

/// A named set of parameter values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub params: Vec<(String, f64)>,
}

/// Catalog entry for a device that can be added to a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceDescriptor {
    /// `builtin:<key>`, `clap:<plugin id>`, `vst3:<class id>`.
    pub uri: String,
    pub name: String,
    pub vendor: String,
    pub category: DeviceCategory,
    pub description: String,
    /// Known up front for built-ins; plugins report theirs once instantiated.
    pub params: Vec<ParamInfo>,
    pub presets: Vec<Preset>,
}

/// Transport state handed to devices for each block.
#[derive(Copy, Clone, Debug)]
pub struct ProcessInfo {
    pub sample_rate: f64,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub playing: bool,
    /// Song position at the first sample of the block.
    pub position_beats: f64,
    /// `(start, end)` in beats while the transport is looping.
    pub loop_range: Option<(f64, f64)>,
}

/// The most events the engine passes to one `process` call. Hosts of
/// external plugin formats size their event queues from it.
pub const MAX_EVENTS_PER_BLOCK: usize = 4096;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EventKind {
    /// `velocity` is 0..=1.
    NoteOn {
        pitch: u8,
        velocity: f32,
    },
    NoteOff {
        pitch: u8,
    },
    AllNotesOff,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Event {
    /// Sample offset from the start of the block.
    pub offset: u32,
    pub kind: EventKind,
}

/// The audio-thread half of a device.
///
/// Everything except `prepare` is called on the audio thread and must be
/// realtime-safe.
pub trait DeviceProcessor: Send {
    /// Called off the audio thread before the first `process`, and again if
    /// the sample rate changes. Allocate here.
    fn prepare(&mut self, sample_rate: f64, max_block: usize);

    /// `index` is the position in the device's parameter list; `value` is in
    /// plain units. Out-of-range indices are ignored.
    fn set_param(&mut self, index: usize, value: f64);

    /// Processes one block in place. `left` and `right` have equal length, at
    /// most `max_block`. Instruments mix their output on top of the input;
    /// `events` are sorted by offset.
    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]);

    /// Silences voices and clears delay lines (transport stop, seek).
    fn reset(&mut self);

    fn latency_samples(&self) -> u32 {
        0
    }
}

/// The off-audio-thread half of a device. Built-in devices keep all of their
/// state in the project model and do not need one.
pub trait DeviceController: Send {
    /// Opaque state to store in the project file.
    fn save_state(&mut self) -> Option<Vec<u8>>;

    /// Current value of every parameter, in parameter-list order, if the
    /// device can change them on its own (for example from its editor).
    fn param_values(&mut self) -> Option<Vec<f64>> {
        None
    }
}

/// A freshly created device, ready to be handed to the engine.
pub struct DeviceInstance {
    pub processor: Box<dyn DeviceProcessor>,
    pub params: Vec<ParamInfo>,
    pub controller: Option<Box<dyn DeviceController>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("unknown device '{0}'")]
    Unknown(String),
    #[error("failed to load '{uri}': {reason}")]
    LoadFailed { uri: String, reason: String },
}

/// Creates devices from their model description. Implemented by the built-in
/// device registry and by each plugin host; the app composes them.
pub trait DeviceFactory: Send + Sync {
    /// Everything this factory can create.
    fn descriptors(&self) -> Vec<DeviceDescriptor>;

    /// Instantiates `device`, restores its saved state and parameter values,
    /// and prepares it for `sample_rate`.
    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError>;
}

/// A shared factory is a factory, so one instance can serve several owners.
impl<T: DeviceFactory + ?Sized> DeviceFactory for std::sync::Arc<T> {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        (**self).descriptors()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        (**self).create(device, sample_rate, max_block)
    }
}

/// Splits `builtin:polysynth` into `("builtin", "polysynth")`.
pub fn split_uri(uri: &str) -> Option<(&str, &str)> {
    uri.split_once(':')
}

/// Tries each factory in turn, so built-ins and plugin hosts appear as one.
pub struct CompositeFactory {
    factories: Vec<Box<dyn DeviceFactory>>,
}

impl CompositeFactory {
    pub fn new(factories: Vec<Box<dyn DeviceFactory>>) -> Self {
        Self { factories }
    }
}

impl DeviceFactory for CompositeFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.factories.iter().flat_map(|f| f.descriptors()).collect()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        for factory in &self.factories {
            match factory.create(device, sample_rate, max_block) {
                Err(DeviceError::Unknown(_)) => continue,
                result => return result,
            }
        }
        Err(DeviceError::Unknown(device.uri.clone()))
    }
}
