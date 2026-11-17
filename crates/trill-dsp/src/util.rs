//! Small DSP building blocks shared by the built-in devices.

use std::f32::consts::PI;

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db * 0.05)
}

#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-9).log10()
}

#[inline]
pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Per-sample multiplier that decays a value by 60 dB over `seconds`.
#[inline]
pub fn decay_coef(seconds: f32, sample_rate: f32) -> f32 {
    (-6.9078 / (seconds.max(1e-4) * sample_rate)).exp()
}

/// One-pole smoothing coefficient for a time constant of `seconds`.
#[inline]
pub fn smooth_coef(seconds: f32, sample_rate: f32) -> f32 {
    1.0 - (-1.0 / (seconds.max(1e-5) * sample_rate)).exp()
}

/// Constant-power pan gains for `pan` in -1..=1, unity at center.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * 0.25 * PI;
    (angle.cos() * std::f32::consts::SQRT_2, angle.sin() * std::f32::consts::SQRT_2)
}

/// Dry and wet gains for an insert effect's `mix` control. The dry signal
/// stays at unity up to 0.5 and the wet signal reaches unity there, so adding
/// a little effect never turns the source down.
#[inline]
pub fn mix_gains(mix: f32) -> (f32, f32) {
    let mix = mix.clamp(0.0, 1.0);
    ((2.0 * (1.0 - mix)).min(1.0), (2.0 * mix).min(1.0))
}

/// Equal-power crossfade gains, for effects that replace the signal.
#[inline]
pub fn crossfade_gains(mix: f32) -> (f32, f32) {
    let angle = mix.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

/// Fast white noise (xorshift32).
#[derive(Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in -1..1.
    #[inline]
    pub fn bipolar(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }

    /// Uniform in 0..1.
    #[inline]
    pub fn unipolar(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }
}

/// One-pole parameter smoother.
#[derive(Copy, Clone)]
pub struct Smooth {
    value: f32,
    coef: f32,
}

impl Smooth {
    pub fn new(value: f32) -> Self {
        Self { value, coef: 1.0 }
    }

    pub fn set_time(&mut self, seconds: f32, sample_rate: f32) {
        self.coef = smooth_coef(seconds, sample_rate);
    }

    pub fn snap(&mut self, value: f32) {
        self.value = value;
    }

    #[inline]
    pub fn next(&mut self, target: f32) -> f32 {
        self.value += (target - self.value) * self.coef;
        self.value
    }
}

/// Outputs of one state-variable filter step.
#[derive(Copy, Clone)]
pub struct SvfOut {
    pub low: f32,
    pub band: f32,
    pub high: f32,
}

/// Zero-delay-feedback state variable filter (Simper/Cytomic form). Stable
/// under fast cutoff modulation.
#[derive(Copy, Clone, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Svf {
    pub fn set(&mut self, cutoff_hz: f32, q: f32, sample_rate: f32) {
        let cutoff = cutoff_hz.clamp(10.0, sample_rate * 0.45);
        let g = (PI * cutoff / sample_rate).tan();
        self.k = 1.0 / q.max(0.05);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    #[inline]
    pub fn tick(&mut self, input: f32) -> SvfOut {
        let v3 = input - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        SvfOut { low: v2, band: v1, high: input - self.k * v1 - v2 }
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

/// Biquad in transposed direct form II, with RBJ cookbook designs.
#[derive(Copy, Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }
}

impl Biquad {
    fn set(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }

    pub fn set_bypass(&mut self) {
        self.set(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    fn omega(freq: f32, sample_rate: f32) -> (f32, f32) {
        let w = 2.0 * PI * freq.clamp(10.0, sample_rate * 0.45) / sample_rate;
        (w.sin(), w.cos())
    }

    pub fn set_peak(&mut self, freq: f32, gain_db: f32, q: f32, sample_rate: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let (sin, cos) = Self::omega(freq, sample_rate);
        let alpha = sin / (2.0 * q.max(0.05));
        self.set(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a);
    }

    /// A low shelf. `q` is the shelf's slope as a filter's Q: 0.707 is the
    /// plain shelf, more overshoots on either side of the corner.
    pub fn set_low_shelf(&mut self, freq: f32, gain_db: f32, q: f32, sample_rate: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let (sin, cos) = Self::omega(freq, sample_rate);
        let beta = 2.0 * a.sqrt() * sin / (2.0 * q.max(0.05));
        self.set(
            a * ((a + 1.0) - (a - 1.0) * cos + beta),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - beta),
            (a + 1.0) + (a - 1.0) * cos + beta,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - beta,
        );
    }

    /// A high shelf; `q` as for [`Biquad::set_low_shelf`].
    pub fn set_high_shelf(&mut self, freq: f32, gain_db: f32, q: f32, sample_rate: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let (sin, cos) = Self::omega(freq, sample_rate);
        let beta = 2.0 * a.sqrt() * sin / (2.0 * q.max(0.05));
        self.set(
            a * ((a + 1.0) + (a - 1.0) * cos + beta),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - beta),
            (a + 1.0) - (a - 1.0) * cos + beta,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - beta,
        );
    }

    pub fn set_low_pass(&mut self, freq: f32, q: f32, sample_rate: f32) {
        let (sin, cos) = Self::omega(freq, sample_rate);
        let alpha = sin / (2.0 * q.max(0.05));
        self.set((1.0 - cos) * 0.5, 1.0 - cos, (1.0 - cos) * 0.5, 1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    }

    pub fn set_high_pass(&mut self, freq: f32, q: f32, sample_rate: f32) {
        let (sin, cos) = Self::omega(freq, sample_rate);
        let alpha = sin / (2.0 * q.max(0.05));
        self.set((1.0 + cos) * 0.5, -(1.0 + cos), (1.0 + cos) * 0.5, 1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    }

    #[inline]
    pub fn tick(&mut self, input: f32) -> f32 {
        let out = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * out + self.z2;
        self.z2 = self.b2 * input - self.a2 * out;
        out
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// The Q of each biquad of a Butterworth filter made of `stages` of them in
/// a row (order `2 * stages`, so 12 dB an octave a stage): maximally flat
/// up to the corner, 3 dB down at it. For up to four stages; the unused
/// places hold the Q of a single stage.
pub fn butterworth_qs(stages: usize) -> [f32; 4] {
    let stages = stages.clamp(1, 4);
    let mut qs = [std::f32::consts::FRAC_1_SQRT_2; 4];
    if stages > 1 {
        for (index, q) in qs.iter_mut().take(stages).enumerate() {
            let angle = std::f64::consts::PI * (2 * index + 1) as f64 / (4 * stages) as f64;
            *q = (1.0 / (2.0 * angle.cos())) as f32;
        }
    }
    qs
}

/// One-pole low-pass, also usable as a high-pass via `input - low`.
#[derive(Copy, Clone, Default)]
pub struct OnePole {
    state: f32,
    coef: f32,
}

impl OnePole {
    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: f32) {
        self.coef = 1.0 - (-2.0 * PI * cutoff_hz.clamp(1.0, sample_rate * 0.49) / sample_rate).exp();
    }

    #[inline]
    pub fn low(&mut self, input: f32) -> f32 {
        self.state += (input - self.state) * self.coef;
        self.state
    }

    #[inline]
    pub fn high(&mut self, input: f32) -> f32 {
        input - self.low(input)
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }
}

/// Power-of-two ring buffer with fractional reads.
pub struct DelayLine {
    buffer: Vec<f32>,
    mask: usize,
    write: usize,
}

impl DelayLine {
    pub fn new() -> Self {
        Self { buffer: vec![0.0; 2], mask: 1, write: 0 }
    }

    /// Allocates room for at least `max_samples` of delay. Not realtime-safe.
    pub fn resize(&mut self, max_samples: usize) {
        let size = (max_samples + 4).next_power_of_two();
        self.buffer = vec![0.0; size];
        self.mask = size - 1;
        self.write = 0;
    }

    pub fn max_delay(&self) -> f32 {
        (self.buffer.len() - 3) as f32
    }

    #[inline]
    pub fn write(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write = (self.write + 1) & self.mask;
    }

    /// Reads `delay` samples behind the most recent write (>= 1).
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let delay = delay.clamp(1.0, self.max_delay());
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let a = self.buffer[self.write.wrapping_sub(whole) & self.mask];
        let b = self.buffer[self.write.wrapping_sub(whole + 1) & self.mask];
        a + (b - a) * frac
    }

    /// Reads at an integer delay (>= 1).
    #[inline]
    pub fn tap(&self, delay: usize) -> f32 {
        self.buffer[self.write.wrapping_sub(delay.max(1)) & self.mask]
    }

    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
    }
}

impl Default for DelayLine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_line_reads_back_what_was_written() {
        let mut line = DelayLine::new();
        line.resize(100);
        for i in 0..50 {
            line.write(i as f32);
        }
        assert_eq!(line.tap(1), 49.0);
        assert_eq!(line.tap(10), 40.0);
        assert_eq!(line.read(10.5), 39.5);
    }

    #[test]
    fn butterworth_stages_have_the_textbook_qs() {
        let close = |qs: [f32; 4], expected: &[f32]| qs.iter().zip(expected).all(|(q, e)| (q - e).abs() < 5e-4);
        assert!(close(butterworth_qs(1), &[std::f32::consts::FRAC_1_SQRT_2]));
        assert!(close(butterworth_qs(2), &[0.5412, 1.3066]));
        assert!(close(butterworth_qs(4), &[0.5098, 0.6013, 0.9000, 2.5629]));
        // A single stage is the same number the filters always used, to the bit.
        assert_eq!(butterworth_qs(1)[0], std::f32::consts::FRAC_1_SQRT_2);
    }

    #[test]
    fn pan_is_unity_at_center_and_constant_power() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        let (l, r) = pan_gains(-1.0);
        assert!(r.abs() < 1e-6 && (l - std::f32::consts::SQRT_2).abs() < 1e-6);
        for pan in [-0.7, -0.2, 0.4, 0.9] {
            let (l, r) = pan_gains(pan);
            assert!((l * l + r * r - 2.0).abs() < 1e-5);
        }
    }

    #[test]
    fn svf_low_pass_attenuates_highs() {
        let sr = 48_000.0;
        let mut filter = Svf::default();
        filter.set(500.0, 0.707, sr);
        let rms = |freq: f32, filter: &mut Svf| {
            filter.reset();
            let mut sum = 0.0;
            for i in 0..4800 {
                let x = (2.0 * PI * freq * i as f32 / sr).sin();
                let y = filter.tick(x).low;
                if i >= 2400 {
                    sum += y * y;
                }
            }
            (sum / 2400.0).sqrt()
        };
        assert!(rms(100.0, &mut filter) > 0.65);
        assert!(rms(5000.0, &mut filter) < 0.02);
    }
}
