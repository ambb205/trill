//! Live input: what is played right now, on a MIDI keyboard or through
//! [`Session::note_on`](crate::Session::note_on), on its way to the audio
//! thread, and what the audio thread sends back while a take is recorded.
//!
//! Whoever plays pushes a time-stamped event into one queue that never
//! blocks ([`LiveSender`]). The audio thread empties it at the start of
//! each callback and places every event in the block by its time stamp, so
//! that a key is heard a fixed moment after it went down however the
//! callbacks happen to fall. Nothing on that path locks or allocates.

use crate::clock;
use crate::queue::Queue;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use trill_core::TrackId;
use trill_core::device::EventKind;

/// Events waiting for the audio thread. A player manages a few hundred a
/// second with a pitch wheel in motion; the audio thread takes them all
/// every few milliseconds.
const QUEUE_CAPACITY: usize = 4096;
/// The most tracks live input reaches at once, and the most a take records.
pub const MAX_LIVE_TRACKS: usize = 16;

/// Where a live event is to be heard.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiveTarget {
    /// Wherever a keyboard goes: the armed tracks, or the selected track
    /// when none is armed.
    Routed,
    /// This track, armed or not.
    Track(TrackId),
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct LiveInput {
    /// When it was played, on [`clock::now_ns`] (or the clock of the host
    /// that drives the engine, in tests).
    pub time_ns: u64,
    pub target: LiveTarget,
    pub kind: EventKind,
}

/// The way in for live input. Cheap to clone; every clone feeds the same
/// engine, and keeps doing so when the engine changes its audio device.
/// Sending never blocks and never allocates, so it is safe from a MIDI
/// driver's callback.
#[derive(Clone)]
pub struct LiveSender {
    queue: Arc<Queue<LiveInput>>,
    /// Set when an event could not be queued. What was lost may have been
    /// a key coming up, so the audio thread then lets go of everything.
    overflowed: Arc<AtomicBool>,
}

impl LiveSender {
    pub(crate) fn new() -> Self {
        Self { queue: Arc::new(Queue::new(QUEUE_CAPACITY)), overflowed: Arc::new(AtomicBool::new(false)) }
    }

    pub(crate) fn queue(&self) -> Arc<Queue<LiveInput>> {
        self.queue.clone()
    }

    pub(crate) fn overflowed(&self) -> Arc<AtomicBool> {
        self.overflowed.clone()
    }

    fn push(&self, input: LiveInput) -> bool {
        let queued = self.queue.push(input);
        if !queued {
            self.overflowed.store(true, Ordering::Relaxed);
        }
        queued
    }

    /// Plays `kind`, now, wherever a keyboard goes: on the armed tracks,
    /// or on the selected track when none is armed. Returns `false` if the
    /// event had to be dropped because the audio thread is not keeping up;
    /// every note that sounds is then let go of, since the event that was
    /// lost may have been a key coming up.
    pub fn play(&self, kind: EventKind) -> bool {
        self.play_at(clock::now_ns(), kind)
    }

    /// [`LiveSender::play`] for an event that happened at `time_ns` on the
    /// engine's clock ([`crate::host::now_ns`]).
    pub fn play_at(&self, time_ns: u64, kind: EventKind) -> bool {
        self.push(LiveInput { time_ns, target: LiveTarget::Routed, kind })
    }

    /// Plays `kind`, now, on `track` whether or not it is armed.
    pub fn play_on(&self, track: TrackId, kind: EventKind) -> bool {
        self.play_on_at(clock::now_ns(), track, kind)
    }

    /// [`LiveSender::play_on`] for an event that happened at `time_ns`.
    pub fn play_on_at(&self, time_ns: u64, track: TrackId, kind: EventKind) -> bool {
        self.push(LiveInput { time_ns, target: LiveTarget::Track(track), kind })
    }
}

/// A handful of tracks, small enough to hand to the audio thread by value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackSet {
    tracks: [TrackId; MAX_LIVE_TRACKS],
    len: u8,
}

impl Default for TrackSet {
    fn default() -> Self {
        Self { tracks: [TrackId(0); MAX_LIVE_TRACKS], len: 0 }
    }
}

impl TrackSet {
    /// The first [`MAX_LIVE_TRACKS`] of `tracks`.
    pub fn of(tracks: &[TrackId]) -> Self {
        let mut set = Self::default();
        for &track in tracks.iter().take(MAX_LIVE_TRACKS) {
            if !set.contains(track) {
                set.tracks[set.len as usize] = track;
                set.len += 1;
            }
        }
        set
    }

    pub fn as_slice(&self) -> &[TrackId] {
        &self.tracks[..self.len as usize]
    }

    pub fn contains(&self, track: TrackId) -> bool {
        self.as_slice().contains(&track)
    }
}

/// What the audio thread reports while a take is being recorded.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Captured {
    /// The audio thread has taken up the take numbered `take`. What comes
    /// after is that take's; what came before was an earlier one's.
    Began { take: u32 },
    /// Something was played on a track that is being recorded. `beat` is
    /// where the song was *heard* to be when it was played: the engine's
    /// own position, less the output latency and the time the event took
    /// to reach its block. During a count-in that is before the take's
    /// start.
    Event { track: TrackId, kind: EventKind, beat: f64, pass: u32 },
    /// Where the hand had left a controller on a recorded track as the
    /// song started rolling (after [`Captured::Rolling`]): the take begins
    /// with it there. Only controllers that are away from rest, or have
    /// none, are reported.
    Standing { track: TrackId, kind: trill_core::ControllerKind, value: f32, pass: u32 },
    /// The take has begun: the count-in, if there was one, is over and the
    /// song is rolling from `beat`.
    Rolling { beat: f64 },
    /// The loop came round: `pass` is the pass now beginning.
    Wrapped { pass: u32 },
    /// The transport stopped at `beat`, and with it the take.
    Stopped { beat: f64, pass: u32 },
    /// Something happened to the take's audio when `at` frames of the
    /// recorded inputs had been handed to its files: the frame handed
    /// over next is the first that `mark` is true of.
    Audio { at: u64, mark: AudioMark },
}

/// What a take's audio is placed by: see [`Captured::Audio`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum AudioMark {
    /// The song began to roll from `beat`. The input frame handed over
    /// next was read as the song's first frame was made, which is heard
    /// `latency` frames later; the input was heard `delay` frames before
    /// it was read. So what was sung to the song's first frame is that
    /// many frames further on in the file.
    Rolling {
        beat: f64,
        latency: u32,
        delay: u32,
    },
    /// The song is no longer where the frames handed over lead to: it was
    /// at `from` and is at `to`. It came round its loop (`round`), or was
    /// sent there. What is handed over from here on was sung to another
    /// place of the song.
    Jumped {
        from: f64,
        to: f64,
        round: bool,
    },
    /// The song, at `beat`, came into the stretch the take is punched in
    /// over; and left it. Each time it does: a song that is sent back
    /// into the stretch comes into it again.
    PunchedIn {
        beat: f64,
    },
    PunchedOut {
        beat: f64,
    },
    /// The song stopped at `beat`, or the take was ended there.
    Stopped {
        beat: f64,
    },
    /// Nothing more is handed over.
    Ended,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_set_keeps_the_first_sixteen_once_each() {
        let many: Vec<TrackId> = (1..=40).map(TrackId).collect();
        let set = TrackSet::of(&many);
        assert_eq!(set.as_slice(), &many[..MAX_LIVE_TRACKS]);
        assert!(set.contains(TrackId(16)) && !set.contains(TrackId(17)));

        let repeated = TrackSet::of(&[TrackId(3), TrackId(3), TrackId(5)]);
        assert_eq!(repeated.as_slice(), [TrackId(3), TrackId(5)]);
        assert!(TrackSet::default().as_slice().is_empty());
    }

    #[test]
    fn senders_share_one_queue() {
        let sender = LiveSender::new();
        let other = sender.clone();
        assert!(sender.play_at(5, EventKind::NoteOn { pitch: 60, velocity: 1.0 }));
        assert!(other.play_on_at(7, TrackId(2), EventKind::NoteOff { pitch: 60 }));
        let queue = sender.queue();
        let first = queue.pop().unwrap();
        assert_eq!((first.time_ns, first.target), (5, LiveTarget::Routed));
        let second = queue.pop().unwrap();
        assert_eq!((second.time_ns, second.target), (7, LiveTarget::Track(TrackId(2))));
        assert!(queue.pop().is_none());
    }
}
