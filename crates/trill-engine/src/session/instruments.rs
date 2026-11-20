//! What the session knows about the instruments its tracks play through:
//! which track is a drum track, which device is a track's instrument, and
//! what was learned about that kind of instrument before.

use super::Session;
use crate::instruments::InstrumentKnowledge;
use crate::instruments::kit::{self, KitMap};
use std::collections::BTreeMap;
use std::sync::Arc;
use trill_core::device::{BEND_RANGE_PARAM, DeviceCategory, DeviceDescriptor, PITCH_BEND_SEMITONES};
use trill_core::{
    ChainRef, Channel, Device, DeviceId, InstrumentProfile, Project, Role, SampledInstrument, TrackId, TrackKind,
};

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
        let Ok(found) = self.project.track(track) else { return false };
        found.devices.iter().any(|device| self.factory.plays_drums(&device.uri))
            // An instrument that is loaded from a file is drums or not as
            // the file has it.
            || self.sampled_of(track).is_some_and(|sampled| sampled.profile.role == Some(Role::Drums))
    }

    /// What a device says of the instrument it has loaded, where it plays
    /// one that is made of recordings and that a file describes: what the
    /// instrument is, and what a track that plays through it is told of
    /// its notes. `None` for any other device, and for one with nothing
    /// loaded.
    pub fn sampled(&self, device: DeviceId) -> Option<Arc<SampledInstrument>> {
        self.bank.devices.get(&device)?.controller.as_ref()?.lock().sampled()
    }

    /// [`sampled`](Self::sampled) of the instrument a track plays through.
    pub fn sampled_of(&self, track: TrackId) -> Option<Arc<SampledInstrument>> {
        self.sampled(self.instrument_of(track)?.id)
    }

    /// The device a track's notes play through: the first instrument in
    /// its chain. `None` for a track without one, and for a plugin that is
    /// not installed, which nothing is known about.
    pub fn instrument_of(&self, track: TrackId) -> Option<&Device> {
        let track = self.project.track(track).ok()?;
        let descriptors = self.factory.descriptors();
        track.devices.iter().find(|device| is_instrument(&descriptors, &device.uri))
    }

    /// How far the pitch wheel of a track's instrument bends, in semitones
    /// each way, where the instrument is a built-in one that has a setting
    /// for it ([`BEND_RANGE_PARAM`]): what the song has it set to. `None`
    /// for a plugin, whose own setting a host cannot read, and for a wheel
    /// that is set to bend nothing.
    pub fn instrument_bend_range(&self, track: TrackId) -> Option<f64> {
        let device = self.instrument_of(track)?;
        let descriptors = self.factory.descriptors();
        let descriptor = descriptors.iter().find(|descriptor| descriptor.uri == device.uri)?;
        let param = descriptor.params.iter().find(|param| param.id == BEND_RANGE_PARAM)?;
        let range = device.params.get(BEND_RANGE_PARAM).copied().unwrap_or(param.default);
        (range > 0.0).then_some(range)
    }

    /// The drums of the kit a track's instrument has loaded, by name,
    /// where the instrument says what is on its keys: a drum plugin whose
    /// state names the drum in each place and the keys that strike it. The
    /// plugin is asked now, so this is the kit it has whichever way it
    /// came by it (a preset, its own window, a song that was opened).
    /// `None` for an instrument that does not say, which is nearly all,
    /// and for a track that does not play drums.
    pub fn kit_of(&self, track: TrackId) -> Option<KitMap> {
        if !self.plays_drums(track) {
            return None;
        }
        let device = self.instrument_of(track)?;
        let controller = self.bank.devices.get(&device.id)?.controller.as_ref()?;
        // Answered on the thread plugins are made on: after the making in
        // hand, and before another is begun.
        let _held = self.hold_makings();
        let sounds = controller.lock().key_sounds()?;
        (!sounds.is_empty()).then(|| kit::drum_map(&sounds))
    }

    /// What the instrument of a track calls its MIDI channels, where it
    /// gives them meanings of their own: what the track says
    /// ([`InstrumentProfile::channels`]: an organ's pedalboard is on its
    /// second channel whatever the organ is set to); or, where the track
    /// says nothing and its instrument is one whose channels follow a
    /// setting of its own (a guitar that has a string to a channel while
    /// a switch of its is on, and not otherwise), what that setting has
    /// them be now. That is read from the instrument each time it is
    /// asked, as a kit is ([`kit_of`](Self::kit_of)), and kept nowhere in
    /// the song. Empty for nearly every instrument, which plays all
    /// sixteen channels alike; and for a plugin that has not arrived yet.
    pub fn channel_names(&self, track: TrackId) -> BTreeMap<Channel, String> {
        let Ok(found) = self.project.track(track) else { return BTreeMap::new() };
        if found.profile.names_channels() {
            return found.profile.channels.clone();
        }
        let Some(device) = self.instrument_of(track) else { return BTreeMap::new() };
        if !InstrumentKnowledge::channels_follow_a_setting(&device.uri) {
            return BTreeMap::new();
        }
        // Its values as it has them now, by the names it gives them.
        let Ok(params) = self.device_params(device.id) else { return BTreeMap::new() };
        let value = |name: &str| params.iter().find(|param| param.info.name == name).map(|param| param.value);
        InstrumentKnowledge::channels_now(&device.uri, &value)
    }

    /// What is known about the instrument a track plays through, for
    /// whoever writes for it: what the track says ([`Track::profile`]);
    /// where it says nothing of the pitch wheel, how far the instrument's
    /// own setting has it bend
    /// ([`instrument_bend_range`](Self::instrument_bend_range)); where its
    /// instrument is one that a file describes, what the file says that
    /// the track does not (the range, whether the keys are drums, the
    /// ways of playing a key chooses, the wheel: read from the instrument
    /// that is loaded each time, and kept nowhere in the song;
    /// [`sampled_of`](Self::sampled_of)); where its instrument gives its
    /// channels meanings as it is set now, what it calls them
    /// ([`channel_names`](Self::channel_names)); and on a
    /// drum track whose instrument says what its kit has on its keys, the
    /// kit's drums by name in place of any other map
    /// ([`kit_of`](Self::kit_of)). The default for a track that is not
    /// there.
    ///
    /// [`Track::profile`]: trill_core::Track::profile
    pub fn profile_of(&self, track: TrackId) -> InstrumentProfile {
        let Ok(found) = self.project.track(track) else { return InstrumentProfile::default() };
        let mut profile = found.profile.clone();
        if profile.bend_range.is_none() {
            // Two semitones is what a profile that says nothing means.
            profile.bend_range =
                self.instrument_bend_range(track).filter(|range| *range != f64::from(PITCH_BEND_SEMITONES));
        }
        if profile.channels.is_empty() {
            profile.channels = self.channel_names(track);
        }
        if let Some(sampled) = self.sampled_of(track) {
            let own = &sampled.profile;
            profile.role = profile.role.or(own.role);
            profile.range = profile.range.or(own.range);
            profile.bend_range = profile.bend_range.or(own.bend_range);
            if profile.articulations.is_empty() {
                profile.articulations = own.articulations.clone();
            }
        }
        if let Some(kit) = self.kit_of(track) {
            let shipped = self
                .instrument_of(track)
                .and_then(|device| InstrumentKnowledge::built_in(&device.uri))
                .map(|known| known.profile);
            profile = kit.under(profile, shipped.as_ref());
        }
        profile
    }

    /// A project that is being opened, with what is known about their
    /// instruments given to the tracks that say nothing of their notes
    /// themselves: a song from before that was known, or from a machine
    /// where it is not, is then written for and read like one made here.
    /// It is not an edit; the song is as unchanged as any just opened.
    pub(super) fn with_known_instruments(&self, mut project: Project) -> Project {
        let descriptors = self.factory.descriptors();
        for track in &mut project.tracks {
            if track.kind != TrackKind::Normal || !track.profile.is_empty() {
                continue;
            }
            // Its instrument; or, where the plugin is not installed here, the first device anything is known of.
            let instrument = track.devices.iter().find(|device| is_instrument(&descriptors, &device.uri));
            let known = match instrument {
                Some(device) => self.instruments.of(&device.uri),
                None => track.devices.iter().find_map(|device| self.instruments.of(&device.uri)),
            };
            if let Some(known) = known {
                track.profile = known.profile;
            }
        }
        project
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
