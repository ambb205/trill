//! Two programs in one song on this machine, measured: how long joining
//! takes and how many bytes it is, how long it is from an edit in one
//! program to the other's graph swapped in, and how many bytes a second a
//! drag says.
//!
//! ```sh
//! cargo run -p trill-runtime --example opens -- long      # makes target/opens/long/long.trill
//! cargo run -p trill-runtime --example together -- target/opens/long/long.trill
//! cargo run -p trill-runtime --example together -- target/opens/long/long.trill --through-here
//! cargo run -p trill-runtime --example together -- target/opens/long/long.trill --through ADDRESS KEY
//! ```
//!
//! With `--through-here` the two meet through a meeting point that is
//! started in this process, on the loopback; with `--through` through one
//! that runs somewhere (its address and a key its keeper gave), which is
//! how the Linux program is measured under WSL. Without either they are
//! connected directly.
//!
//! Both programs are runtimes in this one process, without audio devices
//! and without plugins, connected over the loopback as two programs are:
//! the connection is the real one, encrypted, with its threads. Nothing of
//! the user's is read or written; what the two keep is in a folder under
//! the system's temporary one, removed at the end.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::Note;
use trill_core::merge::lot::Lot;
use trill_dsp::BuiltinFactory;
use trill_engine::{AudioBackend, Session};
use trill_runtime::{Meeting, Reach, Runtime, RuntimeOptions, Storage};
use trill_together::line::{Line, Pace};
use trill_together::{Invitation, meet};

fn start(folder: &Path, name: &str) -> Runtime {
    let storage = Storage::At { settings: folder.join(name).join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Runtime::start(options).expect("the runtime starts");
    runtime.set_name_together(name);
    runtime
}

fn song(runtime: &Runtime) -> String {
    let mut song = runtime.session.lock().project().clone();
    song.count_ids_from(0);
    song.to_json()
}

/// Waits until `done`, looking every fifth of a millisecond.
fn until(what: &str, done: impl Fn() -> bool) -> Duration {
    let began = Instant::now();
    while !done() {
        assert!(began.elapsed() < Duration::from_secs(900), "timed out waiting until {what}");
        std::thread::sleep(Duration::from_micros(200));
    }
    began.elapsed()
}

fn middle(mut times: Vec<Duration>) -> (f64, f64, f64) {
    times.sort();
    let ms = |index: usize| times[index.min(times.len() - 1)].as_secs_f64() * 1e3;
    (ms(times.len() / 2), ms(times.len() * 9 / 10), ms(times.len() - 1))
}

fn main() {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    let usage = || -> ! {
        eprintln!("usage: together SONG.trill [--through-here | --through ADDRESS KEY] [--line UP:DOWN:MS]");
        std::process::exit(2)
    };
    // The line the two are on, where they are not to have the loopback's.
    let pace = match arguments.iter().position(|argument| argument == "--line") {
        Some(at) if at + 1 < arguments.len() => {
            let pace = Pace::read(&arguments[at + 1]).unwrap_or_else(|| usage());
            arguments.drain(at..at + 2);
            Some(pace)
        }
        Some(_) => usage(),
        None => None,
    };
    let Some(file) = arguments.first().cloned() else { usage() };
    // The meeting point the two meet through, where they do: its address
    // and a key, and the meeting point itself where it is started here.
    let mut through: Option<(String, String, Option<meet::Server>)> = match &arguments[1..] {
        [] => None,
        [here] if here == "--through-here" => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port on the loopback");
            let address = listener.local_addr().unwrap().to_string();
            let key = meet::Key::new().expect("a key by lot");
            let keys = meet::Keys::of(vec![meet::keys::Given::of(&key, "Ada")]);
            let log: meet::Log = Arc::new(|line: &str| println!("    [the meeting point] {line}"));
            let server = meet::Server::start(vec![listener], keys, meet::Limits::default(), log).expect("it starts");
            Some((address, key.to_string(), Some(server)))
        }
        [through, address, key] if through == "--through" => Some((address.clone(), key.clone(), None)),
        _ => usage(),
    };
    // Through a meeting point both are on the line: each connects out to
    // it over a line of its own home.
    let mut line = match (&mut through, pace) {
        (Some((address, _, _)), Some(pace)) => {
            let line = Line::to(address, pace).expect("a line on the loopback");
            *address = line.address();
            println!("each on a line of {} to the meeting point", pace.said());
            Some(line)
        }
        _ => None,
    };
    let folder = std::env::temp_dir().join(format!("trill-together-{}", std::process::id()));
    let (ada, ben) = (start(&folder, "Ada"), start(&folder, "Ben"));
    ada.session.lock().load(Path::new(&file)).expect("the song opens");
    let (tracks, notes, bytes) = {
        let session = ada.session.lock();
        let project = session.project();
        let notes: usize = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();
        (project.tracks.len(), notes, project.to_json().len())
    };
    println!("{file}: {tracks} tracks, {notes} notes, {:.1} MB as a file", bytes as f64 / 1e6);

    // ---- joining ---------------------------------------------------------
    let invitation = match &through {
        None => {
            let invitation = ada.share(Reach::ThisMachine, 0).expect("sharing starts");
            // Directly, the one who joins reaches the other over the line.
            match pace {
                Some(pace) => {
                    let direct = Invitation::parse(&invitation).expect("an invitation");
                    let (between, over) = trill_together::line::before(&direct, pace).expect("a line on the loopback");
                    println!("the one who joins on a line of {} to the other", pace.said());
                    line = Some(between);
                    over.to_string()
                }
                None => invitation,
            }
        }
        Some((address, key, _)) => {
            ada.set_meeting_point(address, Some(key)).expect("what a keeper handed over");
            let began = Instant::now();
            let invitation = ada.share_through_meeting_point().expect("sharing starts");
            until("the meeting is open at the meeting point", || {
                matches!(ada.together().meeting, Some((_, Meeting::Held)))
            });
            println!(
                "through the meeting point at {address}: the meeting was open there {:.0} ms after sharing",
                began.elapsed().as_secs_f64() * 1e3
            );
            invitation
        }
    };
    let began = Instant::now();
    ben.join(&invitation, true).expect("the invitation is read");
    until("the one who joined has the song", || ben.session.lock().project().tracks.len() == tracks);
    let joined = began.elapsed();
    until("each sees the other", || ada.together().people.len() == 1 && ben.together().people.len() == 1);
    assert!(song(&ada) == song(&ben), "both hold the same song");
    let said = ada.together().said;
    println!(
        "joining: the song was in the other program after {:.0} ms, and {:.2} MB were said for it",
        joined.as_secs_f64() * 1e3,
        said.0 as f64 / 1e6
    );
    let kept = {
        let folder = folder.join("saved");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("shared.trill");
        ben.session.lock().save(&path).unwrap();
        std::fs::metadata(&path).unwrap().len()
    };
    println!("         saved as a shared song by the one who joined: {:.2} MB", kept as f64 / 1e6);

    // ---- from an edit to the other's graph --------------------------------
    let (track, clip) = {
        let session = ada.session.lock();
        let track = session.project().tracks.iter().filter(|track| !track.clips.is_empty()).nth(3).unwrap();
        (track.id, track.clips[track.clips.len() / 2].id)
    };
    let travel = |edit: &dyn Fn(usize)| -> Vec<Duration> {
        (0..100)
            .map(|round| {
                let before = ben.session.lock().revision();
                let began = Instant::now();
                edit(round);
                until("the other has it", || ben.session.lock().revision() != before);
                let took = began.elapsed();
                // Not in step with the two programs' looks at each other.
                std::thread::sleep(Duration::from_millis(7 + (round as u64 * 13) % 31));
                took
            })
            .collect()
    };
    let fader = travel(&|round| {
        ada.session.lock().edit(|project| project.set_track_volume(track, -1.0 - (round % 20) as f32)).unwrap();
    });
    let (mid, nine, worst) = middle(fader);
    println!(
        "a fader set, to the other's graph swapped in: {mid:.0} ms in the middle, {nine:.0} ms for nine in ten, {worst:.0} ms at worst"
    );
    let note = travel(&|round| {
        let note = Note { pitch: 40 + (round % 40) as u8, start: (round % 16) as f64, ..Default::default() };
        ada.session.lock().edit(|project| project.clip_mut(clip)?.add_notes(&[note])).unwrap();
    });
    let (mid, nine, worst) = middle(note);
    println!(
        "a note written, to the other's graph swapped in: {mid:.0} ms in the middle, {nine:.0} ms for nine in ten, {worst:.0} ms at worst"
    );

    // ---- what a drag says ---------------------------------------------------
    let drag = |what: &str, edit: &dyn Fn(usize)| {
        let before = ada.together().said;
        let seconds = 5;
        let began = Instant::now();
        ada.session.lock().begin_gesture();
        // As a window does: an edit a frame, sixty frames a second.
        for frame in 0..seconds * 60 {
            edit(frame);
            let due = began + Duration::from_secs_f64((frame + 1) as f64 / 60.0);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
        }
        ada.session.lock().end_gesture();
        std::thread::sleep(Duration::from_millis(300));
        let after = ada.together().said;
        let (bytes, messages) = (after.0 - before.0, after.1 - before.1);
        println!(
            "{what}, sixty edits a second for {seconds} s: {:.1} kB a second in {:.0} messages a second ({} bytes a message)",
            bytes as f64 / seconds as f64 / 1e3,
            messages as f64 / seconds as f64,
            bytes / messages.max(1)
        );
    };
    drag("a fader dragged", &|frame| {
        ada.session.lock().edit(|project| project.set_track_volume(track, -(frame as f32) / 20.0)).unwrap();
    });
    let start = ada.session.lock().project().clip(clip).unwrap().start;
    drag("a clip dragged", &|frame| {
        ada.session.lock().edit(|project| project.move_clip(clip, None, start + frame as f64 / 16.0)).unwrap();
    });
    until("the other has where the drag ended", || song(&ada) == song(&ben));
    // What the two programs say when nothing is changed: who is here.
    let before = ada.together().said;
    std::thread::sleep(Duration::from_secs(4));
    let after = ada.together().said;
    println!("nothing changed for 4 s: {:.0} bytes a second (who is here)", (after.0 - before.0) as f64 / 4.0);

    drop((ada, ben));
    // (Where the meeting point is this process's, its last lines say how
    // much it passed.)
    std::thread::sleep(Duration::from_millis(300));
    drop(line);
    drop(through);

    // ---- what taking a change in costs the one who hears it ----------------
    // Two sessions without a wire between them: what one says is handed to
    // the other here, and the time is the time the other's session is held.
    let bare = || {
        let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        session.hear_changes(true);
        session
    };
    let (mut a, mut b) = (bare(), bare());
    a.load(Path::new(&file)).expect("the song opens");
    // The same edit before the song is shared, to hold the others by.
    let alone: Vec<Duration> = (0..100)
        .map(|round| {
            let began = Instant::now();
            a.edit(|project| project.set_track_volume(track, -1.0 - (round % 20) as f32)).unwrap();
            began.elapsed()
        })
        .collect();
    println!("a fader set in the song before it is shared: {:.1} ms", middle(alone).0);
    a.share();
    let began = Instant::now();
    let whole = a.whole_song();
    let written = began.elapsed();
    let began = Instant::now();
    b.take_song(&whole).expect("the song is taken");
    println!(
        "the whole song: written out in {:.0} ms by the one who has it, taken in {:.0} ms by the one who joins",
        written.as_secs_f64() * 1e3,
        began.elapsed().as_secs_f64() * 1e3
    );
    a.take_said();
    let mut heard = |what: &str, edit: &mut dyn FnMut(&mut Session, usize)| {
        let (mut here, mut there) = (Vec::new(), Vec::new());
        for round in 0..100 {
            let began = Instant::now();
            edit(&mut a, round);
            here.push(began.elapsed());
            let (said, floors, from) = (a.take_said(), a.floors(), a.replica());
            let began = Instant::now();
            b.merge("Ada", said, Some(&floors), from).expect("what a session says is taken");
            there.push(began.elapsed());
        }
        let ((mine, _, _), (theirs, nine, _)) = (middle(here), middle(there));
        println!(
            "{what}: {mine:.1} ms for the one who made it, {theirs:.1} ms for the one who hears of it ({nine:.1} ms for nine in ten)"
        );
    };
    heard("a fader set", &mut |session, round| {
        session.edit(|project| project.set_track_volume(track, -1.0 - (round % 20) as f32)).unwrap();
    });
    heard("a note written", &mut |session, round| {
        let note = Note { pitch: 40 + (round % 40) as u8, start: (round % 16) as f64, ..Default::default() };
        session.edit(|project| project.clip_mut(clip)?.add_notes(&[note])).unwrap();
    });
    heard("a clip moved", &mut |session, round| {
        session.edit(|project| project.move_clip(clip, None, start + (round % 2) as f64 / 2.0)).unwrap();
    });

    // ---- what an hour of work adds ------------------------------------------
    // An edit a second for an hour, by one of the two: six in ten write a
    // note or three, one removes some, one moves a clip, one sets a fader
    // and one a device's parameter. What is said of it, and what the song's
    // file has grown by for each of the two.
    let size = |session: &mut Session, name: &str| {
        let path = folder.join("saved").join(name);
        session.save(&path).expect("the song is saved");
        std::fs::metadata(&path).unwrap().len() as f64
    };
    let (before_a, before_b) = (size(&mut a, "a.trill"), size(&mut b, "b.trill"));
    let (clips, tracks, devices) = {
        let project = a.project();
        let midi = project.tracks.iter().flat_map(|track| &track.clips).filter(|clip| !clip.notes().is_empty());
        let clips: Vec<_> = midi.map(|clip| (clip.id, clip.start)).collect();
        let tracks: Vec<_> = project.tracks.iter().map(|track| track.id).collect();
        let devices: Vec<_> = project.devices().map(|device| device.id).collect();
        (clips, tracks, devices)
    };
    let mut lot = Lot::new(7);
    let (mut said_bytes, mut made) = (0usize, 0usize);
    for round in 0..3600 {
        let (clip, at) = *lot.pick(&clips).unwrap();
        let (track, device) = (*lot.pick(&tracks).unwrap(), *lot.pick(&devices).unwrap());
        let (pitch, beat, some) = (36 + lot.below(48) as u8, lot.below(16) as f64 / 4.0, 1 + lot.below(3));
        let done = match round % 10 {
            0..=5 => a
                .edit(|project| {
                    let notes: Vec<Note> = (0..some)
                        .map(|n| Note {
                            pitch: pitch + n as u8 * 4,
                            start: beat,
                            length: 0.5,
                            velocity: 96,
                            ..Default::default()
                        })
                        .collect();
                    project.clip_mut(clip)?.add_notes(&notes)
                })
                .is_ok(),
            6 => a.edit(|project| project.clip_mut(clip)?.remove_notes(|note| note.pitch == pitch).map(drop)).is_ok(),
            7 => a.edit(|project| project.move_clip(clip, None, at + beat / 8.0)).is_ok(),
            8 => a.edit(|project| project.set_track_volume(track, -(pitch as f32) / 4.0)).is_ok(),
            _ => match a.device_params(device).ok().and_then(|params| params.first().map(|param| param.info.clone())) {
                Some(info) => {
                    let value = info.min + (info.max - info.min) * beat / 4.0;
                    a.set_device_params(device, &[(info.id, value)]).is_ok()
                }
                None => false,
            },
        };
        made += usize::from(done);
        let (said, floors, from) = (a.take_said(), a.floors(), a.replica());
        said_bytes += serde_json::to_vec(&said).map_or(0, |bytes| bytes.len());
        b.merge("Ada", said, Some(&floors), from).expect("what a session says is taken");
    }
    let (after_a, after_b) = (size(&mut a, "a.trill"), size(&mut b, "b.trill"));
    println!(
        "an hour of work, {made} edits: {:.0} kB said of it; the file of the one who made them grew by {:.0} kB, \
         the other's by {:.0} kB (of {:.1} MB)",
        said_bytes as f64 / 1e3,
        (after_a - before_a) / 1e3,
        (after_b - before_b) / 1e3,
        before_a / 1e6
    );
    // The same hour by someone working alone: what the song itself grew by.
    let mut alone = bare();
    alone.load(Path::new(&file)).expect("the song opens");
    let before = size(&mut alone, "alone.trill");
    let project = a.project().clone();
    alone
        .edit(|song| {
            *song = project;
            Ok(())
        })
        .expect("the song is put in place");
    println!(
        "         the same song with those edits in it, never shared: {:.0} kB more",
        (size(&mut alone, "alone.trill") - before) / 1e3
    );
    drop((a, b, alone));
    let _ = std::fs::remove_dir_all(&folder);
}
