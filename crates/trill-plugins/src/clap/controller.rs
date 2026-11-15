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
pub(crate) struct ClapController {
    main: MainThread<PluginThread>,
    instance: InstanceId,
    plugin_id: String,
}

impl ClapController {
    pub(crate) fn new(main: MainThread<PluginThread>, instance: InstanceId, plugin_id: String) -> Self {
        Self { main, instance, plugin_id }
    }
}

impl DeviceController for ClapController {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.clap.save_state(instance)) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("[{}] cannot save its state: {error}", self.plugin_id);
                None
            }
        }
    }

    fn param_values(&mut self) -> Option<Vec<f64>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.clap.param_values(instance)) {
            Ok(values) => values,
            Err(error) => {
                log::warn!("[{}] cannot report its parameter values: {error}", self.plugin_id);
                None
            }
        }
    }
}
