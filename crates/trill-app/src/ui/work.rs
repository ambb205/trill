//! Work the window never does itself: decoding audio files, building
//! waveform overviews, searching the library, starting previews, and the
//! edits that bring a new audio file into the song.
//!
//! All of it runs on a few worker threads. A job is a closure that returns
//! a [`Done`]; the window drains what has finished at the top of each frame
//! ([`Work::finished`]) and is woken when something does. Nothing here is
//! ever waited for by the UI thread.
//!
//! Two kinds of job share the workers. *Urgent* ones answer something the
//! user just did (a click that previews, a drop, a keystroke in the search
//! field) and must not queue behind a screenful of waveforms; one worker
//! takes nothing else. *Bulk* ones (waveforms) are done in the order asked.

use super::sounds::Done;
use eframe::egui;
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

/// How many workers take bulk jobs as well as urgent ones.
const GENERAL_WORKERS: usize = 2;

type Job = Box<dyn FnOnce() -> Done + Send>;

/// How soon a job is wanted.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Priority {
    /// The user is waiting for it.
    Urgent,
    /// Wanted soon: a waveform to draw.
    Bulk,
}

#[derive(Default)]
struct Queue {
    urgent: VecDeque<Job>,
    bulk: VecDeque<Job>,
    /// Jobs a worker has taken and not yet reported.
    running: usize,
    /// Workers take nothing while this is set. For tests, which use it to
    /// show that the window carries on without the answers.
    held: bool,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Notified when a job ends, for [`Work::wait_until_idle`].
    idle: Condvar,
}

/// The worker threads and what they have finished.
pub struct Work {
    shared: Arc<Shared>,
    finished: Receiver<Done>,
    threads: Vec<JoinHandle<()>>,
}

impl Work {
    /// Starts the workers. `ctx` is repainted whenever a job finishes.
    pub fn start(ctx: &egui::Context) -> Self {
        let shared = Arc::new(Shared { queue: Mutex::default(), wake: Condvar::new(), idle: Condvar::new() });
        let (sender, finished) = channel();
        let threads = (0..=GENERAL_WORKERS)
            .filter_map(|index| {
                let (shared, sender, ctx) = (shared.clone(), sender.clone(), ctx.clone());
                // The first worker is the one kept free for urgent jobs.
                let bulk_too = index > 0;
                std::thread::Builder::new()
                    .name(format!("sound-work-{index}"))
                    .spawn(move || worker(&shared, &sender, &ctx, bulk_too))
                    .ok()
            })
            .collect();
        Self { shared, finished, threads }
    }

    /// Queues `job`. It runs on a worker thread, some time after this
    /// returns.
    pub fn spawn(&self, priority: Priority, job: impl FnOnce() -> Done + Send + 'static) {
        let mut queue = self.shared.queue.lock();
        match priority {
            Priority::Urgent => queue.urgent.push_back(Box::new(job)),
            Priority::Bulk => queue.bulk.push_back(Box::new(job)),
        }
        drop(queue);
        self.shared.wake.notify_all();
    }

    /// What has finished since the last call, oldest first.
    pub fn finished(&self) -> Vec<Done> {
        self.finished.try_iter().collect()
    }

    /// True while nothing is queued or running. What finished may still be
    /// waiting to be collected.
    pub fn is_idle(&self) -> bool {
        let queue = self.shared.queue.lock();
        queue.urgent.is_empty() && queue.bulk.is_empty() && queue.running == 0
    }

    /// Stops the workers from taking jobs, or lets them go on.
    #[cfg(test)]
    pub fn hold(&self, held: bool) {
        self.shared.queue.lock().held = held;
        self.shared.wake.notify_all();
    }

    /// Blocks until nothing is queued or running, or `timeout` has passed,
    /// and says whether the workers are idle. For tests, which have
    /// nothing else to do meanwhile.
    #[cfg(test)]
    pub fn wait_until_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut queue = self.shared.queue.lock();
        while !(queue.urgent.is_empty() && queue.bulk.is_empty() && queue.running == 0) {
            if self.shared.idle.wait_until(&mut queue, deadline).timed_out() {
                return queue.urgent.is_empty() && queue.bulk.is_empty() && queue.running == 0;
            }
        }
        true
    }
}

impl Drop for Work {
    /// Lets the jobs that are running finish, forgets the rest and waits
    /// for the threads: they hold the runtime, which must not end on one of
    /// them.
    fn drop(&mut self) {
        {
            let mut queue = self.shared.queue.lock();
            queue.closed = true;
            queue.urgent.clear();
            queue.bulk.clear();
        }
        self.shared.wake.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn worker(shared: &Shared, finished: &Sender<Done>, ctx: &egui::Context, bulk_too: bool) {
    loop {
        let job = {
            let mut queue = shared.queue.lock();
            loop {
                if queue.closed {
                    return;
                }
                if !queue.held {
                    let urgent = queue.urgent.pop_front();
                    let next = match urgent {
                        Some(job) => Some(job),
                        None if bulk_too => queue.bulk.pop_front(),
                        None => None,
                    };
                    if let Some(job) = next {
                        queue.running += 1;
                        break job;
                    }
                }
                shared.wake.wait(&mut queue);
            }
        };
        // A job that panics must not take the worker, or the count of
        // running jobs, with it.
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        if let Ok(done) = done {
            // The receiver is gone only when the window has closed.
            let _ = finished.send(done);
        }
        shared.queue.lock().running -= 1;
        shared.idle.notify_all();
        ctx.request_repaint();
    }
}
