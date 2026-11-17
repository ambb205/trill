//! A plugin that has an input to listen to, given another track to listen
//! to: played by the engine through a scripted plugin of each format,
//! which adds what is in its second input to what it passes on. One body
//! for both formats: `clap/tests.rs` and `vst3/tests.rs` each stage it
//! with their own plugins.

use crate::transport::written::Format;
use crate::transport_tests::Noting;
use std::sync::Mutex;
use trill_core::device::{DeviceFactory, DeviceInstance, Purpose};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId};
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::{AudioSources, Processor};

const SR: f64 = 48_000.0;
/// 120 BPM at 48 kHz.
const BEAT: usize = 24_000;

/// A format's scripted plugins, as this test needs them.
pub(crate) struct Stage<'a> {
    pub format: Format,
    pub factory: &'a dyn DeviceFactory,
    /// An instrument that answers a note with one sample.
    pub instrument: String,
    /// An effect that has a second input, and adds what is in it to what
    /// it passes on.
    pub keyed: String,
    /// The serial of an instance, which it reports as a parameter.
    pub serial: fn(&mut DeviceInstance) -> u64,
    /// For a format in which the host switches a plugin's inputs on:
    /// whether the instance with this serial had its second input
    /// switched on, and whether that input was flagged silent in the last
    /// block it was given.
    pub key_input: Option<fn(u64) -> (bool, bool)>,
}

/// The song as the engine keeps it from one change to the next.
struct Rig<'a> {
    noting: &'a Noting<'a>,
    bank: DeviceBank,
    meters: Meters,
    audio: AudioSources,
    processor: Processor,
}

impl Rig<'_> {
    /// Brings the engine in step with `project`, as an edit does, and
    /// plays it from the start: three beats of it, left channel.
    fn played(&mut self, project: &Project) -> Vec<f32> {
        let mut compiler = Compiler {
            factory: self.noting,
            bank: &mut self.bank,
            meters: &mut self.meters,
            audio: &self.audio,
            stand_ins: None,
            sample_rate: SR,
            purpose: Purpose::Playing,
        };
        let (graph, problems) = compiler.compile(project, true);
        assert!(problems.is_empty(), "{problems:?}");
        drop(self.processor.swap_graph(graph));
        self.processor.seek(0.0);
        self.processor.play();
        let (mut left, mut right) = (vec![0.0f32; 3 * BEAT], vec![0.0f32; 3 * BEAT]);
        for (l, r) in left.chunks_mut(512).zip(right.chunks_mut(512)) {
            self.processor.process(l, r);
        }
        self.processor.stop();
        left
    }

    /// The serials of the instances made for `device`, in order.
    fn made_for(&self, device: DeviceId) -> Vec<u64> {
        self.noting.made.lock().unwrap().iter().filter(|(id, _)| *id == device).map(|(_, serial)| *serial).collect()
    }
}

/// A kick on beat 1, and a bass on beat 2 that goes through the keyed
/// effect: the tracks, and the effect.
fn song(stage: &Stage) -> (Project, TrackId, DeviceId) {
    let mut project = Project::new("t");
    let mut track = |name: &str, beat: f64| {
        let track = project.add_track(name);
        project.add_device(ChainRef::Track(track), stage.instrument.clone(), "Instrument", None).unwrap();
        let clip = project.add_midi_clip(track, "clip", 0.0, 4.0).unwrap();
        let note = Note { pitch: 60, start: beat, length: 0.5, velocity: 127 };
        project.clip_mut(clip).unwrap().add_notes(&[note]).unwrap();
        track
    };
    let (kick, bass) = (track("Kick", 1.0), track("Bass", 2.0));
    let effect = project.add_device(ChainRef::Track(bass), stage.keyed.clone(), "Keyed", None).unwrap();
    (project, kick, effect)
}

/// A plugin with a second input hears the track its device listens to in
/// it, beside its own signal and only while it listens; and where the
/// format has the host switch that input on, it is on for a device that
/// listens and off for one that does not.
pub(crate) fn a_plugin_with_a_key_input_hears_the_track_its_device_listens_to(stage: &Stage) {
    let noting =
        Noting { format: stage.format, factory: stage.factory, serial: stage.serial, made: Mutex::new(Vec::new()) };
    let mut rig = Rig {
        noting: &noting,
        bank: DeviceBank::default(),
        meters: Meters::default(),
        audio: AudioSources::default(),
        processor: Processor::new(SR),
    };
    let (mut project, kick, effect) = song(stage);
    let said = |what: &str| format!("{:?}: {what}", stage.format);

    // As it comes: the kick on its beat and the bass on its own, each
    // once. The engine knows that the effect could listen.
    let own = rig.played(&project);
    let (kick_alone, bass_alone) = (own[BEAT], own[2 * BEAT]);
    assert!(kick_alone > 0.1 && bass_alone > 0.1, "{}", said(&format!("{kick_alone} and {bass_alone}")));
    assert_eq!(own.iter().filter(|sample| **sample != 0.0).count(), 4, "{}", said("two notes, begun and ended"));
    assert!(rig.bank.devices[&effect].takes_key, "{}", said("the effect takes a key"));
    assert_eq!(rig.made_for(effect).len(), 1);

    // Listening to the kick: the effect adds the kick to what it passes
    // on, so the kick is heard twice over on its beat, and the bass as it
    // was on its own.
    project.set_sidechain(effect, Some(kick)).unwrap();
    let keyed = rig.played(&project);
    assert!((keyed[BEAT] - 2.0 * kick_alone).abs() < 1e-6, "{}", said(&format!("{} on the kick", keyed[BEAT])));
    assert_eq!(keyed[2 * BEAT], bass_alone, "{}", said("the bass is as it was"));

    // And to its own input again: as it was.
    project.set_sidechain(effect, None).unwrap();
    let again = rig.played(&project);
    assert!(again == own, "{}", said("without the sidechain the song is what it was"));

    let made = rig.made_for(effect);
    match stage.key_input {
        // Nothing to switch on: the one instance did all three.
        None => assert_eq!(made.len(), 1, "{}", said("the plugin was made once")),
        // Made again for the key, with its input switched on and no longer
        // flagged silent; and again without, as the first was.
        Some(key_input) => {
            assert_eq!(made.len(), 3, "{}", said("the plugin was made again for the key, and again without"));
            let found: Vec<(bool, bool)> = made.iter().map(|serial| key_input(*serial)).collect();
            assert_eq!(found, [(false, true), (true, false), (false, true)], "{}", said("switched on, flagged silent"));
        }
    }
}
