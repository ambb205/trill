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

use crate::input::{ChannelInput, Channels, NoteInput, parse_channel, parse_pitch_with, parse_played};
use crate::server::{Reply, TrillServer, edit, explain, finish};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use trill_core::pitch::{parse_drum, pitch_name};
use trill_core::{Articulation, Channel, ClipId, Drum, InstrumentProfile, Lasts, Note, Role, TrackId};
use trill_engine::Session;
use trill_engine::instruments::{InstrumentKnowledge, Known};

/// Appended to the server's instructions so a model knows these tools
/// exist and when to reach for them.
pub(crate) const INSTRUCTIONS: &str = "\
What a track's notes mean
- A track knows what its notes are to its instrument, and get_project shows it: a drum track (`plays: drums`), \
the articulations it can be asked for, its range, the octave it sounds at. A track is given this with its \
instrument: Trill ships it for some plugins and remembers what you taught it about others. A song that is \
opened takes it too, for the tracks that say nothing themselves.
- get_instrument_profile on a track is what to read before writing for it. For the instruments Trill knows \
(the Ample guitars and bass, Superior Drummer 3, the SWAM winds and strings, and whatever was taught on this \
machine) it is enough to write a part without probing anything.
- A drum track's notes are drums, written by name: write_drums lanes, and drum names in create_clip and \
add_notes. Which key a drum is on is the kit's business, and kits of one plugin differ: one hangs its china \
where another has a crash, or has no splash at all. Where the plugin says what its kit has on its keys \
(Superior Drummer 3 does), Trill reads the kit that is loaded, each time: get_instrument_profile lists its \
drums (`drum_map`), says what each is (`kit`) and which usual drums it has not got (`has_no`), and that changes \
when another kit is loaded. A drum the kit has not got is left out and the reply says so; it is never played on \
the key some other kit has it on. Never write a drum by key number, and do not assume General MIDI.
- Sampled guitars and basses choose how a note is played with keyswitches: silent keys under the range. Never \
place those as notes. Ask by name: `articulation` on write_chords, write_bass, write_arpeggio and write_melody \
(\"palm_mute\"), a mark on one note of a melody (\"1:0.5!palm_mute\"), or `how` on a note given to create_clip, \
add_notes or set_notes ([\"E2\", 1, 0.25, 100, \"palm_mute\"]). The tool presses the key just before the note \
and fits the velocity where velocity decides (on the metal guitars and the bass a note hit under 64 is a muted \
one: ask for \"muted\" or \"dead\" by name, and a plain note is raised to where it rings). A note that says \
nothing is played the ordinary way, and the keyswitches of a clip are kept right for all its notes however \
many calls put them there.
- A hammer-on or pull-off (\"!hammer\", `how: \"hammer\"`) joins a note to the one before; a slide from the \
note before is \"!slide\", a slide up into a note from below \"!scoop\". A harmonic rings above the key that \
plays it: the writing tools take the pitch you want to hear and write the key; in create_clip and add_notes a \
pitch is the key itself, as it is for a bass that sounds an octave under its keys.
- When you learn something about an instrument that the track does not know (a key that switches, a drum on \
another note, where its range ends, that it sounds an octave down, how far it bends), call \
set_instrument_profile once with all of it. It is kept in the song and remembered for that plugin in every \
song. get_instrument_profile reads it back and, in `notes`, says what else there is to know before writing for \
it: a setting of the plugin its keyswitches depend on, how far a hammer-on reaches, what was measured and what \
was not.
- MIDI channels are for an instrument that names its channels, and are left alone otherwise. Nearly every \
instrument plays all sixteen channels alike: for those never give `channel`, and everything is on the first. A \
few give their channels meanings of their own, and then get_instrument_profile lists them under `channels` \
(get_project shows them on the track): an organ whose pedalboard is channel 2, a guitar whose strings are \
channels 1 to 6 while its own \"String Channels\" setting is on. For such an instrument, and only for one, say \
the channel by its name there or by its number: `channel` on write_chords, write_bass, write_arpeggio, \
write_melody and write_drums puts the whole part on it (with `clip`, only that channel's notes in the bars \
written are replaced, so a bass line goes onto the pedalboard of the clip that already holds the manual's \
chords); `channel` on create_clip, add_notes and set_notes is for every note that says none; one note says its \
own as {\"pitch\": \"C2\", \"start\": 0, \"length\": 4, \"channel\": \"pedalboard\"}. A controller curve is on a \
channel too (`channel` on set_controllers and shape_dynamics): the pitch wheel of a channel bends that \
channel's notes and no others, which is how one string of a chord bends while the others hold. transform_notes \
puts notes on a channel with `to_channel`. Replies say channels only where a clip uses another than the first. \
Do not use channels to tell parts apart, to layer sounds or to route anything: that is what tracks are for.";

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
    /// Names for keys, each a MIDI note number, a note name or a key as a probe names it ("E3 (52)"):
    /// {"china": 52, "ride_bell": "F2"}. On a drum
    /// track these are its drums where they differ from General MIDI (which fills in the rest). On a pitched
    /// track they are keys that play a noise instead of a note (a pick scrape). null removes a name.
    pub sounds: Option<BTreeMap<String, Value>>,
    /// The ways of playing a note the instrument can be asked for, by name. A bare key is a keyswitch that
    /// stays chosen until another is pressed: {"palm_mute": 26}. An object says more:
    /// {"open": {"key": 24, "velocity": [64, 126], "default": true}, "muted": {"key": 24, "velocity": [32, 63]},
    /// "slide": {"key": 27, "lasts": "once"}, "hammer": {"key": 29, "lasts": "joins"}}.
    /// `lasts`: "stays" (default; a probe's `latches: true`), "once" (the next note only) or "joins" (each note that starts while the one
    /// before still sounds is joined to it: hammer-ons, a slide along the string). `velocity` is the lowest and
    /// highest velocity that plays it, where how hard a note is hit decides. `default` marks how a note is
    /// played when nothing is asked for. Name them hammer, glide and slide and write_melody's ornaments use
    /// them. null removes one.
    pub articulations: Option<BTreeMap<String, Value>>,
    /// The lowest and highest key that plays a note: [36, 84], ["C2", "C6"], or the text a probe reports
    /// ("E1-G4 (28-67)"; of several runs the lowest and highest key are taken). [] takes the range away.
    pub range: Option<Value>,
    /// How many octaves from the key the instrument sounds: -1 if it sounds an octave under what is written.
    /// The writing tools then write an octave up so that what you ask for is what is heard.
    pub sounds_octaves: Option<i8>,
    /// The same as a probe reports it: -12 for an octave under what is written. Give this or `sounds_octaves`.
    pub offset_semitones: Option<f64>,
    /// How far the instrument's pitch wheel bends, in semitones each way (2 unless it says otherwise). Bends
    /// and vibrato in write_melody are scaled by it.
    pub bend_range: Option<f64>,
    /// true if a note that starts while the one before still sounds is slurred into, with no new attack (wind
    /// models, monophonic synths with legato). A hammer-on in write_melody is then written as an overlap.
    pub slurs: Option<bool>,
    /// What the instrument's MIDI channels are, where it gives them meanings of their own, by number:
    /// {"2": "Pedalboard"} for an organ that plays its pedals from channel 2, {"1": "String 1", "2": "String 2"}
    /// for a guitar with a string to a channel. Notes and controller curves can then be put on a channel by
    /// that name. null removes a name. Say nothing here for an ordinary instrument, which plays every channel
    /// alike.
    pub channels: Option<BTreeMap<String, Value>>,
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
        // With what the kit that is loaded says of its keys, where it says.
        Self { profile: session.profile_of(track), drums: session.plays_drums(track) }
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
        if self.drums && self.profile.whole {
            // The kit said what is on every key: a drum it has not got is on none of them.
            return match self.profile.find_drum(name) {
                Drum::Key(key) => Some(key.pitch),
                Drum::Lacking(_) | Drum::Unknown => None,
            };
        }
        // A drum name means the General MIDI drum on any track, as it always has.
        self.profile.named_key(name, self.drums).or_else(|| parse_drum(name))
    }

    /// What a name could have been, for the error that it is none.
    fn expected(&self) -> String {
        if self.drums && self.profile.whole {
            let mut names: Vec<String> = self.profile.sounds.keys().cloned().collect();
            names.sort_by_key(|name| (name.len(), name.clone()));
            names.truncate(40);
            return format!(
                "a drum of this track's kit, which has no other (the shorter names of those it has: {})",
                names.join(", ")
            );
        }
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

    /// The notes with, in step with them, how each asks to be played. A
    /// note that says no channel is on `unsaid`.
    pub(crate) fn played(
        &self,
        notes: &[NoteInput],
        unsaid: Channel,
    ) -> Result<(Vec<Note>, Vec<Option<String>>), String> {
        let channels = Channels { profile: &self.profile, unsaid };
        parse_played(notes, &|name| self.key(name), &self.expected(), channels)
    }

    /// The channel `input` asks for, by its number or by what the
    /// track's instrument calls it: the first where nothing is asked.
    pub(crate) fn channel(&self, input: Option<&ChannelInput>) -> Result<Channel, String> {
        input.map_or(Ok(Channel::FIRST), |input| parse_channel(&input.0, &self.profile))
    }

    /// The channel `input` asks for, if it asks.
    pub(crate) fn channel_asked(&self, input: Option<&ChannelInput>) -> Result<Option<Channel>, String> {
        input.map(|input| parse_channel(&input.0, &self.profile)).transpose()
    }

    /// What there is to say of something written on `channel`: nothing
    /// on the first, or where the instrument names its channels; a
    /// warning on any other channel of an instrument that names none,
    /// where a channel is not anything to choose.
    pub(crate) fn channel_warning(&self, channel: Channel) -> Option<String> {
        channel_warning(&self.profile, channel)
    }
}

/// What there is to say of something written on `channel` for an
/// instrument with `profile`: see [`TrackKeys::channel_warning`].
pub(crate) fn channel_warning(profile: &InstrumentProfile, channel: Channel) -> Option<String> {
    (!channel.is_first() && !profile.names_channels()).then(|| {
        format!(
            "This was written on MIDI channel {channel}, and this track's instrument names none of its channels: \
             most instruments play every channel alike, and one that listens to the first only plays none of \
             this. Use a channel only for an instrument whose get_instrument_profile lists `channels`."
        )
    })
}

/// Notes that say how they are played, as the clip they go into is to
/// hold them.
pub(crate) struct Played {
    /// Every note the clip is to hold: the ones it had, these, and the
    /// keyswitches for all of them.
    pub all: Vec<Note>,
    /// The keyswitches these notes took, by articulation.
    pub keyswitches: BTreeMap<String, usize>,
    /// What was done that was not asked for, in words.
    pub remarks: Vec<String>,
}

/// Works out what a clip on `track` holds once `notes` are in it, on an
/// instrument that has articulations: the instrument's keyswitches are
/// written for the whole clip, `held` being what it holds now, so that
/// each note is played as it says (`how`, in step with the notes; a note
/// that says nothing is played the instrument's ordinary way) and leaves
/// the others played as they were.
///
/// `None` where the notes go in as they are, as they always did: on a
/// track whose instrument has no articulations, and where the notes
/// themselves hold keyswitches that someone placed as notes.
pub(crate) fn played(
    session: &Session,
    track: TrackId,
    held: &[Note],
    notes: &[Note],
    how: &[Option<String>],
) -> Result<Option<Played>, String> {
    use trill_theory::articulation::{Asked, articulate_asked, is_our_writing, read_back, unknown};
    let profile = session.profile_of(track);
    let drums = session.plays_drums(track);
    let by_hand = notes.iter().find(|note| !drums && profile.is_keyswitch(note.pitch));
    match (how.iter().flatten().next(), by_hand) {
        (Some(first), _) if drums => {
            return Err(format!(
                "'{first}': track {} is a drum track, whose notes are drums by name; `how` is for an instrument                  with articulations (a sampled guitar or bass)",
                track.0
            ));
        }
        (Some(first), _) if profile.articulations.is_empty() => return Err(unknown(&profile, first).to_string()),
        (Some(_), Some(switch)) => {
            return Err(format!(
                "{} is this instrument's '{}' keyswitch, placed as a note beside notes that say how they are                  played: leave it out and give `how` to the notes it was meant for",
                pitch_name(switch.pitch),
                profile.keyswitch_name(switch.pitch).unwrap_or_default()
            ));
        }
        // Nothing to press for, or presses placed by hand: the notes are their own.
        (None, _) if drums || profile.articulations.is_empty() || by_hand.is_some() || notes.is_empty() => {
            return Ok(None);
        }
        _ => {}
    }
    let asked: Vec<Asked> = notes.iter().zip(how).map(|(note, how)| Asked::new(*note, how.clone())).collect();
    let mut remarks = Vec::new();
    // A clip without a keyswitch in it has none that was placed by hand.
    let ours = !held.iter().any(|note| profile.is_keyswitch(note.pitch)) || is_our_writing(&profile, held);
    let (articulated, keyswitches) = if ours {
        let mut all = read_back(&profile, held);
        let kept = articulate_asked(&profile, &all, 0.0).map_err(|error| error.to_string())?.presses;
        all.extend(asked);
        let articulated = articulate_asked(&profile, &all, 0.0).map_err(|error| error.to_string())?;
        let mut added = articulated.presses.clone();
        for (name, count) in &mut added {
            *count = count.saturating_sub(kept.get(name).copied().unwrap_or(0));
        }
        added.retain(|_, count| *count > 0);
        (articulated, added)
    } else {
        remarks.push(
            "This clip holds keyswitches that were placed by hand, another way than these tools place them. They \
             were left as they are, and these notes were given switches of their own: check how the notes after \
             them are played (analyze_music reads it), or write the clip again with `how` on its notes."
                .to_string(),
        );
        let mut articulated = articulate_asked(&profile, &asked, 0.0).map_err(|error| error.to_string())?;
        let keyswitches = articulated.presses.clone();
        articulated.notes.extend(held.iter().copied());
        (articulated, keyswitches)
    };
    if let (Some((low, high)), true) = (articulated.default_range, articulated.fitted > 0) {
        remarks.push(format!(
            "{} note(s) played the ordinary way (a note that is slid or hammered into is one too) were outside              the velocities {low}-{high} that play it; they were moved to the nearest of those. Say how such a note              is to be played if softer or harder was meant.",
            articulated.fitted
        ));
    }
    Ok(Some(Played { all: articulated.notes, keyswitches, remarks }))
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
    if !profile.channels.is_empty() {
        out.insert("channels".into(), json!(profile.channels));
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
    let plain = articulation.lasts == Lasts::Stays
        && articulation.velocity.is_none()
        && !articulation.default
        && articulation.sounds == 0;
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
            if articulation.sounds != 0 {
                out.insert("sounds_semitones".into(), json!(articulation.sounds));
            }
            Value::Object(out)
        }
    }
}

/// Reads a key given as a number, a note name, or the way a probe names
/// one: `"D1 (26)"`.
fn key_of(value: &Value, what: &str) -> Result<u8, String> {
    if let Some(number) = value.as_str().and_then(numbered) {
        return Ok(number);
    }
    parse_pitch_with(value, &|_| None, "a key as a probe names it (like \"D1 (26)\")")
        .map_err(|error| format!("{what}: {error}"))
}

/// The number in a key written as a probe writes it: `"D1 (26)"`.
fn numbered(text: &str) -> Option<u8> {
    let (_, rest) = text.trim().rsplit_once('(')?;
    rest.strip_suffix(')')?.trim().parse::<u8>().ok().filter(|key| *key <= 127)
}

/// Reads a range: `[low, high]`, or runs of keys as a probe lists them
/// (`"E1-G4 (28-67), C5 (72)"`), of which the lowest and the highest key
/// are taken. `[]` is no range.
fn range_of(value: &Value) -> Result<Option<(u8, u8)>, String> {
    let wrong = || {
        format!(
            "`range` {value} should be the lowest and highest key, as in [36, 84] or \"E1-G4 (28-67)\"; [] takes it away"
        )
    };
    match value {
        Value::Array(keys) if keys.is_empty() => Ok(None),
        Value::Array(keys) if keys.len() == 2 => Ok(Some((key_of(&keys[0], "range")?, key_of(&keys[1], "range")?))),
        Value::String(text) => {
            let mut keys: Vec<u8> = Vec::new();
            for run in text.split(',').map(str::trim).filter(|run| !run.is_empty()) {
                // "E1-G4 (28-67)" says it in numbers; "E1-G4" in names.
                let ends: Vec<String> = match run.rsplit_once('(') {
                    Some((_, numbers)) => numbers.trim_end_matches(')').split('-').map(str::to_string).collect(),
                    None => split_names(run),
                };
                for end in ends {
                    keys.push(
                        key_of(&json!(end.trim()), "range")
                            .or_else(|_| end.trim().parse::<u8>().ok().filter(|key| *key <= 127).ok_or_else(wrong))?,
                    );
                }
            }
            match (keys.iter().min(), keys.iter().max()) {
                (Some(low), Some(high)) => Ok(Some((*low, *high))),
                _ => Err(wrong()),
            }
        }
        _ => Err(wrong()),
    }
}

/// The two names of a run written as `"E1-G4"`: split at the dash that a
/// note letter follows, since an octave may be negative (`"C-1-G9"`).
fn split_names(run: &str) -> Vec<String> {
    let dash = run.char_indices().skip(1).find(|(index, c)| {
        *c == '-' && run[index + 1..].starts_with(|next: char| matches!(next.to_ascii_uppercase(), 'A'..='G'))
    });
    match dash {
        Some((index, _)) => vec![run[..index].to_string(), run[index + 1..].to_string()],
        None => vec![run.to_string()],
    }
}

/// What an articulation may say of itself.
const ARTICULATION_FIELDS: [&str; 6] = ["key", "lasts", "latches", "velocity", "default", "sounds_semitones"];

fn articulation_of(name: &str, value: &Value) -> Result<Articulation, String> {
    let what = format!("articulation '{name}'");
    let Value::Object(fields) = value else {
        return Ok(Articulation::key(name, key_of(value, &what)?));
    };
    if let Some(unknown) = fields.keys().find(|field| !ARTICULATION_FIELDS.contains(&field.as_str())) {
        return Err(format!(
            "{what}: '{unknown}' is not something an articulation has; it has key, lasts (or latches), velocity, \
             default and sounds_semitones"
        ));
    }
    let key = fields.get("key").filter(|key| !key.is_null()).map(|key| key_of(key, &what)).transpose()?;
    let lasts = match fields.get("lasts").filter(|lasts| !lasts.is_null()) {
        // As a probe reports it: a key whose change goes on after it has come up.
        None => match fields.get("latches").and_then(Value::as_bool) {
            Some(false) => Lasts::Once,
            _ => Lasts::Stays,
        },
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
    let sounds = match fields.get("sounds_semitones").filter(|sounds| !sounds.is_null()) {
        None => 0,
        Some(sounds) => match sounds.as_i64().filter(|semitones| (-48..=48).contains(semitones)) {
            Some(semitones) => semitones as i8,
            None => {
                return Err(format!(
                    "{what}: `sounds_semitones` {sounds} should be how many semitones above its key a note \
                     played this way is heard, as a whole number (12 for a harmonic an octave up)"
                ));
            }
        },
    };
    Ok(Articulation { name: name.to_string(), key, lasts, velocity, default, sounds })
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
        profile.range = range_of(range)?;
    }
    match (request.sounds_octaves, request.offset_semitones) {
        (Some(_), Some(_)) => return Err("give `sounds_octaves` or `offset_semitones`, not both".to_string()),
        (Some(octaves), None) => profile.sounds_octaves = octaves,
        (None, Some(semitones)) => {
            let octaves = semitones / 12.0;
            if !semitones.is_finite() || octaves.fract() != 0.0 || octaves.abs() > 4.0 {
                return Err(format!(
                    "`offset_semitones` {semitones} is not a whole number of octaves; an instrument that is tuned \
                     or transposed otherwise is set right in the instrument (get_device, update_device), not here"
                ));
            }
            profile.sounds_octaves = octaves as i8;
        }
        (None, None) => {}
    }
    if let Some(bend) = request.bend_range {
        profile.bend_range = Some(bend);
    }
    if let Some(slurs) = request.slurs {
        profile.slurs = slurs;
    }
    for (number, name) in request.channels.iter().flatten() {
        let channel = number.trim().parse::<u8>().ok().and_then(Channel::new).ok_or_else(|| {
            format!("`channels`: '{number}' is not a MIDI channel; they are numbered 1 to 16, as in {{\"2\": \"Pedalboard\"}}")
        })?;
        match name {
            Value::Null => drop(profile.channels.remove(&channel)),
            Value::String(name) => drop(profile.channels.insert(channel, name.clone())),
            other => return Err(format!("`channels`: channel {channel} should be given a name, got {other}")),
        }
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
    if profile.names_channels() {
        let names: Vec<String> = profile.channels.keys().map(|channel| profile.channel_label(*channel)).collect();
        parts.push(format!("channels: {}", names.join(", ")));
    }
    parts.join("; ")
}

fn known_json(known: &Known) -> Value {
    json!({ "uri": known.uri, "device": known.device, "source": known.source.name(), "profile": profile_json(&known.profile) })
}

/// What a probe's reply says about how much of what it measures is known
/// already about the device type `uri`, and where to keep what is not.
pub(crate) fn probe_note(session: &Session, uri: &str, track: Option<TrackId>) -> String {
    let on_track = track.and_then(|id| session.project().track(id).ok()).is_some_and(|track| !track.profile.is_empty());
    match session.instruments().of(uri) {
        Some(known) => format!(
            "Trill already knows this instrument ({}): {}. get_instrument_profile reads it all; a probe is only \
             needed to check it or to find what it lacks.",
            match known.source {
                trill_engine::instruments::Source::BuiltIn => "it ships with an entry for it",
                trill_engine::instruments::Source::Learned => "it was taught on this machine",
            },
            summary(&known.profile)
        ),
        None if on_track => "This track says what its notes mean (get_instrument_profile); nothing is remembered \
                             for the plugin itself yet. set_instrument_profile remembers it."
            .to_string(),
        None => "Nothing is recorded about this instrument yet. Keep what this probe found with one call: \
                 set_instrument_profile with `range` (the text above), `offset_semitones`, and for keys that \
                 switch or play something else `articulations` and `sounds`, written as this reply names them. \
                 It is then known for every song."
            .to_string(),
    }
}

/// Why a track that had notes made no sound, where what the track knows
/// about its instrument says: its notes were keyswitches, or outside the
/// range. `None` where the track knows nothing that explains it.
pub(crate) fn known_silence(track: &trill_core::Track, start: f64, end: Option<f64>) -> Option<String> {
    let profile = &track.profile;
    if profile.is_empty() {
        return None;
    }
    let notes: Vec<Note> = track
        .clips
        .iter()
        .filter(|clip| !clip.muted)
        .flat_map(|clip| clip.timeline_notes())
        .filter(|note| note.end() > start && end.is_none_or(|end| note.start < end))
        .collect();
    if notes.is_empty() {
        return None;
    }
    let switches = notes.iter().filter(|note| profile.is_keyswitch(note.pitch)).count();
    if switches == notes.len() {
        let mut names: Vec<&str> = notes.iter().filter_map(|note| profile.keyswitch_name(note.pitch)).collect();
        names.sort_unstable();
        names.dedup();
        return Some(format!(
            " Its {} note(s) here are all keyswitches of this instrument ({}): they choose how notes are played \
             and make no sound themselves. Write the notes to be heard, asking for the articulation by name.",
            notes.len(),
            names.join(", ")
        ));
    }
    let (low, high) = profile.range?;
    let music: Vec<&Note> = notes
        .iter()
        .filter(|note| !profile.is_keyswitch(note.pitch) && profile.sound_name(note.pitch).is_none())
        .collect();
    let outside = music.iter().filter(|note| !profile.in_range(note.pitch)).count();
    (outside > 0 && outside == music.len()).then(|| {
        let lowest = music.iter().map(|note| note.pitch).min().unwrap_or(low);
        let highest = music.iter().map(|note| note.pitch).max().unwrap_or(high);
        format!(
            " Its notes here run from {} to {}, all outside this instrument's range ({}-{}): move them into it.",
            pitch_name(lowest),
            pitch_name(highest),
            pitch_name(low),
            pitch_name(high)
        )
    })
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
    // With what the instrument says of its channels as it is set now.
    let channels = session.profile_of(track).channels;
    if !channels.is_empty() {
        out["channels"] = json!(channels);
    }
}

/// What is wrong with the notes of a clip for the instrument that plays
/// them, in words: things that cannot be heard from a reply.
pub(crate) fn profile_warnings(session: &Session, clip: ClipId) -> Vec<String> {
    let project = session.project();
    let (Ok(clip), Ok(track)) = (project.clip(clip), project.clip_track(clip)) else { return Vec::new() };
    // What the notes mean is the session's to say: the track's own words, and under them what an
    // instrument that a file describes says of itself.
    let notes = clip.notes();
    note_warnings(&session.profile_of(track), session.plays_drums(track), notes, notes.len())
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
    if profile.bend_range.is_none()
        && let Some(range) = session.instrument_bend_range(id)
    {
        // The instrument's own setting, which the track follows while it says nothing itself.
        out["pitch_wheel"] = json!(format!(
            "bends {range} semitone(s) each way: the instrument's `bend_range` parameter, which bends written for \
             this track follow (set that parameter to change it)"
        ));
    }
    // A sampled instrument says what its keys do in its own file: its range, whether they are drums, the
    // ways of playing a key chooses. The track follows that wherever it says nothing itself.
    let sampled = session.sampled_of(id);
    if let Some(said) = &sampled {
        out["instrument_says"] = json!({
            "name": said.name,
            "profile": profile_json(&said.profile),
            "note": "Read from the instrument's file, now: none of it is kept in the song, and it changes with the \
                     instrument that is loaded. What the track says itself (`profile`) is laid over it.",
        });
    }
    // What the instrument's channels are: the track's own words, or what the instrument is set to now.
    let whole = session.profile_of(id);
    if whole.names_channels() {
        let own = profile.names_channels();
        out["channels"] = json!({
            "names": whole.channels,
            "read_from": if own {
                "the track's profile, which is kept in the song"
            } else {
                "the instrument as it is set now: kept nowhere in the song, and it changes with the instrument's \
                 own setting"
            },
            "write_on_one": "`channel` on create_clip, add_notes, set_notes and the write_ tools puts notes on a \
                             channel, by its number or its name here; on one note of create_clip, add_notes or \
                             set_notes as {\"pitch\": \"C2\", \"start\": 0, \"length\": 4, \"channel\": 2}; \
                             `channel` on set_controllers and shape_dynamics puts a curve there; transform_notes \
                             moves notes with `to_channel`. A channel that is not named plays as the first does.",
        });
    }
    let asked_by_name = !profile.articulations.is_empty()
        || sampled.as_ref().is_some_and(|said| !said.profile.articulations.is_empty());
    if !drums && asked_by_name {
        out["ask_by_name"] = json!(
            "`articulation` on write_chords, write_bass, write_arpeggio and write_melody; a mark on one note of a \
             melody (\"E3:0.5!palm_mute\"); `how` on a note of create_clip, add_notes and set_notes ([\"E2\", 0, \
             0.5, 100, \"palm_mute\"]). The keyswitches are pressed for you: never write them as notes."
        );
    }
    if drums {
        // The whole map, the kit's own names first: what a lane or a note may be called. Where the
        // instrument says what its kit has on its keys, it is that kit's, read now.
        let whole = session.profile_of(id);
        let map: Map<String, Value> = whole.drum_map().into_iter().map(|(name, key)| (name, json!(key))).collect();
        out["drum_map"] = Value::Object(map);
        if whole.whole {
            let lacking: Vec<&str> =
                USUAL_DRUMS.iter().copied().filter(|name| matches!(whole.find_drum(name), Drum::Lacking(_))).collect();
            out["kit"] = json!({
                "read_from": if sampled.is_some() {
                    "the instrument's file, now: it is the kit that is loaded, and changes with it"
                } else {
                    "the plugin's own state, now: it is the kit that is loaded, and changes with it"
                },
                "what": whole.notes,
                "has_no": lacking,
                "note": "These names are the whole kit: a drum that is not in `drum_map` is not played by any key, \
                         and the writing tools leave it out and say so rather than hit another drum. General \
                         MIDI's keys do not hold here.",
            });
        }
    }
    out
}

/// The drums a song is usually written with: which of them a kit has not
/// got is worth saying before a part is written for it.
const USUAL_DRUMS: [&str; 18] = [
    "kick",
    "snare",
    "rimshot",
    "rim",
    "clap",
    "closed_hat",
    "pedal_hat",
    "open_hat",
    "high_tom",
    "mid_tom",
    "low_tom",
    "floor_tom",
    "crash",
    "crash2",
    "china",
    "splash",
    "ride",
    "ride_bell",
];

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
            || request.offset_semitones.is_some()
            || request.bend_range.is_some()
            || request.slurs.is_some()
            || request.channels.is_some()
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
