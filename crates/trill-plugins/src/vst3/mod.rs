//! The VST3 host.
//!
//! A plugin instance is split along VST3's thread model, and the modules
//! mirror those of the CLAP host next door:
//!
//! * `factory`: the [`Vst3Factory`] the DAW talks to. It knows which
//!   plugins are installed and turns a `vst3:<class id>` URI into an
//!   instance.
//! * `instance`: everything VST3 calls "UI thread": loading modules,
//!   creating and destroying plugins, their buses, parameters and state.
//!   It lives on the plugin main thread, which CLAP plugins share.
//! * `processor`: the audio-thread half, a realtime-safe
//!   [`DeviceProcessor`](trill_core::device::DeviceProcessor). It is built
//!   from `buffers` (the plugin's audio buses and their mapping to the
//!   device's stereo signal), `events` (the engine's notes as VST3 events,
//!   and the transport) and `changes` (parameter values on their way into
//!   and out of `process`).
//! * `controller`: the [`DeviceController`](trill_core::device::DeviceController)
//!   the session keeps; it forwards to the plugin main thread.
//! * `editor`: the plugin's view, opened in a window of the host's.
//! * `links`: what the two halves share. A VST3 plugin is itself two
//!   objects, a processor and an edit controller, which only learn of each
//!   other's parameter changes through the host.
//! * `host`: the objects a plugin is given to call back into.
//! * `module`, `paths` and `probe`: loading plugin files, finding them and
//!   reading their classes.
//! * `presets`: finding a plugin's preset files and the programs of its
//!   program lists.
//! * `class_id`, `params`, `state`, `stream`, `text`: VST3's ids,
//!   parameters, saved state and strings, translated to and from the
//!   DAW's terms.
//!
//! VST3 is a COM-style API, used here through the `vst3` crate's bindings.
//! Those are unsafe by nature; `com` holds the few helpers everything else
//! leans on.

mod buffers;
mod changes;
mod class_id;
mod com;
mod controller;
mod editor;
#[cfg(all(test, windows))]
mod editor_tests;
mod events;
mod factory;
mod host;
mod instance;
mod links;
mod module;
mod params;
mod paths;
mod presets;
mod probe;
mod processor;
#[cfg(test)]
mod scripted;
mod state;
mod stream;
#[cfg(test)]
mod tests;
mod text;

pub use factory::{Vst3Config, Vst3Factory};
pub(crate) use instance::Vst3Thread;
pub(crate) use paths::is_plugin_file;
pub use paths::{find_plugin_files, standard_search_paths};
pub(crate) use presets::read_presets;
pub use presets::standard_preset_paths;
pub(crate) use probe::probe;

/// URI scheme of VST3 devices: `vst3:<class id>`, where the class id is
/// that of the plugin's processor class, written as 32 hexadecimal digits.
pub const VST3_SCHEME: &str = "vst3";

/// What a VST3 plugin is named: a bundle directory `Name.vst3` or, on
/// Windows, a single library of that name.
pub(crate) const PLUGIN_EXTENSION: &str = "vst3";
