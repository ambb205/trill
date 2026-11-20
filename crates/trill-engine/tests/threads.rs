//! A render on more than one thread: the same samples as on one, in the
//! mix and in every file it writes; devices that keep something between
//! them run in turn; and a device that is not to be run beside anything
//! leaves the render on one thread, which the report says.
//!
//! The song here is made to have every way one track can wait for
//! another: two groups, one inside the other's send; three returns, one
//! fed by a group and one by another return's neighbours; a compressor
//! that listens to another track; a master that listens to one; faders,
//! pans, sends and a device's parameter that move.

use std::path::Path;
use std::sync::Arc;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::project::{AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderJob, RenderOptions, Rendered, Stems, usual_threads};
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, ScriptedFactory};
use trill_engine::sources::AudioSources;

const SR: u32 = 48_000;

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100, ..Default::default() }
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

/// Sixteen beats of a line that keeps an instrument busy: a note every
/// half beat, on pitches that go round `pitches`.
fn busy(pitches: &[u8]) -> Vec<Note> {
    (0..32).map(|step| note(pitches[step % pitches.len()], step as f64 * 0.5, 0.4)).collect()
}

fn song() -> Project {
    let mut project = Project::new("threads");
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
    // A send that is its envelope alone, thrown open half way.
    let throw = AutomationTarget::Send { track: pad, to: crush };
    project.set_automation(throw, points(&[(0.0, -70.0), (7.0, -70.0), (9.0, -8.0), (16.0, -70.0)])).unwrap();

    let bell = part(&mut project, "Bell", "builtin:polysynth", &busy(&[84, 79, 76]));
    project.track_mut(bell).unwrap().volume_db = -9.0;
    project.set_send(bell, echo, Some(-6.0)).unwrap();
    let second = part(&mut project, "Second bass", BASS, &busy(&[52, 47, 45, 47]));
    project.set_send(second, crush, Some(-12.0)).unwrap();
    project.track_mut(second).unwrap().pan = 0.4;

    // A group sends too, and the master ducks under the drums.
    project.set_send(keys, room, Some(-10.0)).unwrap();
    project.set_send(band, crush, Some(-18.0)).unwrap();
    device(&mut project, ChainRef::Master, "builtin:eq", &[]);
    let pump = device(&mut project, ChainRef::Master, "builtin:compressor", &[("threshold", -24.0)]);
    project.set_sidechain(pump, Some(drums)).unwrap();
    device(&mut project, ChainRef::Master, "builtin:limiter", &[("gain", 3.0)]);
    project
}

fn factory(scripted: &ScriptedFactory) -> Arc<dyn DeviceFactory> {
    Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())]))
}

fn render(project: &Project, scripted: &ScriptedFactory, options: &RenderOptions) -> Rendered {
    let job =
        RenderJob { project: project.clone(), factory: factory(scripted), audio: AudioSources::default(), apart: None };
    job.render(options).unwrap()
}

fn options(threads: Option<usize>) -> RenderOptions {
    RenderOptions { sample_rate: SR, max_tail_seconds: 3.0, threads, ..RenderOptions::default() }
}

/// How many threads a render takes on this machine when it is not told:
/// half its processors, eight at the most, or what the environment says.
fn processors() -> usize {
    usual_threads()
}

#[test]
fn a_song_renders_to_the_same_samples_on_one_thread_and_on_many() {
    let project = song();
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let with_stems = |threads: Option<usize>, name: &str| RenderOptions {
        stems: Some(Stems::all(folder.path().join(name))),
        ..options(threads)
    };
    let one = render(&project, &scripted, &with_stems(Some(1), "one"));
    assert_eq!(one.measured.threads, 1);
    assert!(one.left.iter().any(|sample| sample.abs() > 0.05), "the song sounds");
    assert_eq!(one.measured.stems.len(), 6 + 6 + 2 + 3 + 1, "every track twice, groups, returns, the premaster");

    for (threads, name) in [(None, "all"), (Some(2), "two"), (Some(3), "three"), (Some(64), "many")] {
        let many = render(&project, &scripted, &with_stems(threads, name));
        if processors() > 1 {
            assert!(many.measured.threads > 1, "{name}: it used {} thread(s)", many.measured.threads);
        }
        assert!(many.measured.threads <= threads.unwrap_or_else(usual_threads));
        // Not close: the same.
        assert!(many.left == one.left && many.right == one.right, "{name}: the mix is other samples");
        assert_eq!(many.track_peaks, one.track_peaks, "{name}");
        for (stem, single) in many.measured.stems.iter().zip(&one.measured.stems) {
            let (written, expected) = (std::fs::read(&stem.path).unwrap(), std::fs::read(&single.path).unwrap());
            assert!(written == expected, "{name}: {} is other samples than {}", stem.path, single.path);
            assert_eq!((stem.what, stem.track), (single.what, single.track));
            assert_eq!(stem.stats.loudness_lufs.to_bits(), single.stats.loudness_lufs.to_bits(), "{}", stem.path);
        }
        let rows = |rendered: &Rendered| -> Vec<(TrackId, u32, u32)> {
            let row = |row: &trill_engine::render::TrackLevel| {
                (row.track, row.peak_db.to_bits(), row.loudness_lufs.to_bits())
            };
            rendered.measured.tracks.iter().map(row).collect()
        };
        assert_eq!(rows(&many), rows(&one), "{name}: what was measured of each track");
        assert_eq!(many.measured.stats.loudness_lufs.to_bits(), one.measured.stats.loudness_lufs.to_bits());
        assert_eq!(many.measured.dynamics.len(), one.measured.dynamics.len());
        for (taken, single) in many.measured.dynamics.iter().zip(&one.measured.dynamics) {
            assert_eq!((taken.device, taken.most_db.to_bits()), (single.device, single.most_db.to_bits()), "{name}");
        }
    }
    assert!(Path::new(&one.measured.stems[0].path).exists());
}

#[test]
fn a_render_takes_half_the_machine_at_the_most_unless_it_is_told() {
    // (The environment may say another number for the whole program,
    // which is then what is taken; the tests here read it the same way.)
    let machine = std::thread::available_parallelism().map_or(1, usize::from);
    if std::env::var_os("TRILL_RENDER_THREADS").is_none() {
        assert_eq!(usual_threads(), (machine / 2).clamp(1, 8));
    }
    // The song has five tracks that wait for nothing (the bass listens
    // to the drums): told nothing, a render takes no more threads than
    // that, and no more than usual.
    let rendered = render(&song(), &ScriptedFactory::new(), &options(None));
    assert_eq!(rendered.measured.threads, usual_threads().min(5));
    // Told, it takes what it is told, up to what the song can use.
    let told = render(&song(), &ScriptedFactory::new(), &options(Some(64)));
    assert_eq!(told.measured.threads, 5);
    assert_eq!(render(&song(), &ScriptedFactory::new(), &options(Some(3))).measured.threads, 3);
}

#[test]
fn a_song_whose_tempo_and_time_signature_change_is_the_same_samples_on_many_threads() {
    // A jump, a glide that arrives and a tempo that takes over, and a bar
    // of seven: blocks end where the tempo and the meter change, on every
    // thread alike.
    let mut project = song();
    project.set_tempo_at(3.0, 96.0, false).unwrap();
    project.set_tempo_at(6.0, 96.0, false).unwrap();
    project.set_tempo_at(10.0, 168.0, true).unwrap();
    project.set_tempo_at(10.0, 120.0, false).unwrap();
    project.set_time_signature_at(8.0, 7, 8).unwrap();
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let with_stems = |threads: Option<usize>, name: &str| RenderOptions {
        stems: Some(Stems::all(folder.path().join(name))),
        ..options(threads)
    };
    let one = render(&project, &scripted, &with_stems(Some(1), "one"));
    let plain = render(&song(), &scripted, &options(Some(1)));
    assert!(one.left.len() != plain.left.len(), "the changes of tempo are heard: the song is another length");
    let many = render(&project, &scripted, &with_stems(Some(6), "many"));
    assert!(many.left == one.left && many.right == one.right);
    for (stem, single) in many.measured.stems.iter().zip(&one.measured.stems) {
        let (written, expected) = (std::fs::read(&stem.path).unwrap(), std::fs::read(&single.path).unwrap());
        assert!(written == expected, "{} is other samples than {}", stem.path, single.path);
    }
}

#[test]
fn a_part_of_the_song_and_a_song_heard_in_part_are_the_same_samples_on_many_threads() {
    // From the middle, to an end inside a note, and with tracks left out
    // by a solo and a mute: what is made is another graph each time.
    let mut project = song();
    let scripted = ScriptedFactory::new();
    let from_the_middle = |threads| RenderOptions { start_beat: 5.25, end_beat: Some(11.1), ..options(threads) };
    let one = render(&project, &scripted, &from_the_middle(Some(1)));
    let many = render(&project, &scripted, &from_the_middle(None));
    assert!(many.left == one.left && many.right == one.right);

    let ids: Vec<TrackId> = project.tracks.iter().map(|track| track.id).collect();
    for (soloed, muted) in [(vec![5usize], vec![]), (vec![7, 9], vec![6]), (vec![0], vec![]), (vec![], vec![3, 8])] {
        for track in &mut project.tracks {
            (track.solo, track.mute) = (false, false);
        }
        for at in soloed.iter() {
            project.tracks[*at].solo = true;
        }
        for at in muted.iter() {
            project.tracks[*at].mute = true;
        }
        let one = render(&project, &scripted, &options(Some(1)));
        let many = render(&project, &scripted, &options(None));
        let heard = |at: &usize| project.track(ids[*at]).unwrap().name.clone();
        let names: (Vec<String>, Vec<String>) = (soloed.iter().map(heard).collect(), muted.iter().map(heard).collect());
        assert!(many.left == one.left && many.right == one.right, "soloed {:?}, muted {:?}", names.0, names.1);
        assert_eq!(many.track_peaks, one.track_peaks);
    }
}

#[test]
fn instruments_that_keep_something_between_them_are_run_in_turn_and_in_the_order_of_the_song() {
    // Every scripted instrument of this factory counts the notes that any
    // of them has sounded, and a note's level is what the count says: what
    // comes out depends on the order they are run in, block by block.
    //
    // Only the devices that take turns wait for each other. What comes
    // after the last of them on a track is run beside the other tracks,
    // and the track's block is made in two parts: here with the part that
    // takes turns ending at the instrument (the second bass, before its
    // EQ), after it (the bass, whose amplifier takes turns too, before its
    // compressor), with nothing after it (the drums), and on a track whose
    // instrument is built in and whose amplifier takes turns (the bell).
    // What goes into each track's effects is written to a file where it
    // goes by, which is in the one part or the other.
    let mut project = song();
    let track = |name: &str| project.tracks.iter().find(|track| track.name == name).unwrap().id;
    let (bell, second) = (track("Bell"), track("Second bass"));
    device(&mut project, ChainRef::Track(bell), AMP, &[]);
    device(&mut project, ChainRef::Track(second), "builtin:eq", &[]);
    let folder = tempfile::tempdir().unwrap();
    let with_stems = |threads: Option<usize>, name: &str| RenderOptions {
        stems: Some(Stems::all(folder.path().join(name))),
        ..options(threads)
    };
    let sharing = Loading { shares: true, ..Loading::default() };
    let scripted = ScriptedFactory::loading(sharing);
    let one = render(&project, &scripted, &with_stems(Some(1), "one"));
    assert!(!scripted.overlapped());

    let scripted = ScriptedFactory::loading(sharing);
    let many = render(&project, &scripted, &with_stems(None, "many"));
    assert!(many.left == one.left && many.right == one.right, "the order of one thread was kept");
    assert!(!scripted.overlapped(), "and no two of them were run at once");
    if processors() > 1 {
        assert!(many.measured.threads > 1, "though the rest of the song was made beside them");
    }
    assert_eq!(many.measured.stems.len(), one.measured.stems.len());
    for (stem, single) in many.measured.stems.iter().zip(&one.measured.stems) {
        let (written, expected) = (std::fs::read(&stem.path).unwrap(), std::fs::read(&single.path).unwrap());
        assert!(written == expected, "{} is other samples than {}", stem.path, single.path);
    }

    // What that is worth: taken on trust, the same instruments are run at
    // once, which they notice.
    if processors() > 1 {
        let scripted = ScriptedFactory::loading(sharing);
        let trusting = RenderOptions { trusting: true, ..options(None) };
        // (Whether they are caught at it is a matter of timing: a few
        // renders are as good as certain to.)
        let caught = (0..20).any(|_| {
            render(&project, &scripted, &trusting);
            scripted.overlapped()
        });
        assert!(caught, "instruments that were trusted were never run at once");
    }
}

#[test]
fn the_plugins_of_a_song_are_rehearsed_at_once_and_begin_the_render_where_one_thread_leaves_them() {
    // Instruments with five recordings of each key, and amplifiers whose
    // level goes up and down with every frame they are run: what the song
    // sounds like says how often the wait before it played each note, and
    // how much it ran each amplifier.
    let project = song();
    let telling = Loading { takes: 5, free: true, ..Loading::default() };
    let plain = render(&project, &ScriptedFactory::new(), &options(Some(1)));
    let one = render(&project, &ScriptedFactory::loading(telling), &options(Some(1)));
    assert!(one.left != plain.left, "the takes and the amplifiers are heard");
    // Nothing is known of them yet on several threads either, and then
    // they are known.
    for round in 0..2 {
        let many = render(&project, &ScriptedFactory::loading(telling), &options(None));
        assert!(many.left == one.left && many.right == one.right, "round {round}");
    }
    // A key of the basses that arrives a second after the rest is waited
    // for on its thread while the others are done with theirs.
    let late = Loading { late: Some((45, std::time::Duration::from_millis(1000))), ..telling };
    let begun = std::time::Instant::now();
    let many = render(&project, &ScriptedFactory::loading(late), &options(None));
    assert!(begun.elapsed() >= std::time::Duration::from_millis(1000));
    assert!(many.left == one.left && many.right == one.right, "after a wait of {:?}", begun.elapsed());
}

#[test]
fn a_device_that_is_not_to_be_run_beside_anything_leaves_the_render_on_one_thread_and_the_report_says_so() {
    let project = song();
    let alone = ScriptedFactory::loading(Loading { alone: true, ..Loading::default() });
    let plain = render(&project, &ScriptedFactory::new(), &options(Some(1)));
    let kept = render(&project, &alone, &options(None));
    assert_eq!(kept.measured.threads, 1);
    assert!(kept.left == plain.left && kept.right == plain.right);
    // Each such device is named, with its track.
    let said: Vec<(&str, &str)> =
        kept.measured.alone.iter().map(|alone| (alone.track_name.as_str(), alone.device_name.as_str())).collect();
    assert_eq!(said, [("Drums", DRUMS), ("Bass", BASS), ("Bass", AMP), ("Second bass", BASS)]);

    // Asked for one thread, there is nothing to say; and nothing where
    // the devices that would say it are not part of what is heard.
    assert!(render(&project, &alone, &options(Some(1))).measured.alone.is_empty());
    let mut without = project.clone();
    for track in without.tracks.iter_mut().filter(|track| ["Drums", "Bass", "Second bass"].contains(&&*track.name)) {
        track.mute = true;
    }
    // (The master listens to the drums, which would have them made
    // though they are muted.)
    without.master.devices.iter_mut().for_each(|device| device.sidechain = None);
    let free = render(&without, &alone, &options(None));
    assert!(free.measured.alone.is_empty(), "{:?}", free.measured.alone);
    if processors() > 1 {
        assert!(free.measured.threads > 1);
    }
}

#[test]
fn a_song_whose_every_track_waits_for_another_is_made_on_one_thread_without_a_word() {
    // One instrument in a group: at no moment are there two things to do.
    let mut project = Project::new("chain");
    let group = project.add_track_of_kind("Group", TrackKind::Group);
    let only = part(&mut project, "Only", "builtin:polysynth", &busy(&[60, 67]));
    project.set_track_group(only, Some(group)).unwrap();
    let rendered = render(&project, &ScriptedFactory::new(), &options(None));
    assert_eq!(rendered.measured.threads, 1);
    assert!(rendered.measured.alone.is_empty());
}
