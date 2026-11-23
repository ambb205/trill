#!/bin/sh
# Makes a release of Trill for macOS on Apple silicon, into target/release-out/<version>:
#
#   Trill-<version>-macos.zip               Trill.app and its READ ME, for people to download
#   trill-<version>-macos-aarch64.zip       the files of Trill.app: what an installed Trill
#                                           fetches to update itself
#   build-macos-aarch64.json                what that archive is, unsigned: the machine that has
#                                           the release key announces it with the other systems'
#   SHA256SUMS.txt
#
#   sh scripts/release-macos.sh                  # builds and gathers
#   sh scripts/release-macos.sh --no-build       # gathers what the last run built
#   sh scripts/release-macos.sh --changed        # from a tree with changes that are not committed
#   sh scripts/release-macos.sh --out DIR        # into DIR instead
#
# The version is the workspace's, in Cargo.toml. The build is a release: it says its version and
# commit, names no folder of the machine it was built on, asks for no library the system does not
# have, and is built apart from everything else (target/dist). Trill.app is not signed with a
# developer's identity nor notarised: it is signed ad hoc, which seals its files and is what
# Apple silicon asks of a program (docs/installing-and-updating.md says what someone who
# downloads it does to open it).
#
# The release key is not here: no announcement is signed here. The machine that has it takes
# the archive and the description (`cargo run -p trill-update --example release -- describe`,
# which this script ran, and which reads nothing but the archive) and announces every system's
# build together.
#
# It publishes nothing and pushes nothing.
#
# A trial of the updater, on this machine and with nothing published (scripts/update-trial-macos.sh):
#
#   sh scripts/release-macos.sh --trial KEYFILE [--trial-version X.Y.Z] [--trial-from http://127.0.0.1:8765/]
#
# builds a Trill that trusts the key in KEYFILE beside its makers', says the version given, looks
# where TRILL_UPDATE_ADDRESS says when it runs, and is a bundle of its own ("Trill trial.app",
# another identifier), never the Trill a machine really has. Its announcement is signed with that
# key and names the archive below the address given. Such a build is never published.
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

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    echo "release-macos: this makes Trill for macOS on Apple silicon, on such a Mac" >&2
    exit 1
fi

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
        --key) echo "release-macos: the release key stays on the machine that announces releases" >&2; exit 1 ;;
        *) echo "release-macos: '$1' is not something this script knows: see its first lines" >&2; exit 1 ;;
    esac
    shift
done

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
if [ -z "$version" ]; then
    echo "release-macos: the version was not found in Cargo.toml ([workspace.package], version)" >&2
    exit 1
fi
commit=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain)" ]; then
    if [ "$changed" = no ] && [ -z "$trial" ]; then
        echo "release-macos: there are changes that are not committed, and a release says which commit it is:" >&2
        echo "               commit them, or say --changed for a build that is not to be published" >&2
        exit 1
    fi
    commit="$commit-changed"
fi
if [ -z "$trial" ] && [ -n "$trial_version" ]; then
    echo "release-macos: only a trial's build says another version than the workspace's" >&2
    exit 1
fi

tool() {
    cargo run --quiet -p trill-update --example release -- "$@"
}
runs_on=macos-aarch64
name=$version
app=Trill
identifier=io.github.ambb205.trill
if [ -n "$trial" ]; then
    trial_public=$(tool public "$trial")
    version=${trial_version:-$version}
    name="$version-trial"
    # Never the Trill this machine really has: another name, another identifier.
    app="Trill trial"
    identifier=io.github.ambb205.trill.trial
fi
out=${out:-$root/target/release-out/$name}
dist="$root/target/dist"
built="$dist/release"

if [ "$build" = yes ]; then
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
            *"'"*) echo "release-macos: a folder with ' in its name cannot be kept out of the programs: $folder" >&2; exit 1 ;;
        esac
        remap="$remap, '--remap-path-prefix', '$folder'"
    done
    cargo build --release -p trill-app -p trill-mcp --target-dir "$dist" \
        --config "target.aarch64-apple-darwin.rustflags=[${remap#, }]"
    unset TRILL_RELEASE_COMMIT TRILL_UPDATE_TRIAL_KEY TRILL_UPDATE_TRIAL_VERSION
fi

expected="Trill $version (build $commit)"
[ -z "$trial" ] || expected="$expected, made to try the updater out: not a release"
oldest=
for program in trill trill-mcp; do
    if [ ! -f "$built/$program" ]; then
        echo "release-macos: $built/$program is not built: run without --no-build" >&2
        exit 1
    fi
    # Apple silicon's, and only that.
    if [ "$(lipo -archs "$built/$program")" != arm64 ]; then
        echo "release-macos: $program is built for '$(lipo -archs "$built/$program")', not for Apple silicon alone" >&2
        exit 1
    fi
    # Every library it asks for is the system's: one that is not would not be on another Mac.
    foreign=$(otool -L "$built/$program" | tail -n +2 | awk '{print $1}' | grep -v -e '^/usr/lib/' -e '^/System/Library/' || true)
    if [ -n "$foreign" ]; then
        echo "release-macos: $program asks for libraries another Mac has not got: $foreign" >&2
        exit 1
    fi
    # What is gathered is what is being released: each program says which Trill it is.
    says=$("$built/$program" --version 2>/dev/null || true)
    if [ "$says" != "$expected" ]; then
        echo "release-macos: $program says it is '$says'," >&2
        echo "               and what is being made is '$expected': run without --no-build" >&2
        exit 1
    fi
    # And nothing in it names the folders of whoever built it.
    for folder in "$HOME" "$root"; do
        if grep -a -q -F -- "$folder" "$built/$program"; then
            echo "release-macos: $program holds a path of this machine ($folder...):" >&2
            echo "               it was not built by this script as it is now: run without --no-build" >&2
            exit 1
        fi
    done
    # The oldest macOS it runs on, which the bundle says too.
    minos=$(otool -l "$built/$program" | awk '/LC_BUILD_VERSION/ {found=1} found && $1 == "minos" {print $2; exit}')
    [ -n "$oldest" ] || oldest=$minos
done
if [ -z "$oldest" ]; then
    echo "release-macos: the programs do not say which macOS they need (LC_BUILD_VERSION)" >&2
    exit 1
fi

# Only a folder this script made is cleared.
if [ -e "$out" ]; then
    if [ -d "$out" ] && [ -z "$(ls -A "$out")" ]; then
        :
    elif [ -f "$out/made-by-release.sh" ]; then
        rm -rf "$out"
    else
        echo "release-macos: $out is there and is no folder this script made: name one that is not there yet" >&2
        exit 1
    fi
fi
mkdir -p "$out"
: > "$out/made-by-release.sh"
bundle="$out/$app.app"
contents="$bundle/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources/licences"
cp "$built/trill" "$built/trill-mcp" "$contents/MacOS/"
chmod 755 "$contents/MacOS/trill" "$contents/MacOS/trill-mcp"
cp "$root/LICENSE" "$contents/Resources/licences/Trill (GPL-3.0-or-later).txt"
cp "$root/crates/trill-noir/assets/fonts/LICENSE-Inter.txt" "$contents/Resources/licences/Inter typeface (SIL Open Font License).txt"
cp "$root/catalogue/CREDITS.md" "$contents/Resources/licences/Sampled instruments Trill can fetch (credits).md"
{
    echo "Trill is built from these libraries. Each is its makers', under the licence beside its name;"
    echo "the texts of those licences are with each library's source, which its name and version find at crates.io."
    echo
    cargo tree --offline -p trill-app -p trill-mcp --edges normal --prefix none --format "{p} | {l}" |
        sed -e 's/ ([^)]*)//' | grep -v '^trill-' | sort -u
} > "$contents/Resources/licences/Libraries Trill is built from.txt"

# The icon, from the mark's picture, by the system's own tools.
icons="$out/icon.iconset"
mkdir -p "$icons"
for size in 16 32 64 128 256; do
    sips -z "$size" "$size" "$root/assets/trill-256.png" --out "$icons/icon_${size}x${size}.png" > /dev/null
done
for size in 16 32 64 128; do
    cp "$icons/icon_$((size * 2))x$((size * 2)).png" "$icons/icon_${size}x${size}@2x.png"
done
rm "$icons/icon_64x64.png" "$icons/icon_64x64@2x.png" 2>/dev/null || true
iconutil -c icns "$icons" -o "$contents/Resources/Trill.icns"
rm -rf "$icons"

# What macOS knows the bundle by. No kind of file is said to open with it: a song opens from
# File, Open (docs/status.md).
cat > "$contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>$app</string>
    <key>CFBundleExecutable</key>
    <string>trill</string>
    <key>CFBundleIconFile</key>
    <string>Trill</string>
    <key>CFBundleIdentifier</key>
    <string>$identifier</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$app</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.music</string>
    <key>LSMinimumSystemVersion</key>
    <string>$oldest</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Trill records what you play or sing into a track you armed to record it.</string>
    <key>NSHumanReadableCopyright</key>
    <string>Trill is free software under the GNU General Public License, version 3 or later.</string>
</dict>
</plist>
EOF
plutil -lint "$contents/Info.plist" > /dev/null

# Signed as a whole, ad hoc: no developer's identity, and a seal over every file of the bundle. A
# bundle whose programs carry only their linker's signature is one macOS calls damaged when it was
# downloaded, and opens only after a command in Terminal; one with a seal is one it could not
# check, which Privacy & Security opens. An update brings the new seal with the files it holds.
codesign --force --sign - "$contents/MacOS/trill-mcp" 2> /dev/null
codesign --force --sign - "$bundle" 2> /dev/null
if ! codesign --verify --strict "$bundle" 2> /dev/null; then
    echo "release-macos: the bundle's signature does not hold: $(codesign --verify --strict "$bundle" 2>&1)" >&2
    exit 1
fi

cat > "$out/READ ME.txt" <<EOF
Trill $version (build $commit), for Macs with Apple silicon

Trill.app is Trill. Drag it into your Applications folder (or any folder of
yours) and start it from there: it updates itself only where it can write.
Inside it, Contents/MacOS/trill-mcp is the same program without the window,
for an AI client that speaks MCP.

Trill is not signed with a developer's certificate nor notarised by Apple,
so macOS will not open it the first time you try: it says that it could not
check it. Open System Settings, then Privacy & Security, scroll down to where
it names Trill, and click "Open Anyway". macOS asks once more; after that
Trill opens like any other application.

Trill keeps its settings, and what it fetches when you ask it to, in your
own folders (~/Library/Application Support/trill and ~/Library/Caches/trill),
and changes nothing else on the computer.

File, Setup has a tab that says which Trill this is and whether a newer
one is out. A newer one is fetched when you ask for it there, and goes in
the next time Trill starts while no other Trill is open.

Trill is free software under the GNU General Public License, version 3 or
later (Contents/Resources/licences in the bundle). Its source is at
https://github.com/ambb205/trill, and this build is commit $commit of it.
EOF

# The archive an installed Trill updates from: the files of the bundle, below it.
zip="$out/trill-$version-$runs_on.zip"
tool pack "$bundle" "$zip"
if [ -z "$trial" ]; then
    url="https://github.com/ambb205/trill/releases/download/v$version/trill-$version-$runs_on.zip"
    tool describe --zip "$zip" --url "$url" --runs-on "$runs_on" --out "$out/build-$runs_on.json"
else
    # Described by a build that trusts the trial's key: only such a one takes an address on this machine.
    TRILL_UPDATE_TRIAL_KEY="$trial_public" cargo run --quiet -p trill-update --example release -- \
        describe --zip "$zip" --url "${trial_from}trill-$version-$runs_on.zip" --runs-on "$runs_on" \
        --out "$out/build-$runs_on.json"
fi

# What people download: the bundle and its READ ME, as the system's own tool packs a bundle
# (keeping what is a program).
people="$out/Trill-$version-macos.zip"
gathered="$out/Trill-$version"
mkdir -p "$gathered"
ditto "$bundle" "$gathered/$app.app"
cp "$out/READ ME.txt" "$gathered/"
ditto -c -k --keepParent "$gathered" "$people"
rm -rf "$gathered"

if [ -n "$trial" ]; then
    notes="$out/notes.txt"
    echo "A trial of the updater: Trill $version, which nobody released." > "$notes"
    # Read by a build that trusts the trial's key: only such a one takes an address on this machine.
    TRILL_UPDATE_TRIAL_KEY="$trial_public" cargo run --quiet -p trill-update --example release -- \
        announce --key "$trial" --version "$version" --date "$(date +%Y-%m-%d)" --notes "$notes" --page "" \
        --build "$out/build-$runs_on.json" --out "$out/trill-release.json"
fi

(cd "$out" && shasum -a 256 -- *.zip *.json 2>/dev/null > SHA256SUMS.txt || true)

echo
echo "release-macos: Trill $version (build $commit) for macOS is in $out"
ls -l "$out"
if [ -n "$trial" ]; then
    echo "release-macos: this is a trial's build. It trusts a trial's key: do not publish it."
else
    cat <<EOF
release-macos: nothing was published. The machine that has the release key announces this build with the
others: it describes $(basename "$zip") itself and signs the announcement (scripts/release.sh, and
'describe' in crates/trill-update/examples/release.rs); build-$runs_on.json is what this machine worked out,
to hold that against.
EOF
fi
