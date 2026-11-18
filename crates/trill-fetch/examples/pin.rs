//! What a part is pinned by: the size and checksum of a file as it is
//! fetched and, for an archive, of what it unpacks to. This is how the
//! numbers of a `Fetched` are come by, from a file that was fetched by
//! hand and looked at.
//!
//! ```text
//! cargo run -p trill-fetch --example pin -- FILE            # a file kept as it comes
//! cargo run -p trill-fetch --example pin -- ARCHIVE.zip ""  # an archive, all of it kept
//! cargo run -p trill-fetch --example pin -- ARCHIVE.zip "repository-abc123/"   # what is below a folder of it
//! ```
//!
//! For an archive it unpacks into a folder beside the file (`<name>.unpacked`),
//! which is left there to be looked at and is nobody's to keep.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use trill_fetch::{checksum, said, tree_checksum, zip};

fn main() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(file) = arguments.first().map(PathBuf::from) else {
        return Err("usage: pin FILE, or pin ARCHIVE.zip UNDER".to_owned());
    };
    let stop = AtomicBool::new(false);
    let said_of = |ended: trill_fetch::Ended| format!("{ended:?}");
    let size = std::fs::metadata(&file).map_err(|error| format!("{}: {error}", file.display()))?.len();
    println!("size:   {size} ({})", said(size));
    println!("sha256: {}", checksum(&file, &stop).map_err(said_of)?);
    let Some(under) = arguments.get(1) else { return Ok(()) };
    let members = zip::listing(&file)?;
    let folder = PathBuf::from(format!("{}.unpacked", file.display()));
    let wanted = |member: &str| member.strip_prefix(under.as_str()).filter(|rest| !rest.is_empty()).map(str::to_owned);
    let written = zip::unpack(&file, &folder, &wanted, &stop, &mut |_| {}).map_err(said_of)?;
    let paths: Vec<String> = written.iter().map(|(path, _)| path.clone()).collect();
    println!("the archive has {} files, of which {} are below '{under}'", members.len(), written.len());
    println!("unpacked: {} ({})", written.iter().map(|(_, size)| size).sum::<u64>(), folder.display());
    println!("tree:   {}", tree_checksum(&folder, &paths, &stop).map_err(said_of)?);
    Ok(())
}
