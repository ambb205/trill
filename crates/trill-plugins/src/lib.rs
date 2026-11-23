//! Hosting for third-party audio plugins.
//!
//! A hosted plugin looks exactly like a built-in device to the rest of the
//! DAW: [`ClapFactory`] and [`Vst3Factory`] implement
//! [`trill_core::device::DeviceFactory`], and the instances they create
//! implement the processor and controller halves of the device API. The
//! engine never learns which kind it is driving. [`PluginHost`] sets up
//! all formats at once.
//!
//! Layout:
//!
//! * [`clap`]: the CLAP host: discovery, instantiation, processing, state.
//! * [`vst3`]: the VST3 host, module for module the same shape.
//! * [`scan`]: format-independent scanning: the crash-isolated scanner
//!   process, the on-disk cache of its results and the catalog built from
//!   them.
//! * `presets`: finding the presets a plugin offers: read in the scanner
//!   process too, in the background, and cached beside the scan.
//! * `main_thread` and `plugin_thread`: the single thread on which every
//!   plugin's main-thread calls are made, whichever thread the DAW asked
//!   from and whichever format the plugin is.
//! * `editor`: the window a plugin's own interface is shown in, and what
//!   the formats share in hosting it.
//! * `signal` and `notes`: what every format's audio-thread half needs:
//!   the mapping between a plugin's ports and the device's stereo signal,
//!   and note bookkeeping.
//! * `param_text`: a parameter's value from the text a plugin shows for
//!   it, for the plugins that will not say themselves.
//! * `vendor`: the presets vendors keep in files of their own formats,
//!   listed with a plugin's other presets and loaded by building the
//!   state one stands for.
//! * `edits` and `exclusive`: how the audio-thread and main-thread halves
//!   of a hosted plugin pass things to each other without waiting.
//!
//! Hosting happens in the DAW's own process. Only *scanning* and reading
//! a plugin's list of presets are isolated in a child process; a plugin
//! that crashes while instantiated, while processing audio or while it
//! loads a preset takes the DAW with it.
//!
//! # Example
//!
//! ```no_run
//! use trill_core::device::{CompositeFactory, DeviceFactory};
//! use trill_plugins::{PluginHost, Scanner};
//!
//! # fn main() -> std::io::Result<()> {
//! let plugins = PluginHost::new(Scanner::Auto)?;
//! // The first use scans the system's plugin directories. That can take a
//! // while the first time, so an application does it off its main thread.
//! for failure in plugins.scan_failures() {
//!     eprintln!("skipped {}: {}", failure.path.display(), failure.reason);
//! }
//! // Built-in devices and plugins behind one factory. The plugin factories
//! // share their scans with `plugins`, which stays usable for rescanning.
//! let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(trill_dsp::BuiltinFactory)];
//! factories.extend(plugins.factories());
//! let devices = CompositeFactory::new(factories);
//! println!("{} devices", devices.descriptors().len());
//! # Ok(())
//! # }
//! ```

mod beside;
pub mod clap;
mod editor;
mod edits;
mod exclusive;
pub mod first_thread;
#[cfg(test)]
mod key_tests;
mod main_thread;
pub mod making;
mod notes;
mod param_text;
mod plugin_host;
mod plugin_thread;
mod presets;
mod repeats;
pub mod scan;
mod signal;
#[cfg(test)]
mod test_support;
mod transport;
#[cfg(test)]
mod transport_tests;
mod vendor;
pub mod vst3;
#[cfg(target_os = "linux")]
mod x_errors;

pub use clap::{CLAP_SCHEME, ClapConfig, ClapFactory};
pub use plugin_host::PluginHost;

pub use presets::file::file_of as preset_file_of;
pub use scan::{Known, PluginRecord, ScanFailure, ScanMode, Scanner};
pub use vendor::{VendorPlaces, describe_state, split_state};
pub use vst3::{VST3_SCHEME, Vst3Config, Vst3Factory};

/// Puts Trill's handler of X errors in place now, where libX11 is loaded, as
/// the host does after a plugin is made and after an editor opens; and says
/// how many errors it has taken in this process. For a test in a process of
/// its own: the handler is the process's.
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub fn hold_x_errors() -> u64 {
    x_errors::hold();
    x_errors::heard()
}

/// Why no plugin's own window can be shown in this run of the program, where
/// that is so (on Linux, no X server could be reached): a sentence to say
/// once. No device then says it has one, so that none is offered.
pub fn editors_cannot_be_shown() -> Option<&'static str> {
    editor::cannot_show()
}

/// Keeps the windows of plugin editors this process opens from here on out
/// of sight: for a program that takes pictures of them. On macOS they are
/// never put on screen nor given the keyboard, and the program is never
/// brought to the front (a picture is asked of the views, and nothing is
/// read off the screen, which takes the user's leave there). On Linux it is
/// a window the window manager leaves alone, far off every screen. Elsewhere
/// nothing changes.
pub fn keep_editors_out_of_sight() {
    editor::keep_out_of_sight();
}
