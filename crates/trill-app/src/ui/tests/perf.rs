//! Not correctness tests: these print how long a frame takes on a project
//! far busier than a song. Run with
//! `cargo test -p trill-app -- --ignored frame_cost --nocapture`.

use super::*;
use crate::ui::state::LaneOwner;
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
