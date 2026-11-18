# The catalogue of sampled instruments

Trill ships no recordings. This folder is text: the sampled instruments that other people publish under licences
that let anyone use them and hand them on, each with what it is, its licence in its maker's own words, whom to name
for it, where it is fetched from, and what it takes to know that what arrived is what was read. A user fetches what
they choose from where its maker keeps it. The text is built into the program (`crates/trill-catalogue`), which
reads it whole or not at all: an entry that breaks a rule below is an error that names it, and a test reads the
catalogue that is here.

**No recording is ever added to this folder, or to the repository.** Not a sample, not a part of one, not an
instrument's `.sfz` file.

| File | What it holds |
|---|---|
| `instruments.toml` | The sets and their instruments. |
| `files.toml` | For each set that is fetched file by file: every file, with its checksum and size. |
| `left-out.toml` | What was found and is not offered, with its terms as they were read and the reason. |
| `CREDITS.md` | The lines of the makers who ask to be named. A test holds it to `instruments.toml`. |

## What may be in it

A set is offered only when its licence lets anyone **hand it on**, for any purpose, and its maker's terms were read
at their source:

- dedicated to the public domain: CC0 1.0, The Unlicense;
- asking for no more than to be named: CC BY 3.0, CC BY 4.0. Such a set has a `credit` line, and the program shows
  it wherever the set is offered and says it for a song that plays one of its instruments.

Everything else is left out, and `left-out.toml` says why in one of five words:

| `reason` | What it means |
|---|---|
| `share-alike` | Its licence asks that what is made from it be shared under the same licence (CC BY-SA, the GPL). Listed apart: whether a song made with such an instrument is "made from it" is the owner's to decide, not a program's. |
| `non-commercial` | It may not be used, or handed on, for money. |
| `own-terms` | Its own terms forbid handing it on, or passing it on in a collection. Free to use is not free to hand on. |
| `unclear` | It says no licence, says two that disagree, or says one that cannot be told. Unclear is out. |
| `form` | Its licence passes, and it is published only in a form the program does not fetch (see below). |

A licence is quoted, never summed up: `licence_says` is a sentence of the licence file as it stands at
`licence_at`, which is an address at the pinned version, so that what was read can be read again.

## A set

A set is what is fetched as one thing: a maker's set at one version.

```toml
[[set]]
id = "small-grand"            # the name of its folder: lower-case letters, digits and dashes
name = "Small Grand"
about = "A grand piano recorded in a small room."
by = "A. Maker"
licence = "CC-BY-4.0"         # CC0-1.0, Unlicense, CC-BY-3.0 or CC-BY-4.0, and nothing else
licence_at = "https://…/blob/<version>/LICENSE"
licence_says = "…the sentence of it that allows this, word for word…"
credit = "Small Grand by A. Maker, licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)"
from = "https://…"            # where it is published
version = "<the commit that was read: 40 hexadecimal digits>"
read = "2026-11-17"           # the day its licence and its files were read
bytes = 123456789             # what all its files take, and how many there are
files = 321
[set.fetch]
…                             # one of the two ways below
```

`credit` is there exactly when the licence asks for one. It names the work, its maker and its licence with the
licence's address, so that it can stand alone wherever it is pasted.

### How it is fetched

**As one archive**, where the whole of a repository at one commit is the set:

```toml
[set.fetch]
archive = "https://github.com/<owner>/<repository>/archive/<version>.zip"
under = "<repository>-<version>/"     # what in the archive is the set: everything below this
sum = "<the tree checksum of the set's files>"
archive_bytes = 123456789             # what the archive took the day it was fetched and checked: an estimate
```

Such an archive is made by its source each time it is asked for, so its own bytes and its exact size cannot be
pinned: two fetches of the same commit may differ as files and unpack to the same thing. What it unpacks to is
pinned instead, and `archive_bytes` is only what the program shows and asks the disk for. For the same reason
such an archive is never gone on with from the middle: a fetch of it that was stopped begins again (measured:
its source does not go on from the middle either). A set that is fetched file by file is gone on with. The **tree checksum** of a folder is the SHA-256 of one line for each file below it,

```
<the file's SHA-256 in hexadecimal><two spaces><its path, with forward slashes><a line feed>
```

with the lines in the order of their paths as bytes. Two files in a set's folder are not the set's and are passed
over: `part.json` (the record the program's fetching keeps there) and anything that ends in `.partial` (what had
come of a file when a fetch was stopped).

**File by file**, where a set is a part of a larger library, or where an archive would not hold the files as they
are (a repository that asks for its text files to be rewritten on the way out):

```toml
[set.fetch]
base = "https://raw.githubusercontent.com/<owner>/<repository>/<version>/"
list = "small-grand"                  # the name of a list in files.toml
```

and in `files.toml` a line for each file, `<SHA-256> <size in bytes> <path below the set>`. A file's address is
`base` followed by its path with every character an address cannot hold written as `%XX`. Paths hold spaces,
commas, `#` and letters that are not ASCII.

**From another place later.** A set may list mirrors: archives that somebody keeps, whose own bytes are pinned.
None is listed today.

```toml
[[set.mirror]]
url = "https://…/small-grand.zip"
sha256 = "<the archive's own SHA-256>"
bytes = 123456789
under = ""
```

Only zip archives are fetched. Eleven public-domain instruments that are published only as 7z or tar archives are
in `left-out.toml` as `form`.

### What it takes to know a set arrived

`Set::check(folder)` (`crates/trill-catalogue/src/check.rs`) reads every file: for an archive the tree checksum of
the folder, for a list each file's size and checksum. Its error is a sentence that tells apart a set that is not
all there (fetch it again; what is there is kept), one that is not what was read (replaced at its source, changed
on the disk, or damaged on the way: the files are named) and one whose folder holds something else. A set that
changed at its source therefore never arrives quietly: its entry is read again by a person and pinned again, and
until then the fetch says what changed.

## An instrument

One file of a set that the Sampled Instrument plays (`builtin:sampled`):

```toml
[[set.instrument]]
file = "Grand.sfz"            # below the set's folder
text = "<the SHA-256 of that file>"
name = "Small Grand"
kind = "piano"
words = "Grand piano, close and dry, four layers of loudness"   # a sentence a search finds it by
range = [21, 108]             # the lowest and highest key that sounds
drums = true                  # only where its keys are drums and not pitches
layers = 4                    # the most layers of loudness a key has
takes = 1                     # the most recordings of one note at one loudness
recordings = 352              # how many recordings it plays, and what they take
bytes = 123456789
ways = ["sustain", "staccato"]        # the ways of playing a key of it chooses
noise = [70.0, 55.0]
tuning = [2.0, 3.0, 9.0]
evenness = [1.5, 4.0]
level = -12.0
marks = ["starter"]
misses = "…"                  # what its file asks for that the player does not do, in the player's sentence
note = "…"                    # why a figure is not given, or what else there is to know
```

`text` is how an instrument on a disk is known again, wherever its folder was put and however it came there: that
is how the program says which instruments of the catalogue are in the sound library, and what a song owes. One
file is one instrument: a file that is, to the byte, one that is listed already is not listed twice. A file that
others include is a part and not an instrument, and an instrument none of whose recordings are in its set is not
listed.

The kinds are `piano`, `electric-piano`, `organ`, `keyboard` (harpsichords, celestas, accordions and other keys),
`guitar`, `bass`, `strings` (bowed), `brass`, `woodwind`, `voice`, `mallets` (struck bars and bells that play
pitches), `plucked` (harps, zithers, banjos), `drum-kit`, `percussion` (what is struck and is not a kit), `synth`
and `effects`.

### What was measured, and what a mark takes

Every instrument was loaded and played through the player
(`cargo run -p trill-engine --example sampled -- facts FOLDER OUT.jsonl`: a line of JSON for each instrument file
below a folder), and what was measured is in its entry. A mark is a rule on those figures, so that a noisy or badly
tuned instrument is offered and not put forward: a search leaves the marked ones out until it asks for them, and
`starter` is given only to an instrument with no other mark.

| Figure | What it is | Where it means nothing, and is not given |
|---|---|---|
| `noise` | How far under its peak the quietest twentieth of a second of a recording lies, in dB: for the middle recording, and for the noisiest tenth of them. | An instrument whose notes are held or looped (bowed strings, winds, organs, pads): such a note never falls quiet. |
| `tuning` | How far from equal temperament its recorded keys between C2 and C7 are, in cents: the middle one (the median), how far a key usually lies from that (the median of the distances), and the worst. Middles, so that one key whose pitch was read wrongly decides nothing. | A kit, percussion, effects: their keys are different sounds. Fewer than five keys. |
| `evenness` | How the peak steps from one recorded key to the next, played at one loudness through the player, in dB: the usual step (the median) and the most. | The same, and an instrument that was silent. |
| `level` | The peak of what was played, at the device's own volume, in dB. | |

| Mark | It is given when |
|---|---|
| `starter` | It is among the best of its kind that were found, and nothing below speaks against it. Chosen by a person. |
| `noisy` | The quietest part of its middle recording lies less than 40 dB under that recording's peak. Judged only where keys are pitches: a struck sound is often cut while it still rings, and its figure is given without a mark. |
| `out-of-tune` | Its middle key lies more than 25 cents from equal temperament, or half its keys lie more than 15 cents from the middle one. Not judged for mallets and bells (their partials are not harmonics, and the pitch read from the wave is not always the one that is heard) or for voices (a voice wavers about its note): their figure is given, and `note` says so. |
| `uneven` | The usual step from one recorded key to the next is over 6 dB: at every other key the level doubles or halves. (Recordings of orchestral instruments that were left at their natural levels step by 3 to 4 dB, and are not marked for it.) |
| `silent` | All that was played peaked under −60 dB. `note` says when it is only waiting: an instrument whose every note asks for a controller to have been moved plays once one is. |
| `incomplete` | Its file names recordings that are not in its set, or that are not audio the player reads. |
| `unfaithful` | More than a quarter of what it plays is made by something the player does not do (a wave the instrument asks its player to make): what is heard is not what its maker meant. |

The figures are measurements, not judgements of how an instrument sounds: nobody has chosen by ear. A tuning
figure of an instrument that is played with vibrato, or whose low notes are short, is less sure than it looks.

### The starter selection

What is offered first, ticked: the best of each kind a song commonly needs (a grand piano, electric pianos, the
string sections of an orchestra, an acoustic and an electric bass, a drum kit, guitars, an organ, hand
percussion), 1.8 GB in twelve sets. It is the sets that hold an instrument marked `starter`: a set is fetched
whole. "Everything" is the other choice. Which instruments are in it is written down in
`crates/trill-catalogue/make/starter.txt`; it was chosen by what was measured, and nobody has chosen by ear.

## Adding a set, or bringing one up to date

The catalogue is written by the scripts in `crates/trill-catalogue/make/`, from what a person read and wrote down
there (which commit each set is pinned to, what it is, whom to name, what was left out and why), from the sets as
they are published and from what the player makes of them. `crates/trill-catalogue/make/README.md` says what
they need and gives the steps: fetch the pinned commits, measure every instrument through the player, build,
and then prove through the program's own fetching that what a user gets is what was read. Nothing in the program
runs them, and no test does.

Two tools for looking at one set by hand: `cargo run -p trill-catalogue --example sets -- sum FOLDER` prints a
folder's tree checksum, its files and bytes, and the checksum of every `.sfz` file in it;
`… -- check SET FOLDER` checks a folder against the catalogue's entry.

When a set changed at its source, do not move the pin without reading it again: its licence file first.
