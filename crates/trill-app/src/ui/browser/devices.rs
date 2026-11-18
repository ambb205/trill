//! The browser's Devices tab: every device that can be added, and the
//! presets of the device selected in the detail panel's Devices tab.
//!
//! Double-clicking a device adds it to the chain the Devices tab shows.
//! Clicking a preset applies it to the selected device.
//!
//! After the built-in devices come the sounds: an instrument and the
//! effects after it, saved from a track or brought by a built-in
//! instrument. The ones that were saved are listed; the ones a built-in
//! instrument brings are a row for each kind (Bass, Pad) until the kind
//! is opened, since there are eighty of them. A search lists whatever
//! matches. What was saved is renamed, described and deleted from the
//! menu of its row (a right-click). Double-clicking one makes it the sound of the
//! track that is shown; dragging it to a track does the same there, and
//! below the tracks makes a track of its own.
//!
//! The library's sampled instruments are listed with the instruments: one
//! row for each, whatever number of recordings it is made of. One is
//! dragged onto a track, or double-clicked, as a device is.

use super::{EDGE, ROW, SEARCH_BLOCK, SEARCH_HEIGHT};
use crate::ui::sound_presets::{self, Listed, Target};
use crate::ui::sounds::{DropTarget, InstrumentRow};
use crate::ui::state::Cx;
use crate::ui::{arrangement, devices, dragging, widgets};
use eframe::egui::{self, Align2, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use trill_core::ChainRef;
use trill_core::device::{BUILTIN_SCHEME, DeviceCategory, DeviceDescriptor, split_uri};
use trill_noir::{MenuItem, paint, text};

const SECTION: f32 = 27.0;

/// Which list a device belongs in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Shelf {
    Instruments,
    Effects,
    /// Third-party devices of either kind.
    Plugins,
}

impl Shelf {
    const ALL: [(Shelf, &str); 3] =
        [(Shelf::Instruments, "Instruments"), (Shelf::Effects, "Effects"), (Shelf::Plugins, "Plugins")];

    fn of(descriptor: &DeviceDescriptor) -> Self {
        match (plugin_format(&descriptor.uri), descriptor.category) {
            (Some(_), _) => Shelf::Plugins,
            (None, DeviceCategory::Instrument) => Shelf::Instruments,
            (None, DeviceCategory::Effect) => Shelf::Effects,
        }
    }
}

/// The format of a third-party device, from its URI scheme: `"CLAP"` for
/// `clap:com.example.synth`. `None` for built-in devices.
fn plugin_format(uri: &str) -> Option<String> {
    split_uri(uri).filter(|(scheme, _)| *scheme != BUILTIN_SCHEME).map(|(scheme, _)| scheme.to_uppercase())
}

/// True if `name` matches what is typed in the search field.
fn matches(name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || name.to_lowercase().contains(&query)
}

/// The devices of one shelf that match `query`.
///
/// Effects and plugins are listed by name, since that is how they are
/// looked for. The built-in instruments are few and keep the catalog's
/// order, which leads with the one new tracks are given.
fn shelf<'a>(descriptors: &'a [DeviceDescriptor], which: Shelf, query: &str) -> Vec<&'a DeviceDescriptor> {
    let mut found: Vec<&DeviceDescriptor> = descriptors
        .iter()
        .filter(|descriptor| Shelf::of(descriptor) == which && matches(&descriptor.name, query))
        .collect();
    if which != Shelf::Instruments {
        found.sort_by_key(|descriptor| descriptor.name.to_lowercase());
    }
    found
}

/// The tab, in `area`: the browser below its row of tabs.
pub(super) fn show(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    let theme = cx.theme;
    let search = Rect::from_min_size(
        pos2(area.left() + EDGE, area.top() + 8.0),
        vec2(area.width() - 2.0 * EDGE - 1.0, SEARCH_HEIGHT),
    );
    let id = ui.scope_id().with("search");
    let response =
        trill_noir::text_field(ui, theme, search, id, &mut cx.state.browser.search, "Search devices and plugins");
    cx.hint(&response, "Type to narrow the lists below");

    let lists =
        Rect::from_min_max(pos2(area.left(), area.top() + SEARCH_BLOCK), pos2(area.right() - 1.0, area.bottom()));
    widgets::place(ui, lists, "lists", egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_clip_rect(lists.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            device_lists(ui, cx);
            preset_list(ui, cx);
            ui.add_space(8.0);
        });
    });
}

/// A caps heading with a rule under it.
fn section(ui: &mut Ui, cx: &Cx, title: &str) {
    let theme = cx.theme;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), SECTION), Sense::hover());
    let label =
        Rect::from_min_max(pos2(rect.left() + EDGE, rect.top() + 8.0), pos2(rect.right() - EDGE, rect.bottom() - 3.0));
    text::paint(ui.painter(), label, Align2::LEFT_CENTER, theme.text.caps, title, theme.ink_dim);
    paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
    ui.add_space(2.0);
}

/// One row of a list. `selected` marks the current choice.
fn row(ui: &mut Ui, cx: &Cx, name: &str, badge: &str, selected: bool) -> egui::Response {
    row_sensing(ui, cx, name, badge, selected, Sense::click())
}

/// A row that is called `label` by those who do not see it, where its
/// name alone would be taken for something else.
fn row_named(ui: &mut Ui, cx: &Cx, name: &str, badge: &str, selected: bool, label: &str) -> egui::Response {
    let response = row_sensing(ui, cx, name, badge, selected, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, label));
    response
}

/// A row that can be dragged out of the list as well.
fn row_sensing(ui: &mut Ui, cx: &Cx, name: &str, badge: &str, selected: bool, sense: Sense) -> egui::Response {
    let theme = cx.theme;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 0.0, theme.selected);
        } else if response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        let color = if selected || response.hovered() { theme.ink } else { theme.ink_dim };
        let mut content = rect.shrink2(vec2(EDGE, 0.0));
        if !badge.is_empty() {
            // On a lit row the quietest ink would drop below legibility.
            let quiet = if selected || response.hovered() { theme.ink_dim } else { theme.ink_faint };
            let width = text::paint(painter, content, Align2::RIGHT_CENTER, theme.text.micro, badge, quiet);
            content.max.x -= width + 6.0;
        }
        text::paint(painter, content, Align2::LEFT_CENTER, theme.text.body, name, color);
    }
    response
}

/// The longest name of an instrument a sound's row says in full.
const SOUND_DEVICE: usize = 12;

/// What a sound's row says beside its name: its instrument, and how many
/// effects come with it.
fn sound_badge(sound: &Listed) -> String {
    let device = match sound.device.chars().count() > SOUND_DEVICE {
        true => format!("{}\u{2026}", sound.device.chars().take(SOUND_DEVICE - 1).collect::<String>().trim_end()),
        false => sound.device.clone(),
    };
    match sound.after.len() {
        0 => device.to_uppercase(),
        count => format!("{} +{count}", device.to_uppercase()),
    }
}

/// One sound: its name, and what it is made of. It can be carried off.
fn sound_row(ui: &mut Ui, cx: &Cx, sound: &Listed) -> egui::Response {
    let theme = cx.theme;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click_and_drag());
    let label = format!("Sound {}", sound.name);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let lit = response.hovered() || response.dragged();
        if lit {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        let mut content = rect.shrink2(vec2(EDGE, 0.0));
        let quiet = if lit { theme.ink_dim } else { theme.ink_faint };
        let badge = sound_badge(sound);
        let width = text::paint(painter, content, Align2::RIGHT_CENTER, theme.text.micro, &badge, quiet);
        content.max.x -= width + 6.0;
        let color = if lit { theme.ink } else { theme.ink_dim };
        text::paint_name(painter, content, Align2::LEFT_CENTER, theme.text.body, &sound.name, color);
    }
    response
}

/// The menu of something that was saved: another name and description
/// for it, or none of it any more.
fn saved_menu(ui: &mut Ui, cx: &mut Cx, uri: &str, name: &str) {
    let theme = cx.theme;
    ui.set_min_width(190.0);
    if MenuItem::new("Rename or describe\u{2026}").show(ui, theme).clicked() {
        sound_presets::edit_form(cx, uri, name);
    }
    if MenuItem::new("Delete\u{2026}").show(ui, theme).clicked() {
        cx.state.deleting_preset = Some((uri.to_owned(), name.to_owned()));
    }
}

/// One sound of the list, with what a click, a drag and a right-click on
/// it do.
fn sound_entry(ui: &mut Ui, cx: &mut Cx, sound: &Listed, target: Target, onto: &str) {
    let response = sound_row(ui, cx, sound);
    cx.hint(&response, format!("{} \u{00B7} {onto}, or drag it to a track", sound.made_of()));
    if response.drag_started() {
        sound_presets::start(cx, sound);
    }
    if widgets::activated(ui, &response) {
        sound_presets::give(cx, &sound.uri, &sound.id, &sound.name, target);
    }
    if sound.saved {
        response.context_menu(|ui| saved_menu(ui, cx, &sound.uri, &sound.name));
    }
    if !sound.description.is_empty() && cx.state.preset_drag.is_none() {
        response.on_hover_text(&sound.description);
    }
}

/// The sounds: an instrument and the effects after it.
fn sound_list(ui: &mut Ui, cx: &mut Cx) {
    let query = cx.state.browser.search.clone();
    let searching = !query.trim().is_empty();
    let found =
        |sound: &Listed| matches(&sound.name, &query) || matches(&sound.device, &query) || matches(&sound.kind, &query);
    let sounds: Vec<Listed> = sound_presets::listed(cx).into_iter().filter(found).collect();
    if sounds.is_empty() {
        return;
    }
    let target = match cx.state.shown_chain() {
        ChainRef::Track(track) => Target::Track(track),
        ChainRef::Master => Target::NewTrack,
    };
    let onto = match target {
        Target::Track(track) => {
            let name = cx.snap.track(track).map_or_else(|| "the track".to_owned(), |track| track.name.clone());
            format!("double-click to make it the sound of {name}")
        }
        Target::NewTrack => "double-click for a new track with it".to_owned(),
    };
    section(ui, cx, "Sounds");
    for sound in sounds.iter().filter(|sound| sound.saved) {
        sound_entry(ui, cx, sound, target, &onto);
    }
    // The ones a built-in instrument brings: kind by kind, in the order
    // the instrument has them.
    let mut kinds: Vec<&str> = Vec::new();
    for sound in sounds.iter().filter(|sound| !sound.saved) {
        if !kinds.contains(&sound.kind.as_str()) {
            kinds.push(&sound.kind);
        }
    }
    for kind in kinds {
        let of_kind: Vec<&Listed> = sounds.iter().filter(|sound| !sound.saved && sound.kind == kind).collect();
        let open = searching || cx.state.browser.open_kinds.iter().any(|open| open == kind);
        if !searching {
            let label = format!("Sounds of the kind {kind}");
            let response = row_named(ui, cx, kind, &of_kind.len().to_string(), open, &label);
            cx.hint(
                &response,
                format!(
                    "{} sounds of this kind \u{00B7} click to {}",
                    of_kind.len(),
                    if open { "close" } else { "open" }
                ),
            );
            if response.clicked() {
                match open {
                    true => cx.state.browser.open_kinds.retain(|known| known != kind),
                    false => cx.state.browser.open_kinds.push(kind.to_owned()),
                }
            }
        }
        if open {
            for sound in of_kind {
                sound_entry(ui, cx, sound, target, &onto);
            }
        }
    }
}

/// Instruments, effects and plugins.
fn device_lists(ui: &mut Ui, cx: &mut Cx) {
    let chain = cx.state.shown_chain();
    let devices = cx.snap.project.chain(chain).map(Vec::as_slice).unwrap_or_default();
    let selected_uri = cx.state.selected_device.and_then(|id| devices.iter().find(|device| device.id == id));
    let selected_uri = selected_uri.map(|device| device.uri.clone());
    let target = match chain {
        ChainRef::Track(track) => {
            cx.snap.track(track).map_or_else(|| "the track".to_owned(), |track| track.name.clone())
        }
        ChainRef::Master => "the master".to_owned(),
    };
    let descriptors = cx.descriptors;
    let query = cx.state.browser.search.clone();

    for (which, title) in Shelf::ALL {
        // The sounds follow what is built in, ahead of the plugins: those
        // may be a long list.
        if which == Shelf::Plugins {
            sound_list(ui, cx);
        }
        let found = shelf(descriptors, which, &query);
        if found.is_empty() {
            continue;
        }
        section(ui, cx, title);
        for descriptor in found {
            let badge = plugin_format(&descriptor.uri).unwrap_or_default();
            let selected = selected_uri.as_deref() == Some(descriptor.uri.as_str());
            let response = row(ui, cx, &descriptor.name, &badge, selected);
            let instrument = descriptor.category == DeviceCategory::Instrument;
            let action = match (chain, instrument) {
                // The master carries no notes, so an instrument gets a
                // track of its own.
                (ChainRef::Master, true) => format!("Double-click for a new track with {}", descriptor.name),
                (_, true) => format!("Double-click to make {} the instrument of {target}", descriptor.name),
                (_, false) => format!("Double-click to add {} to {target}", descriptor.name),
            };
            cx.hint(&response, action);
            if widgets::activated(ui, &response) {
                match (chain, instrument) {
                    (ChainRef::Master, true) => arrangement::add_track_with(cx, &descriptor.uri),
                    _ => devices::add_device(cx, chain, descriptor),
                }
            }
            if !descriptor.description.is_empty() {
                response.on_hover_text(&descriptor.description);
            }
        }
        if which == Shelf::Instruments {
            sampled_instruments(ui, cx, chain, &target, &query);
        }
    }
}

/// The library's sampled instruments, under the instruments: each a row
/// that is dragged to a track or double-clicked.
fn sampled_instruments(ui: &mut Ui, cx: &mut Cx, chain: ChainRef, target: &str, query: &str) {
    let all = cx.sounds.instruments();
    let found: Vec<&InstrumentRow> = all.iter().filter(|row| matches(&row.name, query)).collect();
    if found.is_empty() {
        return;
    }
    section(ui, cx, "Sampled instruments");
    // The one the selected device plays, if it plays one.
    let playing = cx.state.selected_device.and_then(|device| cx.snap.instruments.get(&device));
    // (Spelled as the library spells its files: a song may name the
    // instrument from where the program was started.)
    let playing = playing.and_then(|shown| shown.said.as_ref()).map(|said| std::path::PathBuf::from(&said.file));
    let playing = playing.map(|file| std::path::absolute(&file).unwrap_or(file));
    for instrument in found {
        let selected = playing.as_deref() == Some(instrument.path.as_path());
        let response = row_sensing(ui, cx, &instrument.name, &instrument.size, selected, Sense::click_and_drag());
        let action = match chain {
            ChainRef::Master => format!("Double-click for a new track that plays {}", instrument.name),
            ChainRef::Track(_) => format!("Double-click to make {} the instrument of {target}", instrument.name),
        };
        cx.hint(&response, format!("{} \u{00B7} {action} \u{00B7} or drag it to a track", instrument.about));
        if response.drag_started() {
            dragging::start(cx, vec![instrument.path.clone()], None, None, trill_library::Tags::EMPTY);
        }
        if widgets::activated(ui, &response) {
            let place = match chain {
                ChainRef::Track(track) => DropTarget::TrackInstrument { track },
                ChainRef::Master => {
                    DropTarget::NewInstrument { color: dragging::new_track_colors(cx, 1).first().copied() }
                }
            };
            cx.sounds.drop_files(vec![instrument.path.clone()], place, instrument.name.clone(), 0.0);
        }
        response.on_hover_ui(|ui| {
            ui.set_max_width(420.0);
            ui.label(egui::RichText::new(&instrument.name).color(cx.theme.ink));
            ui.label(egui::RichText::new(&instrument.about).color(cx.theme.ink_dim));
            ui.label(egui::RichText::new(instrument.path.display().to_string()).color(cx.theme.ink_faint));
        });
    }
}

/// The presets of the selected device, the current one marked.
fn preset_list(ui: &mut Ui, cx: &mut Cx) {
    let chain = cx.state.shown_chain();
    let devices = cx.snap.project.chain(chain).map(Vec::as_slice).unwrap_or_default();
    let Some(device) = cx.state.selected_device.and_then(|id| devices.iter().find(|device| device.id == id)) else {
        return;
    };
    let Some(descriptor) = cx.descriptors.iter().find(|descriptor| descriptor.uri == device.uri) else { return };
    let query = cx.state.browser.search.clone();
    let presets: Vec<_> = descriptor.presets.iter().filter(|preset| matches(&preset.name, &query)).collect();
    let device_id = device.id;
    // The ones that were saved come first: they are the user's own.
    let saved: Vec<_> =
        devices::saved_presets(cx, device).iter().filter(|preset| matches(&preset.name, &query)).collect();
    if !saved.is_empty() {
        section(ui, cx, &format!("Saved presets \u{00B7} {}", descriptor.name));
        let current = devices::current_saved(cx, device);
        for preset in saved {
            let response = row(ui, cx, &preset.name, "", current == Some(preset.id.as_str()));
            cx.hint(
                &response,
                if preset.description.is_empty() {
                    format!("{}, as it was saved", preset.name)
                } else {
                    preset.description.clone()
                },
            );
            if response.clicked() {
                let id = preset.id.clone();
                cx.try_session(|session| session.apply_preset(device_id, &id));
            }
            let (uri, name) = (descriptor.uri.clone(), preset.name.clone());
            response.context_menu(|ui| saved_menu(ui, cx, &uri, &name));
        }
    }
    if presets.is_empty() {
        return;
    }

    section(ui, cx, &format!("Presets \u{00B7} {}", descriptor.name));
    let current = devices::current_own(cx, device, descriptor);
    for preset in presets {
        let response = row(ui, cx, &preset.name, "", current == Some(preset.name.as_str()));
        cx.hint(
            &response,
            if preset.description.is_empty() { preset.name.clone() } else { preset.description.clone() },
        );
        if response.clicked() {
            let name = preset.name.clone();
            cx.try_session(|session| session.apply_preset(device_id, &name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::device::DeviceFactory;
    use trill_dsp::BuiltinFactory;

    fn plugin(uri: &str, name: &str, category: DeviceCategory) -> DeviceDescriptor {
        DeviceDescriptor {
            uri: uri.to_owned(),
            name: name.to_owned(),
            vendor: "Example".to_owned(),
            category,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }
    }

    #[test]
    fn the_format_badge_comes_from_the_uri_scheme() {
        assert_eq!(plugin_format("clap:com.example.synth"), Some("CLAP".to_owned()));
        assert_eq!(plugin_format("vst3:ABCDEF"), Some("VST3".to_owned()));
        assert_eq!(plugin_format("builtin:reverb"), None);
        assert_eq!(plugin_format("no-scheme"), None);
    }

    #[test]
    fn devices_are_shelved_by_origin_then_kind_and_sorted() {
        let mut catalog = BuiltinFactory.descriptors();
        catalog.push(plugin("clap:b", "wave Forge", DeviceCategory::Effect));
        catalog.push(plugin("clap:a", "Surge XT", DeviceCategory::Instrument));

        let names = |which| shelf(&catalog, which, "").iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(Shelf::Instruments)[..2], ["Poly Synth", "Drum Kit"], "the catalog's order");
        let effects = names(Shelf::Effects);
        assert_eq!(effects.len(), 13);
        assert_eq!(&effects[..3], ["Auto Filter", "Chorus", "Compressor"]);
        assert_eq!(names(Shelf::Plugins), ["Surge XT", "wave Forge"], "plugins of both kinds, whatever their case");
    }

    #[test]
    fn the_search_narrows_every_shelf() {
        let mut catalog = BuiltinFactory.descriptors();
        catalog.push(plugin("clap:a", "Surge XT Effects", DeviceCategory::Effect));
        let names = |which, query| shelf(&catalog, which, query).iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(Shelf::Effects, " REV "), ["Reverb"]);
        assert_eq!(
            names(Shelf::Effects, "e"),
            ["Auto Filter", "Compressor", "Delay", "EQ", "Limiter", "Pitch Shifter", "Reverb", "Tape Stop", "Tremolo"]
        );
        assert_eq!(names(Shelf::Plugins, "eff"), ["Surge XT Effects"]);
        assert!(names(Shelf::Instruments, "zzz").is_empty());
        assert!(matches("Poly Synth", "") && matches("Poly Synth", "SYN") && !matches("Poly Synth", "drum"));
    }
}
