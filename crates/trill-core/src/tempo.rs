//! How beats become time, and how beats become bars.
//!
//! Everything in a song is placed in beats (quarter notes): notes, clips,
//! envelopes, markers. A song has a tempo and a time signature to begin
//! with, and may change either along the way:
//!
//! * a [`TempoChange`] sets the tempo from a beat on, at once or (a
//!   `ramp`) by gliding to it from the change before, evenly in beats;
//! * a [`MeterChange`] sets the time signature from a beat on, and a new
//!   bar begins there, wherever that falls in the bar before.
//!
//! Neither moves anything: what changes is how long a beat lasts and which
//! bar it is in. The two questions are answered by two maps, made from the
//! project ([`Project::tempo_map`](crate::Project::tempo_map),
//! [`Project::meter_map`](crate::Project::meter_map)) and by nobody else:
//!
//! * a [`TempoMap`] turns beats into seconds, and into frames at a sample
//!   rate, and back, in closed form for steps and ramps alike. With one
//!   tempo it computes what `beats * 60 / tempo` always did, to the bit;
//! * a [`MeterMap`] turns beats into bars and back.
//!
//! Looking a position up in either searches a short sorted list and
//! neither allocates nor waits, so the audio thread may.

use crate::time::{Beats, TimeSignature};
use serde::{Deserialize, Serialize};

/// How near two positions are to count as the same beat: a change set
/// "at" a beat replaces the one that is there.
pub const SAME_BEAT: Beats = 1e-9;

/// A change of tempo inside a song.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoChange {
    /// Where the tempo is reached, in beats from the start of the song;
    /// after the start.
    pub beat: Beats,
    /// In beats per minute.
    pub tempo: f64,
    /// The tempo glides here from the change before (or from the start of
    /// the song), evenly from beat to beat: an accelerando or a
    /// ritardando. Without it the tempo before holds until `beat` and
    /// jumps.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ramp: bool,
}

fn is_false(flag: &bool) -> bool {
    !*flag
}

/// A change of time signature inside a song.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeterChange {
    /// Where the signature takes over, in beats from the start of the
    /// song; after the start. A bar begins here.
    pub beat: Beats,
    pub signature: TimeSignature,
}

/// One stretch of a song in which the tempo is steady or glides.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Piece {
    /// Where it begins, in beats and in seconds from the start of the song.
    beat: Beats,
    seconds: f64,
    /// Where it ends: infinite for the last.
    end: Beats,
    /// The tempo at its beginning and at its end: the same, unless it is a
    /// ramp.
    from: f64,
    to: f64,
}

impl Piece {
    fn is_ramp(&self) -> bool {
        self.from != self.to && self.end.is_finite()
    }

    /// BPM gained per beat along a ramp.
    fn slope(&self) -> f64 {
        (self.to - self.from) / (self.end - self.beat)
    }

    fn tempo_at(&self, beat: Beats) -> f64 {
        if self.is_ramp() {
            self.from + self.slope() * (beat - self.beat).clamp(0.0, self.end - self.beat)
        } else {
            self.from
        }
    }

    /// Seconds from `from` to `to`, both in the piece.
    fn seconds_between(&self, from: Beats, to: Beats) -> f64 {
        if self.is_ramp() {
            // The time a beat takes is 60 / tempo, and the tempo is a line
            // in beats: the sum of it is a logarithm.
            60.0 / self.slope() * (self.tempo_at(to) / self.tempo_at(from)).ln()
        } else {
            (to - from) * 60.0 / self.from
        }
    }

    /// The beat reached `seconds` after `from`, which is in the piece; it
    /// may lie beyond the piece's end.
    fn beat_after(&self, from: Beats, seconds: f64) -> Beats {
        if self.is_ramp() {
            let slope = self.slope();
            from + self.tempo_at(from) / slope * ((slope * seconds / 60.0).exp() - 1.0)
        } else {
            from + seconds * self.from / 60.0
        }
    }
}

/// How beats become time over a whole song: see the [module](self).
#[derive(Clone, Debug, PartialEq)]
pub struct TempoMap {
    /// In order, the first from beat 0, each ending where the next begins;
    /// never empty.
    pieces: Vec<Piece>,
}

impl TempoMap {
    /// One tempo for the whole song.
    pub fn constant(tempo: f64) -> Self {
        Self::new(tempo, &[])
    }

    /// The map of a song that begins at `first` and has `changes`, which
    /// are in order of their beats.
    pub fn new(first: f64, changes: &[TempoChange]) -> Self {
        let mut pieces: Vec<Piece> = Vec::with_capacity(changes.len() + 1);
        let (mut beat, mut seconds, mut tempo) = (0.0, 0.0, first);
        for change in changes {
            let at = change.beat.max(beat);
            let to = if change.ramp { change.tempo } else { tempo };
            let piece = Piece { beat, seconds, end: at, from: tempo, to };
            // A change at the very beat of the one before leaves no time
            // between them: nothing is played at the tempo in between.
            if at > beat {
                seconds += piece.seconds_between(beat, at);
                pieces.push(piece);
            }
            (beat, tempo) = (at, change.tempo);
        }
        pieces.push(Piece { beat, seconds, end: f64::INFINITY, from: tempo, to: tempo });
        Self { pieces }
    }

    /// Whether the song has one tempo throughout.
    pub fn is_constant(&self) -> bool {
        self.pieces.len() == 1
    }

    /// Whether the tempo glides anywhere.
    pub fn has_ramps(&self) -> bool {
        self.pieces.iter().any(Piece::is_ramp)
    }

    /// The piece `beat` lies in. A beat on the border of two is in the
    /// later one.
    fn piece_at(&self, beat: Beats) -> &Piece {
        let next = self.pieces.partition_point(|piece| piece.beat <= beat);
        &self.pieces[next.saturating_sub(1)]
    }

    /// The piece that ends at `beat`, for looking back from there, or the
    /// one it lies in.
    fn piece_before(&self, beat: Beats) -> &Piece {
        let next = self.pieces.partition_point(|piece| piece.beat < beat);
        &self.pieces[next.saturating_sub(1)]
    }

    /// The tempo at `beat`, in beats per minute. On a change that jumps it
    /// is the tempo from there on.
    pub fn tempo_at(&self, beat: Beats) -> f64 {
        self.piece_at(beat.max(0.0)).tempo_at(beat)
    }

    /// Seconds from the start of the song to `beat`.
    pub fn seconds_at(&self, beat: Beats) -> f64 {
        let piece = self.piece_at(beat.max(0.0));
        piece.seconds + piece.seconds_between(piece.beat, beat)
    }

    /// Seconds from `from` to `to`.
    pub fn seconds_between(&self, from: Beats, to: Beats) -> f64 {
        self.seconds_at(to) - self.seconds_at(from)
    }

    /// Seconds that `length` beats from `from` on last; negative for a
    /// length that goes back.
    pub fn seconds_over(&self, from: Beats, length: Beats) -> f64 {
        let to = from + length;
        let piece = self.piece_at(from.min(to).max(0.0));
        if !piece.is_ramp() && from.max(to) <= piece.end {
            // Within one steady tempo: as it always was, to the bit.
            return length * (60.0 / piece.from);
        }
        self.seconds_between(from, to)
    }

    /// The beat the song is at `seconds` after its start.
    pub fn beat_at_seconds(&self, seconds: f64) -> Beats {
        let next = self.pieces.partition_point(|piece| piece.seconds <= seconds);
        let piece = &self.pieces[next.saturating_sub(1)];
        piece.beat_after(piece.beat, seconds - piece.seconds)
    }

    /// Where the tempo next changes after `beat`: a jump, or the beginning
    /// or the end of a glide. `None` after the last.
    pub fn next_change(&self, beat: Beats) -> Option<Beats> {
        Some(self.piece_at(beat.max(0.0)).end).filter(|end| end.is_finite())
    }

    /// Whether the tempo is gliding at `beat`.
    pub fn is_ramping_at(&self, beat: Beats) -> bool {
        self.piece_at(beat.max(0.0)).is_ramp()
    }

    // ---- in frames, at a sample rate -----------------------------------

    /// Frames of audio from the start of the song to `beat`.
    pub fn frames_at(&self, beat: Beats, sample_rate: f64) -> f64 {
        match self.pieces[0] {
            // As it was always worked out for a song of one tempo.
            first if beat <= first.end && !first.is_ramp() => beat * (60.0 * sample_rate / first.from),
            _ => self.seconds_at(beat) * sample_rate,
        }
    }

    /// Frames of audio from `from` to `to`; negative if `to` is earlier.
    pub fn frames_between(&self, from: Beats, to: Beats, sample_rate: f64) -> f64 {
        let piece = self.piece_at(from.min(to).max(0.0));
        if !piece.is_ramp() && from.max(to) <= piece.end {
            // Within one steady tempo: as it always was, to the bit.
            return (to - from) / (piece.from / (60.0 * sample_rate));
        }
        self.seconds_between(from, to) * sample_rate
    }

    /// Frames of audio that `length` beats from `from` on last; negative
    /// for a length that goes back.
    pub fn frames_over(&self, from: Beats, length: Beats, sample_rate: f64) -> f64 {
        let to = from + length;
        let piece = self.piece_at(from.min(to).max(0.0));
        if !piece.is_ramp() && from.max(to) <= piece.end {
            // Within one steady tempo: as it always was, to the bit.
            return length / (piece.from / (60.0 * sample_rate));
        }
        self.seconds_between(from, to) * sample_rate
    }

    /// The beat reached `frames` frames of audio after `from`: later for a
    /// positive count, earlier for a negative one, and never before the
    /// start of the song's own time (beats run on below zero at the first
    /// tempo, for a count-in).
    pub fn beat_after(&self, from: Beats, frames: f64, sample_rate: f64) -> Beats {
        if frames < 0.0 {
            return self.beat_before(from, -frames, sample_rate);
        }
        let (mut from, mut frames) = (from, frames);
        loop {
            let piece = self.piece_at(from.max(0.0));
            if !piece.is_ramp() {
                // As a song of one tempo always moved, to the bit.
                let reached = from + frames * (piece.from / (60.0 * sample_rate));
                if reached <= piece.end {
                    return reached;
                }
            } else {
                let reached = piece.beat_after(from, frames / sample_rate);
                if reached <= piece.end {
                    return reached;
                }
            }
            frames -= piece.seconds_between(from, piece.end) * sample_rate;
            from = piece.end;
            if frames <= 0.0 {
                return from;
            }
        }
    }

    /// The beat the song was at `frames` frames of audio before `from`.
    fn beat_before(&self, from: Beats, frames: f64, sample_rate: f64) -> Beats {
        let (mut from, mut frames) = (from, frames);
        loop {
            if from <= 0.0 {
                // Before the song: at its first tempo.
                return from - frames * (self.pieces[0].from / (60.0 * sample_rate));
            }
            let piece = self.piece_before(from);
            let reached = if piece.is_ramp() {
                piece.beat_after(from, -frames / sample_rate)
            } else {
                from - frames * (piece.from / (60.0 * sample_rate))
            };
            if reached >= piece.beat {
                return reached;
            }
            frames -= piece.seconds_between(piece.beat, from) * sample_rate;
            from = piece.beat;
            if frames <= 0.0 {
                return from;
            }
        }
    }
}

/// A bar of a song.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Bar {
    /// Counted from 0 at the start of the song.
    pub index: u32,
    /// Where it begins, and how long it is, in beats. A bar that a change
    /// of meter cuts short is as long as it got.
    pub start: Beats,
    pub length: Beats,
    pub signature: TimeSignature,
}

impl Bar {
    pub fn end(&self) -> Beats {
        self.start + self.length
    }

    /// The bar's number as it is said and shown: from 1.
    pub fn number(&self) -> u32 {
        self.index + 1
    }
}

/// One stretch of a song in one time signature.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Section {
    beat: Beats,
    /// The index of the bar that begins at `beat`.
    bar: u32,
    signature: TimeSignature,
    /// Where it ends: infinite for the last.
    end: Beats,
}

impl Section {
    fn bar_length(&self) -> Beats {
        self.signature.beats_per_bar()
    }

    /// How many bars it holds; the last may be cut short.
    fn bars(&self) -> u32 {
        (((self.end - self.beat) / self.bar_length()) - SAME_BEAT).ceil().max(1.0) as u32
    }

    fn bar(&self, index: u32) -> Bar {
        let start = self.beat + f64::from(index - self.bar) * self.bar_length();
        Bar { index, start, length: self.bar_length().min(self.end - start), signature: self.signature }
    }
}

/// How beats become bars over a whole song: see the [module](self).
#[derive(Clone, Debug, PartialEq)]
pub struct MeterMap {
    /// In order, the first from beat 0; never empty.
    sections: Vec<Section>,
}

impl MeterMap {
    /// One time signature for the whole song.
    pub fn constant(signature: TimeSignature) -> Self {
        Self::new(signature, &[])
    }

    /// The map of a song that begins in `first` and has `changes`, which
    /// are in order of their beats.
    pub fn new(first: TimeSignature, changes: &[MeterChange]) -> Self {
        let mut sections = vec![Section { beat: 0.0, bar: 0, signature: first, end: f64::INFINITY }];
        for change in changes {
            let last = sections.len() - 1;
            if change.beat <= sections[last].beat + SAME_BEAT {
                // Two at one beat: the later one is the one that counts.
                sections[last].signature = change.signature;
                continue;
            }
            sections[last].end = change.beat;
            let bar = sections[last].bar + sections[last].bars();
            sections.push(Section { beat: change.beat, bar, signature: change.signature, end: f64::INFINITY });
        }
        Self { sections }
    }

    /// Whether the song has one time signature throughout.
    pub fn is_constant(&self) -> bool {
        self.sections.len() == 1
    }

    fn section_at(&self, beat: Beats) -> &Section {
        // A beat a rounding error short of a change is in the bar the
        // change begins.
        let next = self.sections.partition_point(|section| section.beat <= beat + SAME_BEAT);
        &self.sections[next.saturating_sub(1)]
    }

    /// The time signature at `beat`.
    pub fn signature_at(&self, beat: Beats) -> TimeSignature {
        self.section_at(beat.max(0.0)).signature
    }

    /// The time signature the song arrives at `beat` in: on a change, the
    /// one before it.
    pub fn signature_before(&self, beat: Beats) -> TimeSignature {
        let next = self.sections.partition_point(|section| section.beat < beat - SAME_BEAT);
        self.sections[next.saturating_sub(1)].signature
    }

    /// The beat from which the signature at `beat` has been in force: a
    /// bar line, from which its bars are counted.
    pub fn since(&self, beat: Beats) -> Beats {
        self.section_at(beat.max(0.0)).beat
    }

    /// Where the time signature next changes after `beat`; `None` after
    /// the last change.
    pub fn next_change(&self, beat: Beats) -> Option<Beats> {
        Some(self.section_at(beat.max(0.0)).end).filter(|end| end.is_finite())
    }

    /// The bar `beat` lies in. A beat on a bar line is in the bar that
    /// begins there.
    pub fn bar_at(&self, beat: Beats) -> Bar {
        let beat = beat.max(0.0);
        let section = self.section_at(beat);
        let within = ((beat - section.beat) / section.bar_length() + SAME_BEAT).floor().max(0.0);
        section.bar(section.bar + within as u32)
    }

    /// The bar with the index `index` (from 0).
    pub fn bar(&self, index: u32) -> Bar {
        let next = self.sections.partition_point(|section| section.bar <= index);
        self.sections[next.saturating_sub(1)].bar(index)
    }

    /// Where the bar with the index `index` (from 0) begins.
    pub fn bar_start(&self, index: u32) -> Beats {
        self.bar(index).start
    }

    /// A position as bars: how many bars lie before `beat`, with the part
    /// of the bar it is in as a fraction. The start of bar 3 (index 2) is
    /// 2.0, and half way through it 2.5.
    pub fn bars_at(&self, beat: Beats) -> f64 {
        let bar = self.bar_at(beat);
        let full = bar.signature.beats_per_bar();
        f64::from(bar.index) + ((beat.max(0.0) - bar.start) / full).clamp(0.0, 1.0)
    }

    /// The other way round from [`bars_at`](Self::bars_at): the beat that
    /// lies `bars` bars into the song.
    pub fn beat_of_bars(&self, bars: f64) -> Beats {
        let bars = bars.max(0.0);
        let bar = self.bar(bars.floor() as u32);
        bar.start + (bars - bars.floor()) * bar.signature.beats_per_bar()
    }

    /// A position as it is said: the bar and the beat in it, both counted
    /// from 1, with the beat left out on a bar line. "bar 3", "bar 3 beat
    /// 2.5". Beats are quarter notes, as everywhere.
    pub fn said(&self, beat: Beats) -> String {
        let bar = self.bar_at(beat);
        let within = beat.max(0.0) - bar.start;
        if within <= SAME_BEAT {
            return format!("bar {}", bar.number());
        }
        format!("bar {} beat {}", bar.number(), ((within + 1.0) * 1000.0).round() / 1000.0)
    }

    /// The first bar line at or after `beat`.
    pub fn bar_line_at_or_after(&self, beat: Beats) -> Beats {
        let bar = self.bar_at(beat);
        if beat.max(0.0) - bar.start <= SAME_BEAT { bar.start } else { bar.end() }
    }

    /// The bars that have any part between `from` and `to`, in order.
    pub fn bars_between(&self, from: Beats, to: Beats) -> impl Iterator<Item = Bar> + '_ {
        let first = self.bar_at(from).index;
        (first..).map(|index| self.bar(index)).take_while(move |bar| bar.start < to)
    }

    /// The bars that a change of meter cuts short: each is as long as it
    /// got, and its signature says how long it would have been.
    pub fn cut_bars(&self) -> impl Iterator<Item = Bar> + '_ {
        let cut = |section: &&Section| section.end.is_finite();
        let last = |section: &Section| section.bar(section.bar + section.bars() - 1);
        self.sections.iter().filter(cut).map(last).filter(|bar| bar.signature.beats_per_bar() - bar.length > SAME_BEAT)
    }

    /// Each stretch of the song in one signature: where it begins, the bar
    /// that begins there, and the signature.
    pub fn sections(&self) -> impl Iterator<Item = Bar> + '_ {
        self.sections.iter().map(|section| section.bar(section.bar))
    }
}

/// A stretch of an old timeline that a new one is made of: the new
/// timeline has, from `at` on and for `length` beats, what the old one had
/// from `from` on. With `hold`, it has for that long what the old one had
/// at `from`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Stretch {
    pub at: Beats,
    pub from: Beats,
    pub length: Beats,
    pub hold: bool,
}

/// The tempo of a timeline put together from `stretches` of the one that
/// begins at `first` and has `changes`. Returns its first tempo and its
/// changes.
pub(crate) fn rearranged_tempo(first: f64, changes: &[TempoChange], stretches: &[Stretch]) -> (f64, Vec<TempoChange>) {
    let old = TempoMap::new(first, changes);
    // The tempo the old song arrives at `beat` with: on a jump, the tempo
    // before it.
    let arriving = |beat: Beats| if beat <= 0.0 { first } else { old.piece_before(beat).tempo_at(beat) };
    let jump = |beat: Beats, tempo: f64| TempoChange { beat, tempo, ramp: false };
    let mut made: Vec<TempoChange> = Vec::new();
    for stretch in stretches.iter().filter(|stretch| stretch.length > 0.0) {
        if stretch.hold {
            made.push(jump(stretch.at, arriving(stretch.from)));
            continue;
        }
        made.push(jump(stretch.at, old.tempo_at(stretch.from)));
        let end = stretch.from + stretch.length;
        let inside =
            changes.iter().filter(|point| point.beat > stretch.from + SAME_BEAT && point.beat < end - SAME_BEAT);
        made.extend(inside.map(|point| TempoChange { beat: point.beat - stretch.from + stretch.at, ..*point }));
        // A glide that the stretch's end cuts through ends there, at the
        // tempo it had got to.
        if end.is_finite() && old.piece_before(end).is_ramp() {
            made.push(TempoChange { beat: stretch.at + stretch.length, tempo: arriving(end), ramp: true });
        }
    }
    // What says nothing goes: a change to the tempo already in force,
    // unless a glide sets out from it.
    let mut kept: Vec<TempoChange> = Vec::with_capacity(made.len());
    for (index, point) in made.iter().enumerate() {
        let sets_out = made.get(index + 1).is_some_and(|next| next.ramp);
        if sets_out || kept.last().is_none_or(|last| last.tempo != point.tempo) {
            kept.push(*point);
        }
    }
    let first = kept.iter().rfind(|point| point.beat <= SAME_BEAT).map_or(first, |point| point.tempo);
    (first, kept.into_iter().filter(|point| point.beat > SAME_BEAT).collect())
}

/// [`rearranged_tempo`], for the time signature. A stretch that begins in
/// the signature the one before it ended in goes on in its bars.
pub(crate) fn rearranged_meter(
    first: TimeSignature,
    changes: &[MeterChange],
    stretches: &[Stretch],
) -> (TimeSignature, Vec<MeterChange>) {
    let old = MeterMap::new(first, changes);
    let mut made: Vec<MeterChange> = Vec::new();
    for stretch in stretches.iter().filter(|stretch| stretch.length > 0.0) {
        let signature = if stretch.hold { old.signature_before(stretch.from) } else { old.signature_at(stretch.from) };
        if made.last().is_none_or(|last| last.signature != signature) {
            made.push(MeterChange { beat: stretch.at, signature });
        }
        if stretch.hold {
            continue;
        }
        let end = stretch.from + stretch.length;
        let inside =
            changes.iter().filter(|change| change.beat > stretch.from + SAME_BEAT && change.beat < end - SAME_BEAT);
        made.extend(inside.map(|change| MeterChange { beat: change.beat - stretch.from + stretch.at, ..*change }));
    }
    let first = made.iter().rfind(|change| change.beat <= SAME_BEAT).map_or(first, |change| change.signature);
    (first, made.into_iter().filter(|change| change.beat > SAME_BEAT).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn step(beat: Beats, tempo: f64) -> TempoChange {
        TempoChange { beat, tempo, ramp: false }
    }

    fn ramp(beat: Beats, tempo: f64) -> TempoChange {
        TempoChange { beat, tempo, ramp: true }
    }

    fn close(got: f64, wanted: f64) -> bool {
        (got - wanted).abs() <= 1e-9 * wanted.abs().max(1.0)
    }

    #[test]
    fn one_tempo_is_what_it_always_was_to_the_bit() {
        for tempo in [120.0, 96.5, 123.456_000_000_000_02, 240.0, 61.3] {
            let map = TempoMap::constant(tempo);
            assert!(map.is_constant() && !map.has_ramps());
            let beats_per_frame = tempo / (60.0 * SR);
            for (anchor, frames) in [(0.0, 0u64), (0.0, 1), (3.25, 480), (17.123_456, 1_000_003), (4096.5, 77)] {
                let old = anchor + frames as f64 * beats_per_frame;
                assert_eq!(
                    map.beat_after(anchor, frames as f64, SR).to_bits(),
                    old.to_bits(),
                    "{tempo} {anchor} {frames}"
                );
            }
            for (from, to) in [(0.0, 4.0), (1.5, 1.500_001), (8.0, 2.0), (0.0, 1234.567)] {
                let old = (to - from) / beats_per_frame;
                assert_eq!(map.frames_between(from, to, SR).to_bits(), old.to_bits());
                assert_eq!(map.frames_over(from, 0.37, SR).to_bits(), (0.37 / beats_per_frame).to_bits());
            }
            for beat in [0.0, 1.0, 7.75, 345.678] {
                assert_eq!(map.frames_at(beat, SR).to_bits(), (beat * (60.0 * SR / tempo)).to_bits());
                assert!(close(map.seconds_at(beat), beat * 60.0 / tempo));
            }
            assert_eq!(map.tempo_at(99.0), tempo);
            assert_eq!(map.next_change(0.0), None);
        }
    }

    #[test]
    fn a_tempo_that_jumps_makes_each_stretch_last_as_long_as_its_tempo_says() {
        // Four beats at 120 (two seconds), four at 60 (four seconds), then 240.
        let map = TempoMap::new(120.0, &[step(4.0, 60.0), step(8.0, 240.0)]);
        assert!(!map.is_constant() && !map.has_ramps());
        for (beat, seconds) in [(0.0, 0.0), (2.0, 1.0), (4.0, 2.0), (6.0, 4.0), (8.0, 6.0), (12.0, 7.0)] {
            assert!(close(map.seconds_at(beat), seconds), "{beat}: {}", map.seconds_at(beat));
            assert!(close(map.beat_at_seconds(seconds), beat), "{seconds} s: {}", map.beat_at_seconds(seconds));
            assert!(close(map.frames_at(beat, SR), seconds * SR));
        }
        assert_eq!((map.tempo_at(3.999), map.tempo_at(4.0), map.tempo_at(8.0)), (120.0, 60.0, 240.0));
        assert_eq!((map.next_change(0.0), map.next_change(4.0), map.next_change(9.0)), (Some(4.0), Some(8.0), None));
        // Frames forward across both jumps, and back again.
        let reached = map.beat_after(3.0, 5.0 * SR, SR);
        assert!(close(reached, 10.0), "half a second, four seconds, half a second at 240: {reached}");
        assert!(close(map.beat_after(reached, -5.0 * SR, SR), 3.0));
        assert!(close(map.frames_between(3.0, 10.0, SR), 5.0 * SR));
        assert!(close(map.frames_between(10.0, 3.0, SR), -5.0 * SR));
        assert!(close(map.seconds_over(3.0, 7.0), 5.0) && close(map.seconds_over(10.0, -7.0), -5.0));
        assert_eq!(map.seconds_over(5.0, 2.0).to_bits(), (2.0 * (60.0 / 60.0f64)).to_bits());
        // Before the song, a count-in runs at its first tempo.
        assert!(close(map.beat_after(1.0, -SR, SR), -1.0));
    }

    #[test]
    fn a_tempo_that_glides_is_even_from_beat_to_beat() {
        // From 120 at beat 4 to 180 at beat 8, then on at 180.
        let map = TempoMap::new(120.0, &[step(4.0, 120.0), ramp(8.0, 180.0)]);
        assert!(map.has_ramps() && map.is_ramping_at(5.0) && !map.is_ramping_at(3.0) && !map.is_ramping_at(8.0));
        for (beat, tempo) in [(4.0, 120.0), (5.0, 135.0), (6.0, 150.0), (8.0, 180.0), (20.0, 180.0)] {
            assert!(close(map.tempo_at(beat), tempo), "{beat}: {}", map.tempo_at(beat));
        }
        // The glide lasts 60 / 15 * ln(180 / 120) seconds.
        let glide = 4.0 * (1.5f64).ln();
        assert!(close(map.seconds_at(8.0), 2.0 + glide), "{}", map.seconds_at(8.0));
        // Checked against the sum of many small steps.
        let mut seconds = 0.0;
        let steps = 400_000;
        for index in 0..steps {
            let beat = 4.0 + 4.0 * (index as f64 + 0.5) / steps as f64;
            seconds += 4.0 / steps as f64 * 60.0 / map.tempo_at(beat);
        }
        assert!((seconds - glide).abs() < 1e-9, "{seconds} against {glide}");
        // There and back, in seconds and in frames, through the glide.
        for beat in [0.0, 3.5, 4.0, 4.7, 6.2, 8.0, 11.0] {
            assert!(close(map.beat_at_seconds(map.seconds_at(beat)), beat), "{beat}");
            let frames = map.frames_between(1.0, beat, SR);
            assert!(close(map.beat_after(1.0, frames, SR), beat), "{beat}: {}", map.beat_after(1.0, frames, SR));
            assert!((map.beat_after(beat, -map.frames_between(2.0, beat, SR), SR) - 2.0).abs() < 1e-9);
        }
        // A glide down is one too.
        let slowing = TempoMap::new(140.0, &[ramp(8.0, 70.0)]);
        assert!(close(slowing.tempo_at(4.0), 105.0));
        assert!(close(slowing.seconds_at(8.0), 60.0 / (-70.0 / 8.0) * (0.5f64).ln()));
        assert!(close(slowing.beat_at_seconds(slowing.seconds_at(6.5)), 6.5));
    }

    #[test]
    fn frames_add_up_the_same_however_they_are_counted() {
        let map = TempoMap::new(120.0, &[step(2.0, 140.0), ramp(6.0, 90.0), step(9.0, 200.0)]);
        // One long stride, or many strides of odd lengths from the same
        // anchor: the same beat, as the engine counts frames from where
        // the song last started.
        let whole = map.beat_after(0.5, 400_000.0, SR);
        for stride in [1.0, 37.0, 441.0, 512.0] {
            let mut frames = 0.0;
            while frames + stride <= 400_000.0 {
                frames += stride;
            }
            let reached = map.beat_after(0.5, frames, SR);
            let rest = map.beat_after(reached, 400_000.0 - frames, SR);
            assert!((rest - whole).abs() < 1e-9, "strides of {stride}: {rest} against {whole}");
        }
        assert!(close(map.frames_between(0.5, whole, SR), 400_000.0));
    }

    fn meter(beat: Beats, numerator: u8, denominator: u8) -> MeterChange {
        MeterChange { beat, signature: TimeSignature::new(numerator, denominator).unwrap() }
    }

    #[test]
    fn bars_are_counted_through_changes_of_meter() {
        // Two bars of 4/4, two of 7/8, then 3/4.
        let four = TimeSignature::default();
        let map = MeterMap::new(four, &[meter(8.0, 7, 8), meter(15.0, 3, 4)]);
        assert!(!map.is_constant());
        let starts: Vec<Beats> = (0..7).map(|index| map.bar_start(index)).collect();
        assert_eq!(starts, [0.0, 4.0, 8.0, 11.5, 15.0, 18.0, 21.0]);
        for (beat, index, start, length) in
            [(0.0, 0, 0.0, 4.0), (3.99, 0, 0.0, 4.0), (4.0, 1, 4.0, 4.0), (8.0, 2, 8.0, 3.5), (11.5, 3, 11.5, 3.5)]
        {
            let bar = map.bar_at(beat);
            assert_eq!((bar.index, bar.start, bar.length), (index, start, length), "at {beat}");
        }
        assert_eq!((map.bar_at(14.9).number(), map.bar_at(15.0).number(), map.bar_at(100.0).number()), (4, 5, 33));
        assert_eq!(map.signature_at(9.0), TimeSignature::new(7, 8).unwrap());
        assert_eq!((map.since(9.0), map.since(3.0), map.since(40.0)), (8.0, 0.0, 15.0));
        assert_eq!((map.next_change(0.0), map.next_change(8.0), map.next_change(15.0)), (Some(8.0), Some(15.0), None));
        // Positions as bars, there and back.
        assert_eq!((map.bars_at(8.0), map.bars_at(9.75), map.bars_at(16.5)), (2.0, 2.5, 4.5));
        for beat in [0.0, 2.0, 8.0, 9.75, 13.0, 15.0, 31.25] {
            assert!((map.beat_of_bars(map.bars_at(beat)) - beat).abs() < 1e-9, "{beat}");
        }
        assert_eq!(
            (map.bar_line_at_or_after(8.0), map.bar_line_at_or_after(8.1), map.bar_line_at_or_after(14.0)),
            (8.0, 11.5, 15.0)
        );
        let between: Vec<u32> = map.bars_between(7.0, 15.5).map(|bar| bar.number()).collect();
        assert_eq!(between, [2, 3, 4, 5]);
        let sections: Vec<(u32, Beats)> = map.sections().map(|bar| (bar.number(), bar.start)).collect();
        assert_eq!(sections, [(1, 0.0), (3, 8.0), (5, 15.0)]);
        assert_eq!(
            (map.said(8.0), map.said(13.0), map.said(0.25)),
            ("bar 3".into(), "bar 4 beat 2.5".into(), "bar 1 beat 1.25".into())
        );
        // A rounding error short of a bar line is on it.
        assert_eq!(map.bar_at(11.5 - 1e-12).index, 3);
    }

    #[test]
    fn a_change_of_meter_begins_a_bar_wherever_it_falls() {
        // 4/4, and 6/8 from beat 6: the second bar is cut after two beats.
        let map = MeterMap::new(TimeSignature::default(), &[meter(6.0, 6, 8)]);
        let second = map.bar_at(5.0);
        assert_eq!((second.index, second.start, second.length, second.end()), (1, 4.0, 2.0, 6.0));
        let third = map.bar_at(6.0);
        assert_eq!((third.index, third.start, third.length), (2, 6.0, 3.0));
        assert_eq!(map.bar_start(3), 9.0);
        assert_eq!(map.bar_line_at_or_after(4.5), 6.0, "the cut bar ends where the new meter begins");
        assert_eq!(map.cut_bars().map(|bar| (bar.number(), bar.length)).collect::<Vec<_>>(), [(2, 2.0)]);
        // A change to the signature already in force begins a bar too: a
        // song with a pickup of one beat.
        let pickup = MeterMap::new(TimeSignature::default(), &[meter(1.0, 4, 4)]);
        assert_eq!((pickup.bar_at(0.5).length, pickup.bar_start(1), pickup.bar_start(2)), (1.0, 1.0, 5.0));
        assert_eq!(pickup.signature_before(1.0), TimeSignature::default());
        // Two changes at one beat: the later one counts.
        let twice = MeterMap::new(TimeSignature::default(), &[meter(4.0, 3, 4), meter(4.0, 5, 4)]);
        assert_eq!((twice.bar_at(4.0).length, twice.bar_start(2)), (5.0, 9.0));
    }

    #[test]
    fn time_taken_out_put_in_and_repeated_takes_its_tempo_and_meter_along() {
        // 120, 150 from beat 8, and from beat 16 a glide to 90 at beat 24.
        let changes = [step(8.0, 150.0), step(16.0, 150.0), ramp(24.0, 90.0)];
        let whole = |from: Beats| Stretch { at: 0.0, from, length: f64::INFINITY, hold: false };
        let rest = |at: Beats, from: Beats| Stretch { at, ..whole(from) };
        let part = |at: Beats, from: Beats, length: Beats| Stretch { at, from, length, hold: false };
        let tempo_of = |first: f64, changes: &[TempoChange], beat: Beats| TempoMap::new(first, changes).tempo_at(beat);

        // Beats 4 to 12 taken out: 120 until 4, then what was at 12 on.
        let (first, cut) = rearranged_tempo(120.0, &changes, &[part(0.0, 0.0, 4.0), rest(4.0, 12.0)]);
        assert_eq!((first, &cut[..]), (120.0, &[step(4.0, 150.0), step(8.0, 150.0), ramp(16.0, 90.0)][..]));
        // Taken out through the glide: it goes on from the tempo it had.
        let (_, cut) = rearranged_tempo(120.0, &changes, &[part(0.0, 0.0, 10.0), rest(10.0, 20.0)]);
        assert!(close(tempo_of(120.0, &cut, 10.0), 120.0), "{cut:?}");
        assert!(close(tempo_of(120.0, &cut, 12.0), 105.0) && close(tempo_of(120.0, &cut, 14.0), 90.0), "{cut:?}");
        assert_eq!(tempo_of(120.0, &cut, 9.0), 150.0);

        // Four beats opened up at 20: the glide waits there, and goes on.
        let hold = Stretch { at: 20.0, from: 20.0, length: 4.0, hold: true };
        let (_, held) = rearranged_tempo(120.0, &changes, &[part(0.0, 0.0, 20.0), hold, rest(24.0, 20.0)]);
        for (beat, tempo) in [(18.0, 135.0), (20.0, 120.0), (22.0, 120.0), (24.0, 120.0), (26.0, 105.0), (28.0, 90.0)] {
            assert!(
                close(tempo_of(120.0, &held, beat), tempo),
                "at {beat}: {} in {held:?}",
                tempo_of(120.0, &held, beat)
            );
        }

        // Beats 4 to 12 heard twice: the jump at 8 comes again at 16, and
        // each time round starts from 120.
        let (_, twice) =
            rearranged_tempo(120.0, &changes, &[part(0.0, 0.0, 12.0), part(12.0, 4.0, 8.0), rest(20.0, 12.0)]);
        for (beat, tempo) in
            [(6.0, 120.0), (10.0, 150.0), (12.0, 120.0), (15.9, 120.0), (16.0, 150.0), (20.0, 150.0), (32.0, 90.0)]
        {
            assert!(close(tempo_of(120.0, &twice, beat), tempo), "at {beat}: {twice:?}");
        }
        // The song from beat 8 on begins at 150.
        assert_eq!(rearranged_tempo(120.0, &changes, &[whole(8.0)]).0, 150.0);

        // The meter goes the same ways.
        let four = TimeSignature::default();
        let seven = TimeSignature::new(7, 8).unwrap();
        let meters = [meter(8.0, 7, 8), meter(15.0, 4, 4)];
        let (first, cut) = rearranged_meter(four, &meters, &[part(0.0, 0.0, 4.0), rest(4.0, 8.0)]);
        assert_eq!((first, &cut[..]), (four, &[meter(4.0, 7, 8), meter(11.0, 4, 4)][..]));
        let (_, twice) =
            rearranged_meter(four, &meters, &[part(0.0, 0.0, 15.0), part(15.0, 8.0, 7.0), rest(22.0, 15.0)]);
        assert_eq!(&twice[..], &[meter(8.0, 7, 8), meter(22.0, 4, 4)][..], "the 7/8 bars twice over, as one stretch");
        assert_eq!(rearranged_meter(four, &meters, &[whole(9.0)]).0, seven);
        // Time opened up where a change is has what the song arrived
        // there with, and the change comes after it.
        let hold = Stretch { at: 8.0, from: 8.0, length: 4.0, hold: true };
        let (_, opened) = rearranged_meter(four, &meters, &[part(0.0, 0.0, 8.0), hold, rest(12.0, 8.0)]);
        assert_eq!(&opened[..], &[meter(12.0, 7, 8), meter(19.0, 4, 4)][..]);
        let (_, opened) = rearranged_tempo(120.0, &changes, &[part(0.0, 0.0, 8.0), hold, rest(12.0, 8.0)]);
        assert_eq!(&opened[..], &[step(12.0, 150.0), step(20.0, 150.0), ramp(28.0, 90.0)][..]);
        // Nothing taken from the front leaves a song as it was.
        let same = rearranged_tempo(120.0, &changes, &[part(0.0, 0.0, 0.0), rest(0.0, 0.0)]);
        assert_eq!((same.0, &same.1[..]), (120.0, &changes[..]));
    }
}
