//! The lane under the piano roll's notes when it shows a MIDI controller:
//! choosing what it shows, editing a clip's curve with the pointer, and
//! what it says about the clip and the instrument.

use super::*;
use crate::ui::state::{FocusArea, LaneShown, Tool};
use trill_core::project::automation_value;
use trill_core::{AutomationPoint, ControllerKind, Segment};

/// Space the lane keeps clear above and below its values.
const PAD: f32 = 6.0;

const EXPRESSION: ControllerKind = ControllerKind::EXPRESSION;

/// A point in the lane: at `beat` of the clip, `position` of the way up.
fn in_lane(harness: &Harness, beat: f64, position: f32) -> Pos2 {
    harness.velocity_lane(beat, PAD + position * (piano_roll::VELOCITY_LANE - 2.0 * PAD))
}

/// Where step `value` (0 to 127) of a controller is in the lane at `beat`.
fn at(harness: &Harness, beat: f64, value: u8) -> Pos2 {
    in_lane(harness, beat, f32::from(value) / 127.0)
}

fn curve(harness: &Harness, clip: ClipId, kind: ControllerKind) -> Vec<AutomationPoint> {
    let project = harness.project();
    project.clip(clip).unwrap().controller(kind).map(|curve| curve.points.clone()).unwrap_or_default()
}

/// A curve as beats and steps of 127, for reading in a failed assertion.
fn steps(points: &[AutomationPoint]) -> Vec<(f64, i32)> {
    points.iter().map(|point| (point.beat, (point.value * 127.0).round() as i32)).collect()
}

fn set_curve(harness: &mut Harness, clip: ClipId, kind: ControllerKind, points: &[(f64, u8)]) {
    let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, f64::from(*value) / 127.0)).collect();
    harness.session.lock().edit(|project| project.clip_mut(clip)?.set_controller(kind, points)).unwrap();
    harness.idle(1);
}

fn selected(harness: &Harness) -> Vec<AutomationPoint> {
    harness.app.state().piano.lane_points.clone()
}

/// The window on two tracks, with the first one's clip in the piano roll
/// and its lane showing `kind`, for which the clip holds `points`.
fn with_lane(kind: ControllerKind, points: &[(f64, u8)]) -> (Harness, ClipId) {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    if !points.is_empty() {
        set_curve(&mut harness, clip, kind, points);
    }
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    harness.click_label(&format!("{} lane", kind.label()));
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(kind));
    (harness, clip)
}

#[test]
fn the_lane_shows_a_controller_and_a_click_adds_a_point() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Velocity, "velocity, until something else is chosen");
    assert!(harness.is_on("Velocity lane") && !harness.is_on("Expression lane"));
    let before = harness.undo_depth();

    harness.click_label("Expression lane");
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(EXPRESSION));
    assert!(harness.is_on("Expression lane") && !harness.is_on("Velocity lane"));
    assert!(harness.is_on_screen("Expression curve"));
    assert_eq!(harness.undo_depth(), before, "what the lane shows is a view setting, not an edit");

    harness.move_to(at(&harness, 2.3, 100));
    harness.idle(1);
    assert!(
        harness.hint().starts_with("100 at 1.3 ") && harness.hint().contains("click adds a point"),
        "{}",
        harness.hint()
    );

    harness.click(at(&harness, 2.3, 100));
    let points = curve(&harness, clip, EXPRESSION);
    assert_eq!(steps(&points), [(2.25, 100)], "on the grid, at the step under the pointer");
    assert_eq!(points[0].shape, Segment::Linear);
    assert_eq!(harness.undo_depth(), before + 1, "one undo step per point");
    assert_eq!(harness.app.state().focus, FocusArea::Controllers);
    assert_eq!(selected(&harness), points, "the new point is selected");

    harness.click(at(&harness, 3.5, 40));
    assert_eq!(steps(&curve(&harness, clip, EXPRESSION)), [(2.25, 100), (3.5, 40)]);
    // A click on the line adds a point on it rather than off it.
    let ends = curve(&harness, clip, EXPRESSION);
    let on_line = automation_value(&ends, 3.0);
    harness.click(in_lane(&harness, 3.0, on_line as f32));
    let points = curve(&harness, clip, EXPRESSION);
    assert_eq!(points.len(), 3);
    assert_eq!(points[1].value, on_line, "exactly on the line");
    assert_eq!(harness.undo_depth(), before + 3);

    // Undo takes the points back one at a time.
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(curve(&harness, clip, EXPRESSION), ends);

    // The notes are untouched by any of it, and velocity is a click away.
    assert_eq!(harness.project().clip(clip).unwrap().notes().len(), 1);
    harness.click_label("Velocity lane");
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Velocity);
    assert!(!harness.is_on_screen("Expression curve"));
    assert_eq!(curve(&harness, clip, EXPRESSION), ends, "the curve stays when the lane shows something else");
}

#[test]
fn a_point_drags_on_the_grid_between_its_neighbors_as_one_undo_step() {
    let (mut harness, clip) = with_lane(EXPRESSION, &[(1.0, 30), (2.0, 100), (3.0, 30)]);
    let before = harness.undo_depth();

    harness.move_to(at(&harness, 2.0, 100));
    harness.idle(1);
    assert!(harness.hint().starts_with("100 at 1.3 ") && harness.hint().contains("drag to move"), "{}", harness.hint());

    harness.drag(at(&harness, 2.0, 100), at(&harness, 2.6, 60));
    let moved = curve(&harness, clip, EXPRESSION)[1];
    assert_eq!(steps(&[moved]), [(2.5, 60)], "on the grid, at the step it was let go on");
    assert_eq!(harness.undo_depth(), before + 1, "the whole drag is one undo step");
    assert_eq!(selected(&harness), [moved], "it stays selected");

    // It cannot pass the point after it.
    harness.drag(at(&harness, 2.5, 60), at(&harness, 3.8, 60));
    assert_eq!(curve(&harness, clip, EXPRESSION)[1].beat, 3.0);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(curve(&harness, clip, EXPRESSION)[1], moved);

    // Alt lets it land between grid lines.
    harness.drag_holding(Modifiers::ALT, at(&harness, 2.5, 60), at(&harness, 2.6, 60));
    let free = curve(&harness, clip, EXPRESSION)[1];
    assert!((free.beat - 2.6).abs() < 0.02, "{}", free.beat);

    // Shift moves the value a tenth as far.
    let start = at(&harness, free.beat, 60);
    harness.drag_holding(Modifiers::SHIFT, start, start - vec2(0.0, 20.0));
    let fine = steps(&curve(&harness, clip, EXPRESSION))[1].1;
    assert!((63..=68).contains(&fine), "20 pixels are 58 steps, a tenth of that about 6: {fine}");

    // A double-click removes a point, as one more step.
    let depth = harness.undo_depth();
    harness.settle();
    harness.double_click(at(&harness, 1.0, 30));
    assert_eq!(curve(&harness, clip, EXPRESSION).len(), 2);
    assert_eq!(harness.undo_depth(), depth + 1);
}

#[test]
fn a_line_moves_by_both_ends_and_bends_with_alt() {
    let (mut harness, clip) = with_lane(EXPRESSION, &[(1.0, 20), (3.0, 100)]);
    let before = harness.undo_depth();
    let on_line = |harness: &Harness| {
        let middle = automation_value(&curve(harness, clip, EXPRESSION), 2.0);
        in_lane(harness, 2.0, middle as f32)
    };

    harness.move_to(on_line(&harness));
    harness.idle(1);
    assert!(harness.hint().contains("drag moves the line"), "{}", harness.hint());
    let grab = on_line(&harness);
    harness.drag(grab, grab - vec2(0.0, 8.0));
    let points = curve(&harness, clip, EXPRESSION);
    let rose = steps(&points);
    assert_eq!(points.len(), 2, "a drag moves the line, it adds no point: {points:?}");
    assert!(rose[0].1 > 35 && rose[1].1 - rose[0].1 == 80, "both ends rose by the same 8 pixels: {rose:?}");
    assert_eq!((points[0].beat, points[1].beat), (1.0, 3.0), "and stayed where they were in time");
    assert_eq!(harness.undo_depth(), before + 1);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(steps(&curve(&harness, clip, EXPRESSION)), [(1.0, 20), (3.0, 100)]);

    // Alt-drag bends the line: a crescendo that opens late.
    let grab = on_line(&harness);
    harness.drag_holding(Modifiers::ALT, grab, grab + vec2(0.0, 12.0));
    let points = curve(&harness, clip, EXPRESSION);
    let Segment::Curve(amount) = points[0].shape else { panic!("not bent: {points:?}") };
    assert!(amount != 0.0, "{amount}");
    let middle = automation_value(&points, 2.0) * 127.0;
    assert!(middle < 55.0, "the middle of the line sank below 60: {middle}");
    assert_eq!(steps(&points), [(1.0, 20), (3.0, 100)], "the ends stay");
    assert_eq!(harness.undo_depth(), before + 1, "the bend is one step, in place of the one undone");

    // The menu makes the line a step, or straight again.
    harness.right_click(in_lane(&harness, 2.0, (middle / 127.0) as f32));
    harness.click_label("Step: hold, then jump");
    assert_eq!(curve(&harness, clip, EXPRESSION)[0].shape, Segment::Hold);
    harness.right_click(at(&harness, 2.0, 20));
    harness.click_label("Straight line");
    assert_eq!(curve(&harness, clip, EXPRESSION)[0].shape, Segment::Linear);
}

#[test]
fn points_are_selected_by_a_box_and_removed_together() {
    let (mut harness, clip) = with_lane(EXPRESSION, &[(0.5, 20), (1.5, 100), (2.5, 90), (3.5, 20)]);
    let before = harness.undo_depth();

    // A drag over nothing draws a box; what is inside is selected.
    harness.drag(at(&harness, 1.2, 60), at(&harness, 2.8, 127));
    assert_eq!(steps(&selected(&harness)), [(1.5, 100), (2.5, 90)]);
    assert_eq!(harness.undo_depth(), before, "selecting edits nothing");
    assert_eq!(harness.app.state().focus, FocusArea::Controllers);

    // Dragging one of them moves both, keeping their distance.
    harness.drag(at(&harness, 1.5, 100), at(&harness, 1.75, 80));
    assert_eq!(steps(&curve(&harness, clip, EXPRESSION)), [(0.5, 20), (1.75, 80), (2.75, 70), (3.5, 20)]);
    assert_eq!(harness.undo_depth(), before + 1);

    // Delete removes the selection as one step.
    harness.key(Modifiers::NONE, Key::Delete);
    assert_eq!(steps(&curve(&harness, clip, EXPRESSION)), [(0.5, 20), (3.5, 20)]);
    assert_eq!(harness.undo_depth(), before + 2);
    assert!(selected(&harness).is_empty());
    assert_eq!(harness.project().clip(clip).unwrap().notes().len(), 1, "Delete in the lane leaves the notes alone");

    // Shift-click adds a point to the selection; Ctrl+A takes them all.
    harness.click(at(&harness, 0.5, 20));
    harness.click_holding(Modifiers::SHIFT, at(&harness, 3.5, 20));
    assert_eq!(selected(&harness).len(), 2);
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.key(Modifiers::COMMAND, Key::A);
    assert_eq!(selected(&harness).len(), 4, "every point of the curve, and no notes");
    assert!(harness.app.state().selected_notes.is_empty());

    // The menu clears the whole curve, as one step.
    let depth = harness.undo_depth();
    harness.right_click(at(&harness, 3.0, 5));
    harness.click_label("Clear the expression curve");
    assert!(harness.project().clip(clip).unwrap().controller(EXPRESSION).is_none(), "no curve is left behind");
    assert_eq!(harness.undo_depth(), depth + 1);
    assert!(harness.is_on_screen("Expression curve"), "the lane still shows expression, with nothing written");
}

#[test]
fn the_pencil_writes_steps_on_the_grid_in_one_undo_step() {
    let (mut harness, clip) = with_lane(EXPRESSION, &[]);
    let before = harness.undo_depth();
    harness.key(Modifiers::NONE, Key::B);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Draw);

    harness.move_to(at(&harness, 1.1, 80));
    harness.idle(1);
    assert!(harness.hint().contains("drag to draw steps"), "{}", harness.hint());

    // One stroke across four sixteenths, falling.
    harness.drag(at(&harness, 1.1, 100), at(&harness, 1.9, 40));
    let points = curve(&harness, clip, EXPRESSION);
    assert!(points.len() >= 4, "{points:?}");
    assert!(points.iter().all(|point| (point.beat * 4.0).fract() == 0.0), "every step is on the grid: {points:?}");
    let (written, after) = points.split_at(points.len() - 1);
    assert!(written.iter().all(|point| point.shape == Segment::Hold), "steps hold their value: {points:?}");
    assert_eq!(after[0].beat, 2.0, "the stroke ends at the end of its last cell: {points:?}");
    let first = steps(&points)[0];
    assert!(first.0 == 1.0 && (80..=100).contains(&first.1), "from the cell the stroke began in: {points:?}");
    assert!(written.windows(2).all(|pair| pair[1].value < pair[0].value), "falling, as it was drawn: {points:?}");
    let last = automation_value(&points, 1.8) * 127.0;
    assert!((last - 40.0).abs() < 4.0, "the stroke ended near 40: {last}");
    assert_eq!(harness.undo_depth(), before + 1, "the whole stroke is one undo step");

    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(curve(&harness, clip, EXPRESSION).is_empty());

    // A click with the pencil is its shortest stroke: one cell.
    harness.click(at(&harness, 2.3, 64));
    let points = curve(&harness, clip, EXPRESSION);
    assert_eq!(steps(&points)[0], (2.25, 64));
    assert!((automation_value(&points, 2.4) * 127.0 - 64.0).abs() < 1.0);
    assert_eq!(harness.undo_depth(), before + 1);
}

#[test]
fn a_pedal_is_written_as_down_and_up() {
    let (mut harness, clip) = with_lane(ControllerKind::SUSTAIN, &[]);
    harness.move_to(in_lane(&harness, 1.0, 0.9));
    harness.idle(1);
    assert!(harness.hint().starts_with("Down at 1.2 "), "{}", harness.hint());

    harness.click(in_lane(&harness, 1.0, 0.8));
    harness.settle();
    harness.click(in_lane(&harness, 3.0, 0.1));
    let points = curve(&harness, clip, ControllerKind::SUSTAIN);
    assert_eq!(points.iter().map(|point| (point.beat, point.value)).collect::<Vec<_>>(), [(1.0, 1.0), (3.0, 0.0)]);
    assert!(points.iter().all(|point| point.shape == Segment::Hold), "a pedal steps, it does not glide: {points:?}");
    // What is played: up until the first point, down from there to the second.
    let clip = harness.project().clip(clip).unwrap().clone();
    let pedal = |beat: f64| clip.controller(ControllerKind::SUSTAIN).unwrap().value_at(beat);
    assert_eq!((pedal(0.5), pedal(2.0), pedal(3.5)), (0.0, 1.0, 0.0));
}

#[test]
fn the_pitch_wheel_rests_in_the_middle_of_its_lane() {
    let (mut harness, clip) = with_lane(ControllerKind::PitchBend, &[]);
    harness.click(in_lane(&harness, 1.0, 0.5));
    harness.settle();
    harness.click(in_lane(&harness, 2.0, 1.0));
    harness.settle();
    harness.click(in_lane(&harness, 3.0, 0.0));
    let points = curve(&harness, clip, ControllerKind::PitchBend);
    let values: Vec<f64> = points.iter().map(|point| point.value).collect();
    assert_eq!(values, [0.0, 1.0, -1.0], "at rest, fully up, fully down");
    harness.move_to(in_lane(&harness, 2.0, 1.0));
    harness.idle(1);
    assert!(harness.hint().starts_with("+100 %"), "{}", harness.hint());
}

#[test]
fn the_strip_says_which_controllers_a_clip_holds_and_finds_any_other() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    set_curve(&mut harness, clip, ControllerKind::MOD_WHEEL, &[(0.0, 0), (4.0, 127)]);
    set_curve(&mut harness, clip, ControllerKind::Controller(23), &[(0.0, 64)]);
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);

    // The standard four are always offered; what the clip holds beside
    // them has a chip of its own.
    for chip in ["Velocity lane", "Expression lane", "Mod wheel lane", "Pitch wheel lane", "Sustain lane", "CC 23 lane"]
    {
        assert!(harness.is_on_screen(chip), "{chip}");
    }
    assert!(!harness.is_on_screen("Breath lane"));
    // Each says whether the clip has a curve for it, without being opened.
    for (chip, held) in [("Mod wheel lane", true), ("CC 23 lane", true), ("Expression lane", false)] {
        harness.move_to(harness.labeled(chip));
        harness.idle(1);
        let says = harness.hint().contains("the clip has a curve for it");
        assert_eq!(says, held, "{chip}: {}", harness.hint());
    }
    assert_eq!(harness.app.state().piano.lane, LaneShown::Velocity, "looking opened nothing");

    // Any other controller is found by name or number.
    harness.click_label("Other controllers");
    harness.idle(2);
    assert!(harness.is_on_screen("Breath") && harness.is_on_screen("Pressure"), "the named ones are listed");
    harness.type_text("20");
    harness.idle(2);
    assert!(!harness.is_on_screen("Breath"), "the search narrows the list");
    harness.click_label("CC 20");
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(ControllerKind::Controller(20)));
    assert!(harness.is_on("CC 20 lane"), "what the lane shows has a chip while it shows it");
    assert!(harness.app.state().piano.lane_search.is_empty(), "the search is cleared for next time");

    harness.click_label("Other controllers");
    harness.idle(2);
    harness.type_text("breath");
    harness.idle(2);
    harness.click_label("Breath");
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(ControllerKind::BREATH));
    assert!(!harness.is_on_screen("CC 20 lane"), "nothing was written for 20, so its chip goes with it");
    assert!(harness.is_on("Breath lane"));
}

#[test]
fn the_lane_stays_as_chosen_from_clip_to_clip_and_keeps_time_with_the_notes() {
    let (mut harness, [.., two], [a, _, c]) = with_three_tracks(UiOptions::default());
    let _ = two;
    set_curve(&mut harness, a, EXPRESSION, &[(1.0, 100)]);
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    harness.click_label("Expression lane");
    harness.idle(2);
    harness.click(at(&harness, 1.0, 100));
    assert_eq!(selected(&harness).len(), 1);

    // Another clip: the lane goes on showing expression, with that clip's
    // curve and nothing selected in it.
    harness.click(harness.lane(10.0, 1));
    harness.idle(2);
    assert_eq!(harness.app.state().selected_clip, Some(c));
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(EXPRESSION));
    assert!(selected(&harness).is_empty());
    harness.click(at(&harness, 2.0, 50));
    assert_eq!(steps(&curve(&harness, c, EXPRESSION)), [(2.0, 50)]);
    assert_eq!(steps(&curve(&harness, a, EXPRESSION)), [(1.0, 100)], "the first clip's curve is its own");

    // Zoomed in, the lane is still under the notes: a point lands at the
    // beat of the note cell above it.
    let zoom_at = harness.note_cell(1.0, 60);
    let before = harness.app.state().piano.time.pixels_per_beat;
    harness.wheel(Modifiers::COMMAND, zoom_at, 1.0);
    assert!(harness.app.state().piano.time.pixels_per_beat > before, "zoomed in");
    let cell = harness.note_cell(3.0, 60);
    assert!(cell.x < SCREEN.x - 20.0, "beat 3 is still in view: {cell:?}");
    harness.settle();
    harness.click(pos2(cell.x, at(&harness, 0.0, 90).y));
    assert_eq!(steps(&curve(&harness, c, EXPRESSION)), [(2.0, 50), (3.0, 90)]);

    // A clip trimmed from its start keeps its curve where its notes are:
    // the lane is in the clip's own time, as the notes above it.
    harness.session.lock().edit(|project| project.trim_clip_start(c, 9.0)).unwrap();
    harness.idle(2);
    let note = harness.project().clip(c).unwrap().notes()[0];
    assert_eq!(note.start, 1.0, "the note is where it was in the clip");
    harness.settle();
    harness.double_click(at(&harness, 2.0, 50));
    assert_eq!(steps(&curve(&harness, c, EXPRESSION)), [(3.0, 90)], "and so is the point under the pointer");
}

#[test]
fn a_looping_clip_shades_what_does_not_play_and_still_edits_all_of_it() {
    let (mut harness, clip) = with_lane(EXPRESSION, &[(0.0, 64)]);
    harness
        .session
        .lock()
        .edit(|project| {
            let clip = project.clip_mut(clip)?;
            clip.set_loop_length(Some(2.0))
        })
        .unwrap();
    harness.idle(2);
    // A point past the loop's end is written like any other; it is not
    // played until the loop is made longer, exactly as a note there.
    harness.click(at(&harness, 3.0, 100));
    assert_eq!(steps(&curve(&harness, clip, EXPRESSION)), [(0.0, 64), (3.0, 100)]);
    let project = harness.project();
    let held = project.clip(clip).unwrap();
    // Song beat 8 is the clip's start; the loop is two beats long.
    let heard = held.controller_value_at(EXPRESSION, 8.0 + 3.0).unwrap();
    let again = held.controller_value_at(EXPRESSION, 8.0 + 1.0).unwrap();
    assert_eq!(heard, again, "beat 3 of the clip plays beat 1 of its loop");
}
