# Plugins, presets and sounds

**Plugins**: installed CLAP and VST3 plugins appear beside the built-in devices as `clap:<plugin id>` and
`vst3:<class id>`. Their state is saved in the project, and their parameters are automatable and reachable over
MCP like any other device's. A plugin's own editor opens in a window (on Windows, macOS and Linux), from the Editor
button on its panel or at the end of its chip in the mixer, and what is changed there flows back into the project.
On Linux that window is an X window, also on a Wayland desktop (through Xwayland): where no X server can be
reached, no plugin offers one, and the window says so once as it starts. An error the X server reports on a
plugin's own connection, which would otherwise end the program, is taken by Trill, said once in the log, and passed
on to a handler the plugin set of its own.

**Which thread drives a plugin**: a plugin is made, asked and let go of from one thread of the program's. On
Windows that is a thread of its own. On macOS plugins take the program's first thread for theirs and post work of
their own to it, and some make no sound until that work has run, so there the first thread drives them: the
program without a window and every process that renders, measures or probes lend it to the plugins and do their
own work beside it. Measured on this: an amplifier (Archetype Tim Henson X) was silent when `trill-mcp` played it
live, and Superior Drummer 3 and Hammond B-3X never answered their notes in a render, which was then refused; driven
from the first thread, each sounds. A song's other plugins render to the same samples as before.
`cargo run -p trill-plugins --example plugin_threads -- "vst3:Serum 2"` says, for an installed plugin, which thread
its own work runs on and whether it sounds.

The window on macOS draws on its first thread too, so there it drives its plugins between frames: a plugin is made a
step at a time (its instance, its state, setting up, its parameters, activating), with a frame drawn between two
steps, and its file is loaded beforehand on another thread. While a step runs the window stands still: on a song of
eight plugins, 0.6 s at the most (Hammond B-3X activating), and the line that says which device is coming next is on
screen before it. An editor's window on macOS is made on that thread too, and opening one stands the window still for as long as the
plugin takes to make its view: measured with the `editor` example, 0.1 to 0.35 s (Pianoteq 9, SWAM Violin 3, Serum 2,
ValhallaRoom and ValhallaSupermassive, Consort's Hall and Marimba in CLAP) and 3 s for Hammond B-3X. A view's sizes
are in points and it is told no scale: AppKit draws it at its screen's resolution. **A plugin that hangs on macOS
freezes the window**, which cannot be drawn or closed until the
program is ended (Force Quit). The song is not lost: the safety copy of unsaved work goes on being written by
another thread every twenty seconds, with each plugin's state as it last was, and the next start offers it back. On
Windows a plugin that hangs costs a sentence and the window goes on.

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

**MIDI channels**: a plugin is told each note, wheel, pressure and controller on the channel the song has it on,
in both formats. A CLAP note says its channel, and what a note port takes as MIDI carries it in the message. A
VST3 note says its channel, and a controller is the parameter the plugin names for that controller *on that
channel*: the plugin is asked once, when it is made (never on an audio thread), for every controller, the
pressure and the wheel on each channel its event bus has. A plugin built with JUCE, as the Consort collection is,
names one for each of sixteen channels. A key comes up on the channel it went down on; a pedal that is worked out
for a plugin that takes none holds the keys of its own channel; and silencing a plugin lets go of every key on
its own channel and tells the first channel, and every other one anything was ever said on, that all sound is
off (as MIDI to a CLAP port that takes it, through the parameter a VST3 plugin names for it on that channel).

*A channel the plugin has not got is its first.* A song may say more channels than a VST3 plugin's event bus has
(many say they have one). Whatever is on such a channel, notes and controllers alike, goes to the plugin on its
first: nothing is left out, the plugin is never handed a channel it said it has not got, and such a plugin plays
a song that uses channels as an instrument that listens to every channel would. A controller the plugin names a
parameter for on its first channel only is set there whatever channel the song has it on. Nothing in the reply of
a tool says how many channels a plugin has: what a channel means is the instrument's to say, in its profile.

The host's fixed room was sized for one channel and is now sized for sixteen: a block may set 128 parameters
through controllers (it was 32) and 1024 values of them (it was 256), and a silence may let go of every key of
every channel. A guitar with a string to a channel moving the wheel and the pressure of six strings all through a
block of 512 frames is 192 values on 12 parameters.

**What a plugin costs when nothing goes into it**: every device of a song runs all the time, so that a track
is there the moment a note is played on it or it is unmuted. Trill tells a plugin of every block whose input is
nothing but silence, in the way its format has for that (a VST3 bus's silence flags, a CLAP channel that is
constant), and a plugin may take that as leave to rest. Whether it does is the plugin's own business, and the
amplifiers that were measured do not: Archetype Tim Henson X took 8.9 % of a processor on noise, 8.6 to 8.8 % on
silence it was told of and 8.8 % on silence it was not; Archetype Cory Wong X 12.6 %, 12.5 to 12.6 % and 13.2 %
(blocks of 480 frames at 48 kHz, one instance alone; `cargo test -p trill-plugins --test silence -- --include-ignored
--nocapture` measures any installed effect so). In a song of seven guitars through such amplifiers, the seven
cost two thirds of a processor while the song stood still. A synthesizer rests by itself as a rule (Serum 2:
0.4 % standing, 14 % playing), and a reverb that costs 0.3 % has nothing to save. One plugin was seen to act on
the word: Surge XT's effect as a vocoder with nothing to listen to, which was never the same from one render to
the next, came out silent or at another level than before. Trill does not stop running a plugin on its own
account: some give another sound depending on how many blocks they were run, and none was
found that asks for it. What a track costs is in the window's Playback load and in `playback_load`; switching a
device off is what takes its cost away, and muting its track is not.

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
