//! The per-voice building blocks shared by the polyphonic instruments: the
//! ADSR envelope and the stereo multimode filter.

use crate::util::{Svf, db_to_gain, decay_coef};

/// How far one step of velocity (of 127) moves the level of a sample
/// player at full sensitivity, in dB.
pub const VELOCITY_DB_PER_STEP: f32 = 0.4;
/// The velocity a sample player plays at its volume, whatever its
/// sensitivity: harder is louder than that, softer is quieter.
pub const VELOCITY_CENTRE: f32 = 75.0;

/// The description of a sample player's velocity sensitivity, which says
/// the law to whoever sets it.
pub const VELOCITY_SENS_NAME: &str = "Velocity Sensitivity (0 = every hit at the volume; 1 = 0.4 dB a step of velocity, up from the volume above velocity 75 and down below it, so a hard hit is louder than the volume says: +10 dB at 100, +21 dB at 127, -12 dB at 45; 0.5 is half of that in dB)";

/// The gain of a note of `velocity` (0..=1) on a sample player whose
/// sensitivity is `sens`: [`VELOCITY_DB_PER_STEP`] times the sensitivity
/// for every step from [`VELOCITY_CENTRE`], so that the same change of
/// velocity is the same change in dB wherever on the keyboard's travel it
/// is, and a hit can be louder than the volume as well as quieter.
#[inline]
pub fn velocity_gain(sens: f32, velocity: f32) -> f32 {
    db_to_gain(sens * VELOCITY_DB_PER_STEP * (velocity * 127.0 - VELOCITY_CENTRE))
}

#[derive(Copy, Clone, PartialEq)]
pub enum Stage {
    Idle,
    Attack,
    Decay,
    Release,
}

/// ADSR envelope: linear attack, exponential decay towards the sustain level
/// and exponential release.
#[derive(Copy, Clone)]
pub struct Envelope {
    pub stage: Stage,
    pub level: f32,
}

/// The rates of an [`Envelope`], derived from its times once per parameter
/// change rather than once per voice.
#[derive(Copy, Clone, Default)]
pub struct EnvShape {
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

impl EnvShape {
    pub fn new(attack: f32, decay: f32, sustain: f32, release: f32, sample_rate: f32) -> Self {
        Self {
            attack_step: 1.0 / (attack.max(0.0005) * sample_rate),
            decay_coef: decay_coef(decay, sample_rate),
            sustain,
            release_coef: decay_coef(release, sample_rate),
        }
    }
}

impl Envelope {
    pub const fn idle() -> Self {
        Self { stage: Stage::Idle, level: 0.0 }
    }

    #[inline]
    pub fn tick(&mut self, shape: &EnvShape) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += shape.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = shape.sustain + (self.level - shape.sustain) * shape.decay_coef;
            }
            Stage::Release => {
                self.level *= shape.release_coef;
                if self.level < 1e-4 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }

    pub fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }
}

/// Names of the [`FilterMode`]s, in index order, for parameter tables.
pub const FILTER_MODES: &[&str] = &["lowpass", "lowpass24", "highpass", "bandpass"];

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub enum FilterMode {
    #[default]
    LowPass,
    /// Two 12 dB stages in series.
    LowPass24,
    HighPass,
    BandPass,
}

impl FilterMode {
    /// Maps an index into [`FILTER_MODES`]; anything past the end is band-pass.
    pub fn from_index(index: usize) -> Self {
        match index {
            0 => Self::LowPass,
            1 => Self::LowPass24,
            2 => Self::HighPass,
            _ => Self::BandPass,
        }
    }
}

/// Converts a 0..=1 resonance control to the filter's Q.
#[inline]
pub fn resonance_to_q(resonance: f32) -> f32 {
    0.5 * 40f32.powf(resonance)
}

/// A voice's stereo multimode filter. The second stage only runs in the
/// 24 dB mode, with a fixed gentle Q so resonance is not doubled.
#[derive(Copy, Clone, Default)]
pub struct VoiceFilter {
    /// `[stage][channel]`.
    stages: [[Svf; 2]; 2],
}

impl VoiceFilter {
    pub fn set(&mut self, cutoff_hz: f32, q: f32, sample_rate: f32) {
        for channel in 0..2 {
            self.stages[0][channel].set(cutoff_hz, q, sample_rate);
            self.stages[1][channel].set(cutoff_hz, 0.6, sample_rate);
        }
    }

    #[inline]
    pub fn tick(&mut self, mode: FilterMode, left: f32, right: f32) -> (f32, f32) {
        let [first, second] = &mut self.stages;
        let (l, r) = (first[0].tick(left), first[1].tick(right));
        match mode {
            FilterMode::LowPass => (l.low, r.low),
            FilterMode::LowPass24 => (second[0].tick(l.low).low, second[1].tick(r.low).low),
            FilterMode::HighPass => (l.high, r.high),
            FilterMode::BandPass => (l.band, r.band),
        }
    }

    pub fn reset(&mut self) {
        for stage in &mut self.stages {
            for filter in stage {
                filter.reset();
            }
        }
    }
}

/// A short linear fade to silence, for a voice that has to stop at once
/// (stolen for a new note, choked by another drum) without a click.
#[derive(Copy, Clone)]
pub struct FadeOut {
    gain: f32,
    /// Subtracted from the gain every frame; zero while not fading.
    step: f32,
}

impl FadeOut {
    pub const fn idle() -> Self {
        Self { gain: 1.0, step: 0.0 }
    }

    /// Starts fading over `seconds`. A fade already under way is only ever
    /// made faster, never restarted or slowed.
    pub fn start(&mut self, seconds: f32, sample_rate: f32) {
        self.step = self.step.max(1.0 / (seconds * sample_rate).max(1.0));
    }

    pub fn fading(&self) -> bool {
        self.step > 0.0
    }

    /// The gain reached so far.
    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn finished(&self) -> bool {
        self.gain <= 0.0
    }

    #[inline]
    pub fn tick(&mut self) -> f32 {
        self.gain = (self.gain - self.step).max(0.0);
        self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::gain_to_db;

    #[test]
    fn fade_out_reaches_silence_in_the_given_time_and_only_speeds_up() {
        let mut fade = FadeOut::idle();
        assert!(!fade.fading() && fade.tick() == 1.0);
        fade.start(0.01, 48_000.0);
        fade.start(1.0, 48_000.0);
        let frames = (0..1_000).take_while(|_| fade.tick() > 0.0).count();
        assert!((479..=481).contains(&frames), "took {frames} frames");
        assert!(fade.finished());
    }

    #[test]
    fn envelope_runs_through_its_stages() {
        let shape = EnvShape::new(0.01, 0.1, 0.5, 0.05, 48_000.0);
        let mut envelope = Envelope::idle();
        assert_eq!(envelope.tick(&shape), 0.0);
        envelope.stage = Stage::Attack;
        let attack = (0..10_000).take_while(|_| envelope.tick(&shape) < 1.0).count();
        assert!((478..=481).contains(&attack), "attack took {attack} frames");
        for _ in 0..48_000 {
            envelope.tick(&shape);
        }
        assert!((envelope.level - 0.5).abs() < 1e-3);
        envelope.release();
        let release = (0..48_000).take_while(|_| envelope.tick(&shape) > 0.0).count();
        assert!(envelope.stage == Stage::Idle && release < 4_000);
    }

    #[test]
    fn velocity_moves_the_level_evenly_in_decibels_about_velocity_75() {
        let db = |sens: f32, velocity: u8| gain_to_db(velocity_gain(sens, f32::from(velocity) / 127.0));
        // The five velocities and the two sensitivities a hi-hat and a clap
        // were measured at in the program this law is shared with.
        for (velocity, at_80) in [(46, -9.3), (60, -4.8), (75, 0.0), (100, 8.0), (110, 11.2)] {
            assert!((db(0.8, velocity) - at_80).abs() < 0.05, "velocity {velocity}: {}", db(0.8, velocity));
        }
        assert!((db(0.35, 108) - 4.62).abs() < 0.01 && (db(0.35, 118) - 6.02).abs() < 0.01);
        // A step is the same anywhere, and scales with the sensitivity.
        for sens in [0.25f32, 0.5, 1.0] {
            for velocity in [1u8, 30, 74, 75, 126] {
                let step = db(sens, velocity + 1) - db(sens, velocity);
                assert!((step - 0.4 * sens).abs() < 1e-3, "{sens} at {velocity}: {step}");
            }
        }
        assert_eq!(velocity_gain(0.0, 0.1), 1.0, "no sensitivity: every hit at the volume");
        assert!((velocity_gain(1.0, 75.0 / 127.0) - 1.0).abs() < 1e-6);
    }
}
