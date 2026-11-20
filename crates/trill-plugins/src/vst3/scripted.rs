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
//!
//! Some of the plugins have a view. It draws nothing; what it does is
//! everything a view does to its host: it negotiates a size, and has a
//! "user" ([`act`]) who turns its knob and makes it ask for another size.

use super::class_id::ClassId;
use super::text;
use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use vst3::Steinberg::Vst::BusDirections_::kInput;
use vst3::Steinberg::Vst::BusInfo_::BusFlags_::kDefaultActive;
use vst3::Steinberg::Vst::BusTypes_::{kAux, kMain};
use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{
    kCanAutomate, kIsHidden, kIsList, kIsProgramChange, kIsReadOnly,
};
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
    FIDString, FUnknown, IBStream, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewContentScaleSupport,
    IPlugViewContentScaleSupportTrait, IPlugViewTrait, IPluginBaseTrait, IPluginFactory, IPluginFactory2,
    IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait, IPluginFactoryTrait, PClassInfo, PClassInfo2,
    PClassInfoW, PFactoryInfo, TBool, TUID, ViewRect, char16, int16, int32, kInternalError, kInvalidArgument,
    kNoInterface, kNotImplemented, kResultFalse, kResultOk, kResultTrue, tresult, uint32,
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
    /// [`MONO_GAIN`] with a view the user may resize, to sizes that are
    /// multiples of ten pixels and no smaller than half its natural size,
    /// and which takes its monitor's scale from the host.
    pub(crate) const EDITOR: u8 = 10;
    /// [`MONO_GAIN`] with a view of one size that will not hear of scales.
    pub(crate) const FIXED_EDITOR: u8 = 11;
    /// [`EDITOR`] whose view cannot be put into a window.
    pub(crate) const FAILS_ATTACH: u8 = 12;
    /// [`EDITOR`] whose view only finds out how large it is once it is in
    /// a window, and asks for that size from inside `attached`, while it
    /// goes on reporting the old one until it is told the new.
    pub(crate) const ASKS_WHEN_ATTACHED: u8 = 13;
    /// [`MONO_GAIN`] that goes on setting itself up behind the scenes for
    /// [`SETTING_UP`](super::SETTING_UP) after it is made, as a plugin
    /// that reads its sounds on threads of its own does. When that work
    /// is done it says so to the plugin's main thread, in a message to a
    /// window of its own there, and what handles the message does not
    /// look whether the instance is still wanted: a host that has taken
    /// it down by the time the message is delivered has it complain
    /// ([`taken_down_under_its_work`](super::taken_down_under_its_work)).
    /// The host is told to leave such a plugin alone for a while
    /// ([`super::super::quirks`]).
    pub(crate) const SETS_UP_BEHIND: u8 = 14;
    /// An instrument whose event bus has
    /// [`CHANNELS_ON_BUS`](super::CHANNELS_ON_BUS) channels, and which
    /// says what arrived on which: every note becomes one sample at its
    /// frame that is [`said`](crate::notes::said) of it, and so does every
    /// value of a parameter it named for a MIDI controller. It names one
    /// for the pitch wheel, the pressure and the mod wheel on each of its
    /// channels ([`channel_param`](super::channel_param)), one for the
    /// expression pedal on its first only, one for "all sound off" on
    /// each, and none for the sustain pedal.
    pub(crate) const CHANNELS: u8 = 15;

    /// Plugins that keep whatever state they are handed and give it back,
    /// under the class ids of plugins whose vendors keep presets in files
    /// of their own ([`crate::vendor`]): what the readers of those files
    /// are tried on. Each is a [`MONO_GAIN`] otherwise. A state that has
    /// the word `refuse-this` in it is refused, and one with `ignore-this`
    /// is agreed to and not kept. They are a module of their own
    /// ([`super::keepers`]).
    pub(crate) const KEEPS_SERUM: u8 = 20;
    pub(crate) const KEEPS_AMPLE: u8 = 21;
    pub(crate) const KEEPS_SUPERIOR: u8 = 22;
    pub(crate) const KEEPS_ARCHETYPE: u8 = 23;
    pub(crate) const KEEPERS: [u8; 4] = [KEEPS_SERUM, KEEPS_AMPLE, KEEPS_SUPERIOR, KEEPS_ARCHETYPE];

    pub(crate) const ALL: [u8; 16] = [
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
        EDITOR,
        FIXED_EDITOR,
        FAILS_ATTACH,
        ASKS_WHEN_ATTACHED,
        SETS_UP_BEHIND,
        CHANNELS,
    ];
}

/// How many channels the event bus of a [`kind::CHANNELS`] plugin has.
pub(crate) const CHANNELS_ON_BUS: int32 = 8;
const CHANNEL_PARAMS: ParamID = 100_000;
const NUMBERS_A_CHANNEL: ParamID = 130;

/// The parameter a [`kind::CHANNELS`] plugin names for `controller` (as
/// VST3 numbers them: 128 the pressure, 129 the wheel) on the channel it
/// counts as `channel`, from 0.
pub(crate) fn channel_param(channel: u8, controller: u8) -> ParamID {
    CHANNEL_PARAMS + ParamID::from(channel) * NUMBERS_A_CHANNEL + ParamID::from(controller)
}

/// What a parameter of [`channel_param`] stands for: what the plugin says
/// it was told, and the channel as a person calls it.
fn of_channel_param(id: ParamID) -> Option<(crate::notes::Told, u8)> {
    use crate::notes::Told;
    let at = id.checked_sub(CHANNEL_PARAMS)?;
    let (channel, controller) = (at / NUMBERS_A_CHANNEL, at % NUMBERS_A_CHANNEL);
    let what = match controller {
        120 => Told::AllSoundOff,
        128 => Told::Pressure,
        129 => Told::Wheel,
        _ => Told::Controller,
    };
    (channel < CHANNELS_ON_BUS as ParamID).then_some((what, channel as u8 + 1))
}

/// How long a [`kind::SETS_UP_BEHIND`] plugin goes on working after it is
/// made.
pub(crate) const SETTING_UP: std::time::Duration = std::time::Duration::from_millis(150);

/// Whether `class` is a plugin that sets itself up behind the scenes.
pub(crate) fn sets_itself_up_behind(class: ClassId) -> bool {
    class == class_id(kind::SETS_UP_BEHIND, false)
}

/// The instances that were told of the end of their work behind the
/// scenes after they had been taken down: in a real plugin, work done on
/// an object that is gone.
static TAKEN_DOWN_UNDER_WORK: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Whether the instance with this serial was taken down before it had
/// heard from the work it did to set itself up.
pub(crate) fn taken_down_under_its_work(serial: u64) -> bool {
    TAKEN_DOWN_UNDER_WORK.lock().unwrap().contains(&serial)
}

/// Starts the work a [`kind::SETS_UP_BEHIND`] plugin does on a thread of
/// its own once it is made, on the plugin's main thread. When the work is
/// done it tells that thread, as a plugin framework does: in a message to
/// a window of its own there, which is delivered when the thread next
/// serves its windows. (Where there are no windows, the work turns to the
/// instance itself.)
fn set_up_behind(serial: u64) {
    #[cfg(windows)]
    let tell = {
        let window = behind::window();
        move || behind::post(window, serial)
    };
    #[cfg(not(windows))]
    let tell = move || work_done(serial);
    std::thread::spawn(move || {
        std::thread::sleep(SETTING_UP);
        tell();
    });
}

/// What an instance's work behind the scenes comes to, when it is done:
/// it turns to the instance without asking whether there still is one.
fn work_done(serial: u64) {
    if lifecycle(serial).contains(&"terminate") {
        TAKEN_DOWN_UNDER_WORK.lock().unwrap().push(serial);
    }
}

/// The window a [`kind::SETS_UP_BEHIND`] plugin has on its main thread,
/// which its work behind the scenes posts to.
#[cfg(windows)]
mod behind {
    use std::cell::Cell;
    use std::ptr::{null, null_mut};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, HWND_MESSAGE, PostMessageW, RegisterClassExW, WM_APP, WNDCLASSEXW,
    };

    /// "The work of the instance whose serial comes with this is done."
    const DONE: u32 = WM_APP + 0x51;

    thread_local! {
        /// The calling thread's window, once it has one.
        static WINDOW: Cell<isize> = const { Cell::new(0) };
    }

    unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if message == DONE {
            super::work_done(wparam as u64);
            return 0;
        }
        // SAFETY: the arguments are passed on as they were given.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    /// The calling thread's window for these messages, made on first use.
    /// It goes with the thread, as every window does.
    pub(super) fn window() -> isize {
        if WINDOW.get() != 0 {
            return WINDOW.get();
        }
        static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
        let class = CLASS.get_or_init(|| {
            let name: Vec<u16> = "TrillScriptedPluginSetsUpBehind\0".encode_utf16().collect();
            // SAFETY: the class is all zeroes but for what is set, its
            // name is NUL-terminated and kept for the life of the process,
            // and its procedure is the one above.
            unsafe {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(procedure),
                    hInstance: GetModuleHandleW(null()),
                    lpszClassName: name.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassExW(&class);
            }
            name
        });
        // SAFETY: the class is registered and its name NUL-terminated; a
        // window that could not be made is null, and posting to that
        // fails without harm.
        let window = unsafe {
            let module = GetModuleHandleW(null());
            CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), module, null())
        };
        WINDOW.set(window as isize);
        window as isize
    }

    /// Says to the thread of `window`, from any thread, that the work of
    /// the instance `serial` is done.
    pub(super) fn post(window: isize, serial: u64) {
        // SAFETY: posting to a window, alive or not, is sound.
        unsafe { PostMessageW(window as HWND, DONE, serial as WPARAM, 0) };
    }
}

/// Whether the plugin's edit controller makes a view.
fn has_view(kind: u8) -> bool {
    matches!(kind, kind::EDITOR | kind::FIXED_EDITOR | kind::FAILS_ATTACH | kind::ASKS_WHEN_ATTACHED)
}

fn is_instrument(kind: u8) -> bool {
    matches!(kind, kind::IMPULSE | kind::LOST_CONTROLLER | kind::CHANNELS)
}

/// Whether the plugin's edit controller is a class of its own.
fn has_separate_controller(kind: u8) -> bool {
    is_instrument(kind)
}

fn class_id(kind: u8, controller: bool) -> ClassId {
    // A plugin that stands in for a vendor's has the id the vendor's has,
    // or one that begins as all of that vendor's do.
    let vendors = match kind {
        kind::KEEPS_SERUM => Some("56534558667350736572756D20320000"),
        kind::KEEPS_AMPLE => Some("ABCDEF019182FAEB4170736454657374"),
        kind::KEEPS_SUPERIOR => Some("565354534433617375706572696F7220"),
        kind::KEEPS_ARCHETYPE => Some("ABCDEF019182FAEB4E44535054657374"),
        _ => None,
    };
    if let Some(id) = vendors {
        return ClassId::parse(id).expect("a class id");
    }
    let role = if controller { 0x636F_6E74 } else { 0x7072_6F63 };
    ClassId::from_tuid(&uid(0x5343_5250, 0x7465_7374, role, u32::from(kind)))
}

/// Whether the plugin keeps whatever state it is handed.
fn is_keeper(kind: u8) -> bool {
    kind::KEEPERS.contains(&kind)
}

/// The component state a keeper starts with: what its vendor's plugin
/// has when it is new, made here in the vendor's format. (The synth
/// starts with none: its presets are whole states, not laid over one.)
fn initial_state(kind: u8) -> Vec<u8> {
    use crate::vendor::{ample, archetype, superior};
    match kind {
        kind::KEEPS_AMPLE => {
            let settings = [("M-Master Vol", "0.9"), ("Auto Legato", "1"), ("Amp", "Lead 800")];
            let tune = std::path::Path::new("<no tuning files>");
            ample::state(&ample::document("Default", "4.1.0", "AMHBg", tune, &settings)).component
        }
        kind::KEEPS_SUPERIOR => superior::state("Kick1").component,
        kind::KEEPS_ARCHETYPE => {
            archetype::state(&[("gateThreshold", "-80"), ("doublerActive", "false"), ("gateLocked", "false")]).component
        }
        _ => Vec::new(),
    }
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

/// Selects one of [`PROGRAMS`]: the plugin's program parameter. A device
/// does not list it; programs are a device's presets.
pub(crate) const PROGRAM: ParamID = 30;
/// The programs of the plugin's one program list, each with the
/// (normalized) gain it sets.
pub(crate) const PROGRAMS: [(&str, f64); 3] = [("Quiet", 0.125), ("Loud", 0.75), ("", 0.25)];
pub(crate) const PROGRAM_LIST_NAME: &str = "Factory Programs";
const PROGRAM_LIST: ProgramListID = 7;

/// The gain of the program a value of [`PROGRAM`] selects.
fn program_gain(value: f64) -> f64 {
    let steps = (PROGRAMS.len() - 1) as f64;
    PROGRAMS[(value.clamp(0.0, 1.0) * steps).round() as usize].1
}

/// The frequency a normalized cutoff stands for: 20 Hz to 20 kHz, evenly
/// by pitch.
fn cutoff_hz(value: f64) -> f64 {
    20.0 * 1000f64.powf(value.clamp(0.0, 1.0))
}

/// The value the "editor" gives [`MANUAL`] when [`SWITCH`] is turned on.
pub(crate) const MANUAL_EDIT: f64 = 0.625;
pub(crate) const SHAPES: [&str; 4] = ["Sine", "Saw", "Square", "Noise"];
const FILTER_1: UnitID = 10;
const FILTER_2: UnitID = 20;

const PARAMS: [Param; 14] = [
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
    Param { flags: kIsProgramChange | kIsHidden, ..param(PROGRAM, "Program", PROGRAMS.len() as int32 - 1, 0.0) },
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
    /// The process mode the last block was given in.
    pub block_mode: Option<int32>,
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

// ---- the view ----------------------------------------------------------------

/// The view's size at a scale of 1.
pub(crate) const VIEW_SIZE: (i32, i32) = (300, 200);
/// What the view rounds the sizes it is offered down to.
pub(crate) const VIEW_STEP: i32 = 10;
/// The size [`kind::ASKS_WHEN_ATTACHED`] asks for when it is attached.
pub(crate) const ATTACHED_SIZE: (i32, i32) = (450, 300);

/// Something the user of a view does in it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Turns the gain knob to this normalized value, as one gesture.
    TurnKnob(f64),
    /// Makes the view ask its host for this size.
    Resize(i32, i32),
}

/// What a view observed, and what it is.
struct ViewState {
    kind: u8,
    /// The values of the controller that made the view.
    values: Arc<[AtomicU64; PARAMS.len()]>,
    handler: Option<ComPtr<IComponentHandler>>,
    frame: Mutex<Option<ComPtr<IPlugFrame>>>,
    /// The view's own interface, for the calls that pass it along. Not a
    /// reference: null once the view is gone.
    this: AtomicPtr<IPlugView>,
    /// The scale the host gave, as `f32` bits; 0 before it gave one.
    scale: AtomicU32,
    width: AtomicI32,
    height: AtomicI32,
    /// The window the view was put into, as a number.
    parent: AtomicUsize,
    /// The timer the view runs while it is attached, or 0.
    timer: AtomicUsize,
    ticks: AtomicU32,
    /// What the host answered each request for another size.
    resize_results: Mutex<Vec<tresult>>,
}

/// What the tests read back about a view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewSeen {
    pub scale: f32,
    pub size: (i32, i32),
    pub parent: usize,
    pub ticks: u32,
    pub resize_results: Vec<tresult>,
}

/// The view most recently made for each instance, by serial.
static VIEWS: Mutex<Option<HashMap<u64, Arc<ViewState>>>> = Mutex::new(None);
static VIEW_STEPS: Mutex<Vec<(u64, &'static str)>> = Mutex::new(Vec::new());
static ACTIONS: Mutex<Vec<(u64, Action)>> = Mutex::new(Vec::new());

fn view_log(serial: u64, step: &'static str) {
    VIEW_STEPS.lock().unwrap().push((serial, step));
}

fn view_of(serial: u64) -> Option<Arc<ViewState>> {
    VIEWS.lock().unwrap().get_or_insert_default().get(&serial).cloned()
}

/// The calls the views of the instance with this serial have received, in
/// order. Outlives the views, so their destruction can be observed.
pub(crate) fn view_steps(serial: u64) -> Vec<&'static str> {
    VIEW_STEPS.lock().unwrap().iter().filter(|(instance, _)| *instance == serial).map(|(_, step)| *step).collect()
}

/// What the latest view of the instance with this serial has observed.
pub(crate) fn view_seen(serial: u64) -> Option<ViewSeen> {
    let view = view_of(serial)?;
    Some(ViewSeen {
        scale: f32::from_bits(view.scale.load(Ordering::Relaxed)),
        size: (view.width.load(Ordering::Relaxed), view.height.load(Ordering::Relaxed)),
        parent: view.parent.load(Ordering::Relaxed),
        ticks: view.ticks.load(Ordering::Relaxed),
        resize_results: view.resize_results.lock().unwrap().clone(),
    })
}

/// Has the user of the view of the instance with this serial do `action`.
/// The view carries it out on its next timer tick, as a plugin reacts to
/// its user: from the message loop, with the host free to answer at once.
pub(crate) fn act(serial: u64, action: Action) {
    ACTIONS.lock().unwrap().push((serial, action));
}

/// Has the view do `action` right now. Called from a job on the plugin
/// main thread, this is a view calling its host back while the host is in
/// the middle of something.
pub(crate) fn act_now(serial: u64, action: Action) {
    if let Some(view) = view_of(serial) {
        view.perform(action);
    }
}

impl ViewState {
    /// The size the view accepts in place of the one offered: no smaller
    /// than half its natural size, and a multiple of [`VIEW_STEP`].
    fn accepted(width: i32, height: i32) -> (i32, i32) {
        let fit = |length: i32, natural: i32| (length.max(natural / 2) / VIEW_STEP) * VIEW_STEP;
        (fit(width, VIEW_SIZE.0), fit(height, VIEW_SIZE.1))
    }

    fn set_size(&self, (width, height): (i32, i32)) {
        self.width.store(width, Ordering::Relaxed);
        self.height.store(height, Ordering::Relaxed);
    }

    fn perform(&self, action: Action) {
        match action {
            Action::TurnKnob(value) => {
                if let Some(index) = param_index(GAIN) {
                    self.values[index].store(value.to_bits(), Ordering::Relaxed);
                }
                if let Some(handler) = &self.handler {
                    // SAFETY: the handler is the host's live object.
                    unsafe {
                        handler.beginEdit(GAIN);
                        handler.performEdit(GAIN, value);
                        handler.endEdit(GAIN);
                    }
                }
            }
            Action::Resize(width, height) => {
                let frame = self.frame.lock().unwrap().clone();
                let Some(frame) = frame else { return };
                let mut rect = ViewRect { left: 0, top: 0, right: width, bottom: height };
                // SAFETY: the frame is the host's live object, the view
                // pointer the view's own, and the rectangle a local.
                let result = unsafe { frame.resizeView(self.this.load(Ordering::Relaxed), &mut rect) };
                self.resize_results.lock().unwrap().push(result);
            }
        }
    }

    /// Counts a tick of the view's timer, and does what the view's user
    /// did since the last one.
    fn tick(&self, serial: u64) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        let mine: Vec<Action> = {
            let mut actions = ACTIONS.lock().unwrap();
            let (mine, others) = actions.drain(..).partition(|(instance, _)| *instance == serial);
            *actions = others;
            mine.into_iter().map(|(_, action)| action).collect()
        };
        for action in mine {
            self.perform(action);
        }
    }
}

/// The timers real views get from the operating system, in the way a view
/// that owns no window can have one: a timer of the thread.
#[cfg(windows)]
mod view_timer {
    use super::*;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

    const PERIOD_MS: u32 = 5;

    unsafe extern "system" fn tick(_window: HWND, _message: u32, timer: usize, _time: u32) {
        let views: Vec<(u64, Arc<ViewState>)> = {
            let views = VIEWS.lock().unwrap();
            views.iter().flatten().map(|(serial, view)| (*serial, view.clone())).collect()
        };
        for (serial, view) in views {
            if view.timer.load(Ordering::Relaxed) == timer {
                view.tick(serial);
            }
        }
    }

    pub(super) fn start(view: &ViewState) {
        // SAFETY: a timer without a window belongs to the calling thread,
        // whose message loop calls `tick`.
        view.timer.store(unsafe { SetTimer(std::ptr::null_mut(), 0, PERIOD_MS, Some(tick)) }, Ordering::Relaxed);
    }

    pub(super) fn stop(view: &ViewState) {
        let timer = view.timer.swap(0, Ordering::Relaxed);
        if timer != 0 {
            // SAFETY: the timer is one `start` made on this thread.
            unsafe { KillTimer(std::ptr::null_mut(), timer) };
        }
    }
}

#[cfg(not(windows))]
mod view_timer {
    pub(super) fn start(_view: &super::ViewState) {}

    pub(super) fn stop(_view: &super::ViewState) {}
}

/// A scripted plugin's view. With `SCALES` it takes its monitor's scale
/// from the host; without, it has no interface to be told by.
struct View<const SCALES: bool> {
    serial: u64,
    state: Arc<ViewState>,
}

impl Class for View<true> {
    type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
}

impl Class for View<false> {
    type Interfaces = (IPlugView,);
}

impl<const SCALES: bool> Drop for View<SCALES> {
    fn drop(&mut self) {
        view_timer::stop(&self.state);
        self.state.this.store(std::ptr::null_mut(), Ordering::Relaxed);
        view_log(self.serial, "destroy");
    }
}

impl<const SCALES: bool> IPlugViewTrait for View<SCALES> {
    unsafe fn isPlatformTypeSupported(&self, r#type: FIDString) -> tresult {
        // SAFETY: the host passes a C string, or null.
        let supported = !r#type.is_null() && unsafe { CStr::from_ptr(r#type) } == c"HWND";
        if supported { kResultTrue } else { kResultFalse }
    }

    unsafe fn attached(&self, parent: *mut c_void, _type: FIDString) -> tresult {
        if self.state.kind == kind::FAILS_ATTACH {
            return kResultFalse;
        }
        view_log(self.serial, "attached");
        self.state.parent.store(parent as usize, Ordering::Relaxed);
        view_timer::start(&self.state);
        if self.state.kind == kind::ASKS_WHEN_ATTACHED {
            self.state.perform(Action::Resize(ATTACHED_SIZE.0, ATTACHED_SIZE.1));
        }
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        view_log(self.serial, "removed");
        view_timer::stop(&self.state);
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        // SAFETY: the host passes its own `ViewRect`.
        let Some(size) = (unsafe { size.as_mut() }) else { return kInvalidArgument };
        let (width, height) = (self.state.width.load(Ordering::Relaxed), self.state.height.load(Ordering::Relaxed));
        *size = ViewRect { left: 0, top: 0, right: width, bottom: height };
        kResultOk
    }

    /// Like a real view, takes only sizes it would have offered itself.
    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        // SAFETY: the host passes its own `ViewRect`.
        let Some(rect) = (unsafe { new_size.as_ref() }) else { return kInvalidArgument };
        let size = (rect.right - rect.left, rect.bottom - rect.top);
        if self.state.kind != kind::FIXED_EDITOR && size != ViewState::accepted(size.0, size.1) {
            return kResultFalse;
        }
        view_log(self.serial, "onSize");
        self.state.set_size(size);
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultFalse
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        // SAFETY: the host passes its frame, a live object or null.
        *self.state.frame.lock().unwrap() = unsafe { ComRef::from_raw(frame) }.map(|frame| frame.to_com_ptr());
        view_log(self.serial, if frame.is_null() { "frame cleared" } else { "frame set" });
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        if self.state.kind == kind::FIXED_EDITOR { kResultFalse } else { kResultTrue }
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        // SAFETY: the host passes its own `ViewRect`.
        let Some(rect) = (unsafe { rect.as_mut() }) else { return kInvalidArgument };
        if self.state.kind == kind::FIXED_EDITOR {
            return kNotImplemented;
        }
        let (width, height) = ViewState::accepted(rect.right - rect.left, rect.bottom - rect.top);
        (rect.right, rect.bottom) = (rect.left + width, rect.top + height);
        kResultTrue
    }
}

impl<const SCALES: bool> IPlugViewContentScaleSupportTrait for View<SCALES> {
    unsafe fn setContentScaleFactor(&self, factor: f32) -> tresult {
        view_log(self.serial, "scale");
        self.state.scale.store(factor.to_bits(), Ordering::Relaxed);
        let scaled = |length: i32| (length as f32 * factor).round() as i32;
        self.state.set_size((scaled(VIEW_SIZE.0), scaled(VIEW_SIZE.1)));
        kResultOk
    }
}

// ---- the module ------------------------------------------------------------

struct ClassEntry {
    id: ClassId,
    category: &'static str,
    kind: u8,
    controller: bool,
}

/// The classes of the module a factory of `version` is: the keepers are
/// a module of their own, which is [`KEEPERS_MODULE`].
fn classes_of(version: u8) -> Vec<ClassEntry> {
    if version == KEEPERS_MODULE {
        let category = "Audio Module Class";
        return kind::KEEPERS
            .map(|kind| ClassEntry { id: class_id(kind, false), category, kind, controller: false })
            .into();
    }
    classes()
}

/// The factory "version" that stands for the module of keepers, which
/// speaks the first version of the factory interface.
const KEEPERS_MODULE: u8 = 9;

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

impl Class for Factory<KEEPERS_MODULE> {
    type Interfaces = (IPluginFactory,);
}

/// The module of plugins that keep whatever state they are handed: see
/// [`kind::KEEPERS`].
pub(crate) fn keepers() -> ComPtr<IPluginFactory> {
    ComWrapper::new(Factory::<KEEPERS_MODULE>).to_com_ptr().expect("the class implements IPluginFactory")
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
    match kind {
        // A vendor's folder is named as its product is.
        kind::KEEPS_ARCHETYPE => "Archetype Test X".to_string(),
        kind if is_keeper(kind) => format!("Keeper {kind}"),
        _ => format!("Scripted {kind} v{version}"),
    }
}

impl<const VERSION: u8> IPluginFactoryTrait for Factory<VERSION> {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        // SAFETY: the host passes its own `PFactoryInfo`.
        let Some(info) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        text::to_chars("trill-plugins tests", &mut info.vendor);
        kResultOk
    }

    /// One more than there are: the extra index is a class the factory
    /// fails to describe.
    unsafe fn countClasses(&self) -> int32 {
        classes_of(VERSION).len() as int32 + 1
    }

    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        let classes = classes_of(VERSION);
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
        let Some(class) = classes_of(VERSION).into_iter().find(|entry| entry.id == class) else { return kNoInterface };
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
    /// Shared with the views the controller makes, which is where the
    /// user turns the knobs.
    controller_values: Arc<[AtomicU64; PARAMS.len()]>,
    host: Mutex<Option<ComPtr<IHostApplication>>>,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
    /// Lifecycle steps of a controller that does not know its serial yet.
    unlogged: Mutex<Vec<&'static str>>,
    stereo: AtomicBool,
    /// Whether the host has switched the side-chain input on.
    key_on: AtomicBool,
    sample_rate: AtomicU64,
    blocks: AtomicU32,
    activations: AtomicU32,
    /// What a keeper was last handed: its component's state and its
    /// controller's.
    kept: Mutex<[Vec<u8>; 2]>,
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
            controller_values: Arc::new(PARAMS.each_ref().map(|param| AtomicU64::new(param.initial.to_bits()))),
            host: Mutex::new(None),
            peer: Mutex::new(None),
            handler: Mutex::new(None),
            unlogged: Mutex::new(Vec::new()),
            stereo: AtomicBool::new(false),
            key_on: AtomicBool::new(false),
            sample_rate: AtomicU64::new(0),
            blocks: AtomicU32::new(0),
            activations: AtomicU32::new(0),
            kept: Mutex::new([initial_state(kind), Vec::new()]),
        };
        if ROLE != CONTROLLER {
            INSTANCES.lock().unwrap().push((serial, kind));
        }
        object.log("create");
        if ROLE != CONTROLLER && kind == kind::SETS_UP_BEHIND {
            set_up_behind(serial);
        }
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

    /// Everything a stream holds.
    fn read_bytes(stream: *mut IBStream) -> Option<Vec<u8>> {
        // SAFETY: the host passes a live stream, and the buffer is a local
        // of the size given.
        let stream = unsafe { ComRef::from_raw(stream) }?;
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0u8; 4096];
            let mut count = 0;
            // SAFETY: as above.
            let result = unsafe { stream.read(buffer.as_mut_ptr().cast(), buffer.len() as int32, &mut count) };
            if result != kResultOk || count <= 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count as usize]);
        }
        Some(bytes)
    }

    /// Reads a state: its magic, then values.
    fn read_state(stream: *mut IBStream, magic: &[u8; 4]) -> Option<Vec<f64>> {
        let bytes = Self::read_bytes(stream)?;
        let values = bytes.strip_prefix(magic)?;
        Some(values.chunks_exact(8).map(|value| f64::from_le_bytes(value.try_into().unwrap())).collect())
    }

    /// A keeper is handed a state for the half `half` of it (0: its
    /// component, 1: its controller).
    fn keep(&self, half: usize, stream: *mut IBStream) -> tresult {
        let Some(bytes) = Self::read_bytes(stream) else { return kInvalidArgument };
        let has = |word: &[u8]| bytes.windows(word.len()).any(|window| window == word);
        if has(b"refuse-this") {
            return kResultFalse;
        }
        if !has(b"ignore-this") {
            self.kept.lock().unwrap()[half] = bytes;
        }
        kResultOk
    }

    /// A keeper gives back what it was handed for the half `half` of it.
    fn give(&self, half: usize, stream: *mut IBStream) -> tresult {
        // SAFETY: the host passes a live stream, and `bytes` is readable.
        let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else { return kInvalidArgument };
        let mut bytes = self.kept.lock().unwrap()[half].clone();
        if bytes.is_empty() {
            return kResultOk;
        }
        // SAFETY: as above.
        unsafe { stream.write(bytes.as_mut_ptr().cast(), bytes.len() as int32, std::ptr::null_mut()) }
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
        if media == kEvent as MediaType && self.kind == kind::CHANNELS {
            bus.channelCount = CHANNELS_ON_BUS;
        }
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
        if media == kAudio as MediaType && direction == kInput as BusDirection && index == 1 {
            self.key_on.store(state != 0, Ordering::Relaxed);
        }
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
        if is_keeper(self.kind) {
            return self.keep(0, state);
        }
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
        if is_keeper(self.kind) {
            return self.give(0, state);
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
                seen.block_mode = Some(data.processMode);
                seen.lists_missing |= lists.contains(&true);
                seen.context = data.processContext.as_ref().copied();
                note_thread(&mut seen.audio_threads);
            });
            if let Some(context) = data.processContext.as_ref() {
                use crate::transport::written::{self, Block, Format};
                use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_ as flags;
                let has = |flag| context.state & flag as u32 != 0;
                written::down(Format::Vst3, serial, || Block {
                    frames,
                    playing: has(flags::kPlaying),
                    tempo: context.tempo,
                    signature: (context.timeSigNumerator as u32, context.timeSigDenominator as u32),
                    beats: context.projectTimeMusic,
                    seconds: context.projectTimeSamples as f64 / context.sampleRate,
                    samples: Some(context.projectTimeSamples),
                    bar_start: context.barPositionMusic,
                    bar: None,
                    looping: has(flags::kCycleActive).then_some((context.cycleStartMusic, context.cycleEndMusic)),
                    loop_seconds: None,
                    steady: context.continousTimeSamples,
                });
            }
            if self.kind == kind::FAILS_PROCESS && blocks >= 3 {
                return kInternalError;
            }

            // Parameters first: they apply from the start of the block.
            let (mut gain_given, mut silenced) = (false, false);
            // What a plugin that says its channels was told through the
            // parameters it named for controllers: frame and sample.
            let mut told: Vec<(int32, f32)> = Vec::new();
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for index in 0..changes.getParameterCount() {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else { continue };
                    let (id, points) = (queue.getParameterId(), queue.getPointCount());
                    let (mut offset, mut value) = (0, 0.0);
                    if self.kind == kind::CHANNELS
                        && let Some((what, channel)) = of_channel_param(id)
                    {
                        for point in 0..points {
                            if queue.getPoint(point, &mut offset, &mut value) == kResultOk {
                                told.push((offset, crate::notes::said(what, channel)));
                            }
                        }
                        continue;
                    }
                    if points > 0 && queue.getPoint(points - 1, &mut offset, &mut value) == kResultOk {
                        observe(serial, |seen| seen.received.push((id, value, offset)));
                        Self::set_value(&self.processor_values, id, value);
                        if id == PROGRAM {
                            // Switching program is the processor's doing.
                            Self::set_value(&self.processor_values, GAIN, program_gain(value));
                        }
                        gain_given |= id == GAIN || id == PROGRAM;
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
                if self.kind == kind::CHANNELS {
                    use crate::notes::{Told, said};
                    for (offset, sample) in told {
                        if let Some(out) = out.get_mut(offset as usize) {
                            *out += sample;
                        }
                    }
                    if silenced {
                        out[0] += said(Told::AllSoundOff, 1);
                    }
                    if let Some(events) = ComRef::from_raw(data.inputEvents) {
                        for index in 0..events.getEventCount() {
                            let mut event: Event = std::mem::zeroed();
                            if events.getEvent(index, &mut event) != kResultOk {
                                continue;
                            }
                            let sample = match u32::from(event.r#type) {
                                kind if kind == kNoteOnEvent as u32 => {
                                    said(Told::NoteOn, event.__field0.noteOn.channel as u8 + 1)
                                }
                                kind if kind == kNoteOffEvent as u32 => {
                                    said(Told::NoteOff, event.__field0.noteOff.channel as u8 + 1)
                                }
                                _ => continue,
                            };
                            if let Some(out) = out.get_mut(event.sampleOffset as usize) {
                                *out += sample;
                            }
                        }
                    }
                    return kResultOk;
                }
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
                // A side-chain that is switched on is listened to: what
                // is in it is added to what goes out, so that it can be
                // seen there. One that is not is not looked at.
                let key = inputs.get(1).filter(|_| self.key_on.load(Ordering::Relaxed));
                if let Some(key) = key.and_then(|bus| channel(bus, 0)) {
                    for (out, key) in out.iter_mut().zip(std::slice::from_raw_parts(key, frames)) {
                        *out += key;
                    }
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
        if is_keeper(self.kind) {
            return kResultOk;
        }
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
        if is_keeper(self.kind) {
            return self.keep(1, state);
        }
        let Some(values) = Self::read_state(state, CONTROLLER_MAGIC) else { return kResultFalse };
        for (id, value) in [VOICES].into_iter().zip(values) {
            Self::set_value(&self.controller_values, id, value);
        }
        self.log("controller.setState");
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        self.on_main_thread();
        if is_keeper(self.kind) {
            return self.give(1, state);
        }
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
            // The unit ("x") is in the parameter's description, not here.
            GAIN => text::to_utf16(&format!("{:.2}", value * 2.0), string),
            SWITCH => text::to_utf16(if value >= 0.5 { "On" } else { "Off" }, string),
            SHAPE => text::to_utf16(SHAPES[((value * 4.0) as usize).min(3)], string),
            VOICES => text::to_utf16(&((value * 127.0).round() as u32 + 1).to_string(), string),
            UNNAMED => return kResultFalse,
            // These put their unit into the text as well.
            CUTOFF_1 | CUTOFF_2 => text::to_utf16(&format!("{:.0} Hz", cutoff_hz(value)), string),
            ECHO => text::to_utf16(&format!("{:.0} %", value * 100.0), string),
            _ => text::to_utf16(&value.to_string(), string),
        }
        kResultOk
    }

    /// As plugins do it: the gain is read from a bare number (not from one
    /// with its unit); the echo is answered with zero whatever is asked;
    /// and the rest are not converted at all.
    unsafe fn getParamValueByString(&self, id: ParamID, string: *mut TChar, value: *mut ParamValue) -> tresult {
        // SAFETY: the host passes its own value to fill in.
        let Some(value) = (unsafe { value.as_mut() }) else { return kInvalidArgument };
        if string.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a NUL-terminated string.
        let asked: Vec<u16> = (0..256).map(|at| unsafe { *string.add(at) }).take_while(|unit| *unit != 0).collect();
        let asked = String::from_utf16_lossy(&asked);
        match id {
            GAIN => match asked.trim().parse::<f64>() {
                Ok(gain) => *value = (gain / 2.0).clamp(0.0, 1.0),
                Err(_) => return kResultFalse,
            },
            ECHO => *value = 0.0,
            _ => return kNotImplemented,
        }
        kResultOk
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
        if id == PROGRAM {
            // The controller shows the program's values as its own.
            Self::set_value(&self.controller_values, GAIN, program_gain(value));
        }
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

    /// Only some of the plugins have a view; the others answer the way a
    /// plugin without an editor does.
    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        if !has_view(self.kind) {
            return std::ptr::null_mut();
        }
        let serial = self.serial();
        let state = Arc::new(ViewState {
            kind: self.kind,
            values: self.controller_values.clone(),
            handler: self.handler.lock().unwrap().clone(),
            frame: Mutex::new(None),
            this: AtomicPtr::new(std::ptr::null_mut()),
            scale: AtomicU32::new(0),
            width: AtomicI32::new(VIEW_SIZE.0),
            height: AtomicI32::new(VIEW_SIZE.1),
            parent: AtomicUsize::new(0),
            timer: AtomicUsize::new(0),
            ticks: AtomicU32::new(0),
            resize_results: Mutex::new(Vec::new()),
        });
        VIEWS.lock().unwrap().get_or_insert_default().insert(serial, state.clone());
        view_log(serial, "create");
        let view: Option<ComPtr<IPlugView>> = if self.kind == kind::FIXED_EDITOR {
            ComWrapper::new(View::<false> { serial, state: state.clone() }).to_com_ptr()
        } else {
            ComWrapper::new(View::<true> { serial, state: state.clone() }).to_com_ptr()
        };
        let Some(view) = view else { return std::ptr::null_mut() };
        state.this.store(view.as_ptr(), Ordering::Relaxed);
        // The host receives the reference `into_raw` gives up.
        view.into_raw()
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
        let programs = if id == kRootUnitId { PROGRAM_LIST } else { kNoProgramListId };
        (info.id, info.parentUnitId, info.programListId) = (id, parent, programs);
        text::to_utf16(name, &mut info.name);
        kResultOk
    }

    /// A keeper has no programs: its presets are its vendor's files.
    unsafe fn getProgramListCount(&self) -> int32 {
        int32::from(!is_keeper(self.kind))
    }

    unsafe fn getProgramListInfo(&self, index: int32, info: *mut ProgramListInfo) -> tresult {
        // SAFETY: the host passes its own `ProgramListInfo`.
        let Some(info) = (unsafe { info.as_mut() }).filter(|_| index == 0) else { return kInvalidArgument };
        (info.id, info.programCount) = (PROGRAM_LIST, PROGRAMS.len() as int32);
        text::to_utf16(PROGRAM_LIST_NAME, &mut info.name);
        kResultOk
    }

    unsafe fn getProgramName(&self, list: ProgramListID, index: int32, name: *mut String128) -> tresult {
        let program = usize::try_from(index).ok().and_then(|index| PROGRAMS.get(index));
        // SAFETY: the host passes its own `String128`.
        let (Some((program, _)), Some(name), true) = (program, unsafe { name.as_mut() }, list == PROGRAM_LIST) else {
            return kInvalidArgument;
        };
        text::to_utf16(program, name);
        kResultOk
    }

    /// Only the first program says what kind of sound it is, and only by
    /// its instrument.
    unsafe fn getProgramInfo(
        &self,
        list: ProgramListID,
        index: int32,
        attribute: vst3::Steinberg::Vst::CString,
        value: *mut String128,
    ) -> tresult {
        // SAFETY: the host passes a NUL-terminated attribute id and its
        // own `String128`.
        let (attribute, value) = unsafe { (std::ffi::CStr::from_ptr(attribute), value.as_mut()) };
        match (value, attribute.to_bytes()) {
            (Some(value), b"MusicalInstrument") if list == PROGRAM_LIST && index == 0 => {
                text::to_utf16("Synth|Soft Pad", value);
                kResultOk
            }
            _ => kResultFalse,
        }
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
    /// sound off", on the first channel of the first bus. The one that
    /// says its channels names more ([`kind::CHANNELS`]).
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: int32,
        channel: int16,
        controller: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if self.kind == kind::CHANNELS && bus == 0 && (0..CHANNELS_ON_BUS as int16).contains(&channel) {
            // (Its first channel's "all sound off" is the one every scripted
            // instrument names; each other channel has one of its own.)
            let named = matches!(controller, 1 | 128 | 129)
                || (controller == 11 && channel == 0)
                || (controller == 120 && channel > 0);
            if named {
                // SAFETY: the host passes its own `ParamID`.
                unsafe { *id = channel_param(channel as u8, controller as u8) };
                return kResultOk;
            }
        }
        if !is_instrument(self.kind) || (bus, channel, controller) != (0, 0, 120) {
            return kResultFalse;
        }
        // SAFETY: the host passes its own `ParamID`.
        unsafe { *id = ALL_SOUND_OFF };
        kResultOk
    }
}
