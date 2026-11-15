//! VST3 plugins written for this crate's tests and served straight from the
//! test binary, without a plugin file.
//!
//! The installed plugins the integration tests use are real, but they are
//! whatever happens to be installed, and they are well behaved. These are
//! neither: each one is scripted to show the host one thing, exactly and on
//! every machine, including the things a plugin should never do.
//!
//! What a plugin observes (the transport it was given, the threads it was
//! called on, the buses and parameter changes it received) it records in
//! [`Seen`], which the tests read back by the instance's serial number.

use super::class_id::ClassId;
use super::text;
use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::ThreadId;
use vst3::Steinberg::Vst::BusDirections_::kInput;
use vst3::Steinberg::Vst::BusInfo_::BusFlags_::kDefaultActive;
use vst3::Steinberg::Vst::BusTypes_::{kAux, kMain};
use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsHidden, kIsList, kIsReadOnly};
use vst3::Steinberg::Vst::RestartFlags_::{kLatencyChanged, kParamValuesChanged};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, BusDirection, BusInfo, BusType, CtrlNumber, Event, IAttributeListTrait, IAudioProcessor,
    IAudioProcessorTrait, IComponent, IComponentHandler, IComponentHandlerTrait, IComponentTrait, IConnectionPoint,
    IConnectionPointTrait, IEditController, IEditControllerTrait, IEventListTrait, IHostApplication,
    IHostApplicationTrait, IMessage, IMessageTrait, IMidiMapping, IMidiMappingTrait, IParamValueQueueTrait,
    IParameterChangesTrait, IUnitInfo, IUnitInfoTrait, IoMode, MediaType, ParamID, ParamValue, ParameterInfo,
    ProcessContext, ProcessData, ProcessSetup, ProgramListID, ProgramListInfo, RoutingInfo, SpeakerArr,
    SpeakerArrangement, String128, TChar, UnitID, UnitInfo, kNoParentUnitId, kNoProgramListId, kRootUnitId,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IBStream, IBStreamTrait, IPlugView, IPluginBaseTrait, IPluginFactory, IPluginFactory2,
    IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait, IPluginFactoryTrait, PClassInfo, PClassInfo2,
    PClassInfoW, PFactoryInfo, TBool, TUID, int16, int32, kInternalError, kInvalidArgument, kNoInterface,
    kNotImplemented, kResultFalse, kResultOk, tresult, uint32,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, uid};

/// What each scripted plugin does.
pub(crate) mod kind {
    /// An instrument with an event bus, no audio input and a mono output,
    /// whose edit controller is a class of its own. Every note event
    /// becomes one sample: `velocity * gain` for a note-on, `-0.5 * gain`
    /// for a note-off, `-0.125` for "all sound off".
    pub(crate) const IMPULSE: u8 = 0;
    /// An effect with a mono input and a mono output, which is its own
    /// edit controller and refuses to be anything but mono:
    /// `out = in * gain`.
    pub(crate) const MONO_GAIN: u8 = 1;
    /// [`MONO_GAIN`] that goes stereo when asked, and has a side-chain
    /// input and a second output besides. It writes 9.0 to the second
    /// output, and reports latency.
    pub(crate) const STEREO_GAIN: u8 = 2;
    /// A component without buses, parameters, state or edit controller.
    pub(crate) const BARE: u8 = 3;
    /// Cannot be initialized.
    pub(crate) const FAILS_INIT: u8 = 4;
    /// Cannot be activated.
    pub(crate) const FAILS_ACTIVATE: u8 = 5;
    /// [`MONO_GAIN`] whose third block, and every later one, fails.
    pub(crate) const FAILS_PROCESS: u8 = 6;
    /// [`MONO_GAIN`] that takes a fifth of a second to initialize.
    pub(crate) const SLOW_INIT: u8 = 7;
    /// [`IMPULSE`] that names an edit controller class its module does
    /// not have.
    pub(crate) const LOST_CONTROLLER: u8 = 8;
    /// [`MONO_GAIN`] that can be activated once, and never again.
    pub(crate) const FAILS_REACTIVATE: u8 = 9;

    pub(crate) const ALL: [u8; 10] = [
        IMPULSE,
        MONO_GAIN,
        STEREO_GAIN,
        BARE,
        FAILS_INIT,
        FAILS_ACTIVATE,
        FAILS_PROCESS,
        SLOW_INIT,
        LOST_CONTROLLER,
        FAILS_REACTIVATE,
    ];
}

fn is_instrument(kind: u8) -> bool {
    matches!(kind, kind::IMPULSE | kind::LOST_CONTROLLER)
}

/// Whether the plugin's edit controller is a class of its own.
fn has_separate_controller(kind: u8) -> bool {
    is_instrument(kind)
}

fn class_id(kind: u8, controller: bool) -> ClassId {
    let role = if controller { 0x636F_6E74 } else { 0x7072_6F63 };
    ClassId::from_tuid(&uid(0x5343_5250, 0x7465_7374, role, u32::from(kind)))
}

/// The device URI of the scripted plugin of `kind`.
pub(crate) fn uri(kind: u8) -> String {
    format!("vst3:{}", class_id(kind, false))
}

// ---- parameters ----------------------------------------------------------

struct Param {
    id: ParamID,
    title: &'static str,
    units: &'static str,
    step_count: int32,
    /// As declared to the host.
    default: f64,
    /// The normalized value the plugin actually starts with.
    initial: f64,
    unit: UnitID,
    flags: int32,
}

const fn param(id: ParamID, title: &'static str, step_count: int32, default: f64) -> Param {
    Param { id, title, units: "", step_count, default, initial: default, unit: kRootUnitId, flags: kCanAutomate }
}

/// Scales the output: the gain is twice the normalized value.
pub(crate) const GAIN: ParamID = 1;
/// A switch. Turning it on makes the "editor" move [`MANUAL`].
pub(crate) const SWITCH: ParamID = 2;
/// Four named choices.
pub(crate) const SHAPE: ParamID = 3;
pub(crate) const VOICES: ParamID = 4;
/// Five steps the plugin does not name.
pub(crate) const UNNAMED: ParamID = 5;
pub(crate) const CUTOFF_1: ParamID = 6;
pub(crate) const CUTOFF_2: ParamID = 7;
/// The instance's serial number, in millionths.
pub(crate) const SERIAL: ParamID = 4_000_000_000;
/// Follows [`GAIN`]: the processor reports it as changed whenever it is
/// given a gain.
pub(crate) const ECHO: ParamID = 9;
pub(crate) const HIDDEN: ParamID = 20;
pub(crate) const METER: ParamID = 21;
/// Not automatable: only the plugin's own editor sets it.
pub(crate) const MANUAL: ParamID = 22;
/// What the plugin maps MIDI's "all sound off" to.
pub(crate) const ALL_SOUND_OFF: ParamID = 23;

/// The value the "editor" gives [`MANUAL`] when [`SWITCH`] is turned on.
pub(crate) const MANUAL_EDIT: f64 = 0.625;
pub(crate) const SHAPES: [&str; 4] = ["Sine", "Saw", "Square", "Noise"];
const FILTER_1: UnitID = 10;
const FILTER_2: UnitID = 20;

const PARAMS: [Param; 13] = [
    Param { units: "x", ..param(GAIN, "Gain", 0, 0.5) },
    param(SWITCH, "Switch", 1, 0.0),
    // Declares a default that is not what it starts with, as a plugin with
    // a patch loaded does.
    Param { initial: 1.0 / 3.0, flags: kCanAutomate | kIsList, ..param(SHAPE, "Shape", 3, 2.0 / 3.0) },
    Param { units: "voices", ..param(VOICES, "Voices", 127, 16.0 / 127.0) },
    Param { units: "steps", ..param(UNNAMED, "Unnamed", 4, 0.5) },
    // Two parameters with one title, told apart by their unit.
    Param { unit: FILTER_1, units: "Hz", ..param(CUTOFF_1, "Cutoff", 0, 0.25) },
    Param { unit: FILTER_2, units: "Hz", ..param(CUTOFF_2, "Cutoff", 0, 0.75) },
    param(SERIAL, "Serial", 0, 0.0),
    param(ECHO, "Echo", 0, 0.5),
    // Parameters no device lists.
    Param { flags: kCanAutomate | kIsHidden, ..param(HIDDEN, "Hidden", 0, 0.0) },
    Param { flags: kIsReadOnly, ..param(METER, "Meter", 0, 0.0) },
    Param { flags: 0, ..param(MANUAL, "Manual", 0, 0.0) },
    Param { flags: kIsHidden, ..param(ALL_SOUND_OFF, "All Sound Off", 0, 0.0) },
];

/// How many parameters a device should end up listing.
pub(crate) const LISTED_PARAMS: usize = 9;

fn param_index(id: ParamID) -> Option<usize> {
    PARAMS.iter().position(|param| param.id == id)
}

// ---- what the tests read back ---------------------------------------------

/// One audio bus as a plugin found it in `process`: channel count, silence
/// flags, and whether every channel had a buffer.
pub(crate) type BusSeen = (int32, u64, bool);

/// What an instance observed.
#[derive(Clone, Default)]
pub(crate) struct Seen {
    /// The host's name.
    pub host: String,
    /// `setupProcessing`: sample rate, largest block, process mode, sample size.
    pub setup: Option<(f64, int32, int32, int32)>,
    /// Every `activateBus`: media type, direction, index, state.
    pub activated: Vec<(MediaType, BusDirection, int32, bool)>,
    /// `process` calls, and those among them without frames.
    pub blocks: u32,
    pub flushes: u32,
    pub largest_block: int32,
    /// The transport of the last block.
    pub context: Option<ProcessContext>,
    /// The audio buses of the last block with frames: inputs, outputs.
    pub buses: (Vec<BusSeen>, Vec<BusSeen>),
    /// Every parameter change `process` was given: id, value, offset.
    pub received: Vec<(ParamID, f64, int32)>,
    /// Every value the host set on the edit controller.
    pub controller_sets: Vec<(ParamID, f64)>,
    /// Whether every `process` came with all four lists (events and
    /// parameter changes, in and out). True until one did not.
    pub lists_missing: bool,
    /// Whether the plugin could add an event of its own.
    pub event_taken: Option<bool>,
    /// The threads the plugin's main-thread functions were called on, and
    /// those `process` and `setProcessing` were.
    pub main_threads: Vec<ThreadId>,
    pub audio_threads: Vec<ThreadId>,
    /// What the edit controller told the component when they were
    /// connected: a string and some bytes.
    pub greeting: Option<(String, Vec<u8>)>,
}

static SEEN: Mutex<Option<HashMap<u64, Seen>>> = Mutex::new(None);
static LIFECYCLE: Mutex<Vec<(u64, &'static str)>> = Mutex::new(Vec::new());
static CONTROLLER_LIFECYCLE: Mutex<Vec<(u64, &'static str)>> = Mutex::new(Vec::new());
/// The serial and kind of every component created.
static INSTANCES: Mutex<Vec<(u64, u8)>> = Mutex::new(Vec::new());
static HOST_CONTEXTS: AtomicU32 = AtomicU32::new(0);

fn observe(serial: u64, change: impl FnOnce(&mut Seen)) {
    change(SEEN.lock().unwrap().get_or_insert_default().entry(serial).or_default());
}

fn note_thread(threads: &mut Vec<ThreadId>) {
    let current = std::thread::current().id();
    if !threads.contains(&current) {
        threads.push(current);
    }
}

/// What the instance with this serial has observed so far.
pub(crate) fn seen(serial: u64) -> Seen {
    SEEN.lock().unwrap().get_or_insert_default().get(&serial).cloned().unwrap_or_default()
}

/// The serials of the components of `kind` created so far, oldest first.
pub(crate) fn instances(kind: u8) -> Vec<u64> {
    INSTANCES.lock().unwrap().iter().filter(|(_, instance)| *instance == kind).map(|(serial, _)| *serial).collect()
}

/// The lifecycle calls the component with this serial has received, in
/// order. Outlives the instance, so its destruction can be observed.
pub(crate) fn lifecycle(serial: u64) -> Vec<&'static str> {
    LIFECYCLE.lock().unwrap().iter().filter(|(instance, _)| *instance == serial).map(|(_, step)| *step).collect()
}

/// The same for the separate edit controller of that instance.
pub(crate) fn controller_lifecycle(serial: u64) -> Vec<&'static str> {
    let log = CONTROLLER_LIFECYCLE.lock().unwrap();
    log.iter().filter(|(instance, _)| *instance == serial).map(|(_, step)| *step).collect()
}

/// How often a host has given the scripted module its context.
pub(crate) fn host_contexts() -> u32 {
    HOST_CONTEXTS.load(Ordering::SeqCst)
}

// ---- the module ------------------------------------------------------------

struct ClassEntry {
    id: ClassId,
    category: &'static str,
    kind: u8,
    controller: bool,
}

fn classes() -> Vec<ClassEntry> {
    let mut classes = Vec::new();
    for kind in kind::ALL {
        classes.push(ClassEntry { id: class_id(kind, false), category: "Audio Module Class", kind, controller: false });
        if has_separate_controller(kind) && kind != kind::LOST_CONTROLLER {
            let category = "Component Controller Class";
            classes.push(ClassEntry { id: class_id(kind, true), category, kind, controller: true });
        }
    }
    classes
}

fn sub_categories(kind: u8) -> &'static str {
    if is_instrument(kind) { "Instrument|Synth" } else { "Fx" }
}

/// The scripted module's factory, implementing the factory interface up to
/// version `VERSION`. Each version names the classes differently, so that
/// a test can tell which one a host listened to.
struct Factory<const VERSION: u8>;

impl Class for Factory<1> {
    type Interfaces = (IPluginFactory,);
}

impl Class for Factory<2> {
    type Interfaces = (IPluginFactory2,);
}

impl Class for Factory<3> {
    type Interfaces = (IPluginFactory3,);
}

/// The scripted module's factory, as `GetPluginFactory` would return it.
pub(crate) fn factory() -> ComPtr<IPluginFactory> {
    ComWrapper::new(Factory::<3>).to_com_ptr().expect("the class implements IPluginFactory")
}

/// The same module as an older plugin would present it: through the
/// second or the first version of the factory interface only.
pub(crate) fn old_factory(version: u8) -> ComPtr<IPluginFactory> {
    match version {
        1 => ComWrapper::new(Factory::<1>).to_com_ptr(),
        _ => ComWrapper::new(Factory::<2>).to_com_ptr(),
    }
    .expect("the class implements IPluginFactory")
}

fn class_name(kind: u8, version: u8) -> String {
    format!("Scripted {kind} v{version}")
}

impl<const VERSION: u8> IPluginFactoryTrait for Factory<VERSION> {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        // SAFETY: the host passes its own `PFactoryInfo`.
        let Some(info) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        text::to_chars("daw-plugins tests", &mut info.vendor);
        kResultOk
    }

    /// One more than there are: the extra index is a class the factory
    /// fails to describe.
    unsafe fn countClasses(&self) -> int32 {
        classes().len() as int32 + 1
    }

    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        let classes = classes();
        // SAFETY: the host passes its own `PClassInfo`.
        let (Some(class), Some(info)) = (classes.get(index as usize), unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        info.cid = class.id.to_tuid();
        text::to_chars(class.category, &mut info.category);
        text::to_chars(&class_name(class.kind, 1), &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        // SAFETY: the host passes two 16-byte ids and its own pointer
        // variable.
        let (class, obj) = unsafe { (ClassId::from_tuid(&cid.cast::<TUID>().read_unaligned()), &mut *obj) };
        *obj = std::ptr::null_mut();
        let Some(class) = classes().into_iter().find(|entry| entry.id == class) else { return kNoInterface };
        let object: Option<ComPtr<FUnknown>> = match (class.controller, has_separate_controller(class.kind)) {
            (true, _) => ComWrapper::new(Object::<CONTROLLER>::new(class.kind)).to_com_ptr(),
            (false, true) => ComWrapper::new(Object::<PROCESSOR>::new(class.kind)).to_com_ptr(),
            (false, false) if class.kind == kind::BARE => {
                ComWrapper::new(Object::<PROCESSOR>::new(class.kind)).to_com_ptr()
            }
            (false, false) => ComWrapper::new(Object::<SINGLE>::new(class.kind)).to_com_ptr(),
        };
        let Some(object) = object else { return kNoInterface };
        // SAFETY: `iid` is a 16-byte interface id. A successful query
        // leaves the caller a reference of its own in `obj`.
        match unsafe { object.as_com_ref().cast_raw(iid.cast()) } {
            Some(interface) => {
                *obj = interface;
                kResultOk
            }
            None => kNoInterface,
        }
    }
}

/// `queryInterface` by an id that is only known at run time.
trait CastRaw {
    /// # Safety
    ///
    /// `iid` must point to 16 readable bytes.
    unsafe fn cast_raw(&self, iid: *const [u8; 16]) -> Option<*mut c_void>;
}

impl CastRaw for ComRef<'_, FUnknown> {
    unsafe fn cast_raw(&self, iid: *const [u8; 16]) -> Option<*mut c_void> {
        // SAFETY: `iid` is readable per this function's contract, and the
        // reference is to a live object.
        unsafe { <FUnknown as vst3::com_scrape_types::Unknown>::query_interface(self.as_ptr(), &iid.read_unaligned()) }
    }
}

impl<const VERSION: u8> IPluginFactory2Trait for Factory<VERSION> {
    unsafe fn getClassInfo2(&self, index: int32, info: *mut PClassInfo2) -> tresult {
        let classes = classes();
        // SAFETY: the host passes its own `PClassInfo2`.
        let (Some(class), Some(info)) = (classes.get(index as usize), unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        info.cid = class.id.to_tuid();
        text::to_chars(class.category, &mut info.category);
        text::to_chars(&class_name(class.kind, 2), &mut info.name);
        text::to_chars(sub_categories(class.kind), &mut info.subCategories);
        text::to_chars("Class Vendor", &mut info.vendor);
        text::to_chars("2.0", &mut info.version);
        kResultOk
    }
}

impl<const VERSION: u8> IPluginFactory3Trait for Factory<VERSION> {
    unsafe fn getClassInfoUnicode(&self, index: int32, info: *mut PClassInfoW) -> tresult {
        let classes = classes();
        // SAFETY: the host passes its own `PClassInfoW`.
        let (Some(class), Some(info)) = (classes.get(index as usize), unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        info.cid = class.id.to_tuid();
        text::to_chars(class.category, &mut info.category);
        text::to_utf16(&class_name(class.kind, 3), &mut info.name);
        text::to_chars(sub_categories(class.kind), &mut info.subCategories);
        text::to_utf16("Vendor ♪", &mut info.vendor);
        text::to_utf16("3.0", &mut info.version);
        kResultOk
    }

    unsafe fn setHostContext(&self, context: *mut FUnknown) -> tresult {
        if !context.is_null() {
            HOST_CONTEXTS.fetch_add(1, Ordering::SeqCst);
        }
        kResultOk
    }
}

// ---- the plugin ------------------------------------------------------------

/// Which of a plugin's objects an [`Object`] is.
const PROCESSOR: u8 = 0;
const CONTROLLER: u8 = 1;
/// A component that is its own edit controller.
const SINGLE: u8 = 2;

const STATE_MAGIC: &[u8; 4] = b"SCRP";
const CONTROLLER_MAGIC: &[u8; 4] = b"CTRL";

/// A scripted plugin's component, its edit controller, or both in one.
///
/// Every interface is implemented for every role; which ones an object
/// actually has is decided by its [`Class`].
struct Object<const ROLE: u8> {
    kind: u8,
    /// A component draws its serial when it is created. A separate
    /// controller starts at 0 and learns its component's serial when the
    /// two are connected.
    serial: AtomicU64,
    /// Normalized parameter values, as `f64` bits, in `PARAMS` order: the
    /// processor's, and the controller's copy of them.
    processor_values: [AtomicU64; PARAMS.len()],
    controller_values: [AtomicU64; PARAMS.len()],
    host: Mutex<Option<ComPtr<IHostApplication>>>,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
    /// Lifecycle steps of a controller that does not know its serial yet.
    unlogged: Mutex<Vec<&'static str>>,
    stereo: AtomicBool,
    sample_rate: AtomicU64,
    blocks: AtomicU32,
    activations: AtomicU32,
}

impl Class for Object<PROCESSOR> {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl Class for Object<CONTROLLER> {
    type Interfaces = (IEditController, IConnectionPoint, IUnitInfo, IMidiMapping);
}

impl Class for Object<SINGLE> {
    type Interfaces = (IComponent, IAudioProcessor, IEditController, IUnitInfo, IMidiMapping);
}

impl<const ROLE: u8> Object<ROLE> {
    fn new(kind: u8) -> Self {
        static SERIALS: AtomicU64 = AtomicU64::new(1);
        let serial = if ROLE == CONTROLLER { 0 } else { SERIALS.fetch_add(1, Ordering::Relaxed) };
        let object = Self {
            kind,
            serial: AtomicU64::new(serial),
            processor_values: PARAMS.each_ref().map(|param| AtomicU64::new(param.initial.to_bits())),
            controller_values: PARAMS.each_ref().map(|param| AtomicU64::new(param.initial.to_bits())),
            host: Mutex::new(None),
            peer: Mutex::new(None),
            handler: Mutex::new(None),
            unlogged: Mutex::new(Vec::new()),
            stereo: AtomicBool::new(false),
            sample_rate: AtomicU64::new(0),
            blocks: AtomicU32::new(0),
            activations: AtomicU32::new(0),
        };
        if ROLE != CONTROLLER {
            INSTANCES.lock().unwrap().push((serial, kind));
        }
        object.log("create");
        object
    }

    fn serial(&self) -> u64 {
        self.serial.load(Ordering::Relaxed)
    }

    fn log(&self, step: &'static str) {
        let serial = self.serial();
        if ROLE != CONTROLLER {
            LIFECYCLE.lock().unwrap().push((serial, step));
        } else if serial == 0 {
            self.unlogged.lock().unwrap().push(step);
        } else {
            CONTROLLER_LIFECYCLE.lock().unwrap().push((serial, step));
        }
    }

    /// A separate controller is told which instance it belongs to.
    fn adopt(&self, serial: u64) {
        self.serial.store(serial, Ordering::Relaxed);
        let steps = std::mem::take(&mut *self.unlogged.lock().unwrap());
        CONTROLLER_LIFECYCLE.lock().unwrap().extend(steps.into_iter().map(|step| (serial, step)));
    }

    fn on_main_thread(&self) {
        observe(self.serial(), |seen| note_thread(&mut seen.main_threads));
    }

    fn value(values: &[AtomicU64; PARAMS.len()], id: ParamID) -> f64 {
        param_index(id).map_or(0.0, |index| f64::from_bits(values[index].load(Ordering::Relaxed)))
    }

    fn set_value(values: &[AtomicU64; PARAMS.len()], id: ParamID, value: f64) {
        if let Some(index) = param_index(id) {
            values[index].store(value.to_bits(), Ordering::Relaxed);
        }
    }

    fn has_input(&self) -> bool {
        !is_instrument(self.kind) && self.kind != kind::BARE
    }

    fn bus_count(&self, media: MediaType, direction: BusDirection) -> int32 {
        let input = direction == kInput as BusDirection;
        match (self.kind, media == kAudio as MediaType, media == kEvent as MediaType) {
            (kind::BARE, ..) => 0,
            (kind::STEREO_GAIN, true, _) => 2,
            (_, true, _) => int32::from(!input || self.has_input()),
            (kind, _, true) => int32::from(input && is_instrument(kind)),
            _ => 0,
        }
    }

    /// The arrangement of an audio bus: the main one is mono until a host
    /// has it otherwise; the side-chain is mono and the extra output stereo.
    fn arrangement(&self, direction: BusDirection, index: int32) -> Option<SpeakerArrangement> {
        if index < 0 || index >= self.bus_count(kAudio as MediaType, direction) {
            return None;
        }
        let stereo = self.stereo.load(Ordering::Relaxed);
        Some(match (index, direction == kInput as BusDirection) {
            (0, _) if stereo => SpeakerArr::kStereo,
            (0, _) | (_, true) => SpeakerArr::kMono,
            (_, false) => SpeakerArr::kStereo,
        })
    }

    /// Sends the peer a message made by the host.
    fn send(&self, id: &CStr, fill: impl FnOnce(&ComRef<'_, vst3::Steinberg::Vst::IAttributeList>)) {
        let (host, peer) = (self.host.lock().unwrap().clone(), self.peer.lock().unwrap().clone());
        let (Some(host), Some(peer)) = (host, peer) else { return };
        let mut message: *mut c_void = std::ptr::null_mut();
        let mut class = IMessage::IID.map(|byte| byte as std::ffi::c_char);
        let mut interface = class;
        // SAFETY: the host and the peer are live objects, the ids are 16
        // bytes, and the message is released when `message` is dropped.
        unsafe {
            if host.createInstance(&mut class, &mut interface, &mut message) != kResultOk {
                return;
            }
            let Some(message) = ComPtr::from_raw(message.cast::<IMessage>()) else { return };
            message.setMessageID(id.as_ptr());
            if let Some(attributes) = ComRef::from_raw(message.getAttributes()) {
                fill(&attributes);
            }
            peer.notify(message.as_ptr());
        }
    }

    /// Reads a state: its magic, then values.
    fn read_state(stream: *mut IBStream, magic: &[u8; 4]) -> Option<Vec<f64>> {
        // SAFETY: the host passes a live stream, and the buffer is a local
        // of the size given.
        let stream = unsafe { ComRef::from_raw(stream) }?;
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0u8; 16];
            let mut count = 0;
            // SAFETY: as above.
            let result = unsafe { stream.read(buffer.as_mut_ptr().cast(), buffer.len() as int32, &mut count) };
            if result != kResultOk || count <= 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count as usize]);
        }
        let values = bytes.strip_prefix(magic)?;
        Some(values.chunks_exact(8).map(|value| f64::from_le_bytes(value.try_into().unwrap())).collect())
    }

    fn write_state(stream: *mut IBStream, magic: &[u8; 4], values: &[f64]) -> tresult {
        // SAFETY: the host passes a live stream, and `bytes` is readable.
        let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else { return kInvalidArgument };
        let mut bytes = magic.to_vec();
        bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
        // SAFETY: as above.
        unsafe { stream.write(bytes.as_mut_ptr().cast(), bytes.len() as int32, std::ptr::null_mut()) }
    }
}

impl<const ROLE: u8> Drop for Object<ROLE> {
    fn drop(&mut self) {
        self.log("destroy");
    }
}

impl<const ROLE: u8> IPluginBaseTrait for Object<ROLE> {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        if self.kind == kind::FAILS_INIT {
            return kResultFalse;
        }
        if self.kind == kind::SLOW_INIT {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        self.on_main_thread();
        // SAFETY: the host passes its context, a live object or null.
        let host = unsafe { ComRef::from_raw(context) }.and_then(|context| context.cast::<IHostApplication>());
        if let Some(host) = &host {
            let mut name: String128 = [0; 128];
            // SAFETY: `name` is a local `String128`.
            if unsafe { host.getName(&mut name) } == kResultOk {
                observe(self.serial(), |seen| seen.host = text::from_utf16(&name));
            }
        }
        *self.host.lock().unwrap() = host;
        self.log("initialize");
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.on_main_thread();
        self.log("terminate");
        *self.host.lock().unwrap() = None;
        *self.handler.lock().unwrap() = None;
        kResultOk
    }
}

impl<const ROLE: u8> IComponentTrait for Object<ROLE> {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        if !has_separate_controller(self.kind) {
            return kNotImplemented;
        }
        // SAFETY: the host passes its own `TUID`.
        unsafe { *class_id = self::class_id(self.kind, true).to_tuid() };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kNotImplemented
    }

    unsafe fn getBusCount(&self, media: MediaType, direction: BusDirection) -> int32 {
        self.bus_count(media, direction)
    }

    unsafe fn getBusInfo(&self, media: MediaType, direction: BusDirection, index: int32, bus: *mut BusInfo) -> tresult {
        // SAFETY: the host passes its own `BusInfo`.
        let Some(bus) = (unsafe { bus.as_mut() }) else { return kInvalidArgument };
        if index < 0 || index >= self.bus_count(media, direction) {
            return kInvalidArgument;
        }
        let arrangement = self.arrangement(direction, index).unwrap_or(SpeakerArr::kMono);
        bus.mediaType = media;
        bus.direction = direction;
        bus.channelCount = arrangement.count_ones() as int32;
        text::to_utf16(if index == 0 { "Main" } else { "Extra" }, &mut bus.name);
        bus.busType = if index == 0 { kMain } else { kAux } as BusType;
        bus.flags = if index == 0 { kDefaultActive as uint32 } else { 0 };
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _input: *mut RoutingInfo, _output: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, media: MediaType, direction: BusDirection, index: int32, state: TBool) -> tresult {
        self.on_main_thread();
        observe(self.serial(), |seen| seen.activated.push((media, direction, index, state != 0)));
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        self.on_main_thread();
        if state != 0 {
            let earlier = self.activations.fetch_add(1, Ordering::Relaxed);
            if self.kind == kind::FAILS_ACTIVATE || (self.kind == kind::FAILS_REACTIVATE && earlier > 0) {
                return kResultFalse;
            }
        }
        self.log(if state != 0 { "activate" } else { "deactivate" });
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        self.on_main_thread();
        let Some(values) = Self::read_state(state, STATE_MAGIC) else { return kResultFalse };
        for (id, value) in [GAIN, SHAPE].into_iter().zip(values) {
            Self::set_value(&self.processor_values, id, value);
        }
        self.log("setState");
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        self.on_main_thread();
        if self.kind == kind::BARE {
            return kNotImplemented;
        }
        let values = [GAIN, SHAPE].map(|id| Self::value(&self.processor_values, id));
        Self::write_state(state, STATE_MAGIC, &values)
    }
}

impl<const ROLE: u8> IAudioProcessorTrait for Object<ROLE> {
    /// Mono plugins stay mono. Only [`kind::STEREO_GAIN`] follows the host,
    /// and only for its main buses, both at once.
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: int32,
        outputs: *mut SpeakerArrangement,
        num_outs: int32,
    ) -> tresult {
        self.on_main_thread();
        self.log("arrange");
        let (audio, input, output) = (kAudio as MediaType, kInput as BusDirection, 1);
        if num_ins != self.bus_count(audio, input) || num_outs != self.bus_count(audio, output) {
            return kResultFalse;
        }
        // SAFETY: the host passes as many arrangements as it says.
        let main = |arrangements: *mut SpeakerArrangement, count: int32| (count > 0).then(|| unsafe { *arrangements });
        let wanted = [main(inputs, num_ins), main(outputs, num_outs)];
        let all = |arrangement| wanted.iter().flatten().all(|wanted| *wanted == arrangement);
        if self.kind == kind::STEREO_GAIN && all(SpeakerArr::kStereo) {
            self.stereo.store(true, Ordering::Relaxed);
            kResultOk
        } else if all(SpeakerArr::kMono) {
            self.stereo.store(false, Ordering::Relaxed);
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(&self, direction: BusDirection, index: int32, arr: *mut SpeakerArrangement) -> tresult {
        match self.arrangement(direction, index) {
            Some(arrangement) => {
                // SAFETY: the host passes its own `SpeakerArrangement`.
                unsafe { *arr = arrangement };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn canProcessSampleSize(&self, symbolic_sample_size: int32) -> tresult {
        if symbolic_sample_size == kSample32 as int32 { kResultOk } else { kResultFalse }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        match self.kind {
            kind::STEREO_GAIN => (f64::from_bits(self.sample_rate.load(Ordering::Relaxed)) / 1000.0) as uint32,
            _ => 0,
        }
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.on_main_thread();
        // SAFETY: the host passes its own `ProcessSetup`.
        let Some(setup) = (unsafe { setup.as_ref() }) else { return kInvalidArgument };
        self.sample_rate.store(setup.sampleRate.to_bits(), Ordering::Relaxed);
        let seen = (setup.sampleRate, setup.maxSamplesPerBlock, setup.processMode, setup.symbolicSampleSize);
        observe(self.serial(), |observed| observed.setup = Some(seen));
        self.log("setup");
        kResultOk
    }

    /// Like most plugins, some of these have nothing to do here and say so
    /// with a result a host must not mistake for a failure.
    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        observe(self.serial(), |seen| note_thread(&mut seen.audio_threads));
        self.log(if state != 0 { "start" } else { "stop" });
        if self.kind == kind::MONO_GAIN { kNotImplemented } else { kResultOk }
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes its own `ProcessData`, and everything it
        // points to is valid for the call as the VST3 API requires: lists
        // are live objects or null, and each bus has `numChannels` buffers
        // of `numSamples` frames.
        unsafe {
            let Some(data) = data.as_ref() else { return kInvalidArgument };
            let serial = self.serial();
            let blocks = self.blocks.fetch_add(1, Ordering::Relaxed) + 1;
            let frames = data.numSamples.max(0) as usize;
            let lists = [
                data.inputParameterChanges.is_null(),
                data.outputParameterChanges.is_null(),
                data.inputEvents.is_null(),
                data.outputEvents.is_null(),
            ];
            observe(serial, |seen| {
                seen.blocks = blocks;
                seen.flushes += u32::from(frames == 0);
                seen.largest_block = seen.largest_block.max(data.numSamples);
                seen.lists_missing |= lists.contains(&true);
                seen.context = data.processContext.as_ref().copied();
                note_thread(&mut seen.audio_threads);
            });
            if self.kind == kind::FAILS_PROCESS && blocks >= 3 {
                return kInternalError;
            }

            // Parameters first: they apply from the start of the block.
            let (mut gain_given, mut silenced) = (false, false);
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for index in 0..changes.getParameterCount() {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else { continue };
                    let (id, points) = (queue.getParameterId(), queue.getPointCount());
                    let (mut offset, mut value) = (0, 0.0);
                    if points > 0 && queue.getPoint(points - 1, &mut offset, &mut value) == kResultOk {
                        observe(serial, |seen| seen.received.push((id, value, offset)));
                        Self::set_value(&self.processor_values, id, value);
                        gain_given |= id == GAIN;
                        silenced |= id == ALL_SOUND_OFF;
                    }
                }
            }
            let normalized_gain = Self::value(&self.processor_values, GAIN);
            let gain = (normalized_gain * 2.0) as f32;

            // What the processor changes by itself goes to the host.
            if gain_given && let Some(changes) = ComRef::from_raw(data.outputParameterChanges) {
                let (id, mut index) = (ECHO, 0);
                if let Some(queue) = ComRef::from_raw(changes.addParameterData(&id, &mut index)) {
                    queue.addPoint(0, normalized_gain, &mut index);
                }
            }
            if let Some(events) = ComRef::from_raw(data.outputEvents) {
                let mut event: Event = std::mem::zeroed();
                let taken = events.addEvent(&mut event) == kResultOk;
                observe(serial, |seen| seen.event_taken = Some(taken));
            }
            if frames == 0 {
                return kResultOk;
            }

            let buses = |buses: *mut AudioBusBuffers, count: int32| -> &[AudioBusBuffers] {
                if buses.is_null() { &[] } else { std::slice::from_raw_parts(buses, count.max(0) as usize) }
            };
            let (inputs, outputs) = (buses(data.inputs, data.numInputs), buses(data.outputs, data.numOutputs));
            let channel = |bus: &AudioBusBuffers, channel: usize| -> Option<*mut f32> {
                let buffers = bus.__field0.channelBuffers32;
                (!buffers.is_null() && channel < bus.numChannels as usize)
                    .then(|| *buffers.add(channel))
                    .filter(|samples| !samples.is_null())
            };
            let found = |buses: &[AudioBusBuffers]| -> Vec<BusSeen> {
                let complete = |bus: &AudioBusBuffers| (0..bus.numChannels as usize).all(|c| channel(bus, c).is_some());
                buses.iter().map(|bus| (bus.numChannels, bus.silenceFlags, complete(bus))).collect()
            };
            observe(serial, |seen| seen.buses = (found(inputs), found(outputs)));

            let Some(main_out) = outputs.first() else { return kResultOk };
            if is_instrument(self.kind) {
                // An impulse per note event, at the event's exact frame.
                let Some(out) = channel(main_out, 0) else { return kResultOk };
                let out = std::slice::from_raw_parts_mut(out, frames);
                out.fill(0.0);
                if silenced {
                    out[0] -= 0.125;
                }
                if let Some(events) = ComRef::from_raw(data.inputEvents) {
                    for index in 0..events.getEventCount() {
                        let mut event: Event = std::mem::zeroed();
                        if events.getEvent(index, &mut event) != kResultOk {
                            continue;
                        }
                        let sample = match u32::from(event.r#type) {
                            kind if kind == kNoteOnEvent as u32 => event.__field0.noteOn.velocity * gain,
                            kind if kind == kNoteOffEvent as u32 => -0.5 * gain,
                            _ => continue,
                        };
                        if let Some(out) = out.get_mut(event.sampleOffset as usize) {
                            *out += sample;
                        }
                    }
                }
                return kResultOk;
            }

            for index in 0..main_out.numChannels as usize {
                let input = inputs.first().and_then(|bus| channel(bus, index));
                let (Some(out), Some(input)) = (channel(main_out, index), input) else { continue };
                let (out, input) =
                    (std::slice::from_raw_parts_mut(out, frames), std::slice::from_raw_parts(input, frames));
                for (out, input) in out.iter_mut().zip(input) {
                    *out = input * gain;
                }
            }
            // A plugin that does not care whether a bus was switched on.
            for bus in outputs.iter().skip(1) {
                for index in 0..bus.numChannels as usize {
                    if let Some(out) = channel(bus, index) {
                        std::slice::from_raw_parts_mut(out, frames).fill(9.0);
                    }
                }
            }
            kResultOk
        }
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

impl<const ROLE: u8> IEditControllerTrait for Object<ROLE> {
    /// Takes over the component's values, and asks the host for things it
    /// may or may not act on, as plugins do after loading a state.
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        self.on_main_thread();
        let Some(values) = Self::read_state(state, STATE_MAGIC) else { return kResultFalse };
        for (id, value) in [GAIN, SHAPE].into_iter().zip(values) {
            Self::set_value(&self.controller_values, id, value);
        }
        Self::set_value(&self.controller_values, ECHO, Self::value(&self.controller_values, GAIN));
        self.log("setComponentState");
        if let Some(handler) = self.handler.lock().unwrap().clone() {
            // SAFETY: the handler is the host's live object.
            unsafe { handler.restartComponent(kParamValuesChanged | kLatencyChanged) };
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        self.on_main_thread();
        let Some(values) = Self::read_state(state, CONTROLLER_MAGIC) else { return kResultFalse };
        for (id, value) in [VOICES].into_iter().zip(values) {
            Self::set_value(&self.controller_values, id, value);
        }
        self.log("controller.setState");
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        self.on_main_thread();
        Self::write_state(state, CONTROLLER_MAGIC, &[Self::value(&self.controller_values, VOICES)])
    }

    /// Two more than there are: the extra indices are a repeated id and a
    /// parameter the plugin fails to describe.
    unsafe fn getParameterCount(&self) -> int32 {
        PARAMS.len() as int32 + 2
    }

    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        let param = match index as usize {
            index if index < PARAMS.len() => &PARAMS[index],
            index if index == PARAMS.len() => &PARAMS[0],
            _ => return kInvalidArgument,
        };
        // SAFETY: the host passes its own `ParameterInfo`.
        let Some(info) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        info.id = param.id;
        text::to_utf16(param.title, &mut info.title);
        text::to_utf16(param.title, &mut info.shortTitle);
        text::to_utf16(param.units, &mut info.units);
        info.stepCount = param.step_count;
        info.defaultNormalizedValue = param.default;
        info.unitId = param.unit;
        info.flags = param.flags;
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, id: ParamID, value: ParamValue, string: *mut String128) -> tresult {
        // SAFETY: the host passes its own `String128`.
        let Some(string) = (unsafe { string.as_mut() }) else { return kInvalidArgument };
        match id {
            SHAPE => text::to_utf16(SHAPES[((value * 4.0) as usize).min(3)], string),
            UNNAMED => return kResultFalse,
            _ => text::to_utf16(&value.to_string(), string),
        }
        kResultOk
    }

    unsafe fn getParamValueByString(&self, _id: ParamID, _string: *mut TChar, _value: *mut ParamValue) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn plainParamToNormalized(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        self.on_main_thread();
        match id {
            SERIAL => self.serial() as f64 / 1e6,
            _ => Self::value(&self.controller_values, id),
        }
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        self.on_main_thread();
        if param_index(id).is_none() {
            return kInvalidArgument;
        }
        Self::set_value(&self.controller_values, id, value);
        observe(self.serial(), |seen| seen.controller_sets.push((id, value)));
        // The plugin's "editor": turning the switch on moves a control
        // that only the editor can move.
        if id == SWITCH && value >= 0.5 {
            Self::set_value(&self.controller_values, MANUAL, MANUAL_EDIT);
            if let Some(handler) = self.handler.lock().unwrap().clone() {
                // SAFETY: the handler is the host's live object.
                unsafe {
                    handler.beginEdit(MANUAL);
                    handler.performEdit(MANUAL, MANUAL_EDIT);
                    handler.endEdit(MANUAL);
                }
            }
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        self.on_main_thread();
        // SAFETY: the host passes its handler, a live object or null.
        *self.handler.lock().unwrap() = unsafe { ComRef::from_raw(handler) }.map(|handler| handler.to_com_ptr());
        self.log(if handler.is_null() { "handler cleared" } else { "handler set" });
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

impl<const ROLE: u8> IConnectionPointTrait for Object<ROLE> {
    /// On being connected, a component tells its controller which instance
    /// it is, and a controller greets its component, each with a message
    /// the host has to provide.
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        self.on_main_thread();
        // SAFETY: the host passes the peer's connection point, live or null.
        let Some(other) = (unsafe { ComRef::from_raw(other) }) else { return kInvalidArgument };
        *self.peer.lock().unwrap() = Some(other.to_com_ptr());
        self.log("connect");
        if ROLE == CONTROLLER {
            let text: Vec<u16> = "héllo\0".encode_utf16().collect();
            // SAFETY: the attribute ids are C strings, and the data
            // passed is readable for the length given.
            self.send(c"greeting", |attributes| unsafe {
                attributes.setString(c"text".as_ptr(), text.as_ptr());
                attributes.setBinary(c"blob".as_ptr(), [1u8, 2, 3].as_ptr().cast(), 3);
            });
        } else {
            let serial = self.serial() as i64;
            // SAFETY: the attribute id is a C string.
            self.send(c"serial", |attributes| unsafe {
                attributes.setInt(c"value".as_ptr(), serial);
            });
        }
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        self.on_main_thread();
        *self.peer.lock().unwrap() = None;
        self.log("disconnect");
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        // SAFETY: the peer passes a live message, whose id is a C string
        // and whose attributes hand out pointers valid while it lives.
        unsafe {
            let Some(message) = ComRef::from_raw(message) else { return kInvalidArgument };
            let id = message.getMessageID();
            let Some(attributes) = ComRef::from_raw(message.getAttributes()) else { return kResultFalse };
            if id.is_null() {
                return kResultFalse;
            }
            match CStr::from_ptr(id).to_bytes() {
                b"serial" => {
                    let mut serial = 0;
                    if attributes.getInt(c"value".as_ptr(), &mut serial) == kResultOk {
                        self.adopt(serial as u64);
                    }
                }
                b"greeting" => {
                    let mut text = [0u16; 16];
                    let (mut blob, mut size) = (std::ptr::null(), 0);
                    let got_text = attributes.getString(c"text".as_ptr(), text.as_mut_ptr(), 32) == kResultOk;
                    let got_blob = attributes.getBinary(c"blob".as_ptr(), &mut blob, &mut size) == kResultOk;
                    if got_text && got_blob {
                        let blob = std::slice::from_raw_parts(blob.cast::<u8>(), size as usize).to_vec();
                        observe(self.serial(), |seen| seen.greeting = Some((text::from_utf16(&text), blob)));
                    }
                }
                _ => return kResultFalse,
            }
            kResultOk
        }
    }
}

impl<const ROLE: u8> IUnitInfoTrait for Object<ROLE> {
    unsafe fn getUnitCount(&self) -> int32 {
        3
    }

    unsafe fn getUnitInfo(&self, index: int32, info: *mut UnitInfo) -> tresult {
        let (id, parent, name) = match index {
            0 => (kRootUnitId, kNoParentUnitId, "Root"),
            1 => (FILTER_1, kRootUnitId, "Filter 1"),
            2 => (FILTER_2, kRootUnitId, "Filter 2"),
            _ => return kInvalidArgument,
        };
        // SAFETY: the host passes its own `UnitInfo`.
        let Some(info) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        (info.id, info.parentUnitId, info.programListId) = (id, parent, kNoProgramListId);
        text::to_utf16(name, &mut info.name);
        kResultOk
    }

    unsafe fn getProgramListCount(&self) -> int32 {
        0
    }

    unsafe fn getProgramListInfo(&self, _index: int32, _info: *mut ProgramListInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn getProgramName(&self, _list: ProgramListID, _index: int32, _name: *mut String128) -> tresult {
        kNotImplemented
    }

    unsafe fn getProgramInfo(
        &self,
        _list: ProgramListID,
        _index: int32,
        _attribute: vst3::Steinberg::Vst::CString,
        _value: *mut String128,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn hasProgramPitchNames(&self, _list: ProgramListID, _index: int32) -> tresult {
        kNotImplemented
    }

    unsafe fn getProgramPitchName(
        &self,
        _list: ProgramListID,
        _index: int32,
        _pitch: int16,
        _name: *mut String128,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn getSelectedUnit(&self) -> UnitID {
        kRootUnitId
    }

    unsafe fn selectUnit(&self, _unit: UnitID) -> tresult {
        kNotImplemented
    }

    unsafe fn getUnitByBus(
        &self,
        _media: MediaType,
        _direction: BusDirection,
        _bus: int32,
        _channel: int32,
        _unit: *mut UnitID,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn setUnitProgramData(&self, _list_or_unit: int32, _index: int32, _data: *mut IBStream) -> tresult {
        kNotImplemented
    }
}

impl<const ROLE: u8> IMidiMappingTrait for Object<ROLE> {
    /// Only instruments take MIDI controllers, and of those only "all
    /// sound off", on the first channel of the first bus.
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: int32,
        channel: int16,
        controller: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if !is_instrument(self.kind) || (bus, channel, controller) != (0, 0, 120) {
            return kResultFalse;
        }
        // SAFETY: the host passes its own `ParamID`.
        unsafe { *id = ALL_SOUND_OFF };
        kResultOk
    }
}
