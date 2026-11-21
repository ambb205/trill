//! The audio-thread core: turns a [`Graph`] into samples.
//!
//! Nothing in here allocates, locks or blocks once constructed. The same
//! processor drives realtime playback and offline rendering, so a bounce is
//! exactly what you heard.
//!
//! A block is made a track at a time, in the order the graph has them.
//! Each track is told what there is to tell it before its own notes
//! ([`prelude`]: what the block lets go of, what a hand played, where its
//! controllers are), begins from what was fed to it and plays its notes
//! and audio ([`sound`]) through its devices ([`through`]), is listened
//! to and brought to where its fader has it ([`finish`]), and is added to
//! where it goes ([`mix`]).
//!
//! One thread can do all of that, track after track, and for a small song
//! it does. The tracks that do not feed one another can also be made at
//! once ([`crate::spread`]): by a render on the threads it was given, and
//! by a song that plays on the [`Team`] it was given, once making its
//! tracks takes a share of the block's time that is worth handing across
//! ([`Processor::spread_from`]). Either way they are the same steps on the
//! same numbers, added up in the same order by the one thread that asked,
//! so that the samples are the same. What only the thread that asked may
//! do stays with it: every [`prelude`] (the hand's events and a take's
//! reports go through queues that have one end), every [`mix`], the
//! master and the monitor.

use crate::capture::Reader as InputReader;
use crate::clock::thread_cycles;
use crate::engine::RecordPhase;
use crate::glide::{DECLICK_SECONDS, STEP, Span, gated};
use crate::graph::{
    AS_MADE, AudioData, Bus, Chain, DeviceSlot, Graph, MAX_BLOCK, RegionAudio, Switch, TrackNode, TrackState, UNKNOWN,
};
use crate::live::{AudioMark, Captured, LiveTarget, TrackSet};
use crate::monitor::{Beats, Monitor};
use crate::recorder::Feeds;
use crate::spread::{Spread, Workers};
use crate::team::Team;
use std::sync::Arc;
use std::time::Instant;
use trill_core::Channel;
use trill_core::controllers::{CONTROLLER_KINDS, ControllerKind, ControllerLane};
use trill_core::device::{Event, EventKind, KeyInput, MAX_EVENTS_PER_BLOCK, ProcessInfo, SongTime};
use trill_core::project::MIN_VOLUME_DB;
use trill_core::{DeviceId, MeterMap, TempoMap, TrackId};
use trill_dsp::util::{db_to_gain, pan_gains};

const EVENT_CAPACITY: usize = MAX_EVENTS_PER_BLOCK;
/// Leave room for the note-offs that follow a burst of note-ons.
const NOTE_ON_LIMIT: usize = EVENT_CAPACITY - 512;
/// How often, in frames, a controller that a clip moves is told where it
/// is: fine enough that a swell is a slope and not a staircase (two thirds
/// of a millisecond at 48 kHz). The frames are counted from where the song
/// last started or jumped, not from the start of a buffer, so the same
/// song makes the same events whatever the buffer size.
const CONTROL_STEP: u64 = STEP;
/// How often, in frames, a device's parameter is moved to where its
/// envelope has it. A device takes a parameter for a whole block, so while
/// a song with such an envelope plays, blocks end on this grid, which is
/// counted like [`CONTROL_STEP`]: the parameter moves at the same frames
/// whatever the buffer size, and in a render.
const PARAM_STEP: u64 = 256;
/// The share of the time its buffers last that making a song's tracks
/// must take, all of them together and over a quarter of a second
/// ([`WINDOW_NS`]), before a song that plays is spread over its team:
/// half. Below it one thread has the buffers made with half their time to
/// spare, as it always did, and is as even as one thread is: with every
/// thread more that a buffer waits for, it is more likely that the system
/// takes one of them off its processor in the middle of a track (one or
/// two buffers in fifteen thousand of 64 frames were measured late so, of
/// a song that one thread plays in a seventh of its time).
pub const SPREAD_ABOVE: f64 = 0.5;
/// The part of that share under which a song that is playing goes back to
/// one thread. (One that stands still stays as it is: it is as cheap
/// either way, and when it is started it is on the threads it needed when
/// it last played.)
const SPREAD_BELOW: f64 = 0.5;
/// The share of one buffer's time that making its tracks takes for the
/// song to be spread from the next buffer on, without waiting for the
/// quarter of a second to be over: that buffer was as good as late.
const URGENT: f64 = 0.8;
/// Over how long what the tracks took is added up before it is held
/// against [`SPREAD_ABOVE`], in nanoseconds: one buffer that the system
/// held up says nothing of the song.
const WINDOW_NS: f64 = 0.25e9;
/// How long the most a buffer's tracks took is remembered at half its
/// weight, in nanoseconds. By that the threads are asked for, ahead of the
/// song being spread over them ([`Processor::wants_threads`]).
const WEIGHT_HALVES_NS: f64 = 2e9;
/// The most events a track's controllers add to one block. One that finds
/// no room is told a step later.
const CONTROL_LIMIT: usize = 768;
/// Below this, what a track's devices put out counts as nothing.
const AUDIBLE: f32 = 1e-5;
/// Live events held for one callback. A player with ten fingers, two feet
/// and a pitch wheel does not come near it; a controller that floods can.
/// So the room is not given out evenly: see [`Processor::push_live`].
const LIVE_CAPACITY: usize = 1024;
/// From this many waiting on, the newest value of a wheel or a controller
/// takes the place of the one already waiting instead of joining the line.
const LIVE_THIN_AT: usize = 512;
/// The last of the room is kept for what ends notes: keys coming up, the
/// pedal, "all notes off". New notes and controllers stop short of it.
const LIVE_RESERVE: usize = 128;

/// What became of a live event handed to the processor.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Taken {
    /// It will be played (or, for a controller in a flood, its value will).
    Kept,
    /// It was a new note and there was no room: it is not played, and
    /// nothing hangs for want of it.
    Skipped,
    /// It could not be kept and may have been the end of a note. The
    /// caller must let go of everything.
    Lost,
}

/// Whether two events are the same wheel or the same controller.
fn same_control(a: EventKind, b: EventKind) -> bool {
    match (a, b) {
        (EventKind::PitchBend { .. }, EventKind::PitchBend { .. }) => true,
        (EventKind::ChannelPressure { .. }, EventKind::ChannelPressure { .. }) => true,
        (EventKind::Control { controller: one, .. }, EventKind::Control { controller: other, .. }) => one == other,
        _ => false,
    }
}
/// Fade applied at the edges of audio clips to avoid clicks.
pub(crate) const CLIP_FADE_SECONDS: f64 = 0.003;
/// How long two clips that are faded across one another both sound: the
/// seam is in the middle of it.
pub(crate) const CROSS_FADE_SECONDS: f64 = 0.012;
/// Length of the cross-fade from a stand-in to the audio that replaces it.
const HAND_OVER_SECONDS: f64 = 0.01;

/// Which sounding notes to release at the start of the next block.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Flush {
    Nothing,
    /// The notes of the song, because it jumped (a seek, the loop coming
    /// round). What is being played by hand is left alone: a chord held
    /// across the loop point keeps sounding.
    Song,
    /// Everything, held keys included: the transport stopped.
    Everything,
}

/// Something played by hand, waiting for its block.
#[derive(Copy, Clone)]
struct LiveEvent {
    target: LiveTarget,
    kind: EventKind,
    /// Frames from the start of the next `process` call.
    offset: u32,
    /// How many frames before the frame it is placed at it was played.
    early: f64,
}

/// A take in progress.
#[derive(Copy, Clone)]
struct Recording {
    tracks: TrackSet,
    /// Frames of count-in still to go; the song starts when it reaches 0.
    count_in: u64,
    /// Added to the count-in's positions for the metronome, a whole number
    /// of bars, so that counting in to the start of the song does not ask
    /// it for clicks at negative positions.
    click_shift: f64,
    /// How many times the loop has come round during the take.
    pass: u32,
    /// Set as the song starts rolling: the next block reports where the
    /// hand has left each controller, which is how the take begins.
    standing_due: bool,
    /// The stretch of the song the take is punched in over, if it is: only
    /// what is played and sung inside it is the take's. And whether the
    /// song was inside it when that was last looked at.
    punch: Option<(f64, f64)>,
    inside: bool,
}

/// A signal played to the output while an input is listened to, to find
/// out how long sound takes to go round: see
/// [`Session::measure_latency`](crate::Session::measure_latency).
pub(crate) struct Measure {
    /// What is played, to both sides, and how loud.
    pub signal: Arc<[f32]>,
    pub gain: f32,
    /// Which channel of the input is listened to, and room for what it
    /// hears from the frame the signal begins at: longer than the signal
    /// by as long as it may take to come back.
    pub channel: usize,
    pub heard: Box<[f32]>,
    /// Frames done so far.
    pub at: usize,
}

impl Measure {
    /// Plays the next `frames` frames into the block and keeps what the
    /// input heard meanwhile. Returns whether that was the last of it.
    fn block(&mut self, left: &mut [f32], right: &mut [f32], heard: Option<&[f32]>, frames: usize) -> bool {
        for frame in 0..frames {
            let at = self.at + frame;
            if let Some(sample) = self.signal.get(at) {
                left[frame] += sample * self.gain;
                right[frame] += sample * self.gain;
            }
            if let Some(kept) = self.heard.get_mut(at) {
                *kept = heard.map_or(0.0, |heard| heard[frame]);
            }
        }
        self.at += frames;
        self.at >= self.heard.len()
    }
}

pub struct Processor {
    graph: Box<Graph>,
    sample_rate: f64,
    playing: bool,
    /// Where the song was last put (started, sought, come round its loop),
    /// and the frames played since. The song's position is that many
    /// frames on from there along its tempo ([`TempoMap::beat_after`]),
    /// which keeps it exact however long playback runs and however the
    /// tempo changes on the way.
    origin_beat: f64,
    since_origin: u64,
    /// The origin in frames from beat 0, which has a fraction: audio is
    /// read by it and by the whole frames played since.
    origin_frames: f64,
    /// The stretch of the song that is playing, through which it moves
    /// evenly: `anchor_beat` plus `frames` at `beats_per_frame`. With one
    /// tempo that is the whole song from the origin on. Where the tempo
    /// jumps, a stretch ends at the first frame at or past the jump, and
    /// the next begins where the tempo has the song at that frame; while
    /// the tempo glides, a stretch is [`PARAM_STEP`] frames of it.
    anchor_beat: f64,
    frames: u64,
    beats_per_frame: f64,
    /// The tempo of the stretch, as a device is told it.
    tempo: f64,
    /// How many frames long the stretch is.
    stretch_frames: u64,
    /// What to release at the start of the next block.
    flush: Flush,
    /// What was played by hand since the last callback.
    live: Vec<LiveEvent>,
    /// The tracks a keyboard plays.
    live_targets: TrackSet,
    /// Room for the block of the track that is being made.
    room: Room,
    /// The threads a render makes its blocks on, if it was given any: see
    /// [`Processor::spread_over`]. Never while a song plays.
    spread: Option<Box<Spread>>,
    /// The threads a song that plays may be made on beside this one, if it
    /// was given any: see [`Processor::play_over`].
    team: Option<Box<Team>>,
    /// Whether the blocks of the song that plays are being spread, where
    /// there is a team and the graph can use one; the share of a block's
    /// time above which they are; and how much of it the tracks have
    /// taken of late.
    /// Whether the cycles a thread runs over each track's block are counted
    /// beside the clock: see [`Processor::count_cycles`].
    cycles: bool,
    spreading: bool,
    /// Nothing is known yet of what the song costs to play: devices came
    /// with its graph that have not been through a quarter of a second of
    /// playing. Until that is known the song is spread wherever it can be
    /// (there is a team, and the graph says what can be made at once), so
    /// that the first buffers of a song that needs its threads are made on
    /// them; a song that does not is back on one thread a quarter of a
    /// second into its first play.
    unknown: bool,
    spread_above: f64,
    weight: f64,
    /// What the tracks have taken to make, and how long the buffers they
    /// were made for last, since the two were last held against each
    /// other: both in nanoseconds. See [`WINDOW_NS`].
    window: (f64, f64),
    /// What the tracks of the blocks of this call have taken to make so
    /// far, in nanoseconds, added up over whoever made them.
    made_ns: u64,
    /// How many blocks were spread since that was last asked.
    spread_blocks: u64,
    /// Set when the song starts or jumps: the next block says where every
    /// controller is at its first frame.
    chase: bool,
    /// Set when the song starts playing or jumps while it plays: the
    /// controllers it has curves for are its own again, whoever moved them.
    take_back: bool,
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    /// How many frames an abrupt change of a gain is spread over.
    declick: u32,
    /// The audition voice and the metronome, heard but never rendered.
    monitor: Monitor,
    /// Frames between rendering a frame and hearing it, as the device last
    /// said and as it has said of late (negative before it said anything).
    output_latency: f64,
    steady_latency: f64,
    recording: Option<Recording>,
    /// Where what is recorded goes; `None` when rendering offline.
    capture: Option<rtrb::Producer<Captured>>,
    /// Frames played since the song position last jumped (or playback
    /// began), and where the song was just before: for telling where the
    /// song was heard to be a moment ago.
    continuity: u64,
    before_jump: f64,
    jump_was_wrap: bool,
    /// What an input device hears, in the engine's clock: a frame of it
    /// for every frame made ([`crate::capture`]). `None` with no input
    /// open, and always in a render.
    input: Option<Box<InputReader>>,
    /// Where the recorded inputs' frames go while a take is recorded, and
    /// whether they are being handed over; how many frames are still to
    /// be handed over after the song has stopped (what was sung to the
    /// last of it is still on its way in); and the ends that are done
    /// with, to be let go of elsewhere.
    feeds: Option<Box<Feeds>>,
    feeding: bool,
    trailing: Option<u64>,
    finished_feeds: Option<Box<Feeds>>,
    /// A measurement of how long sound takes to go round, while one runs,
    /// and one that has ended.
    measure: Option<Box<Measure>>,
    finished_measure: Option<Box<Measure>>,
}

impl Processor {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            graph: Box::new(Graph::empty()),
            sample_rate,
            playing: false,
            origin_beat: 0.0,
            since_origin: 0,
            origin_frames: 0.0,
            anchor_beat: 0.0,
            frames: 0,
            beats_per_frame: 120.0 / (60.0 * sample_rate),
            tempo: 120.0,
            stretch_frames: u64::MAX,
            flush: Flush::Nothing,
            live: Vec::with_capacity(LIVE_CAPACITY),
            live_targets: TrackSet::default(),
            room: Room::new(),
            spread: None,
            team: None,
            cycles: false,
            spreading: false,
            unknown: false,
            spread_above: SPREAD_ABOVE,
            weight: 0.0,
            window: (0.0, 0.0),
            made_ns: 0,
            spread_blocks: 0,
            chase: true,
            take_back: false,
            mix_left: vec![0.0; MAX_BLOCK],
            mix_right: vec![0.0; MAX_BLOCK],
            declick: (DECLICK_SECONDS * sample_rate).round().max(1.0) as u32,
            monitor: Monitor::new(sample_rate),
            output_latency: 0.0,
            steady_latency: -1.0,
            recording: None,
            capture: None,
            continuity: 0,
            before_jump: 0.0,
            jump_was_wrap: false,
            input: None,
            feeds: None,
            feeding: false,
            trailing: None,
            finished_feeds: None,
            measure: None,
            finished_measure: None,
        }
    }

    /// Gives the processor what an input device hears, or takes it away,
    /// and returns what it had, to be let go of off the audio thread.
    pub(crate) fn set_input(&mut self, input: Option<Box<InputReader>>) -> Option<Box<InputReader>> {
        let before = std::mem::replace(&mut self.input, input);
        self.read_what_is_wanted();
        before
    }

    /// Gives a processor that is driven by hand an input to hear: one
    /// that is read `delay_frames` of the processor's frames after it was
    /// heard, on the processor's own clock or (`one_clock`) on one they
    /// share. Before each buffer the input's side has pushed what there
    /// is and [`Processor::begin_input`] has been called. This allocates:
    /// never while the processor plays.
    pub fn hear(&mut self, source: crate::capture::Source, delay_frames: u32, one_clock: bool) {
        let reader = InputReader::new(source.0, self.sample_rate, delay_frames, one_clock);
        self.set_input(Some(Box::new(reader)));
    }

    /// The device has asked for `frames` frames at `now_ns`: the input is
    /// told, which sets how fast it is read for them.
    pub fn begin_input(&mut self, now_ns: u64, frames: usize) {
        if let Some(input) = &mut self.input {
            input.begin(now_ns, frames);
        }
    }

    /// Has only the channels of the input read that something takes: a
    /// track that lets its input through, a take, a measurement.
    fn read_what_is_wanted(&mut self) {
        let recorded = self.feeds.as_ref().map_or(0, |feeds| feeds.channels());
        let measured = self.measure.as_ref().map_or(0, |measure| 1u32.checked_shl(measure.channel as u32).unwrap_or(0));
        let wanted = self.graph.inputs | recorded | measured;
        if let Some(input) = &mut self.input {
            input.want(wanted);
        }
    }

    /// Gives the processor where the recorded inputs of the take that is
    /// asked for next go, and returns the ends of a take before it that
    /// it still had.
    pub(crate) fn set_feeds(&mut self, feeds: Box<Feeds>) -> Option<Box<Feeds>> {
        let before = self.feeds.replace(feeds);
        (self.feeding, self.trailing) = (false, None);
        self.read_what_is_wanted();
        before
    }

    /// The ends of a take's files that nothing more is handed to, to be
    /// let go of off the audio thread.
    pub(crate) fn take_finished_feeds(&mut self) -> Option<Box<Feeds>> {
        self.finished_feeds.take()
    }

    /// Begins a measurement, and returns one that was still running.
    pub(crate) fn start_measure(&mut self, measure: Box<Measure>) -> Option<Box<Measure>> {
        let before = self.measure.replace(measure);
        self.read_what_is_wanted();
        before
    }

    /// A measurement that has ended, with what it heard.
    pub(crate) fn take_finished_measure(&mut self) -> Option<Box<Measure>> {
        self.finished_measure.take()
    }

    /// Ends the take that is being recorded and lets the song play on: its
    /// file is full, its device has gone.
    pub(crate) fn end_take(&mut self) {
        if let Some(heard) = self.heard() {
            let (beat, pass) = heard.at(0, 0.0);
            self.recording = None;
            self.report(Captured::Stopped { beat, pass });
            self.end_audio();
        }
    }

    /// The take's song has stopped, or the take was ended: its audio goes
    /// on being handed over for as long as what was sung to the last of
    /// the song takes to come in.
    fn end_audio(&mut self) {
        if !self.feeding || self.trailing.is_some() {
            return;
        }
        let Some(feeds) = &self.feeds else { return };
        let (at, more) = (feeds.handed, u64::from(feeds.tail));
        let delay = self.input.as_ref().map_or(0, |input| u64::from(input.delay()));
        self.report(Captured::Audio { at, mark: AudioMark::Stopped { beat: self.position() } });
        self.trailing = Some((delay + self.steady_output_latency().ceil() as u64 + more).max(1));
    }

    /// Nothing more of the take's audio is handed over.
    fn finish_audio(&mut self) {
        let at = self.feeds.as_ref().map_or(0, |feeds| feeds.handed);
        self.report(Captured::Audio { at, mark: AudioMark::Ended });
        (self.feeding, self.trailing) = (false, None);
        self.finished_feeds = self.feeds.take();
        self.read_what_is_wanted();
    }

    /// Says that the song has come into the stretch a take is punched in
    /// over, and that it has left it, each time it does: asked at the
    /// first frame of every block, and blocks end on both edges.
    fn mark_punch(&mut self) {
        let Some(recording) = self.recording else { return };
        let Some((from, to)) = recording.punch else { return };
        if !self.playing || recording.count_in > 0 {
            return;
        }
        let position = self.position();
        let at = self.feeds.as_ref().map_or(0, |feeds| feeds.handed);
        let inside = position >= from - 1e-9 && position < to - 1e-9;
        if inside == recording.inside {
            return;
        }
        if let Some(recording) = &mut self.recording {
            recording.inside = inside;
        }
        let mark =
            if inside { AudioMark::PunchedIn { beat: position } } else { AudioMark::PunchedOut { beat: position } };
        self.report(Captured::Audio { at, mark });
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    fn beats_per_frame(&self) -> f64 {
        self.beats_per_frame
    }

    /// Current song position in beats.
    pub fn position(&self) -> f64 {
        self.anchor_beat + self.frames as f64 * self.beats_per_frame
    }

    /// Puts the song at `beat`: frames are counted from here.
    fn put(&mut self, beat: f64) {
        self.origin_beat = beat;
        self.since_origin = 0;
        let map = &self.graph.tempo_map;
        self.origin_frames = match map.is_constant() {
            // As it was always worked out for a song of one tempo.
            true => beat / (map.tempo_at(0.0) / (60.0 * self.sample_rate)),
            false => map.frames_at(beat, self.sample_rate),
        };
        self.begin_stretch();
    }

    /// The song's position `ahead` frames on from where it is, along its
    /// tempo from the origin.
    fn position_after(&self, ahead: u64) -> f64 {
        self.graph.tempo_map.beat_after(self.origin_beat, (self.since_origin + ahead) as f64, self.sample_rate)
    }

    /// Where the block that begins at `beat` is in the song's time and
    /// among its bars. The time is the frames the song has played: where
    /// it was put, to the nearest frame, and those counted since. So it
    /// goes on by a block's length with every block, through every change
    /// of tempo, and is the tempo map's time for `beat` to within a frame.
    fn song_time(&self, beat: f64) -> SongTime {
        let (tempo, meter) = (&self.graph.tempo_map, &self.graph.meter_map);
        let bar = meter.bar_at(beat);
        SongTime {
            seconds: (self.origin_frames.round() + self.since_origin as f64) / self.sample_rate,
            bar: bar.index,
            bar_start: bar.start,
            loop_seconds: self.graph.loop_region.map(|(start, end)| (tempo.seconds_at(start), tempo.seconds_at(end))),
        }
    }

    /// Begins the stretch of even movement that the frame now due is the
    /// first of: see the fields.
    fn begin_stretch(&mut self) {
        let map = &self.graph.tempo_map;
        let rate = self.sample_rate;
        let beat = self.position_after(0);
        self.anchor_beat = beat;
        self.frames = 0;
        if map.is_ramping_at(beat) {
            // A step of the glide, or what is left of it: taken evenly, so
            // that the song is where its tempo has it at both ends.
            let end = map.next_change(beat).unwrap_or(f64::INFINITY);
            let left = (map.frames_between(beat, end, rate) - 1e-6).ceil().max(1.0);
            let step = if left < PARAM_STEP as f64 { left as u64 } else { PARAM_STEP };
            self.beats_per_frame = (self.position_after(step) - beat) / step as f64;
            self.tempo = self.beats_per_frame * 60.0 * rate;
            self.stretch_frames = step;
        } else {
            self.tempo = map.tempo_at(beat);
            self.beats_per_frame = self.tempo / (60.0 * rate);
            self.stretch_frames = match map.next_change(beat) {
                // To the first frame at or past the change. The small bias
                // absorbs float rounding, as it does for a loop.
                Some(change) => ((change - beat) / self.beats_per_frame - 1e-6).ceil().max(1.0) as u64,
                None => u64::MAX,
            };
        }
    }

    pub fn play(&mut self) {
        // Asked to play in the middle of a count-in: the count-in is over.
        if let Some(recording) = &mut self.recording
            && recording.count_in > 0
        {
            recording.count_in = 0;
            self.rolling();
        }
        if !self.playing {
            self.playing = true;
            self.chase = true;
            self.take_back = true;
            self.jumped(self.position(), false);
            // What the song costs is measured anew from here: a quarter
            // of a second that is all playing.
            self.window = (0.0, 0.0);
        }
    }

    /// Reports that the take's song has started rolling from here.
    fn rolling(&mut self) {
        if let Some(recording) = &mut self.recording {
            recording.standing_due = true;
        }
        self.report(Captured::Rolling { beat: self.position() });
        if let (true, Some(feeds)) = (self.feeding, &self.feeds) {
            let at = feeds.handed;
            let delay = self.input.as_ref().map_or(0, |input| input.delay());
            let latency = self.steady_output_latency().round() as u32;
            let mark = AudioMark::Rolling { beat: self.position(), latency, delay };
            self.report(Captured::Audio { at, mark });
        }
    }

    /// Stops the transport. Sounding notes are released, those held by
    /// hand included (stopping a song that is not playing is how to
    /// silence a key that hangs), and tails ring out. A take that was being
    /// recorded ends here.
    pub fn stop(&mut self) {
        if let Some(heard) = self.heard() {
            // Where the song was heard to be when it was stopped.
            let (beat, pass) = heard.at(0, 0.0);
            self.recording = None;
            self.report(Captured::Stopped { beat, pass });
            self.end_audio();
        }
        self.playing = false;
        self.flush = Flush::Everything;
        self.window = (0.0, 0.0);
    }

    /// Forgets what its devices have measured so far (how far each has
    /// turned the signal down): a render asks this once it has played what
    /// it keeps nothing of.
    pub(crate) fn forget_measured(&mut self) {
        let chains = self.graph.tracks.iter_mut().map(|track| &mut track.chain);
        for slot in chains.chain(std::iter::once(&mut self.graph.master.chain)).flat_map(|chain| &mut chain.devices) {
            if let Some(processor) = slot.processor.as_mut() {
                let _ = processor.gain_reduction();
            }
        }
    }

    /// Lets go of every note that sounds, played by hand or by the song,
    /// at the start of the next block.
    pub(crate) fn release_everything(&mut self) {
        self.flush = Flush::Everything;
    }

    /// Puts things right after a block that a device went down over
    /// (`process` did not come back from it): what was played by hand for
    /// that call is let go of, so that the key a device fails on is not
    /// played to it again with every block from then on, and everything
    /// that sounds is released, because what would have ended a note may
    /// be among what was lost.
    pub(crate) fn after_fault(&mut self) {
        self.live.clear();
        self.flush = Flush::Everything;
    }

    /// What it takes to say where the song is heard to be right now, while
    /// a take is being recorded.
    fn heard(&self) -> Option<Heard> {
        self.recording.map(|recording| Heard {
            tracks: recording.tracks,
            pass: recording.pass,
            start_beat: self.position(),
            beats_per_frame: self.beats_per_frame(),
            count_in: recording.count_in as f64,
            latency: self.output_latency,
            continuity: self.continuity as f64,
            before_jump: self.before_jump,
            jump_was_wrap: self.jump_was_wrap,
        })
    }

    pub fn seek(&mut self, beat: f64) {
        let before = self.position();
        self.put(beat.max(0.0));
        self.flush = self.flush.max(Flush::Song);
        self.chase = true;
        // A song that stands still takes nothing back: what the hand set
        // while it stood is for playing by hand until it runs.
        self.take_back |= self.playing;
        self.jumped(before, false);
    }

    /// Notes that the song position is no longer where the frames played
    /// so far lead to: it was at `before` and is now somewhere else.
    fn jumped(&mut self, before: f64, wrap: bool) {
        self.continuity = 0;
        self.before_jump = before;
        self.jump_was_wrap = wrap;
        // A take's audio is told where the song was sent. (Where it came
        // round its loop is said there, by the loop's own end.)
        let to = self.position();
        if !wrap && to != before {
            self.mark_jump(before, to, false);
        }
    }

    /// Tells a take's audio that the song is no longer where the frames
    /// handed over lead to: what is handed over from here on was sung to
    /// another place. Only while the take's song rolls; where it rolls
    /// from is said when it does.
    fn mark_jump(&mut self, from: f64, to: f64, round: bool) {
        let rolls = self.playing && self.recording.is_some_and(|recording| recording.count_in == 0);
        if rolls
            && self.feeding
            && self.trailing.is_none()
            && let Some(at) = self.feeds.as_ref().map(|feeds| feeds.handed)
        {
            self.report(Captured::Audio { at, mark: AudioMark::Jumped { from, to, round } });
        }
    }

    /// Silences everything immediately, including reverb and delay tails.
    pub fn panic(&mut self) {
        self.flush = Flush::Everything;
        for track in &mut self.graph.tracks {
            for slot in &mut track.chain.devices {
                if let Some(processor) = slot.processor.as_mut() {
                    processor.reset();
                }
            }
            // A device that was reset may have forgotten where its
            // controllers were.
            track.state.resend = true;
        }
        for slot in &mut self.graph.master.chain.devices {
            if let Some(processor) = slot.processor.as_mut() {
                processor.reset();
            }
        }
    }

    pub fn set_param(&mut self, device: DeviceId, index: usize, value: f64) {
        if let Some(processor) = self.graph.processor_mut(device) {
            processor.set_param(index, value);
        }
    }

    /// Starts auditioning a sound over the song: once through, or with
    /// `round` again and again. Returns the buffer that was being
    /// auditioned before, to be freed off the audio thread.
    pub fn start_audition(
        &mut self,
        audio: std::sync::Arc<AudioData>,
        gain: f32,
        round: Option<crate::monitor::Round>,
    ) -> Option<std::sync::Arc<AudioData>> {
        self.monitor.start_audition(audio, gain, round)
    }

    pub fn stop_audition(&mut self) {
        self.monitor.stop_audition();
    }

    /// The buffer of an audition that has ended, to be freed off the audio
    /// thread.
    pub fn take_finished_audition(&mut self) -> Option<std::sync::Arc<AudioData>> {
        self.monitor.take_finished()
    }

    pub fn is_auditioning(&self) -> bool {
        self.monitor.is_auditioning()
    }

    pub fn set_metronome(&mut self, on: bool) {
        self.monitor.set_metronome(on);
    }

    /// How long after a frame is rendered it is heard, in frames. Takes are
    /// written where the player heard the song to be, which is this much
    /// behind where the engine is.
    pub fn set_output_latency(&mut self, frames: u32) {
        self.output_latency = frames as f64;
        // What a device says of it with every buffer flutters by a
        // fraction of a millisecond with the moment it is asked; what
        // lies between the engine and the speaker does not. A take's
        // audio is placed by a figure that moves over seconds, so that
        // two takes, and a measurement, are placed by the same one.
        let steady = self.steady_latency;
        let far = (self.output_latency - steady).abs() > 0.02 * self.sample_rate;
        self.steady_latency =
            if steady < 0.0 || far { self.output_latency } else { steady + (self.output_latency - steady) * 0.004 };
    }

    /// [`Processor::set_output_latency`] as it has been of late: what a
    /// take's audio is placed by.
    pub(crate) fn steady_output_latency(&self) -> f64 {
        self.steady_latency.max(0.0)
    }

    /// Where a take's events are reported. Returns where they went before.
    pub(crate) fn set_capture(
        &mut self,
        capture: Option<rtrb::Producer<Captured>>,
    ) -> Option<rtrb::Producer<Captured>> {
        std::mem::replace(&mut self.capture, capture)
    }

    fn report(&mut self, captured: Captured) {
        if let Some(capture) = &mut self.capture {
            // A full queue loses the report: nobody is collecting.
            let _ = capture.push(captured);
        }
    }

    /// Plays `kind` on `track` at the start of the next block. Dropped if
    /// more is waiting than the processor holds.
    pub fn live_event(&mut self, track: TrackId, kind: EventKind) {
        self.live_on(track, kind, 0, 0.0);
    }

    /// Plays `kind` on `track`, `offset` frames into the next call to
    /// [`Processor::process`]. It was played `early` frames before that
    /// frame.
    pub(crate) fn live_on(&mut self, track: TrackId, kind: EventKind, offset: u32, early: f64) -> Taken {
        self.push_live(LiveEvent { target: LiveTarget::Track(track), kind, offset, early })
    }

    /// Like [`Processor::live_on`], on the tracks a keyboard plays.
    pub(crate) fn live_routed(&mut self, kind: EventKind, offset: u32, early: f64) -> Taken {
        self.push_live(LiveEvent { target: LiveTarget::Routed, kind, offset, early })
    }

    /// Takes a live event for the next call to `process`. There is room
    /// for [`LIVE_CAPACITY`] between two calls, and when it runs short it
    /// is what would leave a note hanging that is kept longest:
    ///
    /// * a wheel or a controller is thinned from [`LIVE_THIN_AT`] on: its
    ///   newest value replaces the one waiting, so where it ends up is
    ///   never lost, only some of the way there;
    /// * new notes and controllers stop [`LIVE_RESERVE`] short of the end;
    /// * what ends notes is taken to the very last slot.
    fn push_live(&mut self, event: LiveEvent) -> Taken {
        let waiting = self.live.len();
        let capacity = self.live.capacity();
        let ends_notes =
            matches!(event.kind, EventKind::NoteOff { .. } | EventKind::AllNotesOff) || event.kind.sustain().is_some();
        if ends_notes {
            if waiting == capacity {
                return Taken::Lost;
            }
        } else if let EventKind::NoteOn { .. } = event.kind {
            if waiting + LIVE_RESERVE >= capacity {
                return Taken::Skipped;
            }
        } else {
            if waiting >= LIVE_THIN_AT {
                let same = |held: &&mut LiveEvent| held.target == event.target && same_control(held.kind, event.kind);
                if let Some(held) = self.live.iter_mut().rev().find(same) {
                    held.kind = event.kind;
                    held.offset = held.offset.max(event.offset);
                    held.early = event.early;
                    return Taken::Kept;
                }
            }
            if waiting + LIVE_RESERVE >= capacity {
                return Taken::Lost;
            }
        }
        self.live.push(event);
        Taken::Kept
    }

    /// Sets the tracks a keyboard plays. A track that no longer is one lets
    /// go of what the keyboard was holding on it.
    pub(crate) fn set_live_targets(&mut self, targets: TrackSet) {
        for track in &mut self.graph.tracks {
            if self.live_targets.contains(track.id) && !targets.contains(track.id) {
                track.state.release_routed |= track.state.holds_routed();
            }
        }
        self.live_targets = targets;
    }

    /// Lets go of everything a keyboard is holding, on every track.
    pub(crate) fn release_live(&mut self) {
        for track in &mut self.graph.tracks {
            track.state.release_routed |= track.state.holds_routed();
        }
    }

    /// Starts the take numbered `take` on `tracks`. If the song is stopped
    /// it starts playing, after `count_in_beats` of metronome; if it is
    /// playing, the take begins where the song is. With `punch`, only what
    /// is played and sung while the song is inside that stretch is the
    /// take's. The inputs that are recorded are handed to the ends the
    /// processor was given for this take ([`Processor::set_feeds`]) from
    /// here on, the count-in too: what was sung into the downbeat is there
    /// to be drawn out from under the clip's edge.
    pub(crate) fn record(&mut self, take: u32, tracks: TrackSet, count_in_beats: f64, punch: Option<(f64, f64)>) {
        self.report(Captured::Began { take });
        (self.feeding, self.trailing) = (self.feeds.is_some(), None);
        // The take is counted in at the tempo, and in the meter, that the
        // song has where it starts.
        let signature = self.graph.meter_map.signature_at(self.position());
        let bar = signature.numerator as f64 * 4.0 / signature.denominator as f64;
        let count_in_beats = if self.playing || !count_in_beats.is_finite() { 0.0 } else { count_in_beats.max(0.0) };
        let count_in = (count_in_beats / self.beats_per_frame()).round() as u64;
        let click_shift = if bar > 0.0 { (count_in_beats / bar).ceil() * bar } else { 0.0 };
        self.recording =
            Some(Recording { tracks, count_in, click_shift, pass: 0, standing_due: false, punch, inside: false });
        if count_in == 0 {
            self.play();
            self.rolling();
        }
    }

    pub(crate) fn record_phase(&self) -> RecordPhase {
        match &self.recording {
            None => RecordPhase::Off,
            Some(recording) if recording.count_in > 0 => RecordPhase::CountIn,
            Some(_) => RecordPhase::Rolling,
        }
    }

    /// Installs `new`, carrying running state across, and returns the old
    /// graph so the caller can free it off the audio thread.
    pub fn swap_graph(&mut self, mut new: Box<Graph>) -> Box<Graph> {
        // Threads were given for the graph that goes. (A processor that
        // plays never has any: nothing is let go of here on the audio
        // thread.)
        if self.spread.is_some() {
            self.spread = None;
        }
        let position = self.position();
        // Devices that come with the graph have never been run here: what
        // the song costs with them is not known.
        if new.brings_devices() {
            self.unknown = true;
            self.window = (0.0, 0.0);
        }
        new.adopt(&mut self.graph);
        if new.rounds.is_none() {
            self.spreading = false;
        }
        let tempo_changed = new.tempo_map != self.graph.tempo_map;
        let old = std::mem::replace(&mut self.graph, new);
        self.read_what_is_wanted();
        if tempo_changed {
            // The song stays at the beat it is at, and goes on from there
            // at the tempo it now has.
            self.put(position);
        }
        old
    }

    /// Has the blocks of the graph it now holds made on up to `threads`
    /// threads at once, this one among them, and returns how many it will
    /// be: as many as there are tracks that can be made at once, at the
    /// most, and one where every track waits for another. For a render
    /// only: threads are started and waited on, which a processor that
    /// plays must never do. The samples are the ones one thread makes.
    pub fn spread_over(&mut self, threads: usize) -> usize {
        self.spread = Spread::new(&self.graph, threads).map(Box::new);
        self.spread.as_ref().map_or(1, |spread| spread.threads())
    }

    /// Gives the song that plays the threads it may be made on beside
    /// this one, or takes them away, and returns the ones it had: they are
    /// to be let go of off the audio thread. A block is spread over them
    /// where the graph says what can be made at once ([`Graph::rounds`])
    /// and making its tracks is worth handing across
    /// ([`Processor::spread_from`]); the samples are the ones this thread
    /// alone makes.
    pub fn play_over(&mut self, team: Option<Box<Team>>) -> Option<Box<Team>> {
        std::mem::replace(&mut self.team, team)
    }

    /// Sets the share of a block's own time that making the tracks must
    /// take before a song that plays is spread over its team:
    /// [`SPREAD_ABOVE`] unless told otherwise. Nothing spreads every block
    /// from the first, which is how to hear that it makes no difference.
    pub fn spread_from(&mut self, share: f64) {
        self.spread_above = share.max(0.0);
        if share <= 0.0 {
            self.spreading = true;
        }
    }

    /// Has the cycles a thread runs over each track's block counted beside
    /// the clock's time, for the longest block of each
    /// ([`crate::graph::Cost::longest_ran`]): what tells a block that took
    /// long to work out from a thread the system kept from working. It
    /// asks the system twice a track and block, so it is for measuring and
    /// off otherwise.
    pub fn count_cycles(&mut self, on: bool) {
        self.cycles = on;
    }

    /// Whether the song that plays would be spread if it had threads to be
    /// spread over: it takes long enough to make, it can be made in
    /// parts, and nobody has given it any.
    pub fn wants_threads(&self) -> bool {
        // Asked for at half of what the song is spread at, and by the most
        // a buffer took of late: so that they are there when it is.
        let may_soon = self.weight > self.spread_above * SPREAD_BELOW;
        (self.spreading || may_soon) && self.team.is_none() && self.graph.rounds.is_some()
    }

    /// How many threads make the blocks of the song that plays right now,
    /// this one among them.
    pub fn threads(&self) -> usize {
        match (self.spreading || self.unknown, &self.team, &self.graph.rounds) {
            (true, Some(team), Some(rounds)) => (team.len() + 1).min(rounds.width()),
            _ => 1,
        }
    }

    /// How many blocks were spread over more than one thread since this
    /// was last asked.
    pub fn take_spread_blocks(&mut self) -> u64 {
        std::mem::take(&mut self.spread_blocks)
    }

    /// How long this thread has waited for the others a render was spread
    /// over, its own share of the work done: nothing where it has none.
    pub fn waited_for_threads(&self) -> std::time::Duration {
        self.spread.as_ref().map_or(std::time::Duration::ZERO, |spread| spread.waited())
    }

    /// Renders `left.len()` frames. The slices must have equal length.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let _denormals = DenormalGuard::new();
        let total = left.len().min(right.len());
        let mut done = 0;
        while done < total {
            let mut frames = (total - done).min(MAX_BLOCK);

            // End the block exactly where a count-in ends, so the song
            // starts on the sample.
            let counting = self.recording.map_or(0, |recording| recording.count_in);
            let starts = counting > 0 && counting <= frames as u64;
            if starts {
                frames = counting as usize;
            }

            // End the block where the next parameter is due to move.
            if self.playing && self.graph.moves_params {
                frames = frames.min((PARAM_STEP - self.frames % PARAM_STEP) as usize);
            }

            // End the block where the stretch does: at the first frame at
            // or past a change of tempo. And at the first frame at or past
            // a change of time signature, so that a block is in one meter.
            if self.playing {
                frames = (frames as u64).min(self.stretch_frames - self.frames) as usize;
                let position = self.position();
                if let Some(change) = self.graph.meter_map.next_change(position) {
                    let until = ((change - position) / self.beats_per_frame - 1e-6).ceil().max(1.0);
                    if until < frames as f64 {
                        frames = until as usize;
                    }
                }
                // And where a take that is punched in begins and ends, so
                // that what is under it goes and comes on the frame.
                if let Some((from, to)) = self.recording.and_then(|recording| recording.punch) {
                    for edge in [from, to] {
                        if position < edge - 1e-9 {
                            let until = ((edge - position) / self.beats_per_frame - 1e-6).ceil().max(1.0);
                            if until < frames as f64 {
                                frames = until as usize;
                            }
                        }
                    }
                }
            }

            // End the block exactly at the loop end so the wrap is sample
            // accurate.
            let mut wrap = None;
            if self.playing
                && let Some((start, end)) = self.graph.loop_region
            {
                let position = self.position();
                if position < end {
                    // The small bias absorbs float rounding, so a loop that
                    // is a whole number of frames long stays exactly that.
                    let until_end = ((end - position) / self.beats_per_frame() - 1e-6).ceil().max(1.0);
                    if until_end <= frames as f64 {
                        frames = until_end as usize;
                        wrap = Some((start, end));
                    }
                }
            }

            let limit = wrap.map(|(_, end)| end);
            self.mark_punch();
            self.process_block(&mut left[done..done + frames], &mut right[done..done + frames], limit, done);
            done += frames;
            if let Some(left) = self.trailing {
                self.trailing = Some(left.saturating_sub(frames as u64));
                if left <= frames as u64 {
                    self.finish_audio();
                }
            }

            if self.playing {
                self.frames += frames as u64;
                self.since_origin += frames as u64;
                self.continuity += frames as u64;
                if self.frames >= self.stretch_frames && wrap.is_none() {
                    self.begin_stretch();
                }
                if let Some((start, end)) = wrap {
                    // Restart exactly on the loop start. The block above ran
                    // to the first frame at or past the loop end, so a loop
                    // is at most one frame longer than its length in beats,
                    // and a note sitting on the loop start is never missed.
                    let reached = self.position();
                    self.put(start);
                    self.flush = self.flush.max(Flush::Song);
                    self.chase = true;
                    self.take_back = true;
                    self.jumped(reached, true);
                    // A take's audio came round with it: a pass ends on
                    // the loop's end, and the next begins on its start.
                    self.mark_jump(end, start, true);
                    let pass = self.recording.as_mut().map(|recording| {
                        recording.pass += 1;
                        recording.pass
                    });
                    if let Some(pass) = pass {
                        self.report(Captured::Wrapped { pass });
                    }
                }
            }
            if let Some(recording) = &mut self.recording {
                recording.count_in -= (frames as u64).min(recording.count_in);
            }
            if starts {
                self.play();
                self.rolling();
            }
        }

        // How much of the time this call covers its tracks took to make,
        // added up over whoever made them, with what it was of late fading
        // behind it: by that a song that plays is spread, and gathered
        // again. (Over the call and not block by block: a block may be a
        // frame long, and what devices cost to be called at all says
        // nothing of the song.)
        let call_ns = total as f64 * 1e9 / self.sample_rate;
        let made_ns = std::mem::take(&mut self.made_ns) as f64;
        let share = if call_ns > 0.0 { made_ns / call_ns } else { 0.0 };
        self.weight = share.max(self.weight * 0.5f64.powf(call_ns / WEIGHT_HALVES_NS));
        self.window = (self.window.0 + made_ns, self.window.1 + call_ns);
        let always = self.spread_above <= 0.0;
        if self.graph.rounds.is_none() {
            self.spreading = false;
        } else if always || share > URGENT {
            self.spreading = true;
        }
        if self.window.1 >= WINDOW_NS {
            let level = self.window.0 / self.window.1;
            self.window = (0.0, 0.0);
            if self.graph.rounds.is_some() && !always {
                if level > self.spread_above {
                    self.spreading = true;
                } else if level < self.spread_above * SPREAD_BELOW && self.playing {
                    self.spreading = false;
                }
            }
            // (The quarter of a second began when the song was started, or
            // later: all of it was playing.)
            if self.playing {
                self.unknown = false;
            }
        }

        // What was placed in this call has been played; anything placed
        // beyond it waits for the next.
        let played = total as u32;
        self.live.retain_mut(|event| {
            let waiting = event.offset >= played;
            if waiting {
                event.offset -= played;
            }
            waiting
        });
    }

    /// Renders one block, which begins `block_start` frames into the
    /// current call to `process`. `limit` caps the beat range that may
    /// trigger notes, so nothing past a loop end sounds before the wrap.
    fn process_block(&mut self, out_left: &mut [f32], out_right: &mut [f32], limit: Option<f64>, block_start: usize) {
        let frames = out_left.len();
        // The input's frames for this block, taken once and before any
        // track is made: every track, on whichever thread, reads the same
        // ones; and a take's files are handed them.
        if let Some(input) = &mut self.input {
            input.take(frames);
        }
        if let (true, Some(feeds)) = (self.feeding, &mut self.feeds) {
            feeds.push(self.input.as_deref(), frames);
        }
        let start_beat = self.position();
        let beats_per_frame = self.beats_per_frame();
        // Computed the same way as the next block's start, so consecutive
        // blocks tile the timeline exactly and a note on a block boundary is
        // played once: never dropped, never doubled. A block that ends its
        // stretch ends where the next one begins: where the tempo has the
        // song at that frame.
        let ends_stretch = self.playing && self.frames + frames as u64 >= self.stretch_frames;
        let next_start = match ends_stretch {
            true => self.position_after(frames as u64),
            false => self.anchor_beat + (self.frames + frames as u64) as f64 * beats_per_frame,
        };
        let end_beat = limit.map_or(next_start, |limit| next_start.min(limit));
        // A note sounds at the frame nearest to it, and is played by the
        // block that frame is in: the blocks hand notes over half a frame
        // before their edges, so that where a note lands does not depend
        // on where a buffer happens to end. Where the song is put down, a
        // stretch begins or the loop ends, the edge is the beat itself.
        let half = 0.5 * beats_per_frame;
        let notes_from = if self.frames > 0 { start_beat - half } else { start_beat };
        let notes_until = if ends_stretch || limit.is_some() { end_beat } else { end_beat - half };
        let signature = self.graph.meter_map.signature_at(start_beat);
        let info = ProcessInfo {
            sample_rate: self.sample_rate,
            tempo: self.tempo,
            time_signature: signature,
            playing: self.playing,
            position_beats: start_beat,
            song: self.song_time(start_beat),
            loop_range: self.graph.loop_region,
        };
        let flush = std::mem::replace(&mut self.flush, Flush::Nothing);
        let playing = self.playing;
        let chase = std::mem::take(&mut self.chase);
        let take_back = std::mem::take(&mut self.take_back);
        let standing_due = self.recording.as_mut().is_some_and(|recording| std::mem::take(&mut recording.standing_due));
        let block = block_start as u32..(block_start + frames) as u32;
        let has_live = self.live.iter().any(|event| block.contains(&event.offset));
        // Where a take's events are written: see `Heard`.
        let heard = self.heard();
        // The tracks a take is being recorded on right now: what they
        // have under it is not heard. With a punch, only inside it.
        let over = self.recording.filter(|recording| playing && recording.count_in == 0).filter(|recording| {
            recording.punch.is_none_or(|(from, to)| start_beat >= from - 1e-9 && start_beat < to - 1e-9)
        });
        let over = over.map_or_else(TrackSet::default, |recording| recording.tracks);
        let span = Span { playing, start_beat, anchor_beat: self.anchor_beat, elapsed: self.frames, beats_per_frame };
        // Where the song is for the parameters its envelopes move.
        let param_beat = span.beat_on_grid(PARAM_STEP);
        let declick = self.declick;

        let mix_left = &mut self.mix_left[..frames];
        let mix_right = &mut self.mix_right[..frames];
        mix_left.fill(0.0);
        mix_right.fill(0.0);

        let Graph { tracks, buses, keys, master, tempo_map, meter_map, rounds, .. } = &mut *self.graph;
        let (tempo_map, meter_map): (&TempoMap, &MeterMap) = (tempo_map, meter_map);
        for bus in buses.iter_mut() {
            bus.left[..frames].fill(0.0);
            bus.right[..frames].fill(0.0);
        }

        let common = Common {
            info: &info,
            flush,
            controls: ControlBlock {
                playing,
                chase,
                take_back,
                start_beat,
                end_beat,
                anchor_beat: self.anchor_beat,
                elapsed: self.frames,
                beats_per_frame,
                frames,
            },
            notes: (notes_from, notes_until),
            origin_frames: self.origin_frames,
            since_origin: self.since_origin,
            tempo_map,
            span,
            param_beat,
            declick,
            sample_rate: self.sample_rate,
            cycles: self.cycles,
            input: self.input.as_deref(),
            over,
        };
        let mut hand = Hand {
            live: &self.live,
            targets: &self.live_targets,
            capture: &mut self.capture,
            flush: &mut self.flush,
            heard,
            block,
            has_live,
            standing_due,
        };
        // Who makes the tracks. A render that was given threads has its
        // own. A song that plays has its team, where the graph says what
        // can be made at once and making it has been taking long enough to
        // be worth handing across. Otherwise this thread makes them one
        // after the other. What the three make is the same.
        let made_ns = if let Some(spread) = self.spread.as_mut() {
            let Spread { crew, rounds } = &mut **spread;
            for (track, room) in tracks.iter_mut().zip(rounds.rooms_mut()) {
                prelude(track, &common, room, &mut hand);
            }
            rounds.block(&*crew, tracks, buses, keys, &common, mix_left, mix_right)
        } else if let (true, Some(team), Some(rounds)) =
            (self.spreading || self.unknown, self.team.as_deref(), rounds.as_deref_mut())
        {
            for (track, room) in tracks.iter_mut().zip(rounds.rooms_mut()) {
                prelude(track, &common, room, &mut hand);
            }
            self.spread_blocks += 1;
            rounds.block(team, tracks, buses, keys, &common, mix_left, mix_right)
        } else {
            let room = &mut self.room;
            let mut made = 0;
            for track in tracks.iter_mut() {
                prelude(track, &common, room, &mut hand);
                let began = Instant::now();
                let ran = common.cycles.then(thread_cycles);
                // A group or return starts from what was fed to its bus;
                // every track that feeds it has already run.
                sound(track, &common, buses, room);
                let devices = track.chain.devices.len();
                through(track, &common, room, keys, 0..devices);
                // What the devices that listen to this track hear of it:
                // as it leaves its own devices, whatever its fader and its
                // mute say.
                let listened = track.key_out.and_then(|key| keys.get_mut(key));
                finish(track, &common, room, listened);
                room.spent = began.elapsed().as_nanos() as u64;
                room.ran = ran.map_or(0, |ran| thread_cycles().saturating_sub(ran));
                made += tally(track, &common, room);
                mix(track, &common, room, buses, mix_left, mix_right);
            }
            made
        };

        self.made_ns += made_ns;

        let master_began = Instant::now();
        if let Some(tap) = &mut master.tap {
            tap.push(mix_left, mix_right);
        }
        move_params(&mut master.chain, param_beat);
        run_devices(&mut master.chain.devices, &info, &[], mix_left, mix_right, keys);

        let (volume, volume_db) = (&master.volume_automation, master.volume_db);
        let level = |beat: f64| {
            let level = volume.as_ref().map_or(volume_db, |envelope| envelope.value_at(beat) as f32);
            [if level > MIN_VOLUME_DB { db_to_gain(level) } else { 0.0 }]
        };
        let mut gains = [&mut self.room.gain_left[..frames]];
        let (peak_left, peak_right) =
            match master.fader.fill(&span, volume.is_some(), declick, frames, &mut gains, level) {
                Some([gain]) => (scale(mix_left, gain), scale(mix_right, gain)),
                None => (scale_by(mix_left, gains[0]), scale_by(mix_right, gains[0])),
            };
        out_left.copy_from_slice(mix_left);
        out_right.copy_from_slice(mix_right);
        master.meter.record(peak_left, peak_right);
        master.meter.cost(master_began.elapsed().as_nanos() as u64, 0, frames);

        // The metronome counts the time signature the block is in, in
        // bars that begin where that signature took over.
        let since = meter_map.since(start_beat);
        let beats_from = |start: f64, until: f64| Beats {
            start,
            per_frame: beats_per_frame,
            count: 4.0 / signature.denominator as f64,
            counts_per_bar: signature.numerator as u32,
            until,
            song: start_beat,
        };
        match self.recording.filter(|recording| recording.count_in > 0) {
            // A count-in clicks whether or not the metronome is on, on the
            // song's own grid running up to where the take starts, in the
            // meter the take starts in.
            Some(recording) => {
                let before = recording.count_in as f64 * beats_per_frame;
                let start = start_beat - since - before + recording.click_shift;
                self.monitor.mix(out_left, out_right, Some(beats_from(start, f64::INFINITY)), true);
            }
            None => {
                let until = meter_map.next_change(start_beat).map_or(f64::INFINITY, |change| change - since);
                self.monitor.mix(out_left, out_right, playing.then(|| beats_from(start_beat - since, until)), false)
            }
        }
        // A measurement plays its signal straight to the device, past the
        // song and the master, and keeps what the input hears meanwhile.
        if let Some(measure) = &mut self.measure {
            let heard = self.input.as_deref().map(|input| input.channel(measure.channel));
            if measure.block(out_left, out_right, heard.filter(|heard| heard.len() >= frames), frames) {
                self.finished_measure = self.measure.take();
                self.read_what_is_wanted();
            }
        }
    }
}

/// What a block is to every track of it alike.
pub(crate) struct Common<'a> {
    info: &'a ProcessInfo,
    /// What every track lets go of as the block begins.
    flush: Flush,
    controls: ControlBlock,
    /// The beats from which, and up to which, a note that starts is the
    /// block's to play: half a frame inside its edges, unless an edge is
    /// where the song was put down or a stretch ends.
    notes: (f64, f64),
    /// Where the song was last put, in frames from beat 0, and the frames
    /// played since: audio is read by them.
    origin_frames: f64,
    since_origin: u64,
    /// How the song's beats become frames.
    tempo_map: &'a TempoMap,
    span: Span,
    /// Where the song is for the parameters its envelopes move.
    param_beat: f64,
    /// How many frames an abrupt change of a gain is spread over.
    declick: u32,
    sample_rate: f64,
    /// The cycles each thread runs over a track's block are counted
    /// beside the clock's time: see [`Processor::count_cycles`].
    pub cycles: bool,
    /// What the audio device's input heard for the block, a frame of it
    /// for every frame of the block: the same for every track.
    input: Option<&'a InputReader>,
    /// The tracks a take is being recorded over in this block.
    over: TrackSet,
}

impl Common<'_> {
    /// How many frames the block has.
    pub fn frames(&self) -> usize {
        self.controls.frames
    }
}

/// Room for one track's block: its sound, the events it is given, and the
/// gains that move over it. One is enough where a block's tracks are made
/// one after the other; where several are made at once, each has its own.
pub(crate) struct Room {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    events: Vec<Event>,
    /// Room to put a block's events in order.
    sorted: Vec<Event>,
    /// Room for a block of gains that move: a fader's two sides, or a send.
    gain_left: Vec<f32>,
    gain_right: Vec<f32>,
    /// Room for how far a gate is open over a block, while it moves.
    shares: Vec<f32>,
    /// Whether what the track has been told so far in the block (by hand,
    /// or by the song's controllers) is not in order of time: said by
    /// [`prelude`] for [`sound`].
    unordered: bool,
    /// How long the track's block has taken to make so far, in
    /// nanoseconds, by whoever made it; and the cycles they ran in that
    /// time, where those are counted.
    pub spent: u64,
    pub ran: u64,
}

impl Room {
    pub fn new() -> Self {
        Self {
            left: vec![0.0; MAX_BLOCK],
            right: vec![0.0; MAX_BLOCK],
            events: Vec::with_capacity(EVENT_CAPACITY),
            sorted: Vec::with_capacity(EVENT_CAPACITY),
            gain_left: vec![0.0; MAX_BLOCK],
            gain_right: vec![0.0; MAX_BLOCK],
            shares: vec![0.0; MAX_BLOCK],
            unordered: false,
            spent: 0,
            ran: 0,
        }
    }
}

/// What a hand did in a block, and where a take's events are reported:
/// what [`prelude`] reads beside the track. All of it belongs to the
/// thread that asked for the block.
pub(crate) struct Hand<'a> {
    /// What was played by hand since the last callback.
    live: &'a [LiveEvent],
    /// The tracks a keyboard plays.
    targets: &'a TrackSet,
    /// Where what is recorded goes.
    capture: &'a mut Option<rtrb::Producer<Captured>>,
    /// Set to have the next block let go of everything: an event that may
    /// have ended a note found no room.
    flush: &'a mut Flush,
    /// Where a take's events are written, while one is recorded.
    heard: Option<Heard>,
    /// The frames of the callback that the block covers.
    block: std::ops::Range<u32>,
    /// Whether anything was played by hand in those frames.
    has_live: bool,
    /// The take has just started rolling: where the hand has left each
    /// controller is reported as how it begins.
    standing_due: bool,
}

/// What a track is told in the block ahead of its own notes, into `room`:
/// the notes that the block lets go of, what a hand played on it, and
/// where its controllers are. What is played while a take is recorded is
/// reported here. Always by the thread that asked for the block, track
/// after track in the order of the graph, whoever makes the tracks
/// afterwards: what was played and the reports go through queues that
/// have one end.
pub(crate) fn prelude(track: &mut TrackNode, common: &Common, room: &mut Room, hand: &mut Hand) {
    (room.spent, room.ran) = (0, 0);
    room.events.clear();
    release_notes(track, common.flush, &mut room.events);
    let events = &mut room.events;
    let recorded = hand.heard.as_ref().filter(|heard| heard.tracks.contains(track.id));
    let mut by_hand = false;
    if std::mem::take(&mut track.state.release_routed) {
        by_hand = true;
        let id = track.id;
        release_routed(track, events, |kind| {
            if let (Some(heard), Some(capture)) = (recorded, &mut *hand.capture) {
                let (beat, pass) = heard.at(0, 0.0);
                let _ = capture.push(Captured::Event { track: id, kind, beat, pass });
            }
        });
    }
    if hand.has_live {
        for event in hand.live.iter().filter(|event| hand.block.contains(&event.offset)) {
            let routed = event.target == LiveTarget::Routed;
            let plays = match event.target {
                LiveTarget::Routed => hand.targets.contains(track.id),
                LiveTarget::Track(id) => id == track.id,
            };
            if !plays {
                continue;
            }
            if events.len() >= EVENT_CAPACITY {
                // Cannot happen: fewer live events are held than a
                // device takes in a block. If it ever does, the
                // event may have been the end of a note.
                *hand.flush = Flush::Everything;
                continue;
            }
            // A key the keyboard does not hold here has nothing to
            // let go of: it went down before this track was one it
            // plays, or the note was ended for it already.
            if let (true, EventKind::NoteOff { pitch }) = (routed, event.kind)
                && track.state.routed_held & (1 << pitch.min(127)) == 0
            {
                continue;
            }
            by_hand = true;
            let offset = event.offset - hand.block.start;
            // A key that switches, pressed by hand: what the
            // instrument is on is the hand's doing now.
            if let EventKind::NoteOn { pitch, .. } = event.kind
                && track.switch_keys & (1 << pitch.min(127)) != 0
            {
                track.state.latched = UNKNOWN;
            }
            track.state.played(event.kind, routed);
            events.push(Event::new(offset, event.kind));
            let keeps = matches!(event.kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. })
                || ControllerKind::of_event(event.kind).is_some();
            if let (true, Some(heard), Some(capture)) = (keeps, recorded, &mut *hand.capture) {
                let (beat, pass) = heard.at(offset, event.early);
                let _ = capture.push(Captured::Event { track: track.id, kind: event.kind, beat, pass });
            }
        }
    }
    // The song's controllers, after whatever the hand did in this block
    // (which makes a controller the hand's) and before the song's notes,
    // so that a note begins at the level meant for it.
    let moved = say_controllers(track, common, room);
    if let (true, Some(heard), Some(capture)) = (hand.standing_due, recorded, &mut *hand.capture) {
        // Where the hand has left the controllers is how the take
        // begins: an instrument that was given its expression before the
        // take needs it to play the take back.
        for (index, value) in track.state.live.iter().enumerate().filter(|(_, value)| !value.is_nan()) {
            let Some(kind) = ControllerKind::from_index(index) else { continue };
            if kind.rest().is_none_or(|rest| rest as f32 != *value) {
                let report = Captured::Standing { track: track.id, kind, value: *value, pass: heard.pass };
                let _ = capture.push(report);
            }
        }
    }
    room.unordered = by_hand || moved;
}

/// A track's block begins to sound: from what was fed to its bus, if it
/// is a group or a return (every track that feeds it has been added to
/// it), with its own notes after what [`prelude`] left it and its audio
/// ([`gather`]).
pub(crate) fn sound(track: &mut TrackNode, common: &Common, buses: &[Bus], room: &mut Room) {
    let frames = common.controls.frames;
    let (left, right) = (&mut room.left[..frames], &mut room.right[..frames]);
    match track.input_bus.and_then(|bus| buses.get(bus)) {
        Some(bus) => {
            left.copy_from_slice(&bus.left[..frames]);
            right.copy_from_slice(&bus.right[..frames]);
        }
        None => {
            left.fill(0.0);
            right.fill(0.0);
        }
    }
    let unordered = room.unordered;
    gather(track, common, room, unordered);
    hear_input(track, common, room);
}

/// What a track that records an input of the audio device makes of it in
/// the block: nothing of what its clips play is heard while a take is
/// recorded over them, and the input is let through to the track's
/// devices where it is monitored. Both come and go over a few
/// milliseconds. A track that records no input is not touched.
fn hear_input(track: &mut TrackNode, common: &Common, room: &mut Room) {
    let Some(tap) = track.input else { return };
    let frames = common.controls.frames;
    let Room { left, right, shares, .. } = room;
    let (left, right, shares) = (&mut left[..frames], &mut right[..frames], &mut shares[..frames]);
    match track.state.under.fill(!common.over.contains(track.id), common.declick, shares) {
        // Wide open: what the track has under the take is as it was.
        Some(share) if share >= 1.0 => {}
        Some(share) => {
            scale(left, share);
            scale(right, share);
        }
        None => {
            scale_by(left, shares);
            scale_by(right, shares);
        }
    }
    let heard = common.input.filter(|_| tap.heard);
    let share = track.state.monitor.fill(heard.is_some(), common.declick, shares);
    // (The gate is worked whether or not there is an input, so that one
    // which comes back finds it shut and comes up from nothing.)
    let Some(input) = common.input else { return };
    let from = |channel: usize| Some(input.channel(channel)).filter(|heard| heard.len() >= frames);
    let Some(one) = from(tap.left) else { return };
    // One channel alone is in the middle; a pair is left and right.
    let other = tap.right.map_or(Some(one), from).unwrap_or(&one[..0]);
    match share {
        // Shut: nothing of the input is heard.
        Some(share) if share <= 0.0 => {}
        Some(share) => {
            add_scaled(&one[..frames], left, share);
            add_scaled(&other[..frames.min(other.len())], right, share);
        }
        None => {
            add_scaled_by(&one[..frames], left, shares);
            add_scaled_by(&other[..frames.min(other.len())], right, shares);
        }
    }
}

/// What is left of a track's block once its devices have all run: it is
/// noticed whether they answered their notes, `listened` takes what the
/// devices that listen to this track hear of it (as it leaves its own
/// devices, whatever its fader and its mute say), and it is brought to
/// where its fader has it.
pub(crate) fn finish(track: &mut TrackNode, common: &Common, room: &mut Room, listened: Option<&mut Bus>) {
    let frames = common.controls.frames;
    answer(track, common, room);
    if let Some(listened) = listened {
        listened.left[..frames].copy_from_slice(&room.left[..frames]);
        listened.right[..frames].copy_from_slice(&room.right[..frames]);
    }
    fade(track, common, room);
}

/// Puts on record what the track's block took to make, and returns it in
/// nanoseconds.
pub(crate) fn tally(track: &TrackNode, common: &Common, room: &Room) -> u64 {
    track.meter.cost(room.spent, room.ran, common.controls.frames);
    room.spent
}

/// Adds the song's controllers to what a track is told in the block, and
/// returns whether there were any.
pub(crate) fn say_controllers(track: &mut TrackNode, common: &Common, room: &mut Room) -> bool {
    let before = room.events.len();
    collect_controllers(track, &common.controls, &mut room.events);
    room.events.len() > before
}

/// What a track's devices are given in the block, into `room`: its notes
/// after what it has been told so far, all of it in order of time, and
/// its audio; and their parameters are put where their envelopes have
/// them. The second part of [`sound`].
pub(crate) fn gather(track: &mut TrackNode, common: &Common, room: &mut Room, unordered: bool) {
    let ControlBlock { playing, start_beat, beats_per_frame, frames, .. } = common.controls;
    let Room { left, right, events, sorted, .. } = room;
    let (left, right) = (&mut left[..frames], &mut right[..frames]);
    if common.controls.chase | std::mem::take(&mut track.state.rechoose) {
        chase_switches(track, &common.controls, events);
    }
    let before_notes = events.len();
    if playing {
        collect_notes(track, start_beat, common.notes, beats_per_frame, frames, events);
        let block = Block {
            start_beat,
            beats_per_frame,
            anchor_frames: common.origin_frames,
            elapsed: common.since_origin,
            sample_rate: common.sample_rate,
            tempo: common.tempo_map,
        };
        render_audio(track, &block, left, right);
    }
    if unordered {
        // What is played by hand comes at any offset, and the
        // song's controllers and notes were added after it: devices
        // want one list in order. Ties keep the order they were
        // added in.
        sort_by_offset(&mut events[..before_notes]);
        merge_by_offset(events, before_notes, sorted);
    }
    move_params(&mut track.chain, common.param_beat);
}

/// Runs some of a track's devices over what `room` holds: those at the
/// places `devices` of its chain. The whole chain in one go, or a first
/// part and then the rest, comes to the same. A render that keeps what
/// goes into the track's effects hears it where it goes by: ahead of the
/// first effect, in whichever part that one is run.
pub(crate) fn through(
    track: &mut TrackNode,
    common: &Common,
    room: &mut Room,
    keys: &[Bus],
    devices: std::ops::Range<usize>,
) {
    let frames = common.controls.frames;
    let Room { left, right, events, .. } = room;
    let (left, right) = (&mut left[..frames], &mut right[..frames]);
    let all = &mut track.chain.devices;
    let (from, to) = (devices.start.min(all.len()), devices.end.min(all.len()));
    let count = all.len();
    let part = &mut all[from..to.max(from)];
    match &mut track.source {
        Some((sources, tap)) if from <= *sources && *sources < to => {
            let (source, effects) = part.split_at_mut(*sources - from);
            run_devices(source, common.info, events, left, right, keys);
            tap.push(left, right);
            run_devices(effects, common.info, events, left, right, keys);
        }
        // Nothing comes after the instrument: it is heard as the last
        // device has run.
        Some((sources, tap)) if *sources >= count && to == count => {
            run_devices(part, common.info, events, left, right, keys);
            tap.push(left, right);
        }
        _ => run_devices(part, common.info, events, left, right, keys),
    }
}

/// Notices whether a track's devices have answered the notes they were
/// given, once they have all run: the first part of [`finish`].
pub(crate) fn answer(track: &mut TrackNode, common: &Common, room: &mut Room) {
    let frames = common.controls.frames;
    listen_for_an_answer(track, &room.left[..frames], &room.right[..frames]);
}

/// Brings what a track has played to where its fader, its pan, and mute
/// and solo have it, in place, so that its output and every send carry the
/// same signal; and lets its meter and whatever listens to it hear that.
pub(crate) fn fade(track: &mut TrackNode, common: &Common, room: &mut Room) {
    let frames = common.controls.frames;
    let (span, declick) = (&common.span, common.declick);
    let Room { left, right, gain_left, gain_right, shares, .. } = room;
    let (left, right) = (&mut left[..frames], &mut right[..frames]);
    // Where the fader and the pan have the two sides at a position
    // of the song: where their envelopes have them, or where they
    // were set by hand.
    let (volume, pan) = (&track.volume_automation, &track.pan_automation);
    let (volume_db, pan_set) = (track.volume_db, track.pan);
    let sides = |beat: f64| {
        let level = volume.as_ref().map_or(volume_db, |envelope| envelope.value_at(beat) as f32);
        if level <= MIN_VOLUME_DB {
            return [0.0, 0.0];
        }
        let gain = db_to_gain(level);
        let (pan_left, pan_right) = pan_gains(pan.as_ref().map_or(pan_set, |envelope| envelope.value_at(beat) as f32));
        [gain * pan_left, gain * pan_right]
    };
    let moving = volume.is_some() || pan.is_some();
    let shares = &mut shares[..frames];
    let mut gains = [&mut gain_left[..frames], &mut gain_right[..frames]];
    let state = &mut track.state;
    let flat = state.fader.fill(span, moving, declick, frames, &mut gains, sides);
    // A track that passes no sound, on to its output or into a
    // send, is shut: mute and solo work a gate on what the fader
    // leaves, so that neither makes a step when the other moves.
    let flat = gated(flat, state.passes.fill(track.passes, declick, shares), shares, &mut gains);
    let (peak_left, peak_right) = match flat {
        Some([gain_left, gain_right]) => (scale(left, gain_left), scale(right, gain_right)),
        None => (scale_by(left, gains[0]), scale_by(right, gains[1])),
    };
    track.meter.record(peak_left, peak_right);
    if let Some(tap) = &mut track.tap {
        tap.push(left, right);
    }
}

/// Adds what a track has come to, in `room`, to where it goes: its output
/// (its group's bus, or the mix) and the bus of every return it sends to.
/// What goes on to the output, and what goes to each return, are opened
/// and closed by mute and solo apart from each other: a track that feeds
/// a soloed return is not heard itself.
pub(crate) fn mix(
    track: &mut TrackNode,
    common: &Common,
    room: &mut Room,
    buses: &mut [Bus],
    mix_left: &mut [f32],
    mix_right: &mut [f32],
) {
    let frames = common.controls.frames;
    let (span, declick) = (&common.span, common.declick);
    let Room { left, right, gain_left, shares, .. } = room;
    let (left, right) = (&left[..frames], &right[..frames]);
    let shares = &mut shares[..frames];
    let (out_left, out_right) = match track.output_bus.and_then(|bus| buses.get_mut(bus)) {
        Some(bus) => (&mut bus.left[..frames], &mut bus.right[..frames]),
        None => (&mut *mix_left, &mut *mix_right),
    };
    match track.state.out.fill(track.to_output, declick, shares) {
        Some(0.0) => {}
        Some(share) => {
            add_scaled(left, out_left, share);
            add_scaled(right, out_right, share);
        }
        None => {
            add_scaled_by(left, out_left, shares);
            add_scaled_by(right, out_right, shares);
        }
    }
    for send in &mut track.sends {
        // What the send carries: its level at a position of the
        // song, where its envelope has it or where it was set by
        // hand, times how far mute and solo leave it open.
        let (automation, set) = (&send.automation, send.gain);
        let level = |beat: f64| match automation {
            Some(envelope) => {
                let level = envelope.value_at(beat) as f32;
                [if level > MIN_VOLUME_DB { db_to_gain(level) } else { 0.0 }]
            }
            None => [set],
        };
        let mut gains = [&mut gain_left[..frames]];
        let flat = send.glide.fill(span, automation.is_some(), declick, frames, &mut gains, level);
        let flat = gated(flat, send.gate.fill(send.open, declick, shares), shares, &mut gains);
        let Some(bus) = buses.get_mut(send.bus) else { continue };
        match flat {
            Some([0.0]) => {}
            Some([gain]) => {
                add_scaled(left, &mut bus.left[..frames], gain);
                add_scaled(right, &mut bus.right[..frames], gain);
            }
            None => {
                add_scaled_by(left, &mut bus.left[..frames], gains[0]);
                add_scaled_by(right, &mut bus.right[..frames], gains[0]);
            }
        }
    }
}

/// What it takes to say where the song was *heard* to be when something
/// was played: the take is written there, not where the engine had got to.
///
/// The engine runs ahead of the speakers by the output latency, and an
/// event reaches its block a moment after it was played. Both are known to
/// the frame, so both are taken off again. If that reaches back past a
/// jump of the song position (the loop coming round, a seek, the start of
/// playback), the answer is on the far side of the jump: a note played as
/// the loop was heard to end belongs at the loop's end, in the pass before.
struct Heard {
    tracks: TrackSet,
    pass: u32,
    start_beat: f64,
    beats_per_frame: f64,
    /// Frames of count-in left at the start of the block.
    count_in: f64,
    latency: f64,
    /// Frames played since the last jump, at the start of the block.
    continuity: f64,
    before_jump: f64,
    jump_was_wrap: bool,
}

impl Heard {
    /// The heard position, and the loop pass it belongs to, of an event
    /// placed `offset` frames into the block and played `early` frames
    /// before that.
    fn at(&self, offset: u32, early: f64) -> (f64, u32) {
        let offset = offset as f64;
        let back = early + self.latency;
        if self.count_in > 0.0 {
            // The song stands still at the take's start, which is this many
            // frames away.
            let until_start = (self.count_in - offset).max(0.0);
            return (self.start_beat - (until_start + back) * self.beats_per_frame, self.pass);
        }
        let since_jump = self.continuity + offset;
        if back <= since_jump {
            (self.start_beat + (offset - back) * self.beats_per_frame, self.pass)
        } else {
            let pass = if self.jump_was_wrap { self.pass.saturating_sub(1) } else { self.pass };
            (self.before_jump - (back - since_jump) * self.beats_per_frame, pass)
        }
    }
}

/// Releases what `flush` asks of a track at the start of a block.
fn release_notes(track: &mut TrackNode, flush: Flush, events: &mut Vec<Event>) {
    let state = &mut track.state;
    match flush {
        Flush::Nothing => {}
        // The song's notes go, each on the channel it is on; the keys held
        // by hand, which are on the first, stay down.
        Flush::Song if state.live_held != 0 => {
            let held = state.live_held;
            state.sounding.each(|channel, pitch| {
                if !(channel.is_first() && held & (1 << pitch) != 0) {
                    events.push(Event::on(channel, 0, EventKind::NoteOff { pitch }));
                }
            });
            state.sounding.clear();
        }
        // Every note of every channel.
        Flush::Song | Flush::Everything => {
            events.push(Event::new(0, EventKind::AllNotesOff));
            state.sounding.clear();
            state.forget_live();
        }
    }
}

/// Lets go of what a keyboard was holding on a track: every key, the
/// sustain pedal and the pitch wheel. `released` is told each event.
fn release_routed(track: &mut TrackNode, events: &mut Vec<Event>, mut released: impl FnMut(EventKind)) {
    let state = &mut track.state;
    let mut push = |kind: EventKind| {
        events.push(Event::new(0, kind));
        released(kind);
    };
    let held = std::mem::take(&mut state.routed_held);
    state.live_held &= !held;
    for pitch in (0..128u8).filter(|pitch| held & (1 << pitch) != 0) {
        push(EventKind::NoteOff { pitch });
    }
    state.release_sprung(|kind| push(kind.event(0.0)));
}

/// Puts `events` in order of offset, given that the part before `middle`
/// and the part from it on each already are. Events at one offset keep
/// their order, those of the first part before those of the second. One
/// pass, however many there are: `room` is somewhere to lay them out.
fn merge_by_offset(events: &mut [Event], middle: usize, room: &mut Vec<Event>) {
    if middle == 0 || middle >= events.len() || events[middle - 1].offset <= events[middle].offset {
        return;
    }
    room.clear();
    let (mut first, mut second) = (0, middle);
    while first < middle && second < events.len() {
        if events[second].offset < events[first].offset {
            room.push(events[second]);
            second += 1;
        } else {
            room.push(events[first]);
            first += 1;
        }
    }
    room.extend_from_slice(&events[first..middle]);
    room.extend_from_slice(&events[second..]);
    events.copy_from_slice(room);
}

/// Where a block lies for the controllers the song moves.
#[derive(Copy, Clone)]
struct ControlBlock {
    playing: bool,
    /// The song has just started or jumped: every controller is said at
    /// the block's first frame.
    chase: bool,
    /// The song has just started playing or jumped while playing: it takes
    /// the controllers it has curves for back from the hand.
    take_back: bool,
    start_beat: f64,
    end_beat: f64,
    anchor_beat: f64,
    /// Frames played since the anchor, at the block's first frame.
    elapsed: u64,
    beats_per_frame: f64,
    frames: usize,
}

/// Appends the events that put each controller of `track`'s lanes where the
/// song has it over the block, in order of offset.
///
/// While the song plays, a lane is looked at every [`CONTROL_STEP`] frames
/// and a controller is told whenever it has moved since it was last told.
/// While the song stands still it is looked at once a block, where the
/// song stands: what stays put (expression, the mod wheel) is where the
/// song has it there, so that a note played by hand sounds as it would in
/// the song, and what springs back (the pedals, the pitch wheel) is at
/// rest, so that nothing is held or bent while nothing plays.
///
/// A controller the hand has moved is the hand's and is left alone, until
/// the song takes it back as it next starts or jumps.
fn collect_controllers(track: &mut TrackNode, block: &ControlBlock, events: &mut Vec<Event>) {
    let TrackNode { state, controllers, .. } = track;
    let room = |events: &Vec<Event>| events.len() < CONTROL_LIMIT;
    if std::mem::take(&mut state.resend) {
        // The instrument knows nothing: what the song said must be said
        // again, and first of all what the hand has set.
        state.sent.forget();
        for (index, value) in state.live.iter().enumerate().filter(|(_, value)| !value.is_nan()) {
            if let (Some(kind), true) = (ControllerKind::from_index(index), room(events)) {
                events.push(Event::new(0, kind.event(*value)));
            }
        }
    }
    if std::mem::take(&mut state.lanes_changed) {
        // A controller the song moved and has no curve for any more, on
        // any channel: what springs back goes to rest, so that no pedal
        // stays down for a curve that was deleted; the rest stay where
        // they were left.
        for channel in Channel::all() {
            for index in 0..CONTROLLER_KINDS {
                let sent = state.sent.get(channel, index);
                let has_lane = |lane: &ControllerLane| lane.channel == channel && lane.kind.index() == index;
                if sent.is_nan() || controllers.iter().any(has_lane) {
                    continue;
                }
                state.sent.set(channel, index, f32::NAN);
                let rest = ControllerKind::from_index(index).and_then(|kind| Some((kind, kind.rest()? as f32)));
                // (The hand's are on the first channel.)
                let hands = channel.is_first() && !state.live[index].is_nan();
                if let Some((kind, rest)) = rest
                    && sent != rest
                    && !hands
                    && room(events)
                {
                    events.push(Event::on(channel, 0, kind.event(rest)));
                }
            }
        }
    }
    if controllers.is_empty() {
        return;
    }
    // What the hand has moved is on the first channel: the song's curves
    // on the others are never anyone's but the song's.
    if block.take_back {
        for lane in controllers.iter().filter(|lane| lane.channel.is_first()) {
            let index = lane.kind.index();
            if !state.live[index].is_nan() {
                state.live[index] = f32::NAN;
                state.sent.set(Channel::FIRST, index, f32::NAN);
            }
        }
    }

    let mut tell = |offset: usize, beat: f64| {
        for lane in controllers.iter() {
            let index = lane.kind.index();
            if lane.channel.is_first() && !state.live[index].is_nan() {
                continue;
            }
            let value = match lane.kind.rest() {
                Some(rest) if !block.playing => rest as f32,
                _ => lane.value_at(beat) as f32,
            };
            if value != state.sent.get(lane.channel, index) && room(events) {
                state.sent.set(lane.channel, index, value);
                events.push(Event::on(lane.channel, offset as u32, lane.kind.event(value)));
            }
        }
    };
    if !block.playing || block.chase {
        tell(0, block.start_beat);
    }
    if block.playing {
        let mut offset = ((CONTROL_STEP - block.elapsed % CONTROL_STEP) % CONTROL_STEP) as usize;
        while offset < block.frames {
            // Computed as the start of a block is, so that a step that
            // falls on a block's first frame reads the lane where the
            // block does.
            let beat = block.anchor_beat + (block.elapsed + offset as u64) as f64 * block.beats_per_frame;
            if beat >= block.end_beat {
                break;
            }
            tell(offset, beat);
            offset += CONTROL_STEP as usize;
        }
    }
}

/// How long a key that switches is held, in beats, when the song presses
/// it for a start: what the writing tools hold one for
/// (`trill_theory::articulation::SWITCH_HOLD`), so that no two are down
/// together.
const SWITCH_HOLD_BEATS: f64 = 0.01;
/// How hard the key that chooses the default way of playing is pressed.
const SWITCH_VELOCITY: f32 = 100.0 / 127.0;

/// Presses the keys that choose how notes are played where the song has
/// just started or jumped to: what the part pressed before that place
/// and has not undone (see [`crate::graph::chosen_at`]). Starting anywhere
/// is then starting where the song would have the instrument.
///
/// The key that stays is pressed only if the instrument is not on it
/// already, as far as the song knows ([`TrackState::latched`]): a song
/// that was put somewhere while it stood still pressed it then, well
/// ahead of the first note, and a loop that comes round to the way of
/// playing it ends in presses nothing. (Some instruments take a key only
/// if it comes before its note: a render therefore begins to play a
/// moment ahead of its start, which a song that plays cannot.) Where the
/// part has chosen nothing yet and the instrument was left on something,
/// the key the track calls its default is pressed, if it names one.
///
/// A key the part is still holding there is held to where the part lets
/// go of it, whatever the instrument is on; any other for a moment. While
/// the song stands still a key is pressed and let go in the one block,
/// and one that is for the next note only is left to the start.
fn chase_switches(track: &mut TrackNode, block: &ControlBlock, events: &mut Vec<Event>) {
    let TrackNode { state, switches, unsaid_key, .. } = track;
    if switches.is_empty() {
        return;
    }
    let [stays, once] = crate::graph::chosen_at(switches, block.start_beat);
    let held_until = |switch: &Switch| switch.end.max(block.start_beat + SWITCH_HOLD_BEATS);
    // (pitch, velocity, the beat it is let go at)
    let mut presses: [Option<(u8, f32, f64)>; 2] = [None, None];
    match stays {
        Some(switch) => {
            let still_held = switch.end > block.start_beat + SWITCH_HOLD_BEATS;
            if state.latched != switch.pitch || (block.playing && still_held) {
                presses[0] = Some((switch.pitch, f32::from(switch.velocity.clamp(1, 127)) / 127.0, held_until(switch)));
            }
        }
        None => {
            if let Some(key) = unsaid_key.filter(|key| state.latched != AS_MADE && state.latched != *key) {
                presses[0] = Some((key, SWITCH_VELOCITY, block.start_beat + SWITCH_HOLD_BEATS));
            }
        }
    }
    if let Some((pitch, ..)) = presses[0] {
        state.latched = pitch;
    }
    if let (true, Some(switch)) = (block.playing, once) {
        presses[1] = Some((switch.pitch, f32::from(switch.velocity.clamp(1, 127)) / 127.0, held_until(switch)));
    }
    for (pitch, velocity, until) in presses.into_iter().flatten() {
        if events.len() + 3 > NOTE_ON_LIMIT {
            break;
        }
        if !block.playing {
            let last = block.frames.saturating_sub(1) as u32;
            events.push(Event::new(0, EventKind::NoteOn { pitch, velocity }));
            events.push(Event::new(last, EventKind::NoteOff { pitch }));
            continue;
        }
        // (On the first channel, where the keys that switch are.)
        if state.sounding.begin(Channel::FIRST, pitch, until) {
            events.push(Event::new(0, EventKind::NoteOff { pitch }));
        }
        events.push(Event::new(0, EventKind::NoteOn { pitch, velocity }));
    }
}

/// How often, in frames, an instrument that has notes to play and has
/// made no sound yet is told its controllers again.
const TELL_AGAIN: u32 = 16_384;

/// Keeps count of the notes a track is given without anything coming out
/// of its devices: see [`Meter::unanswered_frames`](crate::graph::Meter).
/// `left` and `right` are what the chain put out, ahead of the fader.
///
/// While that goes on, the track's controllers are said again a few times
/// a second. A plugin that has only just been made may not be listening
/// yet: a modelled instrument that was sent its expression while it was
/// still waking up never heard it, and a level that does not move is not
/// sent a second time. Without this it would stay silent until the song
/// was stopped and started again.
fn listen_for_an_answer(track: &mut TrackNode, left: &[f32], right: &[f32]) {
    let state = &mut track.state;
    let sounding = !state.sounding.is_empty() || state.live_held != 0;
    if sounding && !state.had_notes {
        // A phrase begins: what was heard before is no answer to it.
        state.answered = false;
    }
    state.had_notes = sounding;
    if (sounding && !state.answered) || state.unanswered > 0 {
        let heard = left.iter().chain(right).any(|sample| sample.abs() > AUDIBLE);
        if heard {
            state.answered = true;
            state.unanswered = 0;
        } else if sounding && !state.answered {
            let before = state.unanswered;
            state.unanswered = before.saturating_add(left.len() as u32);
            if state.unanswered / TELL_AGAIN != before / TELL_AGAIN {
                // And the key that is chosen, which it may have missed too.
                (state.latched, state.rechoose) = (UNKNOWN, true);
                state.resend = true;
            }
        }
    }
    if state.unanswered != state.unanswered_told {
        state.unanswered_told = state.unanswered;
        track.meter.set_unanswered(state.unanswered);
    }
}

/// Stable insertion sort by offset: the lists are short and nearly sorted.
fn sort_by_offset(events: &mut [Event]) {
    for i in 1..events.len() {
        let mut j = i;
        while j > 0 && events[j - 1].offset > events[j].offset {
            events.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Applies one gain in place and returns the resulting peak.
#[inline]
fn scale(samples: &mut [f32], gain: f32) -> f32 {
    let mut peak = 0.0f32;
    for sample in samples.iter_mut() {
        *sample *= gain;
        peak = peak.max(sample.abs());
    }
    peak
}

/// Applies a gain for every frame in place and returns the resulting peak.
#[inline]
fn scale_by(samples: &mut [f32], gains: &[f32]) -> f32 {
    let mut peak = 0.0f32;
    for (sample, gain) in samples.iter_mut().zip(gains) {
        *sample *= gain;
        peak = peak.max(sample.abs());
    }
    peak
}

/// Adds `source`, scaled, into `destination`.
#[inline]
fn add_scaled(source: &[f32], destination: &mut [f32], gain: f32) {
    for (out, sample) in destination.iter_mut().zip(source) {
        *out += sample * gain;
    }
}

/// Adds `source` into `destination`, each frame at its own gain.
#[inline]
fn add_scaled_by(source: &[f32], destination: &mut [f32], gains: &[f32]) {
    for ((out, sample), gain) in destination.iter_mut().zip(source).zip(gains) {
        *out += sample * gain;
    }
}

/// Sets the parameters of a chain's devices to where their automation has
/// them at `beat`.
fn move_params(chain: &mut Chain, beat: f64) {
    let Chain { devices, automation } = chain;
    for lane in automation.iter_mut() {
        let value = lane.envelope.value_at(beat);
        if value != lane.last {
            lane.last = value;
            if let Some(processor) = devices.get_mut(lane.slot).and_then(|slot| slot.processor.as_mut()) {
                processor.set_param(lane.param, value);
            }
        }
    }
}

/// Runs devices one after the other over a block. A device that listens to
/// another track is handed what that track put out in this block, which
/// `keys` holds: the track was played before this one.
fn run_devices(
    devices: &mut [DeviceSlot],
    info: &ProcessInfo,
    events: &[Event],
    left: &mut [f32],
    right: &mut [f32],
    keys: &[Bus],
) {
    for slot in devices.iter_mut() {
        if slot.enabled
            && let Some(processor) = slot.processor.as_mut()
        {
            match slot.key.and_then(|key| keys.get(key)) {
                Some(key) => {
                    let key = KeyInput { left: &key.left[..left.len()], right: &key.right[..left.len()] };
                    processor.process_keyed(info, events, left, right, key);
                }
                None => processor.process(info, events, left, right),
            }
        }
    }
}

/// Appends the note events of `track` that fall in `[from, until)`, ordered
/// by sample offset, in a block that starts at `start_beat`.
fn collect_notes(
    track: &mut TrackNode,
    start_beat: f64,
    (from, until): (f64, f64),
    beats_per_frame: f64,
    frames: usize,
    events: &mut Vec<Event>,
) {
    let first_new = events.len();
    let last = frames.saturating_sub(1) as f64;
    let offset_of = |beat: f64| ((beat - start_beat) / beats_per_frame).round().clamp(0.0, last) as u32;
    let stay_keys = track.stay_keys;
    let state = &mut track.state;

    // Notes from earlier blocks that end in this one: each key comes up on
    // the channel it went down on.
    let release_ended = |state: &mut TrackState, events: &mut Vec<Event>| {
        state.sounding.end_before(until, |channel, pitch, end| {
            events.push(Event::on(channel, offset_of(end), EventKind::NoteOff { pitch }));
        });
    };
    release_ended(state, events);

    // Notes that start in this block. A key that is still sounding on the
    // note's channel is released first, so repeated notes retrigger
    // cleanly; the same key on another channel is another note and goes on.
    let first = track.notes.partition_point(|note| note.start < from);
    let mut started = false;
    for note in &track.notes[first..] {
        if note.start >= until || events.len() >= NOTE_ON_LIMIT {
            break;
        }
        let offset = offset_of(note.start);
        let channel = note.channel;
        if state.sounding.begin(channel, note.pitch, note.end) {
            events.push(Event::on(channel, offset, EventKind::NoteOff { pitch: note.pitch }));
        }
        let velocity = note.velocity as f32 / 127.0;
        events.push(Event::on(channel, offset, EventKind::NoteOn { pitch: note.pitch, velocity }));
        // A key that stays chosen is what the instrument is on from here.
        if channel.is_first() && stay_keys & (1 << note.pitch.min(127)) != 0 {
            state.latched = note.pitch;
        }
        started = true;
    }

    // Notes short enough to start and end inside this block.
    if started {
        release_ended(state, events);
    }

    // Stable insertion sort by offset. The three passes above were appended
    // in the order they must keep when offsets tie.
    let new = &mut events[first_new..];
    for i in 1..new.len() {
        let mut j = i;
        while j > 0 && new[j - 1].offset > new[j].offset {
            new.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Where a block lies on the timeline.
struct Block<'a> {
    start_beat: f64,
    beats_per_frame: f64,
    /// Where the song was last put, in output frames from beat 0, and the
    /// frames played since. Their sum is the block's first frame. They are
    /// kept apart because the first has a fraction and the count does not:
    /// a position rounded from the first alone rounds the same way in
    /// every block, where one rounded from the sum could flip between
    /// blocks.
    anchor_frames: f64,
    elapsed: u64,
    sample_rate: f64,
    /// How the song's beats become frames: a clip's length and its fades
    /// are counted in frames, through whatever the tempo does under it.
    tempo: &'a TempoMap,
}

/// Reads a region's audio for the frames of one block.
enum Reader<'a> {
    /// One frame of audio per output frame: frames are copied.
    Copy { left: &'a [f32], right: &'a [f32], first: i64 },
    /// Any other speed: linear interpolation.
    Interpolate { left: &'a [f32], right: &'a [f32], first: f64, step: f64 },
}

impl<'a> Reader<'a> {
    fn new(audio: &'a RegionAudio, block: &Block) -> Self {
        let frames = audio.data.frames();
        let (left, right) = (&audio.data.left[..frames], &audio.data.right[..frames]);
        if audio.step == 1.0 {
            // Snapping to whole frames moves the clip by at most half a
            // sample and plays the file exactly as it is stored. The sums
            // saturate: a clip with an absurd offset is silent, not a panic.
            let first = ((audio.origin + block.anchor_frames).round() as i64).saturating_add(block.elapsed as i64);
            Self::Copy { left, right, first }
        } else {
            let first = audio.origin + (block.anchor_frames + block.elapsed as f64) * audio.step;
            Self::Interpolate { left, right, first, step: audio.step }
        }
    }

    /// The audio for frame `i` of the block; silence outside the file.
    #[inline]
    fn at(&self, i: usize) -> (f32, f32) {
        match *self {
            Self::Copy { left, right, first } => match usize::try_from(first.saturating_add(i as i64)) {
                Ok(index) if index < left.len() => (left[index], right[index]),
                _ => (0.0, 0.0),
            },
            Self::Interpolate { left, right, first, step } => {
                let position = first + i as f64 * step;
                // Also catches positions that are not numbers at all.
                if !(0.0..left.len() as f64 - 1.0).contains(&position) {
                    return (0.0, 0.0);
                }
                let index = position as usize;
                let fraction = (position - index as f64) as f32;
                (
                    left[index] + (left[index + 1] - left[index]) * fraction,
                    right[index] + (right[index + 1] - right[index]) * fraction,
                )
            }
        }
    }
}

/// Mixes the audio clips of `track` into the block.
fn render_audio(track: &mut TrackNode, block: &Block, left: &mut [f32], right: &mut [f32]) {
    let frames = left.len();
    let end_beat = block.start_beat + frames as f64 * block.beats_per_frame;
    let guard = CLIP_FADE_SECONDS * block.sample_rate;
    let hand_over = (HAND_OVER_SECONDS * block.sample_rate) as u32;

    for region in &mut track.audio {
        if region.end <= block.start_beat || region.start >= end_beat {
            continue;
        }
        // Output frames between the clip start and this block (negative if
        // the clip starts inside the block).
        let rate = block.sample_rate;
        let elapsed = block.tempo.frames_between(region.clip_start, block.start_beat, rate);
        let length = block.tempo.frames_between(region.clip_start, region.clip_end, rate);
        // The clip's own fades, never shorter than the click guard except
        // where the clip joins a neighbour and there is no edge to guard.
        let fade_in = block.tempo.frames_over(region.clip_start, region.fade_in, rate);
        let fade_in = fade_in.max(if region.joins_previous { 0.0 } else { guard });
        let fade_out = block.tempo.frames_over(region.clip_end - region.fade_out, region.fade_out, rate);
        let fade_out = fade_out.max(if region.joins_next { 0.0 } else { guard });
        // A piece of a clip is faded over the click guard where it meets
        // another piece: counted, like the clip's fades, in frames from
        // the piece's own edges.
        let crossed = region.cross_before || region.cross_after;
        let seams = (region.seam_before || region.seam_after || crossed).then(|| {
            let since = block.tempo.frames_between(region.start, block.start_beat, rate);
            (since, block.tempo.frames_between(region.start, region.end, rate))
        });
        let cross = CROSS_FADE_SECONDS * block.sample_rate;
        // An edge that is faded across another clip has no fade of its
        // own: the region reaches past the clip's edge, and the fade
        // across the seam is all there is to it.
        let fade_in = if region.cross_before { 0.0 } else { fade_in };
        let fade_out = if region.cross_after { 0.0 } else { fade_out };
        // Both edges are worked out the same way from a timeline position,
        // so that where one clip ends on the beat another starts on, the
        // frame at the seam belongs to exactly one of them.
        let frame_of = |beat: f64| ((beat - block.start_beat) / block.beats_per_frame).ceil().max(0.0) as usize;
        let first = frame_of(region.start);
        let last = frame_of(region.end).min(frames);

        let audio = Reader::new(&region.audio, block);
        let replaced = region.replaces.as_ref().map(|audio| Reader::new(audio, block));
        for i in first..last {
            let t = elapsed + i as f64;
            let (mut sample_left, mut sample_right) = audio.at(i);
            if let Some(replaced) = &replaced
                && region.blended < hand_over
            {
                let (old_left, old_right) = replaced.at(i);
                let mix = region.blended as f32 / hand_over as f32;
                sample_left = old_left + (sample_left - old_left) * mix;
                sample_right = old_right + (sample_right - old_right) * mix;
                region.blended += 1;
            }
            let rise = if fade_in > 0.0 { t / fade_in } else { 1.0 };
            let fall = if fade_out > 0.0 { (length - t) / fade_out } else { 1.0 };
            let mut gain = region.gain * rise.min(fall).clamp(0.0, 1.0) as f32;
            if let Some((since, piece)) = seams {
                let t = since + i as f64;
                let rise = if region.seam_before { t / guard } else { 1.0 };
                let fall = if region.seam_after { (piece - t) / guard } else { 1.0 };
                gain *= rise.min(fall).clamp(0.0, 1.0) as f32;
                // Two recordings across a seam: one comes as the sine of
                // what the other goes as the cosine of, which keeps what
                // they add up to as loud as either.
                if region.cross_before {
                    gain *= ((t / cross).clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).sin() as f32;
                }
                if region.cross_after {
                    gain *= (((piece - t) / cross).clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).sin() as f32;
                }
            }
            left[i] += sample_left * gain;
            right[i] += sample_right * gain;
        }
    }
}

/// Flushes denormal floats to zero while alive. Decaying filters and reverb
/// tails otherwise drift into subnormals, which are orders of magnitude
/// slower to compute and show up as CPU spikes in silence. The setting is
/// a thread's own: every thread that makes sound has to have it, or the
/// same devices give other samples on it.
pub(crate) struct DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    saved: u32,
}

impl DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn new() -> Self {
        let mut saved = 0u32;
        // SAFETY: stmxcsr/ldmxcsr only read and write the SSE control
        // register through a valid pointer; the previous value is restored on
        // drop, so no Rust code outside this scope observes the change.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut saved, options(nostack, preserves_flags));
            // Flush-to-zero (bit 15) and denormals-are-zero (bit 6).
            let flushed = saved | 0x8040;
            std::arch::asm!("ldmxcsr [{}]", in(reg) &flushed, options(nostack, preserves_flags, readonly));
        }
        Self { saved }
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl Drop for DenormalGuard {
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: restores the control register saved in `new`.
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &self.saved, options(nostack, preserves_flags, readonly));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Meter, NoteEvent};
    use std::sync::Arc;

    fn track(notes: &[(f64, f64, u8)]) -> TrackNode {
        TrackNode {
            input: None,
            id: TrackId(1),
            notes: (notes.iter())
                .map(|&(start, end, pitch)| NoteEvent { start, end, pitch, velocity: 127, channel: Channel::FIRST })
                .collect(),
            switches: Vec::new(),
            switch_keys: 0,
            stay_keys: 0,
            unsaid_key: None,
            controllers: Vec::new(),
            fresh_devices: false,
            audio: Vec::new(),
            chain: Chain::default(),
            volume_db: 0.0,
            pan: 0.0,
            heard: true,
            passes: true,
            to_output: true,
            volume_automation: None,
            pan_automation: None,
            meter: Arc::new(Meter::default()),
            state: TrackState::default(),
            input_bus: None,
            output_bus: None,
            sends: Vec::new(),
            key_out: None,
            tap: None,
            source: None,
        }
    }

    /// Runs `blocks` blocks of 100 frames at 0.01 beats per frame and
    /// returns `(absolute frame, is note-on, pitch)` for every event.
    fn events_of(track: &mut TrackNode, blocks: usize) -> Vec<(usize, bool, u8)> {
        let mut out = Vec::new();
        let mut events = Vec::with_capacity(EVENT_CAPACITY);
        for block in 0..blocks {
            events.clear();
            let start = block as f64;
            collect_notes(track, start, (start, start + 1.0), 0.01, 100, &mut events);
            for event in &events {
                let frame = block * 100 + event.offset as usize;
                match event.kind {
                    EventKind::NoteOn { pitch, .. } => out.push((frame, true, pitch)),
                    EventKind::NoteOff { pitch } => out.push((frame, false, pitch)),
                    _ => {}
                }
            }
        }
        out
    }

    #[test]
    fn back_to_back_notes_release_before_they_retrigger() {
        // Two legato notes on one pitch, meeting mid-block and on a boundary.
        let mut track = track(&[(0.25, 0.5, 60), (0.5, 1.0, 60), (1.0, 1.5, 60)]);
        assert_eq!(
            events_of(&mut track, 3),
            [(25, true, 60), (50, false, 60), (50, true, 60), (100, false, 60), (100, true, 60), (150, false, 60)]
        );
        assert_eq!(track.state.sounding.count(), 0);
    }

    #[test]
    fn notes_shorter_than_a_block_start_and_stop_inside_it() {
        let mut track = track(&[(0.10, 0.15, 60), (0.12, 0.90, 64), (0.50, 0.5001, 67)]);
        assert_eq!(
            events_of(&mut track, 1),
            [(10, true, 60), (12, true, 64), (15, false, 60), (50, true, 67), (50, false, 67), (90, false, 64)]
        );
    }

    #[test]
    fn overlapping_notes_on_one_pitch_leave_nothing_hanging() {
        let mut track = track(&[(0.0, 2.5, 60), (1.2, 1.4, 60)]);
        assert_eq!(events_of(&mut track, 3), [(0, true, 60), (120, false, 60), (120, true, 60), (140, false, 60)]);
        assert_eq!(track.state.sounding.count(), 0);
    }
}
