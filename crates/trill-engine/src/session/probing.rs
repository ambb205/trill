//! Handing an instrument to a probe ([`crate::probe`]): what a probe plays
//! through, taken from the session as a render takes the project, so that
//! the probe runs without holding the session and changes nothing in it.

use super::{Session, SessionError, preset_values};
use crate::probe::ProbeJob;
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
                    let info = self.find_preset(uri, wanted)?;
                    device.preset = Some(info.reference());
                    match (self.saved_preset(uri, &info)?, preset_values(descriptor, &info)) {
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
