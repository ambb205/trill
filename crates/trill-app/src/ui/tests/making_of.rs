//! The making-of, driven through the window: a session that was made
//! through the session's own doors (calls of the assistant's, edits by
//! hand, a conversation) is played again, watched, scrubbed and left.

use super::*;
use crate::ui::MakingOfStart;
use trill_core::journal::{Author, Outcome, PlanItem, PlanState};
use trill_engine::journal::acting;

const CTRL_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

fn note_at(start: f64, pitch: u8) -> Note {
    Note { pitch, start, length: 0.4, velocity: 100, ..Default::default() }
}

/// One call of the assistant's that edits the song.
fn call<T>(session: &mut Session, tool: &str, words: &str, edit: impl FnOnce(&mut Session) -> T) -> T {
    let call = session.begin_call(Author::Assistant, tool, words).expect("a journal is kept");
    let made = {
        let _acting = acting(call);
        edit(session)
    };
    session.end_call(call, Outcome::default());
    made
}

/// A short session: a request, a plan, a bass track with a line of sixteen
/// notes, drums with a beat, a look at the mix, an answer, and a fader
/// moved by hand. Returns the bass track and its clip.
fn a_session(session: &mut Session) -> (TrackId, ClipId) {
    session.say(Author::Person, "Give me a bass line and a beat.");
    session.note_plan(vec![
        PlanItem { text: "Write the bass".into(), state: PlanState::Doing },
        PlanItem { text: "Write the drums".into(), state: PlanState::Pending },
    ]);
    session.say(Author::Assistant, "A bass line first, then the **drums**.");
    let bass = call(session, "create_track", "Added the track \u{201C}Bass\u{201D}", |session| {
        let track = session.edit(|project| Ok(project.add_track("Bass"))).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        track
    });
    let line = call(session, "write_bass", "Wrote a bass line on \u{201C}Bass\u{201D}", |session| {
        let edit = |project: &mut Project| {
            let clip = project.add_midi_clip(bass, "Line", 0.0, 16.0)?;
            let notes: Vec<Note> = (0..16).map(|step| note_at(step as f64, 36 + (step % 4) as u8)).collect();
            project.clip_mut(clip)?.add_notes(&notes)?;
            Ok(clip)
        };
        session.edit(edit).unwrap()
    });
    session.note_plan(vec![
        PlanItem { text: "Write the bass".into(), state: PlanState::Done },
        PlanItem { text: "Write the drums".into(), state: PlanState::Doing },
    ]);
    let drums = call(session, "create_track", "Added the track \u{201C}Drums\u{201D}", |session| {
        let track = session.edit(|project| Ok(project.add_track("Drums"))).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:drumkit", None, None).unwrap();
        track
    });
    call(session, "write_drums", "Wrote drums on \u{201C}Drums\u{201D}", |session| {
        let edit = |project: &mut Project| {
            let clip = project.add_midi_clip(drums, "Beat", 0.0, 16.0)?;
            let notes: Vec<Note> = (0..8).map(|step| note_at(step as f64 * 2.0, 36)).collect();
            project.clip_mut(clip)?.add_notes(&notes)
        };
        session.edit(edit).unwrap();
    });
    let measured = session.begin_call(Author::Assistant, "measure", "Measured the song").unwrap();
    session.end_call(measured, Outcome { heard: Some("\u{2212}16.0 LUFS".into()), ..Outcome::default() });
    session.say(Author::Assistant, "Done: a bass line and a beat, four bars each.");
    session.mark_journal(Author::Assistant, "Finished in 40 s");
    // And by hand, a fader.
    session.edit(|project| project.set_track_volume(bass, -6.0)).unwrap();
    (bass, line)
}

/// The window on the short session, with its making-of up and held at
/// the beginning.
fn watching() -> (Harness, TrackId, ClipId) {
    let mut made = None;
    let options = UiOptions { making_of: Some(MakingOfStart { at: 0.0, playing: false }), ..UiOptions::default() };
    let mut harness = Harness::new(options, |session| made = Some(a_session(session)));
    harness.read_the_journal();
    let (bass, line) = made.expect("the session was made");
    (harness, bass, line)
}

impl Harness {
    /// Runs frames until the making-of has read its journal: that happens
    /// on a helper thread, by the clock on the wall.
    fn read_the_journal(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.idle(2);
            if !self.is_on_screen("Reading the journal") {
                return;
            }
            assert!(Instant::now() < deadline, "the journal was never read");
            std::thread::yield_now();
        }
    }

    fn replay(&self) -> &crate::ui::making_of::Watching {
        self.app.making_of().and_then(|making_of| making_of.watching()).expect("the making-of is up")
    }

    /// The song as the making-of shows it.
    fn shown_song(&self) -> &Project {
        &self.replay().snapshot().project
    }

    /// Whether something on screen is labeled exactly `label`.
    fn tells(&self, label: &str) -> bool {
        self.labels.iter().any(|(text, _)| text == label)
    }
}

#[test]
fn a_session_is_played_again_and_the_clips_arrive() {
    let (mut harness, bass, line) = watching();
    // Held at the beginning: the song as it began, and nothing told yet
    // but what was asked.
    assert!(harness.shown_song().tracks.is_empty());
    assert!(!harness.replay().is_playing());
    assert!(harness.tells("You: Give me a bass line and a beat."), "{:?}", harness.labels);
    assert!(!harness.tells("The assistant: Added the track \u{201C}Bass\u{201D}"));
    let length = harness.replay().replay().length();
    assert!(length > 5.0 && length < 20.0, "a replay of {length} s");

    // Played: time passes at the replay's own pace, the tracks appear, and
    // the clip's notes arrive a few at a time, in the order they sound.
    harness.key(Modifiers::NONE, Key::Space);
    assert!(harness.replay().is_playing());
    let mut seen: Vec<(usize, usize)> = Vec::new();
    let mut frames = 0;
    while harness.replay().is_playing() {
        harness.idle(1);
        frames += 1;
        assert!(frames < 60 * 40, "the replay never came to its end");
        let song = harness.shown_song();
        let notes = song.clip(line).map_or(0, |clip| clip.notes().len());
        if seen.last() != Some(&(song.tracks.len(), notes)) {
            seen.push((song.tracks.len(), notes));
        }
    }
    assert!((frames as f64 * FRAME - length).abs() < 0.2, "{frames} frames for {length} s");
    let tracks: Vec<usize> = seen.iter().map(|(tracks, _)| *tracks).collect();
    assert!(tracks.windows(2).all(|pair| pair[0] <= pair[1]) && tracks.contains(&1) && tracks.last() == Some(&2));
    let notes: Vec<usize> = seen.iter().map(|(_, notes)| *notes).collect();
    assert!(notes.windows(2).all(|pair| pair[0] <= pair[1]), "notes only ever arrive: {notes:?}");
    let steps: std::collections::BTreeSet<usize> = notes.iter().copied().collect();
    assert!(steps.len() >= 12, "the line was watched arriving, not put there at once: {notes:?}");
    // At the end the song is the song as it is, and the story is told.
    assert_eq!(harness.shown_song().tracks.len(), 2);
    assert_eq!(harness.shown_song().track(bass).unwrap().volume_db, -6.0);
    assert_eq!(harness.shown_song().clip(line).unwrap().notes(), harness.project().clip(line).unwrap().notes());
    for told in [
        "The assistant: Added the track \u{201C}Bass\u{201D}",
        "The assistant: Wrote a bass line on \u{201C}Bass\u{201D}",
        "The assistant: Measured the song: \u{2212}16.0 LUFS",
        "The assistant: Done: a bass line and a beat, four bars each.",
        "Finished in 40 s",
        "You: Changed the volume of \u{201C}Bass\u{201D}",
        "Plan: 1 of 2 done",
        "Write the drums: in hand",
    ] {
        let story: Vec<&str> =
            harness.labels.iter().map(|(text, _)| text.as_str()).filter(|text| text.len() > 12).collect();
        assert!(harness.tells(told), "the story does not tell '{told}': {story:?}");
    }
    // The view went where the action was: the Clip tab is on a clip that
    // was written, without anyone having selected it.
    let view = harness.replay().view();
    assert!(view.selected_clip.is_some() && view.detail_tab == DetailTab::Clip);
    assert_eq!(harness.app.state().selected_clip, None, "the window's own selection is left alone");

    // Space at the end plays it again from the beginning.
    harness.key(Modifiers::NONE, Key::Space);
    assert!(harness.replay().is_playing() && harness.replay().time() < 1.0);
    assert!(harness.shown_song().tracks.is_empty());
}

#[test]
fn the_replay_is_stepped_through_scrubbed_and_sped_up() {
    let (mut harness, _, line) = watching();
    // Right goes to the next step, Left back; the song follows.
    harness.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!(harness.shown_song().tracks.len(), 1, "the first step: the bass track");
    assert_eq!(harness.replay().steps(), (1, 5));
    harness.key(Modifiers::NONE, Key::ArrowRight);
    harness.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!((harness.replay().steps(), harness.shown_song().tracks.len()), ((3, 5), 2));
    harness.key(Modifiers::NONE, Key::ArrowLeft);
    assert_eq!(harness.replay().steps(), (2, 5));
    harness.key(Modifiers::NONE, Key::Home);
    assert_eq!((harness.replay().time(), harness.shown_song().tracks.len()), (0.0, 0));
    // The buttons do the same.
    harness.click_label("Step forward");
    assert_eq!(harness.replay().steps(), (1, 5));
    harness.click_label("Step back");
    assert_eq!(harness.replay().steps(), (0, 5));

    // A click on the line goes there; a drag scrubs, both ways.
    let session_line = harness.all_labeled("Session")[0];
    harness.click(pos2(session_line.right() - 14.0, session_line.center().y));
    assert_eq!(harness.replay().steps(), (5, 5));
    assert_eq!(harness.shown_song().clip(line).unwrap().notes().len(), 16);
    let (from, to) = (session_line.right() - 14.0, session_line.left() + 14.0);
    harness.drag(pos2(from, session_line.center().y), pos2(to, session_line.center().y));
    assert!(harness.replay().time() < 0.1, "scrubbed back to the beginning: {}", harness.replay().time());
    assert!(harness.shown_song().tracks.is_empty());
    assert!(!harness.replay().is_playing());

    // Twice the speed is half the frames.
    harness.click_label("2\u{00D7}");
    assert_eq!(harness.replay().speed(), 2.0);
    harness.click_label("Play");
    let length = harness.replay().replay().length();
    let mut frames = 0;
    while harness.replay().is_playing() {
        harness.idle(1);
        frames += 1;
        assert!(frames < 60 * 40);
    }
    assert!((frames as f64 * FRAME * 2.0 - length).abs() < 0.3, "{frames} frames for {length} s at twice the speed");
    // A click on the song holds it or plays it; it does nothing else.
    harness.click(pos2(600.0, 300.0));
    assert!(harness.replay().is_playing());
    harness.click(pos2(600.0, 300.0));
    assert!(!harness.replay().is_playing());
}

#[test]
fn nothing_done_while_watching_reaches_the_song_and_leaving_puts_the_window_back() {
    let mut made = None;
    let mut harness = Harness::new(UiOptions::default(), |session| made = Some(a_session(session)));
    let (bass, line) = made.unwrap();
    // The window as the user had it: a clip selected, the mixer open.
    harness.click_label("Mixer");
    harness.app.state_mut().select_clip(bass, line);
    harness.idle(2);
    let before = harness.project();
    let revision = harness.session.lock().revision();
    let entries = harness.session.lock().journal().unwrap().len();

    // Opened with its shortcut, it plays from the beginning.
    harness.key(CTRL_SHIFT, Key::M);
    harness.read_the_journal();
    assert!(harness.replay().is_playing());
    harness.key(Modifiers::NONE, Key::Space);
    harness.key(Modifiers::NONE, Key::ArrowRight);
    harness.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!(harness.shown_song().tracks.len(), 1);
    let (shown, time) = (harness.shown_song().clone(), harness.replay().time());

    // Everything a hand might do by accident: clicks and drags on the
    // track's controls and its clip, a double-click on an empty lane, the
    // keys that edit, undo, save and new.
    for label in ["Mute", "Solo", "Arm", "Volume"] {
        if harness.is_on_screen(label) {
            let at = harness.labeled(label);
            harness.click(at);
            harness.drag(at, at + vec2(40.0, 0.0));
        }
    }
    harness.drag(pos2(420.0, 140.0), pos2(700.0, 260.0));
    harness.double_click(pos2(900.0, 400.0));
    harness.double_click(pos2(500.0, 760.0));
    harness.drag(pos2(500.0, 760.0), pos2(640.0, 700.0));
    for (modifiers, key) in [
        (Modifiers::NONE, Key::Delete),
        (Modifiers::COMMAND, Key::Z),
        (Modifiers::COMMAND, Key::D),
        (Modifiers::COMMAND, Key::A),
        (Modifiers::COMMAND, Key::T),
        (Modifiers::COMMAND, Key::N),
        (Modifiers::NONE, Key::Q),
        (Modifiers::NONE, Key::M),
    ] {
        harness.key(modifiers, key);
    }
    harness.type_text("x");
    harness.idle(5);
    assert_eq!(harness.project(), before, "the song is as it was");
    assert_eq!(harness.session.lock().revision(), revision, "nothing was even tried on it");
    assert_eq!(harness.session.lock().journal().unwrap().len(), entries);
    assert!(harness.app.making_of().is_some(), "and the making-of is still up");
    // The clicks on the song held it and played it; nothing else moved.
    let watching = harness.app.making_of_mut().unwrap().watching_mut().unwrap();
    if watching.is_playing() {
        watching.toggle();
    }
    watching.seek(time);
    harness.idle(1);
    assert_eq!(harness.shown_song(), &shown, "the replay shows the song as it was, untouched too");

    // Meanwhile the song itself can still be changed from elsewhere (an
    // assistant at work), and the replay, a copy, is not disturbed.
    harness.session.lock().edit(|project| project.set_tempo(77.0)).unwrap();
    harness.idle(3);
    assert_eq!(harness.shown_song().tempo, 120.0);

    // Leaving puts the window back as it was: the clip selected, the
    // mixer open, the song as it now is.
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(harness.app.making_of().is_none());
    assert_eq!(harness.app.state().selected_clip, Some(line));
    assert_eq!(harness.app.state().detail_tab, DetailTab::Mixer);
    assert_eq!(harness.app.snapshot().project.tempo, 77.0);
    assert!(harness.is_on_screen("File"), "the window's own bar is back");
    // And the song can be edited again.
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(harness.project().tempo, 120.0);

    // The View menu opens it too, and the Leave button leaves.
    harness.click_label("View");
    harness.click_label("Making of");
    harness.read_the_journal();
    assert!(harness.app.making_of().is_some());
    harness.click_label("Leave");
    harness.idle(2);
    assert!(harness.app.making_of().is_none());
}

#[test]
fn a_song_nothing_has_happened_to_says_so_and_a_journal_can_be_switched_off_and_deleted() {
    let folder = tempfile::tempdir().unwrap();
    let storage = trill_runtime::Storage::At { settings: folder.path().join("settings.json"), library_index: None };
    let options = RuntimeOptions { storage, ..RuntimeOptions::hermetic() };
    let runtime = Arc::new(Runtime::start(options).unwrap());
    let mut harness = Harness::on(runtime, UiOptions::default(), |_| {});
    harness.key(CTRL_SHIFT, Key::M);
    harness.read_the_journal();
    assert!(harness.is_on_screen("Nothing has happened in this song since its journal began."), "{:?}", harness.labels);
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.app.making_of().is_none());

    // Something happens, and the journal has it; then it is deleted, from
    // the View menu, after a question.
    harness.session.lock().edit(|project| Ok(project.add_track("Keys"))).unwrap();
    harness.session.lock().say(Author::Person, "Something said.");
    assert_eq!(harness.session.lock().journal().unwrap().len(), 2);
    harness.click_label("View");
    harness.click_label("Delete this song\u{2019}s journal");
    harness.idle(3);
    assert!(harness.is_on_screen("How \"Untitled\" was made, step by step"), "{:?}", harness.labels);
    harness.click_label("Cancel");
    assert_eq!(harness.session.lock().journal().unwrap().len(), 2, "not without a yes");
    harness.click_label("View");
    harness.click_label("Delete this song\u{2019}s journal");
    harness.click_label("Delete");
    harness.idle(2);
    assert!(harness.session.lock().journal().unwrap().is_empty());
    assert_eq!(harness.project().tracks.len(), 1, "the song is not touched");

    // Switched off from the same menu, and remembered for the next run.
    harness.click_label("View");
    harness.click_label("Keep a journal");
    harness.idle(2);
    assert!(!harness.session.lock().keeps_journal());
    assert!(harness.app.sounds_mut().wait_until_idle(Duration::from_secs(10)));
    harness.idle(2);
    assert!(!trill_library::Settings::load(&folder.path().join("settings.json")).settings.journal.keep);
    // With none kept, the making-of says what to do about it.
    harness.key(CTRL_SHIFT, Key::M);
    harness.read_the_journal();
    assert!(harness.is_on_screen("No journal is kept"), "{:?}", harness.labels);
    harness.click_label("Leave");
    harness.click_label("View");
    harness.click_label("Keep a journal");
    harness.idle(2);
    assert!(harness.session.lock().keeps_journal());
}
