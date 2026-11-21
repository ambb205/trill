//! The sound a film is made with, rendered as a film renders it: a plugin
//! that takes the render's process down is a sentence in place of the
//! film, and a film that is stopped while its sound is being rendered
//! stops the render.
//!
//! The process that renders is this test program, started again to run
//! [`a_process_that_does_jobs`] and nothing else; the plugin is the
//! engine's scripted bass. No picture is made, so no graphics card is
//! needed.

use std::time::{Duration, Instant};
use trill_core::{ChainRef, Note, TrackId};
use trill_engine::render::RenderOptions;
use trill_engine::scripted::{BASS, Dies, Loading, ScriptedFactory};
use trill_runtime::{Jobs, Parts, Program, Runtime, RuntimeOptions};
use trill_stage::export::{STOPPED_BEFORE_ITS_FIRST_FRAME, sound};
use trill_stage::film::Progress;

/// What this test program is when it is started to render for another:
/// run by name, with nothing to do otherwise.
#[test]
fn a_process_that_does_jobs() {
    trill_runtime::jobs::run_as_job_if_requested();
}

/// A runtime with the scripted bass on a track that plays `beats` beats
/// of one line over and over, whose renders are done by this program
/// started again, or here.
fn with_a_bass(loading: Loading, beats: f64, apart: bool) -> (Runtime, TrackId) {
    let parts = Parts { devices: vec![Box::new(ScriptedFactory::loading(loading))], ..Parts::default() };
    let runtime = Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap();
    if apart {
        let program = Program::ThisTest("a_process_that_does_jobs".to_string());
        runtime.do_jobs_apart(Jobs { scripted: Some(loading), ..Jobs::by(program) }).unwrap();
    }
    let track = runtime.session.lock().edit(|project| Ok(project.add_track("Bass"))).unwrap();
    runtime.session.lock().add_device(ChainRef::Track(track), BASS, None, None).unwrap();
    let notes: Vec<Note> = (0..32u8)
        .map(|step| Note {
            pitch: 40 + step % 5,
            start: f64::from(step) * 0.5,
            length: 0.4,
            velocity: 100,
            ..Default::default()
        })
        .collect();
    runtime
        .session
        .lock()
        .edit(|project| {
            let clip = project.add_midi_clip(track, "Line", 0.0, 16.0)?;
            project.clip_mut(clip)?.add_notes(&notes)?;
            project.clip_mut(clip)?.set_loop_length(Some(16.0))?;
            project.set_clip_span(clip, 0.0, beats)
        })
        .unwrap();
    (runtime, track)
}

#[test]
fn a_film_whose_song_takes_its_render_down_says_so_in_place_of_the_film() {
    // The bass takes its process down in the middle of the song.
    let (runtime, bass) = with_a_bass(Loading { dies: Some(Dies::Played(40)), ..Loading::default() }, 16.0, true);
    let job = runtime.session.lock().render_job();
    let steps = runtime.session.lock().revision();
    let said = sound(&job, RenderOptions::default(), &Progress::default()).map(|_| ()).unwrap_err();
    assert!(
        said.starts_with("The song could not be rendered: the render was taken down while it was being played"),
        "{said}"
    );
    assert!(said.contains("the song that is open is untouched") && said.contains("Try again"), "{said}");
    assert!(said.ends_with('.') && !said.ends_with(".."), "{said}");
    // The session is alive and the song is what it was; with the track
    // muted a film has its sound.
    assert_eq!(runtime.session.lock().revision(), steps);
    runtime.session.lock().edit(|project| project.track_mut(bass).map(|track| track.mute = true)).unwrap();
    let job = runtime.session.lock().render_job();
    assert!(sound(&job, RenderOptions::default(), &Progress::default()).is_ok());
}

#[test]
fn a_film_that_is_stopped_while_its_sound_is_rendered_stops_the_render() {
    for apart in [true, false] {
        // A song of most of an hour: its render takes far longer than the
        // test waits.
        let (runtime, _) = with_a_bass(Loading::default(), 6_400.0, apart);
        let job = runtime.session.lock().render_job();
        let progress = Progress::default();
        let (outcome, stopped_at, ended) = std::thread::scope(|scope| {
            let stopper = scope.spawn(|| {
                // Once the render is well under way.
                std::thread::sleep(Duration::from_millis(1_500));
                progress.stop();
                Instant::now()
            });
            let outcome = sound(&job, RenderOptions::default(), &progress).map(|_| ());
            (outcome, stopper.join().unwrap(), Instant::now())
        });
        assert_eq!(outcome, Err(STOPPED_BEFORE_ITS_FIRST_FRAME.to_string()), "in a process of its own: {apart}");
        let after = ended.duration_since(stopped_at);
        assert!(after < Duration::from_secs(5), "in a process of its own: {apart}: it went on for {after:?}");
    }
}
