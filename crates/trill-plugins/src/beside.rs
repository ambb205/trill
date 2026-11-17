//! What a plugin may be run beside when a render uses more than one thread
//! ([`Beside`]).
//!
//! The instances of a plugin share its module, and a module may keep
//! things for all of them: some keep where each instance is in a round of
//! recordings, or one stream of chances that every instance draws from.
//! Run at once, such instances draw from it in whatever order the threads
//! happen to come by, and the same song does not render to the same
//! samples twice even in a program that has just started. So unless a
//! plugin is known to keep nothing of the kind, its instances are run in
//! turn, in the order of the song, as one thread would run them; they are
//! still run beside the instances of every other plugin.
//!
//! What is known was measured, plugin by plugin, with the `beside`
//! example of this crate on the machine it was written on (November
//! 2026): three instances of the plugin on tracks of their own beside two
//! tracks of built-in devices, rendered on one thread three times, on
//! several with the instances in turn twice, and on several with the
//! instances at once twice. A plugin whose renders were the same samples
//! all seven times keeps nothing between its instances that what comes
//! out of them depends on, and is listed here as one to run beside
//! anything.
//!
//! What was found of the others, which are run in turn:
//!
//! - The instruments of Ample Sound give the same samples in turn and
//!   other ones at once (some 40 dB under the song): they share a stream
//!   that every instance draws from.
//! - Superior Drummer 3 does not give the same samples twice on one
//!   thread: two renders differ in the last bits (155 dB under the song).
//! - Surge XT, ValhallaVintageVerb, the modelled instruments of Audio
//!   Modeling (the SWAM ones, PolySWAM) and its Ambiente do not give the
//!   same sound twice at all, so nothing can be told of them sample for
//!   sample.
//! - Splice Sounds and Synthesizer V play nothing as they come.
//! - Archetype Cory Wong X took the process down while it was measured
//!   (as Neural DSP's amplifiers can when an instance is let go of young
//!   beside another: see the host's notes on taking plugins down). Its
//!   two siblings that were measured are listed; it is not.
//!
//! None of the plugins measured was found to need a render to itself, so
//! [`ALONE`] is empty: it is there for the one that does.

use std::path::Path;
use trill_core::device::Beside;

/// The plugins whose instances were measured to give the same samples run
/// at once as run one after the other, by the beginning of their URI.
const AT_ONCE: &[&str] = &[
    // Neural DSP: Archetype Gojira X, Archetype Tim Henson X.
    "vst3:ABCDEF019182FAEB4E4453504E474A58",
    "vst3:ABCDEF019182FAEB4E4453504E544858",
    // Xfer Records: Serum 2, Serum 2 FX.
    "vst3:56534558667350736572756D20320000",
    "vst3:56534558667351736572756D20322066",
    // Consort: its instruments, its amplifier and its hall.
    "vst3:ABCDEF019182FAEB436E7372",
    // Surge Synth Team: Surge XT Effects (the synthesizer is not listed).
    "clap:org.surge-synth-team.surge-xt-fx",
    "vst3:ABCDEF019182FAEB566D624153465854",
    // Valhalla DSP: ValhallaRoom, ValhallaSupermassive (ValhallaVintageVerb
    // does not give the same samples twice, and is not listed).
    "vst3:5653545275756D76616C68616C6C6172",
    "vst3:565354734D617376616C68616C6C6173",
    // JS80P.
    "vst3:00565354414D4A386A73383070000000",
    // WaveForge, WaveSynth.
    "clap:dev.waveforge.waveforge",
    "vst3:14225ACC5699DD03DD458D70221E0FCA",
    "clap:com.wavesynth.wavesynth",
    "vst3:ABCDEF019182FAEB5776737957767331",
];

/// The plugins that are not to be run beside anything, by the beginning
/// of their URI: a render of a song that has one is made on one thread.
const ALONE: &[&str] = &[];

/// What the plugin `uri`, which is in the file `module`, may be run beside.
pub(crate) fn of(uri: &str, module: &Path) -> Beside {
    let listed = |list: &[&str]| {
        list.iter().any(|known| uri.get(..known.len()).is_some_and(|start| start.eq_ignore_ascii_case(known)))
    };
    if listed(ALONE) {
        Beside::Nothing
    } else if listed(AT_ONCE) {
        Beside::Anything
    } else {
        Beside::InTurn(module.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_nothing_is_known_of_is_run_in_turn_with_the_other_instances_of_its_module() {
        let module = Path::new("C:/plugins/Unknown.vst3");
        let in_turn = Beside::InTurn(module.display().to_string());
        assert_eq!(of("vst3:0123456789ABCDEF0123456789ABCDEF", module), in_turn);
        assert_eq!(of("clap:com.example.unknown", module), of("clap:com.example.other", module));
        // A sampled guitar that shares a stream between its instances, and
        // a drum kit that does not do the same thing twice.
        assert_eq!(of("vst3:ABCDEF019182FAEB4170736441677066", module), in_turn);
        assert_eq!(of("vst3:565354534433617375706572696F7220", module), in_turn);
        // Nor is a name that only begins like a listed one after its scheme,
        // or one with letters that are more than one byte.
        assert_eq!(of("clap:org.surge-synth-team.surge-xt", module), in_turn);
        assert_eq!(of("vst3:ABCDÉF019182FAEB4E4453504E544858", module), in_turn);
    }

    #[test]
    fn a_plugin_that_was_measured_to_keep_nothing_between_its_instances_is_run_beside_anything() {
        let module = Path::new("C:/plugins/Known.vst3");
        // An amplifier, in whatever case its id is written; a synthesizer;
        // every plugin of a maker whose ids begin alike.
        assert_eq!(of("vst3:ABCDEF019182FAEB4E4453504E544858", module), Beside::Anything);
        assert_eq!(of("vst3:abcdef019182faeb4e4453504e544858", module), Beside::Anything);
        assert_eq!(of("vst3:56534558667350736572756D20320000", module), Beside::Anything);
        assert_eq!(of("vst3:ABCDEF019182FAEB436E737248617270", module), Beside::Anything);
        assert_eq!(of("clap:org.surge-synth-team.surge-xt-fx", module), Beside::Anything);
    }
}
