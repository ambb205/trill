//! The setup by hand: what there is to fetch, fetching a part, and the
//! timed test, with the real parts from where they are published.
//!
//! ```text
//! cargo run -p trill-study --example setup -- FOLDER status
//! cargo run -p trill-study --example setup -- FOLDER fetch runtime
//! cargo run -p trill-study --example setup -- FOLDER remove runtime
//! cargo run -p trill-study --example setup -- FOLDER test
//! cargo run -p trill-study --example setup -- FOLDER programs
//! ```
//!
//! `FOLDER` is where the parts are kept: a folder under `target/` for
//! trying this out, never the user's own. `fetch` asks the network, which
//! nothing else in this repository does unasked.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use trill_fetch::{Ended, PartState, said};
use trill_study::setup::{Setup, provider_name};

fn main() -> Result<(), String> {
    trill_study::helper::run_if_requested();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (Some(folder), Some(what)) = (arguments.first(), arguments.get(1)) else {
        return Err("usage: setup FOLDER status | fetch PART | remove PART | test | programs".to_owned());
    };
    let setup = Setup::real(PathBuf::from(folder));
    match what.as_str() {
        "status" => {
            for list in setup.fetches().status() {
                println!("{} ({})", list.title, list.folder.display());
                for status in list.parts {
                    println!(
                        "  {:<16} {:<9} {:>9} of {:>9}  {:>9} on disk  {}",
                        status.part.id,
                        format!("{:?}", status.state).to_lowercase(),
                        said(status.got),
                        said(status.part.size()),
                        said(status.on_disk),
                        status.part.name
                    );
                    println!("                   {}", status.part.licence);
                }
            }
            match setup.machine() {
                Some(machine) => println!(
                    "the models run on {}{}",
                    provider_name(machine.chosen),
                    if setup.untested() { " (tested on other parts: test again)" } else { "" }
                ),
                None => println!("the timed test has not been run: the models run on the processor"),
            }
        }
        "fetch" => {
            let id = arguments.get(2).ok_or("fetch which part?")?;
            let begun = std::time::Instant::now();
            let mut last = (PartState::Missing, 0u64);
            let ended = setup.fetches().fetch_now(id, &AtomicBool::new(false), &mut |state, got| {
                if state != last.0 || got >= last.1 + (32 << 20) {
                    println!("  {:?} {}", state, said(got));
                    last = (state, got);
                }
            });
            match ended {
                Ok(()) => println!("{id} is here, after {:.1} s", begun.elapsed().as_secs_f64()),
                Err(Ended::Stopped) => println!("stopped"),
                Err(Ended::Failed(why)) => return Err(why),
            }
        }
        "remove" => {
            let id = arguments.get(2).ok_or("remove which part?")?;
            println!("{} freed", said(setup.fetches().remove(id)?));
        }
        "test" => {
            let program = std::env::current_exe().map_err(|error| error.to_string())?;
            let mut last = String::new();
            let machine = setup.test(&program, &AtomicBool::new(false), &mut |trying, _| {
                if trying != last {
                    println!("{trying}");
                    last = trying.to_owned();
                }
            })?;
            for timed in &machine.results {
                match (&timed.checked, &timed.error) {
                    (Some(checked), _) => println!(
                        "{:<44} {:>6.2} s a minute of music, ready in {:>5.1} s, {} at the most, sum {:.4}",
                        timed.device,
                        checked.seconds_a_minute,
                        checked.ready_seconds,
                        said(timed.peak),
                        checked.sum
                    ),
                    (None, why) => {
                        println!("{:<44} did not run: {}", timed.device, why.as_deref().unwrap_or("it did not say"))
                    }
                }
            }
            println!("chosen: {}", provider_name(machine.chosen));
        }
        "programs" => {
            for program in setup.programs() {
                match &program.path {
                    Some(path) => println!("{:<8} {}", program.name, path.display()),
                    None => println!("{:<8} not found: {}", program.name, program.get),
                }
            }
        }
        other => return Err(format!("'{other}' is not something to do: status, fetch, remove, test or programs")),
    }
    Ok(())
}
