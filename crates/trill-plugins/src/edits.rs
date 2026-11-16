//! The edits a plugin makes to its own parameters, on their way to whoever
//! wants to know: the knobs a user turns in the plugin's editor.
//!
//! A plugin reports them where it happens to be: a CLAP plugin in the
//! output events of `process`, on the audio thread; a VST3 plugin through
//! its component handler, on the main thread. The session asks for them
//! from yet another thread. So they travel through a queue that nobody
//! waits on and that never allocates once it is built.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// How many edits can be waiting to be collected. A user's gesture is a
/// few dozen values per parameter between two collections; this leaves
/// room for a plugin that changes all of its parameters at once.
const CAPACITY: usize = 4096;

/// What a plugin did to one of its parameters.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Edit {
    /// The user took hold of the parameter's control.
    Begin,
    /// The parameter has this value now, in the plugin's own terms.
    Value(f64),
    /// The user let go of the control.
    End,
}

const TAG_BEGIN: u64 = 0;
const TAG_VALUE: u64 = 1;
const TAG_END: u64 = 2;
/// How many of an entry's low bits say which kind of edit it is.
const TAG_BITS: u32 = 2;

/// One queued edit: which parameter and what kind, and the value if it
/// has one, as `f64` bits.
#[derive(Default)]
struct Entry {
    key: AtomicU64,
    value: AtomicU64,
}

/// A queue of edits with one producer at a time and one consumer.
///
/// Every field is an atomic, so the queue is sound whatever a plugin does
/// with it; a plugin that reports edits from two threads at once can
/// garble what is queued, and nothing worse. An edit that finds the queue
/// full is dropped and the queue remembers that some were lost.
pub(crate) struct EditQueue {
    entries: Box<[Entry]>,
    /// Entries ever taken out. Written by the consumer only.
    head: AtomicUsize,
    /// Entries ever put in. Written by the producer only.
    tail: AtomicUsize,
    overflowed: AtomicBool,
}

impl EditQueue {
    pub(crate) fn new() -> Self {
        Self {
            entries: (0..CAPACITY).map(|_| Entry::default()).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            overflowed: AtomicBool::new(false),
        }
    }

    /// Queues an edit of the parameter in `slot`. Never waits and never
    /// allocates, so the audio thread may.
    pub(crate) fn push(&self, slot: usize, edit: Edit) {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(self.head.load(Ordering::Acquire)) >= self.entries.len() {
            self.overflowed.store(true, Ordering::Release);
            return;
        }
        let (tag, value) = match edit {
            Edit::Begin => (TAG_BEGIN, 0.0),
            Edit::Value(value) => (TAG_VALUE, value),
            Edit::End => (TAG_END, 0.0),
        };
        let entry = &self.entries[tail % self.entries.len()];
        entry.key.store(((slot as u64) << TAG_BITS) | tag, Ordering::Relaxed);
        entry.value.store(value.to_bits(), Ordering::Relaxed);
        // Publishing the new tail is what hands the entry to the consumer.
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
    }

    /// Hands every queued edit to `take`, oldest first, as
    /// `(slot, edit)`. Returns whether edits were lost to a full queue
    /// since the last call, in which case the values of the plugin's
    /// parameters are best read afresh.
    pub(crate) fn drain(&self, mut take: impl FnMut(usize, Edit)) -> bool {
        let tail = self.tail.load(Ordering::Acquire);
        let mut head = self.head.load(Ordering::Relaxed);
        while head != tail {
            let entry = &self.entries[head % self.entries.len()];
            let key = entry.key.load(Ordering::Relaxed);
            let edit = match key & ((1 << TAG_BITS) - 1) {
                TAG_BEGIN => Edit::Begin,
                TAG_END => Edit::End,
                _ => Edit::Value(f64::from_bits(entry.value.load(Ordering::Relaxed))),
            };
            take((key >> TAG_BITS) as usize, edit);
            head = head.wrapping_add(1);
        }
        self.head.store(head, Ordering::Release);
        self.overflowed.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn drained(queue: &EditQueue) -> (Vec<(usize, Edit)>, bool) {
        let mut edits = Vec::new();
        let lost = queue.drain(|slot, edit| edits.push((slot, edit)));
        (edits, lost)
    }

    #[test]
    fn edits_come_out_in_the_order_they_went_in() {
        let queue = EditQueue::new();
        assert_eq!(drained(&queue), (Vec::new(), false));

        queue.push(3, Edit::Begin);
        queue.push(3, Edit::Value(0.25));
        queue.push(70_000, Edit::Value(-1.5));
        queue.push(3, Edit::End);
        let expected = [(3, Edit::Begin), (3, Edit::Value(0.25)), (70_000, Edit::Value(-1.5)), (3, Edit::End)];
        assert_eq!(drained(&queue), (expected.to_vec(), false));
        assert_eq!(drained(&queue), (Vec::new(), false), "collected edits are gone");
    }

    #[test]
    fn a_full_queue_drops_the_newest_and_says_so() {
        let queue = EditQueue::new();
        for round in 0..CAPACITY + 10 {
            queue.push(round, Edit::Value(round as f64));
        }
        let (edits, lost) = drained(&queue);
        assert!(lost);
        assert_eq!(edits.len(), CAPACITY);
        assert_eq!(edits[CAPACITY - 1], (CAPACITY - 1, Edit::Value((CAPACITY - 1) as f64)));

        // There is room again, and the loss is reported once.
        queue.push(1, Edit::End);
        assert_eq!(drained(&queue), (vec![(1, Edit::End)], false));
    }

    #[test]
    fn nothing_is_lost_or_reordered_between_two_threads() {
        let queue = Arc::new(EditQueue::new());
        let producer = {
            let queue = queue.clone();
            std::thread::spawn(move || {
                for round in 0..200_000u32 {
                    // Spin while the consumer catches up: this test is
                    // about order, not about overflow.
                    while queue.tail.load(Ordering::Relaxed).wrapping_sub(queue.head.load(Ordering::Acquire))
                        >= CAPACITY
                    {
                        std::thread::yield_now();
                    }
                    queue.push((round % 7) as usize, Edit::Value(f64::from(round)));
                }
            })
        };
        let mut next = 0u32;
        let mut check = |slot: usize, edit: Edit| {
            assert_eq!((slot, edit), ((next % 7) as usize, Edit::Value(f64::from(next))));
            next += 1;
        };
        while !producer.is_finished() {
            assert!(!queue.drain(&mut check));
        }
        producer.join().unwrap();
        assert!(!queue.drain(&mut check));
        assert_eq!(next, 200_000);
    }
}
