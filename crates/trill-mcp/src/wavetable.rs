//! The wavetable synth as a model reaches it beyond its parameters: the
//! tables an oscillator reads (the ones that ship, and ones made from a
//! recipe or an audio file) and the names of its macros.
//!
//! A model cannot hear a table. So a table is shown as what it is, frame
//! by frame: how bright, how hollow, how loud against its peak, and the
//! recipe it was made from, which is something to copy and change.

use crate::server::{TrillServer, explain, finish};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use trill_core::{Device, DeviceId};
use trill_dsp::wavetable::recipe::{FileSpec, Reading, Recipe, Shape};
use trill_dsp::wavetable::{self, TableSource, factory, patches};
use trill_engine::Session;
use trill_engine::session::{TableReport, factory_tables};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Shaping a sound: the wavetable synth
- builtin:wavetable is the built-in instrument for electronic sounds: basses, leads, pads, plucks and keys. Its \
presets say in a sentence what each sounds like and what it is for; search_presets finds them (\"reese bass\", \
\"glass pad\", \"electric piano\") and create_track takes one as it takes any preset. It loads at once, needs no \
file and renders to the same samples every time.
- It is the one synth here whose every part you can read and set, so reach for it when a sound is to be shaped \
and not only chosen. describe_device \"builtin:wavetable\" explains the parts; get_device on one shows its tables, \
what its matrix sends where (in words) and its macros. A sound is: a table for each oscillator and a position in \
it, what moves the position and the filters (the matrix: `modN_source` x `modN_amount` -> `modN_dest`), \
envelopes and LFOs. Start from the preset nearest to what you want and change that.
- Macros are the handles a preset offers under names (\"Cutoff\", \"Wobble\"): get_device says what each moves. Set \
`macro1`..`macro4` (0 to 1) with update_device, or draw one with set_automation for a sound that changes over a \
section: that is how a build opens a filter or a bass starts to move.
- A table can be made. describe_wavetable lists the tables that ship and shows any of them frame by frame, with \
the recipe it is made from; set_wavetable gives an oscillator one of them, or a table from a recipe of your own \
(a few key frames, each a named shape with its numbers or a list of harmonics; between two key frames of the same \
shape the numbers move), or one read from an audio file (a wavetable file of single cycles, or a recording of a \
held note). describe_wavetable with `recipe` shows what a recipe would be, frame by frame, changing nothing.
- You cannot hear it. After shaping a sound, probe its track (how loud, how bright, how long each note rings) and \
measure the song. Its presets are matched in level by measurement, and nobody has tuned them by ear yet: for a \
sound a maker's preset of an installed instrument covers, that preset may well be the better one. For plain analog \
sounds with fewer parts to set there is builtin:polysynth.
- It has no effects inside. Its presets other than the basses are sounds that bring a chorus, a delay or a reverb \
as devices behind it: the reply that gives one says which were made, and they are yours to change or delete. A \
sound shaped from `init` brings none: add builtin:chorus, builtin:delay or builtin:reverb after it, or a send to a \
return that has them.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DescribeWavetableRequest {
    /// The name of a table that ships ("Acid", "Formant Sweep"): that table frame by frame, and its recipe.
    /// Leave everything out for the list of the tables that ship and how a recipe is written.
    pub table: Option<String>,
    /// Device id of a builtin:wavetable: the table one of its oscillators plays now, its own included.
    pub device: Option<u64>,
    /// Which oscillator of `device`: 1 (the default) or 2.
    pub oscillator: Option<usize>,
    /// A recipe, to see what table it would make without giving it to any device. Written as in set_wavetable.
    pub recipe: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetWavetableRequest {
    /// Id of a builtin:wavetable device.
    pub device: u64,
    /// Which oscillator gets the table: 1 (the default) or 2.
    pub oscillator: Option<usize>,
    /// The name of a table that ships (describe_wavetable lists them): the oscillator plays that one. Give
    /// this, `recipe` or `file`.
    pub table: Option<String>,
    /// A table of the oscillator's own, written down: `{"name": "Thin Reed", "size": 32, "frames": [...]}`.
    /// `frames` are key frames spread evenly over the table's `size` frames (1-256; 32 if left out and there is
    /// more than one key frame). A key frame is a shape with its numbers: `{"shape": "pulse", "width": 0.3}`.
    /// Shapes: sine, triangle, saw, square; pulse (`width` 0.01-0.99); spectrum (every harmonic alike, to be
    /// shaped by the numbers any frame takes); fm (a sine bent by a sine `ratio` times as fast, by `index`
    /// radians, 0-24); fold (a sine folded back on itself, `amount` 0-1); sync (a sine restarted `ratio` times a
    /// cycle, 1-32); formant (a burst at harmonic `at`, 1-256, lasting `width` of the cycle, 0.02-1); noise
    /// (harmonics drawn by lot from `seed`); or `{"harmonics": [1, 0.5, 0.33]}`, the level of each harmonic from
    /// the first (up to 512), with optional `phases` in cycles. Any frame also takes `tilt` (dB per octave of
    /// harmonic: -6 on spectrum is a sawtooth's levels), `cutoff` (the harmonic above which levels fall 24 dB an
    /// octave) with `peak` (dB of resonance there), `odd` and `even` (what those harmonics are multiplied by,
    /// 0-2) and `comb` (0-1, a phaser's notches). Between two key frames of the same shape the numbers move, so
    /// a pulse of width 0.5 and one of 0.05 make a table that sweeps the width; between different shapes one
    /// wave goes over into the other. `"normalize": "table"` keeps the frames' levels against one another;
    /// left out, every frame reaches full level.
    pub recipe: Option<BTreeMap<String, Value>>,
    /// An audio file to read a table of the oscillator's own from: a wavetable file (single cycles laid end to
    /// end, a frame each) or a recording of one held note (its harmonics are read along its length). Which it
    /// is is told from the file unless `cycle` or `root` says.
    pub file: Option<String>,
    /// With `file`: the samples of one cycle, for a file of single cycles (wavetable files have 2048).
    pub cycle: Option<usize>,
    /// With `file`: the note a recording plays, as a MIDI number (60 = middle C; fractions allowed).
    pub root: Option<f64>,
    /// With `file`: how many frames to read from a recording (1-256, 64 if left out).
    pub frames: Option<usize>,
    /// What to call the macros, the first of the list being macro 1: `["Brightness", "Motion"]`. null leaves a
    /// name as it is and "" takes one away. What a macro does is what the matrix sends it to (`modN_source`
    /// "macro1"...). May be given alone, without a table.
    pub macros: Option<Vec<Option<String>>>,
}

/// How many frames of a table a reply shows, evenly through it.
const SHOWN_FRAMES: usize = 9;

/// A table in a reply: what it is, and what it is like at a few places
/// along it.
fn table_json(report: &TableReport) -> Value {
    let mut out = json!({ "name": report.name, "frames": report.frames.len() });
    if report.own {
        out["own"] = json!(true);
    }
    if let Some(about) = report.about {
        out["about"] = json!(about);
    }
    if let Some(file) = &report.file {
        out["file"] = json!(file);
    }
    match &report.reading {
        Some(Reading::Cycles(cycle)) => {
            out["read_as"] = json!(format!("single cycles of {cycle} samples, a frame each"))
        }
        Some(Reading::Recording(hz)) => {
            out["read_as"] = json!(format!("a recording of a note at {hz:.1} Hz: its harmonics along its length"))
        }
        None => {}
    }
    let count = report.frames.len();
    let shown = count.min(SHOWN_FRAMES);
    let places = (0..shown).map(|step| if shown == 1 { 0 } else { step * (count - 1) / (shown - 1) });
    out["at"] = places
        .map(|index| {
            let frame = report.frames[index];
            let position = if count == 1 { 0.0 } else { index as f64 / (count - 1) as f64 };
            json!({
                "position": (position * 100.0).round() / 100.0,
                "bright": (f64::from(frame.centre) * 10.0).round() / 10.0,
                "top": frame.highest,
                "odd": (f64::from(frame.odd) * 100.0).round() / 100.0,
                "level_db": (f64::from(frame.rms_db) * 10.0).round() / 10.0,
            })
        })
        .collect();
    out
}

/// What the numbers a table is shown with mean.
const READING_A_TABLE: &str = "`at` is the table at a few positions: `bright` is where the weight of the spectrum \
    lies, as a harmonic number (1 = a sine, 6 = a sawtooth's first fifty harmonics, 20 and up = thin and harsh); \
    `top` the highest harmonic within 40 dB of the strongest; `odd` the share of the power in odd harmonics (1 = \
    hollow like a square or a clarinet, 0.75 = a sawtooth); `level_db` how loud the frame is against its own peak \
    (-3 = a sine, lower = spikier, so quieter at the same level setting).";

/// The recipe a model was given, read; or what is wrong with it, with what
/// would be right.
fn recipe_of(written: &BTreeMap<String, Value>) -> Result<Recipe, String> {
    let shapes = Shape::NAMES.join(", ");
    let value = Value::Object(written.clone().into_iter().collect());
    let recipe: Recipe = serde_json::from_value(value).map_err(|error| {
        format!(
            "the recipe cannot be read: {error}. A recipe is {{\"name\": ..., \"size\": 32, \"frames\": [key \
             frames]}}; a key frame is {{\"shape\": one of {shapes}}} with that shape's numbers, or \
             {{\"harmonics\": [levels]}}; describe_wavetable `table` shows the recipe of any table that ships"
        )
    })?;
    recipe.check().map_err(|problem| format!("the recipe is not one a table can be made from: {problem}"))?;
    Ok(recipe)
}

/// The slots of a wavetable synth's matrix that do something, in words:
/// `env2 -> filter1_cutoff 0.32`.
fn matrix_of(device: &Device) -> Vec<(String, String, f64)> {
    let value = |id: &str| device.params.get(id).copied().unwrap_or(0.0);
    let choice = |choices: &[&str], index: f64| choices.get(index as usize).copied().unwrap_or("none").to_string();
    (1..=wavetable::params::SLOTS)
        .filter_map(|slot| {
            let from = choice(wavetable::params::SOURCES, value(&format!("mod{slot}_source")));
            let to = choice(wavetable::params::DESTINATIONS, value(&format!("mod{slot}_dest")));
            let amount = value(&format!("mod{slot}_amount"));
            (from != "none" && to != "none").then_some((from, to, amount))
        })
        .collect()
}

/// Adds to a `get_device` reply what a wavetable synth has beside its
/// parameters: the table each oscillator plays, its matrix in words, its
/// macros by name with what each moves, and what its preset says of itself.
pub(crate) fn device_into(out: &mut Value, session: &Session, device: &Device) {
    if device.uri != wavetable::URI {
        return;
    }
    let mut about = json!({});
    for oscillator in 1..=2 {
        about[format!("osc{oscillator}_table")] = match session.wavetable_table(device.id, oscillator) {
            Ok(report) => {
                let mut table = json!({ "name": report.name, "frames": report.frames.len() });
                if report.own {
                    table["own"] = json!(true);
                }
                if let Some(file) = report.file {
                    table["file"] = json!(file);
                }
                table
            }
            Err(error) => json!({ "problem": error.to_string() }),
        };
    }
    let matrix = matrix_of(device);
    let amount = |amount: f64| (amount * 1000.0).round() / 1000.0;
    about["matrix"] = matrix
        .iter()
        .filter(|(from, ..)| !from.starts_with("macro"))
        .map(|(from, to, by)| json!(format!("{from} -> {to} {}", amount(*by))))
        .collect();
    if let Ok(state) = session.wavetable_state(device.id) {
        about["macros"] = (0..4)
            .map(|which| {
                let param = format!("macro{}", which + 1);
                let moves: Vec<String> = matrix
                    .iter()
                    .filter(|(from, ..)| *from == param)
                    .map(|(_, to, by)| format!("{to} {:+}", amount(*by)))
                    .collect();
                let mut entry = json!({ "param": param, "name": state.macro_name(which) });
                entry["value"] = json!(device.params.get(&param).copied().unwrap_or(0.0));
                entry["moves"] = if moves.is_empty() { json!("nothing yet") } else { json!(moves) };
                entry
            })
            .collect();
    }
    // What the preset it was last given says of itself, in full: a
    // listing cuts it short.
    if let Some(patch) = device.preset.as_ref().and_then(|preset| patches::named(&preset.name)) {
        about["preset_says"] = json!(patch.description);
    }
    about["note"] = json!(
        "set_wavetable gives an oscillator another table or names the macros; describe_wavetable shows a table \
         frame by frame. A slot of the matrix is three parameters: modN_source, modN_dest, modN_amount."
    );
    out["wavetable"] = about;
}

#[tool_router(router = wavetable_router, vis = "pub(crate)")]
impl TrillServer {
    /// The wavetables of builtin:wavetable, as numbers and recipes, since they cannot be heard. With nothing:
    /// the tables that ship, each with a sentence, and how a recipe is written. With `table`: that table at
    /// nine positions along it (how bright, how hollow, how loud) and the recipe it is made from, to copy and
    /// change. With `device` (and `oscillator`): the table that oscillator plays now. With `recipe`: the table
    /// that recipe would make, so that a table can be shaped before it is given to anything. Changes nothing.
    #[tool(annotations(title = "Describe wavetable", read_only_hint = true))]
    fn describe_wavetable(&self, Parameters(request): Parameters<DescribeWavetableRequest>) -> CallToolResult {
        let session = self.session.lock();
        let reply = (|| {
            if let Some(recipe) = &request.recipe {
                let recipe = recipe_of(recipe)?;
                let report = session.report_of(&TableSource::Recipe(recipe))?;
                let mut out = table_json(&report);
                out["reading"] = json!(READING_A_TABLE);
                return Ok(out);
            }
            if let Some(device) = request.device {
                let id = DeviceId(device);
                let oscillator = request.oscillator.unwrap_or(1);
                let report =
                    session.wavetable_table(id, oscillator).map_err(|error| explain(session.project(), error))?;
                let mut out = table_json(&report);
                out["oscillator"] = json!(oscillator);
                let state = session.wavetable_state(id).map_err(|error| explain(session.project(), error))?;
                match (report.own, state.table(oscillator - 1), factory::named(&report.name)) {
                    (true, Some(TableSource::Recipe(recipe)), _) => out["recipe"] = json!(recipe),
                    (false, _, Some((_, table))) => out["recipe"] = json!(table.recipe()),
                    _ => {}
                }
                out["reading"] = json!(READING_A_TABLE);
                return Ok(out);
            }
            if let Some(name) = &request.table {
                let Some((index, table)) = factory::named(name) else {
                    let names: Vec<&str> = factory::TABLES.iter().map(|table| table.name).collect();
                    return Err(format!("no table ships as '{name}'; the tables are: {}", names.join(", ")));
                };
                let report = factory_tables().into_iter().nth(index - 1).ok_or("the table could not be made")?;
                let mut out = table_json(&report);
                out["recipe"] = json!(table.recipe());
                out["reading"] = json!(READING_A_TABLE);
                return Ok(out);
            }
            let tables: Vec<Value> = factory_tables()
                .iter()
                .map(|table| json!({ "name": table.name, "frames": table.frames.len(), "about": table.about }))
                .collect();
            Ok(json!({
                "tables": tables,
                "choosing": "An oscillator plays one by `osc1_table` / `osc2_table` (its name), at `osc1_position` \
                             (0 = its first frame, 1 = its last). `table` here shows one frame by frame, with its \
                             recipe.",
                "making": "set_wavetable `recipe` makes a table of an oscillator's own from key frames, each a \
                           shape with its numbers or a list of harmonics; its description says how. `recipe` here \
                           shows what a recipe would make, changing nothing. set_wavetable `file` reads one from \
                           an audio file.",
                "example": {
                    "name": "Thin Reed",
                    "size": 32,
                    "frames": [{ "shape": "pulse", "width": 0.5 }, { "shape": "pulse", "width": 0.04, "tilt": -3 }]
                },
            }))
        })();
        finish(reply)
    }

    /// Give an oscillator of a builtin:wavetable its table: one that ships, by name (`table`); one made from a
    /// recipe of key frames you write (`recipe`); or one read from an audio file (`file`). And name its macros
    /// (`macros`). The reply shows the table frame by frame, so that a recipe can be judged without hearing it.
    /// One undo step. A table of the oscillator's own is kept with the device, and a file it was read from is
    /// one of the song's files: missing_files lists it if it goes, and relink_files finds it again. Giving a
    /// device a table of its own makes it again: notes that sound are cut.
    #[tool(annotations(title = "Set wavetable"))]
    fn set_wavetable(&self, Parameters(request): Parameters<SetWavetableRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let id = DeviceId(request.device);
            let oscillator = request.oscillator.unwrap_or(1);
            let given = [request.table.is_some(), request.recipe.is_some(), request.file.is_some()];
            match given.iter().filter(|given| **given).count() {
                0 if request.macros.is_none() => {
                    return Err("set_wavetable needs `table` (a table that ships, by name), `recipe` (key frames \
                                to make one from) or `file` (an audio file to read one from), or `macros` to name \
                                the macros; describe_wavetable lists the tables"
                        .to_string());
                }
                0 | 1 => {}
                _ => return Err("give one of `table`, `recipe` and `file`: an oscillator plays one table".to_string()),
            }
            if request.file.is_none() && (request.cycle.is_some() || request.root.is_some() || request.frames.is_some())
            {
                return Err("`cycle`, `root` and `frames` say how a `file` is read; a recipe says its own \
                            number of frames as `size`"
                    .to_string());
            }
            // Read before anything is touched, so that what is wrong
            // changes nothing.
            let source = match (&request.recipe, &request.file) {
                (Some(recipe), _) => Some(TableSource::Recipe(recipe_of(recipe)?)),
                (None, Some(file)) => Some(TableSource::File(FileSpec {
                    file: file.clone(),
                    name: String::new(),
                    cycle: request.cycle,
                    root: request.root,
                    frames: request.frames,
                })),
                (None, None) => None,
            };
            session.begin_gesture();
            let result: Result<Value, String> = (|| {
                let mut out = json!({ "device_id": id });
                if let Some(source) = source {
                    let report = session
                        .set_wavetable(id, oscillator, source)
                        .map_err(|error| explain(session.project(), error))?;
                    out["oscillator"] = json!(oscillator);
                    out["table"] = table_json(&report);
                    out["reading"] = json!(READING_A_TABLE);
                } else if let Some(name) = &request.table {
                    let report = session
                        .choose_wavetable(id, oscillator, name)
                        .map_err(|error| explain(session.project(), error))?;
                    out["oscillator"] = json!(oscillator);
                    out["table"] = table_json(&report);
                }
                if let Some(names) = &request.macros {
                    if names.len() > 4 {
                        return Err(format!("{} names for four macros", names.len()));
                    }
                    let named: Vec<(usize, String)> =
                        names.iter().enumerate().filter_map(|(which, name)| Some((which + 1, name.clone()?))).collect();
                    let names = session.name_macros(id, &named).map_err(|error| explain(session.project(), error))?;
                    out["macros"] = json!(names);
                }
                Ok(out)
            })();
            session.end_gesture();
            let mut out = result?;
            let model = session.project().device(id).map_err(|error| explain(session.project(), error))?;
            out["name"] = json!(model.name);
            Ok(out)
        })();
        finish(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(value: Value) -> BTreeMap<String, Value> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_recipe_that_cannot_be_read_says_how_one_is_written() {
        let error = recipe_of(&written(json!({ "frames": [{ "shape": "sawtooth" }] }))).unwrap_err();
        assert!(error.contains("sawtooth") && error.contains("saw") && error.contains("describe_wavetable"), "{error}");
        let error = recipe_of(&written(json!({ "frames": [{ "shape": "saw", "width": 0.2 }] }))).unwrap_err();
        assert!(error.contains("`width`") && error.contains("key frame 1"), "{error}");
        let error = recipe_of(&written(json!({ "frames": "saw" }))).unwrap_err();
        assert!(error.contains("cannot be read"), "{error}");
        assert_eq!(recipe_of(&written(json!({ "frames": [{ "shape": "saw" }] }))).unwrap().frames.len(), 1);
    }

    #[test]
    fn the_instructions_say_what_is_so() {
        // Every preset category it names, and nothing it does not have.
        for word in ["describe_wavetable", "set_wavetable", "macro1", "builtin:polysynth", "nobody has tuned"] {
            assert!(INSTRUCTIONS.contains(word), "{word}");
        }
        assert!(patches::all().count() > 12);
    }
}
