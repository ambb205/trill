//! Handing an instrument to a probe ([`crate::probe`]): what a probe plays
//! through, taken from the session as a render takes the project, so that
//! the probe runs without holding the session and changes nothing in it.

use super::{Session, SessionError, preset_values};
use crate::probe::ProbeJob;
use crate::ready::LoadWatch;
use std::sync::Arc;
use trill_core::device::DeviceCategory;
use trill_core::{Device, DeviceId, TrackId};

/// What a probe plays through.
#[derive(Clone, Debug, PartialEq)]
pub enum ProbeTarget {
    /// The instrument of a track, exactly as it is now: its state, its
    /// preset, what was done in its editor. With `effects`, the notes go
    /// on through every device after it on the track, as far as the
    /// fader.
    Track { track: TrackId, effects: bool },
    /// An instrument by its URI, which need not be in the song, started
    /// from `preset` if one is named (by name, by category and name, or
    /// by id), else as it comes. The preset may be one of the type's own
    /// or one that was saved.
    Device { uri: String, preset: Option<String> },
}

impl Session {
    /// The plugins of the song that seem to be loading what they play:
    /// the ones that were made, or given another state or preset, since
    /// the program last stood still for a few seconds
    /// ([`LoadWatch`]). Notes played to them now may not sound. This
    /// looks at the program and never waits; a window asks it every
    /// frame.
    pub fn loading(&self) -> Vec<DeviceId> {
        self.bank.loads.loading()
    }

    /// The watch behind [`Self::loading`], to ask without the session:
    /// [`LoadWatch::loading_now`] is for a caller that asks once in a
    /// while and may wait a tenth of a second for the answer.
    pub fn load_watch(&self) -> Arc<LoadWatch> {
        self.bank.loads.clone()
    }

    /// What it takes to find out whether `device` is ready to be played,
    /// after it was added, given another preset or had a kit loaded in
    /// its own window: a snapshot to wait on away from the session, and
    /// the pitches to ask it (the ones its track plays, or for an
    /// instrument with no notes yet a spread of the keyboard; none for an
    /// effect, which is given noise to pass).
    ///
    /// ```ignore
    /// let (job, pitches) = session.lock().readiness_job(device)?;
    /// // On a thread that may wait, without the session:
    /// let readiness = job.wait_until_ready(&pitches, Duration::from_secs(10))?;
    /// ```
    ///
    /// What is waited for is a copy of the device, made from its state as
    /// it is now, not the one that is playing: that one cannot be played
    /// notes that nobody is to hear. A sampler's copy finds the samples
    /// its twin has loaded, so the two are ready together; where a plugin
    /// loads everything again for every copy, this takes as long as that
    /// does. [`Self::loading`] is the quick question instead: which of
    /// the song's plugins seem to be loading right now.
    pub fn readiness_job(&mut self, device: DeviceId) -> Result<(ProbeJob, Vec<u8>), SessionError> {
        self.capture_state_of(device);
        let found = self.project.device(device)?.clone();
        let track = self.project.tracks.iter().find(|track| track.devices.iter().any(|other| other.id == device));
        let instrument = track.is_some_and(|track| self.instrument_device(track.id).is_some_and(|i| i.id == device));
        let mut pitches: Vec<u8> = Vec::new();
        if instrument {
            let notes = track.into_iter().flat_map(|track| track.clips.iter()).flat_map(|clip| clip.notes().to_vec());
            for note in notes {
                if !pitches.contains(&note.pitch) && pitches.len() < crate::ready::OPENING_NOTES {
                    pitches.push(note.pitch);
                }
            }
            if pitches.is_empty() {
                pitches = (24..=96).step_by(6).collect();
            }
        }
        let job =
            ProbeJob { devices: vec![found], factory: self.factory.clone(), preset: None, tempo: self.project.tempo };
        Ok((job, pitches))
    }

    /// A snapshot of what `target` names, for a probe to play. Nothing in
    /// the session changes: no undo step, no device added, nothing heard.
    ///
    /// Fails if the track has no instrument, or the URI is not an
    /// instrument this session can create, or has no such preset; the
    /// error says what there is instead.
    pub fn probe_job(&mut self, target: &ProbeTarget) -> Result<ProbeJob, SessionError> {
        let (devices, preset) = match target {
            ProbeTarget::Track { track, effects } => {
                let name = self.project.track(*track)?.name.clone();
                let Some(instrument) = self.instrument_device(*track).map(|device| device.id) else {
                    return Err(SessionError::Device(format!(
                        "track {} ({name}) has no instrument to probe. Probe a track that has one, or a device by \
                         its URI",
                        track.0
                    )));
                };
                // What the running instances have become is what is probed.
                let ids: Vec<DeviceId> = self.project.track(*track)?.devices.iter().map(|device| device.id).collect();
                for id in ids {
                    self.capture_state_of(id);
                }
                let devices = self.project.track(*track)?.devices.iter().skip_while(|device| device.id != instrument);
                let wanted = devices.filter(|device| device.enabled).take(if *effects { usize::MAX } else { 1 });
                (wanted.cloned().collect(), None)
            }
            ProbeTarget::Device { uri, preset } => {
                let descriptors = self.factory.descriptors();
                let Some(descriptor) = descriptors.iter().find(|descriptor| descriptor.uri == *uri) else {
                    return Err(SessionError::Device(format!(
                        "there is no device '{uri}'. The devices that can be created are listed with their URIs"
                    )));
                };
                if descriptor.category != DeviceCategory::Instrument {
                    return Err(SessionError::Device(format!(
                        "'{uri}' ({}) is an effect: a probe plays notes, which takes an instrument",
                        descriptor.name
                    )));
                }
                let mut device = Device {
                    // An id no device of a project has.
                    id: DeviceId(u64::MAX),
                    uri: uri.clone(),
                    name: descriptor.name.clone(),
                    enabled: true,
                    params: Default::default(),
                    state: None,
                    preset: None,
                };
                let mut to_load = None;
                if let Some(wanted) = preset {
                    // By its name or id, or by the path of its file.
                    let (info, saved) = self.wanted_preset(uri, wanted)?;
                    device.preset = Some(info.reference());
                    match (saved, preset_values(descriptor, &info)) {
                        // A preset that was saved is the device as it was
                        // then: its values and its state.
                        (Some(saved), _) => (device.params, device.state) = (saved.params, saved.state),
                        // A preset that is a set of values is the device.
                        (None, Some(values)) => device.params = values.into_iter().collect(),
                        // Any other only the plugin can load, once it exists.
                        (None, None) => to_load = Some(info),
                    }
                }
                (vec![device], to_load)
            }
        };
        Ok(ProbeJob { devices, factory: self.factory.clone(), preset, tempo: self.project.tempo })
    }
}
