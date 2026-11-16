//! Arming tracks and recording takes: what the window adds to the session's
//! own rules.
//!
//! The session decides where a keyboard plays (the armed tracks, or the
//! selected track when none is armed) and what a take records (the armed
//! tracks). The window keeps it told of the selection, shows what is armed
//! and what a take is doing, and adds the manners producers expect:
//!
//! * arming a track disarms the others, unless Ctrl is held;
//! * a track gets an arm control when it has an instrument to play;
//! * asked to record with nothing armed, the window arms the selected
//!   track if that can be played, and says why not if it cannot;
//! * while a take is written, the Clip tab shows its clip, so the notes
//!   and the wheels and pedals moved are seen going down as they are
//!   played;
//! * when a take ends, what it wrote is selected.
//!
//! Neither arming nor the count-in is part of the project: they are not
//! saved with it and are not undo steps. A take is one undo step.

use super::shortcuts::{self, Command};
use super::snapshot::Snapshot;
use super::state::{Cx, FocusArea, UiState};
use eframe::egui::{Color32, Id, Rect, Response, Ui};
use trill_core::device::DeviceCategory;
use trill_core::{Track, TrackId, TrackKind};
use trill_engine::{RecordState, RecordStatus, TakeClip};
use trill_noir::{Chip, Icon};

/// The count-ins offered, in bars. The session takes any number up to
/// eight; these are the ones anybody asks for.
pub const COUNT_INS: [u32; 4] = [0, 1, 2, 4];

/// How a count-in of `bars` reads.
pub fn count_in_name(bars: u32) -> String {
    match bars {
        0 => "No count-in".to_owned(),
        1 => "1 bar".to_owned(),
        bars => format!("{bars} bars"),
    }
}

/// Whether `track` has an instrument: something for a note to play.
pub fn has_instrument(cx: &Cx, track: &Track) -> bool {
    track.devices.iter().any(|device| {
        let known = cx.descriptors.iter().find(|known| known.uri == device.uri);
        // A device the catalog does not know (a plugin still being looked
        // for) may well be one.
        known.is_none_or(|known| known.category == DeviceCategory::Instrument)
    })
}

/// Whether `track` gets an arm control: it holds clips and has an
/// instrument, or it is armed already and must be able to be disarmed.
pub fn armable(cx: &Cx, track: &Track) -> bool {
    cx.live.armed.contains(&track.id) || (track.kind == TrackKind::Normal && has_instrument(cx, track))
}

/// The arm control of `track` was clicked. Arming it disarms the others,
/// as one keyboard player wants, unless `keep_others` (Ctrl held); a click
/// on an armed track disarms that one.
pub fn arm_clicked(cx: &mut Cx, track: TrackId, keep_others: bool) {
    cx.try_session(|session| {
        if session.is_armed(track) {
            return session.set_armed(track, false);
        }
        if !keep_others {
            for other in session.armed().to_vec() {
                session.set_armed(other, false)?;
            }
        }
        session.set_armed(track, true)
    });
}

/// The arm control of a track, in `rect`: lit in the signal color while
/// the track is armed.
pub fn arm_chip(ui: &Ui, cx: &mut Cx, track: &Track, rect: Rect, id: Id, quiet: Color32, name: &str) -> Response {
    let theme = cx.theme;
    let armed = cx.live.armed.contains(&track.id);
    let recording = cx.live.take.as_ref().is_some_and(|take| take.tracks.contains(&track.id));
    let chip = Chip::new("", armed).glyph(Icon::Record).accent(theme.signal).quiet(quiet).named(name);
    let response = trill_noir::chip(ui, theme, rect, id, &chip);
    let about = match (armed, recording) {
        (_, true) => "This track is being recorded",
        (true, false) => "Armed: keyboards play this track and Record records it \u{00B7} click to disarm",
        (false, false) => {
            "Arm: keyboards play this track and Record records it \u{00B7} Ctrl-click keeps the others armed"
        }
    };
    cx.hint(&response, about);
    if response.clicked() {
        let keep_others = ui.input(|input| input.modifiers.command);
        arm_clicked(cx, track.id, keep_others);
    }
    response
}

/// Why nothing can be recorded on the selected track, when nothing is
/// armed and it cannot stand in.
fn why_not(cx: &Cx) -> String {
    let add = shortcuts::keys(Command::AddTrack);
    match cx.state.selected_track.and_then(|id| cx.snap.track(id)) {
        None => format!("There is no track to record on. Add one ({add}): it comes with an instrument."),
        Some(track) if track.kind == TrackKind::Group => {
            format!("{} is a group and holds no clips. Select or arm a track with an instrument to record.", track.name)
        }
        Some(track) if track.kind == TrackKind::Return => {
            format!(
                "{} is a return and holds no clips. Select or arm a track with an instrument to record.",
                track.name
            )
        }
        Some(track) => {
            format!(
                "{} has no instrument, so there is nothing to record. Select or arm a track that has one.",
                track.name
            )
        }
    }
}

/// The record control: starts a take on the armed tracks, or ends the one
/// being recorded. With nothing armed, the selected track is armed if it
/// can be played; if it cannot, the window says why and records nothing.
pub fn toggle(cx: &mut Cx) {
    if cx.live.take.is_some() {
        return stop(cx);
    }
    let selected = cx.state.selected_track.and_then(|id| cx.snap.track(id));
    let stand_in = selected.filter(|track| track.kind == TrackKind::Normal && has_instrument(cx, track)).map(|t| t.id);
    let mut refused = false;
    let started = cx.try_session(|session| {
        if session.is_recording() {
            // Started elsewhere a moment ago: the control ends it.
            session.stop();
            return Ok(());
        }
        if session.armed().is_empty() {
            match stand_in {
                Some(track) => session.set_armed(track, true)?,
                None => {
                    refused = true;
                    return Ok(());
                }
            }
        }
        session.record()
    });
    if refused && started.is_some() {
        let reason = why_not(cx);
        cx.state.toasts.info(reason);
    }
}

/// Ends the take, and stops the song with it, as the session defines.
pub fn stop(cx: &mut Cx) {
    cx.with_session(|session| session.stop());
}

pub fn set_count_in(cx: &mut Cx, bars: u32) {
    cx.with_session(|session| session.set_count_in(bars));
}

/// What a take is doing, in the two words the transport has room for.
pub fn state_label(take: &RecordStatus, looping: bool) -> String {
    match take.state {
        RecordState::Idle => String::new(),
        RecordState::CountIn => "Count-in".to_owned(),
        RecordState::Recording if looping => format!("Pass {}", take.pass + 1),
        RecordState::Recording => "Rec".to_owned(),
    }
}

/// What a take is doing, in a sentence.
pub fn describe(cx: &Cx, take: &RecordStatus) -> String {
    let names: Vec<&str> =
        take.tracks.iter().filter_map(|id| cx.snap.track(*id)).map(|track| track.name.as_str()).collect();
    let on = match names.as_slice() {
        [] => String::new(),
        [only] => format!(" on {only}"),
        [first, rest @ ..] => format!(" on {first} and {} more", rest.len()),
    };
    let stop = shortcuts::keys(Command::TogglePlay);
    match take.state {
        RecordState::Idle => String::new(),
        RecordState::CountIn => format!("Counting in{on}: recording starts with the song \u{00B7} {stop} cancels"),
        RecordState::Recording => {
            let notes = match take.notes {
                1 => "1 note".to_owned(),
                notes => format!("{notes} notes"),
            };
            let pass = if cx.snap.project.loop_region.enabled {
                format!(" \u{00B7} pass {} of the loop", take.pass + 1)
            } else {
                String::new()
            };
            format!("Recording{on}{pass} \u{00B7} {notes} so far \u{00B7} {stop} or Stop ends the take")
        }
    }
}

/// A take is writing into `clips`: the one on the selected track (or the
/// first, when that track is not recorded) becomes the clip the Clip tab
/// shows, so that what is played is seen going down there, the controller
/// lane's curve with the notes. Once per take: someone who then looks at
/// another clip is left to.
pub fn take_writes(state: &mut UiState, snapshot: &Snapshot, clips: &[TakeClip]) {
    let on_selected = clips.iter().find(|clip| Some(clip.track) == state.selected_track);
    let Some(wanted) = on_selected.or(clips.first()) else { return };
    if state.take_shown == Some(wanted.clip) || snapshot.clip(wanted.clip).is_none() {
        return;
    }
    state.take_shown = Some(wanted.clip);
    state.select_clips(vec![wanted.clip]);
}

/// A take has ended: what it wrote is selected, so that the Clip tab shows
/// it and the next key press acts on it. A take in which nothing was
/// played wrote nothing, and says so.
pub fn take_ended(state: &mut UiState, snapshot: &Snapshot, clips: &[TakeClip]) {
    let written: Vec<&TakeClip> = clips.iter().filter(|clip| snapshot.clip(clip.clip).is_some()).collect();
    let Some(last) = written.last() else {
        state.toasts.info("Nothing was played during the take, so nothing was recorded");
        return;
    };
    state.select_track(last.track);
    state.select_clips(written.iter().map(|clip| clip.clip).collect());
    state.arrangement.time_selection = None;
    state.focus = FocusArea::Arrangement;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(state: RecordState, pass: u32) -> RecordStatus {
        RecordStatus { state, tracks: Vec::new(), start_beat: None, pass, notes: 0, clips: Vec::new() }
    }

    #[test]
    fn the_transport_says_what_a_take_is_doing() {
        assert_eq!(state_label(&take(RecordState::CountIn, 0), false), "Count-in");
        assert_eq!(state_label(&take(RecordState::CountIn, 0), true), "Count-in");
        assert_eq!(state_label(&take(RecordState::Recording, 0), false), "Rec");
        assert_eq!(state_label(&take(RecordState::Recording, 0), true), "Pass 1", "passes are counted from one");
        assert_eq!(state_label(&take(RecordState::Recording, 3), true), "Pass 4");
        assert_eq!(state_label(&take(RecordState::Idle, 0), true), "");
    }

    #[test]
    fn count_ins_read_in_bars() {
        let names: Vec<String> = COUNT_INS.iter().map(|bars| count_in_name(*bars)).collect();
        assert_eq!(names, ["No count-in", "1 bar", "2 bars", "4 bars"]);
        assert!(COUNT_INS.iter().all(|bars| *bars <= trill_engine::MAX_COUNT_IN_BARS));
    }
}
