//! Sounds in the air: dragged from the browser, or from another program,
//! towards the song.
//!
//! A drag is a [`SoundDrag`] in the window's state. Each frame runs in
//! three steps:
//!
//! 1. [`begin`], before the views: where is the pointer, and has another
//!    program brought files over the window or let them go?
//! 2. The views. Whichever can take the sound where the pointer is says
//!    so with [`SoundDrag::offer`] and draws how it would land: the
//!    arrangement a ghost clip, a pad or a sampler its outline.
//! 3. [`end`], after the views: if the button was let go, the drop is
//!    carried out on what was offered.
//!
//! Files from another program arrive differently. The system says that
//! files are over the window and, later, that they were dropped, but (with
//! the windowing library in use) not where. On Windows the pointer is asked
//! for directly, so such a drag shows its ghost like any other. Elsewhere
//! the drop waits a moment for the pointer's next report and lands there.

use super::state::{Cx, DragSource, SoundDrag, SystemDrop};
use super::{colors, timeline};
use crate::ui::sounds::{DropTarget, file_stem, is_instrument};
use eframe::egui::{self, Align2, CursorIcon, Event, Id, LayerId, Order, Pos2, Rect, vec2};
use std::path::PathBuf;
use trill_core::TrackKind;
use trill_noir::{paint, text};

/// Frames a drop from another program waits to learn where the pointer
/// is before it lands at the edit cursor instead.
const SYSTEM_DROP_WAIT: u32 = 20;

/// Starts dragging `paths` out of the browser. `seconds`, `tempo` and
/// `tags` describe the first of them, as far as the library knows.
pub fn start(cx: &mut Cx, paths: Vec<PathBuf>, seconds: Option<f64>, tempo: Option<f64>, tags: trill_library::Tags) {
    let Some(first) = paths.first() else { return };
    cx.state.sound_drag = Some(SoundDrag {
        source: DragSource::Browser,
        label: file_stem(first),
        paths,
        seconds,
        tempo,
        tags,
        pointer: None,
        target: None,
        refusal: None,
        study: None,
    });
}

/// Step one of a frame; see the module notes.
pub fn begin(ctx: &egui::Context, cx: &mut Cx) {
    let (hovered, dropped, pointer_reported, latest) = ctx.input(|input| {
        let hovered: Vec<PathBuf> = input.raw.hovered_files.iter().filter_map(|file| file.path.clone()).collect();
        let dropped: Vec<PathBuf> = input.raw.dropped_files.iter().map(|file| file.path().to_path_buf()).collect();
        let reported =
            input.events.iter().any(|event| matches!(event, Event::PointerMoved(_) | Event::PointerButton { .. }));
        (hovered, dropped, reported, input.pointer.latest_pos())
    });
    // Sounds, or an instrument: one, since a track plays through one.
    let audio = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
        match paths.iter().find(|path| is_instrument(path)) {
            Some(instrument) => vec![instrument.clone()],
            None => paths.into_iter().filter(|path| trill_media::is_audio_file(path)).collect(),
        }
    };

    if !dropped.is_empty() {
        let files = audio(dropped);
        if files.is_empty() {
            cx.state.toasts.info("Nothing in what was dropped is an audio file or an instrument Trill reads");
            cx.state.sound_drag = None;
        } else {
            cx.state.sound_drag = Some(system_drag(files.clone()));
            cx.state.system_drop = Some(SystemDrop { paths: files, waited: 0 });
        }
    } else if !hovered.is_empty() {
        let over = cx.state.sound_drag.as_ref().is_some_and(|drag| drag.source == DragSource::System);
        if !over {
            let files = audio(hovered);
            if !files.is_empty() {
                cx.state.sound_drag = Some(system_drag(files));
            }
        }
    } else if cx.state.system_drop.is_none()
        && cx.state.sound_drag.as_ref().is_some_and(|drag| drag.source == DragSource::System)
    {
        // The files were carried back out of the window.
        cx.state.sound_drag = None;
    }

    let waiting = cx.state.system_drop.is_some();
    let Some(drag) = &mut cx.state.sound_drag else { return };
    drag.pointer = match drag.source {
        // In a window of its own the workspace has the pointer: where that is in this one.
        DragSource::Study => crate::ui::study::window::pointer_here(ctx).or(latest),
        DragSource::Browser => latest,
        // After the drop the pointer reports again as soon as it moves.
        DragSource::System => system_pointer(ctx).or(latest.filter(|_| waiting && pointer_reported)),
    };
    drag.target = None;
    drag.refusal = None;
    // What a file from another program is like is not known until it has
    // been looked at, which a worker does once the ghost asks for it.
    if drag.source == DragSource::System
        && drag.seconds.is_none()
        && !drag.is_instrument()
        && let Some(wave) = drag.paths.first().and_then(|path| cx.sounds.wave(&path.to_string_lossy()).ready().cloned())
    {
        drag.seconds = Some(wave.seconds());
        drag.tempo = wave.facts.tempo;
        drag.tags = wave.facts.tags;
    }
}

fn system_drag(paths: Vec<PathBuf>) -> SoundDrag {
    let label = match paths.as_slice() {
        [only] => file_stem(only),
        [first, rest @ ..] => format!("{} +{}", file_stem(first), rest.len()),
        [] => String::new(),
    };
    SoundDrag {
        source: DragSource::System,
        paths,
        label,
        seconds: None,
        tempo: None,
        tags: trill_library::Tags::EMPTY,
        pointer: None,
        target: None,
        refusal: None,
        study: None,
    }
}

/// Step three of a frame; see the module notes.
pub fn end(ctx: &egui::Context, cx: &mut Cx) {
    let Some(drag) = cx.state.sound_drag.clone() else {
        cx.state.system_drop = None;
        return;
    };
    hint(cx, &drag);
    match drag.source {
        DragSource::Browser | DragSource::Study => {
            if crate::ui::study::window::held(ctx) {
                ctx.set_cursor_icon(if drag.target.is_some() { CursorIcon::Copy } else { CursorIcon::Grabbing });
                floating_label(ctx, cx, &drag);
                // The ghost follows the pointer.
                ctx.request_repaint();
                return;
            }
            cx.state.sound_drag = None;
            if let Some(target) = drag.target.clone() {
                match drag.study.clone() {
                    Some(study) => crate::ui::study::land(ctx, cx, study, target),
                    None => land(cx, drag, target),
                }
            }
        }
        DragSource::System => {
            let Some(pending) = &mut cx.state.system_drop else {
                // macOS says nothing while files hover over the window: each
                // frame asks for the next, which looks where the pointer is.
                #[cfg(target_os = "macos")]
                ctx.request_repaint();
                return;
            };
            if drag.pointer.is_none() && pending.waited < SYSTEM_DROP_WAIT {
                pending.waited += 1;
                ctx.request_repaint();
                return;
            }
            cx.state.system_drop = None;
            cx.state.sound_drag = None;
            let fallback = || fallback_target(cx, drag.paths.len()).for_drag(drag.is_instrument()).ok();
            match drag.target.clone().or_else(fallback) {
                Some(target) => land(cx, drag, target),
                // Let go over something that cannot take it.
                None => cx.state.toasts.info(drag.refusal.unwrap_or("That cannot be dropped there")),
            }
        }
    }
}

/// Puts down a sound dragged from the browser, where it was: Escape.
/// Returns whether there was one. Files another program is dragging are
/// that program's to let go of.
pub fn cancel(cx: &mut Cx) -> bool {
    let dragging = cx.state.sound_drag.as_ref().is_some_and(|drag| drag.source != DragSource::System);
    if dragging {
        cx.state.sound_drag = None;
    }
    dragging
}

/// Carries a drop out.
fn land(cx: &mut Cx, drag: SoundDrag, target: DropTarget) {
    let project = &cx.snap.project;
    let length = drag.beats(project.tempo, project.time_signature.beats_per_bar());
    cx.sounds.drop_files(drag.paths, target, drag.label, length);
}

/// Where sounds go when nothing says where: onto the selected track at the
/// edit cursor if it holds clips, else onto tracks of their own.
fn fallback_target(cx: &Cx, files: usize) -> DropTarget {
    let beat = cx.state.arrangement.cursor;
    let selected = cx.state.selected_track.and_then(|id| cx.snap.track(id));
    match selected.filter(|track| track.kind == TrackKind::Normal && !cx.state.master_selected) {
        Some(track) => DropTarget::Lane { track: track.id, beat },
        None => DropTarget::NewTrack { beat, colors: new_track_colors(cx, files) },
    }
}

/// The colors of the next `count` tracks the project is given.
pub fn new_track_colors(cx: &Cx, count: usize) -> Vec<u32> {
    let first = cx.snap.project.tracks.len();
    (0..count).map(|offset| colors::new_track_color(first + offset)).collect()
}

/// Says in the status bar where the drag would land.
fn hint(cx: &mut Cx, drag: &SoundDrag) {
    let project = &cx.snap.project;
    let bars = project.meter_map();
    let at = |beat: f64| timeline::format_position(beat, &bars);
    let name = |track| cx.snap.track(track).map_or("the track", |track| track.name.as_str());
    let device = |id| project.device(id).map_or("the device", |device| device.name.as_str());
    let text = match (&drag.target, drag.refusal) {
        (Some(DropTarget::Lane { track, beat }), _) => {
            format!("Drop to add {} to {} at {}", drag.label, name(*track), at(*beat))
        }
        (Some(DropTarget::NewTrack { beat, .. }), _) => {
            format!("Drop for a new audio track with {} at {}", drag.label, at(*beat))
        }
        (Some(DropTarget::Sampler { device: id }), _) => {
            format!("Drop to make {} what {} plays", drag.label, device(*id))
        }
        (Some(DropTarget::Pad { note, .. }), _) => {
            format!("Drop to put {} on pad {}", drag.label, crate::ui::sample_panels::pad_title(*note))
        }
        (Some(DropTarget::Rack { device: id }), _) => {
            format!("Drop to put {} on the pad of {} it belongs on", drag.label, device(*id))
        }
        (Some(DropTarget::Instrument { device: id }), _) => {
            format!("Drop to make {} what {} plays", drag.label, device(*id))
        }
        (Some(DropTarget::TrackInstrument { track }), _) => {
            format!("Drop to make {} the instrument of {}", drag.label, name(*track))
        }
        (Some(DropTarget::NewInstrument { .. }), _) => {
            format!("Drop for a new track that plays {}", drag.label)
        }
        (None, Some(refusal)) => refusal.to_owned(),
        (None, None) if drag.is_instrument() => {
            "Drag onto a track, below the tracks for a new one, or onto a Sampled Instrument \u{00B7} Esc cancels"
                .to_owned()
        }
        (None, None) => {
            "Drag onto a track, below the tracks for a new one, or onto a sampler or a pad \u{00B7} Esc cancels"
                .to_owned()
        }
    };
    cx.state.hint.set(text);
}

/// The name of what is being dragged, beside the pointer.
fn floating_label(ctx: &egui::Context, cx: &Cx, drag: &SoundDrag) {
    let Some(pointer) = drag.pointer else { return };
    let theme = cx.theme;
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("sound-drag")));
    let width = (text::width(&painter, theme.text.body, &drag.label) + 16.0).min(320.0);
    let screen = ctx.content_rect();
    let corner = pointer + vec2(14.0, 12.0);
    let left = corner.x.min(screen.right() - width - 4.0);
    let top = corner.y.min(screen.bottom() - 24.0);
    let plate = Rect::from_min_size(Pos2::new(left.round(), top.round()), vec2(width.ceil(), 20.0));
    painter.rect_filled(plate, 0.0, theme.raised);
    paint::frame(&painter, plate, if drag.target.is_some() { theme.ink_dim } else { theme.hairline });
    let ink = if drag.target.is_some() { theme.ink } else { theme.ink_dim };
    text::paint_name(&painter, plate.shrink2(vec2(8.0, 0.0)), Align2::LEFT_CENTER, theme.text.body, &drag.label, ink);
}

/// Where the pointer is while another program drags files over the
/// window, if the system can be asked.
#[cfg(windows)]
fn system_pointer(ctx: &egui::Context) -> Option<Pos2> {
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
    }
    let mut point = Point { x: 0, y: 0 };
    // SAFETY: `GetCursorPos` writes one POINT (two 32-bit integers) through
    // the pointer it is given, which is to a live value of that layout.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return None;
    }
    // The window's content area on the desktop, in points; unknown when
    // there is no real window (a test).
    let inner = ctx.input(|input| input.viewport().inner_rect)?;
    let scale = ctx.pixels_per_point();
    let position = Pos2::new(point.x as f32 / scale, point.y as f32 / scale) - inner.min.to_vec2();
    Rect::from_min_size(Pos2::ZERO, inner.size()).contains(position).then_some(position)
}

/// The same on macOS, which tells a window that files came over it and
/// were dropped, and nothing between: the pointer is asked of CoreGraphics
/// (where it is, in points from the top left of the main display), which
/// takes no permission. Looking is not watching: no event is listened to.
#[cfg(target_os = "macos")]
fn system_pointer(ctx: &egui::Context) -> Option<Pos2> {
    let (inner, native) = ctx.input(|input| (input.viewport().inner_rect, input.viewport().native_pixels_per_point));
    in_window(screen_pointer()?, inner?, native?, ctx.pixels_per_point())
}

/// Where the pointer is, in points from the top left of the main display.
#[cfg(target_os = "macos")]
fn screen_pointer() -> Option<Pos2> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *mut c_void) -> Point;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
    }
    // SAFETY: an event made from no source only says where the pointer is;
    // it is let go of once it has.
    let point = unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        point
    };
    Some(Pos2::new(point.x as f32, point.y as f32))
}

/// Where a point the system says in its points is in the window's content
/// area of `inner`, which is in the window's points: the system's points are
/// `native` pixels each and the window's `scale`, which differ where the
/// window is zoomed. `None` outside it.
#[cfg(target_os = "macos")]
fn in_window(system: Pos2, inner: Rect, native: f32, scale: f32) -> Option<Pos2> {
    let position = Pos2::new(system.x * native / scale, system.y * native / scale) - inner.min.to_vec2();
    Rect::from_min_size(Pos2::ZERO, inner.size()).contains(position).then_some(position)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_pointer(_ctx: &egui::Context) -> Option<Pos2> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_the_system_says_is_found_in_the_window_at_any_zoom() {
        let inner = Rect::from_min_size(Pos2::new(100.0, 50.0), egui::vec2(800.0, 600.0));
        // A window that is not zoomed: its points are the system's.
        assert_eq!(in_window(Pos2::new(150.0, 60.0), inner, 2.0, 2.0), Some(Pos2::new(50.0, 10.0)));
        assert_eq!(in_window(Pos2::new(99.0, 60.0), inner, 2.0, 2.0), None, "left of it");
        assert_eq!(in_window(Pos2::new(150.0, 700.0), inner, 2.0, 2.0), None, "under it");
        // Zoomed in by a quarter, the window's points are larger: where it
        // is is said in them too.
        let zoomed = Rect::from_min_size(Pos2::new(80.0, 40.0), egui::vec2(640.0, 480.0));
        assert_eq!(in_window(Pos2::new(150.0, 60.0), zoomed, 2.0, 2.5), Some(Pos2::new(40.0, 8.0)));
    }

    #[test]
    fn the_system_says_where_the_pointer_is_without_being_asked_for_leave() {
        let pointer = screen_pointer().expect("CoreGraphics says where the pointer is");
        assert!(pointer.x.is_finite() && pointer.y.is_finite(), "{pointer:?}");
    }
}
