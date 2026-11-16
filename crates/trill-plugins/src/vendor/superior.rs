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

use super::{Built, Mark, VendorPlaces, read_whole};
use crate::presets::store::PluginRef;
use crate::presets::{Locator, NAME_LIMIT, Sink, Watch, tidy};
use crate::vst3::state::PluginState;
use std::ops::Range;
use std::path::PathBuf;
use trill_core::presets::PresetInfo;

/// Superior Drummer 3's processor class: `VST` and the four letters of
/// the VST2 plugin it wraps, `SD3a`, then its name.
pub(crate) const CLASS: &str = "565354534433617375706572696F7220";
/// The file a sound library keeps its factory presets in.
const PRESETS_FILE: &str = "s3presetconf";
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

/// The preset files of the sound libraries that are installed where
/// Toontrack's installer puts them: each library a folder of its own
/// under `Toontrack/Superior/Superior Drummer`, beside the programs.
fn preset_files(places: &VendorPlaces) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for programs in &places.programs {
        let libraries = programs.join("Toontrack").join("Superior").join("Superior Drummer");
        let Ok(entries) = std::fs::read_dir(&libraries) else { continue };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path().join(PRESETS_FILE))
            .filter(|file| file.is_file())
            .collect();
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
    for file in preset_files(places) {
        // The one file is what to look at for whether its presets changed.
        sink.watch(Watch { path: file.clone(), extensions: Vec::new() });
        let Ok(bytes) = read_whole(&file) else { continue };
        let text = latin1(&bytes);
        let listed = presets(&text).into_iter().enumerate().map(|(index, preset)| PresetInfo {
            name: tidy(&preset.name, NAME_LIMIT),
            category: tidy(&preset.menu, NAME_LIMIT),
            locator: Locator::Vendor {
                format: super::Format::Superior3,
                path: file.clone(),
                key: Some(key_of(index, &preset.name)),
            }
            .to_text(),
            ..PresetInfo::default()
        });
        sink.presets(listed.collect());
    }
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

    let version = kit.iter().find_map(|line| line.strip_prefix("version ")).and_then(|version| version.trim().parse::<u32>().ok());
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
    let first = first.filter(|first| *first + 1 < kit.len()).ok_or_else(|| format!("the kit '{name}' has no drums in it"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Found;
    use crate::test_support::TestDir;

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
            ["Clean Kits/Ayotte Default", "Clean Kits/Gretsch {Dry}", "Metal/Heavy Rock/Death Metal", "Roomy/Ayotte Default"]
        );
        // Two kits of one name are two presets.
        let locators: Vec<Locator> = found.presets.iter().map(|preset| Locator::parse(&preset.locator).unwrap()).collect();
        let key = |index: usize| match &locators[index] {
            Locator::Vendor { format: super::super::Format::Superior3, path, key } if *path == file => key.clone().unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!((key(0), key(3)), ("0:Ayotte Default".to_string(), "3:Ayotte Default".to_string()));
        // The file itself is what is watched.
        assert_eq!(found.watch, [Watch { path: file, extensions: Vec::new() }]);
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
        assert!(text.starts_with("PlugVersion 3.4.4\nSAMPLER {\nHQ {\nsoundlib \"SL-TEST\"\n}\ninstbox 0 \"Kick\" {\n"), "{text}");
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
        assert!(text.contains("soundlib \"SL-TEST\"\n}\nxmics {\nmic {\nname \"X-Kick-In\"\n}\n}\ninstbox 0 \"Kick\" {\n"), "{text}");
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
        assert_eq!(wrong(b"not presets", Some("0:Ayotte Default"), &current), "the file no longer has a kit called 'Ayotte Default'");
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
        assert_eq!((nesting("instbox 0 \"Kick\" {"), nesting("}"), nesting("\"Gretsch {Dry}\" {"), nesting("name \"}}\"")), (1, -1, 1, 0));
        assert_eq!(quoted("submenu \"Clean Kits\""), Some("Clean Kits"));
        assert_eq!(quoted("no quotes"), None);
        let text = String::from_utf8(presets_file(&KITS)).unwrap();
        let found = presets(&text);
        assert_eq!(found.len(), 4);
        assert!(text[found[1].at.clone()].starts_with("\"Gretsch {Dry}\" {\nversion 5\n") && text[found[1].at.clone()].ends_with("macros {\n}\n}"));
        // A file that is cut off inside a kit has the kits before it.
        assert_eq!(presets(&text[..found[2].at.start + 40]).len(), 2);
    }
}
