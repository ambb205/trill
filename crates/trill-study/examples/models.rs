//! The real models on this machine, by hand: for finding out what one
//! does and how long it takes, on parts that were fetched already.
//!
//! ```sh
//! cargo run -p trill-study --example models -- <parts folder> <provider> <what> ...
//!
//!   check                                  the timed test, in a process of its own
//!   stems <audio> <out folder> [best]      the stems of a recording, as WAV files
//!   beats <audio>                          its beats and downbeats
//!   words <vocal audio> [language]         the words of an isolated vocal, placed
//!   study <audio> <studies folder> [best]  a whole study, in a process of its own
//! ```
//!
//! A provider is `processor`, `card` (the first graphics card the system
//! lists; `card1` the second), or `nvidia`. Nothing is fetched: a part
//! that is not in the folder is said.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use trill_core::audio::AudioBuffer;
use trill_listen::{RATE, Sound};
use trill_media::WavFormat;
use trill_study::helper::{Job, Order, Outcome, Said, attend};
use trill_study::lyrics::{self, Envelope};
use trill_study::models::real::{OnnxModels, Provider, Setup};
use trill_study::models::{Models, Which};
use trill_study::room;
use trill_study::run::Programs;
use trill_study::store::{Source, StemChoice, Studies, StudyOptions};

fn provider(text: &str) -> Result<Provider, String> {
    match text {
        "processor" => Ok(Provider::Processor),
        "nvidia" => Ok(Provider::Nvidia),
        other => match other.strip_prefix("card") {
            Some(number) => Ok(Provider::AnyCard {
                adapter: if number.is_empty() {
                    0
                } else {
                    number.parse().map_err(|_| format!("'{other}' is not a card"))?
                },
            }),
            None => Err(format!("'{other}' is not a provider: processor, card, card1 or nvidia")),
        },
    }
}

fn sound(path: &str) -> Result<Sound, String> {
    Ok(Sound::of(&trill_media::decode(Path::new(path)).map_err(|error| error.to_string())?))
}

/// Attends a process that was started for `order` and says what it says.
fn attended(order: &Order, limit: Option<u64>) -> Result<Outcome, String> {
    let program = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut job = Job::start(&program, order)?;
    let began = Instant::now();
    let mut stage_began = Instant::now();
    let (outcome, held) = attend(&mut job, limit, &AtomicBool::new(false), &mut |said| match said {
        Said::Stage(stage) => {
            println!("  {:7.1} s  {}", began.elapsed().as_secs_f64(), stage.doing());
            stage_began = Instant::now();
        }
        Said::Device(device) => println!("  on {device}"),
        _ => {}
    });
    println!(
        "  {:7.1} s in all (the last stage {:.1} s); the most memory held: {}",
        began.elapsed().as_secs_f64(),
        stage_began.elapsed().as_secs_f64(),
        room::said(held.peak)
    );
    Ok(outcome)
}

fn main() -> Result<(), String> {
    trill_study::helper::run_if_requested();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: models <parts folder> <provider> check|stems|beats|words|study ...".to_owned());
    }
    let setup = Setup { parts: PathBuf::from(&args[0]), provider: provider(&args[1])? };
    let rest = &args[3..];
    let need = |count: usize, usage: &str| {
        if rest.len() < count { Err(format!("usage: models <parts> <provider> {usage}")) } else { Ok(()) }
    };
    match args[2].as_str() {
        "check" => match attended(&Order::Check { models: Which::Real(setup), seconds: 30.0 }, None)? {
            Outcome::Checked(checked) => println!(
                "{}: ready in {:.1} s, a minute of music in {:.2} s (sum {:.3})",
                checked.device, checked.ready_seconds, checked.seconds_a_minute, checked.sum
            ),
            other => println!("{other:?}"),
        },
        "stems" => {
            need(2, "stems <audio> <out folder> [best]")?;
            let mix = sound(&rest[0])?;
            let mut models = OnnxModels::new(setup)?;
            let began = Instant::now();
            let choice =
                if rest.get(2).is_some_and(|word| word == "best") { StemChoice::Best } else { StemChoice::Fast };
            let stems = models.stems(&mix, choice, &mut |_| {})?;
            println!(
                "{} | {:.1} s of audio split in {:.2} s",
                models.device(),
                mix.seconds(),
                began.elapsed().as_secs_f64()
            );
            std::fs::create_dir_all(&rest[1]).map_err(|error| error.to_string())?;
            for stem in stems {
                let buffer = AudioBuffer { sample_rate: RATE, left: stem.sound.left, right: stem.sound.right };
                trill_media::write_wav(
                    &Path::new(&rest[1]).join(format!("{}.wav", stem.name)),
                    &buffer,
                    WavFormat::Float32,
                )
                .map_err(|error| error.to_string())?;
            }
        }
        "beats" => {
            need(1, "beats <audio>")?;
            let mix = sound(&rest[0])?;
            let mut models = OnnxModels::new(setup)?;
            let began = Instant::now();
            let found = models.beats(&mix.mono(), &mut |_| {})?;
            println!(
                "{} | {} beats and {} downbeats in {:.2} s",
                models.device(),
                found.beats.len(),
                found.downbeats.len(),
                began.elapsed().as_secs_f64()
            );
            let list = |times: &[f64]| times.iter().map(|time| format!("{time:.3}")).collect::<Vec<_>>().join(",");
            println!("{{\"beats\":[{}],\"downbeats\":[{}]}}", list(&found.beats), list(&found.downbeats));
        }
        "words" => {
            need(1, "words <vocal audio> [language]")?;
            let vocal = sound(&rest[0])?.mono();
            let mut models = OnnxModels::new(setup)?;
            let began = Instant::now();
            let transcript = models.words(&vocal, rest.get(1).map(String::as_str), &mut |_| {})?;
            let heard = began.elapsed().as_secs_f64();
            let mut words = transcript.rough_words();
            let began = Instant::now();
            let placed = models.word_times(&vocal, &mut words, &mut |_| {})?;
            let envelope = Envelope::new(&vocal);
            lyrics::settle(&mut words);
            let all = words.len();
            words.retain(|word| envelope.sung(word.start, word.end));
            envelope.refine(&mut words, placed);
            println!(
                "{} | language {:?}, {} words in {} stretches written down in {heard:.2} s, placed in {:.2} s (placed: {placed}); {} had nothing sung under them",
                models.device(),
                transcript.language,
                all,
                transcript.stretches.len(),
                began.elapsed().as_secs_f64(),
                all - words.len()
            );
            for line in lyrics::lines(words, &envelope) {
                println!("{:8.2} {:8.2}  {}", line.start, line.end, line.text);
            }
        }
        "study" => {
            need(2, "study <audio> <studies folder> [best]")?;
            let source = PathBuf::from(&rest[0]);
            let title = source.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let options = StudyOptions {
                stems: if rest.get(2).is_some_and(|word| word == "best") { StemChoice::Best } else { StemChoice::Fast },
                ..StudyOptions::default()
            };
            let (folder, study) = Studies::new(&rest[1]).create(&title, Source::File { path: source }, options)?;
            println!("study {} in {}", study.id, folder.path.display());
            let programs = Programs { ffmpeg: trill_study::link::find_program("ffmpeg"), fetcher: None };
            let outcome =
                attended(&Order::Study { folder: folder.path.clone(), models: Which::Real(setup), programs }, None)?;
            println!("{outcome:?}");
            for report in folder.study()?.stages {
                println!(
                    "  {:<26} {:?} {:>6} s  {}",
                    report.stage.doing(),
                    report.state,
                    report.seconds.map_or(String::new(), |seconds| format!("{seconds:.1}")),
                    report.note
                );
            }
            println!("  the study takes {} on disk", room::said(folder.size()));
        }
        other => return Err(format!("'{other}' is not something to do: check, stems, beats, words or study")),
    }
    Ok(())
}
