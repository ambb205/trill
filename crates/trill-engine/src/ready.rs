//! Waiting for plugins to be ready before audio is made with them.
//!
//! A render plays freshly created devices, and a plugin that has only just
//! been created may not be ready to play: a sampler goes on loading its
//! samples on threads of its own, a modelled instrument goes on waking up,
//! and meanwhile either is silent, or plays the part of its sound it has
//! got so far. A render that starts at once then loses the beginning of
//! the song: a drum hit on bar one that never sounds, a first note without
//! its attack. Neither plugin format has a way of asking a plugin whether
//! it is ready.
//!
//! So readiness is measured by the one thing that shows it: **a plugin is
//! ready when it answers the notes it is about to be given, and has gone
//! on answering them the same way for a quarter of a second.** Before a
//! render begins, each track whose instrument is a
//! plugin is rehearsed on its own: the first different notes it will play,
//! one after the other in their order (a keyswitch stays ahead of the
//! notes it is for), each with the controllers the song has for it. How
//! much comes out while each note is down is written down. When a
//! rehearsal is answered, and answered again note for note as the ones
//! before it were for a quarter of a second (a sampler that loads its
//! kit piece by piece answers a little more each time until it is done),
//! the plugin has stopped changing and is taken to be ready; until then
//! it is rehearsed again some ten times a second, for as long as the
//! caller has patience. Then the notes are let go of, what rings is left to die
//! away, the plugins are reset, and the render starts from the top.
//!
//! Only plugins are rehearsed, and only they are run: the built-in devices
//! are ready the moment they exist and are not touched, so a song of
//! built-in devices renders to the same samples it always did.
//!
//! A track that never answers (its notes lie outside the instrument's
//! range, its clips hold only keyswitches, the instrument has no sound
//! loaded) costs the caller's whole patience once. It is remembered by
//! what it was (the plugins, their state, the notes), and the next render
//! of the same thing waits only a moment for it.
//!
//! What this cannot see: a plugin that stops changing for longer than a
//! quarter of a second and is still not done (a library that loads for
//! many seconds with pauses in it, or one whose later notes are not among
//! the dozen that are rehearsed), and an effect plugin behind a built-in
//! instrument, which is given nothing to answer. Neither is worse off
//! than before there was any waiting.

use crate::graph::Graph;
use parking_lot::Mutex;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use trill_core::device::{DeviceCategory, DeviceFactory, DeviceProcessor, Event, EventKind, ProcessInfo};
use trill_core::{ControllerKind, Device, DeviceId, Project, TimeSignature, TrackId};

/// The most notes of a track's opening that are rehearsed.
const OPENING_NOTES: usize = 12;
/// Frames in each block a rehearsal processes.
const BLOCK: usize = 256;
/// How long each rehearsed note is held, and how long the plugin is
/// listened to after the last.
const NOTE_SECONDS: f64 = 0.05;
const AFTER_SECONDS: f64 = 0.1;
/// Output above this (-80 dB) is an answer, unless the plugin makes more
/// noise than that when it is given nothing.
const ANSWER: f32 = 1e-4;
/// How far above its own noise an answer must be.
const ABOVE_NOISE: f32 = 4.0;
/// Two answers this close in level, in dB, are the same answer.
const SAME_ANSWER_DB: f32 = 3.0;
/// How long the answers must have stayed the same for the plugin to be
/// taken as ready.
const SETTLED_FOR: Duration = Duration::from_millis(250);
/// How many more times an answer that keeps changing is asked for before
/// it is taken as it is: an instrument that never plays a note the same
/// way twice is ready all the same.
const CHANGING_ANSWERS: u32 = 10;
/// How long a rehearsal waits between tries.
const BETWEEN_TRIES: Duration = Duration::from_millis(100);
/// The longest a rehearsed plugin is left to ring out before it is reset.
const RING_OUT_SECONDS: f64 = 3.0;
/// How long output must stay down to count as rung out.
const QUIET_SECONDS: f64 = 0.1;
/// How long a track that is known not to answer is waited for.
const KNOWN_SILENT: Duration = Duration::from_secs(1);
/// How many things that did not answer are remembered.
const REMEMBERED: usize = 256;

/// What did not answer the last time it was asked, by [`signature`].
static KNOWN_NOT_TO_ANSWER: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// How waiting for one track's plugin went.
#[derive(Clone, Debug, PartialEq)]
pub struct Waited {
    pub track: TrackId,
    /// The plugin that was waited for: the track's instrument.
    pub device: DeviceId,
    /// How long it took to answer, or how long it was waited for in vain.
    pub seconds: f64,
    /// Whether it answered. If not, the render went ahead without it.
    pub answered: bool,
}

/// One note of a rehearsal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpeningNote {
    pub pitch: u8,
    /// 0..=1.
    pub velocity: f32,
    /// Where the controllers stand for it, sent just ahead of it.
    pub controllers: Vec<EventKind>,
}

/// What a plugin is given to see whether it answers.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Opening {
    pub notes: Vec<OpeningNote>,
}

/// Devices in a row, driven by hand outside any graph: events in at the
/// front, and what comes out of the last one listened to.
pub(crate) struct Stand<'a> {
    devices: Vec<&'a mut Box<dyn DeviceProcessor>>,
    info: ProcessInfo,
    left: Vec<f32>,
    right: Vec<f32>,
    /// The most the devices put out while they were given nothing.
    noise: f32,
}

impl<'a> Stand<'a> {
    pub fn new(devices: Vec<&'a mut Box<dyn DeviceProcessor>>, sample_rate: f64, tempo: f64) -> Self {
        let info = ProcessInfo {
            sample_rate,
            tempo,
            time_signature: TimeSignature::default(),
            playing: false,
            position_beats: 0.0,
            loop_range: None,
        };
        Self { devices, info, left: vec![0.0; BLOCK], right: vec![0.0; BLOCK], noise: 0.0 }
    }

    pub fn sample_rate(&self) -> f64 {
        self.info.sample_rate
    }

    pub fn frames(&self, seconds: f64) -> usize {
        (seconds * self.info.sample_rate).ceil() as usize
    }

    /// Runs the devices for `frames` frames, with `events` in the first
    /// block, and hands every block of what comes out to `hear`. Stops
    /// early, at the end of a block, once `hear` returns false.
    pub fn run_into(&mut self, events: &[Event], frames: usize, mut hear: impl FnMut(&[f32], &[f32]) -> bool) {
        let mut done = 0;
        while done < frames {
            let count = (frames - done).min(BLOCK);
            let (left, right) = (&mut self.left[..count], &mut self.right[..count]);
            left.fill(0.0);
            right.fill(0.0);
            let events = if done == 0 { events } else { &[] };
            for device in &mut self.devices {
                device.process(&self.info, events, left, right);
            }
            done += count;
            if !hear(left, right) {
                break;
            }
        }
    }

    /// Runs the devices for `seconds`, with `events` in the first block,
    /// and returns the highest sample that came out.
    pub fn run(&mut self, events: &[Event], seconds: f64) -> f32 {
        let frames = self.frames(seconds);
        let mut peak = 0.0f32;
        self.run_into(events, frames, |left, right| {
            peak = left.iter().chain(right).fold(peak, |peak, sample| peak.max(sample.abs()));
            true
        });
        peak
    }

    /// Listens to the devices with nothing going in, to know their noise.
    pub fn hear_noise(&mut self) {
        self.noise = self.run(&[], 0.05);
    }

    /// The noise that was heard, for a stand of the same devices to be
    /// told later.
    pub fn noise(&self) -> f32 {
        self.noise
    }

    pub fn set_noise(&mut self, noise: f32) {
        self.noise = noise;
    }

    /// The level above which output is sound and not the devices' own
    /// noise.
    pub fn heard_above(&self) -> f32 {
        ANSWER.max(self.noise * ABOVE_NOISE)
    }

    /// Runs the devices for `seconds`, with `events` in the first block,
    /// and returns what came out as an [`Answer`].
    fn hear(&mut self, events: &[Event], seconds: f64) -> Answer {
        let frames = self.frames(seconds);
        let (mut peak, mut energy) = (0.0f32, 0.0f64);
        self.run_into(events, frames, |left, right| {
            for sample in left.iter().chain(right) {
                peak = peak.max(sample.abs());
                energy += f64::from(*sample) * f64::from(*sample);
            }
            true
        });
        Answer { peak, energy }
    }

    /// Plays `opening` a note at a time and returns what came out while
    /// each was down, and after the last.
    fn rehearse(&mut self, opening: &Opening) -> Vec<Answer> {
        let mut answers = Vec::with_capacity(opening.notes.len() + 1);
        let mut events: Vec<Event> = Vec::new();
        for note in &opening.notes {
            events.extend(note.controllers.iter().map(|kind| Event { offset: 0, kind: *kind }));
            events.push(Event { offset: 0, kind: EventKind::NoteOn { pitch: note.pitch, velocity: note.velocity } });
            answers.push(self.hear(&events, NOTE_SECONDS));
            events.clear();
            events.push(Event { offset: 0, kind: EventKind::NoteOff { pitch: note.pitch } });
        }
        answers.push(self.hear(&events, AFTER_SECONDS));
        answers
    }

    /// Lets go of every note, leaves what rings to die away (not for
    /// ever), and resets the devices: they are as they were made, but
    /// awake.
    pub fn settle(&mut self) {
        let quiet = self.heard_above() / 2.0;
        let needed = self.frames(QUIET_SECONDS);
        let limit = self.frames(RING_OUT_SECONDS);
        let all_off = [Event { offset: 0, kind: EventKind::AllNotesOff }];
        let (mut done, mut silent) = (0, 0);
        while done < limit && silent < needed {
            let events: &[Event] = if done == 0 { &all_off } else { &[] };
            let peak = self.run(events, BLOCK as f64 / self.info.sample_rate);
            silent = if peak < quiet { silent + BLOCK } else { 0 };
            done += BLOCK;
        }
        self.reset();
        // Some devices act on a reset with their next block (a VST3 plugin
        // is told to be silent then): that block is this one, not the
        // first of what comes after.
        self.run(&[], BLOCK as f64 / self.info.sample_rate);
    }

    /// Resets every device: voices silenced, delay lines cleared.
    pub fn reset(&mut self) {
        for device in &mut self.devices {
            device.reset();
        }
    }
}

/// What came out of a plugin over a short stretch: its highest sample, by
/// which it is known to have answered at all, and its energy, by which
/// one answer is compared with the next. A note that has only now begun
/// to sound adds energy even under the ringing of the note before it.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Answer {
    peak: f32,
    energy: f64,
}

/// One plugin's wait: what it is asked, and how it has answered so far.
pub(crate) struct Wait<'a> {
    pub stand: Stand<'a>,
    pub opening: Opening,
    /// What is waited for, for remembering it if it never answers.
    pub signature: u64,
    /// The answers of the last try and when it was made, if it was
    /// answered at all.
    last: Option<(Vec<Answer>, Instant)>,
    /// Since when the answers have been the same.
    same_since: Option<Instant>,
    /// How many answers so far were not like the one before.
    changing: u32,
    /// When it was found ready, or given up on.
    outcome: Option<(Duration, bool)>,
}

impl<'a> Wait<'a> {
    pub fn new(stand: Stand<'a>, opening: Opening, signature: u64) -> Self {
        Self { stand, opening, signature, last: None, same_since: None, changing: 0, outcome: None }
    }

    /// One try. Returns whether the wait is over.
    fn attempt(&mut self, begun: Instant) -> bool {
        let floor = self.stand.heard_above();
        let now = Instant::now();
        let answers = self.stand.rehearse(&self.opening);
        if answers.iter().all(|answer| answer.peak <= floor) {
            (self.last, self.same_since) = (None, None);
            return false;
        }
        // Note for note: both silent, or both sounding with about the same
        // energy.
        let alike = |now: Answer, before: Answer| match (now.peak > floor, before.peak > floor) {
            (false, false) => true,
            (true, true) => (10.0 * (now.energy / before.energy).log10()).abs() <= f64::from(SAME_ANSWER_DB),
            _ => false,
        };
        let before = self.last.take();
        let same =
            before.as_ref().is_some_and(|(last, _)| answers.iter().zip(last).all(|(now, before)| alike(*now, *before)));
        self.changing += u32::from(before.is_some() && !same);
        self.same_since = match (same, self.same_since, before) {
            (true, Some(since), _) => Some(since),
            (true, None, Some((_, at))) => Some(at),
            _ => None,
        };
        self.last = Some((answers, now));
        let settled = self.same_since.is_some_and(|since| now.duration_since(since) >= SETTLED_FOR);
        if settled || self.changing >= CHANGING_ANSWERS {
            self.outcome = Some((begun.elapsed(), true));
        }
        self.outcome.is_some()
    }

    /// How the wait went: how long it took, and whether it was answered.
    pub fn outcome(&self) -> (Duration, bool) {
        self.outcome.unwrap_or_default()
    }
}

/// Tries every wait in turn until each is over or `patience` has run out,
/// then settles them all. Waits that are known not to be answered get
/// [`KNOWN_SILENT`] at most; what was not answered this time is remembered.
pub(crate) fn wait_for(waits: &mut [Wait], patience: Duration) {
    let begun = Instant::now();
    let known: Vec<u64> = KNOWN_NOT_TO_ANSWER.lock().clone();
    for wait in waits.iter_mut() {
        wait.stand.hear_noise();
    }
    loop {
        let waited = begun.elapsed();
        let mut pending = false;
        for wait in waits.iter_mut().filter(|wait| wait.outcome.is_none()) {
            let limit = if known.contains(&wait.signature) { patience.min(KNOWN_SILENT) } else { patience };
            if wait.attempt(begun) {
                continue;
            }
            if waited >= limit {
                wait.outcome = Some((waited, false));
            } else {
                pending = true;
            }
        }
        if !pending {
            break;
        }
        std::thread::sleep(BETWEEN_TRIES);
    }
    let mut known = KNOWN_NOT_TO_ANSWER.lock();
    for wait in waits.iter_mut() {
        wait.stand.settle();
        let answered = wait.outcome().1;
        known.retain(|signature| *signature != wait.signature);
        if !answered {
            known.push(wait.signature);
        }
    }
    let surplus = known.len().saturating_sub(REMEMBERED);
    known.drain(..surplus);
}

/// What identifies a wait: the plugins, their saved states, and what they
/// are asked.
pub(crate) fn signature<'a>(devices: impl Iterator<Item = &'a Device>, opening: &Opening) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for device in devices {
        device.uri.hash(&mut hasher);
        device.state.hash(&mut hasher);
        device.preset.as_ref().map(|preset| &preset.id).hash(&mut hasher);
        for (key, value) in &device.params {
            key.hash(&mut hasher);
            value.to_bits().hash(&mut hasher);
        }
    }
    for note in &opening.notes {
        (note.pitch, note.velocity.to_bits()).hash(&mut hasher);
        for kind in &note.controllers {
            if let Some((controller, value)) = ControllerKind::of_event(*kind) {
                (controller.index(), value.to_bits()).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Tells which devices may not be ready when they have just been made,
/// and which of them are instruments.
pub(crate) struct Plugins {
    /// The URIs of every instrument that is not built in.
    instruments: Vec<String>,
}

impl Plugins {
    pub fn of(factory: &dyn DeviceFactory) -> Self {
        let instruments = factory
            .descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.category == DeviceCategory::Instrument && Self::is_plugin(&descriptor.uri))
            .map(|descriptor| descriptor.uri)
            .collect();
        Self { instruments }
    }

    /// Whether a device with this URI is a plugin: something that may not
    /// be ready when it has just been created.
    pub fn is_plugin(uri: &str) -> bool {
        !uri.starts_with("builtin:")
    }

    fn is_instrument(&self, uri: &str) -> bool {
        self.instruments.iter().any(|instrument| instrument == uri)
    }
}

/// Sees to it that every plugin instrument of `graph` that has notes to
/// play between the beats `from` and `to` is ready to play them, waiting
/// up to `patience` for those that are not. See the module documentation.
pub(crate) fn wake(
    graph: &mut Graph,
    project: &Project,
    plugins: &Plugins,
    (from, to): (f64, f64),
    sample_rate: f64,
    patience: Duration,
) -> Vec<Waited> {
    let tempo = graph.tempo;
    let mut waits = Vec::new();
    let mut who = Vec::new();
    for node in &mut graph.tracks {
        let Ok(track) = project.track(node.id) else { continue };
        // What plays the track's notes: its first device that is not an
        // effect. Only if that is a plugin is there anything to wait for.
        let sounding = track.devices.iter().filter(|device| device.enabled);
        let Some(instrument) = sounding.clone().find(|device| trill_dsp::is_instrument(&device.uri) != Some(false))
        else {
            continue;
        };
        if !plugins.is_instrument(&instrument.uri) {
            continue;
        }
        // The instrument and the plugins after it: what a note goes
        // through before it is heard, the built-in devices left out.
        let from_instrument = sounding.skip_while(|device| device.id != instrument.id);
        let rehearsed: Vec<&Device> = from_instrument.filter(|device| Plugins::is_plugin(&device.uri)).collect();
        let mut notes: Vec<OpeningNote> = Vec::new();
        for note in node.notes.iter().filter(|note| note.start >= from - 1e-9 && note.start < to) {
            if notes.len() == OPENING_NOTES {
                break;
            }
            if notes.iter().any(|known| known.pitch == note.pitch) {
                continue;
            }
            let controllers = node
                .controllers
                .iter()
                .map(|lane| {
                    // A level is taken at its highest under the note: a
                    // line that swells from nothing is still a line.
                    let at = |beat: f64| lane.value_at(beat);
                    let value = match lane.kind.rest() {
                        Some(_) => at(note.start),
                        None => at(note.start).max(at((note.start + note.end) / 2.0)).max(at(note.end)),
                    };
                    lane.kind.event(value as f32)
                })
                .collect();
            let velocity = f32::from(note.velocity.clamp(1, 127)) / 127.0;
            notes.push(OpeningNote { pitch: note.pitch, velocity, controllers });
        }
        if notes.is_empty() {
            // Nothing to play, so nothing to be late with.
            continue;
        }
        let opening = Opening { notes };
        let signature = signature(rehearsed.iter().copied(), &opening);
        let ids: Vec<DeviceId> = rehearsed.iter().map(|device| device.id).collect();
        let slots = node.chain.devices.iter_mut().filter(|slot| ids.contains(&slot.id));
        let processors: Vec<_> = slots.filter_map(|slot| slot.processor.as_mut()).collect();
        if processors.len() != ids.len() {
            // One of them could not be created: a problem of its own.
            continue;
        }
        waits.push(Wait::new(Stand::new(processors, sample_rate, tempo), opening, signature));
        who.push((node.id, instrument.id));
    }
    if waits.is_empty() {
        return Vec::new();
    }
    wait_for(&mut waits, patience);
    waits
        .iter()
        .zip(who)
        .map(|(wait, (track, device))| {
            let (waited, answered) = wait.outcome();
            Waited { track, device, seconds: waited.as_secs_f64(), answered }
        })
        .collect()
}
