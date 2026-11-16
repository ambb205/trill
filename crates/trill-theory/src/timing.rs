//! Musical time: where the beats of a bar fall, and how durations are
//! written.
//!
//! As everywhere in Trill, a "beat" is a quarter note whatever the time
//! signature, so a bar of 6/8 is 3 beats long and a bar of 7/8 is 3.5.

use crate::error::{Error, Result};
use trill_core::TimeSignature;

/// Tolerance when comparing positions in beats.
pub(crate) const EPSILON: f64 = 1e-6;

/// The starts of the main beats of one bar, in beats from the bar line.
///
/// In simple time these are the counted beats (four in 4/4). In compound
/// time they are the dotted beats (two in 6/8, four in 12/8), and in the odd
/// eighth-note meters the bar is grouped in twos with a three at the end
/// (7/8 is 2+2+3), which is how such bars are normally felt.
pub fn pulses(signature: TimeSignature) -> Vec<f64> {
    let unit = 4.0 / signature.denominator as f64;
    let count = signature.numerator as usize;
    let groups: Vec<usize> = if signature.denominator < 8 || count < 3 {
        vec![1; count]
    } else if count.is_multiple_of(3) {
        vec![3; count / 3]
    } else if count.is_multiple_of(2) {
        vec![2; count / 2]
    } else {
        let mut groups = vec![2; (count - 3) / 2];
        groups.push(3);
        groups
    };
    let mut position = 0.0;
    groups
        .into_iter()
        .map(|group| {
            let start = position;
            position += group as f64 * unit;
            start
        })
        .collect()
}

/// How strongly a position in the bar is felt: 1 on the downbeat, 0.5 on the
/// other pulses, 0 in between.
pub fn metric_weight(beat_in_bar: f64, signature: TimeSignature) -> f64 {
    let on = |pulse: &f64| (pulse - beat_in_bar).abs() < 1e-3;
    match pulses(signature).iter().position(on) {
        Some(0) => 1.0,
        Some(_) => 0.5,
        None => 0.0,
    }
}

/// Durations a hair away from a third or a sixth of a beat are meant as
/// triplets: `0.33` cannot be written exactly, so it is read as 1/3.
pub fn snap_triplet(beats: f64) -> f64 {
    let sixths = (beats * 6.0).round();
    if (beats * 6.0 - sixths).abs() < 0.03 { sixths / 6.0 } else { beats }
}

/// Parses a duration in beats: `"1"`, `"0.5"`, `".25"`, `"0.33"` (a triplet
/// eighth). `None` unless it is a positive number.
pub fn parse_beats(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    (value.is_finite() && value > 0.0).then(|| snap_triplet(value))
}

/// Parses a note value into beats: `"1/4"` is 1, `"1/8"` 0.5, `"1/16"` 0.25,
/// with `t` for a triplet (`"1/8t"` is a third of a beat) and `d` or `.` for
/// a dotted note (`"1/8d"` is 0.75).
pub fn note_value(text: &str) -> Result<f64> {
    let invalid = || {
        Error::new(format!(
            "'{text}' is not a note value; expected one like 1/4, 1/8, 1/16, 1/8t (triplet) or 1/8d (dotted)"
        ))
    };
    let trimmed = text.trim().to_ascii_lowercase();
    let (body, factor) = if let Some(body) = trimmed.strip_suffix('t') {
        (body, 2.0 / 3.0)
    } else if let Some(body) = trimmed.strip_suffix(['d', '.']) {
        (body, 1.5)
    } else {
        (trimmed.as_str(), 1.0)
    };
    let division: u32 = body.trim().strip_prefix("1/").ok_or_else(invalid)?.parse().map_err(|_| invalid())?;
    if !(1..=64).contains(&division) || !division.is_power_of_two() {
        return Err(invalid());
    }
    Ok(4.0 / division as f64 * factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(numerator: u8, denominator: u8) -> TimeSignature {
        TimeSignature::new(numerator, denominator).unwrap()
    }

    #[test]
    fn pulses_follow_the_meter() {
        let cases: &[((u8, u8), &[f64])] = &[
            ((4, 4), &[0.0, 1.0, 2.0, 3.0]),
            ((3, 4), &[0.0, 1.0, 2.0]),
            ((2, 2), &[0.0, 2.0]),
            ((6, 8), &[0.0, 1.5]),
            ((9, 8), &[0.0, 1.5, 3.0]),
            ((12, 8), &[0.0, 1.5, 3.0, 4.5]),
            ((7, 8), &[0.0, 1.0, 2.0]),
            ((5, 8), &[0.0, 1.0]),
            ((5, 4), &[0.0, 1.0, 2.0, 3.0, 4.0]),
            ((3, 8), &[0.0]),
            ((4, 8), &[0.0, 1.0]),
            ((7, 16), &[0.0, 0.5, 1.0]),
        ];
        for ((numerator, denominator), expected) in cases {
            assert_eq!(pulses(signature(*numerator, *denominator)), *expected, "{numerator}/{denominator}");
        }
    }

    #[test]
    fn downbeats_weigh_most() {
        let four = signature(4, 4);
        assert_eq!(metric_weight(0.0, four), 1.0);
        assert_eq!(metric_weight(2.0, four), 0.5);
        assert_eq!(metric_weight(2.5, four), 0.0);
        assert_eq!(metric_weight(1.5, signature(6, 8)), 0.5);
        assert_eq!(metric_weight(1.0, signature(6, 8)), 0.0);
    }

    #[test]
    fn durations_parse_and_triplets_are_exact() {
        assert_eq!(parse_beats("1"), Some(1.0));
        assert_eq!(parse_beats(".5"), Some(0.5));
        assert_eq!(parse_beats("0.33"), Some(1.0 / 3.0));
        assert_eq!(parse_beats("0.667"), Some(2.0 / 3.0));
        assert_eq!(parse_beats("0.1667"), Some(1.0 / 6.0));
        assert_eq!(parse_beats("0.75"), Some(0.75));
        assert_eq!(parse_beats("0.3"), Some(0.3), "only values that round to a third are snapped");
        for bad in ["0", "-1", "x", "", "inf", "NaN", "1/3"] {
            assert_eq!(parse_beats(bad), None, "{bad}");
        }
    }

    #[test]
    fn note_values_are_fractions_of_a_whole_note() {
        assert_eq!(note_value("1/4").unwrap(), 1.0);
        assert_eq!(note_value("1/8").unwrap(), 0.5);
        assert_eq!(note_value("1/16").unwrap(), 0.25);
        assert_eq!(note_value("1/32").unwrap(), 0.125);
        assert_eq!(note_value("1/8t").unwrap(), 1.0 / 3.0);
        assert_eq!(note_value("1/16T").unwrap(), 1.0 / 6.0);
        assert_eq!(note_value("1/8d").unwrap(), 0.75);
        assert_eq!(note_value("1/4.").unwrap(), 1.5);
        assert_eq!(note_value("1/1").unwrap(), 4.0);
        for bad in ["", "16", "1/3", "1/0", "2/4", "1/128", "sixteenth"] {
            let error = note_value(bad).unwrap_err();
            assert!(error.message().contains(&format!("'{bad}'")), "{error}");
        }
    }
}
