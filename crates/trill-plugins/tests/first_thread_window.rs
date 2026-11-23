//! Plugins served from a first thread that runs a loop of the program's own,
//! as a window's (macOS, [`serve_here`]): a question asked on that thread
//! is answered there, after the jobs sent before it; with no frame drawn,
//! jobs are taken one per 50 ms at the most; and the session's lock, held
//! by a thread that waits for a plugin, is waited for on the first thread
//! without the two waiting on each other. Elsewhere there is nothing of
//! the kind, and this program says so and passes.
//!
//! A test program without libtest's harness: `cargo test -p trill-plugins
//! --test first_thread_window`.
//!
//! [`serve_here`]: trill_plugins::first_thread

#[cfg(not(target_os = "macos"))]
fn main() {
    println!(
        "\nrunning 0 tests (plugins share a window's thread on macOS only)\n\ntest result: ok. 0 passed; 0 failed\n"
    );
}

#[cfg(target_os = "macos")]
fn main() {
    macos::main()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};
    use std::time::{Duration, Instant};
    use trill_engine::{AudioBackend, Session, SessionLock};
    use trill_plugins::first_thread::{on_the_plugin_thread, serve_here, serve_while_waiting};

    const WAIT: Duration = Duration::from_secs(10);

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, after_one: u8) -> i32;
    }

    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    }

    /// Has the first thread run `work` from its loop, outside any job, as a
    /// window's code runs.
    fn on_the_first_thread(work: Box<dyn FnOnce() + Send>) {
        extern "C" fn run(context: *mut c_void) {
            // SAFETY: the box made below, taken back once.
            let work = unsafe { Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
            work();
        }
        let context = Box::into_raw(Box::new(work)).cast::<c_void>();
        // SAFETY: the main queue, which the first thread's loop serves.
        unsafe { dispatch_async_f(&raw const _dispatch_main_q, context, run) };
    }

    pub fn main() {
        let first = thread::current().id();
        assert!(serve_here(), "this is the first thread");
        SessionLock::serve_while_waiting(serve_while_waiting);
        let done = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(Mutex::new(0));
        let tests = {
            let (done, failed) = (done.clone(), failed.clone());
            thread::spawn(move || {
                *failed.lock().unwrap() = run(first);
                done.store(true, Ordering::Release);
            })
        };
        while !done.load(Ordering::Acquire) {
            // SAFETY: the first thread runs its own loop, as a window does.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0) };
        }
        tests.join().unwrap();
        std::process::exit(i32::from(*failed.lock().unwrap() > 0));
    }

    type Test = Box<dyn Fn() -> Result<(), String>>;

    fn run(first: ThreadId) -> usize {
        let tests: [(&str, Test); 4] = [
            ("plugin_work_is_done_on_the_first_thread_that_runs_the_window", Box::new(move || work_is_first(first))),
            ("a_question_asked_on_the_first_thread_is_answered_there_after_the_jobs_before_it", Box::new(asked_there)),
            ("with_no_frame_drawn_jobs_are_taken_one_in_fifty_milliseconds", Box::new(paced)),
            (
                "a_wait_for_the_session_on_the_first_thread_serves_the_plugins_that_its_holder_waits_for",
                Box::new(no_deadlock),
            ),
        ];
        println!("\nrunning {} tests", tests.len());
        let mut failed = 0;
        for (name, test) in tests {
            match test() {
                Ok(()) => println!("test {name} ... ok"),
                Err(why) => {
                    failed += 1;
                    println!("test {name} ... FAILED\n  {why}");
                }
            }
        }
        println!(
            "\ntest result: {}. {} passed; {failed} failed\n",
            if failed == 0 { "ok" } else { "FAILED" },
            4 - failed
        );
        failed
    }

    fn work_is_first(first: ThreadId) -> Result<(), String> {
        let plugins = on_the_plugin_thread(WAIT, || thread::current().id()).ok_or("no answer")?;
        if plugins == first { Ok(()) } else { Err(format!("plugins on {plugins:?}, the first thread {first:?}")) }
    }

    fn asked_there() -> Result<(), String> {
        let seen = Arc::new(Mutex::new(Vec::<&str>::new()));
        // Two jobs sent from here, which wait for frames that are not drawn,
        // and a question asked on the first thread meanwhile.
        let senders: Vec<_> = ["the first job", "the second job"]
            .into_iter()
            .map(|label| {
                let seen = seen.clone();
                let sent = thread::spawn(move || on_the_plugin_thread(WAIT, move || seen.lock().unwrap().push(label)));
                thread::sleep(Duration::from_millis(5));
                sent
            })
            .collect();
        let (answer, answered) = std::sync::mpsc::channel();
        let asked = seen.clone();
        on_the_first_thread(Box::new(move || {
            let reply = on_the_plugin_thread(WAIT, move || asked.lock().unwrap().push("the question"));
            let _ = answer.send(reply.is_some());
        }));
        let answered = answered.recv_timeout(WAIT).map_err(|_| "the first thread never asked")?;
        for sent in senders {
            sent.join().map_err(|_| "a sender panicked")?.ok_or("a job was not done")?;
        }
        let seen = seen.lock().unwrap().clone();
        match (answered, seen.as_slice()) {
            (true, ["the first job", "the second job", "the question"]) => Ok(()),
            _ => Err(format!("answered: {answered}, in the order {seen:?}")),
        }
    }

    fn paced() -> Result<(), String> {
        let begun = Instant::now();
        let senders: Vec<_> = (0..4).map(|_| thread::spawn(|| on_the_plugin_thread(WAIT, Instant::now))).collect();
        let mut times: Vec<Instant> = Vec::new();
        for sent in senders {
            times.push(sent.join().map_err(|_| "a sender panicked")?.ok_or("a job was not done")?);
        }
        times.sort();
        let gaps: Vec<u128> = times.windows(2).map(|pair| pair[1].duration_since(pair[0]).as_millis()).collect();
        if gaps.iter().all(|gap| (45..200).contains(gap)) {
            Ok(())
        } else {
            Err(format!("taken {:?} apart, from {:?} after they were sent", gaps, times[0].duration_since(begun)))
        }
    }

    fn no_deadlock() -> Result<(), String> {
        let session = Session::new(Arc::new(trill_dsp::BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 })
            .map_err(|error| error.to_string())?;
        let shared = Arc::new(SessionLock::new(session));
        // A thread holds the session and waits for a plugin, as a safety
        // copy does when it asks for their states ...
        let holder = {
            let shared = shared.clone();
            thread::spawn(move || {
                let _held = shared.lock();
                thread::sleep(Duration::from_millis(100));
                on_the_plugin_thread(WAIT, || ()).is_some()
            })
        };
        thread::sleep(Duration::from_millis(30));
        // ... while the first thread wants the session.
        let (took, taken) = std::sync::mpsc::channel();
        let wanted = shared.clone();
        on_the_first_thread(Box::new(move || {
            let begun = Instant::now();
            drop(wanted.lock());
            let _ = took.send(begun.elapsed());
        }));
        let waited = taken.recv_timeout(WAIT).map_err(|_| "the first thread never had the session")?;
        let answered = holder.join().map_err(|_| "the holder panicked")?;
        if answered && waited < Duration::from_secs(2) {
            Ok(())
        } else {
            Err(format!("the holder's plugin answered: {answered}; the first thread waited {waited:?}"))
        }
    }
}
