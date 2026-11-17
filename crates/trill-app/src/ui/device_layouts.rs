//! How the built-in devices lay their parameters out: which go in which
//! section, under what short label, and which display a section draws.
//!
//! A device describes its parameters as a flat list. This table, keyed by
//! device URI, is what turns that list into a panel a musician can read:
//! columns of named sections, in signal order. A device without an entry
//! (every third-party plugin, and a built-in device nobody has laid out
//! yet) gets the generic layout in `devices`.
//!
//! The tests at the bottom keep the table honest: every parameter of a
//! device that has a layout is placed exactly once, and nothing here names
//! a device or a parameter that does not exist.

/// The layout of one device's panel.
pub struct Layout {
    pub uri: &'static str,
    pub columns: &'static [Column],
}

/// A column of sections, top to bottom.
pub struct Column {
    pub width: f32,
    pub sections: &'static [Section],
}

/// A group of controls under a caps heading.
pub struct Section {
    /// Empty for a section that needs no heading.
    pub title: &'static str,
    /// A display above the controls, drawn from their values.
    pub display: Option<Display>,
    pub rows: &'static [Row],
}

/// One line of a section.
pub enum Row {
    /// A parameter as its kind dictates: a bar for a number, a chip for a
    /// switch.
    Param { id: &'static str, label: &'static str, format: Format },
    /// A parameter with a few states, as a row of chips with these labels.
    Choice { id: &'static str, labels: &'static [&'static str] },
    /// Several switches side by side.
    Toggles(&'static [(&'static str, &'static str)]),
    /// Chips that set a continuous parameter to useful values.
    Picks { id: &'static str, picks: &'static [(&'static str, f64)] },
    /// Groups of rows that share the space: a chip per group, and the rows
    /// of the chosen one.
    Pages(&'static [Page]),
    /// The track the device listens to in place of its own input, to
    /// choose among the tracks there are. Only for a device that takes a
    /// key; it is no parameter.
    Sidechain,
}

pub struct Page {
    pub label: &'static str,
    pub rows: &'static [Row],
}

/// How a bar words its value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    /// By the parameter's unit and range.
    Auto,
    /// A length in beats as a note value where it is one: `1/8D`.
    NoteValue,
    /// A MIDI note number by name: `C4`.
    NoteName,
}

/// What a section's display shows; the ids are the parameters it reads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Display {
    /// A filter's response. `shapes` maps the type parameter's choices;
    /// `slope` names the parameter that makes every shape twice as steep.
    Filter {
        kind: &'static str,
        shapes: &'static [Shape],
        cutoff: &'static str,
        resonance: &'static str,
        slope: Option<&'static str>,
    },
    /// An envelope's shape.
    Envelope { attack: &'static str, decay: &'static str, sustain: &'static str, release: &'static str },
    /// The equalizer's response. Fills whatever height the column has left.
    Eq,
    /// The compressor's input-to-output curve.
    Compressor,
    /// The saturator's waveshaper.
    Saturator,
    /// One cycle of the pump's gain.
    Pump,
    /// Two cycles of the tremolo's level, the left side and the right.
    Tremolo,
}

/// A filter type, as the display needs it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    LowPass,
    LowPass24,
    HighPass,
    BandPass,
    Notch,
    /// The filter is switched off: a flat response.
    Off,
}

pub const NARROW: f32 = 150.0;
/// A column that holds a display.
pub const WIDE: f32 = 156.0;
/// The equalizer's single column: room for five band chips in a row.
const EQ_WIDTH: f32 = 204.0;
/// A filter column whose type chips include "Off".
const FILTER_WITH_OFF: f32 = 178.0;

const fn p(id: &'static str, label: &'static str) -> Row {
    Row::Param { id, label, format: Format::Auto }
}

const fn beats(id: &'static str, label: &'static str) -> Row {
    Row::Param { id, label, format: Format::NoteValue }
}

const fn key(id: &'static str, label: &'static str) -> Row {
    Row::Param { id, label, format: Format::NoteName }
}

const fn section(title: &'static str, rows: &'static [Row]) -> Section {
    Section { title, display: None, rows }
}

const fn shown(title: &'static str, display: Display, rows: &'static [Row]) -> Section {
    Section { title, display: Some(display), rows }
}

const WAVES: &[&str] = &["Sin", "Tri", "Saw", "Sqr"];
/// How steep a cut of the equalizer is, per octave.
const SLOPES: &[&str] = &["12 dB", "24 dB", "48 dB"];

pub static LAYOUTS: &[Layout] = &[
    Layout {
        uri: "builtin:polysynth",
        columns: &[
            Column {
                width: NARROW,
                sections: &[
                    section(
                        "Oscillators",
                        &[
                            Row::Choice { id: "osc1_wave", labels: WAVES },
                            Row::Choice { id: "osc2_wave", labels: WAVES },
                            p("osc2_semitones", "Pitch"),
                            p("osc2_detune", "Detune"),
                            p("fm_amount", "FM"),
                            p("fm_env", "FM Env"),
                        ],
                    ),
                    section("Mix", &[p("osc_mix", "Osc 1 / 2"), p("sub_level", "Sub"), p("noise_level", "Noise")]),
                ],
            },
            Column {
                width: WIDE,
                sections: &[shown(
                    "Filter",
                    Display::Filter {
                        kind: "filter_type",
                        shapes: &[Shape::LowPass, Shape::LowPass24, Shape::HighPass, Shape::BandPass],
                        cutoff: "cutoff",
                        resonance: "resonance",
                        slope: None,
                    },
                    &[
                        Row::Choice { id: "filter_type", labels: &["LP 12", "LP 24", "HP", "BP"] },
                        p("cutoff", "Cutoff"),
                        p("resonance", "Reso"),
                        p("filter_env", "Env"),
                        p("key_track", "Key Track"),
                    ],
                )],
            },
            Column {
                width: WIDE,
                sections: &[
                    shown(
                        "Amp Envelope",
                        Display::Envelope { attack: "attack", decay: "decay", sustain: "sustain", release: "release" },
                        &[p("attack", "Attack"), p("decay", "Decay"), p("sustain", "Sustain"), p("release", "Release")],
                    ),
                    section("LFO", &[p("lfo_rate", "Rate"), p("lfo_pitch", "Vibrato"), p("lfo_cutoff", "Cutoff")]),
                ],
            },
            Column {
                width: WIDE,
                sections: &[
                    shown(
                        "Filter Envelope",
                        Display::Envelope {
                            attack: "filter_attack",
                            decay: "filter_decay",
                            sustain: "filter_sustain",
                            release: "filter_release",
                        },
                        &[
                            p("filter_attack", "Attack"),
                            p("filter_decay", "Decay"),
                            p("filter_sustain", "Sustain"),
                            p("filter_release", "Release"),
                        ],
                    ),
                    section(
                        "Unison",
                        &[p("unison", "Voices"), p("unison_detune", "Detune"), p("unison_spread", "Spread")],
                    ),
                ],
            },
            Column {
                width: NARROW,
                sections: &[
                    section(
                        "Voice",
                        &[
                            Row::Choice { id: "mono", labels: &["Poly", "Mono"] },
                            p("glide", "Glide"),
                            p("velocity_sens", "Velocity"),
                        ],
                    ),
                    section("Output", &[p("volume", "Volume")]),
                ],
            },
        ],
    },
    Layout {
        uri: "builtin:drumkit",
        columns: &[
            Column {
                width: NARROW,
                sections: &[
                    section(
                        "Kick",
                        &[
                            p("kick_pitch", "Pitch"),
                            p("kick_decay", "Decay"),
                            p("kick_punch", "Punch"),
                            p("kick_drive", "Drive"),
                            p("kick_level", "Level"),
                        ],
                    ),
                    section(
                        "Snare",
                        &[
                            p("snare_pitch", "Pitch"),
                            p("snare_decay", "Decay"),
                            p("snare_noise", "Noise"),
                            p("snare_level", "Level"),
                        ],
                    ),
                ],
            },
            Column {
                width: NARROW,
                sections: &[
                    section(
                        "Hats",
                        &[
                            p("hat_decay", "Closed"),
                            p("open_hat_decay", "Open"),
                            p("hat_tone", "Tone"),
                            p("hat_level", "Level"),
                        ],
                    ),
                    section("Toms", &[p("tom_decay", "Decay"), p("tom_level", "Level")]),
                    section("Cymbals", &[p("cymbal_decay", "Decay"), p("cymbal_level", "Level")]),
                ],
            },
            Column {
                width: NARROW,
                sections: &[
                    section("Percussion", &[p("clap_level", "Clap"), p("perc_level", "Perc")]),
                    section("Output", &[p("volume", "Volume")]),
                ],
            },
        ],
    },
    Layout {
        uri: "builtin:sampler",
        columns: &[
            Column {
                width: NARROW,
                sections: &[section(
                    "Playback",
                    &[
                        key("root", "Root"),
                        p("transpose", "Transpose"),
                        p("fine", "Fine"),
                        p("start", "Start"),
                        p("end", "End"),
                        Row::Toggles(&[("reverse", "Reverse"), ("one_shot", "One-Shot")]),
                    ],
                )],
            },
            Column {
                // Wide enough for the three loop modes side by side.
                width: WIDE,
                sections: &[
                    section(
                        "Loop",
                        &[
                            Row::Choice { id: "loop_mode", labels: &["Off", "Loop", "Ping Pong"] },
                            p("loop_start", "Start"),
                            p("loop_end", "End"),
                            p("loop_crossfade", "Crossfade"),
                        ],
                    ),
                    section(
                        "Output",
                        &[
                            p("velocity_sens", "Velocity"),
                            p("voices", "Voices"),
                            p("volume", "Volume"),
                            p("pan", "Pan"),
                        ],
                    ),
                ],
            },
            Column {
                width: FILTER_WITH_OFF,
                sections: &[shown(
                    "Filter",
                    Display::Filter {
                        kind: "filter_type",
                        shapes: &[Shape::Off, Shape::LowPass, Shape::LowPass24, Shape::HighPass, Shape::BandPass],
                        cutoff: "cutoff",
                        resonance: "resonance",
                        slope: None,
                    },
                    &[
                        Row::Choice { id: "filter_type", labels: &["Off", "LP 12", "LP 24", "HP", "BP"] },
                        p("cutoff", "Cutoff"),
                        p("resonance", "Reso"),
                        p("filter_env", "Env"),
                        p("key_track", "Key Track"),
                    ],
                )],
            },
            Column {
                width: WIDE,
                sections: &[shown(
                    "Amp Envelope",
                    Display::Envelope { attack: "attack", decay: "decay", sustain: "sustain", release: "release" },
                    &[p("attack", "Attack"), p("decay", "Decay"), p("sustain", "Sustain"), p("release", "Release")],
                )],
            },
            Column {
                width: WIDE,
                sections: &[shown(
                    "Filter Envelope",
                    Display::Envelope {
                        attack: "filter_attack",
                        decay: "filter_decay",
                        sustain: "filter_sustain",
                        release: "filter_release",
                    },
                    &[
                        p("filter_attack", "Attack"),
                        p("filter_decay", "Decay"),
                        p("filter_sustain", "Sustain"),
                        p("filter_release", "Release"),
                    ],
                )],
            },
        ],
    },
    Layout {
        uri: "builtin:delay",
        columns: &[
            Column {
                width: NARROW,
                sections: &[section(
                    "Time",
                    &[
                        Row::Toggles(&[("sync", "Sync"), ("ping_pong", "Ping Pong")]),
                        Row::Picks {
                            id: "time_beats",
                            picks: &[("1/16", 0.25), ("1/8", 0.5), ("1/8D", 0.75), ("1/4", 1.0)],
                        },
                        beats("time_beats", "Synced"),
                        p("time_ms", "Free"),
                        p("feedback", "Feedback"),
                    ],
                )],
            },
            Column {
                width: NARROW,
                sections: &[
                    section("Filter", &[p("low_cut", "Low Cut"), p("high_cut", "High Cut")]),
                    section("Output", &[p("mix", "Mix"), Row::Toggles(&[("legacy", "Legacy")])]),
                ],
            },
        ],
    },
    Layout {
        uri: "builtin:reverb",
        columns: &[
            Column {
                width: NARROW,
                sections: &[
                    section("Space", &[p("size", "Size"), p("decay", "Decay"), p("predelay", "Pre-delay")]),
                    section("Tail", &[p("damping", "Damping"), p("modulation", "Mod")]),
                ],
            },
            Column {
                width: NARROW,
                sections: &[
                    section("Input", &[p("low_cut", "Low Cut"), p("high_cut", "High Cut")]),
                    section("Output", &[p("width", "Width"), p("mix", "Mix"), Row::Toggles(&[("legacy", "Legacy")])]),
                ],
            },
        ],
    },
    Layout {
        uri: "builtin:eq",
        columns: &[Column {
            width: EQ_WIDTH,
            sections: &[shown(
                "",
                Display::Eq,
                &[Row::Pages(&[
                    Page {
                        label: "Cut",
                        rows: &[
                            p("low_cut", "Low Cut"),
                            Row::Choice { id: "low_cut_slope", labels: SLOPES },
                            p("high_cut", "High Cut"),
                            Row::Choice { id: "high_cut_slope", labels: SLOPES },
                        ],
                    },
                    Page {
                        label: "Low",
                        rows: &[p("low_freq", "Low Freq"), p("low_gain", "Low Gain"), p("low_q", "Low Q")],
                    },
                    Page {
                        label: "Mid 1",
                        rows: &[p("mid1_freq", "Mid 1 Freq"), p("mid1_gain", "Mid 1 Gain"), p("mid1_q", "Mid 1 Q")],
                    },
                    Page {
                        label: "Mid 2",
                        rows: &[p("mid2_freq", "Mid 2 Freq"), p("mid2_gain", "Mid 2 Gain"), p("mid2_q", "Mid 2 Q")],
                    },
                    Page {
                        label: "High",
                        rows: &[p("high_freq", "High Freq"), p("high_gain", "High Gain"), p("high_q", "High Q")],
                    },
                ])],
            )],
        }],
    },
    Layout {
        uri: "builtin:compressor",
        columns: &[
            Column {
                width: WIDE,
                sections: &[shown(
                    "Dynamics",
                    Display::Compressor,
                    &[
                        Row::Choice { id: "detector", labels: &["Peak", "Bus"] },
                        p("threshold", "Threshold"),
                        p("ratio", "Ratio"),
                        p("knee", "Knee"),
                        p("attack", "Attack"),
                        p("release", "Release"),
                        Row::Toggles(&[("auto_release", "Auto Release")]),
                    ],
                )],
            },
            Column {
                width: WIDE,
                sections: &[
                    section("Sidechain", &[Row::Sidechain, p("key_low_cut", "Low Cut"), p("key_high_cut", "High Cut")]),
                    section(
                        "Output",
                        &[p("makeup", "Makeup"), Row::Toggles(&[("auto_makeup", "Auto Makeup")]), p("mix", "Mix")],
                    ),
                ],
            },
        ],
    },
    Layout {
        uri: "builtin:limiter",
        columns: &[Column {
            width: NARROW,
            sections: &[section("Limiter", &[p("gain", "Input"), p("ceiling", "Ceiling"), p("release", "Release")])],
        }],
    },
    Layout {
        uri: "builtin:chorus",
        columns: &[Column {
            width: NARROW,
            sections: &[
                section("Modulation", &[p("rate", "Rate"), p("depth", "Depth"), p("feedback", "Feedback")]),
                section("Output", &[p("width", "Width"), p("mix", "Mix")]),
            ],
        }],
    },
    Layout {
        uri: "builtin:filter",
        columns: &[Column {
            width: WIDE,
            sections: &[
                shown(
                    "Filter",
                    Display::Filter {
                        kind: "type",
                        shapes: &[Shape::LowPass, Shape::HighPass, Shape::BandPass, Shape::Notch],
                        cutoff: "cutoff",
                        resonance: "resonance",
                        slope: Some("slope"),
                    },
                    &[
                        Row::Choice { id: "type", labels: &["LP", "HP", "BP", "Notch"] },
                        Row::Choice { id: "slope", labels: &["12 dB", "24 dB"] },
                        p("cutoff", "Cutoff"),
                        p("resonance", "Reso"),
                    ],
                ),
                section("LFO", &[p("lfo_rate", "Rate"), p("lfo_amount", "Amount")]),
            ],
        }],
    },
    Layout {
        uri: "builtin:tremolo",
        columns: &[Column {
            width: WIDE,
            sections: &[shown(
                "Tremolo",
                Display::Tremolo,
                &[
                    Row::Choice { id: "shape", labels: &["Sine", "Tri", "Square"] },
                    Row::Toggles(&[("sync", "Sync")]),
                    p("rate", "Rate"),
                    beats("rate_beats", "Synced"),
                    p("depth", "Depth"),
                    p("spread", "Spread"),
                ],
            )],
        }],
    },
    Layout {
        uri: "builtin:pump",
        columns: &[Column {
            width: WIDE,
            sections: &[shown(
                "Pump",
                Display::Pump,
                &[beats("rate_beats", "Rate"), p("depth", "Depth"), p("curve", "Curve"), p("offset", "Offset")],
            )],
        }],
    },
    Layout {
        uri: "builtin:saturator",
        columns: &[Column {
            width: WIDE,
            sections: &[
                shown(
                    "Drive",
                    Display::Saturator,
                    &[Row::Choice { id: "shape", labels: &["Soft", "Hard", "Tube"] }, p("drive", "Drive")],
                ),
                section("Output", &[p("output", "Output"), p("mix", "Mix")]),
            ],
        }],
    },
    Layout {
        uri: "builtin:utility",
        columns: &[Column {
            width: NARROW,
            sections: &[
                section("Gain", &[p("gain", "Gain")]),
                section("Stereo", &[p("pan", "Pan"), p("width", "Width"), Row::Toggles(&[("mono", "Mono")])]),
            ],
        }],
    },
];

/// The layout for the device at `uri`, if it has one.
pub fn layout_for(uri: &str) -> Option<&'static Layout> {
    LAYOUTS.iter().find(|layout| layout.uri == uri)
}

impl Row {
    /// The parameters this row controls, pages included.
    pub fn param_ids(&self) -> Vec<&'static str> {
        match self {
            Row::Param { id, .. } | Row::Choice { id, .. } | Row::Picks { id, .. } => vec![id],
            Row::Toggles(toggles) => toggles.iter().map(|(id, _)| *id).collect(),
            Row::Pages(pages) => pages.iter().flat_map(|page| page.rows).flat_map(Row::param_ids).collect(),
            Row::Sidechain => Vec::new(),
        }
    }
}

/// A length in beats as musicians name it, where it is a note value:
/// `0.75` is a dotted eighth, `"1/8D"`.
pub fn note_value(beats: f64) -> Option<&'static str> {
    const NAMES: [(f64, &str); 14] = [
        (0.0625, "1/64"),
        (0.125, "1/32"),
        (1.0 / 6.0, "1/16T"),
        (0.25, "1/16"),
        (1.0 / 3.0, "1/8T"),
        (0.375, "1/16D"),
        (0.5, "1/8"),
        (2.0 / 3.0, "1/4T"),
        (0.75, "1/8D"),
        (1.0, "1/4"),
        (1.5, "1/4D"),
        (2.0, "1/2"),
        (3.0, "1/2D"),
        (4.0, "1/1"),
    ];
    NAMES.iter().find(|(length, _)| (length - beats).abs() < 0.004).map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use trill_core::device::{DeviceDescriptor, DeviceFactory, ParamKind};
    use trill_dsp::BuiltinFactory;

    /// Every parameter id a layout's controls refer to.
    fn placed(layout: &Layout) -> Vec<&'static str> {
        let sections = layout.columns.iter().flat_map(|column| column.sections);
        sections.flat_map(|section| section.rows).flat_map(Row::param_ids).collect()
    }

    /// The parameters a display reads.
    fn display_param_ids(display: Display) -> Vec<&'static str> {
        match display {
            Display::Filter { kind, cutoff, resonance, slope, .. } => {
                [kind, cutoff, resonance].into_iter().chain(slope).collect()
            }
            Display::Envelope { attack, decay, sustain, release } => vec![attack, decay, sustain, release],
            Display::Eq => vec![
                "low_cut",
                "low_freq",
                "low_gain",
                "mid1_freq",
                "mid1_gain",
                "mid1_q",
                "mid2_freq",
                "mid2_gain",
                "mid2_q",
                "high_freq",
                "high_gain",
                "high_cut",
                "low_cut_slope",
                "high_cut_slope",
                "low_q",
                "high_q",
            ],
            Display::Compressor => vec!["threshold", "ratio", "knee"],
            Display::Saturator => vec!["shape", "drive"],
            Display::Pump => vec!["depth", "curve"],
            Display::Tremolo => vec!["depth", "shape", "spread"],
        }
    }

    /// The built-in devices that have a layout, each with it.
    fn laid_out() -> Vec<(DeviceDescriptor, &'static Layout)> {
        let catalog = BuiltinFactory.descriptors();
        catalog
            .into_iter()
            .filter_map(|descriptor| layout_for(&descriptor.uri).map(|layout| (descriptor, layout)))
            .collect()
    }

    #[test]
    fn every_layout_is_for_a_built_in_device() {
        let catalog = BuiltinFactory.descriptors();
        for layout in LAYOUTS {
            assert!(catalog.iter().any(|descriptor| descriptor.uri == layout.uri), "{} is not a device", layout.uri);
            assert_eq!(LAYOUTS.iter().filter(|other| other.uri == layout.uri).count(), 1, "{} twice", layout.uri);
        }
        // The devices a new project starts from are all laid out; a device
        // that is not falls back to the generic layout.
        for uri in ["builtin:polysynth", "builtin:drumkit", "builtin:limiter", "builtin:eq", "builtin:reverb"] {
            assert!(layout_for(uri).is_some(), "{uri} has no layout");
        }
        assert!(layout_for("clap:com.example.synth").is_none());
    }

    #[test]
    fn every_parameter_is_placed_and_every_placement_is_a_parameter() {
        for (descriptor, layout) in laid_out() {
            let ids = placed(layout);
            for param in &descriptor.params {
                assert!(ids.contains(&param.id.as_str()), "{}: '{}' is not in the layout", descriptor.uri, param.id);
            }
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for id in &ids {
                assert!(
                    descriptor.params.iter().any(|param| param.id == *id),
                    "{}: the layout names '{id}', which is not a parameter",
                    descriptor.uri
                );
                *counts.entry(id).or_default() += 1;
            }
            // Quick picks sit beside the bar of the same parameter; nothing
            // else is shown twice.
            let picks: Vec<&str> = layout
                .columns
                .iter()
                .flat_map(|column| column.sections)
                .flat_map(|section| section.rows)
                .filter_map(|row| if let Row::Picks { id, .. } = row { Some(*id) } else { None })
                .collect();
            for (id, count) in counts {
                let expected = if picks.contains(&id) { 2 } else { 1 };
                assert_eq!(count, expected, "{}: '{id}' appears {count} times", descriptor.uri);
            }
        }
    }

    #[test]
    fn rows_fit_the_kind_of_their_parameter() {
        for (descriptor, layout) in laid_out() {
            let kind = |id: &str| &descriptor.params.iter().find(|param| param.id == id).unwrap().kind;
            let mut rows: Vec<&Row> =
                layout.columns.iter().flat_map(|column| column.sections).flat_map(|section| section.rows).collect();
            while let Some(row) = rows.pop() {
                match row {
                    Row::Param { id, label, .. } => {
                        assert!(!label.is_empty(), "{}: '{id}' needs a label", descriptor.uri);
                        assert!(
                            matches!(kind(id), ParamKind::Float | ParamKind::Int),
                            "{}: '{id}' is not a number; give it a Choice or Toggles row",
                            descriptor.uri
                        );
                    }
                    Row::Choice { id, labels } => match kind(id) {
                        ParamKind::Enum(choices) => {
                            assert_eq!(choices.len(), labels.len(), "{}: '{id}' labels", descriptor.uri)
                        }
                        ParamKind::Bool => assert_eq!(labels.len(), 2, "{}: '{id}' is off or on", descriptor.uri),
                        other => panic!("{}: '{id}' is {other:?}, not a choice", descriptor.uri),
                    },
                    Row::Toggles(toggles) => {
                        for (id, _) in *toggles {
                            assert_eq!(*kind(id), ParamKind::Bool, "{}: '{id}'", descriptor.uri);
                        }
                    }
                    Row::Picks { id, picks } => {
                        let param = descriptor.params.iter().find(|param| param.id == *id).unwrap();
                        for (label, value) in *picks {
                            assert!((param.min..=param.max).contains(value), "{}: pick '{label}'", descriptor.uri);
                        }
                    }
                    Row::Pages(pages) => rows.extend(pages.iter().flat_map(|page| page.rows)),
                    Row::Sidechain => {}
                }
            }
        }
    }

    /// A device is offered a track to listen to where it can listen to one,
    /// and nowhere else.
    #[test]
    fn the_devices_that_take_a_key_are_the_ones_with_a_sidechain_row() {
        for (descriptor, layout) in laid_out() {
            let rows = layout.columns.iter().flat_map(|column| column.sections).flat_map(|section| section.rows);
            let offered = rows.filter(|row| matches!(row, Row::Sidechain)).count();
            let mut project = trill_core::Project::new("t");
            let id = project.add_device(trill_core::ChainRef::Master, descriptor.uri.clone(), "d", None).unwrap();
            let Ok(instance) = BuiltinFactory.create(project.device(id).unwrap(), 48_000.0, 512) else { continue };
            assert_eq!(offered, usize::from(instance.processor.takes_key()), "{}", descriptor.uri);
        }
        assert!(layout_for("builtin:compressor").is_some());
    }

    #[test]
    fn displays_read_parameters_that_exist() {
        for (descriptor, layout) in laid_out() {
            let displays =
                layout.columns.iter().flat_map(|column| column.sections).filter_map(|section| section.display);
            for display in displays {
                for id in display_param_ids(display) {
                    assert!(
                        descriptor.params.iter().any(|param| param.id == id),
                        "{}: {display:?} reads '{id}', which is not a parameter",
                        descriptor.uri
                    );
                }
                if let Display::Filter { kind, shapes, .. } = display {
                    let choices = descriptor.params.iter().find(|param| param.id == kind).unwrap();
                    let ParamKind::Enum(choices) = &choices.kind else { panic!("{kind} is not an enum") };
                    assert_eq!(choices.len(), shapes.len(), "{}: a shape per filter type", descriptor.uri);
                }
            }
        }
    }

    #[test]
    fn beats_read_as_note_values() {
        assert_eq!(note_value(0.75), Some("1/8D"));
        assert_eq!(note_value(1.0), Some("1/4"));
        assert_eq!(note_value(0.25), Some("1/16"));
        assert_eq!(note_value(1.0 / 3.0), Some("1/8T"));
        assert_eq!(note_value(4.0), Some("1/1"));
        assert_eq!(note_value(0.8), None);
        assert_eq!(note_value(0.7), None);
    }
}
