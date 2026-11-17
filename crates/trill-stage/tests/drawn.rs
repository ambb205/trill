//! What the graphics card makes of a picture, and what a film is written
//! as. These need a graphics card (and two of them `ffmpeg`), so none of
//! them is part of the default run:
//!
//!     cargo test -p trill-stage --test drawn -- --ignored
//!
//! What is painted, and when, is tested without a card in the crate itself.

use std::path::Path;
use trill_core::{InstrumentProfile, Note, Project, Role, TrackId};
use trill_noir::Theme;
use trill_stage::director::{Entry, Kind, Plan, Shot, Take};
use trill_stage::film::{Filmer, Progress, Shoot};
use trill_stage::gpu::{Gpu, Quality, Renderer};
use trill_stage::scene::Layout;
use trill_stage::{Format, Heard, Look, Score, Song, Sound, Stage};

/// Sixteen bars: a kick on every beat, a line of eighths, two sections.
fn score() -> (Score, TrackId) {
    let mut project = Project::new("Drawn");
    let drums = project.add_track("Drums");
    let clip = project.add_midi_clip(drums, "beat", 0.0, 64.0).unwrap();
    let kicks: Vec<Note> =
        (0..64).map(|beat| Note { pitch: 36, start: beat as f64, length: 0.25, velocity: 110 }).collect();
    project.clip_mut(clip).unwrap().add_notes(&kicks).unwrap();
    project.set_track_profile(drums, InstrumentProfile { role: Some(Role::Drums), ..Default::default() }).unwrap();
    let lead = project.add_track("Lead");
    let clip = project.add_midi_clip(lead, "line", 0.0, 64.0).unwrap();
    let line: Vec<Note> = (0..128)
        .map(|step| Note { pitch: 60 + (step % 7) as u8, start: step as f64 * 0.5, length: 0.45, velocity: 100 })
        .collect();
    project.clip_mut(clip).unwrap().add_notes(&line).unwrap();
    project.add_marker(0.0, "Intro").unwrap();
    project.add_marker(32.0, "Verse").unwrap();
    (Score::read(&project), lead)
}

/// The song filmed in one take of `kind` from its first beat to its last.
fn in_one_take(kind: Kind) -> Song {
    let (score, lead) = score();
    let shot = match kind {
        Kind::Close => Shot { kind, rolls: vec![lead], window: (8.0, 8.0), roll_share: 0.8 },
        _ => Shot { kind, rolls: Vec::new(), window: (32.0, 32.0), roll_share: 0.0 },
    };
    let plan = Plan { takes: vec![Take { start: 0.0, end: score.length, section: 0, shot, entry: Entry::Cut }] };
    Song { score, plan }
}

struct Drawn {
    pixels: Vec<u8>,
    width: u32,
}

impl Drawn {
    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let first = ((y * self.width + x) * 4) as usize;
        [self.pixels[first], self.pixels[first + 1], self.pixels[first + 2]]
    }

    fn light(&self, x: u32, y: u32) -> u32 {
        self.at(x, y).iter().map(|channel| *channel as u32).sum()
    }
}

/// Draws `song` at `beat` with `look` on a card of the stage's own.
fn draw(stage: &mut Stage, renderer: &mut Renderer, song: &Song, beat: f64, look: Look) -> Drawn {
    let format = Format::HD;
    let camera = song.camera(beat, format);
    let picture = stage.paint(song, &camera, &Heard::silence(), format, 1, look);
    renderer.draw(&picture);
    let mut pixels = Vec::new();
    let [width, _] = renderer.read(&mut pixels).unwrap();
    Drawn { pixels, width }
}

fn on_a_card() -> (Stage, Renderer) {
    (Stage::new(Theme::noir()), Renderer::new(Gpu::headless().unwrap(), Quality::FILM))
}

/// The look of a film with nothing in it that differs from frame to frame
/// or darkens the picture: what is left is the light and the focus.
fn lit(light: f32, glow: f32, blur: f32) -> Look {
    Look { light, glow, spread: 0.8, blur, ..Look::plain() }
}

fn rgb(color: egui::Color32) -> [u8; 3] {
    [color.r(), color.g(), color.b()]
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn the_interface_is_drawn_as_it_was_painted_before_anything_is_done_to_it() {
    let (mut stage, mut renderer) = on_a_card();
    let song = in_one_take(Kind::Wide);
    let drawn = draw(&mut stage, &mut renderer, &song, 20.0, Look::plain());
    assert_eq!(drawn.pixels.len(), 1920 * 1080 * 4);
    let theme = Theme::noir();
    // The window's own colors, to the last bit: the background…
    assert_eq!(drawn.at(3, 3), rgb(theme.bg));
    // …and the playhead, which stands where the camera has it.
    let camera = song.camera(20.0, Format::HD);
    let layout = Layout::of(Format::HD.points(), camera.roll_share);
    let axis = layout.axis();
    let x = axis.min + camera.across(camera.beat) as f32 * axis.span();
    let column = (x * Format::HD.pixels_per_point()).round() as u32;
    let row = (layout.lanes.center().y * Format::HD.pixels_per_point()) as u32;
    assert_eq!(drawn.at(column, row), rgb(theme.signal), "at {column}, {row}");
    assert_eq!(drawn.at(column + 1, row), rgb(theme.signal), "two pixels wide at this size");
    assert_ne!(drawn.at(column + 3, row), rgb(theme.signal));
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn what_sounds_is_brighter_and_its_light_spreads_to_what_is_beside_it() {
    let (mut stage, mut renderer) = on_a_card();
    let song = in_one_take(Kind::Close);
    // On a note of the line and a kick.
    let plain = draw(&mut stage, &mut renderer, &song, 20.0, Look::plain());
    let added = draw(&mut stage, &mut renderer, &song, 20.0, lit(1.6, 0.0, 0.0));
    let spread = draw(&mut stage, &mut renderer, &song, 20.0, lit(1.6, 1.0, 0.0));
    let (mut brighter, mut reached, mut darker) = (0u32, 0u32, 0u32);
    for y in (0..1080).step_by(2) {
        for x in (0..1920).step_by(2) {
            let (was, with_light, with_glow) = (plain.light(x, y), added.light(x, y), spread.light(x, y));
            brighter += u32::from(with_light > was);
            darker += u32::from(with_light < was || with_glow < with_light);
            // Where the light itself added nothing, its glow still does.
            reached += u32::from(with_light == was && with_glow > was);
        }
    }
    assert!(brighter > 300, "the notes that sound, the keys they hold, the playhead: {brighter}");
    assert_eq!(darker, 0, "light only ever adds");
    assert!(reached > 4 * brighter, "the glow reaches well beyond what is lit: {reached} against {brighter}");
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn what_is_out_of_focus_is_soft_and_what_is_in_focus_stays_as_it_was() {
    let (mut stage, mut renderer) = on_a_card();
    let song = in_one_take(Kind::Close);
    let sharp = draw(&mut stage, &mut renderer, &song, 20.0, Look::plain());
    let soft = draw(&mut stage, &mut renderer, &song, 20.0, lit(0.0, 0.0, 7.5));

    let camera = song.camera(20.0, Format::HD);
    let layout = Layout::of(Format::HD.points(), camera.roll_share);
    let scale = Format::HD.pixels_per_point();
    let pixels = |points: f32| (points * scale) as u32;
    // How many edges a stretch of a picture has: places where one pixel
    // is clearly not its neighbor. Fine detail is full of them, a blur
    // has none.
    let edges = |drawn: &Drawn, x: std::ops::Range<u32>, y: std::ops::Range<u32>| -> u32 {
        let mut total = 0;
        for row in y {
            for column in x.clone() {
                total += u32::from(drawn.light(column, row).abs_diff(drawn.light(column + 1, row)) > 36);
            }
        }
        total
    };
    // The lanes have stepped back: all of them are soft.
    let axis = layout.axis();
    let lanes_x = pixels(axis.min + 40.0)..pixels(axis.max - 40.0);
    let lanes_y = pixels(layout.lanes.top() + 4.0)..pixels(layout.lanes.bottom() - 4.0);
    let (before, after) = (edges(&sharp, lanes_x.clone(), lanes_y.clone()), edges(&soft, lanes_x, lanes_y));
    assert!(before > 200 && after * 20 < before, "the lanes lost their edges: {before} to {after}");
    // The piano roll about the playhead has the focus: not a pixel moved.
    let playhead = axis.min + camera.across(camera.beat) as f32 * axis.span();
    let near = pixels(playhead - 60.0)..pixels(playhead + 60.0);
    let roll_y = pixels(layout.rolls.top() + 30.0)..pixels(layout.rolls.bottom() - 10.0);
    for row in roll_y.step_by(3) {
        for column in near.clone().step_by(3) {
            assert_eq!(soft.at(column, row), sharp.at(column, row), "at {column}, {row}");
        }
    }
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn a_frame_is_the_same_frame_whenever_it_is_drawn() {
    let (score, _) = score();
    let song = Song::from(score);
    let shoot = Shoot { format: Format::new(640, 360), ..Shoot::fine(Format::HD) };
    let sound = Sound::default();
    let mut filmer = Filmer::new().unwrap();
    let first = filmer.frame(&song, &sound, 9.25, &shoot).unwrap().to_vec();
    // Other frames in between, in another order, on the same card…
    let elsewhere = filmer.frame(&song, &sound, 3.0, &shoot).unwrap().to_vec();
    let again = filmer.frame(&song, &sound, 9.25, &shoot).unwrap().to_vec();
    assert!(first == again, "the frame at 9.25 s differs the second time it is drawn");
    assert!(first != elsewhere);
    // …and from scratch.
    let mut other = Filmer::new().unwrap();
    assert!(other.frame(&song, &sound, 9.25, &shoot).unwrap() == first.as_slice());
    // Its grain is its own: the frame after it has another.
    let next = filmer.frame(&song, &sound, 9.25 + 1.0 / 60.0, &shoot).unwrap().to_vec();
    let differing = first.iter().zip(&next).filter(|(a, b)| a != b).count();
    assert!(differing > first.len() / 8, "{differing} of {} bytes", first.len());
}

/// Half a second of sound at 8 kHz, as a file.
fn write_tone(path: &Path) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 8_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for index in 0..8_000 {
        let sample = ((index as f32 * 0.3).sin() * 9_000.0) as i16;
        writer.write_sample(sample).unwrap();
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}

fn small() -> Shoot {
    Shoot { format: Format::new(480, 270), rate: 30, exposures: 2, shutter: 0.5, oversample: 1 }
}

#[test]
#[ignore = "needs a graphics card and ffmpeg: cargo test -p trill-stage --test drawn -- --ignored"]
fn a_film_is_one_file_with_its_frames_and_its_sound() {
    let folder = tempfile::tempdir().unwrap();
    let (audio, film) = (folder.path().join("mix.wav"), folder.path().join("made/film.mp4"));
    write_tone(&audio);
    let (score, _) = score();
    let song = Song::from(score);
    let progress = Progress::default();
    let mut filmer = Filmer::new().unwrap();
    let filmed = filmer.film(&song, &Sound::default(), 0.25, 0.75, &small(), Some(&audio), &film, &progress).unwrap();
    assert_eq!((filmed.frames, filmed.note.as_deref()), (15, None));
    assert_eq!(filmed.path, film);
    assert_eq!(progress.frames(), (15, 15));
    let written = std::fs::read(&film).unwrap();
    assert!(written.len() > 2_000, "{} bytes", written.len());
    // An MP4 says what it is a few bytes in, and this one has sound.
    assert_eq!(&written[4..8], b"ftyp");
    assert!(written.windows(4).any(|window| window == b"mp4a"), "no audio in the file");
    // A stretch with no length is refused in words.
    let nothing = filmer.film(&song, &Sound::default(), 2.0, 2.0, &small(), None, &film, &progress).unwrap_err();
    assert!(nothing.contains("nothing to film"), "{nothing}");
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn where_there_is_no_ffmpeg_a_film_is_its_frames_as_pictures_and_says_so() {
    let folder = tempfile::tempdir().unwrap();
    let (audio, film) = (folder.path().join("mix.wav"), folder.path().join("film.mp4"));
    write_tone(&audio);
    let (score, _) = score();
    let song = Song::from(score);
    let mut filmer = Filmer::new().unwrap().with_encoder("an-encoder-that-is-not-installed");
    let filmed =
        filmer.film(&song, &Sound::default(), 0.0, 0.2, &small(), Some(&audio), &film, &Progress::default()).unwrap();
    assert_eq!(filmed.frames, 6);
    let frames = folder.path().join("film.frames");
    assert_eq!(filmed.path, frames);
    let note = filmed.note.unwrap();
    assert!(note.contains("ffmpeg was not found") && note.contains("film.frames"), "{note}");
    for number in 1..=6 {
        assert!(frames.join(format!("frame-{number:06}.png")).is_file(), "frame {number}");
    }
    assert!(frames.join("mix.wav").is_file(), "the sound is beside them");
    assert!(!film.exists());
}

#[test]
#[ignore = "needs a graphics card and ffmpeg: cargo test -p trill-stage --test drawn -- --ignored"]
fn a_film_that_is_stopped_leaves_nothing_behind() {
    let folder = tempfile::tempdir().unwrap();
    let film = folder.path().join("film.mp4");
    let (score, _) = score();
    let song = Song::from(score);
    let progress = Progress::default();
    progress.stop();
    let mut filmer = Filmer::new().unwrap();
    let stopped = filmer.film(&song, &Sound::default(), 0.0, 1.0, &small(), None, &film, &progress).unwrap_err();
    assert!(stopped.contains("stopped at frame 0 of 30"), "{stopped}");
    assert!(!film.exists(), "nothing of it was kept");
}
