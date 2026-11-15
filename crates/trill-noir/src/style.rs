//! Sets egui up for the kit: Inter as the typeface and a style that makes
//! egui's own widgets (text fields, menus, tooltips, scroll bars, dialogs)
//! match the custom-painted ones: square corners, hairlines, flat fills and
//! no shadows.

use crate::theme::{Theme, over};
use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, vec2};
use std::sync::Arc;

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.otf");

/// Installs the typeface and `theme`'s style on `ctx`. Call at startup and
/// again whenever the theme changes.
pub fn install(ctx: &egui::Context, theme: &Theme) {
    install_fonts(ctx);
    ctx.set_theme(if theme.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.all_styles_mut(|style| apply(style, theme));
}

/// Inter leads the proportional family; egui's bundled fonts stay behind
/// it for the symbols Inter does not have.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".to_owned(), Arc::new(FontData::from_static(INTER)));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".to_owned());
    ctx.set_fonts(fonts);
}

fn apply(style: &mut egui::Style, t: &Theme) {
    let body = FontId::proportional(t.text.body.size);
    style.text_styles = [
        (egui::TextStyle::Small, FontId::proportional(t.text.value.size)),
        (egui::TextStyle::Body, body.clone()),
        (egui::TextStyle::Button, body.clone()),
        (egui::TextStyle::Heading, FontId::proportional(t.text.readout.size)),
        (egui::TextStyle::Monospace, body),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = vec2(6.0, t.metric.gap);
    spacing.button_padding = vec2(2.0 * t.metric.pad, 3.0);
    spacing.interact_size = vec2(t.metric.control, t.metric.control);
    // The hairline of a popup is its only frame; rows run edge to edge.
    spacing.menu_margin = Margin::same(1);
    spacing.menu_spacing = 1.0;
    spacing.window_margin = Margin::same(12);
    spacing.tooltip_width = 380.0;
    spacing.scroll.floating = true;
    spacing.scroll.bar_width = t.metric.scroll_bar;
    spacing.scroll.floating_width = 3.0;
    spacing.scroll.foreground_color = false;
    spacing.scroll.dormant_handle_opacity = 0.0;

    // Tooltips repeat what the status bar's hint already says at a glance,
    // so they wait until the pointer has clearly settled.
    style.interaction.tooltip_delay = 0.6;
    style.interaction.selectable_labels = false;

    let hairline = Stroke::new(1.0, t.hairline);
    let visuals = &mut style.visuals;
    visuals.dark_mode = t.dark;
    visuals.override_text_color = None;
    visuals.panel_fill = t.bg;
    visuals.window_fill = t.panel;
    visuals.window_stroke = hairline;
    visuals.window_corner_radius = CornerRadius::ZERO;
    visuals.menu_corner_radius = CornerRadius::ZERO;
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;
    visuals.extreme_bg_color = t.bg;
    visuals.text_edit_bg_color = Some(t.bg);
    visuals.faint_bg_color = t.panel;
    visuals.code_bg_color = t.bg;
    visuals.hyperlink_color = t.ink;
    visuals.warn_fg_color = t.warn;
    visuals.error_fg_color = t.signal;
    // Opaque, so selected text keeps the contrast of text on a wash.
    visuals.selection.bg_fill = over(over(t.bg, t.selected), t.selected);
    visuals.selection.stroke = Stroke::new(1.0, t.ink);
    visuals.text_cursor.stroke = Stroke::new(1.0, t.ink);
    visuals.slider_trailing_fill = true;
    visuals.button_frame = true;
    visuals.striped = false;

    let widgets = &mut visuals.widgets;
    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = CornerRadius::ZERO;
        state.expansion = 0.0;
        state.bg_stroke = hairline;
    }
    widgets.noninteractive.bg_fill = t.panel;
    widgets.noninteractive.weak_bg_fill = t.panel;
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.ink_dim);

    widgets.inactive.bg_fill = t.selected;
    widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    widgets.inactive.fg_stroke = Stroke::new(1.0, t.ink_dim);

    widgets.hovered.bg_fill = t.ink_faint;
    widgets.hovered.weak_bg_fill = t.hover;
    widgets.hovered.fg_stroke = Stroke::new(1.0, t.ink);

    widgets.active.bg_fill = t.ink_faint;
    widgets.active.weak_bg_fill = t.selected;
    widgets.active.fg_stroke = Stroke::new(1.0, t.ink);

    widgets.open.bg_fill = t.selected;
    widgets.open.weak_bg_fill = t.selected;
    widgets.open.fg_stroke = Stroke::new(1.0, t.ink);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_egui_draws_is_rounded_or_shadowed() {
        for theme in Theme::all() {
            let ctx = egui::Context::default();
            install(&ctx, &theme);
            let style = ctx.global_style();
            let visuals = &style.visuals;
            assert_eq!(visuals.window_corner_radius, CornerRadius::ZERO);
            assert_eq!(visuals.menu_corner_radius, CornerRadius::ZERO);
            assert_eq!(visuals.window_shadow, Shadow::NONE);
            assert_eq!(visuals.popup_shadow, Shadow::NONE);
            let widgets = &visuals.widgets;
            for state in [&widgets.noninteractive, &widgets.inactive, &widgets.hovered, &widgets.active, &widgets.open]
            {
                assert_eq!(state.corner_radius, CornerRadius::ZERO);
            }
            assert_eq!(visuals.panel_fill, theme.bg);
            assert_eq!(visuals.dark_mode, theme.dark);
        }
    }

    #[test]
    fn inter_is_the_first_proportional_face() {
        let mut seen = false;
        crate::testing::with_ui(|ui, _| {
            // U+2212 is in Inter; with only egui's bundled fonts a minus
            // sign is as narrow as a hyphen.
            let font = FontId::proportional(12.0);
            let (minus, plus) =
                ui.fonts_mut(|fonts| (fonts.glyph_width(&font, '\u{2212}'), fonts.glyph_width(&font, '+')));
            assert!((minus - plus).abs() < 0.01, "minus is {minus}, plus is {plus}");
            seen = true;
        });
        assert!(seen);
    }
}
