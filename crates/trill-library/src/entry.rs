//! One file of the library.

use crate::key::Key;
use crate::tags::Tags;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a library file is, decided by its extension.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A sound: any format `trill-media` decodes.
    Audio,
    /// A standard MIDI file.
    Midi,
    /// A project of this program.
    Project,
    /// A plugin preset.
    Preset,
}

/// Extensions (lower case) of the kinds that are not audio. Audio is
/// whatever [`trill_media::is_audio_file`] accepts, so the library can never
/// list a sound the decoder has not heard of.
const EXTENSIONS: &[(&str, EntryKind)] = &[
    ("mid", EntryKind::Midi),
    ("midi", EntryKind::Midi),
    ("trill", EntryKind::Project),
    ("vstpreset", EntryKind::Preset),
    ("fxp", EntryKind::Preset),
    ("fxb", EntryKind::Preset),
    ("aupreset", EntryKind::Preset),
    // A plugin maker's own format: Serum 2's patches, which are sold in
    // packs and kept with the samples.
    ("serumpreset", EntryKind::Preset),
];

/// What the extension of a preset of one of Ample Sound's instruments
/// ends with: the instrument's letters come before it (`amhstate`).
const INSTRUMENT_STATE: &str = "state";

impl EntryKind {
    pub const ALL: [EntryKind; 4] = [EntryKind::Audio, EntryKind::Midi, EntryKind::Project, EntryKind::Preset];

    /// The kind of the file at `path`, or `None` if the library does not
    /// list files like it. Looks at the name only.
    pub fn of(path: &Path) -> Option<EntryKind> {
        if trill_media::is_audio_file(path) {
            return Some(EntryKind::Audio);
        }
        let extension = path.extension()?.to_str()?;
        let known = EXTENSIONS.iter().find(|(known, _)| known.eq_ignore_ascii_case(extension)).map(|(_, kind)| *kind);
        known.or_else(|| {
            let letters = extension.len().checked_sub(INSTRUMENT_STATE.len()).filter(|letters| *letters >= 2)?;
            let named_so = extension.is_char_boundary(letters)
                && extension[letters..].eq_ignore_ascii_case(INSTRUMENT_STATE)
                && extension[..letters].chars().all(|c| c.is_ascii_alphanumeric());
            named_so.then_some(EntryKind::Preset)
        })
    }

    /// A word for the kind, as shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Audio => "audio",
            EntryKind::Midi => "midi",
            EntryKind::Project => "project",
            EntryKind::Preset => "preset",
        }
    }

    pub(crate) fn code(self) -> u8 {
        self as u8
    }

    pub(crate) fn from_code(code: u8) -> Option<EntryKind> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// What the header of an audio file says.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AudioFacts {
    /// Length in frames, when the file states it.
    pub frames: Option<u64>,
    /// Sample rate in hertz; never zero.
    pub sample_rate: u32,
    /// Channels in the file. Everything plays back as stereo.
    pub channels: u16,
}

impl AudioFacts {
    /// Length in seconds, when the file states its length.
    pub fn seconds(&self) -> Option<f64> {
        self.frames.map(|frames| frames as f64 / f64::from(self.sample_rate.max(1)))
    }
}

/// One file of the library and everything known about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// The name to show: the file name without its extension.
    pub name: String,
    pub kind: EntryKind,
    /// Size in bytes.
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// For audio whose header has been read. `None` for other kinds, for
    /// audio a running scan has found but not yet examined, and for audio
    /// that is [`unreadable`](Entry::unreadable).
    pub audio: Option<AudioFacts>,
    /// The file has an audio extension but cannot be decoded: it is damaged
    /// or in a format this program does not read. It is listed, so that the
    /// library shows what is on disk, and left out of searches unless they
    /// ask for it. `trill_media::probe` gives the reason.
    pub unreadable: bool,
    /// Beats per minute, read from the file's name or folders.
    pub tempo: Option<f32>,
    /// Musical key, read from the file's name.
    pub key: Option<Key>,
    /// What kind of sound it is, read from names and length.
    pub tags: Tags,
}

impl Entry {
    /// Length in seconds, for audio that states it.
    pub fn seconds(&self) -> Option<f64> {
        self.audio.and_then(|audio| audio.seconds())
    }

    /// An audio file whose header a scan has yet to read.
    pub(crate) fn is_pending(&self) -> bool {
        self.kind == EntryKind::Audio && self.audio.is_none() && !self.unreadable
    }
}

/// The name shown for a file: its name without the extension. Names that
/// are not valid Unicode are shown with the offending parts replaced.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map(|name| without_extension(&name.to_string_lossy()).to_string()).unwrap_or_default()
}

/// A file name without its extension: everything before the last dot,
/// unless that dot is what the name starts with.
pub(crate) fn without_extension(file_name: &str) -> &str {
    match file_name.rfind('.') {
        Some(0) | None => file_name,
        Some(dot) => &file_name[..dot],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_extensions_in_any_case() {
        assert_eq!(EntryKind::of(Path::new("a/Kick 01.WAV")), Some(EntryKind::Audio));
        assert_eq!(EntryKind::of(Path::new("pad.Flac")), Some(EntryKind::Audio));
        assert_eq!(EntryKind::of(Path::new("riff.MID")), Some(EntryKind::Midi));
        assert_eq!(EntryKind::of(Path::new("song.trill")), Some(EntryKind::Project));
        assert_eq!(EntryKind::of(Path::new("Lead.vstpreset")), Some(EntryKind::Preset));
        // Presets in formats of the plugins' makers.
        assert_eq!(EntryKind::of(Path::new("OMEGA SAW.SerumPreset")), Some(EntryKind::Preset));
        assert_eq!(EntryKind::of(Path::new("9 String Metal.amhstate")), Some(EntryKind::Preset));
        assert_eq!(EntryKind::of(Path::new("Clean.AGPFState")), Some(EntryKind::Preset));
        for other in ["song.state", "x.estate", "notes.über-state", "a.b state"] {
            assert_eq!(EntryKind::of(Path::new(other)), None, "{other}");
        }
        assert_eq!(EntryKind::of(Path::new("notes.txt")), None);
        assert_eq!(EntryKind::of(Path::new("Kick.wav.asd")), None);
        assert_eq!(EntryKind::of(Path::new("wav")), None);
        for kind in EntryKind::ALL {
            assert_eq!(EntryKind::from_code(kind.code()), Some(kind));
            assert!(!kind.label().is_empty());
        }
        assert_eq!(EntryKind::from_code(200), None);
    }

    #[test]
    fn names_drop_only_the_last_extension() {
        assert_eq!(display_name(Path::new("packs/Kick 01.wav")), "Kick 01");
        assert_eq!(display_name(Path::new("mix v1.2.flac")), "mix v1.2");
        assert_eq!(display_name(Path::new(".hidden")), ".hidden");
        assert_eq!(display_name(Path::new("")), "");
        assert_eq!(display_name(Path::new("two.dots..wav")), "two.dots.");
        assert_eq!(display_name(Path::new("no extension")), "no extension");
        // The same answer the standard library gives for a path.
        for name in ["Kick 01.wav", "a.b.c", "trailing.", "x", "ünï.côde.flac", "many....dots.aif"] {
            assert_eq!(
                Some(without_extension(name)),
                Path::new(name).file_stem().and_then(|stem| stem.to_str()),
                "{name}"
            );
        }
    }
}
