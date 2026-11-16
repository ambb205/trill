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

use crate::presets::{PresetInfo, PresetList};
use crate::project::Device;
use crate::time::TimeSignature;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

/// A named set of parameter values: a preset of a device whose whole sound
/// is its parameters, which is every built-in one. It is listed with the
/// device type ([`DeviceDescriptor::presets`]) and asked for by its name.
///
/// A plugin's presets are not of this kind. They are found some time after
/// the plugin is, there may be thousands, and only the plugin can load
/// one; see [`DeviceFactory::presets`], which lists both kinds alike.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub params: Vec<(String, f64)>,
}

impl Preset {
    /// The preset as a preset list has it: asked for by its name.
    pub fn info(&self) -> PresetInfo {
        PresetInfo {
            id: self.name.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            ..PresetInfo::default()
        }
    }
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
    /// The presets that are sets of parameter values. Empty for plugins,
    /// whose presets [`DeviceFactory::presets`] lists.
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
    /// Channel pressure: how hard the keys are being leaned on after they
    /// went down (aftertouch for the whole keyboard), 0..=1.
    ChannelPressure {
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

    /// How far the device has turned the signal down since this was last
    /// asked, for a device whose business that is (a compressor, a
    /// limiter); `None` for any other. Asked off the audio thread, of a
    /// device that is not being processed meanwhile: by a render, once it
    /// is done, so that a threshold can be set from a number.
    fn gain_reduction(&mut self) -> Option<GainReduction> {
        None
    }
}

/// How far a compressor or a limiter turned the signal down over a stretch
/// of audio. All of it is in dB taken off, so 0 is "did nothing".
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct GainReduction {
    /// The most it took off at any moment.
    pub most_db: f32,
    /// What it took off on average, over the time it took off anything
    /// worth the name (half a dB or more).
    pub typical_db: f32,
    /// The share of the time it was doing that, 0..=1.
    pub active: f32,
}

/// What a device is created for.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Purpose {
    /// To be played, under an audio device's clock.
    #[default]
    Playing,
    /// To be rendered, as fast as the machine goes and with nobody
    /// listening meanwhile. A plugin is told so where its format has a way
    /// of saying it, so that it can prefer being right over being on time:
    /// a sampler that loads what it needs instead of streaming it in
    /// behind the music.
    Rendering,
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

    /// Loads one of the presets its factory lists for the device's type
    /// ([`DeviceFactory::presets`]), replacing the device's sound the way
    /// choosing the preset in the device's own browser would. The device
    /// goes on playing meanwhile.
    ///
    /// Returns once the device has taken the preset on, or has said that
    /// it will: a device may finish loading a moment later, on threads of
    /// its own. A device that refuses, or cannot be made to answer, is an
    /// error that says which.
    fn load_preset(&mut self, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        let _ = preset;
        Err(PresetLoadError::Unsupported)
    }

    /// Which MIDI controllers the device does something with, if it can
    /// say. Cheap: a view may ask whenever it is drawn.
    fn controller_support(&self) -> ControllerSupport {
        ControllerSupport::Unknown
    }
}

/// Why a device did not load a preset.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PresetLoadError {
    /// The device has no way of being given a preset.
    #[error("the device cannot load presets")]
    Unsupported,
    /// The device refused the preset, or failed while loading it. It may
    /// have been changed by the attempt.
    #[error("{0}")]
    Failed(String),
    /// The device did not answer. Nothing more can be asked of it for
    /// now, its state included.
    #[error("{0}")]
    Unresponsive(String),
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

/// Which MIDI controllers a device does something with, as far as that can
/// be known without playing it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ControllerSupport {
    /// The device does not say.
    #[default]
    Unknown,
    /// The device is handed every controller as MIDI. What it does with
    /// each is its own business.
    Any,
    /// The device responds to exactly these.
    Listed(Vec<crate::controllers::ControllerKind>),
}

impl ControllerSupport {
    /// Whether the device is known to respond to `kind` (`Some(true)`),
    /// known not to (`Some(false)`), or cannot say (`None`).
    pub fn responds_to(&self, kind: crate::controllers::ControllerKind) -> Option<bool> {
        match self {
            Self::Unknown | Self::Any => None,
            Self::Listed(kinds) => Some(kinds.contains(&kind)),
        }
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

    /// [`create`](Self::create), saying what the device is for. A factory
    /// whose devices are the same either way needs no implementation of
    /// its own.
    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        max_block: usize,
        purpose: Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        let _ = purpose;
        self.create(device, sample_rate, max_block)
    }

    /// What is wrong with `device` as this factory would create it, short
    /// of failing to create it. Empty when all is well, and for devices of
    /// other factories. Called off the audio thread, and often: it should
    /// cost little when nothing has changed.
    fn warnings(&self, _device: &Device) -> Vec<DeviceWarning> {
        Vec::new()
    }

    /// The presets of the device type `uri`, as far as they are known
    /// right now; `None` if the type is not one of this factory's.
    ///
    /// Never waits. Where finding the presets takes time (a plugin's),
    /// the first call starts that in the background and returns a list
    /// that says it is still being read; later calls return more of it.
    ///
    /// Without an implementation of its own, a factory offers the presets
    /// its [`descriptors`](Self::descriptors) carry.
    fn presets(&self, uri: &str) -> Option<PresetList> {
        let descriptor = self.descriptors().into_iter().find(|descriptor| descriptor.uri == uri)?;
        Some(PresetList::ready(descriptor.presets.iter().map(Preset::info).collect::<Vec<_>>()))
    }

    /// [`presets`](Self::presets), waiting up to `timeout` for a list that
    /// is being read to be complete, and returning what there is by then.
    /// For callers that may wait, such as a tool call; never for a thread
    /// the user is waiting on.
    fn presets_within(&self, uri: &str, timeout: Duration) -> Option<PresetList> {
        let _ = timeout;
        self.presets(uri)
    }

    /// Has the presets of the device type `uri` read again, in the
    /// background: for presets that were added or removed while the
    /// program ran. Does nothing for a type whose presets never change.
    fn refresh_presets(&self, uri: &str) {
        let _ = uri;
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

    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        max_block: usize,
        purpose: Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        (**self).create_for(device, sample_rate, max_block, purpose)
    }

    fn warnings(&self, device: &Device) -> Vec<DeviceWarning> {
        (**self).warnings(device)
    }

    fn presets(&self, uri: &str) -> Option<PresetList> {
        (**self).presets(uri)
    }

    fn presets_within(&self, uri: &str, timeout: Duration) -> Option<PresetList> {
        (**self).presets_within(uri, timeout)
    }

    fn refresh_presets(&self, uri: &str) {
        (**self).refresh_presets(uri)
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
        self.create_for(device, sample_rate, max_block, Purpose::Playing)
    }

    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        max_block: usize,
        purpose: Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        for factory in &self.factories {
            match factory.create_for(device, sample_rate, max_block, purpose) {
                Err(DeviceError::Unknown(_)) => continue,
                result => return result,
            }
        }
        Err(DeviceError::Unknown(device.uri.clone()))
    }

    fn warnings(&self, device: &Device) -> Vec<DeviceWarning> {
        self.factories.iter().flat_map(|factory| factory.warnings(device)).collect()
    }

    fn presets(&self, uri: &str) -> Option<PresetList> {
        self.factories.iter().find_map(|factory| factory.presets(uri))
    }

    fn presets_within(&self, uri: &str, timeout: Duration) -> Option<PresetList> {
        self.factories.iter().find_map(|factory| factory.presets_within(uri, timeout))
    }

    fn refresh_presets(&self, uri: &str) {
        for factory in &self.factories {
            factory.refresh_presets(uri);
        }
    }
}
