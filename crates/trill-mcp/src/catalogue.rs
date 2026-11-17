//! Tools for the sampled instruments others publish (`trill-catalogue`):
//! what there is to be had, and what a song owes for the ones it plays.
//!
//! * `instrument_catalogue` reads the catalogue: counted by kind and by
//!   licence, searched by words, or one set with its licence as its maker
//!   wrote it. It says which instruments are in the sound library already
//!   and where. It fetches nothing: a set is fetched by the user, who is
//!   shown its size and its licence first.
//! * `song_credits` reads the song: for the sampled instruments it plays,
//!   the lines of the makers who ask to be named.

use crate::server::{Reply, TrillServer, finish};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use trill_catalogue::{Catalogue, Fetch, Instrument, Kind, Licence, Search, Set};
use trill_core::pitch::pitch_name;

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Sampled instruments to be had, and what a song owes
- Trill comes with no recordings. It knows a catalogue of sampled instruments that others publish under licences \
that let anyone use them and hand them on: pianos, electric pianos, strings, basses, guitars, winds, drum kits \
and more. instrument_catalogue lists them: with no arguments what there is by kind, with `query` or `kind` the \
instruments (each with a sentence on what it is, its size, its licence, what was measured against it, and `here` \
with its file where it is in the sound library already), with `set` one set with its licence in its maker's words.
- You cannot fetch one, and must not try another way: a set is fetched by the user, in Trill's settings, who is \
shown its size and its licence first. When a song wants an instrument that is not `here`, say which set has it \
and how large it is, and go on with what is there. One that is `here` loads by its file: `preset` on create_track \
or add_device with builtin:sampled.
- Some of those makers ask to be named when their instrument is used. song_credits says what the open song owes: \
the lines to put where the song is released, each with the tracks it is for. Say so when a song is finished and \
plays such an instrument; nothing is owed for the ones in the public domain.";

/// How many instruments a reply lists when the caller does not say, and
/// the most it can ask for.
const DEFAULT_LISTED: usize = 30;
const MAX_LISTED: usize = 200;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct InstrumentCatalogueRequest {
    /// Words to find instruments by, every one of which must be in an instrument's name, the sentence that
    /// says what it is, its kind, or its set's name or maker: "upright piano", "nylon guitar", "brushes".
    pub query: Option<String>,
    /// Only instruments of this kind: "piano", "electric-piano", "organ", "keyboard", "guitar", "bass",
    /// "strings", "brass", "woodwind", "voice", "mallets", "plucked", "drum-kit", "percussion", "synth",
    /// "effects".
    pub kind: Option<String>,
    /// One set by its id (as `set` in a listing): everything about it, with its licence in its maker's words,
    /// whom to credit, where it is published, and every instrument in it.
    pub set: Option<String>,
    /// Only the starter selection: the best of each kind that was found, about a gigabyte in all.
    pub starter: Option<bool>,
    /// Only instruments in the public domain, for which nobody has to be named.
    pub public_domain: Option<bool>,
    /// Also list the instruments something that was measured speaks against (noisy, out of tune, uneven,
    /// silent, incomplete, or played without what makes most of their sound). Left out otherwise.
    pub include_marked: Option<bool>,
    /// Only instruments that are in the sound library already.
    pub here: Option<bool>,
    /// The most instruments to list (default 30, at most 200).
    pub limit: Option<usize>,
    /// How many to skip, for the next page.
    pub offset: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SongCreditsRequest {}

/// `"712 MB"`, `"1.9 GB"`, `"840 KB"`.
fn size_words(bytes: u64) -> String {
    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    if megabytes >= 1024.0 {
        format!("{:.1} GB", megabytes / 1024.0)
    } else if megabytes >= 1.0 {
        format!("{megabytes:.0} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

fn instrument_json(set: &Set, instrument: &Instrument, here: &HashMap<String, PathBuf>) -> Value {
    let mut out = json!({
        "name": instrument.name,
        "kind": instrument.kind.id(),
        "what": instrument.words,
        "set": set.id,
        "size": size_words(instrument.bytes),
        "set_size": size_words(set.bytes),
        "licence": set.licence.id(),
    });
    if let Some((low, high)) = instrument.range {
        let what = if instrument.drums { "drums_on" } else { "range" };
        out[what] = json!(format!("{}-{}", pitch_name(low), pitch_name(high)));
    }
    if instrument.layers > 1 {
        out["layers"] = json!(instrument.layers);
    }
    if instrument.takes > 1 {
        out["takes"] = json!(instrument.takes);
    }
    if !instrument.ways.is_empty() {
        out["ways_of_playing"] = json!(instrument.ways);
    }
    if instrument.is_starter() {
        out["starter"] = json!(true);
    }
    let against: Vec<&str> = instrument.marks.iter().filter(|mark| mark.is_fault()).map(|mark| mark.words()).collect();
    if !against.is_empty() {
        out["measured_against_it"] = json!(against);
    }
    if let Some(misses) = &instrument.misses {
        out["not_played"] = json!(misses);
    }
    if set.credit.is_some() {
        out["credit_asked"] = json!(true);
    }
    if let Some(path) = here.get(&instrument.text) {
        out["here"] = json!(path.display().to_string());
    }
    out
}

/// What was measured of an instrument, for the listing of one set.
fn measured_json(instrument: &Instrument) -> Value {
    let mut out = Map::new();
    if let Some((middle, noisiest)) = instrument.noise {
        out.insert(
            "noise".into(),
            json!(format!(
                "the quietest part of a recording lies {middle:.0} dB under its peak (the middle recording), \
                 {noisiest:.0} dB for the noisiest tenth"
            )),
        );
    }
    if let Some((middle, usual, worst)) = instrument.tuning {
        out.insert(
            "tuning".into(),
            json!(format!(
                "its recorded keys lie {middle:+.0} cents from equal temperament (the middle one), usually within                  {usual:.0} cents of that, {worst:+.0} at the worst"
            )),
        );
    }
    if let Some((usual, most)) = instrument.evenness {
        out.insert(
            "evenness".into(),
            json!(format!(
                "from one recorded key to the next the level usually steps by {usual:.1} dB, {most:.1} dB at the most"
            )),
        );
    }
    if let Some(level) = instrument.level {
        out.insert("level".into(), json!(format!("{level:.0} dB at the device's own volume")));
    }
    if let Some(note) = &instrument.note {
        out.insert("note".into(), json!(note));
    }
    Value::Object(out)
}

fn set_json(set: &Set, here: &HashMap<String, PathBuf>) -> Value {
    let instruments: Vec<Value> = set
        .instruments
        .iter()
        .map(|instrument| {
            let mut out = instrument_json(set, instrument, here);
            let measured = measured_json(instrument);
            if measured.as_object().is_some_and(|measured| !measured.is_empty()) {
                out["measured"] = measured;
            }
            if let Some(object) = out.as_object_mut() {
                for said_once in ["set", "set_size", "licence", "credit_asked"] {
                    object.remove(said_once);
                }
            }
            out
        })
        .collect();
    let (how, from) = match &set.fetch {
        Fetch::Archive { url, .. } => ("as one archive", url.clone()),
        Fetch::Files { base, files } => (if files.len() == 1 { "as one file" } else { "file by file" }, base.clone()),
    };
    let mut out = json!({
        "set": set.id,
        "name": set.name,
        "what": set.about,
        "by": set.by,
        "size": size_words(set.bytes),
        "files": set.files,
        "licence": { "id": set.licence.id(), "name": set.licence.name(), "says": set.licence_says, "read_at": set.licence_at, "read_on": set.read },
        "published_at": set.from,
        "version": set.version,
        "fetched": { "how": how, "from": from },
        "instruments": instruments,
        "fetching": "The user fetches a set in Trill's settings, where its size and licence are shown first. This tool fetches nothing.",
    });
    match &set.credit {
        Some(line) => out["credit"] = json!(line),
        None => out["credit"] = json!("none asked: it is in the public domain"),
    }
    if set.is_starter() {
        out["starter"] = json!(true);
    }
    out
}

/// What there is, counted: the reply to a call that asks for nothing.
fn overview(catalogue: &Catalogue, here: &HashMap<String, PathBuf>) -> Value {
    let mut kinds: BTreeMap<Kind, (usize, usize)> = BTreeMap::new();
    let mut licences: BTreeMap<Licence, (usize, usize)> = BTreeMap::new();
    for (set, instrument) in catalogue.instruments() {
        let of_kind = kinds.entry(instrument.kind).or_default();
        of_kind.0 += 1;
        of_kind.1 += usize::from(instrument.is_put_forward());
        licences.entry(set.licence).or_default().1 += 1;
    }
    for set in &catalogue.sets {
        licences.entry(set.licence).or_default().0 += 1;
    }
    let starter = catalogue.starter();
    let starter_bytes: u64 = starter.iter().map(|set| set.bytes).sum();
    let all_bytes: u64 = catalogue.sets.iter().map(|set| set.bytes).sum();
    let by_kind: Map<String, Value> = kinds
        .iter()
        .map(|(kind, (all, forward))| (kind.id().to_string(), json!({ "instruments": all, "put_forward": forward })))
        .collect();
    let by_licence: Map<String, Value> = licences
        .iter()
        .map(|(licence, (sets, instruments))| {
            (
                licence.id().to_string(),
                json!({ "sets": sets, "instruments": instruments, "credit_asked": licence.asks_credit() }),
            )
        })
        .collect();
    json!({
        "sets": catalogue.sets.len(),
        "instruments": catalogue.instruments().count(),
        "size": size_words(all_bytes),
        "by_kind": by_kind,
        "by_licence": by_licence,
        "starter": {
            "what": "the best of each kind that was found; what a user is offered first",
            "size": size_words(starter_bytes),
            "sets": starter.iter().map(|set| json!({ "set": set.id, "name": set.name, "size": size_words(set.bytes) })).collect::<Vec<_>>(),
        },
        "here": here.len(),
        "left_out": catalogue.left_out.len(),
        "next": "Give `query` or `kind` for instruments, `set` for one set with its licence. `put_forward` counts \
                 the instruments nothing that was measured speaks against; the others are listed with \
                 `include_marked`. Nothing here is fetched by a tool: the user fetches a set in Trill's settings.",
    })
}

fn read(catalogue: &Catalogue, here: &HashMap<String, PathBuf>, request: &InstrumentCatalogueRequest) -> Reply {
    if let Some(id) = request.set.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
        return match catalogue.set(id) {
            Some(set) => Ok(set_json(set, here)),
            None => {
                // The sets whose id or name has what was asked for, or a few of them all.
                let asked = id.to_lowercase();
                let mut near: Vec<&str> = catalogue
                    .sets
                    .iter()
                    .filter(|set| set.id.contains(&asked) || set.name.to_lowercase().contains(&asked))
                    .map(|set| set.id.as_str())
                    .collect();
                if near.is_empty() {
                    near = catalogue.sets.iter().take(12).map(|set| set.id.as_str()).collect();
                }
                Err(format!(
                    "there is no set '{id}' in the catalogue; sets are named by their id, as a listing gives it \
                     (`set`). Some there are: {}",
                    near.join(", ")
                ))
            }
        };
    }
    let kind = match request.kind.as_deref().map(str::trim).filter(|kind| !kind.is_empty()) {
        Some(asked) => Some(Kind::of(asked).ok_or_else(|| {
            let kinds: Vec<&str> = Kind::ALL.iter().map(|kind| kind.id()).collect();
            format!("'{asked}' is not a kind of instrument; the kinds are {}", kinds.join(", "))
        })?),
        None => None,
    };
    let words = request.query.clone().unwrap_or_default();
    let asks = !words.trim().is_empty()
        || kind.is_some()
        || request.starter == Some(true)
        || request.public_domain == Some(true)
        || request.here == Some(true)
        || request.include_marked == Some(true);
    if !asks {
        return Ok(overview(catalogue, here));
    }
    let search = Search {
        words,
        kind,
        public_domain_only: request.public_domain == Some(true),
        starter_only: request.starter == Some(true),
        with_faults: request.include_marked == Some(true),
    };
    let found: Vec<(&Set, &Instrument)> = catalogue
        .find(&search)
        .into_iter()
        .filter(|(_, instrument)| request.here != Some(true) || here.contains_key(&instrument.text))
        .collect();
    let (offset, limit) = (request.offset.unwrap_or(0), request.limit.unwrap_or(DEFAULT_LISTED).clamp(1, MAX_LISTED));
    let listed: Vec<Value> =
        found.iter().skip(offset).take(limit).map(|(set, instrument)| instrument_json(set, instrument, here)).collect();
    let mut out = json!({ "found": found.len(), "instruments": listed });
    let shown = offset + out["instruments"].as_array().map_or(0, Vec::len);
    if shown < found.len() {
        out["more"] = json!(format!("{shown} of {} listed; offset {shown} lists the next", found.len()));
    }
    if found.is_empty() {
        // How many there would be with the marked ones, so that nobody takes "none" for "none exist".
        let marked = catalogue.find(&Search { with_faults: true, ..search.clone() }).len();
        out["note"] = json!(if marked > 0 && !search.with_faults {
            format!(
                "Nothing that is put forward. {marked} match that something measured speaks against: \
                 `include_marked` lists them, each with what was found."
            )
        } else {
            "Nothing in the catalogue matches. With no arguments this lists what there is by kind.".to_string()
        });
    }
    Ok(out)
}

#[tool_router(router = catalogue_router, vis = "pub(crate)")]
impl TrillServer {
    /// The sampled instruments others publish that may be used and handed on freely, which Trill can play
    /// (builtin:sampled) once the user has fetched them: with no arguments what there is by kind and by
    /// licence, with `query` or `kind` the instruments (each with what it is, its size, its licence, what was
    /// measured against it, and `here` with its file if it is in the sound library already), with `set` one
    /// set with its licence as its maker wrote it. Reads only; it fetches nothing, and no tool does: the user
    /// fetches a set in Trill's settings.
    #[tool(annotations(title = "Instrument catalogue", read_only_hint = true))]
    fn instrument_catalogue(&self, Parameters(request): Parameters<InstrumentCatalogueRequest>) -> CallToolResult {
        let runtime = self.runtime();
        finish(read(runtime.catalogue(), &runtime.catalogue_here(), &request))
    }

    /// What the open song owes for the sampled instruments it plays: the credit lines of the makers who ask
    /// to be named (ready to paste where the song is released), each with the tracks it is for; the
    /// instruments nothing is owed for; and the instrument files Trill's catalogue does not know, whose terms
    /// are their own. Reads only.
    #[tool(annotations(title = "Song credits", read_only_hint = true))]
    fn song_credits(&self, Parameters(_): Parameters<SongCreditsRequest>) -> CallToolResult {
        let owed = self.runtime().owed();
        let lines: Vec<Value> = owed
            .lines
            .iter()
            .map(|(credit, uses)| {
                json!({
                    "credit": credit.line,
                    "for": uses,
                    "set": credit.set,
                    "licence": credit.licence.id(),
                    "licence_read_at": credit.at,
                })
            })
            .collect();
        let mut out = json!({ "owed": lines, "text": owed.text() });
        if !owed.free.is_empty() {
            out["nothing_owed_for"] = json!(owed.free);
        }
        if !owed.unknown.is_empty() {
            out["not_in_the_catalogue"] = json!(owed.unknown);
            out["not_in_the_catalogue_note"] = json!(
                "Trill's catalogue does not know these instrument files, so it cannot say what they owe: their \
                 own licence does, where they came from."
            );
        }
        out["next"] = json!(match (owed.lines.is_empty(), owed.free.is_empty() && owed.unknown.is_empty()) {
            (false, _) => "Put `text` where the song is released (its description, its notes, its credits).",
            (true, true) => "The song plays no sampled instrument, so nothing is owed for one.",
            (true, false) => "No maker of the catalogue's asks to be named for what this song plays.",
        });
        finish(Ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_read_as_counts_as_a_search_and_as_one_set() {
        let catalogue = Catalogue::shipped();
        let here = HashMap::new();
        let ask = |request: InstrumentCatalogueRequest| read(catalogue, &here, &request);
        let all = ask(InstrumentCatalogueRequest::default()).unwrap();
        assert_eq!(all["sets"], json!(catalogue.sets.len()));
        assert!(all["by_kind"]["piano"]["instruments"].as_u64().unwrap() > 0, "{all}");
        assert!(all["by_licence"]["CC0-1.0"]["sets"].as_u64().unwrap() > 0, "{all}");
        assert!(all["next"].as_str().unwrap().contains("the user fetches a set in Trill's settings"));

        let pianos =
            ask(InstrumentCatalogueRequest { kind: Some("piano".into()), limit: Some(3), ..Default::default() });
        let pianos = pianos.unwrap();
        assert!(pianos["found"].as_u64().unwrap() >= 3 && pianos["instruments"].as_array().unwrap().len() == 3);
        assert!(pianos["more"].as_str().unwrap().contains("offset 3 lists the next"), "{pianos}");
        let first = &pianos["instruments"][0];
        assert_eq!(first["starter"], true, "what is fetched first is listed first: {first}");
        assert!(first.get("measured_against_it").is_none() && first.get("here").is_none(), "{first}");
        let set = first["set"].as_str().unwrap();
        let whole = ask(InstrumentCatalogueRequest { set: Some(set.into()), ..Default::default() }).unwrap();
        assert!(whole["licence"]["says"].as_str().unwrap().len() > 10, "{whole}");
        assert!(whole["licence"]["read_at"].as_str().unwrap().starts_with("https://"));
        assert!(whole["fetching"].as_str().unwrap().contains("This tool fetches nothing"));
        assert!(whole["instruments"].as_array().unwrap().iter().all(|i| i.get("set").is_none()), "said once");

        let error = ask(InstrumentCatalogueRequest { kind: Some("fiddles".into()), ..Default::default() });
        assert!(error.unwrap_err().contains("the kinds are piano, electric-piano"));
        let error = ask(InstrumentCatalogueRequest { set: Some("no-such-set".into()), ..Default::default() });
        assert!(error.unwrap_err().contains("there is no set 'no-such-set' in the catalogue"));
        let none = ask(InstrumentCatalogueRequest { query: Some("zzzzzz qqqq".into()), ..Default::default() }).unwrap();
        assert_eq!(none["found"], 0);
        assert!(none["note"].as_str().unwrap().starts_with("Nothing in the catalogue matches"));
        assert_eq!(
            (size_words(746_586_112), size_words(2_040_109_465), size_words(300_000)),
            ("712 MB".into(), "1.9 GB".into(), "293 KB".into())
        );
    }
}
