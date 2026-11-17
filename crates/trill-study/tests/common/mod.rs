//! What the tests of a study share: the program that studies for them,
//! and a small recording made of tones, which no model is needed to split.
#![allow(dead_code)]

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use trill_core::audio::AudioBuffer;
use trill_media::{WavFormat, write_wav};
use trill_study::models::scripted::Script;

pub const RATE: f64 = 44_100.0;

pub fn program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trill-study-job"))
}

/// Adds a note of MIDI pitch `note`, with so many harmonics.
pub fn tone(out: &mut [f32], at: f64, seconds: f64, note: f64, level: f32, harmonics: usize) {
    let hertz = 440.0 * 2f64.powf((note - 69.0) / 12.0);
    let start = (at * RATE) as usize;
    for index in 0..(seconds * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        let time = index as f64 / RATE;
        let fade = (time / 0.004).min(1.0) * ((seconds - time) / 0.012).clamp(0.0, 1.0);
        let wave: f64 = (1..=harmonics).map(|k| (TAU * hertz * k as f64 * time).sin() / (k * k) as f64).sum();
        *slot += level * (wave * fade) as f32;
    }
}

/// Adds a tick of bright noise: a hat.
pub fn hat(out: &mut [f32], at: f64, seed: &mut u32) {
    let start = (at * RATE) as usize;
    let mut previous = 0.0f32;
    for index in 0..(0.045 * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        let hiss = (*seed >> 8) as f32 / 8_388_608.0 - 1.0;
        *slot += 0.5 * (hiss - previous) * (-(index as f32) / (0.009 * RATE as f32)).exp();
        previous = hiss;
    }
}

/// Eight bars at 100 beats a minute: hats on the eighths, a bass note to
/// the beat, and a voice that sings four notes in every other bar.
pub fn recording(path: &Path) {
    let beat = 0.6;
    let mut out = vec![0.0f32; ((0.3 + 32.0 * beat + 0.6) * RATE) as usize];
    let mut seed = 77u32;
    for index in 0..64 {
        hat(&mut out, 0.3 + index as f64 * beat / 2.0, &mut seed);
    }
    for index in 0..32 {
        // A bass with nothing over 180 Hz, which is where the scripted split has it.
        tone(&mut out, 0.3 + index as f64 * beat, beat * 0.9, [33.0, 33.0, 40.0, 36.0][index % 4], 0.5, 2);
    }
    for bar in (0..8).step_by(2) {
        for (index, note) in [69.0, 72.0, 76.0, 72.0].into_iter().enumerate() {
            tone(&mut out, 0.3 + (bar * 4 + index) as f64 * beat, beat * 0.8, note, 0.3, 4);
        }
    }
    write_wav(path, &AudioBuffer { sample_rate: RATE, left: out.clone(), right: out }, WavFormat::Int24).unwrap();
}

pub fn script() -> Script {
    Script { words: ["one", "two", "three", "four"].map(String::from).to_vec(), ..Script::default() }
}
