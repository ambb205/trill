//! Bringing something into view: the track list scrolled to some tracks,
//! and the timeline scrolled, and zoomed only if it must be, to a stretch
//! of time.
//!
//! Whoever wants something shown leaves a [`Reveal`] in the arrangement's
//! state; it is carried out on the next frame, when the layout knows where
//! things are. It is for going somewhere on request (the assistant showing
//! what it changed, a row that leads to a clip), never for following
//! something that moves. While the song plays and the view follows the
//! playhead, time is left to the playhead and only the tracks are shown.

use super::Geometry;
use super::rows::Rows;
use crate::ui::state::Cx;
use crate::ui::timeline::TimeView;
use eframe::egui::Ui;
use trill_core::{Beats, TrackId};

/// A stretch narrower than this many pixels is zoomed in on.
const TOO_SMALL: f64 = 24.0;
/// How much of the view a stretch that had to be zoomed in on is given.
const ZOOMED_IN_SHARE: f64 = 0.25;
/// Zooming in on something small stops here: a bar of 4/4 this wide.
const ZOOMED_IN_LIMIT: f64 = 120.0;
/// The room left beside a stretch that was scrolled to, as a share of the
/// view.
const MARGIN: f64 = 0.06;
/// Frames to wait for a track the snapshot does not have yet.
const PATIENCE: u8 = 4;

/// Something to bring into view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reveal {
    /// The tracks to scroll the track list to. A group that folds one of
    /// them away is unfolded.
    pub tracks: Vec<TrackId>,
    /// The stretch of time to show, in beats.
    pub span: Option<(Beats, Beats)>,
    /// Frames this has waited for its tracks to be there.
    pub waited: u8,
}

/// Where to scroll a list `visible` tall, now at `scroll`, so that what
/// lies from `top` to `bottom` in it shows: no further than it takes, and
/// to its top if it is taller than the view.
fn scroll_to_show(scroll: f32, top: f32, bottom: f32, visible: f32) -> f32 {
    if bottom - top >= visible || top < scroll {
        top
    } else if bottom > scroll + visible {
        bottom - visible
    } else {
        scroll
    }
}

/// The view that shows `start..end` across `width` pixels, starting from
/// `time`: as it is if the stretch is in view, scrolled if that is enough,
/// and zoomed only for a stretch too long to fit or too short to see.
fn time_to_show(time: TimeView, start: Beats, end: Beats, width: f32) -> TimeView {
    let width = width.max(1.0) as f64;
    let span = (end - start).max(0.0);
    let pixels = span * time.pixels_per_beat;
    let fits = pixels <= width * (1.0 - 2.0 * MARGIN);
    let legible = pixels >= TOO_SMALL || span == 0.0;
    let mut to = time;
    if !fits {
        to.fit(start, end, width as f32);
        return to;
    }
    if !legible {
        // No closer than it takes to see it among what is around it.
        to.pixels_per_beat = (width * ZOOMED_IN_SHARE / span)
            .clamp(time.pixels_per_beat, ZOOMED_IN_LIMIT.max(time.pixels_per_beat))
            .min(TimeView::MAX_PIXELS_PER_BEAT);
        let shown = width / to.pixels_per_beat;
        to.origin = (start + span / 2.0 - shown / 2.0).max(0.0);
        return to;
    }
    let shown = width / time.pixels_per_beat;
    let margin = shown * MARGIN;
    if start < time.origin {
        to.origin = (start - margin).max(0.0);
    } else if end > time.origin + shown {
        to.origin = (end + margin - shown).max(0.0);
    }
    to
}

/// Carries out what was asked to be shown, if anything was.
pub(super) fn carry_out(ui: &Ui, cx: &mut Cx, geometry: &mut Geometry) {
    let Some(mut reveal) = cx.state.arrangement.show.take() else { return };
    let tracks = &cx.snap.project.tracks;
    let view = &mut cx.state.arrangement;
    let known = |id: &TrackId| tracks.iter().position(|track| track.id == *id);
    // Made a moment ago, a track may not have reached the snapshot yet.
    if reveal.tracks.iter().any(|id| known(id).is_none()) && reveal.waited < PATIENCE {
        reveal.waited += 1;
        view.show = Some(reveal);
        ui.ctx().request_repaint();
        return;
    }

    let mut unfolded = false;
    for group in reveal.tracks.iter().filter_map(known).filter_map(|index| tracks[index].group) {
        unfolded |= view.folded.remove(&group);
    }
    if unfolded {
        geometry.rows = Rows::build(tracks, view);
    }
    let rows = reveal.tracks.iter().filter_map(known).filter_map(|index| geometry.rows.track(index));
    let (top, bottom) = rows
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(top, bottom), row| (top.min(row.top), bottom.max(row.bottom())));
    if top <= bottom {
        let mut scroll = scroll_to_show(view.scroll_y, top, bottom, geometry.lanes.height());
        if scroll > view.scroll_y {
            // Scrolled down: on to where a row begins, if what was asked
            // for still shows from its top, so that no row is left cut in
            // half along the top.
            let whole = geometry.rows.rows.iter().map(|row| row.top).find(|row_top| *row_top >= scroll);
            scroll = whole.filter(|row_top| *row_top <= top).unwrap_or(scroll);
        }
        view.scroll_y = scroll.clamp(0.0, geometry.max_scroll());
        geometry.scroll_y = view.scroll_y;
    }

    // While the view pages along with the playhead it would be taken
    // straight back to it; the tracks are still shown.
    let following = cx.live.playing && view.follow;
    if let Some((start, end)) = reveal.span.filter(|_| !following) {
        // From where the view is headed, if it is on its way somewhere.
        let from = view.glide.map_or(view.time, |glide| glide.to);
        let to = time_to_show(from, start, end, geometry.lanes.width());
        let now = ui.input(|input| input.time);
        view.glide_to(to, now);
        if view.glide.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_track_list_scrolls_no_further_than_it_takes() {
        // In view: nothing moves.
        assert_eq!(scroll_to_show(100.0, 120.0, 180.0, 300.0), 100.0);
        // Above: its top comes to the top.
        assert_eq!(scroll_to_show(100.0, 40.0, 100.0, 300.0), 40.0);
        // Below: its bottom comes to the bottom.
        assert_eq!(scroll_to_show(100.0, 380.0, 440.0, 300.0), 140.0);
        // Taller than the view: from its top.
        assert_eq!(scroll_to_show(0.0, 200.0, 900.0, 300.0), 200.0);
    }

    #[test]
    fn time_is_left_alone_scrolled_or_zoomed_as_little_as_shows_the_stretch() {
        let view = TimeView { origin: 16.0, pixels_per_beat: 20.0 };
        // 1000 pixels show beats 16 to 66.
        assert_eq!(time_to_show(view, 20.0, 36.0, 1000.0), view, "in view already");
        assert_eq!(time_to_show(view, 64.0, 66.0, 1000.0), view, "to the very edge is still in view");

        // To the right: scrolled until its end is in, with room to spare.
        let right = time_to_show(view, 80.0, 96.0, 1000.0);
        assert_eq!(right.pixels_per_beat, 20.0, "the zoom is the user\u{2019}s");
        assert_eq!(right.origin, 96.0 + 3.0 - 50.0);
        let (from, to) = right.visible(1000.0);
        assert!(from <= 80.0 && to >= 96.0);
        // To the left: its start a little in from the edge, never before
        // the beginning.
        assert_eq!(time_to_show(view, 8.0, 12.0, 1000.0).origin, 5.0);
        assert_eq!(time_to_show(view, 1.0, 5.0, 1000.0).origin, 0.0);

        // Too long to fit: zoomed out until it does.
        let long = time_to_show(view, 0.0, 400.0, 1000.0);
        assert!(long.pixels_per_beat < 20.0);
        let (from, to) = long.visible(1000.0);
        assert!(from <= 0.0 && to >= 400.0, "{from}..{to}");

        // Too short to see: zoomed in, the stretch in the middle, but not
        // all the way.
        let far = TimeView { origin: 0.0, pixels_per_beat: 2.0 };
        let small = time_to_show(far, 200.0, 204.0, 1000.0);
        assert_eq!(small.pixels_per_beat, 62.5, "a quarter of the view");
        let (from, to) = small.visible(1000.0);
        assert!(((from + to) / 2.0 - 202.0).abs() < 1e-6);
        let tiny = time_to_show(far, 200.0, 200.25, 1000.0);
        assert_eq!(tiny.pixels_per_beat, ZOOMED_IN_LIMIT);
        // A point in time is only scrolled to.
        assert_eq!(time_to_show(far, 600.0, 600.0, 1000.0).pixels_per_beat, 2.0);
        assert_eq!(time_to_show(far, 100.0, 100.0, 1000.0), far);
    }
}
