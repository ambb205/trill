//! JSON views of the project for tool replies. Replies are shaped for a
//! reader that pays per token: compact, but with the ids and positions it
//! needs to plan its next call.

use serde_json::{Value, json};
use trill_core::device::{DeviceCategory, DeviceDescriptor, ParamInfo, ParamKind};
use trill_core::pitch::pitch_name;
use trill_core::project::{AutomationLane, AutomationTarget, Clip, ClipContent, Segment, WarpMode};
use trill_core::{Device, Note, Project, Track, TrackKind};
use trill_engine::{AudioConfig, AudioDevice, RecordState, TakeClip};
use trill_engine::{ParamState, SampleFile, Session};
use trill_library::{Entry, ScanStatus};
use trill_runtime::{AudioStatus, MidiInputInfo};

/// Rounds for display so replies do not carry float noise.
pub fn round(value: f64) -> f64 {
    (value * 1e4).round() / 1e4
}

pub fn note_json(note: &Note) -> Value {
    json!({
        "pitch": note.pitch,
        "name": pitch_name(note.pitch),
        "start": round(note.start),
        "length": round(note.length),
        "velocity": note.velocity,
    })
}

pub fn clip_json(clip: &Clip, beats_per_bar: f64, include_notes: bool) -> Value {
    let mut out = json!({
        "id": clip.id,
        "name": clip.name,
        "start": round(clip.start),
        "length": round(clip.length),
        "start_bar": round(clip.start / beats_per_bar + 1.0),
    });
    let fields = out.as_object_mut().expect("object literal");
    if clip.muted {
        fields.insert("muted".into(), json!(true));
    }
    match &clip.content {
        ClipContent::Midi(midi) => {
            fields.insert("type".into(), json!("midi"));
            fields.insert("note_count".into(), json!(midi.notes.len()));
            if let Some(loop_length) = midi.loop_length {
                fields.insert("loop_length".into(), json!(round(loop_length)));
            }
            if midi.start_offset != 0.0 {
                fields.insert("start_offset".into(), json!(round(midi.start_offset)));
            }
            if let (Some(low), Some(high)) =
                (midi.notes.iter().map(|n| n.pitch).min(), midi.notes.iter().map(|n| n.pitch).max())
            {
                fields.insert("pitch_range".into(), json!(format!("{}-{}", pitch_name(low), pitch_name(high))));
            }
            if include_notes {
                fields.insert("notes".into(), midi.notes.iter().map(note_json).collect());
            }
        }
        ClipContent::Audio(audio) => {
            fields.insert("type".into(), json!("audio"));
            fields.insert("path".into(), json!(audio.path));
            fields.insert("offset_seconds".into(), json!(round(audio.offset_seconds)));
            fields.insert("gain_db".into(), json!(audio.gain_db));
            if audio.fade_in != 0.0 || audio.fade_out != 0.0 {
                fields.insert("fade_in".into(), json!(round(audio.fade_in)));
                fields.insert("fade_out".into(), json!(round(audio.fade_out)));
            }
            if audio.warp != WarpMode::Off {
                fields.insert("warp".into(), json!(warp_name(audio.warp)));
            }
            if let Some(tempo) = audio.source_tempo {
                fields.insert("source_tempo".into(), json!(round(tempo)));
            }
            if audio.transpose != 0.0 {
                fields.insert("transpose".into(), json!(round(audio.transpose)));
            }
        }
    }
    out
}

pub fn warp_name(warp: WarpMode) -> &'static str {
    match warp {
        WarpMode::Off => "off",
        WarpMode::Repitch => "repitch",
        WarpMode::Beats => "beats",
        WarpMode::Tonal => "tonal",
    }
}

/// The files a sample-playing device uses, each with what is wrong with it.
pub fn samples_json(files: &[SampleFile]) -> Value {
    files
        .iter()
        .map(|file| {
            let mut out = json!({ "slot": file.slot, "path": file.path });
            if let Some(problem) = &file.problem {
                out["problem"] = json!(problem);
            }
            out
        })
        .collect()
}

/// One library entry, for search results.
pub fn sound_json(entry: &Entry, favorite: bool) -> Value {
    let mut out = json!({ "path": entry.path.display().to_string(), "name": entry.name });
    let fields = out.as_object_mut().expect("object literal");
    if entry.kind != trill_library::EntryKind::Audio {
        fields.insert("kind".into(), json!(entry.kind.label()));
    }
    if let Some(seconds) = entry.seconds() {
        fields.insert("seconds".into(), json!((seconds * 1e3).round() / 1e3));
    }
    if let Some(tempo) = entry.tempo {
        fields.insert("tempo".into(), json!(tempo));
    }
    if let Some(key) = entry.key {
        fields.insert("key".into(), json!(key.name()));
    }
    if !entry.tags.is_empty() {
        fields.insert("tags".into(), entry.tags.iter().map(|tag| tag.label()).collect());
    }
    if favorite {
        fields.insert("favorite".into(), json!(true));
    }
    out
}

/// Whether the library is scanning and how far it has got.
pub fn scan_json(status: &ScanStatus) -> Value {
    let mut out = json!({ "scanning": status.is_scanning() });
    if status.is_scanning() {
        out["progress"] = json!({
            "phase": format!("{:?}", status.phase).to_lowercase(),
            "folders_listed": status.folders,
            "files_found": status.files,
            "headers_read": status.headers_read,
            "headers_to_read": status.headers_to_read,
        });
    }
    if let Some(last) = &status.last {
        let mut scan = json!({
            "files": last.files,
            "unreadable": last.unreadable,
            "seconds": (last.elapsed.as_secs_f64() * 100.0).round() / 100.0,
        });
        if last.cancelled {
            scan["cancelled"] = json!(true);
        }
        if !last.problems.is_empty() {
            scan["problems"] = json!(last.problems);
        }
        out["last_scan"] = scan;
    }
    out
}

pub fn device_json(device: &Device) -> Value {
    let mut out = json!({ "id": device.id, "uri": device.uri, "name": device.name });
    if !device.enabled {
        out["enabled"] = json!(false);
    }
    out
}

pub fn track_json(track: &Track, beats_per_bar: f64, include_notes: bool) -> Value {
    let mut out = track_fields(track, beats_per_bar, include_notes);
    match track.kind {
        TrackKind::Normal => {}
        TrackKind::Group => out["kind"] = json!("group"),
        TrackKind::Return => out["kind"] = json!("return"),
    }
    if let Some(group) = track.group {
        out["group"] = json!(group);
    }
    if !track.sends.is_empty() {
        out["sends"] = track.sends.iter().map(|send| json!({ "to": send.to, "level_db": send.level_db })).collect();
    }
    out
}

fn track_fields(track: &Track, beats_per_bar: f64, include_notes: bool) -> Value {
    json!({
        "id": track.id,
        "name": track.name,
        "volume_db": track.volume_db,
        "pan": track.pan,
        "mute": track.mute,
        "solo": track.solo,
        "devices": track.devices.iter().map(device_json).collect::<Vec<_>>(),
        "clips": track.clips.iter().map(|clip| clip_json(clip, beats_per_bar, include_notes)).collect::<Vec<_>>(),
    })
}

pub fn target_json(target: &AutomationTarget) -> Value {
    match target {
        AutomationTarget::TrackVolume { track } => json!({ "type": "track_volume", "track": track }),
        AutomationTarget::TrackPan { track } => json!({ "type": "track_pan", "track": track }),
        AutomationTarget::DeviceParam { device, param } => {
            json!({ "type": "device_param", "device": device, "param": param })
        }
        AutomationTarget::MasterVolume => json!({ "type": "master_volume" }),
    }
}

pub fn lane_json(lane: &AutomationLane, include_points: bool) -> Value {
    let mut out = json!({ "target": target_json(&lane.target), "point_count": lane.points.len() });
    if !lane.enabled {
        out["enabled"] = json!(false);
    }
    if include_points {
        out["points"] = lane
            .points
            .iter()
            .map(|p| match p.shape {
                Segment::Linear => json!([round(p.beat), round(p.value)]),
                Segment::Hold => json!([round(p.beat), round(p.value), "hold"]),
                Segment::Curve(bend) => json!([round(p.beat), round(p.value), round(bend as f64)]),
            })
            .collect();
    }
    out
}

/// Song position as `bar.beat`, both 1-based, e.g. beat 5.5 in 4/4 → "2.2.5".
pub fn bar_beat(beat: f64, beats_per_bar: f64) -> String {
    let bar = (beat / beats_per_bar).floor();
    let within = beat - bar * beats_per_bar;
    format!("{}.{}", bar as i64 + 1, round(within + 1.0))
}

pub fn transport_json(session: &Session) -> Value {
    let status = session.transport();
    let beats_per_bar = session.project().time_signature.beats_per_bar();
    let mut out = json!({
        "playing": status.playing,
        "position_beats": round(status.position_beats),
        "position_bar_beat": bar_beat(status.position_beats, beats_per_bar),
    });
    if session.metronome() {
        out["metronome"] = json!(true);
    }
    if let Some(recording) = recording_json(session) {
        out["recording"] = recording;
    }
    out
}

/// The take being recorded, if one is.
pub fn recording_json(session: &Session) -> Option<Value> {
    let status = session.record_status();
    let state = match status.state {
        RecordState::Idle => return None,
        RecordState::CountIn => "count_in",
        RecordState::Recording => "recording",
    };
    let mut out = json!({ "state": state, "tracks": status.tracks, "notes": status.notes });
    if let Some(start) = status.start_beat {
        out["from_beat"] = json!(round(start));
    }
    if status.pass > 0 {
        out["loop_pass"] = json!(status.pass + 1);
    }
    Some(out)
}

/// The clips a take wrote into.
pub fn take_json(clips: &[TakeClip]) -> Value {
    clips
        .iter()
        .map(|clip| json!({ "track_id": clip.track, "clip_id": clip.clip, "notes": clip.notes, "new_clip": clip.created }))
        .collect()
}

/// The audio stream: what is running, and what was asked for where that
/// differs.
pub fn audio_json(status: &AudioStatus) -> Value {
    let info = &status.info;
    let mut out = json!({
        "output": info.output,
        "device": info.device,
        "sample_rate": info.sample_rate,
        "buffer_frames": info.buffer_frames,
        "buffer_ms": round(info.buffer_ms()),
        "latency_ms": round(status.latency_ms),
        "exclusive": info.exclusive,
        "load": round(f64::from(status.load)),
    });
    if let Some(warning) = &info.warning {
        out["warning"] = json!(warning);
    }
    let asked = &status.config;
    if *asked != AudioConfig::default() {
        out["asked_for"] = json!({
            "output": asked.output,
            "sample_rate": asked.sample_rate,
            "buffer_size": asked.buffer_size,
            "exclusive": asked.exclusive,
        });
    }
    out
}

pub fn audio_device_json(device: &AudioDevice) -> Value {
    let mut out = json!({ "name": device.name, "sample_rates": device.sample_rates, "channels": device.channels });
    if device.is_default {
        out["default"] = json!(true);
    }
    if device.exclusive {
        out["exclusive"] = json!(true);
    }
    out
}

pub fn midi_input_json(input: &MidiInputInfo) -> Value {
    let mut out = json!({ "name": input.name, "enabled": input.enabled, "listening": input.open });
    if !input.present {
        out["unplugged"] = json!(true);
    }
    if let Some(problem) = &input.problem {
        out["problem"] = json!(problem);
    }
    if input.events > 0 {
        out["messages"] = json!(input.events);
    }
    out
}

pub fn project_json(session: &Session, include_notes: bool) -> Value {
    let project: &Project = session.project();
    let beats_per_bar = project.time_signature.beats_per_bar();
    let length = project.length_beats();
    let info = session.audio_info();
    let mut out = json!({
        "name": project.name,
        "tempo": project.tempo,
        "time_signature": format!("{}/{}", project.time_signature.numerator, project.time_signature.denominator),
        "beats_per_bar": beats_per_bar,
        "length_beats": round(length),
        "length_bars": round(length / beats_per_bar),
        "length_seconds": round(length * 60.0 / project.tempo),
        "loop": {
            "enabled": project.loop_region.enabled,
            "start": project.loop_region.start,
            "end": project.loop_region.end,
        },
        "transport": transport_json(session),
        "tracks": project
            .tracks
            .iter()
            .map(|track| {
                let mut out = track_json(track, beats_per_bar, include_notes);
                if session.is_armed(track.id) {
                    out["armed"] = json!(true);
                }
                out
            })
            .collect::<Vec<_>>(),
        "master": {
            "volume_db": project.master.volume_db,
            "devices": project.master.devices.iter().map(device_json).collect::<Vec<_>>(),
        },
        "automation": project.automation.iter().map(|lane| lane_json(lane, false)).collect::<Vec<_>>(),
        "audio_output": info.output,
        "file": session.path().map(|p| p.display().to_string()),
        "unsaved_changes": session.is_modified(),
    });
    if !project.markers.is_empty() {
        out["markers"] = project
            .markers
            .iter()
            .map(|marker| json!({ "beat": round(marker.beat), "bar": round(marker.beat / beats_per_bar + 1.0), "name": marker.name }))
            .collect();
    }
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    if !problems.is_empty() {
        out["problems"] = json!(problems);
    }
    // Warped clips play in time but repitched until their audio is ready.
    if session.preparing() > 0 {
        out["preparing_clips"] = json!(session.preparing());
    }
    if let Some(warning) = &info.warning {
        out["audio_warning"] = json!(warning);
    }
    out
}

pub fn param_info_json(info: &ParamInfo) -> Value {
    let mut out = json!({ "id": info.id, "name": info.name });
    let fields = out.as_object_mut().expect("object literal");
    match &info.kind {
        ParamKind::Enum(choices) => {
            fields.insert("choices".into(), json!(choices));
            fields.insert("default".into(), json!(choices.get(info.default as usize)));
        }
        ParamKind::Bool => {
            fields.insert("type".into(), json!("bool"));
            fields.insert("default".into(), json!(info.default >= 0.5));
        }
        kind => {
            if *kind == ParamKind::Int {
                fields.insert("type".into(), json!("int"));
            }
            fields.insert("min".into(), json!(info.min));
            fields.insert("max".into(), json!(info.max));
            fields.insert("default".into(), json!(info.default));
        }
    }
    if !info.unit.is_empty() {
        fields.insert("unit".into(), json!(info.unit));
    }
    out
}

/// A parameter with its current value, shown the way it can be set.
pub fn param_state_json(state: &ParamState) -> Value {
    let mut out = param_info_json(&state.info);
    out["value"] = match &state.info.kind {
        ParamKind::Enum(choices) => json!(choices.get(state.value as usize)),
        ParamKind::Bool => json!(state.value >= 0.5),
        _ => json!(round(state.value)),
    };
    out
}

fn category_name(category: DeviceCategory) -> &'static str {
    match category {
        DeviceCategory::Instrument => "instrument",
        DeviceCategory::Effect => "effect",
    }
}

/// Catalog entry without parameters, for listings.
pub fn descriptor_summary_json(descriptor: &DeviceDescriptor) -> Value {
    let mut out = json!({
        "uri": descriptor.uri,
        "name": descriptor.name,
        "category": category_name(descriptor.category),
        "description": descriptor.description,
    });
    if descriptor.vendor != "Built-in" {
        out["vendor"] = json!(descriptor.vendor);
    }
    // A plugin installed in two formats is listed twice under one name; the
    // format tells the entries apart.
    if let Some((scheme, _)) = trill_core::device::split_uri(&descriptor.uri)
        && scheme != trill_core::device::BUILTIN_SCHEME
    {
        out["format"] = json!(scheme);
    }
    if !descriptor.presets.is_empty() {
        out["presets"] = descriptor
            .presets
            .iter()
            .map(|preset| json!({ "name": preset.name, "description": preset.description }))
            .collect();
    }
    out
}

/// Full catalog entry, with every parameter.
pub fn descriptor_json(descriptor: &DeviceDescriptor) -> Value {
    let mut out = descriptor_summary_json(descriptor);
    if descriptor.params.is_empty() {
        out["params_note"] = json!("Plugin parameters are known once it is added to a track; use get_device then.");
    } else {
        out["params"] = descriptor.params.iter().map(param_info_json).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_beat_is_one_based() {
        assert_eq!(bar_beat(0.0, 4.0), "1.1");
        assert_eq!(bar_beat(5.5, 4.0), "2.2.5");
        assert_eq!(bar_beat(3.0, 3.0), "2.1");
    }
}
