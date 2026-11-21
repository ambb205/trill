//! Sound that comes in: what an input device hears, brought into the
//! engine's clock.
//!
//! A microphone is one device and the speakers are usually another, and no
//! two devices count time alike: a second of one is a few hundred
//! millionths longer than a second of the other, and they may not even run
//! at the same rate. Left alone that is a recording that drifts out of its
//! song, and a monitored voice that sooner or later runs dry or runs over.
//! So nothing in the engine ever sees an input's own samples. What it sees
//! is one frame of input for every frame it makes:
//!
//! * the input's thread ([`Sink`]) writes what it is handed into a ring and
//!   says, from the time stamp each buffer came with, how its frames lie
//!   on the host's clock (a line: a frame, a time, and frames a second);
//! * the engine's thread ([`Reader`]) reads the ring at a place that moves
//!   by a *ratio* for each frame it makes. The place it is meant to be at
//!   is where that line has the input a fixed time ago ([`Reader::delay`]),
//!   and the ratio is steered so that it stays there: gently, because a
//!   ratio that moves is a pitch that moves. Between the ring's frames it
//!   reads through a filter that keeps everything up to 20 kHz and leaves
//!   nothing else above the floor ([`Kernel`]).
//!
//! The delay is constant for as long as the input is open, which is what
//! lets a take be put where it was sung and a monitored voice be said to
//! be so many milliseconds late: both are sums of figures that do not
//! move. It is where the line has the input, not how full the ring is, so
//! it is right from the first frame, before the two clocks have been told
//! apart.
//!
//! Frames that did not arrive in time are silence of the right length and
//! are counted ([`Health`]): what comes after them is where it belongs.
//!
//! Nothing here allocates, locks or waits once it is made: the ring is
//! atomics, and the line crosses in a handful of them that are read again
//! if they were being written.

use crate::graph::MAX_BLOCK;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The most channels of an input device that are taken.
pub const MAX_INPUT_CHANNELS: usize = 32;
/// How much of an input the ring holds: far more than the delay it is read
/// at, so that an engine which was kept from its work for a moment finds
/// what it missed.
const RING_SECONDS: f64 = 2.0;
/// How quickly the lines follow the clocks they are of, in radians a
/// second: slow enough that a time stamp which is a little early or late
/// moves nothing that can be heard, quick enough that a clock which is off
/// is found within a few seconds.
const CLOCK_FOLLOWS: f64 = 2.0;
/// How quickly the place that is read is brought back to where it is meant
/// to be.
const PLACE_FOLLOWS: f64 = 1.5;
/// How far a device's clock is taken to be from what it says it runs at,
/// at the most: five parts in a thousand, which no device is near.
const CLOCK_RANGE: f64 = 0.005;
/// How far from its place the reading may be before it is put there at
/// once, in seconds: an input that stopped and started, or an engine that
/// was away. Anything less is steered out.
const JUMP_SECONDS: f64 = 0.05;
/// A buffer whose time stamp is this far ahead of the frames before it, in
/// seconds, was preceded by frames the device never handed over. They are
/// written as silence.
const GAP_SECONDS: f64 = 0.02;

/// How many places between two frames of the input the filter is kept at.
const PHASES: usize = 512;
/// How far under a sound whatever the filter lets through that is not the
/// sound lies, in decibels, by design.
const FLOOR_DB: f64 = 120.0;
/// The most taps the filter has, which is what an input at four times the
/// engine's rate asks for.
const MAX_TAPS: usize = 384;
/// The highest rate an input is read at against the engine's.
const MAX_RATIO: f64 = 8.0;

/// A line through the frames of a stream on the host's clock: at `ns` the
/// stream was at `frame`, and it moves by `rate` frames a second.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Line {
    frame: f64,
    ns: u64,
    rate: f64,
}

impl Line {
    /// The line after it has been shown that the stream was at `frame` at
    /// `ns`: moved towards that, a little of the way, so that what it says
    /// is steadier than what it is shown. `nominal` is the rate the stream
    /// says it runs at.
    fn shown(self, frame: f64, ns: u64, nominal: f64) -> Self {
        let seconds = ns.saturating_sub(self.ns) as f64 / 1e9;
        if seconds <= 0.0 {
            return self;
        }
        // A critically damped follower: how far it trusts what it is shown
        // depends on how long it has been since it was last shown anything.
        let kept = (-CLOCK_FOLLOWS * seconds).exp();
        let expected = self.frame + self.rate * seconds;
        let off = frame - expected;
        let rate = self.rate + (1.0 - kept) * (1.0 - kept) * off / seconds;
        Self {
            frame: expected + (1.0 - kept * kept) * off,
            ns,
            rate: rate.clamp(nominal * (1.0 - CLOCK_RANGE), nominal * (1.0 + CLOCK_RANGE)),
        }
    }

    /// Where the stream is at `ns`, which may be before the line's own
    /// moment.
    fn at(&self, ns: f64) -> f64 {
        self.frame + self.rate * (ns - self.ns as f64) / 1e9
    }
}

/// What the input's thread and the engine's share.
pub(crate) struct Link {
    /// The rate the device says it runs at, and how many channels it has.
    rate: f64,
    channels: usize,
    /// Frames the ring holds: a power of two.
    capacity: usize,
    /// The ring: every frame's channels side by side, as the bits of an
    /// `f32` each.
    samples: Box<[AtomicU32]>,
    /// How many frames have been written since the input was opened.
    written: AtomicU64,
    /// The line of the input's frames on the host's clock, behind a count
    /// that is odd while it is being written.
    turn: AtomicU64,
    line_frame: AtomicU64,
    line_ns: AtomicU64,
    line_rate: AtomicU64,
    /// Frames the device never handed over, which were written as silence;
    /// and how often the line was begun anew because the device's clock
    /// was nowhere near it.
    lost: AtomicU64,
    restarts: AtomicU32,
    /// The highest level each channel has had since it was last asked, as
    /// the bits of an `f32`, and how many of its samples were at full
    /// scale or over.
    peaks: Box<[AtomicU32]>,
    clipped: Box<[AtomicU32]>,
}

impl Link {
    fn new(rate: f64, channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_INPUT_CHANNELS);
        let capacity = ((rate * RING_SECONDS) as usize).max(4 * MAX_BLOCK).next_power_of_two();
        Self {
            rate,
            channels,
            capacity,
            samples: (0..capacity * channels).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            turn: AtomicU64::new(0),
            line_frame: AtomicU64::new(0),
            line_ns: AtomicU64::new(0),
            line_rate: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            restarts: AtomicU32::new(0),
            peaks: (0..channels).map(|_| AtomicU32::new(0)).collect(),
            clipped: (0..channels).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    fn publish(&self, line: Line) {
        self.turn.fetch_add(1, Ordering::SeqCst);
        self.line_frame.store(line.frame.to_bits(), Ordering::SeqCst);
        self.line_ns.store(line.ns, Ordering::SeqCst);
        self.line_rate.store(line.rate.to_bits(), Ordering::SeqCst);
        self.turn.fetch_add(1, Ordering::SeqCst);
    }

    /// The line as it was last said, or nothing where none has been said
    /// yet or it was being written every time it was looked at (which a
    /// few looks do not come to: the writing is three stores).
    fn line(&self) -> Option<Line> {
        for _ in 0..8 {
            let before = self.turn.load(Ordering::SeqCst);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let line = Line {
                frame: f64::from_bits(self.line_frame.load(Ordering::SeqCst)),
                ns: self.line_ns.load(Ordering::SeqCst),
                rate: f64::from_bits(self.line_rate.load(Ordering::SeqCst)),
            };
            if self.turn.load(Ordering::SeqCst) == before {
                return Some(line);
            }
        }
        None
    }

    /// The rate the device says it runs at.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The highest level each channel has had since this was last asked
    /// (1 is full scale), and whether any of its samples was at full scale
    /// or over.
    pub fn take_levels(&self) -> Vec<(f32, bool)> {
        let peaks = self.peaks.iter().map(|peak| f32::from_bits(peak.swap(0, Ordering::Relaxed)));
        let clipped = self.clipped.iter().map(|count| count.swap(0, Ordering::Relaxed) > 0);
        peaks.zip(clipped).collect()
    }

    /// How many frames the device never handed over, and how often its
    /// clock was begun anew.
    pub fn lost(&self) -> (u64, u32) {
        (self.lost.load(Ordering::Relaxed), self.restarts.load(Ordering::Relaxed))
    }
}

/// The end of an input that its device's thread writes to. Realtime-safe:
/// it neither allocates nor waits.
pub struct Sink {
    link: Arc<Link>,
    /// The line through what it has been handed so far.
    line: Option<Line>,
    written: u64,
}

/// Opens the way in for an input device that runs at `rate` with
/// `channels` channels: what its thread writes to, and what the engine
/// reads.
pub(crate) fn link(rate: f64, channels: usize) -> (Sink, Arc<Link>) {
    let link = Arc::new(Link::new(rate, channels));
    (Sink { link: link.clone(), line: None, written: 0 }, link)
}

impl Sink {
    /// How many channels a frame has.
    pub fn channels(&self) -> usize {
        self.link.channels
    }

    /// Takes `frames` frames that the device has just handed over, the
    /// first of which it heard at `heard_ns` on the host's clock.
    /// `sample(frame, channel)` reads one sample of them.
    pub fn push(&mut self, heard_ns: u64, frames: usize, mut sample: impl FnMut(usize, usize) -> f32) {
        let link = &*self.link;
        let (rate, channels, mask) = (link.rate, link.channels, link.capacity - 1);
        if let Some(line) = self.line {
            // Where the clock has the device against how far it has got.
            let ahead = line.at(heard_ns as f64) - self.written as f64;
            let gap = (GAP_SECONDS * rate).max(2.0 * frames as f64);
            if ahead > gap {
                // Frames that never came: silence of their length, so that
                // what follows is where it belongs.
                let missing = ahead.round() as u64;
                for frame in 0..missing.min(link.capacity as u64) {
                    let at = ((self.written + missing - 1 - frame) as usize & mask) * channels;
                    for slot in &link.samples[at..at + channels] {
                        slot.store(0, Ordering::Relaxed);
                    }
                }
                self.written += missing;
                link.lost.fetch_add(missing, Ordering::Relaxed);
            } else if ahead < -gap {
                // The device's clock is nowhere near where it was: it is
                // followed from here.
                self.line = None;
                link.restarts.fetch_add(1, Ordering::Relaxed);
            }
        }
        for frame in 0..frames {
            let at = ((self.written as usize + frame) & mask) * channels;
            for channel in 0..channels {
                let value = sample(frame, channel);
                let value = if value.is_finite() { value } else { 0.0 };
                link.samples[at + channel].store(value.to_bits(), Ordering::Relaxed);
                let level = value.abs();
                // (The bits of a float that is not negative are in its order.)
                link.peaks[channel].fetch_max(level.to_bits(), Ordering::Relaxed);
                if level >= 1.0 {
                    link.clipped[channel].fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let line = match self.line {
            Some(line) => line.shown(self.written as f64, heard_ns, rate),
            None => Line { frame: self.written as f64, ns: heard_ns, rate },
        };
        self.line = Some(line);
        self.written += frames as u64;
        link.written.store(self.written, Ordering::Release);
        link.publish(line);
    }
}

/// The filter the ring is read through: a sinc under a Kaiser window, at
/// [`PHASES`] places between two frames.
struct Kernel {
    taps: usize,
    /// `PHASES + 1` rows of `taps`: the weights of the frames around a
    /// place that is so many phases past a frame.
    rows: Vec<f32>,
}

/// The zeroth modified Bessel function of the first kind, which the Kaiser
/// window is made of.
fn bessel(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    let half = x / 2.0;
    for k in 1..64 {
        term *= (half / k as f64) * (half / k as f64);
        sum += term;
        if term < sum * 1e-18 {
            break;
        }
    }
    sum
}

impl Kernel {
    /// The filter for reading an input at `input_rate` into `engine_rate`.
    /// It passes what both can hold up to 20 kHz (less of a lower rate)
    /// and is at the floor from where anything would fold back under
    /// that.
    fn new(input_rate: f64, engine_rate: f64) -> Self {
        let nyquist = input_rate.min(engine_rate) / 2.0;
        let pass = 20_000.0f64.min(nyquist * 20_000.0 / 22_050.0);
        // The band between what is kept and what is gone, in radians a
        // frame of the input, decides how long the filter is.
        let width = 2.0 * std::f64::consts::PI * 2.0 * (nyquist - pass) / input_rate;
        let wanted = ((FLOOR_DB - 7.95) / (2.285 * width)).ceil() as usize;
        let taps = (wanted + wanted % 2).clamp(16, MAX_TAPS);
        let half = (taps / 2) as f64;
        let beta = 0.1102 * (FLOOR_DB - 8.7);
        let cutoff = nyquist / input_rate;
        let mut rows = vec![0.0f32; (PHASES + 1) * taps];
        for phase in 0..=PHASES {
            let fraction = phase as f64 / PHASES as f64;
            let row = &mut rows[phase * taps..(phase + 1) * taps];
            let mut weights = vec![0.0f64; taps];
            for (tap, weight) in weights.iter_mut().enumerate() {
                // How far the frame under this tap is from the place read.
                let away = tap as f64 - (half - 1.0) - fraction;
                let reach = away / half;
                if reach.abs() >= 1.0 {
                    continue;
                }
                let window = bessel(beta * (1.0 - reach * reach).sqrt()) / bessel(beta);
                let angle = 2.0 * std::f64::consts::PI * cutoff * away;
                let sinc = if angle.abs() < 1e-12 { 1.0 } else { angle.sin() / angle };
                *weight = 2.0 * cutoff * sinc * window;
            }
            // Every place passes what stands still exactly as it is.
            let sum: f64 = weights.iter().sum();
            for (kept, weight) in row.iter_mut().zip(&weights) {
                *kept = (weight / sum) as f32;
            }
        }
        Self { taps, rows }
    }

    /// The weights for a place `fraction` of the way from one frame to the
    /// next, into `row`.
    #[inline]
    fn row(&self, fraction: f64, row: &mut [f32]) {
        let place = (fraction * PHASES as f64).clamp(0.0, PHASES as f64 - 1e-9);
        let phase = place as usize;
        let between = (place - phase as f64) as f32;
        let here = &self.rows[phase * self.taps..(phase + 1) * self.taps];
        let next = &self.rows[(phase + 1) * self.taps..(phase + 2) * self.taps];
        for ((out, a), b) in row.iter_mut().zip(here).zip(next) {
            *out = a + (b - a) * between;
        }
    }
}

/// How an input has done since it was opened; see [`Reader::health`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    /// Frames the device never handed over. They are silence where they
    /// would have been.
    pub lost: u64,
    /// Frames that had not arrived when the engine came to read them: the
    /// same.
    pub late: u64,
    /// How often the reading was put somewhere else at once, because the
    /// device's clock or the engine's had jumped.
    pub jumps: u32,
}

/// What a [`Reader`] counts, for whoever asks from another thread.
#[derive(Default)]
pub(crate) struct Counts {
    late: AtomicU64,
    jumps: AtomicU32,
    /// The ratio the input is read at against what the two rates say, in
    /// parts a million, as the bits of an `f64`.
    off_ppm: AtomicU64,
}

/// The end of an input that the engine's thread reads: a frame of every
/// channel it is asked for, for each frame the engine makes. Realtime-safe
/// once it is made.
pub(crate) struct Reader {
    link: Arc<Link>,
    kernel: Kernel,
    engine_rate: f64,
    /// How long before the engine reads a frame the input heard it, in
    /// the engine's frames and in nanoseconds: see [`Reader::delay`].
    delay_frames: u32,
    delay_ns: u64,
    /// The engine's own frames on the host's clock, by the buffers it has
    /// been asked for.
    engine: Option<Line>,
    /// Frames made so far.
    made: u64,
    /// Where in the input's frames the next frame of the engine is read,
    /// how many of them go by for each, and what has been added to that
    /// to keep the place (frames a second).
    place: f64,
    ratio: f64,
    trim: f64,
    placed: bool,
    /// Which channels are read: a bit each.
    wanted: u32,
    /// Room for the frames of the input a block is read from, a channel
    /// after the other; for the weights of one place; and for the block.
    span: Vec<f32>,
    span_frames: usize,
    row: Vec<f32>,
    block: Vec<f32>,
    counts: Arc<Counts>,
}

impl Reader {
    /// A reader of `link` for an engine at `engine_rate`, which reads what
    /// the input heard `delay_frames` of its own frames before.
    pub fn new(link: Arc<Link>, engine_rate: f64, delay_frames: u32) -> Self {
        let kernel = Kernel::new(link.rate, engine_rate);
        let channels = link.channels;
        let span_frames =
            (MAX_BLOCK as f64 * MAX_RATIO.min(link.rate / engine_rate * 1.02)).ceil() as usize + kernel.taps + 4;
        Self {
            ratio: link.rate / engine_rate,
            row: vec![0.0; kernel.taps],
            kernel,
            engine_rate,
            delay_frames,
            delay_ns: (delay_frames as f64 * 1e9 / engine_rate).round() as u64,
            engine: None,
            made: 0,
            place: 0.0,
            trim: 0.0,
            placed: false,
            wanted: 0,
            span: vec![0.0; span_frames * channels],
            span_frames,
            block: vec![0.0; MAX_BLOCK * channels],
            counts: Arc::new(Counts::default()),
            link,
        }
    }

    /// How many frames of the input lie on either side of a place that is
    /// read: what the reading needs to have arrived beyond its place.
    pub fn reach(input_rate: f64, engine_rate: f64) -> usize {
        Kernel::new(input_rate, engine_rate).taps / 2
    }

    pub fn link(&self) -> &Arc<Link> {
        &self.link
    }

    /// How many of the engine's frames before it reads a frame the input
    /// heard it: constant for as long as the input is open. A take is put
    /// where it was sung by taking this off, and what the output takes to
    /// be heard; a monitored voice is this late, and that.
    pub fn delay(&self) -> u32 {
        self.delay_frames
    }

    pub fn counts(&self) -> Arc<Counts> {
        self.counts.clone()
    }

    /// Says which channels are read from here on, a bit each: the others
    /// cost nothing and are silence.
    pub fn want(&mut self, channels: u32) {
        self.wanted = channels;
    }

    /// The engine has been asked for `frames` frames at `now_ns`: the
    /// ratio the input is read at is set for them.
    pub fn begin(&mut self, now_ns: u64, frames: usize) {
        let made = self.made as f64;
        let engine = match self.engine {
            // An engine that was kept from making frames for a while (its
            // device did not ask) goes on counting from where it is: the
            // time it lost is not made up for a little at a time.
            Some(line) if (line.at(now_ns as f64) - made).abs() > GAP_SECONDS * self.engine_rate => {
                Line { frame: made, ns: now_ns, rate: line.rate }
            }
            Some(line) => line.shown(made, now_ns, self.engine_rate),
            None => Line { frame: made, ns: now_ns, rate: self.engine_rate },
        };
        self.engine = Some(engine);
        let Some(input) = self.link.line() else { return };
        // When this buffer was due by the engine's own steady count, which
        // is a little before or after the moment it was asked for.
        let due = now_ns as f64 + (made - engine.frame) / engine.rate * 1e9;
        let meant = input.at(due - self.delay_ns as f64);
        let off = meant - self.place;
        if !self.placed || off.abs() > JUMP_SECONDS * self.link.rate {
            if self.placed {
                self.counts.jumps.fetch_add(1, Ordering::Relaxed);
            }
            (self.place, self.trim, self.placed) = (meant, 0.0, true);
            self.ratio = input.rate / engine.rate;
        } else {
            let seconds = frames as f64 / self.engine_rate;
            self.trim += PLACE_FOLLOWS * PLACE_FOLLOWS * off * seconds;
            let speed = input.rate + 2.0 * PLACE_FOLLOWS * off + self.trim;
            let nominal = self.link.rate / self.engine_rate;
            self.ratio =
                (speed / engine.rate).clamp(nominal * (1.0 - 2.0 * CLOCK_RANGE), nominal * (1.0 + 2.0 * CLOCK_RANGE));
        }
        let nominal = self.link.rate / self.engine_rate;
        self.counts.off_ppm.store(((self.ratio / nominal - 1.0) * 1e6).to_bits(), Ordering::Relaxed);
    }

    /// Reads the next `frames` frames (a block's worth at the most) of
    /// every channel that is wanted. [`Reader::channel`] has them.
    pub fn take(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCK);
        let (channels, taps) = (self.link.channels, self.kernel.taps);
        let half = taps / 2;
        if !self.placed || self.wanted == 0 {
            self.block[..MAX_BLOCK * channels].fill(0.0);
            self.place += frames as f64 * self.ratio;
            self.made += frames as u64;
            return;
        }
        // The frames of the input the block is read from, out of the ring
        // into one stretch a channel: from the first frame under the
        // filter at the block's first place to the last at its last.
        let first_place = self.place;
        let last_place = self.place + (frames.max(1) - 1) as f64 * self.ratio;
        let from = first_place.floor() as i64 - (half as i64 - 1);
        let until = last_place.floor() as i64 + half as i64 + 1;
        let count = ((until - from).max(0) as usize).min(self.span_frames);
        let written = self.link.written.load(Ordering::Acquire) as i64;
        let (capacity, mask) = (self.link.capacity as i64, self.link.capacity - 1);
        for channel in 0..channels {
            let span = &mut self.span[channel * self.span_frames..][..count];
            if self.wanted & (1 << channel) == 0 {
                continue;
            }
            for (index, sample) in span.iter_mut().enumerate() {
                let frame = from + index as i64;
                // What has not arrived, or was written over long ago, is
                // silence.
                *sample = if frame < 0 || frame >= written || frame < written - capacity {
                    0.0
                } else {
                    f32::from_bits(
                        self.link.samples[(frame as usize & mask) * channels + channel].load(Ordering::Relaxed),
                    )
                };
            }
        }
        // Frames whose filter reaches past what has arrived are silence as
        // a whole, and counted: half a sound is worse than none.
        let mut late = 0u64;
        let ring_moved = self.link.written.load(Ordering::Acquire) as i64 - capacity;
        for index in 0..frames {
            let place = first_place + index as f64 * self.ratio;
            let frame = place.floor();
            let start = frame as i64 - (half as i64 - 1) - from;
            let reaches = frame as i64 + half as i64;
            let there = reaches < written && frame as i64 - (half as i64 - 1) >= ring_moved.max(0);
            let fits = start >= 0 && (start as usize + taps) <= count;
            if !(there && fits) {
                late += u64::from(reaches >= written || !fits);
                for channel in 0..channels {
                    self.block[channel * MAX_BLOCK + index] = 0.0;
                }
                continue;
            }
            self.kernel.row(place - frame, &mut self.row);
            let start = start as usize;
            for channel in 0..channels {
                if self.wanted & (1 << channel) == 0 {
                    self.block[channel * MAX_BLOCK + index] = 0.0;
                    continue;
                }
                let span = &self.span[channel * self.span_frames + start..][..taps];
                // In four sums, which a compiler makes one instruction of.
                let mut sums = [0.0f32; 4];
                for (weights, samples) in self.row.chunks_exact(4).zip(span.chunks_exact(4)) {
                    for lane in 0..4 {
                        sums[lane] += weights[lane] * samples[lane];
                    }
                }
                self.block[channel * MAX_BLOCK + index] = (sums[0] + sums[1]) + (sums[2] + sums[3]);
            }
        }
        if late > 0 {
            self.counts.late.fetch_add(late, Ordering::Relaxed);
        }
        self.place += frames as f64 * self.ratio;
        self.made += frames as u64;
    }

    /// What [`Reader::take`] last read of `channel`: the block's frames.
    /// Silence for a channel the device has not got.
    pub fn channel(&self, channel: usize) -> &[f32] {
        match channel < self.link.channels {
            true => &self.block[channel * MAX_BLOCK..(channel + 1) * MAX_BLOCK],
            false => &self.block[..0],
        }
    }

    pub fn channels(&self) -> usize {
        self.link.channels
    }
}

/// What a reader has counted, with what its input lost: asked from any
/// thread.
pub(crate) fn health(link: &Link, counts: &Counts) -> Health {
    Health {
        lost: link.lost.load(Ordering::Relaxed),
        late: counts.late.load(Ordering::Relaxed),
        jumps: counts.jumps.load(Ordering::Relaxed) + link.restarts.load(Ordering::Relaxed),
    }
}

/// How far the ratio an input is read at is from what the two rates say,
/// in parts a million: what the two devices' clocks differ by.
pub(crate) fn off_ppm(counts: &Counts) -> f64 {
    f64::from_bits(counts.off_ppm.load(Ordering::Relaxed))
}

/// How long before the engine reads a frame the input must have heard it,
/// in frames of the engine, for nothing to be late: what the device says
/// lies between its microphone and its buffers (`latency`, in its own
/// frames), a buffer of its own, a buffer of the engine's, the reach of
/// the filter, and something for buffers that are not handed over on the
/// dot.
pub(crate) fn delay_frames(input_rate: f64, buffer: u32, latency: u32, engine_rate: f64, engine_buffer: u32) -> u32 {
    let own = (buffer + latency) as f64 / input_rate;
    let engines = engine_buffer.max(1) as f64 / engine_rate;
    let reach = Reader::reach(input_rate, engine_rate) as f64 / input_rate;
    let spare = 0.003f64.max(0.5 * buffer as f64 / input_rate).max(0.5 * engines);
    ((own + engines + reach + spare) * engine_rate).ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An input device and an engine, each with a clock of its own, run
    /// against one another without a thread: `heard(channel, seconds)` is
    /// what the input hears at a moment of the host's clock.
    struct Bench<F: Fn(usize, f64) -> f32> {
        sink: Sink,
        reader: Reader,
        heard: F,
        /// The input: its true rate, its buffer, and when its next buffer
        /// is handed over.
        input_rate: f64,
        input_buffer: usize,
        input_frames: u64,
        /// The engine: its true rate, its buffer, and frames made.
        engine_rate: f64,
        engine_buffer: usize,
        engine_frames: u64,
        delay: u32,
    }

    impl<F: Fn(usize, f64) -> f32> Bench<F> {
        fn new(input: (f64, f64, usize), engine: (f64, f64, usize), channels: usize, heard: F) -> Self {
            let (nominal_in, ppm_in, input_buffer) = input;
            let (nominal, ppm, engine_buffer) = engine;
            let (sink, link) = link(nominal_in, channels);
            let delay = delay_frames(nominal_in, input_buffer as u32, 0, nominal, engine_buffer as u32);
            let mut reader = Reader::new(link, nominal, delay);
            reader.want(u32::MAX);
            Self {
                sink,
                reader,
                heard,
                input_rate: nominal_in * (1.0 + ppm_in / 1e6),
                input_buffer,
                input_frames: 0,
                engine_rate: nominal * (1.0 + ppm / 1e6),
                engine_buffer,
                engine_frames: 0,
                delay,
            }
        }

        /// The engine makes one buffer, and every channel's frames are
        /// handed to `made` a block at a time.
        fn buffer(&mut self, mut made: impl FnMut(&Reader, usize)) {
            let now = self.engine_frames as f64 / self.engine_rate;
            // What the input has handed over by now: a buffer is there
            // once its last frame has been heard.
            while (self.input_frames + self.input_buffer as u64) as f64 / self.input_rate <= now {
                let first = self.input_frames;
                let (rate, heard) = (self.input_rate, &self.heard);
                let heard_ns = (first as f64 / rate * 1e9).round() as u64;
                self.sink.push(heard_ns, self.input_buffer, |frame, channel| {
                    heard(channel, (first + frame as u64) as f64 / rate)
                });
                self.input_frames += self.input_buffer as u64;
            }
            self.reader.begin((now * 1e9).round() as u64, self.engine_buffer);
            let mut done = 0;
            while done < self.engine_buffer {
                let frames = (self.engine_buffer - done).min(MAX_BLOCK);
                self.reader.take(frames);
                made(&self.reader, frames);
                done += frames;
            }
            self.engine_frames += self.engine_buffer as u64;
        }

        /// The first channel of the next `seconds` of the engine's time.
        fn run(&mut self, seconds: f64) -> Vec<f32> {
            let mut out = Vec::new();
            let until = self.engine_frames as f64 + seconds * self.engine_rate;
            while (self.engine_frames as f64) < until {
                self.buffer(|reader, frames| out.extend_from_slice(&reader.channel(0)[..frames]));
            }
            out
        }
    }

    /// A transform of `re` and `im` in place, whose length is a power of
    /// two: in doubles, because what is looked for here lies under what
    /// single precision leaves of a sum this long.
    fn transform(re: &mut [f64], im: &mut [f64]) {
        let count = re.len();
        let mut j = 0;
        for i in 1..count {
            let mut bit = count >> 1;
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
        let mut size = 2;
        while size <= count {
            let angle = -2.0 * std::f64::consts::PI / size as f64;
            for start in (0..count).step_by(size) {
                for k in 0..size / 2 {
                    let (cos, sin) = ((angle * k as f64).cos(), (angle * k as f64).sin());
                    let (a, b) = (start + k, start + k + size / 2);
                    let (x, y) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                    (re[b], im[b]) = (re[a] - x, im[a] - y);
                    (re[a], im[a]) = (re[a] + x, im[a] + y);
                }
            }
            size *= 2;
        }
    }

    /// The level of a sine of `hz` in `samples` at `rate` (a power of two
    /// of them), and of all that is not within `near` hertz of it, in
    /// decibels: through a window whose skirts lie far under anything
    /// looked for here.
    fn sine_and_rest(samples: &[f32], rate: f64, hz: f64, near: f64) -> (f64, f64) {
        let count = samples.len();
        let beta = 26.0;
        let mut re: Vec<f64> = (0..count)
            .map(|n| {
                let reach = 2.0 * n as f64 / (count - 1) as f64 - 1.0;
                samples[n] as f64 * bessel(beta * (1.0 - reach * reach).max(0.0).sqrt()) / bessel(beta)
            })
            .collect();
        let mut im = vec![0.0; count];
        transform(&mut re, &mut im);
        let (mut inside, mut outside) = (0.0, 0.0);
        for bin in 1..count / 2 {
            let power = re[bin] * re[bin] + im[bin] * im[bin];
            let at = bin as f64 * rate / count as f64;
            if (at - hz).abs() <= near { inside += power } else { outside += power }
        }
        (10.0 * inside.log10(), 10.0 * outside.max(1e-300).log10())
    }

    fn sine(hz: f64) -> impl Fn(usize, f64) -> f32 {
        move |_, seconds| (0.5 * (2.0 * std::f64::consts::PI * hz * seconds).sin()) as f32
    }

    #[test]
    fn a_sine_comes_through_with_nothing_beside_it_a_hundred_decibels_down() {
        // At equal clocks, at 200 parts a million either way, and from
        // 44.1 kHz into 48 kHz.
        let cases = [
            (48_000.0, 0.0, "equal"),
            (48_000.0, 200.0, "fast"),
            (48_000.0, -200.0, "slow"),
            (44_100.0, 0.0, "44.1 kHz"),
        ];
        for (rate, ppm, name) in cases {
            let mut bench = Bench::new((rate, ppm, 441), (48_000.0, 0.0, 128), 1, sine(1000.0));
            // The first seconds are the clocks being told apart.
            bench.run(6.0);
            let out = bench.run(1.0);
            let out = &out[..32_768];
            // What the input hears is 1 kHz of the host's clock whatever
            // its own says: it is 1 kHz of the engine's.
            let (sine, rest) = sine_and_rest(out, 48_000.0, 1000.0, 20.0);
            assert!(rest < sine - 100.0, "{name}: what is not the sine is {:.1} dB under it", sine - rest);
        }
    }

    #[test]
    fn the_path_is_flat_to_twenty_kilohertz() {
        for (rate, name) in [(48_000.0, "48 kHz"), (44_100.0, "44.1 kHz"), (96_000.0, "96 kHz")] {
            let mut levels = Vec::new();
            for hz in [50.0, 1000.0, 5000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0] {
                let mut bench = Bench::new((rate, 0.0, 480), (48_000.0, 0.0, 256), 1, sine(hz));
                bench.run(1.0);
                let out = bench.run(0.25);
                let peak = out.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                // A sine's peak is found between frames only if it is
                // looked for: its level by what it sums to.
                let power: f64 = out.iter().map(|sample| (*sample as f64).powi(2)).sum::<f64>() / out.len() as f64;
                levels.push((hz, 10.0 * (power / 0.125).log10(), peak));
            }
            for (hz, db, _) in &levels {
                assert!(db.abs() < 0.1, "{name}: {hz} Hz is {db:.3} dB from where it went in");
            }
        }
    }

    #[test]
    fn what_is_heard_is_read_a_fixed_time_later_whatever_the_clocks_do() {
        // A short bump every half second of the host's clock: where each
        // comes out says how late the input is read.
        let bump = |_: usize, seconds: f64| {
            let nearest = (seconds * 2.0).round() / 2.0;
            let away = (seconds - nearest) * 48_000.0 / 6.0;
            (0.8 * (-away * away).exp()) as f32
        };
        for (ppm_in, ppm_out) in [(0.0, 0.0), (200.0, 0.0), (-200.0, 0.0), (150.0, -150.0)] {
            let mut bench = Bench::new((48_000.0, ppm_in, 480), (48_000.0, ppm_out, 128), 1, bump);
            let engine_rate = bench.engine_rate;
            let delay_seconds = bench.delay as f64 / 48_000.0;
            let out = bench.run(120.0);
            let mut worst: f64 = 0.0;
            for bump in 1..238 {
                let heard = bump as f64 * 0.5;
                // The frame the engine made `delay` after the bump was
                // heard, by the host's clock.
                let expected = (heard + delay_seconds) * engine_rate;
                let near = expected.round() as usize;
                let window = &out[near - 40..near + 40];
                let (top, _) = window
                    .iter()
                    .enumerate()
                    .fold((0, 0.0f32), |best, (index, sample)| if *sample > best.1 { (index, *sample) } else { best });
                // Between frames, by the three around the top.
                let (a, b, c) = (window[top - 1] as f64, window[top] as f64, window[top + 1] as f64);
                let found = (near - 40 + top) as f64 + 0.5 * (a - c) / (a - 2.0 * b + c);
                let off_ms = (found - expected) / 48.0;
                if heard > 2.0 {
                    worst = worst.max(off_ms.abs());
                }
                assert!(off_ms.abs() < 0.5, "{ppm_in}/{ppm_out} ppm: the bump at {heard} s is {off_ms:.3} ms off");
            }
            assert!(worst < 0.05, "{ppm_in}/{ppm_out} ppm: once the clocks are told apart, {worst:.4} ms at the most");
        }
    }

    #[test]
    fn frames_a_device_never_hands_over_are_silence_of_their_length() {
        let (mut sink, link) = link(48_000.0, 1);
        let mut reader = Reader::new(link.clone(), 48_000.0, 2000);
        reader.want(1);
        let ns = |frame: u64| (frame as f64 / 48_000.0 * 1e9).round() as u64;
        // A ramp that says which frame of the host's time it is. For ten
        // buffers the device hands nothing over, and time goes on.
        let mut out = Vec::new();
        for buffer in 0..400u64 {
            let first = buffer * 480;
            if !(100..110).contains(&buffer) {
                sink.push(ns(first), 480, |index, _| ((first + index as u64) % 1000) as f32 / 1000.0);
            }
            reader.begin(ns(first + 480), 480);
            reader.take(480);
            out.extend_from_slice(&reader.channel(0)[..480]);
        }
        assert_eq!(link.lost().0, 4800);
        // After the gap the ramp is where it would have been: frame n of
        // the engine reads frame n - 2000 + 480 of the device's count.
        let reads = |made: usize| ((made as i64 + 480 - 2000).rem_euclid(1000)) as f32 / 1000.0;
        for made in [20_000usize, 40_000, 60_000, 150_000] {
            assert!((out[made] - reads(made)).abs() < 1e-3, "frame {made}: {} for {}", out[made], reads(made));
        }
        // And the gap itself is silence.
        let gap = 48_000 + 2000 - 480;
        assert!(out[gap + 200..gap + 4600].iter().all(|sample| sample.abs() < 1e-3));
    }

    #[test]
    fn what_has_not_arrived_is_silence_and_is_counted() {
        let (mut sink, link) = link(48_000.0, 2);
        // Read with no delay at all: nothing can be there in time.
        let mut reader = Reader::new(link.clone(), 48_000.0, 0);
        reader.want(0b11);
        sink.push(0, 480, |_, _| 0.5);
        reader.begin(10_000_000, 480);
        reader.take(480);
        assert!(reader.channel(0)[..480].iter().all(|sample| *sample == 0.0));
        let health = health(&link, &reader.counts());
        assert_eq!(health.late, 480);
        let levels = link.take_levels();
        assert_eq!(levels, [(0.5, false), (0.5, false)]);
        assert_eq!(link.take_levels(), [(0.0, false), (0.0, false)], "a level is said once");
    }

    #[test]
    fn a_channel_nobody_wants_costs_nothing_and_is_silence() {
        let mut bench =
            Bench::new((48_000.0, 0.0, 480), (48_000.0, 0.0, 256), 3, |channel, _| channel as f32 * 0.25 + 0.1);
        bench.reader.want(0b101);
        let mut last = [0.0f32; 3];
        for _ in 0..200 {
            bench.buffer(|reader, frames| {
                for (channel, last) in last.iter_mut().enumerate() {
                    *last = reader.channel(channel)[frames - 1];
                }
            });
        }
        assert!((last[0] - 0.1).abs() < 1e-5 && last[1] == 0.0 && (last[2] - 0.6).abs() < 1e-5, "{last:?}");
    }
}
