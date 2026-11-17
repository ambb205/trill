//! Writing what was heard as MIDI: the bass line, the sung melody, the drum
//! groove and the chords.
//!
//! Notes are placed by the recording's own beat grid and not by the clock,
//! so a file lines up with a song's bars even when the recording's tempo
//! wanders.

use crate::beats::Grid;
use crate::harmony::Chord;
use crate::hits::GrooveNote;
use crate::pitch::VoiceNote;
use trill_core::midi_file::{MidiFile, MidiTrack};
use trill_core::project::Note;
use trill_core::time::TimeSignature;

/// A note in the recording's own time: seconds.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TimedNote {
    pub start: f64,
    pub end: f64,
    pub note: u8,
    pub velocity: u8,
}

/// The shortest a written note is, in beats: a sixty-fourth note.
const SHORTEST: f64 = 1.0 / 16.0;

/// A one-track MIDI file of `notes`, in beats counted along `grid` from
/// `origin` seconds. `quantize` is the steps to a beat that note starts
/// are moved onto (4 for sixteenths), or 0 to leave the timing as played.
pub fn file(notes: &[TimedNote], grid: &Grid, origin: f64, name: &str, drums: bool, quantize: u32) -> MidiFile {
    let zero = grid.beat_at(origin);
    let mut written: Vec<Note> = notes
        .iter()
        .map(|note| {
            let mut start = grid.beat_at(note.start) - zero;
            if quantize > 0 {
                start = (start * f64::from(quantize)).round() / f64::from(quantize);
            }
            let start = start.max(0.0);
            let end = (grid.beat_at(note.end) - zero).max(start + SHORTEST);
            Note { pitch: note.note.min(127), start, length: end - start, velocity: note.velocity.clamp(1, 127) }
        })
        .collect();
    written.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    MidiFile {
        tempo: Some(if grid.bpm > 0.0 { grid.bpm } else { 120.0 }),
        time_signature: u8::try_from(grid.beats_per_bar).ok().and_then(|beats| TimeSignature::new(beats, 4)),
        tracks: vec![MidiTrack { name: name.to_owned(), drums, notes: written, controllers: Vec::new() }],
        ..MidiFile::default()
    }
}

/// The notes of a voice between `start` and `end` seconds, louder notes
/// struck harder. `loudest` is the level a full strike stands for; left
/// out, it is the loudest note of the stretch.
pub fn melody(found: &[VoiceNote], start: f64, end: f64, loudest: Option<f64>) -> Vec<TimedNote> {
    let inside: Vec<&VoiceNote> = found.iter().filter(|note| note.end > start && note.start < end).collect();
    let Some(top) = loudest.or_else(|| inside.iter().map(|note| note.level).max_by(f64::total_cmp)) else {
        return Vec::new();
    };
    inside
        .into_iter()
        .map(|note| TimedNote {
            start: note.start.max(start),
            end: note.end.min(end),
            note: note.midi,
            velocity: (110.0 + 2.5 * (note.level - top)).clamp(30.0, 120.0) as u8,
        })
        .collect()
}

/// The hits of a groove between `start` and `end` seconds, each a short
/// note on its drum.
pub fn drum_part(groove: &[GrooveNote], start: f64, end: f64) -> Vec<TimedNote> {
    groove
        .iter()
        .filter(|hit| hit.time >= start && hit.time < end)
        .map(|hit| TimedNote { start: hit.time, end: hit.time + 0.08, note: hit.note, velocity: hit.velocity })
        .collect()
}

/// Each chord between `start` and `end` seconds in close position from its
/// root (C3 to B3), with the root again an octave below as the bass.
pub fn chord_part(chords: &[Chord], start: f64, end: f64) -> Vec<TimedNote> {
    let mut notes = Vec::new();
    for chord in chords.iter().filter(|chord| chord.end > start && chord.start < end) {
        let (begin, finish) = (chord.start.max(start), chord.end.min(end));
        let root = 48 + chord.root % 12;
        notes.push(TimedNote { start: begin, end: finish, note: root - 12, velocity: 84 });
        notes.extend(chord.quality.steps().iter().map(|step| TimedNote {
            start: begin,
            end: finish,
            note: root + step,
            velocity: 76,
        }));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harmony::Quality;
    use crate::hits::HitKind;

    fn grid() -> Grid {
        // A recording that slows down: beats half a second apart, then 0.6.
        Grid {
            beats: vec![1.0, 1.5, 2.0, 2.5, 3.1, 3.7, 4.3],
            bars: vec![0, 4],
            bpm: 110.0,
            beats_per_bar: 4,
            ..Grid::default()
        }
    }

    #[test]
    fn notes_are_written_on_the_recordings_own_beats() {
        let notes = [
            TimedNote { start: 1.0, end: 1.5, note: 40, velocity: 100 },
            // Half-way through the fifth beat, which is slower.
            TimedNote { start: 3.4, end: 3.7, note: 43, velocity: 90 },
        ];
        let file = file(&notes, &grid(), 1.0, "Bass line", false, 0);
        assert_eq!(file.tempo, Some(110.0));
        assert_eq!(file.time_signature, TimeSignature::new(4, 4));
        let track = &file.tracks[0];
        assert_eq!((track.name.as_str(), track.drums), ("Bass line", false));
        assert_eq!((track.notes[0].start, track.notes[0].length), (0.0, 1.0));
        assert!((track.notes[1].start - 4.5).abs() < 1e-9 && (track.notes[1].length - 0.5).abs() < 1e-9);
        // And the file is one a song can read back.
        let bytes = trill_core::midi_file::write(&file);
        let read = trill_core::midi_file::parse(&bytes).unwrap();
        assert_eq!(read.tracks[0].notes.len(), 2);
        assert_eq!(read.tracks[0].notes[1].pitch, 43);
    }

    #[test]
    fn starts_are_moved_onto_the_steps_and_ends_are_left_as_played() {
        let notes = [
            TimedNote { start: 1.07, end: 1.4, note: 36, velocity: 100 },
            TimedNote { start: 1.26, end: 1.27, note: 42, velocity: 60 },
        ];
        let file = file(&notes, &grid(), 1.0, "Groove", true, 4);
        let track = &file.tracks[0];
        assert!(track.drums);
        // 0.14 of a beat is nearest the first sixteenth; half a beat is on one.
        assert_eq!(track.notes[0].start, 0.25);
        assert!((track.notes[0].length - 0.55).abs() < 1e-9);
        assert_eq!(track.notes[1].start, 0.5);
        // A note is never shorter than a sixty-fourth.
        assert_eq!(track.notes[1].length, SHORTEST);
        // A note from before the origin starts at it.
        let early =
            super::file(&[TimedNote { start: 0.2, end: 1.2, note: 36, velocity: 100 }], &grid(), 1.0, "Early", true, 0);
        assert_eq!(early.tracks[0].notes[0].start, 0.0);
    }

    #[test]
    fn a_melody_a_groove_and_chords_become_notes() {
        let sung = [
            VoiceNote { start: 0.5, end: 1.0, midi: 64, cents: 3.0, level: -20.0 },
            VoiceNote { start: 1.2, end: 2.4, midi: 67, cents: -8.0, level: -12.0 },
            VoiceNote { start: 5.0, end: 5.5, midi: 60, cents: 0.0, level: -9.0 },
        ];
        let notes = melody(&sung, 0.0, 2.0, None);
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[1].note, notes[1].velocity, notes[1].end), (67, 110, 2.0));
        assert_eq!(notes[0].velocity, 90);
        assert!(melody(&sung, 3.0, 4.0, None).is_empty());

        let hits = [
            GrooveNote { time: 0.1, kind: HitKind::Kick, note: 36, velocity: 120 },
            GrooveNote { time: 2.0, kind: HitKind::Snare, note: 38, velocity: 100 },
        ];
        let notes = drum_part(&hits, 0.0, 2.0);
        assert_eq!(notes.len(), 1);
        assert_eq!((notes[0].note, notes[0].end), (36, 0.1 + 0.08));

        let chords = [Chord { start: 0.0, end: 2.0, root: 9, quality: Quality::MinorSeventh }];
        let notes = chord_part(&chords, 1.0, 3.0);
        assert_eq!(notes.iter().map(|note| note.note).collect::<Vec<_>>(), [45, 57, 60, 64, 67]);
        assert!(notes.iter().all(|note| note.start == 1.0 && note.end == 2.0));
    }
}
