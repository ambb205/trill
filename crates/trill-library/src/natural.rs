//! The order names are listed in.

use std::cmp::Ordering;

/// Compares two names the way a person sorts them: letters without regard
/// to case, and runs of digits by their value, so `Kick 2` comes before
/// `Kick 10`.
///
/// It is a total order: names that differ at all never compare equal, so a
/// sorted list has one arrangement.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a.chars().peekable(), b.chars().peekable());
    loop {
        let (Some(&l), Some(&r)) = (left.peek(), right.peek()) else {
            // One is a prefix of the other, as far as this order can tell.
            return left.peek().is_some().cmp(&right.peek().is_some()).then_with(|| a.cmp(b));
        };
        let order = if l.is_ascii_digit() && r.is_ascii_digit() {
            compare_numbers(&digits(&mut left), &digits(&mut right))
        } else {
            left.next();
            right.next();
            l.to_lowercase().cmp(r.to_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// Takes the run of digits at the front of `chars`.
fn digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(digit) = chars.next_if(char::is_ascii_digit) {
        run.push(digit);
    }
    run
}

/// Compares two runs of digits as numbers of any length.
fn compare_numbers(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_the_way_people_expect() {
        let mut names = vec![
            "Kick 10", "kick 2", "Kick 1", "Snare", "clap", "Kick 02", "Kick", "Kick 2b", "808", "9 lives", "Äpfel",
            "Zebra",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "9 lives", "808", "clap", "Kick", "Kick 1", "Kick 02", "kick 2", "Kick 2b", "Kick 10", "Snare",
                "Zebra", "Äpfel"
            ]
        );
    }

    #[test]
    fn the_order_is_total() {
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_ne!(natural_cmp("a", "A"), Ordering::Equal);
        assert_ne!(natural_cmp("a1", "a01"), Ordering::Equal);
        assert_eq!(natural_cmp("a1", "a01"), natural_cmp("a01", "a1").reverse());
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("a", ""), Ordering::Greater);
        // Numbers too long for any integer type still compare by value.
        assert_eq!(natural_cmp("x99999999999999999999999", "x100000000000000000000000"), Ordering::Less);
    }
}
