//! What the session knows about the instruments its tracks play through:
//! which track is a drum track, which device is a track's instrument, and
//! what was learned about that kind of instrument before.

use super::Session;
use crate::instruments::InstrumentKnowledge;
use std::sync::Arc;
use trill_core::device::{DeviceCategory, DeviceDescriptor};
use trill_core::{ChainRef, Device, InstrumentProfile, Role, TrackId};

impl Session {
    /// What is known about instruments by device type: where it is kept is
    /// the store's to be told ([`InstrumentKnowledge::open`]), which an
    /// application does once, when it starts. Until then, and in a session
    /// that is to leave nothing behind, what is learned is kept in memory.
    pub fn instruments(&self) -> &Arc<InstrumentKnowledge> {
        &self.instruments
    }

    /// Whether a track's notes are drums: because the track says so
    /// ([`InstrumentProfile::role`]), or else because it plays through a
    /// drum kit or a plugin that calls itself a drum instrument. False for
    /// a track that is not there.
    pub fn plays_drums(&self, track: TrackId) -> bool {
        let Ok(found) = self.project.track(track) else { return false };
        match found.profile.role {
            Some(role) => role == Role::Drums,
            None => self.instrument_plays_drums(track),
        }
    }

    /// Whether a track plays through a drum kit or a plugin that calls
    /// itself a drum instrument, whatever the track itself says: what
    /// [`plays_drums`](Self::plays_drums) falls back on, and what a track
    /// that says nothing is taken for.
    pub fn instrument_plays_drums(&self, track: TrackId) -> bool {
        let Ok(track) = self.project.track(track) else { return false };
        track.devices.iter().any(|device| self.factory.plays_drums(&device.uri))
    }

    /// The device a track's notes play through: the first instrument in
    /// its chain. `None` for a track without one, and for a plugin that is
    /// not installed, which nothing is known about.
    pub fn instrument_of(&self, track: TrackId) -> Option<&Device> {
        let track = self.project.track(track).ok()?;
        let descriptors = self.factory.descriptors();
        track.devices.iter().find(|device| is_instrument(&descriptors, &device.uri))
    }

    /// The profile a track should be given along with the instrument
    /// `descriptor`, about to be added to `chain`: what is known about that
    /// kind of instrument, if the track has no instrument yet and nobody
    /// has said anything about its notes.
    pub(super) fn profile_for_new_instrument(
        &self,
        chain: ChainRef,
        descriptor: &DeviceDescriptor,
        descriptors: &[DeviceDescriptor],
    ) -> Option<(TrackId, InstrumentProfile)> {
        let ChainRef::Track(id) = chain else { return None };
        if descriptor.category != DeviceCategory::Instrument {
            return None;
        }
        let track = self.project.track(id).ok()?;
        let has_instrument = track.devices.iter().any(|device| is_instrument(descriptors, &device.uri));
        if has_instrument || !track.profile.is_empty() {
            return None;
        }
        self.instruments.of(&descriptor.uri).map(|known| (id, known.profile))
    }
}

fn is_instrument(descriptors: &[DeviceDescriptor], uri: &str) -> bool {
    descriptors.iter().any(|descriptor| descriptor.uri == uri && descriptor.category == DeviceCategory::Instrument)
}
