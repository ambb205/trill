//! Exporting from the window a song whose plugin takes down the process
//! that renders it: the window says so where it says that an export
//! failed, and goes on with the song as it was.
//!
//! The process that renders is this test program, started again to run
//! [`a_process_that_does_jobs`] and nothing else; the plugin is the
//! engine's scripted bass, told to fault. Where a file goes is asked by
//! the system's own dialog, which nothing here can answer: the tests hand
//! the place over where the dialog's answer would arrive.

use super::*;
use trill_engine::scripted::{BASS, Dies, Loading, ScriptedFactory};
use trill_runtime::{Jobs, Parts, Program};

/// What this test program is when it is started to render for another:
/// run by name, with nothing to do otherwise.
#[test]
fn a_process_that_does_jobs() {
    trill_runtime::jobs::run_as_job_if_requested();
}

/// The window on a song with the scripted bass, whose renders are done by
/// this program started again.
pub(super) fn with_a_bass(loading: Loading) -> (Harness, TrackId) {
    let parts = Parts { devices: vec![Box::new(ScriptedFactory::loading(loading))], ..Parts::default() };
    let runtime = Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap();
    let test = "ui::tests::export::a_process_that_does_jobs".to_string();
    runtime.do_jobs_apart(Jobs { scripted: Some(loading), ..Jobs::by(Program::ThisTest(test)) }).unwrap();
    let mut bass = None;
    let harness = Harness::on(Arc::new(runtime), UiOptions::default(), |session| {
        let track = session.edit(|project| Ok(project.add_track("Bass"))).unwrap();
        session.add_device(ChainRef::Track(track), BASS, None, None).unwrap();
        let notes: Vec<Note> = (0..32u8)
            .map(|step| Note { pitch: 40 + step % 5, start: f64::from(step) * 0.5, length: 0.4, velocity: 100 })
            .collect();
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "Line", 0.0, 16.0)?;
                project.clip_mut(clip)?.add_notes(&notes)
            })
            .unwrap();
        bass = Some(track);
    });
    (harness, bass.unwrap())
}

/// Waits for the next thing the window says in a toast and returns it.
pub(super) fn next_toast(harness: &mut Harness, before: usize) -> String {
    let began = Instant::now();
    while harness.app.state().toasts.len() == before {
        assert!(began.elapsed() < Duration::from_secs(120), "nothing was said");
        std::thread::sleep(Duration::from_millis(20));
        harness.idle(1);
    }
    harness.app.state().toasts.texts().last().map(|text| (*text).to_owned()).unwrap()
}

#[test]
fn an_export_that_a_plugin_takes_down_is_said_in_the_window_and_the_song_is_as_it_was() {
    let folder = tempfile::tempdir().unwrap();
    // The bass takes its process down in the middle of the song.
    let (mut harness, bass) = with_a_bass(Loading { dies: Some(Dies::Played(40)), ..Loading::default() });
    harness.idle(3);
    let (song, steps) = (harness.project(), harness.undo_depth());
    let path = folder.path().join("song.wav");
    let before = harness.app.state().toasts.len();
    harness.app.export_to(&harness.ctx.clone(), path.clone(), true);
    let said = next_toast(&mut harness, before);
    assert!(said.starts_with("Export failed: the render was taken down while it was being played"), "{said}");
    assert!(said.contains("the song that is open is untouched, and what it had begun to write was removed"), "{said}");
    assert!(said.contains("Try again"), "{said}");
    // Nothing was written, and the song is what it was.
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0, "something was left in the folder");
    // (A render takes down what state the song's own instruments have,
    // which is no step of the song's.)
    assert_eq!(harness.project().tracks[0].clips, song.tracks[0].clips, "the song changed");
    assert_eq!(harness.undo_depth(), steps, "an export that failed is no step of the song's");

    // With the track muted the export goes through, in this process: there
    // is no plugin left to make.
    harness.session.lock().edit(|project| project.track_mut(bass).map(|track| track.mute = true)).unwrap();
    harness.idle(3);
    let before = harness.app.state().toasts.len();
    harness.app.export_to(&harness.ctx.clone(), path.clone(), false);
    let said = next_toast(&mut harness, before);
    assert!(said.contains("song.wav") && !said.contains("failed"), "{said}");
    assert!(path.is_file());
}
