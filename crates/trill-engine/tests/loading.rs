//! Plugins that go on loading after they are made, and how that is told:
//! a kit that arrives piece by piece over longer than its answers take to
//! settle, a drum that comes last under the ringing of another, keys that
//! play nothing, and the questions a front end asks: "is this device
//! ready yet", and "which devices are loading".
//!
//! These are in a test program of their own, and run one at a time,
//! because a sign of loading is the whole program growing: a test that
//! grows on purpose would be seen by every other wait in the same
//! program, and how long a wait takes is what some of them measure.

use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::{ChainRef, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderError, RenderJob, RenderOptions, Rendered, Stems, render};
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, ScriptedFactory};
use trill_engine::{AudioBackend, Session};

const SR: u32 = 48_000;

/// Held by each test while it runs.
static ONE_AT_A_TIME: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |peak, sample| peak.max(sample.abs()))
}

fn factory(scripted: &ScriptedFactory) -> Arc<dyn DeviceFactory> {
    Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())]))
}

/// One track with `instrument` on it that plays `notes`: pitch and the
/// beat it starts on, each half a beat long.
fn song(instrument: &str, notes: &[(u8, f64)]) -> Project {
    let mut project = Project::new("song");
    let track = project.add_track("Part");
    project.add_device(ChainRef::Track(track), instrument, "Instrument", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    let notes: Vec<Note> =
        notes.iter().map(|(pitch, start)| Note { pitch: *pitch, start: *start, length: 0.5, velocity: 100 }).collect();
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    project
}

/// A bass line that opens low and goes above middle C half a second in.
fn line() -> Project {
    song(BASS, &[(40, 0.0), (64, 1.0)])
}

fn timed(project: &Project, factory: &ScriptedFactory, options: &RenderOptions) -> (Duration, Rendered) {
    let begun = Instant::now();
    let out = render(project, factory, options).unwrap();
    (begun.elapsed(), out)
}

/// The sign of loading is memory, which only Windows is asked about.
#[cfg(windows)]
#[test]
fn a_kit_whose_last_pieces_come_long_after_the_first_is_waited_for_while_the_program_grows() {
    let _alone = ONE_AT_A_TIME.lock();
    let options = RenderOptions { sample_rate: SR, max_tail_seconds: 1.0, ..RenderOptions::default() };
    let ready = render(&line(), &ScriptedFactory::new(), &options).unwrap();
    let high = (0.5 * f64::from(SR)) as usize..(0.6 * f64::from(SR)) as usize;
    assert!(peak(&ready.left[high.clone()]) > 0.1);

    // The low notes answer after 0.2 s and nothing changes about them
    // from then on; the notes from middle C up arrive 1.5 s after the
    // plugin was made, and all the while the program grows.
    let loading = Loading {
        first: Duration::from_millis(200),
        upper: Duration::from_millis(1500),
        grows: true,
        ..Loading::default()
    };
    assert!(!trill_engine::ready::is_loading(), "nothing is loading yet");
    let (took, pieces) = timed(&line(), &ScriptedFactory::loading(loading), &options);
    assert!(took >= Duration::from_millis(1500), "it was waited for: {took:?}");
    assert_eq!(peak(&pieces.left[high.clone()]), peak(&ready.left[high.clone()]), "the high note is there");
    assert!(pieces.left == ready.left);
    let waited = &pieces.measured.waited[0];
    assert!(waited.answered && waited.seconds >= 1.5 && waited.silent.is_empty(), "{waited:?}");

    // A kit that shows nothing while it loads is waited for all the same:
    // the note that has not arrived is asked for, and heard to be silent.
    let quiet = Loading { grows: false, ..loading };
    let (took, pieces) = timed(&line(), &ScriptedFactory::loading(quiet), &options);
    assert!(took >= Duration::from_millis(1500), "{took:?}");
    assert!(pieces.left == ready.left, "the high note had arrived");
    assert!(pieces.measured.waited[0].silent.is_empty(), "{:?}", pieces.measured.waited);
}

#[test]
fn a_drum_that_comes_after_the_rest_is_waited_for_though_another_rings_over_it() {
    let _alone = ONE_AT_A_TIME.lock();
    // A crash that rings for seconds, and a hat right after it: the hat
    // is the last of the kit to load, and nothing shows while it does.
    let part = song(DRUMS, &[(49, 0.0), (42, 0.25), (36, 1.0), (42, 2.0)]);
    let options = RenderOptions {
        sample_rate: SR,
        max_tail_seconds: 1.0,
        patience: Duration::from_secs(4),
        ..RenderOptions::default()
    };
    let ready = render(&part, &ScriptedFactory::new(), &options).unwrap();
    let late = Loading {
        first: Duration::from_millis(100),
        late: Some((42, Duration::from_millis(1200))),
        ..Loading::default()
    };
    let (took, kit) = timed(&part, &ScriptedFactory::loading(late), &options);
    assert!(took >= Duration::from_millis(1200), "the hat was waited for: {took:?}");
    assert!(kit.left == ready.left, "and is in the render, as from a kit that was ready");
    let waited = &kit.measured.waited[0];
    assert!(waited.answered && waited.silent.is_empty(), "{waited:?}");
}

#[test]
fn notes_an_instrument_does_not_play_are_waited_out_once_and_said() {
    let _alone = ONE_AT_A_TIME.lock();
    // A bass that is ready, and a note far below its range: a key that
    // plays nothing sounds like a sample that has not been loaded.
    let options = RenderOptions {
        sample_rate: SR,
        max_tail_seconds: 0.5,
        patience: Duration::from_secs(2),
        ..RenderOptions::default()
    };
    let scripted = ScriptedFactory::new();
    let (first, out) = timed(&song(BASS, &[(41, 0.0), (20, 1.0)]), &scripted, &options);
    assert!(first >= Duration::from_secs(1), "held to it for half the patience: {first:?}");
    let waited = &out.measured.waited[0];
    assert!(waited.answered && waited.silent == [20], "said, so that a reply can: {waited:?}");
    assert!(peak(&out.left) > 0.1);

    // What was waited out is known of the instrument from then on,
    // whatever else it is asked.
    let (again, out) = timed(&song(BASS, &[(43, 0.0), (20, 1.0), (47, 2.0)]), &scripted, &options);
    assert!(again < first, "{again:?} after {first:?}");
    assert!(out.measured.waited.iter().all(|waited| waited.silent.is_empty()), "{:?}", out.measured.waited);
}

#[test]
fn a_key_that_sounds_one_time_and_not_the_next_does_not_keep_a_wait_going() {
    let _alone = ONE_AT_A_TIME.lock();
    // (Left to itself such a wait went on for two minutes: every try was
    // answered differently from the one before.)
    let options = RenderOptions { sample_rate: SR, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let fickle = ScriptedFactory::loading(Loading { fickle: Some(44), ..Loading::default() });
    let (took, out) = timed(&song(BASS, &[(42, 0.0), (44, 1.0)]), &fickle, &options);
    assert!(took < Duration::from_secs(30), "{took:?}");
    assert!(out.measured.waited.iter().all(|waited| waited.answered && waited.silent.is_empty()));
}

#[test]
fn an_instrument_that_speaks_late_is_heard_all_the_same() {
    let _alone = ONE_AT_A_TIME.lock();
    // Its notes begin 80 ms after their keys, which is after a rehearsed
    // note is usually let go. Notes that are taken to be silent are held
    // for half the patience there is, which here is a long time.
    let options = RenderOptions {
        sample_rate: SR,
        max_tail_seconds: 0.5,
        patience: Duration::from_secs(40),
        ..RenderOptions::default()
    };
    let late = ScriptedFactory::loading(Loading { onset: Duration::from_millis(80), ..Loading::default() });
    let (took, out) = timed(&song(BASS, &[(46, 0.0), (48, 1.0)]), &late, &options);
    assert!(peak(&out.left) > 0.1);
    assert!(took < Duration::from_secs(15), "it was not held for notes it had answered: {took:?}");
    assert!(out.measured.waited.iter().all(|waited| waited.answered && waited.silent.is_empty()));
}

#[test]
fn a_plugin_that_was_just_made_is_said_to_be_loading_and_a_built_in_device_never_is() {
    let _alone = ONE_AT_A_TIME.lock();
    let scripted =
        ScriptedFactory::loading(Loading { first: Duration::from_secs(1), grows: true, ..Loading::default() });
    let mut session = Session::new(factory(&scripted), AudioBackend::Null { sample_rate: SR }).unwrap();
    assert!(session.loading().is_empty());
    let (bass, synth) = session
        .edit(|project| {
            let track = project.add_track("Bass");
            let bass = project.add_device(ChainRef::Track(track), BASS, "Bass", None)?;
            let other = project.add_track("Keys");
            let synth = project.add_device(ChainRef::Track(other), "builtin:polysynth", "Synth", None)?;
            Ok((bass, synth))
        })
        .unwrap();
    assert_eq!(session.loading(), [bass], "the plugin, and not device {synth:?}");
    // Asked without the session, as a server does between requests.
    let watch = session.load_watch();
    assert_eq!(watch.loading_now(), [bass]);
    #[cfg(windows)]
    assert!(trill_engine::ready::is_loading(), "the program is growing");
    // While it loads it stays so: nothing is forgotten in a quiet moment.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(session.loading(), [bass]);
    // A device that is gone is not loading.
    session.edit(|project| project.remove_device(bass)).unwrap();
    assert!(session.loading().is_empty() && watch.loading_now().is_empty());
}

#[test]
fn a_front_end_can_ask_whether_one_device_is_ready() {
    let _alone = ONE_AT_A_TIME.lock();
    let scripted = ScriptedFactory::loading(Loading { first: Duration::from_millis(500), ..Loading::default() });
    let mut session = Session::new(factory(&scripted), AudioBackend::Null { sample_rate: SR }).unwrap();
    let (bass, amp, synth) = session
        .edit(|project| {
            let track = project.add_track("Bass");
            let bass = project.add_device(ChainRef::Track(track), BASS, "Bass", None)?;
            let amp = project.add_device(ChainRef::Track(track), AMP, "Amp", None)?;
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(&[Note { pitch: 40, start: 0.0, length: 1.0, velocity: 100 }])?;
            let other = project.add_track("Keys");
            let synth = project.add_device(ChainRef::Track(other), "builtin:polysynth", "Synth", None)?;
            Ok((bass, amp, synth))
        })
        .unwrap();
    let revision = session.revision();

    // The instrument: asked the notes its track plays, away from the
    // session, and ready once it has loaded.
    let (job, pitches) = session.readiness_job(bass).unwrap();
    assert_eq!(pitches, [40]);
    let readiness = job.wait_until_ready(&pitches, Duration::from_secs(5)).unwrap();
    assert!(readiness.ready && readiness.seconds >= 0.5 && readiness.seconds < 4.0, "{readiness:?}");
    assert!(readiness.silent.is_empty(), "{readiness:?}");
    // The effect after it: no notes to ask, so it is given noise to pass.
    let (job, pitches) = session.readiness_job(amp).unwrap();
    assert!(pitches.is_empty());
    assert!(job.wait_until_ready(&pitches, Duration::from_secs(5)).unwrap().ready);
    // A built-in device is ready when it exists: nothing is waited for.
    let (job, pitches) = session.readiness_job(synth).unwrap();
    let readiness = job.wait_until_ready(&pitches, Duration::from_secs(5)).unwrap();
    assert!(readiness.ready && readiness.seconds == 0.0 && pitches.len() > 5, "{readiness:?}");
    assert_eq!(session.revision(), revision, "asking changes nothing");
    assert!(session.readiness_job(trill_core::DeviceId(999)).is_err());

    // One that never answers is not ready after the patience it was given.
    let never = ScriptedFactory::loading(Loading { never: true, ..Loading::default() });
    let mut session = Session::new(factory(&never), AudioBackend::Null { sample_rate: SR }).unwrap();
    let bass = session
        .edit(|project| {
            let track = project.add_track("Bass");
            project.add_device(ChainRef::Track(track), BASS, "Bass", None)
        })
        .unwrap();
    let (job, pitches) = session.readiness_job(bass).unwrap();
    let begun = Instant::now();
    let readiness = job.wait_until_ready(&pitches, Duration::from_millis(800)).unwrap();
    let first = begun.elapsed();
    assert!(!readiness.ready && first >= Duration::from_millis(800), "{readiness:?}");
}

#[test]
fn what_never_answered_is_not_waited_for_as_long_again_and_what_was_ready_is_ready_sooner() {
    let _alone = ONE_AT_A_TIME.lock();
    let options = RenderOptions { sample_rate: SR, max_tail_seconds: 0.5, ..RenderOptions::default() };
    // A plugin that never answers costs the caller's patience once.
    let never = ScriptedFactory::loading(Loading { never: true, ..Loading::default() });
    let patient = RenderOptions { patience: Duration::from_millis(2500), ..options.clone() };
    let refused = |options: &RenderOptions| {
        let begun = Instant::now();
        let refused = render(&line(), &never, options);
        assert!(matches!(refused, Err(RenderError::NotAnswered(_))), "it went on without the instrument");
        begun.elapsed()
    };
    let first = refused(&patient);
    assert!(first >= Duration::from_millis(2500), "{first:?}");
    let again = refused(&patient);
    assert!(again < Duration::from_millis(2000), "known not to answer: {again:?} after {first:?}");

    // One that answers is held to its answers for a while the first time,
    // and for less when it answers the same way the next.
    let ready = ScriptedFactory::new();
    let (first, out) = timed(&line(), &ready, &options);
    assert!(peak(&out.left) > 0.1 && first >= Duration::from_millis(250), "{first:?}");
    let (again, same) = timed(&line(), &ready, &options);
    assert!(again < first, "{again:?} after {first:?}");
    assert!(same.left == out.left);
}

#[test]
fn a_render_does_not_go_on_without_an_instrument_that_never_answered_and_writes_nothing() {
    let _alone = ONE_AT_A_TIME.lock();
    // A bass that plays, and drums that never answer: one of two.
    let mut project = song(BASS, &[(40, 0.0), (43, 1.0)]);
    let drums = project.add_track("Drums");
    project.add_device(ChainRef::Track(drums), DRUMS, "Kit", None).unwrap();
    let clip = project.add_midi_clip(drums, "clip", 0.0, 8.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch: 77, start: 0.0, length: 0.5, velocity: 100 }]).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let (mix, stems) = (folder.path().join("song.wav"), folder.path().join("song stems"));
    let options = RenderOptions {
        sample_rate: SR,
        max_tail_seconds: 0.5,
        patience: Duration::from_millis(1500),
        stems: Some(Stems::all(&stems)),
        ..RenderOptions::default()
    };
    let scripted = ScriptedFactory::new();
    let job = |project: &Project| RenderJob {
        project: project.clone(),
        factory: factory(&scripted),
        audio: trill_engine::sources::AudioSources::default(),
    };
    // Key 77 is no drum of the kit. The render stops and names the track
    // that is missing, and that one only; no file is left anywhere.
    let Err(RenderError::NotAnswered(missing)) = job(&project).render_to_file(&mix, &options) else {
        panic!("a file was written without the drums")
    };
    let names: Vec<&str> = missing.iter().map(|waited| waited.track_name.as_str()).collect();
    assert_eq!(names, ["Drums"]);
    assert!(!mix.exists(), "no file where the song was to go");
    assert!(!stems.exists(), "and no stems, finished or begun, nor a folder for them");
    // Measuring is refused the same way: the numbers would not be the
    // song's.
    assert!(matches!(job(&project).measure(&options), Err(RenderError::NotAnswered(_))));

    // Muted, the track is left out on purpose, and the rest is rendered.
    let mut without = project.clone();
    without.track_mut(drums).unwrap().mute = true;
    let report = job(&without).render_to_file(&mix, &options).unwrap();
    assert!(mix.exists() && report.stats.peak_db > -40.0 && report.waited.iter().all(|waited| waited.answered));

    // And where the track itself says why its notes are silent (it knows
    // the key as a keyswitch of its instrument), the render goes on.
    let mut explained = project.clone();
    let profile = trill_core::InstrumentProfile {
        articulations: vec![trill_core::Articulation::key("choke", 77)],
        ..trill_core::InstrumentProfile::default()
    };
    explained.track_mut(drums).unwrap().profile = profile.validated().unwrap();
    let rendered = job(&explained).render(&options).unwrap();
    assert!(peak(&rendered.left) > 0.05);
    let waited: Vec<bool> = rendered.measured.waited.iter().map(|waited| waited.answered).collect();
    assert_eq!(waited, [false], "said, and no more: {:?}", rendered.measured.waited);
}

#[test]
fn a_plugin_that_is_held_up_while_others_are_let_go_is_waited_for_beyond_the_patience() {
    let _alone = ONE_AT_A_TIME.lock();
    // The host is behind with other work for two seconds after the bass
    // is made, and the bass does not begin to load before: for longer
    // than the caller has patience, nothing at all happens, and none of
    // it is the instrument's doing.
    let held_up = Loading { held_up: Duration::from_secs(2), first: Duration::from_millis(200), ..Loading::default() };
    let options = RenderOptions {
        sample_rate: SR,
        max_tail_seconds: 0.5,
        patience: Duration::from_millis(800),
        ..RenderOptions::default()
    };
    let (took, out) = timed(&line(), &ScriptedFactory::loading(held_up), &options);
    assert!(took >= Duration::from_millis(2200), "{took:?}");
    assert!(peak(&out.left) > 0.1, "and it is in the render");
    let waited = &out.measured.waited[0];
    assert!(waited.answered && waited.seconds >= 2.2, "{waited:?}");

    // One that does nothing with nothing in its way is still given up on
    // when the patience is over.
    // (Other notes than the tests beside this one play: what never
    // answered is remembered by what it was asked.)
    let idle = Loading { first: Duration::from_secs(3), ..Loading::default() };
    let begun = Instant::now();
    let refused = render(&song(BASS, &[(51, 0.0), (53, 1.0)]), &ScriptedFactory::loading(idle), &options);
    assert!(matches!(refused, Err(RenderError::NotAnswered(_))) && begun.elapsed() < Duration::from_millis(2800));
}
