# Making the catalogue again

The catalogue in `catalogue/` is written by the scripts in this folder from three things: what a person read and
wrote down here, the sets as they are published, and what Trill's own player makes of them. Nothing in the
program runs these scripts and no test does: they are run by hand, by whoever adds a set, moves a pin or changes
what a mark takes. They fetch only when someone runs them.

## What it needs

- Python 3.11 or later (nothing beyond what it comes with), and `git`.
- This repository built far enough to run `cargo run -p trill-engine --example sampled`.
- A folder for the sets on a disk with room: 28 GB for all of them. Under `target/` is the place for it; never the
  repository itself, and never a sound library that is in use. `fetch.py` stops when the disk has less than 30 GB
  free.
- The network, for `fetch.py` only: it talks to github.com with git, and says nothing of who runs it.

## What is written down here

| File | What it holds |
|---|---|
| `pins.txt` | Each set's folder, its repository, the branch it was read on and **the commit it is pinned to**. |
| `sets.py` | What was read of each set by hand: its id and name, what it is in a sentence, who made it, the kind of its instruments, the credit line where its licence asks for one; how the two large libraries are split into sets; and `LEFT_OUT`, what was found and is not offered, with its terms and the reason. |
| `starter.txt` | The starter selection: a set a line, and which of its instruments are put first. |
| `listen-also.txt` | Instruments that are not in the starter selection and are played beside it in the listening files. |
| `archive-bytes.txt` | What each set's archive took the day it was fetched and checked: an estimate the program shows and asks the disk for. Written from the report of the test that fetches every set (below). |

Nothing that is measured, counted or summed is written by hand: sizes, checksums, ranges, layers, noise, tuning
and what the player does not do all come from the files.

## The steps

All from the root of the repository; `SETS` and `WORK` are folders of your own, for the sets and for what is
worked out of them.

```sh
python crates/trill-catalogue/make/fetch.py SETS WORK      # each set at its pinned commit; WORK/sets/<name>.files
python crates/trill-catalogue/make/measure.py SETS WORK    # every instrument through the player; WORK/facts/<name>.jsonl
python crates/trill-catalogue/make/build.py SETS WORK      # catalogue/*.toml and CREDITS.md; WORK/out for the ears
cargo test -p trill-catalogue                              # the catalogue that was written keeps its own rules
```

`fetch.py` and `measure.py` pass over what is there already, so a run that was stopped is gone on with;
`measure.py` takes about a hundred minutes for all the sets. `build.py` reads each set's licence file where the
set was fetched and **stops when the sentence that is quoted is not in that file word for word**; it stops too
when a set that asks for credit has no line, or a line and no such licence, and when a starter instrument has a
mark against it. It writes `WORK/out/everything-by-kind.md` (every instrument with what was measured, for whoever
is choosing) and `WORK/out/listening/*.list`, which
`cargo run -p trill-engine --example sampled -- tour LIST OUT.wav` plays into one file a kind.

Then prove that what a user fetches is what was read, through the program's own fetching (it uses the network;
26 GB and an hour or two for every set, one at a time, each removed when it has been checked):

```sh
TRILL_CATALOGUE_SETS=all TRILL_CATALOGUE_REPORT=WORK/fetched.tsv \
    cargo test -p trill-runtime --test instruments the_catalogue -- --ignored --nocapture
```

The report has a line a set: `ok` with what its archive took, or `FAILED` with why. The archive sizes go into
`archive-bytes.txt` (the set's id, a space, the bytes), and `build.py` is run once more.

## Adding a set

1. Read its licence where it is published, at the commit you will pin. It is offered only if the licence lets
   anyone hand it on: public domain, or asking no more than to be named (`catalogue/README.md` has the rules).
   If it does not pass, it goes into `LEFT_OUT` in `sets.py` with the sentence that decides it. Unclear is out.
2. Add its line to `pins.txt` and its entry to `sets.py`: for an attribution licence with the credit line, which
   names the work, its maker and its licence with the licence's address.
3. Run the steps. A set whose repository rewrites its text files on the way out (a `.gitattributes` with
   `eol=crlf`) is marked `by_file=True` in `sets.py`: an archive of it would not hold the files as they are, so it
   is fetched file by file.

## Moving a pin

`python crates/trill-catalogue/make/fetch.py --heads` says which branches have moved since they were read. A pin
is not moved without reading the set again, its licence file first: then change the commit in `pins.txt`, fetch
and measure that set again (`fetch.py SETS WORK NAME`, `measure.py SETS WORK NAME`), and build.

## Changing what a mark takes

The rules are at the top of `build.py` ("measured to marks") and in words in `catalogue/README.md`: change both,
and build. Nothing has to be fetched or measured again for that.
