//! What the views read: a [`Snapshot`] of the project, taken only when the
//! session's revision changes, and the [`Live`] values that move on their
//! own (transport, meters, CPU load).
//!
//! The session is shared with other front ends (an MCP server editing from
//! another thread), so views never look at it directly. They paint from the
//! snapshot, which costs one project clone per edit and lets the session
//! lock be released before any painting starts.

use super::instrument_panel::Shown;
use super::timeline;
use super::units::{METER_FLOOR_DB, gain_to_db};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use trill_core::device::{ControllerSupport, DeviceDescriptor, DeviceFactory};
use trill_core::presets::PresetInfo;
use trill_core::project::AutomationLane;
use trill_core::{AutomationPoint, AutomationTarget, Beats, Clip, ClipId, DeviceId, Note, Project, Track, TrackId};
use trill_engine::{Meter, ParamState, Problem, RecordStatus, SampleFile, Session, SilentInstrument};

/// What a MIDI clip plays, as the arrangement draws it.
pub struct ClipNotes {
    /// The notes at their places on the timeline: offset, loop and trim
    /// resolved by [`Clip::timeline_notes`].
    pub notes: Vec<Note>,
    /// Lowest and highest pitch of everything in the clip, heard or not, so
    /// the picture does not rescale while an edge is being trimmed.
    pub range: Option<(u8, u8)>,
}

/// The project and everything derived from it, as of one revision.
pub struct Snapshot {
    pub revision: u64,
    pub project: Project,
    pub can_undo: bool,
    pub can_redo: bool,
    pub modified: bool,
    pub path: Option<PathBuf>,
    /// Compile problems (plugins that failed to load, missing audio files).
    pub problems: Vec<String>,
    /// Why each audio clip that is not playing as it should is not: its
    /// file is missing or unreadable, or could not be stretched.
    pub clip_problems: HashMap<ClipId, String>,
    /// The files of the sample-playing devices `params` covers, with what
    /// is wrong with each.
    pub samples: HashMap<DeviceId, Vec<SampleFile>>,
    /// What the Sampled Instruments `params` covers say they have loaded,
    /// as their panels draw it; one with nothing loaded is in it too.
    pub instruments: HashMap<DeviceId, Shown>,
    /// The tracks whose devices `params` covers, besides the master's, in
    /// ascending order.
    pub params_tracks: Vec<TrackId>,
    /// Parameters of the devices the Devices tab and the automation lanes
    /// show, or why a device has none (it failed to load).
    pub params: HashMap<DeviceId, Result<Vec<ParamState>, String>>,
    /// The devices that bring a window of their own (plugins do, built-in
    /// devices do not), in ascending order.
    pub editors: Vec<DeviceId>,
    /// The devices that can listen to another track in place of their own
    /// input (a compressor, a plugin with an input for it), in ascending
    /// order.
    pub listeners: Vec<DeviceId>,
    /// The presets that were saved for the device types `params` covers,
    /// by device URI; a type without any is not in it.
    pub saved_presets: HashMap<String, Vec<PresetInfo>>,
    /// The saved presets that are sounds, of every device type: each with
    /// what it brings behind its first device.
    pub saved_sounds: Vec<trill_engine::SavedSound>,
    /// Which MIDI controllers the instrument of each track does something
    /// with, where that is known.
    controller_support: HashMap<TrackId, ControllerSupport>,
    /// The tracks whose notes are drums.
    drum_tracks: HashSet<TrackId>,
    /// The tracks that play through a drum instrument, whatever they say
    /// of their notes themselves.
    drum_instruments: HashSet<TrackId>,
    /// What each MIDI clip plays, worked out the first time it is drawn and
    /// kept until the next revision.
    clip_notes: RefCell<HashMap<ClipId, Rc<ClipNotes>>>,
    /// Meter handles, in track order. Reading a meter needs no lock.
    track_meters: Vec<(TrackId, Arc<Meter>)>,
    master_meter: Arc<Meter>,
}

impl Snapshot {
    /// Which MIDI controllers the instrument of `track` does something
    /// with: [`ControllerSupport::Unknown`] where that cannot be said.
    pub fn controller_support(&self, track: TrackId) -> ControllerSupport {
        self.controller_support.get(&track).cloned().unwrap_or_default()
    }

    /// Whether the notes of `track` are drums: the track says so, or says
    /// nothing and plays through a drum instrument.
    pub fn plays_drums(&self, track: TrackId) -> bool {
        self.drum_tracks.contains(&track)
    }

    /// Whether `track` plays through a drum kit or a plugin that calls
    /// itself a drum instrument, whatever the track says of its notes.
    pub fn has_drum_instrument(&self, track: TrackId) -> bool {
        self.drum_instruments.contains(&track)
    }

    /// Copies what the views need out of the session. `devices_for` are the
    /// tracks whose device parameters to include (see [`Self::covers`]).
    pub fn capture(session: &Session, devices_for: &[TrackId]) -> Self {
        let project = session.project().clone();
        let mut shown: Vec<&Track> = devices_for.iter().filter_map(|id| project.track(*id).ok()).collect();
        shown.sort_by_key(|track| track.id.0);
        shown.dedup_by_key(|track| track.id);
        let params = shown
            .iter()
            .flat_map(|track| track.devices.iter())
            .chain(project.master.devices.iter())
            .map(|device| (device.id, session.device_params(device.id).map_err(|error| error.to_string())))
            .collect();
        let clip_problems = session
            .problems()
            .iter()
            .filter_map(|problem| match problem {
                Problem::AudioFile { clip, reason, .. } => Some((*clip, reason.clone())),
                _ => None,
            })
            .collect();
        let samples = shown
            .iter()
            .flat_map(|track| track.devices.iter())
            .chain(project.master.devices.iter())
            .filter(|device| crate::ui::sample_panels::plays_samples(&device.uri))
            .filter_map(|device| Some((device.id, session.sample_files(device.id).ok()?)))
            .collect();
        let instruments = shown
            .iter()
            .flat_map(|track| track.devices.iter())
            .filter(|device| device.uri == crate::ui::instrument_panel::SAMPLED)
            .map(|device| {
                let problems = session.problems().iter().filter_map(|problem| match problem {
                    Problem::Device { device: id, reason } | Problem::Sample { device: id, reason, .. }
                        if *id == device.id =>
                    {
                        Some(reason.clone())
                    }
                    _ => None,
                });
                (device.id, Shown::of(session.sampled(device.id), problems.collect()))
            })
            .collect();
        let track_meters = project
            .tracks
            .iter()
            .filter_map(|track| session.track_meter(track.id).map(|meter| (track.id, meter)))
            .collect();
        // Asked of every device, shown or not: the mixer lists them all.
        // For a built-in device it is a failed lookup and no more.
        let chains = project.tracks.iter().map(|track| &track.devices).chain([&project.master.devices]);
        let devices: Vec<DeviceId> = chains.flatten().map(|device| device.id).collect();
        let mut editors: Vec<DeviceId> = devices.iter().copied().filter(|id| session.device_has_editor(*id)).collect();
        editors.sort_unstable();
        let mut listeners: Vec<DeviceId> = devices.iter().copied().filter(|id| session.device_takes_key(*id)).collect();
        listeners.sort_unstable();
        let controller_support =
            project.tracks.iter().map(|track| (track.id, session.controller_support(track.id))).collect();
        let ids = || project.tracks.iter().map(|track| track.id);
        let drum_tracks = ids().filter(|id| session.plays_drums(*id)).collect();
        let drum_instruments = ids().filter(|id| session.instrument_plays_drums(*id)).collect();
        let mut saved_presets: HashMap<String, Vec<PresetInfo>> = HashMap::new();
        for device in shown.iter().flat_map(|track| track.devices.iter()).chain(project.master.devices.iter()) {
            if !saved_presets.contains_key(&device.uri) {
                let saved = session.saved_presets().list(&device.uri);
                if !saved.is_empty() {
                    saved_presets.insert(device.uri.clone(), saved);
                }
            }
        }
        Self {
            controller_support,
            drum_tracks,
            drum_instruments,
            saved_presets,
            saved_sounds: session.saved_presets().sounds(),
            revision: session.revision(),
            can_undo: session.can_undo(),
            can_redo: session.can_redo(),
            modified: session.is_modified(),
            path: session.path().map(PathBuf::from),
            problems: session.problems().iter().map(ToString::to_string).collect(),
            clip_problems,
            samples,
            instruments,
            params_tracks: shown.iter().map(|track| track.id).collect(),
            params,
            editors,
            listeners,
            clip_notes: RefCell::default(),
            track_meters,
            master_meter: session.master_meter(),
            project,
        }
    }

    /// A snapshot of a song as it was, for playing a session again: the
    /// song and what can be said of it without a session behind it.
    /// Built-in devices have their parameters, with the values the song
    /// holds; nothing is said of meters, editors, files or problems, which
    /// belong to a song that is running. `revision` tells one such
    /// snapshot from the next.
    pub fn of_song(
        project: Project,
        revision: u64,
        descriptors: &[DeviceDescriptor],
        factory: &dyn DeviceFactory,
    ) -> Self {
        let chains = || project.tracks.iter().map(|track| &track.devices).chain([&project.master.devices]);
        let params = chains()
            .flatten()
            .map(|device| {
                let known = descriptors.iter().find(|descriptor| descriptor.uri == device.uri);
                let states = known.map(|descriptor| {
                    let states = descriptor.params.iter().map(|info| ParamState {
                        value: device.params.get(&info.id).copied().unwrap_or(info.default),
                        info: info.clone(),
                    });
                    states.collect()
                });
                (device.id, Ok(states.unwrap_or_default()))
            })
            .collect();
        let plays_drums = |track: &&Track| track.devices.iter().any(|device| factory.plays_drums(&device.uri));
        let drum_instruments: HashSet<TrackId> =
            project.tracks.iter().filter(plays_drums).map(|track| track.id).collect();
        let drum_tracks = project
            .tracks
            .iter()
            .filter(|track| match track.profile.role {
                Some(role) => role == trill_core::Role::Drums,
                None => drum_instruments.contains(&track.id),
            })
            .map(|track| track.id)
            .collect();
        let mut params_tracks: Vec<TrackId> = project.tracks.iter().map(|track| track.id).collect();
        params_tracks.sort_by_key(|track| track.0);
        Self {
            controller_support: HashMap::new(),
            drum_tracks,
            drum_instruments,
            saved_presets: HashMap::new(),
            saved_sounds: Vec::new(),
            revision,
            can_undo: false,
            can_redo: false,
            modified: false,
            path: None,
            problems: Vec::new(),
            clip_problems: HashMap::new(),
            samples: HashMap::new(),
            instruments: HashMap::new(),
            params_tracks,
            params,
            editors: Vec::new(),
            listeners: Vec::new(),
            clip_notes: RefCell::default(),
            track_meters: Vec::new(),
            master_meter: Arc::default(),
            project,
        }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.project.tracks.iter().find(|track| track.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.project.tracks.iter().position(|track| track.id == id)
    }

    /// A clip and the track that holds it.
    pub fn clip(&self, id: ClipId) -> Option<(&Track, &Clip)> {
        self.project
            .tracks
            .iter()
            .find_map(|track| track.clips.iter().find(|clip| clip.id == id).map(|clip| (track, clip)))
    }

    /// True if `params` covers exactly the devices of `wanted` (ascending,
    /// without repeats) that still exist. When it does not, the snapshot is
    /// taken again.
    pub fn covers(&self, wanted: &[TrackId]) -> bool {
        let existing = wanted.iter().filter(|id| self.track(**id).is_some());
        existing.eq(self.params_tracks.iter())
    }

    /// What `clip` plays, for drawing it. Computed once per clip and
    /// revision: a song has thousands of notes and is painted every frame.
    pub fn clip_notes(&self, clip: &Clip) -> Rc<ClipNotes> {
        let mut cache = self.clip_notes.borrow_mut();
        let entry = cache.entry(clip.id).or_insert_with(|| {
            Rc::new(ClipNotes { notes: clip.timeline_notes(), range: timeline::pitch_range(clip.notes()) })
        });
        entry.clone()
    }

    /// The lane that automates `target`, if there is one: its envelope, and
    /// whether it is switched on.
    pub fn lane(&self, target: &AutomationTarget) -> Option<&AutomationLane> {
        self.project.automation.iter().find(|lane| lane.target == *target)
    }

    /// The envelope that automates `target`, if there is one, switched on
    /// or not.
    pub fn envelope(&self, target: &AutomationTarget) -> Option<&[AutomationPoint]> {
        self.lane(target).map(|lane| lane.points.as_slice())
    }

    /// What `target`'s envelope makes of it at `beat`: what is heard.
    /// `None` if it has no envelope, or one that is switched off, in which
    /// case the target stays where it is set by hand.
    pub fn automated(&self, target: &AutomationTarget, beat: Beats) -> Option<f64> {
        let lane = self.lane(target).filter(|lane| lane.enabled)?;
        Some(lane.value_at(beat))
    }

    /// Whether `device` brings a window of its own to open.
    pub fn has_editor(&self, device: DeviceId) -> bool {
        self.editors.binary_search(&device).is_ok()
    }

    /// Whether `device` can be given another track to listen to.
    pub fn can_listen(&self, device: DeviceId) -> bool {
        self.listeners.binary_search(&device).is_ok()
    }

    /// The description of a device's parameter, when the snapshot covers
    /// the device.
    pub fn param(&self, device: DeviceId, param: &str) -> Option<&ParamState> {
        self.params.get(&device)?.as_ref().ok()?.iter().find(|state| state.info.id == param)
    }
}

/// Level falls this fast once the signal drops, in dB per second.
const METER_FALL_DB_PER_SECOND: f32 = 40.0;
/// How long the peak marker stays put before it starts to fall.
const PEAK_HOLD_SECONDS: f32 = 1.2;
const PEAK_FALL_DB_PER_SECOND: f32 = 24.0;

/// A stereo peak meter with display ballistics: instant attack, steady fall
/// and a peak marker that lingers. Levels are in dBFS.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MeterState {
    pub left_db: f32,
    pub right_db: f32,
    /// Highest recent level of either channel.
    pub peak_db: f32,
    peak_age: f32,
    /// Latched when the signal exceeded full scale; cleared by the user.
    pub clipped: bool,
}

impl Default for MeterState {
    fn default() -> Self {
        Self {
            left_db: f32::NEG_INFINITY,
            right_db: f32::NEG_INFINITY,
            peak_db: f32::NEG_INFINITY,
            peak_age: 0.0,
            clipped: false,
        }
    }
}

impl MeterState {
    /// Advances the meter by `dt` seconds, given the linear peak of each
    /// channel over that time.
    pub fn feed(&mut self, left: f32, right: f32, dt: f32) {
        let fall = METER_FALL_DB_PER_SECOND * dt;
        self.left_db = gain_to_db(left).max(self.left_db - fall);
        self.right_db = gain_to_db(right).max(self.right_db - fall);

        let loudest = gain_to_db(left.max(right));
        if loudest >= self.peak_db {
            self.peak_db = loudest;
            self.peak_age = 0.0;
        } else {
            self.peak_age += dt;
            if self.peak_age > PEAK_HOLD_SECONDS {
                self.peak_db -= PEAK_FALL_DB_PER_SECOND * dt;
            }
        }
        if left.max(right) > 1.0 {
            self.clipped = true;
        }
    }

    /// False once everything has fallen off the bottom of the scale, at
    /// which point the meter no longer needs repainting.
    pub fn is_moving(&self) -> bool {
        self.left_db.max(self.right_db).max(self.peak_db) > METER_FLOOR_DB
    }
}

/// Values that change without the project changing.
#[derive(Default)]
pub struct Live {
    pub playing: bool,
    pub metronome: bool,
    /// Song position in beats.
    pub position: f64,
    /// Smoothed fraction of the audio thread's time budget in use.
    pub cpu_load: f32,
    /// What the audio threads made of the song over the last ten seconds:
    /// how they kept up, what each track cost, and whether the song is
    /// more than the machine plays in time.
    pub keeping: trill_engine::KeepingUp,
    /// Audio callbacks that panicked and were replaced by silence.
    pub audio_faults: u32,
    /// How many warped clips play a stand-in while their stretched audio
    /// is being made.
    pub preparing: usize,
    /// The devices whose own windows are open right now. Asked of the
    /// session every time: the user closes them without telling anyone.
    pub open_editors: Vec<DeviceId>,
    /// The armed tracks, in the order they were armed: where keyboards
    /// play and what a take records. The session's, not the project's.
    pub armed: Vec<TrackId>,
    /// The take being counted in or recorded, while one is.
    pub take: Option<RecordStatus>,
    /// The tracks whose instrument has been given notes and has put out
    /// nothing in answer, with which instrument that is.
    pub silent: Vec<(TrackId, SilentInstrument)>,
    /// The plugins that seem to be loading what they play: made or given
    /// another sound lately, with the program still reading.
    pub loading: Vec<DeviceId>,
    /// How far the song that was opened is with its devices, while any of
    /// them is still being made, and the line that says so: see
    /// [`super::opening`].
    pub arriving: Option<trill_engine::Arriving>,
    pub arriving_words: String,
    /// How far each Sampled Instrument on show is with reading its
    /// recordings from the disk; a device that has nothing to read, or
    /// that was busy when asked, is not in it.
    pub reading: HashMap<DeviceId, trill_core::Reading>,
    /// Bars of count-in before a take that starts from a stopped song.
    pub count_in: u32,
    pub master: MeterState,
    tracks: HashMap<TrackId, MeterState>,
}

impl Live {
    /// Whether the editor of `device` is open.
    pub fn editor_open(&self, device: DeviceId) -> bool {
        self.open_editors.contains(&device)
    }

    /// Reads which of the snapshot's editors are open. A handful of
    /// lookups, and nothing at all for a project without plugins.
    pub fn update_editors(&mut self, snapshot: &Snapshot, session: &Session) {
        self.open_editors.clear();
        self.open_editors.extend(snapshot.editors.iter().copied().filter(|id| session.is_device_editor_open(*id)));
    }

    /// The instrument of `track`, if it is being given notes and stays
    /// silent.
    pub fn silent_instrument(&self, track: TrackId) -> Option<&SilentInstrument> {
        self.silent.iter().find(|(id, _)| *id == track).map(|(_, silent)| silent)
    }

    /// Reads which instruments are given notes and stay silent: a number
    /// per track, and no more where all is well.
    pub fn update_silent(&mut self, snapshot: &Snapshot, session: &Session) {
        self.silent.clear();
        let tracks = snapshot.project.tracks.iter();
        self.silent.extend(tracks.filter_map(|track| Some((track.id, session.silent_instrument(track.id)?))));
    }

    /// Reads how far the snapshot's Sampled Instruments are with their
    /// recordings: a number or two per device, and no waiting. With
    /// `shown` false nothing is said of any.
    pub fn update_reading(&mut self, snapshot: &Snapshot, session: &Session, shown: bool) {
        self.reading.clear();
        if shown {
            let devices = snapshot.instruments.keys();
            self.reading.extend(devices.filter_map(|device| Some((*device, session.reading(*device)?))));
        }
    }

    /// Reads what is armed and whether a take is running.
    pub fn update_recording(&mut self, session: &Session) {
        if self.armed != session.armed() {
            self.armed = session.armed().to_vec();
        }
        self.take = session.is_recording().then(|| session.record_status());
        self.count_in = session.count_in();
    }

    pub fn track_meter(&self, id: TrackId) -> MeterState {
        self.tracks.get(&id).copied().unwrap_or_default()
    }

    pub fn set_cpu_load(&mut self, load: f32) {
        // The raw value jumps around from callback to callback.
        self.cpu_load += (load - self.cpu_load) * 0.15;
    }

    /// Pulls the peaks since the last call out of the engine's meters.
    pub fn update_meters(&mut self, snapshot: &Snapshot, dt: f32) {
        let (left, right) = snapshot.master_meter.take();
        self.master.feed(left, right, dt);

        self.tracks.retain(|id, _| snapshot.track_meters.iter().any(|(track, _)| track == id));
        for (id, meter) in &snapshot.track_meters {
            let (left, right) = meter.take();
            self.tracks.entry(*id).or_default().feed(left, right, dt);
        }
    }

    /// True while any meter still shows something.
    pub fn meters_moving(&self) -> bool {
        self.master.is_moving() || self.tracks.values().any(MeterState::is_moving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::device::CompositeFactory;
    use trill_core::{ChainRef, Note};
    use trill_dsp::BuiltinFactory;
    use trill_engine::AudioBackend;

    fn session() -> Session {
        let factory = Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory)]));
        Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    #[test]
    fn meter_attacks_instantly_and_falls_steadily() {
        let mut meter = MeterState::default();
        assert!(!meter.is_moving());
        meter.feed(0.5, 0.25, 0.016);
        assert!((meter.left_db + 6.02).abs() < 0.01);
        assert!((meter.right_db + 12.04).abs() < 0.01);
        assert!(meter.is_moving());

        meter.feed(0.0, 0.0, 0.1);
        assert!((meter.left_db + 6.02 + 4.0).abs() < 0.01, "falls 40 dB/s: {}", meter.left_db);
        assert!((meter.peak_db + 6.02).abs() < 0.01, "the peak marker holds");

        // A louder block takes over immediately.
        meter.feed(1.0, 0.0, 0.016);
        assert_eq!(meter.left_db, 0.0);
        assert!(!meter.clipped);

        for _ in 0..400 {
            meter.feed(0.0, 0.0, 0.016);
        }
        assert!(!meter.is_moving(), "silence eventually empties the meter");
    }

    #[test]
    fn peak_marker_falls_after_its_hold_time() {
        let mut meter = MeterState::default();
        meter.feed(1.0, 1.0, 0.01);
        meter.feed(0.0, 0.0, 1.0);
        assert_eq!(meter.peak_db, 0.0, "still holding");
        meter.feed(0.0, 0.0, 0.5);
        assert!(meter.peak_db < -10.0, "falling after the hold: {}", meter.peak_db);
    }

    #[test]
    fn overs_latch_the_clip_flag() {
        let mut meter = MeterState::default();
        meter.feed(1.2, 0.1, 0.01);
        meter.feed(0.0, 0.0, 5.0);
        assert!(meter.clipped);
    }

    #[test]
    fn capture_reflects_the_session() {
        let mut session = session();
        let before = Snapshot::capture(&session, &[]);
        assert!(before.project.tracks.is_empty());
        assert!(!before.can_undo && !before.modified);
        // A blank project has a limiter on the master, and its parameters
        // are always included.
        assert_eq!(before.params.len(), 1);

        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        let clip = session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "Riff", 4.0, 8.0)?;
                project.clip_mut(clip)?.add_notes(&[Note { pitch: 60, start: 0.0, length: 1.0, velocity: 90 }])?;
                Ok(clip)
            })
            .unwrap();

        let after = Snapshot::capture(&session, &[track]);
        assert!(after.revision > before.revision);
        assert!(after.can_undo && after.modified);
        assert_eq!(after.track_index(track), Some(0));
        assert_eq!(after.track(track).unwrap().name, "Lead");
        let (owner, found) = after.clip(clip).unwrap();
        assert_eq!((owner.id, found.name.as_str(), found.notes().len()), (track, "Riff", 1));
        assert_eq!(after.params_tracks, [track]);
        assert!(after.covers(&[track]) && !after.covers(&[]));
        let synth_params = after.params[&synth].as_ref().unwrap();
        assert!(synth_params.iter().any(|param| param.info.id == "cutoff"));

        // Without a track selected only the master's devices are covered.
        let master_only = Snapshot::capture(&session, &[]);
        assert!(!master_only.params.contains_key(&synth));
        assert!(master_only.param(synth, "cutoff").is_none());
        assert_eq!(after.param(synth, "cutoff").unwrap().info.id, "cutoff");
        // A track that no longer exists is not an error, and asking for it
        // again does not make the snapshot look out of date.
        let stale = Snapshot::capture(&session, &[TrackId(9999), track, track]);
        assert_eq!(stale.params_tracks, [track]);
        assert!(stale.covers(&[track, TrackId(9999)]));

        // What a clip plays is worked out once and shared.
        let (_, clip_ref) = after.clip(clip).unwrap();
        let notes = after.clip_notes(clip_ref);
        assert_eq!((notes.notes.len(), notes.notes[0].start, notes.range), (1, 4.0, Some((60, 60))));
        assert!(Rc::ptr_eq(&notes, &after.clip_notes(clip_ref)));
    }

    #[test]
    fn a_song_as_it_was_is_shown_without_a_session_behind_it() {
        let mut project = Project::new("Then");
        let lead = project.add_track("Lead");
        let drums = project.add_track("Drums");
        let hand = project.add_track("Hand drums");
        let synth = project.add_device(ChainRef::Track(lead), "builtin:polysynth", "Poly Synth", None).unwrap();
        project.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0);
        project.add_device(ChainRef::Track(drums), "builtin:drumkit", "Drum Kit", None).unwrap();
        let unknown = project.add_device(ChainRef::Master, "clap:not-installed", "Somebody's Plugin", None).unwrap();
        let profile = trill_core::InstrumentProfile { role: Some(trill_core::Role::Drums), ..Default::default() };
        project.set_track_profile(hand, profile).unwrap();
        let clip = project.add_midi_clip(lead, "Riff", 4.0, 8.0).unwrap();

        let descriptors = BuiltinFactory.descriptors();
        let snapshot = Snapshot::of_song(project, 7, &descriptors, &BuiltinFactory);
        assert_eq!((snapshot.revision, snapshot.can_undo, snapshot.modified), (7, false, false));
        // A built-in device has its parameters, at the values the song has.
        let cutoff = snapshot.param(synth, "cutoff").expect("the synth's parameters are known");
        assert_eq!(cutoff.value, 900.0);
        let resonance = snapshot.param(synth, "resonance").unwrap();
        assert_eq!(resonance.value, resonance.info.default);
        // A plugin's are its own to say, and it is not there to ask.
        assert_eq!(snapshot.params[&unknown].as_ref().map(Vec::len), Ok(0));
        // Drums are drums by their instrument, or because the track says so.
        assert!(snapshot.plays_drums(drums) && snapshot.has_drum_instrument(drums));
        assert!(snapshot.plays_drums(hand) && !snapshot.has_drum_instrument(hand));
        assert!(!snapshot.plays_drums(lead));
        assert!(snapshot.covers(&[lead, drums, hand]));
        assert_eq!(snapshot.clip(clip).unwrap().1.name, "Riff");
        assert!(snapshot.problems.is_empty() && snapshot.editors.is_empty());
    }

    #[test]
    fn live_tracks_meters_for_current_tracks_only() {
        let mut session = session();
        let track = session.edit(|project| Ok(project.add_track("A"))).unwrap();
        let mut live = Live::default();
        live.update_meters(&Snapshot::capture(&session, &[]), 0.016);
        assert_eq!(live.track_meter(track), MeterState::default());
        assert!(!live.meters_moving());

        session.edit(|project| project.remove_track(track).map(|_| ())).unwrap();
        live.update_meters(&Snapshot::capture(&session, &[]), 0.016);
        assert!(live.tracks.is_empty());
    }

    #[test]
    fn envelopes_are_looked_up_by_what_they_automate() {
        let mut session = session();
        let track = session.edit(|project| Ok(project.add_track("A"))).unwrap();
        let volume = AutomationTarget::TrackVolume { track };
        session
            .edit(|project| {
                let points = vec![AutomationPoint::new(0.0, -48.0), AutomationPoint::new(4.0, 0.0)];
                project.set_automation(volume.clone(), points)
            })
            .unwrap();
        let snapshot = Snapshot::capture(&session, &[]);
        assert_eq!(snapshot.envelope(&volume).unwrap().len(), 2);
        // What is heard half way: half way along the fader between the two,
        // which is what the engine plays, not the middle of the decibels.
        let half_way = snapshot.automated(&volume, 2.0).unwrap();
        assert!((half_way + 14.925).abs() < 0.01, "{half_way}");
        assert_eq!(snapshot.automated(&AutomationTarget::TrackPan { track }, 2.0), None);

        // Switched off, the envelope is still there and says nothing.
        session.edit(|project| project.set_automation_enabled(&volume, false)).unwrap();
        let snapshot = Snapshot::capture(&session, &[]);
        assert_eq!(snapshot.envelope(&volume).unwrap().len(), 2);
        assert!(snapshot.lane(&volume).is_some_and(|lane| !lane.enabled));
        assert_eq!(snapshot.automated(&volume, 2.0), None);
    }
}
