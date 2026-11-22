#!/bin/sh
# Tries the installer and the updater out on the real programs, on this machine, with nothing
# published and nothing asked of a network. Run it after touching crates/trill-update/src/place.rs,
# trill_update::start, the installer's script or scripts/release.sh.
#
#   sh scripts/update-trial.sh            # all of it; reuses the two builds a run before made
#   sh scripts/update-trial.sh --again    # builds both again first
#
# What it does, each step checked and said:
#
#   1. builds a Trill 0.1.0 and a Trill 0.1.1 as trial builds (scripts/release.sh --trial): they trust
#      a key made here, look where they are told, and are installed as "Trill trial", never as the
#      Trill this machine really has;
#   2. installs 0.1.0 with its installer into target/trial/install, without asking anything;
#   3. serves 0.1.1 from this machine (127.0.0.1) as a release would be published;
#   4. has the installed trill-mcp look for a newer Trill, fetch it and say it is ready: nothing of
#      the installed programs has changed then;
#   5. starts the installed window, which puts 0.1.1 in place and starts it: the programs are 0.1.1's,
#      Windows lists 0.1.1, and the next start clears away what was replaced;
#   6. publishes a 0.1.2 whose window is a program that ends at once, fetches it the same way and
#      starts the window: 0.1.2 does not come up, and 0.1.1 is put back whole and goes on;
#   7. takes Trill off again with its uninstaller: the folder, the entry in Windows' list and the
#      Start menu are as they were before.
#
# The windows it starts are never seen: each is started to take its own picture, which keeps a
# window off the screen (the pictures are in target/trial, and show what Trill said). It needs
# Inno Setup's compiler (see release.sh) and Python. Two release builds take a while the first time.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
again=no
[ "${1:-}" = "--again" ] && again=yes

trial="$root/target/trial"
key="$trial/trial.key"
install="$trial/install"
profile="$trial/profile"
port=8765
from="http://127.0.0.1:$port/"
runs_on=windows-x86_64
entry='HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{7A1D6C5E-3B2F-4D48-8E67-1C9A0F5B2D34}_is1'
shortcut="$APPDATA/Microsoft/Windows/Start Menu/Programs/Trill trial.lnk"
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
# Windows' own programs take options that begin with a slash, which MSYS would rewrite as paths.
windows() {
    MSYS2_ARG_CONV_EXCL='*' "$@"
}
listed() {
    windows reg query "$entry" /v DisplayVersion 2>/dev/null | sed -n 's/.*DisplayVersion *REG_SZ *//p' | tr -d '\r'
}
# How many programs run from the folder Trill was installed into.
running() {
    powershell -NoProfile -Command "@(Get-CimInstance Win32_Process | Where-Object { \$_.ExecutablePath -like '$(cygpath -w "$install")\\*' }).Count" | tr -d '\r'
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
    "$install/trill.exe" --version | tr -d '\r'
}
# Starts the installed window to take its own picture, and waits until nothing of it runs any more.
start_window() {
    TRILL_UPDATE_ADDRESS="${from}trill-release.json" "$install/trill.exe" \
        --screenshot "$(cygpath -w "$trial/$1.png")" --null-audio --no-plugins --no-mcp || true
    wait_until_none_runs
}
fetch_through_the_server() {
    TRILL_UPDATE_ADDRESS="${from}trill-release.json" python "$root/scripts/update-trial.py" \
        "$install/trill-mcp.exe" "$profile" "$1" "$2" "$trial/trill-mcp-$2.log"
    wait_until_none_runs
}
tidy() {
    [ -z "$server" ] || kill "$server" 2>/dev/null || true
    if [ -f "$install/unins000.exe" ]; then
        windows "$install/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART || true
    fi
}
trap tidy EXIT

mkdir -p "$trial"
[ -f "$key" ] || tool key "$key" > /dev/null
public=$(tool public "$key")
[ "$again" = no ] || rm -rf "$trial/a" "$trial/b"

said "1. the two builds"
if [ ! -f "$trial/a/trill-release.json" ]; then
    sh scripts/release.sh --trial "$key" --trial-version 0.1.0 --trial-from "$from" --out "$trial/a"
fi
if [ ! -f "$trial/b/trill-release.json" ]; then
    sh scripts/release.sh --trial "$key" --trial-version 0.1.1 --trial-from "$from" --out "$trial/b"
fi
setup=$(ls "$trial"/a/Trill-*-setup.exe 2>/dev/null | head -n 1)
[ -n "$setup" ] || wrong "there is no installer in $trial/a: Inno Setup's compiler was not found (see release.sh)"
head -n 1 "$trial/a/Trill/READ ME.txt" "$trial/b/Trill/READ ME.txt" | grep Trill | sed 's/^/    /'

said "2. installing 0.1.0 into $install"
[ ! -e "$install" ] || wrong "$install is there already: a trial before did not end; take it off with its unins000.exe, or delete it"
[ -z "$(listed)" ] || wrong "Windows lists a Trill trial already: take it off first"
rm -rf "$profile"
windows "$setup" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER "/TASKS=" "/DIR=$(cygpath -w "$install")"
for file in trill.exe trill-mcp.exe "READ ME.txt" unins000.exe "licences/Trill (GPL-3.0-or-later).txt"; do
    [ -f "$install/$file" ] || wrong "$file was not installed"
done
[ "$(listed)" = 0.1.0 ] || wrong "Windows lists '$(listed)', not 0.1.0"
[ -f "$shortcut" ] || wrong "there is no entry in the Start menu"
case "$(version_of)" in
    "Trill 0.1.0 (build "*) said "   installed: $(version_of)" ;;
    *) wrong "the installed window says '$(version_of)'" ;;
esac
if windows reg query 'HKCU\Software\Classes\Trill.Song' > /dev/null 2>&1; then
    wrong "songs were made to open with the trial, which was told not to"
fi

said "3. serving 0.1.1 at $from"
python -m http.server "$port" --bind 127.0.0.1 --directory "$trial/b" > "$trial/served.log" 2>&1 &
server=$!
tries=0
until curl --silent --fail --output /dev/null "${from}trill-release.json"; do
    tries=$((tries + 1))
    [ "$tries" -lt 50 ] || wrong "nothing answers at $from"
    sleep 0.2
done

said "4. the installed trill-mcp looks, fetches, and says it is ready"
fetch_through_the_server 0.1.0 0.1.1
[ -f "$install/.update/ready.json" ] || wrong "nothing is staged beside the programs"
cmp -s "$install/trill.exe" "$trial/a/Trill/trill.exe" || wrong "the window's program changed while the update was only fetched"
case "$(version_of)" in "Trill 0.1.0 "*) ;; *) wrong "it is '$(version_of)' before any start" ;; esac

said "5. the window starts, puts 0.1.1 in place and starts it"
start_window after-update
case "$(version_of)" in
    "Trill 0.1.1 (build "*) said "   now: $(version_of)" ;;
    *) wrong "after a start the window says '$(version_of)', not 0.1.1" ;;
esac
cmp -s "$install/trill.exe" "$trial/b/Trill/trill.exe" || wrong "the window's program is not 0.1.1's"
cmp -s "$install/trill-mcp.exe" "$trial/b/Trill/trill-mcp.exe" || wrong "the server's program is not 0.1.1's"
[ "$(listed)" = 0.1.1 ] || wrong "Windows lists '$(listed)' after the update, not 0.1.1"
[ ! -f "$install/.update/ready.json" ] || wrong "the update is still said to be staged"
[ -f "$trial/after-update.png" ] || wrong "the new window did not come up to take its picture"
start_window after-tidy
[ ! -e "$install/.update/old" ] || wrong "what the update replaced was not cleared away by the next start"

said "6. a 0.1.2 that does not start is put back"
mkdir -p "$trial/b/c"
rm -rf "$trial/c-files"
cp -r "$trial/b/Trill" "$trial/c-files"
# In the window's place, a program that ends at once with a fault whatever it is started with.
cp "$(cygpath "$SYSTEMROOT")/System32/where.exe" "$trial/c-files/trill.exe"
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
cmp -s "$install/trill.exe" "$trial/b/Trill/trill.exe" || wrong "the window's program is not 0.1.1's again"
cmp -s "$install/trill-mcp.exe" "$trial/b/Trill/trill-mcp.exe" || wrong "the server's program is not 0.1.1's again"
[ -f "$trial/after-put-back.png" ] || wrong "0.1.1 did not go on to take its picture after putting itself back"
[ ! -f "$install/.update/ready.json" ] || wrong "the version that did not start is still staged"

said "7. taking Trill off again"
windows "$install/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
# (The uninstaller goes on in a copy of itself after the program that was started has ended.)
tries=0
while [ -e "$install" ]; do
    tries=$((tries + 1))
    [ "$tries" -lt 60 ] || wrong "the uninstaller left $install: $(ls -A "$install" | tr '\n' ' ')"
    sleep 1
done
[ -z "$(listed)" ] || wrong "Windows still lists the trial"
[ ! -f "$shortcut" ] || wrong "the Start menu still has the trial"
[ -d "$profile" ] || wrong "the uninstaller took the settings, which are the user's"

said "all of it went as it should. The pictures are in $trial: after-update.png, after-put-back.png"
