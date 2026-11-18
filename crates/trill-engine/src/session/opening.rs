//! A song that is opened is there at once, and its devices come after.
//!
//! Reading a song's file takes milliseconds. Making its plugins takes as
//! long as each of them takes to load its file, take its state on and
//! read what it plays: a second or many apiece, one after the other on
//! the one thread plugins are made on. A session that made them where it
//! stood kept everyone who wanted the song waiting for all of that: the
//! window, which had nothing to show; a model; its own chores.
//!
//! So a song that is opened is compiled with the devices that take time
//! ([`DeviceFactory::takes_time`]) left out, their places kept
//! ([`crate::compile::DeviceMeta::coming`]): its tracks, clips and mixer
//! are there, and everything that is made in no time plays. The others are
//! made by a thread of the session's own, one by one in the order of the
//! song, with nobody waiting; each is put in its place by the session's
//! chores when it has arrived ([`Session::housekeeping`]), as an edit to
//! nothing: the song is not changed, its history and its journal know
//! nothing of it.
//!
//! **Until a device is there**, its place is empty: an instrument is
//! silent, an effect lets through what comes into it. It keeps the state
//! the file gave it, which is what a save, a safety copy and a render
//! take; [`Session::loading`] names it, and [`Session::arriving`] says how
//! far the whole is.
//!
//! **A device is the one it was made as, or it is not put in.** What
//! arrives is held against the song as it is by then: a device that was
//! removed meanwhile, or made anew because the song asked for another
//! state of it, is let go of. One for a song that is no longer the open
//! one never arrives at all: the thread looks at which song is open before
//! and after each making.
//!
//! **The new song first, the taking down after.** The plugins of the song
//! that was open before have to be taken down on the same thread their
//! successors are made on, each a wait. What is let go of while a song is
//! being opened is handed to the thread that makes devices, which takes
//! it down when it has nothing left to make, and with nobody waiting.

use super::{Session, SessionError};
use crate::engine::Message;
use crate::graph::MAX_BLOCK;
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use trill_core::device::{DeviceError, DeviceFactory, DeviceInstance, Purpose};
use trill_core::{Device, DeviceId, Project};

/// The log target under which what a making took is said: the plugin
/// host's own lines are under the same.
const MAKING: &str = "trill::making";

/// How far a song that was opened is with its devices: see
/// [`Session::arriving`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arriving {
    /// How many of the song's devices are there (or could not be made), of
    /// how many it has.
    pub made: usize,
    pub of: usize,
    /// The device that is being made right now, if one is.
    pub now: Option<DeviceId>,
    /// The devices that are not there yet, in the order they are made.
    pub coming: Vec<DeviceId>,
}

impl Arriving {
    /// In a line: `12 of 38 devices: Archetype Tim Henson X on Lead`.
    pub fn words(&self, project: &Project) -> String {
        let counted = format!("{} of {} devices", self.made, self.of);
        let Some(device) = self.now.and_then(|id| project.device(id).ok()) else { return counted };
        let on = project.tracks.iter().find(|track| track.devices.iter().any(|other| other.id == device.id));
        match on {
            Some(track) => format!("{counted}: {} on {}", device.name, track.name),
            None => format!("{counted}: {} on the master", device.name),
        }
    }
}

/// What making one device of a song that was opened took: see
/// [`Session::made_since_opening`].
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub device: DeviceId,
    pub name: String,
    pub uri: String,
    /// How long its making took, from when its turn came.
    pub took: Duration,
    /// How long after the song was opened it was in its place.
    pub there_after: Duration,
    /// Why it could not be made, if it could not.
    pub failed: Option<String>,
}

/// A device to make.
struct Make {
    song: u64,
    device: Device,
    sample_rate: f64,
}

/// A device that was made, or could not be.
struct Arrival {
    song: u64,
    from: Device,
    sample_rate: f64,
    made: Result<DeviceInstance, DeviceError>,
    took: Duration,
}

/// Something to take down, away from whoever is using the song: a graph
/// the audio thread retired, with the devices that went with it, or a
/// device that was made for a song that is gone.
type Gone = Box<dyn Send>;

#[derive(Default)]
struct Queue {
    makes: VecDeque<Make>,
    gone: Vec<Gone>,
    quit: bool,
}

/// What the session and the thread that makes devices have between them.
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Counts the songs opened. A making for another number than this is
    /// one for a song that is gone.
    song: AtomicU64,
    /// The device that is being made (its id), or 0.
    now: AtomicU64,
    /// See [`Session::take_down_before_making`].
    down_first: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn let_go_of(&self, gone: Gone) {
        self.queue.lock().gone.push(gone);
        self.wake.notify_one();
    }
}

/// What the thread that makes devices does until it is told to go.
fn work(shared: &Shared, factory: &dyn DeviceFactory, arrivals: &Sender<Arrival>) {
    loop {
        let make = {
            let mut queue = shared.queue.lock();
            loop {
                // What there is to make comes before what there is to take
                // down: the song that was opened is waited for by someone.
                let down_first = shared.down_first.load(Ordering::Relaxed) && !queue.gone.is_empty();
                if !down_first && let Some(make) = queue.makes.pop_front() {
                    break Some(make);
                }
                if let Some(gone) = queue.gone.pop() {
                    drop(queue);
                    // (A plugin is taken down on its own thread, and that
                    // is waited for here, by nobody else.)
                    drop(gone);
                    break None;
                }
                if queue.quit {
                    return;
                }
                shared.wake.wait(&mut queue);
            }
        };
        let Some(make) = make else { continue };
        let open = || shared.song.load(Ordering::Acquire);
        if make.song != open() {
            continue;
        }
        shared.now.store(make.device.id.0, Ordering::Release);
        let began = Instant::now();
        let create = || factory.create_for(&make.device, make.sample_rate, MAX_BLOCK, Purpose::Playing);
        // A factory that goes down over a device has failed to make it,
        // and the others are still to be made.
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(create)).unwrap_or_else(|_| {
            let reason = "the host failed unexpectedly while making it".to_string();
            Err(DeviceError::LoadFailed { uri: make.device.uri.clone(), reason })
        });
        let took = began.elapsed();
        log::debug!(
            target: MAKING,
            "{} ({}): made in {:.0} ms, away from the song",
            make.device.name,
            make.device.uri,
            took.as_secs_f64() * 1e3
        );
        shared.now.store(0, Ordering::Release);
        if make.song != open() {
            // The song it was for is gone: it is taken down in its turn.
            shared.let_go_of(Box::new(made));
            continue;
        }
        let arrival = Arrival { song: make.song, from: make.device, sample_rate: make.sample_rate, made, took };
        if let Err(unsent) = arrivals.send(arrival) {
            // Nobody is there to take it: the session has gone.
            drop(unsent);
        }
    }
}

/// What a session keeps of the song it opened last: see the module.
pub(super) struct Opening {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    arrivals: Receiver<Arrival>,
    /// The other end, for the thread that is started when there is first
    /// something to make.
    sender: Sender<Arrival>,
    /// The devices that are still to come, in the order they are made.
    coming: Vec<DeviceId>,
    /// When the song was opened, and what has arrived of it since.
    began: Instant,
    made: Vec<Made>,
    /// See [`Session::open_all_at_once`].
    pub(super) all_at_once: bool,
}

impl Opening {
    pub(super) fn new() -> Self {
        let (sender, arrivals) = channel();
        Self {
            shared: Arc::default(),
            worker: None,
            arrivals,
            sender,
            coming: Vec::new(),
            began: Instant::now(),
            made: Vec::new(),
            all_at_once: false,
        }
    }
}

impl Drop for Opening {
    /// Lets go of what was being made and waits for the thread: for the
    /// one device it may be in the middle of, and for everything that was
    /// handed to it to be taken down. A program does not end in the middle
    /// of a plugin.
    fn drop(&mut self) {
        self.shared.song.fetch_add(1, Ordering::AcqRel);
        {
            let mut queue = self.shared.queue.lock();
            queue.makes.clear();
            queue.quit = true;
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Session {
    /// How far the song that was opened is with its devices: how many are
    /// there of how many, and which is being made. `None` when none is on
    /// its way, which is nearly always. Never waits; a window asks it with
    /// every frame.
    pub fn arriving(&self) -> Option<Arriving> {
        let coming = self.opening.coming.clone();
        if coming.is_empty() {
            return None;
        }
        let of = self.project.devices().count();
        let now = DeviceId(self.opening.shared.now.load(Ordering::Acquire));
        Some(Arriving { made: of.saturating_sub(coming.len()), of, now: coming.contains(&now).then_some(now), coming })
    }

    /// What making each device took that was made after the song was
    /// opened, in the order they arrived: for whoever measures.
    pub fn made_since_opening(&self) -> &[Made] {
        &self.opening.made
    }

    /// Has a song that is opened made whole where it is opened, every
    /// device before `load` comes back, as it was before devices came
    /// after; or, with `false`, as it is now. For measuring the one
    /// against the other.
    pub fn open_all_at_once(&mut self, all_at_once: bool) {
        self.opening.all_at_once = all_at_once;
    }

    /// Has what the song before leaves behind taken down ahead of the
    /// devices of the song that is opened, in place of after them; or,
    /// with `false`, as it is. For measuring the one against the other:
    /// the first keeps less in memory at once, the second has the new
    /// song's devices sooner.
    pub fn take_down_before_making(&mut self, first: bool) {
        self.opening.shared.down_first.store(first, Ordering::Relaxed);
    }

    /// Waits until every device of the song that was opened has arrived,
    /// or `timeout` has passed, and returns whether all have. For tests
    /// and tools: it blocks whoever holds the session, which a front end
    /// must not do. Those leave it to [`Session::housekeeping`] and watch
    /// [`Session::arriving`].
    pub fn wait_until_opened(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.tend_opening();
            if self.opening.coming.is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether `device` is one whose place is kept while it is being made.
    pub fn is_coming(&self, device: DeviceId) -> bool {
        self.bank.devices.get(&device).is_some_and(|meta| meta.coming)
    }

    /// Refuses, in a sentence, what is asked of a device that is still
    /// being made and cannot be done until it is there.
    pub(super) fn there(&self, device: DeviceId) -> Result<(), SessionError> {
        if !self.is_coming(device) {
            return Ok(());
        }
        let name = self.project.device(device).map_or_else(|_| device.to_string(), |device| device.name.clone());
        Err(SessionError::Device(format!(
            "{name} is still being made: the song was opened a moment ago, and its devices arrive one by one. Ask \
             again when it is there; until then it is listed as loading."
        )))
    }

    /// The song that was open is gone, or its devices are to be made anew
    /// where the session stands: what is being made for it is let go of,
    /// and nothing of it arrives.
    pub(super) fn forget_opening(&mut self) {
        let opening = &mut self.opening;
        opening.shared.song.fetch_add(1, Ordering::AcqRel);
        opening.shared.queue.lock().makes.clear();
        // What had arrived and was not taken yet goes too, in its turn.
        while let Ok(arrival) = opening.arrivals.try_recv() {
            opening.shared.let_go_of(Box::new(arrival.made));
        }
        opening.coming.clear();
        opening.made.clear();
        opening.began = Instant::now();
    }

    /// Has every device made whose place the compile that just ran kept:
    /// by the session's own thread, in the order of the song.
    pub(super) fn ask_for_what_is_coming(&mut self) {
        let coming: Vec<Device> = self.project.devices().filter(|device| self.is_coming(device.id)).cloned().collect();
        if coming.is_empty() {
            return;
        }
        let opening = &mut self.opening;
        if opening.worker.is_none() {
            let (shared, factory, sender) = (opening.shared.clone(), self.factory.clone(), opening.sender.clone());
            let work = move || work(&shared, factory.as_ref(), &sender);
            opening.worker = std::thread::Builder::new().name("trill-devices".into()).spawn(work).ok();
            // From here on what the audio thread retires is taken down by
            // that thread, behind what it has to make.
            let shared = opening.shared.clone();
            self.engine.retire_through(Box::new(move |graph| shared.let_go_of(graph)));
        }
        if opening.worker.is_none() {
            // No thread to be had: they are made here after all.
            log::warn!("no thread could be started to make the song's devices; they are made where the song is");
            self.bank.devices.retain(|_, meta| !meta.coming);
            self.compile_and_swap();
            return;
        }
        let (song, sample_rate) = (opening.shared.song.load(Ordering::Acquire), self.engine.sample_rate());
        opening.coming = coming.iter().map(|device| device.id).collect();
        let mut queue = opening.shared.queue.lock();
        queue.makes.extend(coming.into_iter().map(|device| Make { song, device, sample_rate }));
        drop(queue);
        opening.shared.wake.notify_one();
    }

    /// Puts the devices that have arrived in their places. One of the
    /// session's chores; cheap when nothing has.
    pub(super) fn tend_opening(&mut self) {
        let mut landed: Vec<(Device, Duration, Option<String>)> = Vec::new();
        while let Ok(arrival) = self.opening.arrivals.try_recv() {
            let Arrival { song, from, sample_rate, made, took } = arrival;
            let id = from.id;
            let open = self.opening.shared.song.load(Ordering::Acquire);
            // Still the device it was made as, in the song it was made
            // for, at the rate the engine runs at?
            let kept = song == open && self.is_coming(id) && sample_rate == self.engine.sample_rate();
            let same = |device: &Device| {
                device.uri == from.uri && device.state == from.state && device.sidechain == from.sidechain
            };
            match self.project.device(id) {
                Ok(device) if kept && same(device) => {
                    let failed = made.as_ref().err().map(ToString::to_string);
                    self.bank.arrived.insert(id, made);
                    landed.push((from, took, failed));
                }
                Ok(device) if kept => {
                    // The song asks for another state of it than the one
                    // it was made from: made again, as it is now.
                    let make = Make { song, device: device.clone(), sample_rate: self.engine.sample_rate() };
                    self.opening.shared.let_go_of(Box::new(made));
                    self.opening.shared.queue.lock().makes.push_back(make);
                    self.opening.shared.wake.notify_one();
                }
                _ => self.opening.shared.let_go_of(Box::new(made)),
            }
        }
        // A device that is no longer waited for (it was removed, or the
        // song made it anew where it stood) is no longer counted.
        let waited: Vec<DeviceId> = self.opening.coming.clone();
        self.opening.coming = waited.into_iter().filter(|id| self.is_coming(*id)).collect();
        if landed.is_empty() {
            return;
        }
        self.compile_and_swap();
        let there_after = self.opening.began.elapsed();
        for (from, took, failed) in landed {
            // What was set on it while it was on its way is told to it now.
            if let (Some(meta), Ok(device)) = (self.bank.devices.get(&from.id), self.project.device(from.id)) {
                for (index, info) in meta.params.iter().enumerate() {
                    let value = device.params.get(&info.id);
                    if value != from.params.get(&info.id) {
                        let value = value.copied().unwrap_or(info.default);
                        self.engine.send(Message::Param { device: from.id, index, value });
                    }
                }
            }
            self.opening.coming.retain(|id| *id != from.id);
            self.opening.made.push(Made { device: from.id, name: from.name, uri: from.uri, took, there_after, failed });
        }
        // Whoever draws the song looks again: there is more of it. It is
        // no more changed than it was.
        let modified = self.is_modified();
        self.revision += 1;
        if !modified {
            self.saved_revision = self.revision;
        }
    }
}
