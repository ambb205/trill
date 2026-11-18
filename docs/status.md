# Status

Trill is an early build. Everything the other pages describe works and is held by tests. This page lists what is
not built yet.

Only Windows has been run so far.

## Arrangement and engine

- A session (clip-launching) view.
- Audio recording. MIDI is recorded; audio is not.
- Polyphonic aftertouch and MPE (per-note expression). A clip holds one curve per controller for the whole
  channel.
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

- A plugin's position in seconds and the start of its bar. It is told the tempo, the time signature and the beat
  of each block; the seconds and the bar line it is given are still worked out from those alone, so after a
  change they are not the song's, and through a glide the seconds do not run evenly.
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

## Sounds of more than one device

- What is in front of the instrument.
- A plugin that is not installed kept in the song until it is. The sound is made without it.
- Taking back the deletion of a saved one. It is asked about first, and is then for good.
