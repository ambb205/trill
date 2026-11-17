//! What was measured of the song's sound.
//!
//! The sound sets the picture's mood and nothing that has to land on a
//! beat: how bright the picture is, how much of it glows, how lit a lane
//! is. [`Heard`] is the sound at one moment; a [`Sound`] is a whole
//! recording measured ahead, which a film reads frame by frame. The window
//! makes a [`Heard`] from its meters instead.
//!
//! Levels are measured against the recording's own loud passages, not
//! against full scale: a quiet master and a loud one of the same song make
//! the same film.

use trill_core::TrackId;

/// Values kept per second of sound.
const RATE: f64 = 100.0;
/// Seconds of sound each kind of level looks at.
const BODY_WINDOW: f64 = 0.4;
const FAST_WINDOW: f64 = 0.05;
/// A level this many decibels under the loud passages is dark.
const BODY_RANGE: f32 = 28.0;
/// The fast level opens between these decibels under the loud passages.
const FAST_RANGE: (f32, f32) = (-44.0, -16.0);
/// Seconds the fast level takes to rise and to fall.
const FAST_RISE: f64 = 0.012;
const FAST_FALL: f64 = 0.14;
/// A recording whose loud passages are below this, in decibels, is silence.
const SILENCE_DB: f32 = -80.0;
/// A meter reading this many decibels under full scale is dark.
const METER_RANGE: f32 = 36.0;

/// The sound at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heard {
    /// How loud the mix is, 0..=1: the body of the sound, which moves
    /// slowly.
    pub level: f32,
    /// The same measured fast, 0..=1: it rises with every hit.
    pub punch: f32,
    /// The level of each track that was measured, 0..=1.
    pub tracks: Vec<(TrackId, f32)>,
}

impl Heard {
    /// Nothing sounds.
    pub fn silence() -> Self {
        Self::default()
    }

    /// From what meters show, in decibels under full scale: the mix, and
    /// each track.
    pub fn from_meters(mix_db: f32, tracks: impl IntoIterator<Item = (TrackId, f32)>) -> Self {
        let level = |db: f32| ((db + METER_RANGE) / METER_RANGE).clamp(0.0, 1.0);
        Self {
            level: level(mix_db),
            punch: level(mix_db),
            tracks: tracks.into_iter().map(|(track, db)| (track, level(db))).collect(),
        }
    }

    /// The level of `track`, if it was measured.
    pub fn track(&self, track: TrackId) -> Option<f32> {
        self.tracks.iter().find(|(id, _)| *id == track).map(|(_, level)| *level)
    }
}

/// A recording, measured for every hundredth of a second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sound {
    level: Vec<f32>,
    punch: Vec<f32>,
    /// Each track that was measured: how many values a second, and them.
    tracks: Vec<(TrackId, f64, Vec<f32>)>,
    /// Where in the song the recording begins, in seconds.
    starts: f64,
}

impl Sound {
    /// Measures a mix. `left` and `right` are its channels at
    /// `sample_rate`; a mono recording gives the same slice twice.
    pub fn measure(left: &[f32], right: &[f32], sample_rate: u32) -> Self {
        let power = running_power(left, right);
        let body = decibels(&windowed(&power, sample_rate, BODY_WINDOW));
        let fast = decibels(&windowed(&power, sample_rate, FAST_WINDOW));
        let top = loud_passages(&body);
        if top < SILENCE_DB {
            return Self { level: vec![0.0; body.len()], punch: vec![0.0; body.len()], ..Self::default() };
        }
        let level = body.iter().map(|db| ((db - top + BODY_RANGE) / BODY_RANGE).clamp(0.0, 1.0)).collect();
        let open: Vec<f32> = fast.iter().map(|db| smoothstep(FAST_RANGE.0, FAST_RANGE.1, db - top)).collect();
        Self { level, punch: eased(&open, FAST_RISE, FAST_FALL), ..Self::default() }
    }

    /// The same recording, as one that begins `seconds` into the song: a
    /// render of a stretch that does not start at the first bar.
    pub fn starting_at(mut self, seconds: f64) -> Self {
        self.starts = seconds;
        self
    }

    /// Adds what one track sounds like on its own, measured against its
    /// own loud passages: a quiet part is as lit while it plays as a loud
    /// one.
    pub fn add_track(&mut self, track: TrackId, left: &[f32], right: &[f32], sample_rate: u32) {
        let body = decibels(&windowed(&running_power(left, right), sample_rate, BODY_WINDOW / 4.0));
        let top = loud_passages(&body);
        let level = match top < SILENCE_DB {
            true => vec![0.0; body.len()],
            false => body.iter().map(|db| ((db - top + BODY_RANGE) / BODY_RANGE).clamp(0.0, 1.0)).collect(),
        };
        self.tracks.retain(|(id, ..)| *id != track);
        self.tracks.push((track, RATE, level));
    }

    /// Adds a track from its level as it went, in decibels, `per_second`
    /// values a second: what a render measures of every track as it makes
    /// the mix, with no audio of the track kept. Measured against the
    /// track's own loud passages, as [`Sound::add_track`] does.
    pub fn add_track_levels(&mut self, track: TrackId, decibels: &[f32], per_second: f64) {
        let top = loud_passages(decibels);
        let level = match top < SILENCE_DB {
            true => vec![0.0; decibels.len()],
            false => decibels.iter().map(|db| ((db - top + BODY_RANGE) / BODY_RANGE).clamp(0.0, 1.0)).collect(),
        };
        self.tracks.retain(|(id, ..)| *id != track);
        self.tracks.push((track, per_second.max(1e-6), level));
    }

    /// How long the recording is, in seconds.
    pub fn duration(&self) -> f64 {
        self.level.len() as f64 / RATE
    }

    /// The sound `seconds` into the song. Before the recording and after
    /// it there is silence.
    pub fn heard(&self, seconds: f64) -> Heard {
        let seconds = seconds - self.starts;
        Heard {
            level: at(&self.level, seconds, RATE),
            punch: at(&self.punch, seconds, RATE),
            tracks: self.tracks.iter().map(|(track, rate, level)| (*track, at(level, seconds, *rate))).collect(),
        }
    }
}

/// The value `seconds` in, between the two measurements it falls between.
fn at(values: &[f32], seconds: f64, rate: f64) -> f32 {
    let place = seconds * rate;
    if place < 0.0 || values.is_empty() {
        return 0.0;
    }
    let index = place.floor() as usize;
    let (Some(before), after) = (values.get(index), values.get(index + 1)) else { return 0.0 };
    let after = after.copied().unwrap_or(0.0);
    before + (after - before) * (place - index as f64) as f32
}

/// The power of the two channels' mean, summed from the start: the mean
/// power of any stretch is then one subtraction.
fn running_power(left: &[f32], right: &[f32]) -> Vec<f64> {
    let mut total = 0.0f64;
    let mut sums = Vec::with_capacity(left.len().min(right.len()) + 1);
    sums.push(0.0);
    for (l, r) in left.iter().zip(right) {
        let mono = (*l as f64 + *r as f64) / 2.0;
        total += mono * mono;
        sums.push(total);
    }
    sums
}

/// The level around each measured moment, over `seconds` of sound.
fn windowed(power: &[f64], sample_rate: u32, seconds: f64) -> Vec<f32> {
    let samples = power.len() - 1;
    let count = (samples as f64 / sample_rate as f64 * RATE).ceil() as usize;
    let half = ((seconds * sample_rate as f64 / 2.0) as usize).max(1);
    (0..count)
        .map(|index| {
            let center = (index as f64 / RATE * sample_rate as f64).round() as usize;
            let (low, high) = (center.saturating_sub(half).min(samples), (center + half).min(samples));
            (((power[high] - power[low]) / (high - low).max(1) as f64).sqrt()) as f32
        })
        .collect()
}

fn decibels(levels: &[f32]) -> Vec<f32> {
    levels.iter().map(|level| 20.0 * (level + 1e-6).log10()).collect()
}

/// The level the recording's loud passages reach: all but its loudest
/// twentieth is under it.
fn loud_passages(decibels: &[f32]) -> f32 {
    if decibels.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mut sorted = decibels.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted[((sorted.len() - 1) as f64 * 0.95).round() as usize]
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let part = ((value - low) / (high - low)).clamp(0.0, 1.0);
    part * part * (3.0 - 2.0 * part)
}

/// Follows `values`, taking about `rise` seconds to climb to them and
/// `fall` to drop.
fn eased(values: &[f32], rise: f64, fall: f64) -> Vec<f32> {
    let (up, down) = (1.0 - (-1.0 / (RATE * rise)).exp() as f32, 1.0 - (-1.0 / (RATE * fall)).exp() as f32);
    let mut now = values.first().copied().unwrap_or(0.0);
    values
        .iter()
        .map(|value| {
            now += (value - now) * if *value > now { up } else { down };
            now
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE_HZ: u32 = 8_000;

    /// A tone at `gain` for `seconds`.
    fn tone(gain: f32, seconds: f64) -> Vec<f32> {
        (0..(seconds * RATE_HZ as f64) as usize).map(|index| gain * (index as f32 * 0.5).sin()).collect()
    }

    /// Two seconds quiet, two loud, two silent.
    fn recording(loud: f32) -> Vec<f32> {
        let mut samples = tone(loud * 0.05, 2.0);
        samples.extend(tone(loud, 2.0));
        samples.extend(vec![0.0; 2 * RATE_HZ as usize]);
        samples
    }

    #[test]
    fn the_level_follows_the_sound_and_silence_is_dark() {
        let samples = recording(0.5);
        let sound = Sound::measure(&samples, &samples, RATE_HZ);
        assert_eq!(sound.duration(), 6.0);
        let (quiet, loud, silent) = (sound.heard(1.0).level, sound.heard(3.0).level, sound.heard(5.5).level);
        assert!((loud - 1.0).abs() < 0.02, "the loud passage is what the rest is measured against: {loud}");
        // Twenty-six decibels under it, of twenty-eight.
        assert!(quiet > 0.03 && quiet < 0.12, "{quiet}");
        assert_eq!(silent, 0.0);
        assert_eq!(sound.heard(-1.0), Heard::silence());
        assert_eq!(sound.heard(60.0).level, 0.0);
    }

    #[test]
    fn a_quiet_master_makes_the_same_film_as_a_loud_one() {
        let (loud, quiet) = (recording(0.8), recording(0.08));
        let (loud, quiet) = (Sound::measure(&loud, &loud, RATE_HZ), Sound::measure(&quiet, &quiet, RATE_HZ));
        for seconds in [0.5, 1.0, 2.5, 3.0, 4.5] {
            let (a, b) = (loud.heard(seconds), quiet.heard(seconds));
            assert!((a.level - b.level).abs() < 0.01 && (a.punch - b.punch).abs() < 0.01, "at {seconds}: {a:?} {b:?}");
        }
    }

    #[test]
    fn the_fast_level_jumps_with_a_hit_and_falls_slower() {
        let mut samples = vec![0.0f32; RATE_HZ as usize];
        samples.extend(tone(0.5, 0.2));
        samples.extend(vec![0.0f32; RATE_HZ as usize]);
        let sound = Sound::measure(&samples, &samples, RATE_HZ);
        assert!(sound.heard(0.9).punch < 0.01);
        assert!(sound.heard(1.08).punch > 0.9, "up within a few hundredths: {}", sound.heard(1.08).punch);
        let after = sound.heard(1.2 + 0.14).punch;
        assert!(after > 0.2 && after < 0.6, "and down over a few tenths: {after}");
    }

    #[test]
    fn a_track_is_measured_against_its_own_loudest() {
        let mix = recording(0.5);
        let mut sound = Sound::measure(&mix, &mix, RATE_HZ);
        let part = recording(0.01);
        sound.add_track(TrackId(7), &part, &part, RATE_HZ);
        let heard = sound.heard(3.0);
        assert!(heard.track(TrackId(7)).unwrap() > 0.95, "{heard:?}");
        assert_eq!(heard.track(TrackId(8)), None, "a track that was not measured says nothing");
        assert_eq!(sound.heard(5.5).track(TrackId(7)), Some(0.0));
    }

    #[test]
    fn a_track_is_lit_from_the_levels_a_render_measured_of_it() {
        let mix = recording(0.5);
        // Ten values a second, as a render gives them: two seconds at
        // -40 dB, two at -14, two of silence.
        let levels: Vec<f32> = (0..60).map(|step| [-40.0, -14.0, -120.0][step / 20]).collect();
        let mut sound = Sound::measure(&mix, &mix, RATE_HZ);
        sound.add_track_levels(TrackId(3), &levels, 10.0);
        assert_eq!(sound.heard(3.0).track(TrackId(3)), Some(1.0), "its loud passage is what it is measured against");
        let quiet = sound.heard(1.0).track(TrackId(3)).unwrap();
        assert!((quiet - 2.0 / 28.0).abs() < 0.01, "26 dB under it, of 28: {quiet}");
        assert_eq!(sound.heard(5.0).track(TrackId(3)), Some(0.0));

        // A render of a stretch that begins ten seconds into the song is
        // heard there, and not before.
        let later = sound.starting_at(10.0);
        assert_eq!(later.heard(13.0).track(TrackId(3)), Some(1.0));
        assert!((later.heard(13.0).level - 1.0).abs() < 0.02);
        assert_eq!(later.heard(3.0), Heard { level: 0.0, punch: 0.0, tracks: vec![(TrackId(3), 0.0)] });
    }

    #[test]
    fn a_silent_recording_is_dark_throughout() {
        let silence = vec![0.0f32; 2 * RATE_HZ as usize];
        let sound = Sound::measure(&silence, &silence, RATE_HZ);
        assert_eq!(sound.heard(1.0), Heard { level: 0.0, punch: 0.0, tracks: Vec::new() });
        assert_eq!(Sound::default().heard(1.0), Heard::silence());
    }

    #[test]
    fn meters_are_read_as_levels() {
        let heard = Heard::from_meters(-9.0, [(TrackId(1), -72.0), (TrackId(2), 0.0)]);
        assert!((heard.level - 0.75).abs() < 1e-6);
        assert_eq!((heard.track(TrackId(1)), heard.track(TrackId(2))), (Some(0.0), Some(1.0)));
    }
}
