//! A wavetable written down: the recipe a table is made from.
//!
//! A recipe is a few key frames and the number of frames the table is to
//! have. A key frame is a named shape with the numbers that shape takes
//! (`{"shape": "pulse", "width": 0.3}`), or a list of harmonics. Between
//! two key frames of the same shape the numbers are what moves, so a pulse
//! of width 0.5 and one of 0.05 make a table that sweeps the width;
//! between two different shapes the frames go from one wave to the other.
//!
//! ```json
//! { "name": "Thin Reed", "size": 32, "frames": [
//!     { "shape": "pulse", "width": 0.5 },
//!     { "shape": "pulse", "width": 0.04, "tilt": -3 } ] }
//! ```
//!
//! The factory tables are recipes too ([`super::factory`]), so that what
//! ships can be read, copied and changed.
//!
//! A table can also be taken from an audio file ([`FileSpec`]): a file of
//! single cycles laid end to end, as wavetable files are, or a recording
//! of a held note, of which the harmonics are read along its length.

use super::table::{Harmonics, MAX_FRAMES, MAX_HARMONICS, Table, peak_of};
use crate::fft::RealFft;
use crate::util::Rng;
use serde::{Deserialize, Serialize};
use std::f64::consts::{PI, TAU};
use trill_core::audio::AudioBuffer;

/// The frames a recipe of two or more key frames makes when it does not
/// say how many.
const DEFAULT_SIZE: usize = 32;
/// The points a shape that is worked out as a wave is drawn with, before
/// its harmonics are read: enough that what lies over [`MAX_HARMONICS`]
/// folds back far under them.
const DRAWN: usize = 16_384;
/// The samples of a cycle in a wavetable file that does not say.
pub const FILE_CYCLE: usize = 2048;
/// The frames read from a recording of a held note that does not say.
const RECORDING_FRAMES: usize = 64;

/// What a key frame can be.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Sine,
    Triangle,
    Saw,
    Square,
    /// A pulse of `width`.
    Pulse,
    /// Every harmonic at the same level, to be shaped by `tilt`, `cutoff`,
    /// `peak`, `odd`, `even` and `comb`.
    Spectrum,
    /// A sine whose phase a sine `ratio` times as fast bends by `index`.
    Fm,
    /// A sine folded back on itself, `amount` of the way to eight folds.
    Fold,
    /// A sine restarted `ratio` times in the cycle.
    Sync,
    /// A burst of a sine at harmonic `at`, `width` of the cycle long.
    Formant,
    /// Harmonics at levels drawn by lot from `seed`.
    Noise,
    /// The harmonics listed in `harmonics`.
    #[default]
    Harmonics,
}

impl Shape {
    pub const NAMES: &'static [&'static str] = &[
        "sine",
        "triangle",
        "saw",
        "square",
        "pulse",
        "spectrum",
        "fm",
        "fold",
        "sync",
        "formant",
        "noise",
        "harmonics",
    ];

    fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// The numbers the shape takes, besides the ones any shape does.
    fn takes(self) -> &'static [&'static str] {
        match self {
            Shape::Sine | Shape::Triangle | Shape::Saw | Shape::Square | Shape::Spectrum => &[],
            Shape::Pulse => &["width"],
            Shape::Fm => &["ratio", "index"],
            Shape::Fold => &["amount"],
            Shape::Sync => &["ratio"],
            Shape::Formant => &["at", "width"],
            Shape::Noise => &["seed"],
            Shape::Harmonics => &["harmonics", "phases"],
        }
    }
}

/// One key frame of a recipe.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    /// Left out, a frame with `harmonics` is those and any other a sine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    /// `pulse`: the share of the cycle the pulse is high, 0.01..=0.99
    /// (0.5 is a square). `formant`: the share of the cycle the burst
    /// lasts, 0.02..=1 (shorter is a broader formant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// `fold`: 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    /// `fm`: how many times the fundamental the modulator is (whole
    /// numbers keep the cycle seamless). `sync`: how many times the sine
    /// restarts, 1..=32.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    /// `fm`: how far the phase is bent, in radians, 0..=24.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<f64>,
    /// `formant`: the harmonic the burst rings at, 1..=256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// `noise`: which lot is drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    /// `harmonics`: the level of each harmonic from the first, up to 512.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harmonics: Option<Vec<f64>>,
    /// `harmonics`: where in its cycle each harmonic begins, in cycles
    /// (0.25 is a cosine). Left out, each is a sine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<f64>>,
    /// Any shape: dB added per octave of harmonic (-6 makes `spectrum` a
    /// sawtooth's levels, -12 a triangle's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<f64>,
    /// Any shape: the harmonic from which the levels fall away, 24 dB an
    /// octave: a low-pass filter drawn into the wave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<f64>,
    /// Any shape: dB by which the harmonics around `cutoff` are raised: a
    /// filter's resonance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak: Option<f64>,
    /// Any shape: what the odd harmonics above the first are multiplied
    /// by, 0..=2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub odd: Option<f64>,
    /// Any shape: what the even harmonics are multiplied by, 0..=2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub even: Option<f64>,
    /// Any shape: notches drawn across the harmonics as a phaser draws
    /// them, 0..=1: harmonic `h` is multiplied by the square of
    /// `cos(pi * h * comb / 2)`, so 0.1 takes out every tenth, from the
    /// fifth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comb: Option<f64>,
}

impl Frame {
    /// A frame of `shape` with nothing else said.
    pub fn of(shape: Shape) -> Self {
        Self { shape: Some(shape), ..Self::default() }
    }

    fn shape(&self) -> Shape {
        self.shape.unwrap_or(if self.harmonics.is_some() { Shape::Harmonics } else { Shape::Sine })
    }

    /// What is wrong with the frame, as the end of a sentence about it.
    fn check(&self) -> Result<(), String> {
        let shape = self.shape();
        let given = [
            ("width", self.width.is_some()),
            ("amount", self.amount.is_some()),
            ("ratio", self.ratio.is_some()),
            ("index", self.index.is_some()),
            ("at", self.at.is_some()),
            ("seed", self.seed.is_some()),
            ("harmonics", self.harmonics.is_some()),
            ("phases", self.phases.is_some()),
        ];
        for (name, given) in given {
            if given && !shape.takes().contains(&name) {
                let takes = match shape.takes() {
                    [] => "takes none of its own".to_string(),
                    takes => format!("takes {}", takes.join(" and ")),
                };
                return Err(format!(
                    "has `{name}`, which the shape `{}` does not take (it {takes}; any shape takes tilt, cutoff, \
                     peak, odd, even and comb)",
                    shape.name()
                ));
            }
        }
        let within = |name: &str, value: Option<f64>, low: f64, high: f64| match value {
            Some(value) if !value.is_finite() || value < low || value > high => {
                Err(format!("has `{name}` at {value}, which is outside {low} to {high}"))
            }
            _ => Ok(()),
        };
        let width = if shape == Shape::Formant { (0.02, 1.0) } else { (0.01, 0.99) };
        within("width", self.width, width.0, width.1)?;
        within("amount", self.amount, 0.0, 1.0)?;
        within("ratio", self.ratio, if shape == Shape::Sync { 1.0 } else { 0.0 }, 32.0)?;
        within("index", self.index, 0.0, 24.0)?;
        within("at", self.at, 1.0, 256.0)?;
        within("tilt", self.tilt, -48.0, 24.0)?;
        within("cutoff", self.cutoff, 1.0, MAX_HARMONICS as f64)?;
        within("peak", self.peak, 0.0, 48.0)?;
        within("odd", self.odd, 0.0, 2.0)?;
        within("even", self.even, 0.0, 2.0)?;
        within("comb", self.comb, 0.0, 1.0)?;
        if shape == Shape::Harmonics {
            let Some(harmonics) = &self.harmonics else {
                return Err("is of the shape `harmonics` and lists none (`harmonics`: [1, 0.5, ...])".to_string());
            };
            if harmonics.is_empty() || harmonics.len() > MAX_HARMONICS {
                return Err(format!("lists {} harmonics; a frame has 1 to {MAX_HARMONICS}", harmonics.len()));
            }
            if harmonics.iter().any(|level| !level.is_finite() || level.abs() > 1e6) {
                return Err("lists a harmonic that is not a number a level can be".to_string());
            }
            if self.phases.as_ref().is_some_and(|phases| phases.iter().any(|phase| !phase.is_finite())) {
                return Err("lists a phase that is not a number".to_string());
            }
        }
        Ok(())
    }

    /// Whether the way from this frame to `other` is made by moving the
    /// numbers: the same shape, and the same lot where one is drawn.
    fn moves_to(&self, other: &Frame) -> bool {
        self.shape() == other.shape() && self.seed == other.seed
    }

    /// The frame `t` of the way from this one to `other`, which is of the
    /// same shape.
    fn towards(&self, other: &Frame, t: f64) -> Frame {
        let line = |a: Option<f64>, b: Option<f64>, unsaid: f64| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(unsaid) + (b.unwrap_or(unsaid) - a.unwrap_or(unsaid)) * t),
        };
        // What is heard as a ratio moves as one.
        let ratio = |a: Option<f64>, b: Option<f64>, unsaid: f64| match (a, b) {
            (None, None) => None,
            (a, b) => {
                let (a, b) = (a.unwrap_or(unsaid).max(1e-6), b.unwrap_or(unsaid).max(1e-6));
                Some(a * (b / a).powf(t))
            }
        };
        let list = |a: &Option<Vec<f64>>, b: &Option<Vec<f64>>| match (a, b) {
            (None, None) => None,
            (a, b) => {
                let (a, b) = (a.as_deref().unwrap_or_default(), b.as_deref().unwrap_or_default());
                let at = |list: &[f64], index: usize| list.get(index).copied().unwrap_or(0.0);
                Some((0..a.len().max(b.len())).map(|index| at(a, index) + (at(b, index) - at(a, index)) * t).collect())
            }
        };
        let shape = self.shape();
        let (width, ratio_of, index, at) = defaults(shape);
        Frame {
            shape: Some(shape),
            width: line(self.width, other.width, width),
            amount: line(self.amount, other.amount, 0.0),
            ratio: if shape == Shape::Sync {
                ratio(self.ratio, other.ratio, ratio_of)
            } else {
                line(self.ratio, other.ratio, ratio_of)
            },
            index: line(self.index, other.index, index),
            at: ratio(self.at, other.at, at),
            seed: self.seed,
            harmonics: list(&self.harmonics, &other.harmonics),
            phases: list(&self.phases, &other.phases),
            tilt: line(self.tilt, other.tilt, 0.0),
            cutoff: ratio(self.cutoff, other.cutoff, MAX_HARMONICS as f64),
            peak: line(self.peak, other.peak, 0.0),
            odd: line(self.odd, other.odd, 1.0),
            even: line(self.even, other.even, 1.0),
            comb: line(self.comb, other.comb, 0.0),
        }
    }
}

/// What a shape's numbers are where a frame does not say: `width`,
/// `ratio`, `index` and `at`.
fn defaults(shape: Shape) -> (f64, f64, f64, f64) {
    match shape {
        Shape::Formant => (0.25, 1.0, 1.0, 4.0),
        Shape::Sync => (0.5, 2.0, 1.0, 4.0),
        _ => (0.5, 1.0, 1.0, 4.0),
    }
}

/// How the frames of a table are made as loud as one another.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalize {
    /// Every frame reaches full level: the peak of each is 1.
    #[default]
    Each,
    /// The loudest frame reaches full level and the others keep their
    /// level against it.
    Table,
}

/// A wavetable written down. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// How many frames the table has, 1..=256: the key frames are spread
    /// evenly over them. Left out, one key frame is a table of one frame
    /// and more make 32, or as many as there are key frames if that is
    /// more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    pub frames: Vec<Frame>,
    #[serde(default, skip_serializing_if = "is_each")]
    pub normalize: Normalize,
}

fn is_each(normalize: &Normalize) -> bool {
    *normalize == Normalize::Each
}

impl Recipe {
    pub fn new(name: &str, size: usize, frames: Vec<Frame>) -> Self {
        Self { name: name.to_string(), size: Some(size), frames, normalize: Normalize::Each }
    }

    /// How many frames the table made from this has.
    pub fn frame_count(&self) -> usize {
        match (self.size, self.frames.len()) {
            (Some(size), _) => size,
            (None, keys) if keys <= 1 => 1,
            (None, keys) => keys.max(DEFAULT_SIZE),
        }
    }

    /// What is wrong with the recipe, in a sentence that says what would
    /// be right.
    pub fn check(&self) -> Result<(), String> {
        if self.frames.is_empty() {
            return Err(format!(
                "a table needs at least one frame: `frames` is a list of key frames, each a shape ({}) with its \
                 numbers, or a list of `harmonics`",
                Shape::NAMES.join(", ")
            ));
        }
        if self.frames.len() > MAX_FRAMES {
            return Err(format!("{} key frames; a table has {MAX_FRAMES} frames at most", self.frames.len()));
        }
        if let Some(size) = self.size
            && !(1..=MAX_FRAMES).contains(&size)
        {
            return Err(format!("`size` is {size}; a table has 1 to {MAX_FRAMES} frames"));
        }
        for (index, frame) in self.frames.iter().enumerate() {
            frame.check().map_err(|problem| format!("key frame {} {problem}", index + 1))?;
        }
        Ok(())
    }

    /// The frames of the table, each as its harmonics. A recipe that does
    /// not [`check`](Self::check) makes what it can.
    pub fn harmonics(&self) -> Vec<Harmonics> {
        let mut maker = Maker::new();
        let count = self.frame_count().clamp(1, MAX_FRAMES);
        let keys = &self.frames[..self.frames.len().min(MAX_FRAMES)];
        let mut frames: Vec<Harmonics> = Vec::with_capacity(count);
        // A key frame is made once however many frames lie on it.
        let mut made: Vec<Option<Harmonics>> = vec![None; keys.len()];
        for index in 0..count {
            let at = if count == 1 || keys.len() <= 1 {
                0.0
            } else {
                index as f64 / (count - 1) as f64 * (keys.len() - 1) as f64
            };
            let Some(first) = keys.get((at.floor() as usize).min(keys.len().saturating_sub(1))) else {
                frames.push(Harmonics::silent());
                continue;
            };
            let from = (at.floor() as usize).min(keys.len() - 1);
            let t = at - from as f64;
            let mut key =
                |index: usize, maker: &mut Maker| made[index].get_or_insert_with(|| maker.frame(&keys[index])).clone();
            let frame = match keys.get(from + 1) {
                Some(second) if t > 1e-9 => {
                    if first.moves_to(second) {
                        maker.frame(&first.towards(second, t))
                    } else {
                        let (a, b) = (key(from, &mut maker), key(from + 1, &mut maker));
                        a.mixed(&b, t as f32)
                    }
                }
                _ => key(from, &mut maker),
            };
            frames.push(frame);
        }
        normalize(&mut frames, self.normalize);
        frames
    }

    /// The table this is the recipe of.
    pub fn table(&self) -> Table {
        Table::new(self.name.clone(), &self.harmonics())
    }
}

fn normalize(frames: &mut [Harmonics], how: Normalize) {
    let peaks: Vec<f32> = frames.iter().map(peak_of).collect();
    let loudest = peaks.iter().fold(0.0f32, |most, peak| most.max(*peak));
    for (frame, peak) in frames.iter_mut().zip(peaks) {
        let against = match how {
            Normalize::Each => peak,
            Normalize::Table => loudest,
        };
        if against > 1e-9 {
            frame.scale(1.0 / against);
        }
    }
}

/// Makes frames: holds what is costly to set up between one and the next.
struct Maker {
    plan: RealFft,
    wave: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Maker {
    fn new() -> Self {
        Self {
            plan: RealFft::new(DRAWN),
            wave: vec![0.0; DRAWN],
            re: vec![0.0; DRAWN / 2 + 1],
            im: vec![0.0; DRAWN / 2 + 1],
        }
    }

    /// The harmonics of the wave `draw` gives for a phase from 0 to 1.
    fn drawn(&mut self, draw: impl Fn(f64) -> f64) -> Harmonics {
        for (point, sample) in self.wave.iter_mut().enumerate() {
            *sample = draw(point as f64 / DRAWN as f64) as f32;
        }
        self.plan.forward(&self.wave, &mut self.re, &mut self.im);
        let mut frame = Harmonics::silent();
        let scale = 2.0 / DRAWN as f32;
        for h in 1..=MAX_HARMONICS {
            frame.cos[h - 1] = self.re[h] * scale;
            frame.sin[h - 1] = -self.im[h] * scale;
        }
        frame
    }

    /// One key frame as its harmonics, before the table is made even.
    fn frame(&mut self, frame: &Frame) -> Harmonics {
        let shape = frame.shape();
        let (width, ratio, index, at) = defaults(shape);
        let width = frame.width.unwrap_or(width);
        let ratio = frame.ratio.unwrap_or(ratio);
        let index = frame.index.unwrap_or(index);
        let at = frame.at.unwrap_or(at);
        let mut made = Harmonics::silent();
        let each = |made: &mut Harmonics, level: &dyn Fn(f64) -> (f64, f64)| {
            for h in 1..=MAX_HARMONICS {
                let (sin, cos) = level(h as f64);
                made.sin[h - 1] = sin as f32;
                made.cos[h - 1] = cos as f32;
            }
        };
        match shape {
            Shape::Sine => made.sin[0] = 1.0,
            // All four begin at nothing and rise, so that a note which
            // starts at the beginning of the cycle starts without a step.
            Shape::Saw => each(&mut made, &|h| (if h % 2.0 == 1.0 { 2.0 } else { -2.0 } / (PI * h), 0.0)),
            Shape::Square => each(&mut made, &|h| (if h % 2.0 == 1.0 { 4.0 / (PI * h) } else { 0.0 }, 0.0)),
            Shape::Triangle => each(&mut made, &|h| {
                let sign = if h % 4.0 == 1.0 { 1.0 } else { -1.0 };
                (if h % 2.0 == 1.0 { sign * 8.0 / (PI * PI * h * h) } else { 0.0 }, 0.0)
            }),
            Shape::Pulse => {
                let width = width.clamp(0.01, 0.99);
                each(&mut made, &|h| {
                    let turn = TAU * h * width;
                    (2.0 / (PI * h) * (1.0 - turn.cos()), 2.0 / (PI * h) * turn.sin())
                });
            }
            Shape::Spectrum => each(&mut made, &|_| (1.0, 0.0)),
            Shape::Noise => {
                let mut lot = Rng::new(frame.seed.unwrap_or(1).wrapping_mul(0x9E37_79B9) | 1);
                for h in 1..=MAX_HARMONICS {
                    let level = 0.25 + 0.75 * f64::from(lot.unipolar());
                    let turn = TAU * f64::from(lot.unipolar());
                    made.sin[h - 1] = (level * turn.cos()) as f32;
                    made.cos[h - 1] = (level * turn.sin()) as f32;
                }
            }
            Shape::Harmonics => {
                let levels = frame.harmonics.as_deref().unwrap_or_default();
                let phases = frame.phases.as_deref().unwrap_or_default();
                for (index, level) in levels.iter().take(MAX_HARMONICS).enumerate() {
                    let turn = TAU * phases.get(index).copied().unwrap_or(0.0);
                    made.sin[index] = (level * turn.cos()) as f32;
                    made.cos[index] = (level * turn.sin()) as f32;
                }
            }
            Shape::Fm => made = self.drawn(|phase| (TAU * phase + index * (TAU * ratio * phase).sin()).sin()),
            Shape::Fold => {
                let drive = 1.0 + 7.0 * frame.amount.unwrap_or(0.0).clamp(0.0, 1.0);
                // Whatever goes past full level comes back down, and
                // past the bottom back up: with no drive, the sine itself.
                made = self.drawn(|phase| {
                    let pushed = drive * (TAU * phase).sin();
                    1.0 - ((pushed + 1.0).rem_euclid(4.0) - 2.0).abs()
                });
            }
            Shape::Sync => {
                let ratio = ratio.clamp(1.0, 32.0);
                // The last sixteenth of the cycle fades to the nothing the
                // next one begins at, so that the restart is not a step.
                made = self.drawn(|phase| {
                    let fade =
                        if phase > 15.0 / 16.0 { 0.5 + 0.5 * (PI * (phase - 15.0 / 16.0) * 16.0).cos() } else { 1.0 };
                    (TAU * ratio * phase).sin() * fade
                });
            }
            Shape::Formant => {
                let width = width.clamp(0.02, 1.0);
                made = self.drawn(|phase| {
                    if phase >= width {
                        return 0.0;
                    }
                    (TAU * at * phase).sin() * (0.5 - 0.5 * (TAU * phase / width).cos())
                });
            }
        }

        // What any shape takes: the levels of its harmonics moved.
        let tilt = frame.tilt.unwrap_or(0.0);
        let cutoff = frame.cutoff;
        let peak = frame.peak.unwrap_or(0.0);
        let (odd, even) = (frame.odd.unwrap_or(1.0), frame.even.unwrap_or(1.0));
        let comb = frame.comb.unwrap_or(0.0);
        if tilt != 0.0 || cutoff.is_some() || odd != 1.0 || even != 1.0 || comb != 0.0 {
            for h in 1..=MAX_HARMONICS {
                let number = h as f64;
                let mut gain = 10f64.powf(tilt * number.log2() / 20.0);
                if let Some(cutoff) = cutoff {
                    let octaves = (number / cutoff.max(1.0)).log2();
                    if octaves > 0.0 {
                        gain *= 10f64.powf(-24.0 * octaves / 20.0);
                    }
                    // A resonance a third of an octave wide either side.
                    gain *= 10f64.powf(peak * (-(octaves * 3.0).powi(2)).exp() / 20.0);
                }
                if h > 1 {
                    gain *= if h % 2 == 1 { odd } else { even };
                }
                if comb != 0.0 {
                    gain *= (0.5 * PI * number * comb).cos().powi(2);
                }
                made.sin[h - 1] *= gain as f32;
                made.cos[h - 1] *= gain as f32;
            }
        }
        made
    }
}

/// A table taken from an audio file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    /// The audio file.
    pub file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The samples of one cycle, for a file of single cycles laid end to
    /// end: each cycle is a frame. Wavetable files have 2048.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<usize>,
    /// The note the file plays (60 is middle C, fractions allowed), for a
    /// recording of a held note: its harmonics are read along its length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<f64>,
    /// How many frames to read from a recording, 1..=256 (64 unsaid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<usize>,
}

/// How a file was read as a table.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    /// As cycles of this many samples laid end to end.
    Cycles(usize),
    /// As a recording of a note at this many Hz.
    Recording(f64),
}

impl FileSpec {
    pub fn check(&self) -> Result<(), String> {
        if self.file.trim().is_empty() {
            return Err("`file` is empty: it is the path of an audio file".to_string());
        }
        if self.cycle.is_some() && self.root.is_some() {
            return Err("`cycle` says the file is single cycles laid end to end and `root` that it is a recording \
                        of a note: say one of the two"
                .to_string());
        }
        if let Some(cycle) = self.cycle
            && !(16..=65_536).contains(&cycle)
        {
            return Err(format!("`cycle` is {cycle}; a cycle has 16 to 65536 samples (wavetable files have 2048)"));
        }
        if let Some(root) = self.root
            && !(0.0..=127.0).contains(&root)
        {
            return Err(format!("`root` is {root}; it is a note from 0 to 127 (60 is middle C)"));
        }
        if let Some(frames) = self.frames
            && !(1..=MAX_FRAMES).contains(&frames)
        {
            return Err(format!("`frames` is {frames}; a table has 1 to {MAX_FRAMES} frames"));
        }
        Ok(())
    }

    /// The frames `audio` makes, with how it was read; or why it makes
    /// none, as the end of a sentence about the file.
    pub fn harmonics(&self, audio: &AudioBuffer) -> Result<(Vec<Harmonics>, Reading), String> {
        let mono: Vec<f32> = audio.left.iter().zip(&audio.right).map(|(left, right)| 0.5 * (left + right)).collect();
        if mono.iter().all(|sample| sample.abs() < 1e-9) {
            return Err("is silent".to_string());
        }
        let whole_cycles = mono.len() >= FILE_CYCLE && mono.len() % FILE_CYCLE == 0;
        let as_cycles = self
            .cycle
            .or((self.root.is_none() && whole_cycles && mono.len() / FILE_CYCLE <= MAX_FRAMES).then_some(FILE_CYCLE));
        let (mut frames, reading) = match as_cycles {
            Some(cycle) => {
                if mono.len() < cycle {
                    return Err(format!("has {} samples, which is less than one cycle of {cycle}", mono.len()));
                }
                (cycles(&mono, cycle), Reading::Cycles(cycle))
            }
            None => {
                let hz = match self.root {
                    Some(root) => 440.0 * 2f64.powf((root - 69.0) / 12.0),
                    None => pitch_of(&mono, audio.sample_rate).ok_or_else(|| {
                        "has no steady pitch that could be found: say the note it plays as `root`, or the samples \
                         of one cycle as `cycle`"
                            .to_string()
                    })?,
                };
                let count = self.frames.unwrap_or(RECORDING_FRAMES).clamp(1, MAX_FRAMES);
                (recording(&mono, audio.sample_rate, hz, count)?, Reading::Recording(hz))
            }
        };
        normalize(&mut frames, Normalize::Each);
        Ok((frames, reading))
    }
}

/// The frames of a file of single cycles of `cycle` samples: every cycle,
/// or 256 of them evenly through the file.
fn cycles(mono: &[f32], cycle: usize) -> Vec<Harmonics> {
    let available = mono.len() / cycle;
    let count = available.min(MAX_FRAMES);
    let mut plan = cycle.is_power_of_two().then(|| RealFft::new(cycle));
    let (mut re, mut im) = (vec![0.0f32; cycle / 2 + 1], vec![0.0f32; cycle / 2 + 1]);
    (0..count)
        .map(|index| {
            let which = if count == available { index } else { index * (available - 1) / (count - 1).max(1) };
            let wave = &mono[which * cycle..(which + 1) * cycle];
            let mut frame = Harmonics::silent();
            let top = MAX_HARMONICS.min((cycle - 1) / 2);
            match &mut plan {
                Some(plan) => {
                    plan.forward(wave, &mut re, &mut im);
                    let scale = 2.0 / cycle as f32;
                    for h in 1..=top {
                        frame.cos[h - 1] = re[h] * scale;
                        frame.sin[h - 1] = -im[h] * scale;
                    }
                }
                None => {
                    for h in 1..=top {
                        let (sin, cos) = tone(wave, h as f64 / cycle as f64, None);
                        frame.sin[h - 1] = sin as f32;
                        frame.cos[h - 1] = cos as f32;
                    }
                }
            }
            frame
        })
        .collect()
}

/// How much of the tone at `cycles_per_sample` is in `wave`, as the
/// amplitudes of its sine and its cosine; under a window, if one is given.
fn tone(wave: &[f32], cycles_per_sample: f64, window: Option<&[f64]>) -> (f64, f64) {
    let (mut sin, mut cos, mut weight) = (0.0f64, 0.0f64, 0.0f64);
    for (n, sample) in wave.iter().enumerate() {
        let turn = TAU * cycles_per_sample * n as f64;
        let under = window.map_or(1.0, |window| window[n]);
        sin += f64::from(*sample) * under * turn.sin();
        cos += f64::from(*sample) * under * turn.cos();
        weight += under;
    }
    (2.0 * sin / weight.max(1e-12), 2.0 * cos / weight.max(1e-12))
}

/// The frames of a recording of a note at `hz`: the levels of its
/// harmonics at `count` moments along it, each read under a window eight
/// cycles long. The harmonics are kept as sines: how they lay against one
/// another in the recording is not kept, so that the frames go into one
/// another without cancelling.
fn recording(mono: &[f32], sample_rate: f64, hz: f64, count: usize) -> Result<Vec<Harmonics>, String> {
    let period = sample_rate / hz;
    let span = (period * 8.0).round() as usize;
    if mono.len() < span || span < 16 {
        return Err(format!(
            "is {:.2} s long, too short to read a note of {hz:.1} Hz from (it takes eight cycles, {:.2} s)",
            mono.len() as f64 / sample_rate,
            span as f64 / sample_rate
        ));
    }
    let window: Vec<f64> = (0..span).map(|n| 0.5 - 0.5 * (TAU * n as f64 / span as f64).cos()).collect();
    let top = MAX_HARMONICS.min((0.49 * sample_rate / hz).floor() as usize);
    let frames = (0..count)
        .map(|index| {
            let room = mono.len() - span;
            let start = if count == 1 { room / 2 } else { index * room / (count - 1) };
            let wave = &mono[start..start + span];
            let mut frame = Harmonics::silent();
            for h in 1..=top {
                let (sin, cos) = tone(wave, h as f64 * hz / sample_rate, Some(&window));
                frame.sin[h - 1] = (sin * sin + cos * cos).sqrt() as f32;
            }
            frame
        })
        .collect();
    Ok(frames)
}

/// The pitch of a recording, in Hz, from how well its loudest stretch
/// matches itself moved along: the shortest move that matches nearly as
/// well as any. `None` where nothing matches well.
pub fn pitch_of(mono: &[f32], sample_rate: f64) -> Option<f64> {
    let longest = (sample_rate / 30.0) as usize;
    let shortest = (sample_rate / 4_000.0).max(2.0) as usize;
    let span = (longest * 2).min(mono.len() / 2);
    if span < shortest * 4 {
        return None;
    }
    // The stretch with the most in it.
    let hop = (span / 2).max(1);
    let energy = |start: usize| mono[start..start + span * 2].iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
    let start =
        (0..=mono.len() - span * 2).step_by(hop).max_by(|a, b| energy(*a).total_cmp(&energy(*b))).unwrap_or_default();
    let piece = &mono[start..start + span * 2];
    let likeness = |lag: usize| {
        let (mut both, mut here, mut there) = (0.0f64, 0.0f64, 0.0f64);
        for n in 0..span {
            let (a, b) = (f64::from(piece[n]), f64::from(piece[n + lag]));
            both += a * b;
            here += a * a;
            there += b * b;
        }
        both / (here * there).sqrt().max(1e-12)
    };
    let lags: Vec<(usize, f64)> = (shortest..=longest.min(span)).map(|lag| (lag, likeness(lag))).collect();
    let best = lags.iter().fold(0.0f64, |best, (_, likeness)| best.max(*likeness));
    if best < 0.6 {
        return None;
    }
    // The first peak that is nearly the best: a note also matches itself
    // two and three cycles on.
    let peak =
        lags.windows(3).find(|trio| trio[1].1 >= best * 0.93 && trio[1].1 >= trio[0].1 && trio[1].1 >= trio[2].1)?;
    // Between the lags, by the parabola through the three.
    let (a, b, c) = (peak[0].1, peak[1].1, peak[2].1);
    let bend = a - 2.0 * b + c;
    let offset = if bend.abs() > 1e-12 { 0.5 * (a - c) / bend } else { 0.0 };
    Some(sample_rate / (peak[1].0 as f64 + offset.clamp(-0.5, 0.5)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(json: &str) -> Recipe {
        serde_json::from_str(json).unwrap_or_else(|error| panic!("{json}: {error}"))
    }

    /// The wave of a frame, as `points` samples of one cycle.
    fn wave(frame: &Harmonics, points: usize) -> Vec<f64> {
        (0..points)
            .map(|point| {
                let phase = point as f64 / points as f64;
                (1..=MAX_HARMONICS)
                    .map(|h| {
                        let turn = TAU * h as f64 * phase;
                        f64::from(frame.sin[h - 1]) * turn.sin() + f64::from(frame.cos[h - 1]) * turn.cos()
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn the_named_shapes_are_the_waves_they_are_named_for() {
        let frames = recipe(
            r#"{ "frames": [ {"shape":"sine"}, {"shape":"triangle"}, {"shape":"saw"}, {"shape":"square"},
                             {"shape":"pulse","width":0.25} ], "size": 5 }"#,
        )
        .harmonics();
        let waves: Vec<Vec<f64>> = frames.iter().map(|frame| wave(frame, 400)).collect();
        for (point, phase) in (0..400).map(|point| (point, point as f64 / 400.0)) {
            // Away from the edges, where a wave of 512 harmonics still rings.
            let edge = [0.0, 0.25, 0.5, 1.0].iter().any(|edge| (phase - edge).abs() < 0.02);
            let triangle = if phase < 0.25 {
                4.0 * phase
            } else if phase < 0.75 {
                2.0 - 4.0 * phase
            } else {
                4.0 * phase - 4.0
            };
            assert!((waves[0][point] - (TAU * phase).sin()).abs() < 1e-3);
            assert!((waves[1][point] - triangle).abs() < 0.01, "triangle at {phase}: {}", waves[1][point]);
            if !edge {
                // Each frame is made to reach 1, which for a wave with an
                // edge is the overshoot beside it: 9 % of the step over
                // the wave, and the step is from -1 to 1.
                let over = 1.179;
                let saw = if phase < 0.5 { 2.0 * phase } else { 2.0 * phase - 2.0 };
                assert!((waves[2][point] * over - saw).abs() < 0.03, "saw at {phase}: {}", waves[2][point]);
                let square = if phase < 0.5 { 1.0 } else { -1.0 };
                assert!((waves[3][point] * over - square).abs() < 0.03, "square at {phase}: {}", waves[3][point]);
                // A quarter high, three quarters low, about nothing.
                let pulse = if phase < 0.25 { 1.5 } else { -0.5 };
                assert!((waves[4][point] * (0.5 + over) - pulse).abs() < 0.04, "pulse at {phase}: {}", waves[4][point]);
            }
        }
        // Every one of them starts at nothing, going up.
        for wave in &waves[..4] {
            assert!(wave[0].abs() < 1e-3 && wave[1] > 0.0);
        }
    }

    #[test]
    fn between_key_frames_of_one_shape_the_numbers_move() {
        // A pulse from a half to a tenth: the frame half way is a pulse of
        // three tenths, not a mix of the two.
        let table =
            recipe(r#"{ "size": 3, "frames": [ {"shape":"pulse","width":0.5}, {"shape":"pulse","width":0.1} ] }"#);
        let frames = table.harmonics();
        assert_eq!(frames.len(), 3);
        let middle = wave(&frames[1], 1000);
        let high = middle.iter().filter(|sample| **sample > 0.0).count();
        assert!((290..=310).contains(&high), "{high} of 1000 points are high");
        // Between two shapes the frame half way is half of each wave.
        let table =
            recipe(r#"{ "size": 3, "normalize": "table", "frames": [ {"shape":"sine"}, {"shape":"square"} ] }"#);
        let frames = table.harmonics();
        let mixed = frames[0].mixed(&frames[2], 0.5);
        for h in 1..=64 {
            assert!((frames[1].amplitude(h) - mixed.amplitude(h)).abs() < 1e-6, "harmonic {h}");
        }
        // A cutoff moves as a ratio: from the first harmonic to the
        // sixty-fourth, half way is the eighth.
        let sweep = recipe(
            r#"{ "size": 3, "frames": [ {"shape":"spectrum","tilt":-6,"cutoff":1}, {"shape":"spectrum","tilt":-6,"cutoff":64} ] }"#,
        )
        .harmonics();
        let level = |frame: &Harmonics, h: usize| 20.0 * (frame.amplitude(h) / frame.amplitude(1)).log10();
        assert!((level(&sweep[1], 8) + 18.0).abs() < 0.1, "{}", level(&sweep[1], 8));
        assert!(
            (level(&sweep[1], 16) + 24.0 + 24.0).abs() < 0.1,
            "an octave over the cutoff: {}",
            level(&sweep[1], 16)
        );
    }

    #[test]
    fn how_many_frames_a_recipe_makes() {
        let frame = r#"{"shape":"saw"}"#;
        assert_eq!(recipe(&format!(r#"{{ "frames": [{frame}] }}"#)).harmonics().len(), 1);
        assert_eq!(recipe(&format!(r#"{{ "frames": [{frame}, {frame}] }}"#)).harmonics().len(), 32);
        assert_eq!(recipe(&format!(r#"{{ "size": 7, "frames": [{frame}, {frame}] }}"#)).harmonics().len(), 7);
        let many = vec![frame; 40].join(",");
        assert_eq!(recipe(&format!(r#"{{ "frames": [{many}] }}"#)).harmonics().len(), 40);
        // Fewer frames than key frames: the first and the last are kept.
        let keys = r#"{"harmonics":[1]}, {"harmonics":[0,1]}, {"harmonics":[0,0,1]}"#;
        let two = recipe(&format!(r#"{{ "size": 2, "frames": [{keys}] }}"#)).harmonics();
        assert!(two[0].amplitude(1) > 0.99 && two[1].amplitude(3) > 0.99);
    }

    #[test]
    fn harmonics_are_taken_as_written_and_shaped_by_what_any_frame_takes() {
        let frames = recipe(
            r#"{ "normalize": "table", "frames": [
                 {"harmonics": [1, 0, 0.5], "phases": [0, 0, 0.25]},
                 {"shape": "spectrum", "tilt": -6, "even": 0, "cutoff": 8, "peak": 12} ], "size": 2 }"#,
        )
        .harmonics();
        // Against the first harmonic, which normalizing leaves as it is.
        let first = frames[0].amplitude(1);
        assert!((frames[0].amplitude(3) / first - 0.5).abs() < 1e-6);
        assert!(frames[0].cos[2].abs() > 0.1 && frames[0].sin[2].abs() < 1e-6, "a quarter of a cycle on is a cosine");
        assert_eq!(frames[0].amplitude(2), 0.0);
        let shaped = &frames[1];
        let level = |h: usize| 20.0 * (shaped.amplitude(h) / shaped.amplitude(1)).log10();
        assert_eq!(shaped.amplitude(2), 0.0, "no even harmonics");
        // The seventh harmonic: a sawtooth's 17 dB down, and most of the
        // resonance's 12 dB up at the eighth.
        let wanted = -16.9 + 12.0 * (-((7.0f32 / 8.0).log2() * 3.0).powi(2)).exp();
        assert!((level(7) - wanted).abs() < 0.2, "{}", level(7));
        assert!(level(33) < level(17) - 20.0, "24 dB an octave over the cutoff");
    }

    #[test]
    fn a_recipe_that_is_wrong_says_what_would_be_right() {
        let wrong = |json: &str| recipe(json).check().unwrap_err();
        assert!(wrong(r#"{ "frames": [] }"#).contains("at least one frame"));
        let error = wrong(r#"{ "frames": [ {"shape":"saw"}, {"shape":"saw","width":0.2} ] }"#);
        assert!(error.contains("key frame 2") && error.contains("`width`") && error.contains("tilt"), "{error}");
        let error = wrong(r#"{ "frames": [ {"shape":"pulse","width":3} ] }"#);
        assert!(error.contains("outside 0.01 to 0.99"), "{error}");
        assert!(wrong(r#"{ "size": 999, "frames": [ {"shape":"saw"} ] }"#).contains("1 to 256"));
        assert!(wrong(r#"{ "frames": [ {"shape":"harmonics"} ] }"#).contains("lists none"));
        // A word that is no shape, or no part of a frame, is refused as it is read.
        let error =
            serde_json::from_str::<Recipe>(r#"{ "frames": [ {"shape":"sawtooth"} ] }"#).unwrap_err().to_string();
        assert!(error.contains("sawtooth") && error.contains("saw"), "{error}");
        assert!(serde_json::from_str::<Recipe>(r#"{ "frames": [ {"shape":"saw","wdith":1} ] }"#).is_err());
        // What is right checks, and is written back as it was read.
        let right = recipe(
            r#"{ "name": "Reed", "size": 8, "frames": [ {"shape":"pulse","width":0.5}, {"shape":"fm","ratio":2,"index":3} ] }"#,
        );
        assert_eq!(right.check(), Ok(()));
        assert_eq!(recipe(&serde_json::to_string(&right).unwrap()), right);
    }

    #[test]
    fn the_drawn_shapes_are_what_they_say() {
        let frames = recipe(
            r#"{ "size": 4, "normalize": "table", "frames": [
                 {"shape":"fm","ratio":2,"index":0}, {"shape":"fold","amount":0},
                 {"shape":"formant","at":12,"width":0.25}, {"shape":"sync","ratio":4} ] }"#,
        );
        assert_eq!(frames.check(), Ok(()));
        let frames = frames.harmonics();
        // No bend and no fold: a sine (a little under full level, which the
        // loudest of the four frames has).
        for frame in &frames[..2] {
            assert!(frame.amplitude(1) > 0.97 && frame.amplitude(2) < 1e-3 && frame.amplitude(3) < 1e-3);
        }
        // A formant at the twelfth harmonic, some four harmonics wide.
        let strongest =
            |frame: &Harmonics| (1..=64).max_by(|a, b| frame.amplitude(*a).total_cmp(&frame.amplitude(*b))).unwrap();
        assert_eq!(strongest(&frames[2]), 12);
        assert!(frames[2].amplitude(24) < frames[2].amplitude(12) * 0.02);
        // A sine restarted four times in the cycle is mostly its fourth harmonic.
        assert_eq!(strongest(&frames[3]), 4);
        // A lot drawn twice is the same lot, and another seed another.
        let lots = recipe(r#"{ "size": 3, "frames": [ {"shape":"noise","seed":7}, {"shape":"noise","seed":7}, {"shape":"noise","seed":8} ] }"#)
            .harmonics();
        assert_eq!(lots[0], lots[1]);
        assert!(lots[0] != lots[2]);
    }

    fn buffer(samples: Vec<f32>, sample_rate: f64) -> AudioBuffer {
        AudioBuffer { sample_rate, left: samples.clone(), right: samples }
    }

    #[test]
    fn a_file_of_single_cycles_is_a_frame_for_each() {
        // Three cycles of 2048: a sine, its second harmonic, its third.
        let mut samples = Vec::new();
        for h in 1..=3 {
            samples.extend((0..FILE_CYCLE).map(|n| (TAU * h as f64 * n as f64 / FILE_CYCLE as f64).sin() as f32 * 0.5));
        }
        let spec = FileSpec { file: "cycles.wav".into(), ..FileSpec::default() };
        let (frames, reading) = spec.harmonics(&buffer(samples.clone(), 44_100.0)).unwrap();
        assert_eq!((frames.len(), reading), (3, Reading::Cycles(2048)));
        for (index, frame) in frames.iter().enumerate() {
            assert!((frame.amplitude(index + 1) - 1.0).abs() < 1e-3, "made to reach 1");
            assert!(frame.amplitude(((index + 1) % 3) + 1) < 1e-4);
        }
        // Cycles of a length that is no power of two, said by the caller.
        let odd: Vec<f32> = (0..600 * 2).map(|n| (TAU * 2.0 * n as f64 / 600.0).sin() as f32).collect();
        let spec = FileSpec { file: "odd.wav".into(), cycle: Some(600), ..FileSpec::default() };
        let (frames, _) = spec.harmonics(&buffer(odd, 48_000.0)).unwrap();
        assert!(frames.len() == 2 && (frames[0].amplitude(2) - 1.0).abs() < 1e-3 && frames[0].amplitude(1) < 1e-4);
        // What cannot be a table says why.
        let spec = FileSpec { file: "x.wav".into(), cycle: Some(4096), ..FileSpec::default() };
        assert!(spec.harmonics(&buffer(vec![0.5; 100], 48_000.0)).unwrap_err().contains("less than one cycle"));
        assert!(spec.harmonics(&buffer(vec![0.0; 8192], 48_000.0)).unwrap_err().contains("silent"));
        let both = FileSpec { cycle: Some(2048), root: Some(60.0), ..spec };
        assert!(both.check().unwrap_err().contains("one of the two"));
    }

    #[test]
    fn a_recording_of_a_note_is_read_as_its_harmonics_along_its_length() {
        // A note of 220.5 Hz whose third harmonic comes in over two seconds.
        let sample_rate = 44_100.0;
        let frames_of_audio = 2 * 44_100;
        let samples: Vec<f32> = (0..frames_of_audio)
            .map(|n| {
                let turn = TAU * 220.5 * n as f64 / sample_rate;
                let third = n as f64 / frames_of_audio as f64;
                (0.5 * turn.sin() + 0.5 * third * (3.0 * turn + 1.0).sin()) as f32
            })
            .collect();
        let hz = pitch_of(&samples, sample_rate).unwrap();
        assert!((hz - 220.5).abs() < 0.3, "{hz} Hz");
        let spec = FileSpec { file: "note.wav".into(), frames: Some(9), ..FileSpec::default() };
        let (frames, reading) = spec.harmonics(&buffer(samples.clone(), sample_rate)).unwrap();
        assert!(matches!(reading, Reading::Recording(hz) if (hz - 220.5).abs() < 0.3), "{reading:?}");
        assert_eq!(frames.len(), 9);
        let third = |frame: &Harmonics| frame.amplitude(3) / frame.amplitude(1);
        assert!(third(&frames[0]) < 0.03 && (third(&frames[4]) - 0.5).abs() < 0.03 && third(&frames[8]) > 0.95);
        assert!(frames[4].amplitude(2) < 0.01 && frames[4].amplitude(5) < 0.01);
        // Told the note, it does not have to find it: A3 is 220 Hz.
        let told = FileSpec { root: Some(57.0), ..spec.clone() };
        assert!(
            matches!(told.harmonics(&buffer(samples, sample_rate)).unwrap().1, Reading::Recording(hz) if hz == 220.0)
        );
        // Noise has no pitch to find, and says what to say instead.
        let mut lot = Rng::new(3);
        let noise: Vec<f32> = (0..44_100).map(|_| lot.bipolar()).collect();
        let error = spec.harmonics(&buffer(noise, sample_rate)).unwrap_err();
        assert!(error.contains("`root`") && error.contains("`cycle`"), "{error}");
    }
}
