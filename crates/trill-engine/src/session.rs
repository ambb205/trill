//! The session: one open project, its undo history and the engine playing
//! it. This is the single API every front end drives: the GUI, the MCP
//! server and tests all go through here, so they cannot disagree about what
//! an edit means.

use crate::compile::{Compiler, DeviceBank, Meters, Problem, StandIns};
use crate::engine::{AudioBackend, AudioSetup, Engine, EngineError, Message, StreamInfo};
use crate::graph::Meter;
use crate::instruments::InstrumentKnowledge;
use crate::render::RenderJob;
use crate::saved::{SavedPresets, WithSaved};
use crate::sources::AudioSources;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{DeviceDescriptor, DeviceFactory, EventKind, ParamInfo};
use trill_core::journal::Reversal;
use trill_core::presets::PresetInfo;
use trill_core::project::AutomationTarget;
use trill_core::{ChainRef, Device, DeviceId, EditError, Project, TrackId};
use trill_dsp::{SampleRef, StateError, drumrack, sampler};
use trill_media::MediaCache;

mod controllers;
mod editors;
mod files;
mod instruments;
mod params;
mod playing;
mod presets;
mod probing;
mod sounds;
mod story;
mod take;
mod wavetable;

pub use sounds::{Added, Given, ListedSound, SavedAsSound, listed_sounds, sound_after};
pub use wavetable::{TableReport, factory_tables};

pub use controllers::SilentInstrument;
pub use files::file_facts;
pub use playing::{MAX_COUNT_IN_BARS, RecordState, RecordStatus};
pub use probing::ProbeTarget;
pub use take::TakeClip;

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
    /// Arming or recording was asked for and cannot be done as things
    /// stand; the text says what to do about it.
    #[error("{0}")]
    Recording(String),
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

/// Identifies one step of the undo history. A step keeps its id as it goes
/// from the undo side to the redo side and back, so a front end can ask
/// whether the step it means is still the one [`Session::undo`] would take
/// back ([`Session::undo_step`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct UndoStep(u64);

/// An undo group that is open; see [`Session::begin_group`]. It is closed
/// by handing it back to [`Session::end_group`].
#[derive(Debug, PartialEq, Eq)]
#[must_use = "an undo group stays open until it is handed to Session::end_group"]
pub struct UndoGroup(u64);

/// One step of the undo history: the project as it was before the step on
/// the undo side, as it was after it on the redo side.
struct Step {
    id: UndoStep,
    project: Project,
    /// What the step is in the journal.
    story: story::Story,
}

/// A group between its [`Session::begin_group`] and [`Session::end_group`].
struct OpenGroup {
    id: u64,
    /// How many undo steps are older than the group. The steps above them
    /// are the group's, and become one when it closes.
    floor: usize,
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
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The undo groups that are open; see [`Session::begin_group`].
    groups: Vec<OpenGroup>,
    /// The last id given to an undo step or a group.
    last_id: u64,
    // Field order is drop order: the engine (which owns every running
    // device processor) and the bank (their controllers) must go before the
    // factory that created them, so plugins are torn down while their host
    // is still alive.
    engine: Engine,
    bank: DeviceBank,
    factory: Arc<dyn DeviceFactory>,
    /// The presets someone saved, which `factory` lists with each device
    /// type's own.
    saved: Arc<SavedPresets>,
    /// What is known about instruments by device type, which a track is
    /// given along with its instrument.
    instruments: Arc<InstrumentKnowledge>,
    /// Where renders, measurements and probes of songs with plugins are
    /// done, if not in this process: see [`Session::do_jobs_apart`].
    apart: Option<crate::apart::Apart>,
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
    /// Arming, the selection, and the take being recorded.
    playing: playing::Playing,
    /// The journal of the open project: how it came to be.
    story: story::Keeping,
    /// What was written down about the files the song plays when it was
    /// last saved or opened; see `session/files.rs`.
    files: trill_core::FileTable,
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
        Self::with_audio(factory, media, backend.into())
    }

    /// [`Session::with_media`], saying which audio device to open and how
    /// ([`AudioSetup`]): what an application does with the user's saved
    /// choice. If that device is not there or refuses the format, the
    /// nearest thing is opened and [`Session::audio_info`] says so; see
    /// [`Session::set_audio_config`].
    pub fn with_audio(
        factory: Arc<dyn DeviceFactory>,
        media: Arc<MediaCache>,
        setup: AudioSetup,
    ) -> Result<Self, SessionError> {
        let engine = Engine::start_with(setup)?;
        media.set_loader_rate(Some(engine.sample_rate()));
        let saved = Arc::new(SavedPresets::default());
        let factory: Arc<dyn DeviceFactory> = Arc::new(WithSaved::new(factory, saved.clone()));
        let mut session = Self {
            project: Project::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            groups: Vec::new(),
            last_id: 0,
            factory,
            saved,
            instruments: Arc::new(InstrumentKnowledge::default()),
            apart: None,
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
            playing: playing::Playing::new(),
            story: story::Keeping::new(),
            files: trill_core::FileTable::default(),
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

    /// The step [`Session::undo`] would take back now, if there is one.
    pub fn undo_step(&self) -> Option<UndoStep> {
        self.undo.last().map(|step| step.id)
    }

    /// The step [`Session::redo`] would bring back now, if there is one.
    pub fn redo_step(&self) -> Option<UndoStep> {
        self.redo.last().map(|step| step.id)
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
        self.end_journal_gesture();
    }

    /// Opens an undo group: everything that changes the project until
    /// [`Session::end_group`], whoever makes the change, becomes one undo
    /// step when the group closes. Use it around work that spans many calls
    /// and many holds of the session, such as everything an assistant does
    /// in answer to one request.
    ///
    /// A gesture cannot do this. There is one gesture for the whole
    /// session, so the first front end to end one (a fader let go in the
    /// window, a tool that wraps its own edits in a gesture) would close it
    /// for everybody. A group is closed only by whoever holds its
    /// [`UndoGroup`], and what happens inside keeps its usual meaning while
    /// the group is open: edits and whole gestures are steps, and
    /// [`Session::undo`] takes back the last of them, which is what a tool
    /// relies on to back out of an edit it could not finish. Only at the
    /// close do the steps made since the opening become one.
    ///
    /// A gesture that is under way when the group opens or closes is cut
    /// there: what it changed before stays outside the group, what it
    /// changes after is inside. Groups may be open at the same time; each
    /// gathers what was done since it was opened.
    pub fn begin_group(&mut self) -> UndoGroup {
        self.cut_gesture();
        let id = self.fresh_id();
        self.groups.push(OpenGroup { id, floor: self.undo.len() });
        UndoGroup(id)
    }

    /// Closes an undo group. Returns the one step that now holds everything
    /// done while it was open, or `None` if that left the project as it
    /// was: then the history is as if the group had never been opened.
    pub fn end_group(&mut self, group: UndoGroup) -> Option<UndoStep> {
        let index = self.groups.iter().position(|open| open.id == group.0)?;
        let floor = self.groups.remove(index).floor;
        self.cut_gesture();
        if self.undo.len() <= floor {
            return None;
        }
        // The oldest of the group's steps holds the project as it was
        // before any of them; the rest are the stops along the way. In the
        // journal the one step is all of theirs.
        let story = story::Story::joined(self.undo[floor..].iter().map(|step| &step.story));
        self.undo.truncate(floor + 1);
        self.undo[floor].story = story;
        let unchanged = self.undo[floor].project == self.project;
        if unchanged {
            self.undo.truncate(floor);
        }
        // A group opened inside this one has nothing of its own left.
        let depth = self.undo.len();
        for open in &mut self.groups {
            open.floor = open.floor.min(depth);
        }
        (!unchanged).then(|| self.undo[floor].id)
    }

    /// Makes the next edit of a gesture under way a step of its own.
    fn cut_gesture(&mut self) {
        if let Some(has_step) = &mut self.gesture {
            *has_step = false;
        }
        self.end_journal_gesture();
    }

    fn fresh_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    fn commit(&mut self, mut before: Project, scope: Scope) {
        if self.project == before {
            return;
        }
        self.keep_leaving_state(&mut before);
        self.sync(&before, scope);
        self.make_step(before);
    }

    /// Makes the change from `before` to the project as it is now an undo
    /// step. The engine is the caller's to have brought in line.
    fn make_step(&mut self, before: Project) {
        let story = self.record_step(&before);
        // Inside a gesture only the state before its first edit is kept.
        if self.gesture != Some(true) {
            self.push_step(before, story);
        } else if let Some(step) = self.undo.last_mut() {
            step.story.and(story);
        }
        if let Some(has_step) = &mut self.gesture {
            *has_step = true;
        }
        self.redo.clear();
        self.revision += 1;
    }

    /// Puts a step on the undo history, forgetting the oldest one when
    /// there are too many.
    fn push_step(&mut self, before: Project, story: story::Story) {
        // Steps older than a group may have been undone since it opened.
        // An edit made from there is the group's, and so is its step.
        let depth = self.undo.len();
        for open in &mut self.groups {
            open.floor = open.floor.min(depth);
        }
        let id = UndoStep(self.fresh_id());
        self.undo.push(Step { id, project: before, story });
        if self.undo.len() > UNDO_LIMIT {
            // The oldest step goes, unless it is where an open group began:
            // that group must still be able to undo all the way back, so
            // one of its stops along the way goes instead.
            let forgotten = usize::from(self.groups.iter().any(|open| open.floor == 0));
            self.undo.remove(forgotten);
            for open in &mut self.groups {
                if open.floor > forgotten {
                    open.floor -= 1;
                }
            }
        }
    }

    /// Takes back the last step. In the journal the step stays, followed
    /// by one that takes it back; a call that undoes what it has just done
    /// itself backs out of it instead, and leaves no trace.
    pub fn undo(&mut self) -> bool {
        let Some(Step { id, project: previous, story }) = self.undo.pop() else { return false };
        let mut current = std::mem::replace(&mut self.project, previous);
        self.keep_leaving_state(&mut current);
        self.sync(&current, Scope::Everything);
        let story = if self.retract_step(&story, true) {
            story::Story::default()
        } else {
            self.record_reversal(Reversal::Undo, &story, &current);
            Self::settled(story)
        };
        self.redo.push(Step { id, project: current, story });
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(Step { id, project: next, story }) = self.redo.pop() else { return false };
        let mut current = std::mem::replace(&mut self.project, next);
        self.keep_leaving_state(&mut current);
        self.sync(&current, Scope::Everything);
        self.record_reversal(Reversal::Redo, &story, &current);
        self.undo.push(Step { id, project: current, story });
        self.revision += 1;
        true
    }

    /// Takes back the step that was just made and forgets it: for an
    /// operation that could not be finished. Nothing is left to redo, and
    /// the journal has it as if it had not been made.
    fn back_out(&mut self) {
        let Some(Step { project: previous, story, .. }) = self.undo.pop() else { return };
        let mut current = std::mem::replace(&mut self.project, previous);
        self.keep_leaving_state(&mut current);
        self.sync(&current, Scope::Everything);
        if !self.retract_step(&story, false) {
            self.record_reversal(Reversal::Undo, &story, &current);
        }
        self.revision += 1;
    }

    /// Adds a device, optionally starting from one of its presets, asked
    /// for by name or id as [`Session::load_preset`] takes them. One undo
    /// step. Fails, leaving the project untouched, if the device cannot be
    /// instantiated or the preset cannot be found or loaded.
    ///
    /// A preset that is a sound brings its effects, made behind the device
    /// in the same step; [`Session::add_given`] also says which they are.
    pub fn add_device(
        &mut self,
        chain: ChainRef,
        uri: &str,
        preset: Option<&str>,
        index: Option<usize>,
    ) -> Result<DeviceId, SessionError> {
        self.add_given(chain, uri, preset, index).map(|added| added.device)
    }

    /// [`Session::add_device`], with what the preset came to: which preset
    /// it was and, for a sound, the devices that were made behind the
    /// first, the ones an earlier sound had brought there and that went,
    /// and what the sound names that this machine could not make. One undo
    /// step for all of it.
    pub fn add_given(
        &mut self,
        chain: ChainRef,
        uri: &str,
        preset: Option<&str>,
        index: Option<usize>,
    ) -> Result<Added, SessionError> {
        let descriptors = self.factory.descriptors();
        let descriptor = descriptors
            .iter()
            .find(|d| d.uri == uri)
            .cloned()
            .ok_or_else(|| SessionError::Device(format!("unknown device '{uri}'")))?;
        // A track's first instrument brings what is known about its keys.
        let profile = self.profile_for_new_instrument(chain, &descriptor, &descriptors);
        // Found before anything is added, so that a preset there is none
        // of, or a file that is no preset of this device, adds nothing.
        let wanted = preset.map(|wanted| self.wanted_preset(uri, wanted)).transpose()?;
        let (preset, saved) = match wanted {
            Some((info, saved)) => (Some(info), saved),
            None => (None, None),
        };
        // A preset that is a set of parameter values goes into the model
        // with the device, and so does one that was saved, state and all:
        // the device is created as the preset has it. Any other is the
        // device's own to load, once there is a device.
        let values = preset.as_ref().and_then(|info| preset_values(&descriptor, info));
        // What a sound brings behind its device, as far as this machine
        // has it.
        let after = preset.as_ref().and_then(|info| sounds::after_of(Some(&descriptor), info, saved.as_ref()));
        let (after, without) = sounds::makeable(&descriptors, after);

        // The effects of a sound that do not start are taken out again,
        // which is to be part of the one step.
        let own_gesture = self.gesture.is_none() && after.as_ref().is_some_and(|after| !after.is_empty());
        if own_gesture {
            self.begin_gesture();
        }
        let added = self.edit(|project| {
            let id = project.add_device(chain, uri, descriptor.name.clone(), index)?;
            let device = project.device_mut(id)?;
            if let (Some(info), Some(saved)) = (&preset, &saved) {
                device.params = saved.params.clone();
                device.state = saved.state.clone();
                device.preset = Some(info.reference());
            } else if let (Some(info), Some(values)) = (&preset, &values) {
                device.params.extend(values.iter().cloned());
                device.preset = Some(info.reference());
                // A preset with a state of its own brings it: a wavetable
                // synth's tables, and what its macros are called.
                if let Some(state) = preset_state(&descriptor, info) {
                    device.state = Some(state);
                }
            }
            if let Some((track, profile)) = profile {
                project.set_track_profile(track, profile)?;
            }
            let brought = match (&preset, &after) {
                (Some(info), Some(after)) => project.bring_after(id, &info.reference(), after)?,
                _ => trill_core::presets::Brought::default(),
            };
            Ok((id, brought))
        });
        let (id, brought) = match added {
            Ok(added) => added,
            Err(error) => {
                if own_gesture {
                    self.end_gesture();
                }
                return Err(error);
            }
        };

        let failure = self.bank.devices.get(&id).and_then(|meta| meta.error.clone()).map(SessionError::Device);
        let failure = failure.or_else(|| match (&preset, &values, &saved) {
            (Some(info), None, None) => self.start_from_preset(id, info).err(),
            _ => None,
        });
        if let Some(error) = failure {
            self.back_out();
            if own_gesture {
                self.end_gesture();
            }
            return Err(error);
        }
        let given = preset.map(|info| self.given(info, after.is_some(), brought, without));
        if own_gesture {
            self.end_gesture();
        }
        Ok(Added { device: id, given })
    }

    /// Gives a device one of its presets, by the preset's name or id. The
    /// same as [`Session::load_preset`], which also says which preset it
    /// came to.
    pub fn apply_preset(&mut self, id: DeviceId, preset: &str) -> Result<(), SessionError> {
        self.load_preset(id, preset).map(|_| ())
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
            self.back_out();
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
        self.remember_plugin_values(id, &states, &resolved);
        self.edit_scoped(Scope::ParamsOnly, |project| {
            project.device_mut(id)?.params.extend(resolved);
            Ok(())
        })
    }

    // ---- transport -----------------------------------------------------

    pub fn play(&mut self) {
        self.engine.send(Message::Play);
    }

    /// Stops the song. A take that was being recorded ends here: the keys
    /// still down end with it, and what was played is in the project by
    /// the time this returns ([`Session::last_take`]).
    pub fn stop(&mut self) {
        self.engine.send(Message::Stop);
        self.finish_take(true);
    }

    pub fn seek(&mut self, beat: f64) {
        self.engine.send(Message::Seek(beat.max(0.0)));
    }

    /// Silences everything at once, tails included.
    pub fn panic(&mut self) {
        self.engine.send(Message::Stop);
        self.engine.send(Message::Panic);
        self.finish_take(true);
    }

    pub fn transport(&self) -> TransportStatus {
        TransportStatus { playing: self.engine.is_playing(), position_beats: self.engine.position() }
    }

    /// Plays a sound over the song without adding it to the project, as a
    /// browser does when a sample is clicked. Replaces any sound already
    /// being auditioned.
    pub fn audition(&mut self, audio: Arc<trill_core::audio::AudioBuffer>, gain_db: f32) {
        self.engine.collect_garbage();
        self.engine.send(Message::Audition { audio, gain: 10f32.powf(gain_db / 20.0), round: None });
    }

    /// Plays a sound over the song round and round, until
    /// [`Session::stop_audition`] or another audition. With `beats` it
    /// keeps time with the song while the song plays: one round lasts that
    /// many beats, and is read from where the song is, so that a loop
    /// that was made to the song's tempo is heard in its place in the bar.
    /// While the song stands still, and with `None`, it goes round at its
    /// own length.
    pub fn audition_round(&mut self, audio: Arc<trill_core::audio::AudioBuffer>, gain_db: f32, beats: Option<f64>) {
        self.engine.collect_garbage();
        let round = Some(crate::monitor::Round { beats });
        self.engine.send(Message::Audition { audio, gain: 10f32.powf(gain_db / 20.0), round });
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

    /// Plays a note on a track's instrument right now, whether or not the
    /// track is armed. It sounds like a key of a MIDI keyboard would and,
    /// while a take is recorded on that track, is recorded like one.
    pub fn note_on(&mut self, track: TrackId, pitch: u8, velocity: u8) {
        let velocity = velocity.clamp(1, 127) as f32 / 127.0;
        let now = self.engine.now_ns();
        self.engine.live().play_on_at(now, track, EventKind::NoteOn { pitch: pitch.min(127), velocity });
    }

    /// Lets go of a note started with [`Session::note_on`].
    pub fn note_off(&mut self, track: TrackId, pitch: u8) {
        let now = self.engine.now_ns();
        self.engine.live().play_on_at(now, track, EventKind::NoteOff { pitch: pitch.min(127) });
    }

    // ---- files ---------------------------------------------------------

    pub fn new_project(&mut self) {
        self.replace_project(Self::blank_project("Untitled"));
        self.path = None;
    }

    pub fn save(&mut self, path: &Path) -> Result<(), SessionError> {
        // A knob turned in a plugin's own window a moment ago belongs in
        // the file, whether or not housekeeping has come round since.
        self.absorb_editor_edits();
        self.capture_device_state();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| io_error(path, e))?;
        }
        // Write beside the target and rename, so a crash mid-save cannot
        // leave a truncated project file.
        // The song goes into the file with what is known of its files,
        // and where each is from the folder the song is saved in.
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, self.project_text(Some(path))).map_err(|e| io_error(path, e))?;
        std::fs::rename(&temporary, path).map_err(|e| io_error(path, e))?;
        self.path = Some(path.to_path_buf());
        self.saved_revision = self.revision;
        // The journal is written with the song, in the program's own
        // folder: the file just written holds none of it.
        self.write_journal(path);
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<(), SessionError> {
        let json = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
        // A file that is beside the song, where the song says it was when
        // it was saved, is the one it plays: see `session/files.rs`.
        let (project, files) = self.read_project(&json, Some(path))?;
        let project = self.with_known_instruments(project);
        self.replace_project(project);
        self.files = files;
        self.path = Some(path.to_path_buf());
        self.take_up_journal(path);
        Ok(())
    }

    /// The text `save` would write right now, with every device's state as
    /// it is, without saving: where the project lives and whether it has
    /// unsaved changes stay as they were. For keeping a copy in case the
    /// program does not live to save it.
    pub fn snapshot_json(&mut self) -> String {
        self.absorb_editor_edits();
        self.capture_device_state();
        let song = self.path.clone();
        self.project_text(song.as_deref())
    }

    /// Opens `copy`, a project file kept for safety, in place of a project
    /// that was lost with the program. It takes the lost project's place
    /// on disk (`original`) without being written there: the project
    /// counts as changed, and it is the user who decides, by saving,
    /// whether the copy replaces what the file holds.
    pub fn load_recovered(&mut self, copy: &Path, original: Option<&Path>) -> Result<(), SessionError> {
        let json = std::fs::read_to_string(copy).map_err(|e| io_error(copy, e))?;
        // The copy is not where the song lives: its paths stand as they are.
        let (project, files) = self.read_project(&json, None)?;
        let project = self.with_known_instruments(project);
        self.replace_project(project);
        self.files = files;
        self.path = original.map(Path::to_path_buf);
        self.take_up_recovered_journal(copy, original);
        self.revision += 1;
        Ok(())
    }

    /// A snapshot that can be rendered without holding the session.
    pub fn render_job(&mut self) -> RenderJob {
        self.capture_device_state();
        RenderJob {
            project: self.project.clone(),
            factory: self.factory.clone(),
            audio: self.audio.clone(),
            apart: self.apart.clone(),
        }
    }

    /// Has the renders, measurements and probes of songs with plugins done
    /// in a process of their own from now on ([`crate::apart`]), or, with
    /// `None`, in this one.
    pub fn do_jobs_apart(&mut self, apart: Option<crate::apart::Apart>) {
        self.apart = apart;
    }

    /// Where jobs are done that are not done in this process, if anywhere.
    pub fn jobs_apart(&self) -> Option<&crate::apart::Apart> {
        self.apart.as_ref()
    }

    /// The session's regular chores; call it a few times a second. Frees
    /// memory the audio thread has released, gives clips that were playing
    /// a stand-in their warped audio once it has been prepared, writes
    /// what has been recorded so far into the project, and looks after the
    /// audio device: one that was lost is replaced by the next best, and
    /// one that came back is taken up again
    /// ([`Session::audio_revision`]).
    pub fn housekeeping(&mut self) {
        self.engine.collect_garbage();
        self.tend_audio();
        self.recover_lost_messages();
        self.tend_take();
        self.tend_journal();
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
        self.abandon_take();
        self.project = project;
        self.undo.clear();
        self.redo.clear();
        // Ids restart in every project; nothing running may be mistaken for
        // a device or a clip of the new one.
        self.bank.devices.clear();
        self.stand_ins = StandIns::default();
        self.compile_and_swap();
        // Track ids restart too: what was armed or selected was the old
        // project's.
        self.send_live_targets();
        // And another story begins.
        self.restart_journal();
        self.files = trill_core::FileTable::default();
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
            purpose: trill_core::device::Purpose::Playing,
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
            // A track that is gone is no longer armed or played.
            self.send_live_targets();
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

    /// A plugin keeps its own values, and the project stores only those
    /// that were set through the session. Before `setting` stores some
    /// for the first time, this notes what the plugin has them at now
    /// (`states`), which is what they return to when the setting is
    /// undone. The values the plugin was created with would not do: it
    /// may have been given a preset since, or had its knobs turned in its
    /// own editor.
    fn remember_plugin_values(&mut self, id: DeviceId, states: &[ParamState], setting: &[(String, f64)]) {
        let Ok(device) = self.project.device(id) else { return };
        let Some(meta) = self.bank.devices.get_mut(&id).filter(|meta| meta.controller.is_some()) else { return };
        for (info, state) in meta.params.iter_mut().zip(states) {
            if !device.params.contains_key(&info.id) && setting.iter().any(|(key, _)| *key == info.id) {
                info.default = state.value.clamp(info.min, info.max);
            }
        }
    }

    /// Before `leaving` goes onto the undo or redo stack, stores in it the
    /// state of the plugins that the project now current will not carry
    /// on with: those it no longer has, and those it has in another state
    /// (a preset was loaded, or is being taken back). Coming back to
    /// `leaving` then restores them as they were at this moment, with
    /// whatever was done in their editors.
    fn keep_leaving_state(&mut self, leaving: &mut Project) {
        let parting: Vec<DeviceId> = leaving
            .devices()
            .filter(|device| self.project.device(device.id).map_or(true, |now| now.state != device.state))
            .map(|device| device.id)
            .collect();
        for id in parting {
            let held = leaving.device(id).ok().and_then(|device| device.state.clone());
            let Some(captured) = self.news_of_state(id, held.as_ref()) else { continue };
            let Ok(device) = leaving.device_mut(id) else { continue };
            if device.state.as_ref() == Some(&captured) {
                continue;
            }
            let stale = device.state.replace(captured.clone());
            self.correct_history(id, &stale, &captured);
        }
    }

    /// What the running plugin behind a device says its state is.
    fn live_state(&self, id: DeviceId) -> Option<Vec<u8>> {
        let controller = self.bank.devices.get(&id)?.controller.as_ref()?;
        controller.lock().save_state()
    }

    /// [`Session::live_state`], for replacing `held`, the state a project
    /// holds for the device. `None` also when the plugin was created from
    /// `held` a moment ago: until it has had time to take that state on,
    /// what it was given is truer than what it says
    /// (`DeviceMeta::settling`).
    fn news_of_state(&self, id: DeviceId, held: Option<&Vec<u8>>) -> Option<Vec<u8>> {
        if self.bank.devices.get(&id)?.is_settling_into(held) {
            return None;
        }
        self.live_state(id)
    }

    /// Replaces a device's state in every history entry that holds
    /// `stale`, the copy the project had until now, by `captured`.
    ///
    /// What a plugin changed on its own (in its editor, say) is not part
    /// of the undo history. Without this, undoing an unrelated edit would
    /// bring back the old state and reset the plugin.
    fn correct_history(&mut self, id: DeviceId, stale: &Option<Vec<u8>>, captured: &[u8]) {
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if let Ok(device) = step.project.device_mut(id)
                && device.state == *stale
            {
                device.state = Some(captured.to_vec());
            }
        }
    }

    /// Copies each plugin's current state into the project model. This is
    /// bookkeeping, not an edit: it creates no undo step, and the history
    /// is corrected along with the present ([`Session::correct_history`]).
    fn capture_device_state(&mut self) {
        let ids: Vec<DeviceId> = self.project.devices().map(|d| d.id).collect();
        for id in ids {
            self.capture_state_of(id);
        }
    }

    /// [`Session::capture_device_state`] for one device.
    fn capture_state_of(&mut self, id: DeviceId) {
        let held = self.project.device(id).ok().and_then(|device| device.state.clone());
        let Some(captured) = self.news_of_state(id, held.as_ref()) else { return };
        let Ok(device) = self.project.device_mut(id) else { return };
        if device.state.as_ref() == Some(&captured) {
            return;
        }
        let stale = device.state.replace(captured.clone());
        if let Some(meta) = self.bank.devices.get_mut(&id) {
            meta.applied_state = Some(captured.clone());
        }
        self.correct_history(id, &stale, &captured);
        self.note_device(id, true);
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

/// The parameter values a preset stands for, if it is one of the sets of
/// values `descriptor` carries.
fn preset_values(descriptor: &DeviceDescriptor, preset: &PresetInfo) -> Option<Vec<(String, f64)>> {
    descriptor.presets.iter().find(|candidate| candidate.name == preset.id).map(|found| found.params.clone())
}

/// The state a preset of `descriptor` gives the device, if it is one of
/// the descriptor's own and has one.
fn preset_state(descriptor: &DeviceDescriptor, preset: &PresetInfo) -> Option<Vec<u8>> {
    descriptor.presets.iter().find(|candidate| candidate.name == preset.id).and_then(|found| found.state.clone())
}

fn io_error(path: &Path, error: std::io::Error) -> SessionError {
    SessionError::Io(format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_dsp::BuiltinFactory;

    fn session() -> Session {
        Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    /// How many steps can be undone, found by undoing them and putting
    /// them back.
    fn depth(session: &mut Session) -> usize {
        let mut depth = 0;
        while session.undo() {
            depth += 1;
        }
        for _ in 0..depth {
            session.redo();
        }
        depth
    }

    fn volume(session: &Session, track: TrackId) -> f32 {
        session.project().track(track).unwrap().volume_db
    }

    #[test]
    fn a_group_is_one_undo_step_whatever_happens_inside() {
        let mut session = session();
        let track = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
        let before = session.project().clone();

        let group = session.begin_group();
        // An edit of the assistant's.
        let bass = session.edit(|p| Ok(p.add_track("Bass"))).unwrap();
        // A fader dragged in the window meanwhile: a whole gesture, begun
        // and ended inside the group. Ending it must not end the group.
        session.begin_gesture();
        for step in 1..=5 {
            session.edit(|p| p.set_track_volume(track, -(step as f32))).unwrap();
        }
        session.end_gesture();
        // A tool that wraps its own edits in a gesture, as several do.
        session.begin_gesture();
        session.edit(|p| p.set_track_pan(bass, 0.25)).unwrap();
        session.edit(|p| p.set_track_volume(bass, -3.0)).unwrap();
        session.end_gesture();
        // A tool that fails halfway backs out with an undo of its own. It
        // must take back that one edit and nothing done before it.
        assert!(session.add_device(ChainRef::Track(bass), "builtin:polysynth", Some("no such preset"), None).is_err());
        let doomed = session.edit(|p| Ok(p.add_track("Doomed"))).unwrap();
        assert!(session.undo(), "the tool backs out");
        assert!(session.project().track(doomed).is_err());
        assert!(session.project().track(bass).is_ok(), "and what came before it stands");
        // And more edits after that.
        session.edit(|p| p.add_midi_clip(bass, "Line", 0.0, 4.0)).unwrap();
        let after = session.project().clone();
        assert!(depth(&mut session) > 3, "inside the group the steps are separate, as ever");

        let step = session.end_group(group).expect("the group changed the project");
        assert_eq!(session.project(), &after, "closing a group changes nothing in the project");
        assert_eq!(depth(&mut session), 2, "the track from before, and the group");
        assert_eq!(session.undo_step(), Some(step));

        assert!(session.undo());
        assert_eq!(session.project(), &before, "one undo takes back everything done in the group");
        assert_eq!(session.redo_step(), Some(step), "the step keeps its id on the redo side");
        assert!(session.redo());
        assert_eq!(session.project(), &after, "and one redo brings it all back");
        assert_eq!(session.undo_step(), Some(step));
    }

    #[test]
    fn a_group_that_changes_nothing_leaves_no_step() {
        let mut session = session();
        let track = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
        let before = session.project().clone();
        let step_before = session.undo_step();

        // Nothing at all happened.
        let group = session.begin_group();
        assert_eq!(session.end_group(group), None);
        assert_eq!((depth(&mut session), session.undo_step()), (1, step_before));

        // Edits that were refused, and a look around, change nothing.
        let group = session.begin_group();
        assert!(session.edit(|p| p.set_track_volume(TrackId(999), 0.0)).is_err());
        assert!(session.add_device(ChainRef::Track(track), "builtin:nope", None, None).is_err());
        assert_eq!(session.end_group(group), None);
        assert_eq!(depth(&mut session), 1);

        // An edit that was put right again by another leaves the project
        // as it was, and no step to undo.
        let group = session.begin_group();
        session.edit(|p| p.set_track_volume(track, -6.0)).unwrap();
        session.edit(|p| p.set_track_volume(track, 0.0)).unwrap();
        assert_eq!(session.end_group(group), None);
        assert_eq!(session.project(), &before);
        assert_eq!(depth(&mut session), 1);

        // So does an edit that was undone inside the group.
        let group = session.begin_group();
        session.edit(|p| p.set_track_volume(track, -6.0)).unwrap();
        assert!(session.undo());
        assert_eq!(session.end_group(group), None);
        assert_eq!(session.project(), &before);
        assert_eq!(session.undo_step(), step_before);
    }

    #[test]
    fn a_group_cut_short_holds_what_was_done_and_the_next_edit_is_its_own_step() {
        let mut session = session();
        let before = session.project().clone();

        // Work that was stopped halfway: the group is closed all the same.
        let group = session.begin_group();
        let track = session.edit(|p| Ok(p.add_track("Half"))).unwrap();
        session.edit(|p| p.set_track_volume(track, -9.0)).unwrap();
        let step = session.end_group(group).expect("what was done before the stop is a step");
        let halfway = session.project().clone();

        // Nothing is left open: what comes next is separate.
        session.edit(|p| p.set_track_pan(track, -0.5)).unwrap();
        assert_ne!(session.undo_step(), Some(step));
        assert!(session.undo());
        assert_eq!(session.project(), &halfway);
        assert_eq!(session.undo_step(), Some(step));
        assert!(session.undo());
        assert_eq!(session.project(), &before);
        assert!(!session.can_undo());
    }

    #[test]
    fn a_gesture_under_way_is_cut_where_a_group_opens_and_closes() {
        let mut session = session();
        let track = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();

        // The user is dragging a fader when the assistant's turn begins...
        session.begin_gesture();
        session.edit(|p| p.set_track_volume(track, -1.0)).unwrap();
        session.edit(|p| p.set_track_volume(track, -2.0)).unwrap();
        let group = session.begin_group();
        // ...keeps dragging while the assistant works...
        session.edit(|p| p.set_track_volume(track, -3.0)).unwrap();
        let bass = session.edit(|p| Ok(p.add_track("Bass"))).unwrap();
        session.edit(|p| p.set_track_volume(track, -4.0)).unwrap();
        let step = session.end_group(group).expect("the group changed the project");
        // ...and is still dragging after it has finished.
        session.edit(|p| p.set_track_volume(track, -5.0)).unwrap();
        session.edit(|p| p.set_track_volume(track, -6.0)).unwrap();
        session.end_gesture();

        // The drag after the group, the group, the drag before it, the track.
        assert_eq!(depth(&mut session), 4);
        assert!(session.undo());
        assert_eq!(volume(&session, track), -4.0);
        assert_eq!(session.undo_step(), Some(step));
        assert!(session.undo());
        assert_eq!(volume(&session, track), -2.0, "the group's undo takes back what was done while it was open");
        assert!(session.project().track(bass).is_err());
        assert!(session.undo());
        assert_eq!(volume(&session, track), 0.0);
    }

    #[test]
    fn older_steps_undone_inside_a_group() {
        let mut session = session();
        let track = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
        session.edit(|p| p.set_track_volume(track, -3.0)).unwrap();
        let mine = session.undo_step();

        // The user undoes and redoes a step of their own while a group is
        // open: nothing of the group's, so no step for it.
        let group = session.begin_group();
        assert!(session.undo());
        assert!(session.redo());
        assert_eq!(session.end_group(group), None);
        assert_eq!((depth(&mut session), session.undo_step()), (2, mine));

        // The user undoes their step, and then the assistant edits from
        // there: the group's undo returns to where that edit started.
        let group = session.begin_group();
        assert!(session.undo());
        assert_eq!(volume(&session, track), 0.0);
        session.edit(|p| p.set_track_pan(track, 0.5)).unwrap();
        session.edit(|p| p.set_track_volume(track, -12.0)).unwrap();
        let step = session.end_group(group).expect("the group changed the project");
        assert_eq!(depth(&mut session), 2, "the track and the group");
        assert!(session.undo());
        assert_eq!(session.redo_step(), Some(step));
        let project = session.project();
        assert_eq!((project.track(track).unwrap().volume_db, project.track(track).unwrap().pan), (0.0, 0.0));
    }

    #[test]
    fn a_long_group_is_still_one_step_past_the_history_limit() {
        let mut session = session();
        let track = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
        let before = session.project().clone();

        let group = session.begin_group();
        for step in 1..=(UNDO_LIMIT + 40) {
            session.edit(|p| p.set_track_volume(track, -(step as f32) / 10.0)).unwrap();
        }
        assert!(session.end_group(group).is_some());
        assert!(session.undo());
        assert_eq!(session.project(), &before, "the group's beginning was kept while older stops were forgotten");

        // Without a group the oldest steps go, as they always have.
        assert!(session.redo());
        for step in 1..=(UNDO_LIMIT + 40) {
            session.edit(|p| p.set_track_pan(track, (step % 2) as f32 - 0.5)).unwrap();
        }
        assert_eq!(depth(&mut session), UNDO_LIMIT);
    }

    #[test]
    fn groups_opened_inside_one_another() {
        let mut session = session();
        let before = session.project().clone();

        let outer = session.begin_group();
        let track = session.edit(|p| Ok(p.add_track("Outer"))).unwrap();
        let inner = session.begin_group();
        session.edit(|p| p.set_track_volume(track, -6.0)).unwrap();
        session.edit(|p| p.set_track_pan(track, 0.5)).unwrap();
        let inner_step = session.end_group(inner).expect("the inner group changed the project");
        assert_eq!(depth(&mut session), 2, "the inner group is one step of the outer one");
        assert_eq!(session.undo_step(), Some(inner_step));
        session.edit(|p| p.add_midi_clip(track, "Clip", 0.0, 4.0)).unwrap();
        assert!(session.end_group(outer).is_some());
        assert_eq!(depth(&mut session), 1);
        assert!(session.undo());
        assert_eq!(session.project(), &before);

        // Closed in the other order, the one closed last has nothing left
        // of its own: the first took it all.
        assert!(session.redo());
        let outer = session.begin_group();
        session.edit(|p| p.set_track_volume(track, -1.0)).unwrap();
        let inner = session.begin_group();
        session.edit(|p| p.set_track_volume(track, -2.0)).unwrap();
        assert!(session.end_group(outer).is_some());
        assert_eq!(session.end_group(inner), None);
        assert_eq!(depth(&mut session), 2);
    }

    #[test]
    fn another_project_opened_inside_a_group() {
        let mut session = session();
        session.edit(|p| Ok(p.add_track("Old"))).unwrap();

        let group = session.begin_group();
        session.edit(|p| Ok(p.add_track("Older still"))).unwrap();
        // The history does not carry over to another project, and neither
        // does what the group had gathered so far.
        session.new_project();
        let blank = session.project().clone();
        let track = session.edit(|p| Ok(p.add_track("New"))).unwrap();
        session.edit(|p| p.set_track_volume(track, -6.0)).unwrap();
        assert!(session.end_group(group).is_some());
        assert_eq!(depth(&mut session), 1);
        assert!(session.undo());
        assert_eq!(session.project(), &blank);
    }
}
