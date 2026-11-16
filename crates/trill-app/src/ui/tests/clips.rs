//! Clip editing in the arrangement: trimming, splitting, selecting,
//! moving, copying, the clipboard, time selection and fades.

use super::*;
use crate::ui::state::Tool;
use crate::ui::timeline::{GridChoice, Snap};
use trill_core::project::AudioContent;

/// Where every note of track `index` sounds.
fn sounding(harness: &Harness, index: usize) -> Vec<f64> {
    let project = harness.project();
    let mut starts: Vec<f64> =
        project.tracks[index].clips.iter().flat_map(|clip| clip.timeline_notes()).map(|note| note.start).collect();
    starts.sort_by(f64::total_cmp);
    starts
}

/// A point on the name strip of a clip on row `row`.
fn title(harness: &Harness, beat: f64, row: usize) -> Pos2 {
    pos2(harness.lane(beat, row).x, harness.row_top(row) + 8.0)
}

#[test]
fn a_clips_left_edge_trims_without_moving_what_it_plays() {
    let (mut harness, _, [a, ..]) = with_three_tracks(UiOptions::default());
    let before = harness.undo_depth();
    assert_eq!(sounding(&harness, 0), [9.0, 17.0]);

    // In from bar 3 to beat 10: the note at 9 is hidden, not deleted.
    harness.drag(harness.lane(8.0, 0) + vec2(2.0, 0.0), harness.lane(10.0, 0));
    let clip = harness.project().clip(a).unwrap().clone();
    assert_eq!((clip.start, clip.length), (10.0, 2.0));
    assert_eq!(clip.notes().len(), 1, "the note is still in the clip");
    assert_eq!(sounding(&harness, 0), [17.0]);
    assert_eq!(harness.undo_depth(), before + 1, "the whole drag is one undo step");

    // Back out past where it began: the note returns where it was, and the
    // new room in front of it is empty.
    harness.drag(harness.lane(10.0, 0) + vec2(2.0, 0.0), harness.lane(6.0, 0));
    let clip = harness.project().clip(a).unwrap().clone();
    assert_eq!((clip.start, clip.length), (6.0, 6.0));
    assert_eq!(sounding(&harness, 0), [9.0, 17.0]);

    // The edge cannot be dragged past the clip's end.
    harness.drag(harness.lane(6.0, 0) + vec2(2.0, 0.0), harness.lane(30.0, 0));
    let clip = harness.project().clip(a).unwrap().clone();
    assert_eq!((clip.start, clip.length), (11.0, 1.0), "one grid step is left");
    assert_eq!(harness.undo_depth(), before + 3);
}

#[test]
fn a_looped_clip_keeps_looping_as_it_is_extended() {
    let (mut harness, _, [a, ..]) = with_three_tracks(UiOptions::default());
    harness.session.lock().edit(|project| project.clip_mut(a)?.set_loop_length(Some(2.0))).unwrap();
    harness.idle(1);
    assert_eq!(sounding(&harness, 0), [9.0, 11.0, 17.0]);

    harness.drag(harness.lane(12.0, 0) - vec2(3.0, 0.0), harness.lane(15.0, 0));
    assert_eq!(harness.project().clip(a).unwrap().length, 7.0);
    assert_eq!(sounding(&harness, 0), [9.0, 11.0, 13.0, 17.0], "the phrase repeats into the new length");
}

#[test]
fn a_click_in_a_clip_places_the_cursor_and_ctrl_e_splits_there() {
    let (mut harness, _, [a, ..]) = with_three_tracks(UiOptions::default());
    let before = harness.undo_depth();
    harness.click(harness.lane(10.0, 0));
    assert_eq!(harness.selected_clips(), [a]);
    assert_eq!(harness.app.state().arrangement.cursor, 10.0);
    harness.wait_for("the playhead to follow the cursor", |session| session.transport().position_beats == 10.0);

    harness.key(Modifiers::COMMAND, Key::E);
    assert_eq!(harness.spans(0), [(8.0, 2.0), (10.0, 2.0), (16.0, 4.0)]);
    assert_eq!(sounding(&harness, 0), [9.0, 17.0], "both halves play what the clip played");
    assert_eq!(harness.undo_depth(), before + 1);

    // The name strip stands for the whole clip: the cursor goes to its start.
    harness.click(title(&harness, 17.5, 0));
    assert_eq!(harness.app.state().arrangement.cursor, 16.0);
    // With the cursor on a clip's edge there is nothing to split.
    harness.key(Modifiers::COMMAND, Key::E);
    assert_eq!(harness.spans(0).len(), 3);
    assert_eq!(harness.app.state().toasts.len(), 1, "and the app says so");

    // Nothing selected: the clip of the selected track under the cursor.
    harness.click(harness.ruler(18.0, false));
    harness.click(harness.header(0, 120.0, 14.0));
    assert!(harness.selected_clips().is_empty() || harness.app.state().selected_track.is_some());
    harness.key(Modifiers::NONE, Key::Escape);
    harness.key(Modifiers::COMMAND, Key::E);
    assert_eq!(harness.spans(0), [(8.0, 2.0), (10.0, 2.0), (16.0, 2.0), (18.0, 2.0)]);
}

#[test]
fn the_blade_splits_under_the_pointer() {
    let (mut harness, _, [_, b, _]) = with_three_tracks(UiOptions::default());
    harness.key(Modifiers::NONE, Key::C);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Blade);
    harness.move_to(harness.lane(18.0, 0));
    harness.idle(1);
    assert!(harness.hint().starts_with("Click to split"), "{}", harness.hint());

    let before = harness.undo_depth();
    harness.click(harness.lane(18.2, 0));
    assert_eq!(harness.spans(0), [(8.0, 4.0), (16.0, 2.0), (18.0, 2.0)], "on the grid line nearest the pointer");
    assert_eq!(harness.project().clip(b).unwrap().length, 2.0);
    assert_eq!(harness.undo_depth(), before + 1);
    // With the blade out, a drag does not move clips.
    harness.drag(harness.lane(9.0, 0), harness.lane(13.0, 1));
    assert_eq!(harness.spans(0)[0], (8.0, 4.0));

    harness.key(Modifiers::NONE, Key::Escape);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Pointer);
    harness.key(Modifiers::NONE, Key::C);
    harness.key(Modifiers::NONE, Key::C);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Pointer, "the key puts it away again");
}

#[test]
fn shift_and_ctrl_clicks_build_a_selection() {
    let (mut harness, [_, two, _], [a, b, c]) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.click_holding(Modifiers::SHIFT, harness.lane(10.0, 1));
    assert_eq!(harness.selected_clips(), [a, c]);
    assert_eq!(harness.app.state().selected_clip, Some(c), "the piano roll shows the one clicked last");
    assert_eq!(harness.app.state().selected_track, Some(two));

    harness.click_holding(Modifiers::COMMAND, harness.lane(18.0, 0));
    assert_eq!(harness.selected_clips(), [a, c, b]);
    harness.click_holding(Modifiers::COMMAND, harness.lane(10.0, 1));
    assert_eq!(harness.selected_clips(), [a, b], "a second click takes a clip out again");
    assert_eq!(harness.app.state().selected_clip, Some(b));

    // Shift-clicking empty space keeps the selection; a plain click drops it.
    harness.click_holding(Modifiers::SHIFT, harness.lane(26.0, 2));
    assert_eq!(harness.selected_clips(), [a, b]);
    harness.settle();
    harness.click(harness.lane(26.0, 2));
    assert!(harness.selected_clips().is_empty());
    assert_eq!(harness.undo_depth(), 1, "selecting edits nothing");
}

#[test]
fn a_rubber_band_selects_clips_and_time() {
    let (mut harness, [one, two, _], [a, b, c]) = with_three_tracks(UiOptions::default());
    // From empty space before the clips, across two tracks.
    harness.drag(harness.lane(5.2, 0), harness.lane(13.1, 1));
    assert_eq!(harness.selected_clips(), [a, c]);
    let view = &harness.app.state().arrangement;
    let selection = view.time_selection.clone().expect("the band is also a time selection");
    assert_eq!((selection.start, selection.end), (5.0, 13.0), "snapped to the grid");
    assert_eq!(selection.tracks, [one, two]);
    assert_eq!(view.cursor, 5.0);

    // With Shift held a second band adds to the selection.
    harness.drag_holding(Modifiers::SHIFT, harness.lane(14.5, 0), harness.lane(22.0, 0));
    assert_eq!(harness.selected_clips(), [a, c, b]);
    // A click elsewhere drops both selections.
    harness.click(harness.lane(26.0, 2));
    assert!(harness.selected_clips().is_empty());
    assert_eq!(harness.app.state().arrangement.time_selection, None);
    assert_eq!(harness.undo_depth(), 1);
}

#[test]
fn a_selection_moves_together_keeping_its_lanes() {
    let (mut harness, [one, two, three], [a, b, c]) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.click_holding(Modifiers::SHIFT, harness.lane(10.0, 1));
    let before = harness.undo_depth();

    // Grab A a beat in; drop it a bar later, one track down.
    harness.drag(harness.lane(9.0, 0), harness.lane(13.0, 1));
    let project = harness.project();
    assert_eq!((project.clip_track(a).unwrap(), project.clip(a).unwrap().start), (two, 12.0));
    assert_eq!((project.clip_track(c).unwrap(), project.clip(c).unwrap().start), (three, 12.0), "C went along");
    assert_eq!(project.clip_track(b).unwrap(), one, "B was not selected");
    assert_eq!(harness.selected_clips(), [a, c]);
    assert_eq!(harness.undo_depth(), before + 1, "one undo step for the whole move");

    // Further down there is no track for C, so the pair stays in its lanes.
    harness.drag(harness.lane(13.0, 1), harness.lane(17.0, 2));
    let project = harness.project();
    assert_eq!((project.clip_track(a).unwrap(), project.clip(a).unwrap().start), (two, 16.0));
    assert_eq!(project.clip_track(c).unwrap(), three);
    // And together they stop at the start of the song.
    harness.drag(harness.lane(17.0, 1), harness.lane(-40.0, 0));
    let project = harness.project();
    assert_eq!((project.clip_track(a).unwrap(), project.clip(a).unwrap().start), (one, 0.0));
    assert_eq!((project.clip_track(c).unwrap(), project.clip(c).unwrap().start), (two, 0.0));

    for _ in 0..3 {
        harness.key(Modifiers::COMMAND, Key::Z);
    }
    let project = harness.project();
    assert_eq!((project.clip_track(a).unwrap(), project.clip(a).unwrap().start), (one, 8.0));
    assert_eq!((project.clip_track(c).unwrap(), project.clip(c).unwrap().start), (two, 8.0));
}

#[test]
fn ctrl_drag_copies_and_alt_suspends_snapping() {
    let (mut harness, [one, two, _], [a, ..]) = with_three_tracks(UiOptions::default());
    let before = harness.undo_depth();
    harness.drag_holding(Modifiers::COMMAND, harness.lane(9.0, 0), harness.lane(25.0, 1));
    let project = harness.project();
    assert_eq!(project.clip(a).unwrap().start, 8.0, "the original stays");
    assert_eq!(project.clip_track(a).unwrap(), one);
    let copy = harness.selected_clips()[0];
    assert_ne!(copy, a, "the copy is what was dragged, and is selected");
    assert_eq!((project.clip_track(copy).unwrap(), project.clip(copy).unwrap().start), (two, 24.0));
    assert_eq!(project.clip(copy).unwrap().notes().len(), 1);
    assert_eq!(harness.undo_depth(), before + 1, "copying and moving are one step");
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().clip(copy).is_err(), "undo takes the copy away altogether");

    // With Alt held the clip lands where it is dropped, off the grid.
    harness.drag_holding(Modifiers::ALT, harness.lane(9.0, 0), harness.lane(10.4, 0));
    let start = harness.project().clip(a).unwrap().start;
    assert!((start - 9.4).abs() < 0.05, "{start}");
}

#[test]
fn keys_nudge_mute_and_delete_the_selection() {
    let (mut harness, _, [a, _, c]) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.click_holding(Modifiers::SHIFT, harness.lane(10.0, 1));
    let before = harness.undo_depth();
    let starts = |harness: &Harness| {
        let project = harness.project();
        (project.clip(a).unwrap().start, project.clip(c).unwrap().start)
    };

    harness.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!(starts(&harness), (9.0, 9.0), "one grid step, both clips");
    harness.key(Modifiers::NONE, Key::ArrowLeft);
    harness.key(Modifiers::NONE, Key::ArrowLeft);
    assert_eq!(starts(&harness), (7.0, 7.0));
    assert_eq!(harness.undo_depth(), before + 3);

    let muted = |harness: &Harness| {
        let project = harness.project();
        (project.clip(a).unwrap().muted, project.clip(c).unwrap().muted)
    };
    harness.key(Modifiers::NONE, Key::Num0);
    assert_eq!(muted(&harness), (true, true));
    harness.key(Modifiers::NONE, Key::Num0);
    assert_eq!(muted(&harness), (false, false));

    harness.key(Modifiers::NONE, Key::Delete);
    let project = harness.project();
    assert!(project.clip(a).is_err() && project.clip(c).is_err());
    assert!(harness.selected_clips().is_empty());
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().clip(a).is_ok() && harness.project().clip(c).is_ok(), "both come back together");

    // Without a selection the arrows are not the arrangement's to take.
    harness.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!(starts(&harness), (7.0, 7.0));
}

#[test]
fn ctrl_d_duplicates_the_selection_after_itself() {
    let (mut harness, _, [a, b, _]) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.click_holding(Modifiers::SHIFT, harness.lane(18.0, 0));
    assert_eq!(harness.selected_clips(), [a, b]);
    let before = harness.undo_depth();

    // A and B span beats 8 to 20; the copies follow from 20.
    harness.key(Modifiers::COMMAND, Key::D);
    assert_eq!(harness.spans(0), [(8.0, 4.0), (16.0, 4.0), (20.0, 4.0), (28.0, 4.0)]);
    let copies = harness.selected_clips();
    assert!(copies.len() == 2 && !copies.contains(&a) && !copies.contains(&b), "the copies are selected");
    assert_eq!(harness.undo_depth(), before + 1);
    // Again: the copies are duplicated in turn, laying out a row.
    harness.key(Modifiers::COMMAND, Key::D);
    assert_eq!(harness.spans(0).len(), 6);
    assert_eq!(harness.spans(0)[4], (32.0, 4.0));
}

#[test]
fn copy_cut_and_paste_go_through_the_cursor_and_the_selected_track() {
    let (mut harness, [_, two, three], [a, _, c]) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.click_holding(Modifiers::SHIFT, harness.lane(10.0, 1));
    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("2 clips from Trill"), "a note for the system clipboard");
    assert_eq!(harness.undo_depth(), 1, "copying edits nothing");

    // Paste at bar 7 on the second track: A lands there, C a track below.
    harness.click(harness.lane(24.0, 1));
    let before = harness.undo_depth();
    harness.paste();
    let project = harness.project();
    let pasted = harness.selected_clips();
    assert_eq!(pasted.len(), 2);
    assert_eq!((project.clip_track(pasted[0]).unwrap(), project.clip(pasted[0]).unwrap().start), (two, 24.0));
    assert_eq!((project.clip_track(pasted[1]).unwrap(), project.clip(pasted[1]).unwrap().start), (three, 24.0));
    assert_eq!(project.clip(pasted[0]).unwrap().name, "A");
    assert_eq!(harness.undo_depth(), before + 1, "one step for everything pasted");
    assert_eq!(harness.app.state().arrangement.cursor, 28.0, "the cursor moves past what was pasted");

    // Pasting again follows on. The key press works as well as the event.
    harness.key(Modifiers::COMMAND, Key::V);
    assert_eq!(harness.project().track(two).unwrap().clips.len(), 3);
    assert_eq!(harness.project().track(two).unwrap().clips[2].start, 28.0);

    // On the last track there is no lane below for the second clip.
    harness.click(harness.lane(4.0, 2));
    harness.paste();
    assert_eq!(harness.selected_clips().len(), 1);
    assert_eq!(harness.app.state().toasts.len(), 1, "the app says what it could not place");

    // Cut removes and keeps: the originals can be pasted back.
    harness.click(harness.lane(10.0, 0));
    harness.cut();
    assert!(harness.project().clip(a).is_err());
    assert!(harness.project().clip(c).is_ok(), "only what was selected");
    harness.click(harness.lane(0.3, 0));
    harness.paste();
    assert_eq!(harness.spans(0)[0], (0.0, 4.0));
}

#[test]
fn ctrl_a_selects_every_clip_and_escape_none() {
    let (mut harness, _, clips) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(26.0, 2));
    harness.key(Modifiers::COMMAND, Key::A);
    let mut selected = harness.selected_clips();
    selected.sort_by_key(|clip| clip.0);
    assert_eq!(selected, clips);
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.selected_clips().is_empty());
}

#[test]
fn selected_time_can_be_looped_deleted_and_duplicated() {
    let (mut harness, _, [a, b, c]) = with_three_tracks(UiOptions::default());
    // Drag along the foot of the ruler: bars 3 and 4 on every track.
    harness.drag(harness.ruler(8.2, false), harness.ruler(11.9, false));
    let selection = harness.app.state().arrangement.time_selection.clone().unwrap();
    assert_eq!((selection.start, selection.end), (8.0, 12.0));
    assert!(selection.tracks.is_empty(), "drawn on the ruler, it spans every track");
    assert_eq!(harness.undo_depth(), 1, "selecting time edits nothing");

    harness.key(Modifiers::COMMAND, Key::L);
    let region = harness.project().loop_region;
    assert_eq!((region.enabled, region.start, region.end), (true, 8.0, 12.0));

    let before = harness.undo_depth();
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::D);
    let project = harness.project();
    assert_eq!(harness.spans(0), [(8.0, 4.0), (12.0, 4.0), (20.0, 4.0)], "A repeated, B moved along");
    assert_eq!(project.clip(b).unwrap().start, 20.0);
    assert_eq!(harness.spans(1), [(8.0, 4.0), (12.0, 4.0)]);
    assert_eq!(harness.undo_depth(), before + 1);
    let selection = harness.app.state().arrangement.time_selection.clone().unwrap();
    assert_eq!((selection.start, selection.end), (12.0, 16.0), "the copy is selected, to repeat again");

    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Delete);
    assert_eq!(harness.spans(0), [(8.0, 4.0), (16.0, 4.0)], "the copy is cut out and B closes up");
    assert_eq!(harness.project().clip(a).unwrap().start, 8.0);
    assert_eq!(harness.project().clip(c).unwrap().start, 8.0);
    assert_eq!(harness.app.state().arrangement.time_selection, None);
    assert_eq!(harness.undo_depth(), before + 2);

    // With no time selected the commands explain themselves instead.
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Delete);
    assert_eq!(harness.spans(0).len(), 2);
    assert_eq!(harness.app.state().toasts.len(), 1);
}

/// One track holding an audio clip over beats 4 to 12.
fn with_audio_clip() -> (Harness, ClipId) {
    let mut clip = None;
    let harness = Harness::new(UiOptions::default(), |session| {
        let added = session.edit(|project| {
            let track = project.add_track("Audio");
            project.add_audio_clip(track, "Take", 4.0, 8.0, AudioContent::new("missing-take.wav"))
        });
        clip = Some(added.unwrap());
    });
    (harness, clip.unwrap())
}

#[test]
fn fades_are_dragged_in_from_an_audio_clips_top_corners() {
    let (mut harness, clip) = with_audio_clip();
    let fades = |harness: &Harness| {
        let project = harness.project();
        let audio = project.clip(clip).unwrap().audio().unwrap().clone();
        (audio.fade_in, audio.fade_out)
    };
    assert_eq!(fades(&harness), (0.0, 0.0));
    let before = harness.undo_depth();
    // The handles sit just under the name strip, at the clip's corners.
    let band = |harness: &Harness, beat: f64| pos2(harness.lane(beat, 0).x, harness.row_top(0) + 19.0);

    harness.move_to(band(&harness, 4.0) + vec2(2.0, 0.0));
    harness.idle(1);
    assert_eq!(harness.hint(), "Drag to set the fade");
    harness.drag(band(&harness, 4.0) + vec2(2.0, 0.0), band(&harness, 6.0));
    let (fade_in, fade_out) = fades(&harness);
    assert!((fade_in - 2.0).abs() < 0.05 && fade_out == 0.0, "{fade_in} {fade_out}");
    assert_eq!(harness.undo_depth(), before + 1, "one undo step per fade");

    harness.drag(band(&harness, 12.0) - vec2(2.0, 0.0), band(&harness, 9.0));
    let (fade_in, fade_out) = fades(&harness);
    assert!((fade_in - 2.0).abs() < 0.05 && (fade_out - 3.0).abs() < 0.05, "{fade_in} {fade_out}");

    // A fade cannot run into the other one.
    harness.drag(band(&harness, 6.0), band(&harness, 30.0));
    let (fade_in, fade_out) = fades(&harness);
    assert!((fade_in + fade_out - 8.0).abs() < 1e-9, "{fade_in} + {fade_out} fill the clip");
    // Below the band the edge still trims.
    harness.drag(harness.lane(4.0, 0) + vec2(2.0, 0.0), harness.lane(5.0, 0));
    assert_eq!(harness.project().clip(clip).unwrap().start, 5.0);
}

#[test]
fn a_clip_is_renamed_on_its_name_strip() {
    let (mut harness, _, [a, ..]) = with_three_tracks(UiOptions::default());
    let before = harness.undo_depth();
    harness.double_click(title(&harness, 9.5, 0));
    harness.idle(2);
    assert!(harness.app.state().renaming_clip_inline.is_some());
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("Verse");
    assert_eq!(harness.project().clip(a).unwrap().name, "A", "nothing is edited while typing");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(harness.project().clip(a).unwrap().name, "Verse");
    assert_eq!(harness.undo_depth(), before + 1, "a rename is one undo step");
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);

    // Ctrl+R does the same for the selected clip; Escape abandons it, and
    // the keys typed meanwhile are not shortcuts.
    harness.click(harness.lane(10.0, 0));
    harness.key(Modifiers::COMMAND, Key::R);
    harness.idle(2);
    harness.type_text("0 c");
    harness.key(Modifiers::NONE, Key::Escape);
    let clip = harness.project().clip(a).unwrap().clone();
    assert_eq!((clip.name.as_str(), clip.muted), ("Verse", false));
    assert_eq!(harness.app.state().arrangement.tool, Tool::Pointer);
    assert_eq!(harness.selected_clips(), [a], "the Escape that ends typing does not also deselect");
}

#[test]
fn the_context_menus_act_on_clips_and_on_empty_lanes() {
    let (mut harness, _, [a, ..]) = with_three_tracks(UiOptions::default());
    harness.right_click(harness.lane(10.0, 0));
    assert_eq!(harness.selected_clips(), [a], "a right-click selects what it is on");
    harness.click_label("Mute");
    assert!(harness.project().clip(a).unwrap().muted);

    harness.right_click(harness.lane(10.0, 0));
    harness.click_label("Split here");
    assert_eq!(harness.spans(0)[..2], [(8.0, 2.0), (10.0, 2.0)]);

    harness.right_click(harness.lane(9.0, 0));
    harness.click_label("Duplicate");
    assert_eq!(harness.spans(0)[..3], [(8.0, 2.0), (10.0, 2.0), (10.0, 2.0)]);
    harness.key(Modifiers::COMMAND, Key::Z);

    // On empty lane space the menu works at the place it was opened.
    harness.right_click(harness.lane(24.3, 1));
    assert_eq!(harness.app.state().arrangement.cursor, 24.0);
    harness.click_label("Add marker here");
    assert_eq!(harness.project().markers[0].beat, 24.0);
    harness.right_click(harness.lane(26.0, 1));
    harness.click_label("Add MIDI clip here");
    assert_eq!(harness.spans(1), [(8.0, 4.0), (24.0, 4.0)], "the bar under the pointer");
}

#[test]
fn the_grid_menu_and_the_snap_chip_decide_where_edits_land() {
    let (mut harness, _, [a, ..]) = with_three_tracks(UiOptions::default());
    let start = |harness: &Harness| harness.project().clip(a).unwrap().start;
    assert!(harness.app.state().arrangement.snap);

    // The grid's dropdown reads "Auto" until another grid is chosen.
    harness.click_label("Auto");
    harness.click_label("1/16");
    assert_eq!(harness.app.state().arrangement.grid, GridChoice::Fixed(Snap::Sixteenth));
    harness.drag(harness.lane(9.0, 0), harness.lane(10.3, 0));
    assert_eq!(start(&harness), 9.25, "a sixteenth grid");

    harness.click_label("Snap to grid");
    assert!(!harness.app.state().arrangement.snap);
    harness.drag(harness.lane(10.0, 0), harness.lane(10.4, 0));
    assert!((start(&harness) - 9.65).abs() < 0.05, "no snapping at all: {}", start(&harness));
    harness.click_label("Snap to grid");
    assert!(harness.app.state().arrangement.snap);

    harness.click_label("Follow playhead");
    assert!(!harness.app.state().arrangement.follow);
    harness.key(Modifiers::NONE, Key::F);
    assert!(harness.app.state().arrangement.follow, "F toggles it too");
}

#[test]
fn z_zooms_to_the_selection_and_shift_z_to_the_song() {
    let (mut harness, _, _) = with_three_tracks(UiOptions::default());
    let whole = harness.app.state().arrangement.time;
    harness.click(harness.lane(10.0, 0));
    harness.key(Modifiers::NONE, Key::Z);
    harness.idle(1);
    let gliding = harness.app.state().arrangement.time;
    assert!(gliding.pixels_per_beat > whole.pixels_per_beat, "on its way");
    harness.idle(30);
    let zoomed = harness.app.state().arrangement.time;
    let (first, last) = zoomed.visible(1004.0);
    assert!(first <= 8.0 && last >= 12.0 && last - first < 5.0, "the clip fills the view: {first}..{last}");
    assert!(harness.app.state().arrangement.glide.is_none(), "and the glide has arrived");

    harness.key(Modifiers::SHIFT, Key::Z);
    harness.idle(30);
    let back = harness.app.state().arrangement.time;
    assert!((back.pixels_per_beat - whole.pixels_per_beat).abs() < 1e-6 && back.origin == 0.0);
}

#[test]
fn ctrl_alt_scroll_zooms_the_tracks_height() {
    let (mut harness, ..) = with_three_tracks(UiOptions::default());
    assert_eq!(harness.app.state().arrangement.track_height, arrangement::TRACK_HEIGHT);
    let time = harness.app.state().arrangement.time;
    harness.wheel(Modifiers::COMMAND | Modifiers::ALT, harness.lane(10.0, 1), 3.0);
    let view = &harness.app.state().arrangement;
    assert!(view.track_height > arrangement::TRACK_HEIGHT, "{}", view.track_height);
    assert_eq!(view.time, time, "the timeline's zoom is left alone");
    // Over the headers Ctrl alone does it.
    let taller = view.track_height;
    harness.wheel(Modifiers::COMMAND, harness.header(0, 100.0, 20.0), -3.0);
    assert!(harness.app.state().arrangement.track_height < taller);
}

#[test]
fn an_edit_from_elsewhere_in_the_middle_of_a_drag_does_no_harm() {
    let (mut harness, [_, two, _], [a, _, c]) = with_three_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.click_holding(Modifiers::SHIFT, harness.lane(10.0, 1));

    // Two clips are on their way a bar later when another front end
    // removes one of them.
    harness.drag_start(Modifiers::NONE, harness.lane(9.0, 0), harness.lane(13.0, 0));
    assert_eq!(harness.project().clip(c).unwrap().start, 12.0);
    harness.session.lock().edit(|project| project.remove_clip(c).map(drop)).unwrap();
    harness.move_to(harness.lane(17.0, 1));
    harness.move_to(harness.lane(17.0, 1));
    harness.drag_end(harness.lane(17.0, 1));

    let project = harness.project();
    assert!(project.clip(c).is_err(), "what was removed stays removed");
    assert_eq!((project.clip_track(a).unwrap(), project.clip(a).unwrap().start), (two, 16.0), "the other carried on");
    assert_eq!(harness.selected_clips(), [a], "the selection lost only the clip that is gone");
    assert_eq!(harness.app.state().toasts.len(), 0, "nothing went wrong, so nothing is reported");
    assert!(harness.app.state().arrangement.drag.is_none());

    // The clip being trimmed disappears under the pointer.
    harness.drag_start(Modifiers::NONE, harness.lane(20.0, 1) - vec2(3.0, 0.0), harness.lane(22.0, 1));
    assert_eq!(harness.project().clip(a).unwrap().length, 6.0);
    harness.session.lock().edit(|project| project.remove_clip(a).map(drop)).unwrap();
    harness.move_to(harness.lane(24.0, 1));
    harness.drag_end(harness.lane(24.0, 1));
    assert!(harness.project().clip(a).is_err());
    assert!(harness.selected_clips().is_empty());
    assert_eq!(harness.app.state().toasts.len(), 0);

    // The window still works: a new clip can be made and moved.
    harness.double_click(harness.lane(4.5, 2));
    harness.drag(harness.lane(5.0, 2), harness.lane(9.0, 2));
    assert_eq!(harness.spans(2), [(8.0, 4.0)]);
}
