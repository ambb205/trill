//! Prints how the director reads a song and the takes it plans.
//!
//!     cargo run -p trill-stage --example plan -- SONG.trill
//!
//! Nothing is drawn and nothing is written: this is the plan a film of the
//! song would follow, as text.

use trill_core::Project;
use trill_stage::director::{Plan, Reading};
use trill_stage::score::Score;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: plan SONG.trill");
        std::process::exit(2);
    };
    let project = match std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|json| Project::from_json(&json).map_err(|error| error.to_string()))
    {
        Ok(project) => project,
        Err(reason) => {
            eprintln!("{path} could not be read: {reason}");
            std::process::exit(1);
        }
    };
    let score = Score::read(&project);
    let reading = Reading::of(&score);
    println!(
        "{}: {} bars from {} BPM, {:.1} s; {} parts; {} kicks, {} snares, {} crashes",
        score.name,
        score.bar_count(),
        score.tempo,
        score.duration(),
        score.parts.len(),
        score.hits.kicks.len(),
        score.hits.snares.len(),
        score.hits.crashes.len()
    );
    println!("\nSections (fullness; then each part's notes and how much it is the tune):");
    for (index, section) in score.sections.iter().enumerate() {
        let parts: Vec<String> = score
            .parts
            .iter()
            .enumerate()
            .filter(|(part, _)| reading.activity[index][*part].notes > 0)
            .map(|(part, found)| {
                format!("{} {}/{:.2}", found.name, reading.activity[index][part].notes, reading.tunes[index][part])
            })
            .collect();
        println!(
            "  {:<10} bars {:>3}-{:<3} {:.2}  {}",
            section.name,
            score.bar_number(section.start),
            score.bar_number((section.end - 1e-6).max(section.start)),
            reading.fullness[index],
            parts.join(", ")
        );
    }
    let plan = Plan::direct(&score);
    println!("\nTakes:");
    for take in &plan.takes {
        let rolls: Vec<&str> =
            take.shot.rolls.iter().filter_map(|track| score.part(*track)).map(|part| part.name.as_str()).collect();
        println!(
            "  bars {:>3}-{:<3} {:>6.1}s  {:<10} {:<9} {:<10} window {:>5.1}->{:<5.1} {}",
            score.bar_number(take.start),
            score.bar_number((take.end - 1e-6).max(take.start)),
            score.seconds(take.start),
            score.sections[take.section].name,
            format!("{:?}", take.shot.kind),
            format!("{:?}", take.entry),
            take.shot.window.0,
            take.shot.window.1,
            rolls.join(" + ")
        );
    }
}
