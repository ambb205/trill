#!/bin/sh
# Tries the updater out on the real programs on Linux, on this machine, with nothing published and
# nothing asked of a network. Run it after touching crates/trill-update/src/place.rs,
# trill_update::start or scripts/release-linux.sh. (scripts/update-trial.sh is Windows'.)
#
#   sh scripts/update-trial-linux.sh            # all of it; reuses the two builds a run before made
#   sh scripts/update-trial-linux.sh --again    # builds both again first
#
# What it does, each step checked and said:
#
#   1. builds a Trill 0.1.0 and a Trill 0.1.1 as trial builds (scripts/release-linux.sh --trial): they
#      trust a key made here and look where they are told, never where a release is announced;
#   2. unpacks 0.1.0 into target/trial-linux/install, as someone does with the archive they
#      downloaded (unzip): its programs can be run;
#   3. serves 0.1.1 from this machine (127.0.0.1) as a release would be published;
#   4. has the unpacked trill-mcp look for a newer Trill, fetch it and say it is ready: nothing of
#      the programs has changed then;
#   5. starts the unpacked window, which puts 0.1.1 in place and starts it: the programs are 0.1.1's
#      and can be run, and the next start clears away what was replaced;
#   6. publishes a 0.1.2 whose window is a program that ends at once, fetches it the same way and
#      starts the window: 0.1.2 does not come up, and 0.1.1 is put back whole and goes on;
#   7. takes Trill off again (the folder is deleted): nothing of it runs, and the settings, which
#      are the user's, are where they were.
#
# The windows it starts are never seen: each is started to take its own picture, which keeps a
# window off every screen (an X window the window manager leaves alone; the pictures are in
# target/trial-linux, and show what Trill said). It needs an X server (a Wayland desktop has one),
# unzip, curl and Python 3. Two release builds take a while the first time.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
again=no
[ "${1:-}" = "--again" ] && again=yes

trial="$root/target/trial-linux"
key="$trial/trial.key"
install="$trial/install"
window="$install/trill"
server_program="$install/trill-mcp"
work="$install/.update"
profile="$trial/profile"
port=8765
from="http://127.0.0.1:$port/"
runs_on=linux-x86_64
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
for needed in unzip curl python3; do
    command -v "$needed" > /dev/null || wrong "$needed is not on this machine, and the trial needs it"
done
[ -n "${DISPLAY:-}" ] || wrong "there is no X server here (DISPLAY), and the windows take their pictures through one"
# The programs that run from the folder Trill was unpacked into, by their process ids.
from_the_folder() {
    for process in /proc/[0-9]*; do
        case "$(readlink "$process/exe" 2>/dev/null || true)" in
            "$install/"*) echo "${process#/proc/}" ;;
        esac
    done
}
running() {
    from_the_folder | wc -l | tr -d ' '
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
    env -u DISPLAY -u WAYLAND_DISPLAY "$window" --version
}
# Starts the unpacked window to take its own picture, and waits until nothing of it runs any more.
start_window() {
    TRILL_UPDATE_ADDRESS="${from}trill-release.json" "$window" \
        --screenshot "$trial/$1.png" --null-audio --no-plugins --no-mcp --profile "$profile" \
        > "$trial/window-$1.log" 2>&1 || true
    wait_until_none_runs
}
fetch_through_the_server() {
    TRILL_UPDATE_ADDRESS="${from}trill-release.json" python3 -I "$root/scripts/update-trial.py" \
        "$server_program" "$profile" "$1" "$2" "$trial/trill-mcp-$2.log"
    wait_until_none_runs
}
tidy() {
    [ -z "$server" ] || kill "$server" 2>/dev/null || true
    for process in $(from_the_folder); do
        kill "$process" 2>/dev/null || true
    done
    rm -rf "$install"
}
trap tidy EXIT

mkdir -p "$trial"
[ -f "$key" ] || tool key "$key" > /dev/null
public=$(tool public "$key")
[ "$again" = no ] || rm -rf "$trial/a" "$trial/b"

said "1. the two builds"
if [ ! -f "$trial/a/trill-release.json" ]; then
    sh scripts/release-linux.sh --trial "$key" --trial-version 0.1.0 --trial-from "$from" --out "$trial/a"
fi
if [ ! -f "$trial/b/trill-release.json" ]; then
    sh scripts/release-linux.sh --trial "$key" --trial-version 0.1.1 --trial-from "$from" --out "$trial/b"
fi
head -n 1 "$trial/a/Trill/READ ME.txt" "$trial/b/Trill/READ ME.txt" | grep Trill | sed 's/^/    /'

said "2. unpacking 0.1.0 into $install"
[ ! -e "$install" ] || wrong "$install is there already: a trial before did not end; delete it"
rm -rf "$profile"
# As someone who downloaded it does: the archive unpacked into a folder of its own.
unzip -q "$trial/a/trill-0.1.0-$runs_on.zip" -d "$install"
for file in trill trill-mcp "READ ME.txt" "licences/Trill (GPL-3.0-or-later).txt"; do
    [ -f "$install/$file" ] || wrong "$file was not unpacked"
done
[ -x "$window" ] && [ -x "$server_program" ] || wrong "the programs that were unpacked cannot be run"
case "$(version_of)" in
    "Trill 0.1.0 (build "*) said "   unpacked: $(version_of)" ;;
    *) wrong "the unpacked window says '$(version_of)'" ;;
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

said "4. the unpacked trill-mcp looks, fetches, and says it is ready"
fetch_through_the_server 0.1.0 0.1.1
[ -f "$work/ready.json" ] || wrong "nothing is staged beside the programs"
cmp -s "$window" "$trial/a/Trill/trill" || wrong "the window's program changed while the update was only fetched"
case "$(version_of)" in "Trill 0.1.0 "*) ;; *) wrong "it is '$(version_of)' before any start" ;; esac

said "5. the window starts, puts 0.1.1 in place and starts it"
start_window after-update
case "$(version_of)" in
    "Trill 0.1.1 (build "*) said "   now: $(version_of)" ;;
    *) wrong "after a start the window says '$(version_of)', not 0.1.1" ;;
esac
cmp -s "$window" "$trial/b/Trill/trill" || wrong "the window's program is not 0.1.1's"
cmp -s "$server_program" "$trial/b/Trill/trill-mcp" || wrong "the server's program is not 0.1.1's"
[ -x "$window" ] && [ -x "$server_program" ] || wrong "the programs that went in cannot be run"
[ ! -x "$install/READ ME.txt" ] || wrong "the READ ME was made a program"
[ ! -f "$work/ready.json" ] || wrong "the update is still said to be staged"
[ -f "$trial/after-update.png" ] || wrong "the new window did not come up to take its picture"
start_window after-tidy
[ ! -e "$work/old" ] || wrong "what the update replaced was not cleared away by the next start"

said "6. a 0.1.2 that does not start is put back"
mkdir -p "$trial/b/c"
rm -rf "$trial/c-files"
cp -r "$trial/b/Trill" "$trial/c-files"
# In the window's place, a program that ends at once with a fault whatever it is started with.
cp /bin/false "$trial/c-files/trill"
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
cmp -s "$window" "$trial/b/Trill/trill" || wrong "the window's program is not 0.1.1's again"
cmp -s "$server_program" "$trial/b/Trill/trill-mcp" || wrong "the server's program is not 0.1.1's again"
[ -f "$trial/after-put-back.png" ] || wrong "0.1.1 did not go on to take its picture after putting itself back"
[ ! -f "$work/ready.json" ] || wrong "the version that did not start is still staged"

said "7. taking Trill off again"
rm -rf "$install"
[ ! -e "$install" ] || wrong "$install is still there"
[ -d "$profile" ] || wrong "the settings, which are the user's, went with the folder"
[ "$(running)" = 0 ] || wrong "something of the trial still runs"

said "all of it went as it should. The pictures are in $trial: after-update.png, after-put-back.png"
