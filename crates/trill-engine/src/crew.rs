//! The threads a render uses beside its own.
//!
//! While a song plays, one thread makes all of it: the audio thread, which
//! has a speaker to keep up with and may not wait for anything. A render
//! has nothing to keep up with and a whole machine to use, so it may hand
//! the tracks of a block that do not feed one another to several threads
//! at once ([`crate::processor`] says which those are). This is what it
//! hands them to: a few threads that are made once for a render, sleep
//! until there is work, do it, and say so.
//!
//! Nothing here is used while a song plays.

use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Threads that work on what [`Crew::all`] is given, beside the caller.
pub(crate) struct Crew {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    /// Told when there is work, and when the crew is to go home.
    work: Condvar,
    /// Told when the last thread at work has finished.
    done: Condvar,
}

struct State {
    /// The work at hand.
    task: Option<Task>,
    /// Counts the tasks, so that a thread takes each of them once.
    round: u64,
    /// How many more threads may take up the task at hand.
    places: usize,
    /// How many have taken it up and not finished.
    at_work: usize,
    /// One of them panicked over it.
    panicked: bool,
    quit: bool,
}

/// What every thread of the crew is to run. The pointer is to something on
/// the stack of [`Crew::all`], which does not return while a thread may
/// still follow it.
#[derive(Copy, Clone)]
struct Task(*const (dyn Fn() + Sync));

// It is handed to other threads, and what it points to may be called from
// several at once: that is what `Sync` on the closure says.
unsafe impl Send for Task {}

impl Crew {
    /// A crew of `threads` threads, each of which does `settle` once
    /// before anything else: whatever a thread must have set to make the
    /// same sound as the caller's.
    pub fn new<Kept: 'static>(threads: usize, settle: fn() -> Kept) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State { task: None, round: 0, places: 0, at_work: 0, panicked: false, quit: false }),
            work: Condvar::new(),
            done: Condvar::new(),
        });
        let threads = (0..threads)
            .filter_map(|index| {
                let shared = shared.clone();
                let run = move || {
                    let _kept = settle();
                    shared.serve();
                };
                // A machine that has no thread to spare renders with the
                // ones it gave.
                std::thread::Builder::new().name(format!("trill-render-{}", index + 1)).spawn(run).ok()
            })
            .collect();
        Self { shared, threads }
    }

    /// How many threads there are beside the caller's.
    pub fn len(&self) -> usize {
        self.threads.len()
    }

    /// Runs `work` on the caller's thread and on up to `helpers` threads
    /// of the crew at once, and returns when every one of them has come
    /// back from it. `work` is written to be run by several threads at
    /// once, each taking a share of what there is to do until nothing is
    /// left; a thread that wakes late finds nothing, and is not waited
    /// for once the caller has come back.
    pub fn all(&self, helpers: usize, work: &(dyn Fn() + Sync)) {
        let helpers = helpers.min(self.threads.len());
        if helpers == 0 {
            return work();
        }
        // The crew's threads outlive this call, and the borrow in `work`
        // does not: they are given a pointer, and the guard below sees to
        // it that none of them follows it once this function has returned
        // or unwound.
        let task = Task(unsafe {
            std::mem::transmute::<*const (dyn Fn() + Sync + '_), *const (dyn Fn() + Sync + 'static)>(work)
        });
        {
            let mut state = self.shared.state.lock();
            state.task = Some(task);
            state.round += 1;
            state.places = helpers;
            state.panicked = false;
        }
        for _ in 0..helpers {
            self.shared.work.notify_one();
        }
        let over = Over(&self.shared);
        work();
        drop(over);
        assert!(!self.shared.state.lock().panicked, "a thread of the render panicked");
    }
}

/// Ends a task when it is dropped: nobody else may take it up, and those
/// who have are waited for.
struct Over<'a>(&'a Shared);

impl Drop for Over<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        state.places = 0;
        while state.at_work > 0 {
            self.0.done.wait(&mut state);
        }
        state.task = None;
    }
}

impl Shared {
    /// What a thread of the crew does until it is sent home.
    fn serve(&self) {
        let mut seen = 0;
        let mut state = self.state.lock();
        loop {
            while !state.quit && (state.places == 0 || state.round == seen) {
                self.work.wait(&mut state);
            }
            if state.quit {
                return;
            }
            let Some(task) = state.task else { continue };
            seen = state.round;
            state.places -= 1;
            state.at_work += 1;
            drop(state);
            // While `at_work` counts this thread, `Crew::all` has not
            // returned, and what the pointer leads to is there.
            let came_back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { (*task.0)() }));
            state = self.state.lock();
            state.panicked |= came_back.is_err();
            state.at_work -= 1;
            if state.at_work == 0 {
                self.done.notify_all();
            }
        }
    }
}

impl Drop for Crew {
    fn drop(&mut self) {
        self.shared.state.lock().quit = true;
        self.shared.work.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_share_of_the_work_is_done_once_and_all_of_it_before_the_call_returns() {
        let crew = Crew::new(3, || ());
        for round in 0..200 {
            let shares: Vec<AtomicUsize> = (0..17).map(|_| AtomicUsize::new(0)).collect();
            let next = AtomicUsize::new(0);
            let work = || {
                while let Some(share) = shares.get(next.fetch_add(1, Ordering::Relaxed)) {
                    share.fetch_add(1, Ordering::Relaxed);
                }
            };
            crew.all(round % 4, &work);
            assert!(shares.iter().all(|share| share.load(Ordering::Relaxed) == 1), "round {round}");
        }
    }

    #[test]
    fn the_threads_of_a_crew_do_work_of_their_own_and_are_settled_first() {
        thread_local! {
            static SETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        let crew = Crew::new(2, || SETTLED.set(true));
        assert_eq!(crew.len(), 2);
        // Work that only ends once two threads besides the caller are in
        // it at the same time.
        let (inside, settled) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let work = || {
            settled.fetch_add(usize::from(SETTLED.get()), Ordering::SeqCst);
            inside.fetch_add(1, Ordering::SeqCst);
            let begun = std::time::Instant::now();
            while inside.load(Ordering::SeqCst) < 3 && begun.elapsed() < std::time::Duration::from_secs(20) {
                std::thread::yield_now();
            }
        };
        crew.all(2, &work);
        assert_eq!(inside.load(Ordering::SeqCst), 3, "three threads were at it at once");
        assert_eq!(settled.load(Ordering::SeqCst), 2, "and the crew's two had been settled");
    }

    #[test]
    fn a_panic_on_a_thread_of_the_crew_is_the_callers_once_everyone_is_back() {
        let crew = Crew::new(1, || ());
        let caller = std::thread::current().id();
        let work = || {
            if std::thread::current().id() != caller {
                panic!("on purpose");
            }
            // Give the other thread time to take the work up.
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        let came_back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crew.all(1, &work)));
        assert!(came_back.is_err());
        // And the crew still works.
        let done = AtomicUsize::new(0);
        crew.all(1, &|| {
            done.fetch_add(1, Ordering::SeqCst);
        });
        assert!(done.load(Ordering::SeqCst) >= 1);
    }
}
