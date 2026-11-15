//! The MCP tools. Each one is a thin adapter from a request to calls on the
//! shared [`Session`](daw_engine::Session); no musical logic lives here that
//! the GUI could not also reach.

use crate::input::{find_param, parse_notes, parse_param_value, parse_pitch_value, parse_points};
use crate::requests::*;
use crate::view::{
    clip_json, descriptor_json, descriptor_summary_json, device_json, lane_json, param_state_json, project_json, round,
    transport_json,
};
use daw_core::device::DeviceCategory;
use daw_core::project::{AudioContent, AutomationTarget, MAX_VOLUME_DB};
use daw_core::{ChainRef, ClipId, DeviceId, EditError, Project, TrackId};
use daw_engine::audio_file::read_wav;
use daw_engine::{RenderOptions, RenderReport, Session, SessionError, SharedSession};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const INSTRUCTIONS: &str = "\
This server controls a digital audio workstation. You compose, arrange and mix a song by calling tools; the \
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

Musical guidance
- Vary velocity and use transform_notes to humanize; identical velocities sound mechanical.
- Keep bass and kick in the centre; use low_cut on the EQ for everything else; leave headroom on the master.
- Give sections contrast: add and remove parts, automate filters and volume, change the drum pattern.
- Every edit can be reversed with history (undo/redo).";

type Reply = Result<Value, String>;

fn finish(reply: Reply) -> CallToolResult {
    match reply {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
    }
}

/// Turns an error into a message that says what to do next: a missing id
/// comes back with the ids that do exist.
fn explain(project: &Project, error: impl Into<SessionError>) -> String {
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
fn edit<T>(session: &mut Session, change: impl FnOnce(&mut Project) -> Result<T, EditError>) -> Result<T, String> {
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
        None => std::env::temp_dir().join("daw-renders"),
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
pub struct DawServer {
    session: SharedSession,
    tool_router: ToolRouter<Self>,
}

impl DawServer {
    pub fn new(session: SharedSession) -> Self {
        Self { session, tool_router: Self::tool_router() }
    }
}

#[tool_router]
impl DawServer {
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
    #[tool(annotations(title = "Create track"))]
    fn create_track(&self, Parameters(request): Parameters<CreateTrackRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            if request.preset.is_some() && request.instrument.is_none() {
                return Err("`preset` needs `instrument`".to_string());
            }
            let track = edit(&mut session, |project| {
                let id = project.add_track(request.name.clone());
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

    /// Change a track's name, volume, pan, mute, solo or position in the track list.
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
            Ok(json!({
                "id": track.id, "name": track.name, "volume_db": track.volume_db, "pan": track.pan,
                "mute": track.mute, "solo": track.solo,
            }))
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

    /// A device's current parameter values, with ranges and units.
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

    /// Place a WAV file on a track as an audio clip. It plays at its original speed and pitch.
    #[tool(annotations(title = "Create audio clip"))]
    fn create_audio_clip(&self, Parameters(request): Parameters<CreateAudioClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| {
            let path = Path::new(&request.path);
            let data = read_wav(path).map_err(|error| error.to_string())?;
            let offset = request.offset_seconds.unwrap_or(0.0);
            let seconds = data.left.len() as f64 / data.sample_rate - offset;
            if seconds <= 0.0 {
                return Err(format!("offset_seconds {offset} is past the end of the file"));
            }
            let length = request.length.unwrap_or(seconds * session.project().tempo / 60.0);
            let name = request.name.clone().unwrap_or_else(|| {
                path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into())
            });
            let content = AudioContent {
                offset_seconds: offset,
                gain_db: request.gain_db.unwrap_or(0.0),
                fade_in: request.fade_in.unwrap_or(0.0),
                fade_out: request.fade_out.unwrap_or(0.0),
                ..AudioContent::new(request.path.clone())
            };
            let clip = edit(&mut session, |project| {
                project.add_audio_clip(TrackId(request.track), name, request.start, length, content)
            })?;
            Ok(json!({ "clip_id": clip, "length": round(length), "file_seconds": round(seconds) }))
        })();
        finish(reply)
    }

    /// Rename, move, resize, loop or mute a clip, or move it to another track.
    #[tool(annotations(title = "Update clip"))]
    fn update_clip(&self, Parameters(request): Parameters<UpdateClipRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let id = ClipId(request.clip);
        let reply = (|| {
            if request.trim_start.is_some() && request.start.is_some() {
                return Err("give either `start` (move the clip) or `trim_start` (move its left edge), not both".into());
            }
            edit(&mut session, |project| {
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
impl ServerHandler for DawServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("daw", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
