//! Drum Kit: synthesized drums on the General MIDI drum map.

use crate::define_params;
use crate::params::{ParamValues, PresetDef, float};
use crate::util::{Rng, Svf, db_to_gain, decay_coef, pan_gains};
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
39 clap, 42 closed hat, 46 open hat (choked by 42), 41/43 low toms, 45/47 mid toms, 48/50 high toms, \
49 crash, 51 ride, 54 tambourine, 56 cowbell, 70 shaker, 75 claves. Velocity sets loudness.";

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

/// Square-wave cluster at the classic inharmonic ratios, the raw material of
/// hats and cymbals.
#[derive(Clone, Default)]
struct Metal {
    phases: [f32; 6],
}

const METAL_HZ: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

impl Metal {
    #[inline]
    fn tick(&mut self, scale: f32, inv_sr: f32) -> f32 {
        let mut sum = 0.0;
        for (phase, hz) in self.phases.iter_mut().zip(METAL_HZ) {
            *phase += hz * scale * inv_sr;
            *phase -= phase.floor();
            sum += if *phase < 0.5 { 1.0 } else { -1.0 };
        }
        sum * (1.0 / 6.0)
    }
}

#[derive(Clone, Default)]
struct Kick {
    amp: f32,
    sweep: f32,
    click: f32,
    phase: f32,
    velocity: f32,
}

#[derive(Clone, Default)]
struct Snare {
    body: f32,
    noise: f32,
    sweep: f32,
    phase1: f32,
    phase2: f32,
    velocity: f32,
    filter: Svf,
}

#[derive(Clone, Default)]
struct Clap {
    amp: f32,
    /// Samples since the trigger; drives the three retriggered bursts.
    age: u32,
    velocity: f32,
    filter: Svf,
}

#[derive(Clone, Default)]
struct Hat {
    amp: f32,
    open: bool,
    velocity: f32,
    metal: Metal,
    filter: Svf,
}

#[derive(Clone, Default)]
struct Tom {
    amp: f32,
    sweep: f32,
    phase: f32,
    hz: f32,
    velocity: f32,
}

#[derive(Clone, Default)]
struct Cymbal {
    amp: f32,
    velocity: f32,
    metal: Metal,
    filter: Svf,
}

/// Two decaying oscillators: rim shot, cowbell, claves.
#[derive(Clone, Default)]
struct Blip {
    amp: f32,
    phases: [f32; 2],
    velocity: f32,
}

/// Filtered noise burst: shaker, tambourine.
#[derive(Clone, Default)]
struct Hiss {
    amp: f32,
    velocity: f32,
    filter: Svf,
}

#[derive(Clone, Default)]
struct Coefs {
    inv_sr: f32,
    kick_hz: f32,
    kick_amp: f32,
    kick_sweep_depth: f32,
    kick_sweep: f32,
    kick_click: f32,
    kick_click_level: f32,
    kick_gain: f32,
    kick_norm: f32,
    snare_hz: f32,
    snare_body: f32,
    snare_noise: f32,
    snare_noise_level: f32,
    snare_sweep: f32,
    clap_burst: f32,
    clap_tail: f32,
    clap_gap: u32,
    hat_closed: f32,
    hat_open: f32,
    hat_scale: f32,
    tom_amp: f32,
    tom_sweep: f32,
    crash: f32,
    ride: f32,
    rim: f32,
    cowbell: f32,
    claves: f32,
    shaker: f32,
    tambourine: f32,
    levels: [f32; 7],
    volume: f32,
}

const KICK: usize = 0;
const SNARE: usize = 1;
const CLAP: usize = 2;
const HAT: usize = 3;
const TOM: usize = 4;
const CYMBAL: usize = 5;
const PERC: usize = 6;

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

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        let c = &mut self.c;
        c.inv_sr = 1.0 / sr;

        let punch = p.get(P::KickPunch as usize);
        let drive = p.get(P::KickDrive as usize);
        c.kick_hz = p.get(P::KickPitch as usize);
        c.kick_amp = decay_coef(p.get(P::KickDecay as usize), sr);
        c.kick_sweep_depth = 0.5 + punch * 5.0;
        c.kick_sweep = decay_coef(0.12, sr);
        c.kick_click = decay_coef(0.012, sr);
        c.kick_click_level = punch * 0.35;
        c.kick_gain = 1.0 + drive * 4.0;
        c.kick_norm = 1.0 / c.kick_gain.tanh();

        c.snare_hz = p.get(P::SnarePitch as usize);
        let snare_decay = p.get(P::SnareDecay as usize);
        c.snare_body = decay_coef(0.08 + snare_decay * 0.35, sr);
        c.snare_noise = decay_coef(snare_decay, sr);
        c.snare_noise_level = p.get(P::SnareNoise as usize);
        c.snare_sweep = decay_coef(0.05, sr);

        c.clap_burst = decay_coef(0.02, sr);
        c.clap_tail = decay_coef(0.28, sr);
        c.clap_gap = ((0.011 * sr) as u32).max(1);

        c.hat_closed = decay_coef(p.get(P::HatDecay as usize), sr);
        c.hat_open = decay_coef(p.get(P::OpenHatDecay as usize), sr);
        let tone = p.get(P::HatTone as usize);
        c.hat_scale = 1.0 + tone;
        let hat_cutoff = 4500.0 + tone * 5500.0;

        c.tom_amp = decay_coef(p.get(P::TomDecay as usize), sr);
        c.tom_sweep = decay_coef(0.1, sr);

        let cymbal = p.get(P::CymbalDecay as usize);
        c.crash = decay_coef(cymbal, sr);
        c.ride = decay_coef(cymbal * 0.7, sr);
        c.rim = decay_coef(0.035, sr);
        c.cowbell = decay_coef(0.3, sr);
        c.claves = decay_coef(0.06, sr);
        c.shaker = decay_coef(0.09, sr);
        c.tambourine = decay_coef(0.2, sr);

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

        self.snare.filter.set(1800.0, 0.7, sr);
        self.clap.filter.set(1100.0, 1.6, sr);
        self.hat.filter.set(hat_cutoff, 0.9, sr);
        self.crash.filter.set(3800.0 + tone * 2000.0, 0.7, sr);
        self.ride.filter.set(5200.0 + tone * 2000.0, 1.2, sr);
        self.shaker.filter.set(6000.0, 0.8, sr);
        self.tambourine.filter.set(7500.0, 1.5, sr);
        self.params.changed = false;
    }

    fn trigger(&mut self, pitch: u8, velocity: f32) {
        // Perceptual loudness curve.
        let v = velocity * velocity.sqrt();
        match pitch {
            35 | 36 => self.kick = Kick { amp: 1.0, sweep: 1.0, click: 1.0, phase: 0.0, velocity: v },
            37 => self.rim = Blip { amp: 1.0, phases: [0.0; 2], velocity: v },
            38 | 40 => {
                self.snare.body = 1.0;
                self.snare.noise = 1.0;
                self.snare.sweep = 1.0;
                self.snare.phase1 = 0.0;
                self.snare.phase2 = 0.0;
                self.snare.velocity = v;
            }
            39 => {
                self.clap.amp = 1.0;
                self.clap.age = 0;
                self.clap.velocity = v;
            }
            42 | 44 | 46 => {
                self.hat.amp = 1.0;
                self.hat.open = pitch == 46;
                self.hat.velocity = v;
            }
            41 | 43 | 45 | 47 | 48 | 50 => {
                let (slot, hz) = match pitch {
                    41 => (0, 82.0),
                    43 => (0, 98.0),
                    45 => (1, 123.0),
                    47 => (1, 147.0),
                    48 => (2, 185.0),
                    _ => (2, 220.0),
                };
                self.toms[slot] = Tom { amp: 1.0, sweep: 1.0, phase: 0.0, hz, velocity: v };
            }
            49 | 52 | 55 | 57 => {
                self.crash.amp = 1.0;
                self.crash.velocity = v;
            }
            51 | 53 | 59 => {
                self.ride.amp = 1.0;
                self.ride.velocity = v;
            }
            54 => {
                self.tambourine.amp = 1.0;
                self.tambourine.velocity = v;
            }
            56 => self.cowbell = Blip { amp: 1.0, phases: [0.0; 2], velocity: v },
            69 | 70 | 82 => {
                self.shaker.amp = 1.0;
                self.shaker.velocity = v;
            }
            75..=77 => self.claves = Blip { amp: 1.0, phases: [0.0; 2], velocity: v },
            _ => {}
        }
    }

    #[inline]
    fn tick(&mut self) -> (f32, f32) {
        const SILENT: f32 = 1e-4;
        let c = &self.c;
        let mut l = 0.0;
        let mut r = 0.0;

        let kick = &mut self.kick;
        if kick.amp > SILENT {
            kick.phase += c.kick_hz * (1.0 + c.kick_sweep_depth * kick.sweep) * c.inv_sr;
            kick.phase -= kick.phase.floor();
            let body = (kick.phase * TAU).sin() * kick.amp;
            let click = self.rng.bipolar() * kick.click * c.kick_click_level;
            let out = ((body + click) * c.kick_gain).tanh() * c.kick_norm * kick.velocity * c.levels[KICK] * 0.75;
            kick.amp *= c.kick_amp;
            kick.sweep *= c.kick_sweep;
            kick.click *= c.kick_click;
            l += out;
            r += out;
        }

        let snare = &mut self.snare;
        if snare.body > SILENT || snare.noise > SILENT {
            let bend = 1.0 + 0.4 * snare.sweep;
            snare.phase1 += c.snare_hz * bend * c.inv_sr;
            snare.phase1 -= snare.phase1.floor();
            snare.phase2 += c.snare_hz * 1.78 * bend * c.inv_sr;
            snare.phase2 -= snare.phase2.floor();
            let body = ((snare.phase1 * TAU).sin() * 0.6 + (snare.phase2 * TAU).sin() * 0.3) * snare.body;
            let noise = snare.filter.tick(self.rng.bipolar()).high * snare.noise * c.snare_noise_level;
            let out = (body + noise * 0.8) * snare.velocity * c.levels[SNARE] * 0.52;
            snare.body *= c.snare_body;
            snare.noise *= c.snare_noise;
            snare.sweep *= c.snare_sweep;
            l += out;
            r += out;
        }

        let clap = &mut self.clap;
        if clap.amp > SILENT {
            let bursting = clap.age < c.clap_gap * 3;
            if bursting && clap.age > 0 && clap.age.is_multiple_of(c.clap_gap) {
                clap.amp = 1.0;
            }
            let out = clap.filter.tick(self.rng.bipolar()).band * clap.amp * clap.velocity * c.levels[CLAP] * 1.2;
            clap.amp *= if bursting { c.clap_burst } else { c.clap_tail };
            clap.age += 1;
            l += out;
            r += out;
        }

        let hat = &mut self.hat;
        if hat.amp > SILENT {
            let source = hat.metal.tick(c.hat_scale, c.inv_sr) * 0.75 + self.rng.bipolar() * 0.35;
            let out = hat.filter.tick(source).high * hat.amp * hat.velocity * c.levels[HAT] * 0.55;
            hat.amp *= if hat.open { c.hat_open } else { c.hat_closed };
            l += out * self.hat_pan.0;
            r += out * self.hat_pan.1;
        }

        for (tom, pan) in self.toms.iter_mut().zip(self.tom_pans) {
            if tom.amp > SILENT {
                tom.phase += tom.hz * (1.0 + 0.8 * tom.sweep) * c.inv_sr;
                tom.phase -= tom.phase.floor();
                let out = (tom.phase * TAU).sin() * tom.amp * tom.velocity * c.levels[TOM] * 0.55;
                tom.amp *= c.tom_amp;
                tom.sweep *= c.tom_sweep;
                l += out * pan.0;
                r += out * pan.1;
            }
        }

        let crash = &mut self.crash;
        if crash.amp > SILENT {
            let source = crash.metal.tick(1.31, c.inv_sr) * 0.6 + self.rng.bipolar() * 0.5;
            let out = crash.filter.tick(source).high * crash.amp * crash.velocity * c.levels[CYMBAL] * 0.45;
            crash.amp *= c.crash;
            l += out * self.crash_pan.0;
            r += out * self.crash_pan.1;
        }

        let ride = &mut self.ride;
        if ride.amp > SILENT {
            let source = ride.metal.tick(1.73, c.inv_sr) * 0.8 + self.rng.bipolar() * 0.15;
            let filtered = ride.filter.tick(source);
            let out = (filtered.band + filtered.high * 0.5) * ride.amp * ride.velocity * c.levels[CYMBAL] * 0.4;
            ride.amp *= c.ride;
            l += out * self.ride_pan.0;
            r += out * self.ride_pan.1;
        }

        let mut perc = 0.0;
        let rim = &mut self.rim;
        if rim.amp > SILENT {
            perc += blip(rim, [1720.0, 480.0], false, c.inv_sr) * 0.55;
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
        let shaker = &mut self.shaker;
        if shaker.amp > SILENT {
            hiss += shaker.filter.tick(self.rng.bipolar()).high * shaker.amp * shaker.velocity * 0.2;
            shaker.amp *= c.shaker;
        }
        let tambourine = &mut self.tambourine;
        if tambourine.amp > SILENT {
            hiss += tambourine.filter.tick(self.rng.bipolar()).band * tambourine.amp * tambourine.velocity * 0.25;
            tambourine.amp *= c.tambourine;
        }
        hiss *= c.levels[PERC];
        l += hiss * self.shaker_pan.0;
        r += hiss * self.shaker_pan.1;

        (l * c.volume, r * c.volume)
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
        self.kick.amp = 0.0;
        self.snare.body = 0.0;
        self.snare.noise = 0.0;
        self.clap.amp = 0.0;
        self.hat.amp = 0.0;
        for tom in &mut self.toms {
            tom.amp = 0.0;
        }
        self.crash.amp = 0.0;
        self.ride.amp = 0.0;
        self.rim.amp = 0.0;
        self.cowbell.amp = 0.0;
        self.claves.amp = 0.0;
        self.shaker.amp = 0.0;
        self.tambourine.amp = 0.0;
    }
}
