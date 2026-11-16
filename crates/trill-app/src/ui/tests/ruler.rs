//! The ruler and the transport beside it: markers, drawing the loop, the
//! time signature and the metronome.

use super::*;

fn markers(harness: &Harness) -> Vec<(f64, String)> {
    harness.project().markers.iter().map(|marker| (marker.beat, marker.name.clone())).collect()
}

/// A point on the flag of the marker at `beat`.
fn flag(harness: &Harness, beat: f64) -> Pos2 {
    harness.marker_strip(beat) + vec2(3.0, 0.0)
}

#[test]
fn markers_are_added_moved_renamed_and_deleted() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();
    // M adds one at the edit cursor.
    harness.click(harness.ruler(8.0, false));
    assert_eq!(harness.app.state().arrangement.cursor, 8.0);
    harness.key(Modifiers::NONE, Key::M);
    assert_eq!(markers(&harness), [(8.0, "Marker 1".to_owned())]);
    // A double-click on the marker strip adds one there.
    harness.double_click(harness.marker_strip(16.2));
    assert_eq!(markers(&harness), [(8.0, "Marker 1".to_owned()), (16.0, "Marker 2".to_owned())]);
    assert_eq!(harness.undo_depth(), before + 2);

    // Dragged, a marker follows the grid, as one undo step.
    harness.drag(flag(&harness, 8.0), harness.marker_strip(12.0));
    assert_eq!(markers(&harness)[0], (12.0, "Marker 1".to_owned()));
    assert_eq!(harness.undo_depth(), before + 3);
    // Past the other one, it is still the one that moves.
    harness.drag(flag(&harness, 12.0), harness.marker_strip(24.0));
    assert_eq!(markers(&harness), [(16.0, "Marker 2".to_owned()), (24.0, "Marker 1".to_owned())]);
    assert_eq!(harness.undo_depth(), before + 4);

    // A click on a marker goes there.
    harness.click(flag(&harness, 24.0));
    assert_eq!(harness.app.state().arrangement.cursor, 24.0);
    harness.wait_for("the playhead to go to the marker", |session| session.transport().position_beats == 24.0);

    // A double-click renames it in place.
    harness.double_click(flag(&harness, 16.0));
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("Chorus");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(markers(&harness)[0], (16.0, "Chorus".to_owned()));
    // M typed into a name is a letter, not a new marker.
    harness.double_click(flag(&harness, 16.0));
    harness.idle(2);
    harness.type_text("m");
    harness.key(Modifiers::NONE, Key::Escape);
    assert_eq!(markers(&harness).len(), 2);

    // The Edit menu lists the markers to jump to.
    harness.click_label("Edit");
    harness.click_label("Chorus");
    assert_eq!(harness.app.state().arrangement.cursor, 16.0);

    harness.right_click(flag(&harness, 24.0));
    harness.click_label("Delete marker");
    assert_eq!(markers(&harness), [(16.0, "Chorus".to_owned())]);
    harness.right_click(harness.marker_strip(4.0));
    harness.click_label("Add marker here");
    assert_eq!(markers(&harness)[0].0, 4.0);
}

#[test]
fn a_new_loop_is_drawn_in_the_rulers_top_strip() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!((harness.project().loop_region.start, harness.project().loop_region.end), (0.0, 16.0));
    let before = harness.undo_depth();
    // Beside the bar, a drag draws a region of its own.
    harness.drag(harness.ruler(20.2, true), harness.ruler(27.8, true));
    let region = harness.project().loop_region;
    assert_eq!((region.start, region.end), (20.0, 28.0));
    assert!(!region.enabled, "drawing the region does not switch looping on");
    assert_eq!(harness.undo_depth(), before + 1);
    // Drawn right to left it is the same region.
    harness.drag(harness.ruler(12.0, true), harness.ruler(4.0, true));
    let region = harness.project().loop_region;
    assert_eq!((region.start, region.end), (4.0, 12.0));
}

#[test]
fn the_time_signature_is_chosen_from_a_menu_or_dragged() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    let signature = |harness: &Harness| {
        let signature = harness.project().time_signature;
        (signature.numerator, signature.denominator)
    };
    harness.click_label("Time signature");
    harness.click_label("6 / 8");
    assert_eq!(signature(&harness), (6, 8));

    // Each number drags on its own: the left one is the beats in a bar.
    let before = harness.undo_depth();
    let middle = harness.labeled("Time signature");
    harness.drag(middle - vec2(12.0, 0.0), middle - vec2(12.0, 27.0));
    assert_eq!(signature(&harness), (9, 8), "three steps up");
    assert_eq!(harness.undo_depth(), before + 1, "one undo step however far it is dragged");
    harness.drag(middle + vec2(12.0, 0.0), middle + vec2(12.0, 10.0));
    assert_eq!(signature(&harness), (9, 4), "the right one is the note value of a beat");
}

#[test]
fn the_metronome_is_switched_by_its_button_and_by_k() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert!(!harness.session.lock().metronome());
    harness.click_label("Metronome");
    assert!(harness.session.lock().metronome());
    harness.key(Modifiers::NONE, Key::K);
    assert!(!harness.session.lock().metronome());
    assert_eq!(harness.undo_depth(), 1, "the metronome is not part of the song");
}
