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

`cargo test --workspace` runs about 2,630 tests in a few minutes. It needs nothing installed and touches nothing
outside the repository and the temporary folder. The tests that do need a particular machine are skipped unless
asked for:

```sh
cargo test -p trill-plugins -- --include-ignored    # against the CLAP and VST3 plugins installed here (minutes)
cargo test -p trill-mcp --test presets -- --include-ignored --nocapture    # an assistant's pass over Surge XT's presets, an amplifier's, a kit, a guitar tone and a patch of their makers', and a bought preset found among the samples
cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1    # what installed plugins say of their parameters, channels and presets
cargo test -p trill-plugins --test key -- --include-ignored --nocapture --test-threads 1    # Surge XT playing what its device listens to, and its effect, as a vocoder, shaping a bass by a muted kick, in both formats
cargo test -p trill-plugins --test tempo -- --include-ignored --nocapture --test-threads 1    # Serum 2's arpeggiator through two changes of tempo: every step heard where the song's tempo map has its beat
cargo test -p trill-plugins --test lifetimes -- --include-ignored --nocapture    # plugin instances made and dropped beside one that stays, an attempt to a process: counts the crashes, and says where each was and what the machine had of memory; half an hour, and no attempt is started on a machine short of memory
cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1    # the makers' own presets of Serum 2, the Ample guitars, Superior Drummer 3 and the Archetypes: listed, loaded by name and by a file's path, and heard to differ
cargo test -p trill-mcp --test controllers -- --include-ignored --nocapture    # a SWAM saxophone and Surge XT played from the controller curves of clips
cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture    # renders and probes of Superior Drummer 3, a SWAM saxophone and an Ample bass; the Ample instruments rendered again to the same samples; a song of installed plugins on one processor and on several
cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture --test-threads 1    # what Trill ships about the Ample guitars and bass and Superior Drummer 3, checked on the plugins themselves: a drum asked for by name and heard on two kits, a slide, a scoop, a harmonic and a bend written by name and heard
cargo test -p trill-app a_modelled_saxophone -- --include-ignored --nocapture    # the same saxophone typed on the computer keyboard, through the window's own code (no window opens)
cargo test -p trill-mcp --test wavetable against_the_installed -- --ignored --nocapture    # a sawtooth on the wavetable synth beside Serum 2's and Surge XT's as they come: what of each is not on a harmonic, its pitch and its level (a measurement)
cargo test -p trill-mcp --test wavetable the_filters_against -- --ignored --nocapture    # the wavetable synth's low-pass filters beside Surge XT's at the same settings: where each is 3 dB down, how high its peak stands, how steeply it falls (a measurement)
cargo test -p trill-mcp --test hearing a_guitar_through_its_amplifier -- --include-ignored --nocapture    # an Ample guitar through Archetype Tim Henson, saved as one sound in a song and given to a track of another: the same samples in both
cargo test -p trill-dsp --lib share_of_a_core -- --ignored --nocapture    # what the wavetable synth costs, from one voice to thirty-two notes of sixteen copies (a measurement; run it alone)
cargo test -p trill-engine --test bank measured -- --ignored --nocapture    # every number of every sound of the wavetable synth's bank, and the volume that would put each at its kind's level
cargo test -p trill-mcp --test bank serum -- --ignored --nocapture    # Serum 2's factory presets measured by kind on the notes the bank is measured on: the ranges the bank is held to (minutes; it loads the plugin about a hundred times, so check free memory first)
cargo test -p trill-media -- --include-ignored      # decoding audio files found in this machine's usual folders
TRILL_MAGPIE_PROJECTS=<...>/data/projects cargo test -p trill-listen --test magpie -- --ignored --nocapture --test-threads 1    # what Trill hears in recordings that Magpie took apart, against what Magpie found in them (the owner's Magpie project and its saved projects; minutes)
TRILL_MAGPIE_PROJECTS=<...>/data/projects TRILL_STUDY_PARTS=<the fetched parts> TRILL_STUDY_ON=nvidia cargo test -p trill-study --test magpie -- --ignored --nocapture --test-threads 1    # those recordings studied with the real models (stems, beats, words and their times) against what Magpie found; needs the parts File, Setup fetches, and runs a graphics card for a minute a recording (processor, card, card2 or nvidia)
TRILL_TEST_PLUGIN_WINDOWS=1 cargo test -p trill-plugins --test editor_windows -- --test-threads=1   # opens windows
TRILL_TEST_CLAUDE=1 cargo test -p trill-assistant --test real_cli    # talks to an installed, signed-in Claude Code
TRILL_TEST_CLAUDE=1 cargo test -p trill-app live_ -- --test-threads=1    # ...through the assistant's panel (seven messages)
cargo test -p trill-engine --test machine -- --include-ignored --nocapture     # opens this machine's audio outputs (silence only) and prints their latency
cargo test -p trill-runtime --test machine -- --include-ignored --nocapture    # lists and opens this machine's MIDI inputs
cargo test -p trill-engine --test journal what_recording_a_step_costs -- --ignored --nocapture    # what keeping the journal costs an edit on a large project (a measurement; run it alone)
cargo test -p trill-stage --test drawn -- --ignored    # what the graphics card makes of the stage, and a film as a file (a graphics card; two of them ffmpeg)
cargo test -p trill-mcp --test mcp a_film_is_made -- --ignored --nocapture    # a film of a song and of its making over MCP, kept in the assistant's working folder (a graphics card)
cargo test -p trill-app a_film_is_made_from_the_window -- --ignored --nocapture    # a film from the window's panel, watched as it is made, and one stopped (a graphics card)
cargo test -p trill-dsp --test real_sfz -- --ignored --nocapture    # the SFZ reader on the files a synth installed here keeps: their text only, counted (TRILL_SFZ_FOLDER names another folder)
cargo test -p trill-runtime --test instruments the_catalogue -- --ignored --nocapture    # fetches five small sets of the catalogue of instruments from where they are published (84 MB, by the system's curl) through the program's own fetching, and checks each twice, finds it in the library, plays it and removes it (TRILL_CATALOGUE_SETS names others by id, or `starter`, or `all`: every set, one at a time, 26 GB and half an hour; TRILL_CATALOGUE_REPORT names a file for a line a set, which a later run goes on from)
cargo test -p trill-runtime --test instruments a_fetch_that_is_stopped -- --ignored --nocapture    # a set that comes as one archive and one that comes file by file, each stopped in the middle and asked for again, with where each went on from (350 MB)
cargo test -p trill-runtime --test instruments a_set_kept_under_a_deep_folder -- --ignored --nocapture    # two small sets fetched into a folder deeper than 260 letters, checked and played (36 MB)
cargo test -p trill-app the_starter_selection_is_fetched_from_the_panel -- --ignored --nocapture    # the starter selection asked for with the Setup panel's own button on the catalogue that is built in: fetched, checked, found by the library, on a track, played (1.8 GB)
cargo run -p trill-fetch --example many -- <a list of addresses> <a folder>    # how long many small files take one at a time, on one connection and several at once: what the engine's count was chosen by
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
  tool call is a line in plain words that unfolds to what was sent and answered, and Stop ends the turn;
- a big request, a whole song, runs to its end by itself: the assistant writes a plan, which stands above the
  message field as a checklist it ticks off, and the panel says how long it has run, which step it is on of how
  many one request may take, and what Claude Code estimates it has cost so far. It is still one undo step.

Under the message field: which model answers (Claude Code's own choice, one of the names it takes: `fable`, `opus`,
`sonnet`, `haiku`, or any name you type), how hard it thinks (Claude Code's own, or `low` to `max`), whether it may
use the web, and the most steps one request may take. The model and the effort are passed to Claude Code as they
are; what your account can use is for Claude Code to say.

With **Web** on, the assistant may search the web and read pages for what Trill does not know (a genre's
conventions, a technique), and every search and page is a line in the conversation with its query or address. Off,
it is given no way to reach the web. It never has a tool that reads or writes your files or runs a program: a page
it reads can at worst talk it into changing the song (one Undo) or writing a project, render or MIDI file into the
folders named below. Why the line is drawn there is in
[`crates/trill-assistant/src/lib.rs`](crates/trill-assistant/src/lib.rs).

Trill never asks for, reads or stores a login: if Claude Code is not installed or not signed in, the panel says so
and what to do about it in a terminal. Conversations are kept per project beside the settings and taken up again
the next time the project is open. The assistant comes in by a door of its own on the MCP server (`/assistant`),
through which files are written only into the project's folder or `Documents/Trill`.

![The assistant after a turn that added a bass line, with the Undo for it](docs/images/assistant-answered.png)

### Watch how a song was made

Trill keeps a journal of the session: every change to the song is a step in it, with who made it (you at the
window, the assistant, or a program working through MCP), when, what it was in words and where in the song it
landed; between the steps, what you and the assistant said, its plan, and what it measured. **Making of**
(`Ctrl+Shift+M`, or the View menu) plays the session again from it: the arrangement and the piano roll show the
song as it was, clips arriving and notes filling in in the order they sound, the view going where each step
landed; the conversation as it came stands down the right; and the whole session is a line along the bottom to
play, hold, step through (`Left`, `Right`) and scrub, at half speed up to eight times.

- The waiting is cut out. A step is given the time it takes to watch and a long wait is a beat, so an hour of
  work plays in a few minutes, or in one at a higher speed.
- Nothing there can change the song: what is watched is a copy, and leaving (`Esc`) puts the window back as it
  was.
- Undo takes nothing out of the story. A step that was undone is followed by one that takes it back, and a redo
  by one that brings it back.
- **The journal never travels with the song.** It has your conversation in it, so it is kept in Trill's own
  folder (`journals`, beside the settings), written when the song is saved, and never in the project file or
  beside it: sending someone a `.trill` file sends the song. To hand the story on, *File, Making of, Export*
  writes it to a `.trillstory` file, without the conversation unless you choose the item that says with it; the
  other side watches it with *Watch one from a file*.
- It costs little: a step holds what it changed, and a plugin's state (megabytes) is held once however many
  steps it lasts through. A hundred steps on a song with a megabyte of state add about 30 KB. Past 20,000
  entries or 32 MB of story the oldest entries are let go, and the story begins with the song as it was after
  them. Journals are kept for the two hundred songs most recently saved.
- *View, Keep a journal* switches it off; *Delete this song's journal* deletes one.

### The stage, and a film of the song

![The stage on the verse of the demo song: its lanes above, the piano rolls of the two parts that drive the section below, lit by what plays](docs/images/stage.png)

**The stage** (`F11`, the button beside the metronome, or the View menu) is the song filmed as it plays: Trill's
own lanes, clips and piano rolls on the whole screen, lit by what sounds, with a director choosing the shots.
Nothing on it is made up for the camera. It is the song's interface, staged.

- **A director that reads the song.** Wide on the fullest sections, close on the piano roll of a part that
  carries the tune, the lanes over the rolls of whoever drives a section otherwise; a cut where the song jumps
  and a move where it does not. The shots are a function of the song alone, so the same song is the same film.
- **Two things light it, and they are kept apart.** What is *written* lands exactly: a note lights on its beat
  and a kick is a flash, to the frame. What is *heard* sets the mood: how loud the mix is, and each part by its
  own level.
- **Recordings and automation are on it too.** An audio clip, or the sample a track plays, is its waveform, lit
  where the playhead has just been; an automated parameter is its line.
- **A tempo or a time signature that changes is part of the picture.** The stage follows the song through every
  change of either, so a note still lights on the frame it sounds on and a film is as long as its sound. The
  ruler says what arrives beside the bar it arrives at ("7/8 · 140 BPM", an arrow before a tempo the song
  glides to), the place at the top right reads in the bars and the tempo the song has there and lights as they
  change, and the arrival is titled as a section is.
- It is drawn by passes of its own on the graphics card (light that spreads, depth of focus, and in a film the
  blur of motion), and the window's own drawing is untouched: with the stage shut, none of it runs.
- `Space` plays and stops. `Esc` or `F11` leaves, and the window is as it was, its size included.

**File, Export a film** (`Ctrl+Shift+F`) makes an `.mp4` of it. The panel asks what the song does not say:

- *what of*: the song, or how it was made (below);
- *size and rate*: 1920×1080, 3840×2160, or 1080×1920 upright for a phone, at 60, 30 or 24 frames a second;
- *which stretch*: all of it, the time selected in the arrangement, or a section by its marker.

The film renders **the song's own sound** in the same job, as *Export WAV* would: plugins are waited for, and
each part is lit by its own level in that render. Frame 600 of a film at sixty a second is the stage at ten
seconds however long it took to draw, so nothing depends on how busy the machine is. *Fine* exposes every frame
from eight moments and draws it at twice its size; *Plain* is one moment, as the window shows it, and about
twice as fast. The film is made away from the window, which goes on being worked in: the panel and the status
bar say how far it is, and *Stop the film* ends it and keeps nothing. Frames are handed to
[ffmpeg](https://ffmpeg.org), a program found on the `PATH` when a film is made and not a part of Trill;
without it the film is numbered pictures in a folder with its sound beside them, and the panel says so before
any work is done. A film needs a graphics card.

**A film of how the song was made** (*How it was made* in that panel, or *Film…* in the making-of's bar) stages
the journal. The camera goes where each step landed: close on a part while it is being written, wide between.
Notes arrive lit. What you asked and what the assistant answered are cards over the song, as in a silent film;
its plan ticks off; and the film ends on the finished song. The story's time is the film's, told as much faster
as it takes to be no longer than the song, because **the finished song plays under it**, timed so that the film
ends as the song does. (What was being made could not be heard while it was made: half of it was not there yet.
The song it became is the one sound that is true of all of it.) The film is made from the journal and has your
conversation in it, so it goes where you say and nowhere else.

An assistant films too, and directs: see `film_plan` and `film` under [MCP tools](#mcp-tools). Without the
window, a song that plays through the built-in devices is filmed by
`cargo run -p trill-stage --example film -- SONG.trill --film OUT.mp4`.

## What is in the box

**Instruments**

| Device | |
|---|---|
| `builtin:polysynth` | 16-voice subtractive/FM synth: two oscillators that can be turned off, unison, sub, noise (as wide as it is set), multimode filter, two envelopes, a pitch envelope (an 808's knock), LFO, mono/glide, a pitch wheel whose range can be set. 15 presets from pads and basses to FM keys and bells. |
| `builtin:wavetable` | Wavetable synth, and the instrument to shape a sound with: two oscillators that each read a table of waves at a position that can move (unison to 16, sync, bend and squeeze, FM of one by the other), a sub, noise, two filters (12 and 24 dB low, band, high and notch, two combs; in a row, side by side or one for each oscillator), three envelopes, three LFOs (free or in time with the song), four macros a sound gives names to, and a matrix of sixteen slots that says what moves what. 24 tables ship, made in code; a table can also be made from a recipe or read from an audio file. `mono_below` draws what lies under a frequency to the middle, which every bass of its bank sets. Its bank is 82 sounds: 19 basses, 10 leads, 10 plucks, 10 pads, 8 keys, 6 bells and mallets, 3 strings, 3 brass, 5 textures that move and 8 effects a song is built with (two risers, a fall, an impact, a sub drop, a sweep, a siren, a laser). Each is a sound: it brings the effects it wants as devices behind the synth, and says what it is for, which keys it is for and what its macros do. |
| `builtin:drumkit` | Synthesized drum machine on the General MIDI map, with 808, 909, tight, trap and lo-fi kits. Each drum is shaped after measurements of recorded ones: where its energy sits and how long it rings. |
| `builtin:sampler` | Plays one audio file across the keyboard: loops with crossfade, reverse, one-shot, amp and filter envelopes, a pitch wheel whose range can be set. |
| `builtin:drumrack` | Sixteen pads on the General MIDI drum notes, each playing its own audio file, with level, pan, tune, decay and choke groups. |
| `builtin:sampled` | Plays a sampled instrument: a piano, a bass, a string section, a drum kit made of recordings, as an SFZ file describes it. Layers of loudness that fade into one another, several recordings of a note taken in turn or at random, loops, release sounds, the pedal, ways of playing chosen by a key and asked for by name. Its recordings stay on the disk until they are played. It comes with no sounds: it plays the instruments you have. |

The wavetable synth is built to be shaped by someone who cannot hear it, which is what an assistant is. Every
part of it is a parameter whose name says what it does and in what unit; nothing is routed that the matrix does
not say (`mod1_source` x `mod1_amount` -> `mod1_dest`); a preset says in a sentence what it sounds like, what it
is for and what its macros do, and `search_presets` finds it by those words. A table is data too: a recipe is a
few key frames, each a named shape with its numbers (`{"shape": "pulse", "width": 0.3}`) or a list of harmonics,
and between two key frames of the same shape the numbers move, so two pulses make a table that sweeps the width.
The tables that ship are such recipes, to read and to change, and a table is shown as what it is frame by frame
(how bright, how hollow, how loud) in place of being heard. It needs no file and no loading, and the same song is
the same samples on every render, at any buffer size and on any number of threads: what a note draws by lot is
drawn from the note. Measured (and held in tests): what is not on a harmonic of a sawtooth lies 84 dB under it
at the worst pitch and 110 to 129 dB under it in the top octave of a piano at 48 kHz; a voice is as loud as the
poly synth's at the same volume; eight notes of eight copies on both oscillators take 4 % of one core, and thirty-two
notes of sixteen a quarter of one. It has no
effects inside: a preset that wants an echo or a room is a sound (see Sounds, below) and brings the delay, the
chorus or the reverb as devices behind the synth, to be seen, changed and taken away like any other. Its low-pass filters
beside Surge XT's at 1 kHz (a measurement, on demand): without resonance the 12 dB ones are 3 dB down at the same
place (687 and 688 Hz) and fall 12.0 and 11.2 dB an octave from 2 to 8 kHz, the 24 dB ones 24.7 and 22.3; at half
the resonance's travel this synth's 12 dB filter peaks 9.9 dB over its passband and Surge's 6.2, the 24 dB ones
7.2 and 11.3; at nine tenths Surge's is all but ringing by itself (36 dB and more) where this one's stands 19 to
22 dB. In the window its panel draws the wave an oscillator
reads at its position, the envelopes and the LFOs, and lists the matrix slot by slot.

Its bank is held to numbers, because whoever wrote it cannot hear: the owner has heard the first sixteen sounds,
and nobody has heard the other sixty-six. Every sound is measured through its own effects, on the notes of its
kind (`trill-engine/tests/bank.rs`):

- **Level.** A kind is at one level for the same playing: the loudest 400 ms of its phrase at -16.5 dBFS for
  sounds that hold, -18 for keys, strings and brass, -20 for plucks and bells, which are over sooner. The phrase
  peaks 4 dB under full scale at the most, and the fullest chord of the kind at full velocity stays under it.
  Ten sounds have not that room at their kind's level (a clav is over in a tenth of a second) and are named in
  the test with how far under it each is, 1.1 to 3.7 dB.
- **A bass is in the middle** under 120 Hz: what its sides do not share lies 24 dB or more under what they do.
- **Every key and every macro** (the synth's own tests): each key of a sound's range sounds and is in line
  with its neighbours, and each macro moves its sound from end to end without a gap, a click or a jump in level.
- **What it costs**: the fullest chord of the costliest sound (thirty-two voices of sixteen copies in a long
  room) takes about a sixteenth of the time it lasts, and half of them a fortieth or less.
- **Against a synth that is known to be good.** 91 of Serum 2's factory presets, up to ten of a kind, were
  measured by the same rule on the same notes (`trill-mcp/tests/bank.rs`, on demand), and of them a range for
  each kind is kept for seven numbers: level, where the weight of the spectrum lies, how much of the sound is
  under 250 Hz and over 4 kHz, width, and how fast a note begins and ends. Nothing else of them is kept, and no
  sound of theirs is rebuilt. Of the bank's 574 numbers 532 lie inside the range of their kind. The 42 that do
  not are named in the test, each with why: a sine lead has nothing over 4 kHz, a riser rises for as long as it
  is held, a stab is over when the key is let go; eight are on sounds the owner heard and passed, which were
  left as they were heard; three say plainly that the bank's brass carries less of its weight low than the
  five brass presets measured, which is a thing to listen for. What the ranges found and was put right before
  that: pads that ended sooner and were narrower than any pad measured, three bells brighter than any bell,
  mallets in one spot.
- **Found by what is asked for.** `search_presets` is how a sound is chosen, and a test holds that the words
  a model asks with ("808 sub", "warm pad", "glass bell", "reese", "supersaw lead", "riser" and thirty-five
  more) each find a fitting sound of the bank among the first five answers.

`cargo run -p trill-app --example wavetable_listen -- target/wavetable-listening` writes the bank to listen to: a
file for each kind in which every sound plays a short phrase that suits it, four short pieces made of the bank
and the built-in drum kit alone, and a list that says when each sound begins.

The sampled instrument plays instruments that are made of recordings, hundreds or thousands of them, and that
a text file describes: which recording sounds for which keys and how hard they are struck, and what is done to
it. The file format it reads is SFZ, which most free and open sampled instruments are published in; the reader
is apart from the player, so another format is another reader. An `.sfz` file is a preset of the device: give
its path as `preset` to `create_track`, `add_device` or `update_device`, drop it on a track, or pick it in the
browser, where the library lists an instrument as one thing that says what it is (its range, its layers, its
size, its ways of playing) and keeps its recordings out of searches for sounds. What the instrument says of
itself is what its track goes by: its range, whether its keys are drums (and which drum is on which, read from
the kit's own labels and the names of its recordings), and the ways of playing a key chooses, which a note asks
for by name. Nothing of that is kept in the song.

It does what instruments in that format most often ask for (the list is at the top of
`trill-dsp/src/sfz/mod.rs`): keys and velocities with a root and a tuning, levels that add up through the file's
headers, includes and definitions, recordings taken in turn and at random, loops with a crossfade (and the loop
a WAV file carries), an envelope and a filter for each region, vibrato and tremolo, release sounds, the pedal,
groups that silence one another, layers that fade into one another by velocity, by key and by a controller,
controllers read along the instrument's own curves, keyswitches, the controllers a note has of its own (how hard
it was struck, its key, a number drawn for it, every other note), numbered slow waves that move the pitch, the
level, the filter and one another, where in a recording a note begins by a controller, the sounds no key plays
and a controller sets off by coming into a span (the noise of a pedal going down and coming up: once each time,
at a place in the song like any note), and the controllers only a maker's own window moves, which stand where
the file puts them (a kit's microphones at their maker's mix; the pressure on a single key, which nothing here
moves, at rest). A set made for the ARIA engine may name its folder of recordings in a bank file beside its programs: that is read
too. What a file asks for that it does not do
is not passed over: loading the instrument says so in a sentence, naming each thing and what a listener
misses for it ("It asks for 1 thing this player does not do, so it will not sound quite as in its maker's
player. Not heard: equalisers of its own (eq1_gain). Everything else plays."), and the instrument plays. Of the
3,251 SFZ files of the 93 freely published sets it was tried on, 2,672 are instruments (the rest are parts that
others include), and 2,416 of those ask for nothing it does not do.
Not done: the equalizer and effect sections, envelopes with any number of stages (`egN_`), a second filter,
the shape of an envelope's curves, waves the player is asked to make (`sample=*sine`), sostenuto. A recording that is there and is not audio it can read is counted and said, in the
panel and over MCP; plain WAV files with a longer format chunk than usual, which a whole library is written
with, are read.

A song is the same samples on every render, at any buffer size and on any number of threads: recordings taken
in turn count from where playing began, and one taken at random is drawn from the key and the note's place in
the song. Measured, and held in tests on instruments the tests write themselves: every key within a hundredth
of a cent of its pitch (the worst of 39 keys was 0.00007 cents off); what a bright recording folds into the
band when it is played above its pitch lies 79 dB or more under what belongs, up to two octaves up; no click at
a loop, a release, a voice taken for another note or the crossing from one layer to the next; 128 voices, the
most it plays, take 41 % of one core.

It holds the start of every recording in memory (a third of a second) and reads the rest of one when it is
played, on threads of its own, keeping what it has read within 2 GB and letting go of what was played longest
ago. The audio thread never reads a file and never waits for one. On a concert grand of 637 recordings that is
1.9 GB on the disk as WAV files, every note could begin 0.13 seconds after loading when the system had read the
files lately, and 0.9 seconds when it had not for an hour of other work, with 94 MB held; the rest of a note's
recording arrived in 13 milliseconds for most and 55 at the worst, well inside the third of a second that was
there already. (As FLAC files, 712 MB, the same piano: 0.13 and 1.1 seconds. A disk that has never read the
files, after a restart, has not been measured.) Whole, it would be 2.6 GB in memory and six to seven seconds of
reading. Played before the start of its recording is read, a note is
silent; one whose start runs out before the rest arrives is faded out in five milliseconds; both are counted,
and `get_device` says how many. A render never has either: it reads what it plays and waits. An instrument may
name 16,384 recordings; one that names more is refused in a sentence.

The two sample players follow velocity by one law, in decibels: the volume is the level of a note at velocity 75,
and the sensitivity moves harder and softer notes from it by 0.4 dB a step at full (half of that at 0.5), so a
hard hit is louder than the volume says and a roll swells as it is written. The level of every instrument, like a
fader, a send and a Utility's gain, is silence at its lowest value (-70 dB), so a fade drawn there ends in nothing.

**Effects**

| Device | |
|---|---|
| `builtin:eq` | Six bands: a low cut and a high cut of 12, 24 or 48 dB an octave (Butterworth), two shelves with a Q, two bells. Every band reaches from 20 Hz to 20 kHz. |
| `builtin:compressor` | Two detectors: `peak` follows every sample, for a single track; `bus` holds the peaks, so that a slow attack still works on a group or the whole mix. Of a stereo sound the peak detector hears the power of the two sides together. Automatic release, automatic makeup (0.7 of what a full-scale signal would lose, as Ableton Live's), parallel mix, and a **sidechain**: any other track as its key, with a low cut and a high cut on what it hears. |
| `builtin:limiter` | Look-ahead brickwall limiter. |
| `builtin:reverb` | Algorithmic reverb that sounds from its pre-delay on, with a filter on what goes in (low cut and high cut, 12 dB an octave), a damping that shortens and quietens the highs, modulation and a stereo width. A longer decay returns more: 3 to 4 dB for each doubling. |
| `builtin:delay` | Tempo-synced echo, stereo or ping-pong: the left and the right repeat of a pair at one level, feeding back once a pair. Filters what comes in and each time round, 12 dB an octave, and returns whole what lies between the cuts. |
| `builtin:filter` | Resonant low-pass, high-pass, band-pass or notch at 12 or 24 dB an octave, with an LFO. |
| `builtin:chorus` | Stereo chorus with a width. |
| `builtin:tremolo` | Tremolo and auto-pan: sine, triangle or square, free or in time with the song, the two sides together or taking turns. |
| `builtin:pump` | Ducking locked to the grid, for when there is no kick to listen to. |
| `builtin:shifter` | Pitch shifter: any sound up or down by as much as two octaves, its length kept. Pitch and mix can be automated: a chord drawn two octaves down over a beat melts away. |
| `builtin:saturator` | Waveshaping distortion on four curves: soft, hard, tube, and a quarter of a sine that is flat above its ceiling (the overtones that make a sine bass heard on small speakers). It does not turn down what it drives. |
| `builtin:utility` | Gain down to silence, pan and width. A gain that an envelope draws is followed in straight lines, 256 frames behind the drawing: a stop is a stop. |

A compressor's sidechain is the track as it leaves its devices, ahead of its fader and its mute, so a muted track
serves as a key. It is set in the compressor's panel ("Listen to") and with `sidechain` on `add_device` and
`update_device`, and a track that is listened to is still played when only some tracks are heard.

A hosted plugin that has an input for it listens the same way: its panel has the same "Listen to", and
`sidechain` takes it. What it hears goes into the input after an effect's own (the format's sidechain), or into
an instrument's first, a vocoder's voice; what the plugin does with it is the plugin's business, and most have a
switch of their own for listening to it. A CLAP plugin's input is simply filled. A VST3 plugin's is switched on
by the host, and a plugin may take one that is switched on to be the one to listen to; so it is switched on only
for a device that listens, and the plugin is made again, from the state the song has of it, when its device is
given a track to listen to or has it taken away.

What these devices do is held to numbers by renders (`trill-engine/tests/effects.rs`): the cuts to a Butterworth
filter of their order, the compressor to what it takes off a signal of known crest, the delay to its first
repeats, the pitch shifter to the line its pitch is drawn along, and the reverb, the makeup, the steep filter
and the saturator's sine curve to what Ableton Live's were measured to do on two songs. The instruments are held
the same way (`tests/instruments.rs`): the sampler's levels to what Live's Simpler plays at five velocities, a
sine on the synth to the level of Live's Operator at the same volume setting, its pitch envelope and its noise
to what Operator's were read to be.

A song saved before a device was changed keeps the sound it had. The project file says which format it is, and
opening an older one writes into each device what that format meant: the reverb and the delay of format 1 have
their `legacy` switched on, and so have the sample players (the velocity law they had), the saturator (its level
turned down as the drive rises), the utility (a lowest gain that is 70 dB down and not silence, smoothed over a
hundredth of a second) and the synth (its noise) of formats 1 to 3; and the reverb (which began a line's length
after its pre-delay, as wide as it could be), the delay (cuts of 6 dB an octave), the compressor (a peak detector
that heard the louder side, and 0.72 for its automatic makeup) and the synth (a volume that moved in steps) of
formats up to 6 are set to the sound of those. The synth volumes of formats 1 to 3 are also
written 21 dB down the scale, where they mean what they did, and so is an envelope on one: its points, and the
level its line's travel is measured from where it follows the fader of those formats, so that it plays the curve
it played. A journal
kept before such a change is read by the same rule, so the making-of plays the song as it sounded.
`trill-engine/tests/old_songs.rs` holds a file of each format to the samples the build that wrote it rendered.

**Plugins**: installed CLAP and VST3 plugins appear beside the built-in devices as `clap:<plugin id>` and
`vst3:<class id>`. Their state is saved in the project, and their parameters are automatable and reachable over
MCP like any other device's. A plugin's own editor opens in a window (Windows only so far), from the Editor button
on its panel or at the end of its chip in the mixer, and what is changed there flows back into the project.

A plugin's presets are listed, searched and loaded like a built-in device's. They are found the way each format
provides: a CLAP plugin's through its preset-discovery providers, a VST3 plugin's as the `.vstpreset` files in the
standard preset folders and the programs of its program lists. A big synth has thousands, so they are read in the
background, in the scanner process, and remembered until the plugin or its preset folders change. Loading one is
done by the running plugin, as its own preset browser would, while the song plays; it is one undo step, and what
it comes to is saved with the project. (A program list that is only numbered slots, "Prog 1" to "Prog 128", is not
listed: there is no sound to choose by a number.)
`cargo run -p trill-plugins --example presets -- "clap:Surge XT" warm pad` shows what an installed plugin offers.

**The makers' own presets**: most sampled instruments and amplifiers tell a host of no presets at all, and keep
them as files of a format of their own for the browser in their window. For the plugins it knows by their ids,
Trill reads those files where the maker's installer put them and lists them with the rest:

| Plugin | Its presets |
|---|---|
| Serum 2 | the patches in `Documents/Xfer/Serum 2 Presets`: a JSON header and zstd-compressed CBOR |
| Ample Sound's guitars and basses | the tones in `Documents/Ample Sound/<instrument>/Presets4`: XML documents |
| Superior Drummer 3 | the factory kits of each installed sound library: one text file of them |
| Neural DSP's Archetypes | the presets in the shared `Neural DSP/<plugin>` folder: trees in JUCE's binary form |

Loading one builds the state the plugin would save with that preset in it and hands it to the running plugin.
Trill then asks the plugin for its state again and looks for the preset's settings in it, so a preset the plugin
did not take (a damaged file, a version this reader does not know, a plugin that refuses) is a sentence that names
it, and the device keeps the sound it had. The files are read in the scanner process, in bounded sizes, and nothing
is ever written to a maker's folder. Where a preset makes the plugin read samples for a while (a drum kit, a
guitar's other set of samples) the reply says so; a probe, a measurement and a render wait for it, and the
device is marked as loading meanwhile. It also says
when an Ample preset switches the instrument's automatic legato on, as nearly all of them do: notes that ring over
each other are then hammered on unasked, until the parameter "Auto Legato" is set to 0. A folder or a product that
is not there is simply no presets.

**A preset from a file, anywhere**: presets that were bought or sent lie with the samples, not where a plugin's
presets are listed from. Any place that takes a preset by name also takes the whole path of a preset file: a
`.vstpreset` for any VST3 plugin (which is also the form a plugin's state has in another host's project), a file
of the maker's own format where Trill reads it (`.SerumPreset`, an Ample instrument's preset, an Archetype's; for
a file of many, such as Superior Drummer's kits, `path#name`), a file a CLAP plugin loads itself, or a
`.trillpreset` that was saved on another machine. It goes through the same door as a preset by name: one undo
step, the plugin checked to have taken it where that can be seen, and a sentence when the file is another
plugin's, damaged or gone. In the window it is "Load preset file…" in a device's preset menu. Preset files in the
sample library's folders are found by `search_presets` by their names, each with the device it is for and its
path. The assistant in the window may be pointed at a preset file on any disk of this machine, and at none on a
network share: opening one would sign the user in to whatever machine the path names.

**Saved presets**: any device's sound can be kept under a name: a plugin's state with everything that was set in
its own window, a built-in device's every parameter. That is how a plugin whose presets are neither told to the
host nor in files that Trill reads gets presets a host can list: choose a sound in the plugin's window once, then
"Save preset…" in the preset menu of its panel, or `save_preset` over MCP. A saved
preset is listed with the device type's own, under "Saved", in every song, and is given to a device like any
other. They are small files in `saved-presets` beside the settings file, one folder per device type. What was
saved is renamed and described, and deleted: in the window from its row in the browser or from the preset menu
of a device that is on it, over MCP with `edit_preset` and `delete_preset`. A rename keeps everything the preset
holds and takes the open song's devices with it, as one undo step. Deleting asks first, because the file is not
in the undo history; the in-app assistant is refused it, since its panel cannot ask.

**Sounds**: a sound is often more than one device: a guitar and its amplifier, a synth with its chorus and
reverb. A track's instrument and every effect after it can be kept as one thing, in their order, each with all a
project keeps of it (a plugin's state, a sample player's files, every parameter): "Save sound…" in the preset
menu of the track's instrument, which asks for a name, a description and tags, or `save_sound` over MCP. A sound is a preset of its instrument that says what
follows, so it is listed, searched and given like any preset: `create_track`, `add_device` and `update_device`
take it as `preset`, and `search_presets` says what each brings. A built-in instrument's own preset may be one,
which is how the wavetable synth's pads come with their chorus and reverb. Giving one makes the instrument and
its effects in one undo step, and the reply says which devices were made. On a track that has a chain already,
the device takes the sound on in place; the effects the sound before it had brought go, and the new ones are
made directly behind the device; an effect that was added otherwise, or was given a preset of its own since,
stays, behind them. A sound that names a plugin this machine has not got, or one that does not start, is made
without it, and the reply says which. What a sound does not hold belongs to a song: the fader, the pan, sends
and groups, automation, and the track a device listens to, which saving says where the track has one. The file
is its instrument's `.trillpreset` with the state of each device beside it; a preset of one device is what it
always was. The files a sound's devices name (a sampler's sample, a table read from a file) are the song's files
once it is on a track: listed when they are missing, and relinked. In the window the browser lists the sounds
after the built-in devices: the ones that were saved, each with its instrument and how many effects come with it,
then a row for each kind of the sounds the built-in instruments bring, which opens. A double-click makes one
the sound of the track that is shown, dragging one onto a track does the same there, and dropped below the tracks
it gets a track of its own. On the installed plugins, on demand: an Ample guitar through Archetype Tim Henson,
saved in one song and given to a track of another, renders to the same samples in both.

**Reading a plugin**: a plugin's parameters are numbers from 0 to 1 whose meaning only the plugin knows, so each
is shown with the text the plugin itself displays for it ("-6.0 dB", "Dynamic 57", "On"), and can be set by that
text: where the plugin converts text to a value its answer is checked against what it then shows, and where it
does not, the value is found among what it shows. Every device says what channels it takes and gives; a device
with one input channel is handed left and right mixed, and is marked in the chain. (A plugin that takes a stereo
pair may still be mono inside: a guitar amplifier is, and widens the sound with a doubler of its own.)
`cargo run -p trill-plugins --example inspect -- "vst3:<plugin name>" mic` shows an installed plugin's channels
and the parameters with that word in their name, as the plugin shows them.

Some of what a plugin can do has no parameter: an Ample guitar's saved state is a document of some 480 named
settings, and about 250 of them (its effects, pedals, amplifier and strummer) are not among the 226 parameters it
shows a host.
Those are found by name and set through the state, as one undo step, with the values the plugin itself writes.

The same example is where reading another maker's presets begins. With `--state <folder> --files` it says what
the plugin's state is (text, a document, a container that is known, or bytes that give nothing away), writes the
state out to look at, and lists the files that look like presets in the maker's folders; `--file <path>` says
what one of those files is, in the same words. When both are the same kind of thing, a preset can be made into a
state.

**Songwriting**: chords by symbol or Roman numeral, voicings, rhythms, arpeggios, bass lines, drum grooves with
fills (and a reply that says what each fill took the place of), melodies by scale degree with their ornaments (a
bend up to a pitch and its release, a slide from the note before, a scoop up into a note, a hammer-on or pull-off,
vibrato: `5:2!bend=6!vib`), swing and humanizing, and reading a song's key and chords back from its notes
(`trill-theory`). Over MCP an assistant says `Am9 | Fmaj9 | Cmaj7 G/B | Em7`, drop-2, house groove, rather than
spelling notes. A melody's bars are the song's bars where it is written, so one phrase runs through a change of
time signature: four bars of 4/4 and then bars of 7/8, each as long as the song has it.

**A track knows what its notes mean** to its instrument: whether they are drums or pitches (a drum kit, and a
plugin that calls itself a drum instrument, are drums without being told; any track can be switched), which key
each drum is on, which keys are keyswitches and what they choose, which play a noise, where the range ends, the
octave the instrument really sounds at, how far its pitch wheel bends. It is kept in the song, on the track, and
everything reads it. A drum name lands on the kit's own key. `articulation: "palm_mute"` presses the keyswitch
just before the note, and fits the velocity where velocity decides how a note is played; a single note says how it
is played too, in a phrase (`5!palm_mute`) and in `create_clip`, `add_notes` and `set_notes`
(`[64, 0, 1, 100, "harmonic"]`), and a clip's keyswitches are written for all its notes together, so a plain note
is picked the ordinary way and never plays in whatever was left chosen. A harmonic is written on the key under the
pitch that was asked for. An ornament is written as the instrument's own keyswitch where it has one for it, and as
a pitch-bend curve scaled to its bend range where it has not. Reading a song back leaves keyswitches and drums out
of the harmony and reads a bass at the octave it sounds. A part that lands outside the range, or on a keyswitch,
says so when it is written.

**A kit says what is on its keys.** Two kits of one drum plugin do not keep their drums on the same keys: of the
203 kits that came with Superior Drummer 3 on the machine this was written on, there are 37 different tables.
Where a plugin's state says what hangs in each place and which keys strike it (Superior Drummer 3's does;
`trill-plugins/src/vendor/superior.rs` reads it, with the few words the library has for its drums), the track's
drum map is the kit that is loaded: read from the plugin whenever it is asked for, so it follows a preset, the
plugin's own window and a song that was opened, and nothing of it is kept in the song. `china` is the china of
this kit, a second crash is `crash2`, a stroke the names have no word for is there under the maker's own
(`snare_rim_only`, `ride_bell_tip`), and `get_instrument_profile` lists them all with what each drum is. A drum the
kit has not got is left out and said (`left_out`, with the names the kit does have), in place of a hit on whatever
is on the key the name usually means. An instrument that says nothing of its keys is written for as before: the
track's own map, then General MIDI.

What is found out about a plugin is remembered for that plugin on this machine (`instruments`, beside the settings
file) and given to the next track that plays it. Trill ships what was measured of a few: Ample Guitar PF, Metal
Eclipse, Metal Hellrazer and Metal Ray5, Superior Drummer 3, and the SWAM winds and strings. A song opened from a
file takes what is known where its tracks say nothing themselves. A probe says whether the instrument it played
is known already, and names keys and ranges in the words `set_instrument_profile` takes, so that what one probe
found is written down in one call.

**Sounds**: a library of the sample folders you point it at, scanned in the background and searchable by words,
tags, tempo, key and length (all read from file and folder names). Audio clips play WAV, AIFF, FLAC, MP3 and Ogg
Vorbis at any sample rate, and can be warped to follow the project tempo: `beats` keeps drum hits sharp, `tonal`
keeps sustained sound smooth, `repitch` plays faster or slower like a record; warped clips can be transposed.

**Listening to a recording.** Trill says what is in an audio file, with no model and nothing changed: the
**beat grid** (every beat and bar line, each beat moved onto the hit it belongs to, and the tempo), how far the
recording sits from A = 440, its **key** with the runners-up, a **chord** for each stretch of beats, its **parts**
lettered so that parts that return share a letter (`A B A B C B`), the **loops** worth having ranked by how well
they come back round, the cleanest **single hits** of each kind of drum and a bass note for each pitch, the drum
**groove**, and the **notes** of a bass line and of a melody. Any of the last four is written as **MIDI on the
recording's own beats**, so it stays in time where the recording's tempo wanders: into a file, or into the song on
a new track. When the tracker has the beat wrong it is reread: half or double the tempo, the bar one beat later,
or so many beats to a bar; everything else follows.

- It works on one file and better on one instrument. A drum loop's groove, a bass line's notes and a voice's
  melody are right; of a whole mix the groove is a sketch (the bass plays in the kick's band), and bass and melody
  are what was loudest in their registers. Chords and parts are estimates: a map, not a transcription. Without a
  model the start of the bar is the least sure thing in an answer.
- Over MCP today (`listen`, `transcribe`); the window does not show it yet. Of a whole mix, a *study* (below)
  hears the same things in its stems, where they are right.
- The method is the one the owner's Magpie project proved, translated from its Python (`crates/trill-listen`). On
  five recordings Magpie had taken apart, over its own beat grids, Trill finds the same parts, 313 of its 316
  loops to the bar, 187 of its 189 one-shots, its groove and every bass and vocal note; chords are the same 52 to
  84 % of the time (11 % on one recording whose chords Magpie has in another key), read from a chroma of Trill's
  own.

**Studying a recording.** Trill takes a recording apart with models that run on your own machine, and keeps what
it found as a *study*: the **stems** (vocals, drums, bass and the rest, which add up to the recording), the
**beats and bar lines** as a model hears them, the **words** that are sung, in sung lines, with a time and a note
for every word, and then everything listening finds (key, chords, parts, loops, single hits, the groove, the bass
line, the melody), heard in the stems, where it is right of a whole mix too. A study is made once and kept:
reading it again runs nothing. A stem is an audio file like any other, and goes on a track.

- **The models are not part of Trill, and Trill hosts none of them.** File, Setup lists each part there is (the
  runtime, what drives a graphics card, each model) with its size, its licence in the words of its source and
  where it comes from, and fetches a part when you press the button under those: with the system's own `curl`,
  from where its makers publish it, into a file that is kept only if its checksum is the one Trill was made for.
  A fetch can be stopped and is gone on with from where it was. Nothing is fetched unasked, by a test or for a
  picture, and the assistant in the window cannot fetch at all. The stem model's licence is unclear, in its
  author's own words, and the panel says so before you fetch it. Everything a study uses is 3.1 GB for a
  graphics card and 2.6 GB for a processor; the four models of the best stems are 643 MB more.
- **What runs the models is measured, not assumed.** A timed test splits the same half minute of noise on the
  processor and on every graphics card, each in a process of its own, and takes the fastest that gave the same
  stems. On the machine this was written on (a Ryzen 7 7800X3D, an RTX 4080 SUPER and the processor's own
  graphics) a minute of music is split in 19 s by the processor, in 7 s by the 4080 through the part that drives
  a card of any make, in 3 s through NVIDIA's own libraries (an optional part of a gigabyte, under NVIDIA's
  licence), and in 69 s by the integrated card, which is therefore not used though it is there. Until the test
  has run, a study runs on the processor.
- **A study runs in a process of its own**, as a render of a song with plugins does: a model's runtime that
  faults costs a sentence that names the stage, and every stage that had finished is kept; a study the machine
  has not the memory for is refused before it starts, and one that takes more than it was given is ended. It can
  be stopped at any moment and is taken up at the stage it was in. A song of four minutes and a half took 30 to
  37 s in all on the 4080 through NVIDIA's libraries (5.5 GB of memory at the most), 50 s through the part for
  any card (6.1 GB) and 142 s on the processor alone (3.1 GB). On the integrated card the same study took
  eleven minutes. A recording of fifteen minutes, the longest that is studied, took 101 s and 8.7 GB through
  NVIDIA's libraries, 152 s and 7.3 GB through the part for any card, and eight and a half minutes and 7.0 GB
  on the processor. A run says beforehand what it will need, by what runs the models and by how long the
  recording is, and that is what it is refused or ended by.
- **Where the words are written down is where a model hears a voice.** A transcriber makes words of whatever it
  is handed, and a vocal stem is not silent where nobody sings. So a small model that tells a voice from
  everything else cuts the stem first (as Magpie's transcriber does), and a word with nothing sung under it is
  left out. Without that cut, on two of four recordings the transcriber wrote down twice the words that are
  there, one phrase thirty times over.
- **Against what Magpie found** in the five recordings it took apart, on this machine. *Stems:* as near to the
  ones Magpie kept as Magpie's own second run is (its model shifts the recording by a random amount, so two of
  its runs differ): 27 dB for the vocals of one recording where Magpie against itself has 26 to 27, 19 for the
  drums where it has 18, 24 for the bass (22 to 23) and 21 for the rest (20 to 21). *Beats:* on the two
  recordings Magpie tracked with the same model, the same beats and bar lines (0.998 and 1.000 of them, where
  listening without a model has 0.81 and 0.96 of the beats and 0.63 and 0.92 of the bar lines). *Words:* Magpie
  uses a larger transcriber; on the recording both hear best, 276 of its 285 words are the same and begin
  about 20 ms from where Magpie has them at the median, nine in ten within a fifth of a second; on the three
  others 83, 86 and 68 % of its words are the same, and the last of them, which is sung in two languages, came
  out with 326 words on one run and 359 on the next. Neither writes a word for a recording nobody sings in.
- **A recording behind a link** is fetched by a program you installed (`yt-dlp`, which needs `ffmpeg`), found
  when it is wanted and never shipped, into the study's own folder. It is yours to say what you may study: the
  tool says so once, and the assistant in the window is not handed links at all.
- **A study is a folder** in Trill's own data folder, which File, Setup moves to a disk with more room, as it
  does what was fetched: about 60 MB for a minute of music, nearly all of it the four stems, which are kept as
  24-bit WAV. They are not packed: the one small FLAC encoder there is in Rust made 24-bit stems larger than
  they were. Every answer about a study and the Setup panel say what the studies take.
- **One study at a time on the machine**, whichever program asks: the window and a headless server take
  turns over one file, and the one that waits says so.

**A study in the window.** The Studies place of the browser lists every study, the ones being made with the
stage they are at, a bar that fills, Stop and "take up again"; a failure is a sentence under the study's name.
*Study a recording* (there, and in the File menu) asks for a file or a link in one field, and says whose a
recording behind a link is before anything is fetched. A study that is done opens in the **Study tab** along the
bottom: the whole recording small with its parts, then under the recording's own bars its parts, its chords and
its words where they are sung, and its stems as lanes with mute and solo. Beside the lanes is what was found:
loops best first (with whether a part plays alone there), single hits as pads, the sung lines, and the bass
line, the melody, the chords and the groove as notes.

- **A stretch is selected across the lanes**, its ends on bar lines, beats or wherever (Snap), and *played
  round*: made by the plan that taking it follows and rendered by the code that plays the song, so what is
  heard is what would be taken. It is heard in the song's tempo, in time with the song while the song plays,
  and, where asked, in the key the song says it is in; *As recorded* hears and takes it untouched.
- **It goes to the song** by being dragged onto a lane (or below the tracks for a new one), or with *To the
  song* or Enter onto the selected track at the edit cursor. A loop, a line (named by its words) and a part as
  notes are dragged from their lists the same way; a hit goes onto a lane, a pad of a drum rack or a sampler.
  One undo step each.
- **A window of its own** (the Window switch) shows the same workspace beside the song or on another screen,
  and a drag crosses from it into the song: the window the button went down in keeps the pointer, and Trill
  works out where that is in the other from where the two stand on the desktop. This is proved without a real
  mouse (the test runs the two as one, and the arithmetic is tested by itself): if it misbehaves on a machine,
  *To the song* and Enter do the same.
- **What a stretch becomes** is an audio clip on the stem's own file that follows the song's tempo, a beat of
  the recording for a beat of the song: transients kept for drums, sustained sound kept smooth for everything
  else. Several stems together are added up into one file in the study's folder. A recording whose tempo moves
  is laid in as many clips as keep every bar line within 12 ms, cut at bar lines; taking the study's tempo
  into the song first keeps it in one, untouched. Moving it to another key is the stretcher's work: a voice's
  formants move with the pitch, so a couple of semitones pass and beyond three or four a voice no longer sounds
  like itself. Drums are never moved for a key. A clip is not yet a loop that repeats when it is drawn out:
  drawn out it shows more of the recording; `times` (over MCP) lays a loop end to end.
- **The grid can be read another way** (half, double, the bar a beat later, three, four or six beats to a
  bar): what follows from it is heard again without a model. **A word can be put right** by double-clicking it
  where it is sung; its time stays.
- **As a whole** a study gives the song its tempo (bar by bar through the tempo map, a steady recording as one
  tempo, a bar the model found shorter as a short bar with its own time signature; nothing in the song moves),
  its parts as markers, its key, a drum rack of its cleanest hit of each drum, and the recording itself as a
  **reference**.
- **A reference track** is a recording beside the song: silent unless it is listened to (the L in its header,
  where other tracks have mute and solo), heard in place of the song while it is, and in no render, stem, film
  or length of the song, whatever is being listened to. Its study's parts, chords and words are drawn along its
  clip. Any audio track can be made one from its menu. Measuring with only that track named measures the
  recording itself, which is how a mix is held against it.
- **A song says its key** (`set_project`, or taken from a study): nothing follows it, and it is what a sound
  from a study is moved to.
- **A study's files are the song's files.** A clip taken from a study plays a file in Trill's own folder, so it
  is listed, found missing and relinked like any other. Removing a study, or dropping its stems to make room
  (the Studies place, Setup, or the `study` tool), first says which of the songs Trill saved or opened play
  them, and asks. *Gather what the song plays of studies* (File menu, or `study` with action "gather") cuts
  what the open song uses, with a second to spare on each side, into a folder beside it and has the song play
  it from there: for a song that goes to another machine. Trill has no general "collect all and save" yet.
- Everything above is one tool for an assistant, `take_from_study`, over the same code the window uses.

**Files that moved**: a song names its audio files by their paths, and a folder of samples that is moved or
renamed leaves it with clips and pads that play nothing. Trill says so in one place (a line in the status bar and
the list it opens, `missing_files` in `get_project`, and first in the notes of a `measure` or a `render`, which
is then the song without those files and not a quieter mix), and looks for the files itself:

- *Where the folder went, then by name.* If one file that was at `A\x\y.wav` turns up at `B\x\y.wav`, everything
  else that was under `A` is looked for under `B`. Where a folder went is read from any file found by its name in
  the sound library, from a folder of the same name near where the missing one was, from a folder of the library
  that is itself missing and turns up with its files in it, and from what you point at.
- *A file of the same name can be another file.* Each thing found says how sure it is, and why: `sure` only where
  it is where the file's folder went or has the size and length the song wrote down, `likely` for the only file of
  that name with nothing against it, `unsure` when there are several, when it is not what the song knew, or when
  it is shorter than a clip plays. Only what is sure is taken by the one action; the rest is offered, and taken a
  file at a time.
- *Relinking is one edit*, one undo step however many clips and pads it touches (`Project::relink`,
  `relink_files`): each clip goes on playing the stretch of its file it played, each pad keeps its settings.
- *The library's own folders.* One that is gone is marked in the browser and listed (`library` status,
  `missing_folders`) in place of being silently stale; saying where it went (`move_folder`) keeps the favorites
  and the recently used files with their files and reads nothing again, and it goes along with the song's files
  when the same move explains both.
- *A song that travels.* A project file also says where each of its files is from the song's own folder, and
  what the file was (size, length, sample rate), in a `files` table beside the song. When a song is opened, **the
  file beside the song wins**: one that is found where the table leads from the folder the song is in now, and
  that is the file the song knew, is the one it plays; otherwise the path stands as it is. So a folder holding a
  song and its samples can be moved or sent whole. The table is an addition to the file, not a new format: a
  build that does not know it reads the song as before, and a file without one means what it always meant.

**Engine**: sample-accurate MIDI and audio clips, clip looping, non-destructive trimming and splitting, audio
fades, automation of track and master volume, pan, sends and any device parameter with straight, held or curved
segments (see below), MIDI controllers in clips (see below), group tracks and return tracks with sends, a metronome, sample-exact transport looping, mute/solo, undo/redo, offline rendering that is bit-identical to
playback, and render analysis (see below).

**A line of automation follows its control.** Between two points of an envelope the value moves as if a hand
moved the control at an even speed, which is not the same as so many decibels or hertz a beat:

| What is moved | How a line travels | Half way from → to |
|---|---|---|
| A track's or the master's volume, a send, an instrument's volume | as a fader of Ableton Live's does: even in decibels through the top 24 dB of its travel, and ever faster below (a stretch that would have been y dB takes y + y²/8), to silence at the bottom | 0 dB → −60 dB: **−16.3 dB** (a send: −18.6 dB); +6 dB → −60 dB: −13.3 dB |
| A gain that reaches silence (a Utility's) | as a gain of Live's does: even in decibels down to −17.5 dB, and below that the gain itself falls as the square of what is left of the travel | 0 dB → −60 dB: **−16.7 dB** |
| A frequency or a time (a range of a hundred times its lowest value or more) | by ratio: every octave takes the same time | 300 Hz → 9 kHz: **1,643 Hz** |
| Pan, mix and other amounts, an EQ's gain, a threshold, a plugin's parameters | evenly in the value | −12 → +12: 0 |

So two points are a fade or a sweep. Straight in decibels, a fade from 0 dB to −60 dB is at −30 dB half way and as
good as over; straight in hertz, a sweep from 300 Hz to 9 kHz is at 4,650 Hz and spends its time in the top
octaves. The law is one piece of code (`trill-core/src/travel.rs`): the window's faders and knobs are laid out by
it, so such a line is a straight line in its lane, and the engine plays it. `shape` on a point bends a line
further, and an envelope can be told to go straight in its units instead (`travel` in `set_automation`; the
lane's menu in the window).

*A level travels as the same control does in Ableton Live*, so that a fade of two points is the fade it would be
there. The two laws are fitted to what Live shows at 257 places along a Utility's gain, a mixer's send and
Operator's volume: the gain's is within 0.1 dB of Live's along a line, the send's within 0.01 dB, and a volume's
within the whole decibel Live shows one to. From 0 dB to −60 dB, a quarter, a half and three quarters of the way:

| | Trill | Live shows |
|---|---|---|
| A Utility's gain | −8.4, −16.7, −27.4 dB | −8.4, −16.8, −27.5 dB |
| A send | −9.3, −18.6, −30.1 dB | −9.3, −18.6, −30.1 dB |
| A track's or an instrument's volume | −8.2, −16.3, −30.0 dB | −8, −16, −30 dB (Operator's) |

A send's fader and a volume's are the same law, with its knee 23.8 dB under the top of the control (0 dB for a
send in Live, +6 dB for a volume); a gain's is another, and in Live's own figures the two are 2.6 dB apart three
quarters of the way down such a fade and 5 dB apart near its end, which is why they are two. A volume's fader in the window is laid out by it too: 0 dB at 85% of its travel, as on
Live's, −12 dB at 55% and −48 dB at 10%. `trill-engine/tests/automation.rs` renders three fades of two points
and holds every beat of them to what Live shows.

*A song saved before this plays as it did.* An envelope keeps what its lines are straight along, and a project
file written before envelopes did (format version 1 or 2) says nothing: its envelopes are straight in their units, as
they were played then, and stay so when their points are edited. They become a fader's or a knob's only when
they are written anew over MCP, or by the lane's menu. A file saved since is format version 3 or later, which an
earlier build refuses to open instead of playing its fades as other curves.

A file of format 3, 4 or 5 holds levels that follow the fader of its day: even in the square of the decibels above
silence, which is at −20 dB half way from 0 dB to −60 dB and from there as much as 7 dB under Live's. Such an
envelope is read as that law, which is kept under the name "square", and plays as it did, to the sample; the
lane's menu says that its lines are as the song saved them. It becomes a fader's of now when it is written anew
over MCP or told to follow its control (`travel` "control"; "Lines follow the fader" in the lane's menu). A file
saved now is format version 6.

*A send can be automated* like a volume: its envelope is the send, at the bottom of which nothing is sent, with or
without a level set by hand. A send opened for single notes (a delay throw) needs no return of its own.

*Envelopes are played to the frame.* A fader, a pan, a send and the master's volume are read from their envelopes
every 32 frames of the song and move in a straight line between two readings; a device's parameter is moved
every 256. Both are counted from where the song last started or jumped, not from the start of a buffer, so the
same song is the same samples at any buffer size and in a render. What no envelope accounts for (a fader moved
by hand, a mute, the song jumping to where an envelope is at another level) is let go of evenly over ten
milliseconds. (The built-in filter still reads its cutoff every sixteen frames of the block it is handed: with a
swept cutoff it is the same samples only at buffer sizes that are a multiple of sixteen.)

**A tempo and a time signature that change inside the song.** A song begins at a tempo and in a time signature,
and may change either along the way. A change of tempo is a jump at a beat, or a glide that arrives there from
the change before it (an accelerando, a ritardando); one beat can hold both, a glide that arrives and a tempo
that takes over (a ritardando, and then the old tempo again). A change of time signature begins a bar where it is
set. Nothing in the song moves when either is set: positions are beats, and beats stay where they are. What
changes is how long a beat lasts and which bar it is in, and two maps made from the project answer those two
questions for everyone (`Project::tempo_map`, `Project::meter_map`, in `trill-core/src/tempo.rs`), in closed form
through jumps and glides alike.

Everything that is counted in beats follows: notes, controller curves, envelopes, markers and the loop; the
metronome, which counts the time signature it is in, and the count-in, which is a bar of the meter the take
starts in at the tempo it has there; a take's clips, which are cut to the song's bars; a render, which is as long
as the tempo makes it, and its sections by bar. Audio that keeps its own speed is laid along the song's time and
ignores the tempo; audio that follows the tempo (warp) follows every change of it, as one stretched piece for
each tempo it lies under, faded across the seams over three milliseconds. A plugin is told the tempo and the time
signature as they pass, block by block, and a block ends where either changes. A MIDI file is read and written
with every tempo and time signature in it (a glide goes out as eight jumps to the beat that keep its time), and
time that is cut out of the song, repeated or opened up takes its changes with it.

*How exact.* The audio thread moves through the song evenly within a stretch of one tempo; a stretch ends at the
first frame at or past a jump, and a glide is taken in steps of 256 frames, each on the beat at both ends. A note
sounds within a frame of where the map has its beat, whatever the buffer size, and a render is what was played
(`trill-engine/tests/tempo.rs`). A song of one tempo is one stretch, and every number in it is what it was: such
a song renders to the samples it rendered to before.

*An older file is one tempo and one meter.* Changes are saved since format version 5. A file saved before says
nothing of them and has none; nothing is written into it.

In the window the foot of the ruler says where the tempo or the time signature changes, beside the bar's number
("7/8 · 140 BPM", an arrow before a tempo the song glides to). A double-click types another value, a drag moves
the change along the song, and the right-click menu sets one where it was clicked, removes one, or makes a tempo
a glide. The grid, snapping, the bar numbers and the position are counted through the changes, the piano roll
counts a clip's bars as the song has them under it, and the tempo and the time signature in the top bar are
those the song has where it is.

**Hearing without ears.** Whoever cannot listen (a script, an assistant) measures instead, and a few things make
that quick and true:

- *A render waits for its plugins.* A sampler goes on loading after it is created (a drum kit one drum after
  another, for as much as a minute, the cymbals last), a modelled instrument goes on waking up, and an amplifier
  may pass nothing for a moment; a render that started at once lost the beginning of the song. Plugins are told
  they are being rendered offline (VST3 process mode, CLAP `render`). Before the first sample each plugin
  instrument is rehearsed on the notes it is about to play, each note by itself (a tom that rings would hide a
  cymbal that has not arrived), and each plugin effect on bursts of noise, until every note has answered and
  nothing more has arrived for a quarter of a second or more, with the program not growing meanwhile: while
  samples are being read its memory rises. Every rehearsed note is sounded exactly three times and then left
  alone, however long the wait lasts: an instrument that plays its recordings of a note in turn then begins
  every render at the same one, and the same song renders to the same samples again. A key that the track
  knows for a keyswitch is not asked to sound: it is pressed with the notes the part plays under it, each of
  which is rehearsed so (a pitch played under two switches is two notes, with recordings of their own), and
  where a part begins before it has chosen anything the instrument is left on the way of playing its track
  calls the default. An instrument that listens to another track (a vocoder, a synthesizer on a patch that
  plays what it is fed) is lent noise to listen to while its notes are tried, since by itself it would answer
  none. What a plugin has been run by the time a render begins is counted the
  same way, because what some give depends on it to the block (an amplifier was measured whose sound is
  another after one block more of silence, whatever a reset does): an effect is tried until it has passed
  three bursts and no more, the effects after an instrument are run only on the notes the instrument itself
  sounds, and once nothing is left to ask nothing is run. A sampler was watched to pause for seconds between
  two drums with
  nothing to show for it, so an instrument that answers some of its notes and not others is held to that for
  five seconds before the silent ones are taken to be keys that play nothing; that is remembered of the plugin,
  and the reply names the notes, because a key that plays nothing and a sample that is still loading sound the
  same. Built-in devices are not touched, so their renders are the samples they always were.
  `Session::readiness_job` asks the same of one device. The plugins that are heard live are not waited for:
  `Session::loading` says which of them were made or given another sound lately while the program still shows
  the signs of loading. The window writes "Loading" on their panels, and `get_project`, `get_device` and
  `transport` tell a model, so that neither presses play into half a kit unawares.
  `cargo run -p trill-plugins --example loading -- "vst3:<class id>" --preset "<kit>"` watches an installed
  sampler load, key by key, beside the program's memory, reads and processor time.
- *A render does not go on without an instrument.* If a plugin never answers any of the notes it is to play
  for as long as it is waited for, the render is refused before its first sample: nothing is measured, no file
  is written, and the refusal names the track, says what its notes say of why (their range, or that they are
  keyswitches) and what to do. A mix without one of its instruments is not the song, and a line in a reply
  that otherwise looks like a success is easily missed. A track that is muted is left out on purpose, and a
  silence the track itself explains (its notes are keyswitches of its instrument, or outside its range) is
  no refusal. While the plugins' own thread is behind with other work (a song's plugins being let go of, each
  taking its time), a plugin that has not had its turn to load is waited for and not given up on.
- *A render measures everything as it goes*, without keeping a stem per track: the mix (peak, true peak, LUFS,
  stereo correlation, spectral balance), every track after its fader in the same terms, the loudness of every
  section between the markers (or of bar ranges), and how many dB each built-in compressor and limiter took
  off, at the most and typically. It can be made without writing a file, and with only some tracks heard,
  the song's mute and solo left alone.
- *A render uses the machine.* While a song plays one thread makes every block, a track at a time. A render
  hands the tracks that do not feed one another to several threads (half as many as the machine has
  processors, eight at the most, unless `threads` or `TRILL_RENDER_THREADS` in the environment says another
  number), and then adds them up, by one thread, in the order one thread would have: the mix, every stem and
  every number measured are the same samples and the same figures as on one, sooner (the reply says how many
  threads it was). It makes no more devices for it than it does on one: a plugin of the song is there once. A track waits for what feeds it: a group for its members, a return for the
  tracks that send to it, a track for the one its compressor listens to. Plugins take more care. The
  instances of one plugin share its module, and some modules keep things for all their instances: run at
  once, the instruments of Ample Sound give other samples than run one after the other. So the instances
  of one plugin are run in turn and in the order of the song, beside everything else, unless the plugin
  was measured to give the same samples either way. Only those devices take turns: what comes after them
  on a track (the amplifier behind a sampled guitar, which costs many times what the guitar does) goes on
  beside the other tracks as soon as its track has had its turn. (`trill-plugins/src/beside.rs` lists
  what was measured, and
  `cargo run -p trill-plugins --example beside` measures an installed plugin, or all of them, each in a
  process of its own). A plugin that is listed as one not to run beside anything leaves the render on one
  thread, and the reply says which it was.
- *A song's plugins are rendered in a process of their own.* A render, a measurement and a probe of a song
  with plugins are done by the program started a second time (`trill` and `trill-mcp` start themselves; nothing
  else is installed or looked for). It is handed the song with every plugin's state, the options, where the
  plugins' files are and what is known of them, and nothing else of the user's; it makes its own copies of the
  plugins and hands back the figures, the files and what it has to say. The song that is open, its history and
  what plays live stay where they are. Every job has a process that has just started, which ends where it
  stands when the job is done, with no plugin taken down (taking plugins down is where they fault most).
  - *A plugin that crashes costs a sentence, not the program*: at what stage it happened, which plugin where
    that can be told (the device that was being made, or the one whose file the fault was in), that the song is
    untouched, and what to do next. The window says it where it says that an export failed, the tools as the
    call's error, and the film export in place of its film. A film that is stopped while its sound is being
    rendered stops the render there and then.
  - *The same song is the same samples.* An instrument that draws its recordings at random begins at the same
    place in every process that has just started: the four instruments of Ample Sound, as they come, rendered
    to the same samples to the bit each time (in a program that rendered for itself, beside the song's own
    instruments, two renders of the bass left -33 dB of one another). A change to the song still moves every
    draw after it, which is what `repeatable` is for (below).
  - *It gives back all it took.* A song of seventeen plugin instruments held 8.0 GB open, its render another
    8.6 GB in the process that made it, and nothing two seconds after the answer. A job the machine has no room
    for is refused in a sentence before it is started.
  - *It can be stopped at once*: the process is ended, and nothing half-written is left under a name that was
    asked for (files are written under other names and given theirs when they are whole).
  - *It costs seconds.* A song of eight plugins was measured in 45.5 s in a process of its own and in 42.3 s in
    the program; the song of seventeen was rendered with every instrument's stem in 116 to 121 s and in 131 s.
    The two processes together hold a seventh to a fifth more at their fullest than the program alone did (6.0
    GB against 5.0 GB; 16.6 GB against 14.5 GB).
  A song without plugins is rendered where it is (nothing in it can fault, and it is the same samples either
  way), and decoded audio is not handed over: the process reads and stretches a song's files itself.
  `TRILL_JOBS=here` has a program do every job itself; `TRILL_JOB_LOG=trace` has the process say every try of
  its waits in the program's log.
- *A render can be asked to repeat itself.* `repeatable` on `measure` and `render` takes what is known to
  draw at random out of the render's own copies of the instruments (Ample Sound's: their recordings in turn,
  no noise of the fretting hand), and leaves the song as it is: two such renders are the same samples and can
  be compared, where a change to the song would otherwise show in every note after it. Without it the reply
  names the instruments that draw, and says what a change will do. An instrument that is already set so in the
  song is not named. Superior Drummer 3 is named for what nothing was found to stop: it chooses for itself,
  anew every time it is made, which recording a cymbal plays when it is struck again while it still rings
  (`trill-plugins/src/repeats.rs` has what was measured and what was ruled out, and
  `cargo run -p trill-plugins --example takes` makes one part of a song again with one thing changed).
- *A song begins in its middle as it would have it there.* What a part set before the place where the song
  starts is in force there, for what an instrument is told: its controllers, wheel and pedals (said afresh at
  every start, jump and turn of the loop), and the keyswitch it chose. The key that stays chosen is pressed
  again, and after it one that was pressed for a note which has not begun; a key the part is still holding is
  held to where the part lets go of it. Some instruments take such a key only if it comes before its note
  (Ample Sound's), so a render begins to play 0.06 beats ahead of its start where one is chosen and keeps
  nothing of them, and a song that plays presses the key when it is put somewhere while it stands still, not
  again with the first note; a loop that comes round to the way of playing it ends in presses nothing. Put
  back before its part chose anything, a song presses the key its track calls the default.
  - *Notes that are already sounding there.* A song that plays does not begin them: a sampler's long recording
    begun in its middle would be the wrong sound, and whoever plays can start a bar earlier. A render does
    what they need: it begins to play where the earliest of them began (32 beats before its start at the
    most) and keeps and measures nothing until its start, so that a pad is in its sustain, a sampler is where
    in its recording the song has it, and what rings over from those notes is there. A note that began
    earlier still is left out, and the reply says so.
  - *A stretch rendered alone lies on the frames it has in the whole song.* A render starts on a frame of the
    song as it is counted from its beginning. For an instrument that does the same thing whenever it is
    told the same, a stretch alone is then that stretch of a render of the whole song, to the sample. On the
    installed guitars of Ample Sound, with a palm mute chosen before the stretch and their recordings in turn
    (`repeatable`), it leaves -85 to -96 dB of the song's, whatever the part played before. Their bass goes
    through its recordings and strokes by a count of every note it is played, four to a round: its stretch
    is the song's (-134 dB) where it has played as many notes by then in both renders, counted in fours, and
    the same notes on other recordings where it has not. What only rings over from notes that ended before
    the start is not in a stretch, and envelopes are read on a grid that begins where the render does.
- *A render makes only what is heard.* A track that is muted, or left out by a solo or by `tracks`, is not put
  into the render at all: its devices are not created and its plugins are not waited for. Hearing one track of
  a song with many plugins costs what that track costs. (What plays live keeps every track running, so that a
  track is there the moment it is unmuted.) What feeds a track that is heard is made with it: the members of a
  group, and the tracks whose sends reach a soloed return.
- *A return can be heard alone.* Soloing a return, in the window or by naming it in `tracks`, gives the return
  as the whole song feeds it: every track's send still reaches it, a group's too, while the tracks themselves
  and the other returns are not heard. (Soloing a track or a group keeps the returns open, so that it is heard
  with its reverb.) Each of these opens and closes over ten milliseconds, whatever the buffer size, so a solo
  changed while the song plays makes no click; a send that an envelope is moving goes on moving meanwhile, and
  what is heard is the two multiplied.
- *A render runs at the rate it is asked for.* `measure` and `render` take `sample_rate`; left out, they run at
  the engine's rate, which is 48 kHz when there is no audio device. The reply says which it was.
- *One render writes every stem.* Asked to, a render writes parts of the song to files of their own as it
  goes (`render` with `stems`; "Export WAV and stems" in the window): every track as it leaves its fader and
  pan, what goes into each track's effects (its instrument alone), every group, every return fed by every
  send, and the mix as it reaches the master's devices. Each is taken where it goes by, written a block at a
  time as 32-bit float (a part may go over full scale before the master brings it down), cut to the length of
  the mix and measured as the mix is; none of them is held in memory. A track's file is what a project holding
  that track alone renders to, sample for sample, so one render stands for as many as the song has tracks.
- *A probe plays one instrument a list of notes* and measures each: whether it sounds, how loud, the pitch that
  is heard, how soon it starts, how long it rings, how bright it is. One call gives an instrument's range and
  the octave it really sounds at, a drum plugin's map (which notes are a low thump, a noisy mid hit, a long
  bright wash), or which keys switch the articulation and how. It plays a copy of a track's instrument as it is
  now, or an instrument that is not in the song yet; nothing in the song changes and nothing is written.

**Expression, wheels and pedals belong to the clip.** A MIDI clip holds a curve for any controller by its number,
for the pitch wheel and for channel pressure, with the straight, held and curved segments automation has. The
curves loop, trim, split, move and duplicate with the notes, are undone with them, and are read from and written
to MIDI files. They are played smoothly and in time, at the right value wherever playback starts, seeks or wraps,
and a render is the same as playback. Instruments that are silent until they are given expression (physically
modelled winds and strings) get it from the clip, and the built-in instruments take expression and channel volume
as a level. Three rules:

- *After a clip ends*, the pitch wheel, pressure and the pedals return to rest; every other controller stays
  where the clip left it.
- *By hand against a clip*: a controller moved by hand is the hand's until the song next starts playing, seeks
  while playing or loops round.
- *The pedal* is recorded as a controller, and notes are as long as their keys were down: nothing is written
  twice, and half-pedalling survives.

**Playing and recording**: every MIDI keyboard that is plugged in plays the armed tracks (or the selected one)
without a visit to the settings: notes, velocity, pitch wheel, mod wheel and sustain pedal, for built-in
instruments and plugins alike. A key is placed in its audio buffer by the time it went down, so it is heard a
fixed moment later rather than whenever the next buffer happens to start. Recording writes what is played into
clips where it was *heard*: the engine takes the output latency off again, to the sample. The wheels, the pedals
and expression are recorded with the notes, thinned to the few points of a curve that follows the hand. A
count-in, loop recording that layers each pass onto the last, one undo step per take. See
[Playing and latency](#playing-and-latency).

**Application**: an arrangement with multi-clip editing (move and copy across tracks, trim either edge, split,
fades, a clipboard, time selection, deleting, repeating or inserting a stretch of time on every track at once,
and copying one, with its automation, to paste over what is there or with room made for it), markers, group and
return tracks, and automation lanes that are edited by hand or drawn in steps; a piano roll, which lists a drum
track's rows by the names its kit has for them; device panels generated from each device's parameters; a mixer; MIDI import and export; project files; WAV
export, with every track, group and return as files of their own if wanted. `F1` lists the keyboard shortcuts.

The lane under the piano roll's notes shows velocity or a MIDI controller of the clip, chosen from the chips
above it: expression, the mod wheel, the pitch wheel, the sustain pedal, whatever else the clip holds, and any
other from the `+` beside them. A dot on a chip says the clip has a curve for that controller. The curve is
edited as an automation lane is: a click adds a point, points and lines drag, `Alt`-drag bends a line, a
double-click or `Delete` removes, a drag over nothing selects, and the pencil writes steps on the grid; each
gesture is one undo step. While a take is recorded its clip is in the piano roll, and the lane shows the wheel or
the pedal going down as it is moved. A track whose instrument is given notes and stays silent for a second is
marked on its header and in the lane's strip, with what to do about it (`--lane expression` opens the window with
the lane on a controller).

An automated control is not locked: drag it and it is yours. Its envelope is switched off, not lost, the mark in
the control's corner turns into an outline, and a click on that mark (or on the switch of its lane) hands the
control back to the envelope. That holds for a volume, a pan, a device's parameter and a send alike: a send's bar
in a track's header and in the mixer shows where its envelope has it, and a track's lanes offer its send to each
return beside its volume and pan. A lane of thousands of points (a compressor worked out elsewhere and drawn in)
is painted by its width: where points crowd, the line alone is drawn, and their squares come back as the view is
zoomed in. And when the program did not live to save a project, the window that opens next
offers it back: recover it, discard it, or decide later (`File`, `Recover unsaved work`).

The browser's **Sounds** tab is the sample library: add folders, search as you type, filter by one-shot or loop,
kind, tempo and key, mark favorites, and hear a sound as you select it. Drag a sound onto a track for an audio
clip where it lands (loops whose name states a tempo follow the song's), below the tracks for a new track, onto a
sampler or onto a drum rack pad; files dragged in from the file manager land the same way. Audio clips show their
waveform, and the Clip tab shows an audio clip's file with its gain, offset, fades, warp mode, source tempo and
transposition. The sampler shows its sample and the drum rack its sixteen pads.

**Instruments to play it with.** Trill ships no recordings. It knows a catalogue (`catalogue/`, text and nothing
else) of the sampled instruments that other people publish under licences that let anyone use them and hand them
on: 574 instruments in 119 sets, 26 GB in all, from grand pianos to drum kits to an orchestra's strings, brass
and woodwinds. 109 of the sets are dedicated to the public domain (CC0, The Unlicense) and ten ask only that
their maker be named (CC BY). Each licence was read where the set is published and is quoted word for word with
the address it was read at; each set is pinned to one version of its source and to what it unpacks to, so that
what arrives is what was read or a sentence says what is not. What was found and may not be handed on is listed
too, with its terms and the reason (`catalogue/left-out.toml`: four share-alike sets, which wait on a decision;
non-commercial ones; ones that say no licence that can be told).

Every instrument in it was loaded and played through the Sampled Instrument, and its entry says what it is, its
range, layers, takes and size, and what was measured: how far under its peak a recording falls quiet, how far
its keys lie from equal temperament, how its level steps from key to key, and, in the player's own sentence,
what its file asks for that the player does not do. A mark is a rule on those figures (`catalogue/README.md`
states each): 27 instruments are marked out of tune, uneven, noisy or silent as they load, and are offered
without being put forward. The figures are measurements; nobody has chosen by ear yet. A starter selection of
1.8 GB in twelve sets is marked for offering first: a grand piano, two electric pianos, the violins, violas and
cellos of a chamber orchestra, a double bass plucked and bowed, an electric bass, a drum kit, a nylon-string and
an electric guitar, a pipe organ and hand percussion.

**File, Setup, Instruments** is where a set is fetched. The catalogue is shown by kind, each kind opening to its
sets and a set to its instruments, with a field that narrows all of it to a word. A set's row says its size, its
licence and whether it is here; chosen, it shows everything there is to read before it is fetched: who made it,
its licence in its maker's words and where that was read, the line its maker asks to be named by, the version
it is pinned to and what it is checked against, and each instrument with what was measured against it where
something was (such an instrument comes with its set, and is not put forward). Over the catalogue stand the two
things most people ask for, each with its size on the button: **Starter** and **Everything**; a kind is fetched
alone from its own row, and a set by its own button. What was asked for at once is fetched one set after the
other, the smallest first, and is shown as one line with how many of its sets are here, how much has come and
one Stop. What was whole when it was stopped is kept and asking again goes on: a set that is published file by
file from the file it had reached, a set that comes as one archive from its beginning, because its source makes
that archive anew each time it is asked for. The room a set takes, as its archive and unpacked both, is asked
of the disk before it starts, and what was asked for at once is refused as a whole where there is not the room.
The sets are kept in a folder of Trill's own, or in the folder Move names (one of the sound library's from then
on), each in a folder of its own; a set that arrives is checked against what the catalogue pins, by what it
unpacks to or file by file, before the library is told of it, and one that is not what was read is a sentence
that says which file. A set is removed by the button that fetched it. Nothing is fetched until a button is
pressed, and the assistant in the window cannot fetch at all.

Every set of the catalogue was fetched once this way, one after the other (119 sets, 26 GB, 31 minutes on the
machine it was built on), and each arrived whole and as it was read; the starter selection asked for with the
panel's own button came in 103 seconds, and every instrument of it was found by the library and played. A set
under a folder deeper than Windows' 260 letters arrives and plays, and a fetch that was stopped was asked for
again on a set of each kind.

A model asks `instrument_catalogue` what there is (by kind, by words, one set with its licence) and which of it
is in the sound library already, and fetches a set with `setup` when the user asks for it by name (a client of
the user's own; the assistant's endpoint refuses), having said its size and its licence.
An instrument of the catalogue that is in the sound library, however it came there, is known again by its file
wherever it lies. Where a maker asks to be named, Trill does it: File, Credits and the `song_credits` tool give, for the instruments the
open song plays, the lines to put where the song is released, and `catalogue/CREDITS.md` is the list for the
sets Trill offers. The catalogue is written by the scripts in `crates/trill-catalogue/make` (its `README.md` has
the steps: what was read of each set, the pinned commits fetched, each instrument measured through the player),
which nothing runs but a person.

A sampled instrument of the library is one row among the devices, with its size. Drag it onto a track and it is
what the track plays through, below the tracks for a track of its own, or double-click it; an `.sfz` file dragged
in from the file manager lands the same way. Its panel shows where its recordings lie over the keys and over how
hard they are struck, the keys that choose a way of playing, how much there is of it, and while it loads how far
the reading is. A sampler and a pad turn an instrument away, and the instrument's panel a single sound, each
with the reason in the status bar.

![The sound browser, waveforms on audio clips and an audio clip in the Clip tab](docs/images/sounds-browser.png)

**When a song opens with files missing**, the clips and pads that lost their file are marked, the status bar
says how many files are missing and how many were found, and a click on that line (or `File`, `Missing files`)
opens the list: each file with where it is used and what was found for it. `Relink` takes everything that was
found for sure, as one undo step; `Use` takes a file that is only likely, one at a time; `Locate` points at a
file by hand and `Look in a folder` at a folder to search, and what either says about where a folder went finds
the rest. A folder of the sound library that is gone is listed there too (and marked in the browser), and is
pointed at its new place along with the song's files.

**Arm a track and record.** A track with an instrument has an arm button in its header and on its mixer strip,
lit while it is armed. The armed tracks are what every keyboard plays and what a take records; arming one disarms
the rest, and `Ctrl`-click keeps them. `F9`, or the record button in the transport, starts a take after the
count-in chosen in the box beside it (none, one, two or four bars). The transport says what the take is doing
(counting in, recording, and which pass of the loop when the loop is on); `Space` or Stop ends it, what it wrote is
selected, and one undo takes the whole take back. Asked to record with nothing armed, Trill arms the selected track
if it has an instrument, and otherwise says why it cannot.

**Audio and MIDI settings** are one panel: `Ctrl+,`, `File`, or a click on the device in the status bar. It has
the output device, exclusive mode, the sample rate and the buffer size, and beside the choice what is running now
with its latency in milliseconds as the device reports it. What a choice costs is said before it is applied:
exclusive mode silences other programs on that device, and another sample rate stops playback and closes plugin
windows. Devices are listed and opened away from the window's own thread, so a driver that takes its time shows
as "Opening the device…" in a window that still draws. A device that was unplugged, a list that comes back
empty, a choice that fell back to something else: each is a sentence in the panel, with what Trill does about it.
Below are the MIDI inputs, each with a switch, a lamp that lights when it sends, what is wrong with it if
anything is, and a rescan. The status bar shows the device, rate, buffer and latency as they are at the moment.

**Play it from the keyboard you type on.** `Ctrl+K`, or the keyboard button in the top bar, turns the two middle
rows of letters into a piano that plays where a MIDI keyboard plays (the armed tracks, or the selected one) and
is recorded like one: `A S D F G H J K L ; '` are the white keys from C
and `W E T Y U O P` the black ones, `Z` and `X` move them an octave down and up, `C` and `V` play softer and
louder, `B` and `N` give less and more expression, `M` and `,` turn the mod wheel down and up, and `Tab`
sustains while it is held. The keys are where they are on the keyboard, whatever layout types
on them. A strip above the detail panel shows the octave, the velocity, expression, the mod wheel, the keys with
the letter on each and the notes being held, and says why nothing sounds when nothing can (no track selected, a
track with no instrument, a muted one, an instrument that is given the notes and puts nothing out); the piano
roll's own keyboard lights the same notes. The switch, the octave and the velocity are remembered.

A track is told the keyboard's expression (full, until it is set lower) and where its mod wheel is just before
the first note typed on it, and whenever either changes after that: an instrument that waits for breath sounds
when it is simply typed on, and a take records what it was played with. When the keys stop playing a track, its
mod wheel is put back at rest and its expression back at full.

While it is on, a letter it uses plays a note, and the shortcut that letter had is `Alt` with the letter (`F1`
lists them as they are at the moment). It never takes a key while a text field, the sound search or a dialog has
the keyboard, or while `Ctrl`, `Alt`, `Shift` or the Windows key is held. And no note is left sounding: whatever
ends the playing (the switch, another track selected or armed, an octave change under a held key, the window
losing the keyboard, a dialog, another audio device, another project, exit) ends its notes, and gives back the
controllers the keyboard moved.

![A chord held on the computer keyboard, lit in the strip and on the piano roll](docs/images/keyboard-playing.png)

No samples at hand? `cargo run -p trill-app --example sound_fixtures -- some/folder` synthesizes a small pack and a
song made from it (it is what the screenshots are taken from); add `some/folder/Night Shift` in the browser, or
start with `--library "some/folder/Night Shift" --browse sounds some/folder/night-shift.trill`.

## MCP tools

| Area | Tools |
|---|---|
| Project | `get_project`, `set_project`, `new_project`, `save_project`, `load_project`, `recover_project`, `import_midi`, `export_midi`, `history` |
| Tempo and meter along the song | `set_tempo` (the tempo from a bar on: a jump, or with `ramp` a glide that arrives there), `set_time_signature` (the time signature from a bar on, for so many bars if asked), `get_tempo_map` (both, with the bar, the beat and the second of every change, the bars a change cuts short, and the beat any bar starts at) |
| How it was made | `get_journal` (the steps of the session, who made each and where it landed, a page at a time; found by their words), `get_song_at` (the song as it was after any step), `journal` (keep one or not, write it out to hand on, delete it) |
| Tracks | `create_track`, `update_track`, `delete_track` |
| Devices | `list_devices`, `describe_device`, `add_device`, `get_device`, `update_device`, `delete_device`, `show_device_editor` |
| Presets | `search_presets` (one search across the presets of every installed instrument and the saved ones), `save_preset` (keeps a device's sound under a name), `save_sound` (keeps a track's instrument and the effects after it as one sound, which is then a preset of the instrument), `edit_preset` (renames what was saved, or changes its description and tags), `delete_preset` (for good; the in-app assistant is refused it) |
| Clips | `create_clip`, `create_audio_clip`, `update_clip`, `split_clip`, `duplicate_clip`, `delete_clip` |
| Sounds | `search_sounds` (sounds, or with `kind: "instrument"` the sampled instruments, each with what it is), `library` (its folders, and `move_folder` for one that went elsewhere), `preview_sound`, `set_sample` |
| Studying | `study` (takes a recording apart with models on this machine and keeps it: start with a file, a clip or, for a client of the user's own, a link; report with the stage it is at; stop; take up again; list; delete and drop its stems, each after saying which songs play them; gather what the open song plays of studies beside it), `take_from_study` (a stretch of its stems or one of its loops as audio that follows the song's tempo and, where asked, its key; a sung line or word as that stretch of the vocal; a single hit onto a lane, a pad or a sampler; a kit as a drum rack; a part as notes on an instrument track, a groove by the kit's own drum names; its parts as markers; its tempo bar by bar; its key; the recording as a reference track), `setup` (every part there is to fetch with its size, licence and source, which are here, fetch, stop and remove one; a list as long as the catalogue of instruments is counted, and read a page at a time when it is asked for by name; what runs the models and the test that times it, the programs that were found; the assistant in the window reads it and fetches nothing) |
| Instruments to be had | `instrument_catalogue` (the sampled instruments others publish that may be used and handed on: counted by kind and licence, searched by words or kind, or one set with its licence as its maker wrote it; says which are in the library already, and how a set is fetched with `setup` when the user asks for it; fetches nothing itself), `song_credits` (what the open song owes the makers of the sampled instruments it plays: the lines to put where it is released) |
| Listening | `listen` (what is in an audio file of the library, of the user's or of a clip, or what a `study` found in a recording's stems, with the words line by line and each word's bar and beat, or of one `stem` of it: tempo and bars, key, tuning, and on request its chords, parts, loops, single hits, groove, bass and melody in the recording's own bars, paged; corrections to its beat grid), `transcribe` (its bass line, melody, groove or chords as MIDI on its own beats: into a file, or into the song on a new track in one undo step) |
| Wavetables | `describe_wavetable` (the tables of `builtin:wavetable` that ship, any of them or a recipe of your own shown frame by frame in numbers; changes nothing), `set_wavetable` (gives an oscillator a table that ships, one made from a recipe, or one read from an audio file; names the macros) |
| Missing files | `missing_files` (what the song names that is not on disk, where it is used, and what was found with how sure it is), `relink_files` (what is sure, or more on request, or a folder or files named by hand: one undo step) |
| Notes | `get_notes`, `add_notes`, `set_notes`, `remove_notes`, `transform_notes` |
| Controllers | `get_controllers`, `set_controllers`, `shape_dynamics` (gives a line its dynamics from its notes, as an expression curve) |
| Music | `write_chords`, `write_drums`, `write_arpeggio`, `write_bass`, `write_melody`, `analyze_music`, `music_reference` |
| Instruments | `get_instrument_profile`, `set_instrument_profile` (what a track's notes mean to its instrument: drums or pitches, the drum map, keyswitches, range, octave, bend range; kept in the song and remembered for the plugin) |
| Automation | `set_automation`, `get_automation`, `set_markers` |
| Arrangement | `edit_time` (remove, repeat or open up a stretch of time on every track at once; copy one to another place, on every track or some, over what is there or making room) |
| Playback | `transport` (play, stop, seek, record) |
| Film | `film_plan` (what a film would be, take by take, before any of it is made; costs nothing), `film` (makes it: the song or a stretch of it with its own sound, or how it was made, as an .mp4) |
| Hearing | `probe` (what an instrument does: range, octave, keyswitches, drum map), `measure` (the mix, every track, every section, gain reduction; writes nothing), `render` (the same, and a WAV file; with `stems`, every track, group and return in files of their own from the same pass) |
| Playing live | `play_notes`, `midi_inputs`, `audio` (device, sample rate, buffer size, latency) |

Time is in beats. Pitches are MIDI numbers, note names (`"F#3"`) or drum names (`"kick"`). Notes can be written
compactly as `[pitch, start, length, velocity]`. `set_automation` takes a track's volume or pan, the master's
volume, a device's parameter or a send (`{"type": "send", "track": 3, "to": 9}`), says in its description how a
line travels for each, and answers with what the envelope's lines are straight along. `get_automation` lists a
thousand points a reply and reads on by target, by a stretch of beats or by offset: a song may hold tens of
thousands. Errors say what to do next: an unknown id comes back with the ids
that exist, a bad note with its index and the reason.

Presets are part of the device tools, for plugins as for built-in devices: `describe_device` and `get_device` list
them a page at a time and search them (`preset_query: "warm pad"`, `preset_category: "Pads"`), and `create_track`,
`add_device` and `update_device` take one by its name, its category and name, or its id. `search_presets` looks
for a sound ("warm pad", "808 sub") across every installed instrument at once and answers with the device and the
preset to give `create_track`; the presets are read in the background the first time and remembered, and the reply
says which are still being read. `list_devices` says how many presets a plugin has, where that is known. The
presets a maker keeps in files of its own format (see "The makers' own presets") are among them, and `preset`
also takes the whole path of a preset file that lies anywhere (`search_presets` finds those of the sample library:
an item with `file`). After a preset
that sets the plugin reading samples, the reply has a `note` that says so. A plugin that lists none says so in a
sentence: its presets are in its own window, and `save_preset` keeps what is chosen there.

`get_device` is small whatever the device: for a plugin with hundreds of parameters it gives the count and the
ones that are not at the plugin's own defaults, `query` finds others by name with their ranges and choices, and
`show: "all"` pages through every one. It says which track a compressor listens to (`sidechain`), which
`add_device` and `update_device` set. Each of a plugin's parameters comes with `text`, the value as the plugin
displays it, and `update_device` takes that text as well as a number: `{"Cab L Mic Type": "Dynamic 57", "Input
Gain": "-6 dB"}`. It also says which channels the device takes and gives. Where a plugin's saved state has
settings that no parameter reaches, `get_device` counts them (`settings_count`), `query` finds them by name beside
the parameters, and `update_device` sets them: `"settings": {"M-Fret Sound": 1}`.

Every call is entered in the session's journal as it is made, under what it did in words and by the name the
client gave: `get_journal` lists them, `search: "chorus"` finds the step where the chorus was written, `after`
lists what changed since, and `get_song_at` shows the song as it was there. Undoing takes nothing out: a step
that was undone is followed by one that takes it back.

`film` takes minutes, so `film_plan` comes first: it lists the takes the director would shoot, each with its
bars, its shot (`wide`, `close`, `ensemble`, `song`), the parts shown as piano rolls and whether it is cut or
moved to. `direct` changes them, a stretch at a time: `{"section": "Chorus 2", "shot": "close", "parts": [7]}`
holds the lead close for that chorus, `{"from_bar": 17, "to_bar": 24, "shot": "wide", "enter": "cut"}` cuts wide
at bar 17. What is left out stays the director's, and the plan read again shows what was got. `size` is
`"1080p"`, `"2160p"` or `"upright"`; `section` or `start`/`end` film a stretch; `of: "making_of"` films the
journal instead. The assistant in the window writes films only into its working folders, as it writes
everything else.

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
  trill-core      Project model, edit operations, the device API, and the journal of a session with its replay.
                  No audio or UI dependencies.
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
                  safety copies of unsaved work.
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

Not yet: running plugins out of process (only scanning and the reading of presets are isolated today, so a plugin
that crashes while playing or while it loads a preset still takes the app with it; the safety copy is what saves
the work. One such crash is known and worked around: Neural DSP's Archetypes take the process down when an
instance is taken down before a message the plugin posted to itself has reached it while another instance is
alive, which probes and renders of a song that uses one did; an instance that is let go of is now kept, and taken
down only when the plugin thread has delivered what was waiting and the plugin has had fifteen seconds of it), a preset browser for plugins in the window (the session lists, searches and loads them, and so does
MCP; the device panel shows built-in and saved presets only), asking a CLAP plugin for a stereo layout when its
default is mono (a VST3 plugin is asked),
plugin editors on Linux and macOS,
hearing a making-of in the window (it is silent there: the song as it was is shown, not played; a film of one
has the finished song under it), filming a making-of that was handed over as a file (a film is of the song that
is open, whose finished state it ends on), choosing the shots of a film in the window (the director's are used;
over MCP they can be directed),
finding a song's journal again after its file was moved or renamed (it is kept by where the file is),
"collect all and save" (copying every file a song plays into a folder beside it; the song already says where
its files are from its own folder and plays the ones beside it, so what is missing is the copying, a rule for
two files of one name, and an edit that points the song at the copies),
finding files a plugin keeps in its own state (only audio clips and the built-in sampler and drum rack name
theirs to Trill),
a loaded kit's own names in the window (the piano roll names a drum track's rows from the track's own map and
General MIDI, not from what the plugin says is on its keys; over MCP every tool reads the kit), keyswitches for
notes drawn by hand in the window (the tools write them; a note that is drawn is a plain key), kits of drum
plugins other than Superior Drummer 3 (none of the others on the machine says what is on its keys in a state
Trill reads),
for the sampled instrument: the four share-alike sets and the eleven instruments published only as 7z or tar
archives (listed, not offered), anyone's ears on which instruments are the best of their kind (the starter
selection is by measurement), an archive gone on with from the middle (the sources make theirs anew each time),
a set that changed at its source taken as the newer one (it is a sentence; the pin is moved by hand),
the parts of SFZ named above as not done, formats other than SFZ, a drum kit's microphones as separate levels
(a kit that brings several is played as its file mixes them), taking the instrument out of a device in the window
(another takes its place, or the device is removed), and the time to a first note from a disk that has not
read the files before,
an audio clip that repeats when it is drawn out (a loop from a study is laid end to end instead), chords as
something a song shows by itself (they are drawn along a reference, and written as notes), a general "collect
all and save" (only what a song plays of studies is gathered), a drag between Trill's two windows tried with a
real mouse, a study's models on anything but Windows (the models' own files are the same; the
runtime and what drives a card are pinned for Windows alone), a transcriber as large as Magpie's, the six-stem
model,
for the wavetable synth: sounds tuned by ear (the owner has heard sixteen of its eighty-two; the others are held
to measurements and nobody has heard them), effects inside it (a preset that wants them brings them as devices), making a table or naming a macro in the window (over MCP; the window chooses among
the tables there are and shows the one that is played), a formant filter, moving the unison's detune or an
envelope's times from the matrix, and warps free of what a corner in the cycle adds (sync, bend and squeeze leave
42 to 57 dB under the note what does not belong to it and is heard, two octaves over middle C; FM by a bright
wave far more),
for sounds of more than one device: what is in front of the
instrument, a plugin that is not installed kept in the song until it is (the sound is made without it), and
taking back the deletion of a saved one (it is asked about first, and is then for good),
session (clip-launching) view, audio recording (MIDI is recorded; audio is not), polyphonic aftertouch and MPE
(per-note expression; clips hold one curve per controller for the whole channel), ASIO, warp markers, plugin
delay compensation, multi-core rendering. Only Windows has been run so far.

Where the tempo or the time signature changes inside a song, not yet: a plugin's position in seconds and the
start of its bar (it is told the tempo, the time signature and the beat of each block; the seconds and the bar
line it is given are still worked out from those alone, so after a change they are not the song's, and through a
glide the seconds do not run evenly); a
warped clip under a glide without one stretched copy of its file for every step of the glide, and without the
three-millisecond fade where two copies meet, which a held tone can show; the length a sound dropped into a
later section is given, which goes by the tempo the song begins at; and the Clip tab's own picture of a
recording that keeps its speed, which is drawn at the tempo where the clip starts.

## Licence

Trill is free software: you can redistribute it and modify it under the terms of the GNU General Public License,
version 3 or (at your option) any later version. It comes with no warranty. See [LICENSE](LICENSE).

The Inter typeface bundled in `crates/trill-noir/assets/fonts` is under the SIL Open Font License; its text is beside
the font.
