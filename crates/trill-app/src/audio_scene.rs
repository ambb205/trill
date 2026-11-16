//! Made-up audio and MIDI devices, for pictures of the Audio and MIDI panel.
//!
//! A screenshot run touches nothing of the machine's hardware, so its panel
//! would always show the same thing: no device at all. A scene puts the
//! engine's scripted devices in the hardware's place, so that the panel can
//! be seen as it is on a machine with a studio's worth of them, or with the
//! chosen interface unplugged. Nothing here opens a real device or port.

use std::sync::Arc;
use trill_engine::AudioConfig;
use trill_engine::host::scripted::{ScriptedDevice, ScriptedHost};
use trill_runtime::midi::scripted::ScriptedMidi;
use trill_runtime::{Parts, Runtime};

/// What the made-up machine has plugged in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AudioScene {
    /// Speakers, an interface and a monitor's output; three MIDI inputs,
    /// one of them held by another program and one switched off.
    Studio,
    /// The interface that was chosen is not plugged in: the speakers play
    /// and the panel says why.
    Fallback,
    /// No audio output and no MIDI input at all.
    None,
}

const INTERFACE: &str = "Studio Interface";

impl AudioScene {
    pub const ALL: [(&'static str, AudioScene); 3] =
        [("studio", AudioScene::Studio), ("fallback", AudioScene::Fallback), ("none", AudioScene::None)];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// What stands in for the machine's audio devices and MIDI ports.
    pub fn parts(self) -> Parts {
        let host = ScriptedHost::new();
        let midi = ScriptedMidi::new();
        if self != AudioScene::None {
            host.add_device(ScriptedDevice::new("Speakers"));
        }
        if self == AudioScene::Studio {
            let interface = ScriptedDevice {
                sample_rates: vec![48_000, 44_100, 88_200, 96_000],
                shared_buffer: 480,
                shared_latency: 960,
                ..ScriptedDevice::new(INTERFACE)
            };
            host.add_device(interface);
            let monitor = ScriptedDevice {
                sample_rates: vec![48_000],
                exclusive: false,
                ..ScriptedDevice::new("Monitor (HDMI)")
            };
            host.add_device(monitor);
            for port in ["Keystation 61", "Launchpad Mini", "Pedalboard"] {
                midi.plug(port);
            }
            midi.refuse("Launchpad Mini", Some("another program is using this port"));
        }
        Parts { audio: Some(Arc::new(host)), midi: Some(Box::new(midi)), ..Parts::default() }
    }

    /// What was chosen on the made-up machine, once the program runs.
    pub fn stage(self, runtime: &Runtime) {
        match self {
            AudioScene::Studio => {
                let chosen = AudioConfig {
                    output: Some(INTERFACE.to_owned()),
                    buffer_size: Some(128),
                    exclusive: true,
                    ..AudioConfig::default()
                };
                let _ = runtime.set_audio(chosen);
                let _ = runtime.set_midi_input_enabled("Pedalboard", false);
                runtime.rescan_midi_inputs();
            }
            AudioScene::Fallback => {
                let chosen = AudioConfig { output: Some(INTERFACE.to_owned()), ..AudioConfig::default() };
                let _ = runtime.set_audio(chosen);
            }
            AudioScene::None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_runtime::RuntimeOptions;

    #[test]
    fn every_scene_starts_and_shows_what_it_says() {
        for (name, scene) in AudioScene::ALL {
            assert_eq!(AudioScene::named(name), Some(scene));
            let runtime = Runtime::start_custom(RuntimeOptions::hermetic(), scene.parts()).unwrap();
            scene.stage(&runtime);
            let status = runtime.audio_status();
            let (outputs, inputs) = (runtime.audio_devices().outputs, runtime.midi_inputs());
            match scene {
                AudioScene::Studio => {
                    assert_eq!(status.info.device.as_deref(), Some(INTERFACE));
                    assert!(status.info.exclusive && status.info.warning.is_none(), "{:?}", status.info);
                    assert_eq!((outputs.len(), inputs.len()), (3, 3));
                    assert!(inputs.iter().any(|input| input.problem.is_some()), "one port is held elsewhere");
                    assert!(inputs.iter().any(|input| !input.enabled), "and one is switched off");
                }
                AudioScene::Fallback => {
                    assert_eq!(status.info.device.as_deref(), Some("Speakers"));
                    let warning = status.info.warning.expect("the stream is not what was asked for");
                    assert!(warning.contains(INTERFACE), "{warning}");
                    assert_eq!(status.config.output.as_deref(), Some(INTERFACE), "and the interface is still wanted");
                }
                AudioScene::None => {
                    assert_eq!(status.info.device, None);
                    assert!(outputs.is_empty() && inputs.is_empty());
                }
            }
        }
        assert_eq!(AudioScene::named("stadium"), None);
    }
}
