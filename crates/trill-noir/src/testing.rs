//! Helpers for this crate's tests: frames of a headless egui context with
//! the kit installed.

use crate::Theme;
use egui::{Event, Pos2, RawInput, Rect, Ui, vec2};

/// Runs one frame of `ctx` at `time` with `events`, in a 400 x 200 window.
pub fn frame(ctx: &egui::Context, time: f64, events: Vec<Event>, mut add: impl FnMut(&mut Ui, &Theme)) {
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))),
        time: Some(time),
        events,
        focused: true,
        ..Default::default()
    };
    let theme = Theme::noir();
    let mut output = ctx.run_ui(input, |ui| add(ui, &theme));
    // Nothing is drawn, so the font atlas updates have nowhere to go.
    output.textures_delta.clear();
}

/// A context with Noir installed.
pub fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::install(&ctx, &Theme::noir());
    ctx
}

/// Runs `check` inside one frame of a fresh context.
pub fn with_ui(check: impl FnOnce(&mut Ui, &Theme)) {
    let mut check = Some(check);
    frame(&context(), 0.0, Vec::new(), |ui, theme| {
        if let Some(check) = check.take() {
            check(ui, theme);
        }
    });
}
