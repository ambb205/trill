# The engine

**Engine**: sample-accurate MIDI and audio clips, clip looping (notes, and an audio clip that plays a stretch of
its recording round), non-destructive trimming and splitting, audio
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

**A song that plays uses the cores the machine has.** One thread plays a small song, as it always did: while
making its tracks takes less than half the time its buffers last (measured over a quarter of a second), nothing
is handed to another thread and the song is played exactly as before. A song that needs more is spread. The
tracks that do not feed one another are made at once, on a few threads beside the audio device's own, and added
up by the device's thread in the order one thread would have added them: what is played is the same samples on
any number of threads and at any buffer size (`trill-engine/tests/cores.rs` holds that to the bit, through keys
played by hand, a take being recorded, the metronome, an edit arriving while it plays, the loop and a jump). A
track waits for what feeds it, as in a render: a group for its members, a return for the tracks that send to it,
a track for the one its compressor listens to. The instances of a plugin that keep something between them take
turns in the order of the song, also as in a render (below), and what comes after them on a track (the amplifier
behind a sampled guitar) is taken up by whichever thread is free the moment the turn is over. A song is spread
at once when a single buffer takes four fifths of its time, goes back to one thread when it has played for a
quarter of a second at under a quarter, and stays as it is while it stands still, so that it starts on the
threads it needed when it last played.

*How many threads.* Half as many as the machine has processors, eight at the most, and never more than the
song has tracks to make at once; `TRILL_PLAY_THREADS` in the environment says another number, and 1 leaves
everything to the device's thread. The threads sleep between buffers, and are made off the audio thread: with
the first song that has anything to make at once, where a device is playing, so that they are there before a
note of it is heard.

*The first seconds.* What a song costs is only known once it has played. So a song that has just been opened,
or been given a device it had not got, is spread from its first buffer, and a quarter of a second of playing
decides: one that a single thread plays with time to spare goes back to it. Before this the threads were made
when a song had shown that it needed them, and the song of nineteen plugins below was first spread 214 ms after
it was open, with 10 to 12 of the twenty-odd buffers before that late, standing still or with play pressed at
once. Now its first buffer is made on its threads: none of the 211 buffers of its first two seconds was late
through the system's device with play pressed at once, and one or two (at 101 and 102 % of their time) on two
other tries, which is plugins doing something for the first time.

*What those threads may do.* They are audio threads like the device's own. None of them allocates or takes a
lock; work is handed across in atomics; the device's thread makes tracks itself like the others, never sleeps
on them (it looks again until they are done, and takes what has become ready meanwhile), and wakes them
without waiting for anything. All of them run in the class Windows keeps for audio ("Pro Audio"). So does the
device's thread in shared mode, which until now ran at the priority of any other thread of the program, because
the library that opens the device raises it only when it is built to (the exclusive mode's thread was always
raised). On the machine this was written on that thread was at the system's priority 0, an ordinary thread's,
before it asked, and was granted the audio class when it did. A device that fails costs the buffer it failed
in, on whichever thread, and what was played by hand in that buffer is let go of, so that the key it failed on
is not played to it again with every buffer after.

*Falling short is a sentence.* When five or more buffers come late within ten seconds, the song is said to be
more than this machine plays in time: once, in words, with how many buffers came late, that an export is not
affected, which tracks cost most and what helps (switching off or removing devices; muting a track does not
help, because its devices go on running). The window shows it as a notice, a line in its status bar and the
head of its Playback load panel ([the window](window.md)); `transport` and `playback_load` say it to a model.
It is one thing for as long as it holds and ten seconds after, and a new one only when the song has kept up for
a minute in between. What each track costs can be asked at any time, in the same two places.

*What was measured*, on a machine of eight cores (sixteen processors) that other work kept between a tenth and
a third busy, with `cargo run -p trill-runtime --example keeps_up`, which plays a song in real time against a
device that makes no sound. The song is one of nineteen plugins on sixteen tracks: seven sampled guitars, each
through an amplifier, a sampled bass, a drum kit, two synthesizers and a reverb; 191 seconds at 48 kHz. Load is
the share of a buffer's time that making it took; a buffer that takes more than its time is late, and heard.

| The device asks for | On one thread | Spread (eight threads) |
|---|---|---|
| 480 frames (10 ms, the shared device) | 115.7 % on average, 292 % at the worst, 10,569 of 16,345 buffers late | 34 to 38 % on average over eight plays, 74 to 146 % at the worst; six plays with no buffer late, one with 1 of 19,306 and one with 12, in nine of its seconds |
| 128 frames (2.7 ms) | 113.9 % on average, 282 % at the worst, 40,416 of 62,367 late | 38.9 % on average, 117 % at the worst, 2 of 72,396 late (37.5 % and 1 late on another run) |
| 64 frames (1.3 ms) | 119.4 % on average, 581 % at the worst, 84,489 of 120,615 late | 40.9 % on average, 100 of 144,798 late (41.7 % and 47 late on another run) |

*Through the device itself* (`--device`: the system's default output, shared with the other programs, handed
silence and asked for nothing) the same song took 35.7 % and 37.1 % on average on two plays, 74 % and 99 % at
the worst, with none of 19,306 buffers late in either, on a machine a fifth busy: what the stand-in says. That device (USB, 48 kHz) asked for exactly 480
frames every time, never more than 11.2 ms after it had last asked, and what it plays is heard 21.6 ms later.

*The buffers that still come late* are single blocks of one track that take many times what they usually do,
and there are two kinds of them. `--whose-time` holds the clock's time for a block against the cycles its thread
ran in it:

- *A plugin working.* A synthesizer's longest block took 3.3 to 3.8 ms of its thread's own running, and an
  amplifier's and a drum kit's about 3 ms, at any buffer size. In a buffer of 10 ms that fits. One of 2.7 ms or
  1.3 ms it makes late by itself, and no number of threads changes that.
- *A thread kept from its work.* The longest block of a run at 64 frames was a sampled guitar's track: 5.19 ms
  on the clock, of which its thread ran 0.19 ms. Every block of 5 to 17 ms that was seen (the one late buffer at
  480 frames was such a track's block of 11.6 ms) was on a track with a sampled instrument of one maker, on one
  thread as on eight, and never on a synthesizer's track, whose long blocks its thread ran from end to end. In
  the one play at 480 frames that had twelve late buffers, all eight tracks with such an instrument had a block
  of 5 to 12 ms, and the synthesizers' and the drum kit's had what they always have. That
  points to the instrument waiting inside its own call (for a recording to come from the disk, or for a thread
  of its own) and not to the system taking audio threads off their processors, which would not choose its
  tracks; it was not proved, since the system does not say why a thread was not running.

On four threads the song took 38.1 % at 480 frames, so the last four buy little: with eight, each track costs
a fifth to a third more than it does alone (139 to 154 % of one processor for all of them, against 115 %; 121 %
with four), presumably because they share the cores' caches and, with other work on the machine, the cores
themselves. Standing still the song cost one thread 75 to 102 % of its time, since an amplifier costs as much
on silence as on a guitar ([plugins](plugins-and-presets.md) has what was measured of that), and costs 17 to
23 % spread.

*What sets the limit now* is the longest thing that cannot be divided: the one track that costs most (here a
synthesizer at a quarter of a processor), with the returns and the master after it. The instruments that take
turns are not it in this song: its two chains are three sampled guitars each, some 2 % of a processor an
instrument, and the amplifiers behind them go on beside each other. A song with eight instances of one such
instrument would wait for all eight before its last amplifier. Handing work across costs little beside that: a
thread that was asleep is at its work 10 to 15 microseconds after it is called on average (a few hundred at the
latest), and the device's thread, with nothing left to do itself, waits for the others some 200 microseconds a
round.

*A small song is left alone*, because every thread more that a buffer waits for is one more that the system
may hold up in the middle of a track. A song of built-in devices (fourteen tracks, forty-eight devices, which
one thread plays in 13 % of its time) was played at three buffer sizes on one thread, as it comes, and spread
by force:

| The device asks for | On one thread | As it comes | Spread by force |
|---|---|---|---|
| 64 frames | 14.5 % on average, 38 % at the worst | 14.7 %, 40 % | 6.5 %, 36 % |
| 128 frames | 14.2 %, 28 % | 14.2 %, 40 % | 5.9 %, 17 % |
| 480 frames | 13.1 %, 30 % | 13.0 %, 22 % | 5.0 %, 10 % |

No buffer was late in these nine runs, and as it comes the song stayed on one thread throughout. Spread by
force it takes less than half the time; on another such run at 64 frames one buffer of 15,005 took longer than
it lasts, which none did on one thread.

**A plugin comes after the song has it.** Reading a song's file takes milliseconds, and so does an edit; making
a plugin takes as long as it takes to load its file, take its state on and read what it plays, on the one thread
plugins are made on. Until now the session made plugins where it stood, and everyone who wanted the song waited
for them: the window, which showed the song before with not a word when a song was opened and did not answer
when a plugin was dropped on a track, a model, the session's own chores. Now, whatever brings a plugin (a song
that is opened or begun, a device added, a sound or a preset given, a track duplicated, an undo or a redo, the
audio device at another sample rate), the song has it at once and it is made after: the song is compiled with
its place kept, a thread of the session's own makes such devices one by one in the order they were asked for,
with nobody waiting, and each is put in its place when it has arrived. The edit is the step of the history it
always was; the device arriving is none, and the journal knows nothing of it.

*Until a device is there* its place is empty: an instrument is silent, and an effect lets through what comes
into it. (A device that is made anew in a song that plays, because it was given another state, goes on as it
was until its successor is there.) Play is never refused, and both front ends say what is missing
([the window](window.md); `loading` and `not_made_yet` for a model, in [AI and MCP](ai-and-mcp.md)). The song
says everything of the device that is needed to make it, which is what a save, a safety copy and a render take:
a song saved in the middle is the whole song, and a render, a measurement or a probe in the middle makes its
own devices as it always does. That holds for a preset too: one that only the plugin can load, given before
the plugin is there, is in the song as what the device is to start from, whoever makes the device has it load
that, and the device arrives as that preset; the state it came to takes its place in the song when plugins'
states are next taken (a save does). What cannot be done to a device that is not there (a parameter, its
window) is refused in a sentence. A device is put in only as the one it was made as: one that was removed
meanwhile, or that the song has in another state by then, is let go of, and so is everything that was on its
way for a song when another is opened. One that cannot be made, or does not load the preset it was to start
from, is a problem of the song in a sentence; it used to be an edit that failed, and is now a device in the
song that says why it is not there, which an undo takes away.

*What leaves is taken down the same way*, by the same thread, after what there is to make and with nobody
waiting: the plugins of a song that was open before, of a device that was removed or a track that was deleted,
of a stream at another rate. Before, the session's chores did it with the session held. The other order was
measured and is worse: with the song before taken down first, the song below opened over itself had all its
devices after 22.7 s in place of 15.7 s, because sampled instruments that share what they have read with the
instances before them read it again, and it did not leave more memory free while it opened.

*The audio device at another sample rate* has every device made anew, since nothing of the rate before can go
on playing. The stream is there at once with what is made in no time, the song stands where it was, stopped,
and its plugins come after as when it was opened, with the same line; play is there to be pressed meanwhile.
Before, the change waited for all of them (as long as opening the song), with the session held.

*Nobody asks a plugin anything while one is being made*, where the answer can wait. A question to a plugin
(what are your parameters at, what is your state) is answered on the thread plugins are made on, so one asked
during a making is answered when the making is done. A window asks for the parameters of the devices it shows
whenever the song has changed, and the song changes with every device that arrives: measured as a window
looks, on the song below, the longest of those looks had the session for 6.1 s, in which a window stands
still. Now a device's values are those it last said while anything is being made or taken down, and that look
takes 46 ms at the longest of 1,807. What cannot wait holds the thread back from beginning another making
until it is answered, so that it waits for the one in hand and not for one a plugin: the states of all the
plugins that are there, for a save (4.2 s in the middle of a load from a cold disk, where the plugin in hand
was a sampled guitar that takes fourteen), a preset, a plugin's window. The safety copy, which nobody asked
for, is put off until nothing is being made.

*What was measured*, on the machine above, with `cargo run -p trill-runtime --example opens`, which opens a song
against a device that makes no sound and says when the song was there, when each device arrived, what each step
of each plugin's making took, and how long a window that looked at it meanwhile had to wait. The song is the
one of nineteen plugins above (38 devices on 16 tracks). How long its plugins take depends on whether anything
has read their files lately: the same song, the same day, takes two and a half times as long from a disk that
has not.

| | Made where the song is opened (before) | The song first (now) |
|---|---|---|
| The song is there (and the window has it to show) | after 16.5 s; 36.3 s from a cold disk | after 0.09 s; 0.10 s cold |
| Its first plugin | with the song | after 1.9 s; 2.3 s cold |
| All nineteen | with the song | after 16.9 to 18.3 s; 41.6 and 42.5 s on two cold runs |
| No plugin still reading what it plays | after 20.7 s | after 20.9 s; 45.6 and 46.5 s cold |

The makings themselves are the plugins' own time and are as long as they were: 14.9 s in the plugins' code on a
warm run, and 1.4 s more that one maker's amplifiers spend on what they post to themselves after each is made
(0.14 s after the first, 0.43 s after the sixth: it grows with how many of them there are). What is the host's
is the tenth of a second the song takes, a second activation of each plugin that has values of the song's to
start from (0.14 s for all of them), and handing each over (some 0.3 s). Nothing is read or scanned twice: a
plugin's file is loaded once, and a second instance of it takes none of that time.

| Plugin (its first instance, then the others) | Made in | From a cold disk | Of which |
|---|---|---|---|
| A drum kit | 1.8 s | 2.2 s | its state (it reads its kit) |
| A sampled bass | 1.4 s | 6.9 and 7.5 s | the instance 0.4 s (4.1 to 4.5 s cold), its state 1.0 s (2.8 to 3.0 s) |
| A sampled guitar (three kinds, eight instances) | 1.5 to 1.9 s the first of a kind, 0.45 to 0.7 s the others | 13.1 and 13.9 s the first, 3.8 to 4.2 s the first of the other two kinds, the others as warm | the instance, nearly all of it (11.3 s of the 13.9) |
| An amplifier (six instances) | 1.0 to 1.3 s, then 0.05 to 0.07 s | 1.6 s, then the same | its file 0.3 to 0.4 s and the instance 0.5 to 0.6 s the first time |
| Another amplifier, of the same maker | 2.3 to 3.0 s | 2.9 and 4.1 s | its file 0.3 s, the instance 0.5 s, its state 1.5 to 2.1 s |
| A synthesizer (two instances) | 0.44 s, then 0.16 s | 0.5 s, then 0.45 s | its file 0.3 s the first time |
| A reverb | 0.02 s | 0.05 s | |

*An edit that brings a plugin.* A sampled guitar added to that song, its files warm: the edit had the session
for 1.6 s before, which is the making, and has it for 3 ms now, with the guitar there 1.9 s after it was asked
for. From a cold disk the same guitar is the 13.9 s of the table: that is how long a window did not answer
when it was dropped on a track, and it is now how long the track says that its instrument is being made.

Opened over itself, with its own nineteen plugins to take down, the song was there after 0.1 s and whole after
15.7 s (its plugins' files were loaded already); taking the others down took 3.8 s more, after that, with
nobody waiting (a drum kit 1.1 s, a sampled instrument 0.15 to 0.34 s each). While the amplifiers of the song
before are still there, each new one costs their maker's own work 0.46 to 0.86 s in place of 0.14 to 0.43 s:
3.9 s over the song in place of 1.4 s. They are kept for a quarter of a minute because taking one down young
takes the program down ([plugins](plugins-and-presets.md)), so that is theirs too.

*A long song without plugins* was never the plugins' wait and is nearly all recordings. One of 57 tracks and
183 built-in devices, 36,864 notes in 1,536 clips and eight recordings of four minutes at another rate than the
device's opened in 1.56 s: 0.02 s for its file of 6 MB, 0.04 s for everything else, and 1.5 s for the
recordings, read and brought to the device's rate one after the other. They are now read four at a time, ahead
of the compile, and the song opens in 0.87 s. With four more tracks of a sampled grand piano it opens in
0.98 s: a sampled instrument is made in 2 to 32 ms and reads its recordings on threads of its own, and none was
still reading after 1.2 s.

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
- *A render uses the machine.* It has no speaker to keep up with, so it is spread whatever it costs. A render
  hands the tracks that do not feed one another to several threads (half as many as the machine has
  processors, eight at the most, unless `threads` or `TRILL_RENDER_THREADS` in the environment says another
  number), and then adds them up, by one thread, in the order one thread would have: the mix, every stem and
  every number measured are the same samples and the same figures as on one, sooner (the reply says how many
  threads it was). It makes no more devices for it than it does on one: a plugin of the song is there once. A track waits for what feeds it: a group for its members, a return for the
  tracks that send to it, a track for the one its compressor listens to. Plugins take more care. The
  instances of one plugin share its module, and some modules keep things for all their instances: run at
  once, the instruments of Ample Sound give other samples than run one after the other. So the instances
  of one plugin are run in turn and in the order of the song, beside everything else, unless the plugin
  was measured to give the same samples either way. Only those devices take turns, and only where a song
  has more than one of a plugin: what comes after them on a track (the amplifier behind a sampled guitar,
  which costs many times what the guitar does) goes on beside everything else as soon as its track has had
  its turn. (`trill-plugins/src/beside.rs` lists
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

**By channel.** A note and a curve are each on one of sixteen MIDI channels, the first unless they say another,
and whatever the engine remembers for a track it remembers by channel: which keys are down (the same key on two
channels is two notes, and a key struck again while it sounds is let go of first on its own channel and nowhere
else), where each controller stands, what is said again where a song starts in its middle, jumps or comes round
(each channel's wheel, pressure and pedals on that channel, before the first note), and what is let go of where
it stops. The lead-in of a render tells an instrument the same, and the wait before a render tries each note on
the channel the part plays it on. The keys that choose how notes are played are keys on the first channel, and
what is played by hand is on the first. Nothing of this is allocated while a song plays, and a song that uses
channels is the same samples at any buffer size and on any number of threads, as any other
(`trill-engine/tests/channels.rs`, and the part on three channels in `tests/cores.rs`).

*MIDI files.* A file is written on the channels its notes and curves have, a drum part that says none on the
tenth, as files have it. Read, a track whose notes are all on one channel comes in on the first: there the channel
only said which instrument a part was for. A track that uses several keeps them, curves too. (A file of the kind
that holds every part in one track is taken apart by channel, a part a channel, as it always was.) So a part
that is wholly on another channel than the first comes back from a file on the first.
