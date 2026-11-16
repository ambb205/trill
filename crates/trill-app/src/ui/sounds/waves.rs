//! What the window knows about the waveforms of audio files.
//!
//! A waveform is drawn from a [`Peaks`] overview, which takes a decode of
//! the whole file to build. That happens on a worker; here is only the
//! table of what has come back, read while painting.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_media::{MediaError, PeakColumn, Peaks, scan_columns};
use trill_runtime::{Runtime, SoundFacts};

/// How long a file that could not be read is left alone before it is tried
/// again: long enough to cost nothing, short enough that a file put back
/// where it belongs shows up by itself.
const RETRY_AFTER: Duration = Duration::from_secs(2);
/// More waveforms than this and the ones not drawn lately are let go.
const KEEP: usize = 600;
/// A waveform drawn within this many frames is in use.
const IN_USE_FRAMES: u64 = 120;

/// A file's waveform overview and what its header and name say.
pub struct WaveData {
    peaks: Arc<Peaks>,
    /// The decoded file, for as long as something else keeps it: the cache,
    /// or the engine playing it. Deep zooms read samples from it.
    audio: Weak<AudioBuffer>,
    pub facts: SoundFacts,
}

/// What is known about a file's waveform.
#[derive(Clone)]
pub enum WaveState {
    /// A worker is reading the file, or is about to.
    Loading,
    Ready(Arc<WaveData>),
    /// The file is missing or cannot be decoded; the reason, in words.
    Failed(Arc<str>),
}

impl WaveState {
    pub fn ready(&self) -> Option<&Arc<WaveData>> {
        match self {
            WaveState::Ready(wave) => Some(wave),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn failure(&self) -> Option<&str> {
        match self {
            WaveState::Failed(reason) => Some(reason),
            _ => None,
        }
    }
}

impl WaveData {
    #[cfg(test)]
    pub fn of(buffer: &Arc<AudioBuffer>, facts: SoundFacts) -> Self {
        Self { peaks: Arc::new(Peaks::build(buffer)), audio: Arc::downgrade(buffer), facts }
    }

    /// Length of the file in seconds.
    pub fn seconds(&self) -> f64 {
        self.peaks.frames() as f64 / self.peaks.sample_rate()
    }

    /// The range of the signal in each of `columns` equal slices of the
    /// stretch of the file from `seconds.start` to `seconds.end`. Slices
    /// before the file's start or past its end are silent.
    ///
    /// This is what a waveform is drawn from, one column per pixel. It
    /// reads the overview, a few values per column, or the samples
    /// themselves once a column is narrower than the overview's finest
    /// step and the decoded file is still in memory.
    pub fn columns(&self, seconds: Range<f64>, columns: usize) -> Vec<PeakColumn> {
        let mut out = vec![PeakColumn::SILENT; columns];
        let span = seconds.end - seconds.start;
        if columns == 0 || span.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return out;
        }
        let rate = self.peaks.sample_rate();
        let total = self.peaks.frames();
        let per_column = span * rate / columns as f64;
        // The columns that lie inside the file, and the frames under them.
        let column_at = |second: f64| ((second - seconds.start) / span * columns as f64).round();
        let first = column_at(0.0).clamp(0.0, columns as f64) as usize;
        let last = column_at(total as f64 / rate).clamp(0.0, columns as f64) as usize;
        if last <= first {
            return out;
        }
        let frame_at = |column: usize| (seconds.start * rate + column as f64 * per_column).round().max(0.0) as usize;
        let frames = frame_at(first)..frame_at(last).min(total);
        let inside = last - first;
        let audio = if per_column < Peaks::BLOCK as f64 { self.audio.upgrade() } else { None };
        let found = match audio {
            Some(audio) => scan_columns(&audio, frames, inside),
            None => self.peaks.columns(frames, inside),
        };
        out[first..last].copy_from_slice(&found);
        out
    }
}

/// Reads the file and builds its overview: the worker's part.
pub(super) fn load(runtime: &Runtime, path: &Path) -> Result<WaveData, MediaError> {
    // The overview first: if the file cannot be read, the reason then names
    // it as the project does, not by where it would be on this machine.
    let peaks = runtime.media.peaks(path)?;
    let facts = runtime.sound_facts(path)?;
    // In the cache already, as the overview was built from it.
    let audio = runtime.media.audio(path).map(|audio| Arc::downgrade(&audio)).unwrap_or_default();
    Ok(WaveData { peaks, audio, facts })
}

struct Slot {
    state: WaveState,
    /// The frame it was last asked for.
    used: Cell<u64>,
    /// When a failed load may be tried again.
    retry_at: Option<Instant>,
    /// A worker has been asked and has not answered.
    asked: Cell<bool>,
}

/// The table. Reading it needs only `&self`, so painting code can look a
/// waveform up, and thereby ask for it, wherever it happens to be.
#[derive(Default)]
pub(super) struct Waves {
    slots: RefCell<HashMap<String, Slot>>,
    wanted: RefCell<Vec<String>>,
    frame: Cell<u64>,
}

impl Waves {
    pub fn get(&self, path: &str) -> WaveState {
        let frame = self.frame.get();
        let mut slots = self.slots.borrow_mut();
        match slots.get(path) {
            Some(slot) => {
                slot.used.set(frame);
                let retry = slot.retry_at.is_some_and(|at| Instant::now() >= at) && !slot.asked.get();
                if retry {
                    slot.asked.set(true);
                    self.wanted.borrow_mut().push(path.to_owned());
                }
                slot.state.clone()
            }
            None => {
                let slot =
                    Slot { state: WaveState::Loading, used: Cell::new(frame), retry_at: None, asked: Cell::new(true) };
                slots.insert(path.to_owned(), slot);
                self.wanted.borrow_mut().push(path.to_owned());
                WaveState::Loading
            }
        }
    }

    /// Has a file whose waveform is known looked at again: what is shown
    /// stays until the answer is in. Nothing happens for a file nobody
    /// has asked about.
    pub fn recheck(&self, path: &str) {
        if let Some(slot) = self.slots.borrow().get(path)
            && !slot.asked.replace(true)
        {
            self.wanted.borrow_mut().push(path.to_owned());
        }
    }

    pub fn peek(&self, path: &str) -> Option<Arc<WaveData>> {
        self.slots.borrow().get(path).and_then(|slot| slot.state.ready().cloned())
    }

    /// Whether a file is being read for the first time: something on
    /// screen is still a placeholder. A file that failed and is being
    /// tried again does not count; that goes on for as long as it is shown.
    pub fn is_loading(&self) -> bool {
        self.slots.borrow().values().any(|slot| matches!(slot.state, WaveState::Loading))
    }

    /// The files asked for since the last call, and the start of a new
    /// frame: waveforms nobody has drawn for a while are let go.
    pub fn take_requests(&mut self) -> Vec<String> {
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        let slots = self.slots.get_mut();
        if slots.len() > KEEP {
            slots.retain(|_, slot| slot.asked.get() || frame - slot.used.get() <= IN_USE_FRAMES);
        }
        std::mem::take(self.wanted.get_mut())
    }

    pub fn store(&mut self, path: &str, result: Result<Arc<WaveData>, String>) {
        // Let go of while it was loading: nobody is waiting for it.
        let Some(slot) = self.slots.get_mut().get_mut(path) else { return };
        slot.asked.set(false);
        match result {
            Ok(wave) => {
                slot.state = WaveState::Ready(wave);
                slot.retry_at = None;
            }
            Err(reason) => {
                slot.state = WaveState::Failed(reason.into());
                slot.retry_at = Some(Instant::now() + RETRY_AFTER);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_library::Tags;

    fn facts(seconds: f64) -> SoundFacts {
        SoundFacts { seconds, sample_rate: 1024, channels: 2, tempo: None, key: None, tags: Tags::EMPTY }
    }

    /// One second at 1024 Hz: silence, then full scale from frame `from` on.
    fn step_at(from: usize) -> Arc<AudioBuffer> {
        let channel: Vec<f32> = (0..1024).map(|frame| if frame >= from { 1.0 } else { 0.0 }).collect();
        Arc::new(AudioBuffer { sample_rate: 1024.0, left: channel.clone(), right: channel })
    }

    fn step() -> Arc<AudioBuffer> {
        step_at(512)
    }

    #[test]
    fn columns_follow_the_stretch_of_the_file_asked_for() {
        let audio = step();
        let wave = WaveData::of(&audio, facts(1.0));
        assert_eq!(wave.seconds(), 1.0);
        let highs = |seconds: Range<f64>, columns| -> Vec<f32> {
            wave.columns(seconds, columns).iter().map(|column| column.mixed.max).collect()
        };
        assert_eq!(highs(0.0..1.0, 4), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(highs(0.5..1.0, 2), [1.0, 1.0], "an offset into the file");
        assert_eq!(highs(0.25..0.75, 2), [0.0, 1.0]);
        // Past the end of the file, and before its start, there is silence
        // rather than the file stretched to fit.
        assert_eq!(highs(0.5..1.5, 4), [1.0, 1.0, 0.0, 0.0]);
        assert_eq!(highs(-1.0..1.0, 4), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(highs(2.0..3.0, 3), [0.0, 0.0, 0.0]);
        assert!(highs(0.0..1.0, 0).is_empty());
        assert_eq!(highs(1.0..0.0, 2), [0.0, 0.0], "a backwards stretch is empty");
        assert_eq!(highs(f64::NAN..1.0, 2), [0.0, 0.0]);
    }

    #[test]
    fn deep_zooms_read_the_samples_while_they_are_in_memory() {
        // The step falls inside one of the overview's finest entries.
        let audio = step_at(500);
        let wave = WaveData::of(&audio, facts(1.0));
        let around = 495.0 / 1024.0..505.0 / 1024.0;
        // Ten frames across ten columns: exact.
        let exact: Vec<f32> = wave.columns(around.clone(), 10).iter().map(|column| column.mixed.max).collect();
        assert_eq!(exact, [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
        // Without the samples the overview answers: the entry that holds
        // the jump bounds every column rather than placing it.
        drop(audio);
        let bounded: Vec<f32> = wave.columns(around, 10).iter().map(|column| column.mixed.max).collect();
        assert_eq!(bounded, [1.0; 10]);
    }

    #[test]
    fn a_waveform_is_asked_for_once_and_tried_again_after_a_failure() {
        let mut waves = Waves::default();
        assert!(matches!(waves.get("a.wav"), WaveState::Loading));
        assert!(matches!(waves.get("a.wav"), WaveState::Loading));
        assert!(waves.is_loading());
        assert_eq!(waves.take_requests(), ["a.wav"]);
        assert!(waves.is_loading(), "until the answer is in");
        assert!(waves.take_requests().is_empty(), "asked once, however often it is drawn");
        assert!(waves.peek("a.wav").is_none());

        waves.store("a.wav", Err("gone".into()));
        assert_eq!(waves.get("a.wav").failure(), Some("gone"));
        assert!(!waves.is_loading());
        assert!(waves.take_requests().is_empty(), "not again straight away");
        waves.slots.get_mut().get_mut("a.wav").unwrap().retry_at = Some(Instant::now());
        assert_eq!(waves.get("a.wav").failure(), Some("gone"), "the old answer stands while it is retried");
        assert!(!waves.is_loading(), "a retry is not a placeholder on screen");
        assert_eq!(waves.take_requests(), ["a.wav"]);

        waves.store("a.wav", Ok(Arc::new(WaveData::of(&step(), facts(1.0)))));
        assert!(waves.get("a.wav").ready().is_some() && waves.peek("a.wav").is_some());
        waves.store("never-asked.wav", Err("ignored".into()));
        assert!(waves.peek("never-asked.wav").is_none());

        // Looked at again on request: asked once, and what is known stays
        // until the answer is in.
        waves.recheck("a.wav");
        waves.recheck("a.wav");
        waves.recheck("never-asked.wav");
        assert!(waves.peek("a.wav").is_some());
        assert_eq!(waves.take_requests(), ["a.wav"]);
        waves.store("a.wav", Err("gone again".into()));
        assert_eq!(waves.get("a.wav").failure(), Some("gone again"));
    }

    #[test]
    fn waveforms_nobody_draws_are_let_go() {
        let mut waves = Waves::default();
        for index in 0..=KEEP {
            waves.get(&format!("{index}.wav"));
        }
        for path in waves.take_requests() {
            waves.store(&path, Err("x".into()));
        }
        for _ in 0..IN_USE_FRAMES + 2 {
            waves.get("0.wav");
            waves.take_requests();
        }
        assert_eq!(waves.slots.get_mut().len(), 1, "only the one still drawn is kept");
    }
}
