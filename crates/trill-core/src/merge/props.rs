//! The properties of the things a song holds, one at a time.
//!
//! Two people who change two properties of one thing both have their way,
//! so a change names a thing and one property of it. For each kind of
//! thing this file lists the properties that are set one by one (a `…Set`:
//! one variant a property, carrying the value it is set to), with what
//! reads one out of the thing, puts one into it, and finds the ones in
//! which two versions of the thing differ.
//!
//! The lists are made by [`props!`], which names every field of the
//! model's struct in a pattern: the ones that are properties, and beside
//! them the ones that are something else (what the thing holds, where it
//! is, which thing it is). A field added to the model does not compile
//! here until it is in one list or the other.
//!
//! The number of a property ([`prop`](TrackSet::prop)) is part of the file
//! format and of what programs say to one another: a new property goes at
//! the end of its list.

use crate::channel::Channel;
use crate::files::FileIs;
use crate::instrument::InstrumentProfile;
use crate::presets::{PresetInfo, PresetRef};
use crate::project::{
    AudioContent, AudioLoop, AutomationLane, AutomationPoint, Clip, Device, LoopRegion, Marker, MidiContent, Note,
    Project, Segment, SongKey, Track, WarpMode,
};
use crate::takes::TrackInput;
use crate::time::{Beats, TimeSignature};
use crate::travel::Travel;
use crate::{TrackId, TrackKind};
use serde::{Deserialize, Serialize};

/// The number of the first property a `…Set` lists: the ones below it are
/// what every thing has (whether it is there, where it is).
pub const FIRST_SET: u8 = 8;

/// A device's saved state as a change carries it: the bytes, written as a
/// project file writes them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Blob(#[serde(with = "crate::project::state_base64")] pub Option<Vec<u8>>);

impl From<Option<Vec<u8>>> for Blob {
    fn from(bytes: Option<Vec<u8>>) -> Self {
        Blob(bytes)
    }
}

impl From<Blob> for Option<Vec<u8>> {
    fn from(blob: Blob) -> Self {
        blob.0
    }
}

macro_rules! props {
    (
        $(#[$meta:meta])*
        $set:ident of $thing:ident from $first:expr;
        { $( $variant:ident($type:ty) = $field:ident, )* }
        beside { $( $other:ident ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $set {
            $( $variant($type), )*
        }

        impl $set {
            /// The number of the property this sets.
            #[allow(unused_assignments)]
            pub fn prop(&self) -> u8 {
                let mut number: u8 = $first;
                $(
                    if matches!(self, $set::$variant(_)) {
                        return number;
                    }
                    number += 1;
                )*
                number
            }

            /// Sets the property.
            pub fn put(self, thing: &mut $thing) {
                match self {
                    $( $set::$variant(value) => thing.$field = value.into(), )*
                }
            }

            /// The property numbered `prop` as `thing` has it.
            #[allow(unused_assignments)]
            pub fn read(thing: &$thing, prop: u8) -> Option<$set> {
                let mut number: u8 = $first;
                $(
                    if prop == number {
                        return Some($set::$variant(thing.$field.clone().into()));
                    }
                    number += 1;
                )*
                None
            }

            /// Every property of `thing`, as it has them.
            pub fn all(thing: &$thing) -> Vec<$set> {
                vec![ $( $set::$variant(thing.$field.clone().into()), )* ]
            }

            /// The properties in which `now` is not as `was`.
            pub fn differing(was: &$thing, now: &$thing, mut found: impl FnMut($set)) {
                let $thing { $( $field, )* $( $other: _, )* } = now;
                $(
                    if *$field != was.$field {
                        found($set::$variant($field.clone().into()));
                    }
                )*
            }
        }
    };
}

props! {
    /// What is set of the song itself.
    SongSet of Project from FIRST_SET;
    {
        Name(String) = name,
        Tempo(f64) = tempo,
        TimeSignature(TimeSignature) = time_signature,
        Key(Option<SongKey>) = key,
        // One's own, and never anyone else's: see `Op::is_personal`.
        Loop(LoopRegion) = loop_region,
    }
    beside { format_version, tempo_changes, meter_changes, tracks, master, automation, markers, next_id }
}

props! {
    /// What is set of a marker.
    MarkerSet of Marker from FIRST_SET;
    {
        Beat(Beats) = beat,
        Name(String) = name,
    }
    beside { id }
}

props! {
    /// What is set of a track.
    TrackSet of Track from FIRST_SET;
    {
        Name(String) = name,
        Color(u32) = color,
        Volume(f32) = volume_db,
        Pan(f32) = pan,
        Mute(bool) = mute,
        // One's own, and never anyone else's: see `Op::is_personal`.
        Solo(bool) = solo,
        Group(Option<TrackId>) = group,
        Profile(InstrumentProfile) = profile,
        // One's own, and never anyone else's: which input of this
        // machine's audio device the track records, and how it is heard
        // meanwhile. See `Op::is_personal`.
        Input(Option<TrackInput>) = input,
    }
    beside { id, kind, sends, devices, clips, reference, takes }
}

props! {
    /// What is set of a clip of either kind.
    ClipSet of Clip from FIRST_SET;
    {
        Name(String) = name,
        Start(Beats) = start,
        Length(Beats) = length,
        Muted(bool) = muted,
    }
    beside { id, content }
}

/// The number of the first property of a notes clip's own.
pub const FIRST_MIDI: u8 = 32;
/// The number of the first property of an audio clip's own.
pub const FIRST_AUDIO: u8 = 64;

props! {
    /// What is set of a notes clip beside what every clip has.
    MidiSet of MidiContent from FIRST_MIDI;
    {
        LoopLength(Option<Beats>) = loop_length,
        StartOffset(Beats) = start_offset,
    }
    beside { notes, controllers }
}

props! {
    /// What is set of an audio clip beside what every clip has.
    AudioSet of AudioContent from FIRST_AUDIO;
    {
        Path(String) = path,
        Offset(f64) = offset_seconds,
        Gain(f32) = gain_db,
        FadeIn(Beats) = fade_in,
        FadeOut(Beats) = fade_out,
        Warp(WarpMode) = warp,
        SourceTempo(Option<f64>) = source_tempo,
        Transpose(f64) = transpose,
        Looped(Option<AudioLoop>) = looped,
        Crossfade(bool) = crossfade,
        Is(Option<FileIs>) = is,
    }
    beside { }
}

/// The number of the property that says where an audio clip's file lies:
/// each machine's own, said to nobody and taken from nobody.
pub fn path_prop() -> u8 {
    AudioSet::Path(String::new()).prop()
}

/// The number of the property that says what an audio clip's file is.
pub fn is_prop() -> u8 {
    AudioSet::Is(None).prop()
}

props! {
    /// What is set of a note.
    NoteSet of Note from FIRST_SET;
    {
        Pitch(u8) = pitch,
        Start(Beats) = start,
        Length(Beats) = length,
        Velocity(u8) = velocity,
        Channel(Channel) = channel,
    }
    beside { id }
}

props! {
    /// What is set of a device. Its parameters are set by name, each on
    /// its own (`Op::Param`), and its state is one thing: a plugin's state
    /// is not merged.
    DeviceSet of Device from FIRST_SET;
    {
        Name(String) = name,
        Enabled(bool) = enabled,
        State(Blob) = state,
        Preset(Option<PresetRef>) = preset,
        FromPreset(Option<PresetInfo>) = from_preset,
        Sidechain(Option<TrackId>) = sidechain,
    }
    beside { id, uri, params, files }
}

props! {
    /// What is set of a point of an envelope or of a controller's curve.
    PointSet of AutomationPoint from FIRST_SET;
    {
        Beat(Beats) = beat,
        Value(f64) = value,
        Shape(Segment) = shape,
    }
    beside { id }
}

props! {
    /// What is set of an envelope beside its points.
    LaneSet of AutomationLane from FIRST_SET;
    {
        Travel(Travel) = travel,
        Enabled(bool) = enabled,
    }
    beside { target, points }
}

/// The number of the property that says how a track is grouped: what
/// decides which of two edits that cannot both hold was made first.
pub fn group_prop() -> u8 {
    TrackSet::Group(None).prop()
}

/// The number of the property that says which track a device listens to.
pub fn sidechain_prop() -> u8 {
    DeviceSet::Sidechain(None).prop()
}

/// The number of the property an envelope is taken to have been made
/// with: how its lines travel, which is said when it is made.
pub fn travel_prop() -> u8 {
    LaneSet::Travel(Travel::Linear).prop()
}

/// A track's kind as a number, for whoever orders by it.
pub fn kind_rank(kind: TrackKind) -> u8 {
    match kind {
        TrackKind::Normal => 0,
        TrackKind::Group => 1,
        TrackKind::Return => 2,
    }
}
