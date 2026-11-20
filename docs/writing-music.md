# Writing music

**Songwriting**: chords by symbol or Roman numeral, voicings, rhythms, arpeggios, bass lines, drum grooves with
fills (and a reply that says what each fill took the place of), melodies by scale degree with their ornaments (a
bend up to a pitch and its release, a slide from the note before, a scoop up into a note, a hammer-on or pull-off,
vibrato: `5:2!bend=6!vib`), swing and humanizing, and reading a song's key and chords back from its notes
(`trill-theory`). Over MCP an assistant says `Am9 | Fmaj9 | Cmaj7 G/B | Em7`, drop-2, house groove, rather than
spelling notes. A melody's bars are the song's bars where it is written, so one phrase runs through a change of
time signature: four bars of 4/4 and then bars of 7/8, each as long as the song has it.

**A track knows what its notes mean** to its instrument: whether they are drums or pitches (a drum kit, and a
plugin that calls itself a drum instrument, are drums without being told; any track can be switched), which key
each drum is on, which keys are keyswitches and what they choose, which play a noise, where the range ends, the
octave the instrument really sounds at, how far its pitch wheel bends. It is kept in the song, on the track, and
everything reads it. A drum name lands on the kit's own key. `articulation: "palm_mute"` presses the keyswitch
just before the note, and fits the velocity where velocity decides how a note is played; a single note says how it
is played too, in a phrase (`5!palm_mute`) and in `create_clip`, `add_notes` and `set_notes`
(`[64, 0, 1, 100, "harmonic"]`), and a clip's keyswitches are written for all its notes together, so a plain note
is picked the ordinary way and never plays in whatever was left chosen. A harmonic is written on the key under the
pitch that was asked for. An ornament is written as the instrument's own keyswitch where it has one for it, and as
a pitch-bend curve scaled to its bend range where it has not. Reading a song back leaves keyswitches and drums out
of the harmony and reads a bass at the octave it sounds. A part that lands outside the range, or on a keyswitch,
says so when it is written.

**MIDI channels.** A note is on one of sixteen channels, and a clip keeps a curve for each controller on each
channel. Nearly every instrument plays all sixteen alike, and for those nothing ever says a channel: everything is
on the first, a song that uses no other is written exactly as it was before notes had channels, and a song saved
before plays to the bit as it did. A few instruments give their channels meanings of their own, and then the
track's profile names them (`channels`). The Consort Organ plays its pedalboard from channel 2 and its manual from
every other; the Consort Electric Guitar, while its own *String Channels* switch is on, has its strings on channels
1 to 6 (and 7, on the body with a seventh), each with a pitch wheel of its own. The organ's names ship with Trill
and are kept on the track. The guitar's are read from the plugin's switch whenever they are asked for, as a kit is,
and kept nowhere in the song: switch it off and they are gone, and what was written stays on its channels.

A part is written onto a channel by its name or its number (`channel: "pedalboard"`). A whole part goes there with
the writing tools, and into a clip that already holds another channel's part it replaces only what its own channel
had in those bars: a bass line onto the pedals of the clip that holds the manual's chords. A single note says its
own (`{"pitch": "C2", "start": 0, "length": 4, "channel": 2}`). A curve is a channel's too, so the wheel of one
string bends that string while the rest of the chord holds. Everything that moves, splits, loops, copies,
quantizes or transposes notes keeps their channels; the same key at the same time on two channels is two notes,
and each comes up on the channel it went down on. A reply says channels only where a clip uses another than the
first, and a part written on another channel for an instrument that names none is told what that means: such an
instrument may listen to the first only. What is played by hand (the computer keyboard, a MIDI port, a take) is on
the first channel.

Both were measured on the installed plugins, through the tools (`trill-mcp/tests/consort.rs`, opt-in). The
organ's C3 on the manual and the same key on the pedalboard are two sounds: 355 crossings of zero a second
against 173, and the pipe at the key's own pitch 7.6 dB quieter under the pedals. On the guitar, with a G held on
the first string and a D on the second, the second channel's wheel all the way up puts the second string at
64.08 (an E is 64) and leaves the first at 66.87 (a G is 67): in a render, in a song that plays, in a render that
starts after the bend began (64.04 to 64.08), and in a song started there (64.05).

**A kit says what is on its keys.** Two kits of one drum plugin do not keep their drums on the same keys: of the
203 kits that came with Superior Drummer 3 on the machine this was written on, there are 37 different tables.
Where a plugin's state says what hangs in each place and which keys strike it (Superior Drummer 3's does;
`trill-plugins/src/vendor/superior.rs` reads it, with the few words the library has for its drums), the track's
drum map is the kit that is loaded: read from the plugin whenever it is asked for, so it follows a preset, the
plugin's own window and a song that was opened, and nothing of it is kept in the song. `china` is the china of
this kit, a second crash is `crash2`, a stroke the names have no word for is there under the maker's own
(`snare_rim_only`, `ride_bell_tip`), and `get_instrument_profile` lists them all with what each drum is. A drum the
kit has not got is left out and said (`left_out`, with the names the kit does have), in place of a hit on whatever
is on the key the name usually means. An instrument that says nothing of its keys is written for as before: the
track's own map, then General MIDI.

What is found out about a plugin is remembered for that plugin on this machine (`instruments`, beside the settings
file) and given to the next track that plays it. Trill ships what was measured of a few: Ample Guitar PF, Metal
Eclipse, Metal Hellrazer and Metal Ray5, Superior Drummer 3, the SWAM winds and strings, and the Consort Organ and
Electric Guitar (what their MIDI channels are). A song opened from a
file takes what is known where its tracks say nothing themselves. A probe says whether the instrument it played
is known already, and names keys and ranges in the words `set_instrument_profile` takes, so that what one probe
found is written down in one call.
