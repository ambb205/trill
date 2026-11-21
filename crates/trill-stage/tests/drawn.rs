//! What the graphics card makes of a picture, and what a film is written
//! as. These need a graphics card (and two of them `ffmpeg`), so none of
//! them is part of the default run:
//!
//!     cargo test -p trill-stage --test drawn -- --ignored
//!
//! What is painted, and when, is tested without a card in the crate itself.

use std::path::Path;
use std::sync::Arc;
use trill_core::journal::{Author, Journal, Outcome, Who};
use trill_core::project::AudioContent;
use trill_core::{InstrumentProfile, Note, Project, Role, TrackId};
use trill_noir::Theme;
use trill_stage::director::{Entry, Kind, Plan, Shot, Take};
use trill_stage::film::{Filmer, Playing, Progress, Shoot, Subject};
use trill_stage::gpu::{Gpu, Quality, Renderer};
use trill_stage::making::Making;
use trill_stage::scene::Layout;
use trill_stage::voice::{Wave, voiced};
use trill_stage::{Format, Heard, Look, Picture, Score, Song, Sound, Stage};

/// Sixteen bars: a kick on every beat, a line of eighths, two sections.
fn score() -> (Score, TrackId) {
    let mut project = Project::new("Drawn");
    let drums = project.add_track("Drums");
    let clip = project.add_midi_clip(drums, "beat", 0.0, 64.0).unwrap();
    let kicks: Vec<Note> = (0..64)
        .map(|beat| Note { pitch: 36, start: beat as f64, length: 0.25, velocity: 110, ..Default::default() })
        .collect();
    project.clip_mut(clip).unwrap().add_notes(&kicks).unwrap();
    project.set_track_profile(drums, InstrumentProfile { role: Some(Role::Drums), ..Default::default() }).unwrap();
    let lead = project.add_track("Lead");
    let clip = project.add_midi_clip(lead, "line", 0.0, 64.0).unwrap();
    let line: Vec<Note> = (0..128)
        .map(|step| Note {
            pitch: 60 + (step % 7) as u8,
            start: step as f64 * 0.5,
            length: 0.45,
            velocity: 100,
            ..Default::default()
        })
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

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn a_recording_is_drawn_as_its_waveform_and_is_lit_where_the_playhead_has_just_been() {
    let mut project = Project::new("Sung");
    let voice = project.add_track("Voice");
    project.add_audio_clip(voice, "take", 0.0, 64.0, AudioContent::new("take.wav")).unwrap();
    // The take as it was read: thirty-two seconds of a loud tone.
    let samples: Vec<f32> = (0..32 * 8000).map(|frame| (frame as f32 * 0.3).sin() * 0.8).collect();
    let wave = Arc::new(Wave::of(&samples, &samples, 8000.0));
    let take = |score: Score| {
        let shot = Shot { kind: Kind::Wide, rolls: Vec::new(), window: (32.0, 32.0), roll_share: 0.0 };
        let plan = Plan { takes: vec![Take { start: 0.0, end: score.length, section: 0, shot, entry: Entry::Cut }] };
        Song { score, plan }
    };
    let bare = take(Score::read(&project));
    let sung = take(Score::read(&project).with_voices(voiced(&project, |_| Some(wave.clone()))));
    let (mut stage, mut renderer) = on_a_card();
    let without = draw(&mut stage, &mut renderer, &bare, 20.0, Look::plain());
    let plain = draw(&mut stage, &mut renderer, &sung, 20.0, Look::plain());
    let lit_up = draw(&mut stage, &mut renderer, &sung, 20.0, lit(1.6, 0.0, 0.0));

    let camera = sung.camera(20.0, Format::HD);
    let layout = Layout::of(Format::HD.points(), camera.roll_share);
    let (axis, scale) = (layout.axis(), Format::HD.pixels_per_point());
    let playhead = ((axis.min + camera.across(camera.beat) as f32 * axis.span()) * scale).round() as u32;
    let rows = (layout.lanes.top() * scale) as u32..(layout.lanes.bottom() * scale) as u32;
    let count = |columns: std::ops::Range<u32>, differs: &dyn Fn(u32, u32) -> bool| -> u32 {
        columns.flat_map(|x| rows.clone().map(move |y| (x, y))).map(|(x, y)| u32::from(differs(x, y))).sum()
    };
    // The outline is in the clip: it is not the flat block it is without one.
    let outlined = count(playhead - 300..playhead + 300, &|x, y| plain.at(x, y) != without.at(x, y));
    assert!(outlined > 2_000, "{outlined} pixels of waveform around the playhead");
    // It is lit where the playhead has just been, and not where it has yet to go.
    let brighter = |x, y| lit_up.light(x, y) > plain.light(x, y);
    let (behind, ahead) =
        (count(playhead - 60..playhead - 6, &brighter), count(playhead + 6..playhead + 60, &brighter));
    assert!(behind > 300 && behind > 8 * ahead, "{behind} pixels lit behind the playhead, {ahead} ahead of it");
}

/// A session of four steps with what was asked before them and what was
/// answered after, and the song they come to.
fn story() -> (Journal, Project) {
    let mut song = Project::new("Made");
    let mut journal = Journal::begin(&song, 0);
    let mut at = 0;
    let mut step = |journal: &mut Journal, song: &mut Project, label: &str, change: &dyn Fn(&mut Project)| {
        at += 20_000;
        let before = song.clone();
        change(song);
        let call = journal.begin_call(Author::Assistant, "write", label);
        journal.record(at, Who::Call(call), &before, song, None);
        journal.end_call(at, call, Outcome::default());
    };
    let line = |pitch: u8| -> Vec<Note> {
        (0..16)
            .map(|n| Note {
                pitch: pitch + (n % 5) as u8,
                start: f64::from(n) * 0.5,
                length: 0.4,
                velocity: 100,
                ..Default::default()
            })
            .collect()
    };
    journal.say(1_000, Author::Person, "Write me a bass line and a lead.");
    step(&mut journal, &mut song, "Added the tracks", &|song| {
        song.add_track("Bass");
        song.add_track("Lead");
    });
    let (bass, lead) = (song.tracks[0].id, song.tracks[1].id);
    step(&mut journal, &mut song, "Wrote the bass", &|song| {
        let clip = song.add_midi_clip(bass, "bass", 0.0, 8.0).unwrap();
        song.clip_mut(clip).unwrap().add_notes(&line(36)).unwrap();
    });
    step(&mut journal, &mut song, "Wrote the lead", &|song| {
        let clip = song.add_midi_clip(lead, "lead", 8.0, 8.0).unwrap();
        song.clip_mut(clip).unwrap().add_notes(&line(72)).unwrap();
    });
    journal.say(70_000, Author::Assistant, "Done: a bass line and a lead after it.");
    (journal, song)
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn the_making_of_a_song_is_the_same_frame_for_the_same_moment_however_it_is_come_to() {
    let (journal, finished) = story();
    let mut making = Making::of(journal, &finished, &[], 4.0).unwrap();
    let (length, shoot) = (making.length(), small());
    let mut filmer = Filmer::new().unwrap();
    // The card with what was asked, a step being written, the finished song at the end.
    let moments = [making.film_time(2.0), making.film_time(41.0), length - 1.0];
    let frames: Vec<Vec<u8>> =
        moments.iter().map(|at| filmer.frame_of(&mut making, *at, &shoot).unwrap().to_vec()).collect();
    assert!(frames[0] != frames[1] && frames[1] != frames[2] && frames[0] != frames[2]);
    // Back to where it has been: the story is gone through again, to the same picture.
    for (at, frame) in moments.iter().zip(&frames).rev() {
        assert!(filmer.frame_of(&mut making, *at, &shoot).unwrap() == frame.as_slice(), "the frame at {at} s differs");
    }
    // And from scratch, on another card, straight to the middle.
    let (journal, finished) = story();
    let mut other = Making::of(journal, &finished, &[], 4.0).unwrap();
    assert!(Filmer::new().unwrap().frame_of(&mut other, moments[1], &shoot).unwrap() == frames[1].as_slice());
    // The words of the story are in the picture: while they are asked, a card over the song.
    let (journal, finished) = story();
    let unspoken = Journal::from_text(&journal.to_text(false)).unwrap();
    let mut unspoken = Making::of(unspoken, &finished, &[], 4.0).unwrap();
    let bare = filmer.frame_of(&mut unspoken, moments[0], &shoot).unwrap();
    assert!(bare != frames[0].as_slice(), "what was asked has a place in the frame");
}

/// Two bars of 4/4 at 120, two of 7/8 at 140 and two of 4/4 at 120: a
/// held chord, and a kick on every bar line and at beat 22.
fn changing() -> Song {
    let mut project = Project::new("Changing");
    let drums = project.add_track("Drums");
    let clip = project.add_midi_clip(drums, "beat", 0.0, 23.0).unwrap();
    let kicks: Vec<Note> = [0.0, 4.0, 8.0, 11.5, 15.0, 19.0, 22.0]
        .iter()
        .map(|beat| Note { pitch: 36, start: *beat, length: 0.25, velocity: 127, ..Default::default() })
        .collect();
    project.clip_mut(clip).unwrap().add_notes(&kicks).unwrap();
    project.set_track_profile(drums, InstrumentProfile { role: Some(Role::Drums), ..Default::default() }).unwrap();
    let pad = project.add_track("Pad");
    let clip = project.add_midi_clip(pad, "held", 0.0, 23.0).unwrap();
    project
        .clip_mut(clip)
        .unwrap()
        .add_notes(&[Note { pitch: 60, start: 0.0, length: 23.0, velocity: 90, ..Default::default() }])
        .unwrap();
    project.set_time_signature_at(8.0, 7, 8).unwrap();
    project.set_tempo_at(8.0, 140.0, false).unwrap();
    project.set_time_signature_at(15.0, 4, 4).unwrap();
    project.set_tempo_at(15.0, 120.0, false).unwrap();
    Song::of(&project)
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn a_kick_is_lit_on_the_frame_the_tempo_map_says_it_sounds_on_after_the_song_has_changed_tempo() {
    let song = changing();
    let sound = Sound::default();
    // One moment a frame, so that a frame is of its instant alone.
    let shoot = Shoot::plain(Format::new(640, 360));
    let mut filmer = Filmer::new().unwrap();
    let mut light = |seconds: f64| -> f64 {
        let pixels = filmer.frame(&song, &sound, seconds, &shoot).unwrap();
        pixels.chunks_exact(4).map(|pixel| f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])).sum::<f64>()
            / (pixels.len() / 4) as f64
    };
    let frame = 1.0 / 60.0;
    // The kicks at beats 19 and 22 sound at 9 and 10.5 seconds, after two
    // changes of tempo: the frame of each is brighter than the one before
    // it, and the frames after fall back.
    let mut steps = Vec::new();
    for sounds in [9.0, 10.5] {
        let (before, on, after) = (light(sounds - frame), light(sounds), light(sounds + 12.0 * frame));
        assert!(on > before * 1.01, "the frame at {sounds} s is lit by its kick: {before:.3} then {on:.3}");
        assert!(after < on, "and it falls away: {on:.3} then {after:.3}");
        steps.push(on - before);
    }
    // At one tempo throughout they would have sounded at 9.5 and 11
    // seconds. Nothing is struck there.
    for wrong in [9.5, 11.0 - frame] {
        let step = light(wrong) - light(wrong - frame);
        assert!(step < steps[0] * 0.1, "nothing lights the frame at {wrong:.2} s: {step:.4} against {:.4}", steps[0]);
    }
}

#[test]
#[ignore = "needs a graphics card: cargo test -p trill-stage --test drawn -- --ignored"]
fn a_change_of_tempo_and_meter_is_in_the_picture_from_the_frame_it_arrives_on() {
    let song = changing();
    let (mut stage, mut renderer) = on_a_card();
    // The top right says where the song is: with a time signature for a
    // song that changes it, which changes as the song does.
    let corner = |drawn: &Drawn| -> Vec<[u8; 3]> {
        (1500..1900).flat_map(|x| (8..40).map(move |y| (x, y))).map(|(x, y)| drawn.at(x, y)).collect()
    };
    let just_before = draw(&mut stage, &mut renderer, &song, 7.99, Look::plain());
    let on = draw(&mut stage, &mut renderer, &song, 8.0, Look::plain());
    let differing = corner(&just_before).iter().zip(corner(&on)).filter(|(a, b)| *a != b).count();
    assert!(differing > 60, "the readout says 7/8 and 140 from the beat they arrive on: {differing} pixels changed");
    // A song that changes neither is drawn as it always was: the same
    // corner twice, a beat apart inside a bar, differs only in its count.
    let steady = in_one_take(Kind::Wide);
    let (a, b) = (
        draw(&mut stage, &mut renderer, &steady, 8.0, Look::plain()),
        draw(&mut stage, &mut renderer, &steady, 8.0, Look::plain()),
    );
    assert!(corner(&a) == corner(&b));
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

    // Stopped part of the way, with frames already handed over: nothing is
    // kept, and nothing turns up a moment later either. (Where `ffmpeg` is
    // a launcher for the real one, killing it left the real one to finish
    // the file after it had been removed. The window's test of a stopped
    // film, which has sound and full-size frames, is the one that showed it.)
    struct Stopping<'a> {
        playing: Playing<'a>,
        progress: &'a Progress,
        after: u32,
    }
    impl Subject for Stopping<'_> {
        fn picture(&mut self, stage: &mut Stage, seconds: f64, shoot: &Shoot, seed: u32) -> Picture {
            self.after = self.after.saturating_sub(1);
            if self.after == 0 {
                self.progress.stop();
            }
            self.playing.picture(stage, seconds, shoot, seed)
        }
    }
    let (progress, sound) = (Progress::default(), Sound::default());
    let mut subject = Stopping { playing: Playing { song: &song, sound: &sound }, progress: &progress, after: 40 };
    let stopped = filmer.film_of(&mut subject, 0.0, 4.0, &small(), None, &film, &progress).unwrap_err();
    assert!(stopped.contains("stopped at frame") && !stopped.contains("frame 0 of"), "{stopped}");
    assert!(!film.exists(), "nothing of it was kept");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(!film.exists(), "and nothing of it turned up afterwards");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
}
