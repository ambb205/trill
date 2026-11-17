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
use crate::project::MIN_VOLUME_DB;
use serde::{Deserialize, Serialize};

/// A level in dB whose range starts this low or lower, and ends above
/// 0 dB, is a fader: its bottom stands for silence. See
/// [`Travel::of_param`].
const FADER_FROM_DB: f64 = -40.0;
/// A range that spans at least this many times its lowest value travels
/// by ratio.
const RATIO_FROM: f64 = 100.0;

/// The law of a control's travel.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// A fader, for a level that reaches down to silence: even in the
    /// square of the decibels above `floor`, the level that stands for
    /// silence. The top of the travel is nearly even in decibels and has
    /// most of the room; the last stretch falls away quickly. From 0 dB
    /// to silence at −70 dB a fader is at −9 dB after a quarter of the
    /// way, −20 dB after half and −35 dB after three quarters.
    Fader { floor: f64 },
}

impl Travel {
    /// The travel of the mixer's faders: a track's volume, the master's,
    /// a send.
    pub const FADER: Travel = Travel::Fader { floor: MIN_VOLUME_DB as f64 };

    /// How the control of a device's parameter travels, from what the
    /// parameter says of itself:
    ///
    /// * a level in dB that starts at −40 dB or lower and ends above
    ///   0 dB (a gain, an instrument's volume) is a [`Travel::Fader`]
    ///   down to its lowest value;
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
        if info.unit.trim().eq_ignore_ascii_case("db") && info.min <= FADER_FROM_DB && info.max > 0.0 {
            return Travel::Fader { floor: info.min };
        }
        Travel::Linear
    }

    pub fn is_linear(&self) -> bool {
        *self == Travel::Linear
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
            Travel::Fader { floor } => {
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
            Travel::Fader { floor } if floor < max => {
                let share = ((value - floor) / (max - floor)).clamp(0.0, 1.0);
                share * share
            }
            Travel::Fader { .. } => (value - min) / (max - min),
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
            Travel::Fader { floor } if floor < max => floor + position.sqrt() * (max - floor),
            Travel::Fader { .. } => min + position * (max - min),
        };
        value.clamp(min, max)
    }

    /// The word a tool or a file names the law by: `linear`, `log`, `fader`.
    pub fn name(&self) -> &'static str {
        match self {
            Travel::Linear => "linear",
            Travel::Log => "log",
            Travel::Fader { .. } => "fader",
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
    use crate::project::MAX_VOLUME_DB;

    fn param(min: f64, max: f64, unit: &str, kind: ParamKind) -> ParamInfo {
        ParamInfo { id: "p".into(), name: "P".into(), min, max, default: min, unit: unit.into(), kind }
    }

    fn close(got: f64, wanted: f64, within: f64) -> bool {
        (got - wanted).abs() <= within
    }

    #[test]
    fn a_fade_along_a_fader_is_still_heard_half_way() {
        let fader = Travel::FADER;
        // The lines of the bench's report, half way: a stop, a stop from
        // above unity, and a send thrown open.
        assert!(close(fader.between(0.0, -60.0, 0.5), -20.0, 1e-9), "{}", fader.between(0.0, -60.0, 0.5));
        assert!(close(fader.between(6.0, -60.0, 0.5), -15.8, 0.05), "{}", fader.between(6.0, -60.0, 0.5));
        assert!(close(fader.between(-40.0, -6.0, 0.5), -20.0, 0.05), "{}", fader.between(-40.0, -6.0, 0.5));
        // Straight in decibels the first would be at -30 dB, as good as gone.
        assert_eq!(Travel::Linear.between(0.0, -60.0, 0.5), -30.0);
        // To silence itself: a quarter, a half and three quarters of the way.
        let down = |progress: f64| fader.between(0.0, -70.0, progress);
        assert!(close(down(0.25), -9.4, 0.05) && close(down(0.5), -20.5, 0.05) && close(down(0.75), -35.0, 0.05));
        // Up is the same line the other way.
        assert!(close(fader.between(-60.0, 0.0, 0.5), -20.0, 1e-9));
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
        for travel in [Travel::Linear, Travel::Log, Travel::FADER, Travel::Fader { floor: -48.0 }] {
            for (from, to) in [(0.1, 7.3), (-33.3, 5.9), (812.7, 20.1), (-70.0, 6.0), (-120.0, -3.0)] {
                assert_eq!(travel.between(from, to, 0.0), from, "{travel:?}");
                assert_eq!(travel.between(from, to, 1.0), to, "{travel:?}");
                let (near, far) = (from.min(to), from.max(to));
                for step in 1..10 {
                    let value = travel.between(from, to, step as f64 / 10.0);
                    // A value under a fader's floor is the floor: silence.
                    let lowest = match travel {
                        Travel::Fader { floor } => near.max(floor).min(far),
                        _ => near,
                    };
                    assert!(value >= lowest - 1e-9 && value <= far + 1e-9, "{travel:?} {from}..{to}: {value}");
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
            (Travel::Fader { floor: -48.0 }, -48.0, 12.0, 0.0, -30.0),
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
        assert!(fader.position(0.0, min, max) > 0.8, "unity gain is near the top");
        assert!(fader.position(-12.0, min, max) > 0.5 && fader.position(-48.0, min, max) < 0.1);
        for db in [-70.0, -48.0, -24.0, -12.0, -6.0, 0.0, 3.0, 6.0] {
            assert!(close(fader.value_at(fader.position(db, min, max), min, max), db, 1e-9), "{db} dB");
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
        // A level that reaches silence is a fader down to its floor.
        assert_eq!(Travel::of_param(&param(-70.0, 24.0, "dB", float.clone())), Travel::Fader { floor: -70.0 });
        assert_eq!(Travel::of_param(&param(-48.0, 12.0, "dB", float.clone())), Travel::Fader { floor: -48.0 });
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
    fn a_travel_is_written_as_a_word_and_a_fader_with_its_floor() {
        let text = |travel: Travel| serde_json::to_string(&travel).unwrap();
        assert_eq!(text(Travel::Linear), "\"linear\"");
        assert_eq!(text(Travel::Log), "\"log\"");
        assert_eq!(text(Travel::FADER), "{\"fader\":{\"floor\":-70.0}}");
        for travel in [Travel::Linear, Travel::Log, Travel::FADER] {
            assert_eq!(serde_json::from_str::<Travel>(&text(travel)).unwrap(), travel);
        }
        assert_eq!((Travel::Linear.name(), Travel::Log.name(), Travel::FADER.name()), ("linear", "log", "fader"));
    }
}
