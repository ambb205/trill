//! The processors a song that plays is made on, and what it costs them.
//!
//! One thread cannot always make a block in the time it lasts. The engine
//! spreads the tracks of a block that do not feed one another over a few
//! threads ([`crate::team`]), once the song has shown that it needs them
//! and never before: a song that one thread plays with most of its time
//! to spare is left on that thread. How many it may have is asked and
//! said here, with what the audio threads have made of it of late: how
//! much of their time went, how many buffers came late, and what each
//! track took.
//!
//! **Falling short is a sentence.** A buffer that takes longer to make
//! than it lasts is a gap in what is heard. When that goes on, the session
//! says so in words ([`Shortfall::sentence`]): that the song is more than
//! the machine plays in time, that an export is not affected, and which
//! tracks cost most. It says it as one standing thing for as long as it
//! holds and for a while after, with a number that moves when it begins
//! anew ([`Shortfall::episode`]), so that a front end can show it once
//! and not once a buffer.
//!
//! The audio threads only count ([`crate::engine::Load`],
//! [`crate::graph::Cost`]: atomics that are taken when they are asked
//! for). The session takes them every so often, with its chores, and
//! keeps the last ten seconds: any number of front ends may ask
//! [`Session::keeping_up`] without taking anything from one another.
//! [`Session::take_load`] and [`Session::take_costs`] are for one tool
//! that measures: each gives what was counted since it was last asked.

use super::Session;
use crate::engine::{Load, usual_play_threads};
use crate::graph::Cost;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use trill_core::TrackId;

/// How long what the audio threads made of the song is kept:
/// [`Session::keeping_up`] speaks of this long.
const RECENT: Duration = Duration::from_secs(10);
/// How many buffers have to come late within that time for the song to be
/// said to be more than the machine plays in time. One or two are a
/// plugin that took its time over one block, which the same song does on
/// any machine now and then.
const SHORT_FROM: u64 = 5;
/// How long no buffer has to come late for that to be over.
const QUIET: Duration = Duration::from_secs(10);
/// How long it has to have been over for the next time to be a new thing
/// to say ([`Shortfall::episode`]).
const AGAIN_AFTER: Duration = Duration::from_secs(60);
/// How often the audio threads' counts are taken at the most, unless
/// somebody asks for all of them: a front end that asks with every frame
/// it draws gets what the last look found.
const LOOK_EVERY: Duration = Duration::from_millis(100);

/// What making one track's blocks took: see [`Session::take_costs`].
#[derive(Clone, Debug, PartialEq)]
pub struct TrackCost {
    pub track: TrackId,
    pub name: String,
    pub cost: Cost,
}

/// What making the song's blocks took, part by part, since that was last
/// asked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Costs {
    /// Every track of the song, in its order. A track's cost is its
    /// devices' and its own: its notes, its audio, its fader.
    pub tracks: Vec<TrackCost>,
    /// The master's devices and its fader.
    pub master: Cost,
}

/// What the audio threads have made of the song lately: see
/// [`Session::keeping_up`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeepingUp {
    /// How long the figures cover, in seconds of sound: ten at the most,
    /// and nothing where no buffer has been asked for.
    pub seconds: f64,
    /// How the audio thread kept up over that time.
    pub load: Load,
    /// Every track of the song with what it cost over that time, in the
    /// song's order.
    pub tracks: Vec<TrackCost>,
    /// The master's devices and its fader.
    pub master: Cost,
    /// The rate the engine runs at, which turns a cost into a share of a
    /// processor ([`Cost::share`]).
    pub sample_rate: f64,
    /// Set while the song is more than the machine plays in time.
    pub short: Option<Shortfall>,
}

/// The song is more than this machine plays in time: buffers have been
/// coming late, each a gap in what is heard.
#[derive(Clone, Debug, PartialEq)]
pub struct Shortfall {
    /// Counts the times this began in the session. It moves only when the
    /// song had kept up for a minute in between: a front end that says the
    /// sentence once says it again when this is another number.
    pub episode: u64,
    /// How many buffers came late since it began, of how many.
    pub late: u64,
    pub buffers: u64,
    /// The tracks that cost most, three at the most, each with the share
    /// of one processor it takes (1 is a whole processor).
    pub costliest: Vec<(String, f64)>,
    /// How many threads the song is made on, and how many it may have.
    pub threads: usize,
    pub allowed: usize,
    /// Why the threads that make sound are not in the class the system
    /// keeps for audio, and what the user can do about it, where they are
    /// not ([`crate::host::priority::refused_because`]).
    pub unprioritized: Option<String>,
}

impl Shortfall {
    /// What there is to say of it, in words fit to show a user or to hand
    /// a model: what is the matter, what is not, where the time goes and
    /// what helps.
    pub fn sentence(&self) -> String {
        let mut words = format!(
            "This song is more than this machine plays in time: {} of the last {} buffers came late, and each is a \
             gap in what is heard. An export is not affected.",
            grouped(self.late),
            grouped(self.buffers)
        );
        if !self.costliest.is_empty() {
            let named: Vec<String> = (self.costliest.iter())
                .map(|(name, share)| format!("{name} ({:.0} % of a processor)", share * 100.0))
                .collect();
            words.push_str(&format!(
                " What costs most: {}. Switching off or removing devices there is what helps; muting a track does \
                 not, because its devices go on running.",
                named.join(", ")
            ));
        }
        if let Some(why) = &self.unprioritized {
            words.push(' ');
            words.push_str(why);
        }
        words
    }

    /// The same in a line: for a place that has room for no more.
    pub fn headline(&self) -> String {
        "This song is more than this machine plays in time".to_string()
    }
}

/// `1234567` as `1,234,567`.
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// What one look at the audio threads' counts found.
struct Look {
    at: Instant,
    load: Load,
    tracks: Vec<(TrackId, Cost)>,
    master: Cost,
}

/// What the session keeps of how the song is played: see the module.
#[derive(Default)]
pub(super) struct Watch {
    /// The looks of the last ten seconds, oldest first.
    recent: VecDeque<Look>,
    looked: Option<Instant>,
    /// What was counted since [`Session::take_load`] and
    /// [`Session::take_costs`] were last asked.
    load: Load,
    tracks: HashMap<TrackId, Cost>,
    master: Cost,
    verdict: Verdict,
}

impl Session {
    /// How many threads the song that plays may be made on, the audio
    /// callback's own among them.
    pub fn play_threads(&self) -> usize {
        self.engine.play_threads()
    }

    /// Says how many threads the song that plays may be made on, the audio
    /// callback's own among them; `None` for what suits the machine
    /// ([`usual_play_threads`]). One has the callback make everything.
    /// Not an edit: the song is as it was, and sounds as it did.
    pub fn set_play_threads(&mut self, threads: Option<usize>) {
        let threads = threads.unwrap_or_else(usual_play_threads).max(1);
        if threads == self.engine.play_threads() {
            return;
        }
        self.engine.set_play_threads(threads);
        // The graph says what can be made at once, or says nothing.
        self.compile_and_swap();
    }

    /// Sets the share of a block's own time that making its tracks must
    /// take before the song that plays is spread over its threads. Zero
    /// spreads every block: for hearing, and measuring, that it makes no
    /// difference to what is played.
    pub fn spread_from(&mut self, share: f64) {
        self.engine.spread_from(share);
    }

    /// Has the audio threads count, for the longest block of each track,
    /// how many of their processor's cycles they ran while they made it
    /// ([`Cost::longest_ran`]), or stops that. Beside the clock's time it
    /// tells a block that took long to work out from a thread the system
    /// kept from its work. It asks the system twice a track and block: for
    /// a tool that measures.
    pub fn count_cycles(&mut self, on: bool) {
        self.engine.count_cycles(on);
    }

    /// How the audio thread has kept up since this was last asked: how
    /// much of the time it had it used, how many buffers came late, and
    /// how many threads it is made on. For one tool that measures; a
    /// front end asks [`Session::keeping_up`].
    pub fn take_load(&mut self) -> Load {
        self.look(true);
        let load = std::mem::take(&mut self.cores.load);
        // (What says how things stand, and counts nothing, stays said.)
        let Load { threads, allowed, audio_class, thread_level, thread_level_before, .. } = load;
        self.cores.load = Load { threads, allowed, audio_class, thread_level, thread_level_before, ..Load::default() };
        load
    }

    /// What making each track's blocks took since this was last asked,
    /// and the master's. A share of one processor's time comes of a cost
    /// and the rate the engine runs at ([`Cost::share`]). For one tool
    /// that measures; a front end asks [`Session::keeping_up`].
    pub fn take_costs(&mut self) -> Costs {
        self.look(true);
        let mut counted = std::mem::take(&mut self.cores.tracks);
        let tracks = (self.project.tracks.iter())
            .map(|track| TrackCost {
                track: track.id,
                name: track.name.clone(),
                cost: counted.remove(&track.id).unwrap_or_default(),
            })
            .collect();
        Costs { tracks, master: std::mem::take(&mut self.cores.master) }
    }

    /// What the audio threads have made of the song over the last ten
    /// seconds: how much of their time went, how many buffers came late,
    /// on how many threads, what each track cost, and whether the song is
    /// more than the machine plays in time. Takes nothing from anyone: it
    /// may be asked as often as a window draws.
    pub fn keeping_up(&mut self) -> KeepingUp {
        self.look(false);
        let watch = &self.cores;
        let (mut load, mut master, mut seconds) = (Load::default(), Cost::default(), 0.0);
        let mut by_track: HashMap<TrackId, Cost> = HashMap::new();
        let sample_rate = self.engine.info().sample_rate;
        for look in &watch.recent {
            load.add(&look.load);
            master.add(&look.master);
            seconds += look.load.frames as f64 / sample_rate.max(1.0);
            for (track, cost) in &look.tracks {
                by_track.entry(*track).or_default().add(cost);
            }
        }
        let tracks = (self.project.tracks.iter())
            .map(|track| TrackCost {
                track: track.id,
                name: track.name.clone(),
                cost: by_track.remove(&track.id).unwrap_or_default(),
            })
            .collect();
        KeepingUp { seconds, load, tracks, master, sample_rate, short: watch.verdict.short.clone() }
    }

    /// Whether the song is more than the machine plays in time, as of the
    /// session's last look: see [`Shortfall`].
    pub fn falling_short(&self) -> Option<&Shortfall> {
        self.cores.verdict.short.as_ref()
    }

    /// Forgets what was counted of the song that was open: another one
    /// begins with nothing said of it.
    pub(super) fn forget_load(&mut self) {
        // (What the engine has counted of the old song goes with it.)
        self.look(true);
        let verdict = Verdict { episodes: self.cores.verdict.episodes, ..Verdict::default() };
        self.cores = Watch { verdict, ..Watch::default() };
    }

    /// Takes what the audio threads have counted and keeps it: one of the
    /// session's chores, and what every way of asking does first. Without
    /// `now`, not more often than every tenth of a second.
    pub(super) fn look(&mut self, now: bool) {
        let at = Instant::now();
        if !now && self.cores.looked.is_some_and(|looked| at.duration_since(looked) < LOOK_EVERY) {
            return;
        }
        self.cores.looked = Some(at);
        let load = self.engine.take_load();
        let tracks: Vec<(TrackId, Cost)> = (self.project.tracks.iter())
            .filter_map(|track| Some((track.id, self.meters.tracks.get(&track.id)?.take_cost())))
            .collect();
        let master = self.meters.master.take_cost();

        let watch = &mut self.cores;
        watch.load.add(&load);
        watch.master.add(&master);
        for (track, cost) in &tracks {
            watch.tracks.entry(*track).or_default().add(cost);
        }
        let (late, buffers, threads) = (load.late, load.callbacks, (load.threads, load.allowed));
        // (A look that found no buffer asked for has nothing to keep.)
        if buffers > 0 {
            watch.recent.push_back(Look { at, load, tracks, master });
        }
        while watch.recent.front().is_some_and(|look| at.duration_since(look.at) > RECENT) {
            watch.recent.pop_front();
        }
        let recent = |count: fn(&Load) -> u64| watch.recent.iter().map(|look| count(&look.load)).sum::<u64>();
        let of_late = (recent(|load| load.late), recent(|load| load.callbacks));
        let (project, sample_rate) = (&self.project, self.engine.info().sample_rate);
        let Watch { recent, verdict, .. } = watch;
        verdict.judge(at, (late, buffers), of_late, threads, || costliest(recent, project, sample_rate));
    }
}

/// Whether the song falls short, and how often it has begun to: see the
/// module.
#[derive(Default)]
struct Verdict {
    short: Option<Shortfall>,
    /// When a buffer last came late.
    last_late: Option<Instant>,
    /// When it last stopped falling short, and how often it began.
    over: Option<Instant>,
    episodes: u64,
}

impl Verdict {
    /// Takes in what a look at `at` found: how many buffers came late of
    /// how many since the look before (`now`) and over the last ten
    /// seconds (`of_late`), on how many threads of how many. The tracks
    /// that cost most are asked for only where they are to be said.
    fn judge(
        &mut self,
        at: Instant,
        now: (u64, u64),
        of_late: (u64, u64),
        (threads, allowed): (usize, usize),
        costliest: impl FnOnce() -> Vec<(String, f64)>,
    ) {
        if now.0 > 0 {
            self.last_late = Some(at);
        }
        let quiet = self.last_late.is_none_or(|last| at.duration_since(last) > QUIET);
        match &mut self.short {
            Some(_) if quiet => {
                self.short = None;
                self.over = Some(at);
            }
            Some(short) => {
                short.late += now.0;
                short.buffers += now.1;
                (short.threads, short.allowed) = (threads, allowed);
                short.costliest = costliest();
            }
            None if of_late.0 >= SHORT_FROM && !quiet => {
                // A new thing to say, unless it was said a moment ago.
                if self.over.is_none_or(|over| at.duration_since(over) > AGAIN_AFTER) {
                    self.episodes += 1;
                }
                let (late, buffers) = of_late;
                let episode = self.episodes;
                let unprioritized = crate::host::priority::refused_because();
                let costliest = costliest();
                self.short = Some(Shortfall { episode, late, buffers, costliest, threads, allowed, unprioritized });
            }
            None => {}
        }
    }
}

/// The three tracks that cost most over `recent`, by name, each with the
/// share of one processor it took: only ones that took a hundredth of one
/// or more.
fn costliest(recent: &VecDeque<Look>, project: &trill_core::Project, sample_rate: f64) -> Vec<(String, f64)> {
    let mut by_track: HashMap<TrackId, Cost> = HashMap::new();
    for (track, cost) in recent.iter().flat_map(|look| &look.tracks) {
        by_track.entry(*track).or_default().add(cost);
    }
    let mut shares: Vec<(String, f64)> = (project.tracks.iter())
        .filter_map(|track| Some((track.name.clone(), by_track.get(&track.id)?.share(sample_rate))))
        .filter(|(_, share)| *share >= 0.01)
        .collect();
    shares.sort_by(|a, b| b.1.total_cmp(&a.1));
    shares.truncate(3);
    shares
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falling_short_is_one_thing_while_it_lasts_and_a_new_one_only_after_a_minute_of_keeping_up() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let tracks = || vec![("Pad".to_string(), 0.3)];
        let mut verdict = Verdict::default();
        // One late buffer, and two more: a plugin that took its time.
        verdict.judge(at(0.0), (1, 20), (1, 20), (1, 1), tracks);
        verdict.judge(at(0.2), (2, 20), (3, 40), (1, 1), tracks);
        assert!(verdict.short.is_none());
        // Five within the ten seconds: the song falls short.
        verdict.judge(at(0.4), (2, 20), (5, 60), (1, 1), tracks);
        let short = verdict.short.clone().expect("said");
        assert_eq!((short.episode, short.late, short.buffers), (1, 5, 60));
        assert_eq!(short.costliest, tracks());
        // It goes on: the same thing, counted further.
        verdict.judge(at(0.6), (20, 20), (25, 80), (8, 8), tracks);
        let short = verdict.short.clone().expect("still");
        assert_eq!((short.episode, short.late, short.buffers, short.threads), (1, 25, 80, 8));
        // Ten seconds without a late buffer and it is over.
        verdict.judge(at(8.0), (0, 700), (25, 800), (8, 8), tracks);
        assert!(verdict.short.is_some(), "not yet");
        verdict.judge(at(10.7), (0, 270), (0, 1_000), (8, 8), tracks);
        assert!(verdict.short.is_none());
        // What is left in the ten seconds from before does not begin it
        // again by itself.
        verdict.judge(at(10.9), (0, 20), (9, 1_000), (8, 8), tracks);
        assert!(verdict.short.is_none());
        // Again within the minute: said, and the same thing as before.
        verdict.judge(at(30.0), (6, 20), (6, 1_000), (8, 8), tracks);
        assert_eq!(verdict.short.as_ref().map(|short| short.episode), Some(1));
        verdict.judge(at(45.0), (0, 1_000), (0, 1_000), (8, 8), tracks);
        assert!(verdict.short.is_none());
        // After more than a minute of keeping up it is a new thing to say.
        verdict.judge(at(110.0), (9, 20), (9, 1_000), (8, 8), tracks);
        assert_eq!(verdict.short.as_ref().map(|short| (short.episode, short.late)), Some((2, 9)));
    }

    #[test]
    fn the_sentence_says_what_is_the_matter_what_is_not_and_what_helps() {
        let short = Shortfall {
            episode: 1,
            late: 1_412,
            buffers: 2_000,
            costliest: vec![("Pad".into(), 0.259), ("Clean Chords".into(), 0.16)],
            threads: 8,
            allowed: 8,
            unprioritized: None,
        };
        let sentence = short.sentence();
        assert!(
            sentence.starts_with("This song is more than this machine plays in time: 1,412 of the last 2,000 buffers")
        );
        assert!(sentence.contains("An export is not affected."), "{sentence}");
        assert!(sentence.contains("Pad (26 % of a processor), Clean Chords (16 % of a processor)"), "{sentence}");
        assert!(sentence.contains("muting a track does not"), "{sentence}");
        // With nothing known of the tracks, nothing is said of them.
        let bare = Shortfall { costliest: Vec::new(), ..short }.sentence();
        assert!(bare.ends_with("An export is not affected."), "{bare}");
        // Where the audio threads were not given their class, it says why and what to do.
        let why = "The audio threads run at an ordinary priority.";
        let unprioritized = Shortfall { unprioritized: Some(why.into()), ..short }.sentence();
        assert!(unprioritized.ends_with(&format!("muting a track does not, because its devices go on running. {why}")));
        assert_eq!((grouped(7), grouped(1_000), grouped(1_234_567)), ("7".into(), "1,000".into(), "1,234,567".into()));
    }
}
