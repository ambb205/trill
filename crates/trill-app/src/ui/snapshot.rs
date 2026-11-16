//! What the views read: a [`Snapshot`] of the project, taken only when the
//! session's revision changes, and the [`Live`] values that move on their
//! own (transport, meters, CPU load).
//!
//! The session is shared with other front ends (an MCP server editing from
//! another thread), so views never look at it directly. They paint from the
//! snapshot, which costs one project clone per edit and lets the session
//! lock be released before any painting starts.

use super::timeline;
use super::units::{METER_FLOOR_DB, gain_to_db};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use trill_core::project::AutomationLane;
use trill_core::{AutomationPoint, AutomationTarget, Beats, Clip, ClipId, DeviceId, Note, Project, Track, TrackId};
use trill_engine::{Meter, ParamState, Problem, SampleFile, Session};

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
    /// The tracks whose devices `params` covers, besides the master's, in
    /// ascending order.
    pub params_tracks: Vec<TrackId>,
    /// Parameters of the devices the Devices tab and the automation lanes
    /// show, or why a device has none (it failed to load).
    pub params: HashMap<DeviceId, Result<Vec<ParamState>, String>>,
    /// What each MIDI clip plays, worked out the first time it is drawn and
    /// kept until the next revision.
    clip_notes: RefCell<HashMap<ClipId, Rc<ClipNotes>>>,
    /// Meter handles, in track order. Reading a meter needs no lock.
    track_meters: Vec<(TrackId, Arc<Meter>)>,
    master_meter: Arc<Meter>,
}

impl Snapshot {
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
        let track_meters = project
            .tracks
            .iter()
            .filter_map(|track| session.track_meter(track.id).map(|meter| (track.id, meter)))
            .collect();
        Self {
            revision: session.revision(),
            can_undo: session.can_undo(),
            can_redo: session.can_redo(),
            modified: session.is_modified(),
            path: session.path().map(PathBuf::from),
            problems: session.problems().iter().map(ToString::to_string).collect(),
            clip_problems,
            samples,
            params_tracks: shown.iter().map(|track| track.id).collect(),
            params,
            clip_notes: RefCell::default(),
            track_meters,
            master_meter: session.master_meter(),
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
        Some(trill_core::project::automation_value(&lane.points, beat))
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
    /// Audio callbacks that panicked and were replaced by silence.
    pub audio_faults: u32,
    /// How many warped clips play a stand-in while their stretched audio
    /// is being made.
    pub preparing: usize,
    pub master: MeterState,
    tracks: HashMap<TrackId, MeterState>,
}

impl Live {
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
                let points = vec![AutomationPoint::new(0.0, -12.0), AutomationPoint::new(4.0, 0.0)];
                project.set_automation(volume.clone(), points)
            })
            .unwrap();
        let snapshot = Snapshot::capture(&session, &[]);
        assert_eq!(snapshot.envelope(&volume).unwrap().len(), 2);
        assert_eq!(snapshot.automated(&volume, 2.0), Some(-6.0));
        assert_eq!(snapshot.automated(&AutomationTarget::TrackPan { track }, 2.0), None);

        // Switched off, the envelope is still there and says nothing.
        session.edit(|project| project.set_automation_enabled(&volume, false)).unwrap();
        let snapshot = Snapshot::capture(&session, &[]);
        assert_eq!(snapshot.envelope(&volume).unwrap().len(), 2);
        assert!(snapshot.lane(&volume).is_some_and(|lane| !lane.enabled));
        assert_eq!(snapshot.automated(&volume, 2.0), None);
    }
}
