//! Plays a song in real time against a device that makes no sound, and
//! says whether the machine kept up: how much of the audio thread's time
//! went (on average, and for the worst buffer), how many buffers took
//! longer to make than they last, how many threads it was made on, and
//! what each track cost. For finding out whether a song of some size is
//! played in time on a machine, before somebody hears that it is not.
//!
//! ```text
//! cargo run -p trill-runtime --example keeps_up -- <song.trill> [options]
//! cargo run -p trill-runtime --example keeps_up -- built-in [options]
//!
//! `built-in` in place of a file is a song made here, of the devices Trill
//! ships and of no file: a drum kit, nine parts of the wavetable synth on
//! sounds of its bank with the effects they bring, two of the poly synth,
//! an equaliser and a compressor on every track, two returns, a line that
//! moves a filter, and a compressor and a limiter on the master. It goes
//! round until the time is up (half a minute unless `--seconds` says).
//!
//! --profile <folder>  where the program keeps what it keeps (settings, the list of
//!                     plugins it found). Default: `target/keeps-up`. Never the
//!                     user's own.
//! --buffer <frames>   how many frames the device asks for at a time (default 480,
//!                     which is what a shared device of Windows asks for at 48 kHz)
//! --rate <hz>         the rate the device runs at (default 48000)
//! --threads <n>       how many threads the song may be made on; 1 is the audio
//!                     callback alone (default: what suits the machine)
//! --always            spread every block from the first, whatever it costs
//! --from <beat>       where the song is started (default 0)
//! --seconds <s>       how long it is played (default: to its end)
//! --no-plugins        the built-in devices only
//! --no-device-lanes   leaves out the lines that move devices' parameters, here and
//!                     for this run only: with them a song's blocks end every 256
//!                     frames, so this says what that costs
//! ```
//!
//! The song is read and never written. It opens with the plugins that are
//! installed, which takes as long as they take to load and as much memory
//! as the song takes in the window: look at what the machine has free
//! first. No window is opened, and nothing is heard.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::project::{AutomationLane, AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, Note, TrackKind};
use trill_engine::host::NullHost;
use trill_engine::{AudioBackend, Session};
use trill_plugins::Scanner;
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

struct Options {
    song: PathBuf,
    profile: PathBuf,
    buffer: usize,
    rate: u32,
    threads: Option<usize>,
    always: bool,
    from: f64,
    seconds: Option<f64>,
    plugins: bool,
    device_lanes: bool,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let [song, rest @ ..] = arguments else {
        return Err("usage: keeps_up <song.trill> [--profile <folder>] [--buffer <frames>] [--rate <hz>] \
                    [--threads <n>] [--always] [--from <beat>] [--seconds <s>] [--no-plugins] [--no-device-lanes]"
            .to_string());
    };
    let mut options = Options {
        song: song.into(),
        profile: PathBuf::from("target").join("keeps-up"),
        buffer: 480,
        rate: 48_000,
        threads: None,
        always: false,
        from: 0.0,
        seconds: None,
        plugins: true,
        device_lanes: true,
    };
    let mut rest = rest.iter();
    while let Some(name) = rest.next() {
        match name.as_str() {
            "--always" => options.always = true,
            "--no-plugins" => options.plugins = false,
            "--no-device-lanes" => options.device_lanes = false,
            _ => {
                let value = rest.next().ok_or(format!("{name} needs a value"))?;
                let number = || value.parse::<f64>().map_err(|_| format!("{name} takes a number, not '{value}'"));
                match name.as_str() {
                    "--profile" => options.profile = value.into(),
                    "--buffer" => options.buffer = number()? as usize,
                    "--rate" => options.rate = number()? as u32,
                    "--threads" => options.threads = Some((number()? as usize).max(1)),
                    "--from" => options.from = number()?,
                    "--seconds" => options.seconds = Some(number()?),
                    other => return Err(format!("there is no option '{other}'")),
                }
            }
        }
    }
    Ok(options)
}

/// The word that stands for the song made here: see the top of the file.
const BUILT_IN: &str = "built-in";

/// Makes the song of built-in devices in `session`: see the top of the
/// file. Sixteen bars at 120, with the loop round all of it.
fn built_in(session: &mut Session) -> Result<(), String> {
    let said = |error: trill_engine::SessionError| error.to_string();
    let (room, echo) = session
        .edit(|project| {
            project.name = "Built-in".to_string();
            project.tempo = 120.0;
            Ok((
                project.add_track_of_kind("Room", TrackKind::Return),
                project.add_track_of_kind("Echo", TrackKind::Return),
            ))
        })
        .map_err(said)?;
    session.add_device(ChainRef::Track(room), "builtin:reverb", None, None).map_err(said)?;
    session.add_device(ChainRef::Track(echo), "builtin:delay", None, None).map_err(said)?;
    // The part, its instrument, its sound if it has one, the lowest note
    // of its chord, how many notes sound together and how long each is.
    type Part = (&'static str, &'static str, Option<&'static str>, u8, usize, f64);
    let parts: [Part; 12] = [
        ("Drums", "builtin:drumkit", None, 36, 1, 0.5),
        ("Bass", "builtin:wavetable", Some("Solid Bass"), 36, 1, 0.5),
        ("Sub", "builtin:wavetable", Some("Deep Sub"), 24, 1, 4.0),
        ("Glass", "builtin:wavetable", Some("Glass Pad"), 60, 4, 4.0),
        ("Strings", "builtin:wavetable", Some("PWM Strings"), 55, 4, 4.0),
        ("Choir", "builtin:wavetable", Some("Choir"), 48, 4, 8.0),
        ("Reese", "builtin:wavetable", Some("Reese"), 33, 1, 1.0),
        ("Acid", "builtin:wavetable", Some("Acid Line"), 45, 1, 0.25),
        ("Growl", "builtin:wavetable", Some("Growl Bass"), 40, 1, 0.5),
        ("Wobble", "builtin:wavetable", Some("Wobble Bass"), 38, 1, 2.0),
        ("Lead", "builtin:polysynth", None, 72, 1, 0.5),
        ("Chords", "builtin:polysynth", None, 60, 3, 2.0),
    ];
    for (index, (name, instrument, sound, lowest, together, length)) in parts.into_iter().enumerate() {
        let track = session.edit(|project| Ok(project.add_track(name))).map_err(said)?;
        session.add_device(ChainRef::Track(track), instrument, sound, None).map_err(said)?;
        session.add_device(ChainRef::Track(track), "builtin:eq", None, None).map_err(said)?;
        session.add_device(ChainRef::Track(track), "builtin:compressor", None, None).map_err(said)?;
        let filter = match index {
            10 => Some(session.add_device(ChainRef::Track(track), "builtin:filter", None, None).map_err(said)?),
            _ => None,
        };
        let edited = session.edit(|project| {
            let clip = project.add_midi_clip(track, name, 0.0, 64.0)?;
            let steps = (64.0 / length) as usize;
            let mut notes = Vec::new();
            for step in 0..steps {
                let start = step as f64 * length;
                // A kit's pattern, or a chord that moves by fourths and
                // thirds every bar.
                let root = match index {
                    0 => [36, 42, 38, 42][step % 4],
                    _ => {
                        lowest
                            + [0, 5, 3, 7][(start / 4.0) as usize % 4]
                            + [0, 0, 12, 7][step % 4] * u8::from(length < 1.0)
                    }
                };
                for voice in 0..together {
                    let pitch = root + [0, 4, 7, 11][voice % 4];
                    notes.push(Note { pitch, start, length: length * 0.9, velocity: 100 });
                }
            }
            project.clip_mut(clip)?.add_notes(&notes)?;
            project.set_send(track, room, Some(-14.0))?;
            if index % 2 == 1 {
                project.set_send(track, echo, Some(-16.0))?;
            }
            if let Some(filter) = filter {
                let cutoff = AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() };
                let line = vec![
                    AutomationPoint::new(0.0, 400.0),
                    AutomationPoint::new(32.0, 8000.0),
                    AutomationPoint::new(64.0, 400.0),
                ];
                project.set_automation(cutoff, line)?;
            }
            Ok(())
        });
        edited.map_err(said)?;
    }
    session.add_device(ChainRef::Master, "builtin:compressor", None, None).map_err(said)?;
    session.add_device(ChainRef::Master, "builtin:limiter", None, None).map_err(said)?;
    session.edit(|project| project.set_loop(true, 0.0, 64.0)).map_err(said)?;
    Ok(())
}

fn run(options: &Options) -> Result<(), String> {
    let storage = Storage::At {
        settings: options.profile.join("settings.json"),
        library_index: Some(options.profile.join("library-index.json")),
    };
    let runtime_options = RuntimeOptions {
        audio: AudioBackend::Null { sample_rate: options.rate },
        plugins: options.plugins.then_some(Scanner::CurrentExecutable),
        storage,
    };
    // A device that asks as a real one does, at the priority a real one
    // has, and plays nothing.
    let device = NullHost::like_a_device(options.rate, options.buffer);
    let parts = Parts { audio: Some(Arc::new(device)), ..Parts::default() };
    let runtime = Runtime::start_custom(runtime_options, parts).map_err(|error| error.to_string())?;

    let began = Instant::now();
    let (seconds, rate) = {
        let mut session = runtime.session.lock();
        let made_here = options.song.as_os_str() == BUILT_IN;
        if made_here {
            built_in(&mut session)?;
        } else {
            session.load(&options.song).map_err(|error| format!("{}: {error}", options.song.display()))?;
        }
        if let Some(threads) = options.threads {
            session.set_play_threads(Some(threads));
        }
        if options.always {
            session.spread_from(0.0);
        }
        if !options.device_lanes {
            // In the song as it is held here, which is never written.
            let moved = |lane: &AutomationLane| matches!(lane.target, AutomationTarget::DeviceParam { .. });
            let lanes = session.project().automation.iter().filter(|lane| moved(lane)).count();
            let edited = session.edit(|project| {
                project.automation.retain(|lane| !moved(lane));
                Ok(())
            });
            edited.map_err(|error| error.to_string())?;
            println!("left out: {lanes} line(s) that move devices' parameters");
        }
        let project = session.project();
        let tempo = project.tempo_map();
        let whole = tempo.seconds_between(options.from, project.length_beats().max(options.from));
        let info = session.audio_info();
        println!(
            "'{}': {} tracks, {} devices, {:.0} s from beat {}; a device of {} frames at {} Hz ({:.1} ms a buffer); it may \
             be made on {} thread(s)",
            project.name,
            project.tracks.len(),
            project.devices().count(),
            whole,
            options.from,
            info.buffer_frames,
            info.sample_rate,
            info.buffer_ms(),
            session.play_threads(),
        );
        for problem in session.problems() {
            println!("  problem: {problem}");
        }
        // The song made here goes round: for half a minute, then.
        let all = if made_here { 30.0 } else { whole + 2.0 };
        (options.seconds.unwrap_or(all), info.sample_rate)
    };

    // Until its plugins have read what they play: a song that is started
    // into a kit that is half there is not the song.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let loading = runtime.session.lock().loading().len();
        if loading == 0 || Instant::now() >= deadline {
            println!("opened and loaded in {:.1} s ({loading} device(s) still loading)", began.elapsed().as_secs_f64());
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // What it costs while it stands still: its devices run all the same.
    std::thread::sleep(Duration::from_secs(3));
    let _ = runtime.session.lock().take_load();
    let _ = runtime.session.lock().take_costs();
    std::thread::sleep(Duration::from_secs(3));
    let idle = runtime.session.lock().take_load();
    println!(
        "standing still: {:.0} % of the audio thread's time on average, {:.0} % at the worst, {} late of {} buffers, on \
         {} thread(s)",
        idle.mean * 100.0,
        idle.worst * 100.0,
        idle.late,
        idle.callbacks,
        idle.threads
    );
    // What each part costs with nothing going into it, to hold beside
    // what it costs when the song plays.
    let still = runtime.session.lock().take_costs();

    {
        let mut session = runtime.session.lock();
        session.seek(options.from);
        session.play();
    }
    let started = Instant::now();
    // Looked at once a second, so that the worst second can be named.
    let (mut callbacks, mut late, mut busy, mut worst, mut worst_at, mut spread) =
        (0u64, 0u64, 0.0f64, 0.0f32, 0.0, 0u64);
    let (mut threads, mut allowed, mut team) = (1, 1, None::<trill_engine::team::TeamStats>);
    let mut late_seconds = 0;
    while started.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_secs(1));
        let load = runtime.session.lock().take_load();
        callbacks += load.callbacks;
        late += load.late;
        late_seconds += u64::from(load.late > 0);
        busy += f64::from(load.mean) * load.callbacks as f64;
        spread += load.spread_blocks;
        if load.worst > worst {
            (worst, worst_at) = (load.worst, started.elapsed().as_secs_f64());
        }
        (threads, allowed) = (threads.max(load.threads), load.allowed);
        if let Some(now) = load.team {
            let all = team.get_or_insert_with(Default::default);
            let (rounds, joined, woken) = (all.rounds + now.rounds, all.joined + now.joined, all.woken + now.woken);
            let mean = |before: f64, of_before: u64, after: f64, of_after: u64| match of_before + of_after {
                0 => 0.0,
                both => (before * of_before as f64 + after * of_after as f64) / both as f64,
            };
            *all = trill_engine::team::TeamStats {
                late_mean_us: mean(all.late_mean_us, all.woken, now.late_mean_us, now.woken),
                late_worst_us: all.late_worst_us.max(now.late_worst_us),
                waited_mean_us: mean(all.waited_mean_us, all.rounds, now.waited_mean_us, now.rounds),
                waited_worst_us: all.waited_worst_us.max(now.waited_worst_us),
                rounds,
                joined,
                woken,
            };
        }
    }
    let costs = runtime.session.lock().take_costs();
    let faults = runtime.session.lock().audio_faults();
    runtime.session.lock().stop();

    let played = started.elapsed().as_secs_f64();
    println!(
        "PLAYED {played:.0} s: {:.1} % of the audio thread's time on average, {:.0} % for the worst buffer (after {:.0} \
         s); {late} of {callbacks} buffers came late, in {late_seconds} of the seconds",
        busy / callbacks.max(1) as f64 * 100.0,
        worst * 100.0,
        worst_at,
    );
    println!(
        "made on up to {threads} thread(s) of the {allowed} it may have and can use; {spread} blocks were spread over \
         more than one; {faults} buffer(s) were lost to a device that failed"
    );
    if let Some(team) = team.filter(|team| team.rounds > 0) {
        println!(
            "the threads beside the callback's: work was handed out {} times and they took it up {} times; {} of those \
             times a thread was asleep, and was at it {:.0} us after it was called on average and {:.0} us at the \
             latest; the callback, with nothing left to do itself, waited for them {:.0} us a time on average and \
             {:.0} us at the longest",
            team.rounds,
            team.joined,
            team.woken,
            team.late_mean_us,
            team.late_worst_us,
            team.waited_mean_us,
            team.waited_worst_us
        );
    }
    println!(
        "what each part took: the share of one processor on average / how long its longest block took / its share \
         while the song stood still"
    );
    let mut tracks = costs.tracks;
    tracks.sort_by_key(|track| std::cmp::Reverse(track.cost.nanos));
    let mut sum = 0.0;
    let mut sum_still = 0.0;
    for track in &tracks {
        let share = track.cost.share(rate);
        let standing = still.tracks.iter().find(|other| other.track == track.track);
        let standing = standing.map_or(0.0, |other| other.cost.share(rate));
        sum += share;
        sum_still += standing;
        println!(
            "  {:5.1} % / {:6.2} ms / {:5.1} %  {}",
            share * 100.0,
            track.cost.longest as f64 / 1e6,
            standing * 100.0,
            track.name
        );
    }
    println!(
        "  {:5.1} % / {:6.2} ms / {:5.1} %  (the master)",
        costs.master.share(rate) * 100.0,
        costs.master.longest as f64 / 1e6,
        still.master.share(rate) * 100.0
    );
    println!("  {:5.1} %             / {:5.1} %  all the tracks together", sum * 100.0, sum_still * 100.0);
    Ok(())
}

fn main() -> ExitCode {
    // Plugin files are scanned by this program started a second time.
    trill_plugins::scan::run_as_scanner_if_requested();
    trill_runtime::jobs::run_as_job_if_requested();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse(&arguments).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}");
            ExitCode::FAILURE
        }
    }
}
