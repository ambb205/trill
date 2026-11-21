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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The most channels of an input device that are taken.
pub const MAX_INPUT_CHANNELS: usize = 32;
/// How much of an input the ring holds: far more than the delay it is read
/// at, so that an engine which was kept from its work for a moment finds
/// what it missed.
const RING_SECONDS: f64 = 2.0;
/// How long a clock's time stamps count for when its line is drawn, in
/// seconds: each counts for less the older it is, and one this old for a
/// third of a new one. Long, because a real device's stamps are each half
/// a millisecond out one way or the other, and only many of them say how
/// its clock runs to the parts in a million that a pitch is steady by;
/// and short against how slowly a crystal drifts as it warms.
const MEMORY_SECONDS: f64 = 20.0;
/// How much a device's rate is trusted to be what it says before its time
/// stamps have had their say: what a clock five parts in ten thousand off
/// would come to against stamps that are a tenth of a millisecond out.
/// With a stamp or two the rate is what the device says; with a second's
/// worth it is what they say.
const TRUSTED: f64 = 0.05;
/// A time stamp is late or it is right, and never early: the engine's
/// own clock is read off the moments its device asks for a buffer, and a
/// thread is woken late by whatever else the machine is doing; an input's
/// buffers are stamped by a driver that may say when it handed one over
/// where it should say when it heard it. So of the stamps of each stretch
/// this long, in seconds, only the least late is shown to the line.
/// (Measured on a virtual cable under a machine at full load: its
/// capture stamps were up to nine milliseconds late and the callbacks up
/// to two; the least late of every quarter second lay on a line to within
/// a frame or two.)
const LEAST_LATE_OF: f64 = 0.25;
/// A time stamp that is this far from where the line has its frame, in
/// seconds, counts as no further: one buffer that was handed over late
/// does not turn the line. But a line that this many stamps in a row lie
/// that far to one side of is not their line: it is begun anew from them.
/// (The first buffer a device hands over is often stamped as if it did
/// not belong to the rest.)
const OUT_AT_MOST: f64 = 0.003;
const OUT_IN_A_ROW: i32 = 8;
/// How quickly the place that is read is brought back to where it is meant
/// to be, in radians a second: slowly, because the ratio moves for it.
const PLACE_FOLLOWS: f64 = 0.3;
/// How far a device's clock is taken to be from what it says it runs at,
/// at the most: five parts in a thousand, which no device is near.
const CLOCK_RANGE: f64 = 0.005;
/// How far from its place the reading may be before it is put there at
/// once, in seconds: an input that stopped and started, or an engine that
/// was away. Anything less is steered out.
const JUMP_SECONDS: f64 = 0.05;
/// A clock that is this far, in seconds, from where its line has it is
/// not the clock the line was drawn through: an input whose stamps have
/// fallen that far back, an engine whose device did not ask for that long.
/// The line is begun anew.
///
/// (A stamp that far *ahead* on an input is a late stamp and nothing else.
/// It was once taken for frames the device had not handed over, and
/// silence written for them: a buffer handed over twenty milliseconds late
/// then put everything after it twenty milliseconds late. Frames are
/// missing when the device says so: see [`Missing`].)
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
    /// Where the stream is at `ns`, which may be before the line's own
    /// moment.
    fn at(&self, ns: f64) -> f64 {
        self.frame + self.rate * (ns - self.ns as f64) / 1e9
    }
}

/// The line that fits a stream's time stamps best: every stamp it has
/// been shown, the newer the weightier ([`MEMORY_SECONDS`]), by least
/// squares. A clock that runs at a rate of its own is a line, so nothing
/// lags behind it however far its rate is from what it says; and what one
/// stamp is out by is spread over all of them.
///
/// It is kept as sums about the latest stamp, of how far each stamp's
/// frame is from where the rate the device says would have it: small
/// numbers, whatever the clock reads.
#[derive(Copy, Clone, Debug)]
struct Fit {
    nominal: f64,
    /// The latest stamp's moment, and the frame the device's own rate has
    /// there, counted on from the first stamp.
    ns: u64,
    base: f64,
    /// The stamps' weights; and, weighted, their times in seconds before
    /// `ns`, how far their frames are from `base`'s line, the squares of
    /// the times, and the times with the distances.
    weight: f64,
    time: f64,
    off: f64,
    time_time: f64,
    time_off: f64,
    /// How many stamps in a row have lain further from the line than a
    /// stamp may: counted up for those ahead of it and down for those
    /// behind.
    out: i32,
}

impl Fit {
    /// The fit of one stamp: the stream was at `frame` at `ns`, and runs
    /// at `nominal` frames a second for all anyone knows.
    fn begun(frame: f64, ns: u64, nominal: f64) -> Self {
        Self { nominal, ns, base: frame, weight: 1.0, time: 0.0, off: 0.0, time_time: 0.0, time_off: 0.0, out: 0 }
    }

    /// How far the stream is from `base`'s line at the latest stamp, and
    /// how much faster than `nominal` it runs, in frames a second.
    fn solve(&self) -> (f64, f64) {
        let (mean_time, mean_off) = (self.time / self.weight, self.off / self.weight);
        let spread = self.time_time - self.time * mean_time;
        let together = self.time_off - self.time * mean_off;
        let faster = (together / (spread + TRUSTED)).clamp(-CLOCK_RANGE * self.nominal, CLOCK_RANGE * self.nominal);
        (mean_off - faster * mean_time, faster)
    }

    /// The line as it is fitted now.
    fn line(&self) -> Line {
        let (off, faster) = self.solve();
        Line { frame: self.base + off, ns: self.ns, rate: self.nominal + faster }
    }

    /// The fit after it has been shown one more stamp, and whether it was
    /// begun anew with it: the stamps had left the line.
    fn shown(self, frame: f64, ns: u64) -> (Self, bool) {
        let seconds = ns.saturating_sub(self.ns) as f64 / 1e9;
        if seconds <= 0.0 {
            return (self, false);
        }
        // Every stamp is that much older, and counts for that much less.
        let kept = (-seconds / MEMORY_SECONDS).exp();
        let (weight, time, off) = (self.weight * kept, self.time * kept, self.off * kept);
        let (time_time, time_off) = (self.time_time * kept, self.time_off * kept);
        let moved = Self {
            nominal: self.nominal,
            ns,
            base: self.base + self.nominal * seconds,
            weight,
            time: time - seconds * weight,
            off,
            time_time: time_time - 2.0 * seconds * time + seconds * seconds * weight,
            time_off: time_off - seconds * off,
            out: self.out,
        };
        // The new stamp, no further from the line than a stamp may be.
        let (expected, _) = moved.solve();
        let reach = OUT_AT_MOST * self.nominal;
        let away = frame - moved.base - expected;
        // The line is shown the least late stamp of a stretch: one that is
        // this far out had no stamp near the line in all that time, which
        // is not lateness. The stream is not where the line has it (frames
        // went missing that nobody said, or its clock was set), and the
        // line is begun anew from where it is.
        if away.abs() > GAP_SECONDS * self.nominal {
            return (Self::begun(frame, ns, self.nominal), true);
        }
        let out = match away {
            away if away > reach => self.out.max(0) + 1,
            away if away < -reach => self.out.min(0) - 1,
            _ => 0,
        };
        if out.abs() >= OUT_IN_A_ROW {
            return (Self::begun(frame, ns, self.nominal), true);
        }
        let seen = expected + away.clamp(-reach, reach);
        (Self { weight: moved.weight + 1.0, off: moved.off + seen, out, ..moved }, false)
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
    /// Frames the device never handed over, which were written as silence
    /// before the engine came to read them; and how often the line was
    /// begun anew because the device's clock was nowhere near it.
    lost: AtomicU64,
    restarts: AtomicU32,
    /// Whether the line is through the least late stamp of a stretch: not
    /// through the first one the device gave, which may be as late as any
    /// and which the line is begun again without.
    settled: AtomicBool,
    /// The first frame the engine has yet to read: what lies before it
    /// was read, as what it was or as silence for what had not arrived.
    unread: AtomicU64,
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
            settled: AtomicBool::new(false),
            unread: AtomicU64::new(0),
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

    /// The highest level each channel has had since this was last asked
    /// (1 is full scale), and whether any of its samples was at full scale
    /// or over.
    pub fn take_levels(&self) -> Vec<(f32, bool)> {
        let peaks = self.peaks.iter().map(|peak| f32::from_bits(peak.swap(0, Ordering::Relaxed)));
        let clipped = self.clipped.iter().map(|count| count.swap(0, Ordering::Relaxed) > 0);
        peaks.zip(clipped).collect()
    }
}

/// The end of an input that its device's thread writes to. Realtime-safe:
/// it neither allocates nor waits.
pub struct Sink {
    link: Arc<Link>,
    /// The line through what it has been handed so far.
    fit: Option<Fit>,
    written: u64,
    /// Which stamps the line is shown.
    least_late: LeastLate,
    /// The stamp of the buffer before, believed or not.
    stamped: Option<u64>,
}

/// What a device says of a buffer it hands over, beyond its frames.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// When it heard the buffer's first frame, on the host's clock.
    /// Nothing where it gave no stamp, gave one it does not stand by, or
    /// gave one that cannot be (the zero of its counter, a moment that has
    /// yet to come): such a buffer's frames are taken and its stamp is not
    /// shown to the line.
    pub heard_ns: Option<u64>,
    /// Whether it handed over every frame since the buffer before.
    pub missing: Missing,
}

impl Said {
    /// A buffer the device says nothing of but when it heard it.
    pub fn at(heard_ns: u64) -> Self {
        Self { heard_ns: Some(heard_ns), missing: Missing::Nothing }
    }
}

/// Whether frames are missing before a buffer, by the device's own word:
/// the flag it sets on a buffer that does not follow the one before, and
/// its own count of the frames it has heard. Never by a time stamp alone:
/// a stamp is late whenever a driver or a thread was, and frames taken for
/// missing that were not put everything after them late.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Missing {
    /// None that it says.
    #[default]
    Nothing,
    /// So many, by its own count of frames.
    Frames(u64),
    /// Some, and it does not count them: as many as its time stamp has it
    /// ahead of what it handed over. (None, where the stamp is not
    /// believed: there is then nothing to tell how many by.)
    Uncounted,
}

/// Picks, of the stamps of each stretch of [`LEAST_LATE_OF`] seconds, the
/// one that is least late by the line as it stands, and shows the line
/// that one alone.
#[derive(Copy, Clone, Debug, Default)]
struct LeastLate {
    /// The least late so far in the stretch that is going by: how far its
    /// frame is ahead of where the line has its moment, its frame and its
    /// moment.
    best: Option<(f64, f64, u64)>,
    /// When the stretch began, and whether any has ended yet.
    since: u64,
    chosen_once: bool,
}

impl LeastLate {
    /// Takes the stamp "the stream was at `frame` at `ns`" and returns the
    /// line as it is from here on, and whether it was begun anew. Until
    /// the first stretch has gone by the line is through the first stamp,
    /// which may be as late as any; then it is begun again from the least
    /// late of that stretch, and shown one of each stretch after.
    fn shown(&mut self, fit: Option<Fit>, frame: f64, ns: u64, nominal: f64) -> (Fit, bool) {
        let Some(fit) = fit else {
            *self = Self { best: None, since: ns, chosen_once: false };
            return (Fit::begun(frame, ns, nominal), true);
        };
        let ahead = frame - fit.line().at(ns as f64);
        if self.best.is_none_or(|(most, ..)| ahead > most) {
            self.best = Some((ahead, frame, ns));
        }
        match self.best {
            Some((_, frame, at)) if ns.saturating_sub(self.since) as f64 >= LEAST_LATE_OF * 1e9 => {
                (self.best, self.since) = (None, ns);
                match std::mem::replace(&mut self.chosen_once, true) {
                    true => fit.shown(frame, at),
                    false => (Fit::begun(frame, at, nominal), true),
                }
            }
            _ => (fit, false),
        }
    }
}

/// The engine's end of an input, before a processor has been given it
/// ([`Processor::hear`](crate::Processor::hear)).
pub struct Source(pub(crate) Arc<Link>);

/// Opens the way in for an input that runs at `rate` with `channels`
/// channels, for whoever drives a processor by hand: what the input's
/// side writes to ([`Sink::push`]), and what the processor is given. An
/// engine does this itself when it opens a device.
pub fn pair(rate: f64, channels: usize) -> (Sink, Source) {
    let (sink, link) = link(rate, channels);
    (sink, Source(link))
}

/// Opens the way in for an input device that runs at `rate` with
/// `channels` channels: what its thread writes to, and what the engine
/// reads.
pub(crate) fn link(rate: f64, channels: usize) -> (Sink, Arc<Link>) {
    let link = Arc::new(Link::new(rate, channels));
    (Sink { link: link.clone(), fit: None, written: 0, least_late: LeastLate::default(), stamped: None }, link)
}

impl Sink {
    /// How many channels a frame has.
    pub fn channels(&self) -> usize {
        self.link.channels
    }

    /// Takes `frames` frames that the device has just handed over, the
    /// first of which it heard at `heard_ns` on the host's clock, and of
    /// which it says nothing else. `sample(frame, channel)` reads one
    /// sample of them.
    pub fn push(&mut self, heard_ns: u64, frames: usize, sample: impl FnMut(usize, usize) -> f32) {
        self.push_said(Said::at(heard_ns), frames, sample);
    }

    /// Takes `frames` frames that the device has just handed over, with
    /// what it `said` of them. `sample(frame, channel)` reads one sample
    /// of them.
    pub fn push_said(&mut self, said: Said, frames: usize, mut sample: impl FnMut(usize, usize) -> f32) {
        let link = &*self.link;
        let (rate, channels, mask) = (link.rate, link.channels, link.capacity - 1);
        // A buffer's first frame was heard after the first frame of the
        // buffer before: a stamp that says otherwise is not believed. (It
        // is held against the stamp before whether or not that one was
        // believed, so that one stamp that is far out costs one.)
        let heard = said.heard_ns.filter(|ns| self.stamped.is_none_or(|before| *ns > before));
        self.stamped = said.heard_ns.or(self.stamped);
        // Frames that never came, by the device's word: silence of their
        // length, so that what follows is where it belongs.
        let missing = match (said.missing, heard, self.fit) {
            (Missing::Nothing, ..) => 0,
            (Missing::Frames(frames), ..) => frames,
            (Missing::Uncounted, Some(ns), Some(fit)) => {
                (fit.line().at(ns as f64) - self.written as f64).round().max(0.0) as u64
            }
            (Missing::Uncounted, ..) => 0,
        };
        // (No device is believed that an hour went missing: a count that
        // says so is a count gone wrong, and must not run this one over.)
        let missing = missing.min((rate * 3600.0) as u64);
        if missing > 0 {
            for frame in 0..missing.min(link.capacity as u64) {
                let at = ((self.written + missing - 1 - frame) as usize & mask) * channels;
                for slot in &link.samples[at..at + channels] {
                    slot.store(0, Ordering::Relaxed);
                }
            }
            // Those the engine has read already it counted itself, as
            // frames that had not arrived.
            let unread = link.unread.load(Ordering::Relaxed).max(self.written);
            self.written += missing;
            link.lost.fetch_add(self.written.saturating_sub(unread), Ordering::Relaxed);
        }
        if let (Some(ns), Some(fit)) = (heard, self.fit) {
            // Where the clock has the device against how far it has got.
            let ahead = fit.line().at(ns as f64) - self.written as f64;
            let gap = (GAP_SECONDS * rate).max(2.0 * frames as f64);
            if ahead < -gap {
                // The device's clock is nowhere near where it was, by two
                // stamps running (the first of them went backwards, and
                // was not believed): it is followed from here.
                self.fit = None;
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
        // The line is shown the stamp, if it is one to believe; the frames
        // are there either way.
        let shown = heard.map(|ns| self.least_late.shown(self.fit, self.written as f64, ns, rate));
        if let Some((fit, anew)) = shown {
            if anew {
                // Whoever reads by the line reads by another one now.
                link.restarts.fetch_add(1, Ordering::Relaxed);
            }
            self.fit = Some(fit);
        }
        self.written += frames as u64;
        link.written.store(self.written, Ordering::Release);
        if let Some((fit, _)) = shown {
            link.publish(fit.line());
        }
        link.settled.store(self.fit.is_some() && self.least_late.chosen_once, Ordering::Relaxed);
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
    /// Frames the device never handed over, found out before the engine
    /// came to read them. They are silence where they would have been.
    pub lost: u64,
    /// Frames that had not arrived when the engine came to read them,
    /// late or never: the same.
    pub late: u64,
    /// How often the reading was put somewhere else at once, because the
    /// device's clock or the engine's had jumped.
    pub jumps: u32,
}

/// How far out, in seconds, the place of an input that is taken to share
/// the engine's clock has to be before that is doubted; for how long it
/// has to have been going further out without a break; and over how long
/// "further out than it was" looks back. See [`Reader::parts`].
const PARTED_BY: f64 = 0.00015;
const PARTED_FOR: f64 = 3.0;
const PARTED_LOOKS_BACK: f64 = 1.0;
/// A place that is on a whole frame and stays there ([`Reader::whole`])
/// is put again, while nothing is kept of the input, wherever the time
/// stamps have it this many frames or more from where it is; and it
/// counts as
/// well put, for what may be concluded from its being off later, once
/// the stamps have agreed with it for this many seconds. (The stamps an
/// input opens with can all be late together, on a machine that is busy
/// just then, and nothing draws such a place back afterwards: measured
/// through a cable, a place put by the first quarter second alone was
/// once a third of a millisecond out, and the two ends were then taken
/// for two clocks.)
const OUT_OF_PLACE: f64 = 1.0;
const WELL_PUT_AFTER: f64 = 1.0;

/// What a [`Reader`] counts, for whoever asks from another thread.
#[derive(Default)]
pub(crate) struct Counts {
    late: AtomicU64,
    jumps: AtomicU32,
    /// Set when an input that was taken to count time by the output's
    /// clock was found, by its time stamps, to have one of its own.
    parted: AtomicBool,
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
    engine: Option<Fit>,
    /// Frames made so far.
    made: u64,
    /// Where in the input's frames the next frame of the engine is read,
    /// how many of them go by for each, and what has been added to that
    /// to keep the place (frames a second).
    place: f64,
    ratio: f64,
    trim: f64,
    placed: bool,
    /// How often the input's line had been begun anew when the place was
    /// last put by it.
    line_number: u32,
    /// Which of the moments its buffers were asked for the engine's line
    /// is shown.
    least_late: LeastLate,
    /// Whether the input and the engine count time by one clock, and for
    /// how many seconds the place has been where it was last put.
    one_clock: bool,
    /// Whether they count it by one clock at one rate: a frame of the
    /// input is then a frame of the engine, and is handed across as it
    /// is. See [`Reader::begin`].
    whole: bool,
    /// For how long the time stamps have agreed with where such a place
    /// is, while nothing was kept of the input: see [`WELL_PUT_AFTER`].
    agreed_for: f64,
    /// Whether something keeps what is read: see [`Reader::keep`].
    kept: bool,
    /// What the place was off by a moment ago, and for how long it has
    /// been leaving where the time stamps have it: see [`Reader::parts`].
    off_before: f64,
    leaving_for: f64,
    placed_for: f64,
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
    /// the input heard `delay_frames` of its own frames before. With
    /// `one_clock` the input counts time by the clock the engine's device
    /// counts by: the ratio is then what the two rates say and never
    /// moves for long, and the time stamps only say where the two began.
    pub fn new(link: Arc<Link>, engine_rate: f64, delay_frames: u32, one_clock: bool) -> Self {
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
            line_number: 0,
            least_late: LeastLate::default(),
            one_clock,
            whole: one_clock && (link.rate - engine_rate).abs() < 1e-9,
            agreed_for: 0.0,
            kept: false,
            off_before: 0.0,
            leaving_for: 0.0,
            placed_for: 0.0,
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

    /// Whether the place that is read has been put by lines that are
    /// through the least late stamps of their first stretch, the input's
    /// and the engine's both. Until then it is where the first stamps had
    /// it, which may each be as late as any, and is put again at once
    /// when the lines are begun without them: a step, which a take must
    /// not have in it. Asked after [`Reader::begin`] for a block.
    pub fn settled(&self) -> bool {
        self.placed
            && self.least_late.chosen_once
            && self.link.settled.load(Ordering::Relaxed)
            && self.line_number == self.link.restarts.load(Ordering::Relaxed)
    }

    /// Says whether something keeps what is read from here on: a take
    /// writes it down, a measurement is made by it. While something does,
    /// a place that is on a whole frame stays where it is, so that what
    /// is kept has no step in it. (What is only heard, through a track
    /// that lets its input through, is not kept: a frame's step in it in
    /// the first seconds an input is open buys a place that is right for
    /// every take after.)
    pub fn keep(&mut self, kept: bool) {
        self.kept = kept;
    }

    /// Says which channels are read from here on, a bit each: the others
    /// cost nothing and are silence.
    pub fn want(&mut self, channels: u32) {
        self.wanted = channels;
    }

    /// The engine has been asked for `frames` frames at `now_ns`: the
    /// ratio the input is read at is set for them.
    ///
    /// An input that counts time by the engine's own clock, at the
    /// engine's rate, is not read between its frames at all: its place is
    /// put on a whole frame, and a frame of the engine is a frame of the
    /// input as the device gave it, to the bit. While nothing is kept of
    /// the input (no take is asked for, nothing measures: see
    /// [`Reader::keep`]) the place is put again wherever the time stamps
    /// come to have it a frame from where it is, which makes it the
    /// better for every stamp since. From the moment something is kept,
    /// it stays: the stamps then say nothing more but whether the two are
    /// one clock at all ([`Reader::parts`]), and no filter and no creeping
    /// place lies between a microphone and its file.
    pub fn begin(&mut self, now_ns: u64, frames: usize) {
        let made = self.made as f64;
        // An engine that was kept from making frames for a while (its
        // device did not ask) goes on counting from where it is: the time
        // it lost is not made up for a little at a time.
        let away = |fit: &Fit| (fit.line().at(now_ns as f64) - made).abs() > GAP_SECONDS * self.engine_rate;
        let known = self.engine.filter(|fit| !away(fit));
        let (fit, mut anew) = self.least_late.shown(known, made, now_ns, self.engine_rate);
        self.engine = Some(fit);
        let engine = fit.line();
        let Some(input) = self.link.line() else { return };
        // A line that was begun anew, on either side, is another line: the
        // place is put by it at once, not steered there with the pitch.
        let line_number = self.link.restarts.load(Ordering::Relaxed);
        anew |= std::mem::replace(&mut self.line_number, line_number) != line_number;
        // When this buffer was due by the engine's own steady count, which
        // is a little before the moment it was asked for: where the line
        // has its first frame.
        let due = engine.ns as f64 + (made - engine.frame) / engine.rate * 1e9;
        let meant = input.at(due - self.delay_ns as f64);
        let off = meant - self.place;
        if !self.placed || anew || off.abs() > JUMP_SECONDS * self.link.rate {
            if self.placed && off.abs() > 0.5 {
                self.counts.jumps.fetch_add(1, Ordering::Relaxed);
            }
            (self.place, self.trim, self.placed, self.placed_for) = (meant, 0.0, true, 0.0);
            (self.off_before, self.leaving_for) = (0.0, 0.0);
            self.ratio = match self.one_clock {
                true => self.link.rate / self.engine_rate,
                false => input.rate / engine.rate,
            };
            if self.whole {
                (self.place, self.ratio, self.agreed_for) = (meant.round(), 1.0, 0.0);
            }
        } else if self.whole && !self.kept {
            // Nothing is kept of it: the place is put where the stamps
            // have it by now, if that is somewhere else.
            let seconds = frames as f64 / self.engine_rate;
            self.placed_for += seconds;
            self.ratio = 1.0;
            if off.abs() >= OUT_OF_PLACE {
                self.place = meant.round();
                (self.off_before, self.leaving_for, self.agreed_for) = (0.0, 0.0, 0.0);
            } else {
                self.agreed_for += seconds;
            }
        } else if self.whole && !self.parts(off, frames) {
            // One clock at one rate: a frame for a frame, from where the
            // place was put.
            self.placed_for += frames as f64 / self.engine_rate;
            self.ratio = 1.0;
        } else if self.one_clock && !self.parts(off, frames) {
            // One clock: a frame of the one is a frame of the other, and
            // the place never drifts. Where the time stamps have it is
            // right on the whole and a little out each time, so the place
            // is drawn towards it by less the longer it has been there:
            // towards what they all say, not what the last one said.
            self.placed_for += frames as f64 / self.engine_rate;
            let patience = self.placed_for.clamp(2.0, 300.0);
            let nominal = self.link.rate / self.engine_rate;
            self.ratio = (nominal + off / patience / self.engine_rate).clamp(nominal * 0.9995, nominal * 1.0005);
        } else {
            let seconds = frames as f64 / self.engine_rate;
            // Quickly while the clocks are first being told apart, when
            // what the place is off by is real; slowly from then on, when
            // it is what one stamp or another was out by.
            self.placed_for += seconds;
            let follows = PLACE_FOLLOWS + 1.7 * (-self.placed_for / 2.0).exp();
            self.trim += follows * follows * off * seconds;
            let speed = input.rate + 2.0 * follows * off + self.trim;
            let nominal = self.link.rate / self.engine_rate;
            self.ratio =
                (speed / engine.rate).clamp(nominal * (1.0 - 2.0 * CLOCK_RANGE), nominal * (1.0 + 2.0 * CLOCK_RANGE));
        }
        // What the two clocks differ by, as their lines have them.
        let nominal = self.link.rate / self.engine_rate;
        let apart = (input.rate / engine.rate / nominal - 1.0) * 1e6;
        self.counts.off_ppm.store(apart.to_bits(), Ordering::Relaxed);
    }

    /// Whether an input that is taken to count time by the engine's clock
    /// has just been found not to: asked with every block while that is
    /// believed, with what the place is `off` by. From then on the ratio
    /// is followed, as for any two devices, from where the place is: no
    /// jump.
    ///
    /// One clock is a belief. Whoever said so (the system, by saying two
    /// endpoints are one piece of hardware) can be wrong, and then the
    /// place is drawn the same way again and again, and further each
    /// time. A place that is merely a little out is drawn back and comes
    /// nearer; one whose clock runs apart goes on leaving. So the belief
    /// is dropped when the place has been more than [`PARTED_BY`] out and
    /// further out than it was a moment before, for [`PARTED_FOR`]
    /// without a break.
    fn parts(&mut self, off: f64, frames: usize) -> bool {
        let seconds = frames as f64 / self.engine_rate;
        // What it was off by a moment ago, smoothed over a second: what
        // it is off by now is held against that.
        let before = self.off_before;
        self.off_before += (off - self.off_before) * (seconds / PARTED_LOOKS_BACK).min(1.0);
        let leaving =
            off.abs() > PARTED_BY * self.link.rate && off.signum() == before.signum() && off.abs() > before.abs();
        self.leaving_for = if leaving { self.leaving_for + seconds } else { 0.0 };
        // Where nothing draws the place (one rate, a frame for a frame),
        // what it is off by is what the two clocks have run apart by and
        // nothing else: twice as far out as may be doubted is not doubted.
        // (Of a place the stamps had agreed with before anything was kept
        // of it. One that was kept from the first may simply be out, and
        // is doubted only as any other is.)
        let well_put = self.whole && self.agreed_for >= WELL_PUT_AFTER;
        let far = well_put && off.abs() > 2.0 * PARTED_BY * self.link.rate;
        if self.leaving_for < PARTED_FOR && !far {
            return false;
        }
        (self.one_clock, self.whole) = (false, false);
        self.trim = 0.0;
        // What the place is off by is brought in as slowly as it is of an
        // input that has long been open: this one has, and its line is
        // not new.
        self.placed_for = self.placed_for.max(PARTED_FOR + 1.0);
        self.counts.parted.store(true, Ordering::Relaxed);
        true
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
        // A frame of the input for each of the engine's, and the place on
        // one: nothing is read between frames, and what is handed across
        // is what the device gave.
        let as_it_is = self.whole && self.ratio == 1.0 && first_place.fract() == 0.0;
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
            let start = start as usize;
            if as_it_is {
                // (The frame at the place is the one the filter would
                // have weighed by one, and every other by nothing.)
                for channel in 0..channels {
                    self.block[channel * MAX_BLOCK + index] = match self.wanted & (1 << channel) {
                        0 => 0.0,
                        _ => self.span[channel * self.span_frames + start + half - 1],
                    };
                }
                continue;
            }
            self.kernel.row(place - frame, &mut self.row);
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
        self.link.unread.store(self.place.max(0.0) as u64, Ordering::Relaxed);
    }

    /// What [`Reader::take`] last read of `channel`: the block's frames.
    /// Silence for a channel the device has not got.
    pub fn channel(&self, channel: usize) -> &[f32] {
        match channel < self.link.channels {
            true => &self.block[channel * MAX_BLOCK..(channel + 1) * MAX_BLOCK],
            false => &self.block[..0],
        }
    }
}

/// What a reader has counted, with what its input lost: asked from any
/// thread.
pub(crate) fn health(link: &Link, counts: &Counts) -> Health {
    Health {
        lost: link.lost.load(Ordering::Relaxed),
        late: counts.late.load(Ordering::Relaxed),
        jumps: counts.jumps.load(Ordering::Relaxed),
    }
}

/// Whether an input that was taken to share the engine's clock was found
/// to have one of its own: see [`Reader::parts`].
pub(crate) fn parted(counts: &Counts) -> bool {
    counts.parted.load(Ordering::Relaxed)
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
            let mut reader = Reader::new(link, nominal, delay, false);
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
        let mut reader = Reader::new(link.clone(), 48_000.0, 2000, false);
        reader.want(1);
        let ns = |frame: u64| (frame as f64 / 48_000.0 * 1e9).round() as u64;
        // A ramp that says which frame of the host's time it is. For ten
        // buffers the device hands nothing over, and time goes on.
        let mut out = Vec::new();
        for buffer in 0..400u64 {
            let first = buffer * 480;
            if !(100..110).contains(&buffer) {
                // The device says so with the first buffer after them, by
                // its own count.
                let missing = if buffer == 110 { Missing::Frames(4800) } else { Missing::Nothing };
                let said = Said { heard_ns: Some(ns(first)), missing };
                sink.push_said(said, 480, |index, _| ((first + index as u64) % 1000) as f32 / 1000.0);
            }
            reader.begin(ns(first + 480), 480);
            reader.take(480);
            out.extend_from_slice(&reader.channel(0)[..480]);
        }
        // Counted once each: by the engine where it came to read them
        // before anyone knew they were not coming, by the device's end
        // for the rest. (A few more for the frames whose filter reached
        // into them.)
        let health = health(&link, &reader.counts());
        assert!(health.lost > 0 && health.late > 0, "{health:?}");
        assert!((4800..4900).contains(&(health.lost + health.late)), "{health:?}");
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

    /// A device that hands a ramp over, a buffer of 480 frames a hundredth
    /// of a second, read by an engine 2000 frames later: `said` is what it
    /// says of buffer `n`, given what it would say of one like any other.
    /// Returns what the engine read, how the input did, and how often the
    /// line was begun anew.
    fn ramp_read(buffers: u64, said: impl Fn(u64, Said) -> Option<Said>) -> (Vec<f32>, Health, u32) {
        let (mut sink, link) = link(48_000.0, 1);
        let mut reader = Reader::new(link.clone(), 48_000.0, 2000, false);
        reader.want(1);
        let ns = |frame: u64| 1_000_000 + (frame as f64 / 48_000.0 * 1e9).round() as u64;
        let mut out = Vec::new();
        for buffer in 0..buffers {
            let first = buffer * 480;
            if let Some(said) = said(buffer, Said::at(ns(first))) {
                sink.push_said(said, 480, |index, _| ((first + index as u64) % 1000) as f32 / 1000.0);
            }
            reader.begin(ns(first + 480), 480);
            reader.take(480);
            out.extend_from_slice(&reader.channel(0)[..480]);
        }
        (out, health(&link, &reader.counts()), link.restarts.load(Ordering::Relaxed))
    }

    /// What frame `made` of the engine reads of the ramp, when nothing
    /// has moved it.
    fn ramp_at(made: usize) -> f32 {
        ((made as i64 + 480 - 2000).rem_euclid(1000)) as f32 / 1000.0
    }

    /// The first frame, after the first second, at which the engine read
    /// something other than the ramp where it belongs.
    fn off_the_ramp(out: &[f32]) -> Option<usize> {
        (48_000..out.len()).find(|made| {
            // (Not held to it near the ramp's own fall, which rings.)
            let on_the_ramp = (*made as i64 + 480 - 2000).rem_euclid(1000);
            (100..900).contains(&on_the_ramp) && (out[*made] - ramp_at(*made)).abs() > 2e-3
        })
    }

    #[test]
    fn a_stamp_that_is_late_is_a_late_stamp_and_not_frames_that_are_missing() {
        // A buffer handed over thirty milliseconds late, and stamped so:
        // more than the fiftieth of a second that a gap was once read
        // into. Every frame is there.
        let late = |buffer: u64, said: Said| {
            let by = if buffer == 300 { 30_000_000 } else { 0 };
            Some(Said { heard_ns: said.heard_ns.map(|ns| ns + by), ..said })
        };
        let (out, health, restarts) = ramp_read(600, late);
        assert_eq!((health.lost, health.late, health.jumps), (0, 0, 0), "nothing was missing: {health:?}");
        assert_eq!(off_the_ramp(&out), None, "and nothing was put anywhere else");
        // The line was begun once, and once again from the least late
        // stamp of its first quarter second: not a third time.
        assert_eq!(restarts, 2);
    }

    #[test]
    fn a_stamp_the_device_does_not_stand_by_or_that_cannot_be_is_not_believed() {
        // One buffer without a stamp, one whose stamp is a second before
        // the stamp of the buffer before it, and one stamped as if nothing
        // had yet been heard: each time the frames are taken and the line
        // is left as it is.
        let odd = |buffer: u64, said: Said| {
            let heard_ns = match buffer {
                200 => None,
                300 => said.heard_ns.map(|ns| ns - 1_000_000_000),
                400 => Some(1),
                _ => said.heard_ns,
            };
            Some(Said { heard_ns, ..said })
        };
        let (out, health, restarts) = ramp_read(600, odd);
        assert_eq!((health.lost, health.late, health.jumps), (0, 0, 0), "{health:?}");
        assert_eq!(off_the_ramp(&out), None);
        assert_eq!(restarts, 2, "the line was never begun anew for them");
    }

    #[test]
    fn frames_a_device_says_are_missing_without_counting_them_are_as_many_as_its_stamp_says() {
        // Ten buffers never come, and the device says only that something
        // is missing before the next.
        let gap = |buffer: u64, said: Said| match buffer {
            100..110 => None,
            110 => Some(Said { missing: Missing::Uncounted, ..said }),
            _ => Some(said),
        };
        let (out, health, _) = ramp_read(400, gap);
        assert!((4800..4900).contains(&(health.lost + health.late)), "{health:?}");
        for made in [20_000usize, 60_000, 150_000] {
            assert!((out[made] - ramp_at(made)).abs() < 2e-3, "frame {made}: {} for {}", out[made], ramp_at(made));
        }
        // The same buffers gone and nothing said of them: no silence is
        // made up for them by one stamp. But a whole quarter second with
        // no stamp near the line is not lateness: the line is begun anew
        // from where the device is, half a second after at the most.
        let unsaid = |buffer: u64, said: Said| (!(100..110).contains(&buffer)).then_some(said);
        let (out, health, restarts) = ramp_read(600, unsaid);
        assert_eq!(health.lost, 0, "nothing was written for them: {health:?}");
        assert_eq!(restarts, 3, "the line was begun anew, once");
        assert_eq!(health.jumps, 1, "{health:?}");
        let again = (1.1 + 0.6) * 48_000.0;
        let off = (again as usize..out.len()).find(|made| {
            let on_the_ramp = (*made as i64 + 480 - 2000).rem_euclid(1000);
            (100..900).contains(&on_the_ramp) && (out[*made] - ramp_at(*made)).abs() > 2e-3
        });
        assert_eq!(off, None, "and the ramp is where it belongs from then on");
    }

    #[test]
    fn the_place_is_settled_once_both_lines_are_through_their_least_late_stamps() {
        let (mut sink, link) = link(48_000.0, 1);
        let mut reader = Reader::new(link.clone(), 48_000.0, 2000, false);
        reader.want(1);
        let ns = |frame: u64| 1_000_000 + (frame as f64 / 48_000.0 * 1e9).round() as u64;
        let mut settled_at = None;
        let mut jumps_after = 0;
        for buffer in 0..200u64 {
            let first = buffer * 480;
            // The first stamp of all is five milliseconds late.
            let late = if buffer == 0 { 5_000_000 } else { 0 };
            sink.push(ns(first) + late, 480, |_, _| 0.25);
            reader.begin(ns(first + 480), 480);
            let jumps = reader.counts.jumps.load(Ordering::Relaxed);
            match settled_at {
                None if reader.settled() => settled_at = Some((buffer, jumps)),
                Some((_, then)) => jumps_after = jumps - then,
                None => {}
            }
            reader.take(480);
        }
        let (at, jumps_before) = settled_at.expect("it settles");
        // A quarter of a second, and the block it takes to be seen.
        assert!((25..=27).contains(&at), "settled at buffer {at}");
        assert_eq!(jumps_before, 1, "the place was put again, by the line that left the late stamp out");
        assert_eq!(jumps_after, 0, "and never again");
    }

    #[test]
    fn an_input_on_the_engines_own_clock_at_its_rate_is_handed_across_to_the_bit() {
        let (mut sink, link) = link(48_000.0, 1);
        let mut reader = Reader::new(link.clone(), 48_000.0, 2000, true);
        reader.want(1);
        let ns = |frame: u64| 1_000_000 + (frame as f64 / 48_000.0 * 1e9).round() as u64;
        // What it hears is noise, every frame of it another: a frame that
        // is not the one it should be, or is anything but itself, shows.
        let heard = |frame: u64| {
            let mut bits = frame.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
            bits ^= bits >> 29;
            bits = bits.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            bits ^= bits >> 32;
            (bits >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        };
        // Its buffers are stamped up to two milliseconds late, as buffers
        // are, and the engine is asked for its own up to a third of one.
        let late = |buffer: u64, most_ns: u64| buffer.wrapping_mul(2_654_435_761) % 1000 * most_ns / 1000;
        let mut out = Vec::new();
        for buffer in 0..3000u64 {
            let first = buffer * 480;
            // A take is asked for a second after the input was opened.
            reader.keep(buffer >= 100);
            sink.push(ns(first) + late(buffer, 2_000_000), 480, |index, _| heard(first + index as u64));
            reader.begin(ns(first + 480) + late(buffer + 7, 300_000), 480);
            reader.take(480);
            out.extend_from_slice(&reader.channel(0)[..480]);
        }
        // From the take's first frame, half a minute of it: frame n of the
        // engine is frame n - K of the input, for one K, and not a bit of
        // it is other than the device gave.
        let from = 48_000usize;
        let lag = (0..4000u64)
            .find(|lag| out[from].to_bits() == heard(from as u64 - lag).to_bits())
            .expect("a frame of the engine is a frame of the input");
        let other = (from..out.len()).find(|made| out[*made].to_bits() != heard(*made as u64 - lag).to_bits());
        assert_eq!(other, None, "the first frame that is not what the input gave, {lag} frames before");
        // And K is what the delay says, to what the least late of the
        // first stamps were out by.
        assert!((lag as i64 - (2000 - 480)).abs() <= 8, "read {lag} frames late");
        let health = health(&link, &reader.counts());
        assert_eq!((health.lost, health.late), (0, 0), "{health:?}");
        assert!(!parted(&reader.counts()), "and they go on being taken for one clock");
    }

    #[test]
    fn what_has_not_arrived_is_silence_and_is_counted() {
        let (mut sink, link) = link(48_000.0, 2);
        // Read with no delay at all: nothing can be there in time.
        let mut reader = Reader::new(link.clone(), 48_000.0, 0, false);
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
