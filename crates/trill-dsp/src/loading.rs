//! What the sample-playing devices share off the audio thread: reading and
//! writing their state documents, turning file paths into prepared samples,
//! and saying what went wrong.
//!
//! A device's parameters live in the project as numbers. Which files it
//! plays cannot, so that goes in the device's `state`: a small JSON document
//! with a `version`, readable in a text editor and in a diff. Each device
//! module defines its own document (`sampler::State`, `drumrack::State`);
//! the rules they share are here:
//!
//! * Fields this build does not know are kept and written back, so a project
//!   edited by an older build loses nothing a newer one stored.
//! * A document from a newer `version` is not interpreted at all. The device
//!   loads empty and its state is saved back untouched.
//! * Nothing here fails device creation. A project with a missing sample
//!   must still open and play everything else; the device plays silence for
//!   that sample, and [`sample_problems`](crate::sample_problems) says why.

use crate::sample::Sample;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use trill_core::audio::{AudioBuffer, AudioLoader};
use trill_core::device::DeviceController;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum StateError {
    #[error("the device state is not a valid document: {0}")]
    Invalid(String),
    #[error("the device state was written by a newer version (format {found}, this build reads up to {supported})")]
    TooNew { found: u32, supported: u32 },
}

/// Serializes a state document as indented JSON.
pub(crate) fn encode<T: Serialize>(state: &T) -> Vec<u8> {
    serde_json::to_vec_pretty(state).expect("state documents hold only strings and numbers")
}

/// Parses a state document, refusing versions newer than `supported`.
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8], supported: u32) -> Result<T, StateError> {
    #[derive(serde::Deserialize)]
    struct Header {
        version: u32,
    }
    // The version is read on its own first: a newer format may have changed
    // the meaning or type of anything else.
    let header: Header = serde_json::from_slice(bytes).map_err(|error| StateError::Invalid(error.to_string()))?;
    if header.version > supported {
        return Err(StateError::TooNew { found: header.version, supported });
    }
    serde_json::from_slice(bytes).map_err(|error| StateError::Invalid(error.to_string()))
}

/// A sample a device's state refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleRef {
    /// What the device uses it for: `sample`, `pad 36`...
    pub slot: String,
    pub path: String,
}

/// Prepared samples, shared between the devices that play the same file.
///
/// Preparing a sample filters the whole file several times over, and every
/// offline render creates a second instance of every device; with the store
/// that work is done once per file. Entries are held weakly: a sample is
/// freed when the last device playing it goes away.
#[derive(Default)]
pub(crate) struct SampleStore {
    prepared: Mutex<HashMap<String, Prepared>>,
}

/// The buffer a sample was prepared from, and the result.
type Prepared = (Weak<AudioBuffer>, Weak<Sample>);

impl SampleStore {
    fn prepare(&self, path: &str, buffer: &Arc<AudioBuffer>) -> Arc<Sample> {
        let mut prepared = self.prepared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // The loader hands out a new buffer when the file has changed on
        // disk, so the entry is only good for the buffer it was made from.
        if let Some((source, sample)) = prepared.get(path)
            && source.upgrade().is_some_and(|source| Arc::ptr_eq(&source, buffer))
            && let Some(sample) = sample.upgrade()
        {
            return sample;
        }
        let sample = Arc::new(Sample::new(buffer));
        prepared.retain(|_, (_, sample)| sample.strong_count() > 0);
        prepared.insert(path.to_string(), (Arc::downgrade(buffer), Arc::downgrade(&sample)));
        sample
    }

    /// How many holders each prepared sample has, by path.
    #[cfg(test)]
    pub fn holders(&self) -> Vec<(String, usize)> {
        let prepared = self.prepared.lock().unwrap();
        prepared.iter().map(|(path, (_, sample))| (path.clone(), sample.strong_count())).collect()
    }
}

/// Where a device being created gets its samples from.
pub(crate) struct SampleSource<'a> {
    pub loader: Option<&'a dyn AudioLoader>,
    pub store: Option<&'a SampleStore>,
    /// Where the tables made for wavetable synths are kept, so that the
    /// same table is made once.
    pub tables: Option<&'a crate::wavetable::TableStore>,
}

impl SampleSource<'_> {
    /// Loads and prepares the file at `path`. A file that cannot be loaded
    /// is `Ok(None)`: the device goes without. The only error is having no
    /// loader at all, which is a mistake in how the factory was set up
    /// rather than a problem with the project.
    pub fn load(&self, path: &str) -> Result<Option<Arc<Sample>>, String> {
        let loader = self.loader.ok_or_else(|| {
            format!(
                "it plays the sample '{path}', but this device factory was created without an audio loader \
                 (use trill_dsp::Builtins::with_loader)"
            )
        })?;
        Ok(loader.load(path).ok().map(|buffer| match self.store {
            Some(store) => store.prepare(path, &buffer),
            None => Arc::new(Sample::new(&buffer)),
        }))
    }
}

/// The controller of a sample-playing device. The device's state does not
/// change while it runs (a new sample means a new instance), so saving it
/// returns the document the device was created from, byte for byte.
pub(crate) struct SampleController {
    pub state: Option<Vec<u8>>,
}

impl DeviceController for SampleController {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.state.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemoryLoader;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Document {
        version: u32,
        name: String,
    }

    #[test]
    fn documents_round_trip_and_newer_versions_are_refused() {
        let document = Document { version: 2, name: "kick".into() };
        let bytes = encode(&document);
        assert!(
            String::from_utf8(bytes.clone()).unwrap().contains("\n  \"name\": \"kick\""),
            "should be indented JSON"
        );
        assert_eq!(decode::<Document>(&bytes, 2).unwrap(), document);
        assert_eq!(decode::<Document>(&bytes, 1), Err(StateError::TooNew { found: 2, supported: 1 }));
        assert!(matches!(decode::<Document>(b"not json", 1), Err(StateError::Invalid(_))));
        assert!(matches!(decode::<Document>(b"{\"name\": \"x\"}", 1), Err(StateError::Invalid(_))));
        // A newer document is refused even if the rest of it no longer fits.
        assert_eq!(
            decode::<Document>(b"{\"version\": 9, \"name\": [1, 2]}", 1),
            Err(StateError::TooNew { found: 9, supported: 1 })
        );
    }

    #[test]
    fn prepared_samples_are_shared_until_the_file_changes() {
        let loader = MemoryLoader::default();
        loader.insert("a.wav", vec![0.5; 2_000], 48_000.0);
        let store = SampleStore::default();
        let source = SampleSource { loader: Some(&loader), store: Some(&store), tables: None };

        let first = source.load("a.wav").unwrap().unwrap();
        let second = source.load("a.wav").unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(source.load("missing.wav").unwrap().is_none());

        // The loader now returns a different buffer for the same path.
        loader.insert("a.wav", vec![0.25; 1_000], 48_000.0);
        let third = source.load("a.wav").unwrap().unwrap();
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(third.frames(), 1_000);

        // Nothing is kept alive by the store itself.
        let weak = Arc::downgrade(&third);
        drop((first, second, third));
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn loading_without_a_loader_is_an_error_that_says_what_to_do() {
        let source = SampleSource { loader: None, store: None, tables: None };
        let error = source.load("a.wav").err().unwrap();
        assert!(error.contains("a.wav") && error.contains("Builtins::with_loader"), "{error}");
    }
}
