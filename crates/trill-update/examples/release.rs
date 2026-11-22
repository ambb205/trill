//! What making a release of Trill needs beside the build: the key
//! releases are announced under, the archive an update is fetched as, and
//! the announcement, signed. `scripts/release.sh` calls it; by hand:
//!
//! ```sh
//! cargo run -p trill-update --example release -- key FILE
//!     # makes a new key and writes its secret half to FILE (which must not be there yet),
//!     # and says the line that goes into `KEYS` in crates/trill-update/src/release.rs
//! cargo run -p trill-update --example release -- public FILE
//!     # the public half of the key in FILE
//! cargo run -p trill-update --example release -- pack FOLDER OUT.zip
//!     # every file below FOLDER, packed
//! cargo run -p trill-update --example release -- announce --key FILE --version 0.2.0 \
//!     --date 2026-11-22 --notes NOTES.txt --page URL --url URL-OF-THE-ZIP --zip OUT.zip --folder FOLDER \
//!     [--runs-on windows-x86_64] --out trill-release.json
//!     # the announcement of the release whose files are in FOLDER and packed in OUT.zip
//! cargo run -p trill-update --example release -- read ANNOUNCEMENT [PUBLIC-KEY]
//!     # what an announcement says, read under the keys this build trusts or under the one given
//! ```
//!
//! The secret half of the key is whoever's makes releases. Keep its file
//! away from the repository and from any machine that builds unattended;
//! nothing here prints it.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use trill_update::release::{self, Build, Piece, PublicKey, Release, SecretKey};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let done = match words.as_slice() {
        ["key", file] => key(Path::new(file)),
        ["public", file] => read_key(Path::new(file)).map(|key| println!("{}", key.public().text())),
        ["pack", folder, out] => pack(Path::new(folder), Path::new(out)),
        ["announce", rest @ ..] => announce(rest),
        ["read", file] => read(Path::new(file), None),
        ["read", file, key] => read(Path::new(file), Some(key)),
        _ => Err("usage: release key FILE | public FILE | pack FOLDER OUT.zip | announce ... | read FILE [PUBLIC-KEY] \
                  (see the top of crates/trill-update/examples/release.rs)"
            .to_owned()),
    };
    if let Err(why) = done {
        eprintln!("release: {why}");
        std::process::exit(1);
    }
}

fn said(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

fn key(file: &Path) -> Result<(), String> {
    if file.exists() {
        return Err(format!("{} is there already: a key is not written over", file.display()));
    }
    let key = SecretKey::make()?;
    if let Some(folder) = file.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        std::fs::create_dir_all(folder).map_err(said(folder))?;
    }
    std::fs::write(file, key.file_text()).map_err(said(file))?;
    println!("The secret half of a new key is in {}.", file.display());
    println!("Whoever has that file can announce a release of Trill: keep it off the repository, and keep a copy.");
    println!();
    println!("Its public half goes into KEYS in crates/trill-update/src/release.rs, as this line:");
    println!("    \"{}\",", key.public().text());
    Ok(())
}

fn read_key(file: &Path) -> Result<SecretKey, String> {
    SecretKey::parse(&std::fs::read_to_string(file).map_err(said(file))?)
}

/// The files below `folder`, each by its path with forward slashes, in
/// the order of those paths.
fn files_below(folder: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    fn walk(folder: &Path, below: &str, found: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        for entry in std::fs::read_dir(folder).map_err(said(folder))? {
            let entry = entry.map_err(said(folder))?;
            let name = entry.file_name().into_string().map_err(|name| format!("{name:?} is no name a release can hold"))?;
            let path = if below.is_empty() { name } else { format!("{below}/{name}") };
            if entry.path().is_dir() {
                walk(&entry.path(), &path, found)?;
            } else {
                found.push((path, entry.path()));
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(folder, "", &mut found)?;
    found.sort();
    if found.is_empty() {
        return Err(format!("there is no file in {}", folder.display()));
    }
    Ok(found)
}

fn pack(folder: &Path, out: &Path) -> Result<(), String> {
    let files = files_below(folder)?;
    let mut read = Vec::with_capacity(files.len());
    for (name, path) in &files {
        read.push((name.as_str(), std::fs::read(path).map_err(said(path))?));
    }
    let members: Vec<(&str, &[u8], bool)> = read.iter().map(|(name, bytes)| (*name, bytes.as_slice(), true)).collect();
    std::fs::write(out, trill_fetch::zip::archive_of(&members)).map_err(said(out))?;
    println!("{}: {} files", out.display(), members.len());
    Ok(())
}

fn sum(path: &Path) -> Result<String, String> {
    trill_fetch::checksum(path, &AtomicBool::new(false)).map_err(|ended| format!("{}: {ended:?}", path.display()))
}

fn size(path: &Path) -> Result<u64, String> {
    Ok(std::fs::metadata(path).map_err(said(path))?.len())
}

fn announce(args: &[&str]) -> Result<(), String> {
    let value = |name: &str| {
        args.iter().position(|arg| *arg == name).and_then(|at| args.get(at + 1)).copied()
    };
    let needed = |name: &str| value(name).ok_or_else(|| format!("announce needs {name}"));
    let key = read_key(Path::new(needed("--key")?))?;
    let (zip, folder, out) = (Path::new(needed("--zip")?), Path::new(needed("--folder")?), Path::new(needed("--out")?));
    let notes = Path::new(needed("--notes")?);
    let files = files_below(folder)?
        .into_iter()
        .map(|(path, file)| Ok(Piece { sha256: sum(&file)?, size: size(&file)?, path }))
        .collect::<Result<Vec<Piece>, String>>()?;
    let release = Release {
        version: needed("--version")?.parse()?,
        date: needed("--date")?.to_owned(),
        notes: std::fs::read_to_string(notes).map_err(said(notes))?.trim().to_owned(),
        page: needed("--page")?.to_owned(),
        builds: vec![Build {
            runs_on: value("--runs-on").map_or_else(release::runs_on, str::to_owned),
            url: needed("--url")?.to_owned(),
            sha256: sum(zip)?,
            size: size(zip)?,
            files,
        }],
    };
    let text = release::announce(&release, &key)?;
    // Read back as a program will read it, before it is anybody's.
    release::read(&text, &[key.public()])?;
    std::fs::write(out, text).map_err(said(out))?;
    println!("{}: Trill {} announced under the key {}", out.display(), release.version, key.public().name());
    Ok(())
}

fn read(file: &Path, key: Option<&str>) -> Result<(), String> {
    let trusted = match key {
        Some(key) => vec![PublicKey::parse(key)?],
        None => release::trusted(),
    };
    let release = release::read(&std::fs::read_to_string(file).map_err(said(file))?, &trusted)?;
    println!("Trill {} of {}", release.version, release.date);
    println!("{}", release.page);
    for line in release.notes.lines() {
        println!("    {line}");
    }
    for build in &release.builds {
        println!("for {}: {} ({} bytes, sha256 {})", build.runs_on, build.url, build.size, build.sha256);
        for piece in &build.files {
            println!("    {} ({} bytes)", piece.path, piece.size);
        }
    }
    Ok(())
}
