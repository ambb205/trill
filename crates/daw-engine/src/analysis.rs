//! Measurements of rendered audio: level, loudness and spectral balance.
//! These let a caller that cannot listen (a script, an AI) judge a mix.

use serde::Serialize;
use std::f64::consts::PI;

const FLOOR_DB: f32 = -120.0;
const FFT_SIZE: usize = 4096;

/// Upper edges of the spectral bands, in Hz. The last band is open-ended.
const BAND_EDGES: [f64; 5] = [60.0, 250.0, 2000.0, 6000.0, f64::INFINITY];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AudioStats {
    pub duration_seconds: f64,
    /// Highest sample peak, dBFS. Above 0 means the file would clip.
    pub peak_db: f32,
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

#[derive(Clone, Debug, PartialEq, Serialize)]
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

pub fn analyze(left: &[f32], right: &[f32], sample_rate: u32) -> AudioStats {
    let frames = left.len().min(right.len());
    let (left, right) = (&left[..frames], &right[..frames]);

    let mut peak = 0.0f32;
    let mut clipped = 0;
    let (mut sum_ll, mut sum_rr, mut sum_lr) = (0.0f64, 0.0f64, 0.0f64);
    for (&l, &r) in left.iter().zip(right) {
        let magnitude = l.abs().max(r.abs());
        peak = peak.max(magnitude);
        clipped += usize::from(l.abs() >= 1.0) + usize::from(r.abs() >= 1.0);
        sum_ll += (l as f64) * (l as f64);
        sum_rr += (r as f64) * (r as f64);
        sum_lr += (l as f64) * (r as f64);
    }
    let mean_square = if frames == 0 { 0.0 } else { (sum_ll + sum_rr) / (2.0 * frames as f64) };
    let denominator = (sum_ll * sum_rr).sqrt();

    AudioStats {
        duration_seconds: frames as f64 / sample_rate as f64,
        peak_db: to_db((peak as f64) * (peak as f64)),
        rms_db: to_db(mean_square),
        loudness_lufs: loudness(left, right, sample_rate as f64),
        clipped_samples: clipped,
        stereo_correlation: if denominator > 0.0 { (sum_lr / denominator) as f32 } else { 1.0 },
        spectrum: spectrum(left, right, sample_rate as f64),
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

    fn tick(&mut self, input: f64) -> f64 {
        let out = self.b[0] * input + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [input, self.x[0]];
        self.y = [out, self.y[0]];
        out
    }
}

/// Integrated loudness in LUFS, with the standard absolute and relative gates.
fn loudness(left: &[f32], right: &[f32], sample_rate: f64) -> f32 {
    let block = (0.4 * sample_rate) as usize;
    let hop = (0.1 * sample_rate) as usize;
    if left.len() < block || hop == 0 {
        return FLOOR_DB;
    }

    let mut weighted_power = vec![0.0f64; left.len()];
    for channel in [left, right] {
        let mut filters = Weighting::k_weighting(sample_rate);
        for (power, &sample) in weighted_power.iter_mut().zip(channel) {
            let shelved = filters[0].tick(sample as f64);
            let weighted = filters[1].tick(shelved);
            *power += weighted * weighted;
        }
    }

    let mut blocks = Vec::new();
    let mut start = 0;
    while start + block <= weighted_power.len() {
        blocks.push(weighted_power[start..start + block].iter().sum::<f64>() / block as f64);
        start += hop;
    }

    let lufs = |power: f64| -0.691 + 10.0 * power.log10();
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;

    let audible: Vec<f64> = blocks.into_iter().filter(|&p| p > 0.0 && lufs(p) > -70.0).collect();
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

/// Energy share per band from an averaged, Hann-windowed power spectrum.
fn spectrum(left: &[f32], right: &[f32], sample_rate: f64) -> Spectrum {
    let mut bands = [0.0f64; 5];
    let window: Vec<f32> =
        (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos()).collect();
    let mut re = vec![0.0f32; FFT_SIZE];
    let mut im = vec![0.0f32; FFT_SIZE];
    let bin_hz = sample_rate / FFT_SIZE as f64;

    let mut start = 0;
    while start + FFT_SIZE <= left.len() {
        for i in 0..FFT_SIZE {
            re[i] = (left[start + i] + right[start + i]) * 0.5 * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let mut band = 0;
        for bin in 1..FFT_SIZE / 2 {
            while bin as f64 * bin_hz >= BAND_EDGES[band] {
                band += 1;
            }
            bands[band] += (re[bin] as f64).powi(2) + (im[bin] as f64).powi(2);
        }
        start += FFT_SIZE / 2;
    }

    let total: f64 = bands.iter().sum();
    let share = |energy: f64| if total > 0.0 { to_db(energy / total) } else { FLOOR_DB };
    Spectrum {
        sub_db: share(bands[0]),
        bass_db: share(bands[1]),
        mid_db: share(bands[2]),
        presence_db: share(bands[3]),
        air_db: share(bands[4]),
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
