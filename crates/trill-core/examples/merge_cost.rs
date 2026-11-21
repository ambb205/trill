//! What the parts of an edit cost on a song: the song copied, compared,
//! settled, its changes found, its properties made and the song worked out
//! from them.
//!
//! ```sh
//! cargo run -p trill-core --example merge_cost -- SONG.trill
//! ```
//!
//! Each is done fifty times; the fastest and the middle time are printed,
//! in microseconds.

use std::time::{Duration, Instant};
use trill_core::merge::{self, Change, Doc, Stamp};
use trill_core::{Note, Project};

fn timed<T>(what: &str, mut run: impl FnMut() -> T) -> T {
    let mut times: Vec<Duration> = Vec::new();
    let mut last = None;
    for _ in 0..50 {
        let began = Instant::now();
        let made = run();
        times.push(began.elapsed());
        last = Some(made);
    }
    times.sort();
    let micros = |index: usize| times[index].as_secs_f64() * 1e6;
    println!("{what:<44} {:>8.0} us at the fastest, {:>8.0} us in the middle", micros(0), micros(times.len() / 2));
    last.expect("it ran")
}

fn main() {
    let Some(file) = std::env::args().nth(1) else {
        eprintln!("usage: merge_cost SONG.trill");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&file).expect("the file is read");
    let mut song = timed("the file read into a song", || Project::from_json(&text).expect("it is a song"));
    song.settle(None);
    let copy = timed("the song copied", || song.clone());
    timed("two songs compared", || assert!(copy == song));
    timed("the song settled against the one before", || {
        let mut now = song.clone();
        now.settle(Some(&copy));
        now
    });
    timed("  (of which the copy)", || song.clone());
    let doc = timed("its properties made", || Doc::from_project(&song));
    timed("the song worked out from its properties", || doc.derive());
    timed("its properties copied", || doc.clone());

    // An edit: a note written into a clip in the middle of the song.
    let clip = {
        let track = song.tracks.iter().filter(|track| !track.clips.is_empty()).nth(3).expect("a track with clips");
        track.clips[track.clips.len() / 2].id
    };
    let mut after = song.clone();
    let note = Note { pitch: 61, start: 1.0, ..Default::default() };
    after.clip_mut(clip).unwrap().add_notes(&[note]).unwrap();
    after.settle(Some(&song));
    let ops = timed("what a written note changed, found", || merge::diff(&song, &after, &doc));
    println!("    ({} properties set)", ops.len());
    timed("that change taken into the properties", || {
        let mut doc = doc.clone();
        let stamp = Stamp(doc.clock() + 1, 0);
        doc.apply(&Change { stamp, ops: ops.clone() })
    });
    timed("  (of which the properties copied)", || doc.clone());
    timed("the song as text", || song.to_json());
}
