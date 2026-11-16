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
//! * `main_thread` and `plugin_thread`: the single thread on which every
//!   plugin's main-thread calls are made, whichever thread the DAW asked
//!   from and whichever format the plugin is.
//! * `editor`: the window a plugin's own interface is shown in, and what
//!   the formats share in hosting it.
//! * `signal` and `notes`: what every format's audio-thread half needs:
//!   the mapping between a plugin's ports and the device's stereo signal,
//!   and note bookkeeping.
//! * `edits` and `exclusive`: how the audio-thread and main-thread halves
//!   of a hosted plugin pass things to each other without waiting.
//!
//! Hosting happens in the DAW's own process. Only *scanning* is isolated
//! in a child process; a plugin that crashes while instantiated or while
//! processing audio takes the DAW with it.
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

pub mod clap;
mod editor;
mod edits;
mod exclusive;
mod main_thread;
mod notes;
mod plugin_host;
mod plugin_thread;
pub mod scan;
mod signal;
#[cfg(test)]
mod test_support;
pub mod vst3;

pub use clap::{CLAP_SCHEME, ClapConfig, ClapFactory};
pub use plugin_host::PluginHost;
pub use scan::{PluginRecord, ScanFailure, ScanMode, Scanner};
pub use vst3::{VST3_SCHEME, Vst3Config, Vst3Factory};
