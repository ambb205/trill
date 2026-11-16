//! Stages what two of the window's screenshots are taken of, in a folder of
//! their own, so that the pictures show nobody's own work or settings:
//!
//! ```sh
//! cargo run -p trill-app --example window_fixtures -- crates/trill-app/screenshots/fixtures
//! ```
//!
//! * `<DIR>/held-automation.trill`: the example song with two of its
//!   envelopes switched off, as they are after someone has taken hold of
//!   the controls they automate, beside others that are on.
//! * `<DIR>/lost-work`: a profile folder (`trill --profile <DIR>/lost-work`)
//!   holding two projects that were not saved before their program
//!   stopped: one that lived in a file, lost two days ago, and one that was
//!   never saved, lost just now. A window opened on it offers them back.
//!
//! Run it from the repository root: the song is read from `examples/`, and
//! that path is what the offer shows as the place the lost project lived.
//! Running it again starts both over.

use anyhow::Context;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use trill_core::{AutomationPoint, AutomationTarget, Note, Project};
use trill_engine::{AudioBackend, Session};
use trill_runtime::{Runtime, RuntimeOptions, Storage};

const SONG: &str = "examples/first-light.trill";

/// The example song with the envelopes of its first track taken hold of.
fn held_automation() -> anyhow::Result<Project> {
    let text =
        std::fs::read_to_string(SONG).with_context(|| format!("cannot read {SONG} (run from the repository root)"))?;
    let mut project = Project::from_json(&text)?;
    let (pad, keys) = (project.tracks[0].id, project.tracks[1].id);
    let synth = project.tracks[0].devices[0].id;
    let cutoff = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
    anyhow::ensure!(project.automation.iter().any(|lane| lane.target == cutoff), "the song automates no cutoff");

    // The filter was opened by hand, and stays where it was put.
    project.set_automation_enabled(&cutoff, false)?;
    project.device_mut(synth)?.params.insert("cutoff".into(), 5200.0);
    // So was the track's volume, which has an envelope of its own.
    let volume = AutomationTarget::TrackVolume { track: pad };
    let swell = [(0.0, -18.0), (32.0, -6.0), (96.0, -9.0)];
    project.set_automation(volume.clone(), swell.map(|(beat, value)| AutomationPoint::new(beat, value)).to_vec())?;
    project.set_automation_enabled(&volume, false)?;
    // The next track's volume is automated and left alone, for contrast.
    let rise = [(0.0, -12.0), (48.0, -3.5)];
    let target = AutomationTarget::TrackVolume { track: keys };
    project.set_automation(target, rise.map(|(beat, value)| AutomationPoint::new(beat, value)).to_vec())?;
    Ok(project)
}

/// A program that keeps what it keeps under `profile`, makes a project
/// with `work`, and stops without saving it or saying goodbye.
fn lose(profile: &Path, work: impl FnOnce(&mut Session) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let options = RuntimeOptions {
        audio: AudioBackend::Null { sample_rate: 48_000 },
        plugins: None,
        storage: Storage::At { settings: profile.join("settings.json"), library_index: None },
    };
    let runtime = Runtime::start(options)?;
    work(&mut runtime.session.lock())?;
    runtime.refresh_safety_copy();
    runtime.abandon();
    Ok(())
}

/// Makes the safety copies in `recovery` (so far there is one) say they
/// were made `ago` ago. The date is in the description the runtime keeps
/// beside each copy; if that ever changes its form, the picture shows
/// today's date and nothing else breaks.
fn backdate(recovery: &Path, ago: Duration) -> anyhow::Result<()> {
    let made = SystemTime::now().duration_since(UNIX_EPOCH)?.saturating_sub(ago).as_secs();
    for entry in std::fs::read_dir(recovery)?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "json") {
            let mut about: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            if about.get("made").is_some() {
                about["made"] = made.into();
                std::fs::write(&path, about.to_string())?;
            }
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let Some(folder) = std::env::args().nth(1) else {
        anyhow::bail!("usage: window_fixtures <DIR>");
    };
    let folder = Path::new(&folder);
    std::fs::create_dir_all(folder)?;
    std::fs::write(folder.join("held-automation.trill"), held_automation()?.to_json())?;

    // Start over: only the files a safety copy is made of are removed.
    let profile = folder.join("lost-work");
    let recovery = profile.join("recovery");
    if let Ok(entries) = std::fs::read_dir(&recovery) {
        for path in entries.flatten().map(|entry| entry.path()) {
            let ours = path.extension().and_then(|extension| extension.to_str());
            if matches!(ours, Some("trill" | "json" | "lock" | "tmp")) {
                std::fs::remove_file(&path)?;
            }
        }
    }
    // A song that lived in a file, changed and not saved again.
    lose(&profile, |session| {
        session.load(Path::new(SONG))?;
        session.edit(|project| project.set_tempo(104.0))?;
        Ok(())
    })?;
    backdate(&recovery, Duration::from_secs(2 * 86_400 + 3 * 3600))?;
    // And a sketch that never was.
    lose(&profile, |session| {
        session.edit(|project| {
            let track = project.add_track("Sketch");
            let clip = project.add_midi_clip(track, "Idea", 0.0, 8.0)?;
            let notes: Vec<Note> = [60, 63, 67, 70]
                .iter()
                .enumerate()
                .map(|(index, pitch)| Note { pitch: *pitch, start: index as f64 * 2.0, length: 1.5, velocity: 96 })
                .collect();
            project.clip_mut(clip)?.add_notes(&notes)
        })?;
        Ok(())
    })?;
    println!("wrote {} and the lost work in {}", folder.join("held-automation.trill").display(), profile.display());
    Ok(())
}
