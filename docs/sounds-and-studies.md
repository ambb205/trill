# Sounds, listening to a recording, and studies

**Sounds**: a library of the sample folders you point it at, scanned in the background and searchable by words,
tags, tempo, key and length (all read from file and folder names). Audio clips play WAV, AIFF, FLAC, MP3 and Ogg
Vorbis at any sample rate, and can be warped to follow the project tempo: `beats` keeps drum hits sharp, `tonal`
keeps sustained sound smooth, `repitch` plays faster or slower like a record; warped clips can be transposed.

**Listening to a recording.** Trill says what is in an audio file, with no model and nothing changed: the
**beat grid** (every beat and bar line, each beat moved onto the hit it belongs to, and the tempo), how far the
recording sits from A = 440, its **key** with the runners-up, a **chord** for each stretch of beats, its **parts**
lettered so that parts that return share a letter (`A B A B C B`), the **loops** worth having ranked by how well
they come back round, the cleanest **single hits** of each kind of drum and a bass note for each pitch, the drum
**groove**, and the **notes** of a bass line and of a melody. Any of the last four is written as **MIDI on the
recording's own beats**, so it stays in time where the recording's tempo wanders: into a file, or into the song on
a new track. When the tracker has the beat wrong it is reread: half or double the tempo, the bar one beat later,
or so many beats to a bar; everything else follows.

- It works on one file and better on one instrument. A drum loop's groove, a bass line's notes and a voice's
  melody are right; of a whole mix the groove is a sketch (the bass plays in the kick's band), and bass and melody
  are what was loudest in their registers. Chords and parts are estimates: a map, not a transcription. Without a
  model the start of the bar is the least sure thing in an answer.
- Over MCP today (`listen`, `transcribe`); the window does not show it yet. Of a whole mix, a *study* (below)
  hears the same things in its stems, where they are right.
- The method is the one the author's Magpie project proved, translated from its Python (`crates/trill-listen`). On
  five recordings Magpie had taken apart, over its own beat grids, Trill finds the same parts, 313 of its 316
  loops to the bar, 187 of its 189 one-shots, its groove and every bass and vocal note; chords are the same 52 to
  84 % of the time (11 % on one recording whose chords Magpie has in another key), read from a chroma of Trill's
  own.

**Studying a recording.** Trill takes a recording apart with models that run on your own machine, and keeps what
it found as a *study*: the **stems** (vocals, drums, bass and the rest, which add up to the recording), the
**beats and bar lines** as a model hears them, the **words** that are sung, in sung lines, with a time and a note
for every word, and then everything listening finds (key, chords, parts, loops, single hits, the groove, the bass
line, the melody), heard in the stems, where it is right of a whole mix too. A study is made once and kept:
reading it again runs nothing. A stem is an audio file like any other, and goes on a track.

- **The models are not part of Trill, and Trill hosts none of them.** File, Setup lists each part there is (the
  runtime, what drives a graphics card, each model) with its size, its licence in the words of its source and
  where it comes from, and fetches a part when you press the button under those: with the system's own `curl`,
  from where its makers publish it, into a file that is kept only if its checksum is the one Trill was made for.
  A fetch can be stopped and is gone on with from where it was. Nothing is fetched unasked, by a test or for a
  picture, and the assistant in the window cannot fetch at all. The stem model's licence is unclear, in its
  author's own words, and the panel says so before you fetch it. Everything a study uses is 3.1 GB for a
  graphics card and 2.6 GB for a processor; the four models of the best stems are 643 MB more.
- **What runs the models is measured, not assumed.** A timed test splits the same half minute of noise on the
  processor and on every graphics card, each in a process of its own, and takes the fastest that gave the same
  stems. On the machine this was written on (a Ryzen 7 7800X3D, an RTX 4080 SUPER and the processor's own
  graphics) a minute of music is split in 19 s by the processor, in 7 s by the 4080 through the part that drives
  a card of any make, in 3 s through NVIDIA's own libraries (an optional part of a gigabyte, under NVIDIA's
  licence), and in 69 s by the integrated card, which is therefore not used though it is there. Until the test
  has run, a study runs on the processor.
- **A study runs in a process of its own**, as a render of a song with plugins does: a model's runtime that
  faults costs a sentence that names the stage, and every stage that had finished is kept; a study the machine
  has not the memory for is refused before it starts, and one that takes more than it was given is ended. It can
  be stopped at any moment and is taken up at the stage it was in. A song of four minutes and a half took 30 to
  37 s in all on the 4080 through NVIDIA's libraries (5.5 GB of memory at the most), 50 s through the part for
  any card (6.1 GB) and 142 s on the processor alone (3.1 GB). On the integrated card the same study took
  eleven minutes. A recording of fifteen minutes, the longest that is studied, took 101 s and 8.7 GB through
  NVIDIA's libraries, 152 s and 7.3 GB through the part for any card, and eight and a half minutes and 7.0 GB
  on the processor. A run says beforehand what it will need, by what runs the models and by how long the
  recording is, and that is what it is refused or ended by.
- **Where the words are written down is where a model hears a voice.** A transcriber makes words of whatever it
  is handed, and a vocal stem is not silent where nobody sings. So a small model that tells a voice from
  everything else cuts the stem first (as Magpie's transcriber does), and a word with nothing sung under it is
  left out. Without that cut, on two of four recordings the transcriber wrote down twice the words that are
  there, one phrase thirty times over.
- **Against what Magpie found** in the five recordings it took apart, on this machine. *Stems:* as near to the
  ones Magpie kept as Magpie's own second run is (its model shifts the recording by a random amount, so two of
  its runs differ): 27 dB for the vocals of one recording where Magpie against itself has 26 to 27, 19 for the
  drums where it has 18, 24 for the bass (22 to 23) and 21 for the rest (20 to 21). *Beats:* on the two
  recordings Magpie tracked with the same model, the same beats and bar lines (0.998 and 1.000 of them, where
  listening without a model has 0.81 and 0.96 of the beats and 0.63 and 0.92 of the bar lines). *Words:* Magpie
  uses a larger transcriber; on the recording both hear best, 276 of its 285 words are the same and begin
  about 20 ms from where Magpie has them at the median, nine in ten within a fifth of a second; on the three
  others 83, 86 and 68 % of its words are the same, and the last of them, which is sung in two languages, came
  out with 326 words on one run and 359 on the next. Neither writes a word for a recording nobody sings in.
- **A recording behind a link** is fetched by a program you installed (`yt-dlp`, which needs `ffmpeg`), found
  when it is wanted and never shipped, into the study's own folder. It is yours to say what you may study: the
  tool says so once, and the assistant in the window is not handed links at all.
- **A study is a folder** in Trill's own data folder, which File, Setup moves to a disk with more room, as it
  does what was fetched: about 60 MB for a minute of music, nearly all of it the four stems, which are kept as
  24-bit WAV. They are not packed: the one small FLAC encoder there is in Rust made 24-bit stems larger than
  they were. Every answer about a study and the Setup panel say what the studies take.
- **One study at a time on the machine**, whichever program asks: the window and a headless server take
  turns over one file, and the one that waits says so.

**A study in the window.** The Studies place of the browser lists every study, the ones being made with the
stage they are at, a bar that fills, Stop and "take up again"; a failure is a sentence under the study's name.
*Study a recording* (there, and in the File menu) asks for a file or a link in one field, and says whose a
recording behind a link is before anything is fetched. A study that is done opens in the **Study tab** along the
bottom: the whole recording small with its parts, then under the recording's own bars its parts, its chords and
its words where they are sung, and its stems as lanes with mute and solo. Beside the lanes is what was found:
loops best first (with whether a part plays alone there), single hits as pads, the sung lines, and the bass
line, the melody, the chords and the groove as notes.

- **A stretch is selected across the lanes**, its ends on bar lines, beats or wherever (Snap), and *played
  round*: made by the plan that taking it follows and rendered by the code that plays the song, so what is
  heard is what would be taken. It is heard in the song's tempo, in time with the song while the song plays,
  and, where asked, in the key the song says it is in; *As recorded* hears and takes it untouched.
- **It goes to the song** by being dragged onto a lane (or below the tracks for a new one), or with *To the
  song* or Enter onto the selected track at the edit cursor. A loop, a line (named by its words) and a part as
  notes are dragged from their lists the same way; a hit goes onto a lane, a pad of a drum rack or a sampler.
  One undo step each.
- **A window of its own** (the Window switch) shows the same workspace beside the song or on another screen,
  and a drag crosses from it into the song: the window the button went down in keeps the pointer, and Trill
  works out where that is in the other from where the two stand on the desktop. This is proved without a real
  mouse (the test runs the two as one, and the arithmetic is tested by itself): if it misbehaves on a machine,
  *To the song* and Enter do the same.
- **What a stretch becomes** is an audio clip on the stem's own file that follows the song's tempo, a beat of
  the recording for a beat of the song: transients kept for drums, sustained sound kept smooth for everything
  else. Several stems together are added up into one file in the study's folder. A recording whose tempo moves
  is laid in as many clips as keep every bar line within 12 ms, cut at bar lines; taking the study's tempo
  into the song first keeps it in one, untouched. Moving it to another key is the stretcher's work: a voice's
  formants move with the pitch, so a couple of semitones pass and beyond three or four a voice no longer sounds
  like itself. Drums are never moved for a key.
- **A stretch plays round.** An audio clip can play a stretch of its recording again and again, as a notes clip
  plays its loop: drawn out in the arrangement it repeats, in time with the song and through its tempo changes,
  each round faded into the next over a few milliseconds, and its start drawn out comes round from the loop's
  end. A stretch or a loop taken from a study is such a clip (`times`, over MCP, says how many rounds long). Any
  audio clip has the switch (Loop, in the Clip tab: what the clip plays now becomes what repeats), and
  `update_clip` sets it with `loop_length` in beats, as for a notes clip. A file saved before clips could plays
  as it did.
- **A recording's bars are one thing.** Each bar is as long as it was found: a model that hears beats finds a
  bar of two among bars of four at some transitions. `listen`, `take_from_study`, the workspace's ruler and the
  tempo taken into a song all count the same bars, so bar 60 is the same place wherever it is named; `listen`
  with show "bars" says of a bar that is not as long as the rest how many beats it holds.
- **The grid can be read another way** (half, double, the bar a beat later, three, four or six beats to a
  bar): what follows from it is heard again without a model. **A word can be put right** by double-clicking it
  where it is sung; its time stays.
- **As a whole** a study gives the song its tempo (bar by bar through the tempo map, a steady recording as one
  tempo, a bar the model found shorter as a short bar with its own time signature; nothing in the song moves),
  its parts as markers, its key, a drum rack of its cleanest hit of each drum, and the recording itself as a
  **reference**.
- **The ruler stays readable** where a recording's tempo has put many changes close together: a marker's name
  is written whole or left out, a change of tempo or time signature says as much of itself as it has room for
  (its bar and what changes, what changes, or its line alone), and the pointer says the rest in the status bar.
- **A reference track** is a recording beside the song: silent unless it is listened to (the L in its header,
  where other tracks have mute and solo), heard in place of the song while it is, and in no render, stem, film
  or length of the song, whatever is being listened to. Its study's parts, chords and words are drawn along its
  clip. Any audio track can be made one from its menu. Measuring with only that track named measures the
  recording itself, which is how a mix is held against it.
- **A song says its key** (`set_project`, or taken from a study): nothing follows it, and it is what a sound
  from a study is moved to.
- **A study's files are the song's files.** A clip taken from a study plays a file in Trill's own folder, so it
  is listed, found missing and relinked like any other. Removing a study, or dropping its stems to make room
  (the Studies place, Setup, or the `study` tool), first says which of the songs Trill saved or opened play
  them, and asks. *Gather what the song plays of studies* (File menu, or `study` with action "gather") cuts
  what the open song uses, with a second to spare on each side, into a folder beside it and has the song play
  it from there: for a song that goes to another machine. Trill has no general "collect all and save" yet.
- Everything above is one tool for an assistant, `take_from_study`, over the same code the window uses.
