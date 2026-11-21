//! What another program says is not trusted.
//!
//! A change that arrives is held to the rules the model's own edits keep
//! before anything of it is taken in ([`Change::checked`]): every number
//! is one the edit method would have taken, every text and state is of a
//! size, every id is one a program gives. What cannot be told of one part
//! alone (that a clip is on a track that holds clips, that nothing waits
//! for itself) is the business of [`Doc::derive`](super::Doc::derive),
//! which makes a song of whatever properties there are.

use super::doc::impersonal;
use super::props::{ClipSet, DeviceSet, LaneSet, MarkerSet, MidiSet, PointSet, SongSet, TrackSet};
use super::{Change, ClipOp, DeviceOp, Home, MAX_ID, MarkerOp, NoteOp, Op, PointOp, Stamp, TrackOp};
use crate::controllers::ControllerKind;
use crate::project::{
    AudioContent, AutomationPoint, Clip, ClipContent, Device, EditError, MAX_VOLUME_DB, MIN_LOOP_SECONDS,
    MIN_VOLUME_DB, Note, Segment, Track, valid_signature, valid_tempo, validate_span, validate_travel,
};
use crate::tempo::SAME_BEAT;

/// The longest text a change may carry: a name, a path, a preset's id.
pub const MAX_TEXT: usize = 4096;
/// The largest state a device may be given.
pub const MAX_STATE_BYTES: usize = 64 << 20;
/// How many parts one change may have, and how many notes or points one
/// part.
pub const MAX_OPS: usize = 200_000;
/// How far above the count this program has seen a change may say it was
/// made: beyond it the count could be run out.
const CLOCK_AHEAD: u64 = 1 << 40;

/// A change that was not taken in, and why.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub String);

fn refused<T>(why: impl Into<String>) -> Result<T, Refused> {
    Err(Refused(why.into()))
}

impl From<EditError> for Refused {
    fn from(error: EditError) -> Self {
        Refused(error.to_string())
    }
}

fn id(id: u64) -> Result<(), Refused> {
    match id {
        1..=MAX_ID => Ok(()),
        _ => refused(format!("{id} is no id a program gives")),
    }
}

fn text(text: &str) -> Result<(), Refused> {
    match text.len() <= MAX_TEXT {
        true => Ok(()),
        false => refused(format!("a text of {} bytes: more than {MAX_TEXT}", text.len())),
    }
}

fn finite(value: f64, what: &str) -> Result<(), Refused> {
    match value.is_finite() {
        true => Ok(()),
        false => refused(format!("{what} is not a number")),
    }
}

fn position(beat: f64, what: &str) -> Result<(), Refused> {
    match beat.is_finite() && beat >= 0.0 {
        true => Ok(()),
        false => refused(format!("{what} is at beat {beat}, which is nowhere in a song")),
    }
}

/// A level as the model keeps one: no higher than a fader goes, and no
/// lower than silence.
fn level(level: f32) -> Result<f32, Refused> {
    match level.is_finite() && level <= MAX_VOLUME_DB {
        true => Ok(level.max(MIN_VOLUME_DB)),
        false => refused(format!("a level of {level} dB")),
    }
}

fn note(note: &Note) -> Result<(), Refused> {
    id(note.id)?;
    Ok(note.validate()?)
}

fn point(point: &AutomationPoint, of: Option<ControllerKind>) -> Result<(), Refused> {
    id(point.id)?;
    position(point.beat, "a point")?;
    value(point.value, of)?;
    shape(point.shape)
}

/// A point's value: a number, and for a controller's curve one the
/// controller has.
fn value(value: f64, of: Option<ControllerKind>) -> Result<(), Refused> {
    finite(value, "a point's value")?;
    if let Some(kind) = of {
        let (low, high) = kind.range();
        if value < low || value > high {
            return refused(format!("{kind} value {value} is outside {low}..={high}"));
        }
    }
    Ok(())
}

fn shape(shape: Segment) -> Result<(), Refused> {
    match shape {
        Segment::Curve(bend) if !bend.is_finite() => refused("a bend that is not a number"),
        _ => Ok(()),
    }
}

fn audio(audio: &AudioContent) -> Result<(), Refused> {
    text(&audio.path)?;
    audio.validate()?;
    if !audio.gain_db.is_finite() {
        return refused("a clip's gain is not a number");
    }
    for fade in [audio.fade_in, audio.fade_out] {
        if !fade.is_finite() || fade < 0.0 {
            return refused(format!("a fade of {fade} beats"));
        }
    }
    looped(audio.looped)
}

fn looped(looped: Option<crate::AudioLoop>) -> Result<(), Refused> {
    match looped {
        Some(looped)
            if !(looped.start_seconds.is_finite() && looped.seconds.is_finite())
                || looped.start_seconds < 0.0
                || looped.seconds < MIN_LOOP_SECONDS =>
        {
            refused(format!("a loop of {} s from {} s", looped.seconds, looped.start_seconds))
        }
        _ => Ok(()),
    }
}

fn loop_length(length: Option<f64>) -> Result<(), Refused> {
    match length {
        Some(length) if !length.is_finite() || length <= 0.0 => refused(format!("a loop of {length} beats")),
        _ => Ok(()),
    }
}

fn start_offset(offset: f64) -> Result<(), Refused> {
    match offset.is_finite() && offset >= 0.0 {
        true => Ok(()),
        false => refused(format!("a clip that begins {offset} beats into its notes")),
    }
}

fn kind(kind: ControllerKind) -> Result<(), Refused> {
    Ok(kind.validate()?)
}

fn clip(clip: &Clip) -> Result<(), Refused> {
    id(clip.id.0)?;
    text(&clip.name)?;
    validate_span(clip.start, clip.length)?;
    match &clip.content {
        ClipContent::Audio(content) => audio(content),
        ClipContent::Midi(midi) => {
            loop_length(midi.loop_length)?;
            start_offset(midi.start_offset)?;
            if midi.notes.len() > MAX_OPS {
                return refused(format!("a clip of {} notes", midi.notes.len()));
            }
            midi.notes.iter().try_for_each(note)?;
            for curve in &midi.controllers {
                kind(curve.controller)?;
                if curve.points.len() > MAX_OPS {
                    return refused(format!("a curve of {} points", curve.points.len()));
                }
                curve.points.iter().try_for_each(|found| point(found, Some(curve.controller)))?;
            }
            Ok(())
        }
    }
}

fn state(state: &Option<Vec<u8>>) -> Result<(), Refused> {
    match state {
        Some(bytes) if bytes.len() > MAX_STATE_BYTES => {
            refused(format!("a state of {} bytes: more than {MAX_STATE_BYTES}", bytes.len()))
        }
        _ => Ok(()),
    }
}

/// Something that is written as it stands: no larger written out than a
/// few texts.
fn small(value: &impl serde::Serialize, what: &str) -> Result<(), Refused> {
    match serde_json::to_string(value) {
        Ok(written) if written.len() <= 64 * MAX_TEXT => Ok(()),
        Ok(written) => refused(format!("{what} of {} bytes", written.len())),
        Err(error) => refused(format!("{what} that cannot be written: {error}")),
    }
}

fn device(device: &Device) -> Result<(), Refused> {
    id(device.id.0)?;
    text(&device.uri)?;
    text(&device.name)?;
    state(&device.state)?;
    small(&device.preset, "a preset")?;
    small(&device.from_preset, "a preset")?;
    if let Some(track) = device.sidechain {
        id(track.0)?;
    }
    if device.params.len() > MAX_OPS {
        return refused(format!("a device with {} parameters set", device.params.len()));
    }
    for (name, value) in &device.params {
        text(name)?;
        finite(*value, "a parameter")?;
    }
    Ok(())
}

fn track(mut track: Track) -> Result<Track, Refused> {
    id(track.id.0)?;
    text(&track.name)?;
    track.volume_db = level(track.volume_db)?;
    pan(track.pan)?;
    if let Some(group) = track.group {
        id(group.0)?;
    }
    for send in &mut track.sends {
        id(send.to.0)?;
        send.level_db = level(send.level_db)?;
    }
    track.profile = track.profile.validated()?;
    small(&track.profile, "what a track's notes mean")?;
    if let Some(study) = track.reference.as_ref().and_then(|reference| reference.study.as_ref()) {
        text(study)?;
    }
    // What a track holds is made apart, and what is one's own is not said.
    (track.clips, track.devices) = (Vec::new(), Vec::new());
    Ok(impersonal(track))
}

fn pan(pan: f32) -> Result<(), Refused> {
    match pan.is_finite() && (-1.0..=1.0).contains(&pan) {
        true => Ok(()),
        false => refused(format!("a pan of {pan}")),
    }
}

impl Op {
    /// This part of a change as the model would have taken it: its values
    /// held to the model's rules, and what the model would have clamped,
    /// clamped. An error for one no edit could have made, and for what is
    /// one's own and nobody else's to say.
    pub fn checked(self) -> Result<Op, Refused> {
        if self.is_personal() {
            return refused("what is one's own listening is not said to anyone else");
        }
        Ok(match self {
            Op::Song(set) => {
                match &set {
                    SongSet::Name(name) => text(name)?,
                    SongSet::Tempo(tempo) => drop(valid_tempo(*tempo)?),
                    SongSet::TimeSignature(signature) => {
                        valid_signature(signature.numerator, signature.denominator)?;
                    }
                    SongSet::Key(Some(key)) if key.tonic > 11 => return refused("a key that is none of the twelve"),
                    SongSet::Key(_) | SongSet::Loop(_) => {}
                }
                Op::Song(set)
            }
            Op::MasterVolume(volume) => Op::MasterVolume(level(volume)?),
            Op::Tempo(beat, ramp, tempo) => {
                if !beat.is_finite() || beat <= SAME_BEAT {
                    return refused("a change of tempo lies after the start of the song");
                }
                if let Some(tempo) = tempo {
                    valid_tempo(tempo)?;
                }
                Op::Tempo(beat, ramp, tempo)
            }
            Op::Meter(beat, signature) => {
                if !beat.is_finite() || beat <= SAME_BEAT {
                    return refused("a change of time signature lies after the start of the song");
                }
                if let Some(signature) = signature {
                    valid_signature(signature.numerator, signature.denominator)?;
                }
                Op::Meter(beat, signature)
            }
            Op::Marker(marker, part) => {
                id(marker)?;
                match &part {
                    MarkerOp::Whole(whole) => {
                        position(whole.beat, "a marker")?;
                        text(&whole.name)?;
                    }
                    MarkerOp::Set(MarkerSet::Beat(beat)) => position(*beat, "a marker")?,
                    MarkerOp::Set(MarkerSet::Name(name)) => text(name)?,
                    MarkerOp::Alive(_) => {}
                }
                Op::Marker(marker, part)
            }
            Op::Track(of, part) => {
                id(of.0)?;
                let part = match part {
                    TrackOp::Whole(whole, place) => {
                        finite(place, "a place")?;
                        TrackOp::Whole(Box::new(track(*whole)?), place)
                    }
                    TrackOp::Place(place) => {
                        finite(place, "a place")?;
                        TrackOp::Place(place)
                    }
                    TrackOp::Reference(beside) => {
                        if let Some(study) = beside.as_ref().and_then(|beside| beside.study.as_ref()) {
                            text(study)?;
                        }
                        TrackOp::Reference(beside)
                    }
                    TrackOp::Set(set) => TrackOp::Set(match set {
                        TrackSet::Name(name) => {
                            text(&name)?;
                            TrackSet::Name(name)
                        }
                        TrackSet::Color(color) => TrackSet::Color(color & 0xFF_FFFF),
                        TrackSet::Volume(volume) => TrackSet::Volume(level(volume)?),
                        TrackSet::Pan(value) => {
                            pan(value)?;
                            TrackSet::Pan(value)
                        }
                        TrackSet::Group(group) => {
                            if let Some(group) = group {
                                id(group.0)?;
                            }
                            TrackSet::Group(group)
                        }
                        TrackSet::Profile(profile) => {
                            let profile = profile.validated()?;
                            small(&profile, "what a track's notes mean")?;
                            TrackSet::Profile(profile)
                        }
                        set @ (TrackSet::Mute(_) | TrackSet::Solo(_)) => set,
                    }),
                    part @ (TrackOp::Alive(_) | TrackOp::Listening(_)) => part,
                };
                Op::Track(of, part)
            }
            Op::Send(from, to, sent) => {
                id(from.0)?;
                id(to.0)?;
                Op::Send(from, to, sent.map(level).transpose()?)
            }
            Op::Clip(of, part) => {
                id(of.0)?;
                match &part {
                    ClipOp::Whole(whole, on) => {
                        id(on.0)?;
                        clip(whole)?;
                    }
                    ClipOp::Track(on) => id(on.0)?,
                    ClipOp::Alive(_) | ClipOp::Set(ClipSet::Muted(_)) => {}
                    ClipOp::Set(ClipSet::Name(name)) => text(name)?,
                    ClipOp::Set(ClipSet::Start(start)) => position(*start, "a clip")?,
                    ClipOp::Set(ClipSet::Length(length)) => validate_span(0.0, *length)?,
                    ClipOp::Midi(MidiSet::LoopLength(length)) => loop_length(*length)?,
                    ClipOp::Midi(MidiSet::StartOffset(offset)) => start_offset(*offset)?,
                    ClipOp::Audio(set) => {
                        // Held to what an audio clip may be, as one that has
                        // nothing else set.
                        let mut alone = AudioContent::default();
                        set.clone().put(&mut alone);
                        audio(&alone)?;
                    }
                }
                Op::Clip(of, part)
            }
            Op::Notes(of, notes) => {
                id(of.0)?;
                if notes.len() > MAX_OPS {
                    return refused(format!("{} notes at once", notes.len()));
                }
                notes.iter().try_for_each(note)?;
                Op::Notes(of, notes)
            }
            Op::Note(of, which, part) => {
                id(of.0)?;
                id(which)?;
                if let NoteOp::Set(set) = &part {
                    // Held to what a note may be, as one that has nothing
                    // else set.
                    let mut alone = Note::default();
                    set.clone().put(&mut alone);
                    alone.validate()?;
                }
                Op::Note(of, which, part)
            }
            Op::Device(of, part) => {
                id(of.0)?;
                match &part {
                    DeviceOp::Whole(whole, _, place) => {
                        finite(*place, "a place")?;
                        device(whole)?;
                    }
                    DeviceOp::At(_, place) => finite(*place, "a place")?,
                    DeviceOp::Alive(_) | DeviceOp::Set(DeviceSet::Enabled(_)) => {}
                    DeviceOp::Set(DeviceSet::Name(name)) => text(name)?,
                    DeviceOp::Set(DeviceSet::State(blob)) => state(&blob.0)?,
                    DeviceOp::Set(DeviceSet::Preset(preset)) => small(preset, "a preset")?,
                    DeviceOp::Set(DeviceSet::FromPreset(preset)) => small(preset, "a preset")?,
                    DeviceOp::Set(DeviceSet::Sidechain(to)) => {
                        if let Some(to) = to {
                            id(to.0)?;
                        }
                    }
                }
                if let DeviceOp::Whole(_, crate::ChainRef::Track(on), _) | DeviceOp::At(crate::ChainRef::Track(on), _) =
                    &part
                {
                    id(on.0)?;
                }
                Op::Device(of, part)
            }
            Op::Param(of, name, set) => {
                id(of.0)?;
                text(&name)?;
                if let Some(set) = set {
                    finite(set, "a parameter")?;
                }
                Op::Param(of, name, set)
            }
            Op::Lane(target, set) => {
                small(&target, "what an envelope moves")?;
                if let LaneSet::Travel(travel) = &set {
                    validate_travel(*travel)?;
                }
                Op::Lane(target, set)
            }
            Op::Points(home, points) => {
                let of = home_kind(&home)?;
                if points.len() > MAX_OPS {
                    return refused(format!("{} points at once", points.len()));
                }
                points.iter().try_for_each(|found| point(found, of))?;
                Op::Points(home, points)
            }
            Op::Point(home, which, part) => {
                let of = home_kind(&home)?;
                id(which)?;
                match &part {
                    PointOp::Set(PointSet::Beat(beat)) => position(*beat, "a point")?,
                    PointOp::Set(PointSet::Value(found)) => value(*found, of)?,
                    PointOp::Set(PointSet::Shape(found)) => shape(*found)?,
                    PointOp::Alive(_) => {}
                }
                Op::Point(home, which, part)
            }
        })
    }
}

/// The controller a home's points are a curve of, for a curve.
fn home_kind(home: &Home) -> Result<Option<ControllerKind>, Refused> {
    match home {
        Home::Lane(target) => {
            small(target, "what an envelope moves")?;
            Ok(None)
        }
        Home::Curve(clip, _, controller) => {
            id(clip.0)?;
            kind(*controller)?;
            Ok(Some(*controller))
        }
    }
}

impl Change {
    /// The change as the model would have taken it ([`Op::checked`]), for
    /// one that another program says. `clock` is the highest count seen
    /// here: a change that says it was made far above it is refused.
    pub fn checked(self, clock: u64) -> Result<Change, Refused> {
        let Stamp(count, _) = self.stamp;
        if self.stamp == Stamp::ZERO || count > clock.saturating_add(CLOCK_AHEAD) {
            return refused(format!("a change that says it was made at count {count}"));
        }
        if self.ops.len() > MAX_OPS {
            return refused(format!("a change of {} parts", self.ops.len()));
        }
        let ops = self.ops.into_iter().map(Op::checked).collect::<Result<Vec<Op>, Refused>>()?;
        Ok(Change { stamp: self.stamp, ops })
    }
}
