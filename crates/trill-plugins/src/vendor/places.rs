//! Where on a machine vendors put what belongs to their plugins.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The folders that vendors keep their plugins' presets under: each
/// vendor has its own corner of one of them. Nothing is assumed to be
/// there: a folder that is missing is a product that is not installed, or
/// keeps its presets elsewhere, and yields no presets.
///
/// [`VendorPlaces::system`] is this machine's; a test names folders of its
/// own, and [`VendorPlaces::default`] names none, so that nothing of the
/// user's is read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorPlaces {
    /// The user's documents, wherever the system keeps them.
    pub documents: Vec<PathBuf>,
    /// Where programs keep what all users share (`C:\ProgramData`).
    pub shared_data: Vec<PathBuf>,
    /// Where programs are installed (`C:\Program Files` and its 32-bit
    /// twin): some keep their factory content beside themselves.
    pub programs: Vec<PathBuf>,
}

impl VendorPlaces {
    /// The places of the machine this runs on.
    pub fn system() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
        let mut places = Self::default();
        if cfg!(windows) {
            let documents = crate::vst3::documents_folder().or_else(|| Some(var("USERPROFILE")?.join("Documents")));
            places.documents.extend(documents);
            places.shared_data.extend(var("PROGRAMDATA"));
            places.programs.extend(["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"].into_iter().filter_map(var));
        } else {
            places.documents.extend(var("HOME").map(|home| home.join("Documents")));
        }
        for folders in [&mut places.documents, &mut places.shared_data, &mut places.programs] {
            let mut seen = std::collections::HashSet::new();
            folders.retain(|folder| seen.insert(folder.clone()));
        }
        places
    }
}
