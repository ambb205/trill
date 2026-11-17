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
cargo test --workspace                              # about 2,040 tests, a few minutes once built
cargo test -p trill-engine --test playing           # one integration test file
cargo test -p trill-app a_take_is_counted_in        # tests whose name contains this (the app's are in its binary)
cargo test -p trill-core time_ops -- --nocapture
cargo clippy --workspace --all-targets              # must be clean
cargo fmt --all -- --check                          # rustfmt.toml: 120 columns
sh scripts/warm-worktree.sh                         # first thing in a new git worktree: reuses built libraries
sh crates/trill-app/screenshots/render.sh <word>    # pictures of the window, into target/screenshots
cargo run -p trill-stage --example film -- SONG.trill --still 42 out.png   # a picture of the stage (see the example)
```

- The default test run needs nothing installed and reads nothing of the user's. Tests that need this machine
  (installed plugins, audio devices, MIDI ports, a signed-in Claude Code, a graphics card, ffmpeg) are `#[ignore]`d
  with the command that runs them; `README.md`, "Building and testing", lists them. Run the plugin ones after
  touching `trill-plugins`, and `cargo test -p trill-stage --test drawn -- --ignored` after touching `trill-stage`'s
  `gpu/` or `film.rs`.
- A few tests hold a wall-clock budget and can fail on a machine that is busy with other builds
  (`trill-library` `scale::a_hundred_thousand_files…`, `trill-engine` `session_transport_runs_in_real_time` and
  `changing_a_sample_while_playing_keeps_the_song_going` and
  `the_session_says_how_many_clips_it_is_still_preparing`, its `audio_settings` and `loading` tests, `trill-mcp`
  `an_agent_arms_a_track_plays_it_and_records_a_take`, `trill-assistant`'s `pipeline`, the real-plugin tests). If one is the only failure, run it alone before concluding anything.
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
| `trill-core` | The model and nothing else: `project.rs` (`Project`, tracks, clips with their notes and controller curves, automation, every edit), `travel.rs` (how a control's values lie along its travel, and so how an automation line gets from one point to the next), `tempo.rs` (how beats become time and how they become bars: the tempo map and the meter map of a song whose tempo and time signature change), `time_ops.rs`, `instrument.rs` (what a track knows of its instrument: drums or pitches, drum map, keyswitches, range, octave), `device.rs` (the device API, `EventKind`), `presets.rs`, `midi_file.rs`, `formats.rs` (what a file of an older format meant by what it does not say), and `journal/` (how a song came to be: `mod.rs` the entries and their text, `change.rs` what a step keeps, `place.rs` where it landed, `words.rs` an edit in words, `replay.rs` a session laid out to be watched). No audio or UI dependencies. |
| `trill-dsp` | The built-in devices (`synth`, `drums`, `sampler`, `drumrack`, `effects/`: one file per effect or family, the compressor with its sidechain in `compressor.rs`) and offline time-stretching. `lib.rs` is the registry. |
| `trill-media` | Decoding audio files, the shared `MediaCache`, waveform peaks, resampling, WAV writing. |
| `trill-library` | The sample library (scan, index, search, tags from file names) and the settings file. |
| `trill-theory` | Music theory as pure functions: chord symbols, progressions, voicings, grooves, bass lines, melody notation with its ornaments (`ornament.rs`: as an instrument's keys or the pitch wheel), keyswitches for articulations (`articulation.rs`), key and chord reading. |
| `trill-plugins` | CLAP (`clap/`) and VST3 (`vst3/`) hosting. Per format: `factory`, `instance` (the plugin's main-thread half), `processor` (its audio-thread half), `controller`. Plugins are scanned, and their presets read, in a child process (`scan/`, `presets/`, the `trill-plugin-scan` binary) so one that crashes costs a message. `vendor/` reads the presets some makers keep in files of their own (one module per format; `look.rs` says what a state is, for the `inspect` example) and builds the state that loads one. A preset file that lies anywhere is made a preset by `DeviceFactory::preset_file` (`presets/file.rs`), which is what a path given as a preset comes to. All plugin main-thread work runs on one thread (`main_thread.rs`). `scripted.rs` in each format is a fake plugin for tests. |
| `trill-engine` | `engine.rs` (the stream and its queues), `processor.rs` (the audio thread), `glide.rs` (a gain that is where its envelope has it, frame by frame), `compile.rs`, `graph.rs`, `render.rs` with `analysis.rs` (what a render measures, as it goes) and `ready.rs` (waiting for plugins before one), `probe.rs` (playing an instrument notes to learn what it does), `session.rs` with `session/` (plugin editors, live playing and audio settings, presets, parameters by their text, recording takes, probes, and `story.rs`: the journal the session keeps), `journal.rs` (who is acting on the session, and where journals are kept), `saved.rs` (the sounds a user saved as presets), `instruments.rs` (what is known about a plugin: learned on this machine, or shipped in `instruments/known.rs` from measurements, by its URI), `host/` (audio hosts: the system's, WASAPI exclusive, and `scripted` for tests), `scripted.rs` (instruments for tests that behave like slow plugins), `sources.rs` and `stretcher.rs` (clip audio). |
| `trill-runtime` | `Runtime`: the session, media cache, library, settings, MIDI input ports, safety copies of unsaved work and the housekeeping thread, built once. Both programs start from it. |
| `trill-mcp` | The MCP server and the windowless `trill-mcp` binary. `requests.rs` (tool arguments), `server.rs`, `music.rs` and `sounds.rs` (the tools), `hearing.rs` (`probe`, `measure` and `render`, with their arguments and replies), `instruments.rs` (what a track's notes mean, read and set, and the warnings that come of it), `story.rs` (the journal's tools), `words.rs` (every tool call in plain words, where it landed and what it heard: read by the journal, the server and the window), `view.rs` (replies), `guard.rs` (limits on the in-app assistant's endpoint). |
| `trill-assistant` | The back end of the in-app assistant: runs the user's own Claude Code CLI, one process per message, confined to Trill's tools. |
| `trill-noir` | The egui widget kit and theme ("Instrument Noir"): Bars, Chips, Faders, text, icons, tokens, the mark. |
| `trill-stage` | The stage: the song's own interface, filmed. A frame is a function of the song, what was measured of its sound, a time and the shot, and of nothing else. `score.rs` (what it reads of a project: sections, each part's notes, the drum hits), `director.rs` (the takes: which shot each stretch gets, from the song alone), `camera.rs` (where the camera is at a beat), `light.rs` (what lights when, from the notes), `sound.rs` (what was measured of the audio, which sets the mood), `scene.rs` (the painting, with the kit, in three layers: the interface, what emits light, how far out of focus each part is), `gpu/` (the passes over those layers, on wgpu: bloom, focus blur, exposure from several moments, the finish), `stage.rs` (a song and a time in, a picture out), `film.rs` (frames handed to ffmpeg with the audio). |
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
  `making_of/` (the session played again from its journal), `recovery.rs`, `files.rs`, `stage.rs` (F11: the stage
  in place of the panes. It is drawn by `trill-stage`'s passes on the window's graphics device into a texture; the
  window's own renderer only shows that texture).
- `tests/` drives the real app headlessly: `Harness::new(options, |session| …)` opens it on a hermetic `Runtime`,
  simulated input goes in, and assertions read the session and the widgets' accessibility labels.

### Where a change goes

- **A new edit to the song:** a method on `Project` with tests in `trill-core`; then an MCP tool; then the window.
  Not the other way round: an edit that exists only in the window cannot be reached by an AI.
- **An MCP tool:** an argument struct in `requests.rs` (its doc comments are the schema descriptions the model
  reads, so write them for the model), the tool in `server.rs` or `music.rs`, a compact reply through `view.rs`,
  a test in `tests/mcp.rs` (and its name in `tools_are_listed_with_schemas`). If it changes how a song should be
  written, say so in the `INSTRUCTIONS` texts, which every client reads. Give it words too, and say what it
  made (`trill-mcp/src/words.rs`: `describe`, `of_call`, and `heard` if it listens): the journal enters every
  call under them, the assistant's panel shows them, and a test there fails for a tool that has none.
- **A built-in device:** a module in `trill-dsp` registered in `lib.rs`, and a layout in
  `trill-app/src/ui/device_layouts.rs`. Its parameters' names are the schema a model reads: say the unit, and
  what the ends of the range do.
- **A change to how a built-in device sounds:** a song saved before must sound as it did. A project file holds
  only the parameters that were set, so a new parameter defaults to what the device did before, or the device
  gets a line in `trill-core/src/formats.rs` (what a file of the older format is taken to have said, written into
  it as it is read) and the file format's number goes up. `trill-engine/tests/old_songs.rs` holds every effect to
  the samples a format-1 file rendered to; `tests/effects.rs` holds what they do now to numbers.
- **Routing:** a device may listen to another track (`Device::sidechain`, `Project::set_sidechain`). The tracks
  are played in `Project::playing_order`, each after the tracks that feed it (`Project::feeds`: a group's
  members, a return's senders, a device's key), and the edits refuse a loop. Whatever decides which tracks a
  render makes has to ask `feeds`.
- **Something that changes the song without being an edit** (a state a plugin came to in its own window):
  enter it in the journal as an aside (`Session::note_device`), or the song the journal leads to drifts from the
  song that is open.
- **Something the window remembers:** `Settings` (`trill-library/src/settings.rs`) has fixed fields; state that
  belongs to one part of the window lives in its own file beside it (`browser.json`, `assistant.json`,
  `keyboard.json`).

## Conventions that are particular to this repository

- **A failure is a sentence, not a crash.** A device that vanishes, a plugin that refuses, a file that is gone:
  each ends in a message the user can read (`Session::problems`, `StreamInfo::warning`, a toast) and the song
  keeps playing. No stuck notes: whatever starts a note owns stopping it.
- **A render is not a quick look.** It creates every device afresh and waits for plugins to be ready
  (`ready.rs`), as a probe does: until every note asked for sounds, heard one at a time. To find out what an
  instrument does, use a probe (`Session::probe_job`, the `probe` tool); to check a mix, `RenderJob::measure`
  (the `measure` tool), which writes nothing. A plugin that plays live is not waited for; `Session::loading`
  says whether it may still be reading its samples. A render compiles only the tracks that are heard
  (`Routing::needed` in `compile.rs`), and writes every stem in the one pass when asked
  (`RenderOptions::stems`): never render a song once per track. It is refused, with nothing written, when a
  plugin instrument never answered its notes (`RenderError::NotAnswered`).
- **A line of automation follows its control, and positions are read from the song.** A lane keeps what its
  lines are straight along (`AutomationLane::travel`; `Travel::of_param` says it for a device's parameter); a lane
  that says nothing is straight in its units, which is every lane of a file saved before (format version 1 or 2), and
  must go on playing exactly so. Whoever writes a new lane says how it travels (`Project::write_automation`);
  `set_automation` keeps what a lane has. On the audio thread an envelope is read on a grid of the song's own
  frames (`glide.rs`, `PARAM_STEP`), never once per buffer: the same song is the same samples at any buffer size.
  What mute and solo open and shut (a track's sound, its way to its output, a send) is a `Gate` that multiplies
  the level, over a fixed number of frames: never a ramp across a buffer. Everything there turns a position in
  beats into a value and nothing else; how frames become beats is the transport's business alone (below).
- **Beats become time, and bars, through the two maps.** A song's `tempo` and `time_signature` are what it begins
  with; `tempo_changes` and `meter_changes` are what it does after. Whatever turns a beat into seconds or frames
  asks `Project::tempo_map`, and whatever counts bars asks `Project::meter_map` (`trill-core/src/tempo.rs`):
  nothing multiplies by `60 / project.tempo` or divides by `time_signature.beats_per_bar()` for a position in the
  song. Setting a change moves nothing: beats stay where they are. A song of one tempo and one meter must come
  out to the bit as it did before there were maps, which is why the maps compute what was always computed in
  that case, and why `trill-engine/tests/old_songs.rs` and the buffer-size tests hold it. On the audio thread the
  song moves evenly through a *stretch* (`Processor::begin_stretch`): to the first frame at or past a jump of the
  tempo, or 256 frames of a glide; a block also ends where the time signature changes, so that a device is told
  the tempo and the meter it is in. A note belongs to the block its nearest frame is in. Audio at its own speed
  is anchored in the song's time; audio that follows the tempo is a piece for each tempo it lies under
  (`compile.rs`, `pieces`). Tools that think in bars write the song's own bars (`trill-mcp/src/music.rs`,
  `written_bars`), and the window counts through `timeline::Bars`, which a `TimeSignature` and a `MeterMap` both
  are. `trill-engine/tests/tempo.rs` and `trill-mcp/tests/tempo.rs` are where a change to any of this is proved.
- **The journal is the session's, and it stays on the machine.** Every change to the project goes through
  `Session::make_step`, which enters it in the journal (`session/story.rs`); a thread that acts for a tool call
  says so (`journal::acting`) and its edits are that call's step. Undo and redo are steps too: nothing leaves the
  story. A journal holds a conversation, so it is written only into the program's own folder (`journals`, beside
  the settings; beside a safety copy for unsaved work) and never into a project file or next to one; handing it
  on is `Session::export_journal`, without the conversation unless asked. A replay (`journal::replay`) reads
  nothing but the journal: the same journal and moment are the same frame, which is what a film will be made of.
  The making-of draws the song with the window's own views, which it hands a session that is not the song's.
- **Hermetic by default.** Tests, screenshots and scripts never read or write the user's settings
  (`%APPDATA%\trill`), sample library or safety copies: `RuntimeOptions::hermetic()` or `Storage::At` on a
  temporary folder in tests, `trill-mcp --scratch`, or `--profile <folder>` on either program, by hand.
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
- **Nothing of a plugin maker's is in the repository**: no preset file, no part of one, no dump of a state.
  Tests of `trill-plugins/src/vendor` build their own small files in the format and load them into the scripted
  plugins; the ones on installed plugins are `#[ignore]`d (`tests/vendor.rs`). Trill only reads in a maker's folders.
- **Dependencies** must be GPL-compatible and worth their build time (`cargo build --timings`).
- **Code is written here.** Nothing is copied from the owner's other projects; `trill-noir`, which carries their
  design language, is the one exception.
- **Processes:** several sessions may run `trill` or `trill-mcp` on the same machine. Stop a process by its PID,
  never by image name.
- **Git:** work is committed on `dev` (or a worktree's own branch that is merged into it). Nothing is pushed
  unless the owner asks.
