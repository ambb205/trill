//! MIDI channels in the piano roll: the channel of the selected notes is
//! set in the sidebar, new notes are drawn on the chosen one, and the lane
//! under the notes shows and edits the chosen channel's curve.

use super::*;
use crate::ui::state::LaneShown;
use trill_core::{AutomationPoint, Channel, ControllerKind, InstrumentProfile};

const EXPRESSION: ControllerKind = ControllerKind::EXPRESSION;
/// How far the Channel field is dragged for one channel: the field moves
/// a twelfth of a channel a point.
const A_CHANNEL: f32 = 12.5;

fn channel(number: u8) -> Channel {
    Channel::new(number).unwrap()
}

/// The notes of `clip` as `(pitch, channel)`.
fn channels_of(harness: &Harness, clip: ClipId) -> Vec<(u8, u8)> {
    harness.project().clip(clip).unwrap().notes().iter().map(|note| (note.pitch, note.channel.number())).collect()
}

/// The window on two tracks with the first one's clip, holding `notes`,
/// in the piano roll.
fn with_notes(notes: &[Note]) -> (Harness, TrackId, ClipId) {
    let (mut harness, track, _, clip) = with_two_tracks(UiOptions::default());
    let notes = notes.to_vec();
    harness.session.lock().edit(|project| project.clip_mut(clip)?.set_notes(notes)).unwrap();
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    (harness, track, clip)
}

/// Drags the sidebar's Channel field up by `channels` channels.
fn raise_channel(harness: &mut Harness, channels: f32) {
    let field = harness.labeled("Channel");
    harness.drag(field, field - vec2(0.0, channels * A_CHANNEL));
    harness.idle(1);
}

fn note(pitch: u8, start: f64) -> Note {
    Note { pitch, start, length: 1.0, velocity: 100, ..Default::default() }
}

#[test]
fn the_channel_of_the_selected_notes_is_set_in_the_sidebar_and_new_notes_are_drawn_on_the_chosen_one() {
    let (mut harness, _, clip) = with_notes(&[note(60, 1.0), note(64, 2.0)]);
    assert_eq!(channels_of(&harness, clip), [(60, 1), (64, 1)]);
    assert_eq!(harness.app.state().piano.channel, Channel::FIRST);

    // One note selected, and the field dragged up two channels: that
    // note is on the third, the other where it was, in one undo step.
    harness.click(harness.note_cell(1.5, 60));
    harness.idle(1);
    assert_eq!(harness.app.state().selected_notes.len(), 1);
    let before = harness.undo_depth();
    raise_channel(&mut harness, 2.0);
    assert_eq!(channels_of(&harness, clip), [(60, 3), (64, 1)]);
    assert_eq!(harness.undo_depth(), before + 1, "the whole drag is one undo step");
    assert_eq!(harness.app.state().piano.channel, channel(3), "and it is the chosen channel now");
    assert_eq!(harness.app.state().selected_notes[0].channel, channel(3), "the note is still the selection");

    // A note drawn now is drawn on the chosen channel.
    harness.settle();
    harness.click(harness.note_cell(3.5, 67));
    harness.idle(1);
    assert_eq!(channels_of(&harness, clip), [(60, 3), (64, 1), (67, 3)]);
    // Moved, it keeps its channel.
    harness.settle();
    harness.drag(harness.note_cell(3.5, 67), harness.note_cell(5.5, 69));
    assert_eq!(channels_of(&harness, clip), [(60, 3), (64, 1), (69, 3)]);

    // Every note selected and the field dragged down past the first:
    // they are all on the first again.
    harness.key(Modifiers::COMMAND, Key::A);
    raise_channel(&mut harness, -6.0);
    assert_eq!(channels_of(&harness, clip), [(60, 1), (64, 1), (69, 1)]);
    assert_eq!(harness.app.state().piano.channel, Channel::FIRST);
}

#[test]
fn the_lane_shows_and_edits_the_curve_of_the_chosen_channel() {
    let (mut harness, _, clip) = with_notes(&[note(60, 1.0)]);
    let second = channel(2);
    harness
        .session
        .lock()
        .edit(|project| {
            let clip = project.clip_mut(clip)?;
            clip.set_controller(EXPRESSION, vec![AutomationPoint::new(0.0, 0.8)])?;
            clip.set_controller_on(second, EXPRESSION, vec![AutomationPoint::new(0.0, 0.2)])
        })
        .unwrap();
    harness.idle(1);
    harness.click_label("Expression lane");
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(EXPRESSION));
    assert!(harness.is_on_screen("Expression curve"), "the first channel's, which needs no saying");

    // The second channel chosen, with nothing selected: the lane is its
    // curve now, and says so.
    raise_channel(&mut harness, 1.0);
    assert_eq!(harness.app.state().piano.channel, second);
    assert!(harness.is_on_screen("Expression curve, channel 2"));
    assert!(!harness.is_on_screen("Expression curve"));

    // A click in the lane adds a point to that curve, and leaves the
    // first channel's as it was.
    let points = |harness: &Harness, channel: Channel| {
        let project = harness.project();
        let curve = project.clip(clip).unwrap().controller_on(channel, EXPRESSION).map(|curve| curve.points.len());
        curve.unwrap_or(0)
    };
    assert_eq!((points(&harness, Channel::FIRST), points(&harness, second)), (1, 1));
    harness.settle();
    harness.click(harness.velocity_lane(2.5, 6.0 + 0.7 * (piano_roll::VELOCITY_LANE - 12.0)));
    harness.idle(1);
    assert_eq!((points(&harness, Channel::FIRST), points(&harness, second)), (1, 2));
}

#[test]
fn an_instrument_that_names_its_channels_is_said_so_where_a_channel_is_chosen() {
    let (mut harness, track, _) = with_notes(&[note(60, 1.0)]);
    // An ordinary instrument: a channel is nothing to choose.
    harness.move_to(harness.labeled("Channel"));
    harness.idle(1);
    assert!(harness.hint().contains("leave it on 1"), "{}", harness.hint());
    assert!(harness.app.snapshot().channel_names(track).is_none());

    // One that calls its second channel its pedalboard.
    let mut profile = InstrumentProfile::default();
    profile.channels.insert(channel(2), "Pedalboard".to_string());
    harness.session.lock().edit(|project| project.set_track_profile(track, profile)).unwrap();
    harness.idle(2);
    let names = harness.app.snapshot().channel_names(track).cloned().unwrap_or_default();
    assert_eq!(names.get(&channel(2)).map(String::as_str), Some("Pedalboard"));
    harness.move_to(harness.labeled("Channel"));
    harness.idle(1);
    assert!(harness.hint().contains("gives its channels meanings"), "{}", harness.hint());
}
