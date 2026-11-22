//! Files of a song between the programs that hold it: handed out and
//! taken in by what they are, and by nothing else.
//!
//! **A path another program says is never opened, read, written or
//! handed out.** That is held here by there being nothing that could
//! break it:
//!
//! * A program hands out bytes through [`Offered`], which is a list this
//!   program made itself, of files of its own open song that it has, each
//!   by its content ([`Content`]: a hash and a length). It is asked by a
//!   content. There is no way to ask it by a path, and what it answers for
//!   a content it was not given is the same whatever the reason.
//! * A program takes bytes in through [`Received`], into one folder of its
//!   own, under a name it makes itself: the hash it asked for, and one of
//!   a fixed list of endings. The other program chooses nothing of the
//!   name. What arrives is taken only where it was asked for (this file,
//!   this place in it, no more than the file is), and is a file of the
//!   song only once all of it is here and its bytes hash to what was asked
//!   for. Otherwise it is thrown away.
//! * **What a file may cost is settled before its first byte**
//!   ([`Bounds`], in [`Received::begin`]): a length nobody could mean is
//!   refused, a file the disk has not the room for waits, and more than
//!   the user said may arrive unasked waits for their word. A program that
//!   says it has a file of 2^60 bytes costs this one nothing: no name is
//!   made for it and nothing is asked for.
//!
//! Nothing here knows of a song, a connection or a thread: whoever drives
//! the song says what is offered and what is wanted, and hands across what
//! arrives (`trill-runtime`).

use crate::wire::MAX_PIECE;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use trill_core::files::{Content, FileIs};

/// How much of a file is read at once to hash it.
const READ: usize = 1 << 20;
/// What a file is called here while not all of it has arrived.
const PART: &str = "part";

/// Reads the file at `path` and says what it is. `go_on` is asked after
/// every stretch that was read, with how many bytes that is so far, and
/// stops the reading by saying no (`Ok(None)`). This reads all of a file,
/// which takes as long as the disk takes: never with a song held.
pub fn content_of(path: &Path, mut go_on: impl FnMut(u64) -> bool) -> io::Result<Option<Content>> {
    let mut file = File::open(path)?;
    let (mut hash, mut read) = (Sha256::new(), 0u64);
    let mut room = vec![0u8; READ];
    loop {
        match file.read(&mut room)? {
            0 => break,
            count => {
                hash.update(&room[..count]);
                read += count as u64;
                if !go_on(read) {
                    return Ok(None);
                }
            }
        }
    }
    let hash: [u8; 32] = hash.finalize().into();
    Ok(Some(Content::new(&hash, read)))
}

/// A file this program hands out: where it lies here, and when it was
/// last written as of the moment its bytes were read and found to be what
/// it is offered as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shelved {
    pub path: PathBuf,
    /// `None` for a file that is never written again (one that arrived).
    pub written: Option<SystemTime>,
}

/// What a program hands out: files of its own open song that it has, by
/// what they are. Whoever drives the song makes the list; nothing a
/// program says adds to it.
#[derive(Clone, Debug, Default)]
pub struct Offered {
    files: HashMap<Content, Shelved>,
}

impl Offered {
    pub fn new(files: impl IntoIterator<Item = (Content, Shelved)>) -> Self {
        Self { files: files.into_iter().filter(|(content, _)| content.is_well_formed()).collect() }
    }

    /// What is offered, in an order that is always the same.
    pub fn contents(&self) -> Vec<Content> {
        let mut contents: Vec<Content> = self.files.keys().cloned().collect();
        contents.sort();
        contents
    }

    pub fn has(&self, file: &Content) -> bool {
        self.files.contains_key(file)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Up to `len` bytes of an offered file from `at`, and no more than
    /// one piece carries. `None` for whatever cannot be handed out: a
    /// content that is not offered, a file that is gone or was written
    /// since it was read (it may no longer be what it is offered as), a
    /// place past its end. The answer does not say which.
    pub fn read(&self, file: &Content, at: u64, len: u64) -> Option<Vec<u8>> {
        let shelved = self.files.get(file)?;
        let mut open = File::open(&shelved.path).ok()?;
        let found = open.metadata().ok()?;
        if found.len() != file.bytes || at >= file.bytes {
            return None;
        }
        if shelved.written.is_some() && found.modified().ok() != shelved.written {
            return None;
        }
        let len = len.min(MAX_PIECE as u64).min(file.bytes - at) as usize;
        if len == 0 {
            return None;
        }
        let mut bytes = vec![0u8; len];
        open.seek(SeekFrom::Start(at)).ok()?;
        open.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }
}

/// What taking files in may cost this machine, settled before the first
/// byte of each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The longest file there is: one that is said to be longer is said
    /// by a program that means something else by it.
    pub longest: u64,
    /// How many bytes may be on their way at once without anyone having
    /// been asked: above it, files wait for the user's word.
    pub unasked: u64,
    /// How much room the disk keeps free beside what is taken in.
    pub spare: u64,
}

impl Default for Bounds {
    fn default() -> Self {
        Self { longest: 64 << 30, unasked: 500 << 20, spare: 256 << 20 }
    }
}

/// Why a file that is wanted is not begun yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Waits {
    /// The disk has not the room: what is still to arrive of the file,
    /// and what the disk has free.
    ForRoom { needs: u64, free: u64 },
    /// More would be on its way at once than may be without the user's
    /// word: how much with this file.
    ForAWord { on_the_way: u64 },
}

/// How taking a file in begins.
#[derive(Debug, PartialEq, Eq)]
pub enum Begun {
    /// All of it is here already.
    Here(PathBuf),
    /// This many bytes of it are here: the rest is to be asked for.
    From(u64),
    /// It is not begun, and nothing was made for it: see [`Waits`].
    Waits(Waits),
}

/// What came of bytes that arrived.
#[derive(Debug, PartialEq, Eq)]
pub enum Took {
    /// They are kept, and this many bytes of the file are here now.
    More(u64),
    /// The file is whole, its bytes are what was asked for, and it lies
    /// here now.
    Whole(PathBuf),
    /// Nobody asked for them: another file, another place in it, or more
    /// than the file is. Nothing was written.
    NotAsked,
    /// All of the file arrived and its bytes are not what was asked for:
    /// what arrived is thrown away.
    Other,
    /// This machine could not keep them (the disk), and why. What had
    /// arrived of the file stays, to be gone on from.
    Failed(String),
}

/// A file of which not all has arrived yet.
struct Arriving {
    home: PathBuf,
    part: PathBuf,
    file: File,
    have: u64,
    hash: Sha256,
}

/// The folder of files that came from other programs, and the files that
/// are on their way into it. Every name in it is made here.
pub struct Received {
    dir: PathBuf,
    arriving: HashMap<Content, Arriving>,
    /// Called with a file that is whole and is what was asked for, under
    /// the name it has as a part and the name it is about to be given:
    /// for whoever reads it before anyone can find it under its name.
    before_named: Option<BeforeNamed>,
    bounds: Bounds,
    /// How much room the disk a folder is on has free, where the machine
    /// can be asked.
    room: Option<Room>,
}

/// How much room the disk a folder is on has free, in bytes: `None`
/// where the machine cannot be asked.
pub type Room = Box<dyn Fn(&Path) -> Option<u64> + Send>;

/// What is done with a file that has arrived whole, before it is given
/// its name: it is handed the name the file has now and the one it gets.
pub type BeforeNamed = Box<dyn FnMut(&Path, &Path) + Send>;

impl Received {
    /// The files that came from other programs are kept in `dir`: a
    /// folder of this program's own. Nothing is made until a file is
    /// begun.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), arriving: HashMap::new(), before_named: None, bounds: Bounds::default(), room: None }
    }

    /// Has `before` called with every file that arrived whole and is what
    /// was asked for, before it is given its name.
    pub fn before_named(mut self, before: BeforeNamed) -> Self {
        self.before_named = Some(before);
        self
    }

    /// With what taking in may cost, and how the room on a disk is asked.
    pub fn bounded(mut self, bounds: Bounds, room: Option<Room>) -> Self {
        (self.bounds, self.room) = (bounds, room);
        self
    }

    pub fn bounds(&self) -> &Bounds {
        &self.bounds
    }

    /// The user's word: from here on, however much is on its way.
    pub fn allow_any_amount(&mut self) {
        self.bounds.unasked = u64::MAX;
    }

    /// How many bytes are still to arrive of the files that are on their
    /// way.
    pub fn on_the_way(&self) -> u64 {
        self.arriving.iter().map(|(file, arriving)| file.bytes - arriving.have).sum()
    }

    /// Removes the parts of files that are not among `named`, the files
    /// the song says it plays: a part is kept to be gone on from, and one
    /// of a file the song no longer names never will be. Returns how many
    /// went. (Whole files are never removed here, whatever the song names.)
    pub fn sweep(&mut self, named: &std::collections::HashSet<Content>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut gone = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(hash) = name.strip_suffix(&format!(".{PART}")).and_then(|name| name.split('.').next()) else {
                continue;
            };
            let wanted = |file: &Content| file.sha256 == hash;
            if !named.iter().any(wanted)
                && !self.arriving.keys().any(wanted)
                && std::fs::remove_file(entry.path()).is_ok()
            {
                gone += 1;
            }
        }
        gone
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where a file that is `is` lies here once it is here: in this
    /// folder, under its hash and its kind.
    pub fn home(&self, is: &FileIs) -> PathBuf {
        home_in(&self.dir, is)
    }

    /// How many bytes of a file that is on its way are here.
    pub fn have(&self, file: &Content) -> Option<u64> {
        self.arriving.get(file).map(|arriving| arriving.have)
    }

    /// The files that are on their way.
    pub fn arriving(&self) -> Vec<Content> {
        self.arriving.keys().cloned().collect()
    }

    /// Begins to take in a file, or goes on with one of which a part is
    /// here (from before a connection was lost, or from an earlier run of
    /// the program): says how much of it is here. A file that is said to
    /// be something that is not written as a content is refused, and so
    /// is one of a length nobody could mean. Where the disk has not the
    /// room, or more would be on its way than may be unasked, the file
    /// waits ([`Begun::Waits`]) and nothing is made for it.
    pub fn begin(&mut self, is: &FileIs) -> io::Result<Begun> {
        if !is.is_well_formed() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "what the file is said to be is no hash and name"));
        }
        if is.bytes > self.bounds.longest {
            let said = format!("it is said to be {} bytes long, which no file of a song is", is.bytes);
            return Err(io::Error::new(io::ErrorKind::InvalidInput, said));
        }
        let (content, home) = (is.content(), self.home(is));
        if std::fs::metadata(&home).is_ok_and(|found| found.is_file() && found.len() == is.bytes) {
            self.arriving.remove(&content);
            return Ok(Begun::Here(home));
        }
        if let Some(arriving) = self.arriving.get(&content) {
            return Ok(Begun::From(arriving.have));
        }
        let part = home.with_extension(format!("{}.{PART}", is.kind()));
        // What it may cost is settled before anything is made for it.
        let here = std::fs::metadata(&part).map_or(0, |found| found.len().min(is.bytes));
        let needs = is.bytes - here;
        let on_the_way = self.on_the_way() + needs;
        if on_the_way > self.bounds.unasked {
            return Ok(Begun::Waits(Waits::ForAWord { on_the_way }));
        }
        if let Some(free) = self.room.as_ref().and_then(|room| room(&self.dir))
            && free < on_the_way.saturating_add(self.bounds.spare)
        {
            return Ok(Begun::Waits(Waits::ForRoom { needs, free }));
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut file = File::options().read(true).write(true).create(true).truncate(false).open(&part)?;
        // What is here of it from before is read again, so that the hash
        // of the whole can be told when the rest has arrived.
        let mut hash = Sha256::new();
        let mut have = file.metadata()?.len();
        if have > is.bytes {
            file.set_len(0)?;
            have = 0;
        }
        let mut room = vec![0u8; READ];
        let mut left = have;
        while left > 0 {
            let count = file.read(&mut room[..left.min(READ as u64) as usize])?;
            if count == 0 {
                break;
            }
            hash.update(&room[..count]);
            left -= count as u64;
        }
        have -= left;
        file.set_len(have)?;
        file.seek(SeekFrom::Start(have))?;
        let mut arriving = Arriving { home, part, file, have, hash };
        // (A file of no bytes is whole as it is begun.)
        if have == is.bytes {
            return match finish(&mut arriving, &content, &mut self.before_named) {
                Took::Whole(home) => Ok(Begun::Here(home)),
                Took::Failed(why) => Err(io::Error::other(why)),
                _ if is.bytes == 0 => Err(io::Error::new(io::ErrorKind::InvalidInput, "no file of no bytes is that")),
                // What was here of it was not it: begun again from nothing.
                _ => self.begin(is),
            };
        }
        self.arriving.insert(content, arriving);
        Ok(Begun::From(have))
    }

    /// Takes in bytes of a file that was begun: only the bytes that come
    /// next, and no more than the file is. When the last of them is here,
    /// the whole is held against what was asked for.
    pub fn take(&mut self, file: &Content, at: u64, bytes: &[u8]) -> Took {
        let Some(arriving) = self.arriving.get_mut(file) else { return Took::NotAsked };
        let fits = at.checked_add(bytes.len() as u64).is_some_and(|end| end <= file.bytes);
        if at != arriving.have || bytes.is_empty() || !fits {
            return Took::NotAsked;
        }
        if let Err(error) = arriving.file.write_all(bytes) {
            // (As much as was here before, and from there again.)
            let _ = arriving.file.set_len(arriving.have);
            let _ = arriving.file.seek(SeekFrom::Start(arriving.have));
            return Took::Failed(error.to_string());
        }
        arriving.hash.update(bytes);
        arriving.have += bytes.len() as u64;
        if arriving.have < file.bytes {
            return Took::More(arriving.have);
        }
        let mut arriving = self.arriving.remove(file).expect("it was found above");
        finish(&mut arriving, file, &mut self.before_named)
    }

    /// Lets go of a file that is on its way. What arrived of it stays as
    /// a part, and is gone on from if the file is begun again.
    pub fn let_go(&mut self, file: &Content) {
        self.arriving.remove(file);
    }
}

/// Where a file that is `is` lies in `dir`, a folder of files that came
/// from other programs.
pub fn home_in(dir: &Path, is: &FileIs) -> PathBuf {
    dir.join(is.kept_as())
}

/// All of a file has arrived: it is what was asked for, and is given its
/// name, or it is not, and is thrown away.
fn finish(arriving: &mut Arriving, file: &Content, before_named: &mut Option<BeforeNamed>) -> Took {
    let hash: [u8; 32] = std::mem::take(&mut arriving.hash).finalize().into();
    if Some(hash) != file.hash() {
        let _ = arriving.file.set_len(0);
        let _ = std::fs::remove_file(&arriving.part);
        return Took::Other;
    }
    if let Err(error) = arriving.file.sync_all() {
        return Took::Failed(error.to_string());
    }
    if let Some(before) = before_named {
        before(&arriving.part, &arriving.home);
    }
    // (What lies under its name already is not known to be it: a file
    // that was cut short there, say. This one is.)
    let _ = std::fs::remove_file(&arriving.home);
    match std::fs::rename(&arriving.part, &arriving.home) {
        Ok(()) => Took::Whole(arriving.home.clone()),
        Err(error) => Took::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of this test's own under the system's temporary one,
    /// removed when the test is over.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let unique = format!("trill-together-{name}-{}-{:?}", std::process::id(), std::thread::current().id());
            let dir = std::env::temp_dir().join(unique.replace(['(', ')'], ""));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Folder(dir)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Bytes that are not the same anywhere along them.
    fn bytes(count: usize, seed: u8) -> Vec<u8> {
        (0..count).map(|at| (at as u32).wrapping_mul(2_654_435_761).to_le_bytes()[1] ^ seed).collect()
    }

    fn offer(path: &Path) -> (FileIs, Offered) {
        let content = content_of(path, |_| true).unwrap().unwrap();
        let written = std::fs::metadata(path).unwrap().modified().ok();
        let is = FileIs::new(content.clone(), &path.to_string_lossy());
        (is, Offered::new([(content, Shelved { path: path.to_owned(), written })]))
    }

    /// Hands a file across as two programs would, a piece at a time.
    fn cross(offered: &Offered, received: &mut Received, is: &FileIs) -> Took {
        let content = is.content();
        let mut at = match received.begin(is).unwrap() {
            Begun::Here(home) => return Took::Whole(home),
            Begun::From(have) => have,
            Begun::Waits(waits) => panic!("the file waits: {waits:?}"),
        };
        loop {
            let piece = offered.read(&content, at, MAX_PIECE as u64).expect("the file is offered");
            match received.take(&content, at, &piece) {
                Took::More(have) => at = have,
                took => return took,
            }
        }
    }

    #[test]
    fn a_file_is_what_its_bytes_hash_to_and_reading_it_can_be_stopped() {
        let folder = Folder::new("hash");
        let abc = content_of(&folder.file("abc.txt", b"abc"), |_| true).unwrap().unwrap();
        assert_eq!(abc.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(abc.bytes, 3);
        let empty = content_of(&folder.file("empty", b""), |_| true).unwrap().unwrap();
        assert_eq!(empty.sha256, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let long = folder.file("long.wav", &bytes(3 * READ + 17, 1));
        let mut asked = 0;
        assert_eq!(content_of(&long, |_| false).unwrap(), None, "stopped after the first stretch");
        let whole = content_of(&long, |read| {
            asked += 1;
            read <= (3 * READ + 17) as u64
        });
        assert_eq!((whole.unwrap().unwrap().bytes, asked), ((3 * READ + 17) as u64, 4));
        assert!(content_of(&folder.0.join("no such file"), |_| true).is_err());
    }

    #[test]
    fn a_file_crosses_by_what_it_is_and_lies_under_a_name_made_here() {
        let (theirs, mine) = (Folder::new("theirs"), Folder::new("mine"));
        let sung = bytes(5 * MAX_PIECE + 1234, 7);
        let (is, offered) = offer(&theirs.file("Vocal take 3.WAV", &sung));
        assert_eq!((is.name.as_str(), is.kind()), ("Vocal take 3.WAV", "wav"));
        let mut received = Received::new(mine.0.join("received"));
        assert!(!received.dir().exists(), "nothing is made until a file is begun");
        let Took::Whole(home) = cross(&offered, &mut received, &is) else { panic!("it did not arrive whole") };
        assert_eq!(std::fs::read(&home).unwrap(), sung, "the same bytes");
        assert_eq!(home, received.dir().join(format!("{}.wav", is.sha256)), "under its hash and its kind");
        assert_eq!(std::fs::read_dir(received.dir()).unwrap().count(), 1, "and nothing else is left there");
        // Asked for again, it is here.
        assert_eq!(received.begin(&is).unwrap(), Begun::Here(home.clone()));
        assert!(received.arriving().is_empty());

        // Whatever name a program says, the name here is made here: a name
        // that is no name is refused, and an ending that is none of the
        // kinds a song plays is not taken over.
        let odd = FileIs { name: "run me.exe".into(), ..is.clone() };
        assert_eq!(received.home(&odd), received.dir().join(format!("{}.bin", is.sha256)));
        for name in ["..\\..\\Startup\\x.wav", "../x.wav", "C:x.wav", "a/b.wav", "x\u{0}.wav"] {
            let planted = FileIs { name: name.into(), ..is.clone() };
            assert!(received.begin(&planted).is_err(), "{name:?} is a path, and is not begun");
        }
        let unhashed = FileIs { sha256: "..\\..\\x".into(), ..is.clone() };
        assert!(received.begin(&unhashed).is_err());
        assert_eq!(std::fs::read_dir(received.dir()).unwrap().count(), 1, "none of them made a file");

        // A file of no bytes is whole as it is begun.
        let (nothing, _) = offer(&theirs.file("silence.flac", b""));
        let Begun::Here(empty) = received.begin(&nothing).unwrap() else { panic!("a file of no bytes waits") };
        assert_eq!(std::fs::metadata(empty).unwrap().len(), 0);
    }

    #[test]
    fn nothing_is_handed_out_but_what_was_offered_as_it_was_when_it_was_offered() {
        let folder = Folder::new("offered");
        let kick = folder.file("kick.wav", &bytes(100_000, 3));
        let secret = folder.file("letters.txt", &bytes(5_000, 9));
        let (is, offered) = offer(&kick);
        let content = is.content();
        assert_eq!(offered.contents(), std::slice::from_ref(&content));
        assert_eq!(offered.read(&content, 0, 10).unwrap(), bytes(100_000, 3)[..10]);
        assert_eq!(offered.read(&content, 99_990, 4000).unwrap().len(), 10, "no further than its end");
        assert_eq!(offered.read(&content, 0, u64::MAX).unwrap().len(), MAX_PIECE, "and no more than a piece");
        // Past its end, and nothing at all.
        assert_eq!(offered.read(&content, 100_000, 10), None);
        assert_eq!(offered.read(&content, u64::MAX, 10), None);
        assert_eq!(offered.read(&content, 0, 0), None);
        // A file that lies beside it and was not offered: by what it is,
        // by what the offered one is with another length, by nothing.
        let other = content_of(&secret, |_| true).unwrap().unwrap();
        assert_eq!(offered.read(&other, 0, 10), None);
        assert_eq!(offered.read(&Content { bytes: 5_000, ..content.clone() }, 0, 10), None);
        assert_eq!(offered.read(&Content { sha256: secret.to_string_lossy().into_owned(), bytes: 5_000 }, 0, 10), None);
        // A list cannot be made to hold something that is no content.
        let planted = Content { sha256: "../letters.txt".into(), bytes: 5_000 };
        assert!(Offered::new([(planted, Shelved { path: secret.clone(), written: None })]).is_empty());

        // Written again since it was read: it may no longer be what it is
        // offered as, and is not handed out. (The same length, as a mix
        // that is bounced again has.)
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&kick, bytes(100_000, 4)).unwrap();
        let later = std::fs::metadata(&kick).unwrap().modified().unwrap();
        if Some(later) != offered.files[&content].written {
            assert_eq!(offered.read(&content, 0, 10), None);
        }
        std::fs::write(&kick, bytes(90_000, 3)).unwrap();
        assert_eq!(offered.read(&content, 0, 10), None, "nor at another length");
        std::fs::remove_file(&kick).unwrap();
        assert_eq!(offered.read(&content, 0, 10), None, "nor gone");
    }

    #[test]
    fn what_arrives_is_taken_only_where_it_was_asked_for_and_only_if_it_is_what_was_asked_for() {
        let (theirs, mine) = (Folder::new("sender"), Folder::new("taker"));
        let sung = bytes(3 * MAX_PIECE, 5);
        let (is, offered) = offer(&theirs.file("take.wav", &sung));
        let content = is.content();
        let mut received = Received::new(mine.0.join("received"));
        let piece = offered.read(&content, 0, MAX_PIECE as u64).unwrap();

        // Bytes of a file nobody asked for.
        assert_eq!(received.take(&content, 0, &piece), Took::NotAsked);
        assert!(!received.dir().exists(), "and nothing was written for them");

        assert_eq!(received.begin(&is).unwrap(), Begun::From(0));
        // Another place than the next, no bytes, and more than the file is.
        assert_eq!(received.take(&content, MAX_PIECE as u64, &piece), Took::NotAsked);
        assert_eq!(received.take(&content, 0, &[]), Took::NotAsked);
        assert_eq!(received.take(&content, u64::MAX, &piece), Took::NotAsked);
        assert_eq!(received.take(&Content { bytes: 7, ..content.clone() }, 0, &piece), Took::NotAsked);
        assert_eq!(received.take(&content, 0, &piece), Took::More(MAX_PIECE as u64));
        assert_eq!(received.take(&content, 0, &piece), Took::NotAsked, "the same again");
        // A file that never ends: the pieces go on past what the file is.
        let second = offered.read(&content, MAX_PIECE as u64, MAX_PIECE as u64).unwrap();
        assert_eq!(received.take(&content, MAX_PIECE as u64, &second), Took::More(2 * MAX_PIECE as u64));
        let too_long = [piece.as_slice(), &[1u8]].concat();
        assert_eq!(received.take(&content, 2 * MAX_PIECE as u64, &too_long), Took::NotAsked);
        assert_eq!(received.have(&content), Some(2 * MAX_PIECE as u64), "none of that moved it");

        // Other bytes than were asked for, of the right length: all of
        // it is thrown away, and nothing of it is a file here.
        let mut wrong = offered.read(&content, 2 * MAX_PIECE as u64, MAX_PIECE as u64).unwrap();
        wrong[100] ^= 1;
        assert_eq!(received.take(&content, 2 * MAX_PIECE as u64, &wrong), Took::Other);
        assert!(!received.home(&is).exists());
        assert_eq!(std::fs::read_dir(received.dir()).unwrap().count(), 0, "not a part of it either");
        assert_eq!(received.have(&content), None);
        // Asked for again, from someone who has it, it arrives.
        assert!(matches!(cross(&offered, &mut received, &is), Took::Whole(_)));
        assert_eq!(std::fs::read(received.home(&is)).unwrap(), sung);
    }

    #[test]
    fn what_a_file_may_cost_is_settled_before_its_first_byte() {
        let (theirs, mine) = (Folder::new("costly"), Folder::new("bounded"));
        let dir = mine.0.join("received");
        // A length nobody could mean, from a program that says it has it.
        let mut received = Received::new(&dir);
        let endless = FileIs::new(Content::new(&[9; 32], 1 << 60), "everything.wav");
        let refused = received.begin(&endless).unwrap_err().to_string();
        assert!(refused.contains("which no file of a song is"), "{refused}");
        assert_eq!(received.take(&endless.content(), 0, &[1, 2, 3]), Took::NotAsked);
        assert!(!dir.exists(), "and nothing was made for it");

        // The disk has not the room for it and what is to stay free.
        let (is, offered) = offer(&theirs.file("long.wav", &bytes(3 * MAX_PIECE, 1)));
        let room = |free: u64| -> Option<Room> { Some(Box::new(move |_| Some(free))) };
        let tight = Bounds { spare: 1000, ..Bounds::default() };
        let mut received = Received::new(&dir).bounded(tight.clone(), room(is.bytes + 999));
        let waits = Waits::ForRoom { needs: is.bytes, free: is.bytes + 999 };
        assert_eq!(received.begin(&is).unwrap(), Begun::Waits(waits));
        assert!(!dir.exists(), "nothing was made for it either");
        let mut received = Received::new(&dir).bounded(tight, room(is.bytes + 1000));
        assert_eq!(received.begin(&is).unwrap(), Begun::From(0), "with the room, it is begun");
        drop(received);

        // More on its way at once than may be without anyone having been
        // asked: the file that would make it so waits for the user's word.
        let (second, _) = offer(&theirs.file("second.wav", &bytes(2 * MAX_PIECE, 2)));
        let asked = Bounds { unasked: is.bytes + second.bytes - 1, ..Bounds::default() };
        let mut received = Received::new(&dir).bounded(asked, None);
        assert_eq!(received.begin(&is).unwrap(), Begun::From(0));
        let both = is.bytes + second.bytes;
        assert_eq!(received.begin(&second).unwrap(), Begun::Waits(Waits::ForAWord { on_the_way: both }));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "only the first has a part");
        // What has arrived is no longer on its way.
        let piece = offered.read(&is.content(), 0, MAX_PIECE as u64).unwrap();
        assert_eq!(received.take(&is.content(), 0, &piece), Took::More(MAX_PIECE as u64));
        assert_eq!(received.on_the_way(), is.bytes - MAX_PIECE as u64);
        assert_eq!(received.begin(&second).unwrap(), Begun::From(0));
        // And with the user's word, however much.
        let (third, _) = offer(&theirs.file("third.wav", &bytes(4 * MAX_PIECE, 3)));
        assert!(matches!(received.begin(&third).unwrap(), Begun::Waits(Waits::ForAWord { .. })));
        received.allow_any_amount();
        assert_eq!(received.begin(&third).unwrap(), Begun::From(0));
    }

    #[test]
    fn a_part_of_a_file_the_song_no_longer_names_goes_and_whole_files_stay() {
        let (theirs, mine) = (Folder::new("named"), Folder::new("swept"));
        let dir = mine.0.join("received");
        let mut received = Received::new(&dir);
        let (whole, offered) = offer(&theirs.file("whole.wav", &bytes(MAX_PIECE, 1)));
        assert!(matches!(cross(&offered, &mut received, &whole), Took::Whole(_)));
        let (half, offered) = offer(&theirs.file("half.wav", &bytes(2 * MAX_PIECE, 2)));
        let (dropped, of_dropped) = offer(&theirs.file("dropped.flac", &bytes(2 * MAX_PIECE, 3)));
        for (is, offered) in [(&half, &offered), (&dropped, &of_dropped)] {
            assert_eq!(received.begin(is).unwrap(), Begun::From(0));
            let piece = offered.read(&is.content(), 0, MAX_PIECE as u64).unwrap();
            assert!(matches!(received.take(&is.content(), 0, &piece), Took::More(_)));
        }
        std::fs::write(dir.join("something of the user's.txt"), b"left alone").unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 4);
        // The song names the first two. A part that is on its way stays
        // whatever the song names; let go of, the one it no longer names goes.
        let named: std::collections::HashSet<Content> = [whole.content(), half.content()].into();
        assert_eq!(received.sweep(&named), 0);
        received.let_go(&dropped.content());
        received.let_go(&half.content());
        assert_eq!(received.sweep(&named), 1);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let expected = [
            format!("{}.wav", whole.sha256),
            format!("{}.wav.{PART}", half.sha256),
            "something of the user's.txt".to_owned(),
        ];
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(left, expected);
        // Nothing the song names any more: the part goes, the whole file stays.
        assert_eq!(received.sweep(&std::collections::HashSet::new()), 1);
        assert!(received.home(&whole).exists(), "what arrived whole is never removed here");
        assert_eq!(received.begin(&half).unwrap(), Begun::From(0), "and is begun from nothing if it is named again");
    }

    #[test]
    fn a_file_that_was_half_here_goes_on_from_where_it_was() {
        let (theirs, mine) = (Folder::new("from"), Folder::new("half"));
        let sung = bytes(4 * MAX_PIECE + 99, 11);
        let (is, offered) = offer(&theirs.file("long take.flac", &sung));
        let content = is.content();
        let dir = mine.0.join("received");
        let mut received = Received::new(&dir);
        assert_eq!(received.begin(&is).unwrap(), Begun::From(0));
        for at in [0, MAX_PIECE as u64] {
            let piece = offered.read(&content, at, MAX_PIECE as u64).unwrap();
            assert!(matches!(received.take(&content, at, &piece), Took::More(_)));
        }
        // The connection is lost, and found again: begun again, it is as
        // far as it was.
        received.let_go(&content);
        assert_eq!(received.begin(&is).unwrap(), Begun::From(2 * MAX_PIECE as u64));
        // The program is closed and started again.
        drop(received);
        let mut received = Received::new(&dir);
        assert_eq!(received.begin(&is).unwrap(), Begun::From(2 * MAX_PIECE as u64));
        let Took::Whole(home) = cross(&offered, &mut received, &is) else { panic!("it did not arrive whole") };
        assert_eq!(std::fs::read(home).unwrap(), sung);

        // A part that is not the start of the file (the disk, or someone's
        // hand) is found out when the rest has arrived, and thrown away.
        let (other, offered) = offer(&theirs.file("other.flac", &bytes(2 * MAX_PIECE, 13)));
        let part = dir.join(format!("{}.flac.{PART}", other.sha256));
        std::fs::write(&part, bytes(MAX_PIECE, 14)).unwrap();
        let mut received = Received::new(&dir);
        assert_eq!(received.begin(&other).unwrap(), Begun::From(MAX_PIECE as u64));
        assert_eq!(cross(&offered, &mut received, &other), Took::Other);
        assert!(!part.exists() && !received.home(&other).exists());
    }
}
