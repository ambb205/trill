//! Standard MIDI Files (`.mid`): reading the notes and the controllers out
//! of one, and writing them to one.
//!
//! Only what a song needs is kept: notes with their timing in beats, the
//! movement of controllers, the pitch wheel and channel pressure, track
//! names, and the tempo and the time signature with every change of them.
//! Program changes
//! and key pressure are skipped, and so are the controllers that are not
//! movements but messages: bank select, data entry, the registered and
//! non-registered parameter numbers, and the fine halves of fourteen-bit
//! controllers (32 to 63).
//!
//! A file holds a controller as the values it took, one event at a time. A
//! clip holds it as a curve. Reading thins the events to breakpoints
//! ([`controllers::thin`]); writing samples the curve at every tick and
//! writes an event wherever its value, in the file's resolution, changes.
//!
//! A file's tempo only jumps. A tempo that glides is written as the jumps
//! that keep its time: [`GLIDE_STEPS_PER_BEAT`] to a beat, each at the
//! tempo that makes its stretch last as long as the glide does there. Read
//! back, it is those jumps.

use crate::controllers::{self, ControllerCurve, ControllerKind, GESTURE_GAP_SECONDS, THIN_TOLERANCE};
use crate::project::Note;
use crate::tempo::{MeterChange, MeterMap, SAME_BEAT, TempoChange, TempoMap};
use crate::time::TimeSignature;
use std::collections::BTreeMap;

/// Ticks per quarter note in files this module writes.
const WRITE_DIVISION: u16 = 480;
/// The same, for those who sample a curve the way a written file will.
pub(crate) const TICKS_PER_BEAT: f64 = WRITE_DIVISION as f64;
/// The tempo a file is taken to have until it says otherwise.
const DEFAULT_TEMPO: f64 = 120.0;
/// How many jumps a beat of a gliding tempo is written as.
pub const GLIDE_STEPS_PER_BEAT: u32 = 8;
/// The General MIDI drum channel (channel 10, counted from zero).
const DRUM_CHANNEL: u8 = 9;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum MidiFileError {
    #[error("not a MIDI file")]
    NotMidi,
    #[error("the MIDI file is cut short or damaged")]
    Truncated,
    #[error("unsupported MIDI file: {0}")]
    Unsupported(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MidiFile {
    /// The first tempo in the file, in beats per minute.
    pub tempo: Option<f64>,
    /// The first time signature in the file.
    pub time_signature: Option<TimeSignature>,
    /// Where the tempo changes after its first, in beats from the start
    /// of the file and in their order. A file read has only jumps.
    pub tempo_changes: Vec<TempoChange>,
    /// Where the time signature changes after its first, likewise.
    pub meter_changes: Vec<MeterChange>,
    pub tracks: Vec<MidiTrack>,
}

/// What a file's tracks say of its time, gathered while they are read:
/// each tempo and each time signature with the tick it is set at.
#[derive(Default)]
struct Conducted {
    tempos: Vec<(u64, f64)>,
    signatures: Vec<(u64, TimeSignature)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MidiTrack {
    pub name: String,
    /// Whether the notes are on the General MIDI drum channel.
    pub drums: bool,
    /// Sorted by start; positions are in beats from the start of the file.
    pub notes: Vec<Note>,
    /// The controllers the track moves, one curve each in the order of
    /// [`ControllerKind`], over beats from the start of the file.
    pub controllers: Vec<ControllerCurve>,
}

impl MidiFile {
    /// The end of the last note, in beats.
    pub fn length_beats(&self) -> f64 {
        self.tracks.iter().flat_map(|track| track.notes.iter()).map(Note::end).fold(0.0, f64::max)
    }
}

/// Bounds-checked reader over the file's bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], MidiFileError> {
        let end = self.at.checked_add(count).filter(|end| *end <= self.bytes.len()).ok_or(MidiFileError::Truncated)?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, MidiFileError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, MidiFileError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, MidiFileError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A variable-length quantity: seven bits per byte, high bit set on all
    /// but the last.
    fn variable(&mut self) -> Result<u32, MidiFileError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.u8()?;
            value = (value << 7) | (byte & 0x7F) as u32;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(MidiFileError::Truncated)
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }
}

/// A note-on waiting for its note-off.
struct Sounding {
    start: u64,
    velocity: u8,
}

/// Reads a Standard MIDI File (format 0 or 1).
pub fn parse(bytes: &[u8]) -> Result<MidiFile, MidiFileError> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(4).map_err(|_| MidiFileError::NotMidi)? != b"MThd" {
        return Err(MidiFileError::NotMidi);
    }
    let header_length = reader.u32()? as usize;
    let mut header = Reader { bytes: reader.take(header_length)?, at: 0 };
    let format = header.u16()?;
    let _track_count = header.u16()?;
    let division = header.u16()?;
    if format > 1 {
        return Err(MidiFileError::Unsupported(format!("format {format} (only 0 and 1 are read)")));
    }
    if division & 0x8000 != 0 || division == 0 {
        return Err(MidiFileError::Unsupported("timing in frames per second rather than beats".into()));
    }
    let ticks_per_beat = division as f64;

    let mut file = MidiFile::default();
    let mut conducted = Conducted::default();
    while !reader.done() {
        let kind = reader.take(4)?;
        let length = reader.u32()? as usize;
        let chunk = reader.take(length)?;
        // Chunks of other kinds are allowed and skipped.
        if kind == b"MTrk" {
            parse_track(chunk, ticks_per_beat, &mut file, &mut conducted)?;
        }
    }

    // The first of each is what the file begins with, wherever it stands;
    // the later ones are its changes, and of two at one moment the later
    // one counts.
    conducted.tempos.sort_by_key(|(tick, _)| *tick);
    file.tempo = conducted.tempos.first().map(|(_, tempo)| *tempo);
    for (tick, tempo) in conducted.tempos.into_iter().skip(1) {
        let beat = tick as f64 / ticks_per_beat;
        match file.tempo_changes.last_mut() {
            _ if beat <= SAME_BEAT => file.tempo = Some(tempo),
            Some(last) if beat - last.beat <= SAME_BEAT => last.tempo = tempo,
            _ => file.tempo_changes.push(TempoChange { beat, tempo, ramp: false }),
        }
    }
    conducted.signatures.sort_by_key(|(tick, _)| *tick);
    file.time_signature = conducted.signatures.first().map(|(_, signature)| *signature);
    for (tick, signature) in conducted.signatures.into_iter().skip(1) {
        let beat = tick as f64 / ticks_per_beat;
        match file.meter_changes.last_mut() {
            _ if beat <= SAME_BEAT => file.time_signature = Some(signature),
            Some(last) if beat - last.beat <= SAME_BEAT => last.signature = signature,
            _ => file.meter_changes.push(MeterChange { beat, signature }),
        }
    }
    // A change that says what was already so is none: a tempo that is
    // the tempo, and a signature that is the signature where a bar of it
    // begins anyway.
    let mut tempo = file.tempo.unwrap_or(DEFAULT_TEMPO);
    file.tempo_changes.retain(|change| std::mem::replace(&mut tempo, change.tempo) != change.tempo);
    let mut said: Vec<MeterChange> = Vec::with_capacity(file.meter_changes.len());
    for change in std::mem::take(&mut file.meter_changes) {
        let bars = MeterMap::new(file.time_signature.unwrap_or_default(), &said);
        let bar = bars.bar_at(change.beat);
        if bar.signature != change.signature || (change.beat - bar.start).abs() > SAME_BEAT {
            said.push(change);
        }
    }
    file.meter_changes = said;
    Ok(file)
}

fn parse_track(
    chunk: &[u8],
    ticks_per_beat: f64,
    file: &mut MidiFile,
    conducted: &mut Conducted,
) -> Result<(), MidiFileError> {
    let mut reader = Reader { bytes: chunk, at: 0 };
    let mut tick = 0u64;
    let mut status = 0u8;
    let mut name = String::new();
    // Per channel: the notes finished so far, and those still sounding by pitch.
    let mut notes: BTreeMap<u8, Vec<Note>> = BTreeMap::new();
    let mut sounding: BTreeMap<(u8, u8), Vec<Sounding>> = BTreeMap::new();
    // Per channel and controller: where it was put, and when, in beats.
    let mut moves: BTreeMap<(u8, ControllerKind), Vec<(f64, f64)>> = BTreeMap::new();

    let finish = |channel: u8, pitch: u8, started: Sounding, end: u64, notes: &mut BTreeMap<u8, Vec<Note>>| {
        let length = (end.saturating_sub(started.start).max(1)) as f64 / ticks_per_beat;
        notes.entry(channel).or_default().push(Note {
            pitch,
            start: started.start as f64 / ticks_per_beat,
            length,
            velocity: started.velocity,
        });
    };

    while !reader.done() {
        tick += reader.variable()? as u64;
        let first = reader.u8()?;
        // A data byte here means "same status as the previous event".
        let data = if first & 0x80 != 0 {
            status = first;
            None
        } else {
            Some(first)
        };
        match status {
            0xFF => {
                let kind = reader.u8()?;
                let length = reader.variable()? as usize;
                let payload = reader.take(length)?;
                match kind {
                    0x03 if name.is_empty() => name = String::from_utf8_lossy(payload).trim().to_string(),
                    0x51 if payload.len() == 3 => {
                        let micros = u32::from_be_bytes([0, payload[0], payload[1], payload[2]]);
                        if micros > 0 {
                            // The file stores whole microseconds per beat, so 132 BPM
                            // comes back as 132.000132; a thousandth of a BPM is
                            // finer than any tempo a person sets.
                            let tempo = (60_000_000_000.0 / micros as f64).round() / 1000.0;
                            file.tempo.get_or_insert(tempo);
                            conducted.tempos.push((tick, tempo));
                        }
                    }
                    0x58 if payload.len() >= 2 => {
                        let denominator = 1u32.checked_shl(payload[1] as u32).unwrap_or(0);
                        let signature = u8::try_from(denominator).ok().and_then(|d| TimeSignature::new(payload[0], d));
                        // One this program has no bars for is passed over:
                        // the signature before it goes on.
                        if let Some(signature) = signature {
                            file.time_signature.get_or_insert(signature);
                            conducted.signatures.push((tick, signature));
                        }
                    }
                    0x2F => break,
                    _ => {}
                }
                // Meta events cancel running status.
                status = 0;
            }
            0xF0 | 0xF7 => {
                let length = reader.variable()? as usize;
                reader.take(length)?;
                status = 0;
            }
            0x80..=0xEF => {
                let first_data = match data {
                    Some(byte) => byte,
                    None => reader.u8()?,
                };
                let two_bytes = !matches!(status & 0xF0, 0xC0 | 0xD0);
                let second_data = if two_bytes { reader.u8()? } else { 0 };
                let channel = status & 0x0F;
                let pitch = first_data & 0x7F;
                let velocity = second_data & 0x7F;
                match status & 0xF0 {
                    0x90 if velocity > 0 => {
                        sounding.entry((channel, pitch)).or_default().push(Sounding { start: tick, velocity });
                    }
                    // A note-on with velocity zero is a note-off.
                    0x80 | 0x90 => {
                        if let Some(queue) = sounding.get_mut(&(channel, pitch))
                            && !queue.is_empty()
                        {
                            let started = queue.remove(0);
                            finish(channel, pitch, started, tick, &mut notes);
                        }
                    }
                    kind @ (0xB0 | 0xD0 | 0xE0) => {
                        let moved = match kind {
                            0xB0 => read_controller(first_data & 0x7F)
                                .map(|controller| (controller, f64::from(second_data & 0x7F) / 127.0)),
                            0xD0 => Some((ControllerKind::Pressure, f64::from(first_data & 0x7F) / 127.0)),
                            _ => {
                                let raw = i32::from(first_data & 0x7F) | i32::from(second_data & 0x7F) << 7;
                                Some((ControllerKind::PitchBend, bend_value(raw)))
                            }
                        };
                        if let Some((controller, value)) = moved {
                            moves.entry((channel, controller)).or_default().push((tick as f64 / ticks_per_beat, value));
                        }
                    }
                    _ => {}
                }
            }
            _ => return Err(MidiFileError::Truncated),
        }
    }

    // Notes never released end where the track does.
    for ((channel, pitch), queue) in sounding {
        for started in queue {
            finish(channel, pitch, started, tick, &mut notes);
        }
    }

    // How long a controller rests before its next movement is a new one.
    let gap = GESTURE_GAP_SECONDS * file.tempo.unwrap_or(DEFAULT_TEMPO) / 60.0;
    let several = notes.len() > 1;
    for (channel, mut notes) in notes {
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        let name = match (name.is_empty(), several) {
            (true, _) => format!("Channel {}", channel + 1),
            (false, true) => format!("{name} ({})", channel + 1),
            (false, false) => name.clone(),
        };
        let controllers = moves
            .range((channel, ControllerKind::Controller(0))..=(channel, ControllerKind::PitchBend))
            .map(|((_, controller), events)| ControllerCurve {
                controller: *controller,
                points: controllers::thin(events, controller.is_switch(), THIN_TOLERANCE, gap),
            })
            .filter(|curve| !curve.points.is_empty())
            .collect();
        file.tracks.push(MidiTrack { name, drums: channel == DRUM_CHANNEL, notes, controllers });
    }
    Ok(())
}

/// The controller a control change with `number` moves, if it is one a
/// clip keeps: see the module documentation for those that are not.
fn read_controller(number: u8) -> Option<ControllerKind> {
    match number {
        0 | 6 | 32..=63 | 96..=101 => None,
        number => ControllerKind::numbered(number),
    }
}

/// The pitch wheel's fourteen bits as -1..=1. The top of the range is one
/// step short of the bottom's, so each side is scaled to reach exactly 1.
fn bend_value(raw: i32) -> f64 {
    let centred = raw - 8192;
    if centred >= 0 { f64::from(centred) / 8191.0 } else { f64::from(centred) / 8192.0 }
}

/// A controller's value as the bytes of the event that sets it on
/// `channel`, and as a number to tell one such value from the next.
fn controller_bytes(kind: ControllerKind, channel: u8, value: f64) -> (u32, [u8; 3], usize) {
    let seven = |value: f64| (value.clamp(0.0, 1.0) * 127.0).round() as u8;
    match kind {
        ControllerKind::Controller(number) => {
            (u32::from(seven(value)), [0xB0 | channel, number & 0x7F, seven(value)], 3)
        }
        ControllerKind::Pressure => (u32::from(seven(value)), [0xD0 | channel, seven(value), 0], 2),
        ControllerKind::PitchBend => {
            let value = value.clamp(-1.0, 1.0);
            let raw = (8192.0 + if value >= 0.0 { value * 8191.0 } else { value * 8192.0 }).round() as u32;
            (raw, [0xE0 | channel, (raw & 0x7F) as u8, (raw >> 7) as u8 & 0x7F], 3)
        }
    }
}

/// The events that make a file's reader hear `curve`: one wherever its
/// value, in the file's resolution, changes from one tick to the next.
fn controller_events(curve: &ControllerCurve, channel: u8, out: &mut Vec<(u64, u8, [u8; 3], usize)>) {
    let Some(last) = curve.points.last() else { return };
    let ticks = (last.beat.max(0.0) * TICKS_PER_BEAT).round() as u64;
    // A reader takes what springs back to be at rest already.
    let mut written = curve.controller.rest().map(|rest| controller_bytes(curve.controller, channel, rest).0);
    for tick in 0..=ticks {
        let value = curve.value_at(tick as f64 / TICKS_PER_BEAT);
        let (step, bytes, length) = controller_bytes(curve.controller, channel, value);
        if written != Some(step) {
            written = Some(step);
            out.push((tick, CONTROLLER_ORDER, bytes, length));
        }
    }
}

/// Where the kinds of event go when they share a tick: a note is let go
/// of before a controller moves, and a controller has moved before a note
/// is struck, so that a note begins at the level meant for it.
const NOTE_OFF_ORDER: u8 = 0;
const CONTROLLER_ORDER: u8 = 1;
const NOTE_ON_ORDER: u8 = 2;

fn write_variable(out: &mut Vec<u8>, value: u32) {
    let mut groups = [0u8; 5];
    let mut count = 0;
    let mut rest = value;
    loop {
        groups[count] = (rest & 0x7F) as u8;
        count += 1;
        rest >>= 7;
        if rest == 0 {
            break;
        }
    }
    for index in (0..count).rev() {
        out.push(groups[index] | if index > 0 { 0x80 } else { 0 });
    }
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(kind);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

fn meta(body: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    meta_after(body, 0, kind, payload);
}

/// A meta event `delta` ticks after the event before it.
fn meta_after(body: &mut Vec<u8>, delta: u64, kind: u8, payload: &[u8]) {
    write_variable(body, delta.min(0x0FFF_FFFF) as u32);
    body.extend_from_slice(&[0xFF, kind]);
    write_variable(body, payload.len() as u32);
    body.extend_from_slice(payload);
}

/// A tempo as a file says it: whole microseconds to the beat.
fn tempo_bytes(tempo: f64) -> [u8; 3] {
    let micros = (60_000_000.0 / tempo).round().clamp(1.0, 16_777_215.0) as u32;
    let bytes = micros.to_be_bytes();
    [bytes[1], bytes[2], bytes[3]]
}

/// The jumps that keep the time of a tempo that begins at `first` and has
/// `changes`: each jump as it is, and each glide as
/// [`GLIDE_STEPS_PER_BEAT`] jumps to the beat.
fn tempo_jumps(first: f64, changes: &[TempoChange]) -> Vec<(u64, f64)> {
    let to_tick = |beat: f64| (beat.max(0.0) * TICKS_PER_BEAT).round() as u64;
    let map = TempoMap::new(first, changes);
    let mut jumps = vec![(0u64, first)];
    let mut from = 0.0;
    for change in changes {
        if change.ramp && change.beat > from {
            let steps = ((change.beat - from) * f64::from(GLIDE_STEPS_PER_BEAT)).ceil().max(1.0) as u32;
            let edge = |step: u32| from + (change.beat - from) * f64::from(step) / f64::from(steps);
            for step in 0..steps {
                // The tempo at which this stretch lasts as long as the
                // glide does over it.
                let (start, end) = (edge(step), edge(step + 1));
                jumps.push((to_tick(start), (end - start) * 60.0 / map.seconds_between(start, end)));
            }
        }
        jumps.push((to_tick(change.beat), change.tempo));
        from = change.beat;
    }
    // Of several at one tick the last is the one a reader keeps.
    let mut kept: Vec<(u64, f64)> = Vec::with_capacity(jumps.len());
    for jump in jumps {
        match kept.last_mut() {
            Some(last) if last.0 == jump.0 => *last = jump,
            _ => kept.push(jump),
        }
    }
    kept
}

/// Writes a format 1 Standard MIDI File: a tempo track, then one track per
/// [`MidiTrack`].
pub fn write(file: &MidiFile) -> Vec<u8> {
    let mut out = Vec::new();
    let mut header = Vec::new();
    header.extend_from_slice(&1u16.to_be_bytes());
    header.extend_from_slice(&((file.tracks.len() + 1) as u16).to_be_bytes());
    header.extend_from_slice(&WRITE_DIVISION.to_be_bytes());
    write_chunk(&mut out, b"MThd", &header);

    // The conductor: every time signature and every tempo where it takes
    // over, a signature before a tempo where they share a tick.
    let to_tick = |beats: f64| (beats.max(0.0) * WRITE_DIVISION as f64).round() as u64;
    let signature_bytes = |signature: TimeSignature| {
        // Twenty-four clocks to the click, eight thirty-seconds to the quarter.
        [signature.numerator, signature.denominator.trailing_zeros() as u8, 24, 8]
    };
    let mut events: Vec<(u64, u8, Vec<u8>)> = Vec::new();
    if let Some(signature) = file.time_signature {
        events.push((0, 0x58, signature_bytes(signature).to_vec()));
    }
    let changes = file.meter_changes.iter();
    events.extend(changes.map(|change| (to_tick(change.beat), 0x58, signature_bytes(change.signature).to_vec())));
    if let Some(tempo) = file.tempo.filter(|tempo| *tempo > 0.0) {
        let jumps = tempo_jumps(tempo, &file.tempo_changes);
        events.extend(jumps.into_iter().map(|(tick, tempo)| (tick, 0x51, tempo_bytes(tempo).to_vec())));
    }
    events.sort_by_key(|(tick, kind, _)| (*tick, std::cmp::Reverse(*kind)));
    let mut conductor = Vec::new();
    let mut last = 0u64;
    for (tick, kind, payload) in events {
        meta_after(&mut conductor, tick - last, kind, &payload);
        last = tick;
    }
    meta(&mut conductor, 0x2F, &[]);
    write_chunk(&mut out, b"MTrk", &conductor);

    for track in &file.tracks {
        let channel = if track.drums { DRUM_CHANNEL } else { 0 };
        // (tick, order within the tick, bytes, how many of them); at one
        // tick, offs go first so a repeated note is released before it is
        // struck again.
        let mut events: Vec<(u64, u8, [u8; 3], usize)> = Vec::with_capacity(track.notes.len() * 2);
        for note in &track.notes {
            let start = to_tick(note.start);
            let end = to_tick(note.end()).max(start + 1);
            events.push((start, NOTE_ON_ORDER, [0x90 | channel, note.pitch & 0x7F, note.velocity & 0x7F], 3));
            events.push((end, NOTE_OFF_ORDER, [0x80 | channel, note.pitch & 0x7F, 0], 3));
        }
        for curve in &track.controllers {
            controller_events(curve, channel, &mut events);
        }
        // Stable, so a controller's events keep the order they were made in.
        events.sort_by_key(|&(tick, order, bytes, _)| {
            (tick, order, if order == CONTROLLER_ORDER { 0 } else { bytes[1] })
        });

        let mut body = Vec::new();
        meta(&mut body, 0x03, track.name.as_bytes());
        let mut last = 0u64;
        for (tick, _, bytes, length) in events {
            write_variable(&mut body, (tick - last).min(0x0FFF_FFFF) as u32);
            last = tick;
            body.extend_from_slice(&bytes[..length]);
        }
        meta(&mut body, 0x2F, &[]);
        write_chunk(&mut out, b"MTrk", &body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{AutomationPoint, Segment};

    fn note(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
        Note { pitch, start, length, velocity }
    }

    #[test]
    fn what_is_written_reads_back_the_same() {
        let file = MidiFile {
            tempo: Some(96.0),
            time_signature: TimeSignature::new(6, 8),
            tempo_changes: Vec::new(),
            meter_changes: Vec::new(),
            tracks: vec![
                MidiTrack {
                    name: "Lead".into(),
                    drums: false,
                    notes: vec![
                        note(60, 0.0, 1.0, 100),
                        note(64, 1.0, 0.5, 80),
                        note(64, 1.5, 0.25, 127),
                        note(67, 4.0, 2.0, 1),
                    ],
                    controllers: Vec::new(),
                },
                MidiTrack {
                    name: "Drums".into(),
                    drums: true,
                    notes: vec![note(36, 0.0, 0.25, 110), note(38, 1.0, 0.25, 90)],
                    controllers: Vec::new(),
                },
            ],
        };
        let parsed = parse(&write(&file)).unwrap();
        assert_eq!(parsed, file);
        assert_eq!(parsed.length_beats(), 6.0);
    }

    #[test]
    fn reads_running_status_velocity_zero_offs_and_tempo() {
        // Format 0, 96 ticks per beat. Tempo 500000 us/beat (120 BPM), then
        // on channel 1: C4 on, (running status) E4 on, both released with
        // velocity-zero note-ons, one beat and two beats later.
        let mut track = Vec::new();
        track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);
        track.extend_from_slice(&[0x00, 0x90, 60, 100]);
        track.extend_from_slice(&[0x00, 64, 90]);
        track.extend_from_slice(&[0x60, 60, 0]);
        track.extend_from_slice(&[0x60, 64, 0]);
        track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        let mut bytes = Vec::new();
        write_chunk(&mut bytes, b"MThd", &[0, 0, 0, 1, 0, 96]);
        write_chunk(&mut bytes, b"MTrk", &track);

        let file = parse(&bytes).unwrap();
        assert_eq!(file.tempo, Some(120.0));
        assert_eq!(file.tracks.len(), 1);
        assert_eq!(file.tracks[0].name, "Channel 1");
        assert_eq!(file.tracks[0].notes, [note(60, 0.0, 1.0, 100), note(64, 0.0, 2.0, 90)]);
    }

    #[test]
    fn channels_in_one_track_become_separate_tracks_and_unreleased_notes_end() {
        let mut track = Vec::new();
        track.extend_from_slice(&[0x00, 0xFF, 0x03, 0x04, b'B', b'a', b'n', b'd']);
        track.extend_from_slice(&[0x00, 0x90, 48, 100]);
        track.extend_from_slice(&[0x00, 0x99, 36, 120]);
        // A program change, a bank select and key pressure in between
        // must be skipped.
        track.extend_from_slice(&[0x10, 0xB0, 0, 3]);
        track.extend_from_slice(&[0x00, 0xC0, 5]);
        track.extend_from_slice(&[0x00, 0xA0, 48, 90]);
        track.extend_from_slice(&[0x50, 0x89, 36, 0]);
        track.extend_from_slice(&[0x60, 0xFF, 0x2F, 0x00]);
        let mut bytes = Vec::new();
        write_chunk(&mut bytes, b"MThd", &[0, 0, 0, 1, 0, 96]);
        write_chunk(&mut bytes, b"MTrk", &track);

        let file = parse(&bytes).unwrap();
        assert_eq!(file.tracks.len(), 2);
        assert_eq!((file.tracks[0].name.as_str(), file.tracks[0].drums), ("Band (1)", false));
        assert_eq!(file.tracks[0].notes, [note(48, 0.0, 2.0, 100)], "held to the end of the track");
        assert_eq!((file.tracks[1].name.as_str(), file.tracks[1].drums), ("Band (10)", true));
        assert_eq!(file.tracks[1].notes, [note(36, 0.0, 1.0, 120)]);
    }

    #[test]
    fn every_tempo_and_time_signature_of_a_file_is_read_and_written() {
        let jump = |beat: f64, tempo: f64| TempoChange { beat, tempo, ramp: false };
        let meter = |beat: f64, numerator: u8, denominator: u8| MeterChange {
            beat,
            signature: TimeSignature::new(numerator, denominator).unwrap(),
        };
        let file = MidiFile {
            tempo: Some(120.0),
            time_signature: TimeSignature::new(4, 4),
            tempo_changes: vec![jump(8.0, 140.0), jump(15.0, 96.5), jump(17.25, 120.0)],
            meter_changes: vec![meter(8.0, 7, 8), meter(15.0, 3, 4), meter(16.0, 3, 4), meter(19.0, 4, 4)],
            tracks: vec![MidiTrack { name: "Lead".into(), notes: vec![note(60, 0.0, 1.0, 100)], ..Default::default() }],
        };
        let bytes = write(&file);
        let read = parse(&bytes).unwrap();
        // The 3/4 said again where a bar of 3/4 would not have begun
        // (a pickup of one beat) is kept, for a bar begins there.
        assert_eq!(read, file);
        assert_eq!(write(&read), bytes, "and it goes out as it came in");

        // A tempo or a signature said again where it changes nothing is
        // none, and of two at one moment the later counts.
        let repeated = MidiFile {
            tempo_changes: vec![
                jump(4.0, 120.0),
                jump(8.0, 140.0),
                jump(12.0, 140.0),
                jump(15.0, 96.5),
                jump(17.25, 120.0),
            ],
            meter_changes: vec![
                meter(4.0, 4, 4),
                meter(8.0, 7, 8),
                meter(11.5, 7, 8),
                meter(15.0, 3, 4),
                meter(16.0, 3, 4),
                meter(19.0, 4, 4),
            ],
            ..file.clone()
        };
        assert_eq!(parse(&write(&repeated)).unwrap(), file);

        // A tempo that glides is written as the jumps that keep its time:
        // eight to the beat, and the song is as long as it was.
        let gliding = MidiFile {
            tempo_changes: vec![jump(4.0, 120.0), TempoChange { beat: 8.0, tempo: 180.0, ramp: true }],
            meter_changes: Vec::new(),
            ..file.clone()
        };
        let read = parse(&write(&gliding)).unwrap();
        assert_eq!(read.tempo_changes.len(), 4 * GLIDE_STEPS_PER_BEAT as usize + 1, "and the tempo it arrives at");
        assert!(read.tempo_changes.iter().all(|change| !change.ramp));
        let (meant, kept) = (TempoMap::new(120.0, &gliding.tempo_changes), TempoMap::new(120.0, &read.tempo_changes));
        for beat in [4.0, 5.0, 6.5, 8.0, 12.0] {
            let apart = (meant.seconds_at(beat) - kept.seconds_at(beat)).abs();
            assert!(apart < 2e-5, "at beat {beat} the file is {apart} s from the glide");
        }
        assert!((kept.tempo_at(100.0) - 180.0).abs() < 1e-9);

        // A file whose first tempo comes late begins with it, and a
        // signature there are no bars for is passed over.
        let mut conductor = Vec::new();
        meta_after(&mut conductor, 0, 0x58, &[4, 2, 24, 8]);
        meta_after(&mut conductor, 960, 0x51, &tempo_bytes(90.0));
        meta_after(&mut conductor, 960, 0x58, &[5, 5, 24, 8]);
        meta_after(&mut conductor, 960, 0x51, &tempo_bytes(150.0));
        meta(&mut conductor, 0x2F, &[]);
        let mut bytes = Vec::new();
        write_chunk(&mut bytes, b"MThd", &[0, 1, 0, 1, 1, 0xE0]);
        write_chunk(&mut bytes, b"MTrk", &conductor);
        let read = parse(&bytes).unwrap();
        assert_eq!((read.tempo, &read.tempo_changes[..]), (Some(90.0), &[jump(6.0, 150.0)][..]));
        assert_eq!((read.time_signature, read.meter_changes.len()), (TimeSignature::new(4, 4), 0));
    }

    #[test]
    fn bad_files_are_errors_not_panics() {
        assert_eq!(parse(b"RIFF....WAVE"), Err(MidiFileError::NotMidi));
        assert_eq!(parse(b""), Err(MidiFileError::NotMidi));
        let good = write(&MidiFile {
            tracks: vec![MidiTrack { notes: vec![note(60, 0.0, 1.0, 100)], ..Default::default() }],
            ..Default::default()
        });
        for cut in [10, 20, good.len() - 3] {
            assert_eq!(parse(&good[..cut]), Err(MidiFileError::Truncated), "cut at {cut}");
        }
        let mut smpte = Vec::new();
        write_chunk(&mut smpte, b"MThd", &[0, 0, 0, 1, 0xE7, 40]);
        assert!(matches!(parse(&smpte), Err(MidiFileError::Unsupported(_))));
        // Garbage inside a track must not hang or panic.
        let mut junk = Vec::new();
        write_chunk(&mut junk, b"MThd", &[0, 0, 0, 1, 0, 96]);
        write_chunk(&mut junk, b"MTrk", &[0x00, 0x12, 0x34, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(parse(&junk).is_err());
    }

    #[test]
    fn variable_lengths_round_trip() {
        for value in [0u32, 1, 127, 128, 8192, 16_383, 16_384, 2_097_151, 0x0FFF_FFFF] {
            let mut bytes = Vec::new();
            write_variable(&mut bytes, value);
            assert_eq!(Reader { bytes: &bytes, at: 0 }.variable().unwrap(), value);
        }
    }

    fn point(beat: f64, value: f64) -> AutomationPoint {
        AutomationPoint::new(beat, value)
    }

    fn step(beat: f64, value: f64) -> AutomationPoint {
        AutomationPoint { beat, value, shape: Segment::Hold }
    }

    fn curve(controller: ControllerKind, points: &[AutomationPoint]) -> ControllerCurve {
        ControllerCurve { controller, points: points.to_vec() }
    }

    #[test]
    fn controllers_are_written_as_events_and_read_back_as_curves() {
        let written = MidiTrack {
            name: "Sax".into(),
            drums: false,
            notes: vec![note(60, 0.0, 4.0, 100)],
            controllers: vec![
                curve(ControllerKind::EXPRESSION, &[point(0.0, 0.25), point(4.0, 1.0), step(4.0, 0.5)]),
                curve(ControllerKind::SUSTAIN, &[step(1.0, 1.0), step(3.0, 0.0)]),
                curve(ControllerKind::Pressure, &[point(2.0, 0.0), point(3.0, 1.0)]),
                curve(ControllerKind::PitchBend, &[point(0.0, 0.0), point(1.0, 1.0), step(2.0, -1.0), step(2.5, 0.0)]),
            ],
        };
        let bytes = write(&MidiFile { tempo: Some(120.0), tracks: vec![written.clone()], ..MidiFile::default() });
        let file = parse(&bytes).unwrap();
        let read = &file.tracks[0];
        assert_eq!(read.notes, written.notes);
        let kinds: Vec<ControllerKind> = read.controllers.iter().map(|curve| curve.controller).collect();
        assert_eq!(kinds, written.controllers.iter().map(|curve| curve.controller).collect::<Vec<_>>());

        // Each curve comes back as a few breakpoints that do what the
        // written one did: within a step of the file's resolution in
        // value, and a tick in time.
        for (was, is) in written.controllers.iter().zip(&read.controllers) {
            assert!(is.points.len() <= was.points.len() + 3, "{}: {:?}", was.controller, is.points);
            let tolerance = if was.controller == ControllerKind::PitchBend { 0.01 } else { 1.5 / 127.0 };
            for n in 0..200 {
                // Just off the ticks, where a step has happened in both.
                let beat = n as f64 / 40.0 + 0.004;
                let (wanted, got) = (was.value_at(beat), is.value_at(beat));
                assert!((wanted - got).abs() <= tolerance, "{} at beat {beat}: {got} for {wanted}", was.controller);
            }
        }
        let pedal = &read.controllers[1];
        assert_eq!(pedal.points, [step(1.0, 1.0), step(3.0, 0.0)], "a pedal's steps are exact");
        assert_eq!(read.controllers[0].points[0], point(0.0, 32.0 / 127.0), "the level a part starts at is written");
        assert_eq!(read.controllers[0].points.last(), Some(&point(4.0, 64.0 / 127.0)));

        // In the file: the expression a note starts at comes before the
        // note, a pedal that is up to begin with is not said to be, and the
        // wheel has all its fourteen bits.
        let track = &bytes[bytes.windows(4).rposition(|chunk| chunk == b"MTrk").unwrap() + 8..];
        let name_end = 4 + "Sax".len();
        assert_eq!(&track[name_end..name_end + 8], &[0x00, 0xB0, 11, 32, 0x00, 0x90, 60, 100]);
        let has = |needle: &[u8]| track.windows(needle.len()).any(|window| window == needle);
        assert!(!has(&[0xB0, 64, 0, 0x00]) || has(&[0xB0, 64, 127]), "no pedal-up before the pedal first goes down");
        assert!(has(&[0xE0, 0x7F, 0x7F]) && has(&[0xE0, 0x00, 0x00]), "the wheel reaches both its ends");
        assert!(has(&[0xD0, 127]), "pressure is a two-byte message");
    }

    #[test]
    fn reads_controllers_in_running_status_and_skips_what_is_not_a_movement() {
        let mut track = Vec::new();
        track.extend_from_slice(&[0x00, 0x90, 60, 100]);
        // Bank select, data entry and a registered parameter number: messages, not movements.
        track.extend_from_slice(&[0x00, 0xB0, 0, 1, 0x00, 32, 2, 0x00, 101, 0, 0x00, 100, 0, 0x00, 6, 12]);
        // The mod wheel in running status, a tick apart: one gesture.
        track.extend_from_slice(&[0x00, 0xB0, 1, 0]);
        for value in 1..=64u8 {
            track.extend_from_slice(&[0x01, 1, value * 2 - 1]);
        }
        // All notes off is no controller either.
        track.extend_from_slice(&[0x10, 123, 0]);
        track.extend_from_slice(&[0x00, 0xE0, 0x00, 0x40, 0x30, 0x00, 0x60, 0x10, 0x80, 60, 0]);
        track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        let mut bytes = Vec::new();
        write_chunk(&mut bytes, b"MThd", &[0, 0, 0, 1, 0, 96]);
        write_chunk(&mut bytes, b"MTrk", &track);

        let file = parse(&bytes).unwrap();
        let controllers = &file.tracks[0].controllers;
        let kinds: Vec<String> = controllers.iter().map(|curve| curve.controller.key()).collect();
        assert_eq!(kinds, ["mod_wheel", "pitch_bend"]);
        assert_eq!(controllers[0].points, [point(0.0, 0.0), point(64.0 / 96.0, 1.0)], "64 events, one line");
        let bend = &controllers[1];
        assert_eq!(bend.value_at(0.0), 0.0);
        assert!((bend.value_at(1.5) - 0.5).abs() < 0.01, "{:?}", bend.points);
    }
}
