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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTrackRequest {
    pub name: String,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAudioClipRequest {
    /// Track id.
    pub track: u64,
    /// Path of a WAV file.
    pub path: String,
    /// Position on the timeline in beats.
    pub start: f64,
    /// Length in beats. Defaults to the length of the file at the current tempo.
    pub length: Option<f64>,
    /// Where in the file to start, in seconds.
    pub offset_seconds: Option<f64>,
    /// Clip gain in dB.
    pub gain_db: Option<f32>,
    /// Fade-in length in beats.
    pub fade_in: Option<f64>,
    /// Fade-out length in beats.
    pub fade_out: Option<f64>,
    pub name: Option<String>,
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
    /// the automation.
    pub points: Vec<PointInput>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    Play,
    Stop,
    Seek,
    Status,
    /// Stop and silence everything immediately, including reverb and delay tails.
    Panic,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransportRequest {
    pub action: TransportAction,
    /// Beat to move the playhead to; required for `seek`, optional for `play` (plays from there).
    pub position: Option<f64>,
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NewProjectRequest {
    pub name: Option<String>,
}
