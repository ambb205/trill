//! Edits to a stretch of time across the whole song: cutting it out so
//! what follows closes up, repeating it, and opening up empty time. Each
//! carries everything that lives on the timeline with it: clips on every
//! track, automation, markers and the loop.
//!
//! An edit that fails may leave the project half changed; run it where a
//! failure is rolled back, as the session's edits are.

use crate::project::{AutomationPoint, EditError, Project, automation_value};
use crate::time::Beats;

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

        let lanes: Vec<_> = self.automation.iter().map(|lane| (lane.target.clone(), lane.points.clone())).collect();
        for (target, points) in lanes {
            // An envelope that has settled before the cut is the same after it.
            if points.last().is_some_and(|last| last.beat >= start) {
                self.set_automation(target, cut_envelope(&points, start, end))?;
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
            let copy = self.paste_clip(track, &source, source.start + length)?;
            if source.start < start {
                // Only the part inside the stretch is repeated.
                self.trim_clip_start(copy, start + length)?;
            }
        }

        let lanes: Vec<_> = self.automation.iter().map(|lane| (lane.target.clone(), lane.points.clone())).collect();
        for (target, points) in lanes {
            if points.last().is_some_and(|last| last.beat >= start) {
                self.set_automation(target, repeated_envelope(&points, start, end))?;
            }
        }
        self.shift_marks(end, length)
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
        let lanes: Vec<_> = self.automation.iter().map(|lane| (lane.target.clone(), lane.points.clone())).collect();
        for (target, points) in lanes {
            if points.last().is_some_and(|last| last.beat >= at) {
                self.set_automation(target, held_envelope(&points, at, length))?;
            }
        }
        self.shift_marks(at, length)
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

/// `points` with `length` beats opened up at `at`, through which the
/// envelope holds the value it had there.
fn held_envelope(points: &[AutomationPoint], at: Beats, length: Beats) -> Vec<AutomationPoint> {
    let value = automation_value(points, at);
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
fn cut_envelope(points: &[AutomationPoint], start: Beats, end: Beats) -> Vec<AutomationPoint> {
    let length = end - start;
    let before = points.iter().filter(|point| point.beat < start).copied();
    let after = points
        .iter()
        .filter(|point| point.beat >= end)
        .map(|point| AutomationPoint { beat: point.beat - length, ..*point });
    let mut result: Vec<AutomationPoint> = before.collect();
    result.push(AutomationPoint::new(start, automation_value(points, start)));
    result.push(AutomationPoint::new(start, automation_value(points, end)));
    result.extend(after);
    tidied(result)
}

/// `points` with the stretch `start..end` played twice.
fn repeated_envelope(points: &[AutomationPoint], start: Beats, end: Beats) -> Vec<AutomationPoint> {
    let length = end - start;
    let later = |point: &AutomationPoint| AutomationPoint { beat: point.beat + length, ..*point };
    let mut result: Vec<AutomationPoint> = points.iter().filter(|point| point.beat < end).copied().collect();
    let at_end = automation_value(points, end);
    // The first pass ends where it did, the second starts where the first
    // started, and after it the envelope picks up again.
    result.push(AutomationPoint::new(end, at_end));
    result.push(AutomationPoint::new(end, automation_value(points, start)));
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
    use crate::{AutomationTarget, ClipId, Note, TrackId};

    fn note(start: Beats) -> Note {
        Note { pitch: 60, start, length: 0.5, velocity: 100 }
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
        let cut = cut_envelope(&ramp, 4.0, 8.0);
        assert_eq!(cut, envelope(&[(0.0, 0.0), (4.0, 4.0), (4.0, 8.0), (12.0, 16.0)]));
        assert_eq!(values(&cut, &[2.0, 3.99, 4.0, 8.0, 12.0]), [2.0, 3.99, 8.0, 12.0, 16.0]);

        // Points inside the cut go; a flat stretch needs no step.
        let stepped = envelope(&[(0.0, 1.0), (5.0, 9.0), (6.0, 1.0), (10.0, 1.0)]);
        assert_eq!(cut_envelope(&stepped, 4.0, 8.0), envelope(&[(0.0, 1.0), (4.0, 7.4), (4.0, 1.0), (6.0, 1.0)]));
        assert_eq!(
            cut_envelope(&envelope(&[(0.0, 3.0), (20.0, 3.0)]), 4.0, 8.0),
            envelope(&[(0.0, 3.0), (4.0, 3.0), (16.0, 3.0)])
        );
    }

    #[test]
    fn a_repeated_envelope_plays_the_stretch_twice() {
        let ramp = envelope(&[(0.0, 0.0), (16.0, 16.0)]);
        let twice = repeated_envelope(&ramp, 4.0, 8.0);
        assert_eq!(
            values(&twice, &[2.0, 6.0, 7.99, 8.0, 10.0, 12.0, 14.0, 20.0]),
            [2.0, 6.0, 7.99, 4.0, 6.0, 8.0, 10.0, 16.0]
        );
        // A point inside the stretch is copied with it.
        let bump = envelope(&[(0.0, 0.0), (6.0, 10.0), (8.0, 0.0)]);
        let twice = repeated_envelope(&bump, 4.0, 8.0);
        assert_eq!(values(&twice, &[6.0, 10.0, 12.0, 14.0]), [10.0, 10.0, 0.0, 0.0]);
    }

    #[test]
    fn time_edits_carry_automation_and_leave_settled_envelopes_alone() {
        let (mut project, track, _) = project();
        let volume = AutomationTarget::TrackVolume { track };
        let pan = AutomationTarget::TrackPan { track };
        project.set_automation(volume.clone(), envelope(&[(0.0, -12.0), (16.0, 4.0)])).unwrap();
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
        let held = held_envelope(&ramp, 4.0, 8.0);
        assert_eq!(held, envelope(&[(0.0, 0.0), (4.0, 4.0), (12.0, 4.0), (24.0, 16.0)]));
        assert_eq!(values(&held, &[2.0, 4.0, 8.0, 12.0, 18.0, 24.0]), [2.0, 4.0, 4.0, 4.0, 10.0, 16.0]);
        // A point exactly where the time opens moves with what follows, and
        // the envelope still reaches its value at the same moment as before.
        let stepped = envelope(&[(0.0, 1.0), (4.0, 5.0), (8.0, 5.0)]);
        assert_eq!(held_envelope(&stepped, 4.0, 2.0), envelope(&[(0.0, 1.0), (4.0, 5.0), (6.0, 5.0), (10.0, 5.0)]));

        let (mut project, track, _) = project();
        let volume = AutomationTarget::TrackVolume { track };
        let pan = AutomationTarget::TrackPan { track };
        project.set_automation(volume.clone(), envelope(&[(0.0, -12.0), (16.0, 4.0)])).unwrap();
        project.set_automation(pan.clone(), envelope(&[(0.0, -1.0), (1.0, 0.0)])).unwrap();
        project.insert_time(8.0, 4.0).unwrap();
        let lane = |target: &AutomationTarget| {
            project.automation.iter().find(|lane| lane.target == *target).unwrap().points.clone()
        };
        assert_eq!(values(&lane(&volume), &[8.0, 10.0, 12.0, 20.0]), [-4.0, -4.0, -4.0, 4.0]);
        assert_eq!(lane(&pan), envelope(&[(0.0, -1.0), (1.0, 0.0)]), "it ended before the new time");
    }

    /// The project of [`project`], with a swell over the whole of the long
    /// clip (0 at beat 4, 1 at beat 12) and a pedal held from 6 to 10.
    fn expressive() -> (Project, TrackId) {
        let (mut project, track, [_, long, _]) = project();
        let clip = project.clip_mut(long).unwrap();
        clip.set_controller(crate::ControllerKind::EXPRESSION, envelope(&[(0.0, 0.0), (8.0, 1.0)])).unwrap();
        let hold = crate::Segment::Hold;
        let pedal = vec![
            AutomationPoint { beat: 2.0, value: 1.0, shape: hold },
            AutomationPoint { beat: 6.0, value: 0.0, shape: hold },
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
