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
fn a_lane_is_resized_by_its_lower_edge() {
    let (mut harness, _) = with_volume_lane(&[]);
    let edge = |harness: &Harness, y: f32| pos2(harness.work_left() + 100.0, harness.content_y(y));
    // The lane lies under the first track: from 60 down the track list.
    let top = arrangement::TRACK_HEIGHT;
    harness.drag(edge(&harness, top + LANE - 2.0), edge(&harness, top + 100.0));
    assert_eq!(harness.app.state().arrangement.lanes[0].height, 100.0);
    assert_eq!(harness.undo_depth(), 1, "a view setting, not an edit");
    // It cannot be dragged shut.
    harness.drag(edge(&harness, top + 98.0), edge(&harness, top - 40.0));
    assert_eq!(harness.app.state().arrangement.lanes[0].height, ArrangementState::MIN_LANE_HEIGHT);
    harness.settle();
    harness.double_click(edge(&harness, top + ArrangementState::MIN_LANE_HEIGHT - 2.0));
    assert_eq!(harness.app.state().arrangement.lanes[0].height, LANE, "a double-click restores the usual height");
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

    // Looking at it changes nothing, and the hint says what a drag would do.
    let before = harness.undo_depth();
    harness.move_to(harness.volume_bar(0, 0.5));
    harness.idle(1);
    assert!(harness.hint().starts_with("Volume") && harness.hint().contains("drag to take hold"), "{}", harness.hint());
    assert_eq!(harness.project().tracks[0].volume_db, 0.0);
    assert_eq!(harness.undo_depth(), before);

    // Its menu clears the envelope, and the control is simply the user's.
    harness.right_click(harness.volume_bar(0, 0.5));
    harness.click_label("Clear automation");
    assert!(envelope(&harness, &target).is_empty());
    harness.drag(harness.volume_bar(0, 0.8), harness.volume_bar(0, 0.4));
    assert!(harness.project().tracks[0].volume_db < -10.0);
    assert!(harness.project().automation.is_empty(), "with no envelope there is nothing to switch off");
}

/// Whether the envelope of `target` is switched on.
fn lane_on(harness: &Harness, target: &AutomationTarget) -> bool {
    harness.project().automation.iter().find(|lane| lane.target == *target).expect("an envelope").enabled
}

#[test]
fn dragging_an_automated_control_takes_hold_of_it_as_one_undo_step() {
    // A flat envelope at -12 dB, with its lane closed again.
    let points = [(0.0, -12.0), (16.0, -12.0)];
    let (mut harness, target) = with_volume_lane(&points);
    harness.click_label("Automation of One");
    let volume = |harness: &Harness| harness.project().tracks[0].volume_db;
    let at_minus_12 = units::db_to_fader(-12.0) as f64;
    let off_mark = "Automation of Vol is off";
    assert!((harness.shown("Vol") - at_minus_12).abs() < 0.01);
    assert!(!harness.is_on_screen(off_mark));
    let before = harness.undo_depth();

    harness.drag(harness.volume_bar(0, 0.84), harness.volume_bar(0, 0.4));
    assert!((-23.0..-21.0).contains(&volume(&harness)), "the bar set the volume: {}", volume(&harness));
    assert!(!lane_on(&harness, &target), "and switched the envelope off");
    assert_eq!(envelope(&harness, &target).len(), 2, "without touching it");
    assert_eq!(harness.undo_depth(), before + 1, "the whole takeover is one undo step");
    // The control shows the value it was given, not the envelope's.
    assert!((harness.shown("Vol") - 0.4).abs() < 0.01, "{}", harness.shown("Vol"));
    assert!(harness.app.state().arrangement.lanes.is_empty(), "no lane is unfolded behind the user's back");

    // It says so: the mark in its corner is a button, and the hint names it.
    assert!(harness.is_on_screen(off_mark));
    harness.move_to(harness.volume_bar(0, 0.5));
    harness.idle(1);
    assert!(harness.hint().contains("its automation is off"), "{}", harness.hint());
    harness.move_to(harness.labeled(off_mark));
    harness.idle(1);
    assert!(harness.hint().starts_with("Automation is off"), "{}", harness.hint());
    // Held, the control is an ordinary one: another drag is another step.
    harness.drag(harness.volume_bar(0, 0.4), harness.volume_bar(0, 0.6));
    assert!((-12.0..-9.0).contains(&volume(&harness)), "{}", volume(&harness));
    assert_eq!(harness.undo_depth(), before + 2);
    harness.key(Modifiers::COMMAND, Key::Z);

    // One click on the mark, and the envelope sets the volume again. What
    // was set by hand is kept under it.
    harness.settle();
    harness.click_label(off_mark);
    assert!(lane_on(&harness, &target));
    assert!((-23.0..-21.0).contains(&volume(&harness)));
    harness.idle(1);
    assert!((harness.shown("Vol") - at_minus_12).abs() < 0.01, "{}", harness.shown("Vol"));
    assert!(!harness.is_on_screen(off_mark));
    assert_eq!(harness.undo_depth(), before + 2);

    // Undo: off again, then everything as it was before the drag.
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(!lane_on(&harness, &target));
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(lane_on(&harness, &target), "undoing the takeover gives the control back to its envelope");
    assert_eq!(volume(&harness), 0.0, "and the volume back as it was");
    assert_eq!(harness.undo_depth(), before);
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert!(!lane_on(&harness, &target) && volume(&harness) < -20.0, "redo takes hold again, in one step");

    // A press without a drag takes hold too: the pointer is the value.
    harness.settle();
    harness.click_label(off_mark);
    let before = harness.undo_depth();
    harness.settle();
    harness.click(harness.volume_bar(0, 0.7));
    assert!(!lane_on(&harness, &target));
    assert!((-8.5..-5.5).contains(&volume(&harness)), "{}", volume(&harness));
    assert_eq!(harness.undo_depth(), before + 1);
}

#[test]
fn a_lane_shows_that_it_is_off_and_one_click_switches_it_back_on() {
    let (mut harness, target) = with_volume_lane(&[(0.0, -12.0), (16.0, -12.0)]);
    let switch = "Envelope of Volume on";
    assert!(harness.is_on(switch));
    let before = harness.undo_depth();

    // Taken hold of from the track's own bar, the lane shows it.
    harness.drag(harness.volume_bar(0, 0.84), harness.volume_bar(0, 0.4));
    assert!(!lane_on(&harness, &target));
    harness.idle(1);
    assert!(!harness.is_on(switch), "the lane's switch is off");
    harness.move_to(harness.labeled(switch));
    harness.idle(1);
    assert!(harness.hint().starts_with("Off: the value stays where it is set by hand"), "{}", harness.hint());

    // An envelope that is off can still be edited, and stays off.
    harness.click(at_volume(&harness, 8.0, -30.0));
    assert_eq!(envelope(&harness, &target).len(), 3);
    assert!(!lane_on(&harness, &target));

    // The switch is the way back, in one click and one undo step.
    harness.settle();
    harness.click_label(switch);
    assert!(lane_on(&harness, &target));
    harness.idle(1);
    assert!(harness.is_on(switch));
    assert_eq!(harness.undo_depth(), before + 3);
    // And the way there: switching off by hand leaves the value alone.
    harness.settle();
    harness.click_label(switch);
    assert!(!lane_on(&harness, &target));
    let held = harness.project().tracks[0].volume_db;
    assert!((-23.0..-21.0).contains(&held), "what the drag set earlier: {held}");

    // The menus of the lane and of the control say the same.
    harness.right_click(at_volume(&harness, 20.0, -50.0));
    harness.click_label("Switch the envelope back on");
    assert!(lane_on(&harness, &target));
    harness.right_click(harness.volume_bar(0, 0.5));
    harness.click_label("Switch automation off");
    assert!(!lane_on(&harness, &target));
    harness.right_click(harness.volume_bar(0, 0.5));
    harness.click_label("Switch automation back on");
    assert!(lane_on(&harness, &target));
    assert_eq!(harness.project().tracks[0].volume_db, held, "none of which moved the control");
}

#[test]
fn pan_and_the_master_volume_are_taken_hold_of_the_same_way() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    let pan = AutomationTarget::TrackPan { track: first };
    set_envelope(&mut harness, &pan, &[(0.0, -1.0)]);
    set_envelope(&mut harness, &AutomationTarget::MasterVolume, &[(0.0, -30.0)]);
    harness.idle(1);
    assert!(harness.shown("Pan") < 0.01, "hard left, as the envelope says");
    let before = harness.undo_depth();

    // The pan cell: three quarters across is half right.
    let pan_left = arrangement::HEADER_WIDTH - arrangement::HEADER_RIGHT - arrangement::HEADER_PAN_WIDTH;
    let cell = harness.header(
        0,
        pan_left + arrangement::HEADER_PAN_WIDTH * 0.75,
        arrangement::HEADER_CONTROLS_TOP + BAR - 4.0,
    );
    harness.click(cell);
    let set = harness.project().track(first).unwrap().pan;
    assert!((set - 0.5).abs() < 0.05, "{set}");
    assert!(!lane_on(&harness, &pan));
    assert_eq!(harness.undo_depth(), before + 1);
    harness.settle();
    harness.click_label("Automation of Pan is off");
    assert!(lane_on(&harness, &pan));

    // The master's volume, in its row under the tracks.
    let before = harness.undo_depth();
    let right = harness.work_left() + arrangement::HEADER_WIDTH - arrangement::HEADER_RIGHT;
    let y = harness.master_top() + arrangement::MASTER_HEIGHT / 2.0 + 4.0;
    let at = |fraction: f32| pos2(right - arrangement::MASTER_VOLUME_WIDTH * (1.0 - fraction), y);
    harness.drag(at(0.8), at(0.5));
    let volume = harness.project().master.volume_db;
    assert!((-17.5..-15.0).contains(&volume), "halfway up the fader scale: {volume}");
    assert!(!lane_on(&harness, &AutomationTarget::MasterVolume));
    assert_eq!(harness.undo_depth(), before + 1);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(lane_on(&harness, &AutomationTarget::MasterVolume));
    assert_eq!(harness.project().master.volume_db, 0.0);
}

#[test]
fn chips_follow_their_envelopes_and_a_click_takes_hold() {
    let mut device = None;
    let mut harness = Harness::new(UiOptions { detail_tab: DetailTab::Devices, ..Default::default() }, |session| {
        let track = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
        device = Some(session.add_device(ChainRef::Track(track), "builtin:delay", None, None).unwrap());
    });
    let device = device.unwrap();
    let ping_pong = AutomationTarget::DeviceParam { device, param: "ping_pong".into() };
    let sync = AutomationTarget::DeviceParam { device, param: "sync".into() };
    let stored = |harness: &Harness, id: &str| harness.project().device(device).unwrap().params.get(id).copied();
    assert!(harness.is_on("Ping Pong") && harness.is_on("Sync"), "both on, as the delay comes");

    // Envelopes switch them off; the chips show what is heard, while the
    // values the device was given by hand are what they were.
    set_envelope(&mut harness, &ping_pong, &[(0.0, 0.0)]);
    set_envelope(&mut harness, &sync, &[(0.0, 0.0)]);
    harness.session.lock().set_device_params(device, &[("sync".into(), 0.0)]).unwrap();
    harness.idle(1);
    assert!(!harness.is_on("Ping Pong") && !harness.is_on("Sync"));
    assert_eq!(stored(&harness, "ping_pong"), None, "nothing was set by hand");
    let before = harness.undo_depth();

    // A click is the user's: the chip goes on and the envelope off. The
    // value by hand was "on" all along, so switching the envelope off is
    // all it takes.
    harness.click_label("Ping Pong");
    assert!(!lane_on(&harness, &ping_pong));
    assert_eq!(stored(&harness, "ping_pong"), None);
    assert_eq!(harness.undo_depth(), before + 1);
    harness.idle(1);
    assert!(harness.is_on("Ping Pong"));
    // Where the value by hand has to change as well, both are one step.
    harness.click_label("Sync");
    assert!(!lane_on(&harness, &sync));
    assert_eq!(stored(&harness, "sync"), Some(1.0));
    assert_eq!(harness.undo_depth(), before + 2, "one undo step for the envelope and the value");
    harness.idle(1);
    assert!(harness.is_on("Sync"));

    // The mark on the chip switches the envelope back on.
    harness.settle();
    harness.click_label("Automation of Ping Pong is off");
    assert!(lane_on(&harness, &ping_pong));
    harness.idle(1);
    assert!(!harness.is_on("Ping Pong"), "and the chip follows it again");

    harness.key(Modifiers::COMMAND, Key::Z);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(lane_on(&harness, &sync), "undo gives the envelope back");
    assert_eq!(stored(&harness, "sync"), Some(0.0), "with the value by hand, together");
    harness.idle(1);
    assert!(!harness.is_on("Sync"));
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

    // The bar in the rack shows the envelope, and dragging it takes hold of
    // the parameter: the value is set and the envelope switched off, as
    // one undo step.
    assert!((harness.shown("Size") - 0.5).abs() < 0.02, "{}", harness.shown("Size"));
    let before = harness.undo_depth();
    harness.drag(harness.first_panel_bar(0, 150.0, 0.2), harness.first_panel_bar(0, 150.0, 0.9));
    let stored = |harness: &Harness| harness.project().device(device).unwrap().params.get("size").copied();
    assert!((stored(&harness).unwrap() - 0.9).abs() < 0.01, "{:?}", stored(&harness));
    assert!(!lane_on(&harness, &target));
    assert_eq!(harness.undo_depth(), before + 1);
    assert!((harness.shown("Size") - 0.9).abs() < 0.01);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(stored(&harness), None);
    assert!(lane_on(&harness, &target));
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
