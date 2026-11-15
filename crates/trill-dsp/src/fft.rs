//! A small real-input FFT for offline analysis and resynthesis.
//!
//! Written here rather than pulled in as a dependency so that results are
//! bit-identical on every machine (no runtime SIMD dispatch), which keeps
//! offline renders reproducible. Not for the audio thread: [`RealFft::new`]
//! allocates, and the transforms are sized for analysis work, not for
//! per-sample use.

use std::f64::consts::PI;

/// Transforms `size` real samples to `size / 2 + 1` complex bins and back.
///
/// Spectra are kept as separate real and imaginary slices, which is what the
/// callers' per-bin loops vectorize best over.
pub struct RealFft {
    size: usize,
    /// Half-size complex transform the real one is built on.
    half: ComplexFft,
    /// `e^(-2 pi i k / size)` for `k` in `0..size / 2`, to untangle the even
    /// and odd samples that the half-size transform processes as one complex
    /// sequence.
    twist_re: Vec<f32>,
    twist_im: Vec<f32>,
    work_re: Vec<f32>,
    work_im: Vec<f32>,
}

impl RealFft {
    /// `size` must be a power of two, at least 4.
    pub fn new(size: usize) -> Self {
        assert!(size.is_power_of_two() && size >= 4, "FFT size must be a power of two >= 4, got {size}");
        let half = size / 2;
        let (twist_re, twist_im) = (0..half)
            .map(|k| {
                let angle = -2.0 * PI * k as f64 / size as f64;
                (angle.cos() as f32, angle.sin() as f32)
            })
            .unzip();
        Self {
            size,
            half: ComplexFft::new(half),
            twist_re,
            twist_im,
            work_re: vec![0.0; half],
            work_im: vec![0.0; half],
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Number of bins in a spectrum: `size / 2 + 1`.
    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// Forward transform, unscaled: a full-scale sine at a bin center has
    /// magnitude `size / 2` in that bin.
    pub fn forward(&mut self, input: &[f32], re: &mut [f32], im: &mut [f32]) {
        let half = self.size / 2;
        assert!(input.len() == self.size && re.len() == half + 1 && im.len() == half + 1);

        // Even samples become the real part and odd samples the imaginary
        // part of a half-length complex sequence.
        for (k, pair) in input.chunks_exact(2).enumerate() {
            self.work_re[k] = pair[0];
            self.work_im[k] = pair[1];
        }
        self.half.forward(&mut self.work_re, &mut self.work_im);

        // With Z the transform of that sequence, the transforms of the even
        // and odd samples are E[k] = (Z[k] + conj(Z[-k])) / 2 and
        // O[k] = (Z[k] - conj(Z[-k])) / 2i, and X[k] = E[k] + w^k O[k].
        for k in 0..half {
            let mirror = (half - k) % half;
            let (zr, zi) = (self.work_re[k], self.work_im[k]);
            let (mr, mi) = (self.work_re[mirror], -self.work_im[mirror]);
            let (er, ei) = (0.5 * (zr + mr), 0.5 * (zi + mi));
            let (or, oi) = (0.5 * (zi - mi), -0.5 * (zr - mr));
            let (wr, wi) = (self.twist_re[k], self.twist_im[k]);
            re[k] = er + wr * or - wi * oi;
            im[k] = ei + wr * oi + wi * or;
        }
        // The Nyquist bin is E[0] - O[0], which is real.
        let (zr, zi) = (self.work_re[0], self.work_im[0]);
        re[half] = zr - zi;
        im[half] = 0.0;
    }

    /// Inverse transform, scaled so that `inverse(forward(x)) == x`. The
    /// imaginary parts of the first and last bins are ignored, as a real
    /// signal has none.
    pub fn inverse(&mut self, re: &[f32], im: &[f32], output: &mut [f32]) {
        let half = self.size / 2;
        assert!(output.len() == self.size && re.len() == half + 1 && im.len() == half + 1);

        // Runs the forward derivation backwards: recover E and O from X[k]
        // and X[k + half] = conj(X[half - k]), then Z = E + i O.
        for k in 0..half {
            let (xr, xi) = (re[k], if k == 0 { 0.0 } else { im[k] });
            let (yr, yi) = (re[half - k], if k == 0 { 0.0 } else { -im[half - k] });
            let (er, ei) = (0.5 * (xr + yr), 0.5 * (xi + yi));
            let (dr, di) = (0.5 * (xr - yr), 0.5 * (xi - yi));
            // O[k] = conj(w^k) * (X[k] - X[k + half]) / 2.
            let (wr, wi) = (self.twist_re[k], -self.twist_im[k]);
            let (or, oi) = (wr * dr - wi * di, wr * di + wi * dr);
            self.work_re[k] = er - oi;
            self.work_im[k] = ei + or;
        }
        self.half.inverse(&mut self.work_re, &mut self.work_im);
        for (k, pair) in output.chunks_exact_mut(2).enumerate() {
            pair[0] = self.work_re[k];
            pair[1] = self.work_im[k];
        }
    }
}

/// In-place radix-2 complex FFT.
struct ComplexFft {
    size: usize,
    /// Index pairs to swap for the bit-reversal permutation.
    swaps: Vec<(u32, u32)>,
    /// Twiddle factors for the stages of half-size 4 and up, laid end to end
    /// so each stage reads its own contiguously: 4, then 8, ... `size / 2`.
    twiddle_re: Vec<f32>,
    twiddle_im: Vec<f32>,
}

impl ComplexFft {
    fn new(size: usize) -> Self {
        debug_assert!(size.is_power_of_two() && size >= 2);
        let bits = size.trailing_zeros();
        let swaps = (0..size as u32)
            .filter_map(|i| {
                let j = i.reverse_bits() >> (32 - bits);
                (i < j).then_some((i, j))
            })
            .collect();

        let mut twiddle_re = Vec::with_capacity(size);
        let mut twiddle_im = Vec::with_capacity(size);
        let mut half = 4;
        while half < size {
            for j in 0..half {
                let angle = -PI * j as f64 / half as f64;
                twiddle_re.push(angle.cos() as f32);
                twiddle_im.push(angle.sin() as f32);
            }
            half *= 2;
        }
        Self { size, swaps, twiddle_re, twiddle_im }
    }

    fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        debug_assert!(re.len() == self.size && im.len() == self.size);
        for &(i, j) in &self.swaps {
            re.swap(i as usize, j as usize);
            im.swap(i as usize, j as usize);
        }

        // The first two stages have trivial twiddle factors (1 and -i) and
        // are done together, four points at a time.
        if self.size >= 4 {
            for (re, im) in re.chunks_exact_mut(4).zip(im.chunks_exact_mut(4)) {
                let (ar, ai) = (re[0] + re[1], im[0] + im[1]);
                let (br, bi) = (re[0] - re[1], im[0] - im[1]);
                let (cr, ci) = (re[2] + re[3], im[2] + im[3]);
                // (c - d) * -i
                let (dr, di) = (im[2] - im[3], re[3] - re[2]);
                (re[0], im[0]) = (ar + cr, ai + ci);
                (re[1], im[1]) = (br + dr, bi + di);
                (re[2], im[2]) = (ar - cr, ai - ci);
                (re[3], im[3]) = (br - dr, bi - di);
            }
        } else {
            (re[0], re[1]) = (re[0] + re[1], re[0] - re[1]);
            (im[0], im[1]) = (im[0] + im[1], im[0] - im[1]);
        }

        // The remaining stages are written over equal-length slices so that
        // the compiler can drop the bounds checks and vectorize the loop.
        let mut half = 4;
        let mut offset = 0;
        while half < self.size {
            let (wr, wi) = (&self.twiddle_re[offset..offset + half], &self.twiddle_im[offset..offset + half]);
            for (block_re, block_im) in re.chunks_exact_mut(half * 2).zip(im.chunks_exact_mut(half * 2)) {
                let (lo_re, hi_re) = block_re.split_at_mut(half);
                let (lo_im, hi_im) = block_im.split_at_mut(half);
                let points = lo_re.iter_mut().zip(lo_im).zip(hi_re.iter_mut().zip(hi_im)).zip(wr.iter().zip(wi));
                for (((lo_re, lo_im), (hi_re, hi_im)), (wr, wi)) in points {
                    let tr = *hi_re * wr - *hi_im * wi;
                    let ti = *hi_re * wi + *hi_im * wr;
                    *hi_re = *lo_re - tr;
                    *hi_im = *lo_im - ti;
                    *lo_re += tr;
                    *lo_im += ti;
                }
            }
            offset += half;
            half *= 2;
        }
    }

    /// Scaled inverse, via the identity that swapping real and imaginary
    /// parts before and after a forward transform inverts it.
    fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        self.forward(im, re);
        let scale = 1.0 / self.size as f32;
        for value in re.iter_mut().chain(im.iter_mut()) {
            *value *= scale;
        }
    }
}

/// A periodic Hann window: `size` points of a raised cosine whose period is
/// exactly `size`, so that hops of `size / 2`, `size / 4`... overlap-add to a
/// constant.
pub fn hann(size: usize) -> Vec<f32> {
    (0..size).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / size as f64).cos()) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    fn naive(input: &[f32], bin: usize) -> (f64, f64) {
        let n = input.len() as f64;
        input.iter().enumerate().fold((0.0, 0.0), |(re, im), (i, &x)| {
            let angle = -2.0 * PI * bin as f64 * i as f64 / n;
            (re + x as f64 * angle.cos(), im + x as f64 * angle.sin())
        })
    }

    #[test]
    fn matches_the_definition_and_round_trips() {
        let mut rng = Rng::new(7);
        for size in [4, 8, 64, 1024] {
            let input: Vec<f32> = (0..size).map(|_| rng.bipolar()).collect();
            let mut fft = RealFft::new(size);
            let (mut re, mut im) = (vec![0.0; fft.bins()], vec![0.0; fft.bins()]);
            fft.forward(&input, &mut re, &mut im);
            for bin in 0..fft.bins() {
                let (want_re, want_im) = naive(&input, bin);
                let error = (re[bin] as f64 - want_re).abs().max((im[bin] as f64 - want_im).abs());
                assert!(error < 1e-3 * (size as f64).sqrt(), "size {size} bin {bin}: error {error}");
            }
            let mut output = vec![0.0; size];
            fft.inverse(&re, &im, &mut output);
            for (a, b) in input.iter().zip(&output) {
                assert!((a - b).abs() < 1e-5, "size {size}: round trip {a} -> {b}");
            }
        }
    }

    #[test]
    fn hann_overlap_adds_to_a_constant() {
        let window = hann(64);
        for i in 0..16 {
            let sum: f32 = (0..4).map(|k| window[i + 16 * k]).sum();
            assert!((sum - 2.0).abs() < 1e-5);
        }
    }
}
