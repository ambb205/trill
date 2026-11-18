# Instruments and effects

The devices that come with Trill, and the sampled instruments it can fetch. Hosted plugins are in
[Plugins, presets and sounds](plugins-and-presets.md).

## Instruments

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
effects inside: a preset that wants an echo or a room is a sound (see [Plugins, presets and sounds](plugins-and-presets.md)) and brings the delay, the
chorus or the reverb as devices behind the synth, to be seen, changed and taken away like any other. Its low-pass filters
beside Surge XT's at 1 kHz (a measurement, on demand): without resonance the 12 dB ones are 3 dB down at the same
place (687 and 688 Hz) and fall 12.0 and 11.2 dB an octave from 2 to 8 kHz, the 24 dB ones 24.7 and 22.3; at half
the resonance's travel this synth's 12 dB filter peaks 9.9 dB over its passband and Surge's 6.2, the 24 dB ones
7.2 and 11.3; at nine tenths Surge's is all but ringing by itself (36 dB and more) where this one's stands 19 to
22 dB. In the window its panel draws the wave an oscillator
reads at its position, the envelopes and the LFOs, and lists the matrix slot by slot.

Its bank is held to numbers, because whoever wrote it cannot hear: the author has heard the first sixteen sounds,
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
  is held, a stab is over when the key is let go; eight are on sounds the author heard and passed, which were
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

## Effects

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

## Sampled instruments to fetch

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
