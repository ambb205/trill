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
and stops when you stop: Trill listens for nobody otherwise.

A program joins wherever it can open a connection to the other. Trill runs nothing that introduces two home
networks to each other: across the internet, one of you forwards a port or both are on a VPN.

**What the other needs.** The song travels; its files and plugins do not. Whoever joins plays the song with their
own copies of the audio files (by the paths the song names) and their own plugins. A plugin one of you has not
got stays in the song as a place that is kept, with the sentence that says so, as in a song that is opened
without it; its settings are carried untouched, and it sounds for whoever has it.

**Three people** need two connections: a program that is connected to two others passes on to each what it hears
from the other.

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

Everything is the song's, and travels, except what is plainly one person's listening: the transport, the loop,
which tracks are soloed, which reference is listened to, what is selected, zoom and scroll, and what is armed.
Mute is the song's.

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
the plugin's whole state, once it has settled for about a second, and the other program makes its copy of the
plugin again from that state in the background while the old one keeps playing. A plugin does not always write
out the bytes it was given, so a state is news only when the plugin was changed in your program: two programs do
not make each other's plugins again for ever.

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

MEASURED

## What is not built

- Audio files travelling between machines. Each of you needs the files where the song names them.
- A plugin one of you has not got heard by them (a stem from whoever has it).
- A meeting point for two home networks.
- A view of what happened while you were apart.
- The assistant as a person of its own in the song.
- Versions of a song that are kept apart and merged back.
- What is kept of removed things and of when each property was set is never thrown away, so a shared song's
  file only grows.
