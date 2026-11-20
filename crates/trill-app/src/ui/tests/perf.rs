//! Not correctness tests: these print how long a frame takes on a project
//! far busier than a song. Run with
//! `cargo test -p trill-app -- --ignored frame_cost --nocapture`.

use super::sounds::{WAIT, sounds_tab, tone};
use super::*;
use crate::ui::sounds::WaveState;
use crate::ui::state::LaneOwner;
use trill_core::project::{AudioContent, WarpMode};
use trill_core::{AutomationPoint, AutomationTarget, Segment};

/// Runs `frames` frames with no input and returns the mean time of one, in
/// milliseconds: laying out and painting, and of that, tessellating.
fn frame_cost(harness: &mut Harness, frames: u32) -> (f64, f64) {
    let started = Instant::now();
    let mut tessellation = Duration::ZERO;
    for _ in 0..frames {
        harness.time += FRAME;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(harness.time),
            focused: true,
            ..Default::default()
        };
        harness.app.tick(&harness.ctx);
        let mut output = harness.ctx.run_ui(input, |ui| harness.app.frame(ui));
        output.textures_delta.clear();
        let before = Instant::now();
        let primitives = harness.ctx.tessellate(output.shapes, output.pixels_per_point);
        tessellation += before.elapsed();
        assert!(!primitives.is_empty());
    }
    let per_frame = started.elapsed() / frames;
    (per_frame.as_secs_f64() * 1000.0, (tessellation / frames).as_secs_f64() * 1000.0)
}

#[test]
#[ignore = "a measurement, not an assertion"]
fn frame_cost_with_thousands_of_notes() {
    let mut harness = Harness::new(UiOptions::default(), |session| {
        session
            .edit(|project| {
                for track in 0..8 {
                    let id = project.add_track(format!("Track {track}"));
                    for clip in 0..4 {
                        let clip = project.add_midi_clip(id, "Clip", clip as f64 * 16.0, 16.0)?;
                        let notes: Vec<Note> = (0..250)
                            .map(|n| Note {
                                pitch: 36 + ((n * 7 + track * 5) % 48) as u8,
                                start: n as f64 * 0.0625,
                                length: 0.0625,
                                velocity: 40 + (n % 80) as u8,
                                ..Default::default()
                            })
                            .collect();
                        project.clip_mut(clip)?.set_notes(notes)?;
                    }
                }
                Ok(())
            })
            .unwrap();
    });
    // Select a clip so the piano roll is busy too.
    harness.click(harness.lane(4.0, 0));
    harness.idle(5);
    let notes: usize =
        harness.project().tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("{notes} notes in the project: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");
}

/// Forty tracks of sixty clips each, every clip sixteen notes, and eight
/// automation lanes of two thousand points open at once.
fn heavy(session: &mut Session) -> Vec<AutomationTarget> {
    session
        .edit(|project| {
            let mut targets = Vec::new();
            for track in 0..40 {
                let id = project.add_track(format!("Track {track}"));
                for clip in 0..60 {
                    let clip = project.add_midi_clip(id, format!("Clip {clip}"), clip as f64 * 4.0, 4.0)?;
                    let notes: Vec<Note> = (0..16)
                        .map(|n| Note {
                            pitch: 40 + ((n * 5 + track * 3) % 36) as u8,
                            start: n as f64 * 0.25,
                            length: 0.25,
                            velocity: 60 + (n * 4) as u8,
                            ..Default::default()
                        })
                        .collect();
                    project.clip_mut(clip)?.set_notes(notes)?;
                }
                if track < 8 {
                    let target = AutomationTarget::TrackVolume { track: id };
                    let points = (0..2000).map(|n| {
                        let shape = if n % 3 == 0 { Segment::Curve(0.4) } else { Segment::Linear };
                        AutomationPoint { beat: n as f64 * 0.12, value: -30.0 + ((n * 37) % 300) as f64 / 10.0, shape }
                    });
                    project.set_automation(target.clone(), points.collect())?;
                    targets.push(target);
                }
            }
            Ok(targets)
        })
        .unwrap()
}

#[test]
#[ignore = "a measurement, not an assertion"]
fn frame_cost_of_a_heavy_arrangement() {
    let mut targets = Vec::new();
    let mut harness = Harness::new(UiOptions { show_automation: true, ..Default::default() }, |session| {
        targets = heavy(session);
    });
    let project = harness.project();
    let clips: usize = project.tracks.iter().map(|track| track.clips.len()).sum();
    let notes: usize = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();
    let points: usize = project.automation.iter().map(|lane| lane.points.len()).sum();
    assert_eq!(harness.app.state().arrangement.lanes.len(), 8);
    assert!(harness.app.state().arrangement.lanes.iter().all(|lane| matches!(lane.owner, LaneOwner::Track(_))));
    eprintln!("{clips} clips, {notes} notes, {points} automation points, 40 tracks, 8 lanes open");

    // The whole song in view: every clip of the visible tracks is on screen.
    harness.idle(5);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("whole song in view: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");

    // Zoomed in on a few bars, with a clip open in the piano roll.
    harness.click(harness.lane(6.0, 0));
    harness.key(Modifiers::NONE, Key::Z);
    harness.idle(40);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("zoomed to one clip: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");

    // The cost of an edit: a clip dragged across the song, each step a
    // change to the project and a new snapshot.
    harness.key(Modifiers::SHIFT, Key::Z);
    harness.idle(40);
    let started = Instant::now();
    harness.drag_start(Modifiers::NONE, harness.lane(5.0, 0), harness.lane(60.0, 0));
    for step in 0..40 {
        harness.move_to(harness.lane(60.0 + step as f64, 0));
    }
    let dragging = started.elapsed() / 50;
    harness.drag_end(harness.lane(100.0, 0));
    eprintln!("dragging a clip: {:.2} ms per frame, edit and new snapshot included", dragging.as_secs_f64() * 1000.0);

    // The mixer with forty strips.
    harness.click_label("Mixer");
    harness.idle(5);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("mixer, 40 tracks: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");
}

/// Runs `frames` frames, each with the events `input` gives it, and returns
/// the mean and the worst time of one, in milliseconds, tessellation
/// included.
fn frame_cost_with(harness: &mut Harness, frames: u32, mut input: impl FnMut(u32) -> Vec<Event>) -> (f64, f64) {
    let mut total = Duration::ZERO;
    let mut worst = Duration::ZERO;
    for frame in 0..frames {
        harness.time += FRAME;
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(harness.time),
            focused: true,
            events: input(frame),
            ..Default::default()
        };
        let started = Instant::now();
        harness.app.tick(&harness.ctx);
        let mut output = harness.ctx.run_ui(raw, |ui| harness.app.frame(ui));
        output.textures_delta.clear();
        let primitives = harness.ctx.tessellate(output.shapes, output.pixels_per_point);
        let took = started.elapsed();
        assert!(!primitives.is_empty());
        total += took;
        worst = worst.max(took);
    }
    ((total / frames).as_secs_f64() * 1000.0, worst.as_secs_f64() * 1000.0)
}

fn wheel(position: Pos2, lines: f32, modifiers: Modifiers) -> Vec<Event> {
    vec![
        Event::PointerMoved(position),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers,
        },
    ]
}

/// Sixteen tracks of audio: hits, loops (half of them warped) and, on two
/// tracks, a take as long as the song. More than three hundred clips, of
/// which the window shows some two hundred at once.
#[test]
#[ignore = "a measurement, not an assertion"]
fn frame_cost_with_many_audio_clips() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let hit = tone(root.join("Kick 01.wav"), 0.5).display().to_string();
    let short = tone(root.join("Bass Loop 120bpm.wav"), 2.0).display().to_string();
    let long = tone(root.join("Drum Loop 120bpm.wav"), 8.0).display().to_string();
    let take = tone(root.join("Vocal Take.wav"), 180.0).display().to_string();

    let mut harness = Harness::new(UiOptions::default(), |session| {
        session
            .edit(|project| {
                for track in 0..16 {
                    let id = project.add_track(format!("Audio {track}"));
                    if track % 8 == 3 {
                        project.add_audio_clip(id, "Take", 0.0, 360.0, AudioContent::new(&take))?;
                        continue;
                    }
                    for clip in 0..24 {
                        let start = clip as f64 * 16.0;
                        let warp = if clip % 2 == 0 { WarpMode::Beats } else { WarpMode::Off };
                        let content = |path: &str| AudioContent {
                            warp,
                            source_tempo: Some(120.0),
                            fade_in: 0.5,
                            fade_out: 1.0,
                            ..AudioContent::new(path)
                        };
                        match (track + clip) % 3 {
                            0 => project.add_audio_clip(id, "Loop", start, 16.0, content(&long))?,
                            1 => project.add_audio_clip(id, "Bass", start, 4.0, content(&short))?,
                            _ => project.add_audio_clip(id, "Kick", start, 1.0, AudioContent::new(&hit))?,
                        };
                    }
                }
                Ok(())
            })
            .unwrap();
    });
    let started = Instant::now();
    harness.settle_work();
    let clips: usize = harness.project().tracks.iter().map(|track| track.clips.len()).sum();
    assert!(matches!(harness.app.sounds().wave(&take), WaveState::Ready(_)));
    eprintln!(
        "{clips} audio clips on 16 tracks from 4 files (one of 3 minutes); waveforms ready {:.0} ms after the window opened",
        started.elapsed().as_secs_f64() * 1000.0
    );

    harness.idle(5);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("whole song in view, still: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");

    // Panning: every frame the view moves, so every waveform is cut anew.
    let over = harness.lane(40.0, 3);
    let (frame, worst) = frame_cost_with(&mut harness, 120, |frame| {
        wheel(over, if frame % 40 < 20 { -1.0 } else { 1.0 }, Modifiers::NONE)
    });
    eprintln!("panning: {frame:.2} ms per frame (worst {worst:.2} ms)");

    // Zooming in and out again around the pointer.
    let (frame, worst) = frame_cost_with(&mut harness, 120, |frame| {
        wheel(over, if frame % 60 < 30 { 1.0 } else { -1.0 }, Modifiers::COMMAND)
    });
    eprintln!("zooming: {frame:.2} ms per frame (worst {worst:.2} ms)");

    // Zoomed far in, with the audio clip panel open on a clip.
    harness.click(harness.lane(2.0, 0));
    harness.key(Modifiers::NONE, Key::Z);
    harness.idle(40);
    harness.settle_work();
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!(
        "zoomed to one clip, its panel open: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation"
    );
    let over = harness.lane(4.0, 0);
    let (frame, worst) = frame_cost_with(&mut harness, 120, |frame| {
        wheel(over, if frame % 40 < 20 { -1.0 } else { 1.0 }, Modifiers::NONE)
    });
    eprintln!("panning there: {frame:.2} ms per frame (worst {worst:.2} ms)");
}

/// A library of a hundred thousand files, written to a temporary folder
/// for the purpose: what scanning it takes, what a frame of the browser
/// costs with all of it listed, and how long a search is out for.
#[test]
#[ignore = "a measurement, not an assertion; writes a hundred thousand small files"]
fn frame_cost_of_a_large_sound_library() {
    const FOLDERS: usize = 200;
    const PER_FOLDER: usize = 500;
    let folder = tempfile::tempdir().unwrap();
    let root = std::path::absolute(folder.path().join("Big Library")).unwrap();
    // One real file, copied: what matters here is how many there are.
    let seed = std::fs::read(tone(folder.path().join("seed.wav"), 0.01)).unwrap();
    let kinds =
        ["Kick", "Snare", "Hat Closed", "Clap", "Bass Loop 120bpm Am", "Drum Loop 140bpm", "Pad C#m", "Vocal Chop"];
    let started = Instant::now();
    for pack in 0..FOLDERS {
        let dir = root.join(format!("Pack {pack:03}")).join(if pack % 2 == 0 { "Drums" } else { "Loops" });
        std::fs::create_dir_all(&dir).unwrap();
        for file in 0..PER_FOLDER {
            let kind = kinds[(pack + file) % kinds.len()];
            std::fs::write(dir.join(format!("{kind} {file:04}.wav")), &seed).unwrap();
        }
    }
    let files = FOLDERS * PER_FOLDER;
    eprintln!("{files} files written in {:.1} s", started.elapsed().as_secs_f64());

    let mut harness = Harness::new(sounds_tab(), |session| {
        session.edit(|project| Ok(project.add_track("One"))).unwrap();
    });
    let started = Instant::now();
    harness.app.sounds_mut().add_folder(root.clone());
    // The window keeps running while the library is scanned.
    let mut scanning = (Duration::ZERO, Duration::ZERO, 0u32);
    while harness.runtime.library.scan_status().is_scanning() || harness.runtime.library.roots().is_empty() {
        let frame = Instant::now();
        harness.idle(1);
        let took = frame.elapsed();
        scanning = (scanning.0 + took, scanning.1.max(took), scanning.2 + 1);
        assert!(started.elapsed() < Duration::from_secs(600), "the scan never finished");
    }
    eprintln!(
        "scanned in {:.1} s; meanwhile {} frames at {:.2} ms each (worst {:.2} ms)",
        started.elapsed().as_secs_f64(),
        scanning.2,
        scanning.0.as_secs_f64() * 1000.0 / f64::from(scanning.2.max(1)),
        scanning.1.as_secs_f64() * 1000.0
    );
    harness.settle_work();
    let listed = harness.app.sounds().search.found.as_ref().map_or(0, |found| found.results.len());
    assert_eq!(listed, files, "every file is listed");

    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("{listed} sounds listed, still: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");

    // Scrolling through the list.
    let over = pos2(harness.work_left() / 2.0, 500.0);
    let (frame, worst) = frame_cost_with(&mut harness, 240, |_| wheel(over, -6.0, Modifiers::NONE));
    eprintln!("scrolling the list: {frame:.2} ms per frame (worst {worst:.2} ms)");

    // The arrows, held down: the selection moves a row a frame.
    harness.click(over);
    harness.settle_work();
    harness.app.state_mut().browser.sounds.auto_preview = false;
    let (frame, worst) = frame_cost_with(&mut harness, 120, |frame| {
        let pressed = frame % 2 == 0;
        vec![Event::Key { key: Key::ArrowDown, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]
    });
    eprintln!("arrow keys through the list: {frame:.2} ms per frame (worst {worst:.2} ms)");

    // Typing a search: the frames that take the keys in, and how long the
    // answer is out for. The search itself runs on a worker.
    for text in ["k", "ki", "kick", "kick 01", "loop 120bpm", "zzz"] {
        harness.app.state_mut().browser.sounds.search = text.to_owned();
        let asked = Instant::now();
        let mut worst = Duration::ZERO;
        let mut frames = 0;
        loop {
            let frame = Instant::now();
            harness.idle(1);
            worst = worst.max(frame.elapsed());
            frames += 1;
            let found = harness.app.sounds().search.found.as_ref();
            if found.is_some_and(|found| found.query.text == text) {
                break;
            }
            assert!(asked.elapsed() < WAIT, "the search never came back");
        }
        let matches = harness.app.sounds().search.found.as_ref().map_or(0, |found| found.results.len());
        eprintln!(
            "search '{text}': {matches} matches after {:.1} ms ({frames} frames, the slowest {:.2} ms)",
            asked.elapsed().as_secs_f64() * 1000.0,
            worst.as_secs_f64() * 1000.0
        );
    }
    // For comparison: the same search run where the window would have run
    // it, had it not been handed to a worker.
    let query = crate::ui::browser::query(&harness.app.state().browser.sounds, 120.0);
    let timed = Instant::now();
    let direct = harness.runtime.library.search(&trill_library::Query { text: "kick".into(), ..query });
    eprintln!(
        "the search for 'kick' itself: {:.1} ms for {} matches",
        timed.elapsed().as_secs_f64() * 1000.0,
        direct.len()
    );
}

/// One lane of 7,332 points over 456 beats, as a compressor drawn on paper
/// leaves it: what a frame of it costs with the whole song in view, where
/// there are six points to the pixel, zoomed in, and while one of its
/// points is dragged.
#[test]
#[ignore = "a measurement, not an assertion"]
fn frame_cost_of_a_lane_of_thousands_of_points() {
    let count = 7_332;
    let beats = 456.0;
    let mut target = None;
    let mut harness = Harness::new(UiOptions { show_automation: true, ..Default::default() }, |session| {
        session
            .edit(|project| {
                let track = project.add_track("Pad");
                project.add_midi_clip(track, "Clip", 0.0, beats)?;
                let volume = AutomationTarget::TrackVolume { track };
                let points = (0..count).map(|index| {
                    let beat = index as f64 * beats / count as f64;
                    AutomationPoint::new(beat, -12.0 * (-(beat.fract()) * 6.0).exp())
                });
                project.set_automation(volume.clone(), points.collect())?;
                target = Some(volume);
                Ok(())
            })
            .unwrap();
    });
    let target = target.unwrap();
    assert_eq!(harness.app.state().arrangement.lanes.len(), 1);
    harness.idle(5);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!(
        "{count} points in one lane, a few bars in view: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation"
    );

    harness.key(Modifiers::SHIFT, Key::Z);
    harness.idle(40);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("the whole song in view: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");

    // The same view with the lane folded away, for what the lane adds.
    harness.app.state_mut().arrangement.lanes.clear();
    harness.idle(5);
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("the same without the lane: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");
    let owner = crate::ui::state::lane_owner(&harness.project(), &target).unwrap();
    harness.app.state_mut().arrangement.open_lane(owner, target.clone());
    harness.idle(5);

    // A point dragged: every frame the lane is written again, the graph
    // compiled and the snapshot taken.
    let top = crate::ui::arrangement::TRACK_HEIGHT;
    let lane_height = crate::ui::state::ArrangementState::DEFAULT_LANE_HEIGHT;
    let point = harness.project().automation[0].points[count / 2];
    let height = crate::ui::units::db_to_fader(point.value as f32);
    let at = |harness: &Harness, up: f32| {
        let y = top + lane_height - 6.0 - (height + up) * (lane_height - 12.0);
        harness.lanes_at(point.beat, y)
    };
    let started = Instant::now();
    harness.drag_start(Modifiers::ALT, at(&harness, 0.0), at(&harness, 0.02));
    for step in 0..40 {
        harness.move_to(at(&harness, 0.02 + step as f32 * 0.005));
    }
    let dragging = started.elapsed() / 50;
    harness.drag_end(at(&harness, 0.3));
    let moved = harness.project().automation[0].points[count / 2];
    assert_ne!(moved.value, point.value, "the drag took hold of the point");
    eprintln!(
        "dragging one of them: {:.2} ms per frame, edit and new snapshot included",
        dragging.as_secs_f64() * 1000.0
    );

    // All of them selected, shown so, and dragged as one.
    harness.key(Modifiers::COMMAND, Key::A);
    harness.idle(2);
    assert_eq!(harness.app.state().arrangement.points.as_ref().map(|chosen| chosen.points.len()), Some(count));
    let (frame, tessellation) = frame_cost(&mut harness, 120);
    eprintln!("all of them selected: {frame:.2} ms per frame, of which {tessellation:.2} ms tessellation");
    let grabbed = harness.project().automation[0].points[count / 2];
    let height = crate::ui::units::db_to_fader(grabbed.value as f32);
    let at = |harness: &Harness, up: f32| {
        let y = top + lane_height - 6.0 - (height + up) * (lane_height - 12.0);
        harness.lanes_at(grabbed.beat, y)
    };
    let started = Instant::now();
    harness.drag_start(Modifiers::ALT, at(&harness, 0.0), at(&harness, -0.02));
    for step in 0..40 {
        harness.move_to(at(&harness, -0.02 - step as f32 * 0.005));
    }
    let dragging = started.elapsed() / 50;
    harness.drag_end(at(&harness, -0.25));
    let lowered = harness.project().automation[0].points.clone();
    assert!(lowered.iter().step_by(61).zip(moved_from(&harness, count)).all(|(now, was)| now.value < was));
    eprintln!("dragging all of them: {:.2} ms per frame", dragging.as_secs_f64() * 1000.0);
}

/// What every sixty-first point of the lane was worth before it was moved.
fn moved_from(_: &Harness, count: usize) -> impl Iterator<Item = f64> {
    (0..count).step_by(61).map(move |index| {
        let beat = index as f64 * 456.0 / count as f64;
        -12.0 * (-(beat.fract()) * 6.0).exp() + 1e-9
    })
}
