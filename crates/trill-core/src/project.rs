//! The project model: a plain, serializable description of a song.
//!
//! All edits go through methods on [`Project`], which validate their input and
//! keep the model's invariants (sorted notes, live automation targets, unique
//! ids). The model knows nothing about audio; the engine compiles it into a
//! render graph.

use crate::ids::{ClipId, DeviceId, TrackId};
use crate::midi_file::{MidiFile, MidiTrack};
use crate::time::{Beats, TimeSignature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIN_TEMPO: f64 = 20.0;
pub const MAX_TEMPO: f64 = 999.0;
pub const MIN_VOLUME_DB: f32 = -70.0;
pub const MAX_VOLUME_DB: f32 = 6.0;
pub const FORMAT_VERSION: u32 = 1;
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
}

fn is_zero(value: &Beats) -> bool {
    *value == 0.0
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioContent {
    /// Path of the audio file on disk.
    pub path: String,
    /// Where in the file playback starts, in seconds.
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
}

impl AudioContent {
    /// The whole file at unity gain, without fades.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into(), ..Self::default() }
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
    /// (earlier when negative), as trimming a clip's left edge does.
    fn advance(&mut self, beats: Beats, tempo: f64) {
        match self {
            ClipContent::Midi(midi) => {
                let target = midi.start_offset + beats;
                midi.start_offset = match midi.loop_length.filter(|length| *length > 0.0) {
                    Some(length) => target.rem_euclid(length),
                    None => target,
                };
            }
            ClipContent::Audio(audio) => audio.offset_seconds += beats * 60.0 / tempo,
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
}

fn enabled_default() -> bool {
    true
}

mod state_base64 {
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
}

impl AutomationLane {
    pub fn value_at(&self, beat: Beats) -> f64 {
        automation_value(&self.points, beat)
    }
}

/// Evaluates a breakpoint envelope. `points` must be sorted and non-empty.
pub fn automation_value(points: &[AutomationPoint], beat: Beats) -> f64 {
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
    a.value + (b.value - a.value) * a.shape.shape((beat - a.beat) / span)
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
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub loop_region: LoopRegion,
    pub tracks: Vec<Track>,
    pub master: Master,
    #[serde(default)]
    pub automation: Vec<AutomationLane>,
    /// Sorted by beat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    next_id: u64,
}

fn format_version_default() -> u32 {
    FORMAT_VERSION
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

    pub fn set_tempo(&mut self, tempo: f64) -> Result<(), EditError> {
        if !tempo.is_finite() || !(MIN_TEMPO..=MAX_TEMPO).contains(&tempo) {
            return invalid(format!("tempo {tempo} is outside {MIN_TEMPO}..={MAX_TEMPO} BPM"));
        }
        self.tempo = tempo;
        Ok(())
    }

    pub fn set_time_signature(&mut self, numerator: u8, denominator: u8) -> Result<(), EditError> {
        match TimeSignature::new(numerator, denominator) {
            Some(signature) => {
                self.time_signature = signature;
                Ok(())
            }
            None => invalid(format!("unsupported time signature {numerator}/{denominator}")),
        }
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
        self.track_mut(id)?.group = group;
        Ok(())
    }

    /// Sets how much of a track is sent to a return track, in dB after the
    /// fader; `None` removes the send.
    pub fn set_send(&mut self, id: TrackId, to: TrackId, level_db: Option<f32>) -> Result<(), EditError> {
        if self.track(id)?.kind == TrackKind::Return {
            return invalid(format!("{id} is a return track; returns cannot send to returns"));
        }
        if self.track(to)?.kind != TrackKind::Return {
            return invalid(format!("{to} is not a return track"));
        }
        let level_db = level_db.map(validate_volume).transpose()?;
        let sends = &mut self.track_mut(id)?.sends;
        sends.retain(|send| send.to != to);
        if let Some(level_db) = level_db {
            sends.push(Send { to, level_db });
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
                    _ => return None,
                };
                Some(AutomationLane { target, points: lane.points.clone() })
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
        if !audio.offset_seconds.is_finite() || audio.offset_seconds < 0.0 {
            return invalid("audio offset must be >= 0 seconds");
        }
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
        let tempo = self.tempo;
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
                    // later, so they still sound where they did.
                    for note in &mut midi.notes {
                        note.start += shortfall;
                    }
                    advance = -midi.start_offset;
                }
            }
            ClipContent::Audio(audio) => {
                // An audio file has nothing before its first sample.
                delta = delta.max(-audio.offset_seconds * tempo / 60.0);
                advance = delta;
            }
        }
        clip.content.advance(advance, tempo);
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
        let tempo = self.tempo;
        let track = self.clip_track(id)?;
        let clip = self.clip(id)?;
        if !at.is_finite() || at <= clip.start || at >= clip.end() {
            return invalid(format!("the split point {at} is not inside the clip ({}..{})", clip.start, clip.end()));
        }
        let first_length = at - clip.start;
        let mut second = clip.clone();
        second.start = at;
        second.length = clip.end() - at;
        second.content.advance(first_length, tempo);
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

    // ---- MIDI files ----------------------------------------------------

    /// Adds the contents of a MIDI file at `at`: one new track with one clip
    /// per track of the file that has notes. Returns the new track and clip
    /// ids with whether each is a drum part, so the caller can give them
    /// instruments. The project's tempo is left alone.
    pub fn import_midi(&mut self, file: &MidiFile, at: Beats) -> Result<Vec<ImportedPart>, EditError> {
        if !at.is_finite() || at < 0.0 {
            return invalid(format!("start {at} must be >= 0 beats"));
        }
        let bar = self.time_signature.beats_per_bar();
        let mut parts = Vec::new();
        for (index, part) in file.tracks.iter().enumerate().filter(|(_, part)| !part.notes.is_empty()) {
            let name = if part.name.is_empty() { format!("MIDI {}", index + 1) } else { part.name.clone() };
            let end = part.notes.iter().map(Note::end).fold(0.0, f64::max);
            // Whole bars, so the clip sits on the grid.
            let length = ((end / bar).ceil().max(1.0)) * bar;
            let track = self.add_track(name.clone());
            let clip = self.add_midi_clip(track, name, at, length)?;
            self.clip_mut(clip)?.add_notes(&part.notes)?;
            parts.push(ImportedPart { track, clip, drums: part.drums });
        }
        Ok(parts)
    }

    /// The song's MIDI as a file: one track per project track that has MIDI
    /// clips, with every note where it sounds on the timeline.
    pub fn export_midi(&self) -> MidiFile {
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
                Some(MidiTrack { name: track.name.clone(), drums, notes })
            })
            .collect();
        MidiFile { tempo: Some(self.tempo), time_signature: Some(self.time_signature), tracks }
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
            Device { id, uri: uri.into(), name: name.into(), enabled: true, params: BTreeMap::new(), state: None },
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

    // ---- automation ----------------------------------------------------

    /// Replaces the envelope for `target`. An empty list removes the lane.
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
        }
        if points.iter().any(|p| !p.beat.is_finite() || !p.value.is_finite() || p.beat < 0.0) {
            return invalid("automation points need finite values and beats >= 0");
        }
        points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.automation.retain(|lane| lane.target != target);
        if !points.is_empty() {
            self.automation.push(AutomationLane { target, points });
        }
        Ok(())
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
        });
        self.automation = lanes;
    }

    // ---- persistence ---------------------------------------------------

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("project model is always serializable")
    }

    pub fn from_json(json: &str) -> Result<Self, EditError> {
        let project: Project =
            serde_json::from_str(json).map_err(|e| EditError::Invalid(format!("invalid project file: {e}")))?;
        if project.format_version > FORMAT_VERSION {
            return invalid(format!(
                "project was saved by a newer version (format {}, this build reads up to {FORMAT_VERSION})",
                project.format_version
            ));
        }
        Ok(project)
    }
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

    #[test]
    fn midi_files_import_as_tracks_and_export_what_sounds() {
        let mut project = Project::new("t");
        project.set_time_signature(3, 4).unwrap();
        let file = MidiFile {
            tempo: Some(140.0),
            time_signature: None,
            tracks: vec![
                MidiTrack { name: "Lead".into(), drums: false, notes: vec![note(60, 0.0), note(64, 3.5)] },
                MidiTrack { name: String::new(), drums: false, notes: vec![] },
                MidiTrack { name: String::new(), drums: true, notes: vec![note(36, 0.0)] },
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
}
