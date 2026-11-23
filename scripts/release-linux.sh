#!/bin/sh
# Makes a release of Trill for Linux on x86-64, into target/release-out/<version>:
#
#   trill-<version>-linux-x86_64.zip     Trill's two programs, its READ ME and its licences: what
#                                        people unpack (into a folder of its own) and what an
#                                        unpacked Trill fetches to update itself
#   build-linux-x86_64.json              what that archive is, unsigned: the machine that has the
#                                        release key announces it with the other systems'
#   SHA256SUMS.txt
#
#   sh scripts/release-linux.sh                  # builds and gathers
#   sh scripts/release-linux.sh --no-build       # gathers what the last run built
#   sh scripts/release-linux.sh --changed        # from a tree with changes that are not committed
#   sh scripts/release-linux.sh --out DIR        # into DIR instead
#
# The version is the workspace's, in Cargo.toml. The build is a release: it says its version and
# commit, names no folder of the machine it was built on, asks for no library but the few every
# Linux desktop has (the rest of what it uses, it looks for when it runs), and is built apart from
# everything else (target/dist). What it needs of the C library is said at the end, and kept in
# the folder (`needs.txt`).
#
# The release key is not here: no announcement is signed here. The machine that has it takes the
# archive and the description (`cargo run -p trill-update --example release -- describe`, which this
# script ran, and which reads nothing but the archive) and announces every system's build together.
#
# It publishes nothing and pushes nothing.
#
# A trial of the updater, on this machine and with nothing published (scripts/update-trial-linux.sh):
#
#   sh scripts/release-linux.sh --trial KEYFILE [--trial-version X.Y.Z] [--trial-from http://127.0.0.1:8765/]
#
# builds a Trill that trusts the key in KEYFILE beside its makers', says the version given, and looks
# where TRILL_UPDATE_ADDRESS says when it runs. Its announcement is signed with that key and names
# the archive below the address given. Such a build is never published.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
here=$(pwd)
whole() {
    case "$1" in
        /*) printf '%s' "$1" ;;
        *) printf '%s' "$here/$1" ;;
    esac
}
cd "$root"

if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
    echo "release-linux: this makes Trill for Linux on x86-64, on such a machine" >&2
    exit 1
fi
for needed in readelf objdump sha256sum; do
    if ! command -v "$needed" > /dev/null; then
        echo "release-linux: $needed is not on this machine, and the build is checked with it (binutils, coreutils)" >&2
        exit 1
    fi
done

build=yes
changed=no
trial=
trial_version=
trial_from=http://127.0.0.1:8765/
out=
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) build=no ;;
        --changed) changed=yes ;;
        --trial) trial=$(whole "$2"); shift ;;
        --trial-version) trial_version=$2; shift ;;
        --trial-from) trial_from=$2; shift ;;
        --out) out=$(whole "$2"); shift ;;
        --key) echo "release-linux: the release key stays on the machine that announces releases" >&2; exit 1 ;;
        *) echo "release-linux: '$1' is not something this script knows: see its first lines" >&2; exit 1 ;;
    esac
    shift
done

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
if [ -z "$version" ]; then
    echo "release-linux: the version was not found in Cargo.toml ([workspace.package], version)" >&2
    exit 1
fi
commit=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain)" ]; then
    if [ "$changed" = no ] && [ -z "$trial" ]; then
        echo "release-linux: there are changes that are not committed, and a release says which commit it is:" >&2
        echo "               commit them, or say --changed for a build that is not to be published" >&2
        exit 1
    fi
    commit="$commit-changed"
fi
if [ -z "$trial" ] && [ -n "$trial_version" ]; then
    echo "release-linux: only a trial's build says another version than the workspace's" >&2
    exit 1
fi

tool() {
    cargo run --quiet -p trill-update --example release -- "$@"
}
runs_on=linux-x86_64
name=$version
if [ -n "$trial" ]; then
    trial_public=$(tool public "$trial")
    version=${trial_version:-$version}
    name="$version-trial"
fi
out=${out:-$root/target/release-out/$name}
dist="$root/target/dist"
built="$dist/release"

if [ "$build" = yes ]; then
    # What marks the build as a release, and what never does: a trial's key and version are given
    # by this script's own flags and by nothing that happens to be set where it runs.
    export TRILL_RELEASE_COMMIT="$commit"
    unset TRILL_UPDATE_TRIAL_KEY TRILL_UPDATE_TRIAL_VERSION RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
    if [ -n "$trial" ]; then
        export TRILL_UPDATE_TRIAL_KEY="$trial_public" TRILL_UPDATE_TRIAL_VERSION="$version"
    fi
    # No path of the machine a release is built on goes into it: each folder a program would name
    # the source of its messages by is written as a name instead. (The last that fits is the one
    # taken, so the most general comes first.)
    remap=
    for folder in "$HOME=home" "${RUSTUP_HOME:-$HOME/.rustup}=rustup" "${CARGO_HOME:-$HOME/.cargo}=cargo" "$root=trill"; do
        case "$folder" in
            *"'"*) echo "release-linux: a folder with ' in its name cannot be kept out of the programs: $folder" >&2; exit 1 ;;
        esac
        remap="$remap, '--remap-path-prefix', '$folder'"
    done
    cargo build --release -p trill-app -p trill-mcp --target-dir "$dist" \
        --config "target.x86_64-unknown-linux-gnu.rustflags=[${remap#, }]"
    unset TRILL_RELEASE_COMMIT TRILL_UPDATE_TRIAL_KEY TRILL_UPDATE_TRIAL_VERSION
fi

expected="Trill $version (build $commit)"
[ -z "$trial" ] || expected="$expected, made to try the updater out: not a release"
# The libraries a program may ask the system for by name when it starts: the C library and its
# parts, and ALSA's, which every Linux desktop has (the sound goes through it, to PipeWire or
# PulseAudio where they are there). The window's libraries (X, Wayland, the graphics card's) are
# looked for when it runs, and a program without them is a sentence, not one that does not start.
allowed='libc.so.6 libm.so.6 libgcc_s.so.1 ld-linux-x86-64.so.2 libdl.so.2 libpthread.so.0 librt.so.1 libasound.so.2'
newest=
for program in trill trill-mcp; do
    if [ ! -f "$built/$program" ]; then
        echo "release-linux: $built/$program is not built: run without --no-build" >&2
        exit 1
    fi
    for library in $(readelf -d "$built/$program" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        case " $allowed " in
            *" $library "*) ;;
            *)
                echo "release-linux: $program asks for $library when it starts, which a Linux desktop need not have:" >&2
                echo "               a user's system would have to have it installed before Trill started at all" >&2
                exit 1
                ;;
        esac
    done
    # What is gathered is what is being released: each program says which Trill it is. One that was
    # built from another commit, as another version, or to try the updater out (which trusts a
    # trial's key) is never taken for it, whatever the last run left here. (No window is made for it.)
    says=$(env -u DISPLAY -u WAYLAND_DISPLAY "$built/$program" --version 2>/dev/null || true)
    if [ "$says" != "$expected" ]; then
        echo "release-linux: $program says it is '$says'," >&2
        echo "               and what is being made is '$expected': run without --no-build" >&2
        exit 1
    fi
    # And nothing in it names the folders of whoever built it.
    for folder in "$HOME" "$root"; do
        if grep -a -q -F -- "$folder" "$built/$program"; then
            echo "release-linux: $program holds a path of this machine ($folder...):" >&2
            echo "               it was not built by this script as it is now: run without --no-build" >&2
            exit 1
        fi
    done
    # The newest version of the C library it asks for (a symbol that is only taken where it is
    # there, a weak one, asks for nothing).
    asks=$(objdump -T "$built/$program" | awk '$2 != "w" || $3 != "DF"' | grep -o 'GLIBC_[0-9.]*' | sed 's/^GLIBC_//' | sort -u -V | tail -n 1)
    newest=$(printf '%s\n%s\n' "$newest" "$asks" | sed '/^$/d' | sort -V | tail -n 1)
done

# Only a folder this script made is cleared.
if [ -e "$out" ]; then
    if [ -d "$out" ] && [ -z "$(ls -A "$out")" ]; then
        :
    elif [ -f "$out/made-by-release-linux.sh" ]; then
        rm -rf "$out"
    else
        echo "release-linux: $out is there and is no folder this script made: name one that is not there yet" >&2
        exit 1
    fi
fi
files="$out/Trill"
mkdir -p "$files/licences"
: > "$out/made-by-release-linux.sh"
cp "$built/trill" "$built/trill-mcp" "$files/"
chmod 755 "$files/trill" "$files/trill-mcp"
cp "$root/LICENSE" "$files/licences/Trill (GPL-3.0-or-later).txt"
cp "$root/crates/trill-noir/assets/fonts/LICENSE-Inter.txt" "$files/licences/Inter typeface (SIL Open Font License).txt"
cp "$root/catalogue/CREDITS.md" "$files/licences/Sampled instruments Trill can fetch (credits).md"
{
    echo "Trill is built from these libraries. Each is its makers', under the licence beside its name;"
    echo "the texts of those licences are with each library's source, which its name and version find at crates.io."
    echo
    cargo tree --offline -p trill-app -p trill-mcp --edges normal --prefix none --format "{p} | {l}" \
        --target x86_64-unknown-linux-gnu | sed -e 's/ ([^)]*)//' | grep -v '^trill-' | sort -u
} > "$files/licences/Libraries Trill is built from.txt"
chmod 644 "$files/licences/"*
cat > "$files/READ ME.txt" <<EOF
Trill $version (build $commit), for Linux on x86-64

trill is Trill: start it from this folder (./trill, or a double-click where
your file manager runs programs). trill-mcp is the same program without the
window, for an AI client that speaks MCP.

It needs a C library (glibc) $newest or newer, and ALSA's library
(libasound.so.2), which every desktop has; its sound goes through PipeWire or
PulseAudio where they are there. Its window is drawn through Wayland or X,
and a plugin's own window through X (a Wayland desktop has it, Xwayland).

Trill keeps its settings in ~/.config/trill, and what it fetches when you
ask it to (the models of studies, sampled instruments) and your studies in
~/.local/share/trill, unless you choose another place; it changes nothing
else on the computer. Taking Trill off is deleting this folder: those two
folders and your songs stay where they are.

File, Setup has a tab that says which Trill this is and whether a newer
one is out. A newer one is fetched when you ask for it there, and goes in
the next time Trill starts while no other Trill from this folder runs. It
needs this folder to be one you can write to.

Trill is free software under the GNU General Public License, version 3 or
later (the licences folder). Its source is at
https://github.com/ambb205/trill, and this build is commit $commit of it.
EOF
chmod 644 "$files/READ ME.txt"
echo "glibc $newest" > "$out/needs.txt"

zip="$out/trill-$version-$runs_on.zip"
tool pack "$files" "$zip"
if [ -z "$trial" ]; then
    url="https://github.com/ambb205/trill/releases/download/v$version/trill-$version-$runs_on.zip"
    tool describe --zip "$zip" --url "$url" --runs-on "$runs_on" --out "$out/build-$runs_on.json"
else
    # Described and announced by a build that trusts the trial's key: only such a one takes an
    # address on this machine.
    TRILL_UPDATE_TRIAL_KEY="$trial_public" cargo run --quiet -p trill-update --example release -- \
        describe --zip "$zip" --url "${trial_from}trill-$version-$runs_on.zip" --runs-on "$runs_on" \
        --out "$out/build-$runs_on.json"
    notes="$out/notes.txt"
    echo "A trial of the updater: Trill $version, which nobody released." > "$notes"
    TRILL_UPDATE_TRIAL_KEY="$trial_public" cargo run --quiet -p trill-update --example release -- \
        announce --key "$trial" --version "$version" --date "$(date +%Y-%m-%d)" --notes "$notes" --page "" \
        --build "$out/build-$runs_on.json" --out "$out/trill-release.json"
fi

(cd "$out" && sha256sum -- *.zip *.json 2>/dev/null > SHA256SUMS.txt || true)

echo
echo "release-linux: Trill $version (build $commit) for Linux is in $out"
echo "release-linux: it needs glibc $newest or newer, and libasound.so.2"
ls -l "$out"
if [ -n "$trial" ]; then
    echo "release-linux: this is a trial's build. It trusts a trial's key: do not publish it."
else
    cat <<EOF
release-linux: nothing was published. The machine that has the release key announces this build with the
others: it describes $(basename "$zip") itself and signs the announcement (scripts/release.sh, and
'describe' in crates/trill-update/examples/release.rs); build-$runs_on.json is what this machine worked out,
to hold that against.
EOF
fi
