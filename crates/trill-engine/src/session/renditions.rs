//! Tracks heard where their plugins are not: renditions, as the session
//! has them (`trill_core::renditions` says what one is).
//!
//! A device that cannot be made on this machine (a plugin that is not
//! installed here, or one that does not start) is *lacking*. Where the
//! song says a track has a rendition, a machine that lacks one of the
//! devices it was made after plays the recording in their place; the
//! compiler does that from the song alone, so a render and a song that
//! plays are the same.
//!
//! What the session does here:
//!
//! * It says what it lacks ([`Session::lacking`]), which whoever connects
//!   it to the others passes on, and what a rendition would take
//!   ([`Session::renditions_wanted`]): making one is not the session's
//!   business (`trill-runtime`).
//! * It takes in a rendition this program made ([`Session::land_rendition`]):
//!   no step, nobody's edit.
//! * It keeps which recording this machine plays for each track's
//!   rendition ([`Session::place_renditions`]): the rendition's own once
//!   that is on this machine, and until then the one before it, so that a
//!   track does not fall silent for as long as a newer recording takes to
//!   arrive. A recording that was let go of for a newer one is removed:
//!   it was a program's making, and nobody's recording.
//!
//! A recording lies where every file that came with the shared song lies:
//! in the song's own folder of the program's, under the name of what it is
//! ([`Session::home_of`]). The program that makes one puts it there too.

use super::Session;
use crate::Problem;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};
use trill_core::files::{FilePlace, lies_in};
use trill_core::merge::{Op, TrackOp, TrackSet};
use trill_core::{DeviceId, Rendition, Track, TrackId, TrackKind};

/// A track's rendition as this machine has it, for a front end.
#[derive(Clone, Debug, PartialEq)]
pub struct RenditionHere {
    pub track: TrackId,
    /// The devices it stands in for, in the track's order.
    pub devices: Vec<DeviceId>,
    /// Those of them this machine has not got. Empty where it has them
    /// all: then it plays them, and no recording.
    pub lacking: Vec<DeviceId>,
    /// Who made it, as they go by.
    pub by: String,
    /// Whether it is of the song as it is: nothing a recording depends on
    /// was changed since it was made.
    pub current: bool,
    /// Whether this machine plays a recording in place of those devices.
    pub played: bool,
    /// Whether the recording it has to play is the rendition's own, and
    /// not the one before it while the newer one is on its way.
    pub newest: bool,
}

impl RenditionHere {
    /// Whether what is heard here is the track as the song has it now.
    pub fn as_the_song_is(&self) -> bool {
        self.current && self.newest
    }
}

/// A rendition that is to be made: of a track, after one of its devices,
/// by a program that has all of `takes`.
#[derive(Clone, Debug, PartialEq)]
pub struct RenditionWanted {
    pub track: TrackId,
    /// The last of the track's devices that someone lacks.
    pub after: DeviceId,
    /// How many of the track's devices that is, from the first.
    pub devices: usize,
    /// The devices a program has to have to make it.
    pub takes: Vec<DeviceId>,
    /// The stretch of the song the track plays anything in, in beats:
    /// what is recorded.
    pub stretch: (f64, f64),
}

/// What each track's rendition would be made of now, as last worked out.
/// Working it out writes the track out, with the states of its plugins,
/// which takes milliseconds in a large song: more than a window that asks
/// with every change to the song can be made to wait for. So where it
/// takes that long it is worked out now and then, and what is said of a
/// rendition meanwhile is as it was a moment ago.
#[derive(Default)]
pub(super) struct Prints {
    /// The revision of the song they were worked out at.
    revision: Option<u64>,
    at: Option<Instant>,
    took: Duration,
    of: HashMap<TrackId, Option<String>>,
}

/// From how long working the prints out is not done with every change.
const WORTH_WAITING: Duration = Duration::from_millis(2);

impl Session {
    fn is_lacking(&self, device: DeviceId) -> bool {
        self.bank.devices.get(&device).is_some_and(|meta| meta.error.is_some())
    }

    /// The devices of the open song that could not be made on this
    /// machine: a plugin that is not installed here, or one that does not
    /// start. (One that is still on its way is not lacking.)
    pub fn lacking(&self) -> Vec<DeviceId> {
        let mut lacking: Vec<DeviceId> =
            self.project.devices().map(|device| device.id).filter(|id| self.is_lacking(*id)).collect();
        lacking.sort_unstable();
        lacking
    }

    /// Says which devices of the song someone else who holds it has not
    /// got. While there is such a device, every program that holds the
    /// song wants the recording that stands in for it, to hand it on:
    /// whoever lacks the device may be connected to a program that neither
    /// lacks it nor made the recording, and gets it from there or not at
    /// all. Nothing of the song changes by this.
    pub fn others_lack(&mut self, devices: &HashSet<DeviceId>) {
        if self.lacked_elsewhere != *devices {
            self.lacked_elsewhere.clone_from(devices);
            self.revision += 1;
        }
    }

    /// The renditions that are to be made where the devices in `lacked`
    /// are lacking on some machine that holds the song: for every track
    /// of the song that plays clips and has one of them, a recording after
    /// the last of them.
    pub fn renditions_wanted(&self, lacked: &HashSet<DeviceId>) -> Vec<RenditionWanted> {
        let of_the_song = |track: &&Track| track.kind == TrackKind::Normal && track.reference.is_none();
        (self.project.tracks.iter().filter(of_the_song))
            .filter_map(|track| {
                let at = track.devices.iter().rposition(|device| lacked.contains(&device.id))?;
                let after = track.devices[at].id;
                // (A track that plays nothing has nothing to record.)
                let stretch = self.project.stretch_played(track.id)?;
                let takes = self.project.rendition_takes(track.id, after);
                Some(RenditionWanted { track: track.id, after, devices: at + 1, takes, stretch })
            })
            .collect()
    }

    /// What a recording of each of `of` (a track, after one of its
    /// devices) depends on, as `Project::rendition_print` says it of the
    /// song as it is said to others: the same text in every program that
    /// holds the song. `None` where there is no such track or device.
    pub fn rendition_prints(&self, of: &[(TrackId, DeviceId)]) -> Vec<Option<String>> {
        if of.is_empty() {
            return Vec::new();
        }
        let said = self.said_song();
        of.iter().map(|(track, after)| said.rendition_print(*track, *after)).collect()
    }

    /// The renditions of the song's tracks, as this machine has them.
    pub fn renditions(&self) -> Vec<RenditionHere> {
        let with: Vec<(&Track, usize)> =
            self.project.tracks.iter().filter_map(|track| Some((track, track.stood_in()?))).collect();
        if with.is_empty() {
            return Vec::new();
        }
        // What each would be made of now: worked out again where the song
        // has changed, unless that takes long and was done a moment ago.
        let mut prints = self.prints.lock();
        let due = prints.took < WORTH_WAITING || prints.at.is_none_or(|at| at.elapsed() >= prints.took * 20);
        if prints.revision != Some(self.revision) && due {
            let began = Instant::now();
            let said = self.said_song();
            prints.of = (with.iter())
                .filter_map(|(track, _)| {
                    Some((track.id, said.rendition_print(track.id, track.rendition.as_ref()?.after)))
                })
                .collect();
            (prints.revision, prints.took, prints.at) = (Some(self.revision), began.elapsed(), Some(Instant::now()));
        }
        let unread = |track: TrackId| {
            let not_read = |problem: &Problem| matches!(problem, Problem::Rendition { track: of, .. } if *of == track);
            self.problems.iter().any(not_read) || self.arriving_files.contains(&FilePlace::Rendition(track))
        };
        (with.into_iter())
            .filter_map(|(track, count)| {
                let rendition = track.rendition.as_ref()?;
                let devices: Vec<DeviceId> = track.devices[..count].iter().map(|device| device.id).collect();
                let lacking: Vec<DeviceId> = devices.iter().copied().filter(|id| self.is_lacking(*id)).collect();
                let here = track.rendition_here();
                let print = prints.of.get(&track.id).cloned().flatten();
                Some(RenditionHere {
                    track: track.id,
                    by: rendition.by.clone(),
                    current: print.as_deref() == Some(rendition.of.as_str()),
                    played: !lacking.is_empty() && here.is_some() && !unread(track.id),
                    newest: here.is_some() && here == self.home_of(&rendition.is).as_deref(),
                    devices,
                    lacking,
                })
            })
            .collect()
    }

    /// Takes in a rendition this program made of `track`. Its recording
    /// lies in the song's own folder of the program's, under the name of
    /// what it is ([`Session::home_of`]), where whoever made it put it.
    /// From here on the track says what stands in for its first devices,
    /// and whoever else holds the song hears of it. No step: this is
    /// nobody's edit, and no undo takes it back.
    ///
    /// Refused, in a sentence, where the song is not one others hold, the
    /// track cannot have one, its device is gone or the recording is not
    /// there.
    pub fn land_rendition(&mut self, track: TrackId, rendition: Rendition) -> Result<(), String> {
        let Some(home) = self.home_of(&rendition.is) else {
            return Err("the song is not one that others hold too: nobody needs a rendition of it".to_owned());
        };
        if !Path::new(&home).is_file() {
            return Err(format!("the recording of the rendition is not where it belongs ({home})"));
        }
        self.project.can_be_rendered(track).map_err(|error| error.to_string())?;
        let held = self.project.track(track).map_err(|error| error.to_string())?;
        if !held.devices.iter().any(|device| device.id == rendition.after) {
            return Err(format!("{} is no longer on {track}", rendition.after));
        }
        if !rendition.is.is_well_formed() || rendition.of.is_empty() {
            return Err("a rendition says what its recording is and what it was made of".to_owned());
        }
        let before = held.rendition.as_ref().map(|before| self.home_of(&before.is)).unwrap_or_default();
        self.write_quietly(vec![Op::Track(track, TrackOp::Set(TrackSet::Rendition(Some(rendition))))]);
        let _ = self.rederive();
        // (It is here: the recording this machine has for the track is
        // this one from now on.)
        self.place_renditions();
        self.revision += 1;
        // The recording before this one was this program's making too,
        // and is nobody's any more.
        if let Some(before) = before.filter(|before| *before != home) {
            self.remove_superseded(&before);
        }
        Ok(())
    }

    /// Brings which recording each track plays for its rendition up to
    /// what is on this machine: the rendition's own once it is here; the
    /// place it will have, where the track had no recording to play; and
    /// otherwise the one it has, until the newer one is here. A track
    /// that no longer has a rendition plays none. One's own, said to
    /// nobody; no step. Returns whether anything changed.
    pub(super) fn place_renditions(&mut self) -> bool {
        let Some(folder) = self.received_in() else { return false };
        let mut ops = Vec::new();
        let mut superseded = Vec::new();
        for track in &self.project.tracks {
            let home = track.rendition.as_ref().map(|found| folder.join(found.is.kept_as()));
            let home = home.map(|home| home.to_string_lossy().into_owned());
            let place = match &home {
                None => String::new(),
                Some(home) if track.rendition_at.is_empty() || Path::new(home).is_file() => home.clone(),
                Some(_) => continue,
            };
            if place != track.rendition_at {
                if !track.rendition_at.is_empty() {
                    superseded.push(track.rendition_at.clone());
                }
                ops.push(Op::Track(track.id, TrackOp::Set(TrackSet::RenditionAt(place))));
            }
        }
        if ops.is_empty() {
            return false;
        }
        self.write_quietly(ops);
        let _ = self.rederive();
        for path in superseded {
            self.remove_superseded(&path);
        }
        true
    }

    /// Removes a recording that no rendition of the song is any more and
    /// that no track plays: only one that lies in the song's own folder of
    /// the program's, where renditions are kept under the names of what
    /// they are.
    fn remove_superseded(&mut self, path: &str) {
        let Some(folder) = self.received_in() else { return };
        if !lies_in(Path::new(path), &folder) {
            return;
        }
        let named = |track: &Track| {
            let home = track.rendition.as_ref().and_then(|found| self.home_of(&found.is));
            track.rendition_at == path || home.as_deref() == Some(path)
        };
        // (Nor anything a clip or a device of the song plays, whatever a
        // song file may have said the track's recording is.)
        let played = |file: &super::FileHere| file.path == path && !matches!(file.place, FilePlace::Rendition(_));
        if self.project.tracks.iter().any(named) || self.files_here().iter().any(played) {
            return;
        }
        self.audio.media().forget(Path::new(path));
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("a rendition that is no longer the song's could not be removed: {error}");
        }
    }
}
