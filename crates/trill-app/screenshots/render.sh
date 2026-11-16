#!/usr/bin/env sh
# Regenerates every screenshot in this folder. Run from the repository root:
#
#   sh crates/trill-app/screenshots/render.sh
#
# With a word after it, only the pictures with that word in their name:
#
#   sh crates/trill-app/screenshots/render.sh assistant
#
# A screenshot run is hermetic: no audio device, no plugins, no pointer and
# no user settings, so the pictures are the same on every machine. Look at
# what changed before committing them: they are the record of what the
# window looks like, and the place where a layout slip shows first.
set -eu
only=${1:-}

cargo build -p trill-app
bin=target/debug/trill
out=crates/trill-app/screenshots
song=examples/first-light.trill
rough=$out/projects/rough-mix.trill
rack=$out/projects/drum-rack.trill

# A sample pack and a song made from it, synthesized here so that the
# pictures of the sound browser show nobody's own library. Written to a
# folder git ignores; the project names its files by this path, so the
# pictures must be made from the repository root.
sounds=$out/sounds
cargo run -p trill-app --example sound_fixtures -- "$sounds"
pack="$sounds/Night Shift"
night=$sounds/night-shift.trill

# shot NAME ARGS...: one window, rendered to NAME.png.
shot() {
    name=$1
    shift
    case "$name" in
        *"$only"*) ;;
        *) return 0 ;;
    esac
    "$bin" "$@" --screenshot "$out/$name.png"
    echo "$name"
}

# The song written through MCP. first-light.png is the README's picture.
shot first-light "$song" --select Lead
shot first-light-clip "$song" --select Lead --size 1920x1080
shot first-light-devices "$song" --select Lead --tab devices
shot first-light-automation "$song" --select "Pad chords" --tab devices --automation
shot first-light-mixer "$song" --select Lead --tab mixer
shot first-light-selection "$song" --select Bass --also-select Arp --also-select Lead
shot graphite-first-light-clip "$song" --select Lead --theme graphite
shot graphite-first-light-devices "$song" --select Lead --tab devices --theme graphite
shot graphite-first-light-automation "$song" --select "Pad chords" --tab devices --automation --theme graphite
shot graphite-first-light-mixer "$song" --select Lead --tab mixer --theme graphite

# The built-in demo: a group, a return with sends, markers, an envelope.
shot demo-clip --demo
shot demo-devices --demo --tab devices
shot demo-drums-playing --demo --select Beat --play
shot demo-mixer --demo --tab mixer --automation
shot demo-mixer-1080 --demo --tab mixer --automation --size 1920x1080
shot graphite-demo-mixer --demo --tab mixer --automation --theme graphite
shot shortcuts --demo --shortcuts

# The edges: nothing at all, the smallest window, and a project where
# everything is awkward (long names, 6/8, audio with fades, missing files,
# a plugin that is not installed).
shot empty
shot audio-clip-6-8 "$rough" --select "Verse 1"
shot min-window-problems "$rough" --select Riff --tab devices --size 1060x640
shot min-window-mixer --demo --tab mixer --automation --size 1060x640
shot graphite-min-window --demo --automation --size 1060x640 --theme graphite
# A drum rack nobody has put a sound on yet.
shot drum-rack-empty "$rack" --select Kit --tab devices

# Sounds: the library in the browser with a sound chosen, and an audio clip
# open in the Clip tab; a search beside the drum rack's pads; the sampler
# with its file; a clip whose file is gone, in a browser with no folders
# yet; and the same things in the smallest window and the other theme.
shot sounds-browser "$night" --browse sounds --library "$pack" --sound "Drum Loop Steady 120bpm" --select "Pad Loop"
shot sounds-drum-rack "$night" --browse sounds --library "$pack" --search kick --select Beat --tab devices
shot sounds-sampler "$night" --browse sounds --library "$pack" --sound "Pluck C3" --select Riff --tab devices
shot sounds-missing-file "$night" --browse sounds --select "Lost Take"
shot min-window-sounds "$night" --browse sounds --library "$pack" --sound "Bass Loop 120bpm Am" --select "Bass Loop"     --size 1060x640
shot graphite-sounds-browser "$night" --browse sounds --library "$pack" --sound "Drum Loop Steady 120bpm"     --select "Pad Loop" --theme graphite
shot graphite-sounds-drum-rack "$night" --browse sounds --library "$pack" --search kick --select Beat --tab devices     --theme graphite

# The assistant's panel: ready, in the middle of a turn, after a turn that
# changed the song, at a usage limit after a tool call that failed, with
# nobody signed in, with no CLI, and in the smallest window with the notice
# that an API key is being billed. What it shows is a scripted conversation
# played through the panel's own handling of a turn; no CLI is started, and
# where the script changes the song, the song is changed.
shot assistant-ready "$song" --select Lead --assistant-scene ready
shot assistant-working "$song" --select "Beat A" --assistant-scene working
shot assistant-answered "$song" --select Bass --assistant-scene answered
shot assistant-limit "$song" --select Lead --assistant-scene limit
shot assistant-signed-out "$song" --select Lead --assistant-scene signed-out
shot assistant-not-installed "$song" --select Lead --assistant-scene not-installed
shot min-window-assistant "$song" --select Bass --assistant-scene api-key --size 1060x640
shot graphite-assistant-answered "$song" --select Bass --assistant-scene answered --theme graphite

# The computer keyboard played as an instrument: a chord held on the lead,
# lit in the strip and on the piano roll's keys; a kick and a hat held on a
# drum track, two octaves down; what the strip says when the selected track has nothing to play
# and when there is no track at all; the shortcut reference while it is on
# (the letters it plays are listed with Alt); the smallest window, with the
# keys an octave down on the bass; and the other theme.
shot keyboard-playing "$song" --select Lead --hold adg
shot keyboard-drums --demo --select Beat --hold zzat
shot keyboard-no-instrument "$rough" --select "Verse 1" --keyboard
shot keyboard-no-track --keyboard
shot keyboard-shortcuts --demo --shortcuts --keyboard
shot min-window-keyboard "$song" --select Bass --hold zadg --size 1060x640
shot graphite-keyboard-playing "$song" --select Lead --hold adg --theme graphite

# Staged for the last pictures, in a folder git ignores: two projects that
# their program did not live to save, in a profile folder of their own (so
# the offer shows nobody's real work, and nothing of the user's is read),
# and the example song with envelopes switched off. Staged here and not at
# the top, so that the work lost "moments ago" still was when it is shown.
fixtures=$out/fixtures
cargo run -p trill-app --example window_fixtures -- "$fixtures"
lost=$fixtures/lost-work
held=$fixtures/held-automation.trill

# The offer to bring back unsaved work, as a window opens on it.
shot recovery-offer --profile "$lost"
shot graphite-recovery-offer --profile "$lost" --theme graphite
shot min-window-recovery-offer --profile "$lost" --size 1060x640

# Automation someone has taken hold of: envelopes that are off beside ones
# that are on, in the lanes, on the device's panel and on the mixer's faders.
shot held-automation "$held" --select "Pad chords" --tab devices --automation
shot held-automation-mixer "$held" --select "Pad chords" --tab mixer --automation
shot graphite-held-automation "$held" --select "Pad chords" --tab devices --automation --theme graphite
