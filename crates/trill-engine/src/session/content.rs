//! The files of a song that other programs hold too: known by what they
//! are, each machine with its own place for them.
//!
//! In a song of one's own a clip names a file by its path, and that is
//! all. A path means one place on one machine, so in a song others hold a
//! file is known by what it *is* (`trill_core::files::FileIs`: the hash
//! of its bytes, how many they are, its name), which is the song's, and
//! where it lies is each machine's own.
//!
//! **No path is taken from another program.** A path is never said
//! (`merge::Change::impersonal`), and one that arrives all the same is
//! never taken: a change that sets one is refused, a clip that arrives
//! whole arrives without one (`merge::Op::checked`), and a device's state
//! that arrives has the files it names given places of this machine's
//! making before it is taken in ([`Session::keep_out_places`]). So there
//! is nothing downstream that could open, read, write or hand out a path
//! someone else chose: whatever reads `audio.path` or a device's state
//! reads a place this machine made.
//!
//! What this file does:
//!
//! * A clip that arrives is given this machine's place for what it is
//!   ([`Session::give_places`]): in the folder files from other programs
//!   are kept in ([`Session::keep_received_in`]), under a name made here.
//!   The file is there or it is on its way; fetching it is not the
//!   session's business (`trill-runtime`).
//! * The program that has a file says what it is, once it has read it
//!   ([`Session::files_here`], [`Session::say_file`]): a property of the
//!   clip, written without a step, which travels like any other.
//! * [`Session::said_files`] is every file the song says it plays, with
//!   where this machine has it or will: what whoever fetches and hands
//!   out goes by.
//! * [`Session::files_are_here`]: files arrived, and the song plays them
//!   from here on without being opened again.

use super::Session;
use std::path::PathBuf;
use trill_core::files::{FileIs, kind_of};
use trill_core::merge::{AudioSet, Blob, Change, ClipOp, DeviceOp, DeviceSet, Kept, Op, is_prop};
use trill_core::project::ClipContent;
use trill_core::{ClipId, Device, DeviceFiles, Project};
use trill_dsp::SampleFiles;

/// A file a clip of the open song names, as this machine has it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileHere {
    pub clip: ClipId,
    /// Where this machine has the file.
    pub path: String,
    /// What the song says it is, if it says.
    pub is: Option<FileIs>,
}

/// The folder files from other programs are named in where nobody has
/// said which: nothing is ever written there by the session.
fn nowhere_yet() -> PathBuf {
    std::env::temp_dir().join("trill-received")
}

impl Session {
    /// Says where the files that come from other programs are kept on
    /// this machine: a folder of the program's own. What arrives in a
    /// shared song is named there.
    pub fn keep_received_in(&mut self, dir: Option<PathBuf>) {
        self.received = dir;
    }

    /// The folder files from other programs are kept in.
    pub fn received_in(&self) -> PathBuf {
        self.received.clone().unwrap_or_else(nowhere_yet)
    }

    /// Where this machine has a file that is `is`, or will have it once
    /// it has arrived: in the folder of files from other programs, under
    /// its hash and its kind.
    pub fn home_of(&self, is: &FileIs) -> String {
        self.received_in().join(is.kept_as()).to_string_lossy().into_owned()
    }

    /// The place this machine gives the file in `slot` of a device whose
    /// state arrived from another program: a name made here, of the
    /// device's number and the slot's letters. `said` is the path that
    /// arrived, of which only the kind of file is read.
    fn slot_home(&self, device: &Device, slot: &str, said: &str) -> String {
        let slot: String = slot.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(64).collect();
        let name = format!("{slot}.{}", kind_of(said));
        self.received_in().join("of devices").join(device.id.0.to_string()).join(name).to_string_lossy().into_owned()
    }

    /// `device` as it is taken in from another program: every file its
    /// state names is given a place of this machine's making. `None`
    /// where the state names none.
    fn placed_here(&self, device: &Device) -> Option<Vec<u8>> {
        let said = SampleFiles.files(device);
        if said.is_empty() {
            return None;
        }
        let here: Vec<(String, String)> =
            said.iter().map(|(slot, path)| (slot.clone(), self.slot_home(device, slot, path))).collect();
        match SampleFiles.relinked(device, &here) {
            Ok(state) => Some(state),
            // A state whose files cannot be given other places is not
            // taken with the places it came with: it is taken without one.
            Err(error) => {
                log::warn!("a device's state arrived whose files could not be given places here: {error}");
                Some(Vec::new())
            }
        }
    }

    /// A change that arrived, before it is taken in: no place a device's
    /// state names is taken from it. (A clip's place is not in it:
    /// [`merge::Op::checked`](trill_core::merge::Op::checked).)
    pub(super) fn keep_out_places(&self, change: &mut Change) {
        for op in &mut change.ops {
            match op {
                Op::Device(_, DeviceOp::Whole(device, ..)) => {
                    if let Some(state) = self.placed_here(device) {
                        device.state = Some(state);
                    }
                }
                Op::Device(id, DeviceOp::Set(DeviceSet::State(Blob(Some(state))))) => {
                    // What kind of device it is, is what the song holds of it.
                    let held = self.project.device(*id).ok().cloned().or_else(|| self.doc.held_device(*id));
                    let Some(mut device) = held else {
                        // A state for a device that is not known here yet
                        // waits with the properties: it is taken when the
                        // device is, and looked at then.
                        continue;
                    };
                    device.state = Some(std::mem::take(state));
                    *state = self.placed_here(&device).or(device.state).unwrap_or_default();
                }
                _ => {}
            }
        }
    }

    /// A song another program handed over, before it is opened: nothing
    /// of it names a place another program chose. Its clips have this
    /// machine's place for what they are, and its devices' files places
    /// made here.
    pub(super) fn keep_places_out_of(&self, song: &mut Project, kept: Kept) -> Kept {
        // (Whatever was said of what is one's own is not taken: places
        // among it.)
        let mut kept = kept.impersonal();
        *song = trill_core::merge::said_song(song);
        for track in &mut song.tracks {
            let clips = track.clips.iter_mut().chain(track.takes.iter_mut().map(|take| &mut take.clip));
            for clip in clips {
                if let ClipContent::Audio(audio) = &mut clip.content {
                    audio.path = audio.is.as_ref().map(|is| self.home_of(is)).unwrap_or_default();
                }
            }
        }
        let devices = song.tracks.iter_mut().flat_map(|track| &mut track.devices).chain(&mut song.master.devices);
        for device in devices {
            if let Some(state) = self.placed_here(device) {
                device.state = Some(state);
            }
        }
        for op in &mut kept.hidden {
            if let Op::Device(_, DeviceOp::Whole(device, ..)) = op
                && let Some(state) = self.placed_here(device)
            {
                device.state = Some(state);
            }
        }
        // (A state that is held for a device apart from the device is
        // rare, and is not worth a second look: it is let go of.)
        kept.hidden.retain(|op| !matches!(op, Op::Device(_, DeviceOp::Set(DeviceSet::State(_)))));
        kept
    }

    /// Gives the clips that have none this machine's place for what they
    /// are, and the clips in `said` (of which another program has just
    /// said what they are) the place for that: where the file lies here
    /// once it has arrived. One's own, said to nobody; no step. Returns
    /// whether any was given one.
    pub(super) fn give_places(&mut self, said: &[ClipId]) -> bool {
        // Where this machine has a file already, by what it is: a clip
        // that another program made of a file one has (a piece cut from
        // one's own recording, say) plays one's own copy, and nobody is
        // asked for it. (A clip of which it was just said what it is does
        // not count: its place was for what it was before.)
        let mut have: std::collections::HashMap<trill_core::Content, String> = std::collections::HashMap::new();
        for file in self.files_here().into_iter().filter(|file| !said.contains(&file.clip)) {
            if let Some(is) = &file.is {
                have.entry(is.content()).or_insert(file.path);
            }
        }
        let mut ops = Vec::new();
        let mut place = |clip: ClipId, path: &str, is: &FileIs, anew: bool| {
            let home = have.get(&is.content()).cloned().unwrap_or_else(|| self.home_of(is));
            if (anew || path.is_empty()) && path != home {
                ops.push(Op::Clip(clip, ClipOp::Audio(AudioSet::Path(home))));
            }
        };
        for track in &self.project.tracks {
            for clip in track.clips.iter().chain(track.takes.iter().map(|take| &take.clip)) {
                if let Some(audio) = clip.audio()
                    && let Some(is) = &audio.is
                {
                    place(clip.id, &audio.path, is, said.contains(&clip.id));
                }
            }
        }
        // And the ones that are held and not in the song as it stands.
        for clip in said.iter().filter(|clip| self.project.clip(**clip).is_err()) {
            let is = self.doc.read(&trill_core::merge::Key::Clip(*clip, is_prop()));
            if let Some(Op::Clip(_, ClipOp::Audio(AudioSet::Is(Some(is))))) = is
                && let Some((_, held)) = self.doc.held_clip(*clip)
                && self.project.tracks.iter().all(|track| track.takes.iter().all(|take| take.clip.id != *clip))
            {
                place(*clip, held.audio().map_or("", |audio| audio.path.as_str()), &is, true);
            }
        }
        if ops.is_empty() {
            return false;
        }
        self.write_quietly(ops);
        let _ = self.rederive();
        true
    }

    /// The files the clips of the open song name that this machine has a
    /// place for, each with what the song says it is: for whoever reads
    /// them to say what they are. (Whether a file is there is theirs to
    /// find out: this touches no disk.)
    pub fn files_here(&self) -> Vec<FileHere> {
        let mut files = Vec::new();
        for track in &self.project.tracks {
            for clip in track.clips.iter().chain(track.takes.iter().map(|take| &take.clip)) {
                if let Some(audio) = clip.audio().filter(|audio| !audio.path.is_empty()) {
                    files.push(FileHere { clip: clip.id, path: audio.path.clone(), is: audio.is.clone() });
                }
            }
        }
        files
    }

    /// Says what the file of `clip` is, which this program has read at
    /// `path`: a property of the clip, which whoever else has the song
    /// hears of. No step. Nothing is said where the clip no longer names
    /// that file, or the song says so already; returns whether it was.
    pub fn say_file(&mut self, clip: ClipId, path: &str, is: FileIs) -> bool {
        let named = self.files_here().into_iter().find(|file| file.clip == clip);
        if !named.is_some_and(|file| file.path == path && file.is.as_ref() != Some(&is)) {
            return false;
        }
        if !is.is_well_formed() {
            return false;
        }
        self.write_quietly(vec![Op::Clip(clip, ClipOp::Audio(AudioSet::Is(Some(is))))]);
        let _ = self.rederive();
        self.revision += 1;
        true
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
    pub fn files_are_here(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.audio.media().forget(path);
        }
        let saved = self.saved_revision == self.revision;
        self.compile_and_swap();
        self.revision += 1;
        if saved {
            self.saved_revision = self.revision;
        }
    }
}
