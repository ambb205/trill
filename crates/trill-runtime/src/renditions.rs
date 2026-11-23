//! Making tracks heard where their plugins are not: the renditions this
//! program makes for the others who hold the song with it
//! (`trill_core::renditions` says what one is, and
//! `trill_engine::session::renditions` what a session does with one).
//!
//! Everyone who holds the song says which of its devices their machine
//! has not got. For each track of the song that has such a device, one
//! program records what the track's devices make of it up to the last of
//! them that someone lacks, and that recording goes to the others like
//! any file of the song, by what it is. Who records: whoever has every
//! device the recording takes, and of those the one whose number is
//! lowest, so that it is made once.
//!
//! **It is made of the song as it stands still.** While a track's notes or
//! the devices in question are being worked on, nothing is recorded: one
//! is begun once what it depends on has been the same for a couple of
//! seconds, and one that is being made of a song that has changed since
//! is stopped. The others hear the track as it was meanwhile, which they
//! are told.
//!
//! **It is a render like any other**, away from the session: of a song
//! with plugins in a process of its own (`trill_engine::apart`), so that a
//! plugin that faults there costs a sentence. **Every track that is
//! wanted at that moment is recorded in the one pass**, each into a file
//! of its own: a render makes the song's plugins anew and waits for them,
//! which is most of what recording one track takes, and is taken once for
//! all of them. (Five tracks of a song, each recorded in a pass of its
//! own, took seven minutes on a busy machine.) Where such a pass comes to
//! nothing, each of its tracks is tried alone, so that the one that
//! cannot be recorded is the only one that is not. A recording is written
//! into a folder beside the song's own folder of the program's, hashed,
//! and given the name of what it is in that folder, where what arrives
//! from others lies too.
//!
//! None of it is done on the thread that connects the session to the
//! wire, which only looks whether there is anything to make and takes in
//! what was made.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};
use trill_core::files::FileIs;
use trill_core::{DeviceId, Rendition, TrackId};
use trill_engine::render::RenderError;
use trill_engine::{After, RenderOptions, SharedSession, Watch};
use trill_together::Person;
use trill_together::files::content_of;

/// How long what a rendition depends on has to have been the same before
/// one is made of it: nobody's every note is recorded.
const SETTLED: Duration = Duration::from_secs(2);
/// How often it is looked whether there is anything to make.
const LOOK_EVERY: Duration = Duration::from_millis(400);
/// The rate a rendition is recorded at.
const RATE: u32 = 48_000;
/// What the folder a recording is made in is called, after the folder of
/// the song's files that it lies beside: on the same disk, so that the
/// finished recording is given its name there by a rename, and not in
/// that folder itself, where whatever is no file of the song and no part
/// of one on its way is cleared away.
const MAKING: &str = "making";
/// How old what a program that is gone left in that folder has to be
/// before it is cleared away.
const LEFT_BEHIND: Duration = Duration::from_secs(3600);
/// What a making that was stopped says came of it.
const STOPPED: &str = "stopped";

/// The people who hold the song with this program: each by their number,
/// with the devices their machine has not got.
type Others = Vec<(u64, Vec<DeviceId>)>;

/// A rendition that is to be made.
struct Wanted {
    after: DeviceId,
    /// How many of the track's devices that is, from the first.
    devices: usize,
    /// The stretch of the song the track plays anything in, in beats.
    stretch: (f64, f64),
    /// What the song is, as far as the recording depends on it.
    print: String,
    /// Since when that has been so.
    since: Instant,
}

/// One track of a pass that is being made.
#[derive(Clone)]
struct One {
    track: TrackId,
    after: DeviceId,
    /// How many of the track's devices that is, from the first.
    devices: usize,
    /// Where in the song its recording begins, in beats.
    from: f64,
    print: String,
    name: String,
    /// Where it is written while it is made.
    part: PathBuf,
}

/// The pass that is being made.
struct Making {
    /// Which pass, counted from the first.
    number: u64,
    tracks: Vec<One>,
    watch: Arc<Watch>,
}

/// What came of a pass: of each of its tracks.
struct Made {
    number: u64,
    tracks: Vec<(One, Result<FileIs, String>)>,
}

/// The renditions this program makes, as the thread that drives the song
/// holds them.
pub(crate) struct Renditions {
    wanted: HashMap<TrackId, Wanted>,
    making: Option<Making>,
    made: (Sender<Made>, Receiver<Made>),
    /// What could not be made, by its track, with what the song was then:
    /// not tried again while that is the song.
    failed: HashMap<TrackId, String>,
    /// The tracks that are recorded in a pass of their own: a pass they
    /// were in with others came to nothing.
    alone: HashSet<TrackId>,
    looked: Instant,
    /// Counts the passes begun.
    begun: u64,
    /// The song and the others as they were when it was last looked what
    /// is wanted: while neither has changed, nothing else is wanted.
    seen: Option<(u64, Others)>,
}

impl Renditions {
    pub fn new() -> Self {
        Self {
            wanted: HashMap::new(),
            making: None,
            made: channel(),
            failed: HashMap::new(),
            alone: HashSet::new(),
            looked: Instant::now(),
            begun: 0,
            seen: None,
        }
    }

    /// What is being recorded for the others, in a few words: the track,
    /// or the first of them and how many more.
    pub fn making(&self) -> Option<String> {
        let tracks = &self.making.as_ref()?.tracks;
        let first = tracks.first()?.name.clone();
        Some(match tracks.len() {
            1 => first,
            2 => format!("{first} and 1 more track"),
            many => format!("{first} and {} more tracks", many - 1),
        })
    }

    /// Nothing is made any more: nobody else holds the song, or it is
    /// another song. What is being made is stopped.
    pub fn stop(&mut self) {
        if let Some(making) = self.making.take() {
            making.watch.stop();
        }
        self.wanted.clear();
        self.failed.clear();
        self.alone.clear();
        self.seen = None;
    }

    /// The song is no longer worked in with anyone: nobody else lacks
    /// anything, and nothing is made.
    pub fn leave(&mut self, session: &SharedSession) {
        self.stop();
        session.lock().others_lack(&HashSet::new());
    }

    /// Looks after the renditions: takes in what was made, and begins the
    /// next pass where one is wanted of this program. `others` are the
    /// people who hold the song with this one, each by their number with
    /// the devices their machine has not got. Returns what there is to
    /// say to the user.
    pub fn tend(&mut self, session: &SharedSession, me: &Person, others: &[(u64, Vec<DeviceId>)]) -> Vec<String> {
        let mut said = Vec::new();
        while let Ok(made) = self.made.1.try_recv() {
            if self.making.as_ref().is_some_and(|making| making.number == made.number) {
                self.making = None;
            }
            self.took(session, me, made, &mut said);
        }
        if self.looked.elapsed() < LOOK_EVERY {
            return said;
        }
        self.looked = Instant::now();
        let lacked: HashSet<DeviceId> = others.iter().flat_map(|(_, lacks)| lacks.iter().copied()).collect();
        // (Whoever holds the song keeps what stands in for a device that
        // someone lacks, to hand it on.)
        session.lock().others_lack(&lacked);
        if lacked.is_empty() {
            // Everyone has everything: nothing is made, and what was is
            // left as it is for whoever opens the song without them.
            self.stop();
            return said;
        }
        self.look(session, me, others, &lacked);
        // A pass none of whose tracks is wanted any more as it is being
        // made (the song has changed under all of them, or they are no
        // longer this program's to make) is stopped. One of which some
        // still are goes on: what the others come to is not taken.
        if let Some(making) = &self.making {
            let still = |one: &One| self.wanted.get(&one.track).is_some_and(|wanted| wanted.print == one.print);
            if !making.tracks.iter().any(still) {
                making.watch.stop();
            }
        }
        if self.making.is_none() {
            self.begin(session);
        }
        said
    }

    /// What is wanted of this program now: a rendition of each track that
    /// has a device someone lacks, where this program has every device it
    /// takes and nobody with a lower number has, and the track has none of
    /// the song as it is.
    fn look(
        &mut self,
        session: &SharedSession,
        me: &Person,
        others: &[(u64, Vec<DeviceId>)],
        lacked: &HashSet<DeviceId>,
    ) {
        let (wanted, mine, prints, has) = {
            let session = session.lock();
            // (Working out what a recording depends on writes its track
            // out: not while nothing has changed.)
            let seen = (session.revision(), others.to_vec());
            if self.seen.as_ref() == Some(&seen) {
                return;
            }
            self.seen = Some(seen);
            let wanted = session.renditions_wanted(lacked);
            let mine: HashSet<DeviceId> = session.lacking().into_iter().collect();
            let of: Vec<(TrackId, DeviceId)> = wanted.iter().map(|wanted| (wanted.track, wanted.after)).collect();
            let prints = session.rendition_prints(&of);
            let has: Vec<Option<(DeviceId, String)>> = (wanted.iter())
                .map(|wanted| {
                    let track = session.project().track(wanted.track).ok()?;
                    track.rendition.as_ref().map(|found| (found.after, found.of.clone()))
                })
                .collect();
            (wanted, mine, prints, has)
        };
        let now = Instant::now();
        let mut still = HashSet::new();
        for ((want, print), has) in wanted.into_iter().zip(prints).zip(has) {
            let Some(print) = print else { continue };
            if want.takes.iter().any(|device| mine.contains(device)) {
                continue;
            }
            let can = |lacks: &[DeviceId]| !want.takes.iter().any(|device| lacks.contains(device));
            if others.iter().any(|(number, lacks)| *number < me.id && can(lacks)) {
                continue;
            }
            // The track has one of the song as it is: nothing to make.
            if has.is_some_and(|(after, of)| after == want.after && of == print) {
                continue;
            }
            if self.failed.get(&want.track) == Some(&print) {
                continue;
            }
            still.insert(want.track);
            match self.wanted.get(&want.track) {
                Some(held) if held.after == want.after && held.print == print => {}
                _ => {
                    let (after, devices, stretch) = (want.after, want.devices, want.stretch);
                    self.wanted.insert(want.track, Wanted { after, devices, stretch, print, since: now });
                }
            }
        }
        self.wanted.retain(|track, _| still.contains(track));
    }

    /// Begins a pass, where anything has stood still long enough to be
    /// recorded: of every track that has, together; or of one alone, where
    /// that one was in a pass that came to nothing. The song is taken as
    /// it is this moment, and recorded away from here.
    fn begin(&mut self, session: &SharedSession) {
        let settled = |(_, wanted): &(&TrackId, &Wanted)| wanted.since.elapsed() >= SETTLED;
        let mut ready: Vec<TrackId> = self.wanted.iter().filter(settled).map(|(track, _)| *track).collect();
        ready.sort_by_key(|track| (self.wanted[track].since, *track));
        if let Some(alone) = ready.iter().copied().find(|track| self.alone.contains(track)) {
            ready = vec![alone];
        }
        if ready.is_empty() {
            return;
        }
        self.begun += 1;
        let number = self.begun;
        let (job, tracks, folder) = {
            let mut session = session.lock();
            let Some(folder) = session.received_in() else { return };
            // (What the song's plugins have come to is read first: the
            // song that is recorded is the song the others have.)
            let job = session.render_job();
            let of: Vec<(TrackId, DeviceId)> = ready.iter().map(|track| (*track, self.wanted[track].after)).collect();
            let prints = session.rendition_prints(&of);
            let making = folder.with_extension(MAKING);
            let mut tracks = Vec::new();
            for (track, print) in ready.iter().zip(prints) {
                let (Some(print), Some(wanted)) = (print, self.wanted.get_mut(track)) else { continue };
                // It is made of the song as it was taken, which is what
                // is wanted from here on.
                wanted.print.clone_from(&print);
                let name = session.project().track(*track).map(|track| track.name.clone()).unwrap_or_default();
                let part = making.join(format!("{} {number} {}.wav", std::process::id(), track.0));
                let (after, devices, from) = (wanted.after, wanted.devices, wanted.stretch.0);
                tracks.push(One { track: *track, after, devices, from, print, name, part });
            }
            (job, tracks, folder)
        };
        if tracks.is_empty() {
            return;
        }
        clear_what_was_left(&folder.with_extension(MAKING));
        let stretches = tracks.iter().map(|one| self.wanted[&one.track].stretch);
        let (from, to) =
            stretches.fold((f64::INFINITY, 0.0f64), |(from, to), (begins, ends)| (from.min(begins), to.max(ends)));
        let watch = Watch::new();
        let options = RenderOptions {
            sample_rate: RATE,
            start_beat: from,
            end_beat: Some(to),
            only_tracks: Some(tracks.iter().map(|one| one.track).collect()),
            after: (tracks.iter())
                .map(|one| After {
                    track: one.track,
                    devices: one.devices,
                    from_beat: one.from,
                    file: one.part.clone(),
                })
                .collect(),
            watch: Some(watch.clone()),
            ..RenderOptions::default()
        };
        let (send, watching, recorded) = (self.made.0.clone(), watch.clone(), tracks.clone());
        let recording = std::thread::Builder::new().name("trill-rendition".into()).spawn(move || {
            let rendered = job.measure(&options).map_err(|error| match error {
                RenderError::Stopped => STOPPED.to_owned(),
                other => sentence(&other.to_string()),
            });
            let tracks = (recorded.into_iter())
                .map(|one| {
                    let came = match &rendered {
                        Ok(_) => named(&one, &folder, &watching),
                        Err(why) => Err(why.clone()),
                    };
                    if came.is_err() {
                        let _ = std::fs::remove_file(&one.part);
                    }
                    (one, came)
                })
                .collect();
            let _ = send.send(Made { number, tracks });
        });
        match recording {
            Ok(_) => self.making = Some(Making { number, tracks, watch }),
            Err(error) => log::warn!("no thread could be started to record tracks for the others: {error}"),
        }
    }

    /// What a pass made is taken in, where it is of the song as it is.
    fn took(&mut self, session: &SharedSession, me: &Person, made: Made, said: &mut Vec<String>) {
        let together = made.tracks.len() > 1;
        for (one, came) in made.tracks {
            let One { track, after, from, print, name, .. } = one;
            match came {
                Ok(is) => {
                    let mut session = session.lock();
                    let now = session.rendition_prints(&[(track, after)]).pop().flatten();
                    let home = session.home_of(&is);
                    let taken = match now.as_deref() == Some(print.as_str()) {
                        true => {
                            let by = me.name.clone();
                            session.land_rendition(track, Rendition { after, of: print, is, by, from })
                        }
                        false => Err("the song has changed since".to_owned()),
                    };
                    drop(session);
                    if let Err(why) = taken {
                        // Of the song as it was: nobody's, and made again.
                        log::info!("a recording of '{name}' for the others was not taken: {why}");
                        if let Some(home) = home {
                            let _ = std::fs::remove_file(home);
                        }
                    }
                    self.wanted.remove(&track);
                    self.alone.remove(&track);
                }
                Err(why) if why == STOPPED => {}
                // A pass of several came to nothing: each of them alone,
                // so that the one that cannot be recorded is found.
                Err(_) if together => drop(self.alone.insert(track)),
                Err(why) => {
                    self.failed.insert(track, print);
                    self.wanted.remove(&track);
                    self.alone.remove(&track);
                    said.push(format!(
                        "'{name}' could not be recorded for the others, who have not got all its devices: {why} \
                         They hear it as it was, or not at all, until something about it is changed."
                    ));
                }
            }
        }
        // (What was taken in changed the song: what is wanted is looked
        // at again.)
        self.seen = None;
    }
}

/// Says what the recording of one track of a pass is, and gives it the
/// name of that in `folder`.
fn named(one: &One, folder: &Path, watch: &Watch) -> Result<FileIs, String> {
    let read = content_of(&one.part, |_| !watch.stopped()).map_err(|error| sentence(&error.to_string()))?;
    let Some(content) = read else { return Err(STOPPED.to_owned()) };
    let is = FileIs::new(content, &format!("{} rendition.wav", one.name));
    let home: PathBuf = folder.join(is.kept_as());
    // (Where nothing has arrived for the song yet, its folder is not there.)
    std::fs::create_dir_all(folder).map_err(|error| sentence(&error.to_string()))?;
    // (The same bytes under that name are the same recording.)
    let _ = std::fs::remove_file(&home);
    std::fs::rename(&one.part, &home).map_err(|error| sentence(&error.to_string()))?;
    Ok(is)
}

/// Clears away what a program that is gone left in the folder recordings
/// are made in: whatever has lain there for an hour.
fn clear_what_was_left(making: &Path) {
    let Ok(entries) = std::fs::read_dir(making) else { return };
    for entry in entries.flatten() {
        let old = entry.metadata().and_then(|found| found.modified()).ok().and_then(|at| at.elapsed().ok());
        if old.is_some_and(|old| old > LEFT_BEHIND) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A reason as a sentence of its own.
fn sentence(why: &str) -> String {
    let why = why.trim();
    match why.ends_with(['.', '!', '?']) {
        true => why.to_owned(),
        false => format!("{why}."),
    }
}
