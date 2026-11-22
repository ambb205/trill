//! Several sessions that hold one song: what each changes reaches the
//! others, each undoes its own work, a clash is a sentence with a way
//! back, and a session that goes away and comes back merges.
//!
//! No wire here: what one session says is handed to another by the test,
//! which is what the wire does (`trill-together`, `trill-runtime`).

use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory, EditorOptions};
use trill_core::files::{Content, FileIs, FilePlace};
use trill_core::journal::Author;
use trill_core::merge::lot::{Lot, any_edit, check, note};
use trill_core::merge::{Change, Op, Said, Stamp, TrackOp, TrackSet};
use trill_core::project::{AudioContent, AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, ClipId, DeviceId, Project, TrackId, TrackInput, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::scripted::{BASS, Loading, ScriptedFactory};
use trill_engine::{AudioBackend, Problem, Session};

fn session() -> Session {
    let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    session.hear_changes(true);
    session
}

/// A session with a song in it, shared, and another that was handed it.
fn two() -> (Session, Session) {
    let mut a = session();
    let keys = a.edit(|song| Ok(song.add_track("Keys"))).unwrap();
    a.edit(|song| Ok(song.add_track("Bass"))).unwrap();
    a.edit(|song| Ok(song.add_track_of_kind("Room", TrackKind::Return))).unwrap();
    a.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
    let verse = a.edit(|song| song.add_midi_clip(keys, "Verse", 0.0, 8.0)).unwrap();
    a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(60, 0.0), note(64, 1.0), note(67, 2.0)])).unwrap();
    a.share();
    let mut b = session();
    b.take_song(&a.whole_song()).unwrap();
    a.take_said();
    assert_eq!(text(&a), text(&b), "the one who joined has the song");
    assert_eq!(a.shared_song(), b.shared_song());
    (a, b)
}

/// The song a session holds, as one who compares two sessions' reads it:
/// to the last id, and without what is each one's own (its listening, and
/// where it has the song's files).
fn text(session: &Session) -> String {
    let mut song = session.said_song();
    song.count_ids_from(0);
    song.to_json()
}

/// What `from` changed since it was last asked reaches `to`, under the
/// name `by`. Returns how many changes that was.
fn pass(from: &mut Session, to: &mut Session, by: &str) -> usize {
    let said = from.take_said();
    let count = said.len();
    let floors = from.floors();
    to.merge(by, said, Some(&floors), from.replica()).expect("what a session says is taken");
    count
}

fn meet(a: &mut Session, b: &mut Session) {
    pass(a, b, "Ada");
    pass(b, a, "Ben");
    assert_eq!(text(a), text(b), "both hold the same song");
    check(a.project());
}

fn track(session: &Session, name: &str) -> TrackId {
    session.project().tracks.iter().find(|track| track.name == name).map(|track| track.id).expect("the track is there")
}

fn clip(session: &Session, name: &str) -> ClipId {
    let clips = session.project().tracks.iter().flat_map(|track| &track.clips);
    clips.into_iter().find(|clip| clip.name == name).map(|clip| clip.id).expect("the clip is there")
}

/// A session whose plugins take a moment to make, as plugins do, and the
/// factory that says how often one was made.
fn with_plugins() -> (Session, ScriptedFactory) {
    let loading = Loading { making: Duration::from_millis(60), window: true, ..Loading::default() };
    let scripted = ScriptedFactory::loading(loading);
    let factory: Arc<dyn DeviceFactory> =
        Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())]));
    let mut session = Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    session.hear_changes(true);
    (session, scripted)
}

/// Does the session's chores until `done`.
fn chores(session: &mut Session, what: &str, done: impl Fn(&Session) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        session.housekeeping();
        if done(session) {
            return;
        }
        assert!(Instant::now() < deadline, "never came to it: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_preset_a_plugin_loads_in_one_program_reaches_the_other_as_its_state_and_makes_the_plugin_there_once() {
    let ((mut a, _), (mut b, made_in_b)) = (with_plugins(), with_plugins());
    let track = a.edit(|song| Ok(song.add_track("Bass"))).unwrap();
    let bass = a.add_device(ChainRef::Track(track), BASS, Some("up"), None).unwrap();
    assert!(a.wait_until_opened(Duration::from_secs(20)));
    // Sharing reads every plugin's state: whoever joins is handed the bass as it is running.
    a.share();
    let up = a.project().device(bass).unwrap().state.clone();
    assert!(up.is_some() && a.project().device(bass).unwrap().from_preset.is_none());
    b.take_song(&a.whole_song()).unwrap();
    a.take_said();
    chores(&mut b, "the bass is there", |b| b.arriving().is_none() && !b.making_devices());
    assert_eq!(made_in_b.made().len(), 1, "{:?}", made_in_b.made());

    // The bass loads another preset itself, in the first program: the song
    // names it at once, and has no state until the plugin has been read.
    a.load_preset(bass, "soft").unwrap();
    assert!(a.project().device(bass).unwrap().state.is_none());
    pass(&mut a, &mut b, "Ada");
    chores(&mut b, "nothing is being made", |b| b.arriving().is_none() && !b.making_devices());
    assert_eq!(b.project().device(bass).unwrap().preset.as_ref().map(|preset| preset.id.as_str()), Some("soft"));
    assert_eq!(made_in_b.made().len(), 1, "what runs in the other goes on until the state is there");
    assert!(b.problems().is_empty(), "{:?}", b.problems());

    // The first program's chores read the state once it has stayed the
    // same for a moment, and say it; the other makes its bass from it.
    chores(&mut a, "the state was read back", |a| a.project().device(bass).is_ok_and(|device| device.state.is_some()));
    let soft = a.project().device(bass).unwrap().state.clone();
    assert!(soft != up);
    assert_eq!(pass(&mut a, &mut b, "Ada"), 1);
    chores(&mut b, "the bass is there again", |b| b.arriving().is_none() && !b.making_devices() && !b.is_coming(bass));
    assert_eq!(b.project().device(bass).unwrap().state, soft);
    assert_eq!(made_in_b.made().len(), 2, "made again once, from the state: {:?}", made_in_b.made());
    // And what the other's plugin writes out is not news to the first.
    chores(&mut b, "a moment passes", |_| true);
    b.save(&std::env::temp_dir().join(format!("trill-together-{}-b.trill", std::process::id()))).unwrap();
    assert_eq!(pass(&mut b, &mut a, "Ben"), 0, "the other said its own reading of the state");
    let _ = std::fs::remove_file(std::env::temp_dir().join(format!("trill-together-{}-b.trill", std::process::id())));
    meet(&mut a, &mut b);
}

#[test]
fn what_is_changed_in_a_plugins_own_window_reaches_the_other_once_it_has_settled_and_is_said_once() {
    let ((mut a, in_a), (mut b, in_b)) = (with_plugins(), with_plugins());
    let track = a.edit(|song| Ok(song.add_track("Bass"))).unwrap();
    let bass = a.add_device(ChainRef::Track(track), BASS, Some("up"), None).unwrap();
    assert!(a.wait_until_opened(Duration::from_secs(20)));
    a.share();
    let up = a.project().device(bass).unwrap().state.clone();
    b.take_song(&a.whole_song()).unwrap();
    a.take_said();
    chores(&mut b, "the bass is there", |b| b.arriving().is_none() && !b.making_devices());
    assert_eq!(b.project().device(bass).unwrap().state, up);

    // Its window is opened in the first program, and a knob is turned in
    // it: nobody asked the session for anything.
    let options = EditorOptions { title: String::new(), owner: None, position: None, activate: false };
    a.open_device_editor(bass, &options).unwrap();
    let (revision, steps) = (b.revision(), a.can_undo());
    assert_eq!(in_a.turned_in_its_window("soft"), 1);
    assert!(a.take_said().is_empty(), "nothing is said of a window that was opened");
    // The chores read the plugin until it has stayed the same for a
    // moment, and its state is said then, whole.
    chores(&mut a, "what the plugin came to is in the song", |a| a.project().device(bass).unwrap().state != up);
    let soft = a.project().device(bass).unwrap().state.clone();
    assert!(soft.is_some() && a.can_undo() == steps, "and it was no step");
    assert_eq!(pass(&mut a, &mut b, "Ada"), 1, "said once");
    assert!(b.revision() != revision);
    chores(&mut b, "the bass is made from it", |b| b.arriving().is_none() && !b.making_devices() && !b.is_coming(bass));
    assert_eq!(b.project().device(bass).unwrap().state, soft);
    assert_eq!(in_b.made().len(), 2, "the other's bass was made again once: {:?}", in_b.made());

    // While the window stays open and nothing is turned, nothing more is
    // said; and the other's own reading of its plugin is not news.
    let until = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < until {
        a.housekeeping();
        b.housekeeping();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(a.take_said().is_empty() && b.take_said().is_empty(), "a state that was said is not said again");
    // Another turn of the knob is another state, said when it has settled.
    assert_eq!(in_a.turned_in_its_window("up"), 1);
    chores(&mut a, "the plugin is read again", |a| a.project().device(bass).unwrap().state == up);
    assert_eq!(pass(&mut a, &mut b, "Ada"), 1);
    a.close_device_editor(bass);
    meet(&mut a, &mut b);
}

// ---- what is recorded --------------------------------------------------------

/// What a track plays of recordings: where each lies, and its file by
/// the name the song says it by (where each program has it is its own).
fn plays(session: &Session, track: TrackId) -> Vec<(f64, f64, String)> {
    let track = session.project().track(track).unwrap();
    let named = |audio: &AudioContent| audio.is.as_ref().map_or_else(|| audio.path.clone(), |is| is.name.clone());
    track.clips.iter().filter_map(|clip| Some((clip.start, clip.end(), named(clip.audio()?)))).collect()
}

/// The takes kept beside a track: each one's lane and name, lane by lane.
fn lanes(session: &Session, track: TrackId) -> Vec<(u32, String)> {
    let track = session.project().track(track).unwrap();
    let mut takes: Vec<(u32, String)> = track.takes.iter().map(|take| (take.lane, take.clip.name.clone())).collect();
    takes.sort();
    takes
}

/// A recording whose maker has read it already: the song says what it is.
fn recording(name: &str) -> AudioContent {
    let mut recording = AudioContent::new(format!("{name}.wav"));
    let of_its_name = [name.bytes().fold(7u8, |sum, byte| sum.wrapping_mul(31).wrapping_add(byte)); 32];
    recording.is = Some(FileIs::new(Content::new(&of_its_name, 48_000), &recording.path));
    recording
}

#[test]
fn takes_and_what_is_made_of_them_are_the_songs_and_which_input_a_track_records_is_ones_own() {
    let (mut a, mut b) = two();
    let bass = track(&a, "Bass");
    a.edit(|song| song.add_audio_clip(bass, "Bed", 0.0, 16.0, recording("bed")).map(drop)).unwrap();
    let first = a.edit(|song| song.add_take(bass, "Take 1", 0.0, 8.0, recording("one"))).unwrap();
    let second = a.edit(|song| song.add_take(bass, "Take 2", 4.0, 8.0, recording("two"))).unwrap();
    a.edit(|song| song.comp_take(second, 6.0, 10.0).map(drop)).unwrap();
    a.edit(|song| song.rename_take(first, "The good one")).unwrap();
    meet(&mut a, &mut b);
    // (What the stretch covered of the bed is kept as a take first.)
    let kept = [(0, "The good one".to_owned()), (1, "Take 2".to_owned()), (2, "Bed".to_owned())];
    assert_eq!(lanes(&b, bass), kept);
    let played = plays(&b, bass);
    assert_eq!(
        played.iter().map(|(start, end, _)| (*start, *end)).collect::<Vec<_>>(),
        [(0.0, 6.0), (6.0, 10.0), (10.0, 16.0)]
    );
    assert_eq!(played[1].2, "two.wav");
    // The seams are faded across, there as here: that is the clip's.
    let faded = |session: &Session| -> Vec<bool> {
        let clips = &session.project().track(bass).unwrap().clips;
        clips.iter().filter_map(|clip| Some(clip.audio()?.crossfade)).collect()
    };
    assert_eq!(faded(&b), [false, true, true]);
    assert_eq!(faded(&a), faded(&b));
    // The other chooses the first take over all of it, and removes the second.
    b.edit(|song| song.choose_take(first).map(drop)).unwrap();
    b.edit(|song| song.remove_take(second).map(drop)).unwrap();
    meet(&mut a, &mut b);
    assert_eq!(plays(&a, bass)[0], (0.0, 8.0, "one.wav".to_owned()));
    assert!(a.project().track(bass).unwrap().take(second).is_none());

    // Which input of the machine's a track records is said to nobody,
    // and nobody's is taken from anyone.
    let mine = TrackInput { channel: 3, stereo: true, monitor: Default::default() };
    a.edit(|song| song.set_track_input(bass, Some(mine))).unwrap();
    assert!(a.take_said().is_empty(), "one's own input is not said");
    b.edit(|song| song.set_track_input(bass, Some(TrackInput::mono(1)))).unwrap();
    meet(&mut a, &mut b);
    assert_eq!(a.project().track(bass).unwrap().input, Some(mine));
    assert_eq!(b.project().track(bass).unwrap().input, Some(TrackInput::mono(1)));
    assert!(!a.whole_song().contains("\"input\""), "and it is not in the song that is handed to whoever joins");
    let someone =
        Change { stamp: Stamp(900, 77), ops: vec![Op::Track(bass, TrackOp::Set(TrackSet::Input(Some(mine))))] };
    let refused = b.merge("someone", vec![Said { change: someone, knew: None }], None, None).unwrap_err();
    assert!(refused.to_string().contains("one's own"), "{refused}");
    // Each undoes their own: the input, and nothing of the other's.
    assert!(a.undo());
    assert_eq!(a.project().track(bass).unwrap().input, None);
    meet(&mut a, &mut b);
    assert_eq!(b.project().track(bass).unwrap().input, Some(TrackInput::mono(1)));
}

#[test]
fn two_takes_put_in_one_lane_at_the_same_moment_lie_in_two_and_one_goes_back_when_there_is_room() {
    let (mut a, mut b) = two();
    let bass = track(&a, "Bass");
    a.edit(|song| song.add_take(bass, "Ada's", 0.0, 8.0, recording("ada")).map(drop)).unwrap();
    b.edit(|song| song.add_take(bass, "Ben's", 4.0, 8.0, recording("ben")).map(drop)).unwrap();
    assert_eq!((lanes(&a, bass)[0].0, lanes(&b, bass)[0].0), (0, 0), "each in the first lane, where it was made");
    meet(&mut a, &mut b);
    let both = lanes(&a, bass);
    assert_eq!(both.iter().map(|(lane, _)| *lane).collect::<Vec<_>>(), [0, 1], "{both:?}");
    assert_eq!(lanes(&b, bass), both);
    // Whoever has the first lane removes theirs: the other lies where it
    // was put, here at once and there when it hears of it.
    let in_first = both[0].1.clone();
    let (first, other) = match in_first.as_str() {
        "Ada's" => (&mut a, &mut b),
        _ => (&mut b, &mut a),
    };
    let take = first.project().track(bass).unwrap().takes.iter().find(|take| take.lane == 0).unwrap().clip.id;
    first.edit(|song| song.remove_take(take).map(drop)).unwrap();
    assert_eq!(lanes(first, bass), [(0, both[1].1.clone())]);
    meet(first, other);
    assert_eq!(lanes(other, bass), [(0, both[1].1.clone())]);
    // Taken back, it has its lane again and the other gives way again.
    assert!(first.undo());
    meet(first, other);
    assert_eq!(lanes(other, bass), both);
}

#[test]
fn of_two_recordings_put_over_one_stretch_at_the_same_moment_the_later_plays_and_the_other_can_be_put_back() {
    let (mut a, mut b) = two();
    let bass = track(&a, "Bass");
    a.edit(|song| song.add_audio_clip(bass, "Bed", 0.0, 16.0, recording("bed")).map(drop)).unwrap();
    let one = a.edit(|song| song.add_take(bass, "Take 1", 0.0, 16.0, recording("one"))).unwrap();
    let two = a.edit(|song| song.add_take(bass, "Take 2", 0.0, 16.0, recording("two"))).unwrap();
    meet(&mut a, &mut b);
    // Each puts a stretch of a take over beats 4 to 8, neither knowing of
    // the other. Ben's is the later: he had done something else first.
    a.edit(|song| song.comp_take(one, 4.0, 8.0).map(drop)).unwrap();
    b.edit(|song| song.set_track_pan(bass, 0.1)).unwrap();
    b.edit(|song| song.comp_take(two, 4.0, 8.0).map(drop)).unwrap();
    let with = |file: &str| -> Vec<(f64, f64, String)> {
        vec![(0.0, 4.0, "bed.wav".to_owned()), (4.0, 8.0, format!("{file}.wav")), (8.0, 16.0, "bed.wav".to_owned())]
    };
    assert_eq!((plays(&a, bass), plays(&b, bass)), (with("one"), with("two")));

    // The later of the two is told nothing, and gives way to nothing:
    // it has only what it did itself to say.
    pass(&mut a, &mut b, "Ada");
    assert!(b.notices().is_empty(), "{:?}", b.notices());
    // The earlier makes room under the later, as if that had been made
    // after it, and is told once.
    assert_eq!(pass(&mut b, &mut a, "Ben"), 2);
    assert_eq!(plays(&a, bass), with("two"), "the later is what the track plays");
    let notices = a.notices().to_vec();
    assert_eq!(notices.len(), 1, "one sentence for the stretch, not one a clip: {notices:?}");
    assert!(notices[0].text.contains("Ben put a recording on the track \u{201C}Bass\u{201D} over the same stretch"));
    assert!(notices[0].text.contains("Yours is kept as a take"), "{}", notices[0].text);
    assert_eq!(notices[0].remedy.as_ref().map(|remedy| remedy.label.as_str()), Some("Put mine back"));
    meet(&mut a, &mut b);
    assert_eq!(plays(&b, bass), with("two"), "and the same everywhere");
    assert!(b.notices().is_empty());
    for take in [one, two] {
        assert!(b.project().track(bass).unwrap().take(take).is_some(), "nothing that was recorded is lost");
    }

    // Put back: one edit, which covers theirs as a recording that lands
    // there does, and which its maker can take back.
    a.remedy(notices[0].id).unwrap();
    assert_eq!(plays(&a, bass), with("one"));
    assert!(a.notices().is_empty());
    meet(&mut a, &mut b);
    assert_eq!(plays(&b, bass), with("one"));
    assert!(a.undo());
    meet(&mut a, &mut b);
    assert_eq!(plays(&b, bass), with("two"));
}

/// The paths of the audio files a session's song cannot play.
fn missing(session: &Session) -> Vec<String> {
    let files = session.problems().iter().filter_map(|problem| match problem {
        Problem::AudioFile { path, .. } => Some(path.clone()),
        _ => None,
    });
    files.collect()
}

/// Where a session has the file of a clip or a take, and what the song
/// says the file is.
fn file_of(session: &Session, clip: ClipId) -> (String, Option<FileIs>) {
    let found = session.files_here().into_iter().find(|file| file.place == FilePlace::Clip(clip));
    found.map(|file| (file.path, file.is)).unwrap_or_default()
}

fn tone_at(file: &std::path::Path) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let tone: Vec<f32> = (0..48_000).map(|n| (n as f32 * 0.05).sin() * 0.2).collect();
    let buffer = trill_core::audio::AudioBuffer { sample_rate: 48_000.0, left: tone.clone(), right: tone };
    trill_media::write_wav(file, &buffer, trill_media::WavFormat::Float32).unwrap();
}

/// Some bytes a file might be: what whoever read it would have found.
fn found_to_be(of: u8, file: &std::path::Path) -> FileIs {
    FileIs::new(Content::new(&[of; 32], std::fs::metadata(file).unwrap().len()), &file.to_string_lossy())
}

#[test]
fn a_take_whose_file_the_other_has_not_got_has_a_place_of_the_others_own_and_plays_when_the_file_is_there() {
    let (mut a, mut b) = two();
    let bass = track(&a, "Bass");
    let folder = tempfile::tempdir().unwrap();
    b.keep_received_in(Some(folder.path().join("at Ben's")));
    let file = folder.path().join("Ada's takes").join("Bass.wav");
    tone_at(&file);
    let named = AudioContent::new(file.to_string_lossy());
    // (Her own hand puts it there.)
    let hand = trill_engine::hand::by_hand();
    let take = a.edit(|song| song.add_take(bass, "Take", 0.0, 4.0, named)).unwrap();
    a.edit(|song| song.choose_take(take).map(drop)).unwrap();
    drop(hand);
    let played = plays(&a, bass).len();
    meet(&mut a, &mut b);
    // The other has the take and what the track plays of it, and no place
    // for the file: where its maker has it was not said, and nothing on
    // this disk is looked at for it.
    assert_eq!(lanes(&b, bass), [(0, "Take".to_owned())]);
    assert_eq!(plays(&b, bass).len(), played);
    assert_eq!(file_of(&b, take), (String::new(), None), "no place, and nothing said of what it is yet");
    assert_eq!(file_of(&a, take).0, file.to_string_lossy(), "its maker has it where it is");
    assert!(missing(&a).is_empty() && !missing(&b).is_empty(), "{:?}", b.problems());
    assert!(
        !format!("{:?} {}", b.problems(), b.project().to_json()).contains("Ada's takes"),
        "nothing names her folder"
    );

    // Its maker says what it is, once it has read it: the other gives it
    // a place of its own for that, in its folder of what this song brought.
    let is = found_to_be(0xab, &file);
    assert!(!a.say_file(&FilePlace::Clip(take), "another file.wav", is.clone()), "only of the file the clip names");
    // (Of the take, and of the clips the track plays of it.)
    let named: Vec<FilePlace> = a.files_here().into_iter().map(|here| here.place).collect();
    assert!(named.contains(&FilePlace::Clip(take)) && named.len() > 1, "{named:?}");
    for place in &named {
        assert!(a.say_file(place, &file.to_string_lossy(), is.clone()));
        assert!(!a.say_file(place, &file.to_string_lossy(), is.clone()), "said once");
    }
    meet(&mut a, &mut b);
    let song = format!("{:032x}", b.shared_song().unwrap().0);
    assert_eq!(b.received_in(), Some(folder.path().join("at Ben's").join(&song)), "a folder for this song alone");
    let home = folder.path().join("at Ben's").join(&song).join(format!("{}.wav", is.sha256));
    assert_eq!(file_of(&b, take), (home.to_string_lossy().into_owned(), Some(is.clone())));
    assert_eq!(file_of(&a, take).0, file.to_string_lossy(), "the maker's place is as it was");
    // (The clips the track plays of the take name the same file.)
    assert_eq!(b.files_here().len(), named.len());
    assert!(b.files_here().iter().all(|here| here.path == home.to_string_lossy()), "{:?}", b.files_here());
    assert!(b.files_here().iter().all(|here| here.may_go), "what the song brought is the song's to hand on");
    // It is not here yet: on its way, which is no problem and no missing
    // file. The clips that play it wait for it.
    assert!(missing(&b).is_empty(), "{:?}", b.problems());
    assert!(
        !b.files_arriving().is_empty() && b.files_arriving().iter().all(|place| matches!(place, FilePlace::Clip(_)))
    );
    assert!(b.is_awaited(&home.to_string_lossy()) && !a.is_awaited(&file.to_string_lossy()));
    assert_eq!(b.said_files(), [(is.clone(), home.clone())]);

    // The other goes on working meanwhile, and what it does is the maker's too.
    b.edit(|song| song.set_track_volume(bass, -3.0)).unwrap();
    b.edit(|song| song.rename_take(take, "Ada's take")).unwrap();
    // A clip the other makes of the recording plays, at its maker's, the
    // maker's own file: nobody is asked for what one has.
    let played = b.project().track(bass).unwrap().clips[0].id;
    let copy = b.edit(|song| song.duplicate_clip(played, 32.0, None)).unwrap();
    meet(&mut a, &mut b);
    assert_eq!(lanes(&a, bass), [(0, "Ada's take".to_owned())]);
    assert_eq!(file_of(&a, take).0, file.to_string_lossy());
    assert_eq!(file_of(&a, copy), (file.to_string_lossy().into_owned(), Some(is.clone())));
    assert_eq!(file_of(&b, copy).0, home.to_string_lossy());

    // The file arrives: it plays, without the song being opened again,
    // and that is no change to the song.
    let (revision, said) = (b.revision(), b.take_said().len());
    std::fs::create_dir_all(home.parent().unwrap()).unwrap();
    std::fs::copy(&file, &home).unwrap();
    b.files_are_here(std::slice::from_ref(&home));
    assert!(missing(&b).is_empty() && b.files_arriving().is_empty(), "{:?}", b.problems());
    assert!(b.revision() > revision && b.take_said().is_empty(), "{said}");

    // A place one gives a file oneself is one's own too: nobody hears of
    // it, and what the file is stays what the song says until it is read.
    let elsewhere = folder.path().join("found again").join("Bass.wav");
    tone_at(&elsewhere);
    let relink = trill_core::Relink { place: FilePlace::Clip(take), path: elsewhere.to_string_lossy().into() };
    assert_eq!(b.relink(&[relink]).unwrap(), 1);
    assert_eq!(pass(&mut b, &mut a, "Ben"), 0, "where one has a file is said to nobody");
    assert_eq!(file_of(&a, take).0, file.to_string_lossy());

    // A song that is handed over whole names no place either.
    let handed = a.whole_song();
    assert!(!handed.contains("Ada's takes") && !handed.contains("\"files\": ["), "{handed}");
    let mut c = session();
    c.keep_received_in(Some(folder.path().join("at Cy's")));
    c.take_song(&handed).unwrap();
    let at_cy = folder.path().join("at Cy's").join(&song).join(format!("{}.wav", is.sha256));
    assert_eq!(file_of(&c, take), (at_cy.to_string_lossy().into_owned(), Some(is)));
    assert_eq!(text(&a), text(&c));
}

#[test]
fn a_file_of_this_machines_goes_only_by_its_users_hand_and_a_song_file_is_a_stranger_too() {
    let folder = tempfile::tempdir().unwrap();
    let (kick, vocal, later) = (
        folder.path().join("samples").join("kick.wav"),
        folder.path().join("vocal.wav"),
        folder.path().join("later.wav"),
    );
    for file in [&kick, &vocal, &later] {
        tone_at(file);
    }
    let clip_of = |session: &mut Session, track: TrackId, name: &str, file: &std::path::Path, at: f64| {
        let recording = AudioContent::new(file.to_string_lossy());
        session.edit(|song| song.add_audio_clip(track, name, at, 2.0, recording)).unwrap()
    };
    // A song of one's own says nothing of files, and holds none back.
    let mut a = session();
    let track = a.edit(|song| Ok(song.add_track("Audio"))).unwrap();
    let first = clip_of(&mut a, track, "Kick", &kick, 0.0);
    assert!(a.files_held().is_empty() && a.received_in().is_none());
    assert!(!a.say_file(&FilePlace::Clip(first), &kick.to_string_lossy(), found_to_be(1, &kick)), "nothing is said");

    // Shared, and nobody has let anything go: what the song names is held.
    a.share();
    assert_eq!(a.files_held(), [kick.to_string_lossy()]);
    assert!(a.files_here().iter().all(|file| !file.may_go));
    assert!(!a.say_file(&FilePlace::Clip(first), &kick.to_string_lossy(), found_to_be(1, &kick)));
    assert!(a.said_files().is_empty(), "so nothing of it is the song's to hand out");

    // The user, who was shown what there is, lets it go.
    a.let_files_go();
    assert!(a.files_held().is_empty());
    assert!(a.say_file(&FilePlace::Clip(first), &kick.to_string_lossy(), found_to_be(1, &kick)));

    // What comes to name a file after that without the user's hand (an
    // assistant, a script) is held; what the hand itself brings goes.
    let by_another = clip_of(&mut a, track, "Vocal", &vocal, 4.0);
    assert_eq!(a.files_held(), [vocal.to_string_lossy()]);
    assert!(!a.say_file(&FilePlace::Clip(by_another), &vocal.to_string_lossy(), found_to_be(2, &vocal)));
    let hand = trill_engine::hand::by_hand();
    let by_hand = clip_of(&mut a, track, "Later", &later, 8.0);
    drop(hand);
    assert_eq!(a.files_held(), [vocal.to_string_lossy()], "the hand's own file is not among what is held");
    assert!(a.say_file(&FilePlace::Clip(by_hand), &later.to_string_lossy(), found_to_be(3, &later)));
    // Taking the hand's step back and making it again brings the same
    // file back, which goes as it did.
    let hand = trill_engine::hand::by_hand();
    assert!(a.undo() && a.redo());
    drop(hand);
    assert_eq!(a.files_held(), [vocal.to_string_lossy()]);

    // Saved, closed and opened again, by anyone: what the file names is
    // held again until its user has been shown it. A song file is not
    // trusted with what it names.
    let saved = folder.path().join("song.trill");
    a.save(&saved).unwrap();
    let mut again = session();
    again.load(&saved).unwrap();
    assert!(again.shared_song().is_some());
    let held = again.files_held();
    assert_eq!(held.len(), 3, "{held:?}");
    assert!(again.files_here().iter().all(|file| !file.may_go && file.path.contains("wav")));
    assert!(again.files_here().iter().any(|file| file.is.is_some()), "what was said of them is still the song's");
    again.let_files_go();
    assert!(again.files_held().is_empty());
}

#[test]
fn what_a_sampler_plays_is_said_without_its_place_and_has_a_place_made_here_at_the_other_side() {
    // (Sessions whose sample players can read files.)
    let reading = || {
        let media = Arc::new(trill_media::MediaCache::new(64 << 20));
        let factory = Arc::new(trill_dsp::Builtins::with_loader(media.clone()));
        let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        session.hear_changes(true);
        session
    };
    let mut a = reading();
    let keys = a.edit(|song| Ok(song.add_track("Keys"))).unwrap();
    a.share();
    let mut b = reading();
    let folder = tempfile::tempdir().unwrap();
    b.keep_received_in(Some(folder.path().join("at Ben's")));
    b.take_song(&a.whole_song()).unwrap();
    a.take_said();
    let song = format!("{:032x}", a.shared_song().unwrap().0);
    let named_by = |device: &trill_core::Device| -> Vec<String> {
        trill_core::DeviceFiles::files(&trill_dsp::SampleFiles, device).into_iter().map(|(_, path)| path).collect()
    };
    let names = |session: &Session, device: DeviceId| named_by(session.project().device(device).unwrap());

    // A file of Ada's, on this very disk, that her own hand gives a sampler.
    let kick = folder.path().join("Ada's samples").join("kick.wav");
    tone_at(&kick);
    let hand = trill_engine::hand::by_hand();
    let sampler = a.add_device(ChainRef::Track(keys), "builtin:sampler", None, None).unwrap();
    a.set_device_state(sampler, Some(trill_dsp::sampler::State::new(kick.to_string_lossy()).to_bytes())).unwrap();
    drop(hand);
    // What is said of it names no place: the slot, and the kind of file.
    let said = a.take_said();
    let mut states = 0;
    for part in said.iter().flat_map(|said| &said.change.ops) {
        let state = match part {
            Op::Device(_, trill_core::merge::DeviceOp::Whole(device, ..)) => named_by(device),
            Op::Device(_, trill_core::merge::DeviceOp::Set(trill_core::merge::DeviceSet::State(state))) => {
                let mut device = a.project().device(sampler).unwrap().clone();
                device.state = state.0.clone();
                named_by(&device)
            }
            _ => continue,
        };
        states += state.len();
        assert!(state.iter().all(|path| path == "sample.wav"), "{state:?}");
    }
    assert_eq!(states, 1, "the state was said");
    let floors = a.floors();
    b.merge("Ada", said, Some(&floors), a.replica()).unwrap();
    assert_eq!(names(&a, sampler), [kick.to_string_lossy()], "the maker's own state names its file where it is");
    let made = folder.path().join("at Ben's").join(&song).join("of devices").join(sampler.0.to_string());
    assert_eq!(names(&b, sampler), [made.join("sample.wav").to_string_lossy()], "the other's names a place made there");
    assert!(kick.exists(), "though the file is on this very disk");

    // Its maker says what the file is: the other gives the slot the place
    // for that, as it does for a clip, and has it handed over by that.
    let is = found_to_be(0xcd, &kick);
    let place = FilePlace::Sample { device: sampler, slot: "sample".into() };
    let here = a.files_here();
    assert_eq!((here.len(), &here[0].place, here[0].may_go), (1, &place, true));
    assert!(a.say_file(&place, &kick.to_string_lossy(), is.clone()));
    meet(&mut a, &mut b);
    let home = folder.path().join("at Ben's").join(&song).join(format!("{}.wav", is.sha256));
    assert_eq!(names(&b, sampler), [home.to_string_lossy()]);
    assert_eq!(names(&a, sampler), [kick.to_string_lossy()], "and its maker's is as it was");
    assert_eq!(b.said_files(), [(is.clone(), home.clone())]);
    assert_eq!(b.project().device(sampler).unwrap().files.get("sample"), Some(&is));
    let silent = |session: &Session| session.problems().iter().any(|problem| format!("{problem:?}").contains("sample"));
    // Its file is not here yet: on its way, and no problem.
    assert!(!silent(&b), "{:?}", b.problems());
    assert_eq!(b.files_arriving(), std::slice::from_ref(&place));
    // The file arrives: the sampler is made anew and has it.
    std::fs::create_dir_all(home.parent().unwrap()).unwrap();
    std::fs::copy(&kick, &home).unwrap();
    b.files_are_here(std::slice::from_ref(&home));
    assert!(!silent(&b) && b.files_arriving().is_empty(), "{:?}", b.problems());

    // The other turns a knob of the sampler's own, which is in its
    // state: the maker's place for the file stays the maker's.
    let mut state = trill_dsp::sampler::State::new(home.to_string_lossy());
    state.extra.insert("something of its own".into(), serde_json::json!(62));
    b.set_device_state(sampler, Some(state.to_bytes())).unwrap();
    meet(&mut a, &mut b);
    assert_eq!(names(&a, sampler), [kick.to_string_lossy()]);
    assert_eq!(names(&b, sampler), [home.to_string_lossy()]);

    // A song that is handed over names no place, and whoever takes it has
    // places of their own: for what is said, by what it is.
    let handed = a.whole_song();
    let mut c = reading();
    c.keep_received_in(Some(folder.path().join("at Cy's")));
    c.take_song(&handed).unwrap();
    let at_cy = folder.path().join("at Cy's").join(&song).join(format!("{}.wav", is.sha256));
    assert_eq!(names(&c, sampler), [at_cy.to_string_lossy()]);
    // What Ben was told he passes on as he holds it, without his places.
    let mut passed_on = 0;
    for said in b.since(&trill_core::merge::Floors::default()) {
        for op in said.change.ops {
            if let Op::Device(_, trill_core::merge::DeviceOp::Whole(device, ..)) = op {
                let named = named_by(&device);
                passed_on += named.len();
                assert!(named.iter().all(|path| path == "sample.wav"), "{named:?}");
            }
        }
    }
    assert_eq!(passed_on, 1, "the device and its file are among what is passed on");

    // A state that arrives naming a place, from a program that does not
    // keep to this: the place is not taken.
    let secret = folder.path().join("Ben's desk").join("letters.wav");
    tone_at(&secret);
    let planted = trill_dsp::sampler::State::new(secret.to_string_lossy()).to_bytes();
    let change = Change {
        stamp: Stamp(b.floors().0.values().max().copied().unwrap_or(0) + 50, 99),
        ops: vec![Op::Device(
            sampler,
            trill_core::merge::DeviceOp::Set(trill_core::merge::DeviceSet::State(trill_core::merge::Blob(Some(
                planted,
            )))),
        )],
    };
    b.merge("Mallory", vec![Said { change, knew: None }], None, Some(99)).unwrap();
    assert_eq!(names(&b, sampler), [home.to_string_lossy()], "the slot keeps the place this machine had for it");

    // Nor one that is spelled through the folder this machine keeps the
    // song's files in, and out of it again. Whoever is in the song knows
    // that folder's last name (it is the song's id), and so can spell any
    // file of this machine's as if it lay there.
    let through =
        folder.path().join("at Ben's").join(&song).join("..").join("..").join("Ben's desk").join("letters.wav");
    assert!(through.exists(), "the same letters, by another spelling");
    let planted = trill_dsp::sampler::State::new(through.to_string_lossy()).to_bytes();
    let change = Change {
        stamp: Stamp(b.floors().0.values().max().copied().unwrap_or(0) + 50, 99),
        ops: vec![Op::Device(
            sampler,
            trill_core::merge::DeviceOp::Set(trill_core::merge::DeviceSet::State(trill_core::merge::Blob(Some(
                planted,
            )))),
        )],
    };
    b.merge("Mallory", vec![Said { change, knew: None }], None, Some(99)).unwrap();
    assert_eq!(names(&b, sampler), [home.to_string_lossy()], "it is no place of this machine's, and is not taken");
    assert!(b.files_here().iter().all(|file| !file.path.contains("letters")), "{:?}", b.files_here());
    assert!(!b.is_awaited(&through.to_string_lossy()));
}

#[test]
fn a_path_that_begins_with_the_songs_own_folder_and_leaves_it_again_is_held_like_any_other() {
    let folder = tempfile::tempdir().unwrap();
    let memo = folder.path().join("music").join("memo.wav");
    tone_at(&memo);
    let mut a = session();
    a.keep_received_in(Some(folder.path().join("profile").join("received")));
    let track = a.edit(|song| Ok(song.add_track("Audio"))).unwrap();
    a.share();
    // The folder what the song brings is kept in, and the memo spelled
    // through it: what a song file someone else wrote can say, and what
    // an assistant's edit can.
    let own = a.received_in().unwrap();
    std::fs::create_dir_all(&own).unwrap();
    let through = own.join("..").join("..").join("..").join("music").join("memo.wav");
    assert!(through.exists(), "the same memo, by another spelling");
    let spelled = through.to_string_lossy().into_owned();
    let recording = AudioContent::new(spelled.clone());
    let clip = a.edit(|song| song.add_audio_clip(track, "Memo", 0.0, 2.0, recording)).unwrap();

    // It is not what the song brought here: nobody's hand let it go, so
    // it is held, nothing is said of it, and nothing is awaited there.
    let named = a.files_here();
    assert_eq!((named.len(), named[0].path.as_str(), named[0].may_go), (1, spelled.as_str(), false));
    assert_eq!(a.files_held(), std::slice::from_ref(&spelled));
    assert!(!a.is_awaited(&spelled));
    assert!(!a.say_file(&FilePlace::Clip(clip), &spelled, found_to_be(7, &memo)), "nothing is said of it");
    assert!(a.said_files().is_empty());
    // What really lies in that folder is the song's to hand on, as before.
    let brought = own.join(format!("{}.wav", found_to_be(8, &memo).sha256));
    std::fs::copy(&memo, &brought).unwrap();
    let recording = AudioContent::new(brought.to_string_lossy());
    a.edit(|song| song.add_audio_clip(track, "Brought", 4.0, 2.0, recording)).unwrap();
    assert!(a.is_awaited(&brought.to_string_lossy()));
    assert_eq!(a.files_held(), [spelled], "only the one that leaves the folder again");
}

#[test]
fn what_either_of_two_sessions_changes_the_other_has() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    let synth = a.project().track(keys).unwrap().devices[0].id;
    let revision = b.revision();

    // Notes, clips, tracks, devices and their values, automation, tempo.
    a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    b.edit(|song| song.add_midi_clip(keys, "Chorus", 8.0, 8.0)).unwrap();
    let lead = a.edit(|song| Ok(song.add_track("Lead"))).unwrap();
    b.add_device(ChainRef::Track(keys), "builtin:reverb", None, None).unwrap();
    a.set_device_params(synth, &[("cutoff".to_owned(), 1234.0)]).unwrap();
    let fade = vec![AutomationPoint::new(0.0, -12.0), AutomationPoint::new(8.0, 0.0)];
    b.edit(|song| song.set_automation(AutomationTarget::TrackVolume { track: keys }, fade)).unwrap();
    a.edit(|song| song.set_tempo(96.0)).unwrap();
    b.edit(|song| song.set_tempo_at(16.0, 120.0, true)).unwrap();
    meet(&mut a, &mut b);

    let song = b.project();
    assert_eq!(song.clip(verse).unwrap().notes().len(), 4);
    assert!(song.track(lead).is_ok() && song.tempo == 96.0 && song.tempo_changes.len() == 1);
    assert_eq!(song.device(synth).unwrap().params["cutoff"], 1234.0);
    assert_eq!(song.track(keys).unwrap().devices.len(), 2);
    assert!(b.revision() > revision, "whoever draws the song looks again");
    assert!(b.is_modified(), "and it is a song with changes to save");

    // What someone else did is no step of one's own: taking back takes
    // back one's own last edit and leaves theirs.
    assert!(b.undo());
    assert!(b.project().tempo_changes.is_empty() && b.project().tempo == 96.0);
    // It is in the journal, under their name.
    let journal = b.journal().expect("a journal is kept");
    let theirs: Vec<String> =
        journal.entries().filter(|entry| entry.by == Author::Other("Ada".into())).map(|entry| entry.line()).collect();
    assert!(theirs.iter().any(|line| line.contains("Lead")), "{theirs:?}");

    // Ids made in a shared song are each program's own, and exact as
    // numbers in JSON.
    let chorus = clip(&a, "Chorus");
    assert!(chorus.0 > 1 << 20 && chorus.0 < 1 << 53 && lead.0 > 1 << 20);
    assert_ne!(chorus.0 >> 20, lead.0 >> 20);
}

#[test]
fn of_two_moves_of_one_clip_the_later_holds_and_the_other_is_told_once_and_can_put_theirs_back() {
    let (mut a, mut b) = two();
    let verse = clip(&a, "Verse");
    a.edit(|song| song.move_clip(verse, None, 8.0)).unwrap();
    b.edit(|song| song.move_clip(verse, None, 16.0)).unwrap();
    let (said_a, said_b) = (a.take_said(), b.take_said());
    let later = said_a[0].change.stamp.max(said_b[0].change.stamp);
    let (floors_a, floors_b) = (a.floors(), b.floors());
    a.merge("Ben", said_b, Some(&floors_b), b.replica()).unwrap();
    b.merge("Ada", said_a, Some(&floors_a), a.replica()).unwrap();
    assert_eq!(text(&a), text(&b));
    // The one whose move gave way is told, the other is not.
    let (loser, winner, name) = match later.replica() == a.replica().unwrap() {
        true => (&mut b, &mut a, "Ada"),
        false => (&mut a, &mut b, "Ben"),
    };
    assert!(winner.notices().is_empty());
    let [notice] = loser.notices() else { panic!("one sentence: {:?}", loser.notices()) };
    assert!(
        notice.text.contains(name) && notice.text.contains("where the clip \u{201C}Verse\u{201D} starts"),
        "{}",
        notice.text
    );
    assert_eq!(notice.remedy.as_ref().unwrap().label, "Put mine back");
    let mine = if name == "Ada" { 16.0 } else { 8.0 };
    // Putting one's own back is an ordinary edit: it travels, and it is a
    // step that can be taken back.
    let id = notice.id;
    loser.remedy(id).unwrap();
    assert!(loser.notices().is_empty());
    assert_eq!(loser.project().clip(verse).unwrap().start, mine);
    let (said, floors, number) = (loser.take_said(), loser.floors(), loser.replica());
    winner.merge("them", said, Some(&floors), number).unwrap();
    assert_eq!(winner.project().clip(verse).unwrap().start, mine);
    assert!(winner.notices().is_empty(), "made knowing of the other's move, it is no clash");
    assert!(loser.undo());
    assert_ne!(loser.project().clip(verse).unwrap().start, mine);
}

#[test]
fn two_people_on_one_fader_is_one_notice_and_not_a_list() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    a.begin_gesture();
    b.begin_gesture();
    for step in 1..=20 {
        a.edit(|song| song.set_track_volume(keys, -(step as f32))).unwrap();
        b.edit(|song| song.set_track_volume(keys, -(step as f32) - 0.5)).unwrap();
        pass(&mut a, &mut b, "Ada");
        pass(&mut b, &mut a, "Ben");
    }
    a.end_gesture();
    b.end_gesture();
    assert!(a.notices().len() + b.notices().len() <= 2, "{:?} {:?}", a.notices(), b.notices());
    assert!(a.notices().len() <= 1 && b.notices().len() <= 1);
    assert_eq!(text(&a), text(&b));
    // A drag is one step for whoever made it, whatever arrived meanwhile.
    assert!(a.undo());
    assert!(!a.project().track(keys).unwrap().volume_db.is_nan());
}

#[test]
fn a_track_that_is_deleted_while_another_writes_in_it_is_gone_said_and_brought_back_whole() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    a.edit(|song| song.remove_track(keys).map(drop)).unwrap();
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0), note(74, 5.0)])).unwrap();
    meet(&mut a, &mut b);
    assert!(a.project().track(keys).is_err() && b.project().track(keys).is_err(), "the track is gone");
    assert!(a.notices().is_empty());
    let [notice] = b.notices() else { panic!("the one who wrote in it is told: {:?}", b.notices()) };
    assert!(notice.text.contains("Ada removed the track \u{201C}Keys\u{201D}"), "{}", notice.text);
    assert_eq!(notice.remedy.as_ref().unwrap().label, "Bring it back");
    let id = notice.id;
    b.remedy(id).unwrap();
    meet(&mut a, &mut b);
    let song = a.project();
    assert_eq!(song.clip(verse).unwrap().notes().len(), 5, "as it was, with what was written meanwhile");
    assert_eq!(song.track(keys).unwrap().devices.len(), 1);
}

#[test]
fn of_two_tracks_made_to_listen_to_each_other_one_does_and_the_other_is_told_how_to_have_theirs() {
    let (mut a, mut b) = two();
    let (keys, bass) = (track(&a, "Keys"), track(&a, "Bass"));
    let on_keys = a.add_device(ChainRef::Track(keys), "builtin:compressor", None, None).unwrap();
    let on_bass = a.add_device(ChainRef::Track(bass), "builtin:compressor", None, None).unwrap();
    meet(&mut a, &mut b);
    a.set_sidechain(on_bass, Some(keys)).unwrap();
    b.set_sidechain(on_keys, Some(bass)).unwrap();
    meet(&mut a, &mut b);
    let listens = |session: &Session, id: DeviceId| session.project().device(id).unwrap().sidechain;
    let (first, second) = (listens(&a, on_bass), listens(&a, on_keys));
    assert!(first.is_some() != second.is_some(), "one of the two: {first:?} {second:?}");
    assert!(a.project().playing_order().is_some());
    let (told, other) = if first.is_some() { (&mut b, &mut a) } else { (&mut a, &mut b) };
    assert!(other.notices().is_empty());
    let [notice] = told.notices() else { panic!("the one whose edit waits is told: {:?}", told.notices()) };
    assert!(notice.text.contains("each wait for the other"), "{}", notice.text);
    assert_eq!(notice.remedy.as_ref().unwrap().label, "Have mine instead");
    let id = notice.id;
    told.remedy(id).unwrap();
    let (said, floors, number) = (told.take_said(), told.floors(), told.replica());
    other.merge("them", said, Some(&floors), number).unwrap();
    assert_eq!(text(told), text(other));
    assert!(first.is_some() != listens(other, on_bass).is_some(), "now the other way round");
    assert!(other.project().playing_order().is_some());
}

// ---- undo is one's own -------------------------------------------------------

#[test]
fn taking_a_step_back_leaves_what_someone_else_has_set_since_and_says_so() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    // One step of a's that sets two things.
    a.begin_gesture();
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    a.edit(|song| song.set_track_pan(keys, -0.5)).unwrap();
    a.end_gesture();
    meet(&mut a, &mut b);
    // b sets one of them, knowing of a's.
    b.edit(|song| song.set_track_volume(keys, -20.0)).unwrap();
    meet(&mut a, &mut b);
    assert!(a.notices().is_empty(), "set knowing of a's, it is no clash");

    let turned = a.take_back();
    assert!(turned.done);
    assert_eq!(turned.theirs, 1, "the volume stays theirs");
    assert_eq!(turned.sentence().as_deref(), Some("One thing stays as someone else has set it since."));
    meet(&mut a, &mut b);
    let found = b.project().track(keys).unwrap();
    assert_eq!((found.volume_db, found.pan), (-20.0, 0.0), "the pan is put back and the volume is theirs");
    // Bringing the step back brings back what it took back, and no more.
    assert_eq!(a.bring_again().theirs, 1);
    meet(&mut a, &mut b);
    let found = b.project().track(keys).unwrap();
    assert_eq!((found.volume_db, found.pan), (-20.0, -0.5));
}

#[test]
fn a_thing_ones_step_made_goes_with_anothers_work_in_it_and_they_can_bring_it_back() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    let idea = a.edit(|song| song.add_midi_clip(keys, "Idea", 16.0, 4.0)).unwrap();
    meet(&mut a, &mut b);
    b.edit(|song| song.clip_mut(idea)?.add_notes(&[note(60, 0.0), note(62, 1.0)])).unwrap();
    meet(&mut a, &mut b);

    assert_eq!(a.take_back(), trill_engine::Turned { done: true, theirs: 0 });
    meet(&mut a, &mut b);
    assert!(b.project().clip(idea).is_err(), "the clip a made is gone, though b worked in it");
    let [notice] = b.notices() else { panic!("b is told: {:?}", b.notices()) };
    assert!(notice.text.contains("the clip \u{201C}Idea\u{201D}"), "{}", notice.text);
    let id = notice.id;
    b.remedy(id).unwrap();
    meet(&mut a, &mut b);
    assert_eq!(a.project().clip(idea).unwrap().notes().len(), 2, "with what b wrote in it");
}

#[test]
fn a_thing_ones_step_removed_comes_back_with_what_others_did_to_it_meanwhile() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    a.edit(|song| song.remove_track(keys).map(drop)).unwrap();
    // b has not heard of it yet, and works on.
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(79, 6.0)])).unwrap();
    b.edit(|song| song.set_track_volume(keys, -9.0)).unwrap();
    meet(&mut a, &mut b);
    assert!(a.project().track(keys).is_err());
    assert!(a.undo());
    meet(&mut a, &mut b);
    let song = b.project();
    assert_eq!(song.track(keys).unwrap().volume_db, -9.0);
    assert_eq!(song.clip(verse).unwrap().notes().len(), 4);
}

#[test]
fn someone_elses_change_never_clears_ones_redo_and_each_undoes_their_own() {
    let (mut a, mut b) = two();
    let (keys, bass) = (track(&a, "Keys"), track(&a, "Bass"));
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    assert!(a.undo());
    assert!(a.can_redo());
    b.edit(|song| song.set_track_volume(bass, -3.0)).unwrap();
    b.edit(|song| song.set_track_pan(bass, 0.25)).unwrap();
    meet(&mut a, &mut b);
    assert!(a.can_redo(), "what b did is no edit of a's");
    assert!(a.redo());
    meet(&mut a, &mut b);
    assert_eq!(b.project().track(keys).unwrap().volume_db, -6.0);
    // Each takes back their own last step, whoever acted last.
    assert!(a.undo());
    assert!(b.undo());
    meet(&mut a, &mut b);
    let song = a.project();
    assert_eq!(song.track(keys).unwrap().volume_db, 0.0);
    assert_eq!((song.track(bass).unwrap().volume_db, song.track(bass).unwrap().pan), (-3.0, 0.0));
}

// ---- going away and coming back ----------------------------------------------

#[test]
fn a_shared_song_that_is_saved_closed_and_opened_again_merges_with_what_the_others_did_meanwhile() {
    let (mut a, b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    let mut b = b;
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    meet(&mut a, &mut b);

    // b saves and closes.
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("ours.trill");
    b.save(&path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("\"shared\""), "a shared song is saved with what it takes to merge it again");
    assert!(Project::from_json(&written).is_ok(), "and is a song to whoever knows nothing of that");
    let floors_when_closed = b.floors();
    drop(b);

    // a goes on alone.
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    a.edit(|song| song.remove_clip(verse).map(drop)).unwrap();
    let lead = a.edit(|song| Ok(song.add_track("Lead"))).unwrap();
    a.take_said();

    // b opens the file again, another day, and works alone too.
    let mut b = session();
    b.load(&path).unwrap();
    assert_eq!(b.shared_song(), a.shared_song(), "it is the same shared song");
    assert_eq!(b.floors(), floors_when_closed);
    assert!(!b.is_modified());
    b.edit(|song| song.set_track_pan(keys, 0.5)).unwrap();
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(76, 5.0)])).unwrap();
    let chorus = b.edit(|song| song.add_midi_clip(keys, "Chorus", 8.0, 8.0)).unwrap();
    assert_ne!(b.replica(), a.replica(), "an opening is a program of its own");
    b.take_said();

    // They meet: each says how much it holds, and is handed the rest.
    let (for_b, for_a) = (a.since(&b.floors()), b.since(&a.floors()));
    assert!(for_b.iter().all(|said| !floors_when_closed.has(said.change.stamp)), "no more than b lacks");
    let (floors_a, floors_b) = (a.floors(), b.floors());
    a.merge("Ben", for_a, Some(&floors_b), None).unwrap();
    b.merge("Ada", for_b, Some(&floors_a), None).unwrap();
    assert_eq!(text(&a), text(&b), "both sets of work, merged");
    let song = a.project();
    assert!(song.track(lead).is_ok() && song.clip(chorus).is_ok());
    assert_eq!((song.track(keys).unwrap().volume_db, song.track(keys).unwrap().pan), (-6.0, 0.5));
    assert!(song.clip(verse).is_err(), "the clip a removed is gone");
    // b wrote a note in it while apart, and is told with the way back.
    assert!(
        b.notices().iter().any(|notice| notice.text.contains("the clip \u{201C}Verse\u{201D}")),
        "{:?}",
        b.notices()
    );
    assert_eq!(a.floors(), b.floors());
    // And from here on they are together again.
    b.edit(|song| song.set_tempo(101.0)).unwrap();
    meet(&mut a, &mut b);
}

#[test]
fn a_song_nobody_shares_is_saved_as_it_always_was_and_keeps_small_ids() {
    let mut alone = session();
    let keys = alone.edit(|song| Ok(song.add_track("Keys"))).unwrap();
    let verse = alone.edit(|song| song.add_midi_clip(keys, "Verse", 0.0, 4.0)).unwrap();
    alone.edit(|song| song.clip_mut(verse)?.add_notes(&[note(60, 0.0)])).unwrap();
    alone.edit(|song| song.remove_clip(verse).map(drop)).unwrap();
    assert!(alone.undo());
    assert!(keys.0 < 10 && verse.0 < 10 && alone.project().clip(verse).unwrap().notes()[0].id < 10);
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mine.trill");
    alone.save(&path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("\"shared\""), "nothing is kept for others who are not there");
    assert!(alone.shared_song().is_none() && alone.replica().is_none());
    assert!(alone.take_said().len() >= 4, "what is changed is kept to be said only while someone is there to hear");
    alone.hear_changes(false);
    alone.edit(|song| song.set_tempo(90.0)).unwrap();
    assert!(alone.take_said().is_empty());
}

// ---- what another program says is not trusted --------------------------------

#[test]
fn what_no_edit_could_have_made_is_refused_whole_and_the_song_is_as_it_was() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    let before = text(&b);
    let revision = b.revision();
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    let mut said = a.take_said();
    // A batch with one good change and one that no edit could have made.
    let bad = Change {
        stamp: Stamp(said[0].change.stamp.0 + 1, 99_999),
        ops: vec![Op::Track(keys, TrackOp::Set(TrackSet::Pan(7.0)))],
    };
    said.push(Said { change: bad, knew: None });
    let refused = b.merge("Mallory", said, None, None).unwrap_err();
    assert!(refused.to_string().contains("pan"), "{refused}");
    assert_eq!((text(&b), b.revision()), (before, revision), "nothing of the batch was taken");
    // Someone else's listening is not theirs to set here.
    let solo = Change { stamp: Stamp(1_000, 99_999), ops: vec![Op::Track(keys, TrackOp::Set(TrackSet::Solo(true)))] };
    assert!(b.merge("Mallory", vec![Said { change: solo, knew: None }], None, None).is_err());
    // A change that is larger than anything an edit makes.
    let many = (0..300_000).map(|_| Op::MasterVolume(0.0)).collect();
    let flood = Change { stamp: Stamp(1_001, 99_999), ops: many };
    assert!(b.merge("Mallory", vec![Said { change: flood, knew: None }], None, None).is_err());
    check(b.project());
    assert!(b.project().tracks.iter().all(|track| !track.solo));
}

#[test]
fn someone_who_goes_away_in_the_middle_of_a_drag_leaves_a_song_that_goes_on() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    a.begin_gesture();
    for step in 1..=7 {
        a.edit(|song| song.move_clip(verse, None, step as f64)).unwrap();
        a.edit(|song| song.set_track_volume(keys, -(step as f32))).unwrap();
        if step <= 4 {
            pass(&mut a, &mut b, "Ada");
        }
    }
    // a is gone: no end to the drag, and what it said last is all there is.
    drop(a);
    let song = b.project();
    assert_eq!((song.clip(verse).unwrap().start, song.track(keys).unwrap().volume_db), (4.0, -4.0));
    check(song);
    // b goes on: nothing is held, and nothing waits for anyone.
    b.edit(|song| song.move_clip(verse, None, 2.0)).unwrap();
    assert!(b.undo());
    assert_eq!(b.project().clip(verse).unwrap().start, 4.0);
    b.play();
    b.housekeeping();
    b.stop();
}

// ---- three sessions, real edits ----------------------------------------------

/// Three sessions make real edits drawn by lot, and what each says
/// reaches the others shuffled, late and more than once, with a stretch
/// in which one of them is cut off. In the end all three hold the same
/// song.
#[test]
fn three_sessions_that_took_the_same_changes_in_any_order_hold_the_same_song() {
    for seed in 1..=6u64 {
        let mut lot = Lot::new(seed);
        let (a, b) = two();
        let mut c = session();
        let mut a = a;
        c.take_song(&a.whole_song()).unwrap();
        a.take_said();
        let mut sessions = [a, b, c];
        let mut on_the_way: Vec<Vec<Said>> = vec![Vec::new(); 3];
        let mut made = 0;
        for round in 0..60 {
            let cut = (20..40).contains(&round).then_some(seed as usize % 3);
            let who = lot.below(3);
            let edit = any_edit(&mut lot, sessions[who].project());
            if sessions[who].edit(edit).is_ok() {
                made += 1;
            }
            // Now and then someone takes their last step back.
            if lot.chance(8) {
                sessions[who].undo();
            }
            for said in sessions[who].take_said() {
                for (to, waiting) in on_the_way.iter_mut().enumerate().filter(|(to, _)| *to != who) {
                    waiting.push(said.clone());
                    if lot.chance(5) {
                        waiting.push(said.clone());
                    }
                    let _ = to;
                }
            }
            for to in (0..3).filter(|to| Some(*to) != cut) {
                while !on_the_way[to].is_empty() && !lot.chance(3) {
                    let at = lot.below(on_the_way[to].len());
                    let said = on_the_way[to].swap_remove(at);
                    let from_cut = cut.is_some_and(|cut| sessions[cut].replica() == Some(said.change.stamp.replica()));
                    if from_cut {
                        on_the_way[to].push(said);
                        break;
                    }
                    sessions[to].merge("someone", vec![said], None, None).unwrap();
                    // (Taking a change in says nothing new, but where it
                    // made room under a recording that was put over one
                    // of its own: that is a change like any other.)
                    for more in sessions[to].take_said() {
                        for other in (0..3).filter(|other| *other != to) {
                            on_the_way[other].push(more.clone());
                        }
                    }
                }
            }
        }
        assert!(made > 15, "seed {seed}: enough of the edits were allowed ({made})");
        while let Some(to) = (0..3).find(|to| !on_the_way[*to].is_empty()) {
            let at = lot.below(on_the_way[to].len());
            let said = on_the_way[to].swap_remove(at);
            sessions[to].merge("someone", vec![said], None, None).unwrap();
            for more in sessions[to].take_said() {
                for other in (0..3).filter(|other| *other != to) {
                    on_the_way[other].push(more.clone());
                }
            }
        }
        let texts: Vec<String> = sessions.iter().map(text).collect();
        assert_eq!(texts[0], texts[1], "seed {seed}");
        assert_eq!(texts[1], texts[2], "seed {seed}");
        check(sessions[0].project());
        // Each still takes back and brings back its own.
        for session in &mut sessions {
            while session.undo() {}
            while session.redo() {}
        }
    }
}
