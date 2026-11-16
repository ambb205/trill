//! Plugin presets: finding the presets a plugin offers, and remembering
//! them.
//!
//! A plugin's presets are not in the plugin's descriptor. They are found
//! by asking code the plugin brings (a CLAP preset-discovery provider, the
//! program lists of a VST3 edit controller) or by walking the folders the
//! format keeps presets in, and a big synth has thousands. So:
//!
//! * nothing here makes a caller wait. The first request for a plugin's
//!   presets starts reading them on a thread of this module's own, and
//!   returns a list that says it is still being read ([`store`]);
//! * whatever runs the plugin's code runs in the scanner process, like
//!   scanning itself ([`crate::scan`]): a plugin that crashes or hangs
//!   while its presets are read costs a message, and what was found before
//!   it did is kept;
//! * what was found is cached on disk beside the scan cache, and read
//!   again only when the plugin or its preset folders have changed
//!   ([`cache`]).
//!
//! What differs between the formats is described by a [`PresetFormat`];
//! each format's own module does the finding (`clap::presets`,
//! `vst3::presets`).
//!
//! *Loading* a preset is not here: it is done by the running plugin, and
//! so by each format's controller, from the [`Locator`] a preset is listed
//! with.

pub(crate) mod cache;
pub(crate) mod store;
pub(crate) mod walk;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use trill_core::presets::{PresetInfo, short_id};

/// The first argument of the scanner executable when it is to read a
/// plugin's presets rather than scan a plugin file:
/// `trill-plugin-scan --presets <format> <plugin file> <plugin id>`.
pub const PRESETS_ARGUMENT: &str = "--presets";

/// How long a running plugin may take to load a preset before whoever
/// asked is told that it is not answering. Presets with samples in them
/// take a while.
pub(crate) const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a plugin that has agreed to load a preset is given to show
/// that it has, and how often it is looked at meanwhile.
const ARRIVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);
const ARRIVAL_POLL: std::time::Duration = std::time::Duration::from_millis(8);

/// Waits for a plugin that has agreed to load a preset to have taken it
/// on, and returns whether it was seen to.
///
/// A plugin may say yes and do the loading a moment later, with its next
/// block or on a thread of its own. Whoever sets a parameter right after
/// asking for a preset means it to go on top of the preset, not to be
/// overwritten when the preset arrives. The one thing that can be seen of
/// that from outside is the plugin's saved state: `state` reads it, and
/// `before` is what it was just before the plugin was asked.
///
/// The wait is short and has an end: a preset that is the sound the plugin
/// already had never shows, and neither does anything in a plugin that
/// saves no state.
pub(crate) fn await_arrival(before: Option<&[u8]>, mut state: impl FnMut() -> Option<Vec<u8>>) -> bool {
    let Some(before) = before else { return false };
    let deadline = std::time::Instant::now() + ARRIVAL_TIMEOUT;
    loop {
        // A plugin that has stopped answering is not waited for either.
        if state().as_deref() != Some(before) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(ARRIVAL_POLL);
    }
}

/// How many presets go into one line of the scanner's output. Small
/// enough that little is lost with the line a crash cuts off.
const BATCH: usize = 64;

/// Where a plugin finds one of its presets: what it needs to be told to
/// load it. Kept in a [`PresetInfo::locator`] as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Locator {
    /// A CLAP preset in a file, with the key that tells it apart if the
    /// file holds several.
    ClapFile { path: PathBuf, key: Option<String> },
    /// A CLAP preset that is part of the plugin itself.
    ClapPlugin { key: Option<String> },
    /// A VST3 preset file.
    Vst3File { path: PathBuf },
    /// A program of one of a VST3 plugin's program lists: `index` of the
    /// `steps + 1` values of the parameter `param` that selects programs.
    Vst3Program { param: u32, index: u32, steps: u32 },
    /// A preset in a file of the vendor's own format ([`crate::vendor`]),
    /// with the key that tells it apart if the file holds several.
    Vendor { format: crate::vendor::Format, path: PathBuf, key: Option<String> },
}

/// What separates the parts of a locator's text. A line break cannot be
/// part of a file name on any system that is in use.
const LOCATOR_SEPARATOR: char = '\n';
/// What the kind of a vendor preset's locator begins with; the name of
/// its format follows.
const VENDOR_KIND: &str = "vendor-";

impl Locator {
    pub(crate) fn to_text(&self) -> String {
        let join = |kind: &str, first: &str, second: &str| {
            format!("{kind}{LOCATOR_SEPARATOR}{first}{LOCATOR_SEPARATOR}{second}")
        };
        match self {
            Locator::ClapFile { path, key } => {
                join("clap-file", &path.to_string_lossy(), key.as_deref().unwrap_or_default())
            }
            Locator::ClapPlugin { key } => join("clap-plugin", "", key.as_deref().unwrap_or_default()),
            Locator::Vst3File { path } => join("vst3-file", &path.to_string_lossy(), ""),
            Locator::Vst3Program { param, index, steps } => {
                join("vst3-program", &param.to_string(), &format!("{index}/{steps}"))
            }
            Locator::Vendor { format, path, key } => {
                let kind = format!("{VENDOR_KIND}{}", format.tag());
                join(&kind, &path.to_string_lossy(), key.as_deref().unwrap_or_default())
            }
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, LOCATOR_SEPARATOR);
        let (kind, first, second) = (parts.next()?, parts.next()?, parts.next()?);
        let key = || (!second.is_empty()).then(|| second.to_string());
        match kind {
            "clap-file" if !first.is_empty() => Some(Locator::ClapFile { path: PathBuf::from(first), key: key() }),
            "clap-plugin" => Some(Locator::ClapPlugin { key: key() }),
            "vst3-file" if !first.is_empty() => Some(Locator::Vst3File { path: PathBuf::from(first) }),
            "vst3-program" => {
                let (index, steps) = second.split_once('/')?;
                Some(Locator::Vst3Program {
                    param: first.parse().ok()?,
                    index: index.parse().ok()?,
                    steps: steps.parse().ok()?,
                })
            }
            vendor if !first.is_empty() => {
                let format = crate::vendor::Format::parse(vendor.strip_prefix(VENDOR_KIND)?)?;
                Some(Locator::Vendor { format, path: PathBuf::from(first), key: key() })
            }
            _ => None,
        }
    }
}

/// A folder whose files are a plugin's presets, and which of them count:
/// what to look at to know whether the presets have to be read again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Watch {
    pub path: PathBuf,
    /// File extensions without the dot, in lower case. Empty: every file.
    /// An empty extension among them: the files whose names have none.
    pub extensions: Vec<String>,
}

/// Takes what reading a plugin's presets finds, as it is found.
pub(crate) trait Sink {
    /// More presets. Their ids are not set yet; see [`identify`].
    fn presets(&mut self, presets: Vec<PresetInfo>);

    /// A folder the presets come from.
    fn watch(&mut self, watch: Watch);
}

/// Everything that was found, in the order it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    pub presets: Vec<PresetInfo>,
    pub watch: Vec<Watch>,
}

impl Sink for Found {
    fn presets(&mut self, presets: Vec<PresetInfo>) {
        self.presets.extend(presets);
    }

    fn watch(&mut self, watch: Watch) {
        if !self.watch.contains(&watch) {
            self.watch.push(watch);
        }
    }
}

/// One line of what the scanner process prints while it reads presets.
/// Whatever else is on its standard output (a plugin's own chatter) is
/// not one of these and is skipped.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Line {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<PresetInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<Watch>,
    /// On the last line, when nothing went wrong.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
    /// On the last line otherwise: why the presets could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// Prints what is found as lines of JSON on standard output, a batch at a
/// time, so that what was found before a plugin took the process down has
/// already left it.
struct Printer {
    waiting: Vec<PresetInfo>,
}

impl Printer {
    fn print(line: &Line) {
        // A line that cannot be encoded or written is a line lost; the
        // parent sees that the last one is missing.
        if let Ok(json) = serde_json::to_string(line) {
            println!("{json}");
        }
    }

    fn flush(&mut self) {
        if !self.waiting.is_empty() {
            Self::print(&Line { presets: std::mem::take(&mut self.waiting), ..Line::default() });
        }
    }
}

impl Sink for Printer {
    fn presets(&mut self, presets: Vec<PresetInfo>) {
        self.waiting.extend(presets);
        if self.waiting.len() >= BATCH {
            self.flush();
        }
    }

    fn watch(&mut self, watch: Watch) {
        Self::print(&Line { watch: vec![watch], ..Line::default() });
    }
}

/// What the scanner process is told of a plugin whose presets it is to
/// read, beyond the plugin's file and id: what the plugin is called, and
/// where vendors keep presets of their own ([`crate::vendor`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct About {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub places: crate::vendor::VendorPlaces,
}

/// Reads the presets of the plugin `id` in `path` **in the current
/// process**: the ones its vendor keeps in files of a format of its own,
/// which takes no code of the plugin's, and then the ones only the
/// plugin's code can tell of. Like
/// [`probe_file`](crate::scan::probe_file), this is what the scanner
/// executable is for.
pub(crate) fn read_in_this_process(
    format: &str,
    path: &Path,
    id: &str,
    about: &About,
    sink: &mut dyn Sink,
) -> Result<(), String> {
    // First, so that a plugin which takes the process down with it has
    // not taken these.
    let plugin = store::PluginRef {
        uri: format!("{format}:{id}"),
        id: id.to_string(),
        path: path.to_path_buf(),
        name: about.name.clone(),
        vendor: about.vendor.clone(),
    };
    crate::vendor::list(&plugin, &about.places, sink);
    match format {
        crate::CLAP_SCHEME => crate::clap::read_presets(path, id, sink),
        crate::VST3_SCHEME => crate::vst3::read_presets(path, id, sink),
        other => Err(format!("'{other}' is not a plugin format")),
    }
}

/// The scanner executable's work when it was started to read presets:
/// `arguments` are what follows [`PRESETS_ARGUMENT`]. Returns the exit
/// code.
pub(crate) fn run_reader(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> u8 {
    let (Some(format), Some(path), Some(id), about, None) =
        (arguments.next(), arguments.next(), arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!(
            "usage: {} {PRESETS_ARGUMENT} <format> <plugin file> <plugin id> [<about the plugin, as JSON>]",
            crate::scan::SCANNER_NAME
        );
        return 2;
    };
    // Without it, or with one that cannot be read, the plugin is asked
    // and no vendor's folder is looked in.
    let about: About = about.and_then(|about| serde_json::from_str(&about.to_string_lossy()).ok()).unwrap_or_default();
    let mut printer = Printer { waiting: Vec::new() };
    let (format, id) = (format.to_string_lossy(), id.to_string_lossy());
    let result = read_in_this_process(&format, Path::new(&path), &id, &about, &mut printer);
    printer.flush();
    match result {
        Ok(()) => {
            Printer::print(&Line { done: true, ..Line::default() });
            0
        }
        Err(reason) => {
            Printer::print(&Line { failed: Some(reason.clone()), ..Line::default() });
            eprintln!("{reason}");
            1
        }
    }
}

/// Gives every preset its id: a short form of its locator, which is the
/// same whenever the preset is found again in the same place. Presets
/// that are found twice (a folder declared twice, a link to it) are
/// listed once.
pub(crate) fn identify(presets: Vec<PresetInfo>) -> Vec<PresetInfo> {
    let mut locators = HashSet::new();
    let mut ids = HashSet::new();
    let mut identified = Vec::with_capacity(presets.len());
    for mut preset in presets {
        if !locators.insert(preset.locator.clone()) {
            continue;
        }
        let id = short_id(&preset.locator);
        // Two locators with one short id are next to impossible, and must
        // still not make one preset stand in for the other.
        preset.id = (1..)
            .map(|attempt| if attempt == 1 { id.clone() } else { format!("{id}-{attempt}") })
            .find(|candidate| !ids.contains(candidate))
            .unwrap_or(id);
        ids.insert(preset.id.clone());
        identified.push(preset);
    }
    identified
}

/// A string from a plugin, made fit to list: one line, without the
/// control characters and the padding some plugins leave in, and no
/// longer than `limit` characters.
pub(crate) fn tidy(text: &str, limit: usize) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut words = cleaned.split_whitespace();
    let mut tidy = String::new();
    if let Some(first) = words.next() {
        tidy.push_str(first);
        for word in words {
            tidy.push(' ');
            tidy.push_str(word);
        }
    }
    if tidy.chars().count() > limit {
        tidy = tidy.chars().take(limit.saturating_sub(1)).collect::<String>().trim_end().to_string();
        tidy.push('…');
    }
    tidy
}

/// The longest name, category part, author and tag kept of a preset, and
/// the longest description, in characters.
pub(crate) const NAME_LIMIT: usize = 120;
pub(crate) const DESCRIPTION_LIMIT: usize = 300;
/// The most tags kept of one preset.
pub(crate) const TAG_LIMIT: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locators_round_trip_through_their_text() {
        let locators = [
            Locator::ClapFile { path: PathBuf::from("C:/Presets/Pads/Warm Pad.fxp"), key: None },
            Locator::ClapFile { path: PathBuf::from("/presets/bank.db"), key: Some("pads/12".into()) },
            Locator::ClapPlugin { key: Some("factory:3".into()) },
            Locator::ClapPlugin { key: None },
            Locator::Vst3File { path: PathBuf::from("C:/Users/me/Documents/VST3 Presets/A/B/c.vstpreset") },
            Locator::Vst3Program { param: 1_886_548_852, index: 41, steps: 127 },
            Locator::Vendor {
                format: crate::vendor::Format::Serum2,
                path: PathBuf::from("C:/Users/me/Documents/Xfer/Serum 2 Presets/Presets/Factory/Pad/Warm.SerumPreset"),
                key: None,
            },
        ];
        for locator in locators {
            assert_eq!(Locator::parse(&locator.to_text()), Some(locator.clone()), "{locator:?}");
        }
        // A key may contain anything, the separator included.
        let odd = Locator::ClapFile { path: PathBuf::from("a"), key: Some("x\ny".into()) };
        assert_eq!(Locator::parse(&odd.to_text()), Some(odd));
    }

    #[test]
    fn text_that_is_no_locator_is_none() {
        for text in ["", "warm_pad", "clap-file\n\n", "vst3-file\n\n", "vst3-program\nx\n1/2", "vst3-program\n1\n12"] {
            assert_eq!(Locator::parse(text), None, "{text:?}");
        }
        assert_eq!(Locator::parse("lv2-file\na\nb"), None);
    }

    fn preset(name: &str, locator: &str) -> PresetInfo {
        PresetInfo { name: name.into(), locator: locator.into(), ..PresetInfo::default() }
    }

    #[test]
    fn ids_come_from_locators_and_a_preset_found_twice_is_listed_once() {
        let identified = identify(vec![preset("A", "one"), preset("B", "two"), preset("A again", "one")]);
        assert_eq!(identified.len(), 2);
        assert_eq!(identified[0].id, short_id("one"));
        assert_eq!(identified[1].id, short_id("two"));
        assert_ne!(identified[0].id, identified[1].id);
        // The same wherever in the list the preset turns up next time.
        let again = identify(vec![preset("B", "two"), preset("A", "one")]);
        assert_eq!(again[1].id, identified[0].id);
    }

    #[test]
    fn lines_carry_only_what_they_have() {
        let line = Line { presets: vec![preset("A", "one")], ..Line::default() };
        let json = serde_json::to_string(&line).unwrap();
        assert_eq!(json, r#"{"presets":[{"id":"","name":"A","locator":"one"}]}"#);
        assert_eq!(serde_json::from_str::<Line>(&json).unwrap(), line);
        assert_eq!(serde_json::to_string(&Line { done: true, ..Line::default() }).unwrap(), r#"{"done":true}"#);
        let failed: Line = serde_json::from_str(r#"{"failed":"it broke"}"#).unwrap();
        assert_eq!(failed.failed.as_deref(), Some("it broke"));
        assert!(serde_json::from_str::<Line>("plugin banner").is_err());
    }

    #[test]
    fn found_collects_in_order_and_watches_a_folder_once() {
        let mut found = Found::default();
        let watch = Watch { path: PathBuf::from("presets"), extensions: vec!["fxp".into()] };
        found.watch(watch.clone());
        found.presets(vec![preset("A", "one")]);
        found.watch(watch.clone());
        found.presets(vec![preset("B", "two")]);
        assert_eq!(found.watch, [watch]);
        assert_eq!(found.presets.iter().map(|preset| preset.name.as_str()).collect::<Vec<_>>(), ["A", "B"]);
    }

    #[test]
    fn strings_from_plugins_are_tidied() {
        assert_eq!(tidy("  Warm\tPad \r\n", 40), "Warm Pad");
        assert_eq!(tidy("line one\nline two", 40), "line one line two");
        assert_eq!(tidy("abcdefghij", 5), "abcd…");
        assert_eq!(tidy("\u{0}\u{7}", 5), "");
        assert_eq!(tidy("Größe", 5), "Größe");
    }

    #[test]
    fn a_preset_has_arrived_when_the_plugins_state_is_no_longer_what_it_was() {
        let started = std::time::Instant::now();
        // Changed already: no waiting.
        assert!(await_arrival(Some(b"old"), || Some(b"new".to_vec())));
        assert!(started.elapsed() < ARRIVAL_TIMEOUT);
        // Changed on the third look.
        let mut looks = 0;
        let arrived = await_arrival(Some(b"old"), || {
            looks += 1;
            Some(if looks < 3 { b"old".to_vec() } else { b"new".to_vec() })
        });
        assert!(arrived && looks == 3);
        // A plugin that stops answering is not waited for.
        assert!(await_arrival(Some(b"old"), || None));

        // Never changed: given up on, after a wait that has an end.
        let started = std::time::Instant::now();
        assert!(!await_arrival(Some(b"old"), || Some(b"old".to_vec())));
        assert!(started.elapsed() >= ARRIVAL_TIMEOUT && started.elapsed() < ARRIVAL_TIMEOUT * 20);
        // Nothing to compare with: nothing to wait for.
        let started = std::time::Instant::now();
        assert!(!await_arrival(None, || Some(b"new".to_vec())));
        assert!(started.elapsed() < ARRIVAL_TIMEOUT);
    }

    #[test]
    fn the_reader_wants_three_arguments_and_takes_a_fourth() {
        let arguments = |list: &[&str]| list.iter().map(std::ffi::OsString::from).collect::<Vec<_>>().into_iter();
        assert_eq!(run_reader(arguments(&[])), 2);
        assert_eq!(run_reader(arguments(&["clap", "x.clap"])), 2);
        assert_eq!(run_reader(arguments(&["clap", "x.clap", "id", "{}", "more"])), 2);
        // The fourth says what the host knows of the plugin. One that
        // cannot be read is done without.
        assert_eq!(run_reader(arguments(&["clap", "no-such-file.clap", "id", "not JSON"])), 1);
        assert_eq!(run_reader(arguments(&["clap", "no-such-file.clap", "id", "{}"])), 1);
        // A format there is none of, and a plugin file there is none of,
        // are failures that say so.
        assert_eq!(run_reader(arguments(&["lv2", "x.lv2", "id"])), 1);
        assert_eq!(run_reader(arguments(&["clap", "no-such-file.clap", "id"])), 1);
    }
}
