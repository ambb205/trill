//! A release as its makers announce it, and the signature that says the
//! announcement is theirs.
//!
//! An announcement is a small text published beside a release: the
//! version, what is new, and for each kind of machine the archive to
//! fetch with its size and its SHA-256. It is signed with a key whose
//! secret half its makers keep off the network, and a program takes an
//! announcement only where one of the keys built into it signed it
//! ([`read`]). So whoever comes to control the place releases are
//! published at can announce nothing: the worst they can do is to
//! withhold a release, or to show an old announcement again, and a
//! program takes no version that is not newer than itself.
//!
//! No signature scheme is written here: the signature is Ed25519, by the
//! `ed25519-dalek` crate.

use crate::Version;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// What is signed begins with these words, so that a signature made for
/// anything else is never one of an announcement.
const SIGNED_AS: &str = "an announcement of a release of Trill, 1\n";
/// An announcement longer than this is not read.
pub const LONGEST: usize = 256 * 1024;
/// What a key's file begins with.
const KEY_FILE: &str = "trill-release-key:";

/// The keys a release of Trill is announced under: each the public half,
/// in hexadecimal. A new one goes at the end; one that is taken out is
/// trusted by the builds that were made with it for as long as they run.
///
/// One is made with `cargo run -p trill-update --example release -- key
/// FILE`, which says the line to add. A build that knows no key takes no
/// announcement.
const KEYS: &[&str] = &[
    // The key Trill's releases are announced under since 0.1.0 (2026-11-23).
    "80bd0ff5ff7ac7b933b48f6d4869569605a558538e8082bd6b46b6ca43f0cff3",
];

/// The kind of machine this build runs on, as an announcement names it.
pub fn runs_on() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The public half of a key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// The key written as 64 hexadecimal digits.
    pub fn parse(text: &str) -> Result<Self, String> {
        let wrong = || "a public key is 64 hexadecimal digits".to_owned();
        let bytes: [u8; 32] = unhex(text.trim()).and_then(|bytes| bytes.try_into().ok()).ok_or_else(wrong)?;
        VerifyingKey::from_bytes(&bytes).map_err(|_| "these 64 digits are no key".to_owned())?;
        Ok(Self(bytes))
    }

    pub fn text(&self) -> String {
        hex(&self.0)
    }

    /// What a signature names its key by: the beginning of it.
    pub fn name(&self) -> String {
        hex(&self.0[..8])
    }
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({})", self.name())
    }
}

/// The secret half of a key: whoever has it announces releases. It is
/// kept in a file its owner chose, away from the repository and from any
/// machine that builds unattended, and nothing here ever prints it.
pub struct SecretKey(SigningKey);

impl SecretKey {
    /// A new key, by the system's own lot.
    pub fn make() -> Result<Self, String> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|error| format!("the system gave no random numbers: {error}"))?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    /// The key as its file holds it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let wrong = || format!("this is no key of Trill's: its file is one line that begins with {KEY_FILE}");
        let digits = text.trim().strip_prefix(KEY_FILE).ok_or_else(wrong)?;
        let seed: [u8; 32] = unhex(digits).and_then(|bytes| bytes.try_into().ok()).ok_or_else(wrong)?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    /// What its file holds: one line. Not for a screen or a log.
    pub fn file_text(&self) -> String {
        format!("{KEY_FILE}{}\n", hex(&self.0.to_bytes()))
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().to_bytes())
    }
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretKey(of {})", self.public().name())
    }
}

/// The keys this build takes an announcement under: the ones written
/// here, and a trial's where the build was made for one
/// (`TRILL_UPDATE_TRIAL_KEY` when it was compiled, which no release is).
pub fn trusted() -> Vec<PublicKey> {
    let written = KEYS.iter().copied();
    let trial = option_env!("TRILL_UPDATE_TRIAL_KEY");
    written
        .chain(trial)
        .filter_map(|text| match PublicKey::parse(text) {
            Ok(key) => Some(key),
            Err(why) => {
                log::warn!("a key this build was made with is not one: {why}");
                None
            }
        })
        .collect()
}

/// Whether this build was made to trust a trial's key beside its makers'.
pub fn made_for_a_trial() -> bool {
    option_env!("TRILL_UPDATE_TRIAL_KEY").is_some()
}

/// One file of a build, as it lies where Trill is installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    /// Its path below the folder Trill is in, with forward slashes.
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

/// A release built for one kind of machine: an archive of the files that
/// take the place of the ones Trill is installed as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// The kind of machine: `windows-x86_64` ([`runs_on`]).
    pub runs_on: String,
    /// Where the archive is published: an address that names this version.
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub files: Vec<Piece>,
}

impl Build {
    /// What it unpacks to, in bytes.
    pub fn unpacked(&self) -> u64 {
        self.files.iter().map(|piece| piece.size).sum()
    }
}

/// A release as it is announced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: Version,
    /// The day it was made: `2026-11-22`.
    pub date: String,
    /// What is new, in a few lines for whoever is asked to take it.
    pub notes: String,
    /// The page that says more, and has the installer.
    pub page: String,
    pub builds: Vec<Build>,
}

impl Release {
    /// The build for a kind of machine, where the release has one.
    pub fn build_for(&self, runs_on: &str) -> Option<&Build> {
        self.builds.iter().find(|build| build.runs_on == runs_on)
    }

    /// Whether it is an announcement a program can act on: every address
    /// one that is fetched over a connection that is checked, every
    /// checksum one, and every file a path below the folder Trill is in
    /// and nowhere else. Its makers signed it; this is against a mistake
    /// of theirs, which would otherwise be every user's.
    pub fn check(&self) -> Result<(), String> {
        let version = self.version;
        if self.notes.len() > 8 * 1024 {
            return Err(format!("the announcement of {version} says more than an announcement holds"));
        }
        if !self.page.is_empty() && !self.page.starts_with("https://") {
            return Err(format!("the announcement of {version} names a page that is no https address"));
        }
        let mut kinds = std::collections::HashSet::new();
        for build in &self.builds {
            let of = &build.runs_on;
            // One build a kind of machine: a program takes the first that is for it.
            if of.is_empty() || !kinds.insert(of.as_str()) {
                return Err(format!("the announcement of {version} has two builds for one kind of machine ('{of}')"));
            }
            // (A build made for a trial of the updater takes one from this machine itself, too: what
            // is fetched is held to the checksum that was signed, wherever it comes from.)
            let on_this_machine = made_for_a_trial() && build.url.starts_with("http://127.0.0.1:");
            if !build.url.starts_with("https://") && !on_this_machine {
                return Err(format!("the build of {version} for {of} is not at an https address"));
            }
            if !is_sha256(&build.sha256) {
                return Err(format!("the build of {version} for {of} has no checksum"));
            }
            if build.files.is_empty() {
                return Err(format!("the build of {version} for {of} holds no file"));
            }
            let mut seen = std::collections::HashSet::new();
            for piece in &build.files {
                if !is_a_path_below(&piece.path) {
                    return Err(format!(
                        "the build of {version} for {of} has a file that would not lie in Trill's own folder ('{}')",
                        piece.path
                    ));
                }
                if !is_sha256(&piece.sha256) {
                    return Err(format!("the build of {version} for {of} has no checksum for {}", piece.path));
                }
                if !seen.insert(piece.path.to_ascii_lowercase()) {
                    return Err(format!("the build of {version} for {of} has {} twice", piece.path));
                }
            }
        }
        Ok(())
    }
}

/// Whether `path` names a file below a folder and nothing else: pieces
/// with forward slashes between them, none of them empty, a dot or two, a
/// drive or the folder an update is kept in while it waits.
fn is_a_path_below(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && !path.starts_with(crate::place::WORK)
        && path.split('/').all(|piece| {
            !piece.is_empty()
                && piece != "."
                && piece != ".."
                && !piece.ends_with(['.', ' '])
                && piece.chars().all(|letter| !letter.is_control() && !"\\:*?\"<>|".contains(letter))
        })
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// An announcement as it is published: the release as text, and who
/// signed that text.
#[derive(Serialize, Deserialize)]
struct Announced {
    /// The release, as the JSON that was signed: read only once a
    /// signature over these very letters has been found good.
    signed: String,
    signatures: Vec<Signed>,
}

#[derive(Serialize, Deserialize)]
struct Signed {
    /// Which key: the beginning of its public half ([`PublicKey::name`]).
    key: String,
    /// The signature, in hexadecimal.
    signature: String,
}

fn message(signed: &str) -> Vec<u8> {
    [SIGNED_AS.as_bytes(), signed.as_bytes()].concat()
}

/// The text that announces `release`, signed with `key`: what is
/// published beside it.
pub fn announce(release: &Release, key: &SecretKey) -> Result<String, String> {
    release.check()?;
    let signed = serde_json::to_string(release).map_err(|error| error.to_string())?;
    let signature = key.0.sign(&message(&signed));
    let announced = Announced {
        signed,
        signatures: vec![Signed { key: key.public().name(), signature: hex(&signature.to_bytes()) }],
    };
    let mut text = serde_json::to_string_pretty(&announced).map_err(|error| error.to_string())?;
    text.push('\n');
    Ok(text)
}

/// The release that `text` announces, where one of the keys `trusted`
/// signed it. Whatever else the text is, it is refused in a sentence, and
/// nothing of it is acted on.
pub fn read(text: &str, trusted: &[PublicKey]) -> Result<Release, String> {
    if trusted.is_empty() {
        return Err("this build of Trill knows no key that a release is announced under, so it takes none".to_owned());
    }
    if text.len() > LONGEST {
        return Err("what was fetched is too long to be the announcement of a release".to_owned());
    }
    let not_one = "what was fetched is not the announcement of a release";
    let announced: Announced = serde_json::from_str(text).map_err(|_| not_one.to_owned())?;
    let message = message(&announced.signed);
    let signed_by_us = announced.signatures.iter().take(8).any(|signed| {
        let Some(key) = trusted.iter().find(|key| key.name() == signed.key) else { return false };
        let Some(bytes) = unhex(&signed.signature).and_then(|bytes| <[u8; 64]>::try_from(bytes).ok()) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&key.0) else { return false };
        key.verify_strict(&message, &Signature::from_bytes(&bytes)).is_ok()
    });
    if !signed_by_us {
        return Err(
            "the announcement that was fetched is not signed by the makers of this build of Trill: nothing of it is taken"
                .to_owned(),
        );
    }
    let release: Release = serde_json::from_str(&announced.signed).map_err(|_| not_one.to_owned())?;
    release.check()?;
    Ok(release)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok()).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn release(version: &str) -> Release {
        Release {
            version: version.parse().unwrap(),
            date: "2026-11-22".to_owned(),
            notes: "Takes of audio.\nA song that others hold too.".to_owned(),
            page: format!("https://example.org/trill/v{version}"),
            builds: vec![Build {
                runs_on: "windows-x86_64".to_owned(),
                url: format!("https://example.org/trill/v{version}/trill.zip"),
                sha256: "ab".repeat(32),
                size: 1000,
                files: vec![
                    Piece { path: "trill.exe".to_owned(), sha256: "cd".repeat(32), size: 2000 },
                    Piece { path: "licences/Trill.txt".to_owned(), sha256: "ef".repeat(32), size: 300 },
                ],
            }],
        }
    }

    #[test]
    fn an_announcement_is_read_back_by_whoever_trusts_its_key() {
        let key = SecretKey::make().unwrap();
        let text = announce(&release("0.3.0"), &key).unwrap();
        let read = read(&text, &[key.public()]).unwrap();
        assert_eq!(read, release("0.3.0"));
        assert_eq!(read.build_for("windows-x86_64").unwrap().unpacked(), 2300);
        assert!(read.build_for("linux-x86_64").is_none());
    }

    #[test]
    fn an_announcement_under_another_key_or_under_none_is_refused() {
        let (ours, theirs) = (SecretKey::make().unwrap(), SecretKey::make().unwrap());
        let text = announce(&release("0.3.0"), &theirs).unwrap();
        let refusal = read(&text, &[ours.public()]).unwrap_err();
        assert!(refusal.contains("not signed by the makers"), "{refusal}");
        // Somebody else's signature under our key's name is no better.
        let renamed = text.replace(&theirs.public().name(), &ours.public().name());
        assert!(read(&renamed, &[ours.public()]).unwrap_err().contains("not signed by the makers"));
        // A build that knows no key takes nothing, whoever signed.
        assert!(read(&text, &[]).unwrap_err().contains("knows no key"));
    }

    #[test]
    fn an_announcement_that_was_changed_after_it_was_signed_is_refused() {
        let key = SecretKey::make().unwrap();
        let text = announce(&release("0.3.0"), &key).unwrap();
        for (from, to) in [("0.3.0", "0.4.0"), ("example.org", "example.com"), (&"ab".repeat(32), &"ba".repeat(32))] {
            let changed = text.replace(from, to);
            assert_ne!(changed, text, "{from} is in the announcement");
            let refusal = read(&changed, &[key.public()]).unwrap_err();
            assert!(refusal.contains("not signed by the makers"), "{from}: {refusal}");
        }
    }

    #[test]
    fn a_signature_made_for_something_else_is_not_one_of_an_announcement() {
        let key = SecretKey::make().unwrap();
        let signed = serde_json::to_string(&release("0.3.0")).unwrap();
        // The same letters signed without the words an announcement is signed as.
        let bare = key.0.sign(signed.as_bytes());
        let text = serde_json::to_string(&Announced {
            signed,
            signatures: vec![Signed { key: key.public().name(), signature: hex(&bare.to_bytes()) }],
        })
        .unwrap();
        assert!(read(&text, &[key.public()]).unwrap_err().contains("not signed by the makers"));
    }

    #[test]
    fn what_is_no_announcement_at_all_is_refused_in_a_sentence() {
        let key = SecretKey::make().unwrap().public();
        for text in ["", "<html>Not Found</html>", "{}", "[1, 2]", "{\"signed\": 3}"] {
            assert!(read(text, &[key]).unwrap_err().contains("not the announcement"), "{text}");
        }
        let long = "x".repeat(LONGEST + 1);
        assert!(read(&long, &[key]).unwrap_err().contains("too long"));
    }

    #[test]
    fn a_release_whose_files_would_lie_anywhere_but_in_trills_folder_is_not_announced_or_taken() {
        let key = SecretKey::make().unwrap();
        for path in [
            "../trill.exe",
            "/trill.exe",
            "C:/Windows/trill.exe",
            "licences/../../x",
            "a\\b",
            "",
            "a//b",
            ".update/ready.json",
            "trill.exe.",
            "./trill.exe",
        ] {
            let mut wrong = release("0.3.0");
            wrong.builds[0].files[0].path = path.to_owned();
            assert!(wrong.check().unwrap_err().contains("own folder"), "{path}");
            assert!(announce(&wrong, &key).is_err(), "{path}");
            // And one that was signed all the same is not taken.
            let signed = serde_json::to_string(&wrong).unwrap();
            let signature = key.0.sign(&message(&signed));
            let text = serde_json::to_string(&Announced {
                signed,
                signatures: vec![Signed { key: key.public().name(), signature: hex(&signature.to_bytes()) }],
            })
            .unwrap();
            assert!(read(&text, &[key.public()]).unwrap_err().contains("own folder"), "{path}");
        }
        let mut twice = release("0.3.0");
        twice.builds[0].files[1].path = "TRILL.EXE".to_owned();
        assert!(twice.check().unwrap_err().contains("twice"));
        let mut plain = release("0.3.0");
        plain.builds[0].url = "http://example.org/trill.zip".to_owned();
        assert!(plain.check().unwrap_err().contains("https"));
    }

    #[test]
    fn a_release_has_a_build_for_each_kind_of_machine_and_no_kind_twice() {
        let key = SecretKey::make().unwrap();
        let mut three = release("0.3.0");
        for kind in ["macos-aarch64", "linux-x86_64"] {
            let mut build = three.builds[0].clone();
            build.runs_on = kind.to_owned();
            build.url = format!("https://example.org/trill/v0.3.0/trill-{kind}.zip");
            three.builds.push(build);
        }
        let read = read(&announce(&three, &key).unwrap(), &[key.public()]).unwrap();
        for kind in ["windows-x86_64", "macos-aarch64", "linux-x86_64"] {
            assert_eq!(read.build_for(kind).map(|build| build.runs_on.as_str()), Some(kind));
        }
        assert!(read.build_for("macos-aarch64").unwrap().url.ends_with("trill-macos-aarch64.zip"));
        assert!(read.build_for("freebsd-x86_64").is_none());
        // Two for one kind is a mistake of its makers', and is not announced.
        let mut twice = three.clone();
        twice.builds[2].runs_on = "macos-aarch64".to_owned();
        assert!(twice.check().unwrap_err().contains("two builds for one kind of machine"));
        assert!(announce(&twice, &key).is_err());
    }

    #[test]
    fn a_key_is_read_from_its_file_and_never_shown() {
        let key = SecretKey::make().unwrap();
        let back = SecretKey::parse(&key.file_text()).unwrap();
        assert_eq!(back.public(), key.public());
        assert_eq!(PublicKey::parse(&key.public().text()).unwrap(), key.public());
        let secret = key.file_text().trim().trim_start_matches(KEY_FILE).to_owned();
        assert!(!format!("{key:?}").contains(&secret));
        assert!(SecretKey::parse("not a key").unwrap_err().contains("one line"));
        assert!(PublicKey::parse("zz").unwrap_err().contains("64"));
    }
}
