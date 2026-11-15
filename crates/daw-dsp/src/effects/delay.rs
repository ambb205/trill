//! Delay: tempo-synced stereo / ping-pong echo with filtered feedback.

use crate::define_params;
use crate::params::{ParamValues, boolean, float};
use crate::util::{DelayLine, OnePole, Smooth, mix_gains};
use daw_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    Sync => boolean("sync", "Tempo Sync", true),
    TimeBeats => float("time_beats", "Synced Time (0.5 = 1/8 note, 0.75 = dotted 1/8, 1 = 1/4)", 0.0625, 4.0, 0.75, "beats"),
    TimeMs => float("time_ms", "Free Time (when sync is off)", 1.0, 2000.0, 350.0, "ms"),
    Feedback => float("feedback", "Feedback", 0.0, 0.95, 0.4, ""),
    PingPong => boolean("ping_pong", "Ping Pong", true),
    LowCut => float("low_cut", "Feedback Low Cut", 20.0, 2000.0, 200.0, "Hz"),
    HighCut => float("high_cut", "Feedback High Cut", 500.0, 20000.0, 6000.0, "Hz"),
    Mix => float("mix", "Mix (0.5 = equal dry and wet, 1 = wet only)", 0.0, 1.0, 0.3, ""),
]);

pub const DESCRIPTION: &str = "Echo that follows the song tempo. `time_beats` is the delay in quarter notes; \
`ping_pong` bounces repeats between left and right; the cut filters darken and thin each repeat.";

const MAX_SECONDS: f32 = 4.0;

pub struct Delay {
    params: ParamValues,
    sample_rate: f32,
    lines: [DelayLine; 2],
    low_cut: [OnePole; 2],
    high_cut: [OnePole; 2],
    time: Smooth,
    primed: bool,
    feedback: f32,
    ping_pong: bool,
    dry: f32,
    wet: f32,
}

impl Delay {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            lines: [DelayLine::new(), DelayLine::new()],
            low_cut: [OnePole::default(); 2],
            high_cut: [OnePole::default(); 2],
            time: Smooth::new(1.0),
            primed: false,
            feedback: 0.0,
            ping_pong: true,
            dry: 1.0,
            wet: 0.0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        self.feedback = p.get(P::Feedback as usize);
        self.ping_pong = p.on(P::PingPong as usize);
        for filter in &mut self.low_cut {
            filter.set_cutoff(p.get(P::LowCut as usize), sr);
        }
        for filter in &mut self.high_cut {
            filter.set_cutoff(p.get(P::HighCut as usize), sr);
        }
        (self.dry, self.wet) = mix_gains(p.get(P::Mix as usize));
        self.params.changed = false;
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
        let target = (seconds * sr).clamp(1.0, self.lines[0].max_delay());
        if !self.primed {
            self.time.snap(target);
            self.primed = true;
        }

        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);
            let delay = self.time.next(target);
            let tap_l = self.lines[0].read(delay);
            let tap_r = self.lines[1].read(delay);
            let back_l = self.low_cut[0].high(self.high_cut[0].low(tap_l)) * self.feedback;
            let back_r = self.low_cut[1].high(self.high_cut[1].low(tap_r)) * self.feedback;

            if self.ping_pong {
                self.lines[0].write((dry_l + dry_r) * 0.5 + back_r);
                self.lines[1].write(back_l);
            } else {
                self.lines[0].write(dry_l + back_l);
                self.lines[1].write(dry_r + back_r);
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
        self.primed = false;
    }
}
