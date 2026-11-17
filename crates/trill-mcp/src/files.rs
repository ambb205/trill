//! Tools for a song whose files are not where it says.
//!
//! A song names its audio files by their paths, and a folder that was
//! moved leaves it with clips and pads that play nothing. A model cannot
//! hear that; it has to be told, and told in one place:
//!
//! * [`brief`] is what `get_project` carries (`missing_files`) and what
//!   `measure` and `render` say first, so that a mix that came out quieter
//!   is never handed back without a word. The line for every single clip
//!   that the session's problems hold is left out there in its favour
//!   ([`without_missing`]).
//! * `missing_files` lists what is missing, every place that names it, and
//!   what was found for each with how sure it is and why
//!   (`trill_runtime::Runtime::missing_files` does the looking).
//! * `relink_files` puts the song right in one undo step: what was found
//!   for sure, or more on request, or what the caller names by hand.

use crate::server::{Reply, TrillServer, explain, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use trill_core::files::{FilePlace, FileUse, FolderMove, Relink, file_name};
use trill_core::{ClipId, DeviceId};
use trill_runtime::{Candidate, Confidence, Look, Missing, MissingFile, Runtime};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Files that are missing
- A song names its audio files by their paths: every audio clip one, and every sample of a sampler or a drum \
rack. When a folder of samples was moved or renamed, those places play nothing. get_project says so in \
`missing_files`, and measure and render say it first in their notes: what they measured is then the song \
without those files, not a quieter mix.
- missing_files lists each missing file with the places that name it and what Trill found for it: the file where \
its folder went, or a file of that name in the sound library's folders. Each thing found says how sure it is \
(`sure`, `likely`, `unsure`) and why. A file of the same name can be another file: only `sure` means it is where \
the file's folder went or has the size and length the song wrote down.
- relink_files puts the song right in one undo step. With no arguments it relinks what is `sure`. Take what is \
`likely` or `unsure` only after reading why, and say to the user which files you took on a name alone. When \
nothing was found, ask the user where the folder went and give it as `folder` (from, to) or `look_in`, or one \
file as `files`.
- The sound library's own folders can be missing too: `library` status lists them (`missing_folders`) and \
missing_files says where each seems to be. relink_files moves the library along when the same move explains \
both; library `move_folder` does it by hand. Favorites stay with their files.";

/// How many missing files a reply lists when the caller does not say, and
/// the most it can ask for.
const DEFAULT_FILES: usize = 40;
const MAX_FILES: usize = 200;
/// How long a look waits for a library scan that is running: a folder of
/// the library is only known to be missing once a scan has tried it.
const SCAN_WAIT: Duration = Duration::from_secs(8);

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MissingFilesRequest {
    /// More folders to look through for the missing files, however deep: whole paths. The sound library's
    /// folders, and the neighbourhood of where each file was, are looked through without being named.
    #[serde(default)]
    pub look_in: Vec<String>,
    /// The most files to list (default 40, at most 200). The counts in the reply are of all of them.
    pub limit: Option<usize>,
    /// How many files to skip, for the next page.
    pub offset: Option<usize>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelinkFound {
    /// Only what is sure: the file where its folder went, or one with the size and length the song wrote down.
    Sure,
    /// Also what is likely: the only file of that name, with nothing against it and nothing else for it.
    Likely,
    /// Everything that was found, the unsure too. Read why each is unsure first (missing_files): it may be
    /// another file of the same name.
    All,
    /// Nothing that was found: only what `files` and `folder` name.
    None,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RelinkOne {
    /// A missing file, by its path as the song has it (`file` in missing_files): every place that names it
    /// plays `to` instead.
    pub file: Option<String>,
    /// Or one audio clip, by id.
    pub clip: Option<u64>,
    /// Or one sample of a device: the device's id, with `slot`.
    pub device: Option<u64>,
    /// With `device`: which of its samples, as missing_files calls it (`sample` on a sampler, `pad 36` on a
    /// drum rack).
    pub slot: Option<String>,
    /// The whole path of the file to play instead. It has to be an audio file that can be read.
    pub to: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FolderWent {
    /// The folder as the song's paths have it.
    pub from: String,
    /// Where it is now.
    pub to: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RelinkFilesRequest {
    /// Which of the files Trill found to relink. Left out: \"sure\" when nothing else is given, \"none\" when
    /// `files` or `folder` is.
    pub found: Option<RelinkFound>,
    /// Files to relink by hand, each to the path given for it.
    #[serde(default)]
    pub files: Vec<RelinkOne>,
    /// A folder that went elsewhere: every missing file that was under `from` is relinked to the same place
    /// under `to`, if it is there.
    pub folder: Option<FolderWent>,
    /// More folders to look through, as for missing_files.
    #[serde(default)]
    pub look_in: Vec<String>,
    /// Whether the sound library is told where its own missing folders went, when they were found for sure or
    /// `folder` names one (default true). Favorites and recently used files stay with their files.
    pub library: Option<bool>,
}

fn count(number: usize, one: &str, many: &str) -> String {
    if number == 1 { format!("1 {one}") } else { format!("{number} {many}") }
}

fn names(tracks: &[String]) -> String {
    let named: Vec<&str> = tracks.iter().map(String::as_str).collect();
    trill_core::journal::words::list(&named)
}

/// What a song is missing, in a few lines: how many files, in how many
/// places, on which tracks, and what to do. `None` when nothing is.
pub(crate) fn brief(missing: &Missing) -> Option<Value> {
    if missing.files.is_empty() {
        return None;
    }
    let tracks = missing.tracks();
    Some(json!({
        "files": missing.files.len(),
        "places": missing.places(),
        "tracks": tracks,
        "note": format!(
            "{} the song names {} not on disk, so {} on {} {} nothing. missing_files lists them with what Trill \
             found for each; relink_files puts them right in one step.",
            count(missing.files.len(), "audio file", "audio files"),
            if missing.files.len() == 1 { "is" } else { "are" },
            count(missing.places(), "place", "places"),
            names(&tracks),
            if missing.places() == 1 { "plays" } else { "play" },
        ),
    }))
}

/// What a render or a measurement says first of a song that is not whole.
pub(crate) fn heard_without(missing: &Missing) -> Option<String> {
    (!missing.files.is_empty()).then(|| {
        format!(
            "{} missing, so {} on {} played nothing: what was measured is the song without them, not a quieter \
             mix. missing_files finds them and relink_files relinks them.",
            match missing.files.len() {
                1 => "1 audio file is".to_owned(),
                files => format!("{files} audio files are"),
            },
            count(missing.places(), "place", "places"),
            names(&missing.tracks()),
        )
    })
}

/// `problems` without the lines that are each about one place whose file
/// is missing: those are said once, by [`brief`].
pub(crate) fn without_missing(problems: Vec<String>, missing: &Missing) -> Vec<String> {
    missing.other_problems(problems)
}

fn use_json(used: &FileUse, track: &str) -> Value {
    let mut out = match &used.place {
        FilePlace::Clip(clip) => json!({ "clip": clip.0 }),
        FilePlace::Sample { device, slot } => json!({ "device": device.0, "slot": slot }),
    };
    out["name"] = json!(used.name);
    out["track"] = json!(track);
    out
}

fn candidate_json(candidate: &Candidate) -> Value {
    let mut out = json!({ "path": candidate.path, "sure": candidate.confidence.word(), "why": candidate.why });
    if let Some(seconds) = candidate.seconds {
        out["seconds"] = json!(round(seconds));
    }
    out
}

fn file_json(file: &MissingFile, project: &trill_core::Project) -> Value {
    let track = |used: &FileUse| match used.track.and_then(|id| project.track(id).ok()) {
        Some(track) => track.name.clone(),
        None => "Master".to_owned(),
    };
    let mut out = json!({
        "file": file.path,
        "used_by": file.uses.iter().map(|used| use_json(used, &track(used))).collect::<Vec<_>>(),
    });
    if let Some(known) = &file.known {
        let mut was = json!({});
        if let Some(seconds) = known.seconds() {
            was["seconds"] = json!(round(seconds));
        }
        if let Some(rate) = known.sample_rate {
            was["sample_rate"] = json!(rate);
        }
        if let Some(bytes) = known.bytes {
            was["bytes"] = json!(bytes);
        }
        out["was"] = was;
    }
    match file.candidates.as_slice() {
        [] => out["found"] = Value::Null,
        [best, others @ ..] => {
            out["found"] = candidate_json(best);
            if !others.is_empty() {
                out["also"] = others.iter().map(candidate_json).collect();
            }
        }
    }
    out
}

/// What was found, as a reply.
fn missing_json(missing: &Missing, project: &trill_core::Project, offset: usize, limit: usize) -> Value {
    let files = missing.files.len();
    let at_least = |sure| missing.found(sure);
    let (sure, likely, any) = (at_least(Confidence::Sure), at_least(Confidence::Likely), at_least(Confidence::Unsure));
    let mut out = json!({
        "missing": files,
        "places": missing.places(),
        "tracks": missing.tracks(),
        "found": { "sure": sure, "likely": likely - sure, "unsure": any - likely, "not_found": files - any },
        "files": missing.files.iter().skip(offset).take(limit).map(|file| file_json(file, project)).collect::<Vec<_>>(),
    });
    if offset + limit < files {
        out["more"] = json!(format!("{} more: ask again with offset {}", files - offset - limit, offset + limit));
    }
    if let Some(folder) = missing.found_in(Confidence::Sure) {
        out["found_in"] = json!(folder);
    }
    if !missing.moves.is_empty() {
        out["folders_moved"] = missing
            .moves
            .iter()
            .map(|(moved, files)| json!({ "from": moved.from, "to": moved.to, "files": files }))
            .collect();
    }
    if !missing.folders.is_empty() {
        out["library"] = missing
            .folders
            .iter()
            .map(|folder| {
                let mut out = json!({ "folder": folder.path.display().to_string(), "files_known": folder.files });
                match &folder.found {
                    Some(found) => {
                        out["found_at"] = json!(found.path.display().to_string());
                        out["sure"] = json!(found.sure());
                        out["why"] = json!(format!(
                            "{} of {} of its files that were looked for are there",
                            found.there, found.checked
                        ));
                    }
                    None => out["found_at"] = Value::Null,
                }
                out
            })
            .collect();
    }
    out["next"] = json!(match (files, sure, any) {
        (0, ..) if missing.folders.is_empty() => "No file of the song is missing.".to_owned(),
        (0, ..) => "No file of the song is missing. The sound library has a folder that is: library `move_folder` \
                    says where it went."
            .to_owned(),
        (files, sure, _) if sure == files => {
            format!("All {files} were found for sure: relink_files with no arguments relinks them in one undo step.")
        }
        (_, 0, 0) => "Nothing was found. Ask the user where the folder went, then give it to relink_files as \
                      `folder` (from, to) or to missing_files as `look_in`."
            .to_owned(),
        (files, sure, _) => format!(
            "relink_files with no arguments relinks the {sure} that {} sure. For the other {}, read `why`: take one \
             with `found` \"likely\" or \"all\", name a file by hand in `files`, or ask the user where they are.",
            if sure == 1 { "is" } else { "are" },
            files - sure
        ),
    });
    out
}

fn folders(given: &[String]) -> Vec<PathBuf> {
    let asked = given.iter().map(|folder| folder.trim()).filter(|folder| !folder.is_empty());
    asked.map(|folder| std::path::absolute(folder).unwrap_or_else(|_| PathBuf::from(folder))).collect()
}

/// Looks for what the song is missing, off the async threads. A library
/// scan that is running is waited for a little first.
async fn look(runtime: &std::sync::Arc<Runtime>, look: Look) -> Result<Missing, String> {
    let runtime = runtime.clone();
    let looking = tokio::task::spawn_blocking(move || {
        runtime.library.wait_until_idle(SCAN_WAIT);
        runtime.missing_files(&look)
    });
    looking.await.map_err(|error| format!("the look for the missing files did not finish: {error}"))
}

#[tool_router(router = files_router, vis = "pub(crate)")]
impl TrillServer {
    /// The audio files the song names that are not on disk (a folder of samples was moved or renamed), every
    /// clip and sample that names each, and what Trill found for it: the file where its folder went, or one
    /// of that name in the sound library, with how sure it is and why. Changes nothing; relink_files does.
    #[tool(annotations(title = "Missing files", read_only_hint = true))]
    async fn missing_files(&self, Parameters(request): Parameters<MissingFilesRequest>) -> CallToolResult {
        for folder in folders(&request.look_in) {
            if !folder.is_dir() {
                return finish(Err(format!("'{}' in `look_in` is not a folder on this machine", folder.display())));
            }
        }
        let asked = Look { folders: folders(&request.look_in), ..Look::default() };
        let reply = look(&self.runtime, asked).await.map(|missing| {
            let session = self.session.lock();
            let limit = request.limit.unwrap_or(DEFAULT_FILES).clamp(1, MAX_FILES);
            missing_json(&missing, session.project(), request.offset.unwrap_or(0), limit)
        });
        finish(reply)
    }

    /// Give the song's missing files back to it, in one undo step: each clip goes on playing the stretch of its
    /// file it played, each pad keeps its settings. With no arguments, relinks what missing_files found for
    /// sure. `found` takes more of what was found, `folder` says where a folder went, `files` names files by
    /// hand. Nothing on disk is touched.
    #[tool(annotations(title = "Relink files"))]
    async fn relink_files(&self, Parameters(request): Parameters<RelinkFilesRequest>) -> CallToolResult {
        finish(self.relink(request).await)
    }
}

impl TrillServer {
    async fn relink(&self, request: RelinkFilesRequest) -> Reply {
        let by_hand = !request.files.is_empty() || request.folder.is_some();
        let found = request.found.unwrap_or(if by_hand { RelinkFound::None } else { RelinkFound::Sure });
        let said = match &request.folder {
            None => None,
            Some(folder) if folder.from.trim().is_empty() || folder.to.trim().is_empty() => {
                return Err(
                    "`folder` needs both `from` (the folder as the song has it) and `to` (where it is now)".to_owned()
                );
            }
            Some(folder) if !Path::new(folder.to.trim()).is_dir() => {
                return Err(format!("'{}', the `to` of `folder`, is not a folder on this machine", folder.to.trim()));
            }
            Some(folder) => Some(FolderMove::new(folder.from.trim(), folder.to.trim())),
        };
        for folder in folders(&request.look_in) {
            if !folder.is_dir() {
                return Err(format!("'{}' in `look_in` is not a folder on this machine", folder.display()));
            }
        }

        // What was found, the least sure first, so that what is said by
        // hand has the last word on a place.
        let asked = Look { folders: folders(&request.look_in), moves: said.iter().cloned().collect() };
        let missing = look(&self.runtime, asked).await?;
        let mut relinks: Vec<Relink> = match found {
            RelinkFound::Sure => missing.relinks(Confidence::Sure),
            RelinkFound::Likely => missing.relinks(Confidence::Likely),
            RelinkFound::All => missing.relinks(Confidence::Unsure),
            RelinkFound::None => Vec::new(),
        };
        if let (Some(said), RelinkFound::None) = (&said, found) {
            for file in &missing.files {
                if let Some(there) = said.apply(&file.path).filter(|there| Path::new(there).is_file()) {
                    relinks.extend(file.relinks_to(&there));
                }
            }
        }
        {
            let session = self.session.lock();
            let uses = session.files();
            for (at, one) in request.files.iter().enumerate() {
                let to = one.to.trim();
                if to.is_empty() {
                    return Err(format!("files[{at}] needs `to`: the whole path of the file to play instead"));
                }
                let to = std::path::absolute(to).map_or_else(|_| to.to_owned(), |path| path.display().to_string());
                let places: Vec<FilePlace> = match (&one.file, one.clip, one.device, &one.slot) {
                    (Some(file), None, None, _) => {
                        let named: Vec<FilePlace> = uses
                            .iter()
                            .filter(|used| trill_core::files::same_path(&used.path, file))
                            .map(|used| used.place.clone())
                            .collect();
                        if named.is_empty() {
                            return Err(format!(
                                "files[{at}]: the song names no file '{file}'. missing_files lists the files it \
                                 names that are missing, each by its path under `file`."
                            ));
                        }
                        named
                    }
                    (None, Some(clip), None, _) => vec![FilePlace::Clip(ClipId(clip))],
                    (None, None, Some(device), Some(slot)) => {
                        vec![FilePlace::Sample { device: DeviceId(device), slot: slot.clone() }]
                    }
                    (None, None, Some(_), None) => {
                        return Err(format!(
                            "files[{at}] names a device: say which of its samples with `slot` (`sample`, `pad 36`)"
                        ));
                    }
                    _ => {
                        return Err(format!(
                            "files[{at}] has to name one thing: a missing `file` by its old path, a `clip`, or a \
                             `device` with `slot`"
                        ));
                    }
                };
                relinks.extend(places.into_iter().map(|place| Relink { place, path: to.clone() }));
            }
        }

        // The new files are decoded off the async threads and without the
        // session. The edit itself is made here, where the thread acts for
        // this call: it is the call's step in the journal.
        let changed = if relinks.is_empty() {
            0
        } else {
            let media = self.runtime.media.clone();
            let mut paths: Vec<String> = relinks.iter().map(|relink| relink.path.clone()).collect();
            paths.sort_unstable();
            paths.dedup();
            let reading = tokio::task::spawn_blocking(move || {
                for path in &paths {
                    // What cannot be read is said by the session, which refuses it.
                    let _ = media.audio(Path::new(path));
                }
            });
            let _ = reading.await;
            let mut session = self.session.lock();
            match session.relink(&relinks) {
                Ok(changed) => changed,
                Err(error) => return Err(explain(session.project(), error)),
            }
        };

        // The library's own folders, when the same move explains them.
        let mut library_moved: Vec<Value> = Vec::new();
        if request.library != Some(false) {
            let mut moves = missing.library_moves();
            if let Some(said) = &said {
                let from = PathBuf::from(&said.from);
                let is_missing = missing.folders.iter().any(|folder| folder.path == from);
                if is_missing && !moves.iter().any(|(known, _)| *known == from) {
                    moves.push((from, PathBuf::from(&said.to)));
                }
            }
            // Only together with files of the song: the library is not
            // rearranged by a call that relinked nothing.
            for (from, to) in moves.into_iter().filter(|_| changed > 0) {
                match self.runtime.move_library_folder(&from, &to) {
                    Ok(moved) => library_moved.push(json!({
                        "from": from.display().to_string(),
                        "to": to.display().to_string(),
                        "favorites": moved.favorites,
                    })),
                    Err(error) => library_moved.push(json!({ "from": from.display().to_string(), "error": error })),
                }
            }
        }

        let session = self.session.lock();
        let still = trill_runtime::files_missing_in(&session);
        let mut files: Vec<&str> = relinks.iter().map(|relink| relink.path.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let clips: Vec<u64> = relinks
            .iter()
            .filter_map(|relink| match &relink.place {
                FilePlace::Clip(clip) => Some(clip.0),
                FilePlace::Sample { .. } => None,
            })
            .collect();
        let mut out = json!({
            "relinked": { "files": if changed == 0 { 0 } else { files.len() }, "places": changed },
            "still_missing": still.files.len(),
        });
        if changed > 0 {
            out["now"] = files.iter().take(DEFAULT_FILES).map(|path| json!(path)).collect();
            out["clips"] = json!(clips);
            out["note"] = json!("One undo step: history undo takes all of it back.");
        }
        if !library_moved.is_empty() {
            out["library_moved"] = json!(library_moved);
        }
        if let Some(brief) = brief(&still) {
            out["missing_files"] = brief;
        }
        if changed == 0 {
            out["note"] = json!(match (missing.files.len(), missing.found(Confidence::Unsure)) {
                (0, _) => "No file of the song is missing, so nothing was relinked.".to_owned(),
                (_, 0) => "Nothing was relinked: nothing was found for the missing files. Ask the user where the \
                           folder went, then give it as `folder` (from, to) or `look_in`."
                    .to_owned(),
                (files, found) => format!(
                    "Nothing was relinked: of {files} missing, {found} {} found, none for sure. missing_files \
                     says why; take them with `found` \"likely\" or \"all\" once you have read it.",
                    if found == 1 { "was" } else { "were" }
                ),
            });
        }
        Ok(out)
    }
}

/// The name of the file a relink of one file points at, for its words.
pub(crate) fn one_file(input: &Value) -> Option<&str> {
    match input.get("files")?.as_array()?.as_slice() {
        [one] => one.get("to")?.as_str().map(file_name),
        _ => None,
    }
}
