//! A take's audio on its way to disk.
//!
//! The audio thread never touches a file. While a take is recorded it
//! hands the frames of each recorded input to a ring ([`Feeds`]), and a
//! thread of the take's own ([`TakeWriter`]) empties the rings into files
//! that are WAV files from their first frame
//! ([`GrowingWav`](trill_media::GrowingWav)). The same thread works out
//! what the recording looks like as it goes ([`Growing::columns`]), so
//! that a clip can be drawn growing without anybody decoding a file that
//! is still being written.
//!
//! A frame that finds no room in its ring (the disk did not keep up for
//! seconds on end) is not shifted onto the next one that does: the audio
//! thread says where the room ran out and for how long ([`Gap`]), and the
//! file gets silence of that length there. It is counted
//! ([`Growing::dropped`]).

use crate::capture::Reader;
use crate::graph::MAX_BLOCK;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use trill_core::TrackId;
use trill_media::{GrowingLimits as Limits, GrowingWav};

/// How many frames of a recording one column of its picture stands for.
pub const COLUMN_FRAMES: usize = 256;
/// How much audio a ring holds: how long the disk may keep the writer
/// waiting before frames find no room.
const RING_SECONDS: f64 = 10.0;
/// How often the writer looks at its rings.
const LOOK_EVERY: Duration = Duration::from_millis(15);
/// How many stretches without room can be waiting to be said.
const GAPS: usize = 64;
/// Frames written at a time.
const CHUNK_FRAMES: usize = 8192;

/// A stretch of frames that found no room on their way to a file: the
/// frame of the take it began at, and how many there were.
#[derive(Copy, Clone, Debug)]
struct Gap {
    at: u64,
    frames: u64,
}

/// The audio thread's end of one file of a take.
struct Feed {
    /// The channels of the input it takes: the left, and the right of a
    /// pair.
    left: usize,
    right: Option<usize>,
    samples: rtrb::Producer<f32>,
    gaps: rtrb::Producer<Gap>,
    /// Frames offered so far, whether or not they found room, and where
    /// the stretch without room began that is going on.
    offered: u64,
    without_room: Option<u64>,
}

impl Feed {
    /// Offers `frames` frames, every one's channels side by side in
    /// `samples`.
    fn offer(&mut self, samples: &[f32], frames: usize) {
        if let Some(from) = self.without_room {
            // The stretch is said before anything follows it, or it goes on.
            let said = self.samples.slots() >= samples.len()
                && self.gaps.push(Gap { at: from, frames: self.offered - from }).is_ok();
            if !said {
                self.offered += frames as u64;
                return;
            }
            self.without_room = None;
        }
        if self.samples.push_entire_slice(samples).is_err() {
            self.without_room = Some(self.offered);
        }
        self.offered += frames as u64;
    }
}

/// The audio thread's end of a take's files: what it hands each block of
/// the recorded inputs to. Realtime-safe.
pub(crate) struct Feeds {
    feeds: Vec<Feed>,
    /// Room for a block of a pair, side by side.
    block: Vec<f32>,
    /// How many frames have been handed over.
    pub handed: u64,
    /// How many frames more than lie between the microphone and the
    /// speaker by the devices' own account are handed over after the song
    /// has stopped: what the measurement added, and a little to fade with.
    pub tail: u32,
}

impl Feeds {
    /// Which channels of the input are recorded, a bit each.
    pub fn channels(&self) -> u32 {
        let bit = |channel: usize| 1u32.checked_shl(channel as u32).unwrap_or(0);
        self.feeds.iter().fold(0, |mask, feed| mask | bit(feed.left) | feed.right.map_or(0, bit))
    }

    /// Hands over the block `input` last read: `frames` frames of every
    /// recorded channel. Without an input (it was closed under the take)
    /// they are silence.
    pub fn push(&mut self, input: Option<&Reader>, frames: usize) {
        let frames = frames.min(MAX_BLOCK);
        let Feeds { feeds, block, .. } = self;
        for feed in feeds.iter_mut() {
            let of = |channel: usize| input.map(|input| input.channel(channel)).filter(|heard| heard.len() >= frames);
            match (of(feed.left), feed.right) {
                (Some(left), None) => feed.offer(&left[..frames], frames),
                (left, right) => {
                    let width = 1 + usize::from(right.is_some());
                    let right = right.and_then(of);
                    for frame in 0..frames {
                        block[frame * width] = left.map_or(0.0, |left| left[frame]);
                        if width == 2 {
                            block[frame * 2 + 1] = right.map_or(0.0, |right| right[frame]);
                        }
                    }
                    feed.offer(&block[..frames * width], frames);
                }
            }
        }
        self.handed += frames as u64;
    }
}

/// A file of a take as it is written, for whoever wants to know how far
/// it is: asked from any thread but an audio thread.
pub struct Growing {
    path: PathBuf,
    track: TrackId,
    channels: u16,
    sample_rate: f64,
    frames: AtomicU64,
    dropped: AtomicU64,
    /// The highest level so far, as the bits of an `f32`.
    peak: AtomicU32,
    /// The lowest and the highest sample of every [`COLUMN_FRAMES`] frames.
    columns: Mutex<Vec<[f32; 2]>>,
    failed: Mutex<Option<String>>,
}

impl Growing {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn track(&self) -> TrackId {
        self.track
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// How many frames have been written.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// How many of them are silence for frames that found no room on the
    /// way: the disk did not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The highest level so far: 1 is full scale.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    /// What the recording looks like from column `from` on: the lowest and
    /// the highest sample of every [`COLUMN_FRAMES`] frames written.
    pub fn columns(&self, from: usize) -> Vec<[f32; 2]> {
        let columns = self.columns.lock();
        columns.get(from..).map(<[_]>::to_vec).unwrap_or_default()
    }

    /// Why nothing more is written to it, if writing has stopped: the disk
    /// is full, or the file is as long as a file of its kind can be.
    pub fn failed(&self) -> Option<String> {
        self.failed.lock().clone()
    }
}

/// The writer's end of one file.
struct Written {
    growing: Arc<Growing>,
    /// `None` once writing to it has failed.
    file: Option<GrowingWav>,
    samples: rtrb::Consumer<f32>,
    gaps: rtrb::Consumer<Gap>,
    waiting: VecDeque<Gap>,
    /// Frames accounted for: written, or silence for a gap.
    done: u64,
    /// The column that is being filled, and how many frames are in it.
    column: ([f32; 2], usize),
    chunk: Vec<f32>,
}

impl Written {
    /// Puts what has been handed over so far in the file.
    fn drain(&mut self) {
        let channels = usize::from(self.growing.channels);
        // What is there to read is counted before the gaps are looked at:
        // a gap is always said before the frames that follow it are
        // handed over, so none of these lie behind a gap not yet seen.
        let mut left = self.samples.slots() / channels;
        while let Ok(gap) = self.gaps.pop() {
            self.waiting.push_back(gap);
        }
        loop {
            if let Some(gap) = self.waiting.front().copied().filter(|gap| gap.at <= self.done) {
                self.waiting.pop_front();
                self.silence(gap.frames);
                continue;
            }
            let until_gap = self.waiting.front().map_or(u64::MAX, |gap| gap.at - self.done);
            let frames = left.min(CHUNK_FRAMES).min(until_gap.min(usize::MAX as u64) as usize);
            if frames == 0 {
                return;
            }
            let count = frames * channels;
            self.chunk.resize(count, 0.0);
            if self.samples.pop_entire_slice(&mut self.chunk[..count]).is_err() {
                return;
            }
            left -= frames;
            self.write(frames);
        }
    }

    /// Writes the frames in `chunk`.
    fn write(&mut self, frames: usize) {
        let channels = usize::from(self.growing.channels);
        let held = std::mem::take(&mut self.chunk);
        let chunk = &held[..frames * channels];
        let mut peak = 0.0f32;
        let mut finished = Vec::new();
        for frame in chunk.chunks_exact(channels) {
            let (low, high) = frame.iter().fold((f32::MAX, f32::MIN), |(low, high), s| (low.min(*s), high.max(*s)));
            let column = &mut self.column;
            (column.0[0], column.0[1]) = (column.0[0].min(low), column.0[1].max(high));
            column.1 += 1;
            peak = peak.max(low.abs()).max(high.abs());
            if column.1 == COLUMN_FRAMES {
                finished.push(column.0);
                *column = ([f32::MAX, f32::MIN], 0);
            }
        }
        self.put(frames as u64, |file| file.write(chunk));
        let growing = &self.growing;
        growing.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        if !finished.is_empty() {
            growing.columns.lock().extend(finished);
        }
        self.chunk = held;
    }

    /// Writes silence for frames that found no room on the way.
    fn silence(&mut self, frames: u64) {
        self.growing.dropped.fetch_add(frames, Ordering::Relaxed);
        self.put(frames, |file| file.write_silence(frames));
        // The picture has the silence too.
        let mut columns = Vec::new();
        let mut left = frames as usize;
        while left > 0 {
            let column = &mut self.column;
            let fits = (COLUMN_FRAMES - column.1).min(left);
            (column.0[0], column.0[1]) = (column.0[0].min(0.0), column.0[1].max(0.0));
            column.1 += fits;
            left -= fits;
            if column.1 == COLUMN_FRAMES {
                columns.push(column.0);
                *column = ([f32::MAX, f32::MIN], 0);
            }
        }
        if !columns.is_empty() {
            self.growing.columns.lock().extend(columns);
        }
    }

    /// Has `write` put `frames` frames in the file, and keeps count. A
    /// file that takes fewer than it is handed, or none, is written to no
    /// more: what it holds stays, and why is said.
    fn put(&mut self, frames: u64, write: impl FnOnce(&mut GrowingWav) -> Result<u64, trill_media::MediaError>) {
        self.done += frames;
        let Some(file) = &mut self.file else { return };
        let why = match write(file) {
            Ok(taken) if taken == frames => {
                self.growing.frames.store(file.frames(), Ordering::Relaxed);
                return;
            }
            Ok(_) => "the recording is as long as one file can be (4 GB)".to_string(),
            Err(error) => error.to_string(),
        };
        // What is in it is put on disk as far as that can still be done,
        // and the file is as long as it then says it is: a take is whole
        // up to there, and not a frame longer than can be read back.
        let mut file = self.file.take().expect("it was there a moment ago");
        let _ = file.checkpoint();
        self.growing.frames.store(file.kept(), Ordering::Relaxed);
        *self.growing.failed.lock() = Some(why);
    }

    fn finish(mut self) {
        // The column that was being filled is part of the picture.
        if self.column.1 > 0 {
            self.growing.columns.lock().push(self.column.0);
        }
        if let Some(file) = self.file.take() {
            let frames = file.frames();
            match file.finish() {
                Ok(_) => self.growing.frames.store(frames, Ordering::Relaxed),
                Err(error) => *self.growing.failed.lock() = Some(error.to_string()),
            }
        }
    }
}

/// One file of a take that is to be written: where, for which track, and
/// from which channels of the input.
pub(crate) struct FileOf {
    pub track: TrackId,
    pub path: PathBuf,
    pub left: usize,
    pub right: Option<usize>,
}

/// The thread that writes a take's files, from when the take is asked for
/// until everything it was handed is on disk.
pub(crate) struct TakeWriter {
    thread: Option<JoinHandle<()>>,
    finish: Arc<AtomicBool>,
    files: Vec<Arc<Growing>>,
}

impl TakeWriter {
    /// Begins the files and the thread that writes them, and returns the
    /// audio thread's end with it. A file that cannot be begun is a
    /// sentence, and then none is.
    /// Every file is held to `limits`: a WAV file's own, as it comes.
    pub fn start(
        files: Vec<FileOf>,
        sample_rate: f64,
        tail: u32,
        limits: Limits,
    ) -> Result<(Self, Box<Feeds>), String> {
        let mut feeds = Vec::with_capacity(files.len());
        let mut written = Vec::with_capacity(files.len());
        for FileOf { track, path, left, right } in files {
            let channels = 1 + u16::from(right.is_some());
            let file =
                GrowingWav::create_within(&path, sample_rate, channels, limits).map_err(|error| error.to_string())?;
            let room = (sample_rate * RING_SECONDS) as usize * usize::from(channels);
            let (samples_in, samples_out) = rtrb::RingBuffer::new(room);
            let (gaps_in, gaps_out) = rtrb::RingBuffer::new(GAPS);
            let growing = Arc::new(Growing {
                path,
                track,
                channels,
                sample_rate,
                frames: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                peak: AtomicU32::new(0),
                columns: Mutex::new(Vec::new()),
                failed: Mutex::new(None),
            });
            feeds.push(Feed { left, right, samples: samples_in, gaps: gaps_in, offered: 0, without_room: None });
            written.push(Written {
                growing,
                file: Some(file),
                samples: samples_out,
                gaps: gaps_out,
                waiting: VecDeque::with_capacity(GAPS),
                done: 0,
                column: ([f32::MAX, f32::MIN], 0),
                chunk: Vec::with_capacity(CHUNK_FRAMES * 2),
            });
        }
        let files: Vec<Arc<Growing>> = written.iter().map(|written| written.growing.clone()).collect();
        let finish = Arc::new(AtomicBool::new(false));
        let done = finish.clone();
        let thread = std::thread::Builder::new()
            .name("take-writer".into())
            .spawn(move || {
                loop {
                    // Read before the rings are emptied: what was handed
                    // over before it was set is then all written.
                    let last = done.load(Ordering::Acquire);
                    for file in &mut written {
                        file.drain();
                    }
                    if last {
                        break;
                    }
                    std::thread::sleep(LOOK_EVERY);
                }
                for file in written {
                    file.finish();
                }
            })
            .map_err(|error| format!("cannot start a thread to write the take: {error}"))?;
        let feeds = Feeds { feeds, block: vec![0.0; MAX_BLOCK * 2], handed: 0, tail };
        Ok((Self { thread: Some(thread), finish, files }, Box::new(feeds)))
    }

    /// The files as they stand.
    pub fn files(&self) -> &[Arc<Growing>] {
        &self.files
    }

    /// Nothing more is handed over: what was is written, the files are
    /// ended, and this returns when they are.
    pub fn finish(mut self) -> Vec<Arc<Growing>> {
        self.finish.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        std::mem::take(&mut self.files)
    }
}

impl Drop for TakeWriter {
    fn drop(&mut self) {
        // A take that is let go of is written all the same: a recording
        // is nobody's to throw away.
        self.finish.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> Vec<f32> {
        let buffer = trill_media::decode(path).unwrap();
        buffer.left
    }

    #[test]
    fn frames_that_find_no_room_are_silence_where_they_were_and_what_follows_is_in_its_place() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("take.wav");
        let files = vec![FileOf { track: TrackId(1), path: path.clone(), left: 0, right: None }];
        // No thread: the writer's end is emptied by hand, when the test
        // says, so that the ring fills.
        let (samples_in, samples_out) = rtrb::RingBuffer::new(1000);
        let (gaps_in, gaps_out) = rtrb::RingBuffer::new(GAPS);
        drop(files);
        let growing = Arc::new(Growing {
            path: path.clone(),
            track: TrackId(1),
            channels: 1,
            sample_rate: 48_000.0,
            frames: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            columns: Mutex::new(Vec::new()),
            failed: Mutex::new(None),
        });
        let mut feed =
            Feed { left: 0, right: None, samples: samples_in, gaps: gaps_in, offered: 0, without_room: None };
        let mut written = Written {
            growing: growing.clone(),
            file: Some(GrowingWav::create(&path, 48_000.0, 1).unwrap()),
            samples: samples_out,
            gaps: gaps_out,
            waiting: VecDeque::new(),
            done: 0,
            column: ([f32::MAX, f32::MIN], 0),
            chunk: Vec::new(),
        };
        // A ramp that says which frame it is, a hundred at a time. The
        // ring holds a thousand: frames 1000 to 1499 find no room.
        let block = |from: usize| -> Vec<f32> { (from..from + 100).map(|n| n as f32 / 10_000.0).collect() };
        for from in (0..1500).step_by(100) {
            feed.offer(&block(from), 100);
        }
        written.drain();
        for from in (1500..3000).step_by(100) {
            feed.offer(&block(from), 100);
            written.drain();
        }
        written.finish();
        let audio = read(&path);
        assert_eq!(audio.len(), 3000);
        assert_eq!(growing.dropped(), 500);
        for frame in [0usize, 999, 1500, 2999] {
            assert_eq!(audio[frame], frame as f32 / 10_000.0, "frame {frame}");
        }
        assert!(audio[1000..1500].iter().all(|sample| *sample == 0.0), "silence of the right length");
        assert_eq!(growing.frames(), 3000);
        assert_eq!(growing.columns(0).len(), 12, "eleven whole columns and what was left");
        assert!((growing.peak() - 0.2999).abs() < 1e-6);
    }

    #[test]
    fn a_take_is_written_by_a_thread_of_its_own_as_one_channel_or_a_pair() {
        let folder = tempfile::tempdir().unwrap();
        let files = vec![
            FileOf { track: TrackId(1), path: folder.path().join("one.wav"), left: 1, right: None },
            FileOf { track: TrackId(2), path: folder.path().join("pair.wav"), left: 0, right: Some(1) },
        ];
        let (writer, mut feeds) = TakeWriter::start(files, 48_000.0, 0, Limits::default()).unwrap();
        assert_eq!(feeds.channels(), 0b11);
        // Without an input there is silence, of the right length.
        for _ in 0..100 {
            feeds.push(None, 480);
        }
        assert_eq!(feeds.handed, 48_000);
        let files = writer.finish();
        assert_eq!(
            files.iter().map(|file| (file.frames(), file.channels())).collect::<Vec<_>>(),
            [(48_000, 1), (48_000, 2)]
        );
        assert!(files.iter().all(|file| file.failed().is_none() && file.dropped() == 0));
        assert_eq!(read(files[1].path()).len(), 48_000);
        // A file that cannot be begun is a sentence.
        let nowhere =
            vec![FileOf { track: TrackId(1), path: folder.path().join("one.wav/x.wav"), left: 0, right: None }];
        assert!(TakeWriter::start(nowhere, 48_000.0, 0, Limits::default()).is_err());
    }
}
