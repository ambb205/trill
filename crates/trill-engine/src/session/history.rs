//! What an undo step is: the properties it set, each with what it was
//! before, and not the song as it was.
//!
//! A step that keeps the whole song can only put the whole song back,
//! which takes everyone else's work with it. One that keeps what it set
//! puts back just that, and only where it is still as the step left it:
//! a property someone else has set since stays theirs, and the undo says
//! so. A thing the step made goes, even if someone has worked in it since
//! (it is held, with their work, and they can bring it back); a thing the
//! step removed comes back with whatever was done in it meanwhile. That is
//! the one way steps are kept, for someone working alone too.

use trill_core::merge::{Doc, Key, Op, Replica, Stamp, Won};

/// One property a step set.
#[derive(Clone, Debug)]
pub(super) struct Record {
    pub key: Key,
    /// What it was before the step.
    pub before: Op,
    /// What the step set it to.
    pub after: Op,
}

/// What a step did.
#[derive(Clone, Debug, Default)]
pub(super) struct Done {
    pub records: Vec<Record>,
}

impl Done {
    /// What the parts of a change that were taken in did.
    pub fn of(won: &[Won]) -> Done {
        let mut records = Vec::new();
        for part in won {
            match part.op.key() {
                Some(key) => {
                    // (Nothing is kept of a property of something that
                    // cannot have one: there is nothing to put back.)
                    if let Some(before) = part.was.clone() {
                        records.push(Record { key, before, after: part.op.clone() });
                    }
                }
                // A thing that was made: taking it back removes it.
                None => {
                    for thing in part.op.makes() {
                        let (key, before, after) = (thing.alive(), thing.set_alive(false), thing.set_alive(true));
                        records.push(Record { key, before, after });
                    }
                }
            }
        }
        Done { records }
    }

    /// Makes this what the two did, one after the other: a drag's further
    /// edit, or the steps of a group.
    pub fn and(&mut self, later: Done) {
        for record in later.records {
            match self.records.iter_mut().find(|held| held.key == record.key) {
                Some(held) => held.after = record.after,
                None => self.records.push(record),
            }
        }
    }

    /// Whether the step left everything as it was.
    pub fn is_nothing(&self) -> bool {
        self.records.iter().all(|record| record.before == record.after)
    }

    /// What taking the step back sets, and how many of the properties it
    /// set stay as someone else has set them since. `mine` are the
    /// numbers this program has written the song under.
    pub fn back(&self, doc: &Doc, mine: &[Replica]) -> (Vec<Op>, usize) {
        self.turn(doc, mine, |record| (&record.after, &record.before))
    }

    /// What bringing the step back sets, and how many properties stay as
    /// someone else has set them since.
    pub fn again(&self, doc: &Doc, mine: &[Replica]) -> (Vec<Op>, usize) {
        self.turn(doc, mine, |record| (&record.before, &record.after))
    }

    /// A property is turned from what the step left it at to the other
    /// side, if it is still what the step left it at and nobody else has
    /// set it since.
    fn turn(&self, doc: &Doc, mine: &[Replica], sides: impl Fn(&Record) -> (&Op, &Op)) -> (Vec<Op>, usize) {
        let mut ops = Vec::new();
        let mut theirs = 0;
        for record in &self.records {
            let (from, to) = sides(record);
            if from == to {
                continue;
            }
            let held = doc.read(&record.key);
            let stamp = doc.stamp(&record.key);
            let own = stamp == Stamp::ZERO || mine.contains(&stamp.replica());
            if held.as_ref() == Some(from) && own {
                ops.push(to.clone());
            } else if held.as_ref() != Some(to) {
                theirs += 1;
            }
        }
        (ops, theirs)
    }

    /// A property was set by something that is no step (a plugin's state
    /// read back, a knob turned in its own window): if this step had set
    /// it to what it was until then, the step now ends at what it is, so
    /// that taking the step back still does, and bringing it back does
    /// not turn the knob. `ahead` for a step that has been taken back:
    /// there it is what the step began from that follows. Returns whether
    /// the step had the property at all: the latest step that has is the
    /// only one that follows.
    pub fn follow(&mut self, part: &Won, ahead: bool) -> bool {
        let Some(key) = part.op.key() else { return false };
        let Some(record) = self.records.iter_mut().find(|record| record.key == key) else { return false };
        let side = if ahead { &mut record.before } else { &mut record.after };
        if Some(&*side) == part.was.as_ref() {
            *side = part.op.clone();
        }
        true
    }
}
