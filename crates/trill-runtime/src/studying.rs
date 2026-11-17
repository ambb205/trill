//! Studies of recordings, and everything Trill fetches: built once, for
//! both front ends.
//!
//! A *study* takes a recording apart (`trill-study`): its stems, beats and
//! words found by models in a process of its own, and everything
//! `trill-listen` hears in the stems. The runtime holds the desk studies
//! are asked for at ([`Runtime::studies`]) and the engine that fetches
//! what Trill does not ship ([`Runtime::fetches`]): the models' parts are
//! its first list, and whoever else has things to fetch adds a list of
//! their own to the same engine (`trill_fetch::Fetches::add`), which both
//! front ends then show and fetch from.
//!
//! Two things only a program that is really one may do, and neither is on
//! until it says so ([`Runtime::study_and_fetch_as_usual`], which `trill`
//! and `trill-mcp` call when they start): start itself a second time to
//! study, and fetch with the system's `curl`. A runtime made by a test
//! does neither unless the test hands it a program that studies and a
//! shelf to fetch from ([`Studying`]).
//!
//! Where nothing is kept between runs ([`Storage::Nowhere`]) there are no
//! studies: one is a folder on disk.

use crate::{Runtime, Storage};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_fetch::{Curl, Fetcher, Fetches, Nothing, Part};
use trill_library::Settings;
use trill_study::models::scripted::Script;
use trill_study::run::Programs;
use trill_study::setup::{self, MODELS, Setup};
use trill_study::{Desk, DeskOptions, Lyrics, Report, StudyFolder};

/// The folders, in the program's data folder, that studies and the parts
/// they are made with are kept in unless the settings say otherwise.
pub const STUDIES_FOLDER: &str = "studies";
pub const MODELS_FOLDER: &str = "models";

/// What [`crate::Parts`] can hand in place of the real thing for studies.
/// Everything left out is as a runtime has it by itself.
#[derive(Default)]
pub struct Studying {
    /// Who fetches. Nobody, unless the program says it is one
    /// ([`Runtime::study_and_fetch_as_usual`]).
    pub fetcher: Option<Arc<dyn Fetcher>>,
    /// The parts of studies, in place of the ones that are published.
    pub parts: Option<Vec<Part>>,
    /// Scripted models in place of the real ones, which then need no part.
    pub stand_in: Option<Script>,
    /// The programs that are taken to be installed, in place of looking.
    pub programs: Option<Programs>,
    /// The program that is started to study.
    pub program: Option<PathBuf>,
    /// The memory that is free, in place of asking the machine.
    pub free: Option<fn() -> Option<u64>>,
}

/// What a runtime holds of studies.
pub(crate) struct Studies {
    fetches: Arc<Fetches>,
    desk: Desk,
    /// Where studies are kept when the settings do not say.
    default_studies: PathBuf,
    /// Where the parts of each list are kept when the settings do not
    /// say: the place its owner gave it.
    defaults: Mutex<HashMap<String, PathBuf>>,
}

impl Studies {
    /// The studies of a runtime that keeps its settings in
    /// `settings_folder`, or nothing where nothing is kept.
    pub fn open(
        storage: &Storage,
        settings: &Settings,
        settings_folder: Option<&Path>,
        studying: Studying,
    ) -> Option<Self> {
        let data = match storage {
            Storage::User => trill_library::data_dir()?,
            Storage::At { .. } => settings_folder?.to_owned(),
            Storage::Nowhere => return None,
        };
        let (default_studies, default_parts) = (data.join(STUDIES_FOLDER), data.join(MODELS_FOLDER));
        let fetches = Arc::new(Fetches::new(studying.fetcher.unwrap_or_else(|| Arc::new(Nothing))));
        let mut list = setup::list(settings.fetched.get(MODELS).cloned().unwrap_or_else(|| default_parts.clone()));
        if let Some(parts) = studying.parts {
            list.parts = parts;
        }
        if let Err(why) = fetches.add(list) {
            log::warn!("the parts of studies are not offered: {why}");
        }
        let setup = Arc::new(Setup::over(fetches.clone(), studying.stand_in, studying.programs));
        let desk = Desk::new(DeskOptions {
            studies: settings.studies.folder.clone().unwrap_or_else(|| default_studies.clone()),
            setup,
            program: studying.program,
            free: studying.free,
        });
        let defaults = Mutex::new([(MODELS.to_owned(), default_parts)].into_iter().collect());
        Some(Self { fetches, desk, default_studies, defaults })
    }
}

/// What is said where nothing is kept.
const NOT_KEPT: &str = "this run keeps nothing on disk, and a study is a folder on disk: start Trill without --scratch, or with --profile <folder>, to study a recording";

/// Moves the folders `names` from `from` into `to`, and answers with how
/// many went. One that is already there under its name stays where it is.
fn move_folders(from: &Path, to: &Path, names: &[String]) -> Result<usize, String> {
    fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    let mut moved = 0;
    for name in names {
        let (source, target) = (from.join(name), to.join(name));
        if !source.exists() || target.exists() {
            continue;
        }
        // In one step on the same disk; copied and then removed across disks.
        if std::fs::rename(&source, &target).is_err() {
            let said = |error: std::io::Error| format!("{}: {error}", target.display());
            if source.is_dir() {
                copy(&source, &target).map_err(said)?;
                std::fs::remove_dir_all(&source).map_err(said)?;
            } else {
                std::fs::copy(&source, &target).map_err(said)?;
                std::fs::remove_file(&source).map_err(said)?;
            }
        }
        moved += 1;
    }
    Ok(moved)
}

impl Runtime {
    /// The desk studies are asked for at, or a sentence where this run
    /// keeps nothing.
    pub fn studies(&self) -> Result<&Desk, String> {
        self.studying.as_ref().map(|studies| &studies.desk).ok_or_else(|| NOT_KEPT.to_owned())
    }

    /// The engine that fetches what Trill does not ship: every list there
    /// is, how each part is, fetching and removing. Whoever has things to
    /// fetch adds a list to it.
    pub fn fetches(&self) -> Result<&Arc<Fetches>, String> {
        self.studying.as_ref().map(|studies| &studies.fetches).ok_or_else(|| NOT_KEPT.to_owned())
    }

    /// Has this program start itself a second time to study, and fetch
    /// with the system's `curl`, as a program does that has called
    /// [`crate::jobs::run_as_job_if_requested`] first thing. A runtime
    /// that was not told so studies nothing and fetches nothing.
    pub fn study_and_fetch_as_usual(&self) {
        let Some(studies) = &self.studying else { return };
        studies.fetches.fetch_by(Arc::new(Curl));
        match std::env::current_exe() {
            Ok(program) => studies.desk.study_by(program),
            Err(error) => log::warn!("nothing can be studied: this program's own file cannot be found ({error})"),
        }
    }

    /// Keeps studies in `folder` from now on (`None`: the program's own
    /// data folder), moves the ones there are across, and says what was
    /// done. Slow across disks: not for the thread that draws.
    pub fn keep_studies_in(&self, folder: Option<PathBuf>) -> Result<String, String> {
        let studies = self.studying.as_ref().ok_or_else(|| NOT_KEPT.to_owned())?;
        let (from, to) = (studies.desk.folder(), folder.clone().unwrap_or_else(|| studies.default_studies.clone()));
        if from == to {
            return Ok(format!("Studies are kept in {} already", to.display()));
        }
        if studies.desk.busy() {
            return Err("a study is running: stop it, or wait for it, before studies are kept elsewhere".to_owned());
        }
        let names: Vec<String> = studies.desk.list().into_iter().map(|report| report.study.id).collect();
        let moved = move_folders(&from, &to, &names)?;
        studies.desk.set_folder(to.clone())?;
        self.update_settings(|settings| settings.studies.folder = folder).map_err(|error| error.to_string())?;
        Ok(match moved {
            0 => format!("Studies are kept in {} from now on", to.display()),
            1 => format!("Studies are kept in {} from now on; the one there was moved there", to.display()),
            count => format!("Studies are kept in {} from now on; the {count} there were moved there", to.display()),
        })
    }

    /// Adds a list of things to fetch to the ones both front ends show
    /// and fetch from: for whoever has some (the models of studies are
    /// there already). `list.folder` is where its parts are kept unless
    /// the user chose another place, which the settings remember.
    pub fn add_fetch_list(&self, mut list: trill_fetch::List) -> Result<(), String> {
        let studies = self.studying.as_ref().ok_or_else(|| NOT_KEPT.to_owned())?;
        studies.defaults.lock().insert(list.id.clone(), list.folder.clone());
        if let Some(chosen) = self.settings().fetched.get(&list.id) {
            list.folder = chosen.clone();
        }
        studies.fetches.add(list)
    }

    /// Keeps the parts of the list `id` in `folder` from now on (`None`:
    /// the place its owner gave it), moves what was fetched across, tells
    /// its owner of each part that moved, and says what was done. Slow
    /// across disks: not for the thread that draws.
    pub fn keep_list_in(&self, id: &str, folder: Option<PathBuf>) -> Result<String, String> {
        let studies = self.studying.as_ref().ok_or_else(|| NOT_KEPT.to_owned())?;
        let Some(from) = studies.fetches.folder_of(id) else {
            return Err(format!("there is no list '{id}'. Lists: {}", studies.fetches.list_ids().join(", ")));
        };
        let own = studies.defaults.lock().get(id).cloned().unwrap_or_else(|| from.clone());
        let to = folder.clone().unwrap_or(own);
        if from == to {
            return Ok(format!("They are kept in {} already", to.display()));
        }
        if id == MODELS && studies.desk.busy() {
            return Err("a study is running: stop it, or wait for it, before its parts are kept elsewhere".to_owned());
        }
        let mut names: Vec<String> = studies
            .fetches
            .status()
            .into_iter()
            .filter(|list| list.id == id)
            .flat_map(|list| list.parts)
            .map(|status| status.part.id)
            .collect();
        if id == MODELS {
            // What the timed test found is kept beside the parts it was run on.
            names.push("machine.json".to_owned());
        }
        // Refused while one of them is being fetched; what is moved is whole.
        studies.fetches.set_folder(id, to.clone())?;
        let moved = match move_folders(&from, &to, &names) {
            Ok(moved) => moved,
            Err(why) => {
                let _ = studies.fetches.set_folder(id, from);
                return Err(why);
            }
        };
        studies.fetches.moved(id, &from);
        self.update_settings(|settings| {
            match folder {
                Some(folder) => settings.fetched.insert(id.to_owned(), folder),
                None => settings.fetched.remove(id),
            };
        })
        .map_err(|error| error.to_string())?;
        Ok(match moved {
            0 => format!("What is fetched is kept in {} from now on", to.display()),
            _ => format!(
                "What is fetched is kept in {} from now on; what was fetched before was moved there",
                to.display()
            ),
        })
    }

    /// The study `id` with what was heard in it and its words, as its
    /// folder holds them: nothing is run. What a study that is not done
    /// yet lacks is `None`.
    pub fn study(&self, id: &str) -> Result<Studied, String> {
        let desk = self.studies()?;
        let report = desk.report(id)?;
        let folder = desk.studies().folder(id);
        Ok(Studied { heard: folder.heard().map(Arc::new), lyrics: folder.lyrics(), folder, report })
    }
}

/// A study as its folder holds it.
pub struct Studied {
    pub report: Report,
    pub folder: StudyFolder,
    /// What was heard in its stems, once that is done.
    pub heard: Option<Arc<trill_listen::Heard>>,
    /// Its words in sung lines, once they are placed.
    pub lyrics: Option<Lyrics>,
}

impl Studied {
    /// The outline of each stem, for drawing them without reading them.
    pub fn outlines(&self) -> Option<trill_study::store::Outlines> {
        self.folder.outlines()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Parts, RuntimeOptions};
    use trill_engine::AudioBackend;

    fn runtime(folder: &Path, studying: Studying) -> Runtime {
        let storage = Storage::At { settings: folder.join("config").join("settings.json"), library_index: None };
        let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
        Runtime::start_custom(options, Parts { studying, ..Parts::default() }).unwrap()
    }

    #[test]
    fn a_run_that_keeps_nothing_has_no_studies_and_says_so() {
        let runtime = Runtime::start(RuntimeOptions::hermetic()).unwrap();
        let refused = runtime.studies().err().unwrap();
        assert!(refused.starts_with("this run keeps nothing on disk, and a study is a folder on disk"), "{refused}");
        assert!(runtime.fetches().is_err() && runtime.study("abc").is_err());
        runtime.study_and_fetch_as_usual();
    }

    #[test]
    fn studies_and_their_parts_are_kept_beside_the_settings_and_moved_when_asked() {
        let folder = tempfile::tempdir().unwrap();
        let runtime = runtime(folder.path(), Studying::default());
        let config = folder.path().join("config");
        let desk = runtime.studies().unwrap();
        assert_eq!(desk.folder(), config.join("studies"));
        assert_eq!(runtime.fetches().unwrap().folder_of(MODELS), Some(config.join("models")));
        // A runtime that was not told it is a program studies nothing and fetches nothing.
        assert_eq!(desk.program(), None);

        // What is there goes along: a study, a part, and what the timed test found.
        let study = desk
            .studies()
            .create("x", trill_study::Source::File { path: "x.wav".into() }, Default::default())
            .unwrap()
            .1;
        std::fs::create_dir_all(config.join("models").join("beats")).unwrap();
        std::fs::write(config.join("models").join("beats").join("beat_this.onnx"), b"model").unwrap();
        std::fs::write(config.join("models").join("machine.json"), b"{}").unwrap();
        let elsewhere = folder.path().join("big disk");
        let said = runtime.keep_studies_in(Some(elsewhere.join("studies"))).unwrap();
        assert!(said.ends_with("from now on; the one there was moved there"), "{said}");
        assert!(elsewhere.join("studies").join(&study.id).join("study.json").is_file());
        assert!(!config.join("studies").join(&study.id).exists());
        assert_eq!(desk.list().len(), 1);
        let said = runtime.keep_list_in(MODELS, Some(elsewhere.join("models"))).unwrap();
        assert!(said.ends_with("what was fetched before was moved there"), "{said}");
        assert_eq!(runtime.settings().fetched.get(MODELS), Some(&elsewhere.join("models")));
        assert_eq!(runtime.keep_list_in("pianos", None).unwrap_err(), "there is no list 'pianos'. Lists: models");
        assert!(elsewhere.join("models").join("beats").join("beat_this.onnx").is_file());
        assert!(
            elsewhere.join("models").join("machine.json").is_file() && !config.join("models").join("beats").exists()
        );

        // The settings remember it, and the next run finds them there.
        assert_eq!(runtime.settings().studies.folder, Some(elsewhere.join("studies")));
        drop(runtime);
        let again = self::runtime(folder.path(), Studying::default());
        assert_eq!(again.studies().unwrap().list().len(), 1);
        assert_eq!(again.fetches().unwrap().folder_of(MODELS), Some(elsewhere.join("models")));
        // And back to the program's own folder.
        assert!(again.keep_studies_in(None).unwrap().contains("was moved there"));
        assert_eq!(again.settings().studies.folder, None);
        assert!(config.join("studies").join(&study.id).is_dir());
        assert!(again.keep_studies_in(None).unwrap().ends_with("already"));
    }
}
