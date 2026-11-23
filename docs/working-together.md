# Working in one song together

Two or more people work in the same song at the same time, each in a Trill of their own. Nobody is the host:
every program that has the song has all of it, plays it with its own plugins and files, and can go on alone. The
programs are connected to each other by invitation: directly, where one can reach the other, or through a
*meeting point*, a small server that both connect out to, where each is at home behind a router of their own.

This is a first build, made to be tried. What it does not do yet is at the end of this page and in
[Status](status.md).

## Sharing and joining

**File, Share this song** opens a panel: the name you go by, and who can reach the song.

- *This computer only* (the default): another Trill on the same computer can join. Nothing is opened to any
  network.
- *My network*: computers on the networks this one is on can join, at home, over a VPN, or through a port you
  forwarded. The first time, Windows asks whether Trill may be reached on the network; say yes for the network you
  are on. Trill changes nothing about the firewall itself.
- *A meeting point*: someone anywhere can join. Both programs connect *out* to a meeting point and it passes
  what each says to the other, so nothing listens on your computer, no firewall asks and no router has to let
  anything in ([through a meeting point](#through-a-meeting-point), below).

**Share** shows the invitation: one line of text with a button that copies it, `trill:HOST:PORT:SECRET`, or
`trill:meet:HOST:PORT:SECRET` through a meeting point. Hand it to whoever is to join; they choose **File, Join a
song** and paste it. From then on both have the song and change it at once.

Whoever has the invitation can read and change the song for as long as it is shared, and nobody else can. The
secret in it never crosses the wire: each side proves it has it, and everything said afterwards is encrypted and
authenticated (the Noise protocol, through the `snow` library; no cipher is written in Trill). **Stop sharing**
closes the port, and the invitation is no longer one; whoever is connected stays. Listening starts when you share
and stops when you stop: Trill listens for nobody otherwise. A connection that has not proved the secret within
ten seconds is closed, and no more than sixteen may be trying at once.

**Letting one person go.** Whoever came into the song by your invitation has *Remove* at the end of their row
under *Who is here*; it asks first. Their Trill is told in a sentence and does not ask for the connection
again, and whoever was in the song through them goes with them. They keep the song as it was then: nothing
that was handed over is taken back, and they get nothing of what is done after. **The invitation becomes
another**, at the same address or the same meeting point: the one they hold lets nobody in, also not someone
who was on the way in by it at that moment. Whoever else came in by it stays, is told the new one over the
connection they have, and comes back by it by themselves when that is lost. Someone whose connection was lost
at just that moment, and anyone who has not joined yet, needs the new invitation from you: the panel shows it.
Nobody is the host, so nobody removes whoever let them in, and someone who came in through another person is
that person's to let go: the refusal says which it is.

What someone who is connected says is not trusted either: a message is bounded in size and messages in rate,
and a change is held to the rules every edit is held to before any of it is taken. A program that says what no
edit could have made is let go of, with a sentence, and the song is as it was.

A program joins wherever it can open a connection to the other: on one computer, on one network, over a VPN,
through a port one of you forwarded, or through a meeting point.

**What the other needs.** The song travels, and so does its audio (below); plugins do not. Whoever joins plays
the song with their own plugins. A plugin one of you has not got stays in the song as a place that is kept; its
settings are carried untouched, and it sounds for whoever has it. Whoever has not got it hears the track as
someone who has it recorded it ([a plugin one of you has not got](#a-plugin-one-of-you-has-not-got), below), so
a friend does not need the same plugins to hear the song, only to change what those plugins do.

**A build to hand to a friend.** `sh scripts/friend-bundle.sh` makes a release build and gathers it into one
folder, `target/friend/Trill`: `trill.exe` (which starts no program but itself), `trill-mcp.exe`, the licences,
and a page that says what their computer needs and how to join. Nothing of yours goes into it: no settings, no
meeting point, no key, no song.

## The song's audio arrives with the song

What the song plays of audio goes to whoever is in it: what its audio clips and its takes play, and what its
samplers and drum racks play. Whoever joins gets it without asking, and what is added later follows by itself:
a take that landed, a clip that was dropped in, either way round. It comes from anyone in the song who has it,
not only from whoever added it.

**A file is known by what it is, and nobody is told where anyone keeps it.** The song says of each file its
SHA-256, its length and its name. Where it lies is each computer's own: no path is said to anyone, and none is
taken from anyone. A program that says one is let go of, a clip that arrives arrives without one, and what the
state of a sampler or a drum rack names is given a place made on this computer before the state is taken in. So
nothing another program says is ever opened, read, written or handed out. A file is asked for by its hash and
by nothing else, and the answer to a hash a program does not hand out is the same whatever the reason: nobody
learns by asking what someone has.

**What arrives is kept apart for each song**, in Trill's own folder (`%LOCALAPPDATA%\trill\received`, a folder a
shared song), under a name made here: its hash, and one of a fixed list of endings. It is a file of the song
only once all of it is here and its bytes are what the song says; anything else is thrown away and said. In a
song, a program hands out only what that song brought it and what its own person's files in that song are.

**Only audio goes, and only what you were shown.** A file is read to be said and handed out only if Trill reads
it as audio, by the reading that plays it: what is no audio is a clip that does not play, as it always was, and
nobody hears of it or of its hash. A song file is a stranger too: what it names on your computer is held until
you have been shown it.

- *Before you share*, the Share panel says what goes: how many files, how large, the folders they lie in, and
  one by one every file that lies outside the song's own folder, Trill's recordings and your sample library.
  Only what Trill reads as audio is counted, because only that goes. What the song names that is no audio is
  listed after it, each by where it really lies, as what stays: a song file someone handed you may name
  anything on your disk, and you should see what it named. Sharing is your word that the audio goes.
- *A file your own hand brings into a shared song* (a clip you drop in, a sample you give a sampler, what you
  record) goes without a question: that is the hand's act.
- *Anything else that comes to name a file here* is **held**: a shared song you opened from a file and joined
  again with, or a file the assistant gave a clip. The status bar and the panel say which files are held, and
  *Let them go* is yours to click. Nothing of a held file is read, said or handed out.

**It arrives within bounds you can see.** The panel and the status bar say what is on its way, from whom, how
much and how far. A file on its way is not a missing file: its clip says *Its audio is on its way*, nobody goes
looking for it, and it plays when it is there without the song being opened again. What a file may cost is
settled before its first byte: one that is said to be longer than any file is (64 GB) is refused, one the disk
has not the room for waits and is tried again, and above 500 MB waiting at once (`together.ask_above_mb` in the
settings) the rest waits for your word, asked once, with *Let them arrive*.

**A file nobody has said waits for whoever brought it.** A clip or a sample that someone else brought into the
song can be in it before their Trill has said what its file is: for a moment while it reads the file, and for
as long as it has not got the file itself, does not read it as audio, or holds it until its person lets it go.
On your computer that is not a missing file, and nothing is looked for on your disk. The clip is silent and
says *Its audio waits for whoever brought it*, the status bar says *1 audio file waits for whoever brought it*,
and the panel names the clip. It is theirs to put right, at their computer.

**An edit never waits for a file.** What is changed in the song, and who is here, go ahead of a file's bytes on
every connection: behind one piece of 48 kB at the most. A file is read to be played before it has its name,
away from the song, so that no edit that arrives meanwhile waits for it.

**It survives the connection.** A file that was half here when a link dropped, or when Trill was closed, goes
on from where it was when the song is in company again, from anyone who has it.

**If you have the file already.** Point the clip at it (a relink): where the song says what the file is, yours
is read and held against that. If it is the file it is taken and nothing is fetched, which is how two people
with the same sample pack each play their own copy. If it is another file it is refused in a sentence.

**A song that was saved opens whole, alone, later.** When you save, what the song brought you comes beside it
as recordings do, into `<song> files/from others`, under the names the song says: a second name for the same
file where the disk allows, a copy otherwise. A song's folder is then whole whoever recorded what is in it, and
Trill's own folder can be cleared without a song losing anything. What arrived for a song you never saved stays
in Trill's folder; nothing that arrived is ever deleted by Trill.

**What does not travel.** A sampled instrument's recordings (hundreds of files) stay where they are: the other
has the instrument's place, and a sentence that its file is not there. What a plugin names inside its own state
travels as the plugin wrote it, with whatever it names. A file Trill does not read as audio.

## Through a meeting point

Two people at home cannot open a connection to each other: a router lets nothing in that was not asked for. A
meeting point is a small program on a server that both connect out to. Someone has to keep one
([Keeping a meeting point](meeting-point.md) says how); Trill ships no address of any.

**Whoever shares** needs two things from its keeper, once: its address (`meet.example.org:443`) and a key. Choose
*A meeting point* in the Share panel and give both; the key is typed into a field that shows dots, leaves the
window with the click that keeps it, and is shown to nobody again. Both are kept in Trill's settings and nowhere
else: not in a song's file, not in the journal, not in a log, and no tool reads or sets them (an AI client says
that no meeting point is set, and that it is yours to set). In the settings file they are `together.meeting_point`,
for whoever runs Trill without its window.

**Whoever joins** needs the invitation and nothing else: it carries the meeting point's address.

**What the meeting point can do.** It passes what the two programs say and counts the bytes. It cannot read the
song or join it: the two programs prove the invitation to each other *through* it, by the handshake they use on
a connection of their own, and everything after is encrypted between the two of them. It knows the meeting by two
things worked out one way from the invitation's secret, neither of which is the secret or leads back to it: one
that names the meeting, and one by which it asks whoever comes whether they have the invitation, so that someone
who only overheard what the meeting is called cannot so much as knock. It does know that your key opened a
meeting, from which addresses people came, and how much was said.

The panel says how the meeting stands there: being reached, open, or away. If the meeting point does not take
your key, sharing ends with the sentence that says so. If its keeper lets a key pass only so much in a day and
that has been passed, the sentence names the meeting point and says that nobody gets in through it until
tomorrow; the song is yours to go on with alone meanwhile, and what each of you does is merged when you meet
again.

**The shape of it.** Whoever shares holds the meeting open with one connection, and each of the others is
connected to them through the meeting point: two connections there for each person, and none between the others.
What one of them does reaches the rest through whoever shares, as with three people connected directly.

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

**A recording is a file, and it follows.** The others have the take and the clip that plays it at once, and the
recording itself a moment later, as any audio of the song arrives (above): on its way meanwhile, not missing.

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

**A connection that is lost comes back by itself.** A cable is pulled, a laptop sleeps, a meeting point is
restarted: nobody left, and nobody has to do anything. The program that asked for the connection asks for it
again, after a second and then after longer and longer, up to every half minute, for as long as the song is open.
The status bar says so for as long as it lasts (*Connection to Ada lost · trying again*, *meeting point away,
trying again*), each of you goes on in the song meanwhile, and what you did is merged when the connection is
back. Nobody pastes an invitation twice in one sitting. This holds for a direct connection as it does through a
meeting point; where the song is shared through one, the meeting is held open again by itself too, under the
invitation it had. Only when asking again is no use does it stop, with a sentence that says to ask for the
invitation again: the other shares under another invitation by now, or ended the meeting. (Someone who *leaves*,
or closes their Trill, has said so: that is not a lost connection, and nothing is asked for again.)

**A line that goes down says nothing, so each connection says that it is there.** When a cable is pulled or a
wireless network drops, neither program is sent a word. The one whose own line it was sees its connection end
within seconds. The other sees nothing at all: as far as its system can tell the connection is open, and it would
go on showing someone who is no longer there for minutes. So every connection says a few bytes every two seconds
when it has nothing else to say, whatever the program is busy with, and a program that hears nothing over a
connection for ten seconds takes it to be lost: it lets go, says so (*the connection was lost: nothing was heard
over it for 10 seconds*), and whichever of the two asked for the connection asks again. Whoever's own line dropped
and came back is usually in the song again before the other was told anything: their program asks again at once,
and the connection it had before is let go of without a word, because its person is here.

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

## A plugin one of you has not got

A song plays plugins, and your friend may not have yours. They have the track all the same, with its notes and
the plugin's place and settings in it; what they have not got is the plugin to play it with. So your Trill plays
it for them: it records what the track's devices make of the track, up to and including the last one that
someone in the song has not got, and that recording goes to them like any audio of the song. Their Trill plays
it in place of those devices. Nobody does anything for this, and nobody is asked.

- **What comes after the plugin is still theirs.** The recording is of the track as it leaves that device: ahead
  of the devices after it, the fader, the pan and the sends. Your friend moves the fader, or an equaliser that
  follows the plugin, and hears it at once.
- **A change to what was recorded is heard when it is recorded again.** A note written into the track, a setting
  of the plugin, the tempo: until your Trill has recorded the track again, your friend hears it as it was, and
  their window says so where the track is (a hollow mark on its header in place of a filled one: *heard as Ada's
  Trill recorded it before the last change*). The recording that is there plays on meanwhile; nothing falls
  silent. Your Trill records once what it depends on has stood still for two seconds, every track that is
  wanted at that moment in one pass, and its status bar says which while it does (*Recording Bass for the
  others*, *Recording Drums and 4 more tracks for the others*): it is a render, and takes what a render of
  those tracks takes.
- **Who records.** Whoever has every device the recording takes. Where two of you have, the one whose program
  drew the lower number does, so that it is made once. A plugin none of you has is as it always was: in the song,
  and silent.
- **The recording is the song's.** It is saved with the song your friend saves, and plays when they open it alone
  another day. It is of the song as it was then, and says so once something it depends on is changed with nobody
  there to record it again.
- **Not for a group, a return or the master.** What those play is what other tracks feed them, and a recording
  of that would stand in for every fader on the way. A plugin that is missing there lets through what comes into
  it, as it always did.

What a computer has not got is said to the others who hold the song (the devices of this song, and nothing
else), so that they know what to record. A recording is 16-bit audio at 48 kHz, about eleven megabytes a
minute, from where its track first plays to where its sound ends, kept in Trill's own folder with the song's
other files from other programs; the one before it is removed when the newer one is there. In the window of
whoever hears by recordings, the status bar says how many tracks are heard so, and a plugin that is heard by a
recording is not counted among what needs attention.

**Measured** on two real windows on one machine, which was busy with other builds: the first with its plugins
and a three-minute song that plays seven of them on six tracks (Superior Drummer 3, two Ample guitars through
an amplifier, two Serum 2 and a reverb on a return), the second with no plugins at all.

- The second had the song a tenth of a second after joining, and the first knew what it lacked 0.2 s later.
- All five tracks that play clips were recorded in one pass and heard in the second window 170 s after it
  joined: 168 MB of recordings. (Recorded a track a pass, which is how this was first built, it was 420 s.) The
  return's reverb has no recording: the second window hears that return dry.
- Each track measured alone in both windows, the plugin in the first and the recording in the second: the same
  loudness and the same peak to a hundredth of a decibel on four of them, and 0.07 LU apart on the drums, whose
  plugin does not play the same hits twice.
- A note written into the drums in the first window: the second said within 6 ms that it heard the drums as
  they were, and heard the note 34 s later.
- The window without plugins held 0.6 GB, the one with them 3.7 GB.

## For an AI

The `shared_song` tool shares (on this computer, its networks, or through the meeting point you have set),
joins, leaves, lets one person go who came in by this program's invitation (`remove`, by their name: the reply
has the invitation that holds from then on), and says who is here and what they hold, with the notices and the
edit that answers one, whether a
meeting is open at its meeting point, which connections are being asked for again, which plugins of the song
each person's computer has not got and which tracks are heard by a recording in their place
([AI and MCP](ai-and-mcp.md)). A client's edits are steps of the program it talks to, said to the others as they
are made. No tool takes a meeting point or its key: what a tool is called with is in the journal and in the
client's own record of the conversation, and a key is neither's to keep. The assistant in the window acts as you:
what it does is yours to undo, and it does not share or join (that opens your song to someone, and is yours to
do).

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
| The song opened, its eight recordings (340 MB) read | 1.1 to 1.8 s | 1.2 to 1.5 s |
| The song opened where its recordings are not found | 0.30 to 0.32 s | 0.25 to 0.34 s |
| A note written into a clip | 1.6 to 2.2, 3.0 to 3.8 | 1.9 to 3.0, 3.3 to 4.7 |
| A clip moved | 1.8 to 2.0, 2.6 to 3.5 | 1.8 to 2.0, 3.0 to 3.9 |
| A fader set | 1.8 to 2.9, 3.0 to 4.0 | 1.8, 3.0 to 4.1 |
| A device's parameter set | 0.4 to 0.6, 0.9 to 1.2 | 0.3 to 0.5, 0.6 to 0.7 |
| A step taken back | 1.5 to 2.3, 2.5 to 3.0 | 2.0 to 2.4, 3.3 to 4.1 |
| A step brought back | 1.4 to 2.2, 2.3 to 2.9 | 2.0 to 2.1, 3.3 to 4.0 |

(The build before is the one with recording in it and without sharing. Six turns each, the two builds in turn,
started from the same folder; the song was opened twelve times more in turn. In one further turn the machine was
busy with something else and the song took 2.4 s to open on the one and 6.2 s on the other.)

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

**Through a meeting point**, the same song and the same two programs on Windows, with the meeting point as the
Linux program under WSL on the same machine (`together -- SONG.trill --through ADDRESS KEY`), held beside the two
connected directly in the same half hour:

- From an edit in one program to the other's graph swapped in: 24 ms in the middle and 36 to 40 ms for nine in
  ten, against 22 and 36 ms directly. Either way it is the 25 ms at which what was changed is said: what the
  meeting point adds cannot be told from that. (With the meeting point in the same process as the two, 23 and
  35 ms.)
- The meeting was open at the meeting point 0.05 s after sharing, and 1.1 s after where WSL had to be woken
  first. Joining took 1.5 s and passed 8.0 MB, as directly.
- It passes what the two say and adds nothing: 6 kB a second under a drag, 107 bytes a second with nothing
  changing, and a word of a few dozen bytes every twenty seconds by which whoever shares says they are there.
- The meeting point stopped and started again: both programs were in the song again 4.2 s after it was back,
  with what each had done meanwhile, and nobody did anything. (Each asks again after 1, 2, 4 seconds and so
  on.)
- What the meeting point itself takes is on [its own page](meeting-point.md#what-it-takes): a megabyte, and
  about 23 kB for each connection.

**The song's audio**, measured with `cargo run -p trill-runtime --example files_cross -- MEGABYTES`, which makes
a recording of that size, shares a song that plays it and times it crossing while a fader is set in the song
every few dozen milliseconds. Two programs in one process on a machine that was building other things, so the
figures swing:

| | 50 MB | 300 MB | a fader set meanwhile, to the other's song |
|---|---|---|---|
| Directly | 0.4 to 1.4 s | 2.1 s | 16 ms in the middle, 29 ms for nine in ten, 34 ms at worst |
| Through a meeting point in the same process | 0.8 s | 3.4 s | 50, 73, 79 ms |
| Through the Linux program under WSL | 2.8 s | 8.5 s | 86, 135, 249 ms |

With nothing crossing, the same edits took as long in each of those runs (17, 29, 31 ms directly; 77, 114, 131
under WSL at the time): a file does not hold an edit up. Reading a file to say what it is costs its SHA-256, about
a gigabyte a second (46 ms for 50 MB, 0.3 to 0.5 s for 300 MB), on a thread of its own, once a run for each
file. While a file crosses, the two programs together hold about 10 MB more than before; what the receiver then
holds is the recording read to be played, as for any audio of a song (600 MB for those 300 MB of 16-bit
stereo).

That measurement found two things that the tests with their small files had not. A file was asked for a piece
at a time, which is thousands of questions a second for a long recording: more, faster, than a program that
works in a song says, and the connection was let go of for it once in every 200 MB or so and came back a second
later. And an edit that arrived while an arrived file was being read to be played waited for that reading, two
seconds for 300 MB. A file is now asked for in strides, and read before it has its name.

**On a line as a home has it.** All of the above is two programs on one machine, where a megabyte takes a
millisecond. With `--line UP:DOWN:MS` each program is put on a stand-in for a home's line: so many kilobits a
second up and down, and so many milliseconds each way (`trill_together::line`). The two still talk over this
machine, so nothing is lost on the way as it can be on a real network, but bytes take the time they would.
Twelve recordings of a megabyte each, through a meeting point, with a fader set in the song meanwhile (the
middle one of the edits; the machine was running other tests, so the figures swing):

| Each program's line | all twelve arrived in | the line alone takes | a fader set meanwhile | the same with nothing crossing |
|---|---|---|---|---|
| 5 Mbit/s up, 50 down, 20 ms each way | 21 s | 19 s | 157 to 167 ms | 68 to 74 ms |
| The same, 60 ms each way | 22 to 23 s | 19 s | 254 to 320 ms | 149 to 206 ms |
| 1 Mbit/s up, 10 down, 40 ms each way (twelve of half a megabyte) | 56 to 61 s | 48 s | 172 to 217 ms | 141 to 171 ms |
| Directly, 5 Mbit/s both ways, 150 ms each way | 22 s | 19 s | 237 to 370 ms | 177 to 289 ms |

So the song's audio takes what the line carries, and what is done in the song meanwhile arrives about a tenth of
a second later than it otherwise would. It did neither before it was measured so. What a program says of the
song goes ahead of the files in its own queue, but behind whatever its system and the line have taken already,
and that is as much as it was asked for. Each file had been asked for ahead on its own, so twelve at once were
twelve times as much: with sixteen recordings arriving, a fader took 540 ms where it took 120 ms with one, and on
the slow line two seconds. And only what arrives in a quarter of a second had been asked for ahead, whatever the
way: where a question and its answer take that long to cross, the line stood idle while the program asked, and
twelve megabytes took 45 seconds on the line that carries them in 19. Now no more is asked of one program at
once, of all its files together, than is on its way there and back and arrives in an eighth of a second; how
long the way is is measured by a small question put while nothing is on its way, and what the line carries is
the most that was counted to arrive of late (going by the last count alone, what was asked for swung until
nearly nothing was). The file with least to go is asked for first, so that the most are whole the soonest.

## What is not built

- A sampled instrument's recordings travelling, and telling the other which set of the catalogue an instrument
  is from. The other has the instrument's place and a sentence that its file is not there.
- Saying *why* a file someone brought has not been said: whoever waits for it is told that it waits for them,
  and not whether it is missing there, held there or no audio.
- A list of what arrived and was never saved with a song, with a way to remove it. It stays in Trill's folder.
- What was read of a file (its hash) kept from one run to the next: every file a shared song names is read once
  each time Trill is started and the song is in company.
- A file that arrives while the meeting point's day is nearly spent waits for the next day; there is no asking
  the keeper for more from inside Trill.
- Giving way when its maker is not there: of two recordings put over one stretch at once, it is the program of
  whoever was earlier that makes room, so until that program is in the song again both play.
- A plugin one of you has not got on a group, a return or the master heard by them: it lets through what comes
  into it. On a track that plays clips it is heard by a recording (above).
- A recording for a plugin one of you has not got made smaller than plain audio, or made again only where the
  track changed: all of the track is recorded again, eleven megabytes a minute.
- Such a recording brought beside the song at a save, or found again where the song's folder went: it stays in
  Trill's own folder, and on another machine it is made again by whoever has the plugin.
- Where three or more hold a song, everyone keeps the recording, also whoever has the plugin and did not make
  it: the one who needs it may be connected to them alone.
- A plugin that does not do the same thing twice (a drummer that draws its hits) is heard by whoever has not got
  it as it was recorded, which is one of the ways it sounds.
- A direct way between two programs that met through a meeting point, where there is one: everything they say
  goes through it for as long as they are in the song.
- Letting go of someone who came in through another person, from anywhere but that person's Trill; and telling
  the new invitation to someone whose connection was lost at the moment another was let go: they are told to
  ask for it. Someone whose own connection is lost at the moment is not among who is here, and can be let go
  once they are back.
- Taking back what someone who was let go already has: they keep the song and its audio as of then.
- A meeting point that is encrypted as a web site is (TLS). What the two programs say to each other is encrypted
  end to end all the same; what is said to the meeting point itself can be read on the way and is of no use
  there, and a network that passes nothing but TLS will not pass it.
- A connection that is asked for again does not give up by itself: where the other never comes back it is asked
  for every half minute until you leave. After someone stops sharing a song they listened for themselves, a
  connection that is then lost cannot tell that from their computer being off.
- A view of what happened while you were apart.
- The assistant as a person of its own in the song.
- Versions of a song that are kept apart and merged back.
- What is kept of removed things and of when each property was set is never thrown away, so a shared song's
  file only grows.
- Two computers. Everything here was run on one machine: both programs on Windows, and the meeting point as the
  Linux program under WSL on the same machine. Nothing has crossed a real network, a real router or the internet.
  Sharing with *My network* has not been run once: it asks the firewall a question that is yours to answer.
