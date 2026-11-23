# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Trill is an open-source digital audio workstation in Rust (GPL-3.0-or-later), developed on Windows and built and
tested on macOS (Apple silicon) and Linux (x86-64) too (`docs/development.md`). Three goals
decide every trade-off: it does not crash or stall, it hosts CLAP and VST3 plugins, and everything it can do is
reachable over MCP so an AI can write and mix a song. `README.md` is the front page (what Trill is,
how to build it and how to use it) and stays short; `docs/` describes the features, a page a topic, and
`docs/status.md` what is not built yet; this file is the map, so that a task needs only the files it touches.

## Commands

```sh
cargo run -p trill-app -- --demo                    # the window, on a built-in song
cargo run -p trill-mcp -- --no-audio --scratch      # no window: MCP on stdio, nothing read or kept
cargo test --workspace                              # about 3,000 tests, a few minutes once built
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
cargo run -p trill-runtime --example keeps_up -- SONG.trill      # a song played in real time against a device that makes no sound: the load, the late buffers, what each track cost (`built-in` in place of a file is a song made there; `--device` is the system's shared output, handed silence; see the example)
cargo run -p trill-runtime --example opens -- SONG.trill         # what opening a song takes: when it was there, when each plugin arrived, and what each step of its making took (`long` in place of a file is a long song without plugins made there; `--at-once` is how it was; `--after OTHER.trill` opens over another song; `--add NAME` then adds a plugin by an edit; see the example)
cargo run -p trill-runtime --example study_listen -- PROFILE STUDY target/study-listening   # what a study does to a song, to listen to: stems, loops at other tempos, a phrase at other pitches, the kit, notes beside their stems, and a list
cargo run -p trill-catalogue --example sets -- list              # the sets of the catalogue of instruments; also sum, files and check (see the example)
cargo run -p trill-engine --example edit_cost -- SONG.trill      # what an edit costs someone working alone, by the kind of edit (it uses nothing a build before sharing lacks: copy it there to compare)
cargo run -p trill-core --example merge_cost -- SONG.trill       # what the parts of an edit cost: the song copied, settled, compared, its properties made, the song worked out from them
cargo run -p trill-runtime --example files_cross -- 300           # a recording of 300 MB, made there, crossing between two programs in a shared song while it is worked in: what reading it costs, how long it takes, how long an edit takes meanwhile (`--through-here`, `--through ADDRESS KEY`; `--files 12` is the megabytes in twelve recordings; `--line 5000:50000:20` puts each program on a line as a home has it, here 5 Mbit/s up, 50 down and 20 ms each way; `--count SONG.trill` says what of a song would travel and sends nothing)
cargo run -p trill-runtime --example together -- SONG.trill      # two programs in one song on this machine: how long joining takes and how many bytes, from an edit to the other's graph, bytes a second for a drag, what hearing of a change costs, what an hour of work adds to the file (`--through-here` has them meet through a meeting point in the same process, `--through ADDRESS KEY` through one that runs somewhere, `--line UP:DOWN:MS` on a line as a home has it)
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build -p trill-meet --release --target x86_64-unknown-linux-musl   # the meeting point as one static file for a Linux server (the `load` example beside it holds many meetings open, to see what that takes)
sh scripts/friend-bundle.sh                         # a release build gathered into target/friend/Trill to hand to a friend: the programs, the licences, a page of what their computer needs
sh scripts/release.sh --key FILE --notes NOTES.txt  # a release into target/release-out/<version>: the installer, the archive an installed Trill updates from, the signed announcement (publishes nothing; `--trial KEYFILE` is a build to try the updater out with, never to publish; see docs/development.md)
cargo run -p trill-update --example release -- read ANNOUNCEMENT   # what an announcement of a release says, read as a program reads it; also key, public, pack, describe (a build worked out from its archive alone) and announce (one or several builds, signed) (see the example)
```

- The default test run needs nothing installed and reads nothing of the user's. Tests that need this machine
  (installed plugins, audio devices, MIDI ports, a signed-in Claude Code, a graphics card, ffmpeg, the models a
  study runs) are `#[ignore]`d
  with the command that runs them; `docs/development.md`, "Building and testing", lists them. Run the plugin ones after
  touching `trill-plugins`, `cargo test -p trill-stage --test drawn -- --ignored` after touching `trill-stage`'s
  `gpu/` or `film.rs`, and `trill-study`'s `tests/magpie.rs` after touching what its real models are fed or what
  is made of their answers (`models/real/`, `lyrics.rs`).
- A few tests hold a wall-clock budget and can fail on a machine that is busy with other builds
  (`trill-library` `scale::a_hundred_thousand_files…`, `trill-meet`'s and `trill-together`'s tests of what a
  stranger is given time for, `trill-engine` `session_transport_runs_in_real_time` and
  `changing_a_sample_while_playing_keeps_the_song_going` and
  `the_session_says_how_many_clips_it_is_still_preparing`, its `audio_settings` and `loading` tests and
  `a_hundred_and_twenty_eight_voices_of_a_sampled_instrument_cost_less_than_the_time_they_play`, `trill-dsp`
  `eight_notes_of_eight_voices_leave_most_of_a_core`, `trill-mcp`
  `an_agent_arms_a_track_plays_it_and_records_a_take`, `trill-assistant`'s `pipeline`, the real-plugin tests). If one is the only failure, run it alone before concluding anything.
- The compiler is the one `rust-toolchain.toml` names, on every machine: "clippy is clean" is judged on it, and a
  newer compiler's new lints are not chased until it is raised there, on all three systems at once.
- Development builds are optimized (`[profile.dev]` in `Cargo.toml`) because unoptimized audio code cannot keep
  up; what only runs while building is not, and debug info is line tables only.
- Do not rename or move a `target` folder: tests of `trill-assistant` and `trill-plugins` have its path built in.

## Architecture

Three rules hold the program together. A change that breaks one is wrong even if it works.

1. **One front door.** `Session` (`trill-engine/src/session.rs`) owns the project, its undo history and the
   engine. The window and the MCP server are thin clients of it, so there is one implementation of every edit.
   An edit is a method on `Project` that validates and returns `EditError`; `Session::edit` runs it, finds what
   it changed by comparing the song before and after (`trill_core::merge::diff`), takes that into the song's
   properties like anyone else's change (`merge::Doc`, which the project is worked out from), keeps what it set
   as one undo step, compiles the project into a render graph and hands it to the audio thread. What someone
   else changed in a program of their own enters by the same door (`Session::merge`): it is compiled and
   swapped in like an edit, moves the revision, and is never a step of this session's history.
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
| `trill-core` | The model and nothing else: `channel.rs` (a MIDI channel: which of sixteen a note or a curve is on), `project.rs` (`Project`, tracks, clips with their notes and controller curves, automation, every edit; the key a song says it is in, and a track that is a reference: a recording beside the song), `files.rs` (the files a song names: `Project::files`, `Project::relink`, where a folder went read from one file that was found, and the table a project file carries of where its files are from the song's folder and what they were), `travel.rs` (how a control's values lie along its travel, and so how an automation line gets from one point to the next), `tempo.rs` (how beats become time and how they become bars: the tempo map and the meter map of a song whose tempo and time signature change), `time_ops.rs`, `instrument.rs` (what a track knows of its instrument: drums or pitches, drum map, keyswitches, range, octave), `device.rs` (the device API, `EventKind`), `presets.rs` (a device type's presets as something to browse, a preset that was saved, and a *sound*: a preset of its first device that says which effects follow it, with the edit that makes them, `Project::bring_after`), `takes.rs` (what a track records and what becomes of it: an input of the audio device and how it is heard meanwhile, the takes kept in lanes beside a track's clips and never played from there, and the comp made of them, `Project::cover`, `comp_take`, `choose_take`), `renditions.rs` (a track heard where its plugins are not: a recording of what its first devices make of it, by a program that has them, which the track says it has; what such a recording depends on, as a print), `midi_file.rs`, `formats.rs` (what a file of an older format meant by what it does not say), `merge/` (a song that several programs hold and change at once: `mod.rs` the changes, as properties set, each with the stamp it was set at; `props.rs` the properties of each kind of thing, listed so that a field added to the model does not compile until it has a place; `doc.rs` the song as its properties, what taking a change in does and what a file keeps of it; `derive.rs` the project worked out from them, with what is done where they say what no song may be; `diff.rs` what an edit did, found by comparing before and after; `check.rs` what another program says held to the model's rules; `lot.rs` songs and edits drawn by a seeded lot, for every crate's tests), and `journal/` (how a song came to be: `mod.rs` the entries and their text, `change.rs` what a step keeps, `place.rs` where it landed, `words.rs` an edit in words, `replay.rs` a session laid out to be watched). No audio or UI dependencies. |
| `trill-dsp` | The built-in devices (`synth`, `wavetable/`, `drums`, `sampler`, `drumrack`, `effects/`: one file per effect or family, the compressor with its sidechain in `compressor.rs`) and offline time-stretching. `lib.rs` is the registry. The wavetable synth is a folder: `table.rs` (a table as an oscillator reads it: every frame at every brightness a pitch may have), `recipe.rs` (a table written down as key frames of shapes and harmonics, or read from an audio file), `factory.rs` (the tables that ship, each a recipe), `params.rs` (its parameters, whose names are what a model reads), `voice.rs` (one note: oscillators, filters, the matrix), `mod.rs` (the device, its state document and what all its notes share), `patches/` (its bank, a file for each kind of sound: each sound with its sentence, its tags, the names of its macros, the keys it is for and the effects it brings). The sampled instrument is a reader and a player that know nothing of each other's insides: `sfz/` (`text.rs`: a file as headers and opcodes, with what it includes and defines; `mod.rs`: what they mean, the list of the opcodes that are done, and for one that is not what a listener misses) and `sampled/` (`model.rs` an instrument as the player takes it, `pool.rs` the recordings in memory and the threads that read them, `read.rs` a recording read at another pitch, `loaded.rs` an instrument that is loaded: its state document, its files, what it says of itself; `kit.rs` the drums of a kit by name, `player.rs` the device, `mod.rs` what speaks for it). |
| `trill-media` | Decoding audio files, the shared `MediaCache`, waveform peaks, resampling, WAV writing. `stream.rs` reads a file a piece at a time, and the loop a WAV file carries. `wav_plain.rs` shows the reader a plain WAV file whose format chunk is longer than it takes as the file it stands for (whole sample libraries are written so). `wav_growing.rs` writes a WAV file as it is recorded, put through to the disk and then given a header that is true every second, so that a take reads whatever became of the program that wrote it or of the machine. |
| `trill-library` | The sample library (scan, index, search, tags from file names) and the settings file. A sampled instrument is one entry that says what it is (`EntryKind::Instrument`, `InstrumentFacts`); its recordings are entries that a search leaves out unless it asks for them (`Entry::of_instrument`, `Query::include_recordings`). The settings hold the meeting point a person shares through (`TogetherSettings::meeting_point`: its address and their key, which is nowhere else and not in what the settings show of themselves). |
| `trill-fetch` | What Trill fetches when its user asks, and never otherwise: *parts* that others publish, in lists that any crate hands it (`Fetches::add`, a `List` of `Part`s: size, licence line, credit, source, files). `lib.rs` is the engine: one part at a time in the order they were asked for, several asked for at once (`fetch_many`, refused as a whole where the disk has not the room; `stop_all`), each file by the system's own `curl` (`Curl`) into a name it does not keep until its SHA-256 is the one pinned, an archive unpacked (`zip.rs`: stored or deflated, the 64-bit form, nothing written outside the folder) and, where its own bytes cannot be pinned, sized by guess and checked by what it unpacks to (`Keep::Tree`, `tree_checksum`, `Fetched::sized_by_guess`); a part of many small files fetched several at a time and recorded a batch at a time; the room asked of the disk before a part starts; a second place a part is published (`Part::elsewhere`); stopped and gone on with; where a list is kept is the user's to choose, and a list's `changed` hears of a part that arrived, was removed or was moved. `scripted.rs` is a shelf tests fetch from; `whole.rs` is files written whole, the room on a disk, programs found on the path, the signal that ended one. `examples/pin.rs` prints what a part is pinned by, `examples/many.rs` measures how many files at once is worth it. No audio, no models. |
| `trill-update` | Which Trill this is, whether a newer one is out, and putting it in place. `lib.rs` (`this`: the version, and for a release the commit, which only `scripts/release.sh` gives a build; everything else is *a build of your own*, which looks for nothing and is never replaced; `start`: the first thing both programs do once they are no job), `version.rs`, `release.rs` (a release as it is announced: a small text under its makers' signature, Ed25519 by the `ed25519-dalek` crate and no scheme written here; the keys a build takes one under, `KEYS`; what an announcement may name, `Release::check`), `updates.rs` (`Updates`: looking, which fetches the announcement and nothing else, and fetching the build to where it is staged, each asked for; what was read is kept and read again under the keys), `place.rs` (the folder Trill is installed in: an update staged beside the programs, the lock every Trill that runs from there holds and one that applies has alone, the swap by renames, what was cut short mended, what does not come up put back), `scripted.rs` (makers of releases for tests: a key and a shelf of their own). `examples/release.rs` is what `scripts/release.sh` calls: a key made, an archive packed, an announcement signed and read. Its only way to a network is `trill-fetch`'s. |
| `trill-catalogue` | The sampled instruments others publish that Trill may hand on, read from the text in `catalogue/` at the root of the repository (built in): `lib.rs` (a `Set` as it is fetched and pinned, an `Instrument` with what was measured of it and its `Mark`s, the licences that pass, what was left out and why, a search, the starter selection, `Catalogue::owed`: what is owed for the instruments a song plays), `check.rs` (whether a folder holds a set, whole and as it was read: the tree checksum, each file's own). It fetches nothing and holds no recording. `make/` is how the catalogue is written again, by hand: what was read of each set (`sets.py`, `pins.txt`, `starter.txt`), and the scripts that fetch the pinned commits, measure them through the player and write `catalogue/` (its `README.md` has the steps). |
| `trill-theory` | Music theory as pure functions: chord symbols, progressions, voicings, grooves, bass lines, melody notation with its ornaments (`ornament.rs`: as an instrument's keys or the pitch wheel), keyswitches for articulations (`articulation.rs`), key and chord reading. |
| `trill-listen` | What is heard in a recording without a model, as pure functions of the audio: `beats.rs` (the pulse of the onsets, the `Grid` with its corrections, seconds to beats), `onsets.rs` (where hits begin, to the foot of the attack; beats moved onto them), `harmony.rs` (tuning, a chroma read from three transforms by register, key, chords), `structure.rs` (what each bar sounds like; the parts), `loops.rs`, `hits.rs` (drum hits told apart, one-shots, bass notes, the groove), `pitch.rs` (YIN: a bass line, a melody), `midi.rs` (any of it as a `MidiFile` on the recording's own beats), `spectrum.rs` and `signal.rs` (the transforms and measurements they share). `lib.rs` is `hear`: a `Recording` (one sound, or its stems) in, a `Heard` out. The method is Magpie's (see "Code is written here"). |
| `trill-study` | A recording taken apart, a *study*: its stems, beats and words found by models, then everything `trill-listen` hears, heard in the stems. `store.rs` (a study on disk, a folder each: `study.json`, `stems/`, `outlines.json`, `beats.json`, `words.json`, `lyrics.json`, `heard.json`), `run.rs` (the stages in order, each writing its product whole; one whose product is there is done), `helper.rs` (the process a run is done in: the program started a second time, or for a crate's tests its test program started to run the one test that serves (`serve_by_test`), one `Order` in and marked `Said` lines back (off Windows, what it held, said just before it ends: an ended process cannot be asked there), attended, stopped by ending it, ended when it takes more memory than the run was given), `desk.rs` (what a program holds of studies: one at a time in the order asked, refused before it starts where the parts or the memory are not there), `room.rs` (what a run needs, by what was measured), `models/` (`Models` is all a run knows of them: `scripted.rs` for tests, `real/` the networks on ONNX Runtime, loaded when a study is run and never linked: `demucs.rs` stems, `beat.rs` beats, `voice.rs` where someone sings, `whisper.rs` the words, `align.rs` where each is), `lyrics.rs` (words put where they are sung, by the loudness of the vocal: Magpie's), `link.rs` (`ffmpeg` and the program that fetches a link, found and never shipped), `setup/` (`catalogue.rs`: the parts, each pinned, with its licence in its source's words; `mod.rs`: the timed test of what runs the models, and the programs found). The `trill-study-job` binary is the process for tests. The models' runtime is kept out of every other crate. |
| `trill-plugins` | CLAP (`clap/`) and VST3 (`vst3/`) hosting. Per format: `factory`, `instance` (the plugin's main-thread half), `processor` (its audio-thread half), `controller`. Plugins are scanned, and their presets read, in a child process (`scan/`, `presets/`, the `trill-plugin-scan` binary) so one that crashes costs a message. `vendor/` reads the presets some makers keep in files of their own (one module per format; `look.rs` says what a state is, for the `inspect` example) and builds the state that loads one; it also reads out of a drum plugin's state what its kit has on each key (`vendor::key_sounds`, `DeviceController::key_sounds`). A preset file that lies anywhere is made a preset by `DeviceFactory::preset_file` (`presets/file.rs`), which is what a path given as a preset comes to. All plugin main-thread work runs on one thread (`main_thread.rs`); what each plugin's making took there, step by step, and what kept the thread meanwhile is said in the log for whoever measures (`making.rs`). What a plugin is told of the song with each block, in either format, is said and filled in one place (`transport.rs`). A block whose input is nothing but silence is said to be, as each format has it (`signal::is_silence`: a VST3 bus's silence flags, a CLAP channel that is constant; the CLAP input structs are filled in by hand in `clap/buffers.rs`, because clack 0.2 writes that mask by the port's place); whether a plugin rests on it is the plugin's business, and `tests/silence.rs` (opt-in) measures it. `vst3/quirks.rs` holds what a particular plugin needs of the host so as not to take the process down, each with what was measured (`tests/lifetimes.rs` counts crashes, an attempt to a process). `repeats.rs` holds what was measured of plugins that do not do the same thing twice, and what stops them (`examples/takes.rs` is how to measure another). `scripted.rs` in each format is a fake plugin for tests. |
| `trill-engine` | `engine.rs` (the stream and its queues), `processor.rs` (what an audio thread does with a block), `capture.rs` (an input brought into the engine's clock: the ring a device's thread hands samples across by, the two clocks read off the least late of their time stamps, and the reader that takes exactly a block for every block made, at a constant delay and at the ratio the clocks have), `recorder.rs` (the thread that writes a take's files, and what it has seen of them for the window to draw), `glide.rs` (a gain that is where its envelope has it, frame by frame), `compile.rs`, `graph.rs`, `render.rs` with `analysis.rs` (what a render measures, as it goes), `ready.rs` (waiting for plugins before one) and `spread.rs` (a block made on several threads, for a render and for a song that plays alike: which tracks at once, in what rounds, and what of a track goes on beside the others after its turn) with `crew.rs` (a render's threads, which sleep on a lock) and `team.rs` (the threads a song that plays is made on beside the callback's: audio threads, which never do), `apart.rs` with `apart/` (a render, a measurement or a probe done by the program started a second time: what the two say to one another, the process that serves, what a fault leaves to be read, how much room the machine has), `probe.rs` (playing an instrument notes to learn what it does), `session.rs` with `session/` (plugin editors, live playing and audio settings, `opening.rs`: a device that takes time to make comes after the song has it, made and taken down by a thread of the session's own, `cores.rs`: how many threads a song that plays may be made on, what it and each of its tracks cost the audio threads over the last ten seconds, and the sentence for a song that is more than the machine plays in time, `history.rs`: an undo step as the properties it set, `content.rs`: the files of a song others hold too, known by what they are, each machine with its own place for them, and which of this machine's may go, `renditions.rs`: which devices this machine has not got, which recording it plays in their place, and a recording this program made of a track taken in, `together.rs`: the song as one others hold too (its id, this program's number, what is changed here kept to be said, `Session::merge`, the notices of a clash and their remedies, a plugin's state as news), presets, `sounds.rs`: a sound given to a device or a track and saved from one, parameters by their text, recording takes, `recording.rs`: the input, open only while something needs it, a take's audio landed as takes where it was sung, a take heard in place, the round trip and its measurement, where recordings are kept and which the song no longer uses, `beside.rs`: a song's recordings brought beside it at its first save, by a second name or by a copy made away from the session, and what names a file, `bin.rs`: where a recording that is removed goes, the system's Recycle Bin (macOS's Trash) or a folder, probes, `wavetable.rs`: a wavetable synth's own tables and the names of its macros, and `story.rs`: the journal the session keeps), `journal.rs` (who is acting on the session, and where journals are kept), `hand.rs` (whether the thread that is asking is the user's own hand, which alone opens a microphone), `saved.rs` (the presets and sounds a user saved, each a file with the states of its devices beside it), `character.rs` (what is measured of a sound to hold it beside others of its kind: its level, where its spectrum lies, its width, how a note begins and ends; and the notes each kind is measured on), `instruments.rs` (what is known about a plugin: learned on this machine, or shipped in `instruments/known.rs` from measurements, by its URI; `instruments/kit.rs` names the drums of a kit that says what is on its keys, which `Session::profile_of` lays over a track's own map each time it is asked), `host/` (audio hosts: the system's, WASAPI exclusive, and `scripted` for tests; `priority.rs` is the class the system keeps for audio threads, which the callback's thread joins in either host and the team's threads as they start; on macOS a time-constraint policy made from the buffer's period, which a device's own thread has from the system already), `scripted.rs` (instruments for tests that behave like slow plugins), `sources.rs` and `stretcher.rs` (clip audio). |
| `trill-runtime` | `Runtime`: the session, media cache, library, settings, MIDI input ports, safety copies of unsaved work and the housekeeping thread, built once. Both programs start from it. `jobs.rs` says which program does a session's renders and probes in a process of its own, and makes the devices there (the `trill-job` binary is that process for tests). `sounds.rs` and `missing.rs` are what both do the same way with sounds: adding one, and looking for the files a song is missing (where their folder went, then by name, each thing found with how sure it is). `listening.rs` is `Runtime::hear`: an audio file listened to (`trill-listen`), kept for the last few files. `recording.rs` is the round trip of a pair of devices: what was measured for them, kept in the settings and given to the session when they are open again (`keep_measured_latency`, `keep_latency_adjustment`). `taking.rs` is what a study does to a song (a stretch planned and then taken, a hit, a kit, notes, parts, the tempo, a reference; `hear_stretch` plays a plan before it is taken) and `study_files.rs` what a song plays out of studies (who uses a study, dropping stems, gathering beside the song, a word put right). `studying.rs` is `Runtime::studies` (the desk) and `Runtime::fetches` (the engine that fetches, with the models' list on it; another crate's list is added with `Runtime::add_fetch_list`), where studies and each list are kept and how they are moved (`keep_studies_in`, `keep_list_in`: the settings remember, and the list's owner is told); neither studies nor fetches until a program says it is one (`study_and_fetch_as_usual`). `together.rs` is working in one song with others: the one thread that connects the session to the wire (`Runtime::share`, `share_through_meeting_point`, `join`, `leave`, `remove_from_song`, `together`: who is here and what they hold, how the meeting stands at its meeting point, which connections were lost and are being asked for again), which takes what the session changed and says it, a drag thinned, hands what arrives to the session a batch at a time, and asks again by itself for a connection that was lost on the way (`Again`); the session is never held across anything that waits on the network. The meeting point's address and key are read from the settings there and set through it (`set_meeting_point`, `meeting_point`, which never says the key). `files.rs` is the audio of a shared song between the programs that hold it: a thread that reads this machine's files to say what they are (only audio, only what may go), one that hands out and takes in, what waits and why, and what a song would hand to whoever joins (`Runtime::files_to_go`, `let_files_go`, `let_files_arrive`). `renditions.rs` is the tracks this program records for the others where it has plugins they have not got: who records, when (once what the recording depends on has stood still), a render away from the session, and the recording given the name of what it is. `instruments.rs` is the catalogue on this machine: its sets as a list for the engine that fetches (`fetch_list`, a part a set; a set that arrives is made the library's by the list's hook, one that is removed is not), several asked for at once (`Ask`: the starter selection, everything, a kind; `Runtime::ask_for_instruments`), a set handed over as files (`take_set`, `set_arrived`), which instruments of the catalogue are here (`catalogue_here`), and what the open song owes (`Runtime::owed`). `updating.rs` is a newer Trill as both front ends have it: `Runtime::updates` (`trill-update`'s, with a place to remember in), the settings (whether Trill looks when it starts, a version its user silenced), the one line said unasked (`update_notice`), and what only a program that says it is one does (`look_for_updates_as_usual` for the window, `look_for_updates_when_asked` for the server, which never looks by itself). |
| `trill-mcp` | The MCP server and the windowless `trill-mcp` binary. `requests.rs` (tool arguments), `server.rs`, `music.rs` and `sounds.rs` (the tools; `sounds.rs` has the search across every preset, `save_preset` and `save_sound`, `edit_preset` and `delete_preset`, and what a reply says of a sound that was given), `hearing.rs` (`probe`, `measure` and `render`, with their arguments and replies), `files.rs` (`missing_files` and `relink_files`, and what the others say of a song that is not whole), `listening.rs` (`listen` and `transcribe`: what is in an audio file or in a study, in the recording's own bars, and MIDI of it), `taking.rs` (`take_from_study`: the arguments turned into what `trill_runtime::taking` takes), `studying.rs` (`study`: a recording taken apart, reported, stopped, listed, removed, its stems dropped, a song's study files gathered; `setup`: every list of parts there is to fetch, what runs the models and the test that finds out, and which Trill this is with a newer one looked for and fetched as the part `trill`; and what the assistant's door refuses of all of it), `instruments.rs` (what a track's notes mean, read and set, the warnings that come of it, and how a note that says how it is played becomes keyswitches), `story.rs` (the journal's tools), `catalogue.rs` (`instrument_catalogue` and `song_credits`: the instruments to be had, which are here, and what a song owes; they fetch nothing), `recording.rs` (`track_input`, `audio_inputs`, `takes`, `recording_latency`, `unused_recordings`, what `transport` says of a take of audio, and what the assistant's door refuses of them), `playing.rs` (`playback_load`: how the machine keeps up with the song as it plays and what each track costs; and what `transport` says of a song that falls short), `together.rs` (`shared_song`: share on this computer, its networks or through the meeting point the user set, join, leave, let one person go, who is here and what they hold, the notices and the edit that answers one; refused at the assistant's door, which reads only; no tool takes a meeting point or its key), `film.rs` (`film_plan` and `film`: arguments turned into what `trill-stage` takes), `wavetable.rs` (`describe_wavetable` and `set_wavetable`: a table shown frame by frame in numbers, chosen, made from a recipe or read from a file; and what `get_device` says of the synth: its matrix in words, its macros by name), `words.rs` (every tool call in plain words, where it landed and what it heard: read by the journal, the server and the window), `view.rs` (replies), `guard.rs` (limits on the in-app assistant's endpoint). |
| `trill-assistant` | The back end of the in-app assistant: runs the user's own Claude Code CLI, one process per message, confined to Trill's tools. |
| `trill-meet` | A meeting point: the small server that two Trills which cannot reach each other both connect out to, and the half of it a Trill speaks. Nothing of Trill is in it (no song, no audio, no window), its dependencies are `snow`, `getrandom` and `sha2`, and it builds as one static file for a Linux server. `secure.rs` (a connection only holders of a secret can read or write: the Noise protocol through the `snow` crate, framed here; no cipher is written), `said.rs` (what is said to a meeting point: one clear line first, `open`, `join` or `take`, then the few words of a holder's connection; every refusal a word with its sentence), `key.rs` (a keeper's key, what a meeting goes by, a knock's ticket), `keys.rs` (the keeper's file, a line a person, which holds what proves a key and not the key), `limits.rs` (every limit with its default), `server.rs` (the meeting point: a thread a connection; a meeting is a record and its holder's connection, a person in it a pipe that is passed unread and counted), `client.rs` (holding a meeting open with a key, asking for one, taking a knock up), `main.rs` (the `trill-meet` program: `serve`, `key`, `keys`, `forget`). |
| `trill-together` | The wire between programs that hold the same song, and nothing of the song: `invite.rs` (an invitation: where to connect, to the program itself or to a meeting point, and a secret, as text), `point.rs` (a meeting point as whoever shares through it has it: its address and the key its keeper gave), `wire.rs` (what is said: hello, the whole song, changes, who is here, and of a song's files: what a program has, what one wants by its hash, and pieces as bytes; who is here says which devices of the song their machine has not got), `files.rs` (files handed out and taken in by what they are and by nothing else: `Offered`, `Received`, the bounds settled before a first byte), `net.rs` (the connections of one program, each with threads of its own, bounded in size and rate; listening only between share and stop, under an invitation that can be made another (`listen_anew`), and for nothing where it shares through a meeting point: a thread holds the meeting open there, opens it again when it is lost, and takes up a connection for whoever asks; a connection says that it is there when it has nothing else to say, and is lost when nothing is heard over it), `line.rs` (a line as a home has it, to stand between two programs in a test or a measurement: slow, late, cut, dead or lost). The connection only holders of a secret can use is `trill-meet`'s (`secure`, handed on from here). |
| `trill-noir` | The egui widget kit and theme ("Instrument Noir"): Bars, Chips, Faders, text, icons, tokens, the mark, the Key Map (`keymap.rs`: boxes over keys and how hard they are struck), people (`people.rs`: the colours others who have the song open are shown in, their tag, outline and playhead). |
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
- Areas: `arrangement/` (lanes, clips, headers, ruler, automation envelopes, and `takes.rs`: a track's takes
  unfolded under it, the comp made by dragging across one, and the take that grows as it is recorded), `piano_roll.rs` (notes, and the lane of
  velocities and controller curves under them), `devices.rs` with
  `device_layouts.rs` (and `wavetable_panel.rs`: the wave an oscillator reads, which is more than its parameters
  say, and `instrument_panel.rs`: what a Sampled Instrument plays), `mixer.rs`, `browser/` and `sounds/`
  (`sounds/instruments.rs`: the library's sampled instruments, and what dropping one does), `sound_presets.rs`
  (sounds of more than one device: which there are for the browser to list, one carried from there to a
  track, the form that names and describes one, and the question before one is deleted; the browser's other
  tab calls audio files sounds), `audio_clip.rs`,
  `sample_panels.rs`, `recording.rs` (arming and takes of either kind; `inputs.rs` is what a track records of
  the audio device: the choice, the monitor switch, the level with its lamp, the line that says an input is live,
  and the recordings the song no longer uses; `recording_scene.rs` is a made-up session on scripted devices for
  its pictures and tests),
  `audio_settings.rs` (its second page is `audio_settings/input.rs`: the input's level, shown when it is asked
  for, and the round trip with its measurement), `setup.rs` (File, Setup: a tab for each list of parts there is to fetch, each part with
  what there is to read of it over the button that fetches it, and the machine's tab; `setup_instruments.rs` is
  the tab of the catalogue of instruments, which is too many parts for a list: by kind, with a search, the
  starter selection and everything as two asks, and what is on its way as one thing with one Stop;
  `setup_program.rs` is the tab of Trill itself: which Trill this is, the newer one with what there is to read
  of it over the button that fetches it, whether Trill looks when it starts, and the line in the status bar with
  the button that silences it; `setup_scene.rs` is made-up machines for its pictures), `missing.rs` (the list of the song's missing files; what it shows is looked for by a worker,
  `sounds/missing.rs`), `keyboard/` (the computer keyboard as a MIDI keyboard), `assistant/` (the chat panel),
  `making_of/` (the session played again from its journal), `study/` (a study in the window: `mod.rs` what the window holds of studies and asks of them, each on a helper
  thread; `view.rs` the workspace, placed by hand in whatever rectangle it is given; `place.rs` the Studies place
  of the browser, the dialog and the question before one goes; `along.rs` what a study found drawn along a
  reference's clip; `window.rs` the workspace in a window of its own and the drag that crosses from it),
  `recovery.rs`, `files.rs`, `stage.rs` (F11: the stage
  in place of the panes. It is drawn by `trill-stage`'s passes on the window's graphics device into a texture; the
  window's own renderer only shows that texture), `filming.rs` (File, Export a film: the panel, and the film made
  on a helper thread on a graphics device of its own), `credits.rs` (File, Credits: what the song owes for its
  sampled instruments, worked out by a worker, and the makers Trill names for the sets it offers), `load.rs`
  (a click on CPU: how the machine keeps up with the song and what each track costs; the notice and the line in
  the status bar for a song that is more than it plays in time; `LoadScene` is a made-up load for its pictures
  and tests), `opening.rs` (a song that is being opened: the plate over the song before while its file is read,
  then the line that says how far its devices are, the mark on each track that waits for one and what a device
  that is not there says on its panel; `OpeningScene` is a made-up opening for its pictures and tests),
  `together.rs` (File, Share this song and Join a song: the panel, with the meeting point's address and key
  given there once, the key in a field that shows dots; who is here in the status bar, and what is away and
  being tried again; other people's selections on clips, headers and notes and their playheads in the ruler,
  each in their colour; the notices of a clash as cards that stay until they are answered; `TogetherScene` is
  made-up company for its pictures and tests), `renditions.rs` (a track heard as someone else's Trill recorded it,
  because its plugin is on their computer and not on this one: the mark on its header, what the panel of the
  device that is not here says, and the status bar's line while this program records one for the others).
- `tests/` drives the real app headlessly: `Harness::new(options, |session| …)` opens it on a hermetic `Runtime`,
  simulated input goes in, and assertions read the session and the widgets' accessibility labels.

### Where a change goes

- **A new edit to the song:** a method on `Project` with tests in `trill-core`; then an MCP tool; then the window.
  Not the other way round: an edit that exists only in the window cannot be reached by an AI. Nothing is
  written for it to travel: what an edit did is found by comparing (`merge::diff`). Draw it in the lot
  (`merge/lot.rs`, `any_edit`), so that the tests of three programs make it too.
- **A field added to the model:** it does not compile until `merge/props.rs` gives it a place: among the
  properties of its thing (a new one goes at the end of its list: the number of a property is in files and on
  the wire), or beside them, and then `doc.rs`, `derive.rs` and `diff.rs` say what it is. A thing that people
  change needs an id that no two programs give the same (`Project::settle` gives notes, points and markers
  theirs; whoever writes a clip's notes or a curve whole says no ids, and a note or point that is equal to one
  that was there is that one, so that writing the same again is no change) or a name that is its own (a
  parameter's, a send's two tracks). If two edits of it that are each
  allowed can together say what no song may be, `derive.rs` decides what the song is then, the same everywhere,
  and the session says a sentence (`session/together.rs`). What is one person's listening and not the song
  (the loop, solo, which reference is listened to) or one machine's (which input a track records, and how it
  is heard meanwhile) is marked so in `Key::is_personal`, and never leaves the program: not in a change, not in
  a track that is made (`Change::impersonal`), not in the song that is handed to whoever joins. The order a
  song's envelopes are held in is a property of its own (`Op::Envelopes`): the order they were made in, as a
  file has them, so that nobody's list of them is shuffled by being shared; the order a track's sends are held
  in is a property of the track for the same reason (`TrackOp::Sends`; `Project::set_send` leaves a send that
  is there in its place). `TRILL_CHECK_MERGE=1` has every edit of every test held against the song worked out from its
  properties: run the engine's tests with it after touching any of this.
- **A file of a shared song** (what its clips, takes, samplers and drum racks play): the song says what the
  file *is* (`trill_core::files::FileIs`: its SHA-256, its length, its name; `AudioContent::is`, and
  `Device::files` by the slot's name), and where it lies is each machine's own. **No path is said, and none is
  taken.** A clip's path is a property that is personal (`Key::is_personal`), the state of a built-in device
  that names files is said without the places (`Session::forget_device_places`, `Session::said_song`), and what
  arrives all the same is not taken: a change that sets a path is refused, a clip that arrives whole arrives
  without one (`Op::checked`), and after anything arrived every file a device's state names is held to being a
  place of this machine's (`Session::give_places`, `session/content.rs`). So nothing that reads `audio.path`
  or a device's state reads a place another program chose: keep it so, and whatever else comes to name a file
  in the song is a property that says what the file is, with its place beside it and personal. Whoever compares
  two programs' songs compares `Session::said_song`. The bytes cross by content and by nothing else, at the
  lowest layer (`trill-together/src/files.rs`): `Offered` is a list this program made of files it has, asked by
  a hash, answering the same for whatever it does not hand out; `Received` takes only the bytes it asked for,
  into the open song's own folder (`received/<the song's id>`, so that a song hands out only what that song
  brought and its own person's files), under a name made here, and makes a file of them only when the whole
  hashes to what was asked for; what a file may cost is settled before its first byte (`Bounds`). **Only audio
  is read to be said or handed out, and only what its user let go**: the runtime's reader (`trill-runtime/src/
  files.rs`, a thread of its own) says a file only if the media cache reads it as audio, and only if it may go
  (`FileHere::may_go`): what the song brought, what the user's hand named (`hand::by_hand`), what was recorded,
  and what the user was shown and let go (`Session::let_files_go`, which sharing is). What was read as audio is
  kept from one run to the next (`files-read.json`, beside the folder of what arrived): a file as long as it was
  and written when it was is not read again. That a file was none is found out again by every run, and what is
  kept lets nothing go: a file that may not go is not looked up. A song file is a
  stranger: what it names is held until then (`Session::files_held`, `Together::held`). **An edit never waits
  for a file**: a connection has a second queue for pieces, taken only when nothing else waits (`net.rs`); and
  since what a program's system and the line have taken is ahead of whatever it says next, no more is asked of
  one program at once than is on its way there and back and arrives in an eighth of a second (`Way` in
  `trill-runtime/src/files.rs`: what the way carries is the most that was counted of late, how long it is the
  shortest that was measured, by a small question put while nothing was on its way), of all the files that come
  over that connection together, the one with least to go first. Files are asked for in strides, ahead of what
  is *written* here; and one that has arrived is read to be played
  before it has its name (`MediaCache::read_as`, `Received::before_named`), because whatever compiles the song
  meanwhile would wait for a reading that began when the name appeared. A file on its way is no missing file
  and no problem (`Session::files_arriving`, `is_awaited`). **Nor is one that nobody has said**
  (`Session::files_unsaid`, `is_unsaid`: a clip that came from another program came without a place, a slot of
  a sample player with a place made here): its bringer's program is still reading it, has not got it, holds it
  or does not read it as audio. It is theirs to put right, so it is among what arrives and never among what
  this machine is missing: shown as waiting for whoever brought it (`Waiting::ToBeSaid`), and nothing is looked
  for on this disk. (Only of what travels: what a sampled instrument names stays with its maker, nobody ever
  says it, and at the others it is a file that is not there, said as that.) What a song would hand over is shown before it is shared (`Runtime::files_to_go`), counted
  by the reading that hands it out (only audio), with what the song names that is none listed as what stays:
  a song file may name anything, and its user is to see what it named. What arrived comes beside the song at a
  save, with the recordings (`session/beside.rs`, `from others`). Hold all of it with strangers who have the
  invitation (`trill-runtime/tests/files.rs`), and after touching how bytes are asked for or handed across run the
  `files_cross` example at 300 MB, and on lines as homes have them (`--files 12 --through-here --line
  5000:50000:20`, and the same at `:60` and at `1000:10000:40`), holding what it prints beside the table in
  `docs/working-together.md`: its small tests did not show a connection let go of for too many questions, an
  edit that waited two seconds, a line left idle more than half the time, nor what is asked for swinging until
  nearly nothing was.
- **A track heard where its plugin is not** (a *rendition*, `trill-core/src/renditions.rs`): in a song others
  hold too, whoever has a plugin records what the track's devices make of the track, up to and including the
  last one that someone lacks, and whoever lacks it plays that recording in place of those devices and runs
  what follows them itself. Four things hold it together. **The compiler plays it from the song alone**: a
  track says what stands in for its first devices (`Track::rendition`: after which device, of the song as
  which print, what the recording is, by whom), and where one of those devices could not be made on this
  machine the graph has the recording where the last of them would have left the track's sound
  (`Compiler::rendition_of`, `StoodIn`, `through` in `processor.rs`, which runs none of them). So a render and
  a song that plays are the same, and whatever comes to run a track's devices in parts goes through `through`.
  **It is the song as it was until it is recorded again, and says so**: what a recording depends on is a print
  (`Project::rendition_print`, asked of `Session::said_song` so that it is the same on every machine): the
  track's clips, the devices up to that one, the envelopes that move them, the tempo, and where one of them
  listens to another track all that is played before. Whatever else comes to change what leaves a device goes
  into `Heard` there; what comes after the device (the fader, the pan, the sends, later devices) and what is
  one person's own stays out, or a fader would have everything recorded again. **It is nobody's edit**:
  `Session::land_rendition` writes it without a step, the journal keeps none (`journal/change.rs` leaves it out
  of a track's settings), one that takes another's place is no clash, and which recording a machine plays is
  its own (`Track::rendition_at`, personal: the rendition's own once that is here, and until then the one
  before it, so that no track falls silent for as long as a recording takes to arrive;
  `Session::place_renditions` keeps it, and removes the recording that was let go of). **Making one is the
  runtime's** (`trill-runtime/src/renditions.rs`): everyone says which devices of the song their machine has
  not got (`Presence::lacks`), whoever has every device a recording takes and the lowest number makes it, once
  what it depends on has stood still for two seconds, by a render away from the session. **Every track that is
  wanted then is recorded in the one pass** (`RenderOptions::after`, a file a track, each beginning where its
  track first plays and ending where its sound does): a render makes the song's plugins anew and waits for
  them, which is most of what one track takes, and a pass a track took seven minutes for five tracks of the
  owner's song. A pass of several that comes to nothing is tried again a track at a time, so that the one that
  cannot be recorded is the only one that is not. The recordings are written into a folder beside the song's
  own folder of files and then given, in that folder, the name of what they are. (Not into that folder while
  they are written: what is no file of the song and no part of one on its way is cleared out of it.) After
  touching any of it, try it on real plugins: two windows on profiles of their own joined in one song, one
  started with `--real-plugins` and one without (which then has none), and what the second is told by
  `shared_song` held beside the numbers in `docs/working-together.md`. While someone lacks a device, everyone
  who holds the song keeps the
  recording (`Session::others_lack`): whoever needs it may be connected to them alone. Only a track of the song
  that plays clips has one (`Project::can_be_rendered`). `trill-engine/tests/renditions.rs` holds what a
  session does with one, `trill-runtime/tests/renditions.rs` two and three programs of which one was not
  handed the instrument (`Runtime::start_with`), and the window's `tests/renditions.rs` what it says.
- **Something another program says:** a `Message` in `trill-together/src/wire.rs`, handled in the one place
  that connects wire and session (`trill-runtime/src/together.rs`). Whatever arrives is untrusted: bounded
  where it is read, and a change is held to the model's rules (`Change::checked`) before any of it is taken
  in. The protocol's number goes up when what is said changes its meaning; a program that speaks another is
  refused in a sentence. Tests bind 127.0.0.1 on a port the system picks and nothing else. What the network
  says of a connection is what the thread goes by (`Event`): one that was lost on the way (`Lost` with
  `told: false`) is asked for again by whoever asked for it, one that either side ended and said so is not,
  and a join that failed says whether asking again is of use (`for_good`). Whatever comes to end a connection
  says which it is. **A line that goes down says nothing**, and the side that only hears would wait for minutes:
  so a connection says that it is there by itself when it has nothing else to say (`Message::Still`, from the
  thread that says, which never waits for the session), and one over which nothing at all is heard for ten
  seconds is lost on the way (`net.rs`, `Pulse`). The same person on a second connection is that person back:
  the older connection is let go of without a word (`heard`, the `Hello`). The loopback shows none of this: a
  megabyte takes a millisecond there, and no connection ends without a word. `trill_together::line` stands a
  line as a home has it between two programs (slow one way, slower the other, late, cut, dead, and *lost*:
  ended for whoever connected and not for where it led), for a test and for the measuring examples (`--line`):
  whatever comes to wait on the network, or to pace what crosses it, is held on one.
- **Something that reaches a meeting point** (`trill-meet`): whoever connects is a stranger until a key is
  proved, and bounded after. It is read with a time limit into room of a fixed size, never into room that a
  length it said asks for, and one connection's fault ends that connection. Every limit is a field of `Limits`
  with a default, a flag of `serve`, a `No` with its word and its sentence, and a line in the log: add all of
  them together, and throw at it in `tests/strangers.rs` whatever the limit is for. The meeting point can
  neither read nor join a song: it passes a pipe's bytes and knows a meeting by `Meeting::of`, which is worked
  out one way from the invitation's secret; the two programs prove the invitation to each other through the
  pipe (`secure`), as they do directly. Whoever has not got the invitation cannot knock: what a meeting goes
  by is said in the clear, so whoever asks for one is put a question (a number by lot) and has to answer with
  what it and the meeting's `Door` come to, which the holder handed over when it opened the meeting; a wrong
  answer is told what an unknown meeting is told, and the holder hears nothing. A key never crosses the
  network: which key it is does, and the rest is the handshake keyed with `Key::proof`. What a connection's
  thread is to let go of, the chores let go of too when its time is over (`Shared::tend`: a knock, what a key
  that was taken away holds), so that nothing is held for good because a thread did not get to it. What can
  happen as often as a stranger likes is a line in the log once and then a count (`Shared::counted`). The log
  holds the names a keeper gave, a few digits of a meeting and counts: never a key, what proves one, either of
  the two things a meeting is known by, a secret or a byte of what is passed (`tests/meeting.rs` looks for
  each). A socket has one handle, shared (`Arc<TcpStream>`; `Hearer::socket`),
  and never a second one of the system's (`try_clone`): on Windows a second handle keeps time limits of its
  own, and ending a connection through one fails now and then with the system saying it is not connected,
  while on Linux neither is so, and what is tested here runs there. `said::VERSION` goes up when
  what is said changes its meaning. Tests bind 127.0.0.1 on a port the system picks, and no address, name or
  key of anyone's is in the repository: an example says `meet.example.org`. A test makes the few connections
  it is about and none in a loop, with their number beside it: a closed connection keeps one of the machine's
  16,384 ports for two minutes, and the tests of this crate and `trill-together` leave about 180 a run.
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
  `session/sounds.rs`): one undo step, and a device this machine has not got, or that is found not to start
  within the step, is left out and said (`Given::without`), never left in the song as a problem. A plugin is
  made after the step (below, "A device that takes time to make comes after the song has it"), so one that
  then does not start, or does not load what it was to start from, stays in the song with its sentence in
  `Session::problems`, as one added by itself does: undo takes it out. A sound holds each device's type,
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
- **Something of a release, or of how a newer Trill goes in** (`trill-update`, `scripts/release.sh`,
  `installer/trill.iss`; [docs/installing-and-updating.md](docs/installing-and-updating.md) is what a user is
  told): four rules hold by every route. **Nothing is taken that its makers did not sign**: an announcement is
  read under the keys built into the program (`release::read`) and a build is held to the checksum it gives, as
  it is fetched and again right before it goes in; no path of a release lies outside Trill's own folder
  (`Release::check`), and no version is taken that is not newer. **Nothing but the announcement is fetched
  unasked**, and that only by the window, at a start, where its user has not switched it off
  (`Runtime::look_as_a_start_does`); the server, the assistant's door, a test, a picture and a build of one's own
  never look. **Nothing changes under a program that runs**: Trill starts itself a second time by the path of its
  own file, so an update is staged by a program that runs and applied only by `trill_update::start`, in the
  window, while it holds the folder's lock alone (`Place::alone`); whatever comes to run from the install folder
  for longer than a job calls `start` and holds what it returns. The server never applies: the program a client
  started has to be the one that answers it. **A version that does not come up is put back** by the one that put
  it in place, which waits to see it up (`Place::try_out`, `came_up` at the window's first frame). The file names
  and the layout of what is staged (`place.rs`) are read by the version before: change them only so that an older
  Trill still applies what a newer one's release is. What the installer registers is named in one place each
  (`INSTALLED_AS`, `RUNS_AS`, and the same two for a trial); a trial's hooks (`made_for_a_trial`: another key, a
  version, an address, a build from this machine) are compiled in only by `release.sh --trial` and a release has
  none of them. What a release is made of is checked where it is gathered (`release.sh`): each program says
  which Trill it is (`trill_update::says`, held to the version and the commit), asks for no runtime DLL, and
  names no folder of the machine that built it (`--remap-path-prefix`, then looked for); whatever else must
  never ship is one more check there. Tests publish on a shelf under a key of their own (`trill_update::scripted::Makers`,
  `Parts::updating`); the real programs are tried with `release.sh --trial`, as `docs/development.md` says, after
  touching `place.rs`, `start` or the installer's script.
- **Something that says a MIDI channel:** a note and a clip's controller curve are each on one of sixteen
  (`trill_core::Channel`: 1 to 16 wherever a person, a model or a file reads or writes one, 0 to 15 only on the
  wire to a plugin, and a value of the type cannot mix the two up). Absent is the first, and neither a note nor a
  curve on the first is written with one, so a song that uses no other channel is the file it always was and
  needs no format number. A clip holds at most one curve a controller a channel
  (`Clip::set_controller_on`, `controller_on`; the plain `set_controller` and `controller` are the first
  channel's). An event says its channel (`Event::channel`; `Event::new` is the first, `Event::on` any), which is
  why a built-in device, which reads only `event.kind`, takes all sixteen as one instrument: keep it so, and a
  device that is to tell channels apart reads the field. Whatever the engine remembers for a track it remembers
  by channel (`Sounding`, `Told` in `graph.rs`: both in a box, because a track's state changes hands between
  graphs), keyswitches and what a hand plays are on the first, and `EventKind::AllNotesOff` lets go of every
  channel whichever it is said on. Both plugin hosts keep keys and pedals by channel (`trill-plugins/src/notes.rs`);
  a VST3 controller is the parameter the plugin names for it on that channel (`vst3/params.rs`, `MidiParams`,
  asked where the plugin is made), and a channel the plugin's event bus has not got is its first. What a channel
  *means* is the instrument's to say: `InstrumentProfile::channels`, shipped in `instruments/known.rs` where it
  was measured, or read from the plugin's own setting each time (`known::channels_now`,
  `Session::channel_names`) and kept nowhere in the song, as a kit is. A tool takes a channel by its number or
  that name (`input::parse_channel`), says channels only where a clip uses another than the first, and warns
  where one is written for an instrument that names none; the texts say a channel is for an instrument that
  names its channels and is left alone otherwise, which is followed to the letter. The scripted plugins of both
  formats have a kind that says what arrived on which channel (`kind::CHANNELS`, `notes::heard`), the engine has
  an instrument with a string to a channel (`scripted:strings`), and `tests/channels.rs`, `cores.rs` and
  `realtime.rs` hold the rest; the proof on real instruments is `trill-mcp/tests/consort.rs` (opt-in).
- **Something that names a file:** a song's files are its audio clips' and what its devices say theirs are
  (`DeviceFiles`; `trill_dsp::SampleFiles` for the sampler and the drum rack). A device that keeps a path in its
  state and does not say so there is not listed as missing, not looked for and not relinked. A sampled
  instrument names its own file, and of its recordings only the ones that are not where that file says: those
  that were given another place, and those that are missing (`Session::song_files` adds what a device says is
  missing to what its state names). A song does not list the hundreds that are where they belong. The project file's
  `files` table (`trill_core::files`, written by `Session::save`) is beside the song, not part of it: no edit
  touches it, and it needs no format number.
- **Something that records an input, or reads one:** the device's thread only hands samples across
  (`capture::Sink::push`: no allocation, no lock, an audio thread under rule 2), and the callback takes exactly a
  block of input for each block it makes (`Reader::take`, once, before any track is made), so that every track on
  every thread reads the same frames: whatever comes to use the input reads `Common::input` and takes nothing
  itself. The two devices' clocks are told apart from time stamps, of which only the least late of each quarter
  second is believed (a stamp is late or right, never early). A stamp says when and never how many: frames are
  missing when the device says so (`capture::Missing`: its flag on a buffer that does not follow the one before,
  its own count of frames), not because a stamp was late, and a stamp the device does not stand by, or that
  cannot be, is not shown to the line (`Said::heard_ns`). A host hands over what its device said with every
  buffer (`Sink::push_said`). That an input shares the output's clock (`InputFormat::shares_clock`) is a belief
  the stamps may overturn (`Reader::parts`), and no evidence at all where the system knows no container for
  either end. The stamps may establish it too, of a pair that is taken for two clocks: the audio thread only
  writes down, stretch by stretch, how far apart the two devices' least late stamps have their counts
  (`capture::Evidence`, into a ring of atomics); it is judged off the audio thread, by a line that a stretch of
  late stamps does not turn (`capture::look`, from `Engine::note_clocks` at the session's chores), and the
  reader takes the verdict only while nothing is kept of the input, so never under a take. The engine keeps a
  pair's verdict for the run (`one_clock_seen`, `two_clocks_seen`). Its constants are what the cable's stamps
  were measured to do: change them with the cable test's figures beside you. While the belief holds and the
  rates are one, a frame of the input is a frame of the engine, handed
  across to the bit (`Reader::whole`: its place is on a whole frame, put again while nothing is kept of the
  input and kept from the moment a take is asked for or a measurement runs, `Reader::keep`; what an exclusive
  output writes to a device is rounded to the
  device's steps, so that the way out is as exact as the way in). The place an input is read at is put again a
  quarter of a second after it opens: a take that is not counted in waits for that (`Processor::record`,
  `Reader::settled`), so that no take has the step in it.
  **In a song that others hold too** a take is a clip that is kept beside its track, in a lane
  (`ClipOp::Lane`, `Key::Clip(_, LANE)`): made, changed and removed as a clip is, and two that were put in one
  lane over one stretch at once lie in two (`Doc::derive`, `Derived::displaced`). What a take writes while it is
  recorded is said when it has landed and not before (`Session::hold_what_is_said`, around the take's group in
  `session/playing.rs`). Which input a track records is its person's (above), so nothing that arrives gives a
  track an input, and what arrives is not the hand: a change from another program opens no microphone. Of two
  recordings put over one stretch of a track that keeps takes at the same moment, the later has it: the program
  whose work was the earlier makes room under the other's (`Session::give_way`, by `Project::cover`), says so
  once and can put its own back over it. A recording's file follows by itself (above, "A file of a shared
  song"): the others have the take at once, and its file on its way.
  A take is written in the engine's clock and rate by a thread of its own (`recorder.rs`), and is put where it
  was heard: the hand-over's delay and the output's latency, or what was measured in its place, come off
  (`shift_of` in `session/recording.rs`). Where the song went during a take is what the audio thread says
  (`AudioMark`: it rolled, jumped, came round, was punched in and out, stopped), never what the session takes
  from a switch: each stretch lands where it was sung as a take of its own (`passes`), and nothing that was
  sung while the song rolled lands nowhere. Nobody waits for a take's last sound with the session held
  (`Session::stop`; `stop_and_wait` is for a test, and the `transport` tool waits off the lock). What a track
  records is in the song's file (`Track::input`, absent in a file saved before and then not written) and is one
  person's where the song is shared; what is armed, what is open and the round trip are the session's. A take's audio goes into the take's undo group
  (`land_audio_take`), takes are kept beside a track's clips and nothing that reads `clips` sees them, and what a
  recording or a comp covers is kept as a take first (`Project::cover`): nothing recorded is ever lost by an
  edit. No recording is ever deleted: one that is removed goes to the system's Recycle Bin, or a `removed`
  folder beside the takes (`session/bin.rs`), and only by `Runtime::delete_unused_recordings`, which a front
  end asks about first and the assistant's door refuses. What is unused is the runtime's to say, because it is
  a question about every song Trill knows of (`Runtime::unused_recordings`: the open song and what undo can
  reach, asked of what names a file, `beside::named_by`; the songs the settings remember; the safety copies).
  An undo step keeps what it set and not a song, so what undo can reach is asked of the properties
  (`Session::each_state`: the song, the song with whatever a step can bring back, and what steps set a name
  from; in a song others hold, everything that was removed, which anyone can bring back), and a recording that
  comes beside the song is named there by a quiet write of everything that is held (`rename_everywhere`),
  and paths are held against one another as the file system has them (`spelled_path`). A song's recordings come
  beside it at its first save by a second name where the disk allows and by a copy made on a thread of its own
  where it does not (`session/beside.rs`); saving under another name leaves them where they are. Frames that did
  not arrive are silence of their length, never a shift, and are counted and said. The numbers it is held to
  are in `docs/engine.md` and `trill-engine/tests/recording.rs`, `hand.rs`, `killed.rs`, `takes.rs`, `cores.rs`
  and `realtime.rs`; `tests/cable.rs` (opt-in) is the same on a virtual cable.
- **Something that changes the song without being an edit** (a state a plugin came to in its own window):
  write it through `Session::write_quietly`, which takes it into the song's properties (so that whoever else has
  the song hears of it) and has the undo steps that had set the same thing follow, and enter it in the journal
  as an aside (`Session::note_device`), or the song the journal leads to drifts from the song that is open.
  Nothing writes `Session::project` behind the properties' back. A plugin's state that is read back is news
  only where the plugin was changed in this program (`Session::state_is_news`): what a plugin writes out is not
  always the bytes it was given.
- **Something a user should know of** (a feature, a limit, a number that was measured): the page of its topic
  in `docs/` (`window.md`, `instruments-and-effects.md`, `plugins-and-presets.md`, `writing-music.md`,
  `sounds-and-studies.md`, `files.md`, `engine.md`, `ai-and-mcp.md`, `making-of-and-film.md`,
  `working-together.md`, `meeting-point.md` for whoever keeps one, `installing-and-updating.md`), a tool in the
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
  audio threads (`Engine::tend_threads`): with the first graph that has anything to make at once where the
  stream's own thread is an audio thread (a device's), so that a song has its threads before it is heard, and
  when a song asks where the stream is driven by hand, as in tests. What a song costs is only known once it has
  played: one that brings devices is spread until a quarter of a second of playing has shown what it needs
  (`Processor::unknown`). A device that goes down costs the buffer it went down in, on whichever thread
  (`Processor::after_fault`). Falling short is a sentence, and the session's (`Shortfall` in `session/cores.rs`):
  it counts what the audio threads count, keeps ten seconds of it for anyone who asks (`Session::keeping_up`,
  which takes nothing from anyone; `take_load` and `take_costs` are for one tool that measures), and both front
  ends say its words, once a time it begins (`Shortfall::episode`). "Keeps up with a real song" is checked with
  the `keeps_up` example, on a song with its plugins: after touching what an audio thread runs or how work is
  handed across, run it and hold what it prints beside the numbers in `docs/engine.md`.
- **A device that takes time to make comes after the song has it.** Whatever brings one (a song opened or
  begun, an edit, an undo or a redo, a preset or a state given, the audio stream at another rate), the session
  compiles the song with the devices that take time (`DeviceFactory::takes_time`: the plugins) left out and
  their places kept (`DeviceMeta::coming`), and a thread of its own makes them one by one in the order they were
  asked for (`session/opening.rs`); `Session::housekeeping` puts each in its place when it has arrived. That is
  no edit: nothing of it is a step of the history or the journal, and `is_modified` stays what it was. Nobody
  waits for a plugin with the session held: whatever else comes to give the song a device goes this way (it
  does by going through `compile_and_swap`), and a front end never calls `wait_until_opened`, which is for
  tests and tools. A place that is kept is empty (an instrument silent, an effect letting through; where a
  device is made anew, what was in its place goes on until its successor is there), is what the song says of
  it (which is what a save, a safety copy and a render take), and is named by `Session::loading` and
  `Session::arriving`; what cannot be done to it is refused by `Session::there` in a sentence, so call that
  first in whatever comes to ask something of a device itself. A preset only the device can load, given
  before the device is there, is in the song as what it is to start from (`Device::from_preset`); whoever
  makes a device from the song makes it through `compile::make`, which has it load that, so that a render, a
  probe and the device that arrives are the same, and the state it came to takes the marker's place when it
  arrives. What arrives is held against the song as it is by then (the same song, uri, state, preset to start
  from and key, the same rate) and let go of otherwise. The graph of another song takes over nothing of the one
  before (`Graph::another_song`, `Graph::adopt`): ids begin again in every song, and a kept place would
  otherwise be given the device of the song before that had its number. **What leaves is taken down by the
  same thread**, after what there is to make: a graph the audio thread retired that holds such a device
  (`Graph::holds_slow`, `Engine::retire_through`), the records of devices an edit removed (`DeviceBank::gone`),
  the processor of a stream at another rate; the other order was measured and is slower. **Nobody asks a
  plugin anything while one is being made, where the answer can wait** (`Session::making_devices`): a question
  goes to the thread plugins are made on and is answered when the making is done, which for a window that asks
  with every change to the song was six seconds of standing still. A device's values are those it last
  reported (`DeviceMeta::reported`), and the safety copy is put off. What cannot wait (every plugin's state,
  for a save; a preset; a window) holds the thread back from beginning another making until it is answered
  (`Session::hold_makings`), so that it waits for the one in hand. `trill-engine/tests/opening.rs` and
  `tests/coming.rs` hold all of it with the scripted devices told to take their time, to have one thread and to
  be slow to let go of (`Loading::making`, `one_thread`, `leaving`), the window's `tests/opening.rs` what is
  said meanwhile, and `trill-mcp/tests/hearing.rs` what a model is told and which tools wait
  (`WAITS_FOR_DEVICES` in `server.rs`: add a tool there that asks a device something). "Opens quickly" and
  "stays alive while it loads" are checked with the `opens` example, on a song with its plugins: hold what it
  prints beside the numbers in `docs/engine.md`.
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
- **Undo is one's own, and a step is what it set.** The history keeps for each step the properties it set and
  what they were (`session/history.rs`), not the song, for someone working alone too. Taking a step back puts
  back what is still as the step left it; what someone else has set since stays theirs and is counted
  (`Session::take_back`, `Turned`). A thing the step made goes with whoever's work is in it, and is held; a
  thing it removed comes back with what was done to it meanwhile. Ids are never given twice: the count goes on
  when a step is taken back, which is why two projects are equal whatever the next id is.
- **Nothing readable or writable without the invitation, and nothing listening unasked.** A program listens
  from the moment its user shares until they stop, and for nothing at all where it shares through a meeting
  point; what connects has to prove the invitation's secret before a word is said; the assistant's door
  neither shares nor joins. Who is here and what they hold is the runtime's, and is no part of the song, the
  session or the journal.
- **A meeting point's address and key are in the settings and nowhere else.** Not in a song's file, a journal,
  a log, a picture or a reply a model reads: no tool takes either (what a tool is called with is in the journal),
  `Runtime::meeting_point` says whether there is a key and never the key, the window's field for it shows dots
  and is emptied by the click that keeps it, and the made-up scenes say `meet.example.org`. (The address is in
  the invitation, which is its holder's to hand on.) `trill-runtime/tests/together.rs` looks for the key in
  everything a program writes or says, and the window's and the server's tests in what each shows.
- **A link that is lost comes back by itself.** A connection that was lost on the way is asked for again by
  the program that asked for it, after a pause that grows, for as long as the song is open; the status bar says
  so meanwhile, and what each did is merged when it is back. Nobody pastes an invitation twice in one sitting.
  "Ask for the invitation again" is said only where asking again is no use. A connection is lost, too, when
  nothing is heard over it for ten seconds: nobody goes on being shown who is no longer there.
- **Whoever let someone in lets them go, and the invitation is another from then on.** Nobody is the host, so
  there is no removing by rank: a program lets go of who came in by its own invitation
  (`Runtime::remove_from_song`, `Here::can_be_removed`, a connection it did not ask for), and refuses in a
  sentence for whoever came in through another program or let this one in. The one let go is told with a word
  (`REMOVED`, so that their program says so and asks for nothing again) and keeps the song as it was: nothing
  handed over is taken back. What makes it hold is the invitation, which they still have: it gets another
  secret at the same place (`Net::listen_anew`; through a meeting point the meeting is ended and another
  held), nobody who was proving the old one at that moment is let in (the gate in `net.rs`), and whatever it
  had let in that this program had not greeted is let go of. Whoever stays is told the new one over the
  connection they have (`Message::Invitation`), and takes **its secret and nothing else**, from the program
  they joined only, for the address they reached it by (`Invitation::with_secret_of`): nothing a program says
  sends another one anywhere. Whatever else comes to end someone's part in a song goes the same way, or the
  invitation they hold brings them back. `trill-runtime/tests/together.rs` holds it between three programs,
  directly and through a meeting point, with a line cut after to see that who stayed comes back by itself.
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
  nor fetches until its program says it is one (`Runtime::study_and_fetch_as_usual`). The one thing fetched
  without being asked for is the announcement of the newest release, a small text that is the same for
  everyone: by the window, at a start, where it is a release and its user has not switched that off (above,
  "Something of a release").
- **A microphone is opened by a hand.** The input is open only while something needs it: a track that records
  it is armed or heard, a measurement runs, the settings panel was told to show its level. Looking at a panel
  opens nothing, and the window says for as long as an input is open that it is. The rule is the session's, in
  one place (`Session::hold_to_the_hand`), and not a check in each tool: the input is opened for a track only by
  the user's own hand, in this run of the program and in this song, by arming a track as it then records an
  input, giving an armed track its input, or switching its monitoring on. A thread says that it is the hand
  (`trill_engine::hand::by_hand`: the window's, an outside MCP client's) and one that says nothing is nobody's,
  so whatever comes to act on the session without saying so opens nothing: the assistant's door, an undo or a
  redo made there, a song opened, begun or recovered, a track duplicated or pasted. A step of the history
  knows whether the hand made it, all of it (`Step::by_hand`; a group or a gesture several had a part in is
  nobody's): the hand's undo or redo is the hand's only of such a step. And the rule is closed in depth: what
  opens the input (`Session::input_needed`, `begin_audio_take`, what `live_now` tells the graph is heard) asks
  `hand.open` and `hand.heard` itself, so that a path which forgets `hold_to_the_hand` leaves an armed track
  that records nothing and says so, never an open input (a test in `session/playing.rs` arms behind the rule's
  back). What comes to want the
  input any other way is disarmed or held, with a sentence (`Session::input_held`, one click lets it go);
  arming does not carry into another song; the device that is recorded from is not changed at the door while
  an input is live. The door's refusals stay, as the polite word; they are not what holds the rule.
  `trill-engine/tests/hand.rs` and `trill-mcp/tests/mcp.rs` try every route at both doors and count the opens
  on the scripted host: add a route there with whatever comes to give a track an input, arm one or bring one
  into a song. An input that was opened and cannot be read (the audio thread could not be told) is closed
  again and said. No test of the default run opens a real input, and
  nothing in the repository ever opens one that is not named to it: `tests/cable.rs` plays into and records from
  a virtual cable, by name, and plays nothing while another program records from it.
- **Hermetic by default.** Tests, screenshots and scripts never read or write the user's settings
  (`%APPDATA%\trill`), sample library, studies or safety copies: `RuntimeOptions::hermetic()` or `Storage::At` on a
  temporary folder in tests, `trill-mcp --scratch`, or `--profile <folder>` on either program, by hand (which keeps
  the list of the plugins a scan found in that folder too, and a run that keeps nothing keeps it in the temporary
  folder: `scans_kept_in` in `trill-runtime`, `PluginHost::keeping_scans_in`). (Studies
  and what was fetched are kept in `%LOCALAPPDATA%\trill` unless the settings say where; with `Storage::At`,
  beside the settings.) Nor does a test leave anything outside the repository and the temporary folder: on macOS a
  process a test takes down on purpose (the scripted instruments' `die`, a scripted model's crash) has the fault
  end it by SIGKILL, of which the system keeps no crash report; a real crash goes nowhere near that, and is
  reported as the system reports it.
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
  `target/screenshots` (off-screen, taking no focus; on Linux and macOS as `ui/unseen.rs` has it: through X as a
  window the window manager leaves alone, and without a title bar by a program never brought to the front); look at
  them and leave them there. Never run it for every
  picture as routine. Only the five the README and `docs/` show, in `docs/images`, are tracked (`render.sh readme`, on purpose): four
  of the window and one of the stage, which the film example draws. Films and their stills go under `target/`.
- **The assistant keeps five rules**, written at the top of `trill-assistant/src/lib.rs`: it never touches
  credentials, never signs anyone in, never bundles or modifies the CLI, never promises usage limits, and is
  called "the assistant", with "Claude Code" only in descriptive sentences. The only tools it is given are
  Trill's, a plan list and web research; files and the shell stay off because its panel cannot ask permission.
- **Nothing of a plugin maker's is in the repository**: no preset file, no part of one, no dump of a state.
  Tests of `trill-plugins/src/vendor` build their own small files in the format and load them into the scripted
  plugins; the ones on installed plugins are `#[ignore]`d (`tests/vendor.rs`). Trill only reads in a maker's folders.
- **Each system's code is its own.** What only Windows does is under `cfg(windows)`, what only macOS does under
  `cfg(target_os = "macos")` and what only Linux does under `cfg(target_os = "linux")`, each in arms or files of its own, so that a change for one leaves the other as it
  was; what every Unix does alike is under `cfg(unix)`. A test that is about one system by its nature (a path as
  Windows writes it, the code a process ended with) says so where it chooses; it is not left out elsewhere. A
  macOS temporary folder is reached through a link (`/var` is `/private/var`), so a test that compares a path
  Trill shows names its files by their real path. What macOS and Linux have not got yet is in `docs/status.md`.
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
