//! What the views read: a [`Snapshot`] of the project, taken only when the
//! session's revision changes, and the [`Live`] values that move on their
//! own (transport, meters, CPU load).
//!
//! The session is shared with other front ends (an MCP server editing from
//! another thread), so views never look at it directly. They paint from the
//! snapshot, which costs one project clone per edit and lets the session
//! lock be released before any painting starts.

use super::units::{METER_FLOOR_DB, gain_to_db};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use trill_core::{Clip, ClipId, DeviceId, Project, Track, TrackId};
use trill_engine::{Meter, ParamState, Session};

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
    /// Which track's devices `params` covers, besides the master's.
    pub params_track: Option<TrackId>,
    /// Parameters of the devices the Devices tab shows, or why a device has
    /// none (it failed to load).
    pub params: HashMap<DeviceId, Result<Vec<ParamState>, String>>,
    /// Meter handles, in track order. Reading a meter needs no lock.
    track_meters: Vec<(TrackId, Arc<Meter>)>,
    master_meter: Arc<Meter>,
}

impl Snapshot {
    /// Copies what the views need out of the session. `devices_for` is the
    /// track whose device parameters to include.
    pub fn capture(session: &Session, devices_for: Option<TrackId>) -> Self {
        let project = session.project().clone();
        let shown_track = devices_for.and_then(|id| project.track(id).ok());
        let params = shown_track
            .into_iter()
            .flat_map(|track| track.devices.iter())
            .chain(project.master.devices.iter())
            .map(|device| (device.id, session.device_params(device.id).map_err(|error| error.to_string())))
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
            params_track: shown_track.map(|track| track.id),
            params,
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
    /// Song position in beats.
    pub position: f64,
    /// Smoothed fraction of the audio thread's time budget in use.
    pub cpu_load: f32,
    /// Audio callbacks that panicked and were replaced by silence.
    pub audio_faults: u32,
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
        let before = Snapshot::capture(&session, None);
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

        let after = Snapshot::capture(&session, Some(track));
        assert!(after.revision > before.revision);
        assert!(after.can_undo && after.modified);
        assert_eq!(after.track_index(track), Some(0));
        assert_eq!(after.track(track).unwrap().name, "Lead");
        let (owner, found) = after.clip(clip).unwrap();
        assert_eq!((owner.id, found.name.as_str(), found.notes().len()), (track, "Riff", 1));
        assert_eq!(after.params_track, Some(track));
        let synth_params = after.params[&synth].as_ref().unwrap();
        assert!(synth_params.iter().any(|param| param.info.id == "cutoff"));

        // Without a track selected only the master's devices are covered.
        let master_only = Snapshot::capture(&session, None);
        assert!(!master_only.params.contains_key(&synth));
        // A track that no longer exists is not an error.
        let stale = Snapshot::capture(&session, Some(TrackId(9999)));
        assert_eq!(stale.params_track, None);
    }

    #[test]
    fn live_tracks_meters_for_current_tracks_only() {
        let mut session = session();
        let track = session.edit(|project| Ok(project.add_track("A"))).unwrap();
        let mut live = Live::default();
        live.update_meters(&Snapshot::capture(&session, None), 0.016);
        assert_eq!(live.track_meter(track), MeterState::default());
        assert!(!live.meters_moving());

        session.edit(|project| project.remove_track(track).map(|_| ())).unwrap();
        live.update_meters(&Snapshot::capture(&session, None), 0.016);
        assert!(live.tracks.is_empty());
    }
}
