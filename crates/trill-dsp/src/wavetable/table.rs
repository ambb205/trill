//! A wavetable as an oscillator reads it: frames of one cycle each, every
//! frame kept at as many brightnesses as a keyboard needs.
//!
//! A frame is its harmonics. A note may only be given the harmonics that
//! lie under half the sample rate, or the rest come back as tones that do
//! not belong to it, so each frame is kept as a row of cycles with fewer
//! and fewer harmonics, three to the octave: the brightest has
//! [`MAX_HARMONICS`], the dullest the fundamental alone. An oscillator
//! reads the brightest one its pitch allows, which always reaches to
//! within a third of an octave of half the sample rate (19 kHz at
//! 48 kHz).
//!
//! A cycle is stored at least eight times longer than its highest
//! harmonic needs (sixteen times where it has few harmonics, each of
//! which is then a larger share of the note), and read between its
//! samples with four points, so that what the reading adds lies far under
//! the note (84 dB under a sawtooth at the worst pitch) and what it takes
//! off the highest harmonics is less than a tenth of a dB.
//!
//! Everything here is made off the audio thread. Reading allocates
//! nothing.

use crate::fft::RealFft;

/// The harmonics a frame can hold: enough for a note of 47 Hz (F#1) to
/// reach 24 kHz, and for the lowest E of a bass to reach 21 kHz.
pub const MAX_HARMONICS: usize = 512;
/// The most frames a table can have.
pub const MAX_FRAMES: usize = 256;
/// How many brightnesses a frame is kept at for every octave of pitch.
const PER_OCTAVE: usize = 3;
/// From [`MAX_HARMONICS`] down to one harmonic.
const LEVELS: usize = 9 * PER_OCTAVE + 1;
/// The longest a stored cycle is, and the shortest.
const LONGEST: usize = 4096;
const SHORTEST: usize = 256;
/// Samples stored around a cycle so that four points can be read at any
/// phase without wrapping: one before it and three after.
const GUARD: usize = 4;
/// The share of half the sample rate a note's highest harmonic may reach.
const HEADROOM: f32 = 0.98;

/// One frame as the amplitudes of its harmonics: the wave is the sum of
/// `sin[h - 1] * sin(2 pi h t) + cos[h - 1] * cos(2 pi h t)` over the
/// harmonics `h` from 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Harmonics {
    pub sin: Vec<f32>,
    pub cos: Vec<f32>,
}

impl Harmonics {
    pub fn silent() -> Self {
        Self { sin: vec![0.0; MAX_HARMONICS], cos: vec![0.0; MAX_HARMONICS] }
    }

    /// The amplitude of harmonic `h`, from 1.
    pub fn amplitude(&self, h: usize) -> f32 {
        (self.sin[h - 1] * self.sin[h - 1] + self.cos[h - 1] * self.cos[h - 1]).sqrt()
    }

    pub fn scale(&mut self, gain: f32) {
        for value in self.sin.iter_mut().chain(self.cos.iter_mut()) {
            *value *= gain;
        }
    }

    /// `self * (1 - t) + other * t`: what a reader half way between two
    /// frames hears.
    pub fn mixed(&self, other: &Harmonics, t: f32) -> Harmonics {
        let mix = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| a + (b - a) * t).collect();
        Harmonics { sin: mix(&self.sin, &other.sin), cos: mix(&self.cos, &other.cos) }
    }
}

/// What a frame is like, in numbers a model can choose by.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FrameInfo {
    /// Where the weight of the spectrum lies, as a harmonic number: 1 for
    /// a sine, about 6 for a sawtooth's first fifty harmonics.
    pub centre: f32,
    /// The highest harmonic within 40 dB of the strongest.
    pub highest: u16,
    /// The share of the power that is in the odd harmonics: 1 for a square
    /// wave, about 0.8 for a sawtooth.
    pub odd: f32,
    /// How loud the frame is against its own peak, in dB: -3 for a sine,
    /// -1.4 for a square, -6 for a sawtooth, lower the spikier the wave.
    pub rms_db: f32,
}

/// One brightness of a table.
#[derive(Copy, Clone, Debug)]
struct Level {
    /// Where its first frame begins in the table's samples.
    offset: usize,
    /// The samples of one cycle.
    len: usize,
}

/// A level of a table, to read from. Copied out once per stretch of
/// frames, so the reading itself touches nothing else.
#[derive(Copy, Clone)]
pub struct Reader<'a> {
    data: &'a [f32],
    offset: usize,
    stride: usize,
    len: f32,
    /// The cycle's length as a power of two.
    bits: u32,
    /// The last frame a pair can start on.
    last: usize,
    /// Frames, less one: what a position of 1 is.
    span: f32,
}

impl Reader<'_> {
    /// The two frames a position between 0 and 1 lies between: where the
    /// first begins in the samples, and how far towards the second it is.
    #[inline(always)]
    pub fn pair(&self, position: f32) -> (usize, f32) {
        let at = position.clamp(0.0, 1.0) * self.span;
        let frame = (at as usize).min(self.last);
        (self.offset + frame * self.stride, at - frame as f32)
    }

    /// The wave at `phase` of the pair of frames at `pair`, `mix` of the
    /// way from the first to the second: `phase` is the cycle as the whole
    /// range of a number, which is how an oscillator counts it, so that
    /// counting adds nothing of its own to the sound.
    #[inline(always)]
    pub fn read_counted(&self, pair: usize, mix: f32, phase: u32) -> f32 {
        let index = (phase >> (32 - self.bits)) as usize;
        let x = ((phase << self.bits) >> 8) as f32 * (1.0 / 16_777_216.0);
        self.read_between(pair, mix, index, x)
    }

    /// The wave at `phase` (0..=1) of the pair of frames at `pair`, `mix`
    /// of the way from the first to the second.
    #[inline(always)]
    pub fn read(&self, pair: usize, mix: f32, phase: f32) -> f32 {
        let at = phase * self.len;
        let index = at as usize;
        self.read_between(pair, mix, index, at - index as f32)
    }

    #[inline(always)]
    fn read_between(&self, pair: usize, mix: f32, index: usize, x: f32) -> f32 {
        let a = &self.data[pair + index..pair + index + 4];
        let b = &self.data[pair + self.stride + index..pair + self.stride + index + 4];
        // Four points, third order: the curve through all four.
        let (below, above, beyond) = (x + 1.0, x - 1.0, x - 2.0);
        let w0 = -x * above * beyond * (1.0 / 6.0);
        let w1 = below * above * beyond * 0.5;
        let w2 = -below * x * beyond * 0.5;
        let w3 = below * x * above * (1.0 / 6.0);
        let first = a[0] * w0 + a[1] * w1 + a[2] * w2 + a[3] * w3;
        let second = b[0] * w0 + b[1] * w1 + b[2] * w2 + b[3] * w3;
        first + (second - first) * mix
    }
}

/// A wavetable, ready to be played.
pub struct Table {
    name: String,
    frames: usize,
    /// At least two, so that there is always a pair to read between.
    stored: usize,
    /// By how bright: the level a pitch may read is [`Table::level_for`].
    levels: [Level; LEVELS],
    data: Vec<f32>,
    info: Vec<FrameInfo>,
}

/// The harmonics of level `index`: [`MAX_HARMONICS`] at 0, a third of an
/// octave fewer with each step.
fn harmonics_of(index: usize) -> usize {
    ((MAX_HARMONICS as f64) * 2f64.powf(-(index as f64) / PER_OCTAVE as f64)).floor().max(1.0) as usize
}

/// How long a cycle of `harmonics` harmonics is stored: eight times as
/// many samples as harmonics, and sixteen where the harmonics are few.
fn cycle_of(harmonics: usize) -> usize {
    let times = if harmonics <= 64 { 16 } else { 8 };
    (harmonics * times).next_power_of_two().clamp(SHORTEST, LONGEST)
}

impl Table {
    /// The table of `frames`, each as its harmonics. An empty list is a
    /// table of one silent frame.
    pub fn new(name: impl Into<String>, frames: &[Harmonics]) -> Self {
        let silent = [Harmonics::silent()];
        let frames = if frames.is_empty() { &silent[..] } else { &frames[..frames.len().min(MAX_FRAMES)] };
        let stored = frames.len().max(2);
        let frame = |index: usize| &frames[index.min(frames.len() - 1)];

        let mut levels = [Level { offset: 0, len: SHORTEST }; LEVELS];
        let mut data: Vec<f32> = Vec::new();
        let mut plans: Vec<RealFft> = Vec::new();
        let mut made: Option<(usize, Level)> = None;
        for (index, level) in levels.iter_mut().enumerate() {
            let harmonics = harmonics_of(index);
            // The dullest levels round to the same number of harmonics.
            if let Some((same, built)) = made
                && same == harmonics
            {
                *level = built;
                continue;
            }
            let len = cycle_of(harmonics);
            *level = Level { offset: data.len(), len };
            made = Some((harmonics, *level));
            if !plans.iter().any(|plan| plan.size() == len) {
                plans.push(RealFft::new(len));
            }
            let plan = plans.iter_mut().find(|plan| plan.size() == len).expect("made above");
            let (mut re, mut im, mut wave) = (vec![0.0f32; len / 2 + 1], vec![0.0f32; len / 2 + 1], vec![0.0f32; len]);
            for stored_frame in 0..stored {
                let source = frame(stored_frame);
                re.fill(0.0);
                im.fill(0.0);
                let half = len as f32 * 0.5;
                for h in 1..=harmonics.min(len / 2 - 1) {
                    re[h] = source.cos[h - 1] * half;
                    im[h] = -source.sin[h - 1] * half;
                }
                plan.inverse(&re, &im, &mut wave);
                data.push(wave[len - 1]);
                data.extend_from_slice(&wave);
                data.extend_from_slice(&wave[..GUARD - 1]);
            }
        }
        let info = frames.iter().map(describe).collect();
        Self { name: name.into(), frames: frames.len(), stored, levels, data, info }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// What each frame is like.
    pub fn info(&self) -> &[FrameInfo] {
        &self.info
    }

    /// The memory the table's samples take, in bytes.
    pub fn bytes(&self) -> usize {
        self.data.len() * std::mem::size_of::<f32>()
    }

    /// The brightest level a note may read whose phase moves on by
    /// `increment` cycles a frame, at its fastest: none of that level's
    /// harmonics lies over half the sample rate.
    #[inline]
    pub fn level_for(increment: f32) -> usize {
        let allowed = HEADROOM * 0.5 / increment.max(1e-9);
        if allowed >= MAX_HARMONICS as f32 {
            return 0;
        }
        let steps = (PER_OCTAVE as f32 * (MAX_HARMONICS as f32 / allowed).log2()).ceil();
        let mut index = (steps.max(0.0) as usize).min(LEVELS - 1);
        // The levels' harmonics are rounded down, which this is not: the
        // level before may fit after all.
        while index + 1 < LEVELS && harmonics_of(index) as f32 > allowed {
            index += 1;
        }
        if index > 0 && harmonics_of(index - 1) as f32 <= allowed {
            index -= 1;
        }
        index
    }

    /// Level `index`, to read from.
    #[inline]
    pub fn reader(&self, index: usize) -> Reader<'_> {
        let level = self.levels[index.min(LEVELS - 1)];
        Reader {
            data: &self.data,
            offset: level.offset,
            stride: level.len + GUARD,
            len: level.len as f32,
            bits: level.len.trailing_zeros(),
            last: self.stored - 2,
            span: (self.frames - 1) as f32,
        }
    }

    /// The wave at `position` (0..=1) of the table, as `points` samples of
    /// one cycle with every harmonic it has: for drawing it.
    pub fn outline(&self, position: f32, points: usize) -> Vec<f32> {
        let reader = self.reader(0);
        let (pair, mix) = reader.pair(position);
        (0..points).map(|point| reader.read(pair, mix, point as f32 / points.max(1) as f32)).collect()
    }
}

/// The largest sample of the wave `harmonics` add up to, measured on a
/// cycle of 4096 points.
pub fn peak_of(harmonics: &Harmonics) -> f32 {
    const POINTS: usize = 4096;
    let mut plan = RealFft::new(POINTS);
    let (mut re, mut im, mut wave) = (vec![0.0f32; POINTS / 2 + 1], vec![0.0f32; POINTS / 2 + 1], vec![0.0f32; POINTS]);
    let half = POINTS as f32 * 0.5;
    for h in 1..=MAX_HARMONICS {
        re[h] = harmonics.cos[h - 1] * half;
        im[h] = -harmonics.sin[h - 1] * half;
    }
    plan.inverse(&re, &im, &mut wave);
    wave.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

fn describe(frame: &Harmonics) -> FrameInfo {
    let powers: Vec<f64> = (1..=MAX_HARMONICS).map(|h| f64::from(frame.amplitude(h)).powi(2)).collect();
    let total: f64 = powers.iter().sum();
    if total <= 0.0 {
        return FrameInfo { centre: 0.0, highest: 0, odd: 0.0, rms_db: -120.0 };
    }
    let weighted: f64 = powers.iter().enumerate().map(|(index, power)| (index + 1) as f64 * power).sum();
    let strongest = powers.iter().fold(0.0f64, |most, power| most.max(*power));
    let highest = powers.iter().rposition(|power| *power >= strongest * 1e-4).map_or(0, |index| index + 1);
    let odd: f64 = powers.iter().step_by(2).sum();
    let peak = f64::from(peak_of(frame)).max(1e-12);
    FrameInfo {
        centre: (weighted / total) as f32,
        highest: highest as u16,
        odd: (odd / total) as f32,
        rms_db: (10.0 * (total * 0.5 / (peak * peak)).log10()) as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saw() -> Harmonics {
        let mut frame = Harmonics::silent();
        for h in 1..=MAX_HARMONICS {
            frame.sin[h - 1] = if h % 2 == 1 { 1.0 } else { -1.0 } * 2.0 / (std::f32::consts::PI * h as f32);
        }
        frame
    }

    fn sine() -> Harmonics {
        let mut frame = Harmonics::silent();
        frame.sin[0] = 1.0;
        frame
    }

    #[test]
    fn a_level_holds_only_the_harmonics_its_pitch_allows() {
        // Three levels to the octave, the dullest the fundamental alone.
        assert_eq!((harmonics_of(0), harmonics_of(3), harmonics_of(6), harmonics_of(LEVELS - 1)), (512, 256, 128, 1));
        for index in 1..LEVELS {
            assert!(harmonics_of(index) <= harmonics_of(index - 1));
        }
        // Whatever the pitch, the level it reads has no harmonic over half
        // the sample rate, and the next brighter one would have.
        for note in 0..136 {
            let hz = 440.0 * 2f32.powf((note as f32 - 69.0) / 12.0);
            let increment = hz / 48_000.0;
            let level = Table::level_for(increment);
            let top = harmonics_of(level) as f32 * hz;
            assert!(top <= 24_000.0, "note {note}: its level reaches {top} Hz");
            if level > 0 && harmonics_of(level - 1) > harmonics_of(level) {
                assert!(
                    harmonics_of(level - 1) as f32 * hz > 24_000.0 * HEADROOM - 1.0,
                    "note {note} could be brighter"
                );
            }
            // It reaches within a third of an octave of the top, where the
            // frame has that many harmonics to give and a harmonic more or
            // less is not a third of an octave itself.
            if level > 0 && harmonics_of(level) >= 8 {
                assert!(top > 24_000.0 * HEADROOM / 1.34, "note {note}: only {top} Hz");
            }
        }
        assert_eq!(Table::level_for(0.0), 0);
        assert_eq!(Table::level_for(0.7), LEVELS - 1);
    }

    #[test]
    fn a_frame_is_read_as_the_wave_its_harmonics_add_up_to() {
        let table = Table::new("t", &[sine(), saw()]);
        assert_eq!((table.frames(), table.name()), (2, "t"));
        let reader = table.reader(0);
        // The first frame is a sine, the second a sawtooth through zero at
        // phase 0, and half way between is half of each.
        for step in 0..997 {
            let phase = step as f32 / 997.0;
            let sine = (std::f32::consts::TAU * phase).sin();
            let (pair, mix) = reader.pair(0.0);
            assert!((reader.read(pair, mix, phase) - sine).abs() < 1e-4, "at {phase}");
            let (pair, mix) = reader.pair(1.0);
            let saw = reader.read(pair, mix, phase);
            if (phase - 0.5).abs() > 0.02 {
                let ramp = if phase < 0.5 { 2.0 * phase } else { 2.0 * phase - 2.0 };
                assert!((saw - ramp).abs() < 0.02, "at {phase}: {saw} against {ramp}");
            }
            let (pair, mix) = reader.pair(0.5);
            assert!((reader.read(pair, mix, phase) - 0.5 * (sine + saw)).abs() < 1e-4);
        }
        // Counted as an oscillator counts it, it is the same wave.
        let (pair, mix) = reader.pair(0.5);
        for step in 0..997u32 {
            let counted = step.wrapping_mul(4_307_891);
            let phase = counted as f32 / 4_294_967_296.0;
            assert!((reader.read_counted(pair, mix, counted) - reader.read(pair, mix, phase)).abs() < 2e-3);
        }
        // The very end of a cycle is its beginning.
        let (pair, mix) = reader.pair(1.0);
        assert!((reader.read(pair, mix, 1.0) - reader.read(pair, mix, 0.0)).abs() < 1e-5);
        // A table of one frame is that frame wherever it is read.
        let one = Table::new("one", &[saw()]);
        let reader = one.reader(0);
        let (first, last) = (reader.pair(0.0), reader.pair(1.0));
        assert!((reader.read(first.0, first.1, 0.3) - reader.read(last.0, last.1, 0.3)).abs() < 1e-6);
        // A dull level of the sawtooth is its fundamental alone.
        let dull = table.reader(LEVELS - 1);
        let (pair, mix) = dull.pair(1.0);
        let fundamental = 2.0 / std::f32::consts::PI;
        assert!((dull.read(pair, mix, 0.25) - fundamental).abs() < 1e-4);
    }

    #[test]
    fn what_a_frame_is_like_is_said_in_numbers() {
        let table = Table::new("t", &[sine(), saw()]);
        let [sine, saw] = [table.info()[0], table.info()[1]];
        assert!((sine.centre - 1.0).abs() < 1e-3 && sine.highest == 1 && (sine.odd - 1.0).abs() < 1e-6);
        assert!((sine.rms_db + 3.01).abs() < 0.05, "{}", sine.rms_db);
        // A sawtooth: its harmonics fall to a hundredth by the hundredth,
        // three quarters of its power is in the odd ones, and it is 6.2 dB
        // under its peak (4.8 dB under its ramp, and the peak is the
        // overshoot beside its edge, 18 % over that).
        assert!((90..=110).contains(&saw.highest), "{}", saw.highest);
        assert!((saw.odd - 0.75).abs() < 0.01, "{}", saw.odd);
        assert!((saw.rms_db + 6.2).abs() < 0.2, "{}", saw.rms_db);
        assert_eq!(Table::new("empty", &[]).info()[0].highest, 0);
    }

    #[test]
    fn a_table_takes_the_memory_it_is_said_to() {
        let table = Table::new("t", &vec![saw(); 4]);
        // Some 120 kB a frame.
        assert!((105_000..130_000).contains(&(table.bytes() / 4)), "{} bytes a frame", table.bytes() / 4);
    }
}
