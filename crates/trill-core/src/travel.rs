//! How a control's values lie along its travel, and so how an automation
//! line gets from one point to the next.
//!
//! A fader does not move in decibels and a cutoff knob does not move in
//! hertz: half way up a fader is not half the decibels, and half way round
//! a cutoff knob is the middle *pitch* of its range, not the middle
//! frequency. A line between two automation points is straight along that
//! travel, as if a hand moved the control at an even speed. That is what a
//! fade or a sweep means: drawn straight in decibels, a fade to silence is
//! all but over half way through, and drawn straight in hertz a sweep
//! spends nearly all of its time in the top octaves.
//!
//! There is one definition of each travel, here. The window's faders and
//! knobs are laid out by it, an envelope's lane is drawn on it (where such
//! a line is a straight line), and the engine plays it.

use crate::device::{ParamInfo, ParamKind};
use crate::project::{MAX_VOLUME_DB, MIN_VOLUME_DB};
use serde::{Deserialize, Serialize};

/// A level in dB whose range starts this low or lower, and ends above
/// 0 dB, is one whose bottom stands for silence: a fader or a gain. See
/// [`Travel::of_param`].
const LEVEL_FROM_DB: f64 = -40.0;
/// Such a level that can be turned up by more than this is a gain, which
/// boosts as far as it cuts; up to it, a volume.
const GAIN_ABOVE_DB: f64 = 12.0;
/// A range that spans at least this many times its lowest value travels
/// by ratio.
const RATIO_FROM: f64 = 100.0;

/// How far below its top a fader's knee is, in dB: the top three fifths of
/// its travel, less a hair.
const FADER_TOP_DB: f64 = 23.8;
/// How much travel a fader has left below its knee, in the decibels it
/// would have been above it.
const FADER_REACH: f64 = 16.2;
/// How much travel a gain has left below its knee, likewise.
const GAIN_REACH: f64 = 17.5;

/// The law of a control's travel.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", from = "Saved")]
pub enum Travel {
    /// Even in the value itself: a plain amount (a mix, a feedback, a
    /// pan), a boost or a cut in dB, anything that moves in steps. Also
    /// how every envelope travelled before lines followed their control,
    /// which is why it is what a lane has when its file does not say.
    #[default]
    Linear,
    /// Even in ratio: an octave is the same distance wherever it is.
    /// Frequencies and times, whose ranges span orders of magnitude.
    Log,
    /// A fader, for a level that reaches down to silence: a track's
    /// volume, the master's, a send, an instrument's volume. Even in
    /// decibels above `knee`, which lies 23.8 dB under the top of the
    /// travel; below it the level falls ever faster, a stretch that would
    /// have been `y` dB taking `y + y²/8`, and is silent 49 dB under the
    /// knee. This is how the faders of Ableton Live travel, to a twentieth
    /// of a decibel where Live shows that much. From 0 dB to −60 dB a
    /// volume is at −8.2 dB after a quarter of the way, −16.3 dB after
    /// half and −30.0 dB after three quarters; a send, whose knee is 6 dB
    /// lower, at −9.3, −18.6 and −30.1 dB.
    Fader { knee: f64 },
    /// A gain, for a level that boosts as far as it cuts and reaches down
    /// to silence: a Utility's. Even in decibels above `knee`; below it the
    /// gain itself (the factor, not the decibels) falls as the square of
    /// what is left of the travel, and is nothing where even decibels
    /// would have reached 17.5 dB under the knee. This is how the gain of
    /// Ableton Live's Utility travels, to a tenth of a decibel. From 0 dB
    /// to −60 dB it is at −8.4 dB after a quarter of the way, −16.7 dB
    /// after half and −27.4 dB after three quarters.
    Gain { knee: f64 },
    /// How a level travelled in songs of formats 3 to 5, which called it
    /// a fader: even in the square of the decibels above `floor`, the
    /// level that stood for silence. Half way from 0 dB to −60 dB it is at
    /// −20 dB. Kept so that an envelope saved with it plays as it did; no
    /// control travels so now.
    Square { floor: f64 },
}

/// A travel as a file or a journal has it. Files of formats 3 to 5 wrote
/// the law that is now [`Travel::Square`] as a fader with a floor; a fader
/// with a knee is the fader of this build.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Saved {
    Linear,
    Log,
    Fader {
        #[serde(default)]
        knee: Option<f64>,
        #[serde(default)]
        floor: Option<f64>,
    },
    Gain {
        knee: f64,
    },
    Square {
        floor: f64,
    },
}

impl From<Saved> for Travel {
    fn from(saved: Saved) -> Self {
        match saved {
            Saved::Linear => Travel::Linear,
            Saved::Log => Travel::Log,
            Saved::Fader { knee: Some(knee), .. } => Travel::Fader { knee },
            Saved::Fader { knee: None, floor } => Travel::Square { floor: floor.unwrap_or(MIN_VOLUME_DB as f64) },
            Saved::Gain { knee } => Travel::Gain { knee },
            Saved::Square { floor } => Travel::Square { floor },
        }
    }
}

impl Travel {
    /// The travel of the mixer's faders: a track's volume and the
    /// master's. Unity is at 85 hundredths of the way up, as on a fader of
    /// Live's.
    pub const FADER: Travel = Travel::Fader { knee: MAX_VOLUME_DB as f64 - FADER_TOP_DB };

    /// The travel of a send, which is a fader whose top is 0 dB.
    pub const SEND: Travel = Travel::Fader { knee: -FADER_TOP_DB };

    /// The travel of a gain: its knee is at −17.5 dB.
    pub const GAIN: Travel = Travel::Gain { knee: -17.5 };

    /// How the control of a device's parameter travels, from what the
    /// parameter says of itself:
    ///
    /// * a level in dB that starts at −40 dB or lower and ends above
    ///   0 dB reaches down to silence. One that can be turned up by more
    ///   than 12 dB (a Utility's gain) is a [`Travel::Gain`]; any other
    ///   (an instrument's volume) is a [`Travel::Fader`] whose knee lies
    ///   23.8 dB under its top;
    /// * a positive range that spans a hundred times its lowest value or
    ///   more (a frequency, most times) is [`Travel::Log`];
    /// * everything else is [`Travel::Linear`]: an amount, a boost or a
    ///   cut, a threshold, a switch, a choice, and every parameter of a
    ///   plugin, whose values are already the positions of its own knobs
    ///   as far as a host can know.
    pub fn of_param(info: &ParamInfo) -> Travel {
        if info.kind != ParamKind::Float || !spans(info.min, info.max) {
            return Travel::Linear;
        }
        if info.min > 0.0 && info.max / info.min >= RATIO_FROM {
            return Travel::Log;
        }
        if info.unit.trim().eq_ignore_ascii_case("db") && info.min <= LEVEL_FROM_DB && info.max > 0.0 {
            return match info.max > GAIN_ABOVE_DB {
                true => Travel::GAIN,
                false => Travel::Fader { knee: info.max - FADER_TOP_DB },
            };
        }
        Travel::Linear
    }

    pub fn is_linear(&self) -> bool {
        *self == Travel::Linear
    }

    /// Where `value` lies along the travel, on a scale of the law's own
    /// that a line is straight on: for a fader and a gain, the decibels
    /// above the knee, and below it what is left of the travel. `None` for
    /// the laws that need no such scale.
    fn along(self, value: f64) -> Option<f64> {
        match self {
            Travel::Fader { knee } if value >= knee => Some(value - knee),
            // `y + y * y / 8` dB under the knee, as far down as it goes.
            Travel::Fader { knee } => Some(-(-4.0 + (16.0 + 8.0 * (knee - value)).sqrt()).min(FADER_REACH)),
            Travel::Gain { knee } if value >= knee => Some(value - knee),
            Travel::Gain { knee } => Some(GAIN_REACH * (10f64.powf((value - knee) / 40.0) - 1.0)),
            Travel::Linear | Travel::Log | Travel::Square { .. } => None,
        }
    }

    /// The value at `along`: [`along`](Self::along) the other way round.
    fn at(self, along: f64) -> f64 {
        match self {
            Travel::Fader { knee } | Travel::Gain { knee } if along >= 0.0 => knee + along,
            Travel::Fader { knee } => {
                let under = (-along).min(FADER_REACH);
                knee - under - under * under / 8.0
            }
            // Where nothing of the travel is left there is no gain at all:
            // said as a level far under any that is heard.
            Travel::Gain { knee } => knee + 40.0 * (1.0 + along / GAIN_REACH).max(1e-9).log10(),
            Travel::Linear | Travel::Log | Travel::Square { .. } => along,
        }
    }

    /// The value `progress` of the way (0..=1) along a line from `from` to
    /// `to`. It depends on the two ends only, not on the control's range:
    /// the same two points make the same line wherever they sit.
    pub fn between(self, from: f64, to: f64, progress: f64) -> f64 {
        if progress <= 0.0 {
            return from;
        }
        if progress >= 1.0 {
            return to;
        }
        match self {
            Travel::Log if from > 0.0 && to > 0.0 => from * (to / from).powf(progress),
            // A ratio to or from nothing is no ratio: such a line is
            // straight.
            Travel::Linear | Travel::Log => from + (to - from) * progress,
            Travel::Fader { .. } | Travel::Gain { .. } => {
                let (start, end) = (self.along(from).unwrap_or(from), self.along(to).unwrap_or(to));
                // Never past either end: a fader's last stretch of travel
                // is silence, whatever level under it a point names.
                self.at(start + (end - start) * progress).max(from.min(to)).min(from.max(to))
            }
            Travel::Square { floor } => {
                let (low, high) = ((from - floor).max(0.0), (to - floor).max(0.0));
                floor + (low * low + (high * high - low * low) * progress).max(0.0).sqrt()
            }
        }
    }

    /// How far along a travel that runs from `min` to `max` the control is
    /// at `value`: 0 at the bottom, 1 at the top.
    pub fn position(self, value: f64, min: f64, max: f64) -> f64 {
        if !spans(min, max) {
            return 0.0;
        }
        let value = value.clamp(min, max);
        let position = match self {
            Travel::Log if min > 0.0 => (value / min).ln() / (max / min).ln(),
            Travel::Linear | Travel::Log => (value - min) / (max - min),
            Travel::Fader { .. } | Travel::Gain { .. } => {
                let along = |value: f64| self.along(value).unwrap_or(value);
                (along(value) - along(min)) / (along(max) - along(min))
            }
            Travel::Square { floor } if floor < max => {
                let share = ((value - floor) / (max - floor)).clamp(0.0, 1.0);
                share * share
            }
            Travel::Square { .. } => (value - min) / (max - min),
        };
        if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.0 }
    }

    /// The value at `position` (0..=1) of a travel that runs from `min` to
    /// `max`: the other way round from [`position`](Self::position).
    pub fn value_at(self, position: f64, min: f64, max: f64) -> f64 {
        if !spans(min, max) {
            return min;
        }
        let position = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.0 };
        let value = match self {
            Travel::Log if min > 0.0 => min * (max / min).powf(position),
            Travel::Linear | Travel::Log => min + position * (max - min),
            // The bottom of the travel is the bottom of the range: the
            // last stretch of a fader is silence.
            Travel::Fader { .. } | Travel::Gain { .. } if position <= 0.0 => min,
            Travel::Fader { .. } | Travel::Gain { .. } => {
                let along = |value: f64| self.along(value).unwrap_or(value);
                self.at(along(min) + position * (along(max) - along(min)))
            }
            Travel::Square { floor } if floor < max => floor + position.sqrt() * (max - floor),
            Travel::Square { .. } => min + position * (max - min),
        };
        value.clamp(min, max)
    }

    /// The word a tool or a file names the law by: `linear`, `log`,
    /// `fader`, `gain`, and `square` for the law of formats 3 to 5.
    pub fn name(&self) -> &'static str {
        match self {
            Travel::Linear => "linear",
            Travel::Log => "log",
            Travel::Fader { .. } => "fader",
            Travel::Gain { .. } => "gain",
            Travel::Square { .. } => "square",
        }
    }
}

/// Whether `min..max` is a range with anything in it. False for bounds that
/// are not numbers.
fn spans(min: f64, max: f64) -> bool {
    min < max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(min: f64, max: f64, unit: &str, kind: ParamKind) -> ParamInfo {
        ParamInfo { id: "p".into(), name: "P".into(), min, max, default: min, unit: unit.into(), kind }
    }

    fn close(got: f64, wanted: f64, within: f64) -> bool {
        (got - wanted).abs() <= within
    }

    /// The law of formats 3 to 5, as their mixer's lanes have it.
    const SQUARE: Travel = Travel::Square { floor: MIN_VOLUME_DB as f64 };

    /// What a line from `from` to `to` is at each eighth of the way.
    fn eighths(travel: Travel, from: f64, to: f64) -> [f64; 7] {
        std::array::from_fn(|index| travel.between(from, to, (index + 1) as f64 / 8.0))
    }

    #[test]
    fn a_level_travels_as_ableton_live_shows_it_to() {
        // What Live shows at each eighth of a line between two levels, read
        // from what it shows at 257 places along a Utility's gain, a
        // mixer's send and Operator's volume (the bench's `laws.json`).
        let shown: [(Travel, f64, f64, [f64; 7], f64); 12] = [
            // A Utility's gain, shown to a tenth of a decibel.
            (Travel::GAIN, 0.0, -60.0, [-4.18, -8.36, -12.56, -16.75, -21.29, -27.48, -37.05], 0.15),
            (Travel::GAIN, 6.0, -40.0, [1.48, -3.04, -7.56, -12.06, -16.61, -21.57, -28.49], 0.15),
            (Travel::GAIN, -10.0, -50.0, [-12.84, -15.59, -18.34, -21.59, -25.49, -30.59, -37.74], 0.15),
            (Travel::GAIN, -60.0, 0.0, [-37.05, -27.48, -21.29, -16.75, -12.56, -8.36, -4.18], 0.15),
            (Travel::GAIN, 0.0, -30.0, [-3.3, -6.61, -9.9, -13.2, -16.52, -20.0, -24.33], 0.15),
            // A send, shown to a thousandth.
            (Travel::SEND, 0.0, -60.0, [-4.66, -9.32, -13.98, -18.64, -23.3, -30.13, -42.35], 0.05),
            (Travel::SEND, -4.0, -66.0, [-8.33, -12.65, -16.98, -21.31, -26.05, -34.7, -48.02], 0.05),
            (Travel::SEND, -12.0, -50.0, [-14.85, -17.7, -20.56, -23.41, -27.02, -32.65, -40.31], 0.05),
            (Travel::SEND, -6.0, -30.0, [-8.74, -11.48, -14.21, -16.95, -19.69, -22.43, -25.39], 0.05),
            // An instrument's volume, whose top is +6 dB as a track's is,
            // shown to the decibel under -10 dB.
            (Travel::FADER, 0.0, -60.0, [-4.11, -8.16, -12.0, -16.0, -21.0, -30.0, -42.94], 0.6),
            (Travel::FADER, 6.0, -40.0, [1.8, -2.4, -6.7, -11.0, -15.0, -20.0, -28.0], 0.6),
            (Travel::FADER, -6.0, -50.0, [-9.1, -12.0, -15.0, -18.0, -23.0, -30.0, -38.5], 0.6),
        ];
        for (travel, from, to, live, within) in shown {
            for (ours, live) in eighths(travel, from, to).into_iter().zip(live) {
                assert!(close(ours, live, within), "{travel:?} from {from} to {to}: {ours:.2} where Live shows {live}");
            }
        }
        // The bench's fade, a quarter, a half and three quarters of the
        // way: where the law of formats 3 to 5 was at -20 dB half way.
        let fade = |progress: f64| Travel::GAIN.between(0.0, -60.0, progress);
        assert!(close(fade(0.25), -8.4, 0.05) && close(fade(0.5), -16.8, 0.1) && close(fade(0.75), -27.5, 0.1));
        assert!(close(SQUARE.between(0.0, -60.0, 0.5), -20.0, 1e-9));
        // Straight in decibels it would be at -30 dB, as good as gone.
        assert_eq!(Travel::Linear.between(0.0, -60.0, 0.5), -30.0);
        // Above its knee a level is even in decibels, whichever it is.
        for travel in [Travel::FADER, Travel::SEND, Travel::GAIN] {
            assert!(close(travel.between(6.0, -10.0, 0.25), 2.0, 1e-9), "{travel:?}");
        }
    }

    #[test]
    fn a_fader_ends_in_silence_and_a_gain_gets_there() {
        // A fader's travel ends 49 dB under its knee: a line to any level
        // under that goes there and no further until it arrives.
        let down = eighths(Travel::FADER, 0.0, -70.0);
        assert!(close(down[1], -8.5, 0.05) && close(down[3], -17.0, 0.05) && close(down[5], -32.9, 0.05), "{down:?}");
        assert!(down[6] > -66.9 && down[6] < -45.0, "{down:?}");
        let further = eighths(Travel::FADER, 0.0, -200.0);
        assert_eq!(further, down, "silence is silence");
        // A gain has no bottom but silence itself, which it nears as fast
        // as a line can be heard to.
        let gone = eighths(Travel::GAIN, 0.0, -70.0);
        assert!(gone.windows(2).all(|pair| pair[1] < pair[0]) && gone[6] > -70.0 && gone[6] < -35.0, "{gone:?}");
        // Up is the same line the other way.
        let up = eighths(Travel::FADER, -70.0, 0.0);
        for (up, down) in up.iter().zip(down.iter().rev()) {
            assert!(close(*up, *down, 1e-9));
        }
    }

    #[test]
    fn the_law_of_older_songs_is_what_it_was() {
        // The lines of the report the law was made for, half way: a stop, a
        // stop from above unity, and a send thrown open.
        assert!(close(SQUARE.between(0.0, -60.0, 0.5), -20.0, 1e-9));
        assert!(close(SQUARE.between(6.0, -60.0, 0.5), -15.8, 0.05));
        assert!(close(SQUARE.between(-40.0, -6.0, 0.5), -20.0, 0.05));
        // To silence itself: a quarter, a half and three quarters of the way.
        let down = |progress: f64| SQUARE.between(0.0, -70.0, progress);
        assert!(close(down(0.25), -9.4, 0.05) && close(down(0.5), -20.5, 0.05) && close(down(0.75), -35.0, 0.05));
        assert!(close(SQUARE.between(-60.0, 0.0, 0.5), -20.0, 1e-9));
        let (min, max) = (MIN_VOLUME_DB as f64, MAX_VOLUME_DB as f64);
        for db in [-70.0, -48.0, -24.0, -12.0, -6.0, 0.0, 3.0, 6.0] {
            assert!(close(SQUARE.value_at(SQUARE.position(db, min, max), min, max), db, 1e-9), "{db} dB");
        }
        assert!(close(SQUARE.position(0.0, min, max), (70.0f64 / 76.0).powi(2), 1e-12));
    }

    #[test]
    fn a_sweep_by_ratio_passes_the_middle_pitch_half_way() {
        let half_way = Travel::Log.between(300.0, 9000.0, 0.5);
        assert!(close(half_way, (300.0f64 * 9000.0).sqrt(), 1e-9), "{half_way}");
        assert!(close(half_way, 1643.2, 0.1), "an octave takes the same time wherever it is: {half_way}");
        assert_eq!(Travel::Linear.between(300.0, 9000.0, 0.5), 4650.0);
        // An octave a quarter: 100 Hz to 1600 Hz is four octaves.
        for (progress, wanted) in [(0.25, 200.0), (0.5, 400.0), (0.75, 800.0)] {
            assert!(close(Travel::Log.between(100.0, 1600.0, progress), wanted, 1e-9));
        }
        assert_eq!(Travel::Log.between(0.0, 100.0, 0.5), 50.0, "nothing has no ratio to anything: straight");
        assert_eq!(Travel::Log.between(-4.0, 4.0, 0.25), -2.0);
    }

    #[test]
    fn a_line_starts_and_ends_exactly_on_its_points() {
        let laws = [
            Travel::Linear,
            Travel::Log,
            Travel::FADER,
            Travel::SEND,
            Travel::GAIN,
            SQUARE,
            Travel::Square { floor: -48.0 },
        ];
        for travel in laws {
            for (from, to) in [(0.1, 7.3), (-33.3, 5.9), (812.7, 20.1), (-70.0, 6.0), (-120.0, -3.0)] {
                assert_eq!(travel.between(from, to, 0.0), from, "{travel:?}");
                assert_eq!(travel.between(from, to, 1.0), to, "{travel:?}");
                let (near, far) = (from.min(to), from.max(to));
                let mut before = from;
                for step in 1..10 {
                    let value = travel.between(from, to, step as f64 / 10.0);
                    // A value under a floor is the floor: silence.
                    let lowest = match travel {
                        Travel::Square { floor } => near.max(floor).min(far),
                        _ => near,
                    };
                    assert!(value >= lowest - 1e-9 && value <= far + 1e-9, "{travel:?} {from}..{to}: {value}");
                    // And it never turns back on the way.
                    assert!((value - before) * (to - from) >= -1e-9, "{travel:?} {from}..{to}: {before} then {value}");
                    before = value;
                }
            }
        }
    }

    #[test]
    fn a_line_is_straight_along_the_travel_it_follows() {
        // Wherever the two points sit on the control, half way between them
        // in time is half way between them along its travel.
        let cases = [
            (Travel::FADER, MIN_VOLUME_DB as f64, MAX_VOLUME_DB as f64, -40.0, -6.0),
            (Travel::SEND, MIN_VOLUME_DB as f64, MAX_VOLUME_DB as f64, -60.0, -4.0),
            (Travel::GAIN, -70.0, 24.0, 0.0, -60.0),
            (Travel::Fader { knee: -11.8 }, -70.0, 12.0, 3.0, -30.0),
            (Travel::Square { floor: -48.0 }, -48.0, 12.0, 0.0, -30.0),
            (Travel::Log, 20.0, 20_000.0, 300.0, 9000.0),
            (Travel::Linear, -1.0, 1.0, -0.5, 0.75),
        ];
        for (travel, min, max, from, to) in cases {
            let (start, end) = (travel.position(from, min, max), travel.position(to, min, max));
            for step in 0..=8 {
                let progress = step as f64 / 8.0;
                let along = travel.position(travel.between(from, to, progress), min, max);
                assert!(close(along, start + (end - start) * progress, 1e-9), "{travel:?} at {progress}: {along}");
            }
        }
    }

    #[test]
    fn positions_and_values_are_each_other_backwards() {
        let fader = Travel::FADER;
        let (min, max) = (MIN_VOLUME_DB as f64, MAX_VOLUME_DB as f64);
        assert_eq!(fader.position(min, min, max), 0.0);
        assert_eq!(fader.position(max, min, max), 1.0);
        assert_eq!(fader.position(-200.0, min, max), 0.0);
        assert_eq!(fader.position(f64::NEG_INFINITY, min, max), 0.0, "silence is the bottom");
        // As on a fader of Live's: unity at 85 hundredths of the way up,
        // -18 dB two fifths of the way, and the knee just above that.
        assert!(close(fader.position(0.0, min, max), 0.85, 1e-12), "{}", fader.position(0.0, min, max));
        assert!(
            close(fader.position(-12.0, min, max), 0.55, 1e-12) && close(fader.position(-17.8, min, max), 0.405, 1e-12)
        );
        assert!(close(fader.position(-48.0, min, max), 0.104, 0.001), "{}", fader.position(-48.0, min, max));
        // The last stretch of the travel is silence: the bottom is the
        // bottom of the range, and just above it the fader's lowest level.
        assert_eq!(fader.value_at(0.0, min, max), min);
        assert!(close(fader.value_at(1e-9, min, max), -66.805, 0.001), "{}", fader.value_at(1e-9, min, max));
        for db in [-66.0, -48.0, -24.0, -17.8, -12.0, -6.0, 0.0, 3.0, 6.0] {
            assert!(close(fader.value_at(fader.position(db, min, max), min, max), db, 1e-9), "{db} dB");
        }
        // A send's knee is lower, and so its travel reaches the bottom of
        // the range before it runs out: 15.6 dB's worth of it under the knee.
        assert!(close(Travel::SEND.position(0.0, min, max), (23.8 + 15.636) / (29.8 + 15.636), 1e-4));
        // A gain is at unity where its range puts it, and goes all the way down.
        let gain = Travel::GAIN;
        assert_eq!((gain.position(-70.0, -70.0, 24.0), gain.position(24.0, -70.0, 24.0)), (0.0, 1.0));
        for db in [-70.0, -60.0, -40.0, -17.5, -6.0, 0.0, 12.0, 24.0] {
            assert!(close(gain.value_at(gain.position(db, -70.0, 24.0), -70.0, 24.0), db, 1e-9), "{db} dB");
        }
        let cutoff = Travel::Log;
        assert!(close(cutoff.value_at(0.5, 20.0, 20_000.0), 632.455, 0.01));
        for value in [20.0, 100.0, 440.0, 5000.0, 20_000.0] {
            let back = cutoff.value_at(cutoff.position(value, 20.0, 20_000.0), 20.0, 20_000.0);
            assert!(close(back, value, 1e-6 * value));
        }
        assert_eq!(Travel::Linear.position(0.0, -8.0, 8.0), 0.5);
        assert_eq!(Travel::Linear.value_at(0.75, -8.0, 8.0), 4.0);
        assert_eq!(Travel::Linear.value_at(7.0, -8.0, 8.0), 8.0, "never past the end");
        assert_eq!(Travel::Linear.position(1.0, 1.0, 1.0), 0.0, "a range of nothing has no travel");
        assert_eq!(Travel::Log.value_at(f64::NAN, 20.0, 20_000.0), 20.0);
    }

    #[test]
    fn a_parameter_says_how_its_control_travels() {
        let float = ParamKind::Float;
        // A level that reaches silence is a gain if it boosts as far as a
        // Utility's does, and a fader with its knee under its top if not.
        assert_eq!(Travel::of_param(&param(-70.0, 24.0, "dB", float.clone())), Travel::GAIN);
        assert_eq!(Travel::of_param(&param(-70.0, 6.0, "dB", float.clone())), Travel::FADER);
        assert_eq!(Travel::of_param(&param(-70.0, 12.0, "dB", float.clone())), Travel::Fader { knee: 12.0 - 23.8 });
        assert_eq!(Travel::of_param(&param(-48.0, 12.0, "dB", float.clone())).name(), "fader");
        // A boost or a cut, a makeup, a threshold and a ceiling are amounts in dB.
        for (min, max) in [(-18.0, 18.0), (-12.0, 24.0), (-60.0, 0.0), (-12.0, 0.0), (0.0, 36.0), (-24.0, 12.0)] {
            assert_eq!(Travel::of_param(&param(min, max, "dB", float.clone())), Travel::Linear, "{min}..{max} dB");
        }
        // Frequencies and times span orders of magnitude.
        assert_eq!(Travel::of_param(&param(20.0, 20_000.0, "Hz", float.clone())), Travel::Log);
        assert_eq!(Travel::of_param(&param(0.001, 10.0, "s", float.clone())), Travel::Log);
        assert_eq!(Travel::of_param(&param(0.0, 200.0, "ms", float.clone())), Travel::Linear, "from nothing: no ratio");
        assert_eq!(Travel::of_param(&param(0.0, 1.0, "", float.clone())), Travel::Linear);
        assert_eq!(Travel::of_param(&param(-8.0, 8.0, "oct", float.clone())), Travel::Linear);
        // What moves in steps has no line to speak of.
        assert_eq!(Travel::of_param(&param(1.0, 700.0, "", ParamKind::Int)), Travel::Linear);
        assert_eq!(Travel::of_param(&param(0.0, 1.0, "", ParamKind::Bool)), Travel::Linear);
        assert_eq!(Travel::of_param(&param(1.0, 1.0, "Hz", float)), Travel::Linear);
    }

    #[test]
    fn a_travel_is_written_as_a_word_and_an_older_file_s_fader_is_the_law_it_was() {
        let text = |travel: Travel| serde_json::to_string(&travel).unwrap();
        assert_eq!(text(Travel::Linear), "\"linear\"");
        assert_eq!(text(Travel::Log), "\"log\"");
        assert_eq!(text(Travel::SEND), "{\"fader\":{\"knee\":-23.8}}");
        assert_eq!(text(Travel::GAIN), "{\"gain\":{\"knee\":-17.5}}");
        assert_eq!(text(SQUARE), "{\"square\":{\"floor\":-70.0}}");
        for travel in [Travel::Linear, Travel::Log, Travel::FADER, Travel::SEND, Travel::GAIN, SQUARE] {
            assert_eq!(serde_json::from_str::<Travel>(&text(travel)).unwrap(), travel);
        }
        // What formats 3 to 5 wrote for a level: a fader with a floor.
        let read = |text: &str| serde_json::from_str::<Travel>(text).unwrap();
        assert_eq!(read("{\"fader\":{\"floor\":-70.0}}"), SQUARE);
        assert_eq!(read("{\"fader\":{\"floor\":-48.0}}"), Travel::Square { floor: -48.0 });
        assert_eq!(read("{\"fader\":{\"knee\":-17.8}}"), Travel::Fader { knee: -17.8 });
        let names = [Travel::Linear, Travel::Log, Travel::FADER, Travel::GAIN, SQUARE].map(|travel| travel.name());
        assert_eq!(names, ["linear", "log", "fader", "gain", "square"]);
    }
}
