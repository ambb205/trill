//! A session playing through audio devices that exist only in the test,
//! with the test as their audio thread, and an instrument that writes down
//! everything it is told to play.
//!
//! Time moves only when a test advances the host, so where something is
//! played and where it lands are exact, whatever the machine is busy with.

#![allow(dead_code)]

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use trill_core::device::{
    CompositeFactory, DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, EventKind, ProcessInfo,
};
use trill_core::project::Device;
use trill_core::{ChainRef, DeviceId, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::host::scripted::{ScriptedDevice, ScriptedHost};
use trill_engine::{AudioConfig, AudioSetup, Patience, Session};
use trill_media::MediaCache;

pub const SR: f64 = 48_000.0;
/// The project tempo the rig sets: a beat is half a second.
pub const TEMPO: f64 = 120.0;
pub const FRAMES_PER_BEAT: f64 = 24_000.0;
/// The buffer and latency of [`ScriptedDevice::new`], shared.
pub const BUFFER: usize = 480;
pub const LATENCY: f64 = 1_440.0;
pub const PROBE_URI: &str = "test:probe";
/// The key a probe panics over when it goes down, in a rig that was told
/// its probes fail ([`ProbeLog::fail`]): a device that fails.
pub const PANIC_KEY: u8 = 1;

/// Something a probe was told to play: by which device, at which frame of
/// that device's life, and where the song was at the start of that block.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Seen {
    pub device: DeviceId,
    pub frame: u64,
    pub kind: EventKind,
    /// The MIDI channel it was told it on.
    pub channel: trill_core::Channel,
}

/// What the probes of a rig have in common.
#[derive(Default)]
pub struct ProbeLog {
    pub seen: Mutex<Vec<Seen>>,
    /// How many probes have been created.
    pub created: AtomicUsize,
    /// How many frames probes have processed, all told. With one probe in
    /// the project this is that probe's clock.
    pub frames: AtomicU64,
    /// The probes go down over [`PANIC_KEY`].
    pub fail: std::sync::atomic::AtomicBool,
    /// How long a probe takes over every block it is given, in
    /// nanoseconds: a device that costs more than the machine has.
    pub slow_ns: AtomicU64,
}

impl ProbeLog {
    /// Takes what was seen since the last call.
    pub fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut self.seen.lock())
    }

    /// What was seen since the last call, as `(frame, kind)`.
    pub fn events(&self) -> Vec<(u64, EventKind)> {
        self.take().into_iter().map(|seen| (seen.frame, seen.kind)).collect()
    }

    /// What was seen since the last call, with the channel each thing was
    /// said on, as a person counts them: `(frame, channel, kind)`.
    pub fn events_on(&self) -> Vec<(u64, u8, EventKind)> {
        self.take().into_iter().map(|seen| (seen.frame, seen.channel.number(), seen.kind)).collect()
    }
}

struct Probe {
    device: DeviceId,
    frames: u64,
    log: Arc<ProbeLog>,
}

impl DeviceProcessor for Probe {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, events: &[Event], left: &mut [f32], _right: &mut [f32]) {
        let fails = |event: &Event| matches!(event.kind, EventKind::NoteOn { pitch: PANIC_KEY, .. });
        if self.log.fail.load(Ordering::Relaxed) && events.iter().any(fails) {
            panic!("a probe was played the key it fails on, on purpose");
        }
        match self.log.slow_ns.load(Ordering::Relaxed) {
            0 => {}
            slow => std::thread::sleep(Duration::from_nanos(slow)),
        }
        if !events.is_empty() {
            let mut seen = self.log.seen.lock();
            for event in events {
                let frame = self.frames + event.offset as u64;
                seen.push(Seen { device: self.device, frame, kind: event.kind, channel: event.channel });
            }
        }
        self.frames += left.len() as u64;
        self.log.frames.fetch_add(left.len() as u64, Ordering::Relaxed);
    }

    fn reset(&mut self) {}
}

/// Makes probes: instruments that play nothing and note what they are told.
pub struct ProbeFactory {
    pub log: Arc<ProbeLog>,
}

impl DeviceFactory for ProbeFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: PROBE_URI.into(),
            name: "Probe".into(),
            vendor: "Test".into(),
            category: DeviceCategory::Instrument,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != PROBE_URI {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        self.log.created.fetch_add(1, Ordering::Relaxed);
        let probe = Probe { device: device.id, frames: 0, log: self.log.clone() };
        Ok(DeviceInstance { processor: Box::new(probe), params: Vec::new(), controller: None })
    }
}

pub struct Rig {
    pub host: ScriptedHost,
    pub session: Session,
    pub probes: Arc<ProbeLog>,
}

/// Timeouts short enough for a test to sit through.
pub fn impatient() -> Patience {
    Patience {
        device: Duration::from_millis(150),
        audio_thread: Duration::from_millis(150),
        stall: Duration::from_millis(60),
        retry: Duration::from_millis(1),
    }
}

impl Rig {
    /// A session at 120 BPM playing through one device, `Speakers`.
    pub fn new() -> Self {
        Self::with(ScriptedHost::with_speakers(), AudioConfig::default(), Patience::default())
    }

    pub fn with(host: ScriptedHost, config: AudioConfig, patience: Patience) -> Self {
        let probes = Arc::new(ProbeLog::default());
        let factory =
            CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(ProbeFactory { log: probes.clone() })]);
        let setup = AudioSetup { patience, ..AudioSetup::on(Arc::new(host.clone()), config) };
        let session = Session::with_audio(Arc::new(factory), Arc::new(MediaCache::default()), setup).unwrap();
        Self { host, session, probes }
    }

    /// Adds a track with a probe for an instrument.
    pub fn probe_track(&mut self, name: &str) -> TrackId {
        let track = self.session.edit(|project| Ok(project.add_track(name))).unwrap();
        self.session.add_device(ChainRef::Track(track), PROBE_URI, None, None).unwrap();
        track
    }

    /// Adds a track with a sine synth whose notes start and stop at once.
    pub fn sine_track(&mut self, name: &str) -> TrackId {
        let track = self.session.edit(|project| Ok(project.add_track(name))).unwrap();
        let synth = self.session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        let params = [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0)];
        let params: Vec<(String, f64)> = params.iter().map(|(key, value)| (key.to_string(), *value)).collect();
        self.session.set_device_params(synth, &params).unwrap();
        track
    }

    /// The host's clock, in frames at 48 kHz.
    pub fn now(&self) -> f64 {
        self.host.now_ns() as f64 * SR / 1e9
    }

    /// The time on the host's clock of `frame`, a frame at 48 kHz.
    pub fn ns(frame: f64) -> u64 {
        (frame * 1e9 / SR).round() as u64
    }

    /// The device asks for `buffers` more buffers.
    pub fn run(&mut self, buffers: usize) {
        for _ in 0..buffers {
            let buffer = self.host.running().map_or(0, |format| format.buffer_frames as usize);
            self.host.advance(buffer);
        }
    }

    /// Runs until the host's clock reaches `frame` (a frame at 48 kHz),
    /// doing the session's chores as the housekeeping thread would.
    pub fn run_until(&mut self, frame: f64) {
        let mut buffers = 0;
        while self.now() < frame {
            self.run(1);
            buffers += 1;
            if buffers % 20 == 0 {
                self.session.housekeeping();
            }
        }
    }

    /// Plays `kind` as a keyboard would, at `frame` on the host's clock:
    /// runs until the buffer in whose time that falls has gone by, and
    /// sends it stamped with that time.
    pub fn key(&mut self, frame: f64, kind: EventKind) {
        self.run_until(frame);
        assert!(self.session.live().play_at(Self::ns(frame), kind));
    }
}

pub fn on(pitch: u8) -> EventKind {
    EventKind::NoteOn { pitch, velocity: 100.0 / 127.0 }
}

pub fn off(pitch: u8) -> EventKind {
    EventKind::NoteOff { pitch }
}

pub fn pedal(down: bool) -> EventKind {
    EventKind::Control { controller: 64, value: if down { 1.0 } else { 0.0 } }
}

/// A device called `name`, like [`ScriptedDevice::new`].
pub fn device(name: &str) -> ScriptedDevice {
    ScriptedDevice::new(name)
}

/// The notes of a clip as `(pitch, start, length)` on the timeline.
pub fn timeline(session: &Session, clip: trill_core::ClipId) -> Vec<(u8, f64, f64)> {
    let clip = session.project().clip(clip).unwrap();
    clip.timeline_notes().iter().map(|note| (note.pitch, note.start, note.length)).collect()
}

/// Asserts that `notes` are `expected`, to within a hundredth of a frame.
pub fn assert_notes(notes: &[(u8, f64, f64)], expected: &[(u8, f64, f64)]) {
    assert_eq!(notes.len(), expected.len(), "{notes:?} against {expected:?}");
    let tolerance = 0.01 / FRAMES_PER_BEAT;
    for (note, wanted) in notes.iter().zip(expected) {
        let close =
            note.0 == wanted.0 && (note.1 - wanted.1).abs() < tolerance && (note.2 - wanted.2).abs() < tolerance;
        assert!(close, "{note:?} should be {wanted:?} (all: {notes:?})");
    }
}
