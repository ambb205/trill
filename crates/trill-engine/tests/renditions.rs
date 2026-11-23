//! A track heard where its plugin is not: one session has the instrument,
//! the other has not, and plays what the first recorded of it in its
//! place.
//!
//! No wire here, and nobody makes a rendition by itself: the test does
//! what the runtime does (`trill-runtime/src/renditions.rs`), a step at a
//! time, and hands what one session says to the other.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::files::{Content, FileIs, FilePlace};
use trill_core::merge::lot::note;
use trill_core::merge::{Change, Op, Said, Stamp, TrackOp, TrackSet};
use trill_core::{ChainRef, ClipId, DeviceId, Rendition, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::scripted::{BASS, ScriptedFactory};
use trill_engine::{After, AudioBackend, Problem, RenderOptions, Session};

/// Ada, who has the bass: a plugin as far as the song is concerned.
fn with_the_bass() -> Session {
    let factory: Arc<dyn DeviceFactory> =
        Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(ScriptedFactory::new())]));
    let mut session = Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    session.hear_changes(true);
    session
}

/// Ben, who has not.
fn without_it() -> Session {
    let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    session.hear_changes(true);
    session
}

struct Song {
    ada: Session,
    ben: Session,
    bass: TrackId,
    line: ClipId,
    /// The instrument only Ada has, and the effect after it that both have.
    plugin: DeviceId,
    eq: DeviceId,
    _folder: tempfile::TempDir,
}

/// A song with a bass line through a plugin and an equaliser, and keys
/// beside it: Ada's, shared, and handed to Ben.
fn song() -> Song {
    let folder = tempfile::tempdir().unwrap();
    let (mut ada, mut ben) = (with_the_bass(), without_it());
    ada.keep_received_in(Some(folder.path().join("at Ada's")));
    ben.keep_received_in(Some(folder.path().join("at Ben's")));
    let bass = ada.edit(|song| Ok(song.add_track("Bass"))).unwrap();
    let keys = ada.edit(|song| Ok(song.add_track("Keys"))).unwrap();
    ada.edit(|song| Ok(song.add_track_of_kind("Room", TrackKind::Return))).unwrap();
    let plugin = ada.add_device(ChainRef::Track(bass), BASS, None, None).unwrap();
    let eq = ada.add_device(ChainRef::Track(bass), "builtin:eq", None, None).unwrap();
    ada.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
    let line = ada.edit(|song| song.add_midi_clip(bass, "Line", 0.0, 8.0)).unwrap();
    ada.edit(|song| song.clip_mut(line)?.add_notes(&[note(40, 0.0), note(43, 2.0), note(47, 4.0)])).unwrap();
    ada.share();
    ben.take_song(&ada.whole_song()).unwrap();
    ada.take_said();
    assert_eq!(text(&ada), text(&ben), "the one who joined has the song");
    Song { ada, ben, bass, line, plugin, eq, _folder: folder }
}

fn text(session: &Session) -> String {
    let mut song = session.said_song();
    song.count_ids_from(0);
    song.to_json()
}

fn pass(from: &mut Session, to: &mut Session, by: &str) {
    let said = from.take_said();
    let floors = from.floors();
    to.merge(by, said, Some(&floors), from.replica()).expect("what a session says is taken");
}

/// The bass alone, as a session plays it: its loudest sample.
fn bass_alone(session: &mut Session, bass: TrackId) -> f32 {
    bass_between(session, bass, 0.0, None)
}

/// The bass alone over a stretch of the song, in beats: its loudest
/// sample there, without what rings on after it.
fn bass_between(session: &mut Session, bass: TrackId, from: f64, to: Option<f64>) -> f32 {
    let tail = if to.is_some() { 0.0 } else { 0.5 };
    let options = RenderOptions {
        start_beat: from,
        end_beat: to,
        only_tracks: Some(vec![bass]),
        max_tail_seconds: tail,
        ..RenderOptions::default()
    };
    let rendered = session.render_job().render(&options).expect("the song renders");
    rendered.left.iter().chain(&rendered.right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()))
}

/// What the runtime does for a rendition that is wanted: the track is
/// recorded as it leaves `after`, the recording is put where the song's
/// files lie under the name of what it is, and the session takes it in.
/// (What it is, is said by its length and a number here: nothing in the
/// session reads a file to hold it against a hash.)
fn make(session: &mut Session, track: TrackId, after: DeviceId, devices: usize, number: u8) -> (Rendition, PathBuf) {
    let print = session.rendition_prints(&[(track, after)]).pop().flatten().expect("the track has that device");
    let folder = session.received_in().expect("the song is one others hold too");
    let part = folder.with_extension("making").join(format!("{number}.wav"));
    // (Of the stretch the track plays anything in, as the runtime does.)
    let wanted = session.renditions_wanted(&HashSet::from([after]));
    let (from, to) = wanted.iter().find(|wanted| wanted.track == track).expect("it plays something").stretch;
    let options = RenderOptions {
        start_beat: from,
        end_beat: Some(to),
        only_tracks: Some(vec![track]),
        after: Some(After { track, devices, file: part.clone() }),
        ..RenderOptions::default()
    };
    session.render_job().measure(&options).expect("the track is recorded");
    let bytes = std::fs::metadata(&part).expect("the recording was written").len();
    let is = FileIs::new(Content::new(&[number; 32], bytes), "Bass rendition.wav");
    let home = PathBuf::from(session.home_of(&is).unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::rename(&part, &home).unwrap();
    let rendition = Rendition { after, of: print, is, by: "Ada".into(), from };
    session.land_rendition(track, rendition.clone()).expect("the rendition is taken in");
    (rendition, home)
}

/// The recording of a rendition arrives at `session`, as the runtime
/// brings it: under the name of what it is, in the song's folder.
fn arrives(session: &mut Session, rendition: &Rendition, from: &Path) -> PathBuf {
    let home = PathBuf::from(session.home_of(&rendition.is).unwrap());
    std::fs::create_dir_all(home.parent().unwrap()).unwrap();
    std::fs::copy(from, &home).unwrap();
    session.files_are_here(std::slice::from_ref(&home));
    home
}

#[test]
fn a_track_whose_plugin_is_not_here_plays_what_another_recorded_of_it_through_its_own_effects_and_fader() {
    let Song { mut ada, mut ben, bass, plugin, eq, .. } = song();

    // Ben has not got the plugin: it is in his song, and makes no sound.
    assert_eq!((ada.lacking(), ben.lacking()), (Vec::new(), vec![plugin]));
    assert!(
        ben.problems().iter().any(|problem| matches!(problem, Problem::Device { device, .. } if *device == plugin))
    );
    let hers = bass_alone(&mut ada, bass);
    assert!(hers > 0.05, "Ada hears her bass: {hers}");
    assert_eq!(bass_alone(&mut ben, bass), 0.0, "and Ben nothing of it");
    assert!(ben.renditions().is_empty() && ada.renditions().is_empty());

    // What is wanted: the bass after the plugin, by whoever has the plugin.
    let wanted = ada.renditions_wanted(&HashSet::from([plugin]));
    assert_eq!(wanted.len(), 1);
    assert_eq!(
        (wanted[0].track, wanted[0].after, wanted[0].devices, &wanted[0].takes),
        (bass, plugin, 1, &vec![plugin])
    );
    assert_eq!(wanted[0].stretch, (0.0, 8.0), "what is recorded is where the track plays anything");
    assert!(ada.renditions_wanted(&HashSet::new()).is_empty(), "where nobody lacks anything, nothing is");
    assert!(ada.renditions_wanted(&HashSet::from([eq])).iter().all(|wanted| wanted.devices == 2));

    // Ada's program makes it. It is no step of hers, and she goes on
    // playing her plugin.
    let (rendition, at_adas) = make(&mut ada, bass, plugin, 1, 1);
    assert_eq!(ada.project().track(bass).unwrap().rendition, Some(rendition.clone()));
    let state = &ada.renditions()[0];
    assert!(state.current && !state.played && state.lacking.is_empty(), "{state:?}");
    assert!((bass_alone(&mut ada, bass) - hers).abs() < 1e-6, "she hears what she heard");
    // Nobody's edit: taking a step back takes back what she did last, the
    // bass line's notes, and the rendition is where it was.
    assert!(ada.undo());
    assert!(ada.project().track(bass).unwrap().rendition.is_some());
    assert!(ada.redo());
    ada.take_said();
    ada.land_rendition(bass, rendition.clone()).unwrap();
    // She hands the recording to whoever asks, and wants nothing herself.
    let of_adas = ada.files_here().into_iter().find(|file| file.place == FilePlace::Rendition(bass)).unwrap();
    assert_eq!((Path::new(&of_adas.path), of_adas.may_go, of_adas.wanted), (at_adas.as_path(), true, false));

    // Ben hears of it. Its recording is on its way: no problem, no
    // missing file, and until it is here the track is as it was.
    pass(&mut ada, &mut ben, "Ada");
    assert_eq!(text(&ada), text(&ben));
    assert_eq!(ben.project().track(bass).unwrap().rendition, Some(rendition.clone()));
    assert!(ben.files_arriving().contains(&FilePlace::Rendition(bass)), "{:?}", ben.files_arriving());
    assert!(!ben.problems().iter().any(|problem| matches!(problem, Problem::Rendition { .. })), "{:?}", ben.problems());
    let of_bens = ben.files_here().into_iter().find(|file| file.place == FilePlace::Rendition(bass)).unwrap();
    assert!(of_bens.wanted && of_bens.is == Some(rendition.is.clone()), "{of_bens:?}");
    assert_ne!(of_bens.path, of_adas.path, "each has its own place for it");
    let state = &ben.renditions()[0];
    assert!(state.current && !state.played && state.lacking == [plugin] && state.by == "Ada", "{state:?}");
    assert_eq!(bass_alone(&mut ben, bass), 0.0);

    // It arrives: Ben hears the bass as Ada does, through his own
    // equaliser and at his own fader.
    arrives(&mut ben, &rendition, &at_adas);
    assert!(ben.files_arriving().is_empty());
    let state = &ben.renditions()[0];
    assert!(state.played && state.as_the_song_is(), "{state:?}");
    let his = bass_alone(&mut ben, bass);
    assert!((his - hers).abs() < 0.002, "Ben hears {his} where Ada hears {hers}");
    // The plugin is still not his, and is said so.
    assert_eq!(ben.lacking(), vec![plugin]);

    // His fader is his to move, and is heard at once: the recording is of
    // the track ahead of it, so it is still of the song as it is.
    ben.edit(|song| song.set_track_volume(bass, -6.0)).unwrap();
    let quieter = bass_alone(&mut ben, bass);
    assert!((quieter - his * 0.501).abs() < 0.01, "six decibels under {his}: {quieter}");
    assert!(ben.renditions()[0].as_the_song_is());
    // And his equaliser, which comes after the plugin.
    let band = ben.device_params(eq).unwrap().into_iter().find(|param| param.info.id.contains("gain")).unwrap();
    ben.set_device_params(eq, &[(band.info.id.clone(), band.info.min)]).unwrap();
    assert!(ben.renditions()[0].as_the_song_is(), "what comes after the plugin is not in the recording");
    pass(&mut ben, &mut ada, "Ben");
    assert_eq!(text(&ada), text(&ben));
    assert!(ada.renditions()[0].current, "nor is the recording of the song as it was for Ada");
}

#[test]
fn a_change_to_what_was_recorded_is_heard_when_it_is_recorded_again_and_the_old_recording_plays_until_then() {
    let Song { mut ada, mut ben, bass, line, plugin, .. } = song();
    let (first, at_adas) = make(&mut ada, bass, plugin, 1, 1);
    pass(&mut ada, &mut ben, "Ada");
    let at_bens = arrives(&mut ben, &first, &at_adas);
    let before = bass_alone(&mut ben, bass);
    assert!(before > 0.05);

    // Ben writes a note into the bass line. His track goes on playing
    // what was recorded, which is of the song as it was: said so.
    ben.edit(|song| song.clip_mut(line)?.add_notes(&[note(52, 6.0)])).unwrap();
    let state = &ben.renditions()[0];
    assert!(state.played && !state.current && !state.as_the_song_is(), "{state:?}");
    assert!((bass_alone(&mut ben, bass) - before).abs() < 1e-6);

    // Ada's program hears of the note and records the track again.
    pass(&mut ben, &mut ada, "Ben");
    assert!(!ada.renditions()[0].current);
    let (second, newer_at_adas) = make(&mut ada, bass, plugin, 1, 2);
    assert_ne!(second.of, first.of);
    assert!(!at_adas.exists(), "the recording before it was her program's making, and is gone");
    assert!(ada.renditions()[0].current);

    // Ben hears of the newer one before he has it: the one he has plays
    // on, and nothing falls silent.
    pass(&mut ada, &mut ben, "Ada");
    let state = &ben.renditions()[0];
    assert!(state.played && state.current && !state.newest, "{state:?}");
    assert_eq!(ben.project().track(bass).unwrap().rendition_here(), Some(at_bens.to_string_lossy().as_ref()));
    assert!((bass_alone(&mut ben, bass) - before).abs() < 1e-6);
    let wanted = ben.files_here().into_iter().find(|file| file.place == FilePlace::Rendition(bass)).unwrap();
    assert!(wanted.wanted && wanted.is == Some(second.is.clone()) && wanted.path != at_bens.to_string_lossy());

    // It arrives: that is what plays, and the one before it is removed.
    let newer_at_bens = arrives(&mut ben, &second, &newer_at_adas);
    assert!(ben.renditions()[0].as_the_song_is());
    assert_eq!(ben.project().track(bass).unwrap().rendition_here(), Some(newer_at_bens.to_string_lossy().as_ref()));
    assert!(!at_bens.exists() && newer_at_bens.exists());
    assert!(bass_alone(&mut ben, bass) > 0.05);
    assert_eq!(text(&ada), text(&ben), "which recording each plays is each one's own, and no part of the song");

    // The plugin goes from the track: the recording stands in for nothing.
    ada.edit(|song| song.remove_device(plugin).map(drop)).unwrap();
    pass(&mut ada, &mut ben, "Ada");
    assert!(ben.renditions().is_empty() && ben.lacking().is_empty());
    assert_eq!(bass_alone(&mut ben, bass), 0.0, "a bass line with no instrument");
}

#[test]
fn a_recording_begins_where_the_track_first_plays_and_is_heard_there() {
    let Song { mut ada, mut ben, bass, line, plugin, .. } = song();
    // The bass comes in at the fifth bar, and plays for two.
    ada.edit(|song| song.move_clip(line, None, 16.0)).unwrap();
    pass(&mut ada, &mut ben, "Ada");
    let hers = bass_between(&mut ada, bass, 16.0, Some(24.0));
    assert!(hers > 0.05 && bass_between(&mut ada, bass, 0.0, Some(16.0)) == 0.0);

    let (rendition, at_adas) = make(&mut ada, bass, plugin, 1, 1);
    assert_eq!(rendition.from, 16.0);
    // Two bars at 120 and what rings on after them, not the four silent
    // bars before: under three seconds of sound a second.
    let seconds = rendition.is.bytes as f64 / (48_000.0 * 4.0);
    assert!((4.0..7.0).contains(&seconds), "{seconds} s were recorded");
    pass(&mut ada, &mut ben, "Ada");
    arrives(&mut ben, &rendition, &at_adas);
    assert!(ben.renditions()[0].as_the_song_is());
    assert_eq!(bass_between(&mut ben, bass, 0.0, Some(16.0)), 0.0, "nothing before the bass comes in");
    let his = bass_between(&mut ben, bass, 16.0, Some(24.0));
    assert!((his - hers).abs() < 0.002, "Ben hears {his} where Ada hears {hers}");
    // From a place in the middle of it too.
    let (hers, his) = (bass_between(&mut ada, bass, 18.0, Some(20.0)), bass_between(&mut ben, bass, 18.0, Some(20.0)));
    assert!(hers > 0.05 && (his - hers).abs() < 0.002, "from the middle: {his} against {hers}");
}

#[test]
fn which_recording_a_machine_plays_is_its_own_and_is_taken_from_nobody() {
    let Song { mut ada, mut ben, bass, plugin, .. } = song();
    let (rendition, _) = make(&mut ada, bass, plugin, 1, 1);
    // What Ada says of it names no place of hers.
    let said = ada.take_said();
    let written = serde_json::to_string(&said).unwrap();
    assert!(written.contains("rendition") && !written.contains("at Ada's") && !written.contains("rendition_at"));
    let floors = ada.floors();
    ben.merge("Ada", said, Some(&floors), ada.replica()).unwrap();

    // Someone who says where Ben is to find it is refused, whole.
    let before = text(&ben);
    let place = Op::Track(bass, TrackOp::Set(TrackSet::RenditionAt("C:/Users/Ben/Documents/voice memo.wav".into())));
    let stranger = Said { change: Change { stamp: Stamp(999, 7), ops: vec![place] }, knew: None };
    assert!(ben.merge("Mallory", vec![stranger], None, None).is_err());
    assert_eq!(text(&ben), before);
    let home = ben.home_of(&rendition.is).unwrap();
    assert_eq!(ben.project().track(bass).unwrap().rendition_here(), Some(home.as_str()), "the place is one made here");

    // Nor is a rendition taken after a device that is none, or of a file
    // that is said by something that is no hash.
    let mut wrong = rendition.clone();
    wrong.is.sha256 = "../../somewhere".into();
    let op = Op::Track(bass, TrackOp::Set(TrackSet::Rendition(Some(wrong))));
    let stranger = Said { change: Change { stamp: Stamp(1000, 7), ops: vec![op] }, knew: None };
    assert!(ben.merge("Mallory", vec![stranger], None, None).is_err());
    assert_eq!(text(&ben), before);

    // A group has none: what it plays is what its members play.
    let group = ada.edit(|song| Ok(song.add_track_of_kind("Low end", TrackKind::Group))).unwrap();
    let on_it = ada.add_device(ChainRef::Track(group), BASS, None, None).unwrap();
    assert!(ada.renditions_wanted(&HashSet::from([on_it])).is_empty());
    let mut of_the_group = rendition.clone();
    of_the_group.after = on_it;
    let refused = ada.land_rendition(group, of_the_group).unwrap_err();
    assert!(refused.contains("is a group"), "{refused}");
}
