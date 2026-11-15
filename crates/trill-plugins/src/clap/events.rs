//! The events and transport handed to a plugin with each block.
//!
//! Everything here is used on the audio thread: fixed capacity, no
//! allocation after construction.

use crate::notes::{HeldKeys, events_in_part};
use clack_host::events::event_types::{
    MidiEvent, NoteChokeEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::io::InputEventBuffer;
use clack_host::events::{Event as _, EventFlags, EventHeader, Match, Pckn, UnknownEvent};
use clack_host::utils::{BeatTime, ClapId, SecondsTime};
use trill_core::device::{Event, EventKind, MAX_EVENTS_PER_BLOCK, ProcessInfo};

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
    silence: bool,
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
            silence: false,
            queue: EventQueue::with_capacity(count + MAX_NOTE_EVENTS + MAX_SILENCE_EVENTS),
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
        self.silence = false;
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

        for (time, kind) in events_in_part(events, start, frames, total) {
            match kind {
                EventKind::NoteOn { pitch, velocity } => {
                    self.held.press(pitch);
                    self.queue.note_on(port.dialect, time, pitch, velocity);
                }
                EventKind::NoteOff { pitch } => {
                    self.held.release(pitch);
                    self.queue.note_off(port.dialect, time, pitch);
                }
                EventKind::AllNotesOff => {
                    for key in self.held.take() {
                        self.queue.note_off(port.dialect, time, key);
                    }
                }
            }
        }
        &self.queue
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
