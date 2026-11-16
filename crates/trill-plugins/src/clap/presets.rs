//! A CLAP plugin's presets: finding them through the plugin's
//! preset-discovery providers.
//!
//! CLAP splits the work between plugin and host. A *provider* the plugin
//! file brings declares where its presets are (folders, single files, or
//! "inside the plugin") and which file types count; the host walks the
//! folders and hands each file back to the provider, which reads it and
//! describes the presets in it. Every step of that runs the plugin's own
//! code on files of unknown health, which is why it is done in the
//! scanner process ([`crate::presets`]).
//!
//! Loading one is the other half of the same extension, and is done by
//! the running instance: see `ClapThread::load_preset`.

use crate::presets::{DESCRIPTION_LIMIT, Locator, NAME_LIMIT, Sink, TAG_LIMIT, Watch, tidy, walk};
use clack_extensions::preset_discovery::prelude::*;
use clack_host::prelude::*;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use trill_core::presets::PresetInfo;

/// The plugin ABI a preset must name to be a preset of a CLAP plugin.
const CLAP_ABI: &str = "clap";
/// The most presets taken from one file. A bank holds some hundreds; a
/// provider that never stops describing presets must be stopped.
const MAX_PRESETS_PER_FILE: usize = 20_000;

/// Loads the CLAP file at `path` into the current process and has its
/// providers tell of the presets of the plugin `plugin_id`.
///
/// See [`crate::presets::read_in_this_process`] for why the DAW does this
/// in a child process.
pub(crate) fn read_presets(path: &Path, plugin_id: &str, sink: &mut dyn Sink) -> Result<(), String> {
    // SAFETY: as for `probe`: loading a plugin runs code this crate has no
    // control over, and the damage a plugin that violates the CLAP ABI can
    // do is contained by running this function in the scanner process.
    let entry = unsafe { PluginEntry::load(path) }.map_err(|error| error.to_string())?;
    discover(&entry, &super::host_info(), plugin_id, sink)
}

/// Has the providers of a loaded CLAP library tell of the presets of the
/// plugin `plugin_id`. A library without providers has no presets a host
/// can list, which is not an error.
pub(crate) fn discover(
    entry: &PluginEntry,
    host: &HostInfo,
    plugin_id: &str,
    sink: &mut dyn Sink,
) -> Result<(), String> {
    let Some(factory) = entry.get_factory::<PresetDiscoveryFactory>() else { return Ok(()) };
    let providers: Vec<CString> =
        factory.provider_descriptors().filter_map(|descriptor| descriptor.id().map(CStr::to_owned)).collect();

    let mut problems = Vec::new();
    for provider_id in providers {
        let mut provider = match Provider::instantiate(Declared::default(), entry, &provider_id, host) {
            Ok(provider) => provider,
            Err(error) => {
                problems
                    .push(format!("its preset provider '{}' did not start: {error}", provider_id.to_string_lossy()));
                continue;
            }
        };
        let declared = std::mem::take(provider.indexer_mut());
        for place in &declared.places {
            read_place(&mut provider, &declared, place, plugin_id, sink);
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// Somewhere a provider keeps presets.
struct Place {
    name: String,
    flags: Flags,
    /// The file or folder; `None` for presets that are part of the plugin.
    path: Option<PathBuf>,
}

/// What a provider declares when it starts: the indexer side of CLAP's
/// preset discovery.
#[derive(Default)]
struct Declared {
    /// File extensions that are presets, without the dot, in lower case.
    extensions: Vec<String>,
    /// A file type without an extension was declared: every file counts.
    every_file: bool,
    places: Vec<Place>,
    /// Sound pack names by id.
    packs: HashMap<String, String>,
}

impl Declared {
    /// The extensions to walk a folder for; none means every file.
    fn extensions(&self) -> Vec<String> {
        if self.every_file { Vec::new() } else { self.extensions.clone() }
    }
}

fn lossy(text: &CStr) -> String {
    text.to_string_lossy().into_owned()
}

impl IndexerImpl for Declared {
    fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
        let extension = file_type.file_extension.map(lossy).unwrap_or_default();
        match extension.trim().trim_start_matches('.').to_lowercase() {
            extension if extension.is_empty() => self.every_file = true,
            extension if self.extensions.contains(&extension) => {}
            extension => self.extensions.push(extension),
        }
        Ok(())
    }

    fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
        let path = location.location.file_path().map(|path| PathBuf::from(lossy(path)));
        self.places.push(Place { name: tidy(&lossy(location.name), NAME_LIMIT), flags: location.flags, path });
        Ok(())
    }

    fn declare_soundpack(&mut self, soundpack: Soundpack) -> Result<(), HostError> {
        self.packs.insert(lossy(soundpack.id), tidy(&lossy(soundpack.name), NAME_LIMIT));
        Ok(())
    }
}

/// Reads the presets of one place a provider declared.
fn read_place(
    provider: &mut Provider<Declared>,
    declared: &Declared,
    place: &Place,
    plugin_id: &str,
    sink: &mut dyn Sink,
) {
    let mut read = |sink: &mut dyn Sink, file: Option<&Path>, root: Option<&Path>| {
        let mut receiver = Receiver::new(plugin_id, place, &declared.packs, file, root);
        match file {
            None => provider.get_metadata(Location::Plugin, &mut receiver),
            Some(file) => {
                // A path with a NUL byte in it is no path a plugin could
                // be given.
                let Ok(path) = CString::new(file.to_string_lossy().as_bytes()) else { return };
                provider.get_metadata(Location::File { path: &path }, &mut receiver);
            }
        }
        sink.presets(receiver.finish());
    };
    match &place.path {
        None => read(sink, None, None),
        Some(folder) if folder.is_dir() => {
            let extensions = declared.extensions();
            sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
            for file in walk::files(folder, &extensions) {
                read(sink, Some(&file), Some(folder));
            }
        }
        // A single file, such as a bank; or nothing at all, which the
        // provider is left to say.
        Some(file) => {
            sink.watch(Watch { path: file.clone(), extensions: Vec::new() });
            read(sink, Some(file), None);
        }
    }
}

/// A preset a provider is in the middle of describing.
#[derive(Default)]
struct Described {
    name: String,
    key: Option<String>,
    /// The plugins it is for, as (ABI, id).
    plugins: Vec<(String, String)>,
    pack: Option<String>,
    flags: Option<Flags>,
    creators: Vec<String>,
    description: String,
    features: Vec<String>,
}

/// Takes what a provider tells about the presets in one file (or in the
/// plugin itself): the metadata-receiver side of CLAP's preset discovery.
struct Receiver<'a> {
    plugin_id: &'a str,
    place: &'a Place,
    packs: &'a HashMap<String, String>,
    file: Option<&'a Path>,
    /// The declared folder `file` was found in, if it was found in one.
    root: Option<&'a Path>,
    current: Option<Described>,
    presets: Vec<PresetInfo>,
}

impl<'a> Receiver<'a> {
    fn new(
        plugin_id: &'a str,
        place: &'a Place,
        packs: &'a HashMap<String, String>,
        file: Option<&'a Path>,
        root: Option<&'a Path>,
    ) -> Self {
        Self { plugin_id, place, packs, file, root, current: None, presets: Vec::new() }
    }

    /// Everything that was described.
    fn finish(mut self) -> Vec<PresetInfo> {
        self.close();
        self.presets
    }

    /// The preset being described is complete.
    fn close(&mut self) {
        let Some(described) = self.current.take() else { return };
        // A preset says which plugins it is for; one that names none is
        // taken to be for the plugin whose provider tells of it.
        let ours = described.plugins.is_empty()
            || described.plugins.iter().any(|(abi, id)| abi == CLAP_ABI && id == self.plugin_id);
        if ours && self.presets.len() < MAX_PRESETS_PER_FILE {
            let preset = self.listed(described);
            self.presets.push(preset);
        }
    }

    fn listed(&self, described: Described) -> PresetInfo {
        let file_name = || self.file.and_then(Path::file_stem).map(|stem| stem.to_string_lossy().into_owned());
        let name = match tidy(&described.name, NAME_LIMIT) {
            name if name.is_empty() => file_name().unwrap_or_else(|| "Preset".to_string()),
            name => name,
        };
        // Filed under the sound pack it belongs to, or else the place it
        // was found in, and below that under its folders.
        let pack = described.pack.as_ref().and_then(|id| self.packs.get(id));
        let bank = pack.map_or(self.place.name.as_str(), String::as_str);
        let below = match (self.root, self.file) {
            (Some(root), Some(file)) => walk::category_below(root, file),
            _ => String::new(),
        };

        let flags = described.flags.unwrap_or(self.place.flags);
        let marks = [
            (Flags::IS_FACTORY_CONTENT, "factory"),
            (Flags::IS_USER_CONTENT, "user"),
            (Flags::IS_DEMO_CONTENT, "demo"),
            (Flags::IS_FAVORITE, "favorite"),
        ];
        let mut tags: Vec<String> = Vec::new();
        let features = described.features.iter().map(|feature| tidy(feature, NAME_LIMIT));
        let marks = marks.iter().filter(|(flag, _)| flags.contains(*flag)).map(|(_, mark)| mark.to_string());
        for tag in features.chain(marks) {
            if !tag.is_empty() && !tags.contains(&tag) && tags.len() < TAG_LIMIT {
                tags.push(tag);
            }
        }

        let creators: Vec<String> =
            described.creators.iter().map(|creator| tidy(creator, NAME_LIMIT)).filter(|c| !c.is_empty()).collect();
        let locator = match self.file {
            Some(file) => Locator::ClapFile { path: file.to_path_buf(), key: described.key },
            None => Locator::ClapPlugin { key: described.key },
        };
        PresetInfo {
            id: String::new(),
            name,
            category: walk::category(&[bank, &below]),
            author: tidy(&creators.join(", "), NAME_LIMIT),
            tags,
            description: tidy(&described.description, DESCRIPTION_LIMIT),
            locator: locator.to_text(),
        }
    }

    fn current(&mut self) -> Option<&mut Described> {
        self.current.as_mut()
    }
}

impl MetadataReceiverImpl for Receiver<'_> {
    fn on_error(&mut self, error_code: i32, error_message: Option<&CStr>) {
        let message = error_message.map(lossy).unwrap_or_default();
        let place = self.file.map_or_else(|| "the plugin's own presets".to_string(), |file| file.display().to_string());
        log::debug!("[{}] cannot read {place}: {message} (error {error_code})", self.plugin_id);
    }

    fn begin_preset(&mut self, name: Option<&CStr>, load_key: Option<&CStr>) -> Result<(), HostError> {
        self.close();
        if self.presets.len() >= MAX_PRESETS_PER_FILE {
            return Err(HostError::Message("too many presets in one place"));
        }
        self.current = Some(Described {
            name: name.map(lossy).unwrap_or_default(),
            key: load_key.map(lossy).filter(|key| !key.is_empty()),
            ..Described::default()
        });
        Ok(())
    }

    fn add_plugin_id(&mut self, plugin_id: UniversalPluginId) {
        if let Some(current) = self.current() {
            current.plugins.push((lossy(plugin_id.abi), lossy(plugin_id.id)));
        }
    }

    fn set_soundpack_id(&mut self, soundpack_id: &CStr) {
        if let Some(current) = self.current() {
            current.pack = Some(lossy(soundpack_id));
        }
    }

    fn set_flags(&mut self, flags: Flags) {
        if let Some(current) = self.current() {
            current.flags = Some(flags);
        }
    }

    fn add_creator(&mut self, creator: &CStr) {
        if let Some(current) = self.current() {
            current.creators.push(lossy(creator));
        }
    }

    fn set_description(&mut self, description: &CStr) {
        if let Some(current) = self.current() {
            current.description = lossy(description);
        }
    }

    fn set_timestamps(&mut self, _creation_time: Option<Timestamp>, _modification_time: Option<Timestamp>) {}

    fn add_feature(&mut self, feature: &CStr) {
        if let Some(current) = self.current() {
            current.features.push(lossy(feature));
        }
    }

    fn add_extra_info(&mut self, _key: &CStr, _value: &CStr) {}
}

/// A locator's place and key as CLAP wants them handed to a plugin.
pub(crate) struct Address {
    path: Option<CString>,
    key: Option<CString>,
}

impl Address {
    /// `None` for a locator that is not a CLAP preset's, or that holds
    /// something no C string can.
    pub(crate) fn of(locator: &Locator) -> Option<Self> {
        let text = |text: &str| CString::new(text.as_bytes()).ok();
        let (path, key) = match locator {
            Locator::ClapFile { path, key } => (Some(text(&path.to_string_lossy())?), key),
            Locator::ClapPlugin { key } => (None, key),
            _ => return None,
        };
        let key = match key {
            Some(key) => Some(text(key)?),
            None => None,
        };
        Some(Self { path, key })
    }

    pub(crate) fn location(&self) -> Location<'_> {
        match &self.path {
            Some(path) => Location::File { path },
            None => Location::Plugin,
        }
    }

    pub(crate) fn key(&self) -> Option<&CStr> {
        self.key.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(name: &str, flags: Flags) -> Place {
        Place { name: name.into(), flags, path: None }
    }

    fn describe(receiver: &mut Receiver<'_>, name: &CStr, key: Option<&CStr>, plugins: &[&CStr]) {
        receiver.begin_preset(Some(name), key).unwrap();
        for plugin in plugins {
            receiver.add_plugin_id(UniversalPluginId { abi: c"clap", id: plugin });
        }
    }

    #[test]
    fn a_preset_is_listed_with_what_the_provider_said_of_it() {
        let place = place("Factory Patches", Flags::IS_FACTORY_CONTENT);
        let packs = HashMap::from([("pack-1".to_string(), "Deep Space".to_string())]);
        let root = Path::new("patches");
        let file = root.join("Pads").join("Soft").join("air.fxp");
        let mut receiver = Receiver::new("com.acme.synth", &place, &packs, Some(&file), Some(root));

        describe(&mut receiver, c"  Air\tPad ", None, &[c"com.acme.synth"]);
        receiver.add_creator(c"Ada");
        receiver.add_creator(c"Bo");
        receiver.set_description(c"Slow and\nwide.");
        receiver.add_feature(c"pad");
        receiver.add_feature(c"pad");
        receiver.add_feature(c"warm");
        // A second preset in the same file, in a sound pack, the user's.
        describe(&mut receiver, c"", Some(c"slot 2"), &[]);
        receiver.set_soundpack_id(c"pack-1");
        receiver.set_flags(Flags::IS_USER_CONTENT | Flags::IS_FAVORITE);
        // And one for another plugin, which is not listed.
        describe(&mut receiver, c"Theirs", None, &[c"com.acme.other"]);
        receiver.add_plugin_id(UniversalPluginId { abi: c"vst3", id: c"com.acme.synth" });

        let presets = receiver.finish();
        assert_eq!(presets.len(), 2);
        let air = &presets[0];
        assert_eq!(air.name, "Air Pad");
        assert_eq!(air.category, "Factory Patches/Pads/Soft");
        assert_eq!(air.author, "Ada, Bo");
        assert_eq!(air.description, "Slow and wide.");
        assert_eq!(air.tags, ["pad", "warm", "factory"], "the place's flags stand where the preset has none");
        assert_eq!(Locator::parse(&air.locator), Some(Locator::ClapFile { path: file.clone(), key: None }));

        let second = &presets[1];
        assert_eq!(second.name, "air", "a preset without a name goes by its file's");
        assert_eq!(second.category, "Deep Space/Pads/Soft");
        assert_eq!(second.tags, ["user", "favorite"]);
        assert_eq!(Locator::parse(&second.locator), Some(Locator::ClapFile { path: file, key: Some("slot 2".into()) }));
    }

    #[test]
    fn presets_inside_the_plugin_are_found_by_their_key() {
        let place = place("Built in", Flags::empty());
        let packs = HashMap::new();
        let mut receiver = Receiver::new("com.acme.synth", &place, &packs, None, None);
        describe(&mut receiver, c"Init", Some(c"0"), &[c"com.acme.synth"]);
        describe(&mut receiver, c"", None, &[]);
        // What comes before the first preset belongs to none.
        let mut early = Receiver::new("com.acme.synth", &place, &packs, None, None);
        early.add_feature(c"pad");
        early.set_flags(Flags::IS_DEMO_CONTENT);
        assert!(early.finish().is_empty());

        let presets = receiver.finish();
        assert_eq!((presets[0].name.as_str(), presets[0].category.as_str()), ("Init", "Built in"));
        assert!(presets[0].tags.is_empty());
        assert_eq!(Locator::parse(&presets[0].locator), Some(Locator::ClapPlugin { key: Some("0".into()) }));
        assert_eq!(presets[1].name, "Preset");
        assert_eq!(Locator::parse(&presets[1].locator), Some(Locator::ClapPlugin { key: None }));
    }

    #[test]
    fn declarations_are_kept_as_the_walk_wants_them() {
        let mut declared = Declared::default();
        let file_type = |extension| FileType { name: c"Patch", description: None, file_extension: extension };
        declared.declare_filetype(file_type(Some(c".FXP"))).unwrap();
        declared.declare_filetype(file_type(Some(c"fxp"))).unwrap();
        declared.declare_filetype(file_type(Some(c"bank"))).unwrap();
        assert_eq!(declared.extensions(), ["fxp", "bank"]);
        declared.declare_filetype(file_type(None)).unwrap();
        assert!(declared.extensions().is_empty(), "a type without an extension makes every file a preset");

        let location = |location| LocationInfo { name: c"Factory", flags: Flags::IS_FACTORY_CONTENT, location };
        declared.declare_location(location(Location::File { path: c"C:/Patches" })).unwrap();
        declared.declare_location(location(Location::Plugin)).unwrap();
        assert_eq!(declared.places[0].path.as_deref(), Some(Path::new("C:/Patches")));
        assert_eq!(declared.places[1].path, None);
    }

    #[test]
    fn a_locator_becomes_the_address_a_plugin_is_given() {
        let file = Locator::ClapFile { path: PathBuf::from("patches/air.fxp"), key: Some("2".into()) };
        let address = Address::of(&file).unwrap();
        assert_eq!(address.location(), Location::File { path: c"patches/air.fxp" });
        assert_eq!(address.key(), Some(c"2"));
        let inside = Address::of(&Locator::ClapPlugin { key: None }).unwrap();
        assert_eq!((inside.location(), inside.key()), (Location::Plugin, None));

        assert!(Address::of(&Locator::Vst3File { path: PathBuf::from("a.vstpreset") }).is_none());
        assert!(Address::of(&Locator::ClapPlugin { key: Some("a\0b".into()) }).is_none());
    }
}
