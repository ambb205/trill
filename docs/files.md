# A song's files

**Files that moved**: a song names its audio files by their paths, and a folder of samples that is moved or
renamed leaves it with clips and pads that play nothing. Trill says so in one place (a line in the status bar and
the list it opens, `missing_files` in `get_project`, and first in the notes of a `measure` or a `render`, which
is then the song without those files and not a quieter mix), and looks for the files itself:

- *Where the folder went, then by name.* If one file that was at `A\x\y.wav` turns up at `B\x\y.wav`, everything
  else that was under `A` is looked for under `B`. Where a folder went is read from any file found by its name in
  the sound library, from a folder of the same name near where the missing one was, from a folder of the library
  that is itself missing and turns up with its files in it, and from what you point at.
- *A file of the same name can be another file.* Each thing found says how sure it is, and why: `sure` only where
  it is where the file's folder went or has the size and length the song wrote down, `likely` for the only file of
  that name with nothing against it, `unsure` when there are several, when it is not what the song knew, or when
  it is shorter than a clip plays. Only what is sure is taken by the one action; the rest is offered, and taken a
  file at a time.
- *Relinking is one edit*, one undo step however many clips and pads it touches (`Project::relink`,
  `relink_files`): each clip goes on playing the stretch of its file it played, each pad keeps its settings.
- *The library's own folders.* One that is gone is marked in the browser and listed (`library` status,
  `missing_folders`) in place of being silently stale; saying where it went (`move_folder`) keeps the favorites
  and the recently used files with their files and reads nothing again, and it goes along with the song's files
  when the same move explains both.
- *A song that travels.* A project file also says where each of its files is from the song's own folder, and
  what the file was (size, length, sample rate), in a `files` table beside the song. When a song is opened, **the
  file beside the song wins**: one that is found where the table leads from the folder the song is in now, and
  that is the file the song knew, is the one it plays; otherwise the path stands as it is. So a folder holding a
  song and its samples can be moved or sent whole. The table is an addition to the file, not a new format: a
  build that does not know it reads the song as before, and a file without one means what it always meant.

**Recordings.** A take is a 32-bit float WAV file at the engine's sample rate, one channel for one input and two
for a pair, one file for each track of each take with every pass of the loop in it (the takes are stretches of
it). A saved song's recordings are in `<song> files\takes` beside it. An unsaved song's are in a folder of
Trill's own (`recordings` beside the settings, a folder a run and a song) and **come beside the song when it is
first saved**, wherever the song names them: in a clip, a take, a sampler or a drum rack, in the song and in
every state undo can reach. On the same disk that costs nothing and no time: each file is given a second name
beside the song, the song is written naming it there, and the first name is taken away. Across disks the song
is saved at once, naming each recording where it is, and the recordings are copied afterwards, away from the
window; the status bar says how far that is, and when a copy is whole the song and its file name it beside the
song and the first copy goes. A crash at any moment of either loses no audio. A take that is still being
recorded when the song is first saved comes beside it when the take ends, and the recordings of unsaved work
that was recovered come beside it when that song is first saved.

**Saving under another name leaves the takes where they are**: `Song v2` goes on naming the files in
`Song v1 files\takes`. So the two songs share them, and a recording is "unused" only when neither plays it.

A take's file is written as it is recorded, and every second what there is of it is put through to the disk and
then the header brought up to date: a program that is killed in the middle of a take, or a machine that loses
its power, leaves a WAV file that reads, holding all but the last second or two, and the safety copy of the
song plays it. Undoing a take leaves its file; letting go of a take leaves its file. **Trill never deletes a
recording.** `File`, `Unused recordings` (the `unused_recordings` tool) lists the ones nothing plays: not the
open song or any state undo or redo can bring back, not another song Trill knows of (the ones it saved or
opened lately), not unsaved work that can be recovered. One that another song plays is listed with that song's
name and refused. The others are removed when that is asked for, in the window only after a second yes, and
removed means sent to the system's Recycle Bin, or into a `removed` folder beside the takes where there is no
bin; the question and the tool's reply say which. A take's recording is one of the song's files like any other:
listed when it is missing, looked for, and relinked.

**When a song opens with files missing**, the clips and pads that lost their file are marked, the status bar
says how many files are missing and how many were found, and a click on that line (or `File`, `Missing files`)
opens the list: each file with where it is used and what was found for it. `Relink` takes everything that was
found for sure, as one undo step; `Use` takes a file that is only likely, one at a time; `Locate` points at a
file by hand and `Look in a folder` at a folder to search, and what either says about where a folder went finds
the rest. A folder of the sound library that is gone is listed there too (and marked in the browser), and is
pointed at its new place along with the song's files.
