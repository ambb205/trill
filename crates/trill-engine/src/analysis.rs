//! Measurements of rendered audio: level, loudness and spectral balance.
//! These let a caller that cannot listen (a script, an AI) judge a mix.

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

const FLOOR_DB: f32 = -120.0;
const FFT_SIZE: usize = 4096;

/// Upper edges of the spectral bands, in Hz. The last band is open-ended.
const BAND_EDGES: [f64; 5] = [60.0, 250.0, 2000.0, 6000.0, f64::INFINITY];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioStats {
    pub duration_seconds: f64,
    /// Highest sample peak, dBFS. Above 0 means the file would clip.
    pub peak_db: f32,
    /// The peak between the samples (true peak, by four times
    /// oversampling), dBFS: what a converter or a lossy encoder makes of
    /// it, which can be a dB or more above the highest sample. Measured
    /// for a mix, not for each track of one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_peak_db: Option<f32>,
    pub rms_db: f32,
    /// Integrated loudness (ITU-R BS.1770). Streaming targets sit around -14.
    pub loudness_lufs: f32,
    /// Samples at or beyond full scale.
    pub clipped_samples: usize,
    /// Left/right correlation: 1 = mono, 0 = wide, below 0 = phase problems.
    pub stereo_correlation: f32,
    /// Share of the total energy per band, in dB (0 = all of it).
    pub spectrum: Spectrum,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spectrum {
    /// Below 60 Hz.
    pub sub_db: f32,
    /// 60-250 Hz.
    pub bass_db: f32,
    /// 250 Hz - 2 kHz.
    pub mid_db: f32,
    /// 2-6 kHz.
    pub presence_db: f32,
    /// Above 6 kHz.
    pub air_db: f32,
}

fn to_db(power: f64) -> f32 {
    if power <= 0.0 { FLOOR_DB } else { (10.0 * power.log10()).max(FLOOR_DB as f64) as f32 }
}

/// A linear level in dB, never below the floor every measurement shares.
pub fn level_db(linear: f32) -> f32 {
    to_db(f64::from(linear) * f64::from(linear))
}

pub fn analyze(left: &[f32], right: &[f32], sample_rate: u32) -> AudioStats {
    let mut analyzer = Analyzer::new(sample_rate);
    analyzer.push(left, right);
    analyzer.stats()
}

/// Measures audio as it goes by, a block at a time, holding on to a few
/// numbers a second and none of the audio: a render measures every track
/// this way without keeping a stem of each.
///
/// What comes out is what [`analyze`] gives for the same audio in one
/// piece, and loudness and peak for any stretch of it afterwards
/// ([`Analyzer::loudness_between`]): the parts of a song.
pub struct Analyzer {
    sample_rate: f64,
    frames: usize,
    peak: f32,
    clipped: usize,
    sum_ll: f64,
    sum_rr: f64,
    sum_lr: f64,
    /// The loudness weighting of each channel.
    weighting: [[Weighting; 2]; 2],
    /// Frames in a tenth of a second, the step loudness is gated in.
    hop: usize,
    /// How much of the current step is in, its weighted energy and peak.
    hop_fill: usize,
    hop_power: f64,
    hop_peak: f32,
    /// The weighted energy and the peak of every whole step so far.
    hops: Vec<f64>,
    hop_peaks: Vec<f32>,
    spectrum: SpectrumMeter,
    true_peak: Option<[Oversampler; 2]>,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        Self {
            sample_rate: rate,
            frames: 0,
            peak: 0.0,
            clipped: 0,
            sum_ll: 0.0,
            sum_rr: 0.0,
            sum_lr: 0.0,
            weighting: [Weighting::k_weighting(rate), Weighting::k_weighting(rate)],
            hop: ((0.1 * rate) as usize).max(1),
            hop_fill: 0,
            hop_power: 0.0,
            hop_peak: 0.0,
            hops: Vec::new(),
            hop_peaks: Vec::new(),
            spectrum: SpectrumMeter::new(rate),
            true_peak: None,
        }
    }

    /// Also measures the peak between the samples, where a converter
    /// would put it: a few dozen multiplications a frame, which is worth
    /// it for a mix and not for every track of one.
    pub fn with_true_peak(mut self) -> Self {
        self.true_peak = Some([Oversampler::new(), Oversampler::new()]);
        self
    }

    /// Takes in the next frames. `left` and `right` are equally long.
    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        let frames = left.len().min(right.len());
        for (&l, &r) in left[..frames].iter().zip(&right[..frames]) {
            let magnitude = l.abs().max(r.abs());
            self.peak = self.peak.max(magnitude);
            self.clipped += usize::from(l.abs() >= 1.0) + usize::from(r.abs() >= 1.0);
            let (l64, r64) = (f64::from(l), f64::from(r));
            self.sum_ll += l64 * l64;
            self.sum_rr += r64 * r64;
            self.sum_lr += l64 * r64;

            let [left_filters, right_filters] = &mut self.weighting;
            let shelved_left = left_filters[0].tick(l64);
            let weighted_left = left_filters[1].tick(shelved_left);
            let shelved_right = right_filters[0].tick(r64);
            let weighted_right = right_filters[1].tick(shelved_right);
            self.hop_power += weighted_left * weighted_left + weighted_right * weighted_right;
            self.hop_peak = self.hop_peak.max(magnitude);
            self.hop_fill += 1;
            if self.hop_fill == self.hop {
                self.hops.push(self.hop_power);
                self.hop_peaks.push(self.hop_peak);
                (self.hop_fill, self.hop_power, self.hop_peak) = (0, 0.0, 0.0);
            }
        }
        self.spectrum.push(&left[..frames], &right[..frames]);
        if let Some([over_left, over_right]) = &mut self.true_peak {
            over_left.push(&left[..frames]);
            over_right.push(&right[..frames]);
        }
        self.frames += frames;
    }

    /// How many frames have been taken in.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The highest sample so far, linear.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// Everything measured so far.
    pub fn stats(&self) -> AudioStats {
        let frames = self.frames;
        let mean_square = if frames == 0 { 0.0 } else { (self.sum_ll + self.sum_rr) / (2.0 * frames as f64) };
        let denominator = (self.sum_ll * self.sum_rr).sqrt();
        let between = self.true_peak.as_ref().map(|[left, right]| left.peak().max(right.peak()));
        AudioStats {
            duration_seconds: frames as f64 / self.sample_rate,
            peak_db: level_db(self.peak),
            // Never below the highest sample: the samples are part of the
            // signal between them.
            true_peak_db: between.map(|between| level_db(between.max(self.peak))),
            rms_db: to_db(mean_square),
            loudness_lufs: gated_loudness(&self.hops, self.hop),
            clipped_samples: self.clipped,
            stereo_correlation: if denominator > 0.0 { (self.sum_lr / denominator) as f32 } else { 1.0 },
            spectrum: self.spectrum.shares(),
        }
    }

    /// Where the energy sits, as one number: the centre of the spectrum
    /// in Hz. `None` for silence, and for less than a tenth of a second.
    pub fn brightness_hz(&self) -> Option<f64> {
        self.spectrum.centroid()
    }

    /// The whole tenths of a second that lie in the frames `from..to`.
    fn steps(&self, from: usize, to: usize) -> std::ops::Range<usize> {
        let first = from.div_ceil(self.hop).min(self.hops.len());
        let last = (to / self.hop).min(self.hops.len());
        first..last.max(first)
    }

    /// The loudness of the frames `from..to` in LUFS, measured as the
    /// whole is and to the tenth of a second. The floor (-120) for a
    /// stretch shorter than the 0.4 s loudness is measured over, or silent.
    pub fn loudness_between(&self, from: usize, to: usize) -> f32 {
        gated_loudness(&self.hops[self.steps(from, to)], self.hop)
    }

    /// How loud every whole tenth of a second so far was, in dB, weighted
    /// as loudness is: the level as it went, ten numbers a second. What
    /// lights a picture of the song is read from this.
    pub fn levels(&self) -> Vec<f32> {
        let frames = (2 * self.hop) as f64;
        self.hops.iter().map(|power| to_db(power / frames)).collect()
    }

    /// The highest sample in the frames `from..to`, in dB, to the tenth
    /// of a second.
    pub fn peak_between(&self, from: usize, to: usize) -> f32 {
        level_db(self.hop_peaks[self.steps(from, to)].iter().fold(0.0f32, |peak, hop| peak.max(*hop)))
    }
}

/// Direct-form biquad in f64, for the loudness weighting filters.
struct Weighting {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Weighting {
    /// The two K-weighting stages of BS.1770, designed for `sample_rate`.
    fn k_weighting(sample_rate: f64) -> [Weighting; 2] {
        let shelf = {
            let k = (PI * 1681.974450955533 / sample_rate).tan();
            let q = 0.7071752369554196;
            let vh = 10f64.powf(3.999843853973347 / 20.0);
            let vb = vh.powf(0.4996667741545416);
            let a0 = 1.0 + k / q + k * k;
            Weighting {
                b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
                a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
                x: [0.0; 2],
                y: [0.0; 2],
            }
        };
        let highpass = {
            let k = (PI * 38.13547087602444 / sample_rate).tan();
            let q = 0.5003270373238773;
            let a0 = 1.0 + k / q + k * k;
            Weighting {
                b: [1.0, -2.0, 1.0],
                a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
                x: [0.0; 2],
                y: [0.0; 2],
            }
        };
        [shelf, highpass]
    }

    #[inline]
    fn tick(&mut self, input: f64) -> f64 {
        let out = self.b[0] * input + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [input, self.x[0]];
        self.y = [out, self.y[0]];
        out
    }
}

/// Integrated loudness in LUFS, with the standard absolute and relative
/// gates, from the weighted energy of consecutive tenths of a second
/// (`hop` frames each): the 0.4 s blocks of BS.1770 are four of them, and
/// overlap by three.
fn gated_loudness(hops: &[f64], hop: usize) -> f32 {
    if hops.len() < 4 {
        return FLOOR_DB;
    }
    let block = (4 * hop) as f64;
    let blocks = hops.windows(4).map(|four| four.iter().sum::<f64>() / block);
    let lufs = |power: f64| -0.691 + 10.0 * power.log10();
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;

    let audible: Vec<f64> = blocks.filter(|&p| p > 0.0 && lufs(p) > -70.0).collect();
    if audible.is_empty() {
        return FLOOR_DB;
    }
    let relative_gate = lufs(mean(&audible)) - 10.0;
    let gated: Vec<f64> = audible.into_iter().filter(|&p| lufs(p) > relative_gate).collect();
    if gated.is_empty() {
        return FLOOR_DB;
    }
    lufs(mean(&gated)) as f32
}

/// Energy per band from an averaged, Hann-windowed power spectrum of the
/// two channels' sum, taken in as the audio goes by.
struct SpectrumMeter {
    bin_hz: f64,
    window: Vec<f32>,
    /// The last frames of the mono sum, up to one transform's worth.
    held: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    bands: [f64; 5],
    /// Energy times frequency, and energy: the centre of the spectrum.
    weighted: f64,
    energy: f64,
}

impl SpectrumMeter {
    fn new(sample_rate: f64) -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos())
            .collect();
        Self {
            bin_hz: sample_rate / FFT_SIZE as f64,
            window,
            held: Vec::with_capacity(FFT_SIZE),
            re: vec![0.0; FFT_SIZE],
            im: vec![0.0; FFT_SIZE],
            bands: [0.0; 5],
            weighted: 0.0,
            energy: 0.0,
        }
    }

    fn push(&mut self, left: &[f32], right: &[f32]) {
        for (l, r) in left.iter().zip(right) {
            self.held.push((l + r) * 0.5);
            if self.held.len() == FFT_SIZE {
                self.transform();
                // The next transform overlaps this one by half.
                self.held.drain(..FFT_SIZE / 2);
            }
        }
    }

    fn transform(&mut self) {
        for i in 0..FFT_SIZE {
            self.re[i] = self.held[i] * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        let mut band = 0;
        for bin in 1..FFT_SIZE / 2 {
            let hz = bin as f64 * self.bin_hz;
            while hz >= BAND_EDGES[band] {
                band += 1;
            }
            let power = f64::from(self.re[bin]).powi(2) + f64::from(self.im[bin]).powi(2);
            self.bands[band] += power;
            self.weighted += power * hz;
            self.energy += power;
        }
    }

    fn shares(&self) -> Spectrum {
        let total: f64 = self.bands.iter().sum();
        let share = |energy: f64| if total > 0.0 { to_db(energy / total) } else { FLOOR_DB };
        Spectrum {
            sub_db: share(self.bands[0]),
            bass_db: share(self.bands[1]),
            mid_db: share(self.bands[2]),
            presence_db: share(self.bands[3]),
            air_db: share(self.bands[4]),
        }
    }

    fn centroid(&self) -> Option<f64> {
        (self.energy > 0.0).then(|| self.weighted / self.energy)
    }
}

/// Taps of each of the four phases of the interpolator.
const PHASE_TAPS: usize = 12;

/// Finds the peak between the samples: the signal is interpolated to four
/// times its rate with a windowed-sinc filter, as BS.1770 describes for
/// true peak, and the highest of what comes out is kept.
struct Oversampler {
    /// The filter, a phase at a time.
    phases: [[f32; PHASE_TAPS]; 4],
    /// The last samples, written round.
    history: [f32; PHASE_TAPS],
    at: usize,
    peak: f32,
}

impl Oversampler {
    fn new() -> Self {
        let taps = 4 * PHASE_TAPS;
        let centre = (taps - 1) as f64 / 2.0;
        let mut phases = [[0.0f32; PHASE_TAPS]; 4];
        for tap in 0..taps {
            let x = (tap as f64 - centre) / 4.0;
            let sinc = if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) };
            let window = 0.5 - 0.5 * (2.0 * PI * (tap as f64 + 0.5) / taps as f64).cos();
            phases[tap % 4][tap / 4] = (sinc * window) as f32;
        }
        // Each phase passes a steady level unchanged.
        for phase in &mut phases {
            let sum: f32 = phase.iter().sum();
            phase.iter_mut().for_each(|tap| *tap /= sum);
        }
        Self { phases, history: [0.0; PHASE_TAPS], at: 0, peak: 0.0 }
    }

    fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.history[self.at] = sample;
            self.at = (self.at + 1) % PHASE_TAPS;
            for phase in &self.phases {
                // The newest sample meets the filter's first tap.
                let mut sum = 0.0f32;
                for (tap, weight) in phase.iter().enumerate() {
                    sum += weight * self.history[(self.at + PHASE_TAPS - 1 - tap) % PHASE_TAPS];
                }
                self.peak = self.peak.max(sum.abs());
            }
        }
    }

    fn peak(&self) -> f32 {
        self.peak
    }
}

/// In-place radix-2 FFT. The length must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f32::consts::PI / len as f32;
        let (step_re, step_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut w_re, mut w_im) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let t_re = re[b] * w_re - im[b] * w_im;
                let t_im = re[b] * w_im + im[b] * w_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                (w_re, w_im) = (w_re * step_re - w_im * step_im, w_re * step_im + w_im * step_re);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, amplitude: f32, seconds: f64, sample_rate: u32) -> Vec<f32> {
        (0..(seconds * sample_rate as f64) as usize)
            .map(|i| (2.0 * PI * hz * i as f64 / sample_rate as f64).sin() as f32 * amplitude)
            .collect()
    }

    #[test]
    fn full_scale_1khz_sine_reads_minus_3_lufs() {
        // BS.1770 reference: a 0 dBFS 997 Hz sine measures -3.01 LUFS in one
        // channel and 0 LUFS in both.
        let tone = sine(997.0, 1.0, 5.0, 48_000);
        let stats = analyze(&tone, &tone, 48_000);
        assert!((stats.loudness_lufs - 0.0).abs() < 0.2, "measured {}", stats.loudness_lufs);
        assert!((stats.peak_db - 0.0).abs() < 0.01);
        assert!((stats.rms_db + 3.01).abs() < 0.05);
        assert!((stats.stereo_correlation - 1.0).abs() < 1e-4);

        let silence = vec![0.0; tone.len()];
        let one_side = analyze(&tone, &silence, 48_000);
        assert!((one_side.loudness_lufs + 3.01).abs() < 0.2, "measured {}", one_side.loudness_lufs);
    }

    #[test]
    fn loudness_tracks_gain_and_sample_rate() {
        let loud = sine(997.0, 0.5, 4.0, 44_100);
        let stats = analyze(&loud, &loud, 44_100);
        assert!((stats.loudness_lufs + 6.02).abs() < 0.2, "measured {}", stats.loudness_lufs);
    }

    #[test]
    fn spectrum_puts_energy_in_the_right_band() {
        for (hz, band) in [(40.0, 0), (120.0, 1), (1000.0, 2), (4000.0, 3), (10_000.0, 4)] {
            let tone = sine(hz, 0.5, 1.0, 48_000);
            let s = analyze(&tone, &tone, 48_000).spectrum;
            let shares = [s.sub_db, s.bass_db, s.mid_db, s.presence_db, s.air_db];
            for (index, share) in shares.iter().enumerate() {
                if index == band {
                    assert!(*share > -0.5, "{hz} Hz: band {index} holds {share} dB");
                } else {
                    assert!(*share < -20.0, "{hz} Hz leaked {share} dB into band {index}");
                }
            }
        }
    }

    #[test]
    fn audio_taken_in_pieces_measures_as_it_does_whole_and_part_by_part() {
        // Two seconds loud, two seconds 12 dB quieter, in odd-sized pieces.
        let mut tone = sine(997.0, 0.5, 2.0, 48_000);
        tone.extend(sine(997.0, 0.125, 2.0, 48_000));
        let right: Vec<f32> = tone.iter().map(|sample| sample * 0.5).collect();
        let whole = analyze(&tone, &right, 48_000);
        let mut pieces = Analyzer::new(48_000);
        let mut at = 0;
        for size in [1usize, 7, 480, 4095, 4097, 31, 1024].iter().cycle() {
            let end = (at + size).min(tone.len());
            pieces.push(&tone[at..end], &right[at..end]);
            at = end;
            if at == tone.len() {
                break;
            }
        }
        assert_eq!(pieces.stats(), whole);
        assert_eq!(pieces.frames(), tone.len());

        // Each half by itself: 12 dB apart, and the loud one louder than
        // the two together.
        let (loud, soft) = (pieces.loudness_between(0, 96_000), pieces.loudness_between(96_000, 192_000));
        assert!((loud - soft - 12.04).abs() < 0.1, "{loud} then {soft}");
        assert!(loud > whole.loudness_lufs && whole.loudness_lufs > soft);
        assert!((pieces.peak_between(0, 96_000) + 6.02).abs() < 0.05);
        assert!((pieces.peak_between(96_000, 192_000) + 18.06).abs() < 0.05);
        // A stretch too short to have a loudness, or beyond the audio.
        assert_eq!(pieces.loudness_between(0, 4_800), FLOOR_DB);
        assert_eq!(pieces.loudness_between(500_000, 600_000), FLOOR_DB);
        assert_eq!(pieces.peak_between(500_000, 600_000), FLOOR_DB);
    }

    #[test]
    fn the_peak_between_the_samples_is_found() {
        // A sine at a quarter of the sample rate, sampled 45 degrees off
        // its crests: every sample is 3 dB below the wave's real height.
        let wave: Vec<f32> = (0..48_000).map(|i| (0.8 * (PI * (i as f64 / 2.0 + 0.25)).sin()) as f32).collect();
        let plain = analyze(&wave, &wave, 48_000);
        assert_eq!(plain.true_peak_db, None, "not measured unless asked for");
        let mut analyzer = Analyzer::new(48_000).with_true_peak();
        analyzer.push(&wave, &wave);
        let stats = analyzer.stats();
        assert!((stats.peak_db + 4.95).abs() < 0.05, "the samples reach {}", stats.peak_db);
        let true_peak = stats.true_peak_db.unwrap();
        assert!((true_peak + 1.94).abs() < 0.3, "the wave reaches -1.94 dB: {true_peak}");

        // A wave whose samples sit on its crests has no more to find, and
        // the true peak is never below the highest sample.
        let slow = sine(100.0, 0.5, 1.0, 48_000);
        let mut analyzer = Analyzer::new(48_000).with_true_peak();
        analyzer.push(&slow, &slow);
        let stats = analyzer.stats();
        let true_peak = stats.true_peak_db.unwrap();
        assert!(true_peak >= stats.peak_db && true_peak - stats.peak_db < 0.1, "{true_peak} against {}", stats.peak_db);
    }

    #[test]
    fn brightness_is_the_centre_of_the_spectrum() {
        let mut low = Analyzer::new(48_000);
        let tone = sine(200.0, 0.5, 1.0, 48_000);
        low.push(&tone, &tone);
        assert!((low.brightness_hz().unwrap() - 200.0).abs() < 20.0, "{:?}", low.brightness_hz());
        let mut both = Analyzer::new(48_000);
        let high = sine(5_000.0, 0.5, 1.0, 48_000);
        let mixed: Vec<f32> = tone.iter().zip(&high).map(|(a, b)| (a + b) * 0.5).collect();
        both.push(&mixed, &mixed);
        assert!((both.brightness_hz().unwrap() - 2_600.0).abs() < 100.0, "{:?}", both.brightness_hz());
        assert_eq!(Analyzer::new(48_000).brightness_hz(), None);
    }

    #[test]
    fn silence_and_clipping_are_reported() {
        let silence = vec![0.0f32; 48_000];
        let stats = analyze(&silence, &silence, 48_000);
        assert_eq!(stats.peak_db, FLOOR_DB);
        assert_eq!(stats.loudness_lufs, FLOOR_DB);
        assert_eq!(stats.clipped_samples, 0);

        let hot = vec![1.5f32; 10];
        assert_eq!(analyze(&hot, &hot, 48_000).clipped_samples, 20);
    }
}
