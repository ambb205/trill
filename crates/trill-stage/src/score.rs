//! What the stage reads of a song: its sections, the notes of each part
//! where they sound, and the drum hits the picture answers on the sample.
//!
//! A [`Score`] is worked out once for a project and read for every frame.
//! It is a pure function of the project (and of which tracks are drums,
//! where the project does not say), so two films of one song are the same
//! film.

use trill_core::{Beats, ClipId, Note, Project, Role, TimeSignature, Track, TrackId, TrackKind};

/// The built-in devices whose notes are drums.
const DRUM_DEVICES: [&str; 2] = ["builtin:drumkit", "builtin:drumrack"];
/// A part is taken to be away once it has been silent for this many bars.
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
    /// Where the part is there: stretches of notes with no silence of two
    /// bars or more inside them.
    pub spans: Vec<(Beats, Beats)>,
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

/// A song as the stage reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub name: String,
    pub tempo: f64,
    pub signature: TimeSignature,
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
        let bar = project.time_signature.beats_per_bar();
        let length = (project.length_beats() / bar).ceil().max(1.0) * bar;
        let mut hits = Hits::default();
        let mut parts = Vec::new();
        for track in project.tracks.iter().filter(|track| track.kind == TrackKind::Normal) {
            let drums = match track.profile.role {
                Some(role) => role == Role::Drums,
                None => drum_instrument(track),
            };
            if let Some(part) = read_part(track, drums, AWAY_AFTER_BARS * bar) {
                if drums && !track.mute {
                    read_hits(track, &part, &mut hits);
                }
                parts.push(part);
            }
        }
        for list in [&mut hits.kicks, &mut hits.snares, &mut hits.crashes] {
            list.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        }
        let sections = match project.markers.is_empty() {
            false => marked_sections(project, length),
            true => found_sections(&parts, length, bar),
        };
        Self {
            name: project.name.clone(),
            tempo: project.tempo,
            signature: project.time_signature,
            length,
            sections,
            parts,
            hits,
        }
    }

    /// Seconds from the start of the song to `beat`.
    pub fn seconds(&self, beat: Beats) -> f64 {
        beat * 60.0 / self.tempo
    }

    /// The beat the song is at `seconds` in.
    pub fn beat_at(&self, seconds: f64) -> Beats {
        seconds * self.tempo / 60.0
    }

    /// How long the song is, in seconds.
    pub fn duration(&self) -> f64 {
        self.seconds(self.length)
    }

    pub fn bar(&self) -> Beats {
        self.signature.beats_per_bar()
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
fn read_part(track: &Track, drums: bool, away: Beats) -> Option<Part> {
    if track.clips.is_empty() {
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
    let mut spans: Vec<(Beats, Beats)> = Vec::new();
    for note in &notes {
        match spans.last_mut() {
            Some((_, end)) if note.start - *end < away => *end = end.max(note.end()),
            _ => spans.push((note.start, note.end())),
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
    })
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
/// eight.
fn found_sections(parts: &[Part], length: Beats, bar: Beats) -> Vec<Section> {
    let bars = (length / bar).round().max(1.0) as usize;
    let present = |index: usize| -> Vec<bool> {
        let (from, to) = (index as f64 * bar, (index + 1) as f64 * bar);
        parts.iter().map(|part| part.spans.iter().any(|(start, end)| *start < to && *end > from)).collect()
    };
    let (shortest, longest) = (FOUND_SECTION_BARS.0 as usize, FOUND_SECTION_BARS.1 as usize);
    let mut starts = vec![0usize];
    let mut before = present(0);
    for index in 1..bars {
        let now = present(index);
        let since = index - starts.last().copied().unwrap_or(0);
        if (now != before && since >= shortest && bars - index >= shortest) || since >= longest {
            starts.push(index);
        }
        before = now;
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| Section {
            name: String::new(),
            start: *start as f64 * bar,
            end: starts.get(index + 1).map_or(length, |next| *next as f64 * bar),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::{Articulation, InstrumentProfile};

    fn note(pitch: u8, start: Beats, length: Beats) -> Note {
        Note { pitch, start, length, velocity: 100 }
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
    fn time_and_beats_convert_at_the_song_tempo() {
        let mut project = Project::new("song");
        project.set_tempo(150.0).unwrap();
        track_with(&mut project, "A", 0.0, 8.0, &[note(60, 0.0, 1.0)]);
        let score = Score::read(&project);
        assert_eq!(score.seconds(5.0), 2.0);
        assert_eq!(score.beat_at(2.0), 5.0);
        assert_eq!(score.duration(), 3.2);
    }
}
