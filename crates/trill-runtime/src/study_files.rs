//! The files a song plays out of studies.
//!
//! A clip taken from a study plays a file in the study's own folder, which
//! is Trill's and not the song's. So before a study is removed, or its
//! stems dropped, a front end asks which songs play them
//! ([`Runtime::study_used_by`]); and a song that is to go to another
//! machine is given what it plays of studies in a folder of its own beside
//! it ([`Runtime::gather_from_studies`]), cut to what it uses.

use crate::Runtime;
use crate::taking::Cut;
use std::path::{Path, PathBuf};
use trill_core::files::{FilePlace, Relink, below};
use trill_core::project::ClipContent;
use trill_core::{ClipId, Project};
use trill_dsp::SampleFiles;

/// How much of a recording is kept on each side of what a clip plays when
/// it is cut out for the song, in seconds: room to draw the clip out a
/// little, and for a fade.
const HANDLE_SECONDS: f64 = 1.0;

/// A song that plays files of a study.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsedBy {
    /// The song's name.
    pub song: String,
    /// Where it is saved; `None` for the open song that never was.
    pub path: Option<PathBuf>,
    /// Whether it is the song that is open.
    pub open: bool,
    /// How many of its clips and samples play files of the study.
    pub places: usize,
}

/// What [`Runtime::gather_from_studies`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Gathered {
    /// The folder beside the song that now holds what it plays of studies.
    pub folder: PathBuf,
    /// The files written there, and their size in bytes.
    pub files: usize,
    pub bytes: u64,
    /// The clips and the samples that now play from there.
    pub clips: usize,
    pub samples: usize,
}

/// How many places of `project` name a file below `folder`.
fn places_below(project: &Project, folder: &Path) -> usize {
    let folder = folder.display().to_string();
    project.files(&SampleFiles).iter().filter(|used| below(&folder, &used.path).is_some()).count()
}

impl Runtime {
    /// The songs Trill knows of that play files of the study `id`: the one
    /// that is open, and the ones it opened lately. With `stems` only the
    /// study's stems are asked after, which is what dropping them takes
    /// away; what was cut from them for a song stays.
    pub fn study_used_by(&self, id: &str, stems: bool) -> Result<Vec<UsedBy>, String> {
        let desk = self.studies()?;
        let mut folder = desk.studies().folder(id).path;
        if stems {
            folder.push("stems");
        }
        let mut used = Vec::new();
        let open = {
            let session = self.session.lock();
            let places = places_below(session.project(), &folder);
            if places > 0 {
                used.push(UsedBy {
                    song: session.project().name.clone(),
                    path: session.path().map(Path::to_path_buf),
                    open: true,
                    places,
                });
            }
            session.path().map(Path::to_path_buf)
        };
        for path in self.settings().recent_projects {
            if open.as_deref() == Some(path.as_path()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok((project, _)) = trill_core::files::read(&text) else { continue };
            let places = places_below(&project, &folder);
            if places > 0 {
                used.push(UsedBy { song: project.name.clone(), path: Some(path), open: false, places });
            }
        }
        Ok(used)
    }

    /// Removes the stems of the study `id` and keeps everything that was
    /// found in them. Answers with the bytes that came free. A study that
    /// is being made is not touched.
    pub fn drop_study_stems(&self, id: &str) -> Result<u64, String> {
        let desk = self.studies()?;
        let report = desk.report(id)?;
        if matches!(report.study.state, trill_study::State::Running | trill_study::State::Waiting) {
            return Err(format!("the study {id} is being made: stop it, or let it end, before dropping its stems"));
        }
        let folder = desk.studies().folder(id);
        let before = folder.size();
        folder.drop_stems()?;
        Ok(before.saturating_sub(folder.size()))
    }

    /// Gives the open song what it plays of studies in a folder of its own
    /// beside it, `<song> files`: each clip's stretch of a stem cut out
    /// with a second to spare on each side, each sample whole, and the
    /// song playing them from there. One undo step. The song has to be
    /// saved, and is to be saved again after: its file then says where
    /// these lie from its own folder, so that the two can be moved together.
    pub fn gather_from_studies(&self) -> Result<Gathered, String> {
        let root = self.studies()?.folder().display().to_string();
        let (song, uses) = {
            let session = self.session.lock();
            let Some(song) = session.path().map(Path::to_path_buf) else {
                return Err(
                    "the song has not been saved: save it first, and what it plays of studies is put beside it"
                        .to_owned(),
                );
            };
            let uses: Vec<_> = session
                .project()
                .files(&SampleFiles)
                .into_iter()
                .filter(|used| below(&root, &used.path).is_some())
                .collect();
            (song, uses)
        };
        let stem = song.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
        let folder = song.with_file_name(format!("{stem} files"));
        let mut gathered = Gathered { folder: folder.clone(), files: 0, bytes: 0, clips: 0, samples: 0 };
        if uses.is_empty() {
            return Ok(gathered);
        }
        // What each place is to play instead: a clip its own cut and where in it it begins, a sample its copy.
        let mut clips: Vec<(ClipId, String, f64)> = Vec::new();
        let mut relinks: Vec<Relink> = Vec::new();
        let mut written: Vec<PathBuf> = Vec::new();
        for used in &uses {
            let from = PathBuf::from(&used.path);
            // The study's folder and the file's own place in it name the copy, so that two studies' drums are two files.
            let inside = below(&root, &used.path).unwrap_or(&used.path);
            let named: String =
                inside.chars().map(|c| if c.is_alphanumeric() || "-_+. ".contains(c) { c } else { '_' }).collect();
            let named = named.trim_end_matches(".wav").to_owned();
            match (&used.place, used.reach_seconds) {
                (FilePlace::Clip(clip), Some(reach)) => {
                    let offset = {
                        let session = self.session.lock();
                        let found = session.project().clip(*clip).map_err(|error| error.to_string())?;
                        found.audio().map_or(0.0, |audio| audio.offset_seconds)
                    };
                    let seconds =
                        self.sound_facts(&from).map_err(|error| format!("{}: {error}", from.display()))?.seconds;
                    let (start, end) = ((offset - HANDLE_SECONDS).max(0.0), (reach + HANDLE_SECONDS).min(seconds));
                    let file = folder.join(format!("{named} {:.3}-{:.3}.wav", start, end));
                    if !file.is_file() {
                        self.make_cut(&file, &Cut { stems: vec![from], from: start, to: end, hit: false })?;
                    }
                    clips.push((*clip, file.display().to_string(), offset - start));
                    written.push(file);
                }
                (place, _) => {
                    let file = folder.join(format!("{named}.wav"));
                    if !file.is_file() {
                        std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
                        std::fs::copy(&from, &file).map_err(|error| format!("{}: {error}", from.display()))?;
                    }
                    relinks.push(Relink { place: place.clone(), path: file.display().to_string() });
                    written.push(file);
                }
            }
        }
        written.sort();
        written.dedup();
        gathered.files = written.len();
        gathered.bytes = written.iter().filter_map(|file| std::fs::metadata(file).ok()).map(|file| file.len()).sum();
        (gathered.clips, gathered.samples) = (clips.len(), relinks.len());
        let mut session = self.session.lock();
        session
            .edit(|project| {
                for (clip, path, offset) in &clips {
                    if let ClipContent::Audio(audio) = &mut project.clip_mut(*clip)?.content {
                        audio.path.clone_from(path);
                        audio.offset_seconds = *offset;
                    }
                }
                project.relink(&relinks, &SampleFiles)?;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        Ok(gathered)
    }
}
