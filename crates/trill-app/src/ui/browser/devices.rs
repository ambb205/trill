//! The browser's Devices tab: every device that can be added, and the
//! presets of the device selected in the detail panel's Devices tab.
//!
//! Double-clicking a device adds it to the chain the Devices tab shows.
//! Clicking a preset applies it to the selected device.
//!
//! After the built-in devices come the sounds that are more than one
//! device: an instrument and the effects after it, saved from a track or
//! brought by a built-in instrument. Double-clicking one makes it the sound of the
//! track that is shown; dragging it to a track does the same there, and
//! below the tracks makes a track of its own.

use super::{EDGE, ROW, SEARCH_BLOCK, SEARCH_HEIGHT};
use crate::ui::sound_presets::{self, Listed, Target};
use crate::ui::state::Cx;
use crate::ui::{arrangement, devices, widgets};
use eframe::egui::{self, Align2, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use trill_core::ChainRef;
use trill_core::device::{BUILTIN_SCHEME, DeviceCategory, DeviceDescriptor, split_uri};
use trill_noir::{paint, text};

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
    let theme = cx.theme;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
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

/// The sounds that are an instrument and the effects after it.
fn sound_list(ui: &mut Ui, cx: &mut Cx) {
    let query = cx.state.browser.search.clone();
    let sounds: Vec<Listed> = sound_presets::listed(cx)
        .into_iter()
        .filter(|sound| matches(&sound.name, &query) || matches(&sound.device, &query))
        .collect();
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
    for sound in &sounds {
        let response = sound_row(ui, cx, sound);
        cx.hint(&response, format!("{} \u{00B7} {onto}, or drag it to a track", sound.made_of()));
        if response.drag_started() {
            sound_presets::start(cx, sound);
        }
        if widgets::activated(ui, &response) {
            sound_presets::give(cx, &sound.uri, &sound.id, &sound.name, target);
        }
        if !sound.description.is_empty() && cx.state.preset_drag.is_none() {
            response.on_hover_text(&sound.description);
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
        assert_eq!(effects.len(), 12);
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
            ["Auto Filter", "Compressor", "Delay", "EQ", "Limiter", "Pitch Shifter", "Reverb", "Tremolo"]
        );
        assert_eq!(names(Shelf::Plugins, "eff"), ["Surge XT Effects"]);
        assert!(names(Shelf::Instruments, "zzz").is_empty());
        assert!(matches("Poly Synth", "") && matches("Poly Synth", "SYN") && !matches("Poly Synth", "drum"));
    }
}
