//! The corner above the track headers: how many tracks there are and the
//! way to add one, and under that the arrangement's own controls: the
//! grid, snapping, following the playhead, and the pencil and blade tools.

use super::Geometry;
use super::headers::{self, HEADER_LEFT, HEADER_RIGHT};
use crate::ui::shortcuts::{self, Command};
use crate::ui::state::{Cx, Tool};
use crate::ui::timeline::GridChoice;
use eframe::egui::{self, Align2, Rect, Sense, Ui, pos2, vec2};
use trill_noir::{Chip, Icon, MenuItem, paint, text};

/// Height of the row of controls along the corner's foot.
const TOOLS_ROW: f32 = 18.0;
const TOOL_BUTTON: f32 = 20.0;

pub(super) fn show(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let corner = geometry.corner;
    let painter = ui.painter_at(corner);
    painter.rect_filled(corner, 0.0, theme.panel);
    paint::hline_above(&painter, corner.x_range(), corner.bottom(), theme.hairline);
    paint::vline_left_of(&painter, corner.right(), corner.y_range(), theme.hairline);

    let tools = Rect::from_min_max(
        pos2(corner.left() + HEADER_LEFT, corner.bottom() - TOOLS_ROW - 3.0),
        pos2(corner.right() - HEADER_RIGHT + 6.0, corner.bottom() - 3.0),
    );
    let top = Rect::from_min_max(pos2(tools.left(), corner.top() + 1.0), pos2(tools.right(), tools.top() - 1.0));

    let count = cx.snap.project.tracks.len();
    let label = if count == 1 { "1 track".to_owned() } else { format!("{count} tracks") };
    text::paint(&painter, top, Align2::LEFT_CENTER, theme.text.micro, &label, theme.ink_faint);
    add_track(ui, cx, top);
    controls(ui, cx, tools);
}

/// "+ Track": a click adds an instrument track, and its menu offers the
/// other kinds.
fn add_track(ui: &mut Ui, cx: &mut Cx, row: Rect) {
    let theme = cx.theme;
    let width = text::width(ui.painter(), theme.text.micro, "+ Track").ceil() + 12.0;
    let rect = Rect::from_min_max(pos2(row.right() - width, row.top() + 1.0), pos2(row.right(), row.bottom() - 1.0));
    let response = ui.interact(rect, ui.scope_id().with("corner-add"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "New track"));
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.hover);
    }
    let ink = if response.hovered() { theme.ink } else { theme.ink_faint };
    text::paint(ui.painter(), rect, Align2::CENTER_CENTER, theme.text.micro, "+ Track", ink);
    cx.hint(
        &response,
        "Add a track with a Poly Synth \u{00B7} Ctrl+T \u{00B7} right-click for a group or return track",
    );
    if response.clicked() {
        headers::add_track(cx);
    }
    response.context_menu(|ui| {
        ui.set_min_width(190.0);
        if MenuItem::new("Add track").shortcut(&shortcuts::keys(Command::AddTrack)).show(ui, theme).clicked() {
            headers::add_track(cx);
        }
        if MenuItem::new("Add group track").show(ui, theme).clicked() {
            headers::add_group_track(cx);
        }
        if MenuItem::new("Add return track").show(ui, theme).clicked() {
            headers::add_return_track(cx);
        }
    });
}

/// Grid menu, snap and follow toggles, and the two tools.
fn controls(ui: &mut Ui, cx: &mut Cx, row: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with("arrangement-controls");
    let gap = theme.metric.gap;
    let mut x = row.left();

    // The grid: what edits snap to and which lines are drawn. It has no
    // label of its own: the corner is narrow, and the Snap chip beside it
    // and its menu say what it is.
    let grid = cx.state.arrangement.grid;
    // Sized for the longest choice, so the controls after it never move.
    let width = trill_noir::dropdown_width(ui, theme, "1/16T");
    let rect = Rect::from_min_size(pos2(x, row.top()), vec2(width, row.height()));
    let response = trill_noir::dropdown(ui, theme, rect, id.with("grid"), "", grid.label(), false);
    cx.hint(&response, "Grid: what clips and points snap to \u{00B7} Auto follows the zoom");
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(170.0);
        trill_noir::menu_heading(ui, theme, "Grid");
        for (index, choice) in GridChoice::ALL.into_iter().enumerate() {
            // Auto, the straight note values, the triplets, off.
            if matches!(index, 1 | 5 | 7) {
                trill_noir::menu_separator(ui, theme);
            }
            let label = match choice {
                GridChoice::Adaptive => "Auto: follow the zoom",
                other => other.label(),
            };
            if MenuItem::new(label).checked(choice == grid).show(ui, theme).clicked() {
                cx.state.arrangement.grid = choice;
            }
        }
    });
    x = rect.right() + gap;

    let mut toggle = |ui: &Ui, cx: &mut Cx, label: &str, name: &str, on: bool, hint: &str| {
        let chip = Chip::new(label, on).named(name);
        let rect = Rect::from_min_size(pos2(x, row.top()), vec2(chip.width(ui, theme), row.height()));
        x = rect.right() + gap;
        let response = trill_noir::chip(ui, theme, rect, id.with(name), &chip);
        cx.hint(&response, hint);
        response.clicked()
    };
    let view = &cx.state.arrangement;
    let (snap, follow) = (view.snap, view.follow);
    if toggle(
        ui,
        cx,
        "Snap",
        "Snap to grid",
        snap,
        "Land edits on the grid \u{00B7} hold Alt while dragging to suspend it",
    ) {
        cx.state.arrangement.snap = !snap;
    }
    // The keys as they are now: with the computer keyboard on, a letter it
    // plays is a shortcut with Alt.
    let playing = cx.keyboard.is_on();
    let about = shortcuts::with_keys("Keep the playhead in view while the song plays", Command::ToggleFollow, playing);
    if toggle(ui, cx, "Follow", "Follow playhead", follow, &about) {
        cx.state.arrangement.follow = !follow;
    }

    // The tools sit at the right end, under "+ Track".
    let tool = cx.state.arrangement.tool;
    let blade = shortcuts::with_keys("Blade: click a clip to split it", Command::ToggleBlade, playing);
    let pencil = shortcuts::with_keys("Pencil: draw automation in steps of the grid", Command::ToggleDraw, playing);
    let tools = [(Tool::Blade, Icon::Split, blade), (Tool::Draw, Icon::Pencil, pencil)];
    let mut right = row.right();
    for (which, icon, hint) in tools {
        let rect = Rect::from_min_size(pos2(right - TOOL_BUTTON, row.top()), vec2(TOOL_BUTTON, row.height()));
        right = rect.left() - gap;
        let on = tool == which;
        let color = if on { theme.ink } else { theme.ink_dim };
        let response = trill_noir::icon_button(ui, theme, rect, id.with(icon), icon, on, color);
        cx.hint(&response, hint);
        if response.clicked() {
            cx.state.arrangement.tool = if on { Tool::Pointer } else { which };
        }
    }
}
