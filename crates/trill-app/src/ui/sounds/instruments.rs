//! Sampled instruments on their way into the song: the library's list of
//! them, for the browser, and what dropping one does.
//!
//! An instrument is one file (`.sfz`) that names its recordings. It is not
//! a sound: it does not become a clip, and a sampler or a pad cannot play
//! it. It goes to a Sampled Instrument, to a track (whose instrument it
//! becomes) or below the tracks (for a track of its own). All of it runs
//! on a worker, as a dropped sound does.

use super::{DropTarget, Dropped, file_stem, stored_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::ChainRef;
use trill_core::pitch::pitch_name;
use trill_library::{EntryKind, InstrumentFacts, Query};
use trill_runtime::Runtime;

/// The device that plays one.
const SAMPLED: &str = trill_dsp::sampled::URI;

/// One instrument of the library, as the browser lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentRow {
    pub path: PathBuf,
    pub name: String,
    /// What it takes on the disk, for the end of its row: `712 MB`.
    pub size: String,
    /// What it is, in a line: its keys, layers, takes and ways of playing.
    pub about: String,
}

/// The library's instruments, as they were at one generation of it.
#[derive(Default)]
pub struct Shelf {
    /// The generation `rows` was read at; `None` before the first answer.
    pub(super) generation: Option<u64>,
    pub(super) in_flight: bool,
    pub(super) rows: Arc<Vec<InstrumentRow>>,
}

/// Whether the file at `path` is an instrument, by its name.
pub fn is_instrument(path: &Path) -> bool {
    trill_dsp::sfz::is_sfz(path)
}

/// Every instrument of the library by name, with the generation the list
/// was read at.
pub(super) fn list(runtime: &Runtime) -> (u64, Vec<InstrumentRow>) {
    // Read before the search, so a change during it is not missed.
    let generation = runtime.library.generation();
    let found = runtime.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() });
    let mut rows: Vec<InstrumentRow> = found
        .entries()
        .map(|entry| InstrumentRow {
            path: entry.path.clone(),
            name: entry.name.clone(),
            size: entry.instrument.as_deref().map(|facts| size_words(facts.bytes)).unwrap_or_default(),
            about: entry.instrument.as_deref().map(about).unwrap_or_else(|| "Its file could not be read".to_owned()),
        })
        .collect();
    rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    (generation, rows)
}

/// `"712 MB"`, `"1.9 GB"`, `"840 KB"`.
pub fn size_words(bytes: u64) -> String {
    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    if megabytes >= 1024.0 {
        format!("{:.1} GB", megabytes / 1024.0)
    } else if megabytes >= 1.0 {
        format!("{megabytes:.0} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

/// What the library knows of an instrument, in a line.
fn about(facts: &InstrumentFacts) -> String {
    let mut words = Vec::new();
    match (facts.range, facts.drums) {
        (Some((low, high)), true) => words.push(format!("Drums {}\u{2013}{}", pitch_name(low), pitch_name(high))),
        (Some((low, high)), false) => words.push(format!("{}\u{2013}{}", pitch_name(low), pitch_name(high))),
        (None, true) => words.push("Drums".to_owned()),
        (None, false) => {}
    }
    if facts.layers > 1 {
        words.push(format!("{} layers", facts.layers));
    }
    if facts.takes > 1 {
        words.push(format!("{} takes", facts.takes));
    }
    words.push(format!("{} recording{}", facts.recordings, if facts.recordings == 1 { "" } else { "s" }));
    if facts.found < facts.recordings {
        words.push(format!("{} of them not in the library", facts.recordings - facts.found));
    }
    if !facts.ways.is_empty() {
        words.push(facts.ways.join(", ").replace('_', " "));
    }
    words.join(" \u{00B7} ")
}

/// Gives the instrument in the file at `path` to what it was dropped on.
/// One undo step; an error is the session's sentence, and the song is as
/// it was.
pub(super) fn place(runtime: &Runtime, path: &Path, target: &DropTarget) -> Result<Dropped, String> {
    let file = stored_path(path);
    let name = file_stem(path);
    let mut session = runtime.session.lock();
    let (device, track) = match target {
        DropTarget::Instrument { device } => {
            session.load_preset(*device, &file).map_err(|error| error.to_string())?;
            (*device, None)
        }
        DropTarget::TrackInstrument { track } => {
            // Into the Sampled Instrument the track plays through, or in
            // place of whatever it plays through: only the first
            // instrument of a chain is given the notes.
            let playing = session.instrument_of(*track).map(|device| (device.id, device.uri == SAMPLED));
            match playing {
                Some((device, true)) => {
                    session.load_preset(device, &file).map_err(|error| error.to_string())?;
                    (device, Some(*track))
                }
                other => {
                    let chain = session.project().track(*track).map_err(|error| error.to_string())?;
                    let index = other
                        .and_then(|(old, _)| chain.devices.iter().position(|device| device.id == old))
                        .unwrap_or(0);
                    session.begin_gesture();
                    let removed = match other {
                        Some((old, _)) => session.edit(|project| project.remove_device(old).map(drop)),
                        None => Ok(()),
                    };
                    // If the instrument cannot be read the session takes
                    // the gesture back, and with it what was removed.
                    let added = removed
                        .and_then(|()| session.add_device(ChainRef::Track(*track), SAMPLED, Some(&file), Some(index)));
                    session.end_gesture();
                    (added.map_err(|error| error.to_string())?, Some(*track))
                }
            }
        }
        DropTarget::NewInstrument { color } => {
            session.begin_gesture();
            let track = session.edit(|project| {
                let track = project.add_track(name.clone());
                if let Some(color) = color {
                    project.track_mut(track)?.color = *color;
                }
                Ok(track)
            });
            let added = track.and_then(|track| {
                session.add_device(ChainRef::Track(track), SAMPLED, Some(&file), None).map(|device| (device, track))
            });
            session.end_gesture();
            let (device, track) = added.map_err(|error| error.to_string())?;
            (device, Some(track))
        }
        _ => {
            return Err(format!(
                "{name} is an instrument, not a sound: drop it on a track, below the tracks, or on a Sampled Instrument."
            ));
        }
    };
    drop(session);
    runtime.library.mark_used(path);
    Ok(Dropped::Instrument { device, track, name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instrument_of_the_library_is_said_in_a_line() {
        let piano = InstrumentFacts {
            recordings: 637,
            regions: 1117,
            found: 637,
            bytes: 746_586_112,
            range: Some((21, 108)),
            layers: 16,
            takes: 1,
            drums: false,
            ways: vec!["natural".into(), "re_tuned".into()],
        };
        assert_eq!(about(&piano), "A0\u{2013}C8 \u{00B7} 16 layers \u{00B7} 637 recordings \u{00B7} natural, re tuned");
        assert_eq!(size_words(piano.bytes), "712 MB");
        let kit = InstrumentFacts { recordings: 12, found: 9, range: Some((36, 59)), drums: true, takes: 4, ..piano };
        assert_eq!(
            about(&InstrumentFacts { ways: Vec::new(), layers: 1, ..kit }),
            "Drums C2\u{2013}B3 \u{00B7} 4 takes \u{00B7} 12 recordings \u{00B7} 3 of them not in the library"
        );
        assert_eq!(
            (size_words(2_040_109_465), size_words(300_000), size_words(0)),
            ("1.9 GB".into(), "293 KB".into(), "0 KB".into())
        );
        assert!(is_instrument(Path::new("C:/sounds/Grand.SFZ")) && !is_instrument(Path::new("C:/sounds/kick.wav")));
    }
}
