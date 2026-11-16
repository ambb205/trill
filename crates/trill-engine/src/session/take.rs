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
//! * A note played a little early, just before recording begins, is kept
//!   and starts with the take; what was played earlier in the count-in is
//!   not.
//!
//! # Controllers
//!
//! * What is played on the wheels, the pedals and any other controller is
//!   recorded with the notes, as a curve in the same clip for each
//!   controller that moved: thinned to the breakpoints that follow the
//!   gesture (`trill_core::controllers::thin`), a pedal as steps.
//! * **The sustain pedal is recorded as a controller, and notes are as
//!   long as their keys were down.** A note that the pedal held sounds on
//!   in playback because the pedal is played back, as it sounded. Writing
//!   the pedal into the note lengths as well would say the same thing
//!   twice, and the two would stop agreeing at the first edit; writing it
//!   only there would lose what a pedal is to an instrument that listens
//!   to it (half-pedalling, the resonance of a piano with the dampers up).
//! * A take begins with the controllers where the hand had left them: one
//!   that was set before the take (the expression a wind instrument needs
//!   before it makes a sound) is the first point of its curve.
//! * Recording into a clip that has a curve for a controller already
//!   replaces that curve from the first movement of the controller to the
//!   end of the take (or of the loop's pass): for that long the hand's
//!   controller was heard and the clip's was not. Where the controller was
//!   not touched, the clip's curve stays.
//! * A pedal still down, or a wheel still bent, when recording stops is
//!   let go of there.

use crate::live::Captured;
use trill_core::controllers::{self, GESTURE_GAP_SECONDS, THIN_TOLERANCE};
use trill_core::device::EventKind;
use trill_core::project::{AutomationPoint, ClipContent, Note};
use trill_core::{ClipId, ControllerKind, EditError, Project, TrackId, TrackKind};

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
    /// The controllers the take wrote curves for.
    pub controllers: Vec<ControllerKind>,
    /// Whether the take made the clip (`true`) or added to one that was
    /// there (`false`).
    pub created: bool,
}

/// A key that is down.
#[derive(Copy, Clone, Debug)]
struct Sounding {
    pitch: u8,
    velocity: u8,
    /// Where it was heard to begin, and in which pass of the loop.
    start: f64,
    pass: u32,
}

/// Where a controller was put during the take.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Moved {
    /// Where the song was heard to be.
    beat: f64,
    value: f64,
    pass: u32,
}

/// One controller's part in a take.
struct ControllerTake {
    kind: ControllerKind,
    moves: Vec<Moved>,
    /// The clip's curve for the controller as it was before the take first
    /// changed it (empty if it had none); `None` until then.
    base: Option<Vec<AutomationPoint>>,
    /// How many of `moves` the clip's curve reflects.
    written: usize,
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
    controllers: Vec<ControllerTake>,
    /// Whether a controller was moved during the take, which is worth a
    /// clip even without a note. Where it stood as the take began is not.
    moved: bool,
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
    /// How long a controller rests, in beats, before its next movement is
    /// a new gesture.
    gesture_gap: f64,
    /// Set once the controllers' curves are as they stay: written after
    /// the take stopped.
    settled: bool,
}

impl Take {
    /// A take numbered `id` on `tracks`, in a song with bars of `bar` beats
    /// at `tempo`, looping over `loop_region` if it loops.
    pub fn new(id: u32, tracks: &[TrackId], loop_region: Option<(f64, f64)>, bar: f64, tempo: f64) -> Self {
        let tempo = if tempo.is_finite() && tempo > 0.0 { tempo } else { 120.0 };
        Self {
            gesture_gap: GESTURE_GAP_SECONDS * tempo / 60.0,
            settled: false,
            id,
            tracks: tracks
                .iter()
                .map(|&track| TrackTake {
                    track,
                    sounding: Vec::new(),
                    controllers: Vec::new(),
                    moved: false,
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
                let mut controllers: Vec<ControllerKind> =
                    track.controllers.iter().filter(|taken| taken.written > 0).map(|taken| taken.kind).collect();
                controllers.sort();
                Some(TakeClip { track: track.track, clip, notes: track.written, controllers, created: track.created })
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
            Captured::Standing { track, kind, value, pass } => {
                // It stood there as the take began, and so from its start.
                let Some(start) = self.start else { return };
                if let Some(track) = self.tracks.iter_mut().find(|candidate| candidate.track == track) {
                    track.controller(kind).moves.push(Moved { beat: start, value: in_range(kind, value), pass });
                }
            }
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
        }
        self.pass = self.pass.max(pass);
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
            // A controller's curve is written as it moves, once there is
            // or will be a clip for it, and once more when the take stops.
            let has_clip = track.clip.is_some() || track.moved;
            let moving = track.controllers.iter().any(|taken| taken.written != taken.moves.len());
            let unsettled = self.stopped.is_some() && !self.settled && !track.controllers.is_empty();
            if has_clip && (moving || unsettled) {
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
            if track.finished.is_empty() && track.clip.is_none() && !track.moved {
                continue;
            }
            let recordable = project.track(track.track).is_ok_and(|found| found.kind == TrackKind::Normal);
            if !recordable {
                // The track is gone; so is what was played on it.
                track.finished.clear();
                track.controllers.clear();
                track.moved = false;
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
                    // Whatever an earlier clip of this take was given, this
                    // one starts from its own curves.
                    for taken in &mut track.controllers {
                        taken.base = None;
                        taken.written = 0;
                    }
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
            let place = Place {
                clip_start,
                offset,
                pass: self.pass,
                loop_end: self.loop_region.filter(|(_, end)| start < *end).map(|(_, end)| end),
                stopped: self.stopped,
                gesture_gap: self.gesture_gap,
            };
            for taken in &mut track.controllers {
                if taken.written == taken.moves.len() && self.stopped.is_none() {
                    continue;
                }
                let clip = project.clip_mut(clip)?;
                let had = clip.controller(taken.kind).map(|curve| curve.points.clone()).unwrap_or_default();
                let base = taken.base.take().unwrap_or(had);
                let curve = taken.curve(&base, &place);
                taken.base = Some(base);
                clip.set_controller(taken.kind, curve)?;
                taken.written = taken.moves.len();
            }
            let wanted_end = self.wanted_end(&self.tracks[index], start);
            if wanted_end > clip_end + EPSILON {
                project.set_clip_span(clip, clip_start, wanted_end - clip_start)?;
            }
        }
        self.settled = self.stopped.is_some();
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

/// A controller's value as a curve may hold it.
fn in_range(kind: ControllerKind, value: f32) -> f64 {
    let (low, high) = kind.range();
    if value.is_finite() { f64::from(value).clamp(low, high) } else { low.max(0.0) }
}

/// Where a take's controller movements go, for making them a curve.
struct Place {
    /// The clip's start on the timeline and where in its content it starts.
    clip_start: f64,
    offset: f64,
    /// The latest pass of the loop heard of.
    pass: u32,
    /// The end of the loop, if the take runs inside it.
    loop_end: Option<f64>,
    /// Where recording stopped, once it has.
    stopped: Option<f64>,
    gesture_gap: f64,
}

impl ControllerTake {
    /// The clip's curve for this controller with the take's movements
    /// written into `base`, what it had before the take.
    ///
    /// Each pass of the loop is a gesture of its own, written over what
    /// was there from its first movement to the end of the pass (or to
    /// where recording stopped): that is how long the hand's controller
    /// was heard in place of the clip's.
    fn curve(&self, base: &[AutomationPoint], place: &Place) -> Vec<AutomationPoint> {
        let content = |beat: f64| (beat - place.clip_start + place.offset).max(0.0);
        let mut passes: Vec<u32> = self.moves.iter().map(|moved| moved.pass).collect();
        passes.sort_unstable();
        passes.dedup();
        let mut curve = base.to_vec();
        for pass in passes {
            let mut events: Vec<(f64, f64)> = self
                .moves
                .iter()
                .filter(|moved| moved.pass == pass)
                .map(|moved| (content(moved.beat), moved.value))
                .collect();
            let Some(&(from, _)) = events.first() else { continue };
            // Where the gesture stops being heard: the loop came round, or
            // recording stopped. Until either, it runs to the end.
            let end = match (place.loop_end, place.stopped) {
                (Some(loop_end), _) if pass < place.pass => Some(loop_end),
                (_, Some(stopped)) => Some(stopped),
                _ => None,
            };
            let to = end.map(|end| content(end).max(from));
            // A pedal still down, or a wheel still bent, when recording
            // stops is let go of there.
            if let (Some(rest), Some(to), Some(last)) =
                (self.kind.rest(), to.filter(|_| pass == place.pass), events.last())
                && last.1 != rest
                && place.stopped.is_some()
            {
                events.push((to, rest));
            }
            let piece = controllers::thin(&events, self.kind.is_switch(), THIN_TOLERANCE, place.gesture_gap);
            curve = controllers::splice(&curve, from, to, &piece);
        }
        curve
    }
}

impl TrackTake {
    /// This track's take of `kind`, begun if need be.
    fn controller(&mut self, kind: ControllerKind) -> &mut ControllerTake {
        let index = match self.controllers.iter().position(|taken| taken.kind == kind) {
            Some(index) => index,
            None => {
                self.controllers.push(ControllerTake { kind, moves: Vec::new(), base: None, written: 0 });
                self.controllers.len() - 1
            }
        };
        &mut self.controllers[index]
    }

    fn play(&mut self, kind: EventKind, beat: f64, pass: u32, loop_end: Option<f64>) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => {
                let pitch = pitch.min(127);
                // The same key again: the note that was sounding ends here.
                self.release(pitch, beat, pass, loop_end);
                let velocity = ((velocity * 127.0).round() as i32).clamp(1, 127) as u8;
                self.sounding.push(Sounding { pitch, velocity, start: beat, pass });
            }
            // A note is as long as its key was down. If the pedal held it
            // longer, the pedal's own curve says so.
            EventKind::NoteOff { pitch } => self.release(pitch, beat, pass, loop_end),
            EventKind::AllNotesOff => {
                for sounding in std::mem::take(&mut self.sounding) {
                    self.finish(sounding, beat, pass, loop_end);
                }
            }
            EventKind::Control { .. } | EventKind::PitchBend { .. } | EventKind::ChannelPressure { .. } => {
                let Some((controller, value)) = ControllerKind::of_event(kind) else { return };
                if beat.is_finite() {
                    self.moved = true;
                    self.controller(controller).moves.push(Moved { beat, value: in_range(controller, value), pass });
                }
            }
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
            // Where the controllers were moved to during the count-in is
            // reported as the song starts rolling: where each stands then.
            for taken in &mut track.controllers {
                taken.moves.retain(|moved| moved.beat >= start - EPSILON);
            }
            track.controllers.retain(|taken| !taken.moves.is_empty());
            track.moved = !track.controllers.is_empty();
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
    /// A note as written: pitch, start and length on the timeline.
    type Written = (u8, f64, f64);

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
        let mut take = Take::new(7, &[TRACK], loop_region, 4.0, 120.0);
        take.handle(Captured::Began { take: 7 });
        take.handle(Captured::Rolling { beat: start });
        (project, take)
    }

    fn write(take: &mut Take, project: &mut Project) {
        take.write(project).unwrap();
    }

    /// The notes of the one clip on the track as `(pitch, start, length)`
    /// on the timeline, and the clip's span.
    fn written(project: &Project) -> (Vec<Written>, (f64, f64)) {
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
            [TakeClip {
                track: TRACK,
                clip: project.tracks[0].clips[0].id,
                notes: 3,
                controllers: vec![],
                created: true
            }]
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
        let mut take = Take::new(1, &[TRACK], None, 4.0, 120.0);
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

    fn moved(kind: ControllerKind, value: f32, beat: f64) -> Captured {
        moved_in(kind, value, beat, 0)
    }

    fn moved_in(kind: ControllerKind, value: f32, beat: f64, pass: u32) -> Captured {
        Captured::Event { track: TRACK, kind: kind.event(value), beat, pass }
    }

    fn point(beat: f64, value: f64) -> AutomationPoint {
        AutomationPoint::new(beat, value)
    }

    fn held(beat: f64, value: f64) -> AutomationPoint {
        AutomationPoint { beat, value, shape: trill_core::Segment::Hold }
    }

    /// The one clip's curve for `kind`, as its points.
    fn curve(project: &Project, kind: ControllerKind) -> Vec<AutomationPoint> {
        let clips = &project.track(TRACK).unwrap().clips;
        assert_eq!(clips.len(), 1, "one clip");
        clips[0].controller(kind).map(|curve| curve.points.clone()).unwrap_or_default()
    }

    /// What `kind` is worth on the track at `beat`, as the engine plays it.
    fn worth(project: &Project, kind: ControllerKind, beat: f64) -> f64 {
        let lanes = project.track(TRACK).unwrap().controller_lanes();
        lanes.iter().find(|lane| lane.kind == kind).expect("the track moves it").value_at(beat)
    }

    #[test]
    fn the_pedal_is_recorded_as_a_controller_and_notes_are_as_long_as_their_keys() {
        let (mut project, mut take) = rolling(0.0, None);
        for captured in [
            on(60, 0.0),
            pedal(true, 0.5),
            off(60, 1.0),
            on(64, 1.5),
            off(64, 1.75),
            on(64, 2.0),
            pedal(false, 3.0),
            off(64, 3.5),
        ] {
            take.handle(captured);
        }
        take.handle(Captured::Stopped { beat: 4.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(written(&project).0, [(60, 0.0, 1.0), (64, 1.5, 0.25), (64, 2.0, 1.5)], "as the keys were played");
        assert_eq!(
            curve(&project, ControllerKind::SUSTAIN),
            [held(0.5, 1.0), held(3.0, 0.0)],
            "and the pedal as it was"
        );
        assert_eq!(worth(&project, ControllerKind::SUSTAIN, 0.25), 0.0, "up until it went down");
        assert_eq!(take.clips()[0].controllers, [ControllerKind::SUSTAIN]);
        assert_eq!(take.clips()[0].notes, 3);
    }

    #[test]
    fn a_gesture_is_written_as_it_is_played_and_follows_what_was_played() {
        let (mut project, mut take) = rolling(4.0, None);
        take.handle(on(60, 4.0));
        take.handle(off(60, 4.5));
        // The mod wheel pushed up and let back over two beats, reported
        // every hundredth of a beat in a controller's 128 steps.
        let wheel = |n: usize| {
            let beat = 5.0 + n as f64 / 100.0;
            let value = ((std::f64::consts::PI * n as f64 / 200.0).sin() * 127.0).round() / 127.0;
            (beat, value)
        };
        for n in 0..=100 {
            let (beat, value) = wheel(n);
            take.handle(moved(ControllerKind::MOD_WHEEL, value as f32, beat));
        }
        // Half way through, the lane already shows the first half.
        assert!(take.has_news(&project));
        write(&mut take, &mut project);
        assert!(!take.has_news(&project), "written once");
        let so_far = curve(&project, ControllerKind::MOD_WHEEL);
        assert!((2..12).contains(&so_far.len()), "{so_far:?}");
        assert_eq!((so_far[0].beat, so_far.last().unwrap().beat), (1.0, 2.0), "in the clip's own beats");

        for n in 101..=200 {
            let (beat, value) = wheel(n);
            take.handle(moved(ControllerKind::MOD_WHEEL, value as f32, beat));
        }
        take.handle(Captured::Stopped { beat: 8.0, pass: 0 });
        write(&mut take, &mut project);
        let points = curve(&project, ControllerKind::MOD_WHEEL);
        assert!(points.len() < 24, "{} points for 201 reports", points.len());
        for n in 0..=200 {
            let (beat, value) = wheel(n);
            let got = worth(&project, ControllerKind::MOD_WHEEL, beat);
            assert!((got - value).abs() <= THIN_TOLERANCE + 1e-6, "at beat {beat}: {got} for {value}");
        }
        assert_eq!(worth(&project, ControllerKind::MOD_WHEEL, 7.5), 0.0, "it stays where the wheel was left");
        assert_eq!(take.clips()[0].controllers, [ControllerKind::MOD_WHEEL]);
        assert!(!take.has_news(&project), "and nothing more is written once the take has settled");
    }

    #[test]
    fn a_take_begins_with_the_controllers_where_the_hand_left_them() {
        let (mut project, mut take) = rolling(4.0, None);
        let standing = |kind: ControllerKind, value: f32| Captured::Standing { track: TRACK, kind, value, pass: 0 };
        take.handle(standing(ControllerKind::EXPRESSION, 0.75));
        // Where a controller stands is not worth a clip by itself.
        assert!(!take.has_news(&project));
        write(&mut take, &mut project);
        assert!(project.track(TRACK).unwrap().clips.is_empty());

        take.handle(on(60, 5.0));
        take.handle(off(60, 6.0));
        take.handle(Captured::Stopped { beat: 7.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(curve(&project, ControllerKind::EXPRESSION), [point(0.0, 0.75)], "from the take's first beat");
        assert_eq!(
            worth(&project, ControllerKind::EXPRESSION, 5.0),
            0.75,
            "the note plays back at the level it was played"
        );
        assert_eq!(take.clips()[0].controllers, [ControllerKind::EXPRESSION]);

        // A take in which nothing but that happened leaves nothing behind.
        let (mut project, mut take) = rolling(0.0, None);
        take.handle(standing(ControllerKind::EXPRESSION, 0.75));
        take.handle(Captured::Stopped { beat: 4.0, pass: 0 });
        assert!(!take.has_news(&project));
        write(&mut take, &mut project);
        assert!(project.track(TRACK).unwrap().clips.is_empty() && take.clips().is_empty());
    }

    #[test]
    fn what_was_moved_in_the_count_in_counts_only_for_where_it_ended() {
        let mut project = Project::new("t");
        project.add_track("Keys");
        let mut take = Take::new(1, &[TRACK], None, 4.0, 120.0);
        take.handle(Captured::Began { take: 1 });
        take.handle(moved(ControllerKind::MOD_WHEEL, 0.875, -2.0));
        take.handle(moved(ControllerKind::MOD_WHEEL, 0.375, -1.0));
        take.handle(pedal(true, -0.5));
        take.handle(Captured::Rolling { beat: 0.0 });
        // The audio thread says where each stands as the song starts.
        take.handle(Captured::Standing { track: TRACK, kind: ControllerKind::MOD_WHEEL, value: 0.375, pass: 0 });
        take.handle(Captured::Standing { track: TRACK, kind: ControllerKind::SUSTAIN, value: 1.0, pass: 0 });
        take.handle(pedal(false, 1.0));
        take.handle(Captured::Stopped { beat: 2.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(curve(&project, ControllerKind::MOD_WHEEL), [point(0.0, 0.375)]);
        assert_eq!(curve(&project, ControllerKind::SUSTAIN), [held(0.0, 1.0), held(1.0, 0.0)]);
    }

    #[test]
    fn a_wheel_alone_is_worth_a_clip_and_what_is_still_held_at_the_stop_is_let_go_of_there() {
        let (mut project, mut take) = rolling(0.0, None);
        take.handle(moved(ControllerKind::PitchBend, 0.5, 1.0));
        take.handle(pedal(true, 1.5));
        take.handle(moved(ControllerKind::EXPRESSION, 0.625, 2.0));
        take.handle(Captured::Stopped { beat: 3.0, pass: 0 });
        write(&mut take, &mut project);
        let (notes, span) = written(&project);
        assert!(notes.is_empty());
        assert_eq!(span, (0.0, 4.0));
        assert_eq!(
            curve(&project, ControllerKind::PitchBend),
            [held(1.0, 0.5), point(3.0, 0.0)],
            "the wheel is let go of"
        );
        assert_eq!(curve(&project, ControllerKind::SUSTAIN), [held(1.5, 1.0), held(3.0, 0.0)], "and the pedal");
        assert_eq!(curve(&project, ControllerKind::EXPRESSION), [point(2.0, 0.625)], "what stays put stays");
        // Nonsense from a broken controller is kept within what a curve may hold.
        let (mut project, mut take) = rolling(0.0, None);
        take.handle(moved(ControllerKind::EXPRESSION, 7.0, 1.0));
        take.handle(moved(ControllerKind::PitchBend, f32::NAN, 1.0));
        take.handle(Captured::Stopped { beat: 2.0, pass: 0 });
        write(&mut take, &mut project);
        assert_eq!(curve(&project, ControllerKind::EXPRESSION), [point(1.0, 1.0)]);
        assert_eq!(curve(&project, ControllerKind::PitchBend), [point(1.0, 0.0)]);
    }

    #[test]
    fn recording_over_a_curve_replaces_it_only_where_the_hand_was_heard() {
        let (mut project, mut take) = rolling(8.0, None);
        let clip = project.add_midi_clip(TRACK, "Part", 8.0, 8.0).unwrap();
        let part = project.clip_mut(clip).unwrap();
        // A swell over the whole clip, and a mod wheel that is not touched.
        part.set_controller(ControllerKind::EXPRESSION, vec![point(0.0, 0.0), point(8.0, 1.0)]).unwrap();
        part.set_controller(ControllerKind::MOD_WHEEL, vec![point(0.0, 0.25)]).unwrap();
        let before = project.clone();

        take.handle(on(60, 9.0));
        take.handle(off(60, 9.5));
        take.handle(moved(ControllerKind::EXPRESSION, 0.875, 10.0));
        take.handle(moved(ControllerKind::EXPRESSION, 0.375, 11.0));
        // While the take runs, the hand has the controller to the end.
        write(&mut take, &mut project);
        assert_eq!(worth(&project, ControllerKind::EXPRESSION, 15.0), 0.375);
        take.handle(Captured::Stopped { beat: 12.0, pass: 0 });
        write(&mut take, &mut project);

        let at = |project: &Project, beat| worth(project, ControllerKind::EXPRESSION, beat);
        assert_eq!(
            (at(&project, 8.5), at(&project, 9.5)),
            (at(&before, 8.5), at(&before, 9.5)),
            "before it was touched"
        );
        assert_eq!(
            (at(&project, 10.0), at(&project, 10.5), at(&project, 11.5)),
            (0.875, 0.875, 0.375),
            "what the hand played"
        );
        assert_eq!((at(&project, 12.0), at(&project, 14.0)), (at(&before, 12.0), at(&before, 14.0)), "after the take");
        assert_eq!(
            project.clip(clip).unwrap().controller(ControllerKind::MOD_WHEEL),
            before.clip(clip).unwrap().controller(ControllerKind::MOD_WHEEL)
        );
        assert_eq!(
            take.clips(),
            [TakeClip { track: TRACK, clip, notes: 1, controllers: vec![ControllerKind::EXPRESSION], created: false }]
        );
    }

    #[test]
    fn each_pass_of_the_loop_writes_its_gesture_over_the_last() {
        let (mut project, mut take) = rolling(0.0, Some((0.0, 8.0)));
        take.handle(on(60, 0.0));
        take.handle(off(60, 1.0));
        take.handle(moved(ControllerKind::EXPRESSION, 0.25, 1.0));
        take.handle(moved(ControllerKind::EXPRESSION, 0.5, 6.0));
        take.heard(7.9);
        write(&mut take, &mut project);
        take.handle(Captured::Wrapped { pass: 1 });
        // The second time round the hand takes it at the fourth beat.
        take.handle(moved_in(ControllerKind::EXPRESSION, 0.75, 4.0, 1));
        take.handle(Captured::Stopped { beat: 5.0, pass: 1 });
        write(&mut take, &mut project);
        let at = |beat| worth(&project, ControllerKind::EXPRESSION, beat);
        assert_eq!((at(0.5), at(2.0)), (0.25, 0.25), "the first pass, where the second did not touch it");
        assert_eq!((at(4.0), at(4.9)), (0.75, 0.75), "the second pass, for as long as it was heard");
        assert_eq!((at(5.5), at(6.5)), (0.25, 0.5), "and the first again after the stop");
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
        assert_eq!(take.clips(), [TakeClip { track: TRACK, clip, notes: 2, controllers: vec![], created: false }]);
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
        let mut take = Take::new(3, &[TRACK], None, 4.0, 120.0);
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
        let mut take = Take::new(1, &[keys, bass], None, 4.0, 120.0);
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
