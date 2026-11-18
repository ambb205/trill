//! A block made on more than one thread.
//!
//! Most of what a block costs is what its tracks' devices cost, and one
//! thread makes them one after the other: a song of seventeen plugins
//! took twice as long to render as to play, and one of nineteen could
//! not be played at all in the time it lasts. Tracks that do not feed one
//! another can be made at once, and here they are: for a render on the
//! threads of a [`Crew`], which sleep until there is work, and for a song
//! that plays on those of a [`crate::team::Team`], which are audio
//! threads and never take a lock. What is done is the same for both
//! ([`Rounds::block`]); only who does it differs ([`Workers`]).
//!
//! **The same samples.** What one thread makes and what many make is the
//! same, sample for sample, wherever the devices themselves do the same
//! thing twice. Three things see to that:
//!
//! - Every track is made by the same steps on the same numbers
//!   ([`crate::processor`]); only who runs them differs, and every thread
//!   that does treats very small numbers as the audio thread does. What a
//!   track is told ahead of its own notes is said by the thread that
//!   asked, for every track before any is made ([`crate::processor::prelude`]).
//! - Tracks are *added up* in the order of the graph, by one thread: a bus
//!   takes what its tracks send it in the order one thread would have
//!   sent it, because adding the same numbers in another order does not
//!   give the same sum.
//! - Devices that keep something between them are run in turn. The
//!   instances of a plugin share its module, and some modules keep things
//!   for all their instances (where each is in a round of recordings,
//!   say). Unless a module is known to keep nothing of the kind, its
//!   instances are never run at once, and always in the order of the song
//!   ([`Beside::InTurn`]): what the module has seen when a block ends is
//!   what it would have seen on one thread. Only those devices take
//!   turns, and only where a song has more than one of a module. What
//!   comes after the last of them on a track (an amplifier behind a
//!   sampled guitar, which costs several times what the guitar does) is
//!   run beside everything else as soon as its track has had its turn:
//!   whoever gave the track its turn says that the rest of it is ready
//!   ([`Workers::more`]) and goes on to the next track's turn.
//!
//! **What is made when.** A track can be made once everything that feeds
//! it has been added up: the members of a group before the group, the
//! tracks that send to a return before the return, a track before the
//! tracks whose devices listen to it. [`plan`] walks the graph in its
//! order and makes, at once, every track that waits for nothing more;
//! then adds tracks up in order until it comes to one that is not made
//! yet, and makes at once what has become ready by then. A song of
//! instruments on tracks of their own, with a few groups and returns, is
//! two or three such rounds a block.
//!
//! A device that says it is not to be run beside anything
//! ([`Beside::Nothing`]) leaves the block on one thread: [`company`]
//! names it, for a render to say so.
//!
//! **Nothing is made ready while a block is.** The steps, the room every
//! track needs and what each takes turns with are worked out when the
//! graph is ([`Rounds::of`]), off the audio thread; making a block
//! allocates nothing and takes no lock, whoever the workers are. A share
//! of a round is taken with one atomic step, and a track is reached
//! through a pointer that only the thread which took its share follows.
//!
//! **A track stays with a thread** where it can. Each thread looks for a
//! share from its own place among the threads on, so that from one block
//! to the next a track is mostly made by the thread that made it last, on
//! a processor that still has what its devices keep.

use crate::crew::Crew;
use crate::graph::{Bus, Graph, TrackNode};
use crate::processor::{Common, DenormalGuard, Room, finish, mix, sound, tally, through};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;
use trill_core::device::{Beside, DeviceFactory};
use trill_core::{DeviceId, Project};

/// Threads that take shares of a round beside the one that asks.
pub(crate) trait Workers: Sync {
    /// How many there are beside the caller's.
    fn len(&self) -> usize;

    /// Runs `work` on the caller's thread and on up to `helpers` of these
    /// at once, and returns when every one of them has come back from it.
    /// `work` is written to be run by several threads at once, each taking
    /// a share of what there is to do until nothing is left; it is told
    /// which of the threads runs it, the caller's being 0 and the others
    /// counted from 1.
    fn all(&self, helpers: usize, work: &(dyn Fn(usize) + Sync));

    /// Says, from inside the work of [`Workers::all`], that there is one
    /// more thing to do at once than there was: one more thread may come
    /// to the work, and one that sleeps is woken for it.
    fn more(&self);
}

/// Where the rest of a track's devices stands in a round: its turn among
/// the devices that take turns is still to come, it has had it and
/// anybody may go on with the rest, or somebody has.
const NOT_YET: u8 = 0;
const READY: u8 = 1;
const TAKEN: u8 = 2;

/// One step of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Make these tracks, by their places in the graph: each list one
    /// after the other and in its order, the lists at once.
    Make(Vec<Vec<usize>>),
    /// Add the track at this place to where it goes.
    Add(usize),
}

/// Writes into `graph` what each of its devices may be run beside, as
/// `factory` says of it, and returns the devices that are not to be run
/// beside anything. With `trusting`, every device is taken to tolerate
/// anything, whatever is known of it: for finding out what one does. With
/// `adopted`, a device that the graph has no processor for counts like
/// any other: in a graph that is to play, the processor is the one that
/// is playing, and comes over as the graph is swapped in.
pub(crate) fn company(
    graph: &mut Graph,
    project: &Project,
    factory: &dyn DeviceFactory,
    (trusting, adopted): (bool, bool),
) -> Vec<DeviceId> {
    let (mut names, mut alone): (Vec<String>, Vec<DeviceId>) = (Vec::new(), Vec::new());
    let chains = graph.tracks.iter_mut().map(|track| &mut track.chain).chain([&mut graph.master.chain]);
    for slot in chains.flat_map(|chain| chain.devices.iter_mut()) {
        slot.turn = None;
        let Ok(device) = project.device(slot.id) else { continue };
        if trusting || (slot.processor.is_none() && !adopted) {
            continue;
        }
        match factory.beside(&device.uri) {
            Beside::Anything => {}
            Beside::Nothing => alone.push(slot.id),
            Beside::InTurn(name) => {
                let at = names.iter().position(|known| *known == name).unwrap_or_else(|| {
                    names.push(name);
                    names.len() - 1
                });
                slot.turn = Some(at as u32);
            }
        }
    }
    // A device that is the song's only one of its module has nothing to
    // take turns with: it is run beside anything.
    let mut of_each = vec![0usize; names.len()];
    let chains = graph.tracks.iter_mut().map(|track| &mut track.chain).chain([&mut graph.master.chain]);
    let slots: Vec<&mut crate::graph::DeviceSlot> = chains.flat_map(|chain| chain.devices.iter_mut()).collect();
    for turn in slots.iter().filter_map(|slot| slot.turn) {
        of_each[turn as usize] += 1;
    }
    for slot in slots {
        if slot.turn.is_some_and(|turn| of_each[turn as usize] < 2) {
            slot.turn = None;
        }
    }
    alone
}

/// The steps in which the blocks of `tracks` are made: see the module
/// documentation. Every track is made once and added once, and added in
/// the order of the graph.
pub(crate) fn plan(tracks: &[TrackNode]) -> Vec<Step> {
    let count = tracks.len();
    // The tracks each one waits for: whatever is added to the bus it
    // begins from, and whatever its devices listen to.
    let feeders: Vec<Vec<usize>> = tracks
        .iter()
        .map(|track| {
            let listens: Vec<usize> = track.chain.devices.iter().filter_map(|slot| slot.key).collect();
            let feeds = |other: &TrackNode| {
                let fed = track
                    .input_bus
                    .is_some_and(|bus| other.output_bus == Some(bus) || other.sends.iter().any(|send| send.bus == bus));
                fed || other.key_out.is_some_and(|key| listens.contains(&key))
            };
            (0..count).filter(|other| feeds(&tracks[*other])).collect()
        })
        .collect();
    let turns: Vec<Vec<u32>> =
        tracks.iter().map(|track| track.chain.devices.iter().filter_map(|slot| slot.turn).collect()).collect();

    let (mut steps, mut made, mut next) = (Vec::new(), vec![false; count], 0);
    while next < count {
        if made[next] {
            steps.push(Step::Add(next));
            next += 1;
            continue;
        }
        // Everything before `next` has been added up. What waits for
        // nothing after that can be made now, unless a track before it
        // that cannot keeps something with it: those keep their order.
        let (mut round, mut held): (Vec<usize>, Vec<u32>) = (Vec::new(), Vec::new());
        for track in (next..count).filter(|track| !made[*track]) {
            let ready = track == next || feeders[track].iter().all(|feeder| *feeder < next);
            if ready && !turns[track].iter().any(|turn| held.contains(turn)) {
                round.push(track);
            } else {
                held.extend(&turns[track]);
            }
        }
        // Those of a round that keep something between them go on one
        // list, in their order.
        let mut lists: Vec<(Vec<usize>, Vec<u32>)> = Vec::new();
        for track in round {
            made[track] = true;
            let mut joined = (vec![track], turns[track].clone());
            let mut apart = Vec::new();
            for list in lists.drain(..) {
                if list.1.iter().any(|turn| joined.1.contains(turn)) {
                    joined.0.extend(list.0);
                    joined.1.extend(list.1);
                } else {
                    apart.push(list);
                }
            }
            joined.0.sort_unstable();
            apart.push(joined);
            lists = apart;
        }
        lists.sort_by_key(|list| list.0[0]);
        steps.push(Step::Make(lists.into_iter().map(|list| list.0).collect()));
    }
    steps
}

/// How many of a track's devices, from the first, are run while the track
/// has its turn: up to the last one that takes turns with the devices of
/// other tracks. None where no device of it does.
fn turn_of(track: &TrackNode) -> usize {
    // (A device that takes turns is one that is there, or will be when
    // the graph plays: see `company`.)
    let taking = |slot: &crate::graph::DeviceSlot| slot.turn.is_some() && slot.enabled;
    track.chain.devices.iter().rposition(taking).map_or(0, |last| last + 1)
}

/// What it takes to make the blocks of a graph's tracks on more than one
/// thread: the steps, and for every track the room its block needs while
/// others are made beside it. Made when the graph is, off the audio
/// thread; nothing is made or let go of while a block is.
pub(crate) struct Rounds {
    steps: Vec<Step>,
    /// For every step, by its place among them: the tracks of a
    /// [`Step::Make`] that go on beside each other after their turn.
    rests: Vec<Vec<usize>>,
    /// For every track, by its place in the graph: where it is among the
    /// rests of its step, if it is.
    rest_at: Vec<usize>,
    /// For every track, by its place in the graph: see [`turn_of`].
    turns: Vec<usize>,
    /// For the lists of the step being made, by their places in it: that
    /// a thread has begun the list.
    begun: Vec<AtomicBool>,
    /// For the rests of the step being made, by their places among them:
    /// [`NOT_YET`], [`READY`] or [`TAKEN`].
    ready: Vec<AtomicU8>,
    /// Room for the block of every track, by its place in the graph.
    rooms: Vec<Room>,
    /// Where a track that is listened to leaves what it sounded like ahead
    /// of its fader, until it is put where the listeners look.
    kept: Vec<Option<Bus>>,
    /// The most there is to do at once in any step.
    width: usize,
}

/// The tracks of a block and what goes with each, for the threads that
/// take shares of a round: each follows these only to the tracks of the
/// share it took.
struct Places {
    tracks: *mut TrackNode,
    rooms: *mut Room,
    kept: *mut Option<Bus>,
    count: usize,
}

// SAFETY: what the pointers lead to is handed to other threads a track at
// a time, each to the one thread that took the share it is in
// ([`Rounds::block`]); a track with its devices, a room and a bus are
// each `Send`.
unsafe impl Sync for Places {}

impl Places {
    /// The track at `at` with its room and where it is listened to.
    ///
    /// # Safety
    ///
    /// No other thread may hold what this returns for the same `at`: it is
    /// called for a place by whoever took the share of the round that the
    /// place is in (a list, or after that the rest of the track, which is
    /// only there to be taken once the list's thread has let go of it), and
    /// what it returns is let go of before the round is over.
    #[allow(clippy::mut_from_ref)]
    unsafe fn at(&self, at: usize) -> Option<(&mut TrackNode, &mut Room, &mut Option<Bus>)> {
        if at >= self.count {
            return None;
        }
        // SAFETY: inside the three slices, which are `count` long and
        // outlive the round; nobody else holds this place (see above).
        unsafe { Some((&mut *self.tracks.add(at), &mut *self.rooms.add(at), &mut *self.kept.add(at))) }
    }
}

impl Rounds {
    /// The rounds in which the blocks of `tracks` are made.
    pub fn of(tracks: &[TrackNode]) -> Self {
        let steps = plan(tracks);
        let turns: Vec<usize> = tracks.iter().map(turn_of).collect();
        // What comes after a track's turn is nobody's but its own.
        let rest = |at: &usize| turns[*at] > 0 && turns[*at] < tracks[*at].chain.devices.len();
        let rests: Vec<Vec<usize>> = steps
            .iter()
            .map(|step| match step {
                Step::Make(lists) => lists.iter().flatten().copied().filter(|at| rest(at)).collect(),
                Step::Add(_) => Vec::new(),
            })
            .collect();
        let mut rest_at = vec![usize::MAX; tracks.len()];
        for (index, at) in rests.iter().flat_map(|rest| rest.iter().enumerate()) {
            rest_at[*at] = index;
        }
        // The most there is to do at once in a round: of every list one
        // track that has its turn, or what comes after the turns of its
        // tracks, whichever is more.
        let widest = steps.iter().map(|step| match step {
            Step::Make(lists) => lists.iter().map(|list| list.iter().filter(|at| rest(at)).count().max(1)).sum(),
            Step::Add(_) => 0,
        });
        let width = widest.max().unwrap_or(0);
        let lists = steps.iter().map(|step| if let Step::Make(lists) = step { lists.len() } else { 0 }).max();
        let most_rests = rests.iter().map(Vec::len).max().unwrap_or(0);
        Self {
            width,
            begun: (0..lists.unwrap_or(0)).map(|_| AtomicBool::new(false)).collect(),
            ready: (0..most_rests).map(|_| AtomicU8::new(NOT_YET)).collect(),
            rest_at,
            steps,
            rests,
            turns,
            rooms: tracks.iter().map(|_| Room::new()).collect(),
            kept: tracks.iter().map(|track| track.key_out.map(|_| Bus::new())).collect(),
        }
    }

    /// How many threads the blocks can use at the most: the most there is
    /// to do at once in any round. One where every track waits for
    /// another.
    pub fn width(&self) -> usize {
        self.width
    }

    /// The room of every track, by its place in the graph: what
    /// [`crate::processor::prelude`] fills before the block is made.
    pub fn rooms_mut(&mut self) -> &mut [Room] {
        &mut self.rooms
    }

    /// Takes over the rooms `old` has been making blocks in, as far as
    /// they go round: memory that has been used is there to be used
    /// again, where memory that was only just asked for may first have to
    /// be found by the system, in the middle of a block. Nothing is kept
    /// in a room from one block to the next. Realtime-safe: pointer moves.
    pub fn adopt(&mut self, old: &mut Rounds) {
        for (room, used) in self.rooms.iter_mut().zip(old.rooms.iter_mut()) {
            std::mem::swap(room, used);
        }
    }

    /// Makes one block of `tracks`, as one thread would have: the buses
    /// and the mix end up holding the same samples. The room of every
    /// track holds what it was told ahead of its notes
    /// ([`crate::processor::prelude`]). Returns how long the tracks took
    /// to make, added up over whoever made them, in nanoseconds.
    ///
    /// Nothing here allocates or takes a lock; whether anything waits is
    /// the workers' business ([`Workers::all`]).
    #[allow(clippy::too_many_arguments)]
    pub fn block(
        &mut self,
        workers: &dyn Workers,
        tracks: &mut [TrackNode],
        buses: &mut [Bus],
        keys: &mut [Bus],
        common: &Common,
        mix_left: &mut [f32],
        mix_right: &mut [f32],
    ) -> u64 {
        let frames = common.frames();
        let Rounds { steps, rests, rest_at, turns, rooms, kept, begun, ready, .. } = self;
        let count = tracks.len().min(rooms.len()).min(kept.len()).min(turns.len()).min(rest_at.len());
        let made = AtomicU64::new(0);
        for (step, rest) in steps.iter().zip(rests.iter()) {
            let lists = match step {
                Step::Add(at) => {
                    if *at < count {
                        mix(&mut tracks[*at], common, &mut rooms[*at], buses, mix_left, mix_right);
                    }
                    continue;
                }
                Step::Make(lists) => lists,
            };
            if lists.len() > begun.len() || rest.len() > ready.len() {
                continue;
            }
            {
                let places =
                    Places { tracks: tracks.as_mut_ptr(), rooms: rooms.as_mut_ptr(), kept: kept.as_mut_ptr(), count };
                let (begun, ready) = (&begun[..lists.len()], &ready[..rest.len()]);
                begun.iter().for_each(|begun| begun.store(false, Ordering::Relaxed));
                ready.iter().for_each(|ready| ready.store(NOT_YET, Ordering::Relaxed));
                let (places, buses, keys, turns, rest_at, made) = (&places, &*buses, &*keys, &*turns, &*rest_at, &made);
                // Every thread takes a list nobody has begun, and makes
                // its tracks one after the other and in its order: of each
                // the devices that take turns with those of the list's
                // other tracks and what comes before them, or all of them
                // where none does or nothing comes after. The rest of a
                // track is there for anybody as soon as its turn is over.
                // A thread looks from its own place on: see the module.
                let work = |me: usize| loop {
                    let free = (0..lists.len()).map(|step| (me + step) % lists.len()).find(|list| {
                        !begun[*list].load(Ordering::Relaxed) && !begun[*list].swap(true, Ordering::AcqRel)
                    });
                    if let Some(list) = free {
                        for at in &lists[list] {
                            let handed_on = {
                                // SAFETY: a track is on one list of a
                                // round, a list is begun by one thread,
                                // and the rest of the track is nobody's
                                // until this thread says it is ready,
                                // having let go of the track.
                                let Some((track, room, kept)) = (unsafe { places.at(*at) }) else { continue };
                                let began = Instant::now();
                                sound(track, common, buses, room);
                                let devices = track.chain.devices.len();
                                let turn = if turns[*at] == 0 { devices } else { turns[*at].min(devices) };
                                through(track, common, room, keys, 0..turn);
                                if turn == devices {
                                    finish(track, common, room, kept.as_mut());
                                }
                                room.spent += began.elapsed().as_nanos() as u64;
                                if turn == devices {
                                    made.fetch_add(tally(track, common, room), Ordering::Relaxed);
                                }
                                turn < devices
                            };
                            if let (true, Some(state)) = (handed_on, ready.get(rest_at[*at])) {
                                state.store(READY, Ordering::Release);
                                workers.more();
                            }
                        }
                        continue;
                    }
                    let waiting = (0..rest.len()).map(|step| (me + step) % rest.len()).find(|index| {
                        let state = &ready[*index];
                        state.load(Ordering::Acquire) == READY
                            && state.compare_exchange(READY, TAKEN, Ordering::AcqRel, Ordering::Acquire).is_ok()
                    });
                    // Nothing for this thread now. What is not ready yet
                    // is the business of whoever makes it ready, who looks
                    // again after and calls for one more.
                    let Some(index) = waiting else { break };
                    let at = rest[index];
                    // SAFETY: a track is in the rest of a round once, it
                    // was let go of before it was said to be ready, and
                    // one thread takes it from there.
                    let Some((track, room, kept)) = (unsafe { places.at(at) }) else { continue };
                    let began = Instant::now();
                    let devices = track.chain.devices.len();
                    through(track, common, room, keys, turns[at].min(devices)..devices);
                    finish(track, common, room, kept.as_mut());
                    room.spent += began.elapsed().as_nanos() as u64;
                    made.fetch_add(tally(track, common, room), Ordering::Relaxed);
                };
                if lists.len() < 2 && rest.is_empty() {
                    work(0);
                } else {
                    workers.all(lists.len() - 1, &work);
                }
            }
            // Nobody listened to these while they were made: whoever
            // does is made in a later round.
            for at in lists.iter().flatten().filter(|at| **at < count) {
                let key = tracks[*at].key_out.and_then(|key| keys.get_mut(key));
                if let (Some(key), Some(kept)) = (key, &kept[*at]) {
                    key.left[..frames].copy_from_slice(&kept.left[..frames]);
                    key.right[..frames].copy_from_slice(&kept.right[..frames]);
                }
            }
        }
        made.into_inner()
    }
}

/// What a render keeps to make its blocks on more than one thread.
pub(crate) struct Spread {
    pub crew: Crew,
    pub rounds: Rounds,
}

impl Spread {
    /// What it takes to make the blocks of `graph` on up to `threads`
    /// threads; nothing if one thread is all it can use.
    pub fn new(graph: &Graph, threads: usize) -> Option<Self> {
        let rounds = Rounds::of(&graph.tracks);
        let threads = threads.min(rounds.width());
        if threads < 2 {
            return None;
        }
        // The thread that asks works too.
        let crew = Crew::new(threads - 1, DenormalGuard::new);
        if crew.len() == 0 {
            return None;
        }
        Some(Self { crew, rounds })
    }

    /// How many threads make a block, the one that asks among them.
    pub fn threads(&self) -> usize {
        self.crew.len() + 1
    }

    /// How long the thread that asks has waited for the others so far,
    /// its own share of a round done.
    pub fn waited(&self) -> std::time::Duration {
        self.crew.waited()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{Compiler, DeviceBank, Meters};
    use crate::scripted::{BASS, DRUMS, Loading, ScriptedFactory};
    use crate::sources::AudioSources;
    use trill_core::device::{CompositeFactory, Purpose};
    use trill_core::{ChainRef, TrackId, TrackKind};

    /// A graph of `project` as a render makes it, with what each device
    /// may be run beside written into it.
    fn graph(project: &Project, scripted: ScriptedFactory) -> Box<Graph> {
        let factory = CompositeFactory::new(vec![Box::new(trill_dsp::BuiltinFactory), Box::new(scripted)]);
        let (mut bank, mut meters) = (DeviceBank::default(), Meters::default());
        let mut compiler = Compiler {
            factory: &factory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &AudioSources::default(),
            stand_ins: None,
            sample_rate: 48_000.0,
            purpose: Purpose::Rendering,
        };
        let (mut graph, _) = compiler.compile(project, false);
        assert!(company(&mut graph, project, &factory, (false, false)).is_empty());
        graph
    }

    fn track(project: &mut Project, name: &str, instrument: &str) -> TrackId {
        let track = project.add_track(name);
        project.add_device(ChainRef::Track(track), instrument, instrument, None).unwrap();
        track
    }

    /// Three instruments, two of them in a group, a return fed by the
    /// group and by the third, and a compressor on the third that listens
    /// to the first.
    fn song(first: &str, second: &str, third: &str) -> Project {
        let mut project = Project::new("plan");
        let room = project.add_track_of_kind("Room", TrackKind::Return);
        let band = project.add_track_of_kind("Band", TrackKind::Group);
        let (one, two, three) = (
            track(&mut project, "One", first),
            track(&mut project, "Two", second),
            track(&mut project, "Three", third),
        );
        project.set_track_group(one, Some(band)).unwrap();
        project.set_track_group(two, Some(band)).unwrap();
        project.set_send(band, room, Some(-6.0)).unwrap();
        project.set_send(three, room, Some(-6.0)).unwrap();
        let duck = project.add_device(ChainRef::Track(three), "builtin:compressor", "Duck", None).unwrap();
        project.set_sidechain(duck, Some(one)).unwrap();
        project
    }

    /// What must hold of any plan: every track is made once and added
    /// once, added in the order of the graph, and made only after
    /// everything that feeds it was added.
    fn holds(graph: &Graph, steps: &[Step]) {
        let tracks = &graph.tracks;
        let (mut made, mut added) = (vec![false; tracks.len()], Vec::new());
        for step in steps {
            match step {
                Step::Add(at) => {
                    assert!(made[*at], "added before it was made");
                    added.push(*at);
                }
                Step::Make(lists) => {
                    for at in lists.iter().flatten() {
                        assert!(!std::mem::replace(&mut made[*at], true), "made twice");
                        let track = &tracks[*at];
                        let listens: Vec<usize> = track.chain.devices.iter().filter_map(|slot| slot.key).collect();
                        for (other, feeder) in tracks.iter().enumerate() {
                            let into_bus = track.input_bus.is_some_and(|bus| {
                                feeder.output_bus == Some(bus) || feeder.sends.iter().any(|send| send.bus == bus)
                            });
                            if into_bus || feeder.key_out.is_some_and(|key| listens.contains(&key)) {
                                assert!(added.contains(&other), "made before what feeds it was added");
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(added, (0..tracks.len()).collect::<Vec<_>>(), "every track is added, in the order of the graph");
    }

    fn place(graph: &Graph, project: &Project, name: &str) -> usize {
        let id = project.tracks.iter().find(|track| track.name == name).unwrap().id;
        graph.tracks.iter().position(|track| track.id == id).unwrap()
    }

    fn makes(steps: &[Step]) -> Vec<&Vec<Vec<usize>>> {
        steps.iter().filter_map(|step| if let Step::Make(lists) = step { Some(lists) } else { None }).collect()
    }

    #[test]
    fn tracks_that_wait_for_nothing_are_made_at_once_and_the_rest_as_soon_as_what_feeds_them_is_added() {
        let project = song("builtin:polysynth", "builtin:polysynth", "builtin:polysynth");
        let graph = graph(&project, ScriptedFactory::new());
        let steps = plan(&graph.tracks);
        holds(&graph, &steps);
        let at = |name: &str| place(&graph, &project, name);
        // The two that wait for nothing first, each on a list of its own;
        // the third listens to the first, and is made once that is added.
        assert_eq!(steps[0], Step::Make(vec![vec![at("One")], vec![at("Two")]]), "{steps:?}");
        let makes = makes(&steps);
        assert!(makes.iter().any(|lists| lists.contains(&vec![at("Three")])), "{steps:?}");
        assert!(makes.iter().any(|lists| lists.contains(&vec![at("Band")])), "{steps:?}");
        assert_eq!(makes.last().unwrap(), &&vec![vec![at("Room")]], "the return last: everything feeds it");
    }

    #[test]
    fn devices_that_keep_something_between_them_are_on_one_list_in_the_order_of_the_graph() {
        // The three instruments say that they are to be run in turn.
        let project = song(BASS, DRUMS, BASS);
        let sharing = ScriptedFactory::loading(Loading { shares: true, ..Loading::default() });
        let graph = graph(&project, sharing);
        let steps = plan(&graph.tracks);
        holds(&graph, &steps);
        let at = |name: &str| place(&graph, &project, name);
        assert_eq!(steps[0], Step::Make(vec![vec![at("One"), at("Two")]]), "one list, though neither waits: {steps:?}");
        // The third is made after the two ahead of it, whatever else is
        // made beside it.
        let order: Vec<usize> = makes(&steps).into_iter().flat_map(|lists| lists.concat()).collect();
        let position = |name: &str| order.iter().position(|made| *made == at(name)).unwrap();
        assert!(position("One") < position("Two") && position("Two") < position("Three"), "{steps:?}");
    }

    #[test]
    fn a_device_that_waits_holds_back_those_after_it_that_it_keeps_something_with() {
        // Two instruments that keep something between them. The first has
        // a compressor that listens to another track, so it waits for
        // that one; the second waits for nothing, and is not made ahead of
        // the first all the same.
        let mut project = Project::new("held");
        let key = track(&mut project, "Key", "builtin:polysynth");
        let listener = track(&mut project, "Listener", BASS);
        let duck = project.add_device(ChainRef::Track(listener), "builtin:compressor", "Duck", None).unwrap();
        project.set_sidechain(duck, Some(key)).unwrap();
        track(&mut project, "Free", DRUMS);
        let sharing = ScriptedFactory::loading(Loading { shares: true, ..Loading::default() });
        let graph = graph(&project, sharing);
        let steps = plan(&graph.tracks);
        holds(&graph, &steps);
        let at = |name: &str| place(&graph, &project, name);
        let expected = [
            Step::Make(vec![vec![at("Key")]]),
            Step::Add(at("Key")),
            Step::Make(vec![vec![at("Listener"), at("Free")]]),
            Step::Add(at("Listener")),
            Step::Add(at("Free")),
        ];
        assert_eq!(steps, expected);

        // Where they keep nothing between them, the one that waits for
        // nothing is made at once with the key.
        let graph = self::graph(&project, ScriptedFactory::new());
        let steps = plan(&graph.tracks);
        holds(&graph, &steps);
        assert_eq!(steps[0], Step::Make(vec![vec![at("Key")], vec![at("Free")]]), "{steps:?}");
    }

    #[test]
    fn one_thread_is_all_a_song_can_use_where_its_tracks_wait_for_each_other() {
        let mut project = Project::new("chain");
        let band = project.add_track_of_kind("Band", TrackKind::Group);
        let only = track(&mut project, "Only", "builtin:polysynth");
        project.set_track_group(only, Some(band)).unwrap();
        let graph = graph(&project, ScriptedFactory::new());
        holds(&graph, &plan(&graph.tracks));
        assert!(Spread::new(&graph, 16).is_none());
        // And however many threads it is given, a song uses no more than
        // it has tracks to make at once.
        let wide = song("builtin:polysynth", "builtin:polysynth", "builtin:polysynth");
        let graph = self::graph(&wide, ScriptedFactory::new());
        assert_eq!(Spread::new(&graph, 16).map(|spread| spread.threads()), Some(2));
        assert!(Spread::new(&graph, 1).is_none());
    }
}
