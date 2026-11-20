//! Note bookkeeping every plugin format needs on the audio thread: which
//! keys are down on which channel, and which of a block's events belong to
//! which part of it.

use trill_core::Channel;
use trill_core::device::{Event, EventKind, Pedal};

/// The keys the plugin has been told are down, on each MIDI channel.
///
/// No plugin format has an "all notes off" that plugins reliably implement
/// (CLAP's wildcard note-offs are optional and wrappers around MIDI-based
/// frameworks mishandle them; VST3 has no such event at all), so the host
/// remembers what it started and releases each key explicitly, on the
/// channel it went down on.
#[derive(Default)]
pub(crate) struct HeldKeys {
    /// A bit per key, by [`Channel::index`].
    bits: [u128; Channel::COUNT],
}

impl HeldKeys {
    pub(crate) fn press(&mut self, channel: Channel, key: u8) {
        self.bits[channel.index()] |= 1 << key.min(127);
    }

    pub(crate) fn release(&mut self, channel: Channel, key: u8) {
        self.bits[channel.index()] &= !(1 << key.min(127));
    }

    /// Forgets every held key and returns them: the channels in order, and
    /// the keys of each lowest first.
    pub(crate) fn take(&mut self) -> impl Iterator<Item = (Channel, u8)> + use<> {
        let bits = std::mem::take(&mut self.bits);
        Channel::all().flat_map(move |channel| {
            let bits = bits[channel.index()];
            (0..128u8).filter(move |key| bits & (1 << key) != 0).map(move |key| (channel, key))
        })
    }
}

/// The sustain pedal of each channel, for a plugin that has to have it
/// worked out: a pedal holds the keys of its own channel and no other's.
#[derive(Default)]
pub(crate) struct Pedals {
    pedals: [Pedal; Channel::COUNT],
}

impl Pedals {
    /// The pedal of `channel`.
    pub(crate) fn of(&mut self, channel: Channel) -> &mut Pedal {
        &mut self.pedals[channel.index()]
    }

    /// Every note has ended: no pedal holds anything, and each stays where
    /// it is.
    pub(crate) fn clear(&mut self) {
        self.pedals.iter_mut().for_each(Pedal::clear);
    }
}

/// The events of a block that fall into its frames `start..start + frames`,
/// each with its offset from `start` and the channel it is on. The block is
/// `total` frames long and `events` are sorted by offset.
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
) -> impl Iterator<Item = (u32, Channel, EventKind)> + '_ {
    let end = start + frames;
    let last = frames.saturating_sub(1);
    events
        .iter()
        .filter(move |event| event.offset as usize >= start)
        // An offset beyond the block is a caller's mistake. Such an event
        // is delivered with the block's last frames rather than lost: a
        // dropped note-off would hang a note.
        .take_while(move |event| (event.offset as usize) < end || end >= total)
        .map(move |event| ((event.offset as usize - start).min(last) as u32, event.channel, event.kind))
}

/// What a scripted plugin that says what it was told on which channel was
/// told: the kinds `CHANNELS` of both formats.
#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Told {
    NoteOn = 1,
    NoteOff = 2,
    Choke = 3,
    Wheel = 4,
    Pressure = 5,
    Controller = 6,
    AllSoundOff = 7,
}

/// The sample such a plugin makes of something it was told on the channel
/// a person calls `channel` (1 to 16): which thing it was before the
/// point, and the channel in hundredths after it.
#[cfg(test)]
pub(crate) fn said(what: Told, channel: u8) -> f32 {
    f32::from(what as u8) + f32::from(channel) / 100.0
}

/// What such a plugin said in the block it wrote to `samples`: the frame,
/// what it was told there and the channel as a person calls it. (Two
/// things on one frame cannot be told apart: a test keeps them apart.)
#[cfg(test)]
pub(crate) fn heard(samples: &[f32]) -> Vec<(usize, Told, u8)> {
    let all =
        [Told::NoteOn, Told::NoteOff, Told::Choke, Told::Wheel, Told::Pressure, Told::Controller, Told::AllSoundOff];
    (samples.iter().enumerate())
        .filter(|(_, sample)| **sample != 0.0)
        .map(|(frame, sample)| {
            let what = all.into_iter().find(|what| *what as u8 == sample.floor() as u8);
            let what = what.unwrap_or_else(|| panic!("{sample} at frame {frame} says nothing"));
            (frame, what, ((sample - sample.floor()) * 100.0).round() as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(number: u8) -> Channel {
        Channel::new(number).expect("a channel")
    }

    #[test]
    fn held_keys_are_released_once_each_on_its_channel() {
        let mut keys = HeldKeys::default();
        let (first, sixth) = (Channel::FIRST, channel(6));
        keys.press(first, 60);
        keys.press(first, 0);
        keys.press(first, 127);
        keys.press(first, 64);
        keys.release(first, 64);
        keys.release(first, 5);
        // The same key on another channel is another key.
        keys.press(sixth, 60);
        keys.press(sixth, 64);
        keys.release(first, 60);
        keys.press(first, 60);
        keys.release(sixth, 64);
        let held: Vec<(u8, u8)> = keys.take().map(|(channel, key)| (channel.number(), key)).collect();
        assert_eq!(held, [(1, 0), (1, 60), (1, 127), (6, 60)]);
        assert_eq!(keys.take().count(), 0);
    }

    #[test]
    fn a_pedal_holds_the_keys_of_its_own_channel() {
        let mut pedals = Pedals::default();
        let (first, second) = (Channel::FIRST, channel(2));
        assert_eq!(pedals.of(second).set(true).count(), 0);
        assert!(pedals.of(first).key_up(60), "the first channel's pedal is up: its key ends");
        assert!(!pedals.of(second).key_up(60), "the second's is down and has the key");
        assert_eq!(pedals.of(first).set(false).count(), 0);
        assert_eq!(pedals.of(second).set(false).collect::<Vec<_>>(), [60]);
        // Cleared, a pedal that is down holds nothing and is still down.
        pedals.of(second).set(true).for_each(drop);
        assert!(!pedals.of(second).key_up(62));
        pedals.clear();
        assert!(pedals.of(second).is_down());
        assert_eq!(pedals.of(second).set(false).count(), 0);
    }

    fn on(offset: u32, pitch: u8) -> Event {
        Event::new(offset, EventKind::NoteOn { pitch, velocity: 1.0 })
    }

    /// Each event of the part as `(offset in the part, pitch)`.
    fn part(events: &[Event], start: usize, frames: usize, total: usize) -> Vec<(u32, u8)> {
        events_in_part(events, start, frames, total)
            .map(|(time, _, kind)| match kind {
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
    fn each_event_goes_with_exactly_one_part_and_says_its_channel() {
        // 100 frames, taken 40 at a time.
        let events = [on(0, 60), on(39, 61), on(40, 62), on(79, 63), on(80, 64), on(99, 65)];
        assert_eq!(part(&events, 0, 40, 100), [(0, 60), (39, 61)]);
        assert_eq!(part(&events, 40, 40, 100), [(0, 62), (39, 63)]);
        assert_eq!(part(&events, 80, 20, 100), [(0, 64), (19, 65)]);

        let wheel = Event::on(channel(3), 41, EventKind::PitchBend { value: 0.5 });
        let events = [on(0, 60), wheel];
        let channels: Vec<u8> = events_in_part(&events, 40, 40, 100).map(|(_, channel, _)| channel.number()).collect();
        assert_eq!(channels, [3]);
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
