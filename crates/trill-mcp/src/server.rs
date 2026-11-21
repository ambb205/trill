//! The MCP tools. Each one is a thin adapter from a request to calls on the
//! shared [`Runtime`]: its session, its sound library and the operations
//! that join them. No musical logic lives here that the GUI could not also
//! reach.

use crate::guard::Guard;
use crate::input::{
    ControllerPointInput, PitchInput, controller_scale, find_param, find_param_index, parse_controller,
    parse_controller_points, parse_notes, parse_param_value, parse_pitch_value, parse_points,
};
use crate::requests::*;
use crate::view::{
    ParamDetail, ParamSpan, PresetSearch, audio_device_json, audio_json, clip_json, controller_json, descriptor_json,
    descriptor_summary_json, device_json, lane_json, midi_input_json, param_state_json, plugin_param_json,
    presets_json_with, project_json, round, samples_json, scan_json, sound_json, take_json, target_json,
    tempo_map_json, track_json, transport_json, warp_name,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use trill_core::device::{BUILTIN_SCHEME, DeviceCategory, DeviceFactory, EditorOptions, EventKind, split_uri};
use trill_core::journal::{Author, CallId, Outcome};
use trill_core::midi_file;
use trill_core::presets::{PresetFile, PresetList};
use trill_core::project::AutomationPoint;
use trill_core::project::{AutomationTarget, MAX_TEMPO, MAX_VOLUME_DB, MIN_TEMPO, WarpMode};
use trill_core::{
    ChainRef, Channel, ClipId, ControllerKind, DeviceId, EditError, Note, Project, TrackId, TrackKind, Travel,
};
use trill_engine::{Session, SessionError, SharedSession};
use trill_library::{EntryKind, Key, Query, Tag, Tags, suggested_locations};
use trill_runtime::{Runtime, SoundClip, SoundError};

const SAMPLER_URI: &str = "builtin:sampler";
const DRUM_RACK_URI: &str = "builtin:drumrack";
/// Search results returned when the caller does not say how many, and the
/// most it can ask for.
const DEFAULT_SOUNDS: usize = 20;
const MAX_SOUNDS: usize = 200;
/// The most notes play_notes plays in one call, and the longest it plays for.
const MAX_LIVE_NOTES: usize = 256;
const MAX_LIVE_SECONDS: f64 = 60.0;
/// How long a library scan is waited for before the reply goes out without
/// it, when the caller does not say, and the longest it can ask for.
const DEFAULT_SCAN_WAIT: f64 = 15.0;
const MAX_SCAN_WAIT: f64 = 120.0;
/// Presets listed when the caller does not say how many, and the most it
/// can ask for.
const DEFAULT_PRESETS: usize = 20;
const MAX_PRESETS: usize = 100;
/// The most presets a reply names without having been asked for any.
const UNASKED_PRESETS: usize = 24;
/// How long a tool waits for a plugin's presets to be read before it
/// answers with what there is.
const PRESET_WAIT: Duration = Duration::from_secs(20);

/// How long a tool waits for a device that is still being made before it
/// goes on without it, and says so.
const DEVICE_WAIT: Duration = Duration::from_secs(60);

/// The tools that wait for a device they name, or for the devices of a
/// track or a clip they name, where one is still being made: they ask the
/// device something (its parameters, its window), or what a track's notes
/// mean, which the instrument says (the drums of the kit it has loaded).
/// The others answer at once; what they say of such a device is that it is
/// not made yet.
const WAITS_FOR_DEVICES: &[&str] = &[
    "update_device",
    "show_device_editor",
    "save_preset",
    "save_sound",
    "set_sample",
    "describe_wavetable",
    "set_wavetable",
    "set_automation",
    "create_clip",
    "update_clip",
    "add_notes",
    "set_notes",
    "remove_notes",
    "transform_notes",
    "get_notes",
    "play_notes",
    "get_instrument_profile",
    "set_instrument_profile",
    "take_from_study",
];
/// A device with more parameters than this is shown by those that were
/// moved from their defaults, unless more is asked for.
const MANY_PARAMS: usize = 64;
/// Parameters listed of a device with many when the caller does not say
/// how many, and the most it can ask for.
const DEFAULT_PARAMS: usize = 40;
const MAX_PARAMS: usize = 200;
/// At most this many parameters found by a search come with their ranges,
/// choices and defaults.
const DETAILED_PARAMS: usize = 12;
/// How far a value may be from a default and still be at it, as a part of
/// the parameter's range: plugins report a knob at its default as 0.499999.
const AT_DEFAULT: f64 = 1e-4;
/// A stepped parameter with at most this many steps is shown by what each
/// step is called.
const NAMED_STEPS: f64 = 64.0;

const INSTRUCTIONS: &str = "\
This server controls Trill, a digital audio workstation. You compose, arrange and mix a song by calling tools; the \
user hears and sees every change live.

Conventions
- Time is in beats (quarter notes), starting at 0. In 4/4, bar N starts at beat (N-1)*4. A beat is a quarter \
note in every time signature: a bar of 7/8 is 3.5 beats, a bar of 6/8 is 3.
- Pitch is a MIDI number (60 = C4 = middle C), a note name (\"C4\", \"F#3\", \"Bb2\") or, on a drum track, the \
name of a drum of its kit (\"kick\", \"snare\", \"closed_hat\", \"open_hat\", \"china\", ...). Write drums by name, \
never by number: which key a drum is on depends on the instrument and on the kit it has loaded. Velocity is 1-127.
- Every track, clip and device has an integer id, returned when it is created and listed by get_project.
- Note positions inside a clip are relative to the clip start.
- A MIDI clip also holds controller curves beside its notes: expression, mod_wheel, pitch_bend, sustain, \
breath, pressure, volume, or any controller number. Their values are 0-127 (pitch_bend: -1 to 1) and their beats \
are relative to the clip start. They loop, trim, split and move with the clip.

A good workflow
1. get_project to see what exists; set_project for tempo and time signature.
2. create_track per part with an instrument and preset (list_devices shows instruments, effects and presets; \
search_presets finds a sound by words across all of them).
   A plugin (clap:, vst3:) may have thousands of presets: before programming one by hand, search them with \
describe_device `preset_query` (\"warm pad\") and give `preset` the name or id to create_track, add_device or \
update_device.
   Before you write for a plugin, call get_instrument_profile on its track: it says what Trill knows of the \
instrument (its range, the octave it sounds at, its articulations by name, a drum kit's drums by name). Where it \
knows the instrument, that is all you need and no probe is called for. Where it knows nothing, call probe on \
the track once: it tells you which notes the instrument plays, at what pitch they sound, and which keys switch \
its articulation.
3. Write the parts: the writing tools below for chords, drums, bass lines and melodies, create_clip with notes \
for anything else. For repeating parts, write one or two bars and set loop_length, or duplicate_clip.
   Instruments modelled on breath or a bow (solo winds, brass, strings) are silent until they are given \
expression, as a saxophone is without air. After writing such a line, call shape_dynamics on its clip: it reads \
the phrases from the notes and writes an expression curve that rises into the high points, swells through long \
notes and breathes between phrases. set_controllers writes or adjusts a curve point by point (a pedal, a pitch \
bend, vibrato on the mod wheel). measure names any track that had notes and made no sound.
4. Shape the sound: add_device for effects (reverb, delay, eq, compressor, filter, chorus, tremolo, saturator, \
pitch shifter, tape stop...), update_device for parameters, set_automation for movement such as filter sweeps, fades and a \
send opened for one note (a delay throw), update_track for the mix. describe_device says what each parameter does \
and in what unit. Two points are enough for a fade or a sweep: the line between them follows the control's travel \
(a fader or a gain as Ableton Live's move for a level, pitch for a frequency), so it does not need to be laid out \
in small steps.
5. measure reports loudness, true peak, spectral balance, every track's level and loudness, each section's \
loudness and what the compressors and limiters took off, without writing a file. You cannot hear, so read those \
numbers and adjust, as often as it takes.
6. render writes the WAV file: once, for the finished song, or when the user asks for one.

Working with the user's own sounds
- search_sounds looks through the user's sample library (drum hits, loops, one-shots) by words, tags, tempo, key \
and length, and returns file paths. If the library has no folders yet, `library` suggests where samples are on \
this machine and adds folders.
- preview_sound plays a file to the user and tells you its length, tempo and key.
- Drum hits: create_track with instrument \"builtin:drumrack\", then set_sample once per pad (\"kick\", \"snare\", \
\"closed_hat\"...) and write the pattern with create_clip using the same drum names. For one sound played across \
the keyboard use \"builtin:sampler\" and set_sample.
- Loops: create_audio_clip on a track without an instrument. Give warp \"beats\" (drums) or \"tonal\" (melodic) so \
the loop follows the project tempo: its beats then line up with the song's, at any tempo, and its default length \
is its own length in beats. `transpose` shifts its pitch into the song's key. Unwarped clips play at their \
recorded speed.
- A freshly warped clip plays in time but repitched for a moment while it is prepared (get_project shows \
preparing_clips); measure and render always wait for the real audio.

Playing and recording live
- update_track `armed` arms a track: a MIDI keyboard (midi_inputs lists them; every input is on unless switched off) then plays its instrument. With no track armed, a keyboard plays the track selected in the window.
- transport `record` records what is played on the armed tracks into clips, from the playhead, after a count-in; `stop` ends the take and lists its clips. Recording inside an existing MIDI clip adds to that clip. With the loop on, each pass is layered onto the last. One take is one undo step.
- play_notes plays notes live through an instrument, as a keyboard would: they are heard now and, during a recording, recorded. It is not how to write a part: create_clip is.
- audio shows the audio device, its buffer and latency, and changes them.

Musical guidance
- Vary velocity and use transform_notes to humanize; identical velocities sound mechanical.
- Keep bass and kick in the centre; use low_cut on the EQ for everything else (slope \"24 dB\" or \"48 dB\" \
clears what is under the corner without thinning what is over it); leave headroom on the master.
- Compressor: on a single track the \"peak\" detector (the default) with attack 1-10 ms; on a group or the master \
set detector \"bus\", which is the one that works with a slow attack on a full mix (threshold about 10 dB under \
the peaks, ratio 2, knee 18, attack 10-30 ms, auto_release on: about 2 dB off). auto_makeup adds a known amount of gain, \
0.7 x -threshold x (1 - 1/ratio) dB. measure says what each compressor took off.
- To make room for the kick, put a compressor on the bass (or the pads) and give it the kick's track as \
`sidechain`: it then turns its own track down whenever the kick sounds. The kick's track may be muted.
- A plugin that has a sidechain input (a compressor, a gate, a vocoder) takes `sidechain` the same way; how it \
uses what it hears is set in the plugin, where it usually has a switch for listening to its sidechain.
- Reverb and delay belong on return tracks with `mix` 1, fed by sends of -10 to -20 dB: both filter what goes into \
them (low_cut, high_cut, 12 dB an octave; what lies between them comes back whole), and the reverb returns more \
the longer its decay (3 to 4 dB for each doubling).
- A level's lowest value is silence: a track's fader, a send, a Utility's `gain` and an instrument's `volume` \
all end in nothing at -70 dB. Draw a fade or a stop down to -70, not to some other low number.
- Samples follow velocity in dB. On builtin:sampler and builtin:drumrack `volume` is the level of a note at \
velocity 75, and `velocity_sens` moves harder and softer notes from it, 0.4 dB a step at 1 (velocity 100 is \
+10 dB, 127 is +21 dB, 45 is -12 dB; 0.5 is half of that): a hard hit is louder than the volume, so leave it \
room, and write rolls, accents and ghost notes with velocity.
- A sub bass on builtin:polysynth: a sine with `pitch_env` 12 and `pitch_decay` 0.03 for the knock of an 808, \
into builtin:saturator on `sine` so that it is heard on small speakers. Noise for a riser or a splash: `osc_level` \
0, `noise_level` 1, `noise_width` 1, through the synth's band-pass with `cutoff` and `volume` automated. The \
saturator does not turn down what it drives: more drive is louder, up to the curve's ceiling; trim with `output`.
- A tape stop (a part, or the whole song, slowing down and sinking to a stop; a record winding down; a power-down \
into a break): builtin:tapestop on the track, or on the master for the whole song, with its `speed` automated: 1 \
held until the stop begins, a line down to 0 over one or two beats, 0 held through the silence, and back to 1 where \
the sound returns. Pitch and speed fall together and nothing is repeated. To make the song itself slower and keep \
its pitch, change the tempo (set_tempo with `ramp`).
- builtin:shifter changes the pitch of any sound and keeps its length, recorded and sampled parts too: an octave \
or a harmony beside a part, a recording moved a few semitones. It is not for a slow-down: shifted far, and most of \
all on chords and distorted sounds, it is grainy.
- A built-in synth's or sampler's `bend_range` is how far its pitch wheel bends; bends written for the track \
follow it, so set it before writing a wide bend or a dive as a pitch_bend curve.
- Give sections contrast: add and remove parts, automate filters and volume, change the drum pattern.
- Rearranging: edit_time removes, repeats or opens up a stretch of time on every track at once (drop a \
section, double a chorus, make room for a bridge).
- Reusing a section: edit_time \"copy\" copies a stretch of time to another place on every track (or some), \
with automation and controller curves, in one call. Do not duplicate a chorus clip by clip.
- Every edit can be reversed with history (undo/redo).

A tempo or a time signature that changes inside the song
- set_project gives the tempo and the time signature the song begins with. set_tempo and set_time_signature \
change them from a bar on: `set_time_signature {bar: 9, time_signature: \"7/8\", bars: 4}` makes bars 9 to 12 \
bars of 7/8 and goes back to what was there before; `set_tempo {bar: 9, bpm: 140}` plays from bar 9 at 140. \
`ramp: true` makes the tempo glide to that bar from the change before it (a ritardando into a bar, an \
accelerando out of one). Nothing in the song moves when either is set: positions are beats, and beats stay \
where they are. What changes is how long a beat lasts and which bar it is in.
- Bars are counted through the changes. Set them from the start of the song towards its end, time signatures \
before the music that goes into their bars. A time signature begins a bar where it is set, so setting one in \
the middle of what follows can cut the bar before the next change short: the reply names such bars \
(`cut_bars`), and get_tempo_map shows the whole map, with the beat and the second each change lies at.
- Positions you give stay in beats. get_tempo_map `bars: [9, 13]` answers with the beat each of those bars \
starts at; within one time signature, bar N starts at that signature's first `beat` plus (N minus its first \
`bar`) times its `beats_per_bar`.
- The writing tools write the song's own bars from `start` on, each in the time signature it has there: a \
progression's fourth chord fills the fourth bar written, whether that is four beats long or three and a half. \
write_melody counts its line through the same bars: a bar line in the phrase is checked against the song's bar \
there, so a line that crosses a change is one call, its bar of 7/8 written as 3.5 beats. A named \
groove or fill is written in a meter of its own (music_reference \"grooves\"); in a bar of another meter its \
steps keep their length, the groove is cut off or begins again to fill the bar, and a fill ends with the bar. \
Lanes you give divide each bar evenly, so write a bar of 7/8 as 7 or 14 steps.
- Envelopes, controller curves, markers and the loop are in beats and follow the changes. An audio clip that \
follows the tempo (warp) follows every change of it; one that does not keeps its own speed. A plugin is told \
the tempo and the time signature as they pass.
- The metronome and the count-in click the time signature they are in. measure and render take sections by \
`bars` of the song as it is counted now.";

/// The points of envelopes one get_automation reply lists when it is not
/// told how many, and the most it can be asked for.
const DEFAULT_POINTS: usize = 1000;
const MAX_POINTS: usize = 5000;

pub(crate) type Reply = Result<Value, String>;

/// What a tool's `target` names: the target, the lowest and the highest
/// value a point of its envelope may have, and how its control travels.
/// An error says what the target needs, or which ids there are.
fn resolve_target(session: &Session, input: &TargetInput) -> Result<(AutomationTarget, f64, f64, Travel), String> {
    let project = session.project();
    let need = |value: Option<u64>, what: &str| value.ok_or_else(|| format!("this target needs `{what}`"));
    let volume = |target: AutomationTarget| {
        // A send is a fader too, with its knee where a send's is.
        let travel = target.travel().unwrap_or(Travel::FADER);
        (target, f64::MIN, MAX_VOLUME_DB as f64, travel)
    };
    let found = |track: TrackId| project.track(track).map_err(|error| explain(project, error));
    Ok(match input.kind {
        TargetKind::TrackVolume => {
            let track = TrackId(need(input.track, "track")?);
            found(track)?;
            volume(AutomationTarget::TrackVolume { track })
        }
        TargetKind::TrackPan => {
            let track = TrackId(need(input.track, "track")?);
            found(track)?;
            (AutomationTarget::TrackPan { track }, -1.0, 1.0, Travel::Linear)
        }
        TargetKind::MasterVolume => volume(AutomationTarget::MasterVolume),
        TargetKind::Send => {
            let (track, to) = (TrackId(need(input.track, "track")?), TrackId(need(input.to, "to")?));
            let (from, into) = (found(track)?, found(to)?);
            let returns: Vec<String> = project
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Return)
                .map(|track| format!("{} ({})", track.id.0, track.name))
                .collect();
            if into.kind != TrackKind::Return {
                return Err(match returns.is_empty() {
                    true => format!(
                        "{to} ({}) is not a return track, and the project has none yet: create_track with kind \
                         \"return\" makes one",
                        into.name
                    ),
                    false => {
                        format!("{to} ({}) is not a return track. Return tracks: {}", into.name, returns.join(", "))
                    }
                });
            }
            if from.kind == TrackKind::Return {
                return Err(format!("{track} ({}) is a return track; returns cannot send to returns", from.name));
            }
            volume(AutomationTarget::Send { track, to })
        }
        TargetKind::DeviceParam => {
            let device = DeviceId(need(input.device, "device")?);
            let key = input.param.as_deref().ok_or("this target needs `param`")?;
            let states = session.device_params(device).map_err(|error| explain(project, error))?;
            let infos: Vec<_> = states.into_iter().map(|state| state.info).collect();
            let info = find_param(&infos, key)?;
            (
                AutomationTarget::DeviceParam { device, param: info.id.clone() },
                info.min,
                info.max,
                Travel::of_param(info),
            )
        }
    })
}

pub(crate) fn finish(reply: Reply) -> CallToolResult {
    match reply {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
    }
}

/// Turns an error into a message that says what to do next: a missing id
/// comes back with the ids that do exist.
/// Says which track `device` listens to in place of its own input, by id
/// and by name, if it listens to one.
fn sidechain_into(out: &mut Value, project: &Project, device: &trill_core::Device) {
    if let Some(track) = device.sidechain {
        let name = project.track(track).map(|track| track.name.clone()).unwrap_or_default();
        out["sidechain"] = json!({ "track": track, "name": name });
    }
}

pub(crate) fn explain(project: &Project, error: impl Into<SessionError>) -> String {
    let error = error.into();
    let list = |items: Vec<String>, what: &str| {
        if items.is_empty() {
            format!("{error}. The project has no {what} yet.")
        } else {
            let shown = items.iter().take(40).cloned().collect::<Vec<_>>().join(", ");
            let more = if items.len() > 40 { ", ..." } else { "" };
            format!("{error}. Existing {what}: {shown}{more}")
        }
    };
    match &error {
        SessionError::Edit(EditError::TrackNotFound(_)) => {
            list(project.tracks.iter().map(|t| format!("{} ({})", t.id.0, t.name)).collect(), "tracks")
        }
        SessionError::Edit(EditError::ClipNotFound(_)) => list(
            project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| format!("{} ({} on {})", c.id.0, c.name, t.name)))
                .collect(),
            "clips",
        ),
        SessionError::Edit(EditError::DeviceNotFound(_)) => {
            list(project.devices().map(|d| format!("{} ({})", d.id.0, d.name)).collect(), "devices")
        }
        _ => error.to_string(),
    }
}

/// Runs an edit and explains its failure against the current project.
pub(crate) fn edit<T>(
    session: &mut Session,
    change: impl FnOnce(&mut Project) -> Result<T, EditError>,
) -> Result<T, String> {
    session.edit(change).map_err(|error| explain(session.project(), error))
}

/// Puts the parts of a MIDI file into the song from `at` beats on, each on
/// a new track with an instrument (`melodic`, or the drum kit for a drum
/// part) and one clip: one undo step. With `use_tempo` the song takes the
/// file's tempo and time signature, and every change of them, from there.
pub(crate) fn import_parts(
    session: &mut Session,
    file: &midi_file::MidiFile,
    at: f64,
    use_tempo: bool,
    melodic: &str,
) -> Result<Vec<Value>, String> {
    // One undo step for the whole import.
    session.begin_gesture();
    let result: Result<Vec<Value>, String> = (|| {
        let parts = edit(session, |project| {
            if use_tempo {
                // Every tempo and time signature of the file, from where it is put.
                let mut timed = file.clone();
                timed.tempo = timed.tempo.map(|tempo| tempo.clamp(MIN_TEMPO, MAX_TEMPO));
                for change in &mut timed.tempo_changes {
                    change.tempo = change.tempo.clamp(MIN_TEMPO, MAX_TEMPO);
                }
                project.take_midi_time(&timed, at)?;
            }
            project.import_midi(file, at)
        })?;
        let mut out = Vec::new();
        for part in parts {
            let uri = if part.drums { "builtin:drumkit" } else { melodic };
            let device =
                session.add_device(ChainRef::Track(part.track), uri, None, None).map_err(|error| error.to_string())?;
            let project = session.project();
            let clip = project.clip(part.clip).map_err(|e| e.to_string())?;
            let mut entry = json!({
                "track_id": part.track,
                "name": project.track(part.track).map_err(|e| e.to_string())?.name,
                "instrument_device_id": device,
                "clip_id": part.clip,
                "note_count": clip.notes().len(),
                "length": round(clip.length),
                "drums": part.drums,
            });
            if !clip.controllers().is_empty() {
                entry["controllers"] = clip.controllers().iter().map(|curve| curve.controller.key()).collect();
            }
            out.push(entry);
        }
        Ok(out)
    })();
    session.end_gesture();
    result
}

fn parse_time_signature(text: &str) -> Result<(u8, u8), String> {
    let invalid = || format!("time signature '{text}' should look like \"4/4\" or \"6/8\"");
    let (numerator, denominator) = text.split_once('/').ok_or_else(invalid)?;
    Ok((numerator.trim().parse().map_err(|_| invalid())?, denominator.trim().parse().map_err(|_| invalid())?))
}

/// Deterministic noise for humanizing, so a seed reproduces a result.
struct Noise(u32);

impl Noise {
    /// Uniform in -1..1.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f64 / 8_388_608.0 - 1.0
    }
}

#[derive(Clone)]
pub struct TrillServer {
    pub(crate) runtime: Arc<Runtime>,
    /// The runtime's session, which most tools need and nothing else.
    pub(crate) session: SharedSession,
    tool_router: ToolRouter<Self>,
    /// Set for a client that works beside the user: see [`crate::guard`].
    guard: Option<Arc<Guard>>,
}

impl TrillServer {
    /// A server for a client the user set up themselves, which may read and
    /// write wherever it is told.
    pub fn new(runtime: Arc<Runtime>) -> Self {
        let session = runtime.session.clone();
        Self { session, runtime, tool_router: Self::every_tool(), guard: None }
    }

    /// Every tool the server offers.
    fn every_tool() -> ToolRouter<Self> {
        Self::tool_router()
            + Self::music_router()
            + Self::sound_router()
            + Self::hearing_router()
            + Self::instrument_router()
            + Self::story_router()
            + Self::film_router()
            + Self::files_router()
            + Self::listening_router()
            + Self::studying_router()
            + Self::taking_router()
            + Self::wavetable_router()
            + Self::catalogue_router()
            + Self::playing_router()
    }

    /// The names of the tools the server offers.
    pub fn tool_names() -> Vec<String> {
        Self::every_tool().list_all().into_iter().map(|tool| tool.name.into_owned()).collect()
    }

    /// Whether this is the door of the assistant in the window, which is
    /// held to limits a client the user set up is not.
    pub(crate) fn is_guarded(&self) -> bool {
        self.guard.is_some()
    }

    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// A server for a client that works beside the user in the window. It
    /// offers the same tools, but writes only into the open project's
    /// folder or `folder`, will not replace a project that has unsaved
    /// changes, and cannot delete a project lost in a crash.
    pub fn guarded(runtime: Arc<Runtime>, folder: PathBuf) -> Self {
        Self { guard: Some(Arc::new(Guard { folder })), ..Self::new(runtime) }
    }

    /// Where a tool that was asked to write `kind` at `requested` may write
    /// it: wherever it was told, or for a guarded client only inside the
    /// working folders. `None` when no place was asked for.
    pub(crate) fn write_path(
        &self,
        session: &Session,
        requested: Option<&str>,
        kind: &str,
        endings: &[&str],
    ) -> Result<Option<PathBuf>, String> {
        match (requested, &self.guard) {
            (None, _) => Ok(None),
            (Some(requested), None) => Ok(Some(PathBuf::from(requested))),
            (Some(requested), Some(guard)) => guard.write_target(session, requested, kind, endings).map(Some),
        }
    }

    /// Where a tool writes `kind` when it was asked to write it at
    /// `requested`, and when it was given no place: `usual` then, for a
    /// client that is trusted as the user is. A guarded client's file goes
    /// under `usual`'s name into `folder` inside its first working folder,
    /// because the usual place may be anywhere a project file happens to be
    /// or the system's temporary folder, and what it makes may be large.
    pub(crate) fn write_path_or(
        &self,
        session: &Session,
        requested: Option<&str>,
        kind: &str,
        endings: &[&str],
        folder: &str,
        usual: impl FnOnce() -> PathBuf,
    ) -> Result<PathBuf, String> {
        if let Some(path) = self.write_path(session, requested, kind, endings)? {
            return Ok(path);
        }
        let usual = usual();
        match &self.guard {
            None => Ok(usual),
            Some(guard) => {
                let name = usual.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                guard.write_target(session, &format!("{folder}/{name}"), kind, endings)
            }
        }
    }

    /// Whether the open project may be replaced by another right now.
    fn may_replace_project(&self, session: &Session) -> Result<(), String> {
        self.guard.as_ref().map_or(Ok(()), |guard| guard.may_replace_project(session))
    }

    /// What a `library` reply always carries: the folders, how much is in
    /// them, and whether a scan is running.
    fn library_status(&self) -> Value {
        let library = &self.runtime.library;
        let index = library.index();
        let mut out = scan_json(&library.scan_status());
        out["folders"] = library.roots().iter().map(|root| json!(root.display().to_string())).collect();
        out["files"] = json!(index.len());
        out["sounds"] = json!(index.count(EntryKind::Audio));
        out["favorites"] = json!(library.favorites().len());
        if let Some(problem) = library.index_problem() {
            out["index_problem"] = json!(problem);
        }
        // A folder that is not where the library has it: what was known
        // of its files is still listed, and that must not pass for all
        // being well.
        let missing = library.missing_roots();
        if !missing.is_empty() {
            out["missing_folders"] = missing
                .iter()
                .map(|folder| {
                    json!({
                        "folder": folder.path.display().to_string(),
                        "files_known": folder.files,
                        "why": folder.reason,
                    })
                })
                .collect();
            out["hint"] = json!(
                "A folder of the library is not where it was (moved, renamed, or on a drive that is not there).                  Its files are still listed and cannot be played. missing_files says where it seems to be;                  `move_folder` with `path` and `to` says where it went, and keeps the favorites with their files."
            );
        }
        if library.roots().is_empty() {
            out["hint"] = json!(
                "The library has no folders. `suggestions` lists where samples usually are on this machine; \
                 `add_folder` adds one. Ask the user which folders they want."
            );
        }
        out
    }

    /// The presets of the device type `uri`, complete if they can be read
    /// within [`PRESET_WAIT`]. A plugin's presets are read in the
    /// background, the first time anyone asks; the wait for that is spent
    /// off the async threads and without the session.
    async fn presets_of(&self, uri: &str) -> PresetList {
        let factory = self.session.lock().factory();
        let uri = uri.to_string();
        let read = tokio::task::spawn_blocking(move || factory.presets_within(&uri, PRESET_WAIT));
        read.await.ok().flatten().unwrap_or_default()
    }

    /// What has to be done before a tool that was given a preset goes to
    /// the session with it, off the async threads and without the
    /// session. For a preset by name: the presets of `uri` are read, as
    /// far as that can be done in [`PRESET_WAIT`]. For a preset by the
    /// path of its file: a guarded client is refused a file it may not be
    /// pointed at, and the file is asked what it holds, so that the
    /// session, asking again, has the answer at once.
    pub(crate) async fn ready_presets(&self, uri: Option<&str>, preset: &Option<String>) -> Result<(), String> {
        let (Some(uri), Some(preset)) = (uri, preset) else { return Ok(()) };
        let Some(asked) = PresetFile::asked(preset) else {
            self.presets_of(uri).await;
            return Ok(());
        };
        if let Some(guard) = &self.guard {
            guard.may_read_preset(&asked.path)?;
        }
        let (factory, uri) = (self.session.lock().factory(), uri.to_string());
        let read = move || factory.preset_file(&uri, &asked.path, asked.within.as_deref());
        // What it says is the session's to act on.
        let _ = tokio::task::spawn_blocking(read).await;
        Ok(())
    }

    /// Waits, with the session free, until `waiting` says of it that there
    /// is nothing to wait for any more, or [`DEVICE_WAIT`] has passed: for
    /// a device that is being made, which the session's chores put in its
    /// place when it has arrived.
    async fn wait_while(&self, waiting: impl Fn(&Session) -> bool) {
        let deadline = std::time::Instant::now() + DEVICE_WAIT;
        while waiting(&self.session.lock()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits for the device `device` where it is still being made.
    pub(crate) async fn await_device(&self, device: DeviceId) {
        self.wait_while(|session| session.is_coming(device)).await;
    }

    /// Before a tool of [`WAITS_FOR_DEVICES`] runs: waits for the devices
    /// its arguments lead to, where any is still being made.
    async fn await_devices_of(&self, tool: &str, input: &Value) {
        if !WAITS_FOR_DEVICES.contains(&tool) {
            return;
        }
        let id = |key: &str| input.get(key).and_then(Value::as_u64);
        let aimed = input.get("target").and_then(|target| target.get("device")).and_then(Value::as_u64);
        let (device, track, clip) = (id("device").or(aimed), id("track"), id("clip"));
        if device.is_none() && track.is_none() && clip.is_none() {
            return;
        }
        self.wait_while(|session| {
            let project = session.project();
            let of_clip = clip.and_then(|clip| {
                project
                    .tracks
                    .iter()
                    .find(|track| track.clips.iter().any(|held| held.id.0 == clip))
                    .map(|track| track.id)
            });
            let on_track = |track: TrackId| {
                project.track(track).is_ok_and(|track| track.devices.iter().any(|device| session.is_coming(device.id)))
            };
            device.is_some_and(|device| session.is_coming(DeviceId(device)))
                || track.is_some_and(|track| on_track(TrackId(track)))
                || of_clip.is_some_and(on_track)
        })
        .await;
    }

    /// Waits for the running scan, up to what the caller asked for, off
    /// the async threads.
    async fn wait_for_scan(&self, wait_seconds: Option<f64>) {
        let seconds = wait_seconds.unwrap_or(DEFAULT_SCAN_WAIT);
        let seconds = if seconds.is_finite() { seconds.clamp(0.0, MAX_SCAN_WAIT) } else { DEFAULT_SCAN_WAIT };
        let library = self.runtime.library.clone();
        let _ = tokio::task::spawn_blocking(move || library.wait_until_idle(Duration::from_secs_f64(seconds))).await;
    }
}

/// The folder a tool was asked about, or why it will not do.
fn folder_argument(path: Option<&str>, action: &str) -> Result<PathBuf, String> {
    let path = path.map(str::trim).filter(|path| !path.is_empty());
    let path = path.ok_or_else(|| format!("{action} needs `path`: the folder"))?;
    Ok(std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)))
}

fn list_folders(folders: &[PathBuf]) -> String {
    if folders.is_empty() {
        "The library has no folders.".to_string()
    } else {
        let shown: Vec<String> = folders.iter().map(|folder| folder.display().to_string()).collect();
        format!("The library's folders: {}", shown.join(", "))
    }
}

fn warp_mode(input: WarpInput) -> WarpMode {
    match input {
        WarpInput::Off => WarpMode::Off,
        WarpInput::Repitch => WarpMode::Repitch,
        WarpInput::Beats => WarpMode::Beats,
        WarpInput::Tonal => WarpMode::Tonal,
    }
}

/// The library query a search request describes.
fn sound_query(request: &SearchSoundsRequest, folder: Option<PathBuf>) -> Result<Query, String> {
    let mut tags = Tags::EMPTY;
    for label in request.tags.iter().flatten() {
        let tag = Tag::from_label(label.trim()).ok_or_else(|| {
            let known: Vec<&str> = Tag::all().map(Tag::label).collect();
            format!("'{label}' is not a tag. Tags: {}", known.join(", "))
        })?;
        tags.insert(tag);
    }
    let kind = match request.kind.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None | Some("audio" | "sound" | "sounds") => Some(EntryKind::Audio),
        Some("midi") => Some(EntryKind::Midi),
        Some("project") => Some(EntryKind::Project),
        Some("preset") => Some(EntryKind::Preset),
        Some("instrument" | "instruments") => Some(EntryKind::Instrument),
        Some("any" | "all") => None,
        Some(other) => {
            return Err(format!(
                "kind '{other}' should be \"audio\", \"midi\", \"project\", \"preset\", \"instrument\" or \"any\""
            ));
        }
    };
    let key = match request.key.as_deref() {
        None => None,
        Some(text) => Some(Key::parse(text).ok_or_else(|| {
            format!("'{text}' is not a key; write it like \"Am\", \"F# minor\", \"Bb\" or \"C major\"")
        })?),
    };
    let tempo = (request.tempo_min.is_some() || request.tempo_max.is_some())
        .then(|| request.tempo_min.unwrap_or(0.0)..=request.tempo_max.unwrap_or(f32::MAX));
    let seconds = (request.seconds_min.is_some() || request.seconds_max.is_some())
        .then(|| request.seconds_min.unwrap_or(0.0)..=request.seconds_max.unwrap_or(f64::MAX));
    Ok(Query {
        text: request.text.clone().unwrap_or_default(),
        kind,
        tags,
        tempo,
        key,
        seconds,
        folder,
        favorites_only: request.favorites_only,
        include_unreadable: false,
        include_recordings: request.include_recordings,
        limit: Some(request.limit.unwrap_or(DEFAULT_SOUNDS).clamp(1, MAX_SOUNDS)),
    })
}

#[tool_router]
impl TrillServer {
    /// Overview of the whole song: tempo, tracks with their devices and clips, master chain, automation,
    /// transport state and any problems. Call this first, and again whenever you need current ids.
    #[tool(annotations(title = "Get project", read_only_hint = true))]
    fn get_project(&self, Parameters(request): Parameters<GetProjectRequest>) -> CallToolResult {
        let lost = self.runtime.recoverable().len();
        // Which plugins are loading is asked of the program, over a tenth
        // of a second if nobody has looked lately: not with the session held.
        let loads = self.session.lock().load_watch();
        loads.loading_now();
        let session = self.session.lock();
        let mut out = project_json(&session, request.include_notes);
        if lost > 0 {
            out["recoverable"] = json!(format!(
                "{lost} project(s) were not saved before the program last stopped. recover_project lists them."
            ));
        }
        finish(Ok(out))
    }

    /// Change song-wide settings: name, tempo, time signature, loop region, master volume. The tempo and the
    /// time signature are those the song begins with; set_tempo and set_time_signature change them from a
    /// later bar on.
    #[tool(annotations(title = "Set project settings"))]
    fn set_project(&self, Parameters(request): Parameters<SetProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let signature = request.time_signature.as_deref().map(parse_time_signature).transpose()?;
            let key = match request.key.as_deref().map(str::trim) {
                None => None,
                Some("none" | "") => Some(None),
                Some(text) => Some(Some(trill_core::SongKey::parse(text).ok_or_else(|| {
                    format!(
                        "'{text}' is not a key: say it like \"F# minor\", \"Eb major\", \"Am\" or \"C\", or \"none\""
                    )
                })?)),
            };
            edit(&mut session, |project| {
                if let Some(name) = &request.name {
                    project.name = name.clone();
                }
                if let Some(tempo) = request.tempo {
                    project.set_tempo(tempo)?;
                }
                if let Some((numerator, denominator)) = signature {
                    project.set_time_signature(numerator, denominator)?;
                }
                if let Some(region) = &request.loop_region {
                    project.set_loop(region.enabled, region.start, region.end)?;
                }
                if let Some(volume) = request.master_volume_db {
                    project.set_master_volume(volume)?;
                }
                if let Some(key) = key {
                    project.set_key(key);
                }
                Ok(())
            })?;
            let project = session.project();
            let mut out = json!({
                "name": project.name,
                "tempo": project.tempo,
                "time_signature": format!("{}/{}", project.time_signature.numerator, project.time_signature.denominator),
                "beats_per_bar": project.time_signature.beats_per_bar(),
                "loop": { "enabled": project.loop_region.enabled, "start": project.loop_region.start, "end": project.loop_region.end },
                "master_volume_db": project.master.volume_db,
            });
            if let Some(key) = project.key {
                out["key"] = json!(key.name());
            }
            if !project.tempo_changes.is_empty() || !project.meter_changes.is_empty() {
                out["note"] = json!(
                    "These are the tempo and the time signature the song begins with; it changes them further on \
                     (get_tempo_map)."
                );
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Set the tempo from a bar of the song on, so that the tempo changes inside the song: a jump, or with
    /// `ramp` a glide that arrives there (accelerando, ritardando). Nothing moves: every beat stays where it
    /// is and lasts as long as the tempo it is under says. Bar 1 is the tempo the song begins at. Replies
    /// with the whole tempo map, each change with its bar, beat and second.
    #[tool(annotations(title = "Set tempo at a bar"))]
    fn set_tempo(&self, Parameters(request): Parameters<SetTempoRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let bar = request.bar.unwrap_or(1);
            let within = request.beat.unwrap_or(1.0);
            if bar == 0 {
                return Err("bars are counted from 1".to_string());
            }
            if !within.is_finite() || within < 1.0 {
                return Err(format!("`beat` {within} is counted from 1 within the bar: 1 is the bar line"));
            }
            let found = session.project().meter_map().bar(bar - 1);
            if within - 1.0 >= found.length - 1e-9 {
                return Err(format!(
                    "bar {bar} is {} beats long ({}/{}), so its beats run from 1 to under {}: `beat` {within} is in \
                     a later bar",
                    round(found.length),
                    found.signature.numerator,
                    found.signature.denominator,
                    round(found.length + 1.0)
                ));
            }
            let beat = found.start + within - 1.0;
            let remove = request.remove.unwrap_or(false);
            let ramp = request.ramp.unwrap_or(false);
            let bpm = match (request.bpm, remove) {
                (Some(bpm), false) => bpm,
                (None, false) => return Err("give `bpm`, the tempo in beats per minute".to_string()),
                (_, true) => 0.0,
            };
            if remove && !session.project().tempo_changes.iter().any(|change| (change.beat - beat).abs() < 1e-6) {
                let map = tempo_map_json(session.project());
                let at: Vec<String> = map["tempo"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .map(|change| match change.get("beat_in_bar") {
                        Some(within) => format!("bar {} beat {within}", change["bar"]),
                        None => format!("bar {}", change["bar"]),
                    })
                    .collect();
                return Err(match (at.is_empty(), beat < 1e-6) {
                    (_, true) => "bar 1 holds the tempo the song begins at, which is set and not removed".to_string(),
                    (true, _) => "the tempo does not change anywhere in the song, so there is nothing to remove".into(),
                    (false, _) => format!("the tempo does not change there; it changes at {}", at.join(", ")),
                });
            }
            edit(&mut session, |project| match remove {
                true => project.remove_tempo_at(beat),
                false => project.set_tempo_at(beat, bpm, ramp),
            })?;
            let mut out = tempo_map_json(session.project());
            out["at_beat"] = json!(round(beat));
            Ok(out)
        })();
        finish(reply)
    }

    /// Set the time signature from a bar of the song on, so that the meter changes inside the song. A bar
    /// begins there. Nothing moves: every beat stays where it is, and the bars from there to the next change
    /// are counted in the new time signature. With `bars`, it is in force for that many bars and the time
    /// signature before it comes back. Replies with the whole map, and names any bar that a change now cuts
    /// short.
    #[tool(annotations(title = "Set time signature at a bar"))]
    fn set_time_signature(&self, Parameters(request): Parameters<SetTimeSignatureRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let bar = request.bar.unwrap_or(1);
            if bar == 0 {
                return Err("bars are counted from 1".to_string());
            }
            let bars = session.project().meter_map();
            let found = bars.bar(bar - 1);
            let beat = found.start;
            if request.remove.unwrap_or(false) {
                if !session.project().meter_changes.iter().any(|change| (change.beat - beat).abs() < 1e-6) {
                    let at: Vec<String> =
                        bars.sections().skip(1).map(|section| format!("bar {}", section.number())).collect();
                    return Err(match (at.is_empty(), bar == 1) {
                        (_, true) => "bar 1 holds the time signature the song begins in, which is set and not \
                                      removed"
                            .to_string(),
                        (true, _) => "the time signature does not change anywhere in the song".to_string(),
                        (false, _) => {
                            format!("the time signature does not change at bar {bar}; it changes at {}", at.join(", "))
                        }
                    });
                }
                edit(&mut session, |project| project.remove_time_signature_at(beat))?;
                let mut out = tempo_map_json(session.project());
                out["at_beat"] = json!(round(beat));
                return Ok(out);
            }
            let Some(text) = request.time_signature.as_deref() else {
                return Err("give `time_signature`, for example \"7/8\"".to_string());
            };
            let (numerator, denominator) = parse_time_signature(text)?;
            let Some(signature) = trill_core::TimeSignature::new(numerator, denominator) else {
                return Err(format!("unsupported time signature {text}: 1 to 32 over 1, 2, 4, 8 or 16"));
            };
            // For so many bars: where they end, the one that was here takes over again.
            let back = match request.bars {
                Some(0) => return Err("`bars` is how many bars the time signature lasts: at least 1".to_string()),
                Some(count) => {
                    let end = beat + f64::from(count) * signature.beats_per_bar();
                    let inside = session
                        .project()
                        .meter_changes
                        .iter()
                        .find(|change| change.beat > beat + 1e-6 && change.beat < end - 1e-6);
                    if let Some(inside) = inside {
                        return Err(format!(
                            "{count} bar(s) of {text} from bar {bar} would run to beat {}, past the change of time \
                             signature at bar {} (beat {}): give fewer `bars`, or remove that change first",
                            round(end),
                            bars.bar_at(inside.beat).number(),
                            round(inside.beat)
                        ));
                    }
                    let already = session.project().meter_changes.iter().any(|change| (change.beat - end).abs() < 1e-6);
                    (!already && found.signature != signature).then_some((end, found.signature))
                }
                None => None,
            };
            edit(&mut session, |project| {
                project.set_time_signature_at(beat, numerator, denominator)?;
                if let Some((end, before)) = back {
                    project.set_time_signature_at(end, before.numerator, before.denominator)?;
                }
                Ok(())
            })?;
            let mut out = tempo_map_json(session.project());
            out["at_beat"] = json!(round(beat));
            if let Some((end, _)) = back {
                out["until_beat"] = json!(round(end));
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// The tempo and the time signature along the song: what it begins with and every change, each with its
    /// bar, the beat it lies at and the second the song reaches it. With `bars`, where those bars start. Use it
    /// to turn a bar number into the beat a clip or an edit needs, in a song whose time signature changes.
    #[tool(annotations(title = "Get tempo map", read_only_hint = true))]
    fn get_tempo_map(&self, Parameters(request): Parameters<GetTempoMapRequest>) -> CallToolResult {
        let session = self.session.lock();
        let reply = (|| {
            let project = session.project();
            let mut out = tempo_map_json(project);
            let (tempo, bars) = (project.tempo_map(), project.meter_map());
            let length = project.length_beats();
            out["length_beats"] = json!(round(length));
            out["length_bars"] = json!(round(bars.bars_at(length)));
            out["length_seconds"] = json!(round(tempo.seconds_at(length)));
            if let Some(asked) = &request.bars {
                if asked.len() > 256 || asked.contains(&0) {
                    return Err("`bars` takes up to 256 bar numbers, counted from 1".to_string());
                }
                let found: Vec<Value> = asked
                    .iter()
                    .map(|number| {
                        let bar = bars.bar(number - 1);
                        json!({
                            "bar": number,
                            "beat": round(bar.start),
                            "beats": round(bar.length),
                            "seconds": round(tempo.seconds_at(bar.start)),
                            "time_signature": format!("{}/{}", bar.signature.numerator, bar.signature.denominator),
                            "bpm": round(tempo.tempo_at(bar.start)),
                        })
                    })
                    .collect();
                out["bars"] = json!(found);
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Add a track, optionally with an instrument. MIDI clips on the track play through its instrument.
    /// `kind` makes a group track (route other tracks through it with update_track `group`) or a return
    /// track (feed it with update_track `send`, and put a shared reverb or delay on it).
    #[tool(annotations(title = "Create track"))]
    async fn create_track(&self, Parameters(request): Parameters<CreateTrackRequest>) -> CallToolResult {
        if let Err(refusal) = self.ready_presets(request.instrument.as_deref(), &request.preset).await {
            return finish(Err(format!("{refusal} No track was created.")));
        }
        let mut session = self.session.lock();
        let reply = (|| {
            if request.preset.is_some() && request.instrument.is_none() {
                return Err("`preset` needs `instrument`".to_string());
            }
            let kind = match request.kind {
                TrackKindInput::Normal => TrackKind::Normal,
                TrackKindInput::Group => TrackKind::Group,
                TrackKindInput::Return => TrackKind::Return,
            };
            let track = edit(&mut session, |project| {
                let id = project.add_track_of_kind(request.name.clone(), kind);
                if let Some(volume) = request.volume_db {
                    project.set_track_volume(id, volume)?;
                }
                if let Some(pan) = request.pan {
                    project.set_track_pan(id, pan)?;
                }
                Ok(id)
            })?;
            let mut out = json!({ "track_id": track });
            if let Some(uri) = &request.instrument {
                match session.add_given(ChainRef::Track(track), uri, request.preset.as_deref(), None) {
                    Ok(added) => {
                        let device = added.device;
                        out["instrument_device_id"] = json!(device);
                        if let Some(preset) = session.device_preset(device) {
                            out["preset"] = json!(preset.name);
                        }
                        crate::sounds::given_into(&mut out, &session, added.given.as_ref());
                        crate::sounds::preset_note_into(&mut out, &mut session, device);
                        crate::sounds::instrument_into(&mut out, &session, device);
                        crate::view::not_made_yet_into(&mut out, &session, device);
                    }
                    Err(error) => {
                        session.undo();
                        let error = preset_hint(error.to_string(), uri);
                        return Err(format!("{error}. No track was created; list_devices shows what is available."));
                    }
                }
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Change a track's name, volume, pan, mute, solo or position in the track list, route it through a
    /// group, or send it to a return track; make it a reference or a track of the song again, and listen to
    /// a reference.
    #[tool(annotations(title = "Update track"))]
    fn update_track(&self, Parameters(request): Parameters<UpdateTrackRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = TrackId(request.track);
        let reply = edit(&mut session, |project| {
            if let Some(volume) = request.volume_db {
                project.set_track_volume(id, volume)?;
            }
            if let Some(pan) = request.pan {
                project.set_track_pan(id, pan)?;
            }
            if let Some(index) = request.index {
                project.move_track(id, index)?;
            }
            if let Some(group) = request.group {
                project.set_track_group(id, (group != 0).then_some(TrackId(group)))?;
            }
            if let Some(send) = &request.send {
                project.set_send(id, TrackId(send.to), send.level_db)?;
            }
            match request.reference {
                Some(true) if project.track(id)?.reference.is_none() => {
                    project.set_reference(id, Some(trill_core::Reference::default()))?;
                }
                Some(false) => project.set_reference(id, None)?,
                _ => {}
            }
            if let Some(listening) = request.listening {
                project.listen_to_reference(id, listening)?;
            }
            let track = project.track_mut(id)?;
            if let Some(name) = &request.name {
                track.name = name.clone();
            }
            if let Some(mute) = request.mute {
                track.mute = mute;
            }
            if let Some(solo) = request.solo {
                track.solo = solo;
            }
            // The clips are not what was asked about, so neither are their bars.
            let mut out = track_json(track, &trill_core::MeterMap::constant(Default::default()), false);
            if let Some(fields) = out.as_object_mut() {
                fields.remove("clips");
            }
            Ok(out)
        });
        // A send that an envelope moves is said to be, so that the level
        // just set is not taken for what is heard.
        let reply = reply.map(|mut out| {
            let project = session.project();
            if let Some(sends) = project.track(id).ok().and_then(|track| crate::view::sends_json(project, track)) {
                out["sends"] = sends;
            }
            out
        });
        // Arming is the session's, not the project's: no undo step.
        let reply = reply.and_then(|mut out| {
            if let Some(armed) = request.armed {
                session.set_armed(id, armed).map_err(|error| explain(session.project(), error))?;
            }
            if session.is_armed(id) {
                out["armed"] = json!(true);
            }
            Ok(out)
        });
        finish(reply)
    }

    /// Delete a track with its clips, devices and automation.
    #[tool(annotations(title = "Delete track", destructive_hint = true))]
    fn delete_track(&self, Parameters(request): Parameters<TrackRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = edit(&mut session, |project| project.remove_track(TrackId(request.track)))
            .map(|track| json!({ "deleted": track.name }));
        finish(reply)
    }

    /// The catalog of instruments and effects that can be added. Built-in devices have `builtin:` URIs and are
    /// listed with their presets; installed plugins appear with `clap:` and `vst3:` URIs (a plugin installed in
    /// both formats is listed once per format; either works), and `describe_device` lists and searches theirs.
    /// `preset_count` says how many presets a plugin has, where they have been read; search_presets reads and
    /// searches them all at once.
    #[tool(annotations(title = "List devices", read_only_hint = true))]
    fn list_devices(&self, Parameters(request): Parameters<ListDevicesRequest>) -> CallToolResult {
        // Read outside the session lock: the first plugin scan can take
        // seconds, and the GUI must not wait on it.
        let factory = self.session.lock().factory();
        let reply = (|| {
            let category = match request.category.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None => None,
                Some("instrument" | "instruments") => Some(DeviceCategory::Instrument),
                Some("effect" | "effects") => Some(DeviceCategory::Effect),
                Some(other) => return Err(format!("category '{other}' should be \"instrument\" or \"effect\"")),
            };
            let query = request.query.as_deref().map(str::to_ascii_lowercase);
            let devices: Vec<Value> = factory
                .descriptors()
                .iter()
                .filter(|d| category.is_none_or(|c| d.category == c))
                .filter(|d| {
                    query.as_deref().is_none_or(|q| {
                        [&d.name, &d.vendor, &d.uri].iter().any(|field| field.to_ascii_lowercase().contains(q))
                    })
                })
                .map(|descriptor| {
                    let mut out = descriptor_summary_json(descriptor);
                    crate::sounds::known_presets_into(&mut out, factory.as_ref(), descriptor);
                    out
                })
                .collect();
            Ok(json!({ "devices": devices }))
        })();
        finish(reply)
    }

    /// Everything about one device type: what it does, every parameter with range and unit, and its presets.
    /// A plugin may have thousands of presets, so they come a page at a time: `preset_query` searches them
    /// (name, category, tags, author), `preset_category` keeps to one category, and the reply says how many
    /// there are, what kinds, and where the next page starts. A plugin's presets are read the first time they
    /// are asked for, which can take a while; the reply says when they are still being read.
    #[tool(annotations(title = "Describe device", read_only_hint = true))]
    async fn describe_device(&self, Parameters(request): Parameters<DescribeDeviceRequest>) -> CallToolResult {
        let factory = self.session.lock().factory();
        let descriptors = factory.descriptors();
        let Some(descriptor) = descriptors.iter().find(|descriptor| descriptor.uri == request.uri) else {
            let unknown = format!("unknown device '{}'; list_devices shows what is available", request.uri);
            return finish(Err(unknown));
        };
        let search = PresetSearch {
            query: request.preset_query.as_deref(),
            category: request.preset_category.as_deref(),
            offset: request.preset_offset.unwrap_or(0),
            limit: request.preset_limit.unwrap_or(DEFAULT_PRESETS).min(MAX_PRESETS),
        };
        let list = self.presets_of(&request.uri).await;
        let saved = self.session.lock().saved_presets().clone();
        let mut out = descriptor_json(descriptor);
        let after = |preset: &trill_core::presets::PresetInfo| trill_engine::sound_after(descriptor, &saved, preset);
        out["presets"] = presets_json_with(&list, &search, &after);
        // A plugin that lists no presets in one format may list them in
        // another: which of its formats a plugin was built to tell a host
        // of its presets in is its maker's choice.
        if list.is_empty() && !list.is_reading() {
            let others: Vec<String> = descriptors
                .iter()
                .filter(|other| other.uri != descriptor.uri && is_plugin(&other.uri))
                .filter(|other| other.name.eq_ignore_ascii_case(&descriptor.name))
                .filter_map(|other| {
                    let count = factory.presets(&other.uri)?.len();
                    Some(match count {
                        0 => format!("{} (describe_device it to see whether it lists presets)", other.uri),
                        count => format!("{} lists {count} presets", other.uri),
                    })
                })
                .collect();
            if is_plugin(&descriptor.uri) {
                out["presets"]["note"] = json!(match others.is_empty() {
                    true => crate::sounds::NO_PRESETS.to_string(),
                    false => format!(
                        "{} The same plugin in another format: {}.",
                        crate::sounds::NO_PRESETS,
                        others.join("; ")
                    ),
                });
            }
        }
        finish(Ok(out))
    }

    /// Add an instrument or effect to a track's chain, or to the master chain when `track` is omitted. `preset`
    /// starts it from one of its presets, a plugin's included (describe_device lists and searches them), or
    /// from a preset file by its path; `params` go on top of the preset. `sidechain` has a compressor listen to
    /// another track.
    #[tool(annotations(title = "Add device"))]
    async fn add_device(&self, Parameters(request): Parameters<AddDeviceRequest>) -> CallToolResult {
        if let Err(refusal) = self.ready_presets(Some(&request.uri), &request.preset).await {
            return finish(Err(format!("{refusal} The device was not added.")));
        }
        let chain = request.track.map_or(ChainRef::Master, |track| ChainRef::Track(TrackId(track)));
        let (before, added) = {
            let mut session = self.session.lock();
            let before = session.undo_step();
            let added = session
                .add_given(chain, &request.uri, request.preset.as_deref(), request.index)
                .map_err(|error| preset_hint(explain(session.project(), error), &request.uri));
            (before, added)
        };
        let added = match added {
            Ok(added) => added,
            Err(error) => return finish(Err(error)),
        };
        let device = added.device;
        if request.params.is_some() {
            // The song has the device; where it is still being made, its
            // parameters are set when it is there.
            self.await_device(device).await;
        }
        let mut session = self.session.lock();
        let reply = (|| {
            let rest: Result<(), String> = (|| {
                if let Some(params) = &request.params {
                    set_params(&mut session, device, params)?;
                }
                if let Some(track) = request.sidechain {
                    let listening = session.set_sidechain(device, Some(TrackId(track)));
                    listening.map_err(|error| explain(session.project(), error))?;
                }
                Ok(())
            })();
            if let Err(error) = rest {
                // Back to before the device was added, whatever of it was done.
                while session.undo_step() != before && session.undo() {}
                return Err(format!("{error}. The device was not added."));
            }
            let model = session.project().device(device).map_err(|e| e.to_string())?;
            let mut out = json!({ "device_id": device, "name": model.name, "uri": model.uri });
            if let Some(preset) = &model.preset {
                out["preset"] = json!(preset.name);
            }
            sidechain_into(&mut out, session.project(), model);
            crate::sounds::given_into(&mut out, &session, added.given.as_ref());
            crate::sounds::preset_note_into(&mut out, &mut session, device);
            crate::sounds::instrument_into(&mut out, &session, device);
            crate::view::not_made_yet_into(&mut out, &session, device);
            Ok(out)
        })();
        finish(reply)
    }

    /// A device as it is now: its parameters with their values, the channels it takes and gives, the preset it
    /// was last given, and how many presets it can be given. A plugin's parameter is a number from 0 to 1 whose
    /// meaning only the plugin knows, so each comes with `text`, the value as the plugin shows it ("-6.0 dB",
    /// "Dynamic 57", "On"); update_device sets a parameter by that text as well as by number. A device with
    /// many parameters is shown by those not at its own defaults: `query` finds parameters by name (with their
    /// ranges and choices), `show: "all"` lists every one, a page at a time. `preset_query` and the other
    /// `preset_` arguments search and page the presets, as in describe_device. For the sample players
    /// (builtin:sampler, builtin:drumrack) also the audio files they play, per slot, with what is wrong with
    /// any that is missing. For a sampled instrument (builtin:sampled) also `instrument`: what it has loaded
    /// and what that is, enough to write for it.
    #[tool(annotations(title = "Get device", read_only_hint = true))]
    async fn get_device(&self, Parameters(request): Parameters<GetDeviceRequest>) -> CallToolResult {
        let id = DeviceId(request.device);
        // Asked for presets, the reply waits for them to have been read:
        // before the session is taken, not while it is held.
        let presets_wanted = request.preset_query.is_some()
            || request.preset_category.is_some()
            || request.preset_offset.is_some()
            || request.preset_limit.is_some();
        let uri = self.session.lock().project().device(id).map(|device| device.uri.clone()).ok();
        let searched = match (&uri, presets_wanted) {
            (Some(uri), true) => Some(self.presets_of(uri).await),
            _ => None,
        };
        let session = self.session.lock();
        let reply = (|| {
            let project = session.project();
            let model = project.device(id).map_err(|error| explain(project, error))?;
            // One that is not made yet has no parameters to list, and says
            // so below in place of them.
            let states = match session.is_coming(id) {
                true => Vec::new(),
                false => session.device_params(id).map_err(|error| explain(project, error))?,
            };
            let mut out = device_json(model);
            out["enabled"] = json!(model.enabled);
            sidechain_into(&mut out, project, model);
            if model.sidechain.is_none() && session.device_takes_key(id) {
                out["sidechain_hint"] =
                    json!("Listens to its own input. update_device `sidechain` has it listen to another track.");
            }
            if session.loading().contains(&id) {
                out["loading"] = json!(true);
            }
            if session.is_coming(id) {
                out["not_made_yet"] = json!(
                    "The song has this device, and it is still being made: its parameters are listed when it is \
                     there. Ask again in a few seconds; get_project says how far the song's devices are."
                );
            }
            out["chain"] = match project.device_chain(id).map_err(|e| e.to_string())? {
                ChainRef::Master => json!("master"),
                ChainRef::Track(track) => json!({ "track": track }),
            };
            if [SAMPLER_URI, DRUM_RACK_URI].contains(&model.uri.as_str()) {
                let files = session.sample_files(id).map_err(|error| explain(project, error))?;
                if files.is_empty() {
                    out["samples_hint"] = json!("No sample loaded, so the device is silent. Use set_sample.");
                }
                out["samples"] = samples_json(&files);
            }
            if model.uri == trill_dsp::sampled::URI {
                crate::sounds::instrument_into(&mut out, &session, id);
                if out.get("instrument").is_none() {
                    out["instrument_hint"] = json!(
                        "No instrument loaded, so the device is silent. update_device `preset` takes the whole path \
                         of an instrument's .sfz file; search_presets finds the ones in the sample library."
                    );
                }
            }
            crate::wavetable::device_into(&mut out, &session, model);
            if session.device_has_editor(id) {
                out["editor"] = json!(if session.is_device_editor_open(id) { "open" } else { "closed" });
            }
            if let Ok(channels) = session.device_channels(id) {
                out["channels"] = json!(channels.describe());
                if channels.folds_input() {
                    out["channels_note"] = json!(
                        "Left and right are mixed into this device's one input channel: whatever differed between \
                         the sides before it (a doubled guitar, a stereo effect) is the same on both from here on."
                    );
                }
            }
            match &model.preset {
                // An effect a sound brought says so, not that it is on a
                // preset of its own type.
                Some(preset) if preset.bringer().is_some() => out["from_sound"] = json!(preset.name),
                Some(preset) => out["preset"] = json!(preset.name),
                None => {}
            }
            let after = |preset: &trill_core::presets::PresetInfo| session.sound_after(&model.uri, preset);
            match &searched {
                Some(list) => {
                    let search = PresetSearch {
                        query: request.preset_query.as_deref(),
                        category: request.preset_category.as_deref(),
                        offset: request.preset_offset.unwrap_or(0),
                        limit: request.preset_limit.unwrap_or(DEFAULT_PRESETS).min(MAX_PRESETS),
                    };
                    out["presets"] = presets_json_with(list, &search, &after);
                }
                // Not asked for: how many there are, and the presets
                // themselves only where they are few.
                None => {
                    let list = session.presets(&model.uri);
                    let limit = if list.len() <= UNASKED_PRESETS { UNASKED_PRESETS } else { 0 };
                    let shown = PresetSearch { limit, ..PresetSearch::default() };
                    let mut presets = presets_json_with(&list, &shown, &after);
                    if let (0, Some(fields)) = (limit, presets.as_object_mut()) {
                        fields.remove("items");
                        fields.remove("kinds");
                        fields.insert("more".into(), json!("preset_query searches them; preset_offset pages through"));
                    }
                    out["presets"] = presets;
                }
            }
            if is_plugin(&model.uri) && out["presets"]["total"] == 0 && out["presets"].get("status").is_none() {
                out["presets"]["note"] = json!(crate::sounds::NO_PRESETS);
            }
            // A reply about presets carries the parameters only if they
            // were asked about too.
            if searched.is_some() && request.query.is_none() && request.show.is_none() {
                return Ok(out);
            }
            params_into(&mut out, &session, id, &states, &request)?;
            crate::sounds::settings_into(&mut out, &session, id, request.query.as_deref());
            Ok(out)
        })();
        finish(reply)
    }

    /// Change a device: switch preset, set parameters or settings, bypass it, or have it listen to another
    /// track (`sidechain`). A preset replaces the device's sound, a plugin's as its own preset browser would
    /// (get_device and describe_device list and search them; a preset file is given by its path); `params` in
    /// the same call go on top of it, and the whole call is one undo step.
    #[tool(annotations(title = "Update device"))]
    async fn update_device(&self, Parameters(request): Parameters<UpdateDeviceRequest>) -> CallToolResult {
        let id = DeviceId(request.device);
        let uri = self.session.lock().project().device(id).map(|device| device.uri.clone()).ok();
        if let Err(refusal) = self.ready_presets(uri.as_deref(), &request.preset).await {
            return finish(Err(format!("{refusal} The device is as it was.")));
        }
        let mut session = self.session.lock();
        let reply = (|| {
            // Validate before touching anything, so a bad value changes nothing.
            let settings = crate::sounds::settings_asked(request.settings.as_ref())?;
            let resolved = match &request.params {
                Some(params) => resolve_params(&session, id, params),
                None => Ok(Vec::new()),
            };
            if (resolved.is_err() || !settings.is_empty())
                && let Some(sorted) = misplaced(&session, id, request.params.as_ref(), &settings)
            {
                return Err(sorted);
            }
            let resolved = resolved?;
            session.begin_gesture();
            let mut loaded = None;
            let result: Result<(), String> = (|| {
                if let Some(preset) = &request.preset {
                    let given = session.give_preset(id, preset).map_err(|error| {
                        preset_hint(explain(session.project(), error), uri.as_deref().unwrap_or_default())
                    })?;
                    loaded = Some(given);
                }
                if !resolved.is_empty() {
                    session.set_device_params(id, &resolved).map_err(|error| explain(session.project(), error))?;
                }
                if !settings.is_empty() {
                    session.set_device_settings(id, &settings).map_err(|error| explain(session.project(), error))?;
                }
                if let Some(enabled) = request.enabled {
                    edit(&mut session, |project| {
                        project.device_mut(id)?.enabled = enabled;
                        Ok(())
                    })?;
                }
                if let Some(track) = request.sidechain {
                    let track = (track != 0).then_some(TrackId(track));
                    session.set_sidechain(id, track).map_err(|error| explain(session.project(), error))?;
                }
                Ok(())
            })();
            session.end_gesture();
            result?;
            let shown = shown_values(&session, id, &resolved);
            let changed: BTreeMap<String, f64> = resolved.into_iter().collect();
            let model = session.project().device(id).map_err(|e| e.to_string())?;
            let mut out = json!({ "device_id": id, "name": model.name, "enabled": model.enabled, "set": changed });
            if let Some(shown) = shown {
                out["shown"] = shown;
            }
            let given = loaded.is_some();
            if let Some(sound) = &loaded {
                out["preset"] = json!(sound.preset.path());
                crate::sounds::given_into(&mut out, &session, Some(sound));
            }
            if !settings.is_empty() {
                out["settings"] = json!(settings.into_iter().collect::<BTreeMap<String, String>>());
            }
            if request.sidechain.is_some() {
                sidechain_into(&mut out, session.project(), model);
                if model.sidechain.is_none() {
                    out["sidechain"] = json!(null);
                }
            }
            crate::sounds::preset_note_into(&mut out, &mut session, id);
            if given {
                crate::sounds::instrument_into(&mut out, &session, id);
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Remove a device from its chain.
    #[tool(annotations(title = "Delete device", destructive_hint = true))]
    fn delete_device(&self, Parameters(request): Parameters<DeviceRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = edit(&mut session, |project| project.remove_device(DeviceId(request.device)))
            .map(|device| json!({ "deleted": device.name }));
        finish(reply)
    }

    /// Create a MIDI clip on a track, optionally filled with notes.
    #[tool(annotations(title = "Create clip"))]
    fn create_clip(&self, Parameters(request): Parameters<CreateClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let keys = crate::instruments::TrackKeys::of(&session, TrackId(request.track));
            let channel = keys.channel(request.channel.as_ref())?;
            let (notes, how) = keys.played(request.notes.as_deref().unwrap_or_default(), channel)?;
            // Where a note says how it is played, the instrument's keyswitches go in with the notes.
            let played = crate::instruments::played(&session, TrackId(request.track), &[], &notes, &how)?;
            let written = played.as_ref().map_or(&notes, |played| &played.all);
            let curves = parse_curves(request.controllers.as_ref())?;
            let name = request.name.clone().unwrap_or_else(|| "Clip".to_string());
            let clip = edit(&mut session, |project| {
                let id = project.add_midi_clip(TrackId(request.track), name, request.start, request.length)?;
                let clip = project.clip_mut(id)?;
                clip.add_notes(written)?;
                if let Some(loop_length) = request.loop_length {
                    clip.set_loop_length(Some(loop_length))?;
                }
                for (kind, points) in &curves {
                    clip.set_controller_on(channel, *kind, points.clone())?;
                }
                Ok(id)
            })?;
            let mut out = json!({ "clip_id": clip, "note_count": notes.len() });
            say_played(&mut out, played.as_ref());
            if !curves.is_empty() {
                out["controllers"] = curves.iter().map(|(kind, _)| kind.key()).collect();
                add_controller_warnings(&mut out, &session, clip, curves.iter().map(|(kind, _)| (channel, *kind)));
            }
            add_range_warning(&mut out, &session, clip);
            say_channels(&mut out, &session, clip, &keys, notes.iter().map(|note| note.channel).chain([channel]));
            Ok(out)
        })();
        finish(reply)
    }

    /// Place an audio file (WAV, AIFF, FLAC, MP3, Ogg) on a track as an audio clip. By default it plays at its
    /// recorded speed and pitch. With `warp` it follows the project tempo instead, which is what a loop needs:
    /// its beats land on the song's beats and stay there when the tempo changes.
    #[tool(annotations(title = "Create audio clip"))]
    fn create_audio_clip(&self, Parameters(request): Parameters<CreateAudioClipRequest>) -> CallToolResult {
        let sound = SoundClip {
            length: request.length,
            name: request.name.clone(),
            offset_seconds: request.offset_seconds.unwrap_or(0.0),
            gain_db: request.gain_db.unwrap_or(0.0),
            fade_in: request.fade_in.unwrap_or(0.0),
            fade_out: request.fade_out.unwrap_or(0.0),
            warp: request.warp.map_or(WarpMode::Off, warp_mode),
            source_tempo: request.source_tempo,
            transpose: request.transpose.unwrap_or(0.0),
            ..SoundClip::new(&request.path, TrackId(request.track), request.start)
        };
        let reply = match self.runtime.add_sound(&sound) {
            Ok(added) => {
                let session = self.session.lock();
                let mut out = json!({
                    "clip_id": added.clip,
                    "length": round(added.length),
                    "file_seconds": round(added.file_seconds),
                });
                if let Some(tempo) = added.source_tempo {
                    out["source_tempo"] = json!(round(tempo));
                }
                if sound.warp != WarpMode::Off {
                    out["warp"] = json!(warp_name(sound.warp));
                }
                add_audio_notes(&mut out, &self.runtime, &session, added.clip);
                Ok(out)
            }
            Err(SoundError::Session(error)) => Err(explain(self.session.lock().project(), error)),
            Err(error) => Err(error.to_string()),
        };
        finish(reply)
    }

    /// Rename, move, resize, loop or mute a clip, or move it to another track. For audio clips also: fades, and
    /// how the clip follows the project tempo (warp, source_tempo, transpose).
    #[tool(annotations(title = "Update clip"))]
    fn update_clip(&self, Parameters(request): Parameters<UpdateClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = (|| {
            if request.trim_start.is_some() && request.start.is_some() {
                return Err("give either `start` (move the clip) or `trim_start` (move its left edge), not both".into());
            }
            let warping = request.warp.is_some() || request.source_tempo.is_some() || request.transpose.is_some();
            // A clip being warped needs its recording's tempo. If it has
            // none and none is given, its file name is asked.
            let named_tempo = match session.project().clip(id).ok().and_then(|clip| clip.audio()) {
                Some(audio) if warping && audio.source_tempo.is_none() && request.source_tempo.is_none() => {
                    self.runtime.sound_facts(Path::new(&audio.path)).ok().and_then(|facts| facts.tempo)
                }
                _ => None,
            };
            edit(&mut session, |project| {
                if warping {
                    let Some(audio) = project.clip(id)?.audio() else {
                        return Err(EditError::Invalid(format!("{id} is a MIDI clip; only audio clips can be warped")));
                    };
                    let warp = request.warp.map_or(audio.warp, warp_mode);
                    let source_tempo = request.source_tempo.or(audio.source_tempo).or(named_tempo);
                    if warp != WarpMode::Off && source_tempo.is_none() {
                        return Err(EditError::Invalid(format!(
                            "'{}' does not say in its name what tempo it was recorded at, so it cannot follow the \
                             project's; give `source_tempo` (its BPM)",
                            audio.path
                        )));
                    }
                    let transpose = request.transpose.unwrap_or(audio.transpose);
                    project.set_clip_warp(id, warp, source_tempo, transpose)?;
                }
                if let Some(edge) = request.trim_start {
                    project.trim_clip_start(id, edge)?;
                }
                let current = project.clip(id)?;
                let (start, length) =
                    (request.start.unwrap_or(current.start), request.length.unwrap_or(current.length));
                project.set_clip_span(id, start, length)?;
                if request.fade_in.is_some() || request.fade_out.is_some() {
                    let (fade_in, fade_out) = project.clip(id)?.audio().map_or((0.0, 0.0), |a| (a.fade_in, a.fade_out));
                    project.set_clip_fades(
                        id,
                        request.fade_in.unwrap_or(fade_in),
                        request.fade_out.unwrap_or(fade_out),
                    )?;
                }
                let clip = project.clip_mut(id)?;
                if let Some(name) = &request.name {
                    clip.name = name.clone();
                }
                if let Some(muted) = request.muted {
                    clip.muted = muted;
                }
                if let Some(loop_length) = request.loop_length {
                    let map = project.tempo_map();
                    let found = project.clip(id)?;
                    // An audio clip plays round the stretch of its recording that so many beats of it
                    // cover from where it begins; a notes clip its first so many beats of notes.
                    match found.audio().map(|audio| audio.played(&map, found.start, loop_length)) {
                        Some(played) => project.set_audio_loop(id, (loop_length > 0.0).then_some(played))?,
                        None => project.clip_mut(id)?.set_loop_length((loop_length > 0.0).then_some(loop_length))?,
                    }
                }
                if let Some(track) = request.track {
                    let start = project.clip(id)?.start;
                    project.move_clip(id, Some(TrackId(track)), start)?;
                }
                Ok(())
            })?;
            let project = session.project();
            let bars = project.meter_map();
            let mut out = clip_json(project.clip(id).map_err(|e| e.to_string())?, &bars, false);
            out["track"] = json!(project.clip_track(id).map_err(|e| e.to_string())?);
            add_range_warning(&mut out, &session, id);
            add_audio_notes(&mut out, &self.runtime, &session, id);
            Ok(out)
        })();
        finish(reply)
    }

    /// Cut a clip in two at a timeline position. Both halves keep playing exactly what they played before;
    /// use it to remove, move or change part of a clip.
    #[tool(annotations(title = "Split clip"))]
    fn split_clip(&self, Parameters(request): Parameters<SplitClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = edit(&mut session, |project| {
            let second = project.split_clip(id, request.at)?;
            let (first, second) = (project.clip(id)?, project.clip(second)?);
            Ok(json!({
                "first": { "clip_id": first.id, "start": round(first.start), "length": round(first.length) },
                "second": { "clip_id": second.id, "start": round(second.start), "length": round(second.length) },
            }))
        });
        finish(reply)
    }

    /// Copy a clip one or more times, back to back. The quickest way to repeat a section.
    #[tool(annotations(title = "Duplicate clip"))]
    fn duplicate_clip(&self, Parameters(request): Parameters<DuplicateClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = edit(&mut session, |project| {
            let source = project.clip(id)?;
            let (first, length) = (request.start.unwrap_or(source.end()), source.length);
            let count = request.count.unwrap_or(1).clamp(1, 256);
            let track = request.track.map(TrackId);
            let mut copies = Vec::new();
            for index in 0..count {
                let start = first + index as f64 * length;
                copies.push(json!({ "clip_id": project.duplicate_clip(id, start, track)?, "start": round(start) }));
            }
            Ok(json!({ "copies": copies }))
        });
        finish(reply)
    }

    /// Delete a clip, with its notes, from the timeline.
    #[tool(annotations(title = "Delete clip", destructive_hint = true))]
    fn delete_clip(&self, Parameters(request): Parameters<ClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = edit(&mut session, |project| project.remove_clip(ClipId(request.clip)))
            .map(|clip| json!({ "deleted": clip.name }));
        finish(reply)
    }

    /// Read the notes of a MIDI clip.
    #[tool(annotations(title = "Get notes", read_only_hint = true))]
    fn get_notes(&self, Parameters(request): Parameters<ClipRequest>) -> CallToolResult {
        let session = self.session.lock();
        let project = session.project();
        let reply = project
            .clip(ClipId(request.clip))
            .map(|clip| clip_json(clip, &project.meter_map(), true))
            .map_err(|error| explain(project, error));
        finish(reply)
    }

    /// Read the controller curves of a MIDI clip: the movement of expression, the mod wheel, the pitch wheel,
    /// the pedals and any other MIDI controller over the clip, as points [beat, value, shape?].
    #[tool(annotations(title = "Get controllers", read_only_hint = true))]
    fn get_controllers(&self, Parameters(request): Parameters<GetControllersRequest>) -> CallToolResult {
        let session = self.session.lock();
        let project = session.project();
        let reply = (|| {
            let wanted = request.controller.as_deref().map(parse_controller).transpose()?;
            let clip = project.clip(ClipId(request.clip)).map_err(|error| explain(project, error))?;
            if clip.midi().is_none() {
                return Err(format!("{} is an audio clip and has no controllers", clip.id));
            }
            let keys = crate::instruments::TrackKeys::of_clip(&session, clip.id);
            let channel = keys.channel_asked(request.channel.as_ref())?;
            // A curve says its channel where the clip uses another than the first, or one was asked for.
            let channels = clip.uses_channels() || channel.is_some();
            let curves: Vec<Value> = clip
                .controllers()
                .iter()
                .filter(|curve| wanted.is_none_or(|kind| curve.controller == kind))
                .filter(|curve| channel.is_none_or(|channel| curve.channel == channel))
                .map(|curve| controller_json(curve, true, channels))
                .collect();
            let mut out = json!({ "clip_id": clip.id, "length": round(clip.length), "controllers": curves });
            if let (Some(kind), true) = (wanted, curves_is_empty(&out)) {
                let on = channel.filter(|_| channels).map(|channel| format!(" on channel {channel}"));
                out["note"] = json!(format!(
                    "the clip has no {kind} curve{}; set_controllers or shape_dynamics writes one",
                    on.unwrap_or_default()
                ));
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Write controller curves into a MIDI clip: expression, the mod wheel, the pitch wheel, the sustain pedal,
    /// breath, pressure or any MIDI controller, each as a few points with straight, held or bent segments
    /// between them. Each curve replaces the clip's curve for that controller; an empty list removes it. A
    /// curve says where its controller is at every moment of the clip: it stays at its last point's value,
    /// and before its first point at that point's value (a pedal or the pitch wheel is at rest until its first
    /// point). After the clip ends, pedals and the pitch wheel return to rest and other controllers stay where
    /// the clip left them. To give a melody its dynamics, shape_dynamics is quicker than points.
    #[tool(annotations(title = "Set controllers"))]
    fn set_controllers(&self, Parameters(request): Parameters<SetControllersRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = (|| {
            if request.controllers.is_empty() {
                return Err(
                    "`controllers` is empty: give at least one, such as {\"expression\": [[0, 90]]}".to_string()
                );
            }
            let curves = parse_curves(Some(&request.controllers))?;
            let keys = crate::instruments::TrackKeys::of_clip(&session, id);
            let channel = keys.channel(request.channel.as_ref())?;
            let written = edit(&mut session, |project| {
                let clip = project.clip_mut(id)?;
                for (kind, points) in &curves {
                    clip.set_controller_on(channel, *kind, points.clone())?;
                }
                let channels = clip.uses_channels();
                let listed = clip.controllers().iter().map(|curve| controller_json(curve, false, channels));
                Ok(listed.collect::<Vec<Value>>())
            })?;
            let removed: Vec<String> =
                curves.iter().filter(|(_, points)| points.is_empty()).map(|(kind, _)| kind.key()).collect();
            let mut out = json!({ "clip_id": id, "controllers": written });
            if !removed.is_empty() {
                out["removed"] = json!(removed);
            }
            let kept = curves.iter().filter(|(_, points)| !points.is_empty()).map(|(kind, _)| (channel, *kind));
            add_controller_warnings(&mut out, &session, id, kept);
            say_channels(&mut out, &session, id, &keys, [channel].into_iter());
            Ok(out)
        })();
        finish(reply)
    }

    /// Add notes to a MIDI clip, keeping the ones already there.
    #[tool(annotations(title = "Add notes"))]
    fn add_notes(&self, Parameters(request): Parameters<NotesRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = (|| {
            let keys = crate::instruments::TrackKeys::of_clip(&session, id);
            let channel = keys.channel(request.channel.as_ref())?;
            let (notes, how) = keys.played(&request.notes, channel)?;
            let played = {
                let project = session.project();
                let track = project.clip_track(id).map_err(|error| explain(project, error))?;
                let held = project.clip(id).ok().and_then(|clip| clip.midi()).map(|midi| midi.notes.clone());
                crate::instruments::played(&session, track, &held.unwrap_or_default(), &notes, &how)?
            };
            let total = edit(&mut session, |project| {
                let clip = project.clip_mut(id)?;
                match &played {
                    // The clip's keyswitches were written for all its notes.
                    Some(played) => clip.set_notes(played.all.clone())?,
                    None => clip.add_notes(&notes)?,
                };
                Ok(clip.notes().len())
            })?;
            let mut out = json!({ "added": notes.len(), "note_count": total });
            say_played(&mut out, played.as_ref());
            add_range_warning(&mut out, &session, id);
            say_channels(&mut out, &session, id, &keys, notes.iter().map(|note| note.channel));
            Ok(out)
        })();
        finish(reply)
    }

    /// Replace every note in a MIDI clip.
    #[tool(annotations(title = "Set notes", destructive_hint = true))]
    fn set_notes(&self, Parameters(request): Parameters<NotesRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = (|| {
            let keys = crate::instruments::TrackKeys::of_clip(&session, id);
            let channel = keys.channel(request.channel.as_ref())?;
            let (notes, how) = keys.played(&request.notes, channel)?;
            let played = {
                let project = session.project();
                let track = project.clip_track(id).map_err(|error| explain(project, error))?;
                crate::instruments::played(&session, track, &[], &notes, &how)?
            };
            let count = notes.len();
            let on: Vec<Channel> = notes.iter().map(|note| note.channel).collect();
            let written = played.as_ref().map_or(notes, |played| played.all.clone());
            edit(&mut session, |project| project.clip_mut(id)?.set_notes(written))?;
            let mut out = json!({ "note_count": count });
            say_played(&mut out, played.as_ref());
            add_range_warning(&mut out, &session, id);
            say_channels(&mut out, &session, id, &keys, on.into_iter());
            Ok(out)
        })();
        finish(reply)
    }

    /// Remove notes from a MIDI clip, filtered by time range and/or pitch. With no filter, removes all notes.
    #[tool(annotations(title = "Remove notes", destructive_hint = true))]
    fn remove_notes(&self, Parameters(request): Parameters<RemoveNotesRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let keys = crate::instruments::TrackKeys::of_clip(&session, ClipId(request.clip));
            let pitch = request.pitch.as_ref().map(|pitch| keys.pitch(&pitch.0)).transpose()?;
            let channel = keys.channel_asked(request.channel.as_ref())?;
            let (start, end) = (request.start.unwrap_or(f64::MIN), request.end.unwrap_or(f64::MAX));
            let (removed, remaining) = edit(&mut session, |project| {
                let clip = project.clip_mut(ClipId(request.clip))?;
                let removed = clip.remove_notes(|note| {
                    note.start >= start
                        && note.start < end
                        && pitch.is_none_or(|p| note.pitch == p)
                        && channel.is_none_or(|channel| note.channel == channel)
                })?;
                Ok((removed, clip.notes().len()))
            })?;
            Ok(json!({ "removed": removed, "note_count": remaining }))
        })();
        finish(reply)
    }

    /// Edit many notes at once: transpose, shift in time, quantize, change lengths or velocities, humanize.
    #[tool(annotations(title = "Transform notes"))]
    fn transform_notes(&self, Parameters(request): Parameters<TransformNotesRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let (start, end) = (request.start.unwrap_or(f64::MIN), request.end.unwrap_or(f64::MAX));
        // Which notes, by pitch and by channel, and the channel they are to be on.
        let keys = crate::instruments::TrackKeys::of_clip(&session, ClipId(request.clip));
        let chosen = (|| {
            let pitch = |input: &Option<PitchInput>| input.as_ref().map(|pitch| keys.pitch(&pitch.0)).transpose();
            let channel = keys.channel_asked(request.channel.as_ref())?;
            let to_channel = keys.channel_asked(request.to_channel.as_ref())?;
            Ok::<_, String>((pitch(&request.lowest)?, pitch(&request.highest)?, channel, to_channel))
        })();
        let (lowest, highest, channel, to_channel) = match chosen {
            Ok(chosen) => chosen,
            Err(error) => return finish(Err(error)),
        };
        let within = |note: &Note| {
            note.start >= start
                && note.start < end
                && lowest.is_none_or(|lowest| note.pitch >= lowest)
                && highest.is_none_or(|highest| note.pitch <= highest)
                && channel.is_none_or(|channel| note.channel == channel)
        };
        let reply = edit(&mut session, |project| {
            let clip = project.clip_mut(ClipId(request.clip))?;
            let mut notes = clip.notes().to_vec();
            let mut noise = Noise(request.seed.unwrap_or(1).max(1));
            let mut changed = 0;
            for note in notes.iter_mut().filter(|note| within(note)) {
                if let Some(channel) = to_channel {
                    note.channel = channel;
                }
                if let Some(semitones) = request.transpose {
                    let pitch = note.pitch as i32 + semitones;
                    if !(0..=127).contains(&pitch) {
                        return Err(EditError::Invalid(format!(
                            "transposing by {semitones} moves pitch {} out of range",
                            note.pitch
                        )));
                    }
                    note.pitch = pitch as u8;
                }
                if let Some(shift) = request.shift {
                    note.start += shift;
                }
                if let Some(grid) = request.quantize.filter(|grid| *grid > 0.0) {
                    note.start = (note.start / grid).round() * grid;
                }
                if let Some(amount) = request.humanize_timing {
                    note.start += noise.next() * amount;
                }
                note.start = note.start.max(0.0);
                if let Some(scale) = request.length_scale {
                    note.length *= scale;
                }
                let mut velocity = note.velocity as f64;
                if let Some(scale) = request.velocity_scale {
                    velocity *= scale;
                }
                if let Some(amount) = request.humanize_velocity {
                    velocity += noise.next() * amount as f64;
                }
                note.velocity = velocity.round().clamp(1.0, 127.0) as u8;
                changed += 1;
            }
            clip.set_notes(notes)?;
            let mut out = json!({ "changed": changed });
            if clip.uses_channels() {
                out["channels"] = json!(clip.channels());
            }
            Ok(out)
        })
        .map(|mut out| {
            if let Some(warning) = to_channel.and_then(|channel| keys.channel_warning(channel)) {
                out["warning"] = json!(warning);
            }
            out
        });
        finish(reply)
    }

    /// Automate a track's volume or pan, a send to a return track, the master volume, or any device parameter,
    /// with a breakpoint envelope over time. Use it for fades, filter sweeps, build-ups, drops and delay throws
    /// (a send opened for one note: target type "send", shut at -70 dB, open at the level wanted).
    ///
    /// A line between two points is straight along the control's travel, as if a hand moved the control at an
    /// even speed. It is not straight in the value's units:
    /// - a level that ends in silence travels as the same control does in Ableton Live, so a fade is heard all
    ///   the way down and a fade-in is not sudden. A track's or the master's volume, a send and an
    ///   instrument's volume move like a fader ("fader"): even in dB through the top 24 dB of the travel, and
    ///   below that ever faster, a stretch that would have been y dB taking y + y*y/8. From 0 dB to -60 dB a
    ///   volume is at -8 dB after a quarter of the time, -16 dB half way and -30 dB after three quarters; a
    ///   send, whose top is 0 dB, at -9, -19 and -30 dB. A Utility's `gain` moves like a gain ("gain"): even in
    ///   dB down to -17.5 dB, and below that the gain itself falls as the square of what is left of the
    ///   travel: -8 dB, -17 dB half way and -27 dB;
    /// - a frequency or a time travels by ratio ("log"): a sweep from 300 Hz to 9 kHz is at 1.6 kHz half way,
    ///   every octave taking the same time;
    /// - everything else evenly ("linear"): pan, mix and other amounts, an EQ's gain, a threshold, and every
    ///   parameter of a plugin (whose values are the positions of its own knobs).
    ///
    /// So two points are a fade or a sweep; there is no need to lay a line out in short pieces. `shape` on a
    /// point bends its line further, and `travel` "linear" asks for lines straight in the units. An envelope
    /// that was in a project file saved before lines followed their controls is "linear", and one of a level
    /// saved before levels travelled as Live's do is "square" (even in the square of the dB above silence:
    /// -20 dB half way from 0 to -60); each stays so until its points are written again or `travel` is set
    /// to "control". The reply says which travel the envelope has.
    #[tool(annotations(title = "Set automation"))]
    fn set_automation(&self, Parameters(request): Parameters<SetAutomationRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            if request.points.is_none() && request.enabled.is_none() && request.travel.is_none() {
                return Err("give `points` (the envelope), `enabled` (to switch it off or on), `travel` (how its \
                            lines go), or several of them"
                    .to_string());
            }
            let points = request.points.as_deref().map(parse_points).transpose()?;
            let (target, min, max, control) = resolve_target(&session, &request.target)?;
            if let Some(point) = points.iter().flatten().find(|point| point.value < min || point.value > max) {
                let lowest = if min == f64::MIN { "at most".to_string() } else { format!("{min} to") };
                return Err(format!(
                    "value {} at beat {} is outside the allowed range: {lowest} {max}",
                    point.value, point.beat
                ));
            }
            let travel = request.travel.map(|travel| match travel {
                TravelInput::Control => control,
                TravelInput::Linear => Travel::Linear,
            });
            edit(&mut session, |project| {
                let written = points.is_some();
                if let Some(points) = points {
                    // An envelope that is written is written as it is
                    // described: along its control, whatever the one
                    // before it did.
                    project.write_automation(target.clone(), points, travel.unwrap_or(control))?;
                }
                let lane = project.automation.iter().find(|lane| lane.target == target);
                let Some((count, was_on)) = lane.map(|lane| (lane.points.len(), lane.enabled)) else {
                    // No envelope: one was just cleared, or there never was one to switch.
                    return match written {
                        true => Ok(json!({ "point_count": 0, "cleared": true })),
                        false => Err(EditError::Invalid(
                            "there is no automation for that target to change; write `points` first".into(),
                        )),
                    };
                };
                if let (false, Some(travel)) = (written, travel) {
                    project.set_automation_travel(&target, travel)?;
                }
                // Writing an envelope means wanting to hear it.
                let enabled = request.enabled.unwrap_or(written || was_on);
                project.set_automation_enabled(&target, enabled)?;
                let travel = project.automation.iter().find(|lane| lane.target == target).map(|lane| lane.travel);
                Ok(json!({
                    "point_count": count,
                    "cleared": false,
                    "enabled": enabled,
                    "travel": travel.unwrap_or_default().name(),
                }))
            })
        })();
        finish(reply)
    }

    /// The automation envelopes of the project: what each moves, how many points it has, what its lines are
    /// straight along ("fader", "gain", "log", "linear" or, in an older song, "square": see set_automation),
    /// and its points as [beat, value, shape?].
    /// A song can hold tens of thousands of points, so one reply lists at most `limit` of them (1000 unless
    /// told otherwise): the envelopes that did not fit are listed without points, and one that fits in part
    /// says `next_offset`. Ask for one `target`, a stretch of beats (`start`, `end`), or the next part
    /// (`offset`) to read the rest.
    #[tool(annotations(title = "Get automation", read_only_hint = true))]
    fn get_automation(&self, Parameters(request): Parameters<GetAutomationRequest>) -> CallToolResult {
        let session = self.session.lock();
        let reply = (|| {
            let only = match &request.target {
                Some(target) => Some(resolve_target(&session, target)?.0),
                None => None,
            };
            if request.offset.is_some() && only.is_none() {
                return Err("`offset` reads on in one envelope: give its `target` too".to_string());
            }
            let (start, end) = (request.start.unwrap_or(f64::NEG_INFINITY), request.end.unwrap_or(f64::INFINITY));
            let mut room = request.limit.unwrap_or(DEFAULT_POINTS).clamp(1, MAX_POINTS);
            let (mut listed, mut total) = (0, 0);
            let mut lanes: Vec<Value> = Vec::new();
            for lane in
                session.project().automation.iter().filter(|lane| only.as_ref().is_none_or(|t| lane.target == *t))
            {
                let from = lane.points.partition_point(|point| point.beat < start);
                let to = lane.points.partition_point(|point| point.beat < end).max(from);
                let within = &lane.points[from..to];
                let skipped = request.offset.unwrap_or(0).min(within.len());
                let shown = &within[skipped..(skipped + room).min(within.len())];
                total += within.len() - skipped;
                listed += shown.len();
                room -= shown.len();
                let mut out = lane_json(lane, (!shown.is_empty() || within.is_empty()).then_some(shown));
                if skipped + shown.len() < within.len() {
                    out["next_offset"] = json!(skipped + shown.len());
                }
                lanes.push(out);
            }
            if let (Some(target), true) = (&only, lanes.is_empty()) {
                return Ok(json!({ "automation": [], "note": format!("nothing automates {}", target_json(target)) }));
            }
            let mut out = json!({ "automation": lanes });
            if listed < total {
                out["points_listed"] = json!(listed);
                out["points_in_all"] = json!(total);
                out["note"] = json!(
                    "Not every point fits in one reply. To read on, call get_automation with one envelope's \
                     `target` and `offset` set to its next_offset (0 for one listed without points), or with \
                     `start` and `end` for a stretch of beats."
                );
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Name the sections of the song (Intro, Verse, Drop...) with markers on the timeline. They are shown to
    /// the user and returned by get_project, which makes later edits easier to place.
    #[tool(annotations(title = "Set markers"))]
    fn set_markers(&self, Parameters(request): Parameters<SetMarkersRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = edit(&mut session, |project| {
            let previous = std::mem::take(&mut project.markers);
            for marker in &request.markers {
                if let Err(error) = project.add_marker(marker.beat, marker.name.clone()) {
                    project.markers = previous;
                    return Err(error);
                }
            }
            Ok(json!({ "marker_count": project.markers.len() }))
        });
        finish(reply)
    }

    /// Change the length of the whole song at one point: cut a stretch of time out, repeat it, or open up
    /// empty time. Every track moves together, with automation, markers and the loop, and clips lying across
    /// the edges are cut there. Use it to drop a section, double a chorus or make room for a bridge, instead of
    /// moving clips one by one.
    /// "copy" copies a stretch to another place in one step: every track or some, with the clips' notes and
    /// controller curves and the tracks' automation, either making room there or over what is there. Use it
    /// to bring a chorus back later in the song instead of duplicating its clips one by one.
    /// Where the tempo or the time signature changes inside the song, time that is deleted, duplicated or
    /// inserted takes its changes along (opened-up time has the tempo and the meter the song arrived with),
    /// and a copy is played at the tempo, and counted in the bars, of where it lands; the reply then gives
    /// the tempo map as it is now.
    #[tool(annotations(title = "Edit time"))]
    fn edit_time(&self, Parameters(request): Parameters<EditTimeRequest>) -> CallToolResult {
        enum Change {
            Delete(f64),
            Duplicate(f64),
            Insert(f64),
            Copy { end: f64, to: f64 },
        }
        let change = match (request.action.as_str(), request.end, request.length) {
            ("delete", Some(end), _) => Ok(Change::Delete(end)),
            ("duplicate", Some(end), _) => Ok(Change::Duplicate(end)),
            ("insert", _, Some(length)) => Ok(Change::Insert(length)),
            ("copy", Some(end), _) => match request.to {
                Some(to) => Ok(Change::Copy { end, to }),
                None => Err("action \"copy\" needs `to`, the beat the copy starts at".to_string()),
            },
            ("delete" | "duplicate" | "copy", None, _) => {
                Err(format!("action \"{}\" needs `end`, the beat where the stretch ends", request.action))
            }
            ("insert", _, None) => Err("action \"insert\" needs `length`, the number of beats to open up".to_string()),
            (other, ..) => {
                Err(format!("unknown action '{other}'; use \"delete\", \"duplicate\", \"insert\" or \"copy\""))
            }
        };
        let change = change.and_then(|change| match change {
            Change::Copy { .. } => Ok(change),
            _ if request.to.is_some() || request.tracks.is_some() || request.make_room.is_some() => Err(format!(
                "`to`, `tracks` and `make_room` go with action \"copy\"; \"{}\" changes every track",
                request.action
            )),
            _ => Ok(change),
        });
        let mut session = self.session.lock();
        let reply = change.and_then(|change| {
            edit(&mut session, |project| {
                let before = project.length_beats();
                let changing = !project.tempo_changes.is_empty() || !project.meter_changes.is_empty();
                let mut copied = None;
                match change {
                    Change::Delete(end) => project.delete_time(request.start, end)?,
                    Change::Duplicate(end) => project.duplicate_time(request.start, end)?,
                    Change::Insert(length) => project.insert_time(request.start, length)?,
                    Change::Copy { end, to } => {
                        let tracks: Option<Vec<TrackId>> =
                            request.tracks.as_ref().map(|tracks| tracks.iter().copied().map(TrackId).collect());
                        let mode = match request.make_room {
                            Some(false) => trill_core::PasteMode::Replace,
                            _ => trill_core::PasteMode::Insert,
                        };
                        let clips = project.copy_time(request.start, end, to, tracks.as_deref(), mode)?;
                        // By track, so that what arrived where can be read.
                        let mut by_track: BTreeMap<String, Vec<u64>> = BTreeMap::new();
                        for id in &clips {
                            let track = project.clip_track(*id)?;
                            by_track.entry(project.track(track)?.name.clone()).or_default().push(id.0);
                        }
                        copied = Some(json!({
                            "from": [round(request.start), round(end)],
                            "to": [round(to), round(to + end - request.start)],
                            "made_room": mode == trill_core::PasteMode::Insert,
                            "clips": by_track,
                        }));
                    }
                }
                let markers: Vec<Value> =
                    project.markers.iter().map(|m| json!({ "beat": round(m.beat), "name": m.name })).collect();
                let mut out = json!({
                    "length_beats": round(project.length_beats()),
                    "was_beats": round(before),
                    "markers": markers,
                });
                if let Some(copied) = copied {
                    out["copied"] = copied;
                }
                // Time that is cut out, repeated or opened up takes its changes of tempo and of time
                // signature along; a copy leaves those of where it lands. Either way, where they are now.
                if changing || !project.tempo_changes.is_empty() || !project.meter_changes.is_empty() {
                    out["tempo_map"] = tempo_map_json(project);
                }
                Ok(out)
            })
        });
        finish(reply)
    }

    /// Open a plugin's own window on the user's screen, bring it to the front, or close it. Only plugins have
    /// one (get_device shows "editor" for those); built-in devices are edited through their parameters. What
    /// the user changes in the window shows up in get_device.
    #[tool(annotations(title = "Show device editor"))]
    fn show_device_editor(&self, Parameters(request): Parameters<DeviceEditorRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = DeviceId(request.device);
        let reply = (|| {
            let name = match session.project().device(id) {
                Ok(device) => device.name.clone(),
                Err(error) => return Err(explain(session.project(), error)),
            };
            if !session.device_has_editor(id) {
                return Err(format!(
                    "{name} has no window of its own. Read and set its parameters with get_device and update_device."
                ));
            }
            if request.open.unwrap_or(true) {
                let options = EditorOptions { title: name.clone(), ..EditorOptions::default() };
                session.open_device_editor(id, &options).map_err(|error| error.to_string())?;
            } else {
                session.close_device_editor(id);
            }
            Ok(json!({ "device": id, "editor": if session.is_device_editor_open(id) { "open" } else { "closed" } }))
        })();
        finish(reply)
    }

    /// Projects that were not saved before the program last stopped (a crash, a power cut): list them, bring
    /// one back, or delete one. A project that is brought back replaces the open one and has unsaved changes;
    /// save_project writes it. Ask the user before discarding anything.
    #[tool(annotations(title = "Recover project"))]
    fn recover_project(&self, Parameters(request): Parameters<RecoverProjectRequest>) -> CallToolResult {
        let lost = self.runtime.recoverable();
        let listed = |lost: &[trill_runtime::SafetyCopy]| -> Vec<Value> {
            lost.iter()
                .enumerate()
                .map(|(index, copy)| {
                    let minutes = copy.made.elapsed().map_or(0, |age| age.as_secs() / 60);
                    json!({
                        "index": index,
                        "name": copy.name,
                        "file": copy.original.as_ref().map(|path| path.display().to_string()),
                        "minutes_ago": minutes,
                    })
                })
                .collect()
        };
        let chosen = || {
            let index = request.index.ok_or("give the `index` of the project, from action \"list\"")?;
            lost.get(index).ok_or_else(|| match lost.len() {
                0 => "there is nothing to recover".to_string(),
                count => format!("there is no project {index}; the list has {count} (index 0 to {})", count - 1),
            })
        };
        let reply = match request.action.as_deref().unwrap_or("list") {
            "list" => Ok(json!({ "unsaved": listed(&lost) })),
            "open" => chosen().and_then(|copy| {
                self.may_replace_project(&self.session.lock())?;
                self.runtime.recover(copy).map_err(|error| error.to_string())?;
                let session = self.session.lock();
                let mut out = project_json(&session, false);
                out["recovered"] = json!(copy.name);
                Ok(out)
            }),
            "discard" if self.guard.is_some() => Err("Deleting a lost project cannot be undone, so that is the \
                user's to do, from the window. You can bring it back with action \"open\" instead."
                .to_string()),
            "discard" => chosen().map(|copy| {
                self.runtime.discard(copy);
                json!({ "discarded": copy.name, "unsaved": listed(&self.runtime.recoverable()) })
            }),
            other => Err(format!("unknown action '{other}'; use \"list\", \"open\" or \"discard\"")),
        };
        finish(reply)
    }

    /// Search the user's sample library: drum hits, loops, one-shots, vocals. Returns file paths with each
    /// sound's length in seconds, tempo, key and tags (read from file and folder names, so not every file has
    /// them), best match first. Feed the paths to preview_sound, set_sample or create_audio_clip.
    #[tool(annotations(title = "Search sounds", read_only_hint = true))]
    fn search_sounds(&self, Parameters(request): Parameters<SearchSoundsRequest>) -> CallToolResult {
        let library = &self.runtime.library;
        let reply = (|| {
            let folder = match request.folder.as_deref() {
                None => None,
                Some(text) => {
                    let folder = folder_argument(Some(text), "the folder filter")?;
                    if library.index().find_folder(&folder).is_none() {
                        return Err(format!(
                            "'{}' is not a folder of the library. {}",
                            folder.display(),
                            list_folders(&library.roots())
                        ));
                    }
                    Some(folder)
                }
            };
            let query = sound_query(&request, folder)?;
            let results = library.search(&query);
            let sounds: Vec<Value> =
                results.entries().map(|entry| sound_json(entry, library.is_favorite(&entry.path))).collect();
            let mut out = json!({ "total": results.total(), "sounds": sounds });
            if results.total() > results.len() {
                out["truncated"] = json!(format!(
                    "showing the best {} of {}; raise `limit` (up to {MAX_SOUNDS}) or narrow the search",
                    results.len(),
                    results.total()
                ));
            }
            if library.roots().is_empty() {
                out["hint"] = json!(
                    "The library has no folders, so there is nothing to find. Use the library tool: `suggestions` \
                     lists where samples are on this machine and `add_folder` adds one."
                );
            } else if library.scan_status().is_scanning() {
                out["hint"] =
                    json!("The library is still being scanned; more may turn up. `library` status shows progress.");
            } else if results.is_empty() {
                out["hint"] = json!("Nothing matched. Try fewer words or filters; names are matched literally.");
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Manage the folders the sample library covers: see its status, add or remove a folder, rescan, or list
    /// folders on this machine that probably hold samples. Folders are remembered between runs. Adding a folder
    /// only reads it; nothing on disk is ever changed.
    #[tool(annotations(title = "Sound library"))]
    async fn library(&self, Parameters(request): Parameters<LibraryRequest>) -> CallToolResult {
        let library = &self.runtime.library;
        let reply = match request.action {
            LibraryAction::Status => Ok(self.library_status()),
            LibraryAction::Suggestions => {
                let roots = library.roots();
                let found: Vec<Value> = suggested_locations()
                    .into_iter()
                    .map(|place| {
                        let mut out = json!({ "label": place.label, "path": place.path.display().to_string() });
                        if roots.iter().any(|root| place.path.starts_with(root)) {
                            out["in_library"] = json!(true);
                        }
                        out
                    })
                    .collect();
                let mut out = json!({ "suggestions": found });
                if found.is_empty() {
                    out["hint"] =
                        json!("No well-known sample folders were found. Ask the user where their samples are.");
                }
                Ok(out)
            }
            LibraryAction::AddFolder => match folder_argument(request.path.as_deref(), "add_folder") {
                Ok(folder) if !folder.is_dir() => Err(format!(
                    "'{}' is not a folder on this machine; check the path (`suggestions` lists likely ones)",
                    folder.display()
                )),
                Ok(folder) => {
                    let added = library.add_root(&folder);
                    if added {
                        self.wait_for_scan(request.wait_seconds).await;
                    }
                    let mut out = self.library_status();
                    out["added"] = json!(added);
                    if !added {
                        out["note"] = json!("That folder was already covered by the library.");
                    }
                    Ok(out)
                }
                Err(error) => Err(error),
            },
            LibraryAction::RemoveFolder => match folder_argument(request.path.as_deref(), "remove_folder") {
                Ok(folder) if library.remove_root(&folder) => {
                    let mut out = self.library_status();
                    out["removed"] = json!(folder.display().to_string());
                    Ok(out)
                }
                Ok(folder) => Err(format!(
                    "'{}' is not one of the library's folders. {}",
                    folder.display(),
                    list_folders(&library.roots())
                )),
                Err(error) => Err(error),
            },
            LibraryAction::MoveFolder => {
                let from = folder_argument(request.path.as_deref(), "move_folder");
                let to = match request.to.as_deref().map(str::trim).filter(|to| !to.is_empty()) {
                    Some(to) => Ok(std::path::absolute(to).unwrap_or_else(|_| PathBuf::from(to))),
                    None => Err("move_folder needs `to`: where the folder is now".to_owned()),
                };
                match (from, to) {
                    (Ok(from), Ok(to)) => match library.move_root(&from, &to) {
                        Ok(moved) => {
                            self.wait_for_scan(request.wait_seconds).await;
                            let mut out = self.library_status();
                            out["moved"] = json!({
                                "from": from.display().to_string(),
                                "to": to.display().to_string(),
                                "files_known": moved.files,
                                "favorites": moved.favorites,
                                "recent": moved.recent,
                            });
                            Ok(out)
                        }
                        Err(error) => Err(error),
                    },
                    (Err(error), _) | (_, Err(error)) => Err(error),
                }
            }
            LibraryAction::Rescan => {
                library.rescan();
                self.wait_for_scan(request.wait_seconds).await;
                Ok(self.library_status())
            }
        };
        // The folders are settings. Saving a change now, rather than at the
        // next look of the housekeeping thread, means "added" is true on
        // disk by the time the caller reads it. Looking saves nothing.
        let reply = reply.map(|mut out| {
            if let Err(error) = self.runtime.save_library_changes() {
                out["warning"] = json!(format!("{error}; the change will be lost when the program closes"));
            }
            out
        });
        finish(reply)
    }

    /// Play an audio file to the user through the speakers, over the song, without adding it to the project;
    /// call it without `path` to stop. The reply describes the sound (length, tempo, key, tags) since you
    /// cannot hear it.
    #[tool(annotations(title = "Preview sound"))]
    async fn preview_sound(&self, Parameters(request): Parameters<PreviewSoundRequest>) -> CallToolResult {
        let Some(path) = request.path.filter(|path| !path.trim().is_empty()) else {
            self.runtime.stop_preview();
            return finish(Ok(json!({ "playing": false })));
        };
        let gain_db = request.gain_db.unwrap_or(0.0);
        if !gain_db.is_finite() || gain_db > MAX_VOLUME_DB {
            return finish(Err(format!("gain_db {gain_db} is above the {MAX_VOLUME_DB} dB maximum")));
        }
        // Decoding a long file takes a moment; it must not hold up the
        // other tools.
        let runtime = self.runtime.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let path = Path::new(&path);
            let facts = runtime.sound_facts(path)?;
            runtime.preview(path, gain_db).map(|_| facts)
        })
        .await;
        finish(match outcome {
            Ok(Ok(facts)) => {
                let mut out = json!({
                    "playing": true,
                    "seconds": round(facts.seconds),
                    "sample_rate": facts.sample_rate,
                    "channels": facts.channels,
                });
                if let Some(tempo) = facts.tempo {
                    out["tempo"] = json!(round(tempo));
                    out["beats"] = json!(round(facts.seconds * tempo / 60.0));
                }
                if let Some(key) = facts.key {
                    out["key"] = json!(key.name());
                }
                if !facts.tags.is_empty() {
                    out["tags"] = facts.tags.iter().map(|tag| tag.label()).collect();
                }
                Ok(out)
            }
            Ok(Err(error)) => Err(format!("{error}. Nothing is playing; search_sounds lists files that exist.")),
            Err(error) => Err(format!("the preview did not start: {error}")),
        })
    }

    /// Give a sample player its audio file: a `builtin:sampler` its one sound, or a pad of a `builtin:drumrack`
    /// (say which with `pad`). `path: null` empties the sampler or the pad. The device keeps its parameter
    /// values, and the change is one undo step.
    #[tool(annotations(title = "Set sample"))]
    fn set_sample(&self, Parameters(request): Parameters<SetSampleRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let id = DeviceId(request.device);
            let Some(path) = &request.path else {
                return Err("set_sample needs `path`: the audio file to load, or null to empty the slot".to_string());
            };
            let path = path.as_deref();
            let project = session.project();
            let model = project.device(id).map_err(|error| explain(project, error))?;
            let (uri, name) = (model.uri.clone(), model.name.clone());
            let changed = match (uri.as_str(), &request.pad) {
                (DRUM_RACK_URI, Some(pad)) => {
                    let note = parse_pitch_value(&pad.0)?;
                    session.set_pad_file(id, note, path)
                }
                (DRUM_RACK_URI, None) => {
                    return Err(format!(
                        "{id} is a drum rack; say which pad with `pad`: a note 36-51 or a drum name such as \"kick\""
                    ));
                }
                (SAMPLER_URI, Some(_)) => {
                    return Err(format!(
                        "{id} is a sampler and has one sample; leave out `pad` (pads are for drum racks)"
                    ));
                }
                (SAMPLER_URI, None) => session.set_sampler_file(id, path),
                _ => {
                    return Err(format!(
                        "{id} ({name}) plays no samples. set_sample works on {SAMPLER_URI} and {DRUM_RACK_URI} devices; \
                         add one with add_device or create_track."
                    ));
                }
            };
            changed.map_err(|error| explain(session.project(), error))?;
            let files = session.sample_files(id).map_err(|error| explain(session.project(), error))?;
            Ok(json!({ "device_id": id, "name": name, "samples": samples_json(&files) }))
        })();
        finish(reply)
    }

    /// Control live playback through the user's speakers: play, stop, seek, read the playhead, or record what
    /// is played live on the armed tracks.
    #[tool(annotations(title = "Transport"))]
    fn transport(&self, Parameters(request): Parameters<TransportRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let position = request.position;
            if position.is_some_and(|beat| !beat.is_finite() || beat < 0.0) {
                return Err("position must be a beat >= 0".to_string());
            }
            if let Some(bars) = request.count_in_bars {
                if bars > trill_engine::MAX_COUNT_IN_BARS {
                    return Err(format!("count_in_bars is at most {}", trill_engine::MAX_COUNT_IN_BARS));
                }
                session.set_count_in(bars);
            }
            if let Some(on) = request.metronome {
                session.set_metronome(on);
            }
            // The audio thread applies commands at its next callback, so the
            // reply states what was asked for rather than a stale reading.
            let current = session.transport();
            let commanded = |playing: bool, beat: f64| json!({ "playing": playing, "position_beats": round(beat) });
            Ok(match request.action {
                TransportAction::Play => {
                    if let Some(beat) = position {
                        session.seek(beat);
                    }
                    session.play();
                    let mut out = commanded(true, position.unwrap_or(current.position_beats));
                    if let Some(note) = crate::view::loading_note(&session) {
                        out["loading"] = json!(note);
                    }
                    if let Some(note) = crate::playing::falling_short_note(&session) {
                        out["falling_short"] = json!(note);
                    }
                    out
                }
                TransportAction::Stop => {
                    let recording = session.is_recording();
                    session.stop();
                    let mut out = commanded(false, current.position_beats);
                    if recording {
                        // The take is in the project by now.
                        out["take"] = take_json(session.last_take());
                        if session.last_take().is_empty() {
                            out["note"] = json!("Nothing was played during the recording, so no clip was made.");
                        }
                    }
                    out
                }
                TransportAction::Seek => {
                    let beat = position.ok_or("seek needs `position`")?;
                    session.seek(beat);
                    commanded(current.playing, beat)
                }
                TransportAction::Panic => {
                    session.panic();
                    commanded(false, current.position_beats)
                }
                TransportAction::Record => {
                    if let Some(beat) = position {
                        session.seek(beat);
                    }
                    session.record().map_err(|error| {
                        format!("{error}: update_track with `armed: true` arms one (get_project lists the tracks).")
                    })?;
                    let from = position.unwrap_or(current.position_beats);
                    let count_in = if current.playing { 0 } else { session.count_in() };
                    let mut out = commanded(true, from);
                    out["recording"] = json!({
                        "state": if count_in > 0 { "count_in" } else { "recording" },
                        "tracks": session.armed(),
                        "count_in_bars": count_in,
                    });
                    out
                }
                TransportAction::Status => transport_json(&session),
            })
        })();
        finish(reply)
    }

    /// The audio device: what is playing the sound, at what sample rate, buffer size and latency, and the
    /// means to change them while the program runs (the project and the playhead are kept; a new sample rate
    /// stops playback). What cannot be had is replaced by the nearest thing, and `warning` then says why.
    #[tool(annotations(title = "Audio device"))]
    async fn audio(&self, Parameters(request): Parameters<AudioRequest>) -> CallToolResult {
        let runtime = self.runtime.clone();
        // Listing devices and opening one both wait on the system.
        let outcome = tokio::task::spawn_blocking(move || -> Reply {
            match request.action {
                AudioAction::Status => Ok(audio_json(&runtime.audio_status())),
                AudioAction::Devices => {
                    let devices = runtime.audio_devices();
                    let mut out = audio_json(&runtime.audio_status());
                    out["outputs"] = devices.outputs.iter().map(audio_device_json).collect();
                    out["inputs"] = devices.inputs.iter().map(audio_device_json).collect();
                    if devices.outputs.is_empty() {
                        out["note"] = json!("No audio output device was found; Trill runs without sound.");
                    }
                    Ok(out)
                }
                AudioAction::Set => {
                    let mut config = runtime.audio_status().config;
                    if let Some(output) = &request.output {
                        let named = output.trim();
                        let follow_system = named.is_empty() || named.eq_ignore_ascii_case("default");
                        config.output = (!follow_system).then(|| named.to_string());
                    }
                    if let Some(rate) = request.sample_rate {
                        config.sample_rate = (rate > 0).then_some(rate);
                    }
                    if let Some(frames) = request.buffer_size {
                        config.buffer_size = (frames > 0).then_some(frames);
                    }
                    if let Some(exclusive) = request.exclusive {
                        config.exclusive = exclusive;
                    }
                    let (status, saved) = runtime.set_audio(config);
                    let mut out = audio_json(&status);
                    if let Err(error) = saved {
                        out["not_saved"] = json!(format!("{error}. The change holds for this run only."));
                    }
                    if status.info.warning.is_some() {
                        out["hint"] = json!("`audio` with action \"devices\" lists what can be chosen.");
                    }
                    Ok(out)
                }
            }
        })
        .await;
        finish(outcome.unwrap_or_else(|error| Err(format!("the audio device did not answer: {error}"))))
    }

    /// The MIDI inputs (keyboards, pads, controllers) and which are listened to. Every input is on unless
    /// switched off, one plugged in later included. What they send plays the armed tracks.
    #[tool(annotations(title = "MIDI inputs"))]
    async fn midi_inputs(&self, Parameters(request): Parameters<MidiInputsRequest>) -> CallToolResult {
        let runtime = self.runtime.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Reply {
            // As things are now, not as they were a second ago.
            let looked = runtime.rescan_midi_inputs();
            let mut note = None;
            if let MidiInputsAction::Enable | MidiInputsAction::Disable = request.action {
                let enable = matches!(request.action, MidiInputsAction::Enable);
                let name = request.name.as_deref().map(str::trim).filter(|name| !name.is_empty());
                let name = name.ok_or("`enable` and `disable` need `name`: the input, as `list` gives it")?;
                let known: Vec<String> = runtime.midi_inputs().into_iter().map(|input| input.name).collect();
                if !known.iter().any(|input| input == name) {
                    return Err(if known.is_empty() {
                        format!("There is no MIDI input called '{name}': none is plugged in.")
                    } else {
                        format!("There is no MIDI input called '{name}'. Inputs: {}", known.join(", "))
                    });
                }
                if let Err(error) = runtime.set_midi_input_enabled(name, enable) {
                    note = Some(format!("{error}. The change holds for this run only."));
                }
                runtime.rescan_midi_inputs();
            }
            let inputs = runtime.midi_inputs();
            let playing: Vec<u64> = runtime.session.lock().live_tracks().iter().map(|track| track.0).collect();
            let mut out = json!({
                "inputs": inputs.iter().map(midi_input_json).collect::<Vec<_>>(),
                "plays_tracks": playing,
            });
            if inputs.is_empty() {
                out["note"] = json!("No MIDI input is plugged in (and none is opened when Trill runs without audio).");
            } else if playing.is_empty() {
                out["note"] = json!("No track is armed, so nothing a keyboard plays is heard: update_track `armed`.");
            }
            if !looked {
                out["stale"] = json!("The MIDI ports did not answer in time; this is how they were before.");
            }
            if let Some(note) = note {
                out["not_saved"] = json!(note);
            }
            Ok(out)
        })
        .await;
        finish(outcome.unwrap_or_else(|error| Err(format!("the MIDI inputs could not be read: {error}"))))
    }

    /// Play notes live through a track's instrument, as a keyboard would: they are heard now, in time at the
    /// project tempo, and recorded if a recording is running. Returns when the last note has ended. To write
    /// a part, use create_clip instead.
    #[tool(annotations(title = "Play notes live"))]
    async fn play_notes(&self, Parameters(request): Parameters<PlayNotesRequest>) -> CallToolResult {
        // What to play and when, worked out before anything sounds.
        let plan = (|| {
            let notes = parse_notes(&request.notes)?;
            let mut moves = Vec::new();
            for (name, value) in request.controllers.iter().flatten() {
                let kind = parse_controller(name)?;
                let (low, high) = kind.range();
                let position = value / controller_scale(kind);
                if !position.is_finite() || position < low || position > high {
                    let scale = controller_scale(kind);
                    return Err(format!("{kind} value {value} is outside {}..={}", low * scale, high * scale));
                }
                moves.push((kind, position));
            }
            if notes.is_empty() {
                return Err("`notes` is empty: give at least one note".to_string());
            }
            if notes.len() > MAX_LIVE_NOTES {
                return Err(format!("play_notes plays at most {MAX_LIVE_NOTES} notes at a time"));
            }
            let session = self.session.lock();
            let track = request.track.map(TrackId);
            let tracks = match track {
                Some(track) => {
                    session.project().track(track).map_err(|error| explain(session.project(), error))?;
                    vec![track]
                }
                None => session.live_tracks(),
            };
            if tracks.is_empty() {
                return Err("No track is armed, so the notes would not be heard. Give `track`, or arm one with \
                            update_track `armed`."
                    .to_string());
            }
            // Played now, at the tempo the song has where it stands.
            let here = session.transport().position_beats;
            let seconds_per_beat = 60.0 / session.project().tempo_map().tempo_at(here);
            let mut events: Vec<(f64, bool, u8, u8)> = Vec::with_capacity(notes.len() * 2);
            for note in &notes {
                events.push((note.start * seconds_per_beat, true, note.pitch, note.velocity));
                events.push((note.end() * seconds_per_beat, false, note.pitch, note.velocity));
            }
            // Stable: a note that ends where another begins ends first.
            events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let seconds = events.last().map_or(0.0, |event| event.0);
            if seconds > MAX_LIVE_SECONDS {
                return Err(format!(
                    "the notes take {seconds:.0} s to play; play_notes plays for at most {MAX_LIVE_SECONDS} s"
                ));
            }
            Ok((track, tracks, events, seconds, moves))
        })();
        let (track, tracks, events, seconds, moves) = match plan {
            Ok(plan) => plan,
            Err(message) => return finish(Err(message)),
        };

        // The controllers first: a note sounds at the level it finds.
        {
            let mut session = self.session.lock();
            for (kind, position) in &moves {
                match track {
                    Some(track) => session.send_controller(track, *kind, *position),
                    None => session.play_live(kind.event(*position as f32)),
                }
            }
        }
        let started = tokio::time::Instant::now();
        for (at, on, pitch, velocity) in events {
            tokio::time::sleep_until(started + Duration::from_secs_f64(at)).await;
            let mut session = self.session.lock();
            match (track, on) {
                (Some(track), true) => session.note_on(track, pitch, velocity),
                (Some(track), false) => session.note_off(track, pitch),
                (None, true) => {
                    session.play_live(EventKind::NoteOn { pitch, velocity: f32::from(velocity) / 127.0 });
                }
                (None, false) => session.play_live(EventKind::NoteOff { pitch }),
            }
        }
        let session = self.session.lock();
        let mut out = json!({
            "played": request.notes.len(),
            "seconds": round(seconds),
            "tracks": tracks.iter().map(|track| track.0).collect::<Vec<_>>(),
        });
        if session.is_recording() {
            out["recording"] = json!("The notes are being recorded; transport `stop` ends the take.");
        }
        finish(Ok(out))
    }

    /// Bring a MIDI file into the song: each part becomes a new track with an instrument and one clip, with
    /// its notes and its controllers (expression, the wheels, the pedals) as curves.
    #[tool(annotations(title = "Import MIDI file"))]
    fn import_midi(&self, Parameters(request): Parameters<ImportMidiRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let bytes = std::fs::read(&request.path).map_err(|error| format!("{}: {error}", request.path))?;
            let file = midi_file::parse(&bytes).map_err(|error| format!("{}: {error}", request.path))?;
            if file.tracks.iter().all(|track| track.notes.is_empty()) {
                return Err(format!("{} contains no notes", request.path));
            }
            let use_tempo = request.use_tempo.unwrap_or(session.project().tracks.is_empty());
            let melodic = request.instrument.as_deref().unwrap_or("builtin:polysynth");
            if !session.factory().descriptors().iter().any(|descriptor| descriptor.uri == melodic) {
                return Err(format!("unknown device '{melodic}'; list_devices shows what is available"));
            }

            let parts = import_parts(&mut session, &file, request.at.unwrap_or(0.0), use_tempo, melodic)?;
            let mut out = json!({ "tracks": parts, "file_tempo": file.tempo, "tempo": session.project().tempo });
            let changes = file.tempo_changes.len() + file.meter_changes.len();
            if changes > 0 {
                out["file_changes"] = json!(format!(
                    "the file changes its tempo {} time(s) and its time signature {} time(s); {}",
                    file.tempo_changes.len(),
                    file.meter_changes.len(),
                    if use_tempo {
                        "the song has them now (get_tempo_map)"
                    } else {
                        "the song kept its own (`use_tempo`: true takes the file's)"
                    }
                ));
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// Write the song's notes and controller curves to a Standard MIDI File, one track per track, exactly as
    /// they sound (loops repeated, trims applied).
    #[tool(annotations(title = "Export MIDI file"))]
    fn export_midi(&self, Parameters(request): Parameters<ExportMidiRequest>) -> CallToolResult {
        let session = self.session.lock();
        let reply = (|| {
            let file = session.project().export_midi();
            if file.tracks.is_empty() {
                return Err("the project has no MIDI notes to export".to_string());
            }
            let path = self
                .write_path(&session, Some(&request.path), "a MIDI file", &["mid", "midi"])?
                .unwrap_or_else(|| PathBuf::from(&request.path));
            if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&path, midi_file::write(&file)).map_err(|error| format!("{}: {error}", path.display()))?;
            let notes: usize = file.tracks.iter().map(|track| track.notes.len()).sum();
            let curves: usize = file.tracks.iter().map(|track| track.controllers.len()).sum();
            Ok(json!({
                "path": path.display().to_string(),
                "tracks": file.tracks.len(),
                "notes": notes,
                "controller_curves": curves,
            }))
        })();
        finish(reply)
    }

    /// Step backwards or forwards through the edit history. Every change made through these tools is undoable.
    #[tool(annotations(title = "Undo / redo"))]
    fn history(&self, Parameters(request): Parameters<HistoryRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let wanted = request.steps.unwrap_or(1).clamp(1, 1000);
        let mut done = 0;
        for _ in 0..wanted {
            let moved = match request.action {
                HistoryAction::Undo => session.undo(),
                HistoryAction::Redo => session.redo(),
            };
            if !moved {
                break;
            }
            done += 1;
        }
        finish(Ok(json!({ "steps_applied": done, "can_undo": session.can_undo(), "can_redo": session.can_redo() })))
    }

    /// Save the project to a file.
    #[tool(annotations(title = "Save project"))]
    fn save_project(&self, Parameters(request): Parameters<SaveProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let requested = self.write_path(&session, request.path.as_deref(), "a project", &["trill"])?;
            let path = match (requested, session.path()) {
                (Some(path), _) => path,
                (None, Some(path)) => path.to_path_buf(),
                (None, None) => return Err("this project has never been saved; give a `path`".to_string()),
            };
            session.save(&path).map_err(|error| error.to_string())?;
            self.runtime.song_is_at(&path);
            Ok(json!({ "saved": path.display().to_string() }))
        })();
        finish(reply)
    }

    /// Open a project file, replacing the current project. Unsaved changes are lost. The reply is the whole song at
    /// once: its tracks, clips and mixer. Its plugins are made after that, one at a time, which takes a second or
    /// more for each (ten, for a sampled instrument nothing has read lately): until all are there the reply and
    /// get_project have a `loading` note that says how far they are and which are not made yet, and such a device is
    /// marked `not_made_yet`. Read and edit the song meanwhile; a tool that needs a device that is not there waits
    /// for it by itself.
    #[tool(annotations(title = "Load project", destructive_hint = true))]
    fn load_project(&self, Parameters(request): Parameters<LoadProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = self.may_replace_project(&session).and_then(|()| {
            session.load(Path::new(&request.path)).map_err(|e| e.to_string())?;
            self.runtime.song_is_at(Path::new(&request.path));
            Ok(project_json(&session, false))
        });
        finish(reply)
    }

    /// Start a project with no tracks, replacing the current one. Unsaved changes are lost. Its master chain is not
    /// empty: it starts with a limiter that keeps the output from clipping, listed in the reply under `master`.
    /// Leave it for a song you mix here; delete it first when you bring a master chain of your own.
    #[tool(annotations(title = "New project", destructive_hint = true))]
    fn new_project(&self, Parameters(request): Parameters<NewProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        if let Err(refusal) = self.may_replace_project(&session) {
            return finish(Err(refusal));
        }
        session.new_project();
        let reply = match request.name {
            Some(name) => edit(&mut session, |project| {
                project.name = name;
                Ok(())
            }),
            None => Ok(()),
        };
        finish(reply.map(|()| project_json(&session, false)))
    }
}

/// Resolves parameter keys and values against a device's parameter list.
/// Whether `uri` names a plugin rather than a built-in device.
/// The curves a tool was given, by controller, as a clip keeps them.
fn parse_curves(
    curves: Option<&BTreeMap<String, Vec<ControllerPointInput>>>,
) -> Result<Vec<(ControllerKind, Vec<AutomationPoint>)>, String> {
    let mut parsed: Vec<(ControllerKind, Vec<AutomationPoint>)> = Vec::new();
    for (name, points) in curves.into_iter().flatten() {
        let kind = parse_controller(name)?;
        if parsed.iter().any(|(known, _)| *known == kind) {
            return Err(format!("{kind} is given twice (as '{name}' and under another name)"));
        }
        let points = parse_controller_points(kind, points).map_err(|error| format!("{name}: {error}"))?;
        parsed.push((kind, points));
    }
    // In the controllers' own order, whatever order they were named in.
    parsed.sort_by_key(|(kind, _)| *kind);
    Ok(parsed)
}

fn curves_is_empty(out: &Value) -> bool {
    out["controllers"].as_array().is_none_or(Vec::is_empty)
}

/// Says what cannot be heard from the reply alone about curves just
/// written to `clip`: an instrument that is known not to respond to one of
/// them, and points that lie past what the clip plays.
pub(crate) fn add_controller_warnings(
    out: &mut Value,
    session: &Session,
    clip: ClipId,
    kinds: impl Iterator<Item = (Channel, ControllerKind)>,
) {
    let project = session.project();
    let (Ok(found), Ok(track)) = (project.clip(clip), project.clip_track(clip)) else { return };
    let support = session.controller_support(track);
    let mut warnings: Vec<String> = Vec::new();
    for (channel, kind) in kinds {
        if support.responds_to(kind) == Some(false) {
            let known = match &support {
                trill_core::device::ControllerSupport::Listed(kinds) if !kinds.is_empty() => {
                    format!(" It responds to: {}.", kinds.iter().map(|kind| kind.key()).collect::<Vec<_>>().join(", "))
                }
                _ => String::new(),
            };
            warnings.push(format!(
                "The instrument on this track does not respond to {kind}, so this curve will not be heard.{known}"
            ));
        }
        let playable =
            found.midi().and_then(|midi| midi.loop_length).map_or(found.length, |length| length.min(found.length));
        let offset = found.midi().map_or(0.0, |midi| midi.start_offset);
        let late = found
            .controller_on(channel, kind)
            .is_some_and(|curve| curve.points.iter().any(|point| point.beat > offset + playable + 1e-6));
        if late {
            warnings.push(format!(
                "Some points of the {kind} curve lie after beat {} of the clip, past what it plays; lengthen the clip \
                 with update_clip or move them earlier.",
                round(offset + playable)
            ));
        }
    }
    if support == trill_core::device::ControllerSupport::Unknown && session.instrument(track).is_none() {
        warnings.push(format!("Track {} has no instrument, so nothing responds to these controllers yet.", track.0));
    }
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
}

pub(crate) fn is_plugin(uri: &str) -> bool {
    split_uri(uri).is_some_and(|(scheme, _)| scheme != BUILTIN_SCHEME)
}

/// Says where to look when what went wrong is that a plugin has no such
/// preset, or more than one: its presets are too many to name in an error.
fn preset_hint(error: String, uri: &str) -> String {
    let about_presets = error.contains("has no preset '") || error.contains(" presets named '");
    if about_presets && is_plugin(uri) && !error.contains("available: ") {
        format!("{error}. describe_device with preset_query searches the presets of '{uri}'")
    } else {
        error
    }
}

/// The parameters of a `get_device` reply: which of the device's, a page
/// of them, and how to get at the rest.
fn params_into(
    out: &mut Value,
    session: &Session,
    id: DeviceId,
    states: &[trill_engine::ParamState],
    request: &GetDeviceRequest,
) -> Result<(), String> {
    let project = session.project();
    let plugin = project.device(id).is_ok_and(|device| is_plugin(&device.uri));
    let defaults = session.device_param_defaults(id).map_err(|error| explain(project, error))?;
    let moved = |index: &usize| {
        let state = &states[*index];
        let room = (state.info.max - state.info.min).abs() * AT_DEFAULT;
        defaults.get(*index).is_some_and(|default| (state.value - default).abs() > room)
    };
    let many = states.len() > MANY_PARAMS;
    let every = || (0..states.len()).collect::<Vec<usize>>();

    #[derive(PartialEq)]
    enum Listed {
        Found,
        Changed,
        All,
    }
    let query = request.query.as_deref().map(str::trim).filter(|query| !query.is_empty());
    let (listed, chosen) = match (query, request.show.as_deref().map(str::to_ascii_lowercase).as_deref()) {
        (Some(query), _) => {
            let (whole, words) = (query.to_lowercase(), query.split_whitespace().map(str::to_lowercase));
            let words: Vec<String> = words.collect();
            let found = every().into_iter().filter(|index| {
                let info = &states[*index].info;
                let name = info.name.to_lowercase();
                info.id.to_lowercase().contains(&whole) || words.iter().all(|word| name.contains(word.as_str()))
            });
            (Listed::Found, found.collect())
        }
        (None, Some("all")) => (Listed::All, every()),
        (None, Some("none")) => (Listed::All, Vec::new()),
        (None, Some("changed")) => (Listed::Changed, every().into_iter().filter(moved).collect()),
        (None, Some(other)) => return Err(format!("show '{other}' should be \"changed\", \"all\" or \"none\"")),
        (None, None) if many => (Listed::Changed, every().into_iter().filter(moved).collect()),
        (None, None) => (Listed::All, every()),
    };
    let limit = request.param_limit.unwrap_or(if many { DEFAULT_PARAMS } else { MAX_PARAMS }).min(MAX_PARAMS);
    let offset = request.param_offset.unwrap_or(0).min(chosen.len());
    let page: Vec<usize> = chosen.iter().copied().skip(offset).take(limit).collect();

    let params: Vec<Value> = if plugin {
        // What the plugin shows for each value; and for the few a search
        // found, for its own default and for the values there are: every
        // step, or the ends of the range.
        let detailed = listed == Listed::Found && page.len() <= DETAILED_PARAMS;
        let mut asked: Vec<(usize, f64)> = Vec::new();
        let mut starts: Vec<usize> = Vec::new();
        for &index in &page {
            let info = &states[index].info;
            starts.push(asked.len());
            asked.push((index, states[index].value));
            if detailed {
                asked.push((index, defaults[index]));
                let span = steps_of(info).unwrap_or_else(|| vec![info.min, info.max]);
                asked.extend(span.into_iter().map(|value| (index, value)));
            }
        }
        starts.push(asked.len());
        let texts = session.device_param_texts(id, &asked).map_err(|error| explain(project, error))?;
        let rows = page.iter().enumerate().map(|(row, &index)| {
            let shown = &texts[starts[row]..starts[row + 1]];
            let detail = detailed.then(|| ParamDetail {
                default: shown[1].clone(),
                span: match steps_of(&states[index].info) {
                    Some(_) => ParamSpan::Choices(shown[2..].to_vec()),
                    None => ParamSpan::Range(shown[2].clone(), shown[3].clone()),
                },
            });
            plugin_param_json(&states[index], &shown[0], detail.as_ref())
        });
        rows.collect()
    } else {
        page.iter().map(|&index| param_state_json(&states[index])).collect()
    };

    out["param_count"] = json!(states.len());
    match listed {
        Listed::Found => out["matching"] = json!(chosen.len()),
        Listed::Changed => out["changed"] = json!(chosen.len()),
        Listed::All => {}
    }
    if offset > 0 {
        out["param_offset"] = json!(offset);
    }
    let shown = offset + page.len();
    if shown < chosen.len() {
        out["params_more"] = json!(format!("{} more: param_offset {shown} lists the next", chosen.len() - shown));
    }
    let mut notes: Vec<String> = Vec::new();
    if listed == Listed::Changed {
        notes.push(format!(
            "Of this device's {} parameters, these {} are not at its own defaults. `query` finds a parameter by \
             name (\"mic\", \"gain\"), with its range and choices; `show: \"all\"` lists every one.",
            states.len(),
            chosen.len()
        ));
    }
    if plugin && !page.is_empty() {
        notes.push(
            "`value` is the raw number (0 to 1 unless `max` says otherwise); `text` is what the plugin shows for \
             it. update_device sets a parameter by either: a number, or the text as a string."
                .to_string(),
        );
    }
    if !notes.is_empty() {
        out["params_note"] = json!(notes.join(" "));
    }
    out["params"] = json!(params);
    Ok(())
}

/// Every value of a parameter that moves in a few steps (a switch, a
/// choice, a count), from the lowest; `None` for one that does not.
fn steps_of(info: &trill_core::device::ParamInfo) -> Option<Vec<f64>> {
    let steps = info.max - info.min;
    let stepped = !matches!(info.kind, trill_core::device::ParamKind::Float) && (1.0..=NAMED_STEPS).contains(&steps);
    stepped.then(|| (0..=steps as usize).map(|step| info.min + step as f64).collect())
}

/// What a plugin's parameters came to after they were set, in the plugin's
/// own words by parameter name: the check that "-6 dB" landed on -6 dB.
/// `None` for a device that has no words of its own.
fn shown_values(session: &Session, device: DeviceId, set: &[(String, f64)]) -> Option<Value> {
    let plugin = session.project().device(device).is_ok_and(|model| is_plugin(&model.uri));
    if !plugin || set.is_empty() {
        return None;
    }
    let states = session.device_params(device).ok()?;
    let found: Vec<(usize, f64)> = set
        .iter()
        .filter_map(|(id, value)| Some((states.iter().position(|state| state.info.id == *id)?, *value)))
        .collect();
    let texts = session.device_param_texts(device, &found).ok()?;
    let shown: serde_json::Map<String, Value> =
        found.iter().zip(texts).map(|((index, _), text)| (states[*index].info.name.clone(), json!(text))).collect();
    Some(Value::Object(shown))
}

/// A sentence for names given on the wrong side of a plugin that has both
/// parameters and settings in its saved state: all of them at once, each
/// setting with the value it has. One refused call then sorts every name
/// of a patch, where it would take a `get_device` for each to learn which
/// side it is on.
fn misplaced(
    session: &Session,
    device: DeviceId,
    params: Option<&BTreeMap<String, Value>>,
    settings: &[(String, String)],
) -> Option<String> {
    let infos: Vec<_> = session.device_params(device).ok()?.into_iter().map(|state| state.info).collect();
    let in_state = session.device_settings(device).unwrap_or_default();
    let setting = |name: &str| in_state.iter().find(|(known, _)| known.eq_ignore_ascii_case(name));
    let are_settings: Vec<String> = params
        .into_iter()
        .flatten()
        .filter(|(key, _)| find_param_index(&infos, key).is_err())
        .filter_map(|(key, _)| setting(key))
        .map(|(name, value)| format!("'{name}' (reads {value} now)"))
        .collect();
    let are_params: Vec<String> = settings
        .iter()
        .filter(|(name, _)| setting(name).is_none())
        .filter_map(|(name, _)| infos.iter().find(|info| info.name.eq_ignore_ascii_case(name)))
        .map(|info| format!("'{}'", info.name))
        .collect();
    let mut said = Vec::new();
    match are_settings.as_slice() {
        [] => {}
        [one] => said.push(format!(
            "{one} is not a parameter of this device but a setting in its saved state: give it under `settings`, \
             written in the form its value has now"
        )),
        several => said.push(format!(
            "{} are not parameters of this device but settings in its saved state: give them under `settings`, \
             each written in the form its value has now",
            several.join(", ")
        )),
    }
    match are_params.as_slice() {
        [] => {}
        [one] => said.push(format!(
            "{one} is a parameter of this device, not a setting in its saved state: give it under `params`"
        )),
        several => said.push(format!(
            "{} are parameters of this device, not settings in its saved state: give them under `params`",
            several.join(", ")
        )),
    }
    (!said.is_empty()).then(|| format!("{}. Nothing was changed", said.join(". ")))
}

fn resolve_params(
    session: &Session,
    device: DeviceId,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<(String, f64)>, String> {
    let project = session.project();
    let states = session.device_params(device).map_err(|error| explain(project, error))?;
    let infos: Vec<_> = states.into_iter().map(|state| state.info).collect();
    let plugin = project.device(device).is_ok_and(|model| is_plugin(&model.uri));
    params
        .iter()
        .map(|(key, value)| {
            let index = find_param_index(&infos, key)?;
            let info = &infos[index];
            let parsed = match value {
                // A plugin's parameter set by a string is set in the
                // plugin's own words: "Dynamic 57", "-6 dB".
                Value::String(text) if plugin => match session.device_param_value(device, index, text) {
                    Ok(Some(value)) => value,
                    _ => parse_param_value(info, value).map_err(|_| not_shown(session, device, index, info, text))?,
                },
                _ => parse_param_value(info, value)?,
            };
            Ok((info.id.clone(), parsed))
        })
        .collect()
}

/// Says that a plugin shows no value of a parameter as `text`, and what it
/// does show: every step of a parameter with a few, or a few values
/// across its range.
fn not_shown(
    session: &Session,
    device: DeviceId,
    index: usize,
    info: &trill_core::device::ParamInfo,
    text: &str,
) -> String {
    const NAMED: usize = 16;
    let steps = steps_of(info);
    let stepped = steps.is_some();
    let at =
        steps.unwrap_or_else(|| (0..5).map(|part| info.min + (info.max - info.min) * f64::from(part) / 4.0).collect());
    let asked: Vec<(usize, f64)> = at.iter().map(|value| (index, *value)).collect();
    let mut shown: Vec<String> = Vec::new();
    for example in session.device_param_texts(device, &asked).unwrap_or_default() {
        if !example.is_empty() && !shown.contains(&example) {
            shown.push(example);
        }
    }
    let more = if shown.len() > NAMED { ", ..." } else { "" };
    let examples: Vec<String> = shown.iter().take(NAMED).map(|example| format!("'{example}'")).collect();
    let (between, among) = if stepped { ("one of", "") } else { ("values like", " across its range") };
    format!(
        "'{}' has no value that the plugin shows as '{text}'. It shows {between} {}{more}{among}. Give one of those as \
         a string, or a number from {} to {}",
        info.name,
        examples.join(", "),
        info.min,
        info.max
    )
}

fn set_params(session: &mut Session, device: DeviceId, params: &BTreeMap<String, Value>) -> Result<(), String> {
    let resolved = resolve_params(session, device, params)?;
    session.set_device_params(device, &resolved).map_err(|error| error.to_string())
}

/// What a caller who cannot hear should know about an audio clip it has
/// just made or changed: settings that have no effect as they stand, a
/// clip longer than its audio, audio still being prepared.
fn add_audio_notes(out: &mut Value, runtime: &Runtime, session: &Session, clip: ClipId) {
    let project = session.project();
    let Ok(clip) = project.clip(clip) else { return };
    let Some(audio) = clip.audio() else { return };
    let mut notes = Vec::new();
    if audio.transpose != 0.0 && !audio.warp.keeps_pitch() {
        notes.push(format!(
            "transpose has no effect with warp \"{}\"; set warp to \"beats\" or \"tonal\" to shift the pitch",
            warp_name(audio.warp)
        ));
    }
    if let Ok(facts) = runtime.sound_facts(Path::new(&audio.path)) {
        let left = (facts.seconds - audio.offset_seconds).max(0.0);
        let audio_beats = audio.beats_over(&project.tempo_map(), clip.start, left);
        out["audio_beats"] = json!(round(audio_beats));
        if clip.length > audio_beats + 1e-3 {
            notes.push(format!(
                "the clip is {} beats long but its audio ends after {}; the rest is silent (set length to {} to fit)",
                round(clip.length),
                round(audio_beats),
                round(audio_beats)
            ));
        }
    }
    if session.preparing() > 0 && audio.warp != WarpMode::Off {
        notes.push("the warped audio is being prepared; until then playback is in time but repitched".to_string());
    }
    if !notes.is_empty() {
        out["notes"] = json!(notes);
    }
}

/// What became of notes that said how they are played: the keyswitches
/// that were pressed for them, and what was done unasked.
/// What a reply says of channels: which the clip uses, where it uses
/// another than the first; and, of what was just written on `written`,
/// that a channel is nothing to choose on an instrument that names none.
pub(crate) fn say_channels(
    out: &mut Value,
    session: &Session,
    clip: ClipId,
    keys: &crate::instruments::TrackKeys,
    written: impl Iterator<Item = Channel>,
) {
    if let Ok(clip) = session.project().clip(clip)
        && clip.uses_channels()
    {
        out["channels"] = json!(clip.channels());
    }
    let mut written: Vec<Channel> = written.filter(|channel| !channel.is_first()).collect();
    written.sort_unstable();
    written.dedup();
    if let Some(warning) = written.first().and_then(|channel| keys.channel_warning(*channel)) {
        let known = out["warning"].as_str().map(|known| format!("{known} ")).unwrap_or_default();
        out["warning"] = json!(format!("{known}{warning}"));
    }
}

fn say_played(out: &mut Value, played: Option<&crate::instruments::Played>) {
    let Some(played) = played else { return };
    out["keyswitches"] = json!(played.keyswitches);
    if !played.remarks.is_empty() {
        out["notes"] = json!(played.remarks);
    }
}

/// Notes that start at or past the end of their clip never play. Say so,
/// since the caller cannot hear that they are missing.
fn add_range_warning(out: &mut Value, session: &Session, clip: ClipId) {
    let Ok(clip) = session.project().clip(clip) else { return };
    let Some(midi) = clip.midi() else { return };
    let playable = midi.loop_length.map_or(clip.length, |length| length.min(clip.length));
    let outside = midi.notes.iter().filter(|note| note.start >= playable).count();
    let mut warnings = Vec::new();
    if outside > 0 {
        warnings.push(format!(
            "{outside} note(s) start at or after beat {} of the clip, past its {}, and will not play. \
             Lengthen the clip or move the notes.",
            round(playable),
            if midi.loop_length.is_some() { "loop length" } else { "end" }
        ));
    }
    // What the track's instrument cannot play: only keyswitches, notes outside its range.
    warnings.extend(crate::instruments::profile_warnings(session, clip.id));
    if !warnings.is_empty() {
        out["warning"] = json!(warnings.join(" "));
    }
}

/// A call's future, polled as the call's doing: whatever it changes in
/// the session while it runs is entered in the journal as that call's.
struct Acted<'a> {
    call: Option<CallId>,
    future: Pin<Box<dyn Future<Output = Result<CallToolResponse, ErrorData>> + Send + 'a>>,
}

impl Future for Acted<'_> {
    type Output = Result<CallToolResponse, ErrorData>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // Only for the length of the poll: between polls the thread is
        // someone else's, and the next poll may be on another.
        let _acting = self.call.map(trill_engine::journal::acting);
        self.future.as_mut().poll(context)
    }
}

/// Closes a call in the journal when it ends, however it ends: a call
/// whose client went away in the middle is closed as one that failed.
struct Closing<'a> {
    session: &'a SharedSession,
    call: Option<CallId>,
}

impl Closing<'_> {
    fn with(mut self, outcome: impl FnOnce(&Session) -> Outcome) {
        if let Some(call) = self.call.take() {
            let mut session = self.session.lock();
            let outcome = outcome(&session);
            session.end_call(call, outcome);
        }
    }
}

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        if let Some(call) = self.call.take() {
            self.session.lock().end_call(call, Outcome { failed: true, ..Outcome::default() });
        }
    }
}

impl TrillServer {
    /// Who a call is entered in the journal as: the assistant in the
    /// window for its door, and any other client by the name it gave.
    fn author(&self, context: &RequestContext<RoleServer>) -> Author {
        match &self.guard {
            Some(_) => Author::Assistant,
            None => {
                let name = context.peer.peer_info().map(|info| info.client_info.name.clone()).unwrap_or_default();
                Author::Client(name)
            }
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TrillServer {
    /// Runs a tool, entering the call in the session's journal: opened
    /// before it runs, under what it does in words, and closed after, with
    /// where it landed and what it heard.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool = request.name.to_string();
        let input = Value::Object(request.arguments.clone().unwrap_or_default());
        // A tool that asks something of a device that is still being made
        // waits for it here, with the session free.
        self.await_devices_of(&tool, &input).await;
        let call = {
            let by = self.author(&context);
            let mut session = self.session.lock();
            let label = crate::words::describe(&tool, &input, session.project()).did;
            session.begin_call(by, &tool, label)
        };
        let closing = Closing { session: &self.session, call };
        let running = ToolCallContext::new(self, request, context);
        let response = Acted { call, future: Box::pin(self.tool_router.call(running)) }.await;
        closing.with(|session| {
            let answered = match &response {
                Ok(CallToolResponse::Complete(result)) if result.is_error != Some(true) => {
                    Some(result.content.first().and_then(|block| block.as_text()).map_or("", |text| text.text.as_str()))
                }
                _ => None,
            };
            match answered {
                None => Outcome { failed: true, ..Outcome::default() },
                Some(answer) => {
                    let place = crate::words::of_call(&tool, &input, answer);
                    let places = place.and_then(|place| place.settled(session.project())).into_iter().collect();
                    Outcome { places, heard: crate::words::heard(&tool, answer), failed: false }
                }
            }
        });
        response
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("trill", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!(
                "{INSTRUCTIONS}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
                crate::sounds::INSTRUCTIONS,
                crate::catalogue::INSTRUCTIONS,
                crate::listening::INSTRUCTIONS,
                crate::studying::INSTRUCTIONS,
                crate::wavetable::INSTRUCTIONS,
                crate::files::INSTRUCTIONS,
                crate::music::INSTRUCTIONS,
                crate::instruments::INSTRUCTIONS,
                crate::hearing::INSTRUCTIONS,
                crate::playing::INSTRUCTIONS,
                crate::story::INSTRUCTIONS,
                crate::film::INSTRUCTIONS
            ))
    }
}
