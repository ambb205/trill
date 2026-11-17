//! What a plugin is told of the song with each block, whatever its format.
//!
//! Both formats hand a plugin the transport with every call: VST3 as a
//! `ProcessContext`, CLAP as a transport event. Each is filled from
//! [`told`], so the two say the same:
//!
//! * **where the song is in beats** (quarter notes), which is the block's
//!   position;
//! * **where it is in time**, from the start of the song, which is the
//!   song's own time and not the beats at the block's tempo: VST3 as
//!   samples, CLAP as seconds. The engine counts the frames the song has
//!   played, so this goes on by exactly a block's length from one block to
//!   the next for as long as the song plays on, through every change of
//!   tempo. A plugin that watches the time for a jump sees one when the
//!   song is moved or comes round its loop, and at no other time;
//! * **the tempo and the time signature** of the block. A block has one of
//!   each: it ends where either changes. Through a glide of the tempo a
//!   plugin is told a staircase, a step for every stretch of at most 256
//!   frames, each at the tempo that carries the song evenly from the
//!   beat it is at to the beat the next block will say (CLAP's "tempo
//!   increment" is nothing, which is how the song moves). So beats, tempo
//!   and time agree from block to block: a plugin that goes on from one
//!   block's position at that block's tempo is where the next block says
//!   the song is, to within the one frame a change of tempo falls in;
//! * **the bar the song is in**: where it began (and, in CLAP, which it
//!   is, from 0), counted through every time signature the song had;
//! * **the loop**, in beats and (CLAP) in the song's time, while there is
//!   one, and whether the song is playing.
//!
//! Neither format has a flag for "the song has just been moved". There is
//! no need of one for notes: the engine releases what sounds before the
//! song goes on from somewhere else. A plugin that keeps time of its own
//! (an arpeggiator, a delay, a groove) finds out by the position, which is
//! why the position must not move unless the song did.

use trill_core::device::ProcessInfo;

/// The transport of the block `info` as of `frame_offset` frames into it,
/// as a plugin can be told it: with a tempo that is one and a position
/// that is somewhere, whatever `info` says. A song that is not playing
/// stays where it is.
///
/// A host that hands a plugin a block in parts gives each part its own:
/// the song moves evenly through a block, and every bar in it is as long
/// as the block's time signature makes it.
pub(crate) fn told(info: &ProcessInfo, frame_offset: usize) -> ProcessInfo {
    let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
    let mut told = *info;
    told.tempo = if info.tempo.is_finite() && info.tempo > 0.0 { info.tempo } else { 120.0 };
    told.sample_rate = if info.sample_rate.is_finite() && info.sample_rate > 0.0 { info.sample_rate } else { 0.0 };
    told.position_beats = finite(info.position_beats);
    told.song.seconds = finite(info.song.seconds);
    told.song.bar_start = finite(info.song.bar_start);
    if !told.playing || told.sample_rate == 0.0 || frame_offset == 0 {
        return told;
    }
    told.position_beats += frame_offset as f64 * told.tempo / (60.0 * told.sample_rate);
    told.song.seconds += frame_offset as f64 / told.sample_rate;
    let per_bar = told.time_signature.beats_per_bar();
    if per_bar > 0.0 {
        // The bars that began since the block did. The small bias keeps a
        // position that is on a bar line, but for rounding, in the bar
        // that begins there.
        let begun = ((told.position_beats - told.song.bar_start) / per_bar + 1e-9).floor().max(0.0);
        told.song.bar += begun as u32;
        told.song.bar_start += begun * per_bar;
    }
    told
}

/// What the scripted plugins of both formats write down of the transport,
/// block by block, for the tests that ask an instance to.
#[cfg(test)]
pub(crate) mod written {
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    pub(crate) enum Format {
        Clap,
        Vst3,
    }

    /// What a plugin was told of the song with one block, in the terms the
    /// two formats share.
    #[derive(Copy, Clone, Debug, PartialEq)]
    pub(crate) struct Block {
        pub frames: usize,
        pub playing: bool,
        pub tempo: f64,
        pub signature: (u32, u32),
        pub beats: f64,
        /// The song's time. VST3 says it in samples, which are kept too.
        pub seconds: f64,
        pub samples: Option<i64>,
        pub bar_start: f64,
        /// Which bar it is, from 0: CLAP says, VST3 does not.
        pub bar: Option<u32>,
        /// The loop in beats, while the song loops; and in seconds, which
        /// CLAP says and VST3 does not.
        pub looping: Option<(f64, f64)>,
        pub loop_seconds: Option<(f64, f64)>,
        /// The count of frames that never jumps or stops.
        pub steady: i64,
    }

    type Written = HashMap<(Format, u64), Vec<Block>>;
    static WRITTEN: Mutex<Option<Written>> = Mutex::new(None);

    /// Has the scripted plugin instance with this serial write down every
    /// block it is given from now on.
    pub(crate) fn begin(format: Format, serial: u64) {
        WRITTEN.lock().unwrap().get_or_insert_default().entry((format, serial)).or_default();
    }

    /// Called by the scripted plugins with every block: kept if the
    /// instance was asked to write down what it is told.
    pub(crate) fn down(format: Format, serial: u64, block: impl FnOnce() -> Block) {
        if let Some(blocks) = WRITTEN.lock().unwrap().as_mut().and_then(|written| written.get_mut(&(format, serial))) {
            blocks.push(block());
        }
    }

    /// What the instance wrote down, which it then no longer has.
    pub(crate) fn taken(format: Format, serial: u64) -> Vec<Block> {
        WRITTEN.lock().unwrap().as_mut().and_then(|written| written.remove(&(format, serial))).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::told;
    use trill_core::TimeSignature;
    use trill_core::device::{ProcessInfo, SongTime};

    fn block() -> ProcessInfo {
        ProcessInfo {
            sample_rate: 48_000.0,
            tempo: 90.0,
            time_signature: TimeSignature::new(7, 8).unwrap(),
            playing: true,
            position_beats: 20.0,
            // A song that was faster before: at 90 from its start it would
            // be 13 1/3 seconds in. Its bars were of another length too.
            song: SongTime { seconds: 11.0, bar: 5, bar_start: 18.0, loop_seconds: Some((1.5, 9.25)) },
            loop_range: Some((2.0, 16.0)),
        }
    }

    #[test]
    fn a_plugin_is_told_the_songs_time_and_bar_and_not_what_the_blocks_tempo_makes_of_its_beat() {
        let told = told(&block(), 0);
        assert_eq!((told.position_beats, told.tempo), (20.0, 90.0));
        assert_eq!(told.song, block().song);
    }

    #[test]
    fn a_part_of_a_block_is_as_far_on_as_its_frames_in_beats_in_time_and_in_bars() {
        // 90 BPM at 48 kHz: 32000 frames to the beat. A bar of 7/8 is three
        // and a half beats: from beat 18 the next begins at 21.5.
        let later = told(&block(), 32_000);
        assert_eq!(later.position_beats, 21.0);
        assert!((later.song.seconds - (11.0 + 2.0 / 3.0)).abs() < 1e-12);
        assert_eq!((later.song.bar, later.song.bar_start), (5, 18.0));
        let later = told(&block(), 48_000);
        assert_eq!(later.position_beats, 21.5);
        assert_eq!((later.song.bar, later.song.bar_start), (6, 21.5), "on the bar line: the bar that begins there");
        let later = told(&block(), 5 * 32_000);
        assert_eq!((later.song.bar, later.song.bar_start), (7, 25.0));
        assert_eq!(later.song.loop_seconds, Some((1.5, 9.25)), "the loop is where it was");

        // A song that is not playing stays where it is.
        let stopped = told(&ProcessInfo { playing: false, ..block() }, 32_000);
        assert_eq!((stopped.position_beats, stopped.song), (20.0, block().song));
    }

    #[test]
    fn nonsense_is_not_passed_on() {
        let broken = ProcessInfo {
            sample_rate: f64::NAN,
            tempo: -3.0,
            position_beats: f64::INFINITY,
            song: SongTime { seconds: f64::NAN, bar: 0, bar_start: f64::NEG_INFINITY, loop_seconds: None },
            ..block()
        };
        let told = told(&broken, 100);
        assert_eq!((told.tempo, told.sample_rate, told.position_beats), (120.0, 0.0, 0.0));
        assert_eq!((told.song.seconds, told.song.bar_start), (0.0, 0.0));
    }
}
