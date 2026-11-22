//! The audio of a shared song between the programs that hold it: each a
//! runtime of its own, connected over this machine's loopback and nothing
//! else, with files this test made in a temporary folder.
//!
//! The rule the whole of it stands on: **a path another program says is
//! never opened, read, written or handed out.** A file is asked for by
//! what it is, handed out only if the open song plays it, and taken in
//! only when its bytes are what was asked for. The strangers here hold
//! the invitation and speak the protocol by hand.
//!
//! Connections made by a run of this file: 14.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::files::{Content, FileIs};
use trill_core::merge::{AudioSet, Change, ClipOp, Floors, Op, Said, Stamp, said_song};
use trill_core::project::{AudioContent, ClipContent};
use trill_core::{ClipId, TrackId};
use trill_engine::AudioBackend;
use trill_engine::render::RenderOptions;
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Reach, Runtime, RuntimeOptions, Storage};
use trill_together::files::content_of;
use trill_together::{Event, Invitation, MAX_PIECE, MAX_WANT, Message, Net, PROTOCOL, Person, secure};

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

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The song a program holds as two that hold the same song both have it:
/// to the last id, without what is each one's own.
fn text(runtime: &Runtime) -> String {
    let mut song = said_song(runtime.session.lock().project());
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

fn content(path: &Path) -> Content {
    content_of(path, |_| true).unwrap().unwrap()
}

/// What a clip of a program's song plays: where the program has the
/// file, and what the song says it is.
fn file_of(runtime: &Runtime, clip: ClipId) -> (String, Option<FileIs>) {
    let session = runtime.session.lock();
    let song = session.project();
    let takes = song.tracks.iter().flat_map(|track| track.takes.iter().map(|take| &take.clip));
    let found = song.clip(clip).ok().or_else(|| takes.clone().find(|take| take.id == clip));
    let audio = found.and_then(|clip| clip.audio()).cloned().unwrap_or_default();
    (audio.path, audio.is)
}

/// Whether the clip's file is on this program's disk, in its folder of
/// files from other programs, with the bytes of `original`.
fn arrived(runtime: &Runtime, received: &Path, clip: ClipId, original: &Path) -> bool {
    let (path, is) = file_of(runtime, clip);
    let Some(is) = is else { return false };
    let here = Path::new(&path);
    here == received.join(is.kept_as())
        && std::fs::read(here).is_ok_and(|bytes| Some(bytes) == std::fs::read(original).ok())
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

/// A program with a song that plays one recording of its own.
fn with_a_recording(folder: &Path, name: &str) -> (Runtime, TrackId, ClipId, PathBuf) {
    let runtime = start(folder, name);
    let vocal = tone(folder.join(format!("{name}'s music")).join("Night session").join("vocal take.wav"), 220.0, 1.0);
    let (track, clip) = {
        let mut session = runtime.session.lock();
        let track = session.edit(|song| Ok(song.add_track("Vocal"))).unwrap();
        let recording = AudioContent::new(vocal.to_string_lossy());
        let clip = session.edit(|song| song.add_audio_clip(track, "Verse vocal", 0.0, 2.0, recording)).unwrap();
        (track, clip)
    };
    (runtime, track, clip, vocal)
}

#[test]
fn the_audio_of_a_song_arrives_with_the_song_by_what_it_is_and_nobody_is_told_a_path() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, track, verse, vocal) = with_a_recording(folder.path(), "Ada");
    assert!(file_of(&ada, verse).1.is_none(), "a song of one's own says nothing of what its files are");
    let ben = start(folder.path(), "Ben");
    let at_ben = folder.path().join("Ben").join("received");

    // Ada shares and Ben joins: he has the song, then what it plays.
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    ben.join(&invitation, false).unwrap();
    eventually("Ada has said what her file is", || file_of(&ada, verse).1.is_some());
    let is = file_of(&ada, verse).1.unwrap();
    assert_eq!((is.content(), is.name.as_str()), (content(&vocal), "vocal take.wav"));
    assert_eq!(file_of(&ada, verse).0, vocal.to_string_lossy(), "and has it where she had it");
    eventually("the recording is at Ben's", || arrived(&ben, &at_ben, verse, &vocal));
    eventually("and nothing is on its way any more", || ben.together().files.is_empty());
    assert_eq!(files_in(&at_ben), [format!("{}.wav", is.sha256)], "under its hash, and nothing beside it");
    assert_eq!(text(&ada), text(&ben), "the same song");
    // He hears it, without the song having been opened again.
    eventually("Ben's song sounds", || loudest(&ben) > 0.2);

    // Nothing Ben holds or was told names a place of Ada's.
    let held = ben.session.lock().project().to_json();
    assert!(!held.contains("Ada's music") && !held.contains("Night session"), "{held}");

    // What is added later follows by itself, either way: a clip Ada
    // drops in, a take, and a recording of Ben's own.
    let chorus = tone(folder.path().join("Ada's music").join("chorus.wav"), 330.0, 0.5);
    let ad_lib = tone(folder.path().join("Ada's music").join("ad lib 3.wav"), 440.0, 0.5);
    let (second, take) = {
        let mut session = ada.session.lock();
        let chorus = AudioContent::new(chorus.to_string_lossy());
        let second = session.edit(|song| song.add_audio_clip(track, "Chorus", 4.0, 1.0, chorus)).unwrap();
        let ad_lib = AudioContent::new(ad_lib.to_string_lossy());
        let take = session.edit(|song| song.add_take(track, "Ad lib 3", 8.0, 1.0, ad_lib)).unwrap();
        (second, take)
    };
    let guitar = tone(folder.path().join("Ben's desk").join("guitar.wav"), 110.0, 0.75);
    let riff = {
        let mut session = ben.session.lock();
        let recording = AudioContent::new(guitar.to_string_lossy());
        session.edit(|song| song.add_audio_clip(track, "Riff", 12.0, 1.5, recording)).unwrap()
    };
    let at_ada = folder.path().join("Ada").join("received");
    eventually("Ada's clip and take are at Ben's", || {
        arrived(&ben, &at_ben, second, &chorus) && arrived(&ben, &at_ben, take, &ad_lib)
    });
    eventually("and Ben's recording at Ada's", || arrived(&ada, &at_ada, riff, &guitar));
    assert_eq!(file_of(&ben, riff).0, guitar.to_string_lossy(), "which he has where he had it");
    assert_eq!(text(&ada), text(&ben));
    assert!(!ada.session.lock().project().to_json().contains("Ben's desk"));

    // From anyone in the song who has it, not only from who added it:
    // Ada leaves, Ben shares on, and a third gets everything from him.
    ada.leave();
    eventually("Ada is gone", || ben.together().people.is_empty());
    let onward = ben.share(Reach::ThisMachine, 0).unwrap();
    let cy = start(folder.path(), "Cy");
    let at_cy = folder.path().join("Cy").join("received");
    cy.join(&onward, false).unwrap();
    eventually("Cy has all four recordings", || {
        arrived(&cy, &at_cy, verse, &vocal)
            && arrived(&cy, &at_cy, second, &chorus)
            && arrived(&cy, &at_cy, take, &ad_lib)
            && arrived(&cy, &at_cy, riff, &guitar)
    });
    assert_eq!(files_in(&at_cy).len(), 4);
    assert_eq!(text(&ben), text(&cy));
    let held = cy.session.lock().project().to_json();
    assert!(!held.contains("Ada's music") && !held.contains("Ben's desk"), "{held}");
    for told in [said(&ada), said(&ben), said(&cy)] {
        assert!(!told.contains("thrown away") && !told.contains("no longer connected"), "{told}");
    }
}

// ---- strangers --------------------------------------------------------------

/// A program that has the invitation and speaks the protocol by hand.
fn by_hand(invitation: &str) -> (secure::Sayer, secure::Hearer) {
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
    let (mut say, mut hearer) = by_hand(invitation);
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
            said @ (Message::Have { .. } | Message::Nothing { .. } | Message::Piece { .. }) => return said,
            _ => continue,
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
    assert!(!handed.contains("\"files\""), "and nothing of where its files are from its folder");
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
        say.say(&Message::Want { file: file.clone(), at: 0, len: 1000 }.encode()).unwrap();
        assert_eq!(next_of_files(&mut hearer), Message::Nothing { file: file.clone() }, "{file}");
    }
    say.say(&Message::Want { file: song_file.clone(), at: song_file.bytes, len: 1000 }.encode()).unwrap();
    assert_eq!(next_of_files(&mut hearer), Message::Nothing { file: song_file.clone() });

    // Making the song name the file does not get it either: a clip that
    // arrives saying where the file lies and what it is. The place is not
    // taken, so Ada's program has no such file for the song, and looks
    // for it among what arrived from others, where it is not.
    let planted = {
        let mut clip = ada.session.lock().project().tracks[0].clips[0].clone();
        clip.id = ClipId(77 << 20 | 5);
        clip.name = "Planted".into();
        let mut audio = AudioContent::new(letters.to_string_lossy());
        audio.is = Some(FileIs::new(other.clone(), "demo for the label.wav"));
        clip.content = ClipContent::Audio(audio);
        clip
    };
    let change = Change {
        stamp: Stamp(500, 77),
        ops: vec![Op::Clip(planted.id, ClipOp::Whole(Box::new(planted.clone()), track))],
    };
    let changes =
        Message::Changes { said: vec![Said { change, knew: None }], floors: Floors::default(), replica: Some(77) };
    say.say(&changes.encode()).unwrap();
    eventually("Ada has the clip", || ada.session.lock().project().clip(planted.id).is_ok());
    let at_ada = folder.path().join("Ada").join("received");
    let (path, is) = {
        let session = ada.session.lock();
        let audio = session.project().clip(planted.id).unwrap().audio().unwrap().clone();
        (audio.path, audio.is.unwrap())
    };
    assert_eq!(Path::new(&path), at_ada.join(is.kept_as()), "its place is one Ada's program made");
    assert!(!path.contains("demo for the label"), "{path}");
    // (Long enough for her program to have looked at its files again.)
    std::thread::sleep(Duration::from_secs(2));
    say.say(&Message::Want { file: other.clone(), at: 0, len: 1000 }.encode()).unwrap();
    loop {
        match next_of_files(&mut hearer) {
            Message::Have { files } => assert!(!files.contains(&other), "Ada says she has the file"),
            answer => break assert_eq!(answer, Message::Nothing { file: other.clone() }),
        }
    }
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
    let place = Op::Clip(planted.id, ClipOp::Audio(AudioSet::Path(letters.to_string_lossy().into_owned())));
    let change = Change { stamp: Stamp(600, 78), ops: vec![place] };
    let changes =
        Message::Changes { said: vec![Said { change, knew: None }], floors: Floors::default(), replica: Some(78) };
    say.say(&changes.encode()).unwrap();
    let (_, why) = until_bye(&mut hearer);
    assert!(why.contains("is not said to anyone else"), "{why}");
    assert_eq!(Path::new(&file_of(&ada, planted.id).0), at_ada.join(is.kept_as()), "the place is as it was");

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
    eventually("Ben has the recording", || arrived(&ben, &folder.path().join("Ben").join("received"), verse, &vocal));
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

#[test]
fn what_arrives_is_a_file_of_the_song_only_if_it_is_what_was_asked_for() {
    let folder = tempfile::tempdir().unwrap();
    // A song with a recording, as a program that shares it hands it over.
    let (eve, _, verse, vocal) = with_a_recording(folder.path(), "Eve");
    let _ = eve.share(Reach::ThisMachine, 0).unwrap();
    eventually("Eve's program has said what the file is", || file_of(&eve, verse).1.is_some());
    let song = eve.session.lock().whole_song();
    assert!(!song.contains("Eve's music"), "{song}");
    drop(eve);
    let real = std::fs::read(&vocal).unwrap();
    let file = content(&vocal);
    let stretch = |bytes: &[u8], at: u64, len: u64| -> Vec<(u64, Vec<u8>)> {
        let (from, to) = (at as usize, (at + len).min(bytes.len() as u64) as usize);
        bytes[from..to]
            .chunks(MAX_PIECE)
            .enumerate()
            .map(|(n, piece)| (at + (n * MAX_PIECE) as u64, piece.to_vec()))
            .collect()
    };

    // Other bytes than were asked for, of the right length: all of it is
    // thrown away, the user is told, and nothing of it is a file.
    let mut wrong = real.clone();
    wrong[real.len() / 2] ^= 0x55;
    let invitation = shared_by_hand(song.clone(), file.clone(), move |at, len| stretch(&wrong, at, len));
    let ben = start(folder.path(), "Ben");
    let at_ben = folder.path().join("Ben").join("received");
    ben.join(&invitation, false).unwrap();
    eventually("Ben is told what was thrown away", || said(&ben).contains("was not the file the song names"));
    assert!(said(&ben).contains("'vocal take.wav'") && said(&ben).contains("Mallory"), "{}", said(&ben));
    assert!(files_in(&at_ben).is_empty(), "nothing of it is left: {:?}", files_in(&at_ben));
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
    let at_cy = folder.path().join("Cy").join("received");
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
    let at_dee = folder.path().join("Dee").join("received");
    dee.join(&invitation, false).unwrap();
    eventually("Dee lets go of it", || said(&dee).contains("sent bytes of a file that were not asked for"));
    // Neither kept a byte more than the file is: what there is under its
    // name, if the whole of it arrived before the rest, is the file.
    for folder in [&at_cy, &at_dee] {
        for name in files_in(folder) {
            let kept = std::fs::read(folder.join(&name)).unwrap();
            assert!(kept.len() <= real.len() && kept == real[..kept.len()], "{name} is {} bytes", kept.len());
        }
    }

    // Bytes that lie elsewhere in the file than was asked.
    let shifted = real.clone();
    let invitation = shared_by_hand(song, file, move |at, len| {
        stretch(&shifted, at, len).into_iter().map(|(at, bytes)| (at + 1, bytes)).collect()
    });
    let fay = start(folder.path(), "Fay");
    fay.join(&invitation, false).unwrap();
    eventually("Fay lets go of it", || said(&fay).contains("sent bytes of a file that were not asked for"));
    assert!(files_in(&folder.path().join("Fay").join("received")).iter().all(|name| name.ends_with(".part")));
}
