//! Sounds that are heard but are not part of the song: the browser's
//! audition voice and the metronome.
//!
//! They are mixed in after the master chain, so they are never coloured by
//! master effects, never metered as the mix, and never rendered: the offline
//! renderer simply does not turn them on.

use crate::graph::AudioData;
use std::f32::consts::TAU;
use std::sync::Arc;

/// Fade applied when an audition starts, so a sample cut mid-wave does not
/// click.
const FADE_IN_SECONDS: f32 = 0.002;
/// Fade applied when an audition is stopped or replaced.
const FADE_OUT_SECONDS: f32 = 0.008;
const CLICK_HZ: f32 = 1000.0;
const ACCENT_HZ: f32 = 1600.0;
const CLICK_LEVEL: f32 = 0.25;
const ACCENT_LEVEL: f32 = 0.4;
/// Time for a click to die away by 60 dB.
const CLICK_SECONDS: f32 = 0.045;
const SILENT: f32 = 1e-4;

/// Where the song is during a block, for placing metronome clicks.
#[derive(Copy, Clone)]
pub(crate) struct Beats {
    pub start: f64,
    pub per_frame: f64,
    /// Length of one count of the time signature in quarter notes (0.5 in
    /// 6/8) and how many of them make a bar.
    pub count: f64,
    pub counts_per_bar: u32,
}

struct Audition {
    audio: Arc<AudioData>,
    /// Position in the source, in source frames.
    position: f64,
    /// Source frames per output frame.
    step: f64,
    gain: f32,
    /// Current fade level, moving toward 1 while playing and 0 once released.
    fade: f32,
    released: bool,
}

pub(crate) struct Monitor {
    sample_rate: f64,
    audition: Option<Audition>,
    fade_in_step: f32,
    fade_out_step: f32,
    metronome: bool,
    click_phase: f32,
    click_step: f32,
    click_level: f32,
    click_decay: f32,
}

impl Monitor {
    pub fn new(sample_rate: f64) -> Self {
        let rate = sample_rate as f32;
        Self {
            sample_rate,
            audition: None,
            fade_in_step: 1.0 / (FADE_IN_SECONDS * rate),
            fade_out_step: 1.0 / (FADE_OUT_SECONDS * rate),
            metronome: false,
            click_phase: 0.0,
            click_step: 0.0,
            click_level: 0.0,
            click_decay: (-6.9078 / (CLICK_SECONDS * rate)).exp(),
        }
    }

    /// Starts auditioning `audio` from its beginning. Returns the buffer that
    /// was playing before, which the caller must free off the audio thread.
    pub fn start_audition(&mut self, audio: Arc<AudioData>, gain: f32) -> Option<Arc<AudioData>> {
        let step = if self.sample_rate > 0.0 { audio.sample_rate / self.sample_rate } else { 1.0 };
        let previous = self.audition.replace(Audition { audio, position: 0.0, step, gain, fade: 0.0, released: false });
        previous.map(|audition| audition.audio)
    }

    /// Fades the audition out. Its buffer becomes available through
    /// [`Monitor::take_finished`] once it is silent.
    pub fn stop_audition(&mut self) {
        if let Some(audition) = &mut self.audition {
            audition.released = true;
        }
    }

    /// The buffer of an audition that has ended, for freeing elsewhere.
    pub fn take_finished(&mut self) -> Option<Arc<AudioData>> {
        let finished = self.audition.as_ref().is_some_and(|audition| audition.released && audition.fade <= 0.0);
        if finished { self.audition.take().map(|audition| audition.audio) } else { None }
    }

    pub fn is_auditioning(&self) -> bool {
        self.audition.as_ref().is_some_and(|audition| !(audition.released && audition.fade <= 0.0))
    }

    pub fn set_metronome(&mut self, on: bool) {
        self.metronome = on;
    }

    /// Adds the monitor's sounds to a block. `beats` is given while the
    /// transport is running.
    pub fn mix(&mut self, left: &mut [f32], right: &mut [f32], beats: Option<Beats>) {
        self.mix_audition(left, right);
        match beats.filter(|_| self.metronome) {
            Some(beats) => self.mix_clicks(left, right, beats),
            // Let a click that has started ring out after a stop.
            None => self.ring_click(left, right, 0),
        }
    }

    fn mix_audition(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(audition) = &mut self.audition else { return };
        let audio = &audition.audio;
        let frames = audio.frames();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let index = audition.position as usize;
            if index + 1 >= frames {
                // Ran off the end: nothing more to play.
                audition.released = true;
                audition.fade = 0.0;
                break;
            }
            if audition.released {
                audition.fade -= self.fade_out_step;
                if audition.fade <= 0.0 {
                    audition.fade = 0.0;
                    break;
                }
            } else {
                audition.fade = (audition.fade + self.fade_in_step).min(1.0);
            }
            let fraction = (audition.position - index as f64) as f32;
            let level = audition.gain * audition.fade;
            *l += (audio.left[index] + (audio.left[index + 1] - audio.left[index]) * fraction) * level;
            *r += (audio.right[index] + (audio.right[index + 1] - audio.right[index]) * fraction) * level;
            audition.position += audition.step;
        }
    }

    fn mix_clicks(&mut self, left: &mut [f32], right: &mut [f32], beats: Beats) {
        let frames = left.len();
        // The first count at or after the start of the block. The small bias
        // keeps a count that lands exactly on the block start from slipping
        // into the previous block through rounding.
        let mut index = (beats.start / beats.count - 1e-9).ceil().max(0.0) as u64;
        let mut from = 0;
        loop {
            let offset = ((index as f64 * beats.count - beats.start) / beats.per_frame).round();
            if offset >= frames as f64 {
                break;
            }
            let offset = (offset.max(0.0) as usize).max(from);
            self.ring_click(left, right, from..offset);
            let accent = beats.counts_per_bar > 0 && index.is_multiple_of(beats.counts_per_bar as u64);
            self.click_phase = 0.0;
            self.click_step = if accent { ACCENT_HZ } else { CLICK_HZ } / self.sample_rate as f32;
            self.click_level = if accent { ACCENT_LEVEL } else { CLICK_LEVEL };
            from = offset;
            index += 1;
        }
        self.ring_click(left, right, from..frames);
    }

    /// Renders the click that is currently sounding into `range` (given as a
    /// range, or a start index meaning "to the end").
    fn ring_click(&mut self, left: &mut [f32], right: &mut [f32], range: impl ClickRange) {
        let range = range.resolve(left.len());
        if self.click_level < SILENT {
            return;
        }
        for (l, r) in left[range.clone()].iter_mut().zip(right[range].iter_mut()) {
            let sample = (self.click_phase * TAU).sin() * self.click_level;
            *l += sample;
            *r += sample;
            self.click_phase += self.click_step;
            self.click_phase -= self.click_phase.floor();
            self.click_level *= self.click_decay;
        }
    }
}

/// Lets `ring_click` take either a range or just a start.
trait ClickRange {
    fn resolve(self, len: usize) -> std::ops::Range<usize>;
}

impl ClickRange for std::ops::Range<usize> {
    fn resolve(self, len: usize) -> std::ops::Range<usize> {
        self.start.min(len)..self.end.min(len)
    }
}

impl ClickRange for usize {
    fn resolve(self, len: usize) -> std::ops::Range<usize> {
        self.min(len)..len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn sine(hz: f64, seconds: f64, rate: f64) -> Arc<AudioData> {
        let samples: Vec<f32> = (0..(seconds * rate) as usize)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / rate).sin() as f32)
            .collect();
        Arc::new(AudioData { sample_rate: rate, left: samples.clone(), right: samples })
    }

    fn run(monitor: &mut Monitor, frames: usize, beats: Option<Beats>) -> Vec<f32> {
        let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let mut done = 0;
        let mut beats = beats;
        while done < frames {
            let n = (frames - done).min(480);
            monitor.mix(&mut left[done..done + n], &mut right[done..done + n], beats);
            if let Some(b) = &mut beats {
                b.start += n as f64 * b.per_frame;
            }
            done += n;
        }
        left
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    fn crossings_per_second(samples: &[f32]) -> f64 {
        samples.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64 / (samples.len() as f64 / SR)
    }

    #[test]
    fn an_audition_plays_in_tune_at_any_file_rate_and_ends() {
        let mut monitor = Monitor::new(SR);
        // A 44.1 kHz file through a 48 kHz engine must still sound at 440 Hz.
        assert!(monitor.start_audition(sine(440.0, 0.5, 44_100.0), 0.5).is_none());
        assert!(monitor.is_auditioning());
        let out = run(&mut monitor, 48_000, None);
        assert!((peak(&out[4_800..19_200]) - 0.5).abs() < 0.01);
        assert!((crossings_per_second(&out[4_800..19_200]) - 440.0).abs() < 4.0);
        assert!(out[0].abs() < 0.01, "it fades in rather than starting at full level");
        assert_eq!(peak(&out[24_100..]), 0.0, "silent once the half-second file is over");
        assert!(!monitor.is_auditioning());
        assert!(monitor.take_finished().is_some(), "the buffer is handed back for freeing");
        assert!(monitor.take_finished().is_none());
    }

    #[test]
    fn stopping_and_replacing_an_audition_hand_buffers_back() {
        let mut monitor = Monitor::new(SR);
        let first = sine(220.0, 2.0, SR);
        monitor.start_audition(first.clone(), 1.0);
        run(&mut monitor, 4_800, None);

        let replaced = monitor.start_audition(sine(330.0, 2.0, SR), 1.0).expect("the old buffer comes back");
        assert!(Arc::ptr_eq(&replaced, &first));

        run(&mut monitor, 4_800, None);
        monitor.stop_audition();
        let tail = run(&mut monitor, 4_800, None);
        assert!(peak(&tail[..96]) > 0.1, "a stop fades out instead of cutting");
        assert_eq!(peak(&tail[480..]), 0.0, "and is silent within 10 ms");
        assert!(monitor.take_finished().is_some());
    }

    #[test]
    fn the_metronome_clicks_on_every_count_and_accents_the_bar() {
        let beats = |count: f64, counts_per_bar: u32| Beats {
            start: 0.0,
            per_frame: 120.0 / (60.0 * SR),
            count,
            counts_per_bar,
        };

        // 4/4 at 120 BPM: a click every 24000 frames, the first accented.
        let mut monitor = Monitor::new(SR);
        monitor.set_metronome(true);
        let out = run(&mut monitor, 5 * 24_000, Some(beats(1.0, 4)));
        for beat in 0..5 {
            let at = beat * 24_000;
            assert!(peak(&out[at..at + 480]) > 0.15, "no click on beat {beat}");
            assert!(peak(&out[at + 6_000..at + 23_900]) < 1e-3, "sound between clicks after beat {beat}");
        }
        let accent = crossings_per_second(&out[..480]);
        let plain = crossings_per_second(&out[24_000..24_480]);
        assert!(accent > plain * 1.3, "the downbeat is pitched higher: {accent} vs {plain}");
        assert!(crossings_per_second(&out[96_000..96_480]) > plain * 1.3, "bar two starts with an accent too");

        // 6/8: six eighth-note counts to the bar.
        let mut monitor = Monitor::new(SR);
        monitor.set_metronome(true);
        let out = run(&mut monitor, 24_000, Some(beats(0.5, 6)));
        assert!(peak(&out[12_000..12_480]) > 0.15, "eighth notes get a click");

        // Off, or stopped, it is silent.
        let mut monitor = Monitor::new(SR);
        assert_eq!(peak(&run(&mut monitor, 24_000, Some(beats(1.0, 4)))), 0.0);
        monitor.set_metronome(true);
        assert_eq!(peak(&run(&mut monitor, 24_000, None)), 0.0);
    }
}
