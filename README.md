<img src="assets/trill-logo.svg" width="420" alt="Trill">

# Trill

Trill is an open-source digital audio workstation written in Rust. The goals:

- **It does not crash or stall.** The audio thread never allocates, locks or waits; plugins are scanned in a
  separate process; a device that panics produces a moment of silence instead of taking the app down. And when
  something does take it down, the work is not lost: a project with unsaved changes is copied to a safe place
  every twenty seconds and offered back the next time the program starts.
- **It hosts your plugins.** CLAP and VST3.
- **An AI can drive all of it.** Every operation is available over the
  [Model Context Protocol](https://modelcontextprotocol.io), so an assistant can compose, arrange, mix and render a
  song, and read back loudness and spectral measurements to judge its own work.

![The arrangement and piano roll, showing a song written through MCP](docs/images/first-light.png)

This is an early build. See [Status](#status) for what works and what does not yet.

## Quick start

```sh
cargo run -p trill-app                 # the application
cargo run -p trill-app -- --demo       # ...opening a demo song
cargo test --workspace               # the test suite
```

Development builds are optimized (see `[profile.dev]`), because unoptimized audio code cannot keep up in real time.

### Building and testing

`cargo test --workspace` runs about 1,300 tests in about a minute. It needs nothing installed and touches nothing
outside the repository and the temporary folder. The tests that do need a particular machine are skipped unless
asked for:

```sh
cargo test -p trill-plugins -- --include-ignored    # against the CLAP and VST3 plugins installed here (minutes)
cargo test -p trill-media -- --include-ignored      # decoding audio files found in this machine's usual folders
TRILL_TEST_PLUGIN_WINDOWS=1 cargo test -p trill-plugins --test editor_windows -- --test-threads=1   # opens windows
TRILL_TEST_CLAUDE=1 cargo test -p trill-assistant --test real_cli    # talks to an installed, signed-in Claude Code
TRILL_TEST_CLAUDE=1 cargo test -p trill-app live_ -- --test-threads=1    # ...through the assistant's panel (seven messages)
cargo test -p trill-engine --test machine -- --include-ignored --nocapture     # opens this machine's audio outputs (silence only) and prints their latency
cargo test -p trill-runtime --test machine -- --include-ignored --nocapture    # lists and opens this machine's MIDI inputs
```

Run the first two after touching plugin hosting or audio decoding, and the last two after touching the audio
device or MIDI port code. Everything else about devices is tested without any: the engine plays through a
scripted host whose audio thread is the test itself (`trill_engine::host::scripted`), so a device can be unplugged,
refuse a format or stop answering, and a recorded note can be checked to the sample.

The build is set up to be quick to repeat: code that only runs while building (procedural macros, build scripts)
is not optimized, debug information is limited to file and line (`CARGO_PROFILE_DEV_DEBUG=full` for a debugging
session), and on Windows the linker is the one that ships with Rust (`.cargo/config.toml`). Everything from
scratch, tests included, takes about four minutes on a 16-core machine; after an edit, seconds.

Working in a second checkout (`git worktree add`)? `sh scripts/warm-worktree.sh` there copies the libraries the
first one has already built, and the first build compiles only Trill's own crates: a minute and a half.

### Let an AI write a song

While the app is open it serves MCP on `http://127.0.0.1:7777/mcp` (`--mcp-port` changes the port, `--no-mcp` turns
it off), so you can watch and hear the edits happen and undo them like your own:

```sh
claude mcp add --transport http trill http://127.0.0.1:7777/mcp
```

Or run the DAW without a window, launched by the MCP client over stdio:

```sh
cargo build -p trill-mcp
claude mcp add trill -- /path/to/target/debug/trill-mcp
```

Then ask for music: *"Write a slow, warm synthwave track in A minor, about a minute long. Render it and check the
mix."* [`examples/first-light.trill`](examples/first-light.trill) was written this way, entirely through tool
calls.

### Or ask from inside the window

The assistant is a panel down the right side (`Ctrl+Shift+A`, or the button at the right end of the top bar). It
runs the Claude Code you have installed, signed in as you are there, against the same MCP server, and adds what a
client outside the window cannot have:

- every message goes with what you are looking at (the selected track, clips, notes and device, the time
  selection, the playhead, the loop, the open tab), so "make this busier" has a "this";
- everything it changes while answering one message is one undo step, with an Undo on the turn itself;
- when it has finished, what it made or edited is selected and brought into view (unless you are busy elsewhere in
  the window at that moment), and each tool call's line leads to what the call changed;
- the reply appears as it is written, set from its Markdown, and can be selected and copied like any text; each
  tool call is a line in plain words that unfolds to what was sent and answered, and Stop ends the turn.

Which model answers is chosen under the message field: Claude Code's own choice, one of the names it takes
(`fable`, `opus`, `sonnet`, `haiku`), or any name you type. It is passed to Claude Code as it is; what your account
can use is for Claude Code to say.

Trill never asks for, reads or stores a login: if Claude Code is not installed or not signed in, the panel says so
and what to do about it in a terminal. Conversations are kept per project beside the settings and taken up again
the next time the project is open. The assistant comes in by a door of its own on the MCP server (`/assistant`),
through which files are written only into the project's folder or `Documents/Trill`.

![The assistant after a turn that added a bass line, with the Undo for it](docs/images/assistant-answered.png)

## What is in the box

**Instruments**

| Device | |
|---|---|
| `builtin:polysynth` | 16-voice subtractive/FM synth: two oscillators, unison, sub, noise, multimode filter, two envelopes, LFO, mono/glide. 15 presets from pads and basses to FM keys and bells. |
| `builtin:drumkit` | Synthesized drum machine on the General MIDI map, with 808, 909, tight, trap and lo-fi kits. Each drum is shaped after measurements of recorded ones: where its energy sits and how long it rings. |
| `builtin:sampler` | Plays one audio file across the keyboard: loops with crossfade, reverse, one-shot, amp and filter envelopes. |
| `builtin:drumrack` | Sixteen pads on the General MIDI drum notes, each playing its own audio file, with level, pan, tune, decay and choke groups. |

**Effects**: `reverb`, `delay` (tempo-synced, ping-pong), `eq` (6-band), `compressor`, `limiter` (look-ahead
brickwall), `chorus`, `filter` (resonant, with LFO), `pump` (sidechain-style ducking locked to the grid),
`saturator`, `utility`.

**Plugins**: installed CLAP and VST3 plugins appear beside the built-in devices as `clap:<plugin id>` and
`vst3:<class id>`. Their state is saved in the project, and their parameters are automatable and reachable over
MCP like any other device's. A plugin's own editor opens in a window (Windows only so far), from the Editor button
on its panel or at the end of its chip in the mixer, and what is changed there flows back into the project.

**Songwriting**: chords by symbol or Roman numeral, voicings, rhythms, arpeggios, bass lines, drum grooves with
fills, melodies by scale degree, swing and humanizing, and reading a song's key and chords back from its notes
(`trill-theory`). Over MCP an assistant says `Am9 | Fmaj9 | Cmaj7 G/B | Em7`, drop-2, house groove, rather than
spelling notes.

**Sounds**: a library of the sample folders you point it at, scanned in the background and searchable by words,
tags, tempo, key and length (all read from file and folder names). Audio clips play WAV, AIFF, FLAC, MP3 and Ogg
Vorbis at any sample rate, and can be warped to follow the project tempo: `beats` keeps drum hits sharp, `tonal`
keeps sustained sound smooth, `repitch` plays faster or slower like a record; warped clips can be transposed.

**Engine**: sample-accurate MIDI and audio clips, clip looping, non-destructive trimming and splitting, audio
fades, automation of track and master volume, pan and any device parameter with straight, held or curved
segments, group tracks and return tracks with sends, a metronome, sample-exact transport looping, mute/solo, undo/redo, offline rendering that is bit-identical to
playback, and render analysis (peak, LUFS, stereo correlation, spectral balance, per-track levels).

**Playing and recording**: every MIDI keyboard that is plugged in plays the armed tracks (or the selected one)
without a visit to the settings: notes, velocity, pitch wheel, mod wheel and sustain pedal, for built-in
instruments and plugins alike. A key is placed in its audio buffer by the time it went down, so it is heard a
fixed moment later rather than whenever the next buffer happens to start. Recording writes what is played into
clips where it was *heard*: the engine takes the output latency off again, to the sample. A count-in, loop
recording that layers each pass onto the last, one undo step per take. See
[Playing and latency](#playing-and-latency).

**Application**: an arrangement with multi-clip editing (move and copy across tracks, trim either edge, split,
fades, a clipboard, time selection, and deleting, repeating or inserting a stretch of time on every track at
once), markers, group and return tracks, and automation lanes that are edited by hand or drawn in steps; a piano
roll; device panels generated from each device's parameters; a mixer; MIDI import and export; project files; WAV
export. `F1` lists the keyboard shortcuts.

An automated control is not locked: drag it and it is yours. Its envelope is switched off, not lost, the mark in
the control's corner turns into an outline, and a click on that mark (or on the switch of its lane) hands the
control back to the envelope. And when the program did not live to save a project, the window that opens next
offers it back: recover it, discard it, or decide later (`File`, `Recover unsaved work`).

The browser's **Sounds** tab is the sample library: add folders, search as you type, filter by one-shot or loop,
kind, tempo and key, mark favorites, and hear a sound as you select it. Drag a sound onto a track for an audio
clip where it lands (loops whose name states a tempo follow the song's), below the tracks for a new track, onto a
sampler or onto a drum rack pad; files dragged in from the file manager land the same way. Audio clips show their
waveform, and the Clip tab shows an audio clip's file with its gain, offset, fades, warp mode, source tempo and
transposition. The sampler shows its sample and the drum rack its sixteen pads.

![The sound browser, waveforms on audio clips and an audio clip in the Clip tab](docs/images/sounds-browser.png)

**Play it from the keyboard you type on.** `Ctrl+K`, or the keyboard button in the top bar, turns the two middle
rows of letters into a piano on the selected track's instrument: `A S D F G H J K L ; '` are the white keys from C
and `W E T Y U O P` the black ones, `Z` and `X` move them an octave down and up, `C` and `V` play softer and
louder, and `Tab` sustains while it is held. The keys are where they are on the keyboard, whatever layout types
on them. A strip above the detail panel shows the octave, the velocity, the keys with the letter on each and the
notes being held, and says why nothing sounds when nothing can (no track selected, a track with no instrument, a
muted one); the piano roll's own keyboard lights the same notes. The switch, the octave and the velocity are
remembered.

While it is on, a letter it uses plays a note, and the shortcut that letter had is `Alt` with the letter (`F1`
lists them as they are at the moment). It never takes a key while a text field, the sound search or a dialog has
the keyboard, or while `Ctrl`, `Alt`, `Shift` or the Windows key is held. And no note is left sounding: whatever
ends the playing (the switch, another track selected, an octave change under a held key, the window losing the
keyboard, a dialog, another project, exit) ends its notes.

![A chord held on the computer keyboard, lit in the strip and on the piano roll](docs/images/keyboard-playing.png)

No samples at hand? `cargo run -p trill-app --example sound_fixtures -- some/folder` synthesizes a small pack and a
song made from it (it is what the screenshots are taken from); add `some/folder/Night Shift` in the browser, or
start with `--library "some/folder/Night Shift" --browse sounds some/folder/night-shift.trill`.

## MCP tools

| Area | Tools |
|---|---|
| Project | `get_project`, `set_project`, `new_project`, `save_project`, `load_project`, `recover_project`, `import_midi`, `export_midi`, `history` |
| Tracks | `create_track`, `update_track`, `delete_track` |
| Devices | `list_devices`, `describe_device`, `add_device`, `get_device`, `update_device`, `delete_device`, `show_device_editor` |
| Clips | `create_clip`, `create_audio_clip`, `update_clip`, `split_clip`, `duplicate_clip`, `delete_clip` |
| Sounds | `search_sounds`, `library`, `preview_sound`, `set_sample` |
| Notes | `get_notes`, `add_notes`, `set_notes`, `remove_notes`, `transform_notes` |
| Music | `write_chords`, `write_drums`, `write_arpeggio`, `write_bass`, `write_melody`, `analyze_music`, `music_reference` |
| Automation | `set_automation`, `get_automation`, `set_markers` |
| Arrangement | `edit_time` (remove, repeat or open up a stretch of time on every track at once) |
| Playback | `transport` (play, stop, seek, record), `render` |
| Playing live | `play_notes`, `midi_inputs`, `audio` (device, sample rate, buffer size, latency) |

Time is in beats. Pitches are MIDI numbers, note names (`"F#3"`) or drum names (`"kick"`). Notes can be written
compactly as `[pitch, start, length, velocity]`. Errors say what to do next: an unknown id comes back with the ids
that exist, a bad note with its index and the reason.

The HTTP server listens on localhost only and has no password: any program on the machine may use it. Pages in a
web browser may not; requests that carry a browser's `Origin` are refused.

## Playing and latency

The audio device, its sample rate and its buffer size are chosen while the program runs and remembered; the
project, its undo history and the playhead are untouched by a change. If the device is not there the next time,
or refuses the format, or is unplugged while a song plays, Trill opens the nearest thing (down to no device at
all), says what it did and why, and takes the device up again when it is back. A driver that never answers
costs a few seconds' wait, not the program.

How short the delay between a key and its sound can be depends on how the device is opened:

- **Shared** (the default): the device also plays every other program, through the system's mixer. On Windows
  that mixer works in periods of 10 ms and keeps a few of them queued, so the delay is around 30 ms whatever
  buffer size is asked for.
- **Exclusive** (Windows; `exclusive` in the audio settings): Trill takes the device for itself and plays it
  directly, at the buffer size asked for, down to about 3 ms on ordinary hardware. No other program can use the
  device meanwhile. This is WASAPI's exclusive mode and needs nothing installed.
- **ASIO** is not built. The audio library Trill uses supports it, but only with Steinberg's ASIO SDK present
  at build time (downloaded under Steinberg's licence, with LLVM for the bindings), which is too much to ask of
  everyone who builds Trill. It would be an optional cargo feature, off by default, plus a fourth audio host in
  `trill-engine/src/host`.

On macOS and Linux the system's own audio (CoreAudio, ALSA) honours the buffer size without any of this; neither
has been run yet.

## Architecture

```
crates/
  trill-core      Project model, edit operations, and the device API. No audio or UI dependencies.
  trill-dsp       Built-in instruments and effects, and offline time-stretching.
  trill-media     Audio file decoding and caching, waveform overviews, sample-rate conversion, WAV writing.
  trill-engine    Realtime engine, offline renderer, analysis, and the Session.
  trill-library   The sound library (scanning, indexing, searching sample folders) and the settings file.
  trill-plugins   Plugin hosting: scanning in a child process, the plugin main thread, CLAP and VST3.
  trill-runtime   What both programs share, built once: session, media cache, library, settings, housekeeping,
                  safety copies of unsaved work.
  trill-theory    Music theory for writing parts: chords, scales, voicings, rhythms, grooves, analysis.
  trill-mcp       MCP server (library) and the headless `trill-mcp` binary.
  trill-assistant Back end of the in-app assistant: runs the user's own Claude Code, confined to Trill's tools.
  trill-noir      The GUI kit: theme, widgets and painting.
  trill-app       The desktop application (`trill`).
```

Three ideas hold it together:

1. **One front door.** `Session` owns the project, its undo history and the engine. The GUI and the MCP server are
   both thin clients of it, so there is exactly one implementation of every edit and the two can never disagree.
2. **The audio thread only plays.** Edits happen off the audio thread. After each one the project is compiled into
   an immutable render graph and handed over through a lock-free queue; the audio thread swaps it in between
   blocks, carries running devices and sounding notes across, and sends the old graph back to be freed elsewhere.
3. **Everything is a device.** Built-in instruments, effects and hosted plugins implement one small trait
   (`DeviceProcessor`), so the engine does not know or care which it is driving.

The same `Processor` runs realtime playback and offline rendering. The tests rely on that: they render projects
and assert on the audio (note onsets to the sample, identical output for any buffer size, identical output while
the graph is swapped on every block). The GUI is tested headlessly with simulated input against a real session.

## Status

Working: everything listed above, with tests.

Not yet: running plugins out of process (only scanning is isolated today, so a plugin that crashes while playing
still takes the app with it; the safety copy is what saves the work), plugin editors on Linux and macOS,
session (clip-launching) view, audio recording (MIDI is recorded; audio is not), recording of the pitch and
mod wheels (they are heard while playing; clips have nowhere to keep them yet), ASIO, warp markers, plugin
delay compensation, multi-core rendering. Only Windows has been run so far.

## Licence

Trill is free software: you can redistribute it and modify it under the terms of the GNU General Public License,
version 3 or (at your option) any later version. It comes with no warranty. See [LICENSE](LICENSE).

The Inter typeface bundled in `crates/trill-noir/assets/fonts` is under the SIL Open Font License; its text is beside
the font.
