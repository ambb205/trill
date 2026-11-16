//! The events and transport handed to a plugin with each block, and the
//! events it hands back.
//!
//! Everything here is used on the audio thread: fixed capacity, no
//! allocation after construction.

use crate::edits::{Edit, EditQueue};
use crate::notes::{HeldKeys, events_in_part};
use clack_host::events::event_types::{
    MidiEvent, NoteChokeEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::io::{InputEventBuffer, OutputEventBuffer, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{Event as _, EventFlags, EventHeader, Match, Pckn, UnknownEvent};
use clack_host::utils::{BeatTime, ClapId, SecondsTime};
use std::collections::HashMap;
use trill_core::device::{Event, EventKind, MAX_EVENTS_PER_BLOCK, Pedal, ProcessInfo};

/// The form in which a plugin's note port accepts notes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoteDialect {
    /// CLAP note events.
    Clap,
    /// Raw MIDI 1.0 messages.
    Midi,
}

/// How a plugin's first note port is addressed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotePort {
    /// The form notes are sent in.
    pub dialect: NoteDialect,
    /// Whether the port also takes raw MIDI, whatever form notes are sent
    /// in. That allows MIDI's channel-wide "all sound off".
    pub midi: bool,
}

/// Notes go to the plugin's first note port, on its first channel.
const NOTE_PORT: u16 = 0;
const NOTE_CHANNEL: u16 = 0;
const MIDI_NOTE_ON: u8 = 0x90;
const MIDI_NOTE_OFF: u8 = 0x80;
const MIDI_CONTROL_CHANGE: u8 = 0xB0;
const MIDI_PITCH_BEND: u8 = 0xE0;
const MIDI_ALL_SOUND_OFF: u8 = 120;
/// Most note events delivered to a plugin in one block; more are dropped.
/// The engine never passes more.
const MAX_NOTE_EVENTS: usize = MAX_EVENTS_PER_BLOCK;
/// Events needed to silence a plugin on top of that: a note-off and a
/// choke for each key, and one MIDI message.
const MAX_SILENCE_EVENTS: usize = 2 * 128 + 1;

#[derive(Copy, Clone)]
enum Queued {
    NoteOn(NoteOnEvent),
    NoteOff(NoteOffEvent),
    NoteChoke(NoteChokeEvent),
    Midi(MidiEvent),
    Param(ParamValueEvent),
}

/// The input events of one block, in the order they were queued. The
/// caller queues them sorted by time, as CLAP requires.
///
/// The capacity is fixed: once full, further events are dropped instead of
/// growing the buffer on the audio thread.
pub(crate) struct EventQueue {
    events: Vec<Queued>,
}

impl EventQueue {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self { events: Vec::with_capacity(capacity) }
    }

    pub(crate) fn clear(&mut self) {
        self.events.clear();
    }

    fn push(&mut self, event: Queued) {
        if self.events.len() < self.events.capacity() {
            self.events.push(event);
        }
    }

    /// Sets a parameter at the start of the block.
    pub(crate) fn param(&mut self, id: ClapId, value: f64) {
        self.push(Queued::Param(ParamValueEvent::new(0, id, Pckn::match_all(), value)));
    }

    /// `velocity` is 0..=1.
    pub(crate) fn note_on(&mut self, dialect: NoteDialect, time: u32, key: u8, velocity: f32) {
        let key = key.min(127);
        let velocity = if velocity.is_finite() { velocity.clamp(0.0, 1.0) } else { 1.0 };
        self.push(match dialect {
            NoteDialect::Clap => Queued::NoteOn(NoteOnEvent::new(time, note_address(key), velocity.into())),
            // In MIDI a note-on with velocity 0 means note-off.
            NoteDialect::Midi => {
                let velocity = ((velocity * 127.0).round() as u8).clamp(1, 127);
                Queued::Midi(MidiEvent::new(time, NOTE_PORT, [MIDI_NOTE_ON | NOTE_CHANNEL as u8, key, velocity]))
            }
        });
    }

    pub(crate) fn note_off(&mut self, dialect: NoteDialect, time: u32, key: u8) {
        let key = key.min(127);
        self.push(match dialect {
            NoteDialect::Clap => Queued::NoteOff(NoteOffEvent::new(time, note_address(key), 0.0)),
            NoteDialect::Midi => {
                Queued::Midi(MidiEvent::new(time, NOTE_PORT, [MIDI_NOTE_OFF | NOTE_CHANNEL as u8, key, 0]))
            }
        });
    }

    /// A MIDI controller, `value` 0..=1, for a port that takes MIDI.
    pub(crate) fn control(&mut self, time: u32, controller: u8, value: f32) {
        let value = if value.is_finite() { (value.clamp(0.0, 1.0) * 127.0).round() as u8 } else { 0 };
        let message = [MIDI_CONTROL_CHANGE | NOTE_CHANNEL as u8, controller & 0x7F, value];
        self.push(Queued::Midi(MidiEvent::new(time, NOTE_PORT, message)));
    }

    /// The pitch wheel, `value` -1..=1, for a port that takes MIDI.
    pub(crate) fn pitch_bend(&mut self, time: u32, value: f32) {
        let value = if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 };
        // Fourteen bits with 8192 at rest; the way up is a step shorter.
        let raw = (8192.0 + if value >= 0.0 { value * 8191.0 } else { value * 8192.0 }).round() as u16;
        let message = [MIDI_PITCH_BEND | NOTE_CHANNEL as u8, (raw & 0x7F) as u8, (raw >> 7) as u8 & 0x7F];
        self.push(Queued::Midi(MidiEvent::new(time, NOTE_PORT, message)));
    }

    /// Cuts every sounding voice of the plugin short at the start of the
    /// block, as far as it lets a host do that: the keys in `held` are
    /// released and choked, and a port that takes MIDI is told "all sound
    /// off".
    pub(crate) fn silence(&mut self, port: NotePort, held: &mut HeldKeys) {
        for key in held.take() {
            self.note_off(port.dialect, 0, key);
            if port.dialect == NoteDialect::Clap {
                self.push(Queued::NoteChoke(NoteChokeEvent::new(0, note_address(key))));
            }
        }
        if port.midi {
            let message = [MIDI_CONTROL_CHANGE | NOTE_CHANNEL as u8, MIDI_ALL_SOUND_OFF, 0];
            self.push(Queued::Midi(MidiEvent::new(0, NOTE_PORT, message)));
        }
    }
}

/// The note id is left unspecified: the engine addresses notes by key.
fn note_address(key: u8) -> Pckn {
    Pckn::new(NOTE_PORT, NOTE_CHANNEL, u16::from(key), Match::All)
}

impl InputEventBuffer for EventQueue {
    fn len(&self) -> u32 {
        // The capacity is nowhere near `u32::MAX`; saturating keeps this
        // free of panics regardless.
        u32::try_from(self.events.len()).unwrap_or(u32::MAX)
    }

    fn get(&self, index: u32) -> Option<&UnknownEvent> {
        self.events.get(index as usize).map(|event| match event {
            Queued::NoteOn(event) => event.as_unknown(),
            Queued::NoteOff(event) => event.as_unknown(),
            Queued::NoteChoke(event) => event.as_unknown(),
            Queued::Midi(event) => event.as_unknown(),
            Queued::Param(event) => event.as_unknown(),
        })
    }
}

/// Turns what the engine tells a device (parameter changes, note events,
/// resets) into the CLAP events of each block.
pub(crate) struct BlockEvents {
    /// CLAP id of each parameter, or `None` if the host may not set it.
    param_ids: Vec<Option<ClapId>>,
    /// Latest value set for each parameter, valid where `dirty` is set.
    pending: Vec<f64>,
    dirty: Vec<bool>,
    /// Indices of the dirty parameters, in the order they were first set.
    /// One entry per parameter at most, so it never outgrows its capacity.
    dirty_list: Vec<usize>,
    notes: Option<NotePort>,
    held: HeldKeys,
    /// The sustain pedal, worked out here for a plugin whose note port
    /// does not take MIDI and so cannot be sent it.
    pedal: Pedal,
    silence: bool,
    /// The note events of blocks the plugin could not be given, waiting
    /// for the next one it can. Never grows beyond its capacity.
    deferred: Vec<EventKind>,
    queue: EventQueue,
}

impl BlockEvents {
    /// `param_ids` is in parameter-list order; `notes` is how the plugin
    /// takes notes, if it does.
    pub(crate) fn new(param_ids: Vec<Option<ClapId>>, notes: Option<NotePort>) -> Self {
        let count = param_ids.len();
        Self {
            param_ids,
            pending: vec![0.0; count],
            dirty: vec![false; count],
            dirty_list: Vec::with_capacity(count),
            notes,
            held: HeldKeys::default(),
            pedal: Pedal::default(),
            silence: false,
            deferred: Vec::with_capacity(MAX_NOTE_EVENTS),
            queue: EventQueue::with_capacity(count + 2 * MAX_NOTE_EVENTS + MAX_SILENCE_EVENTS),
        }
    }

    /// Records a parameter change for the next block. Setting a parameter
    /// several times between blocks sends only the last value.
    pub(crate) fn set_param(&mut self, index: usize, value: f64) {
        if index >= self.pending.len() || !value.is_finite() {
            return;
        }
        self.pending[index] = value;
        if !self.dirty[index] {
            self.dirty[index] = true;
            self.dirty_list.push(index);
        }
    }

    /// Asks for every voice to be cut off with the next block.
    pub(crate) fn silence(&mut self) {
        self.silence = true;
    }

    /// Forgets which keys are down, after the plugin has been deactivated
    /// and with it every voice has ended.
    pub(crate) fn forget_notes(&mut self) {
        self.held.take().for_each(drop);
        self.pedal.clear();
        self.silence = false;
        self.deferred.clear();
    }

    /// Keeps the note events of a block the plugin could not be given, to
    /// deliver them at the start of the next. A key that was released
    /// during such a block would otherwise stay down for good.
    pub(crate) fn defer(&mut self, events: &[Event]) {
        let room = self.deferred.capacity() - self.deferred.len();
        self.deferred.extend(events.iter().take(room).map(|event| event.kind));
    }

    /// Builds the events for the frames `start..start + frames` of a block
    /// that is `total` frames long and came with `events`, sorted by offset.
    ///
    /// The engine never hands over more than the plugin's maximum block
    /// size, so normally this is called once per block for all of it.
    pub(crate) fn fill(&mut self, events: &[Event], start: usize, frames: usize, total: usize) -> &EventQueue {
        self.queue.clear();
        // Parameter changes come first: they apply from the first frame.
        for index in self.dirty_list.drain(..) {
            self.dirty[index] = false;
            if let Some(id) = self.param_ids[index] {
                self.queue.param(id, self.pending[index]);
            }
        }
        let Some(port) = self.notes else { return &self.queue };
        if std::mem::take(&mut self.silence) {
            self.queue.silence(port, &mut self.held);
        }

        // What earlier blocks could not deliver comes first, in order.
        let deferred = self.deferred.len();
        let late = (0..deferred).map(|index| (0, self.deferred[index]));
        for (time, kind) in late.chain(events_in_part(events, start, frames, total)) {
            match kind {
                EventKind::NoteOn { pitch, velocity } => {
                    self.pedal.key_down(pitch);
                    self.held.press(pitch);
                    self.queue.note_on(port.dialect, time, pitch, velocity);
                }
                EventKind::NoteOff { pitch } => {
                    // A plugin that is sent the pedal works it itself. For
                    // one that is not, the key stays down until the pedal
                    // comes up.
                    if port.midi || self.pedal.key_up(pitch) {
                        self.held.release(pitch);
                        self.queue.note_off(port.dialect, time, pitch);
                    }
                }
                EventKind::AllNotesOff => {
                    self.pedal.clear();
                    for key in self.held.take() {
                        self.queue.note_off(port.dialect, time, key);
                    }
                }
                // The wheels and the pedal are MIDI's: a port that takes
                // MIDI gets them as they are. One that takes only CLAP
                // notes has no use for the wheels, and the pedal is worked
                // out for it.
                EventKind::PitchBend { value } => {
                    if port.midi {
                        self.queue.pitch_bend(time, value);
                    }
                }
                EventKind::Control { controller, value } => {
                    if port.midi {
                        self.queue.control(time, controller, value);
                    } else if let Some(down) = kind.sustain() {
                        for key in self.pedal.set(down) {
                            self.held.release(key);
                            self.queue.note_off(port.dialect, time, key);
                        }
                    }
                }
            }
        }
        self.deferred.clear();
        &self.queue
    }
}

/// What a plugin reports about its own parameters: the edits made in its
/// editor. The audio-thread half and the main-thread half of an instance
/// both write here (whichever of them exchanges events with the plugin),
/// and the device's controller reads.
pub(crate) struct ParamFeedback {
    /// Where each parameter is in the device's parameter list, by CLAP id.
    /// Looking one up does not allocate, so the audio thread may.
    indices: HashMap<ClapId, usize>,
    /// The edits, each under its parameter's place in the device's list.
    pub edits: EditQueue,
}

impl ParamFeedback {
    /// `ids` are the parameters' CLAP ids in parameter-list order.
    pub(crate) fn new(ids: &[ClapId]) -> Self {
        Self { indices: ids.iter().enumerate().map(|(index, id)| (*id, index)).collect(), edits: EditQueue::new() }
    }
}

/// The list a plugin puts its output events on, in `process` and in
/// `flush` alike. Of everything a plugin may send, this host has use for
/// what it says about its parameters: that the user took hold of one,
/// what value it has now, that the user let go. Those go to the feedback
/// queue; the rest is dropped.
pub(crate) struct ParamOutput<'a>(pub &'a ParamFeedback);

impl OutputEventBuffer for ParamOutput<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let (id, edit) = match event.as_core_event() {
            // Whatever note the event is addressed to: CLAP lets a value
            // target one voice, but what plugins report to their host is
            // the parameter's value, and those built on JUCE address it
            // to "port 0, channel 0, key 0" where the wildcard was meant.
            Some(CoreEventSpace::ParamValue(event)) => (event.param_id(), Edit::Value(event.value())),
            Some(CoreEventSpace::ParamGestureBegin(event)) => (event.param_id(), Edit::Begin),
            Some(CoreEventSpace::ParamGestureEnd(event)) => (event.param_id(), Edit::End),
            _ => return Ok(()),
        };
        if let Some(index) = id.and_then(|id| self.0.indices.get(&id)) {
            self.0.edits.push(*index, edit);
        }
        Ok(())
    }
}

/// The transport as of `frame_offset` frames into the block `info`
/// describes.
pub(crate) fn transport(info: &ProcessInfo, frame_offset: usize) -> TransportEvent {
    let tempo = if info.tempo.is_finite() && info.tempo > 0.0 { info.tempo } else { 120.0 };
    let mut beats = if info.position_beats.is_finite() { info.position_beats } else { 0.0 };
    let mut flags =
        TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE | TransportFlags::HAS_SECONDS_TIMELINE;
    if info.playing {
        flags |= TransportFlags::IS_PLAYING;
        // The song only advances within the block while it is playing.
        if info.sample_rate > 0.0 {
            beats += frame_offset as f64 * tempo / (60.0 * info.sample_rate);
        }
    }
    let signature = info.time_signature;
    let beats_per_bar = signature.beats_per_bar();
    let bar = (beats / beats_per_bar).floor();
    flags |= TransportFlags::HAS_TIME_SIGNATURE;
    let (loop_start, loop_end) = info.loop_range.unwrap_or((0.0, 0.0));
    if info.loop_range.is_some() {
        flags |= TransportFlags::IS_LOOP_ACTIVE;
    }

    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(beats),
        // The engine's tempo is constant over the whole song.
        song_pos_seconds: SecondsTime::from_float(beats * 60.0 / tempo),
        tempo,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_float(loop_start),
        loop_end_beats: BeatTime::from_float(loop_end),
        loop_start_seconds: SecondsTime::from_float(loop_start * 60.0 / tempo),
        loop_end_seconds: SecondsTime::from_float(loop_end * 60.0 / tempo),
        bar_start: BeatTime::from_float(bar * beats_per_bar),
        bar_number: bar as i32,
        time_signature_numerator: signature.numerator as u16,
        time_signature_denominator: signature.denominator as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(playing: bool, position_beats: f64) -> ProcessInfo {
        ProcessInfo {
            sample_rate: 48_000.0,
            tempo: 120.0,
            time_signature: Default::default(),
            playing,
            position_beats,
            loop_range: None,
        }
    }

    #[test]
    fn events_keep_their_order_and_convert_per_dialect() {
        let mut queue = EventQueue::with_capacity(8);
        queue.param(ClapId::new(7), 0.25);
        queue.note_on(NoteDialect::Clap, 3, 60, 0.5);
        queue.note_off(NoteDialect::Clap, 9, 60);
        queue.note_on(NoteDialect::Midi, 10, 64, 1.0);
        queue.note_off(NoteDialect::Midi, 11, 64);
        assert_eq!(queue.len(), 5);

        let param = queue.get(0).unwrap().as_event::<ParamValueEvent>().unwrap();
        assert_eq!((param.param_id(), param.value(), param.header().time()), (Some(ClapId::new(7)), 0.25, 0));

        let on = queue.get(1).unwrap().as_event::<NoteOnEvent>().unwrap();
        assert_eq!(on.header().time(), 3);
        assert_eq!(on.pckn(), Pckn::new(0u16, 0u16, 60u16, Match::All));
        assert_eq!(on.velocity(), 0.5);

        let off = queue.get(2).unwrap().as_event::<NoteOffEvent>().unwrap();
        assert_eq!((off.header().time(), off.pckn().key), (9, Match::Specific(60)));

        let midi_on = queue.get(3).unwrap().as_event::<MidiEvent>().unwrap();
        assert_eq!((midi_on.header().time(), midi_on.data()), (10, [0x90, 64, 127]));
        let midi_off = queue.get(4).unwrap().as_event::<MidiEvent>().unwrap();
        assert_eq!((midi_off.header().time(), midi_off.data()), (11, [0x80, 64, 0]));

        assert!(queue.get(5).is_none());
        queue.clear();
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn a_quiet_midi_note_is_still_a_note_on() {
        let mut queue = EventQueue::with_capacity(2);
        queue.note_on(NoteDialect::Midi, 0, 200, 0.0001);
        let event = queue.get(0).unwrap().as_event::<MidiEvent>().unwrap();
        assert_eq!(event.data(), [0x90, 127, 1], "the key is clamped and velocity 0 is avoided");
    }

    #[test]
    fn a_full_queue_drops_events_instead_of_growing() {
        let mut queue = EventQueue::with_capacity(4);
        let capacity = queue.events.capacity();
        let storage = queue.events.as_ptr();
        for key in 0..capacity as u8 + 10 {
            queue.note_on(NoteDialect::Clap, 0, key % 128, 1.0);
        }
        assert_eq!(queue.len() as usize, capacity);
        assert_eq!(queue.events.as_ptr(), storage, "the buffer was never reallocated");
    }

    #[test]
    fn silencing_releases_and_chokes_what_is_held() {
        let mut keys = HeldKeys::default();
        keys.press(60);
        keys.press(72);

        let mut queue = EventQueue::with_capacity(16);
        queue.silence(NotePort { dialect: NoteDialect::Clap, midi: true }, &mut keys);
        assert_eq!(queue.len(), 5);
        for (index, key) in [(0, 60), (2, 72)] {
            let off = queue.get(index).unwrap().as_event::<NoteOffEvent>().unwrap();
            let choke = queue.get(index + 1).unwrap().as_event::<NoteChokeEvent>().unwrap();
            assert_eq!((off.pckn().key, choke.pckn().key), (Match::Specific(key), Match::Specific(key)));
            assert_eq!((off.header().time(), choke.header().time()), (0, 0));
        }
        assert_eq!(queue.get(4).unwrap().as_event::<MidiEvent>().unwrap().data(), [0xB0, 120, 0]);
        assert_eq!(keys.take().count(), 0, "the keys are no longer held");

        // A MIDI port gets MIDI only; a CLAP-only port gets no MIDI.
        keys.press(60);
        queue.clear();
        queue.silence(NotePort { dialect: NoteDialect::Midi, midi: true }, &mut keys);
        let sent: Vec<[u8; 3]> =
            (0..queue.len()).map(|index| queue.get(index).unwrap().as_event::<MidiEvent>().unwrap().data()).collect();
        assert_eq!(sent, [[0x80, 60, 0], [0xB0, 120, 0]]);

        queue.clear();
        queue.silence(NotePort { dialect: NoteDialect::Clap, midi: false }, &mut keys);
        assert_eq!(queue.len(), 0, "nothing is held and the port takes no MIDI");
    }

    const CLAP_PORT: Option<NotePort> = Some(NotePort { dialect: NoteDialect::Clap, midi: false });

    fn on(offset: u32, pitch: u8) -> Event {
        Event { offset, kind: EventKind::NoteOn { pitch, velocity: 1.0 } }
    }

    fn off(offset: u32, pitch: u8) -> Event {
        Event { offset, kind: EventKind::NoteOff { pitch } }
    }

    /// Each queued event as `(time, what, key or parameter id, value)`.
    fn listing(queue: &EventQueue) -> Vec<(u32, &'static str, u32, f64)> {
        (0..queue.len())
            .map(|index| {
                let event = queue.get(index).unwrap();
                let time = event.header().time();
                let key = |pckn: Pckn| pckn.key.into_specific().map_or(u32::MAX, u32::from);
                if let Some(event) = event.as_event::<ParamValueEvent>() {
                    (time, "param", event.param_id().map_or(u32::MAX, ClapId::get), event.value())
                } else if let Some(event) = event.as_event::<NoteOnEvent>() {
                    (time, "on", key(event.pckn()), event.velocity())
                } else if let Some(event) = event.as_event::<NoteOffEvent>() {
                    (time, "off", key(event.pckn()), 0.0)
                } else if let Some(event) = event.as_event::<NoteChokeEvent>() {
                    (time, "choke", key(event.pckn()), 0.0)
                } else {
                    (time, "midi", 0, 0.0)
                }
            })
            .collect()
    }

    #[test]
    fn parameter_changes_lead_the_block_and_only_the_last_value_counts() {
        let ids = vec![Some(ClapId::new(100)), None, Some(ClapId::new(300))];
        let mut feed = BlockEvents::new(ids, CLAP_PORT);
        feed.set_param(2, 0.1);
        feed.set_param(0, 0.5);
        feed.set_param(2, 0.9);
        feed.set_param(1, 0.7); // read-only: accepted but never sent
        feed.set_param(3, 1.0); // out of range: ignored
        feed.set_param(0, f64::NAN); // not a value: ignored

        let queue = feed.fill(&[on(5, 60)], 0, 64, 64);
        assert_eq!(listing(queue), [(0, "param", 300, 0.9), (0, "param", 100, 0.5), (5, "on", 60, 1.0)]);

        // Sent once: the next block has no parameter events.
        assert_eq!(feed.fill(&[], 0, 64, 64).len(), 0);
        feed.set_param(0, 0.25);
        assert_eq!(listing(feed.fill(&[], 0, 64, 64)), [(0, "param", 100, 0.25)]);
    }

    #[test]
    fn all_notes_off_releases_exactly_the_held_keys() {
        let mut feed = BlockEvents::new(Vec::new(), CLAP_PORT);
        feed.fill(&[on(0, 60), on(0, 64), on(1, 67), off(2, 64)], 0, 64, 64);

        let flush = Event { offset: 9, kind: EventKind::AllNotesOff };
        assert_eq!(
            listing(feed.fill(&[flush, on(9, 72)], 0, 64, 64)),
            [(9, "off", 60, 0.0), (9, "off", 67, 0.0), (9, "on", 72, 1.0)]
        );
        // Nothing but the new note is held now.
        assert_eq!(listing(feed.fill(&[flush], 0, 64, 64)), [(9, "off", 72, 0.0)]);
        assert_eq!(feed.fill(&[flush], 0, 64, 64).len(), 0);
    }

    /// The MIDI messages in the queue, as `(time, bytes)`.
    fn midi(queue: &EventQueue) -> Vec<(u32, [u8; 3])> {
        (0..queue.len())
            .filter_map(|index| queue.get(index).unwrap().as_event::<MidiEvent>())
            .map(|event| (event.header().time(), event.data()))
            .collect()
    }

    #[test]
    fn the_wheels_and_the_pedal_go_to_a_port_that_takes_midi_as_midi() {
        let port = Some(NotePort { dialect: NoteDialect::Clap, midi: true });
        let mut feed = BlockEvents::new(Vec::new(), port);
        let at = |offset: u32, kind: EventKind| Event { offset, kind };
        let events = [
            on(0, 60),
            at(1, EventKind::Control { controller: 64, value: 1.0 }),
            at(2, EventKind::PitchBend { value: 1.0 }),
            at(3, EventKind::PitchBend { value: -1.0 }),
            at(4, EventKind::PitchBend { value: 0.0 }),
            at(5, EventKind::Control { controller: 1, value: 0.5 }),
            off(6, 60),
            at(7, EventKind::PitchBend { value: f32::NAN }),
        ];
        let queue = feed.fill(&events, 0, 64, 64);
        assert_eq!(
            midi(queue),
            [
                (1, [0xB0, 64, 127]),
                (2, [0xE0, 0x7F, 0x7F]),
                (3, [0xE0, 0, 0]),
                (4, [0xE0, 0, 64]),
                (5, [0xB0, 1, 64]),
                (7, [0xE0, 0, 64]),
            ]
        );
        // The plugin has the pedal, so the key comes up when it comes up.
        let notes: Vec<_> = listing(queue).into_iter().filter(|event| event.1 != "midi").collect();
        assert_eq!(notes, [(0, "on", 60, 1.0), (6, "off", 60, 0.0)]);
    }

    #[test]
    fn the_pedal_is_worked_out_for_a_port_that_takes_only_clap_notes() {
        let mut feed = BlockEvents::new(Vec::new(), CLAP_PORT);
        let pedal = |offset: u32, down: bool| Event {
            offset,
            kind: EventKind::Control { controller: 64, value: if down { 1.0 } else { 0.0 } },
        };
        let bend = Event { offset: 3, kind: EventKind::PitchBend { value: 0.5 } };
        let queue = feed.fill(&[on(0, 60), on(0, 64), pedal(1, true), off(2, 60), bend, on(4, 67)], 0, 64, 64);
        assert_eq!(
            listing(queue),
            [(0, "on", 60, 1.0), (0, "on", 64, 1.0), (4, "on", 67, 1.0)],
            "the key that came up is held, and nothing is sent that the port does not take"
        );
        // The same key again is that key's again; the pedal comes up and
        // lets go of what it alone was holding.
        let queue = feed.fill(&[off(0, 67), on(1, 67), pedal(5, false), off(9, 64)], 0, 64, 64);
        assert_eq!(listing(queue), [(1, "on", 67, 1.0), (5, "off", 60, 0.0), (9, "off", 64, 0.0)]);

        // All notes off ends what the pedal holds, and leaves it down.
        feed.fill(&[pedal(0, true), off(1, 67)], 0, 64, 64);
        let flush = Event { offset: 2, kind: EventKind::AllNotesOff };
        assert_eq!(listing(feed.fill(&[flush], 0, 64, 64)), [(2, "off", 67, 0.0)]);
        assert_eq!(listing(feed.fill(&[on(0, 72), off(1, 72)], 0, 64, 64)), [(0, "on", 72, 1.0)], "still down");
        assert_eq!(listing(feed.fill(&[pedal(0, false)], 0, 64, 64)), [(0, "off", 72, 0.0)]);
    }

    #[test]
    fn silencing_happens_once_before_the_blocks_own_notes() {
        let mut feed = BlockEvents::new(vec![Some(ClapId::new(1))], CLAP_PORT);
        feed.fill(&[on(0, 60)], 0, 64, 64);
        feed.silence();
        feed.set_param(0, 0.5);
        assert_eq!(
            listing(feed.fill(&[on(3, 62)], 0, 64, 64)),
            [(0, "param", 1, 0.5), (0, "off", 60, 0.0), (0, "choke", 60, 0.0), (3, "on", 62, 1.0)]
        );
        assert_eq!(feed.fill(&[], 0, 64, 64).len(), 0);

        // After deactivation there is nothing left to silence or release.
        feed.silence();
        feed.forget_notes();
        assert_eq!(feed.fill(&[Event { offset: 0, kind: EventKind::AllNotesOff }], 0, 64, 64).len(), 0);
    }

    #[test]
    fn the_notes_of_a_block_that_was_not_processed_arrive_with_the_next() {
        let mut feed = BlockEvents::new(Vec::new(), CLAP_PORT);
        feed.fill(&[on(0, 60)], 0, 64, 64);
        // The plugin was busy for two blocks: a key went down in one and
        // the first key was released in the other.
        feed.defer(&[on(40, 64)]);
        feed.defer(&[off(3, 60)]);
        assert_eq!(
            listing(feed.fill(&[on(9, 67)], 0, 64, 64)),
            [(0, "on", 64, 1.0), (0, "off", 60, 0.0), (9, "on", 67, 1.0)]
        );
        assert_eq!(feed.fill(&[], 0, 64, 64).len(), 0, "delivered once");
        // The keys that are down are known as if nothing had happened.
        let flush = Event { offset: 0, kind: EventKind::AllNotesOff };
        assert_eq!(listing(feed.fill(&[flush], 0, 64, 64)), [(0, "off", 64, 0.0), (0, "off", 67, 0.0)]);

        // More than can be kept is dropped rather than growing the buffer.
        let storage = feed.deferred.as_ptr();
        for _ in 0..MAX_NOTE_EVENTS + 10 {
            feed.defer(&[on(0, 60)]);
        }
        assert_eq!((feed.deferred.len(), feed.deferred.as_ptr()), (MAX_NOTE_EVENTS, storage));
    }

    #[test]
    fn a_plugin_without_a_note_port_gets_parameters_only() {
        let mut feed = BlockEvents::new(vec![Some(ClapId::new(1))], None);
        feed.set_param(0, 0.5);
        feed.silence();
        assert_eq!(listing(feed.fill(&[on(0, 60)], 0, 64, 64)), [(0, "param", 1, 0.5)]);
    }

    #[test]
    fn an_oversized_block_is_fed_in_pieces_without_losing_or_repeating_events() {
        let mut feed = BlockEvents::new(vec![Some(ClapId::new(1))], CLAP_PORT);
        feed.set_param(0, 0.5);
        // 100 frames, taken 40 at a time. The last event's offset lies
        // beyond the block.
        let events = [on(0, 60), on(39, 61), on(40, 62), off(79, 60), on(80, 63), off(250, 61)];
        assert_eq!(
            listing(feed.fill(&events, 0, 40, 100)),
            [(0, "param", 1, 0.5), (0, "on", 60, 1.0), (39, "on", 61, 1.0)]
        );
        assert_eq!(listing(feed.fill(&events, 40, 40, 100)), [(0, "on", 62, 1.0), (39, "off", 60, 0.0)]);
        assert_eq!(listing(feed.fill(&events, 80, 20, 100)), [(0, "on", 63, 1.0), (19, "off", 61, 0.0)]);
    }

    #[test]
    fn parameter_events_from_the_plugin_become_edits_by_list_position() {
        use clack_host::events::event_types::{ParamGestureBeginEvent, ParamGestureEndEvent};
        use clack_host::events::io::OutputEvents;

        let feedback = ParamFeedback::new(&[ClapId::new(100), ClapId::new(7)]);
        let mut output = ParamOutput(&feedback);
        let mut events = OutputEvents::from_buffer(&mut output);
        let whole = Pckn::match_all();
        events.try_push(ParamGestureBeginEvent::new(0, ClapId::new(7))).unwrap();
        events.try_push(ParamValueEvent::new(3, ClapId::new(7), whole, 0.25)).unwrap();
        events.try_push(ParamValueEvent::new(4, ClapId::new(100), whole, -6.0)).unwrap();
        events.try_push(ParamGestureEndEvent::new(9, ClapId::new(7))).unwrap();
        // A value addressed the way plugins built on JUCE address theirs:
        // to zeros, where CLAP has -1 for "the whole parameter".
        let zeros = Pckn::new(0u16, 0u16, 0u16, 0u32);
        events.try_push(ParamValueEvent::new(0, ClapId::new(100), zeros, 3.0)).unwrap();
        // Neither of these says anything about a parameter of this device.
        events.try_push(ParamValueEvent::new(0, ClapId::new(999), whole, 1.0)).unwrap();
        events.try_push(NoteOnEvent::new(0, note_address(60), 1.0)).unwrap();

        let mut edits = Vec::new();
        assert!(!feedback.edits.drain(|index, edit| edits.push((index, edit))));
        let expected =
            [(1, Edit::Begin), (1, Edit::Value(0.25)), (0, Edit::Value(-6.0)), (1, Edit::End), (0, Edit::Value(3.0))];
        assert_eq!(edits, expected);
    }

    #[test]
    fn transport_follows_the_song() {
        // 120 BPM at 48 kHz: 24000 frames per beat.
        let playing = transport(&info(true, 9.5), 12_000);
        assert!(playing.flags.contains(TransportFlags::IS_PLAYING | TransportFlags::HAS_TEMPO));
        assert!(playing.flags.contains(TransportFlags::HAS_TIME_SIGNATURE));
        assert!(!playing.flags.contains(TransportFlags::IS_LOOP_ACTIVE));
        assert_eq!(playing.song_pos_beats.to_float(), 10.0);
        assert_eq!(playing.song_pos_seconds.to_float(), 5.0);
        assert_eq!(playing.tempo, 120.0);
        assert_eq!((playing.time_signature_numerator, playing.time_signature_denominator), (4, 4));
        assert_eq!((playing.bar_number, playing.bar_start.to_float()), (2, 8.0));

        // Bars follow the time signature: in 6/8 a bar is three beats long.
        let waltz = ProcessInfo {
            time_signature: trill_core::TimeSignature::new(6, 8).unwrap(),
            loop_range: Some((3.0, 9.0)),
            ..info(true, 9.5)
        };
        let looping = transport(&waltz, 12_000);
        assert_eq!((looping.time_signature_numerator, looping.time_signature_denominator), (6, 8));
        assert_eq!((looping.bar_number, looping.bar_start.to_float()), (3, 9.0));
        assert!(looping.flags.contains(TransportFlags::IS_LOOP_ACTIVE));
        assert_eq!((looping.loop_start_beats.to_float(), looping.loop_end_beats.to_float()), (3.0, 9.0));
        assert_eq!(looping.loop_end_seconds.to_float(), 4.5);

        let stopped = transport(&info(false, 9.5), 12_000);
        assert!(!stopped.flags.contains(TransportFlags::IS_PLAYING));
        assert_eq!(stopped.song_pos_beats.to_float(), 9.5, "a stopped song does not advance");
    }

    #[test]
    fn transport_survives_nonsense() {
        let broken = ProcessInfo {
            sample_rate: 0.0,
            tempo: f64::NAN,
            time_signature: Default::default(),
            playing: true,
            position_beats: f64::INFINITY,
            loop_range: None,
        };
        let event = transport(&broken, 100);
        assert_eq!(event.tempo, 120.0);
        assert_eq!(event.song_pos_beats.to_float(), 0.0);
    }
}
