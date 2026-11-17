//! The audio-thread half of the sampled instrument: the notes it is given,
//! turned into voices that each play one recording.
//!
//! # What decides which recording sounds
//!
//! A key going down is looked up among the regions that cover it: its
//! velocity, where the controllers are, which switch was pressed last. Of
//! several recordings of one note, one is **taken in turn** and one **at
//! random**:
//!
//! * A turn is counted for each region from the moment the device was made
//!   or last reset, which is where a render and a song played from its
//!   start both begin. The same notes are the same takes.
//! * The random number of a note comes from the note and its place: its
//!   key and the frame of the song it begins on, while the song plays.
//!   Nothing is drawn from a stream that goes on between notes, so a note
//!   has the same number in every render and whenever that bar is played.
//!   (A key played while the song stands still has no place; it is given
//!   the count of notes played so far.)
//!
//! # What is the same at any buffer size
//!
//! Everything that moves while a note sounds (where the controllers are on
//! their way to, the pitch, the loudness of a layer that a controller
//! fades) is worked out on a grid of [`TICK`] frames counted from the
//! first frame the device processed, and runs in straight lines between
//! the points of that grid. Where a buffer ends has no part in it.
//!
//! # What a note does whose recording is not in memory
//!
//! While it plays live: a note whose recording has not had its start read
//! yet (the instrument was loaded a moment ago) does not sound, and is
//! counted. A note that reaches the end of its recording's start before a
//! worker has read the rest is faded out over five milliseconds, and
//! counted. In a render neither happens: the render waits for what it
//! needs ([`trill_core::device::Purpose::Rendering`]).

use super::loaded::Loaded;
use super::model::{FilterKind, LoopMode, Moved, OffMode, Region, Stage, Trigger, along};
use super::pool::{Audio, Pool};
use super::read::{Kernel, kernel_for, read, read_mapped};
use crate::define_params;
use crate::level::ChannelLevel;
use crate::params::{ParamValues, float, int};
use crate::util::{OnePole, SILENT_DB, Svf, db_to_gain, level_gain, pan_gains};
use crate::voice::FadeOut;
use std::sync::Arc;
use trill_core::controllers::{CONTROLLER_EXPRESSION, CONTROLLER_VOLUME};
use trill_core::device::{DeviceProcessor, Event, EventKind, Pedal, ProcessInfo, Purpose};

/// The most notes that can sound at once.
pub const MAX_VOICES: usize = 128;
/// Voices beyond those, which only ever hold notes on their way out.
const SPARE_VOICES: usize = 32;
/// Frames between the points at which what moves is worked out.
pub const TICK: usize = 16;
/// How quickly a voice that was taken for a new note gets out of the way.
const STEAL_SECONDS: f32 = 0.005;
/// How quickly a voice goes that another group silences at once.
const FAST_OFF_SECONDS: f32 = 0.006;
/// The fade of a note whose recording runs out before the rest is read.
const SHORT_SECONDS: f32 = 0.005;
/// The fade at an end that the instrument sets inside a recording.
const END_SECONDS: f32 = 0.002;
/// No release is shorter: an instrument that says nothing of its release
/// would otherwise end every note in a click.
const SHORTEST_RELEASE: f32 = 0.012;
/// An attack shorter than this starts the note fully open, so that a drum
/// keeps its first frame.
const INSTANT_ATTACK: f32 = 0.0005;
/// How long a controller takes to cover most of the way to where it was
/// put: fast enough to follow a swell, slow enough that a step is no click.
const CONTROLLER_SECONDS: f32 = 0.004;
/// An envelope under this level is at its end.
const SILENT: f32 = 1e-4;

define_params!(P, PARAMS, [
    Volume => float(
        "volume",
        "Volume (0 plays the instrument at the level its maker set; silent at -70)",
        SILENT_DB as f64,
        12.0,
        -6.0,
        "dB"
    ),
    Pan => float("pan", "Pan", -1.0, 1.0, 0.0, ""),
    Transpose => int("transpose", "Transpose", -48, 48, 0, "st"),
    Tune => float("tune", "Fine Tune", -100.0, 100.0, 0.0, "cents"),
    Dynamics => float(
        "dynamics",
        "Dynamics (how much of a note's level its velocity decides, against what the instrument says: 1 as it was \
         made, 0 every note as loud as the hardest, 2 twice the difference in dB; which recording a velocity \
         plays is not changed)",
        0.0,
        2.0,
        1.0,
        ""
    ),
    Voices => int(
        "voices",
        "Voices (the most that sound at once; a piano held by its pedal wants many)",
        1,
        MAX_VOICES as i32,
        64,
        ""
    ),
    BendRange => float(
        "bend_range",
        "Pitch Wheel Range (how far the wheel bends, each way; 0 leaves it to the instrument, which bends 2 unless \
         its file says otherwise)",
        0.0,
        48.0,
        0.0,
        "st"
    ),
]);

pub const DESCRIPTION: &str = "Plays an instrument that was recorded note by note: a piano, an electric piano, \
strings, a bass, a drum kit. The instrument is a folder of recordings and a file that says which recording plays for \
which keys and how hard they are struck; SFZ files (.sfz) are read. It is not a parameter: it is the device's \
instrument, stored in the device state, and is given as a preset by the whole path of its .sfz file (search_presets \
lists the ones in the library). Without one the device is silent. Loading one says what it is: its range, its layers \
of loudness and takes, whether its keys are drums (the track's drum names are then the kit's), and the ways of \
playing it switches between by a key (the track's articulations are then the instrument's). Several recordings of \
one note are taken in turn or at random, the same in every render. The sustain pedal (controller 64) holds notes. \
What an instrument's file asks for that this player does not do is said when it is loaded, by name; it still plays. \
The start of every recording is kept in memory and the rest is read when its note is played, so a large instrument \
loads in a moment and a render waits for what it needs.";

/// What the parameters come to.
#[derive(Copy, Clone, Default)]
struct Derived {
    gain_left: f32,
    gain_right: f32,
    /// The speed transposing and tuning give every recording.
    ratio: f64,
    dynamics: f32,
    voices: usize,
    /// The wheel's range in cents, where the parameter sets it.
    bend: Option<f32>,
}

#[derive(Copy, Clone, PartialEq)]
enum Phase {
    Delay,
    Attack,
    Hold,
    Decay,
    Release,
    Done,
}

/// An envelope as a voice runs it: one level a frame.
#[derive(Copy, Clone)]
struct Shape {
    phase: Phase,
    level: f32,
    /// Frames left of the delay or the hold.
    left: u32,
    hold: u32,
    /// The level the attack begins at, and whether there is no attack
    /// to speak of.
    start: f32,
    instant: bool,
    attack_step: f32,
    decay: f32,
    sustain: f32,
    release: f32,
}

impl Shape {
    const fn idle() -> Self {
        Self {
            phase: Phase::Done,
            level: 0.0,
            left: 0,
            hold: 0,
            start: 0.0,
            instant: true,
            attack_step: 1.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        }
    }

    /// The envelope of a note of `velocity`, with the controllers where
    /// `controller` has them. `floor` is the shortest release.
    fn of(
        envelope: &super::model::Envelope,
        velocity: f32,
        controller: impl Fn(u8, u16) -> f32 + Copy,
        sample_rate: f32,
        floor: f32,
    ) -> Self {
        let at = |stage: Stage| envelope.stage(stage, velocity, controller);
        // Falling 60 dB over the time that is given.
        let falling = |seconds: f32| if seconds > 0.0 { (-6.9078 / (seconds * sample_rate)).exp() } else { 0.0 };
        let (delay, attack, hold) = (at(Stage::Delay), at(Stage::Attack), at(Stage::Hold));
        let frames = |seconds: f32| (seconds * sample_rate).round().max(0.0) as u32;
        let mut shape = Self {
            phase: Phase::Delay,
            level: 0.0,
            left: frames(delay),
            hold: frames(hold),
            start: envelope.start,
            instant: attack < INSTANT_ATTACK,
            attack_step: 1.0 / (attack * sample_rate).max(1.0),
            decay: falling(at(Stage::Decay)),
            sustain: at(Stage::Sustain),
            release: falling(at(Stage::Release).max(floor)),
        };
        if shape.left == 0 {
            shape.begin();
        }
        shape
    }

    /// The delay is over: the attack begins, or is over at once.
    fn begin(&mut self) {
        if self.instant {
            self.level = 1.0;
            self.phase = Phase::Hold;
            self.left = self.hold;
        } else {
            self.level = self.start;
            self.phase = Phase::Attack;
        }
    }

    #[inline]
    fn tick(&mut self) -> f32 {
        match self.phase {
            Phase::Delay => {
                if self.left > 0 {
                    self.left -= 1;
                    return 0.0;
                }
                self.begin();
            }
            Phase::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.phase = Phase::Hold;
                    self.left = self.hold;
                }
            }
            Phase::Hold => {
                if self.left > 0 {
                    self.left -= 1;
                } else {
                    self.phase = Phase::Decay;
                }
            }
            Phase::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay;
                if self.sustain <= 0.0 && self.level < SILENT {
                    self.level = 0.0;
                    self.phase = Phase::Done;
                }
            }
            Phase::Release => {
                self.level *= self.release;
                if self.level < SILENT {
                    self.level = 0.0;
                    self.phase = Phase::Done;
                }
            }
            Phase::Done => {}
        }
        self.level.max(0.0)
    }

    fn release(&mut self) {
        match self.phase {
            Phase::Done => {}
            // A note let go of before it began never sounds.
            Phase::Delay => self.phase = Phase::Done,
            _ => self.phase = Phase::Release,
        }
    }

    fn done(&self) -> bool {
        self.phase == Phase::Done
    }
}

/// Where a voice goes round while its note is held.
#[derive(Copy, Clone)]
struct Round {
    start: f64,
    /// One past the last frame of the loop.
    end: f64,
    /// Frames over which its end is faded into its start.
    crossfade: f64,
}

/// A voice's filter.
#[derive(Copy, Clone)]
struct Tone {
    kind: FilterKind,
    stages: [[Svf; 2]; 2],
    poles: [OnePole; 2],
    /// What the note itself moves the cutoff by, in cents.
    cents: f32,
    envelope: Shape,
    depth: f32,
    q: f32,
}

impl Tone {
    #[inline]
    fn tick(&mut self, left: f32, right: f32) -> (f32, f32) {
        match self.kind {
            FilterKind::LowPass1 => (self.poles[0].low(left), self.poles[1].low(right)),
            FilterKind::HighPass1 => (self.poles[0].high(left), self.poles[1].high(right)),
            kind => {
                let [first, second] = &mut self.stages;
                let (l, r) = (first[0].tick(left), first[1].tick(right));
                match kind {
                    FilterKind::HighPass2 => (l.high, r.high),
                    FilterKind::BandPass2 => (l.band, r.band),
                    FilterKind::LowPass4 => (second[0].tick(l.low).low, second[1].tick(r.low).low),
                    _ => (l.low, r.low),
                }
            }
        }
    }

    fn set(&mut self, cutoff: f32, sample_rate: f32) {
        for channel in 0..2 {
            self.poles[channel].set_cutoff(cutoff, sample_rate);
            self.stages[0][channel].set(cutoff, self.q, sample_rate);
            self.stages[1][channel].set(cutoff, 0.6, sample_rate);
        }
    }
}

#[derive(Clone)]
struct Voice {
    active: bool,
    /// It has yet to work out where it stands.
    fresh: bool,
    region: u32,
    recording: u32,
    /// The key that sounded it.
    key: u8,
    /// A count of notes: which is older.
    age: u64,
    /// It ends when its key, or the pedal that holds it, lets go.
    held: bool,
    /// A key going down sounded it.
    struck: bool,
    audio: Option<Arc<Audio>>,
    /// It reads from a start, and wants the rest.
    on_start: bool,
    /// The rest did not come: it is fading out.
    short: bool,
    position: f64,
    /// One past the last frame it plays.
    end: f64,
    /// The end is set inside the recording, and is faded to.
    fade_end: bool,
    round: Option<Round>,
    /// It goes round only while its note is held.
    round_while_held: bool,
    rounding: bool,
    been_round: bool,
    ended: bool,
    /// The speed of the note itself, and with what moves it.
    speed: f64,
    rate: f64,
    kernel: &'static Kernel,
    cents: f32,
    /// The speed the device's own tuning had when the rate was worked out.
    ratio: f64,
    /// Frames before it sounds.
    wait: u32,
    /// The level of the note itself.
    gain: f32,
    /// How much of a stereo recording's width is kept.
    width: f32,
    left_gain: f32,
    right_gain: f32,
    left_step: f32,
    right_step: f32,
    envelope: Shape,
    fade: FadeOut,
    tone: Option<Tone>,
    /// Seconds it has sounded, for what begins late.
    seconds: f32,
}

impl Voice {
    fn idle() -> Self {
        Self {
            active: false,
            fresh: false,
            region: 0,
            recording: 0,
            key: 0,
            age: 0,
            held: false,
            struck: false,
            audio: None,
            on_start: false,
            short: false,
            position: 0.0,
            end: 0.0,
            fade_end: false,
            round: None,
            round_while_held: false,
            rounding: false,
            been_round: false,
            ended: false,
            speed: 1.0,
            rate: 1.0,
            kernel: kernel_for(1.0),
            cents: 0.0,
            ratio: 0.0,
            wait: 0,
            gain: 0.0,
            width: 1.0,
            left_gain: 0.0,
            right_gain: 0.0,
            left_step: 0.0,
            right_step: 0.0,
            envelope: Shape::idle(),
            fade: FadeOut::idle(),
            tone: None,
            seconds: 0.0,
        }
    }

    fn stop(&mut self) {
        self.active = false;
        self.held = false;
        // The pool holds it too, so this frees nothing.
        self.audio = None;
    }

    /// Still a note in its own right, not one on its way out.
    fn playing(&self) -> bool {
        self.active && !self.fade.fading()
    }

    fn let_go(&mut self) {
        self.held = false;
        self.envelope.release();
        if let Some(tone) = &mut self.tone {
            tone.envelope.release();
        }
        if self.round_while_held {
            self.rounding = false;
        }
    }

    /// One frame of the recording, and on to the next.
    #[inline]
    fn next(&mut self, audio: &Audio) -> (f32, f32) {
        let position = self.position;
        let frame = match self.round.filter(|_| self.rounding) {
            Some(round) => {
                let length = round.end - round.start;
                let reach = self.kernel.half() as f64;
                if round.crossfade > 0.0 && position >= round.end - round.crossfade {
                    // The end of the loop gives way to what leads into its start.
                    let along = ((position - (round.end - round.crossfade)) / round.crossfade) as f32;
                    let (out, into) = (read(audio, self.kernel, position), read(audio, self.kernel, position - length));
                    (out.0 + (into.0 - out.0) * along, out.1 + (into.1 - out.1) * along)
                } else if round.crossfade == 0.0
                    && (position + reach >= round.end || (self.been_round && position - reach < round.start))
                {
                    // Beside the join, what is read past the end is the start, and the other way round.
                    let (start, end, length) = (round.start as i64, round.end as i64, length as i64);
                    let been_round = self.been_round;
                    read_mapped(audio, self.kernel, position, move |index| {
                        if index >= end {
                            index - length
                        } else if been_round && index < start {
                            index + length
                        } else {
                            index
                        }
                    })
                } else {
                    read(audio, self.kernel, position)
                }
            }
            None => read(audio, self.kernel, position),
        };
        self.position += self.rate;
        match self.round.filter(|_| self.rounding) {
            Some(round) => {
                if self.position >= round.end {
                    self.position -= round.end - round.start;
                    self.been_round = true;
                }
            }
            None => self.ended |= self.position >= self.end,
        }
        frame
    }
}

/// What a voice is told of the moment when it works out where it stands.
struct Moment<'a> {
    loaded: &'a Loaded,
    pool: &'a Pool,
    may_wait: bool,
    sample_rate: f32,
    derived: &'a Derived,
    /// Where the controllers are, on their way.
    controllers: &'a [f32; 128],
    wheel: f32,
    /// Frames to the next point of the grid.
    frames: usize,
}

impl Voice {
    /// Works out where the voice stands: what it reads from, its speed,
    /// its level on each side, its filter.
    fn place(&mut self, moment: &Moment) {
        let region = &moment.loaded.instrument.regions[self.region as usize];
        let sample_rate = moment.sample_rate;
        let curves = &moment.loaded.instrument.curves;
        let controller = |number: u8| moment.controllers[usize::from(number)];

        // The pitch: the wheel, controllers, vibrato.
        let (up, down) = match moment.derived.bend {
            Some(cents) => (cents, -cents),
            None => (region.bend_up, region.bend_down),
        };
        let mut cents = if moment.wheel >= 0.0 { moment.wheel * up } else { -moment.wheel * down };
        let (mut vibrato, mut tremolo, mut volume, mut share, mut pan, mut cutoff) = (0.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        let mut width = region.width;
        for control in &region.controls {
            let position = along(curves, control.curve, controller(control.controller));
            match control.moved {
                Moved::Volume => volume += control.depth * position,
                Moved::Amplitude => share *= control.depth / 100.0 * position,
                Moved::Pan => pan += control.depth / 100.0 * position,
                Moved::Tune => cents += control.depth * position,
                Moved::Cutoff => cutoff += control.depth * position,
                Moved::Vibrato => vibrato += control.depth * position,
                Moved::Tremolo => tremolo += control.depth * position,
                Moved::Width => width += control.depth / 100.0 * position,
                // Read once, when the note began.
                Moved::Dynamics => {}
            }
        }
        let wave = |wobble: &super::model::Wobble, seconds: f32| {
            let since = seconds - wobble.delay;
            if since <= 0.0 {
                return 0.0;
            }
            let arrived = if wobble.fade > 0.0 { (since / wobble.fade).min(1.0) } else { 1.0 };
            arrived * (std::f32::consts::TAU * wobble.rate * since).sin()
        };
        if let Some(wobble) = &region.vibrato {
            cents += (wobble.depth + vibrato) * wave(wobble, self.seconds);
        }
        if self.fresh || cents != self.cents || moment.derived.ratio != self.ratio {
            (self.cents, self.ratio) = (cents, moment.derived.ratio);
            self.rate = self.speed * self.ratio * 2f64.powf(f64::from(cents) / 1_200.0);
            self.kernel = kernel_for(self.rate);
        }

        // What it reads from: the whole recording as soon as that is there.
        if self.on_start {
            let recording = &moment.loaded.recordings[self.recording as usize];
            if let Some(whole) = recording.whole_if_there() {
                self.audio = Some(whole);
                self.on_start = false;
            } else if !self.short
                && let Some(start) = &self.audio
            {
                // How far it reads before it next looks, with room to fade.
                let ahead = (moment.frames as f32 + SHORT_SECONDS * sample_rate + 2.0) as f64;
                let needed = self.position + self.rate * ahead + self.kernel.half() as f64;
                if needed >= start.reach() as f64 {
                    match moment.may_wait.then(|| moment.pool.whole_now(recording)).flatten() {
                        Some(whole) => {
                            self.audio = Some(whole);
                            self.on_start = false;
                        }
                        None => {
                            self.short = true;
                            self.fade.start(SHORT_SECONDS, sample_rate);
                            moment.pool.cut_short();
                        }
                    }
                }
            }
        }
        if self.fade_end {
            let ahead = (moment.frames as f32 + END_SECONDS * sample_rate) as f64;
            if self.position + self.rate * ahead >= self.end {
                self.fade.start(END_SECONDS, sample_rate);
            }
        }

        // The level on each side.
        let mut gain = self.gain * share * db_to_gain(volume);
        for (number, fade) in &region.fade_controllers {
            gain *= fade.gain(controller(*number) * 127.0, region.controller_curve);
        }
        if let Some(wobble) = &region.tremolo {
            gain *= db_to_gain((wobble.depth + tremolo) * wave(wobble, self.seconds));
        }
        self.width = width.clamp(-1.0, 1.0);
        let (pan_left, pan_right) = pan_gains((region.pan + pan).clamp(-1.0, 1.0));
        let (left, right) = (gain * pan_left * moment.derived.gain_left, gain * pan_right * moment.derived.gain_right);
        if self.fresh {
            (self.left_gain, self.right_gain, self.left_step, self.right_step) = (left, right, 0.0, 0.0);
        } else {
            let frames = moment.frames.max(1) as f32;
            self.left_step = (left - self.left_gain) / frames;
            self.right_step = (right - self.right_gain) / frames;
        }

        if let (Some(tone), Some(filter)) = (&mut self.tone, &region.filter) {
            let cents = tone.cents + cutoff + tone.depth * tone.envelope.level;
            tone.set(filter.cutoff * 2f32.powf(cents / 1_200.0), sample_rate);
        }
        self.seconds += moment.frames as f32 / sample_rate;
        self.fresh = false;
    }

    /// Adds the voice's next frames to the output. `levels` is the level
    /// of the whole instrument at each of them.
    fn render(&mut self, levels: &[f32], left: &mut [f32], right: &mut [f32]) {
        // Taken out while it is read, and put back: nothing is counted.
        let Some(audio) = self.audio.take() else {
            self.stop();
            return;
        };
        let width = if audio.is_mono() { 1.0 } else { self.width };
        for ((left, right), level) in left.iter_mut().zip(right.iter_mut()).zip(levels) {
            if self.wait > 0 {
                self.wait -= 1;
                continue;
            }
            if self.ended {
                // Past its last frame there is nothing to read, however
                // much of the buffer is left.
                break;
            }
            let envelope = self.envelope.tick();
            let fade = self.fade.tick();
            let (mut l, mut r) = self.next(&audio);
            if width != 1.0 {
                let (middle, side) = ((l + r) * 0.5, (l - r) * 0.5 * width);
                (l, r) = (middle + side, middle - side);
            }
            if let Some(tone) = &mut self.tone {
                tone.envelope.tick();
                (l, r) = tone.tick(l, r);
            }
            let level = envelope * fade * level;
            *left += l * level * self.left_gain;
            *right += r * level * self.right_gain;
            self.left_gain += self.left_step;
            self.right_gain += self.right_step;
        }
        self.audio = Some(audio);
        if self.envelope.done() || self.fade.finished() || self.ended {
            self.stop();
        }
    }
}

/// A number from 0 up to 1 that is the same for the same `seed`.
fn chance(seed: u64, which: u64) -> f32 {
    let mut x = seed ^ which.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// The sampled instrument. See the module documentation.
pub struct Player {
    params: ParamValues,
    derived: Derived,
    loaded: Option<Arc<Loaded>>,
    pool: Option<Pool>,
    /// Kept while the device plays live: the pool's workers look often
    /// for what it wants.
    _playing: Option<super::pool::Player>,
    may_wait: bool,
    sample_rate: f64,
    voices: Vec<Voice>,
    /// For each region, how many notes it has been the turn of.
    turns: Vec<u32>,
    /// The keys that are down, a bit each; how hard each was struck; and
    /// the frame it went down on.
    down: u128,
    struck: [f32; 128],
    since: [u64; 128],
    pedal: Pedal,
    last_switch: Option<u8>,
    previous: Option<u8>,
    /// Where each controller was put, where it is on its way there, and
    /// the ones the instrument listens to.
    put: [f32; 128],
    controllers: [f32; 128],
    listened: Vec<u8>,
    /// The instrument does something of its own with channel volume or
    /// with expression: then they are not the whole instrument's level.
    own_volume: bool,
    own_expression: bool,
    wheel: f32,
    level: ChannelLevel,
    /// Frames processed, and notes played.
    frame: u64,
    notes: u64,
    clock: u64,
}

impl Player {
    /// A player of `loaded`, or a silent one. `pool` is where its
    /// recordings are; `purpose` says whether it may wait for them.
    pub fn new(loaded: Option<Arc<Loaded>>, pool: Option<Pool>, purpose: Purpose) -> Self {
        super::read::prepare();
        let live = purpose == Purpose::Playing;
        if let (Some(loaded), Some(pool), true) = (&loaded, &pool, live) {
            pool.read_starts(&loaded.recordings);
        }
        let mut put = [0.0f32; 128];
        put[usize::from(CONTROLLER_VOLUME)] = 1.0;
        put[usize::from(CONTROLLER_EXPRESSION)] = 1.0;
        put[10] = 0.5;
        let (mut listened, mut last_switch, mut regions) = (Vec::new(), None, 0);
        if let Some(loaded) = &loaded {
            for (number, position) in &loaded.instrument.controller_starts {
                put[usize::from(*number)] = *position;
            }
            listened = loaded.instrument.controllers();
            last_switch = loaded.instrument.switches.default;
            regions = loaded.instrument.regions.len();
        }
        Self {
            params: ParamValues::new(PARAMS),
            derived: Derived::default(),
            _playing: pool.as_ref().filter(|_| live && loaded.is_some()).map(Pool::player),
            own_volume: listened.contains(&CONTROLLER_VOLUME),
            own_expression: listened.contains(&CONTROLLER_EXPRESSION),
            loaded,
            pool,
            may_wait: !live,
            sample_rate: 48_000.0,
            voices: vec![Voice::idle(); MAX_VOICES + SPARE_VOICES],
            turns: vec![0; regions],
            down: 0,
            struck: [0.0; 128],
            since: [0; 128],
            pedal: Pedal::default(),
            last_switch,
            previous: None,
            put,
            controllers: put,
            listened,
            wheel: 0.0,
            level: ChannelLevel::new(),
            frame: 0,
            notes: 0,
            clock: 0,
        }
    }

    fn refresh(&mut self) {
        self.params.changed = false;
        let p = &self.params;
        let gain = level_gain(p.get(P::Volume as usize));
        let (pan_left, pan_right) = pan_gains(p.get(P::Pan as usize));
        let semitones = f64::from(p.get(P::Transpose as usize)) + f64::from(p.get(P::Tune as usize)) * 0.01;
        let bend = p.get(P::BendRange as usize);
        self.derived = Derived {
            gain_left: gain * pan_left,
            gain_right: gain * pan_right,
            ratio: 2f64.powf(semitones / 12.0),
            dynamics: p.get(P::Dynamics as usize),
            voices: p.index(P::Voices as usize).clamp(1, MAX_VOICES),
            bend: (bend > 0.0).then_some(bend * 100.0),
        };
    }

    fn is_down(&self, key: u8) -> bool {
        self.down & (1 << key.min(127)) != 0
    }

    /// Whether `region` is to sound for a note of `velocity`: by how hard
    /// it is struck, where the controllers are, the switches, its turn
    /// and its share of the note's `chance`.
    fn sounds(&mut self, region: &Region, index: usize, key: u8, velocity: f32, chance: f32) -> bool {
        let struck = (velocity * 127.0).round().clamp(0.0, 127.0) as u8;
        if !(region.velocities.0..=region.velocities.1).contains(&struck) {
            return false;
        }
        for (number, low, high) in &region.when {
            let position = self.put[usize::from(*number)] * 127.0;
            if position < low - 0.5 || position > high + 0.5 {
                return false;
            }
        }
        let switched = &region.switched;
        if let Some((low, high)) = switched.last
            && !self.last_switch.is_some_and(|last| (low..=high).contains(&last))
        {
            return false;
        }
        if switched.down.is_some_and(|key| !self.is_down(key)) || switched.up.is_some_and(|key| self.is_down(key)) {
            return false;
        }
        if switched.previous.is_some_and(|key| self.previous != Some(key)) {
            return false;
        }
        let others = self.down & !(1 << key.min(127)) != 0;
        match region.trigger {
            Trigger::First if others => return false,
            Trigger::Legato if !others => return false,
            _ => {}
        }
        // Its turn among the takes: counted for every note that comes this far.
        let (length, turn) = region.turn;
        if length > 1 {
            let count = self.turns[index];
            self.turns[index] = count.wrapping_add(1);
            if count % u32::from(length) != u32::from(turn - 1) % u32::from(length) {
                return false;
            }
        }
        chance >= region.random.0 && (chance < region.random.1 || region.random.1 >= 1.0)
    }

    /// Starts a voice on `region` for `key`. `gain` is what the moment
    /// adds to the level (a release sound grows quieter the longer its
    /// note was held), `seed` the note's own number.
    fn start(&mut self, loaded: &Loaded, index: usize, key: u8, velocity: f32, gain: f32, seed: u64) {
        let region = &loaded.instrument.regions[index];
        let sample_rate = self.sample_rate as f32;
        let own = seed ^ (index as u64 + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93);
        // Voices this one silences: the groups it turns off, and its own
        // key's where the instrument allows only so many.
        if region.group != 0 {
            for voice in self.voices.iter_mut().filter(|voice| voice.active && voice.age != self.clock) {
                let other = &loaded.instrument.regions[voice.region as usize];
                if other.off_by == Some(region.group) {
                    match other.off_mode {
                        OffMode::Fast => voice.fade.start(FAST_OFF_SECONDS, sample_rate),
                        OffMode::Time(seconds) => voice.fade.start(seconds.max(0.001), sample_rate),
                        OffMode::Normal => voice.let_go(),
                    }
                }
            }
        }
        if let Some(most) = region.polyphony {
            // Its group has only so many voices: the oldest gives way.
            let same = |voice: &&mut Voice| {
                voice.playing()
                    && voice.age != self.clock
                    && loaded.instrument.regions[voice.region as usize].group == region.group
            };
            while self.voices.iter_mut().filter(same).count() >= usize::from(most) {
                let Some(voice) = self.voices.iter_mut().filter(same).min_by_key(|voice| voice.age) else { break };
                match loaded.instrument.regions[voice.region as usize].off_mode {
                    OffMode::Fast => voice.fade.start(FAST_OFF_SECONDS, sample_rate),
                    OffMode::Time(seconds) => voice.fade.start(seconds.max(0.001), sample_rate),
                    // By its own release, and out of the count from now.
                    OffMode::Normal => {
                        voice.let_go();
                        voice.fade.start(region.envelope.release.max(SHORTEST_RELEASE) * 4.0, sample_rate);
                    }
                }
            }
        }
        if let Some(most) = region.note_polyphony {
            let same = |voice: &&mut Voice| {
                voice.playing()
                    && voice.key == key
                    && voice.age != self.clock
                    && loaded.instrument.regions[voice.region as usize].group == region.group
            };
            let sounding = self.voices.iter_mut().filter(same).count();
            if sounding >= usize::from(most) {
                let oldest = self.voices.iter_mut().filter(same).min_by_key(|voice| voice.age);
                if let Some(voice) = oldest {
                    voice.fade.start(FAST_OFF_SECONDS, sample_rate);
                }
            }
        }
        let (Some(recording), Some(pool)) = (region.recording, &self.pool) else { return };
        let stored = &loaded.recordings[recording as usize];
        let found = if self.may_wait { pool.start_now(stored) } else { stored.start() };
        let Some((facts, start)) = found else {
            if !stored.is_settled() {
                pool.not_ready();
            }
            return;
        };
        pool.played(stored);
        let (mut audio, mut on_start) = (start.clone(), !start.is_whole());
        if on_start {
            let whole = if self.may_wait { pool.whole_now(stored) } else { stored.whole_if_there() };
            match whole {
                Some(whole) => (audio, on_start) = (whole, false),
                None if self.may_wait => return,
                None => pool.want(stored),
            }
        }

        let frames = facts.frames;
        let offset = region.offset as usize + (region.offset_random as f32 * chance(own, 1)) as usize;
        let end = region.end.map_or(frames, |end| (end as usize + 1).min(frames));
        if offset >= end {
            return;
        }
        if on_start && offset + super::read::MAX_HALF + TICK * 8 >= audio.reach() {
            // It begins past what is in memory: not this time.
            pool.cut_short();
            return;
        }
        let mode =
            region.loop_mode.unwrap_or(if facts.file_loop.is_some() { LoopMode::Continuous } else { LoopMode::NoLoop });
        let round = match mode {
            LoopMode::Continuous | LoopMode::Sustain => {
                let first = region.loop_start.map(u64::from).or(facts.file_loop.map(|found| found.start)).unwrap_or(0);
                let last = region.loop_end.map(u64::from).or(facts.file_loop.map(|found| found.end));
                let last = last.unwrap_or(frames as u64 - 1).min(frames as u64 - 1);
                (last > first + 1).then(|| {
                    let (start, end) = (first as f64, (last + 1) as f64);
                    let crossfade = (f64::from(region.loop_crossfade) * facts.sample_rate).floor();
                    Round { start, end, crossfade: crossfade.min(start).min((end - start) * 0.5).max(0.0) }
                })
            }
            _ => None,
        };

        let from_root = f32::from(key) - f32::from(region.root);
        let cents = from_root * region.pitch_by_key
            + region.tune
            + region.pitch_by_velocity * velocity
            + region.pitch_random * (chance(own, 2) * 2.0 - 1.0);
        let speed = facts.sample_rate / self.sample_rate * 2f64.powf(f64::from(cents) / 1_200.0);
        let level = region.volume
            + region.level_by_key * (f32::from(key) - f32::from(region.level_centre))
            + region.level_random * (chance(own, 3) * 2.0 - 1.0);
        let put = self.put;
        let curves = &loaded.instrument.curves;
        let controller = |number: u8, curve: u16| along(curves, curve, put[usize::from(number)]);
        // How much the velocity decides: what the region says, moved by
        // the controllers the instrument has for that.
        let track = region
            .controls
            .iter()
            .filter(|control| control.moved == Moved::Dynamics)
            .fold(region.level_by_velocity, |track, control| {
                track + control.depth / 100.0 * controller(control.controller, control.curve)
            });
        let by_velocity = region.velocity_gain_tracking(velocity, track).max(0.0).powf(self.derived.dynamics);
        let gain = gain
            * db_to_gain(level)
            * region.amplitude
            * by_velocity
            * region.fade_keys.gain(f32::from(key), region.key_curve)
            * region.fade_velocities.gain(velocity * 127.0, region.velocity_fade_curve);
        let delay = region.delay + region.delay_random * chance(own, 4);
        let tone = region.filter.as_ref().map(|filter| {
            let (envelope, depth) = match &filter.envelope {
                Some((envelope, depth)) => (Shape::of(envelope, velocity, controller, sample_rate, 0.0), *depth),
                None => (Shape::idle(), 0.0),
            };
            Tone {
                kind: filter.kind,
                stages: Default::default(),
                poles: Default::default(),
                cents: filter.by_velocity * velocity + filter.by_key * (f32::from(key) - f32::from(filter.centre)),
                envelope,
                depth,
                q: std::f32::consts::FRAC_1_SQRT_2 * db_to_gain(filter.resonance),
            }
        });

        // Make room: voices over the limit fade out quickly.
        while self.voices.iter().filter(|voice| voice.playing()).count() >= self.derived.voices {
            let victim = steal(&self.voices);
            self.voices[victim].fade.start(STEAL_SECONDS, sample_rate);
        }
        let slot = free_voice(&self.voices);
        self.voices[slot] = Voice {
            active: true,
            fresh: true,
            region: index as u32,
            recording,
            key,
            age: self.clock,
            held: region.on_key_down() && mode != LoopMode::OneShot,
            struck: region.on_key_down(),
            audio: Some(audio),
            on_start,
            short: false,
            position: offset as f64,
            end: end as f64,
            fade_end: end < frames,
            round,
            round_while_held: mode == LoopMode::Sustain,
            rounding: round.is_some(),
            been_round: false,
            ended: false,
            speed,
            rate: speed,
            kernel: kernel_for(speed),
            cents: 0.0,
            ratio: 0.0,
            wait: (delay * sample_rate).round().max(0.0) as u32,
            gain,
            width: region.width,
            left_gain: 0.0,
            right_gain: 0.0,
            left_step: 0.0,
            right_step: 0.0,
            envelope: Shape::of(&region.envelope, velocity, controller, sample_rate, SHORTEST_RELEASE),
            fade: FadeOut::idle(),
            tone,
            seconds: 0.0,
        };
    }

    fn note_on(&mut self, loaded: &Loaded, key: u8, velocity: f32, place: u64) {
        let key = key.min(127);
        self.clock += 1;
        self.notes += 1;
        if loaded.instrument.is_switch(key) {
            self.last_switch = Some(key);
        }
        // A key struck again lets its earlier note go, as lifting it would.
        for voice in self.voices.iter_mut().filter(|voice| voice.active && voice.held && voice.key == key) {
            voice.let_go();
        }
        self.pedal.key_down(key);
        let seed = place.rotate_left(8) ^ u64::from(key);
        let number = chance(seed, 0);
        for at in 0..loaded.down[usize::from(key)].len() {
            let index = loaded.down[usize::from(key)][at] as usize;
            let region = &loaded.instrument.regions[index];
            if self.sounds(region, index, key, velocity, number) {
                self.start(loaded, index, key, velocity, 1.0, seed);
            }
        }
        self.down |= 1 << key;
        self.struck[usize::from(key)] = velocity;
        self.since[usize::from(key)] = self.frame;
        self.previous = Some(key);
    }

    /// The sounds of a key coming up, of the kind `trigger`.
    fn come_up(&mut self, loaded: &Loaded, key: u8, trigger: Trigger, place: u64) {
        if loaded.up[usize::from(key)].is_empty() {
            return;
        }
        // Only for a note that still sounds: a recording that has run out
        // has no ring to end.
        if !self.voices.iter().any(|voice| voice.active && voice.struck && voice.key == key && !voice.fade.fading()) {
            return;
        }
        self.clock += 1;
        let velocity = self.struck[usize::from(key)];
        let held = self.frame.saturating_sub(self.since[usize::from(key)]) as f32 / self.sample_rate as f32;
        let seed = place.rotate_left(8) ^ u64::from(key) ^ 0x05EE_D0FF_5EED_u64;
        let number = chance(seed, 0);
        for at in 0..loaded.up[usize::from(key)].len() {
            let index = loaded.up[usize::from(key)][at] as usize;
            let region = &loaded.instrument.regions[index];
            if region.trigger == trigger && self.sounds(region, index, key, velocity, number) {
                let gain = db_to_gain(-region.release_decay * held);
                self.start(loaded, index, key, velocity, gain, seed);
            }
        }
    }

    /// The note of `key` ends: the key is up and no pedal holds it.
    fn end_note(&mut self, loaded: &Loaded, key: u8, place: u64) {
        self.come_up(loaded, key, Trigger::Release, place);
        for voice in self.voices.iter_mut().filter(|voice| voice.active && voice.held && voice.key == key) {
            voice.let_go();
        }
    }

    fn handle(&mut self, loaded: &Loaded, kind: EventKind, place: u64) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => self.note_on(loaded, pitch, velocity, place),
            EventKind::NoteOff { pitch } => {
                let key = pitch.min(127);
                if !self.is_down(key) {
                    return;
                }
                self.down &= !(1 << key);
                self.come_up(loaded, key, Trigger::ReleaseKey, place);
                let held_by_pedal = loaded.instrument.pedal && !self.pedal.key_up(key);
                if !held_by_pedal {
                    self.end_note(loaded, key, place);
                }
            }
            EventKind::AllNotesOff => {
                self.pedal.clear();
                self.down = 0;
                self.voices.iter_mut().filter(|voice| voice.active).for_each(Voice::let_go);
            }
            EventKind::PitchBend { value } => {
                self.wheel = if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 };
            }
            EventKind::Control { controller, value } => {
                if !value.is_finite() {
                    return;
                }
                let number = controller.min(127);
                self.put[usize::from(number)] = value.clamp(0.0, 1.0);
                let sounding = self.voices.iter().any(|voice| voice.active);
                if !sounding {
                    self.controllers[usize::from(number)] = self.put[usize::from(number)];
                }
                if number == loaded.instrument.pedal_controller {
                    for key in self.pedal.set(value >= 0.5) {
                        if loaded.instrument.pedal && !self.is_down(key) {
                            self.end_note(loaded, key, place);
                        }
                    }
                }
                let own = (number == CONTROLLER_VOLUME && self.own_volume)
                    || (number == CONTROLLER_EXPRESSION && self.own_expression);
                if !own {
                    self.level.control(number, value, sounding);
                }
            }
            EventKind::ChannelPressure { .. } => {}
        }
    }

    /// Moves the controllers the instrument listens to a step on their
    /// way to where they were put.
    fn glide(&mut self) {
        let sounding = self.voices.iter().any(|voice| voice.active);
        let step = (TICK as f32 / (CONTROLLER_SECONDS * self.sample_rate as f32)).min(1.0);
        for number in &self.listened {
            let (now, put) = (&mut self.controllers[usize::from(*number)], self.put[usize::from(*number)]);
            let wanted = put - *now;
            if !sounding || wanted.abs() < 1e-5 {
                *now = put;
            } else {
                *now += wanted * step;
            }
        }
    }

    fn render(&mut self, loaded: &Loaded, at_point: bool, to_point: usize, left: &mut [f32], right: &mut [f32]) {
        let Some(pool) = &self.pool else { return };
        let sample_rate = self.sample_rate as f32;
        let mut levels = [1.0f32; TICK];
        self.level.fill(&mut levels[..left.len()], sample_rate);
        let moment = Moment {
            loaded,
            pool,
            may_wait: self.may_wait,
            sample_rate,
            derived: &self.derived,
            controllers: &self.controllers,
            wheel: self.wheel,
            frames: to_point,
        };
        for voice in self.voices.iter_mut().filter(|voice| voice.active) {
            if at_point || voice.fresh {
                voice.place(&moment);
            }
            voice.render(&levels[..left.len()], left, right);
        }
    }
}

/// The voice to give up when the limit is reached: the quietest one that
/// was let go of, or failing that the oldest.
fn steal(voices: &[Voice]) -> usize {
    let playing = || voices.iter().enumerate().filter(|(_, voice)| voice.playing());
    let released = playing()
        .filter(|(_, voice)| voice.envelope.phase == Phase::Release)
        .min_by(|a, b| a.1.envelope.level.total_cmp(&b.1.envelope.level));
    released.or_else(|| playing().min_by_key(|(_, voice)| voice.age)).map_or(0, |(index, _)| index)
}

/// The slot for a new note: a silent one, or failing that whichever
/// fading voice is nearest to silence.
fn free_voice(voices: &[Voice]) -> usize {
    if let Some(index) = voices.iter().position(|voice| !voice.active) {
        return index;
    }
    let fading = voices
        .iter()
        .enumerate()
        .filter(|(_, voice)| voice.fade.fading())
        .min_by(|a, b| a.1.fade.gain().total_cmp(&b.1.fade.gain()));
    fading.or_else(|| voices.iter().enumerate().min_by_key(|(_, voice)| voice.age)).map_or(0, |(index, _)| index)
}

impl DeviceProcessor for Player {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            self.refresh();
        }
        let Some(loaded) = self.loaded.clone() else { return };
        // Where the block begins in the song, in frames: what a note's
        // random number is drawn from while the song plays.
        let begins = (info.song.seconds * self.sample_rate).round() as i64;
        let len = left.len();
        let (mut pos, mut next_event) = (0, 0);
        while pos < len {
            while next_event < events.len() && events[next_event].offset as usize <= pos {
                let place = if info.playing { (begins + pos as i64) as u64 } else { (1 << 62) | self.notes };
                self.handle(&loaded, events[next_event].kind, place);
                next_event += 1;
            }
            let at_point = self.frame.is_multiple_of(TICK as u64);
            let to_point = TICK - (self.frame % TICK as u64) as usize;
            let next = events.get(next_event).map_or(len, |event| (event.offset as usize).min(len));
            let end = next.min(pos + to_point);
            if at_point {
                self.glide();
            }
            self.render(&loaded, at_point, to_point, &mut left[pos..end], &mut right[pos..end]);
            self.frame += (end - pos) as u64;
            pos = end;
        }
        for event in &events[next_event..] {
            let place = if info.playing { (begins + len as i64) as u64 } else { (1 << 62) | self.notes };
            self.handle(&loaded, event.kind, place);
        }
    }

    fn reset(&mut self) {
        self.pedal.clear();
        self.level.settle();
        self.down = 0;
        self.previous = None;
        self.controllers = self.put;
        // The takes are counted from here: where a song played from its
        // start and a render of it both begin.
        self.turns.iter_mut().for_each(|turn| *turn = 0);
        for voice in &mut self.voices {
            voice.stop();
        }
    }
}
