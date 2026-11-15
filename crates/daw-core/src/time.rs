//! Musical time. Positions and lengths are measured in beats (quarter notes).

use serde::{Deserialize, Serialize};

/// A position or duration in quarter notes.
pub type Beats = f64;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self { numerator: 4, denominator: 4 }
    }
}

impl TimeSignature {
    pub fn new(numerator: u8, denominator: u8) -> Option<Self> {
        let valid = (1..=32).contains(&numerator) && matches!(denominator, 1 | 2 | 4 | 8 | 16);
        valid.then_some(Self { numerator, denominator })
    }

    /// Length of one bar in quarter notes (4/4 → 4, 6/8 → 3, 7/8 → 3.5).
    pub fn beats_per_bar(self) -> Beats {
        self.numerator as f64 * 4.0 / self.denominator as f64
    }
}

/// Seconds per beat at `tempo` BPM.
pub fn seconds_per_beat(tempo: f64) -> f64 {
    60.0 / tempo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_lengths() {
        assert_eq!(TimeSignature::default().beats_per_bar(), 4.0);
        assert_eq!(TimeSignature::new(6, 8).unwrap().beats_per_bar(), 3.0);
        assert_eq!(TimeSignature::new(7, 8).unwrap().beats_per_bar(), 3.5);
        assert!(TimeSignature::new(4, 3).is_none());
        assert!(TimeSignature::new(0, 4).is_none());
    }
}
