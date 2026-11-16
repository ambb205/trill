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
