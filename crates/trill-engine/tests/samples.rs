//! The sampler and the drum rack driven the way a front end drives them:
//! through the session, with real files, heard through renders.

mod common;

use common::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use trill_core::{ChainRef, DeviceId, Note, TrackId};
use trill_dsp::{BuiltinFactory, Builtins, drumrack, sampler};
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Problem, SampleFile, Session};
use trill_media::MediaCache;

/// A session wired the way the application wires one: one cache, shared by
/// the engine and the sample players. No master limiter, whose look-ahead
/// would shift every measurement.
fn session() -> Session {
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).unwrap();
    session
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 1.0, ..RenderOptions::default() }
}

fn render(session: &mut Session) -> Vec<f32> {
    session.render_job().render(&options()).unwrap().left
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 127, ..Default::default() }
}

/// A track with `uri` on it and a clip of `notes`.
fn instrument(session: &mut Session, uri: &str, notes: &[Note]) -> (TrackId, DeviceId) {
    let track = session.edit(|project| Ok(project.add_track("Sampled"))).unwrap();
    let device = session.add_device(ChainRef::Track(track), uri, None, None).unwrap();
    session
        .edit(|project| {
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(notes)
        })
        .unwrap();
    (track, device)
}

fn sampler_path(session: &Session, device: DeviceId) -> Option<String> {
    let state = session.project().device(device).unwrap().state.clone();
    sampler::State::from_device_state(state.as_deref()).unwrap().path
}

#[test]
fn a_sampler_plays_the_file_it_is_given_and_undo_gives_the_old_one_back() {
    let folder = folder();
    // Files at 44.1 kHz in a 48 kHz engine, so pitch is not right by luck.
    let low = write_wav(&file(&folder, "low.wav"), 44_100.0, &sine(330.0, 44_100.0, 2.0, 0.5));
    let high = write_wav(&file(&folder, "high.wav"), 44_100.0, &sine(550.0, 44_100.0, 2.0, 0.5));
    let mut session = session();
    // The root note from beat 0 and a fifth above it from beat 2.
    let (_, device) = instrument(&mut session, "builtin:sampler", &[note(60, 0.0, 1.5), note(67, 2.0, 1.5)]);
    session.set_device_params(device, &[("volume".into(), -6.0), ("velocity_sens".into(), 0.0)]).unwrap();
    let pitches = |session: &mut Session| {
        let out = render(session);
        let hz = |from: f64| zero_crossing_hz(&out[(from * SR) as usize..((from + 0.6) * SR) as usize], SR);
        (hz(0.05), hz(1.05), peak(&out[4_800..28_800]))
    };

    assert_eq!(peak(&render(&mut session)), 0.0, "an empty sampler is silent");
    assert!(session.problems().is_empty() && session.sample_files(device).unwrap().is_empty());

    session.set_sampler_file(device, Some(&low)).unwrap();
    let (root, fifth, level) = pitches(&mut session);
    println!("low.wav:  root {root:.3} Hz, fifth {fifth:.3} Hz, level {level:.4}");
    assert!((root - 330.0).abs() < 0.2 && (fifth - 330.0 * 1.498_307).abs() < 0.3, "{root} / {fifth}");
    assert!((level - 0.5 * 0.501_187).abs() < 0.005, "the volume set before the file is kept: {level}");
    assert_eq!(
        session.sample_files(device).unwrap(),
        [SampleFile { slot: "sample".into(), path: low.clone(), problem: None }]
    );

    session.set_sampler_file(device, Some(&high)).unwrap();
    let (root, _, level) = pitches(&mut session);
    println!("high.wav: root {root:.3} Hz, level {level:.4}");
    assert!((root - 550.0).abs() < 0.3, "the new file plays: {root}");
    assert!((level - 0.5 * 0.501_187).abs() < 0.005, "parameters survive the change of file");

    // One undo step per file.
    assert!(session.undo());
    assert_eq!(sampler_path(&session, device), Some(low.clone()));
    assert!((pitches(&mut session).0 - 330.0).abs() < 0.2, "undo brought the old file back");
    assert!(session.redo());
    assert!((pitches(&mut session).0 - 550.0).abs() < 0.3);

    // Emptying it, and undoing that.
    session.set_sampler_file(device, None).unwrap();
    assert_eq!(sampler_path(&session, device), None);
    assert_eq!(peak(&render(&mut session)), 0.0);
    assert!(session.undo());
    assert!((pitches(&mut session).0 - 550.0).abs() < 0.3);
}

#[test]
fn bad_sample_requests_change_nothing_and_say_why() {
    let folder = folder();
    let good = write_wav(&file(&folder, "good.wav"), SR, &sine(440.0, SR, 0.5, 0.5));
    let garbage = file(&folder, "garbage.wav");
    std::fs::write(&garbage, vec![3u8; 2_048]).unwrap();
    let mut session = session();
    let (track, sampler_device) = instrument(&mut session, "builtin:sampler", &[note(60, 0.0, 1.0)]);
    let rack = session.add_device(ChainRef::Track(track), "builtin:drumrack", None, None).unwrap();
    let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
    session.set_sampler_file(sampler_device, Some(&good)).unwrap();
    let (before, revision) = (session.project().clone(), session.revision());

    let failures = [
        (session.set_sampler_file(sampler_device, Some("no/such.wav")), "does not exist"),
        (session.set_sampler_file(sampler_device, Some(&garbage.display().to_string())), "garbage.wav"),
        (session.set_sampler_file(synth, Some(&good)), "is not a sampler"),
        (session.set_sampler_file(rack, Some(&good)), "is not a sampler"),
        (session.set_sampler_file(DeviceId(999), Some(&good)), "device 999 not found"),
        (session.set_pad_file(sampler_device, 36, Some(&good)), "is not a drum rack"),
        (session.set_pad_file(rack, 35, Some(&good)), "no pad for note 35"),
        (session.set_pad_file(rack, 52, None), "notes 36 to 51"),
        (session.set_pad_file(rack, 40, Some("no/such.wav")), "keeps the sample it had"),
    ];
    for (result, expected) in failures {
        let error = result.expect_err(expected).to_string();
        assert!(error.contains(expected), "'{error}' should mention '{expected}'");
    }
    assert_eq!(session.project(), &before, "a refused sample must leave the project as it was");
    assert_eq!(session.revision(), revision, "and add nothing to undo");
    assert!(session.problems().is_empty());

    // Setting what is already there is not an edit either.
    session.set_sampler_file(sampler_device, Some(&good)).unwrap();
    session.set_pad_file(rack, 40, None).unwrap();
    assert_eq!(session.revision(), revision);

    // A session whose factory cannot load files refuses, undoes, and says
    // what is missing, rather than leaving a broken device behind.
    let mut plain = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let device = plain.add_device(ChainRef::Master, "builtin:sampler", None, None).unwrap();
    let before = plain.project().clone();
    let error = plain.set_sampler_file(device, Some(&good)).unwrap_err().to_string();
    assert!(error.contains("without an audio loader"), "{error}");
    assert_eq!(plain.project(), &before);
    assert!(plain.device_params(device).is_ok(), "the device is still the working, empty one");
}

#[test]
fn a_drum_rack_plays_its_pads_and_reports_the_missing_ones() {
    let folder = folder();
    let decaying = |hz: f64, rate: f64| -> Vec<f32> {
        sine(hz, SR, 0.25, 0.8).iter().enumerate().map(|(n, s)| s * (-(n as f64) / SR * rate).exp() as f32).collect()
    };
    let kick = write_wav(&file(&folder, "kick.wav"), SR, &decaying(60.0, 12.0));
    let snare_path = file(&folder, "snare.wav");
    let snare = write_wav(&snare_path, SR, &decaying(1_000.0, 30.0));
    let hat = write_wav(&file(&folder, "hat.wav"), SR, &decaying(6_000.0, 60.0));

    let mut session = session();
    // Kick on beats 0 and 2, snare on 1 and 3, a hat on 1.5.
    let hits = [note(36, 0.0, 0.1), note(38, 1.0, 0.1), note(42, 1.5, 0.1), note(36, 2.0, 0.1), note(38, 3.0, 0.1)];
    let (track, rack) = instrument(&mut session, "builtin:drumrack", &hits);
    // Every hit as its file is, whatever its velocity.
    let plain = [("volume".into(), 0.0), ("velocity_sens".into(), 0.0), ("pad38_level".into(), 0.5)];
    session.set_device_params(rack, &plain).unwrap();
    session.begin_gesture();
    session.set_pad_file(rack, 36, Some(&kick)).unwrap();
    session.set_pad_file(rack, 38, Some(&snare)).unwrap();
    session.set_pad_file(rack, 42, Some(&hat)).unwrap();
    session.end_gesture();
    assert!(session.problems().is_empty(), "{:?}", session.problems());

    let beat = |beats: f64| (beats * 0.5 * SR) as usize;
    let heard = |out: &[f32], beats: f64| {
        let hit = &out[beat(beats)..beat(beats) + 4_800];
        (peak(hit), zero_crossing_hz(&hit[100..3_000], SR))
    };
    let out = render(&mut session);
    let (kick_level, kick_hz) = heard(&out, 0.0);
    let (snare_level, snare_hz) = heard(&out, 1.0);
    let (hat_level, hat_hz) = heard(&out, 1.5);
    println!("kick {kick_hz:.1} Hz at {kick_level:.3}, snare {snare_hz:.1} Hz at {snare_level:.3}, hat {hat_hz:.1} Hz");
    assert!((kick_hz - 60.0).abs() < 2.0 && kick_level > 0.5, "the kick is on beat 0: {kick_hz} Hz");
    assert!((snare_hz - 1_000.0).abs() < 5.0, "the snare is on beat 1: {snare_hz} Hz");
    assert!((snare_level - 0.4).abs() < 0.03, "at the half level set before its file was: {snare_level}");
    assert!((hat_hz - 6_000.0).abs() < 60.0 && hat_level > 0.3, "the hat is on beat 1.5: {hat_hz} Hz");
    assert_eq!(
        session.sample_files(rack).unwrap().iter().map(|file| file.slot.as_str()).collect::<Vec<_>>(),
        ["pad 36", "pad 38", "pad 42"]
    );

    // The three pads were one gesture, so one undo empties the rack.
    assert!(session.undo());
    assert_eq!(peak(&render(&mut session)), 0.0);
    assert!(session.redo());

    // Emptying a pad silences only that pad.
    session.set_pad_file(rack, 42, None).unwrap();
    let out = render(&mut session);
    assert!(peak(&out[beat(1.5)..beat(1.5) + 480]) < 0.05, "the hat pad is empty");
    assert!(heard(&out, 1.0).0 > 0.3 && heard(&out, 2.0).0 > 0.5);

    // The project saves, reloads and renders the same.
    let project_file = file(&folder, "song.trill");
    let before = render(&mut session);
    session.save(&project_file).unwrap();
    session.new_project();
    session.load(&project_file).unwrap();
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert!(render(&mut session) == before, "a reloaded project with samples renders differently");
    let state = session.project().device(rack).unwrap().state.clone();
    let pads = drumrack::State::from_device_state(state.as_deref()).unwrap();
    assert_eq!((pads.path(36), pads.path(38), pads.path(42)), (Some(kick.as_str()), Some(snare.as_str()), None));

    // The snare's file goes missing. The project still opens; the problem
    // names the device and the pad, the device is not failed, and the kick
    // still plays.
    std::fs::remove_file(&snare_path).unwrap();
    session.load(&project_file).unwrap();
    match session.problems() {
        [Problem::Sample { device, slot, reason }] => {
            assert_eq!((*device, slot.as_deref()), (rack, Some("pad 38")));
            assert!(
                reason.contains("pad 38") && reason.contains("snare.wav") && reason.contains("does not exist"),
                "{reason}"
            );
        }
        other => panic!("expected the missing snare to be reported, got {other:?}"),
    }
    assert!(session.problems()[0].to_string().starts_with(&format!("{rack}: pad 38")), "{}", session.problems()[0]);
    assert!(session.device_params(rack).is_ok(), "a missing sample does not fail the device");
    let files = session.sample_files(rack).unwrap();
    assert!(files[0].problem.is_none() && files[1].problem.as_deref().is_some_and(|p| p.contains("snare.wav")));
    let out = render(&mut session);
    assert!(heard(&out, 0.0).0 > 0.5 && heard(&out, 2.0).0 > 0.5, "the kick still plays");
    assert!(peak(&out[beat(1.0)..beat(1.0) + 4_800]) < 0.05, "the snare is silent");

    // The file comes back: the next change to the project picks it up and
    // the problem is gone.
    write_wav(&snare_path, SR, &decaying(1_000.0, 30.0));
    session.edit(|project| project.set_track_volume(track, -0.5)).unwrap();
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert!(session.sample_files(rack).unwrap().iter().all(|file| file.problem.is_none()));
    assert!((heard(&render(&mut session), 1.0).1 - 1_000.0).abs() < 5.0, "and the snare plays again");

    // Putting another file on the pad fixes it too, as one undo step.
    std::fs::remove_file(&snare_path).unwrap();
    session.load(&project_file).unwrap();
    assert_eq!(session.problems().len(), 1);
    session.set_pad_file(rack, 38, Some(&hat)).unwrap();
    assert!(session.problems().is_empty());
    assert!(session.undo());
    assert_eq!(session.problems().len(), 1, "undo brings the missing file's pad, and its problem, back");
}

#[test]
fn changing_a_sample_while_playing_keeps_the_song_going() {
    let folder = folder();
    let first = write_wav(&file(&folder, "first.wav"), SR, &sine(220.0, SR, 4.0, 0.5));
    let second = write_wav(&file(&folder, "second.wav"), SR, &sine(330.0, SR, 4.0, 0.5));
    let mut session = session();
    let (track, device) = instrument(&mut session, "builtin:sampler", &[note(60, 0.0, 4.0)]);
    session.edit(|project| project.set_loop(true, 0.0, 4.0)).unwrap();
    session.set_sampler_file(device, Some(&first)).unwrap();
    let meter = session.track_meter(track).unwrap();

    session.play();
    std::thread::sleep(Duration::from_millis(200));
    assert!(meter.take().0 > 0.05, "the sampler is heard");
    let before = session.transport().position_beats;

    session.set_sampler_file(device, Some(&second)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let status = session.transport();
    assert!(status.playing && status.position_beats > before, "the transport kept running: {status:?}");
    assert_eq!(session.audio_faults(), 0);
    // The new instance starts silent and picks up at the loop's next pass.
    session.stop();
    assert!(Path::new(&sampler_path(&session, device).unwrap()).ends_with("second.wav"));
}
