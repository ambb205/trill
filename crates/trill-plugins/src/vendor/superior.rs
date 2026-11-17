//! Superior Drummer 3's kits: the factory presets of a sound library.
//!
//! The plugin's state is text, in an old-style VST bank chunk (the plugin
//! is a VST2 at heart, wrapped): a block for each of fourteen places in
//! the kit (`instbox 0` to `13`) that says which drum is in it, its
//! microphones and which keys play it, then the mixer with its effects
//! and the macros. A sound library keeps its factory presets as that same
//! text, all of them in one file, `s3presetconf`, grouped under the names
//! of the menus they are in.
//!
//! So a kit is loaded by putting a preset's drums, mixer and macros in
//! place of the ones in the plugin's state. Everything else in the state
//! (the song track, the browser's settings) stays as it is. Half the
//! presets stack a second drum on the kick or the snare and give it
//! microphones of its own, listed in a block of the preset called
//! `xmics`: that block goes in ahead of the drums.
//!
//! The plugin then reads the kit's samples, one drum after another, on
//! threads of its own: seconds if the files were read lately, most of a
//! minute from a cold disk. Until a drum has arrived its keys play
//! nothing.
//!
//! Which key plays what is the kit's to say, and it does ([`key_sounds`]):
//! each place's block names the drum that is in it, by the library's code
//! for it, and lists under `alias` the keys for each way of striking it.
//! Kits of one library do not agree on them: one hangs its china where
//! another has a crash, moves the toms, or leaves a place empty. What a
//! code or a stroke *is* in words ("20\" china", "Rimshot") is in a file
//! of the library, `uberconf`, beside its presets.

use super::{Built, Mark, VendorPlaces};
use crate::presets::store::PluginRef;
use crate::presets::{Locator, NAME_LIMIT, Sink, Watch, tidy};
use crate::vst3::state::PluginState;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use trill_core::KeySound;
use trill_core::presets::PresetInfo;

/// Superior Drummer 3's processor class: `VST` and the four letters of
/// the VST2 plugin it wraps, `SD3a`, then its name.
pub(crate) const CLASS: &str = "565354534433617375706572696F7220";
/// The file a sound library keeps its factory presets in.
const PRESETS_FILE: &str = "s3presetconf";
/// The file a sound library says in what its places, drums and strokes
/// are called, and the one it says its own id in.
const WORDS_FILE: &str = "uberconf";
const NAME_FILE: &str = "Name";
/// The form of preset this reader knows.
const KNOWN_VERSION: u32 = 5;
/// What begins a bank, and how far into one the size of its chunk is:
/// after the magic word, the bank's size, its kind, version, plugin id and
/// version, program count and 128 bytes kept for later.
const BANK: &[u8; 4] = b"CcnK";
const CHUNK_SIZE_AT: usize = 156;
/// How far into a state a bank may begin: the wrapper puts a short header
/// before it.
const BANK_WITHIN: usize = 256;
/// What the plugin goes on doing once it has a kit.
const LOADING_NOTE: &str = "Superior Drummer reads the kit's samples in the background, one drum after another: \
                            seconds if they were read lately, up to a minute from a cold disk. Until a drum has \
                            arrived its keys are silent. A render and `probe` wait for that.";

/// Where Toontrack's installer puts the sound libraries, each a folder of
/// its own: `Toontrack/Superior/Superior Drummer`, beside the programs.
fn library_folders(places: &VendorPlaces) -> Vec<PathBuf> {
    let folder = |programs: &PathBuf| programs.join("Toontrack").join("Superior").join("Superior Drummer");
    places.programs.iter().map(folder).collect()
}

/// The preset files of the sound libraries that are installed.
fn preset_files(places: &VendorPlaces) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for libraries in library_folders(places) {
        let Ok(entries) = std::fs::read_dir(&libraries) else { continue };
        let mut found: Vec<PathBuf> =
            entries.flatten().map(|entry| entry.path().join(PRESETS_FILE)).filter(|file| file.is_file()).collect();
        found.sort();
        files.extend(found);
    }
    files
}

/// One preset of a file of presets: where it is, and what it is filed
/// under.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Preset {
    name: String,
    /// The menu the plugin's own browser shows it in.
    menu: String,
    /// The bytes of the file it takes up, from its name to the brace that
    /// closes it.
    at: Range<usize>,
}

/// The text between a line's first pair of double quotes.
fn quoted(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once('"')?;
    Some(rest.split_once('"')?.0)
}

/// How many more blocks a line opens than it closes. Braces inside quotes
/// are part of a name.
fn nesting(line: &str) -> i64 {
    let (mut depth, mut inside) = (0, false);
    for c in line.chars() {
        match c {
            '"' => inside = !inside,
            '{' if !inside => depth += 1,
            '}' if !inside => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The lines of a text with where each begins, without the line breaks
/// and the space around them.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split('\n').map(move |line| {
        let start = at;
        at += line.len() + 1;
        (start, line.trim())
    })
}

/// Finds the presets in the text of a file of presets.
fn presets(text: &str) -> Vec<Preset> {
    let mut found = Vec::new();
    let (mut depth, mut menu) = (0i64, String::new());
    let mut open: Option<(String, usize)> = None;
    for (start, line) in lines(text) {
        if depth == 0 {
            if let Some(name) = line.strip_prefix("submenu ").and_then(quoted) {
                menu = name.to_string();
                continue;
            }
            if line.starts_with('"') && line.ends_with('{') {
                open = quoted(line).map(|name| (name.to_string(), start));
            }
        }
        depth += nesting(line);
        if depth <= 0 {
            depth = 0;
            if let Some((name, from)) = open.take() {
                found.push(Preset { name, menu: menu.clone(), at: from..start + line.len() });
            }
        }
    }
    found
}

/// A preset's key in its locator: its place among the file's presets and
/// its name. The place tells apart two of one name; the name finds the
/// preset again when the file has changed.
fn key_of(index: usize, name: &str) -> String {
    format!("{index}:{name}")
}

/// Lists the factory presets of the sound libraries in `places`: each by
/// its name, under the menu the plugin's own browser has it in.
pub(crate) fn list(_plugin: &PluginRef, places: &VendorPlaces, sink: &mut dyn Sink) {
    // A sound library that is installed later brings a file of presets: a
    // library's few files without an extension are what to look at,
    // wherever the libraries are or come to be.
    for libraries in library_folders(places) {
        sink.watch(Watch { path: libraries, extensions: vec![String::new()] });
    }
    for file in preset_files(places) {
        // A library whose file cannot be read has no presets to list.
        sink.presets(file_presets(&file).unwrap_or_default());
    }
}

/// The presets in the file of presets at `file`, wherever that lies:
/// each by its name, under the menu the plugin's own browser has it in.
/// An error says why the file is not one of Superior Drummer's, as the
/// end of a sentence about it.
pub(crate) fn file_presets(file: &Path) -> Result<Vec<PresetInfo>, String> {
    let bytes = super::read_bounded(file).map_err(|reason| format!("it {reason}"))?;
    let text = latin1(&bytes);
    let found = presets(&text);
    if found.is_empty() {
        return Err("it holds no presets of Superior Drummer's".to_string());
    }
    let listed = found.into_iter().enumerate().map(|(index, preset)| PresetInfo {
        name: tidy(&preset.name, NAME_LIMIT),
        category: tidy(&preset.menu, NAME_LIMIT),
        locator: Locator::Vendor {
            format: super::Format::Superior3,
            path: file.to_path_buf(),
            key: Some(key_of(index, &preset.name)),
        }
        .to_text(),
        ..PresetInfo::default()
    });
    Ok(listed.collect())
}

/// Text from bytes that are one character each, as the plugin writes.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// The other way; a character with no byte of its own becomes `?`.
fn to_latin1(text: &str) -> Vec<u8> {
    text.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect()
}

/// A state taken apart: the text of its settings and what is around it.
struct Unwrapped<'a> {
    /// Everything before the bank.
    before: &'a [u8],
    /// The bank's header, between its size and the size of its chunk.
    header: &'a [u8],
    text: String,
    /// What follows the text inside the chunk, from the zero that ends it.
    tail: &'a [u8],
    /// What follows the bank.
    after: &'a [u8],
}

fn unwrap(state: &[u8]) -> Result<Unwrapped<'_>, String> {
    let not_ours = || "the plugin's state is not the text this reader knows".to_string();
    let search = &state[..state.len().min(BANK_WITHIN)];
    let at = search.windows(BANK.len()).position(|window| window == BANK).ok_or_else(not_ours)?;
    let header = state.get(at + 8..at + CHUNK_SIZE_AT).ok_or_else(not_ours)?;
    if &header[..4] != b"FBCh" {
        return Err(not_ours());
    }
    let size = state.get(at + CHUNK_SIZE_AT..at + CHUNK_SIZE_AT + 4).ok_or_else(not_ours)?;
    let size = u32::from_be_bytes(size.try_into().expect("four bytes")) as usize;
    let start = at + CHUNK_SIZE_AT + 4;
    let chunk = state.get(start..start.checked_add(size).ok_or_else(not_ours)?).ok_or_else(not_ours)?;
    let end = chunk.iter().position(|byte| *byte == 0).unwrap_or(chunk.len());
    Ok(Unwrapped {
        before: &state[..at],
        header,
        text: latin1(&chunk[..end]),
        tail: &chunk[end..],
        after: &state[start + size..],
    })
}

impl Unwrapped<'_> {
    /// The state with `text` in place of its settings: the chunk and the
    /// bank say how long they are, big end first.
    fn wrap(&self, text: &str) -> Vec<u8> {
        let mut chunk = to_latin1(text);
        chunk.extend(self.tail);
        let mut out = Vec::with_capacity(self.before.len() + CHUNK_SIZE_AT + 4 + chunk.len() + self.after.len());
        out.extend(self.before);
        out.extend(BANK);
        out.extend(((self.header.len() + 4 + chunk.len()) as u32).to_be_bytes());
        out.extend(self.header);
        out.extend((chunk.len() as u32).to_be_bytes());
        out.extend(chunk);
        out.extend(self.after);
        out
    }
}

/// The lines of a kit that say which drum is in each place, in order:
/// what a state shows of the kit it holds.
fn drums<'a>(kit: impl Iterator<Item = &'a str>) -> Vec<String> {
    kit.filter(|line| line.starts_with("drum "))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// Where in a state's lines the kit is: from the first drum's block to
/// the line that begins what follows the macros.
fn kit_of(state: &[&str]) -> Option<Range<usize>> {
    let first = state.iter().position(|line| line.starts_with("instbox 0"))?;
    let end = state.iter().position(|line| line.starts_with("midilearn"))?;
    (first < end).then_some(first..end)
}

/// The sound library a text says it is of: `soundlib "X"` in a state,
/// `EZX "X"` in a preset.
fn library<'a>(mut text: impl Iterator<Item = &'a str>, word: &str) -> Option<&'a str> {
    text.find_map(|line| line.strip_prefix(word).and_then(quoted))
}

/// The state that makes the plugin the kit `key` names in the file of
/// presets `file`: `current`, the plugin's state now, with the preset's
/// drums, mixer and macros in place of its own.
pub(crate) fn build(file: &[u8], key: Option<&str>, current: &PluginState) -> Result<Built, String> {
    let unwrapped = unwrap(&current.component)?;
    let text = latin1(file);
    let found = presets(&text);
    let (index, name) = key.and_then(|key| key.split_once(':')).ok_or("it does not say which kit of the file it is")?;
    let preset = index
        .parse::<usize>()
        .ok()
        .and_then(|index| found.get(index))
        .filter(|preset| preset.name == name)
        // The file has changed since it was listed: by name, then.
        .or_else(|| found.iter().find(|preset| preset.name == name))
        .ok_or_else(|| format!("the file no longer has a kit called '{name}'"))?;
    let kit: Vec<&str> = lines(&text[preset.at.clone()]).map(|(_, line)| line).collect();

    let version = kit
        .iter()
        .find_map(|line| line.strip_prefix("version "))
        .and_then(|version| version.trim().parse::<u32>().ok());
    if version.is_some_and(|version| version > KNOWN_VERSION) {
        return Err(format!(
            "the kit '{name}' is written in a newer form (version {}) than this reader knows ({KNOWN_VERSION})",
            version.unwrap_or_default()
        ));
    }
    let state: Vec<&str> = unwrapped.text.split('\n').collect();
    let ours = kit_of(&state).ok_or("the plugin's state has no kit in it to replace")?;
    let (theirs, loaded) = (library(kit.iter().copied(), "EZX "), library(state.iter().copied(), "soundlib "));
    if let (Some(theirs), Some(loaded)) = (theirs, loaded)
        && theirs != loaded
    {
        return Err(format!(
            "the kit '{name}' is of the sound library '{theirs}', and the plugin has '{loaded}' loaded"
        ));
    }

    // The preset's drums, mixer and macros: from its first drum to the
    // line before the brace that closes it.
    let first = kit.iter().position(|line| line.starts_with("instbox 0"));
    let first =
        first.filter(|first| *first + 1 < kit.len()).ok_or_else(|| format!("the kit '{name}' has no drums in it"))?;
    let body = &kit[first..kit.len() - 1];
    // The microphones of a stacked drum, which the preset lists before
    // its drums.
    let stacked = kit.iter().position(|line| *line == "xmics {");
    let extra = stacked.and_then(|from| {
        let to = kit.iter().position(|line| line.starts_with("kit {")).filter(|to| *to > from)?;
        Some(&kit[from..to])
    });

    let mut lines: Vec<&str> = state[..ours.start].to_vec();
    lines.extend(extra.unwrap_or_default());
    lines.extend(body);
    lines.extend(&state[ours.end..]);
    let marks = drums(body.iter().copied())
        .into_iter()
        .enumerate()
        .map(|(index, drum)| Mark { name: format!("drum {index}"), value: drum })
        .collect();
    let component = unwrapped.wrap(&lines.join("\n"));
    Ok(Built {
        state: PluginState { component, controller: current.controller.clone() },
        marks,
        note: Some(LOADING_NOTE.to_string()),
    })
}

/// What `state` shows for the mark `name`: the drum in the place the
/// mark is for.
pub(crate) fn shown(state: &PluginState, name: &str) -> Option<String> {
    let index: usize = name.strip_prefix("drum ")?.parse().ok()?;
    let unwrapped = unwrap(&state.component).ok()?;
    let lines: Vec<&str> = unwrapped.text.split('\n').map(str::trim).collect();
    let kit = kit_of(&lines)?;
    drums(lines[kit].iter().copied()).into_iter().nth(index)
}

/// What a sound library calls its places, drums and strokes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Words {
    /// By the place's code (`Cymbal6`).
    places: HashMap<String, PlaceWords>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PlaceWords {
    /// "Cymbal 5".
    description: String,
    /// What each drum that can be in the place is, by its code.
    drums: HashMap<String, String>,
    /// What each way of striking it is, by the name its keys are listed
    /// under.
    strokes: HashMap<String, String>,
}

/// The words of a line: split at spaces, with what is between double
/// quotes kept whole.
fn words(line: &str) -> Vec<String> {
    let (mut found, mut word, mut inside, mut any) = (Vec::new(), String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => {
                inside = !inside;
                any = true;
            }
            c if c.is_whitespace() && !inside => {
                if any {
                    found.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        found.push(word);
    }
    found
}

/// Reads a library's file of words. The file lists, for each place, a
/// description, the drums that can be in it (`code id "what it is"`) and
/// under `pads` the strokes (`name id description`); several places share
/// a list of strokes through a template, `USE name 3`, in which `$1` is
/// the number. Whatever else it holds is passed over, and nothing in it is
/// trusted to end: a line is read by itself.
fn parse_words(text: &str) -> Words {
    let lines: Vec<&str> =
        text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect();
    // The templates first, wherever they are.
    let mut templates: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut open: Option<(&str, Vec<&str>)> = None;
    let mut body: Vec<&str> = Vec::new();
    for line in &lines {
        match (&mut open, line.strip_prefix("TEMPLATE ")) {
            (None, Some(name)) => open = Some((name.trim(), Vec::new())),
            (Some(_), _) if *line == "END" => {
                let (name, held) = open.take().expect("a template is open");
                templates.insert(name, held);
            }
            (Some((_, held)), _) => held.push(line),
            (None, None) => body.push(line),
        }
    }

    let mut found = Words::default();
    // A place is open from `Code NN {` to the brace that closes it.
    let (mut within, mut depth, mut place_at) = (false, 0i64, 0i64);
    let mut place: Option<(String, PlaceWords)> = None;
    let mut strokes_at: Option<i64> = None;
    let mut queue: std::collections::VecDeque<String> = body.iter().map(|line| (*line).to_string()).collect();
    // No more lines than the file and its templates can make.
    let mut budget = 64 * lines.len() + 1024;
    while let Some(line) = queue.pop_front() {
        budget -= 1;
        if budget == 0 {
            break;
        }
        let said = words(&line);
        let opens = nesting(&line);
        if !within {
            within = said.first().is_some_and(|word| word == "positions") && opens > 0;
            depth = if within { 1 } else { 0 };
            continue;
        }
        match &mut place {
            None => {
                if said.len() == 3 && said[2] == "{" && depth == 1 {
                    place = Some((said[0].clone(), PlaceWords::default()));
                    place_at = depth;
                }
            }
            Some((_, held)) => {
                if let Some(at) = strokes_at {
                    if depth + opens <= at {
                        strokes_at = None;
                    } else if said.len() >= 3 {
                        held.strokes.insert(said[0].clone(), said[2].clone());
                    }
                } else if said.first().is_some_and(|word| word == "pads") && opens > 0 {
                    strokes_at = Some(depth);
                } else if said.first().is_some_and(|word| word == "description") && said.len() >= 2 {
                    held.description = said[1].clone();
                } else if said.first().is_some_and(|word| word == "USE") && said.len() >= 2 {
                    // A template's lines take this line's place, with its number in them.
                    let number = said.get(2).map(String::as_str).unwrap_or_default();
                    for line in templates.get(said[1].as_str()).into_iter().flatten().rev() {
                        queue.push_front(line.replace("$1", number));
                    }
                    continue;
                } else if said.len() == 3 && opens == 0 && line.trim_end().ends_with('"') {
                    held.drums.insert(said[0].clone(), said[2].replace('^', "\""));
                }
            }
        }
        depth += opens;
        if place.is_some() && depth <= place_at {
            let (code, held) = place.take().expect("a place is open");
            found.places.insert(code, held);
            strokes_at = None;
        }
        if depth <= 0 {
            within = false;
        }
    }
    found
}

/// The words of the sound library that calls itself `id`, among the
/// libraries installed in `places`: read once, and `None` for a library
/// that is not there or says nothing that can be read.
fn library_words(id: &str, places: &VendorPlaces) -> Option<Arc<Words>> {
    type Read = HashMap<(String, Vec<PathBuf>), Option<Arc<Words>>>;
    static READ: OnceLock<Mutex<Read>> = OnceLock::new();
    let key = (id.to_string(), places.programs.clone());
    let mut read = READ.get_or_init(Mutex::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(known) = read.get(&key) {
        return known.clone();
    }
    // A file Trill did not write: no more of it is read than a file of
    // words can be, and a reader that gives up on it costs the names only.
    let found = std::panic::catch_unwind(|| {
        let folder = library_folders(places).into_iter().find_map(|libraries| {
            let entries = std::fs::read_dir(&libraries).ok()?;
            entries.flatten().map(|entry| entry.path()).find(|folder| {
                let name = super::read_start(&folder.join(NAME_FILE), 4096).unwrap_or_default();
                let first = latin1(&name);
                first.lines().next().and_then(|line| words(line).pop()).is_some_and(|said| said == id)
            })
        })?;
        let bytes = super::read_start(&folder.join(WORDS_FILE), 4 * 1024 * 1024).ok()?;
        Some(Arc::new(parse_words(&latin1(&bytes))))
    })
    .unwrap_or(None);
    read.insert(key, found.clone());
    found
}

/// What the keys of the kit in `state` play: for every key that strikes a
/// drum the kit has, the place, what is in it and the stroke, in the
/// library's own words where the library is installed in `places` and in
/// its codes where not. A place the kit leaves empty has no keys. `None`
/// for a state that is not the text this reader knows.
pub(crate) fn key_sounds(state: &PluginState, places: &VendorPlaces) -> Option<Vec<KeySound>> {
    let unwrapped = unwrap(&state.component).ok()?;
    let lines: Vec<&str> = unwrapped.text.split('\n').map(str::trim).collect();
    let kit = kit_of(&lines)?;
    let loaded = library(lines.iter().copied(), "soundlib ");
    let mut found: Vec<KeySound> = Vec::new();
    // The library, the place and the drum of the pad the lines are in.
    let (mut of, mut place, mut drum): (Option<String>, String, Option<String>) = (None, String::new(), None);
    for line in &lines[kit] {
        let said = words(line);
        match said.first().map(String::as_str) {
            Some("xpad") => {
                of = said.get(2).filter(|word| *word != "{").cloned().or_else(|| loaded.map(str::to_string));
                (place, drum) = (String::new(), None);
            }
            Some("pos") => place = said.get(1).cloned().unwrap_or_default(),
            // A dash is a place with nothing in it.
            Some("drum") => drum = said.get(1).filter(|code| *code != "-" && !code.is_empty()).cloned(),
            Some("alias") => {
                let (Some(code), Some(stroke)) = (&drum, said.get(1)) else { continue };
                let library = of.as_deref().and_then(|id| library_words(id, places));
                let named = library.as_ref().and_then(|library| library.places.get(&place));
                let sound = KeySound {
                    key: 0,
                    place: named
                        .map(|named| named.description.clone())
                        .filter(|description| !description.is_empty())
                        .unwrap_or_else(|| place.clone()),
                    instrument: named.and_then(|named| named.drums.get(code)).cloned().unwrap_or_default(),
                    stroke: named.and_then(|named| named.strokes.get(stroke)).unwrap_or(stroke).clone(),
                };
                // A key is a note; the triggers and controllers listed with them are not.
                for key in said[2..].iter().filter_map(|word| word.parse::<u8>().ok()).filter(|key| *key <= 127) {
                    found.push(KeySound { key, ..sound.clone() });
                }
            }
            _ => {}
        }
    }
    Some(found)
}

/// A file of presets made here, in the format: each of `kits` a name, the
/// menu it is in, and the drum in its one place.
#[cfg(test)]
pub(crate) fn presets_file(kits: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut text = String::new();
    let mut menu = "";
    for (name, under, drum) in kits {
        if *under != menu {
            text.push_str(&format!("submenu \"{under}\"\n"));
            menu = under;
        }
        let stacked = if drum.ends_with('+') { "xmics {\nmic {\nname \"X-Kick-In\"\n}\n}\n" } else { "" };
        text.push_str(&format!(
            "\"{name}\" {{\nversion 5\nEZX \"SL-TEST\"\n{stacked}kit {{\ns3kit\n\"KickR\" \"SL-TEST\" \"KickR\" \"{drum}\" \"Felt\" 1\n}}\n\
             instbox 0 \"Kick\" {{\nxpad 0 SL-TEST {{\npos \"KickR\"\ndrum \"{drum}\"  \"Felt\" 1\nalias kick 36\n}}\n}}\n\
             mixer {{\nKick-In {{\nvolume 0.5\n}}\n}}\nmacros {{\n}}\n}}\n"
        ));
    }
    text.into_bytes()
}

/// A state of the plugin made here: a kit of one drum, `drum`, in the
/// bank chunk and the wrapper's header the plugin's own state has.
#[cfg(test)]
pub(crate) fn state(drum: &str) -> PluginState {
    let text = format!(
        "PlugVersion 3.4.4\nSAMPLER {{\nHQ {{\nsoundlib \"SL-TEST\"\n}}\ninstbox 0 {{\nxpad 0 SL-TEST {{\npos \"KickR\"\n\
         drum \"{drum}\"  \"Felt\" 1\n}}\n}}\nmixer {{\nlatcomp\n}}\nmacros {{\n}}\nmidilearn {{\n}}\n}}\nSongTrack 0 {{\nTempo 120 0 0\n}}\n"
    );
    let mut chunk = text.into_bytes();
    chunk.extend(b"\0\0JUCEPrivateData\0");
    let mut bank = b"FBCh\0\0\0\x02SD3a\0\x03\x04\x04\0\0\0\0".to_vec();
    bank.extend([0u8; 128]);
    bank.extend((chunk.len() as u32).to_be_bytes());
    bank.extend(&chunk);
    let mut component = b"VstW\0\0\0\x08\0\0\0\x01\0\0\0\0CcnK".to_vec();
    component.extend((bank.len() as u32).to_be_bytes());
    component.extend(bank);
    PluginState { component, controller: Vec::new() }
}

/// A state of the plugin made here, of the library `library`, whose kit is
/// `places`: each a place's code, the drum in it (`-` for none) and the
/// keys of its strokes as the plugin lists them.
#[cfg(test)]
pub(crate) fn kit_state(library: &str, places: &[(&str, &str, &[&str])]) -> PluginState {
    let mut text = format!("PlugVersion 3.4.4\nSAMPLER {{\nHQ {{\nsoundlib \"{library}\"\n}}\n");
    for (index, (place, drum, strokes)) in places.iter().enumerate() {
        let drum = if *drum == "-" { "-".to_string() } else { format!("\"{drum}\"") };
        text.push_str(&format!(
            "instbox {index} {{\nxpad 0 {library} {{\npos \"{place}\"\nmastermics {{\nnrmaster 0\n}}\ndrum {drum}  \"Drumsticks\" 1\n\
             pads {{\nfader {{\nvolume 1\n}}\n"
        ));
        for stroke in *strokes {
            text.push_str(&format!("alias {stroke}\n"));
        }
        text.push_str("}\n}\n}\n");
    }
    text.push_str("mixer {\nlatcomp\n}\nmacros {\n}\nmidilearn {\n}\n}\nSongTrack 0 {\nTempo 120 0 0\n}\n");
    let unwrapped = state("Kick1");
    let wrapper = unwrap(&unwrapped.component).expect("the state made here is one");
    PluginState { component: wrapper.wrap(&text), controller: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Found;
    use crate::test_support::TestDir;

    /// A library's file of words made here, in the format: two places with
    /// strokes of their own and two cymbals that share a template.
    const WORDS: &str = "# words \"Test library\"\n\nproducts {\n\t\"TEST\"\n}\n\npositions {\n\n\tKickR 01 {\n\t\tdescription \"Kick\"\n\
        \t\tkicktool\n\t\tclose\tKick-In\n\n\t\tKick1\tKD01\t\"16x24^ Test Maple\"\n\t\tKick2\tKD02\t\"18x20^ Test Birch\"\n\n\
        \t\tpads {\n\t\t\tkickO\t\tO1_R Open\n\t\t\tkickR\t\tFH_R Hit\t\n\t\t}\n\t}\n\n\tSnare 01 {\n\t\tdescription \"Snare\"\n\n\
        \t\tSnare3\t\tSD03\t\"6.5x14^ Test Brass\"\n\n\t\tpads {\n\t\t\tsnareR\tFH_R Center\n\t\t\tsnareFX\tFX_R Rimshot\n\
        \t\t\tsnareRO\tRO_R \"Rim Only\"\n\t\t\tsnareRF\tRF_R \"Closed Roll\"\tlonefader\n\t\t\tsnareTrig\tnone \"Zone Trigger\"\n\t\t}\n\t}\n\n\
        TEMPLATE StdCrash\n\t\tclose Oh\n\n\t\tpads {\n\t\t\tcrash$1\t\tCR_R\tCrash\n\t\t\tcrash$1BL\tBL_R\t\"Bell Shank\"\n\
        \t\t\tcrash$1M\tMT_R\t\"Mute Hit\"\t\tlonevelvol\n\t\t}\nEND\n\n\tCymbal1 01 {\n\t\tdescription \"Cymbal 1\"\n\t\tUSE StdCrash 1\n\n\
        \t\tcymbal2_1\tCR02\t\"18^ Test Crash\"\n\t\tcymbal3_1\tCR03\t\"20^ Test China\"\n\t}\n\n\tCymbal6 06 {\n\t\tdescription \"Cymbal 5\"\n\
        \t\tUSE StdCrash 6\n\n\t\tcymbal1_6\tCR03\t\"20^ Other China\"\n\t}\n}\n\ndrumsets {\n\tKit1 {\n\t\tKickR\tKick1\n\t}\n}\n";

    #[test]
    fn a_librarys_words_say_what_its_places_drums_and_strokes_are() {
        let found = parse_words(WORDS);
        let mut places: Vec<&String> = found.places.keys().collect();
        places.sort();
        assert_eq!(places, ["Cymbal1", "Cymbal6", "KickR", "Snare"]);
        let kick = &found.places["KickR"];
        assert_eq!(kick.description, "Kick");
        assert_eq!(kick.drums["Kick2"], "18x20\" Test Birch", "inches are written as the mark for them");
        assert_eq!((kick.strokes["kickO"].as_str(), kick.strokes["kickR"].as_str()), ("Open", "Hit"));
        assert_eq!(kick.drums.len(), 2, "what else a place says is no drum: {kick:?}");
        let snare = &found.places["Snare"];
        assert_eq!(snare.strokes["snareRO"], "Rim Only");
        assert_eq!(snare.strokes["snareRF"], "Closed Roll", "what follows a description is not part of it");
        // A template's strokes are each place's own, by its number.
        let (first, last) = (&found.places["Cymbal1"], &found.places["Cymbal6"]);
        assert_eq!((first.strokes["crash1"].as_str(), first.strokes["crash1M"].as_str()), ("Crash", "Mute Hit"));
        assert_eq!(last.strokes["crash6BL"], "Bell Shank");
        assert!(!last.strokes.contains_key("crash1") && last.drums["cymbal1_6"] == "20\" Other China");
        assert_eq!(last.description, "Cymbal 5", "a place is called what it says, not what its code says");

        // Text that is something else, or is cut off, says what it can and never hangs.
        assert_eq!(parse_words("not a file of words\n{{{{\n"), Words::default());
        assert!(parse_words(&WORDS[..WORDS.len() / 2]).places.contains_key("KickR"));
        let endless = "positions {\nA 01 {\nUSE Loop 1\n}\n}\nTEMPLATE Loop\nUSE Loop 1\nEND\n";
        assert!(parse_words(endless).places.len() <= 1, "a template that uses itself is given up on");
        assert_eq!(words("alias \"two words\"  7 \"\" x"), ["alias", "two words", "7", "", "x"]);
    }

    #[test]
    fn a_kit_says_which_key_strikes_which_drum_how() {
        // The library is installed where libraries are, under a folder name of its own.
        let dir = TestDir::new();
        dir.file("programs/Toontrack/Superior/Superior Drummer/SL-Other/Name", b"\"OTHER\" SL-OTHER\n");
        dir.file(
            "programs/Toontrack/Superior/Superior Drummer/SL-TestLib/Name",
            b"\"TEST LIBRARY\" SL-TEST\r\n\"Description\" Doc.pdf\r\n",
        );
        dir.file("programs/Toontrack/Superior/Superior Drummer/SL-TestLib/uberconf", WORDS.as_bytes());
        let places = VendorPlaces { programs: vec![dir.path().join("programs")], ..VendorPlaces::default() };
        // A kit with its china in the first cymbal's place, no snare, and a second cymbal on two keys.
        let state = kit_state(
            "SL-TEST",
            &[
                ("KickR", "Kick2", &["kickO 36 34", "kickR 35"]),
                ("Snare", "-", &["snareR 38", "snareFX 40"]),
                ("Cymbal1", "cymbal3_1", &["crash1 86", "crash1M 83", "crash1Trig 144 CC16"]),
                ("Cymbal6", "cymbal1_6", &["crash6 52 110", "crash6BL 109"]),
            ],
        );
        let found = key_sounds(&state, &places).unwrap();
        let said = |key: u8| {
            let sound =
                found.iter().find(|sound| sound.key == key).unwrap_or_else(|| panic!("no key {key}: {found:?}"));
            format!("{} / {} / {}", sound.place, sound.instrument, sound.stroke)
        };
        assert_eq!(said(36), "Kick / 18x20\" Test Birch / Open");
        assert_eq!(said(34), "Kick / 18x20\" Test Birch / Open");
        assert_eq!(said(35), "Kick / 18x20\" Test Birch / Hit");
        assert_eq!(said(86), "Cymbal 1 / 20\" Test China / Crash");
        assert_eq!(said(83), "Cymbal 1 / 20\" Test China / Mute Hit");
        assert_eq!(said(52), "Cymbal 5 / 20\" Other China / Crash");
        assert_eq!(said(109), "Cymbal 5 / 20\" Other China / Bell Shank");
        let keys: Vec<u8> = found.iter().map(|sound| sound.key).collect();
        assert_eq!(keys, [36, 34, 35, 86, 83, 52, 110, 109], "an empty place has no keys, and a trigger is no key");

        // Without the library's words a kit still says its keys, in the library's codes.
        let bare = key_sounds(&state, &VendorPlaces::default()).unwrap();
        let china = bare.iter().find(|sound| sound.key == 86).unwrap();
        assert_eq!((china.place.as_str(), china.instrument.as_str(), china.stroke.as_str()), ("Cymbal1", "", "crash1"));
        // A state that is something else says nothing.
        let opaque = PluginState { component: b"opaque".to_vec(), controller: Vec::new() };
        assert_eq!(key_sounds(&opaque, &places), None);
    }

    const KITS: [(&str, &str, &str); 4] = [
        ("Ayotte Default", "Clean Kits", "Kick2"),
        ("Gretsch {Dry}", "Clean Kits", "Kick7"),
        ("Death Metal", "Metal/Heavy Rock", "Kick62+"),
        ("Ayotte Default", "Roomy", "Kick9"),
    ];

    fn plugin() -> PluginRef {
        PluginRef {
            uri: format!("vst3:{CLASS}"),
            id: CLASS.into(),
            path: PathBuf::from("Superior Drummer 3.vst3"),
            name: "Superior Drummer 3".into(),
            vendor: "Toontrack".into(),
        }
    }

    #[test]
    fn a_librarys_presets_are_listed_under_the_menus_they_are_in() {
        let dir = TestDir::new();
        let file = dir.file("programs/Toontrack/Superior/Superior Drummer/SL-Test/s3presetconf", &presets_file(&KITS));
        dir.file("programs/Toontrack/Superior/Superior Drummer/SL-Empty/readme.txt", b"no presets here");
        let places = VendorPlaces {
            programs: vec![dir.path().join("programs"), dir.path().join("no such folder")],
            ..VendorPlaces::default()
        };
        let mut found = Found::default();
        list(&plugin(), &places, &mut found);
        let listed: Vec<String> = found.presets.iter().map(|preset| preset.path()).collect();
        assert_eq!(
            listed,
            [
                "Clean Kits/Ayotte Default",
                "Clean Kits/Gretsch {Dry}",
                "Metal/Heavy Rock/Death Metal",
                "Roomy/Ayotte Default"
            ]
        );
        // Two kits of one name are two presets.
        let locators: Vec<Locator> =
            found.presets.iter().map(|preset| Locator::parse(&preset.locator).unwrap()).collect();
        let key = |index: usize| match &locators[index] {
            Locator::Vendor { format: super::super::Format::Superior3, path, key } if *path == file => {
                key.clone().unwrap()
            }
            other => panic!("{other:?}"),
        };
        assert_eq!((key(0), key(3)), ("0:Ayotte Default".to_string(), "3:Ayotte Default".to_string()));
        // The folders of sound libraries are what is watched, the one
        // that is not there too: the files in them that have no extension,
        // which a library's presets are.
        let libraries = |programs: &str| dir.path().join(programs).join("Toontrack/Superior/Superior Drummer");
        let watched = |programs: &str| Watch { path: libraries(programs), extensions: vec![String::new()] };
        assert_eq!(found.watch, [watched("programs"), watched("no such folder")]);
        let before = crate::presets::walk::fingerprint(&found.watch);
        dir.file("programs/Toontrack/Superior/Superior Drummer/SL-Empty/notes.txt", b"not presets");
        assert_eq!(crate::presets::walk::fingerprint(&found.watch), before);
        dir.file("programs/Toontrack/Superior/Superior Drummer/SL-New/s3presetconf", &presets_file(&KITS[..1]));
        assert_ne!(crate::presets::walk::fingerprint(&found.watch), before, "a new library's presets are noticed");
    }

    #[test]
    fn a_kit_takes_the_place_of_the_one_in_the_plugins_state() {
        let current = state("Kick1");
        let file = presets_file(&KITS);
        let built = build(&file, Some("1:Gretsch {Dry}"), &current).unwrap();
        // The state is the one it was but for the drums, the mixer and the
        // macros: its sizes, the wrapper's header and what follows the
        // text are in place.
        let unwrapped = unwrap(&built.state.component).unwrap();
        let text = unwrapped.text.clone();
        assert!(
            text.starts_with("PlugVersion 3.4.4\nSAMPLER {\nHQ {\nsoundlib \"SL-TEST\"\n}\ninstbox 0 \"Kick\" {\n"),
            "{text}"
        );
        assert!(text.contains("drum \"Kick7\"  \"Felt\" 1\nalias kick 36\n") && !text.contains("Kick1"), "{text}");
        assert!(text.contains("mixer {\nKick-In {\nvolume 0.5\n}\n}\nmacros {\n}\nmidilearn {\n}\n}\nSongTrack 0 {\nTempo 120 0 0\n}\n"), "{text}");
        assert!(!text.contains("latcomp") && !text.contains("xmics"), "{text}");
        assert_eq!(unwrapped.tail, b"\0\0JUCEPrivateData\0");
        assert!(built.state.component.starts_with(b"VstW\0\0\0\x08\0\0\0\x01\0\0\0\0CcnK"));
        let bank = u32::from_be_bytes(built.state.component[20..24].try_into().unwrap()) as usize;
        assert_eq!(bank, built.state.component.len() - 24, "the bank says how long it is");

        assert_eq!(built.marks, [Mark { name: "drum 0".into(), value: "drum \"Kick7\" \"Felt\" 1".into() }]);
        assert_eq!(shown(&built.state, "drum 0").as_deref(), Some("drum \"Kick7\" \"Felt\" 1"));
        assert_eq!(shown(&current, "drum 0").as_deref(), Some("drum \"Kick1\" \"Felt\" 1"));
        assert_eq!(shown(&built.state, "drum 1"), None);
        assert!(built.note.unwrap().contains("reads the kit's samples in the background"));

        // A stacked drum's microphones go in ahead of the drums.
        let stacked = build(&file, Some("2:Death Metal"), &current).unwrap();
        let text = unwrap(&stacked.state.component).unwrap().text;
        assert!(
            text.contains("soundlib \"SL-TEST\"\n}\nxmics {\nmic {\nname \"X-Kick-In\"\n}\n}\ninstbox 0 \"Kick\" {\n"),
            "{text}"
        );
        // The second of two kits of one name, and a kit found by its name
        // after the file has changed under it.
        let second = build(&file, Some("3:Ayotte Default"), &current).unwrap();
        assert_eq!(shown(&second.state, "drum 0").as_deref(), Some("drum \"Kick9\" \"Felt\" 1"));
        let moved = build(&file, Some("7:Death Metal"), &current).unwrap();
        assert_eq!(shown(&moved.state, "drum 0").as_deref(), Some("drum \"Kick62+\" \"Felt\" 1"));
    }

    #[test]
    fn a_kit_or_a_state_that_will_not_do_says_why() {
        let current = state("Kick1");
        let file = presets_file(&KITS);
        let wrong = |file: &[u8], key: Option<&str>, state: &PluginState| build(file, key, state).unwrap_err();
        assert_eq!(wrong(&file, Some("0:No Such Kit"), &current), "the file no longer has a kit called 'No Such Kit'");
        assert_eq!(wrong(&file, None, &current), "it does not say which kit of the file it is");
        assert_eq!(
            wrong(b"not presets", Some("0:Ayotte Default"), &current),
            "the file no longer has a kit called 'Ayotte Default'"
        );
        let newer = String::from_utf8(file.clone()).unwrap().replace("version 5", "version 9");
        assert_eq!(
            wrong(newer.as_bytes(), Some("0:Ayotte Default"), &current),
            "the kit 'Ayotte Default' is written in a newer form (version 9) than this reader knows (5)"
        );
        let other = String::from_utf8(file.clone()).unwrap().replace("EZX \"SL-TEST\"", "EZX \"SL-OTHER\"");
        assert_eq!(
            wrong(other.as_bytes(), Some("0:Ayotte Default"), &current),
            "the kit 'Ayotte Default' is of the sound library 'SL-OTHER', and the plugin has 'SL-TEST' loaded"
        );
        let hollow = b"\"Hollow\" {\nversion 5\n}\n";
        assert_eq!(wrong(hollow, Some("0:Hollow"), &current), "the kit 'Hollow' has no drums in it");

        // States that are something else: no bank, a bank cut short, a
        // chunk that claims more than there is, and text without a kit.
        let opaque = PluginState { component: b"opaque".to_vec(), controller: Vec::new() };
        let not_ours = "the plugin's state is not the text this reader knows";
        assert_eq!(wrong(&file, Some("0:Ayotte Default"), &opaque), not_ours);
        let mut cut = current.clone();
        cut.component.truncate(60);
        assert_eq!(wrong(&file, Some("0:Ayotte Default"), &cut), not_ours);
        let mut lying = current.clone();
        lying.component[16 + CHUNK_SIZE_AT..16 + CHUNK_SIZE_AT + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(wrong(&file, Some("0:Ayotte Default"), &lying), not_ours);
        let unwrapped = unwrap(&current.component).unwrap();
        let empty = PluginState { component: unwrapped.wrap("PlugVersion 3.4.4\n"), controller: Vec::new() };
        assert_eq!(wrong(&file, Some("0:Ayotte Default"), &empty), "the plugin's state has no kit in it to replace");
        assert_eq!(shown(&opaque, "drum 0"), None);
    }

    #[test]
    fn braces_in_names_do_not_open_blocks() {
        assert_eq!(
            (nesting("instbox 0 \"Kick\" {"), nesting("}"), nesting("\"Gretsch {Dry}\" {"), nesting("name \"}}\"")),
            (1, -1, 1, 0)
        );
        assert_eq!(quoted("submenu \"Clean Kits\""), Some("Clean Kits"));
        assert_eq!(quoted("no quotes"), None);
        let text = String::from_utf8(presets_file(&KITS)).unwrap();
        let found = presets(&text);
        assert_eq!(found.len(), 4);
        assert!(
            text[found[1].at.clone()].starts_with("\"Gretsch {Dry}\" {\nversion 5\n")
                && text[found[1].at.clone()].ends_with("macros {\n}\n}")
        );
        // A file that is cut off inside a kit has the kits before it.
        assert_eq!(presets(&text[..found[2].at.start + 40]).len(), 2);
    }
}
