//! The MCP tools. Each one is a thin adapter from a request to calls on the
//! shared [`Runtime`]: its session, its sound library and the operations
//! that join them. No musical logic lives here that the GUI could not also
//! reach.

use crate::guard::Guard;
use crate::input::{
    ControllerPointInput, controller_scale, find_param, parse_controller, parse_controller_points, parse_notes,
    parse_param_value, parse_pitch_value, parse_points,
};
use crate::requests::*;
use crate::view::{
    PresetSearch, audio_device_json, audio_json, clip_json, controller_json, descriptor_json, descriptor_summary_json,
    device_json, lane_json, midi_input_json, param_state_json, presets_json, project_json, round, samples_json,
    scan_json, sound_json, take_json, track_json, transport_json, warp_name,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{BUILTIN_SCHEME, DeviceCategory, DeviceFactory, EditorOptions, EventKind, split_uri};
use trill_core::midi_file;
use trill_core::presets::PresetList;
use trill_core::project::AutomationPoint;
use trill_core::project::{AutomationTarget, MAX_TEMPO, MAX_VOLUME_DB, MIN_TEMPO, WarpMode};
use trill_core::{ChainRef, ClipId, ControllerKind, DeviceId, EditError, Project, TrackId, TrackKind};
use trill_engine::render::TrackLevel;
use trill_engine::{RenderOptions, RenderReport, Session, SessionError, SharedSession};
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

const INSTRUCTIONS: &str = "\
This server controls Trill, a digital audio workstation. You compose, arrange and mix a song by calling tools; the \
user hears and sees every change live.

Conventions
- Time is in beats (quarter notes), starting at 0. In 4/4, bar N starts at beat (N-1)*4.
- Pitch is a MIDI number (60 = C4 = middle C), a note name (\"C4\", \"F#3\", \"Bb2\") or, on drum kits, a drum \
name (\"kick\", \"snare\", \"closed_hat\", \"open_hat\", \"clap\", ...). Velocity is 1-127.
- Every track, clip and device has an integer id, returned when it is created and listed by get_project.
- Note positions inside a clip are relative to the clip start.
- A MIDI clip also holds controller curves beside its notes: expression, mod_wheel, pitch_bend, sustain, \
breath, pressure, volume, or any controller number. Their values are 0-127 (pitch_bend: -1 to 1) and their beats \
are relative to the clip start. They loop, trim, split and move with the clip.

A good workflow
1. get_project to see what exists; set_project for tempo and time signature.
2. create_track per part with an instrument and preset (list_devices shows instruments, effects and presets).
   A plugin (clap:, vst3:) may have thousands of presets: before programming one by hand, search them with \
describe_device `preset_query` (\"warm pad\") and give `preset` the name or id to create_track, add_device or \
update_device.
3. create_clip with notes. For repeating parts, write one or two bars and set loop_length, or duplicate_clip.
   Instruments modelled on breath or a bow (solo winds, brass, strings) are silent until they are given \
expression, as a saxophone is without air. After writing such a line, call shape_dynamics on its clip: it reads \
the phrases from the notes and writes an expression curve that rises into the high points, swells through long \
notes and breathes between phrases. set_controllers writes or adjusts a curve point by point (a pedal, a pitch \
bend, vibrato on the mod wheel). render names any track that had notes and made no sound.
4. Shape the sound: add_device for effects (reverb, delay, eq, compressor, pump...), update_device for \
parameters, set_automation for movement such as filter sweeps and fades, update_track for the mix.
5. render writes a WAV and reports loudness, peak, spectral balance and per-track levels. You cannot hear, so \
read those numbers and adjust.

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
preparing_clips); renders always wait for the real audio.

Playing and recording live
- update_track `armed` arms a track: a MIDI keyboard (midi_inputs lists them; every input is on unless switched off) then plays its instrument. With no track armed, a keyboard plays the track selected in the window.
- transport `record` records what is played on the armed tracks into clips, from the playhead, after a count-in; `stop` ends the take and lists its clips. Recording inside an existing MIDI clip adds to that clip. With the loop on, each pass is layered onto the last. One take is one undo step.
- play_notes plays notes live through an instrument, as a keyboard would: they are heard now and, during a recording, recorded. It is not how to write a part: create_clip is.
- audio shows the audio device, its buffer and latency, and changes them.

Musical guidance
- Vary velocity and use transform_notes to humanize; identical velocities sound mechanical.
- Keep bass and kick in the centre; use low_cut on the EQ for everything else; leave headroom on the master.
- Give sections contrast: add and remove parts, automate filters and volume, change the drum pattern.
- Rearranging: edit_time removes, repeats or opens up a stretch of time on every track at once (drop a \
section, double a chorus, make room for a bridge).
- Every edit can be reversed with history (undo/redo).";

pub(crate) type Reply = Result<Value, String>;

pub(crate) fn finish(reply: Reply) -> CallToolResult {
    match reply {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
    }
}

/// Turns an error into a message that says what to do next: a missing id
/// comes back with the ids that do exist.
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

/// What a level at or below this is called: nothing at all.
const SILENT_DB: f32 = -119.0;

/// The tracks that had something to play in the rendered stretch and made
/// no sound, each with what to try. A part that is silent in a full mix is
/// easy to miss in the numbers, and the usual cause with a plugin is not a
/// mistake in the song: instruments modelled on breath or a bow wait for an
/// expression controller, as a saxophone waits for air. The answer to that
/// is a curve in the clip.
fn silent_track_notes(project: &Project, levels: &[TrackLevel], start: f64, end: Option<f64>) -> Vec<String> {
    let soloing = project.tracks.iter().any(|track| track.solo);
    let mut notes = Vec::new();
    for level in levels.iter().filter(|level| level.peak_db <= SILENT_DB) {
        let Ok(track) = project.track(level.track) else { continue };
        // Silenced on purpose is not news.
        if track.mute || (soloing && !track.solo) || track.clips.iter().all(|clip| clip.muted) {
            continue;
        }
        let playing = |clip: &trill_core::Clip| clip.end() > start && end.is_none_or(|end| clip.start < end);
        if !track.clips.iter().any(|clip| !clip.muted && playing(clip)) {
            continue;
        }
        let plugin = track.devices.iter().find(|device| device.enabled && !device.uri.starts_with("builtin:"));
        // Whether the clips that played were given air already.
        let breathes = |clip: &trill_core::Clip| {
            [ControllerKind::EXPRESSION, ControllerKind::BREATH].iter().any(|kind| clip.controller(*kind).is_some())
        };
        let expressive = track.clips.iter().any(|clip| !clip.muted && playing(clip) && breathes(clip));
        notes.push(match plugin {
            Some(device) if expressive => format!(
                "'{}' has clips here but made no sound, though they have an expression curve. Its instrument is \
                 the plugin {} (device {}). Check that the curve is above zero where the notes are (get_controllers), \
                 that the notes are in the instrument's range, and that it has a sound loaded (get_device); or try \
                 one of its presets.",
                track.name, device.name, device.id.0
            ),
            Some(device) => format!(
                "'{}' has clips here but made no sound. Its instrument is the plugin {} (device {}). Instruments \
                 modelled on breath or a bow are silent until they are given expression: call shape_dynamics on \
                 each of its clips (it writes an expression curve from the notes), or write one with \
                 set_controllers, such as {{\"expression\": [[0, 90]]}}. If that is not it, try a preset, or check \
                 the notes are in the instrument's range.",
                track.name, device.name, device.id.0
            ),
            None => format!(
                "'{}' has clips here but made no sound. Check that it has an instrument, that the instrument has a \
                 sound loaded (get_device), and that its volume and its group are up.",
                track.name
            ),
        });
    }
    notes
}

/// The render report plus plain-language observations about it.
fn render_reply(report: &RenderReport, silent_tracks: Vec<String>) -> Value {
    let stats = &report.stats;
    let mut notes: Vec<String> = Vec::new();
    let mut note = |text: &str| notes.push(text.to_owned());
    if stats.peak_db <= SILENT_DB {
        note("The render is silent. Check that tracks have instruments, clips have notes, and nothing is muted.");
    } else {
        if stats.clipped_samples > 0 {
            note("The output clips. Lower track volumes or keep a limiter last on the master.");
        }
        if stats.loudness_lufs < -20.0 {
            note("Quiet master (streaming targets are around -14 LUFS). Raise track levels or the limiter gain.");
        } else if stats.loudness_lufs > -8.0 {
            note("Very loud master; it may sound crushed. Consider lowering the limiter gain.");
        }
        if stats.stereo_correlation < 0.0 {
            note("Negative stereo correlation: parts may cancel in mono. Reduce width or chorus depth.");
        }
        if stats.spectrum.sub_db > -3.0 {
            note("Most of the energy is below 60 Hz. Thin out the sub bass or kick.");
        }
        if stats.spectrum.presence_db < -30.0 && stats.spectrum.air_db < -30.0 {
            note("Very little energy above 2 kHz; the mix may sound dull. Open filters or add brighter parts.");
        }
    }
    notes.extend(silent_tracks);
    let mut out = serde_json::to_value(report).unwrap_or_else(|_| json!({}));
    out["notes"] = json!(notes);
    out
}

#[derive(Clone)]
pub struct TrillServer {
    runtime: Arc<Runtime>,
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
        Self { session, runtime, tool_router: Self::tool_router() + Self::music_router(), guard: None }
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
    fn write_path(
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

    /// Sees to it that the presets of `uri` have been read, as far as
    /// that can be done in [`PRESET_WAIT`], before a tool that was given
    /// a preset looks for it.
    async fn ready_presets(&self, uri: Option<&str>, preset: &Option<String>) {
        if let (Some(uri), Some(_)) = (uri, preset) {
            self.presets_of(uri).await;
        }
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
        Some("any" | "all") => None,
        Some(other) => {
            return Err(format!("kind '{other}' should be \"audio\", \"midi\", \"project\", \"preset\" or \"any\""));
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
        let session = self.session.lock();
        let mut out = project_json(&session, request.include_notes);
        if lost > 0 {
            out["recoverable"] = json!(format!(
                "{lost} project(s) were not saved before the program last stopped. recover_project lists them."
            ));
        }
        finish(Ok(out))
    }

    /// Change song-wide settings: name, tempo, time signature, loop region, master volume.
    #[tool(annotations(title = "Set project settings"))]
    fn set_project(&self, Parameters(request): Parameters<SetProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let signature = request.time_signature.as_deref().map(parse_time_signature).transpose()?;
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
                Ok(())
            })?;
            let project = session.project();
            Ok(json!({
                "name": project.name,
                "tempo": project.tempo,
                "time_signature": format!("{}/{}", project.time_signature.numerator, project.time_signature.denominator),
                "beats_per_bar": project.time_signature.beats_per_bar(),
                "loop": { "enabled": project.loop_region.enabled, "start": project.loop_region.start, "end": project.loop_region.end },
                "master_volume_db": project.master.volume_db,
            }))
        })();
        finish(reply)
    }

    /// Add a track, optionally with an instrument. MIDI clips on the track play through its instrument.
    /// `kind` makes a group track (route other tracks through it with update_track `group`) or a return
    /// track (feed it with update_track `send`, and put a shared reverb or delay on it).
    #[tool(annotations(title = "Create track"))]
    async fn create_track(&self, Parameters(request): Parameters<CreateTrackRequest>) -> CallToolResult {
        self.ready_presets(request.instrument.as_deref(), &request.preset).await;
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
                match session.add_device(ChainRef::Track(track), uri, request.preset.as_deref(), None) {
                    Ok(device) => {
                        out["instrument_device_id"] = json!(device);
                        if let Some(preset) = session.device_preset(device) {
                            out["preset"] = json!(preset.name);
                        }
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
    /// group, or send it to a return track.
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
            let mut out = track_json(track, 4.0, false);
            // The clips are not what was asked about.
            if let Some(fields) = out.as_object_mut() {
                fields.remove("clips");
            }
            Ok(out)
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
                .map(descriptor_summary_json)
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
        let mut out = descriptor_json(descriptor);
        out["presets"] = presets_json(&list, &search);
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
            if !others.is_empty() {
                out["presets"]["note"] = json!(format!(
                    "This device offers no presets that can be listed. The same plugin in another format: {}.",
                    others.join("; ")
                ));
            }
        }
        finish(Ok(out))
    }

    /// Add an instrument or effect to a track's chain, or to the master chain when `track` is omitted. `preset`
    /// starts it from one of its presets, a plugin's included (describe_device lists and searches them);
    /// `params` go on top of the preset.
    #[tool(annotations(title = "Add device"))]
    async fn add_device(&self, Parameters(request): Parameters<AddDeviceRequest>) -> CallToolResult {
        self.ready_presets(Some(&request.uri), &request.preset).await;
        let mut session = self.session.lock();
        let reply = (|| {
            let chain = request.track.map_or(ChainRef::Master, |track| ChainRef::Track(TrackId(track)));
            let device = session
                .add_device(chain, &request.uri, request.preset.as_deref(), request.index)
                .map_err(|error| preset_hint(explain(session.project(), error), &request.uri))?;
            if let Some(params) = &request.params
                && let Err(error) = set_params(&mut session, device, params)
            {
                session.undo();
                return Err(format!("{error}. The device was not added."));
            }
            let model = session.project().device(device).map_err(|e| e.to_string())?;
            let mut out = json!({ "device_id": device, "name": model.name, "uri": model.uri });
            if let Some(preset) = &model.preset {
                out["preset"] = json!(preset.name);
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// A device's current parameter values, with ranges and units, the preset it was last given, and how many
    /// presets it can be given. `preset_query` and the other `preset_` arguments search and page those presets,
    /// as in describe_device. For the sample players (builtin:sampler, builtin:drumrack) also the audio files
    /// they play, per slot, with what is wrong with any that is missing.
    #[tool(annotations(title = "Get device", read_only_hint = true))]
    async fn get_device(&self, Parameters(request): Parameters<GetDeviceRequest>) -> CallToolResult {
        const LIMIT: usize = 200;
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
            let states = session.device_params(id).map_err(|error| explain(project, error))?;
            let query = request.query.as_deref().map(str::to_ascii_lowercase);
            let matching: Vec<Value> = states
                .iter()
                .filter(|state| {
                    query.as_deref().is_none_or(|q| {
                        state.info.id.to_ascii_lowercase().contains(q)
                            || state.info.name.to_ascii_lowercase().contains(q)
                    })
                })
                .map(param_state_json)
                .collect();
            let mut out = device_json(model);
            out["enabled"] = json!(model.enabled);
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
            if session.device_has_editor(id) {
                out["editor"] = json!(if session.is_device_editor_open(id) { "open" } else { "closed" });
            }
            if let Some(preset) = &model.preset {
                out["preset"] = json!(preset.name);
            }
            match &searched {
                Some(list) => {
                    let search = PresetSearch {
                        query: request.preset_query.as_deref(),
                        category: request.preset_category.as_deref(),
                        offset: request.preset_offset.unwrap_or(0),
                        limit: request.preset_limit.unwrap_or(DEFAULT_PRESETS).min(MAX_PRESETS),
                    };
                    out["presets"] = presets_json(list, &search);
                }
                // Not asked for: how many there are, and the presets
                // themselves only where they are few.
                None => {
                    let list = session.presets(&model.uri);
                    let limit = if list.len() <= UNASKED_PRESETS { UNASKED_PRESETS } else { 0 };
                    let mut presets = presets_json(&list, &PresetSearch { limit, ..PresetSearch::default() });
                    if let (0, Some(fields)) = (limit, presets.as_object_mut()) {
                        fields.remove("items");
                        fields.remove("kinds");
                        fields.insert("more".into(), json!("preset_query searches them; preset_offset pages through"));
                    }
                    out["presets"] = presets;
                }
            }
            // A reply about presets carries the parameters only if they
            // were asked about too.
            if searched.is_some() && request.query.is_none() {
                return Ok(out);
            }
            out["param_count"] = json!(matching.len());
            if matching.len() > LIMIT {
                out["truncated"] = json!(format!("showing the first {LIMIT}; narrow it down with `query`"));
            }
            out["params"] = matching.into_iter().take(LIMIT).collect();
            Ok(out)
        })();
        finish(reply)
    }

    /// Change a device: switch preset, set parameters, or bypass it. A preset replaces the device's sound, a
    /// plugin's as its own preset browser would (get_device and describe_device list and search them);
    /// `params` in the same call go on top of it, and the whole call is one undo step.
    #[tool(annotations(title = "Update device"))]
    async fn update_device(&self, Parameters(request): Parameters<UpdateDeviceRequest>) -> CallToolResult {
        let id = DeviceId(request.device);
        let uri = self.session.lock().project().device(id).map(|device| device.uri.clone()).ok();
        self.ready_presets(uri.as_deref(), &request.preset).await;
        let mut session = self.session.lock();
        let reply = (|| {
            // Validate before touching anything, so a bad value changes nothing.
            let resolved = match &request.params {
                Some(params) => resolve_params(&session, id, params)?,
                None => Vec::new(),
            };
            session.begin_gesture();
            let mut loaded = None;
            let result: Result<(), String> = (|| {
                if let Some(preset) = &request.preset {
                    let preset = session.load_preset(id, preset).map_err(|error| {
                        preset_hint(explain(session.project(), error), uri.as_deref().unwrap_or_default())
                    })?;
                    loaded = Some(preset.path());
                }
                if !resolved.is_empty() {
                    session.set_device_params(id, &resolved).map_err(|error| explain(session.project(), error))?;
                }
                if let Some(enabled) = request.enabled {
                    edit(&mut session, |project| {
                        project.device_mut(id)?.enabled = enabled;
                        Ok(())
                    })?;
                }
                Ok(())
            })();
            session.end_gesture();
            result?;
            let changed: BTreeMap<String, f64> = resolved.into_iter().collect();
            let model = session.project().device(id).map_err(|e| e.to_string())?;
            let mut out = json!({ "device_id": id, "name": model.name, "enabled": model.enabled, "set": changed });
            if let Some(preset) = loaded {
                out["preset"] = json!(preset);
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
            let notes = parse_notes(request.notes.as_deref().unwrap_or_default())?;
            let curves = parse_curves(request.controllers.as_ref())?;
            let name = request.name.clone().unwrap_or_else(|| "Clip".to_string());
            let clip = edit(&mut session, |project| {
                let id = project.add_midi_clip(TrackId(request.track), name, request.start, request.length)?;
                let clip = project.clip_mut(id)?;
                clip.add_notes(&notes)?;
                if let Some(loop_length) = request.loop_length {
                    clip.set_loop_length(Some(loop_length))?;
                }
                for (kind, points) in &curves {
                    clip.set_controller(*kind, points.clone())?;
                }
                Ok(id)
            })?;
            let mut out = json!({ "clip_id": clip, "note_count": notes.len() });
            if !curves.is_empty() {
                out["controllers"] = curves.iter().map(|(kind, _)| kind.key()).collect();
                add_controller_warnings(&mut out, &session, clip, curves.iter().map(|(kind, _)| *kind));
            }
            add_range_warning(&mut out, &session, clip);
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
                    clip.set_loop_length((loop_length > 0.0).then_some(loop_length))?;
                }
                if let Some(track) = request.track {
                    let start = project.clip(id)?.start;
                    project.move_clip(id, Some(TrackId(track)), start)?;
                }
                Ok(())
            })?;
            let project = session.project();
            let beats_per_bar = project.time_signature.beats_per_bar();
            let mut out = clip_json(project.clip(id).map_err(|e| e.to_string())?, beats_per_bar, false);
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
            .map(|clip| clip_json(clip, project.time_signature.beats_per_bar(), true))
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
            let curves: Vec<Value> = clip
                .controllers()
                .iter()
                .filter(|curve| wanted.is_none_or(|kind| curve.controller == kind))
                .map(|curve| controller_json(curve, true))
                .collect();
            let mut out = json!({ "clip_id": clip.id, "length": round(clip.length), "controllers": curves });
            if let (Some(kind), true) = (wanted, curves_is_empty(&out)) {
                out["note"] =
                    json!(format!("the clip has no {kind} curve; set_controllers or shape_dynamics writes one"));
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
            let written = edit(&mut session, |project| {
                let clip = project.clip_mut(id)?;
                for (kind, points) in &curves {
                    clip.set_controller(*kind, points.clone())?;
                }
                Ok(clip.controllers().iter().map(|curve| controller_json(curve, false)).collect::<Vec<Value>>())
            })?;
            let removed: Vec<String> =
                curves.iter().filter(|(_, points)| points.is_empty()).map(|(kind, _)| kind.key()).collect();
            let mut out = json!({ "clip_id": id, "controllers": written });
            if !removed.is_empty() {
                out["removed"] = json!(removed);
            }
            let kept = curves.iter().filter(|(_, points)| !points.is_empty()).map(|(kind, _)| *kind);
            add_controller_warnings(&mut out, &session, id, kept);
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
            let notes = parse_notes(&request.notes)?;
            let total = edit(&mut session, |project| {
                let clip = project.clip_mut(id)?;
                clip.add_notes(&notes)?;
                Ok(clip.notes().len())
            })?;
            let mut out = json!({ "added": notes.len(), "note_count": total });
            add_range_warning(&mut out, &session, id);
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
            let notes = parse_notes(&request.notes)?;
            let count = notes.len();
            edit(&mut session, |project| project.clip_mut(id)?.set_notes(notes))?;
            let mut out = json!({ "note_count": count });
            add_range_warning(&mut out, &session, id);
            Ok(out)
        })();
        finish(reply)
    }

    /// Remove notes from a MIDI clip, filtered by time range and/or pitch. With no filter, removes all notes.
    #[tool(annotations(title = "Remove notes", destructive_hint = true))]
    fn remove_notes(&self, Parameters(request): Parameters<RemoveNotesRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let pitch = request.pitch.as_ref().map(|pitch| parse_pitch_value(&pitch.0)).transpose()?;
            let (start, end) = (request.start.unwrap_or(f64::MIN), request.end.unwrap_or(f64::MAX));
            let (removed, remaining) = edit(&mut session, |project| {
                let clip = project.clip_mut(ClipId(request.clip))?;
                let removed = clip.remove_notes(|note| {
                    note.start >= start && note.start < end && pitch.is_none_or(|p| note.pitch == p)
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
        let reply = edit(&mut session, |project| {
            let clip = project.clip_mut(ClipId(request.clip))?;
            let mut notes = clip.notes().to_vec();
            let mut noise = Noise(request.seed.unwrap_or(1).max(1));
            let mut changed = 0;
            for note in notes.iter_mut().filter(|note| note.start >= start && note.start < end) {
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
            Ok(json!({ "changed": changed }))
        });
        finish(reply)
    }

    /// Automate a track's volume or pan, the master volume, or any device parameter, with a breakpoint
    /// envelope over time. Use it for fades, filter sweeps, build-ups and drops.
    #[tool(annotations(title = "Set automation"))]
    fn set_automation(&self, Parameters(request): Parameters<SetAutomationRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            if request.points.is_none() && request.enabled.is_none() {
                return Err("give `points` (the envelope), `enabled` (to switch it off or on), or both".to_string());
            }
            let points = request.points.as_deref().map(parse_points).transpose()?;
            let need = |value: Option<u64>, what: &str| value.ok_or_else(|| format!("this target needs `{what}`"));
            let (target, min, max) = match request.target.kind {
                TargetKind::TrackVolume => (
                    AutomationTarget::TrackVolume { track: TrackId(need(request.target.track, "track")?) },
                    f64::MIN,
                    MAX_VOLUME_DB as f64,
                ),
                TargetKind::TrackPan => {
                    (AutomationTarget::TrackPan { track: TrackId(need(request.target.track, "track")?) }, -1.0, 1.0)
                }
                TargetKind::MasterVolume => (AutomationTarget::MasterVolume, f64::MIN, MAX_VOLUME_DB as f64),
                TargetKind::DeviceParam => {
                    let device = DeviceId(need(request.target.device, "device")?);
                    let key = request.target.param.as_deref().ok_or("this target needs `param`")?;
                    let states = session.device_params(device).map_err(|error| explain(session.project(), error))?;
                    let infos: Vec<_> = states.into_iter().map(|state| state.info).collect();
                    let info = find_param(&infos, key)?;
                    (AutomationTarget::DeviceParam { device, param: info.id.clone() }, info.min, info.max)
                }
            };
            if let Some(point) = points.iter().flatten().find(|point| point.value < min || point.value > max) {
                return Err(format!(
                    "value {} at beat {} is outside the allowed range {min}..={max}",
                    point.value, point.beat
                ));
            }
            edit(&mut session, |project| {
                let written = points.is_some();
                if let Some(points) = points {
                    project.set_automation(target.clone(), points)?;
                }
                let lane = project.automation.iter().find(|lane| lane.target == target);
                let Some(count) = lane.map(|lane| lane.points.len()) else {
                    // No envelope: one was just cleared, or there never was one to switch.
                    return match written {
                        true => Ok(json!({ "point_count": 0, "cleared": true })),
                        false => Err(EditError::Invalid(
                            "there is no automation for that target to switch; write `points` first".into(),
                        )),
                    };
                };
                // Writing an envelope means wanting to hear it.
                let enabled = request.enabled.unwrap_or(written || lane.is_some_and(|lane| lane.enabled));
                project.set_automation_enabled(&target, enabled)?;
                Ok(json!({ "point_count": count, "cleared": false, "enabled": enabled }))
            })
        })();
        finish(reply)
    }

    /// Every automation envelope in the project, with its points as [beat, value] pairs.
    #[tool(annotations(title = "Get automation", read_only_hint = true))]
    fn get_automation(&self) -> CallToolResult {
        let session = self.session.lock();
        let lanes: Vec<Value> = session.project().automation.iter().map(|lane| lane_json(lane, true)).collect();
        finish(Ok(json!({ "automation": lanes })))
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
    #[tool(annotations(title = "Edit time"))]
    fn edit_time(&self, Parameters(request): Parameters<EditTimeRequest>) -> CallToolResult {
        enum Change {
            Delete(f64),
            Duplicate(f64),
            Insert(f64),
        }
        let change = match (request.action.as_str(), request.end, request.length) {
            ("delete", Some(end), _) => Ok(Change::Delete(end)),
            ("duplicate", Some(end), _) => Ok(Change::Duplicate(end)),
            ("insert", _, Some(length)) => Ok(Change::Insert(length)),
            ("delete" | "duplicate", None, _) => {
                Err(format!("action \"{}\" needs `end`, the beat where the stretch ends", request.action))
            }
            ("insert", _, None) => Err("action \"insert\" needs `length`, the number of beats to open up".to_string()),
            (other, ..) => Err(format!("unknown action '{other}'; use \"delete\", \"duplicate\" or \"insert\"")),
        };
        let mut session = self.session.lock();
        let reply = change.and_then(|change| {
            edit(&mut session, |project| {
                let before = project.length_beats();
                match change {
                    Change::Delete(end) => project.delete_time(request.start, end)?,
                    Change::Duplicate(end) => project.duplicate_time(request.start, end)?,
                    Change::Insert(length) => project.insert_time(request.start, length)?,
                }
                let markers: Vec<Value> =
                    project.markers.iter().map(|m| json!({ "beat": round(m.beat), "name": m.name })).collect();
                Ok(json!({
                    "length_beats": round(project.length_beats()),
                    "was_beats": round(before),
                    "markers": markers,
                }))
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
                    commanded(true, position.unwrap_or(current.position_beats))
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
            let seconds_per_beat = 60.0 / session.project().tempo;
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

    /// Render the song to a WAV file, faster than real time, and measure it: peak, loudness (LUFS), clipping,
    /// stereo correlation, spectral balance and the peak level of each track. This is how you check a mix
    /// without hearing it.
    #[tool(annotations(title = "Render to WAV"))]
    async fn render(&self, Parameters(request): Parameters<RenderRequest>) -> CallToolResult {
        // Snapshot under the lock, render outside it: a long bounce must not
        // freeze the GUI or other tools.
        let (job, path, options) = {
            let mut session = self.session.lock();
            let path = match self.write_path(&session, request.path.as_deref(), "a render", &["wav"]) {
                Ok(path) => path.unwrap_or_else(|| default_render_path(&session)),
                Err(refusal) => return finish(Err(refusal)),
            };
            let defaults = RenderOptions::default();
            let options = RenderOptions {
                sample_rate: session.audio_info().sample_rate as u32,
                start_beat: request.start.unwrap_or(defaults.start_beat),
                end_beat: request.end,
                max_tail_seconds: request.max_tail_seconds.unwrap_or(defaults.max_tail_seconds),
            };
            (session.render_job(), path, options)
        };
        let outcome = tokio::task::spawn_blocking(move || {
            let report = job.render_to_file(&path, &options)?;
            let silent = silent_track_notes(&job.project, &report.tracks, options.start_beat, options.end_beat);
            Ok::<_, trill_engine::render::RenderError>((report, silent))
        })
        .await;
        finish(match outcome {
            Ok(Ok((report, silent))) => Ok(render_reply(&report, silent)),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(format!("the render did not finish: {error}")),
        })
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

            // One undo step for the whole import.
            session.begin_gesture();
            let result: Result<Vec<Value>, String> = (|| {
                let parts = edit(&mut session, |project| {
                    if use_tempo {
                        if let Some(tempo) = file.tempo {
                            project.set_tempo(tempo.clamp(MIN_TEMPO, MAX_TEMPO))?;
                        }
                        if let Some(signature) = file.time_signature {
                            project.set_time_signature(signature.numerator, signature.denominator)?;
                        }
                    }
                    project.import_midi(&file, request.at.unwrap_or(0.0))
                })?;
                let mut out = Vec::new();
                for part in parts {
                    let uri = if part.drums { "builtin:drumkit" } else { melodic };
                    let device = session
                        .add_device(ChainRef::Track(part.track), uri, None, None)
                        .map_err(|error| error.to_string())?;
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
            let parts = result?;
            Ok(json!({ "tracks": parts, "file_tempo": file.tempo, "tempo": session.project().tempo }))
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
            Ok(json!({ "saved": path.display().to_string() }))
        })();
        finish(reply)
    }

    /// Open a project file, replacing the current project. Unsaved changes are lost.
    #[tool(annotations(title = "Load project", destructive_hint = true))]
    fn load_project(&self, Parameters(request): Parameters<LoadProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = self.may_replace_project(&session).and_then(|()| {
            session.load(Path::new(&request.path)).map(|()| project_json(&session, false)).map_err(|e| e.to_string())
        });
        finish(reply)
    }

    /// Start an empty project, replacing the current one. Unsaved changes are lost.
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
    kinds: impl Iterator<Item = ControllerKind>,
) {
    let project = session.project();
    let (Ok(found), Ok(track)) = (project.clip(clip), project.clip_track(clip)) else { return };
    let support = session.controller_support(track);
    let mut warnings: Vec<String> = Vec::new();
    for kind in kinds {
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
            .controller(kind)
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

fn is_plugin(uri: &str) -> bool {
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

fn resolve_params(
    session: &Session,
    device: DeviceId,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<(String, f64)>, String> {
    let states = session.device_params(device).map_err(|error| explain(session.project(), error))?;
    let infos: Vec<_> = states.into_iter().map(|state| state.info).collect();
    params
        .iter()
        .map(|(key, value)| {
            let info = find_param(&infos, key)?;
            Ok((info.id.clone(), parse_param_value(info, value)?))
        })
        .collect()
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
        let audio_beats = (facts.seconds - audio.offset_seconds).max(0.0) / audio.seconds_per_beat(project.tempo);
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

/// Notes that start at or past the end of their clip never play. Say so,
/// since the caller cannot hear that they are missing.
fn add_range_warning(out: &mut Value, session: &Session, clip: ClipId) {
    let Ok(clip) = session.project().clip(clip) else { return };
    let Some(midi) = clip.midi() else { return };
    let playable = midi.loop_length.map_or(clip.length, |length| length.min(clip.length));
    let outside = midi.notes.iter().filter(|note| note.start >= playable).count();
    if outside > 0 {
        out["warning"] = json!(format!(
            "{outside} note(s) start at or after beat {} of the clip, past its {}, and will not play. \
             Lengthen the clip or move the notes.",
            round(playable),
            if midi.loop_length.is_some() { "loop length" } else { "end" }
        ));
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TrillServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("trill", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!("{INSTRUCTIONS}\n\n{}", crate::music::INSTRUCTIONS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_that_had_notes_and_made_no_sound_is_named_with_what_to_try() {
        let mut project = Project::new("t");
        let mut track = |name: &str, uri: &str, clip_at: Option<f64>| {
            let id = project.add_track(name);
            project.add_device(ChainRef::Track(id), uri, name, None).unwrap();
            if let Some(start) = clip_at {
                project.add_midi_clip(id, "c", start, 4.0).unwrap();
            }
            id
        };
        let sax = track("Sax", "vst3:ABCDEF", Some(0.0));
        let kit = track("Kit", "builtin:drumrack", Some(0.0));
        let keys = track("Keys", "builtin:polysynth", Some(0.0));
        let later = track("Later", "vst3:ABCDEF", Some(64.0));
        let empty = track("Empty", "vst3:ABCDEF", None);
        let muted = track("Muted", "vst3:ABCDEF", Some(0.0));
        project.track_mut(muted).unwrap().mute = true;
        let level = |track: TrackId, peak_db: f32| TrackLevel { track, name: String::new(), peak_db };
        let levels = [
            level(sax, -120.0),
            level(kit, -120.0),
            level(keys, -9.0),
            level(later, -120.0),
            level(empty, -120.0),
            level(muted, -120.0),
        ];

        let notes = silent_track_notes(&project, &levels, 0.0, Some(16.0));
        assert_eq!(notes.len(), 2, "{notes:#?}");
        // A plugin: the likely cause and the tools that fix it, which is a
        // curve in the clip.
        assert!(notes[0].starts_with("'Sax' has clips here but made no sound."), "{}", notes[0]);
        for word in ["expression", "shape_dynamics", "set_controllers"] {
            assert!(notes[0].contains(word), "{word}: {}", notes[0]);
        }
        assert!(!notes[0].contains("set_automation") && !notes[0].contains("update_device"), "{}", notes[0]);
        // A built-in device: what can be wrong with those.
        assert!(notes[1].starts_with("'Kit' has clips here but made no sound."), "{}", notes[1]);
        assert!(!notes[1].contains("expression"));

        // A plugin whose clips have an expression curve already and is
        // silent all the same is not told to write one.
        let clip = project.track(sax).unwrap().clips[0].id;
        let level = vec![AutomationPoint::new(0.0, 0.0)];
        project.clip_mut(clip).unwrap().set_controller(ControllerKind::EXPRESSION, level).unwrap();
        let notes = silent_track_notes(&project, &levels, 0.0, Some(16.0));
        assert!(notes[0].contains("though they have an expression curve"), "{}", notes[0]);
        assert!(notes[0].contains("get_controllers") && !notes[0].contains("shape_dynamics"), "{}", notes[0]);

        // The whole song: the late clip counts now. A solo elsewhere, and
        // nothing that was silenced on purpose is news.
        let notes = silent_track_notes(&project, &levels, 0.0, None);
        assert!(notes.iter().any(|note| note.starts_with("'Later'")), "{notes:#?}");
        project.track_mut(keys).unwrap().solo = true;
        assert!(silent_track_notes(&project, &levels, 0.0, None).is_empty());
    }
}
