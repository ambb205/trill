//! Feel: swing, dynamics and the small deviations that stop programmed
//! notes sounding programmed. All of it is deterministic; what varies does
//! so from a seed.

use crate::error::{Error, Result};
use crate::timing::metric_weight;
use trill_core::{Note, TimeSignature};

/// A small deterministic random number generator (SplitMix64). The same
/// seed always produces the same sequence, on every platform.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in -1..1.
    pub fn signed(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    /// Uniform in `0..count`; 0 when `count` is 0.
    pub fn below(&mut self, count: usize) -> usize {
        if count == 0 { 0 } else { (self.next_u64() % count as u64) as usize }
    }
}

/// How loud a hit is relative to the rest of its part.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// Barely there: the notes between the notes.
    Ghost,
    Normal,
    Accent,
}

impl Level {
    /// The velocity of a hit at this level in a part whose normal hits are
    /// at `base`.
    pub fn velocity(self, base: f64) -> u8 {
        clamp_velocity(match self {
            Level::Ghost => base * 0.42,
            Level::Normal => base,
            Level::Accent => base * 1.18,
        })
    }
}

/// Rounds to a valid MIDI velocity, never silent.
pub fn clamp_velocity(velocity: f64) -> u8 {
    velocity.round().clamp(1.0, 127.0) as u8
}

/// The velocity plan for a pitched part: a base level, lifted on the beats
/// the meter stresses.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Dynamics {
    /// Velocity of a note between the beats.
    pub velocity: u8,
    /// Added on the downbeat of each bar; half of it on the other beats.
    pub accent: u8,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self { velocity: 84, accent: 10 }
    }
}

impl Dynamics {
    /// The velocity of a note starting at `beat` of the part.
    pub fn at(&self, beat: f64, signature: TimeSignature) -> f64 {
        let bar = signature.beats_per_bar();
        self.velocity as f64 + self.accent as f64 * metric_weight(beat.rem_euclid(bar), signature)
    }
}

/// Swing: every second step of a grid is played late. `percent` is where in
/// each pair of steps the second one lands, the way drum machines count it:
/// 50 is straight, 54-60 a light shuffle, 66.7 a full triplet swing, 75 a
/// dotted feel.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Swing {
    pub percent: f64,
    /// Length in beats of the steps that swing: 0.25 swings sixteenths
    /// (house, hip-hop, garage), 0.5 swings eighths (jazz, blues, shuffle).
    pub unit: f64,
}

impl Swing {
    pub fn new(percent: f64, unit: f64) -> Result<Self> {
        if !percent.is_finite() || !(50.0..=75.0).contains(&percent) {
            return Err(Error::new(format!(
                "swing {percent} is outside 50..=75; 50 is straight, 54-60 a light shuffle, 66.7 triplet swing"
            )));
        }
        if !unit.is_finite() || unit <= 0.0 {
            return Err(Error::new(format!("swing unit {unit} must be a positive number of beats, such as 0.25")));
        }
        Ok(Self { percent, unit })
    }

    /// Where a straight position lands once swung. Time between the grid
    /// points is stretched with them, so rolls and strums keep their order.
    pub fn warp(&self, beat: f64) -> f64 {
        let pair = self.unit * 2.0;
        let start = (beat / pair).floor() * pair;
        let within = beat - start;
        let ratio = self.percent / 100.0;
        let swung = if within < self.unit {
            within * 2.0 * ratio
        } else {
            ratio * pair + (within - self.unit) * 2.0 * (1.0 - ratio)
        };
        start + swung
    }

    /// Swings notes in place. `origin` is the song position of beat 0 of the
    /// notes, so a clip that does not start on the grid still swings with
    /// the rest of the song.
    pub fn apply(&self, notes: &mut [Note], origin: f64) {
        for note in notes {
            let start = (self.warp(note.start + origin) - origin).max(0.0);
            let end = self.warp(note.end() + origin) - origin;
            note.length = (end - start).max(MIN_LENGTH);
            note.start = start;
        }
    }
}

/// Shortest note any generator writes.
pub(crate) const MIN_LENGTH: f64 = 1.0 / 64.0;

/// Random variation in timing and velocity.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Humanize {
    /// Notes move by up to this many beats either way (0.01-0.03 is human).
    pub timing: f64,
    /// Velocities change by up to this much either way (4-12 is human).
    pub velocity: f64,
    pub seed: u64,
}

impl Humanize {
    pub fn apply(&self, notes: &mut [Note]) {
        let mut rng = Rng::new(self.seed);
        for note in notes {
            // Drawn in a fixed order so adding timing does not change the velocities.
            let (timing, velocity) = (rng.signed(), rng.signed());
            note.start = (note.start + timing * self.timing).max(0.0);
            note.velocity = clamp_velocity(note.velocity as f64 + velocity * self.velocity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f64, length: f64) -> Note {
        Note { pitch: 60, start, length, velocity: 100, ..Default::default() }
    }

    #[test]
    fn the_generator_is_deterministic_and_spread_out() {
        let draw = |seed: u64| {
            let mut rng = Rng::new(seed);
            (0..1000).map(|_| rng.unit()).collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
        let values = draw(1);
        assert!(values.iter().all(|value| (0.0..1.0).contains(value)));
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        assert!((mean - 0.5).abs() < 0.05, "{mean}");
        let mut rng = Rng::new(3);
        assert!((0..100).all(|_| rng.below(5) < 5));
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn levels_scale_velocity() {
        assert_eq!(Level::Normal.velocity(100.0), 100);
        assert_eq!(Level::Accent.velocity(100.0), 118);
        assert_eq!(Level::Ghost.velocity(100.0), 42);
        assert_eq!(Level::Accent.velocity(120.0), 127);
        assert_eq!(Level::Ghost.velocity(1.0), 1);
    }

    #[test]
    fn dynamics_stress_the_beats() {
        let four = TimeSignature::default();
        let dynamics = Dynamics { velocity: 80, accent: 12 };
        assert_eq!(dynamics.at(0.0, four), 92.0);
        assert_eq!(dynamics.at(8.0, four), 92.0);
        assert_eq!(dynamics.at(1.0, four), 86.0);
        assert_eq!(dynamics.at(1.5, four), 80.0);
    }

    #[test]
    fn swing_delays_the_offbeats_only() {
        let swing = Swing::new(66.0 + 2.0 / 3.0, 0.5).unwrap();
        assert_eq!(swing.warp(0.0), 0.0);
        assert_eq!(swing.warp(1.0), 1.0);
        assert!((swing.warp(0.5) - 2.0 / 3.0).abs() < 1e-9, "the offbeat eighth becomes the last triplet");
        assert!((swing.warp(3.5) - (3.0 + 2.0 / 3.0)).abs() < 1e-9);
        // Order is preserved everywhere.
        let mut previous = -1.0;
        for step in 0..64 {
            let warped = swing.warp(step as f64 / 16.0);
            assert!(warped > previous);
            previous = warped;
        }
        let straight = Swing::new(50.0, 0.25).unwrap();
        for step in 0..32 {
            let beat = step as f64 / 8.0;
            assert!((straight.warp(beat) - beat).abs() < 1e-12);
        }
        assert!(Swing::new(40.0, 0.25).unwrap_err().message().contains("50..=75"));
        assert!(Swing::new(60.0, 0.0).is_err());
    }

    #[test]
    fn swing_is_measured_from_the_song_grid() {
        let swing = Swing::new(60.0, 0.25).unwrap();
        let mut notes = [note(0.0, 0.25), note(0.25, 0.25), note(0.5, 0.25)];
        swing.apply(&mut notes, 0.0);
        assert!((notes[1].start - 0.3).abs() < 1e-9);
        assert!((notes[0].length - 0.3).abs() < 1e-9, "the note before a swung one stretches to meet it");
        assert_eq!(notes[2].start, 0.5);
        // The same notes in a clip that starts a sixteenth late: now the first one is off the beat.
        let mut shifted = [note(0.0, 0.25), note(0.25, 0.25)];
        swing.apply(&mut shifted, 0.25);
        assert!((shifted[0].start - 0.05).abs() < 1e-9);
        assert!((shifted[1].start - 0.25).abs() < 1e-9);
    }

    #[test]
    fn humanizing_is_bounded_and_repeatable() {
        let humanize = Humanize { timing: 0.02, velocity: 8.0, seed: 11 };
        let fresh = || (0..200).map(|index| note(index as f64 * 0.25, 0.2)).collect::<Vec<_>>();
        let (mut first, mut second) = (fresh(), fresh());
        humanize.apply(&mut first);
        humanize.apply(&mut second);
        assert_eq!(first, second);
        assert_ne!(first, fresh());
        for (index, note) in first.iter().enumerate() {
            assert!((note.start - index as f64 * 0.25).abs() <= 0.02 + 1e-12);
            assert!((92..=108).contains(&note.velocity));
            assert!(note.start >= 0.0);
        }
        let mut other = fresh();
        Humanize { seed: 12, ..humanize }.apply(&mut other);
        assert_ne!(first, other);
    }
}
