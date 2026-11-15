//! Interior mutability for the objects a plugin calls back into from the
//! audio thread.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// A value that one user at a time may touch. A second user that arrives
/// while the first is busy is turned away instead of being made to wait.
///
/// The host's event list is filled by the host before `process` and read
/// by the plugin during it, never both at once, so in a correct run nobody
/// is ever turned away. But the plugin reaches the list through a COM
/// pointer it may use from any thread at any time, and a data race in this
/// host must not depend on a plugin behaving. A lock would make that safe
/// and the audio thread wait; this makes it safe and wait-free.
pub(crate) struct Exclusive<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `value` is only reached through `try_with`, which lets one thread
// in at a time, so sharing an `Exclusive` hands the value from thread to
// thread but never to two at once. That is what `T: Send` permits.
unsafe impl<T: Send> Sync for Exclusive<T> {}

impl<T> Exclusive<T> {
    pub(crate) fn new(value: T) -> Self {
        Self { busy: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    /// Runs `body` with the value, unless it is in use right now.
    pub(crate) fn try_with<R>(&self, body: impl FnOnce(&mut T) -> R) -> Option<R> {
        /// Marks the value free again, also if `body` panics.
        struct Release<'a>(&'a AtomicBool);

        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }

        self.busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok()?;
        let _release = Release(&self.busy);
        // SAFETY: `busy` was false and this call made it true, so no other
        // call is between its exchange and its release: this is the only
        // reference to the value, until `_release` is dropped after `body`
        // has returned it.
        Some(body(unsafe { &mut *self.value.get() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn one_user_at_a_time_gets_the_value() {
        let cell = Exclusive::new(1);
        assert_eq!(cell.try_with(|value| std::mem::replace(value, 2)), Some(1));
        assert_eq!(cell.try_with(|value| *value), Some(2));

        // Someone arriving while it is in use is turned away, and the user
        // inside is not disturbed.
        let nested = cell.try_with(|value| {
            *value = 3;
            cell.try_with(|inner| *inner)
        });
        assert_eq!(nested, Some(None));
        assert_eq!(cell.try_with(|value| *value), Some(3), "free again afterwards");
    }

    #[test]
    fn a_panicking_user_leaves_the_value_usable() {
        let cell = Exclusive::new(0);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cell.try_with(|_| panic!("deliberate"));
        }));
        assert!(panicked.is_err());
        assert_eq!(cell.try_with(|value| *value), Some(0));
    }

    #[test]
    fn concurrent_users_never_overlap() {
        let cell = Arc::new(Exclusive::new(Vec::<usize>::new()));
        let refused = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let (cell, refused) = (cell.clone(), refused.clone());
                std::thread::spawn(move || {
                    let mut entered = 0;
                    for _ in 0..20_000 {
                        // A torn or overlapping update would corrupt the
                        // vector's length or contents.
                        match cell.try_with(|values| values.push(worker)) {
                            Some(()) => entered += 1,
                            None => {
                                refused.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    entered
                })
            })
            .collect();
        let entered: usize = workers.into_iter().map(|worker| worker.join().unwrap()).sum();
        let values = cell.try_with(std::mem::take).unwrap();
        assert_eq!(values.len(), entered);
        assert_eq!(entered + refused.load(Ordering::Relaxed), 80_000);
    }
}
