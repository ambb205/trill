//! An instrument that gives its MIDI channels meanings of their own, as a
//! guitar with a string to a channel does: [`STRINGS`](super::STRINGS).
//!
//! Each channel is a string. A note is a plain tone on the string of its
//! channel, and what moves beside the notes moves one string only: the
//! pitch wheel of a channel bends that channel's notes by up to two
//! semitones and leaves the others where they are, the pressure and the
//! expression of a channel set how loud its notes are, and a channel's
//! pedal holds its own keys. The same key on two channels is two notes,
//! and a key comes up on the channel it went down on: one that is let go
//! of on another channel goes on sounding, which is how a test hears a
//! host that mixes its channels up.
//!
//! Everything is worked out frame by frame from what it is told, at the
//! frame it is told it: the same song is the same samples at any buffer
//! size. Nothing is allocated after it is made.

use trill_core::Channel;
use trill_core::device::{
    CONTROLLER_SUSTAIN, DeviceInstance, DeviceProcessor, Event, EventKind, PITCH_BEND_SEMITONES, ProcessInfo,
};

/// The controller that sets how loud a channel's notes are: expression.
const EXPRESSION: u8 = 11;
/// How many notes sound at once, over all the channels.
const VOICES: usize = 64;
/// The level of one note at full velocity, with nothing else said.
const LEVEL: f32 = 0.1;

#[derive(Copy, Clone, Default)]
struct Voice {
    sounding: bool,
    channel: usize,
    pitch: u8,
    velocity: f32,
    /// Where its tone is in its cycle, 0 to 1.
    phase: f64,
    /// Its key came up while the pedal of its channel was down.
    pedalled: bool,
}

/// What a channel was last told beside its notes.
#[derive(Copy, Clone)]
struct ChannelState {
    /// -1 to 1.
    wheel: f32,
    /// 0 to 1: the half of a note's level that pressure adds.
    pressure: f32,
    /// 0 to 1.
    expression: f32,
    pedal: bool,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self { wheel: 0.0, pressure: 0.0, expression: 1.0, pedal: false }
    }
}

struct Strings {
    sample_rate: f64,
    channels: [ChannelState; Channel::COUNT],
    voices: [Voice; VOICES],
}

/// The instrument, as a factory hands it out.
pub(super) fn instance(sample_rate: f64) -> DeviceInstance {
    let strings = Strings {
        sample_rate,
        channels: [ChannelState::default(); Channel::COUNT],
        voices: [Voice::default(); VOICES],
    };
    DeviceInstance { processor: Box::new(strings), params: Vec::new(), controller: None }
}

impl Strings {
    fn tell(&mut self, event: &Event) {
        let channel = event.channel.index();
        match event.kind {
            EventKind::NoteOn { pitch, velocity } => {
                // The key again on its own channel begins the note again.
                let same = |voice: &Voice| voice.sounding && voice.channel == channel && voice.pitch == pitch;
                let free = self.voices.iter().position(same).or_else(|| self.voices.iter().position(|v| !v.sounding));
                if let Some(at) = free {
                    self.voices[at] = Voice { sounding: true, channel, pitch, velocity, phase: 0.0, pedalled: false };
                }
            }
            EventKind::NoteOff { pitch } => {
                let pedal = self.channels[channel].pedal;
                for voice in self.voices.iter_mut().filter(|v| v.sounding && v.channel == channel && v.pitch == pitch) {
                    match pedal {
                        true => voice.pedalled = true,
                        false => voice.sounding = false,
                    }
                }
            }
            EventKind::AllNotesOff => self.voices.iter_mut().for_each(|voice| voice.sounding = false),
            EventKind::PitchBend { value } => self.channels[channel].wheel = value.clamp(-1.0, 1.0),
            EventKind::ChannelPressure { value } => self.channels[channel].pressure = value.clamp(0.0, 1.0),
            EventKind::Control { controller: EXPRESSION, value } => {
                self.channels[channel].expression = value.clamp(0.0, 1.0);
            }
            EventKind::Control { controller: CONTROLLER_SUSTAIN, value } => {
                let down = value >= 0.5;
                self.channels[channel].pedal = down;
                if !down {
                    for voice in self.voices.iter_mut().filter(|v| v.channel == channel && v.pedalled) {
                        voice.sounding = false;
                    }
                }
            }
            EventKind::Control { .. } => {}
        }
    }
}

impl DeviceProcessor for Strings {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate;
    }

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let mut next = 0;
        for frame in 0..left.len() {
            while next < events.len() && events[next].offset as usize <= frame {
                self.tell(&events[next]);
                next += 1;
            }
            let mut sum = 0.0f32;
            for voice in self.voices.iter_mut().filter(|voice| voice.sounding) {
                let state = &self.channels[voice.channel];
                let semitones =
                    f64::from(voice.pitch) - 69.0 + f64::from(state.wheel) * f64::from(PITCH_BEND_SEMITONES);
                let hertz = 440.0 * (semitones / 12.0).exp2();
                let level = voice.velocity * (0.5 + 0.5 * state.pressure) * state.expression * LEVEL;
                sum += level * (voice.phase * std::f64::consts::TAU).sin() as f32;
                voice.phase = (voice.phase + hertz / self.sample_rate).fract();
            }
            left[frame] = sum;
            right[frame] = sum;
        }
        // What is said beyond the block's last frame is not lost.
        for event in &events[next..] {
            self.tell(event);
        }
    }

    fn reset(&mut self) {
        self.channels = [ChannelState::default(); Channel::COUNT];
        self.voices = [Voice::default(); VOICES];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady() -> ProcessInfo {
        ProcessInfo {
            sample_rate: 48_000.0,
            tempo: 120.0,
            time_signature: Default::default(),
            playing: true,
            position_beats: 0.0,
            song: Default::default(),
            loop_range: None,
        }
        .steady()
    }

    /// What the instrument plays of `events` over `frames`, in blocks of
    /// `block` frames.
    fn played(events: &[Event], frames: usize, block: usize) -> Vec<f32> {
        let mut device = instance(48_000.0);
        let mut out = Vec::new();
        for start in (0..frames).step_by(block) {
            let length = block.min(frames - start);
            let inside: Vec<Event> = events
                .iter()
                .filter(|event| (start..start + length).contains(&(event.offset as usize)))
                .map(|event| Event { offset: event.offset - start as u32, ..*event })
                .collect();
            let (mut left, mut right) = (vec![0.0; length], vec![0.0; length]);
            device.processor.process(&steady(), &inside, &mut left, &mut right);
            assert_eq!(left, right);
            out.extend(left);
        }
        out
    }

    /// How many times `audio` crosses zero upwards: its pitch, over a
    /// stretch as long as this.
    fn cycles(audio: &[f32]) -> usize {
        audio.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count()
    }

    #[test]
    fn a_channels_wheel_bends_its_own_notes_and_a_key_comes_up_on_its_own_channel() {
        let (second, third) = (Channel::new(2).unwrap(), Channel::new(3).unwrap());
        let down = |channel, offset, pitch| Event::on(channel, offset, EventKind::NoteOn { pitch, velocity: 1.0 });
        // A (440 Hz) on the second channel alone: 440 cycles in a second.
        let plain = played(&[down(second, 0, 69)], 48_000, 480);
        assert!((439..=441).contains(&cycles(&plain)), "{}", cycles(&plain));
        // Its wheel all the way up is two semitones: 493.9 Hz. Another
        // channel's wheel is nothing to it.
        let bent =
            played(&[down(second, 0, 69), Event::on(second, 0, EventKind::PitchBend { value: 1.0 })], 48_000, 480);
        assert!((493..=495).contains(&cycles(&bent)), "{}", cycles(&bent));
        let other =
            played(&[down(second, 0, 69), Event::on(third, 0, EventKind::PitchBend { value: 1.0 })], 48_000, 480);
        assert_eq!(other, plain);

        // The key let go of on another channel goes on sounding; on its
        // own it stops.
        let up = |channel| Event::on(channel, 24_000, EventKind::NoteOff { pitch: 69 });
        let wrong = played(&[down(second, 0, 69), up(third)], 48_000, 480);
        assert_eq!(wrong, plain);
        let right = played(&[down(second, 0, 69), up(second)], 48_000, 480);
        assert!(right[24_000..].iter().all(|sample| *sample == 0.0) && right[..24_000] == plain[..24_000]);
        // The same key on two channels is two notes, and "all notes off"
        // ends them both.
        let two = played(&[down(second, 0, 69), down(third, 0, 69)], 4_800, 480);
        assert!(two.iter().zip(&plain).all(|(two, one)| (two - 2.0 * one).abs() < 1e-6));
        let off = Event::on(third, 2_400, EventKind::AllNotesOff);
        let ended = played(&[down(second, 0, 69), down(third, 0, 69), off], 4_800, 480);
        assert!(ended[2_400..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn the_same_events_are_the_same_samples_at_any_block_size() {
        let sixth = Channel::new(6).unwrap();
        let mut events = vec![
            Event::new(7, EventKind::NoteOn { pitch: 60, velocity: 0.8 }),
            Event::on(sixth, 100, EventKind::NoteOn { pitch: 40, velocity: 1.0 }),
            Event::on(sixth, 3_000, EventKind::Control { controller: CONTROLLER_SUSTAIN, value: 1.0 }),
            Event::on(sixth, 4_000, EventKind::NoteOff { pitch: 40 }),
            Event::on(sixth, 9_000, EventKind::Control { controller: CONTROLLER_SUSTAIN, value: 0.0 }),
        ];
        for step in 0..100u32 {
            let value = step as f32 / 100.0;
            events.push(Event::on(sixth, 1_000 + step * 32, EventKind::PitchBend { value }));
            events.push(Event::on(sixth, 1_000 + step * 32, EventKind::ChannelPressure { value }));
            events
                .push(Event::new(1_000 + step * 32, EventKind::Control { controller: EXPRESSION, value: 1.0 - value }));
        }
        events.sort_by_key(|event| event.offset);
        let whole = played(&events, 12_000, 12_000);
        for block in [1, 17, 64, 480, 1_024] {
            assert_eq!(played(&events, 12_000, block), whole, "in blocks of {block}");
        }
    }

    #[test]
    fn a_channels_pedal_holds_its_own_keys() {
        let (second, third) = (Channel::new(2).unwrap(), Channel::new(3).unwrap());
        let pedal = |channel, offset, value| {
            Event::on(channel, offset, EventKind::Control { controller: CONTROLLER_SUSTAIN, value })
        };
        let key = |channel, offset, down: bool| match down {
            true => Event::on(channel, offset, EventKind::NoteOn { pitch: 69, velocity: 1.0 }),
            false => Event::on(channel, offset, EventKind::NoteOff { pitch: 69 }),
        };
        // The second channel's pedal is down when both keys come up: the
        // second's rings on until its pedal comes up, the third's ends.
        let events = [
            key(second, 0, true),
            key(third, 0, true),
            pedal(second, 100, 1.0),
            key(second, 1_000, false),
            key(third, 1_000, false),
            pedal(second, 3_000, 0.0),
        ];
        let heard = played(&events, 4_000, 480);
        let alone = played(&[key(second, 0, true)], 4_000, 480);
        assert_eq!(heard[1_000..3_000], alone[1_000..3_000], "one note is left, the second channel's");
        assert!(heard[3_000..].iter().all(|sample| *sample == 0.0));
    }
}
