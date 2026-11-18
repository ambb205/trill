//! A song that plays on more than one thread: the same samples as on
//! one, at any buffer size and on any number of threads, through
//! everything a playing song has that a render has not (keys played by
//! hand, a take being recorded, the metronome, a graph swapped in while
//! it plays, a loop that comes round, a jump); threads that are there
//! only once a song needs them; and a device that fails on one of them.
//!
//! The song of the first test is made to have every way one track can
//! wait for another, as the one in `threads.rs` is for a render.

mod rig;

use rig::*;
use std::sync::Arc;
use trill_core::device::{CompositeFactory, DeviceFactory, EventKind, Purpose};
use trill_core::project::{AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters, StandIns};
use trill_engine::graph::Graph;
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, ScriptedFactory};
use trill_engine::team::Team;
use trill_engine::{AudioSources, Processor};

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100 }
}

fn device(project: &mut Project, chain: ChainRef, uri: &str, params: &[(&str, f64)]) -> DeviceId {
    let id = project.add_device(chain, uri, uri, None).unwrap();
    let device = project.device_mut(id).unwrap();
    device.params.extend(params.iter().map(|(name, value)| ((*name).to_string(), *value)));
    id
}

fn part(project: &mut Project, name: &str, instrument: &str, notes: &[Note]) -> TrackId {
    let track = project.add_track(name);
    device(project, ChainRef::Track(track), instrument, &[]);
    let clip = project.add_midi_clip(track, "clip", 0.0, 16.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
    track
}

fn points(points: &[(f64, f64)]) -> Vec<AutomationPoint> {
    points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect()
}

/// Sixteen beats of a line that keeps an instrument busy.
fn busy(pitches: &[u8]) -> Vec<Note> {
    (0..32).map(|step| note(pitches[step % pitches.len()], step as f64 * 0.5, 0.4)).collect()
}

/// Two groups, three returns (one fed by a group), a compressor that
/// listens to another track, a master that does, and faders, pans, sends
/// and a device's parameter that move. Returns the track a hand plays.
fn song() -> (Project, TrackId) {
    let mut project = Project::new("cores");
    project.tempo = 132.0;
    let room = project.add_track_of_kind("Room", TrackKind::Return);
    device(&mut project, ChainRef::Track(room), "builtin:reverb", &[("mix", 1.0)]);
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    device(&mut project, ChainRef::Track(echo), "builtin:delay", &[]);
    let crush = project.add_track_of_kind("Crush", TrackKind::Return);
    device(&mut project, ChainRef::Track(crush), "builtin:saturator", &[]);
    let band = project.add_track_of_kind("Band", TrackKind::Group);
    device(&mut project, ChainRef::Track(band), "builtin:compressor", &[("threshold", -30.0), ("makeup", 3.0)]);
    let keys = project.add_track_of_kind("Keys", TrackKind::Group);
    device(&mut project, ChainRef::Track(keys), "builtin:eq", &[]);

    let drums = part(&mut project, "Drums", DRUMS, &busy(&[36, 42, 38, 42, 36, 36, 38, 49]));
    project.set_track_group(drums, Some(band)).unwrap();
    project.set_send(drums, room, Some(-14.0)).unwrap();

    let bass = part(&mut project, "Bass", BASS, &busy(&[40, 40, 43, 45]));
    device(&mut project, ChainRef::Track(bass), AMP, &[]);
    // The bass ducks under the drums.
    let duck = device(&mut project, ChainRef::Track(bass), "builtin:compressor", &[("threshold", -36.0)]);
    project.set_sidechain(duck, Some(drums)).unwrap();
    project.set_track_group(bass, Some(band)).unwrap();
    project.track_mut(bass).unwrap().pan = -0.2;

    let lead = part(&mut project, "Lead", "builtin:polysynth", &busy(&[60, 64, 67, 72, 71, 67]));
    let filter = device(&mut project, ChainRef::Track(lead), "builtin:filter", &[]);
    project.set_track_group(lead, Some(keys)).unwrap();
    project.set_send(lead, echo, Some(-9.0)).unwrap();
    project.set_send(lead, room, Some(-12.0)).unwrap();
    let cutoff = AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() };
    project.set_automation(cutoff, points(&[(0.0, 400.0), (8.0, 6000.0), (16.0, 900.0)])).unwrap();

    let pad =
        part(&mut project, "Pad", "builtin:polysynth", &[note(48, 0.0, 8.0), note(55, 0.0, 8.0), note(52, 8.0, 8.0)]);
    project.set_track_group(pad, Some(keys)).unwrap();
    project.set_automation(AutomationTarget::TrackVolume { track: pad }, points(&[(0.0, -30.0), (6.0, -4.0)])).unwrap();
    project.set_automation(AutomationTarget::TrackPan { track: pad }, points(&[(0.0, -0.8), (16.0, 0.8)])).unwrap();
    let throw = AutomationTarget::Send { track: pad, to: crush };
    project.set_automation(throw, points(&[(0.0, -70.0), (7.0, -70.0), (9.0, -8.0), (16.0, -70.0)])).unwrap();

    let bell = part(&mut project, "Bell", "builtin:polysynth", &busy(&[84, 79, 76]));
    project.track_mut(bell).unwrap().volume_db = -9.0;
    project.set_send(bell, echo, Some(-6.0)).unwrap();
    let second = part(&mut project, "Second bass", BASS, &busy(&[52, 47, 45, 47]));
    device(&mut project, ChainRef::Track(second), AMP, &[]);
    project.set_send(second, crush, Some(-12.0)).unwrap();
    project.track_mut(second).unwrap().pan = 0.4;

    project.set_send(keys, room, Some(-10.0)).unwrap();
    project.set_send(band, crush, Some(-18.0)).unwrap();
    device(&mut project, ChainRef::Master, "builtin:eq", &[]);
    let pump = device(&mut project, ChainRef::Master, "builtin:compressor", &[("threshold", -24.0)]);
    project.set_sidechain(pump, Some(drums)).unwrap();
    device(&mut project, ChainRef::Master, "builtin:limiter", &[("gain", 3.0)]);
    project.set_loop(true, 2.0, 6.0).unwrap();
    (project, bell)
}

/// The controller's side of a graph that plays: what a session keeps.
struct Controller {
    factory: Arc<dyn DeviceFactory>,
    bank: DeviceBank,
    meters: Meters,
    stand_ins: StandIns,
    audio: AudioSources,
}

impl Controller {
    fn new(scripted: &ScriptedFactory) -> Self {
        Self {
            factory: Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())])),
            bank: DeviceBank::default(),
            meters: Meters::default(),
            stand_ins: StandIns::default(),
            audio: AudioSources::default(),
        }
    }

    /// A graph of `project` that says what of it can be made at once, and
    /// how many threads it can use.
    fn compile(&mut self, project: &Project) -> (Box<Graph>, usize) {
        let mut compiler = Compiler {
            factory: self.factory.as_ref(),
            bank: &mut self.bank,
            meters: &mut self.meters,
            audio: &self.audio,
            stand_ins: Some(&mut self.stand_ins),
            sample_rate: SR,
            purpose: Purpose::Playing,
        };
        let (mut graph, problems) = compiler.compile(project, true);
        assert!(problems.is_empty(), "{problems:?}");
        let width = graph.spread(project, self.factory.as_ref());
        (graph, width)
    }
}

/// Plays the song for a while as a session would, on `threads` threads
/// and in buffers of `buffer` frames, with things happening to it on the
/// way, and returns everything that came out.
fn played(loading: Loading, threads: usize, buffer: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut project, by_hand) = song();
    // (Devices of their own for every time the song is played: what they
    // keep between them begins where it does in a program just started.)
    let scripted = ScriptedFactory::loading(loading);
    let mut controller = Controller::new(&scripted);
    let mut processor = Processor::new(SR);
    let (graph, width) = controller.compile(&project);
    assert!(width >= 2, "the song can be made on more than one thread: {width}");
    let mut retired = vec![processor.swap_graph(graph)];
    if threads > 1 {
        assert!(processor.play_over(Some(Box::new(Team::new(threads - 1)))).is_none());
        // Every block from the first, whatever it costs.
        processor.spread_from(0.0);
    }
    let (mut left, mut right) = (vec![0.0f32; buffer], vec![0.0f32; buffer]);
    let (mut out_left, mut out_right) = (Vec::new(), Vec::new());
    let mut run = |processor: &mut Processor, frames: usize| {
        let mut done = 0;
        while done < frames {
            let size = buffer.min(frames - done);
            processor.process(&mut left[..size], &mut right[..size]);
            out_left.extend_from_slice(&left[..size]);
            out_right.extend_from_slice(&right[..size]);
            done += size;
        }
    };
    let beat = (SR * 60.0 / 132.0) as usize;

    // Standing still, with a key played by hand.
    run(&mut processor, 2_000);
    processor.live_event(by_hand, EventKind::NoteOn { pitch: 72, velocity: 0.9 });
    run(&mut processor, 3_000);
    processor.live_event(by_hand, EventKind::NoteOff { pitch: 72 });
    // Playing, with the metronome on, round the loop more than once.
    processor.set_metronome(true);
    processor.play();
    run(&mut processor, 3 * beat);
    processor.live_event(by_hand, EventKind::NoteOn { pitch: 76, velocity: 0.7 });
    processor.live_event(by_hand, EventKind::Control { controller: 1, value: 0.5 });
    run(&mut processor, 3 * beat + 777);
    processor.live_event(by_hand, EventKind::NoteOff { pitch: 76 });
    // An edit arrives while it plays: a track more, a device less on
    // another, the loop gone.
    let extra = part(&mut project, "Extra", DRUMS, &busy(&[36, 38]));
    project.track_mut(extra).unwrap().volume_db = -6.0;
    project.set_loop(false, 2.0, 6.0).unwrap();
    let (graph, width) = controller.compile(&project);
    retired.push(processor.swap_graph(graph));
    run(&mut processor, 2 * beat);
    // A jump while it plays, and on to where the song ends.
    processor.seek(9.25);
    run(&mut processor, 4 * beat);
    processor.set_metronome(false);
    processor.stop();
    run(&mut processor, SR as usize / 2);
    assert!(!scripted.overlapped(), "two devices that keep something between them were run at once");
    if threads > 1 {
        assert_eq!(processor.threads(), threads.min(width), "it was made on the threads it was given");
        assert!(processor.take_spread_blocks() > 0);
    } else {
        assert_eq!((processor.threads(), processor.take_spread_blocks()), (1, 0));
    }
    (out_left, out_right)
}

#[test]
fn a_song_that_plays_is_the_same_samples_on_one_thread_and_on_many_at_any_buffer_size() {
    // With devices that keep nothing between them, which are all made at
    // once; and with instruments that count their notes together, which
    // take turns in the order of the song while the amplifiers behind
    // them go on beside each other.
    let sharing = Loading { shares: true, ..Loading::default() };
    for (what, loading) in [("apart", Loading::default()), ("in turn", sharing)] {
        for buffer in [64, 128, 480, 512, 1024] {
            let (left, right) = played(loading, 1, buffer);
            assert!(left.iter().any(|sample| sample.abs() > 0.05), "the song sounds");
            for threads in [2, 3, 8] {
                let (also_left, also_right) = played(loading, threads, buffer);
                let same = also_left == left && also_right == right;
                assert!(same, "{what}, {threads} threads at {buffer} frames: other samples than one thread plays");
            }
        }
    }
}

// ---- through a session ---------------------------------------------------------------

/// A session of three instruments that write down what they are told, a
/// synth and a return, made on `threads` threads from the first block.
fn band(threads: usize) -> (Rig, [TrackId; 3]) {
    let mut rig = Rig::new();
    rig.host.keep_output();
    rig.session.set_play_threads(Some(threads));
    rig.session.spread_from(0.0);
    let probes = [rig.probe_track("One"), rig.probe_track("Two"), rig.probe_track("Three")];
    let synth = rig.sine_track("Synth");
    rig.session
        .edit(|project| {
            let room = project.add_track_of_kind("Room", TrackKind::Return);
            project.add_device(ChainRef::Track(room), "builtin:reverb", "Reverb", None)?;
            project.set_send(synth, room, Some(-9.0))?;
            for (index, track) in probes.iter().chain([&synth]).enumerate() {
                let clip = project.add_midi_clip(*track, "clip", 0.0, 8.0)?;
                let notes: Vec<Note> =
                    (0..8).map(|step| note(48 + index as u8 * 5 + step as u8 % 3, f64::from(step), 0.5)).collect();
                project.clip_mut(clip)?.add_notes(&notes)?;
            }
            Ok(())
        })
        .unwrap();
    // The threads are made once the song has asked for them, by the
    // session's own chores, and are there with the buffer after.
    rig.run(2);
    rig.session.housekeeping();
    rig.run(2);
    (rig, probes)
}

/// Everything the probes were told, by device and then by when.
fn told(rig: &Rig) -> Vec<(u64, u64, String)> {
    let mut seen: Vec<_> =
        rig.probes.take().into_iter().map(|seen| (seen.device.0, seen.frame, format!("{:?}", seen.kind))).collect();
    seen.sort_by_key(|seen| (seen.0, seen.1));
    seen
}

#[test]
fn a_take_played_while_the_song_is_made_on_several_threads_is_the_take_one_thread_records() {
    let take = |threads: usize| {
        let (mut rig, probes) = band(threads);
        let load = rig.session.take_load();
        assert_eq!(load.threads, threads.min(4), "four tracks can be made at once: {load:?}");
        rig.session.edit(|project| project.set_loop(true, 0.0, 4.0)).unwrap();
        rig.session.set_metronome(true);
        rig.session.set_armed(probes[1], true).unwrap();
        rig.run(2);
        let asked = rig.now();
        rig.session.record().unwrap();
        // A bar of count-in, then round the loop twice with keys, a pedal
        // and a wheel on the way.
        let rolling = asked + 4.0 * FRAMES_PER_BEAT;
        let heard = |beat: f64| rolling + beat * FRAMES_PER_BEAT + LATENCY;
        rig.key(heard(0.5), on(60));
        rig.key(heard(1.25), off(60));
        rig.key(heard(1.5), pedal(true));
        rig.key(heard(2.0), on(64));
        rig.key(heard(2.5), EventKind::PitchBend { value: 0.25 });
        rig.key(heard(3.0), off(64));
        rig.key(heard(3.9), on(67));
        rig.key(heard(4.6), off(67));
        rig.key(heard(5.0), pedal(false));
        rig.key(heard(6.0), on(72));
        rig.run_until(heard(7.0));
        rig.session.stop();
        rig.run(4);
        assert_eq!(rig.session.audio_faults(), 0);
        let clips = rig.session.last_take().to_vec();
        let notes: Vec<Vec<(u8, f64, f64)>> = clips.iter().map(|clip| timeline(&rig.session, clip.clip)).collect();
        let (left, right) = rig.host.take_output();
        (told(&rig), notes, left, right, rig.session.take_load().spread_blocks)
    };
    let (told, notes, left, right, spread) = take(1);
    assert!(notes.iter().flatten().count() >= 4 && spread == 0, "{notes:?}, {spread} blocks spread");
    assert!(left.iter().any(|sample| sample.abs() > 0.01), "the count-in and the synth are heard");
    for threads in [2, 4] {
        let (also_told, also_notes, also_left, also_right, spread) = take(threads);
        assert!(spread > 100, "the song was spread: {spread} blocks");
        assert_eq!(also_notes, notes, "{threads} threads: another take");
        assert_eq!(also_told.len(), told.len(), "{threads} threads");
        assert!(also_told == told, "{threads} threads: the instruments were told something else");
        assert!(also_left == left && also_right == right, "{threads} threads: other samples");
    }
}

#[test]
fn threads_are_made_when_a_song_asks_for_them_and_sent_home_when_it_may_not_have_them() {
    // A song that one thread plays with nearly all of its time to spare
    // is never spread, and no thread is made for it.
    let mut rig = Rig::new();
    rig.session.set_play_threads(Some(4));
    let (one, two) = (rig.probe_track("One"), rig.probe_track("Two"));
    rig.session
        .edit(|project| {
            for track in [one, two] {
                let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
                project.clip_mut(clip)?.add_notes(&[note(60, 0.0, 1.0)])?;
            }
            Ok(())
        })
        .unwrap();
    rig.session.play();
    for _ in 0..20 {
        rig.run(10);
        rig.session.housekeeping();
    }
    let load = rig.session.take_load();
    assert_eq!((load.threads, load.spread_blocks, load.allowed), (1, 0, 2), "{load:?}");
    assert!(load.team.is_none(), "threads were made for a song that needs none");

    // Told to spread whatever it costs, it asks, is given them at the
    // next round of chores, and is made on them.
    rig.session.spread_from(0.0);
    rig.run(2);
    assert_eq!(rig.session.take_load().threads, 1, "nobody has made the threads yet");
    rig.session.housekeeping();
    rig.run(20);
    let load = rig.session.take_load();
    assert_eq!(load.threads, 2, "{load:?}");
    assert!(load.spread_blocks >= 19 && load.team.is_some_and(|team| team.rounds >= 19), "{load:?}");

    // More tracks that can be made at once: more threads, with the next
    // round of chores.
    rig.probe_track("Three");
    rig.probe_track("Four");
    rig.run(2);
    rig.session.housekeeping();
    rig.run(20);
    let load = rig.session.take_load();
    assert_eq!((load.threads, load.allowed), (4, 4), "{load:?}");

    // One thread again: the others go home, and the song plays on.
    rig.session.set_play_threads(Some(1));
    rig.run(4);
    rig.session.housekeeping();
    let _ = rig.session.take_load();
    rig.run(20);
    let load = rig.session.take_load();
    assert_eq!((load.threads, load.spread_blocks, load.allowed), (1, 0, 1), "{load:?}");
    assert!(load.team.is_none());
    assert!(rig.session.transport().playing);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_song_nothing_is_known_of_is_spread_from_its_first_block_until_it_has_played_a_while() {
    // Two instruments that cost nothing to speak of, and threads that are
    // there before the song is.
    let mut project = Project::new("unknown");
    let one = part(&mut project, "One", "builtin:polysynth", &[note(60, 0.0, 1.0)]);
    part(&mut project, "Two", "builtin:polysynth", &[note(64, 0.0, 1.0)]);
    let scripted = ScriptedFactory::new();
    let mut controller = Controller::new(&scripted);
    let mut processor = Processor::new(SR);
    assert!(processor.play_over(Some(Box::new(Team::new(3)))).is_none());
    let (graph, width) = controller.compile(&project);
    assert_eq!(width, 2);
    let mut retired = vec![processor.swap_graph(graph)];
    let (mut left, mut right) = (vec![0.0f32; 480], vec![0.0f32; 480]);
    let mut run = |processor: &mut Processor, buffers: usize| {
        (0..buffers).for_each(|_| processor.process(&mut left, &mut right));
    };

    // What it costs is not known: it is made on its threads from the
    // first block, and for as long as it stands still.
    assert_eq!(processor.threads(), 2);
    run(&mut processor, 200);
    assert_eq!((processor.threads(), processor.take_spread_blocks()), (2, 200));
    assert!(!processor.wants_threads(), "it has not shown that it needs any");

    // A quarter of a second of playing shows that one thread has it made
    // with nearly all its time to spare, and one thread it is.
    processor.play();
    run(&mut processor, 20);
    assert_eq!(processor.threads(), 2, "0.2 s is not long enough to know");
    run(&mut processor, 10);
    assert_eq!(processor.threads(), 1);
    processor.stop();
    let _ = processor.take_spread_blocks();
    run(&mut processor, 100);
    assert_eq!((processor.threads(), processor.take_spread_blocks()), (1, 0), "nor is it spread standing still again");

    // An edit that brings no device changes nothing of that.
    let clip = project.track(one).unwrap().clips[0].id;
    project.clip_mut(clip).unwrap().add_notes(&[note(67, 2.0, 1.0)]).unwrap();
    retired.push(processor.swap_graph(controller.compile(&project).0));
    run(&mut processor, 10);
    assert_eq!(processor.threads(), 1);

    // One that brings a device does: what the song costs with it is not
    // known until it has played again.
    device(&mut project, ChainRef::Track(one), "builtin:reverb", &[]);
    retired.push(processor.swap_graph(controller.compile(&project).0));
    assert_eq!(processor.threads(), 2);
    processor.play();
    run(&mut processor, 30);
    assert_eq!(processor.threads(), 1);
    assert_eq!(retired.len(), 3);
}

#[test]
fn a_stream_whose_thread_is_an_audio_thread_has_its_threads_before_its_song_is_heard() {
    // The thread that asks for the buffers here is this test's own. It
    // asks the system for its audio class, as a device's thread does.
    trill_engine::host::priority::AudioPriority::raise_for_good();
    let mut rig = Rig::new();
    rig.session.set_play_threads(Some(4));
    // (A buffer, so that the engine has seen whose thread it is.)
    rig.run(2);
    let load = rig.session.take_load();
    assert!(load.audio_class.is_some() && load.team.is_none(), "{load:?}");
    rig.probe_track("One");
    rig.probe_track("Two");
    // Nobody has done the session's chores and the song has asked for
    // nothing: the threads came with the graph, and the first buffer of
    // the song was made on them.
    rig.run(1);
    let load = rig.session.take_load();
    assert_eq!((load.threads, load.spread_blocks), (2, 1), "{load:?}");
    assert!(load.team.is_some_and(|team| team.rounds == 1), "{load:?}");
    // It turns out to need none of them.
    rig.session.play();
    rig.run(40);
    let _ = rig.session.take_load();
    rig.run(10);
    let load = rig.session.take_load();
    assert_eq!((load.threads, load.spread_blocks), (1, 0), "{load:?}");
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_song_that_is_more_than_the_machine_plays_in_time_is_said_so_with_what_costs_most() {
    let mut rig = Rig::new();
    rig.session.set_play_threads(Some(1));
    let slow = rig.probe_track("Slow");
    rig.sine_track("Synth");
    rig.run(20);
    let keeping = rig.session.keeping_up();
    assert!(keeping.short.is_none() && rig.session.falling_short().is_none());
    assert_eq!((keeping.load.callbacks, keeping.load.late, keeping.load.threads), (20, 0, 1), "{:?}", keeping.load);
    assert!((keeping.seconds - 0.2).abs() < 1e-9 && keeping.sample_rate == SR, "{keeping:?}");
    assert_eq!(keeping.tracks.iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["Slow", "Synth"]);

    // The probe takes longer over every block than a buffer lasts.
    rig.probes.slow_ns.store(12_000_000, std::sync::atomic::Ordering::Relaxed);
    rig.run(12);
    let keeping = rig.session.keeping_up();
    let short = keeping.short.clone().expect("the song falls short");
    assert_eq!((short.episode, short.late, short.threads, short.allowed), (1, 12, 1, 1), "{short:?}");
    assert_eq!(keeping.load.late, 12);
    assert!(keeping.load.mean > 0.3 && keeping.load.worst > 1.0, "{:?}", keeping.load);
    // The track that costs it is named, with what it takes; the synth,
    // which costs next to nothing, is not.
    let (name, share) = &short.costliest[0];
    assert!(name == "Slow" && *share > 0.4 && short.costliest.len() == 1, "{short:?}");
    let sentence = short.sentence();
    assert!(sentence.contains("12 of the last 32 buffers came late") && sentence.contains("What costs most: Slow ("));
    assert_eq!(rig.session.falling_short(), Some(&short));
    let costs = &keeping.tracks;
    assert!(costs[0].track == slow && costs[0].cost.longest >= 12_000_000 && costs[0].cost.blocks == 32, "{costs:?}");
    assert!(costs[1].cost.share(SR) < 0.2 && keeping.master.blocks == 32, "{costs:?}");

    // It goes on: the same thing, counted further, not a new one.
    rig.run(10);
    let short = rig.session.keeping_up().short.expect("still");
    assert_eq!((short.episode, short.late), (1, 22));

    // Asking that took nothing from a tool that measures: it gets every
    // buffer there was since it last asked, and so does the next asker.
    let load = rig.session.take_load();
    assert_eq!((load.callbacks, load.late), (42, 22), "{load:?}");
    let costs = rig.session.take_costs();
    assert_eq!(costs.tracks[0].cost.blocks, 42);
    assert_eq!(rig.session.take_load().callbacks, 0);
    assert_eq!(rig.session.keeping_up().load.callbacks, 42);

    // Another song is another matter: nothing is said of it yet.
    rig.probes.slow_ns.store(0, std::sync::atomic::Ordering::Relaxed);
    rig.session.new_project();
    rig.run(5);
    let keeping = rig.session.keeping_up();
    assert!(keeping.short.is_none() && keeping.load.late == 0 && keeping.tracks.is_empty(), "{keeping:?}");
}

#[test]
fn a_device_that_fails_on_another_thread_costs_a_block_of_silence_and_the_song_plays_on() {
    let (mut rig, probes) = band(4);
    rig.session.play();
    rig.run(20);
    rig.probes.take();
    let _ = rig.host.take_output();
    assert_eq!(rig.session.audio_faults(), 0);
    // A key that the probe panics over, on whichever thread makes its
    // track when it arrives.
    rig.probes.fail.store(true, std::sync::atomic::Ordering::Relaxed);
    for round in 0..6 {
        rig.session.note_on(probes[round % 3], PANIC_KEY, 100);
        rig.run(3);
    }
    // Each cost the one buffer it was played in, and no more: the key is
    // not played to the device again with the next.
    let faults = rig.session.audio_faults();
    assert_eq!(faults, 6, "{faults} buffers were lost");
    // The song goes on, on its threads, and the instruments are told
    // their notes as before.
    rig.probes.take();
    rig.run(100);
    assert_eq!(rig.session.audio_faults(), faults, "nothing more was lost");
    assert!(told(&rig).len() >= 3, "the song plays on");
    let load = rig.session.take_load();
    assert!(load.threads == 4 && load.spread_blocks > 100, "{load:?}");
}
