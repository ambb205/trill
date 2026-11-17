//! A cache of decoded audio files, shared by everything that needs samples.

use crate::resample::Resampler;
use crate::{MediaError, Peaks, decode};
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::SystemTime;
use trill_core::audio::{AudioBuffer, AudioLoader};

/// A thread-safe cache of decoded audio files and their waveform overviews.
///
/// - A file is decoded once however many clips, devices or threads ask for
///   it; concurrent requests for a file that is still decoding wait for
///   that one decode.
/// - Entries are checked against the file's size and modified time on every
///   request, so a file that was re-exported is reloaded and one that failed
///   to load is retried only once it has changed. A missing file costs one
///   metadata lookup per request, never a read.
/// - Memory is bounded: when what the cache holds exceeds its budget the
///   least recently used entries are let go. A buffer something else still
///   holds stays alive through its `Arc`, and the cache remembers it weakly,
///   so asking again returns that same buffer rather than a second copy.
///
/// This is the [`AudioLoader`] the engine and devices are given.
pub struct MediaCache {
    state: Mutex<State>,
    /// Notified whenever a load finishes, for requests waiting on it.
    finished: Condvar,
}

/// Counters for diagnostics and tests.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CacheStats {
    /// Bytes of audio and overviews the cache itself keeps alive.
    pub bytes: usize,
    /// Files decoded so far; a request served from the cache does not count.
    pub decodes: u64,
}

struct State {
    budget: usize,
    bytes: usize,
    decodes: u64,
    /// Advances on every use; the entry with the lowest stamp is the least
    /// recently used.
    clock: u64,
    /// The rate [`AudioLoader::load`] converts to, if any.
    loader_rate: Option<u32>,
    /// Kept between loads because designing the filter costs more than
    /// converting a short sample, and consecutive loads share their rates.
    resampler: Option<Arc<Resampler>>,
    items: HashMap<Key, Slot>,
}

/// One cached thing: a file's audio, its audio at another rate, or its
/// waveform overview.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    product: Product,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum Product {
    /// The audio as stored in the file.
    Audio,
    /// The audio converted to this sample rate in hertz.
    AudioAt(u32),
    Peaks,
}

enum Slot {
    /// Being produced by some thread; others wait on `finished`.
    Loading,
    Ready(Item),
}

struct Item {
    /// The file's identity when this was produced; `None` if it could not
    /// be examined (usually because it does not exist).
    stamp: Option<FileStamp>,
    used: u64,
    content: Content,
}

enum Content {
    Failed(MediaError),
    Audio(Arc<AudioBuffer>),
    /// Audio the budget no longer pays for, possibly still alive elsewhere.
    Released(Weak<AudioBuffer>),
    Peaks(Arc<Peaks>),
}

impl Content {
    /// Bytes this keeps alive that nothing else may be holding.
    fn bytes(&self) -> usize {
        match self {
            Self::Audio(audio) => audio_bytes(audio),
            Self::Peaks(peaks) => peaks.memory_bytes(),
            Self::Failed(_) | Self::Released(_) => 0,
        }
    }

    fn audio(&self) -> Option<Arc<AudioBuffer>> {
        match self {
            Self::Audio(audio) => Some(audio.clone()),
            Self::Released(audio) => audio.upgrade(),
            _ => None,
        }
    }

    fn peaks(&self) -> Option<Arc<Peaks>> {
        match self {
            Self::Peaks(peaks) => Some(peaks.clone()),
            _ => None,
        }
    }
}

fn audio_bytes(audio: &AudioBuffer) -> usize {
    (audio.left.len() + audio.right.len()) * size_of::<f32>()
}

/// What identifies a version of a file without reading it: its size and
/// when it was last modified. Two stamps of a path that compare equal mean
/// the file has, as far as anyone can cheaply tell, not changed in between.
///
/// The cache keys what it holds by this, and so can anything derived from a
/// file's audio (stretched copies, analyses) that should be made again when
/// the file is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    /// The stamp of the file at `path` as it is now, or `None` if it cannot
    /// be examined (usually because it does not exist).
    pub fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self { len: metadata.len(), modified: metadata.modified().ok() })
    }
}

impl Default for MediaCache {
    /// A cache with a budget of [`MediaCache::DEFAULT_BUDGET`].
    fn default() -> Self {
        Self::new(Self::DEFAULT_BUDGET)
    }
}

impl MediaCache {
    /// One gibibyte: about an hour and a half of stereo audio at 48 kHz.
    pub const DEFAULT_BUDGET: usize = 1 << 30;

    /// A cache that keeps at most `budget_bytes` of decoded audio alive on
    /// its own account.
    pub fn new(budget_bytes: usize) -> Self {
        let state = State {
            budget: budget_bytes,
            bytes: 0,
            decodes: 0,
            clock: 0,
            loader_rate: None,
            resampler: None,
            items: HashMap::new(),
        };
        Self { state: Mutex::new(state), finished: Condvar::new() }
    }

    /// The decoded file at its own sample rate.
    pub fn audio(&self, path: &Path) -> Result<Arc<AudioBuffer>, MediaError> {
        self.fetch(path, Product::Audio, Content::audio, Content::Audio, || {
            self.state.lock().decodes += 1;
            decode(path).map(Arc::new)
        })
    }

    /// The decoded file converted to `sample_rate` (see [`crate::resample`]),
    /// which is the file's own buffer when it is already at that rate.
    /// Converted audio is cached like any other, so the conversion happens
    /// once per file and rate. A rate that is not a positive number means
    /// the file's own.
    pub fn audio_at_rate(&self, path: &Path, sample_rate: f64) -> Result<Arc<AudioBuffer>, MediaError> {
        let Some(rate) = whole_rate(sample_rate) else { return self.audio(path) };
        // A conversion already made answers without touching the original,
        // which is then free to age out of the cache.
        let key = Key { path: path.to_path_buf(), product: Product::AudioAt(rate) };
        let stamp = FileStamp::of(path);
        if let Lookup::Hit(Ok(converted)) = self.state.lock().lookup(&key, stamp, &Content::audio) {
            return Ok(converted);
        }
        let original = self.audio(path)?;
        let Some(resampler) = self.resampler(original.sample_rate, f64::from(rate)) else { return Ok(original) };
        self.fetch(path, key.product, Content::audio, Content::Audio, || Ok(Arc::new(resampler.process(&original))))
    }

    /// The waveform overview of the file, built once and cached with it.
    pub fn peaks(&self, path: &Path) -> Result<Arc<Peaks>, MediaError> {
        self.fetch(path, Product::Peaks, Content::peaks, Content::Peaks, || {
            Ok(Arc::new(Peaks::build(&*self.audio(path)?)))
        })
    }

    /// Makes [`AudioLoader::load`] return audio converted to `sample_rate`
    /// (the engine's rate), or at each file's own rate for `None`.
    pub fn set_loader_rate(&self, sample_rate: Option<f64>) {
        self.state.lock().loader_rate = sample_rate.and_then(whole_rate);
    }

    /// The rate [`AudioLoader::load`] converts to, if one is set.
    pub fn loader_rate(&self) -> Option<f64> {
        self.state.lock().loader_rate.map(f64::from)
    }

    /// Changes the memory budget, letting go of entries if it shrank.
    pub fn set_budget(&self, budget_bytes: usize) {
        let mut state = self.state.lock();
        state.budget = budget_bytes;
        state.trim();
    }

    pub fn budget(&self) -> usize {
        self.state.lock().budget
    }

    pub fn stats(&self) -> CacheStats {
        let state = self.state.lock();
        CacheStats { bytes: state.bytes, decodes: state.decodes }
    }

    /// Forgets everything known about `path`, so the next request reads the
    /// file again even if it looks unchanged.
    pub fn forget(&self, path: &Path) {
        let mut state = self.state.lock();
        state.items.retain(|key, slot| key.path != path || matches!(slot, Slot::Loading));
        state.recount();
    }

    /// Forgets everything. Loads in progress finish and are cached.
    pub fn clear(&self) {
        let mut state = self.state.lock();
        state.items.retain(|_, slot| matches!(slot, Slot::Loading));
        state.recount();
    }

    /// Returns the cached `product` of `path`, producing it if it is absent
    /// or the file has changed. `read` takes the wanted value out of a
    /// cached item and `wrap` puts a new one in.
    fn fetch<T>(
        &self,
        path: &Path,
        product: Product,
        read: impl Fn(&Content) -> Option<Arc<T>>,
        wrap: impl FnOnce(Arc<T>) -> Content,
        produce: impl FnOnce() -> Result<Arc<T>, MediaError>,
    ) -> Result<Arc<T>, MediaError> {
        let stamp = FileStamp::of(path);
        let key = Key { path: path.to_path_buf(), product };

        let mut state = self.state.lock();
        loop {
            match state.lookup(&key, stamp, &read) {
                Lookup::Hit(result) => return result,
                Lookup::Loading => self.finished.wait(&mut state),
                Lookup::Miss => break,
            }
        }
        state.items.insert(key.clone(), Slot::Loading);
        drop(state);

        // If producing panics, the guard clears the marker so that waiting
        // requests retry instead of waiting forever.
        let guard = LoadingGuard { cache: self, key: &key };
        let result = produce();
        std::mem::forget(guard);

        let mut state = self.state.lock();
        state.clock += 1;
        let used = state.clock;
        let content = match &result {
            Ok(value) => wrap(value.clone()),
            Err(error) => Content::Failed(error.clone()),
        };
        state.items.insert(key, Slot::Ready(Item { stamp, used, content }));
        state.recount();
        state.trim();
        drop(state);
        self.finished.notify_all();
        result
    }

    /// A converter between the two rates, reusing the last one when the
    /// rates match. `None` when no conversion is needed or possible.
    fn resampler(&self, source_rate: f64, target_rate: f64) -> Option<Arc<Resampler>> {
        let cached = self.state.lock().resampler.clone();
        if let Some(resampler) = cached
            && resampler.source_rate() == source_rate
            && resampler.target_rate() == target_rate
        {
            return Some(resampler);
        }
        let resampler = Arc::new(Resampler::new(source_rate, target_rate)?);
        self.state.lock().resampler = Some(resampler.clone());
        Some(resampler)
    }
}

impl AudioLoader for MediaCache {
    fn load(&self, path: &str) -> Result<Arc<AudioBuffer>, String> {
        let path = Path::new(path);
        let rate = self.state.lock().loader_rate;
        let loaded = match rate {
            Some(rate) => self.audio_at_rate(path, f64::from(rate)),
            None => self.audio(path),
        };
        loaded.map_err(|error| error.to_string())
    }

    /// The file itself, read as far as it is asked: nothing of it is kept
    /// here, and it is at its own sample rate.
    fn open(&self, path: &str) -> Result<Box<dyn trill_core::audio::AudioStream>, String> {
        match crate::open(Path::new(path)) {
            Ok(file) => Ok(Box::new(file)),
            Err(error) => Err(error.to_string()),
        }
    }

    fn size(&self, path: &str) -> Option<u64> {
        std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file).map(|found| found.len())
    }
}

/// What the cache holds for a key, for the current version of its file.
enum Lookup<T> {
    /// The value, or the remembered reason there is none.
    Hit(Result<Arc<T>, MediaError>),
    /// Another thread is producing it.
    Loading,
    /// Nothing usable: absent, out of date, or released and since dropped.
    Miss,
}

impl State {
    /// Looks `key` up, counting a hit as a use. Audio that had been
    /// released but is still alive goes back onto the budget: it is in use
    /// again.
    fn lookup<T>(
        &mut self,
        key: &Key,
        stamp: Option<FileStamp>,
        read: &impl Fn(&Content) -> Option<Arc<T>>,
    ) -> Lookup<T> {
        self.clock += 1;
        let clock = self.clock;
        let item = match self.items.get_mut(key) {
            Some(Slot::Loading) => return Lookup::Loading,
            Some(Slot::Ready(item)) if item.stamp == stamp => item,
            _ => return Lookup::Miss,
        };
        if let Content::Failed(error) = &item.content {
            return Lookup::Hit(Err(error.clone()));
        }
        let Some(value) = read(&item.content) else { return Lookup::Miss };
        item.used = clock;
        if let Content::Released(audio) = &item.content
            && let Some(audio) = audio.upgrade()
        {
            item.content = Content::Audio(audio);
            self.recount();
            self.trim();
        }
        Lookup::Hit(Ok(value))
    }

    fn recount(&mut self) {
        self.bytes = self.items.values().map(Slot::bytes).sum();
    }

    /// Lets go of the least recently used entries until the budget holds.
    fn trim(&mut self) {
        let mut released = false;
        while self.bytes > self.budget {
            let oldest = self
                .items
                .iter()
                .filter(|(_, slot)| slot.bytes() > 0)
                .min_by_key(|(_, slot)| slot.used())
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            let Some(Slot::Ready(item)) = self.items.get_mut(&key) else { break };
            self.bytes -= item.content.bytes();
            if let Content::Audio(audio) = &item.content {
                item.content = Content::Released(Arc::downgrade(audio));
            } else {
                self.items.remove(&key);
            }
            released = true;
        }
        if released {
            // Released audio nobody else kept is gone; drop its bookkeeping.
            self.items.retain(|_, slot| !slot.is_dead());
        }
    }
}

impl Slot {
    fn bytes(&self) -> usize {
        match self {
            Self::Ready(item) => item.content.bytes(),
            Self::Loading => 0,
        }
    }

    fn used(&self) -> u64 {
        match self {
            Self::Ready(item) => item.used,
            Self::Loading => u64::MAX,
        }
    }

    fn is_dead(&self) -> bool {
        matches!(self, Self::Ready(Item { content: Content::Released(audio), .. }) if audio.strong_count() == 0)
    }
}

/// Removes a `Loading` marker if the load it stands for unwinds.
struct LoadingGuard<'a> {
    cache: &'a MediaCache,
    key: &'a Key,
}

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        self.cache.state.lock().items.remove(self.key);
        self.cache.finished.notify_all();
    }
}

/// A sample rate as whole hertz, if it is a usable one.
fn whole_rate(sample_rate: f64) -> Option<u32> {
    let rounded = sample_rate.round();
    (1.0..=f64::from(u32::MAX)).contains(&rounded).then_some(rounded as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WavFormat, write_wav};
    use std::time::Duration;

    fn tone(frames: usize, rate: f64, level: f32) -> AudioBuffer {
        let channel: Vec<f32> = (0..frames).map(|n| level * (n as f32 * 0.05).sin()).collect();
        AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }
    }

    fn write(path: &Path, frames: usize, rate: f64, level: f32) {
        write_wav(path, &tone(frames, rate, level), WavFormat::Float32).unwrap();
    }

    /// Rewrites `path` with different content of the same length and makes
    /// sure its modified time moves, whatever the file system's resolution.
    fn rewrite(path: &Path, frames: usize, level: f32) {
        let before = std::fs::metadata(path).unwrap().modified().unwrap();
        write(path, frames, 48_000.0, level);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(before + Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn a_file_is_decoded_once_and_shared() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.wav");
        write(&path, 1_000, 48_000.0, 0.5);
        let cache = MediaCache::default();

        let first = cache.audio(&path).unwrap();
        let second = cache.audio(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.frames(), 1_000);
        assert_eq!(cache.stats(), CacheStats { bytes: audio_bytes(&first), decodes: 1 });

        // The same through the interface the engine uses.
        let loader: &dyn AudioLoader = &cache;
        let third = loader.load(path.to_str().unwrap()).unwrap();
        assert!(Arc::ptr_eq(&first, &third));
        assert_eq!(cache.stats().decodes, 1);
    }

    #[test]
    fn a_changed_file_is_reloaded() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.wav");
        write(&path, 1_000, 48_000.0, 0.5);
        let cache = MediaCache::default();
        let before = cache.audio(&path).unwrap();
        let peaks_before = cache.peaks(&path).unwrap();

        // Same size, new content: only the modified time gives it away.
        rewrite(&path, 1_000, 0.25);
        let after = cache.audio(&path).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert!((after.left[100] - before.left[100] * 0.5).abs() < 1.0e-6);
        assert!(!Arc::ptr_eq(&peaks_before, &cache.peaks(&path).unwrap()));

        // A different size with the old modified time is also a change.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        write(&path, 2_000, 48_000.0, 0.25);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
        assert_eq!(cache.audio(&path).unwrap().frames(), 2_000);
        assert_eq!(cache.stats().decodes, 3);
    }

    #[test]
    fn failures_are_remembered_until_the_file_changes() {
        let folder = tempfile::tempdir().unwrap();
        let missing = folder.path().join("missing.wav");
        let cache = MediaCache::default();
        let error = cache.audio(&missing).unwrap_err();
        assert_eq!(error, MediaError::NotFound { path: missing.clone() });
        assert_eq!(cache.audio(&missing).unwrap_err(), error);
        assert_eq!(cache.peaks(&missing).unwrap_err(), error);
        assert_eq!(cache.stats().decodes, 1, "a missing file is looked at once");
        assert!(cache.load(missing.to_str().unwrap()).unwrap_err().contains("does not exist"));

        // The file appears: the failure is no longer current.
        write(&missing, 500, 48_000.0, 0.5);
        assert_eq!(cache.audio(&missing).unwrap().frames(), 500);

        // A file that is not audio fails once, and again only after it changes.
        let garbage = folder.path().join("garbage.wav");
        std::fs::write(&garbage, vec![7u8; 4_096]).unwrap();
        let decodes = cache.stats().decodes;
        assert!(cache.audio(&garbage).is_err());
        assert!(cache.audio(&garbage).is_err());
        assert_eq!(cache.stats().decodes, decodes + 1);
        cache.forget(&garbage);
        assert!(cache.audio(&garbage).is_err());
        assert_eq!(cache.stats().decodes, decodes + 2, "forgetting forces another look");
    }

    #[test]
    fn the_budget_evicts_the_least_recently_used() {
        let folder = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..4).map(|n| folder.path().join(format!("{n}.wav"))).collect();
        for path in &paths {
            write(path, 10_000, 48_000.0, 0.5);
        }
        let one = 10_000 * 8;
        let cache = MediaCache::new(one * 2 + one / 2);

        drop(cache.audio(&paths[0]).unwrap());
        drop(cache.audio(&paths[1]).unwrap());
        assert_eq!(cache.stats().bytes, one * 2);
        // Use 0 again, so 1 is now the least recently used.
        drop(cache.audio(&paths[0]).unwrap());
        drop(cache.audio(&paths[2]).unwrap());
        assert_eq!(cache.stats(), CacheStats { bytes: one * 2, decodes: 3 });

        drop(cache.audio(&paths[0]).unwrap());
        drop(cache.audio(&paths[2]).unwrap());
        assert_eq!(cache.stats().decodes, 3, "0 and 2 were kept");
        drop(cache.audio(&paths[1]).unwrap());
        assert_eq!(cache.stats().decodes, 4, "1 was evicted and had to be decoded again");

        cache.set_budget(0);
        assert_eq!(cache.stats().bytes, 0);
        assert_eq!(cache.budget(), 0);
    }

    #[test]
    fn evicted_buffers_still_in_use_are_handed_back_not_decoded_again() {
        let folder = tempfile::tempdir().unwrap();
        let (a, b) = (folder.path().join("a.wav"), folder.path().join("b.wav"));
        write(&a, 10_000, 48_000.0, 0.5);
        write(&b, 10_000, 48_000.0, 0.5);
        // Room for one file only.
        let cache = MediaCache::new(10_000 * 8);

        let held = cache.audio(&a).unwrap();
        drop(cache.audio(&b).unwrap());
        assert_eq!(cache.stats().bytes, 10_000 * 8, "only b is paid for now");
        // `a` was evicted but lives on in `held`; the cache finds it again.
        let again = cache.audio(&a).unwrap();
        assert!(Arc::ptr_eq(&held, &again));
        assert_eq!(cache.stats().decodes, 2);

        // A file larger than the whole budget is still returned.
        let big = folder.path().join("big.wav");
        write(&big, 50_000, 48_000.0, 0.5);
        let big_audio = cache.audio(&big).unwrap();
        assert_eq!(big_audio.frames(), 50_000);
        assert!(Arc::ptr_eq(&big_audio, &cache.audio(&big).unwrap()));
        assert!(cache.stats().bytes <= 10_000 * 8);
    }

    #[test]
    fn concurrent_requests_share_one_decode() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("long.wav");
        write(&path, 400_000, 48_000.0, 0.5);
        let cache = MediaCache::default();
        let loaded: Vec<Arc<AudioBuffer>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| cache.audio(&path).unwrap())).collect();
            workers.into_iter().map(|worker| worker.join().unwrap()).collect()
        });
        assert!(loaded.iter().all(|audio| Arc::ptr_eq(audio, &loaded[0])));
        assert_eq!(cache.stats().decodes, 1);

        // The same for overviews, which also decode on a miss.
        let other = folder.path().join("other.wav");
        write(&other, 400_000, 48_000.0, 0.5);
        let peaks: Vec<Arc<Peaks>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| cache.peaks(&other).unwrap())).collect();
            workers.into_iter().map(|worker| worker.join().unwrap()).collect()
        });
        assert!(peaks.iter().all(|built| Arc::ptr_eq(built, &peaks[0])));
        assert_eq!(cache.stats().decodes, 2);
    }

    #[test]
    fn peaks_are_built_once_and_match_the_audio() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.wav");
        write(&path, 30_000, 44_100.0, 0.5);
        let cache = MediaCache::default();
        let peaks = cache.peaks(&path).unwrap();
        assert!(Arc::ptr_eq(&peaks, &cache.peaks(&path).unwrap()));
        assert_eq!(*peaks, Peaks::build(&cache.audio(&path).unwrap()));
        assert_eq!(peaks.frames(), 30_000);
        assert_eq!(cache.stats().decodes, 1);
        assert_eq!(cache.stats().bytes, 30_000 * 8 + peaks.memory_bytes());
    }

    #[test]
    fn audio_is_converted_to_the_requested_rate_once() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.wav");
        write(&path, 44_100, 44_100.0, 0.5);
        let cache = MediaCache::default();

        let converted = cache.audio_at_rate(&path, 48_000.0).unwrap();
        assert_eq!((converted.sample_rate, converted.frames()), (48_000.0, 48_000));
        assert!(Arc::ptr_eq(&converted, &cache.audio_at_rate(&path, 48_000.0).unwrap()));
        // Already at the rate: the file's own buffer, not a copy.
        let original = cache.audio(&path).unwrap();
        assert!(Arc::ptr_eq(&original, &cache.audio_at_rate(&path, 44_100.0).unwrap()));
        assert!(Arc::ptr_eq(&original, &cache.audio_at_rate(&path, f64::NAN).unwrap()));
        assert_eq!(cache.stats().decodes, 1);

        // The loader follows the configured rate.
        let name = path.to_str().unwrap();
        assert!(Arc::ptr_eq(&original, &cache.load(name).unwrap()));
        cache.set_loader_rate(Some(48_000.0));
        assert_eq!(cache.loader_rate(), Some(48_000.0));
        assert!(Arc::ptr_eq(&converted, &cache.load(name).unwrap()));

        // A changed file is converted afresh.
        rewrite(&path, 44_100, 0.25);
        let reloaded = cache.load(name).unwrap();
        assert!(!Arc::ptr_eq(&converted, &reloaded));
        assert_eq!(reloaded.sample_rate, 48_000.0);
    }

    #[test]
    fn clearing_forgets_everything() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.wav");
        write(&path, 1_000, 48_000.0, 0.5);
        let cache = MediaCache::default();
        let before = cache.audio(&path).unwrap();
        cache.peaks(&path).unwrap();
        cache.clear();
        assert_eq!(cache.stats().bytes, 0);
        assert!(!Arc::ptr_eq(&before, &cache.audio(&path).unwrap()));
    }
}
