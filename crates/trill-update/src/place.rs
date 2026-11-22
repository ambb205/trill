//! The folder Trill is installed in, and an update put into it.
//!
//! Trill starts itself a second time, by the path of its own file, to
//! scan plugins, to render and to study. A file that changed under a
//! program that is running would pair that program with children of
//! another version. So an update is never put in under anything: it is
//! *staged* (unpacked beside the programs, every file checked against what
//! was announced) by a program that runs, and *applied* by the window when
//! it next starts and finds that no Trill runs from this folder
//! ([`Place::alone`]).
//!
//! Applying is renames within one folder: each file that is there goes
//! into `old`, each new one takes its place. What was cut short (the
//! power, a kill) is mended at the next start ([`Place::mend`]); what does
//! not come up is put back ([`Place::put_back`]) by the program that
//! applied it, which is still the old one and waits to see the new one up
//! ([`Place::try_out`]).
//!
//! Everything is kept in one folder beside the programs ([`WORK`]), which
//! an installer clears and nothing else reads.

use crate::Version;
use crate::release::{Build, Piece};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};
use trill_fetch::{Ended, checksum, write_json};

/// The folder, beside the programs, that an update is kept in while it
/// waits and that what it replaced is kept in until the new one is up.
pub const WORK: &str = ".update";
/// Says that an update is staged whole: written last.
const READY: &str = "ready.json";
/// Says which files an update that is being applied moves: there only
/// while it is, so that one that was cut short is mended.
const APPLYING: &str = "applying.json";
/// Says what the last update replaced, for as long as that can be put back.
const APPLIED: &str = "applied.json";
/// What an update replaced.
const OLD: &str = "old";
/// What every Trill that runs from here holds, and what one that applies
/// an update has to have alone.
const LOCK: &str = "running.lock";
/// Held while a new version is tried out, so that nobody clears away what
/// would be put back.
const TRYING: &str = "trying.lock";
/// The file a program that is tried out writes once it is up.
const CAME_UP: &str = "came-up";
/// Names that file to the program that is tried out.
pub const TRIAL: &str = "TRILL_UPDATE_TRIAL";
/// Tells a program which version it has just taken the place of.
pub const UPDATED_FROM: &str = "TRILL_UPDATED_FROM";
/// How long a new version is given to come up before it is taken to be
/// starting slowly and left to it.
const COMES_UP_WITHIN: Duration = Duration::from_secs(120);

/// An update that is staged: unpacked, checked, and waiting for a start
/// at which no Trill runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    pub version: Version,
    pub files: Vec<Piece>,
}

/// What the last update did, for as long as it can be undone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Applied {
    to: Version,
    files: Vec<String>,
}

/// How trying a new version out ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trial {
    /// It said that it is up, or did what it was asked and ended well.
    CameUp,
    /// It is still starting after a long while, and is left to it.
    StillStarting,
    /// It ended before it was up: a sentence that says how.
    Failed(String),
}

/// The folder a Trill is installed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    folder: PathBuf,
}

/// No Trill runs from a place but the one that holds this.
pub struct Alone {
    _lock: File,
}

/// A Trill runs from a place for as long as this is held.
pub struct Running {
    _lock: Option<File>,
}

/// How trying a new version out ended. For as long as this is held,
/// nobody clears away what the update replaced: whoever puts it back does
/// so before letting go.
pub struct Tried {
    pub ended: Trial,
    _trying: Option<File>,
}

fn said(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

/// What tells one file from another that took its place: its length and
/// when it was written.
pub fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

/// The files of an update in the order they go in: programs last, so that
/// what is cut short has changed as little as can be of what runs.
fn in_order(files: &[Piece]) -> Vec<&Piece> {
    let mut ordered: Vec<&Piece> = files.iter().collect();
    ordered.sort_by_key(|piece| piece.path.ends_with(".exe"));
    ordered
}

impl Place {
    /// The folder the program that is running lies in.
    pub fn of_this_program() -> Option<Self> {
        Some(Self { folder: std::env::current_exe().ok()?.parent()?.to_owned() })
    }

    pub fn at(folder: impl Into<PathBuf>) -> Self {
        Self { folder: folder.into() }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    fn work(&self) -> PathBuf {
        self.folder.join(WORK)
    }

    fn below(&self, path: &str) -> PathBuf {
        path.split('/').fold(self.folder.clone(), |folder, piece| folder.join(piece))
    }

    fn below_work(&self, folder: &str, path: &str) -> PathBuf {
        path.split('/').fold(self.work().join(folder), |folder, piece| folder.join(piece))
    }

    /// Whether this program can write where it is installed, which an
    /// update needs: otherwise a sentence that says what to do instead.
    pub fn writable(&self) -> Result<(), String> {
        let work = self.work();
        let probe = work.join(format!("probe-{}", std::process::id()));
        let tried = std::fs::create_dir_all(&work).and_then(|()| std::fs::write(&probe, b""));
        let _ = std::fs::remove_file(&probe);
        tried.map_err(|_| {
            format!(
                "Trill cannot write to the folder it is installed in ({}), so it cannot update itself there: \
                 run the installer of the new version instead",
                self.folder.display()
            )
        })
    }

    fn lock_file(&self, name: &str) -> Option<File> {
        std::fs::create_dir_all(self.work()).ok()?;
        File::options().read(true).write(true).create(true).truncate(false).open(self.work().join(name)).ok()
    }

    /// Whether no Trill runs from here: held for as long as that has to
    /// stay so. Nothing where one does, or where the folder cannot be
    /// written to.
    pub fn alone(&self) -> Option<Alone> {
        let file = self.lock_file(LOCK)?;
        file.try_lock().ok()?;
        Some(Alone { _lock: file })
    }

    /// Says that a Trill runs from here, for as long as what is returned
    /// is held. Waits while another one is applying an update, which
    /// takes a moment.
    pub fn run(&self) -> Running {
        Running { _lock: self.lock_file(LOCK).filter(|file| file.lock_shared().is_ok()) }
    }

    /// The update that is staged here, if one is, whole.
    pub fn staged(&self) -> Option<Staged> {
        let text = std::fs::read_to_string(self.work().join(READY)).ok()?;
        let staged: Staged = serde_json::from_str(&text).ok()?;
        let folder = staged.version.to_string();
        staged.files.iter().all(|piece| self.below_work(&folder, &piece.path).is_file()).then_some(staged)
    }

    /// Forgets what is staged, and clears away what was unpacked for it.
    pub fn unstage(&self) {
        let _ = std::fs::remove_file(self.work().join(READY));
        let Ok(entries) = std::fs::read_dir(self.work()) else { return };
        for entry in entries.flatten() {
            let is_a_version = entry.file_name().to_str().is_some_and(|name| name.parse::<Version>().is_ok());
            if is_a_version && entry.path().is_dir() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Stages the build that was fetched as `archive`: unpacks its files
    /// beside the programs, holds each to the size and checksum that were
    /// announced, and only then says that an update is ready.
    pub fn stage(
        &self,
        version: Version,
        build: &Build,
        archive: &Path,
        stop: &AtomicBool,
        unpacked: &mut dyn FnMut(u64),
    ) -> Result<Staged, Ended> {
        self.writable().map_err(Ended::Failed)?;
        self.unstage();
        let folder = self.work().join(version.to_string());
        let wanted = |name: &str| build.files.iter().any(|piece| piece.path == name).then(|| name.to_owned());
        let result = (|| {
            trill_fetch::zip::unpack(archive, &folder, &wanted, stop, unpacked)?;
            for piece in &build.files {
                let path = self.below_work(&version.to_string(), &piece.path);
                let is_it = path.is_file()
                    && std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == piece.size)
                    && checksum(&path, stop)? == piece.sha256;
                if !is_it {
                    return Err(Ended::Failed(format!(
                        "{} of Trill {version} is not the file that was announced: nothing of it is kept",
                        piece.path
                    )));
                }
            }
            let staged = Staged { version, files: build.files.clone() };
            write_json(&self.work().join(READY), &staged).map_err(Ended::Failed)?;
            Ok(staged)
        })();
        if result.is_err() {
            self.unstage();
        }
        result
    }

    /// Puts the staged update in place: each file that is there goes into
    /// `old`, each new one takes its place. All of it or none: where one
    /// file cannot be moved, the ones before it are put back, and the
    /// sentence says which and why. Whoever calls this is [`Alone`].
    pub fn apply(&self, staged: &Staged, _alone: &Alone) -> Result<(), String> {
        let version = staged.version;
        let from = version.to_string();
        let never = AtomicBool::new(false);
        // Checked again right before it goes in: it has lain on a disk since.
        for piece in &staged.files {
            let path = self.below_work(&from, &piece.path);
            let is_it = checksum(&path, &never).is_ok_and(|sum| sum == piece.sha256);
            if !is_it {
                self.unstage();
                return Err(format!(
                    "{} of the update to {version} is no longer the file that was fetched: the update was put away, \
                     fetch it again",
                    piece.path
                ));
            }
        }
        let old = self.work().join(OLD);
        if old.exists() {
            std::fs::remove_dir_all(&old)
                .map_err(|error| format!("what the update before left could not be cleared away ({error})"))?;
        }
        let paths: Vec<String> = staged.files.iter().map(|piece| piece.path.clone()).collect();
        write_json(&self.work().join(APPLYING), &Applied { to: version, files: paths.clone() })?;
        let mut done: Vec<&str> = Vec::new();
        for piece in in_order(&staged.files) {
            let (target, kept, new) =
                (self.below(&piece.path), self.below_work(OLD, &piece.path), self.below_work(&from, &piece.path));
            let moved = (|| {
                if target.exists() {
                    if let Some(parent) = kept.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(&target, &kept)?;
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&new, &target)
            })();
            if let Err(error) = moved {
                if !target.exists() && kept.exists() {
                    let _ = std::fs::rename(&kept, &target);
                }
                self.undo(&done);
                let _ = std::fs::remove_file(self.work().join(APPLYING));
                self.unstage();
                return Err(format!("{} could not be put in place ({error}): Trill is as it was", piece.path));
            }
            done.push(&piece.path);
        }
        write_json(&self.work().join(APPLIED), &Applied { to: version, files: paths })?;
        let _ = std::fs::remove_file(self.work().join(APPLYING));
        self.unstage();
        Ok(())
    }

    /// Takes the new file at each of `paths` away again and puts back the
    /// one that was there, last first.
    fn undo(&self, paths: &[&str]) {
        for path in paths.iter().rev() {
            let (target, kept) = (self.below(path), self.below_work(OLD, path));
            if target.exists() && std::fs::remove_file(&target).is_err() {
                // A program that still runs cannot be removed, and can be moved.
                let aside = self.below_work("failed", path);
                if let Some(parent) = aside.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::remove_file(&aside);
                let _ = std::fs::rename(&target, &aside);
            }
            if kept.exists()
                && let Err(error) = std::fs::rename(&kept, &target)
            {
                log::error!("{path} could not be put back ({error}): it lies in {}", kept.display());
            }
        }
    }

    /// Puts back what the last update replaced, and answers with the
    /// version that was taken out again.
    pub fn put_back(&self) -> Result<Version, String> {
        let path = self.work().join(APPLIED);
        let text = std::fs::read_to_string(&path).map_err(said(&path))?;
        let applied: Applied = serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let paths: Vec<&str> = applied.files.iter().map(String::as_str).collect();
        self.undo(&paths);
        let _ = std::fs::remove_file(&path);
        Ok(applied.to)
    }

    /// Mends an update that was cut short while it was applied: every
    /// file it had moved away is put back, so that Trill is whole and as
    /// it was. True where there was something to mend.
    pub fn mend(&self, _alone: &Alone) -> bool {
        let path = self.work().join(APPLYING);
        let Ok(text) = std::fs::read_to_string(&path) else { return false };
        if let Ok(applying) = serde_json::from_str::<Applied>(&text) {
            let from = applying.to.to_string();
            // Only what the update had got to: a file whose predecessor was moved away, or a new
            // one that went in where none was. A file it had not reached yet is the old one, in
            // its place, with its successor still waiting beside it: that one is left alone.
            let reached: Vec<&str> = applying
                .files
                .iter()
                .map(String::as_str)
                .filter(|path| {
                    let moved_away = self.below_work(OLD, path).exists();
                    let went_in = !self.below_work(&from, path).exists() && self.below(path).exists();
                    moved_away || went_in
                })
                .collect();
            self.undo(&reached);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(self.work().join(APPLIED));
        self.unstage();
        true
    }

    /// Clears away what the last update replaced, once nobody would put it
    /// back: not while a new version is being tried out.
    pub fn tidy(&self, _alone: &Alone) {
        let Some(trying) = self.lock_file(TRYING) else { return };
        if trying.try_lock().is_err() {
            return;
        }
        for name in [OLD, "failed"] {
            let _ = std::fs::remove_dir_all(self.work().join(name));
        }
        let _ = std::fs::remove_file(self.work().join(APPLIED));
        // What the programs that were tried out wrote to say that they were up.
        for entry in std::fs::read_dir(self.work()).into_iter().flatten().flatten() {
            if entry.file_name().to_str().is_some_and(|name| name.starts_with(CAME_UP)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        if self.staged().is_none() {
            self.unstage();
        }
    }

    /// Starts the program that was just put in place, with what this one
    /// was started with, and waits to see it up. `from` is the version it
    /// took the place of.
    ///
    /// Whoever applied the update hands over that it is `alone`: the new
    /// program is to find that no Trill runs, as this one did. That is
    /// let go of only once it is settled that nobody clears away what was
    /// replaced, so that no start between the two finds it for the taking.
    pub fn try_out(&self, alone: Alone, program: &Path, args: &[std::ffi::OsString], from: Version) -> Tried {
        let trying = self.lock_file(TRYING).filter(|trying| trying.lock().is_ok());
        drop(alone);
        Tried { ended: self.watch_come_up(program, args, from), _trying: trying }
    }

    fn watch_come_up(&self, program: &Path, args: &[std::ffi::OsString], from: Version) -> Trial {
        // A file of this trial's own: one that a program tried out before left, or still has
        // open, is never taken for this one's word.
        let moment = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_nanos());
        let came_up = self.work().join(format!("{CAME_UP}-{}-{moment}", std::process::id()));
        let mut command = std::process::Command::new(program);
        command.args(args).env(TRIAL, &came_up).env(UPDATED_FROM, from.to_string());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return Trial::Failed(format!("it could not be started ({error})")),
        };
        let began = Instant::now();
        loop {
            if came_up.is_file() {
                return Trial::CameUp;
            }
            match child.try_wait() {
                // It did what it was asked (said its version, took a picture) and ended.
                Ok(Some(status)) if status.success() => return Trial::CameUp,
                Ok(Some(status)) => return Trial::Failed(format!("it ended before it was up ({status})")),
                Ok(None) => {}
                Err(error) => return Trial::Failed(format!("it could not be watched ({error})")),
            }
            if began.elapsed() > COMES_UP_WITHIN {
                return Trial::StillStarting;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Says that this program is up, to the one that is trying it out after
/// an update. Nothing where nobody is.
pub fn came_up() {
    if let Some(path) = std::env::var_os(TRIAL) {
        let _ = std::fs::write(path, b"up\n");
    }
}

/// The version this program has just taken the place of, where it was
/// started by an update.
pub fn updated_from() -> Option<Version> {
    std::env::var(UPDATED_FROM).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::hex;
    use std::sync::atomic::AtomicBool;

    fn sum(bytes: &[u8]) -> String {
        use std::io::Write;
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("x");
        std::fs::File::create(&path).unwrap().write_all(bytes).unwrap();
        checksum(&path, &AtomicBool::new(false)).unwrap()
    }

    /// A folder with a Trill of `version` in it: two programs and a licence.
    fn installed(folder: &Path, version: &str) {
        std::fs::create_dir_all(folder.join("licences")).unwrap();
        std::fs::write(folder.join("trill.exe"), format!("window {version}")).unwrap();
        std::fs::write(folder.join("trill-mcp.exe"), format!("server {version}")).unwrap();
        std::fs::write(folder.join("licences").join("Trill.txt"), format!("licence {version}")).unwrap();
    }

    /// An archive of a Trill of `version`, and the build that announces it.
    fn built(folder: &Path, version: &str) -> (PathBuf, Build) {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("trill.exe".into(), format!("window {version}").into_bytes()),
            ("trill-mcp.exe".into(), format!("server {version}").into_bytes()),
            ("licences/Trill.txt".into(), format!("licence {version}").into_bytes()),
            ("licences/New in this one.txt".into(), b"more".to_vec()),
        ];
        let members: Vec<(&str, &[u8], bool)> =
            files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice(), true)).collect();
        let archive = folder.join(format!("trill-{version}.zip"));
        std::fs::write(&archive, trill_fetch::zip::archive_of(&members)).unwrap();
        let build = Build {
            runs_on: "windows-x86_64".into(),
            url: "https://example.org/trill.zip".into(),
            sha256: checksum(&archive, &AtomicBool::new(false)).unwrap(),
            size: std::fs::metadata(&archive).unwrap().len(),
            files: files
                .iter()
                .map(|(name, bytes)| Piece { path: name.clone(), sha256: sum(bytes), size: bytes.len() as u64 })
                .collect(),
        };
        (archive, build)
    }

    fn read(folder: &Path, path: &str) -> String {
        std::fs::read_to_string(Place::at(folder).below(path)).unwrap_or_else(|_| "(not there)".to_owned())
    }

    fn stage(place: &Place, shelf: &Path, version: &str) -> Staged {
        let (archive, build) = built(shelf, version);
        place.stage(version.parse().unwrap(), &build, &archive, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    #[test]
    fn an_update_is_staged_beside_the_programs_and_changes_none_of_them() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        assert_eq!(place.staged(), None);
        let staged = stage(&place, shelf.path(), "0.3.0");
        assert_eq!(place.staged(), Some(staged));
        assert_eq!(read(home.path(), "trill.exe"), "window 0.2.0");
        assert_eq!(read(home.path(), "licences/Trill.txt"), "licence 0.2.0");
        place.unstage();
        assert_eq!(place.staged(), None);
        assert!(!home.path().join(WORK).join("0.3.0").exists());
    }

    #[test]
    fn an_archive_that_is_not_what_was_announced_is_not_staged() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let (archive, mut build) = built(shelf.path(), "0.3.0");
        build.files[0].sha256 = hex(&[7u8; 32]);
        let ended = place.stage("0.3.0".parse().unwrap(), &build, &archive, &AtomicBool::new(false), &mut |_| {});
        let Err(Ended::Failed(why)) = ended else { panic!("{ended:?}") };
        assert!(why.contains("trill.exe") && why.contains("not the file that was announced"), "{why}");
        assert_eq!(place.staged(), None);
        assert!(!home.path().join(WORK).join("0.3.0").exists(), "nothing of it is kept");
        // One file short is no update either.
        let (archive, mut build) = built(shelf.path(), "0.3.0");
        build.files.push(Piece { path: "missing.txt".into(), sha256: hex(&[1u8; 32]), size: 1 });
        assert!(place.stage("0.3.0".parse().unwrap(), &build, &archive, &AtomicBool::new(false), &mut |_| {}).is_err());
        assert_eq!(place.staged(), None);
    }

    #[test]
    fn applying_puts_every_new_file_in_place_and_keeps_what_was_there() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let staged = stage(&place, shelf.path(), "0.3.0");
        let alone = place.alone().unwrap();
        place.apply(&staged, &alone).unwrap();
        assert_eq!(read(home.path(), "trill.exe"), "window 0.3.0");
        assert_eq!(read(home.path(), "trill-mcp.exe"), "server 0.3.0");
        assert_eq!(read(home.path(), "licences/Trill.txt"), "licence 0.3.0");
        assert_eq!(read(home.path(), "licences/New in this one.txt"), "more");
        assert_eq!(place.staged(), None, "it is staged no longer");
        assert_eq!(read(home.path(), ".update/old/trill.exe"), "window 0.2.0");
        // Once nobody would put it back, what was replaced is cleared away.
        place.tidy(&alone);
        assert!(!home.path().join(WORK).join(OLD).exists());
        assert_eq!(read(home.path(), "trill.exe"), "window 0.3.0");
    }

    #[test]
    fn what_an_update_replaced_is_put_back_whole() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let staged = stage(&place, shelf.path(), "0.3.0");
        let alone = place.alone().unwrap();
        place.apply(&staged, &alone).unwrap();
        assert_eq!(place.put_back().unwrap(), "0.3.0".parse().unwrap());
        assert_eq!(read(home.path(), "trill.exe"), "window 0.2.0");
        assert_eq!(read(home.path(), "trill-mcp.exe"), "server 0.2.0");
        assert_eq!(read(home.path(), "licences/Trill.txt"), "licence 0.2.0");
        assert_eq!(read(home.path(), "licences/New in this one.txt"), "(not there)", "what was new is gone again");
        assert!(place.put_back().is_err(), "there is nothing to put back twice");
    }

    #[cfg(windows)]
    #[test]
    fn an_update_that_cannot_go_in_whole_leaves_trill_as_it_was() {
        use std::os::windows::fs::OpenOptionsExt;
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let staged = stage(&place, shelf.path(), "0.3.0");
        // Something holds the server's file so that it cannot be moved: the last file of the update to go in.
        let held = File::options().read(true).share_mode(0).open(home.path().join("trill-mcp.exe")).unwrap();
        let alone = place.alone().unwrap();
        let why = place.apply(&staged, &alone).unwrap_err();
        assert!(why.contains("trill-mcp.exe") && why.contains("Trill is as it was"), "{why}");
        drop(held);
        // The files that had gone in before it are the old ones again, and what was new is gone.
        assert_eq!(read(home.path(), "trill.exe"), "window 0.2.0");
        assert_eq!(read(home.path(), "trill-mcp.exe"), "server 0.2.0");
        assert_eq!(read(home.path(), "licences/Trill.txt"), "licence 0.2.0");
        assert_eq!(read(home.path(), "licences/New in this one.txt"), "(not there)");
        assert!(!home.path().join(WORK).join(APPLYING).exists());
        assert_eq!(place.staged(), None, "what was half used is fetched again");
    }

    #[test]
    fn a_file_that_changed_on_the_disk_since_it_was_staged_does_not_go_in() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let staged = stage(&place, shelf.path(), "0.3.0");
        std::fs::write(home.path().join(WORK).join("0.3.0").join("trill.exe"), "something else").unwrap();
        let alone = place.alone().unwrap();
        let why = place.apply(&staged, &alone).unwrap_err();
        assert!(why.contains("no longer the file that was fetched"), "{why}");
        assert_eq!(read(home.path(), "trill.exe"), "window 0.2.0");
        assert_eq!(read(home.path(), "trill-mcp.exe"), "server 0.2.0");
        assert_eq!(place.staged(), None, "it is put away, to be fetched again");
    }

    #[test]
    fn an_update_cut_short_while_it_went_in_is_mended_at_the_next_start() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let staged = stage(&place, shelf.path(), "0.3.0");
        // As the power would leave it: the licence swapped, the server moved away and its successor not in yet.
        let work = home.path().join(WORK);
        std::fs::create_dir_all(work.join(OLD).join("licences")).unwrap();
        let files: Vec<String> = staged.files.iter().map(|piece| piece.path.clone()).collect();
        write_json(&work.join(APPLYING), &Applied { to: staged.version, files }).unwrap();
        std::fs::rename(home.path().join("licences/Trill.txt"), work.join(OLD).join("licences/Trill.txt")).unwrap();
        std::fs::rename(work.join("0.3.0/licences/Trill.txt"), home.path().join("licences/Trill.txt")).unwrap();
        // A file that is new in this version had gone in, where none was.
        let new = "licences/New in this one.txt";
        std::fs::rename(work.join("0.3.0").join(new), home.path().join(new)).unwrap();
        std::fs::rename(home.path().join("trill-mcp.exe"), work.join(OLD).join("trill-mcp.exe")).unwrap();
        assert_eq!(read(home.path(), "trill-mcp.exe"), "(not there)");

        let alone = place.alone().unwrap();
        assert!(place.mend(&alone));
        // The window's program had not been reached: it is the old one still, and is left where it is.
        assert_eq!(read(home.path(), "trill.exe"), "window 0.2.0");
        assert_eq!(read(home.path(), "trill-mcp.exe"), "server 0.2.0");
        assert_eq!(read(home.path(), "licences/Trill.txt"), "licence 0.2.0");
        assert_eq!(read(home.path(), new), "(not there)");
        assert_eq!(place.staged(), None, "what was half used is fetched again");
        assert!(!place.mend(&alone), "and there is nothing to mend twice");
    }

    #[test]
    fn a_trill_is_alone_only_while_no_other_runs_from_its_folder() {
        let home = tempfile::tempdir().unwrap();
        let place = Place::at(home.path());
        let alone = place.alone().expect("nobody runs yet");
        assert!(place.alone().is_none(), "one that applies has it to itself");
        drop(alone);
        let (first, second) = (place.run(), place.run());
        assert!(place.alone().is_none(), "two run");
        drop(first);
        assert!(place.alone().is_none(), "one still runs");
        drop(second);
        assert!(place.alone().is_some());
        // Another folder is another Trill.
        let elsewhere = tempfile::tempdir().unwrap();
        let _running = place.run();
        assert!(Place::at(elsewhere.path()).alone().is_some());
    }

    #[test]
    fn what_was_replaced_is_not_cleared_away_while_the_new_version_is_tried_out() {
        let (home, shelf) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        installed(home.path(), "0.2.0");
        let place = Place::at(home.path());
        let staged = stage(&place, shelf.path(), "0.3.0");
        let alone = place.alone().unwrap();
        place.apply(&staged, &alone).unwrap();
        let trying = place.lock_file(TRYING).unwrap();
        trying.lock().unwrap();
        place.tidy(&alone);
        assert_eq!(read(home.path(), ".update/old/trill.exe"), "window 0.2.0", "it may still be put back");
        drop(trying);
        place.tidy(&alone);
        assert!(!home.path().join(WORK).join(OLD).exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_new_version_that_comes_up_is_kept_and_one_that_ends_at_once_is_said() {
        let home = tempfile::tempdir().unwrap();
        let place = Place::at(home.path());
        let shell = PathBuf::from(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into()));
        let from = Version::new(0, 2, 0);
        // One that says it is up, as a program does once its window is there, and stays a while. It
        // ends with a fault afterwards: that it came up is known by what it said, not by how it ended.
        let up = home.path().join("comes-up.cmd");
        // (One line: the script is read once, and is gone with the test's folder before it ends.)
        let script = format!("@echo up> \"%{TRIAL}%\" & ping -n 3 127.0.0.1 > nul & exit /b 9\r\n");
        std::fs::write(&up, script).unwrap();
        assert_eq!(place.try_out(place.alone().unwrap(), &up, &[], from).ended, Trial::CameUp);
        // One that ends at once with a fault.
        let Trial::Failed(why) =
            place.try_out(place.alone().unwrap(), &shell, &["/c".into(), "exit 3".into()], from).ended
        else {
            panic!("a program that ends with a fault did not come up")
        };
        assert!(why.contains("ended before it was up"), "{why}");
        // One that was asked for something short and did it.
        assert_eq!(
            place.try_out(place.alone().unwrap(), &shell, &["/c".into(), "exit 0".into()], from).ended,
            Trial::CameUp
        );
        // One that is not there.
        let Trial::Failed(why) =
            place.try_out(place.alone().unwrap(), &home.path().join("no-such.exe"), &[], from).ended
        else {
            panic!()
        };
        assert!(why.contains("could not be started"), "{why}");
    }
}
