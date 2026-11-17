//! A fetcher for tests: files come from its own shelf and from nowhere
//! else, so that the whole of a fetch (going on from what had come, the
//! checksum, the unpacking) is run with nothing asked of the network.
//!
//! It can be told to go wrong the ways a connection does: to bring a file
//! slowly, and to be lost part of the way.

use super::{Ended, Fetched, Fetcher, Keep, Part};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How much of a file comes at once.
const AT_ONCE: usize = 16 << 10;

/// What a scripted fetcher has to give, by address.
#[derive(Default)]
pub struct Shelf {
    files: Mutex<HashMap<String, Vec<u8>>>,
    /// The addresses that were asked for, in order.
    asked: Mutex<Vec<String>>,
    /// How much of the file was there already, each time one was asked for.
    began: Mutex<Vec<u64>>,
    /// How long each piece of a file takes.
    slow: Mutex<Duration>,
    /// The next fetch is lost after so many bytes.
    cut: Mutex<Option<u64>>,
}

impl Shelf {
    /// Puts `bytes` at `url`, in place of whatever was there.
    pub fn put(&self, url: &str, bytes: Vec<u8>) {
        self.files.lock().insert(url.to_owned(), bytes);
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().clone()
    }

    pub fn began(&self) -> Vec<u64> {
        self.began.lock().clone()
    }

    /// Has every piece of a file take `each` from now on.
    pub fn slow(&self, each: Duration) {
        *self.slow.lock() = each;
    }

    /// Loses the connection of the next fetch once `bytes` of the file are there.
    pub fn cut_after(&self, bytes: u64) {
        *self.cut.lock() = Some(bytes);
    }
}

impl Fetcher for Shelf {
    fn fetch(&self, url: &str, to: &Path, stop: &AtomicBool, got: &mut dyn FnMut(u64)) -> Result<(), Ended> {
        self.asked.lock().push(url.to_owned());
        let Some(bytes) = self.files.lock().get(url).cloned() else {
            return Err(Ended::Failed("the server said 404, not found".to_owned()));
        };
        let said = |error: std::io::Error| Ended::Failed(error.to_string());
        let mut have = std::fs::metadata(to).map_or(0, |metadata| metadata.len()) as usize;
        self.began.lock().push(have as u64);
        let cut = self.cut.lock().take();
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(to).map_err(said)?;
        while have < bytes.len() {
            if stop.load(Ordering::Relaxed) {
                return Err(Ended::Stopped);
            }
            if cut.is_some_and(|cut| have as u64 >= cut) {
                return Err(Ended::Failed("the connection was lost".to_owned()));
            }
            let end = (have + AT_ONCE).min(bytes.len());
            file.write_all(&bytes[have..end]).and_then(|()| file.flush()).map_err(said)?;
            have = end;
            got(have as u64);
            let each = *self.slow.lock();
            if !each.is_zero() {
                std::thread::sleep(each);
            }
        }
        Ok(())
    }
}

fn kept_for_good(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// A part made up for a test, with its files put on `shelf`: each is
/// kept under its own name, pinned to what it is. (What it is made of is
/// never freed, which a test can afford.)
pub fn part(shelf: &Shelf, id: &'static str, name: &'static str, files: &[(&str, Vec<u8>)]) -> Part {
    let fetched: Vec<Fetched> = files
        .iter()
        .map(|(file, bytes)| {
            let url = kept_for_good(format!("https://shelf.test/{id}/{file}"));
            shelf.put(url, bytes.clone());
            let sum: String = Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
            Fetched {
                url,
                sha256: kept_for_good(sum),
                size: bytes.len() as u64,
                keep: Keep::As(kept_for_good((*file).to_owned())),
            }
        })
        .collect();
    Part {
        id,
        name,
        what: "A part made up for a test.",
        licence: "Nobody's: it was made up for a test.",
        from: "shelf.test",
        needs: &[],
        files: Box::leak(fetched.into_boxed_slice()),
    }
}
