//! Everything a front end needs, constructed once.
//!
//! The window and the MCP server are two faces of the same program. What
//! they share is built here, in one place and one way:
//!
//! * one [`MediaCache`], through which the engine, the sample-playing
//!   devices, previews and waveforms all read audio files, so a file is
//!   decoded once however many of them use it;
//! * the device factories: the built-in devices loading through that
//!   cache, and the user's plugins;
//! * the [`Session`];
//! * the sound [`Library`], opened from the user's settings;
//! * the settings file itself ([`Runtime::settings`]);
//! * the audio device the user chose, opened as the settings say, and the
//!   means to choose another while the program runs
//!   ([`Runtime::set_audio`]);
//! * the MIDI inputs: every port that is plugged in and not switched off
//!   plays the armed tracks ([`Runtime::midi_inputs`]);
//! * a housekeeping thread that gives the session its regular turn (which
//!   is what swaps freshly stretched audio into the running graph, writes
//!   a take into the project as it is recorded, and replaces an audio
//!   device that was lost), writes the settings when the library's part of
//!   them changes, and keeps a safety copy of a project with unsaved
//!   changes;
//! * those safety copies, and the ones a program that crashed left behind
//!   ([`Runtime::recoverable`]).
//!
//! On top of those sit the operations that need more than one of them and
//! that every front end must do the same way: [`Runtime::sound_facts`],
//! [`Runtime::preview`], [`Runtime::add_sound`] and
//! [`Runtime::add_sound_on_new_track`]. A front end that grew its own
//! version of these would drift from the other.
//!
//! ```no_run
//! use trill_runtime::{Runtime, RuntimeOptions};
//!
//! # fn main() -> Result<(), trill_runtime::RuntimeError> {
//! let runtime = std::sync::Arc::new(Runtime::start(RuntimeOptions::default())?);
//! runtime.library.add_root("D:/Samples".as_ref());
//! runtime.session.lock().play();
//! # Ok(())
//! # }
//! ```

mod housekeeping;
pub mod midi;
mod recovery;
mod settings;
mod sounds;

pub use midi::MidiInputInfo;
pub use recovery::SafetyCopy;
pub use settings::Storage;
pub use sounds::{AddedSound, NewSoundTrack, SoundClip, SoundError, SoundFacts};

use housekeeping::Housekeeper;
use midi::{MidiBackend, MidiInputs, SystemMidi};
use parking_lot::Mutex;
use recovery::SafetyCopies;
use settings::SettingsFile;
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_dsp::Builtins;
use trill_engine::{
    AudioBackend, AudioConfig, AudioDevices, AudioHost, AudioSetup, Session, SessionError, SharedSession, StreamInfo,
};
use trill_library::{AudioSettings, Library, SaveError, Settings};
use trill_media::MediaCache;
use trill_plugins::{PluginHost, ScanMode, Scanner};

/// How to start the program's shared parts.
#[derive(Clone, Debug)]
pub struct RuntimeOptions {
    /// Where the audio goes.
    pub audio: AudioBackend,
    /// How to scan the user's plugins, or `None` to offer only the devices
    /// that ship with the program.
    pub plugins: Option<Scanner>,
    /// Where settings and the library index are kept.
    pub storage: Storage,
}

impl Default for RuntimeOptions {
    /// An ordinary run: the default audio device, the user's plugins
    /// (scanned in a `trill-plugin-scan` process) and the user's settings.
    fn default() -> Self {
        Self { audio: AudioBackend::Auto, plugins: Some(Scanner::Auto), storage: Storage::User }
    }
}

impl RuntimeOptions {
    /// A run that touches nothing outside the process: no audio device, no
    /// plugins, no settings read or written, an empty library. What a
    /// screenshot or a demonstration wants, so that it looks the same on
    /// every machine.
    pub fn hermetic() -> Self {
        Self { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage: Storage::Nowhere }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("could not start the plugin host: {0}")]
    Plugins(std::io::Error),
    #[error("could not start the audio engine: {0}")]
    Session(#[from] SessionError),
    #[error("could not start a background thread: {0}")]
    Thread(std::io::Error),
}

/// What [`Runtime::start_custom`] can be handed in place of the real
/// thing. Everything left out is as [`Runtime::start`] has it.
#[derive(Default)]
pub struct Parts {
    /// Devices offered beside the built-in ones and the user's plugins.
    pub devices: Vec<Box<dyn DeviceFactory>>,
    /// Who plays the audio, in place of what [`RuntimeOptions::audio`]
    /// says. The saved audio settings are asked of it.
    pub audio: Option<Arc<dyn AudioHost>>,
    /// The MIDI ports, in place of the system's.
    pub midi: Option<Box<dyn MidiBackend>>,
}

/// The audio stream: what was asked for and what is.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioStatus {
    /// What the user asked for, which is what the settings keep.
    pub config: AudioConfig,
    /// The stream that is running, and in its `warning` why it is not the
    /// one asked for, when it is not.
    pub info: StreamInfo,
    /// How long after the engine renders a frame it is heard, in
    /// milliseconds, as the device last reported.
    pub latency_ms: f64,
    /// Fraction of the audio thread's time budget in use.
    pub load: f32,
    /// Changes whenever the stream does; see
    /// [`Session::audio_revision`](trill_engine::Session::audio_revision).
    pub revision: u64,
}

/// The folder, beside the settings file, that saved presets are kept in.
pub const SAVED_PRESETS_FOLDER: &str = "saved-presets";
/// How long listing the audio devices may take.
const DEVICE_LIST_TIMEOUT: Duration = Duration::from_secs(6);
/// How long a rescan of the MIDI ports is waited for.
const MIDI_RESCAN_TIMEOUT: Duration = Duration::from_secs(3);

/// The program's shared parts. Front ends hold it in an `Arc` and reach
/// into the public fields; everything stops when the last holder lets go.
pub struct Runtime {
    // Field order is drop order. The housekeeping thread holds the session
    // too, and so does the thread that watches the MIDI ports, so they are
    // stopped first: the session then ends on the thread that lets go of
    // the runtime, not on a background thread that happened to hold the
    // last reference.
    _housekeeper: Housekeeper,
    midi: Option<MidiInputs>,
    /// The one open project and the engine playing it.
    pub session: SharedSession,
    /// The cache every audio file is read through.
    pub media: Arc<MediaCache>,
    /// The sound library. Changes to its folders, favorites and recently
    /// used files are saved with the settings without further ado.
    pub library: Arc<Library>,
    plugins: Option<PluginHost>,
    settings: Arc<Mutex<SettingsFile>>,
    copies: Arc<Mutex<SafetyCopies>>,
    /// Who plays the audio: asked for the devices without taking the
    /// session.
    audio_host: Arc<dyn AudioHost>,
}

impl Runtime {
    /// Builds everything and starts the audio engine. A library that has
    /// folders starts catching up with what changed in them since the last
    /// run, and the plugins are scanned, both in the background.
    pub fn start(options: RuntimeOptions) -> Result<Self, RuntimeError> {
        Self::start_with(options, Vec::new())
    }

    /// [`Runtime::start`], offering the devices of `extra` beside the
    /// built-in ones and the user's plugins. For tests of what a front end
    /// does with a plugin (its editor, say), which hand in a stand-in for
    /// one and need no plugin installed.
    #[doc(hidden)]
    pub fn start_with(options: RuntimeOptions, extra: Vec<Box<dyn DeviceFactory>>) -> Result<Self, RuntimeError> {
        Self::start_custom(options, Parts { devices: extra, ..Parts::default() })
    }

    /// [`Runtime::start`] with some of its parts replaced: audio devices
    /// and MIDI ports that exist only in a test, say, so that what a front
    /// end does with them can be tested on a machine that has none.
    pub fn start_custom(options: RuntimeOptions, parts: Parts) -> Result<Self, RuntimeError> {
        // The settings come first: they say which audio device to open.
        let (settings, index_path) = SettingsFile::open(&options.storage);
        if let Some(warning) = settings.warning() {
            log::warn!("{warning}");
        }
        let wanted = audio_config(&settings.current().audio);
        let audio = match parts.audio {
            Some(host) => AudioSetup::on(host, wanted),
            None => AudioSetup::new(options.audio, wanted),
        };
        let audio_host = audio.host.clone();
        // Without an audio device there is nothing for a keyboard to play
        // through, and a run that must not touch the machine's hardware
        // must not open its MIDI ports either.
        let midi_backend: Option<Box<dyn MidiBackend>> = match parts.midi {
            Some(backend) => Some(backend),
            None if matches!(options.audio, AudioBackend::Null { .. }) => None,
            None => Some(Box::new(SystemMidi)),
        };

        let media = Arc::new(MediaCache::default());
        let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(Builtins::with_loader(media.clone()))];
        factories.extend(parts.devices);
        let plugins = options.plugins.map(PluginHost::new).transpose().map_err(RuntimeError::Plugins)?;
        if let Some(plugins) = &plugins {
            factories.extend(plugins.factories());
            // The first scan of a machine's plugins takes seconds. It runs
            // in the background; the catalog fills in when it is done.
            let scanning = plugins.clone();
            std::thread::Builder::new()
                .name("plugin-scan".into())
                .spawn(move || {
                    scanning.scan(ScanMode::UseCache);
                    for failure in scanning.scan_failures() {
                        log::warn!("skipped plugin {}: {}", failure.path.display(), failure.reason);
                    }
                })
                .map_err(RuntimeError::Thread)?;
        }
        let factory = Arc::new(CompositeFactory::new(factories));
        let session = Session::with_audio(factory, media.clone(), audio)?;
        if let Some(warning) = &session.audio_info().warning {
            log::warn!("{warning}");
        }
        // The presets the user saved are kept beside the settings; where
        // nothing is kept, they last as long as the program runs.
        if let Some(folder) = settings.folder() {
            session.saved_presets().open(folder.join(SAVED_PRESETS_FOLDER));
        }
        let live = session.live();
        let session: SharedSession = Arc::new(Mutex::new(session));

        let midi = match midi_backend {
            None => None,
            Some(backend) => {
                // A port that goes away cannot say its keys came up.
                let playing = session.clone();
                let on_lost = Box::new(move || playing.lock().release_live());
                let disabled = settings.current().midi.disabled_inputs.clone();
                Some(MidiInputs::start(backend, live, &disabled, on_lost).map_err(RuntimeError::Thread)?)
            }
        };

        let library = Arc::new(Library::open(&settings.current().library, index_path));
        if let Some(problem) = library.index_problem() {
            log::warn!("the saved library index was not used: {problem}");
        }
        // Catch up with what changed on disk since the last run. With no
        // folders there is nothing to catch up with, and a scan would only
        // write an empty index.
        if !library.roots().is_empty() {
            library.rescan();
        }
        let copies = SafetyCopies::open(settings.recovery_folder());
        for copy in copies.orphans() {
            log::warn!("'{}' was not saved before the program last stopped; a copy was kept", copy.name);
        }
        let copies = Arc::new(Mutex::new(copies));
        let settings = Arc::new(Mutex::new(settings));
        let housekeeper = Housekeeper::start(session.clone(), library.clone(), settings.clone(), copies.clone())?;
        Ok(Self { _housekeeper: housekeeper, midi, session, media, library, plugins, settings, copies, audio_host })
    }

    // ---- the audio device ----------------------------------------------

    /// The audio devices that could be chosen. This asks the system, which
    /// takes a moment (and with a driver that does not answer, a few
    /// seconds, after which the lists come back empty): call it when a
    /// settings panel opens, not on every frame. The session is not held
    /// meanwhile.
    pub fn audio_devices(&self) -> AudioDevices {
        let host = self.audio_host.clone();
        let (answer, devices) = std::sync::mpsc::channel();
        let asked = std::thread::Builder::new().name("audio-devices".into()).spawn(move || {
            let _ = answer.send(host.devices());
        });
        match asked {
            Ok(_) => devices.recv_timeout(DEVICE_LIST_TIMEOUT).unwrap_or_else(|_| {
                log::error!("the audio devices could not be listed in time");
                AudioDevices::default()
            }),
            Err(error) => {
                log::error!("cannot start a thread to list the audio devices: {error}");
                AudioDevices::default()
            }
        }
    }

    /// The audio stream as it stands: what was asked for, what is running,
    /// its latency and load. Cheap.
    pub fn audio_status(&self) -> AudioStatus {
        let session = self.session.lock();
        AudioStatus {
            config: session.audio_config().clone(),
            info: session.audio_info().clone(),
            latency_ms: session.output_latency_ms(),
            load: session.audio_load(),
            revision: session.audio_revision(),
        }
    }

    /// Chooses the audio device, sample rate and buffer size, opens the
    /// stream now and remembers the choice for the next run. The project
    /// and the playhead are untouched; see
    /// [`Session::set_audio_config`](trill_engine::Session::set_audio_config)
    /// for what a change of sample rate costs.
    ///
    /// This does not fail: what cannot be had is replaced by the nearest
    /// thing, and the status that comes back says what is running and, in
    /// `info.warning`, why it is not what was asked for. The choice is
    /// remembered as it was made. The error is for the settings file: the
    /// stream has changed even if the choice could not be written down.
    pub fn set_audio(&self, config: AudioConfig) -> (AudioStatus, Result<(), SaveError>) {
        self.session.lock().set_audio_config(config.clone());
        let saved = self.update_settings(|settings| settings.audio = audio_settings(&config));
        (self.audio_status(), saved)
    }

    // ---- MIDI inputs ---------------------------------------------------

    /// The MIDI inputs: every port that is plugged in, and the ones that
    /// are switched off even while they are away. As of the last look at
    /// the ports, which happens once a second; cheap. Empty in a run
    /// without audio hardware, which opens no MIDI ports either.
    ///
    /// What an input that is switched on sends plays the armed tracks, or
    /// the selected track when none is armed; see
    /// [`Session::set_armed`](trill_engine::Session::set_armed).
    pub fn midi_inputs(&self) -> Vec<MidiInputInfo> {
        self.midi.as_ref().map(MidiInputs::inputs).unwrap_or_default()
    }

    /// Switches a MIDI input on or off and remembers it. A port is on
    /// unless it was switched off, one that has never been seen before
    /// included. Switching off an input that has keys down lets go of
    /// them. The port is opened or closed in the background;
    /// [`Runtime::midi_inputs`] shows the outcome a moment later, or at
    /// once after [`Runtime::rescan_midi_inputs`].
    pub fn set_midi_input_enabled(&self, name: &str, enabled: bool) -> Result<(), SaveError> {
        let Some(midi) = &self.midi else { return Ok(()) };
        midi.set_enabled(name, enabled);
        let disabled = midi.disabled();
        self.update_settings(|settings| settings.midi.disabled_inputs = disabled)
    }

    /// Looks at the MIDI ports now rather than at the next second and
    /// waits for that (a moment; at most a few seconds if a driver is
    /// slow). Returns whether the look was finished: if not,
    /// [`Runtime::midi_inputs`] is as it was.
    pub fn rescan_midi_inputs(&self) -> bool {
        self.midi.as_ref().is_none_or(|midi| midi.rescan(MIDI_RESCAN_TIMEOUT))
    }

    /// Projects that were not saved before the program last stopped (it
    /// crashed, or was killed), most recent first. A front end shows them
    /// once at startup and lets the user choose: [`Runtime::recover`] or
    /// [`Runtime::discard`]. Left alone, they are offered again next time.
    pub fn recoverable(&self) -> Vec<SafetyCopy> {
        self.copies.lock().orphans()
    }

    /// Opens a project that was lost with the program. It comes back as it
    /// was when its last safety copy was made (at most twenty seconds
    /// before the end), in the place of the file it was opened from but not
    /// yet written there: it has unsaved changes, and saving is the user's
    /// to do.
    pub fn recover(&self, copy: &SafetyCopy) -> Result<(), SessionError> {
        self.session.lock().load_recovered(&copy.file, copy.original.as_deref())?;
        // From here the project is this program's to keep safe. Only when
        // it has a copy of its own is the old one let go.
        recovery::refresh(&self.session, &self.copies);
        let mut copies = self.copies.lock();
        if copies.copied().is_some() {
            copies.discard(copy);
        }
        Ok(())
    }

    /// Deletes a project that was lost with the program, for good.
    pub fn discard(&self, copy: &SafetyCopy) {
        self.copies.lock().discard(copy);
    }

    /// Brings the safety copy of the open project up to date now rather
    /// than at the housekeeping thread's next look: made anew if there are
    /// unsaved changes it lacks, removed if there are none. Cheap when
    /// nothing changed. For a front end about to do something risky.
    pub fn refresh_safety_copy(&self) {
        recovery::refresh(&self.session, &self.copies);
    }

    /// Ends this runtime the way a crash would: nothing is tidied away,
    /// and the safety copy stays where it is. For tests of what the next
    /// program finds.
    #[doc(hidden)]
    pub fn abandon(self) {
        self.copies.lock().abandon();
    }

    /// The plugin host, when plugins are on: for rescanning and for what
    /// could not be scanned.
    pub fn plugins(&self) -> Option<&PluginHost> {
        self.plugins.as_ref()
    }

    /// The user's settings as they stand, with the library's part taken
    /// from the library as it is now.
    pub fn settings(&self) -> Settings {
        self.settings.lock().with_library(&self.library)
    }

    /// The folder the settings file is kept in, for a front end that keeps
    /// a small file of its own beside it (what its browser was last
    /// showing, say). `None` when nothing is kept between runs
    /// ([`Storage::Nowhere`]): such a file must then not be read or written
    /// either.
    pub fn settings_folder(&self) -> Option<std::path::PathBuf> {
        self.settings.lock().folder().map(std::path::Path::to_path_buf)
    }

    /// What could not be understood in the settings file at startup and
    /// what was done about it, for showing to the user once.
    pub fn settings_warning(&self) -> Option<String> {
        self.settings.lock().warning().map(str::to_string)
    }

    /// Changes the settings and saves them. For the parts that belong to a
    /// front end (theme, window size, recent projects); the library's part
    /// is the library's own and is overwritten with it on every save.
    ///
    /// With [`Storage::Nowhere`] the change lasts as long as the runtime.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<(), SaveError> {
        self.settings.lock().update(&self.library, change)
    }

    /// Saves the library's folders, favorites and recently used files now
    /// if they have changed, rather than at the housekeeping thread's next
    /// look. For a front end that is about to tell someone a folder was
    /// added. Writes nothing when nothing changed.
    pub fn save_library_changes(&self) -> Result<(), SaveError> {
        self.settings.lock().save_library_changes(&self.library).map(|_| ())
    }
}

/// Front ends share the runtime between threads.
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<Runtime>();
};

/// The audio settings as the engine takes them.
fn audio_config(settings: &AudioSettings) -> AudioConfig {
    AudioConfig {
        output: settings.output_device.clone(),
        input: settings.input_device.clone(),
        sample_rate: settings.sample_rate,
        buffer_size: settings.buffer_size,
        exclusive: settings.exclusive,
    }
}

/// What the engine was asked for, as the settings keep it.
fn audio_settings(config: &AudioConfig) -> AudioSettings {
    AudioSettings {
        output_device: config.output.clone(),
        input_device: config.input.clone(),
        sample_rate: config.sample_rate,
        buffer_size: config.buffer_size,
        exclusive: config.exclusive,
    }
}

impl Drop for Runtime {
    /// Saves the library's folders, favorites and recently used files if
    /// they changed since the housekeeping thread last looked. Nothing is
    /// written otherwise: a run that changed nothing leaves no trace. The
    /// thread itself is stopped right after this, as the fields go.
    fn drop(&mut self) {
        housekeeping::save_if_changed(&self.library, &self.settings);
    }
}
