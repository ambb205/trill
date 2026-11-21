//! The keys a meeting point's keeper gave out: a file, a line a person.
//!
//! ```text
//! # ID        WHAT PROVES THE KEY                                               NAME
//! 4KD0-9XQZ   9f2c…(64 digits)                                                  Ada
//! ```
//!
//! A line holds which key it is, what the handshake that opens a meeting
//! is held against ([`Key::proof`]) and the name of whoever was given it.
//! It does not hold the key as it was handed over. Taking a line out
//! takes the key away: the meeting point reads the file again when it has
//! changed, and ends what was held with a key that is no longer in it.

use crate::key::{Key, KeyId};
use crate::text::{hex, unhex};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// The longest name a key's line has.
pub const MAX_NAME: usize = 64;

/// A key that was given: whose, which, and what proves it.
#[derive(Clone, PartialEq, Eq)]
pub struct Given {
    pub id: KeyId,
    pub proof: [u8; 32],
    pub name: String,
}

impl Given {
    pub fn of(key: &Key, name: &str) -> Given {
        Given { id: key.id(), proof: key.proof(), name: name.to_owned() }
    }

    fn line(&self) -> String {
        format!("{} {} {}", self.id, hex(&self.proof), self.name)
    }

    fn read(line: &str) -> Option<Given> {
        let mut words = line.splitn(3, char::is_whitespace);
        let (id, proof, name) = (words.next()?, words.next()?, words.next()?.trim());
        named(name).ok()?;
        Some(Given { id: KeyId::read(id)?, proof: unhex(proof)?, name: name.to_owned() })
    }
}

impl std::fmt::Debug for Given {
    /// Without what proves the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Given({} to {})", self.id, self.name)
    }
}

/// Holds a name to what a line can carry.
fn named(name: &str) -> Result<&str, String> {
    let name = name.trim();
    let plain = !name.is_empty() && name.chars().count() <= MAX_NAME && !name.chars().any(char::is_control);
    match plain && !name.starts_with('#') {
        true => Ok(name),
        false => Err(format!("a key is given to someone with a name: one line, at most {MAX_NAME} letters")),
    }
}

/// What a file of keys holds, and the numbers of its lines that are
/// neither a key nor a remark.
pub fn read(text: &str) -> (Vec<Given>, Vec<usize>) {
    let (mut given, mut wrong): (Vec<Given>, Vec<usize>) = (Vec::new(), Vec::new());
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match Given::read(line) {
            // Of two lines for one key the first is the key.
            Some(found) if !given.iter().any(|known| known.id == found.id) => given.push(found),
            _ => wrong.push(number + 1),
        }
    }
    (given, wrong)
}

fn read_file(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

/// Writes the file whole, for its keeper's eyes only where the system
/// has such a thing.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let beside = path.with_extension("writing");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&beside)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&beside, path)
}

/// Makes a key for `name` and writes its line into the file at `path`,
/// which is made if it is not there. The key is returned to be shown
/// once: the file does not hold it.
pub fn give(path: &Path, name: &str) -> io::Result<Key> {
    let name = named(name).map_err(io::Error::other)?;
    let mut text = read_file(path)?;
    let (given, _) = read(&text);
    if given.iter().any(|known| known.name == name) {
        return Err(io::Error::other(format!(
            "{name} has a key already: take it away first (forget), or give this one to another name"
        )));
    }
    let key = Key::new()?;
    if text.is_empty() {
        text.push_str("# The keys of this meeting point: which key, what proves it, whose it is.\n");
        text.push_str("# A line taken out is a key taken away.\n");
    } else if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&Given::of(&key, name).line());
    text.push('\n');
    write_file(path, &text)?;
    Ok(key)
}

/// Takes away the keys of `who`, a name or which key: its lines go out
/// of the file. Returns the names they were given to.
pub fn forget(path: &Path, who: &str) -> io::Result<Vec<String>> {
    let text = read_file(path)?;
    let (who, id) = (who.trim(), KeyId::read(who));
    let mut gone = Vec::new();
    let kept: Vec<&str> = (text.lines())
        .filter(|line| match Given::read(line.trim()) {
            Some(found) if found.name == who || Some(found.id) == id => {
                gone.push(found.name);
                false
            }
            _ => true,
        })
        .collect();
    if !gone.is_empty() {
        write_file(path, &format!("{}\n", kept.join("\n")))?;
    }
    Ok(gone)
}

/// The keys a meeting point holds meetings to: a file that is read again
/// when it has changed, or for a test a list.
pub struct Keys {
    at: Option<PathBuf>,
    held: Mutex<Held>,
}

struct Held {
    given: Vec<Given>,
    /// When the file was last changed and how long it was, as it was read.
    read_as: Option<(SystemTime, u64)>,
}

/// What reading the keys again came to, for the log.
#[derive(Debug, PartialEq, Eq)]
pub enum Reread {
    /// The names the keys are given to, and the lines that are no key.
    Read { names: Vec<String>, wrong: Vec<usize> },
    /// The file cannot be read: the keys are as they were.
    NotRead(String),
}

impl Keys {
    pub fn at(path: impl Into<PathBuf>) -> Keys {
        Keys { at: Some(path.into()), held: Mutex::new(Held { given: Vec::new(), read_as: None }) }
    }

    /// Keys that are in no file: for a test.
    pub fn of(given: Vec<Given>) -> Keys {
        Keys { at: None, held: Mutex::new(Held { given, read_as: None }) }
    }

    /// Other keys in place of the ones there were: for a test, what
    /// taking a line out of the file is.
    pub fn set(&self, given: Vec<Given>) {
        self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).given = given;
    }

    /// The key `id`, if it is given. The file is read again first if it
    /// has changed since it was read, and what that came to is said.
    pub fn find(&self, id: KeyId, said: &mut dyn FnMut(Reread)) -> Option<Given> {
        let mut held = self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(path) = &self.at {
            let now = std::fs::metadata(path)
                .ok()
                .map(|file| (file.modified().unwrap_or(SystemTime::UNIX_EPOCH), file.len()));
            // (A file that is not there is no keys.)
            let now = now.or(Some((SystemTime::UNIX_EPOCH, 0)));
            if held.read_as != now {
                match read_file(path) {
                    Ok(text) => {
                        let (given, wrong) = read(&text);
                        let names = given.iter().map(|known| known.name.clone()).collect();
                        (held.given, held.read_as) = (given, now);
                        said(Reread::Read { names, wrong });
                    }
                    Err(error) => said(Reread::NotRead(error.to_string())),
                }
            }
        }
        held.given.iter().find(|known| known.id == id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("trill-meet-keys-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn a_key_is_given_to_a_name_kept_as_what_proves_it_and_taken_away_by_its_line() {
        let folder = folder("given");
        let path = folder.join("keys");
        let ada = give(&path, " Ada Lovelace ").unwrap();
        let ben = give(&path, "Ben").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // The file has what proves each key and whose it is, and not the keys.
        let (given, wrong) = read(&text);
        assert_eq!(given, [Given::of(&ada, "Ada Lovelace"), Given::of(&ben, "Ben")]);
        assert!(wrong.is_empty());
        for key in [&ada, &ben] {
            let secret = key.to_string();
            assert!(!text.contains(&secret[10..]), "the key itself is not kept");
            assert!(text.contains(&key.id().to_string()));
        }
        assert!(format!("{given:?}").contains("to Ada Lovelace") && !format!("{given:?}").contains(&hex(&ada.proof())));
        // A name has one key, and a name is one line.
        let again = give(&path, "Ben").unwrap_err().to_string();
        assert!(again.contains("has a key already"), "{again}");
        assert!(give(&path, "two\nlines").is_err() && give(&path, "  ").is_err() && give(&path, "# no").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "and nothing was written for them");

        // Read by a meeting point, and read again when it has changed.
        let keys = Keys::at(&path);
        let mut said = Vec::new();
        assert_eq!(keys.find(ada.id(), &mut |reread| said.push(reread)), Some(Given::of(&ada, "Ada Lovelace")));
        assert_eq!(said, [Reread::Read { names: vec!["Ada Lovelace".into(), "Ben".into()], wrong: vec![] }]);
        assert!(keys.find(ben.id(), &mut |_| panic!("it has not changed")).is_some());
        // Taken away by name, or by which key it is; a remark stays.
        assert_eq!(forget(&path, "Ada Lovelace").unwrap(), ["Ada Lovelace"]);
        assert!(forget(&path, "Nobody").unwrap().is_empty());
        assert_eq!(keys.find(ada.id(), &mut |_| {}), None);
        assert!(keys.find(ben.id(), &mut |_| {}).is_some());
        assert_eq!(forget(&path, &ben.id().to_string()).unwrap(), ["Ben"]);
        assert_eq!(keys.find(ben.id(), &mut |_| {}), None);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# The keys of this meeting point"));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_file_that_is_not_there_or_holds_lines_that_are_no_key_is_no_reason_to_stop() {
        let folder = folder("wrong");
        let path = folder.join("keys");
        let keys = Keys::at(&path);
        let key = Key::new().unwrap();
        let mut said = Vec::new();
        assert_eq!(keys.find(key.id(), &mut |reread| said.push(reread)), None);
        let line = Given::of(&key, "Cy").line();
        std::fs::write(&path, format!("# a remark\n\nnot a key\n{line}\n{line}\n{} short Dee\n", key.id())).unwrap();
        assert_eq!(keys.find(key.id(), &mut |reread| said.push(reread)), Some(Given::of(&key, "Cy")));
        let empty = Reread::Read { names: vec![], wrong: vec![] };
        assert_eq!(said, [empty, Reread::Read { names: vec!["Cy".into()], wrong: vec![3, 5, 6] }]);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
