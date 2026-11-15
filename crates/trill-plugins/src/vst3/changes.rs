//! Parameter changes as they travel with a block: the host's implementation
//! of `IParameterChanges` and `IParamValueQueue`.
//!
//! `process` takes two of these lists. One carries the host's changes to
//! the plugin; the plugin fills the other with the changes it made itself.
//! Plugins commonly use the output list without checking that there is
//! one, so both are always passed.
//!
//! Everything a block needs is allocated when the list is built, and every
//! field is an atomic: the lists are used on the audio thread, where
//! nothing may allocate or wait, and stay sound whatever thread a plugin
//! calls them from.

use super::com::{interface, write_out};
use super::links::ParamLinks;
use super::params::{Mapping, ParamSpec};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use vst3::Steinberg::Vst::{
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, ParamID, ParamValue,
};
use vst3::Steinberg::{int32, kResultFalse, kResultOk, tresult};
use vst3::{Class, ComWrapper};

/// The changes of one parameter within a block.
///
/// VST3 lets a queue hold a curve of points across the block. This host
/// sets parameters at the start of a block and only wants to know where
/// the plugin left them at its end, so a queue remembers a single point,
/// the last one added, however many were.
pub(crate) struct ParamQueue {
    id: AtomicU32,
    /// How many points were added; only the last is kept.
    points: AtomicI32,
    offset: AtomicI32,
    /// The last point's value, as `f64` bits.
    value: AtomicU64,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ParamQueue {
    fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            id: AtomicU32::new(0),
            points: AtomicI32::new(0),
            offset: AtomicI32::new(0),
            value: AtomicU64::new(0),
        })
    }

    /// Makes this the queue of parameter `id`, with no points.
    fn assign(&self, id: ParamID) {
        self.id.store(id, Ordering::Relaxed);
        self.points.store(0, Ordering::Relaxed);
    }

    fn add(&self, offset: int32, value: ParamValue) -> int32 {
        self.offset.store(offset, Ordering::Relaxed);
        self.value.store(value.to_bits(), Ordering::Relaxed);
        self.points.fetch_add(1, Ordering::Relaxed)
    }

    fn count(&self) -> int32 {
        self.points.load(Ordering::Relaxed).max(0)
    }

    fn last(&self) -> (int32, ParamValue) {
        (self.offset.load(Ordering::Relaxed), f64::from_bits(self.value.load(Ordering::Relaxed)))
    }
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.load(Ordering::Relaxed)
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.count()
    }

    /// Every index that exists answers with the point that was kept.
    unsafe fn getPoint(&self, index: int32, sample_offset: *mut int32, value: *mut ParamValue) -> tresult {
        if !(0..self.count()).contains(&index) {
            return kResultFalse;
        }
        let (offset, point) = self.last();
        // SAFETY: each pointer is null or the caller's variable of that type.
        unsafe {
            write_out(sample_offset, offset);
            write_out(value, point);
        }
        kResultOk
    }

    unsafe fn addPoint(&self, sample_offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let added = self.add(sample_offset, value);
        // SAFETY: `index` is null or the caller's `int32`.
        unsafe { write_out(index, added) };
        kResultOk
    }
}

/// The parameter changes of one block: one queue per changed parameter.
pub(crate) struct ParamChanges {
    /// All the queues there will ever be; the first `used` are in use.
    queues: Box<[ComWrapper<ParamQueue>]>,
    used: AtomicUsize,
    /// Handed out once `queues` are used up. A plugin that is given no
    /// queue would, as often as not, write through the null pointer.
    spare: ComWrapper<ParamQueue>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    /// A list with room for changes to `capacity` different parameters in
    /// one block.
    pub(crate) fn with_capacity(capacity: usize) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            queues: (0..capacity).map(|_| ParamQueue::new()).collect(),
            used: AtomicUsize::new(0),
            spare: ParamQueue::new(),
        })
    }

    /// Empties the list for the next block.
    pub(crate) fn clear(&self) {
        self.used.store(0, Ordering::Relaxed);
    }

    fn in_use(&self) -> &[ComWrapper<ParamQueue>] {
        &self.queues[..self.used.load(Ordering::Relaxed).min(self.queues.len())]
    }

    /// Takes the next unused queue for parameter `id`, with its index.
    fn claim(&self, id: ParamID) -> Option<(usize, &ComWrapper<ParamQueue>)> {
        let claim = |used: usize| (used < self.queues.len()).then_some(used + 1);
        let index = self.used.fetch_update(Ordering::Relaxed, Ordering::Relaxed, claim).ok()?;
        let queue = &self.queues[index];
        queue.assign(id);
        Some((index, queue))
    }

    /// Sets parameter `id` to `value` from the start of the block. Returns
    /// whether there was room. Each parameter is pushed at most once per
    /// block.
    pub(crate) fn push(&self, id: ParamID, value: ParamValue) -> bool {
        self.claim(id).map(|(_, queue)| queue.add(0, value)).is_some()
    }

    /// Calls `each` with the parameter and final value of every queue that
    /// holds a point.
    pub(crate) fn for_each(&self, mut each: impl FnMut(ParamID, ParamValue)) {
        for queue in self.in_use().iter().filter(|queue| queue.count() > 0) {
            each(queue.id.load(Ordering::Relaxed), queue.last().1);
        }
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.in_use().len() as int32
    }

    /// The pointer is borrowed, as the VST3 API has it: it does not count
    /// as a reference, and is valid until the block is over.
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let queue = usize::try_from(index).ok().and_then(|index| self.in_use().get(index));
        queue.map_or(null_mut(), interface)
    }

    unsafe fn addParameterData(&self, id: *const ParamID, index: *mut int32) -> *mut IParamValueQueue {
        if id.is_null() {
            return null_mut();
        }
        // SAFETY: `id` is not null, so it points to the caller's `ParamID`.
        let id = unsafe { id.read_unaligned() };
        // A parameter has one queue per block, however often it is asked
        // for.
        let existing = self.in_use().iter().position(|queue| queue.id.load(Ordering::Relaxed) == id);
        let (position, queue) = match existing {
            Some(position) => (position, &self.queues[position]),
            None => self.claim(id).unwrap_or_else(|| {
                self.spare.assign(id);
                (self.queues.len(), &self.spare)
            }),
        };
        // SAFETY: `index` is null or the caller's `int32`.
        unsafe { write_out(index, position as int32) };
        interface(queue)
    }
}

/// The latest value set for each parameter since the last block.
struct Pending {
    /// Normalized values by slot, valid where `dirty` is set.
    values: Vec<f64>,
    dirty: Vec<bool>,
    /// The dirty slots, in the order they were first set. One entry per
    /// parameter at most, so it never outgrows its capacity.
    order: Vec<usize>,
}

impl Pending {
    fn new(slots: usize) -> Self {
        Self { values: vec![0.0; slots], dirty: vec![false; slots], order: Vec::with_capacity(slots) }
    }

    /// A slot the plugin does not have is ignored.
    fn set(&mut self, slot: usize, normalized: f64) {
        let (Some(value), Some(dirty)) = (self.values.get_mut(slot), self.dirty.get_mut(slot)) else { return };
        *value = normalized;
        if !std::mem::replace(dirty, true) {
            self.order.push(slot);
        }
    }

    /// Hands every value set since the last call to `take`, as
    /// `(slot, value)`, in the order the slots were first set.
    fn drain(&mut self, mut take: impl FnMut(usize, f64)) {
        for slot in self.order.drain(..) {
            self.dirty[slot] = false;
            take(slot, self.values[slot]);
        }
    }
}

/// Turns what reaches the audio thread about parameters (the engine's
/// `set_param`, edits from the plugin's own editor, resets) into the
/// parameter changes of each block, and passes on what the plugin changed
/// by itself.
pub(crate) struct BlockParams {
    /// The slot and mapping of each listed parameter, in the order
    /// `set_param` indexes them.
    listed: Vec<(usize, Mapping)>,
    links: Arc<ParamLinks>,
    pending: Pending,
    /// The parameters that stand for MIDI's "all sound off".
    silencing: Vec<ParamID>,
    input: ComWrapper<ParamChanges>,
    output: ComWrapper<ParamChanges>,
}

impl BlockParams {
    /// `listed` are the device's parameters in its order; `silencing` the
    /// parameters that silence the plugin when set.
    pub(crate) fn new(listed: &[ParamSpec], links: Arc<ParamLinks>, silencing: Vec<ParamID>) -> Self {
        let slots = links.len();
        Self {
            listed: listed.iter().map(|spec| (spec.slot, spec.mapping)).collect(),
            pending: Pending::new(slots),
            input: ParamChanges::with_capacity(slots + silencing.len()),
            output: ParamChanges::with_capacity(slots),
            silencing,
            links,
        }
    }

    /// The lists to pass to `process`: the host's changes, and the one the
    /// plugin reports its own in. They are the same for every block.
    pub(crate) fn lists(&self) -> (*mut IParameterChanges, *mut IParameterChanges) {
        (interface(&self.input), interface(&self.output))
    }

    /// Records a value for the listed parameter `index`, in plain units,
    /// for the next block. Setting a parameter several times between
    /// blocks sends only the last value.
    pub(crate) fn set(&mut self, index: usize, plain: f64) {
        let Some(&(slot, mapping)) = self.listed.get(index) else { return };
        if !plain.is_finite() {
            return;
        }
        let normalized = mapping.normalize(plain);
        self.pending.set(slot, normalized);
        // The plugin's controller keeps its own copy of every value, and
        // is what answers when the session asks for them.
        self.links.to_controller.post(slot, normalized);
    }

    /// Builds the parameter changes of the next block. `silence` adds the
    /// plugin's "all sound off".
    pub(crate) fn fill(&mut self, silence: bool) {
        self.input.clear();
        self.output.clear();
        // What the user changed in the plugin's own editor since the last
        // block.
        self.links.to_processor.collect(|slot, value| self.pending.set(slot, value));
        self.pending.drain(|slot, value| {
            if let Some(id) = self.links.id(slot) {
                self.input.push(id, value);
            }
        });
        if silence {
            for &id in &self.silencing {
                self.input.push(id, 0.0);
            }
        }
    }

    /// Notes that the plugin failed a block, for the main thread to
    /// report.
    pub(crate) fn report_failure(&self) {
        self.links.report_failure();
    }

    /// Passes the values the plugin changed during the block on to its
    /// controller.
    pub(crate) fn collect(&self) {
        self.output.for_each(|id, value| {
            if let Some(slot) = self.links.slot(id).filter(|_| value.is_finite()) {
                self.links.to_controller.post(slot, value.clamp(0.0, 1.0));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::{ComPtr, ComRef};

    /// The list the way a plugin sees it: through its COM interface.
    fn list(capacity: usize) -> (ComWrapper<ParamChanges>, ComPtr<IParameterChanges>) {
        let changes = ParamChanges::with_capacity(capacity);
        let interface = changes.to_com_ptr::<IParameterChanges>().expect("the class implements IParameterChanges");
        (changes, interface)
    }

    /// Every queue of the list as `(parameter, point count, last offset, last value)`.
    fn queues(list: &ComPtr<IParameterChanges>) -> Vec<(ParamID, int32, int32, f64)> {
        // SAFETY: the list's queues are valid for as long as the list is,
        // and the out-parameters are local variables.
        unsafe {
            (0..list.getParameterCount())
                .map(|index| {
                    let queue = ComRef::from_raw(list.getParameterData(index)).expect("a queue for every index");
                    let count = queue.getPointCount();
                    let (mut offset, mut value) = (-1, f64::NAN);
                    assert_eq!(queue.getPoint(count - 1, &mut offset, &mut value), kResultOk);
                    (queue.getParameterId(), count, offset, value)
                })
                .collect()
        }
    }

    /// What a plugin does to report a change.
    fn add(list: &ComPtr<IParameterChanges>, id: ParamID, offset: int32, value: f64) -> (int32, int32) {
        let (mut queue_index, mut point_index) = (-1, -1);
        // SAFETY: the pointers are to local variables, and the queue is
        // one of the list's own.
        unsafe {
            let queue = ComRef::from_raw(list.addParameterData(&id, &mut queue_index)).expect("never null");
            assert_eq!(queue.addPoint(offset, value, &mut point_index), kResultOk);
        }
        (queue_index, point_index)
    }

    #[test]
    fn the_hosts_changes_reach_the_plugin_as_one_point_at_the_start_of_the_block() {
        let (changes, list) = list(4);
        assert!(changes.push(7, 0.25));
        assert!(changes.push(4_000_000_000, 1.0));
        assert_eq!(queues(&list), [(7, 1, 0, 0.25), (4_000_000_000, 1, 0, 1.0)]);

        changes.clear();
        assert_eq!(queues(&list), []);
        assert!(changes.push(9, 0.5));
        assert_eq!(queues(&list), [(9, 1, 0, 0.5)], "queues are reused, not accumulated");
    }

    #[test]
    fn the_plugins_changes_are_kept_as_the_last_point_of_each_parameter() {
        let (changes, list) = list(4);
        assert_eq!(add(&list, 3, 0, 0.1), (0, 0));
        assert_eq!(add(&list, 5, 10, 0.5), (1, 0));
        assert_eq!(add(&list, 3, 63, 0.9), (0, 1), "the same parameter gets the same queue");
        assert_eq!(queues(&list), [(3, 2, 63, 0.9), (5, 1, 10, 0.5)]);

        let mut seen = Vec::new();
        changes.for_each(|id, value| seen.push((id, value)));
        assert_eq!(seen, [(3, 0.9), (5, 0.5)]);

        // A queue asked for but never filled reports nothing.
        let id: ParamID = 8;
        // SAFETY: `id` is a local variable; the index is not wanted.
        assert!(!unsafe { list.addParameterData(&id, null_mut()) }.is_null());
        seen.clear();
        changes.for_each(|id, value| seen.push((id, value)));
        assert_eq!(seen, [(3, 0.9), (5, 0.5)]);
    }

    #[test]
    fn a_full_list_drops_changes_but_never_hands_out_null() {
        let (changes, list) = list(2);
        assert!(changes.push(1, 0.1));
        assert!(changes.push(2, 0.2));
        assert!(!changes.push(3, 0.3), "no room: the host is told");

        // The plugin gets a queue it can write to, which is not part of
        // the list.
        assert_eq!(add(&list, 4, 5, 0.4), (2, 0));
        assert_eq!(queues(&list), [(1, 1, 0, 0.1), (2, 1, 0, 0.2)]);
        // A parameter that has a queue still finds it.
        assert_eq!(add(&list, 2, 9, 0.9).0, 1);

        let (_, empty) = self::list(0);
        assert_eq!(add(&empty, 1, 0, 1.0), (0, 0));
        assert_eq!(queues(&empty), []);
    }

    #[test]
    fn nonsense_arguments_are_refused_not_dereferenced() {
        let (changes, list) = list(2);
        changes.push(1, 0.5);
        // SAFETY: every pointer passed is null or to a local variable.
        unsafe {
            assert!(list.addParameterData(std::ptr::null(), null_mut()).is_null());
            assert!(list.getParameterData(-1).is_null());
            assert!(list.getParameterData(1).is_null());
            let queue = ComRef::from_raw(list.getParameterData(0)).unwrap();
            let mut value = 0.0;
            assert_eq!(queue.getPoint(1, null_mut(), &mut value), kResultFalse);
            assert_eq!(queue.getPoint(-1, null_mut(), &mut value), kResultFalse);
            assert_eq!(queue.getPoint(0, null_mut(), null_mut()), kResultOk);
            assert_eq!(queue.addPoint(0, 1.0, null_mut()), kResultOk);
        }
    }

    #[test]
    fn building_a_block_never_allocates_new_queues() {
        let (changes, list) = list(3);
        // SAFETY: index 0 exists once something is pushed.
        let first = |list: &ComPtr<IParameterChanges>| unsafe { list.getParameterData(0) };
        changes.push(1, 0.0);
        let address = first(&list);
        for round in 0..5 {
            changes.clear();
            changes.push(round, 0.5);
            add(&list, 100 + round, 0, 1.0);
            assert_eq!(first(&list), address);
        }
    }

    use trill_core::device::{ParamInfo, ParamKind};

    fn spec(id: ParamID, slot: usize, steps: u32) -> ParamSpec {
        let mapping = if steps == 0 { Mapping::Continuous } else { Mapping::Stepped { steps } };
        let info = ParamInfo {
            id: id.to_string(),
            name: format!("P{id}"),
            min: 0.0,
            max: f64::from(steps.max(1)),
            default: 0.0,
            unit: String::new(),
            kind: if steps == 0 { ParamKind::Float } else { ParamKind::Int },
        };
        ParamSpec { id, slot, info, mapping }
    }

    /// A plugin with four parameters, of which the device lists two: a
    /// continuous one (id 20) and one with four steps (id 40).
    fn feed() -> (BlockParams, Arc<ParamLinks>) {
        let links = Arc::new(ParamLinks::new(vec![10, 20, 30, 40]));
        let listed = [spec(20, 1, 0), spec(40, 3, 4)];
        (BlockParams::new(&listed, links.clone(), vec![777]), links)
    }

    /// The block's input changes as the plugin reads them.
    fn sent(feed: &BlockParams) -> Vec<(ParamID, f64)> {
        // SAFETY: the list is the feed's own, alive as long as it is.
        let list = unsafe { ComRef::from_raw(feed.lists().0) }.expect("an input list");
        let list = list.to_com_ptr();
        let single = |(id, count, offset, value)| {
            assert_eq!((count, offset), (1, 0), "one point, at the start of the block");
            (id, value)
        };
        queues(&list).into_iter().map(single).collect()
    }

    fn posted(mailbox: &crate::vst3::links::Mailbox) -> Vec<(usize, f64)> {
        let mut values = Vec::new();
        mailbox.collect(|slot, value| values.push((slot, value)));
        values
    }

    #[test]
    fn set_values_are_normalized_sent_once_and_told_to_the_controller() {
        let (mut feed, links) = feed();
        feed.set(1, 1.0); // one of four steps
        feed.set(0, 0.1);
        feed.set(0, 0.9); // the last value wins
        feed.set(1, 3.0);
        feed.set(2, 1.0); // out of range: ignored
        feed.set(0, f64::NAN); // not a value: ignored

        feed.fill(false);
        assert_eq!(sent(&feed), [(40, 0.75), (20, 0.9)], "in the order they were first set");
        assert_eq!(posted(&links.to_controller), [(1, 0.9), (3, 0.75)]);

        // Sent once: the next block has no changes.
        feed.fill(false);
        assert_eq!(sent(&feed), []);
        assert_eq!(posted(&links.to_controller), []);
    }

    #[test]
    fn edits_from_the_plugins_editor_reach_the_processor_but_are_not_echoed() {
        let (mut feed, links) = feed();
        links.to_processor.post(0, 0.25); // a parameter the device does not even list
        links.to_processor.post(3, 0.5);
        feed.set(0, 0.6);
        feed.fill(false);
        assert_eq!(sent(&feed), [(20, 0.6), (10, 0.25), (40, 0.5)]);
        assert_eq!(posted(&links.to_controller), [(1, 0.6)], "the controller already has its own edits");

        // An edit and a set value for the same parameter make one change.
        links.to_processor.post(1, 0.3);
        feed.set(0, 0.7);
        feed.fill(false);
        assert_eq!(sent(&feed), [(20, 0.3)]);
    }

    #[test]
    fn silencing_sets_the_all_sound_off_parameters() {
        let (mut feed, _links) = feed();
        feed.set(0, 0.5);
        feed.fill(true);
        assert_eq!(sent(&feed), [(20, 0.5), (777, 0.0)]);
        feed.fill(false);
        assert_eq!(sent(&feed), []);
    }

    #[test]
    fn what_the_plugin_changes_by_itself_is_told_to_the_controller() {
        let (mut feed, links) = feed();
        feed.fill(false);
        // SAFETY: the list is the feed's own, alive as long as it is.
        let output = unsafe { ComRef::from_raw(feed.lists().1) }.expect("an output list").to_com_ptr();
        add(&output, 30, 0, 0.1);
        add(&output, 30, 12, 0.4);
        add(&output, 10, 5, 7.0);
        add(&output, 999, 0, 0.5); // not a parameter of this plugin
        add(&output, 20, 0, f64::NAN);
        feed.collect();
        assert_eq!(posted(&links.to_controller), [(0, 1.0), (2, 0.4)]);

        // The next block starts with an empty output list.
        feed.fill(false);
        feed.collect();
        assert_eq!(posted(&links.to_controller), []);
    }

    #[test]
    fn a_plugin_without_parameters_still_gets_both_lists() {
        let links = Arc::new(ParamLinks::new(Vec::new()));
        let mut feed = BlockParams::new(&[], links, Vec::new());
        feed.set(0, 1.0);
        feed.fill(true);
        let (input, output) = feed.lists();
        assert!(!input.is_null() && !output.is_null());
        assert_eq!(sent(&feed), []);
        feed.collect();
    }
}
