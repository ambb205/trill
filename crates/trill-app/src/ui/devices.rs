//! The Devices tab: the chain of the selected track (or of the master) as
//! a rack of panels, left to right in signal order.
//!
//! A built-in device is laid out by its entry in [`device_layouts`]: named
//! sections in columns, with small displays. The two that play audio files
//! also show them ([`sample_panels`]): the sampler in a column ahead of its
//! parameters, the drum rack as a grid of pads in place of them. Any other
//! device (a plugin) gets the generic layout: its parameters in declared
//! order, flowing into as many columns as they need.

use super::automation::{self, Hold};
use super::device_layouts::{self, Display, Format, Layout, Row};
use super::state::{Cx, TextEditState};
use super::units::{self, ParamScale};
use super::{colors, displays, editors, instrument_panel, sample_panels, wavetable_panel, widgets};
use eframe::egui::{self, Align2, Color32, FontId, Id, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use trill_core::device::{DeviceCategory, DeviceDescriptor, ParamKind};
use trill_core::presets::PresetInfo;
use trill_core::{AutomationTarget, ChainRef, Device, DeviceId, TrackId};
use trill_engine::ParamState;
use trill_noir::{Bar, Chip, Icon, MenuItem, paint, text};

/// Margin around the rack.
pub(super) const RACK_PADDING: f32 = 8.0;
/// Space between neighboring panels.
pub(super) const PANEL_GAP: f32 = 8.0;
pub(super) const PANEL_HEADER: f32 = 22.0;
/// Space between a panel's edge and its controls, left and right.
pub(super) const BODY_PADDING: f32 = 8.0;
pub(super) const BODY_TOP: f32 = 6.0;
const BODY_BOTTOM: f32 = 8.0;
pub(super) const COLUMN_GAP: f32 = 12.0;
/// Height of a section heading, its rule included.
pub(super) const SECTION_HEADER: f32 = 16.0;
/// Extra space above a section that is not the first of its column.
const SECTION_ABOVE: f32 = 4.0;
const DISPLAY_HEIGHT: f32 = 50.0;
/// Width of a column in the generic layout: plugin parameter names are
/// longer than the labels chosen for built-in devices.
const GENERIC_COLUMN: f32 = 176.0;
/// A choice among at most this many is shown as chips when they fit.
const CHIP_CHOICES: usize = 5;
/// A device with more parameters than this gets a box to filter them.
const FILTER_ABOVE: usize = 40;
/// What the tallest column of a built-in layout needs. A rack shorter than
/// this scrolls rather than cutting panels off.
pub(super) const MIN_BODY_HEIGHT: f32 = 245.0;
const MIN_PANEL_HEIGHT: f32 = PANEL_HEADER + BODY_TOP + MIN_BODY_HEIGHT + BODY_BOTTOM;
/// Narrowest a panel may be: room for a name, a preset and the remove
/// button.
const MIN_PANEL_WIDTH: f32 = 150.0 + 2.0 * BODY_PADDING;
/// Width of the panel that stands in for a device that failed to load.
const FAILED_PANEL_WIDTH: f32 = 260.0;
const LIMITER: &str = "builtin:limiter";

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    let snap = cx.snap;
    let chain = cx.state.shown_chain();
    let devices = snap.project.chain(chain).map(Vec::as_slice).unwrap_or_default();
    // Curves that belong to the signal take the track's hue.
    let accent = match chain {
        ChainRef::Track(track) => snap.track(track).map_or(theme.ink, |track| colors::track_color(theme, track.color)),
        ChainRef::Master => theme.ink,
    };
    let panel_height = (area.height() - 2.0 * RACK_PADDING).max(MIN_PANEL_HEIGHT);

    egui::ScrollArea::both().id_salt("rack").auto_shrink([false, false]).show(ui, |ui| {
        let widths: Vec<f32> = devices.iter().map(|device| panel_width(cx, device, panel_height)).collect();
        let panels: f32 = widths.iter().sum::<f32>() + PANEL_GAP * widths.len().saturating_sub(1) as f32;
        let content = vec2(panels.max(1.0) + 2.0 * RACK_PADDING, panel_height + 2.0 * RACK_PADDING);
        let (rack, _) = ui.allocate_exact_size(content, Sense::hover());

        let mut left = rack.left() + RACK_PADDING;
        for (device, width) in devices.iter().zip(widths) {
            let rect = Rect::from_min_size(pos2(left, rack.top() + RACK_PADDING), vec2(width, panel_height));
            if cx.state.reveal_device == Some(device.id) {
                // Asked into view. Once more on the next frame if the
                // device's parameters, which decide how wide the panels
                // are, have yet to arrive.
                ui.scroll_to_rect(rect, None);
                if snap.params.contains_key(&device.id) {
                    cx.state.reveal_device = None;
                }
            }
            if ui.is_rect_visible(rect) {
                panel(ui, cx, device, rect, accent);
            }
            left += width + PANEL_GAP;
        }
        if devices.is_empty() {
            let hint = match chain {
                ChainRef::Master => "No devices on the master. Double-click an effect in the browser to add one.",
                ChainRef::Track(_) => "No devices on this track. Double-click an instrument or effect in the browser.",
            };
            let room = Rect::from_min_size(area.min + vec2(RACK_PADDING + 6.0, RACK_PADDING), vec2(area.width(), 20.0));
            text::paint(ui.painter(), room, Align2::LEFT_CENTER, theme.text.micro, hint, theme.ink_faint);
        }
    });
}

/// The preset whose values the device currently has, if any.
pub fn current_preset<'a>(device: &Device, descriptor: &'a DeviceDescriptor) -> Option<&'a str> {
    descriptor
        .presets
        .iter()
        .find(|preset| {
            preset.params.len() == device.params.len()
                && preset.params.iter().all(|(id, value)| device.params.get(id) == Some(value))
        })
        .map(|preset| preset.name.as_str())
}

/// The presets that were saved for a device's type, by name.
pub fn saved_presets<'a>(cx: &Cx<'a>, device: &Device) -> &'a [PresetInfo] {
    cx.snap.saved_presets.get(&device.uri).map(Vec::as_slice).unwrap_or_default()
}

/// The id of the saved preset a device is on, if it is on one.
pub fn current_saved<'a>(cx: &Cx<'a>, device: &'a Device) -> Option<&'a str> {
    let reference = device.preset.as_ref()?;
    saved_presets(cx, device).iter().any(|preset| preset.id == reference.id).then_some(reference.id.as_str())
}

/// The track whose sound begins at `device`: the device is its
/// instrument, or its first device where it has no instrument. `None` for
/// any other device, and for a device of the master.
fn sound_begins_at(cx: &Cx, device: &Device) -> Option<TrackId> {
    let ChainRef::Track(track) = cx.snap.project.device_chain(device.id).ok()? else { return None };
    let devices = &cx.snap.track(track)?.devices;
    let instrument = |device: &&Device| {
        cx.descriptors.iter().any(|known| known.uri == device.uri && known.category == DeviceCategory::Instrument)
    };
    let first = devices.iter().find(instrument).or(devices.first())?;
    (first.id == device.id).then_some(track)
}

/// What the saved preset `preset` of the type of `device` brings behind
/// it, in words, if it is a sound that brings anything: `with Chorus and
/// Reverb`. Empty otherwise.
fn brought_by(cx: &Cx, device: &Device, preset: &PresetInfo) -> String {
    let sound = cx.snap.saved_sounds.iter().find(|sound| sound.uri == device.uri && sound.info.id == preset.id);
    match sound.map(|sound| sound.after.as_slice()) {
        None | Some([]) => String::new(),
        Some([one]) => format!("with {one}"),
        Some([first @ .., last]) => format!("with {} and {last}", first.join(", ")),
    }
}

/// What a device's preset menu shows for the preset it is on: the saved
/// preset it was given, one of its type's own if its values are that
/// preset's, or for a plugin the preset it was last given. An effect that
/// came with a sound is on no preset of its own: the sound is its
/// instrument's, and is named there.
fn shown_preset<'a>(cx: &Cx<'a>, device: &'a Device, descriptor: Option<&'a DeviceDescriptor>) -> Option<&'a str> {
    let given = device.preset.as_ref().filter(|preset| preset.bringer().is_none());
    let given = given.map(|preset| preset.name.as_str());
    // A saved preset was given by name, and says so whatever its values
    // are: a sound saved from a device on one of its type's own presets
    // has that preset's values.
    if current_saved(cx, device).is_some() {
        return given;
    }
    let own = descriptor.and_then(|descriptor| current_preset(device, descriptor));
    let built_in = descriptor.is_some_and(|descriptor| !descriptor.presets.is_empty());
    // A built-in device's own presets are told by its values alone.
    own.or(given.filter(|_| !built_in))
}

/// The preset of its type's own that a device is on, for marking it in a
/// list: none while it is on a saved one.
pub fn current_own<'a>(cx: &Cx<'_>, device: &Device, descriptor: &'a DeviceDescriptor) -> Option<&'a str> {
    match current_saved(cx, device) {
        Some(_) => None,
        None => current_preset(device, descriptor),
    }
}

/// Adds a device where it belongs in `chain`, as one undo step.
///
/// An instrument takes the place of the chain's current one (only the first
/// instrument receives notes, so a second would be silent) or goes to the
/// front; an effect goes to the end, but ahead of a final limiter.
pub fn add_device(cx: &mut Cx, chain: ChainRef, descriptor: &DeviceDescriptor) {
    let devices = cx.snap.project.chain(chain).map(Vec::as_slice).unwrap_or_default();
    let is_instrument = |device: &Device| {
        cx.descriptors.iter().any(|known| known.uri == device.uri && known.category == DeviceCategory::Instrument)
    };
    let (replaced, index) = match descriptor.category {
        DeviceCategory::Instrument => match devices.iter().position(is_instrument) {
            Some(index) => (Some(devices[index].id), Some(index)),
            None => (None, Some(0)),
        },
        DeviceCategory::Effect => {
            let before_limiter = devices.last().filter(|last| last.uri == LIMITER).map(|_| devices.len() - 1);
            (None, before_limiter)
        }
    };
    let uri = descriptor.uri.clone();
    let added = cx.try_session(|session| {
        session.begin_gesture();
        let removed = match replaced {
            Some(old) => session.edit(|project| project.remove_device(old).map(drop)),
            None => Ok(()),
        };
        // If the new device fails to load, the session rolls the gesture
        // back, which also restores the instrument that was removed.
        let added = removed.and_then(|()| session.add_device(chain, &uri, None, index));
        session.end_gesture();
        added
    });
    if let Some(device) = added {
        cx.state.selected_device = Some(device);
    }
}

// ---- panels ----------------------------------------------------------------

/// How many rows of the generic layout fit a body this tall.
fn generic_rows(body_height: f32, bar: f32, gap: f32) -> usize {
    (((body_height + gap) / (bar + gap)).floor() as usize).max(1)
}

/// How many columns `count` parameters need at `rows` per column, with one
/// slot taken by the filter box when there is one, and one by the track
/// the device listens to when it `listens`.
fn generic_columns(count: usize, rows: usize, listens: bool) -> usize {
    let slots = count + usize::from(count > FILTER_ABOVE) + usize::from(listens);
    slots.div_ceil(rows.max(1)).max(1)
}

fn layout_width(layout: &Layout) -> f32 {
    let columns: f32 = layout.columns.iter().map(|column| column.width).sum();
    columns + COLUMN_GAP * layout.columns.len().saturating_sub(1) as f32 + 2.0 * BODY_PADDING
}

fn panel_width(cx: &Cx, device: &Device, panel_height: f32) -> f32 {
    let width = match (device_layouts::layout_for(&device.uri), cx.snap.params.get(&device.id)) {
        _ if device.uri == sample_panels::DRUM_RACK => sample_panels::drum_rack_width() + 2.0 * BODY_PADDING,
        (Some(layout), _) if device.uri == sample_panels::SAMPLER => {
            layout_width(layout) + sample_panels::SAMPLE_COLUMN + COLUMN_GAP
        }
        (Some(layout), _) if device.uri == instrument_panel::SAMPLED => {
            layout_width(layout) + instrument_panel::INSTRUMENT_COLUMN + COLUMN_GAP
        }
        (Some(layout), _) => layout_width(layout),
        (None, Some(Ok(params))) => {
            let metric = cx.theme.metric;
            let body = panel_height - PANEL_HEADER - BODY_TOP - BODY_BOTTOM;
            let rows = generic_rows(body, metric.bar, metric.gap);
            let columns = generic_columns(params.len(), rows, cx.snap.can_listen(device.id)) as f32;
            columns * GENERIC_COLUMN + (columns - 1.0) * COLUMN_GAP + 2.0 * BODY_PADDING
        }
        (None, Some(Err(_))) => FAILED_PANEL_WIDTH,
        (None, None) => MIN_PANEL_WIDTH,
    };
    width.max(MIN_PANEL_WIDTH)
}

/// What the controls of one device share.
#[derive(Copy, Clone)]
pub(super) struct Body<'a> {
    pub device: &'a Device,
    pub params: &'a [ParamState],
    /// The hue of the track the device is on.
    pub accent: Color32,
    /// The device is bypassed: its controls step back but still work.
    pub dimmed: bool,
    pub id: Id,
}

impl Body<'_> {
    pub fn param(&self, id: &str) -> Option<&ParamState> {
        self.params.iter().find(|param| param.info.id == id)
    }

    fn value(&self, id: &str) -> f64 {
        self.param(id).map_or(0.0, |param| param.value)
    }
}

fn panel(ui: &mut Ui, cx: &mut Cx, device: &Device, rect: Rect, accent: Color32) {
    let theme = cx.theme;
    let id = ui.scope_id().with(("device", device.id));
    let selected = cx.state.selected_device == Some(device.id);

    // Working on a device selects it: a press anywhere in its panel, on a
    // control or between them.
    if ui.rect_contains_pointer(rect) && ui.input(|input| input.pointer.primary_pressed()) {
        cx.state.selected_device = Some(device.id);
    }
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme.panel);
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), PANEL_HEADER));
    if selected {
        painter.rect_filled(header, 0.0, theme.raised);
    }
    paint::hline_above(&painter, header.x_range(), header.bottom(), theme.hairline);
    paint::frame(&painter, rect, theme.hairline);
    panel_header(ui, cx, device, header, id, selected);

    let body = Rect::from_min_max(
        pos2(rect.left() + BODY_PADDING, header.bottom() + BODY_TOP),
        pos2(rect.right() - BODY_PADDING, rect.bottom() - BODY_BOTTOM),
    );
    match cx.snap.params.get(&device.id) {
        // Not there yet: its panel says so, in place of parameters it
        // has not got.
        _ if super::opening::of_device(cx.live, device.id).is_some() => {
            let words = super::opening::device_words(cx.live, device);
            let galley = painter.layout(words, FontId::proportional(12.0), theme.ink_dim, body.width().min(420.0));
            painter.galley(body.min, galley, theme.ink_dim);
        }
        // Not on this computer, and heard as someone else's recorded it:
        // its panel says so, in place of what went wrong with it.
        _ if super::renditions::of_device(&cx.snap.renditions, device.id).is_some() => {
            let words = super::renditions::device_words(&cx.snap.renditions, device, &cx.snap.project);
            let words = words.unwrap_or_default();
            let galley = painter.layout(words, FontId::proportional(12.0), theme.ink_dim, body.width().min(420.0));
            painter.galley(body.min, galley, theme.ink_dim);
        }
        Some(Ok(params)) => {
            let body_of = Body { device, params, accent, dimmed: !device.enabled, id };
            match device_layouts::layout_for(&device.uri) {
                _ if device.uri == sample_panels::DRUM_RACK => sample_panels::drum_rack(ui, cx, &body_of, body, rect),
                Some(layout) if device.uri == sample_panels::SAMPLER => {
                    // The file it plays, then its parameters.
                    let (sample, rest) = body.split_left_right_at_x(body.left() + sample_panels::SAMPLE_COLUMN);
                    sample_panels::sampler_sample(ui, cx, &body_of, sample, rect);
                    laid_out_body(ui, cx, &body_of, layout, rest.with_min_x(rest.left() + COLUMN_GAP));
                }
                Some(layout) if device.uri == instrument_panel::SAMPLED => {
                    // The instrument it plays, then its parameters.
                    let (shown, rest) = body.split_left_right_at_x(body.left() + instrument_panel::INSTRUMENT_COLUMN);
                    instrument_panel::instrument(ui, cx, &body_of, shown, rect);
                    laid_out_body(ui, cx, &body_of, layout, rest.with_min_x(rest.left() + COLUMN_GAP));
                }
                Some(layout) => laid_out_body(ui, cx, &body_of, layout, body),
                None => generic_body(ui, cx, &body_of, body),
            }
        }
        Some(Err(reason)) => {
            let galley = painter.layout(reason.clone(), FontId::proportional(12.0), theme.warn, body.width());
            painter.galley(body.min, galley, theme.warn);
        }
        // The snapshot does not cover this device yet (the selection just
        // changed); it will by the next frame.
        None => {}
    }
}

/// Bypass dot, name, the button for a plugin's own window, preset menu and
/// remove button.
fn panel_header(ui: &mut Ui, cx: &mut Cx, device: &Device, header: Rect, id: Id, selected: bool) {
    let theme = cx.theme;
    let device_id = device.id;
    let painter = ui.painter().clone();
    let quiet = theme.quiet_on(if selected { theme.raised } else { theme.panel });

    let dot_zone = Rect::from_min_size(header.min, vec2(BODY_PADDING + theme.metric.dot + 6.0, header.height()));
    let response = ui.interact(dot_zone, id.with("enable"), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, device.enabled, "Device on"));
    let dot = pos2(header.left() + BODY_PADDING, header.center().y);
    // A plugin that is still reading what it plays: its notes may not
    // sound yet.
    // One of a song that was just opened and is not made yet says that
    // instead.
    let coming = super::opening::of_device(cx.live, device_id);
    let loading = coming.is_none() && cx.live.loading.contains(&device_id);
    if device.enabled {
        let lit = if loading || coming.is_some() { theme.signal } else { theme.ok };
        paint::dot(&painter, dot, theme.metric.dot, if response.hovered() { theme.ink } else { lit });
    } else {
        paint::hollow_dot(&painter, dot, theme.metric.dot, if response.hovered() { theme.ink } else { quiet });
    }
    cx.hint(
        &response,
        if device.enabled { "On \u{00B7} click to bypass" } else { "Bypassed \u{00B7} click to turn on" },
    );
    if response.clicked() {
        let enabled = !device.enabled;
        cx.edit(|project| {
            project.device_mut(device_id)?.enabled = enabled;
            Ok(())
        });
    }

    let close = Rect::from_center_size(pos2(header.right() - 12.0, header.center().y), vec2(18.0, 18.0));
    let response = trill_noir::bare_icon_button(ui, theme, close, id.with("remove"), Icon::Close, "Remove device");
    cx.hint(&response, format!("Remove {} from the chain", device.name));
    if response.clicked() {
        cx.edit(|project| project.remove_device(device_id).map(drop));
    }

    let descriptor = cx.descriptors.iter().find(|descriptor| descriptor.uri == device.uri);
    let mut name_right = close.left() - 4.0;
    {
        // Every device has the menu: its type's own presets, the ones that
        // were saved, and saving the sound it has now.
        let own = descriptor.map(|descriptor| descriptor.presets.as_slice()).unwrap_or_default();
        let saved = saved_presets(cx, device);
        let current = descriptor.and_then(|descriptor| current_own(cx, device, descriptor));
        let current_saved = current_saved(cx, device);
        let shown = shown_preset(cx, device, descriptor).unwrap_or("Preset");
        let width = trill_noir::dropdown_width(ui, theme, shown).min(104.0);
        let rect = Rect::from_min_size(pos2(name_right - width, header.center().y - 8.0), vec2(width, 16.0));
        let response = trill_noir::dropdown(ui, theme, rect, id.with("preset"), "", shown, !device.enabled);
        let about = "Presets of this device, saving its sound as one, and loading one from a file";
        match device.preset.as_ref().filter(|preset| preset.bringer().is_some()) {
            // An effect a sound brought: it goes when the next one comes.
            Some(sound) => cx.hint(
                &response,
                format!(
                    "Came with the sound {}, and goes when the track is given another \u{00B7} {about}",
                    sound.name
                ),
            ),
            None => cx.hint(&response, about),
        }
        let (mut chosen, mut save, mut load, mut save_sound) = (None, false, false, false);
        let (mut renamed, mut doomed) = (None, None);
        // The track whose sound begins at this device, if one does: its
        // instrument, or its first device where it has none.
        let begins = sound_begins_at(cx, device);
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(150.0);
            if !saved.is_empty() {
                trill_noir::menu_heading(ui, theme, "Saved");
            }
            for preset in saved {
                let item = MenuItem::new(&preset.name).checked(current_saved == Some(preset.id.as_str()));
                // A sound says what it brings where it says nothing else.
                let brings = brought_by(cx, device, preset);
                let detail = if preset.description.is_empty() { brings.as_str() } else { preset.description.as_str() };
                if item.detail(detail).show(ui, theme).clicked() {
                    chosen = Some(preset.id.clone());
                }
            }
            if !saved.is_empty() && !own.is_empty() {
                trill_noir::menu_separator(ui, theme);
            }
            for preset in own {
                let item = MenuItem::new(&preset.name).checked(current == Some(preset.name.as_str()));
                if item.detail(&preset.description).show(ui, theme).clicked() {
                    chosen = Some(preset.name.clone());
                }
            }
            if !saved.is_empty() || !own.is_empty() {
                trill_noir::menu_separator(ui, theme);
            }
            let about = "Keep this sound under a name, for every song";
            save = MenuItem::new("Save preset\u{2026}").detail(about).show(ui, theme).clicked();
            if begins.is_some() {
                let about = "This device and the effects after it, as one thing";
                save_sound = MenuItem::new("Save sound\u{2026}").detail(about).show(ui, theme).clicked();
            }
            // The saved preset or sound it is on can be renamed, described
            // and deleted from here.
            if let Some(on) = current_saved.and_then(|id| saved.iter().find(|preset| preset.id == id)) {
                trill_noir::menu_separator(ui, theme);
                let label = format!("Rename or describe \u{201C}{}\u{201D}\u{2026}", on.name);
                if MenuItem::new(&label).show(ui, theme).clicked() {
                    renamed = Some(on.name.clone());
                }
                let label = format!("Delete \u{201C}{}\u{201D}\u{2026}", on.name);
                if MenuItem::new(&label).show(ui, theme).clicked() {
                    doomed = Some(on.name.clone());
                }
            }
            let about = "A preset that lies anywhere: one you bought, or were sent";
            load = MenuItem::new("Load preset file\u{2026}").detail(about).show(ui, theme).clicked();
        });
        if let Some(preset) = chosen {
            cx.try_session(|session| session.apply_preset(device_id, &preset));
        }
        if load {
            super::files::load_preset_file(&ui.ctx().clone(), cx, device_id);
        }
        if save {
            // The name of the saved preset it is on is the likely one to
            // save over; anything else starts empty.
            let text = current_saved.and(device.preset.as_ref()).map(|preset| preset.name.clone()).unwrap_or_default();
            cx.state.naming_preset = Some(TextEditState { target: device_id, text, focused: false });
        }
        if let (true, Some(track)) = (save_sound, begins) {
            // A sound says more of itself than a name: it has a form.
            super::sound_presets::save_form(cx, track, device);
        }
        if let Some(preset) = renamed {
            super::sound_presets::edit_form(cx, &device.uri, &preset);
        }
        if let Some(preset) = doomed {
            cx.state.deleting_preset = Some((device.uri.clone(), preset));
        }
        name_right = rect.left() - 6.0;
    }

    let mut name = Rect::from_min_max(pos2(dot_zone.right() + 2.0, header.top()), pos2(name_right, header.bottom()));
    if cx.state.naming_preset.as_ref().is_some_and(|edit| edit.target == device_id) {
        // The name of the preset is typed where the device's name is.
        let field = name.shrink2(vec2(0.0, 3.0));
        let hint = format!("Name this sound of {} and press Enter; Escape leaves it unsaved", device.name);
        cx.state.hint.set(hint);
        let naming = PresetName(device_id);
        let mut edit = cx.state.naming_preset.take().map(|edit| TextEditState {
            target: naming,
            text: edit.text,
            focused: edit.focused,
        });
        let committed = widgets::name_field(ui, theme, field, naming, "", &mut edit);
        cx.state.naming_preset =
            edit.map(|edit| TextEditState { target: device_id, text: edit.text, focused: edit.focused });
        if let Some(preset) = committed
            && let Some(saved) = cx.try_session(|session| session.save_preset(device_id, &preset, "", &[]))
        {
            let what = if saved.replaced { "Replaced" } else { "Saved" };
            cx.state.toasts.info(format!("{what} the preset \u{201C}{}\u{201D} of {}", saved.info.name, device.name));
        }
        return;
    }
    if let Some(coming) = coming {
        // Said ahead of the name, as what a plugin reads is.
        let word = coming.word();
        let width = text::width(&painter, theme.text.micro, word).ceil();
        let rect = Rect::from_min_size(name.min, vec2(width.min(name.width()), name.height()));
        let response = ui.interact(rect, id.with("coming"), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{word} {}", device.name)));
        cx.hint(&response, super::opening::device_words(cx.live, device));
        text::paint(&painter, rect, Align2::LEFT_CENTER, theme.text.micro, word, theme.signal);
        name.min.x = (rect.right() + 6.0).min(name.max.x);
    }
    if loading {
        // Said ahead of the name, where it is seen however narrow the
        // panel is.
        let width = text::width(&painter, theme.text.micro, LOADING).ceil();
        let rect = Rect::from_min_size(name.min, vec2(width.min(name.width()), name.height()));
        let response = ui.interact(rect, id.with("loading"), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{LOADING} {}", device.name)));
        cx.hint(&response, format!("{} is still loading what it plays: notes may not sound yet", device.name));
        text::paint(&painter, rect, Align2::LEFT_CENTER, theme.text.micro, LOADING, theme.signal);
        name.min.x = (rect.right() + 6.0).min(name.max.x);
    }
    if editors::has(cx, device_id) {
        // The plugin's own window. Its button follows the name, at the
        // panel's left: a plugin with hundreds of parameters makes a panel
        // many screens wide, and this is what its user came for.
        let label = format!("Editor of {}", device.name);
        let chip = Chip::new("Editor", editors::is_open(cx, device_id)).quiet(quiet).named(&label);
        let width = chip.width(ui, theme);
        let after_name = name.left() + text::width(&painter, theme.text.caps, &device.name) + 8.0;
        let left = after_name.min(name.right() - width).max(name.left());
        let rect = Rect::from_min_size(pos2(left, header.center().y - 8.0), vec2(width, 16.0));
        let response = trill_noir::chip(ui, theme, rect, id.with("editor"), &chip);
        cx.hint(&response, editors::hint(cx, device_id, &device.name));
        if response.clicked() {
            editors::toggle(cx, device_id);
        }
        name.max.x = rect.left() - 6.0;
    }
    let color = if device.enabled { theme.ink } else { quiet };
    text::paint(&painter, name, Align2::LEFT_CENTER, theme.text.caps, &device.name, color);
}

/// What a plugin's panel says while the plugin reads what it plays.
const LOADING: &str = "Loading";

/// What tells the field a preset is named in from the other name fields:
/// a device's id is a plain number, as a track's is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct PresetName(DeviceId);

// ---- built-in layouts ------------------------------------------------------

/// Height the rows take, the gaps between them included.
fn rows_height(rows: &[Row], bar: f32, gap: f32) -> f32 {
    let height: f32 = rows
        .iter()
        .map(|row| match row {
            // The chips, then room for the longest page.
            Row::Pages(pages) => {
                let longest = pages.iter().map(|page| rows_height(page.rows, bar, gap)).fold(0.0, f32::max);
                bar + gap + longest
            }
            Row::Shown(_) => DISPLAY_HEIGHT,
            _ => bar,
        })
        .sum();
    height + gap * rows.len().saturating_sub(1) as f32
}

fn laid_out_body(ui: &mut Ui, cx: &mut Cx, body: &Body, layout: &Layout, area: Rect) {
    let theme = cx.theme;
    let (bar, gap) = (theme.metric.bar, theme.metric.gap);
    let painter = ui.painter().clone();
    let mut left = area.left();
    for column in layout.columns {
        let mut top = area.top();
        for (index, section) in column.sections.iter().enumerate() {
            if !section.title.is_empty() {
                if index > 0 {
                    top += SECTION_ABOVE;
                }
                let heading = Rect::from_min_size(pos2(left, top), vec2(column.width, SECTION_HEADER));
                let label = heading.with_max_y(heading.bottom() - 2.0);
                text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.micro, section.title, theme.ink_faint);
                paint::hline_above(&painter, heading.x_range(), heading.bottom(), theme.hairline);
                top = heading.bottom() + 2.0 * gap;
            }
            if let Some(display) = section.display {
                // The equalizer's display takes whatever its controls leave.
                let height = match display {
                    Display::Eq => {
                        (area.bottom() - top - rows_height(section.rows, bar, gap) - gap).max(DISPLAY_HEIGHT)
                    }
                    _ => DISPLAY_HEIGHT,
                };
                let rect = Rect::from_min_size(pos2(left, top), vec2(column.width, height));
                show_display(ui, cx, body, display, rect);
                top = rect.bottom() + gap;
            }
            top = rows(ui, cx, body, section.rows, left, top, column.width);
        }
        left += column.width + COLUMN_GAP;
    }
}

/// Paints a display of the body's device into `rect`.
fn show_display(ui: &Ui, cx: &mut Cx, body: &Body, display: Display, rect: Rect) {
    match display {
        // The wave is the table's, which is more than parameters say.
        Display::Table { oscillator } => wavetable_panel::table(ui, cx, body, rect, oscillator),
        display => {
            let accent = if body.dimmed { trill_noir::fade(body.accent, cx.theme.disabled) } else { body.accent };
            displays::paint(ui.painter(), cx.theme, rect, display, accent, &|id| body.value(id));
        }
    }
}

/// A parameter that is one of many things, as a chip that opens the list
/// of them. Like a Bar it shows what its envelope says while one drives
/// it, and choosing takes hold of it.
fn choice_menu(ui: &Ui, cx: &mut Cx, body: &Body, param: &ParamState, label: &str, rect: Rect) {
    let theme = cx.theme;
    let info = &param.info;
    let ParamKind::Enum(choices) = &info.kind else { return };
    let id = body.id.with(("param", &info.id));
    let target = param_target(body, param);
    let hold = Hold::of(cx, &target);
    let value = hold.driven().unwrap_or(param.value);
    let current = value.round().max(0.0) as usize;
    let shown = units::format_param(info, value);
    let response = trill_noir::dropdown(ui, theme, rect, id, label, &shown, body.dimmed);
    cx.hint(&response, hold.hint(&info.name, "click to choose"));
    let mut chosen = None;
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(rect.width());
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (index, choice) in choices.iter().enumerate() {
                if MenuItem::new(choice).checked(index == current).show(ui, theme).clicked() {
                    chosen = Some(index as f64);
                }
            }
        });
    });
    if let Some(value) = chosen {
        automation::take_hold(cx, &target, hold);
        set_param(cx, body.device.id, param, value);
    }
    let name = if label.is_empty() { units::short_param_name(&info.name) } else { label };
    automation::mark(ui, cx, &target, hold, rect, id, name);
}

/// How a slot of a matrix divides its line: what it reads, what it moves,
/// and the bar for how much.
const SLOT_SOURCE: f32 = 96.0;
const SLOT_DEST: f32 = 130.0;

/// Places `rows` downward from `top`. Returns where the next row would go.
fn rows(ui: &mut Ui, cx: &mut Cx, body: &Body, rows: &[Row], left: f32, mut top: f32, width: f32) -> f32 {
    let theme = cx.theme;
    let (bar, gap) = (theme.metric.bar, theme.metric.gap);
    for row in rows {
        let rect = Rect::from_min_size(pos2(left, top), vec2(width, bar));
        match row {
            Row::Param { id, label, format } => {
                if let Some(param) = body.param(id) {
                    param_bar(ui, cx, body, param, label, *format, rect);
                }
            }
            Row::Choice { id, labels } => {
                if let Some(param) = body.param(id) {
                    hint_row(ui, cx, rect, &param.info.name);
                    let chips = body.id.with(("choice", *id));
                    choice_chips(ui, cx, body, param, labels, None, rect.min, width, chips);
                }
            }
            Row::Toggles(toggles) => {
                let mut x = rect.left();
                for (id, label) in *toggles {
                    let Some(param) = body.param(id) else { continue };
                    let width = Chip::new(label, false).width(ui, theme);
                    let chip_rect = Rect::from_min_size(pos2(x, top), vec2(width, theme.metric.chip));
                    toggle_chip(ui, cx, body, param, label, chip_rect, body.id.with(("toggle", *id)));
                    x = chip_rect.right() + gap - 1.0;
                }
            }
            Row::Picks { id, picks } => {
                if let Some(param) = body.param(id) {
                    hint_row(ui, cx, rect, &param.info.name);
                    let (labels, values): (Vec<&str>, Vec<f64>) = picks.iter().copied().unzip();
                    let chips = body.id.with(("picks", *id));
                    choice_chips(ui, cx, body, param, &labels, Some(&values), rect.min, width, chips);
                }
            }
            Row::Sidechain => sidechain_row(ui, cx, body, rect),
            Row::Menu { id, label } => {
                if let Some(param) = body.param(id) {
                    choice_menu(ui, cx, body, param, label, rect);
                }
            }
            Row::Shown(display) => {
                let rect = rect.with_max_y(rect.top() + DISPLAY_HEIGHT);
                show_display(ui, cx, body, *display, rect);
                top += DISPLAY_HEIGHT - bar;
            }
            Row::Slot { source, dest, amount } => {
                let from = rect.with_max_x(rect.left() + SLOT_SOURCE);
                let to = Rect::from_min_size(pos2(from.right() + gap, top), vec2(SLOT_DEST, bar));
                let by = rect.with_min_x(to.right() + gap);
                if let (Some(source), Some(dest), Some(amount)) =
                    (body.param(source), body.param(dest), body.param(amount))
                {
                    choice_menu(ui, cx, body, source, "", from);
                    choice_menu(ui, cx, body, dest, "", to);
                    // A slot that reads nothing or moves nothing does
                    // nothing: its amount steps back.
                    let idle = source.value < 0.5 || dest.value < 0.5;
                    let quiet = Body { dimmed: body.dimmed || idle, ..*body };
                    param_bar(ui, cx, &quiet, amount, "", Format::Auto, by);
                }
            }
            Row::Macro { id, index } => {
                if let Some(param) = body.param(id) {
                    let name = wavetable_panel::macro_name(body.device, *index);
                    param_bar(ui, cx, body, param, &name, Format::Auto, rect);
                }
            }
            Row::Pages(pages) => {
                // Each row of pages of a device keeps its own page.
                let first = pages.first().and_then(|page| page.rows.iter().flat_map(Row::param_ids).next());
                let memory = body.id.with(("page", first));
                // Opens on the first band that has been moved, so the page
                // shown is the one doing something.
                let moved = |page: &device_layouts::Page| {
                    let ids = page.rows.iter().flat_map(Row::param_ids);
                    ids.filter_map(|id| body.param(id)).any(|param| param.value != param.info.default)
                };
                let stored: Option<usize> = ui.data(|data| data.get_temp(memory));
                let current = stored.unwrap_or_else(|| pages.iter().position(moved).unwrap_or(0)).min(pages.len() - 1);
                let labels: Vec<&str> = pages.iter().map(|page| page.label).collect();
                let (clicked, _) =
                    trill_noir::chip_row(ui, theme, rect.min, width, memory, &labels, Some(current), body.dimmed);
                let current = clicked.unwrap_or(current);
                ui.data_mut(|data| data.insert_temp(memory, current));
                self::rows(ui, cx, body, pages[current].rows, left, top + bar + gap, width);
                top += rows_height(std::slice::from_ref(row), bar, gap) - bar;
            }
        }
        top += bar + gap;
    }
    top
}

/// The track a device listens to in place of its own input, and the tracks
/// to choose it from: every track but the device's own. One that cannot be
/// (it plays through, or listens to, this one) is refused in words when it
/// is chosen.
fn sidechain_row(ui: &Ui, cx: &mut Cx, body: &Body, rect: Rect) {
    let theme = cx.theme;
    let project = &cx.snap.project;
    let own = match project.device_chain(body.device.id) {
        Ok(ChainRef::Track(track)) => Some(track),
        _ => None,
    };
    let current = body.device.sidechain;
    let name = current.and_then(|track| project.track(track).ok()).map(|track| track.name.as_str());
    let shown = name.unwrap_or("Own input");
    let response = trill_noir::dropdown(ui, theme, rect, body.id.with("sidechain"), "Listen to", shown, body.dimmed);
    cx.hint(&response, "The track this device listens to, heard ahead of its fader and mute: a kick to duck a bass to");
    let mut chosen = None;
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(rect.width());
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            if MenuItem::new("Own input").checked(current.is_none()).show(ui, theme).clicked() {
                chosen = Some(None);
            }
            for track in project.tracks.iter().filter(|track| Some(track.id) != own) {
                if MenuItem::new(&track.name).checked(current == Some(track.id)).show(ui, theme).clicked() {
                    chosen = Some(Some(track.id));
                }
            }
        });
    });
    if let Some(track) = chosen.filter(|track| *track != current) {
        let device = body.device.id;
        cx.try_session(|session| session.set_sidechain(device, track));
    }
}

/// Shows a parameter's full name while the pointer is over a row of chips,
/// which have no room for it.
fn hint_row(ui: &Ui, cx: &mut Cx, rect: Rect, name: &str) {
    if ui.rect_contains_pointer(rect) {
        cx.state.hint.set(units::short_param_name(name));
    }
}

fn set_param(cx: &mut Cx, device: DeviceId, param: &ParamState, value: f64) {
    if value != param.value {
        let key = param.info.id.clone();
        cx.try_session(|session| session.set_device_params(device, &[(key, value)]));
    }
}

/// What automating `param` of the body's device is called in the project.
fn param_target(body: &Body, param: &ParamState) -> AutomationTarget {
    AutomationTarget::DeviceParam { device: body.device.id, param: param.info.id.clone() }
}

/// A numeric parameter as a Bar. While an envelope drives the parameter
/// the bar shows what the envelope says at the playhead and carries the
/// automation mark; dragging it takes hold of the parameter (see
/// [`automation::respond`]).
pub(super) fn param_bar(
    ui: &mut Ui,
    cx: &mut Cx,
    body: &Body,
    param: &ParamState,
    label: &str,
    format: Format,
    rect: Rect,
) {
    let info = &param.info;
    let scale = ParamScale::of(info);
    let target = param_target(body, param);
    let hold = Hold::of(cx, &target);
    let value = hold.driven().unwrap_or(param.value);
    let word = |value: f64| match format {
        Format::NoteValue => device_layouts::note_value(value).map(str::to_owned),
        Format::NoteName => Some(trill_core::pitch::pitch_name(value.round().clamp(0.0, 127.0) as u8)),
        Format::Auto => None,
    };
    let shown = word(value).unwrap_or_else(|| units::format_param(info, value));
    let mut bar = Bar::new(label, &shown, scale.normalize(info, value) as f32)
        .default(scale.normalize(info, info.default) as f32)
        .dimmed(body.dimmed)
        .automation(hold.mark());
    // A range that straddles zero fills from its zero point outward.
    if info.min < 0.0 && info.max > 0.0 {
        bar = bar.bipolar(scale.normalize(info, 0.0) as f32);
    }
    let output = trill_noir::bar(ui, cx.theme, rect, body.id.with(("param", &info.id)), &bar);
    let what = format!("{} \u{00B7} {shown}", units::short_param_name(&info.name));
    let gestures = "drag to set, Ctrl-drag for fine, right-click to automate";
    if let Some(position) = automation::respond(cx, &target, hold, &output, &what, gestures) {
        let value = if output.reset { info.default } else { scale.denormalize(info, position as f64) };
        set_param(cx, body.device.id, param, value);
    }
}

/// A parameter that is one of a few things, as the chips to choose it by:
/// the chip of the value it has is on. Like a Bar it shows what its
/// envelope says while one drives it, and choosing takes hold of it.
#[expect(clippy::too_many_arguments, reason = "a placement call: which parameter, its chips and where they go")]
fn choice_chips(
    ui: &Ui,
    cx: &mut Cx,
    body: &Body,
    param: &ParamState,
    labels: &[&str],
    values: Option<&[f64]>,
    origin: egui::Pos2,
    width: f32,
    id: Id,
) {
    let theme = cx.theme;
    let target = param_target(body, param);
    let hold = Hold::of(cx, &target);
    let shown = hold.driven().unwrap_or(param.value);
    // The chips stand for these values, or for 0, 1, 2 and so on.
    let value_of = |index: usize| values.map_or(index as f64, |values| values[index]);
    let current = match values {
        Some(values) => values.iter().position(|value| (value - shown).abs() < 1e-6),
        None => Some(shown.round().max(0.0) as usize),
    };
    let (clicked, _) = trill_noir::chip_row(ui, theme, origin, width, id, labels, current, body.dimmed);
    if let Some(index) = clicked {
        automation::take_hold(cx, &target, hold);
        set_param(cx, body.device.id, param, value_of(index));
    }
    // The row is the control: its corner carries the mark.
    let chips = Rect::from_min_size(origin, vec2(trill_noir::chip_row_width(ui, theme, labels), theme.metric.chip));
    automation::mark(ui, cx, &target, hold, chips, id, units::short_param_name(&param.info.name));
}

/// A switch as a chip, with what its automation has to say; see
/// [`choice_chips`].
fn toggle_chip(ui: &Ui, cx: &mut Cx, body: &Body, param: &ParamState, label: &str, rect: Rect, id: Id) {
    let target = param_target(body, param);
    let hold = Hold::of(cx, &target);
    let on = hold.driven().unwrap_or(param.value) >= 0.5;
    let chip = Chip::new(label, on).dimmed(body.dimmed);
    let response = trill_noir::chip(ui, cx.theme, rect, id, &chip);
    let name = units::short_param_name(&param.info.name);
    cx.hint(&response, hold.hint(name, "click to switch, right-click to automate"));
    if response.clicked() {
        automation::take_hold(cx, &target, hold);
        set_param(cx, body.device.id, param, if on { 0.0 } else { 1.0 });
    }
    response.context_menu(|ui| automation::menu_items(ui, cx, &target));
    automation::mark(ui, cx, &target, hold, rect, id, name);
}

// ---- the generic layout ----------------------------------------------------

/// Parameters in declared order, flowing down columns: bars for numbers, a
/// chip for a switch, a dropdown for a choice. Ahead of them the track the
/// device listens to, for one that can listen (a plugin that has an input
/// for it), and the filter box of one with many parameters.
fn generic_body(ui: &mut Ui, cx: &mut Cx, body: &Body, area: Rect) {
    let theme = cx.theme;
    let (bar, gap) = (theme.metric.bar, theme.metric.gap);
    let rows = generic_rows(area.height(), bar, gap);
    let slot = |index: usize| {
        let (column, row) = (index / rows, index % rows);
        let corner = area.min + vec2(column as f32 * (GENERIC_COLUMN + COLUMN_GAP), row as f32 * (bar + gap));
        Rect::from_min_size(corner, vec2(GENERIC_COLUMN, bar))
    };

    let mut next = 0;
    if cx.snap.can_listen(body.device.id) {
        sidechain_row(ui, cx, body, slot(next));
        next += 1;
    }
    let mut query = String::new();
    if body.params.len() > FILTER_ABOVE {
        let memory = body.id.with("filter");
        query = ui.data(|data| data.get_temp(memory)).unwrap_or_default();
        let hint = format!("Filter {} parameters", body.params.len());
        let response = trill_noir::text_field(ui, theme, slot(next), memory, &mut query, &hint);
        cx.hint(&response, "Type part of a parameter\u{2019}s name to show only the matches");
        ui.data_mut(|data| data.insert_temp(memory, query.clone()));
        next += 1;
    }
    let query = query.trim().to_lowercase();

    for param in body.params {
        let info = &param.info;
        if !query.is_empty() && !info.name.to_lowercase().contains(&query) {
            continue;
        }
        let rect = slot(next);
        next += 1;
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let label = units::short_param_name(&info.name);
        match &info.kind {
            ParamKind::Float | ParamKind::Int => param_bar(ui, cx, body, param, label, Format::Auto, rect),
            ParamKind::Bool => {
                let width = Chip::new(label, false).width(ui, theme).min(rect.width());
                let chip_rect = rect.with_max_x(rect.left() + width);
                toggle_chip(ui, cx, body, param, label, chip_rect, body.id.with(("param", &info.id)));
            }
            ParamKind::Enum(choices) => {
                let id = body.id.with(("param", &info.id));
                // A few short choices sit side by side after the name; more,
                // or longer ones, go behind a dropdown.
                let labels: Vec<&str> = choices.iter().map(String::as_str).collect();
                let label_width = text::width(ui.painter(), theme.text.label, label);
                let chips = Some(trill_noir::chip_row_width(ui, theme, &labels))
                    .filter(|width| choices.len() <= CHIP_CHOICES && label_width + 8.0 + width <= rect.width());
                if let Some(width) = chips {
                    let ink =
                        if body.dimmed { trill_noir::fade(theme.ink_faint, theme.disabled) } else { theme.ink_faint };
                    text::paint(ui.painter(), rect, Align2::LEFT_CENTER, theme.text.label, label, ink);
                    hint_row(ui, cx, rect, &info.name);
                    let origin = pos2(rect.right() - width, rect.top());
                    choice_chips(ui, cx, body, param, &labels, None, origin, width + 1.0, id);
                    continue;
                }
                choice_menu(ui, cx, body, param, label, rect);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::Project;
    use trill_core::device::DeviceFactory;
    use trill_dsp::BuiltinFactory;
    use trill_noir::Theme;

    #[test]
    fn generic_parameters_flow_into_as_many_columns_as_they_need() {
        assert_eq!(generic_rows(245.0, 18.0, 3.0), 11, "eleven bars and ten gaps are 228 px");
        assert_eq!(generic_rows(249.0, 18.0, 3.0), 12, "a twelfth bar needs 249");
        assert_eq!(generic_rows(5.0, 18.0, 3.0), 1, "a body too short for a row still has one");

        assert_eq!(generic_columns(0, 11, false), 1, "an empty panel still has a body");
        assert_eq!(generic_columns(11, 11, false), 1);
        assert_eq!(generic_columns(12, 11, false), 2);
        assert_eq!(generic_columns(40, 11, false), 4);
        assert_eq!(generic_columns(44, 11, false), 5, "above forty the filter box takes the first slot");
        assert_eq!(generic_columns(5, 0, false), 5);
        // The track a device listens to takes a slot too.
        assert_eq!(generic_columns(10, 11, true), 1);
        assert_eq!(generic_columns(11, 11, true), 2);
        assert_eq!(generic_columns(0, 11, true), 1);
        assert_eq!(generic_columns(43, 11, true), 5, "with the filter box, two slots");
    }

    #[test]
    fn every_built_in_layout_fits_the_shortest_rack() {
        let metric = Theme::noir().metric;
        for layout in device_layouts::LAYOUTS {
            for (index, column) in layout.columns.iter().enumerate() {
                let mut height = 0.0;
                for (position, section) in column.sections.iter().enumerate() {
                    if !section.title.is_empty() {
                        height += SECTION_HEADER + 2.0 * metric.gap + if position > 0 { SECTION_ABOVE } else { 0.0 };
                    }
                    if section.display.is_some() {
                        height += DISPLAY_HEIGHT + metric.gap;
                    }
                    height += rows_height(section.rows, metric.bar, metric.gap) + metric.gap;
                }
                // The last row needs no gap after it.
                height -= metric.gap;
                assert!(height <= MIN_BODY_HEIGHT, "{} column {index} is {height} px tall", layout.uri);
            }
        }
    }

    #[test]
    fn rows_of_chips_fit_their_column_on_one_line() {
        // A row is one bar tall; chips that wrapped would land on the row
        // below. Their width depends on the typeface, hence the context.
        let ctx = egui::Context::default();
        let theme = Theme::noir();
        trill_noir::install(&ctx, &theme);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for layout in device_layouts::LAYOUTS {
                for column in layout.columns {
                    // The rows of a column, those of its pages among them.
                    let mut rows: Vec<&Row> = column.sections.iter().flat_map(|section| section.rows).collect();
                    let mut next = 0;
                    while next < rows.len() {
                        if let Row::Pages(pages) = rows[next] {
                            rows.extend(pages.iter().flat_map(|page| page.rows));
                        }
                        next += 1;
                    }
                    for row in rows {
                        let labels: Vec<&str> = match row {
                            Row::Choice { labels, .. } => labels.to_vec(),
                            Row::Picks { picks, .. } => picks.iter().map(|(label, _)| *label).collect(),
                            Row::Pages(pages) => pages.iter().map(|page| page.label).collect(),
                            Row::Toggles(toggles) => toggles.iter().map(|(_, label)| *label).collect(),
                            Row::Param { .. } | Row::Sidechain | Row::Menu { .. } | Row::Shown(_) => continue,
                            Row::Slot { .. } | Row::Macro { .. } => continue,
                        };
                        let width = trill_noir::chip_row_width(ui, &theme, &labels);
                        assert!(width <= column.width, "{}: {labels:?} need {width} of {}", layout.uri, column.width);
                    }
                }
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn pages_reserve_room_for_their_longest_page() {
        let eq = device_layouts::layout_for("builtin:eq").unwrap();
        let rows = eq.columns[0].sections[0].rows;
        // The chips, then four bars: the cuts have a frequency and a slope each.
        assert_eq!(rows_height(rows, 18.0, 3.0), 18.0 + 3.0 + 4.0 * 18.0 + 3.0 * 3.0);
        let reverb = device_layouts::layout_for("builtin:reverb").unwrap();
        assert_eq!(rows_height(reverb.columns[0].sections[0].rows, 18.0, 3.0), 3.0 * 18.0 + 2.0 * 3.0);
        // Two columns and the gap between them.
        assert_eq!(layout_width(reverb), 2.0 * 150.0 + COLUMN_GAP + 2.0 * BODY_PADDING);
    }

    #[test]
    fn the_current_preset_is_recognized_by_its_values() {
        let descriptors = BuiltinFactory.descriptors();
        let synth = descriptors.iter().find(|descriptor| descriptor.uri == "builtin:polysynth").unwrap();
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, "builtin:polysynth", "Synth", None).unwrap();

        // A fresh device has no values set, which is what "init" is.
        assert_eq!(current_preset(project.device(id).unwrap(), synth), Some("init"));

        let pad = synth.presets.iter().find(|preset| preset.name == "warm_pad").unwrap();
        project.device_mut(id).unwrap().params = pad.params.iter().cloned().collect();
        assert_eq!(current_preset(project.device(id).unwrap(), synth), Some("warm_pad"));

        // Touch one parameter and it is no longer that preset.
        project.device_mut(id).unwrap().params.insert("cutoff".into(), 1234.0);
        assert_eq!(current_preset(project.device(id).unwrap(), synth), None);
    }
}
