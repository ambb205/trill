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
use std::path::Path;
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

    /// The value as a reader takes it in: the choice's name, `On` or
    /// `Off`, or the number with its unit (`900 Hz`, `-6 dB`). This is what
    /// a device that has no display of its own is shown with; a plugin has
    /// its own word for each value
    /// ([`DeviceController::param_texts`]).
    pub fn display(&self, value: f64) -> String {
        match &self.kind {
            ParamKind::Enum(choices) => {
                choices.get(value.round().max(0.0) as usize).cloned().unwrap_or_else(|| trimmed(value))
            }
            ParamKind::Bool => if value >= 0.5 { "On" } else { "Off" }.to_string(),
            _ if self.unit.is_empty() => trimmed(value),
            _ => format!("{} {}", trimmed(value), self.unit),
        }
    }

    /// The value `text` names, the other way round from
    /// [`display`](Self::display): a choice by its name, a switch by
    /// `on`/`off`, a number with or without the unit. `None` for text that
    /// is none of those; the value is not checked against the range.
    pub fn parse(&self, text: &str) -> Option<f64> {
        let text = text.trim();
        match &self.kind {
            ParamKind::Enum(choices) => {
                choices.iter().position(|choice| choice.trim().eq_ignore_ascii_case(text)).map(|index| index as f64)
            }
            _ => {
                match text.to_ascii_lowercase().as_str() {
                    "on" | "true" | "yes" => return Some(1.0),
                    "off" | "false" | "no" => return Some(0.0),
                    _ => {}
                }
                // `-6 dB`, `-6dB` and `-6` are all the number.
                let unit = self.unit.trim();
                let lowered = text.to_lowercase();
                let number = match lowered.strip_suffix(&unit.to_lowercase()) {
                    Some(number) if !unit.is_empty() => number.trim_end(),
                    _ => lowered.as_str(),
                };
                number.parse::<f64>().ok().filter(|value| value.is_finite())
            }
        }
    }
}

/// A number without the digits that say nothing: `900`, `0.25`, `-6.5`.
fn trimmed(value: f64) -> String {
    let rounded = (value * 1e4).round() / 1e4;
    // `-0` reads as a mistake.
    format!("{}", if rounded == 0.0 { 0.0 } else { rounded })
}

/// How many channels a device takes and gives. A device is always handed
/// a stereo signal and hands one back; this says what happens to it on the
/// way: a device with one input channel is given both sides mixed, and one
/// with one output channel plays the same on both sides.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChannelLayout {
    /// Channels of the device's main input; 0 for a device that takes no
    /// audio, as most instruments do.
    pub inputs: u32,
    /// Channels of its main output.
    pub outputs: u32,
}

impl ChannelLayout {
    pub const STEREO: Self = Self { inputs: 2, outputs: 2 };

    /// Whether what goes in on the left and on the right is mixed into one
    /// channel before the device sees it.
    pub fn folds_input(&self) -> bool {
        self.inputs == 1
    }

    /// The layout in words: `mono in, stereo out`.
    pub fn describe(&self) -> String {
        let word = |channels: u32| match channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            channels => format!("{channels} channels"),
        };
        match (self.inputs, self.outputs) {
            (0, 0) => "no audio in or out".to_string(),
            (0, outputs) => format!("no audio in, {} out", word(outputs)),
            (inputs, 0) => format!("{} in, no audio out", word(inputs)),
            (inputs, outputs) => format!("{} in, {} out", word(inputs), word(outputs)),
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub params: Vec<(String, f64)>,
    /// Where a browser files the preset (`Bass`, `Pad`); empty for one
    /// that is filed nowhere.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// Words the preset is found by, besides its name and description.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The state the preset gives the device, for a device whose sound is
    /// more than its parameters (a wavetable synth's own tables, and what
    /// its macros are called). A preset without one leaves the device's
    /// state as it is: a sampler keeps its file.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::project::state_base64")]
    pub state: Option<Vec<u8>>,
}

impl Preset {
    /// The preset as a preset list has it: asked for by its name.
    pub fn info(&self) -> PresetInfo {
        PresetInfo {
            id: self.name.clone(),
            name: self.name.clone(),
            category: self.category.clone(),
            tags: self.tags.clone(),
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
///
/// A block lies in one tempo and one time signature. Where either changes
/// inside a song the engine ends the block there, and it takes a tempo
/// that glides in steps of a few hundred frames, each at the one tempo
/// that carries the song from where the glide has it at the step's first
/// frame to where it has it at the first frame of the next. So the song
/// moves evenly through a block, at `tempo`, and the next block begins
/// where this one ends.
#[derive(Copy, Clone, Debug)]
pub struct ProcessInfo {
    pub sample_rate: f64,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub playing: bool,
    /// Song position at the first sample of the block.
    pub position_beats: f64,
    /// Where that is in the song's time and among its bars.
    pub song: SongTime,
    /// `(start, end)` in beats while the transport is looping.
    pub loop_range: Option<(f64, f64)>,
}

/// Where a block begins in the song's time and among its bars: what the
/// song's tempos and time signatures, with every change of them on the
/// way, make of [`ProcessInfo::position_beats`].
///
/// Only in a song of one tempo is the time `beats * 60 / tempo`, and only
/// in one of a single time signature do the bars divide the beats evenly.
/// A device that tells someone else the time or the bar (a host of
/// plugins) reads them here and works nothing out from the block's tempo.
///
/// The default is the start of a song.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SongTime {
    /// Seconds from the start of the song. From the engine this counts
    /// the frames the song has played: it lies on a whole frame, and for
    /// as long as the song plays on without being moved it goes on by
    /// exactly a block's length from one block to the next, whatever the
    /// tempo does meanwhile.
    pub seconds: f64,
    /// The bar the block begins in, counted from 0 at the start of the
    /// song.
    pub bar: u32,
    /// The beat that bar began at.
    pub bar_start: f64,
    /// The loop in seconds, as [`ProcessInfo::loop_range`] is in beats,
    /// while the transport is looping.
    pub loop_seconds: Option<(f64, f64)>,
}

impl SongTime {
    /// Where `beat` is in a song that has one tempo and one time
    /// signature from beginning to end.
    pub fn steady(beat: f64, tempo: f64, signature: TimeSignature, loop_range: Option<(f64, f64)>) -> Self {
        let seconds = |beat: f64| beat * 60.0 / tempo;
        let per_bar = signature.beats_per_bar();
        let bar = (beat / per_bar).floor().max(0.0);
        Self {
            seconds: seconds(beat),
            bar: bar as u32,
            bar_start: bar * per_bar,
            loop_seconds: loop_range.map(|(start, end)| (seconds(start), seconds(end))),
        }
    }
}

impl ProcessInfo {
    /// The block as it is in a song that has had its tempo and its time
    /// signature from the start: its time and its bar worked out from
    /// where it is. A block made by hand that is not at the start of a
    /// song is one of these; the engine's are filled in from the song's
    /// maps.
    pub fn steady(mut self) -> Self {
        self.song = SongTime::steady(self.position_beats, self.tempo, self.time_signature, self.loop_range);
        self
    }
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
/// The parameter by which a built-in instrument says how far its pitch
/// wheel bends, in semitones each way. A track whose profile says nothing
/// of its own takes the range from it, so that what is written for the
/// wheel is written for the wheel there is.
pub const BEND_RANGE_PARAM: &str = "bend_range";
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

    /// Whether the device can listen to a key input: a second signal that
    /// it does not pass on, such as the kick a compressor ducks a bass to.
    /// The same for the whole life of the device.
    fn takes_key(&self) -> bool {
        false
    }

    /// Whether the device has to be made again to be given a key, or to
    /// have it taken away: a plugin whose format has the host switch the
    /// key input on before the plugin runs, where a plugin may take a key
    /// input that is switched on to be the one to listen to. The engine
    /// makes such a device from a model that has a sidechain or from one
    /// that has none ([`crate::Device::sidechain`]), and again when that
    /// changes.
    fn key_is_set_when_made(&self) -> bool {
        false
    }

    /// [`process`](Self::process), with a key input to listen to: `key` is
    /// as long as `left` and `right`. Only a device that
    /// [takes a key](Self::takes_key) is asked this way; without a key it
    /// is asked through `process` and listens to its own input.
    fn process_keyed(
        &mut self,
        info: &ProcessInfo,
        events: &[Event],
        left: &mut [f32],
        right: &mut [f32],
        key: KeyInput<'_>,
    ) {
        let _ = key;
        self.process(info, events, left, right);
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

/// A signal a device listens to and does not pass on: the key input of a
/// compressor, taken from another track. As long as the block it comes with.
#[derive(Copy, Clone, Debug)]
pub struct KeyInput<'a> {
    pub left: &'a [f32],
    pub right: &'a [f32],
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

    /// What the device goes on doing after the preset it was last given,
    /// where that takes long enough to be worth telling whoever asked for
    /// the preset: a drum sampler reads its samples for most of a minute,
    /// and is silent or cut short until it has. `None` for a device that
    /// is as ready as it was. Asking takes the answer: it is given once.
    fn take_preset_note(&mut self) -> Option<String> {
        None
    }

    /// The settings in the device's saved state, by name, with the value
    /// each has as the device itself writes it: for a plugin whose state
    /// is a list of named settings, most of which it shows a host no
    /// parameter for. `None` for a device whose state is not read that
    /// way.
    fn settings(&mut self) -> Option<Vec<(String, String)>> {
        None
    }

    /// Gives settings of [`settings`](Self::settings) other values, by
    /// name: the device is handed its state again with them in place, and
    /// is seen to have taken it before this returns. A name that is no
    /// setting is an error that says so, and changes nothing.
    fn set_settings(&mut self, values: &[(String, String)]) -> Result<(), PresetLoadError> {
        let _ = values;
        Err(PresetLoadError::Unsupported)
    }

    /// What the device's keys play, where its saved state says: a drum
    /// sampler's kit says which drum hangs in each place and which keys
    /// strike it how, and that differs from kit to kit. Every key that
    /// plays something is listed; a key that is not plays nothing. `None`
    /// for a device whose state does not say, which is nearly all.
    fn key_sounds(&mut self) -> Option<Vec<crate::KeySound>> {
        None
    }

    /// Which MIDI controllers the device does something with, if it can
    /// say. Cheap: a view may ask whenever it is drawn.
    fn controller_support(&self) -> ControllerSupport {
        ControllerSupport::Unknown
    }

    /// How many channels the device takes and gives, if it is not the
    /// stereo in and out every device is driven with. Cheap.
    fn channels(&self) -> Option<ChannelLayout> {
        None
    }

    /// What the device itself shows for values of its parameters: for
    /// each `(position in the parameter list, value in plain units)`, the
    /// text (`-6.0 dB`, `SM57`, `On`), or `None` where it has none of its
    /// own. One call for many, since a plugin has to be asked on a thread
    /// of its own.
    fn param_texts(&mut self, values: &[(usize, f64)]) -> Vec<Option<String>> {
        vec![None; values.len()]
    }

    /// The value of the parameter at `index` that the device shows as
    /// `text`, in plain units: the other way round from
    /// [`param_texts`](Self::param_texts). `None` if the device cannot say,
    /// or knows no such value.
    fn param_value(&mut self, index: usize, text: &str) -> Option<f64> {
        let _ = (index, text);
        None
    }

    /// The value the device itself calls each parameter's default, in
    /// parameter-list order and plain units, where that differs from what
    /// [`DeviceInstance::params`] carries: a plugin's list carries the
    /// values it was created with, which are those of whatever patch it
    /// has. `None` for a device whose list has the real defaults.
    fn param_defaults(&self) -> Option<Vec<f64>> {
        None
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
/// What a device may be run beside, where a render uses more than one
/// thread at once. (While a song plays, one thread runs every device.)
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Beside {
    /// Anything: it shares nothing with any other device that what comes
    /// out of it depends on, and does not mind which thread runs it.
    #[default]
    Anything,
    /// Anything but the devices that name the same thing: the module of a
    /// plugin, say, which may keep something for all its instances (some
    /// keep where they are in a round of recordings). Devices that name
    /// the same thing are run one at a time and in the order of the song,
    /// as one thread would run them.
    InTurn(String),
    /// Nothing: a render of a song that has it is made on one thread.
    Nothing,
}

impl Beside {
    /// Whichever of the two leaves less to be run at once.
    pub fn or(self, other: Beside) -> Beside {
        match (self, other) {
            (Beside::Nothing, _) | (_, Beside::Nothing) => Beside::Nothing,
            (Beside::Anything, other) => other,
            (this, _) => this,
        }
    }
}

/// What is known of a device that does not do the same thing twice as it
/// comes (a sampled instrument that draws at random which recording of a
/// note it plays, say), and what makes it: that is what a render gives its
/// own copy of the device when it is asked to be one that can be compared
/// with another ([`DeviceFactory::repeating`]). Where nothing is known
/// that makes it (no setting and no parameter is named), a render can
/// only say so.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Repeating {
    /// What it does, in words that go on from its name: "draws at random
    /// which recording of a note it plays".
    pub what: String,
    /// Settings of the device's saved state to give these values, by name
    /// ([`DeviceController::set_settings`]).
    pub settings: Vec<(String, String)>,
    /// Parameters to set, by name, to these values in their own units. A
    /// name the device has no parameter of is passed over.
    pub params: Vec<(String, f64)>,
    /// What that costs the sound, in a few words that finish "it is
    /// rendered with ...". Empty where nothing is known that stops it.
    pub costs: String,
    /// The device begins its draws at the same place in every process
    /// that has just started, and what it draws depends on nothing but
    /// what it and its like were played there: the same song, rendered in
    /// two such processes, is the same samples all the same. Measured,
    /// not assumed. (A change to the song still moves every draw after
    /// it, which is what [`Repeating`] is for.)
    pub begins_alike: bool,
}

impl Repeating {
    /// Whether anything is known that makes the device repeat itself.
    pub fn can_be_stopped(&self) -> bool {
        !self.settings.is_empty() || !self.params.is_empty()
    }
}

pub trait DeviceFactory: Send + Sync {
    /// Everything this factory can create.
    fn descriptors(&self) -> Vec<DeviceDescriptor>;

    /// Instantiates `device`, restores its saved state and parameter values,
    /// and prepares it for `sample_rate`.
    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError>;

    /// [`create`](Self::create), saying what the device is for. A factory
    /// whose devices are the same either way needs no implementation of
    /// its own. One that hands the work to another factory must hand this
    /// on too, or a plugin behind it is never told that it is rendered.
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

    /// Whether the factory is behind with work that its devices may be
    /// waiting on. The plugins of a host share one thread, on which each
    /// is made, let go of and given its turn; one that takes long to go
    /// holds up another that is waiting to load. Whoever waits for a
    /// device to be ready takes this as a reason to go on waiting. A
    /// factory that hands the work to another must hand this on too.
    fn busy(&self) -> bool {
        false
    }

    /// What a device of this factory may be run beside when a render uses
    /// more than one thread: see [`Beside`]. Anything, for a device that
    /// is not this factory's. A factory that hands the work to another
    /// must hand this on too.
    fn beside(&self, uri: &str) -> Beside {
        let _ = uri;
        Beside::Anything
    }

    /// What makes a device of this factory do the same thing twice, if it
    /// is known not to as it comes and how to stop it: see [`Repeating`].
    /// `None` for a device that repeats itself anyway, and for one nothing
    /// is known of. A factory that hands the work to another must hand
    /// this on too.
    fn repeating(&self, uri: &str) -> Option<Repeating> {
        let _ = uri;
        None
    }

    /// The file the code of a device of this factory is in, where that is
    /// not the program's own: a plugin's file. It is what tells which
    /// plugin a fault was in. `None` for a device that is not this
    /// factory's and for one that is part of the program. A factory that
    /// hands the work to another must hand this on too.
    fn module(&self, uri: &str) -> Option<std::path::PathBuf> {
        let _ = uri;
        None
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

    /// The presets of the device type `uri` as far as they are known
    /// without anything having to be read: what was read in this run of
    /// the program, or is remembered from an earlier one. `None` for a
    /// type that is not this factory's, and for one whose presets nobody
    /// has read yet. Never starts a read, so it is the one to ask about
    /// every device there is.
    fn known_presets(&self, uri: &str) -> Option<PresetList> {
        self.presets(uri)
    }

    /// Whether the device type `uri` is a drum instrument: one whose keys
    /// are drums, not pitches. A track that plays through one is a drum
    /// track unless it is told otherwise. Never waits, and false for a
    /// type that is not this factory's.
    fn plays_drums(&self, uri: &str) -> bool {
        let _ = uri;
        false
    }

    /// The preset in the file at `file`, as a preset of the device type
    /// `uri`: what a device of that type is then told to load
    /// ([`DeviceController::load_preset`]). For a preset that is not
    /// among the type's listed ones because its file lies somewhere else.
    /// `within` says which preset is meant in a file that holds several.
    /// `None` if the type is not one of this factory's.
    ///
    /// An error is the end of a sentence about the file: the type takes
    /// no preset files, the file is another device's, there is no such
    /// file. The file is read only as far as it takes to say whose preset
    /// it is and what it is called, and none of a plugin's code runs;
    /// loading it reads the rest. It may still take as long as starting a
    /// process does: for a caller that may wait.
    fn preset_file(&self, uri: &str, file: &Path, within: Option<&str>) -> Option<Result<PresetInfo, String>> {
        let _ = (file, within);
        let descriptor = self.descriptors().into_iter().find(|descriptor| descriptor.uri == uri)?;
        Some(Err(format!("{} takes no preset files; its presets are asked for by name", descriptor.name)))
    }

    /// The device type the preset file at `file` is for, by its URI, if
    /// that is one of this factory's and can be told without running
    /// anything: by the file's name, and where the name does not say, by
    /// its first bytes. This is for listing the preset files of a folder
    /// with the devices they belong to, so it costs little enough to be
    /// asked of hundreds of files. `None` does not mean that no device
    /// takes the file ([`preset_file`](Self::preset_file) says that).
    fn preset_file_type(&self, file: &Path) -> Option<String> {
        let _ = file;
        None
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

    fn busy(&self) -> bool {
        (**self).busy()
    }

    fn beside(&self, uri: &str) -> Beside {
        (**self).beside(uri)
    }

    fn module(&self, uri: &str) -> Option<std::path::PathBuf> {
        (**self).module(uri)
    }

    fn repeating(&self, uri: &str) -> Option<Repeating> {
        (**self).repeating(uri)
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

    fn known_presets(&self, uri: &str) -> Option<PresetList> {
        (**self).known_presets(uri)
    }

    fn plays_drums(&self, uri: &str) -> bool {
        (**self).plays_drums(uri)
    }

    fn preset_file(&self, uri: &str, file: &Path, within: Option<&str>) -> Option<Result<PresetInfo, String>> {
        (**self).preset_file(uri, file, within)
    }

    fn preset_file_type(&self, file: &Path) -> Option<String> {
        (**self).preset_file_type(file)
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

    fn busy(&self) -> bool {
        self.factories.iter().any(|factory| factory.busy())
    }

    fn beside(&self, uri: &str) -> Beside {
        self.factories.iter().map(|factory| factory.beside(uri)).fold(Beside::Anything, Beside::or)
    }

    fn module(&self, uri: &str) -> Option<std::path::PathBuf> {
        self.factories.iter().find_map(|factory| factory.module(uri))
    }

    fn repeating(&self, uri: &str) -> Option<Repeating> {
        self.factories.iter().find_map(|factory| factory.repeating(uri))
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

    fn known_presets(&self, uri: &str) -> Option<PresetList> {
        self.factories.iter().find_map(|factory| factory.known_presets(uri))
    }

    fn plays_drums(&self, uri: &str) -> bool {
        self.factories.iter().any(|factory| factory.plays_drums(uri))
    }

    fn preset_file(&self, uri: &str, file: &Path, within: Option<&str>) -> Option<Result<PresetInfo, String>> {
        self.factories.iter().find_map(|factory| factory.preset_file(uri, file, within))
    }

    fn preset_file_type(&self, file: &Path) -> Option<String> {
        self.factories.iter().find_map(|factory| factory.preset_file_type(file))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_block_made_by_hand_is_in_a_song_of_one_tempo_and_one_time_signature() {
        use super::{ProcessInfo, SongTime};
        use crate::time::TimeSignature;
        let block = ProcessInfo {
            sample_rate: 48_000.0,
            tempo: 90.0,
            time_signature: TimeSignature::new(6, 8).unwrap(),
            playing: true,
            position_beats: 12.5,
            song: SongTime::default(),
            loop_range: Some((3.0, 9.0)),
        };
        assert_eq!(block.song, SongTime { seconds: 0.0, bar: 0, bar_start: 0.0, loop_seconds: None });
        // 12.5 beats at 90: eight and a third seconds. A bar of 6/8 is
        // three beats, so this is the fifth bar, which began at beat 12.
        let song = block.steady().song;
        assert!((song.seconds - 25.0 / 3.0).abs() < 1e-12);
        assert_eq!((song.bar, song.bar_start), (4, 12.0));
        assert_eq!(song.loop_seconds, Some((2.0, 6.0)));
        // Before the song begins there is no bar before the first.
        let before = ProcessInfo { position_beats: -2.0, ..block }.steady().song;
        assert_eq!((before.bar, before.bar_start), (0, 0.0));
        assert!(before.seconds < 0.0);
    }

    use super::*;

    fn param(unit: &str, kind: ParamKind) -> ParamInfo {
        ParamInfo {
            id: "p".into(),
            name: "P".into(),
            min: -100.0,
            max: 20_000.0,
            default: 0.0,
            unit: unit.into(),
            kind,
        }
    }

    #[test]
    fn a_value_is_displayed_as_a_reader_takes_it_in_and_read_back_from_that() {
        let cutoff = param("Hz", ParamKind::Float);
        assert_eq!(cutoff.display(900.0), "900 Hz");
        assert_eq!(cutoff.display(0.123_456), "0.1235 Hz");
        assert_eq!(cutoff.display(-0.000_001), "0 Hz", "not -0");
        for text in ["900 Hz", "900hz", " 900 ", "900.0 HZ"] {
            assert_eq!(cutoff.parse(text), Some(900.0), "{text}");
        }
        assert_eq!(param("dB", ParamKind::Float).parse("-6 dB"), Some(-6.0));
        assert_eq!(cutoff.parse("loud"), None);
        assert_eq!(cutoff.parse("900 dB"), None, "another unit is another thing");
        assert_eq!(cutoff.parse("inf"), None);

        let plain = param("", ParamKind::Int);
        assert_eq!((plain.display(3.0), plain.parse("3")), ("3".to_string(), Some(3.0)));

        let switch = param("", ParamKind::Bool);
        assert_eq!((switch.display(1.0), switch.display(0.0)), ("On".to_string(), "Off".to_string()));
        assert_eq!((switch.parse("ON"), switch.parse("off"), switch.parse("1")), (Some(1.0), Some(0.0), Some(1.0)));

        let wave = param("", ParamKind::Enum(vec!["Sine".into(), "Saw".into()]));
        assert_eq!((wave.display(1.0), wave.display(7.0)), ("Saw".to_string(), "7".to_string()));
        assert_eq!((wave.parse(" saw "), wave.parse("Square"), wave.parse("1")), (Some(1.0), None, None));
    }

    #[test]
    fn a_channel_layout_is_said_in_words() {
        assert_eq!(ChannelLayout::STEREO.describe(), "stereo in, stereo out");
        let amp = ChannelLayout { inputs: 1, outputs: 2 };
        assert_eq!(amp.describe(), "mono in, stereo out");
        assert!(amp.folds_input() && !ChannelLayout::STEREO.folds_input());
        assert_eq!(ChannelLayout { inputs: 0, outputs: 2 }.describe(), "no audio in, stereo out");
        assert_eq!(ChannelLayout { inputs: 2, outputs: 6 }.describe(), "stereo in, 6 channels out");
        assert_eq!(ChannelLayout { inputs: 0, outputs: 0 }.describe(), "no audio in or out");
    }
}
