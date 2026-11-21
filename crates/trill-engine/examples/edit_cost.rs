//! What an edit costs someone working alone: the time `Session::edit`
//! takes on a song, by the kind of edit.
//!
//! ```sh
//! cargo run -p trill-runtime --example opens -- long     # makes target/opens/long/long.trill
//! cargo run -p trill-engine --example edit_cost -- target/opens/long/long.trill
//! ```
//!
//! The song is opened without an audio device, and each kind of edit is
//! made a hundred times: a note written into a clip, a clip moved, a
//! fader set (which compiles the song), a device's parameter set (which
//! does not), and a step taken back and brought back. For each, the
//! fastest time, the middle one and the time nine in ten stay under, in
//! microseconds. On a machine that is busy with something else the
//! fastest is the one to hold two builds by.
//!
//! Two builds are held beside one another from the same folder: where a
//! song's recordings are found decides how long it takes to open.
//!
//! It uses nothing of the session that an earlier build has not got, so
//! that the same file measures two builds: copy it into the other
//! checkout's `crates/trill-engine/examples` and run it there on the same
//! song.

use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::Note;
use trill_dsp::BuiltinFactory;
use trill_engine::{AudioBackend, Session};

/// The fastest of `times`, the middle one and what nine in ten of them
/// stay under.
fn said(what: &str, mut times: Vec<Duration>) {
    times.sort();
    let micros = |index: usize| times[index.min(times.len() - 1)].as_secs_f64() * 1e6;
    println!(
        "{what:<28} {:>7.0} us at the fastest, {:>7.0} us in the middle, {:>7.0} us for nine in ten",
        micros(0),
        micros(times.len() / 2),
        micros(times.len() * 9 / 10)
    );
}

fn timed(rounds: usize, mut edit: impl FnMut(usize)) -> Vec<Duration> {
    (0..rounds)
        .map(|round| {
            let began = Instant::now();
            edit(round);
            began.elapsed()
        })
        .collect()
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(song) = arguments.next() else {
        eprintln!("usage: edit_cost SONG.trill [ROUNDS]");
        std::process::exit(2);
    };
    let rounds: usize = arguments.next().and_then(|rounds| rounds.parse().ok()).unwrap_or(100);
    let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    let began = Instant::now();
    session.load(std::path::Path::new(&song)).expect("the song opens");
    let opened = began.elapsed();
    let project = session.project();
    let notes: usize = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();
    let clips: usize = project.tracks.iter().map(|track| track.clips.len()).sum();
    // What opening takes is mostly reading the recordings the song plays,
    // where it has any: a song that names them by a path from the folder
    // it was made in finds them from there and nowhere else, and opens in
    // a fraction of the time where it finds none. So that is said.
    let recordings: std::collections::BTreeSet<&str> = (project.tracks.iter())
        .flat_map(|track| &track.clips)
        .filter_map(|clip| Some(clip.audio()?.path.as_str()))
        .collect();
    let found = recordings.iter().filter(|path| std::path::Path::new(path).is_file()).count();
    println!(
        "{song}: {} tracks, {clips} clips, {notes} notes, {} devices, {} bytes as a file; opened in {:.2} s, with {found} of \
         its {} recordings found from this folder",
        project.tracks.len(),
        project.devices().count(),
        project.to_json().len(),
        opened.as_secs_f64(),
        recordings.len()
    );
    // Something of each kind to edit, from the middle of the song.
    let track = project.tracks.iter().filter(|track| !track.clips.is_empty()).nth(3).expect("a track with clips");
    let (track_id, clip) = (track.id, track.clips[track.clips.len() / 2].id);
    let start = track.clips[track.clips.len() / 2].start;
    let device = track.devices.first().expect("a device on the track").id;
    let param = session.device_params(device).expect("its parameters")[0].info.clone();

    let times = timed(rounds, |round| {
        let note = Note { pitch: 40 + (round % 40) as u8, start: (round % 16) as f64, ..Default::default() };
        session.edit(|project| project.clip_mut(clip)?.add_notes(&[note])).unwrap();
    });
    said("a note written into a clip", times);
    let times = timed(rounds, |round| {
        let to = start + if round % 2 == 0 { 0.5 } else { 0.0 };
        session.edit(|project| project.move_clip(clip, None, to)).unwrap();
    });
    said("a clip moved", times);
    let times = timed(rounds, |round| {
        session.edit(|project| project.set_track_volume(track_id, -(round as f32 % 24.0) - 0.5)).unwrap();
    });
    said("a fader set", times);
    let times = timed(rounds, |round| {
        let value = param.min + (param.max - param.min) * ((round % 50) as f64 + 1.0) / 60.0;
        session.set_device_params(device, &[(param.id.clone(), value)]).unwrap();
    });
    said("a device's parameter set", times);
    let half = rounds / 2;
    let times = timed(half, |_| {
        assert!(session.undo());
    });
    said("a step taken back", times);
    let times = timed(half, |_| {
        assert!(session.redo());
    });
    said("a step brought back", times);
}
