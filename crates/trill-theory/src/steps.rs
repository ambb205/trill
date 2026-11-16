//! Step-grid notation: a rhythm written one character per step, the way
//! drum machines show it.
//!
//! ```text
//! x...x...x...x...      four on the floor: 16 steps, so each is a sixteenth
//! X.x.X.x.              accents on the beat: 8 steps, so each is an eighth
//! x.xx.xx.xx.x          12 steps: triplet eighths, a shuffle
//! x.x.x.x.x.x.x3_x.     a triplet roll across two steps
//! x__x__x_              three held notes: 3 + 3 + 2
//! x...x...|x...x.x.     two bars
//! ```
//!
//! | Character      | Meaning                                              |
//! |----------------|------------------------------------------------------|
//! | `x`            | a hit                                                |
//! | `X`            | an accented hit                                      |
//! | `g`            | a ghost note (very soft)                             |
//! | `.` or `-`     | nothing                                              |
//! | `_`            | hold: the hit before it lasts one step longer        |
//! | digit after a hit | a roll: that many evenly spaced hits in the hit's time |
//! | `\|`           | bar line                                             |
//!
//! Each bar's characters divide that bar evenly, so the number of
//! characters sets the resolution and can differ from bar to bar and from
//! lane to lane. Spaces are ignored. `o` and `O` are accepted for `x` and
//! `X`, as in drum tablature.

use crate::error::{Error, Result};
use crate::expression::Level;
use std::fmt;

/// One step of a pattern.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Cell {
    Rest,
    /// A hit, played `roll` times within its length (1 = a single hit).
    Hit {
        level: Level,
        roll: u8,
    },
    /// The previous hit carries on through this step.
    Hold,
}

/// A hit of a pattern placed in time.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hit {
    /// Beats from the start of the pattern.
    pub start: f64,
    /// Its own step plus any holds after it.
    pub length: f64,
    pub level: Level,
}

/// A parsed step pattern of one or more bars.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StepPattern {
    bars: Vec<Vec<Cell>>,
}

/// The legend shown in errors and reference listings.
pub const LEGEND: &str = "x = hit, X = accent, g = ghost, . = rest, _ = hold the previous hit one step longer, a digit \
                          after a hit = a roll of that many hits (x3), | = bar line; each bar's characters divide \
                          the bar evenly (16 = sixteenths, 12 = triplet eighths, 8 = eighths)";

impl StepPattern {
    pub fn parse(text: &str) -> Result<Self> {
        let fail = |problem: String| Err(Error::new(format!("'{text}': {problem}; the notation is: {LEGEND}")));
        let mut bars: Vec<Vec<Cell>> = Vec::new();
        let mut any_hit = false;
        let segments: Vec<&str> = text.split('|').collect();
        let last = segments.len() - 1;
        for (index, segment) in segments.iter().enumerate() {
            let mut cells: Vec<Cell> = Vec::new();
            for c in segment.chars().filter(|c| !c.is_whitespace()) {
                let cell = match c {
                    'x' | 'o' => Cell::Hit { level: Level::Normal, roll: 1 },
                    'X' | 'O' => Cell::Hit { level: Level::Accent, roll: 1 },
                    'g' => Cell::Hit { level: Level::Ghost, roll: 1 },
                    '.' | '-' => Cell::Rest,
                    '_' if any_hit => Cell::Hold,
                    '_' => return fail("a hold '_' needs a hit before it".to_string()),
                    '2'..='9' => match cells.last_mut() {
                        Some(Cell::Hit { roll, .. }) if *roll == 1 => {
                            *roll = c as u8 - b'0';
                            continue;
                        }
                        _ => return fail(format!("the roll count '{c}' must come straight after a hit, as in x3")),
                    },
                    other => return fail(format!("'{other}' is not a step")),
                };
                any_hit |= matches!(cell, Cell::Hit { .. });
                cells.push(cell);
            }
            // Bar lines may frame the pattern; an empty bar between them is a mistake.
            if cells.is_empty() {
                if index == 0 || index == last {
                    continue;
                }
                return fail(format!("bar {} is empty; write rests (....) for a silent bar", bars.len() + 1));
            }
            bars.push(cells);
        }
        if bars.is_empty() {
            return fail("the pattern has no steps".to_string());
        }
        Ok(Self { bars })
    }

    /// A one-bar pattern built from cells.
    pub(crate) fn from_cells(cells: Vec<Cell>) -> Self {
        Self { bars: vec![cells] }
    }

    pub(crate) fn from_bars(bars: Vec<Vec<Cell>>) -> Self {
        Self { bars }
    }

    /// Number of bars.
    pub fn bars(&self) -> usize {
        self.bars.len()
    }

    /// Number of steps in each bar.
    pub fn steps_per_bar(&self) -> Vec<usize> {
        self.bars.iter().map(Vec::len).collect()
    }

    pub fn is_silent(&self) -> bool {
        !self.bars.iter().flatten().any(|cell| matches!(cell, Cell::Hit { .. }))
    }

    /// The hits of one pass through the pattern, in order, with rolls
    /// written out as separate hits. Hits after the first of a roll are a
    /// little softer, so the roll keeps the pulse of the grid.
    pub fn hits(&self, bar_length: f64) -> Vec<Hit> {
        // (start, length, level, roll) with holds folded into the length.
        let mut spans: Vec<(f64, f64, Level, u8)> = Vec::new();
        for (bar, cells) in self.bars.iter().enumerate() {
            let step = bar_length / cells.len() as f64;
            for (index, cell) in cells.iter().enumerate() {
                match cell {
                    Cell::Hit { level, roll } => {
                        spans.push((bar as f64 * bar_length + index as f64 * step, step, *level, *roll));
                    }
                    Cell::Hold => {
                        if let Some(span) = spans.last_mut() {
                            span.1 += step;
                        }
                    }
                    Cell::Rest => {}
                }
            }
        }
        let mut hits = Vec::new();
        for (start, length, level, roll) in spans {
            let each = length / roll as f64;
            for repeat in 0..roll {
                let level = match (repeat, level) {
                    (0, level) => level,
                    (_, Level::Accent) => Level::Normal,
                    (_, level) => level,
                };
                hits.push(Hit { start: start + repeat as f64 * each, length: each, level });
            }
        }
        hits
    }

    /// The hits of the pattern repeated to cover `length` beats.
    pub fn hits_for(&self, bar_length: f64, length: f64) -> Vec<Hit> {
        let once = self.hits(bar_length);
        let period = self.bars.len() as f64 * bar_length;
        let mut out = Vec::new();
        let mut offset = 0.0;
        while offset < length - 1e-9 {
            out.extend(
                once.iter()
                    .filter(|hit| offset + hit.start < length - 1e-9)
                    .map(|hit| Hit { start: offset + hit.start, ..*hit }),
            );
            offset += period;
        }
        out
    }
}

impl fmt::Display for StepPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, bar) in self.bars.iter().enumerate() {
            if index > 0 {
                f.write_str("|")?;
            }
            for cell in bar {
                match cell {
                    Cell::Rest => f.write_str(".")?,
                    Cell::Hold => f.write_str("_")?,
                    Cell::Hit { level, roll } => {
                        f.write_str(match level {
                            Level::Ghost => "g",
                            Level::Normal => "x",
                            Level::Accent => "X",
                        })?;
                        if *roll > 1 {
                            write!(f, "{roll}")?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> StepPattern {
        StepPattern::parse(text).unwrap_or_else(|error| panic!("{error}"))
    }

    fn starts(text: &str, bar_length: f64) -> Vec<f64> {
        pattern(text).hits(bar_length).iter().map(|hit| hit.start).collect()
    }

    #[test]
    fn the_number_of_steps_sets_the_resolution() {
        assert_eq!(starts("x...x...x...x...", 4.0), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts("x.x.x.x.", 4.0), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts("xxxx", 4.0), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts("..x...x...x...x.", 4.0), [0.5, 1.5, 2.5, 3.5]);
        let shuffle = starts("x.xx.xx.xx.x", 4.0);
        assert_eq!(shuffle.len(), 8);
        assert!((shuffle[1] - 2.0 / 3.0).abs() < 1e-9, "twelve steps are triplet eighths: {shuffle:?}");
        assert_eq!(starts("x..x..", 3.0), [0.0, 1.5], "six steps in a 6/8 bar are eighths");
        assert_eq!(starts("x.x.x..", 3.5), [0.0, 1.0, 2.0], "seven in 7/8");
    }

    #[test]
    fn bars_levels_and_spaces() {
        let two_bars = pattern("x... x... | X.g. x-x-");
        assert_eq!(two_bars.bars(), 2);
        assert_eq!(two_bars.steps_per_bar(), [8, 8]);
        let hits = two_bars.hits(4.0);
        assert_eq!(hits.iter().map(|hit| hit.start).collect::<Vec<_>>(), [0.0, 2.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(hits[2].level, Level::Accent);
        assert_eq!(hits[3].level, Level::Ghost);
        assert_eq!(hits[0].level, Level::Normal);
        assert_eq!(two_bars.to_string(), "x...x...|X.g.x.x.");
        assert_eq!(pattern("|x...|x.x.|").bars(), 2, "bar lines may frame the pattern");
        assert_eq!(pattern("o..O").to_string(), "x..X", "tablature letters are understood");
        // Bars can have different resolutions.
        assert_eq!(pattern("x.x.|xxx").steps_per_bar(), [4, 3]);
        assert!(pattern("....").is_silent());
    }

    #[test]
    fn holds_lengthen_and_rolls_subdivide() {
        let held = pattern("x__x__x_").hits(4.0);
        assert_eq!(
            held.iter().map(|hit| (hit.start, hit.length)).collect::<Vec<_>>(),
            [(0.0, 1.5), (1.5, 1.5), (3.0, 1.0)]
        );
        // A hold carries across the bar line.
        let tied = pattern("...x|_...").hits(4.0);
        assert_eq!((tied[0].start, tied[0].length), (3.0, 2.0));

        let roll = pattern("x.x.x.x.x.x.x3_x.").hits(4.0);
        assert_eq!(roll.len(), 10);
        let triplet: Vec<f64> = roll[6..9].iter().map(|hit| hit.start).collect();
        assert!((triplet[0] - 3.0).abs() < 1e-9);
        assert!((triplet[1] - (3.0 + 1.0 / 6.0)).abs() < 1e-9, "three hits across an eighth: {triplet:?}");
        assert!((triplet[2] - (3.0 + 2.0 / 6.0)).abs() < 1e-9);
        assert_eq!(roll[9].start, 3.5);

        let ratchet = pattern("X4...").hits(4.0);
        assert_eq!(ratchet.iter().map(|hit| hit.start).collect::<Vec<_>>(), [0.0, 0.25, 0.5, 0.75]);
        assert_eq!(ratchet[0].level, Level::Accent);
        assert_eq!(ratchet[1].level, Level::Normal, "only the first hit of a roll carries the accent");
        assert_eq!(pattern("x3_.").to_string(), "x3_.");
    }

    #[test]
    fn patterns_repeat_to_fill_a_length() {
        let hits = pattern("x...").hits_for(4.0, 10.0);
        assert_eq!(hits.iter().map(|hit| hit.start).collect::<Vec<_>>(), [0.0, 4.0, 8.0]);
        let hits = pattern("x.x.|..x.").hits_for(4.0, 12.0);
        assert_eq!(hits.iter().map(|hit| hit.start).collect::<Vec<_>>(), [0.0, 2.0, 6.0, 8.0, 10.0]);
    }

    #[test]
    fn mistakes_are_explained_with_the_legend() {
        for (bad, reason) in [
            ("x..q", "'q' is not a step"),
            ("", "no steps"),
            ("|", "no steps"),
            ("_x..", "needs a hit before it"),
            ("3x..", "straight after a hit"),
            ("x..3", "straight after a hit"),
            ("x23.", "straight after a hit"),
            ("x...||x...", "bar 2 is empty"),
            ("x1..", "'1' is not a step"),
        ] {
            let error = StepPattern::parse(bad).unwrap_err();
            assert!(error.message().contains(reason), "{bad}: {error}");
            assert!(error.message().contains("X = accent"), "{bad}: {error}");
            assert!(error.message().starts_with(&format!("'{bad}'")), "{bad}: {error}");
        }
    }
}
