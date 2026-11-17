//! Plugin instances made and let go of in one process, on the plugins
//! installed here: the test that a plugin's fault in the process is
//! counted by, and that a way of containing one is told from luck by.
//!
//! Every probe, measurement and render makes its instruments afresh and
//! lets them go again, beside the ones that play the song. A plugin that
//! is not written for that takes the process down, and with a crash
//! nothing is left to assert on. So each attempt runs in a process of
//! its own (this test binary, started again for the one attempt), says
//! what it is about to do before it does it, and says where it was if it
//! dies: which module the fault was in, on which thread, and how it got
//! there. The test counts.
//!
//! ```text
//! cargo test -p trill-plugins --test lifetimes -- --include-ignored --nocapture
//! ```
//!
//! What it found (2026-11-17, Archetype Cory Wong X): an instance of the
//! amplifier that is destroyed before it has heard from the work it does
//! to set itself up, while another instance of it is alive, has that work
//! done on the destroyed object: an access violation in the plugin's
//! module, on the plugin main thread, in a message the plugin posted to
//! itself (`RtlEnterCriticalSection` on a deleted critical section, called
//! from the plugin, called from its window procedure, called from the
//! host's message loop; once, a write through memory that had been given
//! to something else since). It takes a destroyed instance and a message
//! that is still to come: on a busy machine because the plugin's work
//! takes longer than the instance lives, and on any machine when the
//! plugin thread is kept from its windows by its jobs while the instance
//! is let go of. `vst3/quirks.rs` has the numbers and what the host does
//! about it. Neither Serum 2 nor the Ample instruments were seen to do
//! anything of the kind: sixty of their instances were handed a preset and
//! dropped at once in each of four attempts.
//!
//! `TRILL_LIFETIMES_ATTEMPTS` says how many attempts each part makes (4
//! unless said). Nothing of the user's is read or written, and no window
//! opens.
//!
//! How often the plugin fails hangs on how busy the machine is: taken
//! down at once it ended 3 attempts of 4 with every processor in use, and
//! none of 8 an hour later at 39%. So an attempt keeps half the machine's
//! processors busy itself while it runs (`TRILL_LIFETIMES_BUSY` says how
//! many threads, 0 for none), and the test says how busy the machine was
//! and what it had of memory beside every count.
//!
//! An attempt loads a plugin many times over in one process, and a
//! machine that runs out of memory stops for everyone. So every attempt
//! has a number of instances that is fixed by how it is written, none is
//! started on a machine that is short of memory ([`room`]), and one that
//! comes to more memory than it may stops ([`OWN_AT_MOST`]). Never make
//! instances for as long as a clock says: the first form of the attempt
//! that keeps the plugin thread busy did, had eighty of them on a machine
//! that had become quick, and the machine had to be restarted.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory, DeviceInstance, Event, EventKind, ProcessInfo};
use trill_core::presets::{PresetList, PresetStatus};
use trill_core::{ChainRef, DeviceId, Note, Project, TimeSignature, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::probe::{ProbeNote, ProbeOptions};
use trill_engine::session::ProbeTarget;
use trill_engine::{AudioBackend, RenderOptions, Session};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

/// Set in the process that makes one attempt: which kind, and whether the
/// host is patient with plugins it knows to need it.
const ATTEMPT: &str = "TRILL_LIFETIMES_ATTEMPT";
/// How many attempts each part of the test makes.
const ATTEMPTS: &str = "TRILL_LIFETIMES_ATTEMPTS";
/// How many threads an attempt keeps busy while it runs.
const BUSY: &str = "TRILL_LIFETIMES_BUSY";
/// What an attempt prints before each thing it does, so that the last one
/// printed says where a process that died had got to.
const STEP: &str = "STEP ";
/// How many instances an attempt makes beside the one that stays.
const COPIES: usize = 12;
/// Longer than the host keeps an instance of the amplifier that it was
/// told to let go of.
const KEPT_BUSY: Duration = Duration::from_secs(20);
/// How many instances are asked for at the same moment where an attempt
/// keeps the plugin thread busy. An instance is about 27 MB, five threads
/// and 25 handles, all of which it gives back.
const CROWD: usize = 40;
/// How many instances are weighed.
const WEIGHED: usize = 4;
/// What an attempt says what the process has of memory with.
const WEIGHT: &str = "WEIGHT ";
/// How much memory of its own an attempt may come to. No attempt has more
/// than a few dozen instances at a time, by how it is written; this is for
/// the day one of them is written wrongly, or a plugin does not give back
/// what it took.
const OWN_AT_MOST: u64 = 6 << 30;

/// What the class ids of Neural DSP's plugins begin with.
const NEURAL: &str = "vst3:ABCDEF019182FAEB4E445350";
const PATIENCE: Duration = Duration::from_secs(120);

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("lifetimes").join(name)
}

/// A VST3 factory over the system's plugins and the makers' own preset
/// folders, with caches of the test's own.
fn vst3(patient: bool) -> Vst3Factory {
    let config = Vst3Config {
        scanner: Scanner::Executable(PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan"))),
        cache_path: Some(scratch("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch("vst3 presets")],
        patient_teardown: patient,
        ..Vst3Config::default()
    };
    Vst3Factory::new(config).expect("the plugin main thread starts")
}

fn megabytes(bytes: u64) -> u64 {
    bytes >> 20
}

/// Says what the attempt is about to do and what the process has of
/// memory, and ends an attempt that has more than it may.
fn step(what: &str) {
    let own = machine::own();
    eprintln!("{STEP}{what} [{} MB]", megabytes(own));
    assert!(own <= OWN_AT_MOST, "the attempt has {} MB, more than it may: it stops here", megabytes(own));
}

/// Whether the machine has room for an attempt, and what it has, in
/// words. An attempt loads a plugin many times over, and a machine that
/// runs out of memory stops for everyone: with less than a quarter of its
/// memory free, or more committed than 1.4 times its memory (on the 63 GB
/// this was written on: 16 GB and 88 GB), none is started.
fn room() -> (bool, String) {
    let Some(memory) = machine::memory() else { return (true, "memory unknown".to_string()) };
    let gigabytes = |bytes: u64| bytes as f64 / f64::from(1u32 << 30);
    let enough = memory.free >= memory.physical / 4 && memory.committed <= memory.physical / 10 * 14;
    let has = format!("{:.0} GB free, {:.0} GB committed", gigabytes(memory.free), gigabytes(memory.committed));
    (enough, has)
}

fn listed(factory: &dyn DeviceFactory, uri: &str) -> PresetList {
    let list = factory.presets_within(uri, PATIENCE).expect("a plugin of this factory");
    assert_eq!(list.status, PresetStatus::Ready, "{uri}");
    list
}

/// One instance of `uri`, made as a song makes one, from `state` if
/// there is any.
fn make(factory: &Vst3Factory, uri: &str, state: Option<Vec<u8>>) -> DeviceInstance {
    let mut project = Project::new("lifetimes");
    let id = project.add_device(ChainRef::Master, uri, "plugin", None).unwrap();
    project.device_mut(id).unwrap().state = state;
    factory.create(project.device(id).unwrap(), 48_000.0, 512).unwrap_or_else(|error| panic!("{uri}: {error}"))
}

/// Runs `blocks` blocks of silence, with a note in the first, through an
/// instance.
fn run(instance: &mut DeviceInstance, blocks: usize) {
    let info = ProcessInfo {
        sample_rate: 48_000.0,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: true,
        position_beats: 0.0,
        loop_range: None,
    };
    for block in 0..blocks {
        let (mut left, mut right) = (vec![0.0; 512], vec![0.0; 512]);
        let note = Event { offset: 0, kind: EventKind::NoteOn { pitch: 52, velocity: 0.8 } };
        let events = if block == 0 { vec![note] } else { Vec::new() };
        instance.processor.process(&info, &events, &mut left, &mut right);
    }
}

/// The plainest attempt: one instance of the amplifier stays, and beside
/// it others are made from its state, played a moment and let go at once.
fn copies(factory: &Vst3Factory, amplifier: &str) {
    step("the one that stays");
    let mut stays = make(factory, amplifier, None);
    run(&mut stays, 8);
    let state = stays.controller.as_mut().and_then(|controller| controller.save_state());
    for round in 0..COPIES {
        step(&format!("another, made, {round}"));
        let mut other = make(factory, amplifier, state.clone());
        run(&mut other, 40);
        step(&format!("another, let go, {round}"));
        drop(other);
        run(&mut stays, 8);
    }
    step("the one that stays, let go");
    drop(stays);
    step("done");
}

/// Makes an instance of what `target` names, plays it three notes and
/// lets it go: what the `probe` tool does.
fn probe(session: &mut Session, target: &ProbeTarget) {
    let job = session.probe_job(target).unwrap_or_else(|error| panic!("{target:?}: {error}"));
    let notes: Vec<ProbeNote> =
        [52, 59, 64].iter().map(|pitch| ProbeNote { pitch: *pitch, velocity: 100, with: Vec::new() }).collect();
    let options = ProbeOptions { patience: Duration::from_secs(90), ..ProbeOptions::default() };
    job.run(&notes, &options).unwrap_or_else(|error| panic!("{target:?}: {error}"));
}

/// The attempt that is what a session does: the amplifier is on a track
/// of a song that is playing, and is probed, handed a preset and probed
/// again, a few times over. Each probe makes its own instance and lets
/// it go.
fn song(factory: &Vst3Factory, amplifier: &str) {
    let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(factory.clone())]);
    let mut session = Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    let list = listed(factory, amplifier);
    let presets: Vec<_> = list.presets.iter().filter(|preset| preset.category.starts_with("Artists")).collect();
    assert!(!presets.is_empty(), "the amplifier has its artists' presets");
    let (track, device): (TrackId, DeviceId) = {
        let track = session.edit(|project| Ok(project.add_track("Guitar"))).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:polysynth", Some("pluck"), None).unwrap();
        (track, session.add_device(ChainRef::Track(track), amplifier, None, None).unwrap())
    };
    let target = ProbeTarget::Track { track, effects: true };
    for round in 0..COPIES / 2 {
        step(&format!("the amplifier, played, {round}"));
        probe(&mut session, &target);
        step(&format!("the amplifier, given a preset, {round}"));
        session.load_preset(device, &presets[round % presets.len()].id).unwrap_or_else(|error| panic!("{error}"));
        step(&format!("the amplifier, played again, {round}"));
        probe(&mut session, &target);
    }
    step("the song, let go");
    drop(session);
    step("done");
}

/// The attempt that keeps the plugin thread busy, which is what a render
/// of a song does when it makes its instruments from several threads at
/// once. Beside the one that stays, one instance is made, and while it is
/// being made [`CROWD`] more are asked for at the same moment, each from a
/// thread of its own. Their making queues up on the plugin thread, one
/// behind the other, and the thread serves no window until the last is
/// made: longer than the host keeps an instance it was told to let go of,
/// where a machine takes the 0.4 s to make one that this one does. What
/// the plugin posts to itself for the first waits all that time. The
/// first is let go of as soon as it is made, which the plugin thread
/// comes to behind the others. A host that takes it down there and then,
/// because its time has passed by the clock, has the plugin's messages for
/// it delivered a moment later, to an instance that is gone.
///
/// It has [`CROWD`] instances and two, once: the number is fixed, whatever
/// the machine's speed. When they are let go of it waits for the host to
/// have taken them down, and says what the process has of memory then.
fn crowd(factory: &Vst3Factory, amplifier: &str) {
    step("the one that stays");
    let mut stays = make(factory, amplifier, None);
    run(&mut stays, 8);
    let state = stays.controller.as_mut().and_then(|controller| controller.save_state());
    let alone = machine::own();
    step("one made, and let go while many are being made");
    let began = Instant::now();
    let others: Vec<DeviceInstance> = std::thread::scope(|scope| {
        let making: Vec<_> = (0..CROWD)
            .map(|_| {
                scope.spawn(|| {
                    // Behind the first, and long before the first is made.
                    std::thread::sleep(Duration::from_millis(50));
                    make(factory, amplifier, state.clone())
                })
            })
            .collect();
        let mut other = make(factory, amplifier, state.clone());
        run(&mut other, 40);
        // Let go of while the others are still being made: the plugin
        // thread comes to it behind them, without having served a window
        // since before it was made.
        drop(other);
        step(&format!("the first, gone from its owner {:.0} s after it was asked for", began.elapsed().as_secs_f64()));
        making.into_iter().map(|made| made.join().expect("an instance is made")).collect()
    });
    run(&mut stays, 8);
    step("the many, let go");
    let most = machine::own();
    drop(others);
    run(&mut stays, 8);
    // The host keeps them for a while, and takes them down one by one.
    // Their memory is back when the process has little more than it had
    // with the one that stays.
    let let_go = Instant::now();
    let back = |own: u64| own <= alone + (64 << 20);
    while !back(machine::own()) && let_go.elapsed() < KEPT_BUSY * 6 {
        std::thread::sleep(Duration::from_secs(1));
    }
    eprintln!(
        "{WEIGHT}{} MB with the one that stays, {} MB with {CROWD} more, {} MB {:.0} s after they were let go of",
        megabytes(alone),
        megabytes(most),
        megabytes(machine::own()),
        let_go.elapsed().as_secs_f64()
    );
    assert!(back(machine::own()), "the memory of {CROWD} instances did not come back");
    step("the one that stays, let go");
    drop(stays);
    step("done");
}

/// What an instance of the amplifier weighs, and whether the memory comes
/// back: a few are made one by one and let go of together, twice over, and
/// the attempt says what the process had of memory at each point. A second
/// round that ends well above the first is memory that was not given
/// back, by the plugin or by the host.
fn weigh(factory: &Vst3Factory, amplifier: &str) {
    let weight = |what: &str| eprintln!("{WEIGHT}{what}: {} MB", megabytes(machine::own()));
    weight("before any");
    for round in 1..=2 {
        let mut held = Vec::new();
        for number in 1..=WEIGHED {
            step(&format!("round {round}, made, {number}"));
            let mut instance = make(factory, amplifier, None);
            run(&mut instance, 8);
            held.push(instance);
            weight(&format!("round {round}, {number} made"));
        }
        step(&format!("round {round}, let go"));
        drop(held);
        // The host keeps them for a while, and then they are gone.
        std::thread::sleep(KEPT_BUSY);
        weight(&format!("round {round}, all let go, {} s later", KEPT_BUSY.as_secs()));
    }
    step("done");
}

/// The attempt that is what a render does: the amplifier is on three
/// tracks of a song, each behind a synth with a few notes, and the song
/// is rendered a few times over. A render makes all three instances
/// afresh and lets them go, one after the other and a few seconds old,
/// when it is done. (The rendering work saw this shape end the process in
/// 6 runs of 6 before the host kept such instances.)
fn rendered(factory: &Vst3Factory, amplifier: &str) {
    let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(factory.clone())]);
    let mut session = Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    for name in ["Left", "Right", "Lead"] {
        let track = session
            .edit(|project| {
                let track = project.add_track(name);
                let clip = project.add_midi_clip(track, "riff", 0.0, 8.0)?;
                let notes: Vec<Note> = [(0.0, 52), (2.0, 55), (4.0, 59)]
                    .map(|(start, pitch)| Note { pitch, start, length: 1.5, velocity: 100 })
                    .into();
                project.clip_mut(clip)?.add_notes(&notes)?;
                Ok(track)
            })
            .unwrap();
        session.add_device(ChainRef::Track(track), "builtin:polysynth", Some("pluck"), None).unwrap();
        session.add_device(ChainRef::Track(track), amplifier, None, None).unwrap();
    }
    let options = RenderOptions { end_beat: Some(8.0), max_tail_seconds: 0.5, ..RenderOptions::default() };
    for round in 0..COPIES / 2 {
        step(&format!("the song, rendered, {round}"));
        let out = session.render_job().render(&options).unwrap_or_else(|error| panic!("{error}"));
        assert!(out.left.iter().any(|sample| sample.abs() > 1e-4), "the render has sound in it");
    }
    step("the song, let go");
    drop(session);
    step("done");
}

/// One attempt, in a process of its own: see the module documentation.
/// Without [`ATTEMPT`] it does nothing, so that running every test of the
/// file runs each attempt once, where it is counted.
#[test]
#[ignore = "one attempt of the test below, which starts it"]
fn one_attempt() {
    let Ok(asked) = std::env::var(ATTEMPT) else { return };
    let (kind, patient) = asked.split_once(':').expect("a kind of attempt and whether the host is patient");
    let (enough, has) = room();
    assert!(enough, "the machine is short of memory ({has}): the attempt is not made");
    fault::report_to_stderr();
    let factory = vst3(patient == "patient");
    let installed = factory.descriptors();
    let Some(amplifier) = installed.iter().find(|descriptor| descriptor.uri.starts_with(NEURAL)) else {
        step("no Neural DSP plugin is installed; done");
        return;
    };
    // The plugin fails when the machine is busy: what it does behind the
    // scenes then takes longer than the instance lives. So that the count
    // does not hang on what else the machine happens to be doing, the
    // attempt keeps some of its processors busy itself.
    let busy = Busy::new();
    match kind {
        "copies" => copies(&factory, &amplifier.uri),
        "crowd" => crowd(&factory, &amplifier.uri),
        "weigh" => weigh(&factory, &amplifier.uri),
        "render" => rendered(&factory, &amplifier.uri),
        _ => song(&factory, &amplifier.uri),
    }
    drop(busy);
}

/// Threads that do nothing but take processor time, for as long as this
/// is kept: half as many as the machine has processors, or as many as
/// [`BUSY`] says.
struct Busy {
    stop: Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Busy {
    fn new() -> Self {
        let processors = std::thread::available_parallelism().map_or(4, usize::from);
        let count = std::env::var(BUSY).ok().and_then(|value| value.parse().ok()).unwrap_or(processors / 2);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spin = |stop: Arc<std::sync::atomic::AtomicBool>| {
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        };
        Self { threads: (0..count).map(|_| spin(stop.clone())).collect(), stop }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// What the machine has of memory and how busy it is, and what this
/// process has taken.
#[cfg(windows)]
mod machine {
    use std::ffi::c_void;

    /// In bytes.
    pub struct Memory {
        pub physical: u64,
        pub free: u64,
        /// What all processes together have been promised.
        pub committed: u64,
    }

    #[repr(C)]
    struct Status {
        size: u32,
        load: u32,
        physical: u64,
        free_physical: u64,
        commit_limit: u64,
        commit_left: u64,
        addresses: u64,
        free_addresses: u64,
        extended: u64,
    }

    #[repr(C)]
    struct Counters {
        size: u32,
        faults: u32,
        peak_working_set: usize,
        working_set: usize,
        peak_paged_pool: usize,
        paged_pool: usize,
        peak_nonpaged_pool: usize,
        nonpaged_pool: usize,
        committed: usize,
        peak_committed: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, size: u32) -> i32;
        fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    }

    pub fn memory() -> Option<Memory> {
        // SAFETY: the system fills a local whose size it is told.
        let status = unsafe {
            let mut status: Status = std::mem::zeroed();
            status.size = size_of::<Status>() as u32;
            if GlobalMemoryStatusEx(&mut status) == 0 {
                return None;
            }
            status
        };
        Some(Memory {
            physical: status.physical,
            free: status.free_physical,
            committed: status.commit_limit.saturating_sub(status.commit_left),
        })
    }

    /// What this process has been promised of memory, in bytes.
    pub fn own() -> u64 {
        // SAFETY: as above, about the calling process.
        unsafe {
            let mut counters: Counters = std::mem::zeroed();
            counters.size = size_of::<Counters>() as u32;
            match K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.size) {
                0 => 0,
                _ => counters.committed as u64,
            }
        }
    }

    /// How long the machine's processors have been idle, and how long they
    /// have been running at all, since it started, in units of 100 ns.
    pub fn times() -> (u64, u64) {
        let (mut idle, mut kernel, mut user) = (0, 0, 0);
        // SAFETY: fills three locals. The kernel's time has the idle time
        // in it.
        unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        (idle, kernel + user)
    }
}

#[cfg(not(windows))]
mod machine {
    pub struct Memory {
        pub physical: u64,
        pub free: u64,
        pub committed: u64,
    }
    pub fn memory() -> Option<Memory> {
        None
    }
    pub fn own() -> u64 {
        0
    }
    pub fn times() -> (u64, u64) {
        (0, 0)
    }
}

/// Says, on standard error, where the process was when it died of a
/// fault nobody handled: whose code it was in, on which thread, and how it
/// got there. Each place is a module and how far into it, which is all
/// that can be said of code without symbols, and enough to tell a
/// plugin's from the host's. Lines of it begin with [`fault::MARK`].
#[cfg(windows)]
mod fault {
    use std::ffi::c_void;
    use std::fmt::Write;

    pub const MARK: &str = "FAULT ";
    const FRAMES: usize = 48;

    #[repr(C)]
    struct Record {
        code: u32,
        flags: u32,
        chained: *mut Record,
        address: *mut c_void,
        parameters: u32,
        information: [usize; 15],
    }

    #[repr(C)]
    struct Pointers {
        record: *mut Record,
        context: *mut c_void,
    }

    type Filter = unsafe extern "system" fn(*mut Pointers) -> i32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetUnhandledExceptionFilter(filter: Option<Filter>) -> Option<Filter>;
        fn RtlCaptureStackBackTrace(skip: u32, count: u32, trace: *mut *mut c_void, hash: *mut u32) -> u16;
        fn GetModuleHandleExW(flags: u32, address: *const c_void, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
        fn GetCurrentThreadId() -> u32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn WriteFile(
            file: *mut c_void,
            bytes: *const u8,
            count: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    /// Text put together without the heap, which may be what is broken.
    struct Line {
        bytes: [u8; 512],
        used: usize,
    }

    impl Write for Line {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let room = self.bytes.len() - self.used;
            let taken = text.len().min(room);
            self.bytes[self.used..self.used + taken].copy_from_slice(&text.as_bytes()[..taken]);
            self.used += taken;
            Ok(())
        }
    }

    fn say(arguments: std::fmt::Arguments<'_>) {
        let mut line = Line { bytes: [0; 512], used: 0 };
        let _ = line.write_str(MARK);
        let _ = line.write_fmt(arguments);
        let _ = line.write_char(char::from(10u8));
        let mut written = 0;
        // SAFETY: the handle is the process's own standard error, and the
        // bytes are a local that outlives the call.
        unsafe {
            WriteFile(
                GetStdHandle(-12i32 as u32),
                line.bytes.as_ptr(),
                line.used as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }

    /// The module `address` lies in, by its file's name, and how far into
    /// it; or the address alone.
    fn place(address: *const c_void, out: &mut Line) {
        let mut module = std::ptr::null_mut();
        let mut name = [0u16; 260];
        // SAFETY: asks which module an address is in (flags: by address,
        // without taking a reference) and what its file is called; both
        // write into locals of the sizes given.
        let length = unsafe {
            match GetModuleHandleExW(0x4 | 0x2, address, &mut module) {
                0 => 0,
                _ => GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) as usize,
            }
        };
        if length == 0 {
            let _ = write!(out, "{address:p} (in no module)");
            return;
        }
        let file = &name[..length.min(name.len())];
        let start = file.iter().rposition(|unit| *unit == 0x5c).map_or(0, |at| at + 1);
        for unit in &file[start..] {
            let _ = out.write_char(char::from_u32(u32::from(*unit)).unwrap_or('?'));
        }
        let _ = write!(out, "+{:#x}", address as usize - module as usize);
    }

    unsafe extern "system" fn filter(pointers: *mut Pointers) -> i32 {
        // SAFETY: the system hands the filter a record of the fault.
        let record = unsafe { &*(*pointers).record };
        let mut at = Line { bytes: [0; 512], used: 0 };
        place(record.address, &mut at);
        let what = match (record.code, record.information[0]) {
            (0xC000_0005, 0) => "reading",
            (0xC000_0005, 1) => "writing",
            (0xC000_0005, 8) => "running",
            _ => "at",
        };
        say(format_args!(
            "code {:#x} in {} on thread {} ({}), {what} {:#x}",
            record.code,
            std::str::from_utf8(&at.bytes[..at.used]).unwrap_or("?"),
            // SAFETY: a plain query.
            unsafe { GetCurrentThreadId() },
            std::thread::current().name().unwrap_or("not one of the host's"),
            record.information[1]
        ));
        let mut frames = [std::ptr::null_mut(); FRAMES];
        // SAFETY: fills a local of the size given.
        let count = unsafe { RtlCaptureStackBackTrace(0, FRAMES as u32, frames.as_mut_ptr(), std::ptr::null_mut()) };
        // What led to the fault lies below the system's own way to here.
        let from = frames[..usize::from(count)].iter().position(|frame| *frame == record.address).unwrap_or(0);
        for (depth, frame) in frames[from..usize::from(count)].iter().enumerate() {
            let mut line = Line { bytes: [0; 512], used: 0 };
            place(*frame, &mut line);
            say(format_args!("  {depth:>2} {}", std::str::from_utf8(&line.bytes[..line.used]).unwrap_or("?")));
        }
        // On to whatever would have happened: the process ends.
        0
    }

    pub fn report_to_stderr() {
        // SAFETY: registers a function of this program for the life of the
        // process.
        unsafe { SetUnhandledExceptionFilter(Some(filter)) };
    }
}

#[cfg(not(windows))]
mod fault {
    pub const MARK: &str = "FAULT ";
    pub fn report_to_stderr() {}
}

/// How one attempt ended.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// It got to the end.
    Survived,
    /// The process died, with this exit code, after this step.
    Died { code: Option<i32>, after: String },
    /// It ended in a failed assertion of its own, after this step.
    Failed { after: String },
}

/// Makes one attempt of `kind` in a process of its own, and says what
/// the process said of a fault.
fn attempt(kind: &str, patient: bool) -> (Outcome, Duration) {
    let (enough, has) = room();
    assert!(enough, "the machine is short of memory ({has}): no attempt is started");
    let started = Instant::now();
    let output = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["one_attempt", "--exact", "--include-ignored", "--nocapture", "--test-threads", "1"])
        .env(ATTEMPT, format!("{kind}:{}", if patient { "patient" } else { "at once" }))
        .output()
        .expect("the test binary starts again");
    let said = String::from_utf8_lossy(&output.stderr);
    for weight in said.lines().filter_map(|line| line.strip_prefix(WEIGHT)) {
        eprintln!("{weight}");
    }
    let fault: Vec<&str> =
        said.lines().filter(|line| line.starts_with(fault::MARK) || line.contains("panicked at")).collect();
    if !fault.is_empty() {
        let mut report = String::from("an attempt died:");
        for line in &fault {
            report.push(char::from(10u8));
            report.push_str(line);
        }
        eprintln!("{report}");
    }
    let after = said.lines().rev().find_map(|line| line.strip_prefix(STEP)).unwrap_or("nothing").to_string();
    let outcome = match (output.status.success(), output.status.code()) {
        (true, _) => Outcome::Survived,
        // The test harness ends a failed test with the code 101.
        (false, Some(101)) => Outcome::Failed { after },
        (false, code) => Outcome::Died { code, after },
    };
    (outcome, started.elapsed())
}

/// How many of `attempts` attempts of `kind` did not get to the end.
fn count(kind: &str, patient: bool, attempts: usize) -> usize {
    let mut lost = 0;
    for number in 1..=attempts {
        let before = machine::times();
        let (outcome, took) = attempt(kind, patient);
        let after = machine::times();
        let how = if patient { "patient" } else { "at once" };
        // The machine's load is part of the count: say it beside it.
        let running = (after.1 - before.1).max(1) as f64;
        let load = 100.0 * (1.0 - (after.0 - before.0) as f64 / running);
        let (_, has) = room();
        eprintln!(
            "{kind}, {how}, attempt {number} of {attempts}: {outcome:?} ({:.0} s; the machine {load:.0}% busy, {has})",
            took.as_secs_f64()
        );
        lost += usize::from(outcome != Outcome::Survived);
    }
    lost
}

#[test]
#[ignore = "needs a Neural DSP Archetype (VST3); run with `cargo test -p trill-plugins --test lifetimes -- --include-ignored --nocapture`"]
fn a_process_that_makes_and_lets_go_of_plugin_instances_goes_on_hosting() {
    if std::env::var_os(ATTEMPT).is_some() {
        return;
    }
    let attempts = std::env::var(ATTEMPTS).ok().and_then(|value| value.parse().ok()).unwrap_or(4);

    // What an instance weighs, and whether what it took comes back.
    let (weighed, _) = attempt("weigh", true);
    assert_eq!(weighed, Outcome::Survived, "weighing the amplifier");

    // What the plugin does to a host that takes it down as soon as it is
    // let go of, while the plugin thread is kept from its windows. Counted
    // and said, not asserted: when this comes to none of the attempts,
    // time after time, the plugin has been mended and `vst3/quirks.rs` can
    // let go of it.
    let unprotected = count("crowd", false, attempts);
    eprintln!(
        "taken down at once behind a busy plugin thread, the amplifier ended {unprotected} of {attempts} attempts"
    );

    // And with the host as it is.
    let kept = ["crowd", "copies", "song", "render"].map(|kind| count(kind, true, attempts));
    assert_eq!(kept, [0; 4], "attempts that did not get to the end, of {attempts} each");
}
