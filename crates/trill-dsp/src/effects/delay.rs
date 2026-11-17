//! Delay: tempo-synced stereo / ping-pong echo, filtered on its way into
//! the delay line: what comes in, and every time it comes round.

use crate::define_params;
use crate::params::{ParamValues, boolean, choice, float};
use crate::util::{Biquad, DelayLine, OnePole, Smooth, mix_gains};
use std::f32::consts::FRAC_1_SQRT_2;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    Sync => boolean("sync", "Tempo Sync", true),
    TimeBeats => float("time_beats", "Synced Time (0.5 = 1/8 note, 0.75 = dotted 1/8, 1 = 1/4)", 0.0625, 4.0, 0.75, "beats"),
    TimeMs => float("time_ms", "Free Time (when sync is off)", 1.0, 2000.0, 350.0, "ms"),
    Feedback => float("feedback", "Feedback", 0.0, 0.95, 0.4, ""),
    PingPong => boolean("ping_pong", "Ping Pong (repeats alternate left, right)", true),
    LowCut => float("low_cut", "Low Cut (on what comes in and on every repeat; 20 = off)", 20.0, 2000.0, 200.0, "Hz"),
    HighCut => float(
        "high_cut",
        "High Cut (on what comes in and on every repeat; 20000 = off)",
        500.0,
        20000.0,
        6000.0,
        "Hz"
    ),
    Mix => float("mix", "Mix (0.5 = equal dry and wet, 1 = wet only)", 0.0, 1.0, 0.3, ""),
    Legacy => choice(
        "legacy",
        "Legacy (the echo of older songs. `format 1`: ping pong loses the feedback on every bounce, and the cuts work \
         on the feedback only. `formats 2-6`: the cuts are 6 dB an octave and take a little of what is between them)",
        LEGACY,
        0
    ),
]);

/// What `legacy` can say: the echo as it is, as songs of format 1 have it,
/// and as songs of formats 2 to 6 have it.
const LEGACY: &[&str] = &["off", "format 1", "formats 2-6"];
const FIRST: usize = 1;
const GENTLE: usize = 2;

pub const DESCRIPTION: &str = "Echo that follows the song tempo. `time_beats` is the delay in quarter notes. With \
`ping_pong` the repeats alternate left and right, `time_beats` apart, starting on the left: the left and the right \
repeat of a pair are equally loud, and each pair is `feedback` times the pair before (0.3 = 10.5 dB down a pair). \
Without it the left and the right side echo separately. `low_cut` and `high_cut` (12 dB an octave, flat between \
them) filter what comes in and each time round again, so every repeat is thinner and darker than the last, and what \
lies between the cuts comes back whole. On a return track set `mix` to 1.";

const MAX_SECONDS: f32 = 4.0;

pub struct Delay {
    params: ParamValues,
    sample_rate: f32,
    lines: [DelayLine; 2],
    /// The cuts, 12 dB an octave, and whether each is on: at the end of
    /// its range it is off.
    low_cut: [Biquad; 2],
    high_cut: [Biquad; 2],
    cut_lows: bool,
    cut_highs: bool,
    /// The cuts of formats 1 to 6: 6 dB an octave.
    gentle_low: [OnePole; 2],
    gentle_high: [OnePole; 2],
    time: Smooth,
    primed: bool,
    feedback: f32,
    ping_pong: bool,
    /// Which of [`LEGACY`].
    legacy: usize,
    dry: f32,
    wet: f32,
}

impl Delay {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            lines: [DelayLine::new(), DelayLine::new()],
            low_cut: [Biquad::default(); 2],
            high_cut: [Biquad::default(); 2],
            cut_lows: true,
            cut_highs: true,
            gentle_low: [OnePole::default(); 2],
            gentle_high: [OnePole::default(); 2],
            time: Smooth::new(1.0),
            primed: false,
            feedback: 0.0,
            ping_pong: true,
            legacy: 0,
            dry: 1.0,
            wet: 0.0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        self.feedback = p.get(P::Feedback as usize);
        self.ping_pong = p.on(P::PingPong as usize);
        self.legacy = p.index(P::Legacy as usize);
        let (low, high) = (p.get(P::LowCut as usize), p.get(P::HighCut as usize));
        (self.cut_lows, self.cut_highs) = (low > 20.5, high < 19_900.0);
        for side in 0..2 {
            self.low_cut[side].set_high_pass(low, FRAC_1_SQRT_2, sr);
            self.high_cut[side].set_low_pass(high, FRAC_1_SQRT_2, sr);
            self.gentle_low[side].set_cutoff(low, sr);
            self.gentle_high[side].set_cutoff(high, sr);
        }
        (self.dry, self.wet) = mix_gains(p.get(P::Mix as usize));
        self.params.changed = false;
    }

    /// What is on its way into a side's line, through the cuts.
    #[inline]
    fn cut(&mut self, side: usize, fed: f32) -> f32 {
        if self.legacy == GENTLE {
            self.gentle_low[side].high(self.gentle_high[side].low(fed))
        } else {
            let fed = if self.cut_highs { self.high_cut[side].tick(fed) } else { fed };
            if self.cut_lows { self.low_cut[side].tick(fed) } else { fed }
        }
    }
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for Delay {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        for line in &mut self.lines {
            line.resize((MAX_SECONDS * self.sample_rate) as usize + 8);
        }
        self.time.set_time(0.05, self.sample_rate);
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            self.refresh();
        }
        let sr = self.sample_rate;
        let seconds = if self.params.on(P::Sync as usize) {
            self.params.get(P::TimeBeats as usize) * 60.0 / info.tempo as f32
        } else {
            self.params.get(P::TimeMs as usize) * 0.001
        };
        let mut target = (seconds * sr).clamp(1.0, self.lines[0].max_delay());
        if self.legacy == 0 {
            // A whole number of frames: a repeat is then the sound itself, where one read between two
            // frames is their mean and loses a little of its top every time round.
            target = target.round();
        }
        if !self.primed {
            self.time.snap(target);
            self.primed = true;
        }

        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);
            let delay = self.time.next(target);
            let tap_l = self.lines[0].read(delay);
            let tap_r = self.lines[1].read(delay);
            if self.legacy == FIRST {
                // Format 1: only what is fed back is filtered, and a
                // ping-pong feeds back on its way to either side.
                let back_l = self.gentle_low[0].high(self.gentle_high[0].low(tap_l)) * self.feedback;
                let back_r = self.gentle_low[1].high(self.gentle_high[1].low(tap_r)) * self.feedback;
                if self.ping_pong {
                    self.lines[0].write((dry_l + dry_r) * 0.5 + back_r);
                    self.lines[1].write(back_l);
                } else {
                    self.lines[0].write(dry_l + back_l);
                    self.lines[1].write(dry_r + back_r);
                }
            } else if self.ping_pong {
                // Both sides at equal power into the left line; the left
                // line into the right one as it is, so a pair is one
                // level; and the right line back into the left, once a
                // pair.
                let fed = self.cut(0, (dry_l + dry_r) * FRAC_1_SQRT_2 + tap_r * self.feedback);
                self.lines[0].write(fed);
                self.lines[1].write(tap_l);
            } else {
                let fed_l = self.cut(0, dry_l + tap_l * self.feedback);
                let fed_r = self.cut(1, dry_r + tap_r * self.feedback);
                self.lines[0].write(fed_l);
                self.lines[1].write(fed_r);
            }

            left[i] = dry_l * self.dry + tap_l * self.wet;
            right[i] = dry_r * self.dry + tap_r * self.wet;
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        for filter in self.low_cut.iter_mut().chain(self.high_cut.iter_mut()) {
            filter.reset();
        }
        for filter in self.gentle_low.iter_mut().chain(self.gentle_high.iter_mut()) {
            filter.reset();
        }
        self.primed = false;
    }
}
