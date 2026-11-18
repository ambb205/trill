//! A device that takes time to make comes after the song has it.
//!
//! Reading a song's file takes milliseconds, and so does an edit. Making
//! a plugin takes as long as it takes to load its file, take its state on
//! and read what it plays: a second or many, on the one thread plugins are
//! made on. A session that made plugins where it stood kept everyone who
//! wanted the song waiting for them: the window, which had nothing to
//! show or did not answer; a model; its own chores.
//!
//! So whenever the song comes to have a device that takes time
//! ([`DeviceFactory::takes_time`]), the song is compiled with that device
//! left out and its place kept ([`crate::compile::DeviceMeta::coming`]):
//! when a song is opened or begun, when an edit, an undo or a redo brings
//! a device, when a device is given another state, and when the audio
//! stream comes back at another rate and every device has to be made
//! anew. A thread of the session's own makes the devices one by one, in
//! the order they were asked for, with nobody waiting; each is put in its
//! place by the session's chores when it has arrived
//! ([`Session::housekeeping`]). That is no edit: the song is not changed
//! by it, and its history and its journal know nothing of it.
//!
//! **Until a device is there**, its place is empty: an instrument is
//! silent, an effect lets through what comes into it. (Where a device is
//! made anew in a song that plays, what was in its place goes on until
//! its successor is there.) It keeps what the song says of it, which is
//! what a save, a safety copy and a render take; [`Session::loading`]
//! names it, and [`Session::arriving`] says how far the whole is. What
//! cannot be asked of it yet is refused in a sentence
//! ([`Session::there`]). A preset of its own that it is given meanwhile
//! is in the song as what it is to start from
//! ([`trill_core::Device::from_preset`]), and the thread that makes it
//! has it load that. The state the device comes to with it is taken into
//! the song when any plugin's is (a save, a safety copy, a render), and
//! not as it arrives: some plugins take a preset on with their first
//! blocks, and say what they were born with until then.
//!
//! **A device is the one it was made as, or it is not put in.** What
//! arrives is held against the song as it is by then: a device that was
//! removed meanwhile, or that the song has in another state than the one
//! it was made from, is let go of (and made again, where it is still
//! wanted). One for a song that is no longer the open one never arrives
//! at all: the thread looks at which song is open before and after each
//! making.
//!
//! **What is let go of is taken down by the same thread**, after what
//! there is to make: the plugins of a song that was open before, of a
//! track that was deleted, of a stream at another rate. Each is a wait on
//! the thread plugins are made on, and nobody waits for it here.
//!
//! **Nobody asks a plugin anything while one is being made**, where the
//! answer can wait: a question goes to the same thread and would be
//! answered when the making is done. A device's values are those it last
//! reported ([`Session::making_devices`]). What cannot wait (the state of
//! every plugin, for a save) holds the thread back from beginning another
//! making until it is answered ([`Session::hold_makings`]).

use super::{Session, SessionError};
use crate::compile::DeviceMeta;
use crate::engine::Message;
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

/// How far the song is with the devices that are being made for it: see
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

/// What making one device took: see [`Session::made_since_opening`].
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub device: DeviceId,
    pub name: String,
    pub uri: String,
    /// How long its making took, from when its turn came.
    pub took: Duration,
    /// How long after the first of them was asked for it was in its place.
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
    /// What it says its values are, asked by the thread that made it
    /// while it had the time.
    reported: Option<Vec<f64>>,
}

/// Something to take down, away from whoever is using the song: a graph
/// the audio thread retired, with the devices that went with it; the
/// records of devices that left the song; a device that was made for a
/// song that is gone.
type Gone = Box<dyn Send>;

#[derive(Default)]
struct Queue {
    makes: VecDeque<Make>,
    /// In the order they were let go of: a device's record, with its
    /// window, ahead of the device itself.
    gone: VecDeque<Gone>,
    quit: bool,
    /// Whether the thread has something in hand.
    working: bool,
    /// How many are asking a plugin something that cannot wait: the
    /// thread begins nothing while any is.
    held: usize,
}

/// What the session and the thread that makes devices have between them.
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Counts the songs opened, and the times every device had to be made
    /// anew. A making for another number than this is one for a song that
    /// is gone.
    song: AtomicU64,
    /// The device that is being made (its id), or 0.
    now: AtomicU64,
}

impl Shared {
    fn let_go_of(&self, gone: Gone) {
        self.queue.lock().gone.push_back(gone);
        self.wake.notify_all();
    }

    /// Whether the thread has anything in hand or waiting for it: a making
    /// or a taking down.
    fn busy(&self) -> bool {
        let queue = self.queue.lock();
        queue.working || !queue.makes.is_empty() || !queue.gone.is_empty()
    }
}

/// Keeps the thread from beginning anything for as long as it lives: see
/// [`Session::hold_makings`].
pub(super) struct Held(Arc<Shared>);

impl Drop for Held {
    fn drop(&mut self) {
        let mut queue = self.0.queue.lock();
        queue.held = queue.held.saturating_sub(1);
        drop(queue);
        self.0.wake.notify_all();
    }
}

/// What the thread takes up next.
enum Job {
    Make(Box<Make>),
    Gone(Gone),
}

/// What the thread that makes devices does until it is told to go.
fn work(shared: &Shared, factory: &dyn DeviceFactory, arrivals: &Sender<Arrival>) {
    loop {
        let job = {
            let mut queue = shared.queue.lock();
            queue.working = false;
            loop {
                // Nothing is begun while someone is asking a plugin what
                // cannot wait; at the end, what is left is taken down
                // whoever asks.
                if queue.held == 0 || queue.quit {
                    // What there is to make comes before what there is to
                    // take down: someone is waiting to hear it.
                    if let Some(make) = queue.makes.pop_front() {
                        queue.working = true;
                        break Job::Make(Box::new(make));
                    }
                    if let Some(gone) = queue.gone.pop_front() {
                        queue.working = true;
                        break Job::Gone(gone);
                    }
                }
                if queue.quit {
                    return;
                }
                shared.wake.wait(&mut queue);
            }
        };
        let make = match job {
            Job::Make(make) => *make,
            // (A plugin is taken down on its own thread, and that is
            // waited for here, by nobody else.)
            Job::Gone(gone) => {
                drop(gone);
                continue;
            }
        };
        let open = || shared.song.load(Ordering::Acquire);
        if make.song != open() {
            continue;
        }
        shared.now.store(make.device.id.0, Ordering::Release);
        let began = Instant::now();
        let mut reported = None;
        let create = || {
            let mut instance = crate::compile::make(factory, &make.device, make.sample_rate, Purpose::Playing)?;
            // Asked here, by the thread that has the time for it: what
            // its values are.
            if let Some(controller) = instance.controller.as_mut() {
                reported = controller.param_values();
            }
            Ok(instance)
        };
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
        let (song, sample_rate) = (make.song, make.sample_rate);
        let arrival = Arrival { song, from: make.device, sample_rate, made, took, reported };
        if let Err(unsent) = arrivals.send(arrival) {
            // Nobody is there to take it: the session has gone.
            drop(unsent);
        }
    }
}

/// What a session keeps of the devices that are being made for its song:
/// see the module.
pub(super) struct Opening {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    arrivals: Receiver<Arrival>,
    /// The other end, for the thread that is started when there is first
    /// something for it to do.
    sender: Sender<Arrival>,
    /// The devices that are still to come, in the order they are made.
    coming: Vec<DeviceId>,
    /// When the first of them was asked for, and what has arrived since.
    began: Instant,
    made: Vec<Made>,
    /// See [`Session::open_all_at_once`].
    pub(super) all_at_once: bool,
    /// No thread could be started: devices are made where the song is.
    no_thread: bool,
    /// Whether the thread had anything to do when the session last looked.
    was_busy: bool,
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
            no_thread: false,
            was_busy: false,
        }
    }

    /// What takes the graphs the audio thread retires: one that holds a
    /// device which takes time to take down goes to the thread that makes
    /// devices, any other is freed where it is collected, as it always
    /// was.
    pub(super) fn retiring(&self) -> Box<dyn Fn(Box<crate::graph::Graph>) + Send> {
        let shared = self.shared.clone();
        Box::new(move |graph| match graph.holds_slow() {
            true => shared.let_go_of(graph),
            false => drop(graph),
        })
    }

    /// Starts the thread, if it is not running and there is something for
    /// it to do. Returns whether there is a thread.
    fn tend_thread(&mut self, factory: &Arc<dyn DeviceFactory>) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let idle = {
            let queue = self.shared.queue.lock();
            queue.makes.is_empty() && queue.gone.is_empty()
        };
        if idle || self.no_thread {
            return false;
        }
        let (shared, factory, sender) = (self.shared.clone(), factory.clone(), self.sender.clone());
        let work = move || work(&shared, factory.as_ref(), &sender);
        self.worker = std::thread::Builder::new().name("trill-devices".into()).spawn(work).ok();
        self.no_thread = self.worker.is_none();
        self.worker.is_some()
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
    /// How far the song is with the devices that are being made for it:
    /// how many are there of how many, and which is being made. `None`
    /// when none is on its way, which is nearly always. Never waits; a
    /// window asks it with every frame.
    pub fn arriving(&self) -> Option<Arriving> {
        let coming = self.opening.coming.clone();
        if coming.is_empty() {
            return None;
        }
        let of = self.project.devices().count();
        let now = DeviceId(self.opening.shared.now.load(Ordering::Acquire));
        Some(Arriving { made: of.saturating_sub(coming.len()), of, now: coming.contains(&now).then_some(now), coming })
    }

    /// What making each device took that was made since devices last began
    /// to be made for the song (it was opened, or an edit brought one), in
    /// the order they arrived: for whoever measures.
    pub fn made_since_opening(&self) -> &[Made] {
        &self.opening.made
    }

    /// Has every device made where the song stands, before whatever
    /// brought it comes back, as it was before devices came after; or,
    /// with `false`, as it is now. For measuring the one against the
    /// other.
    pub fn open_all_at_once(&mut self, all_at_once: bool) {
        self.opening.all_at_once = all_at_once;
    }

    /// Waits until every device that is being made for the song has
    /// arrived, or `timeout` has passed, and returns whether all have.
    /// For tests and tools: it blocks whoever holds the session, which a
    /// front end must not do. Those leave it to [`Session::housekeeping`]
    /// and watch [`Session::arriving`].
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

    /// Whether a device is being made or taken down right now, or about
    /// to be: the thread plugins are made on is not free, and whatever is
    /// asked of a plugin waits for it. So what can wait is not asked (the
    /// values of a device's parameters are those it last reported), and a
    /// safety copy is put off until it is over.
    pub fn making_devices(&self) -> bool {
        self.opening.shared.busy()
    }

    /// Keeps the thread that makes devices from beginning another making
    /// or taking down until what is returned is let go of: for asking
    /// plugins what cannot wait (their states, for a save), so that it is
    /// answered after the one making that may be under way, and not after
    /// one for every question.
    pub(super) fn hold_makings(&self) -> Held {
        self.opening.shared.queue.lock().held += 1;
        Held(self.opening.shared.clone())
    }

    /// Whether a device of the type `uri` that the song comes to have is
    /// made after it, away from the session.
    pub(super) fn comes_later(&self, uri: &str) -> bool {
        self.comes_later_at_all() && self.factory.takes_time(uri)
    }

    /// Whether devices that take time are made after the song has them:
    /// unless that was switched off to be measured, or no thread is to be
    /// had for it.
    pub(super) fn comes_later_at_all(&self) -> bool {
        !self.opening.all_at_once && !self.opening.no_thread
    }

    /// Refuses, in a sentence, what is asked of a device that is still
    /// being made and cannot be done until it is there.
    pub(super) fn there(&self, device: DeviceId) -> Result<(), SessionError> {
        if !self.is_coming(device) {
            return Ok(());
        }
        let name = self.project.device(device).map_or_else(|_| device.to_string(), |device| device.name.clone());
        Err(SessionError::Device(format!(
            "{name} is still being made: the song has it, and plugins are made one at a time, away from the song. \
             Ask again when it is there; until then it is listed as loading."
        )))
    }

    /// Frees something that may have a plugin behind it away from the
    /// session: by the thread that makes devices, after what it has to
    /// make.
    pub(super) fn let_go_of(&mut self, gone: impl Send + 'static) {
        self.opening.shared.let_go_of(Box::new(gone));
        self.opening.tend_thread(&self.factory);
    }

    /// Hands the records of devices that the last compiles let go of to
    /// the thread that takes such things down.
    pub(super) fn let_go_of_records(&mut self) {
        let gone: Vec<DeviceMeta> = std::mem::take(&mut self.bank.gone);
        if !gone.is_empty() {
            self.let_go_of(gone);
        }
    }

    /// The song that was open is gone, or every device is to be made
    /// anew: what is being made is let go of, and nothing of it arrives.
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

    /// Has every device made whose place the compile that just ran kept,
    /// and that is not being made already: by the session's own thread,
    /// in the order of the song.
    pub(super) fn ask_for_what_is_coming(&mut self) {
        // A device that is no longer waited for (the edit that was just
        // compiled removed it) is no longer counted.
        let still: Vec<DeviceId> = self.opening.coming.iter().copied().filter(|id| self.is_coming(*id)).collect();
        self.opening.coming = still;
        let waited = &self.opening.coming;
        let asked: Vec<Device> = (self.project.devices())
            .filter(|device| self.is_coming(device.id) && !waited.contains(&device.id))
            .cloned()
            .collect();
        let (song, sample_rate) = (self.opening.shared.song.load(Ordering::Acquire), self.engine.sample_rate());
        {
            // One that is waiting for its turn is made as the song has it
            // now, which an edit since may have changed.
            let mut queue = self.opening.shared.queue.lock();
            // And one that is no longer waited for is not made at all.
            queue.makes.retain(|make| make.song != song || self.is_coming(make.device.id));
            for make in queue.makes.iter_mut().filter(|make| make.song == song) {
                if let Ok(device) = self.project.device(make.device.id) {
                    make.device = device.clone();
                }
            }
        }
        if asked.is_empty() {
            return;
        }
        if self.opening.coming.is_empty() {
            // The first of a new lot: what is measured begins here.
            self.opening.began = Instant::now();
            self.opening.made.clear();
        }
        self.opening.coming.extend(asked.iter().map(|device| device.id));
        let mut queue = self.opening.shared.queue.lock();
        queue.makes.extend(asked.into_iter().map(|device| Make { song, device, sample_rate }));
        drop(queue);
        self.opening.shared.wake.notify_all();
        if !self.opening.tend_thread(&self.factory) {
            // No thread to be had: they are made here after all, now and
            // from now on.
            log::warn!("no thread could be started to make the song's devices; they are made where the song is");
            self.opening.shared.queue.lock().makes.clear();
            self.opening.coming.clear();
            self.bank.devices.retain(|_, meta| !meta.coming);
            self.compile_and_swap();
        }
    }

    /// Puts the devices that have arrived in their places, and sees to the
    /// thread that makes them. One of the session's chores; cheap when
    /// nothing has arrived.
    pub(super) fn tend_opening(&mut self) {
        self.let_go_of_records();
        self.opening.tend_thread(&self.factory);
        let mut landed: Vec<Arrival> = Vec::new();
        while let Ok(arrival) = self.opening.arrivals.try_recv() {
            let id = arrival.from.id;
            let open = self.opening.shared.song.load(Ordering::Acquire);
            // Still the device it was made as, in the song it was made
            // for, at the rate the engine runs at?
            let kept = arrival.song == open && self.is_coming(id) && arrival.sample_rate == self.engine.sample_rate();
            let from = &arrival.from;
            let same = |device: &Device| {
                device.uri == from.uri
                    && device.state == from.state
                    && device.from_preset == from.from_preset
                    && device.sidechain == from.sidechain
            };
            match self.project.device(id) {
                Ok(device) if kept && same(device) => landed.push(arrival),
                Ok(device) if kept => {
                    // The song has it in another state than the one it
                    // was made from: made again, as it is now.
                    let make = Make { song: open, device: device.clone(), sample_rate: self.engine.sample_rate() };
                    self.opening.shared.let_go_of(Box::new(arrival.made));
                    self.opening.shared.queue.lock().makes.push_back(make);
                    self.opening.shared.wake.notify_all();
                }
                _ => self.opening.shared.let_go_of(Box::new(arrival.made)),
            }
        }
        // A device that is no longer waited for (it was removed, or the
        // song made it anew where it stood) is no longer counted.
        let waited: Vec<DeviceId> = self.opening.coming.clone();
        self.opening.coming = waited.into_iter().filter(|id| self.is_coming(*id)).collect();
        // When the thread has nothing more to do, plugins can be asked
        // again: whoever shows their values looks once more.
        let busy = self.making_devices();
        let now_free = std::mem::replace(&mut self.opening.was_busy, busy) && !busy;
        if landed.is_empty() {
            if now_free {
                self.say_that_more_is_there();
            }
            return;
        }
        let mut news = Vec::new();
        for arrival in landed {
            let Arrival { from, made, took, reported, .. } = arrival;
            let failed = made.as_ref().err().map(ToString::to_string);
            // It is there: one that was asked for a second time meanwhile
            // (taken back and brought again while it was being made) is
            // not made a second time.
            self.opening.shared.queue.lock().makes.retain(|make| make.device.id != from.id);
            self.bank.arrived.insert(from.id, made);
            news.push((from, took, failed, reported));
        }
        self.compile_and_swap();
        let there_after = self.opening.began.elapsed();
        for (from, took, failed, reported) in news {
            let id = from.id;
            if let Some(meta) = self.bank.devices.get(&id) {
                *meta.reported.lock() = reported;
            }
            // What was set on it while it was on its way is told to it now.
            if let (Some(meta), Ok(device)) = (self.bank.devices.get(&id), self.project.device(id)) {
                for (index, info) in meta.params.iter().enumerate() {
                    let value = device.params.get(&info.id);
                    if value != from.params.get(&info.id) {
                        let value = value.copied().unwrap_or(info.default);
                        self.engine.send(Message::Param { device: id, index, value });
                    }
                }
            }
            self.opening.coming.retain(|coming| *coming != id);
            self.opening.made.push(Made { device: id, name: from.name, uri: from.uri, took, there_after, failed });
        }
        self.say_that_more_is_there();
    }

    /// Whoever draws the song looks again: there is more of it, or more is
    /// known of it. It is no more changed than it was.
    fn say_that_more_is_there(&mut self) {
        let modified = self.is_modified();
        self.revision += 1;
        if !modified {
            self.saved_revision = self.revision;
        }
    }
}
