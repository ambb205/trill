# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Trill is an open-source digital audio workstation in Rust (GPL-3.0-or-later), developed on Windows. Three goals
decide every trade-off: it does not crash or stall, it hosts CLAP and VST3 plugins, and everything it can do is
reachable over MCP so an AI can write and mix a song. `README.md` lists the features and what is not built yet;
this file is the map, so that a task needs only the files it touches.

## Commands

```sh
cargo run -p trill-app -- --demo                    # the window, on a built-in song
cargo run -p trill-mcp -- --no-audio --scratch      # no window: MCP on stdio, nothing read or kept
cargo test --workspace                              # about 1,650 tests, a few minutes once built
cargo test -p trill-engine --test playing           # one integration test file
cargo test -p trill-app a_take_is_counted_in        # tests whose name contains this (the app's are in its binary)
cargo test -p trill-core time_ops -- --nocapture
cargo clippy --workspace --all-targets              # must be clean
cargo fmt --all -- --check                          # rustfmt.toml: 120 columns
sh scripts/warm-worktree.sh                         # first thing in a new git worktree: reuses built libraries
sh crates/trill-app/screenshots/render.sh <word>    # pictures of the window, into target/screenshots
```

- The default test run needs nothing installed and reads nothing of the user's. Tests that need this machine
  (installed plugins, audio devices, MIDI ports, a signed-in Claude Code) are `#[ignore]`d with the command that
  runs them; `README.md`, "Building and testing", lists them. Run the plugin ones after touching `trill-plugins`.
- A few tests hold a wall-clock budget and can fail on a machine that is busy with other builds
  (`trill-library` `scale::a_hundred_thousand_files…`, `trill-engine` `session_transport_runs_in_real_time`, the
  real-plugin tests). If one is the only failure, run it alone before concluding anything.
- Development builds are optimized (`[profile.dev]` in `Cargo.toml`) because unoptimized audio code cannot keep
  up; what only runs while building is not, and debug info is line tables only.
- Do not rename or move a `target` folder: tests of `trill-assistant` and `trill-plugins` have its path built in.

## Architecture

Three rules hold the program together. A change that breaks one is wrong even if it works.

1. **One front door.** `Session` (`trill-engine/src/session.rs`) owns the project, its undo history and the
   engine. The window and the MCP server are thin clients of it, so there is one implementation of every edit.
   An edit is a method on `Project` that validates and returns `EditError`; `Session::edit` runs it, keeps the
   state before as one undo step, compiles the project into a render graph and hands it to the audio thread.
2. **The audio thread only plays.** It never allocates, locks or waits. The project is compiled off it
   (`compile.rs`) into an immutable `Graph`, swapped in through a lock-free queue, and old graphs go back to be
   freed elsewhere. `tests/realtime.rs` in `trill-engine` and `trill-plugins` enforce this with a counting
   allocator: extend them when you touch what the audio thread runs.
3. **Everything is a device.** Built-in instruments, effects and hosted plugins implement `DeviceProcessor`
   (audio thread) and `DeviceController` (elsewhere), made by a `DeviceFactory` under a URI: `builtin:reverb`,
   `clap:<id>`, `vst3:<class id>`. The engine does not know which it is driving.

The same `Processor` runs live playback and offline rendering, which is why tests can render a project and assert
on the samples.

### Crates, from the bottom up

| Crate | What lives there |
|---|---|
| `trill-core` | The model and nothing else: `project.rs` (`Project`, tracks, clips with their notes and controller curves, automation, every edit), `time_ops.rs`, `instrument.rs` (what a track knows of its instrument: drums or pitches, drum map, keyswitches, range, octave), `device.rs` (the device API, `EventKind`), `presets.rs`, `midi_file.rs`. No audio or UI dependencies. |
| `trill-dsp` | The built-in devices (`synth`, `drums`, `sampler`, `drumrack`, `effects/`) and offline time-stretching. `lib.rs` is the registry. |
| `trill-media` | Decoding audio files, the shared `MediaCache`, waveform peaks, resampling, WAV writing. |
| `trill-library` | The sample library (scan, index, search, tags from file names) and the settings file. |
| `trill-theory` | Music theory as pure functions: chord symbols, progressions, voicings, grooves, bass lines, melody notation with its ornaments (`ornament.rs`: as an instrument's keys or the pitch wheel), keyswitches for articulations (`articulation.rs`), key and chord reading. |
| `trill-plugins` | CLAP (`clap/`) and VST3 (`vst3/`) hosting. Per format: `factory`, `instance` (the plugin's main-thread half), `processor` (its audio-thread half), `controller`. Plugins are scanned, and their presets read, in a child process (`scan/`, `presets/`, the `trill-plugin-scan` binary) so one that crashes costs a message. All plugin main-thread work runs on one thread (`main_thread.rs`). `scripted.rs` in each format is a fake plugin for tests. |
| `trill-engine` | `engine.rs` (the stream and its queues), `processor.rs` (the audio thread), `compile.rs`, `graph.rs`, `render.rs` with `analysis.rs` (what a render measures, as it goes) and `ready.rs` (waiting for plugins before one), `probe.rs` (playing an instrument notes to learn what it does), `session.rs` with `session/` (plugin editors, live playing and audio settings, presets, parameters by their text, recording takes, probes), `saved.rs` (the sounds a user saved as presets), `instruments.rs` (what is known about a plugin: learned on this machine, or shipped in `instruments/known.rs` from measurements, by its URI), `host/` (audio hosts: the system's, WASAPI exclusive, and `scripted` for tests), `scripted.rs` (instruments for tests that behave like slow plugins), `sources.rs` and `stretcher.rs` (clip audio). |
| `trill-runtime` | `Runtime`: the session, media cache, library, settings, MIDI input ports, safety copies of unsaved work and the housekeeping thread, built once. Both programs start from it. |
| `trill-mcp` | The MCP server and the windowless `trill-mcp` binary. `requests.rs` (tool arguments), `server.rs`, `music.rs` and `sounds.rs` (the tools), `hearing.rs` (`probe`, `measure` and `render`, with their arguments and replies), `instruments.rs` (what a track's notes mean, read and set, and the warnings that come of it), `view.rs` (replies), `guard.rs` (limits on the in-app assistant's endpoint). |
| `trill-assistant` | The back end of the in-app assistant: runs the user's own Claude Code CLI, one process per message, confined to Trill's tools. |
| `trill-noir` | The egui widget kit and theme ("Instrument Noir"): Bars, Chips, Faders, text, icons, tokens. |
| `trill-app` | The window (binary `trill`), all under `src/ui/`. |

### The window

- `app.rs` is the shell: each frame it brings a `Snapshot` (`snapshot.rs`) in step with the session when the
  session's revision has moved, and views draw from the snapshot. Views never hold the session lock while
  drawing, and nothing slow runs on the UI thread: decoding, scanning and device changes go to workers
  (`work.rs`).
- Views receive a `Cx` (`state.rs`): the snapshot, the theme, the window's own state (`UiState`: selection,
  scroll, zoom, tabs) and ways to act. `cx.edit(|project| …)` is one undo step. A drag is wrapped in
  `Session::begin_gesture`/`end_gesture` so it is one step; an assistant's whole turn in `begin_group`/`end_group`.
- `shortcuts.rs` is one table that the key handling, the menus and the F1 reference all read.
- Areas: `arrangement/` (lanes, clips, headers, ruler, automation envelopes), `piano_roll.rs` (notes, and the lane of
  velocities and controller curves under them), `devices.rs` with
  `device_layouts.rs`, `mixer.rs`, `browser/` and `sounds/`, `audio_clip.rs`, `sample_panels.rs`, `recording.rs`,
  `audio_settings.rs`, `keyboard/` (the computer keyboard as a MIDI keyboard), `assistant/` (the chat panel),
  `recovery.rs`, `files.rs`.
- `tests/` drives the real app headlessly: `Harness::new(options, |session| …)` opens it on a hermetic `Runtime`,
  simulated input goes in, and assertions read the session and the widgets' accessibility labels.

### Where a change goes

- **A new edit to the song:** a method on `Project` with tests in `trill-core`; then an MCP tool; then the window.
  Not the other way round: an edit that exists only in the window cannot be reached by an AI.
- **An MCP tool:** an argument struct in `requests.rs` (its doc comments are the schema descriptions the model
  reads, so write them for the model), the tool in `server.rs` or `music.rs`, a compact reply through `view.rs`,
  a test in `tests/mcp.rs` (and its name in `tools_are_listed_with_schemas`). If it changes how a song should be
  written, say so in the `INSTRUCTIONS` texts, which every client reads.
- **A built-in device:** a module in `trill-dsp` registered in `lib.rs`, and a layout in
  `trill-app/src/ui/device_layouts.rs`.
- **Something the window remembers:** `Settings` (`trill-library/src/settings.rs`) has fixed fields; state that
  belongs to one part of the window lives in its own file beside it (`browser.json`, `assistant.json`,
  `keyboard.json`).

## Conventions that are particular to this repository

- **A failure is a sentence, not a crash.** A device that vanishes, a plugin that refuses, a file that is gone:
  each ends in a message the user can read (`Session::problems`, `StreamInfo::warning`, a toast) and the song
  keeps playing. No stuck notes: whatever starts a note owns stopping it.
- **A render is not a quick look.** It creates every device afresh and waits for plugins to be ready
  (`ready.rs`). To find out what an instrument does, use a probe (`Session::probe_job`, the `probe` tool); to
  check a mix, `RenderJob::measure` (the `measure` tool), which writes nothing.
- **Hermetic by default.** Tests, screenshots and scripts never read or write the user's settings
  (`%APPDATA%\trill`), sample library or safety copies: `RuntimeOptions::hermetic()` or `Storage::At` on a
  temporary folder in tests, `trill-mcp --scratch` and `trill --profile <folder>` by hand.
- **Tests go through the real path** and are named as sentences
  (`a_take_is_counted_in_recorded_and_selected_as_one_undo_step`). Engine tests render audio; app tests drive
  the window; MCP tests talk to a real server over HTTP.
- **Errors say what to do next.** An MCP error names the ids that do exist; a refusal says where a file may be
  written. Replies are compact: a tool that could return thousands of items pages them.
- **The look** comes from `trill-noir`'s theme tokens: near-black surfaces, hairlines, zero corner radius, caps
  micro labels, the signal orange used sparingly, nothing below 10 px. Extend the kit instead of styling in place.
- **Pictures are not kept.** `render.sh <word>` renders the few pictures that show a change into
  `target/screenshots` (off-screen, taking no focus); look at them and leave them there. Never run it for every
  picture as routine. Only the README's four, in `docs/images`, are tracked (`render.sh readme`, on purpose).
- **The assistant keeps five rules**, written at the top of `trill-assistant/src/lib.rs`: it never touches
  credentials, never signs anyone in, never bundles or modifies the CLI, never promises usage limits, and is
  called "the assistant", with "Claude Code" only in descriptive sentences. The only tools it is given are
  Trill's, a plan list and web research; files and the shell stay off because its panel cannot ask permission.
- **Dependencies** must be GPL-compatible and worth their build time (`cargo build --timings`).
- **Code is written here.** Nothing is copied from the owner's other projects; `trill-noir`, which carries their
  design language, is the one exception.
- **Processes:** several sessions may run `trill` or `trill-mcp` on the same machine. Stop a process by its PID,
  never by image name.
- **Git:** work is committed on `dev` (or a worktree's own branch that is merged into it). Nothing is pushed
  unless the owner asks.
