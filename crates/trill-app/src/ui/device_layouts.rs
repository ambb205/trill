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
    /// A parameter with more choices than fit side by side, behind a menu.
    Menu { id: &'static str, label: &'static str },
    /// A display among the rows, so that a page can have one of its own.
    Shown(Display),
    /// One slot of a modulation matrix on one line: what it reads, what it
    /// moves, and by how much.
    Slot { source: &'static str, dest: &'static str, amount: &'static str },
    /// A macro: a bar under the name the device's sound gives it. `index`
    /// is which macro, from 0.
    Macro { id: &'static str, index: usize },
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
    /// The wave an oscillator of the wavetable synth reads at its position
    /// in its table. `oscillator` is which, from 0.
    Table { oscillator: usize },
    /// Two cycles of an LFO of the wavetable synth.
    Lfo { shape: &'static str, phase: &'static str, depth: &'static str },
    /// The response of a filter of the wavetable synth, of whichever of
    /// its kinds it is.
    TableFilter { kind: &'static str, cutoff: &'static str, resonance: &'static str },
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
/// The saturator's column: room for its four curves in a row, and for its
/// name over them.
const FOUR_CURVES: f32 = 186.0;
/// The pitch shifter's column, and the tape stop's: room for its name.
const SHIFTER_WIDTH: f32 = 224.0;
/// A column of the wavetable synth's matrix: a slot's source, destination
/// and amount on one line.
pub const SLOT_WIDTH: f32 = 316.0;
/// The wavetable synth's oscillator column: room for a table's name beside
/// its label.
const TABLE_WIDTH: f32 = 176.0;

const fn p(id: &'static str, label: &'static str) -> Row {
    Row::Param { id, label, format: Format::Auto }
}

const fn beats(id: &'static str, label: &'static str) -> Row {
    Row::Param { id, label, format: Format::NoteValue }
}

const fn key(id: &'static str, label: &'static str) -> Row {
    Row::Param { id, label, format: Format::NoteName }
}

const fn menu(id: &'static str, label: &'static str) -> Row {
    Row::Menu { id, label }
}

const fn section(title: &'static str, rows: &'static [Row]) -> Section {
    Section { title, display: None, rows }
}

const fn shown(title: &'static str, display: Display, rows: &'static [Row]) -> Section {
    Section { title, display: Some(display), rows }
}

const fn slot(source: &'static str, dest: &'static str, amount: &'static str) -> Row {
    Row::Slot { source, dest, amount }
}

const WAVES: &[&str] = &["Sin", "Tri", "Saw", "Sqr"];
/// How the wavetable synth bends a cycle.
const WARPS: &[&str] = &["Off", "Sync", "Bend", "Sqz"];
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
                    section("Pitch", &[p("pitch_env", "Envelope"), p("pitch_decay", "Time"), p("bend_range", "Wheel")]),
                ],
            },
            Column {
                width: NARROW,
                sections: &[section(
                    "Mix",
                    &[
                        p("osc_level", "Osc 1 + 2"),
                        p("osc_mix", "Osc 1 / 2"),
                        p("sub_level", "Sub"),
                        p("noise_level", "Noise"),
                        p("noise_width", "Noise Width"),
                        Row::Choice { id: "legacy", labels: &["Now", "1-3", "4-6"] },
                    ],
                )],
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
        uri: "builtin:wavetable",
        columns: &[
            Column {
                width: TABLE_WIDTH,
                sections: &[section(
                    "Oscillators",
                    &[Row::Pages(&[
                        Page {
                            label: "Osc 1",
                            rows: &[
                                Row::Shown(Display::Table { oscillator: 0 }),
                                menu("osc1_table", "Table"),
                                p("osc1_position", "Position"),
                                p("osc1_level", "Level"),
                                p("osc1_pan", "Pan"),
                                p("osc1_octave", "Octave"),
                                p("osc1_semitones", "Pitch"),
                                p("osc1_fine", "Fine"),
                            ],
                        },
                        Page {
                            label: "Osc 2",
                            rows: &[
                                Row::Shown(Display::Table { oscillator: 1 }),
                                menu("osc2_table", "Table"),
                                p("osc2_position", "Position"),
                                p("osc2_level", "Level"),
                                p("osc2_pan", "Pan"),
                                p("osc2_octave", "Octave"),
                                p("osc2_semitones", "Pitch"),
                                p("osc2_fine", "Fine"),
                            ],
                        },
                    ])],
                )],
            },
            Column {
                width: FILTER_WITH_OFF,
                sections: &[section(
                    "Shape",
                    &[Row::Pages(&[
                        Page {
                            label: "Osc 1",
                            rows: &[
                                Row::Choice { id: "osc1_warp", labels: WARPS },
                                p("osc1_warp_amount", "Warp"),
                                p("osc1_fm", "FM by Osc 2"),
                                p("osc1_unison", "Unison"),
                                p("osc1_detune", "Detune"),
                                p("osc1_blend", "Blend"),
                                p("osc1_spread", "Spread"),
                                p("osc1_phase", "Phase"),
                                p("osc1_phase_random", "Random"),
                            ],
                        },
                        Page {
                            label: "Osc 2",
                            rows: &[
                                Row::Choice { id: "osc2_warp", labels: WARPS },
                                p("osc2_warp_amount", "Warp"),
                                p("osc2_unison", "Unison"),
                                p("osc2_detune", "Detune"),
                                p("osc2_blend", "Blend"),
                                p("osc2_spread", "Spread"),
                                p("osc2_phase", "Phase"),
                                p("osc2_phase_random", "Random"),
                            ],
                        },
                    ])],
                )],
            },
            Column {
                width: NARROW,
                sections: &[
                    section(
                        "Sub",
                        &[
                            Row::Choice { id: "sub_shape", labels: WAVES },
                            p("sub_octave", "Octave"),
                            p("sub_level", "Level"),
                            Row::Toggles(&[("sub_direct", "Past Filters")]),
                        ],
                    ),
                    section(
                        "Noise",
                        &[p("noise_level", "Level"), p("noise_color", "Color"), p("noise_width", "Width")],
                    ),
                ],
            },
            Column {
                width: FILTER_WITH_OFF,
                sections: &[section(
                    "Filters",
                    &[
                        Row::Pages(&[
                            Page {
                                label: "Filter 1",
                                rows: &[
                                    Row::Shown(Display::TableFilter {
                                        kind: "filter1_type",
                                        cutoff: "filter1_cutoff",
                                        resonance: "filter1_resonance",
                                    }),
                                    menu("filter1_type", "Type"),
                                    p("filter1_cutoff", "Cutoff"),
                                    p("filter1_resonance", "Reso"),
                                    p("filter1_drive", "Drive"),
                                    p("filter1_key_track", "Key Track"),
                                ],
                            },
                            Page {
                                label: "Filter 2",
                                rows: &[
                                    Row::Shown(Display::TableFilter {
                                        kind: "filter2_type",
                                        cutoff: "filter2_cutoff",
                                        resonance: "filter2_resonance",
                                    }),
                                    menu("filter2_type", "Type"),
                                    p("filter2_cutoff", "Cutoff"),
                                    p("filter2_resonance", "Reso"),
                                    p("filter2_drive", "Drive"),
                                    p("filter2_key_track", "Key Track"),
                                ],
                            },
                        ]),
                        Row::Choice { id: "filter_routing", labels: &["Serial", "Parallel", "Split"] },
                        p("filter_balance", "Balance"),
                    ],
                )],
            },
            Column {
                width: WIDE,
                sections: &[
                    section(
                        "Envelopes",
                        &[Row::Pages(&[
                            Page {
                                label: "Level",
                                rows: &[
                                    Row::Shown(Display::Envelope {
                                        attack: "env1_attack",
                                        decay: "env1_decay",
                                        sustain: "env1_sustain",
                                        release: "env1_release",
                                    }),
                                    p("env1_attack", "Attack"),
                                    p("env1_decay", "Decay"),
                                    p("env1_sustain", "Sustain"),
                                    p("env1_release", "Release"),
                                ],
                            },
                            Page {
                                label: "Env 2",
                                rows: &[
                                    Row::Shown(Display::Envelope {
                                        attack: "env2_attack",
                                        decay: "env2_decay",
                                        sustain: "env2_sustain",
                                        release: "env2_release",
                                    }),
                                    p("env2_attack", "Attack"),
                                    p("env2_decay", "Decay"),
                                    p("env2_sustain", "Sustain"),
                                    p("env2_release", "Release"),
                                ],
                            },
                            Page {
                                label: "Env 3",
                                rows: &[
                                    Row::Shown(Display::Envelope {
                                        attack: "env3_attack",
                                        decay: "env3_decay",
                                        sustain: "env3_sustain",
                                        release: "env3_release",
                                    }),
                                    p("env3_attack", "Attack"),
                                    p("env3_decay", "Decay"),
                                    p("env3_sustain", "Sustain"),
                                    p("env3_release", "Release"),
                                ],
                            },
                        ])],
                    ),
                    section("Output", &[p("velocity_sens", "Velocity"), p("volume", "Volume")]),
                ],
            },
            Column {
                width: WIDE,
                sections: &[section(
                    "LFOs",
                    &[Row::Pages(&[
                        Page {
                            label: "LFO 1",
                            rows: &[
                                Row::Shown(Display::Lfo {
                                    shape: "lfo1_shape",
                                    phase: "lfo1_phase",
                                    depth: "lfo1_depth",
                                }),
                                menu("lfo1_shape", "Shape"),
                                p("lfo1_rate", "Rate"),
                                menu("lfo1_sync", "Sync"),
                                p("lfo1_fade", "Fade In"),
                                p("lfo1_depth", "Depth"),
                                p("lfo1_phase", "Phase"),
                                Row::Toggles(&[("lfo1_retrigger", "Retrigger")]),
                            ],
                        },
                        Page {
                            label: "LFO 2",
                            rows: &[
                                Row::Shown(Display::Lfo {
                                    shape: "lfo2_shape",
                                    phase: "lfo2_phase",
                                    depth: "lfo2_depth",
                                }),
                                menu("lfo2_shape", "Shape"),
                                p("lfo2_rate", "Rate"),
                                menu("lfo2_sync", "Sync"),
                                p("lfo2_fade", "Fade In"),
                                p("lfo2_depth", "Depth"),
                                p("lfo2_phase", "Phase"),
                                Row::Toggles(&[("lfo2_retrigger", "Retrigger")]),
                            ],
                        },
                        Page {
                            label: "LFO 3",
                            rows: &[
                                Row::Shown(Display::Lfo {
                                    shape: "lfo3_shape",
                                    phase: "lfo3_phase",
                                    depth: "lfo3_depth",
                                }),
                                menu("lfo3_shape", "Shape"),
                                p("lfo3_rate", "Rate"),
                                menu("lfo3_sync", "Sync"),
                                p("lfo3_fade", "Fade In"),
                                p("lfo3_depth", "Depth"),
                                p("lfo3_phase", "Phase"),
                                Row::Toggles(&[("lfo3_retrigger", "Retrigger")]),
                            ],
                        },
                    ])],
                )],
            },
            Column {
                width: SLOT_WIDTH,
                sections: &[section(
                    "Matrix",
                    &[
                        slot("mod1_source", "mod1_dest", "mod1_amount"),
                        slot("mod2_source", "mod2_dest", "mod2_amount"),
                        slot("mod3_source", "mod3_dest", "mod3_amount"),
                        slot("mod4_source", "mod4_dest", "mod4_amount"),
                        slot("mod5_source", "mod5_dest", "mod5_amount"),
                        slot("mod6_source", "mod6_dest", "mod6_amount"),
                        slot("mod7_source", "mod7_dest", "mod7_amount"),
                        slot("mod8_source", "mod8_dest", "mod8_amount"),
                    ],
                )],
            },
            Column {
                width: SLOT_WIDTH,
                sections: &[section(
                    "Matrix, slots 9 to 16",
                    &[
                        slot("mod9_source", "mod9_dest", "mod9_amount"),
                        slot("mod10_source", "mod10_dest", "mod10_amount"),
                        slot("mod11_source", "mod11_dest", "mod11_amount"),
                        slot("mod12_source", "mod12_dest", "mod12_amount"),
                        slot("mod13_source", "mod13_dest", "mod13_amount"),
                        slot("mod14_source", "mod14_dest", "mod14_amount"),
                        slot("mod15_source", "mod15_dest", "mod15_amount"),
                        slot("mod16_source", "mod16_dest", "mod16_amount"),
                    ],
                )],
            },
            Column {
                width: NARROW,
                sections: &[
                    section(
                        "Macros",
                        &[
                            Row::Macro { id: "macro1", index: 0 },
                            Row::Macro { id: "macro2", index: 1 },
                            Row::Macro { id: "macro3", index: 2 },
                            Row::Macro { id: "macro4", index: 3 },
                        ],
                    ),
                    section(
                        "Voice",
                        &[
                            Row::Choice { id: "mode", labels: &["Poly", "Mono", "Legato"] },
                            p("voices", "Voices"),
                            p("glide", "Glide"),
                            p("bend_range", "Wheel"),
                            p("mono_below", "Mono Below"),
                        ],
                    ),
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
    // The instrument it plays is shown ahead of these, by
    // `instrument_panel`: it is not a parameter.
    Layout {
        uri: "builtin:sampled",
        columns: &[Column {
            width: NARROW,
            sections: &[
                section("Pitch", &[p("transpose", "Transpose"), p("tune", "Fine"), p("bend_range", "Wheel")]),
                section(
                    "Output",
                    &[p("dynamics", "Dynamics"), p("voices", "Voices"), p("volume", "Volume"), p("pan", "Pan")],
                ),
            ],
        }],
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
                        p("bend_range", "Wheel"),
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
                            Row::Toggles(&[("legacy", "Legacy")]),
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
                    section("Output", &[p("mix", "Mix")]),
                    section("As In Format", &[Row::Choice { id: "legacy", labels: &["Now", "1", "2-6"] }]),
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
                    section("Output", &[p("width", "Width"), p("mix", "Mix")]),
                    section("As In Format", &[Row::Choice { id: "legacy", labels: &["Now", "1", "2-6"] }]),
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
                        &[
                            p("makeup", "Makeup"),
                            Row::Toggles(&[("auto_makeup", "Auto Makeup")]),
                            p("mix", "Mix"),
                            Row::Toggles(&[("legacy", "Legacy")]),
                        ],
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
            width: FOUR_CURVES,
            sections: &[
                shown(
                    "Drive",
                    Display::Saturator,
                    &[Row::Choice { id: "shape", labels: &["Soft", "Hard", "Tube", "Sine"] }, p("drive", "Drive")],
                ),
                section("Output", &[p("output", "Output"), p("mix", "Mix"), Row::Toggles(&[("legacy", "Legacy")])]),
            ],
        }],
    },
    Layout {
        uri: "builtin:utility",
        columns: &[Column {
            width: NARROW,
            sections: &[
                section("Gain", &[p("gain", "Gain"), Row::Toggles(&[("legacy", "Legacy")])]),
                section("Stereo", &[p("pan", "Pan"), p("width", "Width"), Row::Toggles(&[("mono", "Mono")])]),
            ],
        }],
    },
    Layout {
        uri: "builtin:shifter",
        columns: &[Column {
            width: SHIFTER_WIDTH,
            sections: &[
                section(
                    "Pitch",
                    &[
                        Row::Picks {
                            id: "pitch",
                            picks: &[("-24", -24.0), ("-12", -12.0), ("0", 0.0), ("+12", 12.0), ("+24", 24.0)],
                        },
                        p("pitch", "Pitch"),
                        p("fine", "Fine"),
                        p("window", "Window"),
                    ],
                ),
                section("Output", &[p("mix", "Mix")]),
            ],
        }],
    },
    Layout {
        uri: "builtin:tapestop",
        columns: &[Column {
            width: SHIFTER_WIDTH,
            sections: &[section(
                "Tape",
                &[
                    p("speed", "Speed"),
                    Row::Picks { id: "speed", picks: &[("0", 0.0), ("0.25", 0.25), ("0.5", 0.5), ("1", 1.0)] },
                ],
            )],
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
            Row::Sidechain | Row::Shown(_) => Vec::new(),
            Row::Menu { id, .. } | Row::Macro { id, .. } => vec![id],
            Row::Slot { source, dest, amount } => vec![source, dest, amount],
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
            Display::Table { oscillator } => {
                vec![["osc1_table", "osc2_table"][oscillator], ["osc1_position", "osc2_position"][oscillator]]
            }
            Display::Lfo { shape, phase, depth } => vec![shape, phase, depth],
            Display::TableFilter { kind, cutoff, resonance } => vec![kind, cutoff, resonance],
        }
    }

    /// Every display of a layout: a section's own, and the ones among its
    /// rows and their pages.
    fn displays(layout: &Layout) -> Vec<Display> {
        let mut found: Vec<Display> =
            layout.columns.iter().flat_map(|column| column.sections).filter_map(|section| section.display).collect();
        let mut rows: Vec<&Row> =
            layout.columns.iter().flat_map(|column| column.sections).flat_map(|section| section.rows).collect();
        while let Some(row) = rows.pop() {
            match row {
                Row::Shown(display) => found.push(*display),
                Row::Pages(pages) => rows.extend(pages.iter().flat_map(|page| page.rows)),
                _ => {}
            }
        }
        found
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
        let laid_out = ["builtin:polysynth", "builtin:wavetable", "builtin:drumkit", "builtin:limiter", "builtin:eq"];
        for uri in laid_out.into_iter().chain(["builtin:reverb"]) {
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
                    Row::Sidechain | Row::Shown(_) => {}
                    Row::Menu { id, label } => {
                        assert!(!label.is_empty(), "{}: '{id}' needs a label", descriptor.uri);
                        assert!(matches!(kind(id), ParamKind::Enum(_)), "{}: '{id}' is no choice", descriptor.uri);
                    }
                    Row::Slot { source, dest, amount } => {
                        assert!(matches!(kind(source), ParamKind::Enum(_)) && matches!(kind(dest), ParamKind::Enum(_)));
                        assert_eq!(*kind(amount), ParamKind::Float, "{}: '{amount}'", descriptor.uri);
                    }
                    Row::Macro { id, index } => {
                        assert_eq!(*kind(id), ParamKind::Float, "{}: '{id}'", descriptor.uri);
                        assert!(*index < 4);
                    }
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
            for display in displays(layout) {
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
