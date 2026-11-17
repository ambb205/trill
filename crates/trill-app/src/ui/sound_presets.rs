//! Sounds of more than one device: an instrument and the effects after
//! it, listed in the browser and carried from there to a track.
//!
//! Such a sound is a preset of its instrument that brings its effects
//! (`trill_engine::Session::give_track`), so the window does nothing of
//! its own with one: it says which sound and which track, and the session
//! makes the devices, in one undo step.
//!
//! (The browser's other tab calls audio files "sounds". Here the word
//! means what a track sounds like; this module's name keeps the two
//! apart.)
//!
//! A sound in the air is a [`PresetDrag`]. As with audio files
//! (`dragging`), each frame runs in three steps: [`begin`] before the
//! views, the views that can take it saying so with [`offer`], and [`end`]
//! after them, where a drag that was let go is carried out.

use super::colors;
use super::state::Cx;
use eframe::egui::{self, Align2, CursorIcon, Id, LayerId, Order, Pos2, Rect, vec2};
use trill_core::device::DeviceCategory;
use trill_core::{ChainRef, TrackId, TrackKind};
use trill_noir::{paint, text};

/// A sound as the browser lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The device type it is a preset of, and what that is called.
    pub uri: String,
    pub device: String,
    /// What the preset is asked for by, and what it is called.
    pub id: String,
    pub name: String,
    pub description: String,
    /// What it brings behind its first device, by name.
    pub after: Vec<String>,
    /// Whether someone saved it; otherwise its device type brings it.
    pub saved: bool,
}

impl Listed {
    /// What the sound is made of, in a few words: `Wavetable, then Chorus
    /// and Reverb`.
    pub fn made_of(&self) -> String {
        match self.after.as_slice() {
            [] => format!("{} alone", self.device),
            [one] => format!("{}, then {one}", self.device),
            [first @ .., last] => format!("{}, then {} and {last}", self.device, first.join(", ")),
        }
    }
}

/// The sounds there are: the ones that were saved first, by name, then
/// the ones the built-in devices bring, in their devices' order.
pub fn listed(cx: &Cx) -> Vec<Listed> {
    let named = |uri: &str| cx.descriptors.iter().find(|descriptor| descriptor.uri == uri);
    let mut sounds: Vec<Listed> = cx
        .snap
        .saved_sounds
        .iter()
        .filter_map(|sound| {
            // One whose first device this machine has not got cannot be
            // given to anything.
            let device = named(&sound.uri)?.name.clone();
            Some(Listed {
                uri: sound.uri.clone(),
                device,
                id: sound.info.id.clone(),
                name: sound.info.name.clone(),
                description: sound.info.description.clone(),
                after: sound.after.clone(),
                saved: true,
            })
        })
        .collect();
    sounds.sort_by_key(|sound| sound.name.to_lowercase());
    for descriptor in cx.descriptors {
        for preset in &descriptor.presets {
            let Some(after) = preset.after.as_deref().filter(|after| !after.is_empty()) else { continue };
            sounds.push(Listed {
                uri: descriptor.uri.clone(),
                device: descriptor.name.clone(),
                id: preset.name.clone(),
                name: preset.name.clone(),
                description: preset.description.clone(),
                after: trill_core::presets::names_after(after),
                saved: false,
            });
        }
    }
    sounds
}

/// Where a sound in the air would land.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// On a track: it becomes the track's sound.
    Track(TrackId),
    /// Below the tracks: a track of its own.
    NewTrack,
}

/// A sound being carried from the browser towards a track.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetDrag {
    /// The device type and the preset of it that the sound is.
    pub uri: String,
    pub preset: String,
    /// What to call it while it is in the air.
    pub label: String,
    pub pointer: Option<Pos2>,
    /// Where it would land if let go now.
    pub target: Option<Target>,
    /// Why it cannot be dropped where the pointer is, for the hint.
    pub refusal: Option<&'static str>,
}

/// Starts carrying `sound` out of the browser.
pub fn start(cx: &mut Cx, sound: &Listed) {
    cx.state.preset_drag = Some(PresetDrag {
        uri: sound.uri.clone(),
        preset: sound.id.clone(),
        label: sound.name.clone(),
        pointer: None,
        target: None,
        refusal: None,
    });
}

/// Step one of a frame; see the module notes.
pub fn begin(ctx: &egui::Context, cx: &mut Cx) {
    let Some(drag) = &mut cx.state.preset_drag else { return };
    drag.pointer = ctx.input(|input| input.pointer.latest_pos());
    drag.target = None;
    drag.refusal = None;
}

/// Where the sound in the air is, if there is one and it is over `area`.
pub fn over(cx: &Cx, area: Rect) -> Option<Pos2> {
    cx.state.preset_drag.as_ref()?.pointer.filter(|pointer| area.contains(*pointer))
}

/// Whether the sound in the air begins with an instrument, which only a
/// track that holds clips can play.
fn wants_notes(cx: &Cx, uri: &str) -> bool {
    let found = cx.descriptors.iter().find(|descriptor| descriptor.uri == uri);
    found.is_none_or(|descriptor| descriptor.category == DeviceCategory::Instrument)
}

/// Offers the track `track` as where the sound in the air would land, and
/// says whether it was taken: an instrument goes on a track that holds
/// clips.
pub fn offer_track(cx: &mut Cx, track: TrackId) -> bool {
    let Some(uri) = cx.state.preset_drag.as_ref().map(|drag| drag.uri.clone()) else { return false };
    let holds_clips = cx.snap.track(track).is_some_and(|track| track.kind == TrackKind::Normal);
    let fits = holds_clips || !wants_notes(cx, &uri);
    if let Some(drag) = &mut cx.state.preset_drag {
        match fits {
            true => drag.target = Some(Target::Track(track)),
            false => drag.refusal = Some("An instrument goes on a track that holds clips, not on a group or a return"),
        }
    }
    fits
}

/// Offers a track of its own as where the sound in the air would land.
pub fn offer_new_track(cx: &mut Cx) {
    if let Some(drag) = &mut cx.state.preset_drag {
        drag.target = Some(Target::NewTrack);
    }
}

/// Step three of a frame; see the module notes.
pub fn end(ctx: &egui::Context, cx: &mut Cx) {
    let Some(drag) = cx.state.preset_drag.clone() else { return };
    hint(cx, &drag);
    if ctx.input(|input| input.pointer.primary_down()) {
        ctx.set_cursor_icon(if drag.target.is_some() { CursorIcon::Copy } else { CursorIcon::Grabbing });
        floating_label(ctx, cx, &drag);
        // The label follows the pointer.
        ctx.request_repaint();
        return;
    }
    cx.state.preset_drag = None;
    if let Some(target) = drag.target {
        give(cx, &drag.uri, &drag.preset, &drag.label, target);
    }
}

/// Puts down the sound that is in the air, where it was: Escape. Returns
/// whether there was one.
pub fn cancel(cx: &mut Cx) -> bool {
    cx.state.preset_drag.take().is_some()
}

/// Gives `target` the sound: the preset `preset` of the device type
/// `uri`. One undo step, and the track it went to is selected. What the
/// sound names that this machine has not got is said, with what was made
/// without it.
pub fn give(cx: &mut Cx, uri: &str, preset: &str, label: &str, target: Target) {
    let added = match target {
        Target::Track(track) => {
            cx.try_session(|session| session.give_track(track, uri, preset)).map(|added| (track, added))
        }
        Target::NewTrack => {
            let index = cx.snap.project.tracks.len();
            let color = colors::new_track_color(index);
            let name = label.to_owned();
            cx.try_session(|session| {
                session.begin_gesture();
                let track = session.edit(|project| {
                    let track = project.add_track(name);
                    project.track_mut(track)?.color = color;
                    Ok(track)
                });
                let added = track.and_then(|track| {
                    let added = session.add_given(ChainRef::Track(track), uri, Some(preset), None);
                    // A sound that was not found before anything was made
                    // of it leaves the track behind: it goes too.
                    if added.is_err() && session.project().track(track).is_ok() {
                        session.undo();
                    }
                    added.map(|added| (track, added))
                });
                session.end_gesture();
                added
            })
        }
    };
    let Some((track, added)) = added else { return };
    cx.state.select_track(track);
    cx.state.selected_device = Some(added.device);
    let without = added.given.map(|given| given.without).unwrap_or_default();
    if !without.is_empty() {
        cx.state.toasts.error(format!("{label} was made without part of it: {}", without.join("; ")));
    }
}

/// Says in the status bar where the sound would land.
fn hint(cx: &mut Cx, drag: &PresetDrag) {
    let name = |track| cx.snap.track(track).map_or("the track", |track| track.name.as_str());
    let text = match (drag.target, drag.refusal) {
        (Some(Target::Track(track)), _) => format!("Drop to make {} the sound of {}", drag.label, name(track)),
        (Some(Target::NewTrack), _) => format!("Drop for a new track with {}", drag.label),
        (None, Some(refusal)) => refusal.to_owned(),
        (None, None) => "Drag onto a track, or below the tracks for a new one \u{00B7} Esc cancels".to_owned(),
    };
    cx.state.hint.set(text);
}

/// The name of what is being carried, beside the pointer.
fn floating_label(ctx: &egui::Context, cx: &Cx, drag: &PresetDrag) {
    let Some(pointer) = drag.pointer else { return };
    let theme = cx.theme;
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("preset-drag")));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_says_what_it_is_made_of() {
        let sound = |after: &[&str]| Listed {
            uri: "builtin:wavetable".into(),
            device: "Wavetable".into(),
            id: "Pad".into(),
            name: "Pad".into(),
            description: String::new(),
            after: after.iter().map(|name| name.to_string()).collect(),
            saved: false,
        };
        assert_eq!(sound(&[]).made_of(), "Wavetable alone");
        assert_eq!(sound(&["Reverb"]).made_of(), "Wavetable, then Reverb");
        assert_eq!(sound(&["Chorus", "Reverb"]).made_of(), "Wavetable, then Chorus and Reverb");
        assert_eq!(sound(&["Amp", "Delay", "Reverb"]).made_of(), "Wavetable, then Amp, Delay and Reverb");
    }
}
