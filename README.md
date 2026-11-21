<img src="assets/trill-logo.svg" width="420" alt="Trill">

# Trill

Trill is an open-source digital audio workstation written in Rust, built so that an AI can use all of it.
Everything the window does is also a tool call over the [Model Context Protocol](https://modelcontextprotocol.io),
so an assistant can write, arrange, mix and render a song, and measure what it made in place of hearing it.

![The arrangement and piano roll, showing a song written through MCP](docs/images/first-light.png)

Three goals decide every trade-off:

- **It does not crash or stall.** The threads that play a song never allocate or take a lock, and a song is
  played on as many processor cores as it needs. Plugins are scanned, and songs with plugins are rendered, in
  separate processes. Unsaved work is copied to a safe place every twenty seconds and offered back the next
  time the program starts.
- **It hosts your plugins.** CLAP and VST3, with their presets and their own editor windows.
- **An AI can drive all of it.** Every edit has one implementation, which the window and the MCP server share.

Trill is an early build and has only been run on Windows. [Status](docs/status.md) lists what is not built yet.

## What is in it

- **A full arrangement**: audio and MIDI clips, a piano roll, a mixer, groups and returns, automation, tempo and
  time-signature changes, MIDI recording, WAV and stem export.
  ([The window](docs/window.md), [the engine](docs/engine.md))
- **Built-in instruments and effects**: a poly synth, a wavetable synth with a bank of 82 sounds, a drum machine,
  a sampler, a drum rack, a player for sampled instruments in SFZ format, and thirteen effects.
  ([Instruments and effects](docs/instruments-and-effects.md))
- **Your plugins**: CLAP and VST3 instruments and effects, with presets listed and searched like the built-in
  ones. ([Plugins, presets and sounds](docs/plugins-and-presets.md))
- **Free sampled instruments on request**: a catalogue of 574 pianos, strings, drum kits and more that their
  makers let anyone use, fetched from File > Setup when you ask.
- **Tools that write music**: chords by symbol, voicings, bass lines, drum grooves, melodies with ornaments and
  articulations. ([Writing music](docs/writing-music.md))
- **A recording taken apart**: stems, beats, lyrics, loops and MIDI from a recording, found by models that run on
  your own machine. ([Sounds, listening and studies](docs/sounds-and-studies.md))
- **An assistant in the window**, and an MCP server for one outside it. ([Working with an AI](docs/ai-and-mcp.md))
- **A song worked on together**: two or more people in one song at once, each in a Trill of their own, connected
  directly by invitation, each undoing their own work. ([Working together](docs/working-together.md))
- **A replay of how a song was made**, a cinematic view of the song, and a film of either as an `.mp4`.
  ([The making-of, the stage and films](docs/making-of-and-film.md))

## Getting started

You need:

- 64-bit Windows. It is developed on Windows 11; macOS and Linux have not been run.
- [Rust](https://rustup.rs) 1.90 or newer with the MSVC toolchain, which needs the "Desktop development with C++"
  workload of the Visual Studio Build Tools.

Build and run from the repository:

```sh
git clone https://github.com/ambb205/trill.git
cd trill
cargo run --release -p trill-app -- --demo    # the window, on a built-in demo song
cargo run --release -p trill-app              # the window, on an empty project
cargo run --release -p trill-app -- examples/first-light.trill    # the window, on a project file
```

The first build takes several minutes. `cargo run -p trill-app` without `--release` is quicker to build and is
what development uses; it is optimized too, because unoptimized audio code cannot keep up.

## Using the window

Start with `--demo` and press `Space` to play. `F1` lists every keyboard shortcut.

| To do this | Do this |
|---|---|
| Add an instrument or an effect | Double-click it in the browser's Devices tab: it goes to the selected track, and an instrument with no track selected gets a new one |
| Add a sound | Add a sample folder in the browser's Sounds tab, then drag a sound onto a track, or below the tracks for a new one |
| Write notes | Double-click a lane to create a clip, then click in the piano roll below it |
| Play an instrument | Plug in a MIDI keyboard, or press `Ctrl+K` to play from the computer keyboard |
| Record what you play | Arm a track and press `F9` |
| Choose the audio device | `Ctrl+,`, or click the device in the status bar |
| Export the song | File > Export WAV |
| Get instruments and study models | File > Setup |
| Ask the assistant | `Ctrl+Shift+A` |
| Watch how the song was made | `Ctrl+Shift+M` |
| Watch the song on the stage | `F11` |

Songs are `.trill` files. [`examples/`](examples) has three that play on the built-in devices alone, and
[`examples/first-light.trill`](examples/first-light.trill) was written entirely through tool calls.

[The window](docs/window.md) describes editing, recording and the audio settings in full.

## Let an AI write a song

While the window is open it serves MCP on `http://127.0.0.1:7777/mcp`, so you can watch and hear the edits happen
and undo them like your own. Any MCP client can connect; with Claude Code:

```sh
claude mcp add --transport http trill http://127.0.0.1:7777/mcp
```

Trill also runs without a window, started by the MCP client over stdio:

```sh
cargo build --release -p trill-mcp
claude mcp add trill -- /path/to/trill/target/release/trill-mcp
```

Then ask for music: *"Write a slow, warm synthwave track in A minor, about a minute long. Render it and check the
mix."*

The assistant inside the window (`Ctrl+Shift+A`) runs the Claude Code you have installed, signed in as you are
there. It knows what you have selected, everything it changes for one message is one undo step, and Trill never
asks for, reads or stores a login.

[Working with an AI](docs/ai-and-mcp.md) has the details and the list of tools.

## Optional extras

Trill ships no recordings and no models, and fetches nothing unless you ask.

| Extra | What it adds | How to get it |
|---|---|---|
| Sampled instruments | Pianos, strings, guitars, drum kits: a starter selection of 1.8 GB, or all 574 | File > Setup > Instruments |
| Study models | Stems, beats and lyrics from a recording, about 3 GB | File > Setup > Studies |
| [ffmpeg](https://ffmpeg.org) | Films as `.mp4`, and recordings in formats Trill does not read itself | `winget install Gyan.FFmpeg` |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Studying a recording behind a link; keep it up to date | `winget install yt-dlp.yt-dlp` |
| [Claude Code](https://claude.com/claude-code) | The assistant in the window | Install it and sign in |

File > Setup shows each part's size, licence and source before you fetch it. File > Credits lists what a song
owes the makers of the sampled instruments it plays.

## Documentation

| Page | What it covers |
|---|---|
| [The window](docs/window.md) | Editing, playing and recording, audio and MIDI settings, latency |
| [Instruments and effects](docs/instruments-and-effects.md) | The built-in devices and the catalogue of sampled instruments |
| [Plugins, presets and sounds](docs/plugins-and-presets.md) | Hosting CLAP and VST3, presets from every source, saved sounds |
| [Writing music](docs/writing-music.md) | Chords, grooves, melodies, drum maps and articulations |
| [Sounds, listening and studies](docs/sounds-and-studies.md) | The sample library, and taking a recording apart |
| [A song's files](docs/files.md) | Missing files and relinking |
| [The engine](docs/engine.md) | Automation, tempo changes, rendering and measuring |
| [Working with an AI](docs/ai-and-mcp.md) | MCP clients, the assistant in the window, the tools |
| [The making-of, the stage and films](docs/making-of-and-film.md) | The session journal, the stage view, film export |
| [Building, testing and architecture](docs/development.md) | The test suite, machine-specific tests, the crates |
| [Status](docs/status.md) | What is not built yet |

## Development

```sh
cargo test --workspace                    # about 2,800 tests; needs nothing installed
cargo clippy --workspace --all-targets    # must be clean
cargo fmt --all -- --check
```

[`CLAUDE.md`](CLAUDE.md) is the map of the code: what lives in which crate, where a change goes, and the
conventions particular to Trill. [CONTRIBUTING.md](CONTRIBUTING.md) says how to send a change.

## Licence

Trill is free software: you can redistribute it and modify it under the terms of the GNU General Public License,
version 3 or (at your option) any later version. It comes with no warranty. See [LICENSE](LICENSE).

The Inter typeface bundled in `crates/trill-noir/assets/fonts` is under the SIL Open Font License; its text is
beside the font. The sampled instruments and models Trill can fetch are their makers', each under its own
licence, and are not part of Trill.
