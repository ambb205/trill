//! A device that takes time to make comes after the song has it, whatever
//! brought it: an edit, an undo, a preset, the audio stream at another
//! rate. And what leaves the song is taken down with nobody waiting.
//!
//! `opening.rs` holds this for a song that is opened; here it is held for
//! everything else, with the scripted devices told to take their time as
//! a plugin does. Where it matters that a plugin is made, asked and let go
//! of on one thread, they are told to have one too.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::presets::PresetInfo;
use trill_core::{ChainRef, DeviceId, Note, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::host::scripted::ScriptedHost;
use trill_engine::render::RenderOptions;
use trill_engine::scripted::{BASS, DRUMS, Loading, ScriptedFactory};
use trill_engine::{AudioBackend, AudioConfig, AudioSetup, Session};
use trill_media::MediaCache;

const SR: u32 = 48_000;
/// How long each scripted device takes to make.
const MAKING: Duration = Duration::from_millis(150);
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

/// A track with four notes of `pitch`.
fn track(session: &mut Session, name: &str, pitch: u8) -> TrackId {
    session
        .edit(|project| {
            let track = project.add_track(name);
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
            let notes: Vec<Note> = (0..4u8)
                .map(|beat| Note { pitch, start: f64::from(beat), length: 0.5, velocity: 100, ..Default::default() })
                .collect();
            project.clip_mut(clip)?.add_notes(&notes)?;
            Ok(track)
        })
        .unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR")).join("coming").join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// What the song has of `device`: the preset it is on, the preset it is to
/// start from, and whether it has a state.
fn standing(session: &Session, device: DeviceId) -> (Option<String>, Option<String>, bool) {
    let device = session.project().device(device).unwrap();
    (
        device.preset.as_ref().map(|preset| preset.id.clone()),
        device.from_preset.as_ref().map(|preset| preset.id.clone()),
        device.state.is_some(),
    )
}

/// How many steps there are to undo, counted by undoing a copy's worth:
/// the steps are undone and done again.
fn steps(session: &mut Session) -> usize {
    let mut count = 0;
    while session.undo() {
        count += 1;
    }
    for _ in 0..count {
        session.redo();
    }
    count
}

/// Does the session's chores until nothing is being made or taken down
/// any more.
fn settle(session: &mut Session) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        session.housekeeping();
        if !session.making_devices() && session.arriving().is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "the session never came to rest");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The state a bass has that was given the preset `preset` where it
/// stood, by a session whose devices are there as soon as they are asked
/// for.
fn state_of(preset: &str) -> Option<Vec<u8>> {
    let mut session = session(&ScriptedFactory::new());
    let track = session.edit(|project| Ok(project.add_track("Bass"))).unwrap();
    let bass = session.add_device(ChainRef::Track(track), BASS, Some(preset), None).unwrap();
    session.save(&scratch(&format!("state of {preset}")).join("song.trill")).unwrap();
    session.project().device(bass).unwrap().state.clone()
}

#[test]
fn an_edit_that_brings_a_device_lands_at_once_and_the_device_comes_after_as_the_preset_it_was_given() {
    let scripted = slow();
    let mut session = session(&scripted);
    let bass_track = track(&mut session, "Bass", 40);
    let entries = session.journal().map(|journal| journal.entries().len());

    let began = Instant::now();
    let bass = session.add_device(ChainRef::Track(bass_track), BASS, Some("up"), None).unwrap();
    assert!(began.elapsed() < MAKING / 2, "the edit waited for the device: {:?}", began.elapsed());
    // The song has it, with what it is to start from, and the history has
    // its step. The device itself is not there.
    assert_eq!(standing(&session, bass), (Some("up".into()), Some("up".into()), false));
    assert!(session.is_coming(bass) && session.can_undo() && session.is_modified());
    assert_eq!(session.loading(), [bass]);
    let arriving = session.arriving().expect("the device is on its way");
    assert_eq!((arriving.made, arriving.of, arriving.coming.clone()), (1, 2, vec![bass]), "the limiter and the bass");
    assert!(scripted.made().is_empty(), "it was made where the edit was");
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert_eq!(session.journal().map(|journal| journal.entries().len()), entries.map(|entries| entries + 1));
    // What cannot be asked of it yet is refused in a sentence.
    let refused = session.set_device_params(bass, &[("level".to_string(), 0.5)]).unwrap_err().to_string();
    assert!(refused.contains("Scripted Bass is still being made") && refused.contains("Ask again"), "{refused}");

    // A second device, with no preset, and a third that is built in: the
    // first two come one after the other, the third is there at once.
    let drums_track = track(&mut session, "Drums", 36);
    let drums = session.add_device(ChainRef::Track(drums_track), DRUMS, None, None).unwrap();
    let reverb = session.add_device(ChainRef::Track(drums_track), "builtin:reverb", None, None).unwrap();
    assert!(session.is_coming(drums) && !session.is_coming(reverb));
    assert!(!session.device_params(reverb).unwrap().is_empty());
    assert_eq!(session.arriving().map(|arriving| arriving.coming), Some(vec![bass, drums]));

    assert!(session.wait_until_opened(PATIENCE), "the devices never arrived");
    assert!(session.arriving().is_none() && session.loading().iter().all(|id| !session.is_coming(*id)));
    let made: Vec<DeviceId> = session.made_since_opening().iter().map(|made| made.device).collect();
    assert_eq!(made, [bass, drums], "in the order they were asked for");
    assert_eq!(scripted.made().len(), 2, "each once: {:?}", scripted.made());
    // Arriving was no step, and entered nothing in the journal: it has
    // the four edits since, and the song is what the edits made it.
    let after = session.journal().map(|journal| journal.entries().len());
    assert_eq!(after, entries.map(|entries| entries + 4), "a device that arrived was entered");
    assert_eq!(standing(&session, bass), (Some("up".into()), Some("up".into()), false));
    // The bass arrived as the preset: when the song takes its plugins'
    // states, as for a save, the state it came to is there in place of
    // the preset to load, and it is the state of that preset.
    session.snapshot_json();
    let up = state_of("up");
    assert!(up.is_some());
    assert_eq!(standing(&session, bass), (Some("up".into()), None, true));
    assert_eq!(session.project().device(bass).unwrap().state, up);
    assert_eq!(steps(&mut session), 5, "two tracks and three devices");
    assert!(session.problems().is_empty(), "{:?}", session.problems());
}

#[test]
fn a_song_saved_or_rendered_before_such_a_device_is_there_has_it_with_its_preset() {
    // What the song renders to with the bass on its preset, made where it
    // stands.
    let options = RenderOptions { sample_rate: SR, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let build = |session: &mut Session| {
        let bass_track = track(session, "Bass", 40);
        session.add_device(ChainRef::Track(bass_track), BASS, Some("up"), None).unwrap()
    };
    let whole = {
        let mut session = session(&ScriptedFactory::new());
        build(&mut session);
        session.render_job().render(&options).unwrap()
    };
    assert!(whole.left.iter().any(|sample| sample.abs() > 0.05), "the song sounds");
    let soft = {
        let mut session = session(&ScriptedFactory::new());
        let bass_track = track(&mut session, "Bass", 40);
        session.add_device(ChainRef::Track(bass_track), BASS, Some("soft"), None).unwrap();
        session.render_job().render(&options).unwrap()
    };
    assert!(soft.left != whole.left, "the presets sound alike: nothing would be shown");

    let scripted = ScriptedFactory::loading(Loading { making: Duration::from_millis(600), ..Loading::default() });
    let mut session = session(&scripted);
    let bass = build(&mut session);
    assert!(session.is_coming(bass));
    // Rendered in the middle: the render makes its own bass, from what the
    // song says, preset and all.
    let in_the_middle = session.render_job().render(&options).unwrap();
    assert!(in_the_middle.left == whole.left && in_the_middle.right == whole.right, "another sound was rendered");
    // Saved in the middle: the file says what the bass is to start from,
    // and whoever opens it has the bass on that preset.
    let file = scratch("saved in the middle").join("song.trill");
    session.save(&file).unwrap();
    assert!(session.is_coming(bass), "the bass arrived before the save: nothing was shown");
    assert_eq!(standing(&session, bass), (Some("up".into()), Some("up".into()), false));
    let mut reader = crate::session(&ScriptedFactory::new());
    reader.load(&file).unwrap();
    let read = reader.project().tracks[0].devices[0].id;
    assert_eq!(standing(&reader, read), (Some("up".into()), Some("up".into()), false));
    let reopened = reader.render_job().render(&options).unwrap();
    assert!(reopened.left == whole.left, "the song that was saved in the middle is another song");
    // Saved again by the reader, whose bass is there, it has the state.
    reader.save(&scratch("saved again").join("song.trill")).unwrap();
    assert_eq!(standing(&reader, read), (Some("up".into()), None, true));
    assert_eq!(reader.project().device(read).unwrap().state, state_of("up"));
    assert!(session.wait_until_opened(PATIENCE));
}

#[test]
fn undo_and_redo_bring_devices_that_come_after_and_let_go_of_those_that_are_no_longer_wanted() {
    let scripted = slow();
    let mut session = session(&scripted);
    let bass_track = track(&mut session, "Bass", 40);
    let bass = session.add_device(ChainRef::Track(bass_track), BASS, Some("up"), None).unwrap();
    // Taken back before it is there: nothing of it arrives, and what was
    // being made is let go of.
    assert!(session.undo());
    assert!(session.project().device(bass).is_err() && session.arriving().is_none());
    settle(&mut session);
    let story = scripted.story();
    let made = story.iter().filter(|line| line.starts_with("made")).count();
    assert_eq!(made, story.len() - made, "what was made for nothing was let go of: {story:?}");
    assert!(scripted.made().len() <= 1, "{:?}", scripted.made());

    // Brought back by redo: in the song at once, and it comes after, as
    // the preset it was added with.
    let began = Instant::now();
    assert!(session.redo());
    assert!(began.elapsed() < MAKING / 2, "the redo waited for the device: {:?}", began.elapsed());
    assert!(session.is_coming(bass));
    assert_eq!(standing(&session, bass), (Some("up".into()), Some("up".into()), false));
    assert!(session.wait_until_opened(PATIENCE));
    assert_eq!(standing(&session, bass), (Some("up".into()), Some("up".into()), false));
    settle(&mut session);
    // (With its state in the song, as after a save.)
    session.snapshot_json();
    assert_eq!(standing(&session, bass), (Some("up".into()), None, true));

    // A track duplicated: its copy of the bass is another device, which
    // comes after as the state the first has. Undone, it goes; the first
    // is not made again for any of it.
    let made_before = scripted.made().len();
    let copy_track = session.edit(|project| project.duplicate_track(bass_track)).unwrap();
    let copy = session.project().track(copy_track).unwrap().devices[0].id;
    assert!(session.is_coming(copy) && !session.is_coming(bass));
    assert!(session.wait_until_opened(PATIENCE));
    assert_eq!(session.project().device(copy).unwrap().state, state_of("up"));
    assert_eq!(scripted.made().len(), made_before + 1);
    assert!(session.undo());
    assert!(session.project().device(copy).is_err() && !session.is_coming(bass));
    // And the undo of the redo, with the device there: it goes, and the
    // history is the two steps it was.
    assert!(session.undo());
    assert!(session.project().device(bass).is_err());
    assert!(session.redo() && session.is_coming(bass));
    assert!(session.wait_until_opened(PATIENCE));
    assert_eq!(session.project().device(bass).unwrap().state, state_of("up"));
    assert!(session.problems().is_empty(), "{:?}", session.problems());
}

#[test]
fn a_preset_that_a_device_does_not_load_when_it_is_made_is_a_sentence_and_the_song_is_there() {
    let scripted = slow();
    let mut session = session(&scripted);
    let bass_track = track(&mut session, "Bass", 40);
    let keys_track = track(&mut session, "Keys", 60);
    session.add_device(ChainRef::Track(keys_track), "builtin:polysynth", None, None).unwrap();
    let bass = session.add_device(ChainRef::Track(bass_track), BASS, None, None).unwrap();
    // The song comes to say that the bass starts from a preset there is
    // none of, as a file may that was written elsewhere.
    let gone = PresetInfo { id: "gone".into(), name: "Gone".into(), ..PresetInfo::default() };
    session
        .edit(|project| {
            project.device_mut(bass)?.from_preset = Some(gone.clone());
            Ok(())
        })
        .unwrap();
    assert!(session.wait_until_opened(PATIENCE));
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("did not load the preset 'Gone' it was to start from"), "{problems:?}");
    // The rest of the song is there, and plays.
    let keys = session.track_meter(keys_track).unwrap();
    keys.take();
    session.play();
    let began = Instant::now();
    while keys.peek().0 == 0.0 && began.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    session.stop();
    assert!(keys.take().0 > 0.0, "the song did not play");
}

#[test]
fn a_stream_at_another_rate_is_there_at_once_and_every_device_is_made_anew_after_it() {
    let scripted = slow();
    let host = ScriptedHost::with_speakers();
    let setup = AudioSetup::on(Arc::new(host.clone()), AudioConfig::default());
    let mut session = Session::with_audio(factory(&scripted), Arc::new(MediaCache::default()), setup).unwrap();
    let run = |host: &ScriptedHost, buffers: usize| {
        for _ in 0..buffers {
            let buffer = host.running().map_or(0, |format| format.buffer_frames as usize);
            host.advance(buffer);
        }
    };
    let bass_track = track(&mut session, "Bass", 40);
    let bass = session.add_device(ChainRef::Track(bass_track), BASS, Some("up"), None).unwrap();
    let drums_track = track(&mut session, "Drums", 36);
    let drums = session.add_device(ChainRef::Track(drums_track), DRUMS, None, None).unwrap();
    assert!(session.wait_until_opened(PATIENCE));
    run(&host, 4);
    session.housekeeping();
    session.play();
    run(&host, 20);
    assert!(session.transport().playing);
    // (With the state of every device in it, as a change of stream takes
    // them.)
    session.snapshot_json();
    let project = session.project().clone();
    assert_eq!(scripted.story(), [format!("made {BASS}"), format!("made {DRUMS}")]);

    let began = Instant::now();
    session.set_audio_config(AudioConfig { sample_rate: Some(44_100), ..AudioConfig::default() });
    assert!(began.elapsed() < MAKING, "the change waited for a device: {:?}", began.elapsed());
    assert_eq!(session.audio_info().sample_rate, 44_100.0);
    // The song is as it was, and stands; its devices are on their way
    // again, at the new rate, and the window is told how far they are.
    assert_eq!(session.project(), &project);
    run(&host, 2);
    assert!(!session.transport().playing, "nothing of the rate before can go on playing");
    assert_eq!(session.arriving().map(|arriving| arriving.coming), Some(vec![bass, drums]));
    assert_eq!(session.loading(), [bass, drums]);
    assert!(session.problems().is_empty(), "{:?}", session.problems());

    assert!(session.wait_until_opened(PATIENCE));
    run(&host, 4);
    // The ones of the rate before are let go of, after their successors
    // are made, with nobody waiting.
    let deadline = Instant::now() + PATIENCE;
    while scripted.story().len() < 6 && Instant::now() < deadline {
        session.housekeeping();
        std::thread::sleep(Duration::from_millis(5));
    }
    let story = scripted.story();
    assert_eq!(story.len(), 6, "{story:?}");
    assert_eq!(story[2..4], [format!("made {BASS}"), format!("made {DRUMS}")], "the new ones first: {story:?}");
    assert!(story[4..].iter().all(|line| line.starts_with("gone")), "the taking down after: {story:?}");
    // The bass is the preset it was, and play is there to be pressed.
    assert_eq!(session.project().device(bass).unwrap().state, state_of("up"));
    assert!(session.can_undo() && session.project() == &project);
    session.play();
    run(&host, 4);
    assert!(session.transport().playing);
}

#[test]
fn what_leaves_the_song_is_taken_down_with_nobody_waiting() {
    // Devices that take a third of a second to let go of, as a drum kit
    // of samples takes a second.
    let leaving = Duration::from_millis(350);
    let scripted =
        ScriptedFactory::loading(Loading { making: Duration::from_millis(1), leaving, ..Loading::default() });
    let mut session = session(&scripted);
    let quick = leaving / 4;
    // Does the session's chores until `done`, and holds every round of
    // them to the time a chore may take.
    let tend = |session: &mut Session, what: &str, done: &dyn Fn(&[String]) -> bool| {
        let deadline = Instant::now() + PATIENCE;
        while !done(&scripted.story()) {
            assert!(Instant::now() < deadline, "{what}: never done ({:?})", scripted.story());
            let began = Instant::now();
            session.housekeeping();
            assert!(began.elapsed() < quick, "{what}: the session's chores waited: {:?}", began.elapsed());
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let gone = |story: &[String], uri: &str| story.iter().filter(|line| **line == format!("gone {uri}")).count();

    let bass_track = track(&mut session, "Bass", 40);
    let bass = session.add_device(ChainRef::Track(bass_track), BASS, None, None).unwrap();
    let drums_track = track(&mut session, "Drums", 36);
    session.add_device(ChainRef::Track(drums_track), DRUMS, None, None).unwrap();
    assert!(session.wait_until_opened(PATIENCE));
    session.play();
    std::thread::sleep(Duration::from_millis(60));

    // A device removed.
    let began = Instant::now();
    session.edit(|project| project.remove_device(bass).map(|_| ())).unwrap();
    assert!(began.elapsed() < quick, "removing the device waited for it to be let go of: {:?}", began.elapsed());
    tend(&mut session, "the bass", &|story| gone(story, BASS) == 1);
    // A track deleted, with its device.
    let began = Instant::now();
    session.edit(|project| project.remove_track(drums_track).map(|_| ())).unwrap();
    assert!(began.elapsed() < quick, "deleting the track waited: {:?}", began.elapsed());
    tend(&mut session, "the drums", &|story| gone(story, DRUMS) == 1);

    // A device given another state: made anew, and its predecessor goes.
    assert!(session.undo() && session.undo());
    assert!(session.wait_until_opened(PATIENCE));
    std::thread::sleep(Duration::from_millis(60));
    let began = Instant::now();
    session.set_device_state(bass, Some(b"up".to_vec())).unwrap();
    assert!(began.elapsed() < quick, "another state waited: {:?}", began.elapsed());
    assert!(session.wait_until_opened(PATIENCE));
    tend(&mut session, "the bass of the state before", &|story| gone(story, BASS) == 2);

    // A new song over this one: both of its devices go.
    std::thread::sleep(Duration::from_millis(60));
    let began = Instant::now();
    session.new_project();
    assert!(began.elapsed() < quick, "the new song waited for the one before: {:?}", began.elapsed());
    tend(&mut session, "the song before", &|story| gone(story, BASS) == 3 && gone(story, DRUMS) == 2);
    let story = scripted.story();
    let made = story.iter().filter(|line| line.starts_with("made")).count();
    assert_eq!(made, story.len() - made, "everything that was made was let go of: {story:?}");
}

#[test]
fn nobody_asks_a_plugin_what_can_wait_while_another_is_being_made_and_a_save_waits_for_one_making() {
    // Seven instruments that are made, asked and let go of on one thread,
    // each made in four tenths of a second.
    let making = Duration::from_millis(400);
    let file = scratch("one thread").join("song.trill");
    {
        let mut session = session(&ScriptedFactory::new());
        for index in 0..7 {
            let on = track(&mut session, &format!("Part {index}"), 40);
            let uri = if index % 2 == 0 { BASS } else { DRUMS };
            session.add_device(ChainRef::Track(on), uri, None, None).unwrap();
        }
        session.save(&file).unwrap();
    }
    let scripted = ScriptedFactory::loading(Loading { making, one_thread: true, ..Loading::default() });
    let mut session = session(&scripted);
    session.load(&file).unwrap();
    // Until three of them are there, and four are still to be made.
    let deadline = Instant::now() + PATIENCE;
    while session.made_since_opening().len() < 3 {
        assert!(Instant::now() < deadline, "the devices never arrived");
        session.housekeeping();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(session.making_devices());
    let there: Vec<DeviceId> = session.made_since_opening().iter().map(|made| made.device).collect();

    // What a window asks with every change to the song: the parameters of
    // the devices it shows. Each of them is answered at once, by what the
    // device said when it arrived, though the thread they would be asked
    // on is making the next for most of a second.
    let asked_before = scripted.longest_wait("values").1;
    let began = Instant::now();
    for _ in 0..40 {
        for device in &there {
            session.device_params(*device).unwrap();
        }
    }
    assert!(began.elapsed() < making / 4, "asking for parameters waited for a making: {:?}", began.elapsed());
    assert_eq!(scripted.longest_wait("values").1, asked_before, "a device was asked while another was being made");

    // A copy of the song, with the state of every device that is there:
    // the three are asked, and all of it waits for the one making that is
    // under way, not for one a device.
    let began = Instant::now();
    let text = session.snapshot_json();
    let took = began.elapsed();
    assert!(text.contains("Part 6"));
    assert!(took < making * 7 / 4, "the copy waited for more than the making in hand: {took:?}");
    let (longest, states) = scripted.longest_wait("state");
    // (A device that arrived within the last half second is not asked:
    // it may not have taken on the state it was made from.)
    assert!(states >= 1 && longest <= making + making / 4, "{states} asked, the longest waited {longest:?}");

    assert!(session.wait_until_opened(PATIENCE));
    // With nothing being made any more, a device is asked again.
    let deadline = Instant::now() + PATIENCE;
    while session.making_devices() && Instant::now() < deadline {
        session.housekeeping();
        std::thread::sleep(Duration::from_millis(5));
    }
    session.device_params(there[0]).unwrap();
    assert!(scripted.longest_wait("values").1 > asked_before);
    assert!(session.problems().is_empty(), "{:?}", session.problems());
}
