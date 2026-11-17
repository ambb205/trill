//! The project model: a plain, serializable description of a song.
//!
//! All edits go through methods on [`Project`], which validate their input and
//! keep the model's invariants (sorted notes, live automation targets, unique
//! ids). The model knows nothing about audio; the engine compiles it into a
//! render graph.

use crate::controllers::{self, ControllerCurve, ControllerKind, ControllerLane, LaneClip};
use crate::ids::{ClipId, DeviceId, TrackId};
use crate::instrument::InstrumentProfile;
use crate::midi_file::{MidiFile, MidiTrack};
use crate::presets::PresetRef;
use crate::tempo::{MeterChange, MeterMap, SAME_BEAT, TempoChange, TempoMap};
use crate::time::{Beats, TimeSignature};
use crate::travel::Travel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIN_TEMPO: f64 = 20.0;
pub const MAX_TEMPO: f64 = 999.0;
pub const MIN_VOLUME_DB: f32 = -70.0;
pub const MAX_VOLUME_DB: f32 = 6.0;
/// The format of the project file this build writes, and the newest it
/// reads.
///
/// * 1: the first.
/// * 2: the reverb returns some 10 dB less and filters what goes into it,
///   and the ping-pong delay feeds back once a pair of repeats.
/// * 3: an automation lane says how its lines travel between points
///   ([`AutomationLane::travel`]), and a send can be automated. A file of
///   an earlier format says neither; its lanes are straight in their
///   units, as they were played when it was saved.
/// * 4: the sampler and the drum rack follow velocity by a law in decibels
///   that plays a hard hit louder than the volume, the saturator no longer
///   turns its level down as its drive rises, the lowest value of a level
///   is silence, and the poly synth's volume is 21 dB up.
/// * 5: the tempo and the time signature can change inside a song
///   ([`Project::tempo_changes`], [`Project::meter_changes`]). A file of
///   an earlier format has one of each, and nothing is written into it.
///
/// A device of an older file keeps the sound it had, and an envelope the
/// curve it played: see [`crate::formats`].
pub const FORMAT_VERSION: u32 = 5;
/// The furthest an audio clip can be transposed, in semitones either way.
pub const MAX_TRANSPOSE: f64 = 24.0;
/// Shortest loop a clip repeats, so a tiny loop length cannot explode into
/// millions of notes.
pub const MIN_LOOP_BEATS: Beats = 1.0 / 64.0;

const TRACK_COLORS: [u32; 10] =
    [0xF2994A, 0x56CCF2, 0xBB6BD9, 0x6FCF97, 0xEB5757, 0xF2C94C, 0x2D9CDB, 0x9B51E0, 0x27AE60, 0xE27AB0];

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("{0} not found")]
    TrackNotFound(TrackId),
    #[error("{0} not found")]
    ClipNotFound(ClipId),
    #[error("{0} not found")]
    DeviceNotFound(DeviceId),
    #[error("{0}")]
    Invalid(String),
}

fn invalid<T>(message: impl Into<String>) -> Result<T, EditError> {
    Err(EditError::Invalid(message.into()))
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// MIDI note number, 60 = C4 (middle C).
    pub pitch: u8,
    /// Offset from the start of the clip.
    pub start: Beats,
    pub length: Beats,
    /// 1..=127.
    pub velocity: u8,
}

impl Note {
    pub fn end(&self) -> Beats {
        self.start + self.length
    }

    fn validate(&self) -> Result<(), EditError> {
        if self.pitch > 127 {
            return invalid(format!("note pitch {} is outside 0..=127", self.pitch));
        }
        if !(1..=127).contains(&self.velocity) {
            return invalid(format!("note velocity {} is outside 1..=127", self.velocity));
        }
        if !self.start.is_finite() || self.start < 0.0 {
            return invalid(format!("note start {} must be >= 0", self.start));
        }
        if !self.length.is_finite() || self.length <= 0.0 {
            return invalid(format!("note length {} must be > 0", self.length));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MidiContent {
    /// Sorted by start, then pitch.
    pub notes: Vec<Note>,
    /// When set and shorter than the clip, the first `loop_length` beats of
    /// notes repeat to fill the clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_length: Option<Beats>,
    /// Where in the notes the clip starts playing, in beats. Trimming or
    /// splitting a clip moves this instead of deleting notes, so the edit
    /// can be dragged back. With a loop, playback wraps at `loop_length`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_offset: Beats,
    /// The movement of controllers (expression, the wheels, the pedals)
    /// through the content: at most one curve per controller, in the order
    /// of [`ControllerKind`]. They loop and are trimmed with the notes. See
    /// [`controllers`](crate::controllers) for what a controller is worth
    /// at any moment of the song.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controllers: Vec<ControllerCurve>,
}

impl MidiContent {
    /// The curve for `kind`, if the content has one.
    pub fn controller(&self, kind: ControllerKind) -> Option<&ControllerCurve> {
        self.controllers.iter().find(|curve| curve.controller == kind)
    }

    /// The length of the loop, if the content loops.
    pub fn period(&self) -> Option<Beats> {
        self.loop_length.filter(|length| *length > 0.0).map(|length| length.max(MIN_LOOP_BEATS))
    }

    /// Where in the content a clip is `position` beats after its start:
    /// the start offset applied and the loop wrapped round. Notes and
    /// controller points at that beat are what is playing then.
    pub fn content_beat(&self, position: Beats) -> Beats {
        let at = self.start_offset.max(0.0) + position;
        match self.period() {
            Some(period) => at.rem_euclid(period),
            None => at,
        }
    }
}

fn is_zero(value: &Beats) -> bool {
    *value == 0.0
}

/// How an audio clip relates to the project tempo.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarpMode {
    /// The recording plays at the speed and pitch it was made at, whatever
    /// the project tempo.
    #[default]
    Off,
    /// Follows the tempo by playing faster or slower, so the pitch moves
    /// with it, like a record on a turntable.
    Repitch,
    /// Follows the tempo and keeps the pitch, protecting transients: for
    /// drums and other rhythmic material.
    Beats,
    /// Follows the tempo and keeps the pitch, keeping sustained sound
    /// smooth: for chords, pads, bass, vocals and full mixes.
    Tonal,
}

impl WarpMode {
    fn is_off(&self) -> bool {
        *self == WarpMode::Off
    }

    /// Whether the mode changes speed without changing pitch, which is what
    /// makes transposing possible.
    pub fn keeps_pitch(self) -> bool {
        matches!(self, WarpMode::Beats | WarpMode::Tonal)
    }
}

/// The audio a clip plays and how it is fitted to the timeline.
///
/// # What a clip plays
///
/// A clip is a window on a recording. `offset_seconds` is where in the
/// recording the window opens: always a time in the file as it was
/// recorded, never affected by tempo or warping. From there on, one beat of
/// the timeline covers [`AudioContent::seconds_per_beat`] of the recording:
///
/// * A clip that **follows the tempo** (its `warp` is not `Off` and its
///   `source_tempo` is known) covers `60 / source_tempo` seconds per beat.
///   N beats of the recording occupy N beats of the timeline at any project
///   tempo, so a bar of a loop stays a bar.
/// * Any other clip plays at the speed it was recorded at and covers
///   `60 / project tempo` seconds per beat: how much of the recording fits
///   under the clip changes with the project tempo, and where the tempo
///   changes under the clip, with the tempo each beat of it has.
///
/// Everything else follows from that one rule
/// ([`AudioContent::seconds_over`]). Trimming the left edge by `b` beats
/// moves `offset_seconds` by the seconds those beats cover, and
/// splitting gives the second half the offset the first half would have
/// reached, so both keep playing exactly what they played. Fades are
/// lengths on the timeline, in beats, applied to what comes out: warping a
/// clip does not move them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioContent {
    /// Path of the audio file on disk.
    pub path: String,
    /// Where in the file playback starts, in seconds of the recording.
    #[serde(default)]
    pub offset_seconds: f64,
    #[serde(default)]
    pub gain_db: f32,
    /// Length of the fade at the start of the clip, in beats.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in: Beats,
    /// Length of the fade at the end of the clip, in beats.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out: Beats,
    /// Whether and how the clip follows the project tempo.
    #[serde(default, skip_serializing_if = "WarpMode::is_off")]
    pub warp: WarpMode,
    /// The tempo the recording was made at, in beats per minute, if known.
    /// A warped clip without one has nothing to follow the tempo with and
    /// plays at its recorded speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tempo: Option<f64>,
    /// Pitch shift in semitones, without changing the speed. Applies in the
    /// warp modes that keep pitch ([`WarpMode::keeps_pitch`]); the others
    /// tie pitch to speed and ignore it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub transpose: f64,
}

impl AudioContent {
    /// The whole file at unity gain, unwarped, without fades.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into(), ..Self::default() }
    }

    /// The recording's tempo, if the clip follows the project tempo with it.
    pub fn followed_tempo(&self) -> Option<f64> {
        self.source_tempo.filter(|_| self.warp != WarpMode::Off)
    }

    /// Seconds of the recording that one beat of the timeline covers at
    /// `project_tempo`. See the type's documentation.
    pub fn seconds_per_beat(&self, project_tempo: f64) -> f64 {
        60.0 / self.followed_tempo().unwrap_or(project_tempo)
    }

    /// How much longer the clip's audio lasts than the stretch of recording
    /// it plays: 1 for a clip at its recorded speed, and
    /// `source_tempo / project_tempo` for one that follows the tempo.
    pub fn time_ratio(&self, project_tempo: f64) -> f64 {
        self.followed_tempo().map_or(1.0, |source| source / project_tempo)
    }

    /// Seconds of the recording that `length` beats of the timeline cover
    /// from `from` on, in a song whose tempo is `map`: negative for a
    /// length that goes back. See the type's documentation.
    pub fn seconds_over(&self, map: &TempoMap, from: Beats, length: Beats) -> f64 {
        match self.followed_tempo() {
            Some(source) => length * (60.0 / source),
            None => map.seconds_over(from, length),
        }
    }

    /// How many beats of the timeline `seconds` of the recording cover
    /// from `from` on: [`seconds_over`](Self::seconds_over) the other way
    /// round.
    pub fn beats_over(&self, map: &TempoMap, from: Beats, seconds: f64) -> Beats {
        match self.followed_tempo() {
            Some(source) => seconds / (60.0 / source),
            None if map.is_constant() => seconds / (60.0 / map.tempo_at(0.0)),
            None => map.beat_at_seconds(map.seconds_at(from) + seconds) - from,
        }
    }

    /// How fast the clip's audio runs against the recording at `beat` of
    /// the song: [`time_ratio`](Self::time_ratio) at the tempo there.
    pub fn time_ratio_at(&self, map: &TempoMap, beat: Beats) -> f64 {
        self.followed_tempo().map_or(1.0, |source| source / map.tempo_at(beat))
    }

    /// The pitch shift that is actually heard, in semitones: `transpose`
    /// in the modes that can shift pitch on its own, zero in the others.
    pub fn pitch_shift(&self) -> f64 {
        if self.warp.keeps_pitch() { self.transpose } else { 0.0 }
    }

    fn validate(&self) -> Result<(), EditError> {
        if !self.offset_seconds.is_finite() || self.offset_seconds < 0.0 {
            return invalid("audio offset must be >= 0 seconds");
        }
        if let Some(tempo) = self.source_tempo
            && (!tempo.is_finite() || !(MIN_TEMPO..=MAX_TEMPO).contains(&tempo))
        {
            return invalid(format!("source tempo {tempo} is outside {MIN_TEMPO}..={MAX_TEMPO} BPM"));
        }
        if !self.transpose.is_finite() || self.transpose.abs() > MAX_TRANSPOSE {
            return invalid(format!(
                "transpose {} is outside -{MAX_TRANSPOSE}..={MAX_TRANSPOSE} semitones",
                self.transpose
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipContent {
    Midi(MidiContent),
    Audio(AudioContent),
}

impl ClipContent {
    /// Moves the point where playback starts `beats` later in the content
    /// (earlier when negative), as trimming a clip's left edge does. `at` is
    /// where the clip starts in the song, whose tempo is `map`.
    fn advance(&mut self, beats: Beats, at: Beats, map: &TempoMap) {
        match self {
            ClipContent::Midi(midi) => {
                let target = midi.start_offset + beats;
                midi.start_offset = match midi.loop_length.filter(|length| *length > 0.0) {
                    Some(length) => target.rem_euclid(length),
                    None => target,
                };
            }
            ClipContent::Audio(audio) => {
                audio.offset_seconds = (audio.offset_seconds + audio.seconds_over(map, at, beats)).max(0.0);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    /// Position on the timeline.
    pub start: Beats,
    pub length: Beats,
    #[serde(default)]
    pub muted: bool,
    pub content: ClipContent,
}

impl Clip {
    pub fn end(&self) -> Beats {
        self.start + self.length
    }

    pub fn midi(&self) -> Option<&MidiContent> {
        match &self.content {
            ClipContent::Midi(midi) => Some(midi),
            ClipContent::Audio(_) => None,
        }
    }

    pub fn audio(&self) -> Option<&AudioContent> {
        match &self.content {
            ClipContent::Audio(audio) => Some(audio),
            ClipContent::Midi(_) => None,
        }
    }

    pub fn notes(&self) -> &[Note] {
        self.midi().map_or(&[], |midi| midi.notes.as_slice())
    }

    /// The notes this clip actually plays, at their positions on the
    /// timeline: the start offset applied, the loop repeated to fill the
    /// clip, and anything hanging past the clip or loop end cut off. Sorted
    /// by start. This is the one definition of what a MIDI clip sounds
    /// like; the engine, the exporters and the views all use it.
    pub fn timeline_notes(&self) -> Vec<Note> {
        let Some(midi) = self.midi() else { return Vec::new() };
        let mut out = Vec::new();
        let mut push = |start: Beats, end: Beats, note: &Note| {
            out.push(Note { start: self.start + start, length: end - start, ..*note });
        };
        let offset = midi.start_offset.max(0.0);

        let Some(period) = midi.loop_length.filter(|length| *length > 0.0) else {
            // No loop: the notes from `offset` on, once.
            for note in &midi.notes {
                let start = note.start - offset;
                if start >= self.length {
                    break;
                }
                // A note that began before the clip's start is not sounded late.
                if start >= 0.0 {
                    push(start, (start + note.length).min(self.length), note);
                }
            }
            return out;
        };

        // Looped: the first `period` beats of notes repeat, entered `offset`
        // beats in. A floor on the period bounds the number of repeats.
        let period = period.max(MIN_LOOP_BEATS);
        let mut repeat_start = -offset.rem_euclid(period);
        while repeat_start < self.length {
            let repeat_end = (repeat_start + period).min(self.length);
            for note in &midi.notes {
                if note.start >= period {
                    break;
                }
                let start = repeat_start + note.start;
                if start >= repeat_end {
                    break;
                }
                if start >= 0.0 {
                    push(start, (start + note.length).min(repeat_end), note);
                }
            }
            repeat_start += period;
        }
        out
    }

    fn midi_mut(&mut self) -> Result<&mut MidiContent, EditError> {
        let id = self.id;
        match &mut self.content {
            ClipContent::Midi(midi) => Ok(midi),
            ClipContent::Audio(_) => invalid(format!("{id} is an audio clip and has no notes")),
        }
    }

    /// The controller curves the clip holds, in the order of
    /// [`ControllerKind`]. Empty for an audio clip.
    pub fn controllers(&self) -> &[ControllerCurve] {
        self.midi().map_or(&[], |midi| midi.controllers.as_slice())
    }

    /// The clip's curve for `kind`, if it has one.
    pub fn controller(&self, kind: ControllerKind) -> Option<&ControllerCurve> {
        self.midi().and_then(|midi| midi.controller(kind))
    }

    /// Replaces the clip's curve for `kind` with `points`, whose beats are
    /// measured from where the content begins, like the starts of notes,
    /// and whose values lie within [`ControllerKind::range`]. No points
    /// removes the curve. The points need not be in order.
    pub fn set_controller(&mut self, kind: ControllerKind, points: Vec<AutomationPoint>) -> Result<(), EditError> {
        let id = self.id;
        let points = controllers::validated(kind, points)?;
        let ClipContent::Midi(midi) = &mut self.content else {
            return invalid(format!("{id} is an audio clip and has no controllers"));
        };
        midi.controllers.retain(|curve| curve.controller != kind);
        if !points.is_empty() {
            let index = midi.controllers.partition_point(|curve| curve.controller < kind);
            midi.controllers.insert(index, ControllerCurve { controller: kind, points });
        }
        Ok(())
    }

    /// Where in the clip's content the timeline position `beat` falls, if
    /// the clip is a MIDI clip and lies there.
    pub fn content_beat(&self, beat: Beats) -> Option<Beats> {
        let midi = self.midi()?;
        (beat >= self.start && beat < self.end()).then(|| midi.content_beat(beat - self.start))
    }

    /// What the clip's curve for `kind` is worth at the timeline position
    /// `beat`; `None` where the clip does not lie or has no such curve.
    pub fn controller_value_at(&self, kind: ControllerKind, beat: Beats) -> Option<f64> {
        let content = self.content_beat(beat)?;
        self.controller(kind).map(|curve| curve.value_at(content))
    }

    /// Adds notes, keeping them sorted.
    pub fn add_notes(&mut self, notes: &[Note]) -> Result<(), EditError> {
        for note in notes {
            note.validate()?;
        }
        let midi = self.midi_mut()?;
        midi.notes.extend_from_slice(notes);
        sort_notes(&mut midi.notes);
        Ok(())
    }

    /// Replaces every note in the clip.
    pub fn set_notes(&mut self, notes: Vec<Note>) -> Result<(), EditError> {
        for note in &notes {
            note.validate()?;
        }
        let midi = self.midi_mut()?;
        midi.notes = notes;
        sort_notes(&mut midi.notes);
        Ok(())
    }

    /// Removes the notes `filter` matches and returns how many were removed.
    pub fn remove_notes(&mut self, filter: impl Fn(&Note) -> bool) -> Result<usize, EditError> {
        let midi = self.midi_mut()?;
        let before = midi.notes.len();
        midi.notes.retain(|note| !filter(note));
        Ok(before - midi.notes.len())
    }

    pub fn set_loop_length(&mut self, loop_length: Option<Beats>) -> Result<(), EditError> {
        if let Some(len) = loop_length
            && (!len.is_finite() || len <= 0.0)
        {
            return invalid(format!("loop length {len} must be > 0"));
        }
        self.midi_mut()?.loop_length = loop_length;
        Ok(())
    }
}

fn sort_notes(notes: &mut [Note]) {
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
}

/// An instrument or effect in a device chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    /// What to instantiate: `builtin:polysynth`, `clap:<plugin id>`, ...
    pub uri: String,
    pub name: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Parameter values in plain units, keyed by parameter id. Parameters
    /// that are absent use the device's default.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// Opaque plugin state, captured when the project is saved.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "state_base64")]
    pub state: Option<Vec<u8>>,
    /// The preset the device was last given: what its sound started from,
    /// whatever has been changed about it since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetRef>,
    /// The track the device listens to in place of its own input: the key
    /// of a compressor, such as the kick a bass is ducked to. It is that
    /// track as it leaves its devices, ahead of its fader and its mute, so
    /// a track that is turned down or muted still serves as a key. Only a
    /// device that takes a key does anything with it. See
    /// [`Project::set_sidechain`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<TrackId>,
}

fn enabled_default() -> bool {
    true
}

pub(crate) mod state_base64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(state: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match state {
            Some(bytes) => s.serialize_some(&STANDARD.encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let encoded: Option<String> = Option::deserialize(d)?;
        encoded.map(|text| STANDARD.decode(text).map_err(serde::de::Error::custom)).transpose()
    }
}

/// What a track is for.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Holds clips and plays them through its devices.
    #[default]
    Normal,
    /// Mixes the tracks that belong to it, so they can be processed and
    /// faded together.
    Group,
    /// Receives what other tracks send to it: a shared reverb or delay.
    Return,
}

impl TrackKind {
    fn is_normal(&self) -> bool {
        *self == TrackKind::Normal
    }
}

/// A copy of a track's signal, taken after its fader, routed to a return
/// track.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub to: TrackId,
    pub level_db: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    #[serde(default, skip_serializing_if = "TrackKind::is_normal")]
    pub kind: TrackKind,
    /// The group track this track plays through instead of the master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<TrackId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<Send>,
    /// 0xRRGGBB.
    pub color: u32,
    pub volume_db: f32,
    /// -1 (left) ..= 1 (right).
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// Signal chain. MIDI clips drive the first instrument; audio clips are
    /// mixed in ahead of the chain.
    pub devices: Vec<Device>,
    pub clips: Vec<Clip>,
    /// What the track's notes mean to its instrument: drums or pitches,
    /// which keys are drums or keyswitches, its range, the octave it
    /// sounds at and how far it bends. Empty for an ordinary instrument,
    /// and then left out of the file.
    #[serde(default, skip_serializing_if = "InstrumentProfile::is_empty")]
    pub profile: InstrumentProfile,
}

impl Track {
    /// What each controller that a clip of this track moves is worth over
    /// the whole song, one lane per controller in the order of
    /// [`ControllerKind`]. Muted clips take no part. This is the one
    /// definition of it (see [`controllers`](crate::controllers)): the
    /// engine plays these lanes and the exporters write them.
    pub fn controller_lanes(&self) -> Vec<ControllerLane> {
        let playing = || self.clips.iter().filter(|clip| !clip.muted).filter_map(|clip| Some((clip, clip.midi()?)));
        let mut kinds: Vec<ControllerKind> =
            playing().flat_map(|(_, midi)| midi.controllers.iter().map(|curve| curve.controller)).collect();
        kinds.sort();
        kinds.dedup();
        kinds
            .into_iter()
            .map(|kind| {
                let clips: Vec<LaneClip<'_>> = playing()
                    .filter_map(|(clip, midi)| {
                        Some(LaneClip {
                            start: clip.start,
                            end: clip.end(),
                            offset: midi.start_offset.max(0.0),
                            period: midi.period(),
                            curve: midi.controller(kind)?,
                        })
                    })
                    .collect();
                controllers::lane_of(kind, &clips)
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Master {
    pub volume_db: f32,
    pub devices: Vec<Device>,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRegion {
    pub enabled: bool,
    pub start: Beats,
    pub end: Beats,
}

/// Which device chain an operation targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainRef {
    Track(TrackId),
    Master,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationTarget {
    /// In dB.
    TrackVolume { track: TrackId },
    /// -1..=1.
    TrackPan { track: TrackId },
    /// In the parameter's plain units.
    DeviceParam { device: DeviceId, param: String },
    /// In dB.
    MasterVolume,
    /// How much of `track` goes to the return track `to`, in dB after the
    /// track's fader, as [`Send::level_db`] is. At [`MIN_VOLUME_DB`] or
    /// under, nothing is sent. The envelope is the send: the track need not
    /// have one set by hand.
    Send { track: TrackId, to: TrackId },
}

impl AutomationTarget {
    /// How the control travels, where the model knows it by itself: the
    /// mixer's faders and sends, and the pan. `None` for a device's
    /// parameter: what it is known only to its device
    /// ([`Travel::of_param`]).
    pub fn travel(&self) -> Option<Travel> {
        match self {
            AutomationTarget::TrackVolume { .. } | AutomationTarget::MasterVolume | AutomationTarget::Send { .. } => {
                Some(Travel::FADER)
            }
            AutomationTarget::TrackPan { .. } => Some(Travel::Linear),
            AutomationTarget::DeviceParam { .. } => None,
        }
    }
}

/// How an envelope travels from one point to the next.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Segment {
    #[default]
    Linear,
    /// Stay at this point's value, then jump at the next point.
    Hold,
    /// Bend the line: -1..=1, where positive values start slowly and finish
    /// fast, and negative values the reverse.
    Curve(f32),
}

impl Segment {
    fn is_linear(&self) -> bool {
        *self == Segment::Linear
    }

    /// Maps progress through the segment (0..=1) to progress in value.
    fn shape(self, t: f64) -> f64 {
        match self {
            Segment::Linear => t,
            Segment::Hold => 0.0,
            Segment::Curve(bend) => {
                let k = 6.0 * (bend as f64).clamp(-1.0, 1.0);
                if k.abs() < 1e-6 { t } else { ((k * t).exp() - 1.0) / (k.exp() - 1.0) }
            }
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub beat: Beats,
    pub value: f64,
    /// The shape of the segment from this point to the next.
    #[serde(default, skip_serializing_if = "Segment::is_linear")]
    pub shape: Segment,
}

impl AutomationPoint {
    /// A point joined to the next by a straight line.
    pub fn new(beat: Beats, value: f64) -> Self {
        Self { beat, value, shape: Segment::Linear }
    }
}

/// A breakpoint envelope. Values follow each point's segment shape between
/// points and are held before the first and after the last.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    /// Sorted by beat; never empty.
    pub points: Vec<AutomationPoint>,
    /// What a line between two points is straight along: the travel of the
    /// control it moves (see [`travel`](crate::travel)), which a lane takes
    /// when it is written. It is kept with the lane, so that a file plays
    /// the same whatever comes to be known about its devices, and
    /// [`Travel::Linear`], straight in the units, is not written at all:
    /// that is how every lane travelled before this was kept, and how a
    /// file from then still plays.
    #[serde(default, skip_serializing_if = "Travel::is_linear")]
    pub travel: Travel,
    /// Off, the lane keeps its points and drives nothing: its target stays
    /// where it is set by hand. This is what lets someone take hold of an
    /// automated control without throwing the envelope away.
    #[serde(default = "on", skip_serializing_if = "is_on")]
    pub enabled: bool,
}

fn on() -> bool {
    true
}

fn is_on(enabled: &bool) -> bool {
    *enabled
}

impl AutomationLane {
    pub fn value_at(&self, beat: Beats) -> f64 {
        envelope_value(&self.points, self.travel, beat)
    }
}

/// Evaluates a breakpoint envelope whose lines are straight in its units:
/// a controller's curve, or a lane of [`Travel::Linear`]. `points` must be
/// sorted and non-empty.
pub fn automation_value(points: &[AutomationPoint], beat: Beats) -> f64 {
    envelope_value(points, Travel::Linear, beat)
}

/// Evaluates a breakpoint envelope whose lines are straight along `travel`.
/// `points` must be sorted and non-empty. However many there are, this is
/// one search among them: it neither allocates nor waits, so the audio
/// thread may.
pub fn envelope_value(points: &[AutomationPoint], travel: Travel, beat: Beats) -> f64 {
    let next = points.partition_point(|p| p.beat <= beat);
    if next == 0 {
        return points[0].value;
    }
    if next == points.len() {
        return points[next - 1].value;
    }
    let (a, b) = (points[next - 1], points[next]);
    let span = b.beat - a.beat;
    if span <= 0.0 {
        return b.value;
    }
    travel.between(a.value, b.value, a.shape.shape((beat - a.beat) / span))
}

/// A track and clip created by [`Project::import_midi`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ImportedPart {
    pub track: TrackId,
    pub clip: ClipId,
    /// Whether the part was on the General MIDI drum channel.
    pub drums: bool,
}

/// A named position on the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub beat: Beats,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "format_version_default")]
    pub format_version: u32,
    pub name: String,
    /// The tempo the song begins at, in beats per minute.
    pub tempo: f64,
    /// The time signature the song begins in.
    pub time_signature: TimeSignature,
    /// Where the tempo changes after the start: in order of their beats,
    /// and at one beat at most a glide that arrives there followed by a
    /// tempo that takes over. Read through [`Project::tempo_map`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tempo_changes: Vec<TempoChange>,
    /// Where the time signature changes after the start, in order of
    /// their beats. Read through [`Project::meter_map`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meter_changes: Vec<MeterChange>,
    pub loop_region: LoopRegion,
    pub tracks: Vec<Track>,
    pub master: Master,
    #[serde(default)]
    pub automation: Vec<AutomationLane>,
    /// Sorted by beat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    pub(crate) next_id: u64,
}

fn format_version_default() -> u32 {
    FORMAT_VERSION
}

fn valid_tempo(tempo: f64) -> Result<f64, EditError> {
    if !tempo.is_finite() || !(MIN_TEMPO..=MAX_TEMPO).contains(&tempo) {
        return invalid(format!("tempo {tempo} is outside {MIN_TEMPO}..={MAX_TEMPO} BPM"));
    }
    Ok(tempo)
}

fn valid_signature(numerator: u8, denominator: u8) -> Result<TimeSignature, EditError> {
    match TimeSignature::new(numerator, denominator) {
        Some(signature) => Ok(signature),
        None => invalid(format!("unsupported time signature {numerator}/{denominator}: 1 to 32 over 1, 2, 4, 8 or 16")),
    }
}

/// Where in the song `what` is set, or why that is nowhere.
fn valid_position(beat: Beats, what: &str) -> Result<Beats, EditError> {
    if !beat.is_finite() || beat < 0.0 {
        return invalid(format!("{what} is set at a position of the song, 0 beats or later, not at {beat}"));
    }
    Ok(beat)
}

/// In order of their beats, and at one beat the glide that arrives before
/// the tempo that takes over.
fn sort_tempo_changes(changes: &mut [TempoChange]) {
    changes.sort_by(|a, b| a.beat.total_cmp(&b.beat).then(b.ramp.cmp(&a.ramp)));
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: name.into(),
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            tempo_changes: Vec::new(),
            meter_changes: Vec::new(),
            loop_region: LoopRegion { enabled: false, start: 0.0, end: 16.0 },
            tracks: Vec::new(),
            master: Master { volume_db: 0.0, devices: Vec::new() },
            automation: Vec::new(),
            markers: Vec::new(),
            next_id: 1,
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // ---- song settings -------------------------------------------------

    /// Sets the tempo the song begins at. Changes further on
    /// ([`Project::set_tempo_at`]) stay as they are.
    pub fn set_tempo(&mut self, tempo: f64) -> Result<(), EditError> {
        self.tempo = valid_tempo(tempo)?;
        Ok(())
    }

    /// Sets the time signature the song begins in. Changes further on
    /// ([`Project::set_time_signature_at`]) stay as they are.
    pub fn set_time_signature(&mut self, numerator: u8, denominator: u8) -> Result<(), EditError> {
        self.time_signature = valid_signature(numerator, denominator)?;
        Ok(())
    }

    /// How beats become time over the song: its first tempo and every
    /// change after it. Whatever turns a beat into seconds or frames asks
    /// this, and nothing reads `tempo` for it.
    pub fn tempo_map(&self) -> TempoMap {
        TempoMap::new(self.tempo, &self.tempo_changes)
    }

    /// How beats become bars over the song: its first time signature and
    /// every change after it. Whatever counts bars asks this.
    pub fn meter_map(&self) -> MeterMap {
        MeterMap::new(self.time_signature, &self.meter_changes)
    }

    /// Sets the tempo at `beat`. Nothing in the song moves: the beats
    /// from there on last as long as the new tempo says.
    ///
    /// Without `ramp` the tempo is `tempo` from `beat` on, and jumps there.
    /// With it the tempo glides to `tempo` from the change before (or from
    /// the start of the song), evenly from beat to beat, and arrives at
    /// `beat`: an accelerando or a ritardando.
    ///
    /// One beat can hold both, a glide that arrives and a tempo that takes
    /// over (a ritardando, and then the old tempo again): setting one
    /// leaves the other. At the start of the song there is only the tempo
    /// it begins at.
    pub fn set_tempo_at(&mut self, beat: Beats, tempo: f64, ramp: bool) -> Result<(), EditError> {
        let tempo = valid_tempo(tempo)?;
        let beat = valid_position(beat, "a tempo")?;
        if beat <= SAME_BEAT {
            if ramp {
                return invalid("a tempo that glides has to arrive somewhere after the start of the song");
            }
            self.tempo = tempo;
            return Ok(());
        }
        self.tempo_changes.retain(|change| (change.beat - beat).abs() > SAME_BEAT || change.ramp != ramp);
        self.tempo_changes.push(TempoChange { beat, tempo, ramp });
        sort_tempo_changes(&mut self.tempo_changes);
        Ok(())
    }

    /// Removes what the tempo does at `beat`: the glide that arrives there
    /// and the tempo that takes over there, whichever there is. The tempo
    /// before goes on.
    pub fn remove_tempo_at(&mut self, beat: Beats) -> Result<(), EditError> {
        let before = self.tempo_changes.len();
        self.tempo_changes.retain(|change| (change.beat - beat).abs() > SAME_BEAT);
        if self.tempo_changes.len() == before {
            let at: Vec<String> = self.tempo_changes.iter().map(|change| change.beat.to_string()).collect();
            return invalid(match at.is_empty() {
                true => format!("the tempo does not change at beat {beat}: it does not change anywhere in the song"),
                false => format!("the tempo does not change at beat {beat}; it changes at beats {}", at.join(", ")),
            });
        }
        Ok(())
    }

    /// Moves what the tempo does at `from` (the glide that arrives and the
    /// tempo that takes over, whichever there is) to `to`, in place of
    /// what the tempo did there.
    pub fn move_tempo_at(&mut self, from: Beats, to: Beats) -> Result<(), EditError> {
        if valid_position(to, "a tempo")? <= SAME_BEAT {
            return invalid("a change of tempo lies after the start of the song, which has the tempo it begins at");
        }
        let at = |change: &TempoChange, beat: Beats| (change.beat - beat).abs() <= SAME_BEAT;
        let moved: Vec<TempoChange> = self
            .tempo_changes
            .iter()
            .filter(|change| at(change, from))
            .map(|change| TempoChange { beat: to, ..*change })
            .collect();
        if moved.is_empty() {
            // Nothing is there: said as removing it would say it.
            return self.remove_tempo_at(from);
        }
        self.tempo_changes.retain(|change| !at(change, from) && !at(change, to));
        self.tempo_changes.extend(moved);
        sort_tempo_changes(&mut self.tempo_changes);
        Ok(())
    }

    /// Replaces every change of tempo after the start of the song.
    pub fn set_tempo_changes(&mut self, mut changes: Vec<TempoChange>) -> Result<(), EditError> {
        for change in &changes {
            valid_tempo(change.tempo)?;
            if valid_position(change.beat, "a tempo")? <= SAME_BEAT {
                return invalid("a change of tempo lies after the start of the song, which has the tempo it begins at");
            }
        }
        sort_tempo_changes(&mut changes);
        let twice = |pair: &&[TempoChange]| pair[1].beat - pair[0].beat <= SAME_BEAT && (!pair[0].ramp || pair[1].ramp);
        if let Some(pair) = changes.windows(2).find(twice) {
            return invalid(format!(
                "at beat {} the tempo is set twice: one beat holds at most a glide that arrives and a tempo that \
                 takes over",
                pair[0].beat
            ));
        }
        self.tempo_changes = changes;
        Ok(())
    }

    /// Sets the time signature from `beat` on. A bar begins there: where
    /// that is not a bar line, the bar before is cut short. Nothing in the
    /// song moves, and changes further on stay at their beats, so the bars
    /// between here and the next change are counted anew
    /// ([`MeterMap::cut_bars`] says which ones no longer come out whole).
    pub fn set_time_signature_at(&mut self, beat: Beats, numerator: u8, denominator: u8) -> Result<(), EditError> {
        let signature = valid_signature(numerator, denominator)?;
        let beat = valid_position(beat, "a time signature")?;
        if beat <= SAME_BEAT {
            self.time_signature = signature;
            return Ok(());
        }
        self.meter_changes.retain(|change| (change.beat - beat).abs() > SAME_BEAT);
        self.meter_changes.push(MeterChange { beat, signature });
        self.meter_changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Ok(())
    }

    /// Removes the change of time signature at `beat`: the signature
    /// before goes on, in its own bars.
    pub fn remove_time_signature_at(&mut self, beat: Beats) -> Result<(), EditError> {
        let before = self.meter_changes.len();
        self.meter_changes.retain(|change| (change.beat - beat).abs() > SAME_BEAT);
        if self.meter_changes.len() == before {
            let at: Vec<String> = self.meter_changes.iter().map(|change| change.beat.to_string()).collect();
            return invalid(match at.is_empty() {
                true => {
                    format!("the time signature does not change at beat {beat}: it is the same all through the song")
                }
                false => {
                    format!("the time signature does not change at beat {beat}; it changes at beats {}", at.join(", "))
                }
            });
        }
        Ok(())
    }

    /// Moves the change of time signature at `from` to `to`, in place of
    /// one that was there.
    pub fn move_time_signature_at(&mut self, from: Beats, to: Beats) -> Result<(), EditError> {
        if valid_position(to, "a time signature")? <= SAME_BEAT {
            return invalid("a change of time signature lies after the start of the song, which has its own");
        }
        let at = |change: &MeterChange, beat: Beats| (change.beat - beat).abs() <= SAME_BEAT;
        let Some(moved) = self.meter_changes.iter().find(|change| at(change, from)).copied() else {
            return self.remove_time_signature_at(from);
        };
        self.meter_changes.retain(|change| !at(change, from) && !at(change, to));
        self.meter_changes.push(MeterChange { beat: to, ..moved });
        self.meter_changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Ok(())
    }

    /// Replaces every change of time signature after the start of the
    /// song.
    pub fn set_meter_changes(&mut self, mut changes: Vec<MeterChange>) -> Result<(), EditError> {
        for change in &changes {
            valid_signature(change.signature.numerator, change.signature.denominator)?;
            if valid_position(change.beat, "a time signature")? <= SAME_BEAT {
                return invalid("a change of time signature lies after the start of the song, which has its own");
            }
        }
        changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        if let Some(pair) = changes.windows(2).find(|pair| pair[1].beat - pair[0].beat <= SAME_BEAT) {
            return invalid(format!("at beat {} the time signature is set twice", pair[0].beat));
        }
        self.meter_changes = changes;
        Ok(())
    }

    pub fn set_loop(&mut self, enabled: bool, start: Beats, end: Beats) -> Result<(), EditError> {
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            return invalid(format!("loop region {start}..{end} must satisfy 0 <= start < end"));
        }
        self.loop_region = LoopRegion { enabled, start, end };
        Ok(())
    }

    /// End of the last clip, in beats.
    pub fn length_beats(&self) -> Beats {
        self.tracks.iter().flat_map(|track| track.clips.iter()).map(Clip::end).fold(0.0, f64::max)
    }

    // ---- tracks --------------------------------------------------------

    pub fn add_track(&mut self, name: impl Into<String>) -> TrackId {
        self.add_track_of_kind(name, TrackKind::Normal)
    }

    /// Adds a normal, group or return track at the end of the list.
    pub fn add_track_of_kind(&mut self, name: impl Into<String>, kind: TrackKind) -> TrackId {
        let id = TrackId(self.alloc_id());
        let color = TRACK_COLORS[self.tracks.len() % TRACK_COLORS.len()];
        self.tracks.push(Track {
            id,
            name: name.into(),
            kind,
            group: None,
            sends: Vec::new(),
            color,
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            devices: Vec::new(),
            clips: Vec::new(),
            profile: InstrumentProfile::default(),
        });
        id
    }

    pub fn remove_track(&mut self, id: TrackId) -> Result<Track, EditError> {
        let index = self.track_index(id)?;
        let track = self.tracks.remove(index);
        // Nothing may keep routing to a track that is gone.
        for other in &mut self.tracks {
            if other.group == Some(id) {
                other.group = None;
            }
            other.sends.retain(|send| send.to != id);
        }
        for device in self.devices_mut() {
            if device.sidechain == Some(id) {
                device.sidechain = None;
            }
        }
        self.prune_automation();
        Ok(track)
    }

    /// Routes a track through a group track, or back to the master with
    /// `None`. Only normal tracks can be grouped.
    pub fn set_track_group(&mut self, id: TrackId, group: Option<TrackId>) -> Result<(), EditError> {
        if self.track(id)?.kind != TrackKind::Normal {
            return invalid(format!("{id} is a group or return track and cannot be put in a group"));
        }
        if let Some(group) = group
            && self.track(group)?.kind != TrackKind::Group
        {
            return invalid(format!("{group} is not a group track"));
        }
        let before = std::mem::replace(&mut self.track_mut(id)?.group, group);
        if let Some(group) = group
            && let Err(error) = self.no_loop(format!("putting {id} in {group}"))
        {
            self.track_mut(id)?.group = before;
            return Err(error);
        }
        Ok(())
    }

    /// Whether `id` may send to `to`: any track but a return, to a return.
    fn can_send(&self, id: TrackId, to: TrackId) -> Result<(), EditError> {
        if self.track(id)?.kind == TrackKind::Return {
            return invalid(format!("{id} is a return track; returns cannot send to returns"));
        }
        if self.track(to)?.kind != TrackKind::Return {
            return invalid(format!("{to} is not a return track"));
        }
        Ok(())
    }

    /// Sets how much of a track is sent to a return track, in dB after the
    /// fader; `None` removes the send. (An envelope may move it over time:
    /// [`AutomationTarget::Send`].)
    pub fn set_send(&mut self, id: TrackId, to: TrackId, level_db: Option<f32>) -> Result<(), EditError> {
        self.can_send(id, to)?;
        let level_db = level_db.map(validate_volume).transpose()?;
        let before = self.track(id)?.sends.clone();
        let sends = &mut self.track_mut(id)?.sends;
        sends.retain(|send| send.to != to);
        if let Some(level_db) = level_db {
            sends.push(Send { to, level_db });
            if let Err(error) = self.no_loop(format!("sending {id} to {to}")) {
                self.track_mut(id)?.sends = before;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Only normal tracks hold clips.
    fn holds_clips(&self, id: TrackId) -> Result<(), EditError> {
        match self.track(id)?.kind {
            TrackKind::Normal => Ok(()),
            TrackKind::Group => invalid(format!("{id} is a group track and does not hold clips")),
            TrackKind::Return => invalid(format!("{id} is a return track and does not hold clips")),
        }
    }

    pub fn move_track(&mut self, id: TrackId, new_index: usize) -> Result<(), EditError> {
        let index = self.track_index(id)?;
        let track = self.tracks.remove(index);
        self.tracks.insert(new_index.min(self.tracks.len()), track);
        Ok(())
    }

    /// Copies a track, with its clips, devices, sends and automation, and
    /// puts the copy right below it. Everything in the copy gets a new id.
    pub fn duplicate_track(&mut self, id: TrackId) -> Result<TrackId, EditError> {
        let index = self.track_index(id)?;
        let mut copy = self.tracks[index].clone();
        copy.id = TrackId(self.alloc_id());
        copy.name = format!("{} copy", copy.name);
        copy.solo = false;
        for clip in &mut copy.clips {
            clip.id = ClipId(self.alloc_id());
        }
        // Old device id to new, so the copy's automation follows its own devices.
        let mut devices = Vec::with_capacity(copy.devices.len());
        for device in &mut copy.devices {
            let new = DeviceId(self.alloc_id());
            devices.push((device.id, new));
            device.id = new;
        }
        let lanes: Vec<AutomationLane> = self
            .automation
            .iter()
            .filter_map(|lane| {
                let target = match &lane.target {
                    AutomationTarget::TrackVolume { track } if *track == id => {
                        AutomationTarget::TrackVolume { track: copy.id }
                    }
                    AutomationTarget::TrackPan { track } if *track == id => {
                        AutomationTarget::TrackPan { track: copy.id }
                    }
                    AutomationTarget::DeviceParam { device, param } => {
                        let (_, new) = devices.iter().find(|(old, _)| old == device)?;
                        AutomationTarget::DeviceParam { device: *new, param: param.clone() }
                    }
                    // What the track sends, the copy sends too. What is
                    // sent to it (a return's copy) stays with the original.
                    AutomationTarget::Send { track, to } if *track == id => {
                        AutomationTarget::Send { track: copy.id, to: *to }
                    }
                    _ => return None,
                };
                Some(AutomationLane { target, ..lane.clone() })
            })
            .collect();
        self.automation.extend(lanes);
        let new_id = copy.id;
        self.tracks.insert(index + 1, copy);
        Ok(new_id)
    }

    pub fn track_index(&self, id: TrackId) -> Result<usize, EditError> {
        self.tracks.iter().position(|t| t.id == id).ok_or(EditError::TrackNotFound(id))
    }

    pub fn track(&self, id: TrackId) -> Result<&Track, EditError> {
        self.tracks.iter().find(|t| t.id == id).ok_or(EditError::TrackNotFound(id))
    }

    pub fn track_mut(&mut self, id: TrackId) -> Result<&mut Track, EditError> {
        self.tracks.iter_mut().find(|t| t.id == id).ok_or(EditError::TrackNotFound(id))
    }

    pub fn set_track_volume(&mut self, id: TrackId, volume_db: f32) -> Result<(), EditError> {
        self.track_mut(id)?.volume_db = validate_volume(volume_db)?;
        Ok(())
    }

    pub fn set_track_pan(&mut self, id: TrackId, pan: f32) -> Result<(), EditError> {
        if !pan.is_finite() || !(-1.0..=1.0).contains(&pan) {
            return invalid(format!("pan {pan} is outside -1..=1"));
        }
        self.track_mut(id)?.pan = pan;
        Ok(())
    }

    /// Says what the track's notes mean to its instrument; see
    /// [`InstrumentProfile`]. An empty profile takes it all back.
    pub fn set_track_profile(&mut self, id: TrackId, profile: InstrumentProfile) -> Result<(), EditError> {
        let profile = profile.validated()?;
        let track = self.track_mut(id)?;
        if track.kind != TrackKind::Normal && !profile.is_empty() {
            return invalid(format!("{id} is a group or return track: it plays no notes to say anything about"));
        }
        track.profile = profile;
        Ok(())
    }

    pub fn set_master_volume(&mut self, volume_db: f32) -> Result<(), EditError> {
        self.master.volume_db = validate_volume(volume_db)?;
        Ok(())
    }

    // ---- clips ---------------------------------------------------------

    pub fn add_midi_clip(
        &mut self,
        track: TrackId,
        name: impl Into<String>,
        start: Beats,
        length: Beats,
    ) -> Result<ClipId, EditError> {
        self.add_clip(track, name.into(), start, length, ClipContent::Midi(MidiContent::default()))
    }

    pub fn add_audio_clip(
        &mut self,
        track: TrackId,
        name: impl Into<String>,
        start: Beats,
        length: Beats,
        audio: AudioContent,
    ) -> Result<ClipId, EditError> {
        audio.validate()?;
        self.add_clip(track, name.into(), start, length, ClipContent::Audio(audio))
    }

    fn add_clip(
        &mut self,
        track: TrackId,
        name: String,
        start: Beats,
        length: Beats,
        content: ClipContent,
    ) -> Result<ClipId, EditError> {
        validate_span(start, length)?;
        self.holds_clips(track)?;
        let id = ClipId(self.alloc_id());
        let track = self.track_mut(track)?;
        track.clips.push(Clip { id, name, start, length, muted: false, content });
        sort_clips(&mut track.clips);
        Ok(id)
    }

    pub fn remove_clip(&mut self, id: ClipId) -> Result<Clip, EditError> {
        for track in &mut self.tracks {
            if let Some(index) = track.clips.iter().position(|c| c.id == id) {
                return Ok(track.clips.remove(index));
            }
        }
        Err(EditError::ClipNotFound(id))
    }

    pub fn clip(&self, id: ClipId) -> Result<&Clip, EditError> {
        self.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).ok_or(EditError::ClipNotFound(id))
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Result<&mut Clip, EditError> {
        self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == id).ok_or(EditError::ClipNotFound(id))
    }

    /// The track that holds `id`.
    pub fn clip_track(&self, id: ClipId) -> Result<TrackId, EditError> {
        self.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == id)).map(|t| t.id).ok_or(EditError::ClipNotFound(id))
    }

    /// Moves and/or resizes a clip.
    pub fn set_clip_span(&mut self, id: ClipId, start: Beats, length: Beats) -> Result<(), EditError> {
        validate_span(start, length)?;
        let track = self.clip_track(id)?;
        let clip = self.clip_mut(id)?;
        clip.start = start;
        clip.length = length;
        sort_clips(&mut self.track_mut(track)?.clips);
        Ok(())
    }

    /// Copies a clip to `start` on the same track, or on `to_track` if given.
    pub fn duplicate_clip(&mut self, id: ClipId, start: Beats, to_track: Option<TrackId>) -> Result<ClipId, EditError> {
        let source = self.clip(id)?.clone();
        validate_span(start, source.length)?;
        let track = match to_track {
            Some(track) => track,
            None => self.clip_track(id)?,
        };
        self.holds_clips(track)?;
        let new_id = ClipId(self.alloc_id());
        let track = self.track_mut(track)?;
        track.clips.push(Clip { id: new_id, start, ..source });
        sort_clips(&mut track.clips);
        Ok(new_id)
    }

    /// Puts a copy of `clip` on `track` at `start`, with a new id. The clip
    /// need not be in the project any more, which is what pasting from a
    /// clipboard requires.
    pub fn paste_clip(&mut self, track: TrackId, clip: &Clip, start: Beats) -> Result<ClipId, EditError> {
        validate_span(start, clip.length)?;
        self.holds_clips(track)?;
        let id = ClipId(self.alloc_id());
        let clips = &mut self.track_mut(track)?.clips;
        clips.push(Clip { id, start, ..clip.clone() });
        sort_clips(clips);
        Ok(id)
    }

    /// Moves a clip to `start`, and to another track if one is given.
    pub fn move_clip(&mut self, id: ClipId, to_track: Option<TrackId>, start: Beats) -> Result<(), EditError> {
        let length = self.clip(id)?.length;
        validate_span(start, length)?;
        let target = match to_track {
            Some(track) => track,
            None => self.clip_track(id)?,
        };
        self.holds_clips(target)?;
        let mut clip = self.remove_clip(id)?;
        clip.start = start;
        let clips = &mut self.track_mut(target)?.clips;
        clips.push(clip);
        sort_clips(clips);
        Ok(())
    }

    /// Moves a clip's left edge to `new_start` without moving what it plays:
    /// trimming hides the beginning, extending reveals it again. Returns
    /// the start actually reached, which stops short where an audio file
    /// has nothing earlier to reveal.
    pub fn trim_clip_start(&mut self, id: ClipId, new_start: Beats) -> Result<Beats, EditError> {
        let map = self.tempo_map();
        let track = self.clip_track(id)?;
        let clip = self.clip_mut(id)?;
        let end = clip.end();
        if !new_start.is_finite() || new_start < 0.0 || new_start >= end {
            return invalid(format!("the clip's start {new_start} must be at least 0 and before its end {end}"));
        }
        // `delta` is how far the edge moves; `advance` how far the point
        // where the content starts playing moves. They differ only when
        // there is nothing earlier in the content to reveal.
        let mut delta = new_start - clip.start;
        let mut advance = delta;
        match &mut clip.content {
            ClipContent::Midi(midi) => {
                let looped = midi.loop_length.is_some_and(|length| length > 0.0);
                let shortfall = -(midi.start_offset + delta);
                if !looped && shortfall > 0.0 {
                    // Make room before the first beat by sliding the notes
                    // later, so they still sound where they did, and the
                    // controller curves with them.
                    for note in &mut midi.notes {
                        note.start += shortfall;
                    }
                    for point in midi.controllers.iter_mut().flat_map(|curve| curve.points.iter_mut()) {
                        point.beat += shortfall;
                    }
                    advance = -midi.start_offset;
                }
            }
            ClipContent::Audio(audio) => {
                // An audio file has nothing before its first sample.
                delta = delta.max(audio.beats_over(&map, clip.start, -audio.offset_seconds));
                advance = delta;
            }
        }
        clip.content.advance(advance, clip.start, &map);
        clip.start += delta;
        clip.length = end - clip.start;
        let start = clip.start;
        sort_clips(&mut self.track_mut(track)?.clips);
        Ok(start)
    }

    /// Cuts a clip in two at `at` (a timeline position strictly inside it).
    /// The original keeps the first part; the new clip, whose id is
    /// returned, plays the rest exactly as before.
    pub fn split_clip(&mut self, id: ClipId, at: Beats) -> Result<ClipId, EditError> {
        let map = self.tempo_map();
        let track = self.clip_track(id)?;
        let clip = self.clip(id)?;
        if !at.is_finite() || at <= clip.start || at >= clip.end() {
            return invalid(format!("the split point {at} is not inside the clip ({}..{})", clip.start, clip.end()));
        }
        let first_length = at - clip.start;
        let mut second = clip.clone();
        second.start = at;
        second.length = clip.end() - at;
        second.content.advance(first_length, clip.start, &map);
        if let ClipContent::Audio(audio) = &mut second.content {
            audio.fade_in = 0.0;
        }
        second.id = ClipId(self.alloc_id());
        let second_id = second.id;

        let first = self.clip_mut(id)?;
        first.length = first_length;
        if let ClipContent::Audio(audio) = &mut first.content {
            audio.fade_out = 0.0;
        }
        let clips = &mut self.track_mut(track)?.clips;
        clips.push(second);
        sort_clips(clips);
        Ok(second_id)
    }

    /// Sets the fades of an audio clip, in beats.
    pub fn set_clip_fades(&mut self, id: ClipId, fade_in: Beats, fade_out: Beats) -> Result<(), EditError> {
        let clip = self.clip_mut(id)?;
        let length = clip.length;
        let valid = |fade: Beats| fade.is_finite() && fade >= 0.0;
        if !valid(fade_in) || !valid(fade_out) || fade_in + fade_out > length {
            return invalid(format!("fades {fade_in} + {fade_out} beats do not fit in a {length}-beat clip"));
        }
        match &mut clip.content {
            ClipContent::Audio(audio) => {
                audio.fade_in = fade_in;
                audio.fade_out = fade_out;
                Ok(())
            }
            ClipContent::Midi(_) => invalid(format!("{id} is a MIDI clip; only audio clips have fades")),
        }
    }

    /// Sets how an audio clip follows the project tempo: its warp mode, the
    /// tempo it was recorded at and its transposition in semitones. See
    /// [`AudioContent`] for what each means.
    ///
    /// The clip keeps its place and length on the timeline and the point in
    /// the recording where it starts. What changes is how much of the
    /// recording its length covers, so a caller that wants the clip to keep
    /// showing the same stretch of the recording resizes it afterwards.
    pub fn set_clip_warp(
        &mut self,
        id: ClipId,
        warp: WarpMode,
        source_tempo: Option<f64>,
        transpose: f64,
    ) -> Result<(), EditError> {
        match &mut self.clip_mut(id)?.content {
            ClipContent::Audio(audio) => {
                let changed = AudioContent { warp, source_tempo, transpose, ..audio.clone() };
                changed.validate()?;
                *audio = changed;
                Ok(())
            }
            ClipContent::Midi(_) => invalid(format!("{id} is a MIDI clip; only audio clips can be warped")),
        }
    }

    // ---- MIDI files ----------------------------------------------------

    /// Adds the contents of a MIDI file at `at`: one new track with one clip
    /// per track of the file that has notes, with its controllers' curves.
    /// Returns the new track and clip ids with whether each is a drum
    /// part, so the caller can give them instruments. The project's tempo
    /// and time signature are left alone: [`Project::take_midi_time`] is
    /// what takes the file's.
    pub fn import_midi(&mut self, file: &MidiFile, at: Beats) -> Result<Vec<ImportedPart>, EditError> {
        if !at.is_finite() || at < 0.0 {
            return invalid(format!("start {at} must be >= 0 beats"));
        }
        let bars = self.meter_map();
        let bar = self.time_signature.beats_per_bar();
        let mut parts = Vec::new();
        for (index, part) in file.tracks.iter().enumerate().filter(|(_, part)| !part.notes.is_empty()) {
            let name = if part.name.is_empty() { format!("MIDI {}", index + 1) } else { part.name.clone() };
            // The part lasts as long as anything in it moves.
            let last_point = part.controllers.iter().filter_map(|curve| curve.points.last()).map(|point| point.beat);
            let end = part.notes.iter().map(Note::end).chain(last_point).fold(0.0, f64::max);
            // Whole bars, so the clip sits on the grid.
            let length = match bars.is_constant() {
                true => ((end / bar).ceil().max(1.0)) * bar,
                false => bars.bar_line_at_or_after(at + end).max(bars.bar_at(at).end()) - at,
            };
            let track = self.add_track(name.clone());
            let clip = self.add_midi_clip(track, name, at, length)?;
            {
                let clip = self.clip_mut(clip)?;
                clip.add_notes(&part.notes)?;
                for curve in &part.controllers {
                    clip.set_controller(curve.controller, curve.points.clone())?;
                }
            }
            parts.push(ImportedPart { track, clip, drums: part.drums });
        }
        Ok(parts)
    }

    /// The song's MIDI as a file: one track per project track that has
    /// notes, with every note where it sounds on the timeline and every
    /// controller as it moves there ([`Track::controller_lanes`]).
    pub fn export_midi(&self) -> MidiFile {
        let length = self.length_beats();
        let tracks = self
            .tracks
            .iter()
            .filter_map(|track| {
                let mut notes: Vec<Note> =
                    track.clips.iter().filter(|clip| !clip.muted).flat_map(Clip::timeline_notes).collect();
                if notes.is_empty() {
                    return None;
                }
                sort_notes(&mut notes);
                let drums = track.devices.iter().any(|device| device.uri.contains("drum"));
                let controllers = track.controller_lanes().iter().filter_map(|lane| lane_curve(lane, length)).collect();
                Some(MidiTrack { name: track.name.clone(), drums, notes, controllers })
            })
            .collect();
        MidiFile {
            tempo: Some(self.tempo),
            time_signature: Some(self.time_signature),
            tempo_changes: self.tempo_changes.clone(),
            meter_changes: self.meter_changes.clone(),
            tracks,
        }
    }

    /// Gives the song the tempo and the time signature of a MIDI file,
    /// with every change of them, from `at` on: what the song had before
    /// `at` stays, and what it had from there on is replaced. A file that
    /// names no tempo, or no time signature, leaves the song's.
    pub fn take_midi_time(&mut self, file: &MidiFile, at: Beats) -> Result<(), EditError> {
        let at = valid_position(at, "a MIDI file's tempo")?;
        if let Some(tempo) = file.tempo {
            let mut changes: Vec<TempoChange> =
                self.tempo_changes.iter().filter(|change| change.beat < at - SAME_BEAT).copied().collect();
            let moved = file.tempo_changes.iter().map(|change| TempoChange { beat: change.beat + at, ..*change });
            match at <= SAME_BEAT {
                true => self.set_tempo(tempo)?,
                false => changes.push(TempoChange { beat: at, tempo: valid_tempo(tempo)?, ramp: false }),
            }
            changes.extend(moved);
            self.set_tempo_changes(changes)?;
        }
        if let Some(signature) = file.time_signature {
            let mut changes: Vec<MeterChange> =
                self.meter_changes.iter().filter(|change| change.beat < at - SAME_BEAT).copied().collect();
            match at <= SAME_BEAT {
                true => self.time_signature = signature,
                false => changes.push(MeterChange { beat: at, signature }),
            }
            changes.extend(file.meter_changes.iter().map(|change| MeterChange { beat: change.beat + at, ..*change }));
            self.set_meter_changes(changes)?;
        }
        Ok(())
    }

    // ---- markers -------------------------------------------------------

    /// Adds a named position to the timeline and returns its index.
    pub fn add_marker(&mut self, beat: Beats, name: impl Into<String>) -> Result<usize, EditError> {
        if !beat.is_finite() || beat < 0.0 {
            return invalid(format!("marker position {beat} must be >= 0 beats"));
        }
        let index = self.markers.partition_point(|marker| marker.beat <= beat);
        self.markers.insert(index, Marker { beat, name: name.into() });
        Ok(index)
    }

    pub fn remove_marker(&mut self, index: usize) -> Result<Marker, EditError> {
        if index >= self.markers.len() {
            return invalid(format!("there is no marker {index}; the project has {}", self.markers.len()));
        }
        Ok(self.markers.remove(index))
    }

    /// Moves a marker to `beat` and returns its new index.
    pub fn move_marker(&mut self, index: usize, beat: Beats) -> Result<usize, EditError> {
        if !beat.is_finite() || beat < 0.0 {
            return invalid(format!("marker position {beat} must be >= 0 beats"));
        }
        let marker = self.remove_marker(index)?;
        self.add_marker(beat, marker.name)
    }

    pub fn rename_marker(&mut self, index: usize, name: impl Into<String>) -> Result<(), EditError> {
        match self.markers.get_mut(index) {
            Some(marker) => {
                marker.name = name.into();
                Ok(())
            }
            None => invalid(format!("there is no marker {index}; the project has {}", self.markers.len())),
        }
    }

    // ---- devices -------------------------------------------------------

    pub fn chain(&self, chain: ChainRef) -> Result<&Vec<Device>, EditError> {
        match chain {
            ChainRef::Track(id) => Ok(&self.track(id)?.devices),
            ChainRef::Master => Ok(&self.master.devices),
        }
    }

    fn chain_mut(&mut self, chain: ChainRef) -> Result<&mut Vec<Device>, EditError> {
        match chain {
            ChainRef::Track(id) => Ok(&mut self.track_mut(id)?.devices),
            ChainRef::Master => Ok(&mut self.master.devices),
        }
    }

    /// Inserts a device at `index` (appended when `None`).
    pub fn add_device(
        &mut self,
        chain: ChainRef,
        uri: impl Into<String>,
        name: impl Into<String>,
        index: Option<usize>,
    ) -> Result<DeviceId, EditError> {
        self.chain(chain)?;
        let id = DeviceId(self.alloc_id());
        let devices = self.chain_mut(chain)?;
        let index = index.unwrap_or(devices.len()).min(devices.len());
        devices.insert(
            index,
            Device {
                id,
                uri: uri.into(),
                name: name.into(),
                enabled: true,
                params: BTreeMap::new(),
                state: None,
                preset: None,
                sidechain: None,
            },
        );
        Ok(id)
    }

    pub fn remove_device(&mut self, id: DeviceId) -> Result<Device, EditError> {
        let chain = self.device_chain(id)?;
        let devices = self.chain_mut(chain)?;
        let index = devices.iter().position(|d| d.id == id).ok_or(EditError::DeviceNotFound(id))?;
        let device = devices.remove(index);
        self.prune_automation();
        Ok(device)
    }

    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.tracks.iter().flat_map(|t| t.devices.iter()).chain(self.master.devices.iter())
    }

    pub fn device(&self, id: DeviceId) -> Result<&Device, EditError> {
        self.devices().find(|d| d.id == id).ok_or(EditError::DeviceNotFound(id))
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Result<&mut Device, EditError> {
        self.tracks
            .iter_mut()
            .flat_map(|t| t.devices.iter_mut())
            .chain(self.master.devices.iter_mut())
            .find(|d| d.id == id)
            .ok_or(EditError::DeviceNotFound(id))
    }

    /// The chain that holds `id`.
    pub fn device_chain(&self, id: DeviceId) -> Result<ChainRef, EditError> {
        if self.master.devices.iter().any(|d| d.id == id) {
            return Ok(ChainRef::Master);
        }
        self.tracks
            .iter()
            .find(|t| t.devices.iter().any(|d| d.id == id))
            .map(|t| ChainRef::Track(t.id))
            .ok_or(EditError::DeviceNotFound(id))
    }

    fn devices_mut(&mut self) -> impl Iterator<Item = &mut Device> {
        self.tracks.iter_mut().flat_map(|t| t.devices.iter_mut()).chain(self.master.devices.iter_mut())
    }

    // ---- routing -------------------------------------------------------

    /// Has a device listen to `track` in place of its own input (see
    /// [`Device::sidechain`]), or to its own input again with `None`.
    ///
    /// The track is heard as it leaves its devices, so it has to be played
    /// before the track the device is on: a device cannot listen to its
    /// own track, to the group its track plays through or to a return its
    /// track sends to, nor to anything else that would have two tracks
    /// each waiting for the other. Whether the device does anything with a
    /// key is not the model's to know; the session refuses one that does
    /// not.
    pub fn set_sidechain(&mut self, id: DeviceId, track: Option<TrackId>) -> Result<(), EditError> {
        let chain = self.device_chain(id)?;
        if let Some(track) = track {
            self.track(track)?;
            if chain == ChainRef::Track(track) {
                return invalid(format!(
                    "{id} is on {track} itself: without a sidechain it already listens to what comes into it"
                ));
            }
        }
        let before = std::mem::replace(&mut self.device_mut(id)?.sidechain, track);
        if let Some(track) = track
            && let Err(error) = self.no_loop(format!("having {id} listen to {track}"))
        {
            self.device_mut(id)?.sidechain = before;
            return Err(error);
        }
        Ok(())
    }

    /// The tracks that must have been played before `track` can be: the
    /// members of a group, the tracks that send to a return (by a send set
    /// by hand or by one that is its envelope, switched on or not), and the
    /// tracks its devices listen to. In the order of the track list,
    /// without repeats.
    pub fn feeds(&self, track: TrackId) -> Vec<TrackId> {
        let Ok(node) = self.track(track) else { return Vec::new() };
        let keys: Vec<TrackId> = node.devices.iter().filter_map(|device| device.sidechain).collect();
        let sends_by_envelope = |from: TrackId| {
            let moved = AutomationTarget::Send { track: from, to: track };
            self.automation.iter().any(|lane| lane.target == moved)
        };
        self.tracks
            .iter()
            .filter(|other| {
                other.id != track
                    && (other.group == Some(track)
                        || other.sends.iter().any(|send| send.to == track)
                        || sends_by_envelope(other.id)
                        || keys.contains(&other.id))
            })
            .map(|other| other.id)
            .collect()
    }

    /// The order to play the tracks in, so that each comes after every
    /// track that [feeds](Self::feeds) it; `None` if there is no such
    /// order, because two tracks each wait for the other.
    ///
    /// Of the tracks that could come next, a normal track goes before a
    /// group and a group before a return, and otherwise the one higher in
    /// the track list: with nothing listening to anything this is all the
    /// normal tracks, then the groups, then the returns.
    pub fn playing_order(&self) -> Option<Vec<TrackId>> {
        let stage = |kind: TrackKind| match kind {
            TrackKind::Normal => 0,
            TrackKind::Group => 1,
            TrackKind::Return => 2,
        };
        let mut waiting: Vec<(u8, TrackId, Vec<TrackId>)> =
            self.tracks.iter().map(|track| (stage(track.kind), track.id, self.feeds(track.id))).collect();
        // Stable, so tracks of one kind stay in the order of the list.
        waiting.sort_by_key(|(stage, _, _)| *stage);
        let mut order = Vec::with_capacity(waiting.len());
        while !waiting.is_empty() {
            let ready = waiting.iter().position(|(_, _, feeds)| feeds.iter().all(|feed| order.contains(feed)))?;
            order.push(waiting.remove(ready).1);
        }
        Some(order)
    }

    /// An error that says `doing` would make a loop, if the tracks can no
    /// longer be put in an order to play them in.
    fn no_loop(&self, doing: String) -> Result<(), EditError> {
        match self.playing_order() {
            Some(_) => Ok(()),
            None => invalid(format!(
                "{doing} would make a loop: each of the two would have to be played before the other. A device can \
                 listen to any track that does not itself play through, send to or listen to the track it is on"
            )),
        }
    }

    // ---- automation ----------------------------------------------------

    /// Replaces the envelope for `target`. An empty list removes the lane.
    ///
    /// A lane that exists keeps the travel its lines follow, so that editing
    /// some of its points leaves the rest of it as it sounded. A new lane
    /// takes its control's ([`AutomationTarget::travel`]); for a device's
    /// parameter only its device knows that, and whoever knows the device
    /// says so with [`Project::write_automation`]. Left unsaid, such a lane
    /// is straight in the parameter's units.
    pub fn set_automation(
        &mut self,
        target: AutomationTarget,
        mut points: Vec<AutomationPoint>,
    ) -> Result<(), EditError> {
        match &target {
            AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
                self.track(*track)?;
            }
            AutomationTarget::DeviceParam { device, .. } => {
                self.device(*device)?;
            }
            AutomationTarget::MasterVolume => {}
            AutomationTarget::Send { track, to } => self.can_send(*track, *to)?,
        }
        if points.iter().any(|p| !p.beat.is_finite() || !p.value.is_finite() || p.beat < 0.0) {
            return invalid("automation points need finite values and beats >= 0");
        }
        points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        let existing = self.automation.iter().find(|lane| lane.target == target);
        // A lane that is switched off stays off when its points are edited.
        let enabled = existing.is_none_or(|lane| lane.enabled);
        let travel = existing.map_or_else(|| target.travel().unwrap_or_default(), |lane| lane.travel);
        let new = existing.is_none();
        self.automation.retain(|lane| lane.target != target);
        if !points.is_empty() {
            self.automation.push(AutomationLane { target: target.clone(), points, travel, enabled });
            // A send that an envelope opens is a send: its return has to be
            // played after the track, like any other it is sent to.
            if let (true, AutomationTarget::Send { track, to }) = (new, &target)
                && let Err(error) = self.no_loop(format!("sending {track} to {to}"))
            {
                self.automation.retain(|lane| lane.target != target);
                return Err(error);
            }
        }
        Ok(())
    }

    /// [`Project::set_automation`], saying what the lines are straight
    /// along: for a lane that is new, or one whose lines are to travel
    /// another way from now on.
    pub fn write_automation(
        &mut self,
        target: AutomationTarget,
        points: Vec<AutomationPoint>,
        travel: Travel,
    ) -> Result<(), EditError> {
        validate_travel(travel)?;
        self.set_automation(target.clone(), points)?;
        if let Some(lane) = self.automation.iter_mut().find(|lane| lane.target == target) {
            lane.travel = travel;
        }
        Ok(())
    }

    /// Changes what the lines of an envelope are straight along, its points
    /// staying where they are: the same points are then another curve.
    pub fn set_automation_travel(&mut self, target: &AutomationTarget, travel: Travel) -> Result<(), EditError> {
        validate_travel(travel)?;
        match self.automation.iter_mut().find(|lane| lane.target == *target) {
            Some(lane) => {
                lane.travel = travel;
                Ok(())
            }
            None => invalid("there is no automation for that target to change the travel of"),
        }
    }

    /// Switches a lane off or on again. Off, it keeps its points but drives
    /// nothing, and its target stays where it is set by hand.
    pub fn set_automation_enabled(&mut self, target: &AutomationTarget, enabled: bool) -> Result<(), EditError> {
        match self.automation.iter_mut().find(|lane| lane.target == *target) {
            Some(lane) => {
                lane.enabled = enabled;
                Ok(())
            }
            None => invalid("there is no automation for that target to switch"),
        }
    }

    /// Drops lanes whose track or device no longer exists.
    fn prune_automation(&mut self) {
        let mut lanes = std::mem::take(&mut self.automation);
        lanes.retain(|lane| match &lane.target {
            AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
                self.track(*track).is_ok()
            }
            AutomationTarget::DeviceParam { device, .. } => self.device(*device).is_ok(),
            AutomationTarget::MasterVolume => true,
            AutomationTarget::Send { track, to } => self.track(*track).is_ok() && self.track(*to).is_ok(),
        });
        self.automation = lanes;
    }

    // ---- persistence ---------------------------------------------------

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("project model is always serializable")
    }

    pub fn from_json(json: &str) -> Result<Self, EditError> {
        let mut project: Project =
            serde_json::from_str(json).map_err(|e| EditError::Invalid(format!("invalid project file: {e}")))?;
        if project.format_version > FORMAT_VERSION {
            return invalid(format!(
                "project was saved by a newer version (format {}, this build reads up to {FORMAT_VERSION})",
                project.format_version
            ));
        }
        // A file of an older format says less than it means: what it leaves
        // unsaid is written into it here, once, so that it sounds as it did,
        // and a value whose scale has moved since is moved, in the device
        // and in the envelopes that drive it.
        crate::formats::keep_project(&mut project);
        // (A file before format 5 has the one tempo and the one time signature
        // it names: there is nothing to write into it.) A map that was
        // written by hand is put in the order the song reads it in, without
        // what cannot be played.
        project
            .tempo_changes
            .retain(|change| change.beat.is_finite() && change.beat > SAME_BEAT && valid_tempo(change.tempo).is_ok());
        sort_tempo_changes(&mut project.tempo_changes);
        project.meter_changes.retain(|change| change.beat.is_finite() && change.beat > SAME_BEAT);
        project.meter_changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Ok(project)
    }
}

/// A lane as one curve over the song up to `length`, in the resolution of a
/// MIDI file: a lane is pieces of several clips' curves, looped and
/// offset, which a file has no way of saying.
fn lane_curve(lane: &ControllerLane, length: Beats) -> Option<ControllerCurve> {
    let step = 1.0 / crate::midi_file::TICKS_PER_BEAT;
    let ticks = (length.min(lane.end()) / step).ceil() as u64 + 1;
    let mut events: Vec<(Beats, f64)> = Vec::new();
    for tick in 0..=ticks {
        let beat = tick as f64 * step;
        let value = lane.value_at(beat);
        if events.last().is_none_or(|(_, last)| *last != value) {
            events.push((beat, value));
        }
    }
    // A controller that never leaves rest need not be written.
    if events.iter().all(|(_, value)| Some(*value) == lane.kind.rest()) {
        return None;
    }
    let points = controllers::thin(&events, lane.kind.is_switch(), controllers::THIN_TOLERANCE / 4.0, 4.0 * step);
    Some(ControllerCurve { controller: lane.kind, points })
}

fn sort_clips(clips: &mut [Clip]) {
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
}

fn validate_span(start: Beats, length: Beats) -> Result<(), EditError> {
    if !start.is_finite() || start < 0.0 {
        return invalid(format!("start {start} must be >= 0 beats"));
    }
    if !length.is_finite() || length <= 0.0 {
        return invalid(format!("length {length} must be > 0 beats"));
    }
    Ok(())
}

fn validate_travel(travel: Travel) -> Result<(), EditError> {
    match travel {
        Travel::Fader { floor } if !floor.is_finite() => invalid("a fader's floor must be a number of dB"),
        _ => Ok(()),
    }
}

fn validate_volume(volume_db: f32) -> Result<f32, EditError> {
    if !volume_db.is_finite() || volume_db > MAX_VOLUME_DB {
        return invalid(format!("volume {volume_db} dB is above the {MAX_VOLUME_DB} dB maximum"));
    }
    Ok(volume_db.max(MIN_VOLUME_DB))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, start: Beats) -> Note {
        Note { pitch, start, length: 1.0, velocity: 100 }
    }

    #[test]
    fn a_lane_can_be_switched_off_and_keeps_its_points() {
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let volume = AutomationTarget::TrackVolume { track };
        let pan = AutomationTarget::TrackPan { track };
        let points = vec![AutomationPoint::new(0.0, -12.0), AutomationPoint::new(8.0, 0.0)];
        project.set_automation(volume.clone(), points.clone()).unwrap();
        assert!(project.automation[0].enabled, "a new lane plays");
        assert!(!project.to_json().contains("enabled\": true"), "which is not worth a line in the file");

        project.set_automation_enabled(&volume, false).unwrap();
        assert_eq!((project.automation[0].enabled, &project.automation[0].points), (false, &points));
        // Editing its points does not switch it back on behind anyone's back.
        project.set_automation(volume.clone(), points[..1].to_vec()).unwrap();
        assert!(!project.automation[0].enabled);
        // It is saved and loaded as it is, and copied with its track.
        let loaded = Project::from_json(&project.to_json()).unwrap();
        assert_eq!(loaded.automation, project.automation);
        let copy = project.duplicate_track(track).unwrap();
        let copied =
            project.automation.iter().find(|lane| lane.target == AutomationTarget::TrackVolume { track: copy });
        assert!(copied.is_some_and(|lane| !lane.enabled));

        project.set_automation_enabled(&volume, true).unwrap();
        assert!(project.automation.iter().find(|lane| lane.target == volume).unwrap().enabled);
        assert!(project.set_automation_enabled(&pan, false).is_err(), "nothing to switch where there is no lane");
    }

    fn lane<'a>(project: &'a Project, target: &AutomationTarget) -> &'a AutomationLane {
        project.automation.iter().find(|lane| lane.target == *target).expect("the target has an envelope")
    }

    #[test]
    fn a_send_is_automated_like_a_fader_and_goes_with_its_tracks() {
        let mut project = Project::new("t");
        let lead = project.add_track("Lead");
        let bus = project.add_track_of_kind("Bus", TrackKind::Group);
        let echo = project.add_track_of_kind("Echo", TrackKind::Return);
        let hall = project.add_track_of_kind("Hall", TrackKind::Return);
        let throw = AutomationTarget::Send { track: lead, to: echo };
        // A throw: shut, open for a note, shut again. The track has no send
        // set by hand; the envelope is the send.
        let points = vec![
            AutomationPoint::new(0.0, -70.0),
            AutomationPoint::new(7.0, -70.0),
            AutomationPoint::new(8.0, -6.0),
            AutomationPoint::new(12.0, -70.0),
        ];
        project.set_automation(throw.clone(), points.clone()).unwrap();
        assert!(project.track(lead).unwrap().sends.is_empty());
        assert_eq!((lane(&project, &throw).travel, &lane(&project, &throw).points), (Travel::FADER, &points));
        // Half way up it is where a fader would be half way between the two.
        let half = lane(&project, &throw).value_at(7.5);
        assert!((half - (-70.0 + (64.0f64 * 64.0 / 2.0).sqrt())).abs() < 1e-9, "{half}");
        assert_eq!(lane(&project, &throw).value_at(20.0), -70.0);
        // A group sends as a track does.
        project.set_automation(AutomationTarget::Send { track: bus, to: hall }, points.clone()).unwrap();

        // Only what can be sent, to where it can go, and both must exist.
        for (track, to) in [(echo, hall), (lead, bus), (lead, lead), (TrackId(99), echo), (lead, TrackId(99))] {
            let refused = project.set_automation(AutomationTarget::Send { track, to }, points.clone());
            assert!(refused.is_err(), "{track} to {to}");
        }
        assert_eq!(project.automation.len(), 2);

        // Saved and loaded as it is.
        let loaded = Project::from_json(&project.to_json()).unwrap();
        assert_eq!(loaded, project);
        assert!(project.to_json().contains("\"type\": \"send\""));

        // The copy of a track sends what the track sends.
        let copy = project.duplicate_track(lead).unwrap();
        assert_eq!(lane(&project, &AutomationTarget::Send { track: copy, to: echo }).points, points);
        // A send set by hand lives beside the envelope and is not it.
        project.set_send(lead, echo, Some(-12.0)).unwrap();
        project.set_send(lead, echo, None).unwrap();
        assert_eq!(lane(&project, &throw).points, points, "removing the send by hand leaves the envelope");

        // A send that is an envelope is a send to whoever orders the tracks:
        // the lead cannot listen to the return it feeds, nor be made to feed
        // a return it listens to.
        let squeeze = project.add_device(ChainRef::Track(lead), "builtin:compressor", "Duck", None).unwrap();
        let loop_of = |error: EditError| error.to_string().contains("would make a loop");
        assert!(project.set_sidechain(squeeze, Some(echo)).is_err_and(loop_of), "it sends there by its envelope");
        assert!(project.feeds(echo).contains(&lead) && project.feeds(hall).contains(&bus));
        project.set_sidechain(squeeze, Some(hall)).unwrap();
        let before = project.clone();
        let to_hall = AutomationTarget::Send { track: lead, to: hall };
        assert!(project.set_automation(to_hall, points.clone()).is_err_and(loop_of));
        assert_eq!(project, before, "and what is refused changes nothing");
        project.remove_device(squeeze).unwrap();

        // An envelope goes with the return it feeds, and with its track.
        project.remove_track(echo).unwrap();
        assert!(
            project
                .automation
                .iter()
                .all(|lane| !matches!(lane.target, AutomationTarget::Send { to, .. } if to == echo))
        );
        assert_eq!(project.automation.len(), 1, "the group's send to the hall is all that is left");
        project.remove_track(bus).unwrap();
        assert!(project.automation.is_empty());
    }

    #[test]
    fn a_new_envelope_follows_its_control_and_an_edited_one_keeps_its_travel() {
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let filter = project.add_device(ChainRef::Track(track), "builtin:filter", "Filter", None).unwrap();
        let volume = AutomationTarget::TrackVolume { track };
        let pan = AutomationTarget::TrackPan { track };
        let cutoff = AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() };
        let fade = vec![AutomationPoint::new(0.0, 0.0), AutomationPoint::new(8.0, -60.0)];
        let sweep = vec![AutomationPoint::new(0.0, 300.0), AutomationPoint::new(8.0, 9000.0)];

        // The mixer's controls are the model's own: a fader, and the pan.
        project.set_automation(volume.clone(), fade.clone()).unwrap();
        project.set_automation(AutomationTarget::MasterVolume, fade.clone()).unwrap();
        project
            .set_automation(pan.clone(), vec![AutomationPoint::new(0.0, -1.0), AutomationPoint::new(8.0, 1.0)])
            .unwrap();
        assert_eq!(lane(&project, &volume).travel, Travel::FADER);
        assert_eq!(lane(&project, &AutomationTarget::MasterVolume).travel, Travel::FADER);
        assert!((lane(&project, &volume).value_at(4.0) + 20.0).abs() < 1e-9, "a fade is still heard half way");
        assert_eq!((lane(&project, &pan).travel, lane(&project, &pan).value_at(4.0)), (Travel::Linear, 0.0));

        // A device's parameter is what its device says: whoever knows says so.
        project.set_automation(cutoff.clone(), sweep.clone()).unwrap();
        assert_eq!((lane(&project, &cutoff).travel, lane(&project, &cutoff).value_at(4.0)), (Travel::Linear, 4650.0));
        project.write_automation(cutoff.clone(), sweep.clone(), Travel::Log).unwrap();
        let middle = lane(&project, &cutoff).value_at(4.0);
        assert!((middle - (300.0f64 * 9000.0).sqrt()).abs() < 1e-9, "the middle pitch, not the middle frequency");

        // Editing the points of a lane leaves how it travels alone...
        let mut longer = sweep.clone();
        longer.push(AutomationPoint::new(16.0, 300.0));
        project.set_automation(cutoff.clone(), longer).unwrap();
        assert_eq!(lane(&project, &cutoff).travel, Travel::Log);
        // ...until that is changed, the points staying where they are.
        project.set_automation_travel(&volume, Travel::Linear).unwrap();
        assert_eq!((lane(&project, &volume).value_at(4.0), &lane(&project, &volume).points), (-30.0, &fade));
        project.set_automation_travel(&volume, Travel::FADER).unwrap();
        assert!(project.set_automation_travel(&volume, Travel::Fader { floor: f64::NAN }).is_err());
        assert!(
            project.write_automation(volume.clone(), fade.clone(), Travel::Fader { floor: f64::INFINITY }).is_err()
        );
        let unwritten = AutomationTarget::DeviceParam { device: filter, param: "resonance".into() };
        assert!(project.set_automation_travel(&unwritten, Travel::Log).is_err(), "no lane, nothing to change");

        // A cleared lane is gone, and the next one for the target starts afresh.
        project.set_automation(cutoff.clone(), Vec::new()).unwrap();
        project.set_automation(cutoff.clone(), sweep).unwrap();
        assert_eq!(lane(&project, &cutoff).travel, Travel::Linear);

        // The travel is saved with the lane, copied with its track, and one
        // that is straight in its units is not worth a line in the file.
        let json = project.to_json();
        assert_eq!(json.matches("\"travel\"").count(), 2, "the two faders: {json}");
        assert_eq!(Project::from_json(&json).unwrap(), project);
        let copy = project.duplicate_track(track).unwrap();
        assert_eq!(lane(&project, &AutomationTarget::TrackVolume { track: copy }).travel, Travel::FADER);
    }

    #[test]
    fn a_file_saved_before_lines_followed_their_controls_plays_as_it_did() {
        for format in [1, 2] {
            an_older_file_plays_as_it_did(format);
        }
    }

    fn an_older_file_plays_as_it_did(format: u32) {
        // A project as a build before format 3 wrote it: a fade and a
        // sweep, neither lane saying how its lines travel, because there
        // was one way only.
        let old = r#"{
            "format_version": 1,
            "name": "before",
            "tempo": 120.0,
            "time_signature": { "numerator": 4, "denominator": 4 },
            "loop_region": { "enabled": false, "start": 0.0, "end": 16.0 },
            "tracks": [{
                "id": 1, "name": "Keys", "color": 15899978,
                "volume_db": 0.0, "pan": 0.0, "mute": false, "solo": false,
                "devices": [{ "id": 2, "uri": "builtin:filter", "name": "Filter", "enabled": true, "params": {} }],
                "clips": []
            }],
            "master": { "volume_db": 0.0, "devices": [] },
            "automation": [
                { "target": { "type": "track_volume", "track": 1 },
                  "points": [{ "beat": 0.0, "value": 0.0 }, { "beat": 8.0, "value": -60.0, "shape": { "curve": 0.5 } },
                             { "beat": 16.0, "value": 0.0 }] },
                { "target": { "type": "device_param", "device": 2, "param": "cutoff" },
                  "points": [{ "beat": 0.0, "value": 300.0 }, { "beat": 8.0, "value": 9000.0 }] }
            ],
            "next_id": 3
        }"#;
        let old = &old.replace("\"format_version\": 1", &format!("\"format_version\": {format}"));
        let mut project = Project::from_json(old).expect("a file of an older format opens");
        let volume = AutomationTarget::TrackVolume { track: TrackId(1) };
        let cutoff = AutomationTarget::DeviceParam { device: DeviceId(2), param: "cutoff".into() };
        // Straight in decibels and in hertz, exactly as it was played then.
        assert_eq!(lane(&project, &volume).travel, Travel::Linear);
        assert_eq!(lane(&project, &volume).value_at(4.0), -30.0);
        assert_eq!(lane(&project, &volume).value_at(2.0), -15.0);
        assert_eq!(lane(&project, &cutoff).value_at(4.0), 4650.0);
        let bent = lane(&project, &volume).value_at(12.0);
        assert_eq!(bent, automation_value(&lane(&project, &volume).points, 12.0), "a bend bends what it bent");

        // Saved by this version it is this version's file, and says nothing
        // new about lanes that are as they were.
        let saved = project.to_json();
        assert!(saved.contains(&format!("\"format_version\": {FORMAT_VERSION}")), "{saved}");
        assert!(!saved.contains("travel"));
        assert_eq!(Project::from_json(&saved).unwrap(), project);

        // Moving a point of such a lane does not change the rest of it.
        let mut points = lane(&project, &volume).points.clone();
        points[2].value = -6.0;
        project.set_automation(volume.clone(), points).unwrap();
        assert_eq!((lane(&project, &volume).travel, lane(&project, &volume).value_at(4.0)), (Travel::Linear, -30.0));
        // A lane written now on the same project is a fader's.
        project.set_automation(AutomationTarget::MasterVolume, vec![AutomationPoint::new(0.0, 0.0)]).unwrap();
        assert_eq!(lane(&project, &AutomationTarget::MasterVolume).travel, Travel::FADER);

        // And a file from a later version than this build is refused in words.
        let later = old
            .replace(&format!("\"format_version\": {format}"), &format!("\"format_version\": {}", FORMAT_VERSION + 1));
        let refusal = Project::from_json(&later).unwrap_err().to_string();
        assert!(refusal.contains("newer version") && refusal.contains(&format!("up to {FORMAT_VERSION}")), "{refusal}");
    }

    /// A song that is opened and saved must come back as it was, to the last
    /// bit: positions that drift a hair on every load end up as notes that
    /// no longer meet the grid or each other. A decimal parser that trades
    /// exactness for speed gets some of these wrong by one unit in the last
    /// place.
    #[test]
    fn saving_and_loading_changes_no_number_by_even_one_bit() {
        // The awkward lengths are ones a swung, humanized song really held.
        let awkward =
            [0.24300000000000002, 0.20700000000000074, 0.10349999999999682, 0.13800000000000523, 0.1 + 0.2, 1.0 / 3.0];
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let clip = project.add_midi_clip(track, "c", 0.0, 4096.0).unwrap();
        // Many more of the same kind, so that a parser which is wrong only
        // now and then is still caught.
        let mut lengths: Vec<Beats> = awkward.to_vec();
        lengths.extend((1..2000).map(|n| (n as f64 * 0.243).sin().abs() + n as f64 * 1e-3 + 1e-9));
        let notes: Vec<Note> = lengths
            .iter()
            .enumerate()
            .map(|(n, length)| Note { pitch: 60, start: n as f64 + length / 7.0, length: *length, velocity: 100 })
            .collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        project.set_tempo(123.45600000000002).unwrap();

        let saved = project.to_json();
        let loaded = Project::from_json(&saved).unwrap();
        let bits = |project: &Project| -> Vec<(u64, u64)> {
            let clip = &project.tracks[0].clips[0];
            clip.timeline_notes().iter().map(|note| (note.start.to_bits(), note.length.to_bits())).collect()
        };
        let changed = bits(&project).iter().zip(bits(&loaded)).filter(|(before, after)| *before != after).count();
        assert_eq!(changed, 0, "of {} notes, {changed} came back different", notes.len());
        assert_eq!(loaded.tempo.to_bits(), project.tempo.to_bits());
        assert_eq!(loaded.to_json(), saved, "a second save writes the same file");
    }

    #[test]
    fn notes_stay_sorted_and_validated() {
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let clip = project.add_midi_clip(track, "c", 0.0, 4.0).unwrap();
        let c = project.clip_mut(clip).unwrap();
        c.add_notes(&[note(64, 2.0), note(60, 0.0), note(67, 2.0), note(62, 1.0)]).unwrap();
        let pitches: Vec<u8> = c.notes().iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, [60, 62, 64, 67]);

        let bad = Note { pitch: 60, start: -1.0, length: 1.0, velocity: 100 };
        assert!(c.add_notes(&[note(72, 0.0), bad]).is_err());
        assert_eq!(c.notes().len(), 4, "a rejected batch must not be partially applied");
        assert!(c.add_notes(&[Note { velocity: 0, ..note(60, 0.0) }]).is_err());
        assert_eq!(c.remove_notes(|n| n.pitch >= 64).unwrap(), 2);
    }

    #[test]
    fn clips_sorted_and_duplicated() {
        let mut project = Project::new("t");
        let a = project.add_track("A");
        let b = project.add_track("B");
        let late = project.add_midi_clip(a, "late", 8.0, 4.0).unwrap();
        let early = project.add_midi_clip(a, "early", 0.0, 4.0).unwrap();
        assert_eq!(project.track(a).unwrap().clips[0].id, early);
        assert_eq!(project.length_beats(), 12.0);

        project.clip_mut(early).unwrap().add_notes(&[note(60, 0.0)]).unwrap();
        let copy = project.duplicate_clip(early, 4.0, Some(b)).unwrap();
        assert_eq!(project.clip_track(copy).unwrap(), b);
        assert_eq!(project.clip(copy).unwrap().notes().len(), 1);
        assert_ne!(copy, early);

        project.set_clip_span(late, 2.0, 1.0).unwrap();
        assert_eq!(project.track(a).unwrap().clips[1].id, late);
        assert!(project.add_midi_clip(a, "x", 0.0, 0.0).is_err());
        assert!(project.add_midi_clip(TrackId(999), "x", 0.0, 1.0).is_err());
    }

    #[test]
    fn devices_and_automation_follow_their_owner() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let fx = project.add_device(ChainRef::Track(track), "builtin:reverb", "Verb", None).unwrap();
        let first = project.add_device(ChainRef::Track(track), "builtin:eq", "EQ", Some(0)).unwrap();
        let order: Vec<DeviceId> = project.track(track).unwrap().devices.iter().map(|d| d.id).collect();
        assert_eq!(order, [first, synth, fx]);
        assert_eq!(project.device_chain(fx).unwrap(), ChainRef::Track(track));

        let target = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
        let points = vec![AutomationPoint::new(4.0, 2000.0), AutomationPoint::new(0.0, 1000.0)];
        project.set_automation(target.clone(), points).unwrap();
        let lane = &project.automation[0];
        assert_eq!(lane.value_at(-1.0), 1000.0);
        assert_eq!(lane.value_at(2.0), 1500.0);
        assert_eq!(lane.value_at(9.0), 2000.0);

        project.remove_device(synth).unwrap();
        assert!(project.automation.is_empty(), "lanes are removed with their device");
        assert!(project.set_automation(target, vec![]).is_err());
    }

    #[test]
    fn a_device_listens_to_a_track_that_can_be_played_before_its_own() {
        let mut project = Project::new("t");
        let (kick, bass) = (project.add_track("Kick"), project.add_track("Bass"));
        let drums = project.add_track_of_kind("Drums", TrackKind::Group);
        let room = project.add_track_of_kind("Room", TrackKind::Return);
        let squeeze = project.add_device(ChainRef::Track(bass), "builtin:compressor", "Duck", None).unwrap();
        let glue = project.add_device(ChainRef::Master, "builtin:compressor", "Glue", None).unwrap();
        let order = |project: &Project| project.playing_order().unwrap();

        // Nothing listens to anything: normal tracks, groups, returns.
        project.set_track_group(kick, Some(drums)).unwrap();
        project.set_send(kick, room, Some(-12.0)).unwrap();
        assert_eq!(order(&project), [kick, bass, drums, room]);
        assert_eq!(project.feeds(drums), [kick]);
        assert!(!project.to_json().contains("sidechain"), "not worth a line in the file while there is none");

        // The bass listens to the kick, which is before it anyway; then to
        // the drum group, which now has to come first; then to the return.
        project.set_sidechain(squeeze, Some(kick)).unwrap();
        assert_eq!(
            (project.device(squeeze).unwrap().sidechain, order(&project)),
            (Some(kick), vec![kick, bass, drums, room])
        );
        project.set_sidechain(squeeze, Some(drums)).unwrap();
        assert_eq!(order(&project), [kick, drums, bass, room]);
        assert_eq!(project.feeds(bass), [drums]);
        project.set_sidechain(squeeze, Some(room)).unwrap();
        assert_eq!(order(&project), [kick, drums, room, bass]);
        // The master is played last and may listen to any track.
        project.set_sidechain(glue, Some(bass)).unwrap();
        // It is saved, and copied with its track.
        assert_eq!(Project::from_json(&project.to_json()).unwrap(), project);
        let copy = project.duplicate_track(bass).unwrap();
        assert_eq!(project.track(copy).unwrap().devices[0].sidechain, Some(room));

        // What would have two tracks wait for each other is refused, by
        // whichever edit comes second, and leaves things as they were.
        let before = project.clone();
        let refused = |result: Result<(), EditError>| result.unwrap_err().to_string();
        assert!(refused(project.set_sidechain(squeeze, Some(bass))).contains("itself"));
        assert!(refused(project.set_send(bass, room, Some(0.0))).contains("loop"), "the bass listens to that return");
        project.set_sidechain(squeeze, Some(drums)).unwrap();
        assert!(refused(project.set_track_group(bass, Some(drums))).contains("loop"));
        let on_kick = project.add_device(ChainRef::Track(kick), "builtin:compressor", "Kick", None).unwrap();
        assert!(
            refused(project.set_sidechain(on_kick, Some(bass))).contains("loop"),
            "the kick plays through the drums"
        );
        assert!(project.set_sidechain(squeeze, Some(TrackId(999))).is_err());
        assert!(project.set_sidechain(DeviceId(999), Some(kick)).is_err());
        project.remove_device(on_kick).unwrap();
        project.set_sidechain(squeeze, Some(room)).unwrap();
        // (One id was spent on the device that came and went.)
        assert_eq!(project.tracks, before.tracks);

        // A track that is deleted is listened to by nothing; its own
        // devices go with it.
        project.remove_track(room).unwrap();
        assert_eq!(project.device(squeeze).unwrap().sidechain, None);
        project.set_sidechain(glue, None).unwrap();
        assert_eq!(project.device(glue).unwrap().sidechain, None);
    }

    #[test]
    fn a_file_of_format_1_is_read_as_what_it_meant() {
        let mut project = Project::new("old");
        let track = project.add_track("A");
        let reverb = project.add_device(ChainRef::Track(track), "builtin:reverb", "Reverb", None).unwrap();
        let delay = project.add_device(ChainRef::Master, "builtin:delay", "Delay", None).unwrap();
        let eq = project.add_device(ChainRef::Master, "builtin:eq", "EQ", None).unwrap();
        project.device_mut(reverb).unwrap().params.insert("decay".into(), 0.7);
        // As a build before format 2 wrote it.
        let this_format = format!("\"format_version\": {FORMAT_VERSION}");
        let old = project.to_json().replace(&this_format, "\"format_version\": 1");
        assert_ne!(old, project.to_json());

        let read = Project::from_json(&old).unwrap();
        assert_eq!(read.format_version, FORMAT_VERSION);
        let params = |id: DeviceId| read.device(id).unwrap().params.clone();
        assert_eq!(params(reverb).get("legacy"), Some(&1.0));
        assert_eq!(params(reverb).get("high_cut"), Some(&20_000.0));
        assert_eq!((params(reverb).get("decay"), params(reverb).len()), (Some(&0.7), 3));
        assert_eq!(params(delay).get("legacy"), Some(&1.0), "on the master as on a track");
        assert!(params(eq).is_empty(), "a device that sounds as it did is left alone");
        // Saved again it is a file of this format, and reads back the same.
        assert_eq!(Project::from_json(&read.to_json()).unwrap(), read);
        // A file of this format is taken at its word: a new reverb stays new.
        assert!(Project::from_json(&project.to_json()).unwrap().device(delay).unwrap().params.is_empty());
        // And one from a later build is refused in words.
        let later = project.to_json().replace(&this_format, "\"format_version\": 99");
        assert!(Project::from_json(&later).unwrap_err().to_string().contains("newer version"));
    }

    #[test]
    fn a_file_from_before_its_devices_changed_is_read_as_what_it_meant() {
        // A file of format 2 has envelopes that are straight in their
        // units, one of format 3 envelopes that follow their controls: the
        // faders of the day, silent at the bottom of the ranges of the day.
        let then = |floor: f64| Travel::Fader { floor };
        an_older_file_keeps_its_devices(2, [Travel::Linear; 4]);
        an_older_file_keeps_its_devices(3, [then(-48.0), then(-48.0), then(-70.0), Travel::FADER]);
    }

    /// `travels`: of the envelopes on the synth's volume, the sampler's
    /// volume, the utility's gain and the track's fader, as the file has
    /// them.
    fn an_older_file_keeps_its_devices(format: u32, travels: [Travel; 4]) {
        let mut project = Project::new("old");
        let track = project.add_track("A");
        let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let plain = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth 2", None).unwrap();
        let sampler = project.add_device(ChainRef::Track(track), "builtin:sampler", "Sampler", None).unwrap();
        let fade = project.add_device(ChainRef::Master, "builtin:utility", "Fade", None).unwrap();
        let reverb = project.add_device(ChainRef::Master, "builtin:reverb", "Reverb", None).unwrap();
        project.device_mut(synth).unwrap().params.insert("volume".into(), 8.0);
        let volume = |device: DeviceId| AutomationTarget::DeviceParam { device, param: "volume".into() };
        let cutoff = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
        let gain = AutomationTarget::DeviceParam { device: fade, param: "gain".into() };
        let fader = AutomationTarget::TrackVolume { track };
        let points = |from: f64, to: f64| vec![AutomationPoint::new(0.0, from), AutomationPoint::new(4.0, to)];
        project.write_automation(volume(synth), points(-48.0, 8.0), travels[0]).unwrap();
        project.write_automation(volume(sampler), points(-48.0, 8.0), travels[1]).unwrap();
        project.write_automation(cutoff.clone(), points(200.0, 8000.0), Travel::Linear).unwrap();
        project.write_automation(gain.clone(), points(0.0, -70.0), travels[2]).unwrap();
        project.write_automation(fader.clone(), points(0.0, -70.0), travels[3]).unwrap();
        // As a build of that format wrote it.
        let this_format = format!("\"format_version\": {FORMAT_VERSION}");
        let old = project.to_json().replace(&this_format, &format!("\"format_version\": {format}"));
        assert_ne!(old, project.to_json());

        let read = Project::from_json(&old).unwrap();
        assert_eq!(read.format_version, FORMAT_VERSION);
        let params = |id: DeviceId| read.device(id).unwrap().params.clone();
        let line = |target: &AutomationTarget| -> (Vec<f64>, Travel) {
            let lane = lane(&read, target);
            (lane.points.iter().map(|point| point.value).collect(), lane.travel)
        };
        // The synth's volume is the same sound under another number,
        // wherever the file has it; nothing else of it has moved.
        let synth_at = |volume: f64| BTreeMap::from([("legacy".to_string(), 1.0), ("volume".to_string(), volume)]);
        assert_eq!((params(synth), params(plain)), (synth_at(-13.0), synth_at(-21.0)));
        assert_eq!(line(&cutoff), (vec![200.0, 8000.0], Travel::Linear));
        // Its envelope is the same line, 21 dB down that scale: the points,
        // and with them the level a fader's travel is measured from. Half
        // way it is where it was, less the 21 dB.
        let moved = match travels[0] {
            Travel::Fader { floor } => Travel::Fader { floor: floor - 21.0 },
            other => other,
        };
        assert_eq!(line(&volume(synth)), (vec![-69.0, -13.0], moved));
        let was = travels[0].between(-48.0, 8.0, 0.5);
        assert!((lane(&read, &volume(synth)).value_at(2.0) - (was - 21.0)).abs() < 1e-9);
        // The sampler keeps its law and the defaults of the day; its own
        // volume means what it did, and its envelope is as it was.
        assert_eq!(params(sampler).get("legacy"), Some(&1.0));
        assert_eq!((params(sampler).get("velocity_sens"), params(sampler).get("volume")), (Some(&0.6), Some(&-6.0)));
        assert_eq!(line(&volume(sampler)), (vec![-48.0, 8.0], travels[1]));
        // A fade that went down to 70 dB under goes there still, the way
        // it went.
        assert_eq!(params(fade), BTreeMap::from([("legacy".to_string(), 1.0)]));
        assert_eq!(line(&gain), (vec![0.0, -70.0], travels[2]));
        assert_eq!(line(&fader), (vec![0.0, -70.0], travels[3]));
        // What changed in format 2 is as the file has it.
        assert!(params(reverb).is_empty());
        // Saved again it is a file of this format, and reads back the same.
        assert_eq!(Project::from_json(&read.to_json()).unwrap(), read);
        // And a file of format 1 is given all of it at once.
        let oldest = old.replace(&format!("\"format_version\": {format}"), "\"format_version\": 1");
        let oldest = Project::from_json(&oldest).unwrap();
        assert_eq!(oldest.device(synth).unwrap().params.get("volume"), Some(&-13.0));
        assert_eq!(oldest.device(reverb).unwrap().params.get("legacy"), Some(&1.0));
    }

    #[test]
    fn json_round_trip_preserves_everything() {
        let mut project = Project::new("Song");
        project.set_tempo(96.5).unwrap();
        let track = project.add_track("A");
        let clip = project.add_midi_clip(track, "c", 0.0, 4.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.5)]).unwrap();
        let device = project.add_device(ChainRef::Master, "clap:x", "X", None).unwrap();
        project.device_mut(device).unwrap().state = Some(vec![0, 1, 2, 255]);
        let audio = AudioContent { offset_seconds: 0.25, gain_db: -3.0, fade_in: 0.5, ..AudioContent::new("a.wav") };
        project.add_audio_clip(track, "a", 4.0, 2.0, audio).unwrap();

        let restored = Project::from_json(&project.to_json()).unwrap();
        assert_eq!(restored, project);
        let mut restored = restored;
        let next = restored.add_track("B");
        assert!(next.0 > device.0, "ids keep counting up after a reload");
    }

    #[test]
    fn splitting_and_trimming_keep_what_the_clip_plays() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let clip = project.add_midi_clip(track, "c", 8.0, 8.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0), note(62, 3.0), note(64, 6.0)]).unwrap();

        // Split at beat 12: the second half starts four beats into the notes.
        let second = project.split_clip(clip, 12.0).unwrap();
        let (first, second) = (project.clip(clip).unwrap(), project.clip(second).unwrap());
        assert_eq!((first.start, first.length), (8.0, 4.0));
        assert_eq!((second.start, second.length), (12.0, 4.0));
        assert_eq!(second.midi().unwrap().start_offset, 4.0);
        assert_eq!(second.notes().len(), 3, "notes are hidden, not deleted");
        assert!(project.split_clip(clip, 8.0).is_err(), "a split must fall inside the clip");
        assert!(project.split_clip(clip, 12.0).is_err());

        // Trimming the left edge in and back out is lossless.
        assert_eq!(project.trim_clip_start(clip, 10.0).unwrap(), 10.0);
        let trimmed = project.clip(clip).unwrap();
        assert_eq!((trimmed.start, trimmed.length, trimmed.midi().unwrap().start_offset), (10.0, 2.0, 2.0));
        assert_eq!(project.trim_clip_start(clip, 8.0).unwrap(), 8.0);
        assert_eq!(project.clip(clip).unwrap().midi().unwrap().start_offset, 0.0);

        // Extending past the first note makes room by sliding the notes.
        assert_eq!(project.trim_clip_start(clip, 6.0).unwrap(), 6.0);
        let extended = project.clip(clip).unwrap();
        assert_eq!((extended.start, extended.length), (6.0, 6.0));
        assert_eq!(extended.notes()[0].start, 2.0, "the first note still sounds at beat 8");
        assert!(project.trim_clip_start(clip, 12.0).is_err(), "the start cannot pass the end");

        // A looped clip keeps its phase when trimmed.
        let looped = project.add_midi_clip(track, "loop", 20.0, 8.0).unwrap();
        project.clip_mut(looped).unwrap().set_loop_length(Some(2.0)).unwrap();
        project.trim_clip_start(looped, 21.0).unwrap();
        assert_eq!(project.clip(looped).unwrap().midi().unwrap().start_offset, 1.0);
        project.trim_clip_start(looped, 18.0).unwrap();
        assert_eq!(project.clip(looped).unwrap().midi().unwrap().start_offset, 0.0, "2 beats before the loop start");
    }

    #[test]
    fn audio_clips_trim_split_and_fade() {
        let mut project = Project::new("t");
        project.set_tempo(120.0).unwrap();
        let track = project.add_track("A");
        let content = AudioContent { offset_seconds: 1.0, ..AudioContent::new("a.wav") };
        let clip = project.add_audio_clip(track, "a", 4.0, 8.0, content).unwrap();
        project.set_clip_fades(clip, 1.0, 2.0).unwrap();
        assert!(project.set_clip_fades(clip, 5.0, 5.0).is_err(), "fades must fit in the clip");

        // Two beats at 120 BPM is one second of audio.
        let second = project.split_clip(clip, 6.0).unwrap();
        let (first, second) =
            (project.clip(clip).unwrap().audio().unwrap(), project.clip(second).unwrap().audio().unwrap());
        assert_eq!((first.offset_seconds, first.fade_in, first.fade_out), (1.0, 1.0, 0.0));
        assert_eq!((second.offset_seconds, second.fade_in, second.fade_out), (2.0, 0.0, 2.0));

        // The file starts one second (two beats) before the clip: the left
        // edge can be pulled back that far and no further.
        assert_eq!(project.trim_clip_start(clip, 0.0).unwrap(), 2.0);
        let extended = project.clip(clip).unwrap();
        assert_eq!((extended.start, extended.length, extended.audio().unwrap().offset_seconds), (2.0, 4.0, 0.0));

        let midi = project.add_midi_clip(track, "m", 0.0, 1.0).unwrap();
        assert!(project.set_clip_fades(midi, 0.1, 0.1).is_err());
    }

    #[test]
    fn warped_clips_measure_the_recording_in_its_own_beats() {
        let mut project = Project::new("t");
        project.set_tempo(120.0).unwrap();
        let track = project.add_track("A");
        // A loop recorded at 100 BPM: one beat of it is 0.6 s of file.
        let content = AudioContent { offset_seconds: 0.6, ..AudioContent::new("loop.wav") };
        let clip = project.add_audio_clip(track, "loop", 4.0, 8.0, content).unwrap();
        let audio = |project: &Project, id: ClipId| project.clip(id).unwrap().audio().unwrap().clone();

        // Unwarped, a beat of the timeline covers a beat of project time.
        assert_eq!(audio(&project, clip).seconds_per_beat(120.0), 0.5);
        assert_eq!((audio(&project, clip).time_ratio(120.0), audio(&project, clip).followed_tempo()), (1.0, None));

        project.set_clip_warp(clip, WarpMode::Beats, Some(100.0), 3.0).unwrap();
        let warped = audio(&project, clip);
        assert_eq!((warped.warp, warped.source_tempo, warped.transpose), (WarpMode::Beats, Some(100.0), 3.0));
        assert_eq!(warped.offset_seconds, 0.6, "warping does not move where the clip starts in the file");
        assert_eq!(warped.seconds_per_beat(120.0), 0.6, "a beat of the timeline is a beat of the recording");
        assert_eq!(warped.seconds_per_beat(60.0), 0.6, "whatever the project tempo");
        assert!((warped.time_ratio(120.0) - 100.0 / 120.0).abs() < 1e-12);
        assert_eq!(warped.pitch_shift(), 3.0);
        let clip_span =
            |project: &Project, id: ClipId| (project.clip(id).unwrap().start, project.clip(id).unwrap().length);
        assert_eq!(clip_span(&project, clip), (4.0, 8.0), "nor its place on the timeline");

        // Splitting two beats in: the second half starts two recorded beats
        // (1.2 s) further into the file, not two project beats (1.0 s).
        let second = project.split_clip(clip, 6.0).unwrap();
        assert!((audio(&project, second).offset_seconds - 1.8).abs() < 1e-12);
        assert_eq!(audio(&project, second).warp, WarpMode::Beats, "both halves stay warped");
        assert_eq!(audio(&project, clip).offset_seconds, 0.6);

        // The file starts one recorded beat before the clip, so the left
        // edge can be pulled back exactly one beat; trimming in again by
        // half a beat moves 0.3 s into the file.
        assert_eq!(project.trim_clip_start(clip, 0.0).unwrap(), 3.0);
        assert!(audio(&project, clip).offset_seconds.abs() < 1e-12);
        assert_eq!(project.trim_clip_start(clip, 3.5).unwrap(), 3.5);
        assert!((audio(&project, clip).offset_seconds - 0.3).abs() < 1e-12);

        // A tempo change leaves a warped clip's offsets alone: they are in
        // the recording's time.
        project.set_tempo(90.0).unwrap();
        project.split_clip(second, 7.0).unwrap();
        assert!((audio(&project, second).offset_seconds - 1.8).abs() < 1e-12);

        // Warped without a known tempo, and repitched: the speed is the
        // recording's own, and transposing has nothing to act on.
        project.set_clip_warp(clip, WarpMode::Tonal, None, -2.0).unwrap();
        assert_eq!(audio(&project, clip).seconds_per_beat(90.0), 60.0 / 90.0);
        assert_eq!((audio(&project, clip).time_ratio(90.0), audio(&project, clip).pitch_shift()), (1.0, -2.0));
        project.set_clip_warp(clip, WarpMode::Repitch, Some(100.0), -2.0).unwrap();
        assert_eq!(audio(&project, clip).pitch_shift(), 0.0, "repitch ties pitch to speed");
        project.set_clip_warp(clip, WarpMode::Off, Some(100.0), 0.0).unwrap();
        assert_eq!(audio(&project, clip).seconds_per_beat(90.0), 60.0 / 90.0, "off ignores the source tempo");
    }

    #[test]
    fn warp_settings_are_validated_and_absent_from_old_files() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let clip = project.add_audio_clip(track, "a", 0.0, 4.0, AudioContent::new("a.wav")).unwrap();
        let midi = project.add_midi_clip(track, "m", 4.0, 4.0).unwrap();
        let before = project.clone();

        for (tempo, transpose) in [(Some(5.0), 0.0), (Some(f64::NAN), 0.0), (Some(5_000.0), 0.0), (None, 25.0)] {
            assert!(project.set_clip_warp(clip, WarpMode::Beats, tempo, transpose).is_err(), "{tempo:?} {transpose}");
        }
        assert!(project.set_clip_warp(clip, WarpMode::Tonal, Some(120.0), f64::INFINITY).is_err());
        assert!(project.set_clip_warp(midi, WarpMode::Beats, Some(120.0), 0.0).is_err(), "MIDI has no audio to warp");
        assert!(project.set_clip_warp(ClipId(999), WarpMode::Beats, Some(120.0), 0.0).is_err());
        assert_eq!(project, before, "a rejected change leaves the clip as it was");
        let bad = AudioContent { source_tempo: Some(0.0), ..AudioContent::new("a.wav") };
        assert!(project.add_audio_clip(track, "bad", 8.0, 1.0, bad).is_err());

        // An unwarped clip is written exactly as before these settings
        // existed, and a file from before them loads as unwarped.
        let json = project.to_json();
        for field in ["warp", "source_tempo", "transpose"] {
            assert!(!json.contains(field), "'{field}' should be left out when it is the default");
        }
        assert_eq!(Project::from_json(&json).unwrap(), project);

        project.set_clip_warp(clip, WarpMode::Tonal, Some(87.5), -12.0).unwrap();
        let json = project.to_json();
        assert!(json.contains("\"warp\": \"tonal\"") && json.contains("\"source_tempo\": 87.5"), "{json}");
        assert_eq!(Project::from_json(&json).unwrap(), project);
    }

    #[test]
    fn clips_move_between_tracks_and_markers_stay_sorted() {
        let mut project = Project::new("t");
        let (a, b) = (project.add_track("A"), project.add_track("B"));
        let clip = project.add_midi_clip(a, "c", 4.0, 2.0).unwrap();
        project.add_midi_clip(b, "other", 0.0, 1.0).unwrap();
        project.move_clip(clip, Some(b), 8.0).unwrap();
        assert!(project.track(a).unwrap().clips.is_empty());
        assert_eq!(project.clip_track(clip).unwrap(), b);
        assert_eq!(project.track(b).unwrap().clips[1].start, 8.0);
        assert!(project.move_clip(clip, Some(TrackId(99)), 0.0).is_err());
        assert_eq!(project.clip_track(clip).unwrap(), b, "a failed move leaves the clip where it was");

        project.add_marker(16.0, "Drop").unwrap();
        assert_eq!(project.add_marker(0.0, "Intro").unwrap(), 0);
        project.add_marker(8.0, "Build").unwrap();
        let names: Vec<&str> = project.markers.iter().map(|marker| marker.name.as_str()).collect();
        assert_eq!(names, ["Intro", "Build", "Drop"]);
        assert_eq!(project.remove_marker(1).unwrap().name, "Build");
        assert!(project.remove_marker(5).is_err());
        assert!(project.add_marker(-1.0, "x").is_err());
    }

    #[test]
    fn automation_segments_bend_and_hold() {
        let points = [
            AutomationPoint { beat: 0.0, value: 0.0, shape: Segment::Hold },
            AutomationPoint { beat: 4.0, value: 10.0, shape: Segment::Curve(1.0) },
            AutomationPoint { beat: 8.0, value: 20.0, shape: Segment::Curve(-1.0) },
            AutomationPoint::new(12.0, 30.0),
        ];
        assert_eq!(automation_value(&points, 3.99), 0.0, "hold stays put until the next point");
        assert_eq!(automation_value(&points, 4.0), 10.0);
        let slow_start = automation_value(&points, 6.0);
        assert!(slow_start > 10.0 && slow_start < 12.0, "a positive bend starts slowly: {slow_start}");
        let fast_start = automation_value(&points, 10.0);
        assert!(fast_start > 28.0 && fast_start < 30.0, "a negative bend starts fast: {fast_start}");
        assert_eq!(automation_value(&points, 8.0), 20.0, "curves still pass through their points");
        assert_eq!(Segment::Curve(0.0).shape(0.25), 0.25, "no bend is a straight line");

        let mut project = Project::new("t");
        project.set_automation(AutomationTarget::MasterVolume, points.to_vec()).unwrap();
        let restored = Project::from_json(&project.to_json()).unwrap();
        assert_eq!(restored, project, "shapes survive saving");
    }

    fn jump(beat: Beats, tempo: f64) -> TempoChange {
        TempoChange { beat, tempo, ramp: false }
    }

    fn glide(beat: Beats, tempo: f64) -> TempoChange {
        TempoChange { beat, tempo, ramp: true }
    }

    fn meter(beat: Beats, numerator: u8, denominator: u8) -> MeterChange {
        MeterChange { beat, signature: TimeSignature::new(numerator, denominator).unwrap() }
    }

    #[test]
    fn the_tempo_is_set_where_it_changes_and_nothing_in_the_song_moves() {
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let clip = project.add_midi_clip(track, "a", 8.0, 8.0).unwrap();
        project.add_marker(16.0, "Chorus").unwrap();
        let before = (project.tracks.clone(), project.markers.clone());

        // 120 to begin with, 140 from beat 8, and from beat 16 a glide
        // that arrives at 90 at beat 24, where 120 takes over again.
        project.set_tempo_at(8.0, 140.0, false).unwrap();
        project.set_tempo_at(24.0, 90.0, true).unwrap();
        project.set_tempo_at(16.0, 140.0, false).unwrap();
        project.set_tempo_at(24.0, 120.0, false).unwrap();
        assert_eq!(project.tempo_changes, [jump(8.0, 140.0), jump(16.0, 140.0), glide(24.0, 90.0), jump(24.0, 120.0)]);
        assert_eq!((&project.tracks, &project.markers), (&before.0, &before.1), "beats are where they were");
        assert_eq!(project.clip(clip).unwrap().start, 8.0);
        let map = project.tempo_map();
        for (beat, tempo) in [(0.0, 120.0), (7.9, 120.0), (8.0, 140.0), (20.0, 115.0), (24.0, 120.0), (99.0, 120.0)] {
            assert!((map.tempo_at(beat) - tempo).abs() < 1e-9, "at {beat}: {}", map.tempo_at(beat));
        }
        // Eight beats at 120 are four seconds, and eight at 140 are 24/7.
        assert!((map.seconds_at(16.0) - (4.0 + 8.0 * 60.0 / 140.0)).abs() < 1e-12);

        // Setting one of the two a beat holds leaves the other; setting it
        // again replaces it. The start of the song has its one tempo.
        project.set_tempo_at(24.0, 100.0, true).unwrap();
        project.set_tempo_at(8.0, 150.0, false).unwrap();
        project.set_tempo_at(0.0, 110.0, false).unwrap();
        assert_eq!(project.tempo, 110.0);
        assert_eq!(project.tempo_changes, [jump(8.0, 150.0), jump(16.0, 140.0), glide(24.0, 100.0), jump(24.0, 120.0)]);
        // Moved, a beat takes what it holds along, in place of what was there.
        let mut moved = project.clone();
        moved.move_tempo_at(24.0, 16.0).unwrap();
        assert_eq!(moved.tempo_changes, [jump(8.0, 150.0), glide(16.0, 100.0), jump(16.0, 120.0)]);
        assert!(moved.move_tempo_at(12.0, 20.0).is_err() && moved.move_tempo_at(16.0, 0.0).is_err());
        project.remove_tempo_at(24.0).unwrap();
        assert_eq!(project.tempo_changes, [jump(8.0, 150.0), jump(16.0, 140.0)]);

        // What cannot be: said in words that name what there is.
        assert!(project.set_tempo_at(0.0, 100.0, true).is_err(), "a glide to the start comes from nowhere");
        assert!(project.set_tempo_at(4.0, 5.0, false).is_err() && project.set_tempo_at(-1.0, 100.0, false).is_err());
        assert!(project.set_tempo_at(f64::NAN, 100.0, false).is_err());
        let error = project.remove_tempo_at(12.0).unwrap_err().to_string();
        assert!(error.contains("beats 8, 16"), "{error}");
        let two = vec![jump(4.0, 100.0), jump(4.0, 110.0)];
        assert!(project.set_tempo_changes(two).is_err(), "two tempos that take over at one beat");
        assert!(project.set_tempo_changes(vec![glide(4.0, 100.0), glide(4.0, 110.0)]).is_err(), "nor two glides");
        project.set_tempo_changes(vec![jump(12.0, 100.0), glide(12.0, 80.0), glide(4.0, 130.0)]).unwrap();
        assert_eq!(project.tempo_changes, [glide(4.0, 130.0), glide(12.0, 80.0), jump(12.0, 100.0)], "put in order");
        project.set_tempo_changes(Vec::new()).unwrap();
        assert!(project.tempo_map().is_constant());
        let error = project.remove_tempo_at(8.0).unwrap_err().to_string();
        assert!(error.contains("does not change anywhere"), "{error}");
    }

    #[test]
    fn the_time_signature_is_set_where_it_changes_and_bars_are_counted_from_there() {
        let mut project = Project::new("t");
        // Two bars of 4/4, two of 7/8, and 4/4 again.
        project.set_time_signature_at(8.0, 7, 8).unwrap();
        project.set_time_signature_at(15.0, 4, 4).unwrap();
        assert_eq!(project.meter_changes, [meter(8.0, 7, 8), meter(15.0, 4, 4)]);
        let bars = project.meter_map();
        let starts: Vec<Beats> = (0..6).map(|bar| bars.bar_start(bar)).collect();
        assert_eq!(starts, [0.0, 4.0, 8.0, 11.5, 15.0, 19.0]);
        assert_eq!(bars.cut_bars().count(), 0);

        // A third bar of 7/8 asked for where the song goes back to 4/4:
        // the change at beat 15 stays, so the signature is replaced there.
        project.set_time_signature_at(15.0, 7, 8).unwrap();
        project.set_time_signature_at(18.5, 4, 4).unwrap();
        assert_eq!(project.meter_changes, [meter(8.0, 7, 8), meter(15.0, 7, 8), meter(18.5, 4, 4)]);
        // A signature set in the middle of what follows cuts a bar short,
        // and the map says which.
        project.set_time_signature_at(10.0, 3, 4).unwrap();
        let cut: Vec<(u32, Beats)> = project.meter_map().cut_bars().map(|bar| (bar.number(), bar.length)).collect();
        assert_eq!(cut, [(3, 2.0), (5, 2.0)], "the 7/8 bar at 8 has two beats, and so has the 3/4 bar before 15");
        project.move_time_signature_at(10.0, 11.5).unwrap();
        assert_eq!(project.meter_changes[1], meter(11.5, 3, 4), "moved to the next bar line of the 7/8");
        assert!(project.move_time_signature_at(10.0, 12.0).is_err(), "nothing is at beat 10 any more");
        project.move_time_signature_at(11.5, 10.0).unwrap();
        project.remove_time_signature_at(10.0).unwrap();
        assert_eq!(project.meter_map().cut_bars().count(), 0);

        project.set_time_signature_at(0.0, 3, 4).unwrap();
        assert_eq!(project.time_signature, TimeSignature::new(3, 4).unwrap());
        assert!(
            project.set_time_signature_at(4.0, 4, 5).is_err() && project.set_time_signature_at(-4.0, 3, 4).is_err()
        );
        let error = project.remove_time_signature_at(4.0).unwrap_err().to_string();
        assert!(error.contains("beats 8, 15, 18.5"), "{error}");
        assert!(project.set_meter_changes(vec![meter(8.0, 3, 4), meter(8.0, 5, 4)]).is_err());
        project.set_meter_changes(vec![meter(12.0, 5, 4), meter(6.0, 6, 8)]).unwrap();
        assert_eq!(project.meter_changes, [meter(6.0, 6, 8), meter(12.0, 5, 4)]);
    }

    #[test]
    fn a_song_with_changes_of_tempo_and_meter_is_saved_and_an_older_file_has_none() {
        let mut project = Project::new("t");
        // A song of one tempo is written as it always was.
        let plain = project.to_json();
        assert!(!plain.contains("tempo_changes") && !plain.contains("meter_changes"), "{plain}");

        project.set_tempo_at(8.0, 140.0, false).unwrap();
        project.set_tempo_at(16.0, 100.0, true).unwrap();
        project.set_time_signature_at(8.0, 7, 8).unwrap();
        let json = project.to_json();
        assert!(
            json.contains("\"ramp\": true") && json.matches("\"ramp\"").count() == 1,
            "only a glide says so: {json}"
        );
        assert_eq!(Project::from_json(&json).unwrap(), project);

        // A file of the format before says nothing of changes and has none.
        let old = plain.replace(&format!("\"format_version\": {FORMAT_VERSION}"), "\"format_version\": 3");
        assert_ne!(old, plain);
        let loaded = Project::from_json(&old).unwrap();
        assert!(loaded.tempo_map().is_constant() && loaded.meter_map().is_constant());
        assert_eq!(
            (loaded.tempo, loaded.time_signature, loaded.format_version),
            (120.0, TimeSignature::default(), FORMAT_VERSION)
        );

        // A map written by hand in any order, with what cannot be played,
        // is read in order and without it.
        let hand = json.replace(
            "\"tempo_changes\": [",
            "\"tempo_changes\": [{ \"beat\": 30.0, \"tempo\": 90.0 }, { \"beat\": 0.0, \"tempo\": 77.0 }, \
             { \"beat\": 4.0, \"tempo\": 5000.0 },",
        );
        let loaded = Project::from_json(&hand).unwrap();
        assert_eq!(loaded.tempo_changes, [jump(8.0, 140.0), glide(16.0, 100.0), jump(30.0, 90.0)]);
    }

    #[test]
    fn a_clip_of_audio_is_trimmed_and_split_at_the_tempo_it_has_where_it_lies() {
        let mut project = Project::new("t");
        let track = project.add_track("Audio");
        // 120 until beat 4, and 60 from there: a beat lasts half a second,
        // then a whole one.
        project.set_tempo_at(4.0, 60.0, false).unwrap();
        let clip = project.add_audio_clip(track, "take", 2.0, 6.0, AudioContent::new("take.wav")).unwrap();
        let offset = |project: &Project, id: ClipId| match &project.clip(id).unwrap().content {
            ClipContent::Audio(audio) => audio.offset_seconds,
            ClipContent::Midi(_) => unreachable!(),
        };
        // Split at beat 6: two beats at 120 and two at 60 have gone by.
        let second = project.split_clip(clip, 6.0).unwrap();
        assert!((offset(&project, second) - 3.0).abs() < 1e-12, "{}", offset(&project, second));
        // Its left edge drawn back to beat 3 reveals three seconds less one
        // beat at 120; drawn back as far as the file goes, it stops at the
        // file's first sample, which lies where the first clip began.
        assert_eq!(project.trim_clip_start(second, 3.0).unwrap(), 3.0);
        assert!((offset(&project, second) - 0.5).abs() < 1e-12, "{}", offset(&project, second));
        assert!((project.trim_clip_start(second, 0.0).unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(offset(&project, second), 0.0);
        // Trimmed forward again across the change.
        project.trim_clip_start(second, 5.0).unwrap();
        assert!((offset(&project, second) - 2.0).abs() < 1e-9, "one second at 120, one beat at 60");

        // A clip that follows the tempo counts in the beats of its
        // recording, whatever the song's tempo does under it.
        let warped = AudioContent { warp: WarpMode::Beats, source_tempo: Some(100.0), ..AudioContent::new("loop.wav") };
        let looped = project.add_audio_clip(track, "loop", 10.0, 8.0, warped).unwrap();
        project.set_tempo_at(12.0, 150.0, false).unwrap();
        let tail = project.split_clip(looped, 14.0).unwrap();
        assert!((offset(&project, tail) - 4.0 * 0.6).abs() < 1e-12);
        let map = project.tempo_map();
        let audio = |id: ClipId| match &project.clip(id).unwrap().content {
            ClipContent::Audio(audio) => audio.clone(),
            ClipContent::Midi(_) => unreachable!(),
        };
        assert!((audio(tail).time_ratio_at(&map, 11.0) - 100.0 / 60.0).abs() < 1e-12);
        assert!((audio(tail).time_ratio_at(&map, 13.0) - 100.0 / 150.0).abs() < 1e-12);
        assert_eq!(audio(second).time_ratio_at(&map, 13.0), 1.0);
        assert!((audio(second).beats_over(&map, 2.0, 3.0) - 4.0).abs() < 1e-9, "two beats at 120, two at 60");
    }

    #[test]
    fn a_midi_file_brings_its_tempo_and_meter_only_when_asked_and_takes_the_song_s_out_again() {
        let file = MidiFile {
            tempo: Some(120.0),
            time_signature: TimeSignature::new(4, 4),
            tempo_changes: vec![jump(8.0, 140.0), jump(15.0, 120.0)],
            meter_changes: vec![meter(8.0, 7, 8), meter(15.0, 4, 4)],
            tracks: vec![MidiTrack {
                name: "Lead".into(),
                drums: false,
                notes: vec![
                    Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100 },
                    Note { pitch: 62, start: 9.0, length: 0.5, velocity: 100 },
                ],
                controllers: Vec::new(),
            }],
        };
        // Written and read, it is the same file; taken into a song and
        // written out of it, it is again.
        let read = crate::midi_file::parse(&crate::midi_file::write(&file)).unwrap();
        assert_eq!(read, file);
        let mut project = Project::new("t");
        project.set_tempo(96.0).unwrap();
        let parts = project.import_midi(&read, 0.0).unwrap();
        assert!(project.tempo_map().is_constant() && project.tempo == 96.0, "importing leaves the song's time alone");
        assert_eq!(project.clip(parts[0].clip).unwrap().length, 12.0, "three bars of the song's 4/4");
        project.take_midi_time(&read, 0.0).unwrap();
        assert_eq!((project.tempo, &project.tempo_changes), (120.0, &file.tempo_changes));
        assert_eq!((project.time_signature, &project.meter_changes), (TimeSignature::default(), &file.meter_changes));
        let exported = project.export_midi();
        assert_eq!((exported.tempo, exported.time_signature), (file.tempo, file.time_signature));
        assert_eq!((&exported.tempo_changes, &exported.meter_changes), (&file.tempo_changes, &file.meter_changes));
        assert_eq!(
            crate::midi_file::parse(&crate::midi_file::write(&exported)).unwrap().tempo_changes,
            file.tempo_changes
        );

        // With the song's meter changing, a part ends on a bar line of it.
        let mut other = Project::new("t");
        other.take_midi_time(&read, 0.0).unwrap();
        let parts = other.import_midi(&read, 0.0).unwrap();
        assert_eq!(other.clip(parts[0].clip).unwrap().length, 11.5, "two bars of 4/4 and one of 7/8");

        // Taken in further on, the file's time begins there, and the
        // song keeps what it had before.
        let mut later = Project::new("t");
        later.set_tempo(100.0).unwrap();
        later.set_tempo_at(4.0, 110.0, false).unwrap();
        later.set_tempo_at(40.0, 66.0, false).unwrap();
        later.take_midi_time(&read, 16.0).unwrap();
        assert_eq!(later.tempo_changes, [jump(4.0, 110.0), jump(16.0, 120.0), jump(24.0, 140.0), jump(31.0, 120.0)]);
        assert_eq!(later.meter_changes, [meter(16.0, 4, 4), meter(24.0, 7, 8), meter(31.0, 4, 4)]);
        // A file that names neither leaves the song as it is.
        let before = later.clone();
        later.take_midi_time(&MidiFile::default(), 0.0).unwrap();
        assert_eq!(later, before);
    }

    #[test]
    fn midi_files_import_as_tracks_and_export_what_sounds() {
        let mut project = Project::new("t");
        project.set_time_signature(3, 4).unwrap();
        let file = MidiFile {
            tempo: Some(140.0),
            time_signature: None,
            tempo_changes: Vec::new(),
            meter_changes: Vec::new(),
            tracks: vec![
                MidiTrack {
                    name: "Lead".into(),
                    drums: false,
                    notes: vec![note(60, 0.0), note(64, 3.5)],
                    controllers: vec![],
                },
                MidiTrack { name: String::new(), drums: false, notes: vec![], controllers: vec![] },
                MidiTrack { name: String::new(), drums: true, notes: vec![note(36, 0.0)], controllers: vec![] },
            ],
        };
        let parts = project.import_midi(&file, 6.0).unwrap();
        assert_eq!(parts.len(), 2, "tracks without notes are skipped");
        assert_eq!(project.tempo, 120.0, "importing does not change the tempo");
        let lead = project.clip(parts[0].clip).unwrap();
        assert_eq!((lead.start, lead.length, lead.notes().len()), (6.0, 6.0, 2), "4.5 beats rounds up to two 3/4 bars");
        assert_eq!(project.track(parts[1].track).unwrap().name, "MIDI 3");
        assert!(parts[1].drums && !parts[0].drums);
        assert!(project.import_midi(&file, -1.0).is_err());

        // Export resolves loops and trims to what is heard.
        let mut project = Project::new("t");
        let track = project.add_track("Bass");
        let clip = project.add_midi_clip(track, "c", 4.0, 4.0).unwrap();
        let c = project.clip_mut(clip).unwrap();
        c.add_notes(&[note(40, 0.0), note(43, 1.0)]).unwrap();
        c.set_loop_length(Some(2.0)).unwrap();
        project.add_track("Empty");
        let exported = project.export_midi();
        assert_eq!(exported.tempo, Some(120.0));
        assert_eq!(exported.tracks.len(), 1);
        let starts: Vec<f64> = exported.tracks[0].notes.iter().map(|n| n.start).collect();
        assert_eq!(starts, [4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn a_duplicated_track_is_independent_of_its_original() {
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let other = project.add_track("Other");
        let clip = project.add_midi_clip(track, "c", 0.0, 4.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0)]).unwrap();
        let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        project.device_mut(synth).unwrap().params.insert("cutoff".into(), 800.0);
        let sweep = vec![AutomationPoint::new(0.0, 200.0), AutomationPoint::new(4.0, 2000.0)];
        project.set_automation(AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() }, sweep).unwrap();
        project.set_automation(AutomationTarget::TrackVolume { track }, vec![AutomationPoint::new(0.0, -6.0)]).unwrap();
        project.set_automation(AutomationTarget::MasterVolume, vec![AutomationPoint::new(0.0, 0.0)]).unwrap();
        project.track_mut(track).unwrap().solo = true;

        let copy = project.duplicate_track(track).unwrap();
        let order: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(order, [track, copy, other], "the copy sits right below the original");
        let duplicate = project.track(copy).unwrap();
        assert_eq!((duplicate.name.as_str(), duplicate.solo), ("Keys copy", false));
        let (new_clip, new_synth) = (duplicate.clips[0].id, duplicate.devices[0].id);
        assert!(new_clip != clip && new_synth != synth, "ids are new");
        assert_eq!(duplicate.clips[0].notes(), project.clip(clip).unwrap().notes());
        assert_eq!(duplicate.devices[0].params.get("cutoff"), Some(&800.0));

        // The copy has its own automation, pointing at its own device.
        assert_eq!(project.automation.len(), 5, "three lanes plus the copy's two");
        let copied = AutomationTarget::DeviceParam { device: new_synth, param: "cutoff".into() };
        assert!(project.automation.iter().any(|lane| lane.target == copied && lane.points.len() == 2));
        project.remove_track(copy).unwrap();
        assert_eq!(project.automation.len(), 3, "removing the copy leaves the original's lanes alone");
        assert!(project.duplicate_track(TrackId(999)).is_err());
    }

    #[test]
    fn clips_paste_from_outside_the_project_and_markers_move() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let clip = project.add_midi_clip(track, "riff", 0.0, 2.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(64, 1.0)]).unwrap();
        // Cut: the clip leaves the project and lives on in a clipboard.
        let clipboard = project.remove_clip(clip).unwrap();
        let pasted = project.paste_clip(track, &clipboard, 8.0).unwrap();
        let again = project.paste_clip(track, &clipboard, 10.0).unwrap();
        assert!(pasted != clip && again != pasted);
        let first = project.clip(pasted).unwrap();
        assert_eq!((first.start, first.length, first.name.as_str(), first.notes().len()), (8.0, 2.0, "riff", 1));
        let bus = project.add_track_of_kind("Bus", TrackKind::Group);
        assert!(project.paste_clip(bus, &clipboard, 0.0).is_err());

        project.add_marker(0.0, "Intro").unwrap();
        project.add_marker(8.0, "Verse").unwrap();
        project.add_marker(16.0, "Drop").unwrap();
        assert_eq!(project.move_marker(0, 12.0).unwrap(), 1, "it lands between the other two");
        project.rename_marker(1, "Build").unwrap();
        let markers: Vec<(f64, &str)> = project.markers.iter().map(|m| (m.beat, m.name.as_str())).collect();
        assert_eq!(markers, [(8.0, "Verse"), (12.0, "Build"), (16.0, "Drop")]);
        assert!(project.move_marker(7, 0.0).is_err() && project.rename_marker(7, "x").is_err());
        assert!(project.move_marker(0, -1.0).is_err());
        assert_eq!(project.markers.len(), 3, "a rejected move loses nothing");
    }

    #[test]
    fn groups_and_returns_route_and_clean_up_after_themselves() {
        let mut project = Project::new("t");
        let (drums, bass) = (project.add_track("Drums"), project.add_track("Bass"));
        let bus = project.add_track_of_kind("Rhythm", TrackKind::Group);
        let reverb = project.add_track_of_kind("Reverb", TrackKind::Return);

        project.set_track_group(drums, Some(bus)).unwrap();
        project.set_track_group(bass, Some(bus)).unwrap();
        project.set_send(drums, reverb, Some(-12.0)).unwrap();
        project.set_send(drums, reverb, Some(-6.0)).unwrap();
        project.set_send(bus, reverb, Some(-20.0)).unwrap();
        assert_eq!(project.track(drums).unwrap().sends, [Send { to: reverb, level_db: -6.0 }], "one send per return");

        // The rules: groups do not nest, returns do not send, only returns
        // receive, and neither kind holds clips.
        assert!(project.set_track_group(bus, Some(bus)).is_err());
        assert!(project.set_track_group(drums, Some(reverb)).is_err());
        assert!(project.set_track_group(reverb, Some(bus)).is_err());
        assert!(project.set_send(reverb, reverb, Some(0.0)).is_err());
        assert!(project.set_send(drums, bus, Some(0.0)).is_err());
        assert!(project.set_send(drums, reverb, Some(40.0)).is_err());
        assert!(project.add_midi_clip(bus, "x", 0.0, 1.0).is_err());
        assert!(project.add_midi_clip(reverb, "x", 0.0, 1.0).is_err());
        let clip = project.add_midi_clip(drums, "c", 0.0, 1.0).unwrap();
        assert!(project.move_clip(clip, Some(reverb), 0.0).is_err());
        assert!(project.duplicate_clip(clip, 2.0, Some(bus)).is_err());

        let restored = Project::from_json(&project.to_json()).unwrap();
        assert_eq!(restored, project);

        project.set_send(drums, reverb, None).unwrap();
        assert!(project.track(drums).unwrap().sends.is_empty());

        // Removing a group or return leaves nothing pointing at it.
        project.set_send(bass, reverb, Some(-3.0)).unwrap();
        project.remove_track(reverb).unwrap();
        project.remove_track(bus).unwrap();
        for id in [drums, bass] {
            let track = project.track(id).unwrap();
            assert!(track.group.is_none() && track.sends.is_empty());
        }
    }

    #[test]
    fn rejects_out_of_range_settings() {
        let mut project = Project::new("t");
        assert!(project.set_tempo(5.0).is_err());
        assert!(project.set_tempo(f64::NAN).is_err());
        assert!(project.set_time_signature(4, 5).is_err());
        assert!(project.set_loop(true, 4.0, 4.0).is_err());
        let track = project.add_track("A");
        assert!(project.set_track_pan(track, 1.5).is_err());
        assert!(project.set_track_volume(track, 12.0).is_err());
        project.set_track_volume(track, -200.0).unwrap();
        assert_eq!(project.track(track).unwrap().volume_db, MIN_VOLUME_DB);
    }

    fn point(beat: Beats, value: f64) -> AutomationPoint {
        AutomationPoint::new(beat, value)
    }

    fn step(beat: Beats, value: f64) -> AutomationPoint {
        AutomationPoint { beat, value, shape: Segment::Hold }
    }

    /// What `kind` is worth on `track` at `beat`, as the engine plays it.
    fn worth(project: &Project, track: TrackId, kind: ControllerKind, beat: Beats) -> f64 {
        let lanes = project.track(track).unwrap().controller_lanes();
        lanes.iter().find(|lane| lane.kind == kind).expect("the track moves it").value_at(beat)
    }

    /// One track with a clip over 8..16 whose expression rises from 0 at its
    /// first beat to 1 at its last, a pedal that is down from its third beat
    /// to its sixth, and a note.
    fn expressive() -> (Project, TrackId, ClipId) {
        let mut project = Project::new("t");
        let track = project.add_track("Sax");
        let clip = project.add_midi_clip(track, "c", 8.0, 8.0).unwrap();
        let c = project.clip_mut(clip).unwrap();
        c.add_notes(&[note(60, 0.0), note(62, 4.0)]).unwrap();
        c.set_controller(ControllerKind::EXPRESSION, vec![point(0.0, 0.0), point(8.0, 1.0)]).unwrap();
        c.set_controller(ControllerKind::SUSTAIN, vec![step(2.0, 1.0), step(5.0, 0.0)]).unwrap();
        (project, track, clip)
    }

    #[test]
    fn a_clip_holds_one_curve_per_controller() {
        let (mut project, track, clip) = expressive();
        let c = project.clip_mut(clip).unwrap();
        c.set_controller(ControllerKind::PitchBend, vec![point(4.0, -1.0), point(0.0, 0.5)]).unwrap();
        c.set_controller(ControllerKind::MOD_WHEEL, vec![point(0.0, 0.25)]).unwrap();
        let kinds: Vec<String> = c.controllers().iter().map(|curve| curve.controller.key()).collect();
        assert_eq!(kinds, ["mod_wheel", "expression", "sustain", "pitch_bend"], "in the controllers' own order");
        assert_eq!(c.controller(ControllerKind::PitchBend).unwrap().points[0].beat, 0.0, "points are put in order");

        // Writing a controller again replaces its curve; no points remove it.
        c.set_controller(ControllerKind::MOD_WHEEL, vec![point(1.0, 0.75)]).unwrap();
        assert_eq!(c.controller(ControllerKind::MOD_WHEEL).unwrap().points, [point(1.0, 0.75)]);
        c.set_controller(ControllerKind::MOD_WHEEL, vec![]).unwrap();
        assert!(c.controller(ControllerKind::MOD_WHEEL).is_none() && c.controllers().len() == 3);

        // A curve that makes no sense changes nothing.
        let before = c.clone();
        assert!(c.set_controller(ControllerKind::EXPRESSION, vec![point(0.0, 1.5)]).is_err());
        assert!(c.set_controller(ControllerKind::EXPRESSION, vec![point(-1.0, 0.5)]).is_err());
        assert!(c.set_controller(ControllerKind::Controller(123), vec![point(0.0, 0.5)]).is_err());
        assert_eq!(*c, before);
        let audio = project.add_audio_clip(track, "a", 0.0, 4.0, AudioContent::new("a.wav")).unwrap();
        let error = project.clip_mut(audio).unwrap().set_controller(ControllerKind::EXPRESSION, vec![point(0.0, 0.5)]);
        assert!(error.unwrap_err().to_string().contains("audio clip"));
        assert!(project.clip(audio).unwrap().controllers().is_empty());

        // Read at a place on the timeline: only where the clip lies.
        let c = project.clip(clip).unwrap();
        assert_eq!(c.controller_value_at(ControllerKind::EXPRESSION, 12.0), Some(0.5));
        assert_eq!(c.controller_value_at(ControllerKind::SUSTAIN, 12.0), Some(1.0));
        assert_eq!(
            c.controller_value_at(ControllerKind::SUSTAIN, 9.0),
            Some(0.0),
            "up until the point that puts it down"
        );
        assert_eq!(c.controller_value_at(ControllerKind::EXPRESSION, 7.9), None);
        assert_eq!(c.controller_value_at(ControllerKind::EXPRESSION, 16.0), None);
        assert_eq!(c.controller_value_at(ControllerKind::BREATH, 12.0), None);
    }

    #[test]
    fn a_clip_without_controllers_is_saved_as_before_and_old_files_load() {
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        let clip = project.add_midi_clip(track, "c", 0.0, 4.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0)]).unwrap();
        let plain = project.to_json();
        assert!(!plain.contains("controllers"), "nothing new in the file of a clip that has none");
        // That file is what every project saved before controllers existed
        // looks like.
        assert_eq!(Project::from_json(&plain).unwrap(), project);

        let c = project.clip_mut(clip).unwrap();
        c.set_controller(ControllerKind::EXPRESSION, vec![point(0.0, 0.25), step(2.0, 0.5), point(4.0, 1.0)]).unwrap();
        c.set_controller(
            ControllerKind::PitchBend,
            vec![AutomationPoint { beat: 1.0, value: -0.5, shape: Segment::Curve(0.5) }],
        )
        .unwrap();
        let saved = project.to_json();
        assert!(saved.contains("\"controller\": 11") && saved.contains("\"controller\": \"pitch_bend\""), "{saved}");
        let loaded = Project::from_json(&saved).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(loaded.to_json(), saved, "a second save writes the same file");
        // Taking the curves out again leaves the file as it was.
        let c = project.clip_mut(clip).unwrap();
        c.set_controller(ControllerKind::EXPRESSION, vec![]).unwrap();
        c.set_controller(ControllerKind::PitchBend, vec![]).unwrap();
        assert_eq!(project.to_json(), plain);
    }

    #[test]
    fn controllers_go_wherever_their_clip_goes() {
        let (mut project, track, clip) = expressive();
        let other = project.add_track("Other");
        let expression = ControllerKind::EXPRESSION;
        let curves = project.clip(clip).unwrap().controllers().to_vec();

        // Moved: the curve is heard at the new place.
        project.move_clip(clip, None, 16.0).unwrap();
        assert_eq!((worth(&project, track, expression, 20.0), worth(&project, track, expression, 8.0)), (0.5, 0.0));
        project.move_clip(clip, Some(other), 8.0).unwrap();
        assert!(project.track(track).unwrap().controller_lanes().is_empty());
        assert_eq!(worth(&project, other, expression, 12.0), 0.5);
        project.move_clip(clip, Some(track), 8.0).unwrap();

        // Copied, pasted, and copied with its track: each copy has the curves.
        let copy = project.duplicate_clip(clip, 32.0, None).unwrap();
        assert_eq!(project.clip(copy).unwrap().controllers(), curves);
        assert_eq!(worth(&project, track, expression, 36.0), 0.5);
        let clipboard = project.clip(clip).unwrap().clone();
        let pasted = project.paste_clip(other, &clipboard, 0.0).unwrap();
        assert_eq!(project.clip(pasted).unwrap().controllers(), curves);
        let twin = project.duplicate_track(track).unwrap();
        assert_eq!(project.track(twin).unwrap().clips[0].controllers(), curves);
        // A copy is its own: changing it leaves the original alone.
        project.clip_mut(copy).unwrap().set_controller(expression, vec![point(0.0, 1.0)]).unwrap();
        assert_eq!(project.clip(clip).unwrap().controllers(), curves);

        // A muted clip moves nothing.
        project.remove_clip(copy).unwrap();
        project.clip_mut(clip).unwrap().muted = true;
        assert!(project.track(track).unwrap().controller_lanes().is_empty());
    }

    #[test]
    fn splitting_and_trimming_keep_what_the_controllers_do() {
        let (mut project, track, clip) = expressive();
        let (expression, pedal) = (ControllerKind::EXPRESSION, ControllerKind::SUSTAIN);
        let inside: Vec<Beats> = (0..32).map(|n| 8.0 + n as f64 * 0.25).collect();
        let heard = |project: &Project, kind| -> Vec<f64> {
            inside.iter().map(|beat| worth(project, track, kind, *beat)).collect()
        };
        let (was_expression, was_pedal) = (heard(&project, expression), heard(&project, pedal));
        assert_eq!((was_expression[8], was_pedal[8], was_pedal[20]), (0.25, 1.0, 0.0));

        // Split in the middle of the swell and of the pedal: together the
        // two halves do exactly what the whole did.
        let second = project.split_clip(clip, 11.0).unwrap();
        assert_eq!(project.clip(second).unwrap().controllers(), project.clip(clip).unwrap().controllers());
        assert_eq!(heard(&project, expression), was_expression);
        assert_eq!(heard(&project, pedal), was_pedal);
        assert_eq!(worth(&project, track, pedal, 16.0), 0.0);

        // Trimmed in from the left and pulled back out: nothing is lost.
        project.trim_clip_start(second, 13.0).unwrap();
        assert_eq!(worth(&project, track, expression, 14.0), 0.75, "what is left plays what it played");
        assert_eq!(worth(&project, track, expression, 12.0), 0.375, "the gap holds where the first half stopped");
        assert_eq!(worth(&project, track, pedal, 12.0), 0.0, "and the pedal comes up at the first half's end");
        project.trim_clip_start(second, 11.0).unwrap();
        assert_eq!(heard(&project, expression), was_expression);
        assert_eq!(heard(&project, pedal), was_pedal);

        // Shortened from the right and lengthened again.
        project.set_clip_span(second, 11.0, 2.0).unwrap();
        assert_eq!((worth(&project, track, expression, 12.0), worth(&project, track, expression, 15.0)), (0.5, 0.625));
        project.set_clip_span(second, 11.0, 5.0).unwrap();
        assert_eq!(heard(&project, expression), was_expression);

        // Pulled out to the left past where the content begins: the notes
        // are slid to make room, and the curves with them.
        project.trim_clip_start(clip, 6.0).unwrap();
        assert_eq!(project.clip(clip).unwrap().notes()[0].start, 2.0);
        assert_eq!(project.clip(clip).unwrap().controller(expression).unwrap().points[0], point(2.0, 0.0));
        assert_eq!(heard(&project, expression)[..12], was_expression[..12]);
        assert_eq!(worth(&project, track, expression, 7.0), 0.0, "the new room holds the curve's first value");
    }

    #[test]
    fn controllers_loop_with_their_clip() {
        let (mut project, track, clip) = expressive();
        let expression = ControllerKind::EXPRESSION;
        // The first two beats of the swell, four times over.
        project.clip_mut(clip).unwrap().set_loop_length(Some(2.0)).unwrap();
        let at = |project: &Project, beat| worth(project, track, expression, beat);
        assert_eq!(
            (at(&project, 8.0), at(&project, 9.0), at(&project, 10.0), at(&project, 13.0)),
            (0.0, 0.125, 0.0, 0.125)
        );
        assert_eq!(at(&project, 16.0), 0.25, "left where the last pass ended");
        // Trimming a looped clip keeps the loop's phase, for the curve too.
        project.trim_clip_start(clip, 9.0).unwrap();
        assert_eq!((at(&project, 9.0), at(&project, 10.0), at(&project, 11.0)), (0.125, 0.0, 0.125));
        // And a split leaves both halves in phase.
        let second = project.split_clip(clip, 12.5).unwrap();
        assert_eq!((at(&project, 12.0), at(&project, 12.5), at(&project, 13.0)), (0.0, 0.0625, 0.125));
        assert_eq!(project.clip(second).unwrap().midi().unwrap().start_offset, 0.5);
        assert_eq!(project.clip(clip).unwrap().content_beat(11.5), Some(1.5));
        assert_eq!(project.clip(clip).unwrap().content_beat(12.5), None);
    }

    #[test]
    fn midi_files_carry_controllers_both_ways() {
        let (mut project, track, clip) = expressive();
        let c = project.clip_mut(clip).unwrap();
        c.set_controller(
            ControllerKind::PitchBend,
            vec![point(0.0, 0.0), point(2.0, -1.0), step(2.0, 0.0), point(6.0, 0.5)],
        )
        .unwrap();
        c.set_controller(ControllerKind::Pressure, vec![point(4.0, 0.0), point(5.0, 0.75)]).unwrap();

        let bytes = crate::midi_file::write(&project.export_midi());
        let file = crate::midi_file::parse(&bytes).unwrap();
        assert_eq!(file.tracks.len(), 1);
        let kinds: Vec<String> = file.tracks[0].controllers.iter().map(|curve| curve.controller.key()).collect();
        assert_eq!(kinds, ["expression", "sustain", "pressure", "pitch_bend"]);
        let points: usize = file.tracks[0].controllers.iter().map(|curve| curve.points.len()).sum();
        assert!(points < 40, "{points} points: curves come back as breakpoints, not as every event");

        let mut imported = Project::new("in");
        let parts = imported.import_midi(&file, 0.0).unwrap();
        let back = parts[0].track;
        // Every controller does in the imported song what it did in the
        // exported one, to within a step of a MIDI controller and a tick.
        for kind in [ControllerKind::EXPRESSION, ControllerKind::PitchBend, ControllerKind::Pressure] {
            for n in 0..96 {
                let beat = 8.0 + n as f64 / 12.0 + 0.01;
                let (was, is) = (worth(&project, track, kind, beat), worth(&imported, back, kind, beat));
                assert!((was - is).abs() < 0.02, "{kind} at beat {beat}: {is} for {was}");
            }
        }
        for (beat, down) in [(9.9, 0.0), (10.01, 1.0), (12.9, 1.0), (13.01, 0.0), (15.9, 0.0)] {
            assert_eq!(worth(&imported, back, ControllerKind::SUSTAIN, beat), down, "the pedal at beat {beat}");
        }
        assert_eq!(worth(&imported, back, ControllerKind::PitchBend, 10.01), 0.0, "the wheel's leap back survives");
        let clip = imported.clip(parts[0].clip).unwrap();
        assert_eq!((clip.start, clip.length, clip.notes().len()), (0.0, 16.0, 2));

        // A looped, offset clip is written as it sounds: the file knows
        // nothing of loops.
        let (mut project, track, clip) = expressive();
        project.clip_mut(clip).unwrap().set_loop_length(Some(2.0)).unwrap();
        let file = crate::midi_file::parse(&crate::midi_file::write(&project.export_midi())).unwrap();
        let mut imported = Project::new("in");
        let back = imported.import_midi(&file, 0.0).unwrap()[0].track;
        for beat in [8.5, 9.5, 10.5, 13.5, 15.5] {
            let (was, is) = (
                worth(&project, track, ControllerKind::EXPRESSION, beat),
                worth(&imported, back, ControllerKind::EXPRESSION, beat),
            );
            assert!((was - is).abs() < 0.02, "at beat {beat}: {is} for {was}");
        }
    }
}
