//! The main-thread half of the host: everything CLAP requires to happen on
//! "the main thread". [`ClapThread`] is the state of the plugin main thread;
//! it owns the loaded libraries and every plugin instance, and is only ever
//! touched by jobs running on that thread.

use super::events::{NoteDialect, NotePort, ParamFeedback, ParamOutput};
use super::gui;
use super::host::{AudioContext, HostMainThread, HostShared, TrillHost};
use super::presets::{self, Address};
use crate::editor::{EditorStatus, HostedEditor, Placement};
use crate::exclusive::Exclusive;
use crate::main_thread::ThreadState;
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::PluginGui;
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::preset_discovery::PluginPresetLoad;
use clack_extensions::state::PluginState;
use clack_extensions::timer::{PluginTimer, TimerId};
use clack_host::events::Pckn;
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::*;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{ParamInfo, ParamKind};

/// How long a plugin's request for a flush may go unanswered by a block
/// before the main thread takes the plugin to be idle and flushes instead.
///
/// Long, on purpose. Nothing tells the main thread that a device is not
/// being processed (switched off, or the engine at rest); it can only
/// conclude so from blocks not coming. An audio thread that is merely
/// late must not find its plugin taken, so the wait is far longer than
/// any hiccup an engine recovers from. What it costs is that an edit made
/// in the editor of a plugin that is not playing is reported a second
/// late; the plugin itself has it at once.
const FLUSH_GRACE: Duration = Duration::from_secs(1);

/// The standard CLAP extensions a plugin may implement that this host
/// makes no use of yet, by their identifiers.
const UNUSED_EXTENSIONS: [&CStr; 16] = [
    c"clap.ambisonic/3",
    c"clap.audio-ports-activation/2",
    c"clap.audio-ports-config",
    c"clap.configurable-audio-ports/1",
    c"clap.context-menu/1",
    c"clap.note-name",
    c"clap.param-indication/4",
    c"clap.posix-fd-support",
    c"clap.remote-controls/2",
    c"clap.render",
    c"clap.state-context/2",
    c"clap.surround/4",
    c"clap.tail",
    c"clap.thread-pool",
    c"clap.track-info/1",
    c"clap.voice-info",
];

/// Identifies a plugin instance on the plugin main thread. Never reused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstanceId(u64);

/// A plugin's audio processor, where both halves of an instance can reach
/// it, one at a time and without waiting. It belongs to the audio thread,
/// which processes with it on every block. The main thread takes it to
/// start and stop the plugin, and borrows it to flush parameter changes
/// when no block is coming. `None` while the plugin is not active.
pub(crate) type AudioSlot = Exclusive<Option<PluginAudioProcessor<TrillHost>>>;

/// What to instantiate, and the model state to bring it up with.
pub(crate) struct CreateRequest {
    pub path: PathBuf,
    pub plugin_id: String,
    /// The plugin's name, for the title of its editor's window.
    pub name: String,
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
struct Activated {
    processor: StoppedPluginAudioProcessor<TrillHost>,
    latency: u32,
}

/// A freshly created, activated instance, ready for the audio thread.
pub(crate) struct Created {
    pub id: InstanceId,
    pub audio: Arc<AudioSlot>,
    pub latency: u32,
    /// In the order the processor's `set_param` indexes them.
    pub params: Vec<ParamSpec>,
    /// `(parameter index, value)` for the model values that were applied.
    pub initial_values: Vec<(usize, f64)>,
    pub ports: PortLayout,
    /// How the plugin takes notes, if it takes any.
    pub notes: Option<NotePort>,
    /// Where the plugin's own parameter edits are reported.
    pub feedback: Arc<ParamFeedback>,
    pub editor: Arc<EditorStatus>,
}

pub(super) struct Instance {
    pub(super) plugin: PluginInstance<TrillHost>,
    /// The plugin's id, for log messages.
    pub(super) plugin_id: String,
    /// The plugin's name, for the title of its editor's window.
    pub(super) name: String,
    state: Option<PluginState>,
    params: Option<PluginParams>,
    latency: Option<PluginLatency>,
    pub(super) gui: Option<PluginGui>,
    timer: Option<PluginTimer>,
    preset_load: Option<PluginPresetLoad>,
    /// In parameter-list order.
    param_ids: Vec<ClapId>,
    audio: Arc<AudioSlot>,
    feedback: Arc<ParamFeedback>,
    /// Since when the plugin has been waiting for a flush that no block
    /// has answered.
    flush_waiting: Option<Instant>,
    pub(super) editor: Option<HostedEditor>,
    pub(super) editor_status: Arc<EditorStatus>,
    /// Set once the editor has to be closed, until it has been.
    pub(super) editor_closing: bool,
    /// Where the editor's window was when it was last closed.
    pub(super) placement: Option<Placement>,
}

impl Instance {
    /// Serves what the plugin asked for since the last call, and whatever
    /// else is due. `nested` is [`ThreadState::idle`]'s.
    fn idle(&mut self, now: Instant, nested: bool, due_timers: &mut Vec<TimerId>) {
        let (callback, restart) =
            self.plugin.access_shared_handler(|shared| (shared.take_callback_request(), shared.take_restart_request()));
        if callback {
            self.plugin.call_on_main_thread_callback();
        }
        if restart {
            log::info!(
                "[{}] asked to be restarted; changes to its latency, ports or parameter list take effect when \
                 the device is next created",
                self.plugin_id
            );
        }
        self.tick_timers(now, due_timers);
        self.flush_if_unanswered(now);
        self.service_editor(nested);
    }

    /// Calls the plugin for each of its timers that is due. `due` is
    /// scratch space, so that idling does not allocate.
    fn tick_timers(&mut self, now: Instant, due: &mut Vec<TimerId>) {
        let Some(timer) = self.timer else { return };
        due.clear();
        self.plugin.access_handler(|handler| handler.take_due_timers(now, due));
        for &id in due.iter() {
            // The tick of one timer may have unregistered the next.
            if self.plugin.access_handler(|handler| handler.has_timer(id)) {
                timer.on_timer(&self.plugin.plugin_handle(), id);
            }
        }
    }

    /// Flushes the plugin's parameter changes if it asked for that and no
    /// block has come along to do it.
    ///
    /// A plugin whose editor changed a parameter tells the host by an
    /// event in its next block. If it is not being processed (its device
    /// is switched off, or nothing is driving the engine), it asks for a
    /// flush instead, which CLAP wants made on the audio thread while the
    /// plugin is active. With no block coming, this thread plays that
    /// part, for as long as it holds the audio processor.
    fn flush_if_unanswered(&mut self, now: Instant) {
        let Some(params) = self.params else { return };
        if !self.plugin.access_shared_handler(HostShared::flush_requested) {
            self.flush_waiting = None;
            return;
        }
        if now.duration_since(*self.flush_waiting.get_or_insert(now)) < FLUSH_GRACE {
            return;
        }
        self.flush_waiting = None;
        // Before the flush, so that a request made during it stands.
        self.plugin.access_shared_handler(HostShared::flush_served);
        let mut output = ParamOutput(&self.feedback);
        let flushed = self.audio.try_with(|audio| {
            let Some(audio) = audio else { return false };
            let _audio_thread = AudioContext::enter();
            let mut output = OutputEvents::from_buffer(&mut output);
            params.flush_active(&mut audio.plugin_handle(), &InputEvents::empty(), &mut output);
            true
        });
        // Turned away: the audio thread is processing after all, and its
        // block is the flush. Not active: the main thread flushes as
        // itself.
        if flushed == Some(false)
            && let Some(mut inactive) = self.plugin.inactive_plugin_handle()
        {
            params.flush(&mut inactive, &InputEvents::empty(), &mut OutputEvents::from_buffer(&mut output));
        }
    }

    /// Returns the plugin to the inactive state, whatever state it was in.
    /// Whoever asks for this has stopped processing the plugin.
    fn deactivate(&mut self) {
        let processor = self.audio.try_with(Option::take).flatten();
        // `stop_processing` is an audio-thread function. Nothing else is
        // processing this plugin, so this thread is its audio thread for
        // the duration.
        let stopped = processor.map(|processor| {
            let _audio_thread = AudioContext::enter();
            processor.into_stopped()
        });
        deactivate(&mut self.plugin, stopped);
    }
}

impl Drop for Instance {
    /// Takes the plugin down in the order CLAP requires: its editor goes,
    /// it stops processing and is deactivated, and only then is the
    /// instance itself dropped, which destroys the plugin. clack keeps a
    /// plugin alive (leaked) if an audio processor handle still exists,
    /// which deactivating has just ruled out.
    fn drop(&mut self) {
        self.close_editor();
        self.deactivate();
    }
}

/// The state of the plugin main thread.
pub(crate) struct ClapThread {
    host_info: HostInfo,
    /// Libraries stay loaded once used: reloading one for every instance is
    /// slow, and plugins are rarely tested against repeated unloading.
    entries: HashMap<PathBuf, PluginEntry>,
    pub(super) instances: HashMap<InstanceId, Instance>,
    next_id: u64,
    /// Scratch space for the timers that are due in one round of idling.
    due_timers: Vec<TimerId>,
    /// The plugins whose unused abilities have been logged, so that each
    /// is listed once and not for every instance.
    described: HashSet<String>,
}

/// How this host introduces itself to plugins, and to their preset
/// providers.
pub(crate) fn host_info() -> HostInfo {
    let version = CString::new(env!("CARGO_PKG_VERSION")).unwrap_or_default();
    HostInfo::new_from_cstring(c"Trill".into(), c"Trill".into(), c"".into(), version)
}

impl ThreadState for ClapThread {
    fn new() -> Self {
        Self {
            host_info: host_info(),
            entries: HashMap::new(),
            instances: HashMap::new(),
            next_id: 1,
            due_timers: Vec::new(),
            described: HashSet::new(),
        }
    }

    fn wants_idle(&self) -> bool {
        !self.instances.is_empty()
    }

    /// Serves the requests plugins left in their [`HostShared`] flags,
    /// their timers and their editors.
    fn idle(&mut self, nested: bool) {
        let now = Instant::now();
        let Self { instances, due_timers, .. } = self;
        for instance in instances.values_mut() {
            instance.idle(now, nested, due_timers);
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
            |shared| HostMainThread::new(shared),
            &entry,
            &plugin_id,
            &self.host_info,
        )
        .map_err(|error| format!("cannot instantiate the plugin: {error}"))?;

        let handle = plugin.plugin_handle();
        let state: Option<PluginState> = handle.get_extension();
        let params: Option<PluginParams> = handle.get_extension();
        let latency: Option<PluginLatency> = handle.get_extension();
        let gui: Option<PluginGui> = handle.get_extension();
        let timer: Option<PluginTimer> = handle.get_extension();
        let preset_load: Option<PluginPresetLoad> = handle.get_extension();
        let has_editor = gui.is_some_and(|gui| gui::is_embeddable(gui, &handle));
        if self.described.insert(request.plugin_id.clone()) {
            log_unused_extensions(&handle, &request.plugin_id);
        }

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
        let param_ids: Vec<ClapId> = specs.iter().map(|spec| spec.id).collect();
        let feedback = Arc::new(ParamFeedback::new(&param_ids));
        let audio = Arc::new(Exclusive::new(Some(activated.processor.into())));
        let editor_status = EditorStatus::new(has_editor);
        let instance = Instance {
            plugin,
            plugin_id: request.plugin_id,
            name: request.name,
            state,
            params,
            latency,
            gui,
            timer,
            preset_load,
            param_ids,
            audio: audio.clone(),
            feedback: feedback.clone(),
            flush_waiting: None,
            editor: None,
            editor_status: editor_status.clone(),
            editor_closing: false,
            placement: None,
        };
        self.instances.insert(id, instance);
        Ok(Created {
            id,
            audio,
            latency: activated.latency,
            params: specs,
            initial_values,
            ports,
            notes,
            feedback,
            editor: editor_status,
        })
    }

    /// Closes the editor of an instance, deactivates and destroys it. Its
    /// audio half must have stopped processing it.
    pub(crate) fn destroy(&mut self, id: InstanceId) {
        // Dropping the instance is what takes the plugin down.
        self.instances.remove(&id);
    }

    /// Re-activates an instance, whose audio half must have stopped
    /// processing it, for a sample rate and block size. Returns its
    /// latency. On failure the instance stays deactivated.
    pub(crate) fn reactivate(&mut self, id: InstanceId, sample_rate: f64, max_block: u32) -> Result<u32, String> {
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        instance.deactivate();
        let Activated { processor, latency } =
            activate(&mut instance.plugin, instance.latency, sample_rate, max_block)?;
        // Dropping a processor that could not be handed over would leave
        // the plugin active with no way to process it; deactivating again
        // is the way out of that.
        if instance.audio.try_with(|audio| *audio = Some(processor.into())).is_none() {
            instance.deactivate();
            return Err("the plugin's audio processor is in use".to_string());
        }
        Ok(latency)
    }

    /// The plugin's complete state, or `None` if it has none to give.
    pub(crate) fn save_state(&mut self, id: InstanceId) -> Option<Vec<u8>> {
        let instance = self.instances.get_mut(&id)?;
        let state = instance.state?;
        let mut bytes = Vec::new();
        match state.save(&instance.plugin.plugin_handle(), &mut bytes) {
            Ok(()) => Some(bytes).filter(|bytes| !bytes.is_empty()),
            Err(error) => {
                log::warn!("[{}] {error}", instance.plugin_id);
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

    /// Has the providers of the plugin file at `path` tell of the presets
    /// of the plugin `plugin_id`, here on the plugin main thread: what is
    /// done when there is no scanner process to do it in.
    pub(crate) fn read_presets(
        &mut self,
        path: &Path,
        plugin_id: &str,
        sink: &mut dyn crate::presets::Sink,
    ) -> Result<(), String> {
        let entry = self.entry(path)?;
        presets::discover(&entry, &self.host_info, plugin_id, sink)
    }

    /// Has an instance load the preset at `address`, as it plays. An
    /// error is the plugin's refusal, in its own words where it gave any.
    pub(crate) fn load_preset(&mut self, id: InstanceId, address: &Address) -> Result<(), String> {
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        let load = instance
            .preset_load
            .ok_or("the plugin cannot be given a preset by its host (it has no CLAP preset-load extension)")?;
        // Whatever it said about an earlier preset is not about this one.
        instance.plugin.access_handler(|handler| handler.take_preset_error());
        let loaded = load.load_from_location(&instance.plugin.plugin_handle(), address.location(), address.key());
        let said = instance.plugin.access_handler(|handler| handler.take_preset_error());
        match (loaded, said) {
            (Ok(()), _) => Ok(()),
            (Err(_), Some(reason)) => Err(format!("the plugin could not load it: {reason}")),
            (Err(_), None) => Err("the plugin refused it without saying why".to_string()),
        }
    }
}

/// Logs which of the standard extensions this host makes no use of the
/// plugin implements: what the plugin could do that it is not asked to.
///
/// This is the nearest thing to a list of unmet requests that can be had.
/// What a plugin asks *of the host* and does not get cannot be listed: the
/// bindings answer those queries without saying which they were.
fn log_unused_extensions(plugin: &PluginMainThreadHandle, plugin_id: &str) {
    let Some(get_extension) = plugin.as_raw().get_extension else { return };
    let offered: Vec<&str> = UNUSED_EXTENSIONS
        .iter()
        // SAFETY: the plugin is live, this is its main thread, and the
        // identifier is a C string constant. The extension is only asked
        // for; what the plugin returns is not dereferenced.
        .filter(|id| !unsafe { get_extension(plugin.as_raw_ptr(), id.as_ptr()) }.is_null())
        // "clap.tail" and "clap.surround/4" alike: the name without the
        // prefix and the version.
        .filter_map(|id| id.to_str().ok()?.strip_prefix("clap.")?.split('/').next())
        .collect();
    if !offered.is_empty() {
        log::info!("[{plugin_id}] also implements {}, which this host does not use yet", offered.join(", "));
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
