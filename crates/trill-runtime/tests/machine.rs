//! What only this machine can show: its own MIDI ports, listed and opened
//! for real. Nothing is sent to them.
//!
//! ```sh
//! cargo test -p trill-runtime --test machine -- --include-ignored --nocapture
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use trill_runtime::midi::{MidiBackend, SystemMidi, parse};

#[test]
#[ignore = "opens this machine's MIDI inputs; see the top of the file"]
fn this_machines_midi_inputs_are_listed_and_can_be_listened_to() {
    let mut midi = SystemMidi;
    let ports = midi.ports().expect("the MIDI inputs can be listed");
    println!("\n{} MIDI input(s)", ports.len());
    if ports.is_empty() {
        println!("  none is attached to this machine");
        return;
    }
    for port in &ports {
        let heard = Arc::new(AtomicU64::new(0));
        let counting = heard.clone();
        let opened = midi.open(
            port,
            Box::new(move |message| {
                if parse(message).is_some() {
                    counting.fetch_add(1, Ordering::Relaxed);
                }
            }),
        );
        match opened {
            Ok(connection) => {
                // Anything played in this second is counted.
                std::thread::sleep(Duration::from_secs(1));
                drop(connection);
                println!("  {}: opened; {} playable messages in a second", port.name, heard.load(Ordering::Relaxed));
            }
            Err(problem) => println!("  {}: not opened: {problem}", port.name),
        }
    }
}
