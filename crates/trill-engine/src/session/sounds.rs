//! Sounds: an instrument and the effects after it, saved and given as one
//! thing.
//!
//! A guitar tone is two plugins, the guitar and its amplifier; a pad is a
//! synth with a chorus and a reverb behind it. Kept one device at a time
//! such a sound is in pieces, and putting it on another track means
//! knowing which pieces belong together. A *sound* keeps them together: it
//! is a preset of its first device that says what follows
//! ([`SavedPreset::after`], [`Preset::after`](trill_core::device::Preset)).
//! So it is listed, searched and asked for as that device type's preset,
//! and whatever takes a preset takes a sound.
//!
//! # What giving one replaces
//!
//! The device it is given to takes the sound's first device on in place,
//! as it takes any preset. Behind it, the effects **that a sound brought**
//! go, and the new sound's effects are made directly behind the device, in
//! their order. An effect that was put on the chain otherwise, or was
//! given a preset of its own since it came, is not a sound's: it stays,
//! behind the new ones. A preset of one device says nothing of what
//! follows and touches nothing there. All of it is one undo step.
//!
//! # What a machine has not got
//!
//! A sound may name a plugin that is not installed here, or one that does
//! not start. The rest of the sound is made without it, and
//! [`Given::without`] says which device that was and why. Only the first
//! device has to be there: without it there is nothing to give the sound
//! to.

use super::{Scope, Session, SessionError};
use crate::saved::{MAX_AFTER, Saved, SavedPresets};
use trill_core::device::{DeviceCategory, DeviceDescriptor};
use trill_core::presets::{Brought, PresetInfo, PresetRef, SavedPreset, SoundDevice, names_after};
use trill_core::{Device, DeviceId, EditError, TrackId};

/// What giving a device a preset came to.
#[derive(Clone, Debug, PartialEq)]
pub struct Given {
    /// The preset it was.
    pub preset: PresetInfo,
    /// Whether the preset is a sound: one that says what follows its
    /// device. The lists below are empty for one that is not.
    pub sound: bool,
    /// The devices that were made behind the first, in their order.
    pub made: Vec<DeviceId>,
    /// The devices an earlier sound had brought there, which went: their
    /// names.
    pub removed: Vec<String>,
    /// What the sound names that was not made, each as the end of a
    /// sentence about the sound: `Amp (vst3:…) is not installed on this
    /// machine`.
    pub without: Vec<String>,
}

/// A device that was added, and what its preset came to if it was given
/// one.
#[derive(Clone, Debug, PartialEq)]
pub struct Added {
    pub device: DeviceId,
    pub given: Option<Given>,
}

/// How saving a track's sound came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedAsSound {
    pub saved: Saved,
    /// The device type the sound is a preset of: its first device's.
    pub uri: String,
    /// What the sound holds: its devices by name, the first one first.
    pub devices: Vec<String>,
    /// What of the track it does not hold, each as a sentence: devices in
    /// front of the instrument, and a device's listening to another track.
    pub left_out: Vec<String>,
}

/// A sound as a browser lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedSound {
    /// The device type it is a preset of, and what that is called.
    pub uri: String,
    pub device: String,
    pub preset: PresetInfo,
    /// What it brings behind its first device, by name.
    pub after: Vec<String>,
    /// Whether someone saved it; otherwise its device type brings it.
    pub saved: bool,
}

/// What the preset `info` brings behind its device, if it is a sound: the
/// saved preset's own, or what the device type's preset of that name says.
pub(super) fn after_of(
    descriptor: Option<&DeviceDescriptor>,
    info: &PresetInfo,
    saved: Option<&SavedPreset>,
) -> Option<Vec<SoundDevice>> {
    match saved {
        Some(saved) => saved.after.clone(),
        None => descriptor?.presets.iter().find(|candidate| candidate.name == info.id)?.after.clone(),
    }
}

/// The devices of `after` that this machine has, each called what its
/// type is called here, and a sentence for each that it has not.
pub(super) fn makeable(
    descriptors: &[DeviceDescriptor],
    after: Option<Vec<SoundDevice>>,
) -> (Option<Vec<SoundDevice>>, Vec<String>) {
    let Some(after) = after else { return (None, Vec::new()) };
    let mut without = Vec::new();
    let mut have = Vec::new();
    for mut device in after {
        match descriptors.iter().find(|descriptor| descriptor.uri == device.uri) {
            Some(found) => {
                device.device = found.name.clone();
                have.push(device);
            }
            None => without.push(format!("{} ({}) is not installed on this machine", device.called(), device.uri)),
        }
    }
    (Some(have), without)
}

/// What the preset `info` of `descriptor`'s type brings behind its device,
/// by name, if it is a sound: `Some` and empty for one that is dry. For
/// saying what a preset in a list is made of, without the session.
pub fn sound_after(descriptor: &DeviceDescriptor, saved: &SavedPresets, info: &PresetInfo) -> Option<Vec<String>> {
    if SavedPreset::name_of(info).is_some() {
        return saved.after(&descriptor.uri, info);
    }
    let own = descriptor.presets.iter().find(|candidate| candidate.name == info.id)?;
    own.after.as_deref().map(names_after)
}

/// The sounds there are to choose from without reading anything: the ones
/// that were saved, of every device type that is installed, and the ones
/// the built-in devices bring with effects behind them.
pub fn listed_sounds(descriptors: &[DeviceDescriptor], saved: &SavedPresets) -> Vec<ListedSound> {
    let named = |uri: &str| descriptors.iter().find(|descriptor| descriptor.uri == uri);
    let mut sounds: Vec<ListedSound> = saved
        .sounds()
        .into_iter()
        .filter_map(|sound| {
            // One whose first device this machine has not got cannot be
            // given to anything.
            let device = named(&sound.uri)?.name.clone();
            Some(ListedSound { uri: sound.uri, device, preset: sound.info, after: sound.after, saved: true })
        })
        .collect();
    for descriptor in descriptors {
        for preset in &descriptor.presets {
            let Some(after) = preset.after.as_deref().filter(|after| !after.is_empty()) else { continue };
            sounds.push(ListedSound {
                uri: descriptor.uri.clone(),
                device: descriptor.name.clone(),
                preset: preset.info(),
                after: names_after(after),
                saved: false,
            });
        }
    }
    sounds
}

impl Session {
    /// Gives a device one of its presets, as [`Session::load_preset`] does,
    /// and says what that came to. For a preset that is a sound: the
    /// device takes the sound's first device on, the effects an earlier
    /// sound brought behind it go, and the sound's own are made there (see
    /// the module documentation). One undo step, whatever was made.
    pub fn give_preset(&mut self, id: DeviceId, preset: &str) -> Result<Given, SessionError> {
        let uri = self.project.device(id)?.uri.clone();
        let (info, saved) = self.wanted_preset(&uri, preset)?;
        let descriptors = self.factory.descriptors();
        let descriptor = descriptors.iter().find(|d| d.uri == uri);
        let after = after_of(descriptor, &info, saved.as_ref());
        let (after, without) = makeable(&descriptors, after);

        // An effect that does not start is taken out again, in the step.
        let own_gesture = self.gesture.is_none() && after.as_ref().is_some_and(|after| !after.is_empty());
        if own_gesture {
            self.begin_gesture();
        }
        let brought = self.take_on(id, &info, saved, descriptor, after.as_deref());
        let given = brought.map(|brought| self.given(info, after.is_some(), brought, without));
        if own_gesture {
            self.end_gesture();
        }
        given
    }

    /// The device `id` made to be what the preset `info` says, and the
    /// effects `after` made behind it.
    fn take_on(
        &mut self,
        id: DeviceId,
        info: &PresetInfo,
        saved: Option<SavedPreset>,
        descriptor: Option<&DeviceDescriptor>,
        after: Option<&[SoundDevice]>,
    ) -> Result<Brought, SessionError> {
        if let Some(saved) = saved {
            return self.load_saved_preset(id, info, saved, after);
        }
        let values = descriptor.and_then(|d| super::preset_values(d, info));
        let state = descriptor.and_then(|d| super::preset_state(d, info));
        match (values, state, after) {
            // A set of values and nothing else: the device stays as it is
            // made, and only what it is told changes.
            (Some(values), None, None) => {
                self.edit_scoped(Scope::ParamsOnly, |project| {
                    let device = project.device_mut(id)?;
                    device.params = values.into_iter().collect();
                    device.preset = Some(info.reference());
                    Ok(())
                })?;
                Ok(Brought::default())
            }
            // A preset that brings a state: the device is made again from
            // it, as it is from a saved preset's. One that is a sound: its
            // effects are made with it.
            (Some(values), state, after) => self.edit(|project| {
                let device = project.device_mut(id)?;
                device.params = values.into_iter().collect();
                if state.is_some() {
                    device.state = state;
                }
                device.preset = Some(info.reference());
                match after {
                    Some(after) => project.bring_after(id, &info.reference(), after),
                    None => Ok(Brought::default()),
                }
            }),
            (None, _, _) => {
                self.load_plugin_preset(id, info)?;
                Ok(Brought::default())
            }
        }
    }

    /// What a preset that was just given came to, once the effects that
    /// did not start are out of the chain again: in the step under way.
    pub(super) fn given(
        &mut self,
        preset: PresetInfo,
        sound: bool,
        brought: Brought,
        mut without: Vec<String>,
    ) -> Given {
        let failed: Vec<(DeviceId, String)> =
            brought.made.iter().filter_map(|id| Some((*id, self.bank.devices.get(id)?.error.clone()?))).collect();
        for (id, reason) in &failed {
            if let Ok(device) = self.project.device(*id) {
                without.push(format!("{} ({}) could not be started: {reason}", device.name, device.uri));
            }
        }
        if !failed.is_empty() {
            let removed = self.edit(|project| {
                for (id, _) in &failed {
                    project.remove_device(*id)?;
                }
                Ok(())
            });
            if let Err(error) = removed {
                log::warn!("a device of a sound that did not start was not taken out again: {error}");
            }
        }
        Given {
            preset,
            sound,
            made: brought.made.into_iter().filter(|id| !failed.iter().any(|(failed, _)| failed == id)).collect(),
            removed: brought.removed.into_iter().map(|device| device.name).collect(),
            without,
        }
    }

    /// Saves the sound of a track as it is now, under `name`: its
    /// instrument and every device behind it, each with all a project
    /// would keep of it (a plugin's state, with what was done in its own
    /// window; a sample player's files; every parameter value of a device
    /// whose sound is its parameters) and in their order. On a track
    /// without an instrument the sound begins at its first device.
    ///
    /// The sound is a preset of its first device's type, in place of the
    /// saved preset of that name if there is one: listed with that type's
    /// presets, under "Saved", and given to a device or a new track like
    /// any other, by its name.
    ///
    /// What it does not hold is in [`SavedAsSound::left_out`] where the
    /// track has any of it: devices in front of the instrument, and the
    /// track a device listens to, which is a track of this song. Sends,
    /// the fader and the rest of the mix are never part of a sound (see
    /// [`SoundDevice`]).
    ///
    /// The track's devices are marked as being that sound, which is one
    /// undo step; the sound itself stays when that is undone.
    pub fn save_sound(
        &mut self,
        track: TrackId,
        name: &str,
        description: &str,
        tags: &[String],
    ) -> Result<SavedAsSound, SessionError> {
        let name = SavedPreset::checked_name(name).map_err(SessionError::Device)?;
        let descriptors = self.factory.descriptors();
        let type_of = |device: &Device| descriptors.iter().find(|descriptor| descriptor.uri == device.uri);
        let (ids, mut left_out) = {
            let on = self.project.track(track)?;
            let instrument = |device: &Device| {
                type_of(device).is_some_and(|descriptor| descriptor.category == DeviceCategory::Instrument)
            };
            let first = on.devices.iter().position(instrument).unwrap_or(0);
            if on.devices.is_empty() {
                return Err(SessionError::Device(format!(
                    "{track} ({}) has no devices, so there is no sound to save",
                    on.name
                )));
            }
            let ids: Vec<DeviceId> = on.devices[first..].iter().map(|device| device.id).collect();
            if ids.len() > MAX_AFTER + 1 {
                return Err(SessionError::Device(format!(
                    "{track} ({}) has {} devices from its instrument on; a sound holds {} at most",
                    on.name,
                    ids.len(),
                    MAX_AFTER + 1
                )));
            }
            let in_front: Vec<String> = on.devices[..first]
                .iter()
                .map(|device| format!("{} comes before the instrument and is not part of the sound", device.name))
                .collect();
            (ids, in_front)
        };
        for id in &ids {
            let meta = self.bank.devices.get(id).ok_or(EditError::DeviceNotFound(*id))?;
            if let Some(error) = &meta.error {
                let name = self.project.device(*id)?.name.clone();
                return Err(SessionError::Device(format!(
                    "{id} ({name}) is unavailable, so the sound cannot be saved whole: {error}"
                )));
            }
        }
        // What a plugin has is asked of it now; what the project holds for
        // it may be from before its editor was last used.
        self.absorb_editor_edits();
        for id in &ids {
            self.capture_state_of(*id);
        }
        let mut kept: Vec<SoundDevice> = Vec::new();
        for id in &ids {
            let plugin = self.bank.devices.get(id).is_some_and(|meta| meta.controller.is_some());
            let device = self.project.device(*id)?;
            let params = if plugin {
                device.params.clone()
            } else {
                // Every value, so that the sound leaves nothing of what a
                // device was to chance.
                self.device_params(*id)?.into_iter().map(|state| (state.info.id, state.value)).collect()
            };
            if let Some(key) = device.sidechain {
                let key = self.project.track(key).map(|track| track.name.clone()).unwrap_or_else(|_| key.to_string());
                left_out.push(format!(
                    "{} listens to {key} in this song; the sound is kept without that, and a device made from it \
                     listens to its own input",
                    device.name
                ));
            }
            kept.push(SoundDevice {
                uri: device.uri.clone(),
                device: type_of(device).map_or_else(|| device.name.clone(), |descriptor| descriptor.name.clone()),
                enabled: device.enabled,
                params,
                state: device.state.clone(),
            });
        }
        let after = kept.split_off(1);
        let first = kept.remove(0);
        let first_is_plugin = self.bank.devices.get(&ids[0]).is_some_and(|meta| meta.controller.is_some());
        if first_is_plugin && first.state.is_none() && first.params.is_empty() {
            return Err(SessionError::Device(format!(
                "{} ({}) gives a host no state to keep, and none of its parameters was set from here, so there is \
                 no sound to save",
                ids[0], first.device
            )));
        }
        let devices: Vec<String> =
            std::iter::once(first.device.clone()).chain(after.iter().map(|device| device.device.clone())).collect();
        let preset = SavedPreset {
            uri: first.uri.clone(),
            device: first.device,
            name,
            description: description.trim().to_string(),
            tags: tags.iter().map(|tag| tag.trim().to_string()).filter(|tag| !tag.is_empty()).collect(),
            params: first.params,
            state: first.state,
            format: Some(trill_core::project::FORMAT_VERSION),
            after: Some(after),
        };
        let saved = self.saved.save(preset).map_err(SessionError::Io)?;
        let reference = saved.info.reference();
        self.edit_scoped(Scope::ParamsOnly, |project| {
            project.device_mut(ids[0])?.preset = Some(reference.clone());
            for id in &ids[1..] {
                project.device_mut(*id)?.preset = Some(PresetRef::brought_by(&reference));
            }
            Ok(())
        })?;
        // The list of this type's presets is another one now.
        self.revision += 1;
        Ok(SavedAsSound { saved, uri: first.uri, devices, left_out })
    }

    /// What the preset `info` of the device type `uri` brings behind its
    /// device, by name, if it is a sound.
    pub fn sound_after(&self, uri: &str, info: &PresetInfo) -> Option<Vec<String>> {
        let descriptors = self.factory.descriptors();
        let descriptor = descriptors.iter().find(|descriptor| descriptor.uri == uri)?;
        sound_after(descriptor, &self.saved, info)
    }
}
