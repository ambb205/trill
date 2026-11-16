//! Drum Kit: synthesized drums on the General MIDI drum map.
//!
//! Every drum is a small patch of its own, made of what drums are made of: a
//! pitched body that drops in pitch as it is struck, band-limited noise for
//! wires, hands and sticks, and a cluster of inharmonic oscillators for
//! metal. How long each drum rings and where its energy sits follow
//! measurements of recorded one-shots rather than taste: hats and cymbals
//! centre around 7 kHz instead of hissing above 10, snares and claps ring for
//! a quarter of a second, kicks and toms hold their level before they fade.
//! `tests::the_drums_sit_where_recorded_drums_do` keeps it that way.

use crate::define_params;
use crate::params::{ParamValues, PresetDef, float};
use crate::util::{OnePole, Rng, Svf, db_to_gain, decay_coef, pan_gains};
use std::f32::consts::TAU;
use trill_core::device::{DeviceProcessor, Event, EventKind, ProcessInfo};

define_params!(P, PARAMS, [
    KickPitch => float("kick_pitch", "Kick Pitch", 30.0, 100.0, 50.0, "Hz"),
    KickDecay => float("kick_decay", "Kick Decay", 0.05, 2.0, 0.45, "s"),
    KickPunch => float("kick_punch", "Kick Punch", 0.0, 1.0, 0.6, ""),
    KickDrive => float("kick_drive", "Kick Drive", 0.0, 1.0, 0.2, ""),
    SnarePitch => float("snare_pitch", "Snare Pitch", 100.0, 300.0, 185.0, "Hz"),
    SnareDecay => float("snare_decay", "Snare Decay", 0.05, 1.0, 0.22, "s"),
    SnareNoise => float("snare_noise", "Snare Noise", 0.0, 1.0, 0.65, ""),
    HatDecay => float("hat_decay", "Closed Hat Decay", 0.01, 0.4, 0.06, "s"),
    OpenHatDecay => float("open_hat_decay", "Open Hat Decay", 0.1, 2.0, 0.45, "s"),
    HatTone => float("hat_tone", "Hat Brightness", 0.0, 1.0, 0.5, ""),
    TomDecay => float("tom_decay", "Tom Decay", 0.05, 1.5, 0.35, "s"),
    CymbalDecay => float("cymbal_decay", "Cymbal Decay", 0.2, 5.0, 1.6, "s"),
    KickLevel => float("kick_level", "Kick Level", 0.0, 2.0, 1.0, ""),
    SnareLevel => float("snare_level", "Snare Level", 0.0, 2.0, 1.0, ""),
    ClapLevel => float("clap_level", "Clap Level", 0.0, 2.0, 1.0, ""),
    HatLevel => float("hat_level", "Hat Level", 0.0, 2.0, 1.0, ""),
    TomLevel => float("tom_level", "Tom Level", 0.0, 2.0, 1.0, ""),
    CymbalLevel => float("cymbal_level", "Cymbal Level", 0.0, 2.0, 1.0, ""),
    PercLevel => float("perc_level", "Percussion Level", 0.0, 2.0, 1.0, ""),
    Volume => float("volume", "Volume", -48.0, 12.0, 0.0, "dB"),
]);

pub const DESCRIPTION: &str = "Synthesized drum machine on the General MIDI map: 36 kick, 37 rim, 38 snare, \
39 clap, 40 a tighter second snare, 42 closed hat, 44 pedal hat, 46 open hat (choked by 42 and 44), \
41/43 low toms, 45/47 mid toms, 48/50 high toms, 49 crash, 51 ride, 54 tambourine, 56 cowbell, 70 shaker, \
75 claves. Velocity sets loudness, and how hard the kick clicks and the snare cracks.";

pub const PRESETS: &[PresetDef] = &[
    PresetDef { name: "default", description: "Balanced electronic kit", params: &[] },
    PresetDef {
        name: "808",
        description: "Long booming kick, soft snare",
        params: &[
            ("kick_pitch", 46.0),
            ("kick_decay", 0.9),
            ("kick_punch", 0.25),
            ("kick_drive", 0.1),
            ("snare_pitch", 170.0),
            ("snare_decay", 0.2),
            ("snare_noise", 0.55),
            ("hat_decay", 0.05),
            ("open_hat_decay", 0.5),
        ],
    },
    PresetDef {
        name: "909",
        description: "Punchy house and techno kit",
        params: &[
            ("kick_pitch", 52.0),
            ("kick_decay", 0.4),
            ("kick_punch", 0.85),
            ("kick_drive", 0.45),
            ("snare_pitch", 200.0),
            ("snare_decay", 0.25),
            ("snare_noise", 0.8),
            ("hat_decay", 0.045),
            ("open_hat_decay", 0.35),
            ("hat_tone", 0.65),
        ],
    },
    PresetDef {
        name: "tight",
        description: "Short, dry kit for pop and funk",
        params: &[
            ("kick_pitch", 58.0),
            ("kick_decay", 0.22),
            ("kick_punch", 0.7),
            ("kick_drive", 0.3),
            ("snare_pitch", 210.0),
            ("snare_decay", 0.14),
            ("snare_noise", 0.6),
            ("hat_decay", 0.03),
            ("open_hat_decay", 0.2),
            ("hat_tone", 0.7),
            ("tom_decay", 0.2),
            ("cymbal_decay", 1.0),
        ],
    },
    PresetDef {
        name: "trap",
        description: "Sub-heavy sustained kick, crisp hats",
        params: &[
            ("kick_pitch", 42.0),
            ("kick_decay", 1.5),
            ("kick_punch", 0.4),
            ("kick_drive", 0.55),
            ("snare_pitch", 220.0),
            ("snare_decay", 0.16),
            ("snare_noise", 0.75),
            ("hat_decay", 0.025),
            ("open_hat_decay", 0.3),
            ("hat_tone", 0.8),
        ],
    },
    PresetDef {
        name: "lofi",
        description: "Dark, driven, dusty kit",
        params: &[
            ("kick_pitch", 55.0),
            ("kick_decay", 0.3),
            ("kick_punch", 0.5),
            ("kick_drive", 0.7),
            ("snare_pitch", 160.0),
            ("snare_decay", 0.2),
            ("snare_noise", 0.5),
            ("hat_decay", 0.07),
            ("open_hat_decay", 0.4),
            ("hat_tone", 0.15),
            ("cymbal_decay", 0.8),
        ],
    },
];

/// Six square waves at inharmonic ratios (those of the classic analog drum
/// machines), the raw material of hats and cymbals.
#[derive(Clone, Default)]
struct Metal {
    phases: [f32; 6],
}

const METAL_HZ: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

impl Metal {
    /// One sample of the cluster at `scale` times its base pitch, as two
    /// signals: the plain mix, which buzzes at its six pitches, and the
    /// three pairs multiplied together, whose partials are far denser and
    /// closer to the wash of a real cymbal.
    #[inline]
    fn tick(&mut self, scale: f32, inv_sr: f32) -> (f32, f32) {
        let mut square = [0.0f32; 6];
        for ((phase, hz), out) in self.phases.iter_mut().zip(METAL_HZ).zip(&mut square) {
            *phase += hz * scale * inv_sr;
            *phase -= phase.floor();
            *out = if *phase < 0.5 { 1.0 } else { -1.0 };
        }
        let mix = square.iter().sum::<f32>() * (1.0 / 6.0);
        let ring = (square[0] * square[3] + square[1] * square[4] + square[2] * square[5]) * (1.0 / 3.0);
        (mix, ring)
    }
}

/// Level of a drum over its life. `life` falls in a straight line from 1 to
/// 0 and the level is its square: the drum holds its level and then fades
/// out completely, where a plain exponential decay would drop away at once
/// and trail a long, quiet tail. Recorded kicks, toms, snares and claps all
/// have this shape; only metal rings out exponentially.
#[inline]
fn skin_level(life: f32) -> f32 {
    life * life
}

#[derive(Clone, Default)]
struct Kick {
    life: f32,
    /// The two stages of the pitch drop: the first few milliseconds, which
    /// are heard as the punch, and the slower settling onto the note.
    fast: f32,
    slow: f32,
    click: f32,
    phase: f32,
    gain: f32,
    click_gain: f32,
    filter: Svf,
}

impl Kick {
    fn hit(&mut self, gain: f32, velocity: f32) {
        (self.life, self.fast, self.slow, self.click, self.phase) = (1.0, 1.0, 1.0, 1.0, 0.0);
        self.gain = gain;
        self.click_gain = 0.35 + 0.65 * velocity;
    }

    #[inline]
    fn tick(&mut self, c: &Coefs, rng: &mut Rng) -> f32 {
        let hz = c.kick_hz * (1.0 + c.kick_fast_depth * self.fast + c.kick_slow_depth * self.slow);
        self.phase += hz * c.inv_sr;
        self.phase -= self.phase.floor();
        let (sin, cos) = (self.phase * TAU).sin_cos();
        let level = skin_level(self.life);
        // sin * cos is the second harmonic. It fades twice as fast as the
        // fundamental, as overtones of a skin do.
        let body = sin * level * (1.0 + 0.6 * cos * level);
        // The beater: a knock in the midrange, not a hiss.
        let click = self.filter.tick(c.noise(rng)).low * self.click * c.kick_click_level * self.click_gain;
        self.life -= c.kick_rate;
        self.fast *= c.kick_fast;
        self.slow *= c.kick_slow;
        self.click *= c.kick_click;
        ((body + click) * c.kick_drive).tanh() * c.kick_norm * self.gain
    }
}

#[derive(Clone, Default)]
struct Snare {
    body: f32,
    /// The wires: a short crack, and the rattle after it, which holds and
    /// fades as a skin does (see `skin_level`).
    crack: f32,
    rattle: f32,
    sweep: f32,
    phase1: f32,
    phase2: f32,
    tune: f32,
    gain: f32,
    wire_gain: f32,
    highpass: Svf,
    lowpass: Svf,
}

impl Snare {
    fn hit(&mut self, gain: f32, velocity: f32, tune: f32) {
        (self.body, self.crack, self.rattle, self.sweep) = (1.0, 1.0, 1.0, 1.0);
        (self.phase1, self.phase2) = (0.0, 0.0);
        self.tune = tune;
        self.gain = gain;
        // Soft hits are mostly drum, hard hits mostly wires.
        self.wire_gain = 0.5 + 0.5 * velocity;
    }

    #[inline]
    fn sounding(&self) -> bool {
        self.body > SILENT || self.rattle > 0.0
    }

    #[inline]
    fn tick(&mut self, c: &Coefs, rng: &mut Rng) -> f32 {
        let hz = c.snare_hz * self.tune * (1.0 + 0.5 * self.sweep);
        self.phase1 += hz * c.inv_sr;
        self.phase1 -= self.phase1.floor();
        // The second mode of a drum head lies 1.59 times above the first.
        self.phase2 += hz * 1.59 * c.inv_sr;
        self.phase2 -= self.phase2.floor();
        let body = ((self.phase1 * TAU).sin() * 0.8 + (self.phase2 * TAU).sin() * 0.45 * self.body) * self.body;
        let wires = self.lowpass.tick(self.highpass.tick(c.noise(rng)).high).low;
        let noise = wires * (skin_level(self.rattle.max(0.0)) + self.crack * 0.5) * c.snare_wires * self.wire_gain;
        self.body *= c.snare_body;
        self.crack *= c.snare_crack;
        self.rattle -= c.snare_rattle;
        self.sweep *= c.snare_sweep;
        // Drum and wires share one skin: pressing them together a little is
        // what makes them one sound rather than a tone with hiss on top.
        ((body + noise) * 0.6).tanh() * self.gain
    }
}

/// Several hands, a few milliseconds apart, and the room after them.
#[derive(Clone, Default)]
struct Clap {
    amp: f32,
    /// The room after the last hand, from 1 to 0 (see `skin_level`).
    room: f32,
    /// Samples since the trigger, and when the next hand lands.
    age: u32,
    next: u32,
    /// Hands still to come. The last one rings out.
    hands: u8,
    gain: f32,
    filters: [Svf; 2],
}

impl Clap {
    fn hit(&mut self, gain: f32, c: &Coefs, rng: &mut Rng) {
        self.amp = 1.0;
        self.room = 1.0;
        self.age = 0;
        self.hands = 3;
        self.next = c.clap_gap + rng.next_u32() % c.clap_jitter.max(1);
        self.gain = gain;
    }

    #[inline]
    fn tick(&mut self, c: &Coefs, rng: &mut Rng) -> (f32, f32) {
        if self.hands > 0 && self.age >= self.next {
            self.hands -= 1;
            self.amp = 1.0;
            self.next = self.age + c.clap_gap + rng.next_u32() % c.clap_jitter.max(1);
        }
        // A noise of its own for each side makes the clap as wide as a
        // roomful of hands; the sides share enough to survive mono.
        let a = self.filters[0].tick(c.noise(rng));
        let b = self.filters[1].tick(c.noise(rng));
        let (a, b) = (a.band, b.band);
        let level = if self.hands > 0 { self.amp } else { skin_level(self.room) } * self.gain;
        if self.hands > 0 {
            self.amp *= c.clap_burst;
        } else {
            self.room -= c.clap_tail;
        }
        self.age += 1;
        ((a * 0.8 + b * 0.3) * level, (b * 0.8 + a * 0.3) * level)
    }
}

#[derive(Clone, Default)]
struct Hat {
    amp: f32,
    decay: f32,
    gain: f32,
    metal: Metal,
    highpass: Svf,
    lowpass: Svf,
}

impl Hat {
    /// The oscillators keep running between hits, so no two hits start
    /// alike; together with the small spread in level this is what keeps a
    /// line of sixteenths from sounding like a machine gun.
    fn hit(&mut self, gain: f32, decay: f32, rng: &mut Rng) {
        self.amp = 1.0;
        self.decay = decay;
        self.gain = gain * (0.9 + 0.2 * rng.unipolar());
    }

    #[inline]
    fn tick(&mut self, c: &Coefs, rng: &mut Rng) -> f32 {
        let (mix, ring) = self.metal.tick(c.hat_scale, c.inv_sr);
        let source = mix * 0.5 + ring * 0.6 + c.noise(rng) * 0.45;
        let out = self.lowpass.tick(self.highpass.tick(source).high).low * self.amp * self.gain;
        self.amp *= self.decay;
        out
    }
}

#[derive(Clone, Default)]
struct Tom {
    life: f32,
    sweep: f32,
    stick: f32,
    phase: f32,
    hz: f32,
    gain: f32,
    filter: OnePole,
}

impl Tom {
    fn hit(&mut self, gain: f32, hz: f32) {
        (self.life, self.sweep, self.stick, self.phase) = (1.0, 1.0, 1.0, 0.0);
        self.hz = hz;
        self.gain = gain;
    }

    #[inline]
    fn tick(&mut self, c: &Coefs, rng: &mut Rng) -> f32 {
        self.phase += self.hz * (1.0 + 0.6 * self.sweep) * c.inv_sr;
        self.phase -= self.phase.floor();
        let (sin, cos) = (self.phase * TAU).sin_cos();
        let level = skin_level(self.life);
        let body = sin * level * (1.0 + 0.9 * cos * level);
        let stick = self.filter.low(c.noise(rng)) * self.stick * 1.1;
        self.life -= c.tom_rate;
        self.sweep *= c.tom_sweep;
        self.stick *= c.tom_stick;
        (body + stick) * self.gain
    }
}

/// What tells a crash from a ride: the pitch of the metal, how much of it
/// is wash, and how long it rings.
struct CymbalTone {
    scale: f32,
    /// Of the plain cluster, the multiplied pairs and noise.
    mix: [f32; 3],
    /// How far the stroke itself stands out over the ringing.
    stroke: f32,
}

const CRASH: CymbalTone = CymbalTone { scale: 2.4, mix: [0.15, 0.6, 0.6], stroke: 0.6 };
const RIDE: CymbalTone = CymbalTone { scale: 4.0, mix: [0.5, 0.45, 0.12], stroke: 0.4 };

#[derive(Clone, Default)]
struct Cymbal {
    amp: f32,
    stroke: f32,
    /// One-pole low-pass state. See `tick`.
    dark: f32,
    gain: f32,
    metal: Metal,
    highpass: Svf,
    lowpass: Svf,
}

impl Cymbal {
    fn hit(&mut self, gain: f32) {
        (self.amp, self.stroke) = (1.0, 1.0);
        self.gain = gain;
    }

    #[inline]
    fn tick(&mut self, tone: &CymbalTone, decay: f32, c: &Coefs, rng: &mut Rng) -> f32 {
        let (mix, ring) = self.metal.tick(tone.scale, c.inv_sr);
        let source = mix * tone.mix[0] + ring * tone.mix[1] + c.noise(rng) * tone.mix[2];
        let shaped = self.lowpass.tick(self.highpass.tick(source).high).low;
        // A cymbal loses its top as it rings out: the low-pass closes with
        // the level, from wide open at the stroke.
        self.dark += (shaped - self.dark) * (c.cymbal_dark + (1.0 - c.cymbal_dark) * self.amp);
        let out = self.dark * (self.amp + self.stroke * tone.stroke) * self.gain;
        self.amp *= decay;
        self.stroke *= c.cymbal_stroke;
        out
    }
}

/// Two decaying oscillators: rim shot, cowbell, claves.
#[derive(Clone, Default)]
struct Blip {
    amp: f32,
    phases: [f32; 2],
    velocity: f32,
}

/// Filtered noise and metal: shaker, tambourine.
#[derive(Clone, Default)]
struct Hiss {
    amp: f32,
    /// Rises from 0 to 1 as the beads or jingles get going.
    swell: f32,
    gain: f32,
    metal: Metal,
    filter: Svf,
}

impl Hiss {
    fn hit(&mut self, gain: f32) {
        (self.amp, self.swell) = (1.0, 0.0);
        self.gain = gain;
    }

    /// `metal` is how much of the sound is jingles rather than beads.
    #[inline]
    fn tick(&mut self, metal: f32, swell: f32, decay: f32, c: &Coefs, rng: &mut Rng) -> f32 {
        let jingles = if metal > 0.0 { self.metal.tick(5.2, c.inv_sr).1 * metal } else { 0.0 };
        let filtered = self.filter.tick(jingles + c.noise(rng) * (1.0 - metal * 0.5));
        let out = (filtered.band + filtered.high * 0.25) * self.amp * self.swell * self.gain;
        self.swell = (self.swell + swell).min(1.0);
        if self.swell >= 1.0 {
            self.amp *= decay;
        }
        out
    }
}

#[derive(Clone, Default)]
struct Coefs {
    inv_sr: f32,
    noise_gain: f32,
    kick_hz: f32,
    kick_rate: f32,
    kick_fast_depth: f32,
    kick_slow_depth: f32,
    kick_fast: f32,
    kick_slow: f32,
    kick_click: f32,
    kick_click_level: f32,
    kick_drive: f32,
    kick_norm: f32,
    snare_hz: f32,
    snare_body: f32,
    snare_crack: f32,
    snare_rattle: f32,
    snare_wires: f32,
    snare_sweep: f32,
    clap_burst: f32,
    clap_tail: f32,
    clap_gap: u32,
    clap_jitter: u32,
    hat_closed: f32,
    hat_pedal: f32,
    hat_open: f32,
    hat_scale: f32,
    tom_rate: f32,
    tom_sweep: f32,
    tom_stick: f32,
    crash: f32,
    ride: f32,
    cymbal_stroke: f32,
    cymbal_dark: f32,
    rim: f32,
    cowbell: f32,
    claves: f32,
    shaker: f32,
    shaker_swell: f32,
    tambourine: f32,
    levels: [f32; 7],
    volume: f32,
}

impl Coefs {
    /// White noise as loud per hertz at every sample rate. Unscaled, a
    /// faster rate spreads the same power over a wider band, and what is
    /// left inside a filter's band (and inside hearing) gets quieter.
    #[inline]
    fn noise(&self, rng: &mut Rng) -> f32 {
        rng.bipolar() * self.noise_gain
    }
}

const KICK: usize = 0;
const SNARE: usize = 1;
const CLAP: usize = 2;
const HAT: usize = 3;
const TOM: usize = 4;
const CYMBAL: usize = 5;
const PERC: usize = 6;

/// Below this a decaying voice is switched off (-80 dB).
const SILENT: f32 = 1e-4;

pub struct DrumKit {
    params: ParamValues,
    c: Coefs,
    sample_rate: f32,
    rng: Rng,
    kick: Kick,
    snare: Snare,
    clap: Clap,
    hat: Hat,
    toms: [Tom; 3],
    crash: Cymbal,
    ride: Cymbal,
    rim: Blip,
    cowbell: Blip,
    claves: Blip,
    shaker: Hiss,
    tambourine: Hiss,
    tom_pans: [(f32, f32); 3],
    hat_pan: (f32, f32),
    crash_pan: (f32, f32),
    ride_pan: (f32, f32),
    perc_pan: (f32, f32),
    shaker_pan: (f32, f32),
}

impl DrumKit {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            c: Coefs::default(),
            sample_rate: 48_000.0,
            rng: Rng::new(0x1234_ABCD),
            kick: Kick::default(),
            snare: Snare::default(),
            clap: Clap::default(),
            hat: Hat::default(),
            toms: Default::default(),
            crash: Cymbal::default(),
            ride: Cymbal::default(),
            rim: Blip::default(),
            cowbell: Blip::default(),
            claves: Blip::default(),
            shaker: Hiss::default(),
            tambourine: Hiss::default(),
            tom_pans: [pan_gains(-0.35), pan_gains(0.0), pan_gains(0.35)],
            hat_pan: pan_gains(0.2),
            crash_pan: pan_gains(-0.3),
            ride_pan: pan_gains(0.35),
            perc_pan: pan_gains(-0.2),
            shaker_pan: pan_gains(0.3),
        }
    }

    /// Works out everything `tick` needs from the parameters. The decay
    /// parameters are scaled so that their defaults give each drum the
    /// length recorded drums have.
    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        let c = &mut self.c;
        c.inv_sr = 1.0 / sr;
        c.noise_gain = (sr / 48_000.0).sqrt();

        let punch = p.get(P::KickPunch as usize);
        let drive = p.get(P::KickDrive as usize);
        c.kick_hz = p.get(P::KickPitch as usize);
        c.kick_rate = 1.0 / (p.get(P::KickDecay as usize) * 0.6 * sr);
        c.kick_fast_depth = punch * 7.0;
        c.kick_slow_depth = 0.4 + punch;
        c.kick_fast = decay_coef(0.035, sr);
        c.kick_slow = decay_coef(0.2, sr);
        c.kick_click = decay_coef(0.02, sr);
        c.kick_click_level = punch * 2.2;
        c.kick_drive = 1.0 + drive * 4.0;
        c.kick_norm = 1.0 / c.kick_drive.tanh();

        c.snare_hz = p.get(P::SnarePitch as usize);
        let snare_decay = p.get(P::SnareDecay as usize);
        c.snare_body = decay_coef(0.15 + snare_decay * 1.2, sr);
        c.snare_crack = decay_coef(0.06, sr);
        c.snare_rattle = 1.0 / (snare_decay * 1.45 * sr);
        c.snare_wires = p.get(P::SnareNoise as usize) * 1.9;
        c.snare_sweep = decay_coef(0.04, sr);

        c.clap_burst = decay_coef(0.03, sr);
        c.clap_tail = 1.0 / (0.2 * sr);
        c.clap_gap = ((0.009 * sr) as u32).max(1);
        c.clap_jitter = ((0.003 * sr) as u32).max(1);

        let hat_decay = p.get(P::HatDecay as usize);
        c.hat_closed = decay_coef(hat_decay * 2.8, sr);
        c.hat_pedal = decay_coef(hat_decay * 1.4, sr);
        c.hat_open = decay_coef(p.get(P::OpenHatDecay as usize) * 0.9, sr);
        let tone = p.get(P::HatTone as usize);
        c.hat_scale = 3.0 + tone * 3.0;

        c.tom_rate = 1.0 / (p.get(P::TomDecay as usize) * 0.9 * sr);
        c.tom_sweep = decay_coef(0.08, sr);
        c.tom_stick = decay_coef(0.03, sr);

        let cymbal = p.get(P::CymbalDecay as usize);
        c.crash = decay_coef(cymbal * 2.2, sr);
        c.ride = decay_coef(cymbal * 1.4, sr);
        c.cymbal_stroke = decay_coef(0.09, sr);
        c.cymbal_dark = 1.0 - (-TAU * 3500.0 / sr).exp();
        c.rim = decay_coef(0.09, sr);
        c.cowbell = decay_coef(0.3, sr);
        c.claves = decay_coef(0.11, sr);
        c.shaker = decay_coef(0.13, sr);
        c.shaker_swell = 1.0 / (0.012 * sr);
        c.tambourine = decay_coef(0.3, sr);

        c.levels = [
            p.get(P::KickLevel as usize),
            p.get(P::SnareLevel as usize),
            p.get(P::ClapLevel as usize),
            p.get(P::HatLevel as usize),
            p.get(P::TomLevel as usize),
            p.get(P::CymbalLevel as usize),
            p.get(P::PercLevel as usize),
        ];
        c.volume = db_to_gain(p.get(P::Volume as usize));

        self.kick.filter.set(2200.0, 1.0, sr);
        self.snare.highpass.set(1600.0, 0.6, sr);
        self.snare.lowpass.set(9000.0, 0.7, sr);
        for filter in &mut self.clap.filters {
            filter.set(1300.0, 1.0, sr);
        }
        self.hat.highpass.set(4000.0 + tone * 5000.0, 0.7, sr);
        self.hat.lowpass.set(8000.0 + tone * 6000.0, 0.7, sr);
        for tom in &mut self.toms {
            tom.filter.set_cutoff(1500.0, sr);
        }
        self.crash.highpass.set(2500.0 + tone * 1500.0, 0.5, sr);
        self.crash.lowpass.set(9000.0 + tone * 4000.0, 0.7, sr);
        self.ride.highpass.set(4500.0 + tone * 1500.0, 0.7, sr);
        self.ride.lowpass.set(12_000.0 + tone * 4000.0, 0.7, sr);
        self.shaker.filter.set(6500.0, 1.0, sr);
        self.tambourine.filter.set(8500.0, 1.4, sr);
        self.params.changed = false;
    }

    fn trigger(&mut self, pitch: u8, velocity: f32) {
        // Perceptual loudness curve.
        let v = velocity * velocity.sqrt();
        match pitch {
            35 | 36 => self.kick.hit(v, velocity),
            37 => self.rim = Blip { amp: 1.0, phases: [0.0; 2], velocity: v },
            38 => self.snare.hit(v, velocity, 1.0),
            40 => self.snare.hit(v, velocity, 1.18),
            39 => self.clap.hit(v, &self.c, &mut self.rng),
            42 => self.hat.hit(v, self.c.hat_closed, &mut self.rng),
            44 => self.hat.hit(v * 0.7, self.c.hat_pedal, &mut self.rng),
            46 => self.hat.hit(v, self.c.hat_open, &mut self.rng),
            41 | 43 | 45 | 47 | 48 | 50 => {
                let (slot, hz) = match pitch {
                    41 => (0, 82.0),
                    43 => (0, 98.0),
                    45 => (1, 123.0),
                    47 => (1, 147.0),
                    48 => (2, 185.0),
                    _ => (2, 220.0),
                };
                self.toms[slot].hit(v, hz);
            }
            49 | 52 | 55 | 57 => self.crash.hit(v),
            51 | 53 | 59 => self.ride.hit(v),
            54 => self.tambourine.hit(v),
            56 => self.cowbell = Blip { amp: 1.0, phases: [0.0; 2], velocity: v },
            69 | 70 | 82 => self.shaker.hit(v),
            75..=77 => self.claves = Blip { amp: 1.0, phases: [0.0; 2], velocity: v },
            _ => {}
        }
    }

    #[inline]
    fn tick(&mut self) -> (f32, f32) {
        let c = &self.c;
        let rng = &mut self.rng;
        let mut l = 0.0;
        let mut r = 0.0;

        if self.kick.life > 0.0 {
            let out = self.kick.tick(c, rng) * c.levels[KICK] * 0.75;
            l += out;
            r += out;
        }

        if self.snare.sounding() {
            let out = self.snare.tick(c, rng) * c.levels[SNARE] * 0.62;
            l += out;
            r += out;
        }

        if self.clap.room > 0.0 {
            let (left, right) = self.clap.tick(c, rng);
            l += left * c.levels[CLAP] * 1.7;
            r += right * c.levels[CLAP] * 1.7;
        }

        if self.hat.amp > SILENT {
            let out = self.hat.tick(c, rng) * c.levels[HAT] * 0.5;
            l += out * self.hat_pan.0;
            r += out * self.hat_pan.1;
        }

        for (tom, pan) in self.toms.iter_mut().zip(self.tom_pans) {
            if tom.life > 0.0 {
                let out = tom.tick(c, rng) * c.levels[TOM] * 0.4;
                l += out * pan.0;
                r += out * pan.1;
            }
        }

        if self.crash.amp > SILENT {
            let out = self.crash.tick(&CRASH, c.crash, c, rng) * c.levels[CYMBAL] * 0.3;
            l += out * self.crash_pan.0;
            r += out * self.crash_pan.1;
        }

        if self.ride.amp > SILENT {
            let out = self.ride.tick(&RIDE, c.ride, c, rng) * c.levels[CYMBAL] * 0.3;
            l += out * self.ride_pan.0;
            r += out * self.ride_pan.1;
        }

        let mut perc = 0.0;
        let rim = &mut self.rim;
        if rim.amp > SILENT {
            // The stick itself: a few milliseconds of click over the ring.
            let click = c.noise(rng) * rim.amp.powi(4) * rim.velocity * 0.15;
            perc += blip(rim, [1720.0, 480.0], false, c.inv_sr) * 0.45 + click;
            rim.amp *= c.rim;
        }
        let cowbell = &mut self.cowbell;
        if cowbell.amp > SILENT {
            perc += blip(cowbell, [540.0, 800.0], true, c.inv_sr) * 0.3;
            cowbell.amp *= c.cowbell;
        }
        let claves = &mut self.claves;
        if claves.amp > SILENT {
            perc += blip(claves, [2500.0, 5020.0], false, c.inv_sr) * 0.4;
            claves.amp *= c.claves;
        }
        perc *= c.levels[PERC];
        l += perc * self.perc_pan.0;
        r += perc * self.perc_pan.1;

        let mut hiss = 0.0;
        if self.shaker.amp > SILENT {
            hiss += self.shaker.tick(0.0, c.shaker_swell, c.shaker, c, rng) * 0.33;
        }
        if self.tambourine.amp > SILENT {
            hiss += self.tambourine.tick(0.7, 1.0, c.tambourine, c, rng) * 0.25;
        }
        hiss *= c.levels[PERC];
        l += hiss * self.shaker_pan.0;
        r += hiss * self.shaker_pan.1;

        (l * c.volume, r * c.volume)
    }

    /// Stops every drum and forgets what the filters were ringing with.
    fn silence(&mut self) {
        self.kick.life = 0.0;
        self.kick.filter.reset();
        (self.snare.body, self.snare.rattle) = (0.0, 0.0);
        self.snare.highpass.reset();
        self.snare.lowpass.reset();
        self.clap.room = 0.0;
        for filter in &mut self.clap.filters {
            filter.reset();
        }
        self.hat.amp = 0.0;
        self.hat.highpass.reset();
        self.hat.lowpass.reset();
        for tom in &mut self.toms {
            tom.life = 0.0;
            tom.filter.reset();
        }
        for cymbal in [&mut self.crash, &mut self.ride] {
            (cymbal.amp, cymbal.dark) = (0.0, 0.0);
            cymbal.highpass.reset();
            cymbal.lowpass.reset();
        }
        for blip in [&mut self.rim, &mut self.cowbell, &mut self.claves] {
            blip.amp = 0.0;
        }
        for hiss in [&mut self.shaker, &mut self.tambourine] {
            hiss.amp = 0.0;
            hiss.filter.reset();
        }
    }
}

#[inline]
fn blip(voice: &mut Blip, hz: [f32; 2], square: bool, inv_sr: f32) -> f32 {
    let mut sum = 0.0;
    for (phase, hz) in voice.phases.iter_mut().zip(hz) {
        *phase += hz * inv_sr;
        *phase -= phase.floor();
        sum += if square { if *phase < 0.5 { 1.0 } else { -1.0 } } else { (*phase * TAU).sin() };
    }
    sum * 0.5 * voice.amp * voice.velocity
}

impl Default for DrumKit {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for DrumKit {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            self.refresh();
        }
        let mut next_event = 0;
        for i in 0..left.len() {
            while next_event < events.len() && events[next_event].offset as usize <= i {
                if let EventKind::NoteOn { pitch, velocity } = events[next_event].kind {
                    self.trigger(pitch, velocity);
                }
                next_event += 1;
            }
            let (l, r) = self.tick();
            left[i] += l;
            right[i] += r;
        }
        for event in &events[next_event..] {
            if let EventKind::NoteOn { pitch, velocity } = event.kind {
                self.trigger(pitch, velocity);
            }
        }
    }

    fn reset(&mut self) {
        self.silence();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::RealFft;
    use crate::testing::{peak, rms, run};
    use std::ops::Range;

    /// Four seconds of the kit after one hit of `pitch`, in mono.
    fn hit(sample_rate: f64, pitch: u8, velocity: f32, params: &[(P, f64)]) -> Vec<f32> {
        let mut kit = DrumKit::new();
        kit.prepare(sample_rate, 256);
        for &(param, value) in params {
            kit.set_param(param as usize, value);
        }
        let frames = (sample_rate * 4.0) as usize;
        let (left, right) = run(&mut kit, sample_rate, 256, frames, &[(0, EventKind::NoteOn { pitch, velocity })]);
        left.iter().zip(&right).map(|(l, r)| (l + r) * 0.5).collect()
    }

    /// The power spectrum of the first second of `samples`, and the width of
    /// one of its bins in Hz.
    fn spectrum(samples: &[f32], sample_rate: f64) -> (Vec<f64>, f64) {
        let frames = sample_rate as usize;
        let mut fft = RealFft::new(frames.next_power_of_two());
        let mut input = samples[..frames].to_vec();
        input.resize(fft.size(), 0.0);
        let (mut re, mut im) = (vec![0.0; fft.bins()], vec![0.0; fft.bins()]);
        fft.forward(&input, &mut re, &mut im);
        let power = re.iter().zip(&im).map(|(re, im)| (*re as f64).powi(2) + (*im as f64).powi(2)).collect();
        (power, sample_rate / fft.size() as f64)
    }

    /// Where the energy of `samples` sits, as far as anyone can hear it: the
    /// centre of gravity of the power spectrum below 20 kHz, in Hz.
    fn centroid(samples: &[f32], sample_rate: f64) -> f64 {
        let (power, bin_hz) = spectrum(samples, sample_rate);
        let audible = &power[..((20_000.0 / bin_hz) as usize).min(power.len())];
        let weighted: f64 = audible.iter().enumerate().map(|(bin, power)| power * bin as f64).sum();
        weighted / audible.iter().sum::<f64>() * bin_hz
    }

    /// Milliseconds from the loudest moment of `samples` until they have
    /// fallen `db` below it for good.
    fn fall_ms(samples: &[f32], sample_rate: f64, db: f32) -> f64 {
        let hop = (sample_rate / 400.0) as usize;
        let levels: Vec<f32> = samples.chunks(hop).map(peak).collect();
        let top = (0..levels.len()).max_by(|&a, &b| levels[a].total_cmp(&levels[b])).unwrap();
        let floor = levels[top] * db_to_gain(-db);
        let last = levels.iter().rposition(|&level| level > floor).unwrap();
        (last + 1 - top) as f64 * hop as f64 / sample_rate * 1000.0
    }

    /// The share of the energy of `samples` above `hz`, in dB.
    fn share_above(samples: &[f32], sample_rate: f64, hz: f64) -> f64 {
        let (power, bin_hz) = spectrum(samples, sample_rate);
        let above: f64 = power[(hz / bin_hz) as usize..].iter().sum();
        10.0 * (above / power.iter().sum::<f64>()).log10()
    }

    /// The ranges are those of recorded one-shots: for each kind of drum,
    /// the middle half of a few dozen samples measured the same way, widened
    /// by a quarter. A drum that leaves its range has stopped sounding like
    /// that drum, whatever else it sounds like.
    #[test]
    fn the_drums_sit_where_recorded_drums_do() {
        // Pitch, name, centre of the spectrum in Hz, time to fall 30 dB in ms.
        let drums: [(u8, &str, Range<f64>, Range<f64>); 8] = [
            (36, "kick", 55.0..160.0, 130.0..320.0),
            (38, "snare", 1700.0..4500.0, 150.0..340.0),
            (39, "clap", 1400.0..3500.0, 90.0..320.0),
            (42, "closed hat", 5800.0..10_500.0, 45.0..125.0),
            (46, "open hat", 5500.0..9500.0, 130.0..300.0),
            (43, "low tom", 90.0..280.0, 150.0..320.0),
            (49, "crash", 5200.0..8500.0, 800.0..3000.0),
            (51, "ride", 5800.0..11_000.0, 400.0..1100.0),
        ];
        for (pitch, name, centre, length) in drums {
            let samples = hit(48_000.0, pitch, 1.0, &[]);
            let (hz, ms) = (centroid(&samples, 48_000.0), fall_ms(&samples, 48_000.0, 30.0));
            println!("{name:11} centre {hz:6.0} Hz, falls 30 dB in {ms:5.0} ms, peak {:.2}", peak(&samples));
            assert!(centre.contains(&hz), "the {name} sits at {hz:.0} Hz, recorded ones at {centre:?}");
            assert!(length.contains(&ms), "the {name} takes {ms:.0} ms to fall 30 dB, recorded ones {length:?}");
        }
        // Metal is bright, not hiss: recorded hats and cymbals keep four
        // fifths of their energy below 10 kHz.
        for (pitch, name) in [(42, "closed hat"), (46, "open hat"), (49, "crash"), (51, "ride"), (70, "shaker")] {
            let air = share_above(&hit(48_000.0, pitch, 1.0, &[]), 48_000.0, 10_000.0);
            assert!(air < -4.0, "the {name} has {air:.1} dB of its energy above 10 kHz");
        }
        // And a kick's click is a knock: next to nothing up there at all.
        let air = share_above(&hit(48_000.0, 36, 1.0, &[]), 48_000.0, 10_000.0);
        assert!(air < -40.0, "the kick has {air:.1} dB of its energy above 10 kHz");
    }

    #[test]
    fn the_kit_sounds_the_same_at_every_sample_rate() {
        for (pitch, name) in
            [(36, "kick"), (38, "snare"), (39, "clap"), (42, "closed hat"), (46, "open hat"), (49, "crash")]
        {
            let reference = hit(48_000.0, pitch, 1.0, &[]);
            let (hz, ms, level) =
                (centroid(&reference, 48_000.0), fall_ms(&reference, 48_000.0, 30.0), rms(&reference[..24_000]));
            for sample_rate in [44_100.0, 96_000.0] {
                let samples = hit(sample_rate, pitch, 1.0, &[]);
                let half = (sample_rate / 2.0) as usize;
                let (other_hz, other_ms, other_level) =
                    (centroid(&samples, sample_rate), fall_ms(&samples, sample_rate, 30.0), rms(&samples[..half]));
                println!(
                    "{name:11} at {sample_rate:6.0}: {other_hz:6.0} Hz ({hz:6.0}), {other_ms:5.0} ms ({ms:5.0}), {:+.1} dB",
                    20.0 * (other_level / level).log10()
                );
                assert!((other_hz / hz - 1.0).abs() < 0.15, "{name} at {sample_rate}: {other_hz:.0} Hz, not {hz:.0}");
                assert!((other_ms / ms - 1.0).abs() < 0.25, "{name} at {sample_rate}: {other_ms:.0} ms, not {ms:.0}");
                assert!((other_level / level).log10().abs() * 20.0 < 2.0, "{name} at {sample_rate}: level");
            }
        }
    }

    #[test]
    fn hard_hits_are_louder_and_crack_more() {
        for pitch in [36, 38, 42, 49] {
            let (soft, hard) = (hit(48_000.0, pitch, 0.4, &[]), hit(48_000.0, pitch, 1.0, &[]));
            assert!(peak(&hard) > peak(&soft) * 2.5, "drum {pitch}: velocity barely changes the level");
        }
        let (soft, hard) = (hit(48_000.0, 38, 0.3, &[]), hit(48_000.0, 38, 1.0, &[]));
        assert!(centroid(&hard, 48_000.0) > centroid(&soft, 48_000.0) * 1.15, "a hard snare hit is mostly wires");
        let (soft, hard) = (hit(48_000.0, 36, 0.3, &[]), hit(48_000.0, 36, 1.0, &[]));
        let knock = |samples: &[f32]| share_above(samples, 48_000.0, 1000.0);
        assert!(knock(&hard) > knock(&soft) + 2.0, "a hard kick hit clicks more");
    }

    #[test]
    fn the_controls_do_what_they_say() {
        let length = |pitch, params: &[(P, f64)]| fall_ms(&hit(48_000.0, pitch, 1.0, params), 48_000.0, 30.0);
        for (pitch, param, name, short, long) in [
            (36, P::KickDecay, "kick decay", 0.2, 1.0),
            (38, P::SnareDecay, "snare decay", 0.1, 0.6),
            (42, P::HatDecay, "closed hat decay", 0.02, 0.2),
            (46, P::OpenHatDecay, "open hat decay", 0.15, 1.0),
            (45, P::TomDecay, "tom decay", 0.15, 1.0),
            (49, P::CymbalDecay, "cymbal decay", 0.4, 3.0),
        ] {
            let (brief, lasting) = (length(pitch, &[(param, short)]), length(pitch, &[(param, long)]));
            assert!(lasting > brief * 2.0, "{name}: {short} gives {brief:.0} ms and {long} gives {lasting:.0} ms");
        }
        let brightness = |tone| centroid(&hit(48_000.0, 42, 1.0, &[(P::HatTone, tone)]), 48_000.0);
        assert!(brightness(1.0) > brightness(0.0) * 1.4, "hat brightness: {} to {}", brightness(0.0), brightness(1.0));
        let pitch = |hz| centroid(&hit(48_000.0, 36, 1.0, &[(P::KickPitch, hz), (P::KickPunch, 0.0)]), 48_000.0);
        assert!(pitch(80.0) > pitch(40.0) * 1.5, "kick pitch: {} to {}", pitch(40.0), pitch(80.0));
        // No wires at all leaves the drum itself, far down the spectrum.
        let snare = |noise| centroid(&hit(48_000.0, 38, 1.0, &[(P::SnareNoise, noise)]), 48_000.0);
        assert!(snare(0.0) < 500.0 && snare(1.0) > 2000.0, "snare noise: {} to {}", snare(0.0), snare(1.0));
    }

    #[test]
    fn hats_vary_from_hit_to_hit_but_only_a_little() {
        let mut kit = DrumKit::new();
        kit.prepare(48_000.0, 256);
        let hits: Vec<(usize, EventKind)> =
            (0..8).map(|i| (i * 12_000, EventKind::NoteOn { pitch: 42, velocity: 0.8 })).collect();
        let (left, _) = run(&mut kit, 48_000.0, 256, 96_000, &hits);
        let levels: Vec<f32> = left.chunks(12_000).map(rms).collect();
        let (quietest, loudest) = levels.iter().fold((f32::MAX, 0.0f32), |(low, high), &l| (low.min(l), high.max(l)));
        assert!(loudest > quietest * 1.02, "every hat hit is the same: {levels:?}");
        assert!(loudest < quietest * 1.5, "hat hits differ by more than 3.5 dB: {levels:?}");
        assert_ne!(left[..12_000], left[12_000..24_000], "two hat hits are sample for sample the same");
    }

    #[test]
    fn a_kick_starts_and_ends_without_a_click() {
        let samples = hit(48_000.0, 36, 1.0, &[(P::KickPunch, 0.0)]);
        // Without its beater a kick is a sine starting from zero: no step
        // anywhere larger than the steepest part of its own first cycle.
        let step = crate::testing::largest_step(&samples);
        assert!(step < 0.05, "largest step {step}");
        assert_eq!(peak(&samples[48_000..]), 0.0, "still sounding after a second");
    }
}
