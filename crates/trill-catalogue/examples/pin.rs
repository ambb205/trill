//! What it takes to pin a set, and whether a folder holds one.
//!
//! ```sh
//! cargo run -p trill-catalogue --example pin -- sum FOLDER      # what a set's entry is written from
//! cargo run -p trill-catalogue --example pin -- files FOLDER    # a line a file, for a set fetched file by file
//! cargo run -p trill-catalogue --example pin -- check SET FOLDER   # is FOLDER that set of the catalogue?
//! cargo run -p trill-catalogue --example pin -- list            # the sets, with their sizes and how they are fetched
//! ```
//!
//! `sum` prints the folder's tree checksum (what `fetch.sum` holds for a
//! set that is fetched as an archive), how many files it has and what they
//! take, and the checksum of each instrument file below it (an entry's
//! `text`). `files` prints `<sha256> <size> <path>` for every file: the
//! lines of a list in `catalogue/files.toml`. Neither reads anything but
//! the folder it is handed, and nothing here fetches anything: see
//! `catalogue/README.md` for how an entry is added or brought up to date.

use std::path::Path;
use std::process::ExitCode;
use trill_catalogue::{Catalogue, Fetch, check};

fn files(folder: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut found = Vec::new();
    let mut waiting = vec![(folder.to_path_buf(), String::new())];
    while let Some((at, below)) = waiting.pop() {
        for entry in std::fs::read_dir(&at).map_err(|error| format!("{}: {error}", at.display()))? {
            let entry = entry.map_err(|error| format!("{}: {error}", at.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if below.is_empty() { name.clone() } else { format!("{below}/{name}") };
            let kind = entry.file_type().map_err(|error| format!("{}: {error}", entry.path().display()))?;
            if kind.is_dir() {
                // A working copy's own folder is not the set's.
                if below.is_empty() && name == ".git" {
                    continue;
                }
                waiting.push((entry.path(), path));
            } else if kind.is_file() {
                let size = entry.metadata().map_err(|error| format!("{}: {error}", entry.path().display()))?.len();
                found.push((path, size));
            }
        }
    }
    found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(found)
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

fn run(arguments: &[String]) -> Result<(), String> {
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match words[..] {
        ["sum", folder] => {
            let folder = Path::new(folder);
            // Of the files that are the set's: a working copy's own folder is passed over here.
            let listed = files(folder)?;
            let mut sums = Vec::with_capacity(listed.len());
            for (path, _) in &listed {
                sums.push(check::file_sum(&check::below(folder, path))?);
            }
            let bytes: u64 = listed.iter().map(|(_, size)| size).sum();
            let sum =
                check::tree_sum_of(listed.iter().zip(&sums).map(|((path, _), sum)| (path.as_str(), sum.as_str())));
            println!("sum = \"{sum}\"");
            println!("files = {}", listed.len());
            println!("bytes = {bytes}    # {}", megabytes(bytes));
            for ((path, _), sum) in listed.iter().zip(&sums) {
                if path.to_lowercase().ends_with(".sfz") {
                    println!("{sum}  {path}");
                }
            }
            Ok(())
        }
        ["files", folder] => {
            let folder = Path::new(folder);
            for (path, size) in files(folder)? {
                println!("{} {size} {path}", check::file_sum(&check::below(folder, &path))?);
            }
            Ok(())
        }
        ["check", id, folder] => {
            let catalogue = Catalogue::shipped();
            let set = catalogue.set(id).ok_or_else(|| {
                format!("the catalogue has no set '{id}'. `pin list` names the {} it has", catalogue.sets.len())
            })?;
            let whole = set.check(Path::new(folder))?;
            println!(
                "{} is whole and as it was read: {} files, {}, {} instrument{}",
                set.name,
                whole.files,
                megabytes(whole.bytes),
                whole.instruments.len(),
                if whole.instruments.len() == 1 { "" } else { "s" }
            );
            Ok(())
        }
        ["list"] => {
            if let Some(problem) = Catalogue::problem() {
                return Err(problem);
            }
            for set in &Catalogue::shipped().sets {
                let how = match &set.fetch {
                    Fetch::Archive { .. } => "one archive".to_string(),
                    Fetch::Files { files, .. } => format!("{} files, one by one", files.len()),
                };
                println!(
                    "{:<34} {:>10}  {:<10} {:>3} instrument{}  {how}",
                    set.id,
                    megabytes(set.bytes),
                    set.licence.id(),
                    set.instruments.len(),
                    if set.instruments.len() == 1 { " " } else { "s" }
                );
            }
            Ok(())
        }
        _ => Err("say `sum FOLDER`, `files FOLDER`, `check SET FOLDER` or `list`: see the notes at the top of \
                  crates/trill-catalogue/examples/pin.rs"
            .to_string()),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}");
            ExitCode::FAILURE
        }
    }
}
