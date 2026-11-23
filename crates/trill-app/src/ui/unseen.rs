//! A window that is only there to have its picture taken, on Linux.
//!
//! On Windows such a window is put far off the screen, where it is never
//! seen and takes no focus. A Wayland compositor lets no window say where
//! it goes: one that asks to be off the screen is shown in the middle of it,
//! and is given the keyboard. So where there is an X server (a session of
//! X, or the X a Wayland desktop runs beside itself for older programs) the
//! window is made through it, as one the window manager leaves alone
//! (*override-redirect*): it goes where it is put, is never in a list of
//! windows, and is never given the keyboard. Where there is no X server it
//! is shown, and the log says so.

use eframe::NativeOptions;

/// Makes the window of `options` one that is neither seen nor given the
/// keyboard, where the system allows it.
pub(super) fn keep_out_of_sight(options: &mut NativeOptions) {
    let x = std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty());
    if !x {
        log::warn!(
            "the window that takes the picture is shown on the screen: a Wayland desktop lets no window \
             place itself off it, and there is no X server here (DISPLAY is not set) to make it through"
        );
        return;
    }
    // X says where a window is in 16 bits, in pixels: far enough up and to
    // the left of any screen, and within them at a scale of up to 3.
    options.viewport =
        options.viewport.clone().with_override_redirect(true).with_position(eframe::egui::pos2(-10_000.0, -10_000.0));
    options.event_loop_builder = Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;
        builder.with_x11();
    }));
}
