//! Tools for finding a sound and for keeping one: a search across the
//! presets of everything that is installed, and presets saved from a
//! device as it stands.
//!
//! A model that cannot hear chooses a sound by its name. Without these it
//! had to know which of the installed instruments to ask (`describe_device`
//! searches one), and had nothing to ask of a plugin that keeps its presets
//! in its own window. So it settled for a built-in synth and set amplifiers
//! parameter by parameter.
//!
//! Also here: what the device tools say after a preset that takes the
//! plugin a while to make ready, and the settings in a plugin's saved
//! state that it has no parameter for.
//!
//! A preset somebody bought is a file among their samples, not in the
//! folder its plugin lists presets from. The sample library has scanned
//! those folders and knows the file by its name, and the plugin host can
//! tell whose preset a file is; the search puts the two together, and
//! answers with the file's path, which is what `preset` takes for it.

use crate::server::{TrillServer, explain, finish, is_plugin};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};
use trill_core::DeviceId;
use trill_core::device::{DeviceCategory, DeviceDescriptor, DeviceFactory};
use trill_core::presets::{CATEGORY_SEPARATOR, PresetInfo, PresetList, PresetStatus, SavedPreset};
use trill_engine::Session;
use trill_library::{EntryKind, Library, Query};

/// Appended to the server's instructions so a model knows these tools
/// exist and when to reach for them.
pub(crate) const INSTRUCTIONS: &str = "\
Finding and keeping sounds
- search_presets finds a sound by words (\"warm pad\", \"808 sub\", \"glass bell\") across the presets of every \
installed instrument and the ones that were saved, and answers with the device and the preset to give \
create_track. Look there before settling for a built-in synth: an installed instrument usually has the better \
sound. The first search reads each plugin's presets, in the background; the reply says which are still being read.
- Presets are the way to a good sound: the makers' own, by people who can hear. Trill lists a plugin's presets \
whichever way the plugin keeps them, the files of its maker's own format included (drum kits, guitar tones, \
amplifier presets, synth patches), so give every plugin instrument and amplifier a preset before touching a \
parameter, and say in a sentence which you chose. Then probe it once (see below): a preset's name does not say \
what its notes sound like or which keys it answers to.
- A reply's `note` after a preset says what to know about it: that a drum sampler or a sampled guitar reads its \
samples for seconds, up to a minute, silent until it has (probe, measure and render wait for that), or that the \
preset switched a setting on that changes how a part is played.
- A preset need not be one that is listed. `preset` also takes the whole path of a preset file: one the user \
bought and keeps with their samples, or names to you. search_presets finds such files in the sample library's \
folders by their names, with the plugin each is for; an item with `file` is one, and `file` is what to give \
`preset`. A file that is another plugin's is refused in a sentence.
- A few plugins still list no presets: they keep them where a host cannot read them, and only the user can choose \
one, in the plugin's own window (show_device_editor opens it). Then save_preset keeps that sound under a name. \
save_preset also keeps a sound you have shaped by parameter. Saved presets are found by search_presets and given \
to a device by `preset` like any other, in every song.
- A plugin's parameters are numbers whose meaning only the plugin knows. get_device shows each value as the \
plugin displays it (`text`), lists the parameters that were moved from their defaults, and finds others by name \
(`query`: \"mic\", \"gain\"). update_device takes a value as that text: \"-6 dB\", \"Dynamic 57\", \"On\".
- get_device shows the channels a device takes and gives, and get_project marks a device that takes one channel: \
what comes before it is mono from there on. Some plugins are mono inside whatever they take (a guitar amplifier \
is) and widen the sound with a parameter of their own, such as a doubler: look for it with get_device `query`.
- Some plugins have settings in their saved state that no parameter reaches (an Ample guitar has 250: its \
effects, pedals, amplifier channels and strummer): get_device counts them (`settings_count`) and its `query` \
finds them by name beside the parameters, and update_device `settings` sets them.
- One parameter that matters for writing: the Ample guitars come with \"Auto Legato\" at 1, and nearly every \
preset of theirs and of the bass puts it there, which hammers a note on to a neighbouring one that still rings. \
For a picked chord or a strum that is to ring, set it to 0 (update_device params), again after every preset.";

/// How many presets a search lists when the caller does not say, and the
/// most it can ask for.
const DEFAULT_HITS: usize = 20;
const MAX_HITS: usize = 100;
/// How long a search waits for presets that are being read before it
/// answers with what there is, and the longest it can be asked to.
const DEFAULT_WAIT: f64 = 8.0;
const MAX_WAIT: f64 = 60.0;
/// The most devices a reply names in one of its lists.
const NAMED_DEVICES: usize = 12;
/// The most saved presets a device listing names.
const NAMED_SAVED: usize = 24;
/// The most preset files of the sample library that one search asks the
/// plugin host about: each costs a look at the file's first bytes at most.
const MAX_LIBRARY_FILES: usize = 2_000;
/// What a preset file of the sample library is filed under, before the
/// folders it lies in.
const LIBRARY_CATEGORY: &str = "Library";

/// What `describe_device` and `get_device` say of a plugin that lists no
/// presets, in place of an empty list.
pub(crate) const NO_PRESETS: &str = "This plugin gives a host no presets to list, and keeps them in no files that \
    Trill reads: they are in its own window, where only the user can reach them. Ask the user to choose one there \
    (show_device_editor opens it), then save_preset keeps the sound under a name, for this song and every other; \
    or shape the sound by parameter (get_device `query` finds parameters by name). A preset file the user has \
    for it (a .vstpreset) is given to it by its path, as `preset`.";
/// The most settings a search of a device's saved state lists.
const SHOWN_SETTINGS: usize = 40;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchPresetsRequest {
    /// Words that describe the sound: "warm pad", "808 sub", "glass bell". Every word must be in a preset's
    /// name, category or tags (or its author or description).
    pub query: String,
    /// Which devices to look through: "instrument" (the default), "effect", or "any".
    pub category: Option<String>,
    /// Only devices whose name, vendor or URI contains this text, e.g. "surge".
    pub device: Option<String>,
    /// Skip this many of the matching presets: the reply's `more` says where the next page starts.
    pub offset: Option<usize>,
    /// How many presets to list (default 20, at most 100).
    pub limit: Option<usize>,
    /// How long to wait, in seconds, for presets that are still being read (default 8, at most 60). The
    /// reading goes on in the background either way.
    pub wait_seconds: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SavePresetRequest {
    /// Device id: the device whose sound, as it is now, becomes the preset.
    pub device: u64,
    /// What to call the preset, e.g. "Glass Lead". A saved preset of this device type with the same name is
    /// replaced.
    pub name: String,
    /// What the sound is like and what it is for, in a sentence. search_presets looks here too.
    pub description: Option<String>,
    /// Words to find it by: "lead", "bright", "djent".
    pub tags: Option<Vec<String>>,
}

/// One device type that was looked through, with its presets as far as
/// they were known when the search answered.
struct Searched {
    descriptor: DeviceDescriptor,
    list: PresetList,
    /// Whether these are not the device's listed presets but preset files
    /// for it in the sample library: each has its path for an id.
    library: bool,
}

/// Whether a search that is kept to `category` and to devices called
/// something like `device` looks through the device type `descriptor`.
fn looked_through(descriptor: &DeviceDescriptor, category: Option<DeviceCategory>, device: Option<&str>) -> bool {
    category.is_none_or(|category| descriptor.category == category)
        && device.is_none_or(|text| {
            [&descriptor.name, &descriptor.vendor, &descriptor.uri]
                .iter()
                .any(|field| field.to_lowercase().contains(text))
        })
}

/// A path as it is compared with another: where the file system does not
/// tell upper case from lower, neither does this.
fn folded(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// The preset files in the sample library that could be what `query` is
/// after, with the device each is a preset of, as presets of those
/// devices to search with the rest. A file's name is the preset's, and
/// the folders it lies in, from the library's folder down, are what it is
/// filed under: that is where a pack says what it is.
///
/// Left out: files that are among the listed presets of `searched`
/// already (the library may cover a plugin's own preset folder), files no
/// installed device is known to take, and devices the search is not
/// looking through.
fn library_presets(
    library: &Library,
    factory: &dyn DeviceFactory,
    query: &str,
    searched: &[Searched],
    looked_through: impl Fn(&DeviceDescriptor) -> bool,
) -> Vec<Searched> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let roots = library.roots();
    let found = library.search(&Query { kind: Some(EntryKind::Preset), ..Query::default() });
    if found.is_empty() {
        return Vec::new();
    }
    let listed: HashSet<String> = searched
        .iter()
        .flat_map(|entry| entry.list.presets.iter())
        .filter_map(trill_plugins::preset_file_of)
        .map(|path| folded(&path))
        .collect();
    let descriptors = factory.descriptors();
    let mut by_device: BTreeMap<String, Vec<PresetInfo>> = BTreeMap::new();
    let mut asked = 0;
    for entry in found.entries() {
        let below = roots.iter().find_map(|root| entry.path.parent()?.strip_prefix(root).ok());
        let folders: Vec<String> = below
            .into_iter()
            .flat_map(Path::components)
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let category = std::iter::once(LIBRARY_CATEGORY.to_string())
            .chain(folders)
            .collect::<Vec<_>>()
            .join(&CATEGORY_SEPARATOR.to_string());
        // Only what has one of the words is worth asking about.
        let text = format!("{} {category}", entry.name).to_lowercase();
        if !words.iter().any(|word| text.contains(word.as_str())) || listed.contains(&folded(&entry.path)) {
            continue;
        }
        if asked >= MAX_LIBRARY_FILES {
            break;
        }
        asked += 1;
        let Some(uri) = factory.preset_file_type(&entry.path) else { continue };
        by_device.entry(uri).or_default().push(PresetInfo {
            id: entry.path.display().to_string(),
            name: entry.name.clone(),
            category,
            ..PresetInfo::default()
        });
    }
    by_device
        .into_iter()
        .filter_map(|(uri, presets)| {
            let descriptor = descriptors.iter().find(|descriptor| descriptor.uri == uri)?.clone();
            looked_through(&descriptor).then(|| Searched {
                descriptor,
                list: PresetList::ready(presets),
                library: true,
            })
        })
        .collect()
}

/// One preset a search found.
struct Hit<'a> {
    /// How near a match it is; the lower the nearer.
    rank: (usize, u8),
    /// The device type it belongs to, by its place in the list of those
    /// searched.
    device: usize,
    preset: &'a PresetInfo,
}

/// The presets of the devices that a search is to look through. Asking
/// for them starts the reading of those nobody has read yet; what is being
/// read is waited for until `wait` is up, the first device first.
fn read_presets(
    factory: &dyn DeviceFactory,
    category: Option<DeviceCategory>,
    device: Option<&str>,
    wait: Duration,
) -> Vec<Searched> {
    let deadline = Instant::now() + wait;
    let wanted = |descriptor: &DeviceDescriptor| looked_through(descriptor, category, device);
    let presets = |uri: &str| factory.presets(uri).unwrap_or_default();
    let mut searched: Vec<Searched> = factory
        .descriptors()
        .into_iter()
        .filter(wanted)
        .map(|descriptor| Searched { list: presets(&descriptor.uri), descriptor, library: false })
        .collect();
    for entry in searched.iter_mut().filter(|entry| entry.list.is_reading()) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        entry.list = factory.presets_within(&entry.descriptor.uri, remaining).unwrap_or_default();
    }
    // What was finished while another was waited for.
    for entry in searched.iter_mut().filter(|entry| entry.list.is_reading()) {
        entry.list = presets(&entry.descriptor.uri);
    }
    searched
}

/// The presets of `searched` that have the words of `query`, best first.
/// Where none has all of them, those that have some, and `true` to say so.
fn hits<'a>(searched: &'a [Searched], query: &str) -> (Vec<Hit<'a>>, bool) {
    let mut hits: Vec<Hit<'a>> = Vec::new();
    for (device, entry) in searched.iter().enumerate() {
        let found = entry.list.matches(query, None);
        hits.extend(found.into_iter().map(|(rank, preset)| Hit { rank: (0, rank), device, preset }));
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let partial = hits.is_empty() && words.len() > 1;
    if partial {
        for (device, entry) in searched.iter().enumerate() {
            // How many of the words each preset has, and how near its
            // nearest match is.
            let mut having: HashMap<&str, (usize, u8, &PresetInfo)> = HashMap::new();
            for word in &words {
                for (rank, preset) in entry.list.matches(word, None) {
                    let found = having.entry(preset.id.as_str()).or_insert((0, rank, preset));
                    found.0 += 1;
                    found.1 = found.1.min(rank);
                }
            }
            // In the list's own order: a map has none.
            let listed = entry.list.presets.iter().filter_map(|preset| having.get(preset.id.as_str()));
            hits.extend(listed.map(|(count, rank, preset)| Hit { rank: (words.len() - count, *rank), device, preset }));
        }
    }

    // Best first; among equals the saved ones, then each device's first
    // before any device's second, so that no device crowds out the rest.
    let mut turns: HashMap<(usize, (usize, u8)), usize> = HashMap::new();
    let mut keyed: Vec<((usize, u8), bool, usize, Hit<'a>)> = hits
        .into_iter()
        .map(|hit| {
            let turn = turns.entry((hit.device, hit.rank)).or_default();
            *turn += 1;
            (hit.rank, SavedPreset::name_of(hit.preset).is_none(), *turn, hit)
        })
        .collect();
    keyed.sort_by_key(|(rank, own, turn, hit)| (*rank, *own, *turn, hit.device));
    (keyed.into_iter().map(|(_, _, _, hit)| hit).collect(), partial)
}

/// Names for a reply: the first few, and that there are more.
fn named(names: &[String]) -> Vec<String> {
    let mut shown: Vec<String> = names.iter().take(NAMED_DEVICES).cloned().collect();
    if names.len() > shown.len() {
        shown.push(format!("and {} more", names.len() - shown.len()));
    }
    shown
}

/// A page of what a search found, with what it could not look through.
fn search_json(searched: &[Searched], query: &str, offset: usize, limit: usize) -> Value {
    let (hits, partial) = hits(searched, query);
    let offset = offset.min(hits.len());
    let items: Vec<Value> = hits
        .iter()
        .skip(offset)
        .take(limit)
        .map(|hit| {
            let Searched { descriptor, library, .. } = &searched[hit.device];
            let preset = hit.preset;
            let mut item = json!({ "device": descriptor.uri, "device_name": descriptor.name, "preset": preset.name });
            if !preset.category.is_empty() {
                item["category"] = json!(preset.category);
            }
            if *library {
                // A file is asked for by where it is.
                item["file"] = json!(preset.id);
            } else if preset.id != preset.name {
                // A built-in device's presets are asked for by their names.
                item["id"] = json!(preset.id);
            }
            item
        })
        .collect();
    let files = items.iter().any(|item| item.get("file").is_some());

    // What a device lists is counted; files in the library are only found.
    let listing = || searched.iter().filter(|entry| !entry.library);
    let total: usize = listing().map(|entry| entry.list.len()).sum();
    let with_presets = listing().filter(|entry| !entry.list.is_empty()).count();
    let mut out = json!({
        "matching": hits.len(),
        "searched": format!("{total} presets of {with_presets} devices"),
    });
    if offset > 0 {
        out["offset"] = json!(offset);
    }
    let shown = offset + items.len();
    if shown < hits.len() {
        out["more"] = json!(format!("{} more: offset {shown} lists the next", hits.len() - shown));
    }
    if partial {
        out["note"] = json!("No preset has all of these words; these have some of them, the most first.");
    }
    if !items.is_empty() {
        out["use"] = json!(
            "create_track takes `device` as `instrument` and `id` (or the preset's name) as `preset`; add_device \
             and update_device take them the same way."
        );
    }
    if files {
        out["files"] = json!(
            "An item with `file` is a preset file in the sample library, not one the device lists: give `file` \
             as `preset`."
        );
    }

    let name = |entry: &Searched| format!("{} ({})", entry.descriptor.name, entry.descriptor.uri);
    let reading: Vec<String> = listing().filter(|entry| entry.list.is_reading()).map(name).collect();
    if !reading.is_empty() {
        out["still_reading"] = json!(named(&reading));
        let whose = match reading.len() {
            1 => "One device's presets are".to_string(),
            count => format!("The presets of {count} devices are"),
        };
        out["status"] =
            json!(format!("{whose} still being read, in the background: search again in a few seconds for theirs."));
    }
    let failed: Vec<String> = listing()
        .filter_map(|entry| match &entry.list.status {
            PresetStatus::Failed(reason) => Some(format!("{}: {reason}", entry.descriptor.name)),
            _ => None,
        })
        .collect();
    if !failed.is_empty() {
        out["could_not_read"] = json!(named(&failed));
    }
    // Said once, where the search begins: which plugins have nothing to
    // search, and what to do about one of them.
    // A plugin installed in two formats is one plugin: it is not without
    // presets if either format lists them, and is named once.
    let has_presets = |name: &str| {
        searched.iter().any(|entry| !entry.list.is_empty() && entry.descriptor.name.eq_ignore_ascii_case(name))
    };
    let mut silent: Vec<String> = listing()
        .filter(|entry| is_plugin(&entry.descriptor.uri))
        .filter(|entry| entry.list.is_empty() && entry.list.status == PresetStatus::Ready)
        .filter(|entry| !has_presets(&entry.descriptor.name))
        .map(|entry| entry.descriptor.name.clone())
        .collect();
    let mut named_already = std::collections::HashSet::new();
    silent.retain(|name| named_already.insert(name.to_lowercase()));
    if offset == 0 && !silent.is_empty() {
        out["without_presets"] = json!(named(&silent));
        out["without_presets_note"] = json!(
            "These plugins give a host no presets to search: they keep them in their own windows. Once the user \
             has chosen a sound there, save_preset keeps it under a name, and it is found here."
        );
    }
    out["items"] = json!(items);
    out
}

/// The settings a call asked to change, as the text a plugin's state has
/// them as: a number as a number, a switch as the number it stands for
/// nowhere, since plugins differ (0 and 1, or -1 and 1), so true and
/// false are refused.
pub(crate) fn settings_asked(asked: Option<&BTreeMap<String, Value>>) -> Result<Vec<(String, String)>, String> {
    let mut settings = Vec::new();
    for (name, value) in asked.into_iter().flatten() {
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            other => {
                return Err(format!(
                    "the setting '{name}' needs a number or a string, written as get_device shows its value now \
                     (given: {other}); a switch is a number, and which number is off differs from plugin to plugin"
                ));
            }
        };
        settings.push((name.clone(), text));
    }
    Ok(settings)
}

/// Says in a reply what a device goes on doing after the preset or the
/// settings it was just given, if that is worth saying.
pub(crate) fn preset_note_into(out: &mut Value, session: &mut Session, device: DeviceId) {
    if let Some(note) = session.take_preset_note(device) {
        out["note"] = json!(note);
    }
}

/// Adds to a `get_device` reply the settings in the device's saved state
/// that it has no parameter for: how many there are, and with a `query`
/// those whose names have its words.
pub(crate) fn settings_into(out: &mut Value, session: &Session, device: DeviceId, query: Option<&str>) {
    let settings = session.device_settings(device).unwrap_or_default();
    if settings.is_empty() {
        return;
    }
    out["settings_count"] = json!(settings.len());
    let words: Vec<String> = query.unwrap_or_default().split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        out["settings_note"] = json!(
            "The plugin's saved state holds this many settings that it has no parameter for. `query` finds them \
             by name, like parameters; update_device `settings` sets one."
        );
        return;
    }
    let found: Vec<&(String, String)> = settings
        .iter()
        .filter(|(name, _)| words.iter().all(|word| name.to_lowercase().contains(word.as_str())))
        .collect();
    out["settings"] =
        found.iter().take(SHOWN_SETTINGS).map(|(name, value)| json!({ "name": name, "value": value })).collect();
    if found.len() > SHOWN_SETTINGS {
        out["settings_more"] = json!(format!("{} more match; narrow the query", found.len() - SHOWN_SETTINGS));
    }
    if !found.is_empty() {
        out["settings_note"] = json!(
            "`settings` are read from the plugin's saved state: it has no parameter for them. update_device \
             `settings` sets one by its name to a value written as it is written here."
        );
    }
}

/// Adds what is known of a device type's presets to its entry in a
/// listing, without reading anything: how many a plugin has, where that
/// was read in this run of the program or an earlier one, and the names of
/// the presets that were saved for it.
pub(crate) fn known_presets_into(out: &mut Value, factory: &dyn DeviceFactory, descriptor: &DeviceDescriptor) {
    let Some(list) = factory.known_presets(&descriptor.uri) else { return };
    if is_plugin(&descriptor.uri) && !list.is_reading() {
        out["preset_count"] = json!(list.len());
    }
    let saved: Vec<&str> = list
        .presets
        .iter()
        .filter(|preset| SavedPreset::name_of(preset).is_some())
        .map(|preset| preset.name.as_str())
        .take(NAMED_SAVED)
        .collect();
    if !saved.is_empty() {
        out["saved_presets"] = json!(saved);
    }
}

#[tool_router(router = sound_router, vis = "pub(crate)")]
impl TrillServer {
    /// Find a sound among the presets of everything installed: every instrument plugin, the built-in
    /// instruments, the presets that were saved, and the preset files in the sample library's folders (presets
    /// the user bought, kept with their samples). `query` is words such as "warm pad", "808 sub" or
    /// "glass bell"; the reply lists the best matches with the device and the preset, ready to give to
    /// create_track (`instrument`, `preset`). Use it before settling for a built-in synth. A plugin's presets
    /// are read the first time they are asked for, in the background, and remembered: the reply names the
    /// devices that are still being read, so search again in a moment for theirs.
    #[tool(annotations(title = "Search presets", read_only_hint = true))]
    async fn search_presets(&self, Parameters(request): Parameters<SearchPresetsRequest>) -> CallToolResult {
        let factory = self.session.lock().factory();
        let runtime = self.runtime.clone();
        let reply = async {
            let query = request.query.trim().to_string();
            if query.is_empty() {
                return Err("give `query`: words that describe the sound, such as \"warm pad\"".to_string());
            }
            let category = match request.category.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None | Some("instrument" | "instruments") => Some(DeviceCategory::Instrument),
                Some("effect" | "effects") => Some(DeviceCategory::Effect),
                Some("any" | "all" | "both") => None,
                Some(other) => {
                    return Err(format!("category '{other}' should be \"instrument\", \"effect\" or \"any\""));
                }
            };
            let device = request.device.as_deref().map(str::trim).filter(|text| !text.is_empty());
            let device = device.map(str::to_lowercase);
            let wait = request.wait_seconds.filter(|seconds| seconds.is_finite()).unwrap_or(DEFAULT_WAIT);
            let wait = Duration::from_secs_f64(wait.clamp(0.0, MAX_WAIT));
            // The wait is spent off the async threads and without the
            // session.
            let (asked, words) = (device.clone(), query.clone());
            let read = tokio::task::spawn_blocking(move || {
                let mut searched = read_presets(factory.as_ref(), category, asked.as_deref(), wait);
                let wanted = |descriptor: &DeviceDescriptor| looked_through(descriptor, category, asked.as_deref());
                let files = library_presets(&runtime.library, factory.as_ref(), &words, &searched, wanted);
                searched.extend(files);
                searched
            });
            let searched = read.await.map_err(|error| format!("the presets could not be read: {error}"))?;
            if let (true, Some(device)) = (searched.is_empty(), &device) {
                return Err(format!("no installed device is called anything like '{device}'; list_devices shows them"));
            }
            let limit = request.limit.unwrap_or(DEFAULT_HITS).min(MAX_HITS);
            Ok(search_json(&searched, &query, request.offset.unwrap_or(0), limit))
        }
        .await;
        finish(reply)
    }

    /// Keep a device's sound as it is now as a preset, under a name: a plugin with everything that was set in
    /// its own window, a built-in device with every parameter. From then on it is one of the presets of that
    /// device type, in this song and every other: search_presets and describe_device find it, and `preset`
    /// gives it to a device in create_track, add_device and update_device. This is how a plugin that lists no
    /// presets of its own gets some.
    #[tool(annotations(title = "Save preset"))]
    fn save_preset(&self, Parameters(request): Parameters<SavePresetRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = DeviceId(request.device);
        let reply = (|| {
            let description = request.description.as_deref().unwrap_or_default();
            let tags = request.tags.clone().unwrap_or_default();
            let saved = session
                .save_preset(id, &request.name, description, &tags)
                .map_err(|error| explain(session.project(), error))?;
            let device = session.project().device(id).map_err(|error| error.to_string())?;
            let mut out = json!({ "saved": saved.info.path(), "id": saved.info.id, "device": device.uri });
            if saved.replaced {
                out["replaced"] = json!(true);
            }
            match &saved.file {
                Some(file) => out["file"] = json!(file.display().to_string()),
                None => {
                    out["note"] = json!("Nothing is kept on disk in this session: the preset is there until it ends.");
                }
            }
            out["use"] = json!(format!(
                "`preset: \"{}\"` gives this sound to a {} device, in create_track, add_device and update_device.",
                saved.info.name, device.name
            ));
            Ok(out)
        })();
        finish(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::presets::short_id;

    fn preset(name: &str, category: &str) -> PresetInfo {
        let locator = format!("{category}/{name}");
        PresetInfo {
            id: short_id(&locator),
            name: name.into(),
            category: category.into(),
            locator,
            ..PresetInfo::default()
        }
    }

    fn device(uri: &str, name: &str, list: PresetList) -> Searched {
        let descriptor = DeviceDescriptor {
            uri: uri.into(),
            name: name.into(),
            vendor: "Test".into(),
            category: DeviceCategory::Instrument,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        };
        Searched { descriptor, list, library: false }
    }

    fn found(reply: &Value) -> Vec<String> {
        let items = reply["items"].as_array().unwrap();
        items
            .iter()
            .map(|item| format!("{}: {}", item["device_name"].as_str().unwrap(), item["preset"].as_str().unwrap()))
            .collect()
    }

    #[test]
    fn a_search_puts_the_nearest_first_and_takes_turns_between_devices() {
        let big: Vec<PresetInfo> = (0..40).map(|n| preset(&format!("Pad {n}"), "Factory/Warm")).collect();
        let saved = SavedPreset { uri: "vst3:B".into(), name: "Warm Pad Mine".into(), ..SavedPreset::default() };
        let searched = [
            device("clap:a", "Big", PresetList::ready(big)),
            device("vst3:B", "Small", PresetList::ready(vec![saved.info(), preset("Warm Pad", "Pads")])),
            device(
                "builtin:polysynth",
                "Poly Synth",
                PresetList::ready(vec![PresetInfo {
                    id: "warm_pad".into(),
                    name: "warm_pad".into(),
                    ..PresetInfo::default()
                }]),
            ),
        ];
        let reply = search_json(&searched, "warm pad", 0, 6);
        assert_eq!(reply["matching"], 43);
        assert_eq!(reply["searched"], "43 presets of 3 devices");
        // The name that is the query, the name that begins with it and
        // the name that has its words; then those found by their
        // category, of which the big device has forty.
        assert_eq!(
            found(&reply),
            [
                "Small: Warm Pad",
                "Small: Warm Pad Mine",
                "Poly Synth: warm_pad",
                "Big: Pad 0",
                "Big: Pad 1",
                "Big: Pad 2"
            ]
        );
        assert_eq!(reply["more"], "37 more: offset 6 lists the next");
        assert!(reply["use"].as_str().unwrap().contains("create_track"));
        assert_eq!(reply["items"][0]["category"], "Pads");
        assert_eq!(reply["items"][0]["id"].as_str().map(str::len), Some(10));
        assert!(reply["items"][2].get("id").is_none(), "a built-in preset is asked for by its name");
        let next = search_json(&searched, "warm pad", 40, 6);
        assert_eq!((next["offset"].as_u64(), found(&next).len()), (Some(40), 3));
        assert!(next.get("more").is_none());
    }

    #[test]
    fn a_search_that_finds_nothing_with_every_word_says_what_has_some() {
        let searched = [
            device("clap:a", "One", PresetList::ready(vec![preset("Sub Bass", "Basses"), preset("Glass", "Keys")])),
            device("clap:b", "Two", PresetList::ready(vec![preset("808 Kick", "Drums"), preset("808 Sub Boom", "")])),
        ];
        let reply = search_json(&searched, "808 sub", 0, 10);
        assert_eq!(found(&reply), ["Two: 808 Sub Boom"]);
        assert!(reply.get("note").is_none());

        let reply = search_json(&searched, "808 sub bass", 0, 10);
        assert_eq!(found(&reply), ["One: Sub Bass", "Two: 808 Sub Boom", "Two: 808 Kick"]);
        assert!(reply["note"].as_str().unwrap().contains("No preset has all of these words"));
        let nothing = search_json(&searched, "theremin", 0, 10);
        assert_eq!((nothing["matching"].as_u64(), found(&nothing).len()), (Some(0), 0));
        assert!(nothing.get("note").is_none() && nothing.get("use").is_none());
    }

    #[test]
    fn a_search_says_what_it_could_not_look_through() {
        let failed =
            PresetList { status: PresetStatus::Failed("the plugin crashed".into()), presets: Vec::new().into() };
        let searched = [
            device("clap:a", "Read", PresetList::ready(vec![preset("Warm", "")])),
            device("clap:b", "Reading", PresetList::reading(vec![preset("Warm So Far", "")])),
            device("vst3:C", "Windowed", PresetList::default()),
            device("vst3:A", "Read", PresetList::default()),
            device("vst3:D", "Broken", failed),
            device("clap:c", "Windowed", PresetList::default()),
            device("builtin:sampler", "Sampler", PresetList::default()),
        ];
        let reply = search_json(&searched, "warm", 0, 10);
        assert_eq!(found(&reply), ["Read: Warm", "Reading: Warm So Far"], "what was read so far counts");
        assert_eq!(reply["still_reading"], json!(["Reading (clap:b)"]));
        assert!(reply["status"].as_str().unwrap().starts_with("One device's presets are still being read"));
        assert_eq!(reply["could_not_read"], json!(["Broken: the plugin crashed"]));
        // A plugin without presets is named, once for both its formats,
        // unless its other format has some; a built-in device without any
        // has none to keep anywhere.
        assert_eq!(reply["without_presets"], json!(["Windowed"]));
        assert!(reply["without_presets_note"].as_str().unwrap().contains("save_preset"));
        assert!(search_json(&searched, "warm", 1, 10).get("without_presets").is_none(), "said once");

        let many: Vec<String> = (0..30).map(|n| format!("Device {n}")).collect();
        let shown = named(&many);
        assert_eq!((shown.len(), shown.last().map(String::as_str)), (13, Some("and 18 more")));
    }
}
