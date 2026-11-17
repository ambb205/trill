//! Loosely typed tool inputs (pitches, notes, parameter values) and the
//! code that turns them into model values.
//!
//! These are parsed by hand rather than through serde's untagged enums so
//! that a mistake produces a message the caller can act on ("notes[17]:
//! velocity 200 is outside 1..=127") instead of "data did not match any
//! variant".

use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use trill_core::device::{ParamInfo, ParamKind};
use trill_core::pitch::{parse_drum, parse_pitch};
use trill_core::project::{AutomationPoint, Segment};
use trill_core::{ControllerKind, Note};

const DEFAULT_VELOCITY: u8 = 100;

/// One note, as an object or a compact array.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct NoteInput(pub Value);

impl JsonSchema for NoteInput {
    fn schema_name() -> Cow<'static, str> {
        "Note".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A note. Either an object {pitch, start, length, velocity?} or, to save space in long \
                lists, an array [pitch, start, length, velocity?]. `pitch` is a MIDI number (60 = C4 = middle C), a \
                note name (\"C4\", \"F#3\", \"Bb2\") or, for drum kits, a drum name (\"kick\", \"snare\", \"clap\", \
                \"closed_hat\", \"open_hat\", \"low_tom\", \"mid_tom\", \"high_tom\", \"crash\", \"ride\", \"rim\", \
                \"cowbell\", \"shaker\", \"tambourine\", \"claves\"). `start` and `length` are in beats (quarter \
                notes); `start` is measured from the beginning of the clip. `velocity` is 1-127 (default 100).",
            "anyOf": [
                {
                    "type": "object",
                    "properties": {
                        "pitch": { "type": ["integer", "string"] },
                        "start": { "type": "number", "minimum": 0 },
                        "length": { "type": "number", "exclusiveMinimum": 0 },
                        "velocity": { "type": "integer", "minimum": 1, "maximum": 127 }
                    },
                    "required": ["pitch", "start", "length"]
                },
                {
                    "type": "array",
                    "items": { "type": ["number", "string"] },
                    "minItems": 3,
                    "maxItems": 4
                }
            ]
        })
    }
}

/// A pitch given as a number or a name.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct PitchInput(pub Value);

impl JsonSchema for PitchInput {
    fn schema_name() -> Cow<'static, str> {
        "Pitch".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "MIDI note number 0-127 (60 = C4 = middle C), a note name such as \"C4\" or \"F#3\", or a \
                drum name such as \"kick\".",
            "type": ["integer", "string"]
        })
    }
}

/// A point of a controller's curve in a clip, as an object or a
/// `[beat, value]` pair.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct ControllerPointInput(pub Value);

impl JsonSchema for ControllerPointInput {
    fn schema_name() -> Cow<'static, str> {
        "ControllerPoint".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A point of a controller's curve: {beat, value, shape?} or [beat, value, shape?]. `beat`                 is measured from the beginning of the clip, like a note's start. `value` is 0-127, as on any MIDI                 controller (64 is half way; fractions are fine), except for pitch_bend, which runs from -1 (wheel                 fully down) to 1 (fully up) with 0 at rest. `shape` says how the value travels from this point to                 the next: omit it for a straight line (a pedal such as sustain steps instead), \"hold\" to stay                 put and jump at the next point, or a number from -1 to 1 to bend the line (positive starts slowly                 and finishes fast, like a crescendo that grows; negative the reverse).",
            "anyOf": [
                {
                    "type": "object",
                    "properties": {
                        "beat": { "type": "number", "minimum": 0 },
                        "value": { "type": "number" },
                        "shape": { "type": ["string", "number"] }
                    },
                    "required": ["beat", "value"]
                },
                { "type": "array", "items": { "type": ["number", "string"] }, "minItems": 2, "maxItems": 3 }
            ]
        })
    }
}

/// An automation breakpoint, as an object or a `[beat, value]` pair.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct PointInput(pub Value);

impl JsonSchema for PointInput {
    fn schema_name() -> Cow<'static, str> {
        "AutomationPoint".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A breakpoint: {beat, value, shape?} or [beat, value, shape?]. `beat` is a song position \
                in beats; `value` is in the target's own units (dB for volume, -1..1 for pan, the parameter's units \
                otherwise). `shape` says how the value travels from this point to the next: omit it for a straight \
                line, \"hold\" to stay put and jump at the next point, or a number from -1 to 1 to bend the line \
                (positive starts slowly and finishes fast, like a build-up; negative the reverse).",
            "anyOf": [
                {
                    "type": "object",
                    "properties": {
                        "beat": { "type": "number", "minimum": 0 },
                        "value": { "type": "number" },
                        "shape": { "type": ["string", "number"] }
                    },
                    "required": ["beat", "value"]
                },
                { "type": "array", "items": { "type": ["number", "string"] }, "minItems": 2, "maxItems": 3 }
            ]
        })
    }
}

pub fn parse_pitch_value(value: &Value) -> Result<u8, String> {
    parse_pitch_with(value, &parse_drum, "a drum name (like \"kick\")")
}

/// Reads a pitch: a number, a note name, or a name that `named` knows a
/// key for (a track's drums, noises and keyswitches). `expected` says what
/// such a name could have been, for the error that it is none.
pub fn parse_pitch_with(value: &Value, named: &dyn Fn(&str) -> Option<u8>, expected: &str) -> Result<u8, String> {
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(n) if n.fract() == 0.0 && (0.0..=127.0).contains(&n) => Ok(n as u8),
            _ => Err(format!("pitch {number} is not a MIDI note number in 0..=127")),
        },
        Value::String(name) => parse_pitch(name)
            .or_else(|| named(name))
            .ok_or_else(|| format!("'{name}' is not a note name (like \"C4\" or \"F#3\") or {expected}")),
        other => Err(format!("pitch must be a number or a string, got {other}")),
    }
}

fn number(value: &Value, what: &str) -> Result<f64, String> {
    value.as_f64().ok_or_else(|| format!("{what} must be a number, got {value}"))
}

fn velocity(value: Option<&Value>) -> Result<u8, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(DEFAULT_VELOCITY) };
    let v = number(value, "velocity")?;
    if !(1.0..=127.0).contains(&v) {
        return Err(format!("velocity {v} is outside 1..=127"));
    }
    Ok(v.round() as u8)
}

fn parse_note(value: &Value, named: &dyn Fn(&str) -> Option<u8>, expected: &str) -> Result<Note, String> {
    let parse_pitch_value = |value: &Value| parse_pitch_with(value, named, expected);
    let (pitch, start, length, velocity) = match value {
        Value::Object(fields) => {
            let field = |name: &str| fields.get(name).ok_or_else(|| format!("missing '{name}'"));
            (
                parse_pitch_value(field("pitch")?)?,
                number(field("start")?, "start")?,
                number(field("length")?, "length")?,
                velocity(fields.get("velocity"))?,
            )
        }
        Value::Array(items) if (3..=4).contains(&items.len()) => (
            parse_pitch_value(&items[0])?,
            number(&items[1], "start")?,
            number(&items[2], "length")?,
            velocity(items.get(3))?,
        ),
        _ => {
            return Err("expected {pitch, start, length, velocity?} or [pitch, start, length, velocity?]".to_string());
        }
    };
    Ok(Note { pitch, start, length, velocity })
}

pub fn parse_notes(notes: &[NoteInput]) -> Result<Vec<Note>, String> {
    parse_notes_with(notes, &parse_drum, "a drum name (like \"kick\")")
}

/// [`parse_notes`], with the names `named` knows keys for; see
/// [`parse_pitch_with`].
pub fn parse_notes_with(
    notes: &[NoteInput],
    named: &dyn Fn(&str) -> Option<u8>,
    expected: &str,
) -> Result<Vec<Note>, String> {
    notes
        .iter()
        .enumerate()
        .map(|(index, note)| parse_note(&note.0, named, expected).map_err(|e| format!("notes[{index}]: {e}")))
        .collect()
}

/// `unsaid` is the shape of a point that does not give one.
fn parse_shape(value: Option<&Value>, unsaid: Segment) -> Result<Segment, String> {
    match value {
        None | Some(Value::Null) => Ok(unsaid),
        Some(Value::String(name)) => match name.trim().to_ascii_lowercase().as_str() {
            "linear" => Ok(Segment::Linear),
            "hold" | "step" => Ok(Segment::Hold),
            other => Err(format!("shape '{other}' should be \"hold\", \"linear\" or a number from -1 to 1")),
        },
        Some(Value::Number(number)) => match number.as_f64() {
            Some(bend) if (-1.0..=1.0).contains(&bend) => {
                Ok(if bend == 0.0 { Segment::Linear } else { Segment::Curve(bend as f32) })
            }
            _ => Err(format!("shape {number} is outside -1..=1")),
        },
        Some(other) => Err(format!("shape must be \"hold\" or a number, got {other}")),
    }
}

pub fn parse_points(points: &[PointInput]) -> Result<Vec<AutomationPoint>, String> {
    parse_point_values(points.iter().map(|point| &point.0), Segment::Linear)
}

/// What a controller is called in a tool's arguments: a name or a number.
pub fn parse_controller(text: &str) -> Result<ControllerKind, String> {
    ControllerKind::parse(text).ok_or_else(|| {
        format!(
            "'{text}' is not a controller. Use a name (\"expression\", \"mod_wheel\", \"pitch_bend\", \"sustain\", \
             \"breath\", \"pressure\", \"volume\", \"pan\") or a controller number from 0 to 119."
        )
    })
}

/// What tools multiply a controller's position by: controllers run from 0
/// to 127 as on any MIDI gear, the pitch wheel from -1 to 1.
pub fn controller_scale(kind: ControllerKind) -> f64 {
    if kind == ControllerKind::PitchBend { 1.0 } else { 127.0 }
}

/// The points of a curve for `kind`, with their values as a clip keeps
/// them. A pedal's points step unless they say otherwise.
pub fn parse_controller_points(
    kind: ControllerKind,
    points: &[ControllerPointInput],
) -> Result<Vec<AutomationPoint>, String> {
    let unsaid = if kind.is_switch() { Segment::Hold } else { Segment::Linear };
    let mut parsed = parse_point_values(points.iter().map(|point| &point.0), unsaid)?;
    let scale = controller_scale(kind);
    let (low, high) = kind.range();
    for (index, point) in parsed.iter_mut().enumerate() {
        if !point.beat.is_finite() || point.beat < 0.0 {
            return Err(format!(
                "points[{index}]: beat {} must be >= 0 (beats are counted from the clip's start)",
                point.beat
            ));
        }
        let position = point.value / scale;
        if !position.is_finite() || position < low - 1e-9 || position > high + 1e-9 {
            return Err(format!(
                "points[{index}]: {kind} value {} is outside {}..={}",
                point.value,
                low * scale,
                high * scale
            ));
        }
        point.value = position.clamp(low, high);
    }
    Ok(parsed)
}

fn parse_point_values<'a>(
    points: impl Iterator<Item = &'a Value>,
    unsaid: Segment,
) -> Result<Vec<AutomationPoint>, String> {
    points
        .enumerate()
        .map(|(index, point)| {
            let (beat, value, shape) = match point {
                Value::Object(fields) => (fields.get("beat"), fields.get("value"), fields.get("shape")),
                Value::Array(items) if (2..=3).contains(&items.len()) => (items.first(), items.get(1), items.get(2)),
                _ => (None, None, None),
            };
            let located = |error: String| format!("points[{index}]: {error}");
            match (beat.and_then(Value::as_f64), value.and_then(Value::as_f64)) {
                (Some(beat), Some(value)) => {
                    Ok(AutomationPoint { beat, value, shape: parse_shape(shape, unsaid).map_err(located)? })
                }
                _ => Err(located("expected {beat, value, shape?} or [beat, value, shape?]".to_string())),
            }
        })
        .collect()
}

/// Finds a parameter by id, or failing that by display name.
pub fn find_param<'a>(params: &'a [ParamInfo], key: &str) -> Result<&'a ParamInfo, String> {
    find_param_index(params, key).map(|index| &params[index])
}

/// The most parameters an error about a parameter names.
const NAMED_PARAMS: usize = 40;

/// Finds a parameter's place in the list: by its id, by its name, or by
/// words that only one name has ("cab l type" for "Cherubs Amp Cab L Mic
/// Type"). Words that several names have are an error that names them.
pub fn find_param_index(params: &[ParamInfo], key: &str) -> Result<usize, String> {
    let wanted = key.trim();
    let position = |matches: &dyn Fn(&ParamInfo) -> bool| params.iter().position(matches);
    if let Some(index) = position(&|info| info.id == wanted)
        .or_else(|| position(&|info| info.id.eq_ignore_ascii_case(wanted)))
        .or_else(|| position(&|info| info.name.eq_ignore_ascii_case(wanted)))
    {
        return Ok(index);
    }
    let words: Vec<String> = wanted.split_whitespace().map(str::to_lowercase).collect();
    let having: Vec<usize> = (0..params.len())
        .filter(|index| {
            let name = params[*index].name.to_lowercase();
            !words.is_empty() && words.iter().all(|word| name.contains(word.as_str()))
        })
        .collect();
    // Plugins give their parameters numbers for ids, which say nothing:
    // those are named by name, with the id to ask for one by.
    let numbered = params.iter().all(|info| info.id.parse::<u64>().is_ok());
    let label = |info: &ParamInfo| if numbered { format!("{} ({})", info.name, info.id) } else { info.id.clone() };
    let listed = |indices: &mut dyn Iterator<Item = usize>, total: usize| {
        let mut names: Vec<String> = indices.take(NAMED_PARAMS).map(|index| label(&params[index])).collect();
        if total > names.len() {
            names.push("...".to_string());
        }
        names.join(", ")
    };
    match having.as_slice() {
        [one] => Ok(*one),
        [] if numbered && params.len() > NAMED_PARAMS => Err(format!(
            "no parameter '{key}' among this device's {}. get_device with `query` finds parameters by name",
            params.len()
        )),
        [] => Err(format!("no parameter '{key}'. Parameters: {}", listed(&mut (0..params.len()), params.len()))),
        several => Err(format!(
            "{} parameters are called something like '{key}': {}. Give the whole name of one, or its id",
            several.len(),
            listed(&mut several.iter().copied(), several.len())
        )),
    }
}

/// Converts a JSON value to a parameter value in plain units: numbers pass
/// through, booleans become 0/1, and strings name an enum choice.
pub fn parse_param_value(info: &ParamInfo, value: &Value) -> Result<f64, String> {
    let out_of_range =
        |v: f64| format!("{} = {v} is outside {}..={}{}", info.id, info.min, info.max, unit_suffix(info));
    let parsed = match value {
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::Number(_) => number(value, &info.id)?,
        Value::String(text) => {
            if let ParamKind::Enum(choices) = &info.kind {
                return choices
                    .iter()
                    .position(|choice| choice.eq_ignore_ascii_case(text.trim()))
                    .map(|index| index as f64)
                    .ok_or_else(|| format!("{}: '{text}' is not one of {}", info.id, choices.join(", ")));
            }
            match text.trim().to_ascii_lowercase().as_str() {
                "true" | "on" | "yes" => 1.0,
                "false" | "off" | "no" => 0.0,
                other => other.parse::<f64>().map_err(|_| format!("{}: '{text}' is not a number", info.id))?,
            }
        }
        other => return Err(format!("{}: expected a number, boolean or choice name, got {other}", info.id)),
    };
    if parsed < info.min || parsed > info.max {
        return Err(out_of_range(parsed));
    }
    // A parameter that counts steps or choices takes a whole one. A plugin's
    // other parameters run from 0 to 1, so a number such as 0.25 given to
    // one of these is most often meant as a quarter of the way, and would
    // otherwise be taken as the step nearest to it without a word.
    if matches!(info.kind, ParamKind::Int | ParamKind::Enum(_)) && parsed.fract() != 0.0 {
        let numbered = info.id.parse::<u64>().is_ok();
        let called = if numbered { format!("{} ({})", info.name, info.id) } else { info.id.clone() };
        return Err(format!(
            "{called} counts whole steps from {} to {}{}, and {parsed} is not one (nor is it taken as a share of              the way). Give a step, or as a string the text the device shows for the value you want",
            info.min,
            info.max,
            unit_suffix(info)
        ));
    }
    Ok(parsed)
}

fn unit_suffix(info: &ParamInfo) -> String {
    if info.unit.is_empty() { String::new() } else { format!(" {}", info.unit) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notes(value: Value) -> Result<Vec<Note>, String> {
        let inputs: Vec<NoteInput> = serde_json::from_value(value).unwrap();
        parse_notes(&inputs)
    }

    fn info(id: &str, name: &str) -> ParamInfo {
        ParamInfo {
            id: id.into(),
            name: name.into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            unit: String::new(),
            kind: ParamKind::Float,
        }
    }

    #[test]
    fn a_parameter_is_found_by_id_by_name_or_by_words_only_its_name_has() {
        let amp = [
            info("101", "Cherubs Amp Cab L Mic Type"),
            info("102", "Cherubs Amp Cab R Mic Type"),
            info("103", "Input Gain"),
            info("104", "Gain"),
        ];
        assert_eq!(find_param_index(&amp, "103"), Ok(2));
        assert_eq!(find_param_index(&amp, " input gain "), Ok(2));
        assert_eq!(find_param_index(&amp, "gain"), Ok(3), "a whole name wins over the names that contain it");
        assert_eq!(find_param_index(&amp, "cab l type"), Ok(0));
        let error = find_param_index(&amp, "mic type").unwrap_err();
        assert!(error.starts_with("2 parameters are called something like 'mic type': "), "{error}");
        assert!(error.contains("Cherubs Amp Cab L Mic Type (101), Cherubs Amp Cab R Mic Type (102)"), "{error}");
        let error = find_param_index(&amp, "presence").unwrap_err();
        assert!(
            error.contains("no parameter 'presence'. Parameters: ") && error.contains("Input Gain (103)"),
            "{error}"
        );

        // A built-in device's ids are words, and are what it is named by.
        let synth = [info("cutoff", "Filter Cutoff"), info("resonance", "Filter Resonance")];
        assert_eq!(find_param(&synth, "Cutoff").map(|info| info.id.as_str()), Ok("cutoff"));
        assert_eq!(find_param_index(&synth, "filter resonance"), Ok(1));
        assert_eq!(
            find_param_index(&synth, "drive").unwrap_err(),
            "no parameter 'drive'. Parameters: cutoff, resonance"
        );

        // A plugin with hundreds: where to look, not all of their names.
        let many: Vec<ParamInfo> = (0..300).map(|n| info(&n.to_string(), &format!("Knob {n}"))).collect();
        let error = find_param_index(&many, "presence").unwrap_err();
        assert!(error.contains("among this device's 300") && error.contains("get_device with `query`"), "{error}");
        let error = find_param_index(&many, "knob").unwrap_err();
        assert!(
            error.starts_with("300 parameters are called something like 'knob'") && error.contains("..."),
            "{error}"
        );
    }

    #[test]
    fn notes_parse_in_both_forms() {
        let parsed = notes(json!([
            { "pitch": 60, "start": 0, "length": 1 },
            { "pitch": "F#3", "start": 1.5, "length": 0.5, "velocity": 64 },
            ["kick", 2, 0.25],
            [67, 3, 1, 127],
        ]))
        .unwrap();
        assert_eq!(parsed[0], Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100 });
        assert_eq!(parsed[1], Note { pitch: 54, start: 1.5, length: 0.5, velocity: 64 });
        assert_eq!(parsed[2], Note { pitch: 36, start: 2.0, length: 0.25, velocity: 100 });
        assert_eq!(parsed[3].velocity, 127);
    }

    #[test]
    fn note_errors_say_which_note_and_why() {
        let error = notes(json!([[60, 0, 1], { "pitch": 60, "start": 0, "length": 1, "velocity": 200 }])).unwrap_err();
        assert_eq!(error, "notes[1]: velocity 200 is outside 1..=127");
        assert!(notes(json!([["H9", 0, 1]])).unwrap_err().starts_with("notes[0]: 'H9' is not a note name"));
        assert!(notes(json!([{ "pitch": 60, "start": 0 }])).unwrap_err().contains("missing 'length'"));
        assert!(notes(json!([[60, 0]])).unwrap_err().contains("expected"));
        assert!(notes(json!([[128, 0, 1]])).unwrap_err().contains("0..=127"));
    }

    #[test]
    fn automation_points_carry_shapes() {
        let inputs: Vec<PointInput> =
            serde_json::from_value(json!([[0, -60], [4, -6, "hold"], { "beat": 8, "value": 0, "shape": 0.5 }]))
                .unwrap();
        let points = parse_points(&inputs).unwrap();
        assert_eq!(points[0], AutomationPoint::new(0.0, -60.0));
        assert_eq!(points[1].shape, Segment::Hold);
        assert_eq!(points[2].shape, Segment::Curve(0.5));

        let bad = |value: Value| parse_points(&serde_json::from_value::<Vec<PointInput>>(value).unwrap()).unwrap_err();
        assert!(bad(json!([[0, 1, "wobbly"]])).starts_with("points[0]: shape 'wobbly'"));
        assert!(bad(json!([[0, 1], [1, 2, 3]])).starts_with("points[1]: shape 3 is outside"));
        assert!(bad(json!([[0]])).contains("expected"));
    }

    #[test]
    fn controller_points_are_in_midi_values_and_a_pedal_steps() {
        let points = |kind: ControllerKind, value: Value| {
            parse_controller_points(kind, &serde_json::from_value::<Vec<ControllerPointInput>>(value).unwrap())
        };
        let swell =
            points(ControllerKind::EXPRESSION, json!([[0, 0], [2, 63.5, 0.5], { "beat": 4, "value": 127 }])).unwrap();
        assert_eq!(swell[0], AutomationPoint::new(0.0, 0.0));
        assert_eq!((swell[1].value, swell[1].shape), (0.5, Segment::Curve(0.5)));
        assert_eq!(swell[2], AutomationPoint::new(4.0, 1.0));
        // A pedal's points step unless they say otherwise.
        let pedal = points(ControllerKind::SUSTAIN, json!([[0, 127], [3.5, 0], [4, 127, "linear"]])).unwrap();
        assert_eq!((pedal[0].shape, pedal[1].shape, pedal[2].shape), (Segment::Hold, Segment::Hold, Segment::Linear));
        // The wheel is the one thing not counted to 127.
        let bend = points(ControllerKind::PitchBend, json!([[0, -1], [1, 0.5]])).unwrap();
        assert_eq!((bend[0].value, bend[1].value), (-1.0, 0.5));

        let error = points(ControllerKind::EXPRESSION, json!([[0, 64], [1, 200]])).unwrap_err();
        assert_eq!(error, "points[1]: expression value 200 is outside 0..=127");
        assert!(points(ControllerKind::PitchBend, json!([[0, 64]])).unwrap_err().contains("outside -1..=1"));
        assert!(points(ControllerKind::EXPRESSION, json!([[-1, 64]])).unwrap_err().contains("beat -1 must be >= 0"));
        assert!(points(ControllerKind::EXPRESSION, json!([[0]])).unwrap_err().contains("expected"));

        assert_eq!(parse_controller("Mod Wheel"), Ok(ControllerKind::MOD_WHEEL));
        assert_eq!(parse_controller("74"), Ok(ControllerKind::Controller(74)));
        assert!(parse_controller("wobble").unwrap_err().contains("\"expression\""));
        assert_eq!(
            (controller_scale(ControllerKind::BREATH), controller_scale(ControllerKind::PitchBend)),
            (127.0, 1.0)
        );
    }

    #[test]
    fn parameter_values_accept_numbers_booleans_and_choice_names() {
        let wave = ParamInfo {
            id: "osc1_wave".into(),
            name: "Osc 1 Wave".into(),
            min: 0.0,
            max: 3.0,
            default: 2.0,
            unit: String::new(),
            kind: ParamKind::Enum(vec!["sine".into(), "triangle".into(), "saw".into(), "square".into()]),
        };
        assert_eq!(parse_param_value(&wave, &json!("Square")), Ok(3.0));
        assert_eq!(parse_param_value(&wave, &json!(1)), Ok(1.0));
        assert!(parse_param_value(&wave, &json!("noise")).unwrap_err().contains("sine, triangle, saw, square"));

        let cutoff = ParamInfo {
            id: "cutoff".into(),
            name: "Filter Cutoff".into(),
            min: 20.0,
            max: 20_000.0,
            default: 20_000.0,
            unit: "Hz".into(),
            kind: ParamKind::Float,
        };
        assert_eq!(parse_param_value(&cutoff, &json!(440.5)), Ok(440.5));
        assert!(parse_param_value(&cutoff, &json!(5)).unwrap_err().contains("outside 20..=20000 Hz"));
        assert!(parse_param_value(&cutoff, &json!(true)).is_err(), "booleans are range-checked too");

        let params = [wave, cutoff];
        assert_eq!(find_param(&params, "cutoff").unwrap().id, "cutoff");
        assert_eq!(find_param(&params, "filter cutoff").unwrap().id, "cutoff");
        assert!(find_param(&params, "resonance").unwrap_err().contains("osc1_wave, cutoff"));
    }

    #[test]
    fn a_parameter_that_counts_steps_refuses_a_number_between_two_of_them() {
        // A plugin's stepped parameter, as a host is told of it: numbered,
        // from 0 to its number of steps.
        let transpose = ParamInfo {
            id: "214".into(),
            name: "Transpose".into(),
            min: 0.0,
            max: 48.0,
            default: 24.0,
            unit: String::new(),
            kind: ParamKind::Int,
        };
        assert_eq!(parse_param_value(&transpose, &json!(12)), Ok(12.0));
        assert_eq!(parse_param_value(&transpose, &json!(12.0)), Ok(12.0));
        let error = parse_param_value(&transpose, &json!(0.25)).unwrap_err();
        assert!(error.starts_with("Transpose (214) counts whole steps from 0 to 48, and 0.25 is not one"), "{error}");
        assert!(error.contains("the text the device shows"), "{error}");

        // A choice by its place in the list is a whole number too.
        let wave = ParamInfo {
            id: "osc1_wave".into(),
            name: "Osc 1 Wave".into(),
            min: 0.0,
            max: 3.0,
            default: 2.0,
            unit: String::new(),
            kind: ParamKind::Enum(vec!["sine".into(), "triangle".into(), "saw".into(), "square".into()]),
        };
        assert!(parse_param_value(&wave, &json!(1.5)).unwrap_err().starts_with("osc1_wave counts whole steps"));
    }
}
