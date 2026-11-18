//! Models for tests: each does something plain and the same every time,
//! in place of a network, so that a whole study can be run through the
//! real process, the real stages and the real files with nothing fetched.
//!
//! They can also be told to go wrong the ways real ones do: to fail with
//! a sentence, to take the process down, to take more memory than the run
//! said it needs, or to take their time.

use super::{Models, Progress, Said, Stretch, Transcript};
use crate::lyrics::Word;
use crate::store::{Stage, StemChoice};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use trill_listen::hits::band;
use trill_listen::onsets::onsets;
use trill_listen::{RATE, Sound, Stem, TrackedBeats};

/// What the scripted models are told to do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Script {
    /// The words that are "sung": one for each note the vocal starts, in
    /// order, round and round.
    #[serde(default)]
    pub words: Vec<String>,
    /// A stage whose model says it cannot.
    #[serde(default)]
    pub fail_at: Option<Stage>,
    /// A stage whose model takes the process down.
    #[serde(default)]
    pub crash_at: Option<Stage>,
    /// A stage whose model takes memory until it is stopped.
    #[serde(default)]
    pub hog_at: Option<Stage>,
    /// How long each model takes, in seconds, on top of its work.
    #[serde(default)]
    pub seconds: f64,
    /// A bar the beat model "finds" two beats long, counted from 0 at the
    /// first downbeat it tracks: every downbeat after it comes two beats
    /// sooner. What a real model does at some transitions.
    #[serde(default)]
    pub short_bar: Option<usize>,
}

pub struct ScriptedModels {
    script: Script,
}

impl ScriptedModels {
    pub fn new(script: Script) -> Self {
        Self { script }
    }

    /// Goes wrong at `stage` the way the script says, or takes its time.
    fn begin(&self, stage: Stage, progress: Progress) -> Result<(), String> {
        if self.script.fail_at == Some(stage) {
            return Err("the scripted model was told to fail here".to_owned());
        }
        if self.script.crash_at == Some(stage) {
            std::process::abort();
        }
        if self.script.hog_at == Some(stage) {
            // A quarter of a gigabyte at a time, touched so that it is
            // really held, until whoever watches has seen enough.
            let mut held: Vec<Vec<u8>> = Vec::new();
            for _ in 0..24 {
                held.push(vec![1u8; 256 << 20]);
                std::thread::sleep(Duration::from_millis(250));
            }
            return Err(format!("the scripted model took {} MB and was not stopped", held.len() * 256));
        }
        // In ten steps, so that a run can be seen to advance and be stopped.
        for step in 0..10 {
            progress(step as f32 / 10.0);
            std::thread::sleep(Duration::from_secs_f64(self.script.seconds.max(0.0) / 10.0));
        }
        Ok(())
    }
}

/// Where the notes of a vocal start, in seconds: the onsets that are
/// followed by sound nearly half as loud as the vocal gets, which leaves
/// out the ticks that leak into it from the rest.
fn notes_of(vocal: &[f32]) -> Vec<f64> {
    let loud = trill_listen::signal::percentile_of(&trill_listen::signal::magnitude(vocal), 99.0) as f32;
    let reach = (0.1 * RATE) as usize;
    onsets(vocal, 0.07)
        .into_iter()
        .filter(|&frame| vocal[frame..(frame + reach).min(vocal.len())].iter().any(|sample| sample.abs() >= 0.4 * loud))
        .map(|frame| frame as f64 / RATE)
        .collect()
}

impl Models for ScriptedModels {
    fn device(&self) -> String {
        "a script".to_owned()
    }

    /// By register: what is under 180 Hz is the bass, what is over 6 kHz
    /// the drums, the middle the vocals, and everything left the rest.
    fn stems(&mut self, mix: &Sound, _choice: StemChoice, progress: Progress) -> Result<Vec<Stem>, String> {
        self.begin(Stage::Stems, progress)?;
        let split = |low: Option<f64>, high: Option<f64>| Sound {
            left: band(&mix.left, low, high),
            right: band(&mix.right, low, high),
        };
        let (vocals, drums, bass) =
            (split(Some(300.0), Some(3000.0)), split(Some(6000.0), None), split(None, Some(180.0)));
        let rest = |channel: &[f32], parts: [&[f32]; 3]| -> Vec<f32> {
            (0..channel.len())
                .map(|index| channel[index] - parts[0][index] - parts[1][index] - parts[2][index])
                .collect()
        };
        let other = Sound {
            left: rest(&mix.left, [&vocals.left, &drums.left, &bass.left]),
            right: rest(&mix.right, [&vocals.right, &drums.right, &bass.right]),
        };
        progress(1.0);
        Ok([("vocals", vocals), ("drums", drums), ("bass", bass), ("other", other)]
            .into_iter()
            .map(|(name, sound)| Stem { name: name.to_owned(), sound })
            .collect())
    }

    /// The beats the pulse of the onsets gives, handed on as a model's.
    fn beats(&mut self, mix: &[f32], progress: Progress) -> Result<TrackedBeats, String> {
        self.begin(Stage::Beats, progress)?;
        let (beats, mut downbeats) = trill_listen::beats::track(mix, mix);
        if let Some(short) = self.script.short_bar {
            // The downbeats as beats, and every one after the short bar two beats sooner.
            let nearest = |time: f64| {
                (0..beats.len()).min_by(|&a, &b| (beats[a] - time).abs().total_cmp(&(beats[b] - time).abs()))
            };
            let moved: Vec<f64> = downbeats
                .iter()
                .enumerate()
                .filter_map(|(bar, &time)| {
                    let index = nearest(time)?;
                    beats.get(if bar > short { index.checked_sub(2)? } else { index }).copied()
                })
                .collect();
            downbeats = moved;
        }
        Ok(TrackedBeats { beats, downbeats })
    }

    /// One word of the script for each note the vocal starts; a pause of
    /// a second starts a new stretch.
    fn words(&mut self, vocal: &[f32], language: Option<&str>, progress: Progress) -> Result<Transcript, String> {
        self.begin(Stage::Words, progress)?;
        let starts = notes_of(vocal);
        let mut stretches: Vec<Stretch> = Vec::new();
        for (index, &start) in starts.iter().enumerate() {
            if self.script.words.is_empty() {
                break;
            }
            let end = starts.get(index + 1).copied().unwrap_or(start + 0.5).min(start + 0.5);
            let text = self.script.words[index % self.script.words.len()].clone();
            match stretches.last_mut() {
                Some(stretch) if start - stretch.end < 1.0 => {
                    stretch.end = end;
                    stretch.words.push(Said { text, p: 0.9 });
                }
                _ => stretches.push(Stretch { start, end, words: vec![Said { text, p: 0.9 }] }),
            }
        }
        Ok(Transcript { language: Some(language.unwrap_or("en").to_owned()), stretches })
    }

    /// Each word onto the note nearest its rough start.
    fn word_times(&mut self, vocal: &[f32], words: &mut [Word], progress: Progress) -> Result<bool, String> {
        self.begin(Stage::WordTimes, progress)?;
        let starts = notes_of(vocal);
        for word in words.iter_mut() {
            if let Some(&nearest) =
                starts.iter().min_by(|a, b| (*a - word.start).abs().total_cmp(&(*b - word.start).abs()))
            {
                let length = (word.end - word.start).max(0.05);
                (word.start, word.end, word.a) = (nearest, nearest + length, Some(0.9));
            }
        }
        Ok(true)
    }
}
