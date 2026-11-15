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
//! * `host`: the callbacks a plugin may make into its host.
//! * `paths` and `probe`: finding plugin files and reading their descriptors.

mod buffers;
mod controller;
mod events;
mod factory;
mod host;
mod instance;
mod paths;
mod probe;
mod processor;
#[cfg(test)]
mod scripted;
#[cfg(test)]
mod tests;

pub use factory::{ClapConfig, ClapFactory};
pub(crate) use instance::ClapThread;
pub use paths::{find_plugin_files, standard_search_paths};
pub(crate) use probe::probe;

/// URI scheme of CLAP devices: `clap:<plugin id>`, where the id is the one
/// in the plugin's CLAP descriptor.
pub const CLAP_SCHEME: &str = "clap";

/// File extension of a CLAP plugin (a library, or on macOS a bundle).
pub(crate) const PLUGIN_EXTENSION: &str = "clap";
