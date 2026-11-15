//! The off-audio-thread half of a hosted plugin: state and parameter values.

use super::instance::InstanceId;
use crate::main_thread::MainThread;
use crate::plugin_thread::PluginThread;
use std::time::Duration;
use trill_core::device::DeviceController;

/// How long a plugin may take to serialize its state or report its
/// parameter values before the request is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Forwards the session's requests to the plugin main thread, whichever
/// thread the session happens to be driven from.
///
/// The controller does not keep the plugin alive: the instance belongs to
/// its processor. Once that is dropped, requests simply return `None`.
pub(crate) struct Vst3Controller {
    main: MainThread<PluginThread>,
    instance: InstanceId,
    /// The plugin's name, for messages.
    name: String,
}

impl Vst3Controller {
    pub(crate) fn new(main: MainThread<PluginThread>, instance: InstanceId, name: String) -> Self {
        Self { main, instance, name }
    }
}

impl DeviceController for Vst3Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.save_state(instance)) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("[{}] cannot save its state: {error}", self.name);
                None
            }
        }
    }

    fn param_values(&mut self) -> Option<Vec<f64>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.param_values(instance)) {
            Ok(values) => values,
            Err(error) => {
                log::warn!("[{}] cannot report its parameter values: {error}", self.name);
                None
            }
        }
    }
}
