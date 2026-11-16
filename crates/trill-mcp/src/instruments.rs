//! Tools for what a track's notes mean to its instrument: drums or
//! pitches, which keys are drums, noises or keyswitches, where the range
//! ends, the octave it sounds at and how far it bends.
//!
//! A model finds such facts by listening (rendering, probing a key), and
//! each costs it a render. These tools are where a fact is kept once it is
//! found: on the track, in the song, and for the plugin on this machine, so
//! that the next track given that plugin starts out knowing. The other
//! tools read it from the track: a drum name lands on the kit's own key, an
//! articulation is asked for by name, and analysis leaves keyswitches and
//! drums out of the harmony.

use crate::input::{NoteInput, PitchInput, parse_notes_with, parse_pitch_with};
use crate::server::{Reply, TrillServer, edit, explain, finish};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use trill_core::pitch::{parse_drum, pitch_name};
use trill_core::{Articulation, ClipId, InstrumentProfile, Lasts, Note, Role, TrackId};
use trill_engine::Session;
use trill_engine::instruments::{InstrumentKnowledge, Known};

/// Appended to the server's instructions so a model knows these tools
/// exist and when to reach for them.
pub(crate) const INSTRUCTIONS: &str = "\
What a track's notes mean
- A track knows what its notes are to its instrument, and get_project shows it: a drum track (`plays: drums`), \
the articulations it can be asked for, its range, the octave it sounds at. A track is given this with its \
instrument: Trill ships it for some plugins and remembers what you taught it about others.
- A drum track's notes are drums: write_drums and drum names (\"china\", \"ride_bell\", \"closed_hat_edge\") go \
by the track's own map, and analyze_music keeps them out of the key and the chords.
- Sampled guitars and basses choose how a note is played with keyswitches: silent keys under the range. Never \
place those by hand. Ask by name: `articulation` on write_chords, write_bass, write_arpeggio and write_melody \
(\"palm_mute\"), or a mark on one note of a melody (\"1:0.5!palm_mute\"). The tool presses the key just before \
the note and fits the velocity where velocity decides (on some guitars a note under 64 is a muted one).
- When you learn something about an instrument that the track does not know (a key that switches, a drum on \
another note, where its range ends, that it sounds an octave down, how far it bends), call \
set_instrument_profile once with all of it. It is kept in the song and remembered for that plugin in every \
song. get_instrument_profile reads it back, with the instrument's whole drum map.";

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetInstrumentProfileRequest {
    /// Track id: what this track's notes mean to its instrument, and what is known about that kind of
    /// instrument.
    pub track: Option<u64>,
    /// A device type (a URI from list_devices): what is known about that kind of instrument, whichever track
    /// plays it. With neither `track` nor `uri`, every instrument something is known about is listed.
    pub uri: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetInstrumentProfileRequest {
    /// Track id. What you give is added to what the track already says; `replace` starts over.
    pub track: u64,
    /// "drums": the track's notes are drums, named by its drum map and kept out of the harmony. "pitched":
    /// they are pitches. "auto" (the default for a new track) leaves it to the instrument: a drum kit or a
    /// plugin that calls itself a drum instrument makes a drum track.
    pub role: Option<String>,
    /// Names for keys, each a MIDI note number or a note name: {"china": 52, "ride_bell": "F2"}. On a drum
    /// track these are its drums where they differ from General MIDI (which fills in the rest). On a pitched
    /// track they are keys that play a noise instead of a note (a pick scrape). null removes a name.
    pub sounds: Option<BTreeMap<String, Value>>,
    /// The ways of playing a note the instrument can be asked for, by name. A bare key is a keyswitch that
    /// stays chosen until another is pressed: {"palm_mute": 26}. An object says more:
    /// {"open": {"key": 24, "velocity": [64, 126], "default": true}, "muted": {"key": 24, "velocity": [32, 63]},
    /// "slide": {"key": 27, "lasts": "once"}, "hammer": {"key": 29, "lasts": "joins"}}.
    /// `lasts`: "stays" (default), "once" (the next note only) or "joins" (each note that starts while the one
    /// before still sounds is joined to it: hammer-ons, a slide along the string). `velocity` is the lowest and
    /// highest velocity that plays it, where how hard a note is hit decides. `default` marks how a note is
    /// played when nothing is asked for. Name them hammer, glide and slide and write_melody's ornaments use
    /// them. null removes one.
    pub articulations: Option<BTreeMap<String, Value>>,
    /// The lowest and highest key that plays a note, as numbers or note names: [36, 84] or ["C2", "C6"].
    pub range: Option<Vec<PitchInput>>,
    /// How many octaves from the key the instrument sounds: -1 if it sounds an octave under what is written.
    /// The writing tools then write an octave up so that what you ask for is what is heard.
    pub sounds_octaves: Option<i8>,
    /// How far the instrument's pitch wheel bends, in semitones each way (2 unless it says otherwise). Bends
    /// and vibrato in write_melody are scaled by it.
    pub bend_range: Option<f64>,
    /// true if a note that starts while the one before still sounds is slurred into, with no new attack (wind
    /// models, monophonic synths with legato). A hammer-on in write_melody is then written as an overlap.
    pub slurs: Option<bool>,
    /// Anything else worth knowing when writing for it, in a sentence or two ("silent without expression").
    pub notes: Option<String>,
    /// true: forget what the track said before and start from what you give now.
    pub replace: Option<bool>,
    /// false: keep this in the song only. By default (true) it is also remembered for the plugin, and given
    /// to every track that is given that plugin from now on.
    pub remember: Option<bool>,
    /// true: forget what was learned about this track's kind of instrument on this machine, and give the
    /// track what ships with Trill for it, or nothing. Give no other field with it.
    pub forget: Option<bool>,
}

/// The names a track gives to keys, beside note names: its drums, or its
/// noises and keyswitches.
pub(crate) struct TrackKeys {
    profile: InstrumentProfile,
    drums: bool,
}

impl TrackKeys {
    pub(crate) fn of(session: &Session, track: TrackId) -> Self {
        let profile = session.project().track(track).map(|track| track.profile.clone()).unwrap_or_default();
        Self { profile, drums: session.plays_drums(track) }
    }

    /// The keys of the track a clip is on; General MIDI for a clip that
    /// is not there, whose absence the edit will report.
    pub(crate) fn of_clip(session: &Session, clip: ClipId) -> Self {
        match session.project().clip_track(clip) {
            Ok(track) => Self::of(session, track),
            Err(_) => Self { profile: InstrumentProfile::default(), drums: false },
        }
    }

    fn key(&self, name: &str) -> Option<u8> {
        // A drum name means the General MIDI drum on any track, as it always has.
        self.profile.named_key(name, self.drums).or_else(|| parse_drum(name))
    }

    /// What a name could have been, for the error that it is none.
    fn expected(&self) -> String {
        let named: Vec<String> = if self.drums {
            self.profile.sounds.keys().cloned().collect()
        } else {
            self.profile
                .sounds
                .keys()
                .cloned()
                .chain(self.profile.articulation_names().iter().map(|n| n.to_string()))
                .collect()
        };
        match named.as_slice() {
            [] => "a drum name (like \"kick\")".to_string(),
            names if self.drums => format!("a drum name (like \"kick\", or this kit's own: {})", names.join(", ")),
            names => format!("a drum name (like \"kick\") or one of this track's keys ({})", names.join(", ")),
        }
    }

    pub(crate) fn pitch(&self, value: &Value) -> Result<u8, String> {
        parse_pitch_with(value, &|name| self.key(name), &self.expected())
    }

    pub(crate) fn notes(&self, notes: &[NoteInput]) -> Result<Vec<Note>, String> {
        parse_notes_with(notes, &|name| self.key(name), &self.expected())
    }
}

/// A profile as it is written and read back: the same shape both ways.
pub(crate) fn profile_json(profile: &InstrumentProfile) -> Value {
    let mut out = Map::new();
    if let Some(role) = profile.role {
        out.insert("role".into(), json!(role_name(role)));
    }
    if !profile.sounds.is_empty() {
        out.insert("sounds".into(), json!(profile.sounds));
    }
    if !profile.articulations.is_empty() {
        let articulations: Map<String, Value> =
            profile.articulations.iter().map(|a| (a.name.clone(), articulation_json(a))).collect();
        out.insert("articulations".into(), Value::Object(articulations));
    }
    if let Some((low, high)) = profile.range {
        out.insert("range".into(), json!([low, high]));
        out.insert("range_names".into(), json!(format!("{}-{}", pitch_name(low), pitch_name(high))));
    }
    if profile.sounds_octaves != 0 {
        out.insert("sounds_octaves".into(), json!(profile.sounds_octaves));
    }
    if let Some(bend) = profile.bend_range {
        out.insert("bend_range".into(), json!(bend));
    }
    if profile.slurs {
        out.insert("slurs".into(), json!(true));
    }
    if !profile.notes.is_empty() {
        out.insert("notes".into(), json!(profile.notes));
    }
    Value::Object(out)
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Drums => "drums",
        Role::Pitched => "pitched",
    }
}

fn articulation_json(articulation: &Articulation) -> Value {
    let plain = articulation.lasts == Lasts::Stays && articulation.velocity.is_none() && !articulation.default;
    match articulation.key {
        Some(key) if plain => json!(key),
        key => {
            let mut out = Map::new();
            if let Some(key) = key {
                out.insert("key".into(), json!(key));
            }
            match articulation.lasts {
                Lasts::Stays => {}
                Lasts::Once => drop(out.insert("lasts".into(), json!("once"))),
                Lasts::Joins => drop(out.insert("lasts".into(), json!("joins"))),
            }
            if let Some((low, high)) = articulation.velocity {
                out.insert("velocity".into(), json!([low, high]));
            }
            if articulation.default {
                out.insert("default".into(), json!(true));
            }
            Value::Object(out)
        }
    }
}

/// Reads a key given as a number or a note name.
fn key_of(value: &Value, what: &str) -> Result<u8, String> {
    parse_pitch_with(value, &|_| None, "nothing else").map_err(|error| format!("{what}: {error}"))
}

fn articulation_of(name: &str, value: &Value) -> Result<Articulation, String> {
    let what = format!("articulation '{name}'");
    let Value::Object(fields) = value else {
        return Ok(Articulation::key(name, key_of(value, &what)?));
    };
    if let Some(unknown) =
        fields.keys().find(|field| !["key", "lasts", "velocity", "default"].contains(&field.as_str()))
    {
        return Err(format!(
            "{what}: '{unknown}' is not something an articulation has; it has key, lasts, velocity and default"
        ));
    }
    let key = fields.get("key").filter(|key| !key.is_null()).map(|key| key_of(key, &what)).transpose()?;
    let lasts = match fields.get("lasts").filter(|lasts| !lasts.is_null()) {
        None => Lasts::Stays,
        Some(lasts) => match lasts.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("stays") => Lasts::Stays,
            Some("once") => Lasts::Once,
            Some("joins") => Lasts::Joins,
            _ => return Err(format!("{what}: `lasts` {lasts} should be \"stays\", \"once\" or \"joins\"")),
        },
    };
    let velocity = match fields.get("velocity").filter(|velocity| !velocity.is_null()) {
        None => None,
        Some(Value::Array(pair)) if pair.len() == 2 => {
            let bound =
                |value: &Value| value.as_u64().filter(|bound| (1..=127).contains(bound)).map(|bound| bound as u8);
            match (bound(&pair[0]), bound(&pair[1])) {
                (Some(low), Some(high)) => Some((low, high)),
                _ => {
                    return Err(format!(
                        "{what}: `velocity` {} should be two velocities from 1 to 127",
                        fields["velocity"]
                    ));
                }
            }
        }
        Some(other) => return Err(format!("{what}: `velocity` {other} should be [lowest, highest]")),
    };
    let default = fields.get("default").and_then(Value::as_bool).unwrap_or(false);
    Ok(Articulation { name: name.to_string(), key, lasts, velocity, default })
}

/// The profile `request` asks for, on top of `base`.
fn requested(request: &SetInstrumentProfileRequest, base: InstrumentProfile) -> Result<InstrumentProfile, String> {
    let mut profile = if request.replace == Some(true) { InstrumentProfile::default() } else { base };
    match request.role.as_deref().map(|role| role.trim().to_ascii_lowercase()).as_deref() {
        None => {}
        Some("drums" | "drum") => profile.role = Some(Role::Drums),
        Some("pitched" | "pitch") => profile.role = Some(Role::Pitched),
        Some("auto") => profile.role = None,
        Some(other) => return Err(format!("`role` '{other}' should be \"drums\", \"pitched\" or \"auto\"")),
    }
    for (name, value) in request.sounds.iter().flatten() {
        let key = trill_core::pitch::name_key(name);
        if value.is_null() {
            profile.sounds.remove(&key);
        } else {
            profile.sounds.insert(key, key_of(value, &format!("sound '{name}'"))?);
        }
    }
    for (name, value) in request.articulations.iter().flatten() {
        let key = trill_core::pitch::name_key(name);
        profile.articulations.retain(|articulation| articulation.name != key);
        if !value.is_null() {
            profile.articulations.push(articulation_of(&key, value)?);
        }
    }
    if let Some(range) = &request.range {
        profile.range = match range.as_slice() {
            [] => None,
            [low, high] => Some((key_of(&low.0, "range")?, key_of(&high.0, "range")?)),
            _ => {
                return Err("`range` should be the lowest and highest key, as in [36, 84]; [] takes it away".to_string());
            }
        };
    }
    if let Some(octaves) = request.sounds_octaves {
        profile.sounds_octaves = octaves;
    }
    if let Some(bend) = request.bend_range {
        profile.bend_range = Some(bend);
    }
    if let Some(slurs) = request.slurs {
        profile.slurs = slurs;
    }
    if let Some(notes) = &request.notes {
        profile.notes = notes.clone();
    }
    profile.validated().map_err(|error| error.to_string())
}

/// One line about a profile, for a listing.
fn summary(profile: &InstrumentProfile) -> String {
    let mut parts: Vec<String> = Vec::new();
    if profile.role == Some(Role::Drums) {
        parts.push("drums".to_string());
    }
    if !profile.sounds.is_empty() {
        parts.push(format!("{} named key(s)", profile.sounds.len()));
    }
    if !profile.articulations.is_empty() {
        parts.push(format!("articulations: {}", profile.articulation_names().join(", ")));
    }
    if let Some((low, high)) = profile.range {
        parts.push(format!("range {}-{}", pitch_name(low), pitch_name(high)));
    }
    if profile.sounds_octaves != 0 {
        parts.push(format!("sounds {} octave(s) from its keys", profile.sounds_octaves));
    }
    if profile.slurs {
        parts.push("slurs overlapping notes".to_string());
    }
    parts.join("; ")
}

fn known_json(known: &Known) -> Value {
    json!({ "uri": known.uri, "device": known.device, "source": known.source.name(), "profile": profile_json(&known.profile) })
}

/// What get_project says of a track's notes: nothing for an ordinary
/// pitched instrument.
pub(crate) fn describe_track(session: &Session, track: TrackId, out: &mut Value) {
    let Ok(found) = session.project().track(track) else { return };
    let profile = &found.profile;
    if session.plays_drums(track) {
        out["plays"] = json!("drums");
    }
    if !profile.articulations.is_empty() {
        out["articulations"] = json!(profile.articulation_names());
    }
    if let Some((low, high)) = profile.range {
        out["range"] = json!(format!("{}-{}", pitch_name(low), pitch_name(high)));
    }
    if profile.sounds_octaves != 0 {
        out["sounds_octaves"] = json!(profile.sounds_octaves);
    }
}

/// What is wrong with the notes of a clip for the instrument that plays
/// them, in words: things that cannot be heard from a reply.
pub(crate) fn profile_warnings(session: &Session, clip: ClipId) -> Vec<String> {
    let project = session.project();
    let (Ok(clip), Ok(track)) = (project.clip(clip), project.clip_track(clip)) else { return Vec::new() };
    let Ok(found) = project.track(track) else { return Vec::new() };
    let notes = clip.notes();
    note_warnings(&found.profile, session.plays_drums(track), notes, notes.len())
}

/// The warnings for `notes`, which were just written to a track with
/// `profile`. `in_clip` is how many notes their clip holds in all.
pub(crate) fn note_warnings(profile: &InstrumentProfile, drums: bool, notes: &[Note], in_clip: usize) -> Vec<String> {
    let mut warnings = Vec::new();
    if drums || profile.is_empty() {
        return warnings;
    }
    let switches: Vec<&Note> = notes.iter().filter(|note| profile.is_keyswitch(note.pitch)).collect();
    if !switches.is_empty() && switches.len() == notes.len() && notes.len() == in_clip {
        let mut names: Vec<&str> = switches.iter().filter_map(|note| profile.keyswitch_name(note.pitch)).collect();
        names.dedup();
        warnings.push(format!(
            "This clip holds only keyswitches ({}), which choose how notes are played and make no sound themselves. \
             Put the notes in the same clip, or ask for the articulation by name when writing them.",
            names.join(", ")
        ));
    }
    if let Some((low, high)) = profile.range {
        let outside: Vec<&Note> = notes
            .iter()
            .filter(|note| !profile.in_range(note.pitch))
            .filter(|note| !profile.is_keyswitch(note.pitch) && profile.sound_name(note.pitch).is_none())
            .collect();
        if !outside.is_empty() {
            let mut shown: Vec<String> = outside.iter().map(|note| pitch_name(note.pitch)).collect();
            shown.sort();
            shown.dedup();
            shown.truncate(6);
            warnings.push(format!(
                "{} note(s) are outside this instrument's range ({}-{}) and will not sound: {}. Move them into the \
                 range, or correct the range with set_instrument_profile if the instrument does play them.",
                outside.len(),
                pitch_name(low),
                pitch_name(high),
                shown.join(", ")
            ));
        }
    }
    warnings
}

#[tool_router(router = instrument_router, vis = "pub(crate)")]
impl TrillServer {
    /// What a track's notes mean to its instrument: whether they are drums, its drum map or keyswitches and
    /// noises, its range, the octave it sounds at and its bend range; and what is known about that kind of
    /// instrument (shipped with Trill, or learned on this machine). With `uri`, what is known about a device
    /// type; with neither, every instrument something is known about.
    #[tool(annotations(title = "Get instrument profile", read_only_hint = true))]
    fn get_instrument_profile(&self, Parameters(request): Parameters<GetInstrumentProfileRequest>) -> CallToolResult {
        let session = self.session.lock();
        finish(read(&session, &request))
    }

    /// Say what a track's notes mean to its instrument, in one call: drums or pitched, names for keys (a drum
    /// map, or noises), articulations with their keyswitches and velocities, the range, the octave it sounds
    /// at, its bend range. It is kept in the song and, unless `remember` is false, remembered for that plugin
    /// on this machine, so the next track given the plugin knows it too. One undo step for the track; what is
    /// remembered stays.
    #[tool(annotations(title = "Set instrument profile"))]
    fn set_instrument_profile(&self, Parameters(request): Parameters<SetInstrumentProfileRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        finish(write(&mut session, &request))
    }
}

fn read(session: &Session, request: &GetInstrumentProfileRequest) -> Reply {
    let knowledge = session.instruments();
    match (request.track, request.uri.as_deref()) {
        (Some(_), Some(_)) => Err("give `track` or `uri`, not both".to_string()),
        (None, Some(uri)) => match knowledge.of(uri) {
            Some(known) => Ok(known_json(&known)),
            None => Ok(json!({
                "uri": uri,
                "source": "none",
                "note": "Nothing is known about this kind of instrument yet. Put it on a track, find out what its \
                         keys do, and say so with set_instrument_profile.",
            })),
        },
        (None, None) => {
            let all: Vec<Value> = knowledge
                .all()
                .iter()
                .map(|known| {
                    json!({ "uri": known.uri, "device": known.device, "source": known.source.name(), "knows": summary(&known.profile) })
                })
                .collect();
            Ok(json!({ "instruments": all }))
        }
        (Some(track), None) => {
            let id = TrackId(track);
            let project = session.project();
            let found = project.track(id).map_err(|error| explain(project, error))?;
            Ok(track_reply(session, id, &found.profile))
        }
    }
}

/// What get and set say about a track.
fn track_reply(session: &Session, id: TrackId, profile: &InstrumentProfile) -> Value {
    let drums = session.plays_drums(id);
    let mut out = json!({
        "track": id,
        "plays": if drums { "drums" } else { "pitched" },
        "profile": profile_json(profile),
    });
    if profile.role.is_none() {
        out["plays_because"] = json!(if drums {
            "its instrument is a drum instrument"
        } else {
            "its instrument is not a drum instrument; role \"drums\" makes it one"
        });
    }
    match session.instrument_of(id) {
        Some(device) => {
            out["instrument"] = json!({ "device_id": device.id, "name": device.name, "uri": device.uri });
            out["known"] = match session.instruments().of(&device.uri) {
                Some(known) if known.profile == *profile => {
                    json!({ "source": known.source.name(), "same_as_track": true })
                }
                Some(known) => known_json(&known),
                None => json!({ "source": "none" }),
            };
        }
        None => out["instrument"] = json!(null),
    }
    if drums {
        // The whole map, the kit's own names first: what a lane or a note may be called.
        let map: Map<String, Value> = profile.drum_map().into_iter().map(|(name, key)| (name, json!(key))).collect();
        out["drum_map"] = Value::Object(map);
    }
    out
}

fn write(session: &mut Session, request: &SetInstrumentProfileRequest) -> Reply {
    let id = TrackId(request.track);
    let project = session.project();
    let base = project.track(id).map_err(|error| explain(project, error))?.profile.clone();
    let instrument = session.instrument_of(id).map(|device| (device.uri.clone(), device.name.clone()));
    let knowledge: std::sync::Arc<InstrumentKnowledge> = session.instruments().clone();

    if request.forget == Some(true) {
        let others = request.role.is_some()
            || request.sounds.is_some()
            || request.articulations.is_some()
            || request.range.is_some()
            || request.sounds_octaves.is_some()
            || request.bend_range.is_some()
            || request.slurs.is_some()
            || request.notes.is_some();
        if others {
            return Err("`forget` takes back what was learned; give it alone, then say what is true in another call"
                .to_string());
        }
        let Some((uri, name)) = instrument else {
            return Err(format!("{id} has no instrument whose profile could be forgotten"));
        };
        let forgotten = knowledge.forget(&uri)?;
        let shipped = knowledge.of(&uri).map(|known| known.profile).unwrap_or_default();
        edit(session, |project| project.set_track_profile(id, shipped.clone()))?;
        let mut out = track_reply(session, id, &shipped);
        out["forgotten"] = json!(if forgotten {
            format!("what was learned about {name} on this machine")
        } else {
            format!("nothing: nothing had been learned about {name}")
        });
        return Ok(out);
    }

    let profile = requested(request, base)?;
    edit(session, |project| project.set_track_profile(id, profile.clone()))?;
    let mut out = track_reply(session, id, &profile);
    let remember = request.remember.unwrap_or(true);
    out["remembered"] = match (&instrument, remember) {
        (_, false) => json!("no: kept in this song only"),
        (None, true) => json!("no: the track has no instrument to remember it for; it is kept in this song"),
        (Some((uri, name)), true) => {
            let remembered = knowledge.remember(uri, name, profile.clone())?;
            // After remembering, the track and what is known agree.
            out["known"] = json!({ "source": "learned", "same_as_track": true });
            match remembered.file {
                Some(file) => json!({ "for": format!("{name} ({uri})"), "file": file.display().to_string() }),
                None => {
                    json!({ "for": format!("{name} ({uri})"), "kept": "for this run only; nothing is written in a scratch session" })
                }
            }
        }
    };
    Ok(out)
}
