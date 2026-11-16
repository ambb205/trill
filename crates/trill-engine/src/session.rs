//! The session: one open project, its undo history and the engine playing
//! it. This is the single API every front end drives: the GUI, the MCP
//! server and tests all go through here, so they cannot disagree about what
//! an edit means.

use crate::compile::{Compiler, DeviceBank, Meters, Problem, StandIns};
use crate::engine::{AudioBackend, Engine, EngineError, Message, StreamInfo};
use crate::graph::Meter;
use crate::render::RenderJob;
use crate::sources::AudioSources;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{DeviceDescriptor, DeviceFactory, EventKind, ParamInfo};
use trill_core::project::AutomationTarget;
use trill_core::{ChainRef, Device, DeviceId, EditError, Project, TrackId};
use trill_dsp::{SampleRef, StateError, drumrack, sampler};
use trill_media::MediaCache;

mod editors;

const UNDO_LIMIT: usize = 200;
const LIMITER_URI: &str = "builtin:limiter";
const SAMPLER_URI: &str = "builtin:sampler";
const DRUM_RACK_URI: &str = "builtin:drumrack";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error("{0}")]
    Device(String),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("{0}")]
    Io(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransportStatus {
    pub playing: bool,
    pub position_beats: f64,
}

/// A parameter with its current value.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamState {
    pub info: ParamInfo,
    pub value: f64,
}

/// An audio file a sample-playing device uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleFile {
    /// What the device uses it for: `sample` on a sampler, `pad 36` on a
    /// drum rack.
    pub slot: String,
    pub path: String,
    /// Why the file is not playing, if it is not.
    pub problem: Option<String>,
}

/// How much of the engine an edit can have affected.
#[derive(Copy, Clone, PartialEq)]
enum Scope {
    /// Anything: the graph is recompiled.
    Everything,
    /// Only device parameter values, which running devices take directly.
    /// Skipping the recompile keeps slider drags cheap on large projects.
    ParamsOnly,
}

pub struct Session {
    project: Project,
    undo: Vec<Project>,
    redo: Vec<Project>,
    // Field order is drop order: the engine (which owns every running
    // device processor) and the bank (their controllers) must go before the
    // factory that created them, so plugins are torn down while their host
    // is still alive.
    engine: Engine,
    bank: DeviceBank,
    factory: Arc<dyn DeviceFactory>,
    meters: Meters,
    audio: AudioSources,
    /// The clips playing a stand-in until their stretched audio is ready.
    stand_ins: StandIns,
    /// How many stretches had ended when the graph was last compiled. More
    /// since then means a stand-in may be ready to be replaced.
    stretches_seen: u64,
    problems: Vec<Problem>,
    path: Option<PathBuf>,
    /// Bumped on every change to the project, so views know when to refresh.
    revision: u64,
    saved_revision: u64,
    /// `Some(has_step)` while a gesture is open; see [`Session::begin_gesture`].
    gesture: Option<bool>,
    metronome: bool,
}

impl Session {
    /// Starts the audio engine and opens an empty project, reading audio
    /// files through a cache of its own.
    ///
    /// An application wants [`Session::with_media`] instead, so that the
    /// session, the sample-playing devices and the browser decode each file
    /// once between them.
    pub fn new(factory: Arc<dyn DeviceFactory>, backend: AudioBackend) -> Result<Self, SessionError> {
        Self::with_media(factory, Arc::new(MediaCache::default()), backend)
    }

    /// Starts the audio engine and opens an empty project. Audio clips are
    /// read through `media`, which should be the loader `factory` gives its
    /// sample players (`trill_dsp::Builtins::with_loader`).
    ///
    /// The cache is told the engine's sample rate, so that what it hands
    /// those devices is already converted to it.
    pub fn with_media(
        factory: Arc<dyn DeviceFactory>,
        media: Arc<MediaCache>,
        backend: AudioBackend,
    ) -> Result<Self, SessionError> {
        let engine = Engine::start(backend)?;
        media.set_loader_rate(Some(engine.sample_rate()));
        let mut session = Self {
            project: Project::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            factory,
            engine,
            bank: DeviceBank::default(),
            meters: Meters::default(),
            audio: AudioSources::new(media),
            stand_ins: StandIns::default(),
            stretches_seen: 0,
            problems: Vec::new(),
            path: None,
            revision: 0,
            saved_revision: 0,
            gesture: None,
            metronome: false,
        };
        session.replace_project(Self::blank_project("Untitled"));
        Ok(session)
    }

    /// An empty project with a safety limiter on the master.
    fn blank_project(name: &str) -> Project {
        let mut project = Project::new(name);
        project.add_device(ChainRef::Master, LIMITER_URI, "Limiter", None).expect("the master chain always exists");
        project
    }

    // ---- reading -------------------------------------------------------

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_modified(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn audio_info(&self) -> &StreamInfo {
        self.engine.info()
    }

    /// Fraction of the audio thread's time budget in use.
    pub fn audio_load(&self) -> f32 {
        self.engine.load()
    }

    pub fn audio_faults(&self) -> u32 {
        self.engine.faults()
    }

    /// What is wrong with the project as it plays now: plugins that failed
    /// to load, samples and audio files that are missing or unreadable. The
    /// project still plays without them. A problem goes away when the
    /// project is next changed after its cause has.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The cache this session reads audio files through. Front ends use it
    /// for the same files: waveforms, previews, lengths.
    pub fn media(&self) -> &Arc<MediaCache> {
        self.audio.media()
    }

    /// How many audio clips are playing a stand-in (in time, but repitched)
    /// because their warped audio is still being prepared. They switch to
    /// the real thing as [`Session::housekeeping`] finds it ready.
    pub fn preparing(&self) -> usize {
        self.stand_ins.len()
    }

    /// The audio files a sample-playing device uses, with what is wrong
    /// with each. Empty for devices that play no samples.
    pub fn sample_files(&self, id: DeviceId) -> Result<Vec<SampleFile>, SessionError> {
        let device = self.project.device(id)?;
        let warnings = self.bank.devices.get(&id).map(|meta| meta.warnings.as_slice()).unwrap_or_default();
        let files = trill_dsp::sample_refs(device).into_iter().map(|SampleRef { slot, path }| {
            let warning = warnings.iter().find(|warning| warning.slot.as_deref() == Some(slot.as_str()));
            SampleFile { problem: warning.map(|warning| warning.message.clone()), slot, path }
        });
        Ok(files.collect())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn track_meter(&self, track: TrackId) -> Option<Arc<Meter>> {
        self.meters.tracks.get(&track).cloned()
    }

    pub fn master_meter(&self) -> Arc<Meter> {
        self.meters.master.clone()
    }

    /// Every device that can be added to a chain.
    ///
    /// This can block while plugins are being scanned for the first time.
    /// Callers that share the session should take [`Session::factory`] and
    /// ask it after releasing the session lock.
    pub fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.factory.descriptors()
    }

    /// The factory that creates this session's devices.
    pub fn factory(&self) -> Arc<dyn DeviceFactory> {
        self.factory.clone()
    }

    /// The parameters of a device in the project, with current values.
    pub fn device_params(&self, id: DeviceId) -> Result<Vec<ParamState>, SessionError> {
        let device = self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(error) = &meta.error {
            return Err(SessionError::Device(format!("{id} is unavailable: {error}")));
        }
        // A plugin keeps its own values (from its saved state, or changed in
        // its editor); ask it. Values set through the session are in the
        // model and win, since the plugin only takes them on at its next
        // audio block.
        let live = meta.controller.as_ref().and_then(|controller| controller.lock().param_values());
        Ok(meta
            .params
            .iter()
            .enumerate()
            .map(|(index, info)| {
                let reported = live.as_ref().and_then(|values| values.get(index)).copied();
                ParamState {
                    value: device.params.get(&info.id).copied().or(reported).unwrap_or(info.default),
                    info: info.clone(),
                }
            })
            .collect())
    }

    // ---- editing -------------------------------------------------------

    /// Applies an edit as one undo step. If `edit` fails, the project is
    /// left exactly as it was.
    pub fn edit<T>(&mut self, edit: impl FnOnce(&mut Project) -> Result<T, EditError>) -> Result<T, SessionError> {
        self.edit_scoped(Scope::Everything, edit)
    }

    fn edit_scoped<T>(
        &mut self,
        scope: Scope,
        edit: impl FnOnce(&mut Project) -> Result<T, EditError>,
    ) -> Result<T, SessionError> {
        let before = self.project.clone();
        match edit(&mut self.project) {
            Ok(value) => {
                self.commit(before, scope);
                Ok(value)
            }
            Err(error) => {
                self.project = before;
                Err(error.into())
            }
        }
    }

    /// Starts a gesture: until [`Session::end_gesture`], all edits collapse
    /// into a single undo step. Use it around continuous interactions such
    /// as dragging a fader or a clip, which edit on every mouse move.
    pub fn begin_gesture(&mut self) {
        self.gesture = Some(false);
    }

    pub fn end_gesture(&mut self) {
        self.gesture = None;
    }

    fn commit(&mut self, mut before: Project, scope: Scope) {
        if self.project == before {
            return;
        }
        self.keep_removed_state(&mut before);
        self.sync(&before, scope);
        // Inside a gesture only the state before its first edit is kept.
        if self.gesture != Some(true) {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        if let Some(has_step) = &mut self.gesture {
            *has_step = true;
        }
        self.redo.clear();
        self.revision += 1;
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else { return false };
        let mut current = std::mem::replace(&mut self.project, previous);
        self.keep_removed_state(&mut current);
        self.sync(&current, Scope::Everything);
        self.redo.push(current);
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        let mut current = std::mem::replace(&mut self.project, next);
        self.keep_removed_state(&mut current);
        self.sync(&current, Scope::Everything);
        self.undo.push(current);
        self.revision += 1;
        true
    }

    /// Adds a device, optionally starting from one of its presets. Fails,
    /// leaving the project untouched, if the device cannot be instantiated.
    pub fn add_device(
        &mut self,
        chain: ChainRef,
        uri: &str,
        preset: Option<&str>,
        index: Option<usize>,
    ) -> Result<DeviceId, SessionError> {
        let descriptor = self
            .factory
            .descriptors()
            .into_iter()
            .find(|d| d.uri == uri)
            .ok_or_else(|| SessionError::Device(format!("unknown device '{uri}'")))?;
        let preset_params = match preset {
            None => Vec::new(),
            Some(name) => {
                descriptor.presets.iter().find(|p| p.name == name).map(|p| p.params.clone()).ok_or_else(|| {
                    let known: Vec<&str> = descriptor.presets.iter().map(|p| p.name.as_str()).collect();
                    SessionError::Device(format!("'{uri}' has no preset '{name}' (available: {})", known.join(", ")))
                })?
            }
        };

        let id = self.edit(|project| {
            let id = project.add_device(chain, uri, descriptor.name.clone(), index)?;
            project.device_mut(id)?.params.extend(preset_params);
            Ok(id)
        })?;

        let failure = self.bank.devices.get(&id).and_then(|meta| meta.error.clone());
        if let Some(reason) = failure {
            self.undo();
            self.redo.pop();
            return Err(SessionError::Device(reason));
        }
        Ok(id)
    }

    /// Resets a device to one of its presets: every parameter returns to
    /// its default, then the preset's values are applied.
    pub fn apply_preset(&mut self, id: DeviceId, preset: &str) -> Result<(), SessionError> {
        let uri = self.project.device(id)?.uri.clone();
        let descriptor = self.factory.descriptors().into_iter().find(|d| d.uri == uri);
        let params = descriptor
            .and_then(|d| d.presets.into_iter().find(|p| p.name == preset))
            .map(|p| p.params)
            .ok_or_else(|| SessionError::Device(format!("'{uri}' has no preset '{preset}'")))?;
        self.edit_scoped(Scope::ParamsOnly, |project| {
            project.device_mut(id)?.params = params.into_iter().collect();
            Ok(())
        })
    }

    /// Replaces a device's saved state, which for built-in sample players
    /// holds what they play. The device is created again from the new state;
    /// one undo step.
    pub fn set_device_state(&mut self, id: DeviceId, state: Option<Vec<u8>>) -> Result<(), SessionError> {
        self.edit(|project| {
            project.device_mut(id)?.state = state;
            Ok(())
        })?;
        match self.bank.devices.get(&id).and_then(|meta| meta.error.clone()) {
            Some(reason) => Err(SessionError::Device(reason)),
            None => Ok(()),
        }
    }

    /// Gives a sampler (`builtin:sampler`) the file it plays, or empties it
    /// with `None`.
    ///
    /// One undo step. The device is created again from its new state, with
    /// its parameter values as they were, and the song keeps playing. Fails,
    /// changing nothing, if the device is not a sampler or the file cannot
    /// be read.
    pub fn set_sampler_file(&mut self, id: DeviceId, path: Option<&str>) -> Result<(), SessionError> {
        let device = self.sample_device(id, SAMPLER_URI, "sampler")?;
        let current = readable(id, sampler::State::from_device_state(device.state.as_deref()))?;
        let mut state = current.clone().unwrap_or_default();
        if let Some(path) = path {
            self.check_sample(path)?;
        }
        state.path = path.map(str::to_string);
        if current.as_ref() == Some(&state) {
            return Ok(());
        }
        self.replace_sample_state(id, state.to_bytes())
    }

    /// Puts a file on the pad of a drum rack (`builtin:drumrack`) that
    /// `note` plays, or empties the pad with `None`. The pads are notes
    /// 36 to 51; see `trill_dsp::drumrack`.
    ///
    /// One undo step, like [`Session::set_sampler_file`], and with the same
    /// guarantees.
    pub fn set_pad_file(&mut self, id: DeviceId, note: u8, path: Option<&str>) -> Result<(), SessionError> {
        let device = self.sample_device(id, DRUM_RACK_URI, "drum rack")?;
        let current = readable(id, drumrack::State::from_device_state(device.state.as_deref()))?;
        let mut state = current.clone().unwrap_or_default();
        let pads = drumrack::PAD_NOTES;
        if !pads.contains(&note) {
            return Err(SessionError::Device(format!(
                "a drum rack has no pad for note {note}; its pads are notes {} to {}",
                pads.start(),
                pads.end()
            )));
        }
        match path {
            Some(path) => {
                self.check_sample(path)?;
                state.set_pad(note, path).map_err(|error| SessionError::Device(error.to_string()))?;
            }
            None => {
                state.clear_pad(note);
            }
        }
        if current.as_ref() == Some(&state) {
            return Ok(());
        }
        self.replace_sample_state(id, state.to_bytes())
    }

    /// The device `id`, if it is a `uri`.
    fn sample_device(&self, id: DeviceId, uri: &str, what: &str) -> Result<&Device, SessionError> {
        let device = self.project.device(id)?;
        if device.uri == uri {
            Ok(device)
        } else {
            Err(SessionError::Device(format!(
                "{id} ({}) is not a {what}, so it has no sample to set; add a {uri} device for that",
                device.name
            )))
        }
    }

    /// Refuses a sample that cannot be read, in words that say so: what a
    /// device does with a bad file is play silence, which helps nobody who
    /// has just chosen it.
    fn check_sample(&self, path: &str) -> Result<(), SessionError> {
        match self.audio.media().audio(Path::new(path)) {
            Ok(_) => Ok(()),
            Err(error) => Err(SessionError::Io(format!("{error}; the device keeps the sample it had"))),
        }
    }

    /// Stores a sample player's new state as one undo step, and takes the
    /// step back if the device cannot be created from it.
    fn replace_sample_state(&mut self, id: DeviceId, state: Vec<u8>) -> Result<(), SessionError> {
        let revision = self.revision;
        let result = self.set_device_state(id, Some(state));
        if result.is_err() && self.revision != revision {
            self.undo();
            self.redo.pop();
        }
        result
    }

    /// Sets parameters by id, in plain units. Values are clamped to range.
    pub fn set_device_params(&mut self, id: DeviceId, values: &[(String, f64)]) -> Result<(), SessionError> {
        let states = self.device_params(id)?;
        let mut resolved = Vec::with_capacity(values.len());
        for (key, value) in values {
            let state = states
                .iter()
                .find(|state| state.info.id == *key)
                .ok_or_else(|| EditError::Invalid(format!("{id} has no parameter '{key}'")))?;
            resolved.push((key.clone(), state.info.clamp(*value)));
        }
        self.edit_scoped(Scope::ParamsOnly, |project| {
            project.device_mut(id)?.params.extend(resolved);
            Ok(())
        })
    }

    // ---- transport -----------------------------------------------------

    pub fn play(&mut self) {
        self.engine.send(Message::Play);
    }

    pub fn stop(&mut self) {
        self.engine.send(Message::Stop);
    }

    pub fn seek(&mut self, beat: f64) {
        self.engine.send(Message::Seek(beat.max(0.0)));
    }

    /// Silences everything at once, tails included.
    pub fn panic(&mut self) {
        self.engine.send(Message::Stop);
        self.engine.send(Message::Panic);
    }

    pub fn transport(&self) -> TransportStatus {
        TransportStatus { playing: self.engine.is_playing(), position_beats: self.engine.position() }
    }

    /// Plays a sound over the song without adding it to the project, as a
    /// browser does when a sample is clicked. Replaces any sound already
    /// being auditioned.
    pub fn audition(&mut self, audio: Arc<trill_core::audio::AudioBuffer>, gain_db: f32) {
        self.engine.collect_garbage();
        self.engine.send(Message::Audition { audio, gain: 10f32.powf(gain_db / 20.0) });
    }

    pub fn stop_audition(&mut self) {
        self.engine.send(Message::StopAudition);
    }

    pub fn is_auditioning(&self) -> bool {
        self.engine.is_auditioning()
    }

    /// Turns the metronome on or off. It clicks on every count of the time
    /// signature while the song plays and is never part of a render.
    pub fn set_metronome(&mut self, on: bool) {
        self.metronome = on;
        self.engine.send(Message::Metronome(on));
    }

    pub fn metronome(&self) -> bool {
        self.metronome
    }

    /// Plays a note on a track's instrument right now, for auditioning.
    pub fn note_on(&mut self, track: TrackId, pitch: u8, velocity: u8) {
        let velocity = velocity.clamp(1, 127) as f32 / 127.0;
        self.engine.send(Message::Live { track, kind: EventKind::NoteOn { pitch: pitch.min(127), velocity } });
    }

    pub fn note_off(&mut self, track: TrackId, pitch: u8) {
        self.engine.send(Message::Live { track, kind: EventKind::NoteOff { pitch: pitch.min(127) } });
    }

    // ---- files ---------------------------------------------------------

    pub fn new_project(&mut self) {
        self.replace_project(Self::blank_project("Untitled"));
        self.path = None;
    }

    pub fn save(&mut self, path: &Path) -> Result<(), SessionError> {
        self.capture_device_state();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| io_error(path, e))?;
        }
        // Write beside the target and rename, so a crash mid-save cannot
        // leave a truncated project file.
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, self.project.to_json()).map_err(|e| io_error(path, e))?;
        std::fs::rename(&temporary, path).map_err(|e| io_error(path, e))?;
        self.path = Some(path.to_path_buf());
        self.saved_revision = self.revision;
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<(), SessionError> {
        let json = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
        let project = Project::from_json(&json)?;
        self.replace_project(project);
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    /// A snapshot that can be rendered without holding the session.
    pub fn render_job(&mut self) -> RenderJob {
        self.capture_device_state();
        RenderJob { project: self.project.clone(), factory: self.factory.clone(), audio: self.audio.clone() }
    }

    /// The session's regular chores; call it a few times a second. Frees
    /// memory the audio thread has released, and gives clips that were
    /// playing a stand-in their warped audio once it has been prepared.
    pub fn housekeeping(&mut self) {
        self.engine.collect_garbage();
        if !self.stand_ins.is_empty() && self.audio.finished() != self.stretches_seen {
            self.compile_and_swap();
        }
    }

    /// Waits until no clip is playing a stand-in any more, or `timeout` has
    /// passed, and returns whether none is. For tests and tools: it blocks
    /// whoever holds the session, which a front end must not do. Those call
    /// [`Session::housekeeping`] and watch [`Session::preparing`].
    pub fn wait_until_prepared(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.stand_ins.is_empty() {
            if !self.audio.wait_for_progress(self.stretches_seen, deadline) {
                return false;
            }
            self.housekeeping();
        }
        true
    }

    // ---- internals -----------------------------------------------------

    /// Swaps in a different project wholesale: no undo history carries over
    /// and every device is created fresh.
    fn replace_project(&mut self, project: Project) {
        self.engine.send(Message::Stop);
        self.engine.send(Message::Seek(0.0));
        self.project = project;
        self.undo.clear();
        self.redo.clear();
        // Ids restart in every project; nothing running may be mistaken for
        // a device or a clip of the new one.
        self.bank.devices.clear();
        self.stand_ins = StandIns::default();
        self.compile_and_swap();
        self.revision += 1;
        self.saved_revision = self.revision;
    }

    fn compile_and_swap(&mut self) {
        let sample_rate = self.engine.sample_rate();
        // Read before compiling: a stretch that ends while the compile runs
        // may be too late for it, and must still count as news afterwards.
        self.stretches_seen = self.audio.finished();
        let (graph, problems) = Compiler {
            factory: self.factory.as_ref(),
            bank: &mut self.bank,
            meters: &mut self.meters,
            audio: &self.audio,
            stand_ins: Some(&mut self.stand_ins),
            sample_rate,
        }
        .compile(&self.project, true);
        self.problems = problems;
        self.engine.swap(graph);
    }

    /// Brings the engine in line with the project after it changed from
    /// `old`.
    fn sync(&mut self, old: &Project, scope: Scope) {
        if scope == Scope::Everything {
            self.compile_and_swap();
        }

        // Devices that were already running keep their instance, so changed
        // values have to be sent to them.
        for device in self.project.devices() {
            let (Some(meta), Ok(previous)) = (self.bank.devices.get(&device.id), old.device(device.id)) else {
                continue;
            };
            for (index, info) in meta.params.iter().enumerate() {
                let value = device.params.get(&info.id);
                if value != previous.params.get(&info.id) {
                    let value = value.copied().unwrap_or(info.default);
                    self.engine.send(Message::Param { device: device.id, index, value });
                }
            }
        }

        // A parameter whose automation was removed returns to its set value.
        for lane in &old.automation {
            let AutomationTarget::DeviceParam { device, param } = &lane.target else { continue };
            if self.project.automation.iter().any(|l| l.target == lane.target) {
                continue;
            }
            let (Some(meta), Ok(model)) = (self.bank.devices.get(device), self.project.device(*device)) else {
                continue;
            };
            if let Some(index) = meta.params.iter().position(|info| info.id == *param) {
                let value = model.params.get(param).copied().unwrap_or(meta.params[index].default);
                self.engine.send(Message::Param { device: *device, index, value });
            }
        }
    }

    /// Before `leaving` goes onto the undo or redo stack, stores the state
    /// of the plugins it has that the current project does not, so that
    /// coming back to it restores them as they were.
    fn keep_removed_state(&mut self, leaving: &mut Project) {
        let removed: Vec<DeviceId> =
            leaving.devices().map(|d| d.id).filter(|id| self.project.device(*id).is_err()).collect();
        for id in removed {
            let state = self
                .bank
                .devices
                .get(&id)
                .and_then(|meta| meta.controller.as_ref())
                .and_then(|controller| controller.lock().save_state());
            if let (Some(state), Ok(device)) = (state, leaving.device_mut(id)) {
                device.state = Some(state);
            }
        }
    }

    /// Copies each plugin's current state into the project model. This is
    /// bookkeeping, not an edit: it creates no undo step.
    ///
    /// What a plugin changed on its own (in its editor, say) is not part of
    /// the undo history, so the new state also replaces the stale copy in
    /// every history entry that still holds it. Otherwise undoing an
    /// unrelated edit would bring back the old state and reset the plugin.
    fn capture_device_state(&mut self) {
        let ids: Vec<DeviceId> = self.project.devices().map(|d| d.id).collect();
        for id in ids {
            let Some(meta) = self.bank.devices.get_mut(&id) else { continue };
            let Some(captured) = meta.controller.as_ref().and_then(|controller| controller.lock().save_state()) else {
                continue;
            };
            let Ok(device) = self.project.device_mut(id) else { continue };
            if device.state.as_ref() == Some(&captured) {
                continue;
            }
            let stale = device.state.replace(captured.clone());
            meta.applied_state = Some(captured.clone());
            for snapshot in self.undo.iter_mut().chain(self.redo.iter_mut()) {
                if let Ok(device) = snapshot.device_mut(id)
                    && device.state == stale
                {
                    device.state = Some(captured.clone());
                }
            }
        }
    }
}

/// The state document of a sample player, for changing. `None` for one that
/// cannot be parsed: it has nothing in it worth keeping and is replaced by
/// a fresh one. One from a newer version of the program is left alone.
fn readable<S>(id: DeviceId, state: Result<S, StateError>) -> Result<Option<S>, SessionError> {
    match state {
        Ok(state) => Ok(Some(state)),
        Err(StateError::Invalid(_)) => Ok(None),
        Err(error @ StateError::TooNew { .. }) => {
            Err(SessionError::Device(format!("{id}: {error}; its samples can only be changed by that version")))
        }
    }
}

fn io_error(path: &Path, error: std::io::Error) -> SessionError {
    SessionError::Io(format!("{}: {error}", path.display()))
}
