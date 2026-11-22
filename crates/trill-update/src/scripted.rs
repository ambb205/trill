//! Makers of releases for tests: a key of their own, and a shelf their
//! releases are published on, so that the whole of an update (the
//! announcement under a signature, the fetch, the checksum, the staging)
//! is run with nothing asked of a network and no key of anyone's.

use crate::release::{Build, Piece, Release, SecretKey, announce};
use crate::updates::Options;
use crate::{Place, This};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_fetch::scripted::Shelf;

/// Where the made-up makers announce their newest release.
pub const ADDRESS: &str = "https://shelf.test/trill-release.json";
/// The kind of machine their builds are for.
pub const RUNS_ON: &str = "windows-x86_64";

/// The SHA-256 of `bytes`, in hexadecimal.
pub fn sum_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Makers of releases that exist only in a test.
pub struct Makers {
    /// What they have published, and what was asked of it.
    pub shelf: Arc<Shelf>,
    key: SecretKey,
}

impl Default for Makers {
    fn default() -> Self {
        Self::new()
    }
}

impl Makers {
    pub fn new() -> Self {
        let key = SecretKey::make().expect("the system gives random numbers");
        Self { shelf: Arc::new(Shelf::default()), key }
    }

    /// A Trill of `version` that these makers released, installed in
    /// `home`, which keeps what it learns in `kept`.
    pub fn program(&self, version: &str, home: &Path, kept: Option<PathBuf>) -> Options {
        Options {
            this: This { version: version.parse().expect("a version"), commit: Some("abc1234".to_owned()) },
            trusted: vec![self.key.public()],
            address: ADDRESS.to_owned(),
            runs_on: RUNS_ON.to_owned(),
            place: Some(Place::at(home)),
            kept,
        }
    }

    /// The release of `version` whose build holds `files` (path, bytes),
    /// with its archive put on the shelf and nothing announced yet.
    pub fn release(&self, version: &str, files: &[(&str, &[u8])]) -> Release {
        let members: Vec<(&str, &[u8], bool)> = files.iter().map(|(path, bytes)| (*path, *bytes, true)).collect();
        let archive = trill_fetch::zip::archive_of(&members);
        let url = format!("https://shelf.test/v{version}/trill-{version}-{RUNS_ON}.zip");
        let release = Release {
            version: version.parse().expect("a version"),
            date: "2026-11-22".to_owned(),
            notes: format!("What is new in {version}."),
            page: format!("https://shelf.test/v{version}"),
            builds: vec![Build {
                runs_on: RUNS_ON.to_owned(),
                url: url.clone(),
                sha256: sum_of(&archive),
                size: archive.len() as u64,
                files: files
                    .iter()
                    .map(|(path, bytes)| Piece {
                        path: (*path).to_owned(),
                        sha256: sum_of(bytes),
                        size: bytes.len() as u64,
                    })
                    .collect(),
            }],
        };
        self.shelf.put(&url, archive);
        release
    }

    /// Announces `release` as the newest, under these makers' key.
    pub fn announce(&self, release: &Release) {
        let text = announce(release, &self.key).expect("an announcement that can be made");
        self.shelf.put(ADDRESS, text.into_bytes());
    }

    /// Publishes `version` with `files` and announces it as the newest.
    pub fn publish(&self, version: &str, files: &[(&str, &[u8])]) -> Release {
        let release = self.release(version, files);
        self.announce(&release);
        release
    }

    /// The text that announces `release` under these makers' key.
    pub fn announcement(&self, release: &Release) -> String {
        announce(release, &self.key).expect("an announcement that can be made")
    }
}
