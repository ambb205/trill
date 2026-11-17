//! The sampled instrument through the session: an instrument given to a
//! device by the path of its file, what the session then knows of it, its
//! files when they are missing, and loading.
//!
//! The instruments are made up here, in a temporary folder: recordings
//! that are tones, and a file that says where each plays. What the device
//! does with notes is held to numbers in `instruments.rs`.

mod common;

use common::*;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::{ChainRef, DeviceId, Drum, FilePlace, Note, Relink, Role, TrackId};
use trill_dsp::Builtins;
use trill_dsp::sampled::{self, State};
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Problem, Session};
use trill_media::MediaCache;

/// A session wired the way the application wires one, without the master
/// limiter.
fn session() -> Session {
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).unwrap();
    session
}

fn render(session: &mut Session) -> Vec<f32> {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    session.render_job().render(&options).unwrap().left
}

fn key_hz(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// A small piano in `folder`: two recordings an octave apart, each soft
/// and loud, three seconds long, and a thump for a key coming up.
fn piano(folder: &Path) -> String {
    let tone = |key: u8, level: f64| sine(key_hz(key), 44_100.0, 3.0, level);
    write_instrument(
        folder,
        "Small Grand",
        "<control> default_path=samples/ label_cc64=Sustain\n<global> ampeg_release=0.3 eq1_gain=2\n\
         <group> lovel=1 hivel=80\n<region> sample=c3_p.wav lokey=36 hikey=59 pitch_keycenter=48\n\
         <region> sample=c5_p.wav lokey=60 hikey=96 pitch_keycenter=72\n\
         <group> lovel=81 hivel=127\n<region> sample=c3_f.wav lokey=36 hikey=59 pitch_keycenter=48\n\
         <region> sample=c5_f.wav lokey=60 hikey=96 pitch_keycenter=72\n\
         <group> trigger=release volume=-20 <region> sample=thump.wav lokey=36 hikey=96 pitch_keytrack=0",
        &[
            ("samples/c3_p.wav", 44_100.0, tone(48, 0.2)),
            ("samples/c3_f.wav", 44_100.0, tone(48, 0.5)),
            ("samples/c5_p.wav", 44_100.0, tone(72, 0.2)),
            ("samples/c5_f.wav", 44_100.0, tone(72, 0.5)),
            ("samples/thump.wav", 44_100.0, sine(90.0, 44_100.0, 0.05, 0.3)),
        ],
    )
}

/// A track that plays `instrument`, with `notes` in a clip at its start.
fn played(session: &mut Session, name: &str, instrument: &str, notes: &[Note]) -> (TrackId, DeviceId) {
    let track = session.edit(|project| Ok(project.add_track(name))).unwrap();
    let device = session.add_device(ChainRef::Track(track), sampled::URI, Some(instrument), None).unwrap();
    session.set_device_params(device, &[("volume".into(), 0.0)]).unwrap();
    if !notes.is_empty() {
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "Part", 0.0, 8.0)?;
                project.clip_mut(clip)?.add_notes(notes)
            })
            .unwrap();
    }
    (track, device)
}

fn note(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let begun = Instant::now();
    while !done() {
        assert!(begun.elapsed() < Duration::from_secs(20), "{what} did not happen");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn an_instrument_is_given_to_a_device_by_the_path_of_its_file_and_says_what_it_is() {
    let folder = folder();
    let grand = piano(folder.path());
    let mut session = session();
    let (track, device) = played(&mut session, "Keys", &grand, &[note(60, 0.0, 2.0, 100), note(48, 2.0, 2.0, 40)]);

    // The device's state names the file, and the device is on that preset.
    let state = State::from_device_state(session.project().device(device).unwrap().state.as_deref()).unwrap();
    assert_eq!(state.path.as_deref(), Some(grand.as_str()));
    assert_eq!(session.device_preset(device).map(|preset| preset.name.as_str()), Some("Small Grand"));
    assert!(session.problems().is_empty(), "{:?}", session.problems());

    // What it is, for whoever writes for it.
    let said = session.sampled(device).unwrap();
    assert_eq!((said.name.as_str(), said.recordings, said.layers, said.takes), ("Small Grand", 5, 2, 1));
    assert_eq!(said.regions.len(), 5);
    assert!(said.bytes > 5 * 100_000, "{}", said.bytes);
    assert!(said.about.starts_with("Small Grand: 5 recordings ("), "{}", said.about);
    assert!(said.about.contains("range C2..C7 (36..96), 2 layers of loudness"), "{}", said.about);
    assert!(said.about.contains("The sustain pedal holds its notes; it has sounds of its own for a key coming up"));
    let unsupported = said.unsupported.as_deref().unwrap();
    assert!(unsupported.contains("equalisers of its own (eq1_gain)") && unsupported.ends_with("Everything else plays."));
    // The track's notes are the instrument's: its range, read from the file and kept nowhere in the song.
    assert_eq!(session.profile_of(track).range, Some((36, 96)));
    assert!(session.project().track(track).unwrap().profile.is_empty());
    assert!(!session.plays_drums(track));
    assert_eq!(session.sampled_of(track).map(|said| said.name.clone()), Some("Small Grand".to_string()));

    // It reads the starts of its recordings in the background, and says so until it has.
    wait_until("the starts being read", || session.loading().is_empty());
    let reading = session.reading(device).unwrap();
    assert_eq!((reading.ready, reading.of, reading.is_loading()), (5, 5, false));

    // And it plays: the loud layer at its level, the soft one at its, each at its key.
    let out = render(&mut session);
    // (The second from where the first has died away: it is the softer by far.)
    let (first, second) = (&out[4_800..43_200], &out[72_000..91_200]);
    assert!((zero_crossing_hz(first, SR) - key_hz(60)).abs() < 0.05 && (zero_crossing_hz(second, SR) - key_hz(48)).abs() < 0.05);
    let by_velocity = |velocity: f32| (velocity / 127.0).powi(2);
    assert!((rms(first) - 0.5 / 2f32.sqrt() * by_velocity(100.0)).abs() < 0.003, "{}", rms(first));
    assert!((rms(second) - 0.2 / 2f32.sqrt() * by_velocity(40.0)).abs() < 0.001, "{}", rms(second));

    // Another instrument takes its place as one step, which can be taken back.
    let kit = write_instrument(
        folder.path(),
        "Tiny Kit",
        "<global> loop_mode=one_shot\n<region> key=36 sample=kit/Kick.wav\n<region> key=38 sample=kit/Snare.wav",
        &[("kit/Kick.wav", 44_100.0, sine(60.0, 44_100.0, 0.2, 0.5)), ("kit/Snare.wav", 44_100.0, sine(200.0, 44_100.0, 0.2, 0.5))],
    );
    assert_eq!(session.load_preset(device, &kit).unwrap().name, "Tiny Kit");
    assert_eq!(session.sampled(device).unwrap().name, "Tiny Kit");
    assert!(session.plays_drums(track));
    assert!(session.undo());
    assert_eq!(session.sampled(device).unwrap().name, "Small Grand");

    // What is no instrument says why, and changes nothing.
    let missing = folder.path().join("Gone.sfz").display().to_string();
    let error = session.load_preset(device, &missing).unwrap_err().to_string();
    assert!(error.contains("cannot be given to 'builtin:sampled' as a preset") && error.contains("there is no such file"), "{error}");
    let recording = folder.path().join("samples").join("thump.wav").display().to_string();
    let error = session.load_preset(device, &recording).unwrap_err().to_string();
    assert!(error.contains("it reads SFZ files"), "{error}");
    assert_eq!(session.sampled(device).unwrap().name, "Small Grand");
    let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
    let error = session.load_preset(synth, &grand).unwrap_err().to_string();
    assert!(error.contains("takes no preset files"), "{error}");
}

#[test]
fn a_kit_gives_its_track_the_kits_drums_and_an_instrument_with_switches_its_ways_of_playing() {
    let folder = folder();
    let hit = |hz: f64| sine(hz, 44_100.0, 0.2, 0.5);
    let kit = write_instrument(
        folder.path(),
        "Room Kit",
        "<control> label_key49=Crash 16\n<global> loop_mode=one_shot\n<region> key=36 sample=kit/Kick_01.wav\n\
         <region> key=40 sample=kit/Snare center.wav\n<region> key=41 sample=kit/snare-rimshot.wav\n\
         <region> key=42 sample=kit/HH closed.wav\n<region> key=46 sample=kit/HH open.wav\n\
         <region> key=47 sample=kit/Tom 1.wav\n<region> key=49 sample=kit/cym_a.wav\n<region> key=70 sample=kit/Cowbell.wav",
        &[
            ("kit/Kick_01.wav", 44_100.0, hit(60.0)),
            ("kit/Snare center.wav", 44_100.0, hit(200.0)),
            ("kit/snare-rimshot.wav", 44_100.0, hit(240.0)),
            ("kit/HH closed.wav", 44_100.0, hit(6_000.0)),
            ("kit/HH open.wav", 44_100.0, hit(5_000.0)),
            ("kit/Tom 1.wav", 44_100.0, hit(140.0)),
            ("kit/cym_a.wav", 44_100.0, hit(4_000.0)),
            ("kit/Cowbell.wav", 44_100.0, hit(800.0)),
        ],
    );
    let mut session = session();
    let (track, device) = played(&mut session, "Drums", &kit, &[]);
    assert!(session.sampled(device).unwrap().about.contains("Its keys are drums"));
    assert_eq!(session.profile_of(track).role, Some(Role::Drums));
    assert!(session.plays_drums(track) && session.instrument_plays_drums(track));
    // The drums are where this kit has them, not where General MIDI does.
    let profile = session.profile_of(track);
    let key = |name: &str| profile.drum(name).map(|drum| drum.pitch);
    assert_eq!((key("kick"), key("snare"), key("rimshot")), (Some(36), Some(40), Some(41)));
    assert_eq!((key("closed_hat"), key("open_hat"), key("crash"), key("cowbell")), (Some(42), Some(46), Some(49), Some(70)));
    assert_eq!(key("high_tom"), Some(47));
    // And a drum the kit has not got is said to be lacking, not played on whatever is on General MIDI's key.
    assert_eq!(profile.find_drum("ride"), Drum::Lacking("ride".to_string()));

    let tone = |level: f64| sine(196.0, 44_100.0, 1.0, level);
    let strings = write_instrument(
        folder.path(),
        "Section",
        "<global> sw_lokey=c1 sw_hikey=d1 sw_default=c1 lokey=c2 hikey=c6 pitch_keycenter=g3 bend_up=1200 bend_down=-1200\n\
         <group> sw_last=c1 sw_label=Sustain <region> sample=s/sus.wav\n\
         <group> sw_last=c#1 sw_label=Pizzicato <region> sample=s/pizz.wav\n\
         <group> sw_last=d1 sw_label=Tremolo <region> sample=s/trem.wav",
        &[("s/sus.wav", 44_100.0, tone(0.1)), ("s/pizz.wav", 44_100.0, tone(0.2)), ("s/trem.wav", 44_100.0, tone(0.4))],
    );
    let (track, _) = played(&mut session, "Strings", &strings, &[]);
    let profile = session.profile_of(track);
    assert_eq!(profile.articulation_names(), ["sustain", "pizzicato", "tremolo"]);
    assert_eq!(profile.default_articulation().map(|way| (way.name.as_str(), way.key)), Some(("sustain", Some(24))));
    assert_eq!(profile.articulation("pizzicato").and_then(|way| way.key), Some(25));
    assert_eq!((profile.range, profile.bend_semitones()), (Some((36, 84)), 12.0));
    assert!(!session.plays_drums(track));
    // What the track says itself goes before what the instrument does.
    session
        .edit(|project| {
            let mut own = project.track(track)?.profile.clone();
            own.range = Some((40, 60));
            project.set_track_profile(track, own)
        })
        .unwrap();
    assert_eq!(session.profile_of(track).range, Some((40, 60)));
    assert_eq!(session.profile_of(track).articulation_names().len(), 3);
}

#[test]
fn a_recording_that_is_missing_is_the_songs_to_find_and_an_instrument_that_moved_takes_its_recordings_along() {
    let folder = folder();
    let grand = piano(folder.path());
    let samples = folder.path().join("samples");
    // One of its recordings is gone before the song is made.
    let elsewhere = folder.path().join("found").join("c5 loud.wav");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    std::fs::rename(samples.join("c5_f.wav"), &elsewhere).unwrap();

    let mut session = session();
    let (_, device) = played(&mut session, "Keys", &grand, &[note(72, 0.0, 2.0, 120)]);
    // It is said once, as a sentence, and the instrument plays without it.
    let gone = samples.join("c5_f.wav").display().to_string();
    let problems: Vec<&str> = session
        .problems()
        .iter()
        .map(|problem| match problem {
            Problem::Sample { reason, .. } => reason.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(problems, [format!("Small Grand: its recording '{gone}' is missing; the notes it plays are silent")]);
    assert_eq!(peak(&render(&mut session)[..48_000]), 0.0);
    // The song names the instrument's file, and the recording that is not where that file says.
    let named: Vec<(String, String)> = session.files().into_iter().map(|file| (file.place.to_string(), file.path)).collect();
    assert_eq!(
        named,
        [
            (format!("instrument of {device}"), grand.clone()),
            (format!("recording samples/c5_f.wav of {device}"), gone.clone()),
        ]
    );

    // Given the file where it is now, the recording plays, in one step.
    let place = FilePlace::Sample { device, slot: "recording samples/c5_f.wav".into() };
    let error = session.relink(&[Relink { place: place.clone(), path: gone.clone() + ".nope" }]).unwrap_err().to_string();
    assert!(error.contains("nothing was relinked"), "{error}");
    assert_eq!(session.relink(&[Relink { place: place.clone(), path: elsewhere.display().to_string() }]).unwrap(), 1);
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert!(rms(&render(&mut session)[4_800..43_200]) > 0.25);
    let state = State::from_device_state(session.project().device(device).unwrap().state.as_deref()).unwrap();
    assert_eq!(state.recordings.get("samples/c5_f.wav").map(String::as_str), Some(elsewhere.display().to_string().as_str()));
    assert!(session.files().iter().any(|file| file.place == place && file.path == elsewhere.display().to_string()));
    assert!(session.undo());
    assert_eq!(session.problems().len(), 1, "taken back, it is missing again");
    assert!(session.redo());

    // The song is saved; then the instrument's whole folder goes somewhere else.
    let song = folder.path().join("Song.trill");
    session.save(&song).unwrap();
    drop(session);
    let moved = folder.path().join("Instruments").join("Grand");
    std::fs::create_dir_all(&moved).unwrap();
    std::fs::rename(&grand, moved.join("Small Grand.sfz")).unwrap();
    std::fs::rename(&samples, moved.join("samples")).unwrap();

    let mut session = self::session();
    session.load(&song).unwrap();
    let device = session.project().tracks[0].devices[0].id;
    let problems: Vec<String> = session.problems().iter().map(|problem| format!("{problem:?}")).collect();
    assert!(problems.len() == 1 && problems[0].contains("there is no such file"), "{problems:?}");
    assert!(session.sampled(device).is_none());
    // One file to put right: the instrument's own. Its recordings are found from it.
    let there = moved.join("Small Grand.sfz").display().to_string();
    let place = FilePlace::Sample { device, slot: "instrument".into() };
    let error = session.relink(&[Relink { place: place.clone(), path: grand.clone() + "x" }]).unwrap_err().to_string();
    assert!(error.contains("there is no file") && error.contains("nothing was relinked"), "{error}");
    assert_eq!(session.relink(&[Relink { place, path: there.clone() }]).unwrap(), 1);
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert_eq!(session.sampled(device).unwrap().file, there);
    assert!(rms(&render(&mut session)[4_800..43_200]) > 0.25, "and the recording that was elsewhere still is");
}

#[test]
fn a_render_waits_for_the_recordings_it_plays_and_a_song_played_live_says_while_it_is_loading() {
    // Recordings many times longer than what is kept of each in memory:
    // a render made the moment the device exists still plays every note
    // to its end.
    let folder = folder();
    let grand = piano(folder.path());
    let mut session = session();
    let notes: Vec<Note> = [48u8, 55, 60, 67, 72].iter().map(|key| note(*key, 0.0, 5.0, 110)).collect();
    let (_, device) = played(&mut session, "Keys", &grand, &notes);
    let at_once = render(&mut session);
    // Two and a half seconds held: far past the third of a second that is kept.
    let late = &at_once[96_000..115_200];
    assert!(rms(late) > 0.3, "the notes still sound two seconds in: {}", rms(late));
    // The same samples as a render made when everything has been read.
    wait_until("the starts being read", || session.loading().is_empty());
    assert!(session.reading(device).is_some_and(|reading| reading.ready == 5));
    assert!(render(&mut session) == at_once);

    // A song that is saved and opened again plays the same.
    let song = folder.path().join("Song.trill");
    session.save(&song).unwrap();
    let mut again = self::session();
    again.load(&song).unwrap();
    assert!(again.problems().is_empty(), "{:?}", again.problems());
    assert!(render(&mut again) == at_once);
    // And what the project file says of its files is the instrument's file, with where it is from the song.
    let text = std::fs::read_to_string(&song).unwrap();
    assert!(text.contains("\"beside\": \"Small Grand.sfz\""), "the instrument is named beside the song");
    assert!(!text.contains("c3_p.wav"), "its recordings are named by the instrument's file, not by the song's");
}
