//! The render graph: an immutable-in-shape snapshot of the project that the
//! audio thread plays. The controller compiles a new graph after every edit
//! and hands it over; the audio thread swaps it in and carries the running
//! state (device instances, sounding notes, fader positions) across.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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
}

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
    /// Whether the sustain pedal is down, and the pitch wheel off centre.
    pub live_sustain: bool,
    pub live_bent: bool,
    /// Set to have the next block let go of what the keyboard holds.
    pub release_routed: bool,
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
            live_sustain: false,
            live_bent: false,
            release_routed: false,
        }
    }
}

impl TrackState {
    /// Whether a keyboard has anything to let go of here.
    pub fn holds_routed(&self) -> bool {
        self.routed_held != 0 || self.live_sustain || self.live_bent
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
            EventKind::PitchBend { value } => self.live_bent = value != 0.0,
            EventKind::Control { .. } => {
                if let Some(down) = kind.sustain() {
                    self.live_sustain = down;
                }
            }
        }
    }

    /// Nothing is held by hand any more: everything was released.
    pub fn forget_live(&mut self) {
        self.live_held = 0;
        self.routed_held = 0;
        self.live_sustain = false;
        self.live_bent = false;
    }
}

pub(crate) struct TrackNode {
    pub id: TrackId,
    /// Sorted by start.
    pub notes: Vec<NoteEvent>,
    pub audio: Vec<AudioRegion>,
    pub chain: Chain,
    pub volume_db: f32,
    pub pan: f32,
    /// Mute and solo, already resolved.
    pub audible: bool,
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
    /// In processing order: every node comes before the bus it feeds
    /// (normal tracks, then groups, then returns).
    pub(crate) tracks: Vec<TrackNode>,
    pub(crate) buses: Vec<Bus>,
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
            }
        }
        self.master.gain = old.master.gain;
        self.master.primed = old.master.primed;
    }
}
