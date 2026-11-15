//! WAV files whose audio is an Ogg Vorbis stream.
//!
//! FL Studio ships most of its factory samples this way: a `.wav` whose
//! format tag says "Ogg Vorbis" and whose data chunk is a complete Ogg
//! stream. Symphonia's WAV reader does not know the tag, but its Ogg reader
//! reads the stream perfectly well once it is handed the stream.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

/// The format tag for "Ogg Vorbis, mode 1": the data chunk holds a whole
/// Ogg Vorbis stream, headers included.
const WHOLE_STREAM: u16 = 0x674F;

/// The other Ogg Vorbis format tags (modes 1+, 2, 2+, 3 and 3+), in which
/// the Vorbis headers are moved into the format chunk or left out. Not read.
const OTHER_LAYOUTS: [u16; 5] = [0x676F, 0x6750, 0x6770, 0x6751, 0x6771];

/// Chunks examined before giving up on finding the audio. Real files have a
/// handful; the bound keeps a file made of thousands of empty chunks from
/// costing thousands of reads.
const MAX_CHUNKS: usize = 64;

/// The largest embedded stream read. The stream is taken into memory whole,
/// which is nothing for the samples this format is used for and must not
/// become a gigabyte for a file that merely claims to be one.
pub(crate) const MAX_STREAM_BYTES: u64 = 1 << 28;

/// What kind of file a WAV turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Embedded {
    /// Not a WAV with Vorbis in it: to be read like any other file.
    None,
    /// The data chunk is an Ogg Vorbis stream, at these bytes of the file.
    Stream { start: u64, len: u64 },
    /// Vorbis in a layout this program does not read.
    Unsupported,
}

/// Looks at the chunks of `file` to see whether it is a WAV holding Ogg
/// Vorbis, and leaves the file positioned at its start again.
///
/// Anything that does not parse as such a WAV is [`Embedded::None`]: the
/// file then goes to the ordinary readers, which say what is wrong with it.
pub(crate) fn find(file: &mut File) -> io::Result<Embedded> {
    let found = examine(file).unwrap_or(Embedded::None);
    file.seek(SeekFrom::Start(0))?;
    Ok(found)
}

fn examine(file: &mut File) -> io::Result<Embedded> {
    let mut header = [0u8; 12];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Ok(Embedded::None);
    }
    let file_len = file.metadata()?.len();
    let mut position = 12u64;
    let mut is_vorbis = false;
    for _ in 0..MAX_CHUNKS {
        let mut chunk = [0u8; 8];
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut chunk)?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        let body = position + 8;
        match &chunk[..4] {
            b"fmt " => {
                let mut tag = [0u8; 2];
                file.read_exact(&mut tag)?;
                match u16::from_le_bytes(tag) {
                    WHOLE_STREAM => is_vorbis = true,
                    tag if OTHER_LAYOUTS.contains(&tag) => return Ok(Embedded::Unsupported),
                    _ => return Ok(Embedded::None),
                }
            }
            b"data" => {
                // A size that runs past the end of the file is a file cut
                // short; what is there is still worth reading.
                let len = size.min(file_len.saturating_sub(body));
                return Ok(if is_vorbis { Embedded::Stream { start: body, len } } else { Embedded::None });
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        position = body + size + (size & 1);
    }
    Ok(Embedded::None)
}

/// Reads the Ogg Vorbis stream at `start..start + len` of `file`, ready
/// for an Ogg reader. At most [`MAX_STREAM_BYTES`] are read.
pub(crate) fn read_stream(file: &mut File, start: u64, len: u64) -> io::Result<Vec<u8>> {
    let mut stream = Vec::new();
    file.seek(SeekFrom::Start(start))?;
    file.take(len.min(MAX_STREAM_BYTES)).read_to_end(&mut stream)?;
    Ok(own_pages_only(stream))
}

/// An Ogg page found in a stream.
struct Page {
    /// Which logical stream the page belongs to.
    serial: u32,
    /// Length of the whole page in bytes.
    len: usize,
}

/// The page starting at `at`, if a whole one does.
fn page_at(stream: &[u8], at: usize) -> Option<Page> {
    const HEADER: usize = 27;
    let header = stream.get(at..at + HEADER)?;
    if &header[..4] != b"OggS" {
        return None;
    }
    let segments = stream.get(at + HEADER..at + HEADER + usize::from(header[26]))?;
    let len = HEADER + segments.len() + segments.iter().map(|size| usize::from(*size)).sum::<usize>();
    let serial = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
    (at + len <= stream.len()).then_some(Page { serial, len })
}

/// Drops the pages that do not belong to the stream the data starts with.
///
/// FL Studio writes an empty page of a second stream (marked as both its
/// first and last page) in front of the final page of many of its samples.
/// A reader takes that for the start of a new chained stream and stops,
/// losing the tail of the sound. Nothing else in these files belongs to any
/// stream but the first, so its pages are the sound and the rest is not.
fn own_pages_only(stream: Vec<u8>) -> Vec<u8> {
    let Some(own) = page_at(&stream, 0).map(|page| page.serial) else { return stream };
    let mut kept = Vec::with_capacity(stream.len());
    let mut at = 0;
    while let Some(page) = page_at(&stream, at) {
        if page.serial == own {
            kept.extend_from_slice(&stream[at..at + page.len]);
        }
        at += page.len;
    }
    // Whatever follows the last whole page (a page cut short, or bytes that
    // are not Ogg at all) is passed on for the reader to judge.
    kept.extend_from_slice(&stream[at..]);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = id.to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(body);
        if body.len() % 2 == 1 {
            bytes.push(0);
        }
        bytes
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(&body);
        bytes
    }

    fn format(tag: u16) -> Vec<u8> {
        let mut body = tag.to_le_bytes().to_vec();
        body.extend_from_slice(&[1, 0, 0x44, 0xAC, 0, 0, 0, 0, 0, 0, 1, 0, 16, 0]);
        chunk(b"fmt ", &body)
    }

    /// What `find` makes of a file holding `bytes`, and where it leaves the
    /// file positioned.
    fn found(bytes: &[u8]) -> (Embedded, u64) {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("file.wav");
        std::fs::write(&path, bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        // Wherever the file was being read, the answer is the same.
        file.seek(SeekFrom::Start(bytes.len() as u64 / 2)).unwrap();
        let embedded = find(&mut file).unwrap();
        (embedded, file.stream_position().unwrap())
    }

    #[test]
    fn the_stream_is_found_wherever_the_data_chunk_is() {
        let stream = b"OggS and the rest of the stream".to_vec();
        let len = stream.len() as u64;
        let plain = riff(&[format(WHOLE_STREAM), chunk(b"data", &stream), chunk(b"fact", &[1, 2, 3, 4])]);
        assert_eq!(found(&plain), (Embedded::Stream { start: 12 + 24 + 8, len }, 0));

        // Other chunks first, one of odd length.
        let later =
            riff(&[chunk(b"JUNK", &[0; 7]), format(WHOLE_STREAM), chunk(b"fact", &[0; 4]), chunk(b"data", &stream)]);
        assert_eq!(found(&later).0, Embedded::Stream { start: 12 + 16 + 24 + 12 + 8, len });

        // A data chunk that claims more than the file holds.
        let mut cut = plain.clone();
        cut.truncate(12 + 24 + 8 + 10);
        assert_eq!(found(&cut).0, Embedded::Stream { start: 44, len: 10 });
    }

    #[test]
    fn everything_else_goes_to_the_ordinary_readers_or_is_refused() {
        let stream = b"OggS".to_vec();
        // Ordinary PCM and float.
        for tag in [1, 3, 0xFFFE] {
            assert_eq!(found(&riff(&[format(tag), chunk(b"data", &stream)])), (Embedded::None, 0));
        }
        for tag in OTHER_LAYOUTS {
            assert_eq!(found(&riff(&[format(tag), chunk(b"data", &stream)])), (Embedded::Unsupported, 0));
        }
        // Not WAV, too short, no data chunk, data before format, and a file
        // of nothing but empty chunks.
        assert_eq!(found(b"OggS\0\0\0\0\0\0\0\0\0\0\0\0"), (Embedded::None, 0));
        assert_eq!(found(b"RIFF"), (Embedded::None, 0));
        assert_eq!(found(b""), (Embedded::None, 0));
        assert_eq!(found(&riff(&[format(WHOLE_STREAM)])), (Embedded::None, 0));
        assert_eq!(found(&riff(&[chunk(b"data", &stream), format(WHOLE_STREAM)])), (Embedded::None, 0));
        let empty_chunks: Vec<Vec<u8>> = (0..1_000).map(|_| chunk(b"pad ", &[])).collect();
        assert_eq!(found(&riff(&empty_chunks)), (Embedded::None, 0));
    }

    /// An Ogg page with the given serial and body. The checksum is left
    /// zero: nothing here verifies it.
    fn page(serial: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = b"OggS\0\0".to_vec();
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&serial.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        let segments: Vec<u8> = body.chunks(255).map(|segment| segment.len() as u8).collect();
        bytes.push(segments.len() as u8);
        bytes.extend_from_slice(&segments);
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn pages_of_other_streams_are_dropped() {
        let (first, second, last) = (page(7, b"headers"), page(7, &[1; 600]), page(7, b"the tail"));
        let stray = page(0xFFFF_FFFF, b"");
        let own: Vec<u8> = [first.clone(), second.clone(), last.clone()].concat();

        // As FL Studio writes it: a stray empty page before the last one.
        let written: Vec<u8> = [first.clone(), second.clone(), stray.clone(), last.clone()].concat();
        assert_eq!(own_pages_only(written), own);
        // Nothing to drop: untouched.
        assert_eq!(own_pages_only(own.clone()), own);
        // Strays anywhere, with bodies of their own.
        let busy: Vec<u8> = [first.clone(), page(9, &[3; 300]), second.clone(), last.clone(), page(9, b"x")].concat();
        assert_eq!(own_pages_only(busy), own);

        // What cannot be parsed is passed on as it is.
        let mut cut = own.clone();
        cut.truncate(own.len() - 3);
        assert_eq!(own_pages_only(cut.clone()), cut);
        let trailing: Vec<u8> = [own.clone(), b"not a page".to_vec()].concat();
        assert_eq!(own_pages_only(trailing.clone()), trailing);
        assert_eq!(own_pages_only(b"not Ogg at all".to_vec()), b"not Ogg at all");
        assert!(own_pages_only(Vec::new()).is_empty());
    }

    #[test]
    fn the_stream_is_read_from_its_place_in_the_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("file");
        let stream: Vec<u8> = [page(1, b"one"), page(2, b""), page(1, b"two")].concat();
        let bytes: Vec<u8> = [b"before".to_vec(), stream.clone(), b"after".to_vec()].concat();
        std::fs::write(&path, &bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        let read = read_stream(&mut file, 6, stream.len() as u64).unwrap();
        assert_eq!(read, [page(1, b"one"), page(1, b"two")].concat());
        // A length past the end of the file reads what there is.
        let read = read_stream(&mut file, 6, 10_000).unwrap();
        assert!(read.ends_with(b"after"));
        assert!(read_stream(&mut file, 10_000, 5).unwrap().is_empty());
    }
}
