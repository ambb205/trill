//! The render graph: an immutable-in-shape snapshot of the project that the
//! audio thread plays. The controller compiles a new graph after every edit
//! and hands it over; the audio thread swaps it in and carries the running
//! state (device instances, sounding notes, fader positions) across.

use crate::glide::{Gate, Glide};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use trill_core::controllers::{CONTROLLER_KINDS, ControllerKind, ControllerLane};
use trill_core::device::{DeviceProcessor, EventKind};
use trill_core::project::{AutomationLane, AutomationPoint, envelope_value};
use trill_core::{DeviceId, MeterMap, TempoMap, TimeSignature, TrackId, Travel};

/// Largest block the engine hands to a device.
pub const MAX_BLOCK: usize = 512;

/// A note with absolute timeline positions, after clip offsets and loops
/// have been resolved.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct NoteEvent {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    pub velocity: u8,
}

/// A key of a track's part that chooses how the notes after it are played
/// and sounds nothing itself (a keyswitch), as the track says it is one:
/// what a song that starts after it has to press again, or its notes play
/// in whatever was left chosen.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Switch {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    pub velocity: u8,
    /// It stays chosen until another that stays is pressed. One that does
    /// not is for the note after it.
    pub stays: bool,
    /// Where the note it is for begins, for one that does not stay:
    /// infinity if none follows.
    pub used_at: f64,
    /// The place in the track's list of the switch that stays chosen as
    /// this one is pressed, itself included; `u32::MAX` if none is.
    pub staying: u32,
}

/// What [`TrackState::latched`] is while the song has pressed no key that
/// stays: the instrument is on what it was made on.
pub(crate) const AS_MADE: u8 = 255;
/// What it is when the song cannot tell.
pub(crate) const UNKNOWN: u8 = 254;

/// The pitches of `switches` as a bit each: all of them, and those that
/// stay chosen.
pub(crate) fn switch_keys(switches: &[Switch]) -> (u128, u128) {
    let bit = |switch: &Switch| 1u128 << switch.pitch.min(127);
    let all = switches.iter().fold(0, |keys, switch| keys | bit(switch));
    let stay = switches.iter().filter(|switch| switch.stays).fold(0, |keys, switch| keys | bit(switch));
    (all, stay)
}

/// The switches among `notes`, which are sorted by start, as `profile`
/// names the keys: in the same order.
pub(crate) fn switches_of(notes: &[NoteEvent], profile: &trill_core::InstrumentProfile) -> Vec<Switch> {
    if !profile.articulations.iter().any(|way| way.key.is_some()) {
        return Vec::new();
    }
    let lasts = |pitch: u8| profile.articulations.iter().find(|way| way.key == Some(pitch)).map(|way| way.lasts);
    let mut switches: Vec<Switch> = Vec::new();
    let mut staying = u32::MAX;
    for (at, note) in notes.iter().enumerate() {
        let Some(lasts) = lasts(note.pitch) else { continue };
        let stays = lasts == trill_core::Lasts::Stays;
        if stays {
            staying = switches.len() as u32;
        }
        // The first note after it that is no switch.
        let used_at =
            notes[at..].iter().find(|later| lasts_of(profile, later.pitch).is_none()).map(|later| later.start);
        switches.push(Switch {
            start: note.start,
            end: note.end,
            pitch: note.pitch,
            velocity: note.velocity,
            stays,
            used_at: used_at.unwrap_or(f64::INFINITY),
            staying,
        });
    }
    switches
}

fn lasts_of(profile: &trill_core::InstrumentProfile, pitch: u8) -> Option<trill_core::Lasts> {
    profile.articulations.iter().find(|way| way.key == Some(pitch)).map(|way| way.lasts)
}

/// The switches that are chosen where a song is at `beat`, of `switches`
/// in their order: the one that stays, and after it one that was pressed
/// for a note which has not begun. What a song that starts there presses.
pub(crate) fn chosen_at(switches: &[Switch], beat: f64) -> [Option<&Switch>; 2] {
    let before = switches.partition_point(|switch| switch.start < beat);
    let Some(last) = before.checked_sub(1).map(|at| &switches[at]) else { return [None, None] };
    let stays = switches.get(last.staying as usize);
    let once = Some(last).filter(|switch| !switch.stays && switch.used_at >= beat);
    [stays, once]
}

/// Decoded audio, shared between clips that use the same file.
pub type AudioData = trill_core::audio::AudioBuffer;

/// Audio, and where in it each moment of the timeline falls.
///
/// The mapping is anchored to the timeline rather than to the clip, so that
/// two clips cut from one (a split) read exactly the same frames at the
/// same moments as the whole did.
#[derive(Clone)]
pub(crate) struct RegionAudio {
    pub data: Arc<AudioData>,
    /// The position in `data`, in frames, that beat 0 of the timeline
    /// corresponds to: output frame `n`, counted from beat 0 as the song's
    /// tempo makes it ([`TempoMap::frames_at`]), plays position
    /// `origin + n * step`. Usually negative or far past the start; only
    /// the part under the clip is ever read.
    pub origin: f64,
    /// Frames of `data` per output frame. Exactly 1 for everything but
    /// stand-ins and tempos beyond what the stretcher covers, and then
    /// frames are copied rather than interpolated.
    pub step: f64,
}

/// A stretch of a clip of audio that plays at one speed: the whole clip,
/// unless it follows a tempo that changes under it. Then it is one piece
/// for each tempo, and where two meet each is played a moment past the
/// seam and they are faded across it.
pub(crate) struct AudioRegion {
    pub start: f64,
    pub end: f64,
    /// The clip's own start and end, which its fades are counted from:
    /// `start` and `end`, for a clip that is one piece.
    pub clip_start: f64,
    pub clip_end: f64,
    /// Whether the piece begins, or ends, at a seam with another piece of
    /// its clip.
    pub seam_before: bool,
    pub seam_after: bool,
    /// The clip's fade lengths in beats. The processor never fades an edge
    /// faster than its own click guard, unless the edge joins a neighbour.
    pub fade_in: f64,
    pub fade_out: f64,
    /// The clip carries straight on from the clip before it, or into the
    /// one after it: the same audio, continuing where the other stops. Such
    /// an edge needs no click guard, and getting one would put a dip in the
    /// middle of what was one clip before it was split.
    pub joins_previous: bool,
    pub joins_next: bool,
    pub gain: f32,
    pub audio: RegionAudio,
    /// The stand-in this audio takes over from, if the previous graph was
    /// playing one for the clip. The first block that plays the region
    /// cross-fades from it, so the hand-over does not click.
    pub replaces: Option<RegionAudio>,
    /// Frames of that cross-fade played so far.
    pub blended: u32,
}

pub(crate) struct DeviceSlot {
    pub id: DeviceId,
    /// `None` in a freshly compiled graph means "keep the instance that is
    /// already running"; the audio thread fills it in during the swap.
    pub processor: Option<Box<dyn DeviceProcessor>>,
    pub enabled: bool,
    /// The key the device listens to, as a place in [`Graph::keys`]: only
    /// for a device that takes a key and was given a track to listen to.
    pub key: Option<usize>,
    /// For a render on more than one thread: the devices with the same
    /// number here are run one at a time and in the order of the song
    /// ([`trill_core::device::Beside::InTurn`]). Nothing for a device that
    /// may be run beside anything, and in a graph that is played.
    pub turn: Option<u32>,
}

/// An envelope as the audio thread plays it: its points, and what the
/// lines between them are straight along.
pub(crate) struct Envelope {
    /// Sorted by beat; never empty.
    pub points: Vec<AutomationPoint>,
    pub travel: Travel,
}

impl Envelope {
    pub fn of(lane: &AutomationLane) -> Self {
        Self { points: lane.points.clone(), travel: lane.travel }
    }

    /// What the envelope is worth at `beat`. One search among its points,
    /// however many there are: nothing is allocated and nothing waited for.
    pub fn value_at(&self, beat: f64) -> f64 {
        envelope_value(&self.points, self.travel, beat)
    }
}

pub(crate) struct ParamAutomation {
    /// Index into the chain's device slots.
    pub slot: usize,
    pub param: usize,
    pub envelope: Envelope,
    /// Last value sent, so unchanged values are not resent every block.
    pub last: f64,
}

/// A copy of a track's faded signal that goes to a return track. What it
/// carries is its level, where its envelope has it or where it was set by
/// hand, times how far mute and solo leave it open.
pub(crate) struct SendNode {
    /// The return's bus, and the return itself: by it what the send has
    /// come to is found again in the graph that takes over.
    pub bus: usize,
    pub to: TrackId,
    /// The level set by hand, as a gain; nothing where the send is its
    /// envelope alone.
    pub gain: f32,
    /// The envelope that moves it, in dB, which then is what counts.
    pub automation: Option<Envelope>,
    /// Whether the send carries anything: shut, because the return is not
    /// heard, or the track is not and the return does not take every
    /// track's send.
    pub open: bool,
    /// Where the level has got to: see [`Glide`].
    pub glide: Glide<1>,
    /// How far open it is: see [`Gate`].
    pub gate: Gate,
}

#[derive(Default)]
pub(crate) struct Chain {
    pub devices: Vec<DeviceSlot>,
    pub automation: Vec<ParamAutomation>,
}

/// Peak level since the last read, written by the audio thread.
#[derive(Default)]
pub struct Meter {
    left: AtomicU32,
    right: AtomicU32,
    /// For how many frames the track has been given notes that nothing
    /// answered: see [`Meter::unanswered_frames`].
    unanswered: AtomicU32,
    /// What making its blocks took since that was last asked
    /// ([`Meter::take_cost`]): how many blocks, of how many frames, in how
    /// many nanoseconds, and how long the longest of them took.
    cost_blocks: AtomicU64,
    cost_frames: AtomicU64,
    cost_nanos: AtomicU64,
    cost_longest: AtomicU64,
    /// The cycles the thread that made that longest block ran in it.
    cost_longest_ran: AtomicU64,
}

/// What making the blocks of a track (or of the master) took over some
/// time: see [`Meter::take_cost`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Cost {
    pub blocks: u64,
    pub frames: u64,
    pub nanos: u64,
    /// How long the one block took that took longest, in nanoseconds.
    /// (Blocks are of any length, down to a frame: as a share of its own
    /// time a short one says nothing.)
    pub longest: u64,
    /// How many of its processor's cycles the thread that made that block
    /// ran while it did ([`crate::host::thread_cycles`]): far fewer than
    /// the clock's time comes to where the thread was kept from its work.
    /// Nothing unless that is being counted ([`crate::Session::count_cycles`]).
    pub longest_ran: u64,
}

impl Cost {
    /// Adds what `later` counted to this.
    pub fn add(&mut self, later: &Cost) {
        self.blocks += later.blocks;
        self.frames += later.frames;
        self.nanos += later.nanos;
        if later.longest > self.longest {
            (self.longest, self.longest_ran) = (later.longest, later.longest_ran);
        }
    }

    /// The share of one processor's time it took, with the song running at
    /// `sample_rate`: 1 is a whole processor. Nothing where no block was
    /// made.
    pub fn share(&self, sample_rate: f64) -> f64 {
        match self.frames {
            0 => 0.0,
            frames => self.nanos as f64 * sample_rate / (frames as f64 * 1e9),
        }
    }
}

impl Meter {
    pub(crate) fn record(&self, left: f32, right: f32) {
        // Single writer, so load-then-store cannot lose a larger value to
        // another writer; a concurrent `take` merely starts a new window.
        if left > f32::from_bits(self.left.load(Ordering::Relaxed)) {
            self.left.store(left.to_bits(), Ordering::Relaxed);
        }
        if right > f32::from_bits(self.right.load(Ordering::Relaxed)) {
            self.right.store(right.to_bits(), Ordering::Relaxed);
        }
    }

    /// Returns the peak (linear) per channel since the previous call.
    pub fn take(&self) -> (f32, f32) {
        (f32::from_bits(self.left.swap(0, Ordering::Relaxed)), f32::from_bits(self.right.swap(0, Ordering::Relaxed)))
    }

    /// Returns the peak without resetting it.
    pub fn peek(&self) -> (f32, f32) {
        (f32::from_bits(self.left.load(Ordering::Relaxed)), f32::from_bits(self.right.load(Ordering::Relaxed)))
    }

    /// Puts on record that a block of `frames` frames took `nanos` to
    /// make. By whichever thread made it: one at a time.
    pub(crate) fn cost(&self, nanos: u64, cycles: u64, frames: usize) {
        self.cost_blocks.fetch_add(1, Ordering::Relaxed);
        self.cost_frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.cost_nanos.fetch_add(nanos, Ordering::Relaxed);
        // (One writer at a time: whoever makes the track's block.)
        if nanos > self.cost_longest.load(Ordering::Relaxed) {
            self.cost_longest.store(nanos, Ordering::Relaxed);
            self.cost_longest_ran.store(cycles, Ordering::Relaxed);
        }
    }

    /// What making the blocks took since the previous call.
    pub fn take_cost(&self) -> Cost {
        Cost {
            blocks: self.cost_blocks.swap(0, Ordering::Relaxed),
            frames: self.cost_frames.swap(0, Ordering::Relaxed),
            nanos: self.cost_nanos.swap(0, Ordering::Relaxed),
            longest: self.cost_longest.swap(0, Ordering::Relaxed),
            longest_ran: self.cost_longest_ran.swap(0, Ordering::Relaxed),
        }
    }

    pub(crate) fn set_unanswered(&self, frames: u32) {
        self.unanswered.store(frames, Ordering::Relaxed);
    }

    /// For how many frames notes have sounded on the track without its
    /// devices putting out anything at all, ahead of its fader. It counts
    /// from the first note of a phrase, only until something is heard, and
    /// goes back to zero as soon as something is; it stays where it is
    /// when the notes end in silence. An instrument that rings out under a
    /// long note has answered and is not counted.
    pub fn unanswered_frames(&self) -> u32 {
        self.unanswered.load(Ordering::Relaxed)
    }
}

/// The controllers that return to rest when let go of, by
/// [`ControllerKind::index`]: the pedals, channel pressure, the pitch wheel.
const SPRUNG: [usize; 8] = [64, 65, 66, 67, 68, 69, 128, 129];

/// Per-track state that must survive a graph swap.
pub(crate) struct TrackState {
    /// End beat of the sounding note per pitch, or negative when silent.
    pub active_until: [f64; 128],
    pub active_count: usize,
    /// Where the fader and the pan have the two sides: see [`Glide`].
    pub fader: Glide<2>,
    /// How much of the track's sound passes at all, and how much of that
    /// goes on to its output: what mute and solo open and shut, each a
    /// [`Gate`].
    pub passes: Gate,
    pub out: Gate,
    /// The keys held by hand on this track, a bit per pitch: through a
    /// keyboard or [`Session::note_on`](crate::Session::note_on).
    pub live_held: u128,
    /// Those of them a keyboard holds, which it lets go of when the track
    /// stops being one it plays.
    pub routed_held: u128,
    /// Where the hand has put each controller, by
    /// [`ControllerKind::index`]; not a number where it has not moved one,
    /// or the song has taken it back. While a controller is the hand's,
    /// the song's curve for it is not played.
    pub live: [f32; CONTROLLER_KINDS],
    /// What the song last told the instrument each controller is at; not
    /// a number where it has said nothing that still stands.
    pub sent: [f32; CONTROLLER_KINDS],
    /// Set to have the next block tell the instrument everything again: it
    /// is new, or was reset, and knows nothing of what it was told.
    pub resend: bool,
    /// The key that stays chosen on the instrument, as far as the song
    /// knows: the last such key it pressed. [`AS_MADE`] before it pressed
    /// any, [`UNKNOWN`] once a hand pressed one or the instrument may
    /// have missed what it was told.
    pub latched: u8,
    /// Set to have the next block press what is chosen where the song is,
    /// as a start does.
    pub rechoose: bool,
    /// Set when the graph changed: a controller the song moved may have
    /// lost its curve.
    pub lanes_changed: bool,
    /// Set to have the next block let go of what the keyboard holds.
    pub release_routed: bool,
    /// Whether notes were sounding in the block before, whether anything
    /// has been heard since they began, and for how many frames nothing
    /// has: see [`Meter::unanswered_frames`].
    pub had_notes: bool,
    pub answered: bool,
    pub unanswered: u32,
    /// What the meter was last told of that.
    pub unanswered_told: u32,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            active_until: [-1.0; 128],
            active_count: 0,
            fader: Glide::default(),
            passes: Gate::default(),
            out: Gate::default(),
            live_held: 0,
            routed_held: 0,
            live: [f32::NAN; CONTROLLER_KINDS],
            sent: [f32::NAN; CONTROLLER_KINDS],
            resend: false,
            latched: AS_MADE,
            rechoose: false,
            lanes_changed: false,
            release_routed: false,
            had_notes: false,
            answered: true,
            unanswered: 0,
            unanswered_told: u32::MAX,
        }
    }
}

impl TrackState {
    /// Whether a keyboard has anything to let go of here: a key, or a
    /// pedal or a wheel that it left away from rest.
    pub fn holds_routed(&self) -> bool {
        self.routed_held != 0 || SPRUNG.iter().any(|&index| !self.live[index].is_nan())
    }

    /// Lets go of the pedals and the wheels the hand left somewhere:
    /// `released` is told each one that was away from rest, to put it
    /// back. From here on they are the song's again.
    pub fn release_sprung(&mut self, mut released: impl FnMut(ControllerKind)) {
        for index in SPRUNG {
            let value = std::mem::replace(&mut self.live[index], f32::NAN);
            if value.is_nan() {
                continue;
            }
            // Whatever the song said last, it must say again.
            self.sent[index] = f32::NAN;
            if value != 0.0
                && let Some(kind) = ControllerKind::from_index(index)
            {
                released(kind);
            }
        }
    }

    /// Keeps track of what is held as `kind` is played by hand; `routed`
    /// when it came from a keyboard.
    pub fn played(&mut self, kind: EventKind, routed: bool) {
        match kind {
            EventKind::NoteOn { pitch, .. } => {
                let key = 1u128 << pitch.min(127);
                self.live_held |= key;
                if routed {
                    self.routed_held |= key;
                }
            }
            EventKind::NoteOff { pitch } => {
                let key = 1u128 << pitch.min(127);
                self.live_held &= !key;
                self.routed_held &= !key;
            }
            EventKind::AllNotesOff => self.forget_live(),
            EventKind::PitchBend { .. } | EventKind::Control { .. } | EventKind::ChannelPressure { .. } => {
                // From now on it is where the hand put it, not where the
                // song would have it.
                if let Some((controller, value)) = ControllerKind::of_event(kind) {
                    self.live[controller.index()] = if value.is_finite() { value } else { 0.0 };
                }
            }
        }
    }

    /// No key is held by hand any more: every note was released. The
    /// pedal and the wheel are where they were; they are still the
    /// keyboard's to put back.
    pub fn forget_live(&mut self) {
        self.live_held = 0;
        self.routed_held = 0;
    }
}

pub(crate) struct TrackNode {
    pub id: TrackId,
    /// Sorted by start.
    pub notes: Vec<NoteEvent>,
    /// The keys among the notes that choose how the notes after them are
    /// played: see [`Switch`]. Sorted by start; none on most tracks.
    pub switches: Vec<Switch>,
    /// Which pitches of the part are such keys, a bit each, and which of
    /// them stay chosen.
    pub switch_keys: u128,
    pub stay_keys: u128,
    /// The key that chooses how a note is played when nothing is said, if
    /// the track says one does: what a song is on before its part has
    /// chosen anything.
    pub unsaid_key: Option<u8>,
    /// What each controller the track's clips move is worth over the song.
    pub controllers: Vec<ControllerLane>,
    /// Whether the graph came with a device of this track created anew,
    /// which has yet to be told where its controllers are.
    pub fresh_devices: bool,
    pub audio: Vec<AudioRegion>,
    pub chain: Chain,
    pub volume_db: f32,
    pub pan: f32,
    /// Mute and solo, already resolved: whether the track's own sound is
    /// wanted in the mix. A render listens to the tracks that are.
    pub heard: bool,
    /// Whether the track passes sound at all: on to its output, or into a
    /// send. One that does not is faded to nothing.
    pub passes: bool,
    /// Whether what it passes goes on to its output, the master or its
    /// group. A track that only feeds a soloed return through a send
    /// passes sound and does not. A track that is not heard but plays
    /// through a group whose sound is wanted all the same (it sends to a
    /// soloed return, or a device listens to it) does.
    pub to_output: bool,
    pub volume_automation: Option<Envelope>,
    pub pan_automation: Option<Envelope>,
    pub meter: Arc<Meter>,
    pub state: TrackState,
    /// The bus this node reads as its input: set for groups and returns,
    /// which process what other tracks feed them.
    pub input_bus: Option<usize>,
    /// The group bus the faded signal goes to; the master when `None`.
    pub output_bus: Option<usize>,
    /// Copies of the faded signal for return tracks.
    pub sends: Vec<SendNode>,
    /// Where the track leaves a copy of itself as it comes out of its
    /// devices, ahead of its fader, for the devices that listen to it: a
    /// place in [`Graph::keys`].
    pub key_out: Option<usize>,
    /// Listens to the track as it leaves its fader. Only a graph that is
    /// rendered has one; a graph that is played measures nothing.
    pub tap: Option<Box<Tap>>,
    /// Listens to what goes into the track's effects: what its audio and
    /// the first so many devices of its chain (up to its instrument) have
    /// made, before anything else is done to it. Only in a render that
    /// was asked for it.
    pub source: Option<(usize, Box<Tap>)>,
}

/// What a render listens with at one point of the graph: it measures what
/// goes by, and writes it to a file if that was asked for (a stem). The
/// file is written a block at a time, so that nothing of it is held.
pub(crate) struct Tap {
    pub analyzer: crate::analysis::Analyzer,
    pub file: Option<trill_media::WavWriter>,
    /// How many frames go by before it begins to listen: what a render
    /// plays ahead of its start and keeps nothing of.
    pub unheard: u64,
}

impl Tap {
    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        let skipped = (self.unheard.min(left.len() as u64)) as usize;
        self.unheard -= skipped as u64;
        let (left, right) = (&left[skipped..], &right[skipped.min(right.len())..]);
        if left.is_empty() {
            return;
        }
        self.analyzer.push(left, right);
        if let Some(file) = &mut self.file {
            file.write(left, right);
        }
    }
}

/// Where a group or return track collects its input during a block.
pub(crate) struct Bus {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Bus {
    pub fn new() -> Self {
        Self { left: vec![0.0; MAX_BLOCK], right: vec![0.0; MAX_BLOCK] }
    }
}

pub(crate) struct MasterNode {
    pub chain: Chain,
    pub volume_db: f32,
    pub volume_automation: Option<Envelope>,
    pub meter: Arc<Meter>,
    /// Where the fader has the mix: see [`Glide`].
    pub fader: Glide<1>,
    /// Listens to the mix as it reaches the master's devices. Only in a
    /// render that was asked for it.
    pub tap: Option<Box<Tap>>,
}

pub struct Graph {
    /// In processing order: every node comes before the bus it feeds and
    /// before the tracks whose devices listen to it (normal tracks, then
    /// groups, then returns, unless a key says otherwise).
    pub(crate) tracks: Vec<TrackNode>,
    pub(crate) buses: Vec<Bus>,
    /// What the tracks that are listened to put out in the current block,
    /// one for each such track.
    pub(crate) keys: Vec<Bus>,
    pub(crate) master: MasterNode,
    /// How the song's beats become time, and how they become bars: what
    /// the audio thread counts frames and clicks the metronome by. Made
    /// with the graph and freed with it; reading them allocates nothing.
    pub(crate) tempo_map: TempoMap,
    pub(crate) meter_map: MeterMap,
    /// `(start, end)` in beats when looping is on.
    pub(crate) loop_region: Option<(f64, f64)>,
    /// Whether an envelope moves a parameter of any device. While such a
    /// graph plays, blocks end on a grid of the song's own, so that a
    /// parameter is moved at the same frames whatever the buffer size.
    pub(crate) moves_params: bool,
    /// What it takes to make the blocks of the tracks on more than one
    /// thread while the graph plays, where it can use more than one:
    /// see [`Graph::spread`].
    pub(crate) rounds: Option<Box<crate::spread::Rounds>>,
}

impl Graph {
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            buses: Vec::new(),
            keys: Vec::new(),
            master: MasterNode {
                chain: Chain::default(),
                volume_db: 0.0,
                volume_automation: None,
                meter: Arc::new(Meter::default()),
                fader: Glide::default(),
                tap: None,
            },
            tempo_map: TempoMap::constant(120.0),
            meter_map: MeterMap::constant(TimeSignature::default()),
            loop_region: None,
            moves_params: false,
            rounds: None,
        }
    }

    /// Works out how the blocks of this graph's tracks can be made on more
    /// than one thread while it plays, and keeps it with the graph: which
    /// tracks at once, and room for each. `factory` says what each device
    /// may be run beside, as it does for a render: the instances of a
    /// plugin in turn unless it was measured to keep nothing between
    /// them. Returns how many threads the graph can use; one, and nothing
    /// is kept, where every track waits for another or a device is not to
    /// be run beside anything.
    ///
    /// Off the audio thread, before the graph is handed over: this
    /// allocates. A processor that plays the graph spreads it where it was
    /// given a team and the song is worth spreading
    /// ([`crate::Processor::play_over`]).
    pub fn spread(&mut self, project: &trill_core::Project, factory: &dyn trill_core::device::DeviceFactory) -> usize {
        self.rounds = None;
        if !crate::spread::company(self, project, factory, (false, true)).is_empty() {
            return 1;
        }
        let rounds = crate::spread::Rounds::of(&self.tracks);
        let width = rounds.width();
        if width >= 2 {
            self.rounds = Some(Box::new(rounds));
        }
        width.max(1)
    }

    fn slots_mut(&mut self) -> impl Iterator<Item = &mut DeviceSlot> {
        self.tracks
            .iter_mut()
            .flat_map(|track| track.chain.devices.iter_mut())
            .chain(self.master.chain.devices.iter_mut())
    }

    /// Whether the graph comes with devices of its own: ones that were made
    /// for it and have not played. (Before [`Graph::adopt`], which hands it
    /// the ones that were playing.)
    pub(crate) fn brings_devices(&mut self) -> bool {
        self.slots_mut().any(|slot| slot.processor.is_some())
    }

    pub(crate) fn processor_mut(&mut self, id: DeviceId) -> Option<&mut Box<dyn DeviceProcessor>> {
        self.slots_mut().find(|slot| slot.id == id).and_then(|slot| slot.processor.as_mut())
    }

    /// Moves running state out of `old` into `self`. Realtime-safe: only
    /// pointer moves and small copies, no allocation.
    pub(crate) fn adopt(&mut self, old: &mut Graph) {
        for slot in self.slots_mut() {
            if slot.processor.is_none()
                && let Some(previous) = old.slots_mut().find(|s| s.id == slot.id)
            {
                slot.processor = previous.processor.take();
            }
        }
        for track in &mut self.tracks {
            if let Some(previous) = old.tracks.iter_mut().find(|t| t.id == track.id) {
                std::mem::swap(&mut track.state, &mut previous.state);
                track.state.lanes_changed = true;
                // A send goes on from where it was, level and gate, so that
                // one which a solo opens or shuts does so without a step.
                // One the track did not have before comes up from nothing.
                for send in &mut track.sends {
                    match previous.sends.iter().find(|before| before.to == send.to) {
                        Some(before) => (send.glide, send.gate) = (before.glide, before.gate),
                        None => send.gate = Gate::shut(),
                    }
                }
            }
            if track.fresh_devices {
                // It is on whatever it was made on, and where the song
                // stands a key may be chosen.
                (track.state.latched, track.state.rechoose) = (AS_MADE, true);
                track.state.resend = true;
            }
        }
        self.master.fader = old.master.fader;
        if let (Some(rounds), Some(used)) = (&mut self.rounds, &mut old.rounds) {
            rounds.adopt(used);
        }
    }
}
