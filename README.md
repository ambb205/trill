<img src="assets/icon.svg" width="64" height="64" alt="">

# DAW

An open-source digital audio workstation written in Rust. The goals:

- **It does not crash or stall.** The audio thread never allocates, locks or waits; plugins are scanned in a
  separate process; a device that panics produces a moment of silence instead of taking the app down.
- **It hosts your plugins.** CLAP today; VST3 is in progress.
- **An AI can drive all of it.** Every operation is available over the
  [Model Context Protocol](https://modelcontextprotocol.io), so an assistant can compose, arrange, mix and render a
  song, and read back loudness and spectral measurements to judge its own work.

![The arrangement and piano roll, showing a song written through MCP](crates/daw-app/screenshots/first-light.png)

This is an early build. See [Status](#status) for what works and what does not yet.

## Quick start

```sh
cargo run -p daw-app                 # the application
cargo run -p daw-app -- --demo       # ...opening a demo song
cargo test --workspace               # the test suite
```

Development builds are optimized (see `[profile.dev]`), because unoptimized audio code cannot keep up in real time.

### Let an AI write a song

While the app is open it serves MCP on `http://127.0.0.1:7777/mcp` (`--mcp-port` changes the port, `--no-mcp` turns
it off), so you can watch and hear the edits happen and undo them like your own:

```sh
claude mcp add --transport http daw http://127.0.0.1:7777/mcp
```

Or run the DAW without a window, launched by the MCP client over stdio:

```sh
cargo build -p daw-mcp
claude mcp add daw -- /path/to/target/debug/daw-mcp
```

Then ask for music: *"Write a slow, warm synthwave track in A minor, about a minute long. Render it and check the
mix."* [`examples/first-light.dawproj`](examples/first-light.dawproj) was written this way, entirely through tool
calls.

## What is in the box

**Instruments**

| Device | |
|---|---|
| `builtin:polysynth` | 16-voice subtractive/FM synth: two oscillators, unison, sub, noise, multimode filter, two envelopes, LFO, mono/glide. 15 presets from pads and basses to FM keys and bells. |
| `builtin:drumkit` | Synthesized drum machine on the General MIDI map, with 808, 909, tight, trap and lo-fi kits. |

**Effects**: `reverb`, `delay` (tempo-synced, ping-pong), `eq` (6-band), `compressor`, `limiter` (look-ahead
brickwall), `chorus`, `filter` (resonant, with LFO), `pump` (sidechain-style ducking locked to the grid),
`saturator`, `utility`.

**Plugins**: installed CLAP plugins appear beside the built-in devices as `clap:<plugin id>`. Their state is saved
in the project, and their parameters are automatable and reachable over MCP like any other device's.

**Engine**: sample-accurate MIDI and audio clips, clip looping, automation of track volume/pan and any device
parameter, sample-exact transport looping, mute/solo, undo/redo, offline rendering that is bit-identical to
playback, and render analysis (peak, LUFS, stereo correlation, spectral balance, per-track levels).

**Application**: arrangement view with clip editing, piano roll, device panels generated from each device's
parameters, mixer strips with meters, project files, WAV export.

## MCP tools

| Area | Tools |
|---|---|
| Project | `get_project`, `set_project`, `new_project`, `save_project`, `load_project`, `history` |
| Tracks | `create_track`, `update_track`, `delete_track` |
| Devices | `list_devices`, `describe_device`, `add_device`, `get_device`, `update_device`, `delete_device` |
| Clips | `create_clip`, `create_audio_clip`, `update_clip`, `duplicate_clip`, `delete_clip` |
| Notes | `get_notes`, `add_notes`, `set_notes`, `remove_notes`, `transform_notes` |
| Automation | `set_automation`, `get_automation` |
| Playback | `transport`, `render` |

Time is in beats. Pitches are MIDI numbers, note names (`"F#3"`) or drum names (`"kick"`). Notes can be written
compactly as `[pitch, start, length, velocity]`. Errors say what to do next: an unknown id comes back with the ids
that exist, a bad note with its index and the reason.

## Architecture

```
crates/
  daw-core      Project model, edit operations, and the device API. No audio or UI dependencies.
  daw-dsp       Built-in instruments and effects.
  daw-engine    Realtime engine, offline renderer, analysis, and the Session.
  daw-plugins   Plugin hosting: scanning in a child process, the plugin main thread, CLAP.
  daw-mcp       MCP server (library) and the headless `daw-mcp` binary.
  daw-app       The desktop application (`daw`).
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

Not yet: VST3 hosting (in progress), plugin editor windows, running plugins out of process (only scanning is
isolated today, so a plugin that crashes while playing still takes the app with it), session (clip-launching)
view, audio recording, time-stretching/warping, group and return tracks, MIDI input devices, plugin delay
compensation, multi-core rendering, compressed audio formats (audio clips are WAV only). Only Windows has been
run so far.
