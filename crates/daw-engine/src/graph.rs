//! The render graph: an immutable-in-shape snapshot of the project that the
//! audio thread plays. The controller compiles a new graph after every edit
//! and hands it over; the audio thread swaps it in and carries the running
//! state (device instances, sounding notes, fader positions) across.

use daw_core::device::DeviceProcessor;
use daw_core::project::AutomationPoint;
use daw_core::{DeviceId, TimeSignature, TrackId};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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
pub type AudioData = daw_core::audio::AudioBuffer;

pub(crate) struct AudioRegion {
    pub start: f64,
    pub end: f64,
    /// Source position at the start of the clip, in source frames.
    pub offset_frames: f64,
    /// Fade lengths in beats; the processor never fades faster than its
    /// own click guard.
    pub fade_in: f64,
    pub fade_out: f64,
    pub gain: f32,
    pub data: Arc<AudioData>,
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
}

impl Default for TrackState {
    fn default() -> Self {
        Self { active_until: [-1.0; 128], active_count: 0, gain_left: 0.0, gain_right: 0.0, primed: false }
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
    pub(crate) tracks: Vec<TrackNode>,
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
