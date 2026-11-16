//! Tracks as a whole: reordering, resizing, the menu, groups and returns.

use super::*;
use crate::ui::colors;
use trill_core::TrackKind;

fn names(harness: &Harness) -> Vec<String> {
    harness.project().tracks.iter().map(|track| track.name.clone()).collect()
}

/// A point on the name of the header on row `row`.
fn name_of(harness: &Harness, row: usize) -> Pos2 {
    harness.header(row, 80.0, 14.0)
}

#[test]
fn dragging_a_header_reorders_tracks_as_one_undo_step() {
    let (mut harness, [one, ..], _) = with_three_tracks(UiOptions::default());
    let before = harness.undo_depth();
    // Onto the lower half of the track below: the two change places.
    harness.drag(name_of(&harness, 0), harness.header(1, 80.0, 50.0));
    assert_eq!(names(&harness), ["Two", "One", "Three"]);
    assert_eq!(harness.undo_depth(), before + 1);
    assert_eq!(harness.app.state().selected_track, Some(one), "the dragged track is selected");
    assert_eq!(harness.project().tracks[1].clips.len(), 2, "its clips go with it");

    // Below the last track: to the end.
    harness.drag(name_of(&harness, 0), harness.header(2, 80.0, 58.0));
    assert_eq!(names(&harness), ["One", "Three", "Two"]);
    // Dropping a track on its own place edits nothing.
    harness.drag(name_of(&harness, 1), harness.header(1, 80.0, 45.0));
    harness.drag(name_of(&harness, 1), harness.header(0, 80.0, 50.0));
    assert_eq!(names(&harness), ["One", "Three", "Two"]);
    assert_eq!(harness.undo_depth(), before + 2);

    harness.key(Modifiers::COMMAND, Key::Z);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(names(&harness), ["One", "Two", "Three"]);
}

#[test]
fn a_tracks_lower_edge_sets_its_height() {
    let (mut harness, [one, two, _], [_, _, c]) = with_three_tracks(UiOptions::default());
    let before = harness.undo_depth();
    harness.drag(harness.header(0, 100.0, 58.0), harness.header(0, 100.0, 100.0));
    assert_eq!(harness.app.state().arrangement.heights.get(&one), Some(&100.0));
    assert_eq!(harness.undo_depth(), before, "a view setting, not an edit");
    // The rows below moved down: the clip on track two is 40 px lower.
    harness.click(harness.lane(10.0, 1) + vec2(0.0, 40.0));
    assert_eq!(harness.selected_clips(), [c]);

    // Dragged short, a header keeps its name row and its chips.
    harness.drag(pos2(harness.header(1, 100.0, 0.0).x, harness.content_y(158.0)), pos2(0.0, harness.content_y(110.0)));
    assert_eq!(harness.app.state().arrangement.heights.get(&two), Some(&26.0), "no shorter than the name row");
    harness.click_label("Mute Two");
    assert!(harness.project().tracks[1].mute);

    // A double-click on the edge returns a track to the common height.
    harness.double_click(harness.header(0, 100.0, 98.0));
    assert_eq!(harness.app.state().arrangement.heights.get(&one), None);
}

#[test]
fn the_track_menu_duplicates_recolors_and_groups() {
    let (mut harness, [one, ..], _) = with_three_tracks(UiOptions::default());
    harness.right_click(name_of(&harness, 0));
    harness.click_label("Duplicate");
    let project = harness.project();
    assert_eq!(names(&harness), ["One", "One copy", "Two", "Three"]);
    assert_eq!(project.tracks[1].clips.len(), 2, "with its clips");
    assert_eq!(harness.app.state().selected_track, Some(project.tracks[1].id));

    harness.right_click(name_of(&harness, 0));
    harness.click_label("Color 4");
    assert_eq!(harness.project().track(one).unwrap().color, colors::new_track_color(3));

    let before = harness.undo_depth();
    harness.right_click(name_of(&harness, 0));
    harness.click_label("Group this track");
    let project = harness.project();
    assert_eq!(names(&harness), ["Group 1", "One", "One copy", "Two", "Three"]);
    assert_eq!(project.tracks[0].kind, TrackKind::Group);
    assert_eq!(project.track(one).unwrap().group, Some(project.tracks[0].id));
    assert_eq!(harness.undo_depth(), before + 1, "the group and its first member are one step");

    harness.right_click(name_of(&harness, 1));
    harness.click_label("Take out of its group");
    assert_eq!(harness.project().track(one).unwrap().group, None);
    harness.right_click(name_of(&harness, 3));
    harness.click_label("Move into Group 1");
    let project = harness.project();
    assert_eq!(project.tracks.iter().find(|track| track.name == "Two").unwrap().group, Some(project.tracks[0].id));
    assert_eq!(names(&harness), ["Group 1", "Two", "One", "One copy", "Three"], "right under its group");
}

#[test]
fn tracks_are_dragged_into_and_out_of_a_group_and_groups_fold() {
    let (mut harness, [one, two, three], _) = with_three_tracks(UiOptions::default());
    harness.click_label("Edit");
    harness.click_label("Add group track");
    let group = harness.project().tracks[3].id;
    assert_eq!(harness.project().tracks[3].kind, TrackKind::Group);
    assert_eq!(harness.app.state().selected_track, Some(group));

    // Onto the group's header: in as a member.
    let before = harness.undo_depth();
    harness.drag(name_of(&harness, 2), harness.header(3, 80.0, 36.0));
    let project = harness.project();
    assert_eq!(project.track(three).unwrap().group, Some(group));
    assert_eq!(names(&harness), ["One", "Two", "Group 1", "Three"], "members follow their group");
    assert_eq!(harness.undo_depth(), before + 1);
    // Onto a member: in beside it.
    harness.drag(name_of(&harness, 1), harness.header(3, 80.0, 50.0));
    assert_eq!(harness.project().track(two).unwrap().group, Some(group));
    assert_eq!(names(&harness), ["One", "Group 1", "Three", "Two"]);

    // The members hold clips; the group's own lane does not.
    harness.double_click(harness.lane(4.5, 1));
    assert!(harness.project().track(group).unwrap().clips.is_empty());
    assert_eq!(harness.app.state().toasts.len(), 1, "the app says why");

    // Folding hides the members, and a drag across the fold skips them.
    harness.click_label("Fold Group 1");
    assert!(harness.app.state().arrangement.folded.contains(&group));
    harness.idle(1);
    assert!(!harness.is_on_screen("Mute Three") && !harness.is_on_screen("Mute Two"));
    assert!(harness.is_on_screen("Mute One"));
    harness.click_label("Unfold Group 1");
    harness.idle(1);
    assert!(harness.is_on_screen("Mute Three"));

    // Out again by dropping it above everything; the group moves whole.
    harness.drag(name_of(&harness, 3), harness.header(0, 80.0, 5.0));
    assert_eq!(harness.project().track(two).unwrap().group, None);
    assert_eq!(names(&harness), ["Two", "One", "Group 1", "Three"]);
    harness.drag(name_of(&harness, 2), harness.header(0, 80.0, 5.0));
    assert_eq!(names(&harness), ["Group 1", "Three", "Two", "One"]);

    // Ctrl+G wraps the selected track in a group of its own.
    harness.click(name_of(&harness, 3));
    assert_eq!(harness.app.state().selected_track, Some(one));
    harness.key(Modifiers::COMMAND, Key::G);
    let project = harness.project();
    assert_eq!(names(&harness), ["Group 1", "Three", "Two", "Group 2", "One"]);
    assert_eq!(project.track(one).unwrap().group, Some(project.tracks[3].id));
}

#[test]
fn a_return_track_takes_sends_and_holds_no_clips() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();
    harness.right_click(harness.labeled("New track"));
    harness.click_label("Add return track");
    let project = harness.project();
    let returns = &project.tracks[2];
    assert_eq!((returns.name.as_str(), returns.kind), ("Return A", TrackKind::Return));
    assert_eq!(returns.devices[0].uri, "builtin:reverb", "a reverb, with nothing dry in it");
    assert_eq!(returns.devices[0].params.get("mix"), Some(&1.0));
    assert_eq!(harness.undo_depth(), before + 1, "track, device and setting are one step");
    let target = returns.id;

    harness.double_click(harness.lane(4.5, 2));
    assert!(harness.project().tracks[2].clips.is_empty());

    // A header tall enough shows a send bar for the return under its
    // volume; the bar sets how much goes there.
    harness.drag(harness.header(0, 100.0, 58.0), harness.header(0, 100.0, 84.0));
    harness.idle(1);
    let send = harness.labeled("Return A");
    let before = harness.undo_depth();
    harness.drag(send - vec2(40.0, 0.0), send);
    let sends = harness.project().track(first).unwrap().sends.clone();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].to, target);
    assert!((-25.0..-10.0).contains(&sends[0].level_db), "around the middle of the scale: {}", sends[0].level_db);
    assert_eq!(harness.undo_depth(), before + 1);
    // A double-click switches the send off again.
    harness.double_click(send);
    assert!(harness.project().track(first).unwrap().sends.is_empty());
}
