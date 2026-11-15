//! Number formatting and parsing, as the kit's `core/vectors` pin them:
//! fixed decimals, a real minus sign, and a zero that is never negative.

use crate::text::MINUS;

/// `value` at `decimals` places, followed by `suffix` after a space when
/// there is one: `"−18.0 dB"`, `"240 ms"`, `"0.35"`.
pub fn fmt_val(value: f64, decimals: usize, suffix: &str) -> String {
    with_suffix(typeset(&format!("{value:.decimals$}")), suffix)
}

/// Like [`fmt_val`] with an explicit sign, for values that go both ways:
/// `"+2.0 dB"`, `"−2.0 dB"`, `"+0.0 dB"`.
pub fn fmt_val_signed(value: f64, decimals: usize, suffix: &str) -> String {
    let number = typeset(&format!("{value:.decimals$}"));
    let number = if number.starts_with(MINUS) { number } else { format!("+{number}") };
    with_suffix(number, suffix)
}

/// Replaces ASCII hyphens used as minus signs with U+2212 in text that was
/// formatted elsewhere, and drops the sign of a zero: `"-0.0 dB"` becomes
/// `"0.0 dB"`.
pub fn typeset(text: &str) -> String {
    let Some(rest) = text.strip_prefix('-') else { return text.to_owned() };
    let number_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let is_zero = rest[..number_end].bytes().all(|byte| byte == b'0' || byte == b'.');
    if number_end > 0 && is_zero { rest.to_owned() } else { format!("{MINUS}{rest}") }
}

fn with_suffix(number: String, suffix: &str) -> String {
    if suffix.is_empty() { number } else { format!("{number} {suffix}") }
}

/// Reads the number at the start of what a user typed, clamped to
/// `low..=high`: `" 240 ms "` is 240, `"−18.5"` is -18.5. `None` when the
/// text does not begin with a number.
pub fn parse_value(text: &str, low: f64, high: f64) -> Option<f64> {
    let (low, high) = (low.min(high), low.max(high));
    let text = text.trim().replace(MINUS, "-");
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(&text);
    let digits = unsigned.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(unsigned.len());
    let sign = text.len() - unsigned.len();
    let value: f64 = text[..sign + digits].parse().ok()?;
    value.is_finite().then(|| value.clamp(low, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cases of the kit's `core/vectors/format.json` and `parse.json`.

    #[test]
    fn values_are_formatted_as_the_kit_pins_them() {
        assert_eq!(fmt_val(-18.0, 1, "DB"), "\u{2212}18.0 DB");
        assert_eq!(fmt_val(240.0, 0, "MS"), "240 MS");
        assert_eq!(fmt_val(0.35, 2, ""), "0.35");
        assert_eq!(fmt_val(-0.0, 1, "DB"), "0.0 DB");
        assert_eq!(fmt_val(-0.04, 1, ""), "0.0");
        assert_eq!(fmt_val(1234.5678, 2, "HZ"), "1234.57 HZ");
        assert_eq!(fmt_val(-3.14259, 3, ""), "\u{2212}3.143");
    }

    #[test]
    fn signed_values_always_show_their_sign() {
        assert_eq!(fmt_val_signed(2.0, 1, "DB"), "+2.0 DB");
        assert_eq!(fmt_val_signed(-2.0, 1, "DB"), "\u{2212}2.0 DB");
        assert_eq!(fmt_val_signed(0.0, 1, "DB"), "+0.0 DB");
        assert_eq!(fmt_val_signed(-0.0, 1, "DB"), "+0.0 DB");
        assert_eq!(fmt_val_signed(-0.04, 1, ""), "+0.0");
        assert_eq!(fmt_val_signed(0.75, 0, ""), "+1");
    }

    #[test]
    fn text_formatted_elsewhere_gets_a_real_minus() {
        assert_eq!(typeset("-6.0 dB"), "\u{2212}6.0 dB");
        assert_eq!(typeset("-0.0 dB"), "0.0 dB");
        assert_eq!(typeset("-0"), "0");
        assert_eq!(typeset("-inf dB"), "\u{2212}inf dB");
        assert_eq!(typeset("+1.5 dB"), "+1.5 dB");
        assert_eq!(typeset("saw-tooth"), "saw-tooth", "only a leading sign is a minus");
    }

    #[test]
    fn typed_values_are_parsed_and_clamped() {
        assert_eq!(parse_value("240", 30.0, 1500.0), Some(240.0));
        assert_eq!(parse_value("  240 ms ", 30.0, 1500.0), Some(240.0));
        assert_eq!(parse_value("-18.5 DB", -60.0, 0.0), Some(-18.5));
        assert_eq!(parse_value("\u{2212}18.5", -60.0, 0.0), Some(-18.5));
        assert_eq!(parse_value("9999", 30.0, 1500.0), Some(1500.0));
        assert_eq!(parse_value("-100", -60.0, 0.0), Some(-60.0));
        assert_eq!(parse_value("+3", 0.0, 24.0), Some(3.0));
        assert_eq!(parse_value(".5", 0.0, 1.0), Some(0.5));
        assert_eq!(parse_value("abc", 0.0, 1.0), None);
        assert_eq!(parse_value("", 0.0, 1.0), None);
        assert_eq!(parse_value("-", -1.0, 1.0), None);
        assert_eq!(parse_value("5", 10.0, 0.0), Some(5.0), "bounds may come in either order");
    }
}
