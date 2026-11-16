//! Instruments that exist only for tests, and behave like the plugins that
//! are hard to drive blind: slow to be ready after they are created, with
//! a range outside which they are silent, a key that switches the
//! articulation, an octave of their own, and drums on some notes and not
//! on others.
//!
//! They are not built in (their URIs begin with `scripted:`), so the
//! engine treats them as it treats any plugin: it waits for them before a
//! render, and tells them what they are created for.
//!
//! * [`BASS`] plays E1 to G4 as they are written, **an octave below**
//!   (written E2 sounds as E1), each note a tone with one overtone that
//!   dies away slowly while its key is down and at once when it comes up.
//!   While C0 (MIDI 12) is held it is palm-muted: short and dark. Notes
//!   outside the range make no sound. It has two presets only it can
//!   load: `Octave up` (which makes it sound as written) and `Soft`.
//! * [`DRUMS`] has a kick on 36 (a low thump), a snare on 38 and on 40 (a
//!   burst of noise in the middle), a closed hat on 42 (a short bright
//!   tick) and a crash on 49 (a long bright wash). Every other note is
//!   silent.
//!
//! A factory made with [`ScriptedFactory::loading`] hands out instruments
//! that are deaf for a while after they are created, on the machine's own
//! clock, as a sampler is while it reads its samples: notes given to them
//! meanwhile never sound.

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
use trill_core::Device;
use trill_core::device::{
    DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, EventKind, PresetLoadError, ProcessInfo, Purpose,
};
use trill_core::presets::{PresetInfo, PresetList};

pub const BASS: &str = "scripted:bass";
pub const DRUMS: &str = "scripted:drums";

/// The key that palm-mutes the bass while it is held: C0.
pub const MUTE_KEY: u8 = 12;
/// The bass's range, as written.
pub const BASS_RANGE: std::ops::RangeInclusive<u8> = 28..=67;

const VOICES: usize = 16;
/// Where the noise of the drums begins, and begins again after a reset.
const NOISE_SEED: u32 = 0x2545_F491;

/// When the instruments a factory hands out are ready to play.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Loading {
    /// How long after it is created an instrument first answers a note.
    pub first: Duration,
    /// How long after it is created it answers the notes from middle C
    /// (MIDI 60) up, if that is later: a sampler that loads piece by piece.
    pub upper: Duration,
    /// It never answers at all.
    pub never: bool,
}

/// Makes the scripted instruments, and writes down what each was made for.
#[derive(Clone, Default)]
pub struct ScriptedFactory {
    loading: Loading,
    made: Arc<Mutex<Vec<(String, Purpose)>>>,
}

impl ScriptedFactory {
    /// Instruments that are ready the moment they exist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Instruments that take their time: see [`Loading`].
    pub fn loading(loading: Loading) -> Self {
        Self { loading, ..Self::default() }
    }

    /// The URI of every instrument made so far and what it was made for,
    /// in order.
    pub fn made(&self) -> Vec<(String, Purpose)> {
        self.made.lock().clone()
    }

    fn preset(id: &str, name: &str, description: &str) -> PresetInfo {
        PresetInfo {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            ..PresetInfo::default()
        }
    }
}

impl DeviceFactory for ScriptedFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        let descriptor = |uri: &str, name: &str, description: &str| DeviceDescriptor {
            uri: uri.to_string(),
            name: name.to_string(),
            vendor: "Trill tests".to_string(),
            category: DeviceCategory::Instrument,
            description: description.to_string(),
            params: Vec::new(),
            presets: Vec::new(),
        };
        vec![
            descriptor(BASS, "Scripted Bass", "A bass that sounds an octave below what is written"),
            descriptor(DRUMS, "Scripted Drums", "A kick, a snare, a hat and a crash"),
        ]
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        self.create_for(device, sample_rate, max_block, Purpose::Playing)
    }

    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        _max_block: usize,
        purpose: Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        let kind = match device.uri.as_str() {
            BASS => Kind::Bass,
            DRUMS => Kind::Drums,
            _ => return Err(DeviceError::Unknown(device.uri.clone())),
        };
        self.made.lock().push((device.uri.clone(), purpose));
        let born = Instant::now();
        let patch = Arc::new(Patch::default());
        // The sound a saved state stands for: the preset it was saved with.
        if let Some(saved) = device.state.as_deref().and_then(|state| std::str::from_utf8(state).ok()) {
            patch.load(saved);
        }
        let processor = Instrument {
            kind,
            sample_rate,
            ready: (!self.loading.never).then(|| born + self.loading.first),
            upper_ready: (!self.loading.never).then(|| born + self.loading.first.max(self.loading.upper)),
            patch: patch.clone(),
            voices: [Voice::default(); VOICES],
            muting: false,
            noise: NOISE_SEED,
        };
        Ok(DeviceInstance {
            processor: Box::new(processor),
            params: Vec::new(),
            controller: Some(Box::new(Controller { patch })),
        })
    }

    fn presets(&self, uri: &str) -> Option<PresetList> {
        match uri {
            BASS => Some(PresetList::ready(vec![
                Self::preset("up", "Octave up", "Sounds as written"),
                Self::preset("soft", "Soft", "Half as loud"),
            ])),
            DRUMS => Some(PresetList::ready(Vec::<PresetInfo>::new())),
            _ => None,
        }
    }
}

/// What a preset changes about an instrument, shared between its two
/// halves.
struct Patch {
    /// Semitones between what is written and what sounds.
    transpose: AtomicI32,
    /// The level, in thousandths.
    level: AtomicI32,
    name: Mutex<String>,
}

impl Default for Patch {
    fn default() -> Self {
        Self { transpose: AtomicI32::new(-12), level: AtomicI32::new(1000), name: Mutex::new(String::new()) }
    }
}

impl Patch {
    fn load(&self, preset: &str) -> bool {
        let (transpose, level) = match preset {
            "up" => (0, 1000),
            "soft" => (-12, 500),
            "" => (-12, 1000),
            _ => return false,
        };
        self.transpose.store(transpose, Ordering::Relaxed);
        self.level.store(level, Ordering::Relaxed);
        *self.name.lock() = preset.to_string();
        true
    }
}

struct Controller {
    patch: Arc<Patch>,
}

impl DeviceController for Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.patch.name.lock().clone().into_bytes())
    }

    fn load_preset(&mut self, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        if self.patch.load(&preset.id) {
            Ok(())
        } else {
            Err(PresetLoadError::Failed(format!("there is no preset '{}'", preset.id)))
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Kind {
    Bass,
    Drums,
}

/// What a voice makes.
#[derive(Copy, Clone, Default, PartialEq)]
enum Sound {
    #[default]
    Tone,
    /// Noise through a filter that lets the middle through.
    MidNoise,
    /// Noise with its lows taken out.
    BrightNoise,
}

#[derive(Copy, Clone, Default)]
struct Voice {
    active: bool,
    pitch: u8,
    sound: Sound,
    /// Radians a frame, for a tone.
    step: f64,
    phase: f64,
    level: f32,
    /// What the level is multiplied by each frame, while the key is down
    /// and after.
    held_decay: f32,
    released_decay: f32,
    released: bool,
    /// The share of the tone that is its overtone.
    overtone: f32,
    /// The noise filters' memory.
    low: f32,
    last: f32,
}

struct Instrument {
    kind: Kind,
    sample_rate: f64,
    /// When notes are first answered, and notes from middle C up; `None`
    /// for an instrument that never answers.
    ready: Option<Instant>,
    upper_ready: Option<Instant>,
    patch: Arc<Patch>,
    voices: [Voice; VOICES],
    /// The mute key is down.
    muting: bool,
    noise: u32,
}

impl Instrument {
    /// What the level is multiplied by each frame to halve in `seconds`.
    fn halving(&self, seconds: f64) -> f32 {
        0.5f64.powf(1.0 / (seconds * self.sample_rate)) as f32
    }

    fn note_on(&mut self, pitch: u8, velocity: f32) {
        let now = Instant::now();
        let ready = if pitch >= 60 { self.upper_ready } else { self.ready };
        if ready.is_none_or(|ready| now < ready) {
            // Still loading: the note is lost, as a sampler's is.
            return;
        }
        let level = velocity * self.patch.level.load(Ordering::Relaxed) as f32 / 1000.0;
        let mut voice = Voice { active: true, pitch, level: 0.5 * level, ..Voice::default() };
        match self.kind {
            Kind::Bass => {
                if pitch == MUTE_KEY {
                    self.muting = true;
                    return;
                }
                if !BASS_RANGE.contains(&pitch) {
                    return;
                }
                let sounding = i32::from(pitch) + self.patch.transpose.load(Ordering::Relaxed);
                let hz = 440.0 * 2f64.powf((f64::from(sounding) - 69.0) / 12.0);
                voice.step = std::f64::consts::TAU * hz / self.sample_rate;
                if self.muting {
                    (voice.held_decay, voice.released_decay) = (self.halving(0.03), self.halving(0.01));
                } else {
                    (voice.held_decay, voice.released_decay) = (self.halving(0.6), self.halving(0.01));
                    voice.overtone = 0.6;
                }
            }
            Kind::Drums => {
                let (sound, hz, rings) = match pitch {
                    36 => (Sound::Tone, 55.0, 0.04),
                    38 | 40 => (Sound::MidNoise, 0.0, 0.03),
                    42 => (Sound::BrightNoise, 0.0, 0.008),
                    49 => (Sound::BrightNoise, 0.0, 0.25),
                    _ => return,
                };
                voice.sound = sound;
                voice.step = std::f64::consts::TAU * hz / self.sample_rate;
                (voice.held_decay, voice.released_decay) = (self.halving(rings), self.halving(rings));
            }
        }
        let free = self.voices.iter().position(|voice| !voice.active).unwrap_or(0);
        self.voices[free] = voice;
    }

    fn handle(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => self.note_on(pitch, velocity),
            EventKind::NoteOff { pitch } => {
                if self.kind == Kind::Bass && pitch == MUTE_KEY {
                    self.muting = false;
                }
                for voice in self.voices.iter_mut().filter(|voice| voice.pitch == pitch) {
                    voice.released = true;
                }
            }
            EventKind::AllNotesOff => {
                self.muting = false;
                self.voices.iter_mut().for_each(|voice| voice.released = true);
            }
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        for frame in 0..left.len() {
            // One step of noise a frame, whether or not it is used: what
            // comes out does not depend on how the audio is cut into blocks.
            self.noise ^= self.noise << 13;
            self.noise ^= self.noise >> 17;
            self.noise ^= self.noise << 5;
            let white = (self.noise >> 8) as f32 / 8_388_608.0 - 1.0;
            let mut out = 0.0f32;
            for voice in self.voices.iter_mut().filter(|voice| voice.active) {
                let sample = match voice.sound {
                    Sound::Tone => {
                        let tone = voice.phase.sin() + f64::from(voice.overtone) * (3.0 * voice.phase).sin();
                        voice.phase += voice.step;
                        tone as f32
                    }
                    Sound::MidNoise => {
                        // A low-pass at a few kHz, then the lows out.
                        voice.low += 0.25 * (white - voice.low);
                        let band = voice.low - voice.last;
                        voice.last += 0.02 * (voice.low - voice.last);
                        2.0 * band
                    }
                    Sound::BrightNoise => {
                        let bright = white - voice.last;
                        voice.last = white;
                        0.5 * bright
                    }
                };
                out += sample * voice.level;
                voice.level *= if voice.released { voice.released_decay } else { voice.held_decay };
                if voice.level < 1e-6 {
                    voice.active = false;
                }
            }
            left[frame] += out;
            right[frame] += out;
        }
    }
}

impl DeviceProcessor for Instrument {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate;
    }

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut done = 0;
        for event in events {
            let at = (event.offset as usize).min(frames);
            self.render(&mut left[done..at], &mut right[done..at]);
            done = at;
            self.handle(event.kind);
        }
        self.render(&mut left[done..frames], &mut right[done..frames]);
    }

    fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
        self.muting = false;
        self.noise = NOISE_SEED;
    }
}
