//! A session played again from its journal.
//!
//! A [`Replay`] lays the entries of a journal out on a clock of its own and
//! says, for any moment on it, what the song was ([`Replay::frame`]) and
//! what had happened by then ([`Replay::moments`]). It reads nothing but the
//! journal: the same journal and the same moment give the same frame every
//! time, on any machine, which is what lets a film be made of one later, at
//! any size and as often as wanted.
//!
//! **Time in a replay is not the clock's.** A session is mostly waiting: for
//! a model to answer, for someone to think. Each entry is given the time it
//! takes to *watch* ([`Pace`]): a step a little over half a second and
//! longer the more it wrote, a message as long as it takes to read, a look
//! a blink. The time that passed between two entries is cut to nothing, to
//! a beat if it was a long wait, and to a slightly longer one, which says
//! how long it was, if someone went away. An hour's work with a few hundred
//! steps plays in three or four minutes at speed 1, and in one or two at
//! the speeds a viewer picks; the clock a viewer runs is theirs, a replay
//! only answers for moments on it.
//!
//! **A step is watched arriving.** The notes and clips a step wrote are not
//! all there at once: they come in the order they sound, left to right,
//! over the first part of the step's time, and the rest of it the step
//! stands. Everything else a step did (a device, a fader, a track) is there
//! from its first moment.

use super::change::{Change, Op, States};
use super::place::{Focus, Place};
use super::{Entry, Journal, What};
use crate::{Beats, ClipId, Note, Project, TrackId};
use std::collections::HashSet;

/// How long things take to watch, in seconds at speed 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    /// A step, before what it wrote is counted.
    pub step: f64,
    /// What each note or clip a step wrote adds, by the square root of how
    /// many there were, and the most that may come to.
    pub arrival: f64,
    pub longest_arrival: f64,
    /// A step that took steps back or brought them back.
    pub reversal: f64,
    /// A call that changed nothing; and one that listened, which is given
    /// time for what it heard to be read.
    pub look: f64,
    pub listen: f64,
    /// What the person said: a pause, and so much for each letter, up to a
    /// most.
    pub message: f64,
    pub message_letter: f64,
    pub longest_message: f64,
    /// What the assistant said, likewise.
    pub reply: f64,
    pub reply_letter: f64,
    pub longest_reply: f64,
    /// The plan as it changed, and a mark.
    pub plan: f64,
    pub mark: f64,
    /// What a wait is cut to, by how long it was: under `short_wait`
    /// seconds nothing; under `long_wait` a `breath`; under `away` a
    /// `beat`; beyond that a `pause`, and the replay says how long it was.
    pub short_wait: f64,
    pub long_wait: f64,
    pub away: f64,
    pub breath: f64,
    pub beat: f64,
    pub pause: f64,
    /// The fractions of a step's time between which what it wrote arrives.
    pub arrives_from: f64,
    pub arrives_by: f64,
    /// How long something that has just arrived counts as arriving.
    pub glow: f64,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            step: 0.55,
            arrival: 0.16,
            longest_arrival: 2.6,
            reversal: 0.5,
            look: 0.28,
            listen: 0.9,
            message: 1.0,
            message_letter: 0.012,
            longest_message: 3.5,
            reply: 0.6,
            reply_letter: 0.006,
            longest_reply: 3.0,
            plan: 0.5,
            mark: 0.4,
            short_wait: 3.0,
            long_wait: 30.0,
            away: 300.0,
            breath: 0.15,
            beat: 0.4,
            pause: 1.0,
            arrives_from: 0.06,
            arrives_by: 0.82,
            glow: 0.35,
        }
    }
}

/// One entry on the replay's clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    /// Which entry, counted from the oldest the journal holds.
    pub entry: usize,
    /// When it begins, in seconds at speed 1.
    pub starts: f64,
    /// How long it is given.
    pub lasts: f64,
    /// How long the clock stood before it, in seconds, where someone was
    /// away long enough for a replay to say so.
    pub away: Option<f64>,
    /// How many notes and clips it wrote that are watched arriving.
    pub arrivals: usize,
}

impl Moment {
    pub fn ends(&self) -> f64 {
        self.starts + self.lasts
    }
}

/// Something a step wrote, at the beat where it sounds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Arriving {
    beat: Beats,
    clip: ClipId,
    /// The note, and where it is among the clip's; `None` for the clip
    /// itself.
    note: Option<(usize, Note)>,
}

/// The clips `change` adds to `before`, the song it is about to be
/// applied to.
fn added_clips(change: &Change, before: &Project) -> HashSet<ClipId> {
    let clips = change.ops().iter().filter_map(|op| match op {
        Op::Clip { clip, .. } if before.clip(clip.id).is_err() => Some(clip.id),
        _ => None,
    });
    clips.collect()
}

/// What `change` wrote that is watched arriving: the clips it added and
/// the notes it put, in the order they sound. `added` are the clips that
/// were not there before it ([`added_clips`]) and `after` the song it
/// made.
fn arriving(change: &Change, added: &HashSet<ClipId>, after: &Project) -> Vec<Arriving> {
    let mut items = Vec::new();
    let mut whole: HashSet<ClipId> = HashSet::new();
    for op in change.ops() {
        match op {
            Op::Clip { clip, .. } if added.contains(&clip.id) => {
                let Ok(clip) = after.clip(clip.id) else { continue };
                if !whole.insert(clip.id) {
                    continue;
                }
                items.push(Arriving { beat: clip.start, clip: clip.id, note: None });
                let notes = clip.notes().iter().enumerate();
                items.extend(notes.map(|(index, note)| Arriving {
                    beat: clip.start + note.start,
                    clip: clip.id,
                    note: Some((index, *note)),
                }));
            }
            Op::Notes { clip, at, put, .. } if !whole.contains(clip) => {
                let Ok(clip) = after.clip(*clip) else { continue };
                // The notes as the song ends up with them: a later part of
                // the change may have moved them.
                let notes = clip.notes().iter().enumerate().skip(*at).take(put.len());
                items.extend(notes.map(|(index, note)| Arriving {
                    beat: clip.start + note.start,
                    clip: clip.id,
                    note: Some((index, *note)),
                }));
            }
            _ => {}
        }
    }
    // A note named by two parts of one change arrives once.
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert((item.clip, item.note.map(|(index, _)| index))));
    items.sort_by(|a, b| {
        let pitch = |item: &Arriving| item.note.map(|(_, note)| note.pitch);
        a.beat.total_cmp(&b.beat).then(a.note.is_some().cmp(&b.note.is_some())).then(pitch(a).cmp(&pitch(b)))
    });
    items
}

/// A note that has just arrived, for a view to light up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrived {
    pub track: TrackId,
    pub clip: ClipId,
    /// The note; `None` for the clip itself.
    pub note: Option<Note>,
    /// Where on the timeline, in beats.
    pub beat: Beats,
    /// How long ago it arrived, in seconds at speed 1.
    pub age: f64,
}

/// A journal laid out to be watched.
pub struct Replay {
    journal: Journal,
    pace: Pace,
    moments: Vec<Moment>,
    length: f64,
    /// The first entry whose change does not apply, with what is wrong: the
    /// replay ends before it.
    broken: Option<(usize, String)>,
}

/// The seconds between two entries, as the clock had them.
fn waited(before: &Entry, entry: &Entry) -> f64 {
    entry.at.saturating_sub(before.at) as f64 / 1000.0
}

impl Replay {
    /// Lays `journal` out at the usual pace.
    pub fn new(journal: Journal) -> Self {
        Self::paced(journal, Pace::default())
    }

    /// Lays `journal` out at `pace`.
    pub fn paced(journal: Journal, pace: Pace) -> Self {
        let mut moments = Vec::with_capacity(journal.len());
        let mut broken = None;
        let mut clock = 0.0;
        // The song is walked through once, to count what each step wrote.
        let mut song = journal.base(States::Without).unwrap_or_else(|_| Project::new("Untitled"));
        let mut previous: Option<&Entry> = None;
        for (index, entry) in journal.entries().enumerate() {
            // An aside is nobody's doing: it is applied in passing.
            if entry.is_aside() {
                if let Some(step) = entry.step()
                    && let Err(error) = step.change.apply(&mut song, States::Without)
                {
                    broken = Some((index, error.0));
                    break;
                }
                moments.push(Moment { entry: index, starts: clock, lasts: 0.0, away: None, arrivals: 0 });
                continue;
            }
            let gap = previous.map_or(0.0, |before| waited(before, entry));
            let (wait, away) = match gap {
                gap if gap < pace.short_wait => (0.0, None),
                gap if gap < pace.long_wait => (pace.breath, None),
                gap if gap < pace.away => (pace.beat, None),
                gap => (pace.pause, Some(gap)),
            };
            let mut arrivals = 0;
            let lasts = match &entry.what {
                What::Step(step) => {
                    let added = added_clips(&step.change, &song);
                    if let Err(error) = step.change.apply(&mut song, States::Without) {
                        broken = Some((index, error.0));
                        break;
                    }
                    if step.reverses.is_some() {
                        pace.reversal
                    } else {
                        arrivals = arriving(&step.change, &added, &song).len();
                        // One thing is simply there; several are watched.
                        if arrivals < 2 {
                            arrivals = 0;
                        }
                        let watching = (pace.arrival * (arrivals as f64).sqrt()).min(pace.longest_arrival);
                        pace.step + watching + if step.heard.is_some() { pace.listen } else { 0.0 }
                    }
                }
                What::Said { text } => {
                    let letters = text.chars().count() as f64;
                    match entry.by {
                        super::Author::Person => {
                            (pace.message + pace.message_letter * letters).min(pace.longest_message)
                        }
                        _ => (pace.reply + pace.reply_letter * letters).min(pace.longest_reply),
                    }
                }
                What::Plan { .. } => pace.plan,
                What::Look(look) if look.heard.is_some() => pace.listen,
                What::Look(_) => pace.look,
                What::Mark { .. } => pace.mark,
            };
            clock += wait;
            moments.push(Moment { entry: index, starts: clock, lasts, away, arrivals });
            clock += lasts;
            previous = Some(entry);
        }
        Self { journal, pace, moments, length: clock, broken }
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    pub fn pace(&self) -> &Pace {
        &self.pace
    }

    /// Every entry on the replay's clock, in order.
    pub fn moments(&self) -> &[Moment] {
        &self.moments
    }

    /// The entry a moment is of.
    pub fn entry(&self, moment: &Moment) -> &Entry {
        self.journal.get(moment.entry).expect("a moment is of an entry the journal holds")
    }

    /// How long the replay is, in seconds at speed 1.
    pub fn length(&self) -> f64 {
        self.length
    }

    /// The speed at which the replay lasts `seconds`.
    pub fn speed_to_last(&self, seconds: f64) -> f64 {
        if seconds > 0.0 && self.length > 0.0 { self.length / seconds } else { 1.0 }
    }

    /// What is wrong with the journal, if the replay had to end before its
    /// last entry: a step that does not apply to the song before it.
    pub fn broken(&self) -> Option<&str> {
        self.broken.as_ref().map(|(_, what)| what.as_str())
    }

    /// The moment under way at `time`: the last that has begun. `None`
    /// before the first.
    pub fn moment_at(&self, time: f64) -> Option<usize> {
        self.moments.partition_point(|moment| moment.starts <= time).checked_sub(1)
    }

    /// The moments that begin after `from` and no later than `to`: what
    /// happened between two times, in order.
    pub fn between(&self, from: f64, to: f64) -> &[Moment] {
        let first = self.moments.partition_point(|moment| moment.starts <= from);
        let last = self.moments.partition_point(|moment| moment.starts <= to);
        &self.moments[first.min(last)..last]
    }

    /// The moment of the first step at or after the moment numbered
    /// `from`, or before it when `back`: where a viewer steps to.
    pub fn step_from(&self, from: usize, back: bool) -> Option<usize> {
        let is_step = |index: &usize| self.entry(&self.moments[*index]).step().is_some_and(|step| !step.aside);
        if back {
            (0..from.min(self.moments.len())).rev().find(is_step)
        } else {
            (from..self.moments.len()).find(is_step)
        }
    }

    /// The song as it was at `time`, and what was going on: see [`Frame`].
    /// `cursor` remembers where the replay was last looked at, so that
    /// looking a little later costs what happened in between and no more.
    pub fn frame<'c>(&self, time: f64, cursor: &'c mut Cursor) -> Frame<'c> {
        let moment = self.moment_at(time);
        let applied = moment.map_or(0, |index| index + 1);
        cursor.seek(self, applied);
        let Some(index) = moment else {
            let cursor: &'c Cursor = cursor;
            return Frame {
                time,
                song: &cursor.song,
                version: cursor.version,
                moment: None,
                progress: 0.0,
                focus: Focus::default(),
                arrived: &[],
            };
        };
        let now = self.moments[index];
        let progress = if now.lasts > 0.0 { ((time - now.starts) / now.lasts).clamp(0.0, 1.0) } else { 1.0 };

        // How much of what the step wrote has arrived.
        let total = cursor.arriving.len();
        let pace = &self.pace;
        let through = ((progress - pace.arrives_from) / (pace.arrives_by - pace.arrives_from)).clamp(0.0, 1.0);
        let shown = if total == 0 { 0 } else { ((through * total as f64).floor() as usize).min(total) };
        cursor.stage(shown);

        // What arrived in the last moments, with how long ago.
        cursor.arrived.clear();
        if total > 0 {
            let each = now.lasts * (pace.arrives_by - pace.arrives_from) / total as f64;
            let first = now.starts + now.lasts * pace.arrives_from;
            for (order, item) in cursor.arriving.iter().enumerate().take(shown).rev() {
                let age = time - (first + each * (order + 1) as f64);
                if age > pace.glow {
                    break;
                }
                let Ok(track) = cursor.song.clip_track(item.clip) else { continue };
                let note = item.note.map(|(_, note)| note);
                cursor.arrived.push(Arrived { track, clip: item.clip, note, beat: item.beat, age: age.max(0.0) });
            }
        }

        // Where the action is: where this entry landed or, for one that
        // changed nothing, where the last step before it did.
        let cursor: &'c Cursor = cursor;
        let song = cursor.staged.as_ref().map_or(&cursor.song, |(_, staged)| staged);
        let places = self.places_at(index);
        let focus = Focus::of(places, song);
        Frame { time, song, version: cursor.version, moment: Some(index), progress, focus, arrived: &cursor.arrived }
    }

    /// Where the moment numbered `index` landed in the song: the places of
    /// its step or, for an entry that changed nothing, of the last step
    /// before it. This is where a view goes and a camera looks.
    pub fn places_at(&self, index: usize) -> &[Place] {
        let steps = self.moments[..=index].iter().rev().filter_map(|moment| self.entry(moment).step());
        let steps = steps.filter(|step| !step.aside);
        steps.map(|step| step.places.as_slice()).find(|places| !places.is_empty()).unwrap_or(&[])
    }
}

/// A moment of a replay: the song as it was, and what was going on.
#[derive(Debug)]
pub struct Frame<'c> {
    /// The moment asked for, in seconds at speed 1.
    pub time: f64,
    /// The song as it was: every step before, and of the step under way
    /// what has arrived. Without its devices' states.
    pub song: &'c Project,
    /// A number that is another whenever the cursor's song is: a view
    /// that drew the song at one version need not look at it again until
    /// it sees the next.
    pub version: u64,
    /// The moment under way, as numbered in [`Replay::moments`]; `None`
    /// before the first.
    pub moment: Option<usize>,
    /// How far through that moment, from 0 to 1.
    pub progress: f64,
    /// Where the action is: what a view brings into sight.
    pub focus: Focus,
    /// The notes and clips that have just arrived, newest first.
    pub arrived: &'c [Arrived],
}

/// Where a replay was last looked at: the song as it was there, kept so
/// that the next look does not begin from the beginning.
pub struct Cursor {
    /// How many entries `song` has had applied.
    applied: usize,
    song: Project,
    /// What the last entry applied wrote that is watched arriving.
    arriving: Vec<Arriving>,
    /// `song` with only so many of those arrived, while not all have.
    staged: Option<(usize, Project)>,
    arrived: Vec<Arrived>,
    /// Moves on whenever the song a frame shows is another.
    version: u64,
    /// The song as it was after so many entries, kept every few hundred as
    /// they are passed: looking back begins from the nearest, not from the
    /// beginning.
    kept: Vec<(usize, Project)>,
}

/// How many entries lie between two songs a cursor keeps to look back from.
const KEPT_EVERY: usize = 256;

impl Cursor {
    /// A cursor at the beginning of `replay`.
    pub fn new(replay: &Replay) -> Self {
        let song = replay.journal.base(States::Without).unwrap_or_else(|_| Project::new("Untitled"));
        Self { applied: 0, song, arriving: Vec::new(), staged: None, arrived: Vec::new(), version: 0, kept: Vec::new() }
    }

    /// Makes `song` the song after `count` entries.
    fn seek(&mut self, replay: &Replay, count: usize) {
        // The replay ends before an entry that does not apply.
        let count = count.min(replay.moments.len());
        if count == self.applied {
            return;
        }
        if count < self.applied {
            // Back to the nearest song that was kept, or to the beginning.
            match self.kept.iter().rev().find(|(applied, _)| *applied <= count) {
                Some((applied, song)) => {
                    self.applied = *applied;
                    self.song.clone_from(song);
                }
                None => {
                    self.applied = 0;
                    self.song = replay.journal.base(States::Without).unwrap_or_else(|_| Project::new("Untitled"));
                }
            }
        }
        self.version += 1;
        self.staged = None;
        self.arriving.clear();
        for index in self.applied..count {
            let Some(step) = replay.journal.get(index).and_then(Entry::step) else { continue };
            // Only the last step applied is watched arriving.
            let watched = index + 1 == count && replay.moments[index].arrivals > 0;
            let added = watched.then(|| added_clips(&step.change, &self.song));
            if step.change.apply(&mut self.song, States::Without).is_err() {
                break;
            }
            if let Some(added) = added {
                self.arriving = arriving(&step.change, &added, &self.song);
            }
            let passed = index + 1;
            if passed % KEPT_EVERY == 0 && self.kept.last().is_none_or(|(applied, _)| *applied < passed) {
                self.kept.push((passed, self.song.clone()));
            }
        }
        self.applied = count;
    }

    /// Makes the staged song the one with the first `shown` arrivals.
    fn stage(&mut self, shown: usize) {
        if shown >= self.arriving.len() {
            if self.staged.take().is_some() {
                self.version += 1;
            }
            return;
        }
        if self.staged.as_ref().is_some_and(|(staged, _)| *staged == shown) {
            return;
        }
        self.version += 1;
        let mut song = self.song.clone();
        let late = &self.arriving[shown..];
        // The clips that have not arrived go whole; of the others, the
        // notes that have not, from the last to the first so that the
        // places of the rest hold.
        let absent: HashSet<ClipId> = late.iter().filter(|item| item.note.is_none()).map(|item| item.clip).collect();
        let mut notes: Vec<(ClipId, usize)> = late
            .iter()
            .filter(|item| !absent.contains(&item.clip))
            .filter_map(|item| Some((item.clip, item.note?.0)))
            .collect();
        notes.sort_unstable_by(|a, b| (a.0, b.1).cmp(&(b.0, a.1)));
        for track in &mut song.tracks {
            track.clips.retain(|clip| !absent.contains(&clip.id));
        }
        let mut at = 0;
        while at < notes.len() {
            let clip = notes[at].0;
            let end = at + notes[at..].iter().take_while(|(id, _)| *id == clip).count();
            if let Ok(held) = song.clip_mut(clip) {
                let mut kept = held.notes().to_vec();
                for (_, index) in &notes[at..end] {
                    if *index < kept.len() {
                        kept.remove(*index);
                    }
                }
                // What is left is a part of what was valid whole.
                let _ = held.set_notes(kept);
            }
            at = end;
        }
        self.staged = Some((shown, song));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Author, Outcome, PlanItem, PlanState, Reversal, Who};
    use crate::{ChainRef, Note};

    const SECOND: u64 = 1_000;
    const MINUTE: u64 = 60 * SECOND;

    fn note_at(start: f64, pitch: u8) -> Note {
        Note { pitch, start, length: 0.25, velocity: 100 }
    }

    /// Records an edit at `at`.
    fn step<R>(journal: &mut Journal, song: &mut Project, at: u64, edit: impl FnOnce(&mut Project) -> R) {
        let before = song.clone();
        edit(song);
        journal.record(at, Who::Person, &before, song, None).expect("the edit changed the song");
    }

    /// A short session: a request, a plan, a track with an instrument, a
    /// clip written with sixteen notes, a listen, a fader, and after a long
    /// while one more note by hand.
    fn session() -> (Replay, Project, ClipId, TrackId) {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        journal.say(SECOND, Author::Person, "Write me a bass line.");
        journal.plan(2 * SECOND, vec![PlanItem { text: "Add a bass track".into(), state: PlanState::Doing }]);
        let mut track = TrackId(0);
        step(&mut journal, &mut song, 20 * SECOND, |p| {
            track = p.add_track("Bass");
            p.add_device(ChainRef::Track(track), "builtin:polysynth", "Poly Synth", None).unwrap();
        });
        let mut clip = ClipId(0);
        step(&mut journal, &mut song, 21 * SECOND, |p| {
            clip = p.add_midi_clip(track, "Line", 8.0, 8.0).unwrap();
            let notes: Vec<Note> = (0..16).map(|index| note_at(index as f64 * 0.5, 36 + (index % 5) as u8)).collect();
            p.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        });
        let call = journal.begin_call(Author::Assistant, "measure", "Measured the song");
        journal.end_call(3 * MINUTE, call, Outcome { heard: Some("-16.0 LUFS".into()), ..Outcome::default() });
        step(&mut journal, &mut song, 3 * MINUTE + SECOND, |p| p.set_track_volume(track, -4.0).unwrap());
        journal.say(3 * MINUTE + 2 * SECOND, Author::Assistant, "Done: a bass line in bars 3 and 4.");
        step(&mut journal, &mut song, 50 * MINUTE, |p| {
            p.clip_mut(clip).unwrap().add_notes(&[note_at(7.75, 48)]).unwrap();
        });
        (Replay::new(journal), song, clip, track)
    }

    #[test]
    fn idle_time_is_cut_and_a_long_wait_is_a_beat() {
        let (replay, ..) = session();
        let pace = *replay.pace();
        let moments = replay.moments();
        assert_eq!(moments.len(), 8);
        // Fifty minutes by the clock, and under ten seconds to watch.
        assert!(replay.length() > 5.0 && replay.length() < 10.0, "{} s", replay.length());
        // Entries follow one another without a gap where the clock moved
        // on at once...
        assert_eq!(moments[0].starts, 0.0);
        assert_eq!(moments[1].starts, moments[0].ends());
        // ...after a breath where it stood a while (18 s)...
        assert!((moments[2].starts - moments[1].ends() - pace.breath).abs() < 1e-9);
        // ...a beat where it stood for minutes...
        assert!((moments[4].starts - moments[3].ends() - pace.beat).abs() < 1e-9);
        // ...and a pause that knows how long it was where someone left.
        assert!((moments[7].starts - moments[6].ends() - pace.pause).abs() < 1e-9);
        assert_eq!(moments[7].away, Some(46.0 * 60.0 + 58.0));
        assert!(moments[..7].iter().all(|moment| moment.away.is_none()));

        // What an entry is given goes by what it is: the step that wrote
        // sixteen notes longer than the one that moved a fader, the listen
        // longer than a look, a message by its length.
        assert_eq!(moments[3].arrivals, 17, "the clip and its sixteen notes");
        assert!(moments[3].lasts > moments[5].lasts + 0.5);
        assert_eq!(moments[5].lasts, pace.step);
        assert_eq!(moments[4].lasts, pace.listen);
        assert!(moments[0].lasts > pace.message && moments[0].lasts < pace.longest_message);
        // A single note by hand is simply there.
        assert_eq!((moments[7].arrivals, moments[7].lasts), (0, pace.step));

        // The same journal lays out the same way twice.
        let again = Replay::new(Journal::from_text(&replay.journal().to_text(true)).unwrap());
        assert_eq!(again.moments(), replay.moments());
        assert_eq!(replay.speed_to_last(replay.length() / 4.0), 4.0);
    }

    #[test]
    fn the_song_at_any_moment_is_the_song_as_it_was_and_a_step_is_watched_arriving() {
        let (replay, song, clip, track) = session();
        let moments = replay.moments().to_vec();
        let mut cursor = Cursor::new(&replay);

        // Before anything: the song as it began, and no moment under way.
        let frame = replay.frame(-1.0, &mut cursor);
        assert!(frame.moment.is_none() && frame.song.tracks.is_empty());
        // During the request and the plan nothing is there yet.
        let frame = replay.frame(moments[1].starts + 0.1, &mut cursor);
        assert_eq!((frame.moment, frame.song.tracks.len()), (Some(1), 0));
        assert!(frame.focus.is_empty());

        // The track and its device are there from the step's first moment.
        let frame = replay.frame(moments[2].starts, &mut cursor);
        assert_eq!((frame.song.tracks.len(), frame.song.tracks[0].devices.len()), (1, 1));
        assert_eq!(frame.focus.tracks, [track]);

        // The clip arrives first, then its notes in the order they sound.
        let writing = moments[3];
        let at = |fraction: f64| writing.starts + writing.lasts * fraction;
        let frame = replay.frame(at(0.0), &mut cursor);
        assert!(frame.song.clip(clip).is_err(), "not there in the step's first instant");
        let mut counts = Vec::new();
        for tenth in 1..=10 {
            let frame = replay.frame(at(tenth as f64 / 10.0 - 0.001), &mut cursor);
            let notes = frame.song.clip(clip).map_or(0, |clip| clip.notes().len());
            // What is there is the beginning of the line, in order.
            let whole = song.clip(clip).unwrap().notes();
            assert_eq!(frame.song.clip(clip).map_or(&[][..], |clip| clip.notes()), &whole[..notes]);
            counts.push(notes);
        }
        assert!(counts.windows(2).all(|pair| pair[0] <= pair[1]), "notes only ever arrive: {counts:?}");
        assert!(counts[0] < 3 && counts[4] > 5 && counts[4] < 13, "{counts:?}");
        assert_eq!(counts[9], 16, "all there before the step is over: {counts:?}");
        assert_eq!(counts[8], 16, "and the step stands a while: {counts:?}");
        // While they arrive the action is where the clip is, and the last
        // to arrive are named for a view to light up.
        let frame = replay.frame(at(0.4), &mut cursor);
        assert_eq!((frame.focus.tracks.as_slice(), frame.focus.span), (&[track][..], Some((8.0, 16.0))));
        let newest = frame.arrived.first().expect("something has just arrived");
        assert_eq!((newest.track, newest.clip), (track, clip));
        assert!(newest.age >= 0.0 && newest.age <= replay.pace().glow);
        assert!(frame.arrived.windows(2).all(|pair| pair[0].beat >= pair[1].beat), "newest first");

        // A look changes nothing, the action stays where it was, and
        // nothing is arriving any more.
        let frame = replay.frame(moments[4].starts + 0.1, &mut cursor);
        assert!(frame.arrived.is_empty());
        assert_eq!(frame.song.clip(clip).unwrap().notes().len(), 16);
        assert_eq!(frame.focus.tracks, [track]);
        assert_eq!(frame.song.track(track).unwrap().volume_db, 0.0);
        // Then the fader, and at the end the song as it is, with nothing
        // left to arrive.
        assert_eq!(replay.frame(moments[5].starts, &mut cursor).song.track(track).unwrap().volume_db, -4.0);
        let mut bare = song.clone();
        for device in bare.tracks.iter_mut().flat_map(|track| &mut track.devices) {
            device.state = None;
        }
        let frame = replay.frame(replay.length() + 5.0, &mut cursor);
        assert_eq!((frame.song, frame.moment, frame.progress), (&bare, Some(7), 1.0));

        // Looking back is the same as having looked there first, and the
        // same moment is the same frame twice.
        let early = moments[3].starts + moments[3].lasts * 0.5;
        let back = replay.frame(early, &mut cursor).song.clone();
        let fresh = replay.frame(early, &mut Cursor::new(&replay)).song.clone();
        assert_eq!(back, fresh);
        assert_eq!(replay.frame(early, &mut cursor).song, &fresh);

        // The version moves when the song shown does, and only then.
        let mut cursor = Cursor::new(&replay);
        let mut seen = Vec::new();
        for hundredth in 0..=100 {
            let frame = replay.frame(at(hundredth as f64 / 100.0), &mut cursor);
            seen.push((frame.version, frame.song.clip(clip).map_or(0, |clip| clip.notes().len() + 1)));
        }
        assert!(seen.windows(2).all(|pair| (pair[0].0 == pair[1].0) == (pair[0].1 == pair[1].1)), "{seen:?}");
        let versions: HashSet<u64> = seen.iter().map(|(version, _)| *version).collect();
        assert_eq!(versions.len(), 18, "the song before, and one more for each thing that arrives");
    }

    #[test]
    fn looking_back_in_a_long_session_shows_the_song_as_it_was() {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        let mut track = TrackId(0);
        step(&mut journal, &mut song, 0, |p| track = p.add_track("Keys"));
        let mut songs = vec![song.clone()];
        for index in 1..1_000u64 {
            step(&mut journal, &mut song, index * SECOND, |p| p.set_track_volume(track, -(index as f32) / 100.0));
            songs.push(song.clone());
        }
        let replay = Replay::new(journal);
        let moments = replay.moments().to_vec();
        let mut cursor = Cursor::new(&replay);
        // Forward to the end, then back and forth across the songs it kept
        // on the way: every look is the song as it was there.
        for index in [999, 3, 255, 256, 257, 700, 511, 512, 0, 998, 600] {
            let frame = replay.frame(moments[index].starts, &mut cursor);
            assert_eq!(frame.song, &songs[index], "at entry {index}");
        }
        assert_eq!(cursor.kept.len(), 3, "a song kept for every {KEPT_EVERY} entries passed");
    }

    #[test]
    fn what_happened_between_two_times_is_listed_in_order() {
        let (replay, ..) = session();
        let moments = replay.moments();
        assert_eq!(replay.between(-1.0, replay.length()).len(), 8);
        assert!(replay.between(0.0, 0.0).is_empty(), "what begins at `from` was there already");
        let some = replay.between(moments[1].starts, moments[4].starts);
        assert_eq!(some.iter().map(|moment| moment.entry).collect::<Vec<_>>(), [2, 3, 4]);
        assert!(replay.between(5.0, 1.0).is_empty());
        let lines: Vec<String> =
            replay.between(-1.0, moments[2].starts).iter().map(|m| replay.entry(m).line()).collect();
        assert_eq!(lines, ["Write me a bass line.", "Plan: 0 of 1 done", "Added the track \u{201C}Bass\u{201D}"]);

        assert_eq!(replay.moment_at(-0.5), None);
        assert_eq!(replay.moment_at(0.0), Some(0));
        assert_eq!(replay.moment_at(1e9), Some(7));
        // Stepping from one step to the next, either way.
        assert_eq!(replay.step_from(0, false), Some(2));
        assert_eq!(replay.step_from(3, false), Some(3));
        assert_eq!(replay.step_from(4, false), Some(5));
        assert_eq!(replay.step_from(5, true), Some(3));
        assert_eq!(replay.step_from(2, true), None);
        assert_eq!(replay.step_from(8, false), None);
    }

    #[test]
    fn steps_taken_back_play_as_they_happened_and_a_damaged_journal_plays_up_to_the_damage() {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        let mut track = TrackId(0);
        step(&mut journal, &mut song, SECOND, |p| track = p.add_track("Keys"));
        let before = song.clone();
        step(&mut journal, &mut song, 2 * SECOND, |p| p.remove_track(track).unwrap());
        journal.reverse(3 * SECOND, Who::Person, Reversal::Undo, (2, 2), ("Deleted the track", 1), &song, &before);
        let replay = Replay::new(journal);
        let mut cursor = Cursor::new(&replay);
        let tracks =
            |index: usize, cursor: &mut Cursor| replay.frame(replay.moments()[index].starts, cursor).song.tracks.len();
        assert_eq!([tracks(0, &mut cursor), tracks(1, &mut cursor), tracks(2, &mut cursor)], [1, 0, 1]);
        assert_eq!(replay.moments()[2].lasts, replay.pace().reversal);
        assert!(replay.broken().is_none());

        // A journal whose third step names a track that is not there.
        let text = replay.journal().to_text(true);
        let damaged = text;
        let last = damaged.rfind("\"tracks\":[1]").expect("the step that brought the track back");
        let damaged = format!("{}\"tracks\":[9]{}", &damaged[..last], &damaged[last + 12..]);
        let replay = Replay::new(Journal::from_text(&damaged).unwrap());
        assert_eq!(replay.moments().len(), 2, "it plays up to the step that does not apply");
        assert!(replay.broken().is_some_and(|what| what.contains("track 9")), "{:?}", replay.broken());
        let mut cursor = Cursor::new(&replay);
        assert_eq!(replay.frame(1e9, &mut cursor).song.tracks.len(), 0);
    }
}
