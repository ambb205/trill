#!/bin/sh
# Tries the updater out on the real programs on macOS, on this machine, with nothing published and
# nothing asked of a network. Run it after touching crates/trill-update/src/place.rs,
# trill_update::start or scripts/release-macos.sh. (scripts/update-trial.sh is Windows'.)
#
#   sh scripts/update-trial-macos.sh            # all of it; reuses the two builds a run before made
#   sh scripts/update-trial-macos.sh --again    # builds both again first
#
# What it does, each step checked and said:
#
#   1. builds a Trill 0.1.0 and a Trill 0.1.1 as trial builds (scripts/release-macos.sh --trial): they
#      trust a key made here, look where they are told, and are "Trill trial.app" with an identifier
#      of their own, never the Trill this machine really has;
#   2. installs 0.1.0 into target/trial-macos/install, as someone does who drags it into a folder;
#   3. serves 0.1.1 from this machine (127.0.0.1) as a release would be published;
#   4. has the installed trill-mcp look for a newer Trill, fetch it and say it is ready: nothing of
#      the bundle has changed then;
#   5. starts the installed window, which puts 0.1.1 in place and starts it: the programs, the
#      bundle's Info.plist and its version are 0.1.1's, and the next start clears away what was
#      replaced;
#   6. publishes a 0.1.2 whose window is a program that ends at once, fetches it the same way and
#      starts the window: 0.1.2 does not come up, and 0.1.1 is put back whole and goes on;
#   7. takes Trill off again (the bundle is deleted) and tells Launch Services to forget the bundles
#      it saw run, so that nothing of the trial is left on the machine.
#
# The windows it starts are never seen: each is started to take its own picture, which keeps a
# window off every screen and the program out of the Dock (the pictures are in target/trial-macos,
# and show what Trill said). It needs Python 3. Two release builds take a while the first time.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
again=no
[ "${1:-}" = "--again" ] && again=yes

trial="$root/target/trial-macos"
key="$trial/trial.key"
install="$trial/install"
bundle="$install/Trill trial.app"
window="$bundle/Contents/MacOS/trill"
server_program="$bundle/Contents/MacOS/trill-mcp"
# Where an update is kept: beside the bundle, never in it.
work="$install/.Trill trial.app.update"
profile="$trial/profile"
port=8765
from="http://127.0.0.1:$port/"
runs_on=macos-aarch64
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
server=

tool() {
    cargo run --quiet -p trill-update --example release -- "$@"
}
said() {
    echo "trial: $*"
}
wrong() {
    echo "trial: WRONG: $*" >&2
    exit 1
}
# How many programs run from the bundle that was installed.
running() {
    pgrep -f "$install/" | wc -l | tr -d ' '
}
wait_until_none_runs() {
    tries=0
    while [ "$(running)" != 0 ]; do
        tries=$((tries + 1))
        [ "$tries" -lt 120 ] || wrong "a program from $install is still running after two minutes"
        sleep 1
    done
}
version_of() {
    "$window" --version
}
listed() {
    plutil -extract CFBundleShortVersionString raw "$bundle/Contents/Info.plist"
}
# Starts the installed window to take its own picture, and waits until nothing of it runs any more.
start_window() {
    TRILL_UPDATE_ADDRESS="${from}trill-release.json" "$window" \
        --screenshot "$trial/$1.png" --null-audio --no-plugins --no-mcp --profile "$profile" || true
    wait_until_none_runs
}
fetch_through_the_server() {
    TRILL_UPDATE_ADDRESS="${from}trill-release.json" python3 -I "$root/scripts/update-trial.py" \
        "$server_program" "$profile" "$1" "$2" "$trial/trill-mcp-$2.log"
    wait_until_none_runs
}
forget_the_bundles() {
    # Launch Services remembers an application it saw run; the trial's are forgotten again.
    for seen in "$bundle" "$trial/a/Trill trial.app" "$trial/b/Trill trial.app"; do
        [ ! -e "$seen" ] || "$lsregister" -u "$seen" > /dev/null 2>&1 || true
    done
}
tidy() {
    [ -z "$server" ] || kill "$server" 2>/dev/null || true
    for program in $(pgrep -f "$install/" || true); do
        kill "$program" 2>/dev/null || true
    done
    forget_the_bundles
    rm -rf "$install"
}
trap tidy EXIT

mkdir -p "$trial"
[ -f "$key" ] || tool key "$key" > /dev/null
public=$(tool public "$key")
[ "$again" = no ] || rm -rf "$trial/a" "$trial/b"

said "1. the two builds"
if [ ! -f "$trial/a/trill-release.json" ]; then
    sh scripts/release-macos.sh --trial "$key" --trial-version 0.1.0 --trial-from "$from" --out "$trial/a"
fi
if [ ! -f "$trial/b/trill-release.json" ]; then
    sh scripts/release-macos.sh --trial "$key" --trial-version 0.1.1 --trial-from "$from" --out "$trial/b"
fi
head -n 1 "$trial/a/READ ME.txt" "$trial/b/READ ME.txt" | grep Trill | sed 's/^/    /'

said "2. installing 0.1.0 into $install"
[ ! -e "$install" ] || wrong "$install is there already: a trial before did not end; delete it"
rm -rf "$profile"
mkdir -p "$install"
# As someone who downloaded it does: the bundle out of the archive people download, into a folder.
ditto -x -k "$trial/a/Trill-0.1.0-macos.zip" "$trial/unpacked"
ditto "$trial/unpacked/Trill-0.1.0/Trill trial.app" "$bundle"
rm -rf "$trial/unpacked"
for file in Contents/MacOS/trill Contents/MacOS/trill-mcp Contents/Info.plist Contents/Resources/Trill.icns \
    "Contents/Resources/licences/Trill (GPL-3.0-or-later).txt"; do
    [ -f "$bundle/$file" ] || wrong "$file is not in the bundle"
done
[ -x "$window" ] && [ -x "$server_program" ] || wrong "the programs in the bundle cannot be run"
[ "$(listed)" = 0.1.0 ] || wrong "the bundle says it is '$(listed)', not 0.1.0"
case "$(version_of)" in
    "Trill 0.1.0 (build "*) said "   installed: $(version_of)" ;;
    *) wrong "the installed window says '$(version_of)'" ;;
esac

said "3. serving 0.1.1 at $from"
python3 -I -m http.server "$port" --bind 127.0.0.1 --directory "$trial/b" > "$trial/served.log" 2>&1 &
server=$!
tries=0
until curl --silent --fail --output /dev/null "${from}trill-release.json"; do
    tries=$((tries + 1))
    [ "$tries" -lt 50 ] || wrong "nothing answers at $from"
    sleep 0.2
done

said "4. the installed trill-mcp looks, fetches, and says it is ready"
fetch_through_the_server 0.1.0 0.1.1
[ -f "$work/ready.json" ] || wrong "nothing is staged in the bundle"
cmp -s "$window" "$trial/a/Trill trial.app/Contents/MacOS/trill" || wrong "the window's program changed while the update was only fetched"
case "$(version_of)" in "Trill 0.1.0 "*) ;; *) wrong "it is '$(version_of)' before any start" ;; esac

said "5. the window starts, puts 0.1.1 in place and starts it"
start_window after-update
case "$(version_of)" in
    "Trill 0.1.1 (build "*) said "   now: $(version_of)" ;;
    *) wrong "after a start the window says '$(version_of)', not 0.1.1" ;;
esac
cmp -s "$window" "$trial/b/Trill trial.app/Contents/MacOS/trill" || wrong "the window's program is not 0.1.1's"
cmp -s "$server_program" "$trial/b/Trill trial.app/Contents/MacOS/trill-mcp" || wrong "the server's program is not 0.1.1's"
[ -x "$window" ] && [ -x "$server_program" ] || wrong "the programs that went in cannot be run"
[ "$(listed)" = 0.1.1 ] || wrong "the bundle says it is '$(listed)' after the update, not 0.1.1"
[ ! -f "$work/ready.json" ] || wrong "the update is still said to be staged"
[ -f "$trial/after-update.png" ] || wrong "the new window did not come up to take its picture"
codesign --verify --strict "$bundle" 2>/dev/null || wrong "the bundle's seal does not hold after the update: $(codesign --verify --strict "$bundle" 2>&1)"
[ "$(ls -A "$bundle")" = Contents ] || wrong "something of the update lies in the bundle: $(ls -A "$bundle" | tr '\n' ' ')"
start_window after-tidy
[ ! -e "$work/old" ] || wrong "what the update replaced was not cleared away by the next start"

said "6. a 0.1.2 that does not start is put back"
mkdir -p "$trial/b/c"
rm -rf "$trial/c-files"
ditto "$trial/b/Trill trial.app" "$trial/c-files"
# In the window's place, a program that ends at once with a fault whatever it is started with.
cp /usr/bin/false "$trial/c-files/Contents/MacOS/trill"
tool pack "$trial/c-files" "$trial/b/c/trill-0.1.2-$runs_on.zip" > /dev/null
echo "A trial: a Trill whose window is not one." > "$trial/c-notes.txt"
cp "$trial/b/trill-release.json" "$trial/b-announcement.json"
TRILL_UPDATE_TRIAL_KEY="$public" cargo run --quiet -p trill-update --example release -- announce --key "$key" \
    --version 0.1.2 --date "$(date +%Y-%m-%d)" --notes "$trial/c-notes.txt" --page "" \
    --url "${from}c/trill-0.1.2-$runs_on.zip" --zip "$trial/b/c/trill-0.1.2-$runs_on.zip" \
    --folder "$trial/c-files" --runs-on "$runs_on" --out "$trial/b/trill-release.json" > /dev/null
fetch_through_the_server 0.1.1 0.1.2
start_window after-put-back
cp "$trial/b-announcement.json" "$trial/b/trill-release.json"
case "$(version_of)" in
    "Trill 0.1.1 (build "*) said "   still: $(version_of)" ;;
    *) wrong "after a version that does not start, the window says '$(version_of)'" ;;
esac
cmp -s "$window" "$trial/b/Trill trial.app/Contents/MacOS/trill" || wrong "the window's program is not 0.1.1's again"
cmp -s "$server_program" "$trial/b/Trill trial.app/Contents/MacOS/trill-mcp" || wrong "the server's program is not 0.1.1's again"
[ "$(listed)" = 0.1.1 ] || wrong "the bundle says it is '$(listed)' after the put back, not 0.1.1"
codesign --verify --strict "$bundle" 2>/dev/null || wrong "the bundle's seal does not hold after the put back"
[ -f "$trial/after-put-back.png" ] || wrong "0.1.1 did not go on to take its picture after putting itself back"
[ ! -f "$work/ready.json" ] || wrong "the version that did not start is still staged"

said "7. taking Trill off again"
forget_the_bundles
rm -rf "$install"
[ ! -e "$install" ] || wrong "$install is still there"
[ -d "$profile" ] || wrong "the settings, which are the user's, went with the bundle"
[ "$(running)" = 0 ] || wrong "something of the trial still runs"

said "all of it went as it should. The pictures are in $trial: after-update.png, after-put-back.png"
