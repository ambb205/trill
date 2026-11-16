//! A bounded queue that many threads may push to and none of them waits
//! on: how live input (MIDI ports, the window, MCP) reaches the audio
//! thread.
//!
//! This is Dmitry Vyukov's bounded queue. Every slot carries a sequence
//! number that says whose turn it is: a pusher claims the slot whose number
//! equals the push position, a popper the slot whose number is one past the
//! pop position. Nothing allocates after construction, and a full queue
//! refuses the push instead of blocking.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Slot<T> {
    sequence: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

pub(crate) struct Queue<T> {
    slots: Box<[Slot<T>]>,
    mask: usize,
    push_at: AtomicUsize,
    pop_at: AtomicUsize,
}

// SAFETY: a slot's value is written only by the thread that won the push
// position for it and read only by the thread that won the pop position,
// and the sequence number orders the two. Values cross threads, hence
// `T: Send`.
unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}

impl<T: Copy> Queue<T> {
    /// A queue holding up to `capacity` values, rounded up to a power of
    /// two.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        let slots = (0..capacity)
            .map(|index| Slot { sequence: AtomicUsize::new(index), value: UnsafeCell::new(MaybeUninit::uninit()) })
            .collect();
        Self { slots, mask: capacity - 1, push_at: AtomicUsize::new(0), pop_at: AtomicUsize::new(0) }
    }

    /// Adds `value`, or returns `false` if the queue is full.
    pub fn push(&self, value: T) -> bool {
        let mut position = self.push_at.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position & self.mask];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let ahead = sequence.wrapping_sub(position) as isize;
            if ahead == 0 {
                match self.push_at.compare_exchange_weak(
                    position,
                    position.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: winning the exchange made this thread the
                        // only one to write this slot until it is popped.
                        unsafe { (*slot.value.get()).write(value) };
                        slot.sequence.store(position.wrapping_add(1), Ordering::Release);
                        return true;
                    }
                    Err(current) => position = current,
                }
            } else if ahead < 0 {
                return false;
            } else {
                position = self.push_at.load(Ordering::Relaxed);
            }
        }
    }

    /// Takes the oldest value, if there is one.
    pub fn pop(&self) -> Option<T> {
        let mut position = self.pop_at.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position & self.mask];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let ahead = sequence.wrapping_sub(position.wrapping_add(1)) as isize;
            if ahead == 0 {
                match self.pop_at.compare_exchange_weak(
                    position,
                    position.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: the sequence number says a push finished
                        // writing this slot, and winning the exchange made
                        // this thread the only one to read it.
                        let value = unsafe { (*slot.value.get()).assume_init_read() };
                        slot.sequence.store(position.wrapping_add(self.mask).wrapping_add(1), Ordering::Release);
                        return Some(value);
                    }
                    Err(current) => position = current,
                }
            } else if ahead < 0 {
                return None;
            } else {
                position = self.pop_at.load(Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn values_come_out_in_the_order_they_went_in_and_a_full_queue_refuses() {
        let queue = Queue::new(4);
        assert_eq!(queue.pop(), None::<u32>);
        for value in 0..4 {
            assert!(queue.push(value));
        }
        assert!(!queue.push(99), "full");
        assert_eq!(queue.pop(), Some(0));
        assert!(queue.push(4), "room again");
        assert_eq!((1..=4).map(|_| queue.pop().unwrap()).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(queue.pop(), None);
        // Round and round, past the end of the slots many times.
        for value in 0..1_000u32 {
            assert!(queue.push(value));
            assert_eq!(queue.pop(), Some(value));
        }
    }

    #[test]
    fn several_threads_push_while_one_pops_and_nothing_is_lost_or_doubled() {
        const THREADS: u64 = 4;
        const EACH: u64 = 20_000;
        let queue = Arc::new(Queue::new(256));
        let pushers: Vec<_> = (0..THREADS)
            .map(|thread| {
                let queue = queue.clone();
                std::thread::spawn(move || {
                    for index in 0..EACH {
                        while !queue.push((thread, index)) {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();

        let mut next = [0u64; THREADS as usize];
        let mut seen = 0;
        while seen < THREADS * EACH {
            match queue.pop() {
                Some((thread, index)) => {
                    assert_eq!(index, next[thread as usize], "each thread's values stay in order");
                    next[thread as usize] += 1;
                    seen += 1;
                }
                None => std::thread::yield_now(),
            }
        }
        for pusher in pushers {
            pusher.join().unwrap();
        }
        assert_eq!(queue.pop(), None);
    }
}
