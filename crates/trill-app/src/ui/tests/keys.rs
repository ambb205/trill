//! Keys that mean different things in different panes, the piano roll's
//! share of them, and the shortcut reference.

use super::*;

fn notes(harness: &Harness, clip: ClipId) -> Vec<Note> {
    harness.project().clip(clip).unwrap().notes().to_vec()
}

/// Opens the fixture's clip in the piano roll.
fn with_open_clip() -> (Harness, ClipId) {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    (harness, clip)
}

/// A point on the piano roll's ruler at `beat` of the clip.
fn roll_ruler(harness: &Harness, beat: f64) -> Pos2 {
    pos2(harness.note_cell(beat, 60).x, harness.detail_top() + detail::TAB_BAR + piano_roll::RULER_HEIGHT / 2.0)
}

#[test]
fn notes_are_copied_cut_pasted_and_duplicated() {
    let (mut harness, clip) = with_open_clip();
    let original = Note { pitch: 60, start: 1.0, length: 1.0, velocity: 100 };
    harness.click(harness.note_cell(1.5, 60));
    assert_eq!(harness.app.state().selected_notes, [original]);
    let before = harness.undo_depth();

    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("1 note from Trill"));
    assert_eq!(notes(&harness, clip).len(), 1);
    assert_eq!(harness.project().tracks[0].clips.len(), 1, "the clip itself was not copied or touched");

    // Paste goes to the roll's cursor, which a click on its ruler places.
    harness.click(roll_ruler(&harness, 3.1));
    assert_eq!(harness.app.state().piano.cursor, 3.0);
    harness.paste();
    let pasted = Note { start: 3.0, ..original };
    assert_eq!(notes(&harness, clip), [original, pasted]);
    assert_eq!(harness.app.state().selected_notes, [pasted], "what was pasted is selected");
    assert_eq!(harness.app.state().piano.cursor, 4.0, "and the cursor has moved past it");
    assert_eq!(harness.undo_depth(), before + 1);

    // Ctrl+D repeats the selection right after itself.
    harness.key(Modifiers::COMMAND, Key::D);
    assert_eq!(notes(&harness, clip)[2], Note { start: 4.0, ..original });
    assert_eq!(harness.project().tracks[0].clips.len(), 1, "notes were duplicated, not the clip");

    // Cut takes the selection out and keeps it for pasting.
    harness.key(Modifiers::COMMAND, Key::A);
    assert_eq!(harness.app.state().selected_notes.len(), 3);
    harness.cut();
    assert!(notes(&harness, clip).is_empty());
    harness.click(roll_ruler(&harness, 0.0));
    harness.key(Modifiers::COMMAND, Key::V);
    let starts: Vec<f64> = notes(&harness, clip).iter().map(|note| note.start).collect();
    assert_eq!(starts, [0.0, 2.0, 3.0], "the phrase, with its spacing, from the cursor");
}

#[test]
fn shift_arrows_move_notes_by_octaves_and_ctrl_u_quantizes() {
    let (mut harness, clip) = with_open_clip();
    harness.click(harness.note_cell(1.5, 60));
    harness.key(Modifiers::SHIFT, Key::ArrowUp);
    assert_eq!(notes(&harness, clip)[0].pitch, 72);
    harness.key(Modifiers::NONE, Key::ArrowDown);
    assert_eq!(notes(&harness, clip)[0].pitch, 71);
    harness.key(Modifiers::SHIFT, Key::ArrowDown);
    assert_eq!(notes(&harness, clip)[0].pitch, 59);

    // Off the grid, then back onto it with one key.
    let loose = vec![
        Note { pitch: 60, start: 1.07, length: 0.5, velocity: 100 },
        Note { pitch: 64, start: 1.9, length: 0.5, velocity: 90 },
    ];
    harness.session.lock().edit(|project| project.clip_mut(clip)?.set_notes(loose)).unwrap();
    harness.idle(2);
    let before = harness.undo_depth();
    harness.key(Modifiers::COMMAND, Key::A);
    harness.key(Modifiers::COMMAND, Key::U);
    let starts: Vec<f64> = notes(&harness, clip).iter().map(|note| note.start).collect();
    assert_eq!(starts, [1.0, 2.0], "onto the sixteenth grid");
    assert_eq!(harness.undo_depth(), before + 1);
    assert_eq!(harness.app.state().selected_notes.len(), 2, "the notes stay selected where they landed");

    // Velocity is set for the whole selection at once.
    let bar = harness.labeled("Velocity");
    harness.click(bar);
    let velocities: Vec<u8> = notes(&harness, clip).iter().map(|note| note.velocity).collect();
    assert_eq!(velocities[0], velocities[1]);
    assert!((60..=68).contains(&velocities[0]), "{velocities:?}");
}

#[test]
fn the_same_keys_act_on_whatever_pane_is_being_worked_in() {
    let (mut harness, clip) = with_open_clip();
    // In the arrangement, Delete would remove the clip. After a click in
    // the piano roll it removes notes, and never the clip.
    harness.click(harness.note_cell(1.5, 60));
    harness.key(Modifiers::NONE, Key::Delete);
    assert!(notes(&harness, clip).is_empty());
    harness.key(Modifiers::NONE, Key::Delete);
    assert!(harness.project().clip(clip).is_ok(), "with no note selected, Delete does nothing here");

    // Escape drops the note selection before the clip selection.
    harness.click(harness.note_cell(2.5, 62));
    assert_eq!(harness.app.state().selected_notes.len(), 1);
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.app.state().selected_notes.is_empty());
    assert_eq!(harness.selected_clips(), [clip]);

    // Back in the arrangement the keys are the clips' again.
    harness.click(harness.lane(10.0, 0));
    harness.key(Modifiers::NONE, Key::Delete);
    assert!(harness.project().clip(clip).is_err());
}

#[test]
fn the_shortcut_reference_opens_from_the_menu_and_the_keyboard() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.key(Modifiers::NONE, Key::F1);
    assert!(harness.app.state().shortcuts_open);
    // While it is up, the keys it lists are not acted on.
    harness.key(Modifiers::NONE, Key::Space);
    harness.idle(2);
    assert!(!harness.session.lock().transport().playing);
    harness.key(Modifiers::COMMAND, Key::T);
    assert_eq!(harness.project().tracks.len(), 2);
    harness.key(Modifiers::NONE, Key::F1);
    assert!(!harness.app.state().shortcuts_open);

    harness.click_label("View");
    harness.click_label("Shortcuts");
    assert!(harness.app.state().shortcuts_open);
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(!harness.app.state().shortcuts_open, "Escape closes it");
    harness.key(Modifiers::COMMAND, Key::T);
    assert_eq!(harness.project().tracks.len(), 3, "and the keys work again");
}

#[test]
fn a_starts_the_selected_tracks_automation_and_the_view_menu_zooms() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    harness.click(harness.header(0, 120.0, 14.0));
    harness.key(Modifiers::NONE, Key::A);
    let lanes = &harness.app.state().arrangement.lanes;
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].target, trill_core::AutomationTarget::TrackVolume { track: first });
    harness.key(Modifiers::NONE, Key::A);
    assert!(harness.app.state().arrangement.lanes.is_empty());

    let whole = harness.app.state().arrangement.time;
    harness.click(harness.lane(10.0, 0));
    harness.click_label("View");
    harness.click_label("Zoom to the selection");
    harness.idle(30);
    assert!(harness.app.state().arrangement.time.pixels_per_beat > whole.pixels_per_beat * 2.0);
    harness.click_label("View");
    harness.click_label("Zoom to the whole song");
    harness.idle(30);
    assert!((harness.app.state().arrangement.time.pixels_per_beat - whole.pixels_per_beat).abs() < 1e-6);

    harness.click_label("View");
    harness.click_label("Mixer");
    assert_eq!(harness.app.state().detail_tab, DetailTab::Mixer);
}
