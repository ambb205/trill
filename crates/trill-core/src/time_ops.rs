//! Edits to a stretch of time across the whole song: cutting it out so
//! what follows closes up, repeating it, opening up empty time, and copying
//! it to another place. Each carries everything that lives on the timeline
//! with it: clips on every track (with the controller curves inside them),
//! automation, markers, the loop, and the changes of tempo and of time
//! signature. Time that is cut out takes its changes with it, time that is
//! repeated has them twice, and time that is opened up has the tempo and
//! the signature the song arrived there with. Copying a stretch to another
//! place ([`Project::paste_time`]) is the one that leaves them alone: what
//! is put down is played at the tempo, and counted in the bars, of where
//! it lands.
//!
//! An edit that fails may leave the project half changed; run it where a
//! failure is rolled back, as the session's edits are.

use crate::controllers::{splice_along, value_before_along};
use crate::project::{AutomationPoint, AutomationTarget, ChainRef, Clip, EditError, Project, Segment, envelope_value};
use crate::tempo::{Stretch, rearranged_meter, rearranged_tempo};
use crate::time::Beats;
use crate::travel::Travel;
use crate::{ClipId, TrackId};

/// What a stretch of time holds, lifted out of the song: the clips of the
/// tracks it was taken from, cut to the stretch, and the course of their
/// automation through it. [`Project::paste_time`] puts it down again.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeExcerpt {
    pub length: Beats,
    /// Each clip with the track it was on. Its start counts from the
    /// start of the stretch.
    pub clips: Vec<(TrackId, Clip)>,
    /// Each envelope's course through the stretch, in beats from its
    /// start: a point at 0, the points inside, and a point at the end.
    pub lanes: Vec<(AutomationTarget, Vec<AutomationPoint>)>,
    /// The tracks it was taken from; `None` for all of them, with the
    /// master's automation.
    pub tracks: Option<Vec<TrackId>>,
}

/// What becomes of the time an excerpt is pasted into.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PasteMode {
    /// Room is made for it: everything from there on, on every track,
    /// moves later by its length.
    Insert,
    /// It takes the place of what its tracks hold there.
    Replace,
}

/// `length` beats of the old timeline from `from` on, kept at `at`.
fn kept(at: Beats, from: Beats, length: Beats) -> Stretch {
    Stretch { at, from, length, hold: false }
}

/// `start..end` as a stretch of time, or why it is none.
fn stretch(start: Beats, end: Beats) -> Result<Beats, EditError> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err(EditError::Invalid(format!("the time {start}..{end} must satisfy 0 <= start < end")));
    }
    Ok(end - start)
}

impl Project {
    /// Removes the time from `start` to `end` from every track: clips inside
    /// it go, clips across its edges are cut there, and everything after it
    /// (clips, automation, markers, the loop) moves earlier to close the gap.
    pub fn delete_time(&mut self, start: Beats, end: Beats) -> Result<(), EditError> {
        let length = stretch(start, end)?;
        let clips: Vec<_> = self.tracks.iter().flat_map(|track| track.clips.iter().map(|clip| clip.id)).collect();
        for id in clips {
            let clip = self.clip(id)?;
            let (clip_start, clip_end) = (clip.start, clip.end());
            if clip_end <= start {
                continue;
            }
            if clip_start >= end {
                self.move_clip(id, None, clip_start - length)?;
            } else if clip_start >= start && clip_end <= end {
                self.remove_clip(id)?;
            } else if clip_start < start && clip_end > end {
                // The cut falls inside the clip: its two ends close up.
                let tail = self.split_clip(id, end)?;
                self.set_clip_span(id, clip_start, start - clip_start)?;
                self.move_clip(tail, None, start)?;
            } else if clip_start < start {
                self.set_clip_span(id, clip_start, start - clip_start)?;
            } else {
                self.trim_clip_start(id, end)?;
                self.move_clip(id, None, start)?;
            }
        }

        for (target, points, travel) in self.envelopes() {
            // An envelope that has settled before the cut is the same after it.
            if points.last().is_some_and(|last| last.beat >= start) {
                self.set_automation(target, cut_envelope(&points, travel, start, end))?;
            }
        }

        let shifted = |beat: Beats| if beat >= end { beat - length } else { beat.min(start) };
        self.markers.retain(|marker| marker.beat < start || marker.beat >= end);
        for marker in &mut self.markers {
            marker.beat = shifted(marker.beat);
        }
        let region = self.loop_region;
        let (loop_start, loop_end) = (shifted(region.start), shifted(region.end));
        if loop_end > loop_start {
            self.set_loop(region.enabled, loop_start, loop_end)?;
        }
        self.lay_time(&[kept(0.0, 0.0, start), kept(start, end, f64::INFINITY)]);
        Ok(())
    }

    /// Repeats the time from `start` to `end` right after itself: everything
    /// from `end` on moves later by its length, and what the stretch holds is
    /// copied into the room that makes.
    pub fn duplicate_time(&mut self, start: Beats, end: Beats) -> Result<(), EditError> {
        let length = stretch(start, end)?;
        let clips = self.make_room(end, length)?;
        // Copy what is left inside the stretch.
        for (track, id) in clips {
            let source = self.clip(id)?.clone();
            if source.start >= end || source.end() <= start {
                continue;
            }
            if source.start < start {
                // Only the part inside the stretch is repeated: cut where
                // it lies, at the tempo it has there.
                let copy = self.paste_clip(track, &source, source.start)?;
                self.trim_clip_start(copy, start)?;
                self.move_clip(copy, None, start + length)?;
            } else {
                self.paste_clip(track, &source, source.start + length)?;
            }
        }

        for (target, points, travel) in self.envelopes() {
            if points.last().is_some_and(|last| last.beat >= start) {
                self.set_automation(target, repeated_envelope(&points, travel, start, end))?;
            }
        }
        self.shift_marks(end, length)?;
        self.lay_time(&[kept(0.0, 0.0, end), kept(end, start, length), kept(end + length, end, f64::INFINITY)]);
        Ok(())
    }

    /// Opens up `length` beats of empty time at `at`: everything from there
    /// on moves later, and a clip lying across it is cut there. Automation
    /// holds its value through the new time.
    pub fn insert_time(&mut self, at: Beats, length: Beats) -> Result<(), EditError> {
        if !at.is_finite() || at < 0.0 || !length.is_finite() || length <= 0.0 {
            return Err(EditError::Invalid(format!(
                "inserting {length} beats at {at} needs a position >= 0 and a length > 0"
            )));
        }
        self.make_room(at, length)?;
        for (target, points, travel) in self.envelopes() {
            if points.last().is_some_and(|last| last.beat >= at) {
                self.set_automation(target, held_envelope(&points, travel, at, length))?;
            }
        }
        self.shift_marks(at, length)?;
        let held = Stretch { at, from: at, length, hold: true };
        self.lay_time(&[kept(0.0, 0.0, at), held, kept(at + length, at, f64::INFINITY)]);
        Ok(())
    }

    /// What the time from `start` to `end` holds on `tracks` (every track
    /// with `None`), as something that can be put down elsewhere. The song
    /// is not changed: a clip lying across an edge is cut in the excerpt
    /// only.
    pub fn excerpt(&self, start: Beats, end: Beats, tracks: Option<&[TrackId]>) -> Result<TimeExcerpt, EditError> {
        let length = stretch(start, end)?;
        for id in tracks.into_iter().flatten() {
            self.track(*id)?;
        }
        let chosen = |id: TrackId| tracks.is_none_or(|tracks| tracks.contains(&id));

        // Cut in a copy of the song, where the cutting is already written.
        let mut scratch = self.clone();
        let ids: Vec<(TrackId, ClipId)> = self
            .tracks
            .iter()
            .filter(|track| chosen(track.id))
            .flat_map(|track| track.clips.iter().map(move |clip| (track.id, clip.id)))
            .collect();
        let mut clips = Vec::new();
        for (track, id) in ids {
            let clip = scratch.clip(id)?;
            if clip.end() <= start || clip.start >= end {
                continue;
            }
            let inside = if clip.start < start { scratch.split_clip(id, start)? } else { id };
            if scratch.clip(inside)?.end() > end {
                scratch.split_clip(inside, end)?;
            }
            let mut clip = scratch.clip(inside)?.clone();
            clip.start -= start;
            clips.push((track, clip));
        }

        let lanes = self
            .automation
            .iter()
            .filter(|lane| match self.target_track(&lane.target) {
                Some(track) => chosen(track),
                // The master's own automation goes with the whole song only.
                None => tracks.is_none(),
            })
            .map(|lane| (lane.target.clone(), course(&lane.points, lane.travel, start, end)))
            .collect();
        Ok(TimeExcerpt { length, clips, lanes, tracks: tracks.map(<[TrackId]>::to_vec) })
    }

    /// Puts `excerpt` down at `at`, on the tracks it was taken from, and
    /// returns the clips this made. With [`PasteMode::Insert`] room is made
    /// for it on every track, as [`Project::insert_time`] makes it; with
    /// [`PasteMode::Replace`] it takes the place of what its tracks hold
    /// there, and clips lying across the edges are cut. Automation follows
    /// the excerpt through the stretch and then picks up where it was.
    /// Markers are not copied. What belonged to a track or a device that
    /// is no longer there is left out.
    pub fn paste_time(&mut self, excerpt: &TimeExcerpt, at: Beats, mode: PasteMode) -> Result<Vec<ClipId>, EditError> {
        if !at.is_finite() || at < 0.0 {
            return Err(EditError::Invalid(format!("the position {at} to paste at must be >= 0")));
        }
        let end = at + excerpt.length;
        match mode {
            PasteMode::Insert => self.insert_time(at, excerpt.length)?,
            PasteMode::Replace => self.clear_time(at, end, excerpt.tracks.as_deref())?,
        }
        let mut pasted = Vec::new();
        for (track, clip) in &excerpt.clips {
            if self.track(*track).is_ok() {
                pasted.push(self.paste_clip(*track, clip, at + clip.start)?);
            }
        }
        for (target, piece) in &excerpt.lanes {
            let Some(lane) = self.automation.iter().find(|lane| lane.target == *target) else { continue };
            let (base, travel) = (lane.points.clone(), lane.travel);
            let moved: Vec<AutomationPoint> =
                piece.iter().map(|point| AutomationPoint { beat: point.beat + at, ..*point }).collect();
            let spliced = tidied(splice_along(&base, travel, at, Some(end), &moved));
            if spliced != base {
                self.set_automation(target.clone(), spliced)?;
            }
        }
        Ok(pasted)
    }

    /// Copies the time from `start` to `end` to `to`: [`Project::excerpt`]
    /// and [`Project::paste_time`] in one. The stretch may be copied onto
    /// or into itself; what is copied is what it held before.
    pub fn copy_time(
        &mut self,
        start: Beats,
        end: Beats,
        to: Beats,
        tracks: Option<&[TrackId]>,
        mode: PasteMode,
    ) -> Result<Vec<ClipId>, EditError> {
        let excerpt = self.excerpt(start, end, tracks)?;
        self.paste_time(&excerpt, to, mode)
    }

    /// Empties the time from `start` to `end` on `tracks` (every track
    /// with `None`) without closing it up: clips inside it go, and clips
    /// across its edges are cut there.
    fn clear_time(&mut self, start: Beats, end: Beats, tracks: Option<&[TrackId]>) -> Result<(), EditError> {
        let ids: Vec<ClipId> = self
            .tracks
            .iter()
            .filter(|track| tracks.is_none_or(|tracks| tracks.contains(&track.id)))
            .flat_map(|track| track.clips.iter().map(|clip| clip.id))
            .collect();
        for id in ids {
            let clip = self.clip(id)?;
            if clip.end() <= start || clip.start >= end {
                continue;
            }
            let inside = if clip.start < start { self.split_clip(id, start)? } else { id };
            if self.clip(inside)?.end() > end {
                self.split_clip(inside, end)?;
            }
            self.remove_clip(inside)?;
        }
        Ok(())
    }

    /// Lays the song's tempo and time signature along a timeline put
    /// together from `stretches` of the one it had. The last thing an edit
    /// of time does: until then the clips are cut at the tempo they have
    /// where they lie.
    fn lay_time(&mut self, stretches: &[Stretch]) {
        if self.tempo_changes.is_empty() && self.meter_changes.is_empty() {
            return;
        }
        (self.tempo, self.tempo_changes) = rearranged_tempo(self.tempo, &self.tempo_changes, stretches);
        (self.time_signature, self.meter_changes) =
            rearranged_meter(self.time_signature, &self.meter_changes, stretches);
    }

    /// Every envelope as it stands: what it moves, its points, and what
    /// its lines are straight along.
    fn envelopes(&self) -> Vec<(AutomationTarget, Vec<AutomationPoint>, Travel)> {
        self.automation.iter().map(|lane| (lane.target.clone(), lane.points.clone(), lane.travel)).collect()
    }

    /// The track an automation target belongs to; `None` for the master's.
    fn target_track(&self, target: &AutomationTarget) -> Option<TrackId> {
        match target {
            AutomationTarget::TrackVolume { track }
            | AutomationTarget::TrackPan { track }
            | AutomationTarget::Send { track, .. } => Some(*track),
            AutomationTarget::DeviceParam { device, .. } => match self.device_chain(*device) {
                Ok(ChainRef::Track(track)) => Some(track),
                _ => None,
            },
            AutomationTarget::MasterVolume => None,
        }
    }

    /// Moves every clip that starts at or after `at` later by `length`, and
    /// cuts a clip lying across `at` there so that its tail moves too.
    /// Returns the clips as they were before, each with its track.
    fn make_room(&mut self, at: Beats, length: Beats) -> Result<Vec<(crate::TrackId, crate::ClipId)>, EditError> {
        let clips: Vec<_> =
            self.tracks.iter().flat_map(|track| track.clips.iter().map(move |clip| (track.id, clip.id))).collect();
        for (_, id) in &clips {
            let clip = self.clip(*id)?;
            let (clip_start, clip_end) = (clip.start, clip.end());
            if clip_start >= at {
                self.move_clip(*id, None, clip_start + length)?;
            } else if clip_end > at {
                let tail = self.split_clip(*id, at)?;
                self.move_clip(tail, None, at + length)?;
            }
        }
        Ok(clips)
    }

    /// Moves the markers and loop ends at or after `at` later by `length`.
    fn shift_marks(&mut self, at: Beats, length: Beats) -> Result<(), EditError> {
        for marker in self.markers.iter_mut().filter(|marker| marker.beat >= at) {
            marker.beat += length;
        }
        let region = self.loop_region;
        let shifted = |beat: Beats| if beat >= at { beat + length } else { beat };
        self.set_loop(region.enabled, shifted(region.start), shifted(region.end))
    }
}

/// The course `points` take from `start` to `end`, in beats from `start`:
/// a point at 0 with the value there, the points inside, and a point at the
/// end with the value the envelope arrives at.
fn course(points: &[AutomationPoint], travel: Travel, start: Beats, end: Beats) -> Vec<AutomationPoint> {
    // The shape of the segment `start` falls in carries on from the cut.
    let shape = match points.partition_point(|point| point.beat <= start) {
        0 => Segment::Linear,
        next if next == points.len() => Segment::Hold,
        next => points[next - 1].shape,
    };
    let mut course = vec![AutomationPoint { beat: 0.0, value: envelope_value(points, travel, start), shape, id: 0 }];
    course.extend(
        points
            .iter()
            .filter(|point| point.beat > start && point.beat < end)
            .map(|point| AutomationPoint { beat: point.beat - start, ..*point }),
    );
    let arrives = value_before_along(points, travel, end);
    course.push(AutomationPoint { beat: end - start, value: arrives, shape: Segment::Hold, id: 0 });
    course
}

/// `points` with `length` beats opened up at `at`, through which the
/// envelope holds the value it had there.
fn held_envelope(points: &[AutomationPoint], travel: Travel, at: Beats, length: Beats) -> Vec<AutomationPoint> {
    let value = envelope_value(points, travel, at);
    let mut result: Vec<AutomationPoint> = points.iter().filter(|point| point.beat < at).copied().collect();
    result.push(AutomationPoint::new(at, value));
    result.push(AutomationPoint::new(at + length, value));
    result.extend(
        points
            .iter()
            .filter(|point| point.beat >= at)
            .map(|point| AutomationPoint { beat: point.beat + length, ..*point }),
    );
    tidied(result)
}

/// `points` with the stretch `start..end` cut out. A point is left on each
/// side of the cut holding the value the envelope had there, so its shape
/// before and after is what it was.
fn cut_envelope(points: &[AutomationPoint], travel: Travel, start: Beats, end: Beats) -> Vec<AutomationPoint> {
    let length = end - start;
    let before = points.iter().filter(|point| point.beat < start).copied();
    let after = points
        .iter()
        .filter(|point| point.beat >= end)
        .map(|point| AutomationPoint { beat: point.beat - length, ..*point });
    let mut result: Vec<AutomationPoint> = before.collect();
    result.push(AutomationPoint::new(start, envelope_value(points, travel, start)));
    result.push(AutomationPoint::new(start, envelope_value(points, travel, end)));
    result.extend(after);
    tidied(result)
}

/// `points` with the stretch `start..end` played twice.
fn repeated_envelope(points: &[AutomationPoint], travel: Travel, start: Beats, end: Beats) -> Vec<AutomationPoint> {
    let length = end - start;
    let later = |point: &AutomationPoint| AutomationPoint { beat: point.beat + length, ..*point };
    let mut result: Vec<AutomationPoint> = points.iter().filter(|point| point.beat < end).copied().collect();
    let at_end = envelope_value(points, travel, end);
    // The first pass ends where it did, the second starts where the first
    // started, and after it the envelope picks up again.
    result.push(AutomationPoint::new(end, at_end));
    result.push(AutomationPoint::new(end, envelope_value(points, travel, start)));
    result.extend(points.iter().filter(|point| point.beat > start && point.beat < end).map(later));
    result.push(AutomationPoint::new(end + length, at_end));
    result.extend(points.iter().filter(|point| point.beat >= end).map(later));
    tidied(result)
}

/// Drops points that say nothing: a repeat of the point before, and the
/// middle one of three in a row at the same place.
fn tidied(points: Vec<AutomationPoint>) -> Vec<AutomationPoint> {
    let mut result: Vec<AutomationPoint> = Vec::with_capacity(points.len());
    for point in points {
        let same = |other: &AutomationPoint| other.beat == point.beat && other.value == point.value;
        if result.last().is_some_and(same) {
            continue;
        }
        let count = result.len();
        if count >= 2 && result[count - 1].beat == point.beat && result[count - 2].beat == point.beat {
            result[count - 1] = point;
            continue;
        }
        result.push(point);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::automation_value;
    use crate::{AutomationLane, AutomationTarget, ClipId, Note, TrackId};

    const STRAIGHT: Travel = Travel::Linear;

    fn note(start: Beats) -> Note {
        Note { pitch: 60, start, length: 0.5, velocity: 100, ..Default::default() }
    }

    /// One track with clips at 0..4, 4..12 (a note on every beat) and
    /// 12..16, a marker in each, and the loop over 8..16.
    fn project() -> (Project, TrackId, [ClipId; 3]) {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let first = project.add_midi_clip(track, "first", 0.0, 4.0).unwrap();
        let long = project.add_midi_clip(track, "long", 4.0, 8.0).unwrap();
        let last = project.add_midi_clip(track, "last", 12.0, 4.0).unwrap();
        let notes: Vec<Note> = (0..8).map(|beat| note(beat as f64)).collect();
        project.clip_mut(long).unwrap().add_notes(&notes).unwrap();
        for (beat, name) in [(2.0, "a"), (7.0, "b"), (14.0, "c")] {
            project.add_marker(beat, name).unwrap();
        }
        project.set_loop(true, 8.0, 16.0).unwrap();
        (project, track, [first, long, last])
    }

    fn spans(project: &Project, track: TrackId) -> Vec<(Beats, Beats)> {
        project.track(track).unwrap().clips.iter().map(|clip| (clip.start, clip.end())).collect()
    }

    /// Where every note of the track sounds.
    fn sounding(project: &Project, track: TrackId) -> Vec<Beats> {
        let clips = &project.track(track).unwrap().clips;
        let mut starts: Vec<Beats> =
            clips.iter().flat_map(|clip| clip.timeline_notes()).map(|note| note.start).collect();
        starts.sort_by(f64::total_cmp);
        starts
    }

    #[test]
    fn deleting_time_closes_the_gap() {
        let (mut project, track, [first, long, last]) = project();
        // Bars 6..10 fall inside the long clip.
        project.delete_time(6.0, 10.0).unwrap();
        assert_eq!(spans(&project, track), [(0.0, 4.0), (4.0, 6.0), (6.0, 8.0), (8.0, 12.0)]);
        assert_eq!(sounding(&project, track), [4.0, 5.0, 6.0, 7.0], "the notes at 10 and 11 follow the tail");
        assert_eq!((project.clip(first).unwrap().start, project.clip(last).unwrap().start), (0.0, 8.0));
        assert_eq!(project.clip(long).unwrap().length, 2.0);
        let markers: Vec<(Beats, &str)> = project.markers.iter().map(|m| (m.beat, m.name.as_str())).collect();
        assert_eq!(markers, [(2.0, "a"), (10.0, "c")], "the marker inside the cut goes with it");
        assert_eq!((project.loop_region.start, project.loop_region.end), (6.0, 12.0));
    }

    #[test]
    fn time_takes_its_tempo_and_its_meter_along_and_audio_keeps_what_it_played() {
        use crate::project::{AudioContent, ClipContent};
        use crate::tempo::{MeterChange, TempoChange};
        use crate::time::TimeSignature;

        // 120 in 4/4; from beat 8, 60 in 3/4; from beat 14 on, 120 in 4/4.
        let (mut song, _, _) = project();
        song.set_tempo_at(8.0, 60.0, false).unwrap();
        song.set_tempo_at(14.0, 120.0, false).unwrap();
        song.set_time_signature_at(8.0, 3, 4).unwrap();
        song.set_time_signature_at(14.0, 4, 4).unwrap();
        // A recording across all of it, played at the speed it was made.
        let audio = song.add_track("Audio");
        let take = song.add_audio_clip(audio, "take", 4.0, 14.0, AudioContent::new("take.wav")).unwrap();
        let jump = |beat: Beats, tempo: f64| TempoChange { beat, tempo, ramp: false };
        let meter = |beat: Beats, n: u8, d: u8| MeterChange { beat, signature: TimeSignature::new(n, d).unwrap() };
        // Where each clip of the recording starts, and where in the file.
        let takes = |song: &Project| -> Vec<(Beats, f64)> {
            let clips = song.track(audio).unwrap().clips.iter();
            clips
                .map(|clip| match &clip.content {
                    ClipContent::Audio(audio) => (clip.start, (audio.offset_seconds * 1e9).round() / 1e9),
                    ClipContent::Midi(_) => unreachable!(),
                })
                .collect()
        };

        // The slow bars cut out: the song is at 120 in 4/4 throughout, with
        // nothing left to say so, and the recording goes on after the cut
        // from where it would have been: two seconds and six more in.
        let mut cut = song.clone();
        cut.delete_time(8.0, 14.0).unwrap();
        assert!(cut.tempo_changes.is_empty() && cut.meter_changes.is_empty(), "{:?}", cut.meter_changes);
        assert_eq!(takes(&cut), [(4.0, 0.0), (8.0, 8.0)]);

        // Their second bar heard twice: the changes after it are a bar
        // later, and the repeat plays the second bar's three seconds again.
        let mut twice = song.clone();
        twice.duplicate_time(11.0, 14.0).unwrap();
        assert_eq!(twice.tempo_changes, [jump(8.0, 60.0), jump(17.0, 120.0)]);
        assert_eq!(twice.meter_changes, [meter(8.0, 3, 4), meter(17.0, 4, 4)]);
        assert_eq!(takes(&twice), [(4.0, 0.0), (14.0, 5.0), (17.0, 8.0)]);
        assert_eq!(take, twice.track(audio).unwrap().clips[0].id);

        // Two bars opened up where the slow bars begin: they have the
        // tempo and the meter the song arrived with, and the slow bars
        // come after them.
        let mut opened = song.clone();
        opened.insert_time(8.0, 8.0).unwrap();
        assert_eq!(opened.tempo_changes, [jump(16.0, 60.0), jump(22.0, 120.0)]);
        assert_eq!(opened.meter_changes, [meter(16.0, 3, 4), meter(22.0, 4, 4)]);
        assert_eq!(takes(&opened), [(4.0, 0.0), (16.0, 2.0)]);

        // The first eight beats cut off: the song begins slow, in three.
        let mut later = song.clone();
        later.delete_time(0.0, 8.0).unwrap();
        assert_eq!((later.tempo, later.time_signature), (60.0, TimeSignature::new(3, 4).unwrap()));
        assert_eq!(
            (&later.tempo_changes[..], &later.meter_changes[..]),
            (&[jump(6.0, 120.0)][..], &[meter(6.0, 4, 4)][..])
        );

        // Copied to another place, a stretch leaves the tempo and the
        // meter of where it lands.
        let mut copied = song.clone();
        copied.copy_time(8.0, 14.0, 20.0, None, PasteMode::Replace).unwrap();
        assert_eq!((&copied.tempo_changes, &copied.meter_changes), (&song.tempo_changes, &song.meter_changes));
    }

    #[test]
    fn deleting_time_handles_every_way_a_clip_can_overlap() {
        let (mut project, track, _) = project();
        // From inside the first clip to inside the long one.
        project.delete_time(2.0, 6.0).unwrap();
        assert_eq!(spans(&project, track), [(0.0, 2.0), (2.0, 8.0), (8.0, 12.0)]);
        assert_eq!(sounding(&project, track)[..2], [2.0, 3.0], "the long clip lost its first two beats, not its notes");

        // A whole clip and nothing else.
        project.delete_time(8.0, 12.0).unwrap();
        assert_eq!(spans(&project, track), [(0.0, 2.0), (2.0, 8.0)]);
        // An empty or backwards stretch is no stretch of time, and says so.
        let before = project.clone();
        for (start, end) in [(4.0, 4.0), (5.0, 4.0), (-1.0, 4.0), (0.0, f64::NAN)] {
            assert!(project.delete_time(start, end).is_err(), "{start}..{end}");
            assert!(project.duplicate_time(start, end).is_err(), "{start}..{end}");
        }
        assert!(project.insert_time(4.0, 0.0).is_err() && project.insert_time(-1.0, 4.0).is_err());
        assert_eq!(project, before);
    }

    #[test]
    fn duplicating_time_repeats_what_the_stretch_holds() {
        let (mut project, track, [_, long, last]) = project();
        // Bars 2..6: the end of the first clip and the start of the long one.
        project.duplicate_time(2.0, 6.0).unwrap();
        assert_eq!(
            spans(&project, track),
            [(0.0, 4.0), (4.0, 6.0), (6.0, 8.0), (8.0, 10.0), (10.0, 16.0), (16.0, 20.0)],
            "the original, then the copies of 2..4 and 4..6, then the long clip's tail and what followed"
        );
        assert_eq!(project.clip(long).unwrap().length, 2.0, "the long clip was cut where the copy goes in");
        assert_eq!(project.clip(last).unwrap().start, 16.0);
        assert_eq!(sounding(&project, track), [4.0, 5.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
        let markers: Vec<Beats> = project.markers.iter().map(|marker| marker.beat).collect();
        assert_eq!(markers, [2.0, 11.0, 18.0], "markers after the stretch move; none is copied");
        assert_eq!((project.loop_region.start, project.loop_region.end), (12.0, 20.0));
    }

    fn envelope(points: &[(Beats, f64)]) -> Vec<AutomationPoint> {
        points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect()
    }

    fn values(points: &[AutomationPoint], beats: &[Beats]) -> Vec<f64> {
        beats.iter().map(|beat| automation_value(points, *beat)).collect()
    }

    #[test]
    fn a_cut_envelope_keeps_its_shape_on_both_sides() {
        // A ramp from 0 at beat 0 to 16 at beat 16.
        let ramp = envelope(&[(0.0, 0.0), (16.0, 16.0)]);
        let cut = cut_envelope(&ramp, STRAIGHT, 4.0, 8.0);
        assert_eq!(cut, envelope(&[(0.0, 0.0), (4.0, 4.0), (4.0, 8.0), (12.0, 16.0)]));
        assert_eq!(values(&cut, &[2.0, 3.99, 4.0, 8.0, 12.0]), [2.0, 3.99, 8.0, 12.0, 16.0]);

        // Points inside the cut go; a flat stretch needs no step.
        let stepped = envelope(&[(0.0, 1.0), (5.0, 9.0), (6.0, 1.0), (10.0, 1.0)]);
        assert_eq!(
            cut_envelope(&stepped, STRAIGHT, 4.0, 8.0),
            envelope(&[(0.0, 1.0), (4.0, 7.4), (4.0, 1.0), (6.0, 1.0)])
        );
        assert_eq!(
            cut_envelope(&envelope(&[(0.0, 3.0), (20.0, 3.0)]), STRAIGHT, 4.0, 8.0),
            envelope(&[(0.0, 3.0), (4.0, 3.0), (16.0, 3.0)])
        );
    }

    #[test]
    fn a_repeated_envelope_plays_the_stretch_twice() {
        let ramp = envelope(&[(0.0, 0.0), (16.0, 16.0)]);
        let twice = repeated_envelope(&ramp, STRAIGHT, 4.0, 8.0);
        assert_eq!(
            values(&twice, &[2.0, 6.0, 7.99, 8.0, 10.0, 12.0, 14.0, 20.0]),
            [2.0, 6.0, 7.99, 4.0, 6.0, 8.0, 10.0, 16.0]
        );
        // A point inside the stretch is copied with it.
        let bump = envelope(&[(0.0, 0.0), (6.0, 10.0), (8.0, 0.0)]);
        let twice = repeated_envelope(&bump, STRAIGHT, 4.0, 8.0);
        assert_eq!(values(&twice, &[6.0, 10.0, 12.0, 14.0]), [10.0, 10.0, 0.0, 0.0]);
    }

    #[test]
    fn time_edits_carry_automation_and_leave_settled_envelopes_alone() {
        let (mut project, track, _) = project();
        let volume = AutomationTarget::TrackVolume { track };
        let pan = AutomationTarget::TrackPan { track };
        project.write_automation(volume.clone(), envelope(&[(0.0, -12.0), (16.0, 4.0)]), STRAIGHT).unwrap();
        project.set_automation(pan.clone(), envelope(&[(0.0, -1.0), (1.0, 0.0)])).unwrap();
        project.delete_time(4.0, 8.0).unwrap();
        let lane = |project: &Project, target: &AutomationTarget| {
            project.automation.iter().find(|lane| lane.target == *target).unwrap().points.clone()
        };
        assert_eq!(values(&lane(&project, &volume), &[0.0, 4.0, 12.0]), [-12.0, -4.0, 4.0]);
        assert_eq!(lane(&project, &pan), envelope(&[(0.0, -1.0), (1.0, 0.0)]), "it ended before the cut");

        project.duplicate_time(4.0, 6.0).unwrap();
        assert_eq!(values(&lane(&project, &volume), &[4.0, 5.0, 6.0, 7.0, 8.0]), [-4.0, -3.0, -4.0, -3.0, -2.0]);
        assert_eq!(lane(&project, &pan).len(), 2);
    }

    /// The envelope of `target`, as it is played.
    fn played<'a>(project: &'a Project, target: &AutomationTarget) -> &'a AutomationLane {
        project.automation.iter().find(|lane| lane.target == *target).expect("the target has an envelope")
    }

    #[test]
    fn an_envelope_that_follows_a_fader_keeps_its_course_through_every_edit_of_time() {
        // A fade and a throw of a send, both straight along a fader: under
        // its knee neither is a straight line in decibels, so a cut that
        // read the envelope in decibels would bend what is left of it.
        let (mut project, track, _) = project();
        let echo = project.add_track_of_kind("Echo", crate::TrackKind::Return);
        let volume = AutomationTarget::TrackVolume { track };
        let send = AutomationTarget::Send { track, to: echo };
        project.set_automation(volume.clone(), envelope(&[(0.0, 0.0), (16.0, -60.0)])).unwrap();
        let throw = envelope(&[(0.0, -70.0), (6.0, -70.0), (9.0, -6.0), (16.0, -70.0)]);
        project.set_automation(send.clone(), throw).unwrap();
        let before = project.clone();
        assert_eq!(played(&before, &volume).travel, Travel::FADER);
        assert!((played(&before, &volume).value_at(8.0) + 16.3).abs() < 0.01, "not the -30 dB of a line in decibels");
        let same = |project: &Project, beat: Beats, was_at: Beats, what: &str| {
            for target in [&volume, &send] {
                let (now, was) = (played(project, target).value_at(beat), played(&before, target).value_at(was_at));
                assert!((now - was).abs() < 1e-9, "{what}: {target:?} at {beat} is {now}, was {was} at {was_at}");
            }
        };

        // Cut 5..10 out: what is left plays as it did, on both sides.
        let mut cut = before.clone();
        cut.delete_time(5.0, 10.0).unwrap();
        for beat in [0.0, 2.5, 4.9] {
            same(&cut, beat, beat, "before the cut");
        }
        for beat in [5.0, 7.3, 11.0] {
            same(&cut, beat, beat + 5.0, "after the cut");
        }
        assert_eq!(played(&cut, &volume).travel, Travel::FADER, "an edit of time leaves the travel alone");

        // Repeat 6..10: the stretch is heard twice, then the rest.
        let mut twice = before.clone();
        twice.duplicate_time(6.0, 10.0).unwrap();
        for beat in [6.0, 7.5, 9.9] {
            same(&twice, beat, beat, "the first time");
            same(&twice, beat + 4.0, beat, "the second time");
        }
        same(&twice, 17.0, 13.0, "what followed");

        // Open four beats at 8: held there, then on as before.
        let mut room = before.clone();
        room.insert_time(8.0, 4.0).unwrap();
        for beat in [8.0, 10.0, 11.9] {
            same(&room, beat, 8.0, "through the new time");
        }
        same(&room, 7.0, 7.0, "before it");
        same(&room, 15.0, 11.0, "after it");

        // Copy 6..10 over 12..16: the copy plays what the stretch played,
        // and before it the envelope is as it was.
        let mut copied = before.clone();
        copied.copy_time(6.0, 10.0, 12.0, None, PasteMode::Replace).unwrap();
        for beat in [6.0, 7.5, 9.9] {
            same(&copied, beat + 6.0, beat, "the copy");
        }
        for beat in [3.0, 8.0, 11.9] {
            same(&copied, beat, beat, "before the copy");
        }
    }

    #[test]
    fn inserting_time_moves_what_follows_and_cuts_what_lies_across() {
        let (mut project, track, [first, long, last]) = project();
        // Four beats at beat 6, inside the long clip.
        project.insert_time(6.0, 4.0).unwrap();
        assert_eq!(spans(&project, track), [(0.0, 4.0), (4.0, 6.0), (10.0, 16.0), (16.0, 20.0)]);
        assert_eq!(sounding(&project, track), [4.0, 5.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0], "no note is lost");
        assert_eq!((project.clip(first).unwrap().start, project.clip(last).unwrap().start), (0.0, 16.0));
        assert_eq!(project.clip(long).unwrap().length, 2.0);
        let markers: Vec<Beats> = project.markers.iter().map(|marker| marker.beat).collect();
        assert_eq!(markers, [2.0, 11.0, 18.0]);
        assert_eq!((project.loop_region.start, project.loop_region.end), (12.0, 20.0));

        // At the very start of a clip nothing is cut: the clip moves whole.
        project.insert_time(0.0, 2.0).unwrap();
        assert_eq!(spans(&project, track), [(2.0, 6.0), (6.0, 8.0), (12.0, 18.0), (18.0, 22.0)]);
    }

    #[test]
    fn an_envelope_holds_through_inserted_time() {
        let ramp = envelope(&[(0.0, 0.0), (16.0, 16.0)]);
        let held = held_envelope(&ramp, STRAIGHT, 4.0, 8.0);
        assert_eq!(held, envelope(&[(0.0, 0.0), (4.0, 4.0), (12.0, 4.0), (24.0, 16.0)]));
        assert_eq!(values(&held, &[2.0, 4.0, 8.0, 12.0, 18.0, 24.0]), [2.0, 4.0, 4.0, 4.0, 10.0, 16.0]);
        // A point exactly where the time opens moves with what follows, and
        // the envelope still reaches its value at the same moment as before.
        let stepped = envelope(&[(0.0, 1.0), (4.0, 5.0), (8.0, 5.0)]);
        assert_eq!(
            held_envelope(&stepped, STRAIGHT, 4.0, 2.0),
            envelope(&[(0.0, 1.0), (4.0, 5.0), (6.0, 5.0), (10.0, 5.0)])
        );

        let (mut project, track, _) = project();
        let volume = AutomationTarget::TrackVolume { track };
        let pan = AutomationTarget::TrackPan { track };
        project.write_automation(volume.clone(), envelope(&[(0.0, -12.0), (16.0, 4.0)]), STRAIGHT).unwrap();
        project.set_automation(pan.clone(), envelope(&[(0.0, -1.0), (1.0, 0.0)])).unwrap();
        project.insert_time(8.0, 4.0).unwrap();
        let lane = |target: &AutomationTarget| {
            project.automation.iter().find(|lane| lane.target == *target).unwrap().points.clone()
        };
        assert_eq!(values(&lane(&volume), &[8.0, 10.0, 12.0, 20.0]), [-4.0, -4.0, -4.0, 4.0]);
        assert_eq!(lane(&pan), envelope(&[(0.0, -1.0), (1.0, 0.0)]), "it ended before the new time");
    }

    /// A second track with one clip over 0..16 holding a note on every
    /// beat, beside the track of [`project`].
    fn two_tracks() -> (Project, TrackId, TrackId) {
        let (mut project, first, _) = project();
        let second = project.add_track("B");
        let clip = project.add_midi_clip(second, "whole", 0.0, 16.0).unwrap();
        let notes: Vec<Note> = (0..16).map(|beat| note(beat as f64)).collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        (project, first, second)
    }

    #[test]
    fn copying_time_over_what_is_there_replaces_it_on_the_chosen_tracks_only() {
        let (mut project, first, second) = two_tracks();
        let before = project.clone();
        // Beats 6..10 (inside the long clip) onto 12..16, on the first track.
        let pasted = project.copy_time(6.0, 10.0, 12.0, Some(&[first]), PasteMode::Replace).unwrap();
        assert_eq!(pasted.len(), 1);
        assert_eq!(spans(&project, first), [(0.0, 4.0), (4.0, 12.0), (12.0, 16.0)]);
        assert_eq!(project.clip(pasted[0]).unwrap().name, "long", "the last clip gave way to a piece of the long one");
        assert_eq!(sounding(&project, first), [4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
        assert_eq!(project.track(second).unwrap(), before.track(second).unwrap(), "the other track is as it was");
        assert_eq!(project.markers, before.markers, "nothing moved, so no marker did");
        assert_eq!(project.loop_region, before.loop_region);

        // Onto a place where clips lie across the edges: they are cut there.
        let (mut project, first, second) = two_tracks();
        project.copy_time(0.0, 2.0, 5.0, None, PasteMode::Replace).unwrap();
        assert_eq!(spans(&project, first), [(0.0, 4.0), (4.0, 5.0), (5.0, 7.0), (7.0, 12.0), (12.0, 16.0)]);
        assert_eq!(sounding(&project, first), [4.0, 7.0, 8.0, 9.0, 10.0, 11.0], "the empty start of the song is there");
        assert_eq!(spans(&project, second), [(0.0, 5.0), (5.0, 7.0), (7.0, 16.0)]);
        assert_eq!(sounding(&project, second).len(), 16, "beats 0 and 1 are heard again at 5 and 6");
    }

    #[test]
    fn copying_time_with_room_made_moves_every_track_and_fills_the_chosen_ones() {
        let (mut project, first, second) = two_tracks();
        // Beats 4..8 to beat 12, making room, from the first track only.
        project.copy_time(4.0, 8.0, 12.0, Some(&[first]), PasteMode::Insert).unwrap();
        assert_eq!(spans(&project, first), [(0.0, 4.0), (4.0, 12.0), (12.0, 16.0), (16.0, 20.0)]);
        assert_eq!(sounding(&project, first)[8..], [12.0, 13.0, 14.0, 15.0]);
        assert_eq!(spans(&project, second), [(0.0, 12.0), (16.0, 20.0)], "it made room too, and was given nothing");
        let markers: Vec<Beats> = project.markers.iter().map(|marker| marker.beat).collect();
        assert_eq!(markers, [2.0, 7.0, 18.0], "markers after the new time move; none is copied");
        assert_eq!((project.loop_region.start, project.loop_region.end), (8.0, 20.0));

        // Into itself: what is copied is what the stretch held before.
        let (mut project, first, _) = two_tracks();
        project.copy_time(4.0, 8.0, 6.0, None, PasteMode::Insert).unwrap();
        assert_eq!(sounding(&project, first), [4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
        assert_eq!(spans(&project, first), [(0.0, 4.0), (4.0, 6.0), (6.0, 10.0), (10.0, 16.0), (16.0, 20.0)]);
    }

    #[test]
    fn copied_time_carries_automation_and_controller_curves() {
        let (mut project, track) = expressive();
        let volume = AutomationTarget::TrackVolume { track };
        let swell = envelope(&[(0.0, -12.0), (4.0, -12.0), (8.0, 0.0), (16.0, 0.0)]);
        project.write_automation(volume.clone(), swell, STRAIGHT).unwrap();
        let before = project.clone();
        // The first half of the swell (4..8) onto the last clip (12..16).
        project.copy_time(4.0, 8.0, 12.0, None, PasteMode::Replace).unwrap();
        for beat in [12.0, 13.0, 15.5] {
            assert_eq!(worth(&project, track, beat), worth(&before, track, beat - 8.0), "the curves, at {beat}");
        }
        let lane = project.automation.iter().find(|lane| lane.target == volume).unwrap().points.clone();
        assert_eq!(values(&lane, &[4.0, 6.0, 8.0, 11.0]), [-12.0, -6.0, 0.0, 0.0], "as it was before the copy");
        assert_eq!(values(&lane, &[12.0, 14.0, 15.0]), [-12.0, -6.0, -3.0], "the rise is heard again");
        assert_eq!(automation_value(&lane, 16.0), 0.0, "and then the envelope is where it was");

        // An excerpt can be kept and put down later, even after its track is gone.
        let (mut project, track) = expressive();
        let excerpt = project.excerpt(4.0, 8.0, Some(&[track])).unwrap();
        assert_eq!((excerpt.length, excerpt.clips.len()), (4.0, 1));
        assert_eq!(excerpt.clips[0].1.start, 0.0, "starts count from the stretch");
        project.remove_track(track).unwrap();
        assert_eq!(project.paste_time(&excerpt, 0.0, PasteMode::Replace).unwrap(), []);

        // What cannot be a stretch or a place says so and changes nothing.
        let (mut project, track) = expressive();
        let before = project.clone();
        assert!(project.copy_time(4.0, 4.0, 0.0, None, PasteMode::Replace).is_err());
        assert!(project.copy_time(4.0, 8.0, -1.0, None, PasteMode::Insert).is_err());
        assert!(project.copy_time(4.0, 8.0, 0.0, Some(&[TrackId(999)]), PasteMode::Insert).is_err());
        assert_eq!(project, before);
        let _ = track;
    }

    #[test]
    fn the_course_of_an_envelope_through_a_stretch_keeps_its_shape() {
        let ramp = envelope(&[(0.0, 0.0), (16.0, 16.0)]);
        let piece = course(&ramp, STRAIGHT, 4.0, 8.0);
        assert_eq!(values(&piece, &[0.0, 2.0, 3.99]), [4.0, 6.0, 7.99]);
        assert_eq!(piece.last().map(|point| (point.beat, point.value)), Some((4.0, 8.0)));
        // A step keeps stepping: held, then the jump, then held to the end.
        let hold = Segment::Hold;
        let steps = vec![
            AutomationPoint { beat: 0.0, value: 1.0, shape: hold, id: 0 },
            AutomationPoint { beat: 6.0, value: 5.0, shape: hold, id: 0 },
        ];
        let piece = course(&steps, STRAIGHT, 4.0, 8.0);
        assert_eq!(values(&piece, &[0.0, 1.9, 2.0, 3.9]), [1.0, 1.0, 5.0, 5.0]);
        // Past its last point an envelope is flat.
        assert_eq!(values(&course(&ramp, STRAIGHT, 20.0, 24.0), &[0.0, 4.0]), [16.0, 16.0]);
    }

    /// The project of [`project`], with a swell over the whole of the long
    /// clip (0 at beat 4, 1 at beat 12) and a pedal held from 6 to 10.
    fn expressive() -> (Project, TrackId) {
        let (mut project, track, [_, long, _]) = project();
        let clip = project.clip_mut(long).unwrap();
        clip.set_controller(crate::ControllerKind::EXPRESSION, envelope(&[(0.0, 0.0), (8.0, 1.0)])).unwrap();
        let hold = crate::Segment::Hold;
        let pedal = vec![
            AutomationPoint { beat: 2.0, value: 1.0, shape: hold, id: 0 },
            AutomationPoint { beat: 6.0, value: 0.0, shape: hold, id: 0 },
        ];
        clip.set_controller(crate::ControllerKind::SUSTAIN, pedal).unwrap();
        (project, track)
    }

    /// What the expression and the pedal are worth on the track at `beat`.
    fn worth(project: &Project, track: TrackId, beat: Beats) -> (f64, f64) {
        let lanes = project.track(track).unwrap().controller_lanes();
        let at = |kind| lanes.iter().find(|lane| lane.kind == kind).unwrap().value_at(beat);
        (at(crate::ControllerKind::EXPRESSION), at(crate::ControllerKind::SUSTAIN))
    }

    #[test]
    fn controllers_follow_their_clips_through_every_edit_of_time() {
        // Cut 6..10 out of the swell: each side plays what it played, the
        // pedal that was down over the cut is down where its clip still is.
        let (before, track) = expressive();
        let mut project = before.clone();
        project.delete_time(6.0, 10.0).unwrap();
        for beat in [4.0, 5.0, 5.75] {
            assert_eq!(worth(&project, track, beat), worth(&before, track, beat), "before the cut, at {beat}");
        }
        for beat in [6.0, 7.0, 7.75] {
            assert_eq!(worth(&project, track, beat), worth(&before, track, beat + 4.0), "after the cut, at {beat}");
        }
        assert!((worth(&project, track, 5.9).0 - 0.2375).abs() < 1e-12);
        assert_eq!(worth(&project, track, 6.0), (0.75, 0.0));

        // Repeat 6..10: the stretch is heard twice, curve and pedal alike.
        let mut project = before.clone();
        project.duplicate_time(6.0, 10.0).unwrap();
        for beat in [6.0, 7.5, 9.75] {
            assert_eq!(worth(&project, track, beat), worth(&before, track, beat), "the first time, at {beat}");
            assert_eq!(worth(&project, track, beat + 4.0), worth(&before, track, beat), "the second time, at {beat}");
        }
        for beat in [14.0, 15.5] {
            assert_eq!(worth(&project, track, beat), worth(&before, track, beat - 4.0), "what followed, at {beat}");
        }

        // Open up four beats at 8: the swell stops there, waits, and goes on;
        // the pedal, which was down, comes up over the empty time.
        let mut project = before.clone();
        project.insert_time(8.0, 4.0).unwrap();
        assert_eq!(worth(&project, track, 7.5), worth(&before, track, 7.5));
        assert_eq!(worth(&project, track, 10.0), (0.5, 0.0), "held where the swell had got to; no pedal over nothing");
        for beat in [12.0, 13.0, 15.5] {
            assert_eq!(worth(&project, track, beat), worth(&before, track, beat - 4.0), "after the room, at {beat}");
        }
    }
}
