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
    /// Releases every note, whatever holds it: a key, or the sustain
    /// pedal. The pedal and the wheels stay where they are.
    AllNotesOff,
    /// The pitch wheel, from -1 (fully down) to 1 (fully up); 0 at rest.
    /// How far that bends is the instrument's business:
    /// [`PITCH_BEND_SEMITONES`] each way unless it says otherwise.
    PitchBend {
        value: f32,
    },
    /// A MIDI controller: `controller` is its number (0..=127) and `value`
    /// its position, 0..=1. The mod wheel is [`CONTROLLER_MOD_WHEEL`] and
    /// the sustain pedal [`CONTROLLER_SUSTAIN`], down from 0.5 up. An
    /// instrument ignores the controllers it has no use for.
    Control {
        controller: u8,
        value: f32,
    },
}

/// How far the pitch wheel bends, each way, where an instrument has no
/// setting of its own for it.
pub const PITCH_BEND_SEMITONES: f32 = 2.0;
/// The controller number of the mod wheel in [`EventKind::Control`].
pub const CONTROLLER_MOD_WHEEL: u8 = 1;
/// The controller number of the sustain pedal in [`EventKind::Control`].
pub const CONTROLLER_SUSTAIN: u8 = 64;

impl EventKind {
    /// Whether the event is a sustain pedal going down (`Some(true)`) or
    /// coming up (`Some(false)`).
    pub fn sustain(&self) -> Option<bool> {
        match *self {
            EventKind::Control { controller: CONTROLLER_SUSTAIN, value } => Some(value >= 0.5),
            _ => None,
        }
    }
}

/// The sustain pedal, for an instrument that has to work it out itself:
/// while the pedal is down, keys that come up go on sounding, and all of
/// them are let go of when it comes up.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Pedal {
    down: bool,
    /// The keys that came up while the pedal was down, a bit per pitch.
    held: u128,
}

impl Pedal {
    pub fn is_down(&self) -> bool {
        self.down
    }

    /// The pedal moved. When it comes up, returns the keys it was holding,
    /// lowest first: their notes end now.
    pub fn set(&mut self, down: bool) -> impl Iterator<Item = u8> + use<> {
        self.down = down;
        let released = if down { 0 } else { std::mem::take(&mut self.held) };
        (0..128u8).filter(move |pitch| released & (1 << pitch) != 0)
    }

    /// A key went down: whatever the pedal held of it is that key's again.
    pub fn key_down(&mut self, pitch: u8) {
        self.held &= !(1 << pitch.min(127));
    }

    /// A key came up. Returns whether its note ends now; if not, the pedal
    /// has it.
    pub fn key_up(&mut self, pitch: u8) -> bool {
        if self.down {
            self.held |= 1 << pitch.min(127);
        }
        !self.down
    }

    /// Every note has ended ([`EventKind::AllNotesOff`]): the pedal holds
    /// nothing, and stays where it is.
    pub fn clear(&mut self) {
        self.held = 0;
    }
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

    /// Whether the device has an editor window of its own to open. Cheap:
    /// a view may ask on every frame.
    fn has_editor(&self) -> bool {
        false
    }

    /// Opens the device's editor in a window of its own. If it is already
    /// open, that window is brought to the front instead.
    ///
    /// The editor closes when the controller is dropped, so it never
    /// outlives the device it belongs to.
    fn open_editor(&mut self, options: &EditorOptions) -> Result<(), EditorError> {
        let _ = options;
        Err(EditorError::NoEditor)
    }

    /// Closes the device's editor, if it is open.
    fn close_editor(&mut self) {}

    /// Whether the device's editor is open right now. The user may close
    /// the window at any time, so views ask rather than remember. Cheap.
    fn is_editor_open(&self) -> bool {
        false
    }

    /// The pixels of the open editor, as it is drawn right now.
    fn capture_editor(&mut self) -> Option<EditorImage> {
        None
    }

    /// What the device changed about its own parameters since the last
    /// call, oldest first: the knobs the user turned in its editor.
    fn take_param_edits(&mut self) -> Vec<ParamEdit> {
        Vec::new()
    }
}

/// How to open a device's editor window.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorOptions {
    /// The window's title. Left empty, the device names the window itself.
    pub title: String,
    /// The native window the editor belongs to (an `HWND` on Windows): the
    /// editor stays above it, and is minimized and closed with it.
    pub owner: Option<isize>,
    /// Where to put the window's top-left corner, in screen pixels. `None`
    /// puts it where it was when this device's editor was last closed, and
    /// the first time wherever the system places a new window.
    pub position: Option<(i32, i32)>,
    /// Whether the window takes the keyboard focus as it opens. Automated
    /// runs turn this off so as not to interrupt whoever is at the machine.
    pub activate: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self { title: String::new(), owner: None, position: None, activate: true }
    }
}

/// Why a device's editor could not be opened.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditorError {
    #[error("the device has no editor")]
    NoEditor,
    #[error("plugin editors are not supported on this platform yet")]
    Unsupported,
    #[error("the editor could not be opened: {0}")]
    Failed(String),
}

/// A picture of an editor window's contents: `width * height` pixels of
/// red, green, blue and alpha, row by row from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A change a device made to one of its own parameters.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ParamEdit {
    /// Position of the parameter in the device's parameter list.
    pub index: usize,
    pub kind: ParamEditKind,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ParamEditKind {
    /// The user took hold of the control.
    Begin,
    /// The parameter's new value, in plain units.
    Value(f64),
    /// The user let go of the control.
    End,
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

/// Something wrong with a device that did not stop it from being created:
/// it runs, with a part of it missing. A sampler whose file has gone is the
/// model case: the project must still open and play everything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceWarning {
    /// The part of the device it concerns, such as `pad 38` of a drum rack;
    /// `None` when it concerns the device as a whole.
    pub slot: Option<String>,
    /// What is wrong, in words for the user, naming the slot.
    pub message: String,
}

/// Creates devices from their model description. Implemented by the built-in
/// device registry and by each plugin host; the app composes them.
pub trait DeviceFactory: Send + Sync {
    /// Everything this factory can create.
    fn descriptors(&self) -> Vec<DeviceDescriptor>;

    /// Instantiates `device`, restores its saved state and parameter values,
    /// and prepares it for `sample_rate`.
    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError>;

    /// What is wrong with `device` as this factory would create it, short
    /// of failing to create it. Empty when all is well, and for devices of
    /// other factories. Called off the audio thread, and often: it should
    /// cost little when nothing has changed.
    fn warnings(&self, _device: &Device) -> Vec<DeviceWarning> {
        Vec::new()
    }
}

/// A shared factory is a factory, so one instance can serve several owners.
impl<T: DeviceFactory + ?Sized> DeviceFactory for std::sync::Arc<T> {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        (**self).descriptors()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        (**self).create(device, sample_rate, max_block)
    }

    fn warnings(&self, device: &Device) -> Vec<DeviceWarning> {
        (**self).warnings(device)
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

    fn warnings(&self, device: &Device) -> Vec<DeviceWarning> {
        self.factories.iter().flat_map(|factory| factory.warnings(device)).collect()
    }
}
