//! The Devices tab: the chain of the selected track (or of the master) as
//! a rack of panels, left to right in signal order.
//!
//! A built-in device is laid out by its entry in [`device_layouts`]: named
//! sections in columns, with small displays. Any other device (a plugin)
//! gets the generic layout: its parameters in declared order, flowing into
//! as many columns as they need.

use super::device_layouts::{self, Display, Format, Layout, Row};
use super::state::Cx;
use super::units::{self, ParamScale};
use super::{colors, displays};
use eframe::egui::{self, Align2, Color32, FontId, Id, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use trill_core::device::{DeviceCategory, DeviceDescriptor, ParamKind};
use trill_core::{ChainRef, Device, DeviceId};
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
const COLUMN_GAP: f32 = 12.0;
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
const MIN_BODY_HEIGHT: f32 = 245.0;
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
/// slot taken by the filter box when there is one.
fn generic_columns(count: usize, rows: usize) -> usize {
    let slots = count + usize::from(count > FILTER_ABOVE);
    slots.div_ceil(rows.max(1)).max(1)
}

fn layout_width(layout: &Layout) -> f32 {
    let columns: f32 = layout.columns.iter().map(|column| column.width).sum();
    columns + COLUMN_GAP * layout.columns.len().saturating_sub(1) as f32 + 2.0 * BODY_PADDING
}

fn panel_width(cx: &Cx, device: &Device, panel_height: f32) -> f32 {
    let width = match (device_layouts::layout_for(&device.uri), cx.snap.params.get(&device.id)) {
        (Some(layout), _) => layout_width(layout),
        (None, Some(Ok(params))) => {
            let metric = cx.theme.metric;
            let body = panel_height - PANEL_HEADER - BODY_TOP - BODY_BOTTOM;
            let columns = generic_columns(params.len(), generic_rows(body, metric.bar, metric.gap)) as f32;
            columns * GENERIC_COLUMN + (columns - 1.0) * COLUMN_GAP + 2.0 * BODY_PADDING
        }
        (None, Some(Err(_))) => FAILED_PANEL_WIDTH,
        (None, None) => MIN_PANEL_WIDTH,
    };
    width.max(MIN_PANEL_WIDTH)
}

/// What the controls of one device share.
struct Body<'a> {
    device: &'a Device,
    params: &'a [ParamState],
    /// The hue of the track the device is on.
    accent: Color32,
    /// The device is bypassed: its controls step back but still work.
    dimmed: bool,
    id: Id,
}

impl Body<'_> {
    fn param(&self, id: &str) -> Option<&ParamState> {
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
        Some(Ok(params)) => {
            let body_of = Body { device, params, accent, dimmed: !device.enabled, id };
            match device_layouts::layout_for(&device.uri) {
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

/// Bypass dot, name, preset menu and remove button.
fn panel_header(ui: &mut Ui, cx: &mut Cx, device: &Device, header: Rect, id: Id, selected: bool) {
    let theme = cx.theme;
    let device_id = device.id;
    let painter = ui.painter().clone();
    let quiet = theme.quiet_on(if selected { theme.raised } else { theme.panel });

    let dot_zone = Rect::from_min_size(header.min, vec2(BODY_PADDING + theme.metric.dot + 6.0, header.height()));
    let response = ui.interact(dot_zone, id.with("enable"), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, device.enabled, "Device on"));
    let dot = pos2(header.left() + BODY_PADDING, header.center().y);
    if device.enabled {
        paint::dot(&painter, dot, theme.metric.dot, if response.hovered() { theme.ink } else { theme.ok });
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
    if let Some(descriptor) = descriptor.filter(|descriptor| !descriptor.presets.is_empty()) {
        let current = current_preset(device, descriptor);
        let shown = current.unwrap_or("Preset");
        let width = trill_noir::dropdown_width(ui, theme, shown).min(104.0);
        let rect = Rect::from_min_size(pos2(name_right - width, header.center().y - 8.0), vec2(width, 16.0));
        let response = trill_noir::dropdown(ui, theme, rect, id.with("preset"), "", shown, !device.enabled);
        cx.hint(&response, "Presets of this device");
        let mut chosen = None;
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(150.0);
            for preset in &descriptor.presets {
                let item = MenuItem::new(&preset.name).checked(current == Some(preset.name.as_str()));
                if item.detail(&preset.description).show(ui, theme).clicked() {
                    chosen = Some(preset.name.clone());
                }
            }
        });
        if let Some(preset) = chosen {
            cx.try_session(|session| session.apply_preset(device_id, &preset));
        }
        name_right = rect.left() - 6.0;
    }

    let name = Rect::from_min_max(pos2(dot_zone.right() + 2.0, header.top()), pos2(name_right, header.bottom()));
    let color = if device.enabled { theme.ink } else { quiet };
    text::paint(&painter, name, Align2::LEFT_CENTER, theme.text.caps, &device.name, color);
}

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
                let accent = if body.dimmed { trill_noir::fade(body.accent, theme.disabled) } else { body.accent };
                displays::paint(&painter, theme, rect, display, accent, &|id| body.value(id));
                top = rect.bottom() + gap;
            }
            top = rows(ui, cx, body, section.rows, left, top, column.width);
        }
        left += column.width + COLUMN_GAP;
    }
}

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
                    let current = param.value.round().max(0.0) as usize;
                    let chips = body.id.with(("choice", *id));
                    let (clicked, _) =
                        trill_noir::chip_row(ui, theme, rect.min, width, chips, labels, Some(current), body.dimmed);
                    hint_row(ui, cx, rect, &param.info.name);
                    if let Some(index) = clicked {
                        set_param(cx, body.device.id, param, index as f64);
                    }
                }
            }
            Row::Toggles(toggles) => {
                let mut x = rect.left();
                for (id, label) in *toggles {
                    let Some(param) = body.param(id) else { continue };
                    let on = param.value >= 0.5;
                    let chip = Chip::new(label, on).dimmed(body.dimmed);
                    let chip_rect = Rect::from_min_size(pos2(x, top), vec2(chip.width(ui, theme), theme.metric.chip));
                    let response = trill_noir::chip(ui, theme, chip_rect, body.id.with(("toggle", *id)), &chip);
                    cx.hint(&response, units::short_param_name(&param.info.name));
                    if response.clicked() {
                        set_param(cx, body.device.id, param, if on { 0.0 } else { 1.0 });
                    }
                    x = chip_rect.right() + gap - 1.0;
                }
            }
            Row::Picks { id, picks } => {
                if let Some(param) = body.param(id) {
                    let labels: Vec<&str> = picks.iter().map(|(label, _)| *label).collect();
                    let current = picks.iter().position(|(_, value)| (value - param.value).abs() < 1e-6);
                    let chips = body.id.with(("picks", *id));
                    let (clicked, _) =
                        trill_noir::chip_row(ui, theme, rect.min, width, chips, &labels, current, body.dimmed);
                    hint_row(ui, cx, rect, &param.info.name);
                    if let Some(index) = clicked {
                        set_param(cx, body.device.id, param, picks[index].1);
                    }
                }
            }
            Row::Pages(pages) => {
                let memory = body.id.with("page");
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

/// A numeric parameter as a Bar.
fn param_bar(ui: &mut Ui, cx: &mut Cx, body: &Body, param: &ParamState, label: &str, format: Format, rect: Rect) {
    let info = &param.info;
    let scale = ParamScale::of(info);
    let word = |value: f64| match format {
        Format::NoteValue => device_layouts::note_value(value).map(str::to_owned),
        Format::NoteName => Some(trill_core::pitch::pitch_name(value.round().clamp(0.0, 127.0) as u8)),
        Format::Auto => None,
    };
    let shown = word(param.value).unwrap_or_else(|| units::format_param(info, param.value));
    let mut bar = Bar::new(label, &shown, scale.normalize(info, param.value) as f32)
        .default(scale.normalize(info, info.default) as f32)
        .dimmed(body.dimmed);
    // A range that straddles zero fills from its zero point outward.
    if info.min < 0.0 && info.max > 0.0 {
        bar = bar.bipolar(scale.normalize(info, 0.0) as f32);
    }
    let output = trill_noir::bar(ui, cx.theme, rect, body.id.with(("param", &info.id)), &bar);
    cx.hint(
        &output.response,
        format!(
            "{} \u{00B7} {shown} \u{00B7} drag to set, Ctrl-drag for fine, double-click for the default",
            units::short_param_name(&info.name)
        ),
    );
    if output.pressed {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac {
        let value = if output.reset { info.default } else { scale.denormalize(info, position as f64) };
        set_param(cx, body.device.id, param, value);
    }
}

// ---- the generic layout ----------------------------------------------------

/// Parameters in declared order, flowing down columns: bars for numbers, a
/// chip for a switch, a dropdown for a choice.
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
    let mut query = String::new();
    if body.params.len() > FILTER_ABOVE {
        let memory = body.id.with("filter");
        query = ui.data(|data| data.get_temp(memory)).unwrap_or_default();
        let hint = format!("Filter {} parameters", body.params.len());
        let response = trill_noir::text_field(ui, theme, slot(0), memory, &mut query, &hint);
        cx.hint(&response, "Type part of a parameter\u{2019}s name to show only the matches");
        ui.data_mut(|data| data.insert_temp(memory, query.clone()));
        next = 1;
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
                let on = param.value >= 0.5;
                let chip = Chip::new(label, on).dimmed(body.dimmed);
                let width = chip.width(ui, theme).min(rect.width());
                let response = trill_noir::chip(
                    ui,
                    theme,
                    rect.with_max_x(rect.left() + width),
                    body.id.with(("param", &info.id)),
                    &chip,
                );
                cx.hint(&response, info.name.clone());
                if response.clicked() {
                    set_param(cx, body.device.id, param, if on { 0.0 } else { 1.0 });
                }
            }
            ParamKind::Enum(choices) => {
                let current = param.value.round().max(0.0) as usize;
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
                    let origin = pos2(rect.right() - width, rect.top());
                    let (clicked, _) =
                        trill_noir::chip_row(ui, theme, origin, width + 1.0, id, &labels, Some(current), body.dimmed);
                    hint_row(ui, cx, rect, &info.name);
                    if let Some(index) = clicked {
                        set_param(cx, body.device.id, param, index as f64);
                    }
                    continue;
                }
                let shown = units::format_param(info, param.value);
                let response = trill_noir::dropdown(ui, theme, rect, id, label, &shown, body.dimmed);
                cx.hint(&response, info.name.clone());
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
                    set_param(cx, body.device.id, param, value);
                }
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

        assert_eq!(generic_columns(0, 11), 1, "an empty panel still has a body");
        assert_eq!(generic_columns(11, 11), 1);
        assert_eq!(generic_columns(12, 11), 2);
        assert_eq!(generic_columns(40, 11), 4);
        assert_eq!(generic_columns(44, 11), 5, "above forty the filter box takes the first slot");
        assert_eq!(generic_columns(5, 0), 5);
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
                    let rows = column.sections.iter().flat_map(|section| section.rows);
                    for row in rows {
                        let labels: Vec<&str> = match row {
                            Row::Choice { labels, .. } => labels.to_vec(),
                            Row::Picks { picks, .. } => picks.iter().map(|(label, _)| *label).collect(),
                            Row::Pages(pages) => pages.iter().map(|page| page.label).collect(),
                            Row::Toggles(toggles) => toggles.iter().map(|(_, label)| *label).collect(),
                            Row::Param { .. } => continue,
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
        // The chips, then three bars: Mid 1 and Mid 2 have three parameters.
        assert_eq!(rows_height(rows, 18.0, 3.0), 18.0 + 3.0 + 3.0 * 18.0 + 2.0 * 3.0);
        let reverb = device_layouts::layout_for("builtin:reverb").unwrap();
        assert_eq!(rows_height(reverb.columns[0].sections[0].rows, 18.0, 3.0), 3.0 * 18.0 + 2.0 * 3.0);
        assert_eq!(layout_width(reverb), 150.0 + 2.0 * BODY_PADDING);
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
