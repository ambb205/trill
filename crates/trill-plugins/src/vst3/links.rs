//! Parameter values in transit between the two halves of a hosted plugin.
//!
//! A VST3 plugin is two objects that never talk about parameters directly:
//! the processor on the audio thread and the edit controller on the main
//! thread. The host carries every value between them. What the engine sets
//! on the processor must be told to the controller, or the controller
//! (which is what answers "what is this parameter now?") never learns of
//! it; what the plugin's own editor changes arrives at the controller and
//! must be told to the processor, or the audio never hears it.
//!
//! Both directions cross between a thread that may not wait and one that
//! may, so each is a [`Mailbox`]: a slot per parameter holding the latest
//! value, written and read with atomics only.
//!
//! The edits made in the plugin's editor are also of interest outside the
//! plugin: the session wants to know that the user turned a knob, and when
//! they took hold of it and let go. For that every one of them, not just
//! the latest value, is kept in a queue as well.

use crate::edits::EditQueue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use vst3::Steinberg::Vst::ParamID;

/// The latest value posted for each parameter, until it is collected.
///
/// Posting never waits and never fails: a value that has not been
/// collected yet is simply replaced, which is right for parameter values,
/// where only the newest matters. Any number of threads may post; one at a
/// time collects.
pub(crate) struct Mailbox {
    slots: Box<[Slot]>,
    /// Whether any slot may be dirty, so that collecting an empty mailbox
    /// costs one load instead of one per parameter.
    pending: AtomicBool,
}

struct Slot {
    /// The posted value, as `f64` bits.
    value: AtomicU64,
    dirty: AtomicBool,
}

impl Mailbox {
    fn new(slots: usize) -> Self {
        let slot = || Slot { value: AtomicU64::new(0), dirty: AtomicBool::new(false) };
        Self { slots: (0..slots).map(|_| slot()).collect(), pending: AtomicBool::new(false) }
    }

    /// Leaves `value` for parameter `slot`. A slot that does not exist is
    /// ignored.
    pub(crate) fn post(&self, slot: usize, value: f64) {
        let Some(slot) = self.slots.get(slot) else { return };
        // The value goes in before the flags that announce it, so whoever
        // sees a flag also sees the value.
        slot.value.store(value.to_bits(), Ordering::Release);
        slot.dirty.store(true, Ordering::Release);
        self.pending.store(true, Ordering::Release);
    }

    /// Hands every value posted since the last call to `take`, as
    /// `(slot, value)`.
    pub(crate) fn collect(&self, mut take: impl FnMut(usize, f64)) {
        if !self.pending.swap(false, Ordering::AcqRel) {
            return;
        }
        for (index, slot) in self.slots.iter().enumerate() {
            // A value posted while this runs is either seen here or leaves
            // `pending` set for the next call; it cannot be lost.
            if slot.dirty.load(Ordering::Acquire) && slot.dirty.swap(false, Ordering::AcqRel) {
                take(index, f64::from_bits(slot.value.load(Ordering::Acquire)));
            }
        }
    }
}

/// What the audio-thread half and the main-thread half of one plugin
/// instance share.
pub(crate) struct ParamLinks {
    /// The VST3 id of every parameter the controller has, listed or not.
    /// A parameter's position here is its slot in the mailboxes.
    ids: Box<[ParamID]>,
    slots: HashMap<ParamID, usize>,
    /// Edits made on the controller's side, on their way to the processor.
    pub to_processor: Mailbox,
    /// Values the processor has taken on, on their way to the controller:
    /// those the engine set, and those the plugin reports having changed
    /// by itself.
    pub to_controller: Mailbox,
    /// What the user did in the plugin's editor, in order and with the
    /// start and end of each gesture, for the device's controller to
    /// report. Values are normalized and filed under the parameter's slot.
    pub edits: EditQueue,
    /// Set by the audio thread when the plugin failed a block and was
    /// bypassed, for the main thread to report.
    failed: AtomicBool,
}

impl ParamLinks {
    /// `ids` are the plugin's parameter ids, each once.
    pub(crate) fn new(ids: Vec<ParamID>) -> Self {
        Self {
            slots: ids.iter().enumerate().map(|(slot, id)| (*id, slot)).collect(),
            to_processor: Mailbox::new(ids.len()),
            to_controller: Mailbox::new(ids.len()),
            edits: EditQueue::new(),
            ids: ids.into_boxed_slice(),
            failed: AtomicBool::new(false),
        }
    }

    /// How many parameters the plugin has.
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    pub(crate) fn id(&self, slot: usize) -> Option<ParamID> {
        self.ids.get(slot).copied()
    }

    /// The slot of the parameter with this id. Looking it up does not
    /// allocate, so the audio thread may.
    pub(crate) fn slot(&self, id: ParamID) -> Option<usize> {
        self.slots.get(&id).copied()
    }

    pub(crate) fn report_failure(&self) {
        self.failed.store(true, Ordering::Release);
    }

    /// Whether a failure was reported since the last call.
    pub(crate) fn take_failure(&self) -> bool {
        self.failed.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn collected(mailbox: &Mailbox) -> Vec<(usize, f64)> {
        let mut values = Vec::new();
        mailbox.collect(|slot, value| values.push((slot, value)));
        values
    }

    #[test]
    fn only_the_latest_value_of_each_parameter_is_delivered_once() {
        let mailbox = Mailbox::new(4);
        assert_eq!(collected(&mailbox), []);

        mailbox.post(2, 0.25);
        mailbox.post(0, 0.5);
        mailbox.post(2, 0.75);
        mailbox.post(9, 1.0); // no such parameter
        assert_eq!(collected(&mailbox), [(0, 0.5), (2, 0.75)]);
        assert_eq!(collected(&mailbox), [], "collected values are gone");

        mailbox.post(2, 0.75);
        assert_eq!(collected(&mailbox), [(2, 0.75)], "the same value again is a new delivery");
    }

    #[test]
    fn nothing_posted_from_another_thread_is_lost() {
        let mailbox = Arc::new(Mailbox::new(8));
        let poster = {
            let mailbox = mailbox.clone();
            std::thread::spawn(move || {
                for round in 1..=20_000u32 {
                    mailbox.post((round % 8) as usize, f64::from(round));
                }
            })
        };
        // Whatever interleaving happens, the last value posted to each
        // slot must be the last one collected for it.
        let mut latest = [0.0; 8];
        while !poster.is_finished() {
            mailbox.collect(|slot, value| latest[slot] = value);
        }
        poster.join().unwrap();
        mailbox.collect(|slot, value| latest[slot] = value);
        let expected: Vec<f64> = (0..8).map(|slot| f64::from(19_993 + (slot + 7) % 8)).collect();
        assert_eq!(latest.to_vec(), expected);
    }

    #[test]
    fn parameters_are_addressed_by_slot_and_by_id() {
        let links = ParamLinks::new(vec![100, 7, 4_000_000_000]);
        assert_eq!(links.len(), 3);
        assert_eq!((links.slot(7), links.slot(4_000_000_000), links.slot(8)), (Some(1), Some(2), None));
        assert_eq!((links.id(0), links.id(2), links.id(3)), (Some(100), Some(4_000_000_000), None));

        links.to_processor.post(1, 0.5);
        assert_eq!(collected(&links.to_processor), [(1, 0.5)]);
        assert_eq!(collected(&links.to_controller), [], "the directions are separate");
    }

    #[test]
    fn a_failure_is_recorded_until_taken() {
        let links = ParamLinks::new(Vec::new());
        assert!(!links.take_failure());
        links.report_failure();
        assert!(links.take_failure());
        assert!(!links.take_failure());
    }
}
