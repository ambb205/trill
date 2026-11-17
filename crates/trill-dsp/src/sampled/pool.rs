//! The recordings of sampled instruments, in memory: the start of each,
//! and the rest of the ones that are being played.
//!
//! A good piano is one to two gigabytes of recordings, of which a song
//! plays a part. So what is held of every recording is its **start**
//! ([`START_FRAMES`], a third of a second), read once when the instrument
//! is loaded; a recording shorter than that and a half is simply whole.
//! When a note sounds a longer one, its voice plays the start from memory
//! while a worker thread reads the **whole** recording, and goes on into
//! that when it is there. The whole recordings are kept, up to a budget
//! ([`DEFAULT_BUDGET`]); above it the ones longest unplayed that no voice
//! is in are let go of, and read again when next played.
//!
//! # What the audio thread does here
//!
//! It never reads a file, waits or frees anything. It asks for a start
//! ([`Recording::start`]: a load of a pointer), asks whether the whole is
//! there ([`Recording::whole_if_there`]: a lock that is tried and never
//! waited for, and one count going up) and says that it wants it
//! ([`Pool::want`]: two numbers). The workers look for what is wanted
//! every couple of milliseconds while anything plays.
//!
//! A voice keeps what it reads from alive by holding it. So that the last
//! holder is never the audio thread, a whole recording is only let go of
//! while the pool is its one holder, which it finds out with the lock held
//! that a voice would have to try: no voice has it then, and none can take
//! it.
//!
//! # What a render does
//!
//! A render may wait, so it does: [`Pool::start_now`] and
//! [`Pool::whole_now`] read on the thread that asks. A render therefore
//! never plays a note short, and reads only the recordings its song plays.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;
use trill_core::audio::{AudioLoader, FileLoop};

/// How many frames of every recording are in memory from the moment its
/// instrument is loaded: a third of a second at 48 kHz, which is how long
/// a worker has to read the rest behind a note that has begun (half that
/// for a note played an octave above its recording).
pub const START_FRAMES: usize = 16_384;
/// A recording up to this long is kept whole in place of its start.
const WHOLE_UP_TO: usize = START_FRAMES + START_FRAMES / 2;
/// How many bytes of whole recordings are kept by default: two gibibytes.
pub const DEFAULT_BUDGET: usize = 2 << 30;
/// Zero frames before the first frame of what is held, and after the last
/// of a recording that is whole, so that a read near either end need not
/// look where it is. As far as the widest interpolation reaches, and some.
pub const GUARD: usize = 80;
/// Frames read from a file at a time.
const PIECE: usize = 1 << 16;
/// How long a worker with nothing to do sleeps before it looks again for
/// what the audio thread wants, while something plays and while nothing
/// does.
const LOOK_EVERY: Duration = Duration::from_millis(2);
const LOOK_IDLE: Duration = Duration::from_millis(100);
/// The most worker threads.
const MAX_WORKERS: usize = 4;

/// A stretch of a recording in memory, from its first frame.
pub struct Audio {
    left: Box<[f32]>,
    /// `None` for a recording of one channel.
    right: Option<Box<[f32]>>,
    frames: usize,
    whole: bool,
}

impl Audio {
    /// `left` and `right` are the frames from the file's first; `whole`
    /// says that they reach its end.
    fn new(left: &[f32], right: Option<&[f32]>, whole: bool) -> Self {
        let pad = |channel: &[f32]| {
            let after = if whole { GUARD } else { 0 };
            let mut padded = vec![0.0f32; GUARD + channel.len() + after];
            padded[GUARD..GUARD + channel.len()].copy_from_slice(channel);
            padded.into_boxed_slice()
        };
        let frames = right.map_or(left.len(), |right| left.len().min(right.len()));
        Self { left: pad(&left[..frames]), right: right.map(|right| pad(&right[..frames])), frames, whole }
    }

    /// How many frames of the recording this holds.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Whether it holds the recording to its end.
    pub fn is_whole(&self) -> bool {
        self.whole
    }

    pub fn is_mono(&self) -> bool {
        self.right.is_none()
    }

    /// How far a read may reach: one past the last frame that can be
    /// asked for. Past the end of a whole recording there is silence;
    /// past the end of a start there is nothing.
    pub fn reach(&self) -> usize {
        if self.whole { self.frames + GUARD } else { self.frames }
    }

    /// The left channel with [`GUARD`] frames before frame 0, and the
    /// right (the left again for a recording of one channel).
    #[inline]
    pub fn channels(&self) -> (&[f32], &[f32]) {
        (&self.left, self.right.as_deref().unwrap_or(&self.left))
    }

    fn bytes(&self) -> usize {
        (self.left.len() + self.right.as_ref().map_or(0, |right| right.len())) * 4
    }
}

/// What a recording's file says of it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Facts {
    /// Its whole length in frames.
    pub frames: usize,
    pub sample_rate: f64,
    pub mono: bool,
    /// The loop the file carries, if it has one.
    pub file_loop: Option<FileLoop>,
}

/// One recording of an instrument, as much of it as is in memory.
pub struct Recording {
    path: String,
    /// The start, once it has been read, or why it cannot be.
    start: OnceLock<Result<(Facts, Arc<Audio>), String>>,
    /// The whole of a recording that is longer than its start, while that
    /// is in memory.
    whole: Mutex<Option<Arc<Audio>>>,
    /// A note is sounding it and waits for the whole.
    wanted: AtomicBool,
    /// When it was last played, by the pool's count of notes.
    used: AtomicU64,
    /// Held by whoever is reading the whole of it.
    reading: Mutex<()>,
}

impl Recording {
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The start of the recording and what its file says, if it has been
    /// read. `None` while it has not, and for a file that cannot be read.
    #[inline]
    pub fn start(&self) -> Option<(&Facts, &Arc<Audio>)> {
        match self.start.get() {
            Some(Ok((facts, audio))) => Some((facts, audio)),
            _ => None,
        }
    }

    /// Whether the start has been read, or found unreadable.
    pub fn is_settled(&self) -> bool {
        self.start.get().is_some()
    }

    /// Why the recording cannot be read, if it was tried and cannot.
    pub fn problem(&self) -> Option<&str> {
        match self.start.get() {
            Some(Err(reason)) => Some(reason),
            _ => None,
        }
    }

    /// The whole recording, if it is in memory and nobody is putting it
    /// there or taking it away this instant. Never waits.
    #[inline]
    pub fn whole_if_there(&self) -> Option<Arc<Audio>> {
        self.whole.try_lock().ok().and_then(|whole| whole.clone())
    }
}

/// Reads the start of the recording at `path`.
fn read_start(loader: &dyn AudioLoader, path: &str) -> Result<(Facts, Arc<Audio>), String> {
    let mut stream = loader.open(path)?;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mono = stream.mono();
    let mut read = 0;
    // One frame more than is kept, to see whether the file ends there.
    while read <= WHOLE_UP_TO {
        match stream.read(&mut left, &mut right, WHOLE_UP_TO + 1 - read)? {
            0 => break,
            more => read += more,
        }
    }
    let known = stream.frames().and_then(|frames| usize::try_from(frames).ok());
    let mut whole = read <= WHOLE_UP_TO;
    if !whole && known.is_none() {
        // A file that does not say how long it is has to be read to find
        // out, and is then kept as it is.
        while stream.read(&mut left, &mut right, PIECE)? > 0 {}
        read = left.len();
        whole = true;
    }
    if read == 0 {
        return Err(format!("'{path}' holds no audio"));
    }
    let frames = if whole { read } else { known.unwrap_or(read).max(read) };
    let kept = if whole { read } else { START_FRAMES };
    let facts = Facts { frames, sample_rate: stream.sample_rate(), mono, file_loop: stream.file_loop() };
    let audio = Audio::new(&left[..kept], (!mono).then(|| &right[..kept.min(right.len())]), whole);
    Ok((facts, Arc::new(audio)))
}

/// Reads the whole of the recording at `path`.
fn read_whole(loader: &dyn AudioLoader, path: &str, expected: usize) -> Result<Audio, String> {
    let mut stream = loader.open(path)?;
    let mono = stream.mono();
    let mut left = Vec::with_capacity(expected);
    let mut right = Vec::with_capacity(if mono { 0 } else { expected });
    while stream.read(&mut left, &mut right, PIECE)? > 0 {}
    Ok(Audio::new(&left, (!mono).then_some(&right[..]), true))
}

struct Shared {
    loader: Arc<dyn AudioLoader>,
    /// Every recording anything plays, by its path.
    recordings: Mutex<HashMap<String, Weak<Recording>>>,
    /// Starts that are to be read, in the order they were asked for.
    starts: Mutex<VecDeque<Arc<Recording>>>,
    signal: Condvar,
    /// How many recordings the audio thread wants the whole of.
    wanted: AtomicUsize,
    /// Bytes of whole recordings in memory, and the most that are kept.
    held: AtomicUsize,
    budget: AtomicUsize,
    /// Counts the notes that were played, to tell which recording was
    /// played longest ago.
    clock: AtomicU64,
    /// How many devices are playing live: while there are any, workers
    /// look often for what they want.
    players: AtomicUsize,
    workers: Mutex<usize>,
    /// Notes that were cut short because the rest of their recording had
    /// not been read by the time its start ran out.
    short: AtomicUsize,
    /// The last holder of the pool let go of it: the workers go home.
    stopped: AtomicBool,
}

/// The recordings of every sampled instrument a factory has made, and the
/// threads that read them. Cheap to clone; the clones are one pool.
#[derive(Clone)]
pub struct Pool {
    shared: Arc<Shared>,
    /// Gone with the last clone, which is what tells the workers to stop.
    _life: Arc<Life>,
}

struct Life {
    shared: Arc<Shared>,
}

impl Drop for Life {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.shared.signal.notify_all();
    }
}

/// How much the pool holds.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// Bytes of whole recordings in memory.
    pub whole_bytes: usize,
    /// The most of those it keeps.
    pub budget: usize,
    /// Notes that were cut short for want of the rest of their recording.
    pub short_notes: usize,
}

impl Pool {
    pub fn new(loader: Arc<dyn AudioLoader>) -> Self {
        let shared = Arc::new(Shared {
            loader,
            recordings: Mutex::new(HashMap::new()),
            starts: Mutex::new(VecDeque::new()),
            signal: Condvar::new(),
            wanted: AtomicUsize::new(0),
            held: AtomicUsize::new(0),
            budget: AtomicUsize::new(DEFAULT_BUDGET),
            clock: AtomicU64::new(0),
            players: AtomicUsize::new(0),
            workers: Mutex::new(0),
            short: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
        });
        Self { _life: Arc::new(Life { shared: shared.clone() }), shared }
    }

    pub fn loader(&self) -> &dyn AudioLoader {
        self.shared.loader.as_ref()
    }

    /// The recording at `path`: the one there is already, if anything
    /// plays it.
    pub fn recording(&self, path: &str) -> Arc<Recording> {
        let mut recordings = lock(&self.shared.recordings);
        if let Some(found) = recordings.get(path).and_then(Weak::upgrade) {
            return found;
        }
        let recording = Arc::new(Recording {
            path: path.to_string(),
            start: OnceLock::new(),
            whole: Mutex::new(None),
            wanted: AtomicBool::new(false),
            used: AtomicU64::new(0),
            reading: Mutex::new(()),
        });
        recordings.retain(|_, known| known.strong_count() > 0);
        recordings.insert(path.to_string(), Arc::downgrade(&recording));
        recording
    }

    /// Has the starts of `recordings` read by the workers, in this order.
    /// Returns at once.
    pub fn read_starts(&self, recordings: &[Arc<Recording>]) {
        let waiting: Vec<Arc<Recording>> =
            recordings.iter().filter(|recording| !recording.is_settled()).cloned().collect();
        if waiting.is_empty() {
            return;
        }
        lock(&self.shared.starts).extend(waiting);
        self.staff();
        self.shared.signal.notify_all();
    }

    /// The start of `recording`, read here and now if it has not been.
    /// For a thread that may wait.
    pub fn start_now<'a>(&self, recording: &'a Recording) -> Option<(&'a Facts, &'a Arc<Audio>)> {
        recording.start.get_or_init(|| read_start(self.shared.loader.as_ref(), &recording.path));
        recording.start()
    }

    /// The whole of `recording`, read here and now if it is not in
    /// memory. For a thread that may wait. `None` for a file that cannot
    /// be read.
    pub fn whole_now(&self, recording: &Recording) -> Option<Arc<Audio>> {
        let (facts, start) = self.start_now(recording)?;
        if start.is_whole() {
            return Some(start.clone());
        }
        let _reading = lock(&recording.reading);
        if let Some(whole) = lock(&recording.whole).clone() {
            return Some(whole);
        }
        let whole = Arc::new(read_whole(self.shared.loader.as_ref(), &recording.path, facts.frames).ok()?);
        *lock(&recording.whole) = Some(whole.clone());
        self.shared.held.fetch_add(whole.bytes(), Ordering::Relaxed);
        self.shared.trim(&recording.path);
        Some(whole)
    }

    /// A note began on `recording`: it is the one most lately played.
    /// For the audio thread.
    #[inline]
    pub fn played(&self, recording: &Recording) {
        recording.used.store(self.shared.clock.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
    }

    /// Asks the workers for the whole of `recording`. For the audio
    /// thread: nothing here waits.
    #[inline]
    pub fn want(&self, recording: &Recording) {
        if !recording.wanted.swap(true, Ordering::AcqRel) {
            self.shared.wanted.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// A note was cut short for want of the rest of its recording.
    #[inline]
    pub fn cut_short(&self) {
        self.shared.short.fetch_add(1, Ordering::Relaxed);
    }

    /// A device that plays live, for as long as what this returns is kept:
    /// the workers look often for what it wants.
    pub fn player(&self) -> Player {
        self.shared.players.fetch_add(1, Ordering::AcqRel);
        self.staff();
        self.shared.signal.notify_all();
        Player { shared: self.shared.clone() }
    }

    pub fn held(&self) -> Held {
        Held {
            whole_bytes: self.shared.held.load(Ordering::Relaxed),
            budget: self.shared.budget.load(Ordering::Relaxed),
            short_notes: self.shared.short.load(Ordering::Relaxed),
        }
    }

    /// Sets how many bytes of whole recordings are kept, and lets go of
    /// what is over.
    pub fn set_budget(&self, bytes: usize) {
        self.shared.budget.store(bytes, Ordering::Relaxed);
        self.shared.trim("");
    }

    /// Starts the worker threads, if they are not running.
    fn staff(&self) {
        let mut workers = lock(&self.shared.workers);
        if *workers > 0 {
            return;
        }
        let wanted = std::thread::available_parallelism().map_or(2, |threads| threads.get() / 2).clamp(1, MAX_WORKERS);
        for index in 0..wanted {
            let shared = self.shared.clone();
            let spawned =
                std::thread::Builder::new().name(format!("trill-recordings-{index}")).spawn(move || work(shared));
            if spawned.is_ok() {
                *workers += 1;
            }
        }
    }
}

/// See [`Pool::player`].
pub struct Player {
    shared: Arc<Shared>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.players.fetch_sub(1, Ordering::AcqRel);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    /// The recording a note waits for the whole of, if there is one.
    fn next_wanted(&self) -> Option<Arc<Recording>> {
        if self.wanted.load(Ordering::Acquire) == 0 {
            return None;
        }
        let recordings = lock(&self.recordings);
        for recording in recordings.values().filter_map(Weak::upgrade) {
            if recording.wanted.swap(false, Ordering::AcqRel) {
                self.wanted.fetch_sub(1, Ordering::AcqRel);
                return Some(recording);
            }
        }
        None
    }

    /// Reads the whole of `recording` for the voices that wait for it.
    fn fetch(&self, recording: &Recording) {
        let Some((facts, start)) = recording.start() else { return };
        if start.is_whole() {
            return;
        }
        let _reading = lock(&recording.reading);
        if lock(&recording.whole).is_some() {
            return;
        }
        let Ok(whole) = read_whole(self.loader.as_ref(), &recording.path, facts.frames) else { return };
        let whole = Arc::new(whole);
        self.held.fetch_add(whole.bytes(), Ordering::Relaxed);
        *lock(&recording.whole) = Some(whole);
        self.trim(&recording.path);
    }

    /// Lets go of whole recordings until the budget holds: the ones
    /// played longest ago first, and never one a voice is in, nor the one
    /// at `keep`, which was read this moment for a note.
    fn trim(&self, keep: &str) {
        while self.held.load(Ordering::Relaxed) > self.budget.load(Ordering::Relaxed) {
            let recordings: Vec<Arc<Recording>> = lock(&self.recordings).values().filter_map(Weak::upgrade).collect();
            let mut oldest: Option<&Arc<Recording>> = None;
            for recording in &recordings {
                if recording.path == keep {
                    continue;
                }
                let idle = lock(&recording.whole).as_ref().is_some_and(|whole| Arc::strong_count(whole) == 1);
                let older = oldest
                    .is_none_or(|found| recording.used.load(Ordering::Relaxed) < found.used.load(Ordering::Relaxed));
                if idle && older {
                    oldest = Some(recording);
                }
            }
            let Some(recording) = oldest else { return };
            let mut whole = lock(&recording.whole);
            // With the lock held no voice can take it, and none has it if
            // the pool is its one holder.
            if whole.as_ref().is_some_and(|audio| Arc::strong_count(audio) == 1)
                && let Some(audio) = whole.take()
            {
                self.held.fetch_sub(audio.bytes(), Ordering::Relaxed);
            }
        }
    }
}

/// What a worker thread does until the pool is gone.
fn work(shared: Arc<Shared>) {
    while !shared.stopped.load(Ordering::Acquire) {
        // What a sounding note waits for comes before a start.
        if let Some(recording) = shared.next_wanted() {
            shared.fetch(&recording);
            continue;
        }
        let next = lock(&shared.starts).pop_front();
        if let Some(recording) = next {
            recording.start.get_or_init(|| read_start(shared.loader.as_ref(), &recording.path));
            continue;
        }
        let pause = if shared.players.load(Ordering::Acquire) > 0 { LOOK_EVERY } else { LOOK_IDLE };
        let starts = lock(&shared.starts);
        if starts.is_empty() {
            let _ = shared.signal.wait_timeout(starts, pause);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemoryLoader;
    use std::time::Instant;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|frame| frame as f32 / frames as f32).collect()
    }

    fn pool(files: &[(&str, usize)]) -> Pool {
        let loader = MemoryLoader::default();
        for (path, frames) in files {
            loader.insert(path, ramp(*frames), 48_000.0);
        }
        Pool::new(Arc::new(loader))
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let begun = Instant::now();
        while !done() {
            assert!(begun.elapsed() < Duration::from_secs(10), "{what} did not happen");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_short_recording_is_whole_and_a_long_one_is_its_start_until_it_is_played() {
        let pool = pool(&[
            ("short.wav", 1_000),
            ("long.wav", 100_000),
            ("edge.wav", WHOLE_UP_TO),
            ("over.wav", WHOLE_UP_TO + 1),
        ]);
        let names = ["short.wav", "long.wav", "edge.wav", "over.wav", "gone.wav"];
        let recordings: Vec<Arc<Recording>> = names.iter().map(|name| pool.recording(name)).collect();
        assert!(recordings.iter().all(|recording| recording.start().is_none() && !recording.is_settled()));
        pool.read_starts(&recordings);
        wait_for("the starts being read", || recordings.iter().all(|recording| recording.is_settled()));

        let (facts, short) = recordings[0].start().unwrap();
        assert_eq!((facts.frames, facts.sample_rate, facts.mono), (1_000, 48_000.0, true));
        assert!(short.is_whole() && short.is_mono() && short.frames() == 1_000 && short.reach() == 1_000 + GUARD);
        let (left, right) = short.channels();
        assert_eq!(left.len(), 1_000 + 2 * GUARD);
        assert_eq!((left[GUARD - 1], left[GUARD], left[GUARD + 500], left[GUARD + 1_000]), (0.0, 0.0, 0.5, 0.0));
        assert_eq!(left.as_ptr(), right.as_ptr(), "one channel is both sides");

        let (facts, long) = recordings[1].start().unwrap();
        assert_eq!(facts.frames, 100_000);
        assert!(!long.is_whole() && long.frames() == START_FRAMES && long.reach() == START_FRAMES);
        assert!(recordings[2].start().unwrap().1.is_whole(), "a recording a little longer than a start is kept whole");
        assert!(!recordings[3].start().unwrap().1.is_whole());
        assert!(recordings[4].start().is_none() && recordings[4].problem().unwrap().contains("no such file"));

        // Nothing is whole that was not played; a note wants it, and a worker reads it.
        assert!(recordings[1].whole_if_there().is_none());
        assert_eq!(pool.held().whole_bytes, 0);
        let _player = pool.player();
        pool.want(&recordings[1]);
        pool.want(&recordings[1]);
        wait_for("the whole being read", || recordings[1].whole_if_there().is_some());
        let whole = recordings[1].whole_if_there().unwrap();
        assert!(whole.is_whole() && whole.frames() == 100_000);
        assert_eq!(
            whole.channels().0[GUARD..GUARD + START_FRAMES],
            long.channels().0[GUARD..],
            "the start is its start"
        );
        assert_eq!(pool.held().whole_bytes, (100_000 + 2 * GUARD) * 4);
        // The same path is the same recording, for whoever asks.
        assert!(Arc::ptr_eq(&pool.recording("long.wav"), &recordings[1]));
    }

    #[test]
    fn a_thread_that_may_wait_reads_what_it_needs_itself() {
        let pool = pool(&[("long.wav", 60_000), ("short.wav", 500)]);
        let (long, short) = (pool.recording("long.wav"), pool.recording("short.wav"));
        let whole = pool.whole_now(&long).unwrap();
        assert!(whole.is_whole() && whole.frames() == 60_000);
        assert!(Arc::ptr_eq(&whole, &pool.whole_now(&long).unwrap()), "read once");
        assert!(Arc::ptr_eq(&pool.whole_now(&short).unwrap(), short.start().unwrap().1), "a short one is its start");
        assert!(pool.whole_now(&pool.recording("gone.wav")).is_none());
        assert_eq!(pool.held().whole_bytes, (60_000 + 2 * GUARD) * 4, "what is whole anyway is not counted twice");
    }

    #[test]
    fn above_the_budget_the_recordings_longest_unplayed_are_let_go_of_but_never_one_a_voice_is_in() {
        let files: Vec<(String, usize)> = (0..6).map(|index| (format!("{index}.wav"), 50_000)).collect();
        let listed: Vec<(&str, usize)> = files.iter().map(|(name, frames)| (name.as_str(), *frames)).collect();
        let pool = pool(&listed);
        let one = (50_000 + 2 * GUARD) * 4;
        pool.set_budget(3 * one);
        let recordings: Vec<Arc<Recording>> = files.iter().map(|(name, _)| pool.recording(name)).collect();
        // A voice is in the first for the whole of this.
        pool.played(&recordings[0]);
        let voice = pool.whole_now(&recordings[0]).unwrap();
        for recording in &recordings[1..] {
            pool.played(recording);
            drop(pool.whole_now(recording));
        }
        let there: Vec<bool> = recordings.iter().map(|recording| recording.whole_if_there().is_some()).collect();
        assert_eq!(there, [true, false, false, false, true, true], "the oldest that nobody is in went");
        assert_eq!(pool.held().whole_bytes, 3 * one);
        drop(voice);
        // Played again, one that went is read again; the one played longest ago goes for it.
        pool.played(&recordings[2]);
        assert!(pool.whole_now(&recordings[2]).is_some());
        let there: Vec<bool> = recordings.iter().map(|recording| recording.whole_if_there().is_some()).collect();
        assert_eq!(there, [false, false, true, false, true, true]);
        // A budget smaller than what is sounding keeps what is sounding.
        let sounding: Vec<Arc<Audio>> = recordings.iter().filter_map(|recording| recording.whole_if_there()).collect();
        pool.set_budget(one);
        assert_eq!(pool.held().whole_bytes, 3 * one);
        drop(sounding);
        pool.set_budget(one);
        assert_eq!(pool.held().whole_bytes, one);
    }

    #[test]
    fn the_workers_go_when_the_pool_does() {
        let pool = pool(&[("a.wav", 100)]);
        let recording = pool.recording("a.wav");
        pool.read_starts(&[recording.clone()]);
        wait_for("the start", || recording.is_settled());
        let shared = Arc::downgrade(&pool.shared);
        drop(pool);
        wait_for("the pool going", || shared.upgrade().is_none());
    }
}
