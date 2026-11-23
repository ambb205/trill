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
//! seconds, one at a time, and one that is being made of a song that has
//! changed since is stopped. The others hear the track as it was
//! meanwhile, which they are told.
//!
//! **It is a render like any other**, away from the session: of a song
//! with plugins in a process of its own (`trill_engine::apart`), so that a
//! plugin that faults there costs a sentence. The recording is written
//! into the song's own folder of the program's, hashed, and given the
//! name of what it is there, where what arrives from others lies too.
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
use trill_engine::{After, RenderJob, RenderOptions, SharedSession, Watch};
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

/// The one that is being made.
struct Making {
    track: TrackId,
    print: String,
    /// The track's name, for whoever is shown what the program is doing.
    name: String,
    watch: Arc<Watch>,
}

/// What came of making one.
struct Made {
    track: TrackId,
    after: DeviceId,
    /// Where in the song the recording begins, in beats.
    from: f64,
    print: String,
    name: String,
    came: Result<FileIs, String>,
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
    looked: Instant,
    /// Counts the recordings begun, for their names while they are made.
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
            looked: Instant::now(),
            begun: 0,
            seen: None,
        }
    }

    /// The track that is being recorded for the others, by its name.
    pub fn making(&self) -> Option<&str> {
        self.making.as_ref().map(|making| making.name.as_str())
    }

    /// Nothing is made any more: nobody else holds the song, or it is
    /// another song. One that is being made is stopped.
    pub fn stop(&mut self) {
        if let Some(making) = self.making.take() {
            making.watch.stop();
        }
        self.wanted.clear();
        self.failed.clear();
        self.seen = None;
    }

    /// The song is no longer worked in with anyone: nobody else lacks
    /// anything, and nothing is made.
    pub fn leave(&mut self, session: &SharedSession) {
        self.stop();
        session.lock().others_lack(&HashSet::new());
    }

    /// Looks after the renditions: takes in what was made, and begins the
    /// next one where one is wanted of this program. `others` are the
    /// people who hold the song with this one, each by their number with
    /// the devices their machine has not got. Returns what there is to
    /// say to the user.
    pub fn tend(&mut self, session: &SharedSession, me: &Person, others: &[(u64, Vec<DeviceId>)]) -> Vec<String> {
        let mut said = Vec::new();
        while let Ok(made) = self.made.1.try_recv() {
            if self.making.as_ref().is_some_and(|making| making.track == made.track && making.print == made.print) {
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
        // One that is being made of the song as it was, or that is no
        // longer this program's to make, is stopped: what comes of it
        // then is nothing.
        if let Some(making) = &self.making
            && self.wanted.get(&making.track).is_none_or(|wanted| wanted.print != making.print)
        {
            making.watch.stop();
        }
        if self.making.is_none() {
            let ready = self.wanted.iter().filter(|(_, wanted)| wanted.since.elapsed() >= SETTLED);
            let next = ready.min_by_key(|(track, wanted)| (wanted.since, **track)).map(|(track, _)| *track);
            if let Some(track) = next {
                self.begin(session, track);
            }
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

    /// Begins to record `track`: the song is taken as it is this moment,
    /// and recorded away from here.
    fn begin(&mut self, session: &SharedSession, track: TrackId) {
        let Some(wanted) = self.wanted.get_mut(&track) else { return };
        let (after, devices, (from, to)) = (wanted.after, wanted.devices, wanted.stretch);
        let (job, print, folder, name) = {
            let mut session = session.lock();
            let Some(folder) = session.received_in() else { return };
            // (What the song's plugins have come to is read first: the
            // song that is recorded is the song the others have.)
            let job = session.render_job();
            let print = session.rendition_prints(&[(track, after)]).pop().flatten();
            let name = session.project().track(track).map(|track| track.name.clone()).unwrap_or_default();
            (job, print, folder, name)
        };
        let Some(print) = print else {
            self.wanted.remove(&track);
            return;
        };
        // It is made of the song as it was taken, which is what is wanted
        // from here on.
        wanted.print.clone_from(&print);
        let watch = Watch::new();
        self.begun += 1;
        let making = folder.with_extension(MAKING);
        clear_what_was_left(&making);
        let part = making.join(format!("{} {}.wav", std::process::id(), self.begun));
        let options = RenderOptions {
            sample_rate: RATE,
            start_beat: from,
            end_beat: Some(to),
            only_tracks: Some(vec![track]),
            after: Some(After { track, devices, file: part.clone() }),
            watch: Some(watch.clone()),
            ..RenderOptions::default()
        };
        let (send, watching) = (self.made.0.clone(), watch.clone());
        let (made_of, called) = (print.clone(), name.clone());
        let recording = std::thread::Builder::new().name("trill-rendition".into()).spawn(move || {
            let came = record(&job, &options, &part, &folder, &called, &watching);
            if came.is_err() {
                let _ = std::fs::remove_file(&part);
            }
            let _ = send.send(Made { track, after, from, print: made_of, name: called, came });
        });
        match recording {
            Ok(_) => self.making = Some(Making { track, print, name, watch }),
            Err(error) => log::warn!("no thread could be started to record a track for the others: {error}"),
        }
    }

    /// What was made is taken in, where it is of the song as it is.
    fn took(&mut self, session: &SharedSession, me: &Person, made: Made, said: &mut Vec<String>) {
        let Made { track, after, from, print, name, came } = made;
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
            }
            Err(why) => {
                self.failed.insert(track, print);
                self.wanted.remove(&track);
                said.push(format!(
                    "'{name}' could not be recorded for the others, who have not got all its devices: {why} They \
                     hear it as it was, or not at all, until something about it is changed."
                ));
            }
        }
    }
}

/// Records what `options` say, into `part`, says what the recording is
/// and gives it the name of that in `folder`.
fn record(
    job: &RenderJob,
    options: &RenderOptions,
    part: &Path,
    folder: &Path,
    name: &str,
    watch: &Watch,
) -> Result<FileIs, String> {
    job.measure(options).map_err(|error| match error {
        RenderError::Stopped => STOPPED.to_owned(),
        other => sentence(&other.to_string()),
    })?;
    let read = content_of(part, |_| !watch.stopped()).map_err(|error| sentence(&error.to_string()))?;
    let Some(content) = read else { return Err(STOPPED.to_owned()) };
    let is = FileIs::new(content, &format!("{name} rendition.wav"));
    let home: PathBuf = folder.join(is.kept_as());
    // (Where nothing has arrived for the song yet, its folder is not there.)
    std::fs::create_dir_all(folder).map_err(|error| sentence(&error.to_string()))?;
    // (The same bytes under that name are the same recording.)
    let _ = std::fs::remove_file(&home);
    std::fs::rename(part, &home).map_err(|error| sentence(&error.to_string()))?;
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
