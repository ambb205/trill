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
//! * [`AMP`] is an effect: it passes what it is given at half its level.
//!
//! A factory made with [`ScriptedFactory::loading`] hands out instruments
//! that are deaf for a while after they are created, on the machine's own
//! clock, as a sampler is while it reads its samples: notes given to them
//! meanwhile never sound. Its amplifier passes nothing for as long. They
//! can be given the other habits samplers were watched to have as well:
//! one key that arrives long after the rest, with nothing to show that
//! anything is still to come; a key that sounds one time and not the
//! next; notes that begin a moment after their key; and going on with
//! the sound they had for a while after they are given a preset.

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
pub const AMP: &str = "scripted:amp";

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
    /// The program grows while it loads, as it does while a sampler reads
    /// its samples: some megabytes every twentieth of a second, held for
    /// as long as the instrument lives.
    pub grows: bool,
    /// One key that arrives later than everything else, and how long
    /// after the instrument is created: a kit's last drum.
    pub late: Option<(u8, Duration)>,
    /// A key that sounds every other time it is pressed, beginning with
    /// the first.
    pub fickle: Option<u8>,
    /// How long after a key goes down its sound begins.
    pub onset: Duration,
    /// How long an instrument goes on with the sound it had after it is
    /// given a preset. Its saved state names the preset at once.
    pub changes_after: Duration,
    /// How long after an instrument is made its host is behind with other
    /// work ([`DeviceFactory::busy`]), as while a song of plugins is let
    /// go of. The instrument does not have its turn before: everything
    /// else it takes its time over begins after this.
    pub held_up: Duration,
    /// How many recordings it has of each key, if more than one: every
    /// time a key sounds it plays the next of them, each a little quieter
    /// than the one before, and then the first again. It begins with the
    /// first when it is made, and a reset does not take it back there: a
    /// sampled instrument's takes, going round.
    pub takes: u32,
}

/// Makes the scripted instruments, and writes down what each was made for.
#[derive(Clone, Default)]
pub struct ScriptedFactory {
    loading: Loading,
    made: Arc<Mutex<Vec<(String, Purpose)>>>,
    /// Until when the host is behind with other work.
    behind_until: Arc<Mutex<Option<Instant>>>,
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
        let descriptor = |uri: &str, name: &str, category: DeviceCategory, description: &str| DeviceDescriptor {
            uri: uri.to_string(),
            name: name.to_string(),
            vendor: "Trill tests".to_string(),
            category,
            description: description.to_string(),
            params: Vec::new(),
            presets: Vec::new(),
        };
        let (instrument, effect) = (DeviceCategory::Instrument, DeviceCategory::Effect);
        vec![
            descriptor(BASS, "Scripted Bass", instrument, "A bass that sounds an octave below what is written"),
            descriptor(DRUMS, "Scripted Drums", instrument, "A kick, a snare, a hat and a crash"),
            descriptor(AMP, "Scripted Amp", effect, "Passes what it is given at half its level"),
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
            AMP => Kind::Amp,
            _ => return Err(DeviceError::Unknown(device.uri.clone())),
        };
        self.made.lock().push((device.uri.clone(), purpose));
        // What holds the host up holds the instrument up: it is as if it
        // were made when that is over.
        let born = Instant::now() + self.loading.held_up;
        if !self.loading.held_up.is_zero() {
            *self.behind_until.lock() = Some(born);
        }
        if kind == Kind::Amp {
            let ready = (!self.loading.never).then(|| born + self.loading.first);
            return Ok(DeviceInstance { processor: Box::new(Amp { ready }), params: Vec::new(), controller: None });
        }
        let patch = Arc::new(Patch::default());
        // The sound a saved state stands for: the preset it was saved with.
        if let Some(saved) = device.state.as_deref().and_then(|state| std::str::from_utf8(state).ok()) {
            patch.load(saved);
        }
        let samples = Arc::new(Mutex::new(Vec::new()));
        if self.loading.grows {
            let (samples, until) = (samples.clone(), born + self.loading.first.max(self.loading.upper));
            std::thread::spawn(move || {
                while Instant::now() < until && Arc::strong_count(&samples) > 1 {
                    samples.lock().push(vec![1u8; 6 * 1024 * 1024]);
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
        let processor = Instrument {
            _samples: samples,
            kind,
            sample_rate,
            ready: (!self.loading.never).then(|| born + self.loading.first),
            upper_ready: (!self.loading.never).then(|| born + self.loading.first.max(self.loading.upper)),
            late: self.loading.late.map(|(key, after)| (key, born + after)),
            fickle: self.loading.fickle,
            presses: 0,
            takes: self.loading.takes,
            sounded: [0; 128],
            onset: self.loading.onset,
            patch: patch.clone(),
            voices: [Voice::default(); VOICES],
            muting: false,
            noise: NOISE_SEED,
        };
        Ok(DeviceInstance {
            processor: Box::new(processor),
            params: Vec::new(),
            controller: Some(Box::new(Controller { patch, changes_after: self.loading.changes_after })),
        })
    }

    fn busy(&self) -> bool {
        self.behind_until.lock().is_some_and(|until| Instant::now() < until)
    }

    fn presets(&self, uri: &str) -> Option<PresetList> {
        match uri {
            BASS => Some(PresetList::ready(vec![
                Self::preset("up", "Octave up", "Sounds as written"),
                Self::preset("soft", "Soft", "Half as loud"),
            ])),
            DRUMS | AMP => Some(PresetList::ready(Vec::<PresetInfo>::new())),
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
    /// A preset that was asked for and is not played yet: from when it
    /// is, and its transposition and level.
    coming: Mutex<Option<(Instant, i32, i32)>>,
}

impl Default for Patch {
    fn default() -> Self {
        Self {
            transpose: AtomicI32::new(-12),
            level: AtomicI32::new(1000),
            name: Mutex::new(String::new()),
            coming: Mutex::new(None),
        }
    }
}

impl Patch {
    /// Takes on the preset now.
    fn load(&self, preset: &str) -> bool {
        self.load_after(preset, Duration::ZERO)
    }

    /// Is named after the preset now, and sounds like it after `wait`.
    fn load_after(&self, preset: &str, wait: Duration) -> bool {
        let (transpose, level) = match preset {
            "up" => (0, 1000),
            "soft" => (-12, 500),
            "" => (-12, 1000),
            _ => return false,
        };
        *self.name.lock() = preset.to_string();
        *self.coming.lock() = Some((Instant::now() + wait, transpose, level));
        self.arrive();
        true
    }

    /// Takes on the sound of a preset that was asked for, once it is time.
    fn arrive(&self) {
        let mut coming = self.coming.lock();
        if let Some((_, transpose, level)) = coming.take_if(|(at, ..)| Instant::now() >= *at) {
            self.transpose.store(transpose, Ordering::Relaxed);
            self.level.store(level, Ordering::Relaxed);
        }
    }
}

struct Controller {
    patch: Arc<Patch>,
    changes_after: Duration,
}

impl DeviceController for Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.patch.name.lock().clone().into_bytes())
    }

    fn load_preset(&mut self, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        if self.patch.load_after(&preset.id, self.changes_after) {
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
    Amp,
}

/// The amplifier: half of what goes in comes out, once it is ready.
struct Amp {
    /// When it begins to pass sound; `None` for one that never does.
    ready: Option<Instant>,
}

impl DeviceProcessor for Amp {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let gain = if self.ready.is_some_and(|ready| Instant::now() >= ready) { 0.5 } else { 0.0 };
        left.iter_mut().chain(right.iter_mut()).for_each(|sample| *sample *= gain);
    }

    fn reset(&mut self) {}
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
    /// How many frames it has yet to wait before it begins.
    wait: u32,
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
    /// What it has "loaded": memory that is let go of with it.
    _samples: Arc<Mutex<Vec<Vec<u8>>>>,
    kind: Kind,
    sample_rate: f64,
    /// When notes are first answered, and notes from middle C up; `None`
    /// for an instrument that never answers.
    ready: Option<Instant>,
    upper_ready: Option<Instant>,
    /// A key that arrives later than the rest, and when.
    late: Option<(u8, Instant)>,
    /// A key that sounds every other time, and how often it was pressed.
    fickle: Option<u8>,
    presses: u32,
    /// How many recordings it has of each key, and how often each key has
    /// sounded since the instrument was made.
    takes: u32,
    sounded: [u32; 128],
    /// How long after a key goes down its sound begins.
    onset: Duration,
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
        let late = self.late.is_some_and(|(key, at)| key == pitch && now < at);
        if late || ready.is_none_or(|ready| now < ready) {
            // Still loading: the note is lost, as a sampler's is.
            return;
        }
        if self.fickle == Some(pitch) {
            self.presses += 1;
            if self.presses.is_multiple_of(2) {
                return;
            }
        }
        self.patch.arrive();
        let level = velocity * self.patch.level.load(Ordering::Relaxed) as f32 / 1000.0;
        let wait = (self.onset.as_secs_f64() * self.sample_rate) as u32;
        let mut voice = Voice { active: true, pitch, level: 0.5 * level, wait, ..Voice::default() };
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
            Kind::Amp => return,
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
        // The next of the key's recordings: each is a twelfth quieter than
        // the one before.
        if self.takes > 1 {
            let key = &mut self.sounded[usize::from(pitch.min(127))];
            voice.level *= 1.0 - (*key % self.takes) as f32 / 12.0;
            *key += 1;
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
                if voice.wait > 0 {
                    voice.wait -= 1;
                    continue;
                }
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
