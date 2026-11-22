#!/bin/sh
# Puts a release build of Trill into one folder to hand to a friend: the
# programs, the licences, and a page that says what their computer needs
# and how to get into your song.
#
#   sh scripts/friend-bundle.sh              # builds, then gathers into target/friend/Trill
#   sh scripts/friend-bundle.sh OUT          # into OUT instead
#   sh scripts/friend-bundle.sh --no-build   # gathers what is built already
#
# `trill.exe` starts no program but itself (it is started a second time to
# scan plugins, to render and to study), so the folder is two files and
# what is written about them: `trill.exe`, and `trill-mcp.exe` for whoever
# wants an AI client to work in the song without the window.
#
# Nothing of yours goes in: no settings, no meeting point, no key, no song.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
build=yes
out="$root/target/friend/Trill"
for word in "$@"; do
    case "$word" in
        --no-build) build=no ;;
        *) out=$word ;;
    esac
done
built="${CARGO_TARGET_DIR:-$root/target}/release"

if [ "$build" = yes ]; then
    (cd "$root" && cargo build --release -p trill-app -p trill-mcp)
fi
for program in trill.exe trill-mcp.exe; do
    if [ ! -f "$built/$program" ]; then
        echo "friend-bundle: $built/$program is not built: run without --no-build" >&2
        exit 1
    fi
done

rm -rf "$out"
mkdir -p "$out/licences"
cp "$built/trill.exe" "$built/trill-mcp.exe" "$out/"
cp "$root/LICENSE" "$out/licences/Trill (GPL-3.0-or-later).txt"
cp "$root/crates/trill-noir/assets/fonts/LICENSE-Inter.txt" "$out/licences/Inter typeface (SIL Open Font License).txt"
cp "$root/catalogue/CREDITS.md" "$out/licences/Sampled instruments Trill can fetch (credits).md"

# What Trill is built from, each with the licence its makers give it under.
{
    echo "Trill is built from these libraries. Each is its makers', under the licence beside its name;"
    echo "the texts of those licences are with each library's source, which its name and version find at crates.io."
    echo
    (cd "$root" && cargo tree --offline -p trill-app -p trill-mcp --edges normal --prefix none --format "{p} | {l}") |
        sed -e 's/ ([^)]*)//' | grep -v '^trill-' | sort -u
} > "$out/licences/Libraries Trill is built from.txt"

commit=$(cd "$root" && git rev-parse --short HEAD 2>/dev/null || echo unknown)
cat > "$out/READ ME.txt" <<EOF
Trill, a build to work in one song together (build $commit)

Start trill.exe. Windows may ask whether to run a program it does not
know: it is not signed by anyone. Trill keeps its settings in your own
folders and changes nothing else on the computer.

To get into a song someone shares with you: File, Join a song, paste the
invitation they sent you (one line that begins with trill:), Join. That is
all: the invitation says where to go. If their Trill shows a meeting point
you need no key and no address of your own, only the invitation.

What your computer needs, for now:

- The same plugins as theirs, installed where plugins are installed. A
  plugin you have not got stays in the song as a place that is kept, with a
  sentence that says so; it is silent for you and sounds for them.
- The same audio files in the same places. A song names its recordings by
  where they are on the computer it was made on. A file that is not there
  is listed as missing (File, Missing files) and is silent for you; if you
  have it somewhere else you can say where. Recordings do not travel
  between the two computers yet.
- The Microsoft Visual C++ runtime, which most computers have. If Windows
  says VCRUNTIME140.dll is missing, install "Microsoft Visual C++
  Redistributable (x64)" from Microsoft.

If the connection drops, do nothing: Trill asks for it again by itself and
says so at the bottom of the window, and what each of you did meanwhile is
merged when it is back. Each of you has the whole song and can save it.

trill-mcp.exe is the same program without the window, for an AI client
that speaks MCP.

Trill is free software under the GNU General Public License, version 3 or
later (licences folder). Whoever gave you this build has to give you its
source if you ask: it is build $commit of Trill.
EOF

echo "friend-bundle: $out"
ls -l "$out" "$out/licences"
