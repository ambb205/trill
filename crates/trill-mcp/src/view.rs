//! JSON views of the project for tool replies. Replies are shaped for a
//! reader that pays per token: compact, but with the ids and positions it
//! needs to plan its next call.

use crate::input::controller_scale;
use serde_json::{Value, json};
use trill_core::ControllerCurve;
use trill_core::device::{DeviceCategory, DeviceDescriptor, ParamInfo, ParamKind};
use trill_core::pitch::pitch_name;
use trill_core::presets::{CATEGORY_SEPARATOR, PresetInfo, PresetList, PresetQuery, PresetStatus};
use trill_core::project::{AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, Segment, WarpMode};
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

/// A controller's curve in a clip: what it is called, and its points as
/// `[beat, value, shape?]` in the values tools take (0 to 127; the pitch
/// wheel -1 to 1).
pub fn controller_json(curve: &ControllerCurve, include_points: bool) -> Value {
    let kind = curve.controller;
    let scale = controller_scale(kind);
    let values = || curve.points.iter().map(|point| point.value * scale);
    let (low, high) = (values().fold(f64::INFINITY, f64::min), values().fold(f64::NEG_INFINITY, f64::max));
    let mut out = json!({
        "controller": kind.key(),
        "point_count": curve.points.len(),
        "range": format!("{}..{}", round(low), round(high)),
    });
    if let (Some(number), true) = (kind.number(), kind.name().is_some()) {
        out["number"] = json!(number);
    }
    if include_points {
        out["points"] = curve
            .points
            .iter()
            .map(|point| {
                let (beat, value) = (round(point.beat), round(point.value * scale));
                match point.shape {
                    // A pedal steps unless it says otherwise, so that is
                    // what needs no saying.
                    Segment::Hold if kind.is_switch() => json!([beat, value]),
                    Segment::Linear if kind.is_switch() => json!([beat, value, "linear"]),
                    Segment::Linear => json!([beat, value]),
                    Segment::Hold => json!([beat, value, "hold"]),
                    Segment::Curve(bend) => json!([beat, value, round(bend as f64)]),
                }
            })
            .collect();
    }
    out
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
            if !midi.controllers.is_empty() {
                let curves = midi.controllers.iter().map(|curve| controller_json(curve, include_notes));
                fields.insert("controllers".into(), curves.collect());
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
    // The track it listens to in place of its own input.
    if let Some(track) = device.sidechain {
        out["sidechain"] = json!(track);
    }
    out
}

/// The devices of a chain as a song's overview lists them, with a mark on
/// each that takes one channel: from there on left and right are the same,
/// which is worth seeing without asking about every device.
pub fn chain_json(session: &Session, devices: &[Device]) -> Vec<Value> {
    let loading = session.loading();
    let listed = devices.iter().map(|device| {
        let mut out = device_json(device);
        if let Ok(channels) = session.device_channels(device.id)
            && channels.folds_input()
        {
            out["channels"] = json!(channels.describe());
        }
        if loading.contains(&device.id) {
            out["loading"] = json!(true);
        }
        out
    });
    listed.collect()
}

/// What to say of the plugins that are still loading what they play, if
/// any are: which, and what that means for playing them live.
pub fn loading_note(session: &Session) -> Option<String> {
    let project = session.project();
    let named =
        |id: &trill_core::DeviceId| project.device(*id).ok().map(|device| format!("{} (device {})", device.name, id.0));
    let names: Vec<String> = session.loading().iter().filter_map(named).collect();
    (!names.is_empty()).then(|| {
        format!(
            "Still loading what they play: {}. Played live now their notes may be silent; get_project shows when \
             they have done. probe, measure and render wait for them by themselves.",
            names.join(", ")
        )
    })
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
        AutomationTarget::Send { track, to } => json!({ "type": "send", "track": track, "to": to }),
    }
}

/// An envelope: what it moves, how many points it has and what its lines
/// are straight along (`fader`, `log` or `linear`). `points` are the ones
/// to list, as `[beat, value, shape?]`; `None` lists none.
pub fn lane_json(lane: &AutomationLane, points: Option<&[AutomationPoint]>) -> Value {
    let mut out = json!({
        "target": target_json(&lane.target),
        "point_count": lane.points.len(),
        "travel": lane.travel.name(),
    });
    if !lane.enabled {
        out["enabled"] = json!(false);
    }
    if let Some(points) = points {
        out["points"] = points
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

/// What `track` sends to the returns: the sends set by hand, each marked
/// if an envelope moves it, and the sends that are an envelope alone.
/// `None` for a track that sends nothing.
pub fn sends_json(project: &Project, track: &Track) -> Option<Value> {
    let moved = |to| {
        let target = AutomationTarget::Send { track: track.id, to };
        project.automation.iter().find(|lane| lane.target == target).map(|lane| lane.enabled)
    };
    let mark = |mut send: Value, to| {
        match moved(to) {
            Some(true) => send["automated"] = json!(true),
            // Its envelope is switched off: the level set by hand counts.
            Some(false) => send["automation_off"] = json!(true),
            None => {}
        }
        send
    };
    let by_hand = track.sends.iter().map(|send| mark(json!({ "to": send.to, "level_db": send.level_db }), send.to));
    let by_envelope = project.automation.iter().filter_map(|lane| match lane.target {
        AutomationTarget::Send { track: from, to }
            if from == track.id && !track.sends.iter().any(|send| send.to == to) =>
        {
            Some(mark(json!({ "to": to }), to))
        }
        _ => None,
    });
    let sends: Vec<Value> = by_hand.chain(by_envelope).collect();
    (!sends.is_empty()).then(|| json!(sends))
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
        .map(|clip| {
            json!({
                "track_id": clip.track,
                "clip_id": clip.clip,
                "notes": clip.notes,
                "controllers": clip.controllers.iter().map(|kind| kind.key()).collect::<Vec<_>>(),
                "new_clip": clip.created,
            })
        })
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
                if let Some(sends) = sends_json(project, track) {
                    out["sends"] = sends;
                }
                if session.is_armed(track.id) {
                    out["armed"] = json!(true);
                }
                out["devices"] = json!(chain_json(session, &track.devices));
                crate::instruments::describe_track(session, track.id, &mut out);
                out
            })
            .collect::<Vec<_>>(),
        "master": {
            "volume_db": project.master.volume_db,
            "devices": chain_json(session, &project.master.devices),
        },
        "automation": project.automation.iter().map(|lane| lane_json(lane, None)).collect::<Vec<_>>(),
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
    if let Some(note) = loading_note(session) {
        out["loading"] = json!(note);
    }
    // Files that are not on disk are said once, with what to do about
    // them, in place of a line for every clip and pad that names one.
    let missing = trill_runtime::files_missing_in(session);
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    let problems = crate::files::without_missing(problems, &missing);
    if !problems.is_empty() {
        out["problems"] = json!(problems);
    }
    if let Some(brief) = crate::files::brief(&missing) {
        out["missing_files"] = brief;
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

/// What a plugin shows for a parameter beyond its current value, in its
/// own words: its own default, and what values there are.
pub struct ParamDetail {
    pub default: String,
    pub span: ParamSpan,
}

/// The values a plugin's parameter can take, as the plugin shows them.
pub enum ParamSpan {
    /// Every step of a parameter with a few steps, from the lowest.
    Choices(Vec<String>),
    /// The two ends of a range.
    Range(String, String),
}

/// A plugin's parameter with its current value. The number says little
/// (most are 0 to 1, whatever they stand for), so it comes with the text
/// the plugin shows for it, and, when a few parameters were asked about,
/// with what the plugin shows at the ends of the range and every choice
/// there is.
pub fn plugin_param_json(state: &ParamState, text: &str, detail: Option<&ParamDetail>) -> Value {
    let info = &state.info;
    // Finer than other numbers in a reply: a thousandth of a knob's travel
    // can be a semitone, and the value is there to be written back.
    let value = (state.value * 1e6).round() / 1e6;
    let mut out = json!({ "id": info.id, "name": info.name, "value": value, "text": text });
    if info.max != 1.0 || info.min != 0.0 {
        out["max"] = json!(info.max);
        if info.min != 0.0 {
            out["min"] = json!(info.min);
        }
    }
    if let Some(detail) = detail {
        match &detail.span {
            ParamSpan::Choices(choices) => out["choices"] = json!(choices),
            ParamSpan::Range(low, high) => {
                out["from"] = json!(low);
                out["to"] = json!(high);
            }
        }
        if detail.default != text {
            out["default"] = json!(detail.default);
        }
    }
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

/// Which of a device's presets a reply lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresetSearch<'a> {
    pub query: Option<&'a str>,
    pub category: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

/// The longest description a preset is listed with, in characters, and
/// the most tags.
const PRESET_DESCRIPTION: usize = 140;
const PRESET_TAGS: usize = 6;
/// The most kinds of preset a reply sums up.
const PRESET_KINDS: usize = 30;

fn preset_json(preset: &PresetInfo) -> Value {
    let mut out = json!({ "name": preset.name });
    if !preset.category.is_empty() {
        out["category"] = json!(preset.category);
    }
    if !preset.author.is_empty() {
        out["author"] = json!(preset.author);
    }
    if !preset.tags.is_empty() {
        out["tags"] = preset.tags.iter().take(PRESET_TAGS).map(|tag| json!(tag)).collect();
    }
    if !preset.description.is_empty() {
        let description: String = preset.description.chars().take(PRESET_DESCRIPTION).collect();
        out["description"] = json!(description);
    }
    // A built-in preset is asked for by its name; any other has an id
    // that tells it from its namesakes.
    if preset.id != preset.name {
        out["id"] = json!(preset.id);
    }
    out
}

/// One page of a device's presets, with what it takes to get at the
/// rest: how many there are, how many match, and where the next page
/// starts. A list of thousands costs a reply no more than a list of ten.
pub fn presets_json(list: &PresetList, search: &PresetSearch) -> Value {
    let text = search.query.unwrap_or_default();
    let query = PresetQuery { text, category: search.category, offset: search.offset, limit: search.limit };
    let page = list.search(&query);
    let narrowed = !text.trim().is_empty() || search.category.is_some_and(|category| !category.trim().is_empty());

    let mut out = json!({ "total": list.len() });
    if narrowed {
        out["matching"] = json!(page.total);
    }
    match &list.status {
        PresetStatus::Ready => {}
        PresetStatus::Reading => {
            out["status"] = json!(format!(
                "still being read ({} found so far); ask again in a few seconds for the rest",
                list.len()
            ));
        }
        PresetStatus::Failed(reason) => {
            out["status"] = json!(format!("could not all be read: {reason}"));
        }
    }
    if page.offset > 0 {
        out["offset"] = json!(page.offset);
    }
    out["items"] = page.presets.iter().map(|preset| preset_json(preset)).collect();
    let shown = page.offset + page.presets.len();
    if shown < page.total {
        let narrow = if narrowed { "" } else { "; preset_query (\"warm pad\") or preset_category narrows them down" };
        out["more"] = json!(format!("{} more: preset_offset {shown} lists the next{narrow}", page.total - shown));
        if !narrowed {
            out["kinds"] = preset_kinds(list);
        }
    }
    if list.is_empty() && list.status == PresetStatus::Ready {
        out["note"] = json!("This device offers no presets that can be listed.");
    }
    out
}

/// What kinds of preset a list has, most numerous first: the last part of
/// each category, which is what tells a pad from a bass whatever bank
/// either is in, with how many presets are filed under it.
fn preset_kinds(list: &PresetList) -> Value {
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for (category, count) in list.categories() {
        let Some(kind) = category.rsplit(CATEGORY_SEPARATOR).next().filter(|kind| !kind.is_empty()) else { continue };
        match kinds.iter_mut().find(|(known, _)| known.eq_ignore_ascii_case(kind)) {
            Some((_, total)) => *total += count,
            None => kinds.push((kind.to_string(), count)),
        }
    }
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    kinds.iter().take(PRESET_KINDS).map(|(kind, count)| json!(format!("{kind} ({count})"))).collect()
}

/// Full catalog entry, with every parameter. Its presets are listed by
/// [`presets_json`], a page at a time.
pub fn descriptor_json(descriptor: &DeviceDescriptor) -> Value {
    let mut out = descriptor_summary_json(descriptor);
    if let Some(fields) = out.as_object_mut() {
        fields.remove("presets");
    }
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

    fn preset(name: &str, category: &str) -> PresetInfo {
        PresetInfo {
            id: trill_core::presets::short_id(&format!("{category}/{name}")),
            name: name.into(),
            category: category.into(),
            ..PresetInfo::default()
        }
    }

    /// A list the size of a big synth's: a hundred presets in each of
    /// thirty folders of two banks.
    fn big() -> PresetList {
        let kinds = ["Pads", "Leads", "Basses", "Keys", "Plucks", "FX"];
        let presets = (0..3000).map(|n| {
            let bank = if n % 2 == 0 { "Factory" } else { "Third Party/Somebody" };
            let kind = kinds[(n / 2) % kinds.len()];
            preset(&format!("{} {n}", kind.trim_end_matches('s')), &format!("{bank}/{kind}"))
        });
        PresetList::ready(presets.collect::<Vec<_>>())
    }

    #[test]
    fn a_big_list_of_presets_is_a_small_reply() {
        let list = big();
        let first = presets_json(&list, &PresetSearch { limit: 20, ..PresetSearch::default() });
        assert_eq!((first["total"].as_u64(), first["items"].as_array().unwrap().len()), (Some(3000), 20));
        assert!(first.get("matching").is_none() && first.get("status").is_none());
        assert!(first["more"].as_str().unwrap().starts_with("2980 more: preset_offset 20 lists the next;"));
        // What there is to choose from, whatever bank it is in.
        assert_eq!(first["kinds"][0], "Basses (500)");
        assert_eq!(first["kinds"].as_array().unwrap().len(), 6);
        assert!(first.to_string().len() < 3000, "{} bytes", first.to_string().len());

        let item = &first["items"][0];
        assert_eq!((item["name"].as_str(), item["category"].as_str()), (Some("Pad 0"), Some("Factory/Pads")));
        assert_eq!(item["id"].as_str().map(str::len), Some(10));
        assert!(item.get("author").is_none() && item.get("tags").is_none() && item.get("description").is_none());

        // Searched, it says how many match and where the next page is.
        let search = PresetSearch { query: Some("pad 1"), category: Some("factory"), offset: 0, limit: 5 };
        let pads = presets_json(&list, &search);
        assert_eq!((pads["total"].as_u64(), pads["items"].as_array().unwrap().len()), (Some(3000), 5));
        let matching = pads["matching"].as_u64().unwrap();
        assert!(matching > 5 && matching < 300, "{matching}");
        assert_eq!(pads["more"], format!("{} more: preset_offset 5 lists the next", matching - 5));
        assert!(pads.get("kinds").is_none());
        let next = presets_json(&list, &PresetSearch { offset: 5, ..search });
        assert_eq!(next["offset"], 5);
        assert_ne!(next["items"][0], pads["items"][0]);
        // The last page says nothing of more.
        let last = presets_json(&list, &PresetSearch { offset: matching as usize - 2, ..search });
        assert_eq!(last["items"].as_array().unwrap().len(), 2);
        assert!(last.get("more").is_none());
    }

    #[test]
    fn a_list_says_how_far_along_it_is_and_a_built_in_preset_needs_no_id() {
        let named = PresetInfo {
            id: "warm_pad".into(),
            name: "warm_pad".into(),
            description: "x".repeat(400),
            tags: (0..20).map(|n| format!("tag{n}")).collect(),
            author: "Ada".into(),
            ..PresetInfo::default()
        };
        let all = PresetSearch { limit: 20, ..PresetSearch::default() };
        let ready = presets_json(&PresetList::ready(vec![named.clone()]), &all);
        let item = &ready["items"][0];
        assert!(item.get("id").is_none() && item.get("category").is_none());
        assert_eq!(item["description"].as_str().unwrap().len(), PRESET_DESCRIPTION);
        assert_eq!(item["tags"].as_array().unwrap().len(), PRESET_TAGS);
        assert_eq!(item["author"], "Ada");
        assert!(ready.get("more").is_none() && ready.get("kinds").is_none() && ready.get("note").is_none());

        let reading = presets_json(&PresetList::reading(vec![named.clone()]), &all);
        assert!(reading["status"].as_str().unwrap().starts_with("still being read (1 found so far)"));
        let failed =
            PresetList { status: PresetStatus::Failed("the plugin crashed".into()), presets: vec![named].into() };
        assert_eq!(presets_json(&failed, &all)["status"], "could not all be read: the plugin crashed");
        assert_eq!(presets_json(&failed, &all)["items"].as_array().unwrap().len(), 1);

        let none = presets_json(&PresetList::default(), &all);
        assert_eq!((none["total"].as_u64(), none["note"].is_string()), (Some(0), true));
        assert!(presets_json(&PresetList::reading(Vec::new()), &all).get("note").is_none());
    }

    #[test]
    fn bar_beat_is_one_based() {
        assert_eq!(bar_beat(0.0, 4.0), "1.1");
        assert_eq!(bar_beat(5.5, 4.0), "2.2.5");
        assert_eq!(bar_beat(3.0, 3.0), "2.1");
    }
}
