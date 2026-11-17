//! Poly Synth: a 16-voice subtractive synthesizer with two oscillators,
//! unison, phase-modulation FM, a multimode filter and two envelopes.

use crate::define_params;
use crate::level::ChannelLevel;
use crate::params::{ParamValues, PresetDef, boolean, choice, float, int};
use crate::util::{Rng, db_to_gain, midi_to_hz, pan_gains};
use crate::voice::{EnvShape, Envelope, FILTER_MODES, FilterMode, Stage, VoiceFilter, resonance_to_q};
use std::f32::consts::TAU;
use trill_core::device::{
    CONTROLLER_MOD_WHEEL, DeviceProcessor, Event, EventKind, PITCH_BEND_SEMITONES, Pedal, ProcessInfo,
};

const MAX_VOICES: usize = 16;
const MAX_UNISON: usize = 7;
const MAX_HELD: usize = 16;
/// Samples between control-rate updates (pitch, filter cutoff, LFO).
const CONTROL_BLOCK: usize = 16;
/// Headroom so that chords do not clip at the default volume.
const VOICE_GAIN: f32 = 0.22;
/// The vibrato the mod wheel adds at the top of its travel, in semitones,
/// at the LFO's rate.
const MOD_WHEEL_VIBRATO: f32 = 0.5;

const WAVES: &[&str] = &["sine", "triangle", "saw", "square"];

define_params!(P, PARAMS, [
    Osc1Wave => choice("osc1_wave", "Osc 1 Wave", WAVES, 2),
    Osc2Wave => choice("osc2_wave", "Osc 2 Wave", WAVES, 2),
    Osc2Semitones => int("osc2_semitones", "Osc 2 Pitch", -24, 24, 0, "st"),
    Osc2Detune => float("osc2_detune", "Osc 2 Detune", -100.0, 100.0, 0.0, "cents"),
    OscMix => float("osc_mix", "Osc Mix (0 = osc 1, 1 = osc 2)", 0.0, 1.0, 0.0, ""),
    FmAmount => float("fm_amount", "FM Amount (osc 2 modulates osc 1)", 0.0, 1.0, 0.0, ""),
    FmEnv => float("fm_env", "FM Follows Filter Envelope", 0.0, 1.0, 0.0, ""),
    SubLevel => float("sub_level", "Sub Oscillator Level", 0.0, 1.0, 0.0, ""),
    NoiseLevel => float("noise_level", "Noise Level", 0.0, 1.0, 0.0, ""),
    Unison => int("unison", "Unison Voices", 1, MAX_UNISON as i32, 1, ""),
    UnisonDetune => float("unison_detune", "Unison Detune", 0.0, 100.0, 20.0, "cents"),
    UnisonSpread => float("unison_spread", "Unison Stereo Spread", 0.0, 1.0, 0.7, ""),
    FilterType => choice("filter_type", "Filter Type", FILTER_MODES, 0),
    Cutoff => float("cutoff", "Filter Cutoff", 20.0, 20000.0, 20000.0, "Hz"),
    Resonance => float("resonance", "Filter Resonance", 0.0, 1.0, 0.1, ""),
    FilterEnv => float("filter_env", "Filter Envelope Amount", -8.0, 8.0, 0.0, "oct"),
    KeyTrack => float("key_track", "Filter Key Tracking", 0.0, 1.0, 0.0, ""),
    Attack => float("attack", "Amp Attack", 0.001, 10.0, 0.005, "s"),
    Decay => float("decay", "Amp Decay", 0.005, 20.0, 0.5, "s"),
    Sustain => float("sustain", "Amp Sustain", 0.0, 1.0, 0.8, ""),
    Release => float("release", "Amp Release", 0.005, 20.0, 0.3, "s"),
    FilterAttack => float("filter_attack", "Filter Attack", 0.001, 10.0, 0.005, "s"),
    FilterDecay => float("filter_decay", "Filter Decay", 0.005, 20.0, 0.5, "s"),
    FilterSustain => float("filter_sustain", "Filter Sustain", 0.0, 1.0, 0.5, ""),
    FilterRelease => float("filter_release", "Filter Release", 0.005, 20.0, 0.3, "s"),
    LfoRate => float("lfo_rate", "LFO Rate", 0.01, 30.0, 5.0, "Hz"),
    LfoPitch => float("lfo_pitch", "LFO to Pitch (vibrato)", 0.0, 12.0, 0.0, "st"),
    LfoCutoff => float("lfo_cutoff", "LFO to Cutoff", -4.0, 4.0, 0.0, "oct"),
    Glide => float("glide", "Glide Time (mono mode)", 0.0, 2.0, 0.0, "s"),
    Mono => boolean("mono", "Mono (legato)", false),
    VelocitySens => float("velocity_sens", "Velocity Sensitivity", 0.0, 1.0, 0.6, ""),
    Volume => float("volume", "Volume", -48.0, 12.0, 0.0, "dB"),
]);

pub const DESCRIPTION: &str = "16-voice subtractive synth: two oscillators (sine/triangle/saw/square) with \
unison and FM, sub oscillator, noise, multimode resonant filter with its own envelope, LFO, mono/glide. \
Covers pads, basses, leads, plucks, keys and bells. Start from a preset.";

pub const PRESETS: &[PresetDef] = &[
    PresetDef { name: "init", description: "Plain sawtooth, filter open", params: &[] },
    PresetDef {
        name: "warm_pad",
        description: "Slow, wide analog pad for chords",
        params: &[
            ("osc2_detune", 8.0),
            ("osc_mix", 0.5),
            ("unison", 5.0),
            ("unison_detune", 28.0),
            ("unison_spread", 0.9),
            ("filter_type", 1.0),
            ("cutoff", 1400.0),
            ("resonance", 0.15),
            ("filter_env", 1.5),
            ("attack", 0.6),
            ("decay", 3.0),
            ("sustain", 0.8),
            ("release", 2.5),
            ("filter_attack", 1.2),
            ("filter_decay", 4.0),
            ("filter_sustain", 0.6),
            ("filter_release", 2.5),
            ("lfo_rate", 0.3),
            ("lfo_cutoff", 0.15),
            ("velocity_sens", 0.3),
        ],
    },
    PresetDef {
        name: "strings",
        description: "Lush ensemble strings",
        params: &[
            ("unison", 7.0),
            ("unison_detune", 35.0),
            ("unison_spread", 1.0),
            ("cutoff", 3200.0),
            ("attack", 0.25),
            ("decay", 2.0),
            ("sustain", 0.9),
            ("release", 1.2),
            ("lfo_rate", 5.2),
            ("lfo_pitch", 0.08),
            ("velocity_sens", 0.4),
            ("key_track", 0.3),
            ("volume", -3.0),
        ],
    },
    PresetDef {
        name: "supersaw",
        description: "Bright stacked saws for big chords and leads",
        params: &[
            ("unison", 7.0),
            ("unison_detune", 45.0),
            ("unison_spread", 1.0),
            ("osc2_semitones", 12.0),
            ("osc_mix", 0.3),
            ("cutoff", 9000.0),
            ("attack", 0.005),
            ("decay", 1.5),
            ("sustain", 0.85),
            ("release", 0.5),
        ],
    },
    PresetDef {
        name: "bass",
        description: "Round analog bass with a filter pluck (write it around E1-G2)",
        params: &[
            ("osc2_wave", 3.0),
            ("osc2_semitones", 0.0),
            ("osc_mix", 0.4),
            ("filter_type", 1.0),
            ("cutoff", 520.0),
            ("resonance", 0.25),
            ("filter_env", 2.5),
            ("attack", 0.002),
            ("decay", 0.8),
            ("sustain", 0.7),
            ("release", 0.15),
            ("filter_attack", 0.001),
            ("filter_decay", 0.5),
            ("filter_sustain", 0.2),
            ("filter_release", 0.15),
            ("mono", 1.0),
            ("glide", 0.03),
            ("velocity_sens", 0.4),
            ("osc2_detune", 6.0),
            ("volume", 8.0),
        ],
    },
    PresetDef {
        name: "sub_bass",
        description: "Deep sine sub (write it around E1-G2)",
        params: &[
            ("osc1_wave", 0.0),
            ("cutoff", 900.0),
            ("attack", 0.005),
            ("decay", 1.0),
            ("sustain", 0.9),
            ("release", 0.18),
            ("mono", 1.0),
            ("glide", 0.04),
            ("velocity_sens", 0.2),
            ("volume", 5.0),
        ],
    },
    PresetDef {
        name: "acid_bass",
        description: "Squelchy resonant mono bass with glide",
        params: &[
            ("filter_type", 1.0),
            ("cutoff", 280.0),
            ("resonance", 0.78),
            ("filter_env", 4.0),
            ("attack", 0.002),
            ("decay", 0.6),
            ("sustain", 0.6),
            ("release", 0.1),
            ("filter_attack", 0.001),
            ("filter_decay", 0.4),
            ("filter_sustain", 0.0),
            ("filter_release", 0.1),
            ("mono", 1.0),
            ("glide", 0.06),
            ("volume", 2.0),
        ],
    },
    PresetDef {
        name: "pluck",
        description: "Short bright pluck for arpeggios and melodies",
        params: &[
            ("osc2_wave", 3.0),
            ("osc_mix", 0.4),
            ("unison", 3.0),
            ("unison_detune", 12.0),
            ("filter_type", 1.0),
            ("cutoff", 500.0),
            ("resonance", 0.2),
            ("filter_env", 4.5),
            ("attack", 0.001),
            ("decay", 0.7),
            ("sustain", 0.0),
            ("release", 0.5),
            ("filter_attack", 0.001),
            ("filter_decay", 0.35),
            ("filter_sustain", 0.0),
            ("filter_release", 0.35),
            ("key_track", 0.5),
            ("volume", 6.0),
        ],
    },
    PresetDef {
        name: "lead",
        description: "Singing mono lead with vibrato and glide",
        params: &[
            ("osc2_wave", 3.0),
            ("osc2_detune", 7.0),
            ("osc_mix", 0.45),
            ("unison", 3.0),
            ("unison_detune", 15.0),
            ("cutoff", 3500.0),
            ("resonance", 0.3),
            ("filter_env", 1.0),
            ("attack", 0.005),
            ("decay", 1.0),
            ("sustain", 0.8),
            ("release", 0.25),
            ("mono", 1.0),
            ("glide", 0.05),
            ("lfo_rate", 5.5),
            ("lfo_pitch", 0.12),
            ("volume", 4.0),
        ],
    },
    PresetDef {
        name: "keys",
        description: "FM electric piano, a little wide (builtin:tremolo with spread 180 makes it a suitcase piano)",
        params: &[
            ("osc1_wave", 0.0),
            ("osc2_wave", 0.0),
            ("fm_amount", 0.4),
            ("fm_env", 0.9),
            // Three voices a few cents apart, the outer two a little to
            // either side: the sides sit some 16 dB under the middle, where
            // one voice alone is the same on both sides to the last bit.
            ("unison", 3.0),
            ("unison_detune", 4.0),
            ("unison_spread", 0.2),
            ("volume", 1.0),
            ("attack", 0.002),
            ("decay", 3.5),
            ("sustain", 0.2),
            ("release", 0.5),
            ("filter_attack", 0.001),
            ("filter_decay", 1.2),
            ("filter_sustain", 0.12),
            ("filter_release", 0.5),
            ("velocity_sens", 0.85),
        ],
    },
    PresetDef {
        name: "bell",
        description: "Glassy FM bell",
        params: &[
            ("osc1_wave", 0.0),
            ("osc2_wave", 0.0),
            ("osc2_semitones", 21.0),
            ("osc2_detune", 69.0),
            ("fm_amount", 0.4),
            ("fm_env", 0.8),
            ("attack", 0.001),
            ("decay", 5.0),
            ("sustain", 0.0),
            ("release", 4.0),
            ("filter_attack", 0.001),
            ("filter_decay", 3.0),
            ("filter_sustain", 0.0),
            ("filter_release", 3.0),
            ("velocity_sens", 0.7),
        ],
    },
    PresetDef {
        name: "mallet",
        description: "Marimba-like mallet",
        params: &[
            ("osc1_wave", 0.0),
            ("osc2_wave", 0.0),
            ("osc2_semitones", 24.0),
            ("fm_amount", 0.22),
            ("fm_env", 1.0),
            ("attack", 0.001),
            ("decay", 1.2),
            ("sustain", 0.0),
            ("release", 0.8),
            ("filter_attack", 0.001),
            ("filter_decay", 0.15),
            ("filter_sustain", 0.0),
            ("filter_release", 0.15),
            ("velocity_sens", 0.8),
        ],
    },
    PresetDef {
        name: "organ",
        description: "Drawbar-style organ",
        params: &[
            ("osc1_wave", 0.0),
            ("osc2_wave", 0.0),
            ("osc2_semitones", 12.0),
            ("osc_mix", 0.4),
            ("sub_level", 0.5),
            ("attack", 0.005),
            ("decay", 0.5),
            ("sustain", 1.0),
            ("release", 0.08),
            ("velocity_sens", 0.0),
            ("lfo_rate", 6.5),
            ("lfo_pitch", 0.05),
            ("volume", -4.0),
        ],
    },
    PresetDef {
        name: "brass",
        description: "Synth brass with a swelling filter",
        params: &[
            ("unison", 2.0),
            ("unison_detune", 10.0),
            ("filter_type", 1.0),
            ("cutoff", 700.0),
            ("resonance", 0.15),
            ("filter_env", 2.8),
            ("attack", 0.04),
            ("decay", 1.0),
            ("sustain", 0.85),
            ("release", 0.25),
            ("filter_attack", 0.08),
            ("filter_decay", 0.8),
            ("filter_sustain", 0.55),
            ("filter_release", 0.25),
        ],
    },
    PresetDef {
        name: "soft_keys",
        description: "Mellow felt-piano-like keys",
        params: &[
            ("osc1_wave", 1.0),
            ("osc2_wave", 0.0),
            ("osc2_semitones", 12.0),
            ("osc_mix", 0.25),
            ("filter_type", 0.0),
            ("cutoff", 900.0),
            ("filter_env", 2.5),
            ("attack", 0.003),
            ("decay", 4.0),
            ("sustain", 0.1),
            ("release", 0.6),
            ("filter_attack", 0.001),
            ("filter_decay", 1.0),
            ("filter_sustain", 0.1),
            ("filter_release", 0.6),
            ("key_track", 0.6),
            ("velocity_sens", 0.8),
            ("volume", 3.0),
        ],
    },
];

/// Everything derived from the parameters, refreshed when one changes.
#[derive(Clone, Default)]
struct Derived {
    wave1: usize,
    wave2: usize,
    osc2_ratio: f32,
    mix1: f32,
    mix2: f32,
    fm: f32,
    fm_env: f32,
    sub: f32,
    noise: f32,
    unison: usize,
    detune: [f32; MAX_UNISON],
    pan_l: [f32; MAX_UNISON],
    pan_r: [f32; MAX_UNISON],
    filter_type: FilterMode,
    cutoff: f32,
    q: f32,
    filter_env: f32,
    key_track: f32,
    amp: EnvShape,
    filt: EnvShape,
    lfo_rate: f32,
    lfo_pitch: f32,
    lfo_cutoff: f32,
    /// Per-sample fraction of the remaining glide distance that is kept.
    glide_keep: f32,
    mono: bool,
    velocity_sens: f32,
    gain: f32,
}

#[derive(Clone)]
struct Voice {
    active: bool,
    held: bool,
    pitch: u8,
    /// Current pitch in semitones; glides toward `target`.
    note: f32,
    target: f32,
    velocity_gain: f32,
    age: u64,
    amp: Envelope,
    filt: Envelope,
    phase1: [f32; MAX_UNISON],
    phase2: [f32; MAX_UNISON],
    sub_phase: f32,
    filter: VoiceFilter,
}

impl Voice {
    fn new() -> Self {
        Self {
            active: false,
            held: false,
            pitch: 0,
            note: 60.0,
            target: 60.0,
            velocity_gain: 0.0,
            age: 0,
            amp: Envelope::idle(),
            filt: Envelope::idle(),
            phase1: [0.0; MAX_UNISON],
            phase2: [0.0; MAX_UNISON],
            sub_phase: 0.0,
            filter: VoiceFilter::default(),
        }
    }

    fn kill(&mut self) {
        self.active = false;
        self.held = false;
        self.amp = Envelope::idle();
        self.filt = Envelope::idle();
    }

    /// `lfo` is the LFO's value for the stretch, `bend` what the wheels add
    /// to the pitch, in semitones, and `levels` the channel's level at each
    /// frame.
    fn render(
        &mut self,
        d: &Derived,
        (lfo, bend, levels): (f32, f32, &[f32]),
        sample_rate: f32,
        rng: &mut Rng,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let len = out_l.len();
        self.note = self.target + (self.note - self.target) * d.glide_keep.powi(len as i32);

        let inc1 = midi_to_hz(self.note + lfo * d.lfo_pitch + bend) / sample_rate;
        let inc2 = inc1 * d.osc2_ratio;
        let sub_inc = inc1 * 0.5;

        let env = self.filt.level;
        let octaves = d.filter_env * env + d.lfo_cutoff * lfo + d.key_track * (self.note - 60.0) / 12.0;
        let cutoff = d.cutoff * 2f32.powf(octaves);
        self.filter.set(cutoff, d.q, sample_rate);
        let fm = d.fm * (1.0 - d.fm_env + d.fm_env * env);
        let gain = self.velocity_gain * d.gain;
        let use_osc1 = d.mix1 > 0.0;
        let use_osc2 = d.mix2 > 0.0 || fm > 0.0;

        for i in 0..len {
            let amp = self.amp.tick(&d.amp);
            self.filt.tick(&d.filt);

            let mut l = 0.0;
            let mut r = 0.0;
            for u in 0..d.unison {
                let step1 = inc1 * d.detune[u];
                let step2 = inc2 * d.detune[u];
                let o2 = if use_osc2 { oscillator(d.wave2, self.phase2[u], step2) } else { 0.0 };
                let o1 = if use_osc1 {
                    let phase = self.phase1[u] + fm * o2;
                    oscillator(d.wave1, phase - phase.floor(), step1)
                } else {
                    0.0
                };
                let sample = o1 * d.mix1 + o2 * d.mix2;
                l += sample * d.pan_l[u];
                r += sample * d.pan_r[u];
                self.phase1[u] += step1;
                self.phase1[u] -= self.phase1[u].floor();
                self.phase2[u] += step2;
                self.phase2[u] -= self.phase2[u].floor();
            }
            if d.sub > 0.0 {
                let sub = (self.sub_phase * TAU).sin() * d.sub;
                l += sub;
                r += sub;
                self.sub_phase += sub_inc;
                self.sub_phase -= self.sub_phase.floor();
            }
            if d.noise > 0.0 {
                let noise = rng.bipolar() * d.noise;
                l += noise;
                r += noise;
            }

            let (l, r) = self.filter.tick(d.filter_type, l, r);

            let level = amp * gain * levels[i];
            out_l[i] += l * level;
            out_r[i] += r * level;
        }

        if self.amp.stage == Stage::Idle {
            self.active = false;
        }
    }
}

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// One band-limited oscillator sample for phase `t` in 0..1 advancing by `dt`.
#[inline]
fn oscillator(wave: usize, t: f32, dt: f32) -> f32 {
    match wave {
        0 => (t * TAU).sin(),
        1 => 1.0 - 4.0 * (t - 0.5).abs(),
        2 => 2.0 * t - 1.0 - poly_blep(t, dt),
        _ => {
            let naive = if t < 0.5 { 1.0 } else { -1.0 };
            let half = if t < 0.5 { t + 0.5 } else { t - 0.5 };
            naive + poly_blep(t, dt) - poly_blep(half, dt)
        }
    }
}

pub struct PolySynth {
    params: ParamValues,
    derived: Derived,
    sample_rate: f32,
    voices: Vec<Voice>,
    rng: Rng,
    clock: u64,
    lfo_phase: f32,
    /// Held keys in press order, for mono note priority.
    held: [u8; MAX_HELD],
    held_len: usize,
    pedal: Pedal,
    /// The pitch wheel, in semitones.
    bend: f32,
    /// The mod wheel, 0..=1: vibrato on top of what the LFO is set to.
    mod_wheel: f32,
    /// Channel volume and expression.
    level: ChannelLevel,
    /// The LFO's value, worked out once every [`CONTROL_BLOCK`] frames of
    /// the synth's own clock, and how many frames it still stands for:
    /// where a buffer or an event divides the frames makes no difference
    /// to it.
    lfo: f32,
    lfo_frames: usize,
}

impl PolySynth {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            derived: Derived::default(),
            sample_rate: 48_000.0,
            voices: vec![Voice::new(); MAX_VOICES],
            rng: Rng::new(0x9E37_79B9),
            clock: 0,
            lfo_phase: 0.0,
            held: [0; MAX_HELD],
            held_len: 0,
            pedal: Pedal::default(),
            bend: 0.0,
            mod_wheel: 0.0,
            level: ChannelLevel::new(),
            lfo: 0.0,
            lfo_frames: 0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        let d = &mut self.derived;
        d.wave1 = p.index(P::Osc1Wave as usize);
        d.wave2 = p.index(P::Osc2Wave as usize);
        d.osc2_ratio = 2f32.powf((p.get(P::Osc2Semitones as usize) + p.get(P::Osc2Detune as usize) / 100.0) / 12.0);
        let mix = p.get(P::OscMix as usize);
        d.mix1 = 1.0 - mix;
        d.mix2 = mix;
        // Up to ~9.4 radians of phase modulation.
        d.fm = p.get(P::FmAmount as usize) * 1.5;
        d.fm_env = p.get(P::FmEnv as usize);
        d.sub = p.get(P::SubLevel as usize);
        d.noise = p.get(P::NoiseLevel as usize);

        let unison = p.index(P::Unison as usize).clamp(1, MAX_UNISON);
        d.unison = unison;
        let detune = p.get(P::UnisonDetune as usize);
        let spread = p.get(P::UnisonSpread as usize);
        let norm = 1.0 / (unison as f32).sqrt();
        for u in 0..unison {
            let position = if unison == 1 { 0.0 } else { u as f32 / (unison - 1) as f32 * 2.0 - 1.0 };
            d.detune[u] = 2f32.powf(position * detune / 1200.0);
            let (l, r) = pan_gains(position * spread);
            d.pan_l[u] = l * norm;
            d.pan_r[u] = r * norm;
        }

        d.filter_type = FilterMode::from_index(p.index(P::FilterType as usize));
        d.cutoff = p.get(P::Cutoff as usize);
        d.q = resonance_to_q(p.get(P::Resonance as usize));
        d.filter_env = p.get(P::FilterEnv as usize);
        d.key_track = p.get(P::KeyTrack as usize);
        d.amp = EnvShape::new(
            p.get(P::Attack as usize),
            p.get(P::Decay as usize),
            p.get(P::Sustain as usize),
            p.get(P::Release as usize),
            sr,
        );
        d.filt = EnvShape::new(
            p.get(P::FilterAttack as usize),
            p.get(P::FilterDecay as usize),
            p.get(P::FilterSustain as usize),
            p.get(P::FilterRelease as usize),
            sr,
        );
        d.lfo_rate = p.get(P::LfoRate as usize);
        d.lfo_pitch = p.get(P::LfoPitch as usize);
        d.lfo_cutoff = p.get(P::LfoCutoff as usize);
        let glide = p.get(P::Glide as usize);
        d.glide_keep = if glide < 0.001 { 0.0 } else { (-3.0 / (glide * sr)).exp() };
        d.mono = p.on(P::Mono as usize);
        d.velocity_sens = p.get(P::VelocitySens as usize);
        d.gain = db_to_gain(p.get(P::Volume as usize)) * VOICE_GAIN;
        self.params.changed = false;
    }

    fn velocity_gain(&self, velocity: f32) -> f32 {
        let sens = self.derived.velocity_sens;
        1.0 - sens + sens * velocity * velocity
    }

    fn note_on(&mut self, pitch: u8, velocity: f32) {
        self.clock += 1;
        self.pedal.key_down(pitch);
        let gain = self.velocity_gain(velocity);
        let unison = self.derived.unison;

        let index = if self.derived.mono {
            self.hold(pitch);
            0
        } else {
            self.allocate(pitch)
        };

        let voice = &mut self.voices[index];
        let legato = self.derived.mono && voice.active && voice.held;
        if !voice.active {
            voice.note = pitch as f32;
            voice.sub_phase = 0.0;
            for u in 0..MAX_UNISON {
                // A single oscillator starts at phase zero for a consistent
                // attack; unison voices start scattered so they do not flange.
                let random = if unison > 1 { self.rng.unipolar() } else { 0.0 };
                voice.phase1[u] = random;
                voice.phase2[u] = random;
            }
            voice.filter.reset();
        } else if !self.derived.mono {
            voice.note = pitch as f32;
        }
        voice.active = true;
        voice.held = true;
        voice.pitch = pitch;
        voice.target = pitch as f32;
        voice.velocity_gain = gain;
        voice.age = self.clock;
        if !legato {
            voice.amp.stage = Stage::Attack;
            voice.filt.stage = Stage::Attack;
        }
    }

    /// Picks a voice: one already playing this pitch, else a free one, else
    /// the quietest released voice, else the oldest.
    fn allocate(&self, pitch: u8) -> usize {
        if let Some(index) = self.voices.iter().position(|v| v.active && v.pitch == pitch) {
            return index;
        }
        if let Some(index) = self.voices.iter().position(|v| !v.active) {
            return index;
        }
        let released = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.held)
            .min_by(|a, b| a.1.amp.level.total_cmp(&b.1.amp.level))
            .map(|(index, _)| index);
        released.unwrap_or_else(|| {
            self.voices.iter().enumerate().min_by_key(|(_, v)| v.age).map(|(index, _)| index).unwrap_or(0)
        })
    }

    fn hold(&mut self, pitch: u8) {
        self.unhold(pitch);
        if self.held_len == MAX_HELD {
            self.held.copy_within(1.., 0);
            self.held_len -= 1;
        }
        self.held[self.held_len] = pitch;
        self.held_len += 1;
    }

    fn unhold(&mut self, pitch: u8) {
        if let Some(index) = self.held[..self.held_len].iter().position(|&p| p == pitch) {
            self.held.copy_within(index + 1..self.held_len, index);
            self.held_len -= 1;
        }
    }

    /// A key came up: its note ends, unless the pedal holds it.
    fn note_off(&mut self, pitch: u8) {
        if self.pedal.key_up(pitch) {
            self.release(pitch);
        }
    }

    fn release(&mut self, pitch: u8) {
        if self.derived.mono {
            self.unhold(pitch);
            let voice = &mut self.voices[0];
            if voice.pitch != pitch || !voice.held {
                return;
            }
            if self.held_len > 0 {
                // Fall back to the most recent key that is still down.
                let previous = self.held[self.held_len - 1];
                voice.pitch = previous;
                voice.target = previous as f32;
            } else {
                voice.held = false;
                voice.amp.release();
                voice.filt.release();
            }
            return;
        }
        for voice in &mut self.voices {
            if voice.active && voice.held && voice.pitch == pitch {
                voice.held = false;
                voice.amp.release();
                voice.filt.release();
            }
        }
    }

    fn all_notes_off(&mut self) {
        self.held_len = 0;
        self.pedal.clear();
        for voice in &mut self.voices {
            voice.held = false;
            voice.amp.release();
            voice.filt.release();
        }
    }

    fn handle(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => self.note_on(pitch, velocity),
            EventKind::NoteOff { pitch } => self.note_off(pitch),
            EventKind::AllNotesOff => self.all_notes_off(),
            EventKind::PitchBend { value } => {
                self.bend = if value.is_finite() { value.clamp(-1.0, 1.0) * PITCH_BEND_SEMITONES } else { 0.0 };
            }
            EventKind::Control { controller: CONTROLLER_MOD_WHEEL, value } => {
                self.mod_wheel = if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 };
            }
            EventKind::Control { controller, value } => {
                if let Some(down) = kind.sustain() {
                    for pitch in self.pedal.set(down) {
                        self.release(pitch);
                    }
                } else {
                    let sounding = self.voices.iter().any(|voice| voice.active);
                    self.level.control(controller, value, sounding);
                }
            }
            // Pressure has nothing to press on here.
            EventKind::ChannelPressure { .. } => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sample_rate = self.sample_rate;
        let mut pos = 0;
        while pos < left.len() {
            if self.lfo_frames == 0 {
                self.lfo = (self.lfo_phase * TAU).sin();
                self.lfo_phase = (self.lfo_phase + self.derived.lfo_rate * CONTROL_BLOCK as f32 / sample_rate).fract();
                self.lfo_frames = CONTROL_BLOCK;
            }
            let len = (left.len() - pos).min(self.lfo_frames);
            self.lfo_frames -= len;
            let lfo = self.lfo;
            let bend = self.bend + lfo * self.mod_wheel * MOD_WHEEL_VIBRATO;
            let mut levels = [1.0f32; CONTROL_BLOCK];
            self.level.fill(&mut levels[..len], sample_rate);
            for voice in self.voices.iter_mut().filter(|v| v.active) {
                voice.render(
                    &self.derived,
                    (lfo, bend, &levels[..len]),
                    sample_rate,
                    &mut self.rng,
                    &mut left[pos..pos + len],
                    &mut right[pos..pos + len],
                );
            }
            pos += len;
        }
    }
}

impl Default for PolySynth {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for PolySynth {
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
        let len = left.len();
        let mut pos = 0;
        let mut next_event = 0;
        while pos < len {
            while next_event < events.len() && events[next_event].offset as usize <= pos {
                self.handle(events[next_event].kind);
                next_event += 1;
            }
            let end = events.get(next_event).map_or(len, |e| (e.offset as usize).min(len));
            self.render(&mut left[pos..end], &mut right[pos..end]);
            pos = end;
        }
        for event in &events[next_event..] {
            self.handle(event.kind);
        }
    }

    fn reset(&mut self) {
        self.held_len = 0;
        self.pedal.clear();
        self.level.settle();
        for voice in &mut self.voices {
            voice.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    const SR: f64 = 48_000.0;
    const ON: fn(u8) -> EventKind = |pitch| EventKind::NoteOn { pitch, velocity: 1.0 };
    const OFF: fn(u8) -> EventKind = |pitch| EventKind::NoteOff { pitch };
    const PEDAL: fn(bool) -> EventKind =
        |down| EventKind::Control { controller: 64, value: if down { 1.0 } else { 0.0 } };

    /// A sine that starts and stops at once, so what is sounding is plain
    /// to measure.
    fn sine_synth(params: &[(&str, f64)]) -> PolySynth {
        let mut synth = PolySynth::new();
        synth.prepare(SR, 512);
        let plain = [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0), ("lfo_pitch", 0.0)];
        for (id, value) in plain.iter().chain(params) {
            let index = PARAMS.iter().position(|def| def.id == *id).unwrap_or_else(|| panic!("no parameter '{id}'"));
            synth.set_param(index, *value);
        }
        synth
    }

    #[test]
    fn the_pitch_wheel_bends_two_semitones_each_way() {
        let mut synth = sine_synth(&[]);
        let bend = |value: f32| EventKind::PitchBend { value };
        let events = [(0, ON(69)), (24_000, bend(1.0)), (48_000, bend(-1.0)), (72_000, bend(0.5)), (96_000, bend(0.0))];
        let (left, _) = run(&mut synth, SR, 256, 120_000, &events);
        let pitch = |second: usize| zero_crossing_hz(&left[second * 24_000 + 2_400..(second + 1) * 24_000], SR);
        let semitones = |hz: f64| 12.0 * (hz / 440.0).log2();
        assert!(semitones(pitch(0)).abs() < 0.01, "at rest: {} Hz", pitch(0));
        assert!((semitones(pitch(1)) - 2.0).abs() < 0.01, "fully up: {} Hz", pitch(1));
        assert!((semitones(pitch(2)) + 2.0).abs() < 0.01, "fully down: {} Hz", pitch(2));
        assert!((semitones(pitch(3)) - 1.0).abs() < 0.01, "half way: {} Hz", pitch(3));
        assert!(semitones(pitch(4)).abs() < 0.01, "back at rest: {} Hz", pitch(4));
        // A bend does not click: the wave carries on from where it was.
        assert!(largest_step(&left[23_000..25_000]) < 0.1);

        // Nonsense from a broken controller is no bend at all.
        let mut synth = sine_synth(&[]);
        let (left, _) = run(&mut synth, SR, 256, 24_000, &[(0, bend(f32::NAN)), (0, ON(69))]);
        assert!(semitones(zero_crossing_hz(&left[2_400..], SR)).abs() < 0.01);
    }

    #[test]
    fn the_pedal_holds_notes_until_it_comes_up() {
        // The key comes up with the pedal down and the note sounds on; the
        // pedal comes up and it ends. A second key is still down then.
        let mut synth = sine_synth(&[]);
        let events = [
            (0, ON(60)),
            (0, ON(72)),
            (4_800, PEDAL(true)),
            (9_600, OFF(60)),
            (33_600, PEDAL(false)),
            (57_600, OFF(72)),
        ];
        let (left, _) = run(&mut synth, SR, 256, 72_000, &events);
        let level = |from: usize| rms(&left[from..from + 4_800]);
        let both = level(2_400);
        assert!(level(24_000) > both * 0.95, "held by the pedal: {} against {both}", level(24_000));
        let one = level(48_000);
        assert!(one > both * 0.3 && one < both * 0.8, "the pedal let one note go: {one} against {both}");
        assert_eq!(peak(&left[62_400..]), 0.0, "and the last ends with its key");

        // The same key again while the pedal holds it is that key's again:
        // it ends when the key comes up after the pedal.
        let mut synth = sine_synth(&[]);
        let events = [(0, ON(60)), (2_400, PEDAL(true)), (4_800, OFF(60)), (9_600, ON(60)), (14_400, PEDAL(false))];
        let (left, _) = run(&mut synth, SR, 256, 30_000, &events);
        assert!(rms(&left[24_000..]) > 0.05, "the key is down, so the note sounds on");

        // Everything is let go of with the pedal down, and it stays down:
        // the next note is held again.
        let mut synth = sine_synth(&[]);
        let events = [
            (0, PEDAL(true)),
            (0, ON(60)),
            (2_400, OFF(60)),
            (9_600, EventKind::AllNotesOff),
            (19_200, ON(64)),
            (21_600, OFF(64)),
        ];
        let (left, _) = run(&mut synth, SR, 256, 48_000, &events);
        assert!(rms(&left[4_800..9_600]) > 0.05);
        assert_eq!(peak(&left[12_000..19_200]), 0.0, "all notes off ends what the pedal held");
        assert!(rms(&left[38_400..]) > 0.05, "the pedal is still down");

        // In mono mode the pedal holds the last key in the same way.
        let mut synth = sine_synth(&[("mono", 1.0)]);
        let events = [(0, ON(60)), (2_400, PEDAL(true)), (4_800, OFF(60)), (24_000, PEDAL(false))];
        let (left, _) = run(&mut synth, SR, 256, 36_000, &events);
        assert!(rms(&left[14_400..24_000]) > 0.05);
        assert_eq!(peak(&left[28_800..]), 0.0);
    }

    #[test]
    fn the_mod_wheel_adds_vibrato() {
        // How far the pitch strays from where it belongs over a second, in
        // semitones, measured a hundredth of a second at a time.
        let wobble = |synth: &mut PolySynth, wheel: f32| {
            let events = [(0, EventKind::Control { controller: 1, value: wheel }), (0, ON(81))];
            let (left, _) = run(synth, SR, 256, 52_800, &events);
            let pitches: Vec<f64> =
                left[4_800..].chunks(480).map(|chunk| 12.0 * (zero_crossing_hz(chunk, SR) / 880.0).log2()).collect();
            pitches.iter().fold(0.0f64, |most, pitch| most.max(pitch.abs()))
        };
        let still = wobble(&mut sine_synth(&[]), 0.0);
        assert!(still < 0.05, "no vibrato with the wheel down: {still}");
        let full = wobble(&mut sine_synth(&[]), 1.0);
        assert!((0.3..0.6).contains(&full), "half a semitone with the wheel up: {full}");
        let half = wobble(&mut sine_synth(&[]), 0.5);
        assert!(half > still && half < full, "{half} between {still} and {full}");
    }

    const CONTROL: fn(u8, f32) -> EventKind = |controller, value| EventKind::Control { controller, value };

    #[test]
    fn expression_and_volume_set_the_level() {
        let level = |events: &[(usize, EventKind)]| {
            let mut synth = sine_synth(&[]);
            let (left, _) = run(&mut synth, SR, 256, 48_000, events);
            rms(&left[24_000..])
        };
        let full = level(&[(0, ON(69))]);
        let half = level(&[(0, CONTROL(11, 0.5)), (0, ON(69))]);
        assert!(
            (half / full - 0.25).abs() < 1e-3,
            "half the expression is a quarter of the amplitude: {}",
            half / full
        );
        let both = level(&[(0, CONTROL(7, 0.5)), (0, CONTROL(11, 0.5)), (0, ON(69))]);
        assert!((both / full - 0.0625).abs() < 1e-3, "volume and expression multiply: {}", both / full);
        assert_eq!(level(&[(0, CONTROL(11, 0.0)), (0, ON(69))]), 0.0, "no expression, no sound");

        // A synth told that both are at the top plays to the bit what one
        // that was never told anything plays.
        let untouched = run(&mut sine_synth(&[]), SR, 256, 4_800, &[(0, ON(69))]).0;
        let topped = [(0, CONTROL(11, 1.0)), (0, CONTROL(7, 1.0)), (0, ON(69))];
        assert_eq!(run(&mut sine_synth(&[]), SR, 256, 4_800, &topped).0, untouched);

        // A level set before a note is the note's level from its first
        // sample: nothing of it is heard louder on the way there.
        let quiet = run(&mut sine_synth(&[]), SR, 256, 4_800, &[(0, CONTROL(11, 0.5)), (0, ON(69))]).0;
        assert!(peak(&quiet) <= 0.25 * peak(&untouched) * 1.001, "{} against {}", peak(&quiet), peak(&untouched));
        // And one that is still in force after every voice was cut off.
        let mut synth = sine_synth(&[]);
        run(&mut synth, SR, 256, 4_800, &[(0, ON(69)), (2_400, CONTROL(11, 0.5))]);
        synth.reset();
        let after = run(&mut synth, SR, 256, 4_800, &[(0, ON(69))]).0;
        assert!((peak(&after) / peak(&untouched) - 0.25).abs() < 1e-3);
    }

    #[test]
    fn a_swell_is_smooth_and_a_step_in_expression_does_not_click() {
        // Expression rising from nothing to full over a second, sent every
        // 32 frames as the engine sends a curve.
        let mut events = vec![(0, CONTROL(11, 0.0)), (0, ON(69))];
        events.extend((1..=1_500).map(|n| (n * 32, CONTROL(11, n as f32 / 1_500.0))));
        let swell = run(&mut sine_synth(&[]), SR, 256, 52_800, &events).0;
        let plain = run(&mut sine_synth(&[]), SR, 256, 52_800, &[(0, ON(69))]).0;
        // What the swell multiplied the plain note by, wherever the wave is
        // far enough from zero to say.
        let gains: Vec<(usize, f32)> = (2_400..48_000)
            .filter(|&frame| plain[frame].abs() > 0.3)
            .map(|frame| (frame, swell[frame] / plain[frame]))
            .collect();
        for &(frame, gain) in &gains {
            // The level follows the square of the expression, a few
            // milliseconds behind.
            let expression = frame as f32 / 48_000.0;
            assert!((gain - expression * expression).abs() < 0.01, "at frame {frame}: {gain}");
        }
        for pair in gains.windows(2).filter(|pair| pair[1].0 == pair[0].0 + 1) {
            let step = pair[1].1 - pair[0].1;
            assert!((-1e-6..5e-5).contains(&step), "the gain steps by {step} at frame {}", pair[1].0);
        }

        // A sudden drop while the note sounds takes its few milliseconds.
        let events = [(0, ON(69)), (24_000, CONTROL(11, 0.2))];
        let dropped = run(&mut sine_synth(&[]), SR, 256, 36_000, &events).0;
        assert!(largest_step(&dropped[23_900..24_600]) <= largest_step(&plain[23_900..24_600]) * 1.05);
        assert!((rms(&dropped[26_400..36_000]) / rms(&plain[26_400..36_000]) - 0.04).abs() < 1e-3);
    }
}
