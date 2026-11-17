//! Tool arguments. Field docs become the JSON-schema descriptions the model
//! reads, so they are written for that audience.

use crate::input::{ControllerPointInput, NoteInput, PitchInput, PointInput};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetProjectRequest {
    /// Include every note of every clip. Leave off unless you need them all; `get_notes` reads a single clip.
    #[serde(default)]
    pub include_notes: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoopInput {
    pub enabled: bool,
    /// Loop start in beats.
    pub start: f64,
    /// Loop end in beats.
    pub end: f64,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetProjectRequest {
    pub name: Option<String>,
    /// Beats per minute, 20-999: the tempo the song begins at. set_tempo changes it from a later bar on.
    pub tempo: Option<f64>,
    /// For example "4/4", "3/4", "6/8" or "7/8": the time signature the song begins in. set_time_signature
    /// changes it from a later bar on.
    pub time_signature: Option<String>,
    /// Region that live playback repeats. Rendering ignores it.
    #[serde(rename = "loop")]
    pub loop_region: Option<LoopInput>,
    /// Master volume in dB (at most 6).
    pub master_volume_db: Option<f32>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKindInput {
    /// Holds clips and plays them through its devices.
    #[default]
    Normal,
    /// Mixes the tracks routed through it, so they share effects and a fader (a drum bus).
    Group,
    /// Receives what other tracks send to it: put one reverb or delay here and share it.
    Return,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendInput {
    /// Id of the return track.
    pub to: u64,
    /// Send level in dB (0 = as loud as the track itself, at most 6). null removes the send. To open a send
    /// for one note or one bar (a delay throw), leave it out here and automate it: set_automation with a
    /// target of type "send".
    pub level_db: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTrackRequest {
    pub name: String,
    /// "normal" (default), "group" or "return". Group and return tracks hold no clips.
    #[serde(default)]
    pub kind: TrackKindInput,
    /// Instrument to put on the track, e.g. "builtin:polysynth" or "builtin:drumkit" (see `list_devices`).
    /// Omit for a track that only holds audio clips.
    pub instrument: Option<String>,
    /// Preset of the instrument, e.g. "warm_pad", "bass", "pluck", "keys" for the poly synth. For a plugin: a
    /// preset's name, its category and name ("Pads/Warm Pad"), or its id, as `describe_device` lists them.
    /// Or the whole path of a preset file that is not among those, such as one the user bought and keeps with
    /// their samples ("D:\\Packs\\Lead.SerumPreset", a .vstpreset): see add_device `preset`.
    pub preset: Option<String>,
    /// Track volume in dB (default 0, at most 6).
    pub volume_db: Option<f32>,
    /// -1 (left) to 1 (right), default 0.
    pub pan: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTrackRequest {
    /// Track id.
    pub track: u64,
    pub name: Option<String>,
    /// Track volume in dB (at most 6).
    pub volume_db: Option<f32>,
    /// -1 (left) to 1 (right).
    pub pan: Option<f32>,
    pub mute: Option<bool>,
    pub solo: Option<bool>,
    /// New position in the track list, 0 = top.
    pub index: Option<usize>,
    /// Route this track through a group track (its id), or back to the master with 0.
    pub group: Option<u64>,
    /// Set, change or remove a send from this track to a return track. The send is taken after the track's
    /// fader.
    pub send: Option<SendInput>,
    /// Arm the track: what is played live (a MIDI keyboard, play_notes) goes to the armed tracks, and
    /// `transport` with action "record" records them. Not part of the project file; not an undo step.
    pub armed: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackRequest {
    /// Track id.
    pub track: u64,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListDevicesRequest {
    /// "instrument" or "effect". Omit for both.
    pub category: Option<String>,
    /// Only devices whose name, vendor or URI contains this text.
    pub query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DescribeDeviceRequest {
    /// Device URI from `list_devices`, e.g. "builtin:reverb".
    pub uri: String,
    /// Words to search the device's presets for, e.g. "warm pad". Each must be in a preset's name, category,
    /// tags, author or description.
    pub preset_query: Option<String>,
    /// Only presets filed under this category or bank; any part of its path will do, e.g. "Pads".
    pub preset_category: Option<String>,
    /// Skip this many of the matching presets: the reply's `more` says where the next page starts.
    pub preset_offset: Option<usize>,
    /// How many presets to list (default 20, at most 100; 0 for none).
    pub preset_limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddDeviceRequest {
    /// Track id. Omit to add the device to the master chain.
    pub track: Option<u64>,
    /// Device URI from `list_devices`.
    pub uri: String,
    /// Preset to start from: its name, its category and name ("Pads/Warm Pad"), or its id, as
    /// `describe_device` lists them. Or the whole path, from the drive down, of a preset file that lies
    /// anywhere: a .vstpreset for any VST3 plugin, a file of the maker's own format where Trill reads it
    /// (.SerumPreset, an Ample instrument's preset, an Archetype's), a file a CLAP plugin loads itself, or a
    /// .trillpreset that save_preset wrote. For a file that holds several presets, name one after a '#':
    /// "C:\\Kits\\s3presetconf#Black Metal". A file that is another plugin's, or damaged, is an error that
    /// says so and adds nothing.
    pub preset: Option<String>,
    /// Parameter values to set on top of the preset, by parameter id or name: numbers in the parameter's units,
    /// true/false for switches, or the choice name for enums (e.g. {"cutoff": 800, "filter_type": "lowpass24"}).
    /// For a plugin a string is the value as the plugin shows it ({"Input Gain": "-6 dB"}); see update_device.
    pub params: Option<BTreeMap<String, Value>>,
    /// Position in the chain, 0 = first. Omit to append. Signal flows first to last; instruments go first.
    pub index: Option<usize>,
    /// Id of the track this device listens to in place of its own input: the key of a compressor. To duck a
    /// bass to the kick, the compressor goes on the bass and `sidechain` is the kick's track. The track is heard
    /// as it leaves its devices, ahead of its fader and mute, so a muted track still serves as a key. Only for
    /// a device that takes a key: builtin:compressor.
    pub sidechain: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDeviceRequest {
    /// Device id.
    pub device: u64,
    /// Only parameters whose name has these words (or whose id contains this text), e.g. "cab mic". A plugin
    /// has hundreds of parameters: this is how to find the one you mean, with its range and its choices.
    pub query: Option<String>,
    /// Which parameters to list when `query` is not given: "changed" (those not at the device's own defaults,
    /// which is the default for a device with many parameters), "all", or "none".
    pub show: Option<String>,
    /// Skip this many of the listed parameters: the reply's `params_more` says where the next page starts.
    pub param_offset: Option<usize>,
    /// How many parameters to list (default 40 for a device with many, at most 200).
    pub param_limit: Option<usize>,
    /// Words to search the presets this device can be given for, e.g. "warm pad". With any `preset_` argument
    /// the reply lists presets, and parameters only if `query` is given too.
    pub preset_query: Option<String>,
    /// Only presets filed under this category or bank; any part of its path will do, e.g. "Pads".
    pub preset_category: Option<String>,
    /// Skip this many of the matching presets: the reply's `more` says where the next page starts.
    pub preset_offset: Option<usize>,
    /// How many presets to list (default 20, at most 100).
    pub preset_limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateDeviceRequest {
    /// Device id.
    pub device: u64,
    /// Preset to switch to: its name, its category and name ("Pads/Warm Pad"), or its id, as `get_device` and
    /// `describe_device` list them; or the whole path of a preset file (see add_device `preset`). It replaces
    /// the device's sound; `params` are applied on top of it.
    pub preset: Option<String>,
    /// Parameter values by parameter id or name. For a built-in device: numbers in the parameter's units,
    /// true/false for switches, or the choice name for enums. For a plugin: a number is the raw value (0 to 1
    /// for most), and a string is the value as the plugin itself shows it, which is what get_device gives as
    /// `text`: {"Cab L Mic Type": "Dynamic 57", "Input Gain": "-6 dB", "Doubler Active": "On"}.
    pub params: Option<BTreeMap<String, Value>>,
    /// Settings in a plugin's saved state that it has no parameter for, by the names get_device lists under
    /// `settings` (its `query` finds them): {"Auto Legato": 0}. A value is written as the plugin writes it,
    /// which get_device shows: keep to the form the value has (a switch that reads 1 is off as 0 or as -1).
    /// When you have the names of a patch and do not know which are parameters and which are settings, give
    /// them all under `params`: the refusal names every one that is a setting, with its value, and nothing is
    /// changed.
    pub settings: Option<BTreeMap<String, Value>>,
    /// false bypasses the device.
    pub enabled: Option<bool>,
    /// Id of the track this device listens to in place of its own input (see add_device), or 0 to have it
    /// listen to its own input again.
    pub sidechain: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeviceRequest {
    /// Device id.
    pub device: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateClipRequest {
    /// Track id.
    pub track: u64,
    /// Position on the timeline in beats (bar N of 4/4 starts at (N-1)*4).
    pub start: f64,
    /// Length in beats.
    pub length: f64,
    pub name: Option<String>,
    /// Notes to put in the clip. Note starts are relative to the clip start.
    pub notes: Option<Vec<NoteInput>>,
    /// If set and shorter than `length`, the first `loop_length` beats of notes repeat to fill the clip.
    /// Write one bar of drums, set loop_length to one bar and length to eight bars to get eight bars.
    pub loop_length: Option<f64>,
    /// Controller curves to put in the clip with the notes, by controller, as set_controllers takes them:
    /// {"expression": [[0, 50], [4, 110]], "sustain": [[0, 127], [3.5, 0]]}.
    pub controllers: Option<BTreeMap<String, Vec<ControllerPointInput>>>,
}

/// How an audio clip follows the project tempo.
#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarpInput {
    /// Plays at the speed and pitch it was recorded at, whatever the project tempo.
    Off,
    /// Follows the tempo by playing faster or slower; the pitch moves with it, like a record.
    Repitch,
    /// Follows the tempo and keeps the pitch, keeping drum hits sharp: for drums and rhythmic loops.
    Beats,
    /// Follows the tempo and keeps the pitch, keeping sustained sound smooth: for chords, pads, bass, vocals.
    Tonal,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAudioClipRequest {
    /// Track id.
    pub track: u64,
    /// Path of an audio file: WAV, AIFF, FLAC, MP3 or Ogg Vorbis. `search_sounds` finds them.
    pub path: String,
    /// Position on the timeline in beats.
    pub start: f64,
    /// Length in beats. Defaults to the whole file (from `offset_seconds` on): for a warped clip that is the
    /// number of beats in the recording, otherwise its duration at the current tempo.
    pub length: Option<f64>,
    /// Where in the file to start, in seconds of the recording.
    pub offset_seconds: Option<f64>,
    /// Clip gain in dB.
    pub gain_db: Option<f32>,
    /// Fade-in length in beats.
    pub fade_in: Option<f64>,
    /// Fade-out length in beats.
    pub fade_out: Option<f64>,
    pub name: Option<String>,
    /// "off" (default), "repitch", "beats" or "tonal". Anything but "off" makes the clip follow the project
    /// tempo: N beats of the loop fill N beats of the song. Use "beats" for drums, "tonal" for melodic material.
    pub warp: Option<WarpInput>,
    /// The tempo the file was recorded at, in BPM. Leave it out when the file name says it ("... 124bpm ...");
    /// warping a file whose name does not is an error until you give it.
    pub source_tempo: Option<f64>,
    /// Pitch shift in semitones (-24 to 24) without changing speed. Needs warp "beats" or "tonal".
    pub transpose: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateClipRequest {
    /// Clip id.
    pub clip: u64,
    pub name: Option<String>,
    /// New position on the timeline in beats. Moves the whole clip.
    pub start: Option<f64>,
    /// New length in beats. Moves the clip's right edge.
    pub length: Option<f64>,
    /// Move the clip's LEFT edge to this beat without moving its contents: later hides the beginning,
    /// earlier reveals it again. Cannot be combined with `start`.
    pub trim_start: Option<f64>,
    /// Fade-in length in beats (audio clips).
    pub fade_in: Option<f64>,
    /// Fade-out length in beats (audio clips).
    pub fade_out: Option<f64>,
    /// New loop length in beats; 0 removes the loop.
    pub loop_length: Option<f64>,
    pub muted: Option<bool>,
    /// Move the clip to this track.
    pub track: Option<u64>,
    /// Audio clips: "off", "repitch", "beats" or "tonal" (see create_audio_clip). The clip keeps its place and
    /// length; the reply says if that no longer matches its audio.
    pub warp: Option<WarpInput>,
    /// Audio clips: the tempo the file was recorded at, in BPM.
    pub source_tempo: Option<f64>,
    /// Audio clips: pitch shift in semitones (-24 to 24). Needs warp "beats" or "tonal".
    pub transpose: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DuplicateClipRequest {
    /// Clip id.
    pub clip: u64,
    /// Where the first copy starts, in beats. Defaults to right after the original.
    pub start: Option<f64>,
    /// Number of copies, placed back to back (default 1).
    pub count: Option<u32>,
    /// Put the copies on this track instead of the original's.
    pub track: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SplitClipRequest {
    /// Clip id.
    pub clip: u64,
    /// Timeline position in beats, strictly inside the clip, where it is cut.
    pub at: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipRequest {
    /// Clip id.
    pub clip: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NotesRequest {
    /// Clip id.
    pub clip: u64,
    pub notes: Vec<NoteInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetControllersRequest {
    /// Clip id.
    pub clip: u64,
    /// Read only this controller: a name ("expression", "mod_wheel", "pitch_bend", "sustain", "breath",
    /// "pressure", "volume") or a controller number. Omit for all the clip has.
    pub controller: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetControllersRequest {
    /// Clip id.
    pub clip: u64,
    /// The curves to write, by controller: a name ("expression", "mod_wheel", "pitch_bend", "sustain",
    /// "breath", "pressure", "volume", "pan") or a controller number as a string ("74"). Each replaces the
    /// clip's curve for that controller; an empty list removes it; controllers not named are left alone.
    /// For example {"expression": [[0, 40], [3, 110, 0.4], [4, 70]], "sustain": [[0, 127], [3.5, 0]]}.
    pub controllers: BTreeMap<String, Vec<ControllerPointInput>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveNotesRequest {
    /// Clip id.
    pub clip: u64,
    /// Only notes starting at or after this beat (relative to the clip).
    pub start: Option<f64>,
    /// Only notes starting before this beat (relative to the clip).
    pub end: Option<f64>,
    /// Only notes of this pitch.
    pub pitch: Option<PitchInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransformNotesRequest {
    /// Clip id.
    pub clip: u64,
    /// Only notes starting at or after this beat (relative to the clip).
    pub start: Option<f64>,
    /// Only notes starting before this beat (relative to the clip).
    pub end: Option<f64>,
    /// Semitones to shift pitch by.
    pub transpose: Option<i32>,
    /// Beats to move notes by (negative = earlier).
    pub shift: Option<f64>,
    /// Snap note starts to this grid in beats (0.25 = sixteenth notes). Applied before humanizing.
    pub quantize: Option<f64>,
    /// Multiply note lengths (0.5 = staccato, 1.1 = slightly overlapping legato).
    pub length_scale: Option<f64>,
    /// Multiply velocities (0.8 = softer).
    pub velocity_scale: Option<f64>,
    /// Randomly move each note start by up to this many beats (0.01-0.03 sounds human).
    pub humanize_timing: Option<f64>,
    /// Randomly change each velocity by up to this amount (5-15 sounds human).
    pub humanize_velocity: Option<u8>,
    /// Seed for the humanizing randomness; the same seed gives the same result.
    pub seed: Option<u32>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    TrackVolume,
    TrackPan,
    DeviceParam,
    MasterVolume,
    /// How much of a track goes to a return track, in dB.
    Send,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TargetInput {
    /// "track_volume" and "master_volume" (dB), "track_pan" (-1 to 1), "send" (dB: how much of `track` goes to
    /// the return track `to`) or "device_param" (the parameter's own units).
    #[serde(rename = "type")]
    pub kind: TargetKind,
    /// Track id, for track_volume, track_pan and send (the track that sends).
    pub track: Option<u64>,
    /// Device id, for device_param.
    pub device: Option<u64>,
    /// Parameter id, for device_param.
    pub param: Option<String>,
    /// Id of the return track, for send.
    pub to: Option<u64>,
}

/// What a line between two points of an envelope is straight along.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TravelInput {
    /// Along the control's own travel: the default.
    Control,
    /// Straight in the value's units: so many dB or Hz a beat.
    Linear,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetAutomationRequest {
    pub target: TargetInput,
    /// The complete envelope; it replaces any existing one. Values go from each point to the next in a line
    /// that is straight along the control's travel (see `travel`) unless a point's `shape` says otherwise, and
    /// are held before the first and after the last. For a send, -70 dB or lower is shut: nothing is sent. An
    /// empty list removes the automation. Omit to keep the envelope as it is (when only switching `enabled`).
    pub points: Option<Vec<PointInput>>,
    /// false switches the automation off without deleting it: the target then stays at the value it is set to
    /// (update_device, update_track). true switches it back on. Writing `points` switches it on unless this
    /// says otherwise.
    pub enabled: Option<bool>,
    /// What the lines are straight along. Omit it (or "control") for the control's own travel, which is what a
    /// fade or a sweep means: a volume or a send moves as a fader does, a frequency or a time by ratio, an
    /// amount evenly. "linear" makes them straight in the value's units instead (a fade then loses the same
    /// number of dB every beat and is all but silent half way down). The reply says which the envelope has.
    pub travel: Option<TravelInput>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetAutomationRequest {
    /// Only this envelope. Omit for every envelope in the project.
    pub target: Option<TargetInput>,
    /// Only the points from this beat on.
    pub start: Option<f64>,
    /// Only the points before this beat.
    pub end: Option<f64>,
    /// Skip this many of the envelope's points (with `target`): how to read on when a reply said `next_offset`.
    pub offset: Option<usize>,
    /// The most points to list in one reply, over all envelopes (default 1000, at most 5000). Every envelope
    /// is listed with its point_count whether or not its points fit.
    pub limit: Option<usize>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    Play,
    /// Ends a take that is being recorded, too: the reply then lists the clips it wrote.
    Stop,
    Seek,
    Status,
    /// Stop and silence everything immediately, including reverb and delay tails.
    Panic,
    /// Record what is played live on the armed tracks into clips, from the playhead. If the song is stopped
    /// it starts after a count-in; if it is playing, recording begins at once. Ends with `stop`.
    Record,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransportRequest {
    pub action: TransportAction,
    /// Beat to move the playhead to; required for `seek`, optional for `play` and `record` (from there).
    pub position: Option<f64>,
    /// Bars of metronome before a recording starts, 0-8 (0 = none). Kept until changed; one bar at first.
    pub count_in_bars: Option<u32>,
    /// Turn the metronome on or off. It clicks while the song plays and is never part of a render.
    pub metronome: Option<bool>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioAction {
    /// The stream that is running: device, sample rate, buffer, latency, load.
    #[default]
    Status,
    /// The same, with the output and input devices that could be chosen.
    Devices,
    /// Change the device, sample rate, buffer size or exclusive mode. Only the fields given change.
    Set,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AudioRequest {
    #[serde(default)]
    pub action: AudioAction,
    /// Name of the output device, as `devices` lists it. "default" follows the system's default device.
    pub output: Option<String>,
    /// Sample rate in Hz. 0 leaves it to the device.
    pub sample_rate: Option<u32>,
    /// Buffer size in frames: smaller is less latency and more risk of drop-outs. 0 leaves it to the device.
    /// A device shared with other programs may ignore it; `exclusive` makes it count.
    pub buffer_size: Option<u32>,
    /// Take the output device for Trill alone: the shortest latency, and no other program can play through
    /// the device meanwhile.
    pub exclusive: Option<bool>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MidiInputsAction {
    #[default]
    List,
    /// Listen to the input called `name`.
    Enable,
    /// Stop listening to the input called `name`.
    Disable,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MidiInputsRequest {
    #[serde(default)]
    pub action: MidiInputsAction,
    /// The input's name, as `list` gives it. Needed for `enable` and `disable`.
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlayNotesRequest {
    /// Track to play on, whether or not it is armed. Omit to play where a keyboard would: on the armed tracks.
    pub track: Option<u64>,
    /// Notes as [pitch, start, length, velocity] or objects, like create_clip takes. `start` is in beats from
    /// now, at the project tempo.
    pub notes: Vec<NoteInput>,
    /// Controllers to move before the first note, by name or number: {"expression": 100} (0-127; pitch_bend
    /// -1 to 1). An instrument that waits for expression is silent until it is given some. They stay where
    /// they are put, and are recorded if a recording is running.
    pub controllers: Option<BTreeMap<String, f64>>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryAction {
    Undo,
    Redo,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HistoryRequest {
    pub action: HistoryAction,
    /// How many steps (default 1).
    pub steps: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveProjectRequest {
    /// Where to save. Omit to save to the file the project was loaded from or last saved to.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadProjectRequest {
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportMidiRequest {
    /// Path of a Standard MIDI File (.mid).
    pub path: String,
    /// Beat at which the imported clips start (default 0).
    pub at: Option<f64>,
    /// Instrument for the new melodic tracks (default "builtin:polysynth"). Drum parts always get
    /// "builtin:drumkit".
    pub instrument: Option<String>,
    /// Also adopt the file's tempo and time signature, with every change of them in the file, from `at` on.
    /// Defaults to true when the project has no tracks yet.
    pub use_tempo: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetTempoRequest {
    /// Beats per minute, 20-999. Needed unless `remove` is true.
    pub bpm: Option<f64>,
    /// The bar the tempo is set at, counted from 1 as get_tempo_map counts them (default 1: the tempo the
    /// song begins at).
    pub bar: Option<u32>,
    /// The beat in that bar, counted from 1 in quarter notes (default 1, the bar line; 3 is two beats in,
    /// 2.5 a beat and a half in).
    pub beat: Option<f64>,
    /// false (default): the tempo jumps to `bpm` here and stays there. true: it glides here, evenly from
    /// beat to beat, from the change before this one (or from the start of the song) and arrives at `bpm`
    /// here: an accelerando or a ritardando. For a glide that sets out later than the change before it, first
    /// set the tempo it sets out from at the bar it sets out at.
    pub ramp: Option<bool>,
    /// true removes what the tempo does here (the glide that arrives and the tempo that takes over, whichever
    /// there is); the tempo before goes on.
    pub remove: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetTimeSignatureRequest {
    /// For example "4/4", "3/4", "6/8", "7/8" or "5/4": 1 to 32 over 1, 2, 4, 8 or 16. Needed unless
    /// `remove` is true.
    pub time_signature: Option<String>,
    /// The bar it takes over at, counted from 1 as get_tempo_map counts them (default 1: the time signature
    /// the song begins in).
    pub bar: Option<u32>,
    /// For this many bars only: after them, the time signature that was in force here before takes over
    /// again. Omit to leave it in force until the next change.
    pub bars: Option<u32>,
    /// true removes the change at this bar; the time signature before it goes on.
    pub remove: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetTempoMapRequest {
    /// Bars to look up, counted from 1: for each, the beat it starts at, how many beats long it is, the
    /// second it starts at, its time signature and the tempo there.
    pub bars: Option<Vec<u32>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportMidiRequest {
    /// Where to write the .mid file.
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkerInput {
    /// Timeline position in beats.
    pub beat: f64,
    /// Section name, e.g. "Intro", "Verse", "Drop".
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetMarkersRequest {
    /// The complete list of markers; it replaces the existing ones. An empty list removes them all.
    pub markers: Vec<MarkerInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditTimeRequest {
    /// "delete": remove the time from `start` to `end`; what follows moves earlier and closes the gap.
    /// "duplicate": repeat the time from `start` to `end` right after itself; what follows moves later.
    /// "insert": open up `length` beats of empty time at `start`; what follows moves later.
    /// "copy": copy the time from `start` to `end` to the beat `to`, on every track or on `tracks`.
    pub action: String,
    /// Where the stretch begins, in beats (bar N of 4/4 starts at (N-1)*4; where the time signature changes,
    /// get_tempo_map gives the beat a bar starts at).
    pub start: f64,
    /// Where the stretch ends, in beats. Needed for "delete", "duplicate" and "copy".
    pub end: Option<f64>,
    /// How many beats to open up. Needed for "insert".
    pub length: Option<f64>,
    /// For "copy": the beat the copy starts at. It may lie anywhere, inside the stretch too.
    pub to: Option<f64>,
    /// For "copy": the ids of the tracks to copy. Omit to copy every track, with the master's automation.
    pub tracks: Option<Vec<u64>>,
    /// For "copy": true (default) makes room at `to`, moving everything from there on later on every track
    /// (reuse a chorus after the bridge). false puts the copy over what the copied tracks hold there, and
    /// nothing moves (fill bars that are still empty, or replace a section).
    pub make_room: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeviceEditorRequest {
    /// Device id of a plugin.
    pub device: u64,
    /// true (default) opens the window or brings it to the front; false closes it.
    pub open: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecoverProjectRequest {
    /// "list" (default) shows what was lost; "open" brings one back in place of the open project;
    /// "discard" deletes one for good.
    pub action: Option<String>,
    /// Which one, by its `index` in the list. Needed for "open" and "discard".
    pub index: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NewProjectRequest {
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchSoundsRequest {
    /// Words to look for, in any order and letter case, in file names, folder names, tags, tempo ("140bpm") and
    /// key ("f#m"): e.g. "deep kick", "808", "piano loop 90bpm". Omit to list everything the filters allow.
    pub text: Option<String>,
    /// Only sounds carrying every one of these tags. Drums: kick, snare, clap, hat, tom, cymbal, perc, drum.
    /// Tonal: 808, bass, lead, pad, pluck, keys, guitar, synth, strings, brass, bell, chord, arp, stab, vocal.
    /// Effects: fx, riser, impact, atmosphere, foley. Form: loop, one-shot, fill.
    pub tags: Option<Vec<String>>,
    /// "audio" (default), "midi", "project", "preset" or "any".
    pub kind: Option<String>,
    /// Only sounds whose tempo is known and at least this many BPM.
    pub tempo_min: Option<f32>,
    /// Only sounds whose tempo is known and at most this many BPM.
    pub tempo_max: Option<f32>,
    /// Only sounds in this key, e.g. "Am", "F# minor", "C".
    pub key: Option<String>,
    /// Only sounds at least this long, in seconds.
    pub seconds_min: Option<f64>,
    /// Only sounds at most this long, in seconds (1.5 keeps to single hits).
    pub seconds_max: Option<f64>,
    /// Only sounds in this folder of the library or the folders beneath it.
    pub folder: Option<String>,
    /// Only sounds the user has marked as favorites.
    #[serde(default)]
    pub favorites_only: bool,
    /// The most results to return (default 20, at most 200). The reply says how many matched in all.
    pub limit: Option<usize>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryAction {
    /// The library's folders, how many files it lists, and whether a scan is running.
    Status,
    /// Add `path` to the library and scan it.
    AddFolder,
    /// Take `path` out of the library. Nothing on disk is touched.
    RemoveFolder,
    /// Say that the library's folder `path` is at `to` now: it was moved or renamed outside Trill. Favorites and
    /// recently used files that were in it stay with their files, and nothing is read again. Nothing on disk is
    /// touched.
    MoveFolder,
    /// Look again for new, changed and deleted files in every folder.
    Rescan,
    /// Folders on this machine that probably hold samples (Splice, DAW packs, a Samples folder), to offer the user.
    Suggestions,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LibraryRequest {
    pub action: LibraryAction,
    /// The folder, for `add_folder`, `remove_folder` and `move_folder` (the folder as the library has it).
    pub path: Option<String>,
    /// For `move_folder`: where the folder is now.
    pub to: Option<String>,
    /// For `add_folder` and `rescan`: how long to wait for the scan before replying, in seconds (default 15, at
    /// most 120). A scan that takes longer carries on in the background; `status` shows its progress.
    pub wait_seconds: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PreviewSoundRequest {
    /// The audio file to play through the user's speakers, over the song. Omit to stop the preview.
    pub path: Option<String>,
    /// Preview level in dB (default 0, at most 6).
    pub gain_db: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetSampleRequest {
    /// Id of a `builtin:sampler` or `builtin:drumrack` device.
    pub device: u64,
    /// Drum racks only: which pad, as a MIDI note 36-51 or a drum name ("kick" = 36, "snare" = 38,
    /// "closed_hat" = 42, "open_hat" = 46, "clap" = 39, "crash" = 49...). Notes written with the same name or
    /// number then play this sample.
    pub pad: Option<PitchInput>,
    /// The audio file to load, or null to empty the sampler or pad. Must be given, one or the other.
    #[serde(default, deserialize_with = "present")]
    #[schemars(with = "Option<String>")]
    pub path: Option<Option<String>>,
}

/// Reads a field that may be null but must be written: absent stays `None`, null becomes `Some(None)`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

// ---- how the song was made: the journal ---------------------------------------

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalShow {
    /// Only the steps: what changed the song.
    #[default]
    Steps,
    /// Everything: also what was said in the window's conversation with the assistant, the assistant's plan, and
    /// the calls that changed nothing (reading, measuring, probing), with what they heard.
    All,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalBy {
    /// The user, at the window.
    Person,
    /// The assistant in the window.
    Assistant,
    /// Programs working through this server, like you.
    Client,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetJournalRequest {
    /// List what came after this step number: a `step` from an earlier reply, or `more.after` to read the next
    /// page. Omit to start where the story begins.
    pub after: Option<u64>,
    /// How many entries to return (default 40, at most 200).
    pub limit: Option<usize>,
    /// Instead of reading from `after`: the latest this many entries, for "what happened last".
    pub last: Option<usize>,
    /// "steps" (default) lists what changed the song; "all" also what was said and what was only looked at.
    #[serde(default)]
    pub show: JournalShow,
    /// Only entries whose words contain this, whatever the case: "chorus", "reverb", "tempo".
    pub search: Option<String>,
    /// Only entries made by "person" (the user at the window), "assistant" (the assistant in the window) or
    /// "client" (programs like you).
    pub by: Option<JournalBy>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetSongAtRequest {
    /// The step to show the song after: a `step` from get_journal. 0, or leaving it out, is the song as it was
    /// when the story begins.
    pub step: Option<u64>,
    /// Include every note of every clip. Leave off unless you need them.
    #[serde(default)]
    pub include_notes: bool,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalAction {
    /// Whether a journal is kept, how many steps it has and how large it is.
    #[default]
    Status,
    /// Keep a journal from now on (the default).
    Keep,
    /// Stop keeping one. What was written stays until it is deleted.
    Stop,
    /// Write the story out to `path` as a .trillstory file that can be handed to someone else and watched there.
    Export,
    /// Delete the journal of the open song for good. The story begins again from the song as it is.
    Delete,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct JournalRequest {
    #[serde(default)]
    pub action: JournalAction,
    /// With "export": where to write the file.
    pub path: Option<String>,
    /// With "export": include what was said between the user and the assistant, and the assistant's plan.
    /// Default false: only how the song was made. Ask the user before turning this on.
    #[serde(default)]
    pub conversation: bool,
}

// ---- films ---------------------------------------------------------------------------

/// What a film is of.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilmOf {
    /// The song as it plays: its lanes, clips and piano rolls on the stage, filmed by a director, with the
    /// song's own sound.
    #[default]
    Song,
    /// How the song was made: the session's journal played again, step by step, with the finished song under it.
    MakingOf,
}

/// How a stretch of a film is framed.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilmShot {
    /// The whole song in the frame, the playhead crossing it.
    Song,
    /// The lanes of every part that plays, eight bars across.
    Wide,
    /// The lanes above and the piano rolls of two or three parts below, four bars across.
    Ensemble,
    /// One part's piano roll, large, with the lanes as a strip above it.
    Close,
}

/// How a stretch of a film is entered.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilmEntry {
    /// At once, on the beat, with a flash.
    Cut,
    /// The camera travels there over a bar.
    Move,
}

/// What is asked of the director for one stretch of the song. Whatever is left out stays the director's choice.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct FilmDirection {
    /// The stretch, as a section by its marker's name ("CHORUS 2"). Give this, or `from_bar`.
    pub section: Option<String>,
    /// The stretch, in bars counted from 1: from the start of this bar…
    pub from_bar: Option<f64>,
    /// …to the end of this one (default: the same bar as `from_bar`, or the end of `section`).
    pub to_bar: Option<f64>,
    /// How to frame it. Omit and name `parts` to let them decide: one part is held close, two or three are an
    /// ensemble.
    pub shot: Option<FilmShot>,
    /// Ids of the tracks to show as piano rolls, top to bottom: for "close" the one to hold close, for
    /// "ensemble" up to three. Omit to let the director pick who drives the stretch.
    pub parts: Option<Vec<u64>>,
    /// How to come to it. Omit for a cut where the frame changes and a move where it does not.
    pub enter: Option<FilmEntry>,
}

/// How much work goes into each frame.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilmQuality {
    /// Each frame exposed from eight moments (the blur of motion) and drawn at twice its size. About half real
    /// time at 1920x1080 on a good graphics card.
    #[default]
    Fine,
    /// One moment a frame at its own size, as the window shows the stage: about twice as fast.
    Plain,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FilmPlanRequest {
    /// What the film would be of (default "song").
    #[serde(default)]
    pub of: FilmOf,
    /// Film only this section, by its marker's name. Omit for the whole song (or give `start`/`end`).
    pub section: Option<String>,
    /// Film from this beat (default 0).
    pub start: Option<f64>,
    /// Film up to this beat (default: the end of the song).
    pub end: Option<f64>,
    /// What you ask of the director, stretch by stretch, in order: where two speak of the same bars the later
    /// one has them. Omit for the director's own plan.
    pub direct: Option<Vec<FilmDirection>>,
    /// With "making_of": how many seconds of the story a second of film is (default 1, or as much more as it
    /// takes for the film to be no longer than the song under it).
    pub speed: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FilmRequest {
    /// Where to write the film, as an .mp4. Defaults to a `films` folder beside the project file (for a song
    /// that has not been saved: the system temp folder, or the folder the assistant works in).
    pub path: Option<String>,
    /// "1080p" (1920x1080, the default), "2160p" (3840x2160), "upright" (1080x1920, for phones), or any
    /// "WIDTHxHEIGHT" with even sides from 320 to 3840.
    pub size: Option<String>,
    /// Frames a second, 24 to 60 (default 60).
    pub rate: Option<u32>,
    #[serde(default)]
    pub quality: FilmQuality,
    /// With "making_of": false for a silent film. Default true: the finished song plays under it.
    pub music: Option<bool>,
    #[serde(flatten)]
    pub what: FilmPlanRequest,
}
