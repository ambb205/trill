# The window

## Editing

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

The Clip tab's sidebar has a *Channel* field under the velocity: the MIDI channel that is chosen. Drag it, or
double-click and type. With notes selected it sets their channel, as one undo step; with none it is the channel
new notes are drawn on. Either way the lane under the notes shows and edits the chosen channel's curve, and says
which where it is not the first. Where a clip uses another channel than the first, or its track's instrument
names its channels (an organ's pedalboard, a guitar's strings), every note carries its channel's number, the
notes of the other channels stand back, the field says what the instrument calls the channel, and the foot of the
sidebar lists the channels the clip uses. On every other clip nothing of channels is drawn but that field
(`--channel 2` opens the window with that channel chosen).

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

![The sound browser, waveforms on audio clips and an audio clip in the Clip tab](images/sounds-browser.png)

## Opening a song, and plugins that come after

**A song is on the screen at once, and its plugins come after.** From the moment a file is chosen the window
says which song it is opening: the song before is dimmed, under a plate with the file's name. A tenth of a
second later (a second, for a song of many long recordings) the song is there: its tracks, clips, names and
mixer, with every built-in device playing. Its plugins take as long to make as they take, one at a time, and the
window is alive meanwhile: the song can be scrolled, selected, edited, saved and played. For a song of nineteen
plugins that is seventeen seconds when their files have been read lately and forty-two when they have not (a
sampled guitar that takes two seconds takes fourteen from a cold disk): how long is the plugins' own, and
what the window does is say how far they are.

While plugins are arriving, one line says how far the whole is, "Loading 12 of 38 devices: Archetype Tim Henson
X on Lead", on a plate with a bar in the corner where the window's messages go, and in the status bar. A track
that waits for a device has a dot on its header (filled for the one that is being made right now), and the
panel of a device that is not there says so in place of parameters it has not got yet. Play is never refused: a
track whose instrument is not there is silent until it is, an effect lets through what comes into it, and
starting the song says so once. A plugin that cannot be made is a problem of the song, in a sentence, and the
rest of the song is there. Closing the window in the middle waits for the one plugin in hand and for nothing
else; choosing another song lets go of everything that was on its way for the first.

**The same holds whatever brings a plugin.** A plugin dropped on a track, a sound that brings one, a track
duplicated, an undo that brings one back: the song has it at once, with its step in the history, the window
goes on answering, and the plugin arrives with the same line and the same dot. A preset given to it meanwhile
is what it arrives as. Changing the audio device's sample rate makes every plugin anew, since nothing of the
rate before can go on playing: the song stands, stopped, with its built-in devices, its plugins come after as
when it was opened, and play is there to be pressed. A save in the middle is the whole song, and waits for the
one plugin that is being made at that moment and for no other.
[The engine](engine.md) has what all of it takes, plugin by plugin, warm and cold.

## Playing and recording

**Playing and recording**: every MIDI keyboard that is plugged in plays the armed tracks (or the selected one)
without a visit to the settings: notes, velocity, pitch wheel, mod wheel and sustain pedal, for built-in
instruments and plugins alike. A key is placed in its audio buffer by the time it went down, so it is heard a
fixed moment later rather than whenever the next buffer happens to start. Recording writes what is played into
clips where it was *heard*: the engine takes the output latency off again, to the sample. The wheels, the pedals
and expression are recorded with the notes, thinned to the few points of a curve that follows the hand. A
count-in, loop recording that layers each pass onto the last, one undo step per take. See
[Playing and latency](#playing-and-latency).

**Arm a track and record.** A track with an instrument has an arm button in its header and on its mixer strip,
lit while it is armed. The armed tracks are what every keyboard plays and what a take records; arming one disarms
the rest, and `Ctrl`-click keeps them. `F9`, or the record button in the transport, starts a take after the
count-in chosen in the box beside it (none, one, two or four bars). The transport says what the take is doing
(counting in, recording, and which pass of the loop when the loop is on); `Space` or Stop ends it, what it wrote is
selected, and one undo takes the whole take back. Asked to record with nothing armed, Trill arms the selected track
if it has an instrument or records an input, and otherwise says why it cannot.

**Record an input.** A track records what a keyboard plays unless it is told to record an input of the audio
device: right-click its header (or click the first row of its mixer strip) and choose a channel, or a pair as
left and right, under *Records*. The track then shows which input it is (`IN 1`, `IN 1+2`: a click changes it),
a monitor switch, and an arm button like any other. **Arming it opens the input**, and for as long as an input is
open the status bar says `INPUT LIVE` and the device's name, in the signal color: a microphone that is open
hears the room, whatever is done with what it hears. It is open only while something needs it (a track that
records it is armed or heard, a measurement runs, the settings show its level) and closes when nothing does. What
arrives is drawn beside the track's own level, a thin bar under its name and a meter beside its fader in the
mixer, with a lamp that stays lit once the input was at full scale until it is clicked: turn the input down at
the device and record again. `Shift+F9` arms the selected track.

*Hearing yourself.* The monitor switch has the input heard through the track's own devices, fader and sends,
where its clips come in: off as it comes, on while the track is armed, or always (from the input's menu). Lit, the
switch says how many milliseconds late the input is heard, which is the whole way from the microphone to the
speaker; the pointer over it says whether that figure was measured or is what the devices say. Use headphones: a
microphone that hears the speakers it is played through feeds back. A render never hears an input.

*A take of audio* is recorded by `F9` with the notes of the armed tracks that record a keyboard, as one take and
one undo step. Its clip grows on the track as it is sung, with its waveform, and what the track has under it is
not heard meanwhile. It is put **where it was heard**: the time the sound took to reach the microphone's buffers
and the time the song takes to reach the speaker are both taken off, from the first frame (see
[the engine's page](engine.md) for how, and *Audio and MIDI settings* below for measuring it). A count-in is
recorded too, ahead of the clip, so a breath before the first bar is there when the clip's start is dragged out.
If the device is pulled out, the disk fills, or the file reaches the size a WAV file can have, the take ends
whole up to there, a sentence says why, and the song plays on. Samples that did not arrive are silence of their
length, never a shift, and are said.

*Takes.* Every recording of a track is kept as a take, beside its clips and never among them. With the loop on,
each pass is a take of its own, and the track then plays the last whole pass; recording over a part keeps what
was there as a take first. Nothing recorded is destroyed by recording again. A track with takes has a caret
where its number was (`T` does the same for the selected track): unfolded, each lane of takes is a row under the
track, each take drawn as the clip it is, with what the track plays of it in the track's color.

- **Drag across a take** to put that stretch on the track, in place of what the track had there. That is the
  comp: ordinary audio clips, with a short crossfade at each seam that neither dips nor clicks.
- **Double-click a take**, or `Use` in its lane's header, to put the whole of it there.
- **`Hear`** plays a take in place of what the track has, without changing the song; again goes back.
- The menu of a take renames it and lets go of it. Its recording stays on disk.

Each is one undo step. What a comp covers is kept as a take first, so choosing between takes never loses one.

*Punch.* The `PUNCH` switch in the transport (`P`) has a take record only inside the stretch the loop marks on
the ruler, while the song plays through it and on: start a few bars before, sing along, and only the stretch is
recorded. For notes and audio alike. The narrowest windows leave the switch out; its key works, and the take
says that it is punched in.

## Audio and MIDI settings

**Audio and MIDI settings** are one panel of two pages: `Ctrl+,`, `File`, or a click on the device in the status
bar. The first page has
the output device and the device that is recorded from, exclusive mode, the sample rate and the buffer size, and
beside the choice what is running now with its latency in milliseconds as the device reports it. What a choice costs is said before it is applied:
exclusive mode silences other programs on that device, and another sample rate stops playback and closes plugin
windows. Devices are listed and opened away from the window's own thread, so a driver that takes its time shows
as "Opening the device…" in a window that still draws. A device that was unplugged, a list that comes back
empty, a choice that fell back to something else: each is a sentence in the panel, with what Trill does about it.
Below are the MIDI inputs, each with a switch, a lamp that lights when it sends, what is wrong with it if
anything is, and a rescan. The status bar shows the device, rate, buffer and latency as they are at the moment.

**Input and round trip**, the second page (a click on `INPUT LIVE` in the status bar opens it). *The panel does
not open a microphone by being looked at*: `Show level` opens the input and shows what arrives on each channel,
with a lamp a channel, and the input closes with the panel unless a track needs it. An input is one device with
all its channels; it is opened as the output is (shared, or exclusively where the output is and the device
allows), and the page says whether it runs on the output's clock or on one of its own, which is the ordinary
case of a microphone on one device and headphones on another.

*The round trip* is how long sound takes from the microphone back to the speaker: what the input says of
itself, what handing the sound across to the engine's clock takes, and what the output says. A take is put where
it was sung by taking all of it off. What devices say of themselves is often short of the truth, and until it is
measured the page, the monitor switch and the tools say that the figure is what the devices say, not measured.
**`Measure`** plays a second of quiet noise and listens for it on the channel chosen: connect the output to the
input with a cable, or hold the microphone to the speaker. What it finds is offered (`Use it`) and changes
nothing until it is taken; taken, it stands in place of what the output says, and is remembered for this pair of
devices as they were opened (shared or exclusive, rate, buffer). `Earlier` and `Later` move takes half a
millisecond at a time by hand, for putting right by ear what cannot be measured; `No nudge` and `Forget` go back.

**`File`, `Unused recordings`** lists the recordings that nothing in the song plays any more and that undo
cannot bring back, with their sizes. Trill never deletes a recording by itself: `Delete` asks first, and only a
second yes deletes.

## Playback load

**CPU in the top bar** is the share of each buffer's time that making it takes. A click on it opens what is
behind the number: how the audio thread kept up over the last ten seconds (on average and for its worst buffer),
how many buffers came late, on how many threads the song is made, the buffer the device asks for, and every
track with the share of one processor it takes, the costliest first. A track costs what it costs whether or not
it is muted or anything plays on it, because its devices run all the time; switching a device off or removing it
is what takes its cost away, and the panel says so.

**A song that is more than the machine plays in time is said so, once.** A buffer that takes longer to make than
it lasts is a gap in what is heard. When five or more come late within ten seconds, a notice says that the song
is more than this machine plays in time and that an export is not affected. It is one notice, not one a buffer:
the next comes only when the song has kept up for a minute in between. For as long as it holds (and ten seconds
after the last late buffer) the status bar has a line with the count, the CPU readout is in the signal colour,
and the panel begins with the whole sentence: how many buffers came late, which tracks cost most, and what
helps. One or two late buffers in a while are not that: every song has a plugin take its time over a block now
and then.

The same figures and the same sentence are what a model gets from `playback_load` and `transport`. A picture of
the window shows no load of its own; `--load calm` and `--load short` open the panel on a made-up one.

## Playing from the computer keyboard

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

![A chord held on the computer keyboard, lit in the strip and on the piano roll](images/keyboard-playing.png)

No samples at hand? `cargo run -p trill-app --example sound_fixtures -- some/folder` synthesizes a small pack and a
song made from it (it is what the screenshots are taken from); add `some/folder/Night Shift` in the browser, or
start with `--library "some/folder/Night Shift" --browse sounds some/folder/night-shift.trill`.

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
