//! Whether the SFZ reader copes with files that were written by others,
//! for another program, and not with this one in mind.
//!
//! Opt-in: it reads files of a synth that is installed on the machine it
//! runs on. It reads their **text** and nothing else: no recording is
//! opened, nothing is played, nothing is copied anywhere, and what it
//! prints is counts and the names of opcodes, which are the format's and
//! not the maker's.
//!
//! ```sh
//! cargo test -p trill-dsp --test real_sfz -- --ignored --nocapture
//! ```
//!
//! By default it reads what Serum 2 keeps under the user's documents;
//! `TRILL_SFZ_FOLDER` names another folder to read instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use trill_dsp::sfz;

/// Every `.sfz` file under `folder`, at most `limit` of them.
fn sfz_files(folder: &Path, limit: usize) -> Vec<PathBuf> {
    let (mut found, mut waiting) = (Vec::new(), vec![folder.to_path_buf()]);
    while let Some(next) = waiting.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                waiting.push(path);
            } else if sfz::is_sfz(&path) && found.len() < limit {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
#[ignore = "reads the text of the SFZ files a synth installed on this machine keeps: \
            cargo test -p trill-dsp --test real_sfz -- --ignored --nocapture"]
fn the_reader_copes_with_the_sfz_files_another_program_keeps() {
    let folder = std::env::var_os("TRILL_SFZ_FOLDER").map(PathBuf::from).or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join("Documents").join("Xfer").join("Serum 2 Presets").join("Multisamples"))
    });
    let Some(folder) = folder.filter(|folder| folder.is_dir()) else {
        println!("no folder of SFZ files on this machine: nothing was read");
        return;
    };
    let files = sfz_files(&folder, 5_000);
    assert!(!files.is_empty(), "no .sfz file under {}", folder.display());
    let text = |path: &Path| std::fs::read_to_string(path).map_err(|error| error.to_string());

    let (mut regions, mut recordings, mut whole) = (0, 0, 0);
    let mut failed: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    // Each opcode that is not done: in how many files, and how many times.
    let mut unsupported: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut problems: BTreeMap<String, usize> = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        // By number: a file's name is its maker's.
        let called = format!("file {}", index + 1);
        match sfz::read(file, &text) {
            Err(reason) => failed.push(format!("{called}: {reason}")),
            Ok(instrument) => {
                if instrument.regions.is_empty() {
                    empty.push(called);
                }
                regions += instrument.regions.len();
                recordings += instrument.recordings.len();
                whole += usize::from(instrument.unsupported.is_empty());
                for item in &instrument.unsupported {
                    let entry = unsupported.entry(item.name.clone()).or_default();
                    entry.0 += 1;
                    entry.1 += item.count;
                }
                for problem in &instrument.problems {
                    // The kind of problem, without the names in it.
                    let kind: String = problem.split('\'').step_by(2).collect::<Vec<_>>().join("'…'");
                    *problems.entry(kind).or_default() += 1;
                }
            }
        }
    }
    println!(
        "{} files read: {regions} regions naming {recordings} recordings; {whole} ask for nothing that is not done",
        files.len()
    );
    for (name, (in_files, times)) in &unsupported {
        println!("  not done: {name} in {in_files} files, {times} times");
    }
    for (kind, times) in &problems {
        println!("  said of a file: {kind} ({times} times)");
    }
    assert!(failed.is_empty(), "these could not be read: {failed:#?}");
    assert!(empty.is_empty(), "these came out with no region: {empty:?}");
}
