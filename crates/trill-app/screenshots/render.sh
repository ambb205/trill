#!/usr/bin/env sh
# Renders pictures of the window, for looking at a change. Run from the
# repository root, and say which pictures:
#
#   sh crates/trill-app/screenshots/render.sh mixer      # those with "mixer" in their name
#   sh crates/trill-app/screenshots/render.sh list       # the names there are
#   sh crates/trill-app/screenshots/render.sh all        # every one (minutes; rarely what is wanted)
#
# They are written to target/screenshots, which git ignores. Pictures are
# for the eyes of whoever made the change: render the few that show it, look
# at them, and leave it there. They are not kept in the repository, because
# a single new button in the top bar changes every one of them.
#
# The exception is the four pictures the README shows, which live in
# docs/images. Refresh those on purpose, when the README should show
# something new:
#
#   sh crates/trill-app/screenshots/render.sh readme
#
# A screenshot run is hermetic: no audio device, no plugins, no pointer and
# no user settings, so a picture is the same on every machine. Its window
# opens off the screen and does not take the keyboard.
set -eu
here=crates/trill-app/screenshots
out=target/screenshots
readme="first-light assistant-answered sounds-browser keyboard-playing"
mode=${1:-}
only=
case "$mode" in
    "")
        sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
        exit 2
        ;;
    list | all) ;;
    readme) out=docs/images ;;
    *) only=$mode ;;
esac

# Whether the picture called $1 was asked for.
wanted() {
    case "$mode" in
        list) return 0 ;;
        readme) case " $readme " in *" $1 "*) return 0 ;; *) return 1 ;; esac ;;
        *) case "$1" in *"$only"*) return 0 ;; *) return 1 ;; esac ;;
    esac
}

# Whether any of the pictures named was asked for and has to be made.
making() {
    [ "$mode" != list ] || return 1
    for name in "$@"; do
        if wanted "$name"; then return 0; fi
    done
    return 1
}

if [ "$mode" != list ]; then
    cargo build -p trill-app
    mkdir -p "$out"
fi
bin=target/debug/trill
song=examples/first-light.trill
rough=$here/projects/rough-mix.trill
rack=$here/projects/drum-rack.trill

# A sample pack and a song made from it, synthesized here so that the
# pictures of the sound browser show nobody's own library. Written to a
# folder git ignores, and only when a picture needs it; the project names
# its files by this path, so the pictures must be made from the repository
# root.
sounds=$here/sounds
pack="$sounds/Night Shift"
night=$sounds/night-shift.trill
if making sounds-browser sounds-drum-rack sounds-sampler sounds-missing-file min-window-sounds \
    graphite-sounds-browser graphite-sounds-drum-rack; then
    cargo run -p trill-app --example sound_fixtures -- "$sounds"
fi

# shot NAME ARGS...: one window, rendered to NAME.png.
shot() {
    name=$1
    shift
    wanted "$name" || return 0
    if [ "$mode" = list ]; then
        echo "$name"
        return 0
    fi
    "$bin" "$@" --screenshot "$out/$name.png"
    echo "$out/$name.png"
}

# The song written through MCP.
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
# changed the song (the window has gone to the clip it wrote), after an
# answer that uses every kind of Markdown the panel sets, with the list of
# models open and with the list of thinking efforts open, in the middle of
# a whole song (a plan partly ticked off, a web search and a page read,
# past a hundred steps) and at its end, the same in the smallest window,
# at a usage limit after a tool call that failed, with nobody
# signed in, with no CLI, and in the smallest window with the notice that
# an API key is being billed. What it shows is a scripted conversation
# played through the panel's own handling of a turn; no CLI is started, and
# where the script changes the song, the song is changed.
shot assistant-ready "$song" --select Lead --assistant-scene ready
shot assistant-working "$song" --select "Beat A" --assistant-scene working
shot assistant-answered "$song" --select Lead --tab mixer --assistant-scene answered
shot assistant-markdown "$song" --select Lead --assistant-scene markdown
shot assistant-model "$song" --select Lead --assistant-scene model
shot assistant-effort "$song" --select Lead --assistant-scene effort
shot assistant-song "$song" --select Bass --assistant-scene song
shot assistant-song-done "$song" --select Bass --assistant-scene song-done
shot min-window-assistant-song "$song" --select Bass --assistant-scene song --size 1060x640
shot assistant-limit "$song" --select Lead --assistant-scene limit
shot assistant-signed-out "$song" --select Lead --assistant-scene signed-out
shot assistant-not-installed "$song" --select Lead --assistant-scene not-installed
shot min-window-assistant "$song" --select Bass --assistant-scene api-key --size 1060x640
shot graphite-assistant-answered "$song" --select Lead --tab mixer --assistant-scene answered --theme graphite
shot graphite-assistant-markdown "$song" --select Lead --assistant-scene markdown --theme graphite

# The making-of: a canned session (a short tune asked of the assistant, a
# few edits by hand a while later, a second request) played again from its
# journal. A clip being written, its notes arriving in the arrangement and
# in the piano roll; the middle of the session, the plan partly ticked off;
# its end, with the whole story told; the smallest window; and the other
# theme. Nothing is read or kept: the session is made in the run itself.
shot making-of-writing --making-of-scene --making-of-at 0.166
shot making-of-middle --making-of-scene --making-of-at 0.62
shot making-of-end --making-of-scene --making-of-at 1
shot min-window-making-of --making-of-scene --making-of-at 0.62 --size 1060x640
shot graphite-making-of --making-of-scene --making-of-at 0.62 --theme graphite

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

# Controllers in clips: the lane under the piano roll's notes showing the
# melody's expression, with a dot on the chip of each controller the clip
# holds; the mod wheel, in steps and ramps; a controller nothing is written
# for yet; the computer keyboard's strip with its expression and mod wheel
# beside the lane; the smallest window; and the other theme.
shot controllers-expression --demo --select Melody --lane expression
shot controllers-mod-wheel --demo --select Melody --lane mod_wheel
shot controllers-empty --demo --select Melody --lane pitch_bend
shot controllers-keyboard --demo --select Melody --lane expression --hold adg
shot min-window-controllers --demo --select Melody --lane expression --keyboard --size 1060x640
shot graphite-controllers-expression --demo --select Melody --lane expression --theme graphite

# Recording: a track armed and a take being counted in, in the arrangement
# and in the mixer, and the same in the smallest window, where the transport
# gives up its clock. Then the Audio and MIDI panel on made-up hardware: a
# studio's worth of devices with one MIDI port held by another program, the
# chosen interface unplugged (the speakers play, and the panel says why),
# and a machine with nothing plugged in at all.
shot recording-count-in "$song" --select Lead --arm Lead --record
shot recording-mixer "$song" --select Lead --arm Lead --arm Bass --tab mixer --record
shot min-window-recording "$song" --select Bass --arm Bass --record --size 1060x640
shot audio-settings "$song" --select Lead --settings --audio-scene studio
shot audio-settings-fallback "$song" --select Lead --settings --audio-scene fallback
shot audio-settings-none --settings --audio-scene none
shot graphite-audio-settings "$song" --select Lead --settings --audio-scene studio --theme graphite

# Staged for the last pictures, in a folder git ignores: two projects that
# their program did not live to save, in a profile folder of their own (so
# the offer shows nobody's real work, and nothing of the user's is read),
# and the example song with envelopes switched off. Staged here and not at
# the top, so that the work lost "moments ago" still was when it is shown.
fixtures=$here/fixtures
lost=$fixtures/lost-work
held=$fixtures/held-automation.trill
thrown=$fixtures/thrown-sends.trill
if making recovery-offer graphite-recovery-offer min-window-recovery-offer held-automation held-automation-mixer \
    graphite-held-automation thrown-sends graphite-thrown-sends; then
    cargo run -p trill-app --example window_fixtures -- "$fixtures"
fi

# The offer to bring back unsaved work, as a window opens on it.
shot recovery-offer --profile "$lost"
shot graphite-recovery-offer --profile "$lost" --theme graphite
shot min-window-recovery-offer --profile "$lost" --size 1060x640

# Automation someone has taken hold of: envelopes that are off beside ones
# that are on, in the lanes, on the device's panel and on the mixer's faders.
shot held-automation "$held" --select "Pad chords" --tab devices --automation
shot held-automation-mixer "$held" --select "Pad chords" --tab mixer --automation
shot graphite-held-automation "$held" --select "Pad chords" --tab devices --automation --theme graphite

# Sends that move: a send thrown open for single notes and one that was
# taken hold of, in a track's lanes and on the send bars of the headers and
# of the mixer; a fade that follows the fader, which is a straight line in
# its lane, beside the song's own filter sweeps, which were saved straight
# in hertz and are drawn as the curves they are.
shot thrown-sends "$thrown" --select "Keys arp" --tab mixer --automation
shot graphite-thrown-sends "$thrown" --select "Keys arp" --tab mixer --automation --theme graphite
