//! What the stage reads of a song: its sections, the notes of each part
//! where they sound, and the drum hits the picture answers on the sample.
//!
//! A [`Score`] is worked out once for a project and read for every frame.
//! It is a pure function of the project (and of which tracks are drums,
//! where the project does not say), so two films of one song are the same
//! film.

use crate::voice::Voiced;
use trill_core::project::automation_value;
use trill_core::{
    AutomationTarget, Bar, Beats, ClipId, MeterMap, Note, Project, Role, TempoMap, TimeSignature, Track, TrackId,
    TrackKind,
};

/// The built-in devices whose notes are drums.
const DRUM_DEVICES: [&str; 2] = ["builtin:drumkit", "builtin:drumrack"];
/// A part is taken to be away once it has been silent for this many bars
/// of the song's first time signature.
const AWAY_AFTER_BARS: f64 = 2.0;
/// Sections found without markers are at least this many bars long, and are
/// cut in pieces of this many where nothing changes for longer.
const FOUND_SECTION_BARS: (f64, f64) = (4.0, 8.0);
/// Snare hits softer than this are ghost notes and move nothing.
const GHOST_VELOCITY: u8 = 60;

/// A stretch of the song with a name: from one marker to the next.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Empty where the song has no marker to name it.
    pub name: String,
    pub start: Beats,
    pub end: Beats,
}

impl Section {
    pub fn length(&self) -> Beats {
        self.end - self.start
    }
}

/// Where a clip lies on its lane.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipSpan {
    pub id: ClipId,
    pub name: String,
    pub start: Beats,
    pub end: Beats,
}

/// Something of a part that the song moves by itself (its volume, a
/// filter's cutoff), as a line over the song.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    /// Where the line is at each beat it was read at, from 0 (the lowest
    /// the envelope goes) to 1 (the highest), in order.
    pub line: Vec<(Beats, f32)>,
}

impl Envelope {
    /// Where the line is at `beat`: held before its first point and after
    /// its last.
    pub fn at(&self, beat: Beats) -> f32 {
        let after = self.line.partition_point(|(at, _)| *at <= beat);
        match (after.checked_sub(1).and_then(|before| self.line.get(before)), self.line.get(after)) {
            (Some((from, low)), Some((to, high))) => low + (high - low) * ((beat - from) / (to - from)) as f32,
            (Some((_, value)), None) | (None, Some((_, value))) => *value,
            (None, None) => 0.5,
        }
    }
}

/// A row of a drum part: one key of the kit and what it is called.
#[derive(Clone, Debug, PartialEq)]
pub struct DrumRow {
    pub pitch: u8,
    pub name: String,
}

/// One track that has something to play.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub track: TrackId,
    pub name: String,
    /// The color stored on the track, 0xRRGGBB.
    pub color: u32,
    /// Its notes are drums: it is shown as a grid of named rows.
    pub drums: bool,
    /// The notes it plays, at their places on the timeline, sorted by
    /// start: what its unmuted clips sound, without the keys that only
    /// switch how its instrument plays.
    pub notes: Vec<Note>,
    /// The longest of them, which bounds how far back a search for what
    /// sounds at a moment has to look.
    pub longest: Beats,
    pub clips: Vec<ClipSpan>,
    /// Lowest and highest note.
    pub low: u8,
    pub high: u8,
    /// The keys a drum part uses, lowest first. Empty for a pitched part.
    pub rows: Vec<DrumRow>,
    /// Where the part is there: stretches of notes (and of audio clips)
    /// with no silence of two bars or more inside them.
    pub spans: Vec<(Beats, Beats)>,
    /// The recordings it plays, where their outlines were read
    /// ([`Score::with_voices`]): shown as waveforms in place of note lines.
    pub voiced: Vec<Voiced>,
    /// What of it the song automates, in the order of the song's lanes.
    pub envelopes: Vec<Envelope>,
}

impl Part {
    /// The notes that sound anywhere between `from` and `to`.
    pub fn notes_between(&self, from: Beats, to: Beats) -> impl Iterator<Item = &Note> {
        let first = self.notes.partition_point(|note| note.start < from - self.longest);
        self.notes[first..].iter().take_while(move |note| note.start < to).filter(move |note| note.end() > from)
    }

    /// The notes that start between `from` (included) and `to`.
    pub fn starting_between(&self, from: Beats, to: Beats) -> &[Note] {
        let first = self.notes.partition_point(|note| note.start < from);
        let end = self.notes.partition_point(|note| note.start < to);
        &self.notes[first..end]
    }

    /// Whether the part is there at `beat`.
    pub fn present(&self, beat: Beats) -> bool {
        self.spans.iter().any(|(start, end)| (*start..*end).contains(&beat))
    }

    /// The row of a drum part that `pitch` is on.
    pub fn row_of(&self, pitch: u8) -> Option<usize> {
        self.rows.iter().position(|row| row.pitch == pitch)
    }
}

/// One drum hit that the picture answers: when, and how hard (0..=1).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hit {
    pub beat: Beats,
    pub hard: f32,
}

/// The drum hits of the song that move the picture, each list sorted by
/// time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hits {
    pub kicks: Vec<Hit>,
    pub snares: Vec<Hit>,
    pub crashes: Vec<Hit>,
}

/// How a song's beats become time: its tempo, and every change of it.
///
/// A song of one tempo is worked out as it always was, to the bit, so that
/// a picture of it is the picture it was before a tempo could change.
#[derive(Clone, Debug, PartialEq)]
pub struct Time {
    first: f64,
    map: TempoMap,
}

impl Time {
    /// The time of `project`.
    pub fn of(project: &Project) -> Self {
        Self { first: project.tempo, map: project.tempo_map() }
    }

    /// One tempo throughout.
    pub fn constant(tempo: f64) -> Self {
        Self { first: tempo, map: TempoMap::constant(tempo) }
    }

    /// Seconds from the start of the song to `beat`.
    pub fn seconds(&self, beat: Beats) -> f64 {
        if self.map.is_constant() { beat * 60.0 / self.first } else { self.map.seconds_at(beat) }
    }

    /// The beat the song is at `seconds` in.
    pub fn beat_at(&self, seconds: f64) -> Beats {
        if self.map.is_constant() { seconds * self.first / 60.0 } else { self.map.beat_at_seconds(seconds) }
    }

    /// Seconds from `from` to `to`; negative if `to` is the earlier.
    pub fn between(&self, from: Beats, to: Beats) -> f64 {
        if self.map.is_constant() { (to - from) * 60.0 / self.first } else { self.map.seconds_between(from, to) }
    }

    /// Seconds that `length` beats last from `from` on.
    pub fn over(&self, from: Beats, length: Beats) -> f64 {
        if self.map.is_constant() { length * 60.0 / self.first } else { self.map.seconds_over(from, length) }
    }

    /// The beat the song was at `seconds` before `beat`.
    pub fn before(&self, beat: Beats, seconds: f64) -> Beats {
        if self.map.is_constant() {
            beat - seconds * self.first / 60.0
        } else {
            self.map.beat_at_seconds(self.map.seconds_at(beat) - seconds)
        }
    }

    /// The tempo at `beat`, in beats per minute: while it glides, where
    /// the glide has it.
    pub fn tempo_at(&self, beat: Beats) -> f64 {
        self.map.tempo_at(beat)
    }

    /// Whether the song has one tempo throughout.
    pub fn is_constant(&self) -> bool {
        self.map.is_constant()
    }
}

/// A place where the song's tempo or its time signature changes.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub beat: Beats,
    /// The time signature that takes over here.
    pub signature: Option<TimeSignature>,
    /// The tempo that takes over here.
    pub tempo: Option<f64>,
    /// Where the song begins to glide to that tempo, if it glides to it
    /// and does not jump.
    pub glides_from: Option<Beats>,
}

impl Change {
    /// What is said of it, as the window's ruler says it: a time
    /// signature, a tempo, an arrow before a tempo the song glides to, with
    /// a dot between: "7/8 · 140 BPM".
    pub fn said(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.signature.map(|signature| format!("{}/{}", signature.numerator, signature.denominator)));
        let arrow = if self.glides_from.is_some() { "\u{2192}" } else { "" };
        parts.extend(self.tempo.map(|tempo| format!("{arrow}{} BPM", tempo_said(tempo))));
        parts.join(" \u{00B7} ")
    }
}

/// `"156"`, `"127.5"`: a tempo without the decimals it does not have.
pub fn tempo_said(tempo: f64) -> String {
    let text = format!("{tempo:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Every place where the tempo or the time signature of `project` changes,
/// in order; what changes at one beat is one change.
fn changes_of(project: &Project) -> Vec<Change> {
    /// The change at `beat`: a new one where nothing changed there yet.
    fn at(changes: &mut Vec<Change>, beat: Beats) -> &mut Change {
        let index = match changes.iter().position(|change| (change.beat - beat).abs() < 1e-6) {
            Some(index) => index,
            None => {
                changes.push(Change { beat, signature: None, tempo: None, glides_from: None });
                changes.len() - 1
            }
        };
        &mut changes[index]
    }
    let mut changes: Vec<Change> = Vec::new();
    for change in &project.meter_changes {
        at(&mut changes, change.beat).signature = Some(change.signature);
    }
    // A glide comes from the change of tempo before it, or from the start.
    let mut before = 0.0;
    for change in &project.tempo_changes {
        let there = at(&mut changes, change.beat);
        (there.tempo, there.glides_from) = (Some(change.tempo), change.ramp.then_some(before));
        before = change.beat;
    }
    changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    changes
}

/// A song as the stage reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub name: String,
    /// The tempo and the time signature the song begins with. What they
    /// are at a beat is asked of [`Score::time`] and [`Score::bars`].
    pub tempo: f64,
    pub signature: TimeSignature,
    /// How its beats become time, and how they become bars.
    pub time: Time,
    pub bars: MeterMap,
    /// Where the tempo or the time signature changes, in order.
    pub changes: Vec<Change>,
    /// Where the last clip ends, taken up to the bar line.
    pub length: Beats,
    /// The whole song, in order, without gaps.
    pub sections: Vec<Section>,
    /// The tracks with notes, in the project's order.
    pub parts: Vec<Part>,
    pub hits: Hits,
}

impl Score {
    /// Reads `project`, taking a track for drums where it says so itself or
    /// plays through a built-in drum instrument.
    pub fn read(project: &Project) -> Self {
        Self::of(project, |track| track.devices.iter().any(|device| DRUM_DEVICES.contains(&device.uri.as_str())))
    }

    /// Reads `project`. `drum_instrument` says whether a track that does
    /// not say what its notes are plays through a drum instrument: a
    /// session knows that of plugins too.
    pub fn of(project: &Project, drum_instrument: impl Fn(&Track) -> bool) -> Self {
        Self::reading(project, drum_instrument, false)
    }

    /// Reads `project` with a part for every track, the ones that hold
    /// nothing yet too: a song that is being made is shown with the
    /// tracks it has, before anything is written on them.
    pub fn of_every_track(project: &Project, drum_instrument: impl Fn(&Track) -> bool) -> Self {
        Self::reading(project, drum_instrument, true)
    }

    fn reading(project: &Project, drum_instrument: impl Fn(&Track) -> bool, every_track: bool) -> Self {
        let bars = project.meter_map();
        // A song is at least a bar long, and ends on a bar line.
        let length = bars.bar_line_at_or_after(project.length_beats()).max(bars.bar(0).end());
        // For how long a gap is: a length, not a place in the song.
        let bar = project.time_signature.beats_per_bar();
        let mut hits = Hits::default();
        let mut parts = Vec::new();
        // A reference is beside the song: a film is of the song.
        let of_the_song = |track: &&Track| track.kind == TrackKind::Normal && track.reference.is_none();
        for track in project.tracks.iter().filter(of_the_song) {
            let drums = match track.profile.role {
                Some(role) => role == Role::Drums,
                None => drum_instrument(track),
            };
            if let Some(mut part) = read_part(track, drums, AWAY_AFTER_BARS * bar, every_track) {
                if drums && !track.mute {
                    read_hits(track, &part, &mut hits);
                }
                part.envelopes = envelopes_of(project, track);
                parts.push(part);
            }
        }
        for list in [&mut hits.kicks, &mut hits.snares, &mut hits.crashes] {
            list.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        }
        let sections = match project.markers.is_empty() {
            false => marked_sections(project, length),
            true => found_sections(&parts, length, &bars),
        };
        Self {
            name: project.name.clone(),
            tempo: project.tempo,
            signature: project.time_signature,
            time: Time::of(project),
            bars,
            changes: changes_of(project),
            length,
            sections,
            parts,
            hits,
        }
    }

    /// The score with the recordings its parts play, as
    /// [`voiced`](crate::voice::voiced) found them.
    pub fn with_voices(mut self, voices: Vec<(TrackId, Vec<Voiced>)>) -> Self {
        for (track, voiced) in voices {
            if let Some(part) = self.parts.iter_mut().find(|part| part.track == track) {
                part.voiced = voiced;
            }
        }
        self
    }

    /// Seconds from the start of the song to `beat`.
    pub fn seconds(&self, beat: Beats) -> f64 {
        self.time.seconds(beat)
    }

    /// The beat the song is at `seconds` in.
    pub fn beat_at(&self, seconds: f64) -> Beats {
        self.time.beat_at(seconds)
    }

    /// How long the song is, in seconds.
    pub fn duration(&self) -> f64 {
        self.seconds(self.length)
    }

    /// How long a whole bar is in the time signature the song has at
    /// `beat`, in beats.
    pub fn bar_length(&self, beat: Beats) -> Beats {
        self.bars.signature_at(beat).beats_per_bar()
    }

    /// The number of the bar `beat` is in, from 1.
    pub fn bar_number(&self, beat: Beats) -> u32 {
        self.bars.bar_at(beat).number()
    }

    /// How many bars the song has.
    pub fn bar_count(&self) -> u32 {
        self.bars.bar_at((self.length - 1e-6).max(0.0)).number()
    }

    /// The bar line nearest to `beat`; half way between two, the later.
    pub fn bar_line_near(&self, beat: Beats) -> Beats {
        let bar = self.bars.bar_at(beat);
        if beat - bar.start < bar.end() - beat { bar.start } else { bar.end() }
    }

    /// The change of tempo or time signature that the song last came to
    /// at or before `beat`.
    pub fn change_before(&self, beat: Beats) -> Option<&Change> {
        self.changes.iter().rev().find(|change| change.beat <= beat + 1e-9)
    }

    /// Which section `beat` is in: the first before the song, the last
    /// after it.
    pub fn section_at(&self, beat: Beats) -> usize {
        self.sections.partition_point(|section| section.end <= beat).min(self.sections.len().saturating_sub(1))
    }

    pub fn part(&self, track: TrackId) -> Option<&Part> {
        self.parts.iter().find(|part| part.track == track)
    }

    pub fn part_index(&self, track: TrackId) -> Option<usize> {
        self.parts.iter().position(|part| part.track == track)
    }
}

/// What a track plays, or `None` for one with no clips.
fn read_part(track: &Track, drums: bool, away: Beats, empty_too: bool) -> Option<Part> {
    if track.clips.is_empty() && !empty_too {
        return None;
    }
    let mut notes: Vec<Note> = Vec::new();
    if !track.mute {
        for clip in track.clips.iter().filter(|clip| !clip.muted) {
            // On a drum track every key is a drum; on another, the keys
            // that switch articulations sound nothing and are not shown.
            notes.extend(clip.timeline_notes().into_iter().filter(|note| drums || is_music(track, note.pitch)));
        }
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    let longest = notes.iter().map(|note| note.length).fold(0.0, f64::max);
    let low = notes.iter().map(|note| note.pitch).min().unwrap_or(60);
    let high = notes.iter().map(|note| note.pitch).max().unwrap_or(60);
    let mut rows = Vec::new();
    if drums {
        let mut keys: Vec<u8> = notes.iter().map(|note| note.pitch).collect();
        keys.sort_unstable();
        keys.dedup();
        rows = keys.into_iter().map(|pitch| DrumRow { pitch, name: track.profile.drum_name(pitch) }).collect();
    }
    // Where it is there: its notes, and the audio clips it plays.
    let mut sounding: Vec<(Beats, Beats)> = notes.iter().map(|note| (note.start, note.end())).collect();
    if !track.mute {
        let audio = track.clips.iter().filter(|clip| !clip.muted && clip.audio().is_some());
        sounding.extend(audio.map(|clip| (clip.start, clip.end())));
        sounding.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    let mut spans: Vec<(Beats, Beats)> = Vec::new();
    for (start, end) in sounding {
        match spans.last_mut() {
            Some((_, until)) if start - *until < away => *until = until.max(end),
            _ => spans.push((start, end)),
        }
    }
    let mut clips: Vec<ClipSpan> = track
        .clips
        .iter()
        .map(|clip| ClipSpan { id: clip.id, name: clip.name.clone(), start: clip.start, end: clip.end() })
        .collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    Some(Part {
        track: track.id,
        name: track.name.clone(),
        color: track.color,
        drums,
        notes,
        longest,
        clips,
        low,
        high,
        rows,
        spans,
        voiced: Vec::new(),
        envelopes: Vec::new(),
    })
}

/// An envelope is read at most this many times, however long it is.
const ENVELOPE_READINGS: usize = 1600;

/// What the song automates of `track`: its volume, its pan, what it sends
/// to a return and the parameters of its devices, each as a line from its
/// lowest to its highest. Envelopes that are switched off, and ones that never move,
/// are left out.
fn envelopes_of(project: &Project, track: &Track) -> Vec<Envelope> {
    let own = |target: &AutomationTarget| match target {
        AutomationTarget::TrackVolume { track: id }
        | AutomationTarget::TrackPan { track: id }
        | AutomationTarget::Send { track: id, .. } => *id == track.id,
        AutomationTarget::DeviceParam { device, .. } => track.devices.iter().any(|own| own.id == *device),
        AutomationTarget::MasterVolume => false,
    };
    let lanes = project.automation.iter().filter(|lane| lane.enabled && lane.points.len() >= 2 && own(&lane.target));
    lanes
        .filter_map(|lane| {
            let (first, last) = (lane.points.first()?.beat, lane.points.last()?.beat);
            // Read every sixteenth, or less often where that would be too many.
            let step = ((last - first) / ENVELOPE_READINGS as f64).max(0.25);
            let count = ((last - first) / step).ceil() as usize;
            let read: Vec<(Beats, f64)> = (0..=count)
                .map(|index| (first + index as f64 * step).min(last))
                .map(|beat| (beat, automation_value(&lane.points, beat)))
                .collect();
            let low = read.iter().map(|(_, value)| *value).fold(f64::INFINITY, f64::min);
            let high = read.iter().map(|(_, value)| *value).fold(f64::NEG_INFINITY, f64::max);
            (high > low).then(|| Envelope {
                line: read.iter().map(|(beat, value)| (*beat, ((value - low) / (high - low)) as f32)).collect(),
            })
        })
        .collect()
}

/// Whether a key of a pitched track plays a note: not a keyswitch, and not
/// one of the noises it has a name for.
fn is_music(track: &Track, pitch: u8) -> bool {
    !track.profile.is_keyswitch(pitch) && track.profile.sound_name(pitch).is_none()
}

/// What kind of hit a drum is, by the name its kit or General MIDI has for
/// it: `(kicks, snares, crashes)`, and how much of its velocity counts.
fn hit_kind(name: &str) -> Option<(usize, f32)> {
    let name = name.to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));
    if starts(&["kick", "bd", "bass_drum"]) {
        Some((0, 1.0))
    } else if starts(&["snare", "sd", "rimshot", "clap", "flam"]) {
        Some((1, 1.0))
    } else if starts(&["rim", "sidestick", "side_stick", "cross_stick"]) {
        // A stick across the rim is a snare played quietly.
        Some((1, 0.5))
    } else if starts(&["crash", "china", "splash", "cymbal"]) {
        Some((2, 1.0))
    } else {
        None
    }
}

fn read_hits(track: &Track, part: &Part, hits: &mut Hits) {
    let kinds: Vec<Option<(usize, f32)>> = (0..=127u8).map(|pitch| hit_kind(&track.profile.drum_name(pitch))).collect();
    for note in &part.notes {
        let Some((kind, weight)) = kinds[note.pitch as usize] else { continue };
        if kind == 1 && note.velocity < GHOST_VELOCITY {
            continue;
        }
        let hit = Hit { beat: note.start, hard: weight * note.velocity as f32 / 127.0 };
        match kind {
            0 => hits.kicks.push(hit),
            1 => hits.snares.push(hit),
            _ => hits.crashes.push(hit),
        }
    }
}

/// The song from marker to marker. What comes before the first marker is a
/// section without a name.
fn marked_sections(project: &Project, length: Beats) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut markers: Vec<(Beats, &str)> = project
        .markers
        .iter()
        .filter(|marker| marker.beat < length)
        .map(|m| (m.beat.max(0.0), m.name.as_str()))
        .collect();
    markers.sort_by(|a, b| a.0.total_cmp(&b.0));
    if markers.first().is_none_or(|(beat, _)| *beat > 0.0) {
        markers.insert(0, (0.0, ""));
    }
    for (index, (start, name)) in markers.iter().enumerate() {
        let end = markers.get(index + 1).map_or(length, |(beat, _)| *beat);
        if end > *start {
            sections.push(Section { name: (*name).to_string(), start: *start, end });
        }
    }
    sections
}

/// Sections for a song without markers: a new one wherever a part comes in
/// or drops out, on the bar, none shorter than four bars or longer than
/// eight; and one wherever the time signature changes, which is a place
/// the song itself marks.
fn found_sections(parts: &[Part], length: Beats, meter: &MeterMap) -> Vec<Section> {
    let all: Vec<Bar> = meter.bars_between(0.0, length).collect();
    let bars = all.len().max(1);
    let present = |index: usize| -> Vec<bool> {
        let (from, to) = all.get(index).map_or((0.0, length), |bar| (bar.start, bar.end()));
        parts.iter().map(|part| part.spans.iter().any(|(start, end)| *start < to && *end > from)).collect()
    };
    let (shortest, longest) = (FOUND_SECTION_BARS.0 as usize, FOUND_SECTION_BARS.1 as usize);
    let mut starts = vec![0usize];
    let mut before = present(0);
    for index in 1..bars {
        let now = present(index);
        let since = index - starts.last().copied().unwrap_or(0);
        let meter_changes = all[index].signature != all[index - 1].signature;
        if (now != before && since >= shortest && bars - index >= shortest) || since >= longest || meter_changes {
            starts.push(index);
        }
        before = now;
    }
    let start_of = |index: usize| all.get(index).map_or(0.0, |bar| bar.start);
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| Section {
            name: String::new(),
            start: start_of(*start),
            end: starts.get(index + 1).map_or(length, |next| start_of(*next)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::{Articulation, InstrumentProfile};

    fn note(pitch: u8, start: Beats, length: Beats) -> Note {
        Note { pitch, start, length, velocity: 100, ..Default::default() }
    }

    /// A track with one clip from `start`, `length` beats long, playing `notes`.
    fn track_with(project: &mut Project, name: &str, start: Beats, length: Beats, notes: &[Note]) -> TrackId {
        let track = project.add_track(name);
        let clip = project.add_midi_clip(track, name, start, length).unwrap();
        project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
        track
    }

    #[test]
    fn a_part_is_the_notes_its_clips_sound_where_they_sound() {
        let mut project = Project::new("song");
        let track = track_with(&mut project, "Lead", 8.0, 8.0, &[note(60, 0.0, 1.0), note(64, 2.0, 1.0)]);
        // A second clip, looped: its one note is heard four times.
        let looped = project.add_midi_clip(track, "loop", 16.0, 8.0).unwrap();
        project.clip_mut(looped).unwrap().add_notes(&[note(67, 0.0, 0.5)]).unwrap();
        project.clip_mut(looped).unwrap().set_loop_length(Some(2.0)).unwrap();
        // A track without clips is no part; a return never is.
        project.add_track("Empty");
        project.add_track_of_kind("Hall", TrackKind::Return);

        let score = Score::read(&project);
        assert_eq!(score.parts.len(), 1);
        let part = &score.parts[0];
        let starts: Vec<Beats> = part.notes.iter().map(|note| note.start).collect();
        assert_eq!(starts, [8.0, 10.0, 16.0, 18.0, 20.0, 22.0]);
        assert_eq!((part.low, part.high, part.drums), (60, 67, false));
        assert_eq!(part.clips.len(), 2);
        assert_eq!(score.length, 24.0);
        // One stretch: the silence between the notes is under two bars.
        assert_eq!(part.spans, [(8.0, 22.5)]);
        assert!(part.present(12.0) && !part.present(4.0));
        let sounding: Vec<u8> = part.notes_between(10.5, 16.2).map(|note| note.pitch).collect();
        assert_eq!(sounding, [64, 67], "the note that began at 10 still sounds at 10.5");
    }

    #[test]
    fn muted_clips_and_keyswitches_are_not_music() {
        let mut project = Project::new("song");
        let track = track_with(&mut project, "Guitar", 0.0, 8.0, &[note(24, 0.0, 0.1), note(52, 0.0, 2.0)]);
        let profile =
            InstrumentProfile { articulations: vec![Articulation::key("open", 24).as_default()], ..Default::default() };
        project.set_track_profile(track, profile).unwrap();
        let muted = project.add_midi_clip(track, "muted", 8.0, 8.0).unwrap();
        project.clip_mut(muted).unwrap().add_notes(&[note(55, 0.0, 1.0)]).unwrap();
        project.clip_mut(muted).unwrap().muted = true;

        let part = &Score::read(&project).parts[0];
        assert_eq!(part.notes.len(), 1, "the keyswitch and the muted clip's note are left out");
        assert_eq!((part.low, part.high), (52, 52));
        assert_eq!(part.clips.len(), 2, "the muted clip is still on the lane");
    }

    #[test]
    fn drum_hits_are_found_by_the_names_of_the_kit() {
        let mut project = Project::new("song");
        let hits = [
            note(36, 0.0, 0.1),
            note(38, 1.0, 0.1),
            Note { velocity: 30, ..note(38, 1.5, 0.1) },
            note(42, 2.0, 0.1),
            note(86, 4.0, 0.1),
            note(49, 8.0, 0.1),
        ];
        let track = track_with(&mut project, "Drums", 0.0, 16.0, &hits);
        // This kit keeps a cymbal on a key General MIDI has no drum on.
        let profile = InstrumentProfile {
            role: Some(Role::Drums),
            sounds: [("cymbal_1".to_string(), 86)].into_iter().collect(),
            ..Default::default()
        };
        project.set_track_profile(track, profile).unwrap();

        let score = Score::read(&project);
        let part = &score.parts[0];
        assert!(part.drums);
        let rows: Vec<(u8, &str)> = part.rows.iter().map(|row| (row.pitch, row.name.as_str())).collect();
        assert_eq!(rows, [(36, "kick"), (38, "snare"), (42, "closed_hat"), (49, "crash"), (86, "cymbal_1")]);
        assert_eq!(score.hits.kicks.iter().map(|hit| hit.beat).collect::<Vec<_>>(), [0.0]);
        assert_eq!(score.hits.snares.iter().map(|hit| hit.beat).collect::<Vec<_>>(), [1.0], "not the ghost note");
        assert_eq!(score.hits.crashes.iter().map(|hit| hit.beat).collect::<Vec<_>>(), [4.0, 8.0]);
        assert!((score.hits.kicks[0].hard - 100.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn sections_run_from_marker_to_marker_and_cover_the_song() {
        let mut project = Project::new("song");
        track_with(&mut project, "A", 0.0, 30.0, &[note(60, 0.0, 1.0)]);
        project.add_marker(8.0, "Verse").unwrap();
        project.add_marker(24.0, "Chorus").unwrap();
        let score = Score::read(&project);
        let found: Vec<(&str, Beats, Beats)> =
            score.sections.iter().map(|section| (section.name.as_str(), section.start, section.end)).collect();
        assert_eq!(found, [("", 0.0, 8.0), ("Verse", 8.0, 24.0), ("Chorus", 24.0, 32.0)]);
        assert_eq!(score.length, 32.0, "taken up to the bar");
        assert_eq!((score.section_at(0.0), score.section_at(8.0), score.section_at(23.9)), (0, 1, 1));
        assert_eq!((score.section_at(-4.0), score.section_at(99.0)), (0, 2));
    }

    #[test]
    fn a_song_without_markers_is_cut_where_parts_come_and_go() {
        let mut project = Project::new("song");
        let steady: Vec<Note> = (0..64).map(|beat| note(40, beat as f64, 0.5)).collect();
        track_with(&mut project, "Bass", 0.0, 64.0, &steady);
        // Comes in at bar 5 and stays for four bars.
        let entering: Vec<Note> = (0..16).map(|beat| note(72, beat as f64, 0.5)).collect();
        track_with(&mut project, "Lead", 16.0, 16.0, &entering);
        let score = Score::read(&project);
        let cuts: Vec<Beats> = score.sections.iter().map(|section| section.start).collect();
        // Where it comes in, where it has gone, and no stretch longer than
        // eight bars.
        assert_eq!(cuts, [0.0, 16.0, 32.0]);
        assert_eq!(score.sections.last().unwrap().end, 64.0);
        assert!(score.sections.iter().all(|section| section.name.is_empty()));
    }

    #[test]
    fn what_the_song_automates_of_a_part_is_a_line_from_its_lowest_to_its_highest() {
        use trill_core::{AutomationPoint, AutomationTarget};
        let mut project = Project::new("song");
        let track = track_with(&mut project, "Pad", 0.0, 16.0, &[note(60, 0.0, 16.0)]);
        let other = track_with(&mut project, "Bass", 0.0, 16.0, &[note(36, 0.0, 16.0)]);
        let volume = AutomationTarget::TrackVolume { track };
        let swell =
            vec![AutomationPoint::new(4.0, -24.0), AutomationPoint::new(8.0, 0.0), AutomationPoint::new(12.0, -12.0)];
        project.set_automation(volume.clone(), swell).unwrap();
        // One that never moves, and the master's, are no part's line.
        let flat = vec![AutomationPoint::new(0.0, 0.2), AutomationPoint::new(8.0, 0.2)];
        project.set_automation(AutomationTarget::TrackPan { track: other }, flat).unwrap();
        let master = vec![AutomationPoint::new(0.0, -6.0), AutomationPoint::new(8.0, 0.0)];
        project.set_automation(AutomationTarget::MasterVolume, master).unwrap();

        let score = Score::read(&project);
        let [envelope] = score.part(track).unwrap().envelopes.as_slice() else { panic!("one envelope") };
        assert!(score.part(other).unwrap().envelopes.is_empty());
        assert_eq!((envelope.at(4.0), envelope.at(8.0), envelope.at(12.0)), (0.0, 1.0, 0.5));
        assert!((envelope.at(6.0) - 0.5).abs() < 1e-6, "halfway up the swell");
        assert_eq!((envelope.at(0.0), envelope.at(99.0)), (0.0, 0.5), "held before it and after it");
        // Switched off, it is not shown.
        project.set_automation_enabled(&volume, false).unwrap();
        assert!(Score::read(&project).part(track).unwrap().envelopes.is_empty());
        // What a part sends to a return is its own line too, and not the return's.
        let echo = project.add_track_of_kind("Echo", trill_core::TrackKind::Return);
        let throw = vec![AutomationPoint::new(8.0, -60.0), AutomationPoint::new(12.0, -6.0)];
        project.set_automation(AutomationTarget::Send { track: other, to: echo }, throw).unwrap();
        let score = Score::read(&project);
        let [sent] = score.part(other).unwrap().envelopes.as_slice() else { panic!("the send's envelope") };
        assert_eq!((sent.at(8.0), sent.at(12.0)), (0.0, 1.0));
        assert!(score.part(track).unwrap().envelopes.is_empty());
    }

    #[test]
    fn a_track_of_audio_clips_is_a_part_that_is_there_while_they_play() {
        use trill_core::project::AudioContent;
        let mut project = Project::new("song");
        let track = project.add_track("Voice");
        project.add_audio_clip(track, "Take", 16.0, 8.0, AudioContent::new("take.wav")).unwrap();
        let score = Score::read(&project);
        let part = score.part(track).unwrap();
        assert!(part.notes.is_empty() && part.voiced.is_empty());
        assert_eq!(part.spans, [(16.0, 24.0)]);
        assert!(part.present(20.0) && !part.present(8.0));
        // Its picture is the outline of what it plays, once that is read.
        let wave = std::sync::Arc::new(crate::voice::Wave::of(&[0.0, 0.5], &[0.0, 0.5], 2.0));
        let voices = crate::voice::voiced(&project, |_| Some(wave.clone()));
        let score = score.with_voices(voices);
        assert_eq!(score.part(track).unwrap().voiced.len(), 1);
    }

    #[test]
    fn time_and_beats_convert_at_the_song_tempo() {
        let mut project = Project::new("song");
        project.set_tempo(150.0).unwrap();
        track_with(&mut project, "A", 0.0, 8.0, &[note(60, 0.0, 1.0)]);
        let score = Score::read(&project);
        assert_eq!(score.seconds(5.0), 2.0);
        assert_eq!(score.beat_at(2.0), 5.0);
        assert_eq!(score.duration(), 3.2);
        assert!(score.time.is_constant() && score.changes.is_empty());
        assert_eq!((score.bar_count(), score.bar_number(7.9), score.bar_length(3.0)), (2, 2, 4.0));
    }

    /// The song the tempo work proves itself on: two bars of 4/4 at 120,
    /// two of 7/8 at 140, and back for two.
    fn changing() -> Project {
        let mut project = Project::new("changing");
        project.set_tempo(120.0).unwrap();
        track_with(&mut project, "A", 0.0, 23.0, &[note(60, 0.0, 1.0)]);
        project.set_time_signature_at(8.0, 7, 8).unwrap();
        project.set_tempo_at(8.0, 140.0, false).unwrap();
        project.set_time_signature_at(15.0, 4, 4).unwrap();
        project.set_tempo_at(15.0, 120.0, false).unwrap();
        project
    }

    #[test]
    fn a_song_that_changes_tempo_and_meter_is_timed_and_counted_through_its_changes() {
        let score = Score::read(&changing());
        // Eight beats at 120 are four seconds; the seven at 140 are three.
        assert_eq!((score.seconds(8.0), score.seconds(15.0), score.seconds(23.0)), (4.0, 7.0, 11.0));
        assert_eq!((score.beat_at(4.0), score.beat_at(7.0)), (8.0, 15.0));
        assert!((score.beat_at(5.5) - 11.5).abs() < 1e-9, "a second and a half into the 7/8, at 140");
        assert_eq!(score.duration(), 11.0);
        // A length of time lasts what the tempo under it says, across a change.
        assert!((score.time.between(7.0, 9.0) - (0.5 + 60.0 / 140.0)).abs() < 1e-12);
        assert!((score.time.before(9.0, 0.5 + 60.0 / 140.0) - 7.0).abs() < 1e-9);
        assert_eq!(
            (score.time.tempo_at(7.99), score.time.tempo_at(8.0), score.time.tempo_at(15.0)),
            (120.0, 140.0, 120.0)
        );
        // Bars: two of four beats, two of three and a half, two of four.
        assert_eq!(score.length, 23.0);
        assert_eq!(score.bar_count(), 6);
        assert_eq!([0.0, 7.9, 8.0, 11.4, 11.5, 15.0, 22.0].map(|beat| score.bar_number(beat)), [1, 2, 3, 3, 4, 5, 6]);
        assert_eq!((score.bar_length(0.0), score.bar_length(9.0), score.bar_length(15.0)), (4.0, 3.5, 4.0));
        assert_eq!((score.bar_line_near(9.5), score.bar_line_near(10.0)), (8.0, 11.5));
        // The changes, as they are said.
        let said: Vec<(Beats, String)> = score.changes.iter().map(|change| (change.beat, change.said())).collect();
        assert_eq!(said, [(8.0, "7/8 \u{00B7} 140 BPM".to_owned()), (15.0, "4/4 \u{00B7} 120 BPM".to_owned())]);
        assert_eq!(score.change_before(7.9), None);
        assert_eq!(score.change_before(8.0).map(|change| change.beat), Some(8.0));
        assert_eq!(score.change_before(20.0).map(|change| change.beat), Some(15.0));
        // Without markers, a section begins where the time signature does.
        let starts: Vec<Beats> = score.sections.iter().map(|section| section.start).collect();
        assert_eq!(starts, [0.0, 8.0, 15.0]);
    }

    #[test]
    fn a_tempo_the_song_glides_to_is_said_with_an_arrow_and_timed_along_the_glide() {
        let mut project = Project::new("gliding");
        project.set_tempo(120.0).unwrap();
        track_with(&mut project, "A", 0.0, 16.0, &[note(60, 0.0, 1.0)]);
        project.set_tempo_at(4.0, 120.0, false).unwrap();
        project.set_tempo_at(8.0, 60.0, true).unwrap();
        let score = Score::read(&project);
        let glide = score.changes.last().unwrap();
        assert_eq!((glide.said().as_str(), glide.glides_from), ("\u{2192}60 BPM", Some(4.0)));
        assert_eq!((score.time.tempo_at(4.0), score.time.tempo_at(6.0), score.time.tempo_at(8.0)), (120.0, 90.0, 60.0));
        // Slower all the way: the four beats of the glide take longer than at
        // 120 and less long than at 60.
        let glided = score.time.between(4.0, 8.0);
        assert!(glided > 2.0 && glided < 4.0, "{glided}");
        assert!((score.beat_at(score.seconds(6.5)) - 6.5).abs() < 1e-9);
    }
}
