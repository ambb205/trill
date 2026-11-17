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
//! views, the views that can take it saying so with [`offer_track`] or
//! [`offer_new_track`], and [`end`] after them, where a drag that was let
//! go is carried out.
//!
//! A sound that is saved says more of itself than a name, so saving one
//! is a small form ([`Form`]: name, description, tags), and the same form
//! renames and describes anew what was saved, a preset of one device
//! included. Deleting what was saved cannot be taken back: it is asked
//! about first. Both are drawn by [`modals`].

use super::state::Cx;
use super::{colors, widgets};
use eframe::egui::{self, Align2, CursorIcon, Id, LayerId, Order, Pos2, Rect, Role, Sense, WidgetInfo, vec2};
use trill_core::device::DeviceCategory;
use trill_core::{ChainRef, Device, TrackId, TrackKind};
use trill_noir::{Variant, paint, text};

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
    /// Where its device type files it (`Bass`, `Pad`); empty for one that
    /// was saved.
    pub kind: String,
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
                kind: String::new(),
            })
        })
        .collect();
    sounds.sort_by_key(|sound| sound.name.to_lowercase());
    for descriptor in cx.descriptors {
        for preset in &descriptor.presets {
            // A sound that is dry is a sound too; a preset that is filed
            // nowhere (a synth's plain starting point) is not one to choose.
            let Some(after) = preset.after.as_deref().filter(|_| !preset.category.is_empty()) else { continue };
            sounds.push(Listed {
                uri: descriptor.uri.clone(),
                device: descriptor.name.clone(),
                id: preset.name.clone(),
                name: preset.name.clone(),
                description: preset.description.clone(),
                after: trill_core::presets::names_after(after),
                saved: false,
                kind: preset.category.clone(),
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

// ---- saving, renaming, describing, deleting ----------------------------------

/// What a [`Form`] is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormFor {
    /// The sound of a track, to be saved.
    Save { track: TrackId },
    /// What the device type `uri` has saved under `name`, to be given
    /// another name, description or tags.
    Edit { uri: String, name: String },
}

/// A sound's name and what it says of itself, as they are typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub what: FormFor,
    pub name: String,
    pub description: String,
    /// The tags, with commas between.
    pub tags: String,
    /// The name's field has yet to be given the keyboard.
    pub fresh: bool,
}

/// Opens the form that saves the sound of `track`, whose first device is
/// `first`: under the name of the saved sound the device is on, if it is
/// on one, since that is the likely one to save over.
pub fn save_form(cx: &mut Cx, track: TrackId, first: &Device) {
    let on = first.preset.as_ref().and_then(|preset| {
        let saved = cx.snap.saved_presets.get(&first.uri)?;
        saved.iter().find(|saved| saved.id == preset.id)
    });
    cx.state.preset_form = Some(Form {
        what: FormFor::Save { track },
        name: on.map(|saved| saved.name.clone()).unwrap_or_default(),
        description: on.map(|saved| saved.description.clone()).unwrap_or_default(),
        tags: on.map(|saved| saved.tags.join(", ")).unwrap_or_default(),
        fresh: true,
    });
}

/// Opens the form on what the device type `uri` has saved under `name`.
pub fn edit_form(cx: &mut Cx, uri: &str, name: &str) {
    let saved = cx.session.lock().saved_presets().list(uri);
    let Some(saved) = saved.into_iter().find(|saved| saved.name == name) else { return };
    cx.state.preset_form = Some(Form {
        what: FormFor::Edit { uri: uri.to_owned(), name: saved.name.clone() },
        name: saved.name,
        description: saved.description,
        tags: saved.tags.join(", "),
        fresh: true,
    });
}

/// The tags typed into a form, one by one.
fn tags_of(typed: &str) -> Vec<String> {
    typed.split(',').map(|tag| tag.trim().to_owned()).filter(|tag| !tag.is_empty()).collect()
}

/// The form, and the question before something saved is deleted, while
/// either is up. Call once the views have run.
pub fn modals(ctx: &egui::Context, cx: &mut Cx) {
    form(ctx, cx);
    delete_question(ctx, cx);
}

fn form(ctx: &egui::Context, cx: &mut Cx) {
    let Some(mut form) = cx.state.preset_form.take() else { return };
    let theme = cx.theme;
    let (title, proceed) = match &form.what {
        FormFor::Save { .. } => ("Save this track\u{2019}s sound", "Save"),
        FormFor::Edit { .. } => ("Rename or describe what was saved", "Keep"),
    };
    let mut choice: Option<bool> = None;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(14, 12));
    let modal =
        egui::Modal::new(Id::unique("preset-form")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
            let (heading, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            text::paint(ui.painter(), heading, Align2::LEFT_CENTER, theme.text.caps, title, theme.ink_dim);
            ui.add_space(6.0);
            let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            let fields: [(&str, &str, &mut String); 3] = [
                ("Name", "What to call it", &mut form.name),
                ("Description", "What it sounds like and what it is for, in a sentence", &mut form.description),
                ("Tags", "Words to find it by, with commas between: bass, warm, house", &mut form.tags),
            ];
            for (index, (label, hint, buffer)) in fields.into_iter().enumerate() {
                ui.add_space(10.0);
                let (caption, response) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
                response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("Sound {label}")));
                let caps = label.to_uppercase();
                text::paint(ui.painter(), caption, Align2::LEFT_CENTER, theme.text.micro, &caps, theme.ink_faint);
                ui.add_space(3.0);
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                let field = trill_noir::text_field(ui, theme, rect, Id::unique(("preset-form", index)), buffer, hint);
                if index == 0 && form.fresh {
                    field.request_focus();
                }
            }
            form.fresh = false;
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if trill_noir::button(ui, theme, proceed, Variant::Primary).clicked() {
                    choice = Some(true);
                }
                if trill_noir::button(ui, theme, "Cancel", Variant::Default).clicked() {
                    choice = Some(false);
                }
            });
        });
    // Enter in any of the fields is the button.
    if choice.is_none() && ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
        choice = Some(true);
    }
    if modal.should_close() {
        choice.get_or_insert(false);
    }
    match choice {
        None => cx.state.preset_form = Some(form),
        Some(false) => {}
        // What cannot be done is said, and the form stays for another try,
        // with the keyboard back in the name: Enter took it out of its field.
        Some(true) => {
            if !carry_out(cx, &form) {
                form.fresh = true;
                cx.state.preset_form = Some(form);
            }
        }
    }
}

/// Does what a form that was confirmed asks, and says whether it was done.
fn carry_out(cx: &mut Cx, form: &Form) -> bool {
    let tags = tags_of(&form.tags);
    match &form.what {
        FormFor::Save { track } => {
            let track = *track;
            let kept = cx.try_session(|session| session.save_sound(track, &form.name, &form.description, &tags));
            let Some(kept) = kept else { return false };
            let what = if kept.saved.replaced { "Replaced" } else { "Saved" };
            let made_of = kept.devices.join(", ");
            cx.state.toasts.info(format!("{what} the sound \u{201C}{}\u{201D}: {made_of}", kept.saved.info.name));
            // What a sound does not hold, where the track has any of it.
            for left_out in kept.left_out {
                cx.state.toasts.info(left_out);
            }
            true
        }
        FormFor::Edit { uri, name } => {
            let change = trill_engine::PresetChange {
                name: Some(form.name.clone()),
                description: Some(form.description.clone()),
                tags: Some(tags),
            };
            let changed = cx.try_session(|session| session.change_saved_preset(uri, name, &change));
            let Some(changed) = changed else { return false };
            let said = match changed.info.name == *name {
                true => format!("Kept what \u{201C}{name}\u{201D} says of itself"),
                false => format!("Renamed \u{201C}{name}\u{201D} to \u{201C}{}\u{201D}", changed.info.name),
            };
            cx.state.toasts.info(said);
            true
        }
    }
}

fn delete_question(ctx: &egui::Context, cx: &mut Cx) {
    let Some((uri, name)) = cx.state.deleting_preset.clone() else { return };
    let whose = cx
        .descriptors
        .iter()
        .find(|descriptor| descriptor.uri == uri)
        .map_or("its device", |found| found.name.as_str());
    let body = format!(
        "\u{201C}{name}\u{201D}, which was saved for {whose}, will be gone for good, from every song\u{2019}s list. \
         Devices that were given it keep the sound they have."
    );
    let title = "Delete what was saved?";
    let Some(proceed) = widgets::question(ctx, cx.theme, "delete-preset", title, &body, "Delete", "Cancel") else {
        return;
    };
    cx.state.deleting_preset = None;
    if proceed {
        match cx.with_session(|session| session.delete_saved_preset(&uri, &name)) {
            true => cx.state.toasts.info(format!("Deleted \u{201C}{name}\u{201D}")),
            false => cx.state.toasts.error(format!("\u{201C}{name}\u{201D} was no longer there to delete")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_typed_with_commas_between() {
        assert_eq!(tags_of(" bass, warm ,, deep house ,"), ["bass", "warm", "deep house"]);
        assert!(tags_of("  ").is_empty());
    }

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
            kind: "Pad".into(),
        };
        assert_eq!(sound(&[]).made_of(), "Wavetable alone");
        assert_eq!(sound(&["Reverb"]).made_of(), "Wavetable, then Reverb");
        assert_eq!(sound(&["Chorus", "Reverb"]).made_of(), "Wavetable, then Chorus and Reverb");
        assert_eq!(sound(&["Amp", "Delay", "Reverb"]).made_of(), "Wavetable, then Amp, Delay and Reverb");
    }
}
