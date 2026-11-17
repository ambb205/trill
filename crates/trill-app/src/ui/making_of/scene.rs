//! A canned session, for the pictures of the making-of.
//!
//! A picture must look the same on every machine, so the session it shows
//! cannot be one somebody happened to have. [`play`] makes one: a short
//! tune asked of the assistant and written in a dozen calls, a look at the
//! mix, a few edits by hand a while later, and a second request. It is
//! made through the session's own doors, as a real one is (calls opened
//! and closed around the edits they make, a drag as a gesture, an undo as
//! an undo), so that what the pictures show is what the journal keeps and
//! what the replay does with it, not a drawing of either. Only the clock
//! is the scene's own: the journal is told the time it is meant to be.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use trill_core::journal::{Author, Outcome, Place, PlanItem, PlanState};
use trill_core::{AutomationPoint, AutomationTarget, ChainRef, ClipId, DeviceId, Note, Project, TrackId};
use trill_engine::Session;
use trill_engine::journal::acting;

/// The session begins on a Monday afternoon: 16 November 2026, 14:03 UTC.
const BEGAN: u64 = 1_794_837_780_000;

/// What the scene holds while it plays.
struct Scene<'a> {
    session: &'a mut Session,
    clock: Arc<AtomicU64>,
}

fn note(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

impl Scene<'_> {
    /// Lets `seconds` pass.
    fn wait(&mut self, seconds: f64) {
        self.clock.fetch_add((seconds * 1000.0) as u64, Ordering::Relaxed);
    }

    fn you(&mut self, seconds: f64, words: &str) {
        self.wait(seconds);
        self.session.say(Author::Person, words);
    }

    fn says(&mut self, seconds: f64, words: &str) {
        self.wait(seconds);
        self.session.say(Author::Assistant, words);
    }

    /// The plan with its first `done` steps done and the next in hand.
    fn plan(&mut self, steps: &[&str], done: usize) {
        self.wait(0.4);
        let items = steps.iter().enumerate().map(|(index, step)| PlanItem {
            text: (*step).to_owned(),
            state: match index.cmp(&done) {
                std::cmp::Ordering::Less => PlanState::Done,
                std::cmp::Ordering::Equal => PlanState::Doing,
                std::cmp::Ordering::Greater => PlanState::Pending,
            },
        });
        self.session.note_plan(items.collect());
    }

    /// A call of the assistant's that takes `seconds` and changes the song
    /// as `edit` does; `place` says where it landed.
    fn call<T>(
        &mut self,
        seconds: f64,
        tool: &str,
        words: &str,
        edit: impl FnOnce(&mut Session) -> T,
        place: impl FnOnce(&T) -> Option<Place>,
    ) -> T {
        self.wait(seconds);
        let call = self.session.begin_call(Author::Assistant, tool, words);
        let made = {
            let _acting = call.map(acting);
            edit(self.session)
        };
        self.wait(0.3);
        if let Some(call) = call {
            let places = place(&made).and_then(|place| place.settled(self.session.project())).into_iter().collect();
            self.session.end_call(call, Outcome { places, ..Outcome::default() });
        }
        made
    }

    /// A call that changed nothing; `heard` is what it heard, if it
    /// listened.
    fn look(&mut self, seconds: f64, tool: &str, words: &str, heard: Option<&str>) {
        self.wait(seconds);
        if let Some(call) = self.session.begin_call(Author::Assistant, tool, words) {
            self.session.end_call(call, Outcome { heard: heard.map(str::to_owned), ..Outcome::default() });
        }
    }

    fn track(&mut self, seconds: f64, name: &str, instrument: &str, preset: Option<&str>) -> (TrackId, DeviceId) {
        let words = format!("Added the track \u{201C}{name}\u{201D}");
        self.call(
            seconds,
            "create_track",
            &words,
            |session| {
                let track = session.edit(|project| Ok(project.add_track(name))).expect("a track can be added");
                let device = session
                    .add_device(ChainRef::Track(track), instrument, preset, None)
                    .expect("a built-in instrument can be added");
                (track, device)
            },
            |(track, _)| Some(Place::track(track.0)),
        )
    }

    /// A clip written whole by one call.
    #[allow(clippy::too_many_arguments)]
    fn write(
        &mut self,
        seconds: f64,
        tool: &str,
        what: &str,
        track: TrackId,
        name: &str,
        (start, length, loop_length): (f64, f64, Option<f64>),
        notes: Vec<Note>,
    ) -> ClipId {
        let on = self.session.project().track(track).map(|track| track.name.clone()).unwrap_or_default();
        let words = format!("Wrote {what} on \u{201C}{on}\u{201D}");
        self.call(
            seconds,
            tool,
            &words,
            |session| {
                let edit = |project: &mut Project| {
                    let clip = project.add_midi_clip(track, name, start, length)?;
                    let held = project.clip_mut(clip)?;
                    held.add_notes(&notes)?;
                    held.set_loop_length(loop_length)?;
                    Ok(clip)
                };
                session.edit(edit).expect("a clip can be written")
            },
            |clip| Place::clips([clip.0]),
        )
    }
}

/// One bar of the drums: kick, snare and hats, with the hats' accents on
/// the beat.
fn drum_bar(bar: usize, busy: bool) -> Vec<Note> {
    let at = bar as f64 * 4.0;
    let mut notes = vec![note(36, at, 0.25, 118), note(36, at + 2.5, 0.25, 104), note(38, at + 1.0, 0.25, 110)];
    notes.push(note(38, at + 3.0, 0.25, 114));
    for eighth in 0..8 {
        let velocity = if eighth % 2 == 0 { 84 } else { 62 };
        notes.push(note(42, at + eighth as f64 * 0.5, 0.1, velocity));
    }
    if busy {
        notes.extend([note(36, at + 1.75, 0.25, 92), note(46, at + 3.5, 0.25, 88)]);
    }
    notes
}

/// The chords of the tune, two bars each: A minor, F, C, G.
const ROOTS: [u8; 4] = [45, 41, 48, 43];
const CHORDS: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [55, 60, 64], [55, 59, 62]];

/// Plays the canned session into `session`, which ends up holding the
/// tune and the journal of how it was made.
pub fn play(session: &mut Session) {
    let clock = Arc::new(AtomicU64::new(BEGAN));
    let reads = clock.clone();
    session.set_journal_clock(Arc::new(move || reads.load(Ordering::Relaxed)));
    // The story begins on an empty song.
    session.new_project();
    let mut scene = Scene { session, clock };

    let steps = ["Set the tempo and the drums", "Write the bass", "Add the chords", "Write a melody", "Check the mix"];
    scene.you(2.0, "Write me a short tune for a night drive: drums, a bass line, chords and a melody. Around 96.");
    scene.says(6.0, "A night drive at 96 BPM, in **A minor**. I\u{2019}ll lay down the drums first, then build up from the bass.");
    scene.plan(&steps, 0);
    scene.look(1.5, "get_project", "Read the project", None);
    scene.call(
        2.0,
        "set_project",
        "Changed the name and tempo",
        |session| {
            let edit = |project: &mut Project| {
                project.name = "Night Drive".into();
                project.set_tempo(96.0)
            };
            session.edit(edit).expect("the tempo can be set");
        },
        |()| None,
    );
    let (drums, _) = scene.track(3.0, "Drums", "builtin:drumkit", None);
    let pattern: Vec<Note> = (0..4).flat_map(|bar| drum_bar(bar, bar == 3)).collect();
    scene.write(9.0, "write_drums", "drums", drums, "Beat", (0.0, 64.0, Some(16.0)), pattern);

    scene.plan(&steps, 1);
    scene.says(5.0, "Drums are in. Now a bass line on the roots, an octave below the chords to come.");
    let (bass, _) = scene.track(3.0, "Bass", "builtin:polysynth", Some("bass"));
    let line: Vec<Note> = (0..64)
        .filter(|step| step % 8 != 7)
        .map(|step| {
            let root = ROOTS[step / 16] - 12;
            let pitch = if step % 8 == 6 { root + 7 } else { root };
            note(pitch, step as f64 * 0.5, 0.4, if step % 4 == 0 { 104 } else { 88 })
        })
        .collect();
    scene.write(11.0, "write_bass", "a bass line", bass, "Bass line", (0.0, 32.0, None), line.clone());
    scene.call(
        2.5,
        "duplicate_clip",
        "Duplicated \u{201C}Bass line\u{201D}",
        |session| {
            let clip = session.project().track(bass).expect("the bass is there").clips[0].id;
            session.edit(|project| project.duplicate_clip(clip, 32.0, None)).expect("a clip can be copied")
        },
        |copy| Place::clips([copy.0]),
    );

    scene.plan(&steps, 2);
    let (pad, pad_synth) = scene.track(4.0, "Pad", "builtin:polysynth", Some("warm_pad"));
    let chords: Vec<Note> = (0..8)
        .flat_map(|change| {
            let chord = CHORDS[change % 4];
            chord.into_iter().map(move |pitch| note(pitch, change as f64 * 8.0, 7.5, 76))
        })
        .collect();
    scene.write(8.0, "write_chords", "chords", pad, "Pad chords", (0.0, 64.0, None), chords);
    scene.call(
        3.0,
        "add_device",
        "Added reverb to \u{201C}Pad\u{201D}",
        |session| session.add_device(ChainRef::Track(pad), "builtin:reverb", None, None).expect("a reverb can be added"),
        |device| Some(Place::device(device.0)),
    );
    let cutoff = AutomationTarget::DeviceParam { device: pad_synth, param: "cutoff".into() };
    let sweep = cutoff.clone();
    scene.call(
        4.0,
        "set_automation",
        "Automated cutoff of \u{201C}Poly Synth\u{201D}",
        |session| {
            let points = vec![AutomationPoint::new(0.0, 600.0), AutomationPoint::new(32.0, 2400.0), AutomationPoint::new(64.0, 900.0)];
            session.edit(|project| project.set_automation(sweep, points)).expect("an envelope can be written");
        },
        |()| Some(Place::Lane { target: cutoff }),
    );

    scene.plan(&steps, 3);
    scene.says(4.0, "Chords and a slow filter sweep on the pad. The melody comes in at bar 9, once the groove has settled.");
    let (lead, _) = scene.track(3.0, "Lead", "builtin:polysynth", Some("lead"));
    let phrase = [(69u8, 0.0, 1.5), (72, 1.5, 0.5), (76, 2.0, 1.0), (74, 3.0, 0.5), (72, 3.5, 0.5), (69, 4.0, 2.0), (67, 6.0, 1.0), (64, 7.0, 1.0)];
    let melody: Vec<Note> = (0..4)
        .flat_map(|turn| {
            let lift = if turn % 2 == 1 { 3 } else { 0 };
            phrase.into_iter().map(move |(pitch, start, length)| note(pitch + lift, turn as f64 * 8.0 + start, length * 0.9, 96))
        })
        .collect();
    let lead_clip = scene.write(12.0, "write_melody", "a melody", lead, "Lead line", (32.0, 32.0, None), melody);

    scene.plan(&steps, 4);
    scene.look(14.0, "measure", "Measured the song", Some("\u{2212}17.8 LUFS, peak \u{2212}4.1 dB, 40.0 s"));
    scene.call(
        2.0,
        "update_track",
        "Changed the volume of \u{201C}Pad\u{201D}",
        |session| session.edit(|project| project.set_track_volume(pad, -7.5)).expect("a fader can be set"),
        |()| Some(Place::track(pad.0)),
    );
    scene.call(
        1.5,
        "update_track",
        "Changed the volume and pan of \u{201C}Lead\u{201D}",
        |session| {
            let edit = |project: &mut Project| {
                project.set_track_volume(lead, -4.0)?;
                project.set_track_pan(lead, 0.15)
            };
            session.edit(edit).expect("a fader can be set");
        },
        |()| Some(Place::track(lead.0)),
    );
    scene.look(11.0, "measure", "Measured the song", Some("\u{2212}15.1 LUFS, peak \u{2212}2.6 dB, 40.0 s"));
    scene.plan(&steps, 5);
    scene.says(
        3.0,
        "Done. **Night Drive** is sixteen bars: drums and bass from the top, the pad opening up over the first eight, \
         and the lead from bar 9. The pad sat a little loud against the lead, so I brought it down.",
    );
    scene.wait(0.5);
    scene.session.mark_journal(Author::Assistant, "Finished in 2 min 38 s");

    // Twelve minutes later, by hand: the bass down a little (a drag), the
    // second half of the bass moved up an octave and put back.
    scene.wait(12.0 * 60.0);
    scene.session.begin_gesture();
    for step in 1..=8 {
        scene.wait(0.05);
        scene.session.edit(|project| project.set_track_volume(bass, -(step as f32) * 0.5)).expect("a fader moves");
    }
    scene.session.end_gesture();
    scene.wait(20.0);
    let second = scene.session.project().track(bass).expect("the bass is there").clips[1].id;
    let raised: Vec<Note> = line.iter().map(|held| Note { pitch: held.pitch + 12, ..*held }).collect();
    scene.session.edit(|project| project.clip_mut(second)?.set_notes(raised)).expect("notes can be rewritten");
    scene.wait(6.0);
    scene.session.undo();

    // And a second request.
    scene.you(40.0, "The melody is too busy in its second half. Thin it out and let the last note ring.");
    scene.look(3.0, "get_notes", "Read the notes of \u{201C}Lead line\u{201D}", None);
    scene.call(
        7.0,
        "set_notes",
        "Rewrote \u{201C}Lead line\u{201D} with 26 notes",
        |session| {
            let thin = |project: &mut Project| {
                let clip = project.clip_mut(lead_clip)?;
                let mut notes: Vec<Note> = clip.notes().iter().filter(|held| held.start < 16.0 || held.length > 0.6).copied().collect();
                if let Some(last) = notes.last_mut() {
                    last.length = 4.0;
                }
                clip.set_notes(notes)
            };
            session.edit(thin).expect("notes can be rewritten");
        },
        |()| Place::clips([lead_clip.0]),
    );
    scene.says(2.5, "Thinned out from bar 13: the short passing notes are gone and the last note rings for a bar.");
    scene.wait(0.4);
    scene.session.mark_journal(Author::Assistant, "Finished in 14 s");
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::journal::replay::Replay;
    use trill_dsp::BuiltinFactory;
    use trill_engine::AudioBackend;

    #[test]
    fn the_canned_session_is_a_song_and_the_story_of_it() {
        let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        play(&mut session);
        let project = session.project();
        let names: Vec<&str> = project.tracks.iter().map(|track| track.name.as_str()).collect();
        assert_eq!((project.name.as_str(), project.tempo, names), ("Night Drive", 96.0, vec!["Drums", "Bass", "Pad", "Lead"]));
        assert!(session.problems().is_empty(), "{:?}", session.problems());

        let journal = session.journal().expect("a journal is kept");
        assert_eq!(&journal.head().unwrap(), project, "the story leads to the song");
        let by = |author: &Author| journal.entries().filter(|entry| entry.by == *author && entry.step().is_some()).count();
        assert_eq!((by(&Author::Assistant), by(&Author::Person)), (15, 3));
        assert_eq!(journal.entries().filter(|entry| entry.is_conversation()).count(), 13);

        // Sixteen minutes by the clock, and under a minute to watch.
        let replay = Replay::new(journal.clone());
        assert!(replay.broken().is_none());
        assert!(replay.length() > 30.0 && replay.length() < 60.0, "{} s", replay.length());
        let away: Vec<f64> = replay.moments().iter().filter_map(|moment| moment.away).collect();
        assert_eq!(away.len(), 1, "one stretch where nobody was there: {away:?}");

        // Played twice, it is the same session.
        let mut again = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        play(&mut again);
        assert_eq!(again.journal().unwrap().to_text(true), journal.to_text(true));
    }
}
