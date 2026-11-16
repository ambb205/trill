//! A parameter's value from the text a plugin shows for it.
//!
//! Both formats let a plugin turn a value into text ("-6.0 dB", "SM57")
//! and text back into a value. Every plugin does the first, since its own
//! editor needs it; many do not do the second, or only for text exactly
//! as they print it. For those the value is found here, by asking what the
//! plugin shows for candidate values: the step of that name, or the value
//! whose number is the one asked for.

/// The most steps a stepped parameter may have for each to be asked about.
const MAX_STEPS: u32 = 4096;
/// How often a continuous parameter is halved in the search for a number:
/// enough to be exact to any display's last digit.
const HALVINGS: usize = 48;
/// How many values of a continuous parameter are asked about when its
/// texts are not numbers.
const SAMPLES: u32 = 256;

/// The values a parameter can take.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Values {
    /// The whole numbers from 0 to this.
    Steps(u32),
    /// Anything from the first to the second.
    Range(f64, f64),
}

/// A text as it is compared: without case, and with one space wherever
/// there is any.
fn canonical(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The number a text begins with, and what follows it (its unit, as a
/// rule), canonical. `None` for a text that does not begin with a number.
pub(crate) fn number(text: &str) -> Option<(f64, String)> {
    let text = text.trim();
    let digits = text
        .char_indices()
        .take_while(|(index, c)| c.is_ascii_digit() || *c == '.' || (*index == 0 && matches!(c, '+' | '-')))
        .last()
        .map(|(index, c)| index + c.len_utf8())?;
    let value: f64 = text[..digits].parse().ok()?;
    value.is_finite().then(|| (value, canonical(&text[digits..])))
}

/// Finds the value that `shown` (what the plugin shows for a value, if
/// anything) turns into `wanted`.
///
/// A step is found by its name, or by its number. A value in a range is
/// found by the number `wanted` begins with, provided the plugin shows
/// numbers in the same unit at both ends of the range and they run in one
/// direction; otherwise by a text that is exactly `wanted` at one of a few
/// hundred values. `None` when nothing the plugin shows is `wanted`.
pub(crate) fn find(wanted: &str, values: Values, mut shown: impl FnMut(f64) -> Option<String>) -> Option<f64> {
    let wanted_text = canonical(wanted);
    if wanted_text.is_empty() {
        return None;
    }
    let wanted_number = number(wanted);
    // Whether a shown text is the wanted one as a number: the same number
    // in the same unit, or in no unit at all.
    let same_number = |text: &str| match (&wanted_number, number(text)) {
        (Some((wanted, unit)), Some((value, shown_unit))) => {
            (unit.is_empty() || *unit == shown_unit) && (wanted - value).abs() <= 1e-9 * wanted.abs().max(1.0)
        }
        _ => false,
    };

    match values {
        Values::Steps(steps) => {
            let steps = steps.min(MAX_STEPS);
            let texts: Vec<Option<String>> = (0..=steps).map(|step| shown(f64::from(step))).collect();
            let named = |matches: &dyn Fn(&str) -> bool| {
                texts.iter().position(|text| text.as_deref().is_some_and(matches)).map(|step| step as f64)
            };
            named(&|text| canonical(text) == wanted_text).or_else(|| named(&same_number))
        }
        Values::Range(low, high) if low < high => {
            let ends = (shown(low).as_deref().and_then(number), shown(high).as_deref().and_then(number));
            if let (Some((wanted, unit)), (Some((at_low, low_unit)), Some((at_high, high_unit)))) =
                (&wanted_number, &ends)
                && low_unit == high_unit
                && (unit.is_empty() || unit == low_unit)
                && at_low != at_high
            {
                let rising = at_low < at_high;
                let (least, most) = if rising { (*at_low, *at_high) } else { (*at_high, *at_low) };
                // A hair of room: what is shown is rounded.
                let room = (most - least).abs() * 1e-6;
                if *wanted < least - room || *wanted > most + room {
                    return None;
                }
                let (mut below, mut above) = (low, high);
                for _ in 0..HALVINGS {
                    let middle = (below + above) / 2.0;
                    let Some((value, _)) = shown(middle).as_deref().and_then(number) else { break };
                    if (value < *wanted) == rising {
                        below = middle;
                    } else {
                        above = middle;
                    }
                }
                // The end the search closed in on from above shows the
                // wanted number, if any value does.
                return [above, below].into_iter().find(|value| shown(*value).as_deref().is_some_and(same_number));
            }
            (0..=SAMPLES)
                .map(|sample| low + (high - low) * f64::from(sample) / f64::from(SAMPLES))
                .find(|value| shown(*value).is_some_and(|text| canonical(&text) == wanted_text || same_number(&text)))
        }
        Values::Range(..) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_begins_with_a_number_or_it_does_not() {
        assert_eq!(number("-6.0 dB"), Some((-6.0, "db".to_string())));
        assert_eq!(number(" +3"), Some((3.0, String::new())));
        assert_eq!(number("50%"), Some((50.0, "%".to_string())));
        assert_eq!(number("1.5  kHz"), Some((1.5, "khz".to_string())));
        assert_eq!(number(".5"), Some((0.5, String::new())));
        for text in ["SM57", "", "-inf dB", "dB 6", "-", "1.2.3"] {
            assert_eq!(number(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_step_is_found_by_its_name_or_its_number() {
        let microphones = ["Ribbon 121", "Dynamic 57", "Condenser 414"];
        let shown = |value: f64| microphones.get(value as usize).map(|name| name.to_string());
        assert_eq!(find("dynamic  57", Values::Steps(2), shown), Some(1.0));
        assert_eq!(find("Condenser 414", Values::Steps(2), shown), Some(2.0));
        assert_eq!(find("Dynamic 58", Values::Steps(2), shown), None);
        assert_eq!(find("", Values::Steps(2), shown), None);

        // Steps that show numbers: "4" finds the step that shows "4 voices".
        let voices = |value: f64| Some(format!("{} voices", value as u32 + 1));
        assert_eq!(find("4", Values::Steps(15), voices), Some(3.0));
        assert_eq!(find("4 Voices", Values::Steps(15), voices), Some(3.0));
        assert_eq!(find("4 notes", Values::Steps(15), voices), None, "another unit is another thing");
        assert_eq!(find("40", Values::Steps(15), voices), None);
        // A plugin that shows nothing for a value is passed over.
        assert_eq!(find("x", Values::Steps(3), |_| None), None);
    }

    #[test]
    fn a_number_is_found_in_a_range_whichever_way_it_runs() {
        // -24 to +24 dB, shown to one decimal.
        let gain = |value: f64| Some(format!("{:.1} dB", value * 48.0 - 24.0));
        let found = find("-6 dB", Values::Range(0.0, 1.0), gain).unwrap();
        assert_eq!(gain(found).unwrap(), "-6.0 dB");
        assert!((found - 0.375).abs() < 2e-3, "{found}");
        assert_eq!(gain(find("12", Values::Range(0.0, 1.0), gain).unwrap()).unwrap(), "12.0 dB");
        assert_eq!(gain(find("24.0 DB", Values::Range(0.0, 1.0), gain).unwrap()).unwrap(), "24.0 dB");
        assert_eq!(gain(find("-24", Values::Range(0.0, 1.0), gain).unwrap()).unwrap(), "-24.0 dB");
        // Out of its range, or in another unit: not a value it has.
        assert_eq!(find("30 dB", Values::Range(0.0, 1.0), gain), None);
        assert_eq!(find("-6 %", Values::Range(0.0, 1.0), gain), None);
        assert_eq!(find("loud", Values::Range(0.0, 1.0), gain), None);

        // Falling, and curved: a cutoff shown as a whole number of Hz.
        let cutoff = |value: f64| Some(format!("{:.0} Hz", 20_000.0 * (1.0 - value).powi(3) + 20.0));
        let found = find("1000 Hz", Values::Range(0.0, 1.0), cutoff).unwrap();
        assert_eq!(cutoff(found).unwrap(), "1000 Hz");
    }

    #[test]
    fn a_range_that_shows_words_or_changes_its_unit_is_looked_through() {
        // Five positions of a switch that says it is continuous.
        let positions = ["Off", "Room", "Hall", "Plate", "Spring"];
        let shown = |value: f64| Some(positions[((value * 4.0).round() as usize).min(4)].to_string());
        let found = find("plate", Values::Range(0.0, 1.0), shown).unwrap();
        assert_eq!(shown(found).unwrap(), "Plate");
        assert_eq!(find("Cathedral", Values::Range(0.0, 1.0), shown), None);

        // Hz below a thousand and kHz above: no single run of numbers,
        // so only a text it shows exactly at one of its samples is found.
        let frequency = |value: f64| {
            let hz = 20.0 + value * 19_980.0;
            Some(if hz < 1000.0 { format!("{hz:.0} Hz") } else { format!("{:.1} kHz", hz / 1000.0) })
        };
        assert_eq!(frequency(find("20 Hz", Values::Range(0.0, 1.0), frequency).unwrap()).unwrap(), "20 Hz");
        assert_eq!(frequency(find("20.0 kHz", Values::Range(0.0, 1.0), frequency).unwrap()).unwrap(), "20.0 kHz");
        assert_eq!(find("x", Values::Range(1.0, 1.0), frequency), None, "an empty range has no values");
    }
}
