//! What a program knows of a newer Trill, and what it does about one.
//!
//! Three steps, each asked for and none taken by itself:
//!
//! - **Looking** ([`Updates::look`]) fetches the announcement of the
//!   newest release, a small text, and takes it only under its makers'
//!   signature ([`crate::release::read`]). Nothing of this program or its
//!   user is sent: the request is for one file that is the same for
//!   everyone. What was read is kept, so that the next run knows it
//!   without asking.
//! - **Fetching** ([`Updates::fetch`]) brings the build for this kind of
//!   machine, holds it to the size and checksum that were announced, and
//!   stages it where Trill is installed ([`Place::stage`]). It can be
//!   stopped, and goes on from what had come.
//! - **Putting it in place** is not done here, and never under a program
//!   that runs: the window does it when it next starts and no Trill runs
//!   ([`crate::start`]).
//!
//! A program that is not a release (a build of one's own), that knows no
//! key, or that was not told to fetch, does none of it and says why
//! ([`Status::off`]).

use crate::place::Place;
use crate::release::{self, PublicKey, Release};
use crate::{This, Version};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use trill_fetch::{Ended, Fetcher, checksum, disk_free, said, write_json};

/// Where the announcement of the newest release of Trill is published.
pub const ADDRESS: &str = "https://github.com/ambb205/trill/releases/latest/download/trill-release.json";
/// The page that has every release, for whoever is sent there.
pub const PAGE: &str = "https://github.com/ambb205/trill/releases";
/// Names another place to look, to a build that was made for a trial
/// ([`release::made_for_a_trial`]); a release does not read it.
pub const TRIAL_ADDRESS: &str = "TRILL_UPDATE_ADDRESS";
/// The last announcement that was read, kept between runs.
const KNOWN: &str = "known.json";

/// What a program is, and where it looks and keeps things.
pub struct Options {
    pub this: This,
    /// The keys an announcement is taken under.
    pub trusted: Vec<PublicKey>,
    /// Where the announcement of the newest release is published.
    pub address: String,
    /// The kind of machine a build has to be for.
    pub runs_on: String,
    /// Where this program is installed, if it can be updated there.
    pub place: Option<Place>,
    /// A folder for what is kept between runs: the last announcement, and
    /// a build while it is fetched. Without one nothing is remembered.
    pub kept: Option<PathBuf>,
}

impl Options {
    /// What the program that is running is: its version and keys, the
    /// place releases are announced, the folder it lies in.
    pub fn of_this_program(kept: Option<PathBuf>) -> Self {
        let this = crate::this();
        let trial = release::made_for_a_trial().then(|| std::env::var(TRIAL_ADDRESS).ok()).flatten();
        Self {
            // A build of one's own lies in a folder of build products, which is nobody's to update.
            place: this.is_release().then(Place::of_this_program).flatten(),
            this,
            trusted: release::trusted(),
            address: trial.unwrap_or_else(|| ADDRESS.to_owned()),
            runs_on: release::runs_on(),
            kept,
        }
    }
}

/// How far an update is on its way into this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Step {
    /// Nothing of it is here, or only what a fetch that was stopped left.
    #[default]
    Idle,
    /// It is being fetched: so many bytes of so many.
    Fetching { got: u64, of: u64 },
    /// It is staged, and goes in the next time the window starts and no
    /// Trill runs.
    Ready(Version),
}

/// How a fetch ended, where it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Came {
    /// The update is staged.
    Ready(Version),
    /// It was stopped, and what had come is kept to go on from.
    Stopped,
}

/// What a program knows of a newer Trill, at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub this: This,
    /// Why this program does not look for a newer one, where it does not.
    pub off: Option<String>,
    pub looking: bool,
    /// When an announcement was last read.
    pub looked: Option<SystemTime>,
    /// The release that is newer than this program, as far as is known.
    pub newer: Option<Release>,
    pub step: Step,
    /// What went wrong the last time something was tried, until something
    /// goes right.
    pub problem: Option<String>,
}

impl Status {
    /// The version there is to tell the user of, where there is one.
    pub fn newer_version(&self) -> Option<Version> {
        match (&self.step, &self.newer) {
            (Step::Ready(version), _) => Some(*version),
            (_, Some(release)) => Some(release.version),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Held {
    looking: bool,
    looked: Option<SystemTime>,
    newer: Option<Release>,
    step: Step,
    problem: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Known {
    /// The announcement as it was fetched: read again under the same
    /// keys, so that nothing is taken from this file that was not signed.
    text: String,
    /// When, in seconds since 1970.
    looked: u64,
}

struct Inner {
    options: Options,
    fetcher: Mutex<Option<Arc<dyn Fetcher>>>,
    held: Mutex<Held>,
    stop: AtomicBool,
    changes: AtomicU64,
}

/// What a program knows of a newer Trill and does about one. Cheap to
/// hand about: every copy is the same one.
#[derive(Clone)]
pub struct Updates(Arc<Inner>);

fn folder_for_now() -> PathBuf {
    std::env::temp_dir().join(format!("trill-update-{}", std::process::id()))
}

impl Updates {
    /// Reads what an earlier run learned, and whether an update is staged.
    /// Asks nothing of a network.
    pub fn new(options: Options) -> Self {
        let mut held = Held::default();
        let known = options.kept.as_ref().and_then(|kept| read_known(&kept.join(KNOWN), &options.trusted));
        if let Some((release, looked)) = known {
            held.looked = Some(looked);
            held.newer = (release.version > options.this.version).then_some(release);
        }
        let staged = options.place.as_ref().and_then(Place::staged);
        if let Some(staged) = staged.filter(|staged| staged.version > options.this.version) {
            held.step = Step::Ready(staged.version);
        }
        Self(Arc::new(Inner {
            options,
            fetcher: Mutex::new(None),
            held: Mutex::new(held),
            stop: AtomicBool::new(false),
            changes: AtomicU64::new(0),
        }))
    }

    /// Says who fetches. Until it is said, this program looks for nothing.
    pub fn fetch_by(&self, fetcher: Arc<dyn Fetcher>) {
        *self.0.fetcher.lock() = Some(fetcher);
        self.changed();
    }

    fn changed(&self) {
        self.0.changes.fetch_add(1, Ordering::Relaxed);
    }

    /// A number that moves whenever [`Updates::status`] may say something
    /// else: for whoever shows it, to read it again only then.
    pub fn changes(&self) -> u64 {
        self.0.changes.load(Ordering::Relaxed)
    }

    /// Why this program does not look for a newer one, where it does not.
    pub fn off(&self) -> Option<String> {
        let options = &self.0.options;
        if !options.this.is_release() {
            return Some(
                "this Trill is a build of your own and not a release, so it does not look for a newer one".to_owned(),
            );
        }
        if options.trusted.is_empty() {
            return Some(
                "this build of Trill knows no key that a release is announced under, so it does not look for a newer one"
                    .to_owned(),
            );
        }
        if self.0.fetcher.lock().is_none() {
            return Some("this run of Trill fetches nothing, so it does not look for a newer one".to_owned());
        }
        None
    }

    pub fn status(&self) -> Status {
        let off = self.off();
        let held = self.0.held.lock();
        Status {
            this: self.0.options.this.clone(),
            off,
            looking: held.looking,
            looked: held.looked,
            newer: held.newer.clone(),
            step: held.step.clone(),
            problem: held.problem.clone(),
        }
    }

    fn fetcher(&self) -> Result<Arc<dyn Fetcher>, String> {
        match self.off() {
            Some(why) => Err(why),
            None => self.0.fetcher.lock().clone().ok_or_else(|| "nobody fetches".to_owned()),
        }
    }

    fn folder(&self) -> PathBuf {
        self.0.options.kept.clone().unwrap_or_else(folder_for_now)
    }

    /// Looks whether a newer Trill is out, and answers with it where one
    /// is. Waits for the network: not for the thread that draws.
    pub fn look(&self) -> Result<Option<Release>, String> {
        let fetcher = self.fetcher()?;
        if std::mem::replace(&mut self.0.held.lock().looking, true) {
            return Err("Trill is looking for a newer version already".to_owned());
        }
        self.changed();
        let read = self.read_announcement(&*fetcher);
        let mut held = self.0.held.lock();
        held.looking = false;
        let answer = match read {
            Ok((release, text)) => {
                let now = SystemTime::now();
                held.looked = Some(now);
                held.problem = None;
                held.newer = (release.version > self.0.options.this.version).then_some(release);
                if let Some(kept) = &self.0.options.kept {
                    let looked = now.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
                    if let Err(why) = write_json(&kept.join(KNOWN), &Known { text, looked }) {
                        log::warn!("what was read of a newer Trill is not kept: {why}");
                    }
                }
                Ok(held.newer.clone())
            }
            Err(why) => {
                held.problem = Some(why.clone());
                Err(why)
            }
        };
        drop(held);
        self.changed();
        answer
    }

    /// Fetches the announcement and reads it under this program's keys.
    fn read_announcement(&self, fetcher: &dyn Fetcher) -> Result<(Release, String), String> {
        let folder = self.folder();
        std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        let to = folder.join("announcement.fetched");
        // Never gone on with: an announcement is small, and the one of yesterday is another.
        let _ = std::fs::remove_file(&to);
        let too_long = AtomicBool::new(false);
        let ended = fetcher.fetch(&self.0.options.address, &to, &too_long, &mut |bytes| {
            if bytes > release::LONGEST as u64 {
                too_long.store(true, Ordering::Relaxed);
            }
        });
        let length = std::fs::metadata(&to).map_or(0, |metadata| metadata.len());
        let text = match ended {
            Ok(()) if length <= release::LONGEST as u64 => std::fs::read_to_string(&to)
                .map_err(|_| "what was fetched is not the announcement of a release".to_owned()),
            Ok(()) | Err(Ended::Stopped) => {
                Err("what was fetched is too long to be the announcement of a release".to_owned())
            }
            Err(Ended::Failed(why)) => Err(format!("Trill could not look for a newer version ({why})")),
        };
        let _ = std::fs::remove_file(&to);
        if self.0.options.kept.is_none() {
            let _ = std::fs::remove_dir(&folder);
        }
        let text = text?;
        Ok((release::read(&text, &self.0.options.trusted)?, text))
    }

    /// Looks on a thread of its own, and says nothing: what it found is in
    /// [`Updates::status`].
    pub fn look_soon(&self) {
        let updates = self.clone();
        let spawned = std::thread::Builder::new().name("trill-update".into()).spawn(move || {
            if let Err(why) = updates.look() {
                log::info!("{why}");
            }
        });
        if let Err(error) = spawned {
            log::warn!("Trill did not look for a newer version: a thread could not be started ({error})");
        }
    }

    /// Looks on a thread of its own where this program looks at all and
    /// has not read an announcement within `every`.
    pub fn look_if_due(&self, every: Duration) {
        if self.off().is_some() {
            return;
        }
        let looked = self.0.held.lock().looked;
        let due = looked.is_none_or(|looked| looked.elapsed().is_ok_and(|since| since >= every));
        if due {
            self.look_soon();
        }
    }

    fn step(&self, step: Step) {
        self.0.held.lock().step = step;
        self.changed();
    }

    /// Fetches the newer Trill that is known and stages it where this one
    /// is installed. Waits for all of it: not for the thread that draws.
    pub fn fetch(&self) -> Result<Came, String> {
        let fetcher = self.fetcher()?;
        let options = &self.0.options;
        let (release, step) = {
            let held = self.0.held.lock();
            (held.newer.clone(), held.step.clone())
        };
        let Some(release) = release else {
            return Err("no newer Trill is known of: look for one first".to_owned());
        };
        let version = release.version;
        match step {
            Step::Fetching { .. } => return Err(format!("Trill {version} is being fetched already")),
            Step::Ready(ready) if ready == version => return Ok(Came::Ready(version)),
            _ => {}
        }
        let elsewhere = if release.page.is_empty() { PAGE } else { &release.page };
        let Some(build) = release.build_for(&options.runs_on) else {
            return Err(format!(
                "Trill {version} has no build for this kind of machine ({}): {elsewhere} says what there is",
                options.runs_on
            ));
        };
        let Some(place) = &options.place else {
            return Err(format!(
                "this Trill cannot be updated where it lies: the installer of {version} is at {elsewhere}"
            ));
        };
        place.writable()?;
        let folder = self.folder();
        std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        let short = |needs: u64, on: &Path| {
            disk_free(on).filter(|free| *free < needs).map(|free| {
                format!(
                    "Trill {version} needs {} on the disk of {} and {} are free there: make room and fetch it again",
                    said(needs),
                    on.display(),
                    said(free)
                )
            })
        };
        if let Some(why) = short(build.size, &folder).or_else(|| short(build.unpacked(), place.folder())) {
            return Err(why);
        }

        self.0.stop.store(false, Ordering::Relaxed);
        self.step(Step::Fetching { got: 0, of: build.size });
        let archive = folder.join(format!("trill-{version}-{}.zip", &build.sha256[..12]));
        clear_other_archives(&folder, &archive);
        let stop = &self.0.stop;
        let ended = (|| {
            // More than the whole of it is something else.
            if std::fs::metadata(&archive).is_ok_and(|metadata| metadata.len() > build.size) {
                let _ = std::fs::remove_file(&archive);
            }
            fetcher.fetch(&build.url, &archive, stop, &mut |got| self.step(Step::Fetching { got, of: build.size }))?;
            if checksum(&archive, stop)? != build.sha256 {
                let _ = std::fs::remove_file(&archive);
                return Err(Ended::Failed(format!(
                    "what was fetched is not Trill {version} as it was announced: nothing of it is kept, fetch it again"
                )));
            }
            place.stage(version, build, &archive, stop, &mut |_| {})
        })();
        let mut held = self.0.held.lock();
        let answer = match ended {
            Ok(_) => {
                let _ = std::fs::remove_file(&archive);
                held.step = Step::Ready(version);
                held.problem = None;
                Ok(Came::Ready(version))
            }
            Err(Ended::Stopped) => {
                held.step = Step::Idle;
                Ok(Came::Stopped)
            }
            Err(Ended::Failed(why)) => {
                held.step = Step::Idle;
                let why = format!("Trill {version} could not be fetched ({why})");
                held.problem = Some(why.clone());
                Err(why)
            }
        };
        drop(held);
        self.changed();
        answer
    }

    /// Stops a fetch that is going. What has come is kept.
    pub fn stop(&self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }

    /// Puts away the update that is staged: Trill stays as it is until it
    /// is fetched again.
    pub fn forget(&self) {
        if let Some(place) = &self.0.options.place {
            place.unstage();
        }
        let mut held = self.0.held.lock();
        if matches!(held.step, Step::Ready(_)) {
            held.step = Step::Idle;
        }
        drop(held);
        self.changed();
    }
}

/// Removes what a fetch of another version left in `folder`.
fn clear_other_archives(folder: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_one =
            entry.file_name().to_str().is_some_and(|name| name.starts_with("trill-") && name.ends_with(".zip"));
        if is_one && path != keep {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn read_known(path: &Path, trusted: &[PublicKey]) -> Option<(Release, SystemTime)> {
    let known: Known = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let release = release::read(&known.text, trusted).ok()?;
    Some((release, UNIX_EPOCH + Duration::from_secs(known.looked)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::{Build, Piece, SecretKey, announce};
    use trill_fetch::scripted::Shelf;

    const ADDRESS: &str = "https://shelf.test/trill-release.json";

    fn sum_of(bytes: &[u8]) -> String {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("x");
        std::fs::write(&path, bytes).unwrap();
        checksum(&path, &AtomicBool::new(false)).unwrap()
    }

    /// A machine with a Trill installed on it, a shelf releases are
    /// published on, and the key they are announced under.
    struct World {
        home: tempfile::TempDir,
        kept: tempfile::TempDir,
        shelf: Arc<Shelf>,
        key: SecretKey,
    }

    impl World {
        fn new() -> Self {
            let world = Self {
                home: tempfile::tempdir().unwrap(),
                kept: tempfile::tempdir().unwrap(),
                shelf: Arc::new(Shelf::default()),
                key: SecretKey::make().unwrap(),
            };
            std::fs::write(world.home.path().join("trill.exe"), "window 0.2.0").unwrap();
            world
        }

        fn options(&self, version: &str) -> Options {
            Options {
                this: This { version: version.parse().unwrap(), commit: Some("abc1234".to_owned()) },
                trusted: vec![self.key.public()],
                address: ADDRESS.to_owned(),
                runs_on: "windows-x86_64".to_owned(),
                place: Some(Place::at(self.home.path())),
                kept: Some(self.kept.path().to_owned()),
            }
        }

        /// A Trill of `version` that runs here and fetches from the shelf.
        fn trill(&self, version: &str) -> Updates {
            let updates = Updates::new(self.options(version));
            updates.fetch_by(self.shelf.clone());
            updates
        }

        /// Publishes `version`: its archive, and the announcement of it.
        fn publish(&self, version: &str) -> Release {
            let window = format!("window {version}").into_bytes();
            let archive = trill_fetch::zip::archive_of(&[("trill.exe", &window, true)]);
            let url = format!("https://shelf.test/v{version}/trill.zip");
            let release = Release {
                version: version.parse().unwrap(),
                date: "2026-11-22".to_owned(),
                notes: format!("What is new in {version}."),
                page: format!("https://shelf.test/v{version}"),
                builds: vec![Build {
                    runs_on: "windows-x86_64".to_owned(),
                    url: url.clone(),
                    sha256: sum_of(&archive),
                    size: archive.len() as u64,
                    files: vec![Piece {
                        path: "trill.exe".to_owned(),
                        sha256: sum_of(&window),
                        size: window.len() as u64,
                    }],
                }],
            };
            self.shelf.put(&url, archive);
            self.shelf.put(ADDRESS, announce(&release, &self.key).unwrap().into_bytes());
            release
        }
    }

    #[test]
    fn a_newer_release_is_found_and_one_that_is_not_newer_is_nothing() {
        let world = World::new();
        let published = world.publish("0.3.0");
        let trill = world.trill("0.2.0");
        assert_eq!(trill.status().newer, None, "nothing is known before it looked");
        assert_eq!(trill.look().unwrap(), Some(published.clone()));
        let status = trill.status();
        assert_eq!((status.newer, status.problem, status.looking), (Some(published), None, false));
        assert!(status.looked.is_some());
        assert_eq!(status.step, Step::Idle, "looking fetches nothing but the announcement");
        assert_eq!(world.shelf.asked(), [ADDRESS]);
        // To the release itself there is nothing newer, and an older announcement shown again is nothing either.
        assert_eq!(world.trill("0.3.0").look().unwrap(), None);
        assert_eq!(world.trill("0.4.0").look().unwrap(), None);
    }

    #[test]
    fn what_was_read_is_known_at_the_next_start_without_asking() {
        let world = World::new();
        let published = world.publish("0.3.0");
        world.trill("0.2.0").look().unwrap();
        let asked = world.shelf.asked().len();
        let again = world.trill("0.2.0");
        assert_eq!(again.status().newer, Some(published));
        assert_eq!(world.shelf.asked().len(), asked, "a start asks nothing of the network");
        // What is kept is read under the keys again: a file somebody wrote there is nothing.
        let other = SecretKey::make().unwrap();
        let forged = announce(&world.publish("9.0.0"), &other).unwrap();
        write_json(&world.kept.path().join(KNOWN), &Known { text: forged, looked: 1 }).unwrap();
        assert_eq!(world.trill("0.2.0").status().newer, None);
    }

    #[test]
    fn an_announcement_nobody_of_ours_signed_is_a_sentence_and_nothing_is_taken() {
        let world = World::new();
        world.publish("0.3.0");
        let trill = world.trill("0.2.0");
        let stranger = SecretKey::make().unwrap();
        let theirs = announce(&world.publish("9.0.0"), &stranger).unwrap();
        world.shelf.put(ADDRESS, theirs.into_bytes());
        let why = trill.look().unwrap_err();
        assert!(why.contains("not signed by the makers"), "{why}");
        let status = trill.status();
        assert_eq!(status.newer, None);
        assert_eq!(status.problem, Some(why));
        assert!(!world.kept.path().join(KNOWN).exists(), "and nothing of it is kept");
        // A page in place of an announcement, and nothing at all.
        world.shelf.put(ADDRESS, b"<html>Sign in</html>".to_vec());
        assert!(trill.look().unwrap_err().contains("not the announcement"));
        world.shelf.put(ADDRESS, vec![b' '; release::LONGEST + 100_000]);
        assert!(trill.look().unwrap_err().contains("too long"));
        world.shelf.take(ADDRESS);
        let why = trill.look().unwrap_err();
        assert!(why.contains("could not look") && why.contains("404"), "{why}");
        // What went right clears what went wrong.
        world.publish("0.3.0");
        trill.look().unwrap();
        assert_eq!(trill.status().problem, None);
    }

    #[test]
    fn a_program_that_is_no_release_or_was_not_told_to_fetch_asks_nothing() {
        let world = World::new();
        world.publish("0.3.0");
        // A build of one's own.
        let mut own = world.options("0.2.0");
        own.this.commit = None;
        let own = Updates::new(own);
        own.fetch_by(world.shelf.clone());
        assert!(own.status().off.unwrap().contains("a build of your own"));
        assert!(own.look().unwrap_err().contains("a build of your own"));
        // One that knows no key.
        let mut keyless = world.options("0.2.0");
        keyless.trusted.clear();
        let keyless = Updates::new(keyless);
        keyless.fetch_by(world.shelf.clone());
        assert!(keyless.look().unwrap_err().contains("knows no key"));
        // One that nobody told to fetch: a test's, a run that keeps nothing.
        let quiet = Updates::new(world.options("0.2.0"));
        assert!(quiet.look().unwrap_err().contains("fetches nothing"));
        quiet.look_if_due(Duration::ZERO);
        assert!(quiet.fetch().is_err());
        assert!(world.shelf.asked().is_empty(), "none of them asked anything: {:?}", world.shelf.asked());
    }

    #[test]
    fn a_newer_release_is_fetched_checked_and_staged_where_trill_is_installed() {
        let world = World::new();
        world.publish("0.3.0");
        let trill = world.trill("0.2.0");
        assert!(trill.fetch().unwrap_err().contains("look for one first"));
        trill.look().unwrap();
        assert_eq!(trill.fetch().unwrap(), Came::Ready("0.3.0".parse().unwrap()));
        let status = trill.status();
        assert_eq!(status.step, Step::Ready("0.3.0".parse().unwrap()));
        assert_eq!(status.newer_version(), Some("0.3.0".parse().unwrap()));
        let place = Place::at(world.home.path());
        assert_eq!(place.staged().unwrap().version, "0.3.0".parse().unwrap());
        assert_eq!(std::fs::read_to_string(world.home.path().join("trill.exe")).unwrap(), "window 0.2.0");
        let left: Vec<_> = std::fs::read_dir(world.kept.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from(KNOWN)], "the archive is not kept once it is staged");
        // Asked for again, it is there already; and the next run knows that it is ready.
        let asked = world.shelf.asked().len();
        assert_eq!(trill.fetch().unwrap(), Came::Ready("0.3.0".parse().unwrap()));
        assert_eq!(world.shelf.asked().len(), asked);
        assert_eq!(world.trill("0.2.0").status().step, Step::Ready("0.3.0".parse().unwrap()));
        // Put away, Trill stays as it is.
        trill.forget();
        assert_eq!(trill.status().step, Step::Idle);
        assert_eq!(place.staged(), None);
    }

    #[test]
    fn a_build_that_is_not_what_was_announced_is_not_staged() {
        let world = World::new();
        let published = world.publish("0.3.0");
        let trill = world.trill("0.2.0");
        trill.look().unwrap();
        // Somebody put another archive where the release's was.
        let other = trill_fetch::zip::archive_of(&[("trill.exe", b"something else", true)]);
        world.shelf.put(&published.builds[0].url, other);
        let why = trill.fetch().unwrap_err();
        assert!(why.contains("not Trill 0.3.0 as it was announced"), "{why}");
        assert_eq!(trill.status().step, Step::Idle);
        assert_eq!(trill.status().problem, Some(why));
        assert_eq!(Place::at(world.home.path()).staged(), None);
        assert_eq!(std::fs::read_to_string(world.home.path().join("trill.exe")).unwrap(), "window 0.2.0");
    }

    #[test]
    fn a_fetch_that_is_stopped_or_lost_goes_on_from_what_had_come() {
        let world = World::new();
        // An archive of several pieces of a connection: a megabyte that does not pack.
        let mut noise = Vec::with_capacity(1 << 20);
        let mut state = 0x2545_f491_4f6c_dd1du64;
        while noise.len() < 1 << 20 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            noise.extend_from_slice(&state.to_le_bytes());
        }
        let archive = trill_fetch::zip::archive_of(&[("trill.exe", &noise, false)]);
        let url = "https://shelf.test/v0.3.0/trill.zip";
        let release = Release {
            version: "0.3.0".parse().unwrap(),
            date: String::new(),
            notes: String::new(),
            page: String::new(),
            builds: vec![Build {
                runs_on: "windows-x86_64".to_owned(),
                url: url.to_owned(),
                sha256: sum_of(&archive),
                size: archive.len() as u64,
                files: vec![Piece { path: "trill.exe".to_owned(), sha256: sum_of(&noise), size: noise.len() as u64 }],
            }],
        };
        world.shelf.put(url, archive);
        world.shelf.put(ADDRESS, announce(&release, &world.key).unwrap().into_bytes());
        let trill = world.trill("0.2.0");
        trill.look().unwrap();
        world.shelf.cut_after(300_000);
        let why = trill.fetch().unwrap_err();
        assert!(why.contains("could not be fetched") && why.contains("connection was lost"), "{why}");
        assert_eq!(trill.status().step, Step::Idle);
        assert_eq!(trill.fetch().unwrap(), Came::Ready("0.3.0".parse().unwrap()));
        let began = world.shelf.began();
        assert!(began.last().is_some_and(|had| *had >= 300_000), "it went on from what had come: {began:?}");
    }

    #[test]
    fn a_release_with_no_build_for_this_machine_says_where_to_look() {
        let world = World::new();
        world.publish("0.3.0");
        let mut options = world.options("0.2.0");
        options.runs_on = "linux-aarch64".to_owned();
        let trill = Updates::new(options);
        trill.fetch_by(world.shelf.clone());
        trill.look().unwrap();
        let why = trill.fetch().unwrap_err();
        assert!(why.contains("no build for this kind of machine (linux-aarch64)"), "{why}");
        assert!(why.contains("https://shelf.test/v0.3.0"), "{why}");
        // And a Trill that lies where it cannot be updated is sent to the installer.
        let mut options = world.options("0.2.0");
        options.place = None;
        let trill = Updates::new(options);
        trill.fetch_by(world.shelf.clone());
        trill.look().unwrap();
        assert!(trill.fetch().unwrap_err().contains("the installer of 0.3.0 is at https://shelf.test/v0.3.0"));
    }

    #[test]
    fn looking_by_itself_waits_until_it_is_due() {
        let world = World::new();
        world.publish("0.3.0");
        let trill = world.trill("0.2.0");
        trill.look().unwrap();
        let asked = world.shelf.asked().len();
        trill.look_if_due(Duration::from_secs(3600));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.shelf.asked().len(), asked, "it looked a moment ago");
        let changes = trill.changes();
        trill.look_if_due(Duration::ZERO);
        let began = std::time::Instant::now();
        while world.shelf.asked().len() == asked && began.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(world.shelf.asked().len(), asked + 1);
        while trill.status().looking && began.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(trill.changes() > changes, "whoever shows it hears that it changed");
    }
}
