# Working in one song together

Two or more people work in the same song at the same time, each in a Trill of their own. Nobody is the host:
every program that has the song has all of it, plays it with its own plugins and files, and can go on alone. The
programs are connected to each other directly, by invitation.

This is a first build, made to be tried. What it does not do yet is at the end of this page and in
[Status](status.md).

## Sharing and joining

**File, Share this song** opens a panel: the name you go by, and who can reach this computer.

- *This computer only* (the default): another Trill on the same computer can join. Nothing is opened to any
  network.
- *My network*: computers on the networks this one is on can join, at home, over a VPN, or through a port you
  forwarded. The first time, Windows asks whether Trill may be reached on the network; say yes for the network you
  are on. Trill changes nothing about the firewall itself.

**Share** shows the invitation: a line of text, `trill:HOST:PORT:SECRET`, with a button that copies it. Hand it to
whoever is to join; they choose **File, Join a song** and paste it. From then on both have the song and change it
at once.

Whoever has the invitation can read and change the song for as long as it is shared, and nobody else can. The
secret in it never crosses the wire: each side proves it has it, and everything said afterwards is encrypted and
authenticated (the Noise protocol, through the `snow` library; no cipher is written in Trill). **Stop sharing**
closes the port, and the invitation is no longer one; whoever is connected stays. Listening starts when you share
and stops when you stop: Trill listens for nobody otherwise. A connection that has not proved the secret within
ten seconds is closed, and no more than sixteen may be trying at once.

What someone who is connected says is not trusted either: a message is bounded in size and messages in rate,
and a change is held to the rules every edit is held to before any of it is taken. A program that says what no
edit could have made is let go of, with a sentence, and the song is as it was.

A program joins wherever it can open a connection to the other. Trill runs nothing that introduces two home
networks to each other: across the internet, one of you forwards a port or both are on a VPN.

**What the other needs.** The song travels; its files and plugins do not. Whoever joins plays the song with their
own copies of the audio files (by the paths the song names) and their own plugins. A plugin one of you has not
got stays in the song as a place that is kept, with the sentence that says so, as in a song that is opened
without it; its settings are carried untouched, and it sounds for whoever has it.

**Three people** need two connections: a program that is connected to two others passes on to each what it hears
from the other.

**Two windows on one computer**, to try it alone: the second Trill needs settings of its own, so start it with
`trill --profile <a folder> --no-mcp` (the first window has the MCP server's port). Share in one with *This
computer only*, join in the other.

## Who is here, and what they hold

While the song is shared the status bar says who is here, each with a square in their colour; a click opens the
panel, which lists everyone with what they hold. In the song itself:

- a clip someone else has selected has an outline in their colour and their name at its top right;
- a track they have selected has a bar of their colour down its header;
- the notes they have selected are outlined in their colour in the piano roll;
- where their song stands, or plays, is a line in the ruler with their name. Each of you has a transport of your
  own: one can play the chorus while the other listens to the bridge.

None of this is part of the song or of its journal, and it is gone when they are.

## What is the song's, and what is yours

Everything is the song's, and travels, except what is plainly one person's listening or one machine's: the
transport, the loop, which tracks are soloed, which reference is listened to, what is selected, zoom and scroll,
what is armed, and of a track that records, which input of your audio device it records and whether you hear it
meanwhile. Your input 3 is not anyone else's: it is never said, and nobody's is ever taken from them. Mute is
the song's. So are takes, the lanes they lie in, what is put on a track of them and the fades across its seams.

## When two of you change the same thing

Both intentions hold wherever they can:

| One of you | The other, at the same moment | The song ends up with |
|---|---|---|
| Writes notes in a clip | Writes other notes in the same clip | All of them |
| Moves a note | Changes that note's velocity | Both changes |
| Moves a clip to bar 3 | Moves the same clip to bar 5 | The later move; the other is told and can put theirs back |
| Turns a knob | Turns the same knob | The later value |
| Adds a device to a chain | Reorders that chain | Both |
| Deletes a track | Writes notes in it | The track gone, said to the other, brought back whole on request |
| Records a take on a track | Records one on the same track over the same bars | Both takes, each in a lane of its own; the track plays the one that landed later, and the other is told and can put theirs back |
| Puts a stretch of a take on a track | Puts another over the same stretch | The later, as if it had been put there after the other; the other is told and can put theirs back |
| Makes A listen to B | Makes B listen to A | The one made first; the other waits, and its maker is told |
| Changes a plugin in its window | Changes the same plugin | One whole state: a plugin's settings are not merged |

**A clash is a notice, once.** Where what someone did overrode something of yours that they had not seen, removed
something you had worked in, or left an edit of yours waiting, a card appears above the status bar with the
sentence and a button: *Put mine back*, *Bring it back* or *Have mine instead*. That button is an ordinary edit,
which can be undone. *Leave it* takes the notice as read. Two people on one fader is one notice, not a list.

**A thing that is removed is kept.** A track, clip, device or note that someone removes is out of the song and
held, with whatever is done in it meanwhile, so that bringing it back loses nothing. What went with it (its
clips and devices, the sends to it, its envelopes) comes back with it.

Where two edits are each allowed and cannot both hold (two tracks listening to each other, a track made a
reference at the moment it is given a send), the one made first stands, as it would have had they been made one
after the other, and the later one waits: it is in the song as soon as what stands in its way is gone, or at
once if you choose *Have mine instead*.

## Recording

A take that is being recorded is nobody else's yet: nothing of it is said while it is played or sung. When it
has landed, the others have it as the one step it is, in its lane beside the track with what the track plays of
it, and its maker's `Ctrl+Z` takes it back for everyone, as with any step. Arming a track, hearing an input
and choosing one stay in your own program, and nothing that arrives from anyone opens a microphone.

**A recording is a file, and files do not travel yet.** The others have the take and the clip that plays it,
naming the file where your computer has it. On their computer that file is missing, which is said once, as any
missing file is, and it plays as soon as the file is there. (Two windows on one computer both find it.)

**Two recordings over one stretch.** A track that keeps takes plays one recording at a time. Where two of you
put one over the same stretch at the same moment, each not knowing of the other (two takes that land there, or
two stretches of takes put there), the one that was made later is what the track plays, as if it had been made
after the other and had covered it. The program of whoever was earlier makes that so and tells them once, with
*Put mine back*, which covers the other's in turn. Nothing that was recorded is lost by any of it: every take
stays in its lane, and whatever a recording covers is kept as a take first. Two takes that were put in the same
lane over the same bars lie in two lanes.

## Undo is your own

`Ctrl+Z` takes back your own last step, whoever acted last: what your step set is put back where it is still as
your step left it. Something it set that someone else has set since stays theirs, and Trill says so ("One thing
stays as someone else has set it since"). A thing your step made goes, even if someone has worked in it since:
they are told, and can bring it back with their work in it. A thing your step removed comes back with whatever
was done to it meanwhile. What someone else does never clears your redo.

This is how undo works for someone working alone too: a step keeps what it changed, not the whole song.

## Going away and coming back

**Leave** (in the panel, or File, Leave the shared song) ends the connections; the song stays as it is. A shared
song is saved with what it takes to merge it again, so each of you can save, close, work alone for a day, and
join again: both sets of work are merged by the same rules as above, and where they clash you are told as above.
Joining a song you do not have replaces the one that is open (Trill asks first if it has changes that are not
saved); joining the song you already share merges the two.

## Plugins

What you set on a device in Trill travels value by value. What you change in a plugin's own window travels as
the plugin's whole state, once it has stayed the same for a second or two, and the other program makes its copy
of the plugin again from that state in the background while the old one keeps playing. A plugin does not always
write out the bytes it was given, so a state is news only when the plugin was changed in your program: two
programs do not make each other's plugins again for ever. When you share a song, every plugin's state is read
once first, so that whoever joins is handed each plugin as it is running in your program. A preset that a plugin
loads itself is the same: the others see its name at once, their own copy goes on playing what it had, and it is
made again once, when the state has been read.

## For an AI

The `shared_song` tool shares, joins, leaves and says who is here and what they hold, with the notices and the
edit that answers one ([AI and MCP](ai-and-mcp.md)). A client's edits are steps of the program it talks to, said
to the others as they are made. The assistant in the window acts as you: what it does is yours to undo, and it
does not share or join (that opens your song to someone, and is yours to do).

## How it works

A song is held as its *properties*, each with the moment it was last set (`trill_core::merge`): whether a thing
is there, where it is, and each of its values are properties of their own, and a thing has an id that needed
nobody's leave (the program's own number, drawn by lot once it changes a shared song, and a count). A property
is what the latest change set it to, which is why the same changes are the same song in whatever order, however
late and however often they arrive. The song an edit reads and writes is worked out from the properties by one
function that is the same everywhere; an edit is still a method on `Project`, and what travels is found by
comparing the song before and after. `crates/trill-core/src/merge/mod.rs` has the whole of it, and its tests
hold three programs that take the same real edits shuffled, late and twice, across a split, to the same song.

## What it costs

Measured on a long song without plugins (57 tracks, 1,544 clips, 36,864 notes, 183 devices; 6.2 MB as a file
before notes had ids), with both programs on one machine that was building other things meanwhile. Two examples
print these: `cargo run -p trill-engine --example edit_cost -- SONG.trill` and
`cargo run -p trill-runtime --example together -- SONG.trill`.

**Working alone costs what it did**, with the exceptions that are said under the table. Opening in seconds; for
the rest the fastest of three hundred and the middle one, in milliseconds, against the build before:

| | Before | Now |
|---|---|---|
| The song opened, its eight recordings (340 MB) read | 1.5 to 1.8 s | 1.4 to 1.5 s |
| The song opened where its recordings are not found | 0.31 to 0.32 s | 0.31 to 0.34 s |
| A note written into a clip | 1.6 to 1.7, 3.0 to 3.3 | 1.9 to 2.2, 3.5 to 3.9 |
| A clip moved | 1.8 to 1.9, 2.6 to 3.2 | 1.8 to 1.9, 3.1 to 3.2 |
| A fader set | 1.8 to 1.9, 3.0 to 3.3 | 1.8, 3.2 |
| A device's parameter set | 0.4, 0.9 | 0.3, 0.6 to 0.7 |
| A step taken back | 1.5 to 1.7, 2.5 to 2.7 | 2.1, 3.6 to 3.8 |
| A step brought back | 1.5 to 1.6, 2.5 to 2.6 | 2.1, 3.6 to 4.0 |

(The build before is the one with recording in it and without sharing; three turns each, the two builds in turn,
started from the same folder.)

- *Opening a song costs what it did*: about 15 ms more on this one, of which 8 ms read what a shared song keeps
  beside it, 2 ms give every note its id and 5 ms make the properties. What takes the time is reading the
  recordings a song plays, and that is as it was. The long song names its recordings by a path from the folder it
  was made in, so a program that is started anywhere else finds none of them and opens it five times as fast:
  the two builds are held beside one another from the same folder, and `edit_cost` says how many it found.
- *Undo and redo take about half as long again* (1.3 to 1.5 times; 0.5 to 1.3 ms more on this song): a step is
  no longer a whole song kept to be swapped in, and the song is worked out again from its properties.
- *A note written takes a fifth longer* (0.3 to 0.5 ms): the song is settled and compared with the song before
  on every edit, which is 0.2 and 0.26 ms of it here, against 2 to 5 ms for the compile.
- The file of a song with many notes is larger, here by a sixth (7.3 MB): every note carries its id.

**Together:**

- Joining: the song was in the other program 1.0 to 2.5 s after it asked, and 8.0 MB were said for it. That is
  what opening the song takes; a song with plugins then makes them in the background, as an opened one does.
- From an edit in one program to the other's graph swapped in: 21 to 25 ms in the middle, 33 to 46 ms for nine in
  ten. What is changed is said once every 25 ms, and taken in as it arrives.
- Taking in someone else's change costs what making it costs: 2.6 to 3.7 ms on this song against 2.4 to 3.0 ms
  for the one who made it. The session is held for that long and for nothing that waits on the network.
- A fader or a clip dragged, at sixty edits a second: 6 kB a second, in 33 messages a second of 180 bytes. Of the
  sixty values a second the other is told the latest every 25 ms.
- Nothing changed: 107 bytes a second, which is who is here.
- An hour of work, an edit a second (3,600: six in ten write notes, the others remove some, move a clip, set a
  fader or a parameter): 0.7 MB said. Each one's file grew by 1.7 MB, of which 0.8 MB is the song itself grown and
  0.8 MB is what a shared song keeps to merge again: when each property was last set, and the notes that were
  removed. A song that was never shared keeps none of that.

**Two windows on a song with plugins**, both on one machine (a song of ten tracks and seven plugin instances:
Superior Drummer 3, an Ample bass and guitar, an Archetype amplifier, two Serum 2 and a Valhalla reverb):

- Sharing took 0.08 s, and the join was answered after 0.5 s with the song there to work in. The joiner's seven
  plugins were all made 22 s later in one sitting and 58 s later in another, which is what opening the song from
  a file took it in that sitting (55 s). Each window held 3.8 to 4.1 GB.
- A fader, the tempo and a plugin's parameter set in one window were in the other within the time a tool takes
  to look (46 ms for the parameter).
- A preset given to Serum 2 in one window took the plugin 2.2 s to load; the other window's Serum went on
  playing what it had, and was made again once, from the state, 3 to 5 s after. What the two then say of
  themselves differs in 21 of 509 parameters' names and texts, and in the same 21 when the second window opens
  the song the first saved: a plugin made from a state does not always describe itself as the one that loaded
  the preset does.

## What is not built

- Audio files travelling between machines. Each of you needs the files where the song names them, and a take
  someone records is a file only they have.
- Giving way when its maker is not there: of two recordings put over one stretch at once, it is the program of
  whoever was earlier that makes room, so until that program is in the song again both play.
- A plugin one of you has not got heard by them (a stem from whoever has it).
- A meeting point for two home networks.
- A view of what happened while you were apart.
- The assistant as a person of its own in the song.
- Versions of a song that are kept apart and merged back.
- What is kept of removed things and of when each property was set is never thrown away, so a shared song's
  file only grows.
- Two computers. Everything here was run with both programs on one, over its loopback. Sharing with *My
  network* has not been run once: it asks the firewall a question that is yours to answer.
