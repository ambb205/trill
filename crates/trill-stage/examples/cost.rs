//! Prints what a frame of the stage costs on this machine, part by part.
//!
//!     cargo run -p trill-stage --example cost -- SONG.trill [--size WIDTHxHEIGHT]
//!
//! Not a test: a measurement, of the song played through at sixty frames a
//! second as the window shows it (one moment a frame, at its own size).

use std::time::{Duration, Instant};
use trill_core::Project;
use trill_noir::Theme;
use trill_stage::gpu::{Gpu, Quality, Renderer};
use trill_stage::{Format, Heard, Song, Stage};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut path, mut format) = (None, Format::HD);
    while let Some(arg) = args.next() {
        if arg == "--size" {
            let size = args.next().unwrap_or_default();
            let (width, height) = size.split_once('x').unwrap_or(("1920", "1080"));
            format = Format::new(width.parse().unwrap_or(1920), height.parse().unwrap_or(1080));
        } else {
            path = Some(arg);
        }
    }
    let Some(path) = path else {
        eprintln!("usage: cost SONG.trill [--size WIDTHxHEIGHT]");
        std::process::exit(2);
    };
    let project = std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|json| Project::from_json(&json).map_err(|error| error.to_string()));
    let project = match project {
        Ok(project) => project,
        Err(reason) => {
            eprintln!("{path} could not be read: {reason}");
            std::process::exit(1);
        }
    };
    let started = Instant::now();
    let song = Song::of(&project);
    println!("reading and directing the song: {:.2} ms", started.elapsed().as_secs_f64() * 1000.0);
    let gpu = match Gpu::headless() {
        Ok(gpu) => gpu,
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    };
    let started = Instant::now();
    let mut renderer = Renderer::new(gpu, Quality::LIVE);
    println!("building the passes: {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    let mut stage = Stage::new(Theme::noir());
    let heard = Heard { level: 0.7, punch: 0.5, tracks: Vec::new() };
    let frames = (song.score.duration() * 60.0) as u32;
    let (mut painting, mut drawing, mut reading) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let (mut worst, mut pixels) = (Duration::ZERO, Vec::new());
    for frame in 0..frames {
        let beat = song.score.beat_at(frame as f64 / 60.0);
        let before = Instant::now();
        let picture = stage.picture(&song, &heard, beat, format, 1, frame);
        let painted = Instant::now();
        renderer.draw(&picture);
        let drawn = Instant::now();
        // Every sixtieth frame is read back, which waits for the card: what
        // the frames before it really took shows there.
        if frame % 60 == 59 {
            renderer.read(&mut pixels).unwrap_or_else(|reason| panic!("{reason}"));
            reading += drawn.elapsed();
        }
        painting += painted - before;
        drawing += drawn - painted;
        worst = worst.max(drawn - before);
    }
    let each = |total: Duration| total.as_secs_f64() * 1000.0 / frames as f64;
    println!(
        "{frames} frames at {}x{}: painting {:.2} ms, handing to the card {:.2} ms, a frame at worst {:.2} ms",
        format.width,
        format.height,
        each(painting),
        each(drawing),
        worst.as_secs_f64() * 1000.0
    );
    let whole = (painting + drawing + reading).as_secs_f64();
    println!(
        "with the card's own time: {:.2} ms a frame, {:.0} frames a second",
        whole * 1000.0 / frames as f64,
        frames as f64 / whole
    );
}
