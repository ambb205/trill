//! Taking members out of a zip archive: what parts come in (a release's
//! zip, a Python wheel, a set of samples).
//!
//! Only what those need: members that are stored or deflated, in an
//! archive of any size (the 64-bit form included). Each member is written
//! under another name until it is whole and has been checked against the
//! archive's own checksum of it, and never anywhere but below the folder
//! it is unpacked into, whatever its name in the archive says.

use crate::Ended;
use crate::whole::{disk_free, partial_of, said};
use flate2::Crc;
use flate2::read::DeflateDecoder;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

fn u16_at(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(eight)
}

/// What a 32-bit field holds when the number is in the 64-bit extra field.
const ELSEWHERE: u64 = 0xffff_ffff;
/// Room left on a disk beyond what is unpacked onto it.
const MARGIN: u64 = 256 << 20;

/// One member as the archive's directory lists it.
struct Member {
    name: String,
    deflated: bool,
    crc: u32,
    packed: u64,
    size: u64,
    /// Where its own header is.
    header: u64,
}

fn directory(file: &mut File, archive: &Path) -> Result<Vec<Member>, String> {
    let not_one = || format!("{} is not an archive Trill can unpack", archive.display());
    let length = file.metadata().map_err(|error| format!("{}: {error}", archive.display()))?.len();
    let mut at = |from: u64, count: usize| -> Result<Vec<u8>, String> {
        let mut bytes = vec![0u8; count];
        file.seek(SeekFrom::Start(from)).and_then(|_| file.read_exact(&mut bytes)).map_err(|_| not_one())?;
        Ok(bytes)
    };
    // The directory's own record is at the very end, before a comment of at most 64 KiB.
    let tail_length = length.min(22 + 65_535);
    let tail_from = length - tail_length;
    let tail = at(tail_from, tail_length as usize)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| tail[at..at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(not_one)?;
    let (mut count, mut size, mut offset) =
        (u16_at(&tail, end + 10) as u64, u32_at(&tail, end + 12), u32_at(&tail, end + 16));
    if count == 0xffff || size == ELSEWHERE || offset == ELSEWHERE {
        // The 64-bit record: a locator just before the record says where it is.
        let locator = (tail_from + end as u64).checked_sub(20).ok_or_else(not_one)?;
        let located = at(locator, 20)?;
        if located[..4] != [0x50, 0x4b, 0x06, 0x07] {
            return Err(not_one());
        }
        let record = at(u64_at(&located, 8), 56)?;
        if record[..4] != [0x50, 0x4b, 0x06, 0x06] {
            return Err(not_one());
        }
        (count, size, offset) = (u64_at(&record, 32), u64_at(&record, 40), u64_at(&record, 48));
    }
    if size > length || count > size / 46 {
        return Err(not_one());
    }
    let listing = at(offset, size as usize)?;
    let mut members = Vec::with_capacity(count as usize);
    let mut at = 0usize;
    for _ in 0..count {
        if at + 46 > listing.len() || listing[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err(not_one());
        }
        let (flags, method) = (u16_at(&listing, at + 8), u16_at(&listing, at + 10));
        let (name_length, extra_length, comment) =
            (u16_at(&listing, at + 28), u16_at(&listing, at + 30), u16_at(&listing, at + 32));
        let name = String::from_utf8_lossy(listing.get(at + 46..at + 46 + name_length).ok_or_else(not_one)?)
            .replace('\\', "/");
        let extra = listing.get(at + 46 + name_length..at + 46 + name_length + extra_length).ok_or_else(not_one)?;
        let (mut packed, mut size, mut header) =
            (u32_at(&listing, at + 20), u32_at(&listing, at + 24), u32_at(&listing, at + 42));
        // The numbers that do not fit in 32 bits are in an extra field, in this order.
        let mut field = 0usize;
        while field + 4 <= extra.len() {
            let (id, length) = (u16_at(extra, field), u16_at(extra, field + 2));
            let body = extra.get(field + 4..field + 4 + length).ok_or_else(not_one)?;
            if id == 1 {
                let mut next = 0usize;
                for number in [&mut size, &mut packed, &mut header] {
                    if *number == ELSEWHERE {
                        *number = u64_at(body.get(next..next + 8).ok_or_else(not_one)?, 0);
                        next += 8;
                    }
                }
            }
            field += 4 + length;
        }
        let folder = name.ends_with('/');
        if !folder && flags & 1 != 0 {
            return Err(format!(
                "{name} in {} is locked with a password, which Trill does not unpack",
                archive.display()
            ));
        }
        if !folder && method != 0 && method != 8 {
            return Err(format!("{name} in {} is packed in a way Trill does not unpack", archive.display()));
        }
        if !folder {
            members.push(Member {
                name,
                deflated: method == 8,
                crc: u32_at(&listing, at + 16) as u32,
                packed,
                size,
                header,
            });
        }
        at += 46 + name_length + extra_length + comment;
    }
    Ok(members)
}

/// The names that stand for devices on Windows, whatever follows a dot.
const DEVICES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Where `name` (a path with forward slashes) is below `folder`, or
/// nothing where it would be anywhere else, or nowhere a file can be.
pub fn below(folder: &Path, name: &str) -> Option<PathBuf> {
    let mut path = folder.to_owned();
    for piece in name.split('/') {
        let stem = piece.split('.').next().unwrap_or(piece).trim_end().to_ascii_uppercase();
        if piece.is_empty()
            || piece == "."
            || piece == ".."
            || piece.contains(['\\', ':'])
            || piece.chars().any(char::is_control)
            || DEVICES.contains(&stem.as_str())
        {
            return None;
        }
        path.push(piece);
    }
    (path != folder).then_some(path)
}

/// The members of `archive` (not its folders), each with its size unpacked.
pub fn listing(archive: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut file = File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    Ok(directory(&mut file, archive)?.into_iter().map(|member| (member.name, member.size)).collect())
}

/// Writes the members of `archive` that `wanted` gives a path to into
/// `folder`, each at that path below it (forward slashes; folders are
/// made), and answers with the paths and their sizes. `unpacked` hears
/// how many bytes have been written. A member that is damaged, or that
/// would be written anywhere but below `folder`, is an error, and leaves
/// nothing under its name. Refused before anything is written when the
/// disk has not the room.
pub fn unpack(
    archive: &Path,
    folder: &Path,
    wanted: &dyn Fn(&str) -> Option<String>,
    stop: &AtomicBool,
    unpacked: &mut dyn FnMut(u64),
) -> Result<Vec<(String, u64)>, Ended> {
    let mut file = File::open(archive).map_err(|error| Ended::Failed(format!("{}: {error}", archive.display())))?;
    let members = directory(&mut file, archive).map_err(Ended::Failed)?;
    std::fs::create_dir_all(folder).map_err(|error| Ended::Failed(format!("{}: {error}", folder.display())))?;
    let mut taken: Vec<(&Member, String, PathBuf)> = Vec::new();
    for member in &members {
        let Some(name) = wanted(&member.name) else { continue };
        let Some(target) = below(folder, &name) else {
            return Err(Ended::Failed(format!(
                "{} in {} would be written outside the folder it belongs in: refused",
                member.name,
                archive.display()
            )));
        };
        taken.push((member, name, target));
    }
    let total: u64 = taken.iter().map(|(member, ..)| member.size).sum();
    if let Some(free) = disk_free(folder).filter(|&free| free < total + MARGIN) {
        return Err(Ended::Failed(format!(
            "there is not the room on the disk to unpack {}: {} are free where {} is, and {} are wanted",
            archive.file_name().unwrap_or_default().to_string_lossy().trim_end_matches(".partial"),
            said(free),
            folder.display(),
            said(total + MARGIN)
        )));
    }
    let mut written = Vec::with_capacity(taken.len());
    let mut done = 0u64;
    let mut buffer = vec![0u8; 1 << 16];
    for (member, name, target) in taken {
        let damaged = || Ended::Failed(format!("{} in {} is damaged", member.name, archive.display()));
        let mut header = [0u8; 30];
        file.seek(SeekFrom::Start(member.header)).and_then(|_| file.read_exact(&mut header)).map_err(|_| damaged())?;
        if header[..4] != [0x50, 0x4b, 0x03, 0x04] {
            return Err(damaged());
        }
        let data = member.header + 30 + (u16_at(&header, 26) + u16_at(&header, 28)) as u64;
        file.seek(SeekFrom::Start(data)).map_err(|_| damaged())?;
        let packed = (&mut file).take(member.packed);
        let mut source: Box<dyn Read + '_> =
            if member.deflated { Box::new(DeflateDecoder::new(packed)) } else { Box::new(packed) };
        let partial = partial_of(&target);
        let io = |error: std::io::Error| Ended::Failed(format!("{}: {error}", target.display()));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut out = File::create(&partial).map_err(io)?;
        let (mut crc, mut total) = (Crc::new(), 0u64);
        let ended = loop {
            if stop.load(Ordering::Relaxed) {
                break Err(Ended::Stopped);
            }
            let read = match source.read(&mut buffer) {
                Ok(read) => read,
                Err(_) => break Err(damaged()),
            };
            if read == 0 {
                break Ok(());
            }
            crc.update(&buffer[..read]);
            total += read as u64;
            // A member that unpacks to more than it said it would is not gone on with.
            if total > member.size {
                break Err(damaged());
            }
            if let Err(error) = out.write_all(&buffer[..read]) {
                break Err(io(error));
            }
            unpacked(done + total);
        };
        drop(out);
        let ended =
            ended.and_then(|()| if crc.sum() != member.crc || total != member.size { Err(damaged()) } else { Ok(()) });
        if let Err(ended) = ended {
            let _ = std::fs::remove_file(&partial);
            return Err(ended);
        }
        std::fs::rename(&partial, &target).map_err(io)?;
        done += total;
        written.push((name, total));
    }
    Ok(written)
}

/// A zip archive of `members` (name, bytes, deflated), as any tool writes
/// one: for tests, here and of whoever hands this crate a list of its own.
pub fn archive_of(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let members: Vec<(&str, &[u8], bool, Option<u32>)> =
        members.iter().map(|&(name, bytes, deflated)| (name, bytes, deflated, None)).collect();
    archive_with_modes(&members)
}

/// The same, where each member may say the Unix mode of its file (`0o755`
/// for a program), as a Unix tool writes it: for whoever unpacks the archive
/// with a tool of their own. [`unpack`] never reads it.
pub fn archive_with_modes(members: &[(&str, &[u8], bool, Option<u32>)]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    let (mut out, mut listing) = (Vec::new(), Vec::new());
    for (name, bytes, deflated, mode) in members {
        let mut crc = Crc::new();
        crc.update(bytes);
        let packed = if *deflated {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            let _ = encoder.write_all(bytes);
            encoder.finish().unwrap_or_default()
        } else {
            bytes.to_vec()
        };
        let offset = out.len() as u32;
        let method: u16 = if *deflated { 8 } else { 0 };
        let common = |out: &mut Vec<u8>| {
            out.extend(method.to_le_bytes());
            out.extend([0u8; 4]);
            out.extend(crc.sum().to_le_bytes());
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
        };
        out.extend([0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0]);
        common(&mut out);
        out.extend([0u8; 2]);
        out.extend(name.as_bytes());
        out.extend(&packed);
        // Made by Unix (3) where a mode is said, so that a tool takes the
        // attributes for a file's mode: a plain file with it.
        let made_by: u8 = if mode.is_some() { 3 } else { 0 };
        listing.extend([0x50, 0x4b, 0x01, 0x02, 20, made_by, 20, 0, 0, 0]);
        common(&mut listing);
        listing.extend([0u8; 8]);
        listing.extend(mode.map_or(0, |mode| (0o100000 | (mode & 0o777)) << 16).to_le_bytes());
        listing.extend(offset.to_le_bytes());
        listing.extend(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend(&listing);
    out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend((members.len() as u16).to_le_bytes());
    out.extend((members.len() as u16).to_le_bytes());
    out.extend((listing.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend([0u8; 2]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(name: &str) -> Option<String> {
        Some(name.to_owned())
    }

    fn quiet() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn the_members_that_are_wanted_are_taken_out_whole_with_their_folders() {
        let folder = tempfile::tempdir().unwrap();
        let text: Vec<u8> = b"the same line again and again\n".repeat(400);
        let path = folder.path().join("part.zip");
        let bytes = archive_of(&[
            ("lib/runtime.dll", &text, true),
            ("lib/runtime.pdb", &[7u8; 5000], true),
            ("notes/licence.txt", b"MIT", false),
            ("samples/piano/C4.flac", &[5u8; 900], false),
        ]);
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(listing(&path).unwrap().len(), 4);
        let out = folder.path().join("out");
        let mut heard = 0u64;
        let written = unpack(
            &path,
            &out,
            &|name| match name {
                "lib/runtime.dll" => Some("runtime.dll".to_owned()),
                name if name.ends_with(".txt") => name.rsplit('/').next().map(str::to_owned),
                name => name.strip_prefix("samples/").map(str::to_owned),
            },
            &quiet(),
            &mut |bytes| heard = bytes,
        )
        .unwrap();
        assert_eq!(
            written,
            [("runtime.dll".to_owned(), 12_000), ("licence.txt".to_owned(), 3), ("piano/C4.flac".to_owned(), 900)]
        );
        assert_eq!(heard, 12_903);
        assert_eq!(std::fs::read(out.join("runtime.dll")).unwrap(), text);
        assert_eq!(std::fs::read(out.join("piano").join("C4.flac")).unwrap(), [5u8; 900]);
        assert!(!out.join("runtime.pdb").exists());
    }

    #[test]
    fn a_damaged_member_and_a_file_that_is_no_archive_are_said() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("part.zip");
        let mut bytes = archive_of(&[("a.bin", &[9u8; 3000], false)]);
        bytes[60] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let Err(Ended::Failed(damaged)) = unpack(&path, folder.path(), &all, &quiet(), &mut |_| {}) else { panic!() };
        assert!(damaged.starts_with("a.bin in ") && damaged.ends_with("is damaged"), "{damaged}");
        assert!(!folder.path().join("a.bin").exists() && !folder.path().join("a.bin.partial").exists());
        std::fs::write(&path, b"not an archive at all, only some text that is long enough").unwrap();
        let Err(Ended::Failed(not_one)) = unpack(&path, folder.path(), &|_| None, &quiet(), &mut |_| {}) else {
            panic!()
        };
        assert!(not_one.ends_with("is not an archive Trill can unpack"), "{not_one}");
        // One that is stopped leaves nothing half-written.
        std::fs::write(&path, archive_of(&[("b.bin", &[1u8; 300_000], true)])).unwrap();
        assert_eq!(unpack(&path, folder.path(), &all, &AtomicBool::new(true), &mut |_| {}), Err(Ended::Stopped));
        assert!(!folder.path().join("b.bin").exists() && !folder.path().join("b.bin.partial").exists());
    }

    #[test]
    fn nothing_is_written_anywhere_but_below_the_folder() {
        let folder = tempfile::tempdir().unwrap();
        let inside = folder.path().join("inside");
        for name in
            ["../escaped.txt", "a/../../escaped.txt", "/rooted.txt", "C:/rooted.txt", "a//b.txt", "nul.wav", "x/COM1"]
        {
            let path = folder.path().join("part.zip");
            std::fs::write(&path, archive_of(&[(name, b"out", false)])).unwrap();
            let Err(Ended::Failed(refused)) = unpack(&path, &inside, &all, &quiet(), &mut |_| {}) else {
                panic!("{name} was unpacked")
            };
            assert!(refused.ends_with("would be written outside the folder it belongs in: refused"), "{refused}");
        }
        assert!(!folder.path().join("escaped.txt").exists());
        assert_eq!(below(Path::new("f"), "a/b.wav"), Some(Path::new("f").join("a").join("b.wav")));
        assert_eq!(below(Path::new("f"), ""), None);
    }

    #[test]
    fn a_mode_is_said_for_a_tool_of_someone_elses_and_never_taken_by_unpacking() {
        // Without modes it is the archive it always was.
        let plain = archive_of(&[("a.txt", b"text", true)]);
        assert_eq!(plain, archive_with_modes(&[("a.txt", b"text", true, None)]));
        let said = archive_with_modes(&[("trill", b"#!/bin/sh\n", true, Some(0o755)), ("a.txt", b"text", true, None)]);
        // Each member's entry in the listing at the end: who made it, and its attributes.
        let entries: Vec<usize> =
            (0..said.len() - 4).filter(|&at| said[at..at + 4] == [0x50, 0x4b, 0x01, 0x02]).collect();
        assert_eq!(entries.len(), 2);
        let made_by = |at: usize| said[at + 5];
        let attributes = |at: usize| u32::from_le_bytes(said[at + 38..at + 42].try_into().unwrap());
        assert_eq!((made_by(entries[0]), attributes(entries[0]) >> 16), (3, 0o100755), "a Unix file, 0755");
        assert_eq!((made_by(entries[1]), attributes(entries[1])), (0, 0), "a member that says nothing");

        // Unpacking reads none of it: what it writes is a plain file, whatever the archive says.
        let folder = tempfile::tempdir().unwrap();
        let archive = folder.path().join("said.zip");
        std::fs::write(&archive, &said).unwrap();
        let out = folder.path().join("out");
        unpack(&archive, &out, &all, &quiet(), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(out.join("trill")).unwrap(), b"#!/bin/sh\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(out.join("trill")).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "no file is made a program by what an archive says: {mode:o}");
        }
    }

    #[test]
    fn an_archive_in_the_64_bit_form_is_read() {
        // One member, with every number that can be moved into the 64-bit fields moved there.
        let body = b"sixty-four".to_vec();
        let mut crc = Crc::new();
        crc.update(&body);
        let name = b"big/one.bin";
        let mut out = Vec::new();
        out.extend([0x50, 0x4b, 0x03, 0x04, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(crc.sum().to_le_bytes());
        out.extend((body.len() as u32).to_le_bytes());
        out.extend((body.len() as u32).to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend([0u8; 2]);
        out.extend(name);
        out.extend(&body);
        let directory_at = out.len() as u64;
        out.extend([0x50, 0x4b, 0x01, 0x02, 45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(crc.sum().to_le_bytes());
        out.extend([0xff; 8]);
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(28u16.to_le_bytes());
        out.extend([0u8; 10]);
        out.extend([0xff; 4]);
        out.extend(name);
        out.extend(1u16.to_le_bytes());
        out.extend(24u16.to_le_bytes());
        out.extend((body.len() as u64).to_le_bytes());
        out.extend((body.len() as u64).to_le_bytes());
        out.extend(0u64.to_le_bytes());
        let directory_size = out.len() as u64 - directory_at;
        let record_at = out.len() as u64;
        out.extend([0x50, 0x4b, 0x06, 0x06]);
        out.extend(44u64.to_le_bytes());
        out.extend([45, 0, 45, 0]);
        out.extend([0u8; 8]);
        out.extend(1u64.to_le_bytes());
        out.extend(1u64.to_le_bytes());
        out.extend(directory_size.to_le_bytes());
        out.extend(directory_at.to_le_bytes());
        out.extend([0x50, 0x4b, 0x06, 0x07, 0, 0, 0, 0]);
        out.extend(record_at.to_le_bytes());
        out.extend(1u32.to_le_bytes());
        out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]);
        out.extend([0xff; 8]);
        out.extend([0u8; 2]);

        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("big.zip");
        std::fs::write(&path, out).unwrap();
        let written = unpack(&path, folder.path(), &all, &quiet(), &mut |_| {}).unwrap();
        assert_eq!(written, [("big/one.bin".to_owned(), 10)]);
        assert_eq!(std::fs::read(folder.path().join("big").join("one.bin")).unwrap(), body);
    }
}
