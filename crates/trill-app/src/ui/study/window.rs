//! The workspace in a window of its own, and a drag that crosses from it
//! into the song.
//!
//! The second window is Trill's own: the same view, drawn in the same
//! frame, by the same code, holding the same state. What is picked up in
//! it is the window's one drag (`dragging`), so the arrangement offers
//! and takes it as it takes a sound from the browser.
//!
//! A drag that leaves a window does not arrive in the other by itself.
//! The window in which the button went down keeps the pointer until the
//! button comes up (the system gives it the mouse for as long), and
//! reports where it is in its own coordinates, past its own edges. Each
//! window knows where it stands on the desktop. So where the pointer is in
//! the song's window is where it is in the study's, moved by how far the
//! two stand apart: [`across`]. Where there is no second window (a test,
//! a backend that has none), the view is a panel inside the first, in the
//! first's own coordinates, and nothing needs carrying.

use crate::ui::state::Cx;
use eframe::egui::{self, Pos2, Rect, ViewportBuilder, ViewportClass, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the window of its own is up: nobody asks after its pointer
/// while it is not.
static SHOWN: AtomicBool = AtomicBool::new(false);

fn id() -> ViewportId {
    ViewportId::from_hash_of("trill-study")
}

/// Where a point of one window is in another, given where the content of
/// each stands on the desktop: both in points.
pub fn across(point: Pos2, from: Rect, to: Rect) -> Pos2 {
    point + from.min.to_vec2() - to.min.to_vec2()
}

/// Where the pointer is in the song's window while the study's window has
/// it; `None` while it has not, or is not there.
pub fn pointer_here(ctx: &egui::Context) -> Option<Pos2> {
    if !SHOWN.load(Ordering::Relaxed) || ctx.embed_viewports() {
        return None;
    }
    let (down, at, theirs) = ctx.input_for(id(), |input| {
        (input.pointer.primary_down(), input.pointer.latest_pos(), input.viewport().inner_rect)
    });
    let ours = ctx.input(|input| input.viewport().inner_rect);
    match (down, at, theirs, ours) {
        (true, Some(at), Some(theirs), Some(ours)) => Some(across(at, theirs, ours)),
        _ => None,
    }
}

/// Whether the button a drag is held by is down, in whichever window has it.
pub fn held(ctx: &egui::Context) -> bool {
    let here = ctx.input(|input| input.pointer.primary_down());
    let there = SHOWN.load(Ordering::Relaxed)
        && !ctx.embed_viewports()
        && ctx.input_for(id(), |input| input.pointer.primary_down());
    here || there
}

/// Shows the workspace in its window while it is asked for there.
pub fn show(ctx: &egui::Context, cx: &mut Cx) {
    SHOWN.store(cx.studies.apart, Ordering::Relaxed);
    if !cx.studies.apart {
        return;
    }
    let title = match cx.studies.open() {
        Some(open) => format!("Study: {} \u{2013} Trill", open.title),
        None => "Study \u{2013} Trill".to_owned(),
    };
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([1180.0, 440.0])
        .with_min_inner_size([640.0, 240.0]);
    let closed = ctx.show_viewport_immediate(id(), builder, |ui, class| {
        match class {
            // No second window to be had: a panel inside the first.
            ViewportClass::EmbeddedWindow => {
                let area = ui.available_rect_before_wrap();
                let area = Rect::from_min_size(area.min, egui::vec2(area.width().max(900.0), area.height().max(360.0)));
                let mut child = ui.new_child(egui::UiBuilder::new().id_salt("study-apart").max_rect(area));
                super::view::show(&mut child, cx);
                ui.allocate_rect(area, egui::Sense::hover());
            }
            _ => {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| super::view::show(ui, cx));
            }
        }
        ui.ctx().input(|input| input.viewport().close_requested())
    });
    if closed {
        cx.studies.apart = false;
        SHOWN.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    #[test]
    fn a_point_in_one_window_is_found_in_the_other_by_how_far_they_stand_apart() {
        // The song's window at the top left of the desktop, the study's below it and to the right.
        let song = Rect::from_min_size(pos2(100.0, 80.0), vec2(1400.0, 800.0));
        let study = Rect::from_min_size(pos2(300.0, 920.0), vec2(1180.0, 440.0));
        // A press in the study's window, forty points in: that far into the desktop, and not in the song's window.
        let pressed = pos2(40.0, 40.0);
        assert_eq!(across(pressed, study, song), pos2(240.0, 880.0));
        // Carried up over the study's own top edge, its own coordinates go negative, and the place is a lane of the song.
        let carried = pos2(200.0, -500.0);
        let there = across(carried, study, song);
        assert_eq!(there, pos2(400.0, 340.0));
        assert!(Rect::from_min_size(Pos2::ZERO, song.size()).contains(there));
        // And back again is where it came from.
        assert_eq!(across(there, song, study), carried);
    }
}
