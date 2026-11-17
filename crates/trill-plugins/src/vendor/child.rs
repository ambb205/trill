//! Reading a vendor's preset file in the scanner process: saying which
//! presets a file holds, and building a preset's state.
//!
//! Loading a vendor's preset means reading a file Trill did not write, in
//! a format worked out from the outside, and making a state of it. That is
//! done where a plugin's presets are read: in a process of the scanner
//! executable, started for the one preset. What it is given and what it
//! gives back are files in a folder made for the occasion:
//!
//! ```text
//! trill-plugin-scan --vendor-preset <format> <preset file> <key> <state now> <state to be>
//! ```
//!
//! Both states are VST3 preset blobs ([`crate::vst3::state`]). The process
//! prints one line of JSON, an [`Outcome`], and whatever else is on its
//! standard output is not its own. Without a scanner executable the state
//! is built here after all, behind a guard against panics.
//!
//! A file that lies outside the vendor's folders, and so is not among the
//! presets that are listed, is asked which presets it holds the same way:
//!
//! ```text
//! trill-plugin-scan --vendor-file <format> <preset file>
//! ```
//!
//! which prints what reading a plugin's presets prints
//! ([`Line`](crate::presets::Line)).

use super::{Built, Format, Mark};
use crate::presets::{Line, identify};
use crate::scan::Scanner;
use crate::scan::child::{self, Finished};
use crate::vst3::class_id::ClassId;
use crate::vst3::state::{self, PluginState};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use trill_core::presets::PresetInfo;

/// The first argument of the scanner executable when it is to build the
/// state of a vendor's preset.
pub const VENDOR_PRESET_ARGUMENT: &str = "--vendor-preset";

/// The first argument of the scanner executable when it is to say which
/// presets a vendor's file holds.
pub const VENDOR_FILE_ARGUMENT: &str = "--vendor-file";

/// How long building one state may take. The largest file of presets
/// there is takes a fraction of a second to read.
const BUILD_TIMEOUT: Duration = Duration::from_secs(60);
/// How many presets go into one line of what the listing process prints.
const BATCH: usize = 64;

/// What the building process says of how it went.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Outcome {
    /// Whether the state was written.
    #[serde(default)]
    built: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    marks: Vec<Mark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Why the state was not built, as a sentence about the preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed: Option<String>,
}

/// A folder of this process's own in the temporary folder, removed when
/// it is let go of.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let name = format!("trill-preset-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let folder = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&folder)?;
        Ok(Self(folder))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds the state that makes a plugin of class `class` the preset in
/// `file`: in a scanner process if `scanner` names one, and otherwise
/// here. See [`super::build`] for the rest.
pub(crate) fn build(
    scanner: &Scanner,
    format: Format,
    file: &Path,
    key: Option<&str>,
    class: ClassId,
    current: &PluginState,
) -> Result<Built, String> {
    let Some(scanner) = scanner.resolve() else {
        // A panic in a reader is a file it was not ready for, not a
        // reason to take the program down.
        let built = std::panic::catch_unwind(|| super::build(format, file, key, current));
        return built.unwrap_or_else(|_| Err(format!("'{}' cannot be loaded: reading it failed", file.display())));
    };
    let scratch = Scratch::new().map_err(|error| format!("cannot make a folder to build the preset in: {error}"))?;
    let (now, then) = (scratch.0.join("now"), scratch.0.join("then"));
    std::fs::write(&now, state::encode(class, current))
        .map_err(|error| format!("cannot write '{}': {error}", now.display()))?;
    let arguments: [&OsStr; 6] = [
        OsStr::new(VENDOR_PRESET_ARGUMENT),
        OsStr::new(format.tag()),
        file.as_os_str(),
        OsStr::new(key.unwrap_or_default()),
        now.as_os_str(),
        then.as_os_str(),
    ];
    let finished = child::run_process(&scanner, &arguments, BUILD_TIMEOUT, None)?;
    let outcome = interpret(&finished, file)?;
    let bytes = std::fs::read(&then).map_err(|error| format!("the preset's state was not built: {error}"))?;
    let (_, state) = state::decode(&bytes).map_err(|reason| format!("the preset's state was not built: {reason}"))?;
    Ok(Built { state, marks: outcome.marks, note: outcome.note })
}

/// The presets in the file at `file`, read as a preset file of `format`:
/// in a scanner process if `scanner` names one, and otherwise here. Each
/// has the id it would be listed with. An error is the end of a sentence
/// about the file; see [`super::file_presets`].
pub(crate) fn file_presets(scanner: &Scanner, format: Format, file: &Path) -> Result<Vec<PresetInfo>, String> {
    let Some(scanner) = scanner.resolve() else {
        let read = std::panic::catch_unwind(|| super::file_presets(format, file));
        return read.unwrap_or_else(|_| Err("reading it failed".to_string())).map(identify);
    };
    let arguments: [&OsStr; 3] = [OsStr::new(VENDOR_FILE_ARGUMENT), OsStr::new(format.tag()), file.as_os_str()];
    listed(&child::run_process(&scanner, &arguments, BUILD_TIMEOUT, None)?)
}

/// What a listing process printed, as the presets it found or the reason
/// it found none.
fn listed(finished: &Finished) -> Result<Vec<PresetInfo>, String> {
    let said = String::from_utf8_lossy(&finished.stdout);
    let (mut presets, mut ended) = (Vec::new(), None);
    for line in said.lines().filter_map(|line| serde_json::from_str::<Line>(line.trim()).ok()) {
        presets.extend(line.presets);
        match line.failed {
            Some(reason) => ended = Some(Err(reason)),
            None if line.done => ended = Some(Ok(())),
            None => {}
        }
    }
    match (ended, finished.status) {
        (Some(Ok(())), _) => Ok(identify(presets)),
        (Some(Err(reason)), _) => Err(reason),
        (None, None) => Err("reading it took too long".to_string()),
        (None, Some(status)) => Err(format!("reading it failed ({})", child::explain_exit(status, &finished.stderr))),
    }
}

/// The scanner executable's work when it was started to say which presets
/// a file holds: `arguments` are what follows [`VENDOR_FILE_ARGUMENT`].
/// Returns the exit code.
pub(crate) fn run_lister(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> u8 {
    let (Some(format), Some(file), None) = (arguments.next(), arguments.next(), arguments.next()) else {
        eprintln!("usage: {} {VENDOR_FILE_ARGUMENT} <format> <preset file>", crate::scan::SCANNER_NAME);
        return 2;
    };
    let print = |line: &Line| {
        if let Ok(json) = serde_json::to_string(line) {
            println!("{json}");
        }
    };
    let format = format.to_string_lossy();
    let read = match Format::parse(&format) {
        Some(format) => super::file_presets(format, Path::new(&file)),
        None => Err(format!("'{format}' is not a preset format")),
    };
    match read {
        Ok(presets) => {
            for batch in presets.chunks(BATCH) {
                print(&Line { presets: batch.to_vec(), ..Line::default() });
            }
            print(&Line { done: true, ..Line::default() });
            0
        }
        Err(reason) => {
            print(&Line { failed: Some(reason), ..Line::default() });
            1
        }
    }
}

/// What a building process left behind, as its outcome or the reason
/// there is none.
fn interpret(finished: &Finished, file: &Path) -> Result<Outcome, String> {
    let said = String::from_utf8_lossy(&finished.stdout);
    let outcome = said.lines().rev().find_map(|line| serde_json::from_str::<Outcome>(line.trim()).ok());
    match (outcome, finished.status) {
        (Some(Outcome { failed: Some(reason), .. }), _) => Err(reason),
        (Some(outcome), _) if outcome.built => Ok(outcome),
        (_, None) => Err(format!("'{}' cannot be loaded: reading it took too long", file.display())),
        (_, Some(status)) => Err(format!(
            "'{}' cannot be loaded: reading it failed ({})",
            file.display(),
            child::explain_exit(status, &finished.stderr)
        )),
    }
}

/// The scanner executable's work when it was started to build a preset's
/// state: `arguments` are what follows [`VENDOR_PRESET_ARGUMENT`]. Returns
/// the exit code.
pub(crate) fn run_builder(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> u8 {
    let (Some(format), Some(file), Some(key), Some(now), Some(then), None) =
        (arguments.next(), arguments.next(), arguments.next(), arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!(
            "usage: {} {VENDOR_PRESET_ARGUMENT} <format> <preset file> <key> <state now> <state to be>",
            crate::scan::SCANNER_NAME
        );
        return 2;
    };
    let key = key.to_string_lossy().into_owned();
    let built = (|| {
        let format = format.to_string_lossy();
        let format = Format::parse(&format).ok_or_else(|| format!("'{format}' is not a preset format"))?;
        let current = std::fs::read(&now).map_err(|error| format!("cannot read the plugin's state: {error}"))?;
        let (class, current) = state::decode(&current)?;
        let built = super::build(format, Path::new(&file), (!key.is_empty()).then_some(key.as_str()), &current)?;
        std::fs::write(&then, state::encode(class, &built.state))
            .map_err(|error| format!("cannot write the preset's state: {error}"))?;
        Ok(built)
    })();
    let (outcome, code) = match built {
        Ok(Built { marks, note, .. }) => (Outcome { built: true, marks, note, failed: None }, 0),
        Err(reason) => (Outcome { failed: Some(reason), ..Outcome::default() }, 1),
    };
    if let Ok(line) = serde_json::to_string(&outcome) {
        println!("{line}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use crate::vendor::serum;

    #[cfg(windows)]
    fn status(code: u32) -> std::process::ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(code)
    }

    #[cfg(unix)]
    fn status(code: u32) -> std::process::ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw((code as i32) << 8)
    }

    fn finished(status: Option<std::process::ExitStatus>, stdout: &str) -> Finished {
        Finished { status, stdout: stdout.as_bytes().to_vec(), stderr: Vec::new(), cancelled: false }
    }

    #[test]
    fn without_a_scanner_process_the_state_is_built_here() {
        let dir = TestDir::new();
        let file = dir.file("Warm.SerumPreset", &serum::preset_file("Warm", "Me", 5.0, &[]));
        let class = ClassId::parse(serum::CLASS).unwrap();
        let built = build(&Scanner::InProcess, Format::Serum2, &file, None, class, &PluginState::default()).unwrap();
        assert_eq!(built.marks, [Mark { name: "presetName".into(), value: "Warm".into() }]);
        assert!(!built.state.component.is_empty() && !built.state.controller.is_empty());

        let missing = dir.path().join("Gone.SerumPreset");
        let error = build(&Scanner::InProcess, Format::Serum2, &missing, None, class, &PluginState::default());
        let error = error.unwrap_err();
        assert!(error.contains("Gone.SerumPreset") && error.contains("cannot be read"), "{error}");
        let garbage = dir.file("Garbage.SerumPreset", b"not a preset");
        let error = build(&Scanner::InProcess, Format::Serum2, &garbage, None, class, &PluginState::default());
        let error = error.unwrap_err();
        assert!(error.ends_with("Garbage.SerumPreset' cannot be loaded: it is not one of Xfer's files"), "{error}");
    }

    #[test]
    fn what_the_building_process_printed_is_how_it_went() {
        let file = Path::new("Warm.SerumPreset");
        // A plugin's chatter around the line that counts.
        let said = "loading...\n{\"built\":true,\"marks\":[{\"name\":\"presetName\",\"value\":\"Warm\"}],\"note\":\"n\"}\nbye\n";
        let outcome = interpret(&finished(Some(status(0)), said), file).unwrap();
        assert_eq!((outcome.marks.len(), outcome.note.as_deref()), (1, Some("n")));
        // Why not, in the reader's own words.
        let failed =
            interpret(&finished(Some(status(1)), "{\"failed\":\"'x' cannot be loaded: it is cut short\"}\n"), file);
        assert_eq!(failed.unwrap_err(), "'x' cannot be loaded: it is cut short");
        // A process that said nothing: it crashed, or was stopped.
        let crashed = interpret(&finished(Some(status(3)), ""), file).unwrap_err();
        assert!(crashed.starts_with("'Warm.SerumPreset' cannot be loaded: reading it failed ("), "{crashed}");
        let stopped = interpret(&finished(None, ""), file).unwrap_err();
        assert_eq!(stopped, "'Warm.SerumPreset' cannot be loaded: reading it took too long");
    }
}
