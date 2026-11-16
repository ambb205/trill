//! The settings file as the running program holds it.
//!
//! [`trill_library::Settings`] knows how to read and write the file safely.
//! This adds what a running program needs on top: which file (or none),
//! the copy in memory, and the rule for who owns what. The library's roots,
//! favorites and recently used files live in the [`Library`] while the
//! program runs, so they are copied from it on every save; everything else
//! lives here.
//!
//! Two programs share the file (the window and the headless MCP server may
//! even run at once), so a save never writes back the copy this process
//! read at startup: it reads the file again, changes its own part and
//! writes that. Whoever saved last wins for the part they both own, and
//! nobody loses the parts only the other touches.

use std::path::PathBuf;
use trill_library::{Library, LibrarySettings, SaveError, Settings};

/// Where the settings and the library index are kept between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Storage {
    /// The user's own: `settings.json` in the per-user configuration
    /// folder and the library index in the per-user cache folder.
    #[default]
    User,
    /// These files. For tests, which must never see or change the user's.
    At {
        settings: PathBuf,
        /// Where the library index is saved; `None` keeps it in memory.
        library_index: Option<PathBuf>,
    },
    /// Nowhere: defaults at startup, nothing written, an empty library that
    /// is never scanned unless a folder is added to it.
    Nowhere,
}

pub(crate) struct SettingsFile {
    /// `None` when nothing is kept on disk.
    path: Option<PathBuf>,
    current: Settings,
    /// The library's part as of the last attempt to write it, to tell when
    /// it needs writing. An attempt, not a success: a disk that cannot be
    /// written is reported once per change, not once a second.
    written_library: LibrarySettings,
    warning: Option<String>,
}

impl SettingsFile {
    /// Reads the settings `storage` names and says where the library index
    /// belongs.
    pub fn open(storage: &Storage) -> (Self, Option<PathBuf>) {
        let (path, index) = match storage {
            Storage::User => match Settings::default_path() {
                Some(path) => (Some(path), Library::default_index_path()),
                None => {
                    log::warn!("this system names no home folder; settings will not be kept");
                    (None, None)
                }
            },
            Storage::At { settings, library_index } => (Some(settings.clone()), library_index.clone()),
            Storage::Nowhere => (None, None),
        };
        let loaded = path.as_deref().map(Settings::load);
        let (current, warning) = loaded.map_or_else(Default::default, |loaded| (loaded.settings, loaded.warning));
        (Self { path, written_library: current.library.clone(), current, warning }, index)
    }

    /// The settings as read or last saved.
    pub fn current(&self) -> &Settings {
        &self.current
    }

    /// Whether the settings are kept on disk at all.
    pub fn is_kept(&self) -> bool {
        self.path.is_some()
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// The settings with the library's part as the library has it now.
    pub fn with_library(&self, library: &Library) -> Settings {
        Settings { library: library.settings(), ..self.current.clone() }
    }

    /// Whether the library's part has changed since it was last written.
    pub fn library_changed(&self, library: &Library) -> bool {
        self.is_kept() && library.settings() != self.written_library
    }

    /// Applies `change` to the settings and writes them, with the library's
    /// part taken from `library`. The rest starts from what is on disk now,
    /// not from what this process read earlier; see the module notes.
    pub fn update(&mut self, library: &Library, change: impl FnOnce(&mut Settings)) -> Result<(), SaveError> {
        let Some(path) = &self.path else {
            change(&mut self.current);
            return Ok(());
        };
        let mut settings = Settings::load(path).settings;
        change(&mut settings);
        settings.library = library.settings();
        self.written_library = settings.library.clone();
        settings.save(path)?;
        self.current = settings;
        Ok(())
    }
}
