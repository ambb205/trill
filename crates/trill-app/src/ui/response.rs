//! The curves the device displays draw, as pure functions of parameter
//! values: filter and equalizer responses, envelope shapes, transfer
//! curves.
//!
//! These follow the analog prototypes of the filters in `trill-dsp` (the same
//! Q for a given resonance, the same shelf slope), which the digital
//! filters match across the audible range except right under Nyquist. A
//! display is a picture of what a control does, not a measurement.

/// What a two-pole filter stage passes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FilterShape {
    LowPass,
    /// Two stages in series: 24 dB per octave.
    LowPass24,
    HighPass,
    BandPass,
    Notch,
}

/// Q of the built-in filters for a resonance of 0..=1.
pub fn resonance_q(resonance: f64) -> f64 {
    0.5 * 40f64.powf(resonance.clamp(0.0, 1.0))
}

/// Linear gain of a filter at `hz`.
pub fn filter_gain(shape: FilterShape, cutoff_hz: f64, resonance: f64, hz: f64) -> f64 {
    let w = hz / cutoff_hz.max(1.0);
    let q = resonance_q(resonance);
    // |1 - w^2 + j w/Q|, the denominator every second-order shape shares.
    let denominator = |q: f64| ((1.0 - w * w).powi(2) + (w / q).powi(2)).sqrt().max(1e-9);
    match shape {
        FilterShape::LowPass => 1.0 / denominator(q),
        // The second stage is fixed and gentle, as in the synth.
        FilterShape::LowPass24 => 1.0 / (denominator(q) * denominator(0.6)),
        FilterShape::HighPass => w * w / denominator(q),
        FilterShape::BandPass => (w / q) / denominator(q),
        FilterShape::Notch => (1.0 - w * w).abs() / denominator(q),
    }
}

/// Linear gain of the filter device at its 24 dB slope: two stages in a
/// row, each with 0.8 times the root of the Q the resonance stands for.
pub fn steep_filter_gain(shape: FilterShape, cutoff_hz: f64, resonance: f64, hz: f64) -> f64 {
    let w = hz / cutoff_hz.max(1.0);
    let q = 0.8 * resonance_q(resonance).sqrt();
    let denominator = ((1.0 - w * w).powi(2) + (w / q).powi(2)).sqrt().max(1e-9);
    let stage = match shape {
        FilterShape::LowPass | FilterShape::LowPass24 => 1.0 / denominator,
        FilterShape::HighPass => w * w / denominator,
        // As the device has it: the band output is not brought to unity.
        FilterShape::BandPass => (w / q) / denominator * q,
        FilterShape::Notch => (1.0 - w * w).abs() / denominator,
    };
    // The one-stage band-pass is drawn at unity on its center; so is this.
    let stage = if shape == FilterShape::BandPass { stage / q } else { stage };
    stage * stage
}

/// Linear gain to decibels, floored so silence stays finite.
pub fn to_db(gain: f64) -> f64 {
    20.0 * gain.max(1e-6).log10()
}

/// The six bands of the built-in equalizer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EqBands {
    /// High-pass corner; at or below 12 Hz the band is off.
    pub low_cut: f64,
    /// How many biquads each cut is made of: 1, 2 or 4, for 12, 24 or
    /// 48 dB an octave.
    pub low_cut_stages: usize,
    /// Frequency, gain in dB, Q.
    pub low_shelf: (f64, f64, f64),
    pub mid1: (f64, f64, f64),
    pub mid2: (f64, f64, f64),
    pub high_shelf: (f64, f64, f64),
    /// Low-pass corner; at or above 19.9 kHz the band is off.
    pub high_cut: f64,
    pub high_cut_stages: usize,
}

/// How many biquads the slope choice `index` of a cut is made of.
pub fn cut_stages(index: f64) -> usize {
    [1, 2, 4][(index.round().max(0.0) as usize).min(2)]
}

/// Magnitude of `(n2 s^2 + n1 s + n0) / (d2 s^2 + d1 s + d0)` at `s = jw`.
fn biquad_magnitude(w: f64, numerator: [f64; 3], denominator: [f64; 3]) -> f64 {
    let magnitude = |[c2, c1, c0]: [f64; 3]| (c0 - c2 * w * w).hypot(c1 * w);
    magnitude(numerator) / magnitude(denominator).max(1e-12)
}

fn peak_gain(freq: f64, gain_db: f64, q: f64, hz: f64) -> f64 {
    let a = 10f64.powf(gain_db / 40.0);
    let q = q.max(0.05);
    biquad_magnitude(hz / freq, [1.0, a / q, 1.0], [1.0, 1.0 / (a * q), 1.0])
}

fn low_shelf_gain(freq: f64, gain_db: f64, q: f64, hz: f64) -> f64 {
    let a = 10f64.powf(gain_db / 40.0);
    let b = a.sqrt() / q.max(0.05);
    a * biquad_magnitude(hz / freq, [1.0, b, a], [a, b, 1.0])
}

fn high_shelf_gain(freq: f64, gain_db: f64, q: f64, hz: f64) -> f64 {
    let a = 10f64.powf(gain_db / 40.0);
    let b = a.sqrt() / q.max(0.05);
    a * biquad_magnitude(hz / freq, [a, b, 1.0], [1.0, b, a])
}

/// Gain of a Butterworth cut of `stages` biquads at `corner`: a low cut,
/// or a high cut with `high`.
fn cut_gain(stages: usize, corner: f64, hz: f64, high: bool) -> f64 {
    let numerator = if high { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let qs = trill_dsp::util::butterworth_qs(stages);
    let stage = |q: &f32| biquad_magnitude(hz / corner, numerator, [1.0, 1.0 / f64::from(*q), 1.0]);
    qs.iter().take(stages.clamp(1, 4)).map(stage).product()
}

/// Gain of the whole equalizer at `hz`, in dB.
pub fn eq_gain_db(bands: &EqBands, hz: f64) -> f64 {
    let mut gain = 1.0;
    if bands.low_cut > 12.0 {
        gain *= cut_gain(bands.low_cut_stages, bands.low_cut, hz, false);
    }
    gain *= low_shelf_gain(bands.low_shelf.0, bands.low_shelf.1, bands.low_shelf.2, hz);
    gain *= peak_gain(bands.mid1.0, bands.mid1.1, bands.mid1.2, hz);
    gain *= peak_gain(bands.mid2.0, bands.mid2.1, bands.mid2.2, hz);
    gain *= high_shelf_gain(bands.high_shelf.0, bands.high_shelf.1, bands.high_shelf.2, hz);
    if bands.high_cut < 19_900.0 {
        gain *= cut_gain(bands.high_cut_stages, bands.high_cut, hz, true);
    }
    to_db(gain)
}

/// Output level of the compressor for an input level, both in dB, before
/// make-up gain: the static curve with its soft knee.
pub fn compressor_curve(threshold_db: f64, ratio: f64, knee_db: f64, input_db: f64) -> f64 {
    let slope = 1.0 / ratio.max(1.0) - 1.0;
    let over = input_db - threshold_db;
    let change = if 2.0 * over < -knee_db {
        0.0
    } else if knee_db > 0.0 && 2.0 * over.abs() <= knee_db {
        let into_knee = over + knee_db * 0.5;
        slope * into_knee * into_knee / (2.0 * knee_db)
    } else {
        slope * over
    };
    input_db + change
}

/// The saturator's waveshapers, by index: soft, hard, tube.
pub fn saturate(shape: usize, x: f64) -> f64 {
    match shape {
        0 => x.tanh(),
        1 => x.clamp(-1.0, 1.0),
        // Asymmetric: the positive half saturates sooner than the negative.
        _ if x >= 0.0 => x.tanh(),
        _ => (x * 0.7).tanh() / 0.7,
    }
}

/// Level of the tremolo at `phase` (in cycles) of a cycle that starts at
/// full level, for a shape by index: sine, triangle, square.
pub fn tremolo_level(shape: usize, depth: f64, phase: f64) -> f64 {
    let phase = phase.rem_euclid(1.0);
    let dip = match shape {
        0 => 0.5 - 0.5 * (phase * std::f64::consts::TAU).cos(),
        1 => 1.0 - (2.0 * phase - 1.0).abs(),
        _ if phase < 0.5 => 0.0,
        _ => 1.0,
    };
    1.0 - depth.clamp(0.0, 1.0) * dip
}

/// Gain of the pump effect at `phase` (0..1) through its cycle.
pub fn pump_gain(depth: f64, curve: f64, phase: f64) -> f64 {
    1.0 - depth * (1.0 - phase.clamp(0.0, 1.0)).powf(curve * 2.0)
}

/// Where the four stages of an envelope end, as fractions of a display's
/// width and height: the peak of the attack, the start of the sustain, its
/// end, and the end of the release.
///
/// Times from a millisecond to twenty seconds have to share a few dozen
/// pixels, so a stage's width grows with the logarithm of its time, the
/// sustain is given a fixed stretch, and the whole is scaled to fill the
/// display: what is read from it is the shape and how the stages compare,
/// not seconds. Only an envelope that is short throughout is left narrower,
/// so a pluck still looks like one.
pub fn envelope_points(attack: f64, decay: f64, sustain: f64, release: f64) -> [(f32, f32); 5] {
    /// 0 for an instant stage, 1 for the longest the devices allow.
    fn span(seconds: f64) -> f32 {
        ((1.0 + seconds.max(0.0) * 25.0).ln() / (1.0 + 20.0 * 25.0f64).ln()).clamp(0.0, 1.0) as f32
    }
    const START: f32 = 0.02;
    const END: f32 = 0.98;
    /// Share of the width the sustain is held for.
    const HOLD: f32 = 0.22;
    /// Stages that add up to less than this do not fill the display.
    const SHORT: f32 = 0.5;
    let stages = [span(attack), span(decay), span(release)];
    let scale = (END - START - HOLD) / stages.iter().sum::<f32>().max(SHORT);
    let level = sustain.clamp(0.0, 1.0) as f32;
    let peak = START + scale * stages[0];
    let settled = peak + scale * stages[1];
    let released = settled + HOLD;
    let end = released + scale * stages[2];
    [(START, 0.0), (peak, 1.0), (settled, level), (released, level), (end, 0.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn filters_pass_and_reject_what_their_names_say() {
        use FilterShape::*;
        let db = |shape, hz| to_db(filter_gain(shape, 1000.0, 0.1, hz));
        assert!(close(db(LowPass, 20.0), 0.0, 0.1));
        assert!(
            close(db(LowPass, 16_000.0), -48.0, 1.0),
            "12 dB per octave, four octaves up: {}",
            db(LowPass, 16_000.0)
        );
        assert!(close(db(LowPass24, 16_000.0), -96.0, 2.0), "twice as steep: {}", db(LowPass24, 16_000.0));
        assert!(close(db(HighPass, 20_000.0), 0.0, 0.1));
        assert!(db(HighPass, 62.5) < -45.0);
        assert!(close(db(BandPass, 1000.0), 0.0, 0.01), "a band-pass is at unity on its center");
        assert!(db(BandPass, 20.0) < -20.0 && db(BandPass, 20_000.0) < -20.0);
        assert!(db(Notch, 1000.0) < -100.0);
        assert!(close(db(Notch, 20.0), 0.0, 0.1));
    }

    #[test]
    fn resonance_peaks_at_the_cutoff_by_its_q() {
        assert!(close(resonance_q(0.0), 0.5, 1e-9));
        assert!(close(resonance_q(1.0), 20.0, 1e-9));
        // At the cutoff a two-pole low-pass has a gain of exactly Q.
        for resonance in [0.0, 0.3, 0.78, 1.0] {
            let at_cutoff = filter_gain(FilterShape::LowPass, 440.0, resonance, 440.0);
            assert!(close(at_cutoff, resonance_q(resonance), 1e-9), "{resonance}: {at_cutoff}");
        }
    }

    /// The Q of a plain shelf, and of a cut of 12 dB an octave.
    const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

    fn flat() -> EqBands {
        EqBands {
            low_cut: 10.0,
            low_cut_stages: 1,
            low_shelf: (100.0, 0.0, BUTTERWORTH_Q),
            mid1: (500.0, 0.0, 1.0),
            mid2: (3000.0, 0.0, 1.0),
            high_shelf: (8000.0, 0.0, BUTTERWORTH_Q),
            high_cut: 20_000.0,
            high_cut_stages: 1,
        }
    }

    #[test]
    fn the_default_equalizer_is_flat() {
        for hz in [20.0, 100.0, 1000.0, 8000.0, 20_000.0] {
            assert!(close(eq_gain_db(&flat(), hz), 0.0, 1e-6), "{hz} Hz: {}", eq_gain_db(&flat(), hz));
        }
    }

    #[test]
    fn equalizer_bands_do_what_they_are_set_to() {
        let peak = EqBands { mid2: (3000.0, 6.0, 2.0), ..flat() };
        assert!(close(eq_gain_db(&peak, 3000.0), 6.0, 1e-6), "a peak reaches its gain at its frequency");
        assert!(eq_gain_db(&peak, 100.0).abs() < 0.1 && eq_gain_db(&peak, 20_000.0).abs() < 0.5);
        let cut = EqBands { mid1: (500.0, -9.0, 4.0), ..flat() };
        assert!(close(eq_gain_db(&cut, 500.0), -9.0, 1e-6));

        let shelves =
            EqBands { low_shelf: (100.0, 6.0, BUTTERWORTH_Q), high_shelf: (8000.0, -4.0, BUTTERWORTH_Q), ..flat() };
        assert!(close(eq_gain_db(&shelves, 20.0), 6.0, 0.3), "{}", eq_gain_db(&shelves, 20.0));
        assert!(close(eq_gain_db(&shelves, 100.0), 3.0, 0.1), "half the gain at the corner");
        assert!(eq_gain_db(&shelves, 1000.0).abs() < 0.3);
        assert!(close(eq_gain_db(&shelves, 20_000.0), -4.0, 0.5), "{}", eq_gain_db(&shelves, 20_000.0));

        let cuts = EqBands { low_cut: 180.0, high_cut: 9000.0, ..flat() };
        assert!(close(eq_gain_db(&cuts, 180.0), -3.01, 0.01), "a Butterworth corner is 3 dB down");
        assert!(close(eq_gain_db(&cuts, 9000.0), -3.01, 0.01));
        assert!(eq_gain_db(&cuts, 45.0) < -23.0 && eq_gain_db(&cuts, 1200.0).abs() < 0.1);

        // Steeper cuts are still 3 dB down at the corner, and fall by 24
        // and by 48 dB an octave: an octave under it, a Butterworth of the
        // order is 24.1 and 48.2 dB down.
        for (stages, octave_down) in [(2, -24.1), (4, -48.2)] {
            let steep = EqBands { low_cut: 180.0, low_cut_stages: stages, ..flat() };
            assert!(close(eq_gain_db(&steep, 180.0), -3.01, 0.01), "{stages}: {}", eq_gain_db(&steep, 180.0));
            assert!(close(eq_gain_db(&steep, 90.0), octave_down, 0.1), "{stages}: {}", eq_gain_db(&steep, 90.0));
            let top = EqBands { high_cut: 4000.0, high_cut_stages: stages, ..flat() };
            assert!(close(eq_gain_db(&top, 8000.0), octave_down, 0.1), "{stages}: {}", eq_gain_db(&top, 8000.0));
        }
        assert_eq!((cut_stages(0.0), cut_stages(1.0), cut_stages(2.0), cut_stages(9.0)), (1, 2, 4, 4));
        // A shelf with a Q overshoots under its corner and dips over it
        // (the cookbook's numbers for +9 dB at 200 Hz and a Q of 1.5).
        let sharp = EqBands { low_shelf: (200.0, 9.0, 1.5), ..flat() };
        assert!(close(eq_gain_db(&sharp, 140.0), 11.0, 0.1) && close(eq_gain_db(&sharp, 320.0), -2.3, 0.1));
        assert!(close(eq_gain_db(&sharp, 200.0), 4.5, 0.01), "still half the gain at the corner");
    }

    #[test]
    fn the_steep_filter_is_two_stages_with_no_more_of_a_peak() {
        use FilterShape::*;
        let db = |hz| to_db(steep_filter_gain(LowPass, 700.0, 0.12, hz));
        assert!(close(db(700.0), -6.0, 0.1), "6 dB down at the cutoff at the song's resonance: {}", db(700.0));
        assert!(close(db(2800.0), -48.0, 1.0), "24 dB an octave: {}", db(2800.0));
        assert!(close(to_db(steep_filter_gain(BandPass, 1000.0, 0.5, 1000.0)), 0.0, 0.01));
        assert!(to_db(steep_filter_gain(HighPass, 1000.0, 0.2, 250.0)) < -44.0);
    }

    #[test]
    fn a_tremolo_starts_at_full_level_and_dips_half_way() {
        for shape in 0..3 {
            assert_eq!(tremolo_level(shape, 0.4, 0.0), 1.0);
            assert!(close(tremolo_level(shape, 0.4, 0.5), 0.6, 1e-9), "{shape}");
            assert_eq!(tremolo_level(shape, 0.0, 0.5), 1.0);
        }
        assert!(close(tremolo_level(1, 1.0, 0.25), 0.5, 1e-9), "a triangle is half way down a quarter in");
        assert_eq!(tremolo_level(2, 1.0, 0.25), 1.0, "a square has not moved yet");
    }

    #[test]
    fn the_compressor_curve_bends_at_the_threshold() {
        let hard = |input| compressor_curve(-18.0, 4.0, 0.0, input);
        assert_eq!(hard(-40.0), -40.0, "below the threshold nothing changes");
        assert_eq!(hard(-18.0), -18.0);
        assert!(close(hard(-10.0), -16.0, 1e-9), "8 dB over at 4:1 comes out 2 dB over");
        assert!(close(hard(0.0), -13.5, 1e-9));

        // A knee rounds the corner without moving the lines it joins.
        let soft = |input| compressor_curve(-18.0, 4.0, 6.0, input);
        assert_eq!(soft(-21.0), -21.0);
        assert!(soft(-18.0) < -18.0 && soft(-18.0) > hard(-14.0) - 4.0);
        assert!(close(soft(-15.0), hard(-15.0), 1e-9));
        assert_eq!(compressor_curve(-18.0, 1.0, 6.0, 0.0), 0.0, "1:1 is a straight line");
    }

    #[test]
    fn waveshapers_and_the_pump_follow_the_dsp() {
        assert!(close(saturate(0, 0.5), 0.5f64.tanh(), 1e-12));
        assert_eq!(saturate(1, 3.0), 1.0);
        assert_eq!(saturate(1, -0.25), -0.25);
        assert!(saturate(2, -1.0).abs() > saturate(2, 1.0).abs(), "the tube shape is asymmetric");
        for shape in 0..3 {
            assert_eq!(saturate(shape, 0.0), 0.0);
        }
        assert!(close(pump_gain(0.7, 2.0, 0.0), 0.3, 1e-12), "ducked by the depth on the beat");
        assert_eq!(pump_gain(0.7, 2.0, 1.0), 1.0);
        assert!(pump_gain(0.7, 6.0, 0.3) > pump_gain(0.7, 0.3, 0.3), "a higher curve recovers sooner");
    }

    #[test]
    fn envelopes_always_fit_their_display() {
        let fastest = envelope_points(0.001, 0.005, 0.0, 0.005);
        let slowest = envelope_points(10.0, 20.0, 1.0, 20.0);
        for points in [fastest, slowest, envelope_points(0.005, 0.5, 0.8, 0.3)] {
            assert_eq!(points[0], (0.02, 0.0));
            assert_eq!(points[1].1, 1.0, "the attack ends at full level");
            assert_eq!(points[2].1, points[3].1, "the sustain is level");
            assert_eq!(points[4].1, 0.0);
            assert!(points.windows(2).all(|pair| pair[0].0 <= pair[1].0), "time runs left to right: {points:?}");
            assert!(points[4].0 <= 1.0 + 1e-6, "{points:?}");
        }
        assert!((slowest[4].0 - 0.98).abs() < 1e-5, "a long envelope uses the whole width");
        let typical = envelope_points(0.005, 1.0, 0.8, 0.25);
        assert!((typical[4].0 - 0.98).abs() < 1e-5, "so does an ordinary one: {typical:?}");
        assert!(fastest[4].0 < 0.5, "one that is short throughout stays narrow: {fastest:?}");
        assert!(fastest[1].0 - fastest[0].0 < 0.02, "an instant attack is a vertical edge");
        let (short, long) = (envelope_points(0.01, 0.1, 0.5, 0.1), envelope_points(0.01, 2.0, 0.5, 0.1));
        let decay_width = |points: [(f32, f32); 5]| points[2].0 - points[1].0;
        assert!(decay_width(long) > decay_width(short), "a longer decay is drawn wider");
        assert_eq!(envelope_points(0.01, 0.1, 0.8, 0.1)[2].1, 0.8);
    }
}
