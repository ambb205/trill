//! Time-stretched audio, made off the audio thread and kept for reuse.
//!
//! Stretching costs 5 to 25 ms per second of audio, too long to do on the
//! thread that made an edit once files are minutes long. So a [`Stretcher`]
//! has a worker thread that takes requests, and a cache of what it has
//! made, keyed by everything the result depends on ([`StretchKey`]).
//!
//! Two kinds of caller use it:
//!
//! * The live graph asks without waiting ([`Stretcher::lookup`], then
//!   [`Stretcher::request`]) and plays a stand-in until the audio is ready.
//!   After every compile it says which keys it still cares about
//!   ([`Stretcher::set_wanted`]): queued work for anything else is dropped,
//!   which is what keeps a tempo dragged through fifty values from
//!   stretching fifty versions of every loop.
//! * An offline render never plays a stand-in: it waits for the worker or
//!   stretches on its own thread ([`Stretcher::prepare_now`]).
//!
//! The cache is bounded in bytes. Audio the live graph wants is never let
//! go; the rest (the tempo before the last change, say, kept so that undo
//! is instant) goes least recently used first.

use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;
use trill_core::audio::AudioBuffer;
use trill_dsp::stretch::{StretchMode, StretchOptions, stretch};
use trill_media::FileStamp;

/// Everything a piece of stretched audio depends on: which version of which
/// file, converted to which sample rate, stretched how.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StretchKey {
    path: PathBuf,
    stamp: Option<FileStamp>,
    sample_rate: u32,
    mode: StretchMode,
    /// The bits of the ratio and the pitch shift, which are what make two
    /// requests the same request.
    time_ratio: u64,
    pitch_semitones: u64,
}

impl StretchKey {
    pub fn new(path: &Path, stamp: Option<FileStamp>, sample_rate: f64, options: &StretchOptions) -> Self {
        Self {
            path: path.to_path_buf(),
            stamp,
            sample_rate: sample_rate.round() as u32,
            mode: options.mode,
            time_ratio: options.time_ratio.to_bits(),
            pitch_semitones: options.pitch_semitones.to_bits(),
        }
    }

    fn options(&self) -> StretchOptions {
        StretchOptions {
            time_ratio: f64::from_bits(self.time_ratio),
            pitch_semitones: f64::from_bits(self.pitch_semitones),
            mode: self.mode,
        }
    }
}

/// What the stretcher has for a key.
pub(crate) enum Lookup {
    Ready(Arc<AudioBuffer>),
    /// Queued or being stretched.
    InProgress,
    /// The stretch did not survive this audio. It is not tried again.
    Failed,
    Absent,
}

enum Slot {
    /// Waiting for the worker, with the audio to stretch.
    Queued(Arc<AudioBuffer>),
    /// Being stretched, by the worker or by a render.
    Running,
    Ready {
        audio: Arc<AudioBuffer>,
        used: u64,
    },
    Failed,
}

struct State {
    slots: HashMap<StretchKey, Slot>,
    /// Keys of the queued slots, oldest first.
    queue: VecDeque<StretchKey>,
    /// What the live graph plays or is waiting for.
    wanted: HashSet<StretchKey>,
    budget: usize,
    /// Bytes of ready audio the cache holds.
    bytes: usize,
    /// Advances on every use; the ready slot with the lowest stamp is the
    /// least recently used.
    clock: u64,
    /// Counts stretches that have ended, so a waiter can tell that there is
    /// something new without knowing what.
    finished: u64,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the worker: work was queued, or it is time to quit.
    work: Condvar,
    /// Notified whenever a stretch ends.
    done: Condvar,
}

/// Stretches audio on a worker thread and remembers the results.
pub(crate) struct Stretcher {
    shared: Arc<Shared>,
    /// Started with the first request, so that a stretcher that only ever
    /// serves renders costs no thread.
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Stretcher {
    /// Half a gibibyte: three quarters of an hour of stretched stereo audio
    /// at 48 kHz beyond what the live graph is using.
    pub const DEFAULT_BUDGET: usize = 512 << 20;

    pub fn new(budget_bytes: usize) -> Self {
        let state = State {
            slots: HashMap::new(),
            queue: VecDeque::new(),
            wanted: HashSet::new(),
            budget: budget_bytes,
            bytes: 0,
            clock: 0,
            finished: 0,
            quit: false,
        };
        let shared = Shared { state: Mutex::new(state), work: Condvar::new(), done: Condvar::new() };
        Self { shared: Arc::new(shared), worker: Mutex::new(None) }
    }

    /// What there is for `key` right now. Never waits.
    pub fn lookup(&self, key: &StretchKey) -> Lookup {
        let mut state = self.shared.state.lock();
        state.clock += 1;
        let clock = state.clock;
        match state.slots.get_mut(key) {
            Some(Slot::Ready { audio, used }) => {
                *used = clock;
                Lookup::Ready(audio.clone())
            }
            Some(Slot::Queued(_) | Slot::Running) => Lookup::InProgress,
            Some(Slot::Failed) => Lookup::Failed,
            None => Lookup::Absent,
        }
    }

    /// Asks for `source` to be stretched in the background. Does nothing if
    /// the key is already known.
    pub fn request(&self, key: StretchKey, source: Arc<AudioBuffer>) {
        {
            let mut state = self.shared.state.lock();
            if state.slots.contains_key(&key) {
                return;
            }
            state.slots.insert(key.clone(), Slot::Queued(source));
            state.queue.push_back(key);
        }
        self.start_worker();
        self.shared.work.notify_one();
    }

    /// The stretched audio for `key`, however long it takes: taken from the
    /// cache, waited for if someone else is making it, or made here.
    /// `None` only if stretching this audio fails.
    pub fn prepare_now(&self, key: &StretchKey, source: &Arc<AudioBuffer>) -> Option<Arc<AudioBuffer>> {
        {
            let mut state = self.shared.state.lock();
            loop {
                state.clock += 1;
                let clock = state.clock;
                match state.slots.get_mut(key) {
                    Some(Slot::Ready { audio, used }) => {
                        *used = clock;
                        return Some(audio.clone());
                    }
                    Some(Slot::Failed) => return None,
                    Some(Slot::Running) => self.shared.done.wait(&mut state),
                    // Not started yet: do it here rather than wait in line.
                    Some(Slot::Queued(_)) => {
                        state.queue.retain(|queued| queued != key);
                        break;
                    }
                    None => break,
                }
            }
            state.slots.insert(key.clone(), Slot::Running);
        }
        self.shared.run(key, source)
    }

    /// Says which keys the live graph uses or waits for. Work queued for
    /// any other key is dropped, and ready audio for other keys becomes
    /// eligible for eviction.
    pub fn set_wanted(&self, wanted: HashSet<StretchKey>) {
        let mut state = self.shared.state.lock();
        let State { slots, queue, .. } = &mut *state;
        queue.retain(|key| {
            let keep = wanted.contains(key);
            if !keep {
                slots.remove(key);
            }
            keep
        });
        state.wanted = wanted;
        state.trim();
    }

    /// How many stretches have ended since this stretcher was made.
    pub fn finished(&self) -> u64 {
        self.shared.state.lock().finished
    }

    /// Waits until a stretch has ended that was not among the first `seen`,
    /// and returns `true`; or returns `false` because none is in progress
    /// to wait for, or `deadline` has passed.
    pub fn wait_for_progress(&self, seen: u64, deadline: Instant) -> bool {
        let mut state = self.shared.state.lock();
        while state.finished == seen && state.in_progress() {
            if self.shared.done.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        state.finished != seen
    }

    fn start_worker(&self) {
        let mut worker = self.worker.lock();
        if worker.is_some() {
            return;
        }
        let shared = self.shared.clone();
        match std::thread::Builder::new().name("stretch".into()).spawn(move || shared.work()) {
            Ok(handle) => *worker = Some(handle),
            // Without a worker nothing queued is ever made; clips keep
            // their stand-ins, and renders still stretch for themselves.
            Err(error) => log::error!("could not start the time-stretch thread: {error}"),
        }
    }
}

impl Drop for Stretcher {
    /// Tells the worker to stop. It is not waited for: a stretch already
    /// under way cannot be interrupted, may take seconds, and nobody is
    /// left to want its result.
    fn drop(&mut self) {
        self.shared.state.lock().quit = true;
        self.shared.work.notify_all();
    }
}

impl Shared {
    /// The body of the worker thread.
    fn work(&self) {
        loop {
            let (key, source) = {
                let mut state = self.state.lock();
                loop {
                    if state.quit {
                        return;
                    }
                    if let Some(key) = state.queue.pop_front() {
                        match state.slots.insert(key.clone(), Slot::Running) {
                            Some(Slot::Queued(source)) => break (key, source),
                            // The queue and the slots are changed together,
                            // so this cannot happen; if it does, the slot
                            // must not stay `Running` with nobody running it.
                            _ => {
                                state.slots.remove(&key);
                            }
                        }
                    } else {
                        self.work.wait(&mut state);
                    }
                }
            };
            self.run(&key, &source);
        }
    }

    /// Stretches `source` as `key` says and records the outcome. The slot
    /// must already be marked `Running`.
    fn run(&self, key: &StretchKey, source: &Arc<AudioBuffer>) -> Option<Arc<AudioBuffer>> {
        let options = key.options();
        // A panic here is a bug in the stretcher, but one that must cost a
        // clip its sound quality, not the session its worker or a render
        // its result.
        let stretched = catch_unwind(AssertUnwindSafe(|| stretch(source, &options))).ok().map(Arc::new);
        if stretched.is_none() {
            log::error!("time-stretching '{}' failed; the clip plays repitched instead", key.path.display());
        }

        let mut state = self.state.lock();
        state.clock += 1;
        let slot = match &stretched {
            Some(audio) => {
                state.bytes += bytes_of(audio);
                Slot::Ready { audio: audio.clone(), used: state.clock }
            }
            None => Slot::Failed,
        };
        state.slots.insert(key.clone(), slot);
        state.finished += 1;
        state.trim();
        drop(state);
        self.done.notify_all();
        stretched
    }
}

impl State {
    fn in_progress(&self) -> bool {
        !self.queue.is_empty() || self.slots.values().any(|slot| matches!(slot, Slot::Running))
    }

    /// Lets go of the least recently used audio nobody wants until the
    /// budget holds.
    fn trim(&mut self) {
        while self.bytes > self.budget {
            let oldest = self
                .slots
                .iter()
                .filter(|(key, _)| !self.wanted.contains(key))
                .filter_map(|(key, slot)| match slot {
                    Slot::Ready { used, .. } => Some((*used, key)),
                    _ => None,
                })
                .min_by_key(|(used, _)| *used)
                .map(|(_, key)| key.clone());
            let Some(key) = oldest else { break };
            if let Some(Slot::Ready { audio, .. }) = self.slots.remove(&key) {
                self.bytes -= bytes_of(&audio);
            }
        }
    }
}

fn bytes_of(audio: &AudioBuffer) -> usize {
    (audio.left.len() + audio.right.len()) * size_of::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tone(frames: usize) -> Arc<AudioBuffer> {
        let channel: Vec<f32> = (0..frames).map(|n| (n as f32 * 0.05).sin() * 0.5).collect();
        Arc::new(AudioBuffer { sample_rate: 48_000.0, left: channel.clone(), right: channel })
    }

    fn key(name: &str, time_ratio: f64) -> StretchKey {
        let options = StretchOptions { time_ratio, pitch_semitones: 0.0, mode: StretchMode::Tonal };
        StretchKey::new(Path::new(name), None, 48_000.0, &options)
    }

    fn wait(stretcher: &Stretcher) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let seen = stretcher.finished();
            if !stretcher.shared.state.lock().in_progress() {
                return;
            }
            assert!(Instant::now() < deadline, "the worker never finished");
            let _ = stretcher.wait_for_progress(seen, deadline);
        }
    }

    #[test]
    fn requests_are_stretched_in_the_background_once() {
        let stretcher = Stretcher::new(Stretcher::DEFAULT_BUDGET);
        let source = tone(48_000);
        let half = key("a.wav", 0.5);
        assert!(matches!(stretcher.lookup(&half), Lookup::Absent));

        stretcher.request(half.clone(), source.clone());
        stretcher.request(half.clone(), source.clone());
        wait(&stretcher);
        let Lookup::Ready(audio) = stretcher.lookup(&half) else { panic!("not ready") };
        assert_eq!(audio.frames(), 24_000);
        assert_eq!(stretcher.finished(), 1, "the second request joined the first");

        // The same key again, from a render: no second stretch.
        let again = stretcher.prepare_now(&half, &source).unwrap();
        assert!(Arc::ptr_eq(&audio, &again));
        assert_eq!(stretcher.finished(), 1);
        // A different ratio is different audio.
        assert_eq!(stretcher.prepare_now(&key("a.wav", 2.0), &source).unwrap().frames(), 96_000);
        assert_eq!(stretcher.finished(), 2);
    }

    #[test]
    fn work_nobody_wants_any_more_is_dropped() {
        let stretcher = Stretcher::new(Stretcher::DEFAULT_BUDGET);
        // Long enough that the queue is still full when it is pruned.
        let source = tone(48_000 * 20);
        let keys: Vec<StretchKey> = (0..6).map(|n| key("long.wav", 0.5 + n as f64 * 0.01)).collect();
        for key in &keys {
            stretcher.request(key.clone(), source.clone());
        }
        let last = keys[5].clone();
        stretcher.set_wanted(HashSet::from([last.clone()]));
        wait(&stretcher);
        assert!(matches!(stretcher.lookup(&last), Lookup::Ready(_)));
        // The one the worker had already started may have finished; the
        // ones still waiting were never made.
        assert!(stretcher.finished() <= 2, "{} stretches were made", stretcher.finished());
        assert!(matches!(stretcher.lookup(&keys[3]), Lookup::Absent));
    }

    #[test]
    fn the_budget_evicts_what_is_not_wanted_least_recently_used_first() {
        let source = tone(10_000);
        let one = 10_000 * 2 * 8;
        // Room for two stretched copies (each twice the length of the source).
        let stretcher = Stretcher::new(one * 2);
        let (a, b, c) = (key("a.wav", 2.0), key("b.wav", 2.0), key("c.wav", 2.0));
        stretcher.prepare_now(&a, &source).unwrap();
        stretcher.prepare_now(&b, &source).unwrap();
        // Use `a` again, so `b` is the older of the two.
        assert!(matches!(stretcher.lookup(&a), Lookup::Ready(_)));
        stretcher.prepare_now(&c, &source).unwrap();
        assert!(matches!(stretcher.lookup(&b), Lookup::Absent), "the least recently used was let go");
        assert!(matches!(stretcher.lookup(&a), Lookup::Ready(_)) && matches!(stretcher.lookup(&c), Lookup::Ready(_)));

        // What the live graph wants stays, even over budget.
        stretcher.set_wanted(HashSet::from([a.clone(), c.clone()]));
        stretcher.prepare_now(&b, &source).unwrap();
        assert!(matches!(stretcher.lookup(&a), Lookup::Ready(_)) && matches!(stretcher.lookup(&c), Lookup::Ready(_)));
        assert!(matches!(stretcher.lookup(&b), Lookup::Absent), "the unwanted newcomer is what goes");
        assert_eq!(stretcher.shared.state.lock().bytes, one * 2);
    }

    #[test]
    fn a_render_takes_queued_work_instead_of_waiting_in_line() {
        let stretcher = Stretcher::new(Stretcher::DEFAULT_BUDGET);
        let long = tone(48_000 * 20);
        let short = tone(4_800);
        stretcher.request(key("long.wav", 0.5), long);
        stretcher.request(key("short.wav", 0.5), short.clone());
        let started = Instant::now();
        let audio = stretcher.prepare_now(&key("short.wav", 0.5), &short).unwrap();
        assert_eq!(audio.frames(), 2_400);
        // Not a timing assertion on the stretch itself: only that the short
        // one did not wait for twenty seconds of audio ahead of it.
        assert!(
            started.elapsed() < Duration::from_secs(5)
                || matches!(stretcher.lookup(&key("long.wav", 0.5)), Lookup::Ready(_))
        );
        wait(&stretcher);
        assert_eq!(stretcher.finished(), 2, "and the worker did not stretch it a second time");
    }
}
