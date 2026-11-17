//! The tables that ship: each a recipe, made in code when the first
//! wavetable synth of a run is created, and kept for all of them.
//!
//! Their order is part of the file format: a song keeps the table an
//! oscillator plays as its number in [`NAMES`], so a table is only ever
//! added at the end, and none is taken out or moved.

use super::recipe::{Frame, Recipe, Shape};
use super::table::Table;
use std::sync::{Arc, OnceLock};

/// What the first choice of an oscillator's table is called: the table
/// the device's own state holds for that oscillator.
pub const CUSTOM: &str = "custom";

/// A table that ships, with what there is to know about it before it is
/// heard.
pub struct Factory {
    pub name: &'static str,
    /// What it sounds like, what moving through it does, and what it is
    /// for, in a sentence.
    pub about: &'static str,
    recipe: fn() -> Recipe,
}

impl Factory {
    /// The recipe the table is made from.
    pub fn recipe(&self) -> Recipe {
        let mut recipe = (self.recipe)();
        recipe.name = self.name.to_string();
        recipe
    }
}

fn of(shape: Shape) -> Frame {
    Frame::of(shape)
}

fn pulse(width: f64) -> Frame {
    Frame { width: Some(width), ..of(Shape::Pulse) }
}

/// Harmonics at a sawtooth's levels, with what is given changed.
fn spectrum(tilt: f64) -> Frame {
    Frame { tilt: Some(tilt), ..of(Shape::Spectrum) }
}

fn fm(ratio: f64, index: f64) -> Frame {
    Frame { ratio: Some(ratio), index: Some(index), ..of(Shape::Fm) }
}

fn harmonics(levels: &[f64]) -> Frame {
    Frame { harmonics: Some(levels.to_vec()), ..Frame::default() }
}

fn sized(size: usize, frames: Vec<Frame>) -> Recipe {
    Recipe::new("", size, frames)
}

/// A vowel on a sawtooth: its harmonics raised around the three places a
/// mouth resonates, given as harmonics of a low note (C2, 131 Hz).
fn vowel(formants: [f64; 3]) -> Frame {
    let levels: Vec<f64> = (1..=96)
        .map(|h| {
            let h = f64::from(h);
            let ring: f64 = formants
                .iter()
                .zip([1.0, 0.6, 0.35])
                .map(|(at, level)| level * (-((h / at).log2() * 3.2).powi(2)).exp())
                .sum();
            (0.06 + ring) / h.sqrt()
        })
        .collect();
    harmonics(&levels)
}

/// Every table that ships, in the order a song knows them by.
pub static TABLES: &[Factory] = &[
    Factory {
        name: "Basic Shapes",
        about: "The four plain waves and a thin pulse: sine at 0, triangle at 0.25, sawtooth at 0.5, square at \
                0.75, narrow pulse at 1. The place to start for anything analog.",
        recipe: || sized(5, vec![of(Shape::Sine), of(Shape::Triangle), of(Shape::Saw), of(Shape::Square), pulse(0.08)]),
    },
    Factory {
        name: "Sine to Saw",
        about: "A sine that gains harmonics until it is a full sawtooth: a low-pass filter drawn into the wave, \
                dark at 0 and bright at 1. For basses and pads that open without a filter.",
        recipe: || {
            sized(
                16,
                vec![Frame { cutoff: Some(1.0), ..spectrum(-6.0) }, Frame { cutoff: Some(512.0), ..spectrum(-6.0) }],
            )
        },
    },
    Factory {
        name: "Pulse Width",
        about: "A square that narrows to a thin pulse: full and hollow at 0, nasal and thin at 1. Move the \
                position slowly with an LFO for the classic pulse-width pad or bass.",
        recipe: || sized(16, vec![pulse(0.5), pulse(0.04)]),
    },
    Factory {
        name: "Saw to Square",
        about: "A sawtooth whose even harmonics fade until it is a square: buzzy at 0, hollow at 1.",
        recipe: || sized(9, vec![spectrum(-6.0), Frame { even: Some(0.0), ..spectrum(-6.0) }]),
    },
    Factory {
        name: "Fold",
        about: "A sine folded back on itself more and more: pure at 0, then growing a hard metallic edge. For \
                growling basses and leads that bite as the position rises.",
        recipe: || {
            sized(
                16,
                vec![Frame { amount: Some(0.0), ..of(Shape::Fold) }, Frame { amount: Some(1.0), ..of(Shape::Fold) }],
            )
        },
    },
    Factory {
        name: "Sync Sweep",
        about: "A sine restarted up to eight times a cycle: the tearing, vowel-like rise of oscillator sync, low \
                at 0 and screaming at 1. For leads; sweep it with an envelope.",
        recipe: || {
            sized(
                32,
                vec![Frame { ratio: Some(1.0), ..of(Shape::Sync) }, Frame { ratio: Some(8.0), ..of(Shape::Sync) }],
            )
        },
    },
    Factory {
        name: "Acid",
        about: "A sawtooth with a resonant filter sweep drawn into it: closed and round at 0, the peak rising \
                through the harmonics to wide open at 1. A squelching bass or pluck from position alone.",
        recipe: || {
            sized(
                24,
                vec![
                    Frame { cutoff: Some(1.5), peak: Some(15.0), ..spectrum(-6.0) },
                    Frame { cutoff: Some(90.0), peak: Some(15.0), ..spectrum(-6.0) },
                ],
            )
        },
    },
    Factory {
        name: "Hollow Sweep",
        about: "A square wave with the same resonant sweep: woody and closed at 0, reedy and open at 1. For \
                plucks, clarinet-like leads and hollow basses.",
        recipe: || {
            sized(
                16,
                vec![
                    Frame { cutoff: Some(1.5), peak: Some(12.0), even: Some(0.0), ..spectrum(-6.0) },
                    Frame { cutoff: Some(70.0), peak: Some(12.0), even: Some(0.0), ..spectrum(-6.0) },
                ],
            )
        },
    },
    Factory {
        name: "FM Electric",
        about: "A sine bent by a sine of its own pitch, as an FM electric piano is made: round at 0, brassy and \
                bright at 1. For keys and bass whose brightness follows velocity or an envelope.",
        recipe: || sized(16, vec![fm(1.0, 0.0), fm(1.0, 5.0)]),
    },
    Factory {
        name: "FM Octave",
        about: "A sine bent by a sine an octave up: hollow and woody low down, hard and clavinet-like high up. \
                For plucked basses and keys.",
        recipe: || sized(16, vec![fm(2.0, 0.0), fm(2.0, 6.0)]),
    },
    Factory {
        name: "FM Glass",
        about: "A sine bent by a sine three times as fast: glassy odd harmonics that spread upwards with the \
                position. For bells, mallets and bright plucks.",
        recipe: || sized(16, vec![fm(3.0, 0.0), fm(3.0, 7.0)]),
    },
    Factory {
        name: "FM Metal",
        about: "A sine bent by a sine seven times as fast: a clangorous, metallic spectrum that thickens with the \
                position. For bells, gongs and harsh digital leads.",
        recipe: || sized(16, vec![fm(7.0, 0.0), fm(7.0, 5.0)]),
    },
    Factory {
        name: "Formant Sweep",
        about: "One vocal resonance climbing from the second harmonic to the thirtieth: 'oo' at 0 through 'ah' \
                to 'ee' at 1. For talking basses and leads; move it with an LFO or the mod wheel.",
        recipe: || {
            sized(
                24,
                vec![
                    Frame { at: Some(2.0), width: Some(0.6), ..of(Shape::Formant) },
                    Frame { at: Some(30.0), width: Some(0.12), ..of(Shape::Formant) },
                ],
            )
        },
    },
    Factory {
        name: "Vowels",
        about: "A voice singing A, E, I, O, U, one vowel every quarter of the way. For choirs, pads and vocal \
                leads; the vowels are truest around C2 to C4.",
        recipe: || {
            sized(
                17,
                vec![
                    vowel([5.6, 8.4, 19.0]),
                    vowel([3.6, 14.0, 19.5]),
                    vowel([2.2, 17.5, 23.0]),
                    vowel([3.4, 6.2, 19.5]),
                    vowel([2.5, 5.0, 18.5]),
                ],
            )
        },
    },
    Factory {
        name: "Organ",
        about: "Drawbar organ registrations: a single flute stop at 0, stops added up the octaves and fifths to \
                the full organ at 1.",
        recipe: || {
            sized(
                9,
                vec![
                    harmonics(&[1.0]),
                    harmonics(&[1.0, 0.8]),
                    harmonics(&[1.0, 0.8, 0.5, 0.6]),
                    harmonics(&[1.0, 0.9, 0.7, 0.7, 0.0, 0.5, 0.0, 0.5]),
                    harmonics(&[1.0, 1.0, 0.9, 0.8, 0.5, 0.7, 0.0, 0.7, 0.0, 0.4, 0.0, 0.4, 0.0, 0.0, 0.0, 0.5]),
                ],
            )
        },
    },
    Factory {
        name: "Harmonics",
        about: "One harmonic at a time, the first sixteen in order: a sine at 0, its octave, its twelfth, and so \
                on up. Step through it for bugle-like arpeggios, or sit between two for a pure bell tone.",
        recipe: || {
            let frames = (0..16)
                .map(|h| {
                    let mut levels = vec![0.0; 16];
                    levels[h] = 1.0;
                    harmonics(&levels)
                })
                .collect();
            sized(16, frames)
        },
    },
    Factory {
        name: "Glass",
        about: "A few far-apart high partials over a soft fundamental, shifting as the position moves: thin and \
                glittering. For glassy pads, bells and digital plucks.",
        recipe: || {
            sized(
                16,
                vec![
                    harmonics(&[1.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.3]),
                    harmonics(&[1.0, 0.0, 0.0, 0.4, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.25]),
                    harmonics(&[
                        1.0, 0.0, 0.3, 0.0, 0.0, 0.0, 0.45, 0.0, 0.0, 0.0, 0.0, 0.0, 0.35, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2,
                    ]),
                    harmonics(&[
                        1.0, 0.2, 0.0, 0.0, 0.0, 0.4, 0.0, 0.0, 0.0, 0.0, 0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.3, 0.0, 0.0,
                        0.0, 0.0, 0.0, 0.0, 0.25,
                    ]),
                ],
            )
        },
    },
    Factory {
        name: "Growl",
        about: "A sawtooth with a phaser's notches drawn across its harmonics, sliding as the position moves: \
                the moving, talking snarl of a growl bass. Sweep it with an LFO in time with the song.",
        recipe: || {
            sized(24, vec![Frame { comb: Some(0.012), ..spectrum(-4.5) }, Frame { comb: Some(0.22), ..spectrum(-4.5) }])
        },
    },
    Factory {
        name: "Grit",
        about: "Harmonics at uneven levels, drawn by lot: buzzing and digital, soft and dark at 0, harsh and \
                bright at 1. For dirty basses, chiptune leads and anything that should not sound analog.",
        recipe: || {
            sized(
                16,
                vec![
                    Frame { seed: Some(11), tilt: Some(-12.0), ..of(Shape::Noise) },
                    Frame { seed: Some(11), tilt: Some(-2.0), ..of(Shape::Noise) },
                ],
            )
        },
    },
    Factory {
        name: "Octaves",
        about: "A note with its octaves stacked on top: the fundamental alone at 0, then the octave, two and \
                three octaves up. Hollow and organ-like; for pure pads and for thickening another oscillator.",
        recipe: || {
            sized(
                13,
                vec![
                    harmonics(&[1.0]),
                    harmonics(&[1.0, 0.7]),
                    harmonics(&[1.0, 0.7, 0.0, 0.5]),
                    harmonics(&[1.0, 0.7, 0.0, 0.5, 0.0, 0.0, 0.0, 0.4]),
                ],
            )
        },
    },
    Factory {
        name: "Tine",
        about: "The tone of a struck metal tine: a round fundamental with a bell partial high above it that \
                grows with the position. For electric pianos and mallets; let velocity move the position.",
        recipe: || {
            sized(
                16,
                vec![
                    harmonics(&[1.0, 0.12, 0.03]),
                    harmonics(&[1.0, 0.3, 0.1, 0.03, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.12, 0.05]),
                    harmonics(&[1.0, 0.45, 0.2, 0.1, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.3, 0.14, 0.05]),
                ],
            )
        },
    },
    Factory {
        name: "Strings",
        about: "The soft, slightly nasal spectrum of bowed strings: mellow at 0, with more rosin and edge at 1. \
                For string pads with a few voices of unison; brighter and smoother than a raw sawtooth.",
        recipe: || {
            sized(
                16,
                vec![
                    Frame { cutoff: Some(6.0), peak: Some(4.0), ..spectrum(-7.5) },
                    Frame { cutoff: Some(40.0), peak: Some(5.0), ..spectrum(-5.5) },
                ],
            )
        },
    },
];

/// The names an oscillator's table is chosen by: [`CUSTOM`], then every
/// table of [`TABLES`] in order.
pub static NAMES: &[&str] = &[
    CUSTOM,
    "Basic Shapes",
    "Sine to Saw",
    "Pulse Width",
    "Saw to Square",
    "Fold",
    "Sync Sweep",
    "Acid",
    "Hollow Sweep",
    "FM Electric",
    "FM Octave",
    "FM Glass",
    "FM Metal",
    "Formant Sweep",
    "Vowels",
    "Organ",
    "Harmonics",
    "Glass",
    "Growl",
    "Grit",
    "Octaves",
    "Tine",
    "Strings",
];

/// The tables that ship, made.
pub struct Tables {
    tables: Vec<Table>,
}

impl Tables {
    /// The tables, made the first time they are asked for in a run of the
    /// program and shared from then on.
    pub fn get() -> Arc<Tables> {
        static MADE: OnceLock<Arc<Tables>> = OnceLock::new();
        MADE.get_or_init(|| Arc::new(Tables { tables: TABLES.iter().map(|table| table.recipe().table()).collect() }))
            .clone()
    }

    /// The table an oscillator's choice `index` names; `None` for the
    /// first choice, which is the device's own.
    #[inline]
    pub fn chosen(&self, index: usize) -> Option<&Table> {
        self.tables.get(index.checked_sub(1)?)
    }

    /// The table to play where none is to be had.
    #[inline]
    pub fn fallback(&self) -> &Table {
        &self.tables[0]
    }

    pub fn bytes(&self) -> usize {
        self.tables.iter().map(Table::bytes).sum()
    }
}

/// The table that ships under `name`, however it is capitalised.
pub fn named(name: &str) -> Option<(usize, &'static Factory)> {
    let wanted = name.trim();
    TABLES.iter().position(|table| table.name.eq_ignore_ascii_case(wanted)).map(|index| (index + 1, &TABLES[index]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wavetable::table::MAX_FRAMES;

    #[test]
    fn the_names_are_the_tables_in_their_order_after_the_devices_own() {
        assert_eq!(NAMES[0], CUSTOM);
        let names: Vec<&str> = TABLES.iter().map(|table| table.name).collect();
        assert_eq!(&NAMES[1..], names);
        // The order is the file format: these never move.
        assert_eq!((NAMES[1], NAMES[7], NAMES[22]), ("Basic Shapes", "Acid", "Strings"));
        assert_eq!(named("basic shapes").map(|(index, _)| index), Some(1));
        assert_eq!(named(" Acid ").map(|(index, table)| (index, table.name)), Some((7, "Acid")));
        assert!(named("custom").is_none() && named("nope").is_none());
    }

    #[test]
    fn every_table_that_ships_is_a_recipe_that_checks_and_says_what_it_is() {
        let made = Tables::get();
        let mut frames = 0;
        for (index, factory) in TABLES.iter().enumerate() {
            let recipe = factory.recipe();
            assert_eq!(recipe.check(), Ok(()), "{}", factory.name);
            assert_eq!(recipe.name, factory.name);
            assert!(factory.about.len() > 60 && factory.about.ends_with('.'), "{}", factory.name);
            let table = made.chosen(index + 1).unwrap();
            assert_eq!((table.name(), table.frames()), (factory.name, recipe.frame_count()));
            assert!(table.frames() <= MAX_FRAMES);
            frames += table.frames();
            // Every frame sounds, and reaches full level.
            for (frame, info) in table.info().iter().enumerate() {
                assert!(info.highest >= 1, "{} frame {frame} is silent", factory.name);
                let outline = table.outline(frame as f32 / (table.frames() - 1).max(1) as f32, 2048);
                let peak = outline.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                assert!((0.9..=1.02).contains(&peak), "{} frame {frame} peaks at {peak}", factory.name);
            }
            // A recipe is written and read back as itself, which is how a
            // model is shown one to start from.
            let json = serde_json::to_string(&recipe).unwrap();
            assert_eq!(serde_json::from_str::<Recipe>(&json).unwrap(), recipe, "{}", factory.name);
        }
        assert!(made.chosen(0).is_none() && made.chosen(NAMES.len()).is_none());
        // All of them together, in memory for as long as the program runs.
        let megabytes = made.bytes() as f64 / (1024.0 * 1024.0);
        println!("{} tables, {frames} frames, {megabytes:.1} MB", TABLES.len());
        assert!(megabytes < 46.0, "{megabytes:.1} MB");
    }

    #[test]
    fn the_tables_do_what_their_sentences_say() {
        let made = Tables::get();
        let table = |name: &str| made.chosen(named(name).unwrap().0).unwrap();
        // Brightness rises along the ones that say it does.
        for name in ["Sine to Saw", "Fold", "Sync Sweep", "Acid", "Hollow Sweep", "FM Electric", "FM Glass", "Grit"] {
            let info = table(name).info();
            assert!(
                info[info.len() - 1].centre > info[0].centre * 2.0,
                "{name}: {:?}",
                (info[0], info[info.len() - 1])
            );
        }
        let basic = table("Basic Shapes").info();
        assert!(basic[0].highest == 1 && basic[3].odd > 0.999 && (basic[2].odd - 0.75).abs() < 0.01);
        assert!(table("Saw to Square").info()[8].odd > 0.999);
        assert_eq!(table("Harmonics").info()[5].highest, 6);
        assert!(table("Pulse Width").info()[15].rms_db < table("Pulse Width").info()[0].rms_db - 6.0);
    }
}
