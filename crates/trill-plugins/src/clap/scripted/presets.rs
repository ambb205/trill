//! The presets of the scripted plugins: a plugin file whose plugins can be
//! given presets, and which brings the providers that tell a host of them.
//!
//! The presets are everything CLAP lets presets be. Some are files in a
//! folder, sorted into subfolders; one file is a bank that holds several;
//! two are part of the plugin itself. All a preset does is set the gain,
//! which is enough to hear and to read back. Some are scripted to fail.

use super::{GAIN, MainThread, Scripted, id, kind};
use clack_extensions::preset_discovery::prelude::*;
use clack_plugin::entry::prelude::*;
use clack_plugin::prelude::*;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

/// The presets that are part of the plugin: name, load key and gain.
pub(crate) const BUILT_IN: [(&CStr, &str, f64); 2] = [(c"Quiet", "quiet", 0.125), (c"Loud", "loud", 1.5)];

/// The preset files, below [`folder`]: path and contents. A patch says
/// `gain=<value>` and may describe itself on a second line; a bank holds a
/// preset per line, as `<name>=<gain>`.
pub(crate) const FILES: [(&str, &str); 9] = [
    ("Bass.patch", "gain=0.5"),
    // The provider fails to read it the first time it is asked.
    ("Flaky.patch", "gain=0.875"),
    ("Drums.bank", "Kick=0.625\nSnare=0.375"),
    // Loads with a complaint to the host, and without one.
    ("Rotten.patch", "rotten"),
    ("Mute.patch", "mute"),
    // Says it is for some other plugin.
    ("Theirs.patch", "other"),
    ("Pads/Warm.patch", "gain=0.75\nA warm\tone."),
    ("Pads/Soft/Air.patch", "gain=0.25"),
    ("notes.txt", "not a preset"),
];

/// The folder the providers declare, with [`FILES`] in it. It is made the
/// first time it is asked for, and the same for every test: none changes
/// it.
pub(crate) fn folder() -> &'static Path {
    static FOLDER: OnceLock<PathBuf> = OnceLock::new();
    FOLDER.get_or_init(|| {
        let folder = std::env::temp_dir().join(format!("trill-scripted-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        for (name, contents) in FILES {
            let path = folder.join(name);
            std::fs::create_dir_all(path.parent().expect("a file is in a folder")).expect("the folder is writable");
            std::fs::write(path, contents).expect("the folder is writable");
        }
        folder
    })
}

/// The gain a preset file's text gives the preset `key` (or, without a
/// key, the only one in it).
fn gain_in(text: &str, key: Option<&str>) -> Option<f64> {
    let wanted = key.unwrap_or("gain");
    text.lines().find_map(|line| line.split_once('=').filter(|(name, _)| *name == wanted)?.1.trim().parse().ok())
}

impl<const KIND: u8> PluginPresetLoadImpl for MainThread<'_, KIND> {
    fn load_from_location(&self, location: Location, load_key: Option<&CStr>) -> Result<(), PluginError> {
        let host = self.shared.host.get_extension::<HostPresetLoad>();
        // Tells the host why, as a plugin should, and fails.
        let refuse = |os_error: i32, message: &CStr| {
            if let Some(host) = host {
                host.on_error(&self.host, location, load_key, os_error, Some(message));
            }
            PluginError::Message("the preset was not loaded")
        };
        let key = load_key.map(|key| key.to_string_lossy().into_owned());
        let gain = match location {
            Location::Plugin => BUILT_IN
                .iter()
                .find(|(_, built_in, _)| Some(*built_in) == key.as_deref())
                .map(|(_, _, gain)| *gain)
                .ok_or_else(|| refuse(0, c"there is no such built-in preset"))?,
            Location::File { path } => {
                let text = std::fs::read_to_string(path.to_string_lossy().as_ref())
                    .map_err(|error| refuse(error.raw_os_error().unwrap_or(0), c""))?;
                if text == "mute" {
                    return Err(PluginError::Message("scripted to refuse without a word"));
                }
                gain_in(&text, key.as_deref()).ok_or_else(|| refuse(0, c" the patch is rotten\n"))?
            }
        };
        if KIND == kind::LATE_PRESETS {
            // Taken on by the audio thread, with its next block.
            self.shared.arriving.store(gain.to_bits(), Ordering::Relaxed);
        } else {
            self.shared.set(GAIN, gain);
        }
        if let Some(host) = host {
            host.loaded(&self.host, location, load_key);
        }
        Ok(())
    }
}

/// The plugins of the scripted preset file, in the order it lists them.
const PLUGINS: [u8; 2] = [kind::PRESETS, kind::LATE_PRESETS];

struct Plugins<const BROKEN: bool> {
    descriptors: Vec<PluginDescriptor>,
}

impl<const BROKEN: bool> PluginFactoryImpl for Plugins<BROKEN> {
    fn plugin_count(&self) -> u32 {
        self.descriptors.len() as u32
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        self.descriptors.get(index as usize)
    }

    fn create_plugin<'a>(&'a self, host_info: HostInfo<'a>, plugin_id: &CStr) -> Option<PluginInstance<'a>> {
        let descriptor = self.descriptors.iter().find(|descriptor| descriptor.id() == Some(plugin_id))?;
        fn instance<'a, P: DefaultPluginFactory>(
            host: HostInfo<'a>,
            descriptor: &'a PluginDescriptor,
        ) -> PluginInstance<'a> {
            PluginInstance::new::<P>(host, descriptor, P::new_shared, P::new_main_thread)
        }
        let wanted = plugin_id.to_str().ok()?;
        Some(match wanted {
            wanted if wanted == id(kind::PRESETS) => instance::<Scripted<{ kind::PRESETS }>>(host_info, descriptor),
            wanted if wanted == id(kind::LATE_PRESETS) => {
                instance::<Scripted<{ kind::LATE_PRESETS }>>(host_info, descriptor)
            }
            _ => instance::<Scripted<{ kind::BROKEN_PRESETS }>>(host_info, descriptor),
        })
    }
}

/// The preset providers of a scripted preset file. A `BROKEN` file has a
/// second provider, which fails to start.
struct Providers<const BROKEN: bool> {
    descriptors: Vec<ProviderDescriptor>,
}

impl<const BROKEN: bool> PresetDiscoveryFactoryImpl for Providers<BROKEN> {
    fn provider_count(&self) -> u32 {
        self.descriptors.len() as u32
    }

    fn provider_descriptor(&self, index: u32) -> Option<&ProviderDescriptor> {
        self.descriptors.get(index as usize)
    }

    fn create_provider<'a>(&'a self, indexer: IndexerInfo<'a>, provider_id: &CStr) -> Option<ProviderInstance<'a>> {
        let index = self.descriptors.iter().position(|descriptor| descriptor.id() == Some(provider_id))?;
        Some(ProviderInstance::new(indexer, &self.descriptors[index], move |mut indexer: Indexer<'a>| {
            if index > 0 {
                return Err(PluginError::Message("scripted to fail when started"));
            }
            let file_type = |name, extension| FileType { name, description: None, file_extension: Some(extension) };
            indexer.declare_filetype(file_type(c"Patch", c"patch"))?;
            // With the dot and in capitals, as plugins also write it.
            indexer.declare_filetype(file_type(c"Bank", c".BANK"))?;
            let path = CString::new(folder().to_string_lossy().as_bytes()).expect("a path has no NUL in it");
            indexer.declare_location(LocationInfo {
                name: c"Factory",
                flags: Flags::IS_FACTORY_CONTENT,
                location: Location::File { path: &path },
            })?;
            indexer.declare_location(LocationInfo {
                name: c"Built in",
                flags: Flags::empty(),
                location: Location::Plugin,
            })?;
            indexer.declare_soundpack(Soundpack {
                flags: Flags::empty(),
                id: c"pack-1",
                name: c"Deep Space",
                description: None,
                homepage_url: None,
                vendor: None,
                image_path: None,
                release_timestamp: None,
            })?;
            Ok(Provider { flaked: false })
        }))
    }
}

struct Provider {
    /// Whether it has failed to read the file it fails on once.
    flaked: bool,
}

/// Says of the preset being described that it is for the scripted
/// plugins with presets, all of them.
fn ours(receiver: &mut MetadataReceiver) {
    for plugin in [kind::PRESETS, kind::LATE_PRESETS, kind::BROKEN_PRESETS] {
        let id = CString::new(id(plugin)).expect("an id has no NUL in it");
        receiver.add_plugin_id(UniversalPluginId { abi: c"clap", id: &id });
    }
}

impl<'a> ProviderImpl<'a> for Provider {
    fn get_metadata(&mut self, location: Location, receiver: &mut MetadataReceiver) -> Result<(), PluginError> {
        let Some(path) = location.file_path() else {
            for (name, key, _) in BUILT_IN {
                let key = CString::new(key).expect("a key has no NUL in it");
                ours(receiver.begin_preset(Some(name), Some(&key))?.add_feature(c"synth").add_feature(c"synth"));
            }
            return Ok(());
        };
        let path = PathBuf::from(path.to_string_lossy().into_owned());
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                receiver.on_error(error.raw_os_error().unwrap_or(0), Some(c"the file cannot be read"));
                return Ok(());
            }
        };
        if path.file_stem().is_some_and(|stem| stem == "Flaky") && !std::mem::replace(&mut self.flaked, true) {
            receiver.on_error(0, Some(c"Error reading Attributes."));
            return Ok(());
        }
        if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("bank")) {
            // One file, several presets, each found again by its key.
            for (name, _) in text.lines().filter_map(|line| line.split_once('=')) {
                let name = CString::new(name).expect("a name has no NUL in it");
                let preset = receiver.begin_preset(Some(&name), Some(&name))?;
                ours(preset.set_soundpack_id(c"pack-1").set_flags(Flags::IS_USER_CONTENT | Flags::IS_FAVORITE));
            }
        } else if text == "other" {
            receiver
                .begin_preset(Some(c"Theirs"), None)?
                .add_plugin_id(UniversalPluginId { abi: c"clap", id: c"test.somebody-else" });
        } else {
            // Named by its file, which is the host's to work out.
            let preset = receiver.begin_preset(None, None)?;
            ours(preset.add_creator(c"Ada").add_creator(c"Bo").add_feature(c"pad"));
            if let Some(description) = text.lines().nth(1) {
                let description = CString::new(description).expect("a description has no NUL in it");
                receiver.set_description(&description);
            }
        }
        Ok(())
    }
}

/// The entry of a scripted plugin file with presets: two factories, one
/// for its plugins and one for its preset providers.
pub(crate) struct PresetsEntry<const BROKEN: bool> {
    plugins: PluginFactoryWrapper<Plugins<BROKEN>>,
    providers: PresetDiscoveryFactoryWrapper<Providers<BROKEN>>,
}

impl<const BROKEN: bool> Entry for PresetsEntry<BROKEN> {
    fn new(_plugin_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        let plugins = if BROKEN {
            vec![Scripted::<{ kind::BROKEN_PRESETS }>::get_descriptor()]
        } else {
            vec![Scripted::<{ kind::PRESETS }>::get_descriptor(), Scripted::<{ kind::LATE_PRESETS }>::get_descriptor()]
        };
        let mut providers = vec![ProviderDescriptor::new("test.presets.provider", "Scripted presets")];
        if BROKEN {
            providers.push(ProviderDescriptor::new("test.presets.broken", "Scripted to fail"));
        }
        debug_assert!(BROKEN || plugins.len() == PLUGINS.len());
        Ok(Self {
            plugins: PluginFactoryWrapper::new(Plugins { descriptors: plugins }),
            providers: PresetDiscoveryFactoryWrapper::new(Providers { descriptors: providers }),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.plugins);
        builder.register_factory(&self.providers);
    }
}
