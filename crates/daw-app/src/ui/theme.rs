//! The look of the app: one dark palette, the fonts and the egui style built
//! from them. Views take every color and size from here so the window reads
//! as one design.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, Vec2};
use std::sync::Arc;

// ---- surfaces, darkest to lightest ---------------------------------------

/// Wells that sit below the panels: lanes, the note grid, text fields.
pub const BG_INSET: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x23);
/// Cards and headers that sit on a panel.
pub const BG_RAISED: Color32 = Color32::from_rgb(0x22, 0x26, 0x2E);
pub const BG_CONTROL: Color32 = Color32::from_rgb(0x2D, 0x32, 0x3C);
pub const BG_CONTROL_HOVER: Color32 = Color32::from_rgb(0x3A, 0x40, 0x4D);
pub const BG_CONTROL_ACTIVE: Color32 = Color32::from_rgb(0x46, 0x4D, 0x5C);

pub const BORDER: Color32 = Color32::from_rgb(0x2B, 0x30, 0x3A);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x41, 0x48, 0x57);

// ---- text ----------------------------------------------------------------

pub const TEXT: Color32 = Color32::from_rgb(0xE4, 0xE7, 0xEC);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9B, 0xA3, 0xB2);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x66, 0x6E, 0x7E);
/// Text on top of a track color or another bright fill.
pub const TEXT_ON_COLOR: Color32 = Color32::from_rgb(0x10, 0x12, 0x16);

// ---- signals -------------------------------------------------------------

/// Selection, focus and the app's own highlights. Track colors carry the
/// rest of the color in the window, so this stays a calm blue.
pub const ACCENT: Color32 = Color32::from_rgb(0x6E, 0xA8, 0xFF);
pub const PLAY: Color32 = Color32::from_rgb(0x4C, 0xD9, 0x8A);
pub const MUTE: Color32 = Color32::from_rgb(0xF2, 0xB5, 0x4A);
pub const SOLO: Color32 = Color32::from_rgb(0x5C, 0xC8, 0xF0);
pub const WARN: Color32 = Color32::from_rgb(0xF5, 0xB5, 0x4A);
pub const ERROR: Color32 = Color32::from_rgb(0xF0, 0x6A, 0x60);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xF4, 0xF6, 0xFA);

// ---- timeline ------------------------------------------------------------

pub const GRID_BAR: Color32 = Color32::from_rgb(0x30, 0x35, 0x40);
pub const GRID_BEAT: Color32 = Color32::from_rgb(0x23, 0x27, 0x2F);
pub const GRID_SUBDIVISION: Color32 = Color32::from_rgb(0x1C, 0x1F, 0x26);
/// Every other bar is tinted with this so long timelines are easy to read.
pub const LANE_ALTERNATE: Color32 = Color32::from_rgb(0x15, 0x18, 0x1D);

// ---- metrics -------------------------------------------------------------

pub const RADIUS: CornerRadius = CornerRadius::same(5);
pub const RADIUS_SMALL: CornerRadius = CornerRadius::same(3);
/// Standard gap between unrelated things; half of it between related ones.
pub const GAP: f32 = 8.0;

const STRONG_FAMILY: &str = "strong";

/// Body text.
pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Semibold text for names and headings.
pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(STRONG_FAMILY.into()))
}

/// `0xRRGGBB`, as stored on tracks, to a color.
pub fn track_color(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Blends `from` toward `to`; `t` = 0 gives `from`, 1 gives `to`.
pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    from.lerp_to_gamma(to, t.clamp(0.0, 1.0))
}

/// `color` with its opacity scaled by `alpha` (0..=1).
pub fn fade(color: Color32, alpha: f32) -> Color32 {
    color.gamma_multiply(alpha.clamp(0.0, 1.0))
}

pub fn hairline(color: Color32) -> Stroke {
    Stroke::new(1.0, color)
}

/// Frame for a panel that fills its side of the window edge to edge.
pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::NONE.fill(fill)
}

/// Installs fonts and the dark style on `ctx`. Call once at startup.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(apply_style);
}

/// Uses the platform's UI typeface where we know where to find one, since it
/// is hinted for small sizes on that platform's screens; elsewhere egui's
/// bundled font is used for both weights.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut add = |name: &str, candidates: &[&str], family: FontFamily| {
        let mut stack = fallbacks.clone();
        if let Some(bytes) = candidates.iter().find_map(|path| std::fs::read(path).ok()) {
            fonts.font_data.insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
            stack.insert(0, name.to_owned());
        }
        fonts.families.insert(family, stack);
    };
    add("ui-regular", &["C:/Windows/Fonts/segoeui.ttf"], FontFamily::Proportional);
    add(
        "ui-semibold",
        &["C:/Windows/Fonts/seguisb.ttf", "C:/Windows/Fonts/segoeuib.ttf"],
        FontFamily::Name(STRONG_FAMILY.into()),
    );
    ctx.set_fonts(fonts);
}

fn apply_style(style: &mut egui::Style) {
    use egui::TextStyle;

    style.text_styles = [
        (TextStyle::Small, font(11.0)),
        (TextStyle::Body, font(13.0)),
        (TextStyle::Button, font(13.0)),
        (TextStyle::Heading, strong(18.0)),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(GAP, 6.0);
    spacing.button_padding = Vec2::new(10.0, 4.0);
    spacing.interact_size = Vec2::new(28.0, 24.0);
    spacing.menu_margin = Margin::same(6);
    spacing.window_margin = Margin::same(14);
    spacing.combo_height = 320.0;
    spacing.scroll.bar_width = 8.0;
    spacing.scroll.floating = true;

    let visuals = &mut style.visuals;
    visuals.dark_mode = true;
    visuals.override_text_color = None;
    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_RAISED;
    visuals.window_stroke = hairline(BORDER_STRONG);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(6);
    visuals.extreme_bg_color = BG_INSET;
    visuals.text_edit_bg_color = Some(BG_INSET);
    visuals.faint_bg_color = BG_RAISED;
    visuals.code_bg_color = BG_INSET;
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARN;
    visuals.error_fg_color = ERROR;
    visuals.selection.bg_fill = fade(ACCENT, 0.45);
    visuals.selection.stroke = hairline(ACCENT);
    visuals.slider_trailing_fill = true;
    visuals.popup_shadow.color = Color32::from_black_alpha(110);
    visuals.window_shadow.color = Color32::from_black_alpha(120);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = BG_PANEL;
    widgets.noninteractive.weak_bg_fill = BG_PANEL;
    widgets.noninteractive.bg_stroke = hairline(BORDER);
    widgets.noninteractive.fg_stroke = hairline(TEXT);
    widgets.noninteractive.corner_radius = RADIUS;

    widgets.inactive.bg_fill = BG_CONTROL;
    widgets.inactive.weak_bg_fill = BG_CONTROL;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.fg_stroke = hairline(TEXT);
    widgets.inactive.corner_radius = RADIUS;
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = BG_CONTROL_HOVER;
    widgets.hovered.weak_bg_fill = BG_CONTROL_HOVER;
    widgets.hovered.bg_stroke = hairline(BORDER_STRONG);
    widgets.hovered.fg_stroke = hairline(Color32::WHITE);
    widgets.hovered.corner_radius = RADIUS;
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = BG_CONTROL_ACTIVE;
    widgets.active.weak_bg_fill = BG_CONTROL_ACTIVE;
    widgets.active.bg_stroke = hairline(ACCENT);
    widgets.active.fg_stroke = hairline(Color32::WHITE);
    widgets.active.corner_radius = RADIUS;
    widgets.active.expansion = 0.0;

    widgets.open.bg_fill = BG_CONTROL_HOVER;
    widgets.open.weak_bg_fill = BG_CONTROL_HOVER;
    widgets.open.bg_stroke = hairline(BORDER_STRONG);
    widgets.open.fg_stroke = hairline(Color32::WHITE);
    widgets.open.corner_radius = RADIUS;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_colors_unpack_rgb() {
        assert_eq!(track_color(0xF2994A), Color32::from_rgb(0xF2, 0x99, 0x4A));
        assert_eq!(track_color(0x000001), Color32::from_rgb(0, 0, 1));
    }

    #[test]
    fn mix_interpolates_between_its_ends() {
        let (a, b) = (Color32::from_rgb(0, 0, 0), Color32::from_rgb(200, 100, 50));
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 7.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 50, 25));
    }
}
