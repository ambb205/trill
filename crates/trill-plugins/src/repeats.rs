//! What makes a plugin do the same thing twice that does not as it comes
//! ([`Repeating`]): what a render gives its own copy of the plugin when
//! it is asked to be one that can be compared with another.
//!
//! What is here was measured (November 2026, `trill-mcp`'s opt-in test
//! `the_same_song_rendered_again`):
//!
//! - The instruments of Ample Sound draw at random from a stream that all
//!   their instances in a process share: the guitars which recording of a
//!   note they play (`Cycle Mode Switch` 0, as they come), the bass the
//!   noise of the hand moving between two frets (`M-Fret Sound` 1), and a
//!   patch that has the fret buzz on, each buzz (`M-Buzz Toggle`). With
//!   the recordings taken in turn and the two noises off, sixteen notes
//!   on two pitches render to the same samples every time, on all four.
//!
//!   The stream begins at the same place in every process that has just
//!   started: as they come, the same song rendered in two such processes
//!   is the same samples (the same opt-in test, each render in a process
//!   of its own), and in one process it is not.
//!
//! - Superior Drummer 3 chooses for itself which recording a cymbal plays
//!   when it is struck again while its last hit still rings (0.375 s
//!   after, in what was measured), and it chooses anew every time it is
//!   made: one song's drums come out in one of two ways on a quiet
//!   machine, and in others on a busy one. What the two leave of one
//!   another is -26 dB of the drums of a whole song, all of it in the
//!   stretches where a cymbal is struck in quick succession; a hit by
//!   itself is the same recording every time, and so is the level, to
//!   0.01 dB. Nothing a host does decides it, and nothing was found that
//!   stops it. What was ruled out, each by making the same part again and
//!   again with one thing changed (`cargo run -p trill-plugins --example
//!   takes`): what the wait before a render asks of it (the same every
//!   time: each key at the first try, three times, none in silence), how
//!   long that wait goes on after it has answered, whether notes fall on
//!   one frame or 2048 apart, how fast it is played (as fast as it goes,
//!   or no faster than one, two or four times the song), the threads, and
//!   the rest of the song (the drums alone do it). With nothing asked of
//!   it at all before the song, it does it too.

use trill_core::device::Repeating;

/// Ample Sound's instruments, by the beginning of their class ids.
const AMPLE: &str = "vst3:ABCDEF019182FAEB41707364";
/// Superior Drummer 3.
const SUPERIOR_DRUMMER: &str = "vst3:565354534433617375706572696F7220";

/// What makes the plugin `uri` repeat itself, where that is known.
pub(crate) fn of(uri: &str) -> Option<Repeating> {
    if uri.eq_ignore_ascii_case(SUPERIOR_DRUMMER) {
        return Some(Repeating {
            what: "chooses for itself, anew every time it is made, which recording a cymbal plays when it is struck \
                   again while it still rings"
                .into(),
            ..Repeating::default()
        });
    }
    let ample = uri.get(..AMPLE.len()).is_some_and(|start| start.eq_ignore_ascii_case(AMPLE));
    ample.then(|| Repeating {
        what: "draws at random which recording of a note it plays".into(),
        settings: vec![("Cycle Mode Switch".into(), "1".into()), ("M-Fret Sound".into(), "0".into())],
        params: vec![("M-Buzz Toggle".into(), 0.0)],
        costs: "its recordings taken in turn and not at random, and without the noise of the fretting hand".into(),
        begins_alike: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sampled_guitars_that_draw_at_random_are_known_and_nothing_else_is() {
        let bass = of("vst3:ABCDEF019182FAEB41707364416D7274").expect("Ample Metal R");
        assert_eq!(bass.settings[0], ("Cycle Mode Switch".to_string(), "1".to_string()));
        assert!(of("vst3:abcdef019182faeb4170736441677066").is_some(), "whatever case the id is written in");
        // Not an amplifier of another maker whose ids begin alike, nor a synthesizer.
        assert_eq!(of("vst3:ABCDEF019182FAEB4E4453504E544858"), None);
        assert_eq!(of("vst3:56534558667350736572756D20320000"), None);
        // The drum sampler is known for what it does, and for nothing that stops it.
        let drums = of("vst3:565354534433617375706572696F7220").expect("Superior Drummer 3");
        assert!(!drums.can_be_stopped() && drums.costs.is_empty() && !drums.begins_alike && bass.can_be_stopped());
        assert_eq!(of("clap:org.surge-synth-team.surge-xt"), None);
    }
}
