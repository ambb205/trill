//! The render graph: an immutable-in-shape snapshot of the project that the
//! audio thread plays. The controller compiles a new graph after every edit
//! and hands it over; the audio thread swaps it in and carries the running
//! state (device instances, sounding notes, fader positions) across.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use trill_core::controllers::{CONTROLLER_KINDS, ControllerKind, ControllerLane};
use trill_core::device::{DeviceProcessor, EventKind};
use trill_core::project::AutomationPoint;
use trill_core::{DeviceId, TimeSignature, TrackId};

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
    /// corresponds to: output frame `n`, counted from beat 0, plays
    /// position `origin + n * step`. Usually negative or far past the
    /// start; only the part under the clip is ever read.
    pub origin: f64,
    /// Frames of `data` per output frame. Exactly 1 for everything but
    /// stand-ins and tempos beyond what the stretcher covers, and then
    /// frames are copied rather than interpolated.
    pub step: f64,
}

pub(crate) struct AudioRegion {
    pub start: f64,
    pub end: f64,
    /// Fade lengths in beats. The processor never fades an edge faster than
    /// its own click guard, unless the edge joins a neighbour.
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
}

pub(crate) struct ParamAutomation {
    /// Index into the chain's device slots.
    pub slot: usize,
    pub param: usize,
    pub points: Vec<AutomationPoint>,
    /// Last value sent, so unchanged values are not resent every block.
    pub last: f64,
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
    pub gain_left: f32,
    pub gain_right: f32,
    /// False until the first block, so the fader starts at its target
    /// instead of ramping up from silence.
    pub primed: bool,
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
            gain_left: 0.0,
            gain_right: 0.0,
            primed: false,
            live_held: 0,
            routed_held: 0,
            live: [f32::NAN; CONTROLLER_KINDS],
            sent: [f32::NAN; CONTROLLER_KINDS],
            resend: false,
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
    /// What each controller the track's clips move is worth over the song.
    pub controllers: Vec<ControllerLane>,
    /// Whether the graph came with a device of this track created anew,
    /// which has yet to be told where its controllers are.
    pub fresh_devices: bool,
    pub audio: Vec<AudioRegion>,
    pub chain: Chain,
    pub volume_db: f32,
    pub pan: f32,
    /// Mute and solo, already resolved.
    pub audible: bool,
    /// The track is not heard, because others are soloed, but the group it
    /// plays through is listened to by a device and is not heard either:
    /// the track goes on playing into its group, so that the device hears
    /// what it would in the whole mix, and into nothing else.
    pub feeds_key: bool,
    pub volume_automation: Option<Vec<AutomationPoint>>,
    pub pan_automation: Option<Vec<AutomationPoint>>,
    pub meter: Arc<Meter>,
    pub state: TrackState,
    /// The bus this node reads as its input: set for groups and returns,
    /// which process what other tracks feed them.
    pub input_bus: Option<usize>,
    /// The group bus the faded signal goes to; the master when `None`.
    pub output_bus: Option<usize>,
    /// Copies of the faded signal for return tracks: bus and linear gain.
    pub sends: Vec<(usize, f32)>,
    /// Where the track leaves a copy of itself as it comes out of its
    /// devices, ahead of its fader, for the devices that listen to it: a
    /// place in [`Graph::keys`].
    pub key_out: Option<usize>,
    /// Measures the track as it leaves its fader. Only a graph that is
    /// rendered has one; a graph that is played measures nothing.
    pub tap: Option<Box<crate::analysis::Analyzer>>,
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
    pub volume_automation: Option<Vec<AutomationPoint>>,
    pub meter: Arc<Meter>,
    pub gain: f32,
    pub primed: bool,
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
    pub(crate) tempo: f64,
    pub(crate) time_signature: TimeSignature,
    /// `(start, end)` in beats when looping is on.
    pub(crate) loop_region: Option<(f64, f64)>,
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
                gain: 1.0,
                primed: false,
            },
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            loop_region: None,
        }
    }

    fn slots_mut(&mut self) -> impl Iterator<Item = &mut DeviceSlot> {
        self.tracks
            .iter_mut()
            .flat_map(|track| track.chain.devices.iter_mut())
            .chain(self.master.chain.devices.iter_mut())
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
            }
            if track.fresh_devices {
                track.state.resend = true;
            }
        }
        self.master.gain = old.master.gain;
        self.master.primed = old.master.primed;
    }
}
