//! An installed plugin that has an input to listen through, given another
//! track to listen to: Surge XT, whose oscillator can be the audio that
//! comes into it, in both of its formats.
//!
//! A track of the built-in synth plays four notes and is muted, so that
//! nothing of it is heard by itself. Beside it Surge XT holds a note on a
//! patch of its makers' that plays what it is handed in place of an
//! oscillator. Played as it comes, the song is silent: nothing is handed
//! to the plugin. With the plugin's device listening to the synth's track,
//! the synth is heard through it.
//!
//! Then the effect of the same makers, which has a sidechain beside its
//! own input: it is made for a device that listens, with that input
//! switched on where the format has the host do it, and plays on; and as
//! a vocoder it shapes the bass by the kick it listens to.
//!
//! What it found (2026-11-17): the synth is silent as it comes and peaks
//! at 0.066 while it listens (CLAP; the VST3 one lists no patches of its
//! own to choose that one by). The vocoder listening to the kick peaks at
//! 0.0091 in both formats, in every one of twelve runs. Not listening, the
//! CLAP one is silent every time: its input is there and holds silence,
//! so the bass is shaped by nothing. The VST3 one, whose sidechain is
//! then switched off, was not the same from run to run: 0.43 in six
//! renders and silent in six. Loud is what a vocoder that
//! shapes the bass by itself would be; why it is sometimes silent is not
//! known, and it is not the key's doing, which is not in those renders.
//!
//! ```text
//! cargo test -p trill-plugins --test key -- --include-ignored --nocapture --test-threads 1
//! ```
//!
//! It makes a handful of instances of two light plugins, one after the
//! other. Nothing of the user's is read or written, and no window opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceFactory, Purpose};
use trill_core::{ChainRef, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::{AudioBackend, AudioSources, Processor, RenderOptions, Session};
use trill_plugins::{ClapConfig, ClapFactory, Scanner, Vst3Config, Vst3Factory};

/// Surge XT and its effect, in both formats.
const SYNTHS: [&str; 2] = ["clap:org.surge-synth-team.surge-xt", "vst3:ABCDEF019182FAEB566D624153675854"];
const EFFECTS: [&str; 2] = ["clap:org.surge-synth-team.surge-xt-fx", "vst3:ABCDEF019182FAEB566D624153465854"];
/// A patch whose first oscillator is the audio that comes into the plugin.
const AUDIO_IN: &str = "Audio In Stereo Osc 1";

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("key").join(name)
}

/// The built-in devices and the plugins installed here, with caches of
/// the test's own.
fn devices() -> Arc<CompositeFactory> {
    let scanner = || Scanner::Executable(PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan")));
    let clap = ClapFactory::new(ClapConfig {
        scanner: scanner(),
        cache_path: Some(scratch("clap").join("scan-cache.json")),
        ..ClapConfig::default()
    })
    .expect("the plugin main thread starts");
    let vst3 = Vst3Factory::new(Vst3Config {
        scanner: scanner(),
        cache_path: Some(scratch("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch("vst3 presets")],
        ..Vst3Config::default()
    })
    .expect("the plugin main thread starts");
    Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(clap), Box::new(vst3)]))
}

fn installed(devices: &dyn DeviceFactory, uri: &str) -> bool {
    let found = devices.descriptors().iter().any(|descriptor| descriptor.uri == uri);
    if !found {
        eprintln!("skipping '{uri}': the plugin is not installed");
    }
    found
}

/// The loudest sample of a render of the song's first eight beats.
fn loudest(session: &mut Session) -> f32 {
    let options = RenderOptions { end_beat: Some(8.0), max_tail_seconds: 0.2, ..RenderOptions::default() };
    let out = session.render_job().render(&options).unwrap_or_else(|error| panic!("{error}"));
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert!(out.left.iter().chain(&out.right).all(|sample| sample.is_finite()));
    out.left.iter().chain(&out.right).fold(0.0f32, |most, sample| most.max(sample.abs()))
}

/// The song as the engine keeps it while it is played: what a session
/// has, without the session.
struct Playing {
    devices: Arc<CompositeFactory>,
    bank: DeviceBank,
    meters: Meters,
    audio: AudioSources,
    processor: Processor,
}

impl Playing {
    fn new(devices: Arc<CompositeFactory>) -> Self {
        Self {
            devices,
            bank: DeviceBank::default(),
            meters: Meters::default(),
            audio: AudioSources::default(),
            processor: Processor::new(48_000.0),
        }
    }

    /// Brings the engine in step with `project`, as an edit does.
    fn follow(&mut self, project: &Project) {
        let mut compiler = Compiler {
            factory: &*self.devices,
            bank: &mut self.bank,
            meters: &mut self.meters,
            audio: &self.audio,
            stand_ins: None,
            sample_rate: 48_000.0,
            purpose: Purpose::Playing,
        };
        let (graph, problems) = compiler.compile(project, true);
        assert!(problems.is_empty(), "{problems:?}");
        drop(self.processor.swap_graph(graph));
    }

    /// Plays the first eight beats (four seconds) and returns the loudest
    /// sample of them.
    fn loudest(&mut self) -> f32 {
        self.processor.seek(0.0);
        self.processor.play();
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let mut most = 0.0f32;
        for _ in 0..4 * 48_000 / 512 {
            self.processor.process(&mut left, &mut right);
            assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
            most = left.iter().chain(&right).fold(most, |most, sample| most.max(sample.abs()));
        }
        self.processor.stop();
        // What still rings is let out before the next pass.
        for _ in 0..48_000 / 512 {
            self.processor.process(&mut left, &mut right);
        }
        most
    }
}

/// Played, not rendered: a render first tries every plugin instrument by
/// itself, to know that it answers the notes it is to play, and this one
/// plays nothing by itself. Such a render is refused
/// (`RenderError::NotAnswered`), with or without a track to listen to.
#[test]
#[ignore = "needs Surge XT (CLAP or VST3); run with `cargo test -p trill-plugins --test key -- --include-ignored --nocapture --test-threads 1`"]
fn a_synth_that_plays_what_comes_into_it_plays_the_track_its_device_listens_to() {
    let devices = devices();
    for uri in SYNTHS {
        if !installed(&*devices, uri) {
            continue;
        }
        // Its presets are read behind the scenes: waited for here.
        let list = devices.presets_within(uri, Duration::from_secs(120)).expect("a plugin of these factories");
        let Some(preset) = list.presets.iter().find(|preset| preset.name == AUDIO_IN) else {
            eprintln!("skipping '{uri}': it has no preset '{AUDIO_IN}' among its {}", list.presets.len());
            continue;
        };

        let mut project = Project::new("key");
        // What is listened to: four notes, on a track that is muted.
        let voice = project.add_track("Voice");
        project.add_device(ChainRef::Track(voice), "builtin:polysynth", "Synth", None).unwrap();
        let clip = project.add_midi_clip(voice, "notes", 0.0, 8.0).unwrap();
        let notes: Vec<Note> =
            [48, 55, 60, 64].iter().zip([0.5, 2.5, 4.5, 6.5]).map(|(pitch, start)| note(*pitch, start)).collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        project.track_mut(voice).unwrap().mute = true;
        // What listens: a key held for the whole of it.
        let held = project.add_track("Listener");
        let plugin = project.add_device(ChainRef::Track(held), uri, "Surge XT", None).unwrap();
        let clip = project.add_midi_clip(held, "held", 0.0, 8.0).unwrap();
        project
            .clip_mut(clip)
            .unwrap()
            .add_notes(&[Note { pitch: 60, start: 0.0, length: 8.0, velocity: 110 }])
            .unwrap();

        let mut playing = Playing::new(devices.clone());
        playing.follow(&project);
        let meta = &playing.bank.devices[&plugin];
        assert!(meta.takes_key, "{uri}: an instrument with an input takes a key");
        let controller = meta.controller.as_ref().expect("a plugin has a controller");
        controller.lock().load_preset(preset).unwrap_or_else(|error| panic!("{uri}: {error}"));
        // The patch is taken on with the plugin's next blocks.
        playing.loudest();

        let alone = playing.loudest();
        project.set_sidechain(plugin, Some(voice)).unwrap();
        playing.follow(&project);
        let listening = playing.loudest();
        project.set_sidechain(plugin, None).unwrap();
        playing.follow(&project);
        let again = playing.loudest();
        eprintln!(
            "{uri}: on '{AUDIO_IN}' the song peaks at {alone:.5} as it comes, {listening:.5} with the plugin \
             listening to the muted synth, and {again:.5} with that taken away again"
        );
        assert!(listening > 0.01, "{uri}: nothing of the track it listens to is heard through the plugin");
        assert!(listening > 20.0 * alone.max(again), "{uri}: it plays as much without anything to listen to");
    }
}

/// [`loudest`], of a song that may be silent.
fn loudest_or_silent(session: &mut Session) -> f32 {
    let options = RenderOptions { end_beat: Some(8.0), max_tail_seconds: 0.2, ..RenderOptions::default() };
    let out = session.render_job().render(&options).unwrap_or_else(|error| panic!("{error}"));
    out.left.iter().chain(&out.right).fold(0.0f32, |most, sample| most.max(sample.abs()))
}

fn note(pitch: u8, start: f64) -> Note {
    Note { pitch, start, length: 1.0, velocity: 110 }
}

#[test]
#[ignore = "needs Surge XT Effects (CLAP or VST3); run with `cargo test -p trill-plugins --test key -- --include-ignored --nocapture --test-threads 1`"]
fn an_effect_with_a_sidechain_is_made_for_a_device_that_listens_and_plays_on() {
    let devices = devices();
    for uri in EFFECTS {
        if !installed(&*devices, uri) {
            continue;
        }
        let mut session = Session::new(devices.clone(), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        let (kick, bass) = session
            .edit(|project| {
                let mut track = |name: &str, pitch: u8| {
                    let track = project.add_track(name);
                    let clip = project.add_midi_clip(track, "notes", 0.0, 8.0)?;
                    let notes: Vec<Note> = (0..8).map(|beat| note(pitch, f64::from(beat))).collect();
                    project.clip_mut(clip)?.add_notes(&notes)?;
                    Ok(track)
                };
                Ok((track("Kick", 36)?, track("Bass", 40)?))
            })
            .unwrap();
        for track in [kick, bass] {
            session.add_device(ChainRef::Track(track), "builtin:polysynth", Some("pluck"), None).unwrap();
        }
        let effect =
            session.add_device(ChainRef::Track(bass), uri, None, None).unwrap_or_else(|error| panic!("{uri}: {error}"));
        let takes = session.device_takes_key(effect);
        eprintln!("{uri}: {}", if takes { "has an input to listen through" } else { "has no input to listen through" });
        if !takes {
            continue;
        }
        let alone = loudest(&mut session);
        session.set_sidechain(effect, Some(kick)).unwrap_or_else(|error| panic!("{uri}: {error}"));
        let listening = loudest(&mut session);
        session.set_sidechain(effect, None).unwrap();
        let again = loudest(&mut session);
        eprintln!("{uri}: the song peaks at {alone:.4} as it comes, {listening:.4} listening, {again:.4} after");
        // What the effect makes of the kick is its business (as it comes,
        // a delay, nothing). The song plays on in all three, as it was.
        assert!(alone > 0.01 && listening > 0.01 && again > 0.01, "{uri}: the song went silent");
        assert!((again - alone).abs() < 0.05 * alone, "{uri}: the song is not what it was");

        // As a vocoder it shapes what comes into it by what it listens
        // to: the bass by the kick. The kick is muted, so that it is heard
        // through the effect or not at all.
        let kinds = session.device_params(effect).unwrap();
        let vocoder = kinds.iter().position(|param| param.info.name == "FX Type").and_then(|index| {
            // The value the plugin itself has for the word.
            let value = session.device_param_value(effect, index, "Vocoder").ok()??;
            Some((kinds[index].info.id.clone(), value))
        });
        let Some(vocoder) = vocoder else {
            eprintln!("{uri}: has no vocoder among the kinds of effect it lists");
            continue;
        };
        let (index, wanted) = (kinds.iter().position(|param| param.info.name == "FX Type").unwrap(), vocoder.1);
        session.set_device_params(effect, &[vocoder]).unwrap();
        let now = session.device_params(effect).unwrap()[index].value;
        let shown = session.device_param_texts(effect, &[(index, now)]).unwrap();
        assert_eq!((now, shown.as_slice()), (wanted, &["Vocoder".to_string()][..]), "{uri}: it is not a vocoder");
        session
            .edit(|project| {
                project.track_mut(kick)?.mute = true;
                Ok(())
            })
            .unwrap();
        // The plugin becomes another kind of effect a moment after it is
        // told to, and a render begins from the state the plugin has when
        // it is asked for: one asked for at once has been heard to come
        // out as the kind before, or as neither.
        std::thread::sleep(Duration::from_millis(1500));
        let alone = loudest_or_silent(&mut session);
        session.set_sidechain(effect, Some(kick)).unwrap();
        let listening = loudest_or_silent(&mut session);
        eprintln!(
            "{uri}: as a vocoder, with the kick muted: {alone:.4} as it comes, {listening:.4} listening to the kick"
        );
        // Shaped by the kick it sounds, quietly, and the same every
        // time. Without anything to listen to it is another thing: how
        // loud is said and not held, because it was not the same from one
        // run to the next (see the top of the file).
        assert!(listening > 0.004 && listening < 0.05, "{uri}: the vocoder does not hear the kick it listens to");
        assert!(alone < 0.25 * listening || alone > 4.0 * listening, "{uri}: listening made no difference");
    }
}
