//! Taking members out of a zip archive: what the runtime's parts come in
//! (a release's zip, a Python wheel).
//!
//! Only what those need: members that are stored or deflated, in an
//! archive under four gigabytes. Each member is written under another
//! name until it is whole and has been checked against the archive's own
//! checksum of it.

use flate2::Crc;
use flate2::read::DeflateDecoder;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

fn u16_at(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]))
}

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
    // The directory's own record is at the very end, before a comment of at most 64 KiB.
    let tail_length = length.min(22 + 65_535);
    let mut tail = vec![0u8; tail_length as usize];
    file.seek(SeekFrom::Start(length - tail_length)).and_then(|_| file.read_exact(&mut tail)).map_err(|_| not_one())?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| tail[at..at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(not_one)?;
    let (count, size, offset) = (u16_at(&tail, end + 10), u32_at(&tail, end + 12), u32_at(&tail, end + 16));
    if count == 0xffff || size == 0xffff_ffff || offset == 0xffff_ffff {
        return Err(format!(
            "{} is an archive of more than four gigabytes, which Trill does not unpack",
            archive.display()
        ));
    }
    let mut listing = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(offset)).and_then(|_| file.read_exact(&mut listing)).map_err(|_| not_one())?;
    let mut members = Vec::with_capacity(count);
    let mut at = 0usize;
    for _ in 0..count {
        if at + 46 > listing.len() || listing[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err(not_one());
        }
        let method = u16_at(&listing, at + 10);
        let (name_length, extra, comment) =
            (u16_at(&listing, at + 28), u16_at(&listing, at + 30), u16_at(&listing, at + 32));
        let name = String::from_utf8_lossy(listing.get(at + 46..at + 46 + name_length).ok_or_else(not_one)?)
            .replace('\\', "/");
        if method != 0 && method != 8 {
            return Err(format!("{name} in {} is packed in a way Trill does not unpack", archive.display()));
        }
        members.push(Member {
            name,
            deflated: method == 8,
            crc: u32_at(&listing, at + 16) as u32,
            packed: u32_at(&listing, at + 20),
            size: u32_at(&listing, at + 24),
            header: u32_at(&listing, at + 42),
        });
        at += 46 + name_length + extra + comment;
    }
    Ok(members)
}

/// Writes the members of `archive` that `wanted` gives a name to into
/// `folder`, each under that name, and answers with the names. A member
/// that is damaged is an error and leaves nothing under its name.
pub fn unpack(archive: &Path, folder: &Path, wanted: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, String> {
    let mut file = File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let members = directory(&mut file, archive)?;
    std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let mut written = Vec::new();
    for member in members.iter().filter(|member| !member.name.ends_with('/')) {
        let Some(name) = wanted(&member.name) else { continue };
        let damaged = || format!("{} in {} is damaged", member.name, archive.display());
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
        let target = folder.join(&name);
        let partial = crate::store::partial_of(&target);
        let said = |error: std::io::Error| format!("{}: {error}", target.display());
        let mut out = File::create(&partial).map_err(said)?;
        let (mut crc, mut total) = (Crc::new(), 0u64);
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = source.read(&mut buffer).map_err(|_| damaged())?;
            if read == 0 {
                break;
            }
            crc.update(&buffer[..read]);
            total += read as u64;
            out.write_all(&buffer[..read]).map_err(said)?;
        }
        drop(out);
        if crc.sum() != member.crc || total != member.size {
            let _ = std::fs::remove_file(&partial);
            return Err(damaged());
        }
        std::fs::rename(&partial, &target).map_err(said)?;
        written.push(name);
    }
    Ok(written)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;

    /// A zip archive of `members` (name, bytes, deflated), as any tool writes one.
    pub(crate) fn archive(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut listing) = (Vec::new(), Vec::new());
        for (name, bytes, deflated) in members {
            let mut crc = Crc::new();
            crc.update(bytes);
            let packed = if *deflated {
                let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(bytes).unwrap();
                encoder.finish().unwrap()
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
            listing.extend([0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0]);
            common(&mut listing);
            listing.extend([0u8; 12]);
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

    #[test]
    fn the_members_that_are_wanted_are_taken_out_whole() {
        let folder = tempfile::tempdir().unwrap();
        let text: Vec<u8> = b"the same line again and again\n".repeat(400);
        let path = folder.path().join("part.zip");
        std::fs::write(
            &path,
            archive(&[
                ("lib/runtime.dll", &text, true),
                ("lib/runtime.pdb", &[7u8; 5000], true),
                ("notes/licence.txt", b"MIT", false),
            ]),
        )
        .unwrap();
        let out = folder.path().join("out");
        let written = unpack(&path, &out, &|name| match name {
            "lib/runtime.dll" => Some("runtime.dll".to_owned()),
            name if name.ends_with(".txt") => name.rsplit('/').next().map(str::to_owned),
            _ => None,
        })
        .unwrap();
        assert_eq!(written, ["runtime.dll", "licence.txt"]);
        assert_eq!(std::fs::read(out.join("runtime.dll")).unwrap(), text);
        assert_eq!(std::fs::read(out.join("licence.txt")).unwrap(), b"MIT");
        assert!(!out.join("runtime.pdb").exists());
    }

    #[test]
    fn a_damaged_member_and_a_file_that_is_no_archive_are_said() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("part.zip");
        let mut bytes = archive(&[("a.bin", &[9u8; 3000], false)]);
        bytes[60] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let damaged = unpack(&path, folder.path(), &|name| Some(name.to_owned())).unwrap_err();
        assert!(damaged.starts_with("a.bin in ") && damaged.ends_with("is damaged"), "{damaged}");
        assert!(!folder.path().join("a.bin").exists() && !folder.path().join("a.bin.partial").exists());
        std::fs::write(&path, b"not an archive at all, only some text that is long enough").unwrap();
        assert!(unpack(&path, folder.path(), &|_| None).unwrap_err().ends_with("is not an archive Trill can unpack"));
    }
}
