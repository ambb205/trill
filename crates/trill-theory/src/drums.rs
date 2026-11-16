//! Drum patterns: one step-grid lane per drum, on the General MIDI drum map
//! the built-in kit plays.
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

use crate::error::{Error, Result, list};
use crate::expression::{Level, MIN_LENGTH};
use crate::steps::StepPattern;
use crate::timing::EPSILON;
use trill_core::Note;
use trill_core::pitch::{DRUM_NAMES, drum_name, parse_drum};

/// One drum and its rhythm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The drum's canonical name, e.g. `"closed_hat"`.
    pub drum: &'static str,
    /// Its General MIDI note.
    pub pitch: u8,
    pub pattern: StepPattern,
}

/// A set of lanes that play together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrumPattern {
    pub lanes: Vec<Lane>,
}

const CRASH: u8 = 49;

/// How hard each drum is normally hit. A kit played at one velocity sounds
/// like a machine; hats and shakers sit well under the kick and snare.
fn usual_velocity(pitch: u8) -> f64 {
    match pitch {
        35 | 36 => 112.0,
        38 | 40 => 106.0,
        39 => 100.0,
        37 => 88.0,
        42 => 78.0,
        44 => 70.0,
        46 => 86.0,
        41 | 43 | 45 | 47 | 48 | 50 => 100.0,
        49 | 52 | 55 | 57 => 100.0,
        51 | 53 | 59 => 80.0,
        54 => 72.0,
        69 | 70 | 82 => 62.0,
        _ => 84.0,
    }
}

/// The drum names lanes can use, one per drum.
pub fn drum_names() -> Vec<&'static str> {
    let mut seen: Vec<u8> = Vec::new();
    let mut names = Vec::new();
    for (name, pitch) in DRUM_NAMES {
        if !seen.contains(pitch) {
            seen.push(*pitch);
            names.push(*name);
        }
    }
    names
}

impl DrumPattern {
    /// Parses lanes given as `(drum, steps)` pairs.
    pub fn parse<'a>(lanes: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut pattern = Self::default();
        for (drum, steps) in lanes {
            pattern.set_lane(drum, steps)?;
        }
        Ok(pattern)
    }

    /// Adds a lane, or replaces the one already there for that drum. A lane
    /// of nothing but rests removes the drum, which is how a groove's part
    /// is silenced.
    pub fn set_lane(&mut self, drum: &str, steps: &str) -> Result<()> {
        let pitch = parse_drum(drum)
            .ok_or_else(|| Error::new(format!("'{drum}' is not a drum; expected one of {}", list(drum_names()))))?;
        let name = drum_name(pitch).unwrap_or("drum");
        let pattern = StepPattern::parse(steps).map_err(|error| error.within(format!("lane '{name}'")))?;
        self.lanes.retain(|lane| lane.pitch != pitch);
        if !pattern.is_silent() {
            self.lanes.push(Lane { drum: name, pitch, pattern });
        }
        Ok(())
    }

    /// Length in bars: that of the longest lane.
    pub fn bars(&self) -> usize {
        self.lanes.iter().map(|lane| lane.pattern.bars()).max().unwrap_or(0)
    }

    /// The lanes as text, for showing what was read.
    pub fn grid(&self) -> Vec<(&'static str, String)> {
        self.lanes.iter().map(|lane| (lane.drum, lane.pattern.to_string())).collect()
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
            let base = usual_velocity(lane.pitch) * level;
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
}

/// Plays `groove` for `bars` bars, with an optional fill.
///
/// In a fill bar the groove stops where the fill takes over. A fill leads
/// somewhere, so the bar after it starts with a crash when that bar is part
/// of what is being written.
pub fn arrange(groove: &DrumPattern, fill: Option<&FillPlan>, bars: usize, bar_length: f64, level: f64) -> Vec<Note> {
    let mut notes = groove.notes(bars, bar_length, level);
    let Some(fill) = fill else { return notes };
    let fill_bars: Vec<usize> = match fill.every {
        0 => bars.checked_sub(1).into_iter().collect(),
        every => (0..bars).filter(|bar| (bar + 1) % every == 0).collect(),
    };
    for bar in fill_bars {
        let start = bar as f64 * bar_length;
        let (from, end) = (start + fill.from.clamp(0.0, 1.0) * bar_length, start + bar_length);
        notes.retain(|note| note.start < from - EPSILON || note.start >= end - EPSILON);
        notes.extend(
            fill.pattern
                .notes(1, bar_length, level)
                .into_iter()
                .filter(|note| note.start >= from - start - EPSILON)
                .map(|note| Note { start: note.start + start, ..note }),
        );
        let crashed = notes.iter().any(|note| note.pitch == CRASH && (note.start - end).abs() < EPSILON);
        if bar + 1 < bars && !crashed {
            let velocity = Level::Normal.velocity(usual_velocity(CRASH) * level);
            notes.push(Note { pitch: CRASH, start: end, length: 0.5, velocity });
        }
    }
    sort(&mut notes);
    notes
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
        let drums: Vec<&str> = pattern.lanes.iter().map(|lane| lane.drum).collect();
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

    #[test]
    fn fills_take_over_the_end_of_a_bar() {
        let groove = basic();
        let fill = DrumPattern::parse([("snare", "........xxxxxxxx"), ("kick", "x.......x.......")]).unwrap();
        let plan = FillPlan { pattern: &fill, from: 0.5, every: 0 };
        let notes = arrange(&groove, Some(&plan), 4, 4.0, 1.0);
        // Three bars of groove, then half a bar of groove and the fill.
        assert_eq!(starts(&notes, HAT).len(), 3 * 8 + 4);
        assert_eq!(starts(&notes, SNARE).len(), 3 * 2 + 1 + 8);
        assert_eq!(
            starts(&notes, KICK).iter().filter(|start| **start >= 12.0).copied().collect::<Vec<_>>(),
            [12.0, 13.0, 14.0]
        );
        assert!(starts(&notes, CRASH).is_empty(), "nothing follows the last bar to crash on");

        // Every two bars: fills in bars 2 and 4, and a crash where bar 3 begins.
        let plan = FillPlan { every: 2, ..plan };
        let notes = arrange(&groove, Some(&plan), 4, 4.0, 1.0);
        assert_eq!(starts(&notes, SNARE).len(), 2 * 2 + 2 * (1 + 8));
        assert_eq!(starts(&notes, CRASH), [8.0]);
        // A whole-bar fill silences the groove for that bar.
        let plan = FillPlan { from: 0.0, every: 0, ..plan };
        let notes = arrange(&groove, Some(&plan), 2, 4.0, 1.0);
        assert_eq!(starts(&notes, HAT).len(), 8);
        assert_eq!(arrange(&groove, None, 2, 4.0, 1.0), groove.notes(2, 4.0, 1.0));
        assert!(arrange(&groove, Some(&plan), 0, 4.0, 1.0).is_empty());
    }
}
