//! Where plugin work is done, in a program that owns its first thread: on
//! macOS the first thread, which the program lends ([`lend`]); elsewhere
//! the plugin thread, a thread of its own. Jobs sent while one runs are
//! taken in the order they came, and on macOS what a plugin posts to the
//! first thread's loop is served between two jobs.
//!
//! A test program without libtest's harness, which would run each test on
//! a thread of its own: `cargo test -p trill-plugins --test first_thread`.
//!
//! [`lend`]: trill_plugins::first_thread

use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;
use trill_plugins::first_thread::on_the_plugin_thread;

const WAIT: Duration = Duration::from_secs(10);

fn main() {
    let first = thread::current().id();
    #[cfg(target_os = "macos")]
    trill_plugins::first_thread::lend(move || run(first));
    #[cfg(not(target_os = "macos"))]
    std::process::exit(run(first));
}

/// A test: what went wrong, if anything did.
type Test = Box<dyn Fn() -> Result<(), String>>;

/// Runs every test and says how each went, as libtest would.
fn run(first: ThreadId) -> i32 {
    let tests: [(&str, Test); 3] = [
        (
            "plugin_work_is_done_on_the_first_thread_where_the_program_lends_it",
            Box::new(move || where_work_is_done(first)),
        ),
        ("jobs_sent_while_one_runs_are_taken_in_the_order_they_came", Box::new(jobs_are_taken_in_order)),
        ("what_a_plugin_posts_to_the_first_threads_loop_is_served_between_two_jobs", Box::new(posted_work_is_served)),
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
    println!("\ntest result: {}. {} passed; {failed} failed\n", if failed == 0 { "ok" } else { "FAILED" }, 3 - failed);
    i32::from(failed > 0)
}

fn where_work_is_done(first: ThreadId) -> Result<(), String> {
    let here = thread::current().id();
    let plugins = on_the_plugin_thread(WAIT, || thread::current().id()).ok_or("the plugin thread did not answer")?;
    let lent = trill_plugins::first_thread::is_first();
    match cfg!(target_os = "macos") {
        true if plugins == first && lent && here != first => Ok(()),
        false if plugins != first && plugins != here && !lent => Ok(()),
        _ => Err(format!("plugins on {plugins:?}, the first thread {first:?}, this test on {here:?}, lent: {lent}")),
    }
}

fn jobs_are_taken_in_order() -> Result<(), String> {
    let done = Arc::new(Mutex::new(Vec::new()));
    // A job that takes its time, and three sent one after another while it
    // runs, each from a thread of its own.
    let long = {
        let done = done.clone();
        thread::spawn(move || {
            on_the_plugin_thread(WAIT, move || {
                thread::sleep(Duration::from_millis(300));
                done.lock().unwrap().push(0);
            })
        })
    };
    thread::sleep(Duration::from_millis(60));
    let queued: Vec<_> = (1..=3)
        .map(|number| {
            let done = done.clone();
            let sent = thread::spawn(move || on_the_plugin_thread(WAIT, move || done.lock().unwrap().push(number)));
            thread::sleep(Duration::from_millis(30));
            sent
        })
        .collect();
    long.join().map_err(|_| "the long job's sender panicked")?.ok_or("the long job did not run")?;
    for sent in queued {
        sent.join().map_err(|_| "a sender panicked")?.ok_or("a job did not run")?;
    }
    let done = done.lock().unwrap().clone();
    if done == [0, 1, 2, 3] { Ok(()) } else { Err(format!("taken in the order {done:?}")) }
}

#[cfg(not(target_os = "macos"))]
fn posted_work_is_served() -> Result<(), String> {
    // Only macOS has plugins post their work to the first thread's loop.
    Ok(())
}

#[cfg(target_os = "macos")]
fn posted_work_is_served() -> Result<(), String> {
    use loops::*;
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    // A job that posts work to the first thread's loop as a plugin does
    // (a source of its own, signalled), with a second job sent while it runs.
    let first_job = {
        let seen = seen.clone();
        thread::spawn(move || {
            on_the_plugin_thread(WAIT, move || {
                let posted = Posted::on_this_loop(seen.clone());
                posted.signal();
                // What a plugin's code that runs a while further does.
                thread::sleep(Duration::from_millis(100));
                seen.lock().unwrap().push("the first job".into());
                // The source stays on the loop, which serves it after this job.
                let _ = posted;
            })
        })
    };
    thread::sleep(Duration::from_millis(30));
    let second = seen.clone();
    let second_job =
        thread::spawn(move || on_the_plugin_thread(WAIT, move || second.lock().unwrap().push("the second job".into())));
    first_job.join().map_err(|_| "a sender panicked")?.ok_or("the first job did not run")?;
    second_job.join().map_err(|_| "a sender panicked")?.ok_or("the second job did not run")?;
    let seen = seen.lock().unwrap().clone();
    let expected = ["the first job", "the posted work, on the first thread", "the second job"];
    if seen == expected { Ok(()) } else { Err(format!("seen in the order {seen:?}")) }
}

/// A plugin's own source on the loop of the thread it is made on, as JUCE
/// posts its messages.
#[cfg(target_os = "macos")]
mod loops {
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    #[repr(C)]
    struct SourceContext {
        version: isize,
        info: *mut c_void,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
        equal: *const c_void,
        hash: *const c_void,
        schedule: *const c_void,
        cancel: *const c_void,
        perform: extern "C" fn(*mut c_void),
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopSourceCreate(allocator: *const c_void, order: isize, context: *mut SourceContext) -> *mut c_void;
        fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopSourceSignal(source: *mut c_void);
        fn CFRunLoopWakeUp(run_loop: *mut c_void);
        fn pthread_main_np() -> i32;
    }

    pub struct Posted {
        source: *mut c_void,
        run_loop: *mut c_void,
    }

    extern "C" fn perform(info: *mut c_void) {
        // SAFETY: the list the source was made with, which is never freed.
        let seen = unsafe { &*info.cast::<Mutex<Vec<String>>>() };
        // SAFETY: asks of the calling thread only.
        let first = unsafe { pthread_main_np() } == 1;
        seen.lock()
            .unwrap()
            .push(format!("the posted work, on {}", if first { "the first thread" } else { "another thread" }));
    }

    impl Posted {
        pub fn on_this_loop(seen: Arc<Mutex<Vec<String>>>) -> Self {
            let info = Arc::into_raw(seen).cast_mut().cast::<c_void>();
            let mut context = SourceContext {
                version: 0,
                info,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
                equal: std::ptr::null(),
                hash: std::ptr::null(),
                schedule: std::ptr::null(),
                cancel: std::ptr::null(),
                perform,
            };
            // SAFETY: a source of order 0, as a plugin's are, added to the
            // calling thread's loop.
            unsafe {
                let source = CFRunLoopSourceCreate(std::ptr::null(), 0, &mut context);
                let run_loop = CFRunLoopGetCurrent();
                CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
                Self { source, run_loop }
            }
        }

        pub fn signal(&self) {
            // SAFETY: the source and loop made above.
            unsafe {
                CFRunLoopSourceSignal(self.source);
                CFRunLoopWakeUp(self.run_loop);
            }
        }
    }
}
