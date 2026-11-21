//! The director: which shot each stretch of the song is filmed in.
//!
//! A [`Plan`] is a list of [`Take`]s that covers the song. It is decided
//! from what the song is and from nothing else: its sections, how much
//! plays in each, which part carries the tune and where a part comes in.
//! The same song is always directed the same way, and a plan can be read
//! and tested without a screen.
//!
//! The rules, in the order they are tried for each section:
//!
//! * The song opens on all of itself and closes on it: the first take
//!   starts wide enough to hold the whole arrangement and moves in on the
//!   playhead, and the last one moves back out.
//! * Where one part stands out as the tune and is not there for most of
//!   the song (a solo, a hook), the shot goes in on its piano roll.
//! * Where the song is at its fullest (a chorus), the shot is wide: every
//!   lane that plays, and nothing else.
//! * Where little plays (a break), the shot is in on what is left.
//! * Otherwise the lanes share the frame with the piano rolls of the
//!   parts that drive the section: the drums and the busiest other part.
//! * A section twice as long as the usual gets a second take halfway.
//!
//! A take is entered by a cut when the song jumps (it gets much fuller or
//! emptier, the shot changes between wide and not, or the camera goes in
//! on a part it was not showing) and by a move of the camera otherwise.

use crate::score::{Part, Score, Section};
use trill_core::{Beats, TrackId};

/// How a shot is framed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The whole song in the frame; the playhead crosses it.
    Overview,
    /// The lanes of everything that plays.
    Wide,
    /// The lanes above, the piano rolls of two or three parts below.
    Ensemble,
    /// One part's piano roll, with the lanes as a strip above it.
    Close,
}

/// What the camera shows during a take.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub kind: Kind,
    /// The parts shown as piano rolls, top to bottom.
    pub rolls: Vec<TrackId>,
    /// How many beats a frame as wide as it is high times 16:9 holds across, at
    /// the start of the take and at its end: a take creeps in or out as it
    /// goes. Narrower frames hold less at the same scale.
    pub window: (Beats, Beats),
    /// The share of the stage's height the piano rolls take, 0..=1.
    pub roll_share: f32,
}

/// How a take is entered.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Entry {
    /// On the beat, at once.
    Cut,
    /// The camera travels for this many beats, half of them before the
    /// take begins.
    Move(Beats),
}

/// One stretch of the song filmed in one shot.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub start: Beats,
    pub end: Beats,
    /// The section of the score it is part of.
    pub section: usize,
    pub shot: Shot,
    pub entry: Entry,
}

/// The takes of a song, in order and without gaps, from its first beat to
/// its last.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub takes: Vec<Take>,
}

/// What one part does in one section.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Activity {
    /// Notes that start in the section.
    pub notes: usize,
    /// Note starts per beat.
    pub rate: f64,
    /// Mean velocity, 0..=1.
    pub velocity: f64,
    /// How many notes sound at once, on average, while any does.
    pub voices: f64,
    /// Mean pitch.
    pub pitch: f64,
    /// Semitones between its lowest and highest note.
    pub span: u8,
}

/// A part needs this many notes in a section to be worth a piano roll.
const ROLL_NOTES: usize = 8;
/// A section this much fuller (or emptier) than the one before is cut to.
const JUMP: f64 = 0.22;
/// Beats a camera move takes.
const MOVE_BEATS: Beats = 4.0;
/// How much a take creeps in while it runs.
const CREEP: f64 = 0.9;
/// The frame holds this many bars across in each kind of shot.
const WIDE_BARS: f64 = 8.0;
const ENSEMBLE_BARS: f64 = 4.0;
/// A close shot holds about this many notes across, within these bars.
const CLOSE_NOTES: f64 = 26.0;
const CLOSE_BARS: (f64, f64) = (1.0, 4.0);
/// The opening and closing moves last this many bars.
const OPENING_BARS: f64 = 2.0;
/// Shares of the stage the rolls take.
const ENSEMBLE_SHARE: f32 = 0.68;
const CLOSE_SHARE: f32 = 0.8;
/// How full a section must be, against the fullest, to be filmed wide, and
/// how empty to be filmed close.
const FULL: f64 = 0.86;
const SPARSE: f64 = 0.45;
/// A part that stands this far above the others as the tune gets the shot.
const LEAD_SCORE: f64 = 0.62;
/// Words in a section's name that say what it is.
const PEAK_WORDS: [&str; 4] = ["chorus", "hook", "drop", "refrain"];
const SOLO_WORDS: [&str; 3] = ["solo", "lead", "melody"];
/// Words in a part's name that say it carries the tune.
const TUNE_WORDS: [&str; 10] = ["lead", "solo", "melody", "vox", "vocal", "voice", "sax", "violin", "flute", "trumpet"];

/// How the director reads a song: what each part does in each section,
/// how full each section is and how much each part is its tune. The plan
/// follows from this and from the sections' names.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// By section, then by part, in the score's order.
    pub activity: Vec<Vec<Activity>>,
    /// How full each section is against the fullest, 0..=1.
    pub fullness: Vec<f64>,
    /// How much each part is the tune of each section, 0 to a little over 1.
    pub tunes: Vec<Vec<f64>>,
}

impl Reading {
    pub fn of(score: &Score) -> Self {
        let activity: Vec<Vec<Activity>> = score
            .sections
            .iter()
            .map(|section| score.parts.iter().map(|part| activity(part, section.start, section.end)).collect())
            .collect();
        let fullness = fullness(&activity);
        // How many sections each part plays in: a part that is seldom
        // there is an event when it is.
        let played: Vec<usize> = (0..score.parts.len())
            .map(|part| activity.iter().filter(|section| section[part].notes > 0).count())
            .collect();
        let tunes = activity.iter().map(|section| tune_scores(score, section, &played)).collect();
        Self { activity, fullness, tunes }
    }
}

impl Plan {
    /// Directs `score`.
    pub fn direct(score: &Score) -> Self {
        let Reading { activity, fullness, tunes } = Reading::of(score);

        let mut takes: Vec<Take> = Vec::new();
        for (index, section) in score.sections.iter().enumerate() {
            // A frame is so many bars across: of the bars the section has.
            let bar = score.bar_length(section.start);
            let shot = shot_for(score, section, &activity[index], fullness[index], &tunes[index], bar);
            let entry = match takes.last() {
                None => Entry::Cut,
                Some(before) => {
                    let jump = (fullness[index] - fullness[before.section]).abs() >= JUMP;
                    let turn = (before.shot.kind == Kind::Wide) != (shot.kind == Kind::Wide);
                    // Going in on a part the frame did not hold: its entrance.
                    let entrance = shot.kind == Kind::Close && before.shot.rolls != shot.rolls;
                    if jump || turn || entrance { Entry::Cut } else { Entry::Move(MOVE_BEATS.min(section.length())) }
                }
            };
            // A long section is filmed twice: the second half from the
            // other distance.
            let usual = usual_length(&score.sections);
            let halves = section.length() >= 2.0 * usual && section.length() >= 8.0 * bar;
            if halves {
                let middle = score.bar_line_near(section.start + section.length() / 2.0);
                let second = other_shot(score, &shot, section, &activity[index], bar);
                takes.push(Take { start: section.start, end: middle, section: index, shot, entry });
                takes.push(Take {
                    start: middle,
                    end: section.end,
                    section: index,
                    shot: second,
                    entry: Entry::Move(MOVE_BEATS),
                });
            } else {
                takes.push(Take { start: section.start, end: section.end, section: index, shot, entry });
            }
        }
        open_and_close(score, &mut takes);
        Self { takes }
    }

    /// Directs `score`, and then does what `directions` ask of it, in
    /// their order: where two speak of the same stretch the later one has
    /// it. With none this is [`Plan::direct`].
    pub fn directed(score: &Score, directions: &[Direction]) -> Self {
        let mut plan = Self::direct(score);
        for direction in directions {
            let (from, to) = (direction.from.clamp(0.0, score.length), direction.to.clamp(0.0, score.length));
            if to <= from || plan.takes.is_empty() {
                continue;
            }
            plan.split_at(from);
            plan.split_at(to);
            let rolls: Vec<TrackId> =
                direction.rolls.iter().copied().filter(|track| score.part(*track).is_some()).collect();
            // Parts that are named are shown: one close, more as an ensemble.
            let kind = direction.kind.or(match rolls.len() {
                0 => None,
                1 => Some(Kind::Close),
                _ => Some(Kind::Ensemble),
            });
            if let Some(kind) = kind {
                // The stretch becomes one take, so that it creeps in once.
                let first = plan.takes.partition_point(|take| take.start < from);
                let last = plan.takes.partition_point(|take| take.start < to);
                let section = score.section_at(from);
                let shot = shot_of(score, kind, &rolls, from, to, score.bar_length(from));
                let take = Take { start: from, end: to, section, shot, entry: Entry::Cut };
                plan.takes.splice(first..last, [take]);
                // How it is come to and left, unless that is said: by a
                // cut where the frame changes its kind or its parts.
                for index in [first, first + 1] {
                    if index == 0 || index >= plan.takes.len() {
                        continue;
                    }
                    let before = plan.takes[index - 1].shot.clone();
                    let take = &mut plan.takes[index];
                    let same = before.kind == take.shot.kind && before.rolls == take.shot.rolls;
                    take.entry = if same { Entry::Move(MOVE_BEATS.min(take.end - take.start)) } else { Entry::Cut };
                }
            }
            if let Some(entry) = direction.entry
                && let Some(take) = plan.takes.iter_mut().find(|take| take.start == from)
                && from > 0.0
            {
                take.entry = match entry {
                    Entry::Cut => Entry::Cut,
                    Entry::Move(beats) => Entry::Move(beats.clamp(0.5, take.end - take.start)),
                };
            }
        }
        plan
    }

    /// Cuts the take that `beat` falls inside in two there. The second
    /// half goes on as the first did: the camera does not notice.
    fn split_at(&mut self, beat: Beats) {
        let Some(index) = self.takes.iter().position(|take| take.start < beat && beat < take.end) else { return };
        let mut second = self.takes[index].clone();
        // Each half creeps over its own length, from where the whole had got to.
        let whole = &mut self.takes[index];
        let through = (beat - whole.start) / (whole.end - whole.start);
        let at = whole.shot.window.0 + (whole.shot.window.1 - whole.shot.window.0) * through;
        (whole.end, whole.shot.window.1) = (beat, at);
        (second.start, second.shot.window.0, second.entry) = (beat, at, Entry::Move(0.0));
        self.takes.insert(index + 1, second);
    }

    /// The take that `beat` falls in: the first before the song and the
    /// last after it.
    pub fn take_at(&self, beat: Beats) -> usize {
        self.takes.partition_point(|take| take.end <= beat).min(self.takes.len().saturating_sub(1))
    }
}

/// What whoever films a song asks of a stretch of it, over the director's
/// own plan: how to frame it, which parts to show, how to come to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Direction {
    /// The stretch, in beats.
    pub from: Beats,
    pub to: Beats,
    /// How to frame it. `None` leaves that to the parts that are named
    /// (one is held close, several are an ensemble) or, with none named,
    /// to the director.
    pub kind: Option<Kind>,
    /// The parts to show as piano rolls, top to bottom. Empty leaves the
    /// choice to the director.
    pub rolls: Vec<TrackId>,
    /// How the stretch is entered. `None` cuts where the frame changes
    /// its kind or its parts and moves where it does not.
    pub entry: Option<Entry>,
}

/// The shot of `kind` for the stretch `from..to`, of `rolls` where they
/// are given and of the parts that drive the stretch where they are not.
fn shot_of(score: &Score, kind: Kind, rolls: &[TrackId], from: Beats, to: Beats, bar: Beats) -> Shot {
    let creeping = |beats: Beats| (beats, beats * CREEP);
    let playing: Vec<Activity> = score.parts.iter().map(|part| activity(part, from, to)).collect();
    let wide = Shot { kind: Kind::Wide, rolls: Vec::new(), window: creeping(WIDE_BARS * bar), roll_share: 0.0 };
    match kind {
        Kind::Overview => Shot { kind, rolls: Vec::new(), window: (score.length, score.length), roll_share: 0.0 },
        Kind::Wide => wide,
        Kind::Ensemble => {
            let rolls: Vec<TrackId> = match rolls.is_empty() {
                true => drivers(score, &playing, &vec![0.0; score.parts.len()]),
                false => rolls.iter().copied().take(3).collect(),
            };
            match rolls.is_empty() {
                true => wide,
                false => Shot { kind, rolls, window: creeping(ENSEMBLE_BARS * bar), roll_share: ENSEMBLE_SHARE },
            }
        }
        Kind::Close => {
            // The part that is named or, with none named, the one the
            // director would go in on: the tune of the stretch, else the
            // busiest that is not the drums, else the drums.
            let busiest = |drums: bool| {
                let parts = (0..score.parts.len()).filter(|index| score.parts[*index].drums == drums);
                let playing = &playing;
                parts
                    .filter(|index| playing[*index].notes > 0)
                    .max_by(|a, b| playing[*a].rate.total_cmp(&playing[*b].rate).then(b.cmp(a)))
            };
            let plays_in =
                |part: &Part, section: &Section| !part.starting_between(section.start, section.end).is_empty();
            let sections_of = |part: &Part| score.sections.iter().filter(|section| plays_in(part, section)).count();
            let played: Vec<usize> = score.parts.iter().map(sections_of).collect();
            let tunes = tune_scores(score, &playing, &played);
            let tune = (0..tunes.len())
                .filter(|index| tunes[*index] > 0.0)
                .max_by(|a, b| tunes[*a].total_cmp(&tunes[*b]).then(b.cmp(a)));
            let named = rolls.first().and_then(|track| score.part_index(*track));
            let chosen = named.or(tune).or_else(|| busiest(false)).or_else(|| busiest(true));
            let Some(index) = chosen else { return wide };
            let window = close_window(playing[index].rate, bar);
            Shot { kind, rolls: vec![score.parts[index].track], window: creeping(window), roll_share: CLOSE_SHARE }
        }
    }
}

/// What `part` plays between `from` and `to`.
pub fn activity(part: &Part, from: Beats, to: Beats) -> Activity {
    let notes = part.starting_between(from, to);
    if notes.is_empty() || to <= from {
        return Activity::default();
    }
    let count = notes.len() as f64;
    let held: f64 = notes.iter().map(|note| note.length).sum();
    // The time during which any note sounds: the notes are sorted by
    // start, so one pass joins those that overlap.
    let (mut sounding, mut until) = (0.0, f64::NEG_INFINITY);
    for note in notes {
        let end = note.end().min(to);
        if note.start >= until {
            sounding += end - note.start;
            until = end;
        } else if end > until {
            sounding += end - until;
            until = end;
        }
    }
    let (low, high) = notes.iter().fold((127u8, 0u8), |(low, high), note| (low.min(note.pitch), high.max(note.pitch)));
    Activity {
        notes: notes.len(),
        rate: count / (to - from),
        velocity: notes.iter().map(|note| note.velocity as f64).sum::<f64>() / count / 127.0,
        voices: if sounding > 0.0 { (held / sounding).max(1.0) } else { 1.0 },
        pitch: notes.iter().map(|note| note.pitch as f64).sum::<f64>() / count,
        span: high - low,
    }
}

/// How full each section is against the fullest, 0..=1: how many parts
/// play, and how busily and how hard.
fn fullness(activity: &[Vec<Activity>]) -> Vec<f64> {
    let raw: Vec<f64> = activity
        .iter()
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.notes > 0)
                .map(|part| 0.35 + (part.rate / 4.0).min(1.0) * (0.4 + 0.6 * part.velocity))
                .sum()
        })
        .collect();
    let most = raw.iter().copied().fold(0.0, f64::max).max(1e-9);
    raw.iter().map(|value| value / most).collect()
}

/// The length most sections have.
fn usual_length(sections: &[Section]) -> Beats {
    let mut lengths: Vec<Beats> = sections.iter().map(Section::length).collect();
    lengths.sort_by(f64::total_cmp);
    lengths.get(lengths.len() / 2).copied().unwrap_or(16.0)
}

/// How much each part is the tune of a section, 0..=1 or a little over:
/// it sits high, plays one note at a time, moves, is seldom there, or is
/// called so.
fn tune_scores(score: &Score, activity: &[Activity], played: &[usize]) -> Vec<f64> {
    let sections = score.sections.len().max(1) as f64;
    score
        .parts
        .iter()
        .zip(activity)
        .enumerate()
        .map(|(index, (part, activity))| {
            if part.drums || activity.notes < ROLL_NOTES || activity.span < 4 {
                return 0.0;
            }
            let register = ((activity.pitch - 40.0) / 40.0).clamp(0.0, 1.0);
            let line = 1.0 / activity.voices;
            let seldom = 1.0 - played[index] as f64 / sections;
            let named = TUNE_WORDS.iter().any(|word| part.name.to_lowercase().contains(word));
            0.3 * register + 0.2 * line + 0.35 * seldom + if named { 0.25 } else { 0.0 }
        })
        .collect()
}

fn names(section: &Section, words: &[&str]) -> bool {
    let name = section.name.to_lowercase();
    words.iter().any(|word| name.contains(word))
}

/// How many beats a close shot of a part that plays at `rate` holds.
fn close_window(rate: f64, bar: Beats) -> Beats {
    let bars = (CLOSE_NOTES / rate.max(0.01) / bar).round().clamp(CLOSE_BARS.0, CLOSE_BARS.1);
    bars * bar
}

/// The parts that drive a section, for the piano rolls of an ensemble
/// shot: the drums if they play, and the busiest of the others; with the
/// tune as a third where there is room for it.
fn drivers(score: &Score, activity: &[Activity], tunes: &[f64]) -> Vec<TrackId> {
    let worth = |index: &usize| activity[*index].notes >= ROLL_NOTES;
    let drums = (0..score.parts.len()).filter(worth).find(|index| score.parts[*index].drums);
    let mut others: Vec<usize> =
        (0..score.parts.len()).filter(worth).filter(|index| !score.parts[*index].drums).collect();
    // Busiest first; of two as busy, the one that is more of a tune.
    others.sort_by(|a, b| {
        let key = |index: usize| activity[index].rate * (0.5 + activity[index].velocity) + tunes[index];
        key(*b).total_cmp(&key(*a)).then(a.cmp(b))
    });
    let mut chosen: Vec<usize> = others.into_iter().take(if drums.is_some() { 1 } else { 2 }).collect();
    chosen.extend(drums);
    // In the order of the lanes, so a roll sits under its own lane's place.
    chosen.sort_unstable();
    chosen.into_iter().map(|index| score.parts[index].track).collect()
}

fn shot_for(score: &Score, section: &Section, activity: &[Activity], fullness: f64, tunes: &[f64], bar: Beats) -> Shot {
    let lead = (0..tunes.len()).max_by(|a, b| tunes[*a].total_cmp(&tunes[*b]).then(b.cmp(a)));
    let lead = lead.filter(|index| tunes[*index] > 0.0);
    let stands_out = lead.is_some_and(|index| tunes[index] >= LEAD_SCORE || names(section, &SOLO_WORDS));
    let creeping = |beats: Beats| (beats, beats * CREEP);

    if let Some(index) = lead.filter(|_| stands_out) {
        let window = close_window(activity[index].rate, bar);
        return Shot {
            kind: Kind::Close,
            rolls: vec![score.parts[index].track],
            window: creeping(window),
            roll_share: CLOSE_SHARE,
        };
    }
    if fullness >= FULL || names(section, &PEAK_WORDS) {
        return Shot { kind: Kind::Wide, rolls: Vec::new(), window: creeping(WIDE_BARS * bar), roll_share: 0.0 };
    }
    let rolls = drivers(score, activity, tunes);
    if rolls.is_empty() {
        // Nothing with enough notes to read: the lanes are the picture.
        return Shot { kind: Kind::Wide, rolls, window: creeping(WIDE_BARS * bar), roll_share: 0.0 };
    }
    if fullness <= SPARSE {
        // Little plays: in on the busiest of what is left.
        let busiest = rolls
            .iter()
            .filter_map(|track| score.part_index(*track))
            .max_by(|a, b| activity[*a].rate.total_cmp(&activity[*b].rate).then(b.cmp(a)));
        if let Some(index) = busiest {
            return Shot {
                kind: Kind::Close,
                rolls: vec![score.parts[index].track],
                window: creeping(close_window(activity[index].rate, bar).max(2.0 * bar)),
                roll_share: CLOSE_SHARE,
            };
        }
    }
    Shot { kind: Kind::Ensemble, rolls, window: creeping(ENSEMBLE_BARS * bar), roll_share: ENSEMBLE_SHARE }
}

/// The shot for the second half of a long section: from the other
/// distance than the first.
fn other_shot(score: &Score, first: &Shot, section: &Section, activity: &[Activity], bar: Beats) -> Shot {
    let creeping = |beats: Beats| (beats, beats * CREEP);
    match first.kind {
        Kind::Wide | Kind::Overview => {
            let tunes = vec![0.0; score.parts.len()];
            let rolls = drivers(score, activity, &tunes);
            if rolls.is_empty() {
                Shot { window: creeping(WIDE_BARS * bar / 2.0), ..first.clone() }
            } else {
                Shot { kind: Kind::Ensemble, rolls, window: creeping(ENSEMBLE_BARS * bar), roll_share: ENSEMBLE_SHARE }
            }
        }
        Kind::Ensemble | Kind::Close => {
            let _ = section;
            Shot { kind: Kind::Wide, rolls: Vec::new(), window: creeping(WIDE_BARS * bar), roll_share: 0.0 }
        }
    }
}

/// Opens the song on the whole of itself and closes it there: the first
/// bars of the first take and the last of the last become takes of their
/// own, which the camera moves out of and into.
fn open_and_close(score: &Score, takes: &mut Vec<Take>) {
    let whole = Shot { kind: Kind::Overview, rolls: Vec::new(), window: (score.length, score.length), roll_share: 0.0 };
    // Of the bars the song begins with, and of the ones it ends with.
    let span = OPENING_BARS * score.bar_length(0.0);
    // Only where the song is long enough for the overview to be one.
    if score.length < 6.0 * span {
        return;
    }
    if let Some(first) = takes.first_mut().filter(|take| take.end - take.start > span) {
        let opening = Take {
            start: first.start,
            end: first.start + span,
            section: first.section,
            shot: whole.clone(),
            entry: Entry::Cut,
        };
        first.start += span;
        // A move is centered on where its take begins: the whole song is
        // held for half the opening, and the camera is in a bar later.
        first.entry = Entry::Move(span);
        takes.insert(0, opening);
    }
    let span = OPENING_BARS * score.bar_length((score.length - 1e-6).max(0.0));
    if let Some(last) = takes.last_mut().filter(|take| take.end - take.start > span) {
        let closing = Take {
            start: last.end - span,
            end: last.end,
            section: last.section,
            shot: whole,
            entry: Entry::Move(span),
        };
        last.end -= span;
        takes.push(closing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::{Note, Project, Role};

    /// A project of `bars` bars in 4/4 at 120.
    struct Song {
        project: Project,
    }

    impl Song {
        fn new() -> Self {
            Self { project: Project::new("song") }
        }

        /// A track that plays `per_bar` notes a bar from bar `from` to bar
        /// `to`, around `pitch`; in chords of `voices`.
        fn part(&mut self, name: &str, from: usize, to: usize, per_bar: usize, pitch: u8, voices: u8) -> TrackId {
            let track = self.project.add_track(name);
            let (start, length) = (from as f64 * 4.0, (to - from) as f64 * 4.0);
            let clip = self.project.add_midi_clip(track, name, start, length).unwrap();
            let step = 4.0 / per_bar as f64;
            let mut notes = Vec::new();
            for index in 0..(to - from) * per_bar {
                for voice in 0..voices {
                    notes.push(Note {
                        // A line that moves, so that it can be a tune.
                        pitch: pitch + (index % 5) as u8 * 2 + voice * 4,
                        start: index as f64 * step,
                        length: step * 0.9,
                        velocity: 100,
                        ..Default::default()
                    });
                }
            }
            self.project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
            track
        }

        fn drums(&mut self, from: usize, to: usize) -> TrackId {
            let track = self.part("Drums", from, to, 8, 36, 1);
            let profile = trill_core::InstrumentProfile { role: Some(Role::Drums), ..Default::default() };
            self.project.set_track_profile(track, profile).unwrap();
            track
        }

        fn marker(&mut self, bar: usize, name: &str) {
            self.project.add_marker(bar as f64 * 4.0, name).unwrap();
        }

        fn plan(&self) -> (Score, Plan) {
            let score = Score::read(&self.project);
            let plan = Plan::direct(&score);
            (score, plan)
        }
    }

    /// Two verses, a chorus twice as long, a solo and a last verse.
    fn song() -> (Song, TrackId, TrackId, TrackId) {
        let mut song = Song::new();
        let drums = song.drums(0, 48);
        song.part("Bass", 0, 48, 4, 36, 1);
        let rhythm = song.part("Rhythm", 0, 48, 8, 52, 2);
        // More in the chorus, which makes it the fullest.
        song.part("Pad", 16, 32, 2, 60, 3);
        song.part("Guitar", 16, 32, 8, 52, 2);
        let lead = song.part("Lead", 32, 40, 12, 72, 1);
        for (bar, name) in [(0, "Verse"), (8, "Verse B"), (16, "Chorus"), (32, "Solo"), (40, "Verse 2")] {
            song.marker(bar, name);
        }
        (song, drums, rhythm, lead)
    }

    fn take_from(plan: &Plan, beat: Beats) -> &Take {
        plan.takes.iter().find(|take| take.start == beat).unwrap_or_else(|| panic!("no take starts at {beat}"))
    }

    #[test]
    fn takes_cover_the_song_in_order_without_gaps() {
        let (song, ..) = song();
        let (score, plan) = song.plan();
        assert_eq!(plan.takes.first().unwrap().start, 0.0);
        assert_eq!(plan.takes.last().unwrap().end, score.length);
        for pair in plan.takes.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "{:?} then {:?}", pair[0], pair[1]);
            assert!(pair[0].end > pair[0].start);
        }
        assert_eq!(plan.take_at(-3.0), 0);
        assert_eq!(plan.take_at(1e6), plan.takes.len() - 1);
        assert_eq!(plan.takes[plan.take_at(64.0)].start, 64.0, "a take begins on its first beat");
    }

    #[test]
    fn the_song_opens_and_closes_on_the_whole_of_itself() {
        let (song, ..) = song();
        let (score, plan) = song.plan();
        let (first, last) = (plan.takes.first().unwrap(), plan.takes.last().unwrap());
        assert_eq!((first.shot.kind, last.shot.kind), (Kind::Overview, Kind::Overview));
        assert_eq!(first.shot.window.0, score.length);
        assert_eq!((first.end - first.start, last.end - last.start), (8.0, 8.0), "two bars each");
        // The camera travels out of the overview and back into it.
        assert_eq!(plan.takes[1].entry, Entry::Move(8.0));
        assert_eq!(last.entry, Entry::Move(8.0));
    }

    #[test]
    fn a_chorus_is_wide_a_solo_is_close_on_its_lead_and_a_verse_shows_who_drives_it() {
        let (song, drums, rhythm, lead) = song();
        let (_, plan) = song.plan();
        let verse = take_from(&plan, 32.0);
        assert_eq!(verse.shot.kind, Kind::Ensemble);
        assert_eq!(verse.shot.rolls, [drums, rhythm], "the drums and the busiest other part, in lane order");
        assert_eq!(verse.shot.window.0, 16.0, "four bars across");
        let chorus = take_from(&plan, 64.0);
        assert_eq!((chorus.shot.kind, chorus.shot.rolls.len(), chorus.shot.roll_share), (Kind::Wide, 0, 0.0));
        assert_eq!(chorus.shot.window.0, 32.0, "eight bars across");
        let solo = take_from(&plan, 128.0);
        assert_eq!(solo.shot.kind, Kind::Close);
        assert_eq!(solo.shot.rolls, [lead]);
        // Twelve notes a bar: about two bars of them make a frame.
        assert_eq!(solo.shot.window.0, 8.0);
        assert!(solo.shot.window.1 < solo.shot.window.0, "a take creeps in as it runs");
    }

    #[test]
    fn a_jump_in_the_song_is_cut_to_and_the_rest_is_moved_to() {
        let (song, ..) = song();
        let (_, plan) = song.plan();
        assert_eq!(take_from(&plan, 32.0).entry, Entry::Move(4.0), "from one verse to another as full");
        assert_eq!(take_from(&plan, 64.0).entry, Entry::Cut, "into the chorus: much fuller, and from rolls to lanes");
        assert_eq!(take_from(&plan, 128.0).entry, Entry::Cut, "in on the lead, which the frame did not hold");
        assert_eq!(take_from(&plan, 160.0).entry, Entry::Cut, "out of the solo to a verse much emptier");
    }

    #[test]
    fn a_long_section_is_filmed_from_two_distances() {
        let (song, ..) = song();
        let (_, plan) = song.plan();
        // The chorus is sixteen bars where the usual section is eight.
        let chorus: Vec<&Take> = plan.takes.iter().filter(|take| take.section == 2).collect();
        assert_eq!(chorus.len(), 2);
        assert_eq!((chorus[0].start, chorus[0].end, chorus[1].end), (64.0, 96.0, 128.0));
        assert_eq!((chorus[0].shot.kind, chorus[1].shot.kind), (Kind::Wide, Kind::Ensemble));
        assert_eq!(chorus[1].entry, Entry::Move(4.0));
        // The others are filmed once.
        assert_eq!(plan.takes.iter().filter(|take| take.section == 1).count(), 1);
    }

    #[test]
    fn a_direction_has_its_stretch_and_the_rest_stays_the_directors() {
        let (song, drums, rhythm, lead) = song();
        let score = Score::read(&song.project);
        let own = Plan::direct(&score);
        assert_eq!(Plan::directed(&score, &[]), own, "with nothing said the plan is the director's");

        // Hold the rhythm part close for bars 5 to 8, in the first verse.
        let close = Direction { from: 16.0, to: 32.0, kind: None, rolls: vec![rhythm], entry: None };
        let plan = Plan::directed(&score, std::slice::from_ref(&close));
        let held = take_from(&plan, 16.0);
        assert_eq!((held.end, held.shot.kind, held.shot.rolls.as_slice()), (32.0, Kind::Close, [rhythm].as_slice()));
        assert_eq!(held.entry, Entry::Cut, "in on a part: a cut");
        assert_eq!(take_from(&plan, 32.0).entry, Entry::Cut, "and out of it again");
        // Before it the verse is as it was, only shorter; what comes after is untouched.
        assert_eq!((plan.takes[1].start, plan.takes[1].end, plan.takes[1].shot.kind), (8.0, 16.0, Kind::Ensemble));
        assert!(plan.takes[1].shot.window.1 > own.takes[1].shot.window.1, "it had not crept all the way in");
        assert_eq!(take_from(&plan, 64.0), take_from(&own, 64.0));
        assert_eq!(plan.takes.last(), own.takes.last());
        for pair in plan.takes.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "still without gaps");
        }

        // A kind alone: the chorus filmed as an ensemble of who drives it.
        let ensemble = Direction { from: 64.0, to: 128.0, kind: Some(Kind::Ensemble), rolls: Vec::new(), entry: None };
        let plan = Plan::directed(&score, &[ensemble]);
        let chorus = take_from(&plan, 64.0);
        assert_eq!((chorus.end, chorus.shot.kind), (128.0, Kind::Ensemble));
        assert!(chorus.shot.rolls.contains(&drums) && chorus.shot.rolls.len() == 2, "{:?}", chorus.shot.rolls);
        assert_eq!(plan.takes.iter().filter(|take| take.section == 2).count(), 1, "one take where there were two");

        // Close with no part named goes in on the tune of the stretch, as
        // the director would; a part the song does not have is not shown.
        let somewhere =
            Direction { from: 128.0, to: 160.0, kind: Some(Kind::Close), rolls: vec![TrackId(999)], entry: None };
        assert_eq!(take_from(&Plan::directed(&score, &[somewhere]), 128.0).shot.rolls, [lead]);

        // The later of two directions has the stretch they share.
        let wide = Direction { from: 24.0, to: 40.0, kind: Some(Kind::Wide), rolls: Vec::new(), entry: None };
        let plan = Plan::directed(&score, &[close, wide]);
        let kinds: Vec<(Beats, Kind)> = plan.takes.iter().map(|take| (take.start, take.shot.kind)).collect();
        assert_eq!(
            &kinds[1..5],
            [(8.0, Kind::Ensemble), (16.0, Kind::Close), (24.0, Kind::Wide), (40.0, Kind::Ensemble)]
        );
    }

    #[test]
    fn a_cut_or_a_move_can_be_asked_for_at_any_bar() {
        let (song, ..) = song();
        let score = Score::read(&song.project);
        // A cut in the middle of the solo, and a slow move into the chorus
        // where the director cuts.
        let cut = Direction { from: 144.0, to: 160.0, kind: None, rolls: Vec::new(), entry: Some(Entry::Cut) };
        let slow = Direction { from: 64.0, to: 96.0, kind: None, rolls: Vec::new(), entry: Some(Entry::Move(8.0)) };
        let plan = Plan::directed(&score, &[cut, slow]);
        let (first, second) = (take_from(&plan, 128.0), take_from(&plan, 144.0));
        assert_eq!((first.end, second.end, second.entry), (144.0, 160.0, Entry::Cut));
        assert_eq!((first.shot.kind, &first.shot.rolls), (second.shot.kind, &second.shot.rolls), "the same shot");
        assert_eq!(first.shot.window.1, second.shot.window.0, "taken up where the first half had got to");
        assert_eq!(take_from(&plan, 64.0).entry, Entry::Move(8.0));
        // A stretch outside the song, or with no length, asks for nothing.
        let nothing = Direction { from: 900.0, to: 950.0, kind: Some(Kind::Wide), rolls: Vec::new(), entry: None };
        let backwards = Direction { from: 40.0, to: 20.0, kind: Some(Kind::Wide), rolls: Vec::new(), entry: None };
        assert_eq!(Plan::directed(&score, &[nothing, backwards]), Plan::direct(&score));
    }

    #[test]
    fn the_same_song_is_directed_the_same_way_twice() {
        let (song, ..) = song();
        let (_, first) = song.plan();
        let (_, second) = song.plan();
        assert_eq!(first, second);
        let kinds: Vec<Kind> = first.takes.iter().map(|take| take.shot.kind).collect();
        use Kind::{Close, Ensemble, Overview, Wide};
        assert_eq!(kinds, [Overview, Ensemble, Ensemble, Wide, Ensemble, Close, Ensemble, Overview]);
    }

    #[test]
    fn where_little_plays_the_shot_goes_in_on_what_is_left() {
        let mut song = Song::new();
        song.drums(0, 8);
        song.part("Bass", 0, 8, 4, 36, 1);
        song.part("Rhythm", 0, 8, 8, 52, 2);
        song.part("Guitar", 0, 8, 8, 52, 2);
        // A break of piano alone, then everyone again.
        let piano = song.part("Piano", 8, 16, 8, 60, 2);
        song.part("Rhythm 2", 16, 24, 8, 52, 2);
        song.part("Guitar 2", 16, 24, 8, 52, 2);
        song.part("Bass 2", 16, 24, 4, 36, 1);
        for (bar, name) in [(0, "A"), (8, "B"), (16, "C")] {
            song.marker(bar, name);
        }
        let (_, plan) = song.plan();
        let quiet = take_from(&plan, 32.0);
        assert_eq!((quiet.shot.kind, quiet.shot.rolls.as_slice()), (Kind::Close, [piano].as_slice()));
        assert!(quiet.shot.window.0 >= 8.0, "never closer than two bars where the song is at rest");
    }

    #[test]
    fn a_song_of_one_long_note_is_still_filmed() {
        let mut project = Project::new("drone");
        let track = project.add_track("Pad");
        let clip = project.add_midi_clip(track, "pad", 0.0, 64.0).unwrap();
        project
            .clip_mut(clip)
            .unwrap()
            .add_notes(&[Note { pitch: 48, start: 0.0, length: 64.0, velocity: 80, ..Default::default() }])
            .unwrap();
        let score = Score::read(&project);
        let plan = Plan::direct(&score);
        assert!(!plan.takes.is_empty());
        assert!(plan.takes.iter().all(|take| take.shot.rolls.is_empty()), "one note is not worth a piano roll");
        // And an empty project has one take of nothing.
        let empty = Plan::direct(&Score::read(&Project::new("empty")));
        assert_eq!(empty.takes.len(), 1);
    }

    #[test]
    fn activity_counts_what_starts_in_a_stretch() {
        let mut song = Song::new();
        let track = song.part("Chords", 0, 4, 2, 60, 3);
        let (score, _) = song.plan();
        let part = score.part(track).unwrap();
        let found = activity(part, 0.0, 8.0);
        assert_eq!(found.notes, 12, "two bars of two chords of three");
        assert!((found.rate - 1.5).abs() < 1e-9);
        assert!((found.voices - 3.0).abs() < 1e-9, "three notes at a time: {}", found.voices);
        assert_eq!(activity(part, 100.0, 108.0), Activity::default());
    }
}
