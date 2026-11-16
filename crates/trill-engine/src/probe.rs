//! Probing an instrument: playing it notes one at a time and measuring
//! what comes back, to find out what it does without writing anything
//! into the song.
//!
//! This is how a caller that cannot listen learns an instrument it has
//! not met: which notes it plays at all, at what pitch they really sound,
//! how soon and for how long, how bright and how wide, and what a held
//! key (a keyswitch) changes about that. A probe makes devices of its
//! own, as a render does: copies of a track's instrument as it is now,
//! with or without the effects after it, or an instrument that is not in
//! the song yet. Nothing of the song is touched, nothing is heard, and no
//! file is written.
//!
//! The devices are waited for until they answer ([`crate::ready`]), then
//! each note is played by itself from silence: whatever the note before
//! left ringing is waited out, or cut off, first.
//!
//! An instrument that is to start from a preset only it can load is made,
//! given the preset, asked for its state and made again from that: what
//! is probed is then what a song would hold of it, and it has nothing of
//! the sound it began with still loaded.

use crate::analysis::{Analyzer, level_db};
use crate::graph::MAX_BLOCK;
use crate::ready::{OPENING_NOTES, Opening, OpeningNote, Plugins, Stand, Wait, device_signature, signature, wait_for};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{DeviceFactory, DeviceInstance, DeviceProcessor, Event, EventKind, Purpose};
use trill_core::presets::PresetInfo;
use trill_core::{ControllerKind, Device};

/// How far below its own peak a sound has to fall to be over, in dB.
const OVER_DB: f32 = 40.0;
/// The steps a sound's level is followed in, in seconds.
const STEP_SECONDS: f64 = 0.005;
/// How long the output must stay down after a note before the next is
/// played.
const QUIET_SECONDS: f64 = 0.05;
/// How long at most a note that is still ringing when the listening
/// stops is waited out before the next one is played.
const RING_OUT_SECONDS: f64 = 4.0;
/// How long a key that is held with a note (a keyswitch) is down before
/// the note.
const WITH_SECONDS: f64 = 0.03;
/// How long after its beginning a sound's pitch is listened for (the
/// pick, the hammer and the breath at the start of a note have none), and
/// for how long at most.
const PITCH_AFTER_SECONDS: f64 = 0.03;
const PITCH_SECONDS: f64 = 0.2;
/// The pitches that can be told: from the lowest note of a five-string
/// bass down an octave, up to the top of a piano.
const LOWEST_HZ: f64 = 20.0;
const HIGHEST_HZ: f64 = 4200.0;
/// How well a sound has to match itself a period later to have a pitch.
/// Noise comes nowhere near; a plucked string that is dying away does.
const CLEAR_PITCH: f64 = 0.7;

/// A note to play.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeNote {
    pub pitch: u8,
    /// 1..=127.
    pub velocity: u8,
    /// Keys held down with it, pressed just before: keyswitches.
    pub with: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ProbeOptions {
    pub sample_rate: u32,
    /// How long each note is held.
    pub hold_seconds: f64,
    /// How long at most a note is listened to after it is let go. The
    /// next note is played as soon as this one has died away.
    pub tail_seconds: f64,
    /// Keys held down through the whole probe.
    pub held: Vec<u8>,
    /// Where the controllers stand through the whole probe.
    pub controllers: Vec<(ControllerKind, f32)>,
    /// How long the instrument may take to answer before the probe goes
    /// ahead regardless.
    pub patience: Duration,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            hold_seconds: 0.5,
            tail_seconds: 1.5,
            held: Vec::new(),
            controllers: Vec::new(),
            patience: Duration::from_secs(10),
        }
    }
}

/// What came of one note.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub note: ProbeNote,
    /// Whether anything was heard at all.
    pub sounded: bool,
    /// Whether the keys held with the note made a sound of their own
    /// before it: they are notes, then, and not switches.
    pub with_sounded: bool,
    /// The highest sample, dBFS.
    pub peak_db: f32,
    /// The pitch that was heard, as a MIDI note number with its fraction
    /// (60.0 is middle C); `None` for a sound without one, such as a drum.
    pub pitch: Option<f64>,
    /// How long after the key went down the sound began.
    pub onset_seconds: Option<f64>,
    /// How long the sound lasted from its beginning, until it had fallen
    /// 40 dB below its peak.
    pub rings_seconds: f64,
    /// Whether it was still sounding when the listening stopped:
    /// `rings_seconds` is then the least it rings.
    pub still_ringing: bool,
    /// The centre of its spectrum in Hz: how bright it is.
    pub brightness_hz: Option<f64>,
    /// How much of it is the difference between left and right: 0 for
    /// mono, about a half for two unrelated channels.
    pub width: f32,
}

/// Whether devices are ready to be played: see
/// [`ProbeJob::wait_until_ready`].
#[derive(Clone, Debug, PartialEq)]
pub struct Readiness {
    /// Whether they answered what they were given, and went on answering
    /// it alike with nothing loading. `false` after `seconds` of waiting
    /// means they may still be loading, or do not play those notes at all.
    pub ready: bool,
    /// How long it took.
    pub seconds: f64,
    /// The pitches that made no sound to the end, though others did: it
    /// does not play them, or had not loaded them after all that time.
    pub silent: Vec<u8>,
}

/// What a probe found.
#[derive(Clone, Debug, PartialEq)]
pub struct Probed {
    /// One entry per note asked for, in order.
    pub heard: Vec<Heard>,
    /// How long the instrument was waited for before the first note.
    pub waited_seconds: f64,
    /// Whether it answered while it was waited for. If not, it may not
    /// have been ready, or none of the notes asked for is one it plays.
    pub answered: bool,
    /// The pitches that made no sound while it was waited for, though
    /// others did, and that it was not known to be silent on: it was held
    /// to that for half the patience it was given. It does not play them,
    /// or had still not loaded them.
    pub waited_for_silent: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("{0}")]
    Device(String),
}

/// What to play through, taken from a session so that the probe runs
/// without holding on to it.
pub struct ProbeJob {
    /// The devices in the order the notes go through them: the
    /// instrument, then any effects.
    pub devices: Vec<Device>,
    pub factory: Arc<dyn DeviceFactory>,
    /// A preset for the instrument to load once it exists: one that only
    /// the plugin itself can load.
    pub preset: Option<PresetInfo>,
    pub tempo: f64,
}

impl ProbeJob {
    /// Plays `notes` one at a time and measures each.
    pub fn run(&self, notes: &[ProbeNote], options: &ProbeOptions) -> Result<Probed, ProbeError> {
        let mut probing = self.open(notes, options)?;
        let heard = probing.play(notes);
        Ok(Probed {
            heard,
            waited_seconds: probing.waited_seconds,
            answered: probing.answered,
            waited_for_silent: probing.silent.clone(),
        })
    }

    /// Makes the devices and waits for them to be ready to play `first`,
    /// the notes that will be asked of them (or a part of them). What
    /// comes back plays notes for as long as it is kept: several rounds
    /// through one instrument that was made and waited for once.
    pub fn open(&self, first: &[ProbeNote], options: &ProbeOptions) -> Result<Probing, ProbeError> {
        // The instrument is asked a spread of the notes, with what is held
        // with each, until it answers and goes on answering alike.
        let standing: Vec<EventKind> = options.controllers.iter().map(|(kind, value)| kind.event(*value)).collect();
        let step = first.len().div_ceil(OPENING_NOTES).max(1);
        let mut opening = Opening::default();
        for note in first.iter().step_by(step) {
            for pitch in options.held.iter().chain(&note.with) {
                opening.notes.push(OpeningNote {
                    pitch: *pitch,
                    velocity: velocity_of(100),
                    controllers: standing.clone(),
                    switch: true,
                });
            }
            opening.notes.push(OpeningNote {
                pitch: note.pitch,
                velocity: velocity_of(note.velocity),
                controllers: standing.clone(),
                switch: false,
            });
        }
        self.open_asking(opening, options)
    }

    /// Whether the devices are ready to be played, found out on copies of
    /// them made now: they are given `pitches` (or, with none, a burst of
    /// noise to pass, which is how an effect is asked) until they answer
    /// and have stopped changing, as before a render ([`crate::ready`]).
    /// Waits as long as that takes, up to two minutes while there are
    /// signs of loading; `patience` is how long nothing at all may happen.
    pub fn wait_until_ready(&self, pitches: &[u8], patience: Duration) -> Result<Readiness, ProbeError> {
        let note = |pitch: &u8| OpeningNote {
            pitch: *pitch,
            velocity: velocity_of(100),
            controllers: Vec::new(),
            switch: false,
        };
        let opening = Opening { notes: pitches.iter().map(note).collect(), signal: pitches.is_empty() };
        let options = ProbeOptions { patience, ..ProbeOptions::default() };
        let probing = self.open_asking(opening, &options)?;
        Ok(Readiness { ready: probing.answered, seconds: probing.waited_seconds, silent: probing.silent })
    }

    fn open_asking(&self, opening: Opening, options: &ProbeOptions) -> Result<Probing, ProbeError> {
        let sample_rate = f64::from(options.sample_rate);
        let mut instances: Vec<DeviceInstance> = Vec::with_capacity(self.devices.len());
        let mut devices = self.devices.clone();
        for (index, device) in devices.iter_mut().enumerate() {
            let failed = |reason: String| ProbeError::Device(format!("{} ({}): {reason}", device.name, device.uri));
            let make = |device: &Device| {
                let made = self.factory.create_for(device, sample_rate, MAX_BLOCK, Purpose::Rendering);
                made.map_err(|error| failed(error.to_string()))
            };
            let mut instance = make(device)?;
            if let (0, Some(preset)) = (index, &self.preset) {
                let loaded = match instance.controller.as_mut() {
                    Some(controller) => controller.load_preset(preset).map_err(|error| error.to_string()),
                    None => Err("it cannot load presets".to_string()),
                };
                loaded.map_err(|reason| failed(format!("did not load the preset '{}': {reason}", preset.name)))?;
                // A sampler that is handed another kit goes on playing
                // drums of the one it had for a moment, with nothing to
                // show for it: made again from the state it now has, it
                // loads the preset's sound and no other.
                let state = instance.controller.as_mut().and_then(|controller| controller.save_state());
                if let Some(state) = state {
                    drop(instance);
                    device.state = Some(state);
                    instance = make(device)?;
                }
            }
            instances.push(instance);
        }

        // Where the controllers stand, and the keys held throughout.
        let standing: Vec<EventKind> = options.controllers.iter().map(|(kind, value)| kind.event(*value)).collect();
        let mut setting: Vec<Event> = standing.iter().map(|kind| Event { offset: 0, kind: *kind }).collect();
        setting.extend(
            options.held.iter().map(|pitch| Event {
                offset: 0,
                kind: EventKind::NoteOn { pitch: *pitch, velocity: velocity_of(100) },
            }),
        );
        let mut probing = Probing {
            instances,
            options: options.clone(),
            tempo: self.tempo,
            noise: 0.0,
            setting,
            waited_seconds: 0.0,
            answered: true,
            silent: Vec::new(),
        };

        let plugins = devices.iter().any(|device| Plugins::is_plugin(&device.uri));
        let mut stand = probing.stand();
        if plugins {
            let signature = signature(devices.iter(), &opening);
            let mut waits = [Wait::new(stand, opening, signature, device_signature(devices.iter()))];
            wait_for(&mut waits, options.patience);
            let [wait] = waits;
            let (waited, answered) = wait.outcome();
            let silent = wait.silent().to_vec();
            stand = wait.stand;
            let noise = stand.noise();
            (probing.noise, probing.waited_seconds, probing.answered) = (noise, waited.as_secs_f64(), answered);
            probing.silent = silent;
        } else {
            stand.hear_noise();
            probing.noise = stand.noise();
        }
        Ok(probing)
    }
}

fn velocity_of(velocity: u8) -> f32 {
    f32::from(velocity.clamp(1, 127)) / 127.0
}

/// An instrument that was made for a probe and is ready to be played.
pub struct Probing {
    instances: Vec<DeviceInstance>,
    options: ProbeOptions,
    tempo: f64,
    /// The most the devices put out when they were given nothing.
    noise: f32,
    /// The controllers and the keys held throughout.
    setting: Vec<Event>,
    /// How long the instrument was waited for before the first note.
    pub waited_seconds: f64,
    /// Whether it answered while it was waited for.
    pub answered: bool,
    /// The pitches that made no sound while it was waited for, though
    /// others did: see [`Probed::waited_for_silent`].
    pub silent: Vec<u8>,
}

impl Probing {
    fn stand(&mut self) -> Stand<'_> {
        let processors: Vec<&mut Box<dyn DeviceProcessor>> =
            self.instances.iter_mut().map(|instance| &mut instance.processor).collect();
        let mut stand = Stand::new(processors, f64::from(self.options.sample_rate), self.tempo);
        stand.set_noise(self.noise);
        stand
    }

    /// Plays `notes` one at a time, each from silence, and measures each.
    pub fn play(&mut self, notes: &[ProbeNote]) -> Vec<Heard> {
        let (options, setting) = (self.options.clone(), self.setting.clone());
        let mut stand = self.stand();
        stand.run(&setting, 0.02);
        let heard = notes.iter().map(|note| play(&mut stand, note, &options, &setting)).collect();
        // The devices are left in silence, with nothing held.
        stand.settle();
        heard
    }
}

/// Plays one note from silence and measures it.
fn play(stand: &mut Stand, note: &ProbeNote, options: &ProbeOptions, setting: &[Event]) -> Heard {
    let rate = stand.sample_rate();
    // What is still ringing from before is not this note's.
    let before = stand.run(&[], 0.01);
    let floor = stand.heard_above().max(before * 2.0);

    let velocity = velocity_of;
    let mut with_sounded = false;
    if !note.with.is_empty() {
        let keys: Vec<Event> = note
            .with
            .iter()
            .map(|pitch| Event { offset: 0, kind: EventKind::NoteOn { pitch: *pitch, velocity: velocity(100) } })
            .collect();
        with_sounded = stand.run(&keys, WITH_SECONDS) > floor;
    }

    let (mut left, mut right): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let on = [Event { offset: 0, kind: EventKind::NoteOn { pitch: note.pitch, velocity: velocity(note.velocity) } }];
    stand.run_into(&on, stand.frames(options.hold_seconds.max(0.01)), |l, r| {
        left.extend_from_slice(l);
        right.extend_from_slice(r);
        true
    });

    // Let go, and listen until it has died away or long enough.
    let mut off = vec![Event { offset: 0, kind: EventKind::NoteOff { pitch: note.pitch } }];
    off.extend(note.with.iter().map(|pitch| Event { offset: 0, kind: EventKind::NoteOff { pitch: *pitch } }));
    let quiet_frames = stand.frames(QUIET_SECONDS);
    // Quiet is quiet by the devices' own measure, not by what the note
    // before left ringing: a note that is taken to be over while it still
    // rings leaves more ringing for the next, and so on up.
    let peak_so_far = left.iter().chain(&right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let quiet = stand.heard_above().max(peak_so_far * 0.003);
    let mut silent = 0;
    stand.run_into(&off, stand.frames(options.tail_seconds.max(QUIET_SECONDS)), |l, r| {
        left.extend_from_slice(l);
        right.extend_from_slice(r);
        let loud = l.iter().chain(r).any(|sample| sample.abs() > quiet);
        silent = if loud { 0 } else { silent + l.len() };
        silent < quiet_frames
    });
    if silent < quiet_frames {
        // Still ringing. What rings is waited out, as far as that goes,
        // so that the next note starts from silence: a quiet hit cannot be
        // measured under a cymbal. (Nothing cuts a sampler's tail short: a
        // reset is not a thing every plugin acts on, and some take one
        // badly in the middle of playing.)
        let mut all_off = vec![Event { offset: 0, kind: EventKind::AllNotesOff }];
        all_off.extend_from_slice(setting);
        let mut silent = 0;
        stand.run_into(&all_off, stand.frames(RING_OUT_SECONDS), |l, r| {
            let loud = l.iter().chain(r).any(|sample| sample.abs() > quiet);
            silent = if loud { 0 } else { silent + l.len() };
            silent < quiet_frames
        });
    }

    let held = stand.frames(options.hold_seconds.max(0.01));
    let measured = measure(&left, &right, rate, floor, Some(held));
    Heard {
        note: note.clone(),
        sounded: measured.sounded,
        with_sounded,
        peak_db: level_db(measured.peak),
        pitch: measured.pitch,
        onset_seconds: measured.onset_seconds,
        rings_seconds: measured.rings_seconds,
        still_ringing: measured.still_ringing,
        brightness_hz: measured.brightness_hz,
        width: measured.width,
    }
}

/// What a stretch of audio that began with a key going down holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteSound {
    pub sounded: bool,
    /// The highest sample, linear.
    pub peak: f32,
    pub pitch: Option<f64>,
    pub onset_seconds: Option<f64>,
    pub rings_seconds: f64,
    pub still_ringing: bool,
    pub brightness_hz: Option<f64>,
    pub width: f32,
}

/// Measures the sound of one note: `left` and `right` from the moment its
/// key went down. Anything at or below `floor` is not sound. `held` is how
/// many frames the key stayed down, if it came up: the pitch is read
/// while the note is held, not while it is being damped.
pub fn measure(left: &[f32], right: &[f32], sample_rate: f64, floor: f32, held: Option<usize>) -> NoteSound {
    let frames = left.len().min(right.len());
    let (left, right) = (&left[..frames], &right[..frames]);
    let level = |frame: usize| left[frame].abs().max(right[frame].abs());
    let peak = (0..frames).fold(0.0f32, |peak, frame| peak.max(level(frame)));
    if peak <= floor {
        return NoteSound { peak, ..NoteSound::default() };
    }
    // The sound is there while it is above this.
    let there = floor.max(peak * 10f32.powf(-OVER_DB / 20.0));
    let onset = (0..frames).find(|frame| level(*frame) > there).unwrap_or(0);
    let step = ((STEP_SECONDS * sample_rate) as usize).max(1);
    let last_step = (0..frames.div_ceil(step))
        .rev()
        .find(|index| (index * step..((index + 1) * step).min(frames)).any(|frame| level(frame) > there))
        .unwrap_or(0);
    let end = ((last_step + 1) * step).min(frames);
    let still_ringing = end + 2 * step >= frames;

    let (mut mid, mut side) = (0.0f64, 0.0f64);
    for frame in 0..frames {
        let (l, r) = (f64::from(left[frame]), f64::from(right[frame]));
        mid += (l + r) * (l + r);
        side += (l - r) * (l - r);
    }
    let width = if mid + side > 0.0 { (side / (mid + side)) as f32 } else { 0.0 };

    let mut spectrum = Analyzer::new(sample_rate as u32);
    spectrum.push(left, right);
    // A sound shorter than the spectrum's window is measured all the same.
    let padding = vec![0.0f32; 4096usize.saturating_sub(frames) + 4096];
    spectrum.push(&padding, &padding);

    let sounding = held.map_or(end, |held| held.clamp(onset, end));
    let from = (onset + (PITCH_AFTER_SECONDS * sample_rate) as usize).min(sounding);
    let to = (from + (PITCH_SECONDS * sample_rate) as usize).min(sounding);
    let mono: Vec<f32> = (from..to).map(|frame| (left[frame] + right[frame]) * 0.5).collect();

    NoteSound {
        sounded: true,
        peak,
        pitch: pitch_of(&mono, sample_rate).map(|hz| 69.0 + 12.0 * (hz / 440.0).log2()),
        onset_seconds: Some(onset as f64 / sample_rate),
        rings_seconds: (end - onset) as f64 / sample_rate,
        still_ringing,
        brightness_hz: spectrum.brightness_hz(),
        width,
    }
}

/// How well `samples` match themselves `lag` frames later, from -1 to 1
/// (the normalized square difference of McLeod and Wyvill).
fn likeness(samples: &[f32], lag: usize) -> f64 {
    let (mut product, mut energy) = (0.0f64, 0.0f64);
    for (a, b) in samples.iter().zip(&samples[lag..]) {
        let (a, b) = (f64::from(*a), f64::from(*b));
        product += a * b;
        energy += a * a + b * b;
    }
    if energy > 0.0 { 2.0 * product / energy } else { 0.0 }
}

/// The pitch of `samples` in Hz, if they have one: the shortest period
/// after which the sound nearly repeats.
pub fn pitch_of(samples: &[f32], sample_rate: f64) -> Option<f64> {
    // Found at half the rate, which is four times less work, and then
    // looked at closely at the full one.
    let halved: Vec<f32> = samples.chunks_exact(2).map(|pair| (pair[0] + pair[1]) * 0.5).collect();
    let rate = sample_rate / 2.0;
    let shortest = ((rate / HIGHEST_HZ).floor() as usize).max(2);
    let longest = ((rate / LOWEST_HZ).ceil() as usize).min(halved.len() / 2);
    if longest <= shortest + 2 {
        return None;
    }
    let scores: Vec<f64> = (0..=longest).map(|lag| if lag < shortest { 0.0 } else { likeness(&halved, lag) }).collect();
    // The peaks of the curve, one between each pair of its crossings from
    // below to above zero. The first lags lie on the slope down from a
    // lag of nothing, where every sound matches itself; they are no peak.
    let mut peaks: Vec<usize> = Vec::new();
    let mut lag = shortest;
    while lag < longest && scores[lag] > 0.0 {
        lag += 1;
    }
    let mut best: Option<usize> = None;
    for lag in lag..longest {
        if scores[lag] <= 0.0 {
            peaks.extend(best.take());
        } else if scores[lag] >= scores[lag - 1]
            && scores[lag] >= scores[lag + 1]
            && best.is_none_or(|best| scores[lag] > scores[best])
        {
            best = Some(lag);
        }
    }
    peaks.extend(best);
    let highest = peaks.iter().map(|lag| scores[*lag]).fold(0.0f64, f64::max);
    if highest < CLEAR_PITCH {
        return None;
    }
    // The first that is nearly as good as the best: a later one is the
    // same wave an octave or more down.
    let coarse = *peaks.iter().find(|lag| scores[**lag] >= 0.9 * highest)?;

    // At the full rate, around twice that lag. Half a rate is too coarse
    // to see a short period by: what was found may be two, three or four
    // periods of the wave, so the shorter ones are looked at too, and the
    // shortest that matches nearly as well is the pitch.
    let (found, score) = closely(samples, 2.0 * coarse as f64)?;
    let shorter = [4.0, 3.0, 2.0].into_iter().filter_map(|part| {
        let lag = 2.0 * coarse as f64 / part;
        (lag >= sample_rate / HIGHEST_HZ).then(|| closely(samples, lag)).flatten()
    });
    let period = shorter.filter(|(_, likeness)| *likeness >= 0.9 * score).map(|(period, _)| period).next();
    Some(sample_rate / period.unwrap_or(found))
}

/// The lag near `around` at which `samples` best match themselves, to a
/// fraction of a frame, and how well they do there; `None` if the best
/// match in reach is not a peak.
fn closely(samples: &[f32], around: f64) -> Option<(f64, f64)> {
    let first = (around.floor() as usize).saturating_sub(2).max(2);
    let last = (around.ceil() as usize + 2).min(samples.len() / 2);
    if last <= first {
        return None;
    }
    // One lag beyond each end, to tell a peak from a slope.
    let scores: Vec<f64> = (first - 1..=last + 1).map(|lag| likeness(samples, lag)).collect();
    let (index, _) = scores[1..scores.len() - 1].iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let (before, at, after) = (scores[index], scores[index + 1], scores[index + 2]);
    if at < before || at < after {
        return None;
    }
    let bend = before - 2.0 * at + after;
    let offset = if bend.abs() > 1e-12 { (0.5 * (before - after) / bend).clamp(-1.0, 1.0) } else { 0.0 };
    Some(((first + index) as f64 + offset, at - 0.25 * (before - after) * offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: f64 = 48_000.0;

    fn tone(hz: f64, seconds: f64, shape: impl Fn(f64) -> f64) -> Vec<f32> {
        (0..(seconds * RATE) as usize).map(|frame| shape(TAU * hz * frame as f64 / RATE) as f32).collect()
    }

    fn midi(hz: f64) -> f64 {
        69.0 + 12.0 * (hz / 440.0).log2()
    }

    #[test]
    fn the_pitch_of_a_tone_is_told_to_within_a_few_cents_across_the_keyboard() {
        for note in [16, 23, 28, 40, 52, 60, 69, 81, 96, 105] {
            let hz = 440.0 * 2f64.powf((f64::from(note) - 69.0) / 12.0);
            let heard = pitch_of(&tone(hz, 0.2, f64::sin), RATE).unwrap_or_else(|| panic!("no pitch at {hz} Hz"));
            assert!((midi(heard) - f64::from(note)).abs() < 0.06, "{note}: heard {} ({heard} Hz)", midi(heard));
            // A wave rich in overtones has the pitch of its fundamental,
            // also when that is the weakest thing in it.
            let rich = |phase: f64| 0.15 * phase.sin() + 0.5 * (2.0 * phase).sin() + 0.35 * (3.0 * phase).sin();
            let heard = pitch_of(&tone(hz, 0.2, rich), RATE).unwrap_or_else(|| panic!("no pitch at {hz} Hz, rich"));
            assert!((midi(heard) - f64::from(note)).abs() < 0.1, "{note}, rich: heard {}", midi(heard));
        }
    }

    #[test]
    fn noise_and_silence_have_no_pitch() {
        let mut seed = 0x2545_F491u32;
        let noise: Vec<f32> = (0..9_600)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect();
        assert_eq!(pitch_of(&noise, RATE), None);
        assert_eq!(pitch_of(&[0.0; 9_600], RATE), None);
        assert_eq!(pitch_of(&[0.5; 40], RATE), None, "too short to tell");
    }

    #[test]
    fn a_note_is_measured_from_the_moment_its_key_went_down() {
        // 10 ms of nothing, then a tone that dies away: half as loud every
        // 50 ms, a little wider on the right.
        let lead = (0.01 * RATE) as usize;
        let sound = |frame: usize, gain: f64| {
            if frame < lead {
                return 0.0;
            }
            let seconds = (frame - lead) as f64 / RATE;
            (0.5 * gain * 0.5f64.powf(seconds / 0.05) * (TAU * 220.0 * seconds).sin()) as f32
        };
        let frames = (1.0 * RATE) as usize;
        let left: Vec<f32> = (0..frames).map(|frame| sound(frame, 1.0)).collect();
        let right: Vec<f32> = (0..frames).map(|frame| sound(frame, 0.5)).collect();
        let heard = measure(&left, &right, RATE, 1e-4, None);
        assert!(heard.sounded);
        assert!((level_db(heard.peak) + 6.3).abs() < 0.5, "{}", level_db(heard.peak));
        assert!((heard.onset_seconds.unwrap() - 0.01).abs() < 0.002, "{:?}", heard.onset_seconds);
        // 40 dB down is 6.64 halvings: a third of a second.
        assert!((heard.rings_seconds - 0.332).abs() < 0.02, "{}", heard.rings_seconds);
        assert!(!heard.still_ringing);
        assert!((heard.pitch.unwrap() - 57.0).abs() < 0.05, "A3: {:?}", heard.pitch);
        let bright = heard.brightness_hz.unwrap();
        assert!((200.0..260.0).contains(&bright), "a pure tone is as bright as it is high: {bright}");
        // Side over mid plus side for gains of 1 and a half: 0.25 / 2.5.
        assert!((heard.width - 0.1).abs() < 0.01, "{}", heard.width);

        // The same tone held to the end is still ringing when the
        // listening stops.
        let held = tone(220.0, 0.5, |phase| 0.5 * phase.sin());
        let heard = measure(&held, &held, RATE, 1e-4, None);
        assert!(heard.still_ringing && (heard.rings_seconds - 0.5).abs() < 0.01, "{heard:?}");
        assert_eq!(heard.width, 0.0, "mono");

        // What does not rise above the floor is no sound.
        let hiss = vec![5e-5f32; 4_800];
        let heard = measure(&hiss, &hiss, RATE, 1e-4, None);
        assert!(!heard.sounded && heard.pitch.is_none() && heard.onset_seconds.is_none());

        // A note that is damped when its key comes up has the pitch it had
        // while it was held: a low string for a tenth of a second, then a
        // thud an octave and a half up as it is stopped.
        let key_up = (0.13 * RATE) as usize;
        let string: Vec<f32> = (0..(0.4 * RATE) as usize)
            .map(|frame| {
                let seconds = frame as f64 / RATE;
                if frame < key_up {
                    (0.5 * (TAU * 41.2 * seconds).sin()) as f32
                } else {
                    let after = (frame - key_up) as f64 / RATE;
                    (0.5 * 0.5f64.powf(after / 0.02) * (TAU * 110.0 * seconds).sin()) as f32
                }
            })
            .collect();
        let heard = measure(&string, &string, RATE, 1e-4, Some(key_up));
        assert!((heard.pitch.unwrap() - 28.0).abs() < 0.2, "E1 while it was held: {:?}", heard.pitch);
    }
}
