//! The audio of a shared song between the programs that hold it: each a
//! runtime of its own, connected over this machine's loopback and nothing
//! else, with files this test made in a temporary folder.
//!
//! The rules the whole of it stands on:
//!
//! * **No path is said, and none is taken.** A file is asked for by what
//!   it is, handed out only if the open song plays it, and taken in only
//!   when its bytes are what was asked for.
//! * **Only audio is ever read to be said or handed out, and only what
//!   its user let go.** A song file is a stranger too.
//! * **What arrives in a song is kept apart for that song.**
//! * **What a file may cost is settled before its first byte.**
//!
//! The strangers here hold the invitation and speak the protocol by hand.
//!
//! Connections made by a run of this file: 28.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::files::{Content, FileIs, FilePlace};
use trill_core::merge::{AudioSet, Change, ClipOp, Floors, Op, Said, Stamp};
use trill_core::project::{AudioContent, ClipContent};
use trill_core::{ChainRef, ClipId, DeviceId, Relink, TrackId};
use trill_engine::AudioBackend;
use trill_engine::hand::by_hand;
use trill_engine::render::RenderOptions;
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Meeting, Reach, Runtime, RuntimeOptions, Storage, Waiting};
use trill_together::files::content_of;
use trill_together::{Event, Invitation, MAX_PIECE, MAX_WANT, Message, Net, PROTOCOL, Person, meet, secure};

const WAIT: Duration = Duration::from_secs(30);

/// A runtime that keeps what it keeps under `folder`: no audio device, no
/// plugins, nothing of the user's.
fn start(folder: &Path, name: &str) -> Runtime {
    let storage = Storage::At { settings: folder.join(name).join("settings.json"), library_index: None };
    let runtime =
        Runtime::start(RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage })
            .expect("the runtime starts");
    runtime.set_name_together(name);
    runtime
}

/// [`start`], for someone who is asked before any of a song's audio
/// arrives: their settings say so.
fn start_asking_first(folder: &Path, name: &str) -> Runtime {
    std::fs::create_dir_all(folder.join(name)).unwrap();
    std::fs::write(folder.join(name).join("settings.json"), r#"{ "together": { "ask_above_mb": 0 } }"#).unwrap();
    start(folder, name)
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The song a program holds as every program that holds it has it: to the
/// last id, without what is each one's own.
fn text(runtime: &Runtime) -> String {
    let mut song = runtime.session.lock().said_song();
    song.count_ids_from(0);
    song.to_json()
}

fn said(runtime: &Runtime) -> String {
    runtime.said_together(0).into_iter().map(|(_, sentence)| sentence).collect::<Vec<_>>().join(" | ")
}

/// Writes `seconds` of a tone of `hertz` and returns the file's path.
fn tone(path: PathBuf, hertz: f64, seconds: f64) -> PathBuf {
    let rate = 44_100.0;
    let channel: Vec<f32> = (0..(seconds * rate) as usize)
        .map(|n| (n as f64 * hertz * std::f64::consts::TAU / rate).sin() as f32 * 0.5)
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(&path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
    path
}

fn written(path: PathBuf, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

fn content(path: &Path) -> Content {
    content_of(path, |_| true).unwrap().unwrap()
}

/// What a place of a program's song plays: where the program has the
/// file, and what the song says it is.
fn file_at(runtime: &Runtime, place: &FilePlace) -> (String, Option<FileIs>) {
    let found = runtime.session.lock().files_here().into_iter().find(|file| file.place == *place);
    found.map(|file| (file.path, file.is)).unwrap_or_default()
}

fn file_of(runtime: &Runtime, clip: ClipId) -> (String, Option<FileIs>) {
    file_at(runtime, &FilePlace::Clip(clip))
}

/// The folder what the open song brought a program is kept in.
fn received(runtime: &Runtime) -> PathBuf {
    runtime.session.lock().received_in().expect("the song is one others hold too")
}

/// Whether the file of a place is on this program's disk, in its folder
/// of what this song brought it, with the bytes of `original`.
fn arrived_at(runtime: &Runtime, place: &FilePlace, original: &Path) -> bool {
    let (path, is) = file_at(runtime, place);
    let Some(is) = is else { return false };
    let here = Path::new(&path);
    here == received(runtime).join(is.kept_as())
        && std::fs::read(here).is_ok_and(|bytes| Some(bytes) == std::fs::read(original).ok())
}

fn arrived(runtime: &Runtime, clip: ClipId, original: &Path) -> bool {
    arrived_at(runtime, &FilePlace::Clip(clip), original)
}

/// Everything under `folder`, by name.
fn files_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(folder) {
        Ok(entries) => entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// How loud the song is as the program plays it.
fn loudest(runtime: &Runtime) -> f32 {
    let job = runtime.session.lock().render_job();
    let options = RenderOptions { sample_rate: 48_000, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let rendered = job.render(&options).expect("the song renders");
    rendered.left.iter().fold(0.0f32, |loudest, sample| loudest.max(sample.abs()))
}

/// Puts a clip that plays `file` on a track, as the user's own hand does.
fn clip_by_hand(runtime: &Runtime, track: TrackId, name: &str, at: f64, file: &Path) -> ClipId {
    let _hand = by_hand();
    let recording = AudioContent::new(file.to_string_lossy());
    runtime.session.lock().edit(|song| song.add_audio_clip(track, name, at, 2.0, recording)).unwrap()
}

/// A program with a song that plays one recording of its own.
fn with_a_recording(folder: &Path, name: &str) -> (Runtime, TrackId, ClipId, PathBuf) {
    let runtime = start(folder, name);
    let vocal = tone(folder.join(format!("{name}'s music")).join("Night session").join("vocal take.wav"), 220.0, 1.0);
    let track = runtime.session.lock().edit(|song| Ok(song.add_track("Vocal"))).unwrap();
    let clip = clip_by_hand(&runtime, track, "Verse vocal", 0.0, &vocal);
    (runtime, track, clip, vocal)
}

/// The files a device of a program's song names.
fn named_by(runtime: &Runtime, device: DeviceId) -> Vec<String> {
    let session = runtime.session.lock();
    let device = session.project().device(device).unwrap();
    trill_core::DeviceFiles::files(&trill_dsp::SampleFiles, device).into_iter().map(|(_, path)| path).collect()
}

/// 3 connections.
#[test]
fn the_audio_of_a_song_arrives_with_the_song_by_what_it_is_and_nobody_is_told_a_path() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, track, verse, vocal) = with_a_recording(folder.path(), "Ada");
    assert!(file_of(&ada, verse).1.is_none(), "a song of one's own says nothing of what its files are");
    // A sampler of hers plays a file too.
    let kick = tone(folder.path().join("Ada's music").join("kit").join("kick.wav"), 60.0, 0.25);
    let sampler = {
        let _hand = by_hand();
        let mut session = ada.session.lock();
        let sampler = session.add_device(ChainRef::Track(track), "builtin:sampler", None, None).unwrap();
        let state = trill_dsp::sampler::State::new(kick.to_string_lossy()).to_bytes();
        session.set_device_state(sampler, Some(state)).unwrap();
        sampler
    };
    let in_sampler = FilePlace::Sample { device: sampler, slot: "sample".into() };
    // Before she shares, she is shown what goes.
    let going = ada.files_to_go();
    assert_eq!((going.files, going.gone, going.folders.len()), (2, 0, 0), "{going:?}");
    assert_eq!(going.elsewhere.len(), 2, "a song without a folder of its own: each file by its path");
    assert!(
        going.sentence().starts_with("2 files (") && going.sentence().contains("vocal take.wav"),
        "{}",
        going.sentence()
    );

    let ben = start(folder.path(), "Ben");
    // Ada shares and Ben joins: he has the song, then what it plays.
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    ben.join(&invitation, false).unwrap();
    eventually("Ada has said what her file is", || file_of(&ada, verse).1.is_some());
    let is = file_of(&ada, verse).1.unwrap();
    assert_eq!((is.content(), is.name.as_str()), (content(&vocal), "vocal take.wav"));
    assert_eq!(file_of(&ada, verse).0, vocal.to_string_lossy(), "and has it where she had it");
    eventually("the recording is at Ben's", || arrived(&ben, verse, &vocal));
    eventually("and what the sampler plays", || arrived_at(&ben, &in_sampler, &kick));
    eventually("and nothing is on its way any more", || ben.together().files.is_empty());
    let song = format!("{:032x}", ada.session.lock().shared_song().unwrap().0);
    let at_ben = folder.path().join("Ben").join("received").join(&song);
    assert_eq!(received(&ben), at_ben, "kept in a folder of this song's own");
    let mut expected = vec![format!("{}.wav", is.sha256), format!("{}.wav", content(&kick).sha256)];
    expected.sort();
    assert_eq!(files_in(&at_ben), expected, "each under its hash, and nothing beside them");
    assert_eq!(text(&ada), text(&ben), "the same song");
    assert_eq!(named_by(&ada, sampler), [kick.to_string_lossy()], "her sampler names her file where it is");
    assert_eq!(
        named_by(&ben, sampler),
        [at_ben.join(&expected[0]).to_string_lossy()].map(|_| file_at(&ben, &in_sampler).0)
    );
    // He hears it, without the song having been opened again, and
    // nothing of it is a missing file or a problem meanwhile or after.
    eventually("Ben's song sounds", || loudest(&ben) > 0.2);
    eventually("nothing is said to be missing or arriving", || {
        let session = ben.session.lock();
        session.problems().is_empty() && session.files_arriving().is_empty()
    });
    assert!(ben.files_missing().files.is_empty());

    // Nothing Ben holds names a place of Ada's.
    let held = ben.session.lock().project().to_json();
    assert!(!held.contains("Ada's music") && !held.contains("Night session"), "{held}");

    // What is added later follows by itself, either way: a clip Ada
    // drops in, a take, and a recording of Ben's own.
    let chorus = tone(folder.path().join("Ada's music").join("chorus.wav"), 330.0, 0.5);
    let ad_lib = tone(folder.path().join("Ada's music").join("ad lib 3.wav"), 440.0, 0.5);
    let second = clip_by_hand(&ada, track, "Chorus", 4.0, &chorus);
    let take = {
        let _hand = by_hand();
        let ad_lib = AudioContent::new(ad_lib.to_string_lossy());
        ada.session.lock().edit(|song| song.add_take(track, "Ad lib 3", 8.0, 1.0, ad_lib)).unwrap()
    };
    let guitar = tone(folder.path().join("Ben's desk").join("guitar.wav"), 110.0, 0.75);
    let riff = clip_by_hand(&ben, track, "Riff", 12.0, &guitar);
    eventually("Ada's clip and take are at Ben's", || arrived(&ben, second, &chorus) && arrived(&ben, take, &ad_lib));
    eventually("and Ben's recording at Ada's", || arrived(&ada, riff, &guitar));
    assert_eq!(file_of(&ben, riff).0, guitar.to_string_lossy(), "which he has where he had it");
    assert_eq!(text(&ada), text(&ben));
    assert!(!ada.session.lock().project().to_json().contains("Ben's desk"));

    // From anyone in the song who has it, not only from who added it:
    // Ada leaves, Ben shares on, and a third gets everything from him.
    ada.leave();
    eventually("Ada is gone", || ben.together().people.is_empty());
    let onward = ben.share(Reach::ThisMachine, 0).unwrap();
    let cy = start(folder.path(), "Cy");
    cy.join(&onward, false).unwrap();
    eventually("Cy has all five files", || {
        arrived(&cy, verse, &vocal)
            && arrived(&cy, second, &chorus)
            && arrived(&cy, take, &ad_lib)
            && arrived(&cy, riff, &guitar)
            && arrived_at(&cy, &in_sampler, &kick)
    });
    assert_eq!(files_in(&received(&cy)).len(), 5);
    assert_eq!(text(&ben), text(&cy));
    let held = cy.session.lock().project().to_json();
    assert!(!held.contains("Ada's music") && !held.contains("Ben's desk"), "{held}");
    for told in [said(&ada), said(&ben), said(&cy)] {
        assert!(!told.contains("thrown away") && !told.contains("no longer connected"), "{told}");
    }

    // Saved, the song is whole in its own folder: what it brought comes
    // beside it under the names the song says, and it opens alone later.
    let saved = folder.path().join("Cy's songs").join("Night.trill");
    std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
    cy.session.lock().save(&saved).unwrap();
    let beside = folder.path().join("Cy's songs").join("Night files").join("from others");
    let mut brought = files_in(&beside);
    brought.sort();
    assert_eq!(brought, ["ad lib 3.wav", "chorus.wav", "guitar.wav", "kick.wav", "vocal take.wav"]);
    assert_eq!(Path::new(&file_of(&cy, verse).0), beside.join("vocal take.wav"));
    assert!(files_in(&received(&cy)).is_empty(), "and none of it is left in the program's own folder");
    drop(cy);
    let alone = start(folder.path(), "Cy later");
    alone.session.lock().load(&saved).unwrap();
    assert!(alone.files_missing().files.is_empty(), "{:?}", alone.files_missing().files);
    assert!(loudest(&alone) > 0.2, "it sounds, alone and later");
}

// ---- strangers --------------------------------------------------------------

/// A program that has the invitation and speaks the protocol by hand.
fn by_hand_with(invitation: &str) -> (secure::Sayer, secure::Hearer) {
    let invitation = Invitation::parse(invitation).unwrap();
    let stream = std::net::TcpStream::connect(invitation.address()).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    secure::secure(stream, &invitation.key(), true).expect("the invitation is taken")
}

fn hello(song: Option<trill_core::merge::SongId>) -> Message {
    Message::Hello {
        protocol: PROTOCOL,
        build: "9.9.9".into(),
        person: Person { id: 4242, name: "Mallory".into(), colour: 0 },
        song,
        floors: Floors::default(),
        replica: None,
    }
}

fn hear(hearer: &mut secure::Hearer) -> Result<Message, String> {
    let bytes = hearer.hear(trill_together::MAX_MESSAGE).map_err(|broken| broken.to_string())?;
    Message::decode(&bytes)
}

/// What the other side says until it ends the connection, and why it did.
fn until_bye(hearer: &mut secure::Hearer) -> (Vec<Message>, String) {
    let mut heard = Vec::new();
    loop {
        match hear(hearer) {
            Ok(Message::Bye { why }) => return (heard, why),
            Ok(message) => heard.push(message),
            Err(why) => return (heard, why),
        }
    }
}

/// Joins by hand, and is handed the song as anyone with the invitation is.
fn joined_by_hand(invitation: &str) -> (secure::Sayer, secure::Hearer, String) {
    let (mut say, mut hearer) = by_hand_with(invitation);
    say.say(&hello(None).encode()).unwrap();
    loop {
        if let Message::Song { text } = hear(&mut hearer).expect("the song is handed over") {
            return (say, hearer, text);
        }
    }
}

/// The next thing said that is about files.
fn next_of_files(hearer: &mut secure::Hearer) -> Message {
    loop {
        match hear(hearer).expect("the connection stays") {
            said @ (Message::Have { .. } | Message::Nothing { .. } | Message::Piece { .. } | Message::Later { .. }) => {
                return said;
            }
            _ => continue,
        }
    }
}

/// Asks for a file by what it is, and says whether anything of it is
/// handed out: bytes, or the word that it is here.
fn asked_for(say: &mut secure::Sayer, hearer: &mut secure::Hearer, file: &Content) -> bool {
    say.say(&Message::Want { file: file.clone(), at: 0, len: 1000 }.encode()).unwrap();
    loop {
        match next_of_files(hearer) {
            Message::Have { files } => assert!(!files.contains(file), "it says it has the file"),
            Message::Nothing { file: of } => {
                assert_eq!(&of, file);
                break false;
            }
            Message::Piece { .. } => break true,
            other => panic!("{other:?}"),
        }
    }
}

/// Asks for all of a file by hand and returns its bytes.
fn fetched(say: &mut secure::Sayer, hearer: &mut secure::Hearer, file: &Content) -> Vec<u8> {
    say.say(&Message::Want { file: file.clone(), at: 0, len: file.bytes.min(MAX_WANT) }.encode()).unwrap();
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < file.bytes.min(MAX_WANT) {
        match next_of_files(hearer) {
            Message::Piece { file: of, at, bytes: piece } => {
                assert_eq!((&of, at), (file, bytes.len() as u64), "in order, and of the file that was asked for");
                assert!(piece.len() <= MAX_PIECE);
                bytes.extend(piece);
            }
            Message::Have { .. } => continue,
            other => panic!("asked for a file the song plays, and heard {other:?}"),
        }
    }
    bytes
}

/// A change that makes a clip on `track` which says its file is `is`
/// and, for good measure, where it lies.
fn planted_clip(of: &Runtime, track: TrackId, id: u64, is: FileIs, at: &Path, stamp: u64) -> (ClipId, Message) {
    let mut clip = of.session.lock().project().tracks[0].clips[0].clone();
    (clip.id, clip.name) = (ClipId(77 << 20 | id), format!("Planted {id}"));
    let mut audio = AudioContent::new(at.to_string_lossy());
    audio.is = Some(is);
    clip.content = ClipContent::Audio(audio);
    let change =
        Change { stamp: Stamp(stamp, 77), ops: vec![Op::Clip(clip.id, ClipOp::Whole(Box::new(clip.clone()), track))] };
    (
        clip.id,
        Message::Changes { said: vec![Said { change, knew: None }], floors: Floors::default(), replica: Some(77) },
    )
}

/// 9 connections.
#[test]
fn a_program_hands_out_nothing_but_what_its_song_plays_asked_for_by_what_it_is() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, track, _, vocal) = with_a_recording(folder.path(), "Ada");
    // A file of Ada's that lies beside the song's and is none of the song's.
    let letters =
        tone(folder.path().join("Ada's music").join("Night session").join("demo for the label.wav"), 99.0, 0.5);
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    let (mut say, mut hearer, handed) = joined_by_hand(&invitation);
    assert!(!handed.contains("Ada's music") && !handed.contains("Night session"), "the song is handed over: {handed}");
    assert!(!handed.contains("\"files\": ["), "and nothing of where its files are from its folder");
    let (song_file, other) = (content(&vocal), content(&letters));

    // What the song plays is said, and handed out by what it is.
    let have = loop {
        if let Message::Have { files } = next_of_files(&mut hearer) {
            break files;
        }
    };
    assert_eq!(have, std::slice::from_ref(&song_file), "what Ada has of the song's files, and nothing else");
    assert_eq!(fetched(&mut say, &mut hearer, &song_file), std::fs::read(&vocal).unwrap());

    // A file the song does not play: asked for by what it is (which this
    // stranger happens to know), it is not here. The answer is the same
    // for a file nobody has, and for the song's own at another length or
    // from past its end.
    let nobody_has = Content::new(&[7; 32], 4096);
    let another_length = Content { bytes: song_file.bytes + 1, ..song_file.clone() };
    for file in [&other, &nobody_has, &another_length] {
        assert!(!asked_for(&mut say, &mut hearer, file), "{file}");
    }
    say.say(&Message::Want { file: song_file.clone(), at: song_file.bytes, len: 1000 }.encode()).unwrap();
    assert_eq!(next_of_files(&mut hearer), Message::Nothing { file: song_file.clone() });

    // Making the song name the file does not get it either: a clip that
    // arrives saying where the file lies and what it is. The place is not
    // taken, so Ada's program has no such file for the song, and looks
    // for it among what this song brought her, where it is not.
    let is = FileIs::new(other.clone(), "demo for the label.wav");
    let (planted, changes) = planted_clip(&ada, track, 5, is.clone(), &letters, 500);
    say.say(&changes.encode()).unwrap();
    eventually("Ada has the clip", || ada.session.lock().project().clip(planted).is_ok());
    let at_ada = received(&ada);
    assert_eq!(Path::new(&file_of(&ada, planted).0), at_ada.join(is.kept_as()), "its place is one Ada's program made");
    // The same with a file that another shared song brought her: it lies
    // in that song's folder, under its hash, and this song does not get it.
    let of_another_song = tone(folder.path().join("made elsewhere").join("someone's stem.wav"), 77.0, 0.5);
    let stem = content(&of_another_song);
    let stem_is = FileIs::new(stem.clone(), "stem.wav");
    let another = at_ada.with_file_name(format!("{:032x}", 0xfeed_u128));
    std::fs::create_dir_all(&another).unwrap();
    std::fs::copy(&of_another_song, another.join(stem_is.kept_as())).unwrap();
    let (second, changes) = planted_clip(&ada, track, 6, stem_is.clone(), &another.join(stem_is.kept_as()), 501);
    say.say(&changes.encode()).unwrap();
    eventually("Ada has that clip too", || ada.session.lock().project().clip(second).is_ok());
    assert_eq!(Path::new(&file_of(&ada, second).0), at_ada.join(stem_is.kept_as()), "a place in this song's folder");
    // (Long enough for her program to have looked at its files again.)
    std::thread::sleep(Duration::from_secs(2));
    assert!(!asked_for(&mut say, &mut hearer, &other) && !asked_for(&mut say, &mut hearer, &stem));
    assert!(files_in(&at_ada).is_empty(), "and nothing was written at Ada's");

    // Asked for by a path, in the only place there is to say one: that is
    // no message, and the connection is ended.
    let by_path = Content { sha256: letters.to_string_lossy().into_owned(), bytes: other.bytes };
    say.say(&Message::Want { file: by_path, at: 0, len: 1000 }.encode()).unwrap();
    let (heard, _) = until_bye(&mut hearer);
    assert!(heard.iter().all(|message| !message.is_piece()), "nothing is handed out for it");
    eventually("Ada is told why it was let go of", || said(&ada).contains("names a file by something that is no hash"));

    // A stretch nobody asks for, and bytes nobody asked for: let go of.
    for (said_by_hand, expected) in [
        (
            Message::Want { file: song_file.clone(), at: 0, len: MAX_WANT + 1 },
            "a stretch of a file that nobody asks for",
        ),
        (Message::Want { file: song_file.clone(), at: 0, len: 0 }, "a stretch of a file that nobody asks for"),
        (
            Message::Piece { file: song_file.clone(), at: 0, bytes: vec![1; 1000] },
            "bytes of a file that were not asked for",
        ),
        (
            Message::Piece { file: nobody_has.clone(), at: 0, bytes: vec![1; 1000] },
            "bytes of a file that were not asked for",
        ),
    ] {
        let (mut say, mut hearer, _) = joined_by_hand(&invitation);
        say.say(&said_by_hand.encode()).unwrap();
        let (_, why) = until_bye(&mut hearer);
        assert!(why.contains(expected), "{why}");
    }
    assert!(files_in(&at_ada).is_empty(), "nothing of it was written at Ada's");
    eventually("Ada says who was let go, and why", || said(&ada).contains("Mallory is no longer connected"));

    // A place said as a change of its own is refused as what nobody says.
    let (mut say, mut hearer, _) = joined_by_hand(&invitation);
    let place = Op::Clip(planted, ClipOp::Audio(AudioSet::Path(letters.to_string_lossy().into_owned())));
    let change = Change { stamp: Stamp(600, 78), ops: vec![place] };
    let changes =
        Message::Changes { said: vec![Said { change, knew: None }], floors: Floors::default(), replica: Some(78) };
    say.say(&changes.encode()).unwrap();
    let (_, why) = until_bye(&mut hearer);
    assert!(why.contains("is not said to anyone else"), "{why}");
    assert_eq!(Path::new(&file_of(&ada, planted).0), at_ada.join(is.kept_as()), "the place is as it was");

    // The same asked for a thousand times by someone who takes none of
    // it: no more is read for them than a little, and they are let go of.
    let (mut say, mut hearer, _) = joined_by_hand(&invitation);
    let all = Message::Want { file: song_file.clone(), at: 0, len: song_file.bytes.min(MAX_WANT) }.encode();
    for _ in 0..1000 {
        if say.say(&all).is_err() {
            break;
        }
    }
    let (_, why) = until_bye(&mut hearer);
    assert!(why.contains("asked for more of the song's files than it took"), "{why}");

    // And someone who is really there gets the song and its file after all that.
    let ben = start(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();
    let verse = ada.session.lock().project().tracks[0].clips[0].id;
    eventually("Ben has the recording", || arrived(&ben, verse, &vocal));
}

/// A song as a file someone made by hand: it plays one recording that is
/// audio and lies beside it, and names five things that are none.
struct Doctored {
    song: PathBuf,
    audio: PathBuf,
    elsewhere: PathBuf,
    /// Each thing that is no audio, and what the song calls the clip.
    strangers: Vec<(&'static str, PathBuf)>,
    #[cfg(windows)]
    _locked: std::fs::File,
}

fn doctored(folder: &Path) -> Doctored {
    let songs = folder.join("songs");
    let audio = tone(songs.join("samples").join("loop.wav"), 150.0, 1.0);
    let elsewhere = tone(folder.join("voice memos").join("private.wav"), 180.0, 1.0);
    let key =
        written(folder.join("home").join(".ssh").join("id_ed25519"), b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot one\n");
    let text =
        written(songs.join("samples").join("notes.wav"), b"RIFF these are only words, under a name that says audio");
    let bare = written(folder.join("home").join("cookies"), &[7u8; 5000]);
    let locked = written(folder.join("home").join("in use.wav"), b"RIFF nobody else may read this while it is open");
    // (Open for itself alone: nobody else reads it.)
    #[cfg(windows)]
    let held_open = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&locked).unwrap()
    };
    let strangers = vec![
        (
            "A key by a path with ..",
            songs.join("samples").join("..").join("..").join("home").join(".ssh").join("id_ed25519"),
        ),
        ("Text under an audio name", text),
        ("A file with no ending", bare),
        ("A folder", folder.join("home")),
        ("A file that cannot be read", locked),
    ];
    assert!(key.exists());
    // The song is written by a program that is nobody's hand.
    let maker = start(folder, "maker");
    let song = songs.join("handed to me.trill");
    {
        let mut session = maker.session.lock();
        let track = session.edit(|song| Ok(song.add_track("Audio"))).unwrap();
        let mut at = 0.0;
        let files = [("Loop", &audio), ("Memo", &elsewhere)].into_iter().map(|(name, path)| (name, path.clone()));
        for (name, path) in files.chain(strangers.iter().map(|(name, path)| (*name, path.clone()))) {
            let recording = AudioContent::new(path.to_string_lossy());
            session.edit(|song| song.add_audio_clip(track, name, at, 2.0, recording)).unwrap();
            at += 2.0;
        }
        session.save(&song).unwrap();
    }
    Doctored {
        song,
        audio,
        elsewhere,
        strangers,
        #[cfg(windows)]
        _locked: held_open,
    }
}

/// Writes into the song file at `song`, by hand, `to` wherever a clip
/// names `from`: the same file by another spelling.
fn respell(song: &Path, from: &Path, to: &Path) {
    fn walk(node: &mut serde_json::Value, from: &str, to: &str, found: &mut usize) {
        match node {
            serde_json::Value::String(text) if text == from => {
                *text = to.to_owned();
                *found += 1;
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|item| walk(item, from, to, found)),
            serde_json::Value::Object(named) => named.values_mut().for_each(|item| walk(item, from, to, found)),
            _ => {}
        }
    }
    let mut held: serde_json::Value = serde_json::from_slice(&std::fs::read(song).unwrap()).unwrap();
    // (Without the table of where its files were, which a file written
    // by hand need not carry: with it, a file is found again from the
    // song's folder and named as the file system spells it.)
    held.as_object_mut().unwrap().remove("files");
    let mut found = 0;
    walk(&mut held, &from.to_string_lossy(), &to.to_string_lossy(), &mut found);
    assert!(found >= 1, "the song file names {}", from.display());
    std::fs::write(song, serde_json::to_vec_pretty(&held).unwrap()).unwrap();
}

fn clip_named(runtime: &Runtime, name: &str) -> ClipId {
    let session = runtime.session.lock();
    let clips = session.project().tracks.iter().flat_map(|track| &track.clips);
    clips.into_iter().find(|clip| clip.name == name).map(|clip| clip.id).expect("the clip is there")
}

/// 3 connections.
#[test]
fn a_song_file_is_a_stranger_too_and_only_audio_its_user_let_go_is_ever_said_or_handed_out() {
    let folder = tempfile::tempdir().unwrap();
    let doctored = doctored(folder.path());
    let hashes: Vec<Content> =
        (doctored.strangers.iter()).filter_map(|(_, path)| content_of(path, |_| true).ok().flatten()).collect();
    assert!(hashes.len() >= 3, "what can be read of them has a hash someone could ask by");

    // And the memo is named by a path that begins with the song's own
    // folder and leaves it again, written into the file by hand: nothing
    // Trill writes spells a path so, and a stranger's file may.
    let through = folder.path().join("songs").join("..").join("voice memos").join("private.wav");
    respell(&doctored.song, &doctored.elsewhere, &through);

    // Its owner opens it. Nothing of it has gone anywhere: there is nobody.
    let ada = start(folder.path(), "Ada");
    ada.session.lock().load(&doctored.song).unwrap();
    let memo_as_named = file_of(&ada, clip_named(&ada, "Memo")).0;
    assert!(memo_as_named.contains(".."), "the song names it as the file spells it: {memo_as_named}");
    // Before she shares she is shown what would go: how many, how large,
    // the folders, and each file that lies outside the song's own folder.
    let going = ada.files_to_go();
    assert_eq!(going.folders.len(), 1, "{going:?}");
    assert_eq!((going.folders[0].0.as_str(), going.folders[0].1), ("the song's folder", 2));
    let shown = going.sentence();
    for outside in ["private.wav", "id_ed25519", "cookies", "in use.wav"] {
        assert!(going.elsewhere.iter().any(|path| path.ends_with(outside)), "{outside} is named: {going:?}");
        assert!(shown.contains(outside), "{shown}");
    }
    assert!(shown.contains("where Trill reads them as audio"), "{shown}");

    // She shares it all the same. Only what Trill reads as audio is said
    // and handed out: the two recordings.
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    let (mut say, mut hearer, handed) = joined_by_hand(&invitation);
    let (looped, memo) = (content(&doctored.audio), content(&doctored.elsewhere));
    let mut have: Vec<Content> = Vec::new();
    while have.len() < 2 {
        if let Message::Have { files } = next_of_files(&mut hearer) {
            have.extend(files);
        }
    }
    have.sort();
    let mut audio = vec![looped.clone(), memo.clone()];
    audio.sort();
    assert_eq!(have, audio, "the two recordings, and nothing else");
    // (And a look that comes later finds nothing more to say.)
    std::thread::sleep(Duration::from_millis(2500));
    for stranger in &hashes {
        assert!(!asked_for(&mut say, &mut hearer, stranger), "{stranger} is handed out");
    }
    let held = serde_json_of(&ada);
    for stranger in &hashes {
        assert!(!handed.contains(&stranger.sha256) && !held.contains(&stranger.sha256), "its hash is in the song");
    }
    for (name, _) in &doctored.strangers {
        assert_eq!(file_of(&ada, clip_named(&ada, name)).1, None, "{name}: the song says nothing of what it is");
    }
    assert_eq!(file_of(&ada, clip_named(&ada, "Loop")).1.map(|is| is.content()), Some(looped.clone()));
    drop((say, hearer));

    // A friend joins and gets the two; the song is saved as the shared
    // song it is, with one more recording that nobody's hand put there.
    let ben = start(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the two recordings", || {
        arrived(&ben, clip_named(&ada, "Loop"), &doctored.audio)
            && arrived(&ben, clip_named(&ada, "Memo"), &doctored.elsewhere)
    });
    let late = tone(folder.path().join("voice memos").join("later.wav"), 200.0, 0.5);
    let added = {
        let recording = AudioContent::new(late.to_string_lossy());
        let track = ada.session.lock().project().tracks[0].id;
        ada.session.lock().edit(|song| song.add_audio_clip(track, "Later", 40.0, 1.0, recording)).unwrap()
    };
    eventually("Ben has the clip", || ben.session.lock().project().clip(added).is_ok());
    eventually("Ada is told that it is held", || ada.together().held == [late.to_string_lossy()]);
    assert!(said(&ada).contains("1 file that the shared song names on this computer has not gone"), "{}", said(&ada));
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(file_of(&ben, added), (String::new(), None), "nothing of it has gone to anyone");
    let shared = folder.path().join("songs").join("shared.trill");
    ada.session.lock().save(&shared).unwrap();
    drop(ada);

    // Later it is opened again, by whoever, and joins the friend who
    // still has it. Everything its file names is held: nothing is read,
    // said or handed out, whatever was let go the time before.
    let onward = ben.share(Reach::ThisMachine, 0).unwrap();
    let again = start(folder.path(), "Ada again");
    again.session.lock().load(&shared).unwrap();
    again.join(&onward, false).unwrap();
    eventually("the two are in the song", || again.together().people.len() == 1 && ben.together().people.len() == 1);
    eventually("what the file names is held", || again.together().held.len() == 8);
    assert!(
        said(&again).contains("8 files that the shared song names on this computer have not gone"),
        "{}",
        said(&again)
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(file_of(&ben, added), (String::new(), None));
    assert!(again.together().files.is_empty() && ben.together().files.is_empty());
    // Shown them, she lets them go: the recording arrives, and of what is
    // no audio there is still nothing, here or there.
    again.let_files_go();
    eventually("the late recording is at Ben's", || arrived(&ben, added, &late));
    eventually("nothing is held any more", || again.together().held.is_empty());
    std::thread::sleep(Duration::from_millis(2500));
    let (at_ben, at_ada) = (serde_json_of(&ben), serde_json_of(&again));
    for stranger in &hashes {
        assert!(!at_ben.contains(&stranger.sha256) && !at_ada.contains(&stranger.sha256));
    }
    for (name, _) in &doctored.strangers {
        assert_eq!(file_of(&ben, clip_named(&again, name)), (String::new(), None), "{name}");
    }
    assert_eq!(files_in(&received(&ben)).len(), 3, "the three recordings, and nothing else");
}

/// Everything a program holds of the song, removed things too, as text.
fn serde_json_of(runtime: &Runtime) -> String {
    let mut session = runtime.session.lock();
    format!("{}\n{}", session.project().to_json(), session.whole_song())
}

/// A program that shares a song by hand: it listens under an invitation,
/// hands whoever joins `song`, says it has `file`, and answers what it is
/// asked for with whatever `answer` makes of the stretch that was asked
/// for (the pieces to send back, each with where it says it lies).
fn shared_by_hand(
    song: String,
    file: Content,
    answer: impl Fn(u64, u64) -> Vec<(u64, Vec<u8>)> + Send + 'static,
) -> String {
    let mut net = Net::new();
    let invitation = net.listen(Reach::ThisMachine, 0).unwrap().to_string();
    std::thread::spawn(move || {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            match net.next(Duration::from_millis(100)) {
                Some(Event::Open { link, .. }) => net.say(link, &hello(None)),
                Some(Event::Heard { link, message: Message::Hello { .. } }) => {
                    net.say(link, &Message::Song { text: song.clone() });
                    net.say(link, &Message::Have { files: vec![file.clone()] });
                }
                Some(Event::Heard { link, message: Message::Want { file, at, len } }) => {
                    for (at, bytes) in answer(at, len) {
                        net.say(link, &Message::Piece { file: file.clone(), at, bytes });
                    }
                }
                Some(Event::Lost { .. }) => return,
                _ => {}
            }
        }
    });
    invitation
}

/// 6 connections.
#[test]
fn what_arrives_is_a_file_of_the_song_only_if_it_is_what_was_asked_for() {
    let folder = tempfile::tempdir().unwrap();
    // A song with a recording, as a program that shares it hands it over.
    let (eve, track, verse, vocal) = with_a_recording(folder.path(), "Eve");
    let _ = eve.share(Reach::ThisMachine, 0).unwrap();
    eventually("Eve's program has said what the file is", || file_of(&eve, verse).1.is_some());
    let song = eve.session.lock().whole_song();
    assert!(!song.contains("Eve's music"), "{song}");
    let real = std::fs::read(&vocal).unwrap();
    let file = content(&vocal);
    let stretch = |bytes: &[u8], at: u64, len: u64| -> Vec<(u64, Vec<u8>)> {
        let (from, to) = (at as usize, (at + len).min(bytes.len() as u64) as usize);
        let pieces = bytes[from..to].chunks(MAX_PIECE).enumerate();
        pieces.map(|(n, piece)| (at + (n * MAX_PIECE) as u64, piece.to_vec())).collect()
    };
    let at = |name: &str| {
        folder.path().join(name).join("received").join(format!("{:032x}", eve.session.lock().shared_song().unwrap().0))
    };

    // Other bytes than were asked for, of the right length: all of it is
    // thrown away, the user is told, and nothing of it is a file.
    let mut wrong = real.clone();
    wrong[real.len() / 2] ^= 0x55;
    let invitation = shared_by_hand(song.clone(), file.clone(), move |at, len| stretch(&wrong, at, len));
    let ben = start(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();
    eventually("Ben is told what was thrown away", || said(&ben).contains("was not the file the song names"));
    assert!(said(&ben).contains("'vocal take.wav'") && said(&ben).contains("Mallory"), "{}", said(&ben));
    assert!(files_in(&at("Ben")).is_empty(), "nothing of it is left: {:?}", files_in(&at("Ben")));
    assert!(!Path::new(&file_of(&ben, verse).0).exists(), "and the clip has no file");
    assert_eq!(ben.together().connected, 1, "the song goes on with whoever sent it");
    drop(ben);

    // Another length than was said: more bytes than the stretch that was
    // asked for. The program that sends them is let go of.
    let longer = real.clone();
    let invitation = shared_by_hand(song.clone(), file.clone(), move |at, len| {
        let mut pieces = stretch(&longer, at, len);
        pieces.push((at + len, vec![9; 1000]));
        pieces
    });
    let cy = start(folder.path(), "Cy");
    cy.join(&invitation, false).unwrap();
    eventually("Cy lets go of it", || said(&cy).contains("sent bytes of a file that were not asked for"));

    // A file that never ends: it goes on past what the file is said to be.
    let endless = real.clone();
    let invitation = shared_by_hand(song.clone(), file.clone(), move |at, len| {
        let mut pieces = stretch(&endless, at, len);
        let mut after = at + len;
        for _ in 0..200 {
            pieces.push((after, vec![3; MAX_PIECE]));
            after += MAX_PIECE as u64;
        }
        pieces
    });
    let dee = start(folder.path(), "Dee");
    dee.join(&invitation, false).unwrap();
    eventually("Dee lets go of it", || said(&dee).contains("sent bytes of a file that were not asked for"));
    // Neither kept a byte more than the file is: what there is of it when
    // everything has come to rest is the start of the file, or all of it.
    drop((cy, dee));
    for folder in [at("Cy"), at("Dee")] {
        for name in files_in(&folder) {
            // (A part may be given its name between the look and the read.)
            let Ok(kept) = std::fs::read(folder.join(&name)) else { continue };
            assert!(kept.len() <= real.len() && kept == real[..kept.len()], "{name} is {} bytes", kept.len());
        }
    }

    // Bytes that lie elsewhere in the file than was asked.
    let shifted = real.clone();
    let invitation = shared_by_hand(song.clone(), file.clone(), move |at, len| {
        stretch(&shifted, at, len).into_iter().map(|(at, bytes)| (at + 1, bytes)).collect()
    });
    let fay = start(folder.path(), "Fay");
    fay.join(&invitation, false).unwrap();
    eventually("Fay lets go of it", || said(&fay).contains("sent bytes of a file that were not asked for"));
    drop(fay);
    assert!(files_in(&at("Fay")).iter().all(|name| name.ends_with(".part")));

    // A file of a length nobody could mean, which someone says they have:
    // it costs nothing here. No name is made for it, nothing is asked
    // for, and the user is told once.
    let vast = FileIs::new(Content::new(&[0xee; 32], 1 << 60), "everything.wav");
    let (_, changes) = planted_clip(&eve, track, 9, vast.clone(), Path::new("x.wav"), 900);
    let Message::Changes { said: planted, .. } = changes else { unreachable!() };
    let floors = eve.session.lock().floors();
    eve.session.lock().merge("Mallory", planted, Some(&floors), Some(77)).unwrap();
    let song = eve.session.lock().whole_song();
    assert!(song.contains(&vast.sha256));
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = asked.clone();
    let invitation = shared_by_hand(song, vast.content(), move |_, _| {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Vec::new()
    });
    let gus = start(folder.path(), "Gus");
    gus.join(&invitation, false).unwrap();
    eventually("Gus is told", || said(&gus).contains("'everything.wav', a file of the shared song, is not taken in"));
    assert!(said(&gus).contains("which no file of a song is"), "{}", said(&gus));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0, "nothing of it was asked for");
    let kept = files_in(&at("Gus"));
    assert!(kept.iter().all(|name| !name.contains(&vast.sha256)), "no name was made for it: {kept:?}");
}

/// 1 connection.
#[test]
fn a_file_is_asked_for_in_strides_and_not_a_piece_at_a_time() {
    // (One question for every piece that arrived is thousands a second
    // for a long recording: more, faster, than a program that works in a
    // song says, and the connection was let go of for it once.)
    let folder = tempfile::tempdir().unwrap();
    let (eve, track, verse, _) = with_a_recording(folder.path(), "Eve");
    let long = tone(folder.path().join("Eve's music").join("a longer take.wav"), 300.0, 12.0);
    let second = clip_by_hand(&eve, track, "Longer", 8.0, &long);
    let _ = eve.share(Reach::ThisMachine, 0).unwrap();
    eventually("Eve's program has said what the files are", || {
        file_of(&eve, verse).1.is_some() && file_of(&eve, second).1.is_some()
    });
    let song = eve.session.lock().whole_song();
    let real = std::fs::read(&long).unwrap();
    let file = content(&long);
    let pieces = real.len().div_ceil(MAX_PIECE);
    assert!(pieces >= 40, "a file of many pieces: {pieces}");
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = asked.clone();
    let invitation = shared_by_hand(song, file, move |at, len| {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (from, to) = (at as usize, (at + len).min(real.len() as u64) as usize);
        let pieces = real[from..to].chunks(MAX_PIECE).enumerate();
        pieces.map(|(n, piece)| (at + (n * MAX_PIECE) as u64, piece.to_vec())).collect()
    });
    let ben = start(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();
    eventually("the long take is at Ben's", || arrived(&ben, second, &long));
    let asked = asked.load(std::sync::atomic::Ordering::Relaxed);
    assert!(asked >= 2 && asked <= pieces / 2, "{asked} questions for {pieces} pieces");
    assert!(!said(&ben).contains("no longer") && !said(&ben).contains("lost"), "{}", said(&ben));
}

/// 2 connections.
#[test]
fn above_what_the_user_set_files_wait_for_their_word_and_a_file_they_point_at_is_taken_if_it_is_the_file() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, track, verse, vocal) = with_a_recording(folder.path(), "Ada");
    let chorus = tone(folder.path().join("Ada's music").join("chorus.wav"), 330.0, 0.5);
    let second = clip_by_hand(&ada, track, "Chorus", 4.0, &chorus);
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    // Ben is asked before anything arrives: his settings say so.
    let ben = start_asking_first(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();
    eventually("both files wait for his word", || {
        let files = ben.together().files;
        files.len() == 2 && files.iter().all(|file| file.waits == Some(Waiting::ForAWord) && file.here == 0)
    });
    eventually("and he is told, once", || said(&ben).contains("than arrives without your word"));
    assert_eq!(said(&ben).matches("without your word").count(), 1);
    assert!(files_in(&received(&ben)).is_empty(), "nothing was made for them");
    // Meanwhile they are no missing files, and no problems: they are said
    // as on their way.
    assert!(ben.files_missing().files.is_empty());
    assert!(ben.session.lock().problems().is_empty(), "{:?}", ben.session.lock().problems());
    assert_eq!(ben.session.lock().files_arriving().len(), 2);
    assert_eq!(text(&ada), text(&ben), "and the song is whole and goes on");

    // He has the first recording himself, somewhere of his own (the same
    // pack, say): pointed at, it is read and held against what the song
    // says, and taken. Nothing is fetched for it.
    let his_own = folder.path().join("Ben's packs").join("vox").join("take 7.wav");
    std::fs::create_dir_all(his_own.parent().unwrap()).unwrap();
    std::fs::copy(&vocal, &his_own).unwrap();
    let pointed = |clip: ClipId, at: &Path| {
        let _hand = by_hand();
        ben.relink(&[Relink { place: FilePlace::Clip(clip), path: at.to_string_lossy().into_owned() }])
    };
    // Another file than the song names is refused in a sentence.
    let refused = pointed(verse, &chorus).unwrap_err().to_string();
    assert!(refused.contains("is not the file the song names as 'vocal take.wav'"), "{refused}");
    assert!(refused.contains("Nothing was relinked"), "{refused}");
    assert_eq!(pointed(verse, &his_own).unwrap(), 1);
    assert_eq!(file_of(&ben, verse).0, his_own.to_string_lossy());
    eventually("one file waits now", || ben.together().files.len() == 1);
    assert_eq!(file_of(&ada, verse).0, vocal.to_string_lossy(), "and Ada's place for it is as it was");
    assert!(files_in(&received(&ben)).is_empty());

    // His word: the other arrives.
    ben.let_files_arrive();
    eventually("the second recording is at Ben's", || arrived(&ben, second, &chorus));
    eventually("nothing waits", || ben.together().files.is_empty() && ben.session.lock().files_arriving().is_empty());
    assert_eq!(files_in(&received(&ben)).len(), 1);
    eventually("it sounds", || loudest(&ben) > 0.2);
}

/// A meeting point on this machine, on a port the system picks, at which
/// one key is given.
struct MeetingPoint {
    server: Option<meet::Server>,
    port: u16,
    address: String,
    key: meet::Key,
    limits: meet::Limits,
}

impl MeetingPoint {
    fn start(limits: meet::Limits) -> MeetingPoint {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let address = format!("127.0.0.1:{port}");
        let mut point = MeetingPoint { server: None, port, address, key: meet::Key::new().unwrap(), limits };
        point.serve(listener);
        point
    }

    fn serve(&mut self, listener: std::net::TcpListener) {
        let said: meet::Log = std::sync::Arc::new(|_: &str| {});
        let keys = meet::Keys::of(vec![meet::keys::Given::of(&self.key, "Ada")]);
        self.server = Some(meet::Server::start(vec![listener], keys, self.limits.clone(), said).unwrap());
    }

    fn stop(&mut self) {
        self.server.take().expect("it is running").stop();
    }

    fn start_again(&mut self) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", self.port)).unwrap();
        self.serve(listener);
    }
}

/// 4 connections.
#[test]
fn where_a_meeting_points_day_is_nearly_spent_the_song_goes_on_and_its_files_wait() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, track, verse, _) = with_a_recording(folder.path(), "Ada");
    // Ten seconds of sound, 1.7 MB: more than the key may still pass today
    // above what is kept for the song itself.
    let long = tone(folder.path().join("Ada's music").join("long take.wav"), 260.0, 10.0);
    let second = clip_by_hand(&ada, track, "Long take", 8.0, &long);
    let kept_for_the_song = 64 << 20;
    let mut point =
        MeetingPoint::start(meet::Limits { bytes_a_day: kept_for_the_song + (600 << 10), ..Default::default() });
    ada.set_meeting_point(&point.address, Some(&point.key.to_string())).unwrap();
    let invitation = ada.share_through_meeting_point().unwrap();
    eventually("the meeting is open", || ada.together().meeting == Some((point.address.clone(), Meeting::Held)));
    let ben = start(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();

    // Some of the audio crosses; then what is left is kept for the song.
    eventually("Ada is told that the files wait", || said(&ada).contains("has nearly passed what its keeper lets"));
    eventually("and so is Ben", || said(&ben).contains("Audio files from Ada wait"));
    let (at_ada, at_ben) = (said(&ada), said(&ben));
    assert!(at_ada.contains("which goes on; its audio files wait, and go on in about 24 h"), "{at_ada}");
    assert!(at_ben.contains("its files go on arriving in about 24 h"), "{at_ben}");
    assert!(ada.together().files_wait.is_some_and(|seconds| seconds > 23 * 3600));
    eventually("what is on its way says why it waits", || {
        ben.together()
            .files
            .iter()
            .any(|file| matches!(file.waits, Some(Waiting::ForTheWay { seconds }) if seconds > 80_000))
    });
    assert!(!arrived(&ben, second, &long), "the long take is not whole at Ben's");
    // The song goes on, both ways.
    ada.session.lock().edit(|song| song.set_tempo(97.0)).unwrap();
    ben.session.lock().edit(|song| song.set_track_volume(track, -7.0)).unwrap();
    eventually("each has what the other did", || {
        ben.session.lock().project().tempo == 97.0
            && ada.session.lock().project().track(track).unwrap().volume_db == -7.0
    });
    assert_eq!(text(&ada), text(&ben));
    assert_eq!((ada.together().connected, ben.together().connected), (1, 1));
    assert!(!said(&ada).contains("passes no more for your key today"), "the key's day was not spent: {}", said(&ada));

    // The keeper allows more (the meeting point is started again with
    // room): the link comes back by itself, and the files go on from
    // where they were.
    let before: u64 = ben.together().files.iter().map(|file| file.here).sum();
    point.stop();
    point.limits = meet::Limits::default();
    point.start_again();
    eventually("the long take is whole at Ben's", || arrived(&ben, second, &long));
    eventually("and the first recording", || {
        arrived(&ben, verse, &folder.path().join("Ada's music").join("Night session").join("vocal take.wav"))
    });
    eventually("Ada is told that they go on", || {
        said(&ada).contains("The song's audio files go on through the meeting point")
    });
    assert!(ada.together().files_wait.is_none());
    let _ = before;
}
