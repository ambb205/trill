//! The session's part in reading a device the way a person reads it: what
//! each parameter's value is called, what a named value is as a number,
//! which values are the device's own defaults, and what channels it takes
//! and gives.
//!
//! A built-in device's parameters are in real units and say everything in
//! their [`ParamInfo`](trill_core::device::ParamInfo). A plugin's are
//! numbers from 0 to 1 whose meaning only the plugin knows ("0.25" is
//! "Dynamic 57" on one and "-12 dB" on the next), so the plugin is asked.

use super::{Session, SessionError};
use trill_core::device::{ChannelLayout, DeviceCategory};
use trill_core::{DeviceId, EditError};

impl Session {
    /// What a device shows for values of its parameters: for each
    /// `(position in its parameter list, value)`, the text. A plugin is
    /// asked for its own word for the value (`-6.0 dB`, `Dynamic 57`); a
    /// built-in device's is the value with its unit, or the choice's name.
    ///
    /// The values need not be the current ones: asked about a parameter's
    /// lowest and highest value, this says what its range is in the
    /// device's own terms. A position the device has no parameter at
    /// comes back empty.
    pub fn device_param_texts(&self, id: DeviceId, values: &[(usize, f64)]) -> Result<Vec<String>, SessionError> {
        self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        let own = match &meta.controller {
            Some(controller) => controller.lock().param_texts(values),
            None => vec![None; values.len()],
        };
        let texts = values.iter().zip(own).map(|(&(index, value), own)| {
            let plain = || meta.params.get(index).map(|info| info.display(value)).unwrap_or_default();
            own.filter(|text| !text.trim().is_empty()).unwrap_or_else(plain)
        });
        Ok(texts.collect())
    }

    /// The value of a device's parameter (by its position in the list)
    /// that the device shows as `text`: `Dynamic 57`, `-6 dB`, `On`. The
    /// other way round from [`Session::device_param_texts`], for setting a
    /// parameter in the device's own terms; the value is within the
    /// parameter's range. `None` if the device shows no value that way.
    ///
    /// A plugin is asked first. One that does not convert text, or does
    /// not know this text, is searched: the step of that name, or the
    /// value whose displayed number is the one asked for.
    pub fn device_param_value(&self, id: DeviceId, index: usize, text: &str) -> Result<Option<f64>, SessionError> {
        self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        let Some(info) = meta.params.get(index) else { return Ok(None) };
        let own = meta.controller.as_ref().and_then(|controller| controller.lock().param_value(index, text));
        let value = own.or_else(|| info.parse(text));
        Ok(value.filter(|value| value.is_finite() && (info.min..=info.max).contains(value)))
    }

    /// The value the device itself calls each parameter's default, in the
    /// order of [`Session::device_params`]. For a plugin this is not the
    /// `default` its parameters are listed with, which is the value each
    /// had when the plugin was created (its patch's): this is the value
    /// the plugin declares, the one a parameter has in a fresh instance.
    /// A value that differs from it was set by someone, or by a preset.
    pub fn device_param_defaults(&self, id: DeviceId) -> Result<Vec<f64>, SessionError> {
        self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        let own = meta.controller.as_ref().and_then(|controller| controller.lock().param_defaults());
        let listed = || meta.params.iter().map(|info| info.default).collect();
        Ok(own.filter(|defaults| defaults.len() == meta.params.len()).unwrap_or_else(listed))
    }

    /// How many channels a device takes and gives. Every device is handed
    /// a stereo signal and hands one back; a device with a single input
    /// channel is given left and right mixed
    /// ([`ChannelLayout::folds_input`]), which is where a doubled guitar
    /// goes mono. A plugin says what its main input and output are; a
    /// built-in effect is stereo throughout, and a built-in instrument
    /// takes no audio.
    ///
    /// What a plugin does inside is its own business: one that takes a
    /// stereo pair may still treat it as one channel.
    pub fn device_channels(&self, id: DeviceId) -> Result<ChannelLayout, SessionError> {
        let device = self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(layout) = meta.controller.as_ref().and_then(|controller| controller.lock().channels()) {
            return Ok(layout);
        }
        let instrument =
            self.factory.descriptors().iter().any(|d| d.uri == device.uri && d.category == DeviceCategory::Instrument);
        Ok(if instrument { ChannelLayout { inputs: 0, outputs: 2 } } else { ChannelLayout::STEREO })
    }

    /// Whether a device can listen to a track other than its own: whether
    /// it does anything with a [sidechain](Session::set_sidechain). Of the
    /// built-in devices the compressor does.
    pub fn device_takes_key(&self, id: DeviceId) -> bool {
        self.bank.devices.get(&id).is_some_and(|meta| meta.takes_key)
    }

    /// Has a device listen to `track` in place of its own input, or to its
    /// own input again with `None`: a compressor then turns its own track
    /// down by how loud the other is (a bass ducked to the kick). The
    /// track is heard as it leaves its devices, ahead of its fader and its
    /// mute, so one that is muted still serves as a key. One undo step.
    ///
    /// Refused, with what to do instead, for a device that listens to
    /// nothing but its input, and for a track that would have to be played
    /// both before and after the device's own
    /// ([`Project::set_sidechain`](trill_core::Project::set_sidechain)).
    pub fn set_sidechain(&mut self, id: DeviceId, track: Option<trill_core::TrackId>) -> Result<(), SessionError> {
        let device = self.project.device(id)?;
        if track.is_some() && !self.device_takes_key(id) {
            return Err(SessionError::Device(format!(
                "{id} ({}) listens to nothing but what comes into it. Of the built-in devices the compressor takes a                  sidechain: put one on the track that is to be turned down",
                device.name
            )));
        }
        self.edit(|project| project.set_sidechain(id, track))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AudioBackend;
    use std::sync::Arc;
    use trill_core::ChainRef;
    use trill_dsp::BuiltinFactory;

    fn session() -> Session {
        Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    #[test]
    fn a_built_in_device_shows_its_values_with_their_units_and_reads_them_back() {
        let mut session = session();
        let synth = session.add_device(ChainRef::Master, "builtin:polysynth", None, None).unwrap();
        let params = session.device_params(synth).unwrap();
        let at = |id: &str| params.iter().position(|state| state.info.id == id).unwrap();
        let (cutoff, kind) = (at("cutoff"), at("filter_type"));

        let texts = session.device_param_texts(synth, &[(cutoff, 800.0), (kind, 0.0), (9999, 1.0)]).unwrap();
        assert_eq!(texts[0], params[cutoff].info.display(800.0));
        assert!(texts[0].starts_with("800"), "{texts:?}");
        assert_eq!(texts[2], "", "there is no such parameter");
        // The name of a choice is its value, and so is a number in the
        // parameter's unit; what is out of range is no value.
        assert_eq!(session.device_param_value(synth, kind, &texts[1]).unwrap(), Some(0.0));
        assert_eq!(session.device_param_value(synth, cutoff, &texts[0]).unwrap(), Some(800.0));
        assert_eq!(session.device_param_value(synth, cutoff, "1e12").unwrap(), None);
        assert_eq!(session.device_param_value(synth, cutoff, "bright").unwrap(), None);
        assert_eq!(session.device_param_value(synth, 9999, "1").unwrap(), None);

        // Its own defaults are the ones it is listed with.
        let listed: Vec<f64> = params.iter().map(|state| state.info.default).collect();
        assert_eq!(session.device_param_defaults(synth).unwrap(), listed);
    }

    #[test]
    fn a_built_in_instrument_takes_no_audio_and_an_effect_is_stereo_throughout() {
        let mut session = session();
        let synth = session.add_device(ChainRef::Master, "builtin:polysynth", None, None).unwrap();
        let reverb = session.add_device(ChainRef::Master, "builtin:reverb", None, None).unwrap();
        assert_eq!(session.device_channels(synth).unwrap().describe(), "no audio in, stereo out");
        assert_eq!(session.device_channels(reverb).unwrap(), ChannelLayout::STEREO);
        assert!(session.device_channels(DeviceId(999)).is_err());
        assert!(session.device_param_texts(DeviceId(999), &[]).is_err());
    }
}
