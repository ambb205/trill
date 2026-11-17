//! The sampled instrument: plays an instrument that was recorded note by
//! note (a piano, an electric piano, strings, a bass, a drum kit).
//!
//! An instrument is a folder of recordings and a file that says which
//! recording plays for which keys and how hard they are struck. Three
//! things are kept apart here:
//!
//! * [`model`] is the instrument as the player takes it, and knows of no
//!   file format. [`crate::sfz`] reads one from an SFZ file; another
//!   format is another reader.
//! * [`pool`] is the recordings in memory: the start of each, and the
//!   whole of the ones that are played, read by worker threads and kept
//!   within a budget. [`read`] reads one at another pitch than its own.
//! * [`player`] is the device on the audio thread: notes to voices.
//!
//! [`loaded`] ties them together for a device: the instrument a state
//! names, read once however many devices and renders play it, with what
//! there is to say about it.

pub mod loaded;
pub mod model;
pub mod player;
pub mod pool;
pub mod read;

pub use loaded::{Disk, Loaded, State, Texts};
pub use model::{Instrument, Region, Summary, Unsupported};
pub use player::{DESCRIPTION, MAX_VOICES, PARAMS, Player};
pub use pool::{Held, Pool, START_FRAMES};

use loaded::Instruments;
use std::sync::{Arc, OnceLock};
use trill_core::audio::AudioLoader;
use trill_core::controllers::ControllerKind;
use trill_core::device::{ControllerSupport, DeviceController, DeviceProcessor, DeviceWarning, Purpose};
use trill_core::{KeySound, Reading, SampledInstrument};

/// The URI of the device.
pub const URI: &str = "builtin:sampled";

/// Where the sampled instruments of a factory come from: their recordings
/// and the instruments that are loaded, shared by every device and render.
pub struct Source {
    pub pool: Pool,
    instruments: Instruments,
    texts: Box<dyn Texts>,
}

impl Source {
    /// Instruments whose files are on this machine's disks and whose
    /// recordings `loader` reads.
    pub fn new(loader: Arc<dyn AudioLoader>) -> Self {
        Self::with_texts(loader, Box::new(Disk))
    }

    /// The same, with the instruments' own files taken from `texts`.
    pub fn with_texts(loader: Arc<dyn AudioLoader>, texts: Box<dyn Texts>) -> Self {
        Self { pool: Pool::new(loader), instruments: Instruments::default(), texts }
    }

    /// The instrument a stored state names, read if it is not loaded.
    /// `Ok(None)` for a state that names none; an error is a sentence
    /// about the state or the file.
    pub fn load(&self, state: Option<&[u8]>) -> Result<Option<Arc<Loaded>>, String> {
        let state = State::from_device_state(state)
            .map_err(|error| format!("{error}; the device is empty until its instrument is set again"))?;
        self.instruments.load(&state, &self.pool, self.texts.as_ref())
    }

    /// What is wrong with the instrument a stored state names that the
    /// device plays in spite of.
    pub fn warnings(&self, state: Option<&[u8]>) -> Vec<DeviceWarning> {
        match self.load(state) {
            Ok(Some(loaded)) => loaded.warnings(),
            Ok(None) => Vec::new(),
            Err(reason) => vec![DeviceWarning { slot: Some(loaded::INSTRUMENT_SLOT.to_string()), message: reason }],
        }
    }
}

/// Makes a device from its stored state. Fails only when the state names
/// an instrument and there is no way to read files at all; an instrument
/// that cannot be read makes a silent device, and
/// [`Source::warnings`] says why.
pub(crate) fn make(
    state: Option<&[u8]>,
    source: Option<&Source>,
    purpose: Purpose,
) -> Result<(Box<dyn DeviceProcessor>, Box<dyn DeviceController>), String> {
    let loaded = match source {
        Some(source) => source.load(state).unwrap_or(None),
        None => {
            if let Ok(State { path: Some(path), .. }) = State::from_device_state(state) {
                return Err(format!(
                    "it plays the instrument '{path}', but this device factory was created without an audio loader \
                     (use trill_dsp::Builtins::with_loader)"
                ));
            }
            None
        }
    };
    let pool = source.map(|source| source.pool.clone());
    let player = Player::new(loaded.clone(), pool.clone(), purpose);
    let controller = Controller { state: state.map(<[u8]>::to_vec), loaded, pool, said: OnceLock::new() };
    Ok((Box::new(player), Box::new(controller)))
}

/// The device's other half: what it says of the instrument it plays.
struct Controller {
    /// The document the device was made from, handed back as it is.
    state: Option<Vec<u8>>,
    loaded: Option<Arc<Loaded>>,
    pool: Option<Pool>,
    said: OnceLock<Arc<SampledInstrument>>,
}

impl DeviceController for Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.state.clone()
    }

    fn sampled(&mut self) -> Option<Arc<SampledInstrument>> {
        let loaded = self.loaded.as_ref()?;
        Some(self.said.get_or_init(|| Arc::new(loaded.said())).clone())
    }

    fn reading(&self) -> Option<Reading> {
        let (loaded, pool) = (self.loaded.as_ref()?, self.pool.as_ref()?);
        let (ready, of) = loaded.settled();
        let held = pool.held();
        Some(Reading { ready, of, unready_notes: held.unready_notes, short_notes: held.short_notes })
    }

    fn missing_files(&mut self) -> Vec<(String, String)> {
        self.loaded.as_ref().map(|loaded| loaded.missing_files()).unwrap_or_default()
    }

    fn key_sounds(&mut self) -> Option<Vec<KeySound>> {
        self.loaded.as_ref()?.key_sounds()
    }

    fn controller_support(&self) -> ControllerSupport {
        let mut kinds =
            vec![ControllerKind::VOLUME, ControllerKind::EXPRESSION, ControllerKind::SUSTAIN, ControllerKind::PitchBend];
        if let Some(loaded) = &self.loaded {
            for number in loaded.instrument.controllers() {
                if let Some(kind) = ControllerKind::numbered(number).filter(|kind| !kinds.contains(kind)) {
                    kinds.push(kind);
                }
            }
        }
        ControllerSupport::Listed(kinds)
    }
}

#[cfg(test)]
mod player_tests;
