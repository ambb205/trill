//! Where the camera is at a beat.
//!
//! A [`Camera`] is worked out from the plan and the beat alone, never from
//! the frame before: a film can be made from any point, in any order, and
//! comes out the same. Inside a take the camera follows the playhead and
//! creeps in; between two takes it either cuts or travels, and a travel is
//! a blend of the two framings by how far through it the beat is. Lanes
//! open as their parts come in and close when they have gone.

use crate::director::{Entry, Kind, Plan, Take};
use crate::score::{Part, Score};
use trill_core::{Beats, TrackId};

/// Where across the time axis the playhead stands while the camera follows
/// it, 0 at the left edge.
pub const ANCHOR: f64 = 0.36;
/// The overview shows this much of the song's length to either side of it.
const OVERVIEW_MARGIN: f64 = 0.03;
/// A lane starts to open this many bars before its part comes in and is
/// open half a bar before it; it closes as long after the part has gone.
const LANE_OPENS: (f64, f64) = (2.0, 0.5);
/// Semitones kept clear above and below the notes of a roll, and the
/// fewest a roll shows.
const ROLL_MARGIN: f32 = 2.5;
const ROLL_FEWEST: f32 = 15.0;

/// One part shown as a piano roll.
#[derive(Clone, Debug, PartialEq)]
pub struct Roll {
    pub track: TrackId,
    /// How much of its place it takes, 0..=1: under 1 while it comes or
    /// goes during a camera move.
    pub weight: f32,
    /// The pitches at the bottom and the top edge. Not read for drums,
    /// which show their own rows.
    pub low: f32,
    pub high: f32,
    /// For a drum part, the drums on stage, lowest first, each with how
    /// open its row is (0..=1): the ones its take plays. A row opens and
    /// closes as the camera moves between takes that play other drums.
    pub drums: Vec<(u8, f32)>,
}

/// One lane of the arrangement.
#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub track: TrackId,
    /// 0 closed, 1 open.
    pub open: f32,
}

/// The camera at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// The playhead, which the picture is of. Held at the song's ends.
    pub beat: Beats,
    /// The beat at the left edge of the time axis.
    pub origin: Beats,
    /// How many beats the time axis holds.
    pub window: Beats,
    /// The share of the stage's height the piano rolls take.
    pub roll_share: f32,
    pub rolls: Vec<Roll>,
    /// Every part of the score, in its order.
    pub lanes: Vec<Lane>,
    /// The take the beat is in.
    pub take: usize,
    /// How much the frame is the overview of the whole song, 0..=1.
    pub overview: f32,
    /// 0 at rest, up to 1 in the middle of a move between two takes.
    pub travel: f32,
    /// Beats since the camera last cut, if it has.
    pub since_cut: Option<Beats>,
}

impl Camera {
    /// The camera at `beat`. `width` says how wide the time axis is against
    /// the one a 16:9 frame has (1.0): a narrower frame holds
    /// proportionally fewer beats, so everything keeps its size.
    pub fn at(score: &Score, plan: &Plan, beat: Beats, width: f64) -> Self {
        let beat = beat.clamp(0.0, score.length);
        let index = plan.take_at(beat);
        let Some(take) = plan.takes.get(index) else {
            return Self::empty(score, beat);
        };
        let here = Framing::of(score, take, beat, width);
        let (mut framing, mut travel) = (here.clone(), 0.0);
        // On the way out to the next take, or on the way in from the last.
        if let Some(next) = plan.takes.get(index + 1)
            && let Some(half) = half_move(take, next)
            && beat > next.start - half
        {
            let through = ((beat - (next.start - half)) / (2.0 * half)) as f32;
            framing = here.blend(&Framing::of(score, next, beat, width), ease(through));
            travel = 1.0 - (2.0 * through - 1.0).abs();
        } else if index > 0
            && let Some(half) = half_move(&plan.takes[index - 1], take)
            && beat < take.start + half
        {
            let through = ((beat - (take.start - half)) / (2.0 * half)) as f32;
            framing = Framing::of(score, &plan.takes[index - 1], beat, width).blend(&here, ease(through));
            travel = 1.0 - (2.0 * through - 1.0).abs();
        }
        let since_cut = plan.takes[..=index]
            .iter()
            .rev()
            .find(|take| take.entry == Entry::Cut && take.start > 0.0)
            .map(|take| beat - take.start);
        let bar = score.bar();
        let lanes = score
            .parts
            .iter()
            .map(|part| {
                let open = lane_open(part, beat, bar);
                // The overview is of every lane the song has.
                Lane { track: part.track, open: open + (1.0 - open) * framing.overview }
            })
            .collect();
        Self {
            beat,
            origin: beat - framing.place * framing.window,
            window: framing.window,
            roll_share: framing.roll_share,
            rolls: framing.rolls,
            lanes,
            take: index,
            overview: framing.overview,
            travel,
            since_cut,
        }
    }

    fn empty(score: &Score, beat: Beats) -> Self {
        Self {
            beat,
            origin: 0.0,
            window: score.length.max(1.0),
            roll_share: 0.0,
            rolls: Vec::new(),
            lanes: Vec::new(),
            take: 0,
            overview: 1.0,
            travel: 0.0,
            since_cut: None,
        }
    }

    /// Where `beat` falls across the time axis, 0 at its left edge and 1
    /// at its right.
    pub fn across(&self, beat: Beats) -> f64 {
        (beat - self.origin) / self.window
    }

    /// The last beat in the frame.
    pub fn end(&self) -> Beats {
        self.origin + self.window
    }
}

/// Half the length of the move that enters `into`, if it is entered by
/// one: no longer than half of either take, so that moves never overlap.
fn half_move(from: &Take, into: &Take) -> Option<Beats> {
    let Entry::Move(beats) = into.entry else { return None };
    let half = (beats / 2.0).min((from.end - from.start) / 2.0).min((into.end - into.start) / 2.0);
    (half > 0.0).then_some(half)
}

/// Slow out of one framing and slow into the other.
pub(crate) fn ease(through: f32) -> f32 {
    let t = through.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// How open the lane of `part` is at `beat`.
fn lane_open(part: &Part, beat: Beats, bar: Beats) -> f32 {
    let (far, near) = (LANE_OPENS.0 * bar, LANE_OPENS.1 * bar);
    let ramp = |distance: f64| (1.0 - ((distance - near) / (far - near)).clamp(0.0, 1.0)) as f32;
    let open = part
        .spans
        .iter()
        .map(|(start, end)| {
            if beat < *start {
                ramp(start - beat)
            } else if beat > *end {
                ramp(beat - end)
            } else {
                1.0
            }
        })
        .fold(0.0, f32::max);
    open * open * (3.0 - 2.0 * open)
}

/// What one take frames at a beat, in numbers that can be blended.
#[derive(Clone, Debug)]
struct Framing {
    window: Beats,
    /// Where across the time axis the playhead stands.
    place: f64,
    roll_share: f32,
    rolls: Vec<Roll>,
    overview: f32,
}

impl Framing {
    fn of(score: &Score, take: &Take, beat: Beats, width: f64) -> Self {
        let shot = &take.shot;
        if shot.kind == Kind::Overview {
            let margin = score.length * OVERVIEW_MARGIN;
            let window = score.length + 2.0 * margin;
            return Self { window, place: (beat + margin) / window, roll_share: 0.0, rolls: Vec::new(), overview: 1.0 };
        }
        let through = ((beat - take.start) / (take.end - take.start).max(1e-9)).clamp(0.0, 1.0);
        let window = (shot.window.0 + (shot.window.1 - shot.window.0) * through) * width;
        let rolls = shot
            .rolls
            .iter()
            .filter_map(|track| score.part(*track))
            .map(|part| {
                let (low, high) = roll_range(part, take.start, take.end);
                Roll { track: part.track, weight: 1.0, low, high, drums: drums_played(part, take.start, take.end) }
            })
            .collect();
        Self { window, place: ANCHOR, roll_share: shot.roll_share, rolls, overview: 0.0 }
    }

    /// This framing on the way to `other`: `through` 0 is this one, 1 the
    /// other. The zoom is blended as a ratio, so a move from far to near
    /// keeps one pace all the way.
    fn blend(&self, other: &Self, through: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * through;
        let window = (self.window.ln() + (other.window.ln() - self.window.ln()) * through as f64).exp();
        let mut rolls: Vec<Roll> = Vec::new();
        for roll in &self.rolls {
            let there = other.rolls.iter().find(|other| other.track == roll.track);
            rolls.push(match there {
                // In both: it stays, and its range travels.
                Some(there) => Roll {
                    track: roll.track,
                    weight: 1.0,
                    low: mix(roll.low, there.low),
                    high: mix(roll.high, there.high),
                    drums: blend_drums(&roll.drums, &there.drums, through),
                },
                None => Roll { weight: 1.0 - through, ..roll.clone() },
            });
        }
        for roll in &other.rolls {
            if !self.rolls.iter().any(|mine| mine.track == roll.track) {
                rolls.push(Roll { weight: through, ..roll.clone() });
            }
        }
        Self {
            window,
            place: self.place + (other.place - self.place) * through as f64,
            roll_share: mix(self.roll_share, other.roll_share),
            rolls,
            overview: mix(self.overview, other.overview),
        }
    }
}

/// The drums `part` plays between `from` and `to`, lowest first, each
/// fully open. Empty for a pitched part.
pub(crate) fn drums_played(part: &Part, from: Beats, to: Beats) -> Vec<(u8, f32)> {
    if !part.drums {
        return Vec::new();
    }
    let mut keys: Vec<u8> = part.starting_between(from, to).iter().map(|note| note.pitch).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter().map(|key| (key, 1.0)).collect()
}

/// The drums of one take on the way to those of another: the ones both
/// play stay, the others close or open.
fn blend_drums(from: &[(u8, f32)], to: &[(u8, f32)], through: f32) -> Vec<(u8, f32)> {
    let open =
        |drums: &[(u8, f32)], key: u8| drums.iter().find(|(found, _)| *found == key).map_or(0.0, |(_, open)| *open);
    let mut keys: Vec<u8> = from.iter().chain(to).map(|(key, _)| *key).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter().map(|key| (key, open(from, key) + (open(to, key) - open(from, key)) * through)).collect()
}

/// The pitches a roll of `part` shows for the notes it plays between
/// `from` and `to`: all of them with room to spare, and never so few rows
/// that a line of three notes fills the frame.
pub(crate) fn roll_range(part: &Part, from: Beats, to: Beats) -> (f32, f32) {
    let notes = part.starting_between(from, to);
    let (low, high) = match notes.is_empty() {
        true => (part.low, part.high),
        false => notes.iter().fold((127u8, 0u8), |(low, high), note| (low.min(note.pitch), high.max(note.pitch))),
    };
    let (mut low, mut high) = (low as f32 - ROLL_MARGIN, high as f32 + 1.0 + ROLL_MARGIN);
    let short = ROLL_FEWEST - (high - low);
    if short > 0.0 {
        low -= short / 2.0;
        high += short / 2.0;
    }
    (low, high)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::Shot;
    use trill_core::{Note, Project};

    fn notes(from: usize, to: usize, pitch: u8) -> Vec<Note> {
        (from * 4..to * 4)
            .map(|beat| Note { pitch, start: (beat - from * 4) as f64, length: 0.5, velocity: 100 })
            .collect()
    }

    /// Sixty-four bars: a bass throughout, a lead from bar 16 to bar 24.
    fn score() -> (Score, TrackId, TrackId) {
        let mut project = Project::new("song");
        let bass = project.add_track("Bass");
        let clip = project.add_midi_clip(bass, "bass", 0.0, 256.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&notes(0, 64, 36)).unwrap();
        let lead = project.add_track("Lead");
        let clip = project.add_midi_clip(lead, "lead", 64.0, 32.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&notes(16, 24, 72)).unwrap();
        (Score::read(&project), bass, lead)
    }

    fn take(start: Beats, end: Beats, kind: Kind, rolls: &[TrackId], window: (Beats, Beats), entry: Entry) -> Take {
        let roll_share = if rolls.is_empty() { 0.0 } else { 0.7 };
        Take { start, end, section: 0, shot: Shot { kind, rolls: rolls.to_vec(), window, roll_share }, entry }
    }

    fn plan(lead: TrackId) -> Plan {
        Plan {
            takes: vec![
                take(0.0, 8.0, Kind::Overview, &[], (256.0, 256.0), Entry::Cut),
                take(8.0, 64.0, Kind::Wide, &[], (32.0, 28.0), Entry::Move(8.0)),
                take(64.0, 96.0, Kind::Close, &[lead], (8.0, 8.0), Entry::Cut),
                take(96.0, 256.0, Kind::Wide, &[], (32.0, 32.0), Entry::Move(4.0)),
            ],
        }
    }

    #[test]
    fn the_camera_follows_the_playhead_and_creeps_in() {
        let (score, _, lead) = score();
        let plan = plan(lead);
        let early = Camera::at(&score, &plan, 20.0, 1.0);
        let late = Camera::at(&score, &plan, 56.0, 1.0);
        for camera in [&early, &late] {
            assert!((camera.across(camera.beat) - ANCHOR).abs() < 1e-9, "the playhead keeps its place");
            assert_eq!((camera.take, camera.travel, camera.overview), (1, 0.0, 0.0));
        }
        assert!(early.window < 32.0 && late.window < early.window && late.window > 28.0);
        // A frame half as wide holds half as many beats.
        assert!((Camera::at(&score, &plan, 20.0, 0.5).window - early.window / 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_song_opens_with_all_of_itself_in_the_frame() {
        let (score, _, lead) = score();
        let camera = Camera::at(&score, &plan(lead), 0.0, 1.0);
        assert!(camera.origin < 0.0 && camera.end() > score.length);
        assert_eq!(camera.overview, 1.0);
        assert!(camera.lanes.iter().all(|lane| lane.open == 1.0), "every lane, the lead's too");
        // Before the song and after it the camera holds its ends.
        assert_eq!(Camera::at(&score, &plan(lead), -5.0, 1.0), camera);
        assert_eq!(Camera::at(&score, &plan(lead), 999.0, 1.0).beat, score.length);
    }

    #[test]
    fn a_move_travels_without_a_jump_and_a_cut_jumps() {
        let (score, _, lead) = score();
        let plan = plan(lead);
        // From the overview into the first take: eight beats around beat 8.
        let mut before = Camera::at(&score, &plan, 3.9, 1.0);
        assert_eq!(before.travel, 0.0);
        let mut most = 0.0f32;
        for step in 0..=84 {
            let beat = 3.9 + step as f64 * 0.1;
            let camera = Camera::at(&score, &plan, beat, 1.0);
            let ratio = (camera.window / before.window).ln().abs();
            assert!(ratio < 0.12, "at {beat}: the zoom went from {} to {}", before.window, camera.window);
            assert!(camera.window <= before.window + 1e-9, "it only ever moves in");
            most = most.max(camera.travel);
            before = camera;
        }
        assert!(most > 0.95, "the middle of the move is where it travels most");
        // Arrived, and creeping in as the take does.
        assert!(before.window > 31.5 && before.window < 32.0 && before.travel == 0.0, "{}", before.window);

        // Into the close shot the camera cuts.
        let (last, first) = (Camera::at(&score, &plan, 63.999, 1.0), Camera::at(&score, &plan, 64.0, 1.0));
        assert!(last.window > 27.0 && first.window == 8.0);
        assert_eq!((last.rolls.len(), first.rolls.len()), (0, 1));
        assert_eq!(first.since_cut, Some(0.0));
        assert_eq!(Camera::at(&score, &plan, 70.0, 1.0).since_cut, Some(6.0));
        assert_eq!(Camera::at(&score, &plan, 20.0, 1.0).since_cut, None, "the song's first beat is not a cut");
    }

    #[test]
    fn a_roll_fades_as_the_camera_leaves_it_and_frames_the_notes_of_its_take() {
        let (score, _, lead) = score();
        let plan = plan(lead);
        let close = Camera::at(&score, &plan, 80.0, 1.0);
        let roll = &close.rolls[0];
        assert_eq!((roll.track, roll.weight), (lead, 1.0));
        assert!(roll.low < 70.0 && roll.high > 74.0 && roll.high - roll.low >= 15.0, "{roll:?}");
        assert_eq!(close.roll_share, 0.7);
        // The move out of it takes four beats around beat 96.
        let leaving = Camera::at(&score, &plan, 96.0, 1.0);
        assert!((leaving.rolls[0].weight - 0.5).abs() < 1e-6 && (leaving.roll_share - 0.35).abs() < 1e-6);
        assert_eq!(leaving.travel, 1.0);
        let gone = Camera::at(&score, &plan, 98.0, 1.0);
        assert!(gone.rolls.iter().all(|roll| roll.weight == 0.0) && gone.roll_share == 0.0);
    }

    #[test]
    fn a_lane_opens_before_its_part_comes_in_and_closes_after_it_has_gone() {
        let (score, bass, lead) = score();
        let plan = plan(lead);
        let open = |beat: Beats, track: TrackId| {
            Camera::at(&score, &plan, beat, 1.0).lanes.iter().find(|lane| lane.track == track).unwrap().open
        };
        assert_eq!(open(40.0, bass), 1.0);
        assert_eq!(open(40.0, lead), 0.0, "six bars before its first note");
        let opening = open(59.0, lead);
        assert!(opening > 0.0 && opening < 1.0, "{opening}");
        assert_eq!(open(62.0, lead), 1.0, "open half a bar before it plays");
        assert_eq!(open(80.0, lead), 1.0);
        assert_eq!(open(110.0, lead), 0.0, "and closed two bars after its last");
    }

    #[test]
    fn the_same_beat_is_always_the_same_camera() {
        let (score, _, lead) = score();
        let plan = plan(lead);
        for beat in [0.0, 5.5, 8.0, 63.9, 64.0, 95.0, 97.3, 255.0] {
            assert_eq!(Camera::at(&score, &plan, beat, 1.0), Camera::at(&score, &plan, beat, 1.0));
        }
    }
}
