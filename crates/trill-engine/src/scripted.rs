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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use trill_core::Device;
use trill_core::device::{
    Beside, DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance,
    DeviceProcessor, Event, EventKind, PresetLoadError, ProcessInfo, Purpose,
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

/// When a bass that is made for a render takes its process down with it,
/// as a plugin that faults does: by writing where nothing may be
/// written. The bass a song plays live never does, and neither do the
/// drums and the amplifier beside it. For showing what
/// becomes of a render when a plugin does this: such a render has to be
/// done in a process of its own ([`crate::apart`]), or whoever asked for
/// it goes down too.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Dies {
    /// As it is made.
    Made,
    /// At the first note it is tried with before the song is played.
    Rehearsed,
    /// In this block of the song, counted from one.
    Played(u32),
    /// As it is let go of.
    LetGo,
}

/// The fault itself.
fn die() -> ! {
    // SAFETY: there is none, on purpose: the address is not one that may
    // be written to, and the system ends the process for it. Only a test
    // asks for this, of a process it started for the purpose.
    unsafe { std::ptr::write_volatile(std::hint::black_box(8usize) as *mut u8, 1) };
    std::process::abort()
}

/// When the instruments a factory hands out are ready to play.
#[derive(Copy, Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// The instruments the factory hands out keep one thing between them,
    /// as the instances of some plugins do: a count of the notes any of
    /// them has sounded, by which the next note of any of them is given
    /// its level. They say that they are to be run in turn, and so does
    /// the amplifier ([`Beside::InTurn`]); the factory notices when two of
    /// the instruments are run at once all the same
    /// ([`ScriptedFactory::overlapped`]).
    pub shares: bool,
    /// Its devices say that they are not to be run beside anything
    /// ([`Beside::Nothing`]).
    pub alone: bool,
    /// The amplifier's level goes up and down slowly from the moment it
    /// is made, with every frame it is run and whatever it is given: what
    /// it makes of a note depends on how much it was run before, and a
    /// reset does not put that back. (An amplifier plugin was measured
    /// that is like this.)
    pub free: bool,
    /// A bass that is made for a render takes its process down: see
    /// [`Dies`].
    pub dies: Option<Dies>,
    /// The bass draws how loud each note is at random, from a stream that
    /// every bass in the program shares and that begins at the same place
    /// in a program that has just started: as some sampled instruments
    /// draw which recording they play. Nothing puts the stream back. Its
    /// setting `takes`, which is `at random` as it comes, stops it when
    /// it is given `in turn` ([`DeviceFactory::repeating`] says so).
    pub random: bool,
    /// The drums draw how loud each hit is from the same stream, and have
    /// no setting that stops it: what is known of them
    /// ([`DeviceFactory::repeating`]) names none.
    pub unruly: bool,
    /// The bass only plays what it listens to: it says it takes a key,
    /// and what its notes come to is as loud as the key it is handed,
    /// which is nothing where it is handed none. A vocoder is like this.
    pub listens: bool,
}

/// What the state of a bass ends in that was told to take its notes in
/// turn.
const IN_TURN: &str = "\u{1}in turn";

/// How many notes the basses that draw at random have drawn for, in this
/// program.
static DRAWS: AtomicU32 = AtomicU32::new(0);

/// What the instruments of a factory keep between them: see
/// [`Loading::shares`].
#[derive(Default)]
struct Shared {
    /// How many notes any of them has sounded.
    notes: AtomicU32,
    /// One of them is being run; and two of them were run at once.
    inside: AtomicBool,
    overlapped: AtomicBool,
}

/// Makes the scripted instruments, and writes down what each was made for.
#[derive(Clone, Default)]
pub struct ScriptedFactory {
    loading: Loading,
    made: Arc<Mutex<Vec<(String, Purpose)>>>,
    /// Until when the host is behind with other work.
    behind_until: Arc<Mutex<Option<Instant>>>,
    shared: Arc<Shared>,
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

    /// Whether two instruments that keep something between them
    /// ([`Loading::shares`]) were ever run at the same time.
    pub fn overlapped(&self) -> bool {
        self.shared.overlapped.load(Ordering::Relaxed)
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
            let amp = Amp { ready, free: self.loading.free, frames: 0 };
            return Ok(DeviceInstance { processor: Box::new(amp), params: Vec::new(), controller: None });
        }
        // Only a bass that is made for a render ever takes its process down.
        let dies = self.loading.dies.filter(|_| purpose == Purpose::Rendering && kind == Kind::Bass);
        if dies == Some(Dies::Made) {
            die();
        }
        let patch = Arc::new(Patch::default());
        // The sound a saved state stands for: the preset it was saved with.
        if let Some(saved) = device.state.as_deref().and_then(|state| std::str::from_utf8(state).ok()) {
            // And whether it was told to take its notes in turn.
            let (saved, in_turn) = match saved.strip_suffix(IN_TURN) {
                Some(name) => (name, true),
                None => (saved, false),
            };
            patch.load(saved);
            patch.in_turn.store(in_turn, Ordering::Relaxed);
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
            shared: self.loading.shares.then(|| self.shared.clone()),
            dies,
            blocks: 0,
            random: (self.loading.random && kind == Kind::Bass) || (self.loading.unruly && kind == Kind::Drums),
            listens: self.loading.listens && kind == Kind::Bass,
            onset: self.loading.onset,
            patch: patch.clone(),
            voices: [Voice::default(); VOICES],
            muting: false,
            noise: NOISE_SEED,
        };
        Ok(DeviceInstance {
            processor: Box::new(processor),
            params: Vec::new(),
            controller: Some(Box::new(Controller {
                patch,
                changes_after: self.loading.changes_after,
                random: self.loading.random && kind == Kind::Bass,
            })),
        })
    }

    fn busy(&self) -> bool {
        self.behind_until.lock().is_some_and(|until| Instant::now() < until)
    }

    fn repeating(&self, uri: &str) -> Option<trill_core::device::Repeating> {
        if uri == DRUMS && self.loading.unruly {
            // Known for what it does, and for nothing that stops it.
            return Some(trill_core::device::Repeating {
                what: "chooses for itself how loud each hit is".into(),
                ..Default::default()
            });
        }
        (uri == BASS && self.loading.random).then(|| trill_core::device::Repeating {
            what: "draws at random how loud each note is".into(),
            settings: vec![("takes".into(), "in turn".into())],
            params: Vec::new(),
            costs: "every note as loud as the last".into(),
            begins_alike: true,
        })
    }

    fn beside(&self, uri: &str) -> Beside {
        match uri {
            BASS | DRUMS | AMP if self.loading.alone => Beside::Nothing,
            BASS | DRUMS | AMP if self.loading.shares => Beside::InTurn("scripted devices".to_string()),
            _ => Beside::Anything,
        }
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
    /// The setting `takes` is `in turn`: a bass that draws at random does
    /// not.
    in_turn: AtomicBool,
}

impl Default for Patch {
    fn default() -> Self {
        Self {
            transpose: AtomicI32::new(-12),
            level: AtomicI32::new(1000),
            name: Mutex::new(String::new()),
            coming: Mutex::new(None),
            in_turn: AtomicBool::new(false),
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
    /// The instrument draws at random, and has a setting that stops it.
    random: bool,
}

impl DeviceController for Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut state = self.patch.name.lock().clone();
        if self.patch.in_turn.load(Ordering::Relaxed) {
            state.push_str(IN_TURN);
        }
        Some(state.into_bytes())
    }

    fn settings(&mut self) -> Option<Vec<(String, String)>> {
        let takes = if self.patch.in_turn.load(Ordering::Relaxed) { "in turn" } else { "at random" };
        self.random.then(|| vec![("takes".to_string(), takes.to_string())])
    }

    fn set_settings(&mut self, values: &[(String, String)]) -> Result<(), PresetLoadError> {
        if !self.random {
            return Err(PresetLoadError::Unsupported);
        }
        for (name, value) in values {
            match (name.as_str(), value.as_str()) {
                ("takes", "in turn") => self.patch.in_turn.store(true, Ordering::Relaxed),
                ("takes", "at random") => self.patch.in_turn.store(false, Ordering::Relaxed),
                _ => {
                    return Err(PresetLoadError::Failed(format!(
                        "there is no setting '{name}' with a value '{value}'"
                    )));
                }
            }
        }
        Ok(())
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

impl Drop for Instrument {
    fn drop(&mut self) {
        if self.dies == Some(Dies::LetGo) {
            die();
        }
    }
}

/// The amplifier: half of what goes in comes out, once it is ready.
struct Amp {
    /// When it begins to pass sound; `None` for one that never does.
    ready: Option<Instant>,
    /// Its level goes up and down with the frames it has been run, which
    /// are counted here: see [`Loading::free`].
    free: bool,
    frames: u64,
}

impl DeviceProcessor for Amp {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let gain = if self.ready.is_some_and(|ready| Instant::now() >= ready) { 0.5 } else { 0.0 };
        if !self.free {
            left.iter_mut().chain(right.iter_mut()).for_each(|sample| *sample *= gain);
            return;
        }
        // Once round in a little under half a second, a fifth up and down.
        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let swing = 1.0 + 0.2 * (self.frames as f64 * std::f64::consts::TAU / 20_011.0).sin() as f32;
            (*left, *right) = (*left * gain * swing, *right * gain * swing);
            self.frames += 1;
        }
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
    /// What it keeps with the factory's other instruments, if anything.
    shared: Option<Arc<Shared>>,
    /// When it takes its process down, and how many blocks of the song it
    /// has played.
    dies: Option<Dies>,
    blocks: u32,
    /// It draws how loud a note is at random, and it only plays what it
    /// listens to: see [`Loading`].
    random: bool,
    listens: bool,
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
        // What every instrument of the factory counts together says how
        // loud this note is.
        if let Some(shared) = &self.shared {
            voice.level *= 1.0 - (shared.notes.fetch_add(1, Ordering::Relaxed) % 5) as f32 / 10.0;
        }
        // A draw from what every such bass in the program draws from.
        if self.random && !self.patch.in_turn.load(Ordering::Relaxed) {
            let draw = DRAWS.fetch_add(1, Ordering::Relaxed).wrapping_mul(2_654_435_761) >> 28;
            voice.level *= 1.0 - (draw % 5) as f32 / 10.0;
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

    fn takes_key(&self) -> bool {
        self.listens
    }

    fn process_keyed(
        &mut self,
        info: &ProcessInfo,
        events: &[Event],
        left: &mut [f32],
        right: &mut [f32],
        key: trill_core::device::KeyInput<'_>,
    ) {
        // As loud as what it listens to, up to its own level.
        let heard = key.left.iter().chain(key.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        self.play(info, events, left, right);
        let gain = (heard * 4.0).min(1.0);
        left.iter_mut().chain(right.iter_mut()).for_each(|sample| *sample *= gain);
    }

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        self.play(info, events, left, right);
        if self.listens {
            // Handed nothing to listen to, it plays nothing.
            left.iter_mut().chain(right.iter_mut()).for_each(|sample| *sample = 0.0);
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
        self.muting = false;
        self.noise = NOISE_SEED;
    }
}

impl Instrument {
    /// What its notes come to over one block, added to what is there.
    fn play(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        self.blocks += u32::from(info.playing);
        let note = events.iter().any(|event| matches!(event.kind, EventKind::NoteOn { .. }));
        match self.dies {
            Some(Dies::Rehearsed) if !info.playing && note => die(),
            Some(Dies::Played(block)) if info.playing && self.blocks >= block => die(),
            _ => {}
        }
        // Instruments that keep something between them notice when two of
        // them are run at once, and take long enough over a block for that
        // to show.
        let shared = self.shared.clone();
        if let Some(shared) = &shared {
            if shared.inside.swap(true, Ordering::SeqCst) {
                shared.overlapped.store(true, Ordering::Relaxed);
            }
            (0..2_000).for_each(|_| std::hint::spin_loop());
        }
        let frames = left.len().min(right.len());
        let mut done = 0;
        for event in events {
            let at = (event.offset as usize).min(frames);
            self.render(&mut left[done..at], &mut right[done..at]);
            done = at;
            self.handle(event.kind);
        }
        self.render(&mut left[done..frames], &mut right[done..frames]);
        if let Some(shared) = &shared {
            shared.inside.store(false, Ordering::SeqCst);
        }
    }
}
