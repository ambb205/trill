//! The main-thread half of the host: everything VST3 requires to happen on
//! "the UI thread". [`Vst3Thread`] is the VST3 part of the plugin main
//! thread's state; it owns the loaded modules and every plugin instance,
//! and is only ever touched by jobs running on that thread.

use super::buffers::BusLayout;
use super::class_id::ClassId;
use super::com::{create_instance, interface, result_name};
use super::editor::Editor;
use super::host::{ComponentHandler, HostApplication};
use super::links::ParamLinks;
use super::module::Module;
use super::params::{self, ParamSpec};
use super::presets;
use super::quirks;
use super::state::{self, PluginState};
use super::stream::MemoryStream;
use crate::editor::{EditorStatus, Placement};
use crate::main_thread::ThreadState;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ProcessModes_::{kOffline, kRealtime};
use vst3::Steinberg::Vst::RestartFlags_::{kParamTitlesChanged, kParamValuesChanged};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    BusDirection, BusInfo, IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentTrait, IConnectionPoint,
    IConnectionPointTrait, IEditController, IEditControllerTrait, MediaType, ParamID, ProcessSetup, SpeakerArr,
    SpeakerArrangement,
};
use vst3::Steinberg::{FUnknown, IPluginBaseTrait, IPluginFactory3, IPluginFactory3Trait, TUID, int32, kResultOk};
use vst3::{ComPtr, ComWrapper};

/// The most audio buses read from a plugin in one direction, and the most
/// channels in a bus (a VST3 speaker arrangement has 64 bits). A plugin
/// reporting more is reporting nonsense, which must not be turned into
/// buffers.
const MAX_BUSES: int32 = 256;
const MAX_CHANNELS: int32 = 64;

/// Identifies a plugin instance on the plugin main thread. Never reused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstanceId(u64);

#[cfg(test)]
impl InstanceId {
    /// An id for a test that needs one and has no instance.
    pub(crate) fn for_tests(id: u64) -> Self {
        Self(id)
    }
}

/// What to instantiate, and the model state to bring it up with.
pub(crate) struct CreateRequest {
    /// The module's library.
    pub path: PathBuf,
    /// The processor class to instantiate.
    pub class: ClassId,
    /// The plugin's name, for messages.
    pub name: String,
    /// Opaque state from an earlier [`Vst3Thread::save_state`].
    pub state: Option<Vec<u8>>,
    /// Parameter values by VST3 parameter id, in plain units, to apply on
    /// top of `state`.
    pub values: Vec<(ParamID, f64)>,
    pub sample_rate: f64,
    pub max_block: u32,
    /// The plugin is made for a render, not to be played: it is set up
    /// for offline processing.
    pub offline: bool,
    /// The audio input bus that is switched on beside the first, for a
    /// device that listens to another track: its key input.
    pub key_bus: Option<usize>,
}

/// The VST3 process mode of a plugin that is rendered (`offline`) or played.
pub(crate) fn process_mode(offline: bool) -> int32 {
    if offline { kOffline as int32 } else { kRealtime as int32 }
}

/// A freshly created, activated instance, ready for the audio thread.
pub(crate) struct Created {
    pub id: InstanceId,
    /// The plugin's processing interface. It belongs to the audio-thread
    /// half, which hands it back to [`Vst3Thread::destroy`].
    pub audio: ComPtr<IAudioProcessor>,
    pub latency: u32,
    /// In the order the processor's `set_param` indexes them.
    pub params: Vec<ParamSpec>,
    /// `(parameter index, plain value)` for the model values to apply.
    pub initial_values: Vec<(usize, f64)>,
    pub buses: BusLayout,
    /// Whether the plugin has an event bus to send notes to.
    pub takes_notes: bool,
    /// The parameters that stand for MIDI's "all sound off".
    pub silencing: Vec<ParamID>,
    /// The parameters that stand for the pedal, the wheels and the other
    /// MIDI controllers.
    pub midi: params::MidiParams,
    pub links: Arc<ParamLinks>,
    pub editor: Arc<EditorStatus>,
    /// Whether the plugin was set up for offline processing; each block
    /// then says so too.
    pub offline: bool,
}

/// A plugin's edit controller.
pub(super) struct Controller {
    pub(super) edit: ComPtr<IEditController>,
    /// Whether the controller is an object of its own rather than another
    /// interface of the component: one that is initialized and terminated
    /// separately.
    separate: bool,
    /// The connection points of the component and of the controller,
    /// through which the two talk to each other once connected.
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
}

/// One plugin: its component, and everything attached to it. Dropping it
/// takes the plugin down in the order VST3 requires.
pub(super) struct Instance {
    pub(super) name: String,
    class: ClassId,
    pub(super) controller: Option<Controller>,
    pub(super) editor: Option<Editor>,
    pub(super) editor_status: Arc<EditorStatus>,
    /// Set once the editor has to be closed, until it has been.
    pub(super) editor_closing: bool,
    /// Where the editor's window was when it was last closed.
    pub(super) placement: Option<Placement>,
    handler: ComWrapper<ComponentHandler>,
    /// Set once the instance is fully created.
    links: Option<Arc<ParamLinks>>,
    /// The parameters the device lists, in its order.
    listed: Vec<ParamSpec>,
    active: bool,
    /// Set up for offline processing: see [`CreateRequest::offline`].
    offline: bool,
    /// When it was made, or last handed a state: what a plugin may go on
    /// working at behind the scenes for a while ([`quirks`]). Moved on by
    /// the time the plugin thread has since been kept from its windows,
    /// which the plugin has not had ([`Vst3Thread::served`]).
    settled: Instant,
    processor: ComPtr<IAudioProcessor>,
    /// Declared last: the component is the plugin, and is released after
    /// every other reference to it.
    component: ComPtr<IComponent>,
}

impl Instance {
    /// Creates and initializes the component of `class`.
    pub(super) fn new(
        module: &Module,
        host: &ComWrapper<HostApplication>,
        class: ClassId,
        name: &str,
    ) -> Result<Self, String> {
        let component: ComPtr<IComponent> =
            create_instance(module.factory(), class).ok_or("the module cannot create the plugin")?;
        let processor = component.cast::<IAudioProcessor>().ok_or("the plugin does not process audio")?;
        // SAFETY: the component was just created and is initialized once,
        // on the plugin main thread, with a host context that outlives it.
        let result = unsafe { component.initialize(interface::<_, FUnknown>(host)) };
        if result != kResultOk {
            return Err(format!("the plugin failed to initialize ({})", result_name(result)));
        }
        // From here on, dropping the instance terminates the component.
        Ok(Self {
            name: name.to_string(),
            class,
            controller: None,
            editor: None,
            editor_status: EditorStatus::new(false),
            editor_closing: false,
            placement: None,
            handler: ComponentHandler::new(),
            links: None,
            listed: Vec::new(),
            active: false,
            offline: false,
            settled: Instant::now(),
            processor,
            component,
        })
    }

    /// Finds the plugin's edit controller, tells it who its host is and
    /// connects it to the component. A plugin whose controller cannot be
    /// had is hosted without one: it processes audio, but has no
    /// parameters.
    pub(super) fn attach_controller(&mut self, module: &Module, host: &ComWrapper<HostApplication>) {
        // The component of a simple plugin is its own controller.
        let (edit, separate) = match self.component.cast::<IEditController>() {
            Some(edit) => (edit, false),
            None => match self.create_controller(module, host) {
                Some(edit) => (edit, true),
                None => return,
            },
        };
        // SAFETY: the controller is live and initialized, and the handler
        // outlives it: the instance clears it again before either goes.
        unsafe { edit.setComponentHandler(interface(&self.handler)) };

        // Two objects that have not met: they are introduced to each other
        // through their connection points, if both have one.
        let connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)> =
            separate.then(|| self.component.cast().zip(edit.cast())).flatten();
        if let Some((component, controller)) = &connection {
            // SAFETY: both are live connection points of this plugin, each
            // given the other for as long as they stay connected.
            unsafe {
                component.connect(controller.as_ptr());
                controller.connect(component.as_ptr());
            }
        }
        self.controller = Some(Controller { edit, separate, connection });
        // The controller is what makes the plugin's view. Whether it will
        // is only known once it has been asked to.
        self.editor_status.set_available(true);
    }

    /// Creates and initializes the controller class the component names.
    fn create_controller(
        &self,
        module: &Module,
        host: &ComWrapper<HostApplication>,
    ) -> Option<ComPtr<IEditController>> {
        let mut class: TUID = [0; 16];
        // SAFETY: the component is live; `class` is a local it fills in.
        let named = unsafe { self.component.getControllerClassId(&mut class) } == kResultOk;
        let class = ClassId::from_tuid(&class);
        if !named || class.is_nil() {
            log::debug!("[{}] names no edit controller; it has no parameters", self.name);
            return None;
        }
        let Some(edit) = create_instance::<IEditController>(module.factory(), class) else {
            log::warn!("[{}] its edit controller ({class}) cannot be created; it has no parameters", self.name);
            return None;
        };
        // SAFETY: the controller was just created and is initialized once,
        // on the plugin main thread, with a host context that outlives it.
        let result = unsafe { edit.initialize(interface::<_, FUnknown>(host)) };
        if result != kResultOk {
            log::warn!("[{}] its edit controller failed to initialize ({})", self.name, result_name(result));
            return None;
        }
        Some(edit)
    }

    /// The component's state, if it has any to give.
    fn component_state(&self) -> Option<Vec<u8>> {
        let stream = MemoryStream::new(Vec::new());
        // SAFETY: the component is live and the stream outlives the call.
        let result = unsafe { self.component.getState(interface(&stream)) };
        (result == kResultOk).then(|| stream.take())
    }

    /// The controller's own state, if it has any to give.
    fn controller_state(&self) -> Option<Vec<u8>> {
        let controller = self.controller.as_ref()?;
        let stream = MemoryStream::new(Vec::new());
        // SAFETY: the controller is live and the stream outlives the call.
        let result = unsafe { controller.edit.getState(interface(&stream)) };
        (result == kResultOk).then(|| stream.take())
    }

    /// Restores `saved`, the bytes of an earlier [`Vst3Thread::save_state`],
    /// or with nothing saved brings the controller in line with the state
    /// the component starts out in.
    fn restore(&mut self, saved: Option<&[u8]>) -> Result<(), String> {
        // An empty state is what `save_state` never returns; treat it as
        // "nothing saved" rather than handing the plugin zero bytes.
        let state = match saved.filter(|bytes| !bytes.is_empty()) {
            Some(bytes) => {
                self.settled = Instant::now();
                let (class, state) = state::decode(bytes)?;
                if class != self.class {
                    return Err(format!("the saved state belongs to another plugin ({class})"));
                }
                if !state.component.is_empty() {
                    let stream = MemoryStream::new(state.component.clone());
                    // SAFETY: the component is live, and the stream
                    // outlives the call. The plugin is inactive when it
                    // is being made, and may be active and processing
                    // when it is handed a preset: VST3 lets a state be
                    // set then, from this thread, and leaves it to the
                    // plugin to bring it over to its audio side.
                    let result = unsafe { self.component.setState(interface(&stream)) };
                    if result != kResultOk {
                        return Err(format!("the plugin rejected its saved state ({})", result_name(result)));
                    }
                }
                state
            }
            None => PluginState { component: self.component_state().unwrap_or_default(), controller: Vec::new() },
        };

        let Some(controller) = &self.controller else { return Ok(()) };
        // A controller keeps its own copy of the parameter values. It is
        // told the component's state so that the copy matches; whether it
        // has use for that is up to the plugin, so its answer is not
        // checked.
        if !state.component.is_empty() {
            let stream = MemoryStream::new(state.component);
            // SAFETY: the controller is live and the stream outlives the call.
            unsafe { controller.edit.setComponentState(interface(&stream)) };
        }
        if !state.controller.is_empty() {
            let stream = MemoryStream::new(state.controller);
            // SAFETY: as above.
            let result = unsafe { controller.edit.setState(interface(&stream)) };
            if result != kResultOk {
                log::warn!("[{}] its editor's saved state was not restored ({})", self.name, result_name(result));
            }
        }
        Ok(())
    }

    fn bus_count(&self, media: MediaType, direction: BusDirection) -> int32 {
        // SAFETY: the component is live.
        unsafe { self.component.getBusCount(media, direction) }.clamp(0, MAX_BUSES)
    }

    /// Asks for a stereo main bus in each direction and reports the
    /// channel count of every audio bus the plugin ends up with.
    ///
    /// A plugin is free to refuse (one that is mono stays mono), so the
    /// result is whatever it says afterwards. Only the main buses are
    /// switched on; side-chains and further outputs stay off.
    fn negotiate_buses(&mut self, key_bus: Option<usize>) -> BusLayout {
        let (audio, input, output) = (kAudio as MediaType, kInput as BusDirection, kOutput as BusDirection);
        let mut arrangements = [input, output].map(|direction| {
            (0..self.bus_count(audio, direction))
                .map(|index| {
                    let mut arrangement: SpeakerArrangement = SpeakerArr::kEmpty;
                    // SAFETY: the processor is live and `arrangement` a
                    // local it fills in. A bus that does not answer keeps
                    // the empty arrangement.
                    unsafe { self.processor.getBusArrangement(direction, index, &mut arrangement) };
                    if index == 0 { SpeakerArr::kStereo } else { arrangement }
                })
                .collect::<Vec<SpeakerArrangement>>()
        });
        let [inputs, outputs] = &mut arrangements;
        if !inputs.is_empty() || !outputs.is_empty() {
            // SAFETY: each pointer is to as many arrangements as the count
            // passed with it says, and the plugin is inactive, which is
            // when its buses may be arranged.
            unsafe {
                self.processor.setBusArrangements(
                    inputs.as_mut_ptr(),
                    inputs.len() as int32,
                    outputs.as_mut_ptr(),
                    outputs.len() as int32,
                );
            }
        }

        let channels = |direction: BusDirection| -> Vec<u32> {
            (0..self.bus_count(audio, direction))
                .map(|index| {
                    let mut info =
                        BusInfo { mediaType: audio, direction, channelCount: 0, name: [0; 128], busType: 0, flags: 0 };
                    // SAFETY: the component is live; `info` is a local it
                    // fills in, and switching buses is done while inactive.
                    unsafe {
                        // A bus the plugin fails to describe still has its
                        // place in `process`; it gets no channels.
                        let described = self.component.getBusInfo(audio, direction, index, &mut info) == kResultOk;
                        // The first bus each way, and the input a key goes
                        // into when the device is to listen to one. No
                        // other: a plugin may take a side-chain that is
                        // switched on to be the one it listens to, and
                        // would then be listening to nothing.
                        let key = direction == input && key_bus == Some(index as usize);
                        self.component.activateBus(audio, direction, index, u8::from(index == 0 || key));
                        if described { info.channelCount.clamp(0, MAX_CHANNELS) as u32 } else { 0 }
                    }
                })
                .collect()
        };
        BusLayout { inputs: channels(input), outputs: channels(output) }
    }

    /// Switches on the plugin's first event input, if it has one, and
    /// returns whether it does.
    fn enable_notes(&mut self) -> bool {
        let takes_notes = self.bus_count(kEvent as MediaType, kInput as BusDirection) > 0;
        if takes_notes {
            // SAFETY: the component is live and inactive.
            unsafe { self.component.activateBus(kEvent as MediaType, kInput as BusDirection, 0, 1) };
        }
        takes_notes
    }

    /// How many MIDI channels the plugin's first event input has: one for
    /// a plugin that has no such input or does not say.
    fn note_channels(&self) -> usize {
        let (media, direction) = (kEvent as MediaType, kInput as BusDirection);
        if self.bus_count(media, direction) == 0 {
            return 1;
        }
        let mut info = BusInfo { mediaType: media, direction, channelCount: 0, name: [0; 128], busType: 0, flags: 0 };
        // SAFETY: the component is live; `info` is a local it fills in.
        let described = unsafe { self.component.getBusInfo(media, direction, 0, &mut info) } == kResultOk;
        if described { info.channelCount.clamp(1, trill_core::Channel::COUNT as int32) as usize } else { 1 }
    }

    /// Tells the inactive plugin how it will be processed.
    fn setup(&mut self, sample_rate: f64, max_block: u32) -> Result<(), String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(format!("invalid sample rate {sample_rate}"));
        }
        // SAFETY: the processor is live.
        if unsafe { self.processor.canProcessSampleSize(kSample32 as int32) } != kResultOk {
            return Err("the plugin cannot process 32-bit float audio".to_string());
        }
        let mut setup = ProcessSetup {
            processMode: process_mode(self.offline),
            symbolicSampleSize: kSample32 as int32,
            maxSamplesPerBlock: int32::try_from(max_block.max(1)).unwrap_or(int32::MAX),
            sampleRate: sample_rate,
        };
        // SAFETY: the processor is live and inactive; `setup` is a local.
        let result = unsafe { self.processor.setupProcessing(&mut setup) };
        if result != kResultOk {
            return Err(format!("cannot set the plugin up for {sample_rate} Hz ({})", result_name(result)));
        }
        Ok(())
    }

    /// Activates the plugin and returns its latency, which is only settled
    /// once it is active.
    fn activate(&mut self) -> Result<u32, String> {
        // SAFETY: the component is live, set up and inactive.
        let result = unsafe { self.component.setActive(1) };
        if result != kResultOk {
            return Err(format!("cannot activate the plugin ({})", result_name(result)));
        }
        self.active = true;
        // SAFETY: the processor is live.
        Ok(unsafe { self.processor.getLatencySamples() })
    }

    fn deactivate(&mut self) {
        if std::mem::take(&mut self.active) {
            // SAFETY: the component is live and active, and its audio half
            // has stopped processing.
            unsafe { self.component.setActive(0) };
        }
    }

    /// Tells the controller the values the processor has taken on since
    /// the last call.
    fn sync_controller(&self) {
        let (Some(links), Some(controller)) = (&self.links, &self.controller) else { return };
        links.to_controller.collect(|slot, value| {
            if let Some(id) = links.id(slot) {
                // SAFETY: the controller is live, on the plugin main thread.
                unsafe { controller.edit.setParamNormalized(id, value) };
            }
        });
    }

    /// Reports what the plugin asked for or ran into since the last call.
    fn report(&self) {
        // New values and names are read from the plugin whenever they are
        // asked for; nothing needs to happen for those.
        let ignored = kParamValuesChanged | kParamTitlesChanged;
        let requested = self.handler.take_restart_requests() & !ignored;
        if requested != 0 {
            log::info!(
                "[{}] asked to be restarted (flags {requested:#X}); changes to its latency, buses or parameter \
                 list take effect when the device is next created",
                self.name
            );
        }
        if self.links.as_ref().is_some_and(|links| links.take_failure()) {
            log::error!("[{}] failed while processing audio; the device is bypassed", self.name);
        }
    }
}

impl Drop for Instance {
    /// Takes the plugin down: close its editor, deactivate, disconnect,
    /// terminate, and only then release.
    fn drop(&mut self) {
        self.close_editor();
        self.deactivate();
        // SAFETY: every object is live and initialized, this is the plugin
        // main thread, and each call undoes what was done when the
        // instance was set up, in reverse order.
        unsafe {
            if let Some(controller) = self.controller.take() {
                if let Some((component, controller)) = &controller.connection {
                    component.disconnect(controller.as_ptr());
                    controller.disconnect(component.as_ptr());
                }
                controller.edit.setComponentHandler(null_mut());
                if controller.separate {
                    controller.edit.terminate();
                }
                // The controller's references are released here, before
                // the component is terminated.
            }
            self.component.terminate();
        }
    }
}

/// The VST3 part of the plugin main thread's state.
pub(crate) struct Vst3Thread {
    /// Declared first: instances must be gone before their modules are.
    pub(super) instances: HashMap<InstanceId, Instance>,
    /// Instances that were let go of and are of a plugin that is taken
    /// down with care ([`quirks`]), each with when it may be, in the order
    /// they were let go of. They are as they were then: active, and no
    /// longer processed. [`served`](ThreadState::served) takes them down.
    leaving: Vec<(Instant, Instance)>,
    /// Modules stay loaded once used: reloading one for every instance is
    /// slow, and plugins are rarely tested against repeated unloading.
    modules: HashMap<PathBuf, Module>,
    /// The context every plugin object is initialized with.
    host: ComWrapper<HostApplication>,
    next_id: u64,
}

impl ThreadState for Vst3Thread {
    fn new() -> Self {
        Self {
            instances: HashMap::new(),
            leaving: Vec::new(),
            modules: HashMap::new(),
            host: HostApplication::new(),
            next_id: 1,
        }
    }

    fn wants_idle(&self) -> bool {
        !self.instances.is_empty() || !self.leaving.is_empty()
    }

    /// Carries values from each plugin's processor to its controller,
    /// reports what plugins left in their flags, and looks after their
    /// editors.
    fn idle(&mut self, nested: bool) {
        for instance in self.instances.values_mut() {
            instance.sync_controller();
            instance.report();
            instance.service_editor(nested);
        }
    }

    /// Takes down an instance that was being kept and has had its time:
    /// one to a call, so that the thread's windows are served again
    /// before the next.
    ///
    /// This is the only place such an instance is taken down in. Here the
    /// thread has just delivered what was waiting for its windows, so what
    /// the plugin had posted to itself for the instance has reached it
    /// while the instance was there; taken down on the way out of a job
    /// instead, it would be gone when the messages that waited through
    /// the job are delivered. And the time jobs kept the thread counts
    /// towards no instance's age: a plugin that is waiting to hear from
    /// its own work has not had it.
    fn served(&mut self, away: Duration) {
        for instance in self.instances.values_mut() {
            instance.settled += away;
        }
        for (due, _) in &mut self.leaving {
            *due += away;
        }
        let now = Instant::now();
        if let Some(place) = self.leaving.iter().position(|(due, _)| *due <= now) {
            let (_, instance) = self.leaving.remove(place);
            let name = instance.name.clone();
            drop(instance);
            crate::making::taken_down(&name, now.elapsed());
        }
    }
}

/// The module at `path`, loading it if this is its first use.
fn module<'a>(
    modules: &'a mut HashMap<PathBuf, Module>,
    host: &ComWrapper<HostApplication>,
    path: &Path,
) -> Result<&'a Module, String> {
    match modules.entry(path.to_path_buf()) {
        Entry::Occupied(loaded) => Ok(loaded.into_mut()),
        Entry::Vacant(place) => {
            let module = Module::load(path).map_err(|error| format!("cannot load '{}': {error}", path.display()))?;
            Ok(place.insert(introduce(module, host)))
        }
    }
}

/// Tells a module that has just been loaded who its host is, if it is of
/// a kind that wants to know.
pub(super) fn introduce(module: Module, host: &ComWrapper<HostApplication>) -> Module {
    if let Some(factory) = module.factory().cast::<IPluginFactory3>() {
        // SAFETY: the factory is live, and the host context outlives every
        // module.
        unsafe { factory.setHostContext(interface::<_, FUnknown>(host)) };
    }
    module
}

impl Vst3Thread {
    /// Registers a module that is compiled into the test binary, as if it
    /// had been loaded from a file, and returns that pretended file's path
    /// and what it contains.
    #[cfg(test)]
    pub(crate) fn preload(
        &mut self,
        name: &str,
        factory: ComPtr<vst3::Steinberg::IPluginFactory>,
    ) -> (PathBuf, Vec<crate::scan::PluginRecord>) {
        let records = super::probe::records(&factory);
        // Named as a plugin's file would be: some things about a plugin
        // are told by its file's name.
        let path = PathBuf::from(format!("<scripted modules>/{name}.vst3"));
        self.modules.insert(path.clone(), introduce(Module::from_factory(factory), &self.host));
        (path, records)
    }

    /// Loads, instantiates and activates a plugin, restoring `state`. The
    /// model's parameter values are resolved but not applied: they reach
    /// the plugin through its processor.
    pub(crate) fn create(&mut self, request: CreateRequest, waited: std::time::Duration) -> Result<Created, String> {
        // What each step takes is said when all are done: which of the
        // time is the plugin's own, and which went before it began.
        let mut steps = crate::making::Steps::begun(waited);
        let module = module(&mut self.modules, &self.host, &request.path)?;
        steps.done("its file");
        let mut instance = Instance::new(module, &self.host, request.class, &request.name)?;
        instance.offline = request.offline;
        instance.attach_controller(module, &self.host);
        steps.done("the instance");
        instance.restore(request.state.as_deref())?;
        steps.done("its state");

        let buses = instance.negotiate_buses(request.key_bus);
        let takes_notes = instance.enable_notes();
        instance.setup(request.sample_rate, request.max_block)?;
        steps.done("setting up");

        let table = instance.controller.as_ref().map(|controller| params::read(&controller.edit, &request.name));
        let table = table.unwrap_or_default();
        let mut listed = table.listed;
        let silencing = instance.controller.as_ref().map(|controller| params::silencing_params(&controller.edit));
        let channels = instance.note_channels();
        let midi = instance.controller.as_ref().map(|controller| params::midi_params(&controller.edit, channels));
        steps.done("its parameters");
        let latency = instance.activate()?;
        if let Some(controller) = &instance.controller {
            // Only now are the values settled: some plugins fill in their
            // parameters while activating.
            params::read_baseline(&controller.edit, &mut listed);
        }
        steps.done("activating");
        steps.say(&request.name);

        let links = Arc::new(ParamLinks::new(table.ids));
        instance.handler.connect(links.clone());
        instance.links = Some(links.clone());
        instance.listed = listed.clone();

        let id = InstanceId(self.next_id);
        self.next_id += 1;
        let created = Created {
            id,
            audio: instance.processor.clone(),
            latency,
            initial_values: params::resolve_values(&listed, &request.values),
            params: listed,
            buses,
            takes_notes,
            silencing: silencing.unwrap_or_default(),
            midi: midi.unwrap_or_else(|| params::MidiParams::plain(channels)),
            links,
            editor: instance.editor_status.clone(),
            offline: request.offline,
        };
        self.instances.insert(id, instance);
        Ok(created)
    }

    /// Deactivates and destroys an instance. `audio` is the processing
    /// interface its audio half held, which must have stopped processing.
    ///
    /// An instance of a plugin that is known not to survive being taken
    /// down while it is young ([`quirks`]) is not taken down here, unless
    /// `patient` is false. Nothing uses it any more, and it goes when it
    /// is old enough and the thread has served its windows
    /// ([`served`](ThreadState::served)), or with the thread. Its editor
    /// goes now.
    pub(crate) fn destroy(&mut self, id: InstanceId, audio: Option<ComPtr<IAudioProcessor>>, patient: bool) {
        // The audio half's reference goes first, so that the last one is
        // released by the instance, after the plugin is terminated.
        drop(audio);
        let Some(mut instance) = self.instances.remove(&id) else { return };
        let patience = quirks::patience(instance.class);
        if patient && !patience.is_zero() {
            instance.close_editor();
            log::debug!("[{}] is kept until the plugin thread has had time for it", instance.name);
            self.leaving.push((instance.settled + patience, instance));
            return;
        }
        // Taken down here and now, on the thread every plugin waits for.
        let (name, began) = (instance.name.clone(), std::time::Instant::now());
        drop(instance);
        crate::making::taken_down(&name, began.elapsed());
    }

    /// Re-activates an instance, whose audio half has stopped processing,
    /// for a sample rate and block size. Returns its latency. On failure
    /// the instance stays deactivated.
    ///
    /// `waited` is how long the caller waited for this thread: said with
    /// what the steps took ([`crate::making`]), because a plugin that is
    /// made with values of the song's is activated a second time.
    pub(crate) fn reactivate(
        &mut self,
        id: InstanceId,
        (sample_rate, max_block): (f64, u32),
        waited: std::time::Duration,
    ) -> Result<u32, String> {
        let mut steps = crate::making::Steps::begun(waited);
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        instance.deactivate();
        steps.done("deactivating");
        instance.setup(sample_rate, max_block)?;
        steps.done("setting up");
        let latency = instance.activate();
        steps.done("activating");
        steps.say_of(&instance.name, "activated again");
        latency
    }

    /// The plugin's complete state, or `None` if it has none to give.
    pub(crate) fn save_state(&mut self, id: InstanceId) -> Option<Vec<u8>> {
        let instance = self.instances.get(&id)?;
        // What the controller saves includes its parameter values.
        instance.sync_controller();
        let component = instance.component_state();
        if component.is_none() {
            log::warn!("[{}] did not save its state", instance.name);
        }
        let state = PluginState {
            component: component.unwrap_or_default(),
            controller: instance.controller_state().unwrap_or_default(),
        };
        (!state.is_empty()).then(|| state::encode(instance.class, &state))
    }

    /// The current value of every listed parameter, in parameter-list
    /// order, or `None` for a plugin without an edit controller.
    pub(crate) fn param_values(&mut self, id: InstanceId) -> Option<Vec<f64>> {
        let instance = self.instances.get(&id)?;
        let controller = instance.controller.as_ref()?;
        instance.sync_controller();
        Some(params::values(&controller.edit, &instance.listed))
    }

    /// What the plugin shows for each `(parameter index, plain value)` of
    /// the parameters the device lists, or `None` where it shows nothing.
    pub(crate) fn param_texts(&mut self, id: InstanceId, values: &[(usize, f64)]) -> Vec<Option<String>> {
        let found = self.instances.get(&id).and_then(|instance| Some((instance, instance.controller.as_ref()?)));
        let Some((instance, controller)) = found else { return vec![None; values.len()] };
        values
            .iter()
            .map(|&(index, value)| params::text(&controller.edit, instance.listed.get(index)?, value))
            .collect()
    }

    /// The plain value of the listed parameter at `index` that the plugin
    /// shows as `text`.
    pub(crate) fn param_value(&mut self, id: InstanceId, index: usize, text: &str) -> Option<f64> {
        let instance = self.instances.get(&id)?;
        params::value_of(&instance.controller.as_ref()?.edit, instance.listed.get(index)?, text)
    }

    /// Lists the programs of the plugin of class `class` in the module at
    /// `path`, here on the plugin main thread: what is done when there is
    /// no scanner process to do it in. The plugin is created for the
    /// asking and destroyed again.
    pub(crate) fn read_presets(
        &mut self,
        path: &Path,
        class: &str,
        sink: &mut dyn crate::presets::Sink,
    ) -> Result<(), String> {
        let class = ClassId::parse(class).ok_or_else(|| format!("'{class}' is not a VST3 class id"))?;
        let module = module(&mut self.modules, &self.host, path)?;
        presets::programs(module, &self.host, class, sink)
    }

    /// Both states of an instance as it is now; `None` for an instance
    /// that is gone. A half the plugin does not have is empty.
    pub(crate) fn state_of(&mut self, id: InstanceId) -> Option<PluginState> {
        let instance = self.instances.get(&id)?;
        instance.sync_controller();
        Some(PluginState {
            component: instance.component_state().unwrap_or_default(),
            controller: instance.controller_state().unwrap_or_default(),
        })
    }

    /// Has an instance take on `state`, as it plays: a state that was
    /// built for it rather than saved by it.
    pub(crate) fn load_state(&mut self, id: InstanceId, state: &PluginState) -> Result<(), String> {
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        if state.is_empty() {
            return Err("the preset holds nothing to hand the plugin".to_string());
        }
        let blob = state::encode(instance.class, state);
        instance.restore(Some(&blob)).map_err(|reason| reason.replace("its saved state", "the preset's state"))
    }

    /// Has an instance take on the states in `preset`, the bytes of a
    /// preset file, as it plays.
    pub(crate) fn load_preset_file(&mut self, id: InstanceId, preset: &[u8]) -> Result<(), String> {
        if preset.is_empty() {
            return Err("the preset file is empty".to_string());
        }
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        // A preset file is a saved state by another name, and is handed
        // to the plugin the same way.
        instance.restore(Some(preset)).map_err(|reason| reason.replace("saved state", "preset"))
    }

    /// Has an instance switch to program `index` of the `steps + 1` its
    /// parameter `param` selects. Like every parameter change, it reaches
    /// the plugin's processor with its next block.
    pub(crate) fn load_program(
        &mut self,
        id: InstanceId,
        param: ParamID,
        index: u32,
        steps: u32,
    ) -> Result<(), String> {
        let instance = self.instances.get_mut(&id).ok_or("the plugin instance no longer exists")?;
        // A program may bring as much with it as a state does.
        instance.settled = Instant::now();
        let (Some(controller), Some(links)) = (&instance.controller, &instance.links) else {
            return Err("the plugin has no programs to choose from".to_string());
        };
        let slot = links.slot(param).ok_or("the plugin no longer has the parameter its programs are chosen by")?;
        let value = presets::program_value(index, steps);
        // SAFETY: the controller is live, on the plugin main thread.
        unsafe { controller.edit.setParamNormalized(param, value) };
        links.to_processor.post(slot, value);
        Ok(())
    }
}
