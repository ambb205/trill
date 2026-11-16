//! What only this machine can show: its own audio devices, opened for
//! real. Nothing but silence is played, the default output is only ever
//! shared (somebody may be listening to it), and Windows' own settings
//! are left alone.
//!
//! ```sh
//! cargo test -p trill-engine --test machine -- --include-ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_dsp::BuiltinFactory;
use trill_engine::host::SystemHost;
use trill_engine::{AudioBackend, AudioConfig, AudioHost, AudioSetup, Session};
use trill_media::MediaCache;

const HOW_TO_RUN: &str = "opens this machine's audio devices (silence only); run with `cargo test -p trill-engine \
                          --test machine -- --include-ignored --nocapture`";

/// Lets the stream run for a moment and says what it is.
fn report(session: &mut Session, asked: &str, took: Duration) {
    std::thread::sleep(Duration::from_millis(500));
    session.housekeeping();
    let info = session.audio_info().clone();
    println!(
        "  {asked:<44} -> {:<52} buffer {:>5} frames ({:>5.2} ms)  latency {:>6.2} ms  load {:>4.1}%  opened in {:>4} ms{}",
        info.output,
        info.buffer_frames,
        info.buffer_ms(),
        session.output_latency_ms(),
        session.audio_load() * 100.0,
        took.as_millis(),
        info.warning.as_deref().map(|warning| format!("\n      warning: {warning}")).unwrap_or_default(),
    );
}

fn change(session: &mut Session, config: AudioConfig, asked: &str) {
    let started = Instant::now();
    session.set_audio_config(config);
    report(session, asked, started.elapsed());
}

#[test]
#[ignore = "opens this machine's audio devices (silence only); see the top of the file"]
fn this_machines_devices_open_change_and_report_their_latency() {
    let _ = HOW_TO_RUN;
    let listing = Instant::now();
    let devices = SystemHost::new().devices();
    println!(
        "\n{} outputs, {} inputs (listed in {} ms)",
        devices.outputs.len(),
        devices.inputs.len(),
        listing.elapsed().as_millis()
    );
    for device in &devices.outputs {
        println!(
            "  out: {}{}  {} Hz now, opens at {:?}, {} channels",
            device.name,
            if device.is_default { " [default]" } else { "" },
            device.default_sample_rate,
            device.sample_rates,
            device.channels
        );
    }
    for device in &devices.inputs {
        println!(
            "  in:  {}{}  {} Hz",
            device.name,
            if device.is_default { " [default]" } else { "" },
            device.default_sample_rate
        );
    }
    if devices.outputs.is_empty() {
        println!("no audio output on this machine; nothing more to check");
        return;
    }

    // An empty project: whatever is opened plays silence.
    let started = Instant::now();
    let setup = AudioSetup::new(AudioBackend::Auto, AudioConfig::default());
    let mut session = Session::with_audio(Arc::new(BuiltinFactory), Arc::new(MediaCache::default()), setup).unwrap();
    let track = session.edit(|project| Ok(project.add_track("Kept"))).unwrap();
    session.seek(8.0);
    session.play();
    println!("\nthe default output, shared with other programs:");
    report(&mut session, "as the system has it", started.elapsed());
    for frames in [128u32, 256, 1024] {
        change(
            &mut session,
            AudioConfig { buffer_size: Some(frames), ..AudioConfig::default() },
            &format!("{frames} frames"),
        );
    }

    // Every other output: shared, then for this program alone.
    for device in devices.outputs.iter().filter(|device| !device.is_default) {
        println!("\n{}:", device.name);
        let named = AudioConfig { output: Some(device.name.clone()), ..AudioConfig::default() };
        change(&mut session, named.clone(), "shared");
        if !device.exclusive {
            continue;
        }
        for frames in [None, Some(64u32), Some(128), Some(256), Some(512)] {
            let asked = frames.map_or("exclusive".to_string(), |frames| format!("exclusive, {frames} frames"));
            change(&mut session, AudioConfig { buffer_size: frames, exclusive: true, ..named.clone() }, &asked);
        }
        if device.sample_rates.contains(&44_100) && device.default_sample_rate != 44_100 {
            change(
                &mut session,
                AudioConfig { sample_rate: Some(44_100), exclusive: true, ..named.clone() },
                "exclusive, 44100 Hz",
            );
            // The devices were made again; the song starts again from there.
            session.play();
        }
    }

    // A device that is not there, and back to the default.
    println!("\nwhat cannot be had:");
    change(
        &mut session,
        AudioConfig { output: Some("No Such Device".into()), ..AudioConfig::default() },
        "a device that is not there",
    );
    change(&mut session, AudioConfig::default(), "the default again");

    // Through all of it the project stayed and the song kept its place.
    assert!(session.project().track(track).is_ok());
    assert_eq!(session.audio_faults(), 0);
    let before = session.transport();
    std::thread::sleep(Duration::from_millis(300));
    let after = session.transport();
    println!("\nthe song: {before:?} then {after:?}");
    assert!(after.position_beats >= 8.0, "the playhead was kept");
    assert!(after.playing && after.position_beats > before.position_beats, "the stream is running");
}
