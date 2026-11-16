//! Tool arguments. Field docs become the JSON-schema descriptions the model
//! reads, so they are written for that audience.

use crate::input::{NoteInput, PitchInput, PointInput};
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
    /// Beats per minute, 20-999.
    pub tempo: Option<f64>,
    /// For example "4/4", "3/4", "6/8" or "7/8".
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
    /// Send level in dB (0 = as loud as the track itself, at most 6). null removes the send.
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
    /// Preset of the instrument, e.g. "warm_pad", "bass", "pluck", "keys" for the poly synth.
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddDeviceRequest {
    /// Track id. Omit to add the device to the master chain.
    pub track: Option<u64>,
    /// Device URI from `list_devices`.
    pub uri: String,
    /// Preset to start from.
    pub preset: Option<String>,
    /// Parameter values to set on top of the preset, by parameter id: numbers in the parameter's units,
    /// true/false for switches, or the choice name for enums (e.g. {"cutoff": 800, "filter_type": "lowpass24"}).
    pub params: Option<BTreeMap<String, Value>>,
    /// Position in the chain, 0 = first. Omit to append. Signal flows first to last; instruments go first.
    pub index: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDeviceRequest {
    /// Device id.
    pub device: u64,
    /// Only parameters whose id or name contains this text. Useful for plugins with hundreds of parameters.
    pub query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateDeviceRequest {
    /// Device id.
    pub device: u64,
    /// Preset to switch to. Resets every parameter first; `params` are applied afterwards.
    pub preset: Option<String>,
    /// Parameter values by parameter id: numbers in the parameter's units, true/false for switches, or the
    /// choice name for enums.
    pub params: Option<BTreeMap<String, Value>>,
    /// false bypasses the device.
    pub enabled: Option<bool>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TargetInput {
    #[serde(rename = "type")]
    pub kind: TargetKind,
    /// Track id, for track_volume and track_pan.
    pub track: Option<u64>,
    /// Device id, for device_param.
    pub device: Option<u64>,
    /// Parameter id, for device_param.
    pub param: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetAutomationRequest {
    pub target: TargetInput,
    /// The complete envelope; it replaces any existing one. Values move in a straight line between points
    /// unless a point says otherwise, and are held before the first and after the last. An empty list removes
    /// the automation. Omit to keep the envelope as it is (when only switching `enabled`).
    pub points: Option<Vec<PointInput>>,
    /// false switches the automation off without deleting it: the target then stays at the value it is set to
    /// (update_device, update_track). true switches it back on. Writing `points` switches it on unless this
    /// says otherwise.
    pub enabled: Option<bool>,
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
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RenderRequest {
    /// Output WAV path. Defaults to a `renders` folder beside the project file, or the system temp folder.
    pub path: Option<String>,
    /// First beat to render (default 0).
    pub start: Option<f64>,
    /// Beat to stop at (default: end of the last clip).
    pub end: Option<f64>,
    /// Longest reverb/delay tail to keep after the end, in seconds (default 10; trimmed when silent sooner).
    pub max_tail_seconds: Option<f64>,
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
    /// Also adopt the file's tempo and time signature. Defaults to true when the project has no tracks yet.
    pub use_tempo: Option<bool>,
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
    pub action: String,
    /// Where the stretch begins, in beats (bar N of 4/4 starts at (N-1)*4).
    pub start: f64,
    /// Where the stretch ends, in beats. Needed for "delete" and "duplicate".
    pub end: Option<f64>,
    /// How many beats to open up. Needed for "insert".
    pub length: Option<f64>,
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
    /// Look again for new, changed and deleted files in every folder.
    Rescan,
    /// Folders on this machine that probably hold samples (Splice, DAW packs, a Samples folder), to offer the user.
    Suggestions,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LibraryRequest {
    pub action: LibraryAction,
    /// The folder, for `add_folder` and `remove_folder`.
    pub path: Option<String>,
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
