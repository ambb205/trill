//! How long many small files take to fetch, so many at once: what the
//! count in `trill_fetch::AT_ONCE` was chosen by.
//!
//! ```text
//! cargo run -p trill-fetch --example many -- LIST FOLDER [COUNT...]
//! ```
//!
//! `LIST` is a text file with a line for each file, an address and the
//! path it is kept at below `FOLDER`, with a tab between. Each `COUNT`
//! (4 8 16 where none is given; 0 is one `curl` for each file, as a part
//! was fetched before) fetches all of them into a folder of its own under
//! `FOLDER` and prints what it took; the folders are left to be looked at
//! and are nobody's to keep. It fetches from the addresses it is given
//! and from nowhere else: run it by hand, on a list of your own.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use trill_fetch::{Curl, Ended, Fetcher, curl_many, said};

fn main() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [list, folder, counts @ ..] = arguments.as_slice() else {
        return Err("usage: many LIST FOLDER [COUNT...]".to_owned());
    };
    let text = std::fs::read_to_string(list).map_err(|error| format!("{list}: {error}"))?;
    let wanted: Vec<(&str, &str)> = text.lines().filter_map(|line| line.split_once('\t')).collect();
    if wanted.is_empty() {
        return Err(format!("{list} has no line that is an address, a tab and a path"));
    }
    let counts: Vec<usize> = if counts.is_empty() {
        vec![4, 8, 16]
    } else {
        counts
            .iter()
            .map(|count| count.parse().map_err(|_| format!("'{count}' is not a count")))
            .collect::<Result<_, _>>()?
    };
    let stop = AtomicBool::new(false);
    for count in counts {
        let into = PathBuf::from(folder).join(format!("at-once-{count}"));
        let mut files = Vec::with_capacity(wanted.len());
        for (url, path) in &wanted {
            let to = path.split('/').fold(into.clone(), |at, piece| at.join(piece));
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            files.push(((*url).to_owned(), to));
        }
        let began = Instant::now();
        let ended = if count == 0 {
            files.iter().try_for_each(|(url, to)| {
                let _ = std::fs::remove_file(to);
                Curl.fetch(url, to, &stop, &mut |_| {})
            })
        } else {
            curl_many(count, &files, &into, &stop, &mut |_| {})
        };
        let took = began.elapsed().as_secs_f64();
        let bytes: u64 = files.iter().map(|(_, to)| std::fs::metadata(to).map_or(0, |found| found.len())).sum();
        match ended {
            Ok(()) => println!(
                "{count:>2} at once: {} files, {} in {took:.1} s ({}/s)",
                files.len(),
                said(bytes),
                said((bytes as f64 / took.max(0.001)) as u64)
            ),
            Err(Ended::Failed(why)) => {
                println!("{count:>2} at once: failed after {took:.1} s with {} here: {why}", said(bytes))
            }
            Err(Ended::Stopped) => println!("{count:>2} at once: stopped"),
        }
    }
    Ok(())
}
