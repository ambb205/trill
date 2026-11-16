//! Plugins that go on loading after they are made, and how that is told:
//! a kit that arrives piece by piece over longer than its answers take to
//! settle, and the question a front end asks of one device, "is it ready
//! yet".
//!
//! These are in a test program of their own, and run one at a time,
//! because the sign of loading is the whole program growing: a test that
//! grows on purpose would be seen by every other wait in the same
//! program, and how long a wait takes is what some of them measure.

use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::{ChainRef, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};
use trill_engine::scripted::{AMP, BASS, Loading, ScriptedFactory};
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

/// A bass line that opens low and goes above middle C half a second in.
fn line() -> Project {
    let mut project = Project::new("bass");
    let track = project.add_track("Bass");
    project.add_device(ChainRef::Track(track), BASS, "Bass", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    let notes = [(40, 0.0), (64, 1.0)].map(|(pitch, start)| Note { pitch, start, length: 0.5, velocity: 100 });
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    project
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
    // plugin was made. Its answers have long settled by then. What says
    // it is not done is that the program is still growing.
    let loading =
        Loading { first: Duration::from_millis(200), upper: Duration::from_millis(1500), grows: true, never: false };
    assert!(!trill_engine::ready::is_loading(), "nothing is loading yet");
    let begun = Instant::now();
    let pieces = render(&line(), &ScriptedFactory::loading(loading), &options).unwrap();
    assert!(begun.elapsed() >= Duration::from_millis(1500), "it was waited for: {:?}", begun.elapsed());
    assert_eq!(peak(&pieces.left[high.clone()]), peak(&ready.left[high.clone()]), "the high note is there");
    assert!(pieces.left == ready.left);
    assert!(pieces.measured.waited[0].answered && pieces.measured.waited[0].seconds >= 1.5);

    // Without that sign the same kit is taken as ready too soon, which is
    // the limit of asking by ear alone, and why memory is watched.
    let quiet = Loading { grows: false, ..loading };
    let early = render(&line(), &ScriptedFactory::loading(quiet), &options).unwrap();
    assert_eq!(peak(&early.left[high]), 0.0, "the high note had not arrived");

    // And the question for a front end: is anything loading right now?
    let scripted =
        ScriptedFactory::loading(Loading { first: Duration::from_secs(2), grows: true, ..Loading::default() });
    let mut session = Session::new(factory(&scripted), AudioBackend::Null { sample_rate: SR }).unwrap();
    session
        .edit(|project| {
            let track = project.add_track("Bass");
            project.add_device(ChainRef::Track(track), BASS, "Bass", None).map(drop)
        })
        .unwrap();
    assert!(trill_engine::ready::is_loading(), "the session's own instrument is loading");
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!trill_engine::ready::is_loading(), "and has done");
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
    let timed = |factory: &ScriptedFactory, options: &RenderOptions| {
        let begun = Instant::now();
        let out = render(&line(), factory, options).unwrap();
        (begun.elapsed(), out)
    };
    let (first, out) = timed(&never, &patient);
    assert!(first >= Duration::from_millis(2500) && !out.measured.waited[0].answered, "{first:?}");
    let (again, _) = timed(&never, &patient);
    assert!(again < Duration::from_millis(2000), "known not to answer: {again:?} after {first:?}");

    // One that answers is held to its answers for a while the first time,
    // and for less when it answers the same way the next.
    let ready = ScriptedFactory::new();
    let (first, out) = timed(&ready, &options);
    assert!(peak(&out.left) > 0.1 && first >= Duration::from_millis(250), "{first:?}");
    let (again, same) = timed(&ready, &options);
    assert!(again < first, "{again:?} after {first:?}");
    assert!(same.left == out.left);
}
