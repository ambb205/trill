//! Tools for writing music in musical terms: chords, grooves, bass lines,
//! arpeggios and melodies, and for reading back what was written.
//!
//! Like the tools in `server`, these are thin: the musical knowledge lives
//! in `trill-theory`, and each tool turns a request into notes there and
//! puts them into a clip in one undoable edit. What this module adds is the
//! part a caller that cannot hear depends on: a reply that says in musical
//! terms what was written, and warnings about what would otherwise go
//! unnoticed.

use crate::input::{PitchInput, controller_scale, parse_controller, parse_pitch_value};
use crate::server::{Reply, TrillServer, add_controller_warnings, edit, explain, finish};
use crate::view::{bar_beat_at, round};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema, Schema, SchemaGenerator, json_schema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::BTreeMap;
use trill_core::controllers::splice;
use trill_core::device::{DeviceCategory, DeviceFactory};
use trill_core::pitch::pitch_name;
use trill_core::project::{AutomationPoint, Segment};
use trill_core::{ClipId, ControllerKind, InstrumentProfile, MeterMap, Note, TimeSignature, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::Session;
use trill_theory::arpeggio::{ArpOrder, Arpeggio, ORDERS};
use trill_theory::articulation::{articulate, unknown};
use trill_theory::bass::{BassLine, BassStyle, bass_line};
use trill_theory::chord::QUALITIES;
use trill_theory::drums::{FillPlan, arrange, drum_names};
use trill_theory::melody::{ORNAMENTS, Phrase, Tone};
use trill_theory::ornament::{Playing, perform};
use trill_theory::phrasing::{self, Phrasing};
use trill_theory::progression::{Comping, Progression, RHYTHMS, Rhythm, StrumDirection, comp};
use trill_theory::steps::LEGEND;
use trill_theory::timing::{note_value, snap_triplet};
use trill_theory::voicing::{Bass, VoicingSpec, VoicingStyle, muddy_intervals};
use trill_theory::{
    Dynamics, FILLS, Fill, GROOVES, Groove, Humanize, NoteName, Scale, ScaleKind, Swing, estimate_key, read_chords,
    summarize,
};

/// Appended to the server's instructions so a model knows these tools
/// exist and when to reach for them.
pub(crate) const INSTRUCTIONS: &str = "\
Writing in musical terms
Prefer these tools to spelling notes one at a time. They know chord symbols, voice leading, grooves and bass \
lines, and each replies with what it wrote (chord names, pitches, ranges) so you can check it without hearing it.
- write_chords: a progression such as \"Am9 | Fmaj7 | C G/B | E7\", or Roman numerals with a key, voiced and \
voice-led into a clip, in a rhythm (sustain, stabs, offbeats, tresillo, a step pattern...) with strum and dynamics.
- write_drums: a groove by name (house, boom_bap, trap, drum_and_bass, uk_garage, reggaeton, funk, rock, \
half_time, shuffle, six_eight, bossa_nova, jazz_swing) with an optional fill, and/or your own lanes in step \
notation, e.g. {\"kick\": \"x...x...x...x...\", \"snare\": \"....x.......x...\"}. Drums are named, and the \
name lands on the key the track's kit uses: china, splash, ride_bell, crash2, floor_tom, rimshot, \
closed_hat_edge, half_open_hat, wide_open_hat beside the usual ones. A fill's reply says what it replaced.
- write_bass and write_arpeggio: a bass line or an arpeggio that follows the same progression text.
- write_melody: one line as scale degrees or note names with lengths, e.g. \":0.5 5 6 5 4 3:2 | 1:4\". A lead \
is more than its notes: mark bends, slides, vibrato and hammer-ons on the notes that have them \
(\"4:2!bend=5!vib\", \"6!hammer\", \"3!slide\", \"5:2!fall\"). The tool writes the instrument's own keyswitch \
where it has one and a pitch-bend curve where not; never draw such curves by hand. Keep a line with ornaments \
on a track of its own: the pitch wheel bends every note of a track at once.
- shape_dynamics: gives a line its dynamics from its notes, as an expression curve in the clip: a rise into each \
phrase's high point, a swell through long notes, a breath between phrases. Call it after writing a melody for a \
wind, brass or string instrument (those modelled on breath or a bow make no sound without it), and whenever a \
line should breathe rather than sit at one level.
- analyze_music: the key, the chord in each bar, the range and density of a clip, a track or the whole song. Use \
it to check your work, and to read what the user wrote before adding to it.
- music_reference: the vocabulary and the notations (chord symbols, scales, voicings, rhythms, grooves, fills, \
ornaments, drum names, what a track knows about its instrument). Look things up there instead of guessing.

Every writing tool takes `track` (it creates a clip there) or `clip` (it writes into that clip), is one undo \
step, and gives the same result for the same arguments. Its reply gives `written_beats` (what was just written) \
and `clip_length` (the clip it is in, which is longer when the written bars loop). The pitches you ask for are \
the pitches that are heard: for an instrument that sounds an octave from its keys the tool writes the other \
octave, and says so. Give all parts of a section the same progression, key \
and swing so they agree. A song usually goes: set_project, create_track per part, write_drums, write_chords, \
write_bass, write_arpeggio, write_melody, analyze_music, then effects, automation and render.
Use create_clip and add_notes for what these cannot say: single hits, sound effects, a voicing spelled by hand.";

const MAX_BARS: u32 = 512;
const MAX_NOTES: usize = 50_000;
/// The most chords write_chords lists back.
const SHOWN_CHORDS: usize = 32;
/// The most fill bars write_drums describes one by one.
const SHOWN_FILLS: usize = 4;
const EPSILON: f64 = 1e-6;

/// A length given as a number of beats or as a note value.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct LengthInput(pub Value);

impl JsonSchema for LengthInput {
    fn schema_name() -> Cow<'static, str> {
        "Length".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A length: a number of beats (quarter notes; 0.25 is a sixteenth, 0.33 a triplet eighth) \
                or a note value as a string (\"1/16\", \"1/8\", \"1/8t\" for a triplet, \"1/8d\" for dotted).",
            "type": ["number", "string"]
        })
    }
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Remove the notes already in the bars being written, then write.
    #[default]
    Replace,
    /// Keep what is there and layer the new notes on top.
    Add,
}

/// Where a writing tool puts its notes.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Target {
    /// Track id. A new clip is created on this track. Give this or `clip`.
    pub track: Option<u64>,
    /// With `track`: where the new clip starts on the timeline, in beats (default 0; bar N of 4/4 is (N-1)*4;
    /// in a song whose time signature changes, get_tempo_map gives the beat each bar starts at). The bars
    /// written are the song's from there on, each in the time signature it has: a bar of 7/8 is 3.5 beats.
    pub start: Option<f64>,
    /// With `track`: name of the new clip.
    pub name: Option<String>,
    /// With `track`: length of the new clip in beats, if it should differ from what is written. When longer,
    /// the written bars loop to fill it (write 2 bars of drums, loop them for 16).
    pub clip_length: Option<f64>,
    /// Id of an existing MIDI clip to write into, instead of creating one.
    pub clip: Option<u64>,
    /// With `clip`: beats from the start of the clip at which writing begins (default 0).
    pub offset: Option<f64>,
    /// With `clip`: "replace" (default) clears the bars being written first; "add" layers on top of them.
    pub mode: Option<WriteMode>,
    /// How many bars to write. What you give repeats, or is cut, to fill them. Defaults to its own length.
    pub bars: Option<u32>,
}

/// Timing feel shared by the writing tools. Give every part of a groove the
/// same swing.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Feel {
    /// Swing, 50-75: where the second of each pair of steps lands. 50 is straight, 54-60 a light shuffle,
    /// 66.7 full triplet swing. Named grooves bring their own; give 50 to play one straight.
    pub swing: Option<f64>,
    /// The steps that swing: "1/16" (default: house, hip-hop, garage) or "1/8" (jazz, blues).
    pub swing_unit: Option<LengthInput>,
    /// Randomly move each note by up to this many beats (0.01-0.03 sounds human).
    pub humanize_timing: Option<f64>,
    /// Randomly change each velocity by up to this much (4-12 sounds human).
    pub humanize_velocity: Option<f64>,
    /// Seed for the humanizing and for random arpeggios; the same seed gives the same result.
    pub seed: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteChordsRequest {
    #[serde(flatten)]
    pub target: Target,
    /// The chords, bar by bar: "Am9 | Fmaj7 | C G/B | E7". Chords in a bar share it evenly; "." holds the
    /// previous chord another share ("Dm7 . . G7"), "%" repeats the previous bar, "C:3 G:1" gives lengths in
    /// beats, "N.C." is silence. Without bar lines each chord gets a bar. Symbols are the usual ones (m7,
    /// maj7, 7sus4, m7b5, dim7, 6/9, add9, 7#9b13, slash bass); with `key`, Roman numerals work too
    /// ("ii7 V7 | Imaj7", "i bVI bIII bVII", "V7/V").
    pub progression: String,
    /// The key, for Roman numerals and for naming notes: "A minor", "F# dorian", "Eb", "Cm".
    pub key: Option<String>,
    /// How the chords are laid out: "close" (default), "open", "drop2", "drop3", "shell" (root, third,
    /// seventh) or "pad" (root and fifth low, colour tones above). Each chord is led smoothly from the last.
    pub voicing: Option<String>,
    /// Lowest note of the chord proper, not counting a bass note (default E3).
    pub low: Option<PitchInput>,
    /// Highest note (default E5). Leave about two octaves between `low` and `high`.
    pub high: Option<PitchInput>,
    /// Most notes in the chord proper (default 4). Bigger chords drop the fifth first, then the root.
    pub voices: Option<u8>,
    /// A bass note under each chord: "none" (default; another track has the bass), "root" or "octaves".
    /// The pad voicing always has its own.
    pub bass: Option<String>,
    /// Play every chord in this inversion (0 = root position) instead of leading each from the last.
    pub inversion: Option<u8>,
    /// When the chords are struck: "sustain" (default), "stabs", "pulse", "half", "eighths", "offbeats",
    /// "charleston", "tresillo", "clave", or a step pattern for one or more bars such as "x__x__x_" or
    /// "x.x.|..x." (x strike, X accent, g soft, . rest, _ hold).
    pub rhythm: Option<String>,
    /// Beats by which each chord change arrives early and is held over the beat (0.5 or 0.25 gives the
    /// "push" of pop, funk and Latin playing).
    pub anticipation: Option<f64>,
    /// Fraction of each strike that sounds (default 1 = legato; 0.5 detached).
    pub gate: Option<f64>,
    /// Beats between the onsets of neighbouring notes of a chord: 0 together (default), 0.02-0.06 strummed.
    pub strum: Option<f64>,
    /// "up" (default, low note first), "down" or "alternate".
    pub strum_direction: Option<String>,
    /// Velocity of an ordinary strike, 1-127 (default 84).
    pub velocity: Option<u8>,
    /// Extra velocity on the downbeat of each bar, half of it on the other beats (default 8).
    pub accent: Option<u8>,
    /// How every note is played, by the name of one of the track's articulations ("palm_mute", "muted";
    /// get_project lists them). The keyswitch is pressed for you. Omit for the instrument's ordinary way.
    pub articulation: Option<String>,
    #[serde(flatten)]
    pub feel: Feel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteDrumsRequest {
    #[serde(flatten)]
    pub target: Target,
    /// A groove to start from: "house", "boom_bap", "trap", "drum_and_bass", "uk_garage", "reggaeton", "funk",
    /// "rock", "half_time", "shuffle", "six_eight", "bossa_nova" or "jazz_swing" (music_reference topic
    /// "grooves" describes them).
    pub groove: Option<String>,
    /// Lanes by drum name, added to the groove or replacing its lane for that drum. A name is one of the
    /// track's own drums (get_instrument_profile lists its map) or a General MIDI one: kick, snare, rimshot,
    /// rim, clap, closed_hat, closed_hat_edge, pedal_hat, half_open_hat, open_hat, wide_open_hat, low_tom,
    /// mid_tom, high_tom, floor_tom, crash, crash2, china, splash, ride, ride_bell, tambourine, cowbell,
    /// shaker... Where a kit has no key for a drum the nearest is played and the reply says so. One character per step:
    /// x hit, X accent, g ghost note, . rest, _ lengthens the hit before it, a digit after a hit makes a roll
    /// (x3_ is three hits across two steps), | separates bars. Each bar's characters divide the bar evenly:
    /// 16 make sixteenths, 12 triplet eighths, 8 eighths. A lane of only rests removes that drum.
    /// Example: {"kick": "x...x...x...x...", "clap": "....x.......x...", "closed_hat": "x.x.x.x.x.x.x3_x."}.
    pub lanes: Option<BTreeMap<String, String>>,
    /// A fill for the end of the phrase: "snare_roll", "toms", "build", "drop", "trap_roll", "triplet_toms"
    /// or "six_eight_toms". The bar after a fill starts with a crash. The reply lists, for each bar a fill
    /// was played in, the hits of the groove it took the place of and the hits it played.
    pub fill: Option<String>,
    /// Put the fill at the end of every this-many bars (4 or 8 is usual). Default: only the last bar.
    pub fill_every: Option<u32>,
    /// Overall loudness: 1 is the usual balance, 0.8 softer (default 1).
    pub level: Option<f64>,
    #[serde(flatten)]
    pub feel: Feel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteArpeggioRequest {
    #[serde(flatten)]
    pub target: Target,
    /// The chords to arpeggiate, in the same notation as write_chords: "Am9 | Fmaj7 | C G/B | E7".
    pub progression: String,
    /// The key, for Roman numerals and for naming notes.
    pub key: Option<String>,
    /// The order: "up" (default), "down", "up_down", "up_down_inclusive", "down_up", "played" (root, third,
    /// fifth...) or "random".
    pub order: Option<String>,
    /// Instead of `order`, chord tones by number from the lowest: 1 is the bottom note, numbers past the top
    /// continue an octave up, 0 rests. [1, 3, 2, 3] is an Alberti bass; [1, 2, 3, 5, 3, 2] a rolling figure.
    pub pattern: Option<Vec<u32>>,
    /// How many octaves the figure climbs through (default 1).
    pub octaves: Option<u8>,
    /// Length of each step (default "1/16").
    pub rate: Option<LengthInput>,
    /// Fraction of each step that sounds (default 0.8; above 1 the notes overlap).
    pub gate: Option<f64>,
    /// Lowest note of the chords being arpeggiated (default A3).
    pub low: Option<PitchInput>,
    /// Highest note before `octaves` extends the figure upward (default A5).
    pub high: Option<PitchInput>,
    /// Most chord tones used (default 4).
    pub voices: Option<u8>,
    /// Velocity of an ordinary note, 1-127 (default 84).
    pub velocity: Option<u8>,
    /// Extra velocity on the downbeat of each bar, half of it on the other beats (default 12).
    pub accent: Option<u8>,
    /// How every note is played, by the name of one of the track's articulations ("palm_mute", "muted";
    /// get_project lists them). The keyswitch is pressed for you. Omit for the instrument's ordinary way.
    pub articulation: Option<String>,
    #[serde(flatten)]
    pub feel: Feel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteBassRequest {
    #[serde(flatten)]
    pub target: Target,
    /// The chords the bass follows, in the same notation as write_chords. A slash chord's bass note is played.
    pub progression: String,
    /// The key, for Roman numerals and for naming notes.
    pub key: Option<String>,
    /// Which notes: "roots" (default), "root_fifth", "octaves" or "walking" (a note per beat, stepping
    /// chromatically into each new root).
    pub style: Option<String>,
    /// When they are played: "sustain", "pulse", "half", "eighths", "offbeats", "tresillo", or a step pattern
    /// such as "x..x..x.x.x..x..". Default: sustain for roots, half for root_fifth, eighths for octaves.
    /// Walking ignores it.
    pub rhythm: Option<String>,
    /// Lowest note; each root sits in the octave above it (default E1).
    pub low: Option<PitchInput>,
    /// Highest note (default G3).
    pub high: Option<PitchInput>,
    /// Fraction of each note that sounds (default 0.9).
    pub gate: Option<f64>,
    /// Beats by which each chord change arrives early (see write_chords).
    pub anticipation: Option<f64>,
    /// Velocity of an ordinary note, 1-127 (default 96).
    pub velocity: Option<u8>,
    /// Extra velocity on the downbeat of each bar, half of it on the other beats (default 10).
    pub accent: Option<u8>,
    /// How every note is played, by the name of one of the track's articulations ("palm_mute", "muted";
    /// get_project lists them). The keyswitch is pressed for you. Omit for the instrument's ordinary way.
    pub articulation: Option<String>,
    #[serde(flatten)]
    pub feel: Feel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteMelodyRequest {
    #[serde(flatten)]
    pub target: Target,
    /// The line, note by note: ":0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4". A pitch is a scale degree of `key`
    /// (1-7; b3 and #4 alter it; 5' is an octave up, 7, an octave down) or a note name with its octave (C4,
    /// F#5). "5:0.5" gives that note a length in beats; a bare ":0.5" sets the length of the notes after it
    /// (1 beat to begin with). "r" rests, "~" lengthens the note before it, "|" marks a bar and is checked.
    /// Ornaments are marks after a note, each begun by "!": "4:2!bend=5" bends up to the fifth degree,
    /// "!bend=5!release" lets it back down, "!prebend=5", "5:2!vib" (vibrato), "6!hammer" and "5!pull" join a
    /// note to the one before with no new attack, "3!slide" slides into it, "5:2!fall" drops off at the end;
    /// "1!palm_mute" asks for one of the instrument's own articulations. They combine: "4:2!bend=5!vib".
    pub phrase: String,
    /// The key the degrees are in: "A minor", "D dorian", "Bb". Not needed for note names.
    pub key: Option<String>,
    /// The octave degree 1 is in (default 4, so in C major 1 is middle C).
    pub octave: Option<i32>,
    /// Fraction of each note that sounds (default 0.95).
    pub gate: Option<f64>,
    /// Velocity of an ordinary note, 1-127 (default 90).
    pub velocity: Option<u8>,
    /// Extra velocity on the downbeat of each bar, half of it on the other beats (default 8).
    pub accent: Option<u8>,
    /// How every note without a mark of its own is played, by the name of one of the track's articulations
    /// ("palm_mute"). The keyswitch is pressed for you.
    pub articulation: Option<String>,
    #[serde(flatten)]
    pub feel: Feel,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShapeDynamicsRequest {
    /// Id of the MIDI clip whose notes are to be given dynamics.
    pub clip: u64,
    /// The controller to write the curve for: "expression" (default), "breath", "volume", "mod_wheel",
    /// "pressure" or a controller number. Use the one the instrument listens to.
    pub controller: Option<String>,
    /// The level of the line's highest point, 0-127 (default 114). Lower phrases peak below it.
    pub high: Option<f64>,
    /// The quietest a sounding note gets, 0-127 (default 57).
    pub low: Option<f64>,
    /// The level between phrases, 0-127 (default 0: the instrument falls silent as the player breathes).
    /// Give it the value of `low` for an instrument that should not dip between phrases.
    pub rest: Option<f64>,
    /// How much long notes swell, 0-1 (default 0.5; 0 for none).
    pub swell: Option<f64>,
    /// A rest of at least this many beats separates two phrases (default 0.5).
    pub phrase_gap: Option<f64>,
    /// Notes at least this many beats long swell (default 2).
    pub long_note: Option<f64>,
    /// Shape only the notes starting at or after this beat (relative to the clip); the rest of the curve is
    /// left as it is.
    pub start: Option<f64>,
    /// Shape only the notes starting before this beat (relative to the clip).
    pub end: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AnalyzeMusicRequest {
    /// Analyze this clip.
    pub clip: Option<u64>,
    /// Analyze every clip on this track. With neither `clip` nor `track`, the whole song is analyzed (drum
    /// tracks left out).
    pub track: Option<u64>,
    /// First beat to look at (default: the start of the music).
    pub start: Option<f64>,
    /// Beat to stop at (default: the end of the music).
    pub end: Option<f64>,
    /// Name a chord per "bar" (default) or per "half_bar".
    pub resolution: Option<String>,
    /// The key you intended. Notes outside it are listed, and chords are spelled in it. Default: the
    /// estimated key.
    pub key: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MusicReferenceRequest {
    /// "chords", "progressions", "scales", "voicings", "rhythms", "arpeggios", "bass", "drums", "grooves" or
    /// "melody". Omit for an overview of all of them.
    pub topic: Option<String>,
}

fn theory(error: trill_theory::Error) -> String {
    error.to_string()
}

fn parse_key(key: Option<&str>) -> Result<Option<Scale>, String> {
    key.map(|text| Scale::parse(text).map_err(theory)).transpose()
}

fn pitch_or(input: Option<&PitchInput>, default: u8, what: &str) -> Result<u8, String> {
    match input {
        Some(pitch) => parse_pitch_value(&pitch.0).map_err(|error| format!("`{what}`: {error}")),
        None => Ok(default),
    }
}

fn length_or(input: Option<&LengthInput>, default: f64, what: &str) -> Result<f64, String> {
    let Some(input) = input else { return Ok(default) };
    let beats = match &input.0 {
        Value::Number(number) => number.as_f64().map(snap_triplet),
        Value::String(text) => match text.trim().parse::<f64>() {
            Ok(beats) => Some(snap_triplet(beats)),
            Err(_) => Some(note_value(text).map_err(|error| format!("`{what}`: {error}"))?),
        },
        _ => None,
    };
    match beats {
        Some(beats) if beats.is_finite() && beats > 0.0 => Ok(beats),
        _ => Err(format!("`{what}` must be a positive number of beats or a note value like \"1/16\", got {}", input.0)),
    }
}

/// Checks an optional number against a range, naming the field when it is out.
fn within(value: Option<f64>, default: f64, range: std::ops::RangeInclusive<f64>, what: &str) -> Result<f64, String> {
    match value {
        None => Ok(default),
        Some(value) if value.is_finite() && range.contains(&value) => Ok(value),
        Some(value) => Err(format!("`{what}` {value} is outside {}..={}", range.start(), range.end())),
    }
}

fn dynamics(velocity: Option<u8>, accent: Option<u8>, defaults: (u8, u8)) -> Result<Dynamics, String> {
    let velocity = velocity.unwrap_or(defaults.0);
    if !(1..=127).contains(&velocity) {
        return Err(format!("`velocity` {velocity} is outside 1..=127"));
    }
    Ok(Dynamics { velocity, accent: accent.unwrap_or(defaults.1).min(60) })
}

/// The name of a pitch: as the key spells it if there is one, else the
/// everyday name (Bb rather than A#, F# rather than Gb).
fn name_pitch(pitch: u8, key: Option<&Scale>) -> String {
    key.map_or_else(|| NoteName::of_pitch_class(pitch % 12).spell(pitch), |key| key.spell(pitch))
}

/// Song position as `bar.beat`, both counted from 1.
fn bar_beat(beat: f64, bar_length: f64) -> String {
    let bar = (beat / bar_length + EPSILON).floor();
    format!("{}.{}", bar as i64 + 1, round(beat - bar * bar_length + 1.0))
}

/// What a track plays its notes through.
#[derive(Clone, Debug, Default, PartialEq)]
struct Instrument {
    /// Whether the track has an instrument at all.
    exists: bool,
    /// Whether its notes are drums: the track says so, or its instrument
    /// is a drum instrument.
    drums: bool,
    /// The built-in instrument it is, if it is one, by URI.
    builtin: Option<String>,
    /// The preset a built-in instrument appears to be on.
    preset: Option<String>,
}

fn instrument(session: &Session, track: TrackId) -> Instrument {
    let Ok(found) = session.project().track(track) else { return Instrument::default() };
    let drums = session.plays_drums(track);
    // Only the built-in catalog is consulted for presets: it is instant.
    let builtins = BuiltinFactory.descriptors();
    for device in &found.devices {
        let Some(descriptor) = builtins.iter().find(|descriptor| descriptor.uri == device.uri) else {
            // A plugin: the first one in the chain is the instrument.
            return Instrument { exists: true, drums, builtin: None, preset: None };
        };
        if descriptor.category != DeviceCategory::Instrument {
            continue;
        }
        // The model stores parameter values, not the preset's name; the
        // preset whose values are all still in place is the one.
        let preset = descriptor
            .presets
            .iter()
            .filter(|preset| !preset.params.is_empty())
            .filter(|preset| {
                preset
                    .params
                    .iter()
                    .all(|(id, value)| device.params.get(id).is_some_and(|current| (current - value).abs() < 1e-6))
            })
            .max_by_key(|preset| preset.params.len())
            .map(|preset| preset.name.clone());
        return Instrument { exists: true, drums, builtin: Some(device.uri.clone()), preset };
    }
    Instrument { drums, ..Instrument::default() }
}

/// What kind of part a tool writes, for checking it against the track.
#[derive(Copy, Clone, PartialEq)]
enum PartKind {
    Drums,
    Pitched,
}

/// Problems with putting these notes on this track that cannot be heard
/// from the reply alone. `notes` are the notes of music, as written.
fn instrument_warnings(
    session: &Session,
    track: TrackId,
    kind: PartKind,
    notes: &[Note],
    profile: &InstrumentProfile,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let count_below = |limit: u8| notes.iter().filter(|note| note.pitch < limit).count();
    let count_above = |limit: u8| notes.iter().filter(|note| note.pitch > limit).count();
    let found = instrument(session, track);
    match (found.drums, kind) {
        _ if !found.exists => {
            warnings
                .push(format!("Track {} has no instrument, so this will be silent; add one with add_device.", track.0));
        }
        (true, PartKind::Pitched) => warnings.push(format!(
            "Track {} is a drum track: these pitches will trigger drums, not notes. Write this on a track with a \
             pitched instrument (or, if this instrument is one, say so with set_instrument_profile role \
             \"pitched\").",
            track.0
        )),
        (false, PartKind::Drums) => warnings.push(format!(
            "Track {} is not a drum track: these drum notes will play as pitches. Write drums on a track whose \
             instrument is a drum kit (builtin:drumkit, or a drum plugin); if this instrument is one that does not \
             say so, set_instrument_profile role \"drums\" tells the track.",
            track.0
        )),
        (false, PartKind::Pitched) => match found.preset {
            Some(preset) if preset.contains("bass") => {
                let high = count_above(60);
                if high > 0 {
                    warnings.push(format!(
                        "{high} note(s) are above C4, where the '{preset}' preset sounds thin; it is voiced for \
                         about E1-G2. Lower the register or use another preset."
                    ));
                }
            }
            Some(preset) => {
                let low = count_below(36);
                if low > 0 {
                    warnings.push(format!(
                        "{low} note(s) are below C2, lower than the '{preset}' preset is voiced for; they will \
                         sound muddy or vanish. Raise the register, or use a bass preset for a bass part."
                    ));
                }
            }
            None => {}
        },
        (true, PartKind::Drums) => {}
    }
    if kind == PartKind::Pitched {
        // A note of music on a key that switches is silent, and changes the notes after it.
        let switching: Vec<&Note> = notes.iter().filter(|note| profile.is_keyswitch(note.pitch)).collect();
        if !switching.is_empty() {
            let mut names: Vec<&str> = switching.iter().filter_map(|note| profile.keyswitch_name(note.pitch)).collect();
            names.sort_unstable();
            names.dedup();
            warnings.push(format!(
                "{} note(s) of this part fall on keys that are keyswitches of this instrument ({}): they make no                  sound, and change how the notes after them are played. Move the part into the instrument's range.",
                switching.len(),
                names.join(", ")
            ));
        }
        // What the track knows of its instrument's range, else what speakers can do.
        warnings.extend(crate::instruments::note_warnings(profile, found.drums, notes, usize::MAX));
        if profile.range.is_none() && count_below(24) > 0 {
            warnings.push(format!("{} note(s) are below C1, under what most speakers reproduce.", count_below(24)));
        }
    }
    warnings
}

/// Where the notes go, worked out before anything is written.
struct Destination {
    track: TrackId,
    clip: Option<ClipId>,
    /// Song position of the first beat written, which swing is measured from.
    origin: f64,
    /// Beats into the clip where writing starts.
    offset: f64,
}

fn destination(session: &Session, target: &Target) -> Result<Destination, String> {
    let project = session.project();
    match (target.clip, target.track) {
        (Some(_), Some(_)) => {
            Err("give either `clip` (write into that clip) or `track` (create a new clip there), not both".to_string())
        }
        (None, None) => Err("give a `track` to create a clip on, or a `clip` to write into".to_string()),
        (Some(clip), None) => {
            if target.start.is_some() || target.name.is_some() || target.clip_length.is_some() {
                return Err("`start`, `name` and `clip_length` describe a new clip and go with `track`; with `clip`, \
                            use `offset` to choose where in it to write"
                    .to_string());
            }
            let id = ClipId(clip);
            let found = project.clip(id).map_err(|error| explain(project, error))?;
            if found.midi().is_none() {
                return Err(format!("{id} is an audio clip and has no notes"));
            }
            let offset = within(target.offset, 0.0, 0.0..=1e6, "offset")?;
            let track = project.clip_track(id).map_err(|error| explain(project, error))?;
            Ok(Destination { track, clip: Some(id), origin: found.start + offset, offset })
        }
        (None, Some(track)) => {
            if target.offset.is_some() || target.mode.is_some() {
                return Err("`offset` and `mode` apply when writing into an existing `clip`; with `track`, use \
                            `start` to place the new clip"
                    .to_string());
            }
            let id = TrackId(track);
            let found = project.track(id).map_err(|error| explain(project, error))?;
            if found.kind != TrackKind::Normal {
                return Err(format!("{id} is a group or return track and does not hold clips"));
            }
            Ok(Destination {
                track: id,
                clip: None,
                origin: within(target.start, 0.0, 0.0..=1e6, "start")?,
                offset: 0.0,
            })
        }
    }
}

/// What a tool hands back to [`write`]: the notes and what to say about them.
#[derive(Default)]
struct Part {
    /// The notes, at the pitches that are to be heard.
    notes: Vec<Note>,
    /// The articulation each note asks of the instrument, in step with
    /// `notes`; empty when none asks for one.
    how: Vec<Option<String>>,
    /// The pitch wheel's course under the notes, in beats like theirs.
    /// `None` for a part that leaves the wheel, and whatever curve the clip
    /// has for it, alone.
    wheel: Option<Vec<AutomationPoint>>,
    /// Bars the notes cover.
    bars: usize,
    /// Fields for the reply, in musical terms.
    summary: Map<String, Value>,
    /// Likely mistakes.
    warnings: Vec<String>,
    /// How something was read, for information.
    remarks: Vec<String>,
    /// The swing the material is normally played with: percent and unit.
    usual_swing: Option<(f64, f64)>,
    /// The drum kit preset the material is written for.
    usual_kit: Option<&'static str>,
    key: Option<Scale>,
    /// The notes are one line, each following the last, and not bars that
    /// each stand for themselves: it cannot be laid bar by bar into bars
    /// of different lengths.
    line: bool,
}

/// One articulation for every note of a part, if one was asked for.
fn all_played(articulation: Option<&str>, notes: &[Note]) -> Vec<Option<String>> {
    match articulation {
        Some(name) => vec![Some(name.to_string()); notes.len()],
        None => Vec::new(),
    }
}

/// The settings a part is built against: the song's, where it is written.
struct Setting {
    signature: TimeSignature,
    tempo: f64,
    /// Bars asked for, if any.
    bars: Option<usize>,
    /// What the track knows about its instrument.
    profile: InstrumentProfile,
}

/// How long before the stretch being written a keyswitch of an earlier
/// write may lie, in beats.
const SWITCH_REACH: f64 = 0.1;

/// The shared half of every writing tool: resolve the target, build the
/// part, fit it to the instrument, apply the feel, write it as one undo
/// step and describe the result.
fn write(
    session: &mut Session,
    target: &Target,
    feel: &Feel,
    kind: PartKind,
    default_name: &str,
    build: impl Fn(&Setting) -> Result<Part, String>,
) -> Reply {
    let destination = destination(session, target)?;
    // The meter and the tempo the song has where the part begins.
    let meter = session.project().meter_map();
    let signature = meter.signature_at(destination.origin);
    let tempo = session.project().tempo_map().tempo_at(destination.origin);
    let bar_length = signature.beats_per_bar();
    let bars = match target.bars {
        Some(bars) if !(1..=MAX_BARS).contains(&bars) => {
            return Err(format!("`bars` {bars} is outside 1..={MAX_BARS}"));
        }
        bars => bars.map(|bars| bars as usize),
    };
    // With how far its instrument's wheel bends, where the track says nothing of it.
    let profile = session.profile_of(destination.track);
    let mut part = build(&Setting { signature, tempo, bars, profile: profile.clone() })?;
    // The bars it is written into are the song's from there on. Where
    // they are not all the first one's, each is laid in its own meter.
    let places = written_bars(&meter, destination.origin, part.bars);
    let even = places.iter().all(|bar| bar.signature == signature && (bar.length - bar_length).abs() < EPSILON);
    if !even {
        lay_in_bars(&mut part, &places, signature, |signature| {
            build(&Setting { signature, tempo, bars: Some(places.len()), profile: profile.clone() })
        })?;
    }
    if part.notes.len() > MAX_NOTES {
        return Err(format!("that would write {} notes; write fewer bars or a slower rate", part.notes.len()));
    }
    let heard = summarize(&part.notes);

    // An instrument that sounds an octave from its keys is written the
    // other way, so that what was asked for is what is heard.
    let pitched = kind == PartKind::Pitched;
    if pitched && profile.sounds_octaves != 0 {
        for note in &mut part.notes {
            note.pitch = profile.written(note.pitch).ok_or_else(|| {
                format!(
                    "{} cannot be played: this instrument sounds {} octave(s) from its keys, and there is no key \
                     that far {}",
                    pitch_name(note.pitch),
                    profile.sounds_octaves,
                    if profile.sounds_octaves < 0 { "up" } else { "down" }
                )
            })?;
        }
    }

    // Feel: swing against the song's grid, then the human touch.
    let swing = match (feel.swing, part.usual_swing) {
        (Some(percent), usual) => {
            let unit = length_or(feel.swing_unit.as_ref(), usual.map_or(0.25, |(_, unit)| unit), "swing_unit")?;
            Some(Swing::new(percent, unit).map_err(theory)?)
        }
        (None, Some((percent, unit))) => {
            Some(Swing::new(percent, length_or(feel.swing_unit.as_ref(), unit, "swing_unit")?).map_err(theory)?)
        }
        (None, None) => None,
    };
    if let Some(swing) = swing.filter(|swing| swing.percent > 50.0) {
        if even {
            swing.apply(&mut part.notes, destination.origin);
            for point in part.wheel.iter_mut().flatten() {
                point.beat = (swing.warp(point.beat + destination.origin) - destination.origin).max(0.0);
            }
        } else {
            // Each bar swings from its own bar line: a bar of an odd
            // number of steps would otherwise turn the next one round.
            for bar in &places {
                let inside = |beat: f64| beat >= bar.start - EPSILON && beat < bar.start + bar.length - EPSILON;
                for note in part.notes.iter_mut().filter(|note| inside(note.start)) {
                    let mut within = [Note { start: note.start - bar.start, ..*note }];
                    swing.apply(&mut within, 0.0);
                    *note = Note { start: within[0].start + bar.start, ..within[0] };
                }
                for point in part.wheel.iter_mut().flatten().filter(|point| inside(point.beat)) {
                    point.beat = (swing.warp(point.beat - bar.start) + bar.start).max(0.0);
                }
            }
        }
        part.summary.insert("swing".into(), json!(swing.percent));
    }
    let timing = within(feel.humanize_timing, 0.0, 0.0..=0.25, "humanize_timing")?;
    let velocity = within(feel.humanize_velocity, 0.0, 0.0..=40.0, "humanize_velocity")?;
    if timing > 0.0 || velocity > 0.0 {
        Humanize { timing, velocity, seed: feel.seed.unwrap_or(1) as u64 }.apply(&mut part.notes);
    }

    // The instrument's keyswitches, pressed where they belong, and the
    // velocities that play what was asked for.
    let music = part.notes.clone();
    let offset = destination.offset;
    let mut presses: BTreeMap<String, usize> = BTreeMap::new();
    if pitched && !profile.articulations.is_empty() {
        let how = |index: usize| part.how.get(index).cloned().flatten();
        let asked: Vec<(Note, Option<String>)> =
            part.notes.iter().enumerate().map(|(index, note)| (*note, how(index))).collect();
        let articulated = articulate(&profile, &asked, offset).map_err(theory)?;
        if let (Some((low, high)), true) = (articulated.default_range, articulated.fitted > 0) {
            part.remarks.push(format!(
                "{} note(s) were outside the velocities {low}-{high} that play this instrument's ordinary note \
                 (softer or harder it plays them another way: {}); they were moved to the nearest of those. Ask \
                 for the other way by name if it was meant.",
                articulated.fitted,
                other_hits(&profile).join(", ")
            ));
        }
        presses = articulated.presses;
        part.notes = articulated.notes;
    } else if let Some(name) = part.how.iter().flatten().next() {
        return Err(theory(unknown(&profile, name)));
    }

    let written = match even {
        true => part.bars as f64 * bar_length,
        false => places.iter().map(|bar| bar.length).sum(),
    };
    for note in &mut part.notes {
        note.start = (note.start + offset).max(0.0);
    }
    let clip_length = match target.clip_length {
        Some(length) if !length.is_finite() || length <= 0.0 => {
            return Err(format!("`clip_length` {length} must be > 0"));
        }
        Some(length) => length,
        None => written,
    };
    let replace = target.mode.unwrap_or_default() == WriteMode::Replace;
    let name = target.name.clone().unwrap_or_else(|| default_name.to_string());
    let (track, notes) = (destination.track, &part.notes);
    // The wheel's course within the stretch that was written.
    let wheel: Option<Vec<AutomationPoint>> = part.wheel.as_ref().map(|points| {
        points
            .iter()
            .map(|point| AutomationPoint { beat: point.beat + offset, ..*point })
            .filter(|point| point.beat <= offset + written + EPSILON)
            .collect()
    });
    let clip = edit(session, |project| match destination.clip {
        Some(id) => {
            let clip = project.clip_mut(id)?;
            if replace {
                let inside = |note: &Note| note.start >= offset - EPSILON && note.start < offset + written - EPSILON;
                // With them go the keyswitches an earlier write pressed just before the stretch.
                let leading = |note: &Note| {
                    profile.is_keyswitch(note.pitch) && note.start >= offset - SWITCH_REACH && note.start < offset
                };
                clip.remove_notes(|note| inside(note) || leading(note))?;
            }
            clip.add_notes(notes)?;
            if let Some(wheel) = &wheel {
                let kind = ControllerKind::PitchBend;
                let existing = clip.controller(kind).map(|curve| curve.points.clone()).unwrap_or_default();
                // A line without ornaments still takes the wheel back from what was there.
                let rest = vec![AutomationPoint { beat: offset, value: 0.0, shape: Segment::Hold }];
                let piece = if wheel.is_empty() { &rest } else { wheel };
                if !(wheel.is_empty() && existing.is_empty()) {
                    let points = match existing.is_empty() {
                        true => piece.clone(),
                        false => splice(&existing, offset, Some(offset + written), piece),
                    };
                    clip.set_controller(kind, points)?;
                }
            }
            Ok(id)
        }
        None => {
            let id = project.add_midi_clip(track, name, destination.origin, clip_length)?;
            let clip = project.clip_mut(id)?;
            clip.add_notes(notes)?;
            if clip_length > written + EPSILON {
                clip.set_loop_length(Some(written))?;
            }
            if let Some(wheel) = wheel.as_ref().filter(|wheel| !wheel.is_empty()) {
                clip.set_controller(ControllerKind::PitchBend, wheel.clone())?;
            }
            Ok(id)
        }
    })?;

    let found = instrument(session, track);
    let mut warnings = std::mem::take(&mut part.warnings);
    warnings.extend(instrument_warnings(session, track, kind, &music, &profile));
    // Suggest the kit a groove is written for, where the track plays the
    // built-in kit and is on another.
    if let (Some(kit), Some("builtin:drumkit")) = (part.usual_kit, found.builtin.as_deref())
        && found.preset.as_deref().unwrap_or("default") != kit
    {
        part.remarks.push(format!(
            "This groove is written for the '{kit}' drum kit preset; the track is on '{}'. update_device can switch it.",
            found.preset.as_deref().unwrap_or("default")
        ));
    }
    warnings.extend(outside_clip(session, clip));
    if wheel.as_ref().is_some_and(|wheel| !wheel.is_empty()) {
        warnings.extend(bent_together(session, track, destination.origin, written));
    }

    let mut out = Map::new();
    out.insert("clip_id".into(), json!(clip));
    out.insert("track".into(), json!(track));
    if destination.clip.is_none() {
        out.insert("created".into(), json!(true));
        if clip_length > written + EPSILON {
            out.insert(
                "loops".into(),
                json!(format!("{} beats written, looped to fill {}", round(written), round(clip_length))),
            );
        }
    }
    out.insert("bars".into(), json!(part.bars));
    // Two lengths that are easy to take for one another: what was written
    // now, and the clip it is in.
    out.insert("written_beats".into(), json!(round(written)));
    if let Ok(clip) = session.project().clip(clip) {
        out.insert("clip_length".into(), json!(round(clip.length)));
    }
    out.insert("note_count".into(), json!(music.len()));
    if let (true, Some(heard)) = (pitched, heard) {
        let key = part.key.as_ref();
        let named = |low: u8, high: u8| format!("{}-{}", name_pitch(low, key), name_pitch(high, key));
        out.insert("pitch_range".into(), json!(named(heard.lowest, heard.highest)));
        if profile.sounds_octaves != 0
            && let Some(written) = summarize(&music)
        {
            out.insert("written_range".into(), json!(named(written.lowest, written.highest)));
            part.remarks.push(format!(
                "This instrument sounds {} octave(s) {} its keys, so the part was written that much {}: \
                 `pitch_range` is what is heard, `written_range` the keys in the clip.",
                profile.sounds_octaves.abs(),
                if profile.sounds_octaves < 0 { "below" } else { "above" },
                if profile.sounds_octaves < 0 { "higher" } else { "lower" },
            ));
        }
    }
    if let Some(key) = &part.key {
        out.insert("key".into(), json!(key.name()));
    }
    out.append(&mut part.summary);
    if !presses.is_empty() {
        out.insert("keyswitches".into(), json!(presses));
    }
    if !part.remarks.is_empty() {
        out.insert("notes".into(), json!(part.remarks));
    }
    if !warnings.is_empty() {
        out.insert("warnings".into(), json!(warnings));
    }
    Ok(Value::Object(out))
}

/// A bar a part is written into.
#[derive(Copy, Clone, Debug, PartialEq)]
struct WrittenBar {
    /// Where it begins, in beats from where the part begins.
    start: f64,
    length: f64,
    signature: TimeSignature,
}

fn said_signature(signature: TimeSignature) -> String {
    format!("{}/{}", signature.numerator, signature.denominator)
}

/// The `count` bars a part is written into from `origin` on: each begins
/// where the one before it ends, in the time signature the song has there,
/// and is a bar of it long, or as long as it gets before the next change.
fn written_bars(meter: &MeterMap, origin: f64, count: usize) -> Vec<WrittenBar> {
    let mut bars = Vec::with_capacity(count);
    let mut at = origin;
    for _ in 0..count {
        let signature = meter.signature_at(at);
        let full = at + signature.beats_per_bar();
        let end = meter.next_change(at).map_or(full, |change| full.min(change));
        bars.push(WrittenBar { start: at - origin, length: end - at, signature });
        at = end;
    }
    bars
}

/// Lays `part`, built in bars of `first`, into `places`: bars that are not
/// all alike. What a part holds in bar N is what it would hold there in a
/// song that was in that bar's time signature throughout, so the part is
/// built once for each signature (`build`) and each bar is taken from the
/// one that is its own. A bar that a change of meter cuts short is cut.
fn lay_in_bars(
    part: &mut Part,
    places: &[WrittenBar],
    first: TimeSignature,
    build: impl Fn(TimeSignature) -> Result<Part, String>,
) -> Result<(), String> {
    // Which bars are in which time signature, said as the song counts
    // them from where the part begins.
    let mut meters: Vec<(TimeSignature, Vec<usize>)> = Vec::new();
    for (index, bar) in places.iter().enumerate() {
        match meters.iter_mut().find(|(signature, _)| *signature == bar.signature) {
            Some((_, bars)) => bars.push(index + 1),
            None => meters.push((bar.signature, vec![index + 1])),
        }
    }
    let said: Vec<String> = meters
        .iter()
        .map(|(signature, bars)| {
            let bars: Vec<String> = bars.iter().map(usize::to_string).collect();
            format!("{} in bar(s) {}", said_signature(*signature), bars.join(", "))
        })
        .collect();
    if part.line {
        return Err(format!(
            "these {} bars cross a change of time signature ({}), and a line is counted through in one: write the \
             bars of each time signature in a call of their own, each starting at its own `start` (get_tempo_map \
             gives the beat each bar starts at)",
            places.len(),
            said.join("; ")
        ));
    }

    // The part as it is in each of the other time signatures.
    let mut others: Vec<(TimeSignature, Part)> = Vec::new();
    for (signature, _) in meters.iter().filter(|(signature, _)| *signature != first) {
        others.push((
            *signature,
            build(*signature).map_err(|error| format!("in the bars of {}: {error}", said_signature(*signature)))?,
        ));
    }
    let of = |signature: TimeSignature| others.iter().find(|(other, _)| *other == signature).map(|(_, part)| part);

    // The bar a note belongs to: the one it starts in, or the next if it
    // comes in early and is held over the bar line (an anticipation).
    let owner = |note: &Note, bar: f64| {
        let within = ((note.start + EPSILON) / bar).floor();
        let line = (within + 1.0) * bar;
        let early = line - note.start <= 1.0 && note.start + note.length / 2.0 > line;
        (if early { within + 1.0 } else { within }).max(0.0) as usize
    };
    let (mut notes, mut how, mut wheel) = (Vec::new(), Vec::new(), Vec::new());
    for (index, place) in places.iter().enumerate() {
        let source = if place.signature == first { &*part } else { of(place.signature).expect("built above") };
        let bar = place.signature.beats_per_bar();
        let from = index as f64 * bar;
        for (at, note) in source.notes.iter().enumerate().filter(|(_, note)| owner(note, bar) == index) {
            let within = note.start - from;
            if within >= place.length - EPSILON {
                continue;
            }
            // A note does not ring on past a bar that was cut short.
            let length =
                if place.length < bar - EPSILON { note.length.min(place.length - within) } else { note.length };
            notes.push(Note { start: (place.start + within).max(0.0), length, ..*note });
            how.push(source.how.get(at).cloned().flatten());
        }
        let points = source.wheel.iter().flatten();
        let inside = |beat: f64| beat >= from - EPSILON && beat < from + place.length.min(bar) - EPSILON;
        wheel.extend(
            points
                .filter(|point| inside(point.beat))
                .map(|point| AutomationPoint { beat: place.start + point.beat - from, ..*point }),
        );
    }
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|a, b| notes[*a].start.total_cmp(&notes[*b].start).then(notes[*a].pitch.cmp(&notes[*b].pitch)));
    part.notes = order.iter().map(|index| notes[*index]).collect();
    if how.iter().any(Option::is_some) {
        part.how = order.iter().map(|index| how[*index].clone()).collect();
    } else {
        part.how.clear();
    }
    if part.wheel.is_some() {
        part.wheel = Some(wheel);
    }

    // What the others have to say, and what was played in their bars.
    let mut in_meters = Map::new();
    for (signature, other) in &others {
        for warning in &other.warnings {
            if !part.warnings.contains(warning) {
                part.warnings.push(warning.clone());
            }
        }
        for remark in &other.remarks {
            if !part.remarks.contains(remark) {
                part.remarks.push(remark.clone());
            }
        }
        if let Some(lanes) = other.summary.get("lanes") {
            in_meters.insert(said_signature(*signature), json!({ "lanes": lanes }));
        }
    }
    part.summary.insert("time_signatures".into(), json!(said));
    if !in_meters.is_empty() {
        part.summary.insert("in_other_time_signatures".into(), Value::Object(in_meters));
    }
    part.remarks.push(format!(
        "The time signature changes under these bars: {}. Each bar holds what it would in a song of its own time \
         signature throughout, so what the reply spells out bar by bar is counted in {}: beats are right in the \
         bars of that signature, and bar numbers in all of them.",
        said.join("; "),
        said_signature(first)
    ));
    Ok(())
}

/// The articulations of an instrument that are chosen by how hard a note
/// is hit, other than its ordinary one.
fn other_hits(profile: &InstrumentProfile) -> Vec<&str> {
    profile
        .articulations
        .iter()
        .filter(|articulation| articulation.velocity.is_some() && !articulation.default)
        .map(|articulation| articulation.name.as_str())
        .collect()
}

/// The pitch wheel moves every note of a track at once. Says so when a
/// stretch that was given bends has other notes of the track sounding
/// together in it: a chord under the line, or another clip of the track.
fn bent_together(session: &Session, track: TrackId, start: f64, length: f64) -> Option<String> {
    let found = session.project().track(track).ok()?;
    let end = start + length;
    // Notes that only touch (a slur, a joined note) are not a chord.
    let sounding: Vec<Note> = found
        .clips
        .iter()
        .filter(|clip| !clip.muted)
        .flat_map(|clip| clip.timeline_notes())
        .filter(|note| found.profile.is_music(note.pitch) && note.end() > start && note.start < end)
        .map(|note| Note { length: (note.length - 0.1).max(0.01), ..note })
        .collect();
    (summarize(&sounding)?.polyphony > 1).then(|| {
        format!(
            "The pitch wheel moves every note of a track at once, and this track has notes sounding together \
             between beats {} and {} (a chord in this clip, or another clip of the track): they bend and waver \
             with the line. Give the lead a track of its own.",
            round(start),
            round(end)
        )
    })
}

/// Notes that start at or past the end of their clip never play.
fn outside_clip(session: &Session, clip: ClipId) -> Option<String> {
    let clip = session.project().clip(clip).ok()?;
    let midi = clip.midi()?;
    let playable = midi.loop_length.map_or(clip.length, |length| length.min(clip.length));
    let outside = midi.notes.iter().filter(|note| note.start >= playable - EPSILON).count();
    (outside > 0).then(|| {
        format!(
            "{outside} note(s) start at or after beat {} of the clip, past its {}, and will not play. Lengthen the \
             clip with update_clip, or write fewer bars.",
            round(playable),
            if midi.loop_length.is_some() { "loop length" } else { "end" }
        )
    })
}

/// A progression read from a request and stretched to the bars asked for,
/// with the names of the chords in one pass of it.
struct Chart {
    progression: Progression,
    key: Option<Scale>,
    /// Chord names bar by bar, for one pass of the progression as written.
    bar_names: Vec<String>,
    remarks: Vec<String>,
}

fn chart(text: &str, key: Option<&str>, setting: &Setting) -> Result<Chart, String> {
    let key = parse_key(key)?;
    let written = Progression::parse(text, setting.signature, key.as_ref()).map_err(theory)?;
    let progression = written.cycled(setting.bars.unwrap_or(written.bars));
    Ok(Chart { bar_names: written.bar_names(), remarks: written.notes.clone(), progression, key })
}

/// A register for chord voicings, checked to be wide enough to voice in.
fn register(low: Option<&PitchInput>, high: Option<&PitchInput>, defaults: (u8, u8)) -> Result<(u8, u8), String> {
    let (low, high) = (pitch_or(low, defaults.0, "low")?, pitch_or(high, defaults.1, "high")?);
    if high < low.saturating_add(12) {
        return Err(format!(
            "the register {}-{} is under an octave; chords need room, so set `high` at least an octave above `low`",
            pitch_name(low),
            pitch_name(high)
        ));
    }
    Ok((low, high))
}

/// The names of the notes in each of the first `bars` bars: `"A1 E2 | F1 C2"`.
fn spell_bars(notes: &[Note], bar_length: f64, bars: usize, key: Option<&Scale>) -> String {
    (0..bars)
        .map(|bar| {
            let (from, to) = (bar as f64 * bar_length, (bar + 1) as f64 * bar_length);
            let mut inside: Vec<&Note> =
                notes.iter().filter(|note| note.start >= from - EPSILON && note.start < to - EPSILON).collect();
            inside.sort_by(|a, b| a.start.total_cmp(&b.start));
            inside.iter().map(|note| name_pitch(note.pitch, key)).collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

#[tool_router(router = music_router, vis = "pub(crate)")]
impl TrillServer {
    /// Write a chord progression into a clip: voiced, led smoothly from chord to chord, and played in a
    /// rhythm. Replies with each chord's name and the notes it was given.
    #[tool(annotations(title = "Write chords"))]
    fn write_chords(&self, Parameters(request): Parameters<WriteChordsRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = write(&mut session, &request.target, &request.feel, PartKind::Pitched, "Chords", |setting| {
            let chart = chart(&request.progression, request.key.as_deref(), setting)?;
            let style = VoicingStyle::parse(request.voicing.as_deref().unwrap_or("close")).map_err(theory)?;
            let (low, high) = register(request.low.as_ref(), request.high.as_ref(), (52, 76))?;
            let bass = match request.bass.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
                None | Some("none") => Bass::None,
                Some("root") => Bass::Root,
                Some("octaves" | "octave") => Bass::Octaves,
                Some(other) => return Err(format!("`bass` '{other}' should be \"none\", \"root\" or \"octaves\"")),
            };
            let spec = VoicingSpec {
                style,
                low,
                high,
                voices: within(request.voices.map(f64::from), 4.0, 1.0..=8.0, "voices")? as usize,
                bass,
                // An octave and a bit under the chords, but never below E1.
                bass_low: low.saturating_sub(16).clamp(28, 36),
                inversion: request.inversion.map(usize::from),
            };
            let strum_direction =
                match request.strum_direction.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
                    None | Some("up") => StrumDirection::Up,
                    Some("down") => StrumDirection::Down,
                    Some("alternate" | "alternating") => StrumDirection::Alternate,
                    Some(other) => {
                        return Err(format!("`strum_direction` '{other}' should be \"up\", \"down\" or \"alternate\""));
                    }
                };
            let comping = Comping {
                rhythm: Rhythm::parse(request.rhythm.as_deref().unwrap_or("sustain"), setting.signature)
                    .map_err(theory)?,
                anticipation: within(request.anticipation, 0.0, 0.0..=2.0, "anticipation")?,
                gate: within(request.gate, 1.0, 0.05..=2.0, "gate")?,
                strum: within(request.strum, 0.0, 0.0..=0.25, "strum")?,
                strum_direction,
                dynamics: dynamics(request.velocity, request.accent, (84, 8))?,
            };
            let voicings = chart.progression.voice(&spec);
            let notes = comp(&chart.progression, &voicings, &comping);

            // Say what each chord of one pass became.
            let bar_length = setting.signature.beats_per_bar();
            let pass = chart.bar_names.len() as f64 * bar_length;
            let mut chords = Vec::new();
            let mut warnings = Vec::new();
            let (mut cramped, mut muddy) = (0, Vec::new());
            for (span, voicing) in chart.progression.spans.iter().zip(&voicings) {
                let (Some(chord), Some(voicing)) = (&span.chord, voicing) else { continue };
                if voicing.upper.iter().any(|pitch| !(low..=high).contains(pitch)) {
                    cramped += 1;
                }
                if let Some((lower, upper)) = muddy_intervals(&voicing.pitches()).first().copied()
                    && muddy.len() < 3
                    && !muddy.iter().any(|(name, _)| *name == chord.name())
                {
                    muddy.push((chord.name(), format!("{}-{}", chord.spell(lower), chord.spell(upper))));
                }
                if span.start < pass - EPSILON && chords.len() < SHOWN_CHORDS {
                    let mut entry = json!({
                        "at": bar_beat(span.start, bar_length),
                        "chord": chord.name(),
                        "voicing": voicing.pitches().iter().map(|pitch| chord.spell(*pitch)).collect::<Vec<_>>().join(" "),
                    });
                    if span.written != chord.name() {
                        entry["written"] = json!(span.written);
                    }
                    chords.push(entry);
                }
            }
            if cramped > 0 {
                warnings.push(format!(
                    "{cramped} chord(s) did not fit between {} and {} in the {} voicing and spill outside; widen the \
                     register or use fewer `voices`.",
                    pitch_name(low),
                    pitch_name(high),
                    style.name()
                ));
            }
            for (name, pair) in muddy {
                warnings.push(format!(
                    "{name} has {pair} close together low down, which sounds muddy; raise `low`, or use the open, \
                     shell or pad voicing."
                ));
            }
            let mut summary = Map::new();
            summary.insert("voicing".into(), json!(style.name()));
            if chords.len() == SHOWN_CHORDS {
                summary
                    .insert("chords_note".into(), json!(format!("Only the first {SHOWN_CHORDS} chords are listed.")));
            }
            summary.insert("chords".into(), json!(chords));
            Ok(Part {
                how: all_played(request.articulation.as_deref(), &notes),
                notes,
                bars: chart.progression.bars,
                summary,
                warnings,
                remarks: chart.remarks,
                key: chart.key,
                ..Part::default()
            })
        });
        finish(reply)
    }

    /// Write a drum part: a groove by name, your own lanes in step notation, or a groove with some lanes
    /// replaced, with an optional fill. Use it on a track whose instrument is a drum kit.
    #[tool(annotations(title = "Write drums"))]
    fn write_drums(&self, Parameters(request): Parameters<WriteDrumsRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = write(&mut session, &request.target, &request.feel, PartKind::Drums, "Drums", |setting| {
            let kit = &setting.profile;
            let groove = request.groove.as_deref().map(Groove::find).transpose().map_err(theory)?;
            let meter = (setting.signature.numerator, setting.signature.denominator);
            let bar_length = setting.signature.beats_per_bar();
            let own = |signature: (u8, u8)| f64::from(signature.0) * 4.0 / f64::from(signature.1);
            // A groove written in another meter keeps its steps: it starts with the bar, is cut off where a
            // shorter bar ends and begins again where a longer one goes on. If the bar is no whole number of
            // its steps, there is no such pattern, and it is fitted into the bar whole.
            let refitted = groove
                .filter(|groove| groove.signature != meter)
                .map(|groove| groove.pattern().refit(own(groove.signature), bar_length, false));
            // A groove is written on the General MIDI map; this kit may keep its drums elsewhere.
            let mut pattern = match (groove, &refitted) {
                (Some(_), Some(Some(fitted))) => fitted.on_kit(kit),
                (Some(groove), _) => groove.pattern().on_kit(kit),
                (None, _) => Default::default(),
            };
            let mut stand_ins: Vec<String> = Vec::new();
            for (drum, steps) in request.lanes.iter().flatten() {
                if let Some(stand_in) = pattern.set_lane_on(kit, drum, steps).map_err(theory)? {
                    stand_ins.push(format!("{stand_in} for {}", trill_core::pitch::name_key(drum)));
                }
            }
            if pattern.lanes.is_empty() {
                return Err(format!(
                    "nothing to play: give a `groove` ({}) and/or `lanes` such as {{\"kick\": \"x...x...x...x...\"}}",
                    GROOVES.iter().map(|groove| groove.name).collect::<Vec<_>>().join(", ")
                ));
            }
            let fill = request.fill.as_deref().map(Fill::find).transpose().map_err(theory)?;
            // A fill written in another meter keeps its steps too, and ends with the bar it is in: it leads
            // into the next downbeat.
            let fill_refitted = fill
                .filter(|fill| fill.signature != meter)
                .map(|fill| fill.pattern().refit(own(fill.signature), bar_length, true));
            let fill_pattern = fill.map(|fill| match &fill_refitted {
                Some(Some(fitted)) => fitted.on_kit(kit),
                _ => fill.pattern().on_kit(kit),
            });
            let fill_from = |fill: &Fill| match &fill_refitted {
                // As many beats before the end of the bar as it is in its own.
                Some(Some(_)) => (1.0 - (1.0 - fill.from) * own(fill.signature) / bar_length).max(0.0),
                _ => fill.from,
            };
            let every = request.fill_every.unwrap_or(0) as usize;
            let crash = kit.drum("crash").map_or(49, |drum| drum.pitch);
            let plan = fill.zip(fill_pattern.as_ref()).map(|(fill, pattern)| FillPlan {
                pattern,
                from: fill_from(fill),
                every,
                crash,
            });
            let bars = setting.bars.unwrap_or(pattern.bars());
            let level = within(request.level, 1.0, 0.1..=1.5, "level")?;
            let arranged = arrange(&pattern, plan.as_ref(), bars, bar_length, level);
            let notes = arranged.notes;

            let mut warnings = pattern.odd_lanes(bar_length);
            let mut remarks = Vec::new();
            let mut summary = Map::new();
            if !stand_ins.is_empty() {
                remarks.push(format!(
                    "This track's kit has no key of its own for some of these drums; the nearest was played: {}. If \
                     the instrument has one, name its key with set_instrument_profile `sounds`.",
                    stand_ins.join(", ")
                ));
            }
            // The hits of some notes, by the names this kit gives its drums.
            let by_drum = |notes: &[Note]| -> BTreeMap<String, usize> {
                let mut counts = BTreeMap::new();
                for note in notes {
                    *counts.entry(kit.drum_name(note.pitch)).or_default() += 1;
                }
                counts
            };
            if let Some(groove) = groove {
                summary.insert("groove".into(), json!(groove.name));
                if groove.signature != meter {
                    let done = match refitted {
                        Some(Some(_)) if own(groove.signature) > bar_length => {
                            "its steps keep their length, and it is cut off where each bar ends"
                        }
                        Some(Some(_)) => "its steps keep their length, and it begins again where its own bar runs out",
                        _ => {
                            "a bar here is no whole number of its steps, so each of its bars was fitted into a bar \
                             whole and its steps are not the notes they were"
                        }
                    };
                    warnings.push(format!(
                        "The '{}' groove is written in {}/{}, but these bars are in {}/{}: {done} (`lanes` shows what \
                         is played). Give lanes of your own for a pattern made for this meter.",
                        groove.name, groove.signature.0, groove.signature.1, meter.0, meter.1
                    ));
                }
                let (slow, fast) = (groove.tempo.0 as f64 * 0.85, groove.tempo.1 as f64 * 1.15);
                if !(slow..=fast).contains(&setting.tempo) {
                    remarks.push(format!(
                        "'{}' is usually played at {}-{} BPM; the project is at {}.",
                        groove.name, groove.tempo.0, groove.tempo.1, setting.tempo
                    ));
                }
            }
            if let Some(fill) = fill {
                if fill.signature != meter {
                    let done = match fill_refitted {
                        Some(Some(_)) => "its steps keep their length, and it ends with the bar",
                        _ => "a bar here is no whole number of its steps, so it was fitted into the bar whole",
                    };
                    warnings.push(format!(
                        "The '{}' fill is written in {}/{}, but these bars are in {}/{}: {done}.",
                        fill.name, fill.signature.0, fill.signature.1, meter.0, meter.1
                    ));
                }
                let in_bars: Vec<usize> = arranged.fills.iter().map(|fill| fill.bar).collect();
                if in_bars.is_empty() {
                    warnings.push(format!(
                        "`fill_every` {every} is more than the {bars} bar(s) written, so no fill was played."
                    ));
                }
                let mut said = json!({ "name": fill.name, "in_bars": in_bars });
                // What each fill took the place of, since that cannot be heard: the first few say it all.
                let shown: Vec<Value> = arranged
                    .fills
                    .iter()
                    .take(SHOWN_FILLS)
                    .map(|bar| {
                        let mut entry = json!({
                            "bar": bar.bar,
                            "from_beat": round(bar.from + 1.0),
                            "replaced": by_drum(&bar.replaced),
                            "played": by_drum(&bar.played),
                        });
                        if bar.crash {
                            entry["then"] = json!(format!("a crash on the downbeat of bar {}", bar.bar + 1));
                        }
                        entry
                    })
                    .collect();
                if !shown.is_empty() {
                    said["bars"] = json!(shown);
                }
                summary.insert("fill".into(), said);
            }
            let lanes: Map<String, Value> =
                pattern.grid().into_iter().map(|(drum, steps)| (drum.to_string(), json!(steps))).collect();
            summary.insert("lanes".into(), Value::Object(lanes));
            summary.insert("hits".into(), json!(by_drum(&notes)));
            Ok(Part {
                notes,
                bars,
                summary,
                warnings,
                remarks,
                usual_swing: groove.and_then(|groove| groove.swing),
                usual_kit: groove.map(|groove| groove.kit),
                ..Part::default()
            })
        });
        finish(reply)
    }

    /// Write an arpeggio over a chord progression: the chords broken into single notes in an order or a
    /// pattern, at a rate, through one or more octaves.
    #[tool(annotations(title = "Write arpeggio"))]
    fn write_arpeggio(&self, Parameters(request): Parameters<WriteArpeggioRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = write(&mut session, &request.target, &request.feel, PartKind::Pitched, "Arpeggio", |setting| {
            let chart = chart(&request.progression, request.key.as_deref(), setting)?;
            let (low, high) = register(request.low.as_ref(), request.high.as_ref(), (57, 81))?;
            let order = match (&request.pattern, request.order.as_deref()) {
                (Some(_), Some(_)) => return Err("give `order` or `pattern`, not both".to_string()),
                (Some(pattern), None) if pattern.is_empty() || pattern.len() > 64 => {
                    return Err("`pattern` needs 1 to 64 chord-tone numbers, e.g. [1, 3, 2, 3]".to_string());
                }
                (Some(pattern), None) => ArpOrder::Pattern(pattern.iter().map(|number| *number as usize).collect()),
                (None, order) => ArpOrder::parse(order.unwrap_or("up")).map_err(theory)?,
            };
            let rate = length_or(request.rate.as_ref(), 0.25, "rate")?;
            if !(1.0 / 16.0..=16.0).contains(&rate) {
                return Err(format!("`rate` {rate} beats is outside 1/64 note (0.0625) to 16 beats"));
            }
            let arpeggio = Arpeggio {
                order,
                octaves: within(request.octaves.map(f64::from), 1.0, 1.0..=4.0, "octaves")? as u8,
                rate,
                gate: within(request.gate, 0.8, 0.05..=4.0, "gate")?,
                seed: request.feel.seed.unwrap_or(1) as u64,
                dynamics: dynamics(request.velocity, request.accent, (84, 12))?,
            };
            let spec = VoicingSpec {
                low,
                high,
                voices: within(request.voices.map(f64::from), 4.0, 1.0..=8.0, "voices")? as usize,
                ..VoicingSpec::default()
            };
            let notes = arpeggio.notes(&chart.progression, &chart.progression.voice(&spec));
            let mut summary = Map::new();
            summary.insert("chords".into(), json!(chart.bar_names));
            summary.insert("rate".into(), json!(round(rate)));
            summary.insert(
                "first_bar".into(),
                json!(spell_bars(&notes, setting.signature.beats_per_bar(), 1, chart.key.as_ref())),
            );
            Ok(Part {
                how: all_played(request.articulation.as_deref(), &notes),
                notes,
                bars: chart.progression.bars,
                summary,
                remarks: chart.remarks,
                key: chart.key,
                ..Part::default()
            })
        });
        finish(reply)
    }

    /// Write a bass line under a chord progression: roots, root and fifth, octaves or a walking line, in a
    /// rhythm, kept in the bass register and following slash-chord basses.
    #[tool(annotations(title = "Write bass line"))]
    fn write_bass(&self, Parameters(request): Parameters<WriteBassRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = write(&mut session, &request.target, &request.feel, PartKind::Pitched, "Bass", |setting| {
            let chart = chart(&request.progression, request.key.as_deref(), setting)?;
            let style = BassStyle::parse(request.style.as_deref().unwrap_or("roots")).map_err(theory)?;
            let (low, high) = register(request.low.as_ref(), request.high.as_ref(), (28, 55))?;
            let rhythm = request
                .rhythm
                .as_deref()
                .map(|rhythm| Rhythm::parse(rhythm, setting.signature))
                .transpose()
                .map_err(theory)?;
            let mut remarks = chart.remarks.clone();
            if style == BassStyle::Walking && rhythm.is_some() {
                remarks.push("A walking line plays one note on every beat, so `rhythm` was not used.".to_string());
            }
            let line = BassLine {
                style,
                rhythm,
                low,
                high,
                gate: within(request.gate, 0.9, 0.05..=1.5, "gate")?,
                anticipation: within(request.anticipation, 0.0, 0.0..=2.0, "anticipation")?,
                dynamics: dynamics(request.velocity, request.accent, (96, 10))?,
            };
            let notes = bass_line(&chart.progression, &line);
            let mut summary = Map::new();
            summary.insert("style".into(), json!(style.name()));
            summary.insert("chords".into(), json!(chart.bar_names));
            let shown = chart.progression.bars.min(4);
            summary.insert(
                "first_bars".into(),
                json!(spell_bars(&notes, setting.signature.beats_per_bar(), shown, chart.key.as_ref())),
            );
            Ok(Part {
                how: all_played(request.articulation.as_deref(), &notes),
                notes,
                bars: chart.progression.bars,
                summary,
                remarks,
                key: chart.key,
                ..Part::default()
            })
        });
        finish(reply)
    }

    /// Write a melody or any single line from a compact phrase: scale degrees or note names with lengths,
    /// rests and ties. Bar lines in the phrase are checked, so a miscounted bar is reported, not written.
    #[tool(annotations(title = "Write melody"))]
    fn write_melody(&self, Parameters(request): Parameters<WriteMelodyRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = write(&mut session, &request.target, &request.feel, PartKind::Pitched, "Melody", |setting| {
            let key = parse_key(request.key.as_deref())?;
            let octave = request.octave.unwrap_or(4);
            if !(-1..=9).contains(&octave) {
                return Err(format!("`octave` {octave} is outside -1..=9"));
            }
            let phrase = Phrase::parse(&request.phrase, key.as_ref(), octave, setting.signature).map_err(theory)?;
            let gate = within(request.gate, 0.95, 0.05..=2.0, "gate")?;
            let bar_length = setting.signature.beats_per_bar();
            let mut remarks = Vec::new();

            // A pickup belongs before a bar line: start it late in the first bar.
            let lead_in = if phrase.pickup > 0.0 { bar_length - phrase.pickup } else { 0.0 };
            if lead_in > 0.0 {
                remarks.push(format!(
                    "The phrase opens with a pickup of {} beat(s); it was placed at the end of bar 1 so that its bar \
                     lines fall on the clip's.",
                    round(phrase.pickup)
                ));
            }
            let own_bars = (((lead_in + phrase.length) / bar_length) - EPSILON).ceil().max(1.0) as usize;
            let bars = setting.bars.unwrap_or(own_bars);
            // The phrase where it falls in the clip, repeated to fill more bars or cut to fit fewer.
            let period = own_bars as f64 * bar_length;
            let total = bars as f64 * bar_length;
            let mut tones: Vec<Tone> = Vec::new();
            let mut repeat = 0;
            while (repeat as f64) * period < total - EPSILON {
                let later = lead_in + repeat as f64 * period;
                tones.extend(
                    phrase
                        .tones
                        .iter()
                        .filter(|tone| tone.start + later < total - EPSILON)
                        .map(|tone| Tone { start: tone.start + later, ..tone.clone() }),
                );
                repeat += 1;
            }
            // What each note becomes on this instrument: a key of its own, or the pitch wheel.
            let playing = Playing {
                gate,
                dynamics: dynamics(request.velocity, request.accent, (90, 8))?,
                signature: setting.signature,
                tempo: setting.tempo,
                profile: &setting.profile,
                articulation: request.articulation.as_deref(),
            };
            let performance = perform(&tones, &playing).map_err(theory)?;
            remarks.extend(performance.remarks.iter().cloned());

            // Named as the phrase spelled them: a #1 is a sharp, not the key's flat second.
            let spelled: Vec<(f64, String)> =
                phrase.tones.iter().map(|tone| (tone.start + lead_in, tone.spelled())).collect();
            if let Some(key) = &key {
                let outside: Vec<String> = phrase
                    .tones
                    .iter()
                    .zip(&spelled)
                    .filter(|(tone, _)| !key.contains(tone.pitch))
                    .map(|(_, (start, name))| format!("{name} at {}", bar_beat(*start, bar_length)))
                    .collect();
                if !outside.is_empty() {
                    let shown = outside.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
                    let more = if outside.len() > 6 { ", ..." } else { "" };
                    remarks.push(format!(
                        "{} note(s) are outside {} (fine if intended): {shown}{more}",
                        outside.len(),
                        key.name()
                    ));
                }
            }
            let by_bar: Vec<String> = (0..own_bars.min(16))
                .map(|bar| {
                    let (from, to) = (bar as f64 * bar_length, (bar + 1) as f64 * bar_length);
                    let names: Vec<&str> = spelled
                        .iter()
                        .filter(|(start, _)| *start >= from - EPSILON && *start < to - EPSILON)
                        .map(|(_, name)| name.as_str())
                        .collect();
                    names.join(" ")
                })
                .collect();
            let mut summary = Map::new();
            summary.insert("pitches".into(), json!(by_bar.join(" | ")));
            if !performance.written.is_empty() {
                // What each ornament was written as: a key of the instrument's, or the wheel.
                let mut ornaments: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for (ornament, how, count) in &performance.written {
                    ornaments.entry(ornament.clone()).or_default().push(format!("{count} as {how}"));
                }
                let said: BTreeMap<String, String> =
                    ornaments.into_iter().map(|(ornament, ways)| (ornament, ways.join("; "))).collect();
                summary.insert("ornaments".into(), json!(said));
                if !performance.wheel.is_empty() {
                    summary.insert(
                        "pitch_bend".into(),
                        json!(format!(
                            "{} point(s) on the clip's pitch_bend curve, for an instrument that bends {} semitone(s) \
                             each way",
                            performance.wheel.len(),
                            round(setting.profile.bend_semitones())
                        )),
                    );
                }
            }
            let (notes, how): (Vec<Note>, Vec<Option<String>>) = performance.notes.into_iter().unzip();
            let line = true;
            Ok(Part {
                notes,
                how,
                wheel: Some(performance.wheel),
                bars,
                summary,
                remarks,
                key,
                line,
                ..Part::default()
            })
        });
        finish(reply)
    }

    /// Give a line its dynamics from its notes, as a controller curve in the clip (expression unless told
    /// otherwise). It finds the phrases (notes separated by rests), raises the level into each phrase's
    /// highest note, swells through long notes and drops to `rest` between phrases, where a player breathes
    /// or lifts the bow. One call makes a wind, brass or string line breathe; instruments modelled on breath
    /// or a bow are silent without such a curve. The curve is a few points, which set_controllers can then
    /// adjust. It replaces the clip's curve for that controller, or only the stretch given by `start` and
    /// `end`. The same arguments always write the same curve.
    #[tool(annotations(title = "Shape dynamics"))]
    fn shape_dynamics(&self, Parameters(request): Parameters<ShapeDynamicsRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        finish(shape_dynamics(&mut session, &request))
    }

    /// Read music back: the key (with how sure the estimate is and the runner-up), the chord sounding in each
    /// bar or half bar, pitch range and density. Works on a clip, a track or the whole song. A chord whose
    /// lowest note is not its root is shown as a slash chord (C/E), so a chord part with no bass under it
    /// reads as the inversions it was voiced in.
    #[tool(annotations(title = "Analyze music", read_only_hint = true))]
    fn analyze_music(&self, Parameters(request): Parameters<AnalyzeMusicRequest>) -> CallToolResult {
        let session = self.session.lock();
        finish(analyze(&session, &request))
    }

    /// The musical vocabulary these tools understand, by topic: chord symbols, the progression, drum and
    /// melody notations, scales, voicings, rhythms, arpeggio orders, bass styles, grooves and fills.
    #[tool(annotations(title = "Music reference", read_only_hint = true))]
    fn music_reference(&self, Parameters(request): Parameters<MusicReferenceRequest>) -> CallToolResult {
        finish(reference(request.topic.as_deref()))
    }
}

/// The body of the shape_dynamics tool.
fn shape_dynamics(session: &mut Session, request: &ShapeDynamicsRequest) -> Reply {
    let id = ClipId(request.clip);
    let kind = match request.controller.as_deref() {
        Some(name) => parse_controller(name)?,
        None => ControllerKind::EXPRESSION,
    };
    if kind == ControllerKind::PitchBend || kind.is_switch() {
        return Err(format!(
            "shape_dynamics writes a level, which {kind} is not; use \"expression\", \"breath\", \"volume\" or another \
             controller an instrument takes as loudness, and set_controllers for pedals and the pitch wheel"
        ));
    }
    let scale = controller_scale(kind);
    let defaults = Phrasing::default();
    let level = |value: Option<f64>, default: f64, what: &str| {
        within(value, (default * scale).round(), 0.0..=scale, what).map(|value| value / scale)
    };
    let wanted = Phrasing {
        rest: level(request.rest, defaults.rest, "rest")?,
        low: level(request.low, defaults.low, "low")?,
        high: level(request.high, defaults.high, "high")?,
        swell: within(request.swell, defaults.swell, 0.0..=1.0, "swell")?,
        phrase_gap: within(request.phrase_gap, defaults.phrase_gap, 0.01..=64.0, "phrase_gap")?,
        long_note: within(request.long_note, defaults.long_note, 0.05..=64.0, "long_note")?,
    };
    if wanted.low > wanted.high {
        return Err(format!(
            "`low` {} is above `high` {}; give the quietest sounding level as `low`",
            round(wanted.low * scale),
            round(wanted.high * scale)
        ));
    }

    let project = session.project();
    let clip = project.clip(id).map_err(|error| explain(project, error))?;
    let Some(midi) = clip.midi() else { return Err(format!("{id} is an audio clip and has no notes to shape")) };
    let in_range = |note: &&Note| {
        request.start.is_none_or(|start| note.start >= start - EPSILON)
            && request.end.is_none_or(|end| note.start < end - EPSILON)
    };
    let notes: Vec<Note> = midi.notes.iter().filter(in_range).copied().collect();
    if notes.is_empty() {
        return Err(match (request.start, request.end) {
            (None, None) => format!("{id} has no notes to shape; write the line first (write_melody, add_notes)"),
            _ => format!("{id} has no notes starting in that stretch; get_notes shows where they are"),
        });
    }
    let shaped = phrasing::shape(&notes, &wanted);

    // The whole clip: the curve is the line's. A stretch of it: the curve
    // is written over what was there for that stretch only.
    let whole = request.start.is_none() && request.end.is_none();
    let points = if whole {
        shaped.points.clone()
    } else {
        let from = request.start.unwrap_or_else(|| shaped.phrases.first().map_or(0.0, |phrase| phrase.start)).max(0.0);
        let to = request.end.unwrap_or_else(|| shaped.phrases.last().map_or(from, |phrase| phrase.end)).max(from);
        let mut piece: Vec<AutomationPoint> = shaped
            .points
            .iter()
            .filter(|point| point.beat >= from - EPSILON && point.beat <= to + EPSILON)
            .copied()
            .collect();
        if piece.first().is_none_or(|first| first.beat > from + EPSILON) {
            piece.insert(0, AutomationPoint { beat: from, value: wanted.rest, shape: Segment::Hold });
        }
        let existing = clip.controller(kind).map(|curve| curve.points.clone()).unwrap_or_default();
        splice(&existing, from, Some(to), &piece)
    };

    // Where a beat of the clip's content is in the song, as bar.beat.
    let bars = project.meter_map();
    let (clip_start, offset) = (clip.start, midi.start_offset);
    let place = |beat: f64| bar_beat_at(&bars, (clip_start + beat - offset).max(0.0));
    let phrases: Vec<Value> = shaped
        .phrases
        .iter()
        .map(|phrase| {
            json!({
                "from": place(phrase.start),
                "to": place(phrase.end),
                "notes": phrase.notes,
                "peak": format!("{} at {}", name_pitch(phrase.peak_pitch, None), place(phrase.peak_beat)),
                "peak_level": (phrase.peak_level * scale).round(),
            })
        })
        .collect();

    let count = points.len();
    edit(session, |project| project.clip_mut(id)?.set_controller(kind, points))?;
    let mut out = json!({
        "clip_id": id,
        "controller": kind.key(),
        "point_count": count,
        "phrases": phrases,
        "levels": {
            "rest": (wanted.rest * scale).round(),
            "low": (wanted.low * scale).round(),
            "high": (wanted.high * scale).round(),
        },
    });
    add_controller_warnings(&mut out, session, id, std::iter::once(kind));
    Ok(out)
}

/// What analysis leaves out of the music: keys that are not notes.
#[derive(Default)]
struct LeftOut {
    /// The stretch of the song that is being read.
    from: f64,
    to: f64,
    keyswitches: usize,
    noises: usize,
    outside_range: usize,
    /// Whether some notes were read at another octave than they are
    /// written at, because that is where they sound.
    moved: bool,
}

impl LeftOut {
    /// The notes of `played` that are music on the instrument `profile`
    /// describes, at the pitch they sound at. The rest is counted.
    fn music(&mut self, profile: &InstrumentProfile, played: Vec<Note>) -> Vec<Note> {
        let mut music = Vec::with_capacity(played.len());
        for note in played.into_iter().filter(|note| note.end() > self.from && note.start < self.to) {
            if profile.is_keyswitch(note.pitch) {
                self.keyswitches += 1;
            } else if profile.sound_name(note.pitch).is_some() {
                self.noises += 1;
            } else if !profile.in_range(note.pitch) {
                self.outside_range += 1;
            } else {
                self.moved |= profile.sounds_octaves != 0;
                music.push(Note { pitch: profile.sounding(note.pitch), ..note });
            }
        }
        music
    }

    /// What was left out, for the reply; `None` if nothing was.
    fn said(&self) -> Option<Value> {
        let mut out = Map::new();
        for (name, count) in
            [("keyswitches", self.keyswitches), ("noises", self.noises), ("outside_range", self.outside_range)]
        {
            if count > 0 {
                out.insert(name.into(), json!(count));
            }
        }
        if self.moved {
            out.insert("read_at".into(), json!("the octave the instrument sounds at, not the keys in the clip"));
        }
        (!out.is_empty()).then_some(Value::Object(out))
    }
}

/// The first `bars` bars of a drum part as lanes in step notation, to the
/// nearest sixteenth, by the kit's own drum names: what write_drums would
/// be given to write them. `None` where a bar is not a whole number of
/// sixteenths.
fn read_lanes(kit: &InstrumentProfile, notes: &[Note], start: f64, bars: usize, bar_length: f64) -> Option<Value> {
    let steps = bar_length * 4.0;
    if bars == 0 || (steps - steps.round()).abs() > EPSILON {
        return None;
    }
    let steps = steps.round() as usize;
    let mut lanes: BTreeMap<String, Vec<char>> = BTreeMap::new();
    for note in notes {
        let step = ((note.start - start) * 4.0).round();
        if step < 0.0 || step as usize >= steps * bars {
            continue;
        }
        let lane = lanes.entry(kit.drum_name(note.pitch)).or_insert_with(|| vec!['.'; steps * bars]);
        lane[step as usize] = match note.velocity {
            0..=50 => 'g',
            51..=114 => 'x',
            _ => 'X',
        };
    }
    let text: Map<String, Value> = lanes
        .into_iter()
        .map(|(drum, lane)| {
            let bars: Vec<String> = lane.chunks(steps).map(|bar| bar.iter().collect()).collect();
            (drum, json!(bars.join("|")))
        })
        .collect();
    (!text.is_empty()).then_some(Value::Object(text))
}

fn analyze(session: &Session, request: &AnalyzeMusicRequest) -> Reply {
    let project = session.project();
    let meter = project.meter_map();
    let bar_length = project.time_signature.beats_per_bar();

    let (from, to) = (request.start.unwrap_or(f64::MIN), request.end.unwrap_or(f64::MAX));
    if from >= to {
        return Err("`start` must be before `end`".to_string());
    }
    // Gather the notes as they sound on the timeline, each track's through
    // what it knows about its instrument.
    let mut left_out = LeftOut { from, to, ..LeftOut::default() };
    let mut drum_kit: Option<InstrumentProfile> = None;
    let (scope, mut notes): (String, Vec<Note>) = match (request.clip, request.track) {
        (Some(_), Some(_)) => return Err("give `clip` or `track`, not both".to_string()),
        (Some(clip), None) => {
            let id = ClipId(clip);
            let found = project.clip(id).map_err(|error| explain(project, error))?;
            if found.midi().is_none() {
                return Err(format!("{id} is an audio clip; only MIDI can be analyzed"));
            }
            let track = project.clip_track(id).map_err(|error| explain(project, error))?;
            let profile = &project.track(track).map_err(|error| explain(project, error))?.profile;
            let notes = if session.plays_drums(track) {
                drum_kit = Some(profile.clone());
                found.timeline_notes()
            } else {
                left_out.music(profile, found.timeline_notes())
            };
            (format!("clip {clip} ({})", found.name), notes)
        }
        (None, Some(track)) => {
            let id = TrackId(track);
            let found = project.track(id).map_err(|error| explain(project, error))?;
            let played: Vec<Note> =
                found.clips.iter().filter(|clip| !clip.muted).flat_map(|clip| clip.timeline_notes()).collect();
            let notes = if session.plays_drums(id) {
                drum_kit = Some(found.profile.clone());
                played
            } else {
                left_out.music(&found.profile, played)
            };
            (format!("track {track} ({})", found.name), notes)
        }
        (None, None) => {
            let pitched: Vec<_> = project
                .tracks
                .iter()
                .filter(|track| !track.mute && !session.plays_drums(track.id))
                .filter(|track| track.clips.iter().any(|clip| !clip.notes().is_empty()))
                .collect();
            let names: Vec<&str> = pitched.iter().map(|track| track.name.as_str()).collect();
            let mut notes = Vec::new();
            for track in &pitched {
                let played = track.clips.iter().filter(|clip| !clip.muted).flat_map(|clip| clip.timeline_notes());
                notes.extend(left_out.music(&track.profile, played.collect()));
            }
            (
                format!("song ({})", if names.is_empty() { "no pitched tracks".to_string() } else { names.join(", ") }),
                notes,
            )
        }
    };
    notes.retain(|note| note.end() > from && note.start < to);
    let Some(summary) = summarize(&notes) else {
        let mut out = json!({ "scope": scope, "note_count": 0, "note": "There are no notes here to analyze." });
        if let Some(said) = left_out.said() {
            out["left_out"] = said;
            out["note"] = json!("There are no notes of music here: only keys that switch or make a noise.");
        }
        return Ok(out);
    };

    // Whole bars around the music, on the song's bar grid.
    let (first_bar, last_bar, start, end) = match meter.is_constant() {
        true => {
            let first = ((request.start.unwrap_or(summary.start) / bar_length) + EPSILON).floor().max(0.0);
            let last = ((request.end.unwrap_or(summary.end) / bar_length) - EPSILON).ceil().max(first + 1.0);
            (first, last, first * bar_length, last * bar_length)
        }
        // Where the time signature changes, the song's bars as its map counts them.
        false => {
            let first = meter.bar_at(request.start.unwrap_or(summary.start).max(0.0) + EPSILON);
            let end = meter.bar_line_at_or_after(request.end.unwrap_or(summary.end) - EPSILON).max(first.end());
            let last = meter.bar_at(end - EPSILON);
            (f64::from(first.index), f64::from(last.index + 1), first.start, end)
        }
    };
    let bars = (last_bar - first_bar) as usize;
    // The first bar looked at is as long as its own time signature makes it.
    let bar_length = meter.bar_at(start + EPSILON).length;
    let mut out = json!({
        "scope": scope,
        "bars": format!("{}-{}", first_bar as usize + 1, last_bar as usize),
        "note_count": summary.note_count,
        "notes_per_bar": round(summary.note_count as f64 / bars as f64),
    });

    if let Some(kit) = &drum_kit {
        // A drum part: what is hit how often, and the first bars as the
        // lanes they would be written as.
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for note in &notes {
            *counts.entry(kit.drum_name(note.pitch)).or_default() += 1;
        }
        out["plays"] = json!("drums");
        out["drums"] = json!(counts);
        // A key the kit has no drum on by any name: what it plays there is anybody's guess.
        let mut unnamed: Vec<u8> =
            notes.iter().map(|note| note.pitch).filter(|pitch| kit.drum_name(*pitch) == pitch_name(*pitch)).collect();
        let hits = unnamed.len();
        unnamed.sort_unstable();
        unnamed.dedup();
        if !unnamed.is_empty() {
            let keys: Vec<String> = unnamed.iter().map(|pitch| pitch_name(*pitch)).collect();
            out["note"] = json!(format!(
                "{hits} hit(s) are on keys this track's kit has no drum name for ({}): it may keep something there \
                 that General MIDI does not, or nothing. get_instrument_profile shows its map and what is said \
                 about it; a probe of those keys says what they play.",
                keys.join(", ")
            ));
        }
        if let Some(lanes) = read_lanes(kit, &notes, start, bars.min(2), bar_length) {
            out["first_bars"] = lanes;
        }
        return Ok(out);
    }
    if let Some(said) = left_out.said() {
        out["left_out"] = said;
    }

    let estimate = estimate_key(&notes).ok_or("the key could not be estimated")?;
    let given = parse_key(request.key.as_deref())?;
    let key = given.unwrap_or(estimate.key);
    let mut key_out = json!({
        "estimate": estimate.key.name(),
        "confidence": (estimate.confidence * 100.0).round() / 100.0,
        "runner_up": estimate.runner_up.name(),
    });
    if estimate.key.relative().is_some_and(|relative| relative.pitch_classes() == estimate.runner_up.pitch_classes())
        && estimate.key.kind != estimate.runner_up.kind
    {
        key_out["note"] = json!(format!(
            "{} and {} are relative keys with the same notes; which one is home depends on where the music rests.",
            estimate.key.name(),
            estimate.runner_up.name()
        ));
    }
    if given.is_some() {
        key_out["given"] = json!(key.name());
    }
    out["key"] = key_out;
    out["pitch_range"] = json!(format!("{}-{}", key.spell(summary.lowest), key.spell(summary.highest)));
    out["polyphony"] = json!(summary.polyphony);

    // Chords bar by bar, with runs of the same bar folded together.
    let halves = match request.resolution.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("bar") => 1,
        Some("half_bar" | "half-bar" | "half") => 2,
        Some(other) => return Err(format!("`resolution` '{other}' should be \"bar\" or \"half_bar\"")),
    };
    let readings = match meter.is_constant() {
        true => read_chords(&notes, bar_length / halves as f64, start, end, Some(&key)),
        // Bar by bar, each as long as it is.
        false => meter
            .bars_between(start, end)
            .flat_map(|bar| read_chords(&notes, bar.length / halves as f64, bar.start, bar.end(), Some(&key)))
            .collect(),
    };
    let per_bar: Vec<String> = readings
        .chunks(halves)
        .map(|bar| {
            let names: Vec<String> = bar
                .iter()
                .map(|reading| match (&reading.chord, reading.pitch_classes.as_slice()) {
                    (Some(chord), _) => chord.name(),
                    (None, []) => "-".to_string(),
                    (None, [only]) => format!("{} (single note)", key.spell_pitch_class(*only)),
                    (None, classes) => {
                        let names: Vec<String> =
                            classes.iter().map(|class| key.spell_pitch_class(*class).to_string()).collect();
                        format!("? ({})", names.join(" "))
                    }
                })
                .collect();
            if names.windows(2).all(|pair| pair[0] == pair[1]) { names[0].clone() } else { names.join(" | ") }
        })
        .collect();
    let mut lines: Vec<String> = Vec::new();
    let mut index = 0;
    while index < per_bar.len() {
        let run = per_bar[index..].iter().take_while(|name| **name == per_bar[index]).count();
        let (first, last) = (first_bar as usize + index + 1, first_bar as usize + index + run);
        let label = if run > 1 { format!("{first}-{last}") } else { first.to_string() };
        lines.push(format!("{label}: {}", per_bar[index]));
        index += run;
    }
    if lines.len() > 96 {
        lines.truncate(96);
        out["chords_note"] = json!("Only the first 96 lines are shown; narrow it down with `start` and `end`.");
    }
    out["chords"] = json!(lines);

    let outside: Vec<&Note> = notes.iter().filter(|note| !key.contains(note.pitch)).collect();
    if !outside.is_empty() {
        let mut shown: Vec<String> = outside
            .iter()
            .take(8)
            .map(|note| format!("{} at {}", key.spell(note.pitch), bar_beat_at(&meter, note.start)))
            .collect();
        if outside.len() > shown.len() {
            shown.push("...".to_string());
        }
        out["outside_key"] = json!({ "key": key.name(), "count": outside.len(), "notes": shown });
    }
    Ok(out)
}

const TOPICS: &[(&str, &str)] = &[
    ("chords", "chord symbols and Roman numerals"),
    ("progressions", "the chart notation: bars, holds, repeats, lengths"),
    ("scales", "keys and scales, with their notes"),
    ("voicings", "how chords are laid out, and the register options"),
    ("rhythms", "named comping and bass rhythms, and step patterns"),
    ("arpeggios", "arpeggio orders and patterns"),
    ("bass", "bass styles"),
    ("drums", "the lane notation and the drum names"),
    ("grooves", "the groove and fill library"),
    ("melody", "the phrase notation"),
    ("ornaments", "bends, slides, vibrato and hammer-ons in a melody"),
    ("instruments", "what a track knows about its instrument: drums, keyswitches, range, octave"),
];

fn reference(topic: Option<&str>) -> Reply {
    let pairs = |items: &[(&str, &str)]| -> Value {
        items.iter().map(|(name, what)| json!({ "name": name, "is": what })).collect()
    };
    let Some(topic) = topic.map(|topic| topic.trim().to_ascii_lowercase()) else {
        return Ok(json!({
            "topics": pairs(TOPICS),
            "quick_start": {
                "write_chords": { "track": 1, "progression": "Am9 | Fmaj7 | C G/B | E7", "voicing": "pad" },
                "write_drums": { "track": 2, "groove": "house", "bars": 8, "fill": "build" },
                "write_bass": { "track": 3, "progression": "Am9 | Fmaj7 | C G/B | E7", "style": "octaves" },
                "write_arpeggio": { "track": 4, "progression": "Am9 | Fmaj7 | C G/B | E7", "order": "up_down", "rate": "1/16" },
                "write_melody": { "track": 5, "key": "A minor", "phrase": ":0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4" },
                "analyze_music": {},
            },
        }));
    };
    Ok(match topic.as_str() {
        "chords" | "chord" => json!({
            "form": "root (A-G with # or b), then a quality, then an optional /bass: Am9, F#m7b5, Bb6/9, G7sus4, C/E",
            "qualities": QUALITIES.iter().map(|quality| if quality.is_empty() { "(major triad)" } else { quality }).collect::<Vec<_>>(),
            "also_written": "maj7 = M7 = Δ; m = min = -; dim = °; m7b5 = ø; aug = +; 7b9 = 7-9; 6/9 = 69; sus = sus4; \
                             alterations and additions combine freely: 7#9b13, maj9#11, m(add9), 7(b9,#11), 7no5",
            "rejected": "C2 and C4 (say sus2/add9 or sus4/add11), lowercase roots, anything that does not parse",
            "no_chord": "N.C. is a silence in a progression",
            "roman_numerals": {
                "need": "a `key`",
                "examples": "ii7 V7 Imaj7 | i bVI bIII bVII | V7/V (secondary dominant) | iv (borrowed) | viiø7 | V65 (inversion)",
                "rules": "case sets major or minor; a plain numeral is the key's own degree (VI in A minor is F); bVI \
                          in minor is the same chord; vii° in minor sits on the raised seventh; 64, 65, 43, 42 are \
                          inversions",
            },
        }),
        "progressions" | "progression" => json!({
            "examples": {
                "Am9 | Fmaj7 | C G/B | E7": "one bar each; C and G/B share a bar",
                "Am F C G": "no bar lines: each chord gets a bar",
                "Dm7 . . G7 | Cmaj7": "dots hold: Dm7 three beats, G7 one",
                "Am | . | F | %": "Am for two bars; % plays the previous bar again",
                "C:3 G:1 | F:1.5 G:2.5": "lengths in beats",
                "ii7 V7 | Imaj7 | N.C.": "Roman numerals (with `key`) and a silent bar",
            },
            "notes": "Chords in a bar share it evenly unless lengths are given. `bars` repeats or cuts the progression. \
                      The same text drives write_chords, write_bass and write_arpeggio.",
        }),
        "scales" | "scale" | "keys" | "key" => json!({
            "form": "a tonic and a scale name: \"A minor\", \"F# dorian\", \"Bb major pentatonic\"; short forms \"C\", \"Am\"",
            "scales": ScaleKind::ALL
                .iter()
                .map(|kind| {
                    let scale = Scale::new(NoteName::of_pitch_class(0), *kind);
                    let notes: Vec<String> = scale.names().iter().map(ToString::to_string).collect();
                    json!({ "name": kind.name(), "on_C": notes.join(" ") })
                })
                .collect::<Vec<_>>(),
            "degrees": "In write_melody, 1 is the tonic and numbers count up the scale; b and # alter a degree by a semitone.",
        }),
        "voicings" | "voicing" => json!({
            "voicings": VoicingStyle::ALL.iter().map(|style| json!({ "name": style.name(), "is": style.description() })).collect::<Vec<_>>(),
            "options": {
                "low, high": "the register of the chord proper (default E3-E5); keep thirds above about C3",
                "voices": "most notes in the chord proper (default 4)",
                "bass": "none | root | octaves: a bass note under the chord; leave it out when a bass track plays",
                "inversion": "fix every chord in one inversion instead of voice leading",
            },
            "advice": "pad for synth pads and strings; close or drop2 for keys; shell when the arrangement is busy; \
                       open for wide, slow chords",
        }),
        "rhythms" | "rhythm" => json!({
            "named": pairs(RHYTHMS),
            "step_patterns": LEGEND,
            "examples": { "x__x__x_": "3+3+2, held", "x.x.|..x.": "two bars of stabs", "X_____.x": "a long chord and a push" },
            "anticipation": "0.5 or 0.25 makes every chord change arrive early and hold over the beat",
        }),
        "arpeggios" | "arpeggio" => json!({
            "orders": pairs(ORDERS),
            "pattern": "chord tones by number from the lowest: [1, 3, 2, 3]; numbers past the top go up an octave; 0 rests",
            "rate": "\"1/16\" (default), \"1/8\", \"1/8t\", \"1/32\", or beats",
            "octaves": "1-4",
        }),
        "bass" => json!({
            "styles": BassStyle::ALL.iter().map(|style| json!({ "name": style.name(), "is": style.description() })).collect::<Vec<_>>(),
            "rhythms": "any rhythm from the \"rhythms\" topic; a step pattern like \"x..x..x.x.x..x..\" gives a custom groove",
            "register": "low (default E1) is the lowest note; roots sit in the octave above it",
        }),
        "drums" | "drum" | "lanes" => json!({
            "notation": LEGEND,
            "drums": drum_names(),
            "which_key": "A drum is asked for by name; the key that plays it is the track's own where its \
                          instrument keeps the drum elsewhere than General MIDI (get_instrument_profile shows a \
                          drum track's whole map). A name the kit has no key for is played by the nearest it has \
                          (half_open_hat by open_hat), and the reply says so.",
            "example": { "kick": "x...x...x...x...", "snare": "....x..g.g..x...", "closed_hat": "x.x.x.x.x.x.x3_x.", "china": "x.......x.......", "ride_bell": "..x...x...x...x." },
            "tips": "Velocities are balanced per drum (hats under the kick and snare). Lanes may have different \
                     lengths; shorter ones repeat. Lanes given with a `groove` replace that groove's lane for the drum. \
                     With a `fill`, the reply lists what the fill took the place of in each bar it was played in.",
        }),
        "grooves" | "groove" | "fills" | "fill" => json!({
            "grooves": GROOVES
                .iter()
                .map(|groove| {
                    json!({
                        "name": groove.name,
                        "is": groove.description,
                        "time_signature": format!("{}/{}", groove.signature.0, groove.signature.1),
                        "tempo": format!("{}-{}", groove.tempo.0, groove.tempo.1),
                        "swing": groove.swing.map(|(percent, _)| percent),
                        "kit": groove.kit,
                        "fills": groove.fills,
                    })
                })
                .collect::<Vec<_>>(),
            "fills": FILLS.iter().map(|fill| json!({ "name": fill.name, "is": fill.description })).collect::<Vec<_>>(),
        }),
        "melody" | "melodies" | "phrase" => json!({
            "examples": {
                ":0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4": "scale degrees in the key",
                ":0.5 E5 F5 E5 D5 C5:2 | :1 A4:2 r G4 | A4:4": "the same line as note names",
                "3:1.5 ~:0.5 b3 #4 1':2": "a tie, a flat third, a sharp fourth, the tonic an octave up",
            },
            "pitches": "a scale degree 1-7 (8 and up continue; b3, #4 alter; ' up an octave, , down) or a note name \
                        with octave (C4, F#5)",
            "lengths": "5:0.5 gives one note a length in beats; a bare :0.5 sets the length of the notes after it (1 \
                        beat to begin with); 0.33 is a triplet eighth",
            "other": "r rests; ~ lengthens the note before it; | marks a bar line and is checked; a short first bar is \
                      a pickup",
            "ornaments": "marks after a note, each begun by !: 4:2!bend=5 5:2!vib 6!hammer 3!slide (topic \"ornaments\")",
        }),
        "ornaments" | "ornament" => json!({
            "form": "after a note and its length, each mark begun by !: 4:2!bend=5!vib",
            "marks": pairs(ORNAMENTS),
            "articulations": "any other word is one of the track's own articulations, by name: 1:0.5!palm_mute \
                              (get_project lists a track's articulations)",
            "examples": {
                "4:2!bend=5": "the fourth degree bent up to the fifth and held",
                "4:2!bend=5!release 3:2!vib": "a bend let back down, then a note with vibrato",
                ":0.5 5 6!hammer 5!pull 3": "a hammer-on and a pull-off: one attack for three notes",
                "1:1 3:1!slide": "the third slid into from the first",
                "E5:2!fall": "a note that drops away at its end",
            },
            "what_they_become": "The instrument's own articulation where it has one (a sampled guitar's hammer, \
                                 glide and slide keyswitches); otherwise a pitch_bend curve in the clip, scaled by \
                                 the track's bend range (2 semitones each way unless the track says otherwise). A \
                                 hammer-on or slide between notes is written without a new attack: the note before \
                                 is held and the wheel steps or glides. The reply says what each was written as.",
            "limits": "A bend wider than the bend range goes as far as the wheel does, and the reply says so. The \
                       pitch wheel moves every note of a track at once: ornaments are for a line on its own track, \
                       not over chords.",
        }),
        "instruments" | "instrument" | "articulations" | "articulation" | "keyswitches" => json!({
            "what": "A track knows what its notes mean to its instrument, and the other tools use it: drums or \
                     pitched, names for keys (a drum map, or noises), articulations with their keyswitches, the \
                     range, the octave it sounds at, its bend range.",
            "read": "get_project shows it per track (plays, articulations, range); get_instrument_profile shows all \
                     of it, and with no arguments every instrument something is known about",
            "write": "set_instrument_profile, once, with everything a probe found; it is kept in the song and \
                      remembered for the plugin in every song",
            "example": {
                "track": 3,
                "range": "E1-G4 (28-67)",
                "offset_semitones": -12,
                "articulations": {
                    "open": { "key": 24, "velocity": [64, 126], "default": true },
                    "muted": { "key": 24, "velocity": [32, 63] },
                    "palm_mute": 26,
                    "slide": { "key": 27, "lasts": "once" },
                    "hammer": { "key": 29, "lasts": "joins" },
                },
                "sounds": { "scrape": 101 },
            },
            "asking": "`articulation` on write_chords, write_bass, write_arpeggio and write_melody, or a mark on one \
                       note of a melody (1:0.5!palm_mute). The keyswitch is pressed just before the note; never \
                       place keyswitch notes by hand.",
            "lasts": "stays: until another keyswitch; once: the next note only; joins: notes that overlap are \
                      joined (hammer-ons, a slide along the string)",
        }),
        _ => {
            return Err(format!(
                "'{topic}' is not a topic; expected one of {}",
                TOPICS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            ));
        }
    })
}
