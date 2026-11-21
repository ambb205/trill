//! A song that other programs hold too: the session's part in it.
//!
//! The session holds its song as the properties it is derived from
//! (`trill_core::merge`), whether or not anyone else has it. This file is
//! what follows from that when someone does:
//!
//! * **A shared song** has an id of its own ([`Session::share`]) and is
//!   saved with what is held beside it, so that it can be closed, worked
//!   on alone and merged again. The program that changes one takes a
//!   number by lot, once an opening, and counts its ids from there.
//! * **What is changed here** is kept for whoever is there to hear it
//!   ([`Session::take_said`]): every change to the song, made by an edit,
//!   an undo or anything else that writes it. What is one's own listening
//!   (the loop, solo, which reference is listened to) is never among it.
//! * **What someone else changed** enters through [`Session::merge`]: it
//!   is held to the model's rules, taken into the properties, and the song
//!   is worked out again, compiled and swapped in like any edit. It moves
//!   the revision, it is entered in the journal under their name, and it
//!   is never a step of this session's own history.
//! * **A clash is a sentence, once** ([`Session::notices`]): where what
//!   arrived overrode something set here that its maker had not seen,
//!   where it removed something that was being worked in here, and where
//!   an edit made here was set aside because it and one of theirs cannot
//!   both hold. Each comes with the edit that puts one's own back, which
//!   is an ordinary edit.
//!
//! Who is there, and what they hold, is not the song's and not the
//! session's: whoever connects the programs keeps it (`trill-together`).

use super::history::Done;
use super::{Scope, Session, SessionError};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};
use trill_core::journal::words::quoted;
use trill_core::merge::{
    self, Aside, Change, ClipOp, DeviceOp, DeviceSet, Floors, Home, Kept, Key, MarkerOp, NoteOp, Op, Refused, Replica,
    Said, SongId, Stamp, Thing, TrackOp, TrackSet, Why,
};
use trill_core::project::AutomationTarget;
use trill_core::{DeviceId, Project};

/// How many of the changes made here are remembered for telling whose
/// work was in something that someone removed.
const RECENT: usize = 4096;
/// How long a plugin's state has to stay as it is read back before it is
/// taken to have settled, and said to the others.
const SETTLED: Duration = Duration::from_millis(900);

/// What putting one's own back takes: an ordinary edit, as the properties
/// it sets.
#[derive(Clone, Debug, PartialEq)]
pub struct Remedy {
    /// What the action is called: "Put mine back".
    pub label: String,
    ops: Vec<Op>,
}

/// Something that happened to one's own work because someone else was
/// working in the song at the same moment: said once for the thing it is
/// about.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub id: u64,
    /// The sentence.
    pub text: String,
    /// What gets one's own back, where something does.
    pub remedy: Option<Remedy>,
    /// What it is about: one notice a property, or a thing.
    about: About,
}

#[derive(Clone, Debug, PartialEq)]
enum About {
    Key(Key),
    Thing(Thing),
}

impl Notice {
    /// A notice that is made up, for a picture or a test of what shows
    /// one: `action` is what its button is called, and does nothing.
    #[doc(hidden)]
    pub fn made_up(id: u64, text: impl Into<String>, action: Option<&str>) -> Self {
        let remedy = action.map(|label| Remedy { label: label.to_owned(), ops: Vec::new() });
        Notice { id, text: text.into(), remedy, about: About::Thing(Thing::Marker(0)) }
    }
}

/// What taking a step back or bringing it back came to.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Turned {
    /// Whether there was a step to take back or bring back.
    pub done: bool,
    /// How many of the things the step had set stay as someone else has
    /// set them since.
    pub theirs: usize,
}

impl Turned {
    /// What to say of it, where there is something to say.
    pub fn sentence(&self) -> Option<String> {
        match self.theirs {
            0 => None,
            1 => Some("One thing stays as someone else has set it since.".to_owned()),
            count => Some(format!("{count} things stay as others have set them since.")),
        }
    }
}

/// How often a plugin that is watched is asked for its state: asking
/// takes the plugin's own time.
const LOOK_EVERY: Duration = Duration::from_millis(1000);
/// How often a plugin whose window is closed is asked before its state is
/// taken never to settle: some write out something else every time.
const LOOKS_WITHOUT_SETTLING: u32 = 12;

/// A plugin whose state may have been changed in this program: watched
/// until it has settled.
struct Watched {
    /// What it last said its state is, and since when it has said so.
    last: Option<(Vec<u8>, Instant)>,
    /// When it is next asked.
    look_at: Instant,
    /// How often it was asked and had another state than the time before.
    unsettled: u32,
}

impl Watched {
    fn from_now() -> Self {
        Self { last: None, look_at: Instant::now() + LOOK_EVERY, unsettled: 0 }
    }
}

/// What the session keeps of the song being one that others hold.
pub(super) struct Sharing {
    /// Which shared song this is, if it is one.
    song: Option<SongId>,
    /// This opening's number, once it has changed the song.
    replica: Option<Replica>,
    /// Every number this opening has had.
    mine: Vec<Replica>,
    /// Someone is there to hear what is changed here.
    live: bool,
    said: Vec<Said>,
    notices: Vec<Notice>,
    last_notice: u64,
    /// The number each thing's notice goes by.
    numbers: Vec<(About, u64)>,
    /// The properties set here lately, for telling whose work was in
    /// something that was removed.
    recent: VecDeque<(Stamp, Key)>,
    /// What is changed here while a take is recorded, kept until it has
    /// landed: nothing half-made is said to anyone.
    held: Vec<Said>,
    holding: bool,
    /// What the song as it stands sets aside.
    pub aside: Vec<Aside>,
    /// How many takes lie in another lane than the one they were put in
    /// ([`merge::Derived::displaced`]).
    pub displaced: usize,
    /// The edits made here that were set aside and have been said.
    told: HashSet<Key>,
    watched: HashMap<DeviceId, Watched>,
    /// How many songs have been open: another song is another story.
    epoch: u64,
}

impl Sharing {
    pub fn new() -> Self {
        Self {
            song: None,
            replica: None,
            mine: Vec::new(),
            live: false,
            said: Vec::new(),
            notices: Vec::new(),
            last_notice: 0,
            numbers: Vec::new(),
            recent: VecDeque::new(),
            held: Vec::new(),
            holding: false,
            aside: Vec::new(),
            displaced: 0,
            told: HashSet::new(),
            watched: HashMap::new(),
            epoch: 0,
        }
    }
}

/// A number by lot: from what the system seeds its hashes with, which is
/// different for every run and every thread of one.
fn lot() -> u64 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos()),
    );
    hasher.finish()
}

/// What a thing is called in a sentence.
fn named(doc: &merge::Doc, thing: &Thing) -> String {
    let name = |thing: &Thing| doc.name_of(thing).map(|name| quoted(&name));
    match thing {
        Thing::Track(_) => name(thing).map_or_else(|| "a track".to_owned(), |name| format!("the track {name}")),
        Thing::Clip(_) => name(thing).map_or_else(|| "a clip".to_owned(), |name| format!("the clip {name}")),
        Thing::Device(_) => name(thing).unwrap_or_else(|| "a device".to_owned()),
        Thing::Marker(_) => name(thing).map_or_else(|| "a marker".to_owned(), |name| format!("the marker {name}")),
        Thing::Note(clip, _) => format!("a note in {}", named(doc, &Thing::Clip(*clip))),
        Thing::Point(Home::Curve(clip, ..), _) => format!("a curve in {}", named(doc, &Thing::Clip(*clip))),
        Thing::Point(Home::Lane(target), _) => envelope(doc, target),
    }
}

fn envelope(doc: &merge::Doc, target: &AutomationTarget) -> String {
    match target {
        AutomationTarget::TrackVolume { track } => {
            format!("the volume envelope of {}", named(doc, &Thing::Track(*track)))
        }
        AutomationTarget::TrackPan { track } => format!("the pan envelope of {}", named(doc, &Thing::Track(*track))),
        AutomationTarget::DeviceParam { device, param } => {
            format!("the envelope of {param} on {}", named(doc, &Thing::Device(*device)))
        }
        AutomationTarget::MasterVolume => "the master's volume envelope".to_owned(),
        AutomationTarget::Send { track, to } => {
            format!(
                "the envelope of the send from {} to {}",
                named(doc, &Thing::Track(*track)),
                named(doc, &Thing::Track(*to))
            )
        }
    }
}

/// What a property is called in a sentence: "the volume of the track
/// “Bass”", "where the clip “Verse” starts".
fn what(doc: &merge::Doc, key: &Key, op: &Op) -> String {
    let of = |thing: Thing| named(doc, &thing);
    match op {
        Op::Song(set) => match set {
            merge::SongSet::Name(_) => "the song's name".to_owned(),
            merge::SongSet::Tempo(_) => "the tempo".to_owned(),
            merge::SongSet::TimeSignature(_) => "the time signature".to_owned(),
            merge::SongSet::Key(_) => "the song's key".to_owned(),
            merge::SongSet::Loop(_) => "the loop".to_owned(),
        },
        Op::MasterVolume(_) => "the master's volume".to_owned(),
        Op::Tempo(beat, ..) => format!("the tempo at beat {beat}"),
        Op::Meter(beat, _) => format!("the time signature at beat {beat}"),
        Op::Marker(id, MarkerOp::Set(merge::MarkerSet::Beat(_))) => format!("where {} is", of(Thing::Marker(*id))),
        Op::Marker(id, _) => of(Thing::Marker(*id)),
        Op::Track(id, part) => {
            let track = of(Thing::Track(*id));
            match part {
                TrackOp::Set(TrackSet::Name(_)) => format!("the name of {track}"),
                TrackOp::Set(TrackSet::Color(_)) => format!("the colour of {track}"),
                TrackOp::Set(TrackSet::Volume(_)) => format!("the volume of {track}"),
                TrackOp::Set(TrackSet::Pan(_)) => format!("the pan of {track}"),
                TrackOp::Set(TrackSet::Mute(_)) => format!("whether {track} is muted"),
                TrackOp::Set(TrackSet::Group(_)) => format!("the group {track} plays through"),
                TrackOp::Set(TrackSet::Profile(_)) => format!("what the notes of {track} mean"),
                TrackOp::Place(_) => format!("where {track} is among the tracks"),
                TrackOp::Reference(_) => format!("whether {track} is a reference"),
                _ => track,
            }
        }
        Op::Send(track, to, _) => format!("the send from {} to {}", of(Thing::Track(*track)), of(Thing::Track(*to))),
        Op::Clip(id, part) => {
            let clip = of(Thing::Clip(*id));
            match part {
                ClipOp::Set(merge::ClipSet::Start(_)) => format!("where {clip} starts"),
                ClipOp::Set(merge::ClipSet::Length(_)) => format!("how long {clip} is"),
                ClipOp::Set(merge::ClipSet::Name(_)) => format!("the name of {clip}"),
                ClipOp::Set(merge::ClipSet::Muted(_)) => format!("whether {clip} is muted"),
                ClipOp::Track(_) => format!("the track {clip} is on"),
                ClipOp::Lane(_) => format!("the lane {clip} is kept in"),
                _ => clip,
            }
        }
        Op::Note(clip, id, part) => match part {
            NoteOp::Set(merge::NoteSet::Velocity(_)) => format!("the velocity of {}", of(Thing::Note(*clip, *id))),
            NoteOp::Set(merge::NoteSet::Length(_)) => format!("the length of {}", of(Thing::Note(*clip, *id))),
            _ => of(Thing::Note(*clip, *id)),
        },
        Op::Device(id, part) => {
            let device = of(Thing::Device(*id));
            match part {
                DeviceOp::Set(DeviceSet::State(_)) => format!("the settings of {device}"),
                DeviceOp::Set(DeviceSet::Enabled(_)) => format!("whether {device} is on"),
                DeviceOp::Set(DeviceSet::Sidechain(_)) => format!("what {device} listens to"),
                DeviceOp::Set(DeviceSet::Preset(_) | DeviceSet::FromPreset(_)) => format!("the preset of {device}"),
                DeviceOp::Set(DeviceSet::Name(_)) => format!("the name of {device}"),
                DeviceOp::At(..) => format!("where {device} is in its chain"),
                _ => device,
            }
        }
        Op::Param(device, name, _) => format!("{name} on {}", of(Thing::Device(*device))),
        Op::Lane(target, _) => envelope(doc, target),
        Op::Envelopes(_) => "the order of the envelopes".to_owned(),
        Op::Point(home, id, _) => of(Thing::Point(home.clone(), *id)),
        Op::Notes(..) | Op::Points(..) => key.thing().map_or_else(|| "something".to_owned(), of),
    }
}

/// The change that takes away what another change set, for the kinds of
/// thing one of which can stand in another's way.
fn cleared(key: &Key) -> Option<Op> {
    match key {
        Key::Device(id, prop) if *prop == DeviceSet::Sidechain(None).prop() => {
            Some(Op::Device(*id, DeviceOp::Set(DeviceSet::Sidechain(None))))
        }
        Key::Track(id, prop) if *prop == TrackSet::Group(None).prop() => {
            Some(Op::Track(*id, TrackOp::Set(TrackSet::Group(None))))
        }
        Key::Track(id, merge::REFERENCE) => Some(Op::Track(*id, TrackOp::Reference(None))),
        Key::Send(track, to) => Some(Op::Send(*track, *to, None)),
        _ => None,
    }
}

impl Session {
    // ---- a shared song -----------------------------------------------------

    /// Which shared song the open song is, if it is one.
    pub fn shared_song(&self) -> Option<SongId> {
        self.sharing.song
    }

    /// Makes the open song one that others can hold too, if it is not one
    /// already, and says which it is. From here on it is saved with what
    /// is held beside it, and what is made in it is given ids that no
    /// other program gives.
    pub fn share(&mut self) -> SongId {
        if let Some(song) = self.sharing.song {
            return song;
        }
        // While the song is still one's own every plugin's state is news
        // to it: read them all now, so that whoever is handed the song has
        // each plugin as it is running here. While plugins are still being
        // made the question would wait for the making in hand with the
        // session held: each is looked at once that is over instead, and
        // its state said if it is not what the song has.
        if self.making_devices() {
            let plugins = self.project.devices().filter(|device| self.factory.takes_time(&device.uri));
            let plugins: Vec<DeviceId> = plugins.map(|device| device.id).collect();
            for device in plugins {
                self.watch(device);
            }
        } else {
            self.absorb_editor_edits();
            self.capture_device_state();
        }
        let song = SongId(u128::from(lot()) << 64 | u128::from(lot()));
        self.sharing.song = Some(song);
        // What the song says of itself has changed: a save writes more.
        self.revision += 1;
        song
    }

    /// This opening's number among the programs that hold the song, once
    /// it has changed it.
    pub fn replica(&self) -> Option<Replica> {
        self.sharing.replica
    }

    /// How many songs this session has had open: goes up when another
    /// takes the place of the one that was. Whoever connects the song to
    /// others looks at it to know that the song it connected is gone.
    pub fn song_epoch(&self) -> u64 {
        self.sharing.epoch
    }

    /// Says whether someone is there to hear what is changed here. While
    /// someone is, every change is kept for [`Session::take_said`].
    pub fn hear_changes(&mut self, live: bool) {
        self.sharing.live = live;
        if !live {
            self.sharing.said.clear();
        }
    }

    /// What was changed here since this was last asked, for the others.
    /// What a take that is still being recorded has written so far is not
    /// among it: that is said when the take has landed.
    pub fn take_said(&mut self) -> Vec<Said> {
        if self.sharing.holding && !self.take_is_open() {
            // (No take is being recorded any more, whatever became of it.)
            self.say_what_was_held();
        }
        std::mem::take(&mut self.sharing.said)
    }

    /// From here until [`Session::say_what_was_held`], what is changed
    /// here is kept and not said: a take is being recorded, and what it
    /// has written so far is nobody else's yet.
    pub(super) fn hold_what_is_said(&mut self) {
        self.sharing.holding = true;
    }

    /// The take has landed, or was let go of: what was changed meanwhile
    /// is said, all of it at once.
    pub(super) fn say_what_was_held(&mut self) {
        self.sharing.holding = false;
        let held = std::mem::take(&mut self.sharing.held);
        self.sharing.said.extend(held);
    }

    /// How much of what each program did is held here.
    pub fn floors(&self) -> Floors {
        self.doc.floors().clone()
    }

    /// What a program that holds what `floors` says lacks of what is held
    /// here.
    pub fn since(&self, floors: &Floors) -> Vec<Said> {
        let knew = Some(self.doc.floors().clone());
        self.doc.since(floors).into_iter().map(|change| Said { change, knew: knew.clone() }).collect()
    }

    /// The song with everything that is held beside it, as the text of a
    /// project file: what a program that has not got the song is handed.
    /// What is one's own listening is not in it.
    pub fn whole_song(&mut self) -> String {
        let id = self.share();
        self.absorb_editor_edits();
        self.capture_device_state();
        let kept = self.doc.kept(id, &self.project).impersonal();
        let mut song = self.project.clone();
        song.loop_region.enabled = false;
        for track in &mut song.tracks {
            (track.solo, track.input) = (false, None);
            if let Some(reference) = &mut track.reference {
                reference.listening = false;
            }
        }
        trill_core::files::write_shared(&song, &self.files, Some(&kept))
    }

    /// Opens a shared song that another program handed over
    /// ([`Session::whole_song`]) in place of the one that is open. It has
    /// no file yet, and counts as changed until it is saved.
    pub fn take_song(&mut self, text: &str) -> Result<(), SessionError> {
        let Some(kept) = trill_core::files::read_shared(text)? else {
            return Err(SessionError::Io("what was handed over is not a shared song".into()));
        };
        let (project, files) = self.read_project(text, None)?;
        let project = self.with_known_instruments(project);
        self.replace_song(project, Some(kept));
        self.files = files;
        self.path = None;
        self.revision += 1;
        Ok(())
    }

    /// What a project file of the open song keeps beside the song, if the
    /// song is a shared one.
    pub(super) fn kept(&self) -> Option<Kept> {
        self.sharing.song.map(|id| self.doc.kept(id, &self.project))
    }

    /// Another song is open: what was kept of the one before is gone.
    pub(super) fn begin_sharing_of(&mut self, song: Option<SongId>) {
        let (live, epoch) = (self.sharing.live, self.sharing.epoch + 1);
        self.sharing = Sharing::new();
        self.sharing.song = song;
        self.sharing.epoch = epoch;
        // (Whoever was listening hears of the other song by its epoch.)
        self.sharing.live = live;
    }

    // ---- writing the song --------------------------------------------------

    /// The stamp of the next change made here.
    pub(super) fn next_stamp(&mut self) -> Stamp {
        let replica = match self.sharing.song {
            Some(_) => self.number(),
            // A song nobody shares has one writer.
            None => {
                if !self.sharing.mine.contains(&0) {
                    self.sharing.mine.push(0);
                }
                0
            }
        };
        Stamp(self.doc.clock() + 1, replica)
    }

    /// The numbers this program has written the song under.
    pub(super) fn mine(&self) -> &[Replica] {
        &self.sharing.mine
    }

    /// This opening's number: drawn by lot the first time it is asked.
    fn number(&mut self) -> Replica {
        if let Some(replica) = self.sharing.replica {
            return replica;
        }
        let span = merge::REPLICA_LIMIT - merge::REPLICA_FLOOR;
        let replica = merge::REPLICA_FLOOR + lot() % span;
        self.sharing.replica = Some(replica);
        self.sharing.mine.push(replica);
        replica
    }

    /// Before an edit of a shared song: what it makes is given ids that
    /// are this program's own.
    pub(super) fn ready_ids(&mut self) {
        if self.sharing.song.is_none() {
            return;
        }
        let replica = self.number();
        let next = self.project.next_id();
        if next >> merge::ID_BITS != replica || !merge::ids_left(next) {
            if next >> merge::ID_BITS == replica {
                // Every id of this number is given: another number.
                self.sharing.replica = None;
            }
            let replica = self.number();
            self.project.count_ids_from(merge::first_id(replica));
        }
    }

    /// Takes a change made here into the properties: the one way the song
    /// is written. Returns what it did, for the history.
    pub(super) fn write(&mut self, ops: Vec<Op>) -> merge::Taken {
        let stamp = self.next_stamp();
        let change = Change { stamp, ops };
        let taken = self.doc.apply(&change);
        self.doc.floors_mut().raise(stamp.replica(), stamp.clock());
        for part in &taken.won {
            if let Some(key) = part.op.key() {
                // What is set here now is no longer what a notice was about.
                self.sharing.notices.retain(|notice| notice.about != About::Key(key.clone()));
                self.sharing.recent.push_back((stamp, key));
            } else {
                for thing in part.op.makes() {
                    self.sharing.recent.push_back((stamp, thing.alive()));
                }
            }
        }
        while self.sharing.recent.len() > RECENT {
            self.sharing.recent.pop_front();
        }
        // Said without what is one's own, which never leaves the program.
        if self.sharing.live
            && let Some(change) = change.impersonal()
        {
            let said = Said { change, knew: Some(self.doc.floors().clone()) };
            match self.sharing.holding {
                true => self.sharing.held.push(said),
                false => self.sharing.said.push(said),
            }
        }
        taken
    }

    /// Writes the song without making a step: what a plugin came to on its
    /// own. The steps that had set the same properties last follow, so
    /// that taking one back still takes it back.
    pub(super) fn write_quietly(&mut self, ops: Vec<Op>) {
        let taken = self.write(ops);
        for part in &taken.won {
            // The step that set the property last, on either side.
            let _ = self.undo.iter_mut().rev().any(|step| step.done.follow(part, false));
            let _ = self.redo.iter_mut().rev().any(|step| step.done.follow(part, true));
        }
    }

    /// Works the song out again from its properties, and returns the
    /// song as it was held until now: nobody who needs the song before
    /// has to copy it first.
    pub(super) fn rederive(&mut self) -> Project {
        let next = self.project.next_id();
        let derived = self.doc.derive();
        let before = std::mem::replace(&mut self.project, derived.project);
        self.project.count_ids_from(next);
        (self.sharing.aside, self.sharing.displaced) = (derived.aside, derived.displaced);
        self.tell_of_what_was_set_aside();
        before
    }

    /// An edit by its properties: what a remedy and "bring it back" are
    /// made with. Returns the song before and what the edit set, for the
    /// step it is.
    pub(super) fn edit_by(&mut self, ops: Vec<Op>) -> Option<(Project, Done)> {
        if ops.is_empty() {
            return None;
        }
        let taken = self.write(ops);
        if taken.won.is_empty() {
            return None;
        }
        let before = self.rederive();
        self.sync(&before, Scope::Everything);
        Some((before, Done::of(&taken.won)))
    }

    /// Brings back something that was removed, with whatever was done in
    /// it meanwhile: one undo step.
    pub fn bring_back(&mut self, thing: &Thing) -> Result<(), SessionError> {
        if !self.doc.knows(thing) {
            return Err(SessionError::Io("there is nothing held of that to bring back".into()));
        }
        self.act(vec![thing.set_alive(true)]);
        Ok(())
    }

    /// Makes an edit that is given as the properties it sets, as a step.
    fn act(&mut self, ops: Vec<Op>) {
        if let Some((before, done)) = self.edit_by(ops) {
            self.finish_step(before, done);
        }
    }

    // ---- what someone else changed -----------------------------------------

    /// Takes in what another program changed. `by` is the name its person
    /// goes by. `floors` is how much the program that said it holds of
    /// everyone's, where it said; `from` is that program's own number,
    /// where it says its own changes itself and in order.
    ///
    /// Everything is held to the model's rules first, and nothing of a
    /// batch is taken if any of it is refused: the error says what, and
    /// whoever connects the programs lets go of one that says such
    /// things. Returns what was news here, for passing on to whoever else
    /// is connected.
    pub fn merge(
        &mut self,
        by: &str,
        said: Vec<Said>,
        floors: Option<&Floors>,
        from: Option<Replica>,
    ) -> Result<Vec<Said>, Refused> {
        let clock = self.doc.clock();
        let mut checked = Vec::with_capacity(said.len());
        for Said { change, knew } in said {
            checked.push(Said { change: change.checked(clock)?, knew });
        }
        let mine = self.sharing.mine.clone();
        let mut news = Vec::new();
        let mut only_params = true;
        for Said { change, knew } in checked {
            let stamp = change.stamp;
            // Whether its maker had seen a change made here.
            let seen = |held: Stamp| knew.as_ref().or(floors).is_some_and(|knew| knew.has(held));
            // What of this program's it may have its way over: looked at
            // before it is taken in.
            let mut over = Vec::new();
            for op in &change.ops {
                let Some(key) = op.key() else { continue };
                let held = self.doc.stamp(&key);
                if held < stamp && mine.contains(&held.replica()) && held != Stamp::ZERO && !seen(held) {
                    over.push((key.clone(), self.doc.read(&key)));
                }
            }
            let taken = self.doc.apply(&change);
            if let Some(replica) = from.filter(|replica| *replica == stamp.replica()) {
                self.doc.floors_mut().raise(replica, stamp.clock());
            }
            if !taken.is_news() {
                continue;
            }
            only_params &= taken.won.iter().all(|part| part.op.is_param());
            for part in taken.won.iter().filter(|part| part.stamp == stamp) {
                if let Some(key) = part.op.key() {
                    if let Some((_, Some(was))) = over.iter().find(|(held, _)| *held == key)
                        && *was != part.op
                    {
                        self.say_overridden(by, &key, was.clone(), &part.op);
                    }
                    if let Some(thing) = key.thing().filter(|thing| part.op == thing.set_alive(false)) {
                        self.say_removed(by, &thing);
                    }
                }
            }
            let own = taken.won.into_iter().filter(|part| part.stamp == stamp).map(|part| part.op);
            let ops: Vec<Op> = own.chain(taken.waiting).collect();
            if !ops.is_empty() {
                news.push(Said { change: Change { stamp, ops }, knew });
            }
        }
        if let Some(floors) = floors {
            self.doc.floors_mut().take(floors);
        }
        if news.is_empty() {
            return Ok(news);
        }
        let before = self.rederive();
        if self.project == before {
            // Nothing of it is in the song as it stands (it is held in
            // something that was removed): there is nothing to play anew.
            return Ok(news);
        }
        self.sync(&before, if only_params { Scope::ParamsOnly } else { Scope::Everything });
        self.revision += 1;
        self.record_of_another(by, &before);
        Ok(news)
    }

    /// What this program's person has hold of right now, in words: the
    /// first thing the drag that is under way set. For the others to see;
    /// no part of the song.
    pub fn holding(&self) -> Option<String> {
        if self.gesture != Some(true) {
            return None;
        }
        let record = self.undo.last()?.done.records.first()?;
        Some(what(&self.doc, &record.key, &record.after))
    }

    // ---- notices -----------------------------------------------------------

    /// What happened to one's own work because someone else was working
    /// at the same moment, each said once: see the [module](self).
    pub fn notices(&self) -> &[Notice] {
        &self.sharing.notices
    }

    /// A notice has been read.
    pub fn dismiss_notice(&mut self, id: u64) {
        self.sharing.notices.retain(|notice| notice.id != id);
    }

    /// Does what a notice offers: the edit that puts one's own back, as
    /// one undo step. The notice goes.
    pub fn remedy(&mut self, id: u64) -> Result<(), SessionError> {
        let Some(at) = self.sharing.notices.iter().position(|notice| notice.id == id) else {
            return Err(SessionError::Io("that has been dealt with already".into()));
        };
        let notice = self.sharing.notices.remove(at);
        if let Some(remedy) = notice.remedy {
            self.act(remedy.ops);
        }
        Ok(())
    }

    fn notice(&mut self, about: About, text: String, remedy: Option<Remedy>) {
        // Once for a thing: what is said of it again takes the place of
        // what was said.
        if let Some(held) = self.sharing.notices.iter_mut().find(|notice| notice.about == about) {
            (held.text, held.remedy) = (text, remedy);
            return;
        }
        // What is said of a thing a second time, after the first was
        // dealt with, goes by the number the first went by: whoever shows
        // it sees the same notice come back, not another.
        let id = match self.sharing.numbers.iter().find(|(known, _)| *known == about) {
            Some((_, id)) => *id,
            None => {
                self.sharing.last_notice += 1;
                self.sharing.numbers.push((about.clone(), self.sharing.last_notice));
                self.sharing.last_notice
            }
        };
        self.sharing.notices.push(Notice { id, text, remedy, about });
    }

    /// Someone set a property that had been set here, without having
    /// seen that.
    fn say_overridden(&mut self, by: &str, key: &Key, mine: Op, theirs: &Op) {
        let about = what(&self.doc, key, theirs);
        let text = format!("{by} set {about} at the same moment as you, and theirs is what the song has.");
        let remedy = Remedy { label: "Put mine back".to_owned(), ops: vec![mine] };
        self.notice(About::Key(key.clone()), text, Some(remedy));
    }

    /// Someone removed a thing: said if work done here is in it, whether
    /// or not they had seen it. (Taking back the step that made a thing
    /// removes it with everyone's work in it.)
    fn say_removed(&mut self, by: &str, thing: &Thing) {
        let doc = &self.doc;
        let own = |(stamp, key): &(Stamp, Key)| doc.stamp(key) == *stamp && doc.is_within(key, thing);
        if !self.sharing.recent.iter().any(own) {
            return;
        }
        let name = named(doc, thing);
        let text = format!("{by} removed {name}, which you had worked in. It is held, with your work in it.");
        let remedy = Remedy { label: "Bring it back".to_owned(), ops: vec![thing.set_alive(true)] };
        self.notice(About::Thing(thing.clone()), text, Some(remedy));
    }

    /// An edit made here that the song as it stands sets aside is said,
    /// once.
    fn tell_of_what_was_set_aside(&mut self) {
        let aside = self.sharing.aside.clone();
        self.sharing.told.retain(|key| aside.iter().any(|held| held.key == *key));
        for found in aside {
            if !self.sharing.mine.contains(&found.stamp.replica()) || !self.sharing.told.insert(found.key.clone()) {
                continue;
            }
            let Some(mine) = self.doc.read(&found.key) else { continue };
            let about = what(&self.doc, &found.key, &mine);
            let theirs = found.by.as_ref().and_then(|by| self.doc.read(by).map(|op| what(&self.doc, by, &op)));
            let reason = match (found.why, &theirs) {
                (Why::Loop, Some(theirs)) => format!(
                    "Someone set {theirs} at the same moment as you set {about}, and with both the two tracks would \
                     each wait for the other. Theirs was first: yours waits until theirs is gone."
                ),
                (Why::Loop, None) => {
                    format!("What you set for {about} would have two tracks each wait for the other: it waits.")
                }
                (Why::Reference, _) => format!(
                    "What you set for {about} cannot hold with what someone set at the same moment: a reference \
                     is beside the song and feeds nothing in it. Theirs was first, and yours waits."
                ),
            };
            let ops = found.by.as_ref().and_then(cleared).map(|clear| vec![clear, mine]);
            let remedy = ops.map(|ops| Remedy { label: "Have mine instead".to_owned(), ops });
            self.notice(About::Key(found.key), reason, remedy);
        }
    }

    // ---- plugins that were changed in their own windows ---------------------

    /// A plugin may have been changed in this program: it is watched until
    /// its state has settled, which is then said to the others.
    pub(super) fn watch(&mut self, device: DeviceId) {
        self.sharing.watched.entry(device).or_insert_with(Watched::from_now);
    }

    /// Whether what a plugin says its state is, is news to the song: for
    /// a song nobody else holds, always (it is what a save writes). For a
    /// shared one only where the plugin was changed in this program: what
    /// a plugin reads back is not always the bytes it was given, and a
    /// state that was said every time it read back differently would have
    /// two programs make each other's plugins again for ever.
    pub(super) fn state_is_news(&self, device: DeviceId) -> bool {
        self.sharing.song.is_none() || self.sharing.watched.contains_key(&device)
    }

    /// The state a watched plugin came to has been written to the song.
    pub(super) fn state_was_said(&mut self, device: DeviceId) {
        self.sharing.watched.remove(&device);
    }

    /// Looks at the plugins that were changed here: one whose state has
    /// stayed as it is for a moment has settled, and its state is written
    /// to the song, whole. One of the session's chores, and only while
    /// someone is there to hear of it.
    pub(super) fn tend_watched(&mut self) {
        if !self.sharing.live || self.sharing.watched.is_empty() || self.making_devices() {
            return;
        }
        let now = Instant::now();
        let due: Vec<DeviceId> =
            self.sharing.watched.iter().filter(|(_, watched)| watched.look_at <= now).map(|(id, _)| *id).collect();
        for id in due {
            let Ok(held) = self.project.device(id).map(|device| device.state.clone()) else {
                self.sharing.watched.remove(&id);
                continue;
            };
            let open = self.is_device_editor_open(id);
            let live = self.news_of_state(id, held.as_ref());
            let entry = self.sharing.watched.get_mut(&id).expect("it is watched");
            entry.look_at = now + LOOK_EVERY;
            let Some(live) = live else { continue };
            match &entry.last {
                Some((last, since)) if *last == live => {
                    if now.duration_since(*since) >= SETTLED {
                        // Settled: said, whole, if it is not what the song
                        // has. A window that is still open may still be
                        // worked in, and is watched on.
                        self.capture_state_of(id);
                        match open {
                            true => drop(
                                self.sharing
                                    .watched
                                    .insert(id, Watched { last: Some((live, now)), ..Watched::from_now() }),
                            ),
                            false => drop(self.sharing.watched.remove(&id)),
                        }
                    }
                }
                _ => {
                    entry.last = Some((live, now));
                    entry.unsettled += 1;
                    if !open && entry.unsettled > LOOKS_WITHOUT_SETTLING {
                        // It writes out something else every time it is
                        // asked: nothing of it is ever news.
                        self.sharing.watched.remove(&id);
                    }
                }
            }
        }
    }
}
