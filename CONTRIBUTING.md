# Contributing to Trill

Thank you for wanting to. Trill is an early build, so the most useful things are reports of what breaks and
changes that keep to its three goals: it does not crash or stall, it hosts plugins, and everything it can do is
reachable over MCP.

## Before you send a change

```sh
cargo test --workspace                    # about 2,700 tests; needs nothing installed
cargo clippy --workspace --all-targets    # must be clean
cargo fmt --all -- --check                # rustfmt.toml: 120 columns
```

The default test run reads nothing of yours and needs no plugin, audio device or network. Tests that need a
particular machine are skipped unless asked for; [Building, testing and architecture](docs/development.md) lists
them with the command for each.

## Where a change goes

[`CLAUDE.md`](CLAUDE.md) is the map: what lives in which crate, where each kind of change goes, and the
conventions that are particular to this repository. The ones that decide most reviews:

- **An edit is a method on `Project` first**, with tests in `trill-core`; then an MCP tool; then the window. An
  edit that exists only in the window cannot be reached by an AI.
- **The threads that play a song never allocate, take a lock, or sleep on anything another thread can hold.**
  `tests/realtime.rs` in `trill-engine` and `trill-plugins` enforce it on every one of them: extend them when you
  touch what an audio thread runs.
- **A song saved before must sound as it did.** A change to how a built-in device sounds comes with a file of
  the older format in `trill-engine/tests/old_songs.rs`.
- **A failure is a sentence, not a crash.** A device that vanishes, a plugin that refuses, a file that is gone:
  each ends in a message the user can read, and the song keeps playing.
- **Tests go through the real path** and are named as sentences. Engine tests render audio, app tests drive the
  window, MCP tests talk to a real server.
- **Nothing of anyone else's is in the repository**: no plugin maker's preset, no recording, no model. Tests
  build their own.

A feature is described in the page of its topic under [`docs/`](docs), and in [Status](docs/status.md) when part
of it is not built. The README stays short: what Trill is and how to start.

## Licence

Trill is under the GNU General Public License, version 3 or later. By contributing you agree that your
contribution is under the same licence. Dependencies must be compatible with it.
