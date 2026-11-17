//! The real models: networks run by ONNX Runtime.

use super::{Models, Progress, Transcript};
use crate::lyrics::Word;
use crate::store::StemChoice;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use trill_listen::{Sound, Stem, TrackedBeats};

/// Where the models' files are and what is to run them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    /// The folder the setup keeps its parts in.
    pub parts: PathBuf,
}

pub struct OnnxModels {}

impl OnnxModels {
    pub fn new(_setup: Setup) -> Result<Self, String> {
        Err("the models are not set up yet".to_owned())
    }
}

impl Models for OnnxModels {
    fn device(&self) -> String {
        String::new()
    }

    fn stems(&mut self, _mix: &Sound, _choice: StemChoice, _progress: Progress) -> Result<Vec<Stem>, String> {
        unimplemented!()
    }

    fn beats(&mut self, _mix: &[f32], _progress: Progress) -> Result<TrackedBeats, String> {
        unimplemented!()
    }

    fn words(&mut self, _vocal: &[f32], _language: Option<&str>, _progress: Progress) -> Result<Transcript, String> {
        unimplemented!()
    }

    fn word_times(&mut self, _vocal: &[f32], _words: &mut [Word], _progress: Progress) -> Result<bool, String> {
        unimplemented!()
    }
}
