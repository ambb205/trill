//! Automation lanes: unfolding them, editing envelopes with the pointer,
//! and what an automated control shows.

use super::*;
use crate::ui::state::{ArrangementState, FocusArea, LaneOwner, Tool};
use crate::ui::units;
use trill_core::{AutomationPoint, AutomationTarget, Segment};

/// Space a lane keeps clear above and below its values.
const PAD: f32 = 6.0;
const LANE: f32 = ArrangementState::DEFAULT_LANE_HEIGHT;

fn envelope(harness: &Harness, target: &AutomationTarget) -> Vec<AutomationPoint> {
    let project = harness.project();
    project.automation.iter().find(|lane| lane.target == *target).map(|lane| lane.points.clone()).unwrap_or_default()
}

fn set_envelope(harness: &mut Harness, target: &AutomationTarget, points: &[(f64, f64)]) {
    let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
    harness.session.lock().edit(|project| project.set_automation(target.clone(), points)).unwrap();
    harness.idle(1);
}

/// A point in the lane that starts `top` down the track list: at `beat`,
/// `position` of the way up its scale.
fn in_lane(harness: &Harness, top: f32, beat: f64, position: f32) -> Pos2 {
    harness.lanes_at(beat, top + LANE - PAD - position * (LANE - 2.0 * PAD))
}

/// Where a volume in dB is in the lane under the first track.
fn at_volume(harness: &Harness, beat: f64, db: f32) -> Pos2 {
    in_lane(harness, arrangement::TRACK_HEIGHT, beat, units::db_to_fader(db))
}

/// Three tracks with the first one's volume lane open.
fn with_volume_lane(points: &[(f64, f64)]) -> (Harness, AutomationTarget) {
    let (mut harness, [one, ..], _) = with_three_tracks(UiOptions::default());
    let target = AutomationTarget::TrackVolume { track: one };
    if !points.is_empty() {
        set_envelope(&mut harness, &target, points);
    }
    harness.click_label("Automation of One");
    harness.idle(2);
    (harness, target)
}

#[test]
fn a_lane_unfolds_under_its_track_and_a_click_adds_a_point() {
    let (mut harness, target) = with_volume_lane(&[]);
    let AutomationTarget::TrackVolume { track } = target else { unreachable!() };
    let lanes = &harness.app.state().arrangement.lanes;
    assert_eq!(lanes.len(), 1);
    assert_eq!((lanes[0].owner, &lanes[0].target), (LaneOwner::Track(track), &target), "volume, with no envelope yet");
    assert_eq!(harness.undo_depth(), 1, "opening a lane edits nothing");

    harness.move_to(at_volume(&harness, 4.3, -12.0));
    harness.idle(1);
    assert!(
        harness.hint().contains("dB at 2.1.") && harness.hint().contains("click adds a point"),
        "{}",
        harness.hint()
    );

    harness.click(at_volume(&harness, 4.3, -12.0));
    let points = envelope(&harness, &target);
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].beat, 4.0, "on the grid");
    assert!((points[0].value + 12.0).abs() < 1.0, "where the pointer was on the scale: {}", points[0].value);
    assert_eq!(harness.undo_depth(), 2, "one undo step per point");
    assert_eq!(harness.app.state().focus, FocusArea::Automation);
    assert_eq!(harness.app.state().arrangement.points.as_ref().unwrap().points, points, "the new point is selected");

    harness.click(at_volume(&harness, 8.0, 0.0));
    assert_eq!(envelope(&harness, &target).len(), 2);
    // A click on the line adds a point on it rather than off it.
    harness.click(at_volume(&harness, 6.0, -6.0));
    let points = envelope(&harness, &target);
    assert_eq!(points.len(), 3);
    let (before, middle, after) = (points[0].value, points[1].value, points[2].value);
    assert_eq!(middle, (before + after) / 2.0, "exactly on the line, half way along it");

    // The rows below moved down by the lane: track two's clip is there.
    harness.click(harness.lanes_at(10.0, arrangement::TRACK_HEIGHT + LANE + 36.0));
    assert_eq!(harness.selected_clips().len(), 1);
    assert_eq!(harness.app.state().focus, FocusArea::Arrangement);

    // The chip closes the lanes again; the envelope stays.
    harness.click_label("Automation of One");
    assert!(harness.app.state().arrangement.lanes.is_empty());
    assert_eq!(envelope(&harness, &target).len(), 3);
}

#[test]
fn a_point_drags_on_the_grid_between_its_neighbors() {
    let (mut harness, target) = with_volume_lane(&[(4.0, -12.0), (8.0, 0.0), (12.0, -12.0)]);
    let before = harness.undo_depth();

    harness.drag(at_volume(&harness, 8.0, 0.0), at_volume(&harness, 9.4, -6.0));
    let moved = envelope(&harness, &target)[1];
    assert_eq!(moved.beat, 9.0, "on the grid");
    assert!((moved.value + 6.0).abs() < 0.6, "{}", moved.value);
    assert_eq!(harness.undo_depth(), before + 1, "the whole drag is one undo step");
    assert_eq!(harness.app.state().arrangement.points.as_ref().unwrap().points, [moved], "it stays selected");

    // It cannot pass the point after it.
    harness.drag(at_volume(&harness, 9.0, moved.value as f32), at_volume(&harness, 20.0, moved.value as f32));
    assert_eq!(envelope(&harness, &target)[1].beat, 12.0);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(envelope(&harness, &target)[1], moved);

    // Alt lets it land between grid lines.
    harness.drag_holding(Modifiers::ALT, at_volume(&harness, 9.0, moved.value as f32), at_volume(&harness, 10.4, -6.0));
    let free = envelope(&harness, &target)[1];
    assert!((free.beat - 10.4).abs() < 0.05, "{}", free.beat);

    // Shift moves the value a tenth as far.
    let start = at_volume(&harness, free.beat, free.value as f32);
    harness.drag_holding(Modifiers::SHIFT, start, start - vec2(0.0, 20.0));
    let fine = envelope(&harness, &target)[1];
    let coarse = units::fader_to_db(units::db_to_fader(free.value as f32) + 20.0 / (LANE - 2.0 * PAD)) as f64;
    assert!(
        fine.value > free.value && fine.value - free.value < (coarse - free.value) * 0.2,
        "{} then {}",
        free.value,
        fine.value
    );

    // A double-click removes a point.
    harness.double_click(at_volume(&harness, 4.0, -12.0));
    assert_eq!(envelope(&harness, &target).len(), 2);
}

#[test]
fn a_line_moves_by_both_ends_bends_with_alt_and_becomes_a_step() {
    let (mut harness, target) = with_volume_lane(&[(4.0, -24.0), (12.0, -6.0)]);
    let before = harness.undo_depth();
    let on_line = |harness: &Harness| {
        let points = envelope(harness, &target);
        let middle = trill_core::project::automation_value(&points, 8.0);
        at_volume(harness, 8.0, middle as f32)
    };

    harness.move_to(on_line(&harness));
    harness.idle(1);
    assert!(harness.hint().contains("drag moves the line"), "{}", harness.hint());
    let grab = on_line(&harness);
    harness.drag(grab, grab - vec2(0.0, 8.0));
    let points = envelope(&harness, &target);
    assert!(points[0].value > -24.0 && points[1].value > -6.0, "both ends rose: {points:?}");
    assert_eq!((points[0].beat, points[1].beat), (4.0, 12.0), "and stayed where they were in time");
    assert_eq!(harness.undo_depth(), before + 1);

    let grab = on_line(&harness);
    harness.drag_holding(Modifiers::ALT, grab, grab - vec2(0.0, 12.0));
    let bent = envelope(&harness, &target);
    let Segment::Curve(amount) = bent[0].shape else { panic!("the line is bent: {:?}", bent[0].shape) };
    assert!(amount < 0.0, "a rising line bowed upward starts fast: {amount}");
    assert_eq!((bent[0].value, bent[1].value), (points[0].value, points[1].value), "bending moves no point");
    assert_eq!(harness.undo_depth(), before + 2);

    // The menu makes the line a step, or straight again.
    harness.right_click(at_volume(&harness, 4.0, bent[0].value as f32));
    harness.click_label("Step: hold, then jump");
    assert_eq!(envelope(&harness, &target)[0].shape, Segment::Hold);
    harness.right_click(at_volume(&harness, 4.0, bent[0].value as f32));
    harness.click_label("Straight line");
    assert_eq!(envelope(&harness, &target)[0].shape, Segment::Linear);
    harness.right_click(at_volume(&harness, 4.0, bent[0].value as f32));
    harness.click_label("Delete point");
    assert_eq!(envelope(&harness, &target).len(), 1);
    harness.right_click(at_volume(&harness, 20.0, -40.0));
    harness.click_label("Clear the envelope");
    assert!(envelope(&harness, &target).is_empty());
}

#[test]
fn a_band_selects_points_and_delete_removes_them() {
    let (mut harness, target) = with_volume_lane(&[(4.0, -12.0), (8.0, -12.0), (12.0, -12.0), (16.0, -12.0)]);
    let before = harness.undo_depth();
    // From empty space above the envelope, around the two middle points.
    harness.drag(at_volume(&harness, 6.5, 3.0), at_volume(&harness, 13.5, -30.0));
    let selected = harness.app.state().arrangement.points.as_ref().unwrap().points.clone();
    assert_eq!(selected.iter().map(|point| point.beat).collect::<Vec<_>>(), [8.0, 12.0]);
    assert_eq!(harness.undo_depth(), before, "selecting edits nothing");

    // The selected points move together.
    harness.drag(at_volume(&harness, 8.0, -12.0), at_volume(&harness, 9.0, -12.0));
    let beats: Vec<f64> = envelope(&harness, &target).iter().map(|point| point.beat).collect();
    assert_eq!(beats, [4.0, 9.0, 13.0, 16.0]);

    harness.key(Modifiers::NONE, Key::Delete);
    assert_eq!(envelope(&harness, &target).len(), 2);
    assert_eq!(harness.undo_depth(), before + 2, "one step for the move, one for the delete");
    assert_eq!(harness.project().tracks[0].clips.len(), 2, "Delete took points, not the track's clips");

    harness.key(Modifiers::COMMAND, Key::A);
    assert_eq!(harness.app.state().arrangement.points.as_ref().unwrap().points.len(), 2);
    harness.key(Modifiers::NONE, Key::Delete);
    assert!(envelope(&harness, &target).is_empty());
    assert_eq!(harness.app.state().arrangement.lanes.len(), 1, "the lane stays open, empty");
}

#[test]
fn the_pencil_draws_steps_on_the_grid() {
    let (mut harness, target) = with_volume_lane(&[]);
    harness.key(Modifiers::NONE, Key::B);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Draw);
    let before = harness.undo_depth();

    // A stroke rising across four beats.
    harness.drag(at_volume(&harness, 4.2, -40.0), at_volume(&harness, 7.8, -3.0));
    let points = envelope(&harness, &target);
    let steps: Vec<&AutomationPoint> = points.iter().filter(|point| point.shape == Segment::Hold).collect();
    assert_eq!(steps.iter().map(|point| point.beat).collect::<Vec<_>>(), [4.0, 5.0, 6.0, 7.0], "one per grid cell");
    assert!(steps.windows(2).all(|pair| pair[0].value < pair[1].value), "each as high as the pencil was: {points:?}");
    assert_eq!(harness.undo_depth(), before + 1, "the whole stroke is one undo step");

    // Drawing over part of it replaces what was there.
    harness.drag(at_volume(&harness, 5.2, -60.0), at_volume(&harness, 6.8, -60.0));
    let points = envelope(&harness, &target);
    let value = |beat: f64| trill_core::project::automation_value(&points, beat);
    assert!(value(5.5) < -50.0 && value(6.5) < -50.0, "{points:?}");
    assert_eq!(value(4.5), steps[0].value, "the cells beside the stroke are untouched");
    assert_eq!(value(7.5), steps[3].value);

    // A click is the shortest stroke: one cell.
    let before = envelope(&harness, &target).len();
    harness.settle();
    harness.click(at_volume(&harness, 12.4, -20.0));
    let points = envelope(&harness, &target);
    let step = |point: &AutomationPoint| point.beat == 12.0 && point.shape == Segment::Hold;
    assert!(points.iter().any(step), "a step at the cell under the click: {points:?}");
    assert!(points.len() > before);

    harness.key(Modifiers::NONE, Key::B);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Pointer);
}

#[test]
fn an_automated_control_shows_it_and_follows_its_envelope() {
    let (mut harness, target) = with_volume_lane(&[(0.0, -70.0), (16.0, 6.0)]);
    harness.click_label("Automation of One");
    assert!(harness.app.state().arrangement.lanes.is_empty());
    let shown = |harness: &Harness| harness.values.iter().find(|(label, _)| label == "Vol").unwrap().1;

    // The header's volume shows what the envelope says at the playhead.
    assert!(shown(&harness) < 0.01, "silent at the start: {}", shown(&harness));
    harness.session.lock().seek(16.0);
    harness.wait_for("the seek", |session| session.transport().position_beats == 16.0);
    harness.idle(2);
    assert!(shown(&harness) > 0.99, "full up at bar 5: {}", shown(&harness));
    harness.session.lock().seek(8.0);
    harness.wait_for("the seek", |session| session.transport().position_beats == 8.0);
    harness.idle(2);
    assert!((shown(&harness) - units::db_to_fader(-32.0) as f64).abs() < 0.01, "{}", shown(&harness));

    // Dragging it does not fight the envelope: it opens the lane instead.
    let before = harness.undo_depth();
    harness.drag(harness.volume_bar(0, 0.8), harness.volume_bar(0, 0.3));
    assert_eq!(harness.project().tracks[0].volume_db, 0.0);
    assert_eq!(harness.undo_depth(), before);
    assert_eq!(harness.app.state().arrangement.lanes[0].target, target);
    harness.move_to(harness.volume_bar(0, 0.5));
    harness.idle(1);
    assert!(harness.hint().starts_with("Automated"), "{}", harness.hint());

    // Its menu clears the envelope, and the control is the user's again.
    harness.right_click(harness.volume_bar(0, 0.5));
    harness.click_label("Clear automation");
    assert!(envelope(&harness, &target).is_empty());
    harness.drag(harness.volume_bar(0, 0.8), harness.volume_bar(0, 0.4));
    assert!(harness.project().tracks[0].volume_db < -10.0);
}

#[test]
fn show_automation_on_a_device_parameter_opens_its_lane() {
    let mut device = None;
    let mut harness = Harness::new(UiOptions { detail_tab: DetailTab::Devices, ..Default::default() }, |session| {
        let track = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
        device = Some(session.add_device(ChainRef::Track(track), "builtin:reverb", None, None).unwrap());
    });
    let device = device.unwrap();
    let target = AutomationTarget::DeviceParam { device, param: "size".into() };
    // "Size" is the first bar of the reverb's panel.
    let size = harness.first_panel_bar(0, 150.0, 0.5);

    harness.right_click(size);
    harness.click_label("Show automation");
    harness.idle(2);
    let lanes = &harness.app.state().arrangement.lanes;
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].target, target);

    // The lane is in the parameter's own units: half way up is half size.
    harness.click(in_lane(&harness, arrangement::TRACK_HEIGHT, 4.0, 0.5));
    let points = envelope(&harness, &target);
    assert_eq!(points.len(), 1);
    assert!((points[0].value - 0.5).abs() < 0.02, "{}", points[0].value);
    harness.move_to(in_lane(&harness, arrangement::TRACK_HEIGHT, 8.0, 0.25));
    harness.idle(1);
    assert!(harness.hint().starts_with("25 %"), "{}", harness.hint());

    // The bar in the rack now belongs to the envelope.
    harness.drag(harness.first_panel_bar(0, 150.0, 0.2), harness.first_panel_bar(0, 150.0, 0.9));
    assert_eq!(harness.project().device(device).unwrap().params.get("size"), None, "the bar did not set a value");
    harness.right_click(size);
    harness.click_label("Hide automation");
    assert!(harness.app.state().arrangement.lanes.is_empty());
    harness.right_click(size);
    harness.click_label("Clear automation");
    assert!(envelope(&harness, &target).is_empty());
}

#[test]
fn the_lane_chooser_lists_everything_the_track_can_automate() {
    let mut ids = None;
    let mut harness = Harness::new(UiOptions::default(), |session| {
        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        ids = Some((track, synth));
    });
    let (track, synth) = ids.unwrap();
    harness.click_label("Automation of Lead");
    harness.idle(2);

    // The chooser reads what the lane shows; its menu offers the rest.
    harness.click_label("Volume");
    harness.click_last_label("Pan");
    assert_eq!(harness.app.state().arrangement.lanes[0].target, AutomationTarget::TrackPan { track });

    // A synth has dozens of parameters, so the menu can be searched. (The
    // header's pan cell is called "Pan" too; the chooser is the lower one.)
    harness.settle();
    harness.click_last_label("Pan");
    harness.idle(3);
    assert!(harness.is_on_screen("Filter Cutoff") && harness.is_on_screen("Volume"));
    harness.type_text("cutoff");
    harness.idle(2);
    assert!(harness.is_on_screen("Filter Cutoff"));
    assert!(!harness.is_on_screen("Volume"), "narrowed to what matches");
    assert!(!harness.session.lock().transport().playing, "typing in the search is not a shortcut");
    harness.click_label("Filter Cutoff");
    let cutoff = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
    assert_eq!(harness.app.state().arrangement.lanes[0].target, cutoff);

    // A click in the lane writes in hertz, on the filter's own scale.
    harness.click(in_lane(&harness, arrangement::TRACK_HEIGHT, 4.0, 0.5));
    let points = envelope(&harness, &cutoff);
    assert!((300.0..3000.0).contains(&points[0].value), "the middle of a logarithmic range: {}", points[0].value);

    // "+" shows another lane, the cross hides one.
    harness.click_label("Add lane");
    harness.idle(1);
    assert_eq!(harness.app.state().arrangement.lanes.len(), 2);
    harness.click_label("Hide lane");
    assert_eq!(harness.app.state().arrangement.lanes.len(), 1);
    assert_eq!(envelope(&harness, &cutoff).len(), 1, "hiding a lane keeps its envelope");
}

#[test]
fn the_master_has_lanes_of_its_own_pinned_above_its_row() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click_label("Automation of the master");
    harness.idle(2);
    let lanes = &harness.app.state().arrangement.lanes;
    assert_eq!((lanes[0].owner, &lanes[0].target), (LaneOwner::Master, &AutomationTarget::MasterVolume));

    let top = harness.master_top() - LANE;
    let x = harness.lane(4.0, 0).x;
    harness.click(pos2(x, top + LANE - PAD - units::db_to_fader(-6.0) * (LANE - 2.0 * PAD)));
    let points = envelope(&harness, &AutomationTarget::MasterVolume);
    assert_eq!(points.len(), 1);
    assert!((points[0].value + 6.0).abs() < 0.6, "{}", points[0].value);
    assert_eq!(points[0].beat, 4.0);

    // The tracks still scroll and click as before above it.
    harness.click(harness.lane(10.0, 0));
    assert_eq!(harness.selected_clips().len(), 1);
}

#[test]
fn a_lane_survives_edits_from_elsewhere_during_a_drag() {
    let (mut harness, target) = with_volume_lane(&[(4.0, -12.0), (8.0, 0.0), (12.0, -12.0)]);
    // A point is being dragged when another front end rewrites the envelope
    // without it.
    harness.drag_start(Modifiers::NONE, at_volume(&harness, 8.0, 0.0), at_volume(&harness, 9.0, 0.0));
    assert_eq!(envelope(&harness, &target)[1].beat, 9.0);
    let rewritten = vec![AutomationPoint::new(2.0, -3.0), AutomationPoint::new(14.0, -3.0)];
    harness.session.lock().edit(|project| project.set_automation(target.clone(), rewritten.clone())).unwrap();
    harness.move_to(at_volume(&harness, 10.0, -6.0));
    harness.move_to(at_volume(&harness, 10.0, -6.0));
    harness.drag_end(at_volume(&harness, 10.0, -6.0));

    let points = envelope(&harness, &target);
    assert!(points.contains(&rewritten[0]) && points.contains(&rewritten[1]), "the other edit is kept: {points:?}");
    assert_eq!(points.len(), 3, "and the dragged point is put down among it");
    assert_eq!(harness.app.state().toasts.len(), 0);

    // The track itself goes away with a lane open and a point selected.
    let track = harness.project().tracks[0].id;
    harness.session.lock().edit(|project| project.remove_track(track).map(drop)).unwrap();
    harness.idle(2);
    assert!(harness.app.state().arrangement.lanes.is_empty());
    assert!(harness.app.state().arrangement.points.is_none());
    harness.click(harness.lane(10.0, 0));
    assert_eq!(harness.selected_clips().len(), 1, "what was the second track is now the first");
}
