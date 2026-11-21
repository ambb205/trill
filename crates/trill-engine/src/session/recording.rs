//! The session's part in recording sound: the input, a take's audio from
//! the moment it is asked for until it lies beside its track, how long
//! sound takes to go round, and where recordings are kept.
//!
//! # The input
//!
//! A track says which input of the audio device it records
//! ([`Project::set_track_input`]); the device is the machine's
//! ([`AudioConfig::input`](crate::AudioConfig)). The input is open only
//! while something needs it: a track that records one is armed or is
//! monitored, a measurement runs, or somebody is looking at its level
//! ([`Session::watch_input`]). It is closed otherwise, and
//! [`Session::input_status`] says which, so that a front end can always
//! show when a microphone is live.
//!
//! # A take
//!
//! [`Session::record`] records the armed tracks, each as what it records:
//! what a keyboard plays as notes ([`take`](super::take)), an input as
//! audio. Both are one take and one undo step. The audio is written to a
//! file a track as it is sung ([`crate::recorder`]), in the engine's own
//! clock ([`crate::capture`]), from the count-in on. When the take ends,
//! what was sung to the song's first frame is found in the file from what
//! the audio thread said of it ([`AudioMark`]): the frame it had handed
//! over when the song began to roll, and from there the time sound takes
//! to go round ([`RecordingLatency`]). Each pass of a loop is a take of
//! its own, in a lane of the track ([`trill_core::takes`]); the track then
//! plays the first pass, and over it the last pass that ran the whole
//! loop. What the track had under the take is kept as a take too.
//!
//! # Where recordings are kept
//!
//! Beside a saved song, in `<song> files/takes`. An unsaved song's are in
//! a folder of the program's own ([`Session::keep_unsaved_recordings_in`])
//! and come beside the song when it is first saved: copied there, named
//! there by the song and by every state undo can reach, and only then
//! taken away from where they were, so that nothing is lost whenever the
//! program dies. Trill never deletes a recording by itself. It lists the
//! ones no state of the song names any more
//! ([`Session::unused_recordings`]) and deletes those it is asked to.

use super::bin::{Bin, Removed};
use super::{Session, SessionError};
use crate::capture::Health;
use crate::compile::Live;
use crate::engine::{InputInfo, Message};
use crate::live::AudioMark;
use crate::processor::Measure;
use crate::recorder::{FileOf, Growing, TakeWriter};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::project::AudioContent;
use trill_core::{ClipId, Monitoring, Project, TrackId, TrackInput};

/// How long after an input could not be opened it is tried again.
const TRY_AGAIN: Duration = Duration::from_secs(3);
/// How much more of an input is recorded after the song has stopped than
/// sound takes to go round, in seconds: something to fade with.
const HANDLE_SECONDS: f64 = 0.05;
/// The shortest stretch that is worth a take, in beats.
const SHORTEST_TAKE: f64 = 1.0 / 64.0;
/// The shortest beginning of another pass of the loop that is a take, in
/// beats, where the song was stopped in it.
const SHORTEST_PASS: f64 = 1.0;
/// The folder beside a song that holds what was recorded for it.
const TAKES_FOLDER: &str = "takes";

/// The input as it stands; see [`Session::input_status`].
#[derive(Clone, Debug, PartialEq)]
pub struct InputStatus {
    /// The input that is open. A microphone that is open is live: what it
    /// hears is in the program, whether or not anything is done with it.
    pub open: Option<InputInfo>,
    /// Whether anything needs the input right now.
    pub needed: bool,
    /// Why it is not open although something needs it, or what is wrong
    /// with it as it is open, in words fit to show the user.
    pub problem: Option<String>,
    /// By which clock the open input counts time; `None` while it is
    /// closed.
    pub clock: Option<InputClock>,
}

/// By which clock an open input counts time: what decides how it is
/// brought into the engine's.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InputClock {
    /// The output's own: an input of the device that is played through.
    /// A frame of the one is a frame of the other.
    Shared,
    /// A clock of its own: the two are told apart by their time stamps,
    /// and the input is read at the ratio they have.
    Own,
    /// It was taken for an input of the output's device, and its time
    /// stamps said otherwise: it is read as one with a clock of its own
    /// from then on, for as long as it is open.
    FoundOwn,
}

impl InputClock {
    /// In words: "the output's own", "its own".
    pub fn said(self) -> &'static str {
        match self {
            InputClock::Shared => "the output's own",
            InputClock::Own => "its own: read by its time stamps",
            InputClock::FoundOwn => {
                "its own: the system has it as part of the output's device, and its time stamps say otherwise"
            }
        }
    }
}

/// Where the correction that is added to what the devices say comes from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Correction {
    /// There is none: the devices' own figures are all that is taken off.
    #[default]
    None,
    /// It was measured, on these devices as they are open.
    Measured,
    /// It was set by hand.
    ByHand,
}

/// How long sound takes to go round, and where each figure is from; see
/// [`Session::recording_latency`]. A take is put where it was sung by
/// taking all of it off; a monitored input is heard that much late.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RecordingLatency {
    /// From the engine to the speaker, as the output device said with its
    /// last buffer.
    pub output_ms: f64,
    /// From the microphone to a buffer, as the input device says.
    pub input_ms: f64,
    /// Handing the input across to the engine's clock: Trill's own, and
    /// exact.
    pub handover_ms: f64,
    /// What lies between the engine and the microphone on the way round
    /// beside the hand-over, where that was measured: it then stands in
    /// place of what the output says, and of what the input leaves
    /// unsaid. A measurement finds it ([`Measured::beyond_ms`]).
    pub measured_ms: Option<f64>,
    /// What was added by hand, on top of either.
    pub adjust_ms: f64,
    /// Where the round trip is from, in a word.
    pub correction: Correction,
    /// All of it: from the microphone to the speaker.
    pub round_trip_ms: f64,
    /// How far the input's clock runs from the output's as it is read
    /// now, in parts a million.
    pub drift_ppm: f64,
}

/// What a measurement of the round trip found; see
/// [`Session::measure_latency`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Measured {
    /// How long the signal took to come back, in milliseconds.
    pub round_trip_ms: f64,
    /// That, less what handing the input across takes: what lies between
    /// the engine and the speaker, and between the microphone and its
    /// buffers, as it is and not as the devices say. This is what the
    /// measurement offers ([`Session::set_measured_latency`]); it does
    /// not move with what a device says from one moment to the next.
    pub beyond_ms: f64,
    /// How much longer the round trip is than the devices said as it was
    /// measured. Negative where they say too much.
    pub more_than_said_ms: f64,
    /// How alike what came back and what was played are, 0 to 1: a
    /// cable gives nearly 1, a microphone at a speaker less.
    pub likeness: f64,
    /// How loud it came back, in decibels of full scale.
    pub heard_db: f64,
}

/// A measurement as it stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Measurement {
    /// None was asked for, or its result was taken.
    Idle,
    Running,
    Done(Measured),
    /// It heard nothing it could use, and why.
    Failed(String),
}

/// One take that a recording left beside a track.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedTake {
    pub track: TrackId,
    /// The take, in its lane.
    pub take: ClipId,
    /// The file it is in.
    pub file: PathBuf,
    pub start_beat: f64,
    pub length_beats: f64,
    pub seconds: f64,
    /// Whether it is what the track plays now.
    pub chosen: bool,
}

/// What the last recording came to; see [`Session::last_recording`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recorded {
    /// Whether the song rolled at all: a take that was called off while
    /// it was counted in has a file and nothing to put on a track.
    pub rolled: bool,
    pub takes: Vec<RecordedTake>,
    /// The highest level of each file, in decibels of full scale.
    pub peaks_db: Vec<(PathBuf, f64)>,
    /// Frames that did not arrive, or found no room on their way to disk,
    /// and are silence where they would have been.
    pub silent_frames: u64,
    /// Why the take ended before it was stopped, if it did.
    pub ended_early: Option<String>,
    /// What was taken off to put it where it was sung, in milliseconds,
    /// and whether that was measured or is what the devices say.
    pub taken_off_ms: f64,
    pub correction: Correction,
}

/// A take's audio while it is recorded, for drawing it as it grows.
#[derive(Clone)]
pub struct GrowingTake {
    pub track: TrackId,
    /// The file, as it is written.
    pub file: Arc<Growing>,
    /// Where in the song the pass that is being recorded began, once the
    /// song is rolling, and the frame of the file that was sung to it.
    pub start_beat: Option<f64>,
    pub first_frame: u64,
}

/// What the audio thread said of a take's audio.
#[derive(Copy, Clone, Debug)]
struct Rolled {
    beat: f64,
    latency: u32,
    delay: u32,
}

/// A stretch of a take's audio that was sung to one stretch of the song.
/// The song goes where it goes while a take is recorded: round its loop
/// or not, to wherever it is sent, past a loop that was switched off. Each
/// time it is no longer where the frames lead to, one stretch ends and
/// another begins; each lands where it was sung.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Stretch {
    /// The frame handed over as it began.
    at: u64,
    start: f64,
    /// Where the song was when it ended; `None` while it goes on.
    end: Option<f64>,
    /// Where the song came round from, and to, if it began by the song
    /// coming round its loop: how long a whole pass of it is.
    round: Option<(f64, f64)>,
}

/// A take's audio between [`Session::record`] and its end.
pub(super) struct AudioTake {
    id: u32,
    /// Whether the audio thread has taken the take up: what it says
    /// before that is an earlier take's.
    begun: bool,
    writer: Option<TakeWriter>,
    /// The tracks that record an input, in the order of their files.
    tracks: Vec<TrackId>,
    rolled: Option<Rolled>,
    /// The stretches of the song the take was sung to, in the order they
    /// were sung: as the audio thread said them, and never as a switch
    /// leads anyone to expect.
    stretches: Vec<Stretch>,
    stopped: Option<(u64, f64)>,
    ended: bool,
    /// The stretch the take is punched in over, if it is.
    punch: Option<(f64, f64)>,
    /// What stands in place of what the output says, where the round trip
    /// was measured, and what was added by hand: in frames, as the take
    /// began.
    measured: Option<i64>,
    adjust: i64,
    health: Health,
    ended_early: Option<String>,
}

/// How long an input has to have been open before a measurement plays
/// its signal, in seconds: until the two devices' clocks have been told
/// apart well enough that a second of noise comes back as itself.
const SETTLE_SECONDS: f64 = 4.0;

/// A measurement between its beginning and its result.
struct Measuring {
    signal: Arc<[f32]>,
    /// What is still to be handed to the audio thread: the input has not
    /// been open long enough yet.
    waiting: Option<Box<Measure>>,
    began: Instant,
    /// What the devices said as it began: from the engine to the speaker,
    /// and from the microphone to the engine, in frames.
    latency: f64,
    delay: u32,
    channel: u16,
}

/// What the session keeps about recording sound.
#[derive(Default)]
pub(super) struct Audio {
    /// Where an unsaved song's takes go.
    unsaved: Option<PathBuf>,
    take: Option<AudioTake>,
    last: Option<Recorded>,
    measured_ms: Option<f64>,
    adjust_ms: f64,
    /// How many are looking at the input's level.
    watchers: u32,
    problem: Option<String>,
    tried: Option<Instant>,
    measuring: Option<Measuring>,
    measured: Option<Result<Measured, String>>,
    /// The take that is heard in place of what its track plays.
    heard: Option<ClipId>,
    /// What ended a take early, until it has been said.
    notice: Option<String>,
    punch: bool,
    /// How many songs this session has had before the one that is open.
    songs: u32,
    /// Whose hand opened what: see [`Session::hold_to_the_hand`].
    hand: Let,
    /// Where a recording that is removed goes.
    bin: Bin,
    /// What a take's files are held to: a WAV file's own length and the
    /// disk, unless a test says less.
    limits: trill_media::GrowingLimits,
}

/// What the user's own hand has let the input be opened for, in this run
/// of the program and in this song.
#[derive(Default)]
struct Let {
    /// The armed tracks that record an input, where a hand made them so.
    open: HashSet<TrackId>,
    /// The tracks whose input is heard through them, and as what (while
    /// armed, or always), where a hand switched that on.
    heard: HashMap<TrackId, Monitoring>,
    /// The tracks whose input would be heard, let or held, and as what,
    /// when this was last looked at: what is the same after an act did
    /// not come about by it.
    wanted: HashMap<TrackId, Monitoring>,
    /// What was held or disarmed for want of a hand, until it is said.
    said: Option<String>,
}

impl Let {
    /// Whether hearing the input of `track` as `heard` is held: no hand
    /// switched it on.
    fn holds(&self, track: TrackId, heard: Monitoring) -> bool {
        self.heard.get(&track) != Some(&heard)
    }
}

impl Audio {
    /// Whether a take that is punched in is being recorded: the song then
    /// plays through and does not go round.
    pub(super) fn punching(&self) -> bool {
        self.take.as_ref().is_some_and(|take| take.punch.is_some())
    }

    pub(super) fn heard_take(&self) -> Option<ClipId> {
        self.heard
    }

    /// Whether the take's audio has all been handed over, or there is none.
    pub(super) fn ended(&self) -> bool {
        self.take.as_ref().is_none_or(|take| take.ended)
    }
}

impl Session {
    // ---- the input -----------------------------------------------------

    /// The input as it stands: whether it is open, whether anything needs
    /// it, and what is wrong.
    pub fn input_status(&self) -> InputStatus {
        let open = self.engine.input().cloned();
        let problem = self.recording.problem.clone().or_else(|| open.as_ref().and_then(|input| input.warning.clone()));
        let clock = open.as_ref().map(|input| match (input.shares_clock, self.engine.input_parted()) {
            (true, false) => InputClock::Shared,
            (true, true) => InputClock::FoundOwn,
            (false, _) => InputClock::Own,
        });
        let problem = problem.or_else(|| self.unreachable_input());
        InputStatus { open, needed: self.input_needed(), problem, clock }
    }

    /// How the open input has done since it was opened: the frames its
    /// device never handed over, the ones that came too late to be read,
    /// and how often the reading was put somewhere else at once. `None`
    /// while it is closed.
    pub fn input_health(&self) -> Option<Health> {
        self.engine.input_health()
    }

    /// Says that somebody is looking at the input's level, or has stopped:
    /// the input is open for as long as anybody is. Looking opens the
    /// input, so only the user's own hand begins to
    /// ([`crate::hand::by_hand`]); anybody stops.
    pub fn watch_input(&mut self, watching: bool) -> Result<(), SessionError> {
        if watching && !crate::hand::is_by_hand() {
            return Err(SessionError::Recording(
                "looking at what arrives on the input opens the microphone: the user does that themselves, in the \
                 audio settings (Show level) or by arming a track"
                    .to_string(),
            ));
        }
        let watchers = &mut self.recording.watchers;
        *watchers = if watching { *watchers + 1 } else { watchers.saturating_sub(1) };
        self.recording.tried = None;
        Ok(())
    }

    /// The highest level each channel of the input has had since this was
    /// last asked (1 is full scale), and whether it was at full scale or
    /// over. Empty while the input is closed.
    pub fn input_levels(&mut self) -> Vec<(f32, bool)> {
        self.engine.take_input_levels()
    }

    /// Opens the input now if anything needs it, in place of waiting for
    /// the next round of chores. Why it cannot be opened is the error.
    pub fn ready_input(&mut self) -> Result<(), SessionError> {
        if !self.input_needed() || self.engine.input().is_some() {
            return Ok(());
        }
        match self.engine.open_input() {
            Ok(()) => {
                (self.recording.problem, self.recording.tried) = (None, None);
                self.playing_audio_changed();
                Ok(())
            }
            Err(why) => {
                let sentence = self.cannot_open(&why);
                (self.recording.problem, self.recording.tried) = (Some(sentence.clone()), Some(Instant::now()));
                Err(SessionError::Recording(sentence))
            }
        }
    }

    fn cannot_open(&self, why: &str) -> String {
        format!("the input cannot be opened: {why}. Choose the device that is recorded from in the audio settings")
    }

    /// Whether anything needs the input: a track that records one is
    /// armed or is always monitored, a take is being recorded, a
    /// measurement runs, or somebody is looking at its level.
    fn input_needed(&self) -> bool {
        // An armed track that records an input was made so by a hand, or
        // it is no longer armed; one that is always heard needs the input
        // only once a hand has let it be.
        let wanted = |track: &trill_core::Track| {
            track.input.is_some_and(|input| {
                self.is_armed(track.id)
                    || (input.monitor == Monitoring::Always && !self.recording.hand.holds(track.id, input.monitor))
            })
        };
        self.recording.watchers > 0
            || self.recording.measuring.is_some()
            || self.recording.take.is_some()
            || self.project.tracks.iter().any(wanted)
    }

    /// A track that records a channel the open input has not got, as a
    /// sentence that says what to do.
    fn unreachable_input(&self) -> Option<String> {
        let open = self.engine.input()?;
        let beyond = self.project.tracks.iter().find_map(|track| {
            let input = track.input?;
            let last = input.channel + u16::from(input.stereo);
            (last > open.channels).then_some((track, input))
        })?;
        Some(format!(
            "'{}' records {}, and '{}' has {}: choose another input for the track, or another device in the audio settings",
            beyond.0.name,
            beyond.1.said(),
            open.device,
            match open.channels {
                1 => "one channel".to_string(),
                channels => format!("{channels} channels"),
            }
        ))
    }

    /// What goes on beside the song that the graph has to know of.
    pub(super) fn live_now(&self) -> Live {
        let held = self.project.tracks.iter().filter(|track| self.input_held(track.id)).map(|track| track.id);
        Live { armed: self.armed().iter().copied().collect(), held: held.collect() }
    }

    // ---- whose hand opened it --------------------------------------------

    /// As what the input of `track` would be heard through it now: while
    /// armed, or always. `None` where it is not heard at all.
    fn heard_as(&self, track: &trill_core::Track) -> Option<Monitoring> {
        match track.input?.monitor {
            Monitoring::Always => Some(Monitoring::Always),
            Monitoring::Armed if self.is_armed(track.id) => Some(Monitoring::Armed),
            _ => None,
        }
    }

    /// Whether the input of `track` is held: the song says it is heard
    /// through the track, and nobody's hand switched that on in this run
    /// of the program and in this song. It is then not heard, and not
    /// opened for being heard; [`Session::let_input_be_heard`] lets it go.
    pub fn input_held(&self, track: TrackId) -> bool {
        let Ok(found) = self.project.track(track) else { return false };
        self.heard_as(found).is_some_and(|heard| self.recording.hand.holds(track, heard))
    }

    /// Lets the input of `track` be heard as the song says, where it was
    /// held: the click on its switch. The user's own hand only.
    pub fn let_input_be_heard(&mut self, track: TrackId) -> Result<(), SessionError> {
        let found = self.project.track(track)?;
        let Some(heard) = self.heard_as(found) else { return Ok(()) };
        if !crate::hand::is_by_hand() {
            return Err(SessionError::Recording(format!(
                "letting the input of '{}' be heard opens the microphone into the speakers: the user does that \
                 themselves, with the track's monitor switch",
                found.name
            )));
        }
        if self.recording.hand.heard.insert(track, heard) != Some(heard) {
            self.input_is_to_be_tried();
            self.compile_and_swap();
        }
        Ok(())
    }

    /// What was held or disarmed for want of the user's hand, said once: a
    /// front end shows it.
    pub fn take_input_note(&mut self) -> Option<String> {
        self.recording.hand.said.take()
    }

    /// Holds the input to its rule, after anything that may have changed
    /// what wants it: **the input is opened for a track only by the user's
    /// own hand, in this run of the program and in this song.** The hand
    /// arms a track that records an input, gives an armed track its input,
    /// or switches its monitoring on; an undo or a redo of the hand's is
    /// the hand's too. Nothing else opens it: not what is asked at a door
    /// where nobody says they are the user ([`crate::hand`]), not a song
    /// that is opened, begun or recovered, not a track that is new (a
    /// duplicate, a paste).
    ///
    /// So an armed track that came to record an input any other way is
    /// disarmed, and an input that the song says is heard, and that no
    /// hand switched on, is held ([`Session::input_held`]).
    ///
    /// `there_before` says whether a track was in the song before what
    /// was just done. Arming does not carry into another song, and
    /// neither does anything a hand let: [`Session::forget_the_hand`].
    pub(super) fn hold_to_the_hand(&mut self, there_before: &dyn Fn(TrackId) -> bool) {
        let by_hand = crate::hand::is_by_hand();
        let mut hand = std::mem::take(&mut self.recording.hand);
        let mut disarmed: Vec<(TrackId, String)> = Vec::new();
        let mut held: Vec<String> = Vec::new();
        let mut wanted = HashMap::new();
        for track in &self.project.tracks {
            let id = track.id;
            let this_act = by_hand && there_before(id);
            // Open for a take: armed, and recording an input.
            let mut armed = self.is_armed(id);
            if armed && track.input.is_some() {
                if !hand.open.contains(&id) && this_act {
                    hand.open.insert(id);
                } else if !hand.open.contains(&id) {
                    disarmed.push((id, track.name.clone()));
                    armed = false;
                }
            } else {
                hand.open.remove(&id);
            }
            // Heard through the track, and as what.
            let heard = match track.input.map(|input| input.monitor) {
                Some(Monitoring::Always) => Some(Monitoring::Always),
                Some(Monitoring::Armed) if armed => Some(Monitoring::Armed),
                _ => None,
            };
            match heard {
                None => drop(hand.heard.remove(&id)),
                Some(heard) => {
                    wanted.insert(id, heard);
                    if hand.heard.get(&id) != Some(&heard) {
                        hand.heard.remove(&id);
                        // It came to be heard so by what was just done,
                        // and that was the user's doing.
                        if this_act && hand.wanted.get(&id) != Some(&heard) {
                            hand.heard.insert(id, heard);
                        } else if hand.wanted.get(&id) != Some(&heard) {
                            held.push(track.name.clone());
                        }
                    }
                }
            }
        }
        let there = |id: &TrackId| self.project.track(*id).is_ok();
        hand.open.retain(|id| there(id));
        hand.heard.retain(|id, _| there(id));
        hand.wanted = wanted;
        let mut said = Vec::new();
        if !disarmed.is_empty() {
            let names: Vec<String> = disarmed.iter().map(|(_, name)| format!("'{name}'")).collect();
            said.push(format!(
                "{} {} disarmed: arming a track that records an input opens the microphone, and that is done by \
                 hand, with the track's arm button",
                names.join(", "),
                if names.len() == 1 { "was" } else { "were" }
            ));
        }
        if !held.is_empty() {
            let names: Vec<String> = held.iter().map(|name| format!("'{name}'")).collect();
            said.push(format!(
                "the input of {} is held, not heard: the song says it is heard through the track, and that opens \
                 the microphone into the speakers. A click on the track's monitor switch lets it be heard",
                names.join(", ")
            ));
        }
        if !said.is_empty() {
            hand.said = Some(said.join(". "));
        }
        self.recording.hand = hand;
        if !disarmed.is_empty() {
            self.playing_disarm(&disarmed.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        }
    }

    /// Another song: nothing a hand let in the song before is this one's,
    /// and nothing that was armed there is armed here. Track ids begin
    /// again in every song.
    pub(super) fn forget_the_hand(&mut self) {
        self.recording.hand = Let::default();
    }

    /// Opens the input when something has come to need it, closes it when
    /// nothing does any more, and ends a take whose input has gone. Part
    /// of [`Session::housekeeping`].
    pub(super) fn tend_input(&mut self) {
        if let Some(failure) = self.engine.input_failure() {
            let device =
                self.engine.input().map_or_else(|| "the input".to_string(), |input| format!("'{}'", input.device));
            self.engine.close_input();
            let what = match failure {
                crate::host::StreamFailure::DeviceLost => "was disconnected",
                crate::host::StreamFailure::Failed => "stopped working",
                crate::host::StreamFailure::Stalled => "stopped responding",
            };
            let sentence = format!("{device} {what}");
            self.recording.problem = Some(sentence.clone());
            self.recording.tried = Some(Instant::now());
            self.end_take_early(&format!("{sentence}: the take ends there, whole up to that moment"));
            self.playing_audio_changed();
        }
        match (self.input_needed(), self.engine.input().is_some()) {
            (true, false) if self.recording.tried.is_none_or(|at| at.elapsed() >= TRY_AGAIN) => {
                match self.engine.open_input() {
                    Ok(()) => (self.recording.problem, self.recording.tried) = (None, None),
                    Err(why) => {
                        self.recording.problem = Some(self.cannot_open(&why));
                        self.recording.tried = Some(Instant::now());
                    }
                }
                self.playing_audio_changed();
            }
            (false, true) => {
                self.engine.close_input();
                (self.recording.problem, self.recording.tried) = (None, None);
                self.playing_audio_changed();
            }
            _ => {}
        }
        self.tend_measurement();
        self.tend_audio_take();
    }

    /// Something about what wants the input has changed: it is looked at
    /// again at the next round of chores, whenever opening it last failed.
    pub(super) fn input_is_to_be_tried(&mut self) {
        self.recording.tried = None;
    }

    /// The input was closed with the stream it was read for: it is opened
    /// again, if it is needed, at the next round of chores.
    pub(super) fn input_is_to_be_opened_again(&mut self) {
        self.recording.tried = None;
        self.recording.measuring = None;
    }

    // ---- a take's audio ------------------------------------------------

    /// Whether takes are punched in: with it on, a take records only while
    /// the song is inside the loop's stretch, and the song plays through
    /// it and does not go round. For notes and for audio alike.
    pub fn punch(&self) -> bool {
        self.recording.punch
    }

    pub fn set_punch(&mut self, on: bool) {
        self.recording.punch = on;
    }

    /// Where a take that is asked for now is punched in, if it is.
    pub(super) fn punch_now(&self) -> Option<(f64, f64)> {
        let region = self.project.loop_region;
        (self.recording.punch && region.end > region.start).then_some((region.start, region.end))
    }

    /// Begins the audio of the take numbered `id` on those of `tracks`
    /// that record an input: opens the input, begins their files and hands
    /// the audio thread its ends of them. Nothing is begun if anything
    /// cannot be: the error says what to do.
    pub(super) fn begin_audio_take(
        &mut self,
        id: u32,
        tracks: &[TrackId],
        punch: Option<(f64, f64)>,
    ) -> Result<(), SessionError> {
        let refuse = |sentence: String| Err(SessionError::Recording(sentence));
        let recorded: Vec<(TrackId, String, TrackInput)> = tracks
            .iter()
            .filter_map(|id| self.project.track(*id).ok())
            .filter_map(|track| Some((track.id, track.name.clone(), track.input?)))
            .collect();
        if recorded.is_empty() {
            return Ok(());
        }
        if self.recording.measuring.is_some() {
            return refuse("the round trip is being measured; record when that is done, in a moment".to_string());
        }
        if self.engine.input().is_none() {
            if let Err(why) = self.engine.open_input() {
                let sentence = self.cannot_open(&why);
                self.recording.problem = Some(sentence.clone());
                return refuse(sentence);
            }
            (self.recording.problem, self.recording.tried) = (None, None);
            self.playing_audio_changed();
        }
        if let Some(sentence) = self.unreachable_input() {
            return refuse(sentence);
        }
        let rate = self.engine.sample_rate();
        let folder = self.recordings_folder();
        let mut files = Vec::with_capacity(recorded.len());
        let mut names = BTreeSet::new();
        for (track, name, input) in &recorded {
            let (left, right) = input.channels();
            let path = free_name(&folder, name, &mut names);
            files.push(FileOf { track: *track, path, left, right });
        }
        let frames = |ms: f64| (ms * rate / 1000.0).round() as i64;
        let (measured, adjust) = (self.recording.measured_ms.map(frames), frames(self.recording.adjust_ms));
        // What is handed over after the song has stopped beyond what the
        // devices account for: what the measurement found more than they
        // say, what was added by hand, and something to fade with.
        let said = self.engine.steady_output_latency().round() as i64;
        let more = measured.map_or(0, |measured| measured - said).max(0) + adjust.max(0);
        let tail = more as u32 + (HANDLE_SECONDS * rate) as u32;
        let (writer, feeds) = match TakeWriter::start(files, rate, tail, self.recording.limits) {
            Ok(started) => started,
            Err(why) => return refuse(format!("the take cannot be written: {why}")),
        };
        self.engine.send(Message::Feeds(feeds));
        self.recording.take = Some(AudioTake {
            id,
            begun: false,
            writer: Some(writer),
            tracks: recorded.iter().map(|(track, ..)| *track).collect(),
            rolled: None,
            stretches: Vec::new(),
            stopped: None,
            ended: false,
            punch,
            measured,
            adjust,
            health: self.engine.input_health().unwrap_or_default(),
            ended_early: None,
        });
        self.recording.last = None;
        Ok(())
    }

    /// Takes in what the audio thread said of the take numbered `take`
    /// having begun.
    pub(super) fn audio_take_began(&mut self, take: u32) {
        if let Some(audio) = &mut self.recording.take {
            audio.begun = take == audio.id;
        }
    }

    /// Takes in what the audio thread said of where the take's audio
    /// stands.
    pub(super) fn audio_mark(&mut self, at: u64, mark: AudioMark) {
        let Some(audio) = self.recording.take.as_mut().filter(|audio| audio.begun && !audio.ended) else { return };
        let rate = self.engine.sample_rate();
        // What the song did after the take was stopped is not the take's.
        if audio.stopped.is_some() && !matches!(mark, AudioMark::Ended) {
            return;
        }
        match mark {
            AudioMark::Rolling { beat, latency, delay } => {
                audio.rolled = Some(Rolled { beat, latency, delay });
                // A take that is punched in begins where the song comes
                // into its stretch, which is said when it does.
                if audio.punch.is_none() {
                    audio.begin(at, beat, None);
                }
            }
            AudioMark::Jumped { from, to, round } => {
                // What was being sung to ends where the song was; what is
                // sung from here on is sung to where it is now.
                if audio.end(from) || audio.punch.is_none() {
                    audio.begin(at, to, round.then_some((from, to)));
                }
            }
            AudioMark::PunchedIn { beat } => {
                if let Some((from, _)) = audio.punch {
                    // A song that plays into the stretch reaches it on the
                    // first frame at or past its edge: that is the edge.
                    let map = self.project.tempo_map();
                    let on_the_edge = beat >= from && map.frames_between(from, beat, rate) < 2.0;
                    audio.end(beat);
                    audio.begin(at, if on_the_edge { from } else { beat }, None);
                }
            }
            AudioMark::PunchedOut { beat } => {
                if let Some((_, to)) = audio.punch {
                    audio.end(beat.min(to));
                }
            }
            AudioMark::Stopped { beat } => {
                audio.end(beat);
                audio.stopped = Some((at, beat));
            }
            AudioMark::Ended => audio.ended = true,
        }
    }

    /// The takes that are being recorded as audio, for drawing them as
    /// they grow.
    pub fn growing_takes(&self) -> Vec<GrowingTake> {
        let Some(audio) = &self.recording.take else { return Vec::new() };
        let Some(writer) = &audio.writer else { return Vec::new() };
        // The stretch that is being sung to now, if one is: a take that
        // is punched in has none before the song comes into its stretch.
        let pass = audio.rolled.map(|rolled| {
            let shift = shift_of(audio, rolled);
            match audio.stretches.last().filter(|stretch| stretch.end.is_none()) {
                Some(stretch) => (Some(stretch.start), stretch.at + shift),
                None => (None, shift),
            }
        });
        let files = writer.files().iter().map(|file| GrowingTake {
            track: file.track(),
            file: file.clone(),
            start_beat: pass.and_then(|pass| pass.0),
            first_frame: pass.map_or(0, |pass| pass.1),
        });
        files.collect()
    }

    /// What the last recording came to, once it has ended: its takes, how
    /// loud they are, and what was silent or cut short. `None` while one
    /// is recorded and before any was.
    pub fn last_recording(&self) -> Option<&Recorded> {
        self.recording.last.as_ref()
    }

    /// What ended a take before it was stopped, or closed the input, said
    /// once: a front end shows it.
    pub fn take_recording_notice(&mut self) -> Option<String> {
        self.recording.notice.take()
    }

    /// Ends the take that is being recorded and lets the song play on:
    /// what was recorded up to here is kept whole, and `why` is said.
    pub(super) fn end_take_early(&mut self, why: &str) {
        if !self.is_recording() {
            return;
        }
        if let Some(audio) = &mut self.recording.take {
            audio.ended_early = Some(why.to_string());
        }
        self.recording.notice = Some(why.to_string());
        log::warn!("{why}");
        self.engine.send(Message::EndTake);
    }

    /// Ends a take whose file can take no more: the disk is full, or the
    /// file is as long as one can be.
    fn tend_audio_take(&mut self) {
        let Some(audio) = &self.recording.take else { return };
        if audio.stopped.is_some() || audio.ended_early.is_some() {
            return;
        }
        let failed = audio.writer.as_ref().and_then(|writer| writer.files().iter().find_map(|file| file.failed()));
        if let Some(why) = failed {
            self.end_take_early(&format!("{why}: the take ends there, whole up to that moment"));
        }
    }

    /// Ends the take's audio: what the audio thread still had is written,
    /// the files are ended, and the takes are put beside their tracks,
    /// each where it was sung. Inside the take's undo group.
    pub(super) fn land_audio_take(&mut self) {
        let Some(mut audio) = self.recording.take.take() else { return };
        let files = audio.writer.take().map(TakeWriter::finish).unwrap_or_default();
        let rate = self.engine.sample_rate();
        let health = self.engine.input_health().unwrap_or_default();
        let silent = (health.lost + health.late).saturating_sub(audio.health.lost + audio.health.late)
            + files.iter().map(|file| file.dropped()).sum::<u64>();
        let ended_early = audio.ended_early.take().or_else(|| files.iter().find_map(|file| file.failed()));
        let db = |level: f32| if level > 0.0 { 20.0 * f64::from(level).log10() } else { -120.0 };
        let mut recorded = Recorded {
            rolled: audio.rolled.is_some(),
            takes: Vec::new(),
            peaks_db: files.iter().map(|file| (file.path().to_path_buf(), db(file.peak()))).collect(),
            silent_frames: silent,
            ended_early,
            taken_off_ms: 0.0,
            correction: correction_of(audio.measured.is_some(), audio.adjust != 0),
        };
        let Some(rolled) = audio.rolled else {
            // The song never rolled: there is nothing of it to put
            // anywhere. The files are there, with what the count-in heard.
            self.recording.last = Some(recorded);
            return;
        };
        let shift = shift_of(&audio, rolled);
        recorded.taken_off_ms = shift as f64 * 1000.0 / rate;
        let stopped = audio.stopped.unwrap_or_else(|| {
            // The audio thread never said: where the song is, and what was
            // written.
            (files.iter().map(|file| file.frames()).min().unwrap_or(0), self.engine.position())
        });
        let mut passes = passes(&audio, stopped);
        if files.iter().any(|file| file.failed().is_some()) {
            // A file that took no more holds less than was sung: the take
            // is what the file holds, whole up to there, and no longer.
            let held = files.iter().map(|file| file.frames()).min().unwrap_or(0);
            let map = self.project.tempo_map();
            for pass in &mut passes {
                let most = held.saturating_sub(pass.at + shift);
                pass.end = pass.end.min(map.beat_after(pass.start, most as f64, rate));
            }
            passes.retain(|pass| pass.end - pass.start >= SHORTEST_TAKE);
        }
        let tracks = audio.tracks.clone();
        let paths: Vec<PathBuf> = files.iter().map(|file| file.path().to_path_buf()).collect();
        match self.edit(|project| put_takes(project, &tracks, &paths, &passes, shift, rate)) {
            Ok(landed) => recorded.takes = landed,
            Err(error) => {
                log::error!("could not put what was recorded beside its track: {error}");
                recorded.ended_early.get_or_insert(format!("the take could not be put beside its track: {error}"));
            }
        }
        // The files are whole now, and may have been looked at while they
        // grew.
        for file in &files {
            self.audio.media().forget(file.path());
        }
        self.recording.last = Some(recorded);
    }

    /// The song with the take that is being recorded in it as far as it
    /// has been written: what a copy kept for safety holds, so that a
    /// program that dies in the middle of a take leaves a song that plays
    /// what was sung. `None` while no audio is being recorded, or before
    /// the song has begun to roll.
    pub(super) fn with_take_so_far(&self) -> Option<Project> {
        let audio = self.recording.take.as_ref()?;
        let (rolled, writer) = (audio.rolled?, audio.writer.as_ref()?);
        let rate = self.engine.sample_rate();
        let shift = shift_of(audio, rolled);
        let paths: Vec<PathBuf> = writer.files().iter().map(|file| file.path().to_path_buf()).collect();
        // How far the song had got when what is written was sung: by the
        // frames of the pass that is under way, from where it began.
        let written = writer.files().iter().map(|file| file.frames()).min()?;
        let at = written.checked_sub(shift)?;
        // How far the song had got in the stretch that is being sung to,
        // by the frames of it that are written.
        let reached = match audio.stretches.last().filter(|stretch| stretch.end.is_none()) {
            Some(open) => self.project.tempo_map().beat_after(open.start, at.checked_sub(open.at)? as f64, rate),
            None => audio.stretches.last().and_then(|last| last.end).unwrap_or(rolled.beat),
        };
        let stopped = audio.stopped.filter(|(stopped, _)| *stopped <= at).unwrap_or((at, reached));
        let mut project = self.project.clone();
        put_takes(&mut project, &audio.tracks, &paths, &passes(audio, stopped), shift, rate).ok()?;
        Some(project)
    }

    /// Whether audio is being recorded right now: a take is not saved
    /// work until it has ended, and whoever keeps copies for safety
    /// should look again.
    pub(super) fn audio_is_rolling(&self) -> bool {
        self.recording.take.as_ref().is_some_and(|audio| audio.rolled.is_some() && !audio.ended)
    }

    /// Drops a take's audio without putting it anywhere: the project it
    /// belonged to is being replaced. Its files are written all the same.
    pub(super) fn abandon_audio_take(&mut self) {
        self.recording.take = None;
        self.recording.heard = None;
        self.recording.songs += 1;
    }

    // ---- hearing a take in place ----------------------------------------

    /// Has a take heard in place of what its track plays there, without
    /// the song being changed: for choosing between takes by ear. `None`
    /// goes back to the song. It is the session's to remember, like which
    /// tracks are armed: it is not saved and is no undo step.
    pub fn hear_take(&mut self, take: Option<ClipId>) -> Result<(), SessionError> {
        if let Some(take) = take {
            self.project.take(take)?;
        }
        if self.recording.heard != take {
            self.recording.heard = take;
            self.compile_and_swap();
            self.revision += 1;
        }
        Ok(())
    }

    /// The take that is heard in place, if one is.
    pub fn heard_take(&self) -> Option<ClipId> {
        self.recording.heard.filter(|take| self.project.take(*take).is_ok())
    }

    // ---- how long sound takes to go round --------------------------------

    /// How long sound takes from the microphone to the speaker as the
    /// devices are open, and where each figure is from. `None` while the
    /// input is closed: it is not known until it is open.
    pub fn recording_latency(&self) -> Option<RecordingLatency> {
        let input = self.engine.input()?;
        let rate = self.engine.sample_rate();
        let ms = |frames: f64| frames * 1000.0 / rate;
        let output = self.engine.steady_output_latency();
        let said = f64::from(input.latency_frames) * rate / f64::from(input.sample_rate);
        let delay = f64::from(input.delay_frames);
        Some(RecordingLatency {
            output_ms: ms(output),
            input_ms: ms(said),
            handover_ms: ms(delay - said),
            measured_ms: self.recording.measured_ms,
            adjust_ms: self.recording.adjust_ms,
            correction: correction_of(self.recording.measured_ms.is_some(), self.recording.adjust_ms != 0.0),
            round_trip_ms: (ms(delay) + self.recording.measured_ms.unwrap_or(ms(output)) + self.recording.adjust_ms)
                .max(0.0),
            drift_ppm: self.engine.input_drift_ppm().unwrap_or(0.0),
        })
    }

    /// What stands in place of what the devices say, where the round trip
    /// was measured, and what was added by hand: both in milliseconds.
    pub fn latency_correction(&self) -> (Option<f64>, f64) {
        (self.recording.measured_ms, self.recording.adjust_ms)
    }

    /// Takes what a measurement found ([`Measured::beyond_ms`]) in place
    /// of what the devices say of how long sound takes to go round, or
    /// with `None` goes back to what they say. It counts from the next
    /// take on, and for these devices as they are open: whoever keeps
    /// settings keeps it for them.
    pub fn set_measured_latency(&mut self, milliseconds: Option<f64>) {
        self.recording.measured_ms = milliseconds.filter(|ms| ms.is_finite()).map(|ms| ms.clamp(0.0, 2000.0));
        self.playing_audio_changed();
    }

    /// Adds `milliseconds` by hand to how long sound is taken to go round,
    /// measured or said: more puts a take earlier. For putting right by
    /// ear what a measurement cannot be made of.
    pub fn set_latency_adjustment(&mut self, milliseconds: f64) {
        self.recording.adjust_ms = if milliseconds.is_finite() { milliseconds.clamp(-500.0, 500.0) } else { 0.0 };
        self.playing_audio_changed();
    }

    /// Measures how long sound takes to go round: plays a second of noise,
    /// quietly, and listens for it on `channel` of the input (counted from
    /// 1). The output has to reach that input: through a cable, or a
    /// microphone held to the speaker. [`Session::measurement`] has the
    /// result a few seconds later; it changes nothing by itself.
    pub fn measure_latency(&mut self, channel: u16) -> Result<(), SessionError> {
        let refuse = |sentence: &str| Err(SessionError::Recording(sentence.to_string()));
        if !crate::hand::is_by_hand() {
            return refuse(
                "the measurement opens the microphone and plays through the output: the user runs it themselves, in \
                 the audio settings (Input and round trip, Measure)",
            );
        }
        if self.is_recording() {
            return refuse("a take is being recorded; measure when it has ended");
        }
        if self.recording.measuring.is_some() {
            return Ok(());
        }
        if self.engine.input().is_none() {
            self.engine.open_input().map_err(|why| SessionError::Recording(self.cannot_open(&why)))?;
            (self.recording.problem, self.recording.tried) = (None, None);
            self.playing_audio_changed();
        }
        let input = self.engine.input().expect("it is open").clone();
        if channel == 0 || channel > input.channels {
            return Err(SessionError::Recording(format!(
                "'{}' has channels 1 to {}; there is no channel {channel} to listen on",
                input.device, input.channels
            )));
        }
        let rate = self.engine.sample_rate();
        let signal = measuring_signal(rate);
        let heard = vec![0.0f32; signal.len() + rate as usize].into_boxed_slice();
        let measure = Measure { signal: signal.clone(), gain: 1.0, channel: usize::from(channel) - 1, heard, at: 0 };
        self.recording.measured = None;
        self.recording.measuring = Some(Measuring {
            signal,
            waiting: Some(Box::new(measure)),
            began: Instant::now(),
            latency: self.engine.steady_output_latency(),
            delay: input.delay_frames,
            channel,
        });
        self.tend_measurement();
        Ok(())
    }

    /// The measurement as it stands. A result is there until the next
    /// measurement is asked for.
    pub fn measurement(&self) -> Measurement {
        match (&self.recording.measuring, &self.recording.measured) {
            (Some(_), _) => Measurement::Running,
            (None, Some(Ok(measured))) => Measurement::Done(*measured),
            (None, Some(Err(why))) => Measurement::Failed(why.clone()),
            (None, None) => Measurement::Idle,
        }
    }

    /// Takes in a measurement that has ended.
    fn tend_measurement(&mut self) {
        let open_for = self.engine.input_open_for();
        let Some(measuring) = &mut self.recording.measuring else { return };
        if measuring.waiting.is_some() {
            // The signal is played once the clocks have been told apart.
            match open_for {
                Some(seconds) if seconds >= SETTLE_SECONDS => {
                    measuring.latency = self.engine.steady_output_latency();
                    let measure = measuring.waiting.take().expect("it is waiting");
                    self.engine.send(Message::Measure(measure));
                }
                Some(_) => {}
                None => {
                    self.recording.measuring = None;
                    self.recording.measured = Some(Err("the input was closed before the measurement began".into()));
                }
            }
            return;
        }
        let Some(measure) = self.engine.take_measured() else {
            if measuring.began.elapsed() > Duration::from_secs(20) {
                self.recording.measuring = None;
                self.recording.measured =
                    Some(Err("the measurement did not end: the audio device is not running".into()));
            }
            return;
        };
        let measuring = self.recording.measuring.take().expect("it was there");
        let rate = self.engine.sample_rate();
        let (lag, likeness, level) = came_back(&measuring.signal, &measure.heard);
        let db = if level > 0.0 { 20.0 * f64::from(level).log10() } else { -120.0 };
        self.recording.measured = Some(if likeness < 0.25 {
            Err(format!(
                "the signal did not come back on input {} (what was heard there was at {db:.0} dB, and not like it): \
                 connect the output to that input with a cable, or hold the microphone to the speaker, and measure again",
                measuring.channel
            ))
        } else {
            let said = f64::from(measuring.delay) + measuring.latency.round();
            Ok(Measured {
                round_trip_ms: lag * 1000.0 / rate,
                beyond_ms: (lag - f64::from(measuring.delay)) * 1000.0 / rate,
                more_than_said_ms: (lag - said) * 1000.0 / rate,
                likeness,
                heard_db: db,
            })
        });
        self.playing_audio_changed();
    }

    // ---- where recordings are kept ---------------------------------------

    /// Says where the takes of a song that has not been saved are kept: a
    /// folder of the program's own, which this song alone writes to. They
    /// come beside the song when it is first saved.
    pub fn keep_unsaved_recordings_in(&mut self, folder: PathBuf) {
        self.recording.unsaved = Some(folder);
    }

    /// Where a take that is recorded now is written: beside a saved song,
    /// in `<song> files/takes`; for one that is not saved, where
    /// [`Session::keep_unsaved_recordings_in`] says.
    pub fn recordings_folder(&self) -> PathBuf {
        match &self.path {
            Some(song) => beside(song),
            None => self.unsaved_folder(),
        }
    }

    fn unsaved_folder(&self) -> PathBuf {
        let unsaved = self.recording.unsaved.clone();
        let base = unsaved
            .unwrap_or_else(|| std::env::temp_dir().join("trill recordings").join(std::process::id().to_string()));
        // Every song that has had no place yet has a folder of its own:
        // what another left there is not this one's to list or delete.
        base.join(format!("song {}", self.recording.songs + 1))
    }

    /// The recordings of an unsaved song that `project` or a state undo
    /// can reach names: what comes beside the song when it is saved.
    fn unsaved_recordings(&self) -> BTreeSet<String> {
        let folder = self.unsaved_folder();
        let mut found = BTreeSet::new();
        self.each_state(|project| {
            for path in recordings_of(project) {
                if Path::new(&path).starts_with(&folder) {
                    found.insert(path);
                }
            }
        });
        found
    }

    /// Calls `look` with the song as it is and as every state undo and
    /// redo can reach has it.
    fn each_state(&self, mut look: impl FnMut(&Project)) {
        look(&self.project);
        for step in self.undo.iter().chain(&self.redo) {
            look(&step.project);
        }
    }

    /// Brings the recordings of a song that had no place beside the place
    /// it is about to be saved at, `song`: each is copied there, and the
    /// song and every state undo can reach name the copy from here on.
    /// Returns where they were, to be taken away once the song's file is
    /// written: until then both are there, and whichever file of the song
    /// is read finds its recordings.
    pub(super) fn bring_recordings_beside(&mut self, song: &Path) -> Result<Vec<PathBuf>, SessionError> {
        let unsaved = self.unsaved_recordings();
        if unsaved.is_empty() {
            return Ok(Vec::new());
        }
        let folder = beside(song);
        std::fs::create_dir_all(&folder).map_err(|error| super::io_error(&folder, error))?;
        let mut moved: Vec<(String, String)> = Vec::new();
        let mut names = BTreeSet::new();
        for old in &unsaved {
            let from = Path::new(old);
            if !from.is_file() {
                // Gone already: the song goes on naming where it was.
                continue;
            }
            let stem = from.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let to = free_name(&folder, &stem, &mut names);
            let copied = std::fs::copy(from, &to).map_err(|error| super::io_error(&to, error))?;
            let whole = std::fs::metadata(from).map(|file| file.len()).unwrap_or(0);
            if copied != whole {
                return Err(SessionError::Io(format!(
                    "'{}' was not copied whole to '{}': the song is not saved, and its recordings are where they were",
                    from.display(),
                    to.display()
                )));
            }
            moved.push((old.clone(), to.to_string_lossy().into_owned()));
        }
        let rename = |project: &mut Project| {
            for track in &mut project.tracks {
                let clips = track.clips.iter_mut().chain(track.takes.iter_mut().map(|take| &mut take.clip));
                for clip in clips {
                    if let trill_core::project::ClipContent::Audio(audio) = &mut clip.content
                        && let Some((_, new)) = moved.iter().find(|(old, _)| *old == audio.path)
                    {
                        audio.path.clone_from(new);
                    }
                }
            }
        };
        rename(&mut self.project);
        for step in self.undo.iter_mut().chain(&mut self.redo) {
            rename(&mut step.project);
        }
        for (old, new) in &moved {
            self.audio.media().forget(Path::new(old));
            self.audio.media().forget(Path::new(new));
        }
        if !moved.is_empty() {
            self.compile_and_swap();
            self.revision += 1;
        }
        Ok(moved.into_iter().map(|(old, _)| PathBuf::from(old)).collect())
    }

    /// The recordings in the song's own folder that neither the song nor
    /// any state undo can reach names any more, each with its size in
    /// bytes: what could be deleted. A take that was removed, or undone,
    /// leaves its file here.
    pub fn unused_recordings(&self) -> Vec<(PathBuf, u64)> {
        let folder = self.recordings_folder();
        let mut named = HashSet::new();
        self.each_state(|project| named.extend(recordings_of(project).into_iter().map(PathBuf::from)));
        // What is being written is nobody's to delete either.
        if let Some(writer) = self.recording.take.as_ref().and_then(|take| take.writer.as_ref()) {
            named.extend(writer.files().iter().map(|file| file.path().to_path_buf()));
        }
        let Ok(entries) = std::fs::read_dir(&folder) else { return Vec::new() };
        let mut unused: Vec<(PathBuf, u64)> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension.eq_ignore_ascii_case("wav")))
            .filter(|entry| !named.contains(&entry.path()))
            .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.len())))
            .collect();
        unused.sort();
        unused
    }

    /// Holds the files of the takes recorded from now on to `limits`: for
    /// a test, which reaches a full disk or the length a file can have in
    /// a second and not in hours.
    #[doc(hidden)]
    pub fn hold_recordings_to(&mut self, limits: trill_media::GrowingLimits) {
        self.recording.limits = limits;
    }

    /// Says where recordings that are removed go: the system's Recycle
    /// Bin, or a folder beside the takes. The folder unless a program says
    /// otherwise, so that nothing of a test's goes into anybody's bin.
    pub fn remove_recordings_into(&mut self, bin: Bin) {
        self.recording.bin = bin;
    }

    /// Where a recording that is removed now would go, for the question
    /// that is asked first: "to the Recycle Bin", "to C:\…\removed".
    pub fn removed_recordings_go(&self) -> String {
        self.recording.bin.goes(&self.recordings_folder())
    }

    /// Removes the recordings among `paths` that are unused
    /// ([`Session::unused_recordings`]) and says how much room they took
    /// and where they went. None is unlinked: each goes to the system's
    /// Recycle Bin or into a folder beside the takes (see
    /// [`Session::remove_recordings_into`]), from where it can be had back.
    ///
    /// One that the open song names, or that is not in its own folder, is
    /// refused, and so is one that `elsewhere` says another song plays (by
    /// that song's name): then none is removed. The session knows the song
    /// that is open; which others there are is its keeper's to say, so a
    /// front end asks [`Runtime`](../../trill_runtime/struct.Runtime.html)
    /// and not this.
    pub fn delete_unused_recordings(
        &mut self,
        paths: &[PathBuf],
        elsewhere: &dyn Fn(&Path) -> Option<String>,
    ) -> Result<Removed, SessionError> {
        let unused = self.unused_recordings();
        for path in paths {
            if let Some(song) = elsewhere(path) {
                return Err(SessionError::Recording(format!(
                    "'{}' is played by {song}: nothing was removed. A recording goes only when no song Trill knows \
                     of plays it",
                    path.display()
                )));
            }
            if !unused.iter().any(|(known, _)| known == path) {
                return Err(SessionError::Recording(format!(
                    "'{}' is not a recording the song has stopped using: nothing was deleted. The ones that can be are: {}",
                    path.display(),
                    match unused.as_slice() {
                        [] => "none".to_string(),
                        unused =>
                            unused.iter().map(|(path, _)| path.display().to_string()).collect::<Vec<_>>().join(", "),
                    }
                )));
            }
        }
        let going: Vec<(PathBuf, u64)> = unused.into_iter().filter(|(path, _)| paths.contains(path)).collect();
        for (path, _) in &going {
            // Nothing reads it any more, so that it can go.
            self.audio.media().forget(path);
        }
        self.recording.bin.remove(&going).map_err(|(_, path, error)| super::io_error(&path, error))
    }
}

/// Puts the takes a recording came to beside their tracks in `project`:
/// each pass as a take, in a lane, where it was sung (`shift` frames after
/// the frame that had been handed over as it began), and on the track the
/// first pass, which may begin before the loop does, with the last pass
/// that ran the whole loop over it. A track that is gone gets none.
fn put_takes(
    project: &mut Project,
    tracks: &[TrackId],
    files: &[PathBuf],
    passes: &[Pass],
    shift: u64,
    rate: f64,
) -> Result<Vec<RecordedTake>, trill_core::EditError> {
    let map = project.tempo_map();
    let mut landed = Vec::new();
    for (track, file) in tracks.iter().zip(files) {
        if project.track(*track).is_err() {
            continue;
        }
        let mut made: Vec<(ClipId, &Pass)> = Vec::new();
        for pass in passes {
            let number = project.track(*track)?.takes.len() + 1;
            let content = AudioContent {
                offset_seconds: (pass.at + shift) as f64 / rate,
                ..AudioContent::new(file.to_string_lossy())
            };
            let take =
                project.add_take(*track, format!("Take {number}"), pass.start, pass.end - pass.start, content)?;
            made.push((take, pass));
        }
        // On the track, in the order they were sung: a later one takes
        // the place of an earlier one where the two lie over one another,
        // which keeps the earlier one as the take it already is.
        for (take, _) in made.iter().filter(|(_, pass)| pass.whole) {
            project.choose_take(*take)?;
        }
        // What the track plays of them now: the ones nothing sung later
        // lies over the whole of.
        let covered = |index: usize, pass: &Pass| {
            let later = made[index + 1..].iter().filter(|(_, later)| later.whole);
            later.into_iter().any(|(_, later)| later.start <= pass.start + 1e-9 && later.end >= pass.end - 1e-9)
        };
        let chosen: Vec<ClipId> = made
            .iter()
            .enumerate()
            .filter(|(index, (_, pass))| pass.whole && !covered(*index, pass))
            .map(|(_, (take, _))| *take)
            .collect();
        for (take, pass) in made {
            landed.push(RecordedTake {
                track: *track,
                take,
                file: file.clone(),
                start_beat: pass.start,
                length_beats: pass.end - pass.start,
                seconds: map.seconds_between(pass.start, pass.end),
                chosen: chosen.contains(&take),
            });
        }
    }
    Ok(landed)
}

/// Where a round trip is from, in a word.
fn correction_of(measured: bool, adjusted: bool) -> Correction {
    match (measured, adjusted) {
        (true, _) => Correction::Measured,
        (false, true) => Correction::ByHand,
        (false, false) => Correction::None,
    }
}

/// How many frames after the frame that had been handed over as the song
/// began to roll what was sung to the song's first frame lies: the
/// hand-over, then what lies between the engine and the microphone on the
/// way round (as it was measured, or as the output said), then what was
/// added by hand.
fn shift_of(audio: &AudioTake, rolled: Rolled) -> u64 {
    let beyond = audio.measured.unwrap_or(i64::from(rolled.latency));
    (i64::from(rolled.delay) + beyond + audio.adjust).max(0) as u64
}

/// The folder beside `song` that holds what was recorded for it.
fn beside(song: &Path) -> PathBuf {
    let stem = song.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
    song.with_file_name(format!("{stem} files")).join(TAKES_FOLDER)
}

/// The paths of every recording `project` names: its audio clips and its
/// takes.
fn recordings_of(project: &Project) -> Vec<String> {
    let clips =
        project.tracks.iter().flat_map(|track| track.clips.iter().chain(track.takes.iter().map(|take| &take.clip)));
    clips.filter_map(|clip| Some(clip.audio()?.path.clone())).collect()
}

/// A name in `folder` for a recording of `what` that no file has and that
/// is not in `taken`, which it then is: `Voice.wav`, `Voice 2.wav`.
fn free_name(folder: &Path, what: &str, taken: &mut BTreeSet<PathBuf>) -> PathBuf {
    let plain: String =
        what.chars()
            .map(|c| {
                if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
    let plain = plain.trim().trim_end_matches('.');
    let plain = if plain.is_empty() { "Take" } else { plain };
    for number in 1.. {
        let name = if number == 1 { format!("{plain}.wav") } else { format!("{plain} {number}.wav") };
        let path = folder.join(name);
        if !path.exists() && taken.insert(path.clone()) {
            return path;
        }
    }
    unreachable!("there is always another number")
}

/// A stretch of a take's audio that is a take of its own: the frame handed
/// over as it began, and where in the song it lies.
struct Pass {
    at: u64,
    start: f64,
    end: f64,
    /// Whether it is put on the track: every stretch that was sung is,
    /// but for a pass of the loop that did not run the whole of it, which
    /// would cover the whole pass before it with less.
    whole: bool,
}

impl AudioTake {
    /// Ends the stretch that is being sung to, where the song was; says
    /// whether there was one.
    fn end(&mut self, beat: f64) -> bool {
        match self.stretches.last_mut().filter(|stretch| stretch.end.is_none()) {
            Some(open) => {
                open.end = Some(beat);
                true
            }
            None => false,
        }
    }

    /// Begins a stretch at `start`, with the frame handed over next.
    fn begin(&mut self, at: u64, start: f64, round: Option<(f64, f64)>) {
        self.stretches.push(Stretch { at, start, end: None, round });
    }
}

/// The takes a take's audio comes to: one for each stretch of the song it
/// was sung to, as the audio thread said them. `stopped` is where the
/// song stopped, or was last known to be, for a stretch nobody ended.
///
/// Every stretch is a take, and is put on the track where it was sung;
/// but a pass of the loop that did not run the whole of it is a take and
/// is not put there, since the whole pass before it is the better thing
/// to hear. And the beginning of one more pass, under a beat of it, when
/// the song was stopped just after it came round, is no take at all: it is
/// in the file, after the pass before it.
fn passes(audio: &AudioTake, stopped: (u64, f64)) -> Vec<Pass> {
    let mut passes: Vec<(Pass, bool)> = audio
        .stretches
        .iter()
        .map(|stretch| {
            let end = stretch.end.unwrap_or(stopped.1);
            let whole = stretch.round.is_none_or(|(from, to)| end - stretch.start >= from - to - 1e-9);
            (Pass { at: stretch.at, start: stretch.start, end, whole }, stretch.round.is_some())
        })
        .collect();
    passes.retain(|(pass, _)| pass.end - pass.start >= SHORTEST_TAKE);
    let sliver =
        |(last, came_round): &(Pass, bool)| *came_round && !last.whole && last.end - last.start < SHORTEST_PASS;
    if passes.len() > 1 && passes.last().is_some_and(sliver) {
        passes.pop();
    }
    passes.into_iter().map(|(pass, _)| pass).collect()
}

/// The signal a measurement plays: a quarter of a second of nothing, a
/// second of noise that is the same every time, between 300 Hz and 6 kHz
/// where speakers and microphones are at home, at a tenth of full scale,
/// and nothing again. Noise, because it is like itself at one moment only:
/// where it lies in what comes back is found to a fraction of a frame.
fn measuring_signal(rate: f64) -> Arc<[f32]> {
    let (lead, noise) = ((rate * 0.25) as usize, rate as usize);
    let mut signal = vec![0.0f32; lead + noise + lead];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let (mut low, mut high) = (0.0f64, 0.0f64);
    let (low_by, high_by) = (
        1.0 - (-2.0 * std::f64::consts::PI * 6000.0 / rate).exp(),
        1.0 - (-2.0 * std::f64::consts::PI * 300.0 / rate).exp(),
    );
    let fade = (rate * 0.02) as usize;
    for (index, sample) in signal[lead..lead + noise].iter_mut().enumerate() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let white = (seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
        low += (white - low) * low_by;
        high += (low - high) * high_by;
        let edge = (index.min(noise - 1 - index) as f64 / fade as f64).min(1.0);
        *sample = ((low - high) * 0.5 * (std::f64::consts::PI * edge / 2.0).sin().powi(2)) as f32;
    }
    let peak = signal.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    for sample in &mut signal {
        *sample *= 0.1 / peak.max(1e-9);
    }
    signal.into()
}

/// Where `signal` lies in `heard`, which began as it was played: how many
/// frames late (between frames too), how alike the two are there (0 to 1),
/// and the highest level heard.
fn came_back(signal: &[f32], heard: &[f32]) -> (f64, f64, f32) {
    let size = (heard.len() + signal.len()).next_power_of_two().max(8);
    let mut transform = trill_dsp::fft::RealFft::new(size);
    let bins = transform.bins();
    let spectrum = |transform: &mut trill_dsp::fft::RealFft, samples: &[f32]| {
        let mut padded = vec![0.0f32; size];
        padded[..samples.len()].copy_from_slice(samples);
        let (mut re, mut im) = (vec![0.0f32; bins], vec![0.0f32; bins]);
        transform.forward(&padded, &mut re, &mut im);
        (re, im)
    };
    let (heard_re, heard_im) = spectrum(&mut transform, heard);
    let (signal_re, signal_im) = spectrum(&mut transform, signal);
    // What comes back times what was played, turned round: how alike the
    // two are at every lag.
    let re: Vec<f32> = (0..bins).map(|bin| heard_re[bin] * signal_re[bin] + heard_im[bin] * signal_im[bin]).collect();
    let im: Vec<f32> = (0..bins).map(|bin| heard_im[bin] * signal_re[bin] - heard_re[bin] * signal_im[bin]).collect();
    let mut alike = vec![0.0f32; size];
    transform.inverse(&re, &im, &mut alike);
    let lags = heard.len().saturating_sub(signal.len()).max(1);
    let (best, top) = alike[..lags]
        .iter()
        .enumerate()
        .fold((0, f32::MIN), |best, (lag, value)| if *value > best.1 { (lag, *value) } else { best });
    let between =
        match (best.checked_sub(1).map(|before| alike[before]), alike.get(best + 1).filter(|_| best + 1 < lags)) {
            (Some(before), Some(after)) => {
                let (a, b, c) = (f64::from(before), f64::from(top), f64::from(*after));
                let bend = a - 2.0 * b + c;
                if bend < 0.0 { 0.5 * (a - c) / bend } else { 0.0 }
            }
            _ => 0.0,
        };
    let energy = |samples: &[f32]| samples.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>();
    let there = &heard[best.min(heard.len())..(best + signal.len()).min(heard.len())];
    let likeness = f64::from(top) / (energy(signal) * energy(there)).sqrt().max(1e-30);
    let level = heard.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    (best as f64 + between, likeness.clamp(0.0, 1.0), level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_is_found_where_it_lies_in_what_came_back_between_frames_too() {
        let signal = measuring_signal(48_000.0);
        assert_eq!(signal.len(), 72_000);
        let peak = signal.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!((peak - 0.1).abs() < 1e-6, "a tenth of full scale: {peak}");
        // Late by 2345 frames, at half the level, with other noise on it.
        let mut heard = vec![0.0f32; signal.len() + 48_000];
        let mut seed = 99u64;
        for (index, sample) in heard.iter_mut().enumerate() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let other = ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.004;
            let played = index.checked_sub(2345).and_then(|at| signal.get(at)).copied().unwrap_or(0.0);
            *sample = played * 0.5 + other;
        }
        let (lag, likeness, level) = came_back(&signal, &heard);
        assert!((lag - 2345.0).abs() < 0.05, "{lag}");
        assert!(likeness > 0.95, "{likeness}");
        assert!((level - 0.05).abs() < 0.005);
        // Nothing came back: nothing is like it.
        let (_, likeness, _) = came_back(&signal, &vec![0.0; heard.len()]);
        assert!(likeness < 0.01);
        let noise: Vec<f32> =
            heard.iter().enumerate().map(|(index, _)| ((index * 7919) % 1000) as f32 / 1e4 - 0.05).collect();
        assert!(came_back(&signal, &noise).1 < 0.2);
    }

    #[test]
    fn recordings_are_named_after_their_track_with_names_no_file_has() {
        let folder = tempfile::tempdir().unwrap();
        let mut taken = BTreeSet::new();
        let first = free_name(folder.path(), "Lead: Vox?", &mut taken);
        assert_eq!(first.file_name().unwrap(), "Lead  Vox.wav");
        assert_eq!(free_name(folder.path(), "Lead: Vox?", &mut taken).file_name().unwrap(), "Lead  Vox 2.wav");
        std::fs::write(folder.path().join("Bass.wav"), b"").unwrap();
        assert_eq!(free_name(folder.path(), "Bass", &mut taken).file_name().unwrap(), "Bass 2.wav");
        assert_eq!(free_name(folder.path(), "...", &mut taken).file_name().unwrap(), "Take.wav");
        assert_eq!(beside(Path::new("songs/My Song.trill")), Path::new("songs/My Song files/takes"));
    }
}
