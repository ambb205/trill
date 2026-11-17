//! Writes a finished song again, step by step, into a journal: a stand-in
//! for the journal of a session, for a song that was made before journals
//! were kept or whose journal is not to hand.
//!
//!     cargo run -p trill-stage --example remake -- SONG.trill OUT.journal
//!         [--asked TEXT] [--answered TEXT]
//!
//! The song is put together in the order a person would write it: its
//! tracks, its sections, then section by section every part that plays in
//! it, then what it automates. Each step is entered as the journal enters a
//! tool call, in the journal's own words for the change. `--asked` and
//! `--answered` put a line of conversation before the first step and after
//! the last.
//!
//! What comes out is not a record of how the song was made. It is for
//! looking at a film of a making-of (`--example film -- --making`) when
//! there is no real journal to film.

use trill_core::journal::{Author, Journal, Outcome, PlanItem, PlanState, Who};
use trill_core::{Beats, Project};

/// A journal being written: the song as far as it has got, and the clock.
struct Remake {
    journal: Journal,
    song: Project,
    at: u64,
}

impl Remake {
    /// Enters one step by the assistant: `change` applied to the song,
    /// `seconds` after the step before, under the journal's own words for
    /// what it did.
    fn step(&mut self, tool: &str, seconds: u64, change: impl FnOnce(&mut Project)) {
        let before = self.song.clone();
        change(&mut self.song);
        // The journal words a change itself when a person makes it: ask it.
        let mut asked = Journal::begin(&before, 0);
        asked.record(0, Who::Person, &before, &self.song, None);
        let Some(label) = asked.last().map(|entry| entry.line()) else { return };
        self.at += seconds * 1000;
        let call = self.journal.begin_call(Author::Assistant, tool, label);
        self.journal.record(self.at, Who::Call(call), &before, &self.song, None);
        self.journal.end_call(self.at, call, Outcome::default());
    }

    fn plan(&mut self, items: &[&str], done: usize) {
        self.at += 2000;
        let state = |index: usize| match index.cmp(&done) {
            std::cmp::Ordering::Less => PlanState::Done,
            std::cmp::Ordering::Equal => PlanState::Doing,
            std::cmp::Ordering::Greater => PlanState::Pending,
        };
        let items =
            items.iter().enumerate().map(|(index, text)| PlanItem { text: (*text).to_string(), state: state(index) });
        self.journal.plan(self.at, items.collect());
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut paths, mut asked, mut answered) = (Vec::new(), None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--asked" => asked = args.next(),
            "--answered" => answered = args.next(),
            _ => paths.push(arg),
        }
    }
    let [song_path, out] = paths.as_slice() else {
        return Err("usage: remake SONG.trill OUT.journal [--asked TEXT] [--answered TEXT]".to_string());
    };
    let json = std::fs::read_to_string(song_path).map_err(|error| format!("{song_path} could not be read: {error}"))?;
    let finished = Project::from_json(&json).map_err(|error| format!("{song_path} is not a song: {error}"))?;

    // The song before anything was written: its settings, and nothing in it.
    let mut empty = finished.clone();
    empty.tracks.clear();
    empty.markers.clear();
    empty.automation.clear();
    let began = 1_760_000_000_000;
    let mut remake = Remake { journal: Journal::begin(&empty, began), song: empty, at: began };
    let plan = ["Set up the tracks", "Mark the sections", "Write each section, part by part", "Draw the automation"];
    if let Some(asked) = &asked {
        remake.at += 4000;
        remake.journal.say(remake.at, Author::Person, asked.as_str());
    }
    remake.plan(&plan, 0);

    for track in &finished.tracks {
        let mut bare = track.clone();
        bare.clips.clear();
        remake.step("create_track", 9, |song| song.tracks.push(bare));
    }
    remake.plan(&plan, 1);
    if !finished.markers.is_empty() {
        let markers = finished.markers.clone();
        remake.step("add_marker", 12, |song| song.markers = markers);
    }
    remake.plan(&plan, 2);

    // Section by section, every part that begins a clip in it.
    let mut bounds: Vec<Beats> = finished.markers.iter().map(|marker| marker.beat).collect();
    if bounds.first().is_none_or(|first| *first > 0.0) {
        bounds.insert(0, 0.0);
    }
    bounds.push(f64::INFINITY);
    for section in bounds.windows(2) {
        for (index, track) in finished.tracks.iter().enumerate() {
            for clip in track.clips.iter().filter(|clip| (section[0]..section[1]).contains(&clip.start)) {
                let clip = clip.clone();
                // A step takes about as long as it has notes to think of.
                let seconds = 6 + clip.notes().len() as u64 / 12;
                remake.step("add_clip", seconds, |song| {
                    let clips = &mut song.tracks[index].clips;
                    let place = clips.partition_point(|other| other.start <= clip.start);
                    clips.insert(place, clip);
                });
            }
        }
    }
    remake.plan(&plan, 3);
    for lane in &finished.automation {
        let lane = lane.clone();
        remake.step("set_automation", 14, |song| song.automation.push(lane));
    }
    remake.plan(&plan, 4);
    if let Some(answered) = &answered {
        remake.at += 5000;
        remake.journal.say(remake.at, Author::Assistant, answered.as_str());
    }

    let (steps, entries) = (remake.journal.steps(), remake.journal.len());
    std::fs::write(out, remake.journal.to_text(true))
        .map_err(|error| format!("{out} could not be written: {error}"))?;
    let minutes = (remake.at - began) / 60_000;
    eprintln!("{out}: {entries} entries, {steps} of them steps, over {minutes} min of a clock that was made up");
    Ok(())
}

fn main() {
    if let Err(reason) = run() {
        eprintln!("{reason}");
        std::process::exit(1);
    }
}
