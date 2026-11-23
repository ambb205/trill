//! Plays a song in real time against a device that makes no sound, and
//! says whether the machine kept up: how much of the audio thread's time
//! went (on average, and for the worst buffer), how many buffers took
//! longer to make than they last, how many threads it was made on, and
//! what each track cost. For finding out whether a song of some size is
//! played in time on a machine, before somebody hears that it is not.
//!
//! The device is a stand-in that asks for buffers as a real one does, or,
//! with `--device`, the system's own default output, opened as any
//! program shares it and handed silence: what the song would have sounded
//! like is made and thrown away. Nothing is asked of that device (no
//! rate, no buffer size, never for this program alone), because whoever
//! sits at the machine may be listening to something else on it.
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
//! --device            the system's default output, shared and handed silence, in
//!                     place of the stand-in (and then neither of the next two)
//! --buffer <frames>   how many frames the stand-in asks for at a time (default 480,
//!                     which is what a shared device of Windows asks for at 48 kHz)
//! --rate <hz>         the rate the stand-in runs at (default 48000)
//! --threads <n>       how many threads the song may be made on; 1 is the audio
//!                     callback alone (default: what suits the machine)
//! --always            spread every block from the first, whatever it costs
//! --from <beat>       where the song is started (default 0)
//! --seconds <s>       how long it is played (default: to its end)
//! --play-at-once      starts the song the moment it is open, for two seconds, to
//!                     see what its first buffers come to; then goes on as usual
//! --whose-time        for the longest block of each track, says how much of the time
//!                     on the clock the thread that made it was running: the rest it
//!                     was kept from its work (by the system, or waiting inside a
//!                     device)
//! --no-plugins        the built-in devices only
//! --no-device-lanes   leaves out the lines that move devices' parameters, here and
//!                     for this run only: with them a song's blocks end every 256
//!                     frames, so this says what that costs
//! --monitor <n>       adds n tracks to the song as it is held here, each hearing a
//!                     channel of an input always: the stand-in's own, which hears a
//!                     quiet tone and is handed over by a thread of its own, as a
//!                     device's is. No microphone is opened. Says what an input that
//!                     is heard costs the song
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
use trill_core::{ChainRef, Monitoring, Note, TrackInput, TrackKind};
use trill_engine::host::{AudioHost, NullHost, SystemHost};
use trill_engine::{AudioBackend, Session};
use trill_plugins::Scanner;
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

struct Options {
    song: PathBuf,
    profile: PathBuf,
    device: bool,
    buffer: Option<usize>,
    rate: Option<u32>,
    at_once: bool,
    whose_time: bool,
    threads: Option<usize>,
    always: bool,
    from: f64,
    seconds: Option<f64>,
    plugins: bool,
    device_lanes: bool,
    /// How many tracks that hear an input are added to the song.
    monitor: usize,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let [song, rest @ ..] = arguments else {
        return Err("usage: keeps_up <song.trill> [--profile <folder>] [--device] [--buffer <frames>] [--rate <hz>] \
                    [--threads <n>] [--always] [--from <beat>] [--seconds <s>] [--play-at-once] [--no-plugins] \
                    [--no-device-lanes] [--monitor <n>]"
            .to_string());
    };
    let mut options = Options {
        song: song.into(),
        profile: PathBuf::from("target").join("keeps-up"),
        device: false,
        buffer: None,
        rate: None,
        at_once: false,
        whose_time: false,
        threads: None,
        always: false,
        from: 0.0,
        seconds: None,
        plugins: true,
        device_lanes: true,
        monitor: 0,
    };
    let mut rest = rest.iter();
    while let Some(name) = rest.next() {
        match name.as_str() {
            "--always" => options.always = true,
            "--device" => options.device = true,
            "--play-at-once" => options.at_once = true,
            "--whose-time" => options.whose_time = true,
            "--no-plugins" => options.plugins = false,
            "--no-device-lanes" => options.device_lanes = false,
            _ => {
                let value = rest.next().ok_or(format!("{name} needs a value"))?;
                let number = || value.parse::<f64>().map_err(|_| format!("{name} takes a number, not '{value}'"));
                match name.as_str() {
                    "--profile" => options.profile = value.into(),
                    "--buffer" => options.buffer = Some(number()? as usize),
                    "--rate" => options.rate = Some(number()? as u32),
                    "--threads" => options.threads = Some((number()? as usize).max(1)),
                    "--from" => options.from = number()?,
                    "--seconds" => options.seconds = Some(number()?),
                    "--monitor" => options.monitor = (number()? as usize).min(16),
                    other => return Err(format!("there is no option '{other}'")),
                }
            }
        }
    }
    if options.device && options.monitor > 0 {
        return Err(
            "--monitor is for the stand-in, whose input is made up: with --device no input is opened".to_string()
        );
    }
    if options.device && (options.buffer.is_some() || options.rate.is_some()) {
        return Err(
            "with --device nothing is asked of the device: it runs at the rate and the buffer the system has it at"
                .to_string(),
        );
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
                    notes.push(Note { pitch, start, length: length * 0.9, velocity: 100, ..Default::default() });
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
    let stand_in_rate = options.rate.unwrap_or(48_000);
    let runtime_options = RuntimeOptions {
        // (No MIDI port is opened for a runtime that says it has no device.)
        audio: AudioBackend::Null { sample_rate: stand_in_rate },
        plugins: options.plugins.then_some(Scanner::CurrentExecutable),
        storage,
    };
    let device: Arc<dyn AudioHost> = match options.device {
        // The system's default output as programs share it, given silence.
        // The profile is this run's own and asks nothing of it.
        true => Arc::new(SystemHost::unheard()),
        // A device that asks as a real one does, at the priority a real
        // one has, and plays nothing.
        false => {
            let stand_in = NullHost::like_a_device(stand_in_rate, options.buffer.unwrap_or(480));
            match options.monitor {
                0 => Arc::new(stand_in),
                // And an input of its own that hears a quiet tone: no
                // microphone is opened.
                tracks => Arc::new(stand_in.with_input(tracks as u16)),
            }
        }
    };
    let parts = Parts { audio: Some(device), ..Parts::default() };
    let runtime = Runtime::start_custom(runtime_options, parts).map_err(|error| error.to_string())?;
    if options.device {
        let info = runtime.session.lock().audio_info().clone();
        let Some(name) = info.device.clone() else {
            return Err(format!("the system's device could not be opened: {}", info.warning.unwrap_or_default()));
        };
        println!(
            "the device: '{name}', shared, {} Hz, {} frames a buffer as it was opened ({:.1} ms), heard {:.1} ms after\
             {}",
            info.sample_rate,
            info.buffer_frames,
            info.buffer_ms(),
            info.latency_ms(),
            info.warning.map(|warning| format!("; {warning}")).unwrap_or_default()
        );
        if info.exclusive {
            return Err("the device was opened for this program alone, which this never asks for".to_string());
        }
    }

    let began = Instant::now();
    let made_here = options.song.as_os_str() == BUILT_IN;
    {
        let mut session = runtime.session.lock();
        if let Some(threads) = options.threads {
            session.set_play_threads(Some(threads));
        }
        if options.always {
            session.spread_from(0.0);
        }
        session.count_cycles(options.whose_time);
        if made_here {
            built_in(&mut session)?;
        } else {
            session.load(&options.song).map_err(|error| format!("{}: {error}", options.song.display()))?;
        }
    }
    // Its devices come after the song: until they are all there, looked at
    // fifty times a second for three minutes at the most, with the session
    // left to its chores in between as a front end leaves it.
    for _ in 0..9_000 {
        if runtime.session.lock().arriving().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (seconds, rate, open_at) = {
        let mut session = runtime.session.lock();
        // From here the audio thread has the song.
        let open_at = Instant::now();
        let _ = session.take_load();
        if options.at_once {
            session.seek(options.from);
            session.play();
        }
        if options.monitor > 0 {
            // Whoever asked for --monitor is the hand that switches it on.
            let _hand = trill_engine::hand::by_hand();
            // In the song as it is held here, which is never written: a
            // track for each channel of the stand-in's input, heard always.
            let tracks = options.monitor;
            let edited = session.edit(|project| {
                let mut added = Vec::new();
                for channel in 1..=tracks as u16 {
                    let track = project.add_track(format!("Input {channel}"));
                    let input = TrackInput { channel, stereo: false, monitor: Monitoring::Always };
                    project.set_track_input(track, Some(input))?;
                    project.set_track_volume(track, -30.0)?;
                    added.push(track);
                }
                Ok(added)
            });
            // A track that is new is held, whoever made it: the click on
            // its switch is what lets it be heard.
            for track in edited.map_err(|error| error.to_string())? {
                session.let_input_be_heard(track).map_err(|error| error.to_string())?;
            }
            session.ready_input().map_err(|error| error.to_string())?;
            let open = session.input_status().open.ok_or("the stand-in's input did not open")?;
            println!(
                "added: {tracks} track(s) that hear an input always ('{}', {} channel(s), {} frames a buffer, on a clock \
                 of its own, read {:.1} ms after it was heard)",
                open.device,
                open.channels,
                open.buffer_frames,
                f64::from(open.delay_frames) * 1000.0 / f64::from(stand_in_rate)
            );
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
        (options.seconds.unwrap_or(all), info.sample_rate, open_at)
    };

    // The first two seconds with the song: how long until its blocks are
    // spread, if they come to be, and what the buffers before that came
    // to. Looked at every five milliseconds, four hundred times.
    let (mut buffers, mut late, mut worst) = (0u64, 0u64, 0.0f32);
    let mut spread_at = None;
    for _ in 0..400 {
        std::thread::sleep(Duration::from_millis(5));
        let load = runtime.session.lock().take_load();
        if spread_at.is_none() && load.spread_blocks > 0 {
            spread_at = Some((open_at.elapsed().as_secs_f64(), buffers, late));
        }
        buffers += load.callbacks;
        late += load.late;
        worst = worst.max(load.worst);
    }
    let how = if options.at_once { "playing from the moment it was open" } else { "standing still" };
    match spread_at {
        Some((after, before, late_before)) => println!(
            "the first two seconds, {how}: its blocks were first spread {:.0} ms after it was open (to within 5 ms), \
             {late_before} of the {before} buffers before that came late; {late} of {buffers} in the two seconds, {:.0} % \
             for the worst",
            after * 1e3,
            worst * 100.0
        ),
        None => println!(
            "the first two seconds, {how}: never spread; {late} of {buffers} buffers came late, {:.0} % for the worst",
            worst * 100.0
        ),
    }
    if options.at_once {
        runtime.session.lock().stop();
    }

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
    // What the device asked for and what its thread is.
    let (mut smallest, mut largest, mut gap, mut class, mut level, mut before) =
        (u32::MAX, 0u32, 0u64, None, None, None);
    let mut frames = 0u64;
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
        if load.smallest_frames > 0 {
            smallest = smallest.min(load.smallest_frames);
        }
        largest = largest.max(load.largest_frames);
        frames += load.frames;
        gap = gap.max(load.longest_gap_ns);
        (class, level, before) = (load.audio_class, load.thread_level, load.thread_level_before);
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
    println!(
        "the device asked for {} to {largest} frames at a time ({:.0} on average), and let {:.1} ms pass between two \
         buffers at the longest; its thread is {}, at priority {} by the system's numbers (at {} before Trill asked)",
        if smallest == u32::MAX { 0 } else { smallest },
        frames as f64 / callbacks.max(1) as f64,
        gap as f64 / 1e6,
        match class {
            Some(true) => "in the system's audio class",
            Some(false) => "NOT in the system's audio class, which was asked for and refused",
            None => "as its host has it: the audio class was never asked for",
        },
        level.map_or("unknown".to_string(), |level| level.to_string()),
        before.map_or("unknown".to_string(), |level| level.to_string())
    );
    if let Some(why) = trill_engine::host::priority::refused_because() {
        println!("{why}");
    }
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
    // How many cycles a processor runs in a nanosecond, where blocks' own
    // cycles were counted: from this thread kept busy, five times, the
    // most of them (a thread that was held up meanwhile comes to less).
    let cycles_per_ns = options.whose_time.then(|| {
        let mut most = 0.0f64;
        for _ in 0..5 {
            let (began, before) = (Instant::now(), trill_engine::host::thread_cycles());
            let mut spun = 0u64;
            while began.elapsed() < Duration::from_millis(20) {
                spun = std::hint::black_box(spun.wrapping_add(1));
            }
            let cycles = trill_engine::host::thread_cycles().saturating_sub(before);
            most = most.max(cycles as f64 / began.elapsed().as_nanos() as f64);
        }
        most
    });
    let ran_ms = |cost: &trill_engine::Cost| {
        cycles_per_ns.filter(|rate| *rate > 0.0).map(|rate| cost.longest_ran as f64 / rate / 1e6)
    };
    println!(
        "what each part took: the share of one processor on average / how long its longest block took / its share \
         while the song stood still{}",
        if options.whose_time { " / how long the thread that made that longest block was running in it" } else { "" }
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
            "  {:5.1} % / {:6.2} ms / {:5.1} %  {}{}",
            share * 100.0,
            track.cost.longest as f64 / 1e6,
            standing * 100.0,
            track.name,
            ran_ms(&track.cost).map(|ran| format!("  / ran {ran:.2} ms")).unwrap_or_default()
        );
    }
    println!(
        "  {:5.1} % / {:6.2} ms / {:5.1} %  (the master)",
        costs.master.share(rate) * 100.0,
        costs.master.longest as f64 / 1e6,
        still.master.share(rate) * 100.0
    );
    println!("  {:5.1} %             / {:5.1} %  all the tracks together", sum * 100.0, sum_still * 100.0);
    // Whose time the longest block of all was.
    if let Some(worst) = tracks.iter().max_by_key(|track| track.cost.longest)
        && let Some(ran) = ran_ms(&worst.cost)
    {
        let took = worst.cost.longest as f64 / 1e6;
        println!(
            "the longest block of the run was one of '{}': {took:.2} ms on the clock, of which the thread that made it \
             was running {ran:.2} ms ({:.0} %); for the other {:.2} ms it was kept from its work",
            worst.name,
            ran / took.max(1e-9) * 100.0,
            (took - ran).max(0.0)
        );
    }
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
