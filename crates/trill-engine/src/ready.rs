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
//! ready when every note it is about to be given has sounded, each by
//! itself, and nothing has been loading for a quarter of a second since.**
//! Before a render begins, each track whose instrument is a plugin is
//! rehearsed on its own: the first different notes it will play, in their
//! order, each with the controllers the song has for it, some ten times a
//! second. Then the notes are let go of, the plugins are silenced and
//! reset, and the render starts from the top.
//!
//! **Every note sounds the same number of times.** A sampled instrument
//! keeps several recordings of a note and plays the next one each time,
//! and where it is in that round is not something a reset puts back. If
//! the rehearsal went on for as long as the clock said, a render would
//! begin at another place in the round each time, and the same song would
//! not render to the same samples twice. So a note is rehearsed until it
//! has sounded three times, and then not again: however long the wait
//! lasts, and whether or not the plugin was found ready before, a render
//! begins with every rehearsed note three takes on. (What is left to the
//! clock is for how long nothing more may arrive before the plugin is
//! taken as ready, and nothing is played once every note has sounded as
//! often as it is to.) This holds an instrument whose takes go round. It
//! cannot hold
//! one that draws its takes at random from a stream its maker shares
//! between every instance in the program (some patches of Ample Sound's
//! do, for a fret noise): whatever else was played in the program moved
//! that, and only a program that has just started begins it at the same
//! place. Nor a note that sounded and then stopped for good while it was
//! rehearsed: the wait does not hang on it.
//!
//! **And every plugin is run the same amount.** What some plugins give
//! depends on how much they were run before, to the block: an amplifier
//! was measured (Archetype Tim Henson X) whose sound is another after one
//! block more of silence, and the same again after the same number,
//! however long that took by the clock. A reset does not put that back
//! either. So the wait counts what it runs. The effects after an
//! instrument are run only on the notes the instrument itself sounds,
//! from where it begins to: while a sampler is still loading and its
//! notes come to nothing, try after try, what stands behind it is given
//! nothing at all. An effect that is tried with noise is tried until it
//! has passed three bursts, and then left alone like a note. Once
//! nothing is left to ask, nothing is run. What a plugin has been given
//! when a render begins is then a matter of what it was asked and how it
//! answered, and not of how long it took to load.
//!
//! **A key that switches is pressed, not heard.** On an instrument with
//! articulations some keys choose how the notes after them are played and
//! sound nothing themselves, and the track says which
//! ([`trill_core::InstrumentProfile::is_keyswitch`]). They are not asked
//! as notes: one would be held to answering for half the caller's
//! patience and then said to have made no sound, and pressed by itself it
//! would leave every note after it in the rehearsal on that way of
//! playing. A note is rehearsed under the switch the part plays it with
//! (pressed just before it, let go with it), and a pitch that the part
//! plays under two switches is two notes, with recordings of their own:
//! each sounds three times. A switch that is for the next note only is
//! pressed with that note and no other. A note the part plays before it
//! has chosen anything is rehearsed on the way of playing the track calls
//! its default, where a key chooses that, and the instrument is left on
//! it when the rehearsal is over, so that the song begins as it would
//! have with nothing rehearsed; a part that chooses before its first note
//! leaves nothing to restore.
//!
//! **Each note is heard alone.** A drum that has not been loaded sounds
//! exactly like a key that plays nothing, and under the ringing of the
//! drum before it, it sounds like nothing at all: a tom that is there
//! hides a cymbal that is not. So after every rehearsed note the plugins
//! are reset, which silences most, and what still rings is left a moment
//! to die away. A plugin that rings on through all of that is heard as
//! before, one note over the last, and its silent keys cannot be told.
//!
//! **A sampler that is partly there is waited for.** A kit arrives one
//! drum after another, and between two of them a sampler may do nothing
//! that shows for seconds: no memory taken, nothing read, no processor
//! used. (Superior Drummer 3 was watched doing so; see the `loading`
//! example of `trill-plugins`.) An instrument that answers some of its
//! notes and not others is therefore not ready after a quarter of a
//! second like one that answers them all: it is held to it for half of
//! the caller's patience with no further note arriving and nothing
//! loading, five seconds as renders and probes ask. Then the silent notes
//! are taken to be keys that play nothing (a keyswitch, a note outside the
//! range), which is remembered of that plugin in that state, so that it
//! is waited out once and not every time. The pitches are handed back
//! ([`Waited::silent`]) for a reply to say that they made no sound,
//! because this is the case that cannot be told from a kit that is still
//! loading after all that time.
//!
//! **Loading shows from outside.** While samples are being read, the
//! program's memory rises by megabytes a second. While it does, nothing
//! is taken as ready, and the wait goes on, up to two minutes; the
//! caller's patience is for how long nothing at all may happen. (Windows
//! can be asked. Elsewhere the answers have to do.) The sign is the whole
//! program's: something else that grows as fast makes a wait longer,
//! never shorter. (How much the program reads from files is not a sign:
//! a sampler that plays its samples from the disk reads whenever it is
//! played a note.)
//!
//! **A plugin that is held up is waited for.** The plugins of a host take
//! their turns on one thread, and a sampler begins to load only when it
//! gets one. While that thread is behind (a song of many plugins is being
//! let go of, say, each taking its time) a plugin that has just been made
//! does nothing at all, and none of it is its doing. The caller's patience
//! does not run out meanwhile ([`DeviceFactory::busy`]). And the memory a
//! song gives back as it goes does not hide what a kit takes as it comes:
//! the mark that growth is measured against comes down with it.
//!
//! **An instrument that listens is lent something to listen to.** A
//! device may listen to another track of the song (a vocoder to a voice,
//! a synthesizer on a patch that plays what it is fed), and a rehearsal
//! plays one track by itself: such an instrument would answer no note,
//! and the render would be refused for want of it. So while a note is
//! tried, every device of the track that listens is handed noise in the
//! place of the track it listens to, the same noise each time, and
//! silence between notes.
//!
//! **Effects wait too.** An amplifier or a reverb that is a plugin may
//! pass no sound for a moment after it is made. Behind a plugin
//! instrument it is rehearsed with it: the notes are heard at the end of
//! the track's plugins, not at the instrument. Anywhere else (after a
//! built-in instrument, on an audio track, a group, a return or the
//! master) there are no notes to give it, so it is given short bursts of
//! noise instead and is ready when three of them have come out and
//! nothing has been loading for a quarter of a second since it first
//! passed one. An effect that lets nothing through (a gate that
//! stays shut, a delay that is all echo) is given up on after a few
//! seconds and remembered, like an instrument that never answers.
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
//! A plugin that was slow to give its first answer is held to it for
//! longer before it is taken as ready (half as long as it took, up to two
//! seconds), and so is one whose three answers to a note were not alike
//! (a second): a kit that plays another recording each time sounds like
//! one that is still arriving.
//!
//! **What was ready is remembered** for as long as the program runs: the
//! plugin, its state, the notes and which of them sounded. A plugin whose
//! notes sound as they did when it was last found ready, with nothing
//! loading, is not held to it for longer than a moment. It is rehearsed
//! just as often.
//!
//! **The plugins of a song are rehearsed at once.** Rehearsing is work:
//! every note of every instrument is played through its plugins three
//! times and more, and a song of seventeen plugins waited over a minute on
//! one thread for what was mostly that. So a render that may use several
//! threads rehearses on them, with the same care as it plays: plugins
//! that may keep something between them (the instances of one plugin,
//! unless it is known to keep nothing) are tried one after the other and
//! in the order of the song, and the others beside them. How often each
//! note sounds does not depend on it.
//!
//! **While it plays, nothing waits.** A plugin in the song that is heard
//! live is not rehearsed: nobody is to hear notes that are not in the
//! song. What can be said of it is that it was just made or given
//! another sound and that the program shows the signs of loading:
//! [`LoadWatch`], which a session keeps, for a window to say "loading" on
//! the device and a model to see it before it presses play.
//!
//! What this cannot see: a sampler that stops for longer than it is held
//! and is still not done (said, as above, where notes are silent; not
//! where every note asked already sounds and others, not asked, are yet
//! to come, which a render does not play), one whose later notes are not
//! among the thirty-two that are rehearsed, a plugin that goes on ringing
//! through a reset, and an effect that passes sound before it is ready to
//! pass it rightly.

use crate::crew::Crew;
use crate::graph::Graph;
use parking_lot::Mutex;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use trill_core::device::{DeviceCategory, DeviceFactory, DeviceProcessor, Event, EventKind, KeyInput, ProcessInfo};
use trill_core::{Channel, ControllerKind, Device, DeviceId, Project, TimeSignature, TrackId};

/// The most notes of a track that are rehearsed: the different pitches it
/// plays, as they first come. A drum part has a dozen; the cymbals, which
/// a sampler loads last, must be among them.
pub const OPENING_NOTES: usize = 32;
/// Frames in each block a rehearsal processes.
const BLOCK: usize = 256;
/// How long each rehearsed note is held; a note that has made no sound by
/// then is held for [`LATE_SECONDS`] more, for an instrument that speaks
/// late.
const NOTE_SECONDS: f64 = 0.05;
const LATE_SECONDS: f64 = 0.1;
/// How long the plugins are listened to after the noise an effect is
/// tried with.
const AFTER_SECONDS: f64 = 0.1;
/// How long at most what a plugin lets ring after it was reset is left to
/// die away before the next note.
const HUSH_SECONDS: f64 = 0.1;
/// Output above this (-80 dB) is an answer, unless the plugin makes more
/// noise than that when it is given nothing.
const ANSWER: f32 = 1e-4;
/// How far above its own noise an answer must be.
const ABOVE_NOISE: f32 = 4.0;
/// Two answers this close in level, in dB, are the same answer.
const SAME_ANSWER_DB: f32 = 3.0;
/// How long the answers must have stayed the same for the plugin to be
/// taken as ready: at least this, at most [`SETTLED_AT_MOST`], and half
/// as long as its first answer took to come.
const SETTLED_FOR: Duration = Duration::from_millis(250);
const SETTLED_AT_MOST: Duration = Duration::from_secs(2);
/// How long the same notes must have sounded, with nothing loading, for a
/// plugin whose levels never settle to be taken as it is.
const UNSETTLED_FOR: Duration = Duration::from_secs(1);
/// The longest an effect that passes nothing is waited for.
const EFFECT_PATIENCE: Duration = Duration::from_secs(3);
/// The level of the noise an effect is tried with: -12 dB, which opens a
/// gate that is set for music.
const SIGNAL_LEVEL: f32 = 0.25;
/// How many stretches of [`NOTE_SECONDS`] the noise lasts.
const SIGNAL_STRETCHES: usize = 3;
/// How long a rehearsal waits between tries.
const BETWEEN_TRIES: Duration = Duration::from_millis(100);
/// Where the noise begins that a device which listens to another track is
/// lent while it is tried.
const KEY_SEED: u32 = 0x9E37_79B9;
/// How many times each rehearsed note is to sound, and an effect to pass a
/// burst of noise, before it is not asked again: see the module
/// documentation.
const SOUNDINGS: usize = 3;
/// A note that has sounded and then stays silent for this many tries is
/// one that may not come back: the wait does not hang on it.
const STALLED_TRIES: u32 = 2;
/// The program is loading something while its memory grows faster than
/// this, in bytes a second: a slow disk reads samples faster.
const LOADING_BYTES_A_SECOND: f64 = 4.0 * 1024.0 * 1024.0;
/// Memory given back in one piece of this size or more is something
/// unloaded: a kit that is being replaced.
const UNLOADED_BYTES: u64 = 64 * 1024 * 1024;
/// The same for a wait, which is over in seconds and shares the program
/// with whatever else it is doing: a song's worth of plugins let go of.
const UNLOADED_UNDER_A_WAIT: u64 = 256 * 1024 * 1024;
/// How long plugins that show signs of loading are waited for at most.
const LOADING_LIMIT: Duration = Duration::from_secs(120);
/// The longest a rehearsed plugin is left to ring out before it is reset:
/// long enough for a hall to forget a short burst of noise.
const RING_OUT_SECONDS: f64 = 10.0;
/// How long output must stay down to count as rung out.
const QUIET_SECONDS: f64 = 0.1;
/// How long a track that is known not to answer is waited for.
const KNOWN_SILENT: Duration = Duration::from_secs(1);
/// How many things that did not answer are remembered.
const REMEMBERED: usize = 256;
/// How long after it was made, or after the last sign of loading, a
/// plugin that plays live is still said to be loading: longer than the
/// pauses a sampler was seen to make between two drums.
const SETTLED_AFTER: Duration = Duration::from_secs(4);
/// How often a [`LoadWatch`] looks at the program at most.
const LOOK_EVERY: Duration = Duration::from_millis(100);
/// How long ago a [`LoadWatch`] may have looked for what it saw then to
/// say anything about now.
const LOOKED_LATELY: Duration = Duration::from_secs(1);

/// Which keys of a plugin are known to sound, and which to stay silent
/// when everything is loaded: by MIDI note number.
type Keys = [Option<bool>; 128];

/// What did not answer the last time it was asked, by [`signature`].
static KNOWN_NOT_TO_ANSWER: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// What was found ready, by [`signature`], and how it answered then.
static KNOWN_READY: Mutex<Vec<(u64, Vec<Answer>)>> = Mutex::new(Vec::new());
/// What is known of the keys of plugins, by [`device_signature`].
static KNOWN_KEYS: Mutex<Vec<(u64, Keys)>> = Mutex::new(Vec::new());

/// What the program shows of its own work, from the outside.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Vitals {
    /// Memory in use (the working set), in bytes.
    pub in_use: u64,
    /// Memory asked for (committed), in bytes.
    pub asked: u64,
    /// Bytes read from files and devices since the program began.
    pub read: u64,
    /// How many reads those were.
    pub reads: u64,
    /// Processor time the program has used, on all its threads.
    pub busy: Duration,
    /// Processor time the thread that asks has used.
    pub own: Duration,
    /// How many handles the program holds: files, threads and the like.
    pub handles: u32,
}

impl Vitals {
    /// The program's vitals now. `None` where they cannot be asked.
    pub fn now() -> Option<Self> {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::FILETIME;
            use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
            use windows::Win32::System::Threading::{
                GetCurrentProcess, GetCurrentThread, GetProcessHandleCount, GetProcessIoCounters, GetProcessTimes,
                GetThreadTimes, IO_COUNTERS,
            };
            let mut counters = PROCESS_MEMORY_COUNTERS::default();
            let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let mut io = IO_COUNTERS::default();
            let mut times = [FILETIME::default(); 6];
            let mut handles = 0u32;
            // SAFETY: the handles are the pseudo-handles of this process and
            // this thread, which are always valid, and every pointer is to
            // a local of the type the call fills in.
            unsafe {
                let process = GetCurrentProcess();
                GetProcessMemoryInfo(process, &mut counters, size).ok()?;
                GetProcessIoCounters(process, &mut io).ok()?;
                let [made, left, kernel, user, own_kernel, own_user] = &mut times;
                GetProcessTimes(process, made, left, kernel, user).ok()?;
                let (mut made, mut left) = (FILETIME::default(), FILETIME::default());
                GetThreadTimes(GetCurrentThread(), &mut made, &mut left, own_kernel, own_user).ok()?;
                GetProcessHandleCount(process, &mut handles).ok()?;
            }
            // Times come in units of 100 ns.
            let spent = |kernel: FILETIME, user: FILETIME| {
                let ticks = |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
                Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
            };
            Some(Self {
                in_use: counters.WorkingSetSize as u64,
                asked: counters.PagefileUsage as u64,
                read: io.ReadTransferCount,
                reads: io.ReadOperationCount,
                busy: spent(times[2], times[3]),
                own: spent(times[4], times[5]),
                handles,
            })
        }
        #[cfg(target_os = "macos")]
        return macos_vitals();
        #[cfg(target_os = "linux")]
        return linux::vitals();
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        None
    }
}

/// [`Vitals::now`] as macOS says them: memory in use is what the program's
/// allocations hold now, by the allocator's count; memory asked for is the
/// footprint (what the program wrote to, in memory or compressed); reads are
/// the blocks read from a disk; handles are files and threads.
///
/// In use is not what is resident, as on Windows: the allocator keeps what
/// was freed and hands it out again, and memory the program grows into that
/// way is resident and counted already. A sampler that loads in a program
/// that let go of a song before shows only in what is allocated.
#[cfg(target_os = "macos")]
fn macos_vitals() -> Option<Vitals> {
    use std::mem::{size_of, zeroed};
    /// What the allocator says of all its zones.
    #[repr(C)]
    struct MallocStatistics {
        blocks_in_use: u32,
        size_in_use: usize,
        max_size_in_use: usize,
        size_allocated: usize,
    }
    unsafe extern "C" {
        fn malloc_zone_statistics(zone: *mut std::ffi::c_void, statistics: *mut MallocStatistics);
    }
    // SAFETY: getpid cannot fail; each structure is one of numbers, for
    // which nothing is a fine start, filled in by the call it is the
    // structure of, for this process or the calling thread.
    unsafe {
        let pid = libc::getpid();
        let mut usage: libc::rusage_info_v4 = zeroed();
        if libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, (&raw mut usage).cast()) != 0 {
            return None;
        }
        let mut times: libc::rusage = zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut times);
        let spent = |time: libc::timeval| Duration::new(time.tv_sec as u64, time.tv_usec as u32 * 1000);
        let mut thread: libc::thread_basic_info = zeroed();
        let mut count = libc::THREAD_BASIC_INFO_COUNT;
        let own = match libc::thread_info(
            libc::pthread_mach_thread_np(libc::pthread_self()),
            libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
            (&raw mut thread).cast(),
            &mut count,
        ) {
            libc::KERN_SUCCESS => {
                let time =
                    |time: libc::time_value_t| Duration::new(time.seconds as u64, time.microseconds as u32 * 1000);
                time(thread.user_time) + time(thread.system_time)
            }
            _ => Duration::ZERO,
        };
        let mut task: libc::proc_taskinfo = zeroed();
        let task_size = size_of::<libc::proc_taskinfo>() as i32;
        let threads = match libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut task).cast(), task_size) {
            size if size == task_size => task.pti_threadnum.max(0) as u32,
            _ => 0,
        };
        // Asked for no room, it says how much the list of files would take.
        let listed = libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0).max(0) as usize;
        let files = (listed / size_of::<libc::proc_fdinfo>()) as u32;
        let mut allocated =
            MallocStatistics { blocks_in_use: 0, size_in_use: 0, max_size_in_use: 0, size_allocated: 0 };
        // No zone: all of them, added up.
        malloc_zone_statistics(std::ptr::null_mut(), &mut allocated);
        Some(Vitals {
            in_use: allocated.size_in_use as u64,
            asked: usage.ri_phys_footprint,
            read: usage.ri_diskio_bytesread,
            reads: times.ru_inblock.max(0) as u64,
            busy: spent(times.ru_utime) + spent(times.ru_stime),
            own,
            handles: files + threads,
        })
    }
}

#[cfg(target_os = "linux")]
#[path = "ready/linux.rs"]
mod linux;

/// Watches the program for the sign that loading samples makes: its
/// memory rises past the most it has held, and goes on rising. Memory
/// that is taken and given back, as any busy program does, does not
/// count.
struct Signs {
    /// When the program was last looked at.
    looked: Option<Instant>,
    /// The most memory it has held so far: in use, and asked for.
    most: (u64, u64),
    /// How much memory has to be given back for that mark to come down
    /// with it, so that what is loaded after something was unloaded shows;
    /// `None` for a mark that only rises.
    follows: Option<u64>,
}

impl Signs {
    fn new() -> Self {
        let now = Vitals::now();
        Self {
            most: now.map(|vitals| (vitals.in_use, vitals.asked)).unwrap_or_default(),
            looked: now.map(|_| Instant::now()),
            follows: None,
        }
    }

    /// [`Self::new`], with a mark that comes down when `bytes` or more
    /// are given back.
    fn following(bytes: u64) -> Self {
        Self { follows: Some(bytes), ..Self::new() }
    }

    /// Whether the program grew since this was last asked at the rate of
    /// something being loaded. Always false where memory cannot be asked
    /// about.
    fn seen(&mut self) -> bool {
        let (Some(then), Some(now)) = (self.looked, Vitals::now()) else { return false };
        let at = Instant::now();
        let seconds = at.duration_since(then).as_secs_f64();
        if seconds < 0.05 {
            return false;
        }
        let grown = now.in_use.saturating_sub(self.most.0).max(now.asked.saturating_sub(self.most.1));
        let given_back = |most: u64, now: u64| self.follows.is_some_and(|bytes| now + bytes < most);
        let mark = |most: u64, now: u64| if given_back(most, now) { now } else { most.max(now) };
        self.most = (mark(self.most.0, now.in_use), mark(self.most.1, now.asked));
        self.looked = Some(at);
        grown as f64 / seconds >= LOADING_BYTES_A_SECOND
    }
}

/// Whether something in the program seems to be loading right now: over
/// the tenth of a second this takes, its memory grew as it does while a
/// plugin reads its samples. It is the program that is
/// asked, not one device, and only Windows, macOS and Linux can be asked:
/// elsewhere the answer is `false`. A sampler pauses between two drums, so `false` is
/// not "done": [`LoadWatch`] keeps looking.
pub fn is_loading() -> bool {
    let mut signs = Signs::new();
    std::thread::sleep(LOOK_EVERY);
    signs.seen()
}

/// Which plugins that play live seem to be loading: the ones that were
/// made, or given another state or preset, since the program last stood
/// still for a few seconds.
///
/// A plugin that is heard live cannot be asked by playing it notes, so
/// this goes by the program alone, whose memory rises while samples are
/// read. It cannot tell which plugin that is for, and says "loading" of
/// every one that began since. A plugin that loads nothing is said to be loading
/// for a few seconds after it is made; one that pauses for longer in the
/// middle is said to have done.
pub struct LoadWatch {
    /// How long after the last sign of loading a plugin is said to have
    /// done.
    settled_after: Duration,
    state: Mutex<Option<Watching>>,
}

impl Default for LoadWatch {
    fn default() -> Self {
        Self::settling_after(SETTLED_AFTER)
    }
}

struct Watching {
    signs: Signs,
    /// When the program was last looked at, and when it last showed a
    /// sign of loading.
    looked: Instant,
    stirred: Option<Instant>,
    /// The plugins that may be loading, and since when.
    fresh: Vec<(DeviceId, Instant)>,
}

impl LoadWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// A watch that says a plugin has done loading once the program has
    /// stood still for `settled_after`.
    pub fn settling_after(settled_after: Duration) -> Self {
        Self { settled_after, state: Mutex::new(None) }
    }

    /// A plugin was made, or given a state or a preset: it may be loading
    /// from now on.
    pub fn began(&self, device: DeviceId) {
        let now = Instant::now();
        let mut state = self.state.lock();
        let watching = state.get_or_insert_with(|| Watching {
            signs: Signs::following(UNLOADED_BYTES),
            looked: now,
            stirred: None,
            fresh: Vec::new(),
        });
        if watching.fresh.is_empty() {
            // What the program did while nothing was watched is not this
            // plugin's.
            (watching.signs, watching.looked, watching.stirred) = (Signs::following(UNLOADED_BYTES), now, None);
        }
        watching.fresh.retain(|(other, _)| *other != device);
        watching.fresh.push((device, now));
    }

    /// The plugin is gone.
    pub fn gone(&self, device: DeviceId) {
        if let Some(watching) = self.state.lock().as_mut() {
            watching.fresh.retain(|(other, _)| *other != device);
        }
    }

    /// The plugins that seem to be loading. Looks at the program if it
    /// has not just done so: asked every frame by a window, this is the
    /// watching. Never waits.
    pub fn loading(&self) -> Vec<DeviceId> {
        let mut state = self.state.lock();
        let Some(watching) = state.as_mut().filter(|watching| !watching.fresh.is_empty()) else {
            return Vec::new();
        };
        let now = Instant::now();
        if now.duration_since(watching.looked) >= LOOK_EVERY {
            if watching.signs.seen() {
                watching.stirred = Some(now);
            }
            watching.looked = now;
        }
        let (stirred, settled_after) = (watching.stirred, self.settled_after);
        watching.fresh.retain(|(_, since)| {
            let last = stirred.map_or(*since, |stirred| stirred.max(*since));
            now.duration_since(last) < settled_after
        });
        watching.fresh.iter().map(|(device, _)| *device).collect()
    }

    /// [`Self::loading`] for a caller that asks once in a while and may
    /// wait a moment, such as a server answering a request: if nobody has
    /// looked lately, the program is looked at over a tenth of a second
    /// first. What it did before that is not known and not counted.
    pub fn loading_now(&self) -> Vec<DeviceId> {
        let stale = {
            let mut state = self.state.lock();
            match state.as_mut().filter(|watching| !watching.fresh.is_empty()) {
                Some(watching) if watching.looked.elapsed() >= LOOKED_LATELY => {
                    (watching.signs, watching.looked) = (Signs::following(UNLOADED_BYTES), Instant::now());
                    true
                }
                _ => false,
            }
        };
        if stale {
            std::thread::sleep(LOOK_EVERY);
        }
        self.loading()
    }
}

/// How waiting for the plugins of one chain went.
#[derive(Clone, Debug, PartialEq)]
pub struct Waited {
    /// The track they are on; `None` for the master.
    pub track: Option<TrackId>,
    /// The plugin that was waited for: the track's instrument, or the
    /// first of the effects that were tried with it.
    pub device: DeviceId,
    /// Whether it was effects that were waited for, tried with noise, and
    /// not an instrument with its notes.
    pub effect: bool,
    /// How long it took to answer, or how long it was waited for in vain.
    pub seconds: f64,
    /// Whether it answered. If not, the render went ahead without it.
    pub answered: bool,
    /// The pitches that made no sound to the end, though others did, and
    /// that were not known to be silent keys of this plugin: it was held
    /// to it for as long as the caller had patience for, and either does
    /// not play them or had not loaded them by then.
    pub silent: Vec<u8>,
}

/// One note of a rehearsal.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OpeningNote {
    pub pitch: u8,
    /// 0..=1.
    pub velocity: f32,
    /// Where the controllers stand for it, each on its channel, sent just
    /// ahead of it.
    pub controllers: Vec<(Channel, EventKind)>,
    /// It is a key held with the note after it (a keyswitch): pressed
    /// just before that note, let go with it, and not expected to sound.
    pub switch: bool,
    /// The channel the part plays it on: an instrument that gives its
    /// channels meanings is asked for the note it will be asked for.
    pub channel: Channel,
}

/// What a plugin is given to see whether it answers.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Opening {
    /// The notes an instrument is played.
    pub notes: Vec<OpeningNote>,
    /// For effects, which take no notes: a burst of noise to pass.
    pub signal: bool,
    /// The key the instrument is left on when the rehearsal is over: the
    /// one that chooses the way of playing the song begins in, where the
    /// rehearsal chose others and the song says nothing of it itself.
    pub left_on: Option<OpeningNote>,
}

impl Opening {
    /// The pitches that are expected to sound, in the order of the
    /// answers a rehearsal gives.
    fn pitches(&self) -> impl Iterator<Item = u8> + '_ {
        self.notes.iter().filter(|note| !note.switch).map(|note| note.pitch)
    }
}

/// Devices in a row, driven by hand outside any graph: events in at the
/// front, and what comes out of the last one listened to.
pub(crate) struct Stand<'a> {
    devices: Vec<&'a mut Box<dyn DeviceProcessor>>,
    /// How many of them, from the first, make the sound that the rest are
    /// given: an instrument that is rehearsed, with the effects after it.
    /// Those are run only from where the instrument sounds
    /// ([`Stand::engaged`]). None where every device is always run.
    sources: usize,
    /// Whether the instrument has sounded in the note that is being
    /// played, so that what comes after it is run; and whether it ever has.
    engaged: bool,
    ever_engaged: bool,
    /// Which of the devices listen to another track of the song, where
    /// any does. Here there is no other track: while a note is tried they
    /// are lent noise to listen to ([`Stand::lending`]), or an instrument
    /// that only plays what it hears would never be heard to answer.
    listening: Vec<bool>,
    /// Whether the key is being lent, the noise it is made of, and where
    /// that noise has got to.
    lent: bool,
    key: Vec<f32>,
    key_seed: u32,
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
            song: Default::default(),
            loop_range: None,
        };
        Self {
            devices,
            sources: 0,
            engaged: false,
            ever_engaged: false,
            listening: Vec::new(),
            lent: false,
            key: vec![0.0; BLOCK],
            key_seed: KEY_SEED,
            info,
            left: vec![0.0; BLOCK],
            right: vec![0.0; BLOCK],
            noise: 0.0,
        }
    }

    /// Says which of the devices listen to another track: one flag for
    /// each, in their order.
    pub fn listening(mut self, listening: Vec<bool>) -> Self {
        self.listening = listening;
        self
    }

    /// Lends the devices that listen a key from now on, or takes it away:
    /// the same noise from its beginning every time it is lent.
    pub fn lend(&mut self, lent: bool) {
        (self.lent, self.key_seed) = (lent && self.listening.contains(&true), KEY_SEED);
    }

    /// From here on the first device is an instrument that is rehearsed,
    /// and the devices after it are run only on the notes it sounds, from
    /// where it begins to: what they have been given by the end depends
    /// on what the instrument played, not on how often it was asked in
    /// vain. Until the stand is [settled](Self::settle).
    fn behind_an_instrument(&mut self) {
        self.sources = 1.min(self.devices.len());
    }

    pub fn sample_rate(&self) -> f64 {
        self.info.sample_rate
    }

    pub fn frames(&self, seconds: f64) -> usize {
        (seconds * self.info.sample_rate).ceil() as usize
    }

    /// How long one block lasts.
    fn block_seconds(&self) -> f64 {
        BLOCK as f64 / self.info.sample_rate
    }

    /// How late what goes in comes out, in seconds: a limiter that looks
    /// ahead answers a note that much after it.
    fn latency_seconds(&self) -> f64 {
        let frames: u32 = self.devices.iter().map(|device| device.latency_samples()).sum();
        f64::from(frames) / self.info.sample_rate
    }

    /// Runs the devices for `frames` frames, with `events` in the first
    /// block, and hands every block of what comes out to `hear`. Stops
    /// early, at the end of a block, once `hear` returns false.
    pub fn run_into(&mut self, events: &[Event], frames: usize, hear: impl FnMut(&[f32], &[f32]) -> bool) {
        self.run_through(events, frames, |left, right| (left.fill(0.0), right.fill(0.0)).0, hear);
    }

    /// [`Self::run_into`] with something going in: `feed` fills every
    /// block before the devices are given it.
    fn run_through(
        &mut self,
        events: &[Event],
        frames: usize,
        mut feed: impl FnMut(&mut [f32], &mut [f32]),
        mut hear: impl FnMut(&[f32], &[f32]) -> bool,
    ) {
        let mut done = 0;
        while done < frames {
            let count = (frames - done).min(BLOCK);
            let (left, right) = (&mut self.left[..count], &mut self.right[..count]);
            feed(left, right);
            let events = if done == 0 { events } else { &[] };
            // What the devices that listen are lent for this block.
            let key = &mut self.key[..count];
            if self.lent {
                for sample in key.iter_mut() {
                    self.key_seed ^= self.key_seed << 13;
                    self.key_seed ^= self.key_seed >> 17;
                    self.key_seed ^= self.key_seed << 5;
                    *sample = ((self.key_seed >> 8) as f32 / 8_388_608.0 - 1.0) * SIGNAL_LEVEL;
                }
            }
            let (info, lent, listening) = (&self.info, self.lent, &self.listening);
            // A device that listens is handed the key; any other is run
            // as it always is.
            let run =
                |index: usize, device: &mut Box<dyn DeviceProcessor>, left: &mut [f32], right: &mut [f32]| match lent
                    && listening.get(index).copied().unwrap_or(false)
                {
                    true => device.process_keyed(info, events, left, right, KeyInput { left: key, right: key }),
                    false => device.process(info, events, left, right),
                };
            let (sources, effects) = self.devices.split_at_mut(self.sources);
            let sourced = sources.len();
            for (index, device) in sources.iter_mut().enumerate() {
                run(index, device, left, right);
            }
            if sourced > 0 && !self.engaged {
                self.engaged = left.iter().chain(right.iter()).any(|sample| sample.abs() > ANSWER);
                self.ever_engaged |= self.engaged;
            }
            if sourced == 0 || self.engaged {
                for (index, device) in effects.iter_mut().enumerate() {
                    run(sourced + index, device, left, right);
                }
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

    /// Listens to the devices with nothing going in, to know their noise:
    /// all of them, for the same short stretch every time.
    pub fn hear_noise(&mut self) {
        let sources = std::mem::take(&mut self.sources);
        self.noise = self.run(&[], 0.05);
        self.sources = sources;
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
    /// and adds what came out to `answer`.
    fn hear(&mut self, events: &[Event], seconds: f64, answer: &mut Answer) {
        let frames = self.frames(seconds);
        self.run_into(events, frames, |left, right| {
            for sample in left.iter().chain(right) {
                answer.peak = answer.peak.max(sample.abs());
                answer.energy += f64::from(*sample) * f64::from(*sample);
            }
            true
        });
    }

    /// Puts noise through the devices for `seconds` and returns what came
    /// out. The noise is the same every time it is begun from `seed`.
    fn pass(&mut self, seed: &mut u32, seconds: f64) -> Answer {
        let frames = self.frames(seconds);
        let mut answer = Answer::default();
        let feed = |left: &mut [f32], right: &mut [f32]| {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                let sample = ((*seed >> 8) as f32 / 8_388_608.0 - 1.0) * SIGNAL_LEVEL;
                (*l, *r) = (sample, sample);
            }
        };
        self.run_through(&[], frames, feed, |left, right| {
            for sample in left.iter().chain(right) {
                answer.peak = answer.peak.max(sample.abs());
                answer.energy += f64::from(*sample) * f64::from(*sample);
            }
            true
        });
        answer
    }

    /// Puts a burst of noise through the devices, which is how an effect
    /// is asked: what came out while the noise went in, stretch by
    /// stretch, and after it.
    fn rehearse_noise(&mut self) -> Vec<Answer> {
        let mut seed = 0x2545_F491u32;
        let mut answers: Vec<Answer> = (0..SIGNAL_STRETCHES).map(|_| self.pass(&mut seed, NOTE_SECONDS)).collect();
        let mut after = Answer::default();
        self.hear(&[], AFTER_SECONDS, &mut after);
        answers.push(after);
        answers
    }

    /// Plays the notes of `opening` that are `asked` for, one at a time
    /// and each from silence, and returns what came out while each was
    /// down. `asked` and what is returned have a place for every note that
    /// is not a key held with another; a note that was not asked for is
    /// not played, and neither are the keys held with it.
    fn rehearse(&mut self, opening: &Opening, asked: &[bool]) -> Vec<Option<Answer>> {
        let floor = self.heard_above();
        // What comes out does so as late as the devices make it.
        let (held, block) = (NOTE_SECONDS + self.latency_seconds(), self.block_seconds());
        let mut answers = Vec::with_capacity(asked.len());
        let mut events: Vec<Event> = Vec::new();
        // The keys that go down with the next note, and what is down.
        let mut with: Vec<&OpeningNote> = Vec::new();
        let mut down: Vec<(Channel, u8)> = Vec::new();
        for note in &opening.notes {
            if note.switch {
                with.push(note);
                continue;
            }
            let wanted = asked.get(answers.len()).copied().unwrap_or(false);
            if !wanted {
                with.clear();
                answers.push(None);
                continue;
            }
            // What comes after the instrument waits for it to sound, note
            // by note.
            self.engaged = false;
            // An instrument that listens to another track is lent
            // something to listen to for as long as the note is down.
            self.lend(true);
            // A key held with the note goes down a moment ahead of it and
            // comes up with it.
            for key in with.drain(..) {
                events.extend(key.controllers.iter().map(|(channel, kind)| Event::on(*channel, 0, *kind)));
                let on = EventKind::NoteOn { pitch: key.pitch, velocity: key.velocity };
                events.push(Event::on(key.channel, 0, on));
                down.push((key.channel, key.pitch));
                self.run(&events, block);
                events.clear();
            }
            events.extend(note.controllers.iter().map(|(channel, kind)| Event::on(*channel, 0, *kind)));
            let on = EventKind::NoteOn { pitch: note.pitch, velocity: note.velocity };
            events.push(Event::on(note.channel, 0, on));
            down.push((note.channel, note.pitch));
            let mut answer = Answer::default();
            self.hear(&events, held, &mut answer);
            if answer.peak <= floor {
                // An instrument that speaks late is given longer.
                self.hear(&[], LATE_SECONDS, &mut answer);
            }
            answers.push(Some(answer));
            events.clear();
            events.extend(down.drain(..).map(|(channel, pitch)| Event::on(channel, 0, EventKind::NoteOff { pitch })));
            self.lend(false);
            self.hush(&events);
            events.clear();
        }
        answers
    }

    /// Lets go of what is down (`off`) and silences the devices, so that
    /// what is played next is heard by itself: they are reset, and what
    /// still rings after that is left a moment to die away.
    fn hush(&mut self, off: &[Event]) {
        let (quiet, block) = (self.heard_above(), self.block_seconds());
        self.run(off, block);
        self.reset();
        // Some devices act on a reset with their next block (a VST3 plugin
        // is told to be silent then).
        let mut waited = 0.0;
        while self.run(&[], block) > quiet && waited < HUSH_SECONDS {
            waited += block;
        }
    }

    /// Presses a key that chooses how notes are played and lets go of it:
    /// the instrument is left on that.
    pub fn choose(&mut self, key: &OpeningNote) {
        let block = self.block_seconds();
        let mut events: Vec<Event> =
            key.controllers.iter().map(|(channel, kind)| Event::on(*channel, 0, *kind)).collect();
        events.push(Event::on(key.channel, 0, EventKind::NoteOn { pitch: key.pitch, velocity: key.velocity }));
        self.run(&events, block);
        self.run(&[Event::on(key.channel, 0, EventKind::NoteOff { pitch: key.pitch })], block);
    }

    /// Lets go of every note, silences the devices, leaves what still
    /// rings to die away (not for ever), and resets them: they are as
    /// they were made, but awake.
    pub fn settle(&mut self) {
        // What the instrument never reached has nothing to let go of.
        self.engaged = self.ever_engaged;
        let quiet = self.heard_above() / 2.0;
        let needed = self.frames(QUIET_SECONDS);
        let limit = self.frames(RING_OUT_SECONDS);
        let block = self.block_seconds();
        // Where a reset silences a plugin, there is nothing to ring out.
        self.run(&[Event::new(0, EventKind::AllNotesOff)], block);
        self.reset();
        self.run(&[], block);
        let (mut done, mut silent, mut rang) = (0, 0, false);
        while done < limit && silent < needed {
            let peak = self.run(&[], block);
            (silent, rang) = if peak < quiet { (silent + BLOCK, rang) } else { (0, true) };
            done += BLOCK;
        }
        if rang {
            self.reset();
            // That reset's block is this one, not the first of what comes
            // after.
            self.run(&[], block);
        }
        // From here on every device is run, whatever the first one does.
        self.sources = 0;
    }

    /// Resets every device: voices silenced, delay lines cleared.
    pub fn reset(&mut self) {
        for device in &mut self.devices {
            device.reset();
        }
    }
}

/// What is remembered of plugins from one wait to the next, as it goes to
/// a process that waits for this one and comes back from it: without it,
/// such a process would wait out every key that plays nothing, and every
/// instrument that never answers, each time anew.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Memory {
    silent: Vec<u64>,
    ready: Vec<(u64, Vec<Answer>)>,
    keys: Vec<(u64, Vec<Option<bool>>)>,
}

/// What is remembered now.
pub(crate) fn remembered() -> Memory {
    Memory {
        silent: KNOWN_NOT_TO_ANSWER.lock().clone(),
        ready: KNOWN_READY.lock().clone(),
        keys: KNOWN_KEYS.lock().iter().map(|(devices, keys)| (*devices, keys.to_vec())).collect(),
    }
}

/// Takes up what another process remembers. What it has found ready is
/// no longer known not to answer here, what it found not to answer is no
/// longer known as ready, and what it knows of a key takes the place of
/// what was known of that key. Key by key, not plugin by plugin: two jobs
/// may run at once, each began from what was known when it started, and
/// what the first found of a key the second never tried must still be
/// known when the second comes back.
pub(crate) fn remember(memory: Memory) {
    let (mut silent, mut ready, mut keys) = (KNOWN_NOT_TO_ANSWER.lock(), KNOWN_READY.lock(), KNOWN_KEYS.lock());
    for (signature, answers) in memory.ready {
        silent.retain(|known| *known != signature);
        ready.retain(|(known, _)| *known != signature);
        ready.push((signature, answers));
    }
    for signature in memory.silent {
        if !ready.iter().any(|(known, _)| *known == signature) && !silent.contains(&signature) {
            silent.push(signature);
        }
    }
    for (devices, theirs) in memory.keys {
        let Ok(theirs) = Keys::try_from(theirs) else { continue };
        let at = keys.iter().position(|(other, _)| *other == devices);
        let mut known = at.map_or([None; 128], |at| keys.remove(at).1);
        for (here, there) in known.iter_mut().zip(theirs) {
            if there.is_some() {
                *here = there;
            }
        }
        keys.push((devices, known));
    }
    let surplus = silent.len().saturating_sub(REMEMBERED);
    silent.drain(..surplus);
    let surplus = ready.len().saturating_sub(REMEMBERED);
    ready.drain(..surplus);
    let surplus = keys.len().saturating_sub(REMEMBERED);
    keys.drain(..surplus);
}

/// What came out of a plugin over a short stretch: its highest sample, by
/// which it is known to have answered at all, and its energy, by which
/// one answer is compared with the next.
#[derive(Copy, Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Answer {
    peak: f32,
    energy: f64,
}

impl Answer {
    /// Whether this answer and `other` are one answer: both nothing, or
    /// both sound of about the same energy. `floor` is what counts as
    /// nothing.
    fn like(&self, other: &Answer, floor: f32) -> bool {
        match (self.peak > floor, other.peak > floor) {
            (false, false) => true,
            (true, true) => (10.0 * (self.energy / other.energy).log10()).abs() <= f64::from(SAME_ANSWER_DB),
            _ => false,
        }
    }
}

fn alike(now: &[Answer], before: &[Answer], floor: f32) -> bool {
    now.len() == before.len() && now.iter().zip(before).all(|(now, before)| now.like(before, floor))
}

/// What a wait is told of what is known already.
#[derive(Copy, Clone, Default)]
struct Known<'a> {
    /// How this was answered when it was last found ready.
    answers: Option<&'a [Answer]>,
    /// Which keys of these plugins are known to sound, and to be silent.
    keys: Option<&'a Keys>,
}

/// One plugin's wait: what it is asked, and how it has answered so far.
pub(crate) struct Wait<'a> {
    pub stand: Stand<'a>,
    pub opening: Opening,
    /// What is waited for, for remembering how it went.
    pub signature: u64,
    /// The plugins alone, whatever they are asked: for what is known of
    /// their keys.
    pub devices: u64,
    /// What the plugins take turns over with those of other waits: see
    /// [`crate::graph::DeviceSlot::turn`]. Waits that share a number here
    /// are never tried at once.
    turns: Vec<u32>,
    /// For an instrument: what came out each time a note sounded, note by
    /// note, [`SOUNDINGS`] times each at the most.
    sounded: Vec<Vec<Answer>>,
    /// For how many tries in a row a note that has sounded has stayed
    /// silent since.
    missed: Vec<u32>,
    /// How many tries there have been.
    tries: u32,
    /// For effects: what came out of each burst of noise that was passed
    /// at all, [`SOUNDINGS`] of them at the most.
    bursts: Vec<Vec<Answer>>,
    /// Since when nothing has been loading and nothing has sounded that
    /// had not before.
    steady_since: Option<Instant>,
    /// Whether the program showed signs of loading while this went on.
    saw_loading: bool,
    /// How long after the wait began the first answer came.
    first_answer: Option<Duration>,
    /// When there was last a sign of life: a note that sounded and was
    /// still to, an effect that answered differently, the program loading.
    stirred: Instant,
    /// When it was found ready, or given up on.
    outcome: Option<(Duration, bool)>,
    /// It was found ready, and not given up on.
    found: bool,
    /// The pitches that have made no sound at any try so far, though
    /// others have, and are not known to be silent keys.
    silent: Vec<u8>,
}

impl<'a> Wait<'a> {
    pub fn new(mut stand: Stand<'a>, opening: Opening, signature: u64, devices: u64) -> Self {
        if !opening.signal {
            stand.behind_an_instrument();
        }
        Self {
            stand,
            opening,
            signature,
            devices,
            turns: Vec::new(),
            sounded: Vec::new(),
            missed: Vec::new(),
            tries: 0,
            bursts: Vec::new(),
            steady_since: None,
            saw_loading: false,
            first_answer: None,
            stirred: Instant::now(),
            outcome: None,
            found: false,
            silent: Vec::new(),
        }
    }

    /// Says what the plugins of this wait take turns over with those of
    /// others.
    pub fn taking_turns(mut self, turns: Vec<u32>) -> Self {
        self.turns = turns;
        self
    }

    /// One try. `loading` is whether the program showed signs of loading
    /// since the last one; `partly` how long an instrument that answers
    /// only some of its notes is held to that. Returns whether the wait
    /// is over.
    fn attempt(&mut self, begun: Instant, loading: bool, known: Known, partly: Duration) -> bool {
        if loading {
            (self.stirred, self.saw_loading) = (Instant::now(), true);
        }
        match self.opening.signal {
            true => self.attempt_noise(begun, loading, known),
            false => self.attempt_notes(begun, loading, known, partly),
        }
    }

    /// A try of an instrument: every note that is still to sound is
    /// played once.
    fn attempt_notes(&mut self, begun: Instant, loading: bool, known: Known, partly: Duration) -> bool {
        let floor = self.stand.heard_above();
        let now = Instant::now();
        let pitches: Vec<u8> = self.opening.pitches().collect();
        self.sounded.resize(pitches.len(), Vec::new());
        self.missed.resize(pitches.len(), 0);
        let expected_silent = |pitch: u8| known.keys.is_some_and(|keys| keys[usize::from(pitch)] == Some(false));
        // A note is done with when it has sounded as often as it is to.
        // One that has sounded and then stopped is asked on, without the
        // wait hanging on it; one that has never sounded is asked on too,
        // unless that is what is known of it, and then only while there
        // are others to ask.
        let done = |heard: &Vec<Answer>| heard.len() >= SOUNDINGS;
        let stalled = |heard: &Vec<Answer>, missed: u32| !heard.is_empty() && !done(heard) && missed >= STALLED_TRIES;
        let others = self
            .sounded
            .iter()
            .zip(&self.missed)
            .any(|(heard, missed)| !heard.is_empty() && !done(heard) && !stalled(heard, *missed));
        let asked: Vec<bool> = (pitches.iter().zip(&self.sounded))
            .map(|(pitch, heard)| !done(heard) && (!expected_silent(*pitch) || others || self.tries == 0))
            .collect();
        // With nothing left to ask, the plugins are not run either: how
        // much they have been given by the time a render begins is then a
        // matter of how often their notes sounded and not of how long
        // this took, which a tail that a reset does not cut would show.
        let answers = match asked.contains(&true) {
            true => self.stand.rehearse(&self.opening, &asked),
            false => vec![None; asked.len()],
        };
        self.tries += 1;
        // Whether a note sounded that was still to, and whether one did
        // that had not before, or had stopped: something has arrived.
        let (mut progressed, mut arrived) = (false, false);
        for ((heard, missed), answer) in self.sounded.iter_mut().zip(&mut self.missed).zip(answers) {
            match answer {
                Some(answer) if answer.peak > floor => {
                    arrived |= heard.is_empty() || *missed > 0;
                    heard.push(answer);
                    (*missed, progressed) = (0, true);
                }
                Some(_) if !heard.is_empty() => *missed += 1,
                _ => {}
            }
        }
        if self.sounded.iter().all(Vec::is_empty) {
            self.steady_since = None;
            self.silent.clear();
            return false;
        }
        let first = *self.first_answer.get_or_insert(begun.elapsed());
        if progressed {
            self.stirred = now;
        }
        // The notes that have made no sound yet, unless that is what is
        // known of them: the instrument may not have loaded them.
        self.silent.clear();
        for (pitch, heard) in pitches.iter().zip(&self.sounded) {
            if heard.is_empty() && !expected_silent(*pitch) && !self.silent.contains(pitch) {
                self.silent.push(*pitch);
            }
        }
        let waiting =
            |(heard, missed): (&Vec<Answer>, &u32)| !heard.is_empty() && !done(heard) && !stalled(heard, *missed);
        let complete = !self.sounded.iter().zip(&self.missed).any(waiting);
        let whole = self.silent.is_empty()
            && !self.sounded.iter().zip(&self.missed).any(|(heard, missed)| stalled(heard, *missed));
        if loading || arrived || self.steady_since.is_none() {
            self.steady_since = Some(now);
        }
        let steady = self.steady_since.map_or(Duration::ZERO, |since| now.duration_since(since));
        // For how long nothing may have arrived or been loading before it
        // is taken as ready: a moment if its notes sound as they did when
        // it was last found ready; longer if it was slow to answer at all,
        // or did not answer a note the same way each time.
        let as_before = known.answers.is_some_and(|before| {
            before.len() == self.sounded.len()
                && before.iter().zip(&self.sounded).all(|(before, heard)| (before.peak > floor) != heard.is_empty())
        });
        let slow = (first / 2).clamp(SETTLED_FOR, SETTLED_AT_MOST);
        let level = self.sounded.iter().all(|heard| heard.windows(2).all(|pair| pair[0].like(&pair[1], floor)));
        let hold = match (as_before && !self.saw_loading, level) {
            (true, _) => BETWEEN_TRIES,
            (false, true) => slow,
            (false, false) => slow.max(UNSETTLED_FOR),
        };
        // And one that leaves notes unanswered may be between two parts of
        // what it loads.
        if complete && !loading && steady >= hold && (whole || steady >= partly) {
            (self.outcome, self.found) = (Some((begun.elapsed(), true)), true);
        }
        self.outcome.is_some()
    }

    /// A try of effects: a burst of noise is put through them, unless they
    /// have passed as many as they are to.
    fn attempt_noise(&mut self, begun: Instant, loading: bool, known: Known) -> bool {
        let floor = self.stand.heard_above();
        let now = Instant::now();
        let mut arrived = false;
        if self.bursts.len() < SOUNDINGS {
            let answers = self.stand.rehearse_noise();
            self.tries += 1;
            if answers.iter().any(|answer| answer.peak > floor) {
                arrived = self.bursts.is_empty();
                self.bursts.push(answers);
                self.stirred = now;
            }
        }
        if self.bursts.is_empty() {
            self.steady_since = None;
            return false;
        }
        let first = *self.first_answer.get_or_insert(begun.elapsed());
        if loading || arrived || self.steady_since.is_none() {
            self.steady_since = Some(now);
        }
        let steady = self.steady_since.map_or(Duration::ZERO, |since| now.duration_since(since));
        // As with notes: a moment for what passes its bursts as it did
        // when it was last found ready, longer for what was slow to pass
        // one at all, or never passes one the same way twice.
        let as_before = known.answers.is_some_and(|before| self.bursts.iter().all(|burst| alike(burst, before, floor)));
        let slow = (first / 2).clamp(SETTLED_FOR, SETTLED_AT_MOST);
        let level = self.bursts.windows(2).all(|pair| alike(&pair[0], &pair[1], floor));
        let hold = match (as_before && !self.saw_loading, level) {
            (true, _) => BETWEEN_TRIES,
            (false, true) => slow,
            (false, false) => slow.max(UNSETTLED_FOR),
        };
        if self.bursts.len() >= SOUNDINGS && !loading && steady >= hold {
            (self.outcome, self.found) = (Some((begun.elapsed(), true)), true);
        }
        self.outcome.is_some()
    }

    /// Whether anything has come out at all.
    fn answered(&self) -> bool {
        !self.bursts.is_empty() || self.sounded.iter().any(|heard| !heard.is_empty())
    }

    /// What to remember of how it answered, to know it by when it is
    /// waited for again: an effect's last answers; for an instrument, the
    /// last sound of each note, or nothing for a note that made none.
    fn remembered(&mut self) -> Vec<Answer> {
        match self.opening.signal {
            true => self.bursts.pop().unwrap_or_default(),
            false => self.sounded.iter().map(|heard| heard.last().copied().unwrap_or_default()).collect(),
        }
    }

    /// How the wait went: how long it took, and whether it was answered.
    pub fn outcome(&self) -> (Duration, bool) {
        self.outcome.unwrap_or_default()
    }

    /// The pitches that made no sound to the end, though others did, and
    /// were not known to be silent keys: see [`Waited::silent`].
    pub fn silent(&self) -> &[u8] {
        &self.silent
    }
}

/// Says every try of a wait to whoever logs at the finest level: how the
/// program stood and how each wait was answered. The `loading` example of
/// `trill-plugins` prints it.
fn tell(begun: Instant, first: &Vitals, loading: bool, waits: &[Wait]) {
    if !log::log_enabled!(log::Level::Trace) {
        return;
    }
    let now = Vitals::now().unwrap_or_default();
    let megabytes = |bytes: u64| bytes as f64 / 1_048_576.0;
    let answers: Vec<String> = waits
        .iter()
        .map(|wait| {
            let floor = wait.stand.heard_above();
            let level = |answer: &Answer| match answer.peak > floor {
                true => format!("{:.0}", 20.0 * answer.peak.log10()),
                false => "-".to_string(),
            };
            match wait.opening.signal {
                true => {
                    let last = wait.bursts.last().map(Vec::as_slice).unwrap_or_default();
                    let said: Vec<String> = last.iter().map(level).collect();
                    format!("{} bursts [{}]", wait.bursts.len(), said.join(" "))
                }
                // How often each note has sounded, and how loud the last time.
                false => {
                    let said = wait.sounded.iter().map(|heard| match heard.last() {
                        Some(last) => format!("{}x{}", heard.len(), level(last)),
                        None => "-".to_string(),
                    });
                    format!("[{}]", said.collect::<Vec<_>>().join(" "))
                }
            }
        })
        .collect();
    log::trace!(
        "wait {:>6.2} s  in use {:>7.1} MB  asked {:>7.1} MB  read {:>8.1} MB  {}  {}",
        begun.elapsed().as_secs_f64(),
        megabytes(now.in_use),
        megabytes(now.asked),
        megabytes(now.read.saturating_sub(first.read)),
        if loading { "loading" } else { "still  " },
        answers.join(" | ")
    );
}

/// Tries every wait in turn until each is over, then settles them all.
/// A wait is given up on when nothing has stirred for `patience` (no
/// answer that differs from the last, the program not loading, and
/// nothing holding the plugins up: `held_up` says whether their host is
/// behind with other work), or after [`LOADING_LIMIT`] whatever stirs.
/// Waits that are known not to be answered get [`KNOWN_SILENT`] at most;
/// how each went is remembered.
pub(crate) fn wait_for(waits: &mut [Wait], patience: Duration, held_up: &(dyn Fn() -> bool + Sync), threads: usize) {
    let begun = Instant::now();
    let silent: Vec<u64> = KNOWN_NOT_TO_ANSWER.lock().clone();
    let ready: Vec<(u64, Vec<Answer>)> = KNOWN_READY.lock().clone();
    let keys: Vec<(u64, Keys)> = KNOWN_KEYS.lock().clone();
    let mut signs = Signs::following(UNLOADED_UNDER_A_WAIT);
    let first = Vitals::now().unwrap_or_default();
    let crew = crew_for(waits, threads);
    each(waits, crew.as_ref(), &|wait| {
        wait.stand.hear_noise();
        wait.stirred = Instant::now();
    });
    loop {
        let loading = signs.seen();
        // A plugin that cannot have its turn is not one that does nothing.
        let waiting_its_turn = held_up();
        tell(begun, &first, loading, waits);
        let pending = AtomicBool::new(false);
        each(waits, crew.as_ref(), &|wait| {
            if wait.outcome.is_some() {
                return;
            }
            let patience = if wait.opening.signal { patience.min(EFFECT_PATIENCE) } else { patience };
            let patience = if silent.contains(&wait.signature) { patience.min(KNOWN_SILENT) } else { patience };
            let known = Known {
                answers: ready.iter().find(|(signature, _)| *signature == wait.signature).map(|(_, a)| a.as_slice()),
                keys: keys.iter().find(|(devices, _)| *devices == wait.devices).map(|(_, keys)| keys),
            };
            if wait.attempt(begun, loading, known, patience / 2) {
                return;
            }
            if waiting_its_turn {
                wait.stirred = Instant::now();
            }
            // Given up on: nothing has happened for as long as the caller
            // has patience, or it has gone on for too long altogether. A
            // wait that was answered is then taken as it is; one that
            // never was went unanswered.
            if wait.stirred.elapsed() >= patience || begun.elapsed() >= LOADING_LIMIT {
                wait.outcome = Some((begun.elapsed(), wait.answered()));
            } else {
                pending.store(true, Ordering::Relaxed);
            }
        });
        if !pending.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(BETWEEN_TRIES);
    }
    each(waits, crew.as_ref(), &|wait| {
        if let Some(key) = &wait.opening.left_on {
            wait.stand.choose(key);
        }
        wait.stand.settle();
    });
    drop(crew);
    let (mut silent, mut ready, mut keys) = (KNOWN_NOT_TO_ANSWER.lock(), KNOWN_READY.lock(), KNOWN_KEYS.lock());
    for wait in waits.iter_mut() {
        silent.retain(|signature| *signature != wait.signature);
        ready.retain(|(signature, _)| *signature != wait.signature);
        match wait.outcome().1 {
            true => {
                // What is now known of these plugins' keys: the ones that
                // sounded do, and the ones that stayed silent for as long
                // as it was held to them are keys that play nothing.
                if !wait.opening.signal {
                    let at = keys.iter().position(|(devices, _)| *devices == wait.devices);
                    let mut known = at.map_or([None; 128], |at| keys.remove(at).1);
                    for (pitch, heard) in wait.opening.pitches().zip(&wait.sounded) {
                        match heard.is_empty() {
                            false => known[usize::from(pitch)] = Some(true),
                            true if wait.found => known[usize::from(pitch)] = Some(false),
                            true => {}
                        }
                    }
                    keys.push((wait.devices, known));
                }
                let answers = wait.remembered();
                ready.push((wait.signature, answers));
            }
            false => silent.push(wait.signature),
        }
    }
    let surplus = silent.len().saturating_sub(REMEMBERED);
    silent.drain(..surplus);
    let surplus = ready.len().saturating_sub(REMEMBERED);
    ready.drain(..surplus);
    let surplus = keys.len().saturating_sub(REMEMBERED);
    keys.drain(..surplus);
}

/// The threads to try `waits` on, and which of them each list of waits
/// that take turns holds, by their places: nothing where one thread is
/// all there is to use, or all it was given.
fn crew_for(waits: &[Wait], threads: usize) -> Option<(Crew, Vec<Vec<usize>>)> {
    let mut lists: Vec<(Vec<usize>, Vec<u32>)> = Vec::new();
    for (at, wait) in waits.iter().enumerate() {
        let mut joined = (vec![at], wait.turns.clone());
        let mut apart = Vec::new();
        for list in lists.drain(..) {
            if list.1.iter().any(|turn| joined.1.contains(turn)) {
                joined.0.extend(list.0);
                joined.1.extend(list.1);
            } else {
                apart.push(list);
            }
        }
        joined.0.sort_unstable();
        apart.push(joined);
        lists = apart;
    }
    lists.sort_by_key(|list| list.0[0]);
    let threads = threads.min(lists.len());
    if threads < 2 {
        return None;
    }
    let crew = Crew::new(threads - 1, || ());
    (crew.len() > 0).then(|| (crew, lists.into_iter().map(|list| list.0).collect()))
}

/// Does `work` to every wait: one after the other, or, given threads,
/// each list of waits at once with the others and in its own order.
fn each<'a>(waits: &mut [Wait<'a>], crew: Option<&(Crew, Vec<Vec<usize>>)>, work: &(dyn Fn(&mut Wait<'a>) + Sync)) {
    let Some((crew, lists)) = crew else { return waits.iter_mut().for_each(work) };
    let mut places: Vec<Option<&mut Wait<'a>>> = waits.iter_mut().map(Some).collect();
    let lists: Vec<Mutex<Vec<&mut Wait<'a>>>> =
        lists.iter().map(|list| Mutex::new(list.iter().filter_map(|at| places[*at].take()).collect())).collect();
    let next = AtomicUsize::new(0);
    let all = |_: usize| {
        while let Some(list) = lists.get(next.fetch_add(1, Ordering::Relaxed)) {
            list.lock().iter_mut().for_each(|wait| work(wait));
        }
    };
    crew.all(lists.len() - 1, &all);
}

fn hash_devices<'a>(devices: impl Iterator<Item = &'a Device>, hasher: &mut impl Hasher) {
    for device in devices {
        device.uri.hash(hasher);
        device.state.hash(hasher);
        device.preset.as_ref().map(|preset| &preset.id).hash(hasher);
        for (key, value) in &device.params {
            key.hash(hasher);
            value.to_bits().hash(hasher);
        }
    }
}

/// What identifies plugins whatever they are asked: what they are, and
/// their saved states.
pub(crate) fn device_signature<'a>(devices: impl Iterator<Item = &'a Device>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_devices(devices, &mut hasher);
    hasher.finish()
}

/// What identifies a wait: the plugins, their saved states, and what they
/// are asked.
pub(crate) fn signature<'a>(devices: impl Iterator<Item = &'a Device>, opening: &Opening) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_devices(devices, &mut hasher);
    opening.signal.hash(&mut hasher);
    opening.left_on.as_ref().map(|key| key.pitch).hash(&mut hasher);
    for note in &opening.notes {
        (note.pitch, note.velocity.to_bits(), note.switch).hash(&mut hasher);
        // (A channel only where it is not the first, so that what was
        // remembered of a wait before notes had channels still stands.)
        if !note.channel.is_first() {
            ("channel", note.channel.number()).hash(&mut hasher);
        }
        for (channel, kind) in &note.controllers {
            if let Some((controller, value)) = ControllerKind::of_event(*kind) {
                (controller.index(), value.to_bits()).hash(&mut hasher);
                if !channel.is_first() {
                    ("channel", channel.number()).hash(&mut hasher);
                }
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

    pub fn is_instrument(&self, uri: &str) -> bool {
        self.instruments.iter().any(|instrument| instrument == uri)
    }
}

/// Sees to it that the plugins of `graph` are ready before it is played:
/// every plugin instrument that has notes to play between the beats
/// `from` and `to`, with the plugins after it, and every other plugin, as
/// an effect. Waits up to `patience` for those that are not. See the
/// module documentation.
pub(crate) fn wake(
    graph: &mut Graph,
    project: &Project,
    (plugins, factory): (&Plugins, &dyn DeviceFactory),
    (from, to): (f64, f64),
    sample_rate: f64,
    (patience, threads): (Duration, usize),
) -> Vec<Waited> {
    // The instruments are tried at the tempo the render starts in.
    let tempo = graph.tempo_map.tempo_at(from);
    let mut waits = Vec::new();
    let mut who = Vec::new();
    for node in &mut graph.tracks {
        let Ok(track) = project.track(node.id) else { continue };
        // What plays the track's notes: its first device that is not an
        // effect. Only if that is a plugin is there an instrument to wait
        // for, and then only if it has notes to play.
        let sounding = track.devices.iter().filter(|device| device.enabled);
        let instrument = sounding
            .clone()
            .find(|device| trill_dsp::is_instrument(&device.uri) != Some(false))
            .filter(|instrument| plugins.is_instrument(&instrument.uri));
        let mut notes: Vec<OpeningNote> = Vec::new();
        let opening_notes = node.notes.iter().filter(|note| note.start >= from - 1e-9 && note.start < to);
        // The keys the track knows for switches choose how the notes after
        // them are played and sound nothing themselves. They are not
        // rehearsed as notes; each note is rehearsed under the switch the
        // part plays it with: the one that stays chosen where the part has
        // got to, or one that is for the next note only.
        let articulation = |pitch: u8| track.profile.articulations.iter().find(|way| way.key == Some(pitch));
        // (A key switches on the first channel only: see `graph::switches_of`.)
        let switching = |note: &crate::graph::NoteEvent| articulation(note.pitch).filter(|_| note.channel.is_first());
        // What the part chose before the render begins is chosen where it
        // begins: the song presses it again there, and so does this.
        let [chosen, pending] = crate::graph::chosen_at(&node.switches, from).map(|switch| {
            switch.map(|switch| OpeningNote {
                pitch: switch.pitch,
                velocity: f32::from(switch.velocity.clamp(1, 127)) / 127.0,
                controllers: Vec::new(),
                switch: true,
                channel: Channel::FIRST,
            })
        });
        let stayed_before = chosen.is_some();
        let (mut stays, mut once): (Option<OpeningNote>, Option<OpeningNote>) = (chosen, pending);
        // Which notes are rehearsed: a pitch under each switch it is first
        // played with, which are other recordings.
        let mut rehearsing: Vec<(Option<u8>, Channel, u8)> = Vec::new();
        // The way a note is played when nothing is said, where the track
        // says which key chooses it: what the instrument is taken to be on
        // as it is made.
        let unsaid = track.profile.articulations.iter().find(|way| way.default && way.key.is_some());
        let unsaid = unsaid.and_then(|way| way.key).map(|pitch| OpeningNote {
            pitch,
            velocity: 100.0 / 127.0,
            controllers: Vec::new(),
            switch: true,
            channel: Channel::FIRST,
        });
        // Whether the part begins on what nothing was said of, and whether
        // a switch that stays is pressed at all.
        let (mut begins_unsaid, mut stayed) = (false, stayed_before);
        for note in opening_notes.filter(|_| instrument.is_some()) {
            let switches = switching(note).map(|way| way.lasts);
            if switches.is_none() {
                if rehearsing.len() == OPENING_NOTES {
                    break;
                }
                let under = once.as_ref().or(stays.as_ref()).map(|key| key.pitch);
                if rehearsing.contains(&(under, note.channel, note.pitch)) {
                    // It is played again, and what was for one note is over.
                    once = None;
                    continue;
                }
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
                    (lane.channel, lane.kind.event(value as f32))
                })
                .collect();
            let velocity = f32::from(note.velocity.clamp(1, 127)) / 127.0;
            let (pitch, channel) = (note.pitch, note.channel);
            match switches {
                Some(trill_core::Lasts::Stays) => {
                    stays = Some(OpeningNote { pitch, velocity, controllers, switch: true, channel });
                    (once, stayed) = (None, true);
                }
                Some(_) => once = Some(OpeningNote { pitch, velocity, controllers, switch: true, channel }),
                None => {
                    let under = once.take().or_else(|| stays.clone());
                    begins_unsaid |= rehearsing.is_empty() && stays.is_none();
                    rehearsing.push((under.as_ref().map(|key| key.pitch), channel, pitch));
                    // A note nothing was said of is one on the way the
                    // instrument plays unasked: by then another may be
                    // chosen, here as in the part.
                    notes.extend(under.or_else(|| unsaid.clone().filter(|_| stays.is_none())));
                    notes.push(OpeningNote { pitch, velocity, controllers, switch: false, channel });
                }
            }
        }
        // Without a switch that stays, nothing is chosen that the
        // instrument would not be on anyway: it is asked as it always was.
        if !stayed {
            notes.retain(|note| !note.switch || Some(note.pitch) != unsaid.as_ref().map(|key| key.pitch));
        }
        // What the instrument is left on: where the part begins on what
        // nothing was said of, that, if a rehearsal chose anything else.
        let left_on = unsaid.filter(|_| begins_unsaid && stayed);
        // The instrument and the plugins after it are what a note goes
        // through before it is heard, the built-in devices left out. With
        // no notes to play, there is nothing to rehearse them with.
        let rehearsed: Vec<&Device> = match instrument.filter(|_| !notes.is_empty()) {
            Some(instrument) => sounding
                .clone()
                .skip_while(|device| device.id != instrument.id)
                .filter(|device| Plugins::is_plugin(&device.uri))
                .collect(),
            None => Vec::new(),
        };
        // Every other plugin of the track is tried as an effect.
        let effects: Vec<&Device> = sounding
            .filter(|device| Plugins::is_plugin(&device.uri) && !rehearsed.iter().any(|other| other.id == device.id))
            .collect();
        let (mut with_notes, mut with_noise) = (Vec::new(), Vec::new());
        let (mut note_turns, mut noise_turns) = (Vec::new(), Vec::new());
        // Which of the rehearsed devices listen to another track.
        let mut listening = Vec::new();
        for slot in node.chain.devices.iter_mut() {
            let (id, turn, listens) = (slot.id, slot.turn, slot.key.is_some());
            let Some(processor) = slot.processor.as_mut() else { continue };
            if rehearsed.iter().any(|device| device.id == id) {
                with_notes.push(processor);
                listening.push(listens);
                note_turns.extend(turn);
            } else if effects.iter().any(|device| device.id == id) {
                with_noise.push(processor);
                noise_turns.extend(turn);
            }
        }
        // (A plugin that could not be created is a problem of its own,
        // and what is left of its chain is not rehearsed without it.)
        if let (Some(instrument), true) = (instrument, !rehearsed.is_empty() && with_notes.len() == rehearsed.len()) {
            let opening = Opening { notes, signal: false, left_on };
            let signature = signature(rehearsed.iter().copied(), &opening);
            let devices = device_signature(rehearsed.iter().copied());
            let stand = Stand::new(with_notes, sample_rate, tempo).listening(listening);
            waits.push(Wait::new(stand, opening, signature, devices).taking_turns(note_turns));
            who.push((Some(node.id), instrument.id, false));
        }
        if let (Some(first), true) = (effects.first(), with_noise.len() == effects.len()) {
            let opening = Opening { notes: Vec::new(), signal: true, left_on: None };
            let signature = signature(effects.iter().copied(), &opening);
            let devices = device_signature(effects.iter().copied());
            let stand = Stand::new(with_noise, sample_rate, tempo);
            waits.push(Wait::new(stand, opening, signature, devices).taking_turns(noise_turns));
            who.push((Some(node.id), first.id, true));
        }
    }
    // The master has no notes: its plugins are effects.
    let effects: Vec<&Device> =
        project.master.devices.iter().filter(|device| device.enabled && Plugins::is_plugin(&device.uri)).collect();
    let slots = graph.master.chain.devices.iter_mut().filter(|slot| effects.iter().any(|device| device.id == slot.id));
    let slots: Vec<_> = slots.filter(|slot| slot.processor.is_some()).collect();
    let master_turns: Vec<u32> = slots.iter().filter_map(|slot| slot.turn).collect();
    let processors: Vec<_> = slots.into_iter().filter_map(|slot| slot.processor.as_mut()).collect();
    if let (Some(first), true) = (effects.first(), processors.len() == effects.len()) {
        let opening = Opening { notes: Vec::new(), signal: true, left_on: None };
        let signature = signature(effects.iter().copied(), &opening);
        let devices = device_signature(effects.iter().copied());
        let stand = Stand::new(processors, sample_rate, tempo);
        waits.push(Wait::new(stand, opening, signature, devices).taking_turns(master_turns));
        who.push((None, first.id, true));
    }
    if waits.is_empty() {
        return Vec::new();
    }
    wait_for(&mut waits, patience, &|| factory.busy(), threads);
    waits
        .iter()
        .zip(who)
        .map(|(wait, (track, device, effect))| {
            let (waited, answered) = wait.outcome();
            let silent = wait.silent().to_vec();
            Waited { track, device, effect, seconds: waited.as_secs_f64(), answered, silent }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_is_said_to_be_loading_until_the_program_has_stood_still_for_a_while() {
        let watch = LoadWatch::settling_after(Duration::from_millis(400));
        assert!(watch.loading().is_empty());
        watch.began(DeviceId(7));
        assert_eq!(watch.loading(), [DeviceId(7)]);
        // While the program grows as it does when samples are read, it
        // goes on being said. (Only Windows, macOS and Linux can be asked.)
        #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
        {
            let mut held = Vec::new();
            let begun = Instant::now();
            while begun.elapsed() < Duration::from_millis(700) {
                held.push(vec![1u8; 8 << 20]);
                std::thread::sleep(Duration::from_millis(40));
                assert_eq!(watch.loading(), [DeviceId(7)], "after {:?}", begun.elapsed());
            }
        }
        // And for a while after, a pause not being the end; then no more.
        let stopped = Instant::now();
        while !watch.loading().is_empty() {
            assert!(stopped.elapsed() < Duration::from_secs(20), "it never came to an end");
            std::thread::sleep(Duration::from_millis(50));
        }
        #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
        assert!(stopped.elapsed() >= Duration::from_millis(200), "{:?}", stopped.elapsed());
        // One that began and is gone again is not loading.
        watch.began(DeviceId(8));
        watch.gone(DeviceId(8));
        assert!(watch.loading().is_empty() && watch.loading_now().is_empty());
    }

    #[test]
    fn what_two_processes_learned_of_one_plugin_at_the_same_time_is_all_kept() {
        // Two jobs on the same plugin began from the same knowledge. The
        // first comes back having found a key that plays nothing; the
        // second, which never tried that key, comes back after it.
        let plugin = 0x7e57_ab1e_0000_0001; // no plugin has this signature
        let learned = |keys: &[(usize, bool)]| {
            let mut known = vec![None; 128];
            for (key, sounds) in keys {
                known[*key] = Some(*sounds);
            }
            Memory { keys: vec![(plugin, known)], ..Memory::default() }
        };
        remember(learned(&[(20, false), (41, true)]));
        remember(learned(&[(44, true)]));
        let of = |memory: Memory| memory.keys.into_iter().find(|(devices, _)| *devices == plugin).unwrap().1;
        let known = of(remembered());
        assert_eq!((known[20], known[41], known[44], known[45]), (Some(false), Some(true), Some(true), None));
        // What a later one found of a key takes the place of what was known.
        remember(learned(&[(20, true)]));
        assert_eq!(of(remembered())[20], Some(true));
    }
}
