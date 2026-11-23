# Status

Trill is an early build. Everything the other pages describe works and is held by tests. This page lists what is
not built yet.

Windows is where Trill is made. macOS on Apple silicon and Linux on x86-64 build, pass the tests and run the
window, the server and the engine; what each has not got yet is below ([macOS](#macos), [Linux](#linux)).

## Arrangement and engine

- A session (clip-launching) view.
- Recording from more than one input device at once. A song records one input device, with all its channels,
  beside the output it plays through; the two may be different devices on different clocks.
- An input and an output on two real clocks, measured. The engine follows the two clocks and its tests hold it
  to a click within half a millisecond over half an hour at 200 parts in a million; on hardware it has been run
  through a virtual cable only, whose two ends share one clock. See [the engine's page](engine.md).
- The first minute of an input and an output that are one device of which the system says nothing (a laptop's
  own sound card, a virtual cable). They are read as two clocks until their time stamps have said, for a
  minute, that they are one: a take in that minute lands within a frame or two of its place and is not the
  device's frames to the bit, as every take after it is. Where the stamps scatter it takes longer, up to five
  minutes of them. What the stamps said is kept while the program runs and not from one run to the next.
- Dither, and one full scale both ways, at an exclusive output of 16 or 24 bits. What is played is rounded to
  the device's steps without dither, at 32767 a full scale for 16 bits where what is recorded is read at 32768:
  what was recorded from such a device and is played to it again is the same to the bit only under half of
  full scale.
- A device's own word that frames are missing, on a real device. What a device says of a buffer (its flag, its
  count of frames, a stamp it does not stand by) is taken as the tests' scripted devices say it; no real device
  was made to drop frames.
- The engine's own clock through a callback that is late and caught up. An output that asks for a buffer more
  than a fiftieth of a second late is taken to have played that long without the engine, which is so where it
  ran dry; a device that holds more than that and catches up puts a step in what is recorded then, and another
  when its line is found again.
- Takes of notes. A take of what a keyboard plays is written into the track's clip, each pass of a loop layered
  onto the last, as before; lanes of takes and a comp are for audio.
- A crossfade at a comp's seam is 12 ms and equal in power; its length and shape cannot be chosen.
- Monitoring that makes up for a plugin's own delay: an input heard through an effect that looks ahead is heard
  that much later than the switch says.
- Polyphonic aftertouch and MPE (per-note expression). A note has a MIDI channel and a clip a curve for each
  controller on each channel, which is what an instrument that gives its channels meanings needs (an organ's
  pedalboard, a guitar with a string to a channel); expression that belongs to one note among others of its
  channel, and pressure on a single key, are not built.
- A channel for what is played by hand. The computer keyboard, a MIDI port and a take are on the first channel,
  whichever channel a keyboard sends on.
- A MIDI file keeps the channels of a part that uses several; a part that is wholly on one channel comes in on
  the first, so one written wholly on another does not come back on it.
- ASIO (see [Playing and latency](window.md#playing-and-latency)).
- Warp markers.
- Plugin delay compensation.
- A tape start that arrives in step with the song. The tape stop set going again plays what comes in from that
  moment and, once at full speed, skips what it fell behind by (half the length of a straight start). To arrive
  in step it would have to read the song half a start ahead of itself: a device is given no sound before it is
  played, and one that held the track back to make room would be late whenever it was idle.
- "Collect all and save": copying every file a song plays into a folder beside it. The song already says where
  its files are from its own folder and plays the ones beside it, so what is missing is the copying, a rule for
  two files of one name, and an edit that points the song at the copies. Only what a song plays of studies is
  gathered today.

## Where the tempo or the time signature changes inside a song

- A warped clip under a glide without one stretched copy of its file for every step of the glide, and without
  the three-millisecond fade where two copies meet, which a held tone can show.
- The length a sound dropped into a later section is given, which goes by the tempo the song begins at.
- The Clip tab's own picture of a recording that keeps its speed, which is drawn at the tempo where the clip
  starts.

## Plugins

- Hosting a plugin that plays in a process of its own. Scanning, the reading of presets, and every render,
  measurement and probe of a song with plugins are done in processes of their own. A plugin that plays live is
  in the program, so one that crashes while playing or while it loads a preset still takes the app with it; the
  safety copy is what saves the work. One such crash is known and worked around: Neural DSP's Archetypes take
  the process down when an instance is taken down before a message the plugin posted to itself has reached it
  while another instance is alive. An instance that is let go of is now kept, and taken down only when the
  plugin thread has delivered what was waiting and the plugin has had fifteen seconds of it.
- A preset browser for plugins in the window. The session lists, searches and loads them, and so does MCP; the
  device panel shows built-in and saved presets only.
- Asking a CLAP plugin for a stereo layout when its default is mono (a VST3 plugin is asked).
- Plugin editors on Linux and macOS.
- Finding files a plugin keeps in its own state. Only audio clips and the built-in sampler and drum rack name
  theirs to Trill.
- A loaded kit's own names in the window. The piano roll names a drum track's rows from the track's own map and
  General MIDI, not from what the plugin says is on its keys; over MCP every tool reads the kit.
- Keyswitches for notes drawn by hand in the window. The tools write them; a note that is drawn is a plain key.
- Kits of drum plugins other than Superior Drummer 3. None of the others that were tried says what is on its
  keys in a state Trill reads.

## The making-of and films

- Hearing a making-of in the window. It is silent there: the song as it was is shown, not played. A film of one
  has the finished song under it.
- Filming a making-of that was handed over as a file. A film is of the song that is open, whose finished state
  it ends on.
- Choosing the shots of a film in the window. The director's are used; over MCP they can be directed.
- Finding a song's journal again after its file was moved or renamed. It is kept by where the file is.

## The sampled instrument and its catalogue

- The four share-alike sets and the eleven instruments published only as 7z or tar archives (listed, not
  offered).
- Anyone's ears on which instruments are the best of their kind. The starter selection is by measurement.
- An archive gone on with from the middle. The sources make theirs anew each time.
- A set that changed at its source taken as the newer one. It is a sentence; the pin is moved by hand.
- The parts of SFZ that [Instruments and effects](instruments-and-effects.md) names as not done, and formats
  other than SFZ.
- A drum kit's microphones as separate levels. A kit that brings several is played as its file mixes them.
- Taking the instrument out of a device in the window. Another takes its place, or the device is removed.
- The time to a first note from a disk that has not read the files before.

## Studies of recordings

- Chords as something a song shows by itself. They are drawn along a reference, and written as notes.
- A drag between Trill's two windows tried with a real mouse.
- A study's models on anything but Windows. The models' own files are the same; the runtime and what drives a
  graphics card are pinned for Windows alone.
- A transcriber as large as Magpie's, and the six-stem model.

## The wavetable synth

- Sounds tuned by ear. The author has heard sixteen of its eighty-two; the others are held to measurements.
- Effects inside it. A preset that wants them brings them as devices.
- Making a table or naming a macro in the window. Both are done over MCP; the window chooses among the tables
  there are and shows the one that is played.
- A formant filter.
- Moving the unison's detune or an envelope's times from the matrix.
- Warps free of what a corner in the cycle adds. Sync, bend and squeeze leave 42 to 57 dB under the note what
  does not belong to it and is heard, two octaves over middle C; FM by a bright wave far more.

## Working in one song together

- A sampled instrument's recordings travelling with a shared song, and the other being told which set of the
  catalogue an instrument is from: they have its place, and a sentence that its file is not there.
- A list of the audio that arrived for shared songs and was never saved with one, with a way to remove it. It
  stays in Trill's own folder (`received`).
- Hearing a plugin one has not got where it is on a group, a return or the master: it lets through what comes
  into it. On a track that plays clips, whoever has the plugin records the track and the others hear that.
- A recording for a plugin someone has not got made smaller than plain audio (eleven megabytes a minute), made
  again only where the track changed, or brought beside the song at a save: it stays in Trill's own folder.
- A direct way between two programs that met through a meeting point, where there is one. Everything they say
  goes through it for as long as they are in the song.
- Letting go of someone who came into a song through another person, from anywhere but that person's Trill.
  Whoever shares lets go of who came in by their own invitation, and the invitation is another from then on.
- Being told why a clip someone else brought has no audio yet (missing at theirs, held there, or no audio): it
  is said to wait for whoever brought it.
- A meeting point that is encrypted as a web site is (TLS).
- A meeting point on ARM: nothing in the program is written for one processor, and nobody has built or run it
  there.
- A connection that is asked for again giving up by itself: it is asked for every half minute until one leaves.
- A view of what happened while one was away. The notices say where work clashed; the journal has every step
  with who made it.
- The assistant as a person of its own in the song. It acts as its user.
- Versions of a song that are kept apart and merged back.
- Throwing away what a shared song keeps of removed things and of when each property was set. Its file only
  grows.
- What a tool call is doing as something the others see. They see a person's selection, what a drag has hold
  of and where their song stands.
- The window's own drag of a note or a clip shown to the others as it moves in any way but by its result
  arriving.
- A journal that keeps what it keeps by the comparison everything else uses. It still has its own, coarser one;
  a step that arrives from someone else is entered under their name.
- A second machine, a second home, and *My network*. The meeting point has run on a real server and two Trills
  have met through it over the internet, with plugins on one side only, a restart of the server and one let go
  ([what it costs](working-together.md#what-it-costs)); both were on one computer behind one router. Sharing to a
  network has not been run once, because it asks the firewall a question that is its user's to answer.

## Sounds of more than one device

- What is in front of the instrument.
- A plugin that is not installed kept in the song until it is. The sound is made without it.
- Taking back the deletion of a saved one. It is asked about first, and is then for good.

## macOS

What Windows has and macOS has not, yet. Each is said where it would be used, never a crash.

- Plugin editors. Opening one says that they are not supported on this system yet. The plugins installed in
  `~/Library/Audio/Plug-Ins` and `/Library/Audio/Plug-Ins` are found, scanned and played.
- Plugins whose framework posts work to the process's main thread (JUCE's do), played live by `trill-mcp`, which
  runs no loop there: such work waits for good. One amplifier tried (Archetype Tim Henson X) is silent until it
  has run; in the window, whose main thread runs one, and in a render, which a plugin is told is offline, it
  sounds.
- Joining a device's audio workgroup. The threads that make a song's sound have the class macOS keeps for audio,
  which on what was measured is enough; a song that keeps more threads busy may want the workgroup as well.
- A plugin that takes a render down named by its file. The render says that its process went down, at which
  stage, and the song is as it was.
- The room on a disk, tried on a Mac. It is asked as on Linux, by a call every Unix has, and that was run on Linux
  alone.
- A study's models. The runtime and what drives a graphics card are pinned for Windows alone.
- Files dragged in from the Finder, tried with a real mouse. Where they will land is shown while they are dragged,
  from where the system says the pointer is, as on Windows.
- A name written in two Unicode forms (an accented letter as one character, or as a letter and its accent) taken
  for one name, as macOS's disks take it. Two spellings that differ in case are one, as there.
- An installer, a release and updates. A build of one's own neither looks for a newer Trill nor is replaced.
- A picture of the window (`--screenshot`) is taken in a window without a title bar, off every screen, by a
  program that has no Dock icon and never comes to the front.

## Linux

What Windows has and Linux has not, yet. Tried on Ubuntu 26.04 (GNOME on Wayland, PipeWire) on x86-64; what runs
there and how it was checked is in [Building and testing](development.md#on-linux).

- The audio threads in the class Linux keeps for audio. They run at an ordinary thread's priority (a built-in song
  of 14 tracks, 30 seconds handed to the system's output as silence: no buffer late), so a song that keeps up on an
  idle machine may fall short on a busy one; `keeps_up` says the class was refused.
- Plugins installed on Linux tried: CLAP and VST3 are looked for in their usual Linux folders, and none has been
  loaded. Plugin editors: opening one says that they are not supported on this system yet.
- Taking an output for Trill alone (exclusive mode). Asked for, it says the device cannot be taken exclusively on
  this system, and the device is shared.
- The trash. A recording that is removed goes to the `removed` folder beside the takes.
- How much memory the machine has free for a render or a probe in a process of its own, and what a thread ran. A
  study asks the machine, and measures and ends its own process.
- The assistant's processes ending with Trill when Trill itself is killed. They are one group, stopped together
  when a turn is stopped or Trill closes, as on Windows; a Windows job also ends them when Trill is killed.
- A study. The models' runtime is published for Windows alone so far: a study says that the part `runtime` is not
  published for this system yet.
- An installer, a release and updates.
- A picture of the window (`--screenshot`) is taken in a window that is never seen and never given the keyboard
  where there is an X server (a Wayland desktop has one beside it); without one the window is shown meanwhile.

## Installing and updating

- A certificate. The installer and the programs are not signed, and Windows says so the first time one is run.
- Anything but Windows on x86-64. A release has a build for each kind of machine it names, and names one.
- Starting again from the window. A newer Trill that was fetched goes in at the next start; the window does not
  close and start itself, and says to close every Trill.
- `trill-mcp` putting a newer Trill in place. It leaves that to the window's next start, and while a client keeps
  it running nothing goes in.
- Fetching only what changed. An update is the whole build.
- Going back to an older Trill from the window. The older version's installer does that.
- Taking a key out of trust in the Trills that are installed. A key that was built into a release is trusted by
  that release for as long as it runs; a second key can be added to the releases after it.
- An update between two published releases. The whole of it was tried on the real programs with builds made for
  the trial, served from the same machine; the first that crosses the internet is the one after 0.1.0.
