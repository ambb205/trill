//! Value conversions and number formatting: decibels, fader and meter
//! scales, and device parameter ranges. Pure functions, unit-tested.
//!
//! Numbers are set as the design asks: a real minus sign (U+2212), never a
//! negative zero, and a space between a value and its unit.

use trill_core::Travel;
use trill_core::device::{ParamInfo, ParamKind};
use trill_core::project::{MAX_VOLUME_DB, MIN_VOLUME_DB};
use trill_noir::format::{fmt_val, fmt_val_signed, typeset};

/// Bottom of the meter scale, dBFS. Anything quieter draws as empty. Meters
/// share the faders' scale so a fader and the level beside it line up.
pub const METER_FLOOR_DB: f32 = MIN_VOLUME_DB;

/// Linear gain to dBFS; silence maps to -infinity.
pub fn gain_to_db(gain: f32) -> f32 {
    if gain > 0.0 { 20.0 * gain.log10() } else { f32::NEG_INFINITY }
}

/// Fader travel, 0 (bottom) to 1 (top), for a volume in dB. Also where a
/// level in dBFS sits on a meter.
///
/// It is the travel automation follows ([`Travel::FADER`]), so a line
/// between two points of a volume's envelope is a straight line in its
/// lane. Travel is quadratic in dB, so the range around unity where mixing
/// happens gets most of the fader and the last stretch drops off quickly:
/// 0 dB sits at 85%, -12 dB at 58%, -48 dB at 8%.
pub fn db_to_fader(db: f32) -> f32 {
    // Not a number is silence, as far as a meter is concerned.
    let db = if db.is_nan() { MIN_VOLUME_DB } else { db };
    Travel::FADER.position(db as f64, MIN_VOLUME_DB as f64, MAX_VOLUME_DB as f64) as f32
}

/// Inverse of [`db_to_fader`].
pub fn fader_to_db(position: f32) -> f32 {
    Travel::FADER.value_at(position as f64, MIN_VOLUME_DB as f64, MAX_VOLUME_DB as f64) as f32
}

/// `"\u{2212}6.0 dB"`, `"+1.5 dB"`, `"0.0 dB"`, and `"\u{2212}\u{221E} dB"` at the bottom of
/// the range, where a fader is silent.
pub fn format_db(db: f32) -> String {
    if db <= MIN_VOLUME_DB + 0.05 {
        return "\u{2212}\u{221E} dB".to_owned();
    }
    if db >= 0.05 { fmt_val_signed(db as f64, 1, "dB") } else { fmt_val(db as f64, 1, "dB") }
}

/// A level for a peak readout: like [`format_db`] without the plus sign,
/// since a level is a position on a scale rather than a change.
pub fn format_level(db: f32) -> String {
    if db <= METER_FLOOR_DB { "\u{2212}\u{221E} dB".to_owned() } else { fmt_val(db as f64, 1, "dB") }
}

/// `"C"`, `"L50"`, `"R100"` for a pan of -1..=1.
pub fn format_pan(pan: f32) -> String {
    let percent = (pan.clamp(-1.0, 1.0) * 100.0).round() as i32;
    match percent {
        0 => "C".to_owned(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

/// How a parameter's range maps onto slider travel: by the travel of its
/// control ([`Travel::of_param`]), which is also what a line of its
/// automation is straight along. A frequency or a time moves by ratio, a
/// level that reaches silence as a fader does, the rest evenly.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ParamScale(pub Travel);

impl ParamScale {
    pub fn of(info: &ParamInfo) -> Self {
        Self(Travel::of_param(info))
    }

    /// Slider position 0..=1 for `value`.
    pub fn normalize(self, info: &ParamInfo, value: f64) -> f64 {
        self.0.position(value, info.min, info.max)
    }

    /// Parameter value at slider position `position` (0..=1), clamped and,
    /// for integer-like kinds, rounded.
    pub fn denormalize(self, info: &ParamInfo, position: f64) -> f64 {
        info.clamp(self.0.value_at(position, info.min, info.max))
    }
}

/// The part of a parameter name worth showing in a narrow column: built-in
/// devices append explanations in parentheses, which go in the tooltip.
pub fn short_param_name(name: &str) -> &str {
    name.split(" (").next().unwrap_or(name).trim()
}

/// A parameter value with its unit, at a precision that suits its size:
/// `"440 Hz"`, `"2.50 kHz"`, `"250 ms"`, `"\u{2212}6.0 dB"`, `"50 %"`, `"saw"`.
pub fn format_param(info: &ParamInfo, value: f64) -> String {
    let unit = short_unit(&info.unit);
    match &info.kind {
        ParamKind::Bool => return if value >= 0.5 { "On" } else { "Off" }.to_owned(),
        ParamKind::Enum(choices) => {
            let index = value.round().max(0.0) as usize;
            return choices.get(index).cloned().unwrap_or_else(|| format!("{index}"));
        }
        ParamKind::Int => return fmt_val(value.round(), 0, unit),
        ParamKind::Float => {}
    }
    let formatted = match unit {
        "Hz" if value >= 1000.0 => format!("{:.2} kHz", value / 1000.0),
        "Hz" => format!("{} Hz", adaptive(value)),
        "s" if value < 1.0 => format!("{} ms", adaptive(value * 1000.0)),
        "s" => format!("{value:.2} s"),
        "dB" => format_signed(value, "dB"),
        // Bipolar ranges read better with an explicit sign.
        unit if info.min < 0.0 && !unit.is_empty() => format_signed(value, unit),
        // Amounts: a feedback that stops at 0.95 is as much a share as a
        // mix that reaches 1.
        "" if info.min == 0.0 && info.max <= 1.0 => format!("{:.0} %", value * 100.0),
        "" => format!("{value:.2}"),
        // Ratios read as one word: "4.00:1".
        unit if unit.starts_with(':') => format!("{}{unit}", adaptive(value)),
        unit => format!("{} {unit}", adaptive(value)),
    };
    typeset(&formatted)
}

/// Units as they are set beside a value: the long ones abbreviated.
fn short_unit(unit: &str) -> &str {
    match unit {
        "cents" => "ct",
        other => other,
    }
}

/// One decimal with a plus on positive values; zero carries no sign.
fn format_signed(value: f64, unit: &str) -> String {
    if value >= 0.05 { fmt_val_signed(value, 1, unit) } else { fmt_val(value, 1, unit) }
}

/// Three significant figures or so: `5.00`, `44.1`, `440`.
fn adaptive(value: f64) -> String {
    let magnitude = value.abs();
    if magnitude >= 100.0 {
        format!("{value:.0}")
    } else if magnitude >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(min: f64, max: f64, unit: &str, kind: ParamKind) -> ParamInfo {
        ParamInfo { id: "p".into(), name: "P".into(), min, max, default: min, unit: unit.into(), kind }
    }

    #[test]
    fn decibels() {
        assert_eq!(gain_to_db(1.0), 0.0);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
        assert_eq!(format_db(0.0), "0.0 dB");
        assert_eq!(format_db(-0.01), "0.0 dB");
        assert_eq!(format_db(0.04), "0.0 dB", "a rounded zero carries no sign");
        assert_eq!(format_db(-6.04), "\u{2212}6.0 dB", "a real minus sign, not a hyphen");
        assert_eq!(format_db(1.5), "+1.5 dB");
        assert_eq!(format_db(MIN_VOLUME_DB), "\u{2212}\u{221E} dB");
        assert_eq!(format_level(-1.04), "\u{2212}1.0 dB");
        assert_eq!(format_level(0.3), "0.3 dB");
        assert_eq!(format_level(f32::NEG_INFINITY), "\u{2212}\u{221E} dB");
    }

    #[test]
    fn fader_law_round_trips_and_favors_the_top() {
        assert_eq!(db_to_fader(MIN_VOLUME_DB), 0.0);
        assert_eq!(db_to_fader(MAX_VOLUME_DB), 1.0);
        assert_eq!(db_to_fader(-200.0), 0.0);
        assert_eq!(db_to_fader(f32::NEG_INFINITY), 0.0, "silence is the bottom of a meter");
        assert_eq!(db_to_fader(40.0), 1.0);
        for db in [-70.0, -48.0, -24.0, -12.0, -6.0, 0.0, 3.0, 6.0] {
            assert!((fader_to_db(db_to_fader(db)) - db).abs() < 1e-3, "{db} dB");
        }
        assert!(db_to_fader(0.0) > 0.8, "unity gain is near the top");
        assert!(db_to_fader(-12.0) > 0.5, "the top half covers the last 12 dB below unity");
        assert!(db_to_fader(-48.0) < 0.1);
    }

    #[test]
    fn pan_labels() {
        assert_eq!(format_pan(0.0), "C");
        assert_eq!(format_pan(0.004), "C");
        assert_eq!(format_pan(-0.5), "L50");
        assert_eq!(format_pan(1.0), "R100");
        assert_eq!(format_pan(7.0), "R100");
    }

    #[test]
    fn frequency_ranges_are_logarithmic() {
        let cutoff = param(20.0, 20_000.0, "Hz", ParamKind::Float);
        let scale = ParamScale::of(&cutoff);
        assert_eq!(scale, ParamScale(Travel::Log));
        assert_eq!(scale.normalize(&cutoff, 20.0), 0.0);
        assert!((scale.normalize(&cutoff, 20_000.0) - 1.0).abs() < 1e-12);
        // The geometric middle of 20 Hz..20 kHz is about 632 Hz.
        assert!((scale.denormalize(&cutoff, 0.5) - 632.455).abs() < 0.01);
        for value in [20.0, 100.0, 440.0, 5000.0, 20_000.0] {
            let back = scale.denormalize(&cutoff, scale.normalize(&cutoff, value));
            assert!((back - value).abs() < 1e-6 * value);
        }
        assert_eq!(scale.denormalize(&cutoff, 7.0), 20_000.0);
    }

    #[test]
    fn other_ranges_are_linear() {
        let linear = ParamScale(Travel::Linear);
        let mix = param(0.0, 1.0, "", ParamKind::Float);
        assert_eq!(ParamScale::of(&mix), linear);
        let bipolar = param(-8.0, 8.0, "oct", ParamKind::Float);
        assert_eq!(ParamScale::of(&bipolar), linear);
        assert_eq!(linear.normalize(&bipolar, 0.0), 0.5);
        assert_eq!(linear.denormalize(&bipolar, 0.75), 4.0);
        // Integers are never logarithmic and always land on whole numbers.
        let unison = param(1.0, 7.0, "", ParamKind::Int);
        assert_eq!(ParamScale::of(&unison), linear);
        assert_eq!(linear.denormalize(&unison, 0.4), 3.0);
        let degenerate = param(1.0, 1.0, "", ParamKind::Float);
        assert_eq!(linear.normalize(&degenerate, 1.0), 0.0);
    }

    #[test]
    fn a_level_that_reaches_silence_is_laid_out_as_a_fader() {
        // A gain's knob gives the last decibels above silence little room
        // and the range where mixing happens most of it, as a fader does.
        let gain = param(-70.0, 24.0, "dB", ParamKind::Float);
        let scale = ParamScale::of(&gain);
        assert_eq!(scale, ParamScale(Travel::Fader { floor: -70.0 }));
        assert_eq!((scale.normalize(&gain, -70.0), scale.normalize(&gain, 24.0)), (0.0, 1.0));
        assert!(scale.normalize(&gain, -40.0) < 0.11 && scale.normalize(&gain, 0.0) > 0.5);
        for value in [-70.0, -30.0, -6.0, 0.0, 12.0, 24.0] {
            assert!((scale.denormalize(&gain, scale.normalize(&gain, value)) - value).abs() < 1e-9);
        }
        // An EQ's gain is a boost or a cut, not a level: even in decibels.
        let boost = param(-18.0, 18.0, "dB", ParamKind::Float);
        assert_eq!(ParamScale::of(&boost), ParamScale(Travel::Linear));
        assert_eq!(ParamScale::of(&boost).normalize(&boost, 0.0), 0.5);
    }

    #[test]
    fn values_are_formatted_with_fitting_units() {
        let hz = param(20.0, 20_000.0, "Hz", ParamKind::Float);
        assert_eq!(format_param(&hz, 440.0), "440 Hz");
        assert_eq!(format_param(&hz, 44.1), "44.1 Hz");
        assert_eq!(format_param(&hz, 2500.0), "2.50 kHz");
        let rate = param(0.01, 30.0, "Hz", ParamKind::Float);
        assert_eq!(format_param(&rate, 5.0), "5.00 Hz");
        let time = param(0.001, 10.0, "s", ParamKind::Float);
        assert_eq!(format_param(&time, 0.005), "5.00 ms");
        assert_eq!(format_param(&time, 0.25), "250 ms");
        assert_eq!(format_param(&time, 1.5), "1.50 s");
        let volume = param(-48.0, 12.0, "dB", ParamKind::Float);
        assert_eq!(format_param(&volume, -6.0), "\u{2212}6.0 dB");
        assert_eq!(format_param(&volume, 3.0), "+3.0 dB");
        assert_eq!(format_param(&volume, -0.01), "0.0 dB");
        let amount = param(-8.0, 8.0, "oct", ParamKind::Float);
        assert_eq!(format_param(&amount, 2.0), "+2.0 oct");
        assert_eq!(format_param(&amount, -1.5), "\u{2212}1.5 oct");
        assert_eq!(format_param(&amount, 0.0), "0.0 oct");
        let mix = param(0.0, 1.0, "", ParamKind::Float);
        assert_eq!(format_param(&mix, 0.5), "50 %");
        let feedback = param(0.0, 0.95, "", ParamKind::Float);
        assert_eq!(format_param(&feedback, 0.4), "40 %");
        let level = param(0.0, 2.0, "", ParamKind::Float);
        assert_eq!(format_param(&level, 1.0), "1.00");
        let detune = param(0.0, 100.0, "cents", ParamKind::Float);
        assert_eq!(format_param(&detune, 20.0), "20.0 ct");
        let bipolar_detune = param(-100.0, 100.0, "cents", ParamKind::Float);
        assert_eq!(format_param(&bipolar_detune, 7.0), "+7.0 ct");
        let ratio = param(1.0, 20.0, ":1", ParamKind::Float);
        assert_eq!(format_param(&ratio, 4.0), "4.00:1");
    }

    #[test]
    fn discrete_values_are_formatted_by_kind() {
        let semitones = param(-24.0, 24.0, "st", ParamKind::Int);
        assert_eq!(format_param(&semitones, 7.0), "7 st");
        assert_eq!(format_param(&semitones, -12.0), "\u{2212}12 st");
        assert_eq!(format_param(&semitones, -0.2), "0 st");
        let voices = param(1.0, 7.0, "", ParamKind::Int);
        assert_eq!(format_param(&voices, 3.2), "3");
        let mono = param(0.0, 1.0, "", ParamKind::Bool);
        assert_eq!(format_param(&mono, 1.0), "On");
        assert_eq!(format_param(&mono, 0.0), "Off");
        let wave = param(0.0, 2.0, "", ParamKind::Enum(vec!["sine".into(), "saw".into(), "square".into()]));
        assert_eq!(format_param(&wave, 1.0), "saw");
        assert_eq!(format_param(&wave, 9.0), "9");
    }

    #[test]
    fn parenthetical_explanations_are_dropped_from_short_names() {
        assert_eq!(short_param_name("FM Amount (osc 2 modulates osc 1)"), "FM Amount");
        assert_eq!(short_param_name("Osc Mix (0 = osc 1, 1 = osc 2)"), "Osc Mix");
        assert_eq!(short_param_name("Volume"), "Volume");
    }
}
