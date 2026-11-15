//! The events and transport handed to a plugin with each block.
//!
//! Everything here is used on the audio thread: fixed capacity, no
//! allocation after construction.

use super::com::{interface, write_out};
use super::exclusive::Exclusive;
use crate::notes::{HeldKeys, events_in_part};
use daw_core::device::{self, EventKind, MAX_EVENTS_PER_BLOCK, ProcessInfo};
use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_ as flags;
use vst3::Steinberg::Vst::{
    Chord, Event, Event__type0, FrameRate, IEventList, IEventListTrait, NoteOffEvent, NoteOnEvent, ProcessContext,
};
use vst3::Steinberg::{int32, kInvalidArgument, kResultFalse, kResultOk, tresult};
use vst3::{Class, ComWrapper};

/// Notes go to the plugin's first event bus, on its first channel.
const NOTE_BUS: int32 = 0;
const NOTE_CHANNEL: i16 = 0;
/// The engine addresses notes by key; VST3's note ids are left unset.
const NO_NOTE_ID: int32 = -1;
/// The quietest note-on sent. Frameworks that turn VST3 events into MIDI
/// would make a note-off of anything that rounds to velocity 0.
const MIN_VELOCITY: f32 = 1.0 / 127.0;
/// Most note events delivered to a plugin in one block; more are dropped.
/// The engine never passes more.
const MAX_NOTE_EVENTS: usize = MAX_EVENTS_PER_BLOCK;
/// Events needed to silence a plugin on top of that: a note-off per key.
const MAX_SILENCE_EVENTS: usize = 128;

/// The events of one block, in the order they were added: the host's
/// implementation of `IEventList`.
///
/// The capacity is fixed: once full, further events are dropped instead of
/// growing the list on the audio thread.
pub(crate) struct EventList {
    events: Exclusive<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    /// A list that holds up to `capacity` events. One that holds none is
    /// what a plugin is given to put its own events into: this host does
    /// not use them, but plugins expect a list to be there.
    pub(crate) fn with_capacity(capacity: usize) -> ComWrapper<Self> {
        ComWrapper::new(Self { events: Exclusive::new(Vec::with_capacity(capacity)) })
    }

    /// Lets `edit` change the list. Returns `None`, with nothing changed,
    /// if a plugin is using the list at this very moment, which it has no
    /// business doing outside `process`.
    fn edit<R>(&self, edit: impl FnOnce(&mut Queue<'_>) -> R) -> Option<R> {
        self.events.try_with(|events| edit(&mut Queue { events }))
    }
}

/// The list while the host fills it.
struct Queue<'a> {
    events: &'a mut Vec<Event>,
}

impl Queue<'_> {
    fn push(&mut self, event: Event) -> bool {
        let room = self.events.len() < self.events.capacity();
        if room {
            self.events.push(event);
        }
        room
    }

    /// `velocity` is 0..=1; `position` is the event's place in the song.
    fn note_on(&mut self, time: u32, position: f64, key: u8, velocity: f32) {
        let velocity = if velocity.is_finite() { velocity.clamp(MIN_VELOCITY, 1.0) } else { 1.0 };
        let note = NoteOnEvent {
            channel: NOTE_CHANNEL,
            pitch: i16::from(key.min(127)),
            tuning: 0.0,
            velocity,
            length: 0,
            noteId: NO_NOTE_ID,
        };
        self.push(event(time, position, kNoteOnEvent as u16, Event__type0 { noteOn: note }));
    }

    fn note_off(&mut self, time: u32, position: f64, key: u8) {
        let note = NoteOffEvent {
            channel: NOTE_CHANNEL,
            pitch: i16::from(key.min(127)),
            velocity: 0.0,
            noteId: NO_NOTE_ID,
            tuning: 0.0,
        };
        self.push(event(time, position, kNoteOffEvent as u16, Event__type0 { noteOff: note }));
    }
}

fn event(time: u32, position: f64, kind: u16, data: Event__type0) -> Event {
    Event {
        busIndex: NOTE_BUS,
        sampleOffset: int32::try_from(time).unwrap_or(int32::MAX),
        ppqPosition: position,
        flags: 0,
        r#type: kind,
        __field0: data,
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.try_with(|events| events.len() as int32).unwrap_or(0)
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        let Ok(index) = usize::try_from(index) else { return kInvalidArgument };
        match self.events.try_with(|events| events.get(index).copied()).flatten() {
            Some(event) => {
                // SAFETY: `e` is not null, so it is the caller's `Event`.
                unsafe { write_out(e, event) };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `e` is not null, so it points to the caller's `Event`,
        // which is plain data that may be copied.
        let event = unsafe { e.read_unaligned() };
        match self.edit(|queue| queue.push(event)) {
            Some(true) => kResultOk,
            _ => kResultFalse,
        }
    }
}

/// Turns what the engine tells a device (note events, resets) into the
/// VST3 events of each block.
pub(crate) struct BlockEvents {
    /// Whether the plugin has an event bus to send notes to.
    takes_notes: bool,
    held: HeldKeys,
    silence: bool,
    list: ComWrapper<EventList>,
}

impl BlockEvents {
    pub(crate) fn new(takes_notes: bool) -> Self {
        Self {
            takes_notes,
            held: HeldKeys::default(),
            silence: false,
            list: EventList::with_capacity(MAX_NOTE_EVENTS + MAX_SILENCE_EVENTS),
        }
    }

    /// The list to pass to `process`. It is the same one for every block.
    pub(crate) fn list(&self) -> *mut IEventList {
        interface(&self.list)
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
    /// Returns whether this is the block that silences the plugin.
    ///
    /// The engine never hands over more than the plugin's maximum block
    /// size, so normally this is called once per block for all of it.
    pub(crate) fn fill(
        &mut self,
        info: &ProcessInfo,
        events: &[device::Event],
        (start, frames, total): (usize, usize, usize),
    ) -> bool {
        let Self { takes_notes, held, silence, list } = self;
        let filled = list.edit(|queue| {
            queue.events.clear();
            if !*takes_notes {
                return false;
            }
            let beats_per_frame = beats_per_frame(info);
            let position = |time: u32| song_position(info) + (start + time as usize) as f64 * beats_per_frame;

            // VST3 has no event that silences a plugin. What a host can do
            // is release every key it knows to be down.
            let silencing = std::mem::take(silence);
            if silencing {
                for key in held.take() {
                    queue.note_off(0, position(0), key);
                }
            }
            for (time, kind) in events_in_part(events, start, frames, total) {
                match kind {
                    EventKind::NoteOn { pitch, velocity } => {
                        held.press(pitch);
                        queue.note_on(time, position(time), pitch, velocity);
                    }
                    EventKind::NoteOff { pitch } => {
                        held.release(pitch);
                        queue.note_off(time, position(time), pitch);
                    }
                    EventKind::AllNotesOff => {
                        for key in held.take() {
                            queue.note_off(time, position(time), key);
                        }
                    }
                }
            }
            silencing
        });
        // If the list could not be touched, the block's events are lost,
        // but nothing is forgotten: a pending silence is still pending.
        filled.unwrap_or(false)
    }
}

fn tempo(info: &ProcessInfo) -> f64 {
    if info.tempo.is_finite() && info.tempo > 0.0 { info.tempo } else { 120.0 }
}

fn song_position(info: &ProcessInfo) -> f64 {
    if info.position_beats.is_finite() { info.position_beats } else { 0.0 }
}

/// How far the song moves with each frame: not at all while it is stopped.
fn beats_per_frame(info: &ProcessInfo) -> f64 {
    if info.playing && info.sample_rate.is_finite() && info.sample_rate > 0.0 {
        tempo(info) / (60.0 * info.sample_rate)
    } else {
        0.0
    }
}

/// The transport as of `frame_offset` frames into the block `info`
/// describes. `continuous_samples` counts the frames processed so far and,
/// unlike the song position, never jumps or stops.
pub(crate) fn process_context(info: &ProcessInfo, frame_offset: usize, continuous_samples: i64) -> ProcessContext {
    let tempo = tempo(info);
    let beats = song_position(info) + frame_offset as f64 * beats_per_frame(info);
    let sample_rate = if info.sample_rate.is_finite() && info.sample_rate > 0.0 { info.sample_rate } else { 0.0 };
    let signature = info.time_signature;
    let beats_per_bar = signature.beats_per_bar();
    let (cycle_start, cycle_end) = info.loop_range.unwrap_or((0.0, 0.0));

    let mut state =
        flags::kTempoValid | flags::kTimeSigValid | flags::kProjectTimeMusicValid | flags::kBarPositionValid;
    state |= flags::kContTimeValid;
    if info.playing {
        state |= flags::kPlaying;
    }
    if info.loop_range.is_some() {
        state |= flags::kCycleValid | flags::kCycleActive;
    }

    ProcessContext {
        // The SDK's flags are a C `int` on Windows and `unsigned` elsewhere.
        #[allow(clippy::unnecessary_cast)]
        state: state as u32,
        sampleRate: sample_rate,
        // The engine's tempo is constant over the whole song.
        projectTimeSamples: (beats * 60.0 / tempo * sample_rate).round() as i64,
        systemTime: 0,
        continousTimeSamples: continuous_samples,
        projectTimeMusic: beats,
        barPositionMusic: (beats / beats_per_bar).floor() * beats_per_bar,
        cycleStartMusic: cycle_start,
        cycleEndMusic: cycle_end,
        tempo,
        timeSigNumerator: int32::from(signature.numerator),
        timeSigDenominator: int32::from(signature.denominator),
        chord: Chord { keyNote: 0, rootNote: 0, chordMask: 0 },
        smpteOffsetSubframes: 0,
        frameRate: FrameRate { framesPerSecond: 0, flags: 0 },
        samplesToNextClock: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::ComPtr;

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

    fn on(offset: u32, pitch: u8) -> device::Event {
        device::Event { offset, kind: EventKind::NoteOn { pitch, velocity: 1.0 } }
    }

    fn off(offset: u32, pitch: u8) -> device::Event {
        device::Event { offset, kind: EventKind::NoteOff { pitch } }
    }

    /// The list the way a plugin reads it: each event as
    /// `(sample offset, "on" or "off", pitch, velocity)`.
    fn listing(list: *mut IEventList) -> Vec<(i32, &'static str, i16, f32)> {
        // SAFETY: `list` is the interface of a live `EventList`, the event
        // is a local variable, and the union is read as the variant its
        // type names.
        unsafe {
            let list = vst3::ComRef::from_raw(list).expect("a list");
            (0..list.getEventCount())
                .map(|index| {
                    let mut event: Event = std::mem::zeroed();
                    assert_eq!(list.getEvent(index, &mut event), kResultOk);
                    assert_eq!((event.busIndex, event.flags), (0, 0));
                    if u32::from(event.r#type) == kNoteOnEvent as u32 {
                        let note = event.__field0.noteOn;
                        assert_eq!((note.channel, note.noteId, note.length, note.tuning), (0, -1, 0, 0.0));
                        (event.sampleOffset, "on", note.pitch, note.velocity)
                    } else {
                        assert_eq!(u32::from(event.r#type), kNoteOffEvent as u32);
                        let note = event.__field0.noteOff;
                        assert_eq!((note.channel, note.noteId), (0, -1));
                        (event.sampleOffset, "off", note.pitch, note.velocity)
                    }
                })
                .collect()
        }
    }

    fn fill(feed: &mut BlockEvents, events: &[device::Event]) -> Vec<(i32, &'static str, i16, f32)> {
        feed.fill(&info(true, 0.0), events, (0, 64, 64));
        listing(feed.list())
    }

    #[test]
    fn notes_become_vst3_events_at_their_offsets() {
        let mut feed = BlockEvents::new(true);
        let quiet = device::Event { offset: 9, kind: EventKind::NoteOn { pitch: 200, velocity: 0.0001 } };
        let events = [on(3, 60), off(7, 60), quiet];
        assert_eq!(
            fill(&mut feed, &events),
            [(3, "on", 60, 1.0), (7, "off", 60, 0.0), (9, "on", 127, MIN_VELOCITY)],
            "the key is clamped and a velocity that would read as a note-off is avoided"
        );
        // The list is rebuilt for every block.
        assert_eq!(fill(&mut feed, &[]), []);
    }

    #[test]
    fn events_carry_their_position_in_the_song() {
        let mut feed = BlockEvents::new(true);
        // 120 BPM at 48 kHz: 24000 frames per beat.
        feed.fill(&info(true, 8.0), &[on(0, 60), on(12_000, 61)], (0, 24_000, 24_000));
        let position = |feed: &BlockEvents, index| {
            // SAFETY: the list holds that many events and `event` is a
            // local.
            unsafe {
                let list = vst3::ComRef::from_raw(feed.list()).unwrap();
                let mut event: Event = std::mem::zeroed();
                assert_eq!(list.getEvent(index, &mut event), kResultOk);
                event.ppqPosition
            }
        };
        assert_eq!((position(&feed, 0), position(&feed, 1)), (8.0, 8.5));

        feed.fill(&info(false, 8.0), &[on(12_000, 62)], (0, 24_000, 24_000));
        assert_eq!(position(&feed, 0), 8.0, "a stopped song does not advance");
    }

    #[test]
    fn all_notes_off_releases_exactly_the_held_keys() {
        let mut feed = BlockEvents::new(true);
        fill(&mut feed, &[on(0, 60), on(0, 64), on(1, 67), off(2, 64)]);

        let flush = device::Event { offset: 9, kind: EventKind::AllNotesOff };
        assert_eq!(
            fill(&mut feed, &[flush, on(9, 72)]),
            [(9, "off", 60, 0.0), (9, "off", 67, 0.0), (9, "on", 72, 1.0)]
        );
        // Nothing but the new note is held now.
        assert_eq!(fill(&mut feed, &[flush]), [(9, "off", 72, 0.0)]);
        assert_eq!(fill(&mut feed, &[flush]), []);
    }

    #[test]
    fn silencing_happens_once_before_the_blocks_own_notes() {
        let mut feed = BlockEvents::new(true);
        fill(&mut feed, &[on(0, 60), on(5, 64)]);
        feed.silence();
        assert!(feed.fill(&info(true, 0.0), &[on(3, 62)], (0, 64, 64)), "this block silences");
        assert_eq!(listing(feed.list()), [(0, "off", 60, 0.0), (0, "off", 64, 0.0), (3, "on", 62, 1.0)]);
        assert!(!feed.fill(&info(true, 0.0), &[], (0, 64, 64)));
        assert_eq!(listing(feed.list()), []);

        // After deactivation there is nothing left to silence or release.
        feed.silence();
        feed.forget_notes();
        let flush = device::Event { offset: 0, kind: EventKind::AllNotesOff };
        assert!(!feed.fill(&info(true, 0.0), &[flush], (0, 64, 64)));
        assert_eq!(listing(feed.list()), []);
    }

    #[test]
    fn a_plugin_without_an_event_bus_gets_no_events() {
        let mut feed = BlockEvents::new(false);
        feed.silence();
        assert_eq!(fill(&mut feed, &[on(0, 60)]), []);
    }

    #[test]
    fn an_oversized_block_is_fed_in_pieces_without_losing_or_repeating_events() {
        let mut feed = BlockEvents::new(true);
        // 100 frames, taken 40 at a time. The last event's offset lies
        // beyond the block.
        let events = [on(0, 60), on(39, 61), on(40, 62), off(79, 60), on(80, 63), off(250, 61)];
        let mut part = |start, frames| {
            feed.fill(&info(true, 0.0), &events, (start, frames, 100));
            listing(feed.list())
        };
        assert_eq!(part(0, 40), [(0, "on", 60, 1.0), (39, "on", 61, 1.0)]);
        assert_eq!(part(40, 40), [(0, "on", 62, 1.0), (39, "off", 60, 0.0)]);
        assert_eq!(part(80, 20), [(0, "on", 63, 1.0), (19, "off", 61, 0.0)]);
    }

    #[test]
    fn a_full_list_drops_events_instead_of_growing() {
        let list = EventList::with_capacity(4);
        let storage = list.events.try_with(|events| events.as_ptr()).unwrap();
        list.edit(|queue| {
            for key in 0..10 {
                queue.note_on(0, 0.0, key, 1.0);
            }
        });
        let (length, address) = list.events.try_with(|events| (events.len(), events.as_ptr())).unwrap();
        assert_eq!(length, 4);
        assert_eq!(address, storage, "the list was never reallocated");
    }

    #[test]
    fn a_plugin_can_add_events_only_where_there_is_room() {
        let list = EventList::with_capacity(1);
        let plugin: ComPtr<IEventList> = list.to_com_ptr().unwrap();
        let mut note = event(5, 0.0, kNoteOnEvent as u16, Event__type0 { noteOn: NoteOnEvent { ..empty_note() } });
        // SAFETY: every pointer passed is null or to a local `Event`.
        unsafe {
            assert_eq!(plugin.addEvent(&mut note), kResultOk);
            assert_eq!(plugin.addEvent(&mut note), kResultFalse, "full");
            assert_eq!(plugin.getEventCount(), 1);
            assert_eq!(plugin.addEvent(std::ptr::null_mut()), kInvalidArgument);
            assert_eq!(plugin.getEvent(0, std::ptr::null_mut()), kInvalidArgument);
            assert_eq!(plugin.getEvent(1, &mut note), kResultFalse);
            assert_eq!(plugin.getEvent(-1, &mut note), kInvalidArgument);
        }

        // The list plugins get for their own events takes none at all.
        let sink = EventList::with_capacity(0);
        let plugin: ComPtr<IEventList> = sink.to_com_ptr().unwrap();
        // SAFETY: `note` is a local `Event`.
        unsafe {
            assert_eq!(plugin.addEvent(&mut note), kResultFalse);
            assert_eq!(plugin.getEventCount(), 0);
        }
    }

    fn empty_note() -> NoteOnEvent {
        NoteOnEvent { channel: 0, pitch: 60, tuning: 0.0, velocity: 1.0, length: 0, noteId: -1 }
    }

    #[test]
    fn transport_follows_the_song() {
        // 120 BPM at 48 kHz: 24000 frames per beat.
        let playing = process_context(&info(true, 9.5), 12_000, 777);
        let has = |context: &ProcessContext, flag| context.state & flag as u32 != 0;
        assert!(has(&playing, flags::kPlaying) && has(&playing, flags::kTempoValid));
        assert!(has(&playing, flags::kTimeSigValid) && has(&playing, flags::kProjectTimeMusicValid));
        assert!(has(&playing, flags::kBarPositionValid) && has(&playing, flags::kContTimeValid));
        assert!(!has(&playing, flags::kCycleActive) && !has(&playing, flags::kCycleValid));
        assert_eq!(playing.projectTimeMusic, 10.0);
        assert_eq!(playing.projectTimeSamples, 240_000, "ten beats are five seconds");
        assert_eq!(playing.continousTimeSamples, 777);
        assert_eq!((playing.tempo, playing.sampleRate), (120.0, 48_000.0));
        assert_eq!((playing.timeSigNumerator, playing.timeSigDenominator), (4, 4));
        assert_eq!(playing.barPositionMusic, 8.0);

        // Bars follow the time signature: in 6/8 a bar is three beats long.
        let waltz = ProcessInfo {
            time_signature: daw_core::TimeSignature::new(6, 8).unwrap(),
            loop_range: Some((3.0, 9.0)),
            ..info(true, 9.5)
        };
        let looping = process_context(&waltz, 12_000, 0);
        assert_eq!((looping.timeSigNumerator, looping.timeSigDenominator), (6, 8));
        assert_eq!(looping.barPositionMusic, 9.0);
        assert!(has(&looping, flags::kCycleActive) && has(&looping, flags::kCycleValid));
        assert_eq!((looping.cycleStartMusic, looping.cycleEndMusic), (3.0, 9.0));

        let stopped = process_context(&info(false, 9.5), 12_000, 0);
        assert!(!has(&stopped, flags::kPlaying));
        assert_eq!(stopped.projectTimeMusic, 9.5, "a stopped song does not advance");
        assert_eq!(stopped.projectTimeSamples, 228_000);
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
        let context = process_context(&broken, 100, 0);
        assert_eq!(context.tempo, 120.0);
        assert_eq!(context.projectTimeMusic, 0.0);
        assert_eq!((context.projectTimeSamples, context.sampleRate), (0, 0.0));
    }
}
