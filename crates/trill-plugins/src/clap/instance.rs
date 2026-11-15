//! The main-thread half of the host: everything CLAP requires to happen on
//! "the main thread". [`ClapThread`] is the state of the plugin main thread;
//! it owns the loaded libraries and every plugin instance, and is only ever
//! touched by jobs running on that thread.

use super::events::{NoteDialect, NotePort};
use super::host::{HostMainThread, HostShared, TrillHost};
use crate::main_thread::ThreadState;
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_host::events::Pckn;
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::*;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use trill_core::device::{ParamInfo, ParamKind};

/// Identifies a plugin instance on the plugin main thread. Never reused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstanceId(u64);

/// What to instantiate, and the model state to bring it up with.
pub(crate) struct CreateRequest {
    pub path: PathBuf,
    pub plugin_id: String,
    /// Opaque state from an earlier [`ClapThread::save_state`].
    pub state: Option<Vec<u8>>,
    /// Parameter values by CLAP parameter id, applied on top of `state`.
    pub values: Vec<(u32, f64)>,
    pub sample_rate: f64,
    pub max_block: u32,
}

/// One parameter of an instance.
pub(crate) struct ParamSpec {
    pub id: ClapId,
    pub info: ParamInfo,
    /// False for parameters the plugin only reports (meters and the like).
    pub writable: bool,
}

/// Channel count of each audio port, in port order. Port 0 is the main one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PortLayout {
    pub inputs: Vec<u32>,
    pub outputs: Vec<u32>,
}

/// A plugin activated for one audio configuration.
pub(crate) struct Activated {
    pub processor: StoppedPluginAudioProcessor<TrillHost>,
    pub latency: u32,
}

/// A freshly created, activated instance, ready for the audio thread.
pub(crate) struct Created {
    pub id: InstanceId,
    pub activated: Activated,
    /// In the order the processor's `set_param` indexes them.
    pub params: Vec<ParamSpec>,
    /// `(parameter index, value)` for the model values that were applied.
    pub initial_values: Vec<(usize, f64)>,
    pub ports: PortLayout,
    /// How the plugin takes notes, if it takes any.
    pub notes: Option<NotePort>,
}

struct Instance {
    plugin: PluginInstance<TrillHost>,
    state: Option<PluginState>,
    params: Option<PluginParams>,
    latency: Option<PluginLatency>,
    /// In parameter-list order.
    param_ids: Vec<ClapId>,
}

/// The state of the plugin main thread.
pub(crate) struct ClapThread {
    host_info: HostInfo,
    /// Libraries stay loaded once used: reloading one for every instance is
    /// slow, and plugins are rarely tested against repeated unloading.
    entries: HashMap<PathBuf, PluginEntry>,
    instances: HashMap<InstanceId, Instance>,
    next_id: u64,
}

impl ThreadState for ClapThread {
    fn new() -> Self {
        let version = CString::new(env!("CARGO_PKG_VERSION")).unwrap_or_default();
        Self {
            host_info: HostInfo::new_from_cstring(c"Trill".into(), c"Trill".into(), c"".into(), version),
            entries: HashMap::new(),
            instances: HashMap::new(),
            next_id: 1,
        }
    }

    fn wants_idle(&self) -> bool {
        !self.instances.is_empty()
    }

    /// Serves the requests plugins left in their [`HostShared`] flags.
    fn idle(&mut self) {
        for instance in self.instances.values_mut() {
            let (callback, restart) = instance
                .plugin
                .access_shared_handler(|shared| (shared.take_callback_request(), shared.take_restart_request()));
            if callback {
                instance.plugin.call_on_main_thread_callback();
            }
            if restart {
                let id = instance.plugin.access_shared_handler(|shared| shared.plugin_id().to_string());
                log::info!(
                    "[{id}] asked to be restarted; changes to its latency, ports or parameter list \
                     take effect when the device is next created"
                );
            }
        }
    }
}

impl ClapThread {
    fn entry(&mut self, path: &Path) -> Result<PluginEntry, String> {
        if let Some(entry) = self.entries.get(path) {
            return Ok(entry.clone());
        }
        // SAFETY: loading a plugin runs code this crate has no control
        // over; no check made here could make that sound. The file was
        // found in a CLAP directory and its descriptors were read by the
        // scanner, so it is at least a library that follows the CLAP ABI
        // far enough to be listed.
        let entry =
            unsafe { PluginEntry::load(path) }.map_err(|error| format!("cannot load '{}': {error}", path.display()))?;
        self.entries.insert(path.to_path_buf(), entry.clone());
        Ok(entry)
    }

    /// Registers a plugin that is compiled into the test binary, as if it
    /// had been loaded from a file, and returns that pretended file's path
    /// and what it contains.
    #[cfg(test)]
    pub(crate) fn preload(
        &mut self,
        descriptor: &'static clack_host::entry::EntryDescriptor,
    ) -> Result<(PathBuf, Vec<crate::scan::PluginRecord>), String> {
        // SAFETY: the descriptor was produced by clack's own entry macro
        // for a plugin written with clack, so it is a compliant CLAP entry.
        let entry =
            unsafe { PluginEntry::load_from_raw(descriptor, c"scripted") }.map_err(|error| error.to_string())?;
        let records = super::probe::records(&entry)?;
        let name = records.first().map_or("empty", |record| record.id.as_str());
        let path = PathBuf::from(format!("<scripted plugin {name}>"));
        self.entries.insert(path.clone(), entry);
        Ok((path, records))
    }

    /// Loads, instantiates and activates a plugin, restoring `state` and
    /// then applying `values` on top of it.
    pub(crate) fn create(&mut self, request: CreateRequest) -> Result<Created, String> {
        let entry = self.entry(&request.path)?;
        let plugin_id = CString::new(request.plugin_id.as_str()).map_err(|_| "the plugin id contains a NUL byte")?;
        let mut plugin = PluginInstance::<TrillHost>::new(
            |_| HostShared::new(&request.plugin_id),
            |_| HostMainThread,
            &entry,
            &plugin_id,
            &self.host_info,
        )
        .map_err(|error| format!("cannot instantiate the plugin: {error}"))?;

        let handle = plugin.plugin_handle();
        let state: Option<PluginState> = handle.get_extension();
        let params: Option<PluginParams> = handle.get_extension();
        let latency: Option<PluginLatency> = handle.get_extension();

        // An empty state is what `save_state` never returns; treat it as
        // "nothing saved" rather than handing the plugin zero bytes.
        if let Some(bytes) = request.state.as_deref().filter(|bytes| !bytes.is_empty()) {
            match state {
                Some(state) => {
                    state.load(&handle, &mut Cursor::new(bytes)).map_err(|_| "the plugin rejected its saved state")?
                }
                None => log::warn!("[{}] has saved state but cannot load state; ignoring it", request.plugin_id),
            }
        }

        let mut specs = params.map(|params| read_params(params, &handle, &request.plugin_id)).unwrap_or_default();
        let initial_values = resolve_values(&specs, &request.values);
        let ports = read_ports(&handle);
        let notes = read_note_port(&handle);

        let mut activated = activate(&mut plugin, latency, request.sample_rate, request.max_block)?;
        if let Some(params) = params {
            // Only now are the values settled: some plugins fill in their
            // parameters while activating.
            read_baseline(params, &plugin.plugin_handle(), &mut specs);

            // The model's values go in while the plugin is inactive, and it
            // is then activated again. A plugin takes values it is given
            // before activation as its starting point; given later, it
            // glides to them, audibly, at the start of a render.
            if !initial_values.is_empty() {
                deactivate(&mut plugin, Some(activated.processor));
                if let Some(mut inactive) = plugin.inactive_plugin_handle() {
                    let events: Vec<ParamValueEvent> = initial_values
                        .iter()
                        .map(|&(index, value)| ParamValueEvent::new(0, specs[index].id, Pckn::match_all(), value))
                        .collect();
                    params.flush(&mut inactive, &InputEvents::from_buffer(&events), &mut OutputEvents::void());
                }
                activated = activate(&mut plugin, latency, request.sample_rate, request.max_block)?;
            }
        }

        let id = InstanceId(self.next_id);
        self.next_id += 1;
        let param_ids = specs.iter().map(|spec| spec.id).collect();
        self.instances.insert(id, Instance { plugin, state, params, latency, param_ids });
        Ok(Created { id, activated, params: specs, initial_values, ports, notes })
    }

    /// Deactivates and destroys an instance. `processor` is its audio
    /// processor, already stopped; `None` if it was lost to a failed
    /// re-activation.
    pub(crate) fn destroy(&mut self, id: InstanceId, processor: Option<StoppedPluginAudioProcessor<TrillHost>>) {
        let Some(mut instance) = self.instances.remove(&id) else { return };
        deactivate(&mut instance.plugin, processor);
        // Dropping the instance destroys the plugin. clack keeps it alive
        // (leaked) instead if an audio processor handle still exists, which
        // `deactivate` has just ruled out.
    }

    /// Re-activates an instance for a different sample rate or block size.
    /// On failure the instance stays deactivated.
    pub(crate) fn reactivate(
        &mut self,
        id: InstanceId,
        processor: Option<StoppedPluginAudioProcessor<TrillHost>>,
        sample_rate: f64,
        max_block: u32,
    ) -> Result<Activated, String> {
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        deactivate(&mut instance.plugin, processor);
        activate(&mut instance.plugin, instance.latency, sample_rate, max_block)
    }

    /// The plugin's complete state, or `None` if it has none to give.
    pub(crate) fn save_state(&mut self, id: InstanceId) -> Option<Vec<u8>> {
        let instance = self.instances.get_mut(&id)?;
        let state = instance.state?;
        let mut bytes = Vec::new();
        match state.save(&instance.plugin.plugin_handle(), &mut bytes) {
            Ok(()) => Some(bytes).filter(|bytes| !bytes.is_empty()),
            Err(error) => {
                let plugin = instance.plugin.access_shared_handler(|shared| shared.plugin_id().to_string());
                log::warn!("[{plugin}] {error}");
                None
            }
        }
    }

    /// The current value of every parameter, in parameter-list order.
    pub(crate) fn param_values(&mut self, id: InstanceId) -> Option<Vec<f64>> {
        let instance = self.instances.get_mut(&id)?;
        let params = instance.params?;
        let handle = instance.plugin.plugin_handle();
        instance.param_ids.iter().map(|&param| params.get_value(&handle, param)).collect()
    }
}

/// Returns the plugin to the inactive state, whatever state it was in.
fn deactivate(plugin: &mut PluginInstance<TrillHost>, processor: Option<StoppedPluginAudioProcessor<TrillHost>>) {
    match processor {
        // `PluginInstance::deactivate` panics on a processor of another
        // instance; the ids make that impossible, but a panic here would
        // skip the plugin's teardown, so check anyway.
        Some(processor) if processor.matches(plugin) => plugin.deactivate(processor),
        Some(processor) => {
            log::error!("a plugin audio processor was returned to the wrong instance");
            drop(processor);
        }
        None => {
            // Not active, or active with its processor handle already gone.
            let _ = plugin.try_deactivate();
        }
    }
}

fn activate(
    plugin: &mut PluginInstance<TrillHost>,
    latency: Option<PluginLatency>,
    sample_rate: f64,
    max_block: u32,
) -> Result<Activated, String> {
    if !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Err(format!("invalid sample rate {sample_rate}"));
    }
    // clack panics on a zero block size.
    let configuration =
        PluginAudioConfiguration { sample_rate, min_frames_count: 1, max_frames_count: max_block.max(1) };
    let processor = plugin
        .activate(|_, _| (), configuration)
        .map_err(|error| format!("cannot activate the plugin at {sample_rate} Hz: {error}"))?;
    // CLAP allows latency to be read only once the plugin is active.
    let latency = latency.map_or(0, |latency| latency.get(&plugin.plugin_handle()));
    Ok(Activated { processor, latency })
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// Reads the plugin's parameter list. Parameters the plugin fails to
/// describe, and repeated ids, are left out: the list defines the indices
/// the engine uses, so every entry must be addressable.
fn read_params(params: PluginParams, handle: &PluginMainThreadHandle, plugin_id: &str) -> Vec<ParamSpec> {
    struct Raw {
        id: ClapId,
        name: String,
        module: String,
        flags: ParamInfoFlags,
        range: (f64, f64, f64),
    }

    let mut seen = HashSet::new();
    let mut raw = Vec::new();
    let mut buffer = ParamInfoBuffer::new();
    for index in 0..params.count(handle) {
        let Some(info) = params.get_info(handle, index, &mut buffer) else {
            log::debug!("[{plugin_id}] does not describe parameter {index}; skipping it");
            continue;
        };
        if !seen.insert(info.id) {
            log::warn!("[{plugin_id}] reports parameter id {} twice; keeping the first", info.id);
            continue;
        }
        raw.push(Raw {
            id: info.id,
            name: lossy(info.name),
            module: lossy(info.module),
            flags: info.flags,
            range: (info.min_value, info.max_value, info.default_value),
        });
    }

    // Plugins reuse short names across modules ("Cutoff" in each filter).
    // Only those get their module path, so unambiguous names stay short.
    let mut uses: HashMap<&str, usize> = HashMap::new();
    for param in &raw {
        *uses.entry(param.name.as_str()).or_default() += 1;
    }
    raw.iter()
        .map(|param| {
            let ambiguous = uses.get(param.name.as_str()).is_some_and(|&count| count > 1);
            let module = param.module.trim_matches('/');
            let name = match (param.name.is_empty(), ambiguous && !module.is_empty()) {
                (true, _) => format!("Parameter {}", param.id),
                (false, true) => format!("{module}/{}", param.name),
                (false, false) => param.name.clone(),
            };
            let (min, max, default) = sanitize_range(param.range);
            ParamSpec {
                id: param.id,
                writable: !param.flags.contains(ParamInfoFlags::IS_READONLY),
                info: ParamInfo {
                    id: param.id.to_string(),
                    name,
                    min,
                    max,
                    default,
                    unit: String::new(),
                    kind: if param.flags.contains(ParamInfoFlags::IS_STEPPED) {
                        ParamKind::Int
                    } else {
                        ParamKind::Float
                    },
                },
            }
        })
        .collect()
}

/// Replaces each parameter's declared default with the value it has now:
/// with the saved state restored, and before any value from the model is
/// applied.
///
/// The model stores only the parameters the user has set. For all others
/// the engine assumes the default, both to display them and to return to
/// when a setting is undone. What a plugin declares as default cannot serve
/// as that: it says nothing about the patch that is loaded, and some
/// plugins declare 0 for every parameter.
fn read_baseline(params: PluginParams, handle: &PluginMainThreadHandle, specs: &mut [ParamSpec]) {
    for spec in specs {
        if let Some(value) = params.get_value(handle, spec.id).filter(|value| value.is_finite()) {
            spec.info.default = value.clamp(spec.info.min, spec.info.max);
        }
    }
}

/// Makes a plugin-reported range safe to clamp with: finite, `min <= max`,
/// and the default inside it. (`f64::clamp` panics otherwise.)
fn sanitize_range((min, max, default): (f64, f64, f64)) -> (f64, f64, f64) {
    let (min, max) = match (min.is_finite(), max.is_finite()) {
        (true, true) if min <= max => (min, max),
        (true, true) => (max, min),
        (true, false) => (min, min + 1.0),
        (false, true) => (max - 1.0, max),
        (false, false) => (0.0, 1.0),
    };
    let default = if default.is_finite() { default.clamp(min, max) } else { min };
    (min, max, default)
}

/// Maps model values (by CLAP id) to parameter indices, dropping ids the
/// plugin does not have and parameters it does not let the host set.
fn resolve_values(specs: &[ParamSpec], values: &[(u32, f64)]) -> Vec<(usize, f64)> {
    values
        .iter()
        .filter_map(|&(id, value)| {
            let index = specs.iter().position(|spec| spec.id.get() == id)?;
            let spec = &specs[index];
            (spec.writable && value.is_finite()).then(|| (index, value.clamp(spec.info.min, spec.info.max)))
        })
        .collect()
}

fn read_ports(handle: &PluginMainThreadHandle) -> PortLayout {
    // A plugin without the audio-ports extension has no audio ports.
    let Some(ports) = handle.get_extension::<PluginAudioPorts>() else { return PortLayout::default() };
    let channels = |is_input: bool| -> Vec<u32> {
        let mut buffer = AudioPortInfoBuffer::new();
        (0..ports.count(handle, is_input))
            // A port the plugin fails to describe still occupies its index
            // in `process`; give it no channels rather than shifting the
            // ports after it.
            .map(|index| ports.get(handle, index, is_input, &mut buffer).map_or(0, |port| port.channel_count))
            .collect()
    };
    PortLayout { inputs: channels(true), outputs: channels(false) }
}

/// How to send notes to the plugin's first note port, if it has one.
fn read_note_port(handle: &PluginMainThreadHandle) -> Option<NotePort> {
    let ports = handle.get_extension::<PluginNotePorts>()?;
    let mut buffer = NotePortInfoBuffer::new();
    let port = ports.get(handle, 0, true, &mut buffer)?;
    note_port(port.supported_dialects)
}

/// CLAP note events are preferred: they are what the engine's events map
/// to without loss. A port that speaks neither dialect gets no notes.
fn note_port(dialects: NoteDialects) -> Option<NotePort> {
    let midi = dialects.intersects(NoteDialects::MIDI | NoteDialects::MIDI_MPE);
    let dialect = if dialects.contains(NoteDialects::CLAP) {
        NoteDialect::Clap
    } else if midi {
        NoteDialect::Midi
    } else {
        return None;
    };
    Some(NotePort { dialect, midi })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: u32, min: f64, max: f64, writable: bool) -> ParamSpec {
        ParamSpec {
            id: ClapId::new(id),
            writable,
            info: ParamInfo {
                id: id.to_string(),
                name: format!("P{id}"),
                min,
                max,
                default: min,
                unit: String::new(),
                kind: ParamKind::Float,
            },
        }
    }

    #[test]
    fn ranges_from_plugins_are_made_safe() {
        assert_eq!(sanitize_range((0.0, 10.0, 5.0)), (0.0, 10.0, 5.0));
        assert_eq!(sanitize_range((10.0, 0.0, 5.0)), (0.0, 10.0, 5.0), "a reversed range is swapped");
        assert_eq!(sanitize_range((0.0, 1.0, 7.0)), (0.0, 1.0, 1.0), "the default is pulled into range");
        assert_eq!(sanitize_range((0.0, 1.0, f64::NAN)), (0.0, 1.0, 0.0));
        assert_eq!(sanitize_range((f64::NAN, f64::INFINITY, 0.5)), (0.0, 1.0, 0.5));
        assert_eq!(sanitize_range((2.0, f64::INFINITY, 0.0)), (2.0, 3.0, 2.0));
        assert_eq!(sanitize_range((f64::NEG_INFINITY, 2.0, 0.0)), (1.0, 2.0, 1.0));
        assert_eq!(sanitize_range((3.0, 3.0, 3.0)), (3.0, 3.0, 3.0), "a constant parameter is legal");

        // Whatever comes out must be usable with `ParamInfo::clamp`.
        for range in [(5.0, -5.0, f64::NAN), (f64::NAN, f64::NAN, f64::NAN), (1.0, 1.0, 9.0)] {
            let (min, max, default) = sanitize_range(range);
            assert!(min <= max && (min..=max).contains(&default), "{range:?}");
        }
    }

    #[test]
    fn note_ports_prefer_clap_events() {
        let both = NoteDialects::CLAP | NoteDialects::MIDI;
        assert_eq!(note_port(both), Some(NotePort { dialect: NoteDialect::Clap, midi: true }));
        assert_eq!(note_port(NoteDialects::CLAP), Some(NotePort { dialect: NoteDialect::Clap, midi: false }));
        assert_eq!(note_port(NoteDialects::MIDI_MPE), Some(NotePort { dialect: NoteDialect::Midi, midi: true }));
        assert_eq!(note_port(NoteDialects::MIDI2), None);
        assert_eq!(note_port(NoteDialects::empty()), None);
    }

    #[test]
    fn model_values_map_to_indices_and_are_clamped() {
        let specs = [spec(10, 0.0, 1.0, true), spec(20, -12.0, 12.0, true), spec(30, 0.0, 1.0, false)];
        let values = [(20, 100.0), (99, 0.5), (30, 0.5), (10, 0.25), (10, f64::NAN)];
        assert_eq!(resolve_values(&specs, &values), [(1, 12.0), (0, 0.25)]);
        assert!(resolve_values(&specs, &[]).is_empty());
    }
}
