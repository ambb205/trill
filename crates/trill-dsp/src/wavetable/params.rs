//! The wavetable synth's parameters: the table a model reads to know what
//! the synth can do, so every name says what the value is and what its
//! ends do.
//!
//! The order of the parameters, and of the choices of each that is a
//! choice, is part of the file format: a song keeps a choice as its
//! number. New ones go at the end.

use super::factory::NAMES;
use crate::define_params;
use crate::params::{boolean, choice, float, int};
use crate::util::SILENT_DB;
use trill_core::device::PITCH_BEND_SEMITONES;

/// The most voices of unison an oscillator has.
pub const MAX_UNISON: usize = 16;
/// The most notes that sound at once.
pub const MAX_VOICES: usize = 32;
/// The slots of the modulation matrix.
pub const SLOTS: usize = 16;

pub const WARPS: &[&str] = &["off", "sync", "bend", "squeeze"];
pub const SUB_SHAPES: &[&str] = &["sine", "triangle", "saw", "square"];
pub const FILTERS: &[&str] = &[
    "off",
    "lowpass12",
    "lowpass24",
    "bandpass12",
    "bandpass24",
    "highpass12",
    "highpass24",
    "notch12",
    "notch24",
    "comb",
    "comb_minus",
];
pub const ROUTINGS: &[&str] = &["serial", "parallel", "split"];
pub const LFO_SHAPES: &[&str] = &["sine", "triangle", "ramp_up", "ramp_down", "square", "steps", "smooth"];
pub const LFO_SYNCS: &[&str] = &[
    "off",
    "8 bars",
    "4 bars",
    "2 bars",
    "1 bar",
    "1/2",
    "1/4",
    "1/8",
    "1/16",
    "1/32",
    "1/2 dotted",
    "1/4 dotted",
    "1/8 dotted",
    "1/4 triplet",
    "1/8 triplet",
    "1/16 triplet",
];
pub const MODES: &[&str] = &["poly", "mono", "legato"];

/// What a slot of the matrix can read.
pub const SOURCES: &[&str] = &[
    "none",
    "env1",
    "env2",
    "env3",
    "lfo1",
    "lfo2",
    "lfo3",
    "velocity",
    "key",
    "mod_wheel",
    "pitch_bend",
    "pressure",
    "breath",
    "macro1",
    "macro2",
    "macro3",
    "macro4",
    "random",
];

/// What a slot of the matrix can move.
pub const DESTINATIONS: &[&str] = &[
    "none",
    "pitch",
    "fine_pitch",
    "osc1_pitch",
    "osc2_pitch",
    "osc1_position",
    "osc2_position",
    "osc1_warp",
    "osc2_warp",
    "osc1_fm",
    "osc1_level",
    "osc2_level",
    "osc1_pan",
    "osc2_pan",
    "sub_level",
    "noise_level",
    "filter1_cutoff",
    "filter2_cutoff",
    "filter1_resonance",
    "filter2_resonance",
    "filter1_drive",
    "filter2_drive",
    "filter_balance",
    "level",
    "pan",
    "lfo1_rate",
    "lfo2_rate",
    "lfo3_rate",
    "lfo1_depth",
    "lfo2_depth",
    "lfo3_depth",
    "env2_depth",
    "env3_depth",
];

/// How many parameters an oscillator has, a filter, an envelope and an
/// LFO: the second of each lies that far after the first.
pub const OSC_PARAMS: usize = 15;
pub const FILTER_PARAMS: usize = 5;
pub const ENV_PARAMS: usize = 4;
pub const LFO_PARAMS: usize = 7;

const TABLE_NAME_1: &str = "Osc 1 Table (the wavetable it reads; `custom` is the one made for this device with \
                            set_wavetable)";
const TABLE_NAME_2: &str = "Osc 2 Table";
const POSITION_NAME: &str = "Table Position (0 = the table's first frame, 1 = its last; in between, the frames \
                             go into one another)";
const WARP_NAME: &str = "Warp (how the cycle is bent before it is read. sync: restarted up to 16 times a cycle; \
                         bend: its ends drawn out and its middle hurried; squeeze: the whole cycle pushed into \
                         the start of the period, as pulse width does to a square)";
const UNISON_NAME: &str = "Unison Voices (copies of the oscillator, detuned against one another)";
const DETUNE_NAME: &str = "Unison Detune (how far the outermost copies are from the note, each way)";
const BLEND_NAME: &str = "Unison Blend (the level of the detuned copies against the one in tune: 0 = it alone, 1 = \
                          all alike)";
const PHASE_RANDOM_NAME: &str = "Start Phase Random (0 = every note starts the cycle at the start phase: the same \
                                 attack each time; 1 = anywhere, each copy for itself)";

/// The table and its index. Most parameters are reached by counting from
/// the first of their kind ([`of`]), so most of the index's names are
/// never written out.
mod table {
    #![allow(dead_code)]
    use super::*;

    define_params!(P, PARAMS, [
        Osc1Table => choice("osc1_table", TABLE_NAME_1, NAMES, 1),
        Osc1Position => float("osc1_position", POSITION_NAME, 0.0, 1.0, 0.5, ""),
        Osc1Warp => choice("osc1_warp", WARP_NAME, WARPS, 0),
        Osc1WarpAmount => float("osc1_warp_amount", "Osc 1 Warp Amount (0 = none)", 0.0, 1.0, 0.0, ""),
        Osc1Level => float("osc1_level", "Osc 1 Level (1 = a wave that reaches full level)", 0.0, 1.0, 1.0, ""),
        Osc1Pan => float("osc1_pan", "Osc 1 Pan (-1 = left, 1 = right)", -1.0, 1.0, 0.0, ""),
        Osc1Octave => int("osc1_octave", "Osc 1 Octave", -4, 4, 0, "oct"),
        Osc1Semitones => int("osc1_semitones", "Osc 1 Pitch", -12, 12, 0, "st"),
        Osc1Fine => float("osc1_fine", "Osc 1 Fine Tune", -100.0, 100.0, 0.0, "cents"),
        Osc1Unison => int("osc1_unison", UNISON_NAME, 1, MAX_UNISON as i32, 1, ""),
        Osc1Detune => float("osc1_detune", DETUNE_NAME, 0.0, 100.0, 20.0, "cents"),
        Osc1Blend => float("osc1_blend", BLEND_NAME, 0.0, 1.0, 0.8, ""),
        Osc1Spread => float("osc1_spread", "Unison Stereo Spread (0 = all in the middle, 1 = across the whole width)", 0.0, 1.0, 0.7, ""),
        Osc1Phase => float("osc1_phase", "Start Phase (where in the cycle a note begins, in cycles)", 0.0, 1.0, 0.0, ""),
        Osc1PhaseRandom => float("osc1_phase_random", PHASE_RANDOM_NAME, 0.0, 1.0, 1.0, ""),

        Osc2Table => choice("osc2_table", TABLE_NAME_2, NAMES, 1),
        Osc2Position => float("osc2_position", "Osc 2 Table Position", 0.0, 1.0, 0.5, ""),
        Osc2Warp => choice("osc2_warp", "Osc 2 Warp", WARPS, 0),
        Osc2WarpAmount => float("osc2_warp_amount", "Osc 2 Warp Amount", 0.0, 1.0, 0.0, ""),
        Osc2Level => float("osc2_level", "Osc 2 Level (0 = silent; it still bends oscillator 1 if `osc1_fm` is up)", 0.0, 1.0, 0.0, ""),
        Osc2Pan => float("osc2_pan", "Osc 2 Pan", -1.0, 1.0, 0.0, ""),
        Osc2Octave => int("osc2_octave", "Osc 2 Octave", -4, 4, 0, "oct"),
        Osc2Semitones => int("osc2_semitones", "Osc 2 Pitch", -12, 12, 0, "st"),
        Osc2Fine => float("osc2_fine", "Osc 2 Fine Tune", -100.0, 100.0, 0.0, "cents"),
        Osc2Unison => int("osc2_unison", "Osc 2 Unison Voices", 1, MAX_UNISON as i32, 1, ""),
        Osc2Detune => float("osc2_detune", "Osc 2 Unison Detune", 0.0, 100.0, 20.0, "cents"),
        Osc2Blend => float("osc2_blend", "Osc 2 Unison Blend", 0.0, 1.0, 0.8, ""),
        Osc2Spread => float("osc2_spread", "Osc 2 Unison Stereo Spread", 0.0, 1.0, 0.7, ""),
        Osc2Phase => float("osc2_phase", "Osc 2 Start Phase", 0.0, 1.0, 0.0, ""),
        Osc2PhaseRandom => float("osc2_phase_random", "Osc 2 Start Phase Random", 0.0, 1.0, 1.0, ""),

        Osc1Fm => float(
            "osc1_fm",
            "Osc 1 FM (how far oscillator 2 bends oscillator 1's phase, whatever oscillator 2's level: 1 = a cycle and \
             a half each way. With oscillator 2 a sine an octave or more up this is FM: bells, electric pianos, growls)",
            0.0,
            1.0,
            0.0,
            ""
        ),

        SubShape => choice("sub_shape", "Sub Oscillator Shape", SUB_SHAPES, 0),
        SubOctave => int("sub_octave", "Sub Oscillator Octave (under the note)", -2, 0, -1, "oct"),
        SubLevel => float("sub_level", "Sub Oscillator Level", 0.0, 1.0, 0.0, ""),
        SubDirect => boolean("sub_direct", "Sub Past the Filters (the sub goes straight to the output, so a filter sweep leaves the bottom alone)", false),

        NoiseLevel => float("noise_level", "Noise Level (at 1 it peaks 13 dB under an oscillator)", 0.0, 1.0, 0.0, ""),
        NoiseColor => float("noise_color", "Noise Color (-1 = dark rumble, 0 = white, 1 = thin hiss)", -1.0, 1.0, 0.0, ""),
        NoiseWidth => float("noise_width", "Noise Stereo Width (0 = the same on both sides, 1 = a noise of its own on each)", 0.0, 1.0, 0.0, ""),

        Filter1Type => choice(
            "filter1_type",
            "Filter 1 Type (off until one is chosen; 12 or 24 dB an octave; comb rings at the cutoff and its multiples like a plucked string, \
             comb_minus at the odd ones, hollow)",
            FILTERS,
            0
        ),
        Filter1Cutoff => float("filter1_cutoff", "Filter 1 Cutoff", 20.0, 20000.0, 20000.0, "Hz"),
        Filter1Resonance => float("filter1_resonance", "Filter 1 Resonance (0 = none, 1 = a sharp ringing peak)", 0.0, 1.0, 0.1, ""),
        Filter1Drive => float("filter1_drive", "Filter 1 Drive (how hard the sound is pushed into saturation before the filter)", 0.0, 24.0, 0.0, "dB"),
        Filter1KeyTrack => float("filter1_key_track", "Filter 1 Key Tracking (0 = the same cutoff for every note, 1 = the cutoff rises with the note, from middle C)", 0.0, 1.0, 0.0, ""),

        Filter2Type => choice("filter2_type", "Filter 2 Type", FILTERS, 0),
        Filter2Cutoff => float("filter2_cutoff", "Filter 2 Cutoff", 20.0, 20000.0, 20000.0, "Hz"),
        Filter2Resonance => float("filter2_resonance", "Filter 2 Resonance", 0.0, 1.0, 0.1, ""),
        Filter2Drive => float("filter2_drive", "Filter 2 Drive", 0.0, 24.0, 0.0, "dB"),
        Filter2KeyTrack => float("filter2_key_track", "Filter 2 Key Tracking", 0.0, 1.0, 0.0, ""),

        FilterRouting => choice(
            "filter_routing",
            "Filter Routing (serial: everything through filter 1, then filter 2. parallel: through both side by side. \
             split: oscillator 1, the sub and the noise through filter 1, oscillator 2 through filter 2)",
            ROUTINGS,
            0
        ),
        FilterBalance => float(
            "filter_balance",
            "Filter Balance (parallel and split: 0 = filter 1 alone, 0.5 = both in full, 1 = filter 2 alone)",
            0.0,
            1.0,
            0.5,
            ""
        ),

        Env1Attack => float("env1_attack", "Envelope 1 Attack (envelope 1 is the level of the note)", 0.001, 10.0, 0.005, "s"),
        Env1Decay => float("env1_decay", "Envelope 1 Decay (to a thousandth of the way to the sustain level)", 0.005, 20.0, 0.5, "s"),
        Env1Sustain => float("env1_sustain", "Envelope 1 Sustain", 0.0, 1.0, 0.8, ""),
        Env1Release => float("env1_release", "Envelope 1 Release", 0.005, 20.0, 0.3, "s"),
        Env2Attack => float("env2_attack", "Envelope 2 Attack (envelopes 2 and 3 move what the matrix sends them to)", 0.001, 10.0, 0.005, "s"),
        Env2Decay => float("env2_decay", "Envelope 2 Decay", 0.005, 20.0, 0.5, "s"),
        Env2Sustain => float("env2_sustain", "Envelope 2 Sustain", 0.0, 1.0, 0.0, ""),
        Env2Release => float("env2_release", "Envelope 2 Release", 0.005, 20.0, 0.3, "s"),
        Env3Attack => float("env3_attack", "Envelope 3 Attack", 0.001, 10.0, 0.005, "s"),
        Env3Decay => float("env3_decay", "Envelope 3 Decay", 0.005, 20.0, 0.5, "s"),
        Env3Sustain => float("env3_sustain", "Envelope 3 Sustain", 0.0, 1.0, 0.0, ""),
        Env3Release => float("env3_release", "Envelope 3 Release", 0.005, 20.0, 0.3, "s"),

        Lfo1Shape => choice("lfo1_shape", "LFO 1 Shape (steps: a new value drawn by lot each cycle; smooth: gliding between them)", LFO_SHAPES, 0),
        Lfo1Rate => float("lfo1_rate", "LFO 1 Rate (when `lfo1_sync` is off)", 0.01, 50.0, 5.0, "Hz"),
        Lfo1Sync => choice("lfo1_sync", "LFO 1 Sync (off, or the length of one cycle in the song's time; an LFO that does not retrigger then keeps its place in the song)", LFO_SYNCS, 0),
        Lfo1Retrigger => boolean("lfo1_retrigger", "LFO 1 Retrigger (on: every note starts the LFO anew, for itself. off: one LFO runs on for all notes)", true),
        Lfo1Fade => float("lfo1_fade", "LFO 1 Fade In (how long after a note starts the LFO takes to reach its depth)", 0.0, 10.0, 0.0, "s"),
        Lfo1Depth => float("lfo1_depth", "LFO 1 Depth (what its output is multiplied by; set it to 0 and send the mod wheel to `lfo1_depth` for a wheel that brings the LFO in)", 0.0, 1.0, 1.0, ""),
        Lfo1Phase => float("lfo1_phase", "LFO 1 Start Phase (in cycles)", 0.0, 1.0, 0.0, ""),
        Lfo2Shape => choice("lfo2_shape", "LFO 2 Shape", LFO_SHAPES, 0),
        Lfo2Rate => float("lfo2_rate", "LFO 2 Rate", 0.01, 50.0, 1.0, "Hz"),
        Lfo2Sync => choice("lfo2_sync", "LFO 2 Sync", LFO_SYNCS, 0),
        Lfo2Retrigger => boolean("lfo2_retrigger", "LFO 2 Retrigger", true),
        Lfo2Fade => float("lfo2_fade", "LFO 2 Fade In", 0.0, 10.0, 0.0, "s"),
        Lfo2Depth => float("lfo2_depth", "LFO 2 Depth", 0.0, 1.0, 1.0, ""),
        Lfo2Phase => float("lfo2_phase", "LFO 2 Start Phase", 0.0, 1.0, 0.0, ""),
        Lfo3Shape => choice("lfo3_shape", "LFO 3 Shape", LFO_SHAPES, 0),
        Lfo3Rate => float("lfo3_rate", "LFO 3 Rate", 0.01, 50.0, 0.2, "Hz"),
        Lfo3Sync => choice("lfo3_sync", "LFO 3 Sync", LFO_SYNCS, 0),
        Lfo3Retrigger => boolean("lfo3_retrigger", "LFO 3 Retrigger", true),
        Lfo3Fade => float("lfo3_fade", "LFO 3 Fade In", 0.0, 10.0, 0.0, "s"),
        Lfo3Depth => float("lfo3_depth", "LFO 3 Depth", 0.0, 1.0, 1.0, ""),
        Lfo3Phase => float("lfo3_phase", "LFO 3 Start Phase", 0.0, 1.0, 0.0, ""),

        Mod1Source => choice("mod1_source", "Matrix Slot 1 Source (what the slot reads)", SOURCES, 0),
        Mod1Dest => choice("mod1_dest", "Matrix Slot 1 Destination (what it moves)", DESTINATIONS, 0),
        Mod1Amount => float(
            "mod1_amount",
            "Matrix Slot 1 Amount (the destination moves by amount x source, where 1 is: 48 st of pitch, 1 st of \
             fine_pitch, 10 octaves of cutoff, the whole table, a whole level, warp, FM, resonance, balance or LFO \
             depth, all of the pan, 24 dB of drive, 5 octaves of LFO rate; env2_depth and env3_depth scale what that \
             envelope sends everywhere, from nothing at -1 to twice at 1)",
            -1.0,
            1.0,
            0.0,
            ""
        ),
        Mod2Source => choice("mod2_source", "Matrix Slot 2 Source", SOURCES, 0),
        Mod2Dest => choice("mod2_dest", "Matrix Slot 2 Destination", DESTINATIONS, 0),
        Mod2Amount => float("mod2_amount", "Matrix Slot 2 Amount", -1.0, 1.0, 0.0, ""),
        Mod3Source => choice("mod3_source", "Matrix Slot 3 Source", SOURCES, 0),
        Mod3Dest => choice("mod3_dest", "Matrix Slot 3 Destination", DESTINATIONS, 0),
        Mod3Amount => float("mod3_amount", "Matrix Slot 3 Amount", -1.0, 1.0, 0.0, ""),
        Mod4Source => choice("mod4_source", "Matrix Slot 4 Source", SOURCES, 0),
        Mod4Dest => choice("mod4_dest", "Matrix Slot 4 Destination", DESTINATIONS, 0),
        Mod4Amount => float("mod4_amount", "Matrix Slot 4 Amount", -1.0, 1.0, 0.0, ""),
        Mod5Source => choice("mod5_source", "Matrix Slot 5 Source", SOURCES, 0),
        Mod5Dest => choice("mod5_dest", "Matrix Slot 5 Destination", DESTINATIONS, 0),
        Mod5Amount => float("mod5_amount", "Matrix Slot 5 Amount", -1.0, 1.0, 0.0, ""),
        Mod6Source => choice("mod6_source", "Matrix Slot 6 Source", SOURCES, 0),
        Mod6Dest => choice("mod6_dest", "Matrix Slot 6 Destination", DESTINATIONS, 0),
        Mod6Amount => float("mod6_amount", "Matrix Slot 6 Amount", -1.0, 1.0, 0.0, ""),
        Mod7Source => choice("mod7_source", "Matrix Slot 7 Source", SOURCES, 0),
        Mod7Dest => choice("mod7_dest", "Matrix Slot 7 Destination", DESTINATIONS, 0),
        Mod7Amount => float("mod7_amount", "Matrix Slot 7 Amount", -1.0, 1.0, 0.0, ""),
        Mod8Source => choice("mod8_source", "Matrix Slot 8 Source", SOURCES, 0),
        Mod8Dest => choice("mod8_dest", "Matrix Slot 8 Destination", DESTINATIONS, 0),
        Mod8Amount => float("mod8_amount", "Matrix Slot 8 Amount", -1.0, 1.0, 0.0, ""),
        Mod9Source => choice("mod9_source", "Matrix Slot 9 Source", SOURCES, 0),
        Mod9Dest => choice("mod9_dest", "Matrix Slot 9 Destination", DESTINATIONS, 0),
        Mod9Amount => float("mod9_amount", "Matrix Slot 9 Amount", -1.0, 1.0, 0.0, ""),
        Mod10Source => choice("mod10_source", "Matrix Slot 10 Source", SOURCES, 0),
        Mod10Dest => choice("mod10_dest", "Matrix Slot 10 Destination", DESTINATIONS, 0),
        Mod10Amount => float("mod10_amount", "Matrix Slot 10 Amount", -1.0, 1.0, 0.0, ""),
        Mod11Source => choice("mod11_source", "Matrix Slot 11 Source", SOURCES, 0),
        Mod11Dest => choice("mod11_dest", "Matrix Slot 11 Destination", DESTINATIONS, 0),
        Mod11Amount => float("mod11_amount", "Matrix Slot 11 Amount", -1.0, 1.0, 0.0, ""),
        Mod12Source => choice("mod12_source", "Matrix Slot 12 Source", SOURCES, 0),
        Mod12Dest => choice("mod12_dest", "Matrix Slot 12 Destination", DESTINATIONS, 0),
        Mod12Amount => float("mod12_amount", "Matrix Slot 12 Amount", -1.0, 1.0, 0.0, ""),
        Mod13Source => choice("mod13_source", "Matrix Slot 13 Source", SOURCES, 0),
        Mod13Dest => choice("mod13_dest", "Matrix Slot 13 Destination", DESTINATIONS, 0),
        Mod13Amount => float("mod13_amount", "Matrix Slot 13 Amount", -1.0, 1.0, 0.0, ""),
        Mod14Source => choice("mod14_source", "Matrix Slot 14 Source", SOURCES, 0),
        Mod14Dest => choice("mod14_dest", "Matrix Slot 14 Destination", DESTINATIONS, 0),
        Mod14Amount => float("mod14_amount", "Matrix Slot 14 Amount", -1.0, 1.0, 0.0, ""),
        Mod15Source => choice("mod15_source", "Matrix Slot 15 Source", SOURCES, 0),
        Mod15Dest => choice("mod15_dest", "Matrix Slot 15 Destination", DESTINATIONS, 0),
        Mod15Amount => float("mod15_amount", "Matrix Slot 15 Amount", -1.0, 1.0, 0.0, ""),
        Mod16Source => choice("mod16_source", "Matrix Slot 16 Source", SOURCES, 0),
        Mod16Dest => choice("mod16_dest", "Matrix Slot 16 Destination", DESTINATIONS, 0),
        Mod16Amount => float("mod16_amount", "Matrix Slot 16 Amount", -1.0, 1.0, 0.0, ""),

        Macro1 => float("macro1", "Macro 1 (a handle of the sound: it does what the matrix sends `macro1` to, under the name the device gives it)", 0.0, 1.0, 0.0, ""),
        Macro2 => float("macro2", "Macro 2", 0.0, 1.0, 0.0, ""),
        Macro3 => float("macro3", "Macro 3", 0.0, 1.0, 0.0, ""),
        Macro4 => float("macro4", "Macro 4", 0.0, 1.0, 0.0, ""),

        Mode => choice(
            "mode",
            "Voice Mode (poly: a voice for every note. mono: one note at a time, each struck anew. legato: one note \
             at a time, and a note played while another is held carries on its envelopes and glides to it)",
            MODES,
            0
        ),
        Voices => int("voices", "Voices (the most notes at once in poly mode; one more takes the place of the quietest)", 1, MAX_VOICES as i32, 16, ""),
        Glide => float("glide", "Glide Time (how long the pitch takes to slide to a new note; 0 = at once. In legato mode only notes played while another is held slide)", 0.0, 5.0, 0.0, "s"),
        BendRange => float(
            "bend_range",
            "Pitch Wheel Range (how far the wheel bends, each way; the track's instrument profile takes it from here)",
            0.0,
            48.0,
            PITCH_BEND_SEMITONES as f64,
            "st"
        ),
        VelocitySens => float("velocity_sens", "Velocity Sensitivity (0 = every note as loud; 1 = the level follows the square of the velocity)", 0.0, 1.0, 0.6, ""),
        Volume => float(
            "volume",
            "Volume (silent at -70; at 0 one voice of one oscillator at full velocity is 8 dB over full scale, so \
             chords need the room the default leaves)",
            SILENT_DB as f64,
            6.0,
            -21.0,
            "dB"
        ),
        MonoBelow => float(
            "mono_below",
            "Mono Below (everything under this frequency is the same on both sides, which a bass wants so that it \
             sits in the middle and is whole on one speaker; the width above it is left alone. 0 = off)",
            0.0,
            500.0,
            0.0,
            "Hz"
        ),
    ]);
}

pub(crate) use table::P;
pub use table::PARAMS;

/// The position of a parameter of the second oscillator, filter, envelope
/// or LFO, from that of the first.
pub(super) fn of(first: P, which: usize, stride: usize) -> usize {
    first as usize + which * stride
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_of_everything_lies_where_it_is_looked_for() {
        let id = |index: usize| PARAMS[index].id;
        assert_eq!(id(of(P::Osc1Table, 1, OSC_PARAMS)), "osc2_table");
        assert_eq!(id(of(P::Osc1PhaseRandom, 1, OSC_PARAMS)), "osc2_phase_random");
        assert_eq!(id(of(P::Filter1Type, 1, FILTER_PARAMS)), "filter2_type");
        assert_eq!(id(of(P::Filter1KeyTrack, 1, FILTER_PARAMS)), "filter2_key_track");
        assert_eq!(id(of(P::Env1Attack, 2, ENV_PARAMS)), "env3_attack");
        assert_eq!(id(of(P::Env1Release, 2, ENV_PARAMS)), "env3_release");
        assert_eq!(id(of(P::Lfo1Shape, 2, LFO_PARAMS)), "lfo3_shape");
        assert_eq!(id(of(P::Lfo1Phase, 2, LFO_PARAMS)), "lfo3_phase");
        for slot in 0..SLOTS {
            let number = slot + 1;
            assert_eq!(id(of(P::Mod1Source, slot, 3)), format!("mod{number}_source"));
            assert_eq!(id(of(P::Mod1Dest, slot, 3)), format!("mod{number}_dest"));
            assert_eq!(id(of(P::Mod1Amount, slot, 3)), format!("mod{number}_amount"));
        }
        assert_eq!(id(P::Volume as usize), "volume");
        assert_eq!(PARAMS.len(), P::MonoBelow as usize + 1);
    }

    #[test]
    fn the_choices_a_song_keeps_by_number_stay_where_they_are() {
        // These are in songs as numbers: nothing is ever put before them.
        assert_eq!((SOURCES[1], SOURCES[7], SOURCES[13], SOURCES[17]), ("env1", "velocity", "macro1", "random"));
        assert_eq!(
            (DESTINATIONS[1], DESTINATIONS[5], DESTINATIONS[16], DESTINATIONS[30]),
            ("pitch", "osc1_position", "filter1_cutoff", "lfo3_depth")
        );
        assert_eq!(
            (FILTERS[2], FILTERS[9], WARPS[3], LFO_SYNCS[6], MODES[2]),
            ("lowpass24", "comb", "squeeze", "1/4", "legato")
        );
    }
}
