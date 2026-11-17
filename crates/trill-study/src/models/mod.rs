//! The four things a study asks of models: a recording's stems, its
//! beats, the words sung in it, and where each word is.
//!
//! [`Models`] is all a run knows of them. The real ones ([`real`]) are
//! networks run by ONNX Runtime on a graphics card or the processor; the
//! scripted ones ([`scripted`]) do something plain in their place, for
//! tests, which fetch nothing and need no card.

pub mod real;
pub mod scripted;

use crate::lyrics::Word;
use crate::store::StemChoice;
use serde::{Deserialize, Serialize};
use trill_listen::{Sound, Stem, TrackedBeats};

/// One stretch of singing as the transcriber heard it: where it starts
/// and ends, to a fiftieth of a second, and its words in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stretch {
    pub start: f64,
    pub end: f64,
    pub words: Vec<Said>,
}

/// One word the transcriber wrote down, and how sure it was of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Said {
    pub text: String,
    pub p: f32,
}

/// The words of a vocal before any of them is placed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// The language sung, as its two letters.
    pub language: Option<String>,
    pub stretches: Vec<Stretch>,
}

impl Transcript {
    /// The words with the times their stretches allow them: each stretch
    /// shared out among its words by their lengths. Rough, and in order.
    pub fn rough_words(&self) -> Vec<Word> {
        let mut words = Vec::new();
        for stretch in &self.stretches {
            let weights: Vec<f64> =
                stretch.words.iter().map(|word| word.text.chars().count().max(1) as f64 + 1.0).collect();
            let total: f64 = weights.iter().sum();
            let mut at = stretch.start;
            for (word, weight) in stretch.words.iter().zip(&weights) {
                let length = (stretch.end - stretch.start).max(0.0) * weight / total.max(1.0);
                words.push(Word {
                    start: at,
                    end: at + length,
                    text: word.text.clone(),
                    p: word.p,
                    a: None,
                    note: None,
                });
                at += length;
            }
        }
        words
    }
}

/// How far a stage has come, 0 to 1.
pub type Progress<'a> = &'a mut dyn FnMut(f32);

/// What a run asks of models. Audio is at [`trill_listen::RATE`]; times
/// are seconds of the recording. An error is a sentence that says what to do.
pub trait Models {
    /// Makes them ready to be asked, where that is not done when they are
    /// made: called before the first stage that asks anything of them.
    fn ready(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// What runs them, as a person reads it: "the processor", or a card by name.
    fn device(&self) -> String;

    /// Whether words can be placed at all: not where the model that
    /// places them was not fetched, and then they keep their rough times.
    fn places_words(&self) -> bool {
        true
    }

    /// Splits a recording into stems, in the model's order. They need not
    /// add up to the recording: whoever asks puts what is left over into
    /// the stem called `other`.
    fn stems(&mut self, mix: &Sound, choice: StemChoice, progress: Progress) -> Result<Vec<Stem>, String>;

    /// The beats and downbeats of a recording, heard as one channel.
    fn beats(&mut self, mix: &[f32], progress: Progress) -> Result<TrackedBeats, String>;

    /// The words sung in an isolated vocal, one channel. `language` is
    /// the two letters of the language sung, or none to be told.
    fn words(&mut self, vocal: &[f32], language: Option<&str>, progress: Progress) -> Result<Transcript, String>;

    /// Places each of `words` where it is sung in `vocal`, starting from
    /// the rough times they come with. `Ok(false)` leaves them as they
    /// are: the words are in a script the aligner does not read.
    fn word_times(&mut self, vocal: &[f32], words: &mut [Word], progress: Progress) -> Result<bool, String>;
}

/// Which models a process that studies is to use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Which {
    Real(real::Setup),
    Scripted(scripted::Script),
}

impl Which {
    /// Makes them, in the process that will run them.
    pub fn make(&self) -> Result<Box<dyn Models>, String> {
        match self {
            Which::Real(setup) => Ok(Box::new(real::OnnxModels::new(setup.clone())?)),
            Which::Scripted(script) => Ok(Box::new(scripted::ScriptedModels::new(script.clone()))),
        }
    }
}

/// Models that are made when the first stage asks for one, in place of
/// when the process starts.
pub struct Lazy {
    which: Which,
    made: Option<Box<dyn Models>>,
}

impl Lazy {
    pub fn new(which: Which) -> Self {
        Self { which, made: None }
    }

    fn made(&mut self) -> Result<&mut dyn Models, String> {
        if self.made.is_none() {
            self.made = Some(self.which.make()?);
        }
        Ok(self.made.as_deref_mut().expect("just made"))
    }
}

impl Models for Lazy {
    fn ready(&mut self) -> Result<(), String> {
        self.made()?.ready()
    }

    /// Nothing until they are made.
    fn device(&self) -> String {
        self.made.as_ref().map_or_else(String::new, |models| models.device())
    }

    fn places_words(&self) -> bool {
        self.made.as_ref().is_none_or(|models| models.places_words())
    }

    fn stems(&mut self, mix: &Sound, choice: StemChoice, progress: Progress) -> Result<Vec<Stem>, String> {
        self.made()?.stems(mix, choice, progress)
    }

    fn beats(&mut self, mix: &[f32], progress: Progress) -> Result<TrackedBeats, String> {
        self.made()?.beats(mix, progress)
    }

    fn words(&mut self, vocal: &[f32], language: Option<&str>, progress: Progress) -> Result<Transcript, String> {
        self.made()?.words(vocal, language, progress)
    }

    fn word_times(&mut self, vocal: &[f32], words: &mut [Word], progress: Progress) -> Result<bool, String> {
        self.made()?.word_times(vocal, words, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stretch_is_shared_out_among_its_words_by_their_lengths() {
        let said = |text: &str| Said { text: text.to_owned(), p: 0.8 };
        let transcript = Transcript {
            language: Some("en".into()),
            stretches: vec![
                Stretch { start: 10.0, end: 12.0, words: vec![said("I"), said("remember")] },
                Stretch { start: 15.0, end: 15.5, words: vec![said("you")] },
            ],
        };
        let words = transcript.rough_words();
        assert_eq!(words.len(), 3);
        assert_eq!((words[0].start, words[2].start, words[2].end), (10.0, 15.0, 15.5));
        // In order, end to end, the longer word the longer.
        assert!((words[0].end - words[1].start).abs() < 1e-9 && (words[1].end - 12.0).abs() < 1e-9);
        assert!(words[1].end - words[1].start > 3.0 * (words[0].end - words[0].start));
    }
}
