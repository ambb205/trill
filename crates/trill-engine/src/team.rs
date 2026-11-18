//! The threads a song that plays is made on, beside the audio callback's.
//!
//! One thread cannot always make a block in the time a block lasts: a song
//! of nineteen plugins was measured at more than the whole of one
//! processor on a machine that has sixteen. The tracks of a block that do
//! not feed one another can be made at once ([`crate::spread`] says which
//! and in what order), and this is what they are handed to while a song
//! plays: a few threads that are made once, off the audio thread, and
//! from then on are audio threads themselves.
//!
//! **What an audio thread may do, here.** None of these threads, and not
//! the callback that hands them work, ever allocates or takes a lock. Work
//! is handed over in atomics: a pointer to it, and one word that counts
//! the places still free and the threads still at it, so that taking a
//! place and being counted are one step. The callback does not sleep
//! either. It makes tracks itself like any of the others, taking the next
//! share for as long as there is one, and when none is left it waits for
//! the shares that others have taken by looking again and again, which a
//! thread of its priority cannot be kept from doing by anything of a
//! lower one; and where a share has become ready meanwhile that nobody
//! has come for, it takes that. What it waits for is a thread of its own
//! class in the middle of a track: at the most as long as the longest
//! track takes.
//!
//! **The others do sleep, between blocks.** A thread with nothing to do
//! looks for work for a moment longer (the rounds of one block follow each
//! other within microseconds) and then parks itself. The callback wakes it
//! with the next block, which is one call to the system and blocks
//! nobody: a thread that wakes late finds the work taken, by the callback
//! if by nobody else, and goes back to sleep. How late they were is
//! counted ([`Watch`]).
//!
//! **Work that becomes ready in the middle of a round** (what comes after
//! a track's turn among the devices that take turns: see
//! [`crate::spread`]) is said by whoever made it ready
//! ([`Workers::more`]): one more place, and one thread that sleeps is
//! woken for it. Nobody looks round and round for work that is not there
//! yet. A thread says that it sleeps before it looks for a place one last
//! time, and whoever frees a place looks for a sleeper after: one of the
//! two sees the other, so no place is left with everyone asleep.
//!
//! **Their priority is the callback's.** Each joins the class the system
//! keeps for audio as it starts ([`AudioPriority`]). Without it the system
//! takes such a thread off its processor in the middle of a track
//! whenever anything else wants a turn, and the callback, which has
//! everything else of the block, waits for it.
//!
//! **A device that never comes back** from its `process` holds whoever
//! runs it for good, as it always did. The callback then stops looking
//! and sleeps a millisecond at a time: the block was lost long before, and
//! a processor is not to be burnt for it. The engine finds the stream
//! stuck and starts over without it, as before.
//!
//! A render is not played and has no speaker to keep up with: its threads
//! are [`crate::crew`]'s, which sleep on a lock like any others.

use crate::clock::now_ns;
use crate::host::priority::AudioPriority;
use crate::processor::DenormalGuard;
use crate::spread::Workers;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::thread::JoinHandle;

/// The low half of [`Shared::state`]: how many more threads may take up
/// the task at hand. The high half counts those that have and are not
/// back.
const PLACES: u64 = 0xFFFF_FFFF;
const ONE_AT_WORK: u64 = 1 << 32;
/// How long a thread with nothing to do goes on looking for work before
/// it parks itself, in nanoseconds: longer than the rounds of one block
/// are apart, and far shorter than a block.
const LINGER_NS: u64 = 40_000;
/// How long the callback looks again and again for the threads that are
/// still at a share before it takes them to be stuck in a device and
/// sleeps between looks, in nanoseconds: several blocks of any size.
const STUCK_NS: u64 = 100_000_000;

/// Threads that work on what [`Team::all`] is given, beside the caller,
/// without anyone allocating or taking a lock: see the module.
pub struct Team {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

struct Shared {
    /// See [`PLACES`].
    state: AtomicU64,
    /// The work at hand: something on the stack of [`Team::all`], which
    /// does not return while a thread is counted at work.
    task: AtomicPtr<Task>,
    /// A thread panicked over the task at hand.
    panicked: AtomicBool,
    quit: AtomicBool,
    /// For every thread of the team, by its place among them: that it has
    /// parked itself, or is about to once it has looked for a place one
    /// last time.
    asleep: Box<[AtomicBool]>,
    told: Told,
}

/// What every thread that takes a place is to run, told which of the
/// threads it is: the caller's is 0, the team's count from 1.
struct Task(*const (dyn Fn(usize) + Sync));

/// What is counted of the team as it works: all of it since it was last
/// taken ([`Watch::take`]).
#[derive(Default)]
struct Told {
    /// When the task at hand was handed out, on [`now_ns`].
    posted_ns: AtomicU64,
    rounds: AtomicU64,
    /// How many times a thread of the team took a place; how many of
    /// those times it had been asleep and was woken for it, how long
    /// after the work was called out in all, and at the most.
    joined: AtomicU64,
    woken: AtomicU64,
    late_ns: AtomicU64,
    latest_ns: AtomicU64,
    /// How long the caller looked for the others once its own share was
    /// done, in all and at the most.
    waited_ns: AtomicU64,
    longest_wait_ns: AtomicU64,
}

/// What a team did since it was last asked: how often it was handed work,
/// how late its threads were to it, and how long the caller waited for
/// them, its own share done.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TeamStats {
    /// How many times work was handed out.
    pub rounds: u64,
    /// How many times a thread of the team took some of it up.
    pub joined: u64,
    /// How many of those times the thread had been asleep and was woken
    /// for it.
    pub woken: u64,
    /// How long after work was called out a thread that was asleep was at
    /// it, on average and at the most, in microseconds.
    pub late_mean_us: f64,
    pub late_worst_us: f64,
    /// How long the caller waited for the others once it had nothing left
    /// to do itself, on average over the rounds and at the most, in
    /// microseconds.
    pub waited_mean_us: f64,
    pub waited_worst_us: f64,
}

/// A look at a team from a thread that does not own it.
#[derive(Clone)]
pub struct Watch(Arc<Shared>);

impl Watch {
    /// What the team did since this was last called.
    pub fn take(&self) -> TeamStats {
        let told = &self.0.told;
        let take = |count: &AtomicU64| count.swap(0, Ordering::Relaxed);
        let (rounds, joined, woken) = (take(&told.rounds), take(&told.joined), take(&told.woken));
        let micros = |ns: u64, over: u64| if over == 0 { 0.0 } else { ns as f64 / 1e3 / over as f64 };
        TeamStats {
            rounds,
            joined,
            woken,
            late_mean_us: micros(take(&told.late_ns), woken),
            late_worst_us: micros(take(&told.latest_ns), 1),
            waited_mean_us: micros(take(&told.waited_ns), rounds),
            waited_worst_us: micros(take(&told.longest_wait_ns), 1),
        }
    }
}

impl Team {
    /// A team of `threads` threads, each at the priority the system keeps
    /// for audio and treating very small numbers as the audio thread does.
    /// Made off the audio thread: starting a thread allocates.
    pub fn new(threads: usize) -> Self {
        Self::settled(threads, || ())
    }

    /// [`Team::new`], whose threads each do `settle` once before anything
    /// else. For tests that watch what such a thread does.
    pub fn settled(threads: usize, settle: fn()) -> Self {
        let shared = Arc::new(Shared {
            state: AtomicU64::new(0),
            task: AtomicPtr::new(std::ptr::null_mut()),
            panicked: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            asleep: (0..threads).map(|_| AtomicBool::new(false)).collect(),
            told: Told::default(),
        });
        let threads = (0..threads)
            .map_while(|index| {
                let shared = shared.clone();
                let run = move || {
                    let _priority = AudioPriority::raise();
                    let _denormals = DenormalGuard::new();
                    settle();
                    shared.serve(index + 1);
                };
                // A machine that has no thread to spare plays on the ones
                // it gave.
                std::thread::Builder::new().name(format!("trill-play-{}", index + 1)).spawn(run).ok()
            })
            .collect();
        Self { shared, threads }
    }

    /// A way to ask what the team does, for whoever does not own it.
    pub fn watch(&self) -> Watch {
        Watch(self.shared.clone())
    }
}

impl Workers for Team {
    fn len(&self) -> usize {
        self.threads.len()
    }

    /// Runs `work` on the caller's thread and on up to `helpers` threads
    /// of the team at once, and returns when every one of them has come
    /// back from it. `work` is written to be run by several threads at
    /// once, each taking a share of what there is to do until nothing is
    /// left. Nothing here allocates, locks or sleeps, unless a thread of
    /// the team never comes back: see the module.
    fn all(&self, helpers: usize, work: &(dyn Fn(usize) + Sync)) {
        if self.threads.is_empty() {
            return work(0);
        }
        let helpers = helpers.min(self.threads.len());
        let shared = &*self.shared;
        // The team's threads outlive this call, and the borrow in `work`
        // does not: they are given a pointer, and the guard below sees to
        // it that none of them follows it once this function has returned
        // or unwound.
        let task = Task(unsafe {
            std::mem::transmute::<*const (dyn Fn(usize) + Sync + '_), *const (dyn Fn(usize) + Sync + 'static)>(work)
        });
        // (What a round before left said, if its caller went down too.)
        shared.panicked.store(false, Ordering::Relaxed);
        shared.told.posted_ns.store(now_ns(), Ordering::Relaxed);
        shared.told.rounds.fetch_add(1, Ordering::Relaxed);
        shared.task.store(&task as *const Task as *mut Task, Ordering::Release);
        // Nobody is at work and no place is free: the round before was
        // waited out. From this store on, places are taken.
        shared.state.store(helpers as u64, Ordering::SeqCst);
        for thread in &self.threads[..helpers] {
            thread.thread().unpark();
        }
        let over = Over(shared);
        work(0);
        // Its own share done, the caller looks on until nobody is at work
        // any more. A place that is free meanwhile is one that a thread
        // which was asked has not come for, or one that was added because
        // something became ready: the caller is as good as any, and takes
        // it.
        let since = now_ns();
        let (mut looks, mut worked) = (0u32, 0u64);
        loop {
            let state = shared.state.load(Ordering::Acquire);
            if state & !PLACES == 0 {
                break;
            }
            if state & PLACES != 0 {
                if shared.state.compare_exchange(state, state - 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    let began = now_ns();
                    work(0);
                    worked += now_ns().saturating_sub(began);
                }
                continue;
            }
            looks = looks.wrapping_add(1);
            if looks.is_multiple_of(4096) && now_ns().saturating_sub(since + worked) > STUCK_NS {
                // A device has not come back for many blocks' time. The
                // block is lost; a processor need not be.
                std::thread::sleep(std::time::Duration::from_millis(1));
            } else {
                std::hint::spin_loop();
            }
        }
        let waited = now_ns().saturating_sub(since + worked);
        shared.told.waited_ns.fetch_add(waited, Ordering::Relaxed);
        shared.told.longest_wait_ns.fetch_max(waited, Ordering::Relaxed);
        drop(over);
        if shared.panicked.swap(false, Ordering::Relaxed) {
            panic!("a thread that makes the song panicked");
        }
    }

    /// One more thread may come to the task at hand, and one that sleeps
    /// is woken for it: something has become ready in the middle of a
    /// round. Called from inside the work of [`Team::all`] only, which is
    /// what keeps the task where it is. Nothing here allocates, locks or
    /// waits.
    fn more(&self) {
        if self.threads.is_empty() {
            return;
        }
        let shared = &*self.shared;
        shared.told.posted_ns.store(now_ns(), Ordering::Relaxed);
        // The place first and the look for a sleeper after: see `serve`.
        shared.state.fetch_add(1, Ordering::SeqCst);
        for (thread, asleep) in self.threads.iter().zip(shared.asleep.iter()) {
            if asleep.load(Ordering::SeqCst) && asleep.swap(false, Ordering::SeqCst) {
                thread.thread().unpark();
                return;
            }
        }
        // Nobody sleeps: whoever is looking finds the place, and whoever
        // is at a share looks for another when it is done.
    }
}

/// Ends a task when it is dropped: those who have taken it up are waited
/// for, and after that nobody may. (A thread at work may still add a
/// place, so the door is shut at a moment when nobody is.)
struct Over<'a>(&'a Shared);

impl Drop for Over<'_> {
    fn drop(&mut self) {
        let shared = self.0;
        let since = now_ns();
        let mut looks = 0u32;
        loop {
            let state = shared.state.load(Ordering::Acquire);
            if state & !PLACES == 0 {
                // Nobody at work, so nobody to add a place: with the
                // places that are left taken away in the same step, the
                // task is nobody's any more.
                if shared.state.compare_exchange(state, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    return;
                }
                continue;
            }
            // (Only where the caller went down over its own share is
            // anybody still at work here.)
            looks = looks.wrapping_add(1);
            if looks.is_multiple_of(4096) && now_ns().saturating_sub(since) > STUCK_NS {
                std::thread::sleep(std::time::Duration::from_millis(1));
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

impl Shared {
    /// Takes a place at the task at hand, if one is free: from then on
    /// this thread is counted at work, and the task stays where it is.
    fn enter(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & PLACES == 0 {
                return false;
            }
            let taken = state - 1 + ONE_AT_WORK;
            match self.state.compare_exchange_weak(state, taken, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(now) => state = now,
            }
        }
    }

    /// What a thread of the team does until it is sent home: `me` is its
    /// place among the team's threads, from 1.
    fn serve(&self, me: usize) {
        let asleep = &self.asleep[me - 1];
        let mut idle_since = now_ns();
        // It has slept since it last had work.
        let mut woken = false;
        loop {
            if self.quit.load(Ordering::Relaxed) {
                return;
            }
            if self.enter() {
                self.told.joined.fetch_add(1, Ordering::Relaxed);
                if std::mem::take(&mut woken) {
                    let late = now_ns().saturating_sub(self.told.posted_ns.load(Ordering::Relaxed));
                    self.told.woken.fetch_add(1, Ordering::Relaxed);
                    self.told.late_ns.fetch_add(late, Ordering::Relaxed);
                    self.told.latest_ns.fetch_max(late, Ordering::Relaxed);
                }
                // While this thread is counted at work, `Team::all` has
                // not returned, and what the pointer leads to is there.
                let task = self.task.load(Ordering::Acquire);
                let came_back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { (*(*task).0)(me) }));
                if came_back.is_err() {
                    self.panicked.store(true, Ordering::Relaxed);
                }
                self.state.fetch_sub(ONE_AT_WORK, Ordering::AcqRel);
                idle_since = now_ns();
                continue;
            }
            if now_ns().saturating_sub(idle_since) < LINGER_NS {
                std::hint::spin_loop();
                continue;
            }
            // It says that it sleeps and then looks one last time: a
            // place that was freed before this was said is seen here, and
            // whoever frees one after finds it said (`Team::more`).
            asleep.store(true, Ordering::SeqCst);
            if self.state.load(Ordering::SeqCst) & PLACES != 0 || self.quit.load(Ordering::SeqCst) {
                asleep.store(false, Ordering::SeqCst);
                continue;
            }
            // Until the caller of the next task wakes it, or someone with
            // more to do. A wake that came while it was still looking is
            // kept for it, so none is lost.
            std::thread::park();
            asleep.store(false, Ordering::SeqCst);
            woken = true;
            idle_since = now_ns();
        }
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn every_share_of_the_work_is_done_once_and_all_of_it_before_the_call_returns() {
        let team = Team::new(3);
        for round in 0..2_000 {
            let shares: Vec<AtomicUsize> = (0..17).map(|_| AtomicUsize::new(0)).collect();
            let next = AtomicUsize::new(0);
            let work = |_: usize| {
                while let Some(share) = shares.get(next.fetch_add(1, Ordering::Relaxed)) {
                    share.fetch_add(1, Ordering::Relaxed);
                }
            };
            team.all(round % 4, &work);
            assert!(shares.iter().all(|share| share.load(Ordering::Relaxed) == 1), "round {round}");
            // Now and then long enough apart for the team to have gone to
            // sleep, so that it is woken too.
            if round % 500 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let stats = team.watch().take();
        assert_eq!(stats.rounds, 2_000, "every round is handed out, with places or without");
        assert!(stats.joined >= stats.woken && stats.woken > 0, "{stats:?}");
        assert!(stats.late_worst_us >= stats.late_mean_us && stats.late_mean_us > 0.0, "{stats:?}");
        assert_eq!(team.watch().take().rounds, 0, "what was taken is not counted again");
    }

    #[test]
    fn the_threads_of_a_team_do_work_of_their_own_and_are_settled_first() {
        thread_local! {
            static SETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        let team = Team::settled(2, || SETTLED.set(true));
        assert_eq!(team.len(), 2);
        // Work that only ends once two threads besides the caller are in
        // it at the same time.
        let (inside, settled) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let work = |_: usize| {
            settled.fetch_add(usize::from(SETTLED.get()), Ordering::SeqCst);
            inside.fetch_add(1, Ordering::SeqCst);
            let begun = std::time::Instant::now();
            while inside.load(Ordering::SeqCst) < 3 && begun.elapsed() < std::time::Duration::from_secs(20) {
                std::thread::yield_now();
            }
        };
        team.all(2, &work);
        assert_eq!(inside.load(Ordering::SeqCst), 3, "three threads were at it at once");
        assert_eq!(settled.load(Ordering::SeqCst), 2, "and the team's two had been settled");
    }

    #[test]
    fn what_becomes_ready_in_the_middle_of_a_round_is_taken_up_by_a_thread_that_slept() {
        let team = Team::new(2);
        for round in 0..20 {
            // Long enough apart for both to have parked themselves.
            std::thread::sleep(std::time::Duration::from_millis(5));
            let (begun, ready, done, by_others) =
                (AtomicBool::new(false), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
            let work = |me: usize| {
                if me == 0 && !begun.swap(true, Ordering::SeqCst) {
                    // The caller makes four things ready, one at a time,
                    // and is busy between them: nobody was asked to help
                    // when the round began.
                    for _ in 0..4 {
                        ready.fetch_add(1, Ordering::SeqCst);
                        team.more();
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                let take = |ready: usize| ready.checked_sub(1);
                while ready.fetch_update(Ordering::SeqCst, Ordering::SeqCst, take).is_ok() {
                    done.fetch_add(1, Ordering::SeqCst);
                    by_others.fetch_add(usize::from(me != 0), Ordering::SeqCst);
                }
            };
            team.all(0, &work);
            assert_eq!((ready.load(Ordering::SeqCst), done.load(Ordering::SeqCst)), (0, 4), "round {round}");
            assert!(by_others.load(Ordering::SeqCst) >= 1, "round {round}: nobody was woken for what became ready");
        }
        // And a team that was told of more than anybody came for is as
        // good as new for the round after.
        let done = AtomicUsize::new(0);
        team.all(2, &|_| {
            team.more();
            done.fetch_add(1, Ordering::SeqCst);
        });
        assert!(done.load(Ordering::SeqCst) >= 1);
        team.all(0, &|me| assert_eq!(me, 0, "a place was left over from the round before"));
    }

    #[test]
    fn a_panic_on_a_thread_of_the_team_is_the_callers_once_everyone_is_back() {
        let team = Team::new(1);
        let caller = std::thread::current().id();
        let work = |_: usize| {
            if std::thread::current().id() != caller {
                panic!("on purpose");
            }
            // Give the other thread time to take the work up.
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        let came_back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| team.all(1, &work)));
        assert!(came_back.is_err());
        // And the team still works.
        let done = AtomicUsize::new(0);
        team.all(1, &|_| {
            done.fetch_add(1, Ordering::SeqCst);
        });
        assert!(done.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn a_caller_that_panics_over_its_own_share_waits_for_the_others_first() {
        let team = Team::new(2);
        let caller = std::thread::current().id();
        let finished = AtomicUsize::new(0);
        let work = |_: usize| {
            if std::thread::current().id() == caller {
                // The others are in it by now, or will find no place.
                std::thread::sleep(std::time::Duration::from_millis(50));
                panic!("on purpose");
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
            finished.fetch_add(1, Ordering::SeqCst);
        };
        let came_back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| team.all(2, &work)));
        assert!(came_back.is_err());
        // Whoever had taken the work up was through with it before the
        // panic went on: what the work borrows is still there for them.
        let joined = team.watch().take().joined;
        assert_eq!(finished.load(Ordering::SeqCst) as u64, joined);
    }
}
