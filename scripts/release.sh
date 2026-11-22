#!/bin/sh
# Makes a release of Trill for Windows, into target/release-out/<version>:
#
#   Trill-<version>-setup.exe            the installer (where Inno Setup is found)
#   trill-<version>-windows-x86_64.zip   the same files as an archive: what an installed Trill
#                                        fetches to update itself, and Trill without an installer
#   trill-release.json                   the announcement of the release, signed (where a key is given)
#   SHA256SUMS.txt
#
#   sh scripts/release.sh                              # builds and gathers
#   sh scripts/release.sh --key FILE --notes FILE      # and announces it under the key in FILE,
#                                                      # with what is new read from the other
#   sh scripts/release.sh --no-build                   # gathers what the last run built
#   sh scripts/release.sh --changed                    # from a tree with changes that are not committed
#   sh scripts/release.sh --out DIR                    # into DIR instead
#
# The version is the workspace's, in Cargo.toml: change it there, commit, and tag the commit
# v<version>. The build is a release: it says its version and commit, links the C runtime in
# (nobody needs Microsoft's redistributable for it), names no folder of the machine it was built
# on, and is built apart from everything else (target/dist), so that making one disturbs no
# other build.
#
# It publishes nothing and pushes nothing. What to publish, and how, is said at the end.
#
# A trial of the updater, on this machine and with nothing published (docs/development.md):
#
#   sh scripts/release.sh --trial KEYFILE [--trial-version X.Y.Z] [--trial-from http://127.0.0.1:8765/]
#
# builds a Trill that trusts the key in KEYFILE beside its makers', says the version given, looks
# where TRILL_UPDATE_ADDRESS says when it runs, and is installed under a name of its own, never
# as the Trill a machine really has. Its announcement is signed with that key and names the
# archive below the address given. Such a build is never published.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
# A path that was given is meant from where the script was started, and is used from the root of
# the repository and by programs that stand elsewhere again: so each is made whole first.
here=$(pwd)
whole() {
    case "$1" in
        /* | [A-Za-z]:*) printf '%s' "$1" ;;
        *) printf '%s' "$here/$1" ;;
    esac
}
cd "$root"

build=yes
changed=no
key=
notes=
trial=
trial_version=
trial_from=http://127.0.0.1:8765/
out=
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) build=no ;;
        --changed) changed=yes ;;
        --key) key=$(whole "$2"); shift ;;
        --notes) notes=$(whole "$2"); shift ;;
        --trial) trial=$(whole "$2"); shift ;;
        --trial-version) trial_version=$2; shift ;;
        --trial-from) trial_from=$2; shift ;;
        --out) out=$(whole "$2"); shift ;;
        *) echo "release: '$1' is not something this script knows: see its first lines" >&2; exit 1 ;;
    esac
    shift
done

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
if [ -z "$version" ]; then
    echo "release: the version was not found in Cargo.toml ([workspace.package], version)" >&2
    exit 1
fi
commit=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain)" ]; then
    if [ "$changed" = no ] && [ -z "$trial" ]; then
        echo "release: there are changes that are not committed, and a release says which commit it is:" >&2
        echo "         commit them, or say --changed for a build that is not to be published" >&2
        exit 1
    fi
    commit="$commit-changed"
fi
if [ -n "$key" ] && [ -z "$notes" ]; then
    echo "release: an announcement says what is new: give --notes FILE (a few lines of text)" >&2
    exit 1
fi
if [ -n "$key" ] && [ -n "$trial" ]; then
    echo "release: a build that trusts a trial's key is not announced under the real one" >&2
    exit 1
fi
if [ -z "$trial" ] && [ -n "$trial_version" ]; then
    echo "release: only a trial's build says another version than the workspace's" >&2
    exit 1
fi

tool() {
    cargo run --quiet -p trill-update --example release -- "$@"
}
runs_on=windows-x86_64
name=$version
if [ -n "$trial" ]; then
    trial_public=$(tool public "$trial")
    version=${trial_version:-$version}
    name="$version-trial"
fi
out=${out:-$root/target/release-out/$name}
dist="$root/target/dist"
built="$dist/release"
# The home folder of whoever makes the release, which no release is to name.
home=$(cygpath -u "${USERPROFILE:-$HOME}")

if [ "$build" = yes ]; then
    # What marks the build as a release, and what never does: a trial's key and version are given
    # by this script's own flags and by nothing that happens to be set where it runs.
    export TRILL_RELEASE_COMMIT="$commit"
    unset TRILL_UPDATE_TRIAL_KEY TRILL_UPDATE_TRIAL_VERSION RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
    if [ -n "$trial" ]; then
        export TRILL_UPDATE_TRIAL_KEY="$trial_public" TRILL_UPDATE_TRIAL_VERSION="$version"
    fi
    # No path of the machine a release is built on goes into it. A program keeps the place of the
    # source each of its messages comes from, and those places begin with the folders of whoever
    # built it: each is written as a name instead, in both ways Windows spells a path. (The last
    # that fits is the one taken, so the most general comes first.)
    remap=
    for spelled in -w -m; do
        for folder in "$home=home" "${RUSTUP_HOME:-$home/.rustup}=rustup" "${CARGO_HOME:-$home/.cargo}=cargo" "$root=trill"; do
            from=$(cygpath "$spelled" "${folder%=*}")
            case "$from" in
                *"'"*) echo "release: a folder with ' in its name cannot be kept out of the programs: $from" >&2; exit 1 ;;
            esac
            remap="$remap, '--remap-path-prefix', '$from=${folder##*=}'"
        done
    done
    cargo build --release -p trill-app -p trill-mcp --target-dir "$(cygpath -w "$dist")" \
        --config "target.x86_64-pc-windows-msvc.rustflags=['-C', 'target-feature=+crt-static'$remap]"
    unset TRILL_RELEASE_COMMIT TRILL_UPDATE_TRIAL_KEY TRILL_UPDATE_TRIAL_VERSION
fi
expected="Trill $version (build $commit)"
[ -z "$trial" ] || expected="$expected, made to try the updater out: not a release"
for program in trill.exe trill-mcp.exe; do
    if [ ! -f "$built/$program" ]; then
        echo "release: $built/$program is not built: run without --no-build" >&2
        exit 1
    fi
    # The C runtime is linked in: a program that still asks for its DLL would not start on a
    # machine that has never had Microsoft's redistributable installed.
    if grep -a -q -i 'VCRUNTIME140' "$built/$program"; then
        echo "release: $program still asks for VCRUNTIME140.dll: it was not built by this script" >&2
        exit 1
    fi
    # What is gathered is what is being released: each program says which Trill it is. One that was
    # built from another commit, as another version, or to try the updater out (which trusts a
    # trial's key) is never taken for it, whatever the last run left here.
    says=$("$built/$program" --version 2>/dev/null | tr -d '\r' || true)
    if [ "$says" != "$expected" ]; then
        echo "release: $program says it is '$says'," >&2
        echo "         and what is being made is '$expected': run without --no-build" >&2
        exit 1
    fi
    # And nothing in it names the folders of whoever built it.
    for spelled in -w -m; do
        if grep -a -q -F -- "$(cygpath "$spelled" "$home")" "$built/$program"; then
            echo "release: $program holds a path of this machine ($(cygpath "$spelled" "$home")...):" >&2
            echo "         it was not built by this script as it is now: run without --no-build" >&2
            exit 1
        fi
    done
done

# Only a folder this script made is cleared.
if [ -e "$out" ]; then
    if [ -d "$out" ] && [ -z "$(ls -A "$out")" ]; then
        :
    elif [ -f "$out/made-by-release.sh" ]; then
        rm -rf "$out"
    else
        echo "release: $out is there and is no folder this script made: name one that is not there yet" >&2
        exit 1
    fi
fi
files="$out/Trill"
mkdir -p "$files/licences"
: > "$out/made-by-release.sh"
cp "$built/trill.exe" "$built/trill-mcp.exe" "$files/"
cp "$root/LICENSE" "$files/licences/Trill (GPL-3.0-or-later).txt"
cp "$root/crates/trill-noir/assets/fonts/LICENSE-Inter.txt" "$files/licences/Inter typeface (SIL Open Font License).txt"
cp "$root/catalogue/CREDITS.md" "$files/licences/Sampled instruments Trill can fetch (credits).md"
{
    echo "Trill is built from these libraries. Each is its makers', under the licence beside its name;"
    echo "the texts of those licences are with each library's source, which its name and version find at crates.io."
    echo
    cargo tree --offline -p trill-app -p trill-mcp --edges normal --prefix none --format "{p} | {l}" |
        sed -e 's/ ([^)]*)//' | grep -v '^trill-' | sort -u
} > "$files/licences/Libraries Trill is built from.txt"
cat > "$files/READ ME.txt" <<EOF
Trill $version (build $commit)

trill.exe is Trill. trill-mcp.exe is the same program without the window,
for an AI client that speaks MCP.

Trill keeps its settings, and what it fetches when you ask it to, in your
own folders (%APPDATA%\\trill and %LOCALAPPDATA%\\trill), and changes
nothing else on the computer. Taking Trill off the computer leaves those
folders and your songs where they are.

File, Setup has a tab that says which Trill this is and whether a newer
one is out. A newer one is fetched when you ask for it there, and goes in
the next time Trill starts while no other Trill is open.

Trill is free software under the GNU General Public License, version 3 or
later (the licences folder). Its source is at
https://github.com/ambb205/trill, and this build is commit $commit of it.
EOF

zip="$out/trill-$version-$runs_on.zip"
tool pack "$files" "$zip"

if [ -n "$key" ]; then
    tool announce --key "$key" --version "$version" --date "$(date +%Y-%m-%d)" --notes "$notes" \
        --page "https://github.com/ambb205/trill/releases/tag/v$version" \
        --url "https://github.com/ambb205/trill/releases/download/v$version/trill-$version-$runs_on.zip" \
        --zip "$zip" --folder "$files" --runs-on "$runs_on" --out "$out/trill-release.json"
elif [ -n "$trial" ]; then
    if [ -z "$notes" ]; then
        notes="$out/notes.txt"
        echo "A trial of the updater: Trill $version, which nobody released." > "$notes"
    fi
    # Read by a build that trusts the trial's key: only such a one takes an address on this machine.
    TRILL_UPDATE_TRIAL_KEY="$trial_public" cargo run --quiet -p trill-update --example release -- \
        announce --key "$trial" --version "$version" --date "$(date +%Y-%m-%d)" --notes "$notes" --page "" \
        --url "${trial_from}trill-$version-$runs_on.zip" \
        --zip "$zip" --folder "$files" --runs-on "$runs_on" --out "$out/trill-release.json"
fi

# Inno Setup's compiler, found when a release is made and never shipped: where TRILL_ISCC
# says, on the path, or where its own installer puts it.
iscc=${TRILL_ISCC:-}
if [ -z "$iscc" ]; then
    for found in "$(command -v iscc 2>/dev/null || true)" \
        "$root/target/tools/inno/ISCC.exe" \
        "${LOCALAPPDATA:-}/Programs/Inno Setup 6/ISCC.exe" \
        "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
        "/c/Program Files/Inno Setup 6/ISCC.exe"; do
        if [ -n "$found" ] && [ -f "$found" ]; then
            iscc=$found
            break
        fi
    done
fi
if [ -n "$iscc" ]; then
    # MSYS would take an option that begins with a slash for a path, and rewrite it.
    if [ -n "$trial" ]; then
        MSYS2_ARG_CONV_EXCL='*' "$iscc" /Qp "/DVersion=$version" "/DSource=$(cygpath -w "$files")" \
            "/DOutput=$(cygpath -w "$out")" "/DAppId=7A1D6C5E-3B2F-4D48-8E67-1C9A0F5B2D34" \
            "/DMutex=TrillTrialIsRunning" "/DName=Trill trial" "$(cygpath -w "$root/installer/trill.iss")"
    else
        MSYS2_ARG_CONV_EXCL='*' "$iscc" /Qp "/DVersion=$version" "/DSource=$(cygpath -w "$files")" \
            "/DOutput=$(cygpath -w "$out")" "$(cygpath -w "$root/installer/trill.iss")"
    fi
else
    echo "release: Inno Setup's compiler (ISCC.exe) was not found, so no installer was made:"
    echo "         install Inno Setup 6 (https://jrsoftware.org/isdl.php), or name ISCC.exe in TRILL_ISCC"
fi

(cd "$out" && sha256sum -- *.zip *.exe *.json 2>/dev/null > SHA256SUMS.txt || true)

echo
echo "release: Trill $version (build $commit) is in $out"
ls -l "$out"
if [ -n "$trial" ]; then
    echo "release: this is a trial's build. It trusts a trial's key: do not publish it."
elif [ -z "$key" ]; then
    echo "release: no key was given, so there is no announcement: an installed Trill will not hear of this release."
else
    cat <<EOF
release: to publish it (nothing was published), with the commit tagged v$version and pushed:
    gh release create v$version --title "Trill $version" --notes-file "$notes" \\
        "$out/Trill-$version-setup.exe" "$zip" "$out/trill-release.json" "$out/SHA256SUMS.txt"
An installed Trill looks at the newest release's trill-release.json: once it is there, every one hears of it.
EOF
fi
