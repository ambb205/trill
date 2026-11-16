//! How a caller that cannot listen hears: renders that wait for plugins to
//! be ready, measure every track and every part of the song, and can be
//! made without writing a file; and probes, which play an instrument notes
//! to find out what it does.
//!
//! The plugins here are scripted ([`trill_engine::scripted`]): a bass and
//! a drum kit that can be told to take their time after they are created,
//! as samplers do.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory, Purpose};
use trill_core::{ChainRef, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::probe::{Heard, ProbeNote, ProbeOptions};
use trill_engine::render::{RenderOptions, Section, render};
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, MUTE_KEY, ScriptedFactory};
use trill_engine::{AudioBackend, ProbeTarget, Session};

const SR: u32 = 48_000;

/// A plugin is taken to be ready when, among other things, the program has
/// stopped growing in memory (`ready.rs`), and every test in this file runs
/// in the one program: a test that renders a long stretch makes it grow, and
/// a wait in another test then lasts longer. The tests that say how *short*
/// a wait was therefore run alone; the rest run beside each other.
static QUIET: RwLock<()> = RwLock::new(());

fn alone() -> RwLockWriteGuard<'static, ()> {
    QUIET.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn beside_the_others() -> RwLockReadGuard<'static, ()> {
    QUIET.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100 }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |peak, sample| peak.max(sample.abs()))
}

fn db(linear: f32) -> f32 {
    20.0 * linear.max(1e-6).log10()
}

fn seconds(range: std::ops::Range<f64>) -> std::ops::Range<usize> {
    (range.start * f64::from(SR)) as usize..(range.end * f64::from(SR)) as usize
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR, max_tail_seconds: 1.0, ..RenderOptions::default() }
}

/// The built-in devices and the scripted ones, as one factory.
fn factory(scripted: &ScriptedFactory) -> Arc<dyn DeviceFactory> {
    Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())]))
}

/// A track playing `uri` with one clip of `notes` from beat 0. The song is
/// at 120 BPM: a beat is half a second.
fn track_with(project: &mut Project, name: &str, uri: &str, notes: &[Note]) -> TrackId {
    let track = project.add_track(name);
    project.add_device(ChainRef::Track(track), uri, name, None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
    track
}

/// A snare on the first sample of the song, one 0.2 s in and one at 1 s.
fn snares() -> Project {
    let mut project = Project::new("drums");
    track_with(&mut project, "Drums", DRUMS, &[note(38, 0.0, 0.25), note(38, 0.4, 0.25), note(38, 2.0, 0.25)]);
    project
}

// ---- a render waits for its plugins ----------------------------------------------

#[test]
fn a_note_on_the_first_sample_sounds_in_full_on_a_plugin_that_is_slow_to_be_ready() {
    let _quiet = alone();
    // Ready at once: what the hits are worth.
    let ready = render(&snares(), &ScriptedFactory::new(), &options()).unwrap();
    let hits = [0.0, 0.2, 1.0];
    let levels = |audio: &[f32]| -> Vec<f32> { hits.iter().map(|at| peak(&audio[seconds(*at..*at + 0.1)])).collect() };
    let attack = |audio: &[f32]| peak(&audio[seconds(0.0..0.005)]);
    assert!(levels(&ready.left).iter().all(|level| *level > 0.1), "{:?}", levels(&ready.left));
    assert!(ready.measured.waited.is_empty(), "nothing to say about a plugin that was ready");

    // Deaf for 0.4 s after it is created, far longer than a render of a
    // second of drums takes: every hit is there all the same, and what is
    // rendered is what the ready one rendered, sample for sample.
    let slow = ScriptedFactory::loading(Loading { first: Duration::from_millis(400), ..Loading::default() });
    let begun = Instant::now();
    let waited = render(&snares(), &slow, &options()).unwrap();
    assert!(begun.elapsed() >= Duration::from_millis(400), "it was waited for");
    assert_eq!(levels(&waited.left), levels(&ready.left));
    assert_eq!(attack(&waited.left), attack(&ready.left), "the first hit has its attack");
    assert!(waited.left == ready.left && waited.right == ready.right, "and the render is the same render");

    // The plugin was told what it was made for.
    assert_eq!(slow.made(), [(DRUMS.to_string(), Purpose::Rendering)]);
}

#[test]
fn a_plugin_that_loads_piece_by_piece_is_waited_for_until_every_note_answers() {
    let _quiet = beside_the_others();
    // The bass answers its low notes after 0.2 s and those from middle C
    // up only after 0.4 s. The song opens low and goes high a beat later.
    let mut project = Project::new("bass");
    track_with(&mut project, "Bass", BASS, &[note(40, 0.0, 0.5), note(64, 1.0, 0.5)]);
    let ready = render(&project, &ScriptedFactory::new(), &options()).unwrap();
    let loading =
        Loading { first: Duration::from_millis(200), upper: Duration::from_millis(400), ..Loading::default() };
    let pieces = render(&project, &ScriptedFactory::loading(loading), &options()).unwrap();
    let high = seconds(0.5..0.6);
    assert!(peak(&ready.left[high.clone()]) > 0.1);
    assert_eq!(peak(&pieces.left[high]), peak(&ready.left[seconds(0.5..0.6)]), "the high note is there too");
    assert!(pieces.left == ready.left);
}

#[test]
fn an_effect_that_passes_no_sound_for_a_moment_is_waited_for_wherever_it_sits() {
    let _quiet = alone();
    // An amplifier that lets nothing through for 0.4 s after it is made:
    // behind a built-in synth, which has nothing to be late with itself,
    // behind a plugin, and on the master.
    let slow = || ScriptedFactory::loading(Loading { first: Duration::from_millis(400), ..Loading::default() });
    let behind = |instrument: &str| {
        let mut project = Project::new("amp");
        let track = track_with(&mut project, "Lead", instrument, &[note(45, 0.0, 1.0), note(45, 2.0, 1.0)]);
        project.add_device(ChainRef::Track(track), AMP, "Amp", None).unwrap();
        project
    };
    let first_and_later =
        |out: &trill_engine::Rendered| (peak(&out.left[seconds(0.0..0.2)]), peak(&out.left[seconds(1.0..1.2)]));
    for instrument in ["builtin:polysynth", BASS] {
        let project = behind(instrument);
        let ready = render(&project, factory(&ScriptedFactory::new()).as_ref(), &options()).unwrap();
        let (first, later) = first_and_later(&ready);
        assert!(first > 0.01 && (first - later).abs() < 0.02, "{instrument}: {first} then {later}");
        let scripted = slow();
        let waited = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
        assert_eq!(first_and_later(&waited), (first, later), "{instrument}: the first note comes through the amp");
        assert!(waited.left == ready.left, "{instrument}: and the render is the same render");
        assert!(scripted.made().iter().any(|(uri, purpose)| uri == AMP && *purpose == Purpose::Rendering));
    }

    // On the master, behind everything.
    let mut project = Project::new("master");
    track_with(&mut project, "Lead", "builtin:polysynth", &[note(45, 0.0, 1.0)]);
    project.add_device(ChainRef::Master, AMP, "Amp", None).unwrap();
    let ready = render(&project, factory(&ScriptedFactory::new()).as_ref(), &options()).unwrap();
    let waited = render(&project, factory(&slow()).as_ref(), &options()).unwrap();
    assert!(peak(&ready.left[seconds(0.0..0.2)]) > 0.01 && waited.left == ready.left);

    // One that never passes anything (a gate that stays shut) is given a
    // few seconds, not the whole of the caller's patience, and is not
    // spoken of: the track is silent, which is said elsewhere.
    let shut = ScriptedFactory::loading(Loading { never: true, ..Loading::default() });
    let patient = RenderOptions { patience: Duration::from_secs(30), ..options() };
    let begun = Instant::now();
    let out = render(&behind("builtin:polysynth"), factory(&shut).as_ref(), &patient).unwrap();
    assert!(begun.elapsed() < Duration::from_secs(8), "{:?}", begun.elapsed());
    assert!(out.measured.waited.is_empty(), "{:?}", out.measured.waited);
    assert_eq!(peak(&out.left), 0.0);
}

#[test]
fn a_long_wait_is_reported_and_a_plugin_that_never_answers_is_given_up_on_and_named() {
    let _quiet = alone();
    let slow = ScriptedFactory::loading(Loading { first: Duration::from_millis(1200), ..Loading::default() });
    let out = render(&snares(), &slow, &options()).unwrap();
    let [waited] = out.measured.waited.as_slice() else { panic!("{:?}", out.measured.waited) };
    assert!(waited.answered && waited.seconds >= 1.2, "{waited:?}");
    assert_eq!((waited.track_name.as_str(), waited.device_name.as_str()), ("Drums", "Drums"));
    assert!(peak(&out.left[seconds(0.0..0.1)]) > 0.1);

    // One that never answers: the render goes ahead after the patience it
    // was given, says which plugin it gave up on, and the next render of
    // the same thing does not wait that long again.
    let never = ScriptedFactory::loading(Loading { never: true, ..Loading::default() });
    let patient = RenderOptions { patience: Duration::from_millis(1500), ..options() };
    let begun = Instant::now();
    let out = render(&snares(), &never, &patient).unwrap();
    let first = begun.elapsed();
    let [waited] = out.measured.waited.as_slice() else { panic!("{:?}", out.measured.waited) };
    assert!(!waited.answered && waited.seconds >= 1.5, "{waited:?}");
    assert!(first >= Duration::from_millis(1500));
    assert_eq!(peak(&out.left), 0.0);
    let again = render(&snares(), &never, &patient).unwrap();
    assert!(!again.measured.waited[0].answered);

    // Notes outside the range of the range being rendered are not waited
    // for: a track with nothing to play in it has nothing to be late with.
    let begun = Instant::now();
    let later = RenderOptions { start_beat: 4.0, end_beat: Some(6.0), ..patient };
    let out = render(&snares(), &never, &later).unwrap();
    assert!(out.measured.waited.is_empty() && begun.elapsed() < Duration::from_millis(1400));
}

#[test]
fn built_in_devices_are_not_touched_by_the_wait_and_render_as_they_always_did() {
    let _quiet = beside_the_others();
    // A synth beside a slow plugin. Heard alone, the synth's part is the
    // same samples as in a song that has no plugin at all.
    let synth_notes = [note(57, 0.0, 1.0), note(64, 1.0, 1.0)];
    let mut alone = Project::new("alone");
    track_with(&mut alone, "Synth", "builtin:polysynth", &synth_notes);
    let scripted = ScriptedFactory::loading(Loading { first: Duration::from_millis(300), ..Loading::default() });
    let plain = render(&alone, factory(&scripted).as_ref(), &options()).unwrap();
    assert!(peak(&plain.left) > 0.01);
    assert!(scripted.made().is_empty());

    let mut both = Project::new("both");
    let synth = track_with(&mut both, "Synth", "builtin:polysynth", &synth_notes);
    track_with(&mut both, "Drums", DRUMS, &[note(36, 0.0, 0.25)]);
    let only = RenderOptions { only_tracks: Some(vec![synth]), ..options() };
    let heard = render(&both, factory(&scripted).as_ref(), &only).unwrap();
    assert_eq!(scripted.made().len(), 1, "the plugin was made, and waited for");
    assert!(heard.left == plain.left && heard.right == plain.right, "sample for sample");

    // And the same render twice is the same render.
    let again = render(&both, factory(&scripted).as_ref(), &only).unwrap();
    assert!(again.left == heard.left);
}

#[test]
fn a_session_plays_its_plugins_and_a_render_of_it_renders_them() {
    let _quiet = beside_the_others();
    let scripted = ScriptedFactory::new();
    let mut session = Session::new(factory(&scripted), AudioBackend::Null { sample_rate: SR }).unwrap();
    session.edit(|project| Ok(track_with(project, "Bass", BASS, &[note(40, 0.0, 1.0)]))).unwrap();
    assert_eq!(scripted.made(), [(BASS.to_string(), Purpose::Playing)]);
    session.render_job().measure(&options()).unwrap();
    assert_eq!(scripted.made()[1], (BASS.to_string(), Purpose::Rendering));
}

// ---- what a render measures --------------------------------------------------------

/// A low synth line and a snare pattern, the first half of the song loud
/// and the second half with the synth alone and softer.
fn two_parts() -> (Project, TrackId, TrackId) {
    let mut project = Project::new("song");
    let mut low = Vec::new();
    for beat in 0..8 {
        low.push(Note { velocity: if beat < 4 { 120 } else { 40 }, ..note(45, f64::from(beat), 1.0) });
    }
    let synth = track_with(&mut project, "Low", "builtin:polysynth", &low);
    let hits: Vec<Note> = (0..8).map(|step| note(38, f64::from(step) * 0.5, 0.25)).collect();
    let drums = track_with(&mut project, "Snare", DRUMS, &hits);
    project.add_marker(0.0, "Loud").unwrap();
    project.add_marker(4.0, "Soft").unwrap();
    (project, synth, drums)
}

fn sections() -> Vec<Section> {
    vec![
        Section { name: "Loud".to_string(), start_beat: 0.0, end_beat: 4.0 },
        Section { name: "Soft".to_string(), start_beat: 4.0, end_beat: 8.0 },
    ]
}

#[test]
fn every_track_is_measured_as_the_mix_is() {
    let _quiet = beside_the_others();
    let (project, synth, drums) = two_parts();
    let scripted = ScriptedFactory::new();
    let out = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
    let measured = &out.measured;
    let of = |track: TrackId| measured.tracks.iter().find(|level| level.track == track).unwrap();
    let (low, snare) = (of(synth), of(drums));
    assert_eq!((low.name.as_str(), snare.name.as_str()), ("Low", "Snare"));

    // Peaks are what the meters said they were.
    for (track, linear) in &out.track_peaks {
        assert!((of(*track).peak_db - db(*linear)).abs() < 0.01, "{track:?}");
    }
    // Where each has its energy: an A2 below 250 Hz, a burst of noise above it.
    assert!(low.spectrum.bass_db > -3.0 && low.spectrum.presence_db < -10.0, "{:?}", low.spectrum);
    assert!(snare.spectrum.bass_db < -15.0, "{:?}", snare.spectrum);
    assert!(snare.spectrum.mid_db > -6.0 || snare.spectrum.presence_db > -6.0, "{:?}", snare.spectrum);
    // Both are measured for loudness, and neither is as loud as the mix.
    assert!(low.loudness_lufs > -60.0 && snare.loudness_lufs > -60.0, "{} {}", low.loudness_lufs, snare.loudness_lufs);
    let loudest = low.loudness_lufs.max(snare.loudness_lufs);
    assert!(measured.stats.loudness_lufs >= loudest - 0.5, "{} against {loudest}", measured.stats.loudness_lufs);
    assert_eq!(low.stereo_correlation.round(), 1.0);

    // A track turned down 6 dB is 6 LU quieter, and nothing else moves.
    let mut quieter = project.clone();
    quieter.track_mut(synth).unwrap().volume_db -= 6.0;
    let turned = render(&quieter, factory(&scripted).as_ref(), &options()).unwrap();
    let after = |track: TrackId| turned.measured.tracks.iter().find(|level| level.track == track).unwrap();
    assert!((low.loudness_lufs - after(synth).loudness_lufs - 6.0).abs() < 0.2, "{}", after(synth).loudness_lufs);
    assert_eq!(after(drums).loudness_lufs, snare.loudness_lufs);
}

#[test]
fn each_part_of_the_song_has_its_own_loudness_in_the_mix_and_on_every_track() {
    let _quiet = beside_the_others();
    let (project, synth, drums) = two_parts();
    let scripted = ScriptedFactory::new();
    let with_parts = RenderOptions { sections: sections(), ..options() };
    let out = render(&project, factory(&scripted).as_ref(), &with_parts).unwrap();
    let [loud, soft] = out.measured.sections.as_slice() else { panic!("{:?}", out.measured.sections) };
    assert_eq!((loud.section.name.as_str(), soft.section.name.as_str()), ("Loud", "Soft"));
    assert!(loud.loudness_lufs > soft.loudness_lufs + 3.0, "{} then {}", loud.loudness_lufs, soft.loudness_lufs);
    assert!(loud.peak_db > soft.peak_db);

    let of = |track: TrackId| out.measured.tracks.iter().find(|level| level.track == track).unwrap();
    // The synth plays softer in the second part; the snare is gone from it.
    let low = &of(synth).section_loudness;
    assert!(low[0] > low[1] + 3.0 && low[1] > -60.0, "{low:?}");
    let snare = &of(drums).section_loudness;
    assert!(snare[0] > -40.0 && snare[1] < -100.0, "{snare:?}");

    // A part that is given but lies outside what was rendered is silent,
    // not an error; without parts there is nothing to list.
    let beyond = vec![Section { name: "Later".to_string(), start_beat: 40.0, end_beat: 44.0 }];
    let out = render(&project, factory(&scripted).as_ref(), &RenderOptions { sections: beyond, ..options() }).unwrap();
    assert!(out.measured.sections[0].loudness_lufs < -100.0);
    let out = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
    assert!(out.measured.sections.is_empty() && out.measured.tracks[0].section_loudness.is_empty());
}

#[test]
fn a_compressor_and_a_limiter_say_how_much_they_took_off() {
    let _quiet = beside_the_others();
    let mut project = Project::new("squash");
    let hits: Vec<Note> = (0..8).map(|step| note(36, f64::from(step) * 0.5, 0.25)).collect();
    let track = track_with(&mut project, "Kick", DRUMS, &hits);
    let compressor = project.add_device(ChainRef::Track(track), "builtin:compressor", "Compressor", None).unwrap();
    let limiter = project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
    let set = |project: &mut Project, device, key: &str, value: f64| {
        project.device_mut(device).unwrap().params.insert(key.to_string(), value);
    };
    // A threshold far above the kick, and a ceiling above the mix: neither
    // has anything to do.
    set(&mut project, compressor, "threshold", 0.0);
    set(&mut project, limiter, "ceiling", 0.0);
    let scripted = ScriptedFactory::new();
    let idle = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
    let [on_track, on_master] = idle.measured.dynamics.as_slice() else { panic!("{:?}", idle.measured.dynamics) };
    assert_eq!((on_track.device, on_track.track, on_track.name.as_str()), (compressor, Some(track), "Compressor"));
    assert_eq!((on_master.device, on_master.track), (limiter, None));
    assert_eq!((on_track.most_db, on_track.typical_db, on_track.active), (0.0, 0.0, 0.0));
    assert_eq!(on_master.most_db, 0.0);
    let untouched = db(idle.track_peaks[0].1);

    // The threshold 20 dB below the kick's peak at 4:1, as fast as it
    // goes, and 20 dB of makeup after it.
    let settings =
        [("threshold", f64::from(untouched) - 20.0), ("ratio", 4.0), ("attack", 0.1), ("knee", 0.0), ("makeup", 20.0)];
    for (key, value) in settings {
        set(&mut project, compressor, key, value);
    }
    set(&mut project, limiter, "ceiling", -12.0);
    let squashed = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
    let [on_track, on_master] = squashed.measured.dynamics.as_slice() else { panic!() };
    // 20 dB over at 4:1 comes out 5 dB over: 15 dB off at the most.
    assert!((on_track.most_db - 15.0).abs() < 1.5, "{on_track:?}");
    assert!(on_track.typical_db > 2.0 && on_track.typical_db < on_track.most_db, "{on_track:?}");
    assert!(on_track.active > 0.05 && on_track.active < 0.9, "{on_track:?}");
    // What the meter says was taken off is what is missing from the peak.
    let taken = untouched + 20.0 - db(squashed.track_peaks[0].1);
    assert!((taken - on_track.most_db).abs() < 2.0, "{taken} dB quieter, {} dB taken off", on_track.most_db);
    assert!(on_master.most_db > 1.0, "the limiter holds the mix under its ceiling: {on_master:?}");
    assert!(squashed.measured.stats.peak_db <= -11.9, "{}", squashed.measured.stats.peak_db);

    // A device that is switched off says nothing.
    project.device_mut(compressor).unwrap().enabled = false;
    let bypassed = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
    assert_eq!(bypassed.measured.dynamics.len(), 1);
}

#[test]
fn a_render_can_be_measured_without_keeping_or_writing_anything() {
    let _quiet = beside_the_others();
    let (project, ..) = two_parts();
    let scripted = ScriptedFactory::new();
    let mut session = Session::new(factory(&scripted), AudioBackend::Null { sample_rate: SR }).unwrap();
    session
        .edit(|working| {
            *working = project.clone();
            Ok(())
        })
        .unwrap();
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mix.wav");
    let with_parts = RenderOptions { sections: sections(), ..options() };
    let written = session.render_job().render_to_file(&path, &with_parts).unwrap();
    let (song, revision) = (session.project().clone(), session.revision());
    assert_eq!(written.path.as_deref(), Some(path.to_str().unwrap()));
    assert!(written.stats.true_peak_db.is_some_and(|true_peak| true_peak >= written.stats.peak_db));

    std::fs::remove_file(&path).unwrap();
    let measured = session.render_job().measure(&with_parts).unwrap();
    assert_eq!(measured.path, None);
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0, "nothing was written");
    // The numbers are the ones the file's render reported.
    assert_eq!(measured.stats, written.stats);
    let loudness = |report: &trill_engine::RenderReport| -> Vec<f32> {
        report
            .tracks
            .iter()
            .map(|track| track.loudness_lufs)
            .chain(report.sections.iter().map(|s| s.loudness_lufs))
            .collect()
    };
    assert_eq!(loudness(&measured), loudness(&written));
    assert_eq!((session.project(), session.revision()), (&song, revision), "and the song is as it was");
}

#[test]
fn hearing_only_some_tracks_leaves_the_songs_own_mute_and_solo_alone() {
    let _quiet = beside_the_others();
    let (mut project, synth, drums) = two_parts();
    // The song has the snare soloed and the synth muted.
    project.track_mut(drums).unwrap().solo = true;
    project.track_mut(synth).unwrap().mute = true;
    let scripted = ScriptedFactory::new();
    let as_it_is = render(&project, factory(&scripted).as_ref(), &options()).unwrap();
    let peak_of = |out: &trill_engine::Rendered, track: TrackId| {
        db(out.track_peaks.iter().find(|(id, _)| *id == track).unwrap().1)
    };
    assert!(peak_of(&as_it_is, synth) < -100.0 && peak_of(&as_it_is, drums) > -40.0);

    // Asked for the synth alone, it is heard, muted or not, and the snare
    // is not, soloed or not.
    let only = RenderOptions { only_tracks: Some(vec![synth]), ..options() };
    let alone = render(&project, factory(&scripted).as_ref(), &only).unwrap();
    assert!(peak_of(&alone, synth) > -40.0 && peak_of(&alone, drums) < -100.0);
    assert!(project.track(drums).unwrap().solo && project.track(synth).unwrap().mute, "the song is as it was");

    // Both named: both heard.
    let only = RenderOptions { only_tracks: Some(vec![synth, drums]), ..options() };
    let both = render(&project, factory(&scripted).as_ref(), &only).unwrap();
    assert!(peak_of(&both, synth) > -40.0 && peak_of(&both, drums) > -40.0);
}

// ---- probing an instrument -----------------------------------------------------------

fn session_with(scripted: &ScriptedFactory) -> Session {
    Session::new(factory(scripted), AudioBackend::Null { sample_rate: SR }).unwrap()
}

fn plain(pitch: u8) -> ProbeNote {
    ProbeNote { pitch, velocity: 100, with: Vec::new() }
}

fn probe_options() -> ProbeOptions {
    ProbeOptions { sample_rate: SR, hold_seconds: 0.3, tail_seconds: 0.5, ..ProbeOptions::default() }
}

fn probe(session: &mut Session, target: ProbeTarget, notes: &[ProbeNote], options: &ProbeOptions) -> Vec<Heard> {
    session.probe_job(&target).unwrap().run(notes, options).unwrap().heard
}

#[test]
fn a_probe_finds_an_instruments_range_and_the_octave_it_really_sounds_at() {
    let _quiet = beside_the_others();
    let scripted = ScriptedFactory::loading(Loading { first: Duration::from_millis(300), ..Loading::default() });
    let mut session = session_with(&scripted);
    let target = ProbeTarget::Device { uri: BASS.to_string(), preset: None };
    // Every fourth note of the keyboard.
    let notes: Vec<ProbeNote> = (0..128).step_by(4).map(plain).collect();
    let job = session.probe_job(&target).unwrap();
    let probed = job.run(&notes, &probe_options()).unwrap();
    assert!(probed.answered && probed.waited_seconds >= 0.3, "it was waited for: {}", probed.waited_seconds);
    let sounding: Vec<u8> = probed.heard.iter().filter(|heard| heard.sounded).map(|heard| heard.note.pitch).collect();
    assert_eq!(sounding, [28, 32, 36, 40, 44, 48, 52, 56, 60, 64], "E1 to G4, and nothing outside");
    for heard in probed.heard.iter().filter(|heard| heard.sounded) {
        let pitch = heard.pitch.unwrap_or_else(|| panic!("{} has a pitch", heard.note.pitch));
        assert!(
            (pitch - (f64::from(heard.note.pitch) - 12.0)).abs() < 0.1,
            "an octave below {}: {pitch}",
            heard.note.pitch
        );
        assert!(heard.onset_seconds.unwrap() < 0.005 && heard.peak_db > -20.0, "{heard:?}");
    }
    let silent = &probed.heard[0];
    assert!(!silent.sounded && silent.pitch.is_none() && silent.peak_db < -100.0, "{silent:?}");

    // Started from a preset only the plugin can load, it sounds as written.
    let target = ProbeTarget::Device { uri: BASS.to_string(), preset: Some("Octave up".to_string()) };
    let heard = probe(&mut session, target, &[plain(40)], &probe_options());
    assert!((heard[0].pitch.unwrap() - 40.0).abs() < 0.1, "{:?}", heard[0].pitch);
}

#[test]
fn a_probe_shows_what_a_held_key_changes_about_a_note() {
    let _quiet = beside_the_others();
    let mut session = session_with(&ScriptedFactory::new());
    let target = ProbeTarget::Device { uri: BASS.to_string(), preset: None };
    let muted = ProbeNote { with: vec![MUTE_KEY], ..plain(40) };
    let other = ProbeNote { with: vec![14], ..plain(40) };
    let options = ProbeOptions { hold_seconds: 1.0, ..probe_options() };
    let heard = probe(&mut session, target, &[plain(40), muted, other, plain(40)], &options);
    let [open, muted, other, after] = heard.as_slice() else { panic!() };
    // Open, it rings for as long as it is held; palm-muted it is over in
    // a fifth of a second, and darker.
    assert!(open.rings_seconds > 0.9, "{open:?}");
    assert!(muted.rings_seconds < 0.3 && muted.sounded, "{muted:?}");
    assert!(muted.brightness_hz.unwrap() < open.brightness_hz.unwrap() * 0.8, "{muted:?} against {open:?}");
    assert!(!muted.with_sounded, "the key is a switch: it makes no sound of its own");
    // A key that switches nothing changes nothing, and the mute does not
    // outlast its key.
    for same in [other, after] {
        assert!((same.rings_seconds - open.rings_seconds).abs() < 0.05, "{same:?}");
        assert!((same.peak_db - open.peak_db).abs() < 0.5);
    }
    // A key that is itself a note is told apart from a switch.
    let target = ProbeTarget::Device { uri: BASS.to_string(), preset: None };
    let with_note = ProbeNote { with: vec![45], ..plain(40) };
    assert!(probe(&mut session, target, &[with_note], &probe_options())[0].with_sounded);
}

#[test]
fn a_probe_tells_a_kick_from_a_snare_from_a_cymbal() {
    let _quiet = beside_the_others();
    let mut session = session_with(&ScriptedFactory::new());
    let target = ProbeTarget::Device { uri: DRUMS.to_string(), preset: None };
    let notes: Vec<ProbeNote> = (35..=50).map(plain).collect();
    let options = ProbeOptions { hold_seconds: 0.1, tail_seconds: 3.0, ..probe_options() };
    let heard = probe(&mut session, target, &notes, &options);
    let sounding: Vec<u8> = heard.iter().filter(|heard| heard.sounded).map(|heard| heard.note.pitch).collect();
    assert_eq!(sounding, [36, 38, 40, 42, 49]);
    let of = |pitch: u8| heard.iter().find(|heard| heard.note.pitch == pitch).unwrap();
    let (kick, snare, hat, crash) = (of(36), of(38), of(42), of(49));
    let bright = |heard: &Heard| heard.brightness_hz.unwrap();
    // A low thump, a noisy hit in the middle, a short bright tick and a
    // long bright wash.
    assert!(bright(kick) < 150.0 && (kick.pitch.unwrap() - 33.0).abs() < 0.5, "{kick:?}");
    assert!(bright(snare) > 1_000.0 && bright(snare) < 8_000.0 && snare.pitch.is_none(), "{snare:?}");
    assert!(bright(hat) > 10_000.0 && hat.rings_seconds < 0.15, "{hat:?}");
    assert!(bright(crash) > 10_000.0 && crash.rings_seconds > 1.0 && !crash.still_ringing, "{crash:?}");
    assert!(bright(snare) < bright(hat) / 2.0);
    // The two snares measure alike.
    assert!((of(40).rings_seconds - snare.rings_seconds).abs() < 0.05);
    assert!((bright(of(40)) / bright(snare) - 1.0).abs() < 0.2);
}

#[test]
fn a_probe_plays_a_track_as_it_is_and_changes_nothing_in_the_song() {
    let _quiet = beside_the_others();
    let scripted = ScriptedFactory::new();
    let mut session = session_with(&scripted);
    let track = session.edit(|project| Ok(track_with(project, "Bass", BASS, &[note(40, 0.0, 1.0)]))).unwrap();
    let device = session.project().track(track).unwrap().devices[0].id;
    session.load_preset(device, "Soft").unwrap();
    let reverb = session.add_device(ChainRef::Track(track), "builtin:reverb", None, None).unwrap();
    session
        .edit(|project| {
            let params = &mut project.device_mut(reverb)?.params;
            params.insert("mix".to_string(), 0.5);
            params.insert("decay".to_string(), 3.0);
            Ok(())
        })
        .unwrap();
    // (What the running plugin says its state is has been written down,
    // as a render or a save writes it down.)
    session.render_job();
    let undo_steps = |session: &mut Session| {
        let mut steps = 0;
        while session.undo() {
            steps += 1;
        }
        for _ in 0..steps {
            session.redo();
        }
        steps
    };
    let steps = undo_steps(&mut session);
    let (before, revision) = (session.project().clone(), session.revision());
    let made = scripted.made().len();

    // The instrument alone, as it is now: with the preset it was given.
    let alone = probe(&mut session, ProbeTarget::Track { track, effects: false }, &[plain(40)], &probe_options());
    let fresh = probe(
        &mut session,
        ProbeTarget::Device { uri: BASS.to_string(), preset: None },
        &[plain(40)],
        &probe_options(),
    );
    assert!(
        (fresh[0].peak_db - alone[0].peak_db - 6.0).abs() < 0.5,
        "half as loud: {} against {}",
        alone[0].peak_db,
        fresh[0].peak_db
    );
    assert_eq!(scripted.made()[made], (BASS.to_string(), Purpose::Rendering), "a copy of its own, not the one playing");

    // Through the effects after it, the note rings on in the reverb.
    let through = probe(&mut session, ProbeTarget::Track { track, effects: true }, &[plain(40)], &probe_options());
    assert!(
        through[0].rings_seconds > alone[0].rings_seconds + 0.2,
        "{} then {}",
        alone[0].rings_seconds,
        through[0].rings_seconds
    );

    assert_eq!((session.project(), session.revision()), (&before, revision), "the song is as it was");
    assert_eq!(undo_steps(&mut session), steps, "and nothing was done to undo");

    // The sound of the track saved as a preset is what a probe of the
    // bare instrument started from that preset plays: half as loud.
    session.saved_presets().open(tempfile::tempdir().unwrap().keep());
    session.save_preset(device, "Quiet take", "", &[]).unwrap();
    let saved = ProbeTarget::Device { uri: BASS.to_string(), preset: Some("Quiet take".to_string()) };
    let from_saved = probe(&mut session, saved, &[plain(40)], &probe_options());
    assert!(
        (from_saved[0].peak_db - alone[0].peak_db).abs() < 0.1,
        "{} against {}",
        from_saved[0].peak_db,
        alone[0].peak_db
    );

    // What cannot be probed says why.
    let empty = session.edit(|project| Ok(project.add_track("Empty"))).unwrap();
    let error = session.probe_job(&ProbeTarget::Track { track: empty, effects: false }).err().unwrap().to_string();
    assert!(error.contains("has no instrument to probe"), "{error}");
    let effect = ProbeTarget::Device { uri: "builtin:reverb".to_string(), preset: None };
    assert!(session.probe_job(&effect).err().unwrap().to_string().contains("is an effect"));
    let unknown = ProbeTarget::Device { uri: "clap:nothing".to_string(), preset: None };
    assert!(session.probe_job(&unknown).err().unwrap().to_string().contains("there is no device 'clap:nothing'"));
    let preset = ProbeTarget::Device { uri: BASS.to_string(), preset: Some("Loud".to_string()) };
    let error = session.probe_job(&preset).err().unwrap().to_string();
    assert!(error.contains("no preset 'Loud'") && error.contains("Octave up"), "{error}");
}

#[test]
fn a_built_in_instrument_is_probed_through_its_preset_and_with_controllers() {
    let _quiet = beside_the_others();
    let mut session = session_with(&ScriptedFactory::new());
    let target =
        |preset: &str| ProbeTarget::Device { uri: "builtin:polysynth".to_string(), preset: Some(preset.to_string()) };
    let heard = probe(&mut session, target("bass"), &[plain(36), plain(48)], &probe_options());
    assert!(heard.iter().all(|heard| heard.sounded));
    assert!((heard[1].pitch.unwrap() - heard[0].pitch.unwrap() - 12.0).abs() < 0.2, "an octave apart: {heard:?}");

    // With the pitch wheel fully up, what is heard is two semitones up;
    // with expression at nothing, nothing.
    let bent = ProbeOptions { controllers: vec![(trill_core::ControllerKind::PitchBend, 1.0)], ..probe_options() };
    let up = probe(&mut session, target("bass"), &[plain(36)], &bent);
    assert!((up[0].pitch.unwrap() - heard[0].pitch.unwrap() - 2.0).abs() < 0.2, "{:?}", up[0].pitch);
    let breathless =
        ProbeOptions { controllers: vec![(trill_core::ControllerKind::EXPRESSION, 0.0)], ..probe_options() };
    assert!(!probe(&mut session, target("bass"), &[plain(36)], &breathless)[0].sounded);
}
