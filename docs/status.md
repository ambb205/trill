# Status

Trill is an early build. Everything the other pages describe works and is held by tests. This page lists what is
not built yet.

Only Windows has been run so far.

## Arrangement and engine

- A session (clip-launching) view.
- Recording from more than one input device at once. A song records one input device, with all its channels,
  beside the output it plays through; the two may be different devices on different clocks.
- An input and an output on two real clocks, measured. The engine follows the two clocks and its tests hold it
  to a click within half a millisecond over half an hour at 200 parts in a million; on hardware it has been run
  through a virtual cable only, whose two ends share one clock. See [the engine's page](engine.md).
- Saying that an input and an output are one device. Where the system says so (an interface with its own
  input and output), a frame of the input is a frame of the song, to the bit. Where it says nothing, as of a
  built-in device or a virtual cable, Trill reads the input by its time stamps as it would a second clock: a
  take then lands within a frame or two and is not a copy of what was heard to the bit. A user cannot yet say
  "these two are one device"; a test can.
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

- Audio files that travel between machines. Each person needs the files where the song names them.
- Hearing a plugin one has not got (a stem from whoever has it). Its place is kept and its settings are carried.
- A meeting point for two home networks. A program joins wherever it can open a connection: the same machine, a
  local network, a VPN, a forwarded port.
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
- A second machine, and *My network*. Everything was run with both programs on one computer, over its
  loopback; sharing to a network has not been run once, because it asks the firewall a question that is its
  user's to answer.

## Sounds of more than one device

- What is in front of the instrument.
- A plugin that is not installed kept in the song until it is. The sound is made without it.
- Taking back the deletion of a saved one. It is asked about first, and is then for good.
