//! Tools for writing music in musical terms: chords, grooves, bass lines,
//! arpeggios and melodies, and for reading back what was written.
//!
//! Like the tools in `server`, these are thin: the musical knowledge lives
//! in `trill-theory`, and each tool turns a request into notes there and
//! puts them into a clip in one undoable edit. What this module adds is the
//! part a caller that cannot hear depends on: a reply that says in musical
//! terms what was written, and warnings about what would otherwise go
//! unnoticed.

use crate::input::parse_pitch_value;
use crate::server::{Reply, TrillServer, edit, explain, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema, Schema, SchemaGenerator, json_schema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::BTreeMap;
use trill_core::device::{DeviceCategory, DeviceFactory};
use trill_core::pitch::{drum_name, pitch_name};
use trill_core::{ClipId, Note, TimeSignature, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::Session;
use trill_theory::arpeggio::{ArpOrder, Arpeggio, ORDERS};
use trill_theory::bass::{BassLine, BassStyle, bass_line};
use trill_theory::chord::QUALITIES;
use trill_theory::drums::{FillPlan, arrange, drum_names};
use trill_theory::melody::Phrase;
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
notation, e.g. {\"kick\": \"x...x...x...x...\", \"snare\": \"....x.......x...\"}.
- write_bass and write_arpeggio: a bass line or an arpeggio that follows the same progression text.
- write_melody: one line as scale degrees or note names with lengths, e.g. \":0.5 5 6 5 4 3:2 | 1:4\".
- analyze_music: the key, the chord in each bar, the range and density of a clip, a track or the whole song. Use \
it to check your work, and to read what the user wrote before adding to it.
- music_reference: the vocabulary and the notations (chord symbols, scales, voicings, rhythms, grooves, fills). \
Look things up there instead of guessing.

Every writing tool takes `track` (it creates a clip there) or `clip` (it writes into that clip), is one undo \
step, and gives the same result for the same arguments. Give all parts of a section the same progression, key \
and swing so they agree. A song usually goes: set_project, create_track per part, write_drums, write_chords, \
write_bass, write_arpeggio, write_melody, analyze_music, then effects, automation and render.
Use create_clip and add_notes for what these cannot say: single hits, sound effects, a voicing spelled by hand.";

const MAX_BARS: u32 = 512;
const MAX_NOTES: usize = 50_000;
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

/// A pitch as a number or a note name. A local alias of the server's own
/// pitch input with wording for registers.
type PitchInput = crate::input::PitchInput;

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
    /// With `track`: where the new clip starts on the timeline, in beats (default 0; bar N of 4/4 is (N-1)*4).
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
    /// Lanes by drum name, added to the groove or replacing its lane for that drum. One character per step:
    /// x hit, X accent, g ghost note, . rest, _ lengthens the hit before it, a digit after a hit makes a roll
    /// (x3_ is three hits across two steps), | separates bars. Each bar's characters divide the bar evenly:
    /// 16 make sixteenths, 12 triplet eighths, 8 eighths. A lane of only rests removes that drum.
    /// Example: {"kick": "x...x...x...x...", "clap": "....x.......x...", "closed_hat": "x.x.x.x.x.x.x3_x."}.
    pub lanes: Option<BTreeMap<String, String>>,
    /// A fill for the end of the phrase: "snare_roll", "toms", "build", "drop", "trap_roll", "triplet_toms"
    /// or "six_eight_toms". The bar after a fill starts with a crash.
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
    #[serde(flatten)]
    pub feel: Feel,
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
#[derive(Clone, Debug, PartialEq)]
enum Instrument {
    None,
    Drums,
    /// A pitched instrument, with the built-in preset it appears to be on.
    Pitched(Option<String>),
}

fn instrument(session: &Session, track: TrackId) -> Instrument {
    let Ok(track) = session.project().track(track) else { return Instrument::None };
    // Only the built-in catalog is consulted: it is instant, while asking
    // the plugin hosts could start a scan with the session locked.
    let builtins = BuiltinFactory.descriptors();
    for device in &track.devices {
        match builtins.iter().find(|descriptor| descriptor.uri == device.uri) {
            Some(descriptor) if descriptor.category != DeviceCategory::Instrument => continue,
            Some(descriptor) if device.uri.contains("drum") => {
                debug_assert_eq!(descriptor.category, DeviceCategory::Instrument);
                return Instrument::Drums;
            }
            Some(descriptor) => {
                // The model stores parameter values, not the preset's name;
                // the preset whose values are all still in place is the one.
                let preset = descriptor
                    .presets
                    .iter()
                    .filter(|preset| !preset.params.is_empty())
                    .filter(|preset| {
                        preset.params.iter().all(|(id, value)| {
                            device.params.get(id).is_some_and(|current| (current - value).abs() < 1e-6)
                        })
                    })
                    .max_by_key(|preset| preset.params.len())
                    .map(|preset| preset.name.clone());
                return Instrument::Pitched(preset);
            }
            // A plugin: the first one in the chain is the instrument.
            None => return Instrument::Pitched(None),
        }
    }
    Instrument::None
}

/// What kind of part a tool writes, for checking it against the track.
#[derive(Copy, Clone, PartialEq)]
enum PartKind {
    Drums,
    Pitched,
}

/// Problems with putting these notes on this track that cannot be heard
/// from the reply alone.
fn instrument_warnings(session: &Session, track: TrackId, kind: PartKind, notes: &[Note]) -> Vec<String> {
    let mut warnings = Vec::new();
    let count_below = |limit: u8| notes.iter().filter(|note| note.pitch < limit).count();
    let count_above = |limit: u8| notes.iter().filter(|note| note.pitch > limit).count();
    match (instrument(session, track), kind) {
        (Instrument::None, _) => {
            warnings
                .push(format!("Track {} has no instrument, so this will be silent; add one with add_device.", track.0));
        }
        (Instrument::Drums, PartKind::Pitched) => warnings.push(format!(
            "Track {}'s instrument is a drum kit: these pitches will trigger drums, not notes. Write this on a \
             track with a pitched instrument.",
            track.0
        )),
        (Instrument::Pitched(_), PartKind::Drums) => warnings.push(format!(
            "Track {}'s instrument is not a drum kit: these drum notes will play as low pitches. Write drums on a \
             track with builtin:drumkit.",
            track.0
        )),
        (Instrument::Pitched(Some(preset)), PartKind::Pitched) if preset.contains("bass") => {
            let high = count_above(60);
            if high > 0 {
                warnings.push(format!(
                    "{high} note(s) are above C4, where the '{preset}' preset sounds thin; it is voiced for about \
                     E1-G2. Lower the register or use another preset."
                ));
            }
        }
        (Instrument::Pitched(Some(preset)), PartKind::Pitched) => {
            let low = count_below(36);
            if low > 0 {
                warnings.push(format!(
                    "{low} note(s) are below C2, lower than the '{preset}' preset is voiced for; they will sound \
                     muddy or vanish. Raise the register, or use a bass preset for a bass part."
                ));
            }
        }
        _ => {}
    }
    if kind == PartKind::Pitched && count_below(24) > 0 {
        warnings.push(format!("{} note(s) are below C1, under what most speakers reproduce.", count_below(24)));
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
struct Part {
    notes: Vec<Note>,
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
    key: Option<Scale>,
}

/// The project settings a part is built against.
struct Setting {
    signature: TimeSignature,
    tempo: f64,
    /// Bars asked for, if any.
    bars: Option<usize>,
}

/// The shared half of every writing tool: resolve the target, build the
/// part, apply the feel, write it as one undo step and describe the result.
fn write(
    session: &mut Session,
    target: &Target,
    feel: &Feel,
    kind: PartKind,
    default_name: &str,
    build: impl FnOnce(&Setting) -> Result<Part, String>,
) -> Reply {
    let destination = destination(session, target)?;
    let signature = session.project().time_signature;
    let bar_length = signature.beats_per_bar();
    let bars = match target.bars {
        Some(bars) if !(1..=MAX_BARS).contains(&bars) => {
            return Err(format!("`bars` {bars} is outside 1..={MAX_BARS}"));
        }
        bars => bars.map(|bars| bars as usize),
    };
    let mut part = build(&Setting { signature, tempo: session.project().tempo, bars })?;
    if part.notes.len() > MAX_NOTES {
        return Err(format!("that would write {} notes; write fewer bars or a slower rate", part.notes.len()));
    }

    // Feel: swing against the song's grid, then the human touch.
    let swing = match (feel.swing, part.usual_swing) {
        (Some(percent), usual) => {
            let unit = length_or(feel.swing_unit.as_ref(), usual.map_or(0.25, |(_, unit)| unit), "swing_unit")?;
            Some(Swing::new(percent, unit).map_err(theory)?)
        }
        (None, Some((percent, unit))) => Some(Swing::new(percent, unit).map_err(theory)?),
        (None, None) => None,
    };
    if let Some(swing) = swing.filter(|swing| swing.percent > 50.0) {
        swing.apply(&mut part.notes, destination.origin);
        part.summary.insert("swing".into(), json!(swing.percent));
    }
    let timing = within(feel.humanize_timing, 0.0, 0.0..=0.25, "humanize_timing")?;
    let velocity = within(feel.humanize_velocity, 0.0, 0.0..=40.0, "humanize_velocity")?;
    if timing > 0.0 || velocity > 0.0 {
        Humanize { timing, velocity, seed: feel.seed.unwrap_or(1) as u64 }.apply(&mut part.notes);
    }

    let written = part.bars as f64 * bar_length;
    let offset = destination.offset;
    for note in &mut part.notes {
        note.start += offset;
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
    let clip = edit(session, |project| match destination.clip {
        Some(id) => {
            let clip = project.clip_mut(id)?;
            if replace {
                clip.remove_notes(|note| note.start >= offset - EPSILON && note.start < offset + written - EPSILON)?;
            }
            clip.add_notes(notes)?;
            Ok(id)
        }
        None => {
            let id = project.add_midi_clip(track, name, destination.origin, clip_length)?;
            let clip = project.clip_mut(id)?;
            clip.add_notes(notes)?;
            if clip_length > written + EPSILON {
                clip.set_loop_length(Some(written))?;
            }
            Ok(id)
        }
    })?;

    let mut warnings = std::mem::take(&mut part.warnings);
    warnings.extend(instrument_warnings(session, track, kind, &part.notes));
    warnings.extend(outside_clip(session, clip));

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
    out.insert("length".into(), json!(round(written)));
    out.insert("note_count".into(), json!(part.notes.len()));
    if kind == PartKind::Pitched
        && let Some(summary) = summarize(&part.notes)
    {
        let key = part.key.as_ref();
        out.insert(
            "pitch_range".into(),
            json!(format!("{}-{}", name_pitch(summary.lowest, key), name_pitch(summary.highest, key))),
        );
    }
    if let Some(key) = &part.key {
        out.insert("key".into(), json!(key.name()));
    }
    out.append(&mut part.summary);
    if !part.remarks.is_empty() {
        out.insert("notes".into(), json!(part.remarks));
    }
    if !warnings.is_empty() {
        out.insert("warnings".into(), json!(warnings));
    }
    Ok(Value::Object(out))
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
                if span.start < pass - EPSILON && chords.len() < 32 {
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
            summary.insert("chords".into(), json!(chords));
            Ok(Part {
                notes,
                bars: chart.progression.bars,
                summary,
                warnings,
                remarks: chart.remarks,
                usual_swing: None,
                key: chart.key,
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
            let groove = request.groove.as_deref().map(Groove::find).transpose().map_err(theory)?;
            let mut pattern = groove.map(Groove::pattern).unwrap_or_default();
            for (drum, steps) in request.lanes.iter().flatten() {
                pattern.set_lane(drum, steps).map_err(theory)?;
            }
            if pattern.lanes.is_empty() {
                return Err(format!(
                    "nothing to play: give a `groove` ({}) and/or `lanes` such as {{\"kick\": \"x...x...x...x...\"}}",
                    GROOVES.iter().map(|groove| groove.name).collect::<Vec<_>>().join(", ")
                ));
            }
            let fill = request.fill.as_deref().map(Fill::find).transpose().map_err(theory)?;
            let fill_pattern = fill.map(Fill::pattern);
            let every = request.fill_every.unwrap_or(0) as usize;
            let plan =
                fill.zip(fill_pattern.as_ref()).map(|(fill, pattern)| FillPlan { pattern, from: fill.from, every });
            let bars = setting.bars.unwrap_or(pattern.bars());
            let bar_length = setting.signature.beats_per_bar();
            let level = within(request.level, 1.0, 0.1..=1.5, "level")?;
            let notes = arrange(&pattern, plan.as_ref(), bars, bar_length, level);

            let meter = (setting.signature.numerator, setting.signature.denominator);
            let mut warnings = pattern.odd_lanes(bar_length);
            let mut remarks = Vec::new();
            let mut summary = Map::new();
            if let Some(groove) = groove {
                summary.insert("groove".into(), json!(groove.name));
                if groove.signature != meter {
                    warnings.push(format!(
                        "The '{}' groove is written in {}/{}, but the project is in {}/{}; its bars will not line up. \
                         Change the time signature with set_project or pick another groove.",
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
                remarks.push(format!("The '{}' drum kit preset suits this groove.", groove.kit));
            }
            if let Some(fill) = fill {
                if fill.signature != meter {
                    warnings.push(format!(
                        "The '{}' fill is written in {}/{}, but the project is in {}/{}.",
                        fill.name, fill.signature.0, fill.signature.1, meter.0, meter.1
                    ));
                }
                let in_bars: Vec<usize> = match every {
                    0 => vec![bars],
                    every => (1..=bars).filter(|bar| bar % every == 0).collect(),
                };
                if in_bars.is_empty() {
                    warnings.push(format!(
                        "`fill_every` {every} is more than the {bars} bar(s) written, so no fill was played."
                    ));
                }
                summary.insert("fill".into(), json!({ "name": fill.name, "in_bars": in_bars }));
            }
            let lanes: Map<String, Value> =
                pattern.grid().into_iter().map(|(drum, steps)| (drum.to_string(), json!(steps))).collect();
            summary.insert("lanes".into(), Value::Object(lanes));
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for note in &notes {
                *counts.entry(drum_name(note.pitch).unwrap_or("other")).or_default() += 1;
            }
            summary.insert("hits".into(), json!(counts));
            Ok(Part {
                notes,
                bars,
                summary,
                warnings,
                remarks,
                usual_swing: groove.and_then(|groove| groove.swing),
                key: None,
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
                notes,
                bars: chart.progression.bars,
                summary,
                warnings: Vec::new(),
                remarks: chart.remarks,
                usual_swing: None,
                key: chart.key,
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
                notes,
                bars: chart.progression.bars,
                summary,
                warnings: Vec::new(),
                remarks,
                usual_swing: None,
                key: chart.key,
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
            let mut notes =
                phrase.notes(gate, &dynamics(request.velocity, request.accent, (90, 8))?, setting.signature);
            // Dynamics are worked out on the phrase's own bar lines, then the phrase is moved.
            for note in &mut notes {
                note.start += lead_in;
            }
            let own_bars = (((lead_in + phrase.length) / bar_length) - EPSILON).ceil().max(1.0) as usize;
            let bars = setting.bars.unwrap_or(own_bars);
            // Repeat the phrase to fill more bars, or cut it to fit fewer.
            let period = own_bars as f64 * bar_length;
            let total = bars as f64 * bar_length;
            let once = notes.clone();
            let mut repeat = 1;
            while repeat as f64 * period < total - EPSILON {
                notes.extend(once.iter().map(|note| Note { start: note.start + repeat as f64 * period, ..*note }));
                repeat += 1;
            }
            notes.retain(|note| note.start < total - EPSILON);

            if let Some(key) = &key {
                let outside: Vec<String> = once
                    .iter()
                    .filter(|note| !key.contains(note.pitch))
                    .map(|note| format!("{} at {}", key.spell(note.pitch), bar_beat(note.start, bar_length)))
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
            let mut summary = Map::new();
            summary.insert("pitches".into(), json!(spell_bars(&once, bar_length, own_bars.min(16), key.as_ref())));
            Ok(Part { notes, bars, summary, warnings: Vec::new(), remarks, usual_swing: None, key })
        });
        finish(reply)
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

fn analyze(session: &Session, request: &AnalyzeMusicRequest) -> Reply {
    let project = session.project();
    let signature = project.time_signature;
    let bar_length = signature.beats_per_bar();

    // Gather the notes as they sound on the timeline.
    let (scope, mut notes, drums): (String, Vec<Note>, bool) = match (request.clip, request.track) {
        (Some(_), Some(_)) => return Err("give `clip` or `track`, not both".to_string()),
        (Some(clip), None) => {
            let id = ClipId(clip);
            let found = project.clip(id).map_err(|error| explain(project, error))?;
            if found.midi().is_none() {
                return Err(format!("{id} is an audio clip; only MIDI can be analyzed"));
            }
            let track = project.clip_track(id).map_err(|error| explain(project, error))?;
            (
                format!("clip {clip} ({})", found.name),
                found.timeline_notes(),
                instrument(session, track) == Instrument::Drums,
            )
        }
        (None, Some(track)) => {
            let id = TrackId(track);
            let found = project.track(id).map_err(|error| explain(project, error))?;
            let notes = found.clips.iter().filter(|clip| !clip.muted).flat_map(|clip| clip.timeline_notes()).collect();
            (format!("track {track} ({})", found.name), notes, instrument(session, id) == Instrument::Drums)
        }
        (None, None) => {
            let pitched: Vec<_> = project
                .tracks
                .iter()
                .filter(|track| !track.mute && instrument(session, track.id) != Instrument::Drums)
                .filter(|track| track.clips.iter().any(|clip| !clip.notes().is_empty()))
                .collect();
            let names: Vec<&str> = pitched.iter().map(|track| track.name.as_str()).collect();
            let notes = pitched
                .iter()
                .flat_map(|track| track.clips.iter().filter(|clip| !clip.muted))
                .flat_map(|clip| clip.timeline_notes())
                .collect();
            (
                format!("song ({})", if names.is_empty() { "no pitched tracks".to_string() } else { names.join(", ") }),
                notes,
                false,
            )
        }
    };
    let (from, to) = (request.start.unwrap_or(f64::MIN), request.end.unwrap_or(f64::MAX));
    if from >= to {
        return Err("`start` must be before `end`".to_string());
    }
    notes.retain(|note| note.end() > from && note.start < to);
    let Some(summary) = summarize(&notes) else {
        return Ok(json!({ "scope": scope, "note_count": 0, "note": "There are no notes here to analyze." }));
    };

    // Whole bars around the music, on the song's bar grid.
    let first_bar = ((request.start.unwrap_or(summary.start) / bar_length) + EPSILON).floor().max(0.0);
    let last_bar = ((request.end.unwrap_or(summary.end) / bar_length) - EPSILON).ceil().max(first_bar + 1.0);
    let (start, end) = (first_bar * bar_length, last_bar * bar_length);
    let bars = (last_bar - first_bar) as usize;
    let mut out = json!({
        "scope": scope,
        "bars": format!("{}-{}", first_bar as usize + 1, last_bar as usize),
        "note_count": summary.note_count,
        "notes_per_bar": round(summary.note_count as f64 / bars as f64),
    });

    if drums {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for note in &notes {
            *counts.entry(drum_name(note.pitch).map_or_else(|| pitch_name(note.pitch), str::to_string)).or_default() +=
                1;
        }
        out["drums"] = json!(counts);
        return Ok(out);
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
    let readings = read_chords(&notes, bar_length / halves as f64, start, end, Some(&key));
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
            .map(|note| format!("{} at {}", key.spell(note.pitch), bar_beat(note.start, bar_length)))
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
            "example": { "kick": "x...x...x...x...", "snare": "....x..g.g..x...", "closed_hat": "x.x.x.x.x.x.x3_x." },
            "tips": "Velocities are balanced per drum (hats under the kick and snare). Lanes may have different \
                     lengths; shorter ones repeat. Lanes given with a `groove` replace that groove's lane for the drum.",
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
        }),
        _ => {
            return Err(format!(
                "'{topic}' is not a topic; expected one of {}",
                TOPICS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            ));
        }
    })
}
