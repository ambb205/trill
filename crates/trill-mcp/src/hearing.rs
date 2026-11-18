//! Tools for hearing: what a caller that cannot listen uses instead of
//! ears.
//!
//! * `probe` plays one instrument a list of notes and measures each: which
//!   sound at all, at what pitch, how soon, for how long, how bright. It
//!   answers what otherwise takes a render per guess: an instrument's
//!   range, its octave, which keys switch its articulation, which notes of
//!   a drum plugin are which drum. It changes nothing and writes nothing.
//! * `measure` renders the song, or a stretch of it, or a few of its
//!   tracks, and reports the numbers without writing a file.
//! * `render` does the same and writes the WAV: for the result.
//!
//! The measuring itself is the engine's (`trill_engine::render`,
//! `trill_engine::probe`). What is here is the arguments, replies short
//! enough to read, and the plain-language notes about what the numbers
//! mean.

use crate::input::{PitchInput, controller_scale, parse_controller, parse_pitch_value};
use crate::server::{Reply, TrillServer, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use trill_core::InstrumentProfile;
use trill_core::pitch::{drum_name, pitch_name};
use trill_core::{ControllerKind, Project, TrackId};
use trill_engine::probe::{Heard, ProbeNote, ProbeOptions};
use trill_engine::render::{RenderError, Section, Stems, TrackLevel, WaitedFor};
use trill_engine::{ProbeTarget, RenderOptions, RenderReport, Session};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Hearing what you wrote
You cannot listen, so measure. Three tools, from quick to slow:
- probe plays one instrument a list of notes and measures each, in a second or two, without touching the song. \
Call it ONCE for every plugin that Trill does not know yet, before you write for it. A track says what it knows \
in get_project (`plays`, `articulations`, `range`) and all of it in get_instrument_profile, and a probe's reply \
says whether the plugin has an entry: one that has needs no probe, and a drum plugin whose kit Trill reads \
(get_instrument_profile shows `kit`) needs none to find its drums. With no `notes` it plays the whole keyboard and tells you which notes sound (the \
range), at what pitch (an instrument may sound an octave away from what is written) and, for a drum plugin, \
what each note is (a low thump, a noisy mid hit, a long bright wash). With `keyswitches` it tells you which \
keys change the articulation and how (shorter, darker). Then write down what it found, once, with \
set_instrument_profile: its arguments take a probe's reply as it stands, and the plugin is known from then on, \
in every song. Notes below or above an instrument's range are silent, and so is a clip that holds only \
keyswitches: with the instrument known, the writing tools keep to its range and press its keyswitches themselves.
- measure renders the song (or `start`/`end`, or only some `tracks`) and reports loudness, true peak, spectral \
balance, the level and loudness of every track, the loudness of every section between the markers, and how many \
dB each compressor and limiter took off. It writes no file. Use it after every change you want to check; set a \
compressor's threshold from its gain reduction, not by guessing.
- render does what measure does and writes a WAV file. Call it for the finished result, or when the user asks \
for a file; never to check your work. With `stems` the same pass also writes every track, group and return to a \
file of its own: ask once for all of them, never render the song once per track.
Both hear only some of the song with `tracks`, and then make and wait for only those tracks and what feeds them: \
hearing one track of a long song costs what that track costs. Naming a return there gives that return alone, as \
every track's send feeds it; naming a group gives the group with its members.
A stretch (`start`, `end`) begins as the song has it there: the keyswitch a part chose before it and its \
controllers, wheel and pedals are where the part left them, and a note that is sounding at the start is heard \
from where it began (up to 32 beats before; the reply says if one began earlier and is left out). What only \
rings over from notes that ended before the start is not in it. The transport does the same wherever it is \
started, but does not begin notes that are already sounding there.
Both run at the engine's sample rate unless `sample_rate` asks for another (without an audio device the engine \
runs at 48000 Hz, whatever the song's files are at), and their replies state the rate they ran at.
probe, measure and render make their copies of a song's plugins in a process of their own. A plugin that \
crashes there costs the call an error that names the plugin, where that can be told, and says what to do; \
the song that is open is untouched. Call again, or mute the track to hear the rest.
Some sampled instruments draw at random which recording of a note they play, and the reply says when a song \
has one. A change anywhere in what such an instrument plays then changes how every later note of it sounds, so \
a render before a change and one after it differ in more than the change. To compare the two, make both with \
`repeatable: true`, which takes the chance out of the render's own copies and leaves the song as it is.
Both use several of the machine's processors (half of them, eight at the most): tracks that do not feed one \
another are made at once, and what comes out is the samples one processor would have made, sooner. The reply's \
`threads` says how many it was; `threads` in the call asks for another number.
A plugin that plays samples (a drum kit, a sampled guitar) reads them after it is made or given another kit or \
preset, for seconds or as much as a minute, and until a drum has arrived its key is silent. probe, measure and \
render wait for that on a copy of their own, so what they report is the whole instrument; if notes stayed \
silent to the end they say which, because a key that plays nothing and a sample that is not loaded yet sound the \
same. If a plugin never answers ANY of the notes it is to play, nothing is measured or rendered and the call \
fails, naming the track and what its notes say: a mix without one of its instruments is not the song. Ask again \
if it was loading; otherwise fix the track, or mute it to hear the rest. The plugin the user hears is not waited for: get_project and get_device mark it `loading` while it reads, \
and transport says so when you press play too early. A plugin is made after the song has it, away from the song, \
whatever brought it: a song that was opened, add_device, create_track, an undo. That takes a second or more a \
plugin, and ten for a sampled instrument nothing has read lately. Until it is there the device is marked \
`not_made_yet`, `loading` says how far the song's devices are, and its track is silent played live. Do not wait \
for it yourself: write on, give it its preset (it arrives as that preset), and a tool that needs the device \
itself waits for it.";

/// The sample rates a render can be asked to run at.
const RENDER_RATES: std::ops::RangeInclusive<u32> = 8_000..=384_000;

/// What a level at or below this is called: nothing at all.
const SILENT_DB: f32 = -119.0;
/// A sound that begins this long after its key went down is said to.
const LATE_ONSET_SECONDS: f64 = 0.01;
/// The most notes a probe plays in one call, and the most audio it makes.
const MAX_PROBE_NOTES: usize = 128;
const MAX_PROBE_SECONDS: f64 = 130.0;
/// How long a note is held and listened to: across the whole keyboard,
/// for notes that were asked for, and under a keyswitch.
const SCAN_TIMES: (f64, f64) = (0.25, 0.75);
const NOTE_TIMES: (f64, f64) = (0.5, 1.5);
const SWITCH_TIMES: (f64, f64) = (0.8, 0.8);

// ---- arguments ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SectionInput {
    /// What to call it in the reply.
    pub name: Option<String>,
    /// First beat of the section. Give `start` and `end`, or `bars`.
    pub start: Option<f64>,
    /// Beat the section ends at.
    pub end: Option<f64>,
    /// The section as bars instead: [first, last], counted from 1, both included. [9, 16] is bars 9 to 16.
    pub bars: Option<Vec<u32>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MeasureRequest {
    /// First beat to render (default 0).
    pub start: Option<f64>,
    /// Beat to stop at (default: end of the last clip).
    pub end: Option<f64>,
    /// Hear only these tracks (their ids), as if they were soloed. Mute and solo in the song are not changed.
    /// Only these tracks, and the groups and returns their sound goes through, are made and waited for: one
    /// track of a song with many plugins costs what that track costs. A return named here is heard alone, as
    /// every track's send feeds it, with the tracks themselves unheard; a group is heard with its members.
    pub tracks: Option<Vec<u64>>,
    /// The sample rate to render at, in Hz: 44100, 48000, 96000 (default: the rate the engine runs at, which
    /// is 48000 when it has no audio device). The reply's `sample_rate` says what it ran at.
    pub sample_rate: Option<u32>,
    /// Stretches of the song to measure one by one. Omit to get one per marker (each from its marker to the
    /// next); give [] for none.
    pub sections: Option<Vec<SectionInput>>,
    /// Add each track's spectral balance, stereo correlation and loudness per section. Leave off unless you
    /// need them: the reply always has every track's peak and loudness.
    #[serde(default)]
    pub detail: bool,
    /// Longest reverb/delay tail to keep after the end, in seconds (default 10; trimmed when silent sooner).
    pub max_tail_seconds: Option<f64>,
    /// Compare two renders with this on in both. Some sampled instruments draw at random which recording of a
    /// note they play, so two renders of one song differ and the difference hides what you changed (the reply
    /// says when a song has such instruments). With `repeatable` the render's own copies of them take their
    /// recordings in turn: the song itself is not changed, two such renders are the same samples, and they
    /// sound slightly different from the song as it plays (no random takes, no fret noise). Leave off to hear
    /// the song as it is.
    #[serde(default)]
    pub repeatable: bool,
    /// How many of the machine's processors make it at the most. Leave it out: half of them are used (eight
    /// at the most), tracks that do not feed one another are made at once, and the samples are the ones a
    /// single processor makes. 1 makes it a track at a time: to compare the time, or to leave the machine to
    /// other work. The reply's `threads` says how many it was.
    pub threads: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RenderRequest {
    /// Output WAV path. Defaults to a `renders` folder beside the project file, or the system temp folder.
    pub path: Option<String>,
    /// Also write parts of the song to WAV files of their own, in this same pass: one render in place of one
    /// per track. Any of "tracks" (every track that has clips, after its effects, fader and pan, without what
    /// its sends give the returns), "instruments" (what goes into each track's effects: its instrument
    /// alone), "groups", "returns" (each return, fed by every track's send), "premaster" (the mix as it reaches
    /// the master's devices), or "all". The files go into a folder beside `path` named after it
    /// ("mix stems" for "mix.wav"); they are 32-bit float, as long as the mix, and the reply lists each with
    /// its track, what it is and its level. Leave out when the mix is all that is wanted.
    pub stems: Option<Vec<StemChoice>>,
    #[serde(flatten)]
    pub what: MeasureRequest,
}

/// A kind of stem to write beside a render.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StemChoice {
    Tracks,
    Instruments,
    Groups,
    Returns,
    Premaster,
    All,
}

/// The stems of `choices`, into the folder that goes with the mix at
/// `mix`: beside it, named after it. `None` when none were asked for.
fn stems_for(choices: &[StemChoice], mix: &Path) -> Option<Stems> {
    let has = |choice: StemChoice| choices.contains(&choice) || choices.contains(&StemChoice::All);
    let name = mix.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "render".into());
    let stems = Stems {
        folder: mix.with_file_name(format!("{name} stems")),
        tracks: has(StemChoice::Tracks),
        instruments: has(StemChoice::Instruments),
        groups: has(StemChoice::Groups),
        returns: has(StemChoice::Returns),
        premaster: has(StemChoice::Premaster),
    };
    (!choices.is_empty()).then_some(stems)
}

/// A pitch, or a run of pitches, to probe.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct PitchesInput(pub Value);

impl JsonSchema for PitchesInput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Pitches".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "One pitch (a MIDI number 0-127, a note name such as \"E2\", or a drum name such as \
                \"snare\") or a run of pitches from one to another, both included: \"E1-G4\", \"36-60\".",
            "type": ["integer", "string"]
        })
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ProbeRequest {
    /// Id of a track: its instrument is played exactly as it is now (its preset, and whatever was changed
    /// since). Give `track`, or `device` for an instrument that is not in the song.
    pub track: Option<u64>,
    /// With `track`: play through the devices after the instrument too (amp, EQ, compressor), as far as the
    /// fader. Default false: the instrument alone, which is what tells you its range and its pitch.
    #[serde(default)]
    pub effects: bool,
    /// URI of an instrument that need not be in the song, e.g. "builtin:polysynth" or a plugin's (see
    /// `list_devices`): try it before adding it.
    pub device: Option<String>,
    /// With `device`: the preset to start from, by name, "Category/Name" or id, or the whole path of a preset
    /// file (as add_device takes one). Omit for the instrument as it comes.
    pub preset: Option<String>,
    /// What to play, each note by itself: pitches and runs of pitches, e.g. ["E1-G4"], [36, 38, "42-51"],
    /// ["E2", "E3"]. Omit to play the whole keyboard quickly, which finds the range of an instrument or the
    /// notes of a drum plugin.
    pub notes: Option<Vec<PitchesInput>>,
    /// With a run of pitches: play every `step`th note of it (default 1, every note).
    pub step: Option<u8>,
    /// Velocities to play every note at, 1-127 (default [100]). Several show how an instrument answers to
    /// being played harder.
    pub velocities: Option<Vec<u8>>,
    /// Seconds each note is held (default 0.5; 0.25 when `notes` is omitted). Hold longer to see how a note
    /// that is held dies away.
    pub length: Option<f64>,
    /// Longest to go on listening after a note is let go, in seconds (default 1.5; less when there are many
    /// notes). Listening stops as soon as the note has died away.
    pub tail: Option<f64>,
    /// Keys held down through the whole probe, e.g. a keyswitch you already know: ["C0"].
    pub hold: Option<Vec<PitchInput>>,
    /// Keys to try as keyswitches: each is held in turn while `notes` are played, and the reply says which of
    /// them change the sound and how, which do nothing, and which are notes themselves. Needs `notes`: one or
    /// two notes the instrument plays. E.g. keyswitches ["C0-B0"], notes ["E2"].
    pub keyswitches: Option<Vec<PitchesInput>>,
    /// Controller values for the whole probe, by name or number: {"expression": 100, "mod_wheel": 64,
    /// "pitch_bend": 1}. Values are 0-127 (pitch_bend: -1 to 1). An instrument that is silent without
    /// expression sounds with {"expression": 100}.
    pub controllers: Option<BTreeMap<String, f64>>,
    /// List every note by itself. Default false: runs of neighbouring notes that measure alike are one entry.
    #[serde(default)]
    pub detail: bool,
}

// ---- measure and render ------------------------------------------------------------

fn default_render_path(session: &Session) -> PathBuf {
    let name: String = session
        .project()
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let folder = match session.path().and_then(Path::parent) {
        Some(parent) => parent.join("renders"),
        None => std::env::temp_dir().join("trill-renders"),
    };
    folder.join(format!("{name}-{stamp}.wav"))
}

/// The stretches of the song a render is to measure one by one: the ones
/// asked for, or one per marker, as far as they lie in what is rendered.
fn sections_of(
    project: &Project,
    asked: Option<&[SectionInput]>,
    start: f64,
    end: f64,
) -> Result<Vec<Section>, String> {
    let Some(asked) = asked else {
        let mut markers: Vec<&trill_core::project::Marker> = project.markers.iter().collect();
        markers.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        let ends = markers.iter().skip(1).map(|marker| marker.beat).chain([f64::INFINITY]);
        return Ok(markers
            .iter()
            .zip(ends)
            .map(|(marker, next)| (marker.name.clone(), marker.beat.max(start), next.min(end)))
            .filter(|(_, from, to)| to > from)
            .map(|(name, start_beat, end_beat)| Section { name, start_beat, end_beat })
            .collect());
    };
    let bars = project.meter_map();
    let mut sections = Vec::with_capacity(asked.len());
    for (index, section) in asked.iter().enumerate() {
        let (from, to, named) = match (section.start, section.end, section.bars.as_deref()) {
            (Some(from), Some(to), None) => (from, to, format!("{}-{}", round(from), round(to))),
            (None, None, Some([first, last])) if *first >= 1 && last >= first => {
                (bars.bar_start(first - 1), bars.bar_start(*last), format!("bars {first}-{last}"))
            }
            _ => {
                return Err(format!(
                    "section {index} needs `start` and `end` in beats, or `bars`: [first, last] counted from 1"
                ));
            }
        };
        if to.partial_cmp(&from) != Some(std::cmp::Ordering::Greater) || from < 0.0 {
            return Err(format!("section {index} is empty: it runs from beat {from} to {to}"));
        }
        sections.push(Section { name: section.name.clone().unwrap_or(named), start_beat: from, end_beat: to });
    }
    Ok(sections)
}

/// The lowest and the highest note the track plays in the stretch, and
/// how many notes it plays there.
fn played_notes(track: &trill_core::Track, start: f64, end: Option<f64>) -> Option<(u8, u8, usize)> {
    let mut found: Option<(u8, u8, usize)> = None;
    for clip in track.clips.iter().filter(|clip| !clip.muted) {
        for note in clip.timeline_notes() {
            if note.start < start || end.is_some_and(|end| note.start >= end) {
                continue;
            }
            found = Some(match found {
                Some((low, high, count)) => (low.min(note.pitch), high.max(note.pitch), count + 1),
                None => (note.pitch, note.pitch, 1),
            });
        }
    }
    found
}

/// The tracks that had something to play in the rendered stretch and made
/// no sound, each with what is known about why and how to find out the
/// rest. A part that is silent in a full mix is easy to miss in the
/// numbers.
///
/// With a plugin the cause is rarely a mistake in the mix. In the order
/// they turn up: the notes lie outside what the instrument plays, the
/// clips hold only keys that switch its articulation, it has no sound
/// loaded, or it is one of the instruments modelled on breath or a bow
/// that wait for expression. What the song itself shows is said (notes
/// that are all very low are keyswitches on most instruments); the rest is
/// one `probe` away, which is what the note asks for.
fn silent_track_notes(
    project: &Project,
    levels: &[TrackLevel],
    waited: &[WaitedFor],
    start: f64,
    end: Option<f64>,
) -> Vec<String> {
    let silent: Vec<TrackId> =
        levels.iter().filter(|level| level.peak_db <= SILENT_DB).map(|level| level.track).collect();
    silent_notes(project, &silent, waited, start, end)
}

/// [`silent_track_notes`], for tracks that are known to have made no
/// sound.
fn silent_notes(
    project: &Project,
    silent: &[TrackId],
    waited: &[WaitedFor],
    start: f64,
    end: Option<f64>,
) -> Vec<String> {
    let soloing = project.tracks.iter().any(|track| track.solo);
    let mut notes = Vec::new();
    for id in silent {
        let Ok(track) = project.track(*id) else { continue };
        // Silenced on purpose is not news.
        if track.mute || (soloing && !track.solo) || track.clips.iter().all(|clip| clip.muted) {
            continue;
        }
        let playing = |clip: &trill_core::Clip| clip.end() > start && end.is_none_or(|end| clip.start < end);
        if !track.clips.iter().any(|clip| !clip.muted && playing(clip)) {
            continue;
        }
        let plugin = track.devices.iter().find(|device| device.enabled && !device.uri.starts_with("builtin:"));
        let Some(device) = plugin else {
            notes.push(format!(
                "'{}' has clips here but made no sound. Check that it has an instrument, that the instrument has a \
                 sound loaded (get_device), and that its volume and its group are up.",
                track.name
            ));
            continue;
        };
        let id = track.id.0;
        let mut note = format!(
            "'{}' has clips here but made no sound. Its instrument is the plugin {} (device {}).",
            track.name, device.name, device.id.0
        );
        // What the track knows about its instrument explains it, if it
        // does; otherwise what the notes themselves say.
        let range = played_notes(track, start, end);
        let known = crate::instruments::known_silence(track, start, end);
        if let Some(reason) = &known {
            note.push_str(reason);
            notes.push(note);
            continue;
        }
        match range {
            None => note.push_str(" Its clips here hold no notes."),
            Some((low, high, count)) if high < 24 => note.push_str(&format!(
                " Its {count} note(s) here lie between {} and {}: that low they are keyswitches on most \
                 instruments, which choose how the next notes are played and make no sound themselves. Write the \
                 notes to be heard as well.",
                pitch_name(low),
                pitch_name(high)
            )),
            Some((low, high, _)) => note.push_str(&format!(
                " Its notes here run from {} to {}; an instrument is silent for notes outside its range.",
                pitch_name(low),
                pitch_name(high)
            )),
        }
        // Whether the clips that played were given air already.
        let breathes = |clip: &trill_core::Clip| {
            [ControllerKind::EXPRESSION, ControllerKind::BREATH].iter().any(|kind| clip.controller(*kind).is_some())
        };
        let expressive = track.clips.iter().any(|clip| !clip.muted && playing(clip) && breathes(clip));
        note.push_str(&format!(
            " Find out what it answers to: probe with track {id} plays it the whole keyboard and lists the notes \
             that sound and the pitch they sound at."
        ));
        if expressive {
            note.push_str(
                " Its clips have an expression curve: check that it is above zero where the notes are \
                 (get_controllers).",
            );
        } else if range.is_some_and(|(_, high, _)| high >= 24) {
            note.push_str(&format!(
                " If the probe is silent too, the instrument has no sound loaded (get_device, or a preset), or it \
                 is one modelled on breath or a bow, which waits for expression: probe with track {id} and \
                 controllers {{\"expression\": 100}} tells, and shape_dynamics writes the curve."
            ));
        }
        if waited.iter().any(|waited| waited.track == Some(track.id) && !waited.answered) {
            note.push_str(" (The render waited for it to answer before it started, and it never did.)");
        }
        notes.push(note);
    }
    notes
}

/// Plain-language observations about what a render measured.
fn observations(report: &RenderReport, project: &Project) -> Vec<String> {
    let stats = &report.stats;
    let mut notes: Vec<String> = Vec::new();
    if stats.peak_db <= SILENT_DB {
        notes.push(
            "The render is silent. Check that tracks have instruments, clips have notes, and nothing is muted."
                .to_string(),
        );
        return notes;
    }
    if stats.clipped_samples > 0 {
        notes.push("The output clips. Lower track volumes or keep a limiter last on the master.".to_string());
    } else if let Some(true_peak) = stats.true_peak_db.filter(|true_peak| *true_peak > -0.1) {
        notes.push(format!(
            "The true peak is {true_peak:.1} dB: between the samples the mix goes over full scale, which clips \
             once it is converted or encoded. Lower the limiter's ceiling to -1 dB."
        ));
    }
    if stats.loudness_lufs < -20.0 {
        notes.push(
            "Quiet master (streaming targets are around -14 LUFS). Raise track levels or the limiter gain.".to_string(),
        );
    } else if stats.loudness_lufs > -8.0 {
        notes.push("Very loud master; it may sound crushed. Consider lowering the limiter gain.".to_string());
    }
    if stats.stereo_correlation < 0.0 {
        notes.push("Negative stereo correlation: parts may cancel in mono. Reduce width or chorus depth.".to_string());
    }
    if stats.spectrum.sub_db > -3.0 {
        notes.push("Most of the energy is below 60 Hz. Thin out the sub bass or kick.".to_string());
    }
    if stats.spectrum.presence_db < -30.0 && stats.spectrum.air_db < -30.0 {
        notes.push(
            "Very little energy above 2 kHz; the mix may sound dull. Open filters or add brighter parts.".to_string(),
        );
    }
    for dynamics in &report.dynamics {
        let on = match dynamics.track.and_then(|track| project.track(track).ok()) {
            Some(track) => format!("on '{}'", track.name),
            None => "on the master".to_string(),
        };
        let device = project.device(dynamics.device).ok();
        let limiter = device.is_some_and(|device| device.uri == "builtin:limiter");
        // The track it listens to in place of its own, if it does.
        let key = device.and_then(|device| device.sidechain).and_then(|track| project.track(track).ok());
        if let (0.0, Some(key)) = (dynamics.most_db, key) {
            notes.push(format!(
                "{} (device {}) {on} listens to '{}', which never reached its threshold over this stretch: it does \
                 nothing. Lower the threshold to below that track's peak as it leaves its devices.",
                dynamics.name, dynamics.device.0, key.name
            ));
        } else if dynamics.most_db == 0.0 && !limiter {
            notes.push(format!(
                "{} (device {}) {on} never reached its threshold: it does nothing. Lower the threshold to below \
                 that track's peak, or remove it.",
                dynamics.name, dynamics.device.0
            ));
        } else if dynamics.typical_db > 10.0 && dynamics.active > 0.5 {
            notes.push(format!(
                "{} (device {}) {on} takes off {:.0} dB most of the time: the part is squashed and {:.0} dB \
                 quieter unless makeup gain puts it back. Raise the threshold or lower the ratio.",
                dynamics.name, dynamics.device.0, dynamics.typical_db, dynamics.typical_db
            ));
        }
    }
    notes.extend(report.waited.iter().filter(|waited| waited.answered).map(|waited| waited_note(waited, project)));
    notes
}

/// What to say of notes that were sounding where a render started and
/// began too long before it to be played from where they began.
fn not_begun_note(left_out: &trill_engine::render::NotBegun) -> String {
    format!(
        "{} on '{}' {} already sounding where this starts, from more than {} beats before: {} not in it. Start \
         earlier to hear {}.",
        if left_out.notes == 1 { "A note".to_string() } else { format!("{} notes", left_out.notes) },
        left_out.track_name,
        if left_out.notes == 1 { "was" } else { "were" },
        trill_engine::render::LEAD_IN_BEATS,
        if left_out.notes == 1 { "it is" } else { "they are" },
        if left_out.notes == 1 { "it" } else { "them" },
    )
}

/// What to say of the instruments of a render that draw at random what
/// they play: that the next render will differ and how to stop it, or,
/// where it was stopped, that this is not quite the song as it plays.
fn draws_notes(draws: &[trill_engine::render::Draws]) -> Vec<String> {
    let named = |these: &[&trill_engine::render::Draws]| -> String {
        let names: Vec<String> =
            these.iter().map(|draws| format!("{} on '{}'", draws.device_name, draws.track_name)).collect();
        names.join(", ")
    };
    let mut notes = Vec::new();
    let stoppable = |draws: &&trill_engine::render::Draws| !draws.costs.is_empty();
    let held: Vec<_> = draws.iter().filter(|draws| draws.held).collect();
    let free: Vec<_> = draws.iter().filter(stoppable).filter(|draws| !draws.held && draws.failed.is_none()).collect();
    // What nothing is known to stop is said by itself, whatever was asked.
    for draws in draws.iter().filter(|draws| !stoppable(draws)) {
        notes.push(format!(
            "{} on '{}' {}. Two renders of this song may differ there, and nothing is known that stops it: \
             `repeatable` leaves it as it is. To compare two renders, go by that track's levels, or leave it out of \
             what you compare (`tracks`).",
            draws.device_name, draws.track_name, draws.what
        ));
    }
    if let Some(first) = held.first() {
        notes.push(format!(
            "Made repeatable: {} rendered with {}. The song itself is unchanged; this is the same samples every \
             time, and not quite what the song sounds like as it plays.",
            named(&held),
            first.costs
        ));
    }
    if let Some(first) = free.first() {
        let one = free.len() == 1;
        // Made in a process that had just started, where these begin their
        // draws at the same place, the song is these samples every time;
        // made anywhere else, it is not.
        let what_follows = match free.iter().all(|draws| draws.again) {
            true => format!(
                "Rendered like this, in a process of its own, the same song comes out the same every time; but a \
                 change to what {} or to another track of the same instrument moves every draw after it, so a \
                 render before a change and one after it differ in more than the change",
                if one { "it plays" } else { "one of them plays" }
            ),
            false => "so the next render of this song will not be these samples".to_string(),
        };
        notes.push(format!(
            "{} draw{} at random what {} as {} set{} {what_follows}. To compare two renders, make both with \
             `repeatable: true`: the render's own copies are given {}, and the song is left as it is.",
            named(&free),
            if one { "s" } else { "" },
            if one { "it plays" } else { "they play" },
            if one { "it is" } else { "they are" },
            if free.iter().all(|draws| draws.again) { "." } else { "," },
            first.costs
        ));
    }
    for draws in draws.iter().filter(|draws| draws.failed.is_some()) {
        notes.push(format!(
            "{} on '{}' could not be made repeatable ({}): it was rendered as it is.",
            draws.device_name,
            draws.track_name,
            draws.failed.as_deref().unwrap_or_default()
        ));
    }
    notes
}

/// What to say of a render that was made on one thread because of the
/// devices in it that are not run beside any other.
fn alone_note(alone: &[trill_engine::render::Alone]) -> Option<String> {
    let first = alone.first()?;
    let more = match alone.len() - 1 {
        0 => String::new(),
        more => format!(" (and {more} more like it)"),
    };
    Some(format!(
        "This was made on one processor, a track at a time, because {} on '{}'{more} is not run beside other \
         devices: it took as long as the song takes without the others.",
        first.device_name, first.track_name
    ))
}

/// What to say of a plugin that was waited for and answered: how long it
/// took, and which of its notes stayed silent, if any did. A key the
/// track itself knows for a keyswitch is not one of them: it is meant to
/// sound nothing, and whoever asked for a way of playing by its name
/// never chose the key.
fn waited_note(waited: &WaitedFor, project: &Project) -> String {
    let mut note = format!(
        "Waited {:.1} s for {} on '{}' to be ready before the render started.",
        waited.seconds, waited.device_name, waited.track_name
    );
    let profile = waited.track.and_then(|track| project.track(track).ok()).map(|track| &track.profile);
    let known = |pitch: u8| profile.is_some_and(|profile| profile.keyswitch_name(pitch).is_some());
    let silent: Vec<u8> = waited.silent.iter().copied().filter(|pitch| !known(*pitch)).collect();
    if !silent.is_empty() {
        // The one case a wait cannot settle: a key that plays nothing and
        // a sample that has not been loaded sound the same.
        note.push_str(&format!(
            " {} of the notes it plays made no sound the whole time: {}. Either it does not play them (a \
             keyswitch, a note outside its range) or it was still loading them when this was made, and then \
             they are missing here. `probe` with track {} and those notes says which.",
            silent.len(),
            runs(&silent),
            waited.track.map_or(0, |track| track.0)
        ));
    }
    note
}

/// What to say of a render that was not made because instruments never
/// answered the notes they are to play: which, what their notes say of
/// why, and what to do.
fn not_answered(session: &Session, waited: &[WaitedFor], start: f64, end: Option<f64>) -> String {
    let project = session.project();
    let (missing, others): (Vec<WaitedFor>, Vec<WaitedFor>) =
        waited.iter().cloned().partition(|waited| !waited.answered);
    let (missing, others) = (missing.as_slice(), others.as_slice());
    let tracks: Vec<TrackId> = missing.iter().filter_map(|waited| waited.track).collect();
    let mut said = format!(
        "Nothing was rendered: {} of the song's instruments never answered the notes {} to play, for as long as {} \
         waited for ({:.0} s). A mix without {} is not the song, so there are no numbers and no file.",
        missing.len(),
        if missing.len() == 1 { "it is" } else { "they are" },
        if missing.len() == 1 { "it was" } else { "they were" },
        missing.iter().map(|waited| waited.seconds).fold(0.0, f64::max),
        if missing.len() == 1 { "it" } else { "them" },
    );
    for note in silent_notes(project, &tracks, missing, start, end) {
        said.push_str("\n- ");
        said.push_str(&note);
    }
    // What else the wait found, which it finds once.
    for other in others {
        said.push_str("\nBesides: ");
        said.push_str(&waited_note(other, project));
    }
    let loading = session.loading();
    let reading = missing.iter().any(|waited| loading.contains(&waited.device));
    said.push_str(if reading {
        "\nA plugin of the song is still loading what it plays: ask again in a few seconds."
    } else {
        "\nIf a plugin had only just been made or given another sound, ask again: it may have been loading. \
         Otherwise mend the track as above, or mute it (update_track) to hear the song without it."
    });
    said
}

/// A render's report as a reply: the mix, every track in two numbers, the
/// sections, what the dynamics took off, and what to make of it. With
/// `detail`, the tracks in full.
fn report_json(
    report: &RenderReport,
    project: &Project,
    only: Option<&[TrackId]>,
    detail: bool,
    mut notes: Vec<String>,
    missing: &trill_runtime::Missing,
) -> Value {
    let db = |value: f32| round(f64::from(value));
    let mut out = serde_json::to_value(&report.stats).unwrap_or_else(|_| json!({}));
    if let Some(path) = &report.path {
        out["path"] = json!(path);
    }
    out["sample_rate"] = json!(report.sample_rate);
    out["threads"] = json!(report.threads);
    out["tracks"] = report
        .tracks
        .iter()
        // Asked to hear only some tracks, the others have nothing to say.
        .filter(|track| only.is_none_or(|only| only.contains(&track.track)))
        .map(|track| {
            let mut entry = json!({
                "track": track.track.0,
                "name": track.name,
                "peak_db": db(track.peak_db),
                "loudness_lufs": db(track.loudness_lufs),
            });
            if detail {
                entry["rms_db"] = json!(db(track.rms_db));
                entry["stereo_correlation"] = json!(db(track.stereo_correlation));
                entry["spectrum"] = serde_json::to_value(&track.spectrum).unwrap_or_default();
                if !track.section_loudness.is_empty() {
                    entry["section_loudness"] = track.section_loudness.iter().map(|lufs| db(*lufs)).collect();
                }
            }
            entry
        })
        .collect();
    if !report.sections.is_empty() {
        out["sections"] = report
            .sections
            .iter()
            .map(|level| {
                json!({
                    "name": level.section.name,
                    "start": round(level.section.start_beat),
                    "end": round(level.section.end_beat),
                    "loudness_lufs": db(level.loudness_lufs),
                    "peak_db": db(level.peak_db),
                })
            })
            .collect();
    }
    if !report.dynamics.is_empty() {
        out["gain_reduction"] = report
            .dynamics
            .iter()
            .map(|dynamics| {
                let on = dynamics.track.and_then(|track| project.track(track).ok()).map(|track| track.name.clone());
                json!({
                    "device": dynamics.device.0,
                    "name": dynamics.name,
                    "on": on.unwrap_or_else(|| "master".to_string()),
                    "most_db": db(dynamics.most_db),
                    "typical_db": db(dynamics.typical_db),
                    "active": round(f64::from(dynamics.active)),
                })
            })
            .collect();
    }
    if !report.stems.is_empty() {
        out["stems"] = report
            .stems
            .iter()
            .map(|stem| {
                let mut entry = json!({ "file": stem.path, "what": stem.what });
                if let Some(track) = stem.track {
                    entry["track"] = json!(track.0);
                    entry["name"] = json!(stem.name);
                }
                entry["peak_db"] = json!(db(stem.stats.peak_db));
                entry["loudness_lufs"] = json!(db(stem.stats.loudness_lufs));
                if let Some(true_peak) = stem.stats.true_peak_db {
                    entry["true_peak_db"] = json!(db(true_peak));
                }
                if detail {
                    entry["rms_db"] = json!(db(stem.stats.rms_db));
                    entry["stereo_correlation"] = json!(db(stem.stats.stereo_correlation));
                    entry["spectrum"] = serde_json::to_value(&stem.stats.spectrum).unwrap_or_default();
                }
                entry
            })
            .collect();
    }
    // Files that are not on disk are said once and first: the lines for
    // each clip and pad that names one give way to that.
    out["problems"] = json!(crate::files::without_missing(report.problems.clone(), missing));
    let mut all: Vec<String> = crate::files::heard_without(missing).into_iter().collect();
    if let Some(brief) = crate::files::brief(missing) {
        out["missing_files"] = brief;
    }
    all.append(&mut observations(report, project));
    all.extend(alone_note(&report.alone));
    all.extend(draws_notes(&report.draws));
    all.extend(report.not_begun.iter().map(not_begun_note));
    all.append(&mut notes);
    out["notes"] = json!(all);
    out
}

// ---- probe -------------------------------------------------------------------------

/// The pitches a list of pitches and runs of pitches stands for, each once,
/// in the order given.
fn parse_pitches(items: &[PitchesInput], step: u8) -> Result<Vec<u8>, String> {
    let step = usize::from(step.max(1));
    let mut pitches: Vec<u8> = Vec::new();
    let mut add = |pitch: u8| {
        if !pitches.contains(&pitch) {
            pitches.push(pitch);
        }
    };
    for PitchesInput(value) in items {
        if let Ok(pitch) = parse_pitch_value(value) {
            add(pitch);
            continue;
        }
        let Value::String(text) = value else { return Err(parse_pitch_value(value).unwrap_err()) };
        // A run: the two ends with "-" (or "..") between them. A note name
        // may hold a "-" of its own ("C-1"), so every place is tried.
        let one = |text: &str| {
            parse_pitch_value(&Value::String(text.trim().to_string()))
                .ok()
                .or_else(|| text.trim().parse::<u8>().ok().filter(|pitch| *pitch <= 127))
        };
        let splits =
            text.match_indices("..").map(|(at, _)| (at, 2)).chain(text.match_indices('-').map(|(at, _)| (at, 1)));
        let ends = splits
            .filter(|(at, _)| *at > 0)
            .find_map(|(at, width)| Some((one(&text[..at])?, one(&text[at + width..])?)));
        let Some((first, last)) = ends else {
            return Err(format!(
                "'{text}' is not a pitch (60, \"C4\", \"snare\") or a run of pitches (\"E1-G4\", \"36-60\")"
            ));
        };
        let (low, high) = (first.min(last), first.max(last));
        (low..=high).step_by(step).for_each(&mut add);
    }
    Ok(pitches)
}

/// A pitch as a reply names it: `E1 (28)`.
fn named(pitch: u8) -> String {
    format!("{} ({pitch})", pitch_name(pitch))
}

/// Pitches as the runs they form: `E1-G4 (28-67), C5 (72)`.
fn runs(pitches: &[u8]) -> String {
    let mut sorted = pitches.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut index = 0;
    while index < sorted.len() {
        let first = sorted[index];
        let mut last = first;
        while index + 1 < sorted.len() && sorted[index + 1] == last + 1 {
            index += 1;
            last = sorted[index];
        }
        parts.push(if first == last {
            named(first)
        } else {
            format!("{}-{} ({first}-{last})", pitch_name(first), pitch_name(last))
        });
        index += 1;
    }
    parts.join(", ")
}

/// How far what was heard is from what was written, in semitones.
fn offset(heard: &Heard) -> Option<f64> {
    heard.pitch.map(|pitch| pitch - f64::from(heard.note.pitch))
}

fn ratio(a: f64, b: f64) -> f64 {
    let (low, high) = (a.min(b).max(1e-9), a.max(b));
    high / low
}

/// Whether two notes that were played one after the other measure alike:
/// enough so to be reported as one.
fn alike(a: &Heard, b: &Heard) -> bool {
    if a.sounded != b.sounded || a.note.velocity != b.note.velocity || a.note.with != b.note.with {
        return false;
    }
    if !a.sounded {
        return true;
    }
    let pitched = match (offset(a), offset(b)) {
        (Some(a), Some(b)) if (a - b).abs() <= 0.5 => true,
        (None, None) => false,
        _ => return false,
    };
    // The notes of one instrument, a semitone apart, are one run though
    // they are not the same: each a little louder, longer or brighter
    // than the last. Hits on neighbouring keys are one run only if they
    // are the same kind of hit.
    let (level, longer, brighter) = if pitched { (9.0, 3.0, 3.0) } else { (6.0, 1.6, 1.3) };
    if (a.peak_db - b.peak_db).abs() > level {
        return false;
    }
    // A note that died away a moment before the listening stopped and one
    // that was still going are as long as each other.
    let apart = (a.rings_seconds - b.rings_seconds).abs();
    if apart > 0.1 && (a.still_ringing != b.still_ringing || ratio(a.rings_seconds, b.rings_seconds) > longer) {
        return false;
    }
    match (a.brightness_hz, b.brightness_hz) {
        (Some(a), Some(b)) => ratio(a, b) <= brighter,
        (None, None) => true,
        _ => false,
    }
}

/// What a sound that is not a played pitch is like, roughly: enough to
/// tell a kick from a snare from a cymbal. How bright it is says where it
/// sits; how long it rings says whether it is a hit or a wash.
fn character(heard: &Heard) -> &'static str {
    let long = heard.still_ringing || heard.rings_seconds >= 1.0;
    let short = !long && heard.rings_seconds < 0.25;
    match heard.brightness_hz.unwrap_or(0.0) {
        hz if hz < 250.0 => match long {
            true => "a long low boom",
            false => "a low thump",
        },
        hz if hz < 1_200.0 => match long {
            true => "a long low-mid ring",
            false => "a low-mid hit",
        },
        hz if hz < 5_000.0 => match long {
            true => "a long noisy mid ring",
            false => "a noisy mid hit",
        },
        _ => match (long, short) {
            (true, _) => "a long bright wash",
            (_, true) => "a short bright tick",
            _ => "a bright hit",
        },
    }
}

/// A value, or the lowest and the highest of several when they differ.
fn spread(values: impl Iterator<Item = f64>, digits: i32) -> Value {
    let scale = 10f64.powi(digits);
    let values: Vec<f64> = values.map(|value| (value * scale).round() / scale).collect();
    let (low, high) =
        values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| (low.min(*v), high.max(*v)));
    if values.is_empty() {
        Value::Null
    } else if low == high {
        json!(low)
    } else {
        json!([low, high])
    }
}

/// One entry of a probe's reply: a note, or a run of notes that measure
/// alike.
fn heard_json(run: &[&Heard], velocities: bool, tonal: Option<f64>, kit: Option<&InstrumentProfile>) -> Value {
    let first = run[0];
    let pitches: Vec<u8> = run.iter().map(|heard| heard.note.pitch).collect();
    let mut entry = Map::new();
    entry.insert("notes".into(), json!(runs(&pitches)));
    if velocities {
        entry.insert("velocity".into(), json!(first.note.velocity));
    }
    // What a note is called on a drum kit, where the instrument is one: by the kit's own map
    // where the track has one, which may keep a drum on another key than General MIDI does.
    let named = match kit {
        Some(kit) => Some(kit.drum_name(first.note.pitch)).filter(|name| *name != pitch_name(first.note.pitch)),
        None => drum_name(first.note.pitch).map(str::to_owned),
    };
    if let (1, None, Some(name)) = (run.len(), tonal, named) {
        entry.insert("drum_name".into(), json!(name));
    }
    if !first.sounded {
        entry.insert("silent".into(), json!(true));
        return Value::Object(entry);
    }
    entry.insert("level_db".into(), spread(run.iter().map(|heard| f64::from(heard.peak_db)), 1));
    let offsets: Vec<f64> = run.iter().filter_map(|heard| offset(heard)).collect();
    // Where the instrument's pitch follows its keys (`tonal`), a note that
    // does so too is a played note, and how far it sounds from what is
    // written is all there is to say. Anything else is a sound of its own:
    // what it is like, and its pitch if it has one.
    let follows = tonal.is_some_and(|common| {
        offsets.len() == run.len() && offsets.iter().all(|offset| (offset - common).abs() <= 0.5)
    });
    let name = |pitch: f64| pitch_name(pitch.round().clamp(0.0, 127.0) as u8);
    if follows {
        let semitones = offsets.iter().sum::<f64>() / offsets.len() as f64;
        // To the semitone, unless the instrument is tuned away from it.
        let whole = semitones.round();
        let shown = if (semitones - whole).abs() <= 0.3 { whole } else { (semitones * 10.0).round() / 10.0 };
        entry.insert("offset_semitones".into(), json!(shown));
        if let (1, Some(pitch)) = (run.len(), first.pitch) {
            entry.insert("heard".into(), json!(name(pitch)));
        }
    } else {
        entry.insert("like".into(), json!(character(first)));
        if let (1, Some(pitch)) = (run.len(), first.pitch) {
            entry.insert("heard".into(), json!(name(pitch)));
        }
    }
    // A sound that begins with its key is not worth a line; one that
    // takes its time is.
    let latest = run.iter().filter_map(|heard| heard.onset_seconds).fold(0.0, f64::max);
    if latest >= LATE_ONSET_SECONDS {
        entry.insert(
            "onset_ms".into(),
            spread(run.iter().filter_map(|heard| heard.onset_seconds).map(|seconds| seconds * 1000.0), 0),
        );
    }
    // How long it rang, or the least it rings if the listening stopped
    // first: ">0.95".
    let longest = run.iter().map(|heard| heard.rings_seconds).fold(0.0, f64::max);
    if run.iter().any(|heard| heard.still_ringing) {
        entry.insert("rings_s".into(), json!(format!(">{:.2}", longest)));
    } else {
        entry.insert("rings_s".into(), spread(run.iter().map(|heard| heard.rings_seconds), 2));
    }
    entry.insert("bright_hz".into(), spread(run.iter().filter_map(|heard| heard.brightness_hz), 0));
    let width = run.iter().map(|heard| f64::from(heard.width)).fold(0.0, f64::max);
    if width >= 0.02 {
        entry.insert("width".into(), json!((width * 100.0).round() / 100.0));
    }
    Value::Object(entry)
}

/// The notes a probe played, as runs of notes that measure alike (or one
/// by one, with `detail`).
fn heard_list(heard: &[Heard], detail: bool, tonal: Option<f64>) -> Vec<Value> {
    heard_list_on(heard, detail, tonal, None)
}

/// [`heard_list`] for an instrument something is known about: drums are
/// called what `kit` calls them.
fn heard_list_on(heard: &[Heard], detail: bool, tonal: Option<f64>, kit: Option<&InstrumentProfile>) -> Vec<Value> {
    let velocities = heard.iter().any(|note| note.note.velocity != heard[0].note.velocity);
    let mut groups: Vec<Vec<&Heard>> = Vec::new();
    for note in heard {
        let joins = groups
            .last()
            .and_then(|run| run.last())
            .is_some_and(|last| !detail && alike(last, note) && note.note.pitch == last.note.pitch + 1);
        match groups.last_mut() {
            Some(run) if joins => run.push(note),
            _ => groups.push(vec![note]),
        }
    }
    // A drum plugin has the same hit on several keys, far apart. Where the
    // pitch does not follow the keys, keys that sound the same are listed
    // together.
    if !detail && tonal.is_none() {
        let mut merged: Vec<Vec<&Heard>> = Vec::new();
        for run in groups {
            let twin = merged.iter_mut().find(|other| run.iter().all(|note| same_sound(other[0], note)));
            match twin {
                Some(other) => other.extend(run),
                None => merged.push(run),
            }
        }
        groups = merged;
    }
    groups.iter().map(|run| heard_json(run, velocities, tonal, kit)).collect()
}

/// Whether two notes are one and the same sound: what a sampler plays
/// when two keys trigger the same recording.
fn same_sound(a: &Heard, b: &Heard) -> bool {
    let near = |a: Option<f64>, b: Option<f64>, by: f64| match (a, b) {
        (Some(a), Some(b)) => ratio(a, b) <= by,
        (None, None) => true,
        _ => false,
    };
    a.sounded
        && b.sounded
        && a.note.velocity == b.note.velocity
        && a.note.with == b.note.with
        && (a.peak_db - b.peak_db).abs() <= 0.3
        && a.still_ringing == b.still_ringing
        && (a.rings_seconds - b.rings_seconds).abs() <= 0.02
        && near(a.brightness_hz, b.brightness_hz, 1.02)
        && (a.width - b.width).abs() <= 0.02
}

/// How far from what is written the instrument sounds, in semitones, if
/// its pitch follows its keys: at least half of the notes that sounded
/// are one and the same distance from their keys. `None` for a drum kit,
/// whose hits have pitches of their own or none. A sampled instrument has
/// noises on some keys beside its notes, which is why half is enough.
fn common_offset(heard: &[Heard]) -> Option<f64> {
    let offsets: Vec<f64> = heard.iter().filter(|heard| heard.sounded).filter_map(offset).collect();
    let sounding = heard.iter().filter(|heard| heard.sounded).count();
    let sharing = |semitones: f64| offsets.iter().filter(|offset| (*offset - semitones).abs() <= 0.5).count();
    let most = offsets.iter().map(|offset| offset.round()).max_by_key(|semitones| sharing(*semitones))?;
    (sharing(most) * 2 >= sounding).then_some(most)
}

/// The notes whose pitch follows their keys, `semitones` from what is
/// written: the ones that are played as notes.
fn following(heard: &[Heard], semitones: f64) -> Vec<u8> {
    let follows =
        |heard: &&Heard| heard.sounded && offset(heard).is_some_and(|offset| (offset - semitones).abs() <= 0.5);
    heard.iter().filter(follows).map(|heard| heard.note.pitch).collect()
}

/// What the notes that sounded say about the instrument's pitch, when
/// enough of them agree to say it.
fn pitch_summary(heard: &[Heard]) -> Option<String> {
    let semitones = common_offset(heard)?;
    if following(heard, semitones).len() < 3 {
        return None;
    }
    Some(match semitones as i32 {
        0 => "sounds as written".to_string(),
        12 => "sounds an octave above what is written (+12 semitones): write it an octave lower".to_string(),
        -12 => "sounds an octave below what is written (-12 semitones): write it an octave higher".to_string(),
        24 => "sounds two octaves above what is written (+24 semitones)".to_string(),
        -24 => "sounds two octaves below what is written (-24 semitones)".to_string(),
        other => format!("sounds {other:+} semitones from what is written"),
    })
}

/// What holding a key changed about a note, in words; empty for nothing.
fn changes(with: &Heard, plain: &Heard) -> Vec<String> {
    let mut found = Vec::new();
    match (with.sounded, plain.sounded) {
        (false, true) => return vec!["silences the note".to_string()],
        (true, false) => return vec!["makes the note sound".to_string()],
        (false, false) => return found,
        (true, true) => {}
    }
    if ratio(with.rings_seconds, plain.rings_seconds) >= 1.4 && (with.rings_seconds - plain.rings_seconds).abs() >= 0.08
    {
        let word = if with.rings_seconds < plain.rings_seconds { "shorter" } else { "longer" };
        found.push(format!("{word} ({:.2} s, was {:.2})", with.rings_seconds, plain.rings_seconds));
    }
    if let (Some(now), Some(before)) = (with.brightness_hz, plain.brightness_hz)
        && ratio(now, before) >= 1.25
    {
        let word = if now < before { "darker" } else { "brighter" };
        found.push(format!("{word} ({now:.0} Hz, was {before:.0})"));
    }
    if (with.peak_db - plain.peak_db).abs() >= 3.0 {
        let word = if with.peak_db < plain.peak_db { "quieter" } else { "louder" };
        found.push(format!("{word} by {:.0} dB", (with.peak_db - plain.peak_db).abs()));
    }
    match (with.pitch, plain.pitch) {
        (Some(now), Some(before)) if (now - before).abs() >= 0.5 => {
            found.push(format!("sounds {:+.0} semitones from where it did", now - before));
        }
        (None, Some(_)) => found.push("has no clear pitch any more".to_string()),
        _ => {}
    }
    if let (Some(now), Some(before)) = (with.onset_seconds, plain.onset_seconds)
        && (now - before).abs() >= 0.03
    {
        let word = if now > before { "later" } else { "sooner" };
        found.push(format!("starts {:.0} ms {word}", (now - before).abs() * 1000.0));
    }
    found
}

/// What a probe is asked to play, worked out from the request.
struct Plan {
    target: ProbeTarget,
    /// The notes to play, and how.
    pitches: Vec<u8>,
    velocities: Vec<u8>,
    /// The keys to try as keyswitches.
    switches: Vec<u8>,
    options: ProbeOptions,
    detail: bool,
    /// What had to be left out or cut short to fit one call.
    remarks: Vec<String>,
}

impl Plan {
    fn of(request: ProbeRequest, sample_rate: u32) -> Result<Self, String> {
        let target = match (request.track, request.device) {
            (Some(track), None) => ProbeTarget::Track { track: TrackId(track), effects: request.effects },
            (None, Some(uri)) => ProbeTarget::Device { uri, preset: request.preset },
            _ => {
                return Err("give `track` (an instrument in the song, as it is now) or `device` (an instrument's \
                            URI, with an optional `preset`), one of the two"
                    .to_string());
            }
        };
        let step = request.step.unwrap_or(1);
        let scanning = request.notes.is_none();
        let switches = match &request.keyswitches {
            Some(keys) => parse_pitches(keys, 1)?,
            None => Vec::new(),
        };
        if !switches.is_empty() && scanning {
            return Err("`keyswitches` needs `notes`: one or two notes the instrument plays, to hear what each key \
                        changes about them. A probe without `notes` finds which notes it plays."
                .to_string());
        }
        let mut pitches = match &request.notes {
            Some(notes) => parse_pitches(notes, step)?,
            None => (0..=127u8).step_by(usize::from(step.max(1))).collect(),
        };
        if pitches.is_empty() {
            return Err("`notes` is empty: give pitches or runs of pitches, or leave it out to play the whole \
                        keyboard"
                .to_string());
        }
        let velocities = request.velocities.unwrap_or_else(|| vec![100]);
        if velocities.is_empty() || velocities.iter().any(|velocity| !(1..=127).contains(velocity)) {
            return Err("`velocities` are 1 to 127, at least one".to_string());
        }
        let held = match &request.hold {
            Some(keys) => keys.iter().map(|PitchInput(value)| parse_pitch_value(value)).collect::<Result<_, _>>()?,
            None => Vec::new(),
        };
        let mut controllers = Vec::new();
        for (name, value) in request.controllers.unwrap_or_default() {
            let kind = parse_controller(&name)?;
            let scale = controller_scale(kind);
            let (low, high) = if kind == ControllerKind::PitchBend { (-1.0, 1.0) } else { (0.0, 127.0) };
            if !(low..=high).contains(&value) {
                return Err(format!("{name} is {value}: it runs from {low} to {high}"));
            }
            controllers.push((kind, (value / scale) as f32));
        }

        // What fits one call.
        let mut remarks = Vec::new();
        let per_pitch = if switches.is_empty() { velocities.len() } else { 1 + 2 * switches.len() };
        let fits = (MAX_PROBE_NOTES / per_pitch).max(1);
        if pitches.len() > fits {
            let rest = pitches.split_off(fits);
            remarks.push(format!(
                "That is more than {MAX_PROBE_NOTES} notes in one call: played {}, left out {}. Ask for those in \
                 another call.",
                runs(&pitches),
                runs(&rest)
            ));
        }
        let count = (pitches.len() * per_pitch) as f64;
        let (hold, tail) = match (scanning, switches.is_empty()) {
            (true, _) => SCAN_TIMES,
            (false, true) => NOTE_TIMES,
            (false, false) => SWITCH_TIMES,
        };
        let length = request.length.unwrap_or(hold);
        if !(0.02..=30.0).contains(&length) {
            return Err(format!("`length` is {length} s: a note is held for between 0.02 and 30 seconds"));
        }
        let mut tail = request.tail.unwrap_or(tail).clamp(0.05, 30.0);
        if count * (length + tail) > MAX_PROBE_SECONDS {
            let fitting = (MAX_PROBE_SECONDS / count - length).max(0.1);
            if count * (length + fitting) > MAX_PROBE_SECONDS * 1.5 {
                return Err(format!(
                    "{count} notes of {length} s each are more than a probe plays in one call ({MAX_PROBE_SECONDS} \
                     s of sound). Ask for fewer notes, or a shorter `length`."
                ));
            }
            remarks.push(format!(
                "To fit {count} notes into one call each was listened to for at most {fitting:.1} s after it was \
                 let go (not {tail:.1}): a note that rings longer is marked still_ringing. Ask for fewer notes to \
                 hear them out."
            ));
            tail = fitting;
        }
        let options = ProbeOptions {
            sample_rate,
            hold_seconds: length,
            tail_seconds: tail,
            held,
            controllers,
            ..ProbeOptions::default()
        };
        Ok(Self { target, pitches, velocities, switches, options, detail: request.detail, remarks })
    }

    /// The notes of a plain probe: every pitch at every velocity.
    fn notes(&self) -> Vec<ProbeNote> {
        let pitches = |velocity: u8| {
            self.pitches.iter().map(move |pitch| ProbeNote { pitch: *pitch, velocity, with: Vec::new() })
        };
        self.velocities.iter().flat_map(|velocity| pitches(*velocity)).collect()
    }

    /// The notes of a keyswitch probe: the notes plain, then for every
    /// key the notes with it held and the notes plain again.
    fn switch_notes(&self) -> Vec<ProbeNote> {
        let velocity = self.velocities[0];
        let plain = || self.pitches.iter().map(move |pitch| ProbeNote { pitch: *pitch, velocity, with: Vec::new() });
        let mut notes: Vec<ProbeNote> = plain().collect();
        for key in &self.switches {
            notes.extend(self.pitches.iter().map(|pitch| ProbeNote { pitch: *pitch, velocity, with: vec![*key] }));
            notes.extend(plain());
        }
        notes
    }
}

/// What a keyswitch probe found, key by key.
fn switches_json(plan: &Plan, heard: &[Heard]) -> Value {
    let per = plan.pitches.len();
    let baseline = &heard[..per];
    // Under a keyswitch a note is a played note: its pitch is given as it
    // is heard, against what was written.
    let tonal = baseline.iter().find_map(offset).map(f64::round);
    let mut list = vec![json!({ "keys": "none", "heard": heard_list(baseline, true, tonal) })];
    let (mut nothing, mut themselves): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let mut state = baseline;
    for (index, key) in plan.switches.iter().enumerate() {
        let with = &heard[per * (1 + 2 * index)..per * (2 + 2 * index)];
        let after = &heard[per * (2 + 2 * index)..per * (3 + 2 * index)];
        // A key that makes a sound of its own is a note, not a switch,
        // unless it changes the notes as well.
        let sounds = with.iter().any(|note| note.with_sounded);
        // Against what the notes were just before the key went down.
        let did: Vec<Vec<String>> = with.iter().zip(state).map(|(with, before)| changes(with, before)).collect();
        if did.iter().all(Vec::is_empty) {
            if sounds { &mut themselves } else { &mut nothing }.push(*key);
            state = after;
            continue;
        }
        // Described against the notes as they were at first; a key that
        // undoes the one before it brings them back to that.
        let against: Vec<String> = with.iter().zip(baseline).flat_map(|(with, plain)| changes(with, plain)).collect();
        let mut entry = json!({ "keys": named(*key) });
        let mut said: Vec<String> = Vec::new();
        for change in &against {
            if !said.contains(change) {
                said.push(change.clone());
            }
        }
        entry["changes"] =
            if said.is_empty() { json!("back to the sound without a key") } else { json!(said.join(", ")) };
        // Whether it goes on after the key has come up.
        let stays = after.iter().zip(with).all(|(after, with)| changes(after, with).is_empty());
        let moved = after.iter().zip(state).any(|(after, before)| !changes(after, before).is_empty());
        entry["latches"] = json!(stays && moved);
        if sounds {
            entry["sounds_itself"] = json!(true);
        }
        entry["heard"] = json!(heard_list(with, true, tonal));
        list.push(entry);
        state = after;
    }
    if !nothing.is_empty() {
        list.push(json!({ "keys": runs(&nothing), "changes": "nothing" }));
    }
    if !themselves.is_empty() {
        list.push(json!({ "keys": runs(&themselves), "changes": "nothing: they are notes themselves, and sound" }));
    }
    json!(list)
}

#[tool_router(router = hearing_router, vis = "pub(crate)")]
impl TrillServer {
    /// Play notes through one instrument and measure each: whether it sounds, how loud, the pitch that is
    /// actually heard, how soon it starts, how long it rings, how bright it is. Nothing in the song changes, no
    /// file is written, nothing is heard. Use it before writing for a plugin you do not know. With no `notes`
    /// it plays the whole keyboard: that gives an instrument's range and octave, or a drum plugin's map (which
    /// notes are a low thump, a noisy mid hit, a long bright wash). With `keyswitches` it tells which keys
    /// change the articulation.
    #[tool(annotations(title = "Probe an instrument", read_only_hint = true))]
    async fn probe(&self, Parameters(request): Parameters<ProbeRequest>) -> CallToolResult {
        // A preset that is a file is asked what it holds before the
        // session is, and a guarded client is kept to this machine's.
        if let Err(refusal) = self.ready_presets(request.device.as_deref(), &request.preset).await {
            return finish(Err(refusal));
        }
        let (job, plan, about, kit) = {
            let mut session = self.session.lock();
            let plan = match Plan::of(request, session.audio_info().sample_rate as u32) {
                Ok(plan) => plan,
                Err(problem) => return finish(Err(problem)),
            };
            let job = match session.probe_job(&plan.target) {
                Ok(job) => job,
                Err(error) => return finish(Err(crate::server::explain(session.project(), error))),
            };
            let mut about = json!({ "instrument": job.devices[0].name });
            about["sample_rate"] = json!(plan.options.sample_rate);
            let mut on_track = None;
            if let ProbeTarget::Track { track, .. } = &plan.target {
                about["device"] = json!(job.devices[0].id.0);
                about["track"] =
                    json!(session.project().track(*track).map(|track| track.name.clone()).unwrap_or_default());
                on_track = Some(*track);
            }
            // Whether this is already known, and where to keep what is found.
            about["known"] = json!(crate::instruments::probe_note(&session, &job.devices[0].uri, on_track));
            // What the keys are called: by the track, or by what is known of an instrument not in the song.
            let kit: Option<InstrumentProfile> = match on_track {
                // With the names of the kit that is loaded, where the instrument says which that is.
                Some(track) => Some(session.profile_of(track)),
                None => session.instruments().of(&job.devices[0].uri).map(|known| known.profile),
            };
            // The track the instrument listens to, where it does.
            let key = job.devices[0].sidechain.and_then(|track| session.project().track(track).ok());
            if let Some(key) = key {
                about["listens_to"] = json!(key.name);
            }
            about["through"] = match job.devices.len() {
                1 => json!("the instrument alone"),
                more => json!(format!("the instrument and the {} device(s) after it", more - 1)),
            };
            (job, plan, about, kit)
        };
        let outcome = tokio::task::spawn_blocking(move || {
            let notes = if plan.switches.is_empty() { plan.notes() } else { plan.switch_notes() };
            job.run(&notes, &plan.options).map(|probed| (probed, plan))
        })
        .await;
        let (probed, plan) = match outcome {
            Ok(Ok(done)) => done,
            Ok(Err(error)) => return finish(Err(error.to_string())),
            Err(error) => return finish(Err(format!("the probe did not finish: {error}"))),
        };

        let mut out = about;
        let mut notes = plan.remarks.clone();
        let heard = &probed.heard;
        out["played"] = json!(heard.len());
        if plan.switches.is_empty() {
            let sounding: Vec<u8> = heard.iter().filter(|note| note.sounded).map(|note| note.note.pitch).collect();
            let silent: Vec<u8> = heard
                .iter()
                .filter(|note| !note.sounded && !sounding.contains(&note.note.pitch))
                .map(|note| note.note.pitch)
                .collect();
            out["sounding"] = if sounding.is_empty() { json!("none") } else { json!(runs(&sounding)) };
            if !silent.is_empty() {
                out["silent"] = json!(runs(&silent));
            }
            let tonal = common_offset(heard);
            if let Some(pitch) = pitch_summary(heard) {
                out["pitch"] = json!(pitch);
                // The notes it plays as notes: its range. What else sounds
                // (the noises a sampled instrument keeps on other keys) is
                // in `sounding` and listed with what it is like.
                let range = following(heard, tonal.unwrap_or_default());
                if range.len() != sounding.len() {
                    out["range"] = json!(runs(&range));
                }
            }
            let sounded: Vec<Heard> = heard.iter().filter(|note| note.sounded).cloned().collect();
            let listed = if plan.detail { heard } else { &sounded };
            out["heard"] = json!(heard_list_on(listed, plan.detail, tonal, kit.as_ref()));
            // Keys under the range that make no sound are where instruments
            // keep their keyswitches.
            let lowest = tonal.and_then(|semitones| following(heard, semitones).into_iter().min());
            let below: Vec<u8> =
                silent.iter().copied().filter(|pitch| lowest.is_some_and(|low| *pitch < low)).collect();
            if let (Some(low), true, Some(_)) = (lowest, below.len() >= 2, tonal) {
                let from =
                    below.iter().copied().filter(|pitch| *pitch + 12 >= low).min().unwrap_or(low.saturating_sub(12));
                notes.push(format!(
                    "The keys below {} make no sound. Sampled instruments keep their keyswitches there: probe with \
                     keyswitches [\"{}-{}\"] and `notes` from the range to see which of them change the sound.",
                    pitch_name(low),
                    pitch_name(from),
                    pitch_name(low - 1)
                ));
            }
            if sounding.is_empty() {
                notes.push(if probed.answered {
                    "None of these notes sounded.".to_string()
                } else {
                    format!(
                        "Nothing sounded, and the instrument never answered while it was waited for ({:.0} s). It \
                         plays none of these notes as it is: it may have no sound loaded (get_device, or a \
                         preset), or wait for a controller ({{\"expression\": 100}} in `controllers`), or these \
                         notes are all outside its range.",
                        probed.waited_seconds
                    )
                });
            }
        } else {
            out["keyswitches"] = switches_json(&plan, heard);
            if heard[..plan.pitches.len()].iter().all(|note| !note.sounded) {
                notes.push(
                    "The notes did not sound without a key held, so there is little to compare: give `notes` the \
                     instrument plays (a probe without `notes` lists them)."
                        .to_string(),
                );
            }
        }
        if probed.waited_seconds >= 1.0 && probed.answered {
            notes.push(format!("Waited {:.1} s for the instrument to be ready.", probed.waited_seconds));
        }
        if probed.lent {
            notes.push(format!(
                "This instrument listens to the track '{}' and plays what it hears there. A probe plays it by \
                 itself, so it was given noise to listen to while each note was held: which notes sound is the \
                 instrument's, and how loud and how bright they are here is not what they will be in the song. \
                 `measure` with `tracks` hears it in the song.",
                out["listens_to"].as_str().unwrap_or_default()
            ));
        }
        let unloaded = |pitch: &u8| heard.iter().any(|note| note.note.pitch == *pitch && !note.sounded);
        if probed.answered && probed.waited_for_silent.iter().any(unloaded) {
            // Said once for an instrument as it stands: what was waited
            // out is remembered.
            notes.push(
                "Some of these keys made no sound for as long as the instrument was waited for. A key that plays \
                 nothing and a sample that is still loading sound the same: if a silent key should sound, probe it \
                 again in a few seconds."
                    .to_string(),
            );
        }
        if !notes.is_empty() {
            out["notes"] = json!(notes);
        }
        finish(Ok(out))
    }

    /// Render the song faster than real time and measure it, WITHOUT writing a file: loudness (LUFS), peak and
    /// true peak, clipping, stereo correlation, spectral balance, the peak and loudness of every track, the
    /// loudness of each section between the markers, and the gain reduction of every compressor and limiter.
    /// This is how you check a mix without hearing it; use it as often as you like. `tracks` hears only some
    /// tracks, `start`/`end` only a stretch.
    #[tool(annotations(title = "Measure the song", read_only_hint = true))]
    async fn measure(&self, Parameters(request): Parameters<MeasureRequest>) -> CallToolResult {
        self.hear(request, None).await
    }

    /// Render the song to a WAV file, and measure it as `measure` does. For the finished result, or when the
    /// user wants a file: to check your work use `measure`, which writes nothing. With `stems`, the same pass
    /// writes every track, group and return to a file of its own as well.
    #[tool(annotations(title = "Render to WAV"))]
    async fn render(&self, Parameters(request): Parameters<RenderRequest>) -> CallToolResult {
        self.hear(request.what, Some((request.path, request.stems.unwrap_or_default()))).await
    }
}

impl TrillServer {
    /// Renders and measures the song. With `file`, writes it too: where
    /// asked, or where renders go when no place is named, with the stems
    /// that were asked for beside it.
    async fn hear(&self, request: MeasureRequest, file: Option<(Option<String>, Vec<StemChoice>)>) -> CallToolResult {
        // Snapshot under the lock, render outside it: a long bounce must not
        // freeze the GUI or other tools.
        let (job, path, options, start, end, missing) = {
            let mut session = self.session.lock();
            let path = match &file {
                None => None,
                Some((requested, _)) => match self.write_path(&session, requested.as_deref(), "a render", &["wav"]) {
                    Ok(path) => Some(path.unwrap_or_else(|| default_render_path(&session))),
                    Err(refusal) => return finish(Err(refusal)),
                },
            };
            // The stems go beside the mix, which is somewhere the mix may be.
            let stems = match (&file, &path) {
                (Some((_, choices)), Some(path)) => stems_for(choices, path),
                _ => None,
            };
            let project = session.project();
            let defaults = RenderOptions::default();
            let start = request.start.unwrap_or(defaults.start_beat).max(0.0);
            let end = request.end.unwrap_or_else(|| project.length_beats());
            let sections = match sections_of(project, request.sections.as_deref(), start, end) {
                Ok(sections) => sections,
                Err(problem) => return finish(Err(problem)),
            };
            let only_tracks = match &request.tracks {
                None => None,
                Some(ids) => {
                    let found: Result<Vec<TrackId>, _> =
                        ids.iter().map(|id| project.track(TrackId(*id)).map(|track| track.id)).collect();
                    match found {
                        Ok(tracks) if !tracks.is_empty() => Some(tracks),
                        Ok(_) => {
                            return finish(Err("`tracks` is empty: name the tracks to hear, or leave it out \
                                                    for the whole song"
                                .to_string()));
                        }
                        Err(error) => return finish(Err(crate::server::explain(project, error))),
                    }
                }
            };
            let sample_rate = match request.sample_rate {
                None => session.audio_info().sample_rate as u32,
                Some(rate) if RENDER_RATES.contains(&rate) => rate,
                Some(rate) => {
                    return finish(Err(format!(
                        "sample_rate {rate} is not a rate to render at: give one from {} to {} Hz (44100 and \
                         48000 are the usual ones), or leave it out for the engine's own, {} Hz",
                        RENDER_RATES.start(),
                        RENDER_RATES.end(),
                        session.audio_info().sample_rate as u32
                    )));
                }
            };
            if request.threads == Some(0) {
                return finish(Err("threads 0 is no number of processors to render on: give 1 for a track at a \
                                   time, more to hold it to that many, or leave it out for all the machine has"
                    .to_string()));
            }
            let (start, end) = (start, request.end);
            let options = RenderOptions {
                threads: request.threads,
                repeatable: request.repeatable,
                sample_rate,
                start_beat: start,
                end_beat: request.end,
                max_tail_seconds: request.max_tail_seconds.unwrap_or(defaults.max_tail_seconds),
                only_tracks,
                sections,
                stems,
                ..defaults
            };
            // The files of the song that are not on disk, on the tracks
            // that are heard: said with what was measured.
            let missing = trill_runtime::files_missing_in(&session);
            let missing = match &options.only_tracks {
                Some(only) => missing.on_tracks(only, session.project()),
                None => missing,
            };
            (session.render_job(), path, options, start, end, missing)
        };
        let detail = request.detail;
        let outcome = tokio::task::spawn_blocking(move || {
            let report = match &path {
                Some(path) => job.render_to_file(path, &options)?,
                None => job.measure(&options)?,
            };
            // What is not heard on purpose is not missing.
            let heard: Vec<TrackLevel> = report
                .tracks
                .iter()
                .filter(|level| options.only_tracks.as_ref().is_none_or(|only| only.contains(&level.track)))
                .cloned()
                .collect();
            let project = match &options.only_tracks {
                Some(only) => {
                    let mut project = job.project.clone();
                    project.tracks.iter_mut().for_each(|track| {
                        track.solo = only.contains(&track.id);
                        track.mute &= !track.solo;
                    });
                    project
                }
                None => job.project.clone(),
            };
            let silent = silent_track_notes(&project, &heard, &report.waited, options.start_beat, options.end_beat);
            let only = options.only_tracks.as_deref();
            Ok::<_, RenderError>(report_json(&report, &job.project, only, detail, silent, &missing))
        })
        .await;
        finish(match outcome {
            Ok(Ok(reply)) => Reply::Ok(reply),
            Ok(Err(RenderError::NotAnswered(missing))) => Err(not_answered(&self.session.lock(), &missing, start, end)),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(format!("the render did not finish: {error}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::project::AutomationPoint;
    use trill_core::{ChainRef, Note};
    use trill_engine::analysis::Spectrum;

    #[test]
    fn a_key_the_track_knows_for_a_keyswitch_is_not_said_to_be_a_note_that_made_no_sound() {
        use trill_core::{Articulation, InstrumentProfile};
        let mut project = Project::default();
        let (guitar, plain) = (project.add_track("Guitar"), project.add_track("Plain"));
        let profile =
            InstrumentProfile { articulations: vec![Articulation::key("open", 24)], ..InstrumentProfile::default() };
        project.set_track_profile(guitar, profile).unwrap();
        let waited = |track: TrackId, name: &str, silent: &[u8]| WaitedFor {
            track: Some(track),
            track_name: name.to_string(),
            device: trill_core::DeviceId(7),
            device_name: "A guitar".to_string(),
            seconds: 6.5,
            answered: true,
            silent: silent.to_vec(),
        };
        // The key that chooses how the notes are played made no sound, as it is meant to.
        let said = waited_note(&waited(guitar, "Guitar", &[24]), &project);
        assert_eq!(said, "Waited 6.5 s for A guitar on 'Guitar' to be ready before the render started.");
        // A key the track knows nothing of is still said, and only that one.
        let said = waited_note(&waited(guitar, "Guitar", &[24, 30]), &project);
        assert!(said.contains("1 of the notes it plays made no sound the whole time: F#1 (30)."), "{said}");
        assert!(said.contains(&format!("`probe` with track {} and those notes", guitar.0)), "{said}");
        // And on a track that says nothing of its keys, both are.
        let said = waited_note(&waited(plain, "Plain", &[24, 30]), &project);
        assert!(said.contains("2 of the notes it plays made no sound the whole time: C1 (24), F#1 (30)."), "{said}");
    }

    fn level(track: TrackId, peak_db: f32) -> TrackLevel {
        TrackLevel {
            track,
            name: String::new(),
            peak_db,
            loudness_lufs: peak_db,
            rms_db: peak_db,
            stereo_correlation: 1.0,
            spectrum: Spectrum { sub_db: 0.0, bass_db: 0.0, mid_db: 0.0, presence_db: 0.0, air_db: 0.0 },
            section_loudness: Vec::new(),
        }
    }

    #[test]
    fn a_track_that_had_notes_and_made_no_sound_is_named_with_what_is_known_and_how_to_find_out() {
        let mut project = Project::new("t");
        let mut track = |name: &str, uri: &str, clip_at: Option<f64>, pitches: &[u8]| {
            let id = project.add_track(name);
            project.add_device(ChainRef::Track(id), uri, name, None).unwrap();
            if let Some(start) = clip_at {
                let clip = project.add_midi_clip(id, "c", start, 4.0).unwrap();
                let notes: Vec<Note> = pitches
                    .iter()
                    .map(|pitch| Note { pitch: *pitch, start: 0.0, length: 1.0, velocity: 100 })
                    .collect();
                project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
            }
            id
        };
        let guitar = track("Guitar", "vst3:ABCDEF", Some(0.0), &[40, 52]);
        let switches = track("Rhythm", "vst3:ABCDEF", Some(0.0), &[12, 14]);
        let kit = track("Kit", "builtin:drumrack", Some(0.0), &[36]);
        let keys = track("Keys", "builtin:polysynth", Some(0.0), &[60]);
        let later = track("Later", "vst3:ABCDEF", Some(64.0), &[60]);
        let empty = track("Empty", "vst3:ABCDEF", None, &[]);
        let muted = track("Muted", "vst3:ABCDEF", Some(0.0), &[60]);
        project.track_mut(muted).unwrap().mute = true;
        let levels = [
            level(guitar, -120.0),
            level(switches, -120.0),
            level(kit, -120.0),
            level(keys, -9.0),
            level(later, -120.0),
            level(empty, -120.0),
            level(muted, -120.0),
        ];

        let notes = silent_track_notes(&project, &levels, &[], 0.0, Some(16.0));
        assert_eq!(notes.len(), 3, "{notes:#?}");
        // A plugin: its notes, the range as the first thing to suspect,
        // and the probe as the way to know. Expression comes last, as one
        // more thing it may be, with how to tell.
        let guitar_note = &notes[0];
        assert!(guitar_note.starts_with("'Guitar' has clips here but made no sound."), "{guitar_note}");
        assert!(
            guitar_note.contains("run from E2 to E3") && guitar_note.contains("outside its range"),
            "{guitar_note}"
        );
        assert!(guitar_note.contains(&format!("probe with track {}", guitar.0)), "{guitar_note}");
        let (range, expression) = (guitar_note.find("range").unwrap(), guitar_note.find("expression").unwrap());
        assert!(
            range < guitar_note.find("probe").unwrap() && guitar_note.find("probe").unwrap() < expression,
            "{guitar_note}"
        );
        assert!(!guitar_note.contains("saxophone"), "{guitar_note}");
        // Clips that hold only very low notes: keyswitches, most likely.
        let switch_note = &notes[1];
        assert!(switch_note.contains("between C0 and D0") && switch_note.contains("keyswitches"), "{switch_note}");
        assert!(!switch_note.contains("expression"), "nothing points at expression: {switch_note}");
        // A built-in device: what can be wrong with those.
        assert!(notes[2].starts_with("'Kit' has clips here but made no sound."), "{}", notes[2]);
        assert!(!notes[2].contains("expression") && !notes[2].contains("probe"));

        // A plugin whose clips have an expression curve already and is
        // silent all the same is not told to write one.
        let clip = project.track(guitar).unwrap().clips[0].id;
        let curve = vec![AutomationPoint::new(0.0, 0.0)];
        project.clip_mut(clip).unwrap().set_controller(ControllerKind::EXPRESSION, curve).unwrap();
        let notes = silent_track_notes(&project, &levels, &[], 0.0, Some(16.0));
        assert!(notes[0].contains("have an expression curve") && notes[0].contains("get_controllers"), "{}", notes[0]);
        assert!(!notes[0].contains("shape_dynamics"), "{}", notes[0]);

        // One the render waited for in vain says so.
        let waited = [WaitedFor {
            track: Some(guitar),
            track_name: "Guitar".into(),
            device: trill_core::DeviceId(3),
            device_name: "Amp".into(),
            seconds: 20.0,
            answered: false,
            silent: Vec::new(),
        }];
        let notes = silent_track_notes(&project, &levels, &waited, 0.0, Some(16.0));
        assert!(notes[0].contains("never did") && !notes[1].contains("never did"), "{notes:#?}");

        // The whole song: the late clip counts now. A solo elsewhere, and
        // nothing that was silenced on purpose is news.
        let notes = silent_track_notes(&project, &levels, &[], 0.0, None);
        assert!(notes.iter().any(|note| note.starts_with("'Later'")), "{notes:#?}");
        project.track_mut(keys).unwrap().solo = true;
        assert!(silent_track_notes(&project, &levels, &[], 0.0, None).is_empty());
    }

    #[test]
    fn pitches_are_read_one_by_one_and_as_runs() {
        let read = |items: &[Value], step: u8| {
            let items: Vec<PitchesInput> = items.iter().cloned().map(PitchesInput).collect();
            parse_pitches(&items, step)
        };
        assert_eq!(read(&[json!(36), json!("E2"), json!("snare")], 1).unwrap(), [36, 40, 38]);
        assert_eq!(read(&[json!("E1-G1")], 1).unwrap(), [28, 29, 30, 31]);
        assert_eq!(read(&[json!("36-40"), json!("38-42")], 1).unwrap(), [36, 37, 38, 39, 40, 41, 42], "each once");
        assert_eq!(read(&[json!("0-12")], 4).unwrap(), [0, 4, 8, 12]);
        // The lowest octave's names have a minus of their own.
        assert_eq!(read(&[json!("C-1")], 1).unwrap(), [0]);
        assert_eq!(read(&[json!("C-1-D-1")], 1).unwrap(), [0, 1, 2]);
        assert_eq!(read(&[json!("B0..D1")], 1).unwrap(), [23, 24, 25, 26]);
        assert_eq!(read(&[json!("G4-E4")], 1).unwrap(), [64, 65, 66, 67], "either way round");
        let error = read(&[json!("loud")], 1).unwrap_err();
        assert!(error.contains("'loud' is not a pitch") && error.contains("\"E1-G4\""), "{error}");
        assert!(read(&[json!(128)], 1).is_err() && read(&[json!([1, 2])], 1).is_err());
        assert_eq!(runs(&[28, 29, 30, 40, 67, 66]), "E1-F#1 (28-30), E2 (40), F#4-G4 (66-67)");
    }

    fn heard(pitch: u8, sounding: Option<(f64, f64, f64)>) -> Heard {
        let note = ProbeNote { pitch, velocity: 100, with: Vec::new() };
        match sounding {
            None => Heard {
                note,
                sounded: false,
                with_sounded: false,
                peak_db: -120.0,
                pitch: None,
                onset_seconds: None,
                rings_seconds: 0.0,
                still_ringing: false,
                brightness_hz: None,
                width: 0.0,
            },
            Some((offset, rings, bright)) => Heard {
                note,
                sounded: true,
                with_sounded: false,
                peak_db: -12.0,
                pitch: offset.is_finite().then(|| f64::from(pitch) + offset),
                onset_seconds: Some(0.004),
                rings_seconds: rings,
                still_ringing: false,
                brightness_hz: Some(bright),
                width: 0.0,
            },
        }
    }

    #[test]
    fn notes_that_measure_alike_are_one_entry_and_the_octave_is_one_read() {
        // A bass that plays 28 to 31 an octave down, brighter as it goes up.
        let mut notes: Vec<Heard> = (26..28).map(|pitch| heard(pitch, None)).collect();
        notes.extend((28..32).map(|pitch| heard(pitch, Some((-12.0, 0.5, 80.0 + f64::from(pitch))))));
        notes.push(heard(32, None));
        let sounded: Vec<Heard> = notes.iter().filter(|note| note.sounded).cloned().collect();
        let list = heard_list(&sounded, false, common_offset(&notes));
        assert_eq!(list.len(), 1, "{list:#?}");
        assert_eq!(list[0]["notes"], "E1-G1 (28-31)");
        assert_eq!(list[0]["offset_semitones"], -12.0);
        assert_eq!(list[0]["bright_hz"], json!([108.0, 111.0]));
        assert_eq!(list[0]["rings_s"], 0.5);
        assert!(list[0].get("like").is_none() && list[0].get("heard").is_none());
        assert!(list[0].get("onset_ms").is_none(), "a note that begins with its key says nothing of it");
        assert_eq!(
            pitch_summary(&notes).unwrap(),
            "sounds an octave below what is written (-12 semitones): write it an octave higher"
        );
        // Asked for in detail, every note is its own entry, the silent too.
        let all = heard_list(&notes, true, common_offset(&notes));
        assert_eq!(all.len(), 7);
        assert_eq!((&all[0]["silent"], &all[2]["heard"]), (&json!(true), &json!("E0")));

        // Drums: a kick, two snares that are alike, a hat, a crash, and the
        // same crash again on a key an octave up.
        let nan = f64::NAN;
        let kit = [
            heard(36, Some((-3.2, 0.2, 90.0))),
            heard(37, Some((nan, 0.3, 2_400.0))),
            heard(38, Some((nan, 0.31, 2_500.0))),
            heard(42, Some((nan, 0.05, 9_000.0))),
            heard(49, Some((nan, 1.4, 8_000.0))),
            heard(61, Some((nan, 1.4, 8_000.0))),
        ];
        let list = heard_list(&kit, false, common_offset(&kit));
        let likes: Vec<&str> = list.iter().map(|entry| entry["like"].as_str().unwrap()).collect();
        assert_eq!(likes, ["a low thump", "a noisy mid hit", "a short bright tick", "a long bright wash"]);
        assert_eq!(list[1]["notes"], "C#2-D2 (37-38)");
        assert_eq!(list[0]["heard"], "A1", "a kick has a pitch, but not its key's");
        assert_eq!(list[3]["notes"], "C#3 (49), C#4 (61)", "one sound on two keys is one entry");
        assert!(list[0].get("offset_semitones").is_none(), "how far a kick is from its key means nothing");
        assert_eq!(pitch_summary(&kit), None, "nothing to say about the pitch of a drum kit");
    }

    #[test]
    fn what_a_held_key_changes_is_said_in_words() {
        let plain = heard(40, Some((0.0, 1.0, 600.0)));
        let muted = heard(40, Some((0.0, 0.2, 300.0)));
        assert_eq!(changes(&muted, &plain), ["shorter (0.20 s, was 1.00)", "darker (300 Hz, was 600)"]);
        assert!(changes(&plain, &plain).is_empty());
        assert_eq!(changes(&heard(40, None), &plain), ["silences the note"]);
        let harmonic = heard(40, Some((12.0, 1.0, 1_200.0)));
        assert!(changes(&harmonic, &plain).contains(&"sounds +12 semitones from where it did".to_string()));
    }

    #[test]
    fn sections_come_from_the_markers_or_from_what_is_asked() {
        let mut project = Project::new("song");
        for (beat, name) in [(0.0, "Intro"), (16.0, "Verse"), (48.0, "Chorus")] {
            project.add_marker(beat, name).unwrap();
        }
        let named = |sections: &[Section]| -> Vec<(String, f64, f64)> {
            sections.iter().map(|section| (section.name.clone(), section.start_beat, section.end_beat)).collect()
        };
        let all = sections_of(&project, None, 0.0, 80.0).unwrap();
        assert_eq!(
            named(&all),
            [("Intro".to_string(), 0.0, 16.0), ("Verse".to_string(), 16.0, 48.0), ("Chorus".to_string(), 48.0, 80.0)]
        );
        // Only what lies in the stretch that is rendered, cut to it.
        let part = sections_of(&project, None, 20.0, 40.0).unwrap();
        assert_eq!(named(&part), [("Verse".to_string(), 20.0, 40.0)]);
        // Asked for: in beats, or in bars counted from one.
        let asked = [
            SectionInput { name: Some("Drop".into()), start: Some(32.0), end: Some(40.0), bars: None },
            SectionInput { bars: Some(vec![9, 16]), ..SectionInput::default() },
        ];
        let sections = sections_of(&project, Some(&asked), 0.0, 80.0).unwrap();
        assert_eq!(named(&sections), [("Drop".to_string(), 32.0, 40.0), ("bars 9-16".to_string(), 32.0, 64.0)]);
        assert!(sections_of(&project, Some(&[]), 0.0, 80.0).unwrap().is_empty(), "none, when none are asked for");
        let bad = [SectionInput { start: Some(8.0), ..SectionInput::default() }];
        assert!(sections_of(&project, Some(&bad), 0.0, 80.0).unwrap_err().contains("`start` and `end`"));
        let backwards = [SectionInput { bars: Some(vec![4, 2]), ..SectionInput::default() }];
        assert!(sections_of(&project, Some(&backwards), 0.0, 80.0).is_err());
    }
}
