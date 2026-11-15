//! Note bookkeeping every plugin format needs on the audio thread: which
//! keys are down, and which of a block's events belong to which part of
//! it.

use daw_core::device::{Event, EventKind};

/// The keys the plugin has been told are down.
///
/// No plugin format has an "all notes off" that plugins reliably implement
/// (CLAP's wildcard note-offs are optional and wrappers around MIDI-based
/// frameworks mishandle them; VST3 has no such event at all), so the host
/// remembers what it started and releases each key explicitly.
#[derive(Default)]
pub(crate) struct HeldKeys {
    bits: u128,
}

impl HeldKeys {
    pub(crate) fn press(&mut self, key: u8) {
        self.bits |= 1 << key.min(127);
    }

    pub(crate) fn release(&mut self, key: u8) {
        self.bits &= !(1 << key.min(127));
    }

    /// Forgets every held key and returns them, lowest first.
    pub(crate) fn take(&mut self) -> impl Iterator<Item = u8> + use<> {
        let bits = std::mem::take(&mut self.bits);
        (0..128u8).filter(move |key| bits & (1 << key) != 0)
    }
}

/// The events of a block that fall into its frames `start..start + frames`,
/// each with its offset from `start`. The block is `total` frames long and
/// `events` are sorted by offset.
///
/// The engine never hands a device more than the plugin's maximum block
/// size, so normally there is one part and it is the whole block. A larger
/// block is fed to the plugin in parts, and this decides which part each
/// event goes with.
pub(crate) fn events_in_part(
    events: &[Event],
    start: usize,
    frames: usize,
    total: usize,
) -> impl Iterator<Item = (u32, EventKind)> + '_ {
    let end = start + frames;
    let last = frames.saturating_sub(1);
    events
        .iter()
        .filter(move |event| event.offset as usize >= start)
        // An offset beyond the block is a caller's mistake. Such an event
        // is delivered with the block's last frames rather than lost: a
        // dropped note-off would hang a note.
        .take_while(move |event| (event.offset as usize) < end || end >= total)
        .map(move |event| ((event.offset as usize - start).min(last) as u32, event.kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_keys_are_released_once() {
        let mut keys = HeldKeys::default();
        keys.press(60);
        keys.press(0);
        keys.press(127);
        keys.press(64);
        keys.release(64);
        keys.release(5);
        assert_eq!(keys.take().collect::<Vec<_>>(), [0, 60, 127]);
        assert_eq!(keys.take().count(), 0);
    }

    fn on(offset: u32, pitch: u8) -> Event {
        Event { offset, kind: EventKind::NoteOn { pitch, velocity: 1.0 } }
    }

    /// Each event of the part as `(offset in the part, pitch)`.
    fn part(events: &[Event], start: usize, frames: usize, total: usize) -> Vec<(u32, u8)> {
        events_in_part(events, start, frames, total)
            .map(|(time, kind)| match kind {
                EventKind::NoteOn { pitch, .. } => (time, pitch),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_block_that_fits_is_one_part_with_every_event() {
        let events = [on(0, 60), on(17, 61), on(63, 62)];
        assert_eq!(part(&events, 0, 64, 64), [(0, 60), (17, 61), (63, 62)]);
        assert_eq!(part(&[], 0, 64, 64), []);
    }

    #[test]
    fn each_event_goes_with_exactly_one_part() {
        // 100 frames, taken 40 at a time.
        let events = [on(0, 60), on(39, 61), on(40, 62), on(79, 63), on(80, 64), on(99, 65)];
        assert_eq!(part(&events, 0, 40, 100), [(0, 60), (39, 61)]);
        assert_eq!(part(&events, 40, 40, 100), [(0, 62), (39, 63)]);
        assert_eq!(part(&events, 80, 20, 100), [(0, 64), (19, 65)]);
    }

    #[test]
    fn an_offset_beyond_the_block_lands_on_its_last_frame() {
        let events = [on(10, 60), on(64, 61), on(250, 62)];
        assert_eq!(part(&events, 0, 64, 64), [(10, 60), (63, 61), (63, 62)]);
        // In a block fed in parts, only the last part takes it.
        assert_eq!(part(&events, 0, 32, 64), [(10, 60)]);
        assert_eq!(part(&events, 32, 32, 64), [(31, 61), (31, 62)]);
    }
}
