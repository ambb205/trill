//! A small library of drum grooves and fills, by style.
//!
//! Each groove is the pattern a drummer or producer in that style would
//! recognise as the starting point: where the kick and backbeat fall, what
//! the hats do, which notes are ghosted, and how much it swings. They are
//! written in the same lane notation callers use, so a groove can be taken
//! as it is or have single lanes replaced.

use crate::drums::DrumPattern;
use crate::error::{Error, Result, list};

/// A drum groove in a named style.
#[derive(Clone, Debug, PartialEq)]
pub struct Groove {
    pub name: &'static str,
    /// Other names the style goes by.
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    /// The time signature it is written in.
    pub signature: (u8, u8),
    /// The usual tempo range, in BPM.
    pub tempo: (u16, u16),
    /// The swing it is normally played with: percent, and the swung step in
    /// beats.
    pub swing: Option<(f64, f64)>,
    /// The built-in drum kit preset that suits it.
    pub kit: &'static str,
    /// Fills that suit it.
    pub fills: &'static [&'static str],
    lanes: &'static [(&'static str, &'static str)],
}

/// A one-bar drum fill.
#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub name: &'static str,
    pub description: &'static str,
    pub signature: (u8, u8),
    /// Where in the bar it takes over from the groove (0.5 = halfway).
    pub from: f64,
    lanes: &'static [(&'static str, &'static str)],
}

pub const GROOVES: &[Groove] = &[
    Groove {
        name: "house",
        aliases: &["four_on_the_floor", "four_to_the_floor", "disco"],
        description: "Four-on-the-floor: kick on every beat, clap on 2 and 4, open hat on every offbeat",
        signature: (4, 4),
        tempo: (118, 128),
        swing: Some((54.0, 0.25)),
        kit: "909",
        fills: &["build", "snare_roll"],
        lanes: &[
            ("kick", "X... X... X... X..."),
            ("clap", ".... X... .... X..."),
            ("closed_hat", "xg.g xg.g xg.g xg.g"),
            ("open_hat", "..x. ..x. ..x. ..x."),
        ],
    },
    Groove {
        name: "boom_bap",
        aliases: &["hip_hop", "hiphop", "lofi", "boombap"],
        description: "90s hip-hop: lazy swung kick pairs around a hard snare on 2 and 4, eighth-note hats",
        signature: (4, 4),
        tempo: (84, 96),
        swing: Some((58.0, 0.25)),
        kit: "lofi",
        fills: &["drop", "snare_roll"],
        lanes: &[
            ("kick", "X... ...x ..x. .... | X... ...x ..x. ...x"),
            ("snare", ".... X... .g.. X..."),
            ("closed_hat", "X.x. X.x. X.x. X.x. | X.x. X.x. X.x. X..."),
            ("open_hat", ".... .... .... .... | .... .... .... ..x."),
        ],
    },
    Groove {
        name: "trap",
        aliases: &[],
        description: "Half-time trap: clap on beat 3, sparse syncopated 808 kick, eighth hats breaking into rolls",
        signature: (4, 4),
        tempo: (130, 150),
        swing: None,
        kit: "trap",
        fills: &["trap_roll", "drop"],
        lanes: &[
            ("kick", "X... ...x ..x. .... | ..x. ...x ..x. ..x."),
            ("clap", ".... .... X... ...."),
            ("closed_hat", "X.x. X... X.x. x3_x. | X.x. X.x4_ X.x. x6___"),
            ("open_hat", ".... ..x. .... .... | .... .... .... ...."),
        ],
    },
    Groove {
        name: "drum_and_bass",
        aliases: &["dnb", "drum_n_bass", "jungle"],
        description: "Two-step drum and bass: kick on 1 and the and of 3, snare on 2 and 4, ghost notes around it",
        signature: (4, 4),
        tempo: (165, 176),
        swing: None,
        kit: "tight",
        fills: &["snare_roll", "build"],
        lanes: &[
            ("kick", "X... .... ..x. ...."),
            ("snare", ".... X... .... X... | .... X..g .g.. X..."),
            ("closed_hat", "x.x. x.x. x.x. x.x."),
        ],
    },
    Groove {
        name: "uk_garage",
        aliases: &["2step", "2_step", "two_step", "garage", "ukg"],
        description: "UK garage 2-step: the kick skips beat 3, clap on 2 and 4, heavily shuffled offbeat hats",
        signature: (4, 4),
        tempo: (128, 136),
        swing: Some((62.0, 0.25)),
        kit: "909",
        fills: &["drop", "snare_roll"],
        lanes: &[
            ("kick", "X... .... ..x. .... | X... ...x ..x. ...."),
            ("clap", ".... X... .... X..."),
            ("closed_hat", "..xg ..x. ..xg ..xg"),
            ("rim", ".... ...x .... .... | .... .... .x.. .x.."),
        ],
    },
    Groove {
        name: "reggaeton",
        aliases: &["dembow"],
        description: "Dembow: kick on every beat with the snare answering in a 3+3+2 pattern across it",
        signature: (4, 4),
        tempo: (88, 100),
        swing: None,
        kit: "tight",
        fills: &["toms", "snare_roll"],
        lanes: &[
            ("kick", "X... X... X... X..."),
            ("snare", "...x ..X. ...x ..X."),
            ("closed_hat", "x.x. x.x. x.x. x.x."),
        ],
    },
    Groove {
        name: "funk",
        aliases: &["breakbeat"],
        description: "Sixteenth-note funk: syncopated kick, backbeat with ghost notes, open hats on the upbeats",
        signature: (4, 4),
        tempo: (92, 112),
        swing: Some((54.0, 0.25)),
        kit: "tight",
        fills: &["toms", "snare_roll"],
        lanes: &[
            ("kick", "X.x. .... ..x. .x.."),
            ("snare", ".... X..g .g.g X..g"),
            ("closed_hat", "XxXx XxX. XxXx X.Xx"),
            ("open_hat", ".... ...x .... .x.."),
        ],
    },
    Groove {
        name: "rock",
        aliases: &["pop", "straight"],
        description: "Straight rock: kick on 1 and 3 with a push, snare on 2 and 4, driving eighth-note hats",
        signature: (4, 4),
        tempo: (100, 140),
        swing: None,
        kit: "default",
        fills: &["toms", "snare_roll", "build"],
        lanes: &[
            ("kick", "X... .... X.x. .... | X... ..x. X... ...."),
            ("snare", ".... X... .... X..."),
            ("closed_hat", "X.x. X.x. X.x. X.x."),
        ],
    },
    Groove {
        name: "half_time",
        aliases: &["halftime"],
        description: "Half-time: one backbeat a bar, on beat 3, so the groove feels half as fast as the tempo",
        signature: (4, 4),
        tempo: (120, 160),
        swing: None,
        kit: "default",
        fills: &["toms", "drop"],
        lanes: &[
            ("kick", "X... .... ..x. .... | X... ..x. .... ..x."),
            ("snare", ".... .... X... ...."),
            ("closed_hat", "x.x. x.x. x.x. x.x. | x.x. x.x. x.x. x..."),
            ("open_hat", ".... .... .... .... | .... .... .... ..x."),
        ],
    },
    Groove {
        name: "shuffle",
        aliases: &["blues", "blues_shuffle"],
        description: "Blues shuffle on a triplet grid: long-short hats, backbeat on 2 and 4 with a ghost before it",
        signature: (4, 4),
        tempo: (90, 130),
        swing: None,
        kit: "default",
        fills: &["triplet_toms"],
        lanes: &[("kick", "X.. ... x.. ..."), ("snare", "..g X.. ..g X.."), ("closed_hat", "X.x X.x X.x X.x")],
    },
    Groove {
        name: "six_eight",
        aliases: &["6/8", "68", "ballad_6_8"],
        description: "6/8: two big beats of three eighths each, kick on the first and snare on the second",
        signature: (6, 8),
        tempo: (80, 140),
        swing: None,
        kit: "default",
        fills: &["six_eight_toms"],
        lanes: &[("kick", "X.. ..x | X.x ..."), ("snare", "... X.."), ("closed_hat", "Xxx Xxx")],
    },
    Groove {
        name: "bossa_nova",
        aliases: &["bossa"],
        description: "Bossa nova: soft kick on 1 and the and of 2, steady eighth hats, the clave on the rim",
        signature: (4, 4),
        tempo: (110, 150),
        swing: None,
        kit: "tight",
        fills: &["snare_roll"],
        lanes: &[("kick", "x..x x..x"), ("closed_hat", "xxxx xxxx"), ("rim", "x..x ..x. | ..x. .x..")],
    },
    Groove {
        name: "jazz_swing",
        aliases: &["jazz", "swing"],
        description: "Jazz ride pattern on a triplet grid: ding, ding-a-ding, hi-hat foot on 2 and 4, feathered kick",
        signature: (4, 4),
        tempo: (110, 220),
        swing: None,
        kit: "default",
        fills: &["triplet_toms"],
        lanes: &[("ride", "x.. x.x x.. x.x"), ("pedal_hat", "... x.. ... x.."), ("kick", "g.. g.. g.. g..")],
    },
];

pub const FILLS: &[Fill] = &[
    Fill {
        name: "snare_roll",
        description: "Sixteenth-note snare roll through the last two beats, growing louder",
        signature: (4, 4),
        from: 0.5,
        lanes: &[("snare", ".... .... ggxx xxXX"), ("kick", ".... .... x... x...")],
    },
    Fill {
        name: "toms",
        description: "Down the toms in sixteenths over the last two beats",
        signature: (4, 4),
        from: 0.5,
        lanes: &[
            ("high_tom", ".... .... xx.. ...."),
            ("mid_tom", ".... .... ..xx ...."),
            ("low_tom", ".... .... .... xxxX"),
            ("kick", ".... .... x... x..."),
        ],
    },
    Fill {
        name: "build",
        description: "A whole-bar build: snare eighths, then sixteenths, then thirty-seconds, over a steady kick",
        signature: (4, 4),
        from: 0.0,
        lanes: &[("snare", "g.g. x.x. xxxx X2X2X2X2"), ("kick", "x... x... x... x...")],
    },
    Fill {
        name: "drop",
        description: "Everything stops on beat 4, then two snare hits lead back in",
        signature: (4, 4),
        from: 0.75,
        lanes: &[("snare", ".... .... .... ..xX")],
    },
    Fill {
        name: "trap_roll",
        description: "Hat rolls that tighten from triplets to thirty-seconds over the last two beats",
        signature: (4, 4),
        from: 0.5,
        lanes: &[
            ("closed_hat", ".... .... x3_x3_ x4_x6_"),
            ("clap", ".... .... X... ...."),
            ("snare", ".... .... .... ..xx"),
        ],
    },
    Fill {
        name: "triplet_toms",
        description: "Triplets down the toms over the last two beats, for shuffle and swing",
        signature: (4, 4),
        from: 0.5,
        lanes: &[("high_tom", "... ... xxx ..."), ("low_tom", "... ... ... xxX"), ("kick", "... ... x.. x..")],
    },
    Fill {
        name: "six_eight_toms",
        description: "Sixteenths down the toms through the second half of a 6/8 bar",
        signature: (6, 8),
        from: 0.5,
        lanes: &[
            ("high_tom", "...... xx...."),
            ("mid_tom", "...... ..xx.."),
            ("low_tom", "...... ....xX"),
            ("kick", "...... x....."),
        ],
    },
];

fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

impl Groove {
    /// Finds a groove by name or alias.
    pub fn find(name: &str) -> Result<&'static Groove> {
        let wanted = normalize(name);
        GROOVES.iter().find(|groove| groove.name == wanted || groove.aliases.contains(&wanted.as_str())).ok_or_else(
            || {
                Error::new(format!(
                    "'{name}' is not a groove; expected one of {}",
                    list(GROOVES.iter().map(|g| g.name))
                ))
            },
        )
    }

    /// The groove as an editable pattern.
    pub fn pattern(&self) -> DrumPattern {
        DrumPattern::parse(self.lanes.iter().copied()).expect("built-in grooves parse")
    }
}

impl Fill {
    pub fn find(name: &str) -> Result<&'static Fill> {
        let wanted = normalize(name);
        FILLS.iter().find(|fill| fill.name == wanted).ok_or_else(|| {
            Error::new(format!("'{name}' is not a fill; expected one of {}", list(FILLS.iter().map(|fill| fill.name))))
        })
    }

    pub fn pattern(&self) -> DrumPattern {
        DrumPattern::parse(self.lanes.iter().copied()).expect("built-in fills parse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::Level;
    use trill_core::Note;
    use trill_core::pitch::parse_drum;

    fn bar_length(signature: (u8, u8)) -> f64 {
        signature.0 as f64 * 4.0 / signature.1 as f64
    }

    /// The starts of one drum over the groove's own length.
    fn starts(name: &str, drum: &str) -> Vec<f64> {
        let groove = Groove::find(name).unwrap();
        let pattern = groove.pattern();
        hits(&pattern.notes(pattern.bars(), bar_length(groove.signature), 1.0), drum, 0)
    }

    /// Starts of a drum's hits at or above a velocity.
    fn hits(notes: &[Note], drum: &str, at_least: u8) -> Vec<f64> {
        let pitch = parse_drum(drum).unwrap();
        notes.iter().filter(|note| note.pitch == pitch && note.velocity >= at_least).map(|note| note.start).collect()
    }

    fn backbeats(name: &str, drum: &str) -> Vec<f64> {
        let groove = Groove::find(name).unwrap();
        let pattern = groove.pattern();
        let loud = Level::Accent.velocity(95.0);
        hits(&pattern.notes(1, bar_length(groove.signature), 1.0), drum, loud)
    }

    #[test]
    fn every_groove_is_well_formed() {
        for groove in GROOVES {
            let pattern = groove.pattern();
            let bars = pattern.bars();
            assert!((1..=2).contains(&bars), "{}", groove.name);
            assert!(pattern.odd_lanes(bar_length(groove.signature)).is_empty(), "{}", groove.name);
            for lane in &pattern.lanes {
                let steps = lane.pattern.steps_per_bar();
                assert!(steps.iter().all(|count| *count == steps[0]), "{} {}: uneven bars", groove.name, lane.drum);
                assert_eq!(bars % lane.pattern.bars(), 0, "{} {}", groove.name, lane.drum);
            }
            // Every lane of a groove sits on one grid.
            let grid = pattern.lanes[0].pattern.steps_per_bar()[0];
            assert!(pattern.lanes.iter().all(|lane| lane.pattern.steps_per_bar()[0] == grid), "{}", groove.name);
            assert!(groove.description.len() > 30 && groove.tempo.0 < groove.tempo.1, "{}", groove.name);
            assert!(["default", "808", "909", "tight", "trap", "lofi"].contains(&groove.kit), "{}", groove.name);
            for fill in groove.fills {
                assert_eq!(Fill::find(fill).unwrap().signature, groove.signature, "{} {fill}", groove.name);
            }
            // Something marks the downbeat.
            let notes = pattern.notes(1, bar_length(groove.signature), 1.0);
            assert!(notes.iter().any(|note| note.start == 0.0 && [36, 51].contains(&note.pitch)), "{}", groove.name);
            assert_eq!(Groove::find(groove.name).unwrap(), groove);
            for alias in groove.aliases {
                assert_eq!(Groove::find(alias).unwrap().name, groove.name);
            }
        }
    }

    #[test]
    fn the_backbeat_is_where_the_style_puts_it() {
        assert_eq!(backbeats("house", "clap"), [1.0, 3.0]);
        assert_eq!(backbeats("boom_bap", "snare"), [1.0, 3.0]);
        assert_eq!(backbeats("drum_and_bass", "snare"), [1.0, 3.0]);
        assert_eq!(backbeats("uk_garage", "clap"), [1.0, 3.0]);
        assert_eq!(backbeats("funk", "snare"), [1.0, 3.0]);
        assert_eq!(backbeats("rock", "snare"), [1.0, 3.0]);
        assert_eq!(backbeats("trap", "clap"), [2.0], "half-time: the backbeat is beat 3");
        assert_eq!(backbeats("half_time", "snare"), [2.0]);
        assert_eq!(backbeats("six_eight", "snare"), [1.5], "the second dotted beat");
        assert_eq!(backbeats("shuffle", "snare"), [1.0, 3.0]);
    }

    #[test]
    fn signature_rhythms_of_each_style() {
        // House: four on the floor, open hats exactly between the kicks.
        assert_eq!(starts("house", "kick"), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts("house", "open_hat"), [0.5, 1.5, 2.5, 3.5]);
        assert!(starts("house", "closed_hat").iter().all(|start| !starts("house", "open_hat").contains(start)));
        // Dembow: 3+3+2 against a straight kick.
        assert_eq!(starts("reggaeton", "kick"), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts("reggaeton", "snare"), [0.75, 1.5, 2.75, 3.5]);
        // Two-step styles leave beat 3 to the bass.
        for style in ["drum_and_bass", "uk_garage"] {
            let kicks = starts(style, "kick");
            assert!(kicks.contains(&0.0) && kicks.contains(&2.5), "{style}: {kicks:?}");
            assert!(!kicks.contains(&2.0) && !kicks.contains(&6.0), "{style}: {kicks:?}");
        }
        // Boom bap: the kick answers itself off the grid's strong points.
        assert_eq!(starts("boom_bap", "kick"), [0.0, 1.75, 2.5, 4.0, 5.75, 6.5, 7.75]);
        // Trap hats: eighths that break into a triplet roll and a faster one.
        let hats = starts("trap", "closed_hat");
        let gaps: Vec<f64> = hats.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.iter().any(|gap| (gap - 1.0 / 6.0).abs() < 1e-9), "a sixteenth-triplet roll: {gaps:?}");
        assert!(gaps.iter().any(|gap| (gap - 0.125).abs() < 1e-9), "a thirty-second roll: {gaps:?}");
        assert!(gaps.iter().filter(|gap| (**gap - 0.5).abs() < 1e-9).count() >= 8, "mostly eighths: {gaps:?}");
        // Funk: ghost notes outnumber the backbeats.
        let funk = Groove::find("funk").unwrap().pattern().notes(1, 4.0, 1.0);
        assert_eq!(hits(&funk, "snare", 0).len() - hits(&funk, "snare", 100).len(), 4);
        // Shuffle and swing live on the triplet grid.
        let third = 1.0 / 3.0;
        let shuffle = starts("shuffle", "closed_hat");
        assert_eq!(shuffle.len(), 8);
        assert!((shuffle[1] - 2.0 * third).abs() < 1e-9 && shuffle[2] == 1.0, "{shuffle:?}");
        let ride = starts("jazz_swing", "ride");
        let expected = [0.0, 1.0, 1.0 + 2.0 * third, 2.0, 3.0, 3.0 + 2.0 * third];
        assert!(ride.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-9) && ride.len() == 6, "{ride:?}");
        assert_eq!(starts("jazz_swing", "pedal_hat"), [1.0, 3.0]);
        // Bossa: the clave over two bars.
        assert_eq!(starts("bossa_nova", "rim"), [0.0, 1.5, 3.0, 5.0, 6.5]);
        assert_eq!(starts("bossa_nova", "kick"), [0.0, 1.5, 2.0, 3.5, 4.0, 5.5, 6.0, 7.5]);
        // 6/8: bars are three beats long.
        assert_eq!(starts("six_eight", "kick"), [0.0, 2.5, 3.0, 4.0]);
        assert_eq!(starts("six_eight", "closed_hat").len(), 12);
    }

    #[test]
    fn fills_are_well_formed_and_play_only_after_they_take_over() {
        for fill in FILLS {
            let pattern = fill.pattern();
            let length = bar_length(fill.signature);
            assert_eq!(pattern.bars(), 1, "{}", fill.name);
            assert!(pattern.odd_lanes(length).is_empty(), "{}", fill.name);
            assert!((0.0..1.0).contains(&fill.from), "{}", fill.name);
            let notes = pattern.notes(1, length, 1.0);
            assert!(!notes.is_empty(), "{}", fill.name);
            assert!(notes.iter().all(|note| note.start >= fill.from * length - 1e-9), "{}", fill.name);
            assert!(
                notes.iter().any(|note| note.start >= length * 0.75),
                "{} should lead into the next bar",
                fill.name
            );
            assert!(fill.description.len() > 20);
        }
        // The snare roll grows: no hit is quieter than the one before.
        let roll = Fill::find("snare_roll").unwrap().pattern().notes(1, 4.0, 1.0);
        let velocities: Vec<u8> = roll.iter().filter(|note| note.pitch == 38).map(|note| note.velocity).collect();
        assert_eq!(velocities.len(), 8);
        assert!(velocities.windows(2).all(|pair| pair[0] <= pair[1]), "{velocities:?}");
        // The build accelerates.
        let build = Fill::find("build").unwrap().pattern().notes(1, 4.0, 1.0);
        let snares = hits(&build, "snare", 0);
        assert_eq!(snares.len(), 4 + 4 + 8);
        assert_eq!(snares[15], 3.875);
    }

    #[test]
    fn unknown_names_list_the_choices() {
        let error = Groove::find("polka").unwrap_err();
        assert!(
            error.message().contains("'polka' is not a groove") && error.message().contains("uk_garage"),
            "{error}"
        );
        let error = Fill::find("solo").unwrap_err();
        assert!(error.message().contains("'solo' is not a fill") && error.message().contains("snare_roll"), "{error}");
        assert_eq!(Groove::find(" Drum-and-Bass ").unwrap().name, "drum_and_bass");
        assert_eq!(Groove::find("2step").unwrap().name, "uk_garage");
        assert_eq!(Fill::find("Snare Roll").unwrap().name, "snare_roll");
    }
}
