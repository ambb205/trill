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
//! Superior Drummer 3 does not repeat either (it chooses its hits for
//! itself), and no setting was found that stops it: it is not here.

use trill_core::device::Repeating;

/// Ample Sound's instruments, by the beginning of their class ids.
const AMPLE: &str = "vst3:ABCDEF019182FAEB41707364";

/// What makes the plugin `uri` repeat itself, where that is known.
pub(crate) fn of(uri: &str) -> Option<Repeating> {
    let ample = uri.get(..AMPLE.len()).is_some_and(|start| start.eq_ignore_ascii_case(AMPLE));
    ample.then(|| Repeating {
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
        assert_eq!(of("clap:org.surge-synth-team.surge-xt"), None);
    }
}
