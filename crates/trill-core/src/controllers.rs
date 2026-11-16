//! MIDI controllers in clips: the movement of an expression pedal, a mod
//! wheel, the sustain pedal, the pitch wheel or channel pressure over time.
//!
//! A MIDI clip holds, beside its notes, one [`ControllerCurve`] for each
//! controller it moves: breakpoints joined by straight, held or curved
//! segments, exactly like an automation lane's. A curve belongs to the
//! clip's content, as its notes do. It is measured in beats from where the
//! content begins, so it loops, is trimmed and split, moves and is copied
//! with the notes above it.
//!
//! # What a controller is worth at any moment
//!
//! One rule decides it, for the engine, the exporters and the views alike
//! ([`ControllerLane`], built by [`Track::controller_lanes`](crate::Track)):
//!
//! * **Under a clip that has a curve for it**, the curve: held at its last
//!   point's value after that point, and before its first point at that
//!   point's value, or at rest if the controller is one that rests (a
//!   pedal is up until the point that puts it down). So a curve says
//!   something about every moment of its clip. Where clips overlap, the one
//!   that starts later is the one heard.
//! * **After such a clip ends**, what springs back on a keyboard springs
//!   back: the pitch wheel, channel pressure and the pedals (controllers
//!   64 to 69) return to rest. Every other controller stays where the clip
//!   left it until another clip on the track moves it.
//! * **Before the first such clip**, a controller that stays waits at that
//!   clip's opening value, so that a song is in the same state at a given
//!   place however it got there.
//! * A track none of whose clips has a curve for a controller says nothing
//!   about it: the instrument keeps whatever it has.

use crate::device::{CONTROLLER_MOD_WHEEL, CONTROLLER_SUSTAIN, EventKind};
use crate::project::{AutomationPoint, EditError, Segment, automation_value};
use crate::time::Beats;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The controller number of the breath controller.
pub const CONTROLLER_BREATH: u8 = 2;
/// The controller number of channel volume.
pub const CONTROLLER_VOLUME: u8 = 7;
/// The controller number of the expression pedal.
pub const CONTROLLER_EXPRESSION: u8 = 11;
/// The highest controller number a clip can hold. Those above it are not
/// controllers but MIDI's channel-mode messages ("all notes off" and the
/// like), which the engine sends when they are due.
pub const MAX_CONTROLLER: u8 = 119;
/// How many different things a curve can move: the controllers by number,
/// channel pressure and the pitch wheel. [`ControllerKind::index`] is below
/// this.
pub const CONTROLLER_KINDS: usize = 130;

/// How far apart, as a fraction of the range, a thinned curve may be from
/// the movement it was made from ([`thin`]): one step of a MIDI controller.
pub const THIN_TOLERANCE: f64 = 1.0 / 127.0;
/// How long a controller must rest, in seconds, for its next movement to
/// count as a new gesture ([`thin`]) and not as the continuation of a
/// ramp. A wheel in motion reports every few milliseconds.
pub const GESTURE_GAP_SECONDS: f64 = 0.06;

/// What a curve in a clip moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ControllerKind {
    /// A MIDI controller by its number, 0..=[`MAX_CONTROLLER`]. Its value
    /// runs from 0 to 1.
    Controller(u8),
    /// Channel pressure (aftertouch on the whole keyboard), from 0 to 1.
    Pressure,
    /// The pitch wheel, from -1 (fully down) to 1 (fully up); 0 at rest.
    PitchBend,
}

/// The names controllers are known by, with their numbers. The first name
/// of a number is the one it is shown as.
const NAMES: &[(&str, u8, &str)] = &[
    ("mod_wheel", CONTROLLER_MOD_WHEEL, "Mod wheel"),
    ("modulation", CONTROLLER_MOD_WHEEL, "Mod wheel"),
    ("breath", CONTROLLER_BREATH, "Breath"),
    ("foot", 4, "Foot pedal"),
    ("portamento_time", 5, "Portamento time"),
    ("volume", CONTROLLER_VOLUME, "Volume"),
    ("balance", 8, "Balance"),
    ("pan", 10, "Pan"),
    ("expression", CONTROLLER_EXPRESSION, "Expression"),
    ("sustain", CONTROLLER_SUSTAIN, "Sustain"),
    ("sustain_pedal", CONTROLLER_SUSTAIN, "Sustain"),
    ("portamento", 65, "Portamento"),
    ("sostenuto", 66, "Sostenuto"),
    ("soft_pedal", 67, "Soft pedal"),
    ("legato", 68, "Legato"),
    ("hold_2", 69, "Hold 2"),
    ("resonance", 71, "Resonance"),
    ("release", 72, "Release"),
    ("attack", 73, "Attack"),
    ("brightness", 74, "Brightness"),
    ("cutoff", 74, "Brightness"),
    ("reverb_send", 91, "Reverb send"),
    ("chorus_send", 93, "Chorus send"),
];

impl ControllerKind {
    pub const MOD_WHEEL: Self = Self::Controller(CONTROLLER_MOD_WHEEL);
    pub const BREATH: Self = Self::Controller(CONTROLLER_BREATH);
    pub const VOLUME: Self = Self::Controller(CONTROLLER_VOLUME);
    pub const EXPRESSION: Self = Self::Controller(CONTROLLER_EXPRESSION);
    pub const SUSTAIN: Self = Self::Controller(CONTROLLER_SUSTAIN);

    /// The controller with `number`, if a clip can hold it.
    pub fn numbered(number: u8) -> Option<Self> {
        (number <= MAX_CONTROLLER).then_some(Self::Controller(number))
    }

    /// Reads a controller given by name (`"expression"`, `"mod_wheel"`,
    /// `"pitch_bend"`, `"sustain"`, `"breath"`, `"pressure"`...) or by number
    /// (`"11"`, `"cc11"`). Case, spaces and dashes do not matter.
    pub fn parse(text: &str) -> Option<Self> {
        let wanted: String =
            text.trim().chars().map(|c| if c == ' ' || c == '-' { '_' } else { c.to_ascii_lowercase() }).collect();
        match wanted.as_str() {
            "pitch_bend" | "pitch_wheel" | "pitchbend" | "bend" | "pitch" => return Some(Self::PitchBend),
            "pressure" | "channel_pressure" | "aftertouch" => return Some(Self::Pressure),
            _ => {}
        }
        if let Some((_, number, _)) = NAMES.iter().find(|(name, ..)| *name == wanted) {
            return Some(Self::Controller(*number));
        }
        let digits = wanted.strip_prefix("cc_").or_else(|| wanted.strip_prefix("cc")).unwrap_or(&wanted);
        digits.parse::<u8>().ok().and_then(Self::numbered)
    }

    /// The name the controller is asked for by, if it has one: the key
    /// [`ControllerKind::parse`] reads back.
    pub fn name(self) -> Option<&'static str> {
        match self {
            Self::PitchBend => Some("pitch_bend"),
            Self::Pressure => Some("pressure"),
            Self::Controller(number) => NAMES.iter().find(|(_, n, _)| *n == number).map(|(name, ..)| *name),
        }
    }

    /// The name if there is one, else the number: what tools call it.
    pub fn key(self) -> String {
        match (self.name(), self) {
            (Some(name), _) => name.to_string(),
            (None, Self::Controller(number)) => number.to_string(),
            (None, _) => unreachable!("the wheel and pressure have names"),
        }
    }

    /// What a person calls it: "Expression", "Pitch wheel", "CC 23".
    pub fn label(self) -> String {
        match self {
            Self::PitchBend => "Pitch wheel".to_string(),
            Self::Pressure => "Pressure".to_string(),
            Self::Controller(number) => match NAMES.iter().find(|(_, n, _)| *n == number) {
                Some((_, _, label)) => (*label).to_string(),
                None => format!("CC {number}"),
            },
        }
    }

    /// The controller's MIDI number, for those that have one.
    pub fn number(self) -> Option<u8> {
        match self {
            Self::Controller(number) => Some(number),
            _ => None,
        }
    }

    /// The lowest and highest value a curve for it may take.
    pub fn range(self) -> (f64, f64) {
        match self {
            Self::PitchBend => (-1.0, 1.0),
            _ => (0.0, 1.0),
        }
    }

    /// Whether it returns to rest by itself when let go of: the pitch
    /// wheel, channel pressure and the pedals. See the module documentation
    /// for what follows from that.
    pub fn springs_back(self) -> bool {
        match self {
            Self::PitchBend | Self::Pressure => true,
            Self::Controller(number) => (64..=69).contains(&number),
        }
    }

    /// Where a controller that springs back comes to rest.
    pub fn rest(self) -> Option<f64> {
        self.springs_back().then_some(0.0)
    }

    /// Whether it is a pedal or a switch: down from half way, up below.
    /// Its movement is recorded and drawn as steps.
    pub fn is_switch(self) -> bool {
        matches!(self, Self::Controller(number) if (64..=69).contains(&number))
    }

    /// A number below [`CONTROLLER_KINDS`], different for each kind: for
    /// tables that keep something per controller.
    pub fn index(self) -> usize {
        match self {
            Self::Controller(number) => usize::from(number.min(127)),
            Self::Pressure => 128,
            Self::PitchBend => 129,
        }
    }

    /// The kind with that [`index`](Self::index).
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            128 => Some(Self::Pressure),
            129 => Some(Self::PitchBend),
            _ => u8::try_from(index).ok().and_then(Self::numbered),
        }
    }

    /// The event that puts the controller at `value`.
    pub fn event(self, value: f32) -> EventKind {
        match self {
            Self::Controller(controller) => EventKind::Control { controller, value },
            Self::Pressure => EventKind::ChannelPressure { value },
            Self::PitchBend => EventKind::PitchBend { value },
        }
    }

    /// The controller an event moves, and where to, if it moves one a clip
    /// can hold.
    pub fn of_event(kind: EventKind) -> Option<(Self, f32)> {
        match kind {
            EventKind::Control { controller, value } => Self::numbered(controller).map(|kind| (kind, value)),
            EventKind::ChannelPressure { value } => Some((Self::Pressure, value)),
            EventKind::PitchBend { value } => Some((Self::PitchBend, value)),
            _ => None,
        }
    }

    fn validate(self) -> Result<(), EditError> {
        match self {
            Self::Controller(number) if number > MAX_CONTROLLER => Err(EditError::Invalid(format!(
                "controller {number} is outside 0..={MAX_CONTROLLER} (the numbers above are MIDI's channel messages, \
                 not controllers)"
            ))),
            _ => Ok(()),
        }
    }
}

impl std::fmt::Display for ControllerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.name(), self) {
            (Some(name), _) => f.write_str(name),
            (None, Self::Controller(number)) => write!(f, "controller {number}"),
            (None, _) => Ok(()),
        }
    }
}

/// In a project file a controller is its number, and the wheel and
/// pressure are their names.
impl Serialize for ControllerKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Controller(number) => serializer.serialize_u8(*number),
            Self::Pressure => serializer.serialize_str("pressure"),
            Self::PitchBend => serializer.serialize_str("pitch_bend"),
        }
    }
}

impl<'de> Deserialize<'de> for ControllerKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Kind;
        impl Visitor<'_> for Kind {
            type Value = ControllerKind;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a controller number from 0 to 119, \"pitch_bend\" or \"pressure\"")
            }

            fn visit_u64<E: de::Error>(self, number: u64) -> Result<ControllerKind, E> {
                u8::try_from(number)
                    .ok()
                    .and_then(ControllerKind::numbered)
                    .ok_or_else(|| E::custom(format!("controller {number} is outside 0..={MAX_CONTROLLER}")))
            }

            fn visit_i64<E: de::Error>(self, number: i64) -> Result<ControllerKind, E> {
                u64::try_from(number)
                    .map_err(|_| E::custom(format!("controller {number} is outside 0..={MAX_CONTROLLER}")))
                    .and_then(|number| self.visit_u64(number))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<ControllerKind, E> {
                ControllerKind::parse(text).ok_or_else(|| E::custom(format!("'{text}' is not a controller")))
            }
        }
        deserializer.deserialize_any(Kind)
    }
}

/// The movement of one controller through a clip's content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControllerCurve {
    pub controller: ControllerKind,
    /// Sorted by beat; never empty. Beats are measured from where the
    /// clip's content begins, like the starts of its notes. Values are
    /// within [`ControllerKind::range`]. Two points on one beat are a step.
    pub points: Vec<AutomationPoint>,
}

impl ControllerCurve {
    /// The curve's value `beat` beats into the content: held after its
    /// last point, and before its first at that point's value, or at rest
    /// if the controller is one that rests
    /// ([`ControllerKind::springs_back`]).
    pub fn value_at(&self, beat: Beats) -> f64 {
        match (self.controller.rest(), self.points.first()) {
            (Some(rest), Some(first)) if beat < first.beat => rest,
            _ => automation_value(&self.points, beat),
        }
    }

    /// The points as they are played: the curve's own, after a point that
    /// says where the controller is before them if that is not where the
    /// first of them puts it. [`automation_value`] over these is
    /// [`ControllerCurve::value_at`].
    pub fn played(&self) -> Vec<AutomationPoint> {
        starting_from_rest(self.controller, self.points.clone())
    }
}

/// Checks and orders the points of a curve for `kind`.
pub(crate) fn validated(
    kind: ControllerKind,
    mut points: Vec<AutomationPoint>,
) -> Result<Vec<AutomationPoint>, EditError> {
    kind.validate()?;
    let (low, high) = kind.range();
    for point in &points {
        if !point.beat.is_finite() || point.beat < 0.0 {
            return Err(EditError::Invalid(format!(
                "a point of the {kind} curve is at beat {}; beats must be >= 0",
                point.beat
            )));
        }
        if !point.value.is_finite() || point.value < low || point.value > high {
            return Err(EditError::Invalid(format!(
                "{kind} value {} at beat {} is outside {low}..={high}",
                point.value, point.beat
            )));
        }
    }
    // Stable: two points on one beat keep their order, which is the step.
    points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    Ok(points)
}

/// The value an envelope has just before `beat`: the same as
/// [`automation_value`] except on a step, where this is the side before it.
pub fn value_before(points: &[AutomationPoint], beat: Beats) -> f64 {
    let next = points.partition_point(|p| p.beat < beat);
    if next == 0 {
        return points[0].value;
    }
    if next == points.len() {
        return points[next - 1].value;
    }
    let (a, b) = (points[next - 1], points[next]);
    let span = b.beat - a.beat;
    if span <= 0.0 {
        return a.value;
    }
    match a.shape {
        Segment::Hold => a.value,
        _ => automation_value(&[a, b], beat),
    }
}

/// A stretch of the timeline in which one clip's curve is what a
/// controller follows.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneSpan {
    pub start: Beats,
    pub end: Beats,
    /// Where in the clip's content `start` falls.
    pub origin: Beats,
    /// The length of the clip's loop, if it loops: the content position
    /// wraps round at it.
    pub period: Option<Beats>,
    /// The curve, in content beats.
    pub points: Vec<AutomationPoint>,
    /// What the controller is worth from `end` until the next span.
    pub after: f64,
}

impl LaneSpan {
    /// Where in the content the timeline position `beat` falls.
    pub fn content_beat(&self, beat: Beats) -> Beats {
        let content = self.origin + (beat - self.start);
        match self.period {
            Some(period) => content.rem_euclid(period),
            None => content,
        }
    }

    fn value_at(&self, beat: Beats) -> f64 {
        automation_value(&self.points, self.content_beat(beat))
    }
}

/// One controller on one track over the whole song: what it is worth at
/// every moment, by the rule in the module documentation. Evaluating it
/// neither allocates nor waits, so the audio thread may.
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerLane {
    pub kind: ControllerKind,
    /// In order, none overlapping another.
    pub spans: Vec<LaneSpan>,
    /// What the controller is worth before the first span.
    pub before: f64,
}

impl ControllerLane {
    pub fn value_at(&self, beat: Beats) -> f64 {
        let next = self.spans.partition_point(|span| span.start <= beat);
        if next == 0 {
            return self.before;
        }
        let span = &self.spans[next - 1];
        if beat < span.end { span.value_at(beat) } else { span.after }
    }

    /// The end of the last span: from here on the lane never changes.
    pub fn end(&self) -> Beats {
        self.spans.last().map_or(0.0, |span| span.end)
    }
}

/// A clip's part in a lane: where it lies and the curve it brings.
pub(crate) struct LaneClip<'a> {
    pub start: Beats,
    pub end: Beats,
    /// Where in its content the clip starts playing.
    pub offset: Beats,
    pub period: Option<Beats>,
    pub curve: &'a ControllerCurve,
}

/// Builds the lane of `kind` from the clips that have a curve for it,
/// given in the order they lie on the track.
pub(crate) fn lane_of(kind: ControllerKind, clips: &[LaneClip<'_>]) -> ControllerLane {
    let mut edges: Vec<Beats> = clips.iter().flat_map(|clip| [clip.start, clip.end]).collect();
    edges.sort_by(f64::total_cmp);
    edges.dedup();

    // Between two neighbouring edges nothing starts or ends, so one clip
    // is heard throughout: of those lying there, the one that starts last.
    let mut pieces: Vec<(usize, Beats, Beats)> = Vec::new();
    for edge in edges.windows(2) {
        let (from, to) = (edge[0], edge[1]);
        let heard = clips
            .iter()
            .enumerate()
            .filter(|(_, clip)| clip.start <= from && clip.end >= to)
            .max_by(|(a_index, a), (b_index, b)| a.start.total_cmp(&b.start).then(a_index.cmp(b_index)))
            .map(|(index, _)| index);
        let Some(index) = heard else { continue };
        match pieces.last_mut() {
            Some((last, _, end)) if *last == index && *end == from => *end = to,
            _ => pieces.push((index, from, to)),
        }
    }

    let rest = kind.rest();
    let played: Vec<Vec<AutomationPoint>> = clips.iter().map(|clip| clip.curve.played()).collect();
    let spans: Vec<LaneSpan> = pieces
        .into_iter()
        .map(|(index, start, end)| {
            let (clip, points) = (&clips[index], &played[index]);
            let origin = clip.offset + (start - clip.start);
            let after = rest.unwrap_or_else(|| {
                // Where the curve had got to as the span ended: the side
                // before, so that a loop which ends on its last beat
                // leaves the controller there and not back at its start.
                let content = origin + (end - start);
                let content = match clip.period {
                    Some(period) => {
                        let wrapped = content.rem_euclid(period);
                        if wrapped == 0.0 { period } else { wrapped }
                    }
                    None => content,
                };
                value_before(points, content)
            });
            LaneSpan { start, end, origin, period: clip.period, points: points.clone(), after }
        })
        .collect();
    let before = rest.unwrap_or_else(|| spans.first().map_or(0.0, |span| span.value_at(span.start)));
    ControllerLane { kind, spans, before }
}

/// Turns the reports of a controller in motion into the few breakpoints of
/// a curve that follows the same movement.
///
/// `events` are `(beat, value)` in the order they happened: each is where
/// the controller was from that moment on. What comes out:
///
/// * Events that follow each other closely (no more than `gap` beats
///   apart), or that differ by no more than a step or two however far
///   apart (a slow swell reports only when it reaches the next step), are
///   one gesture. It becomes straight segments that pass within
///   `tolerance` of every event of it; between two events it is never
///   further from where the controller was than those steps.
/// * A controller that rested for longer than `gap` and then was somewhere
///   else stayed where it was, and jumped: the point before the rest is
///   held.
/// * A pedal or a switch (`switch`, or a controller that only ever reports
///   its two ends) is steps: every change is a held point, exactly where
///   it happened.
pub fn thin(events: &[(Beats, f64)], switch: bool, tolerance: f64, gap: Beats) -> Vec<AutomationPoint> {
    // More than this between two reports that are far apart is a jump.
    let jump = 2.5 * tolerance;
    // Of several reports at one moment, the last is where it was left.
    let mut samples: Vec<(Beats, f64)> = Vec::with_capacity(events.len());
    for &(beat, value) in events {
        if !beat.is_finite() || !value.is_finite() {
            continue;
        }
        match samples.last_mut() {
            Some(last) if last.0 >= beat => last.1 = value,
            _ => samples.push((beat, value)),
        }
    }
    let two_ended = samples.iter().all(|(_, value)| *value == 0.0 || *value == 1.0);
    if switch || (two_ended && samples.len() > 1) {
        let mut points: Vec<AutomationPoint> = Vec::new();
        for (beat, value) in samples {
            if points.last().is_none_or(|last| last.value != value) {
                points.push(AutomationPoint { beat, value, shape: Segment::Hold });
            }
        }
        return points;
    }

    let mut points = Vec::new();
    let mut run_start = 0;
    for index in 1..=samples.len() {
        if index < samples.len() {
            let (before, after) = (samples[index - 1], samples[index]);
            if after.0 - before.0 <= gap || (after.1 - before.1).abs() <= jump {
                continue;
            }
        }
        let run = &samples[run_start..index];
        let from = points.len();
        simplify(run, tolerance.max(0.0), &mut points);
        // The run's last point holds until the next gesture begins.
        if index < samples.len()
            && let Some(last) = points[from..].last_mut()
        {
            last.shape = Segment::Hold;
        }
        run_start = index;
    }
    tidied(points)
}

/// `points`, the curve of `kind`, with a point at its beginning that puts
/// the controller at rest if `kind` is one that rests and the curve does
/// not begin there: a pedal is up until the point that puts it down. Other
/// controllers wait at their first value, which needs no point.
fn starting_from_rest(kind: ControllerKind, mut points: Vec<AutomationPoint>) -> Vec<AutomationPoint> {
    if let (Some(rest), Some(first)) = (kind.rest(), points.first().copied())
        && first.beat > 0.0
        && first.value != rest
    {
        points.insert(0, AutomationPoint { beat: 0.0, value: rest, shape: Segment::Hold });
    }
    points
}

/// Appends the fewest of `run`'s samples, joined by straight lines, that
/// pass within `tolerance` of all of them (Ramer–Douglas–Peucker, measured
/// in value at each sample's moment).
fn simplify(run: &[(Beats, f64)], tolerance: f64, out: &mut Vec<AutomationPoint>) {
    let Some(last) = run.len().checked_sub(1) else { return };
    let mut keep = vec![false; run.len()];
    keep[0] = true;
    keep[last] = true;
    let mut stack = vec![(0usize, last)];
    while let Some((from, to)) = stack.pop() {
        if to <= from + 1 {
            continue;
        }
        let (a, b) = (run[from], run[to]);
        let span = b.0 - a.0;
        let mut worst = (0.0f64, from);
        for (index, sample) in run.iter().enumerate().take(to).skip(from + 1) {
            let line = if span > 0.0 { a.1 + (b.1 - a.1) * (sample.0 - a.0) / span } else { b.1 };
            let distance = (sample.1 - line).abs();
            if distance > worst.0 {
                worst = (distance, index);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((from, worst.1));
            stack.push((worst.1, to));
        }
    }
    out.extend(
        run.iter().zip(keep).filter(|(_, keep)| *keep).map(|(sample, _)| AutomationPoint::new(sample.0, sample.1)),
    );
}

/// `base` with the stretch from `from` to `to` replaced by `piece`, whose
/// points lie in that stretch: before `from` and from `to` on, the curve is
/// what `base` was; in between it is `piece`, held from its last point to
/// `to`. Without a `to` the stretch runs to the end, and nothing of `base`
/// comes after it. With an empty `base` the result is `piece` alone.
///
/// This is how a recorded gesture goes into a clip that already has a
/// curve: it replaces what was there for as long as it was heard instead.
pub fn splice(
    base: &[AutomationPoint],
    from: Beats,
    to: Option<Beats>,
    piece: &[AutomationPoint],
) -> Vec<AutomationPoint> {
    if piece.is_empty() {
        return base.to_vec();
    }
    if base.is_empty() {
        return piece.to_vec();
    }
    // Up to the splice the old curve keeps its course...
    let before = value_before(base, from);
    let mut result: Vec<AutomationPoint> = base.iter().filter(|point| point.beat < from).copied().collect();
    let cut = result.len();
    if let (Some(last), Some(next)) = (result.last_mut(), base.get(cut)) {
        last.shape = part_of(last.shape, (from - last.beat) / (next.beat - last.beat));
    }
    if !result.last().is_some_and(|last| last.shape == Segment::Hold && last.value == before) {
        result.push(AutomationPoint::new(from, before));
    }
    if piece[0].beat > from {
        result.push(AutomationPoint { beat: from, value: piece[0].value, shape: Segment::Hold });
    }
    result.extend_from_slice(piece);
    let Some(to) = to else { return tidied(result) };
    // ...and takes it up again where the splice ends.
    if let Some(last) = result.last_mut() {
        last.shape = Segment::Hold;
    }
    let next = base.partition_point(|point| point.beat <= to);
    let shape = match next.checked_sub(1).map(|index| base[index]) {
        Some(previous) if next < base.len() => {
            part_of(previous.shape, (base[next].beat - to) / (base[next].beat - previous.beat))
        }
        _ => Segment::Linear,
    };
    result.push(AutomationPoint { beat: to, value: automation_value(base, to), shape });
    result.extend(base[next..].iter().copied());
    tidied(result)
}

/// The shape of a stretch cut from either end of a segment of `shape`,
/// `share` of the segment long: with it the stretch follows exactly the
/// course the whole segment took there. A bent line bends evenly along its
/// length, so a part of it is a line bent by that much less.
fn part_of(shape: Segment, share: f64) -> Segment {
    match shape {
        Segment::Curve(bend) if share.is_finite() => Segment::Curve(bend * share.clamp(0.0, 1.0) as f32),
        shape => shape,
    }
}

/// Drops points that say nothing: a repeat of the point before, the middle
/// one of three on one beat, and one that repeats the held value before it
/// without a ramp starting from it.
fn tidied(points: Vec<AutomationPoint>) -> Vec<AutomationPoint> {
    let total = points.len();
    let mut result: Vec<AutomationPoint> = Vec::with_capacity(total);
    for (index, point) in points.into_iter().enumerate() {
        let starts_ramp = point.shape != Segment::Hold && index + 1 < total;
        if let Some(last) = result.last_mut()
            && last.value == point.value
            && (last.beat == point.beat || (last.shape == Segment::Hold && !starts_ramp))
        {
            if last.beat == point.beat || index + 1 == total {
                last.shape = point.shape;
            }
            continue;
        }
        let count = result.len();
        if count >= 2 && result[count - 1].beat == point.beat && result[count - 2].beat == point.beat {
            result[count - 1] = point;
            continue;
        }
        result.push(point);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(beat: Beats, value: f64) -> AutomationPoint {
        AutomationPoint::new(beat, value)
    }

    fn held(beat: Beats, value: f64) -> AutomationPoint {
        AutomationPoint { beat, value, shape: Segment::Hold }
    }

    #[test]
    fn controllers_are_known_by_name_and_by_number() {
        for (text, kind) in [
            ("expression", ControllerKind::EXPRESSION),
            ("Expression", ControllerKind::EXPRESSION),
            ("mod_wheel", ControllerKind::MOD_WHEEL),
            ("mod wheel", ControllerKind::MOD_WHEEL),
            ("modulation", ControllerKind::MOD_WHEEL),
            ("sustain", ControllerKind::SUSTAIN),
            ("breath", ControllerKind::BREATH),
            ("pitch_bend", ControllerKind::PitchBend),
            ("pitch-wheel", ControllerKind::PitchBend),
            ("pressure", ControllerKind::Pressure),
            ("aftertouch", ControllerKind::Pressure),
            ("11", ControllerKind::EXPRESSION),
            ("cc74", ControllerKind::Controller(74)),
            ("CC 23", ControllerKind::Controller(23)),
            ("119", ControllerKind::Controller(119)),
        ] {
            assert_eq!(ControllerKind::parse(text), Some(kind), "{text}");
        }
        for text in ["", "120", "123", "cc200", "-1", "wobble", "1.5"] {
            assert_eq!(ControllerKind::parse(text), None, "{text}");
        }
        assert_eq!(ControllerKind::EXPRESSION.key(), "expression");
        assert_eq!(ControllerKind::Controller(23).key(), "23");
        assert_eq!(ControllerKind::Controller(23).label(), "CC 23");
        assert_eq!(ControllerKind::MOD_WHEEL.label(), "Mod wheel");
        assert_eq!(ControllerKind::PitchBend.to_string(), "pitch_bend");
        // Every name reads back as the controller it names, and every kind
        // has a place of its own in a table.
        let mut seen = [false; CONTROLLER_KINDS];
        let kinds = (0..=MAX_CONTROLLER)
            .map(ControllerKind::Controller)
            .chain([ControllerKind::Pressure, ControllerKind::PitchBend]);
        for kind in kinds {
            assert_eq!(ControllerKind::parse(&kind.key()), Some(kind));
            assert_eq!(ControllerKind::from_index(kind.index()), Some(kind));
            assert!(!std::mem::replace(&mut seen[kind.index()], true));
        }
        assert_eq!(ControllerKind::from_index(125), None, "a channel message is no controller");
    }

    #[test]
    fn what_springs_back_is_the_wheel_pressure_and_the_pedals() {
        let sprung: Vec<String> = (0..CONTROLLER_KINDS)
            .filter_map(ControllerKind::from_index)
            .filter(|kind| kind.springs_back())
            .map(ControllerKind::key)
            .collect();
        assert_eq!(
            sprung,
            ["sustain", "portamento", "sostenuto", "soft_pedal", "legato", "hold_2", "pressure", "pitch_bend"]
        );
        assert_eq!(ControllerKind::PitchBend.range(), (-1.0, 1.0));
        assert_eq!(ControllerKind::EXPRESSION.range(), (0.0, 1.0));
        assert_eq!((ControllerKind::SUSTAIN.rest(), ControllerKind::EXPRESSION.rest()), (Some(0.0), None));
        assert!(ControllerKind::SUSTAIN.is_switch() && !ControllerKind::PitchBend.is_switch());
    }

    #[test]
    fn events_and_kinds_convert_both_ways() {
        let events = [
            EventKind::Control { controller: 11, value: 0.5 },
            EventKind::ChannelPressure { value: 0.25 },
            EventKind::PitchBend { value: -0.5 },
        ];
        for event in events {
            let (kind, value) = ControllerKind::of_event(event).unwrap();
            assert_eq!(kind.event(value), event);
        }
        assert_eq!(ControllerKind::of_event(EventKind::NoteOff { pitch: 60 }), None);
        assert_eq!(ControllerKind::of_event(EventKind::Control { controller: 123, value: 0.0 }), None);
    }

    #[test]
    fn a_kind_is_saved_as_its_number_or_its_name() {
        let json = |kind: ControllerKind| serde_json::to_string(&kind).unwrap();
        assert_eq!(json(ControllerKind::EXPRESSION), "11");
        assert_eq!(json(ControllerKind::PitchBend), "\"pitch_bend\"");
        assert_eq!(json(ControllerKind::Pressure), "\"pressure\"");
        let read = |text: &str| serde_json::from_str::<ControllerKind>(text);
        assert_eq!(read("11").unwrap(), ControllerKind::EXPRESSION);
        assert_eq!(read("\"pitch_bend\"").unwrap(), ControllerKind::PitchBend);
        assert_eq!(read("\"expression\"").unwrap(), ControllerKind::EXPRESSION, "a name written by hand");
        assert!(read("123").is_err() && read("-4").is_err() && read("\"wobble\"").is_err() && read("1.5").is_err());
    }

    #[test]
    fn curves_are_checked_and_ordered() {
        let ok = validated(ControllerKind::EXPRESSION, vec![point(4.0, 1.0), point(0.0, 0.0), held(4.0, 0.5)]).unwrap();
        assert_eq!(ok, [point(0.0, 0.0), point(4.0, 1.0), held(4.0, 0.5)], "points on one beat keep their order");
        for bad in
            [point(-1.0, 0.5), point(0.0, 1.5), point(0.0, -0.1), point(f64::NAN, 0.5), point(0.0, f64::INFINITY)]
        {
            assert!(validated(ControllerKind::EXPRESSION, vec![bad]).is_err(), "{bad:?}");
        }
        assert!(validated(ControllerKind::PitchBend, vec![point(0.0, -1.0)]).is_ok());
        assert!(validated(ControllerKind::PitchBend, vec![point(0.0, -1.1)]).is_err());
        assert!(validated(ControllerKind::Controller(123), vec![point(0.0, 0.0)]).is_err());
    }

    #[test]
    fn the_value_before_a_step_is_the_side_before() {
        let points = [point(0.0, 0.0), point(4.0, 1.0), held(4.0, 0.25), point(8.0, 0.75)];
        assert_eq!((value_before(&points, 4.0), automation_value(&points, 4.0)), (1.0, 0.25));
        assert_eq!(value_before(&points, 8.0), 0.25, "a held point holds up to the next");
        assert_eq!(value_before(&points, 2.0), 0.5);
        assert_eq!((value_before(&points, -1.0), value_before(&points, 99.0)), (0.0, 0.75));
        assert_eq!(value_before(&points, 0.0), 0.0);
    }

    fn curve(controller: ControllerKind, points: &[AutomationPoint]) -> ControllerCurve {
        ControllerCurve { controller, points: points.to_vec() }
    }

    fn clip(start: Beats, end: Beats, offset: Beats, period: Option<Beats>, curve: &ControllerCurve) -> LaneClip<'_> {
        LaneClip { start, end, offset, period, curve }
    }

    #[test]
    fn a_lane_follows_the_clip_under_the_playhead_and_holds_between_clips() {
        let swell = curve(ControllerKind::EXPRESSION, &[point(0.0, 0.2), point(4.0, 1.0)]);
        let fall = curve(ControllerKind::EXPRESSION, &[point(0.0, 0.8), point(2.0, 0.4)]);
        let lane = lane_of(
            ControllerKind::EXPRESSION,
            &[clip(4.0, 8.0, 0.0, None, &swell), clip(12.0, 16.0, 0.0, None, &fall)],
        );
        let at = |beat: Beats| lane.value_at(beat);
        assert_eq!(at(0.0), 0.2, "before the first clip it waits at that clip's opening value");
        assert!(at(4.0) == 0.2 && (at(6.0) - 0.6).abs() < 1e-12);
        assert_eq!((at(8.0), at(10.0)), (1.0, 1.0), "after the clip it stays where the clip left it");
        assert!(at(12.0) == 0.8 && (at(13.0) - 0.6).abs() < 1e-12 && at(15.0) == 0.4);
        assert_eq!(at(99.0), 0.4);
        assert_eq!(lane.end(), 16.0);
    }

    #[test]
    fn what_springs_back_returns_to_rest_where_no_clip_holds_it() {
        let bend = curve(ControllerKind::PitchBend, &[point(0.0, 0.0), point(4.0, 1.0)]);
        let lane = lane_of(ControllerKind::PitchBend, &[clip(4.0, 8.0, 0.0, None, &bend)]);
        assert_eq!((lane.value_at(0.0), lane.value_at(6.0)), (0.0, 0.5));
        assert!((lane.value_at(7.999) - 1.0).abs() < 1e-3);
        assert_eq!(
            (lane.value_at(8.0), lane.value_at(20.0)),
            (0.0, 0.0),
            "the wheel is not left bent at the clip's end"
        );

        let pedal = curve(ControllerKind::SUSTAIN, &[held(0.0, 1.0)]);
        let lane = lane_of(ControllerKind::SUSTAIN, &[clip(0.0, 4.0, 0.0, None, &pedal)]);
        assert_eq!((lane.value_at(3.9), lane.value_at(4.0)), (1.0, 0.0), "nor the pedal down");

        // And it is at rest until its curve's first point: a pedal that
        // goes down on the third beat is not down on the first two.
        let pedal = curve(ControllerKind::SUSTAIN, &[held(2.0, 1.0), held(3.0, 0.0)]);
        assert_eq!(
            (pedal.value_at(0.0), pedal.value_at(1.99), pedal.value_at(2.0), pedal.value_at(3.0)),
            (0.0, 0.0, 1.0, 0.0)
        );
        assert_eq!(pedal.played(), [held(0.0, 0.0), held(2.0, 1.0), held(3.0, 0.0)]);
        let lane = lane_of(ControllerKind::SUSTAIN, &[clip(8.0, 12.0, 0.0, Some(4.0), &pedal)]);
        assert_eq!(
            (lane.value_at(8.0), lane.value_at(9.9), lane.value_at(10.0), lane.value_at(11.0)),
            (0.0, 0.0, 1.0, 0.0)
        );
        // A controller that stays waits at its first value instead.
        let swell = curve(ControllerKind::EXPRESSION, &[point(2.0, 0.5), point(3.0, 1.0)]);
        assert_eq!((swell.value_at(0.0), swell.played().len()), (0.5, 2));
    }

    #[test]
    fn a_lane_loops_and_starts_where_the_clip_does() {
        let ramp = curve(ControllerKind::MOD_WHEEL, &[point(0.0, 0.0), point(2.0, 1.0), point(6.0, 0.5)]);
        // A two-beat loop entered half a beat in, over five beats.
        let lane = lane_of(ControllerKind::MOD_WHEEL, &[clip(10.0, 15.0, 0.5, Some(2.0), &ramp)]);
        let at = |beat: Beats| lane.value_at(beat);
        assert_eq!((at(10.0), at(11.0)), (0.25, 0.75));
        assert_eq!((at(11.5), at(12.5), at(13.5)), (0.0, 0.5, 0.0), "it wraps round where the loop does");
        assert_eq!(at(15.0), 0.75, "and stays where the last pass had got to");
        assert_eq!(at(0.0), 0.25);

        // A loop that ends exactly on its last beat leaves the controller
        // at the loop's end, not back at its start.
        let lane = lane_of(ControllerKind::MOD_WHEEL, &[clip(0.0, 4.0, 0.0, Some(2.0), &ramp)]);
        assert_eq!((lane.value_at(3.0), lane.value_at(4.0)), (0.5, 1.0));

        // Without a loop the offset simply slides the curve.
        let lane = lane_of(ControllerKind::MOD_WHEEL, &[clip(0.0, 4.0, 1.0, None, &ramp)]);
        assert_eq!((lane.value_at(0.0), lane.value_at(1.0), lane.value_at(4.0)), (0.5, 1.0, 0.625));
    }

    #[test]
    fn where_clips_overlap_the_later_one_is_heard() {
        let low = curve(ControllerKind::EXPRESSION, &[point(0.0, 0.25)]);
        let high = curve(ControllerKind::EXPRESSION, &[point(0.0, 0.75)]);
        let lane =
            lane_of(ControllerKind::EXPRESSION, &[clip(0.0, 16.0, 0.0, None, &low), clip(4.0, 8.0, 0.0, None, &high)]);
        let at = |beat: Beats| lane.value_at(beat);
        assert_eq!((at(2.0), at(4.0), at(7.9), at(8.0), at(15.0)), (0.25, 0.75, 0.75, 0.25, 0.25));
        assert_eq!(lane.spans.len(), 3, "the long clip is heard again after the short one");
        // The piece after the overlap carries on from where the long clip
        // would have been.
        let ramp = curve(ControllerKind::EXPRESSION, &[point(0.0, 0.0), point(16.0, 1.0)]);
        let lane =
            lane_of(ControllerKind::EXPRESSION, &[clip(0.0, 16.0, 0.0, None, &ramp), clip(4.0, 8.0, 0.0, None, &high)]);
        assert_eq!((lane.value_at(2.0), lane.value_at(6.0), lane.value_at(12.0)), (0.125, 0.75, 0.75));
    }

    /// A curve passes within `tolerance` of every event it was thinned from.
    fn assert_follows(points: &[AutomationPoint], events: &[(Beats, f64)], tolerance: f64) {
        for &(beat, value) in events {
            let got = automation_value(points, beat);
            assert!((got - value).abs() <= tolerance + 1e-12, "at beat {beat}: {got} for {value}");
        }
    }

    #[test]
    fn a_smooth_gesture_thins_to_a_few_points_that_follow_it() {
        // A swell and a fall, reported every hundredth of a beat in the
        // 128 steps of a MIDI controller.
        let events: Vec<(Beats, f64)> = (0..=400)
            .map(|n| {
                let beat = n as f64 / 100.0;
                let value = (std::f64::consts::PI * beat / 4.0).sin();
                (beat, (value * 127.0).round() / 127.0)
            })
            .collect();
        let points = thin(&events, false, THIN_TOLERANCE, 0.1);
        assert!(points.len() < 24, "{} points for {} events", points.len(), events.len());
        assert_follows(&points, &events, THIN_TOLERANCE);
        assert!(points.iter().all(|point| point.shape == Segment::Linear));
        assert_eq!((points[0].beat, points.last().unwrap().beat), (0.0, 4.0));
    }

    #[test]
    fn a_rest_between_gestures_is_held_and_then_jumps() {
        // Up to half way, a long rest, then on up; then a leap.
        let mut events: Vec<(Beats, f64)> = (0..=10).map(|n| (n as f64 * 0.02, n as f64 * 0.05)).collect();
        events.extend((0..=10).map(|n| (2.0 + n as f64 * 0.02, 0.5 + n as f64 * 0.05)));
        events.push((3.0, 0.1));
        let points = thin(&events, false, THIN_TOLERANCE, 0.1);
        assert_eq!(points, [point(0.0, 0.0), point(0.2, 0.5), point(2.0, 0.5), held(2.2, 1.0), point(3.0, 0.1)]);
        assert_eq!(automation_value(&points, 1.0), 0.5, "it rested where it was");
        assert_eq!((automation_value(&points, 2.99), automation_value(&points, 3.0)), (1.0, 0.1), "and leapt");
    }

    #[test]
    fn a_slow_swell_is_a_line_not_a_staircase() {
        // One step of a MIDI controller every quarter of a beat: far apart
        // in time, a step apart in value.
        let events: Vec<(Beats, f64)> = (0..=64).map(|n| (n as f64 * 0.25, n as f64 / 127.0)).collect();
        let points = thin(&events, false, THIN_TOLERANCE, 0.1);
        assert_eq!(points, [point(0.0, 0.0), point(16.0, 64.0 / 127.0)]);
        // The same swell, played after a leap from far away, starts with
        // the leap.
        let mut leaping = vec![(0.0, 0.9)];
        leaping.extend(events.iter().map(|(beat, value)| (beat + 4.0, *value)));
        let points = thin(&leaping, false, THIN_TOLERANCE, 0.1);
        assert_eq!(points, [held(0.0, 0.9), point(4.0, 0.0), point(20.0, 64.0 / 127.0)]);
    }

    #[test]
    fn a_pedal_is_steps_exactly_where_they_happened() {
        let events = [(0.5, 1.0), (0.5, 1.0), (1.25, 0.0), (1.3, 0.0), (2.0, 1.0), (3.5, 0.0)];
        let points = thin(&events, true, THIN_TOLERANCE, 0.1);
        assert_eq!(points, [held(0.5, 1.0), held(1.25, 0.0), held(2.0, 1.0), held(3.5, 0.0)]);
        // A half-pedalling pedal keeps its positions, as steps.
        let half = thin(&[(0.0, 0.4), (0.01, 0.6), (0.02, 1.0)], true, THIN_TOLERANCE, 0.1);
        assert_eq!(half, [held(0.0, 0.4), held(0.01, 0.6), held(0.02, 1.0)]);
        // A button on any controller that only says "off" and "on" is one too.
        let button = thin(&[(0.0, 0.0), (0.05, 1.0), (0.1, 0.0)], false, THIN_TOLERANCE, 0.1);
        assert_eq!(button, [held(0.0, 0.0), held(0.05, 1.0), held(0.1, 0.0)]);
        // Nothing in, nothing out; one report is one point.
        assert_eq!(thin(&[], false, THIN_TOLERANCE, 0.1), []);
        assert_eq!(thin(&[(1.0, 0.3)], false, THIN_TOLERANCE, 0.1), [point(1.0, 0.3)]);
        assert_eq!(thin(&[(f64::NAN, 0.3), (1.0, f64::NAN)], false, THIN_TOLERANCE, 0.1), []);
    }

    #[test]
    fn a_gesture_spliced_into_a_curve_replaces_only_its_stretch() {
        let base = [point(0.0, 0.0), point(8.0, 0.8)];
        let piece = [point(2.0, 1.0), point(3.0, 0.5)];
        let spliced = splice(&base, 2.0, Some(4.0), &piece);
        let at = |beat: Beats| automation_value(&spliced, beat);
        assert!((at(1.0) - 0.1).abs() < 1e-12 && (value_before(&spliced, 2.0) - 0.2).abs() < 1e-12, "{spliced:?}");
        assert_eq!((at(2.0), at(2.5), at(3.0), at(3.9)), (1.0, 0.75, 0.5, 0.5), "held to the end of the stretch");
        assert!((at(4.0) - 0.4).abs() < 1e-12 && (at(6.0) - 0.6).abs() < 1e-12, "the old curve takes over again");
        assert_eq!(at(8.0), 0.8);

        // Nothing there before: the gesture is the curve.
        assert_eq!(splice(&[], 2.0, Some(4.0), &piece), piece);
        assert_eq!(splice(&base, 2.0, Some(4.0), &[]), base);
        // Without an end, nothing of the old curve comes after the gesture.
        let open = splice(&base, 2.0, None, &piece);
        let at = |beat: Beats| automation_value(&open, beat);
        assert!((at(1.0) - 0.1).abs() < 1e-12 && at(2.0) == 1.0 && at(3.0) == 0.5 && at(8.0) == 0.5, "{open:?}");
        // A curve that had settled before the stretch returns to where it
        // had settled.
        let settled = [point(0.0, 0.3), point(1.0, 0.6)];
        let spliced = splice(&settled, 2.0, Some(4.0), &piece);
        let at = |beat: Beats| automation_value(&spliced, beat);
        assert_eq!((at(1.5), at(2.0), at(3.5), at(4.0), at(9.0)), (0.6, 1.0, 0.5, 0.6, 0.6));
        // A bent segment that the stretch cuts through keeps its course
        // on both sides of it, to the last digit a float can tell.
        let bent = [AutomationPoint { beat: 0.0, value: 0.0, shape: Segment::Curve(0.8) }, point(8.0, 1.0)];
        let spliced = splice(&bent, 3.0, Some(5.0), &[point(3.0, 0.5), point(4.0, 0.25)]);
        for beat in [0.5, 1.0, 2.0, 2.99, 5.0, 5.5, 6.0, 7.0, 7.99] {
            let (was, is) = (automation_value(&bent, beat), automation_value(&spliced, beat));
            assert!((was - is).abs() < 1e-6, "at beat {beat}: {is} for {was}");
        }
        assert_eq!(automation_value(&spliced, 3.5), 0.375);

        // One that begins after it is untouched from where it begins.
        let later = [point(6.0, 0.2), point(7.0, 0.9)];
        let spliced = splice(&later, 2.0, Some(4.0), &piece);
        let at = |beat: Beats| automation_value(&spliced, beat);
        assert_eq!((at(0.0), at(2.0), at(3.0), at(4.0), at(6.0), at(7.0)), (0.2, 1.0, 0.5, 0.2, 0.2, 0.9));
    }
}
