//! CLAP plugins written for this crate's tests and loaded straight from the
//! test binary, without a plugin file.
//!
//! The installed plugins the integration tests use are real, but they are
//! whatever happens to be installed, and they are well behaved. These are
//! neither: each one is scripted to show the host one thing, exactly and on
//! every machine, including the things a plugin should never do.
//!
//! What a plugin observes (the transport it was given, the threads it was
//! called on, how often it was activated) it reports through read-only
//! parameters, so the tests can read it back through the host itself.
//!
//! Some of the plugins have an editor. It draws nothing; what it does is
//! everything an editor does to its host: it negotiates a size, runs a
//! timer while it exists, and has a "user" ([`act`]) who turns its knob,
//! resizes it and closes it.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl,
};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, HostGui, PluginGui, PluginGuiImpl, Window};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts, PluginNotePortsImpl,
};
use clack_extensions::params::{
    HostParams, ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::thread_check::HostThreadCheck;
use clack_extensions::timer::{HostTimer, PluginTimer, PluginTimerImpl, TimerId};
use clack_plugin::clack_entry;
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent, TransportFlags,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

mod presets;

pub(crate) use presets::folder as preset_folder;

/// What each scripted plugin does. The number is the `KIND` of
/// [`Scripted`].
pub(crate) mod kind {
    /// An instrument with a mono output and no input. Every note event
    /// becomes one sample: `velocity * gain` for a note-on, `-0.5 * gain`
    /// for a note-off, `-0.25` for a choke.
    pub(crate) const IMPULSE: u8 = 0;
    /// An effect with a mono input and a mono output: `out = in * gain`.
    pub(crate) const MONO_GAIN: u8 = 1;
    /// [`IMPULSE`], but its note port only takes MIDI. "All sound off"
    /// becomes a sample of `-0.125`.
    pub(crate) const MIDI_IMPULSE: u8 = 2;
    /// A plugin that implements no extension: no ports, parameters, state.
    pub(crate) const BARE: u8 = 3;
    /// Cannot be instantiated.
    pub(crate) const FAILS_INIT: u8 = 4;
    /// Cannot be activated.
    pub(crate) const FAILS_ACTIVATE: u8 = 5;
    /// [`MONO_GAIN`] that refuses to start processing.
    pub(crate) const FAILS_START: u8 = 6;
    /// [`MONO_GAIN`] whose third block, and every later one, fails.
    pub(crate) const FAILS_PROCESS: u8 = 7;
    /// [`MONO_GAIN`] that takes a fifth of a second to instantiate.
    pub(crate) const SLOW_INIT: u8 = 8;

    /// [`MONO_GAIN`] with an editor the user may resize, to sizes that are
    /// multiples of ten pixels and no smaller than half its natural size.
    pub(crate) const EDITOR: u8 = 9;
    /// [`MONO_GAIN`] with an editor of one size, which works out the
    /// scale of its monitor by itself (it does not: it stays unscaled).
    pub(crate) const FIXED_EDITOR: u8 = 10;
    /// [`EDITOR`] that cannot create its editor.
    pub(crate) const FAILS_EDITOR_CREATE: u8 = 11;
    /// [`EDITOR`] that cannot put its editor into a window.
    pub(crate) const FAILS_EDITOR_ATTACH: u8 = 12;
    /// [`EDITOR`] that only offers a window of its own, floating above
    /// the host's.
    pub(crate) const FLOATING_EDITOR: u8 = 13;

    /// An [`IMPULSE`] that can be given presets, in a plugin file that
    /// brings a provider to tell of them; see `scripted::presets`.
    pub(crate) const PRESETS: u8 = 14;
    /// [`PRESETS`], in the same file, which takes a preset on a moment
    /// after it was told to: with its next block.
    pub(crate) const LATE_PRESETS: u8 = 15;
    /// [`PRESETS`] in a file of its own that brings a second provider,
    /// which fails to start.
    pub(crate) const BROKEN_PRESETS: u8 = 16;

    /// Every kind without an editor that instantiates without delay.
    pub(crate) const ALL: [u8; 8] =
        [IMPULSE, MONO_GAIN, MIDI_IMPULSE, BARE, FAILS_INIT, FAILS_ACTIVATE, FAILS_START, FAILS_PROCESS];
    /// Every kind with an editor.
    pub(crate) const EDITORS: [u8; 5] =
        [EDITOR, FIXED_EDITOR, FAILS_EDITOR_CREATE, FAILS_EDITOR_ATTACH, FLOATING_EDITOR];
}

/// The CLAP id of the scripted plugin of `kind`.
pub(crate) fn id(kind: u8) -> &'static str {
    match kind {
        kind::IMPULSE => "test.impulse",
        kind::MONO_GAIN => "test.mono-gain",
        kind::MIDI_IMPULSE => "test.midi-impulse",
        kind::BARE => "test.bare",
        kind::FAILS_INIT => "test.fails-init",
        kind::FAILS_ACTIVATE => "test.fails-activate",
        kind::FAILS_START => "test.fails-start",
        kind::FAILS_PROCESS => "test.fails-process",
        kind::EDITOR => "test.editor",
        kind::FIXED_EDITOR => "test.fixed-editor",
        kind::FAILS_EDITOR_CREATE => "test.fails-editor-create",
        kind::FAILS_EDITOR_ATTACH => "test.fails-editor-attach",
        kind::FLOATING_EDITOR => "test.floating-editor",
        kind::PRESETS => "test.presets",
        kind::LATE_PRESETS => "test.late-presets",
        kind::BROKEN_PRESETS => "test.broken-presets",
        _ => "test.slow-init",
    }
}

fn has_editor(kind: u8) -> bool {
    kind::EDITORS.contains(&kind)
}

fn has_presets(kind: u8) -> bool {
    matches!(kind, kind::PRESETS | kind::LATE_PRESETS | kind::BROKEN_PRESETS)
}

fn is_instrument(kind: u8) -> bool {
    matches!(kind, kind::IMPULSE | kind::MIDI_IMPULSE) || has_presets(kind)
}

fn has_input(kind: u8) -> bool {
    matches!(kind, kind::MONO_GAIN | kind::FAILS_START | kind::FAILS_PROCESS | kind::SLOW_INIT) || has_editor(kind)
}

/// The entry of the scripted plugin of `kind`, as a plugin file would
/// export it.
pub(crate) fn entry(kind: u8) -> &'static EntryDescriptor {
    static IMPULSE: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::IMPULSE }>>);
    static MONO_GAIN: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::MONO_GAIN }>>);
    static MIDI_IMPULSE: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::MIDI_IMPULSE }>>);
    static BARE: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::BARE }>>);
    static FAILS_INIT: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::FAILS_INIT }>>);
    static FAILS_ACTIVATE: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::FAILS_ACTIVATE }>>);
    static FAILS_START: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::FAILS_START }>>);
    static FAILS_PROCESS: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::FAILS_PROCESS }>>);
    static SLOW_INIT: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::SLOW_INIT }>>);
    static EDITOR: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::EDITOR }>>);
    static FIXED_EDITOR: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::FIXED_EDITOR }>>);
    static FAILS_EDITOR_CREATE: EntryDescriptor =
        clack_entry!(SinglePluginEntry<Scripted<{ kind::FAILS_EDITOR_CREATE }>>);
    static FAILS_EDITOR_ATTACH: EntryDescriptor =
        clack_entry!(SinglePluginEntry<Scripted<{ kind::FAILS_EDITOR_ATTACH }>>);
    static FLOATING_EDITOR: EntryDescriptor = clack_entry!(SinglePluginEntry<Scripted<{ kind::FLOATING_EDITOR }>>);
    // One file with two plugins and their preset provider, and one whose
    // second provider is broken.
    static PRESETS: EntryDescriptor = clack_entry!(presets::PresetsEntry<false>);
    static BROKEN_PRESETS: EntryDescriptor = clack_entry!(presets::PresetsEntry<true>);
    match kind {
        kind::IMPULSE => &IMPULSE,
        kind::MONO_GAIN => &MONO_GAIN,
        kind::MIDI_IMPULSE => &MIDI_IMPULSE,
        kind::BARE => &BARE,
        kind::FAILS_INIT => &FAILS_INIT,
        kind::FAILS_ACTIVATE => &FAILS_ACTIVATE,
        kind::FAILS_START => &FAILS_START,
        kind::FAILS_PROCESS => &FAILS_PROCESS,
        kind::EDITOR => &EDITOR,
        kind::FIXED_EDITOR => &FIXED_EDITOR,
        kind::FAILS_EDITOR_CREATE => &FAILS_EDITOR_CREATE,
        kind::FAILS_EDITOR_ATTACH => &FAILS_EDITOR_ATTACH,
        kind::FLOATING_EDITOR => &FLOATING_EDITOR,
        kind::PRESETS | kind::LATE_PRESETS => &PRESETS,
        kind::BROKEN_PRESETS => &BROKEN_PRESETS,
        _ => &SLOW_INIT,
    }
}

// ---- parameters ----------------------------------------------------------

struct Param {
    id: u32,
    name: &'static str,
    module: &'static str,
    /// As declared to the host: minimum, maximum, default.
    range: (f64, f64, f64),
    /// The value the plugin actually starts with.
    initial: f64,
    flags: ParamInfoFlags,
}

const fn param(id: u32, name: &'static str, range: (f64, f64, f64)) -> Param {
    Param { id, name, module: "", range, initial: range.2, flags: ParamInfoFlags::IS_AUTOMATABLE }
}

/// Something the plugin saw, reported back through a read-only parameter.
const fn seen(id: u32, name: &'static str) -> Param {
    Param { id, name, module: "", range: (0.0, 1e9, 0.0), initial: 0.0, flags: ParamInfoFlags::IS_READONLY }
}

pub(crate) const GAIN: u32 = 1;
pub(crate) const STEPS: u32 = 2;
/// What the plugin shows for each value of [`STEPS`].
pub(crate) const STEP_NAMES: [&str; 5] = ["Off", "Room", "Hall", "Plate", "Spring"];
pub(crate) const CUTOFF_1: u32 = 3;
pub(crate) const CUTOFF_2: u32 = 4;
pub(crate) const REVERSED: u32 = 5;
pub(crate) const SERIAL: u32 = 9;
pub(crate) const SEEN_TEMPO: u32 = 10;
pub(crate) const SEEN_BEATS: u32 = 11;
pub(crate) const SEEN_SECONDS: u32 = 12;
pub(crate) const SEEN_PLAYING: u32 = 13;
pub(crate) const SEEN_AUDIO_THREAD: u32 = 14;
pub(crate) const SEEN_MAIN_THREAD: u32 = 15;
pub(crate) const CALLBACKS: u32 = 16;
pub(crate) const BLOCKS: u32 = 17;
pub(crate) const SEEN_SAMPLE_RATE: u32 = 18;
pub(crate) const SEEN_MAX_FRAMES: u32 = 19;
pub(crate) const LARGEST_BLOCK: u32 = 20;
pub(crate) const RESETS: u32 = 21;
pub(crate) const SEEN_STEADY_TIME: u32 = 22;
/// Ticks of the timer the editor runs while it exists.
pub(crate) const TIMER_TICKS: u32 = 23;
/// The scale the host gave the editor, or 0 before it gave one.
pub(crate) const SEEN_SCALE: u32 = 24;
/// The size the editor takes itself to have.
pub(crate) const EDITOR_WIDTH: u32 = 25;
pub(crate) const EDITOR_HEIGHT: u32 = 26;
/// The window the editor was put into, as a number.
pub(crate) const SEEN_PARENT: u32 = 27;
/// Flushes that came the way CLAP has them for an active plugin: on a
/// thread the host calls audio and not main.
pub(crate) const AUDIO_FLUSHES: u32 = 28;

/// The editor's size at a scale of 1.
pub(crate) const EDITOR_SIZE: (u32, u32) = (400, 300);
/// What the editor rounds the sizes it is offered down to.
pub(crate) const EDITOR_STEP: u32 = 10;
const TIMER_PERIOD_MS: u32 = 5;

/// What `CUTOFF_1` reads before the plugin has first been activated. Some
/// real plugins only fill in their parameters while activating.
pub(crate) const UNSETTLED: f64 = 20.0;
pub(crate) const SETTLED_CUTOFF: f64 = 1_000.0;

const PARAMS: [Param; 25] = [
    param(GAIN, "Gain", (0.0, 2.0, 1.0)),
    // Declares a default that is not what it starts with, as a plugin with
    // a patch loaded does.
    Param { flags: ParamInfoFlags::IS_STEPPED, initial: 3.0, ..param(STEPS, "Steps", (0.0, 4.0, 2.0)) },
    // Two parameters with one name, told apart by their module.
    Param { module: "Filter 1", initial: UNSETTLED, ..param(CUTOFF_1, "Cutoff", (20.0, 20_000.0, 500.0)) },
    Param { module: "/Filter 2/", ..param(CUTOFF_2, "Cutoff", (20.0, 20_000.0, 500.0)) },
    // A range no host should trust.
    Param { initial: 0.25, ..param(REVERSED, "Reversed", (1.0, -1.0, f64::NAN)) },
    seen(SERIAL, "Serial"),
    seen(SEEN_TEMPO, "Seen Tempo"),
    seen(SEEN_BEATS, "Seen Beats"),
    seen(SEEN_SECONDS, "Seen Seconds"),
    seen(SEEN_PLAYING, "Seen Playing"),
    seen(SEEN_AUDIO_THREAD, "Seen Audio Thread"),
    seen(SEEN_MAIN_THREAD, "Seen Main Thread"),
    seen(CALLBACKS, "Main Thread Callbacks"),
    seen(BLOCKS, "Blocks"),
    seen(SEEN_SAMPLE_RATE, "Seen Sample Rate"),
    seen(SEEN_MAX_FRAMES, "Seen Max Frames"),
    seen(LARGEST_BLOCK, "Largest Block"),
    seen(RESETS, "Resets"),
    seen(SEEN_STEADY_TIME, "Seen Steady Time"),
    seen(TIMER_TICKS, "Timer Ticks"),
    seen(SEEN_SCALE, "Seen Scale"),
    seen(EDITOR_WIDTH, "Editor Width"),
    seen(EDITOR_HEIGHT, "Editor Height"),
    seen(SEEN_PARENT, "Seen Parent"),
    seen(AUDIO_FLUSHES, "Audio Flushes"),
];

/// How many parameters a host should end up listing.
pub(crate) const PARAM_COUNT: usize = PARAMS.len();

// ---- the lifecycle log ---------------------------------------------------

static LIFECYCLE: Mutex<Vec<(u64, &'static str)>> = Mutex::new(Vec::new());
/// The serial and kind of every instance that got as far as `init`.
static INSTANCES: Mutex<Vec<(u64, u8)>> = Mutex::new(Vec::new());

fn log(serial: u64, step: &'static str) {
    LIFECYCLE.lock().unwrap().push((serial, step));
}

/// The serials of the instances of `kind` created so far, oldest first.
pub(crate) fn instances(kind: u8) -> Vec<u64> {
    INSTANCES.lock().unwrap().iter().filter(|(_, instance)| *instance == kind).map(|(serial, _)| *serial).collect()
}

/// The lifecycle calls the instance with this serial has received, in
/// order. Outlives the instance, so its destruction can be observed.
pub(crate) fn lifecycle(serial: u64) -> Vec<&'static str> {
    LIFECYCLE.lock().unwrap().iter().filter(|(instance, _)| *instance == serial).map(|(_, step)| *step).collect()
}

// ---- the editor's user ---------------------------------------------------

/// Something the user of an editor does in it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Turns the gain knob to this value, as one gesture.
    TurnKnob(f64),
    /// Makes the editor ask its host for this size.
    Resize(u32, u32),
    /// Switches whether the editor may be resized.
    SetResizable(bool),
    /// Has the editor report itself closed, as a plugin does when it
    /// loses the connection to its window.
    Lose,
}

static ACTIONS: Mutex<Vec<(u64, Action)>> = Mutex::new(Vec::new());

/// Has the user of the editor of the instance with this serial do
/// `action`. The editor carries it out on its next timer tick, which only
/// comes while the editor is open.
pub(crate) fn act(serial: u64, action: Action) {
    ACTIONS.lock().unwrap().push((serial, action));
}

fn take_actions(serial: u64) -> Vec<Action> {
    let mut actions = ACTIONS.lock().unwrap();
    let (mine, others) = actions.drain(..).partition(|(instance, _)| *instance == serial);
    *actions = others;
    mine.into_iter().map(|(_, action)| action).collect()
}

/// A parameter event the plugin has yet to send its host.
#[derive(Copy, Clone)]
enum Outgoing {
    Begin,
    Value(f64),
    End,
}

// ---- the plugin ----------------------------------------------------------

pub(crate) struct Scripted<const KIND: u8>;

pub(crate) struct Shared<'a> {
    host: HostSharedHandle<'a>,
    serial: u64,
    /// The value of each parameter, as `f64` bits, in `PARAMS` order.
    values: [AtomicU64; PARAMS.len()],
    /// What the editor did to the gain, for the host to be told with the
    /// next block or flush.
    outbox: Mutex<Vec<Outgoing>>,
    /// The editor's timer, or `u32::MAX` while it has none.
    timer: AtomicU32,
    resizable: AtomicBool,
    /// The gain of a preset the plugin has said it will take on and has
    /// not yet, as `f64` bits; not a number while there is none.
    arriving: AtomicU64,
}

impl<'a> Shared<'a> {
    fn new(host: HostSharedHandle<'a>) -> Self {
        static SERIALS: AtomicU64 = AtomicU64::new(1);
        let shared = Self {
            host,
            serial: SERIALS.fetch_add(1, Ordering::Relaxed),
            values: PARAMS.each_ref().map(|param| AtomicU64::new(param.initial.to_bits())),
            outbox: Mutex::new(Vec::new()),
            timer: AtomicU32::new(u32::MAX),
            resizable: AtomicBool::new(true),
            arriving: AtomicU64::new(f64::NAN.to_bits()),
        };
        shared.set(SERIAL, shared.serial as f64);
        shared
    }

    fn slot(&self, id: u32) -> Option<&AtomicU64> {
        PARAMS.iter().position(|param| param.id == id).map(|index| &self.values[index])
    }

    fn get(&self, id: u32) -> f64 {
        self.slot(id).map_or(0.0, |slot| f64::from_bits(slot.load(Ordering::Relaxed)))
    }

    fn set(&self, id: u32, value: f64) {
        if let Some(slot) = self.slot(id) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    fn add(&self, id: u32, amount: f64) {
        self.set(id, self.get(id) + amount);
    }

    /// Applies the parameter events among `events`, as a plugin does: only
    /// to parameters it lets the host set.
    fn apply(&self, events: &InputEvents) {
        for event in events {
            if let Some(CoreEventSpace::ParamValue(event)) = event.as_core_event()
                && let Some(id) = event.param_id()
                && PARAMS.iter().any(|param| param.id == id && !param.flags.contains(ParamInfoFlags::IS_READONLY))
            {
                self.set(id.get(), event.value());
            }
        }
    }

    /// `(is main thread, is audio thread)`, according to the host.
    fn threads(&self) -> (bool, bool) {
        let Some(check) = self.host.get_extension::<HostThreadCheck>() else { return (false, false) };
        (check.is_main_thread(&self.host).unwrap_or(false), check.is_audio_thread(&self.host).unwrap_or(false))
    }

    /// Tells the host what the editor did to the gain.
    fn emit(&self, output: &mut OutputEvents) {
        let gain = ClapId::new(GAIN);
        for outgoing in self.outbox.lock().unwrap().drain(..) {
            let _ = match outgoing {
                Outgoing::Begin => output.try_push(ParamGestureBeginEvent::new(0, gain)),
                Outgoing::Value(value) => output.try_push(ParamValueEvent::new(0, gain, Pckn::match_all(), value)),
                Outgoing::End => output.try_push(ParamGestureEndEvent::new(0, gain)),
            };
        }
    }

    /// The size the editor accepts in place of `size`: no smaller than
    /// half its natural size, and a multiple of [`EDITOR_STEP`].
    fn accepted(size: GuiSize) -> GuiSize {
        let fit = |length: u32, natural: u32| (length.max(natural / 2) / EDITOR_STEP) * EDITOR_STEP;
        GuiSize { width: fit(size.width, EDITOR_SIZE.0), height: fit(size.height, EDITOR_SIZE.1) }
    }

    fn editor_size(&self) -> GuiSize {
        GuiSize { width: self.get(EDITOR_WIDTH) as u32, height: self.get(EDITOR_HEIGHT) as u32 }
    }

    fn set_editor_size(&self, size: GuiSize) {
        self.set(EDITOR_WIDTH, size.width.into());
        self.set(EDITOR_HEIGHT, size.height.into());
    }
}

impl<'a> PluginShared<'a> for Shared<'a> {}

impl<const KIND: u8> Plugin for Scripted<KIND> {
    type AudioProcessor<'a> = Processor<'a, KIND>;
    type Shared<'a> = Shared<'a>;
    type MainThread<'a> = MainThread<'a, KIND>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared<'_>>) {
        if KIND != kind::BARE {
            builder
                .register::<PluginParams>()
                .register::<PluginState>()
                .register::<PluginAudioPorts>()
                .register::<PluginNotePorts>()
                .register::<PluginRender>();
        }
        if has_editor(KIND) {
            builder.register::<PluginGui>().register::<PluginTimer>();
        }
        if has_presets(KIND) {
            builder.register::<clack_extensions::preset_discovery::PluginPresetLoad>();
        }
    }
}

impl<const KIND: u8> DefaultPluginFactory for Scripted<KIND> {
    fn get_descriptor() -> PluginDescriptor {
        let feature: &CStr = if is_instrument(KIND) { c"instrument" } else { c"audio-effect" };
        PluginDescriptor::new(id(KIND), &format!("Scripted {KIND}"))
            .with_vendor("trill-plugins tests")
            .with_description("A plugin that exists to test its host")
            .with_features([feature, c"mono"])
    }

    fn new_shared(host: HostSharedHandle<'_>) -> Result<Shared<'_>, PluginError> {
        Ok(Shared::new(host))
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Shared<'a>,
    ) -> Result<MainThread<'a, KIND>, PluginError> {
        if KIND == kind::FAILS_INIT {
            return Err(PluginError::Message("scripted to fail when instantiated"));
        }
        if KIND == kind::SLOW_INIT {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        INSTANCES.lock().unwrap().push((shared.serial, KIND));
        log(shared.serial, "init");
        Ok(MainThread { shared, host })
    }
}

// ---- main thread ---------------------------------------------------------

pub(crate) struct MainThread<'a, const KIND: u8> {
    shared: &'a Shared<'a>,
    host: HostMainThreadHandle<'a>,
}

impl<'a, const KIND: u8> PluginMainThread<'a, Shared<'a>> for MainThread<'a, KIND> {
    fn on_main_thread(&self) {
        // Only counted if the host kept its word about the thread.
        if self.shared.threads().0 {
            self.shared.add(CALLBACKS, 1.0);
        }
    }
}

impl<const KIND: u8> Drop for MainThread<'_, KIND> {
    fn drop(&mut self) {
        log(self.shared.serial, "destroy");
    }
}

impl<const KIND: u8> PluginMainThreadParams for MainThread<'_, KIND> {
    /// Two more than there are: the extra indices are a repeated id and a
    /// parameter the plugin fails to describe.
    fn count(&self) -> u32 {
        PARAMS.len() as u32 + 2
    }

    fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
        let param = match index as usize {
            index if index < PARAMS.len() => &PARAMS[index],
            index if index == PARAMS.len() => &PARAMS[0],
            _ => return,
        };
        writer.set(&ParamInfo {
            id: ClapId::new(param.id),
            flags: param.flags,
            cookie: Cookie::empty(),
            name: param.name.as_bytes(),
            module: param.module.as_bytes(),
            min_value: param.range.0,
            max_value: param.range.1,
            default_value: param.range.2,
        });
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        self.shared.slot(id.get()).map(|slot| f64::from_bits(slot.load(Ordering::Relaxed)))
    }

    fn value_to_text(&self, id: ClapId, value: f64, writer: &mut ParamDisplayWriter) -> std::fmt::Result {
        match id.get() {
            GAIN => write!(writer, "{value:.2} x"),
            STEPS => write!(writer, "{}", STEP_NAMES[(value.round().max(0.0) as usize).min(STEP_NAMES.len() - 1)]),
            CUTOFF_1 | CUTOFF_2 => write!(writer, "{value:.0} Hz"),
            _ => write!(writer, "{value}"),
        }
    }

    /// As plugins do it: the gain is read from its text; the second cutoff
    /// is answered with the bottom of its range whatever is asked; and the
    /// rest are not converted at all.
    fn text_to_value(&self, id: ClapId, text: &CStr) -> Option<f64> {
        match id.get() {
            GAIN => text.to_str().ok()?.trim().trim_end_matches('x').trim().parse().ok(),
            CUTOFF_2 => Some(20.0),
            _ => None,
        }
    }

    fn flush(&self, input: &InputEvents, output: &mut OutputEvents) {
        self.shared.apply(input);
        self.shared.emit(output);
    }
}

// ---- the editor ------------------------------------------------------------

impl<const KIND: u8> MainThread<'_, KIND> {
    /// Carries out something the editor's user did.
    fn perform(&self, action: Action) {
        let shared = self.shared;
        let host_gui = shared.host.get_extension::<HostGui>();
        match action {
            Action::TurnKnob(value) => {
                shared.set(GAIN, value);
                shared.outbox.lock().unwrap().extend([Outgoing::Begin, Outgoing::Value(value), Outgoing::End]);
                if let Some(params) = shared.host.get_extension::<HostParams>() {
                    params.request_flush(&shared.host);
                }
            }
            Action::Resize(width, height) => {
                if let Some(gui) = host_gui
                    && gui.request_resize(&shared.host, width, height).is_ok()
                {
                    shared.set_editor_size(GuiSize { width, height });
                }
            }
            Action::SetResizable(resizable) => {
                shared.resizable.store(resizable, Ordering::Relaxed);
                if let Some(gui) = host_gui {
                    gui.resize_hints_changed(&shared.host);
                }
            }
            Action::Lose => {
                if let Some(gui) = host_gui {
                    gui.closed(&shared.host, true);
                }
            }
        }
    }
}

impl<const KIND: u8> PluginGuiImpl for MainThread<'_, KIND> {
    fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        Some(configuration.api_type) == GuiApiType::default_for_current_platform()
            && configuration.is_floating == (KIND == kind::FLOATING_EDITOR)
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        let api_type = GuiApiType::default_for_current_platform()?;
        Some(GuiConfiguration { api_type, is_floating: KIND == kind::FLOATING_EDITOR })
    }

    fn create(&self, _configuration: GuiConfiguration) -> Result<(), PluginError> {
        if KIND == kind::FAILS_EDITOR_CREATE {
            return Err(PluginError::Message("scripted to fail when creating its editor"));
        }
        let shared = self.shared;
        log(shared.serial, "gui create");
        shared.set_editor_size(GuiSize { width: EDITOR_SIZE.0, height: EDITOR_SIZE.1 });
        if let Some(timers) = self.host.get_extension::<HostTimer>()
            && let Ok(timer) = timers.register_timer(&self.host, TIMER_PERIOD_MS)
        {
            shared.timer.store(timer.0, Ordering::Relaxed);
        }
        Ok(())
    }

    fn destroy(&self) {
        log(self.shared.serial, "gui destroy");
        let timer = self.shared.timer.swap(u32::MAX, Ordering::Relaxed);
        if let Some(timers) = self.host.get_extension::<HostTimer>() {
            let _ = timers.unregister_timer(&self.host, TimerId(timer));
        }
    }

    fn set_scale(&self, scale: f64) -> Result<(), PluginError> {
        if KIND == kind::FIXED_EDITOR {
            return Err(PluginError::Message("scripted to work its scale out by itself"));
        }
        log(self.shared.serial, "gui scale");
        self.shared.set(SEEN_SCALE, scale);
        let scaled = |length: u32| (f64::from(length) * scale).round() as u32;
        self.shared.set_editor_size(GuiSize { width: scaled(EDITOR_SIZE.0), height: scaled(EDITOR_SIZE.1) });
        Ok(())
    }

    fn get_size(&self) -> Option<GuiSize> {
        Some(self.shared.editor_size())
    }

    fn can_resize(&self) -> bool {
        KIND != kind::FIXED_EDITOR && self.shared.resizable.load(Ordering::Relaxed)
    }

    fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
        Some(Shared::accepted(size))
    }

    /// Like a real plugin, takes only sizes it would have offered itself.
    fn set_size(&self, size: GuiSize) -> Result<(), PluginError> {
        if size != Shared::accepted(size) {
            return Err(PluginError::Message("not a size this editor has"));
        }
        log(self.shared.serial, "gui size");
        self.shared.set_editor_size(size);
        Ok(())
    }

    fn set_parent(&self, window: Window) -> Result<(), PluginError> {
        if KIND == kind::FAILS_EDITOR_ATTACH {
            return Err(PluginError::Message("scripted to fail when given a window"));
        }
        log(self.shared.serial, "gui parent");
        self.shared.set(SEEN_PARENT, window.as_generic_ptr() as usize as f64);
        Ok(())
    }

    fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("this editor is not asked to float"))
    }

    fn show(&self) -> Result<(), PluginError> {
        log(self.shared.serial, "gui show");
        Ok(())
    }

    fn hide(&self) -> Result<(), PluginError> {
        log(self.shared.serial, "gui hide");
        Ok(())
    }
}

impl<const KIND: u8> PluginTimerImpl for MainThread<'_, KIND> {
    /// Counts the tick, and does what the editor's user did since the
    /// last one.
    fn on_timer(&self, timer_id: TimerId) {
        if timer_id.0 != self.shared.timer.load(Ordering::Relaxed) {
            return;
        }
        self.shared.add(TIMER_TICKS, 1.0);
        for action in take_actions(self.shared.serial) {
            self.perform(action);
        }
    }
}

/// The plugin cares whether it is rendered or played, and writes down
/// which it was told.
impl<const KIND: u8> PluginRenderImpl for MainThread<'_, KIND> {
    fn has_hard_realtime_requirement(&self) -> bool {
        false
    }

    fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
        log(self.shared.serial, if mode == RenderMode::Offline { "render offline" } else { "render realtime" });
        Ok(())
    }
}

const STATE_MAGIC: &[u8; 4] = b"FAKE";

impl<const KIND: u8> PluginStateImpl for MainThread<'_, KIND> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(STATE_MAGIC)?;
        for id in [GAIN, STEPS, CUTOFF_1, CUTOFF_2] {
            output.write_all(&self.shared.get(id).to_le_bytes())?;
        }
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        let values = bytes.strip_prefix(STATE_MAGIC).ok_or(PluginError::Message("not a state of this plugin"))?;
        for (id, value) in [GAIN, STEPS, CUTOFF_1, CUTOFF_2].into_iter().zip(values.chunks_exact(8)) {
            self.shared.set(id, f64::from_le_bytes(value.try_into().unwrap()));
        }
        Ok(())
    }
}

impl<const KIND: u8> PluginAudioPortsImpl for MainThread<'_, KIND> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input { has_input(KIND).into() } else { 1 }
    }

    fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"main",
                channel_count: 1,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::MONO),
                in_place_pair: None,
            });
        }
    }
}

impl<const KIND: u8> PluginNotePortsImpl for MainThread<'_, KIND> {
    fn count(&self, is_input: bool) -> u32 {
        (is_input && is_instrument(KIND)).into()
    }

    fn get(&self, index: u32, _is_input: bool, writer: &mut NotePortInfoWriter) {
        let (dialects, preferred) = match KIND {
            kind::MIDI_IMPULSE => (NoteDialects::MIDI, NoteDialect::Midi),
            _ => (NoteDialects::CLAP, NoteDialect::Clap),
        };
        if index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                supported_dialects: dialects,
                preferred_dialect: Some(preferred),
            });
        }
    }
}

// ---- audio thread --------------------------------------------------------

pub(crate) struct Processor<'a, const KIND: u8> {
    shared: &'a Shared<'a>,
    host: HostAudioProcessorHandle<'a>,
    /// A copy of the input, so the output can be written while reading it.
    input: Vec<f32>,
    blocks: u32,
}

impl<'a, const KIND: u8> PluginAudioProcessor<'a, Shared<'a>, MainThread<'a, KIND>> for Processor<'a, KIND> {
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a, KIND>,
        shared: &'a Shared<'a>,
        configuration: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        if KIND == kind::FAILS_ACTIVATE {
            return Err(PluginError::Message("scripted to fail when activated"));
        }
        log(shared.serial, "activate");
        shared.set(SEEN_SAMPLE_RATE, configuration.sample_rate);
        shared.set(SEEN_MAX_FRAMES, configuration.max_frames_count.into());
        shared.set(SEEN_MAIN_THREAD, f64::from(shared.threads() == (true, false)));
        if shared.get(CUTOFF_1) == UNSETTLED {
            shared.set(CUTOFF_1, SETTLED_CUTOFF);
        }
        Ok(Self { shared, host, input: vec![0.0; configuration.max_frames_count as usize], blocks: 0 })
    }

    fn deactivate(self, _main_thread: &MainThread<'a, KIND>) {
        log(self.shared.serial, "deactivate");
    }

    fn start_processing(&mut self) -> Result<(), PluginError> {
        if KIND == kind::FAILS_START {
            return Err(PluginError::Message("scripted to fail when started"));
        }
        log(self.shared.serial, "start");
        Ok(())
    }

    fn stop_processing(&mut self) {
        log(self.shared.serial, "stop");
    }

    fn reset(&mut self) {
        self.shared.add(RESETS, 1.0);
    }

    fn process(&mut self, process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let shared = self.shared;
        self.blocks += 1;
        shared.set(BLOCKS, self.blocks.into());
        // A preset the plugin was told to load while it played.
        let arriving = f64::from_bits(shared.arriving.swap(f64::NAN.to_bits(), Ordering::Relaxed));
        if !arriving.is_nan() {
            shared.set(GAIN, arriving);
        }
        if KIND == kind::FAILS_PROCESS && self.blocks >= 3 {
            return Err(PluginError::Message("scripted to fail while processing"));
        }
        let frames = audio.frames_count() as usize;
        shared.set(LARGEST_BLOCK, shared.get(LARGEST_BLOCK).max(frames as f64));
        shared.set(SEEN_AUDIO_THREAD, f64::from(shared.threads() == (false, true)));
        shared.set(SEEN_STEADY_TIME, process.steady_time.map_or(-1.0, |time| time as f64));
        if let Some(transport) = process.transport {
            shared.set(SEEN_TEMPO, transport.tempo);
            shared.set(SEEN_BEATS, transport.song_pos_beats.to_float());
            shared.set(SEEN_SECONDS, transport.song_pos_seconds.to_float());
            shared.set(SEEN_PLAYING, f64::from(transport.flags.contains(TransportFlags::IS_PLAYING)));
            use crate::transport::written::{self, Block, Format};
            let looping = transport.flags.contains(TransportFlags::IS_LOOP_ACTIVE);
            written::down(Format::Clap, shared.serial, || Block {
                frames,
                playing: transport.flags.contains(TransportFlags::IS_PLAYING),
                tempo: transport.tempo,
                signature: (transport.time_signature_numerator.into(), transport.time_signature_denominator.into()),
                beats: transport.song_pos_beats.to_float(),
                seconds: transport.song_pos_seconds.to_float(),
                samples: None,
                bar_start: transport.bar_start.to_float(),
                bar: u32::try_from(transport.bar_number).ok(),
                looping: looping.then(|| (transport.loop_start_beats.to_float(), transport.loop_end_beats.to_float())),
                loop_seconds: looping
                    .then(|| (transport.loop_start_seconds.to_float(), transport.loop_end_seconds.to_float())),
                steady: process.steady_time.map_or(-1, |time| time as i64),
            });
        }
        if self.blocks == 1 {
            self.host.request_callback();
        }
        shared.emit(events.output);

        if let Some(input) = audio.input_port(0)
            && let Ok(channels) = input.channels()
            && let Some(channel) = channels.into_f32().and_then(|channels| channels.channel(0))
        {
            self.input[..frames].copy_from_slice(channel);
        }
        let Some(mut output) = audio.output_port(0) else { return Ok(ProcessStatus::Continue) };
        let Some(mut channels) = output.channels()?.into_f32() else { return Ok(ProcessStatus::Continue) };
        let Some(out) = channels.channel_mut(0) else { return Ok(ProcessStatus::Continue) };

        if has_input(KIND) {
            shared.apply(events.input);
            let gain = shared.get(GAIN) as f32;
            for (out, input) in out.iter_mut().zip(&self.input) {
                *out = input * gain;
            }
            return Ok(ProcessStatus::Continue);
        }

        // An impulse per note event, at the event's exact frame. Parameter
        // events take effect in order, so a gain change applies to the
        // notes after it.
        out.fill(0.0);
        for event in events.input {
            let Some(event) = event.as_core_event() else { continue };
            let gain = shared.get(GAIN) as f32;
            let (time, sample) = match event {
                CoreEventSpace::ParamValue(_) => {
                    shared.apply(&InputEvents::from_buffer(&event_as_buffer(event)));
                    continue;
                }
                CoreEventSpace::NoteOn(note) => (note.header().time(), note.velocity() as f32 * gain),
                CoreEventSpace::NoteOff(note) => (note.header().time(), -0.5 * gain),
                CoreEventSpace::NoteChoke(note) => (note.header().time(), -0.25),
                CoreEventSpace::Midi(midi) => {
                    let [status, data, value] = midi.data();
                    let sample = match (status & 0xF0, data) {
                        (0x90, _) => f32::from(value) / 127.0 * gain,
                        (0x80, _) => -0.5 * gain,
                        (0xB0, 120) => -0.125,
                        _ => continue,
                    };
                    (midi.header().time(), sample)
                }
                _ => continue,
            };
            if let Some(out) = out.get_mut(time as usize) {
                *out += sample;
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

/// A single event as an event list, to hand it to [`Shared::apply`].
fn event_as_buffer<'a>(event: CoreEventSpace<'a>) -> [&'a UnknownEvent; 1] {
    match event {
        CoreEventSpace::ParamValue(event) => [event.as_unknown()],
        _ => unreachable!("only called for parameter events"),
    }
}

impl<const KIND: u8> PluginAudioProcessorParams for Processor<'_, KIND> {
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        self.shared.apply(input);
        self.shared.emit(output);
        if self.shared.threads() == (false, true) {
            self.shared.add(AUDIO_FLUSHES, 1.0);
        }
    }
}
