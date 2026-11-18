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

/// What the tempo does along the song, as `(beat, tempo, glides)`.
fn tempos(harness: &Harness) -> Vec<(f64, f64, bool)> {
    harness.project().tempo_changes.iter().map(|change| (change.beat, change.tempo, change.ramp)).collect()
}

/// Where the time signature changes, as `(beat, beats, note)`.
fn signatures(harness: &Harness) -> Vec<(f64, u8, u8)> {
    let changes = harness.project().meter_changes;
    changes.iter().map(|change| (change.beat, change.signature.numerator, change.signature.denominator)).collect()
}

#[test]
fn the_tempo_and_the_time_signature_change_along_the_ruler_and_are_edited_there() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();

    // Set at the foot of the ruler, from its menu: the time signature
    // from bar 3 on, typed where it is to be.
    harness.right_click(harness.ruler(8.0, false));
    harness.click_label("Set the time signature at 3\u{2026}");
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("7/8");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(signatures(&harness), [(8.0, 7, 8)]);
    assert!(harness.is_on_screen("At bar 3: 7/8"), "the ruler says so where it changes");
    // And the tempo, at the same bar: the two are said together.
    harness.right_click(harness.ruler(8.0, false));
    harness.click_label("Set the tempo at 3\u{2026}");
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("140");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(tempos(&harness), [(8.0, 140.0, false)]);
    assert!(harness.is_on_screen("At bar 3: 7/8 \u{00B7} 140 BPM"));
    assert_eq!(harness.undo_depth(), before + 2, "each is one step");

    // The ruler counts the song's own bars: bar 4 is three and a half
    // beats after bar 3, and a click there lands on it.
    harness.click(harness.ruler(11.6, false));
    assert_eq!(harness.app.state().arrangement.cursor, 11.5, "snapped to the bar line of the 7/8");
    harness.wait_for("the playhead to go there", |session| session.transport().position_beats == 11.5);
    harness.idle(3);
    // Where the song is, the top bar shows the tempo and the time
    // signature it has there.
    assert!(harness.is_on_screen("Tempo 140.00 BPM") || harness.is_on_screen("Tempo"), "the tempo field is there");
    let project = harness.project();
    assert_eq!(project.tempo, 120.0, "the song still begins at 120");

    // Typed over with a double-click: the tempo, where the tempo changes.
    let words = harness.labeled("At bar 3: 7/8 \u{00B7} 140 BPM");
    harness.double_click(words);
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("132.5");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(tempos(&harness), [(8.0, 132.5, false)]);

    // Dragged along the ruler, a change goes to another bar line, tempo
    // and time signature together, as one step.
    let depth = harness.undo_depth();
    let words = harness.labeled("At bar 3: 7/8 \u{00B7} 132.5 BPM");
    harness.drag(words, pos2(harness.ruler(16.0, false).x + (words.x - harness.ruler(8.0, false).x), words.y));
    assert_eq!((tempos(&harness), signatures(&harness)), (vec![(16.0, 132.5, false)], vec![(16.0, 7, 8)]));
    assert_eq!(harness.undo_depth(), depth + 1);
    assert!(harness.is_on_screen("At bar 5: 7/8 \u{00B7} 132.5 BPM"));

    // From its menu a tempo becomes one the song glides to, and either
    // change is removed.
    let words = harness.labeled("At bar 5: 7/8 \u{00B7} 132.5 BPM");
    harness.right_click(words);
    harness.click_label("Glide to this tempo");
    assert_eq!(tempos(&harness), [(16.0, 132.5, true)]);
    assert!(harness.is_on_screen("At bar 5: 7/8 \u{00B7} \u{2192}132.5 BPM"), "an arrow says it is arrived at");
    let words = harness.labeled("At bar 5: 7/8 \u{00B7} \u{2192}132.5 BPM");
    harness.right_click(words);
    harness.click_label("Remove the change of time signature");
    assert!(signatures(&harness).is_empty());
    let words = harness.labeled("At bar 5: \u{2192}132.5 BPM");
    harness.right_click(words);
    harness.click_label("Remove the change of tempo");
    assert!(tempos(&harness).is_empty());
    assert!(!harness.is_on_screen("At bar 5: \u{2192}132.5 BPM"));

    // What is typed and is neither is said, and nothing changes.
    harness.right_click(harness.ruler(8.0, false));
    harness.click_label("Set the tempo at 3\u{2026}");
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("fast");
    harness.key(Modifiers::NONE, Key::Enter);
    assert!(tempos(&harness).is_empty());
    let said = harness.app.state().toasts.texts().join("\n");
    assert!(said.contains("is not a tempo"), "{said}");
}

#[test]
fn the_top_bar_shows_and_sets_the_tempo_and_the_time_signature_the_song_has_where_it_is() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    // A song that begins in 4/4 at 120, is in 6/8 at 140 from bar 3 (bars
    // of three beats: 8, 11, 14, 17, 20, 23), and slows from bar 5 to 100
    // by bar 7.
    harness
        .session
        .lock()
        .edit(|project| {
            project.set_time_signature_at(8.0, 6, 8)?;
            project.set_tempo_at(8.0, 140.0, false)?;
            project.set_tempo_at(14.0, 140.0, false)?;
            project.set_tempo_at(20.0, 100.0, true)
        })
        .unwrap();
    harness.idle(2);
    let go_to = |harness: &mut Harness, beat: f64| {
        harness.click(harness.ruler(beat, false));
        harness.wait_for("the playhead to go there", |session| session.transport().position_beats == beat);
        harness.idle(3);
    };
    // The item of the time signature's menu that is ticked.
    let ticked = |harness: &mut Harness| {
        harness.click_label("Time signature");
        harness.idle(3);
        let ticked: Vec<&str> =
            ["4 / 4", "3 / 4", "6 / 8", "7 / 8"].into_iter().filter(|item| harness.is_on(item)).collect();
        // A second click on the field shuts its menu, and chooses nothing.
        let field = harness.labeled("Time signature");
        harness.click(field);
        harness.idle(3);
        assert!(!harness.is_on_screen("12 / 8"), "the menu is shut again");
        ticked
    };
    let type_tempo = |harness: &mut Harness, tempo: &str| {
        harness.double_click_label("Tempo");
        harness.idle(2);
        harness.key(Modifiers::COMMAND, Key::A);
        harness.type_text(tempo);
        harness.key(Modifiers::NONE, Key::Enter);
        harness.idle(2);
    };
    let before = harness.undo_depth();

    // At the start: what the song begins with.
    assert_eq!(harness.shown("Tempo"), 120.0);
    assert_eq!(ticked(&mut harness), ["4 / 4"]);

    // In bar 4 the fields show the tempo and the time signature that took
    // over at bar 3, and what is set there is that change: the song still
    // begins as it did.
    go_to(&mut harness, 11.0);
    assert_eq!(harness.shown("Tempo"), 140.0);
    assert_eq!(ticked(&mut harness), ["6 / 8"]);
    type_tempo(&mut harness, "150");
    assert_eq!(tempos(&harness), [(8.0, 150.0, false), (14.0, 140.0, false), (20.0, 100.0, true)]);
    assert_eq!((harness.project().tempo, harness.shown("Tempo")), (120.0, 150.0));
    let field = harness.labeled("Tempo");
    harness.drag(field, field - vec2(0.0, 40.0));
    assert_eq!(tempos(&harness)[0], (8.0, 160.0, false), "a quarter of a BPM per pixel, upward");
    assert_eq!(harness.undo_depth(), before + 2, "typing is a step, and a drag is one");
    harness.click_label("Time signature");
    harness.click_label("3 / 4");
    harness.idle(2);
    assert_eq!(signatures(&harness), [(8.0, 3, 4)]);
    let begins = harness.project().time_signature;
    assert_eq!((begins.numerator, begins.denominator), (4, 4));
    assert_eq!(ticked(&mut harness), ["3 / 4"]);
    assert_eq!(harness.undo_depth(), before + 3);

    // While the song slows, the field shows the tempo it has got to, and
    // what is typed is the tempo it is on its way to.
    go_to(&mut harness, 17.0);
    let gliding = harness.shown("Tempo");
    assert_eq!(gliding, harness.project().tempo_map().tempo_at(17.0));
    assert!(gliding > 100.0 && gliding < 140.0, "{gliding}");
    type_tempo(&mut harness, "90");
    assert_eq!(tempos(&harness), [(8.0, 160.0, false), (14.0, 140.0, false), (20.0, 90.0, true)]);

    // Once it has arrived, that tempo is the one the song has, and the one
    // that is set: not the tempo of some change before the glide.
    go_to(&mut harness, 23.0);
    assert_eq!(harness.shown("Tempo"), 90.0);
    type_tempo(&mut harness, "96");
    assert_eq!(tempos(&harness), [(8.0, 160.0, false), (14.0, 140.0, false), (20.0, 96.0, true)]);
    assert_eq!(harness.shown("Tempo"), 96.0);

    // Back at the start, it is the tempo the song begins at again.
    go_to(&mut harness, 0.0);
    type_tempo(&mut harness, "110");
    assert_eq!((harness.project().tempo, tempos(&harness).len()), (110.0, 3));
    assert_eq!(harness.undo_depth(), before + 6);
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

#[test]
fn markers_and_changes_that_stand_close_are_named_by_the_pointer_and_not_written_over_one_another() {
    let mut harness = Harness::new(UiOptions::default(), |session| {
        session
            .edit(|project| {
                project.add_track("A");
                // A name that has no room before the next marker's flag, a beat on.
                project.add_marker(8.0, "The long way round to the chorus")?;
                project.add_marker(9.0, "Next")?;
                // A bar of two among bars of four, as a recording's tempo brings into a song.
                project.set_time_signature_at(16.0, 2, 4)?;
                project.set_time_signature_at(18.0, 4, 4)
            })
            .unwrap();
    });
    // Its flag says the name the strip has no room for, and the next marker is its own.
    harness.move_to(harness.marker_strip(8.0) + vec2(3.0, 0.0));
    harness.idle(2);
    assert!(harness.hint().starts_with("The long way round to the chorus at bar 3 "), "{}", harness.hint());
    harness.move_to(harness.marker_strip(9.0) + vec2(3.0, 0.0));
    harness.idle(2);
    assert!(harness.hint().starts_with("Next at bar 3.2 "), "{}", harness.hint());
    // A click on the first flag goes there: what catches the pointer is what is drawn.
    harness.click(harness.marker_strip(8.0) + vec2(3.0, 0.0));
    assert_eq!(harness.app.state().arrangement.cursor, 8.0);
    // A change is said in full to the pointer, and to a screen reader, however little of it is written.
    harness.move_to(harness.ruler(16.0, false) + vec2(2.0, 0.0));
    harness.idle(2);
    assert!(harness.hint().starts_with("At bar 5: 2/4 "), "{}", harness.hint());
    assert!(harness.is_on_screen("At bar 5: 2/4") && harness.is_on_screen("At bar 6: 4/4"));
}
