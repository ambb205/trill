//! What the built-in effects do to audio, proved on renders: a file on a
//! track, through the device, out of the renderer, and the samples
//! measured.
//!
//! The targets are the textbook's where there is one (a Butterworth filter
//! of the stated order, the cookbook's shelf, a second-order low-pass) and
//! otherwise what Ableton Live's devices were measured to do on the bench's
//! songs (`trill-ab`, reports of 16 and 17 November 2026): those numbers
//! are printed beside Trill's. Run with `--nocapture` to see the tables.

mod common;

use common::*;
use std::f64::consts::{PI, TAU};
use trill_core::audio::AudioBuffer;
use trill_core::project::AudioContent;
use trill_core::{ChainRef, DeviceId, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, Rendered, render};
use trill_media::WavFormat;

/// The tempo of every song here: a beat is half a second.
const TEMPO: f64 = 120.0;

fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

fn rms_db(samples: &[f32]) -> f64 {
    db(f64::from(rms(samples)))
}

/// `seconds` as a range of frames.
fn frames(seconds: std::ops::Range<f64>) -> std::ops::Range<usize> {
    (seconds.start * SR).round() as usize..(seconds.end * SR).round() as usize
}

/// Sines of whole numbers of Hz, each at `level`, for `seconds`.
fn tones(hz: &[f64], level: f64, seconds: f64) -> Vec<f32> {
    (0..(seconds * SR) as usize)
        .map(|n| hz.iter().map(|hz| level * (TAU * hz * n as f64 / SR).sin()).sum::<f64>() as f32)
        .collect()
}

/// The amplitude of the sine of `hz` in `samples`, which must hold a whole
/// number of its cycles: a second of audio does for any whole number of Hz,
/// and then no other such sine gets in the way.
fn amplitude(samples: &[f32], hz: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, sample) in samples.iter().enumerate() {
        let angle = TAU * hz * n as f64 / SR;
        re += f64::from(*sample) * angle.cos();
        im += f64::from(*sample) * angle.sin();
    }
    2.0 * (re * re + im * im).sqrt() / samples.len() as f64
}

/// Noise that is the same every time, between -1 and 1.
fn noise(frames: usize, mut seed: u32) -> Vec<f32> {
    (0..frames)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as f32 / 8_388_608.0 - 1.0
        })
        .collect()
}

/// A device to put on a track: its type and what is set on it.
type Effect<'a> = (&'a str, &'a [(&'a str, f64)]);

/// A song under construction: tracks that each play one file.
struct Song {
    project: Project,
    folder: tempfile::TempDir,
    files: usize,
}

impl Song {
    fn new() -> Self {
        let mut project = Project::new("effects");
        project.set_tempo(TEMPO).unwrap();
        Self { project, folder: folder(), files: 0 }
    }

    /// A track that plays `left` and `right` from the start of the song,
    /// through `effects` in order. Returns the track and its devices.
    fn track(&mut self, name: &str, left: &[f32], right: &[f32], effects: &[Effect]) -> (TrackId, Vec<DeviceId>) {
        self.files += 1;
        let path = file(&self.folder, &format!("{}.wav", self.files));
        let buffer = AudioBuffer { sample_rate: SR, left: left.to_vec(), right: right.to_vec() };
        trill_media::write_wav(&path, &buffer, WavFormat::Float32).unwrap();
        let track = self.project.add_track(name);
        let beats = left.len() as f64 / SR * TEMPO / 60.0;
        let content = AudioContent::new(path.display().to_string());
        self.project.add_audio_clip(track, name, 0.0, beats, content).unwrap();
        let devices = effects.iter().map(|effect| self.device(ChainRef::Track(track), effect)).collect();
        (track, devices)
    }

    fn device(&mut self, chain: ChainRef, (uri, params): &Effect) -> DeviceId {
        let id = self.project.add_device(chain, *uri, *uri, None).unwrap();
        let device = self.project.device_mut(id).unwrap();
        for (param, value) in *params {
            device.params.insert((*param).to_string(), *value);
        }
        id
    }

    fn render(&self, tail_seconds: f64) -> Rendered {
        self.render_with(RenderOptions { sample_rate: SR as u32, max_tail_seconds: tail_seconds, ..Default::default() })
    }

    fn render_with(&self, options: RenderOptions) -> Rendered {
        let out = render(&self.project, &BuiltinFactory, &options).unwrap();
        assert!(out.problems.is_empty(), "{:?}", out.problems);
        out
    }
}

/// One file through one chain of effects, rendered with `tail_seconds` of
/// what rings on.
fn through(left: &[f32], right: &[f32], effects: &[Effect], tail_seconds: f64) -> Rendered {
    let mut song = Song::new();
    song.track("Audio", left, right, effects);
    song.render(tail_seconds)
}

fn mono_through(signal: &[f32], effects: &[Effect], tail_seconds: f64) -> Rendered {
    through(signal, signal, effects, tail_seconds)
}

/// What a Butterworth filter of `stages` biquads at `corner` Hz does to
/// `hz`, in dB, as a filter made with the bilinear transform has it: a
/// low cut, or a high cut with `high_cut`.
fn butterworth_db(stages: i32, corner: f64, hz: f64, high_cut: bool) -> f64 {
    let ratio = (PI * corner / SR).tan() / (PI * hz / SR).tan();
    let ratio = if high_cut { 1.0 / ratio } else { ratio };
    -10.0 * (1.0 + ratio.powi(4 * stages)).log10()
}

// ---- EQ --------------------------------------------------------------------

#[test]
fn a_low_cut_is_a_butterworth_filter_of_12_24_or_48_db_an_octave() {
    // The frequencies of the report's table: a low cut at 180 Hz.
    let hz = [80.0, 100.0, 120.0, 140.0, 160.0, 180.0, 200.0, 230.0];
    let live_48 = [-51.7, -39.0, -26.9, -15.3, -8.6, -2.3, -0.7, -0.0];
    let signal = tones(&hz, 0.1, 4.0);
    println!("low cut at 180 Hz, gain in dB");
    println!("     Hz      12 dB    24 dB    48 dB   (Butterworth 48)  (Live 48)");
    let mut table = vec![Vec::new(); hz.len()];
    for (slope, stages) in [(0.0, 1), (1.0, 2), (2.0, 4)] {
        let out = mono_through(&signal, &[("builtin:eq", &[("low_cut", 180.0), ("low_cut_slope", slope)])], 0.0);
        // The third second: every filter has long settled.
        let settled = &out.left[frames(2.0..3.0)];
        for (index, hz) in hz.iter().enumerate() {
            let gain = db(amplitude(settled, *hz) / 0.1);
            let expected = butterworth_db(stages, 180.0, *hz, false);
            assert!(
                (gain - expected).abs() < 0.1,
                "{stages} stages at {hz} Hz: {gain:.2} dB, a Butterworth has {expected:.2}"
            );
            table[index].push(gain);
        }
        // 3 dB down at the corner whatever the slope.
        assert!((table[5].last().unwrap() + 3.01).abs() < 0.05);
    }
    for (index, hz) in hz.iter().enumerate() {
        let row = &table[index];
        let textbook = butterworth_db(4, 180.0, *hz, false);
        println!(
            "  {hz:>5}   {:>7.1}  {:>7.1}  {:>7.1}       {textbook:>7.1}       {:>7.1}",
            row[0], row[1], row[2], live_48[index]
        );
    }
    // What the steepest one is for: 39 dB down a little under an octave
    // below the corner, where the 12 dB cut the EQ had before leaves all
    // but 10 dB. Live's was measured at -39.0 there.
    assert!(table[1][2] < -40.0 && table[1][0] > -11.0, "{:?}", table[1]);
}

#[test]
fn a_high_cut_has_the_same_slopes() {
    let hz = [1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 8000.0];
    let signal = tones(&hz, 0.1, 4.0);
    for (slope, stages) in [(0.0, 1), (1.0, 2), (2.0, 4)] {
        let out = mono_through(&signal, &[("builtin:eq", &[("high_cut", 4000.0), ("high_cut_slope", slope)])], 0.0);
        let settled = &out.left[frames(2.0..3.0)];
        for hz in hz {
            let gain = db(amplitude(settled, hz) / 0.1);
            let expected = butterworth_db(stages, 4000.0, hz, true);
            assert!((gain - expected).abs() < 0.1, "{stages} stages at {hz} Hz: {gain:.2} dB, wanted {expected:.2}");
        }
    }
}

#[test]
fn bells_and_shelves_reach_the_bottom_of_the_range_and_a_shelf_has_a_q() {
    // A bell at 50 Hz, an octave under where the first bell used to start
    // (100 Hz), and the second one at 30 Hz (it started at 200 Hz).
    let hz = [30.0, 50.0, 400.0, 3200.0];
    let signal = tones(&hz, 0.1, 4.0);
    for (bell, at, gain_db) in [("mid1", 50.0, 6.0), ("mid2", 30.0, -9.0)] {
        let names = [format!("{bell}_freq"), format!("{bell}_gain"), format!("{bell}_q")];
        let set = [(names[0].as_str(), at), (names[1].as_str(), gain_db), (names[2].as_str(), 6.0)];
        let out = mono_through(&signal, &[("builtin:eq", &set)], 0.0);
        let gain = |hz: f64| db(amplitude(&out.left[frames(2.0..3.0)], hz) / 0.1);
        assert!((gain(at) - gain_db).abs() < 0.1, "the bell at {at} Hz: {:.2} dB", gain(at));
        assert!(gain(400.0).abs() < 0.1 && gain(3200.0).abs() < 0.05, "and nothing else: {:.2} dB", gain(400.0));
    }

    // A low shelf of +9 dB at 200 Hz: the cookbook's numbers at a Q of
    // 0.707 (the plain shelf, which is what the EQ had) and at 1.5, where
    // it overshoots under the corner and dips over it.
    let hz = [25.0, 140.0, 200.0, 320.0, 3200.0];
    let signal = tones(&hz, 0.05, 4.0);
    for (q, expected) in [(None, [9.00, 7.11, 4.50, 1.32, 0.00]), (Some(1.5), [9.11, 11.01, 4.50, -2.28, -0.03])] {
        let mut shelf = vec![("low_freq", 200.0), ("low_gain", 9.0)];
        shelf.extend(q.map(|q| ("low_q", q)));
        let out = mono_through(&signal, &[("builtin:eq", &shelf)], 0.0);
        for (hz, expected) in hz.iter().zip(expected) {
            let gain = db(amplitude(&out.left[frames(2.0..3.0)], *hz) / 0.05);
            assert!(
                (gain - expected).abs() < 0.05,
                "a shelf of Q {q:?} at {hz} Hz: {gain:.2} dB, wanted {expected:.2}"
            );
        }
    }
    // And a high shelf: 6 dB off the top from 4 kHz, with a Q of 1.5.
    let hz = [1000.0, 3000.0, 4000.0, 6000.0, 16000.0];
    let shelf: &[(&str, f64)] = &[("high_freq", 4000.0), ("high_gain", -6.0), ("high_q", 1.5)];
    let out = mono_through(&tones(&hz, 0.05, 4.0), &[("builtin:eq", shelf)], 0.0);
    for (hz, expected) in hz.iter().zip([0.29, 1.40, -3.00, -7.64, -6.11]) {
        let gain = db(amplitude(&out.left[frames(2.0..3.0)], *hz) / 0.05);
        assert!((gain - expected).abs() < 0.05, "the high shelf at {hz} Hz: {gain:.2} dB, wanted {expected:.2}");
    }
}

// ---- filter ----------------------------------------------------------------

/// What a second-order low-pass of `q` at `cutoff` does to `hz`, in dB.
fn low_pass_db(q: f64, cutoff: f64, hz: f64) -> f64 {
    let x = (PI * hz / SR).tan() / (PI * cutoff / SR).tan();
    -10.0 * ((1.0 - x * x).powi(2) + (x / q).powi(2)).log10()
}

#[test]
fn the_filter_falls_by_12_or_by_24_db_an_octave() {
    // A low-pass shut down to 700 Hz, where the song's sweep starts, at
    // the resonance the song has: one, two and three octaves up.
    let hz = [350.0, 700.0, 1400.0, 2800.0, 5600.0];
    let signal = tones(&hz, 0.1, 4.0);
    let resonance = 0.12;
    let q = 0.5 * 40f64.powf(resonance);
    let set = |slope: f64| [("type", 0.0), ("cutoff", 700.0), ("resonance", resonance), ("slope", slope)];
    println!("low-pass at 700 Hz, resonance 0.12, gain in dB");
    println!("     Hz     12 dB    24 dB");
    let (gentle, steep) = (
        mono_through(&signal, &[("builtin:filter", &set(0.0))], 0.0),
        mono_through(&signal, &[("builtin:filter", &set(1.0))], 0.0),
    );
    for hz in hz {
        let at = |out: &Rendered| db(amplitude(&out.left[frames(2.0..3.0)], hz) / 0.1);
        let (one, two) = (at(&gentle), at(&steep));
        println!("  {hz:>5}   {one:>7.1}  {two:>7.1}");
        // One stage of the Q the resonance stands for; two stages, each of
        // 0.8 times its root.
        let expected = (low_pass_db(q, 700.0, hz), 2.0 * low_pass_db(0.8 * q.sqrt(), 700.0, hz));
        assert!((one - expected.0).abs() < 0.15, "12 dB at {hz} Hz: {one:.2}, wanted {:.2}", expected.0);
        assert!((two - expected.1).abs() < 0.15, "24 dB at {hz} Hz: {two:.2}, wanted {:.2}", expected.1);
    }
    // Two octaves over the cutoff: 24 dB down, and 48. While the song's
    // filter was shut, Live's let 21.5 dB less through between 2.5 and
    // 5 kHz than the 12 dB filter Trill had.
    let at = |out: &Rendered, hz: f64| db(amplitude(&out.left[frames(2.0..3.0)], hz) / 0.1);
    assert!((at(&gentle, 2800.0) + 24.0).abs() < 1.0 && (at(&steep, 2800.0) + 48.0).abs() < 1.0);
    // At that resonance the steep one has no peak: 6 dB down at the cutoff.
    assert!((at(&steep, 700.0) + 6.0).abs() < 0.2, "{:.2}", at(&steep, 700.0));
}

#[test]
fn the_steep_filter_is_swept_by_automation_like_the_gentle_one() {
    use trill_core::project::{AutomationPoint, AutomationTarget};
    // A tone three octaves over where the sweep starts: let through as the
    // cutoff comes up to it, a beat later at 24 dB than at 12.
    let signal = tones(&[4000.0], 0.2, 4.0);
    let level = |slope: f64, seconds: std::ops::Range<f64>| {
        let mut song = Song::new();
        let (_, devices) =
            song.track("Audio", &signal, &signal, &[("builtin:filter", &[("slope", slope), ("resonance", 0.0)])]);
        let target = AutomationTarget::DeviceParam { device: devices[0], param: "cutoff".into() };
        let points = vec![
            AutomationPoint::new(0.0, 500.0),
            AutomationPoint::new(2.0, 500.0),
            AutomationPoint::new(8.0, 16_000.0),
        ];
        song.project.set_automation(target, points).unwrap();
        rms_db(&song.render(0.0).left[frames(seconds)])
    };
    let (early, late) = (0.3..0.7, 3.6..3.8);
    assert!(level(1.0, early.clone()) < level(0.0, early.clone()) - 20.0, "shut, the steep one lets far less through");
    assert!(level(1.0, early) < -60.0);
    assert!((level(1.0, late.clone()) - level(0.0, late)).abs() < 1.0, "open, they are the same");
}

// ---- delay -----------------------------------------------------------------

/// A burst of a 1 kHz tone, 20 ms long, a tenth of a second into `seconds`
/// of silence.
fn burst(seconds: f64, level: f64) -> Vec<f32> {
    let mut signal = vec![0.0f32; (seconds * SR) as usize];
    for (n, sample) in signal[frames(0.1..0.12)].iter_mut().enumerate() {
        // Faded at both ends, so that it is a burst of one tone and not a click.
        let fade = (n.min(959 - n) as f64 / 96.0).min(1.0);
        *sample = (level * fade * (TAU * 1000.0 * n as f64 / SR).sin()) as f32;
    }
    signal
}

/// The level of each of the first `count` repeats of [`burst`], a quarter
/// of a second apart, on the left and on the right, in dB under the burst.
fn repeats(out: &Rendered, burst_rms: f64, count: usize) -> Vec<(f64, f64)> {
    (1..=count)
        .map(|n| {
            let window = frames(0.1 + 0.25 * n as f64..0.12 + 0.25 * n as f64);
            let side = |samples: &[f32]| db(f64::from(rms(&samples[window.clone()])) / burst_rms);
            (side(&out.left), side(&out.right))
        })
        .collect()
}

const OPEN: [(&str, f64); 2] = [("low_cut", 20.0), ("high_cut", 20_000.0)];

#[test]
fn a_ping_pong_plays_the_left_and_the_right_repeat_of_a_pair_at_one_level_and_feeds_back_once_a_pair() {
    let signal = burst(3.0, 0.5);
    let level = f64::from(rms(&signal[frames(0.1..0.12)]));
    // An eighth note at 120 BPM is a quarter of a second.
    let mut set = vec![("time_beats", 0.5), ("feedback", 0.3), ("ping_pong", 1.0), ("mix", 1.0)];
    set.extend(OPEN);
    let found = repeats(&mono_through(&signal, &[("builtin:delay", &set)], 2.0), level, 6);
    println!("ping-pong, feedback 0.3, repeats in dB under the sound that went in (left, right)");
    for (index, (left, right)) in found.iter().enumerate() {
        println!("  {}  {left:>7.1}  {right:>7.1}", index + 1);
    }
    println!(
        "  Live, with its filter at 318 Hz to 10.2 kHz: -18.3 L, -18.3 R, -32.0 L, -30.6 R (a guitar sent at -20)"
    );
    const SILENT: f64 = -90.0;
    // Both sides of the sound go in at equal power, 3 dB over one of them,
    // and come out on the left, then on the right, at the same level.
    let pair = [3.0, 3.0, 3.0 + db(0.3), 3.0 + db(0.3), 3.0 + db(0.09), 3.0 + db(0.09)];
    for (index, ((left, right), expected)) in found.iter().zip(pair).enumerate() {
        let (heard, other) = if index % 2 == 0 { (left, right) } else { (right, left) };
        assert!((heard - expected).abs() < 0.2, "repeat {}: {heard:.2} dB, wanted {expected:.2}", index + 1);
        assert!(*other < SILENT, "repeat {} is on one side only: {other:.1} dB on the other", index + 1);
    }

    // The echo of songs saved before: half as loud to begin with, and the
    // feedback taken off on every bounce.
    set.push(("legacy", 1.0));
    let old = repeats(&mono_through(&signal, &[("builtin:delay", &set)], 2.0), level, 4);
    for (index, ((left, right), expected)) in old.iter().zip([0.0, db(0.3), db(0.09), db(0.027)]).enumerate() {
        let heard = if index % 2 == 0 { left } else { right };
        assert!((heard - expected).abs() < 0.2, "old repeat {}: {heard:.2} dB, wanted {expected:.2}", index + 1);
    }
}

#[test]
fn a_delay_filters_what_comes_in_and_again_every_time_round() {
    // A 1 kHz burst through a low cut at 1 kHz: 3 dB off the first repeat
    // already, and 3 more each time it comes round.
    let signal = burst(3.0, 0.5);
    let level = f64::from(rms(&signal[frames(0.1..0.12)]));
    let set = |legacy: f64| {
        [
            ("time_beats", 0.5),
            ("feedback", 0.5),
            ("ping_pong", 0.0),
            ("mix", 1.0),
            ("low_cut", 1000.0),
            ("high_cut", 20_000.0),
            ("legacy", legacy),
        ]
    };
    let now = repeats(&mono_through(&signal, &[("builtin:delay", &set(0.0))], 2.0), level, 3);
    let before = repeats(&mono_through(&signal, &[("builtin:delay", &set(1.0))], 2.0), level, 3);
    // What one pass through the filter takes off: about 3 dB at its corner.
    let filter = now[0].0;
    assert!((-4.2..=-2.5).contains(&filter), "the first repeat is filtered already: {filter:.2} dB");
    for (index, (left, right)) in now.iter().enumerate() {
        let expected = (index as f64 + 1.0) * filter + index as f64 * db(0.5);
        assert!((left - expected).abs() < 0.6 && (left - right).abs() < 0.01, "repeat {}: {left:.2} dB", index + 1);
    }
    // Before, the first repeat came back as it went in, and the filter
    // began with the second.
    assert!(before[0].0.abs() < 0.1 && (before[1].0 - (filter + db(0.5))).abs() < 0.6, "{before:?}");
}

// ---- compressor ------------------------------------------------------------

/// A mix in miniature: a steady tone 20 dB down with a full-scale hit of
/// 10 ms five times a second. The peaks stand 15 dB over the average.
fn hits(seconds: f64) -> Vec<f32> {
    (0..(seconds * SR) as usize)
        .map(|n| {
            let hit = n % 9_600 < 480;
            ((if hit { 1.0 } else { 0.1 }) * (TAU * 1000.0 * n as f64 / SR).sin()) as f32
        })
        .collect()
}

/// A bus compressor's settings on the bench's song: the Glue Compressor's.
fn glue(detector: f64) -> [(&'static str, f64); 6] {
    [("threshold", -10.0), ("ratio", 2.0), ("attack", 30.0), ("release", 400.0), ("knee", 0.0), ("detector", detector)]
}

#[test]
fn the_bus_detector_works_on_a_mix_whose_peaks_stand_far_over_its_average() {
    let signal = hits(8.0);
    let crest = db(f64::from(peak(&signal))) - rms_db(&signal);
    assert!((crest - 15.2).abs() < 0.3, "the signal's crest: {crest:.1} dB");
    let taken = |detector: f64| {
        let out = mono_through(&signal, &[("builtin:compressor", &glue(detector))], 0.0);
        let dynamics = &out.measured.dynamics[0];
        (
            f64::from(dynamics.typical_db),
            f64::from(dynamics.most_db),
            f64::from(dynamics.active),
            rms_db(&out.left) - rms_db(&signal),
        )
    };
    let (peak_typical, peak_most, peak_active, _) = taken(0.0);
    let (bus_typical, bus_most, bus_active, bus_level) = taken(1.0);
    println!("threshold -10 dB, ratio 2, attack 30 ms, release 400 ms, on a signal with a crest of {crest:.1} dB");
    println!(
        "  peak detector: takes off {peak_typical:.1} dB typically, {peak_most:.1} at most, {:.0} % of the time",
        peak_active * 100.0
    );
    println!(
        "  bus detector:  takes off {bus_typical:.1} dB typically, {bus_most:.1} at most, {:.0} % of the time",
        bus_active * 100.0
    );
    println!(
        "  on the song's mix, Live's Glue Compressor made the mix 1.3 dB quieter than the peak detector's 0.6 dB for 5 %"
    );

    // The hits ask for 5 dB (10 dB over the threshold at a ratio of 2). The
    // bus detector holds that and lets go of it over 400 ms: by the next
    // hit, a fifth of a second on, it has let go of 39 % of it. So it is
    // always between 3.0 and 5.0 dB, and 3.9 dB on average; the attack of
    // 30 ms evens that out without moving the average.
    let held = 5.0 * (1.0 - (-0.5f64).exp()) / 0.5;
    assert!((bus_typical - held).abs() < 0.15, "the bus detector takes off {bus_typical:.2} dB, wanted {held:.2}");
    assert!(bus_most < 5.0 && bus_most > held, "{bus_most}");
    assert!(bus_active > 0.98, "all the time: {bus_active}");
    // The quiet tone between the hits is turned down by all of that: the
    // level of the whole goes down with it.
    assert!((bus_level + held).abs() < 0.5, "{bus_level:.2} dB");
    // The peak detector sees ten milliseconds of hit at a time, and gets a
    // quarter of the way there before it is over.
    assert!(peak_typical < 1.5 && peak_most < 2.0, "the peak detector takes off {peak_typical:.2} dB");
    assert!(peak_active < bus_active);
}

#[test]
fn the_peak_detector_is_the_one_songs_were_made_with() {
    // A song of format 1 has no detector named: it is the peak detector,
    // and what it does is the same samples with and without the name.
    let signal = hits(2.0);
    let mut named = glue(0.0).to_vec();
    let unnamed: Vec<(&str, f64)> = named.iter().copied().filter(|(id, _)| *id != "detector").collect();
    named.push(("auto_makeup", 0.0));
    let (a, b) = (
        mono_through(&signal, &[("builtin:compressor", &named)], 0.0),
        mono_through(&signal, &[("builtin:compressor", &unnamed)], 0.0),
    );
    assert!(a.left == b.left && a.right == b.right);
}

#[test]
fn automatic_makeup_adds_what_its_description_says() {
    // Far under the threshold nothing is taken off, and what is left is the
    // makeup: 0.72 of what a full-scale signal would lose.
    let signal = tones(&[1000.0], 0.003, 2.0);
    let level = |set: &[(&str, f64)]| {
        let out = mono_through(&signal, &[("builtin:compressor", set)], 0.0);
        rms_db(&out.left[frames(1.0..2.0)]) - rms_db(&signal[frames(1.0..2.0)])
    };
    // The six compressors of the bench's song, with the makeup that brought
    // each to the level of Live's stem.
    let song = [
        (-24.0, 6.0, 14.5),
        (-30.0, 4.0, 16.15),
        (-30.0, 4.0, 16.25),
        (-24.0, 4.0, 13.4),
        (-20.0, 3.0, 9.27),
        (-20.0, 3.0, 9.27),
    ];
    println!("threshold  ratio   auto makeup   what fitted Live's stem");
    for (threshold, ratio, fitted) in song {
        let expected = 0.72 * -threshold * (1.0 - 1.0 / ratio);
        let added = level(&[("threshold", threshold), ("ratio", ratio), ("auto_makeup", 1.0)]);
        println!("  {threshold:>6}  {ratio:>5}   {added:>8.2} dB   {fitted:>8.2} dB");
        assert!((added - expected).abs() < 0.02, "{threshold} dB at {ratio}: {added:.2} dB, wanted {expected:.2}");
        assert!(
            (added - fitted).abs() < 0.5,
            "within half a dB of what Live's amounts to: {added:.2} against {fitted}"
        );
    }
    // It comes on top of the makeup that is set, and is nothing when off.
    let both = level(&[("threshold", -24.0), ("ratio", 4.0), ("auto_makeup", 1.0), ("makeup", 2.0)]);
    assert!((both - (0.72 * 24.0 * 0.75 + 2.0)).abs() < 0.02, "{both}");
    assert!(level(&[("threshold", -24.0), ("ratio", 4.0)]).abs() < 0.01);
}

#[test]
fn automatic_release_lets_go_of_one_hit_at_once_and_of_a_loud_passage_over_seconds() {
    // A quiet tone that is never over the threshold, to read the gain off;
    // over it, one hit of 50 ms at 1 s, and from 4 s to 10 s six seconds at
    // full scale.
    let total = 14.0;
    let quiet = tones(&[500.0], 0.01, total);
    let mut signal = quiet.clone();
    for (range, hz) in [(frames(1.0..1.05), 2000.0), (frames(4.0..10.0), 2000.0)] {
        for (n, sample) in signal[range].iter_mut().enumerate() {
            *sample += (0.98 * (TAU * hz * n as f64 / SR).sin()) as f32;
        }
    }
    for detector in [0.0, 1.0] {
        let mut set = glue(detector).to_vec();
        set.push(("auto_release", 1.0));
        let out = mono_through(&signal, &[("builtin:compressor", &set)], 0.0);
        // How far the quiet tone is turned down over a tenth of a second.
        let taken = |from: f64| {
            let window = frames(from..from + 0.1);
            -db(amplitude(&out.left[window.clone()], 500.0) / amplitude(&quiet[window], 500.0))
        };
        let (after_hit, after_passage, later) = (taken(2.0), taken(11.0), taken(13.5));
        println!(
            "detector {detector}: a second after one hit {after_hit:.2} dB, a second after six loud seconds {after_passage:.2} dB, 3.5 s after {later:.2} dB"
        );
        // Of the 5 dB a hit asks for, a fifth at most is left a second on.
        assert!(after_hit < 1.0, "one hit is let go of within a second: {after_hit:.2} dB");
        assert!(after_passage > 3.0, "a loud passage is not: {after_passage:.2} dB");
        assert!(later < after_passage - 0.5 && later > 1.5, "it comes back over seconds: {later:.2} dB");
    }
}

// ---- reverb ----------------------------------------------------------------

/// The two reverbs of the bench's song, as its literal pass set them.
const ROOM: [(&str, f64); 6] =
    [("decay", 0.7), ("predelay", 6.0), ("size", 0.21), ("damping", 0.3), ("low_cut", 110.0), ("mix", 1.0)];
const HALL: [(&str, f64); 6] =
    [("decay", 2.4), ("predelay", 20.0), ("size", 0.6), ("damping", 0.5), ("low_cut", 110.0), ("mix", 1.0)];

/// What a reverb at 100 % wet returns of six seconds of noise, in dB over
/// the noise, once it has filled. The noise is kept between 100 Hz and
/// 3 kHz, where music has most of what it has: the top of white noise dies
/// away in a reverb at once, whatever its decay.
fn returned(set: &[(&str, f64)]) -> f64 {
    let signal = noise(6 * SR as usize, 0x1234_5678);
    let signal: Vec<f32> = signal.iter().map(|sample| sample * 0.25).collect();
    let band: &[(&str, f64)] =
        &[("high_cut", 3000.0), ("high_cut_slope", 2.0), ("low_cut", 100.0), ("low_cut_slope", 2.0)];
    let went_in = mono_through(&signal, &[("builtin:eq", band)], 0.0);
    let out = mono_through(&signal, &[("builtin:eq", band), ("builtin:reverb", set)], 0.0);
    let window = frames(4.0..6.0);
    let both = |out: &Rendered| {
        (f64::from(rms(&out.left[window.clone()])).powi(2) + f64::from(rms(&out.right[window.clone()])).powi(2)).sqrt()
    };
    db(both(&out) / both(&went_in))
}

#[test]
fn a_reverb_returns_what_the_bench_found_lives_to_return() {
    println!(
        "at 100 % wet, for the same noise going in     format 1    now    change   (the change that matched Live's)"
    );
    let mut changes = Vec::new();
    for (name, set, fitted) in [("room, decay 0.7 s", ROOM, -15.0), ("hall, decay 2.4 s", HALL, -7.0)] {
        // As a song of format 1 has it, and as the same settings are asked
        // for now, with the high cut Live's input filter has.
        let mut before = set.to_vec();
        before.extend([("legacy", 1.0), ("high_cut", 20_000.0)]);
        let mut now = set.to_vec();
        now.push(("high_cut", 6300.0));
        let (before, now) = (returned(&before), returned(&now));
        println!("  {name:<40}  {before:>+6.1} dB  {now:>+6.1} dB  {:>+6.1} dB    {fitted:>+6.1} dB", now - before);
        changes.push((now - before, fitted, now));
    }
    // The bench brought Trill's two reverbs to Live's by turning the room
    // down 15 dB and the hall 7 dB, each with a filter on what went in.
    // The same settings now come out within 2 dB of that by themselves.
    for (change, fitted, _) in &changes {
        assert!((change - fitted).abs() < 2.0, "the reverb changed by {change:.1} dB, and {fitted} dB matched Live's");
    }
    // A longer decay returns more: 6 dB for each doubling, so 10.7 dB from
    // 0.7 s to 2.4 s, give or take what the two rooms' filters make of it.
    let (room, hall) = (changes[0].2, changes[1].2);
    assert!((hall - room - 10.7).abs() < 2.0, "the hall returns {:.1} dB more than the room", hall - room);
}

#[test]
fn a_reverbs_level_follows_its_decay_and_not_its_size() {
    let set = |decay: f64, size: f64| {
        [("decay", decay), ("size", size), ("mix", 1.0), ("damping", 0.0), ("low_cut", 20.0), ("high_cut", 20_000.0)]
    };
    // Twice the decay, 6 dB more, up to five seconds.
    let (short, long) = (returned(&set(1.0, 0.5)), returned(&set(2.0, 0.5)));
    assert!((long - short - 6.0).abs() < 1.0, "from 1 s to 2 s: {:.1} dB more", long - short);
    // Any size, the same level, within 2 dB: the smallest room against
    // the largest, and against the one between.
    let (small, large) = (returned(&set(1.0, 0.0)), returned(&set(1.0, 1.0)));
    println!("decay 1 s: size 0 returns {small:+.1} dB, size 0.5 {short:+.1} dB, size 1 {large:+.1} dB");
    assert!((small - large).abs() < 2.5 && (small - short).abs() < 2.5, "size 0: {small:.1} dB, size 1: {large:.1} dB");
    // What the description says a send comes back as.
    println!("decay 1 s returns {short:+.1} dB of what went in, 2 s {long:+.1} dB");
    assert!((-13.0..=-9.0).contains(&short) && (-7.5..=-3.5).contains(&long), "{short:.1} {long:.1}");
}

#[test]
fn a_reverb_filters_what_goes_into_it_and_is_as_wide_as_it_is_set() {
    // A tone two octaves under the low cut and one two octaves over the
    // high cut go in 24 dB down each: 12 dB an octave. The tail is read
    // after the tones have stopped, where nothing but reverb is left.
    let mut signal = tones(&[100.0, 1000.0, 8000.0], 0.2, 2.0);
    signal.extend(vec![0.0f32; SR as usize]);
    let ring = frames(2.05..2.35);
    let tail = |set: &[(&str, f64)], hz: f64| {
        let out = mono_through(&signal, &[("builtin:reverb", set)], 0.0);
        db(amplitude(&out.left[ring.clone()], hz))
    };
    let base = [("decay", 3.0), ("mix", 1.0), ("damping", 0.0), ("modulation", 0.0), ("predelay", 0.0)];
    let with = |low_cut: f64, high_cut: f64| {
        let mut set = base.to_vec();
        set.extend([("low_cut", low_cut), ("high_cut", high_cut)]);
        set
    };
    let (open, cut) = (with(20.0, 20_000.0), with(400.0, 2000.0));
    let lost = |hz: f64| tail(&open, hz) - tail(&cut, hz);
    println!(
        "input cuts at 400 Hz and 2 kHz take off: 100 Hz {:.1} dB, 1 kHz {:.1} dB, 8 kHz {:.1} dB",
        lost(100.0),
        lost(1000.0),
        lost(8000.0)
    );
    assert!((lost(100.0) - 24.0).abs() < 3.0, "two octaves under the low cut: {:.1} dB", lost(100.0));
    assert!((lost(8000.0) - 24.0).abs() < 3.0, "two octaves over the high cut: {:.1} dB", lost(8000.0));
    assert!(lost(1000.0).abs() < 3.0, "between them: {:.1} dB", lost(1000.0));

    // Width: nothing between the sides at 0, the sides doubled at 2.
    let noise = noise(3 * SR as usize, 99);
    let sides = |width: f64| {
        let mut set = base.to_vec();
        set.push(("width", width));
        let out = mono_through(&noise, &[("builtin:reverb", &set)], 0.0);
        let window = frames(1.0..3.0);
        let side: Vec<f32> =
            out.left[window.clone()].iter().zip(&out.right[window.clone()]).map(|(l, r)| (l - r) * 0.5).collect();
        let mid: Vec<f32> =
            out.left[window.clone()].iter().zip(&out.right[window]).map(|(l, r)| (l + r) * 0.5).collect();
        (f64::from(rms(&side)), f64::from(rms(&mid)))
    };
    let (mono, as_it_is, doubled) = (sides(0.0), sides(1.0), sides(2.0));
    assert!(mono.0 < 1e-6 && mono.1 > 0.01, "width 0 is mono: {mono:?}");
    assert!(as_it_is.0 > 0.5 * as_it_is.1, "the reverb itself is wide: {as_it_is:?}");
    assert!((db(doubled.0 / as_it_is.0) - 6.0).abs() < 0.1 && (db(doubled.1 / as_it_is.1)).abs() < 0.1);
}

// ---- chorus and tremolo ----------------------------------------------------

#[test]
fn a_chorus_is_as_wide_as_its_width() {
    let signal = tones(&[220.0, 331.0, 440.0], 0.2, 3.0);
    let sides = |width: f64| {
        let out = mono_through(&signal, &[("builtin:chorus", &[("mix", 0.5), ("depth", 0.6), ("width", width)])], 0.0);
        let window = frames(1.0..3.0);
        let side: Vec<f32> =
            out.left[window.clone()].iter().zip(&out.right[window.clone()]).map(|(l, r)| (l - r) * 0.5).collect();
        (f64::from(rms(&side)), f64::from(rms(&out.left[window])))
    };
    let (mono, as_it_is, wide) = (sides(0.0), sides(1.0), sides(1.4));
    assert!(mono.0 < 1e-6 && mono.1 > 0.05, "width 0 keeps a mono source mono: {mono:?}");
    assert!(as_it_is.0 > 0.01, "the chorus itself is wide: {as_it_is:?}");
    // Live's Chorus-Ensemble at Width 140 %: the sides 1.4 times as much.
    assert!((wide.0 / as_it_is.0 - 1.4).abs() < 0.01, "{:.3}", wide.0 / as_it_is.0);
}

#[test]
fn a_tremolo_dips_as_deep_and_as_often_as_it_is_set_and_pans_when_it_is_spread() {
    let signal = tones(&[1000.0], 0.5, 4.0);
    // The level of each side, a thousand times a second.
    let envelope = |out: &Rendered| -> (Vec<f64>, Vec<f64>) {
        let side =
            |samples: &[f32]| samples.chunks(48).map(|chunk| f64::from(rms(chunk)) / (0.5 / 2f64.sqrt())).collect();
        (side(&out.left), side(&out.right))
    };
    let dips = |levels: &[f64]| {
        let middle = 0.5 * (levels.iter().cloned().fold(0.0, f64::max) + levels.iter().cloned().fold(1.0, f64::min));
        levels.windows(2).filter(|pair| pair[0] >= middle && pair[1] < middle).count()
    };
    let range =
        |levels: &[f64]| (levels.iter().cloned().fold(1.0, f64::min), levels.iter().cloned().fold(0.0, f64::max));

    // A tremolo: 5 dips a second down to 65 %, both sides together.
    let out = mono_through(&signal, &[("builtin:tremolo", &[("rate", 5.0), ("depth", 0.35)])], 0.0);
    let (left, right) = envelope(&out);
    let (low, high) = range(&left[1000..3000]);
    assert!((low - 0.65).abs() < 0.01 && (high - 1.0).abs() < 0.01, "from {low:.3} to {high:.3}");
    assert_eq!(dips(&left[1000..3000]), 10, "five dips a second");
    assert!(left.iter().zip(&right).all(|(l, r)| (l - r).abs() < 1e-4), "both sides move together");

    // An auto-pan: the right side is loud while the left is quiet, and the
    // two together are as loud at every moment.
    let out = mono_through(&signal, &[("builtin:tremolo", &[("rate", 5.0), ("depth", 0.35), ("spread", 180.0)])], 0.0);
    let (left, right) = envelope(&out);
    let quietest = (1000..3000).min_by(|a, b| left[*a].total_cmp(&left[*b])).unwrap();
    assert!((left[quietest] - 0.65).abs() < 0.01 && (right[quietest] - 1.0).abs() < 0.01);
    assert!(left[1000..3000].iter().zip(&right[1000..3000]).all(|(l, r)| (l + r - 1.65).abs() < 0.01));

    // In time with the song: a cycle a beat long starts on the beat at full
    // level and is at its lowest half a beat on. A square stays down for
    // the second half of the cycle.
    let synced: &[(&str, f64)] = &[("sync", 1.0), ("rate_beats", 1.0), ("depth", 1.0), ("shape", 2.0)];
    let out = mono_through(&signal, &[("builtin:tremolo", synced)], 0.0);
    let (left, _) = envelope(&out);
    for beat in 2..7 {
        let at = |beats: f64| left[(beats * 500.0) as usize];
        assert!(
            at(beat as f64 + 0.25) > 0.99 && at(beat as f64 + 0.75) < 0.01,
            "beat {beat}: {} {}",
            at(beat as f64 + 0.25),
            at(beat as f64 + 0.75)
        );
    }
}

// ---- a compressor that listens to another track ----------------------------

/// A bass and a kick: the bass a steady tone, the kick a low thump of 60 ms
/// on every beat, with a hi-hat of 30 ms on every off-beat.
struct Ducking {
    song: Song,
    bass: TrackId,
    kick: TrackId,
    kick_audio: Vec<f32>,
    compressor: DeviceId,
}

const KICK_SECONDS: f64 = 0.06;
/// A compressor that takes nearly everything off while its key sounds and
/// has let go of it a tenth of a second later.
const SQUEEZE: [(&str, f64); 5] =
    [("threshold", -30.0), ("ratio", 10.0), ("attack", 1.0), ("release", 30.0), ("knee", 0.0)];

fn ducking() -> Ducking {
    let seconds = 4.0;
    let bass = tones(&[110.0], 0.25, seconds);
    let mut kick = vec![0.0f32; (seconds * SR) as usize];
    for beat in 0..8 {
        let start = beat as f64 * 0.5;
        for (n, sample) in kick[frames(start..start + KICK_SECONDS)].iter_mut().enumerate() {
            *sample = (0.9 * (TAU * 60.0 * n as f64 / SR).sin()) as f32;
        }
        for (n, sample) in kick[frames(start + 0.25..start + 0.28)].iter_mut().enumerate() {
            *sample = (0.9 * (TAU * 9000.0 * n as f64 / SR).sin()) as f32;
        }
    }
    let mut song = Song::new();
    let (kick_track, _) = song.track("Kick", &kick, &kick, &[]);
    let (bass, devices) = song.track("Bass", &bass, &bass, &[("builtin:compressor", &SQUEEZE)]);
    Ducking { song, bass, kick: kick_track, kick_audio: kick, compressor: devices[0] }
}

/// The level of the bass in `out` while the kick sounds, between the kick
/// and the hat, while the hat sounds and after it, as averages over the
/// beats from the second on, in dB under the bass as it went in.
fn bass_levels(out: &Rendered) -> [f64; 4] {
    let mut levels = [0.0; 4];
    for beat in 1..7 {
        let start = beat as f64 * 0.5;
        let windows = [
            start + 0.02..start + 0.06,
            start + 0.19..start + 0.24,
            start + 0.255..start + 0.28,
            start + 0.42..start + 0.49,
        ];
        for (level, window) in levels.iter_mut().zip(windows) {
            *level += db(f64::from(rms(&out.left[frames(window)])) / (0.25 / 2f64.sqrt())) / 6.0;
        }
    }
    levels
}

fn only(track: TrackId) -> RenderOptions {
    RenderOptions {
        sample_rate: SR as u32,
        max_tail_seconds: 0.0,
        only_tracks: Some(vec![track]),
        ..Default::default()
    }
}

#[test]
fn a_compressor_turns_its_track_down_to_the_track_it_listens_to() {
    let Ducking { mut song, bass, kick, compressor, .. } = ducking();
    // Without a sidechain the compressor hears the bass itself, which is
    // steady: turned down, and by the same at every moment.
    let alone = bass_levels(&song.render_with(only(bass)));
    assert!(alone.iter().all(|level| (level - alone[0]).abs() < 0.3) && alone[0] < -10.0, "{alone:?}");

    // Listening to the kick: down while the kick sounds and while the hat
    // does, and back in between.
    song.project.set_sidechain(compressor, Some(kick)).unwrap();
    let heard = song.render_with(only(bass));
    let [on_kick, between, on_hat, after] = bass_levels(&heard);
    println!(
        "the bass under the kick {on_kick:.1} dB, between {between:.1} dB, under the hat {on_hat:.1} dB, after {after:.1} dB"
    );
    assert!(on_kick < -20.0, "ducked under the kick: {on_kick:.1} dB");
    assert!(on_hat < -15.0, "and under the hat: {on_hat:.1} dB");
    assert!(between > -1.0 && after > -1.0, "and back up between them: {between:.1} dB, {after:.1} dB");
    // The reduction is counted for the bass's compressor, as any is.
    let dynamics = &heard.measured.dynamics[0];
    assert_eq!(dynamics.track, Some(bass));
    assert!(dynamics.most_db > 20.0 && dynamics.active > 0.3 && dynamics.active < 0.8, "{dynamics:?}");

    // The key has a filter of its own: with everything over 200 Hz kept
    // from it, the hat no longer ducks the bass, and the bass itself is
    // not filtered.
    song.project.device_mut(compressor).unwrap().params.insert("key_high_cut".into(), 200.0);
    let [on_kick, _, on_hat, after] = bass_levels(&song.render_with(only(bass)));
    assert!(on_kick < -20.0 && on_hat > -1.0 && after > -1.0, "kick {on_kick:.1} dB, hat {on_hat:.1} dB");
    // And the other way round: only the hat.
    let params = &mut song.project.device_mut(compressor).unwrap().params;
    params.insert("key_high_cut".into(), 20_000.0);
    params.insert("key_low_cut".into(), 2000.0);
    let [on_kick, _, on_hat, _] = bass_levels(&song.render_with(only(bass)));
    assert!(on_kick > -3.0 && on_hat < -15.0, "kick {on_kick:.1} dB, hat {on_hat:.1} dB");
}

#[test]
fn a_track_that_is_listened_to_need_not_be_heard() {
    let Ducking { mut song, bass, kick, kick_audio, compressor } = ducking();
    song.project.set_sidechain(compressor, Some(kick)).unwrap();
    let both = song.render(0.0);
    let ducked = bass_levels(&song.render_with(only(bass)));
    assert!(ducked[0] < -20.0, "{ducked:?}");

    // Muted, or with its fader all the way down, the kick is still what the
    // compressor listens to: the key is taken ahead of the fader.
    for mute in [true, false] {
        let mut quiet = Song { project: song.project.clone(), folder: folder(), files: 0 };
        if mute {
            quiet.project.track_mut(kick).unwrap().mute = true;
        } else {
            quiet.project.set_track_volume(kick, -70.0).unwrap();
        }
        let out = quiet.render(0.0);
        // No kick in it: nothing at 60 Hz to speak of.
        assert!(amplitude(&out.left[frames(1.0..2.0)], 60.0) < 1e-3, "the kick is not heard");
        let levels = bass_levels(&out);
        assert!(levels.iter().zip(ducked).all(|(a, b)| (a - b).abs() < 0.2), "{levels:?} against {ducked:?}");
    }
    // And with the kick heard, it is in the mix beside the ducked bass.
    assert!(amplitude(&both.left[frames(1.0..2.0)], 60.0) > 0.01);

    // Hearing only the bass, the kick is still made and played for it (that
    // is `ducked`, above); and hearing only the kick, it is the kick and
    // nothing of the bass.
    let out = song.render_with(only(kick));
    let window = frames(1.0..2.0);
    let rest: Vec<f32> = out.left[window.clone()].iter().zip(&kick_audio[window]).map(|(a, b)| a - b).collect();
    assert!(rms(&rest) < 1e-4, "{}", rms(&rest));
    assert_eq!(out.measured.tracks.len(), 1, "the bass is not even made: nothing heard listens to it");
}

#[test]
fn a_key_comes_from_any_track_that_can_be_played_first() {
    let Ducking { mut song, bass, kick, compressor, .. } = ducking();
    // The kick through a group: the compressor may listen to the group,
    // which is then played before the bass though it is a group.
    let drums = song.project.add_track_of_kind("Drums", trill_core::TrackKind::Group);
    song.project.set_track_group(kick, Some(drums)).unwrap();
    song.project.set_sidechain(compressor, Some(drums)).unwrap();
    // The group itself is muted here, so that what is rendered is the bass
    // alone: it is listened to all the same, as any track is.
    song.project.track_mut(drums).unwrap().mute = true;
    let whole = bass_levels(&song.render(0.0));
    assert!(whole[0] < -20.0 && whole[1] > -1.0 && whole[3] > -1.0, "{whole:?}");
    song.project.track_mut(drums).unwrap().mute = false;
    // Hearing the bass alone, the kick still plays into the group for it,
    // and neither the kick nor the group is heard.
    let alone = song.render_with(only(bass));
    let levels = bass_levels(&alone);
    assert!(levels.iter().zip(whole).all(|(a, b)| (a - b).abs() < 0.3), "{levels:?} against {whole:?}");
    // A kick that is muted is not in the group, and so not in the key.
    song.project.track_mut(kick).unwrap().mute = true;
    let silent = bass_levels(&song.render_with(only(bass)));
    assert!(silent.iter().all(|level| level.abs() < 0.3), "{silent:?}");
    song.project.track_mut(kick).unwrap().mute = false;

    // A compressor on the master listens to a track as well.
    let on_master = song.device(ChainRef::Master, &("builtin:compressor", &SQUEEZE));
    song.project.set_sidechain(compressor, None).unwrap();
    song.project.device_mut(compressor).unwrap().enabled = false;
    song.project.set_sidechain(on_master, Some(kick)).unwrap();
    song.project.track_mut(kick).unwrap().mute = true;
    let [on_kick, between, _, after] = bass_levels(&song.render(0.0));
    assert!(on_kick < -20.0 && between > -1.0 && after > -1.0, "{on_kick:.1} {between:.1} {after:.1}");

    // What cannot be played first is refused, in words, and nothing changes.
    let before = song.project.clone();
    let refused = |result: Result<(), trill_core::EditError>| result.unwrap_err().to_string();
    assert!(refused(song.project.set_sidechain(compressor, Some(bass))).contains("itself"));
    // The bass through the drum group, and its compressor listening to
    // that group: the group would have to come both before and after.
    song.project.set_sidechain(compressor, Some(drums)).unwrap();
    assert!(refused(song.project.set_track_group(bass, Some(drums))).contains("loop"));
    song.project.set_sidechain(compressor, None).unwrap();
    assert_eq!(song.project, before);
    // Two tracks listening to each other.
    let on_kick = song.device(ChainRef::Track(kick), &("builtin:compressor", &[]));
    song.project.set_sidechain(on_kick, Some(bass)).unwrap();
    assert!(refused(song.project.set_sidechain(compressor, Some(kick))).contains("loop"));
    assert_eq!(song.project.device(compressor).unwrap().sidechain, None);

    // A track that is deleted is listened to by nothing.
    song.project.set_sidechain(on_kick, None).unwrap();
    song.project.set_sidechain(compressor, Some(kick)).unwrap();
    song.project.remove_track(kick).unwrap();
    assert_eq!(song.project.device(compressor).unwrap().sidechain, None);
    song.render(0.0);
}

/// What the compressor of the textbook takes off, in dB, at every frame of
/// a key: the louder side's level, frame by frame, through a second-order
/// Butterworth low-pass at `key_high_cut` Hz; what is over the threshold
/// cut by the ratio, with a soft knee; the cut coming on over the attack
/// and going over the release, each a one-pole time. Worked out here in
/// its own arithmetic, with nothing of the device's in it: it is what the
/// bench drew by hand where Trill had no sidechain, and found Live's
/// compressor to follow within 1 dB from 20 ms after a kick on.
fn on_paper(
    key: &[f32],
    threshold: f64,
    ratio: f64,
    knee: f64,
    attack: f64,
    release: f64,
    key_high_cut: f64,
) -> Vec<f64> {
    // The low-pass, by the bilinear transform.
    let k = (PI * key_high_cut / SR).tan();
    let norm = 1.0 / (1.0 + 2f64.sqrt() * k + k * k);
    let (b0, b1, b2) = (k * k * norm, 2.0 * k * k * norm, k * k * norm);
    let (a1, a2) = (2.0 * (k * k - 1.0) * norm, (1.0 - 2f64.sqrt() * k + k * k) * norm);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    let (up, down) = ((-1.0 / (attack * SR)).exp(), (-1.0 / (release * SR)).exp());
    let slope = 1.0 - 1.0 / ratio;
    let mut cut = 0.0f64;
    key.iter()
        .map(|sample| {
            let x = f64::from(*sample);
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, x, y1, y);
            let over = 20.0 * (y.abs() + 1e-9).log10() - threshold;
            let asked = if over <= -knee / 2.0 {
                0.0
            } else if over >= knee / 2.0 {
                over * slope
            } else {
                slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
            };
            cut = asked + (cut - asked) * if asked > cut { up } else { down };
            cut
        })
        .collect()
}

#[test]
fn a_pad_is_ducked_under_a_kick_as_the_bench_measured_lives_to_do() {
    // The pad of the bench's second song: a compressor that listens to the
    // drums through a low-pass at 90 Hz, so that it hears the kick alone.
    let (threshold, ratio, knee, attack, release, lows) = (-34.0, 4.0, 6.0, 0.001, 0.180, 90.0);
    let seconds = 4.0;
    let pad = tones(&[220.0], 0.25, seconds);
    // A kick every half second: a low thump that falls in pitch and dies
    // away over a tenth of a second. A hi-hat between the kicks, as loud,
    // which the low-pass keeps from the compressor.
    let mut drums = vec![0.0f32; (seconds * SR) as usize];
    for beat in 0..8 {
        let start = (beat as f64 * 0.5 * SR) as usize;
        for n in 0..(0.4 * SR) as usize {
            let t = n as f64 / SR;
            let phase = TAU * (48.0 * t + 1.2 * (1.0 - (-t / 0.02).exp()));
            drums[start + n] += (0.55 * (-t / 0.09).exp() * phase.sin()) as f32;
        }
        for (n, sample) in drums[start + 12_000..start + 13_440].iter_mut().enumerate() {
            *sample += (0.5 * (TAU * 9000.0 * n as f64 / SR).sin()) as f32;
        }
    }
    let mut song = Song::new();
    let (kit, _) = song.track("Drums", &drums, &drums, &[]);
    let duck: &[(&str, f64)] = &[
        ("threshold", threshold),
        ("ratio", ratio),
        ("knee", knee),
        ("attack", attack * 1000.0),
        ("release", release * 1000.0),
        ("key_high_cut", lows),
    ];
    let (bass, devices) = song.track("Pad", &pad, &pad, &[("builtin:compressor", duck)]);
    song.project.set_sidechain(devices[0], Some(kit)).unwrap();
    let out = song.render_with(only(bass));
    let expected = on_paper(&drums, threshold, ratio, knee, attack, release, lows);

    // What is taken off at a moment, by the device (what went in over what
    // came out) and on paper: both read at the frames within 2 ms where
    // the pad's wave is far from zero, and the middle one of them taken.
    let middle_of = |at: f64, read: &dyn Fn(usize) -> f64| {
        let middle = (at * SR) as usize;
        let mut values: Vec<f64> = (middle - 96..middle + 96).filter(|n| pad[*n].abs() > 0.12).map(read).collect();
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    let taken = |at: f64| middle_of(at, &|n| -db(f64::from(out.left[n]) / f64::from(pad[n])));
    let asked = |at: f64| middle_of(at, &|n| expected[n]);
    // The moments the bench read Live's compressor at, after a kick, and
    // what it read on its own pad and kick (other audio than this, so the
    // numbers are beside these and not the same): Live's device, and the
    // compressor on paper run on Live's recording of the drums.
    let moments = [-20.0, 0.0, 6.0, 10.0, 20.0, 40.0, 70.0, 100.0, 150.0, 200.0, 300.0];
    let lives = [-11.9, -10.5, -11.4, -14.0, -20.6, -19.9, -17.2, -15.5, -12.4, -10.9, -6.5];
    let papers = [-11.9, -10.7, -10.4, -11.6, -19.6, -19.4, -17.6, -16.1, -13.1, -10.2, -5.8];
    println!("ms after the kick    this device    on paper    (Live's, on its own song)   (on paper there)");
    let mut deepest = (0.0, 0.0);
    for (index, ms) in moments.iter().enumerate() {
        // The average over the kicks from the third on, once the pattern
        // has settled; on paper, at the same frames.
        let kicks: Vec<f64> = (2..7).map(|beat| beat as f64 * 0.5 + ms / 1000.0).collect();
        let device = -kicks.iter().map(|at| taken(*at)).sum::<f64>() / kicks.len() as f64;
        let paper = -kicks.iter().map(|at| asked(*at)).sum::<f64>() / kicks.len() as f64;
        println!(
            "  {ms:>6}            {device:>7.1} dB   {paper:>7.1} dB        {:>7.1} dB            {:>7.1} dB",
            lives[index], papers[index]
        );
        // The bench found Live's within 1 dB of the paper from 20 ms on;
        // the device is the paper, to a few tenths at every moment.
        assert!((device - paper).abs() < 0.3, "{ms} ms after the kick: {device:.2} dB, on paper {paper:.2} dB");
        if device < deepest.1 {
            deepest = (*ms, device);
        }
    }
    // As in Live's: deepest some 20 to 40 ms after the kick, most of the
    // way back 300 ms after it, and still down when the next kick comes.
    assert!((20.0..=40.0).contains(&deepest.0) && deepest.1 < -15.0, "deepest {deepest:?}");
    let back = -taken(1.5 + 0.3);
    assert!(back > deepest.1 + 10.0 && back < -1.0, "300 ms on: {back:.1} dB, from {:.1} dB", deepest.1);
    // The hat, which is as loud as the kick, is not heard by it: between
    // the kicks nothing is taken off again.
    assert!(taken(1.5 + 0.27) < taken(1.5 + 0.2), "the hat at 250 ms does not duck the pad");
}

// ---- pitch shifter ---------------------------------------------------------

/// A device's parameter drawn over the song: points of (beat, value).
fn draw(song: &mut Song, device: DeviceId, param: &str, points: &[(f64, f64)]) {
    use trill_core::project::{AutomationPoint, AutomationTarget};
    let target = AutomationTarget::DeviceParam { device, param: param.to_string() };
    let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
    song.project.set_automation(target, points).unwrap();
}

/// The dive the bench's second song draws for Live's Shifter at a stop on
/// beat 4: the mix opened over the twentieth of a beat before it, and the
/// pitch drawn from 0 to two octaves down over the beat after it.
fn dive(song: &mut Song, shifter: DeviceId) {
    draw(song, shifter, "mix", &[(0.0, 0.0), (3.95, 0.0), (4.0, 1.0)]);
    draw(song, shifter, "pitch", &[(0.0, 0.0), (4.05, 0.0), (5.0, -24.0)]);
}

#[test]
fn a_chord_melts_two_octaves_down_in_a_beat_where_the_pitch_shifter_is_drawn_to() {
    // One note first, whose pitch can be followed on the way down.
    let note = tones(&[880.0], 0.4, 4.0);
    let mut song = Song::new();
    let (_, devices) = song.track("Saws", &note, &note, &[("builtin:shifter", &[("mix", 0.0)])]);
    dive(&mut song, devices[0]);
    let out = song.render(0.0);
    // Until the stop the shifter is not in the sound at all: the track
    // as it plays without one, to the bit.
    let without = mono_through(&note, &[], 0.0);
    assert!(
        out.left[..frames(0.0..1.9).end] == without.left[..frames(0.0..1.9).end],
        "the shifter is heard before the stop"
    );
    // Then it falls along the line: semitones under 880 Hz, read over a
    // fiftieth of a second around each moment.
    println!("beats after the stop   drawn      heard   (semitones)");
    for after in [0.2, 0.35, 0.5, 0.65, 0.8, 0.95] {
        let drawn = -24.0 * (after - 0.05) / 0.95;
        let middle = (4.0 + after) * 0.5;
        let heard = 12.0 * (zero_crossing_hz(&out.left[frames(middle - 0.01..middle + 0.01)], SR) / 880.0).log2();
        println!("        {after:>4.2}          {drawn:>+6.1}    {heard:>+6.1}");
        assert!((heard - drawn).abs() < 2.0, "{after} beats after the stop: {heard} semitones, drawn {drawn}");
    }
    let landed = 12.0 * (zero_crossing_hz(&out.left[frames(2.7..3.9)], SR) / 880.0).log2();
    assert!((landed + 24.0).abs() < 0.05, "and stays two octaves down: {landed}");
    // As loud all the way as the note was, and with no click in it.
    for middle in [2.1, 2.2, 2.3, 2.4, 2.6, 3.0, 3.5] {
        let level = rms_db(&out.left[frames(middle - 0.04..middle + 0.04)]) - db(0.4 * std::f64::consts::FRAC_1_SQRT_2);
        assert!(level.abs() < 1.5, "{middle} s: {level} dB against the note");
    }
    assert!(largest_step(&out.left[frames(1.9..4.0)]) < 0.4 * 0.2, "{}", largest_step(&out.left[frames(1.9..4.0)]));

    // Then a chord, as the song has: every note of it two octaves down at
    // the end, as loud as it was, and nothing left where it was.
    let chord = tones(&[440.0, 556.0, 660.0, 880.0], 0.15, 4.0);
    let mut song = Song::new();
    let (_, devices) = song.track("Saws", &chord, &chord, &[("builtin:shifter", &[("mix", 0.0)])]);
    dive(&mut song, devices[0]);
    let out = song.render(0.0);
    let after = &out.left[frames(3.0..4.0)];
    // What there is within 12 Hz of a pitch. A chord has no one period the
    // shifter could read it by, so each note comes out with a grain to it:
    // a little of it to either side of where it belongs.
    let around = |hz: f64| (-12..=12).map(|off| amplitude(after, hz + f64::from(off)).powi(2)).sum::<f64>().sqrt();
    for hz in [440.0, 556.0, 660.0, 880.0] {
        let level = db(around(hz / 4.0) / 0.15);
        assert!(level.abs() < 1.5, "{} Hz: {level} dB against the note it was", hz / 4.0);
    }
    assert!(around(556.0) < 0.15 * 0.03 && around(660.0) < 0.15 * 0.03 && around(880.0) < 0.15 * 0.03);
    for middle in [2.1, 2.2, 2.3, 2.4, 2.6, 3.0, 3.5] {
        let level = rms_db(&out.left[frames(middle - 0.04..middle + 0.04)]) - rms_db(&chord[frames(0.5..1.5)]);
        assert!(level.abs() < 2.5, "{middle} s into the chord: {level} dB against it");
    }
}

// ---- saturator -------------------------------------------------------------

#[test]
fn a_saturator_does_not_turn_down_what_it_drives_and_its_sine_curve_adds_the_overtones() {
    // A sub bass as the bench's second song has it out of Live's Operator
    // (a sine that peaks at -3.5 dBFS), into the song's saturator: 9 dB of
    // drive, the output at -8 dB.
    let sub = tones(&[55.0], 0.667, 2.0);
    let driven = |set: &[(&str, f64)]| {
        let params: Vec<(&str, f64)> = [("drive", 9.0), ("output", -8.0)].iter().chain(set).copied().collect();
        mono_through(&sub, &[("builtin:saturator", &params)], 0.0)
    };
    let (soft, sine, old) = (driven(&[("shape", 0.0)]), driven(&[("shape", 3.0)]), driven(&[("legacy", 1.0)]));
    // A second from the middle, clear of where the clip fades in and out.
    let second = frames(0.5..1.5);
    // Driven over its ceiling, the sine curve holds the wave at the output's
    // level: it peaks at -8 dBFS, whatever the drive.
    assert!((db(f64::from(peak(&sine.left))) + 8.0).abs() < 0.01, "{}", db(f64::from(peak(&sine.left))));
    assert!((db(f64::from(peak(&soft.left))) + 8.0 - db(1.88f64.tanh())).abs() < 0.05);
    // A song saved before had half the drive taken off again. The bench
    // found that 6.3 dB under Live's Saturator on this sub, in loudness,
    // which the overtones add to; in level the sine curve is 5.8 dB over it.
    let over = rms_db(&sine.left[second.clone()]) - rms_db(&old.left[second.clone()]);
    println!("the sine curve over the old soft one, on the song's sub: {over:+.2} dB (the bench: Live's 6.3 dB over)");
    assert!((over - 5.8).abs() < 0.2, "{over}");
    assert!(
        (rms_db(&soft.left[second.clone()]) - rms_db(&old.left[second.clone()]) - 4.5).abs() < 0.01,
        "half of 9 dB"
    );

    // The overtones a sine bass is heard by on small speakers. With the
    // two as loud as each other at 55 Hz, the sine curve has 4 to 11 dB
    // more of the three that carry most of them (165, 275 and 385 Hz), and
    // far more further up: what the bench found missing against Live's
    // "Soft Sine" (4 to 8 dB between 120 Hz and 2 kHz). Run on Live's own
    // recording of this sub, the sine curve came within 0.6 dB of Live's
    // track in every band from 20 Hz to 10 kHz, the level at 55 Hz taken
    // out; the soft one was 4 to 8 dB short from 120 Hz to 2 kHz.
    let part = |out: &Rendered, harmonic: f64| db(amplitude(&out.left[second.clone()], 55.0 * harmonic));
    println!("overtone      the sine curve   the soft curve   (dB under each one's own 55 Hz)");
    for harmonic in [3.0, 5.0, 7.0, 9.0, 11.0, 15.0, 21.0, 29.0, 35.0] {
        let (with_sine, with_soft) =
            (part(&sine, harmonic) - part(&sine, 1.0), part(&soft, harmonic) - part(&soft, 1.0));
        println!("  {:>5.0} Hz       {with_sine:>6.1}          {with_soft:>6.1}", 55.0 * harmonic);
        if harmonic <= 7.0 {
            let more = with_sine - with_soft;
            assert!((4.0..=11.5).contains(&more), "{} Hz: {more} dB more", 55.0 * harmonic);
        }
    }
    // And no even ones: the curve is the same both ways.
    assert!(part(&sine, 2.0) - part(&sine, 1.0) < -60.0 && part(&sine, 4.0) - part(&sine, 1.0) < -60.0);
}

// ---- utility ---------------------------------------------------------------

#[test]
fn a_fade_drawn_down_to_the_lowest_gain_ends_in_silence() {
    // A stop, as a song draws one: the gain held, then drawn down to the
    // bottom of its range over a fifth of a beat, and left there.
    let tone = tones(&[220.0], 0.5, 4.0);
    let mut song = Song::new();
    let (_, devices) = song.track("Band", &tone, &tone, &[("builtin:utility", &[])]);
    draw(&mut song, devices[0], "gain", &[(0.0, 0.0), (3.8, 0.0), (4.0, -70.0)]);
    let out = song.render(0.0);
    assert!((rms_db(&out.left[frames(1.0..1.8)]) - rms_db(&tone[frames(1.0..1.8)])).abs() < 0.01);
    // The fade is the line, one stretch of 256 frames behind it: the engine
    // reads an envelope for a device every 256 frames, and the gain goes
    // from one reading to the next in a straight line. Half way down, at
    // -35 dB on the line, it is where the line was 256 frames before
    // (3.7 dB up), and not where a hundredth of a second of smoothing on
    // top of that would have it (11 dB up).
    let half_way = (1.95 * SR) as usize;
    let around = half_way - 96..half_way + 96;
    let gain = rms_db(&out.left[around.clone()]) - rms_db(&tone[around]);
    assert!((gain + 35.0 - 3.7).abs() < 0.6, "{gain} dB half way down");
    // Two stretches after the line gets to the bottom there is nothing:
    // not the tone 70 dB down, which is what the lowest gain was before,
    // and not the tail of a filter.
    let bottom = (2.0 * SR) as usize;
    assert!(peak(&out.left[bottom - 256..bottom]) > 0.0, "still fading as the line ends");
    assert_eq!(peak(&out.left[bottom + 512..]), 0.0);
    // The same file as a build before format 4 wrote it: 70 dB down, as it was.
    let file = song.project.to_json();
    let now = format!("\"format_version\": {}", trill_core::project::FORMAT_VERSION);
    let old = Project::from_json(&file.replace(&now, "\"format_version\": 2")).unwrap();
    let out = render(&old, &BuiltinFactory, &RenderOptions { sample_rate: SR as u32, ..Default::default() }).unwrap();
    let left = rms_db(&out.left[frames(2.5..3.5)]) - rms_db(&tone[frames(2.5..3.5)]);
    assert!((left + 70.0).abs() < 0.05, "{left} dB");
}
