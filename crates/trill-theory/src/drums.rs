//! Drum patterns: one step-grid lane per drum.
//!
//! ```text
//! kick:       X...X...X...X...
//! clap:       ....X.......X...
//! closed_hat: xg.gxg.gxg.gxg.g
//! open_hat:   ..x...x...x...x.
//! ```
//!
//! Lanes are written in [step notation](crate::steps) and may differ in
//! resolution and length; shorter lanes repeat under longer ones.
//!
//! A drum is asked for by name, and which key plays it is the kit's
//! business: the General MIDI map unless the track's instrument has its own
//! ([`InstrumentProfile::drum`]), so that a china or a ride bell lands on
//! the note this kit uses.

use crate::error::{Error, Result, list};
use crate::expression::{Level, MIN_LENGTH};
use crate::steps::StepPattern;
use crate::timing::EPSILON;
use trill_core::pitch::{DRUM_NAMES, DRUM_STAND_INS};
use trill_core::{InstrumentProfile, Note};

/// One drum and its rhythm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The name the drum is listed under, e.g. `"closed_hat"`.
    pub drum: String,
    /// The key that plays it.
    pub pitch: u8,
    pub pattern: StepPattern,
}

/// A set of lanes that play together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrumPattern {
    pub lanes: Vec<Lane>,
}

/// How hard each drum is normally hit. A kit played at one velocity sounds
/// like a machine; hats and shakers sit well under the kick and snare. The
/// drum's name decides, since a kit may keep it on any key.
fn usual_velocity(drum: &str) -> f64 {
    let has = |word: &str| drum.contains(word);
    if has("kick") {
        112.0
    } else if has("snare") || has("rimshot") {
        106.0
    } else if has("rim") || has("stick") {
        88.0
    } else if has("pedal") {
        70.0
    } else if has("hat") && has("open") {
        86.0
    } else if has("hat") {
        78.0
    } else if has("ride") || has("bell") {
        80.0
    } else if has("tom") || has("clap") || has("crash") || has("china") || has("splash") || has("cymbal") {
        100.0
    } else if has("tambourine") {
        72.0
    } else if has("shaker") || has("maracas") || has("cabasa") {
        62.0
    } else {
        84.0
    }
}

/// The drum names of General MIDI, one per drum, followed by the names it
/// has a stand-in for (the degrees of an open hi-hat, a hat's edge).
pub fn drum_names() -> Vec<&'static str> {
    let mut seen: Vec<u8> = Vec::new();
    let mut names = Vec::new();
    for (name, pitch) in DRUM_NAMES {
        if !seen.contains(pitch) {
            seen.push(*pitch);
            names.push(*name);
        }
    }
    names.extend(DRUM_STAND_INS.iter().map(|(name, _)| *name));
    names
}

impl DrumPattern {
    /// Parses lanes given as `(drum, steps)` pairs, on the General MIDI
    /// map.
    pub fn parse<'a>(lanes: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut pattern = Self::default();
        for (drum, steps) in lanes {
            pattern.set_lane(drum, steps)?;
        }
        Ok(pattern)
    }

    /// Adds a lane on the General MIDI map; see
    /// [`set_lane_on`](Self::set_lane_on).
    pub fn set_lane(&mut self, drum: &str, steps: &str) -> Result<()> {
        self.set_lane_on(&InstrumentProfile::default(), drum, steps).map(|_| ())
    }

    /// Adds a lane, or replaces the one already there for that drum, on
    /// the keys of the kit `profile` describes. A lane of nothing but rests
    /// removes the drum, which is how a groove's part is silenced. Returns
    /// the drum that stands in, when the kit has no key for the one asked
    /// for (an open hi-hat for a half-open one).
    pub fn set_lane_on(
        &mut self,
        profile: &InstrumentProfile,
        drum: &str,
        steps: &str,
    ) -> Result<Option<&'static str>> {
        let key = profile.drum(drum).ok_or_else(|| {
            let known: Vec<String> = profile.drum_map().into_iter().map(|(name, _)| name).collect();
            Error::new(format!("'{drum}' is not a drum; expected one of {}", list(known)))
        })?;
        let pattern = StepPattern::parse(steps).map_err(|error| error.within(format!("lane '{}'", key.name)))?;
        self.lanes.retain(|lane| lane.pitch != key.pitch);
        if !pattern.is_silent() {
            self.lanes.push(Lane { drum: key.name, pitch: key.pitch, pattern });
        }
        Ok(key.stand_in)
    }

    /// The pattern on the keys of the kit `profile` describes: each lane
    /// keeps its drum and moves to where that kit has it. A pattern written
    /// on the General MIDI map (a groove of the library) is played on
    /// another kit this way.
    pub fn on_kit(&self, profile: &InstrumentProfile) -> Self {
        let mut pattern = Self::default();
        for lane in &self.lanes {
            let (drum, pitch) = match profile.drum(&lane.drum) {
                Some(key) => (key.name, key.pitch),
                None => (lane.drum.clone(), lane.pitch),
            };
            pattern.lanes.retain(|other| other.pitch != pitch);
            pattern.lanes.push(Lane { drum, pitch, pattern: lane.pattern.clone() });
        }
        pattern
    }

    /// The pattern, written for bars of `own` beats, as it is played in
    /// bars of `bar` beats with its steps as long as they were: see
    /// [`StepPattern::refit`]. `None` if a lane has steps that a bar of
    /// `bar` beats is not a whole number of.
    pub fn refit(&self, own: f64, bar: f64, from_end: bool) -> Option<Self> {
        let lane = |lane: &Lane| Some(Lane { pattern: lane.pattern.refit(own, bar, from_end)?, ..lane.clone() });
        let lanes: Option<Vec<Lane>> = self.lanes.iter().map(lane).collect();
        Some(Self { lanes: lanes?.into_iter().filter(|lane| !lane.pattern.is_silent()).collect() })
    }

    /// Length in bars: that of the longest lane.
    pub fn bars(&self) -> usize {
        self.lanes.iter().map(|lane| lane.pattern.bars()).max().unwrap_or(0)
    }

    /// The lanes as text, for showing what was read.
    pub fn grid(&self) -> Vec<(&str, String)> {
        self.lanes.iter().map(|lane| (lane.drum.as_str(), lane.pattern.to_string())).collect()
    }

    /// Lanes whose step count does not divide the bar into ordinary note
    /// values, which usually means a character too many or too few.
    pub fn odd_lanes(&self, bar_length: f64) -> Vec<String> {
        let mut odd = Vec::new();
        for lane in &self.lanes {
            for (bar, steps) in lane.pattern.steps_per_bar().into_iter().enumerate() {
                // An ordinary step is a whole note divided by a power of two, or by three times one.
                let per_whole = steps as f64 * 4.0 / bar_length;
                let ordinary = [1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0, 96.0]
                    .iter()
                    .any(|division| (per_whole - division).abs() < 1e-6);
                if !ordinary {
                    odd.push(format!(
                        "lane '{}' bar {} has {steps} steps, which is not an even division of a {bar_length}-beat \
                         bar; check for a missing or extra character",
                        lane.drum,
                        bar + 1
                    ));
                }
            }
        }
        odd
    }

    /// The pattern played for `bars` bars. `level` scales every velocity
    /// (1 is the usual balance).
    pub fn notes(&self, bars: usize, bar_length: f64, level: f64) -> Vec<Note> {
        let length = bars as f64 * bar_length;
        let mut notes = Vec::new();
        for lane in &self.lanes {
            let base = usual_velocity(&lane.drum) * level;
            for hit in lane.pattern.hits_for(bar_length, length) {
                notes.push(Note {
                    pitch: lane.pitch,
                    start: hit.start,
                    // Drums are one-shots; the length only has to be sane.
                    length: (hit.length * 0.9).clamp(MIN_LENGTH, 0.5),
                    velocity: hit.level.velocity(base),
                });
            }
        }
        sort(&mut notes);
        notes
    }
}

fn sort(notes: &mut [Note]) {
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
}

/// A fill laid over a groove.
#[derive(Clone, Debug, PartialEq)]
pub struct FillPlan<'a> {
    /// One bar of fill.
    pub pattern: &'a DrumPattern,
    /// Where in the bar the fill takes over from the groove, as a fraction
    /// of the bar (0.5 = the second half).
    pub from: f64,
    /// The fill replaces the end of every this-many bars (4 or 8 are
    /// usual). 0 means only the last bar.
    pub every: usize,
    /// The key of the crash that the bar after a fill starts with.
    pub crash: u8,
}

/// What a fill did to one bar.
#[derive(Clone, Debug, PartialEq)]
pub struct FillBar {
    /// The bar, counted from 1.
    pub bar: usize,
    /// How many beats into the bar the fill takes over.
    pub from: f64,
    /// The hits of the groove that the fill took the place of.
    pub replaced: Vec<Note>,
    /// The hits of the fill.
    pub played: Vec<Note>,
    /// Whether a crash was added on the downbeat after it.
    pub crash: bool,
}

/// A groove played for some bars, with its fills.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arranged {
    pub notes: Vec<Note>,
    /// One entry for every bar a fill was played in, in order.
    pub fills: Vec<FillBar>,
}

/// Plays `groove` for `bars` bars, with an optional fill.
///
/// In a fill bar the groove stops where the fill takes over. A fill leads
/// somewhere, so the bar after it starts with a crash when that bar is part
/// of what is being written.
pub fn arrange(groove: &DrumPattern, fill: Option<&FillPlan>, bars: usize, bar_length: f64, level: f64) -> Arranged {
    let mut notes = groove.notes(bars, bar_length, level);
    let Some(fill) = fill else { return Arranged { notes, fills: Vec::new() } };
    let fill_bars: Vec<usize> = match fill.every {
        0 => bars.checked_sub(1).into_iter().collect(),
        every => (0..bars).filter(|bar| (bar + 1) % every == 0).collect(),
    };
    let mut fills = Vec::new();
    for bar in fill_bars {
        let start = bar as f64 * bar_length;
        let (from, end) = (start + fill.from.clamp(0.0, 1.0) * bar_length, start + bar_length);
        let inside = |note: &Note| note.start >= from - EPSILON && note.start < end - EPSILON;
        let replaced: Vec<Note> = notes.iter().filter(|note| inside(note)).copied().collect();
        notes.retain(|note| !inside(note));
        let played: Vec<Note> = fill
            .pattern
            .notes(1, bar_length, level)
            .into_iter()
            .filter(|note| note.start >= from - start - EPSILON)
            .map(|note| Note { start: note.start + start, ..note })
            .collect();
        notes.extend(played.iter().copied());
        let crashed = notes.iter().any(|note| note.pitch == fill.crash && (note.start - end).abs() < EPSILON);
        let crash = bar + 1 < bars && !crashed;
        if crash {
            let velocity = Level::Normal.velocity(usual_velocity("crash") * level);
            notes.push(Note { pitch: fill.crash, start: end, length: 0.5, velocity });
        }
        fills.push(FillBar { bar: bar + 1, from: from - start, replaced, played, crash });
    }
    sort(&mut notes);
    Arranged { notes, fills }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::Swing;

    const KICK: u8 = 36;
    const SNARE: u8 = 38;
    const HAT: u8 = 42;

    fn starts(notes: &[Note], pitch: u8) -> Vec<f64> {
        notes.iter().filter(|note| note.pitch == pitch).map(|note| note.start).collect()
    }

    fn basic() -> DrumPattern {
        DrumPattern::parse([("kick", "x...x...x...x..."), ("snare", "....x.......x..."), ("hat", "x.x.x.x.x.x.x.x.")])
            .unwrap()
    }

    #[test]
    fn lanes_place_hits_on_the_grid() {
        let notes = basic().notes(1, 4.0, 1.0);
        assert_eq!(starts(&notes, KICK), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts(&notes, SNARE), [1.0, 3.0]);
        assert_eq!(starts(&notes, HAT), [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
        assert!(notes.windows(2).all(|pair| pair[0].start <= pair[1].start), "sorted by time");
        assert_eq!(basic().bars(), 1);
        assert_eq!(
            basic().grid()[2],
            ("closed_hat", "x.x.x.x.x.x.x.x.".to_string()),
            "aliases get the drum's real name"
        );
    }

    #[test]
    fn lanes_keep_their_own_resolution_and_length() {
        let pattern = DrumPattern::parse([
            ("kick", "x...x...x...x...|x...x...x..xx..."),
            ("ride", "x.xx.xx.xx.x"),
            ("snare", "..x."),
        ])
        .unwrap();
        assert_eq!(pattern.bars(), 2);
        let notes = pattern.notes(4, 4.0, 1.0);
        assert_eq!(starts(&notes, KICK).len(), 4 + 5 + 4 + 5, "the two-bar lane plays twice");
        assert_eq!(starts(&notes, 51).len(), 32, "the one-bar ride repeats under it");
        assert!((starts(&notes, 51)[1] - 2.0 / 3.0).abs() < 1e-9, "twelve steps are triplets");
        assert_eq!(starts(&notes, SNARE), [2.0, 6.0, 10.0, 14.0]);
        // A bar of 6/8 is three beats.
        let six = DrumPattern::parse([("kick", "x..x.."), ("hat", "xxxxxx")]).unwrap().notes(2, 3.0, 1.0);
        assert_eq!(starts(&six, KICK), [0.0, 1.5, 3.0, 4.5]);
        assert_eq!(starts(&six, HAT).len(), 12);
    }

    #[test]
    fn velocities_follow_the_drum_and_the_level() {
        let pattern =
            DrumPattern::parse([("kick", "x.X."), ("snare", "g.x."), ("closed_hat", "xXgx"), ("shaker", "x...")])
                .unwrap();
        let notes = pattern.notes(1, 4.0, 1.0);
        let velocity =
            |pitch: u8, start: f64| notes.iter().find(|n| n.pitch == pitch && n.start == start).unwrap().velocity;
        assert_eq!((velocity(KICK, 0.0), velocity(KICK, 2.0)), (112, 127));
        assert_eq!((velocity(SNARE, 0.0), velocity(SNARE, 2.0)), (45, 106), "a ghost note is far quieter");
        assert_eq!((velocity(HAT, 0.0), velocity(HAT, 1.0), velocity(HAT, 2.0)), (78, 92, 33));
        assert!(velocity(70, 0.0) < velocity(HAT, 0.0), "shakers sit under the hats");
        let quiet = pattern.notes(1, 4.0, 0.5);
        assert_eq!(quiet.iter().find(|note| note.pitch == KICK).unwrap().velocity, 56);
        assert!(notes.iter().all(|note| note.length > 0.0 && note.length <= 0.5));
    }

    #[test]
    fn rolls_and_swing() {
        let hats = DrumPattern::parse([("hat", "x.x.x.x.x.x.x3_x.")]).unwrap().notes(1, 4.0, 1.0);
        assert_eq!(hats.len(), 10);
        assert!((hats[7].start - (3.0 + 1.0 / 6.0)).abs() < 1e-9);

        // Sixteenth swing delays the "e" and "a" of every beat and nothing else.
        let mut sixteenths = DrumPattern::parse([("hat", "xxxxxxxxxxxxxxxx")]).unwrap().notes(1, 4.0, 1.0);
        Swing::new(62.0, 0.25).unwrap().apply(&mut sixteenths, 0.0);
        for (step, note) in sixteenths.iter().enumerate() {
            let straight = step as f64 * 0.25;
            let expected = if step % 2 == 1 { straight - 0.25 + 0.5 * 0.62 } else { straight };
            assert!((note.start - expected).abs() < 1e-9, "step {step}: {} vs {expected}", note.start);
        }
        // Eighth swing leaves the sixteenths between in order.
        let mut shuffled = DrumPattern::parse([("hat", "xxxxxxxxxxxxxxxx")]).unwrap().notes(1, 4.0, 1.0);
        Swing::new(66.7, 0.5).unwrap().apply(&mut shuffled, 0.0);
        assert!((shuffled[2].start - 0.667).abs() < 1e-9);
        assert!(shuffled.windows(2).all(|pair| pair[0].start < pair[1].start));
    }

    #[test]
    fn lanes_can_be_replaced_and_silenced() {
        let mut pattern = basic();
        pattern.set_lane("kick", "x.......x.......").unwrap();
        pattern.set_lane("closed_hat", "................").unwrap();
        pattern.set_lane("clap", "....x.......x...").unwrap();
        let drums: Vec<&str> = pattern.lanes.iter().map(|lane| lane.drum.as_str()).collect();
        assert_eq!(drums, ["snare", "kick", "clap"]);
        assert_eq!(starts(&pattern.notes(1, 4.0, 1.0), KICK), [0.0, 2.0]);
    }

    #[test]
    fn mistakes_name_the_lane() {
        let error = DrumPattern::parse([("gong", "x...")]).unwrap_err();
        assert!(error.message().contains("'gong' is not a drum") && error.message().contains("closed_hat"), "{error}");
        let error = DrumPattern::parse([("kick", "x..."), ("snare", "..y.")]).unwrap_err();
        assert!(error.message().starts_with("lane 'snare': '..y.': 'y' is not a step"), "{error}");
        let names = drum_names();
        assert!(names.contains(&"kick") && names.contains(&"ride") && !names.contains(&"bd"), "{names:?}");

        let odd = DrumPattern::parse([("kick", "x...x...x...x.."), ("hat", "x.x.x.x.")]).unwrap().odd_lanes(4.0);
        assert_eq!(odd.len(), 1);
        assert!(odd[0].contains("lane 'kick' bar 1 has 15 steps"), "{odd:?}");
        for fine in ["x...x...x...x...", "xxxxxxxxxxxx", "x.x.", "xxxxxxxxxxxxxxxxxxxxxxxx", "x"] {
            assert!(DrumPattern::parse([("kick", fine)]).unwrap().odd_lanes(4.0).is_empty(), "{fine}");
        }
        assert!(
            DrumPattern::parse([("kick", "x.x.x.."), ("hat", "xxxxxxxxxxxxxx")]).unwrap().odd_lanes(3.5).is_empty()
        );
        assert!(DrumPattern::parse([("kick", "x..x..")]).unwrap().odd_lanes(3.0).is_empty());
    }

    const CRASH: u8 = 49;

    #[test]
    fn fills_take_over_the_end_of_a_bar() {
        let groove = basic();
        let fill = DrumPattern::parse([("snare", "........xxxxxxxx"), ("kick", "x.......x.......")]).unwrap();
        let plan = FillPlan { pattern: &fill, from: 0.5, every: 0, crash: CRASH };
        let arranged = arrange(&groove, Some(&plan), 4, 4.0, 1.0);
        let notes = &arranged.notes;
        // Three bars of groove, then half a bar of groove and the fill.
        assert_eq!(starts(notes, HAT).len(), 3 * 8 + 4);
        assert_eq!(starts(notes, SNARE).len(), 3 * 2 + 1 + 8);
        assert_eq!(
            starts(notes, KICK).iter().filter(|start| **start >= 12.0).copied().collect::<Vec<_>>(),
            [12.0, 13.0, 14.0]
        );
        assert!(starts(notes, CRASH).is_empty(), "nothing follows the last bar to crash on");

        // Every two bars: fills in bars 2 and 4, and a crash where bar 3 begins.
        let plan = FillPlan { every: 2, ..plan };
        let arranged = arrange(&groove, Some(&plan), 4, 4.0, 1.0);
        assert_eq!(starts(&arranged.notes, SNARE).len(), 2 * 2 + 2 * (1 + 8));
        assert_eq!(starts(&arranged.notes, CRASH), [8.0]);
        // A whole-bar fill silences the groove for that bar.
        let plan = FillPlan { from: 0.0, every: 0, ..plan };
        let arranged = arrange(&groove, Some(&plan), 2, 4.0, 1.0);
        assert_eq!(starts(&arranged.notes, HAT).len(), 8);
        let plain = arrange(&groove, None, 2, 4.0, 1.0);
        assert_eq!((plain.notes, plain.fills), (groove.notes(2, 4.0, 1.0), vec![]));
        assert!(arrange(&groove, Some(&plan), 0, 4.0, 1.0).notes.is_empty());
    }

    #[test]
    fn a_fill_says_what_it_took_the_place_of() {
        let groove = basic();
        let fill = DrumPattern::parse([("snare", "........xxxxxxxx"), ("kick", "x.......x.......")]).unwrap();
        let plan = FillPlan { pattern: &fill, from: 0.5, every: 2, crash: CRASH };
        let arranged = arrange(&groove, Some(&plan), 4, 4.0, 1.0);
        let bars: Vec<(usize, f64, bool)> =
            arranged.fills.iter().map(|fill| (fill.bar, fill.from, fill.crash)).collect();
        assert_eq!(bars, [(2, 2.0, true), (4, 2.0, false)], "bar 4 is the last: nothing follows to crash on");
        let first = &arranged.fills[0];
        // The second half of the groove's bar: two kicks, a snare and four hats.
        let count = |notes: &[Note], pitch: u8| notes.iter().filter(|note| note.pitch == pitch).count();
        assert_eq!(
            (count(&first.replaced, KICK), count(&first.replaced, SNARE), count(&first.replaced, HAT)),
            (2, 1, 4)
        );
        assert_eq!((count(&first.played, KICK), count(&first.played, SNARE), count(&first.played, HAT)), (1, 8, 0));
        assert!(first.replaced.iter().chain(&first.played).all(|note| (6.0..8.0).contains(&note.start)));
        // The hats that were replaced are gone from the notes, and what the fill played is in them.
        assert!(first.replaced.iter().filter(|note| note.pitch == HAT).all(|gone| !arranged.notes.contains(gone)));
        assert!(first.played.iter().all(|played| arranged.notes.contains(played)));
    }

    /// A kit that keeps some of its drums off the General MIDI keys.
    fn kit() -> InstrumentProfile {
        InstrumentProfile {
            sounds: [("high_tom", 48), ("wide_open_hat", 26), ("closed_hat_edge", 22), ("crash", 86)]
                .map(|(name, key)| (name.to_string(), key))
                .into(),
            ..InstrumentProfile::default()
        }
    }

    #[test]
    fn lanes_land_on_the_keys_of_the_kit_they_are_written_for() {
        let kit = kit();
        let mut pattern = DrumPattern::default();
        for (drum, steps) in [("kick", "x..."), ("tom_high", ".x.."), ("wide_open_hat", "..x."), ("china", "...x")] {
            assert_eq!(pattern.set_lane_on(&kit, drum, steps).unwrap(), None, "{drum}");
        }
        let keys: Vec<(&str, u8)> = pattern.lanes.iter().map(|lane| (lane.drum.as_str(), lane.pitch)).collect();
        assert_eq!(keys, [("kick", 36), ("high_tom", 48), ("wide_open_hat", 26), ("china", 52)]);
        // A drum the kit has no key for is played by the nearest it has, and says so.
        assert_eq!(pattern.set_lane_on(&kit, "half_open_hat", "x.x."), Ok(Some("open_hat")));
        assert_eq!(pattern.lanes.last().map(|lane| (lane.drum.as_str(), lane.pitch)), Some(("open_hat", 46)));
        assert_eq!(pattern.set_lane_on(&InstrumentProfile::default(), "wide_open_hat", "x..."), Ok(Some("open_hat")));
        let error = pattern.set_lane_on(&kit, "gong", "x...").unwrap_err();
        assert!(error.message().starts_with("'gong' is not a drum; expected one of closed_hat_edge, crash, high_tom"));

        // A pattern written on the General MIDI map moves to the kit's keys, drum by drum.
        let general = DrumPattern::parse([("kick", "x..."), ("high_tom", ".x.."), ("crash", "x..."), ("ride", "..x.")]);
        let moved = general.unwrap().on_kit(&kit);
        let keys: Vec<(&str, u8)> = moved.lanes.iter().map(|lane| (lane.drum.as_str(), lane.pitch)).collect();
        assert_eq!(keys, [("kick", 36), ("high_tom", 48), ("crash", 86), ("ride", 51)]);
    }

    #[test]
    fn a_drum_is_hit_as_hard_as_its_name_says_wherever_the_kit_keeps_it() {
        let kit = kit();
        let mut pattern = DrumPattern::default();
        for drum in [
            "kick",
            "snare",
            "closed_hat",
            "closed_hat_edge",
            "wide_open_hat",
            "pedal_hat",
            "ride_bell",
            "crash",
            "china",
            "shaker",
        ] {
            pattern.set_lane_on(&kit, drum, "x...").unwrap();
        }
        let notes = pattern.notes(1, 4.0, 1.0);
        let velocity = |pitch: u8| notes.iter().find(|note| note.pitch == pitch).unwrap().velocity;
        assert_eq!((velocity(36), velocity(38)), (112, 106));
        assert_eq!((velocity(42), velocity(22)), (78, 78), "the edge of the hat is a hat, on whatever key");
        assert_eq!((velocity(26), velocity(44)), (86, 70));
        assert_eq!((velocity(53), velocity(86), velocity(52), velocity(70)), (80, 100, 100, 62));
        let names = drum_names();
        assert!(
            names.contains(&"china") && names.contains(&"ride_bell") && names.contains(&"half_open_hat"),
            "{names:?}"
        );
    }
}
