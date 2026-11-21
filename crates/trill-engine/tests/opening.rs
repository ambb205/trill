//! A song that is opened is there at once, and its devices come after:
//! what holds while they are still arriving.
//!
//! The devices here are the scripted ones, told to take a while to make
//! as a plugin does. A song of three of them and two built-in devices is
//! written by a session that makes everything where it stands, and opened
//! by one whose devices take their time: the song has to be there before
//! any of them, they have to come in the order of the song, and a save, an
//! edit, another song, a render or a device that fails in the middle has
//! to come out as if nothing were on its way.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::{ChainRef, DeviceId, Note, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::probe::{ProbeNote, ProbeOptions};
use trill_engine::render::RenderOptions;
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, ScriptedFactory};
use trill_engine::{Arriving, AudioBackend, ProbeTarget, Session};

const SR: u32 = 48_000;
/// How long each scripted device takes to make.
const MAKING: Duration = Duration::from_millis(120);
/// Longer than any test's devices take to arrive.
const PATIENCE: Duration = Duration::from_secs(20);

fn slow() -> ScriptedFactory {
    ScriptedFactory::loading(Loading { making: MAKING, ..Loading::default() })
}

fn factory(scripted: &ScriptedFactory) -> Arc<dyn DeviceFactory> {
    Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())]))
}

fn session(scripted: &ScriptedFactory) -> Session {
    Session::new(factory(scripted), AudioBackend::Null { sample_rate: SR }).unwrap()
}

/// The devices of the song, in its order.
#[derive(Copy, Clone, Debug)]
struct Song {
    bass: DeviceId,
    amp: DeviceId,
    drums: DeviceId,
    synth: DeviceId,
    reverb: DeviceId,
    bass_track: TrackId,
    keys_track: TrackId,
}

/// A bass through an amplifier, drums, and a built-in synth through a
/// reverb, each with notes, and the limiter a song has on its master: three
/// devices that take time to make, and three that take none. The bass is
/// on a preset of its own, which is its state.
fn build(session: &mut Session) -> Song {
    let track = |session: &mut Session, name: &str, pitch: u8| {
        session
            .edit(|project| {
                let track = project.add_track(name);
                let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
                let notes: Vec<Note> = (0..4u8)
                    .map(|beat| Note {
                        pitch,
                        start: f64::from(beat),
                        length: 0.5,
                        velocity: 100,
                        ..Default::default()
                    })
                    .collect();
                project.clip_mut(clip)?.add_notes(&notes)?;
                Ok(track)
            })
            .unwrap()
    };
    let bass_track = track(session, "Bass", 40);
    let bass = session.add_device(ChainRef::Track(bass_track), BASS, Some("soft"), None).unwrap();
    let amp = session.add_device(ChainRef::Track(bass_track), AMP, None, None).unwrap();
    let drums_track = track(session, "Drums", 36);
    let drums = session.add_device(ChainRef::Track(drums_track), DRUMS, None, None).unwrap();
    let keys_track = track(session, "Keys", 60);
    let synth = session.add_device(ChainRef::Track(keys_track), "builtin:polysynth", None, None).unwrap();
    let reverb = session.add_device(ChainRef::Track(keys_track), "builtin:reverb", None, None).unwrap();
    Song { bass, amp, drums, synth, reverb, bass_track, keys_track }
}

/// The song in a file of a folder of the test's own, written by a session
/// whose devices are there as soon as they are asked for.
fn written(name: &str) -> (PathBuf, Song) {
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR")).join("opening").join(name);
    let _ = std::fs::remove_dir_all(&folder);
    let mut session = session(&ScriptedFactory::new());
    let song = build(&mut session);
    let path = folder.join("song.trill");
    session.save(&path).unwrap();
    (path, song)
}

/// What a project file says of its song: the project, without where its
/// files were when it was saved.
fn project_of(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    file.get("project").cloned().unwrap_or(file)
}

#[test]
fn a_song_is_there_as_soon_as_it_is_opened_and_its_devices_come_after_in_its_order() {
    let (path, song) = written("in order");
    let scripted = slow();
    let mut session = session(&scripted);
    let before = session.revision();
    let began = Instant::now();
    session.load(&path).unwrap();
    assert!(began.elapsed() < MAKING, "opening the song waited for a device: {:?}", began.elapsed());

    // The song is there: its tracks, its notes, and the devices that are
    // made in no time. The others have their places kept.
    let project = session.project().clone();
    assert_eq!(project.tracks.iter().map(|track| track.name.as_str()).collect::<Vec<_>>(), ["Bass", "Drums", "Keys"]);
    assert_eq!(project.devices().count(), 6);
    assert!(!session.device_params(song.synth).unwrap().is_empty() && session.device_params(song.reverb).is_ok());
    let unknown = session.device_params(song.bass).unwrap_err().to_string();
    assert!(unknown.contains("is still being made"), "{unknown}");
    assert_eq!(session.loading(), [song.bass, song.amp, song.drums], "on their way, in the order of the song");
    let arriving = session.arriving().expect("devices are on their way");
    assert_eq!((arriving.made, arriving.of), (3, 6));
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert!(scripted.made().is_empty(), "none of them was made where the song was opened");
    // Opening is not an edit, and neither is what arrives.
    assert!(!session.is_modified() && !session.can_undo());
    assert!(session.revision() > before);

    // They come one by one, in the order of the song, and each is said
    // while it is being made.
    let mut seen: Vec<Arriving> = vec![arriving];
    let deadline = Instant::now() + PATIENCE;
    while session.arriving().is_some() && Instant::now() < deadline {
        session.housekeeping();
        if let Some(arriving) = session.arriving()
            && seen.last() != Some(&arriving)
        {
            seen.push(arriving);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(session.arriving().is_none(), "they never all arrived: {seen:?}");
    let counts: Vec<usize> = seen.iter().map(|arriving| arriving.made).collect();
    let mut rising = counts.clone();
    rising.dedup();
    assert_eq!(rising, [3, 4, 5], "{seen:?}");
    let named: Vec<DeviceId> = seen.iter().filter_map(|arriving| arriving.now).collect();
    let mut in_order = named.clone();
    in_order.dedup();
    assert_eq!(in_order, [song.bass, song.amp, song.drums], "{seen:?}");
    let words = Arriving { made: 4, of: 6, now: Some(song.amp), coming: vec![song.amp] }.words(session.project());
    assert_eq!(words, format!("4 of 6 devices: {} on Bass", project.device(song.amp).unwrap().name));

    // All there: each is what the song says it is, and what each took is
    // known.
    assert!(session.problems().is_empty());
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS, AMP, DRUMS]);
    assert_eq!(session.device_preset(song.bass).map(|preset| preset.id.as_str()), Some("soft"));
    let took = session.made_since_opening();
    assert_eq!(took.iter().map(|made| made.device).collect::<Vec<_>>(), [song.bass, song.amp, song.drums]);
    assert!(took.iter().all(|made| made.took >= MAKING && made.failed.is_none()), "{took:?}");
    assert!(!session.is_modified() && !session.can_undo(), "what arrived is no edit");
    // And it plays: the bass is heard on its track.
    session.play();
    let deadline = Instant::now() + PATIENCE;
    let meter = session.track_meter(song.bass_track).expect("the track has a meter");
    while meter.peek().0 <= 0.0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(meter.peek().0 > 0.0, "the bass that arrived is not heard");
}

#[test]
fn a_song_saved_while_its_devices_are_on_their_way_is_the_whole_song() {
    let (path, song) = written("saved");
    let original = project_of(&path);
    assert!(!original["tracks"][0]["devices"][0]["state"].is_null(), "the bass has a state: {original}");

    let mut session = session(&slow());
    session.load(&path).unwrap();
    assert_eq!(session.loading().len(), 3);
    // The safety copy that is taken every so often, and a save.
    let copy: serde_json::Value = serde_json::from_str(&session.snapshot_json()).unwrap();
    assert_eq!(copy.get("project").cloned().unwrap_or(copy), original, "the safety copy lost something");
    let elsewhere = path.with_file_name("saved in the middle.trill");
    session.save(&elsewhere).unwrap();
    assert_eq!(project_of(&elsewhere), original, "a song saved in the middle is not the song");
    assert_eq!(session.loading().len(), 3, "and nothing had arrived by then");

    // And it is again when they are all there.
    assert!(session.wait_until_opened(PATIENCE));
    let later = path.with_file_name("saved after.trill");
    session.save(&later).unwrap();
    assert_eq!(project_of(&later), original);
    assert_eq!(session.device_preset(song.bass).map(|preset| preset.id.as_str()), Some("soft"));
}

#[test]
fn edits_while_devices_are_on_their_way_land_as_they_would_on_a_song_that_is_whole() {
    let (path, song) = written("edited");
    let scripted = slow();
    let mut session = session(&scripted);
    session.load(&path).unwrap();

    // A note, and its undo: the song's own business.
    let clip = session.project().track(song.keys_track).unwrap().clips[0].id;
    session
        .edit(|project| {
            project.clip_mut(clip)?.add_notes(&[Note {
                pitch: 72,
                start: 2.0,
                length: 1.0,
                velocity: 90,
                ..Default::default()
            }])
        })
        .unwrap();
    assert!(session.is_modified() && session.can_undo());
    assert!(session.undo());
    // A device that is there takes a value; one that is not says so, and
    // is not changed.
    session.set_device_params(song.reverb, &[("mix".to_string(), 0.5)]).unwrap();
    let refused = session.set_device_params(song.amp, &[("gain".to_string(), 1.0)]).unwrap_err().to_string();
    assert!(refused.contains("is still being made") && refused.contains("Ask again when it is there"), "{refused}");
    // A preset of its own, given to one that is not there to load it: the
    // song says what it is to start from, and it arrives as that preset,
    // whatever it was being made as.
    session.give_preset(song.bass, "up").unwrap();
    let bass = session.project().device(song.bass).unwrap().clone();
    assert_eq!(bass.from_preset.as_ref().map(|preset| preset.id.as_str()), Some("up"));
    assert_eq!((bass.preset.map(|preset| preset.id), bass.state), (Some("up".to_string()), None));
    // One that is on its way is removed, and one is added: it comes after
    // the song has it, like the others.
    session.edit(|project| project.remove_device(song.drums).map(|_| ())).unwrap();
    let added = session.add_device(ChainRef::Track(song.keys_track), DRUMS, None, None).unwrap();
    assert!(session.is_coming(added) && !session.loading().contains(&song.drums));
    assert_eq!(session.arriving().map(|arriving| (arriving.of, arriving.coming.len())), Some((6, 3)));

    assert!(session.wait_until_opened(PATIENCE));
    // The three that were waited for arrived; the one that was removed
    // did not come back with them, and was not made for nothing.
    assert!(session.project().device(song.drums).is_err());
    let mut made: Vec<DeviceId> = session.made_since_opening().iter().map(|made| made.device).collect();
    made.sort();
    let mut waited = [song.bass, song.amp, added];
    waited.sort();
    assert_eq!(made, waited);
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    // The bass is on the preset it was given, and the song has the state
    // it came to with it in place of the preset to load: the state a bass
    // has that was given the preset where it stood.
    let state_of_up = {
        let mut session = crate::session(&ScriptedFactory::new());
        let track = session.edit(|project| Ok(project.add_track("Bass"))).unwrap();
        let bass = session.add_device(ChainRef::Track(track), BASS, Some("up"), None).unwrap();
        let file = Path::new(env!("CARGO_TARGET_TMPDIR")).join("opening").join("state of up.trill");
        session.save(&file).unwrap();
        session.project().device(bass).unwrap().state.clone()
    };
    assert!(state_of_up.is_some());
    session.snapshot_json();
    let bass = session.project().device(song.bass).unwrap().clone();
    assert_eq!((bass.from_preset, bass.preset.map(|preset| preset.id)), (None, Some("up".to_string())));
    assert_eq!(bass.state, state_of_up, "the bass did not arrive as the preset it was given");
    let story = scripted.story();
    assert_eq!(story.iter().filter(|line| **line == format!("made {DRUMS}")).count(), 1, "{story:?}");
    // Undo takes the edits back one by one, and none of them was a device
    // arriving: the removed drums are in the song again, and come after.
    let mut steps = 0;
    while session.undo() {
        steps += 1;
    }
    assert_eq!(steps, 4, "the added drums, the removed drums, the preset and the reverb's value");
    assert!(session.project().device(song.drums).is_ok() && session.project().device(added).is_err());
    assert_eq!(session.device_preset(song.bass).map(|preset| preset.id.as_str()), Some("soft"));
    assert!(session.wait_until_opened(PATIENCE));
    assert!(session.arriving().is_none() && session.problems().is_empty());
    assert!(!session.is_coming(song.drums) && !session.is_coming(song.bass));
}

#[test]
fn another_song_or_a_new_one_in_the_middle_gets_nothing_of_the_first() {
    let (first, _) = written("first");
    let (second, song) = written("second");
    let scripted = slow();
    let mut session = session(&scripted);
    session.load(&first).unwrap();
    // Until the first device of the first song is being made.
    let deadline = Instant::now() + PATIENCE;
    while session.arriving().is_some_and(|arriving| arriving.now.is_none()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    session.load(&second).unwrap();
    assert_eq!(session.arriving().map(|arriving| (arriving.made, arriving.of)), Some((3, 6)));
    assert!(session.wait_until_opened(PATIENCE));
    // Three devices arrived, the second song's; the one of the first that
    // was being made was let go of, and the two behind it never made.
    assert_eq!(session.made_since_opening().len(), 3);
    assert_eq!(session.made_since_opening()[0].device, song.bass);
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS, BASS, AMP, DRUMS], "{made:?}");
    assert!(session.problems().is_empty());

    // A new project in the middle of a third: nothing arrives in it.
    session.load(&first).unwrap();
    session.new_project();
    assert!(session.arriving().is_none());
    assert!(session.wait_until_opened(Duration::from_secs(1)));
    std::thread::sleep(MAKING * 2);
    session.housekeeping();
    assert_eq!(session.project().devices().count(), 1, "the limiter a new song has");
    assert!(session.made_since_opening().is_empty() && session.problems().is_empty());
    // And a session that is let go of in the middle of a song ends.
    session.load(&second).unwrap();
    drop(session);
    let story = scripted.story();
    let made = story.iter().filter(|line| line.starts_with("made")).count();
    let gone = story.iter().filter(|line| line.starts_with("gone")).count();
    assert_eq!(made, gone, "every instrument that was made was let go of: {story:?}");
}

#[test]
fn what_was_open_before_is_taken_down_after_the_new_song_has_its_devices() {
    let (first, _) = written("before");
    let (second, _) = written("after");
    let scripted = slow();
    let mut session = session(&scripted);
    session.load(&first).unwrap();
    assert!(session.wait_until_opened(PATIENCE));
    assert_eq!(scripted.story(), [format!("made {BASS}"), format!("made {DRUMS}")]);
    session.load(&second).unwrap();
    assert!(session.wait_until_opened(PATIENCE));
    // Until the first song's instruments have gone: for a moment after the
    // second song is whole, at the most.
    let deadline = Instant::now() + PATIENCE;
    while scripted.story().len() < 6 && Instant::now() < deadline {
        session.housekeeping();
        std::thread::sleep(Duration::from_millis(5));
    }
    let story = scripted.story();
    assert_eq!(story.len(), 6, "{story:?}");
    assert_eq!(story[2..4], [format!("made {BASS}"), format!("made {DRUMS}")], "the new song first: {story:?}");
    assert!(story[4..].iter().all(|line| line.starts_with("gone")), "the taking down after: {story:?}");
}

#[test]
fn a_place_that_is_kept_is_not_given_the_device_the_song_before_had_under_its_number() {
    // The same song twice: every device of the second has the number of
    // one of the first, which is whole and could play.
    let (first, song) = written("its number");
    let scripted = ScriptedFactory::loading(Loading { making: Duration::from_millis(1200), ..Loading::default() });
    let mut session = session(&scripted);
    session.load(&first).unwrap();
    assert!(session.wait_until_opened(PATIENCE));
    session.load(&first).unwrap();
    assert_eq!(session.loading()[..3], [song.bass, song.amp, song.drums]);

    // Played now, the bass's track says nothing, because its instrument
    // is not there: the bass of the song before, which has its number, is
    // not playing its notes meanwhile. The synth, which was made with the
    // song, sounds.
    let (bass, keys) = (session.track_meter(song.bass_track).unwrap(), session.track_meter(song.keys_track).unwrap());
    bass.take();
    keys.take();
    session.play();
    let began = Instant::now();
    while keys.peek().0 == 0.0 && began.elapsed() < Duration::from_millis(900) {
        std::thread::sleep(Duration::from_millis(5));
    }
    session.stop();
    assert!(session.is_coming(song.bass), "the bass arrived while this was listening: nothing was shown");
    assert!(keys.take().0 > 0.0, "the song did not play at all");
    assert_eq!(bass.take(), (0.0, 0.0), "the song before's bass played this song's notes");
    assert!(session.wait_until_opened(PATIENCE));
}

#[test]
fn a_render_a_measurement_and_a_probe_in_the_middle_have_the_whole_song() {
    let (path, song) = written("rendered");
    // What the song renders to when everything is there.
    let options = RenderOptions { sample_rate: SR, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let whole = {
        let mut session = session(&ScriptedFactory::new());
        session.load(&path).unwrap();
        session.render_job().render(&options).unwrap()
    };
    assert!(whole.left.iter().any(|sample| sample.abs() > 0.05), "the song sounds");

    let mut session = session(&slow());
    session.load(&path).unwrap();
    assert_eq!(session.loading().len(), 3);
    let job = session.render_job();
    let in_the_middle = job.render(&options).unwrap();
    assert!(in_the_middle.left == whole.left && in_the_middle.right == whole.right, "another song was rendered");
    let measured = session.render_job().measure(&options).unwrap();
    assert!(measured.stats.peak_db > -40.0, "{:?}", measured.stats);
    // A probe of the bass, which is not there yet: of an instrument made
    // for the probe from what the song says of it.
    let probe = session.probe_job(&ProbeTarget::Track { track: song.bass_track, effects: false }).unwrap();
    let note = ProbeNote { pitch: 40, velocity: 100, with: Vec::new(), ..Default::default() };
    let heard = probe.run(&[note], &ProbeOptions { sample_rate: SR, ..ProbeOptions::default() }).unwrap();
    assert!(heard.heard[0].sounded, "{:?}", heard.heard);
    assert!(session.wait_until_opened(PATIENCE));
    assert!(!session.is_modified());
}

#[test]
fn a_device_that_cannot_be_made_is_a_sentence_and_the_song_is_there() {
    let (path, song) = written("failing");
    let scripted = ScriptedFactory::loading(Loading { making: MAKING, fails: true, ..Loading::default() });
    let mut session = session(&scripted);
    session.load(&path).unwrap();
    assert!(session.problems().is_empty(), "nothing has failed yet");
    assert!(session.wait_until_opened(PATIENCE));
    // The amplifier could not be made: said, with its reason, as of any
    // device that cannot be made. The others are there, and the song is.
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("told that it cannot be made"), "{problems:?}");
    let made = session.made_since_opening();
    assert_eq!(
        made.iter().filter(|made| made.failed.is_some()).map(|made| made.device).collect::<Vec<_>>(),
        [song.amp]
    );
    assert!(session.arriving().is_none());
    assert_eq!(session.project().devices().count(), 6);
    assert_eq!(session.device_preset(song.bass).map(|preset| preset.id.as_str()), Some("soft"));
    assert!(!session.is_modified());
}

#[test]
fn opening_a_song_and_its_devices_arriving_are_no_steps_of_its_story() {
    let (path, _) = written("story");
    let mut session = session(&slow());
    session.load(&path).unwrap();
    let before = session.journal().map(|journal| journal.entries().len());
    assert!(session.wait_until_opened(PATIENCE));
    session.housekeeping();
    assert_eq!(session.journal().map(|journal| journal.entries().len()), before, "a device that arrived was entered");
    assert!(!session.can_undo() && !session.is_modified());
}

#[test]
fn a_song_brought_back_from_a_safety_copy_is_there_at_once_too() {
    let (copy, song) = written("recovered");
    let scripted = slow();
    let mut session = session(&scripted);
    let began = Instant::now();
    session.load_recovered(&copy, None).unwrap();
    assert!(began.elapsed() < MAKING, "bringing the song back waited for a device: {:?}", began.elapsed());
    assert_eq!(session.loading(), [song.bass, song.amp, song.drums]);
    assert!(session.is_modified(), "what was brought back is not saved yet");

    assert!(session.wait_until_opened(PATIENCE), "the devices never arrived");
    assert!(session.arriving().is_none() && session.problems().is_empty(), "{:?}", session.problems());
    // Still to be saved, as it was: the devices arriving changed nothing.
    assert!(session.is_modified() && !session.can_undo());
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS, AMP, DRUMS]);
}
