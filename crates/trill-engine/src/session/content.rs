//! The files of a song that other programs hold too: known by what they
//! are, each machine with its own place for them.
//!
//! In a song of one's own a clip or a sample player names a file by its
//! path, and that is all. A path means one place on one machine, so in a
//! song others hold a file is known by what it *is*
//! (`trill_core::files::FileIs`: the hash of its bytes, how many they
//! are, its name), which is the song's, and where it lies is each
//! machine's own.
//!
//! **No path is said, and none is taken.** A clip's path is a property
//! that is one's own (`merge::Key::is_personal`); the state of a built-in
//! device that names files is said without the places
//! ([`Session::forget_device_places`]). And whatever arrives all the same
//! is not taken: a change that sets a clip's path is refused, a clip that
//! arrives whole arrives without one (`merge::Op::checked`), and after
//! anything arrived every file a device's state names is held to being a
//! place this machine made or its own user named
//! ([`Session::give_places`]). So nothing downstream can open, read, write
//! or hand out a path someone else chose: whatever reads `audio.path` or a
//! device's state reads a place of this machine's.
//!
//! **What arrives in a song is kept apart for that song**: in a folder of
//! the program's own, by the song's id ([`Session::received_in`]), under a
//! name made here. A program hands out, in a song, only what that song
//! brought it and what its own user's files in that song are.
//!
//! **A file of this machine's goes only by its user's hand.** A song file
//! is a stranger too: what it names on this disk is not read to be said,
//! and is not handed out, until the user who was shown it lets it go
//! ([`Session::let_files_go`]: sharing the song is that, and so is the
//! click on the notice of what is held). A file the hand itself brings
//! into a shared song goes without a question, and so does what is
//! recorded in it. Everything else that comes to name a file here (a song
//! that was opened, an assistant's edit) is held ([`Session::files_held`]).
//!
//! What this file does:
//!
//! * What arrives is given this machine's place for what it is
//!   ([`Session::give_places`]): where this machine has those bytes
//!   already, or in the song's folder, where they will lie once they have
//!   arrived. Fetching them is not the session's business
//!   (`trill-runtime`).
//! * The program that has a file says what it is, once it has read it
//!   ([`Session::files_here`], [`Session::say_file`]): a property of the
//!   clip or of the device, written without a step, which travels like any
//!   other.
//! * [`Session::said_files`] is every file the song says it plays, with
//!   where this machine has it or will: what whoever fetches and hands out
//!   goes by.
//! * [`Session::files_are_here`]: files arrived, and the song plays them
//!   from here on without being opened again.

use super::Session;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use trill_core::files::{Content, FileIs, FilePlace, kind_of};
use trill_core::merge::{AudioSet, Blob, ClipOp, DeviceOp, DeviceSet, Kept, Op, SongId};
use trill_core::project::ClipContent;
use trill_core::{ClipId, Device, DeviceFiles, DeviceId, Project};
use trill_dsp::SampleFiles;

/// The built-in devices whose files travel with a shared song: a file
/// each plays is one recording. (A sampled instrument's file names
/// hundreds; those stay where they are.)
const SAMPLE_PLAYERS: [&str; 2] = ["builtin:sampler", "builtin:drumrack"];

/// A file the open song names, as this machine has it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileHere {
    /// The clip or take, or the slot of a device, that names it.
    pub place: FilePlace,
    /// Where this machine has the file.
    pub path: String,
    /// What the song says it is, if it says.
    pub is: Option<FileIs>,
    /// Whether it may be read to be said and handed out: it is what the
    /// song brought here, or its user's hand let it go.
    pub may_go: bool,
}

/// The folder, in a song's folder of files from other programs, of the
/// places made for devices' files of which nobody said what they are.
const OF_DEVICES: &str = "of devices";

/// The folder files from other programs are named in where nobody has
/// said which: nothing is ever written there by the session.
fn nowhere_yet() -> PathBuf {
    std::env::temp_dir().join("trill-received")
}

/// The name a slot of a device has in a name made here: its letters and
/// digits.
fn slot_name(slot: &str) -> String {
    slot.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(64).collect()
}

/// The files `device` names, slot by slot, where it is a built-in device
/// that names any.
fn named_by(device: &Device) -> Vec<(String, String)> {
    SampleFiles.files(device)
}

/// Every path the clips, takes and devices of `song` name.
fn named_in(song: &Project) -> Vec<String> {
    let mut named = Vec::new();
    for track in &song.tracks {
        let clips = track.clips.iter().chain(track.takes.iter().map(|take| &take.clip));
        named.extend(clips.filter_map(|clip| clip.audio()).map(|audio| audio.path.clone()));
    }
    named.extend(song.devices().flat_map(named_by).map(|(_, path)| path));
    named.retain(|path| !path.is_empty());
    named
}

impl Session {
    /// Says where the files that come from other programs are kept on
    /// this machine: a folder of the program's own, in which each shared
    /// song has one of its own.
    pub fn keep_received_in(&mut self, dir: Option<PathBuf>) {
        self.received = dir;
    }

    /// The folder what arrives in the shared song `song` is kept in.
    fn folder_of(&self, song: SongId) -> PathBuf {
        self.received.clone().unwrap_or_else(nowhere_yet).join(format!("{:032x}", song.0))
    }

    /// The folder the open song's files from other programs are kept in:
    /// `None` for a song that is nobody else's.
    pub fn received_in(&self) -> Option<PathBuf> {
        self.shared_song().map(|song| self.folder_of(song))
    }

    /// Where this machine has a file of the open song that is `is`, or
    /// will have it once it has arrived: in the song's folder, under its
    /// hash and its kind. `None` for a song that is nobody else's.
    pub fn home_of(&self, is: &FileIs) -> Option<String> {
        Some(self.received_in()?.join(is.kept_as()).to_string_lossy().into_owned())
    }

    /// The place this machine makes for the file in `slot` of a device of
    /// which nothing says what the file is: a name made here, of the
    /// device's number and the slot's letters. `said` is the name that
    /// arrived, of which only the kind of file is read.
    fn slot_home(folder: &Path, device: DeviceId, slot: &str, said: &str) -> String {
        let name = format!("{}.{}", slot_name(slot), kind_of(said));
        folder.join(OF_DEVICES).join(device.0.to_string()).join(name).to_string_lossy().into_owned()
    }

    // ---- what leaves -------------------------------------------------------

    /// The state of `device` as it is said to another program: the files
    /// it names without their places, each by its slot and its kind.
    /// `None` for a device whose state names none.
    fn without_places(device: &Device) -> Option<Vec<u8>> {
        let named = named_by(device);
        if named.is_empty() {
            return None;
        }
        let bare: Vec<(String, String)> =
            named.iter().map(|(slot, path)| (slot.clone(), format!("{}.{}", slot_name(slot), kind_of(path)))).collect();
        // (A state whose files cannot be named anew is said without one,
        // not with the places it has.)
        Some(SampleFiles.relinked(device, &bare).unwrap_or_default())
    }

    /// A part of a change as it is said to another program: no place a
    /// built-in device's state names is in it. (A clip's is taken out
    /// with what else is one's own: `merge::Change::impersonal`.)
    pub(super) fn forget_device_places(&self, op: &mut Op) {
        match op {
            Op::Device(_, DeviceOp::Whole(device, ..)) => {
                if let Some(state) = Self::without_places(device) {
                    device.state = Some(state);
                }
            }
            Op::Device(id, DeviceOp::Set(DeviceSet::State(Blob(Some(state))))) => {
                let held = self.project.device(*id).ok().cloned().or_else(|| self.doc.held_device(*id));
                if let Some(mut device) = held {
                    device.state = Some(std::mem::take(state));
                    *state = Self::without_places(&device).or(device.state).unwrap_or_default();
                }
            }
            _ => {}
        }
    }

    /// The open song as it is said to another program, and as every
    /// program that holds it has it: without what is each one's own (its
    /// listening, which input it records) and without where any file
    /// lies, a clip's or a sample player's.
    pub fn said_song(&self) -> Project {
        let mut song = trill_core::merge::said_song(&self.project);
        let devices = song.tracks.iter_mut().flat_map(|track| &mut track.devices).chain(&mut song.master.devices);
        for device in devices {
            if let Some(state) = Self::without_places(device) {
                device.state = Some(state);
            }
        }
        song
    }

    // ---- what arrives ------------------------------------------------------

    /// A song another program handed over, before it is opened: nothing
    /// of it names a place another program chose. Its clips have this
    /// machine's place for what they are, and its devices' files places
    /// made here.
    pub(super) fn keep_places_out_of(&self, song: &mut Project, kept: Kept) -> Kept {
        // (Whatever was said of what is one's own is not taken: places
        // among it.)
        let mut kept = kept.impersonal();
        let folder = self.folder_of(kept.song);
        let home = |is: &FileIs| folder.join(is.kept_as()).to_string_lossy().into_owned();
        *song = trill_core::merge::said_song(song);
        for track in &mut song.tracks {
            let clips = track.clips.iter_mut().chain(track.takes.iter_mut().map(|take| &mut take.clip));
            for clip in clips {
                if let ClipContent::Audio(audio) = &mut clip.content {
                    audio.path = audio.is.as_ref().map(home).unwrap_or_default();
                }
            }
        }
        let placed = |device: &Device| -> Option<Vec<u8>> {
            let named = named_by(device);
            if named.is_empty() {
                return None;
            }
            let here: Vec<(String, String)> = (named.iter())
                .map(|(slot, said)| {
                    let place = match device.files.get(slot) {
                        Some(is) => home(is),
                        None => Self::slot_home(&folder, device.id, slot, said),
                    };
                    (slot.clone(), place)
                })
                .collect();
            Some(SampleFiles.relinked(device, &here).unwrap_or_default())
        };
        let devices = song.tracks.iter_mut().flat_map(|track| &mut track.devices).chain(&mut song.master.devices);
        for device in devices {
            if let Some(state) = placed(device) {
                device.state = Some(state);
            }
        }
        for op in &mut kept.hidden {
            if let Op::Device(_, DeviceOp::Whole(device, ..)) = op
                && let Some(state) = placed(device)
            {
                device.state = Some(state);
            }
        }
        // (A state that is held for a device apart from the device is
        // rare, and is not worth a second look: it is let go of.)
        kept.hidden.retain(|op| !matches!(op, Op::Device(_, DeviceOp::Set(DeviceSet::State(_)))));
        kept
    }

    /// Whether `path` is a place of this machine's: one in the open
    /// song's folder of files from other programs, or one that was named
    /// here (by a song that was opened, an edit, a recording).
    fn is_a_place_here(&self, path: &str) -> bool {
        self.named_here.contains(path) || self.received_in().is_some_and(|folder| Path::new(path).starts_with(folder))
    }

    /// After anything arrived: gives what has none this machine's place
    /// for what it is.
    ///
    /// * A clip that has no place, and every clip in `said` (of which
    ///   another program has just said what it is), is given the place
    ///   for what it is.
    /// * Every file a built-in device's state names is held to being a
    ///   place of this machine's. A slot whose file arrived under a name
    ///   that is none, and every slot in `slots` (of which another
    ///   program has just said what its file is), is given the place for
    ///   what the song says it is; where the song does not say, the place
    ///   the device had for that slot before (`before`), or one made here.
    ///
    /// The place for what a file is: where this machine is known to have
    /// those bytes already ([`Session::files_read`]), so that nobody is
    /// asked for what one has; else in the song's folder, where they lie
    /// once they have arrived. One's own, said to nobody; no step.
    /// Returns whether anything was given a place.
    pub(super) fn give_places(
        &mut self,
        said: &[ClipId],
        slots: &[(DeviceId, String)],
        before: Option<&Project>,
    ) -> bool {
        let Some(folder) = self.received_in() else { return false };
        let place_for = |is: &FileIs| -> String {
            let known = self.read_here.get(&is.content()).cloned();
            known.unwrap_or_else(|| folder.join(is.kept_as()).to_string_lossy().into_owned())
        };
        let mut ops = Vec::new();
        for track in &self.project.tracks {
            for clip in track.clips.iter().chain(track.takes.iter().map(|take| &take.clip)) {
                if let Some(audio) = clip.audio()
                    && let Some(is) = &audio.is
                    && (audio.path.is_empty() || said.contains(&clip.id))
                {
                    let place = place_for(is);
                    if audio.path != place {
                        ops.push(Op::Clip(clip.id, ClipOp::Audio(AudioSet::Path(place))));
                    }
                }
            }
        }
        // And the clips that are held and not in the song as it stands.
        let alive: Vec<FilePlace> = self.files_here().into_iter().map(|file| file.place).collect();
        for clip in said.iter().filter(|clip| !alive.contains(&FilePlace::Clip(**clip))) {
            let is = self.doc.read(&trill_core::merge::Key::Clip(*clip, trill_core::merge::is_prop()));
            if let Some(Op::Clip(_, ClipOp::Audio(AudioSet::Is(Some(is))))) = is
                && self.doc.held_clip(*clip).is_some()
                && self.project.tracks.iter().all(|track| track.takes.iter().all(|take| take.clip.id != *clip))
                && self.project.clip(*clip).is_err()
            {
                ops.push(Op::Clip(*clip, ClipOp::Audio(AudioSet::Path(place_for(&is)))));
            }
        }
        // The devices: their states are placed where they are held, which
        // is the same state to anyone else.
        let mut placed = Vec::new();
        for device in self.project.devices() {
            let had: HashMap<String, String> = before
                .and_then(|song| song.device(device.id).ok())
                .map(named_by)
                .unwrap_or_default()
                .into_iter()
                .collect();
            let mut here = Vec::new();
            for (slot, path) in named_by(device) {
                let here_already = self.is_a_place_here(&path);
                let just_said = slots.iter().any(|(of, said)| *of == device.id && *said == slot);
                let place = match device.files.get(&slot) {
                    _ if here_already && !just_said => continue,
                    Some(is) => place_for(is),
                    None => match had.get(&slot).filter(|had| self.is_a_place_here(had)) {
                        Some(had) => had.clone(),
                        None => Self::slot_home(&folder, device.id, &slot, &path),
                    },
                };
                if place != path {
                    here.push((slot, place));
                }
            }
            if !here.is_empty() {
                // (A state whose files cannot be given places is not taken
                // with the places it came with: it is taken without one.)
                placed.push((device.id, SampleFiles.relinked(device, &here).unwrap_or_default()));
            }
        }
        if ops.is_empty() && placed.is_empty() {
            return false;
        }
        for (device, state) in placed {
            self.doc.place_state(device, Some(state));
        }
        if !ops.is_empty() {
            self.write_quietly(ops);
        }
        let _ = self.rederive();
        true
    }

    // ---- what whoever reads the files asks ---------------------------------

    /// The files the open song names that this machine has a place for,
    /// each with what the song says it is: its clips' and takes', and
    /// what its samplers and drum racks play. (Whether a file is there is
    /// for whoever asks to find out: this touches no disk.)
    pub fn files_here(&self) -> Vec<FileHere> {
        let folder = self.received_in();
        let may_go = |path: &str| {
            self.let_go.contains(path) || folder.as_ref().is_some_and(|folder| Path::new(path).starts_with(folder))
        };
        let mut files = Vec::new();
        for track in &self.project.tracks {
            for clip in track.clips.iter().chain(track.takes.iter().map(|take| &take.clip)) {
                if let Some(audio) = clip.audio().filter(|audio| !audio.path.is_empty()) {
                    files.push(FileHere {
                        place: FilePlace::Clip(clip.id),
                        path: audio.path.clone(),
                        is: audio.is.clone(),
                        may_go: may_go(&audio.path),
                    });
                }
            }
        }
        for device in self.project.devices().filter(|device| SAMPLE_PLAYERS.contains(&device.uri.as_str())) {
            for (slot, path) in named_by(device).into_iter().filter(|(_, path)| !path.is_empty()) {
                let (is, may_go) = (device.files.get(&slot).cloned(), may_go(&path));
                files.push(FileHere { place: FilePlace::Sample { device: device.id, slot }, path, is, may_go });
            }
        }
        files
    }

    /// Says what the file at a place of the song is, which this program
    /// has read at `path`: a property of the clip or of the device, which
    /// whoever else has the song hears of. No step. Nothing is said where
    /// the place no longer names that file, the file may not go, or the
    /// song says so already; returns whether it was.
    pub fn say_file(&mut self, place: &FilePlace, path: &str, is: FileIs) -> bool {
        let named = self.files_here().into_iter().find(|file| file.place == *place);
        let news = named.is_some_and(|file| file.path == path && file.may_go && file.is.as_ref() != Some(&is));
        if !news || !is.is_well_formed() || self.shared_song().is_none() {
            return false;
        }
        self.read_here.insert(is.content(), path.to_owned());
        let op = match place {
            FilePlace::Clip(clip) => Op::Clip(*clip, ClipOp::Audio(AudioSet::Is(Some(is)))),
            FilePlace::Sample { device, slot } => Op::Sample(*device, slot.clone(), Some(is)),
        };
        self.write_quietly(vec![op]);
        let _ = self.rederive();
        self.revision += 1;
        true
    }

    /// Says where this machine was found to have which bytes: every file
    /// of the song that was read here and is what it was read as, by its
    /// content. What arrives and is one of them is played from there.
    pub fn files_read(&mut self, read: HashMap<Content, String>) {
        self.read_here = read;
    }

    /// Every file the song says it plays, by what it is, with where this
    /// machine has it or will: each content once for each place.
    pub fn said_files(&self) -> Vec<(FileIs, PathBuf)> {
        let said = self.files_here().into_iter().filter_map(|file| Some((file.is?, PathBuf::from(file.path))));
        let mut said: Vec<(FileIs, PathBuf)> = said.collect();
        said.sort();
        said.dedup();
        said
    }

    /// Files of the song arrived at `paths`: they are played from here
    /// on. No edit, and whether the song has unsaved changes is as it was.
    ///
    /// What was known of the files is forgotten here and they are read
    /// anew, with the session held: whoever can read them first, away
    /// from the session (through [`Session::media`]), calls
    /// [`Session::files_were_read`] instead.
    pub fn files_are_here(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.audio.media().forget(path);
        }
        self.files_were_read(paths);
    }

    /// [`Session::files_are_here`], for files the caller has read through
    /// the session's media cache since they arrived: the song takes them
    /// up without reading them again.
    pub fn files_were_read(&mut self, paths: &[PathBuf]) {
        // A device that plays one of them is made anew: it looked for its
        // file when it was made, and found none.
        let arrived = |path: &String| paths.iter().any(|here| here == Path::new(path));
        let players: Vec<DeviceId> = (self.project.devices())
            .filter(|device| named_by(device).iter().any(|(_, path)| arrived(path)))
            .map(|device| device.id)
            .collect();
        for device in players {
            if let Some(record) = self.bank.devices.get_mut(&device) {
                record.applied_state = None;
            }
        }
        let saved = self.saved_revision == self.revision;
        self.compile_and_swap();
        self.revision += 1;
        if saved {
            self.saved_revision = self.revision;
        }
    }

    // ---- what may go -------------------------------------------------------

    /// Another song is open: nothing that was named or let in the one
    /// before counts in this one. What this one names on this disk was
    /// named here, and none of it is let go yet.
    pub(super) fn forget_what_may_go(&mut self) {
        self.named_here = named_in(&self.project).into_iter().collect();
        self.let_go.clear();
        self.read_here.clear();
    }

    /// What a change that was made here names: these are places of this
    /// machine's. Where the change is the user's own hand's, the files
    /// may go: the hand brought them into the song.
    pub(super) fn note_what_is_named(&mut self, ops: &[Op]) {
        if ops.iter().all(|op| !matches!(op, Op::Clip(..) | Op::Device(..))) {
            return;
        }
        let mut named = Vec::new();
        for op in ops {
            match op {
                Op::Clip(_, ClipOp::Whole(clip, _)) => named.extend(clip.audio().map(|audio| audio.path.clone())),
                Op::Clip(_, ClipOp::Audio(AudioSet::Path(path))) => named.push(path.clone()),
                Op::Device(_, DeviceOp::Whole(device, ..)) => {
                    named.extend(named_by(device).into_iter().map(|(_, path)| path));
                }
                Op::Device(id, DeviceOp::Set(DeviceSet::State(Blob(Some(state))))) => {
                    let held = self.project.device(*id).ok().cloned().or_else(|| self.doc.held_device(*id));
                    if let Some(mut device) = held {
                        device.state = Some(state.clone());
                        named.extend(named_by(&device).into_iter().map(|(_, path)| path));
                    }
                }
                _ => {}
            }
        }
        named.retain(|path| !path.is_empty());
        if crate::hand::is_by_hand() {
            self.let_go.extend(named.iter().cloned());
        }
        self.named_here.extend(named);
    }

    /// Files this program wrote itself for the open song (what was
    /// recorded in it): places of this machine's, which may go.
    pub(super) fn let_recorded_go(&mut self, paths: impl IntoIterator<Item = String>) {
        for path in paths {
            self.named_here.insert(path.clone());
            self.let_go.insert(path);
        }
    }

    /// A file has another name than it had (a recording that came beside
    /// its song): what was so of it under the old name is so under the new.
    pub(super) fn follow_renamed(&mut self, old: &str, new: &str) {
        // (What the song brought here and now lies beside it is the
        // song's to hand on as it was.)
        let brought = self.is_awaited(old);
        if brought || self.named_here.contains(old) {
            self.named_here.insert(new.to_owned());
        }
        if brought || self.let_go.contains(old) {
            self.let_go.insert(new.to_owned());
        }
    }

    /// Of what a compile found wrong with the song, what is no problem:
    /// a file that is not there and whose place is in the song's folder
    /// of files from other programs is on its way. Returns the places
    /// that wait for one, and the problems that are left.
    pub(super) fn arriving_among(&self, problems: Vec<crate::Problem>) -> (Vec<FilePlace>, Vec<crate::Problem>) {
        let Some(folder) = self.received_in() else { return (Vec::new(), problems) };
        let within = |path: &str| Path::new(path).starts_with(&folder);
        let mut arriving = Vec::new();
        let mut left = Vec::new();
        for problem in problems {
            let place = match &problem {
                crate::Problem::AudioFile { clip, path, .. } if within(path) => Some(FilePlace::Clip(*clip)),
                crate::Problem::Sample { device, slot: Some(slot), .. } => {
                    // Only a file of which the song says what it is can
                    // arrive: one that nobody said anything of (it stayed
                    // with its maker, or is no audio) is a file that is
                    // not here, said as that.
                    let held = self.project.device(*device).ok();
                    let said = held.is_some_and(|device| device.files.contains_key(slot));
                    let named = held.map(named_by).unwrap_or_default();
                    let awaited = said && named.iter().any(|(named, path)| named == slot && within(path));
                    awaited.then(|| FilePlace::Sample { device: *device, slot: slot.clone() })
                }
                _ => None,
            };
            match place {
                Some(place) => arriving.push(place),
                None => left.push(problem),
            }
        }
        (arriving, left)
    }

    /// The places of the song (clips, takes that are played, slots of
    /// sample players) whose file is on its way from another program: it
    /// is not here yet, the place is silent meanwhile, and it plays when
    /// the file is there. These are not among [`Session::problems`] and
    /// not among the song's missing files.
    pub fn files_arriving(&self) -> &[FilePlace] {
        &self.arriving_files
    }

    /// Whether `path` is where a file of the open song lies once it has
    /// arrived from another program.
    pub fn is_awaited(&self, path: &str) -> bool {
        // (Not a place that was made for a device's file of which nobody
        // said what it is: nothing arrives there.)
        self.received_in().is_some_and(|folder| {
            Path::new(path).starts_with(&folder) && !Path::new(path).starts_with(folder.join(OF_DEVICES))
        })
    }

    /// The name the open song says the file at `path` by, where it says:
    /// for a file that came from another program, whose name here is its
    /// hash.
    pub(super) fn said_name_of(&self, path: &str) -> Option<String> {
        let file = self.files_here().into_iter().find(|file| file.path == path)?;
        file.is.map(|is| is.name).filter(|name| !name.is_empty())
    }

    /// The folders this program keeps what it records in, for songs that
    /// have no place of their own yet.
    pub fn recordings_folders(&self) -> Vec<PathBuf> {
        let base = self.unsaved_base();
        vec![base.parent().map_or(base.clone(), Path::to_owned)]
    }

    /// Lets every file the song names on this machine go: from here on
    /// each may be read to say what it is, and is handed to whoever in the
    /// song asks for it by that. For the user who was shown them, and for
    /// nobody else: sharing a song is this, and so is the answer to the
    /// notice of what is held.
    pub fn let_files_go(&mut self) {
        let named = named_in(&self.project);
        self.named_here.extend(named.iter().cloned());
        self.let_go.extend(named);
        self.revision += 1;
    }

    /// The files the song names on this machine that may not go: nobody
    /// was shown them and nobody's hand brought them into the song. Each
    /// once, in order. Empty for a song that is nobody else's.
    pub fn files_held(&self) -> Vec<String> {
        if self.shared_song().is_none() {
            return Vec::new();
        }
        let held: BTreeSet<String> =
            self.files_here().into_iter().filter(|file| !file.may_go).map(|file| file.path).collect();
        held.into_iter().collect()
    }
}
