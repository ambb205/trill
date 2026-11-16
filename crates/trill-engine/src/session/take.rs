//! A take: what is played on the armed tracks while recording, on its way
//! to becoming notes in clips.
//!
//! The audio thread reports what was played and where the song was heard
//! to be ([`Captured`]). A take turns those key presses into notes and
//! writes them into the project as they are completed. Nothing here knows
//! about threads or the engine: reports go in, a project is edited.
//!
//! # Where a take is written
//!
//! * A take goes into a **new clip** on each armed track that something
//!   was played on. The clip starts on the bar line at or before the place
//!   recording began and ends on the bar line after it stopped.
//! * Unless recording began inside a MIDI clip that plays straight through
//!   (it has no loop of its own): then the notes are added **to that
//!   clip**, which is lengthened if the take runs past its end. Recording
//!   over a part overdubs it; undoing the take takes the new notes out.
//! * With the song looping, the clip covers the loop and every pass adds
//!   to it. A key still down when the loop comes round ends at the loop's
//!   end.
//! * A key still down when recording stops ends there.
//! * The sustain pedal is written into the notes: a key let go with the
//!   pedal down lasts until the pedal comes up, as it sounded. Clips have
//!   nowhere to keep the pedal itself, nor the pitch and mod wheels: those
//!   are heard while playing and not recorded.
//! * A note played a little early, just before recording begins, is kept
//!   and starts with the take; what was played earlier in the count-in is
//!   not.

use crate::live::Captured;
use trill_core::device::EventKind;
use trill_core::project::{ClipContent, Note};
use trill_core::{ClipId, EditError, Project, TrackId, TrackKind};

/// How long before the take a note may begin and still be kept, in beats.
const EARLY_BEATS: f64 = 0.25;
/// The shortest note a take writes, in beats: a key cannot be tapped to
/// nothing.
const SHORTEST_NOTE: f64 = 1.0 / 64.0;
/// Slack for positions that should be equal and were computed differently.
const EPSILON: f64 = 1e-9;

/// A clip a take wrote into.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeClip {
    pub track: TrackId,
    pub clip: ClipId,
    /// How many notes the take added.
    pub notes: usize,
    /// Whether the take made the clip (`true`) or added to one that was
    /// there (`false`).
    pub created: bool,
}

/// A key that is down, or held by the pedal.
#[derive(Copy, Clone, Debug)]
struct Sounding {
    pitch: u8,
    velocity: u8,
    /// Where it was heard to begin, and in which pass of the loop.
    start: f64,
    pass: u32,
    /// False once the key is up and only the pedal holds the note.
    key_down: bool,
}

/// A finished note at its place on the timeline.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Played {
    pitch: u8,
    velocity: u8,
    start: f64,
    end: f64,
}

struct TrackTake {
    track: TrackId,
    sounding: Vec<Sounding>,
    pedal: bool,
    /// Finished and not yet in the project.
    finished: Vec<Played>,
    clip: Option<ClipId>,
    created: bool,
    written: usize,
    /// The end of the last note written, for sizing the clip.
    last_end: f64,
}

pub(crate) struct Take {
    /// What the audio thread calls this take; reports of other takes are
    /// not for it.
    id: u32,
    tracks: Vec<TrackTake>,
    /// Set once the audio thread has taken the take up.
    begun: bool,
    /// Where the song was when it started rolling; `None` in the count-in.
    start: Option<f64>,
    /// The loop, if the song was looping when the take was started.
    loop_region: Option<(f64, f64)>,
    /// One bar, in beats.
    bar: f64,
    /// The latest pass of the loop heard of.
    pass: u32,
    /// The furthest the song was heard to get.
    furthest: f64,
    /// Where recording was heard to stop.
    stopped: Option<f64>,
}

impl Take {
    pub fn new(id: u32, tracks: &[TrackId], loop_region: Option<(f64, f64)>, bar: f64) -> Self {
        Self {
            id,
            tracks: tracks
                .iter()
                .map(|&track| TrackTake {
                    track,
                    sounding: Vec::new(),
                    pedal: false,
                    finished: Vec::new(),
                    clip: None,
                    created: false,
                    written: 0,
                    last_end: 0.0,
                })
                .collect(),
            begun: false,
            start: None,
            loop_region,
            bar: if bar.is_finite() && bar > 0.0 { bar } else { 4.0 },
            pass: 0,
            furthest: 0.0,
            stopped: None,
        }
    }

    pub fn tracks(&self) -> Vec<TrackId> {
        self.tracks.iter().map(|track| track.track).collect()
    }

    /// Where the song started rolling, once it has.
    pub fn start(&self) -> Option<f64> {
        self.start
    }

    pub fn pass(&self) -> u32 {
        self.pass
    }

    /// Whether the audio thread has said the take is over.
    pub fn is_stopped(&self) -> bool {
        self.stopped.is_some()
    }

    /// How many notes have been played to the end so far.
    pub fn note_count(&self) -> usize {
        self.tracks.iter().map(|track| track.written + track.finished.len()).sum()
    }

    /// The clips written into so far.
    pub fn clips(&self) -> Vec<TakeClip> {
        self.tracks
            .iter()
            .filter_map(|track| {
                let clip = track.clip?;
                Some(TakeClip { track: track.track, clip, notes: track.written, created: track.created })
            })
            .collect()
    }

    /// Takes in a report from the audio thread.
    pub fn handle(&mut self, captured: Captured) {
        if !self.begun {
            // Whatever comes before this take's beginning is the end of an
            // earlier one.
            self.begun = matches!(captured, Captured::Began { take } if take == self.id);
            return;
        }
        if self.stopped.is_some() {
            return;
        }
        match captured {
            Captured::Began { .. } => {}
            Captured::Rolling { beat } => {
                self.start = Some(beat);
                self.furthest = beat;
                self.settle_early_notes();
            }
            Captured::Wrapped { pass } => self.pass = self.pass.max(pass),
            Captured::Event { track, kind, beat, pass } => {
                self.pass = self.pass.max(pass);
                self.furthest = self.furthest.max(beat);
                let loop_end = self.loop_region.map(|(_, end)| end);
                if let Some(track) = self.tracks.iter_mut().find(|candidate| candidate.track == track) {
                    track.play(kind, beat, pass, loop_end);
                }
            }
            Captured::Stopped { beat, pass } => self.stop_at(beat, pass),
        }
    }

    /// Ends the take at `beat`: every key that is down ends there.
    pub fn stop_at(&mut self, beat: f64, pass: u32) {
        if self.stopped.is_some() {
            return;
        }
        let loop_end = self.loop_region.map(|(_, end)| end);
        for track in &mut self.tracks {
            for sounding in std::mem::take(&mut track.sounding) {
                track.finish(sounding, beat, pass, loop_end);
            }
            track.pedal = false;
        }
        self.furthest = self.furthest.max(beat);
        self.stopped = Some(beat);
    }

    /// Notes that the song has been heard to reach `beat`, so the clips
    /// keep up with it while nothing is played.
    pub fn heard(&mut self, beat: f64) {
        if self.start.is_some() && self.stopped.is_none() && beat.is_finite() {
            self.furthest = self.furthest.max(beat);
        }
    }

    /// Whether [`Take::write`] would change the project.
    pub fn has_news(&self, project: &Project) -> bool {
        let Some(start) = self.start else { return false };
        self.tracks.iter().any(|track| {
            if !track.finished.is_empty() {
                return true;
            }
            let end = track.clip.and_then(|clip| project.clip(clip).ok()).map(|clip| clip.end());
            end.is_some_and(|end| self.wanted_end(track, start) > end + EPSILON)
        })
    }

    /// Writes the notes finished since the last call into the project and
    /// makes the clips long enough for the take so far.
    pub fn write(&mut self, project: &mut Project) -> Result<(), EditError> {
        let Some(start) = self.start else { return Ok(()) };
        let span_start = self.span_start(start);
        for index in 0..self.tracks.len() {
            let track = &mut self.tracks[index];
            // A clip that was deleted meanwhile is not come back to.
            if track.clip.is_some_and(|clip| project.clip(clip).is_err()) {
                track.clip = None;
            }
            if track.finished.is_empty() && track.clip.is_none() {
                continue;
            }
            let recordable = project.track(track.track).is_ok_and(|found| found.kind == TrackKind::Normal);
            if !recordable {
                // The track is gone; so is what was played on it.
                track.finished.clear();
                track.clip = None;
                continue;
            }

            let clip = match track.clip {
                Some(clip) => clip,
                None => {
                    let under = clip_under(project, track.track, span_start, start);
                    let clip = match under {
                        Some(clip) => clip,
                        None => project.add_midi_clip(track.track, "Take", span_start, self.bar)?,
                    };
                    track.clip = Some(clip);
                    track.created = under.is_none();
                    clip
                }
            };

            let (clip_start, clip_end, offset) = {
                let clip = project.clip(clip)?;
                (clip.start, clip.end(), clip.midi().map_or(0.0, |midi| midi.start_offset))
            };
            let notes: Vec<Note> = track
                .finished
                .drain(..)
                .filter(|played| played.start.is_finite() && played.end.is_finite())
                .filter(|played| played.end > clip_start + EPSILON)
                .map(|played| {
                    let begins = played.start.max(clip_start);
                    Note {
                        pitch: played.pitch,
                        start: begins - clip_start + offset,
                        length: (played.end - begins).max(SHORTEST_NOTE),
                        velocity: played.velocity,
                    }
                })
                .collect();
            track.last_end = notes.iter().fold(track.last_end, |end, note| end.max(clip_start + note.end() - offset));
            track.written += notes.len();
            if !notes.is_empty() {
                project.clip_mut(clip)?.add_notes(&notes)?;
            }
            let wanted_end = self.wanted_end(&self.tracks[index], start);
            if wanted_end > clip_end + EPSILON {
                project.set_clip_span(clip, clip_start, wanted_end - clip_start)?;
            }
        }
        Ok(())
    }

    /// Where a new clip for this take starts: the bar line at or before
    /// the take's start, or the start of the loop if that is earlier.
    fn span_start(&self, start: f64) -> f64 {
        let bar_line = ((start + EPSILON) / self.bar).floor() * self.bar;
        match self.looping(start) {
            Some((loop_start, _)) => bar_line.min(loop_start).max(0.0),
            None => bar_line.max(0.0),
        }
    }

    /// The loop, if the take runs inside it.
    fn looping(&self, start: f64) -> Option<(f64, f64)> {
        self.loop_region.filter(|(_, end)| start < *end)
    }

    /// Where `track`'s clip should end by now: the bar line after the
    /// furthest the take has got, or the loop's end once it has come round.
    fn wanted_end(&self, track: &TrackTake, start: f64) -> f64 {
        let reached = self.furthest.max(track.last_end).max(start);
        let bar_line = ((reached - EPSILON) / self.bar).ceil().max(1.0) * self.bar;
        let floor = self.span_start(start) + self.bar;
        match self.looping(start) {
            Some((_, loop_end)) if self.pass > 0 => loop_end.max(floor),
            Some((_, loop_end)) => bar_line.min(loop_end).max(floor.min(loop_end)),
            None => bar_line.max(floor),
        }
    }
}

impl TrackTake {
    fn play(&mut self, kind: EventKind, beat: f64, pass: u32, loop_end: Option<f64>) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => {
                let pitch = pitch.min(127);
                // The same key again: the note that was sounding ends here.
                self.release(pitch, beat, pass, loop_end);
                let velocity = ((velocity * 127.0).round() as i32).clamp(1, 127) as u8;
                self.sounding.push(Sounding { pitch, velocity, start: beat, pass, key_down: true });
            }
            EventKind::NoteOff { pitch } => {
                if self.pedal {
                    for sounding in self.sounding.iter_mut().filter(|sounding| sounding.pitch == pitch) {
                        sounding.key_down = false;
                    }
                } else {
                    self.release(pitch, beat, pass, loop_end);
                }
            }
            EventKind::AllNotesOff => {
                for sounding in std::mem::take(&mut self.sounding) {
                    self.finish(sounding, beat, pass, loop_end);
                }
                self.pedal = false;
            }
            EventKind::Control { .. } => {
                let Some(down) = kind.sustain() else { return };
                self.pedal = down;
                if !down {
                    // The pedal lets go of every note whose key is up.
                    let (held, released): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut self.sounding).into_iter().partition(|sounding| sounding.key_down);
                    self.sounding = held;
                    for sounding in released {
                        self.finish(sounding, beat, pass, loop_end);
                    }
                }
            }
            EventKind::PitchBend { .. } => {}
        }
    }

    /// Ends every sounding note of `pitch` at `beat`.
    fn release(&mut self, pitch: u8, beat: f64, pass: u32, loop_end: Option<f64>) {
        let mut index = 0;
        while index < self.sounding.len() {
            if self.sounding[index].pitch == pitch {
                let sounding = self.sounding.remove(index);
                self.finish(sounding, beat, pass, loop_end);
            } else {
                index += 1;
            }
        }
    }

    fn finish(&mut self, sounding: Sounding, beat: f64, pass: u32, loop_end: Option<f64>) {
        // A note that was sounding when the loop came round ends there.
        let end = match loop_end {
            Some(loop_end) if pass > sounding.pass => loop_end,
            _ => beat,
        };
        self.finished.push(Played {
            pitch: sounding.pitch,
            velocity: sounding.velocity,
            start: sounding.start,
            end: end.max(sounding.start + SHORTEST_NOTE),
        });
    }
}

impl Take {
    /// Drops what was played in the count-in, except a note that began
    /// just ahead of the take, which starts with it. Done once, as the
    /// song starts rolling.
    fn settle_early_notes(&mut self) {
        let Some(start) = self.start else { return };
        for track in &mut self.tracks {
            track.finished.retain_mut(|played| {
                if played.start >= start - EPSILON {
                    return true;
                }
                let kept = played.start >= start - EARLY_BEATS;
                if kept {
                    played.start = start;
                    played.end = played.end.max(start + SHORTEST_NOTE);
                }
                kept
            });
            // The same for keys that are still down.
            track.sounding.retain_mut(|sounding| {
                if sounding.start >= start - EPSILON {
                    return true;
                }
                let kept = sounding.start >= start - EARLY_BEATS;
                if kept {
                    sounding.start = start;
                }
                kept
            });
        }
    }
}

/// The clip on `track` a take starting at `start` is recorded into, if
/// there is one: a MIDI clip that plays straight through, is not muted,
/// holds `start`, and begins no later than a new clip for the take would.
fn clip_under(project: &Project, track: TrackId, span_start: f64, start: f64) -> Option<ClipId> {
    let track = project.track(track).ok()?;
    track
        .clips
        .iter()
        .rev()
        .find(|clip| {
            let plain = matches!(&clip.content, ClipContent::Midi(midi) if midi.loop_length.is_none());
            plain && !clip.muted && clip.start <= span_start + EPSILON && start < clip.end() - EPSILON
        })
        .map(|clip| clip.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: TrackId = TrackId(1);

    fn on(pitch: u8, beat: f64) -> Captured {
        on_in(pitch, beat, 0)
    }

    fn on_in(pitch: u8, beat: f64, pass: u32) -> Captured {
        Captured::Event { track: TRACK, kind: EventKind::NoteOn { pitch, velocity: 100.0 / 127.0 }, beat, pass }
    }

    fn off(pitch: u8, beat: f64) -> Captured {
        off_in(pitch, beat, 0)
    }

    fn off_in(pitch: u8, beat: f64, pass: u32) -> Captured {
        Captured::Event { track: TRACK, kind: EventKind::NoteOff { pitch }, beat, pass }
    }

    fn pedal(down: bool, beat: f64) -> Captured {
        let kind = EventKind::Control { controller: 64, value: if down { 1.0 } else { 0.0 } };
        Captured::Event { track: TRACK, kind, beat, pass: 0 }
    }

    /// A project with one track, and a take on it that has begun rolling
    /// at `start`.
    fn rolling(start: f64, loop_region: Option<(f64, f64)>) -> (Project, Take) {
        let mut project = Project::new("t");
        assert_eq!(project.add_track("Keys"), TRACK);
        let mut take = Take::new(7, &[TRACK], loop_region, 4.0);
        take.handle(Captured::Began { take: 7 });
        take.handle(Captured::Rolling { beat: start });
        (project, take)
    }

    fn write(take: &mut Take, project: &mut Project) {
        take.write(project).unwrap();
    }

    /// The notes of the one clip on the track as `(pitch, start, length)`
    /// on the timeline, and the clip's span.
    fn written(project: &Project) -> (Vec<(u8, f64, f64)>, (f64, f64)) {
        let clips = &project.track(TRACK).unwrap().clips;
        assert_eq!(clips.len(), 1, "one clip");
        let notes = clips[0].timeline_notes().iter().map(|note| (note.pitch, note.start, note.length)).collect();
        (notes, (clips[0].start, clips[0].end()))
    }

    #[test]
    fn notes_land_where_they_were_played_in_a_clip_of_whole_bars() {
        let (mut project, mut take) = rolling(4.0, None);
        for captured in [on(60, 4.0), on(64, 4.5), off(60, 5.0), off(64, 6.25), on(67, 9.5)] {
            take.handle(captured);
        }
        assert!(take.has_news(&project));
        write(&mut take, &mut project);
        // The key that is still down is not a note yet.
        let (notes, span) = written(&project);
        assert_eq!(notes, [(60, 4.0, 1.0), (64, 4.5, 1.75)]);
        assert_eq!(span, (4.0, 12.0), "from the bar line to the bar line past the furthest heard");
        assert!(!take.has_news(&project), "written once");

        take.handle(Captured::Stopped { beat: 10.5, pass: 0 });
        write(&mut take, &mut project);
        let (notes, span) = written(&project);
        assert_eq!(notes, [(60, 4.0, 1.0), (64, 4.5, 1.75), (67, 9.5, 1.0)], "a held key ends where recording stopped");
        assert_eq!(span, (4.0, 12.0));
        assert_eq!(
            take.clips(),
            [TakeClip { track: TRACK, clip: project.tracks[0].clips[0].id, notes: 3, created: true }]
        );
        assert_eq!(project.tracks[0].clips[0].notes()[0].velocity, 100);
    }

    #[test]
    fn a_take_begun_mid_bar_starts_its_clip_on_the_bar_line() {
        let (mut project, mut take) = rolling(5.5, None);
        take.handle(on(60, 6.0));
        take.handle(off(60, 6.5));
        take.handle(Captured::Stopped { beat: 7.0, pass: 0 });
        write(&mut take, &mut project);
        let (notes, span) = written(&project);
        assert_eq!(notes, [(60, 6.0, 0.5)]);
        assert_eq!(span, (4.0, 8.0));
    }

    #[test]
    fn what_was_played_in_the_count_in_is_dropped_but_an_early_downbeat_is_kept() {
        let mut project = Project::new("t");
        project.add_track("Keys");
        let mut take = Take::new(1, &[TRACK], None, 4.0);
        take.handle(Captured::Began { take: 1 });
        // Noodling during the count-in, then the first note a touch early
        // and held into the take.
        take.handle(on(48, -3.0));
        take.handle(off(48, -2.5));
        take.handle(on(60, -0.1));
        take.handle(on(62, -1.0));
        assert!(!take.has_news(&project), "nothing is written before the song rolls");
        take.handle(Captured::Rolling { beat: 0.0 });
        take.handle(off(62, 0.2));
        take.handle(off(60, 1.0));
        take.handle(Captured::Stopped { beat: 2.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(written(&project).0, [(60, 0.0, 1.0)]);
    }

    #[test]
    fn the_pedal_holds_notes_until_it_comes_up() {
        let (mut project, mut take) = rolling(0.0, None);
        for captured in [
            on(60, 0.0),
            pedal(true, 0.5),
            off(60, 1.0),
            on(64, 1.5),
            off(64, 1.75),
            // The same key again while the pedal holds it: the first ends.
            on(64, 2.0),
            pedal(false, 3.0),
            // Its key is still down, so it carries on past the pedal.
            off(64, 3.5),
        ] {
            take.handle(captured);
        }
        take.handle(Captured::Stopped { beat: 4.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(written(&project).0, [(60, 0.0, 3.0), (64, 1.5, 0.5), (64, 2.0, 1.5)]);
    }

    #[test]
    fn repeated_and_tapped_keys_make_separate_notes_with_some_length() {
        let (mut project, mut take) = rolling(0.0, None);
        // A second note-on with no note-off between (two devices on one
        // key), and a key tapped so fast that both edges read the same.
        for captured in [on(60, 0.0), on(60, 1.0), off(60, 1.5), on(72, 2.0), off(72, 2.0)] {
            take.handle(captured);
        }
        take.handle(Captured::Stopped { beat: 3.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(written(&project).0, [(60, 0.0, 1.0), (60, 1.0, 0.5), (72, 2.0, SHORTEST_NOTE)]);
    }

    #[test]
    fn looping_layers_passes_into_one_clip_and_cuts_held_keys_at_the_loop_end() {
        let (mut project, mut take) = rolling(0.0, Some((0.0, 8.0)));
        take.handle(on(60, 1.0));
        take.handle(off(60, 2.0));
        // Held across the loop point: the key comes up in the next pass.
        take.handle(on(48, 7.0));
        take.heard(7.9);
        write(&mut take, &mut project);
        assert_eq!(written(&project).1, (0.0, 8.0), "the first pass grows towards the loop end");

        take.handle(Captured::Wrapped { pass: 1 });
        take.handle(off_in(48, 0.5, 1));
        // A note played just ahead of the loop point is heard, and written,
        // at the end of the pass before.
        take.handle(on_in(72, 7.95, 0));
        take.handle(off_in(72, 0.25, 1));
        take.handle(on_in(64, 1.0, 1));
        take.handle(off_in(64, 3.0, 1));
        take.handle(Captured::Stopped { beat: 4.0, pass: 1 });
        write(&mut take, &mut project);
        let (notes, span) = written(&project);
        assert_eq!(span, (0.0, 8.0));
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[0], (60, 1.0, 1.0));
        assert_eq!(notes[1], (64, 1.0, 2.0), "the second pass is layered onto the first");
        assert_eq!(notes[2], (48, 7.0, 1.0), "cut at the loop end");
        assert_eq!(notes[3].0, 72);
        assert!((notes[3].1 - 7.95).abs() < 1e-9 && (notes[3].2 - 0.05).abs() < 1e-9, "{:?}", notes[3]);
    }

    #[test]
    fn a_take_begun_inside_the_loop_covers_the_loop_from_its_start() {
        let (mut project, mut take) = rolling(6.0, Some((4.0, 12.0)));
        take.handle(on(60, 7.0));
        take.handle(off(60, 7.5));
        take.handle(Captured::Wrapped { pass: 1 });
        take.handle(on_in(62, 4.5, 1));
        take.handle(off_in(62, 5.0, 1));
        take.handle(Captured::Stopped { beat: 5.5, pass: 1 });
        write(&mut take, &mut project);
        let (notes, span) = written(&project);
        assert_eq!(span, (4.0, 12.0));
        assert_eq!(notes, [(62, 4.5, 0.5), (60, 7.0, 0.5)]);
    }

    #[test]
    fn recording_inside_a_clip_adds_to_it_and_lengthens_it() {
        let (mut project, mut take) = rolling(5.0, None);
        // A clip with a trimmed start, holding a note already.
        let clip = project.add_midi_clip(TRACK, "Part", 2.0, 4.0).unwrap();
        project
            .clip_mut(clip)
            .unwrap()
            .add_notes(&[Note { pitch: 40, start: 1.0, length: 0.5, velocity: 90 }])
            .unwrap();
        project.trim_clip_start(clip, 4.0).unwrap();
        assert_eq!(project.clip(clip).unwrap().midi().unwrap().start_offset, 2.0);
        project
            .clip_mut(clip)
            .unwrap()
            .add_notes(&[Note { pitch: 41, start: 2.5, length: 0.5, velocity: 90 }])
            .unwrap();

        take.handle(on(60, 5.0));
        take.handle(off(60, 5.5));
        take.handle(on(62, 8.5));
        take.handle(off(62, 9.0));
        take.handle(Captured::Stopped { beat: 9.5, pass: 0 });
        write(&mut take, &mut project);
        let (notes, span) = written(&project);
        assert_eq!(notes, [(41, 4.5, 0.5), (60, 5.0, 0.5), (62, 8.5, 0.5)], "what was there stays, in its place");
        assert_eq!(span, (4.0, 12.0), "lengthened to the bar line after the take");
        assert_eq!(take.clips(), [TakeClip { track: TRACK, clip, notes: 2, created: false }]);
    }

    #[test]
    fn a_looped_clip_and_a_clip_elsewhere_are_left_alone() {
        let (mut project, mut take) = rolling(4.0, None);
        let looped = project.add_midi_clip(TRACK, "Loop", 4.0, 8.0).unwrap();
        project.clip_mut(looped).unwrap().set_loop_length(Some(1.0)).unwrap();
        let elsewhere = project.add_midi_clip(TRACK, "Later", 16.0, 4.0).unwrap();
        let before = project.clone();

        take.handle(on(60, 4.0));
        take.handle(off(60, 4.5));
        take.handle(Captured::Stopped { beat: 5.0, pass: 0 });
        write(&mut take, &mut project);
        let clips = &project.track(TRACK).unwrap().clips;
        assert_eq!(clips.len(), 3, "a new clip beside them");
        assert_eq!(project.clip(looped).unwrap(), before.clip(looped).unwrap());
        assert_eq!(project.clip(elsewhere).unwrap(), before.clip(elsewhere).unwrap());
        let made = take.clips()[0].clip;
        assert!(take.clips()[0].created);
        assert_eq!(project.clip(made).unwrap().timeline_notes().len(), 1);
    }

    #[test]
    fn nothing_played_makes_no_clip_and_another_takes_reports_are_ignored() {
        let mut project = Project::new("t");
        project.add_track("Keys");
        let mut take = Take::new(3, &[TRACK], None, 4.0);
        // The tail of the take before this one.
        take.handle(on(60, 1.0));
        take.handle(Captured::Stopped { beat: 2.0, pass: 0 });
        assert!(!take.is_stopped());
        take.handle(Captured::Began { take: 2 });
        take.handle(Captured::Stopped { beat: 2.0, pass: 0 });
        assert!(!take.is_stopped());

        take.handle(Captured::Began { take: 3 });
        take.handle(Captured::Rolling { beat: 0.0 });
        take.heard(6.0);
        take.handle(Captured::Stopped { beat: 9.0, pass: 0 });
        assert!(take.is_stopped());
        assert!(!take.has_news(&project));
        write(&mut take, &mut project);
        assert!(project.track(TRACK).unwrap().clips.is_empty());
        assert!(take.clips().is_empty());
        assert_eq!(take.note_count(), 0);
    }

    #[test]
    fn a_track_or_clip_that_goes_away_mid_take_costs_only_its_notes() {
        let mut project = Project::new("t");
        let keys = project.add_track("Keys");
        let bass = project.add_track("Bass");
        let mut take = Take::new(1, &[keys, bass], None, 4.0);
        take.handle(Captured::Began { take: 1 });
        take.handle(Captured::Rolling { beat: 0.0 });
        let play = |take: &mut Take, track, pitch, at: f64| {
            let on = EventKind::NoteOn { pitch, velocity: 1.0 };
            take.handle(Captured::Event { track, kind: on, beat: at, pass: 0 });
            take.handle(Captured::Event { track, kind: EventKind::NoteOff { pitch }, beat: at + 0.5, pass: 0 });
        };
        play(&mut take, keys, 60, 0.0);
        play(&mut take, bass, 36, 0.0);
        write(&mut take, &mut project);
        let first_clip = take.clips()[0].clip;

        // The bass track is deleted and the keys clip with it.
        project.remove_track(bass).unwrap();
        project.remove_clip(first_clip).unwrap();
        play(&mut take, keys, 62, 1.0);
        play(&mut take, bass, 38, 1.0);
        take.handle(Captured::Stopped { beat: 2.0, pass: 0 });
        write(&mut take, &mut project);
        let clips = &project.track(keys).unwrap().clips;
        assert_eq!(clips.len(), 1, "what is played after goes into a new clip");
        assert_eq!(clips[0].timeline_notes().len(), 1);
        assert_eq!(clips[0].notes()[0].pitch, 62);
    }
}
