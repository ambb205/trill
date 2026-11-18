# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Trill is an open-source digital audio workstation in Rust (GPL-3.0-or-later), developed on Windows. Three goals
decide every trade-off: it does not crash or stall, it hosts CLAP and VST3 plugins, and everything it can do is
reachable over MCP so an AI can write and mix a song. `README.md` is the front page (what Trill is,
how to build it and how to use it) and stays short; `docs/` describes the features, a page a topic, and
`docs/status.md` what is not built yet; this file is the map, so that a task needs only the files it touches.

## Commands

```sh
cargo run -p trill-app -- --demo                    # the window, on a built-in song
cargo run -p trill-mcp -- --no-audio --scratch      # no window: MCP on stdio, nothing read or kept
cargo test --workspace                              # about 2,650 tests, a few minutes once built
cargo test -p trill-engine --test playing           # one integration test file
cargo test -p trill-app a_take_is_counted_in        # tests whose name contains this (the app's are in its binary)
cargo test -p trill-core time_ops -- --nocapture
cargo clippy --workspace --all-targets              # must be clean
cargo fmt --all -- --check                          # rustfmt.toml: 120 columns
sh scripts/warm-worktree.sh                         # first thing in a new git worktree: reuses built libraries
sh crates/trill-app/screenshots/render.sh <word>    # pictures of the window, into target/screenshots
cargo run -p trill-stage --example film -- SONG.trill --still 42 out.png   # a picture of the stage (see the example)
cargo run -p trill-stage --example film -- SONG.trill --film out.mp4       # the whole song filmed with its own sound
cargo run -p trill-app --example wavetable_listen -- target/wavetable-listening   # the wavetable synth's bank to listen to: a file for each kind of sound, four pieces, and a list
cargo run -p trill-engine --example sampled -- about FILE.sfz   # what a sampled instrument is; also survey, load, measure, play, facts and tour (see the example)
cargo run -p trill-study --example setup -- target/study-parts status              # what a study's models are, and which were fetched there (fetch PART and test are the other words)
cargo run -p trill-study --example models -- target/study-parts nvidia study SONG.wav target/studies   # a recording studied with the real models, stage by stage, with what it took
cargo run -p trill-runtime --example keeps_up -- SONG.trill      # a song played in real time against a device that makes no sound: the load, the late buffers, what each track cost (`built-in` in place of a file is a song made there; see the example)
cargo run -p trill-runtime --example study_listen -- PROFILE STUDY target/study-listening   # what a study does to a song, to listen to: stems, loops at other tempos, a phrase at other pitches, the kit, notes beside their stems, and a list
cargo run -p trill-catalogue --example sets -- list              # the sets of the catalogue of instruments; also sum, files and check (see the example)
```

- The default test run needs nothing installed and reads nothing of the user's. Tests that need this machine
  (installed plugins, audio devices, MIDI ports, a signed-in Claude Code, a graphics card, ffmpeg, the models a
  study runs) are `#[ignore]`d
  with the command that runs them; `docs/development.md`, "Building and testing", lists them. Run the plugin ones after
  touching `trill-plugins`, `cargo test -p trill-stage --test drawn -- --ignored` after touching `trill-stage`'s
  `gpu/` or `film.rs`, and `trill-study`'s `tests/magpie.rs` after touching what its real models are fed or what
  is made of their answers (`models/real/`, `lyrics.rs`).
- A few tests hold a wall-clock budget and can fail on a machine that is busy with other builds
  (`trill-library` `scale::a_hundred_thousand_files…`, `trill-engine` `session_transport_runs_in_real_time` and
  `changing_a_sample_while_playing_keeps_the_song_going` and
  `the_session_says_how_many_clips_it_is_still_preparing`, its `audio_settings` and `loading` tests and
  `a_hundred_and_twenty_eight_voices_of_a_sampled_instrument_cost_less_than_the_time_they_play`, `trill-dsp`
  `eight_notes_of_eight_voices_leave_most_of_a_core`, `trill-mcp`
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
2. **The audio threads only play.** A song that plays is made by the device's callback and, once one thread
   needs more than half a buffer's time for it, by a few threads beside it (`team.rs`). All of them are audio
   threads, in the class the system keeps for audio, and none of them ever allocates, takes a lock, or sleeps
   on anything another thread can hold. Work goes across in atomics; the callback makes tracks like the others
   and waits for them by looking again; the others park between buffers and are woken without the callback
   waiting for anything. A track is made by one thread at a time and by another one the next block, so nothing
   a device or a step of a track's block keeps may be a thread's. The project is compiled off them
   (`compile.rs`) into an immutable `Graph`, which also says what of it can be made at once (`spread.rs`,
   `Graph::spread`); it is swapped in through a lock-free queue, and old graphs and teams go back to be freed
   elsewhere. `tests/realtime.rs` in `trill-engine` and `trill-plugins` enforce this with a counting allocator,
   on every one of those threads: extend them when you touch what an audio thread runs. (A render is not
   played: its threads sleep on a lock, `crew.rs`, and do the same rounds.)
3. **Everything is a device.** Built-in instruments, effects and hosted plugins implement `DeviceProcessor`
   (audio thread) and `DeviceController` (elsewhere), made by a `DeviceFactory` under a URI: `builtin:reverb`,
   `clap:<id>`, `vst3:<class id>`. The engine does not know which it is driving.

The same `Processor` runs live playback and offline rendering, which is why tests can render a project and assert
on the samples.

### Crates, from the bottom up

| Crate | What lives there |
|---|---|
| `trill-core` | The model and nothing else: `project.rs` (`Project`, tracks, clips with their notes and controller curves, automation, every edit; the key a song says it is in, and a track that is a reference: a recording beside the song), `files.rs` (the files a song names: `Project::files`, `Project::relink`, where a folder went read from one file that was found, and the table a project file carries of where its files are from the song's folder and what they were), `travel.rs` (how a control's values lie along its travel, and so how an automation line gets from one point to the next), `tempo.rs` (how beats become time and how they become bars: the tempo map and the meter map of a song whose tempo and time signature change), `time_ops.rs`, `instrument.rs` (what a track knows of its instrument: drums or pitches, drum map, keyswitches, range, octave), `device.rs` (the device API, `EventKind`), `presets.rs` (a device type's presets as something to browse, a preset that was saved, and a *sound*: a preset of its first device that says which effects follow it, with the edit that makes them, `Project::bring_after`), `midi_file.rs`, `formats.rs` (what a file of an older format meant by what it does not say), and `journal/` (how a song came to be: `mod.rs` the entries and their text, `change.rs` what a step keeps, `place.rs` where it landed, `words.rs` an edit in words, `replay.rs` a session laid out to be watched). No audio or UI dependencies. |
| `trill-dsp` | The built-in devices (`synth`, `wavetable/`, `drums`, `sampler`, `drumrack`, `effects/`: one file per effect or family, the compressor with its sidechain in `compressor.rs`) and offline time-stretching. `lib.rs` is the registry. The wavetable synth is a folder: `table.rs` (a table as an oscillator reads it: every frame at every brightness a pitch may have), `recipe.rs` (a table written down as key frames of shapes and harmonics, or read from an audio file), `factory.rs` (the tables that ship, each a recipe), `params.rs` (its parameters, whose names are what a model reads), `voice.rs` (one note: oscillators, filters, the matrix), `mod.rs` (the device, its state document and what all its notes share), `patches/` (its bank, a file for each kind of sound: each sound with its sentence, its tags, the names of its macros, the keys it is for and the effects it brings). The sampled instrument is a reader and a player that know nothing of each other's insides: `sfz/` (`text.rs`: a file as headers and opcodes, with what it includes and defines; `mod.rs`: what they mean, the list of the opcodes that are done, and for one that is not what a listener misses) and `sampled/` (`model.rs` an instrument as the player takes it, `pool.rs` the recordings in memory and the threads that read them, `read.rs` a recording read at another pitch, `loaded.rs` an instrument that is loaded: its state document, its files, what it says of itself; `kit.rs` the drums of a kit by name, `player.rs` the device, `mod.rs` what speaks for it). |
| `trill-media` | Decoding audio files, the shared `MediaCache`, waveform peaks, resampling, WAV writing. `stream.rs` reads a file a piece at a time, and the loop a WAV file carries. `wav_plain.rs` shows the reader a plain WAV file whose format chunk is longer than it takes as the file it stands for (whole sample libraries are written so). |
| `trill-library` | The sample library (scan, index, search, tags from file names) and the settings file. A sampled instrument is one entry that says what it is (`EntryKind::Instrument`, `InstrumentFacts`); its recordings are entries that a search leaves out unless it asks for them (`Entry::of_instrument`, `Query::include_recordings`). |
| `trill-fetch` | What Trill fetches when its user asks, and never otherwise: *parts* that others publish, in lists that any crate hands it (`Fetches::add`, a `List` of `Part`s: size, licence line, credit, source, files). `lib.rs` is the engine: one part at a time in the order they were asked for, several asked for at once (`fetch_many`, refused as a whole where the disk has not the room; `stop_all`), each file by the system's own `curl` (`Curl`) into a name it does not keep until its SHA-256 is the one pinned, an archive unpacked (`zip.rs`: stored or deflated, the 64-bit form, nothing written outside the folder) and, where its own bytes cannot be pinned, sized by guess and checked by what it unpacks to (`Keep::Tree`, `tree_checksum`, `Fetched::sized_by_guess`); a part of many small files fetched several at a time and recorded a batch at a time; the room asked of the disk before a part starts; a second place a part is published (`Part::elsewhere`); stopped and gone on with; where a list is kept is the user's to choose, and a list's `changed` hears of a part that arrived, was removed or was moved. `scripted.rs` is a shelf tests fetch from; `whole.rs` is files written whole, the room on a disk, programs found on the path. `examples/pin.rs` prints what a part is pinned by, `examples/many.rs` measures how many files at once is worth it. No audio, no models. |
| `trill-catalogue` | The sampled instruments others publish that Trill may hand on, read from the text in `catalogue/` at the root of the repository (built in): `lib.rs` (a `Set` as it is fetched and pinned, an `Instrument` with what was measured of it and its `Mark`s, the licences that pass, what was left out and why, a search, the starter selection, `Catalogue::owed`: what is owed for the instruments a song plays), `check.rs` (whether a folder holds a set, whole and as it was read: the tree checksum, each file's own). It fetches nothing and holds no recording. `make/` is how the catalogue is written again, by hand: what was read of each set (`sets.py`, `pins.txt`, `starter.txt`), and the scripts that fetch the pinned commits, measure them through the player and write `catalogue/` (its `README.md` has the steps). |
| `trill-theory` | Music theory as pure functions: chord symbols, progressions, voicings, grooves, bass lines, melody notation with its ornaments (`ornament.rs`: as an instrument's keys or the pitch wheel), keyswitches for articulations (`articulation.rs`), key and chord reading. |
| `trill-listen` | What is heard in a recording without a model, as pure functions of the audio: `beats.rs` (the pulse of the onsets, the `Grid` with its corrections, seconds to beats), `onsets.rs` (where hits begin, to the foot of the attack; beats moved onto them), `harmony.rs` (tuning, a chroma read from three transforms by register, key, chords), `structure.rs` (what each bar sounds like; the parts), `loops.rs`, `hits.rs` (drum hits told apart, one-shots, bass notes, the groove), `pitch.rs` (YIN: a bass line, a melody), `midi.rs` (any of it as a `MidiFile` on the recording's own beats), `spectrum.rs` and `signal.rs` (the transforms and measurements they share). `lib.rs` is `hear`: a `Recording` (one sound, or its stems) in, a `Heard` out. The method is Magpie's (see "Code is written here"). |
| `trill-study` | A recording taken apart, a *study*: its stems, beats and words found by models, then everything `trill-listen` hears, heard in the stems. `store.rs` (a study on disk, a folder each: `study.json`, `stems/`, `outlines.json`, `beats.json`, `words.json`, `lyrics.json`, `heard.json`), `run.rs` (the stages in order, each writing its product whole; one whose product is there is done), `helper.rs` (the process a run is done in: the program started a second time, or for a crate's tests its test program started to run the one test that serves (`serve_by_test`), one `Order` in and marked `Said` lines back, attended, stopped by ending it, ended when it takes more memory than the run was given), `desk.rs` (what a program holds of studies: one at a time in the order asked, refused before it starts where the parts or the memory are not there), `room.rs` (what a run needs, by what was measured), `models/` (`Models` is all a run knows of them: `scripted.rs` for tests, `real/` the networks on ONNX Runtime, loaded when a study is run and never linked: `demucs.rs` stems, `beat.rs` beats, `voice.rs` where someone sings, `whisper.rs` the words, `align.rs` where each is), `lyrics.rs` (words put where they are sung, by the loudness of the vocal: Magpie's), `link.rs` (`ffmpeg` and the program that fetches a link, found and never shipped), `setup/` (`catalogue.rs`: the parts, each pinned, with its licence in its source's words; `mod.rs`: the timed test of what runs the models, and the programs found). The `trill-study-job` binary is the process for tests. The models' runtime is kept out of every other crate. |
| `trill-plugins` | CLAP (`clap/`) and VST3 (`vst3/`) hosting. Per format: `factory`, `instance` (the plugin's main-thread half), `processor` (its audio-thread half), `controller`. Plugins are scanned, and their presets read, in a child process (`scan/`, `presets/`, the `trill-plugin-scan` binary) so one that crashes costs a message. `vendor/` reads the presets some makers keep in files of their own (one module per format; `look.rs` says what a state is, for the `inspect` example) and builds the state that loads one; it also reads out of a drum plugin's state what its kit has on each key (`vendor::key_sounds`, `DeviceController::key_sounds`). A preset file that lies anywhere is made a preset by `DeviceFactory::preset_file` (`presets/file.rs`), which is what a path given as a preset comes to. All plugin main-thread work runs on one thread (`main_thread.rs`). What a plugin is told of the song with each block, in either format, is said and filled in one place (`transport.rs`). `vst3/quirks.rs` holds what a particular plugin needs of the host so as not to take the process down, each with what was measured (`tests/lifetimes.rs` counts crashes, an attempt to a process). `repeats.rs` holds what was measured of plugins that do not do the same thing twice, and what stops them (`examples/takes.rs` is how to measure another). `scripted.rs` in each format is a fake plugin for tests. |
| `trill-engine` | `engine.rs` (the stream and its queues), `processor.rs` (what an audio thread does with a block), `glide.rs` (a gain that is where its envelope has it, frame by frame), `compile.rs`, `graph.rs`, `render.rs` with `analysis.rs` (what a render measures, as it goes), `ready.rs` (waiting for plugins before one) and `spread.rs` (a block made on several threads, for a render and for a song that plays alike: which tracks at once, in what rounds, and what of a track goes on beside the others after its turn) with `crew.rs` (a render's threads, which sleep on a lock) and `team.rs` (the threads a song that plays is made on beside the callback's: audio threads, which never do), `apart.rs` with `apart/` (a render, a measurement or a probe done by the program started a second time: what the two say to one another, the process that serves, what a fault leaves to be read, how much room the machine has), `probe.rs` (playing an instrument notes to learn what it does), `session.rs` with `session/` (plugin editors, live playing and audio settings, `cores.rs`: how many threads a song that plays may be made on, and what it and each of its tracks cost the audio threads, presets, `sounds.rs`: a sound given to a device or a track and saved from one, parameters by their text, recording takes, probes, `wavetable.rs`: a wavetable synth's own tables and the names of its macros, and `story.rs`: the journal the session keeps), `journal.rs` (who is acting on the session, and where journals are kept), `saved.rs` (the presets and sounds a user saved, each a file with the states of its devices beside it), `character.rs` (what is measured of a sound to hold it beside others of its kind: its level, where its spectrum lies, its width, how a note begins and ends; and the notes each kind is measured on), `instruments.rs` (what is known about a plugin: learned on this machine, or shipped in `instruments/known.rs` from measurements, by its URI; `instruments/kit.rs` names the drums of a kit that says what is on its keys, which `Session::profile_of` lays over a track's own map each time it is asked), `host/` (audio hosts: the system's, WASAPI exclusive, and `scripted` for tests; `priority.rs` is the class the system keeps for audio threads, which the callback's thread joins in either host and the team's threads as they start), `scripted.rs` (instruments for tests that behave like slow plugins), `sources.rs` and `stretcher.rs` (clip audio). |
| `trill-runtime` | `Runtime`: the session, media cache, library, settings, MIDI input ports, safety copies of unsaved work and the housekeeping thread, built once. Both programs start from it. `jobs.rs` says which program does a session's renders and probes in a process of its own, and makes the devices there (the `trill-job` binary is that process for tests). `sounds.rs` and `missing.rs` are what both do the same way with sounds: adding one, and looking for the files a song is missing (where their folder went, then by name, each thing found with how sure it is). `listening.rs` is `Runtime::hear`: an audio file listened to (`trill-listen`), kept for the last few files. `taking.rs` is what a study does to a song (a stretch planned and then taken, a hit, a kit, notes, parts, the tempo, a reference; `hear_stretch` plays a plan before it is taken) and `study_files.rs` what a song plays out of studies (who uses a study, dropping stems, gathering beside the song, a word put right). `studying.rs` is `Runtime::studies` (the desk) and `Runtime::fetches` (the engine that fetches, with the models' list on it; another crate's list is added with `Runtime::add_fetch_list`), where studies and each list are kept and how they are moved (`keep_studies_in`, `keep_list_in`: the settings remember, and the list's owner is told); neither studies nor fetches until a program says it is one (`study_and_fetch_as_usual`). `instruments.rs` is the catalogue on this machine: its sets as a list for the engine that fetches (`fetch_list`, a part a set; a set that arrives is made the library's by the list's hook, one that is removed is not), several asked for at once (`Ask`: the starter selection, everything, a kind; `Runtime::ask_for_instruments`), a set handed over as files (`take_set`, `set_arrived`), which instruments of the catalogue are here (`catalogue_here`), and what the open song owes (`Runtime::owed`). |
| `trill-mcp` | The MCP server and the windowless `trill-mcp` binary. `requests.rs` (tool arguments), `server.rs`, `music.rs` and `sounds.rs` (the tools; `sounds.rs` has the search across every preset, `save_preset` and `save_sound`, `edit_preset` and `delete_preset`, and what a reply says of a sound that was given), `hearing.rs` (`probe`, `measure` and `render`, with their arguments and replies), `files.rs` (`missing_files` and `relink_files`, and what the others say of a song that is not whole), `listening.rs` (`listen` and `transcribe`: what is in an audio file or in a study, in the recording's own bars, and MIDI of it), `taking.rs` (`take_from_study`: the arguments turned into what `trill_runtime::taking` takes), `studying.rs` (`study`: a recording taken apart, reported, stopped, listed, removed, its stems dropped, a song's study files gathered; `setup`: every list of parts there is to fetch, what runs the models and the test that finds out; and what the assistant's door refuses of both), `instruments.rs` (what a track's notes mean, read and set, the warnings that come of it, and how a note that says how it is played becomes keyswitches), `story.rs` (the journal's tools), `catalogue.rs` (`instrument_catalogue` and `song_credits`: the instruments to be had, which are here, and what a song owes; they fetch nothing), `film.rs` (`film_plan` and `film`: arguments turned into what `trill-stage` takes), `wavetable.rs` (`describe_wavetable` and `set_wavetable`: a table shown frame by frame in numbers, chosen, made from a recipe or read from a file; and what `get_device` says of the synth: its matrix in words, its macros by name), `words.rs` (every tool call in plain words, where it landed and what it heard: read by the journal, the server and the window), `view.rs` (replies), `guard.rs` (limits on the in-app assistant's endpoint). |
| `trill-assistant` | The back end of the in-app assistant: runs the user's own Claude Code CLI, one process per message, confined to Trill's tools. |
| `trill-noir` | The egui widget kit and theme ("Instrument Noir"): Bars, Chips, Faders, text, icons, tokens, the mark, the Key Map (`keymap.rs`: boxes over keys and how hard they are struck). |
| `trill-stage` | The stage: the song's own interface, filmed. A frame is a function of the song, what was measured of its sound, a time and the shot, and of nothing else. `score.rs` (what it reads of a project: sections, each part's notes, the drum hits, its time and its bars through the tempo and meter maps, and where either changes), `director.rs` (the takes: which shot each stretch gets, from the song alone), `camera.rs` (where the camera is at a beat), `light.rs` (what lights when, from the notes), `sound.rs` (what was measured of the audio, which sets the mood), `scene.rs` (the painting, with the kit, in three layers: the interface, what emits light, how far out of focus each part is), `gpu/` (the passes over those layers, on wgpu: bloom, focus blur, exposure from several moments, the finish), `stage.rs` (a song and a time in, a picture out), `voice.rs` (the outlines of the recordings a song plays, for their waveforms), `film.rs` (a `Subject` has a picture for any moment; its frames are handed to ffmpeg with the audio; `Progress` is how another thread watches a film and stops it), `export.rs` (the whole of a film of a song: its own sound rendered and measured, each part lit by its own level, then filmed), `making.rs` (a journal's replay staged: where the camera goes for each step, the cards, and its film with the finished song under it). |
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
  `device_layouts.rs` (and `wavetable_panel.rs`: the wave an oscillator reads, which is more than its parameters
  say, and `instrument_panel.rs`: what a Sampled Instrument plays), `mixer.rs`, `browser/` and `sounds/`
  (`sounds/instruments.rs`: the library's sampled instruments, and what dropping one does), `sound_presets.rs`
  (sounds of more than one device: which there are for the browser to list, one carried from there to a
  track, the form that names and describes one, and the question before one is deleted; the browser's other
  tab calls audio files sounds), `audio_clip.rs`,
  `sample_panels.rs`, `recording.rs`,
  `audio_settings.rs`, `setup.rs` (File, Setup: a tab for each list of parts there is to fetch, each part with
  what there is to read of it over the button that fetches it, and the machine's tab; `setup_instruments.rs` is
  the tab of the catalogue of instruments, which is too many parts for a list: by kind, with a search, the
  starter selection and everything as two asks, and what is on its way as one thing with one Stop;
  `setup_scene.rs` is made-up machines for its pictures), `missing.rs` (the list of the song's missing files; what it shows is looked for by a worker,
  `sounds/missing.rs`), `keyboard/` (the computer keyboard as a MIDI keyboard), `assistant/` (the chat panel),
  `making_of/` (the session played again from its journal), `study/` (a study in the window: `mod.rs` what the window holds of studies and asks of them, each on a helper
  thread; `view.rs` the workspace, placed by hand in whatever rectangle it is given; `place.rs` the Studies place
  of the browser, the dialog and the question before one goes; `along.rs` what a study found drawn along a
  reference's clip; `window.rs` the workspace in a window of its own and the drag that crosses from it),
  `recovery.rs`, `files.rs`, `stage.rs` (F11: the stage
  in place of the panes. It is drawn by `trill-stage`'s passes on the window's graphics device into a texture; the
  window's own renderer only shows that texture), `filming.rs` (File, Export a film: the panel, and the film made
  on a helper thread on a graphics device of its own), `credits.rs` (File, Credits: what the song owes for its
  sampled instruments, worked out by a worker, and the makers Trill names for the sets it offers).
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
- **Something that writes notes for a track:** what the notes mean is asked of the session
  (`Session::profile_of`), not read off the track alone: on a drum track whose plugin says what is on its keys the
  map is the kit that is loaded, read each time and kept nowhere in the song. A name the kit has not got is
  `Drum::Lacking`: left out and said, never a hit on the key the name usually means. Notes for an instrument with
  articulations go through `trill-mcp/src/instruments.rs`, `played`, which writes a clip's keyswitches for all its
  notes together (`trill_theory::articulation`), so that no note plays in whatever was left chosen. What ships of
  a plugin (`instruments/known.rs`) is what was measured on it, and says so where it was not.
- **A built-in device:** a module in `trill-dsp` registered in `lib.rs`, and a layout in
  `trill-app/src/ui/device_layouts.rs`. Its parameters' names are the schema a model reads: say the unit, and
  what the ends of the range do.
- **The wavetable synth** (`trill-dsp/src/wavetable`): the same song is the same samples, at any buffer size,
  on any number of threads, every time, and the tests hold it to the bit (`wavetable/tests.rs`,
  `trill-engine/tests/instruments.rs`). So: everything that moves a voice is worked out every `TICK` frames of
  the synth's own clock and reached in a line between (`Glide`), never from the start of a block; at a frame the
  order is the tick, then the events, then the frames; what is drawn by lot (start phases, noise, an LFO's
  steps) is drawn from the note; an LFO in time with the song reads the beat from the last block at which the
  song jumped. A parameter that is moved takes 256 frames to get there (`smoothed`); what cannot be moved
  gradually (a table, a filter's kind, a warp, the number of copies) is changed behind the gate
  (`Derived::same_structure`): add to that list whatever else would step. Tables are made off the audio thread:
  the ones that ship once a run, all of them (`factory::Tables`), a device's own when it is created; their order,
  and the order of every choice (sources, destinations, filters, shapes), is the file format, so a new one goes
  at the end. A preset brings a state (`Preset::state`: the names of its macros), and its description is what a
  model chooses it by: say what it sounds like, what it is for and what each macro does. The numbers the synth
  is held to: what is not on a
  harmonic of a sawtooth 80 dB under it at every pitch and 105 dB in the top octave, a voice at the poly synth's
  level, eight notes of eight copies on both oscillators in a twentieth of a core (4 % on an idle machine).
  The synth has no effects inside: a patch that wants an echo or a room is a sound (below) and says which
  devices it brings (`Patch::effects`); every patch is one, the dry ones too, so that a bass takes a pad's
  reverb away. `mono_below` draws what lies under a frequency to the middle, after the voices are added up.
- **The wavetable synth's bank** (`trill-dsp/src/wavetable/patches`, a file a kind): nobody who writes a sound
  for it can hear it, so a sound is held to numbers, through its own effects, as a track plays it
  (`trill-engine/tests/bank.rs`, on the notes `character::playing` gives its kind). A new one is a `Patch` with
  its sentence, tags, macros, range and effects. `cargo test -p trill-engine --test bank measured -- --ignored
  --nocapture` then says the volume that puts it at its kind's level (`level_of`) with the room a full chord at
  full velocity needs, and the tests hold it there; one that has not the room at that level is named with how
  far under it is (`SHORT_OF_ITS_LEVEL`). It lies where its kind lies in a synth that is known to be good
  (`REFERENCE`: a range a kind for seven numbers, measured of Serum 2's factory presets by the same rule in
  `trill-mcp/tests/bank.rs`, which is opt-in), or it is named with why it is meant not to (`OUTSIDE`); a name
  that is no longer needed in either list fails the test. Only ranges are kept of another maker's sounds:
  none is rebuilt, and nothing else of them is in the repository. A bass sets `mono_below`. The synth's own
  tests play every key of a sound's range and move every macro from end to end (`wavetable/tests.rs`), and two
  sounds of a kind differ in more than a knob
  (`the_bank_has_what_a_song_of_each_style_needs_and_no_sound_twice`). `search_presets` is how a sound is
  chosen: `trill-mcp/tests/bank.rs` holds that the words a model asks with find a fitting sound among the first
  answers, so add the words a new sound would be asked for by. The first sixteen sounds were heard by the owner
  and are left as they were heard, their volume aside; nobody has heard the others.
- **A sound** (an instrument and the effects after it, kept and given as one thing): it is a preset of its
  first device that says what follows (`SavedPreset::after`, `Preset::after`, each device a `SoundDevice`), so
  there is no second way to list, search or give one: whatever takes a preset takes a sound, and whatever lists
  presets says what a sound brings (`with`). `None` there is a preset of one device, which touches nothing
  behind it; empty is a sound that is dry and says so. The one edit is `Project::bring_after`: the effects an
  earlier sound brought behind the device go (each carries a mark, `PresetRef::brought_by`, and loses it when
  it is given a preset of its own), the new ones are made directly behind the device, and what was put on the
  chain otherwise stays behind them. Giving is the session's (`add_given`, `give_preset`, `give_track` in
  `session/sounds.rs`): one undo step, and a device this machine has not got, or that does not start, is left
  out and said (`Given::without`), never left in the song as a problem. A sound holds each device's type,
  values, state and whether it is on. It does not hold what belongs to one song (the track a device listens to,
  sends, the fader, automation, what the track's notes mean), and saving says what it left out. Each device of
  a saved sound is read by the format the sound was saved in, as a saved preset is (`formats::keep_complete`).
  What was saved is renamed, described and deleted through the session (`Session::change_saved_preset`, which
  takes the open song's devices with the new name in one undo step, and `delete_saved_preset`). The store is
  files and not in the undo history: a front end asks before it deletes, and the in-app assistant's endpoint,
  whose panel cannot ask, refuses.
  A front end does nothing of its own with a sound: it names the sound and the track.
- **The sampled instrument** (`trill-dsp/src/sampled`, read by `trill-dsp/src/sfz`): the reader knows nothing of
  playing and the player nothing of SFZ. An opcode that is added is read in `sfz/mod.rs`, added to the list at
  the top of that file, taken out of `missed`, and played in `player.rs`. One that is not done is counted by its
  name and said with what a listener misses (`Loaded::unsupported`): never passed over without a word, and never
  a reason not to load. A region no key sounds and a controller sets off (the noise of a pedal) is set off when
  the controller comes into its span, once, at a place in the song like any note. The same song is the same samples at any buffer size and on any number of threads:
  everything that moves a voice is worked out every `TICK` frames of the device's own clock; recordings taken
  in turn count from where playing began (`reset`), and one taken at random is drawn from the key and the
  note's place in the song. The audio thread plays what is in memory and nothing else (`pool.rs`): the start of
  every recording, read by workers when the instrument loads, and whole recordings as workers hand them over,
  within a budget; it looks with `try_lock`, asks with an atomic, and never frees a recording. A device made
  for a render (`Purpose::Rendering`) reads what it plays where it stands; one that plays never waits, and says
  how far its reading is and what was silent or cut short for it (`DeviceController::reading`). What the
  instrument says of itself (`DeviceController::sampled`: its range, drums or pitches, its ways of playing by
  name) is laid under the track's own words by `Session::profile_of`, each time it is asked, and kept nowhere
  in the song. Its file is a preset of the device (`DeviceFactory::preset_state`): that is how a path given
  as a preset, a row of the browser and a file dropped on a track all load it. Nothing that is anyone's
  recording is in the repository: tests write their own instruments (`write_instrument` in
  `trill-engine/tests/common`), and the one that reads real files reads their text only and is opt-in
  (`trill-dsp/tests/real_sfz.rs`). The numbers it is held to (`trill-engine/tests/instruments.rs`): every key
  within a hundredth of a cent of its pitch, what folds 70 dB under what belongs up to two octaves above a
  recording's pitch, no click at a loop, a release, a taken voice or a layer crossing, and 128 voices in less
  than the time they play.
- **The catalogue of instruments** (`catalogue/`, read by `trill-catalogue`; `catalogue/README.md` is its rule
  book): text about instruments others publish, and never one of their recordings or files. A set is in it only
  if its licence lets anyone hand it on (public domain, or asking no more than to be named), read at its source at
  the pinned commit and quoted word for word; share-alike, non-commercial, a maker's own terms and whatever is
  unclear go in `left-out.toml` with the sentence that decides it. A set is pinned to a commit and to what it
  unpacks to (the tree checksum, which is the one the program's fetching pins an archive by) or file by file. An
  entry's figures are what `sampled -- facts` measured through the player, and a mark is a rule on them that the
  README states: a marked instrument is offered and not put forward, and `starter` goes only to one with no other
  mark. A maker who asks to be named has a `credit` line, which `catalogue/CREDITS.md`, the Credits panel and
  `song_credits` all show: a test holds the list to the catalogue. An instrument on a disk is known again by
  the checksum of its `.sfz` file (`Catalogue::by_text`), wherever it lies. The catalogue fetches nothing itself:
  its sets are a list of the engine that fetches (`trill-runtime/src/instruments.rs`, `fetch_list`), asked for
  in File, Setup by the user who is shown a set's size and its licence, or with the `setup` tool, never by the
  assistant's endpoint and never by a test of the default run (the ones that fetch are `#[ignore]`d,
  `trill-runtime/tests/instruments.rs` and one in the window's `tests/setup_instruments.rs`). Only a program that
  says it is one offers the catalogue that is built in (`Runtime::study_and_fetch_as_usual`); a test hands in a
  catalogue of its own (`Parts::catalogue`) whose sets are on its shelf. What arrives was checked by the engine
  against what the catalogue pins before the library is told of it, and a set that changed at its source is a
  sentence, not a quiet update. An archive a source makes when it is asked for is never gone on with from the
  middle. The catalogue is written by the scripts in `crates/trill-catalogue/make` and by nothing else: change
  what was read there, not the TOML; after a set is added or a pin moved, fetch every set once through the
  program's own fetching (the opt-in test with `TRILL_CATALOGUE_SETS=all`).
- **A change to how a built-in device sounds:** a song saved before must sound as it did. A project file holds
  only the parameters that were set, so a new parameter defaults to what the device did before, or the device
  gets a line in `trill-core/src/formats.rs` (what a file of the older format is taken to have said, written into
  it as it is read; a parameter whose scale moved is moved there too, with the envelopes that drive it) and the
  file format's number goes up. A journal is read by the same rule (`journal/change.rs`, `Change::keep_format`),
  so add nothing there, but everything goes through `formats::keep_project`, `keep_device` and `keep_lane`.
  A device that changes a second time keeps both older sounds as states of its one `legacy` parameter (a choice:
  off, then one state for each, as `formats::FORMAT_6` gives them), and its old code path stays to the letter. A
  saved preset says the format it was saved in (`SavedPreset::format`) and is read by the same rule.
  `trill-engine/tests/old_songs.rs` holds every device to the samples a file of each older format rendered to:
  add a file of the format you are leaving, recorded on the last build that wrote it and before you change the
  device, and never record one again from a newer build. `tests/effects.rs` and `tests/instruments.rs` hold what
  the devices do now to numbers.
- **Routing:** a device may listen to another track (`Device::sidechain`, `Project::set_sidechain`). The tracks
  are played in `Project::playing_order`, each after the tracks that feed it (`Project::feeds`: a group's
  members, a return's senders, a device's key), and the edits refuse a loop. Whatever decides which tracks a
  render makes has to ask `feeds`.
  A device says whether it can listen (`DeviceProcessor::takes_key`) and is then handed the key with its block
  (`process_keyed`). A hosted plugin listens through the input after an effect's own, or an instrument's first
  (`trill-plugins/src/signal.rs`, `key_port`). VST3 has the host switch that input on, which a plugin may take
  to mean "listen to this": it is on only for a device that listens, and such a device is made again when that
  changes (`key_is_set_when_made`; `trill-plugins/src/key_tests.rs` holds both formats).
- **Something heard in audio** (a tempo, a key, where the hits are): a pure function in `trill-listen`, of
  samples at `signal::RATE` and of nothing else: it reads no file, keeps nothing, and hears the same audio the
  same on every machine (the FFT is `trill_dsp::fft`). Times are seconds of the recording; bars and beats come
  from its `Grid`, and whatever follows from the grid (chords, parts, loops) is worked out again when the grid is
  corrected. Its analyses do not wait for one another and are heard on threads of their own; of a recording over
  six minutes a group of them at a time (`AT_ONCE_SECONDS`), because each holds the recording several times over
  while it listens: the same answer either way. Both front ends ask `Runtime::hear`, which decodes through the
  media cache and keeps what was heard: never on the thread that draws. Its tests build their own audio; `tests/magpie.rs` (opt-in) holds it to what
  Magpie found in the recordings it took apart, and its numbers are in `docs/sounds-and-studies.md`: run it after touching the
  crate, on the machine that has them. What is heard in one sound must say so where a mix makes it unsure (the
  `listen` tool's `heard_as`).
- **Something a model finds in a recording** (a stage of a study): a method on `Models`
  (`trill-study/src/models/mod.rs`) with a scripted stand-in that does something plain and the same every time, a
  stage in `run.rs` whose product is one file in the study's folder written whole, and the real model under
  `models/real/`. Whatever is fed to a network goes through `real::tensor`, which refuses a shape the model was
  not made for: a network asks for memory by the square of what it is fed, without a word. A model is made ready
  when its stage asks and let go of after, so that a card holds one at a time. Tests run the real process, the
  real stages and the real files on the scripted models (`tests/run.rs`, `tests/desk.rs`); nothing of the real
  models is run by the default tests. What a study holds is what `listen` reads and what a view of it will draw:
  put what is found on disk, and leave nothing to be worked out by a model a second time. If a run comes to hold
  more memory, measure it and say so in `room.rs`: a run that takes more than it was given is ended.
- **Something a study does to a song:** a function in `trill-runtime/src/taking.rs`, one undo step, that both
  front ends call: the tool says what and where (`trill-mcp/src/taking.rs`), the window drags or sends
  (`study::land`), and neither does anything of its own with a study. What can be heard first is planned first
  (`plan_stretch`) and played from the plan (`hear_stretch`: a song of nothing but the clips, rendered), so that
  what is heard is what is taken. A moment of a recording is found in the song through the clip that plays it
  (`taking::beat_of`), never by multiplying by a tempo. A recording's bars are one thing, and the grid says
  what: `Grid::bar_beat`, `bar_of_beat`, `bar_start` and `place` (`trill-listen/src/beats.rs`) count each bar as
  long as it was found (`grid.bars` are numbers into `grid.beats`: a bar is not always `beats_per_bar` long), and
  `listen`, `take_from_study`, the workspace and the tempo taken into a song all ask them, never
  `beats_per_bar` arithmetic of their own; `trill-mcp/tests/taking.rs` holds them to one numbering on a study
  with a short bar (`Script::short_bar`). Notes for a track that
  is there go through the hook a front end hands (`Keyed`), which is how the server writes an instrument's
  keyswitches; a groove asks the track's kit for each drum by name (`on_kit`).
- **An audio clip that plays round** (`AudioContent::looped`, an `AudioLoop` in seconds of the recording): the
  clip plays that stretch again and again, `offset_seconds` being where in it the clip begins. Whatever asks
  where a clip is in its recording asks `AudioContent::second_at` (and `rounds` for where it comes round), never
  `offset + seconds_over` of its own, which runs on past the loop. In the compiler a round is one more kind of
  piece (`pieces` in `compile.rs`), joined to the one before it by the seam pieces have where the tempo
  changes; `trill-engine/tests/looped.rs` holds rounds to the sample and to the song's beats through a tempo
  change. A clip without a loop is compiled exactly as before, and a file saved before has none. The edits are
  `Project::set_audio_loop` and `loop_clip` (what the clip plays now becomes what repeats).
- **A reference track** (`Track::reference`): beside the song, never part of it. Whoever plays or renders asks
  `Project::as_heard`, which turns it into the mute and solo it comes to (the compiler does, and
  `compile::needed`); whoever measures the song's length asks `length_beats`, which leaves it out
  (`length_shown` for what there is to scroll over); the stage leaves it out; the edits refuse it a group, a
  send and a listener, so that nothing in the song changes when it is left out. A render that names it in
  `only_tracks` plays it as the track it is. `trill-engine/tests/reference.rs` holds all of it: whatever else
  comes to read `mute` and `solo` has to ask `as_heard`.
- **Something that is fetched** (a model, a set of sounds): a `Part` in a `List` handed to the runtime's
  `Fetches` (`trill-fetch`; the models' list is `trill_study::setup::list`). Never a fetcher of its own, and never
  a panel of its own: the Setup panel and the `setup` tool show every list. A part says its size, its licence in
  the words of its source, who asks to be credited and where it comes from, and each of its files is pinned to a
  version by its address and to its bytes by a SHA-256 (`cargo run -p trill-fetch --example pin`). Trill hosts
  nothing: a part comes from where its makers publish it.
- **Something that names a file:** a song's files are its audio clips' and what its devices say theirs are
  (`DeviceFiles`; `trill_dsp::SampleFiles` for the sampler and the drum rack). A device that keeps a path in its
  state and does not say so there is not listed as missing, not looked for and not relinked. A sampled
  instrument names its own file, and of its recordings only the ones that are not where that file says: those
  that were given another place, and those that are missing (`Session::song_files` adds what a device says is
  missing to what its state names). A song does not list the hundreds that are where they belong. The project file's
  `files` table (`trill_core::files`, written by `Session::save`) is beside the song, not part of it: no edit
  touches it, and it needs no format number.
- **Something that changes the song without being an edit** (a state a plugin came to in its own window):
  enter it in the journal as an aside (`Session::note_device`), or the song the journal leads to drifts from the
  song that is open.
- **Something a user should know of** (a feature, a limit, a number that was measured): the page of its topic
  in `docs/` (`window.md`, `instruments-and-effects.md`, `plugins-and-presets.md`, `writing-music.md`,
  `sounds-and-studies.md`, `files.md`, `engine.md`, `ai-and-mcp.md`, `making-of-and-film.md`), a tool in the
  table of `ai-and-mcp.md`, an opt-in test in `development.md`, and what is not built in `status.md`. The
  README is the front page for someone who has never seen Trill: it gains a line only for something a
  newcomer needs to start, and never a paragraph of reference.
- **Something the window remembers:** `Settings` (`trill-library/src/settings.rs`) has fixed fields; state that
  belongs to one part of the window lives in its own file beside it (`browser.json`, `assistant.json`,
  `keyboard.json`).

## Conventions that are particular to this repository

- **A failure is a sentence, not a crash.** A device that vanishes, a plugin that refuses, a file that is gone:
  each ends in a message the user can read (`Session::problems`, `StreamInfo::warning`, a toast) and the song
  keeps playing. No stuck notes: whatever starts a note owns stopping it.
- **A song's plugins are rendered in a process of their own.** A render, a measurement and a probe of a
  song with plugins are done by the program started a second time (`trill-engine/src/apart.rs`: the engine owns
  what is said both ways; `trill-runtime/src/jobs.rs` says which program and how its devices are made, and both
  programs call `jobs::run_as_job_if_requested` first thing; tests use the `trill-job` binary). Every job has a
  process that has just started, and it ends where it stands with no plugin taken down. Live playback stays in
  the program. A plugin that crashes there is a sentence (`apart::Crash`), the same song is the same samples,
  and the memory comes back; a song without plugins is rendered where it is. Whatever a render needs of the
  session has to cross in the job: the project, the options (`RenderOptions` is what is serialized), what is
  remembered of plugins (`ready::Memory`) and what the factory knows of them (`scan::Known`). A new field on
  `Measured` needs `#[serde(default)]`. `TRILL_JOBS=here` does every job in the program, for telling the two
  apart; `trill-runtime/tests/jobs.rs` holds the two to the same samples and takes a process down at every
  stage. A crate that builds no program which could do its jobs has its own test program do them
  (`Program::ThisTest`, `Setup::by_test`: started again to run the one test that serves), as the window's and
  the stage's tests do. A film renders its sound through `trill_stage::export::sound`, which stops the render
  when the film is stopped.
- **A render is not a quick look.** It creates every device afresh and waits for plugins to be ready
  (`ready.rs`), as a probe does: until every note asked for sounds, heard one at a time and each exactly three
  times, so that an instrument whose takes go round begins every render at the same take. The wait counts what
  it runs and never runs a plugin for as long as the clock says: what some plugins give depends on how many
  blocks they were run before. A key the track knows for a keyswitch is pressed with the notes the part plays
  under it and never asked to sound, and a device that listens to another track is lent noise while it is
  tried. What is known of a plugin that does not do the same thing twice, and what stops it, is the factory's
  to say (`DeviceFactory::repeating`, `trill-plugins/src/repeats.rs`): a render names it, and with
  `RenderOptions::repeatable` stops it on its own copies. To find out what an
  instrument does, use a probe (`Session::probe_job`, the `probe` tool); to check a mix, `RenderJob::measure`
  (the `measure` tool), which writes nothing. A plugin that plays live is not waited for; `Session::loading`
  says whether it may still be reading its samples. A sampled instrument is the same: made for a render it
  reads the recordings it plays and waits for them, and played live it is listed as loading until the start of
  every recording is in memory. A render compiles only the tracks that are heard
  (`Routing::needed` in `compile.rs`), and writes every stem in the one pass when asked
  (`RenderOptions::stems`): never render a song once per track. It makes the tracks that do not feed one another
  on several threads at once and adds them up in the graph's order, so that the samples are the ones one thread
  makes (`spread.rs`; `tests/threads.rs` holds that to the bit): a step added to a track's block goes into the
  functions every path calls (`prelude`, `sound`, `through`, `finish`, `mix` in `processor.rs`), and whatever
  comes to feed a track is one more line in `spread::plan`. What a plugin may be run beside is the factory's to
  say (`DeviceFactory::beside`): its instances in turn unless it was measured (`trill-plugins/src/beside.rs`). It is refused, with nothing written, when a
  plugin instrument never answered its notes (`RenderError::NotAnswered`).
- **A song that plays is made on as many threads as it needs, and is the same samples on any number.** One
  thread makes it while that takes less than half a buffer's time (`SPREAD_ABOVE` in `processor.rs`: a small
  song is played exactly as it was, with no thread made for it). Above that the tracks that do not feed one
  another are handed to the team in the rounds a render uses (`spread::Rounds`, made with the graph) and added
  up by the callback in the graph's order: `tests/cores.rs` holds what several threads play to what one plays,
  to the bit, at five buffer sizes and three thread counts, through keys played by hand, a take, the metronome,
  a graph swapped in, a loop and a jump. What a track is told ahead of its own notes (what a hand played, what
  a take hears, what is let go of) is said by the callback, for every track before any is made (`prelude`):
  whatever else comes to read or write something that is not the track's own goes there or into `mix`, never
  into `sound`, `through` or `finish`, which any thread runs. Devices of one plugin that keep something between
  them take turns in the song's order while it plays, as in a render (`Beside`); what follows them on a track
  is called out to whoever is free as soon as the turn is over (`Workers::more`). The team is made off the
  audio threads when a song first needs it (`Engine::tend_threads`, by the housekeeping). A device that goes
  down costs the buffer it went down in, on whichever thread (`Processor::after_fault`). "Keeps up with a real
  song" is checked with the `keeps_up` example, on a song with its plugins: after touching what an audio thread
  runs or how work is handed across, run it and hold what it prints beside the numbers in `docs/engine.md`.
- **Starting anywhere is starting where the song has the instrument.** What a part told its instrument before
  the place a song starts at is told again there: controllers by their lanes (`collect_controllers`), and the
  keyswitch that is chosen (`graph::chosen_at`, `chase_switches` in `processor.rs`; a track's switches are
  compiled from its profile, `graph::switches_of`). The song remembers which key the instrument is on
  (`TrackState::latched`) and does not press it again, so that a key pressed while the song stood still is
  not pressed with the first note: some instruments take one only ahead of its note. A render cannot have
  stood still, so it begins to play ahead of its start (`SWITCH_AHEAD_BEATS`, and `lead_in` in `render.rs`
  for the notes that are sounding at the start, `LEAD_IN_BEATS` back at the most) and keeps and measures
  nothing until its start (`Tap::unheard`). A song that plays does not begin a note that is already
  sounding. A render starts on a frame of the song as counted from beat 0, so that a stretch alone lies on
  the frames it has in the whole song. `tests/chase.rs` holds all of it; whatever else a part comes to set
  and leave set has to be said again at a start in the same place.
- **A line of automation follows its control, and positions are read from the song.** A lane keeps what its
  lines are straight along (`AutomationLane::travel`; `Travel::of_param` says it for a device's parameter); a lane
  that says nothing is straight in its units, which is every lane of a file saved before (format version 1 or 2), and
  must go on playing exactly so. A level travels as the same control does in Ableton Live (`Travel::Fader` for a
  volume or a send, `Travel::Gain` for a gain: fitted to what Live shows, and `trill-engine/tests/automation.rs`
  holds renders to those numbers); a lane that a file of formats 3 to 5 saved along the fader of its day is
  `Travel::Square`, read so by its words, and plays as it did. Whoever writes a new lane says how it travels
  (`Project::write_automation`); `set_automation` keeps what a lane has. On the audio thread an envelope is read on a grid of the song's own
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
  are. The stage reads the same two maps (`Score::time`, `Score::bars` in `trill-stage/src/score.rs`) and nothing
  in it multiplies by a tempo either: a hit is lit on the frame the map has it on, which its tests hold through a
  change (`stage.rs`, `tests/drawn.rs`). `trill-engine/tests/tempo.rs` and `trill-mcp/tests/tempo.rs` are where a
  change to any of this is proved.
  A device is told where its block is in the song's time and among its bars (`ProcessInfo::song`, filled by the
  engine from the frames the song has played and from the meter map), and a host of plugins passes that on: it
  never works the time or the bar out from the block's tempo and time signature, which say neither once either
  has changed (`trill-plugins/src/transport_tests.rs` holds both formats to the maps). A block made by hand is in
  a song of one tempo (`ProcessInfo::steady`).
- **The journal is the session's, and it stays on the machine.** Every change to the project goes through
  `Session::make_step`, which enters it in the journal (`session/story.rs`); a thread that acts for a tool call
  says so (`journal::acting`) and its edits are that call's step. Undo and redo are steps too: nothing leaves the
  story. A journal holds a conversation, so it is written only into the program's own folder (`journals`, beside
  the settings; beside a safety copy for unsaved work) and never into a project file or next to one; handing it
  on is `Session::export_journal`, without the conversation unless asked. A replay (`journal::replay`) reads
  nothing but the journal: the same journal and moment are the same frame, which is what a film will be made of.
  The making-of draws the song with the window's own views, which it hands a session that is not the song's.
- **A film is of the song and of nothing else.** A frame is a function of the song, what was measured of its
  sound, its time and the shot: no clock, no random number that is not seeded by the frame, nothing of the
  machine. So frame N is the same on every run, a film can be stopped and made again, and the tests compare
  pictures byte for byte (`trill-stage/tests/drawn.rs`). The live stage and a film share every painter and pass;
  what differs is where the time comes from. Both front ends make a film through `trill_stage::export`
  (`SongFilm`, `MakingFilm`), which renders the song's own sound once, in the same job: never a WAV laid under it.
  `ffmpeg` is a program found when a film is made, never a dependency, and a film without it is still made (as
  pictures and a sound file, with a sentence that says so).
- **A recording is studied in a process of its own.** A model's runtime is somebody else's code driving a
  graphics card, fed whole recordings. The run is done by the program started a second time
  (`trill-study/src/helper.rs`; `jobs::run_as_job_if_requested` serves it, as it serves renders): one that faults
  costs a sentence that names the stage and leaves every stage that had finished, a run the machine has no room
  for is refused before it starts, one that takes more than it was given is ended, and its memory, the card's
  included, comes back when it ends. Stopping is ending the process, and a process whose asker has gone ends
  itself. What runs the models is what a timed test measured on this machine (`Setup::test`), never what cards
  there are: until it has run, and again when a part that runs models changes, it is the processor.
- **One study at a time on the machine.** A desk takes the machine's turn before it starts a run
  (`DeskOptions::turn`: a file every program names, held while it studies, taken over when its holder is
  gone), so the window and a server do not each run the models. The real models take turns over one file in
  the temporary folder; scripted ones over one beside their settings, so tests do not wait for one another.
- **A study's files are the song's.** A clip taken from a study names a file in Trill's own folder. So a front
  end asks `Runtime::study_used_by` before a study goes or its stems are dropped, and says which songs (the
  open one, and the ones the housekeeping thread remembered as saved or opened: `Settings::recent_projects`);
  the assistant's door refuses where songs play them. `Runtime::gather_from_studies` is the small "collect":
  what the open song plays of studies, cut to what it uses, in `<song> files` beside it.
- **Nothing is fetched unasked.** Trill fetches a part when its user asks for that part by name, having been
  shown its size, licence and source; never by itself, never by a test (a test hands the runtime a shelf:
  `Studying::fetcher`, `trill_fetch::scripted`), never for a picture, and never by the assistant's door, which
  also refuses a link. A recording behind a link is fetched by a program the user installed (`yt-dlp`), found
  when it is wanted, with the sentence about whose the recording is (`desk::WHOSE`). A runtime neither studies
  nor fetches until its program says it is one (`Runtime::study_and_fetch_as_usual`).
- **Hermetic by default.** Tests, screenshots and scripts never read or write the user's settings
  (`%APPDATA%\trill`), sample library, studies or safety copies: `RuntimeOptions::hermetic()` or `Storage::At` on a
  temporary folder in tests, `trill-mcp --scratch`, or `--profile <folder>` on either program, by hand. (Studies
  and what was fetched are kept in `%LOCALAPPDATA%\trill` unless the settings say where; with `Storage::At`,
  beside the settings.)
- **Tests go through the real path** and are named as sentences
  (`a_take_is_counted_in_recorded_and_selected_as_one_undo_step`). Engine tests render audio; app tests drive
  the window; MCP tests talk to a real server over HTTP.
- **A file that is gone is said once, and never relinked by being found.** Each front end says what the song is
  missing as one thing (`Runtime::files_missing`), in place of a line for every clip, and a render or a measurement
  of such a song says it first. What was found says how sure it is (`Confidence`); only `Sure` is taken without
  someone having looked, and a file of the same name alone is never `Sure`.
- **Errors say what to do next.** An MCP error names the ids that do exist; a refusal says where a file may be
  written. Replies are compact: a tool that could return thousands of items pages them.
- **The look** comes from `trill-noir`'s theme tokens: near-black surfaces, hairlines, zero corner radius, caps
  micro labels, the signal orange used sparingly, nothing below 10 px. Extend the kit instead of styling in place.
- **Pictures are not kept.** `render.sh <word>` renders the few pictures that show a change into
  `target/screenshots` (off-screen, taking no focus); look at them and leave them there. Never run it for every
  picture as routine. Only the five the README and `docs/` show, in `docs/images`, are tracked (`render.sh readme`, on purpose): four
  of the window and one of the stage, which the film example draws. Films and their stills go under `target/`.
- **The assistant keeps five rules**, written at the top of `trill-assistant/src/lib.rs`: it never touches
  credentials, never signs anyone in, never bundles or modifies the CLI, never promises usage limits, and is
  called "the assistant", with "Claude Code" only in descriptive sentences. The only tools it is given are
  Trill's, a plan list and web research; files and the shell stay off because its panel cannot ask permission.
- **Nothing of a plugin maker's is in the repository**: no preset file, no part of one, no dump of a state.
  Tests of `trill-plugins/src/vendor` build their own small files in the format and load them into the scripted
  plugins; the ones on installed plugins are `#[ignore]`d (`tests/vendor.rs`). Trill only reads in a maker's folders.
- **Dependencies** must be GPL-compatible and worth their build time (`cargo build --timings`).
- **Code is written here.** Nothing is copied from the owner's other projects, with two exceptions: `trill-noir`,
  which carries their design language, and Magpie (their project that takes a recording apart), whose method
  `trill-listen` translates from its Python and may go on translating, redesigned where Trill wants something
  else. What Magpie has from libraries of others (librosa, scipy) is written here, not copied from them. Nothing
  of Magpie runs in Trill and Trill never talks to it.
- **Other people's recordings stay out of the repository**: no audio, no stem, no lyric, nothing of a Magpie
  project. Tests of what is heard build their own audio; the ones on real recordings are `#[ignore]`d and read
  them where the machine has them. The catalogue of instruments is text about recordings (names, licences,
  checksums, what was measured) and holds none: sets are fetched into a folder under `target/` to be read and
  measured, never into the repository.
- **Processes:** several sessions may run `trill` or `trill-mcp` on the same machine. Stop a process by its PID,
  never by image name.
- **Git:** work is committed on `dev` (or a worktree's own branch that is merged into it). Nothing is pushed
  unless the owner asks.
