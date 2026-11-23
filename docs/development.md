# Building, testing and architecture

[`CLAUDE.md`](../CLAUDE.md) at the root of the repository is the map of the code: what lives in which crate,
where a change goes, and the conventions that are particular to Trill. It is written for AI coding agents and
reads as well for people.

## Building and testing

`cargo test --workspace` runs about 2,900 tests in a few minutes. It needs nothing installed and touches nothing
outside the repository and the temporary folder. The tests that do need a particular machine are skipped unless
asked for:

```sh
cargo test -p trill-plugins -- --include-ignored    # against the CLAP and VST3 plugins installed here (minutes)
cargo test -p trill-mcp --test presets -- --include-ignored --nocapture    # an assistant's pass over Surge XT's presets, an amplifier's, a kit, a guitar tone and a patch of their makers', and a bought preset found among the samples
cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1    # what installed plugins say of their parameters, channels and presets
cargo test -p trill-plugins --test key -- --include-ignored --nocapture --test-threads 1    # Surge XT playing what its device listens to, and its effect, as a vocoder, shaping a bass by a muted kick, in both formats
cargo test -p trill-plugins --test silence -- --include-ignored --nocapture --test-threads 1    # what installed effects cost on noise, on silence the host tells them of and on silence it does not: whether one rests when it is fed nothing (two amplifiers and a reverb as it comes; TRILL_SILENCE_PLUGINS names others)
cargo test -p trill-plugins --test tempo -- --include-ignored --nocapture --test-threads 1    # Serum 2's arpeggiator through two changes of tempo: every step heard where the song's tempo map has its beat
cargo test -p trill-plugins --test lifetimes -- --include-ignored --nocapture    # plugin instances made and dropped beside one that stays, an attempt to a process: counts the crashes, and says where each was and what the machine had of memory; half an hour, and no attempt is started on a machine short of memory
cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1    # the makers' own presets of Serum 2, the Ample guitars, Superior Drummer 3 and the Archetypes: listed, loaded by name and by a file's path, and heard to differ
cargo test -p trill-mcp --test controllers -- --include-ignored --nocapture    # a SWAM saxophone and Surge XT played from the controller curves of clips
cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture    # renders and probes of Superior Drummer 3, a SWAM saxophone and an Ample bass; the Ample instruments rendered again to the same samples; a song of installed plugins on one processor and on several
cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture --test-threads 1    # what Trill ships about the Ample guitars and bass and Superior Drummer 3, checked on the plugins themselves: a drum asked for by name and heard on two kits, a slide, a scoop, a harmonic and a bend written by name and heard
cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1    # the Consort plugins (VST3): each of them scanned, made and heard, one after another; the Organ's pedalboard on channel 2 against its manual; one string of the Electric Guitar bent by its own channel's wheel, in a render, in a song that plays and from a start in the middle; and two files to listen to, written through the tools into target/consort-listening
cargo test -p trill-app a_modelled_saxophone -- --include-ignored --nocapture    # the same saxophone typed on the computer keyboard, through the window's own code (no window opens)
cargo test -p trill-mcp --test wavetable against_the_installed -- --ignored --nocapture    # a sawtooth on the wavetable synth beside Serum 2's and Surge XT's as they come: what of each is not on a harmonic, its pitch and its level (a measurement)
cargo test -p trill-mcp --test wavetable the_filters_against -- --ignored --nocapture    # the wavetable synth's low-pass filters beside Surge XT's at the same settings: where each is 3 dB down, how high its peak stands, how steeply it falls (a measurement)
cargo test -p trill-mcp --test hearing a_guitar_through_its_amplifier -- --include-ignored --nocapture    # an Ample guitar through Archetype Tim Henson, saved as one sound in a song and given to a track of another: the same samples in both
cargo test -p trill-dsp --lib share_of_a_core -- --ignored --nocapture    # what the wavetable synth costs, from one voice to thirty-two notes of sixteen copies (a measurement; run it alone)
cargo test -p trill-engine --test bank measured -- --ignored --nocapture    # every number of every sound of the wavetable synth's bank, and the volume that would put each at its kind's level
cargo test -p trill-mcp --test bank serum -- --ignored --nocapture    # Serum 2's factory presets measured by kind on the notes the bank is measured on: the ranges the bank is held to (minutes; it loads the plugin about a hundred times, so check free memory first)
cargo test -p trill-media -- --include-ignored      # decoding audio files found in this machine's usual folders
TRILL_MAGPIE_PROJECTS=<...>/data/projects cargo test -p trill-listen --test magpie -- --ignored --nocapture --test-threads 1    # what Trill hears in recordings that Magpie took apart, against what Magpie found in them (the author's Magpie project and its saved projects; minutes)
TRILL_MAGPIE_PROJECTS=<...>/data/projects TRILL_STUDY_PARTS=<the fetched parts> TRILL_STUDY_ON=nvidia cargo test -p trill-study --test magpie -- --ignored --nocapture --test-threads 1    # those recordings studied with the real models (stems, beats, words and their times) against what Magpie found; needs the parts File, Setup fetches, and runs a graphics card for a minute a recording (processor, card, card2 or nvidia)
TRILL_TEST_PLUGIN_WINDOWS=1 cargo test -p trill-plugins --test editor_windows -- --test-threads=1   # opens windows
TRILL_TEST_CLAUDE=1 cargo test -p trill-assistant --test real_cli    # talks to an installed, signed-in Claude Code
TRILL_TEST_CLAUDE=1 cargo test -p trill-app live_ -- --test-threads=1    # ...through the assistant's panel (seven messages)
cargo test -p trill-engine --test machine -- --include-ignored --nocapture     # opens this machine's audio outputs (silence only) and prints their latency
cargo test -p trill-runtime --test machine -- --include-ignored --nocapture    # lists and opens this machine's MIDI inputs
cargo test -p trill-engine --test cable -- --ignored --nocapture --test-threads 1    # records what it plays through a virtual audio cable ('Line 1 (Virtual Audio Cable)', out and in; TRILL_CABLE names another pair): where a take lands before and after the round trip is measured, shared and exclusive, a ten-minute take's first click and last, and what is left of what was played when it is taken from what was recorded, laid on it to a fraction of a frame (through Trill, and through the cable alone), in the cable's first minute (read by its time stamps as two clocks), once its stamps have said one clock, which each test waits for and prints (TRILL_CABLE_BY_STAMPS=1 does not wait, and reads on as two clocks), and with its two ends said to be one clock from the first (TRILL_CABLE_ONE_CLOCK=1 for the ten-minute take). Plays nothing while another program records from the cable, and opens no other input: never a microphone
cargo test -p trill-engine --lib a_thread_that_asks_is_given -- --ignored --nocapture    # on Linux, asks this machine's RealtimeKit for real time for a thread and gives it up again (the service is the user's, shared with their other programs)
cargo test -p trill-engine --lib a_thread_that_never_pauses -- --ignored --nocapture    # on Linux, a thread given real time by RealtimeKit and kept busy for twice its limit is taken out of real time at half of it, and the program lives and says why
cargo test -p trill-engine --lib the_systems_bin    # sends two small files of its own making to the system's Recycle Bin (macOS's Trash, the Linux desktop's trash), which is where a recording that is removed goes (they stay there until the bin is emptied)
cargo test -p trill-media what_putting_a_take -- --ignored --nocapture    # what putting a take through to the disk every second costs the thread that writes it, on this machine's disk (TRILL_TAKE_FOLDER names another disk)
cargo test -p trill-engine --test journal what_recording_a_step_costs -- --ignored --nocapture    # what keeping the journal costs an edit on a large project (a measurement; run it alone)
cargo test -p trill-stage --test drawn -- --ignored    # what the graphics card makes of the stage, and a film as a file (a graphics card; two of them ffmpeg)
cargo test -p trill-runtime --test together under_wsl -- --ignored --nocapture    # two programs on Windows sharing a song through the meeting point as the static Linux program, run under WSL on this machine: joined, an edit timed, the meeting point stopped and started again (WSL with a Linux distribution; build the program first with `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build -p trill-meet --release --target x86_64-unknown-linux-musl`)
cargo test -p trill-mcp --test mcp a_film_is_made -- --ignored --nocapture    # a film of a song and of its making over MCP, kept in the assistant's working folder (a graphics card)
cargo test -p trill-app a_film_is_made_from_the_window -- --ignored --nocapture    # a film from the window's panel, watched as it is made, and one stopped (a graphics card)
cargo test -p trill-dsp --test real_sfz -- --ignored --nocapture    # the SFZ reader on the files a synth installed here keeps: their text only, counted (TRILL_SFZ_FOLDER names another folder)
cargo test -p trill-runtime --test instruments the_catalogue -- --ignored --nocapture    # fetches five small sets of the catalogue of instruments from where they are published (84 MB, by the system's curl) through the program's own fetching, and checks each twice, finds it in the library, plays it and removes it (TRILL_CATALOGUE_SETS names others by id, or `starter`, or `all`: every set, one at a time, 26 GB and half an hour; TRILL_CATALOGUE_REPORT names a file for a line a set, which a later run goes on from)
cargo test -p trill-runtime --test instruments a_fetch_that_is_stopped -- --ignored --nocapture    # a set that comes as one archive and one that comes file by file, each stopped in the middle and asked for again, with where each went on from (350 MB)
cargo test -p trill-runtime --test instruments a_set_kept_under_a_deep_folder -- --ignored --nocapture    # two small sets fetched into a folder deeper than 260 letters, checked and played (36 MB)
cargo test -p trill-app the_starter_selection_is_fetched_from_the_panel -- --ignored --nocapture    # the starter selection asked for with the Setup panel's own button on the catalogue that is built in: fetched, checked, found by the library, on a track, played (1.8 GB)
cargo run -p trill-fetch --example many -- <a list of addresses> <a folder>    # how long many small files take one at a time, on one connection and several at once: what the engine's count was chosen by
cargo run -p trill-runtime --example files_cross -- 300    # a recording of that many megabytes, made there, crossing between two programs in a shared song while a fader is set in it: what reading it to say what it is costs, how long it takes to arrive, how long an edit takes meanwhile, what the process holds; `--through-here` and `--through ADDRESS KEY` put a meeting point between them; `--count SONG.trill` says what of a song would travel and sends nothing
cargo run -p trill-runtime --example opens -- SONG.trill    # what opening a song takes: when the song was there, when each of its plugins arrived and what its making took, when all were there and when none was still reading; then what the plugin host says of each making (how long it waited for the plugin thread, and its file, the instance, its state and activating), of each plugin taken down, and of the time the plugin thread spent on what plugins post to themselves; it looks at the song as a window does while the devices arrive and says how long the longest look had the session, and what a copy of the song took in the middle (`--add NAME` then gives the song one more track with the plugin of that name, and says how long the edit had the session and when the plugin was there; `empty` in place of a file is a song with nothing in it; `--at-once` makes every device before the song is there, as it was; `--after OTHER.trill` opens another song first, so that its plugins have to be taken down; `long` in place of a file is a long song without plugins made there, with `--sampled FILE.sfz` for tracks of a sampled instrument; it loads the song's plugins, so look at the free memory first)
cargo run -p trill-runtime --example keeps_up -- SONG.trill    # whether the machine keeps up with a song: it is played in real time, with its installed plugins, against a device that makes no sound, and the example says how much of the audio thread's time went (on average and for the worst buffer), how many buffers came late, on how many threads it was made, and what each track cost, playing and standing still (`--threads 1` is the callback alone, `--buffer 128` a small device, `built-in` in place of a file a song of built-in devices made there; `--device` plays through the system's default output as programs share it, handing it silence and asking nothing of it, and says what the device asked for and what its thread is; `--play-at-once` says what the song's first two seconds came to; `--whose-time` says of each track's longest block how long the thread that made it was running; as long as the song lasts, and it loads the song's plugins, so look at the free memory first)
```

Run the `trill-plugins` ones after touching plugin hosting, the `trill-media` one after touching audio decoding,
and the two `machine` ones after touching the audio device or MIDI port code. Run the `cable` one after touching how an
input is opened, read or placed, on a machine that has a virtual audio cable: it opens no other input, and never a
microphone. Run `keeps_up` on a song with plugins
after touching what an audio thread runs or how a block is handed from one thread to another, and hold what it prints
beside the numbers in [the engine's page](engine.md): that a real song is played in time is checked there and nowhere
else. Everything else about devices is tested without any: the engine plays through a
scripted host whose audio thread is the test itself (`trill_engine::host::scripted`), so a device can be unplugged,
refuse a format or stop answering, and a recorded note can be checked to the sample.

The build is set up to be quick to repeat: code that only runs while building (procedural macros, build scripts)
is not optimized, debug information is limited to file and line (`CARGO_PROFILE_DEV_DEBUG=full` for a debugging
session), and on Windows the linker is the one that ships with Rust (`.cargo/config.toml`). Everything from
scratch, tests included, takes about four minutes on a 16-core machine; after an edit, seconds.

### On Linux

Trill builds and its default tests pass on Linux (tried on Ubuntu 26.04, GNOME on Wayland, PipeWire, x86-64). Besides
[Rust](https://rustup.rs) it needs a C compiler, `pkg-config` and ALSA's headers, which on Debian and Ubuntu are:

```sh
sudo apt install build-essential pkg-config libasound2-dev
```

Nothing else is linked at build time. The window loads what it draws with when it starts (Vulkan or OpenGL,
Wayland or X11, xkbcommon, fontconfig), which a desktop has. Sound goes through ALSA, which on a desktop of PipeWire
or PulseAudio is their ALSA plugin, so Trill plays through whatever the system plays through.

A picture of the window (`--screenshot`, `render.sh`) is taken in a window that is never seen and never given the
keyboard: a Wayland desktop lets no window put itself off the screen, so where there is an X server (`DISPLAY`,
which a Wayland desktop has for older programs) the window is made through it, as one the window manager leaves
alone, far off the screen. Without an X server the window is shown while its picture is taken, and the log says so.

What is not done on Linux yet is in [Status](status.md).

Working in a second checkout (`git worktree add`)? `sh scripts/warm-worktree.sh` there copies the libraries the
first one has already built, and the first build compiles only Trill's own crates: a minute and a half.

## Making a release

A release is made on Windows by one script, which publishes nothing:

```sh
sh scripts/release.sh --key FILE --notes NOTES.txt    # into target/release-out/<version>
```

It builds `trill.exe` and `trill-mcp.exe` as a release, gathers them with the licences, and writes
`Trill-<version>-setup.exe` (the installer), `trill-<version>-windows-x86_64.zip` (the same files: what an
installed Trill fetches to update itself), `trill-release.json` (the announcement, signed) and `SHA256SUMS.txt`.
What an installed Trill does with them is in [Installing and updating](installing-and-updating.md).

- **The version** is the workspace's (`[workspace.package]` in `Cargo.toml`): change it there, commit, and tag
  the commit `v<version>`. The script refuses a tree with changes that are not committed, because a release says
  which commit it is.
- **The build** is apart from every other (`target/dist`), with the C runtime linked in, and is marked as a release
  by the commit it is given when it is compiled. Every other build is "a build of your own": it looks for no newer
  Trill and is never replaced, which is why no test and no run from the source ever asks a network anything.
- **What is gathered is checked.** A program keeps the place in the source that each of its messages comes from,
  and those places begin with the home folder of whoever built it: the script has them written as names
  (`cargo`, `rustup`, `trill`), and refuses a program that still holds that folder, that asks for the C runtime's
  DLL, or that does not say (`--version`) that it is this version at this commit. So `--no-build` gathers the
  last build only if it is the one being released, never one of another commit or one made for a trial.
- **The key.** An installed Trill takes an announcement only under a key that is built into it
  (`KEYS` in `crates/trill-update/src/release.rs`). `cargo run -p trill-update --example release -- key FILE`
  makes one: the secret half goes into `FILE`, which is not written over and is never printed, and the line for
  `KEYS` is printed. Keep the file away from the repository and from any machine that builds unattended, and keep a
  copy: a Trill that was built with a key hears of a release only under that key. A second key can be added at
  the end of `KEYS` at any time, and is trusted by the builds made from then on.
- **The installer** is compiled by Inno Setup 6.3 or later from `installer/trill.iss`, where its compiler
  (`ISCC.exe`) is found: where `TRILL_ISCC` says, on the path, or where its own installer puts it. Without it the
  script makes everything else and says so.
- **Publishing** is by hand, and the script prints the command: the files go to a GitHub release of the tag. An
  installed Trill reads `trill-release.json` of the newest release (`releases/latest/download/`), so the moment that
  file is there every one of them hears of the release. A release without it is one nobody is told of.

**Trying the updater out** takes no release and no network. `sh scripts/release.sh --trial KEYFILE` builds a
Trill that trusts a key made for the trial beside its makers', says the version `--trial-version`
gives, looks where `TRILL_UPDATE_ADDRESS` says when it runs, takes a build from an address on this machine, and is
installed as "Trill trial" under an id of its own, never as the Trill a machine really has. Make one, install it
into a folder under `target`, make a second with a higher `--trial-version`, serve its folder on the loopback
(`python -m http.server --bind 127.0.0.1 8765`), and let the first one look, fetch and start again. A trial's
build is never published: the script names its folder `-trial` and says so.

## Architecture

```
crates/
  trill-core      Project model, edit operations, the device API, the journal of a session with its replay, and
                  the song as properties that several programs change at once (`merge`). No audio or UI
                  dependencies.
  trill-dsp       Built-in instruments and effects, and offline time-stretching.
  trill-media     Audio file decoding and caching, waveform overviews, sample-rate conversion, WAV writing.
  trill-engine    Realtime engine, offline renderer, analysis, and the Session.
  trill-library   The sound library (scanning, indexing, searching sample folders) and the settings file.
  trill-fetch     What is fetched when the user asks: parts that others publish, pinned by checksum, brought by the
                  system's own curl, in lists that any crate hands it: one at a time or many asked for at once,
                  a part of many small files several at a time, the room asked of the disk first.
  trill-catalogue The sampled instruments others publish that Trill may hand on, read from the text in
                  `catalogue/`: licences, pins, what was measured, what a song owes. Fetches nothing; `make/`
                  is the scripts the catalogue is written by.
  trill-listen    What is heard in a recording without a model: beats and bars, tuning, key and chords, parts,
                  loops, single hits, notes, and MIDI of them.
  trill-study     A recording taken apart: its stems, beats and words found by models (ONNX Runtime, loaded when
                  a study runs and never linked) in a process of its own, the study that is kept, and the setup.
  trill-plugins   Plugin hosting: scanning and preset discovery in a child process, the plugin main thread, CLAP
                  and VST3, and the readers of the presets some makers keep in files of their own.
  trill-runtime   What both programs share, built once: session, media cache, library, settings, housekeeping,
                  safety copies of unsaved work, and the thread that connects the session to other programs
                  in the same song.
  trill-meet      A meeting point: the small server two Trills that cannot reach each other both connect out
                  to (the `trill-meet` program, one static file for Linux), and the half of it a Trill speaks.
                  The encrypted connection (the Noise protocol, through `snow`) lives here. Nothing of Trill.
  trill-together  The wire between programs that hold the same song: invitations, the messages, the
                  connections of one program, directly or through a meeting point. Nothing of the song.
  trill-theory    Music theory for writing parts: chords, scales, voicings, rhythms, grooves, ornaments,
                  articulations, analysis.
  trill-mcp       MCP server (library) and the headless `trill-mcp` binary.
  trill-assistant Back end of the in-app assistant: runs the user's own Claude Code, kept to Trill's tools, a plan and the web.
  trill-noir      The GUI kit: theme, widgets and painting.
  trill-stage     The stage: the song's own interface filmed. The director, the camera, what lights when, the
                  passes on the graphics card, and a film as a file (frames to ffmpeg, with the song's own sound).
  trill-app       The desktop application (`trill`).
```

Three ideas hold it together:

1. **One front door.** `Session` owns the project, its undo history and the engine. The GUI and the MCP server are
   both thin clients of it, so there is exactly one implementation of every edit and the two can never disagree.
   What an edit changed is found by comparing the song before and after, and is what an undo step keeps and what
   another program in the same song is told ([Working in one song together](working-together.md)).
2. **The audio thread only plays.** Edits happen off the audio thread. After each one the project is compiled into
   an immutable render graph and handed over through a lock-free queue; the audio thread swaps it in between
   blocks, carries running devices and sounding notes across, and sends the old graph back to be freed elsewhere.
3. **Everything is a device.** Built-in instruments, effects and hosted plugins implement one small trait
   (`DeviceProcessor`), so the engine does not know or care which it is driving.

The same `Processor` runs realtime playback and offline rendering. The tests rely on that: they render projects
and assert on the audio (note onsets to the sample, identical output for any buffer size, identical output while
the graph is swapped on every block). The GUI is tested headlessly with simulated input against a real session.
