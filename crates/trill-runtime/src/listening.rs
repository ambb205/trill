//! Listening to a sound: what both front ends ask when they want to know
//! what is in an audio file (its beats, key, chords, parts, loops, hits and
//! notes: `trill-listen`).
//!
//! It is here because it takes the media cache and outlasts a call: a
//! song's worth of audio takes seconds to listen to, and whoever asked for
//! its tempo asks for its chords next. What was heard is kept for the last
//! few files, each for as long as the file on disk is the one that was
//! heard.

use crate::Runtime;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_listen::{Correction, Heard, Options, Recording, Sound};
use trill_media::{FileStamp, MediaError};

/// The longest recording that is listened to, in seconds. Twenty minutes
/// of stereo are the better part of a gigabyte once the analyses have
/// their copies.
pub const LONGEST_SECONDS: f64 = 15.0 * 60.0;

/// How many files' findings are kept.
const KEPT: usize = 4;

/// Why a file was not listened to: said in a sentence.
#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error(
        "{} is {minutes:.0} minutes long, and a recording is listened to up to {:.0} minutes: cut the part you want into a file of its own",
        path.display(),
        LONGEST_SECONDS / 60.0
    )]
    TooLong { path: PathBuf, minutes: f64 },
}

struct Kept {
    path: PathBuf,
    stamp: Option<FileStamp>,
    corrections: Vec<Correction>,
    heard: Arc<Heard>,
}

/// What was heard in the last few files.
#[derive(Default)]
pub(crate) struct Listening {
    kept: Mutex<Vec<Kept>>,
}

impl Runtime {
    /// What is heard in the audio file at `path`, with `corrections` made
    /// to its beat grid in their order. Slow the first time for a file (a
    /// few seconds for a song): call it off the thread that draws.
    pub fn hear(&self, path: &Path, corrections: &[Correction]) -> Result<Arc<Heard>, ListenError> {
        let stamp = FileStamp::of(path);
        {
            let mut kept = self.listening.kept.lock();
            if let Some(index) = kept.iter().position(|kept| kept.path == path && kept.stamp == stamp && kept.corrections == corrections) {
                // The most recently asked for is kept longest.
                let found = kept.remove(index);
                let heard = found.heard.clone();
                kept.push(found);
                return Ok(heard);
            }
        }
        if let Some(seconds) = trill_media::probe(path)?.seconds().filter(|&seconds| seconds > LONGEST_SECONDS) {
            return Err(ListenError::TooLong { path: path.to_owned(), minutes: seconds / 60.0 });
        }
        let audio = self.media.audio(path)?;
        if audio.seconds() > LONGEST_SECONDS {
            return Err(ListenError::TooLong { path: path.to_owned(), minutes: audio.seconds() / 60.0 });
        }
        let recording = Recording::whole(Sound::of(&audio));
        let options = Options { beats: None, corrections: corrections.to_vec() };
        let heard = Arc::new(trill_listen::hear(&recording, &options));
        let mut kept = self.listening.kept.lock();
        if kept.len() >= KEPT {
            kept.remove(0);
        }
        kept.push(Kept { path: path.to_owned(), stamp, corrections: corrections.to_vec(), heard: heard.clone() });
        Ok(heard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeOptions;
    use trill_core::audio::AudioBuffer;
    use trill_media::{WavFormat, write_wav};

    /// Four bars of clicks at 120 beats a minute.
    fn clicks(path: &Path) {
        let rate = 44_100.0;
        let mut samples = vec![0.0f32; (8.5 * rate) as usize];
        for beat in 0..16 {
            let start = (beat as f64 * 0.5 * rate) as usize + 2_000;
            for index in 0..1_500 {
                let fade = 1.0 - index as f32 / 1_500.0;
                samples[start + index] += fade * (index as f32 * if beat % 4 == 0 { 0.02 } else { 0.35 }).sin() * 0.8;
            }
        }
        write_wav(path, &AudioBuffer { sample_rate: rate, left: samples.clone(), right: samples }, WavFormat::Int16).unwrap();
    }

    #[test]
    fn a_file_is_listened_to_once_and_again_when_it_changes_or_its_grid_is_corrected() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("clicks.wav");
        clicks(&path);
        let runtime = Runtime::start(RuntimeOptions::hermetic()).unwrap();
        let first = runtime.hear(&path, &[]).unwrap();
        assert!((first.grid.bpm - 120.0).abs() < 0.3, "{}", first.grid.bpm);
        // Asked again, it is the same hearing and not a second one.
        let again = runtime.hear(&path, &[]).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let half = runtime.hear(&path, &[Correction::Half]).unwrap();
        assert!((half.grid.bpm - 60.0).abs() < 0.3, "{}", half.grid.bpm);
        assert!(Arc::ptr_eq(&first, &runtime.hear(&path, &[]).unwrap()));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_a_sentence() {
        let folder = tempfile::tempdir().unwrap();
        let runtime = Runtime::start(RuntimeOptions::hermetic()).unwrap();
        let missing = runtime.hear(&folder.path().join("gone.wav"), &[]).unwrap_err().to_string();
        assert!(missing.contains("gone.wav"), "{missing}");
        let text = folder.path().join("notes.wav");
        std::fs::write(&text, "not audio at all").unwrap();
        assert!(!runtime.hear(&text, &[]).unwrap_err().to_string().is_empty());
        let long = ListenError::TooLong { path: PathBuf::from("set.wav"), minutes: 61.0 }.to_string();
        assert_eq!(long, "set.wav is 61 minutes long, and a recording is listened to up to 15 minutes: cut the part you want into a file of its own");
    }
}
