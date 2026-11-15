//! The Devices tab: the selected track's device chain followed by the
//! master's, left to right in signal order. Each device is a card whose
//! controls are generated from the parameters it reports.

use super::state::Cx;
use super::theme;
use super::units::{self, ParamScale};
use super::widgets::{self, Icon};
use daw_core::device::{DeviceCategory, DeviceDescriptor, ParamKind};
use daw_core::{ChainRef, Device};
use daw_engine::ParamState;
use eframe::egui::{self, Align2, Color32, Rect, Sense, StrokeKind, Ui, pos2, vec2};

/// Margin around the strip of cards.
pub(super) const STRIP_MARGIN: f32 = 12.0;
/// Space between neighboring cards.
pub(super) const CARD_GAP: f32 = 10.0;
pub(super) const CHAIN_HEADER_WIDTH: f32 = 138.0;
pub(super) const CARD_HEADER: f32 = 36.0;
pub(super) const CARD_PADDING: f32 = 12.0;
pub(super) const ROW_HEIGHT: f32 = 24.0;
pub(super) const COLUMN_WIDTH: f32 = 266.0;
/// Space between parameter columns.
pub(super) const COLUMN_GAP: f32 = 18.0;
pub(super) const LABEL_WIDTH: f32 = 116.0;
pub(super) const VALUE_WIDTH: f32 = 62.0;
/// Narrowest a card may be: room for its name, a preset menu and the
/// remove button.
const MIN_CARD_WIDTH: f32 = 268.0;
const LIMITER: &str = "builtin:limiter";

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let area = ui.max_rect();
    // Cards fill the height of the tab, less the margins; the scroll bar
    // floats in the lower one.
    let card_height = (area.height() - STRIP_MARGIN * 2.0).max(ROW_HEIGHT * 3.0 + CARD_HEADER);
    let project = &cx.snap.project;
    let track = cx.state.selected_track.and_then(|id| cx.snap.track(id));

    egui::ScrollArea::horizontal().id_salt("device-chains").auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(STRIP_MARGIN);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            ui.add_space(STRIP_MARGIN);
            if let Some(track) = track {
                let color = theme::track_color(track.color);
                chain(ui, cx, ChainRef::Track(track.id), &track.name, color, &track.devices, card_height);
                let (divider, _) = ui.allocate_exact_size(vec2(9.0, card_height), Sense::hover());
                ui.painter().vline(divider.center().x, divider.y_range(), theme::hairline(theme::BORDER));
            }
            chain(ui, cx, ChainRef::Master, "Master", theme::ACCENT, &project.master.devices, card_height);
            // The last card is already followed by one gap.
            ui.add_space(STRIP_MARGIN - CARD_GAP);
        });
    });
}

/// One chain: a heading with the add menu, then a card per device.
fn chain(ui: &mut Ui, cx: &mut Cx, chain: ChainRef, name: &str, color: Color32, devices: &[Device], height: f32) {
    ui.allocate_ui_with_layout(vec2(CHAIN_HEADER_WIDTH, height), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(CHAIN_HEADER_WIDTH);
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.add_space(6.0);
        let kind = if chain == ChainRef::Master { "OUTPUT" } else { "TRACK" };
        ui.label(egui::RichText::new(kind).font(theme::strong(10.0)).color(theme::TEXT_FAINT));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 18.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, color);
            ui.add(egui::Label::new(egui::RichText::new(name).font(theme::strong(14.0))).truncate());
        });
        let count = match devices.len() {
            0 => "No devices".to_owned(),
            1 => "1 device".to_owned(),
            n => format!("{n} devices"),
        };
        ui.label(egui::RichText::new(count).font(theme::font(11.0)).color(theme::TEXT_DIM));
        ui.add_space(8.0);
        ui.menu_button("+  Add device", |ui| add_menu(ui, cx, chain, devices));
    });

    for device in devices {
        card(ui, cx, device, color, height);
    }
    if devices.is_empty() {
        let (rect, _) = ui.allocate_exact_size(vec2(MIN_CARD_WIDTH, height), Sense::hover());
        ui.painter().rect_stroke(rect, 6.0, theme::hairline(theme::BORDER), StrokeKind::Inside);
        let hint = if chain == ChainRef::Master {
            "Effects added here process the whole mix."
        } else {
            "An instrument here plays this track's MIDI clips. Effects shape its sound, audio clips included."
        };
        let galley = ui.painter().layout(hint.to_owned(), theme::font(12.0), theme::TEXT_FAINT, rect.width() - 40.0);
        let corner = rect.center() - galley.size() / 2.0;
        ui.painter().galley(corner, galley, theme::TEXT_FAINT);
    }
}

/// Every device that can be added, instruments first. The master only
/// takes effects.
fn add_menu(ui: &mut Ui, cx: &mut Cx, chain: ChainRef, devices: &[Device]) {
    ui.set_min_width(200.0);
    let sections = [("INSTRUMENTS", DeviceCategory::Instrument), ("EFFECTS", DeviceCategory::Effect)];
    let descriptors = cx.descriptors;
    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        for (title, category) in sections {
            if chain == ChainRef::Master && category == DeviceCategory::Instrument {
                continue;
            }
            let mut entries = descriptors.iter().filter(|descriptor| descriptor.category == category).peekable();
            if entries.peek().is_none() {
                continue;
            }
            ui.add_space(2.0);
            ui.label(egui::RichText::new(title).font(theme::strong(10.0)).color(theme::TEXT_FAINT));
            for descriptor in entries {
                if ui.button(&descriptor.name).on_hover_text(&descriptor.description).clicked() {
                    add_device(cx, chain, descriptor, devices);
                    ui.close();
                }
            }
            ui.add_space(4.0);
        }
    });
}

/// Adds a device where it belongs in the chain, as one undo step.
///
/// An instrument takes the place of the chain's current one (only the first
/// instrument receives notes, so a second would be silent) or goes to the
/// front; an effect goes to the end, but ahead of a final limiter.
fn add_device(cx: &mut Cx, chain: ChainRef, descriptor: &DeviceDescriptor, devices: &[Device]) {
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
    cx.try_session(|session| {
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
}

/// The preset whose values the device currently has, if any.
fn current_preset<'a>(device: &Device, descriptor: &'a DeviceDescriptor) -> Option<&'a str> {
    descriptor
        .presets
        .iter()
        .find(|preset| {
            preset.params.len() == device.params.len()
                && preset.params.iter().all(|(id, value)| device.params.get(id) == Some(value))
        })
        .map(|preset| preset.name.as_str())
}

/// How many columns `count` parameters need at `rows` per column.
fn column_count(count: usize, rows: usize) -> usize {
    count.div_ceil(rows.max(1)).max(1)
}

fn card(ui: &mut Ui, cx: &mut Cx, device: &Device, color: Color32, height: f32) {
    let params = cx.snap.params.get(&device.id);
    let rows = (((height - CARD_HEADER - CARD_PADDING * 2.0) / ROW_HEIGHT).floor() as usize).max(1);
    let columns = match params {
        Some(Ok(params)) => column_count(params.len(), rows),
        _ => 1,
    };
    let width =
        (columns as f32 * COLUMN_WIDTH + (columns - 1) as f32 * COLUMN_GAP + CARD_PADDING * 2.0).max(MIN_CARD_WIDTH);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter().clone();
    painter.rect_filled(rect, 6.0, theme::BG_RAISED);
    painter.rect_stroke(rect, 6.0, theme::hairline(theme::BORDER), StrokeKind::Inside);
    let header = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + CARD_HEADER));
    painter.hline(rect.x_range().shrink(1.0), header.bottom() - 0.5, theme::hairline(theme::BORDER));
    card_header(ui, cx, device, header);

    let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max).shrink(CARD_PADDING);
    match params {
        Some(Ok(params)) => {
            // A bypassed device keeps its controls, dimmed.
            let color = if device.enabled { color } else { theme::TEXT_FAINT };
            for (index, param) in params.iter().enumerate() {
                let (column, row) = (index / rows, index % rows);
                let cell = Rect::from_min_size(
                    pos2(
                        body.left() + column as f32 * (COLUMN_WIDTH + COLUMN_GAP),
                        body.top() + row as f32 * ROW_HEIGHT,
                    ),
                    vec2(COLUMN_WIDTH, ROW_HEIGHT),
                );
                param_row(ui, cx, device, param, cell, color);
            }
            for column in 1..columns {
                let x = body.left() + column as f32 * (COLUMN_WIDTH + COLUMN_GAP) - COLUMN_GAP / 2.0;
                painter.vline(x, body.y_range(), theme::hairline(theme::BORDER));
            }
        }
        Some(Err(reason)) => {
            let galley = painter.layout(reason.clone(), theme::font(12.0), theme::WARN, body.width());
            painter.galley(body.min, galley, theme::WARN);
        }
        // The snapshot does not cover this device yet (the selection just
        // changed); it will by the next frame.
        None => {}
    }
}

/// Bypass toggle, name, preset menu and remove button.
fn card_header(ui: &mut Ui, cx: &mut Cx, device: &Device, header: Rect) {
    let id = ui.scope_id().with(("device", device.id));
    let device_id = device.id;

    let power = Rect::from_center_size(header.left_center() + vec2(20.0, 0.0), vec2(24.0, 22.0));
    let lit = device.enabled.then_some(theme::PLAY);
    let hint = if device.enabled { "On. Click to bypass." } else { "Bypassed. Click to turn on." };
    if widgets::icon_button_at(ui, power, id.with("power"), Icon::Power, lit).on_hover_text(hint).clicked() {
        let enabled = !device.enabled;
        cx.edit(|project| {
            project.device_mut(device_id)?.enabled = enabled;
            Ok(())
        });
    }

    let close = Rect::from_center_size(header.right_center() - vec2(18.0, 0.0), vec2(22.0, 22.0));
    if widgets::icon_button_at(ui, close, id.with("remove"), Icon::Close, None).on_hover_text("Remove device").clicked()
    {
        cx.edit(|project| project.remove_device(device_id).map(drop));
    }

    let descriptor = cx.descriptors.iter().find(|descriptor| descriptor.uri == device.uri);
    let presets = descriptor.filter(|descriptor| !descriptor.presets.is_empty());
    let mut name_right = close.left() - 6.0;
    if let Some(descriptor) = presets {
        let menu = Rect::from_min_max(
            pos2(close.left() - 6.0 - 124.0, header.top()),
            pos2(close.left() - 6.0, header.bottom()),
        );
        name_right = menu.left() - 8.0;
        let mut chosen = None;
        let layout = egui::Layout::right_to_left(egui::Align::Center);
        widgets::place(ui, menu, ("preset", device_id), layout, |ui| {
            egui::ComboBox::from_id_salt("preset")
                .width(menu.width() - 8.0)
                .selected_text(current_preset(device, descriptor).unwrap_or("Preset"))
                .show_ui(ui, |ui| {
                    for preset in &descriptor.presets {
                        let selected = current_preset(device, descriptor) == Some(preset.name.as_str());
                        if ui.selectable_label(selected, &preset.name).on_hover_text(&preset.description).clicked() {
                            chosen = Some(preset.name.clone());
                        }
                    }
                });
        });
        if let Some(preset) = chosen {
            cx.try_session(|session| session.apply_preset(device_id, &preset));
        }
    }

    let name = Rect::from_min_max(pos2(power.right() + 6.0, header.top()), pos2(name_right, header.bottom()));
    let color = if device.enabled { theme::TEXT } else { theme::TEXT_FAINT };
    widgets::text_in(ui.painter(), name, Align2::LEFT_CENTER, &device.name, theme::strong(13.0), color);
}

/// One parameter: its name, a control fitting its kind, and its value.
fn param_row(ui: &mut Ui, cx: &mut Cx, device: &Device, param: &ParamState, cell: Rect, color: Color32) {
    let info = &param.info;
    let id = ui.scope_id().with(("param", device.id, &info.id));

    let label = Rect::from_min_size(cell.min, vec2(LABEL_WIDTH, cell.height()));
    let short = units::short_param_name(&info.name);
    widgets::text_in(ui.painter(), label, Align2::LEFT_CENTER, short, theme::font(12.0), theme::TEXT_DIM);
    // The full name, with the explanation the short one leaves out.
    ui.interact(label, id.with("label"), Sense::hover()).on_hover_text(&info.name);

    let control = Rect::from_min_max(pos2(label.right() + 8.0, cell.top()), cell.max);
    let new_value = match &info.kind {
        ParamKind::Bool => {
            let on = param.value >= 0.5;
            let switch = Rect::from_min_size(pos2(control.left(), control.center().y - 8.0), vec2(30.0, 16.0));
            widgets::switch_at(ui, switch, id, on, color).clicked().then_some(if on { 0.0 } else { 1.0 })
        }
        ParamKind::Enum(choices) => {
            let mut chosen = None;
            let current = param.value.round().max(0.0) as usize;
            let layout = egui::Layout::left_to_right(egui::Align::Center);
            widgets::place(ui, control, ("choice", device.id, &info.id), layout, |ui| {
                egui::ComboBox::from_id_salt("choice")
                    .width(control.width() - 8.0)
                    .selected_text(units::format_param(info, param.value))
                    .show_ui(ui, |ui| {
                        for (index, choice) in choices.iter().enumerate() {
                            if ui.selectable_label(index == current, choice).clicked() {
                                chosen = Some(index as f64);
                            }
                        }
                    });
            });
            chosen
        }
        ParamKind::Float | ParamKind::Int => {
            let value_text = Rect::from_min_max(pos2(control.right() - VALUE_WIDTH, cell.top()), control.max);
            let bar = Rect::from_min_max(
                pos2(control.left(), control.center().y - 8.0),
                pos2(value_text.left() - 8.0, control.center().y + 8.0),
            );
            let scale = ParamScale::of(info);
            // Bipolar parameters fill from their zero point outward.
            let origin = if info.min < 0.0 && info.max > 0.0 { scale.normalize(info, 0.0) } else { 0.0 };
            let drag = widgets::param_bar(
                ui,
                bar,
                id,
                scale.normalize(info, param.value),
                scale.normalize(info, info.default),
                origin,
                color,
            );
            if drag.response.drag_started() {
                cx.begin_gesture();
            }
            let new_value = drag.value.map(|position| scale.denormalize(info, position));
            widgets::text_in(
                ui.painter(),
                value_text,
                Align2::RIGHT_CENTER,
                &units::format_param(info, new_value.unwrap_or(param.value)),
                theme::font(11.0),
                theme::TEXT,
            );
            new_value
        }
    };

    if let Some(value) = new_value.filter(|value| *value != param.value) {
        let (device_id, key) = (device.id, info.id.clone());
        cx.try_session(|session| session.set_device_params(device_id, &[(key, value)]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::Project;
    use daw_core::device::DeviceFactory;
    use daw_dsp::BuiltinFactory;

    #[test]
    fn parameters_flow_into_as_many_columns_as_they_need() {
        assert_eq!(column_count(0, 9), 1, "an empty card still has a body");
        assert_eq!(column_count(9, 9), 1);
        assert_eq!(column_count(10, 9), 2);
        assert_eq!(column_count(32, 9), 4);
        assert_eq!(column_count(5, 0), 5, "a card too short for a row gets one per column");
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
