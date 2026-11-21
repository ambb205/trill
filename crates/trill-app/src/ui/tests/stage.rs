//! The stage: the song filmed as it plays, in place of the window's panes.
//!
//! The harness has no graphics device, so these run the stage as a window
//! without one shows it: the same interface painted plain. What the stage
//! paints is tested where it is made (`trill-stage`); here, that it opens
//! and leaves, and what it lets through.

use super::*;

/// What of the window's state a person would notice had changed.
fn noticed(harness: &Harness) -> impl PartialEq + std::fmt::Debug {
    let state = harness.app.state();
    (
        (state.selected_track, state.selected_clips.clone(), state.selected_notes.clone()),
        (state.detail_tab, state.arrangement.time.origin, state.arrangement.time.pixels_per_beat),
        (state.arrangement.scroll_y, state.arrangement.follow, state.arrangement.cursor),
        (state.piano.time.origin, state.piano.time.pixels_per_beat, state.piano.pitch.top),
        (state.browser.open, state.browser.width, state.browser.tab),
    )
}

fn on_stage(harness: &Harness) -> Option<String> {
    harness.labels.iter().map(|(text, _)| text.clone()).find(|text| text.starts_with("Stage:"))
}

#[test]
fn the_stage_takes_the_window_and_leaving_it_puts_the_window_back_as_it_was() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    // A window that has been worked in: a clip selected, a note of it
    // too, the mixer up, the browser shut.
    harness.click(harness.lane(9.0, 0));
    harness.click(harness.note_cell(1.5, 60));
    harness.key(Modifiers::COMMAND, Key::B);
    harness.idle(2);
    assert_eq!(harness.selected_clips(), [clip]);
    let before = format!("{:?}", noticed(&harness));
    let steps = harness.undo_depth();
    assert!(harness.is_on_screen("File") && on_stage(&harness).is_none());

    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(2);
    assert!(harness.app.state().stage_open);
    assert!(on_stage(&harness).is_some(), "the stage is what the window shows");
    assert!(!harness.is_on_screen("File") && !harness.is_on_screen("Play"), "and none of its panes");

    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(!harness.app.state().stage_open);
    assert!(harness.is_on_screen("File") && on_stage(&harness).is_none());
    assert_eq!(format!("{:?}", noticed(&harness)), before, "nothing was touched while it was up");
    assert_eq!(harness.undo_depth(), steps, "and the song least of all");
}

#[test]
fn the_stage_takes_the_whole_screen_and_gives_it_back_on_leaving() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.app.asked_whole_screen(), None, "a window that opens keeps its size");
    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(2);
    assert_eq!(harness.app.asked_whole_screen(), Some(true), "F11 is the key people expect that of");
    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(2);
    assert_eq!(harness.app.asked_whole_screen(), Some(false), "and the window is its own size again");

    // A run that nobody watches (a picture of the stage) is of the window
    // as it was asked for.
    let picture = UiOptions { stage: true, exit_after: Some(Duration::from_secs(600)), ..Default::default() };
    let (mut harness, ..) = with_two_tracks(picture);
    harness.idle(2);
    assert!(harness.app.state().stage_open);
    assert_eq!(harness.app.asked_whole_screen(), None);
}

#[test]
fn the_stage_opens_from_its_button_and_from_the_view_menu() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click_label("Stage");
    harness.idle(2);
    assert!(harness.app.state().stage_open, "the button beside the metronome");
    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(2);
    assert!(!harness.app.state().stage_open, "the key that opens it leaves it");

    harness.click_label("View");
    harness.click_last_label("Stage");
    harness.idle(2);
    assert!(harness.app.state().stage_open, "the menu");
    assert!(harness.hint().is_empty() || !harness.is_on_screen("File"));
}

#[test]
fn on_the_stage_the_transport_answers_and_the_editing_keys_do_not() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(9.0, 0));
    assert_eq!(harness.selected_clips(), [clip]);
    let steps = harness.undo_depth();
    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(2);

    // What would delete the selected clip, add a track or open a dialog
    // in the window does nothing here.
    harness.key(Modifiers::NONE, Key::Delete);
    harness.key(Modifiers::COMMAND, Key::T);
    harness.key(Modifiers::NONE, Key::F1);
    harness.idle(2);
    assert!(harness.project().clip(clip).is_ok());
    assert_eq!(harness.project().tracks.len(), 2);
    assert!(!harness.app.state().shortcuts_open);
    assert_eq!(harness.undo_depth(), steps);

    // It is the same playback: Space starts and stops it.
    harness.key(Modifiers::NONE, Key::Space);
    harness.wait_for("playback to start", |session| session.transport().playing);
    harness.idle(2);
    assert!(harness.app.state().stage_open);
    harness.key(Modifiers::NONE, Key::Space);
    harness.wait_for("playback to stop", |session| !session.transport().playing);
}

#[test]
fn the_stage_is_of_the_song_as_it_is_now() {
    let mut harness = Harness::new(UiOptions { stage: true, ..Default::default() }, |session| {
        session
            .edit(|project| {
                let track = project.add_track("Lead");
                let clip = project.add_midi_clip(track, "line", 0.0, 64.0)?;
                let notes: Vec<Note> = (0..64)
                    .map(|beat| Note {
                        pitch: 60,
                        start: beat as f64,
                        length: 0.5,
                        velocity: 100,
                        ..Default::default()
                    })
                    .collect();
                project.clip_mut(clip)?.set_notes(notes)?;
                project.add_marker(0.0, "Intro")?;
                project.add_marker(32.0, "Chorus")?;
                Ok(())
            })
            .unwrap();
    });
    harness.idle(2);
    assert!(harness.app.state().stage_open, "opened on the stage");
    assert_eq!(on_stage(&harness).as_deref(), Some("Stage: Intro Untitled"));

    // Wherever the song is sent, the stage is there; and an edit made
    // from elsewhere while it is up is in the next picture.
    harness.session.lock().seek(40.0);
    harness.wait_for("the seek", |session| session.transport().position_beats == 40.0);
    harness.idle(2);
    assert_eq!(on_stage(&harness).as_deref(), Some("Stage: Chorus Untitled"));
    harness.session.lock().edit(|project| project.rename_marker(1, "Refrain")).unwrap();
    harness.idle(2);
    assert_eq!(on_stage(&harness).as_deref(), Some("Stage: Refrain Untitled"));
}

#[test]
fn a_recording_the_song_plays_joins_the_stage_once_it_is_read_and_one_that_is_gone_is_left_out() {
    let folder = tempfile::tempdir().unwrap();
    let take = folder.path().join("take.wav");
    let channel: Vec<f32> = (0..24_000).map(|frame| (frame as f32 * 0.05).sin() * 0.5).collect();
    let buffer = trill_core::audio::AudioBuffer { sample_rate: 48_000.0, left: channel.clone(), right: channel };
    trill_media::write_wav(&take, &buffer, trill_media::WavFormat::Int16).unwrap();
    let path = take.display().to_string();
    let mut harness = Harness::new(UiOptions { stage: true, ..Default::default() }, |session| {
        let edit = |project: &mut Project| {
            let voice = project.add_track("Voice");
            project.add_audio_clip(voice, "Take", 0.0, 8.0, trill_core::project::AudioContent::new(path.as_str()))?;
            project.add_audio_clip(
                voice,
                "Lost",
                8.0,
                8.0,
                trill_core::project::AudioContent::new("no-such-take.wav"),
            )?;
            Ok(())
        };
        session.edit(edit).unwrap();
    });
    // The stage is up at once, by its clips alone: the file is read behind it.
    assert!(on_stage(&harness).is_some());
    harness.wait_until("the recording to be read", |harness| !harness.app.stage().is_reading());
    assert_eq!(
        harness.app.stage().recordings(),
        1,
        "the take that is there; the lost one is a clip without a waveform"
    );

    // A song with nothing recorded in it reads nothing.
    let (mut notes, ..) = with_two_tracks(UiOptions { stage: true, ..Default::default() });
    notes.idle(2);
    assert!(!notes.app.stage().is_reading());
    assert_eq!(notes.app.stage().recordings(), 0);
}
