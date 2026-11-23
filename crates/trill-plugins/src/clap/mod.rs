//! The CLAP host.
//!
//! A plugin instance is split along CLAP's thread model:
//!
//! * `factory`: the [`ClapFactory`] the DAW talks to. It knows which
//!   plugins are installed and turns a `clap:<plugin id>` URI into an
//!   instance.
//! * `instance`: everything CLAP calls "main thread": loading libraries,
//!   creating and destroying instances, parameters' metadata, state. It
//!   lives on the plugin main thread.
//! * `processor`: the audio-thread half, a realtime-safe
//!   [`DeviceProcessor`](trill_core::device::DeviceProcessor). It is built
//!   from `buffers` (the plugin's audio ports and their mapping to the
//!   device's stereo signal) and `events` (the engine's notes and
//!   parameter changes as CLAP events).
//! * `controller`: the [`DeviceController`](trill_core::device::DeviceController)
//!   the session keeps; it forwards to the plugin main thread.
//! * `gui`: the plugin's editor, opened in a window of the host's.
//! * `host`: the callbacks a plugin may make into its host, and `timers`,
//!   the timers it may ask the host to run.
//! * `paths` and `probe`: finding plugin files and reading their descriptors.
//! * `presets`: finding a plugin's presets through its preset-discovery
//!   providers.

mod buffers;
mod controller;
mod events;
mod factory;
mod gui;
#[cfg(all(test, windows))]
mod gui_tests;
mod host;
mod instance;
mod paths;
#[cfg(test)]
mod preset_tests;
mod presets;
mod probe;
mod processor;
// What the scripted editors do is asked only by the tests of editors, which
// are Windows' until editors are hosted elsewhere.
#[cfg(test)]
#[cfg_attr(not(windows), allow(dead_code))]
mod scripted;
#[cfg(test)]
mod tests;
mod timers;

pub use factory::{ClapConfig, ClapFactory};
pub(crate) use instance::{ClapThread, host_info};
pub use paths::{find_plugin_files, standard_search_paths};
pub(crate) use presets::read_presets;
pub(crate) use probe::probe;

/// URI scheme of CLAP devices: `clap:<plugin id>`, where the id is the one
/// in the plugin's CLAP descriptor.
pub const CLAP_SCHEME: &str = "clap";

/// File extension of a CLAP plugin (a library, or on macOS a bundle).
pub(crate) const PLUGIN_EXTENSION: &str = "clap";
