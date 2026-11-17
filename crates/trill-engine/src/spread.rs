//! A render on more than one thread.
//!
//! While a song plays, one thread makes every block, a track at a time.
//! A render has no speaker to keep up with, and most of what it costs is
//! what the tracks' devices cost: a song of seventeen plugins took twice
//! as long to render as to play. Tracks that do not feed one another can
//! be made at once, and here they are.
//!
//! **The same samples.** What one thread makes and what many make is the
//! same, sample for sample, wherever the devices themselves do the same
//! thing twice. Three things see to that:
//!
//! - Every track is made by the same steps on the same numbers
//!   ([`crate::processor`]); only who runs them differs, and every thread
//!   that does treats very small numbers as the audio thread does.
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
//!   turns. What comes after the last of them on a track (an amplifier
//!   behind a sampled guitar, which costs ten times what the guitar does)
//!   is run beside the others as soon as its track has had its turn.
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
//! ([`Beside::Nothing`]) leaves the render on one thread: [`company`]
//! names it, for the render to say so.

use crate::crew::Crew;
use crate::graph::{Bus, Graph, TrackNode};
use crate::processor::{Common, DenormalGuard, Room, answer, begin, fade, gather, mix, say_controllers, through};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use trill_core::device::{Beside, DeviceFactory};
use trill_core::{DeviceId, Project};

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
/// anything, whatever is known of it: for finding out what one does.
pub(crate) fn company(
    graph: &mut Graph,
    project: &Project,
    factory: &dyn DeviceFactory,
    trusting: bool,
) -> Vec<DeviceId> {
    let (mut names, mut alone): (Vec<String>, Vec<DeviceId>) = (Vec::new(), Vec::new());
    let chains = graph.tracks.iter_mut().map(|track| &mut track.chain).chain([&mut graph.master.chain]);
    for slot in chains.flat_map(|chain| chain.devices.iter_mut()) {
        slot.turn = None;
        let Ok(device) = project.device(slot.id) else { continue };
        if trusting || slot.processor.is_none() {
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
    let taking = |slot: &crate::graph::DeviceSlot| slot.turn.is_some() && slot.enabled && slot.processor.is_some();
    track.chain.devices.iter().rposition(taking).map_or(0, |last| last + 1)
}

/// What a render keeps to make its blocks on more than one thread.
pub(crate) struct Spread {
    crew: Crew,
    steps: Vec<Step>,
    /// For every track, by its place in the graph: see [`turn_of`].
    turns: Vec<usize>,
    /// Room for the block of every track, by its place in the graph.
    rooms: Vec<Room>,
    /// Where a track that is listened to leaves what it sounded like ahead
    /// of its fader, until it is put where the listeners look.
    keys: Vec<Option<Bus>>,
}

impl Spread {
    /// What it takes to make the blocks of `graph` on up to `threads`
    /// threads; nothing if one thread is all it can use.
    pub fn new(graph: &Graph, threads: usize) -> Option<Self> {
        let steps = plan(&graph.tracks);
        let turns: Vec<usize> = graph.tracks.iter().map(turn_of).collect();
        // The most there is to do at once: the lists of a round, or the
        // tracks of one that go on beside each other after their turns.
        let rest = |at: &usize| turns[*at] > 0 && turns[*at] < graph.tracks[*at].chain.devices.len();
        let widest = steps.iter().map(|step| match step {
            Step::Make(lists) => lists.len().max(lists.iter().flatten().filter(|at| rest(at)).count()),
            Step::Add(_) => 0,
        });
        let threads = threads.min(widest.max().unwrap_or(0));
        if threads < 2 {
            return None;
        }
        // The thread that asks works too.
        let crew = Crew::new(threads - 1, DenormalGuard::new);
        if crew.len() == 0 {
            return None;
        }
        Some(Self {
            crew,
            steps,
            turns,
            rooms: graph.tracks.iter().map(|_| Room::new()).collect(),
            keys: graph.tracks.iter().map(|track| track.key_out.map(|_| Bus::new())).collect(),
        })
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

    /// Makes one block of `tracks`, as one thread would have: the buses
    /// and the mix end up holding the same samples.
    pub fn block(
        &mut self,
        tracks: &mut [TrackNode],
        buses: &mut [Bus],
        keys: &mut [Bus],
        common: &Common,
        mix_left: &mut [f32],
        mix_right: &mut [f32],
    ) {
        let frames = common.frames();
        let Spread { crew, steps, turns, rooms, keys: kept } = self;
        // What is left of a track's block once its devices have all run.
        let finish = |track: &mut TrackNode, room: &mut Room, kept: &mut Option<Bus>| {
            answer(track, common, room);
            // What the devices that listen to this track hear of it: as
            // it leaves its own devices.
            if let Some(kept) = kept {
                kept.left[..frames].copy_from_slice(&room.left[..frames]);
                kept.right[..frames].copy_from_slice(&room.right[..frames]);
            }
            fade(track, common, room);
        };
        for step in steps.iter() {
            let lists = match step {
                Step::Add(at) => {
                    mix(&mut tracks[*at], common, &mut rooms[*at], buses, mix_left, mix_right);
                    continue;
                }
                Step::Make(lists) => lists,
            };
            {
                // Each list takes its tracks, with their rooms, and
                // whichever thread comes first takes the next list.
                let places = tracks.iter_mut().zip(rooms.iter_mut()).zip(kept.iter_mut()).zip(turns.iter());
                let mut places: Vec<_> =
                    places.map(|(((track, room), kept), turn)| Some((track, room, kept, *turn))).collect();
                let lists: Vec<_> = lists
                    .iter()
                    .map(|list| Mutex::new(list.iter().filter_map(|at| places[*at].take()).collect::<Vec<_>>()))
                    .collect();
                let next = AtomicUsize::new(0);
                let (buses, keys) = (&*buses, &*keys);
                let work = || {
                    while let Some(list) = lists.get(next.fetch_add(1, Ordering::Relaxed)) {
                        for (track, room, kept, turn) in list.lock().iter_mut() {
                            begin(track, common, buses, room);
                            let moved = say_controllers(track, common, room);
                            gather(track, common, room, moved);
                            // The devices that take turns with those of
                            // the list's other tracks, and with them what
                            // comes before them; all of them where none
                            // does, or nothing comes after.
                            let devices = track.chain.devices.len();
                            let turn = if *turn == 0 { devices } else { (*turn).min(devices) };
                            through(track, common, room, keys, 0..turn);
                            if turn == devices {
                                finish(track, room, kept);
                            }
                        }
                    }
                };
                crew.all(lists.len() - 1, &work);

                // What comes after a track's turn is nobody's but its own:
                // those go on beside each other.
                let after = |(track, _, _, turn): &(&mut TrackNode, &mut Room, &mut Option<Bus>, usize)| {
                    *turn > 0 && *turn < track.chain.devices.len()
                };
                let rest: Vec<_> = lists
                    .into_iter()
                    .flat_map(|list| list.into_inner())
                    .filter(after)
                    .map(|place| Mutex::new(Some(place)))
                    .collect();
                if !rest.is_empty() {
                    let next = AtomicUsize::new(0);
                    let work = || {
                        while let Some(place) = rest.get(next.fetch_add(1, Ordering::Relaxed)) {
                            let Some((track, room, kept, turn)) = place.lock().take() else { continue };
                            let devices = track.chain.devices.len();
                            through(track, common, room, keys, turn..devices);
                            finish(track, room, kept);
                        }
                    };
                    crew.all(rest.len() - 1, &work);
                }
            }
            // Nobody listened to these while they were made: whoever
            // does is made in a later round.
            for at in lists.iter().flatten() {
                let key = tracks[*at].key_out.and_then(|key| keys.get_mut(key));
                if let (Some(key), Some(kept)) = (key, &kept[*at]) {
                    key.left[..frames].copy_from_slice(&kept.left[..frames]);
                    key.right[..frames].copy_from_slice(&kept.right[..frames]);
                }
            }
        }
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
        assert!(company(&mut graph, project, &factory, false).is_empty());
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
