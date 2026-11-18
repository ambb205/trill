//! Writes a small recording and a study of it, for screenshots of the
//! study workspace and for trying it without studying anything:
//!
//! ```sh
//! cargo run -p trill-app --example study_fixtures -- crates/trill-app/screenshots/fixtures/study
//! ```
//!
//! The recording is synthesized here from sines and noise, the same on
//! every run: sixteen bars of hats, a bass line and a voice of plain tones,
//! so nothing in the pictures is anybody's recording. It is studied by the
//! scripted models the tests use, which split by register and "sing" the
//! words they are handed: no model is fetched or run. `<DIR>/profile` is
//! then a folder to start the window on (`--profile`), which finds the
//! study in it; the study's id is printed, for `--study`.

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use trill_core::audio::AudioBuffer;
use trill_media::{WavFormat, write_wav};
use trill_study::models::Which;
use trill_study::models::scripted::Script;
use trill_study::run::Programs;
use trill_study::{Source, Studies, StudyOptions};

const RATE: f64 = 44_100.0;
/// Seconds to a beat: 112 beats a minute.
const BEAT: f64 = 60.0 / 112.0;
const BARS: usize = 16;

/// Adds a note of MIDI pitch `note`, with so many harmonics.
fn tone(out: &mut [f32], at: f64, seconds: f64, note: f64, level: f32, harmonics: usize) {
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
fn hat(out: &mut [f32], at: f64, level: f32, seed: &mut u32) {
    let start = (at * RATE) as usize;
    let mut previous = 0.0f32;
    for index in 0..(0.045 * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        let hiss = (*seed >> 8) as f32 / 8_388_608.0 - 1.0;
        *slot += level * (hiss - previous) * (-(index as f32) / (0.009 * RATE as f32)).exp();
        previous = hiss;
    }
}

/// Sixteen bars: four of the bass alone with the hats, eight with a voice
/// that sings a line in every other bar, and four where the hats rest.
fn recording(path: &Path) -> Result<(), String> {
    let lead = 0.4;
    let mut out = vec![0.0f32; ((lead + (BARS * 4) as f64 * BEAT + 0.8) * RATE) as usize];
    let mut seed = 2_026u32;
    let bass = [38.0, 38.0, 45.0, 41.0, 36.0, 36.0, 43.0, 40.0];
    let sung = [[69.0, 72.0, 74.0, 72.0], [72.0, 74.0, 77.0, 76.0]];
    for bar in 0..BARS {
        let start = lead + (bar * 4) as f64 * BEAT;
        if bar < 12 {
            for eighth in 0..8 {
                let level = if eighth % 2 == 0 { 0.5 } else { 0.3 };
                hat(&mut out, start + eighth as f64 * BEAT / 2.0, level, &mut seed);
            }
        }
        for beat in 0..4 {
            tone(&mut out, start + beat as f64 * BEAT, BEAT * 0.9, bass[(bar % 2) * 4 + beat], 0.5, 2);
        }
        if (4..12).contains(&bar) && bar % 2 == 0 {
            for (index, note) in sung[(bar / 2) % 2].into_iter().enumerate() {
                tone(&mut out, start + index as f64 * BEAT, BEAT * 0.8, note, 0.3, 4);
            }
        }
    }
    let parent = path.parent().ok_or("the recording needs a folder")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let audio = AudioBuffer { sample_rate: RATE, left: out.clone(), right: out };
    write_wav(path, &audio, WavFormat::Int24).map_err(|error| error.to_string())
}

fn main() -> Result<(), String> {
    let folder = PathBuf::from(std::env::args().nth(1).ok_or("give the folder to write into")?);
    let recorded = folder.join("recordings").join("night shift (demo).wav");
    recording(&recorded)?;
    // One study, made afresh: a picture names it by the id printed here.
    let kept = folder.join("profile").join("studies");
    if kept.is_dir() {
        std::fs::remove_dir_all(&kept).map_err(|error| format!("{}: {error}", kept.display()))?;
    }
    let (study_folder, study) =
        Studies::new(&kept).create("Night shift (demo)", Source::File { path: recorded }, StudyOptions::default())?;
    let words = ["all", "night", "we", "drive", "till", "day", "is", "near"].map(String::from).to_vec();
    let mut models = Which::Scripted(Script { words, ..Script::default() }).make()?;
    trill_study::run::run(&study_folder, &mut *models, &Programs::default(), &mut |_| {})?;
    println!("{}", study.id);
    Ok(())
}
