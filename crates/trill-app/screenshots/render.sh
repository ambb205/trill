#!/usr/bin/env sh
# Regenerates every screenshot in this folder. Run from the repository root:
#
#   sh crates/trill-app/screenshots/render.sh
#
# A screenshot run is hermetic: no audio device, no plugins, no pointer and
# no user settings, so the pictures are the same on every machine. Look at
# what changed before committing them: they are the record of what the
# window looks like, and the place where a layout slip shows first.
set -eu

cargo build -p trill-app
bin=target/debug/trill
out=crates/trill-app/screenshots
song=examples/first-light.trill
rough=$out/projects/rough-mix.trill
rack=$out/projects/drum-rack.trill

# shot NAME ARGS...: one window, rendered to NAME.png.
shot() {
    name=$1
    shift
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
# A device without a layout of its own gets the generic one.
shot generic-layout "$rack" --select Kit --tab devices
