//! What the arrangement shows while the project has no tracks: how to
//! begin, by hand or with an assistant.

use super::headers;
use crate::ui::sounds::DropTarget;
use crate::ui::state::Cx;
use crate::ui::{dragging, widgets};
use eframe::egui::{self, Align2, Color32, Rect, Sense, Ui, vec2};
use trill_noir::{Variant, paint, text};

/// What a project with no tracks shows instead of an empty grid: how to
/// begin, by hand or with an assistant.
pub(super) fn show(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    const WIDTH: f32 = 520.0;
    const STEPS: [(&str, &str); 3] = [
        ("Add a track", "It comes with a Poly Synth, so it makes sound right away."),
        ("Double-click a lane to create a clip", "Then click in the piano roll below to write notes."),
        ("Press Space to play", "Drag the bar at the top of the ruler to loop a section while you work."),
    ];
    let theme = cx.theme;
    let height: f32 = if cx.mcp_url.is_some() { 318.0 } else { 296.0 };
    let block = Rect::from_center_size(area.center(), vec2(WIDTH.min(area.width() - 32.0), height.min(area.height())));
    let block = block.translate(vec2(0.0, (area.top() + 12.0 - block.top()).max(0.0)));
    widgets::place(ui, block, "welcome", egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_clip_rect(area.intersect(ui.clip_rect()));
        ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
        let line = |ui: &mut Ui, style: trill_noir::TextStyle, text: &str, color: Color32, height: f32| {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
            text::paint(ui.painter(), rect, Align2::LEFT_CENTER, style, text, color);
        };
        line(ui, theme.text.caps, "New project", theme.ink_faint, 16.0);
        ui.add_space(6.0);
        let title = trill_noir::TextStyle { size: 20.0, ..theme.text.body };
        line(ui, title, "Start your song", theme.ink, 28.0);
        line(
            ui,
            theme.text.body,
            "This project is empty. A track, a clip and a few notes are all it takes.",
            theme.ink_dim,
            20.0,
        );
        ui.add_space(14.0);

        for (number, (title, detail)) in STEPS.iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
            let painter = ui.painter();
            let badge = Rect::from_min_size(rect.min + vec2(0.0, 1.0), vec2(18.0, 18.0));
            paint::frame(painter, badge, theme.hairline);
            text::paint(
                painter,
                badge,
                Align2::CENTER_CENTER,
                theme.text.value,
                &(number + 1).to_string(),
                theme.ink_dim,
            );
            let lines = rect.with_min_x(badge.right() + 10.0);
            let (upper, lower) = lines.split_top_bottom_at_y(rect.top() + 19.0);
            text::paint(painter, upper, Align2::LEFT_CENTER, theme.text.body, title, theme.ink);
            let quiet = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
            text::paint(
                painter,
                lower.with_max_y(lower.top() + 16.0),
                Align2::LEFT_CENTER,
                quiet,
                detail,
                theme.ink_dim,
            );
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme.metric.gap;
            if trill_noir::button(ui, theme, "Add first track", Variant::Primary).clicked() {
                headers::add_track(cx);
            }
            if trill_noir::button(ui, theme, "Open demo song", Variant::Default).clicked() {
                load_demo(cx);
            }
            if trill_noir::button(ui, theme, "Open project\u{2026}", Variant::Default).clicked() {
                crate::ui::files::open(ui.ctx(), cx);
            }
        });

        ui.add_space(18.0);
        let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
        ui.add_space(10.0);
        line(ui, theme.text.caps, "Working with an AI assistant", theme.ink_faint, 16.0);
        ui.add_space(4.0);
        let quiet = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
        for sentence in [
            "An assistant connected through MCP can add tracks, write parts and mix while you watch.",
            "Every change appears here as it happens, and Ctrl+Z undoes it like any other edit.",
        ] {
            line(ui, quiet, sentence, theme.ink_dim, 18.0);
        }
        if let Some(url) = cx.mcp_url {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            let used = text::paint(
                ui.painter(),
                rect,
                Align2::LEFT_CENTER,
                theme.text.micro,
                "Connect it to",
                theme.ink_faint,
            );
            let address = rect.with_min_x(rect.left() + used + 6.0);
            text::paint(ui.painter(), address, Align2::LEFT_CENTER, theme.text.value, url, theme.ink);
        }
    });
}

/// A sound dragged over the empty arrangement starts the song: it lands on
/// a track of its own at the first bar.
pub(super) fn sound_drop(ui: &Ui, cx: &mut Cx, area: Rect) {
    let over = cx.state.sound_drag.as_ref().and_then(|drag| drag.pointer).is_some_and(|pointer| area.contains(pointer));
    if !over {
        return;
    }
    let count = cx.state.sound_drag.as_ref().map_or(1, |drag| drag.paths.len());
    let colors = dragging::new_track_colors(cx, count);
    if let Some(drag) = &mut cx.state.sound_drag {
        drag.offer(DropTarget::NewTrack { beat: 0.0, colors });
    }
    paint::frame(&ui.painter_at(area), area.shrink(4.0), cx.theme.ink);
}

/// A sound of an instrument and its effects, dragged over the empty
/// arrangement, starts the song too: on a track of its own.
pub(super) fn preset_drop(ui: &Ui, cx: &mut Cx, area: Rect) {
    if crate::ui::sound_presets::over(cx, area).is_none() {
        return;
    }
    crate::ui::sound_presets::offer_new_track(cx);
    paint::frame(&ui.painter_at(area), area.shrink(4.0), cx.theme.ink);
}

fn load_demo(cx: &mut Cx) {
    if let Some(clip) = cx.try_session(crate::demo::load)
        && let Ok(track) = cx.session.lock().project().clip_track(clip)
    {
        cx.state.select_clip(track, clip);
    }
}
