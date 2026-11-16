//! The MCP tools. Each one is a thin adapter from a request to calls on the
//! shared [`Runtime`]: its session, its sound library and the operations
//! that join them. No musical logic lives here that the GUI could not also
//! reach.

use crate::input::{find_param, parse_notes, parse_param_value, parse_pitch_value, parse_points};
use crate::requests::*;
use crate::view::{
    clip_json, descriptor_json, descriptor_summary_json, device_json, lane_json, param_state_json, project_json, round,
    samples_json, scan_json, sound_json, track_json, transport_json, warp_name,
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
use trill_core::device::{DeviceCategory, EditorOptions};
use trill_core::midi_file;
use trill_core::project::{AutomationTarget, MAX_TEMPO, MAX_VOLUME_DB, MIN_TEMPO, WarpMode};
use trill_core::{ChainRef, ClipId, DeviceId, EditError, Project, TrackId, TrackKind};
use trill_engine::{RenderOptions, RenderReport, Session, SessionError, SharedSession};
use trill_library::{EntryKind, Key, Query, Tag, Tags, suggested_locations};
use trill_runtime::{Runtime, SoundClip, SoundError};

const SAMPLER_URI: &str = "builtin:sampler";
const DRUM_RACK_URI: &str = "builtin:drumrack";
/// Search results returned when the caller does not say how many, and the
/// most it can ask for.
const DEFAULT_SOUNDS: usize = 20;
const MAX_SOUNDS: usize = 200;
/// How long a library scan is waited for before the reply goes out without
/// it, when the caller does not say, and the longest it can ask for.
const DEFAULT_SCAN_WAIT: f64 = 15.0;
const MAX_SCAN_WAIT: f64 = 120.0;

const INSTRUCTIONS: &str = "\
This server controls Trill, a digital audio workstation. You compose, arrange and mix a song by calling tools; the \
user hears and sees every change live.

Conventions
- Time is in beats (quarter notes), starting at 0. In 4/4, bar N starts at beat (N-1)*4.
- Pitch is a MIDI number (60 = C4 = middle C), a note name (\"C4\", \"F#3\", \"Bb2\") or, on drum kits, a drum \
name (\"kick\", \"snare\", \"closed_hat\", \"open_hat\", \"clap\", ...). Velocity is 1-127.
- Every track, clip and device has an integer id, returned when it is created and listed by get_project.
- Note positions inside a clip are relative to the clip start.

A good workflow
1. get_project to see what exists; set_project for tempo and time signature.
2. create_track per part with an instrument and preset (list_devices shows instruments, effects and presets).
3. create_clip with notes. For repeating parts, write one or two bars and set loop_length, or duplicate_clip.
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

/// The render report plus plain-language observations about it.
fn render_reply(report: &RenderReport) -> Value {
    let stats = &report.stats;
    let mut notes = Vec::new();
    if stats.peak_db <= -119.0 {
        notes.push("The render is silent. Check that tracks have instruments, clips have notes, and nothing is muted.");
    } else {
        if stats.clipped_samples > 0 {
            notes.push("The output clips. Lower track volumes or keep a limiter last on the master.");
        }
        if stats.loudness_lufs < -20.0 {
            notes.push("Quiet master (streaming targets are around -14 LUFS). Raise track levels or the limiter gain.");
        } else if stats.loudness_lufs > -8.0 {
            notes.push("Very loud master; it may sound crushed. Consider lowering the limiter gain.");
        }
        if stats.stereo_correlation < 0.0 {
            notes.push("Negative stereo correlation: parts may cancel in mono. Reduce width or chorus depth.");
        }
        if stats.spectrum.sub_db > -3.0 {
            notes.push("Most of the energy is below 60 Hz. Thin out the sub bass or kick.");
        }
        if stats.spectrum.presence_db < -30.0 && stats.spectrum.air_db < -30.0 {
            notes.push("Very little energy above 2 kHz; the mix may sound dull. Open filters or add brighter parts.");
        }
    }
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
}

impl TrillServer {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        let session = runtime.session.clone();
        Self { session, runtime, tool_router: Self::tool_router() + Self::music_router() }
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
        let session = self.session.lock();
        finish(Ok(project_json(&session, request.include_notes)))
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
    fn create_track(&self, Parameters(request): Parameters<CreateTrackRequest>) -> CallToolResult {
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
                    Ok(device) => out["instrument_device_id"] = json!(device),
                    Err(error) => {
                        session.undo();
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

    /// The catalog of instruments and effects that can be added, with their presets. Built-in devices have
    /// `builtin:` URIs; installed plugins appear with `clap:` and `vst3:` URIs (a plugin installed in both
    /// formats is listed once per format; either works).
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

    /// Everything about one device type: what it does, its presets, and every parameter with range and unit.
    #[tool(annotations(title = "Describe device", read_only_hint = true))]
    fn describe_device(&self, Parameters(request): Parameters<DescribeDeviceRequest>) -> CallToolResult {
        let factory = self.session.lock().factory();
        let reply = factory
            .descriptors()
            .iter()
            .find(|descriptor| descriptor.uri == request.uri)
            .map(descriptor_json)
            .ok_or_else(|| format!("unknown device '{}'; list_devices shows what is available", request.uri));
        finish(reply)
    }

    /// Add an instrument or effect to a track's chain, or to the master chain when `track` is omitted.
    #[tool(annotations(title = "Add device"))]
    fn add_device(&self, Parameters(request): Parameters<AddDeviceRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let chain = request.track.map_or(ChainRef::Master, |track| ChainRef::Track(TrackId(track)));
            let device = session
                .add_device(chain, &request.uri, request.preset.as_deref(), request.index)
                .map_err(|error| explain(session.project(), error))?;
            if let Some(params) = &request.params
                && let Err(error) = set_params(&mut session, device, params)
            {
                session.undo();
                return Err(format!("{error}. The device was not added."));
            }
            let model = session.project().device(device).map_err(|e| e.to_string())?;
            Ok(json!({ "device_id": device, "name": model.name, "uri": model.uri }))
        })();
        finish(reply)
    }

    /// A device's current parameter values, with ranges and units. For the sample players (builtin:sampler,
    /// builtin:drumrack) also the audio files they play, per slot, with what is wrong with any that is missing.
    #[tool(annotations(title = "Get device", read_only_hint = true))]
    fn get_device(&self, Parameters(request): Parameters<GetDeviceRequest>) -> CallToolResult {
        const LIMIT: usize = 200;
        let session = self.session.lock();
        let reply = (|| {
            let id = DeviceId(request.device);
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
            out["param_count"] = json!(matching.len());
            if matching.len() > LIMIT {
                out["truncated"] = json!(format!("showing the first {LIMIT}; narrow it down with `query`"));
            }
            out["params"] = matching.into_iter().take(LIMIT).collect();
            Ok(out)
        })();
        finish(reply)
    }

    /// Change a device: switch preset, set parameters, or bypass it.
    #[tool(annotations(title = "Update device"))]
    fn update_device(&self, Parameters(request): Parameters<UpdateDeviceRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let id = DeviceId(request.device);
            // Validate before touching anything, so a bad value changes nothing.
            let resolved = match &request.params {
                Some(params) => resolve_params(&session, id, params)?,
                None => Vec::new(),
            };
            session.begin_gesture();
            let result: Result<(), String> = (|| {
                if let Some(preset) = &request.preset {
                    session.apply_preset(id, preset).map_err(|error| explain(session.project(), error))?;
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
            Ok(json!({ "device_id": id, "name": model.name, "enabled": model.enabled, "set": changed }))
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
            let name = request.name.clone().unwrap_or_else(|| "Clip".to_string());
            let clip = edit(&mut session, |project| {
                let id = project.add_midi_clip(TrackId(request.track), name, request.start, request.length)?;
                let clip = project.clip_mut(id)?;
                clip.add_notes(&notes)?;
                if let Some(loop_length) = request.loop_length {
                    clip.set_loop_length(Some(loop_length))?;
                }
                Ok(id)
            })?;
            let mut out = json!({ "clip_id": clip, "note_count": notes.len() });
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
            let points = parse_points(&request.points)?;
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
            if let Some(point) = points.iter().find(|point| point.value < min || point.value > max) {
                return Err(format!(
                    "value {} at beat {} is outside the allowed range {min}..={max}",
                    point.value, point.beat
                ));
            }
            let count = points.len();
            edit(&mut session, |project| project.set_automation(target, points))?;
            Ok(json!({ "point_count": count, "cleared": count == 0 }))
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

    /// Control live playback through the user's speakers: play, stop, seek, or read the playhead.
    #[tool(annotations(title = "Transport"))]
    fn transport(&self, Parameters(request): Parameters<TransportRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let position = request.position;
            if position.is_some_and(|beat| !beat.is_finite() || beat < 0.0) {
                return Err("position must be a beat >= 0".to_string());
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
                    session.stop();
                    commanded(false, current.position_beats)
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
                TransportAction::Status => transport_json(&session),
            })
        })();
        finish(reply)
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
            let path = request.path.as_deref().map(PathBuf::from).unwrap_or_else(|| default_render_path(&session));
            let defaults = RenderOptions::default();
            let options = RenderOptions {
                sample_rate: session.audio_info().sample_rate as u32,
                start_beat: request.start.unwrap_or(defaults.start_beat),
                end_beat: request.end,
                max_tail_seconds: request.max_tail_seconds.unwrap_or(defaults.max_tail_seconds),
            };
            (session.render_job(), path, options)
        };
        let outcome = tokio::task::spawn_blocking(move || job.render_to_file(&path, &options)).await;
        finish(match outcome {
            Ok(Ok(report)) => Ok(render_reply(&report)),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(format!("the render did not finish: {error}")),
        })
    }

    /// Bring a MIDI file into the song: each part becomes a new track with an instrument and one clip.
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
                    out.push(json!({
                        "track_id": part.track,
                        "name": project.track(part.track).map_err(|e| e.to_string())?.name,
                        "instrument_device_id": device,
                        "clip_id": part.clip,
                        "note_count": clip.notes().len(),
                        "length": round(clip.length),
                        "drums": part.drums,
                    }));
                }
                Ok(out)
            })();
            session.end_gesture();
            let parts = result?;
            Ok(json!({ "tracks": parts, "file_tempo": file.tempo, "tempo": session.project().tempo }))
        })();
        finish(reply)
    }

    /// Write the song's notes to a Standard MIDI File, one track per track, exactly as they sound (loops
    /// repeated, trims applied).
    #[tool(annotations(title = "Export MIDI file"))]
    fn export_midi(&self, Parameters(request): Parameters<ExportMidiRequest>) -> CallToolResult {
        let session = self.session.lock();
        let reply = (|| {
            let file = session.project().export_midi();
            if file.tracks.is_empty() {
                return Err("the project has no MIDI notes to export".to_string());
            }
            let path = Path::new(&request.path);
            if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(path, midi_file::write(&file)).map_err(|error| format!("{}: {error}", request.path))?;
            let notes: usize = file.tracks.iter().map(|track| track.notes.len()).sum();
            Ok(json!({ "path": request.path, "tracks": file.tracks.len(), "notes": notes }))
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
            let path = match (&request.path, session.path()) {
                (Some(path), _) => PathBuf::from(path),
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
        let reply = session
            .load(Path::new(&request.path))
            .map(|()| project_json(&session, false))
            .map_err(|error| error.to_string());
        finish(reply)
    }

    /// Start an empty project, replacing the current one. Unsaved changes are lost.
    #[tool(annotations(title = "New project", destructive_hint = true))]
    fn new_project(&self, Parameters(request): Parameters<NewProjectRequest>) -> CallToolResult {
        let mut session = self.session.lock();
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
