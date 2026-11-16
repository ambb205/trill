//! Tool calls in plain words.
//!
//! The assistant works by calling Trill's tools, and each call is shown in
//! the conversation as one line: what it is doing while it runs, what it did
//! once it has. `create_track {"name": "Bass"}` reads *Added the track
//! "Bass"*. Tracks, clips and devices are called by their names, looked up
//! in the project as it was when the call began; a call to a tool this
//! table does not know reads as its name with the underscores taken out.
//!
//! The arguments and the answer in full are a click away (see the view).

use serde_json::Value;
use trill_assistant::WebLookup;
use trill_core::{ClipId, DeviceId, Project, TrackId};

/// One tool call in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wording {
    /// While it runs: "Adding the track \u{201C}Bass\u{201D}".
    pub doing: String,
    /// Once it has: "Added the track \u{201C}Bass\u{201D}".
    pub did: String,
}

impl Wording {
    fn new(doing: impl Into<String>, did: impl Into<String>) -> Self {
        Self { doing: doing.into(), did: did.into() }
    }

    /// `doing` and `did` followed by the same words: ("Adding", "Added",
    /// "the track \u{201C}Bass\u{201D}").
    fn about(doing: &str, did: &str, what: &str) -> Self {
        Self::new(format!("{doing} {what}"), format!("{did} {what}"))
    }
}

/// A name in the quotation marks names are shown in.
fn quoted(name: &str) -> String {
    format!("\u{201C}{name}\u{201D}")
}

/// Looks things up in the project and in a call's arguments.
struct Call<'a> {
    input: &'a Value,
    project: &'a Project,
}

impl Call<'_> {
    fn text(&self, key: &str) -> Option<&str> {
        self.input.get(key)?.as_str().filter(|text| !text.is_empty())
    }

    fn has(&self, key: &str) -> bool {
        self.input.get(key).is_some_and(|value| !value.is_null())
    }

    /// A number given at the top of the arguments or, for the writing
    /// tools, in their `target`.
    fn id(&self, key: &str) -> Option<u64> {
        let direct = self.input.get(key).and_then(Value::as_u64);
        direct.or_else(|| self.input.get("target")?.get(key)?.as_u64())
    }

    /// The track `key` names: its name in quotes, or "track 7" for one the
    /// project does not have (any more, or yet).
    fn track(&self, key: &str) -> Option<String> {
        let id = TrackId(self.id(key)?);
        Some(self.project.track(id).map_or_else(|_| id.to_string(), |track| quoted(&track.name)))
    }

    fn clip(&self, key: &str) -> Option<String> {
        let id = ClipId(self.id(key)?);
        Some(self.project.clip(id).map_or_else(|_| id.to_string(), |clip| quoted(&clip.name)))
    }

    fn device(&self, key: &str) -> Option<String> {
        let id = DeviceId(self.id(key)?);
        Some(self.project.device(id).map_or_else(|_| id.to_string(), |device| quoted(&device.name)))
    }

    /// Where a writing tool writes: the clip it fills, or the track it puts
    /// a new clip on.
    fn written_to(&self) -> String {
        match (self.clip("clip"), self.track("track")) {
            (Some(clip), _) => format!(" in {clip}"),
            (None, Some(track)) => format!(" on {track}"),
            (None, None) => String::new(),
        }
    }
}

/// What a device is called when only its URI is known: `builtin:reverb` is
/// "reverb", a plugin its id.
fn device_kind(uri: &str) -> &str {
    uri.split_once(':').map_or(uri, |(_, rest)| rest)
}

/// The last part of a path, which is what a file is known by.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().filter(|name| !name.is_empty()).unwrap_or(path)
}

/// "volume", "volume and pan", "volume, pan and mute".
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// `tool`'s name as words: `set_sample` reads "Set sample".
fn spelled_out(tool: &str) -> String {
    let words = tool.replace(['_', '-'], " ");
    let mut letters = words.chars();
    letters.next().map_or_else(|| "A tool".to_owned(), |first| first.to_uppercase().chain(letters).collect())
}

/// An address as it is shown in a line: without the `https://` every
/// address has, and without a slash that only ends it. The whole of it is
/// kept otherwise: where the assistant went is not abridged.
fn address(url: &str) -> &str {
    let bare = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    bare.strip_suffix('/').unwrap_or(bare)
}

/// Says in words what a look at the web is: a search by its query, a page
/// by its address. These are the only things the assistant does that are
/// not Trill's own, so the line always says which and what.
pub fn describe_web(lookup: &WebLookup) -> Wording {
    match lookup {
        WebLookup::Search { query } => {
            Wording::about("Searching", "Searched", &format!("the web for {}", quoted(query)))
        }
        WebLookup::Page { url, .. } => Wording::about("Reading", "Read", address(url)),
    }
}

/// Says in words what calling `tool` with `input` does, naming things as
/// `project` names them.
pub fn describe(tool: &str, input: &Value, project: &Project) -> Wording {
    let call = Call { input, project };
    let about = Wording::about;
    match tool {
        "get_project" => about("Reading", "Read", "the project"),
        "set_project" => {
            let mut changed = Vec::new();
            for (key, what) in [
                ("name", "name"),
                ("tempo", "tempo"),
                ("time_signature", "time signature"),
                ("loop_region", "loop"),
                ("master_volume_db", "master volume"),
            ] {
                if call.has(key) {
                    changed.push(what);
                }
            }
            match (changed.as_slice(), call.input.get("tempo").and_then(Value::as_f64)) {
                (["tempo"], Some(tempo)) => about("Setting", "Set", &format!("the tempo to {tempo} BPM")),
                ([], _) => about("Changing", "Changed", "the project"),
                (changed, _) => about("Changing", "Changed", &format!("the {}", list(changed))),
            }
        }
        "history" => {
            let steps = call.input.get("steps").and_then(Value::as_u64).unwrap_or(1);
            let steps = if steps == 1 { "a step".to_owned() } else { format!("{steps} steps") };
            match call.text("action") {
                Some("redo") => about("Redoing", "Redid", &steps),
                _ => about("Undoing", "Undid", &steps),
            }
        }

        "create_track" => {
            let kind = match call.text("kind") {
                Some("group") => "the group track",
                Some("return") => "the return track",
                _ => "the track",
            };
            let name = call.text("name").map(quoted).unwrap_or_default();
            about("Adding", "Added", format!("{kind} {name}").trim_end())
        }
        "update_track" => {
            let track = call.track("track").unwrap_or_else(|| "a track".to_owned());
            let mut changed = Vec::new();
            for (key, what) in [
                ("volume_db", "volume"),
                ("pan", "pan"),
                ("mute", "mute"),
                ("solo", "solo"),
                ("index", "place"),
                ("group", "group"),
                ("send", "send"),
            ] {
                if call.has(key) {
                    changed.push(what);
                }
            }
            match (call.text("name"), changed.as_slice()) {
                (Some(name), []) => about("Renaming", "Renamed", &format!("{track} to {}", quoted(name))),
                (_, []) => about("Changing", "Changed", &track),
                (_, changed) => about("Changing", "Changed", &format!("the {} of {track}", list(changed))),
            }
        }
        "delete_track" => {
            about("Deleting", "Deleted", &format!("the track {}", call.track("track").unwrap_or_default()))
        }

        "list_devices" => about("Looking through", "Looked through", "the devices"),
        "describe_device" => {
            about("Reading about", "Read about", call.text("uri").map_or("a device", |uri| device_kind(uri)))
        }
        "add_device" => {
            let device = call.text("uri").map_or("a device", |uri| device_kind(uri));
            let owner = call.track("track").unwrap_or_else(|| "the master".to_owned());
            about("Adding", "Added", &format!("{device} to {owner}"))
        }
        "get_device" => about("Reading", "Read", &call.device("device").unwrap_or_else(|| "a device".to_owned())),
        "update_device" => {
            let device = call.device("device").unwrap_or_else(|| "a device".to_owned());
            match (call.text("preset"), call.input.get("enabled").and_then(Value::as_bool), call.has("params")) {
                (Some(preset), _, _) => about("Setting", "Set", &format!("{device} to the preset {preset}")),
                (None, Some(true), false) => about("Switching on", "Switched on", &device),
                (None, Some(false), false) => about("Bypassing", "Bypassed", &device),
                _ => about("Adjusting", "Adjusted", &device),
            }
        }
        "delete_device" => {
            about("Removing", "Removed", &call.device("device").unwrap_or_else(|| "a device".to_owned()))
        }
        "search_presets" => {
            let sound = call.text("query").map(quoted).unwrap_or_else(|| "a sound".to_owned());
            about("Looking for", "Looked for", &format!("{sound} among the presets"))
        }
        "save_preset" => {
            let device = call.device("device").unwrap_or_else(|| "a device".to_owned());
            let name = call.text("name").map(quoted).unwrap_or_default();
            about("Saving", "Saved", format!("the sound of {device} as the preset {name}").trim_end())
        }
        "show_device_editor" => {
            let device = call.device("device").unwrap_or_else(|| "a device".to_owned());
            match call.input.get("open").and_then(Value::as_bool) {
                Some(false) => about("Closing", "Closed", &format!("the editor of {device}")),
                _ => about("Opening", "Opened", &format!("the editor of {device}")),
            }
        }

        "create_clip" | "create_audio_clip" => {
            let name = match (call.text("name"), call.text("path")) {
                (Some(name), _) => format!("the clip {}", quoted(name)),
                (None, Some(path)) => format!("the audio clip {}", quoted(file_name(path))),
                (None, None) => "a clip".to_owned(),
            };
            let track = call.track("track").map(|track| format!(" to {track}")).unwrap_or_default();
            about("Adding", "Added", &format!("{name}{track}"))
        }
        "update_clip" => {
            let clip = call.clip("clip").unwrap_or_else(|| "a clip".to_owned());
            match (call.text("name"), call.input.get("muted").and_then(Value::as_bool)) {
                (Some(name), _) => about("Renaming", "Renamed", &format!("{clip} to {}", quoted(name))),
                (None, Some(true)) => about("Muting", "Muted", &format!("the clip {clip}")),
                (None, Some(false)) => about("Unmuting", "Unmuted", &format!("the clip {clip}")),
                (None, None) => about("Changing", "Changed", &format!("the clip {clip}")),
            }
        }
        "split_clip" => about("Splitting", "Split", &call.clip("clip").unwrap_or_else(|| "a clip".to_owned())),
        "duplicate_clip" => {
            about("Duplicating", "Duplicated", &call.clip("clip").unwrap_or_else(|| "a clip".to_owned()))
        }
        "delete_clip" => about("Deleting", "Deleted", &format!("the clip {}", call.clip("clip").unwrap_or_default())),

        "get_notes" => about("Reading", "Read", &format!("the notes of {}", call.clip("clip").unwrap_or_default())),
        "add_notes" | "set_notes" => {
            let count = call.input.get("notes").and_then(Value::as_array).map_or(0, Vec::len);
            let notes = if count == 1 { "1 note".to_owned() } else { format!("{count} notes") };
            let clip = call.clip("clip").unwrap_or_else(|| "a clip".to_owned());
            if tool == "add_notes" {
                about("Adding", "Added", &format!("{notes} to {clip}"))
            } else {
                about("Rewriting", "Rewrote", &format!("{clip} with {notes}"))
            }
        }
        "remove_notes" => {
            about("Removing", "Removed", &format!("notes from {}", call.clip("clip").unwrap_or_default()))
        }
        "transform_notes" => {
            let notes = format!("the notes of {}", call.clip("clip").unwrap_or_default());
            let transpose = call.input.get("transpose").and_then(Value::as_i64).filter(|by| *by != 0);
            if let Some(by) = transpose {
                about("Transposing", "Transposed", &format!("{notes} by {by:+}"))
            } else if call.has("quantize") {
                about("Quantizing", "Quantized", &notes)
            } else if call.has("humanize_timing") || call.has("humanize_velocity") {
                about("Humanizing", "Humanized", &notes)
            } else {
                about("Changing", "Changed", &notes)
            }
        }

        "write_chords" => about("Writing", "Wrote", &format!("chords{}", call.written_to())),
        "write_drums" => about("Writing", "Wrote", &format!("drums{}", call.written_to())),
        "write_arpeggio" => about("Writing", "Wrote", &format!("an arpeggio{}", call.written_to())),
        "write_bass" => about("Writing", "Wrote", &format!("a bass line{}", call.written_to())),
        "write_melody" => about("Writing", "Wrote", &format!("a melody{}", call.written_to())),
        "analyze_music" => {
            let of = match (call.clip("clip"), call.track("track")) {
                (Some(clip), _) => format!(" of {clip}"),
                (None, Some(track)) => format!(" of {track}"),
                (None, None) => " of the song".to_owned(),
            };
            about("Reading", "Read", &format!("the key and chords{of}"))
        }
        "music_reference" => about("Looking up", "Looked up", call.text("topic").unwrap_or("music theory")),

        "set_automation" | "get_automation" => {
            let target = call.input.get("target");
            let kind = target.and_then(|target| target.get("type")).and_then(Value::as_str);
            let target_call = Call { input: target.unwrap_or(&Value::Null), project };
            let of = match kind {
                Some("track_volume") => format!("the volume of {}", target_call.track("track").unwrap_or_default()),
                Some("track_pan") => format!("the pan of {}", target_call.track("track").unwrap_or_default()),
                Some("master_volume") => "the master volume".to_owned(),
                Some("device_param") => {
                    let param = target_call.text("param").unwrap_or("a parameter");
                    format!("{param} of {}", target_call.device("device").unwrap_or_default())
                }
                _ => String::new(),
            };
            let of = of.trim_end();
            match (tool, of.is_empty()) {
                ("set_automation", true) => about("Writing", "Wrote", "automation"),
                ("set_automation", false) => about("Automating", "Automated", of),
                (_, true) => about("Reading", "Read", "the automation"),
                (_, false) => about("Reading", "Read", &format!("the automation of {of}")),
            }
        }
        "set_markers" => about("Setting", "Set", "the markers"),
        "edit_time" => match call.text("action") {
            Some("delete") => about("Deleting", "Deleted", "a stretch of time"),
            Some("duplicate") => about("Repeating", "Repeated", "a stretch of time"),
            Some("insert") => about("Opening up", "Opened up", "empty time"),
            _ => about("Editing", "Edited", "the timeline"),
        },

        "transport" => match call.text("action") {
            Some("play") => about("Starting", "Started", "playback"),
            Some("stop") => about("Stopping", "Stopped", "playback"),
            Some("seek") => about("Moving", "Moved", "the playhead"),
            Some("panic") => about("Silencing", "Silenced", "everything"),
            _ => about("Checking", "Checked", "the transport"),
        },
        "render" => match call.text("path") {
            Some(path) => about("Rendering", "Rendered", &format!("the song to {}", quoted(file_name(path)))),
            None => about("Rendering", "Rendered", "the song"),
        },

        "save_project" => match call.text("path") {
            Some(path) => about("Saving", "Saved", &format!("the project as {}", quoted(file_name(path)))),
            None => about("Saving", "Saved", "the project"),
        },
        "load_project" => match call.text("path") {
            Some(path) => about("Opening", "Opened", &quoted(file_name(path))),
            None => about("Opening", "Opened", "a project"),
        },
        "new_project" => match call.text("name") {
            Some(name) => about("Starting", "Started", &format!("a new project, {}", quoted(name))),
            None => about("Starting", "Started", "a new project"),
        },
        "recover_project" => match call.text("action") {
            Some("open") => about("Bringing back", "Brought back", "a project that was not saved"),
            Some("discard") => about("Discarding", "Discarded", "a project that was not saved"),
            _ => about("Looking for", "Looked for", "projects that were not saved"),
        },
        "import_midi" => match call.text("path") {
            Some(path) => about("Importing", "Imported", &quoted(file_name(path))),
            None => about("Importing", "Imported", "a MIDI file"),
        },
        "export_midi" => match call.text("path") {
            Some(path) => about("Exporting", "Exported", &format!("the song as {}", quoted(file_name(path)))),
            None => about("Exporting", "Exported", "the song as MIDI"),
        },

        "search_sounds" => match call.text("text") {
            Some(text) => about("Searching", "Searched", &format!("the sounds for {}", quoted(text))),
            None => about("Searching", "Searched", "the sounds"),
        },
        "library" => match call.text("action") {
            Some("add_folder") => about("Adding", "Added", "a folder to the sound library"),
            Some("remove_folder") => about("Taking", "Took", "a folder out of the sound library"),
            Some("rescan") => about("Scanning", "Scanned", "the sound library"),
            _ => about("Checking", "Checked", "the sound library"),
        },
        "preview_sound" => match call.text("path") {
            Some(path) => about("Playing", "Played", &quoted(file_name(path))),
            None => about("Stopping", "Stopped", "the preview"),
        },
        "set_sample" => {
            let device = call.device("device").unwrap_or_else(|| "a device".to_owned());
            match call.text("path") {
                Some(path) => about("Loading", "Loaded", &format!("{} into {device}", quoted(file_name(path)))),
                None => about("Changing", "Changed", &format!("the sample of {device}")),
            }
        }

        other => {
            let words = spelled_out(other);
            Wording::new(words.clone(), words)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trill_core::ChainRef;

    fn song() -> (Project, u64, u64, u64) {
        let mut project = Project::new("t");
        let track = project.add_track("Bass");
        let device = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Poly Synth", None).unwrap();
        let clip = project.add_midi_clip(track, "Verse", 0.0, 4.0).unwrap();
        (project, track.0, clip.0, device.0)
    }

    fn did(tool: &str, input: Value, project: &Project) -> String {
        describe(tool, &input, project).did
    }

    #[test]
    fn calls_read_as_what_they_do_to_named_things() {
        let (project, track, clip, device) = song();
        let p = &project;
        let wording = describe("create_track", &json!({ "name": "Drums", "instrument": "builtin:drumkit" }), p);
        assert_eq!(wording.doing, "Adding the track \u{201C}Drums\u{201D}");
        assert_eq!(wording.did, "Added the track \u{201C}Drums\u{201D}");

        assert_eq!(did("get_project", json!({}), p), "Read the project");
        assert_eq!(did("set_project", json!({ "tempo": 96 }), p), "Set the tempo to 96 BPM");
        assert_eq!(did("set_project", json!({ "tempo": 96, "name": "x" }), p), "Changed the name and tempo");
        assert_eq!(
            did("create_track", json!({ "name": "Verb", "kind": "return" }), p),
            "Added the return track \u{201C}Verb\u{201D}"
        );
        assert_eq!(
            did("update_track", json!({ "track": track, "volume_db": -6, "pan": 0.2, "mute": true }), p),
            "Changed the volume, pan and mute of \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            did("update_track", json!({ "track": track, "name": "Sub" }), p),
            "Renamed \u{201C}Bass\u{201D} to \u{201C}Sub\u{201D}"
        );
        assert_eq!(did("delete_track", json!({ "track": track }), p), "Deleted the track \u{201C}Bass\u{201D}");
        assert_eq!(
            did("add_device", json!({ "track": track, "uri": "builtin:reverb" }), p),
            "Added reverb to \u{201C}Bass\u{201D}"
        );
        assert_eq!(did("add_device", json!({ "uri": "builtin:eq" }), p), "Added eq to the master");
        assert_eq!(
            did("update_device", json!({ "device": device, "params": { "cutoff": 900 } }), p),
            "Adjusted \u{201C}Poly Synth\u{201D}"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "preset": "warm_pad" }), p),
            "Set \u{201C}Poly Synth\u{201D} to the preset warm_pad"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "enabled": false }), p),
            "Bypassed \u{201C}Poly Synth\u{201D}"
        );
        assert_eq!(
            did("create_clip", json!({ "track": track, "start": 0, "length": 8, "name": "Chorus" }), p),
            "Added the clip \u{201C}Chorus\u{201D} to \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            did("create_audio_clip", json!({ "track": track, "path": "D:\\Samples\\Kick 01.wav", "start": 0 }), p),
            "Added the audio clip \u{201C}Kick 01.wav\u{201D} to \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            did("add_notes", json!({ "clip": clip, "notes": [[60, 0, 1, 100], [64, 1, 1, 100]] }), p),
            "Added 2 notes to \u{201C}Verse\u{201D}"
        );
        assert_eq!(
            did("set_notes", json!({ "clip": clip, "notes": [[60, 0, 1, 100]] }), p),
            "Rewrote \u{201C}Verse\u{201D} with 1 note"
        );
        assert_eq!(
            did("transform_notes", json!({ "clip": clip, "transpose": -12 }), p),
            "Transposed the notes of \u{201C}Verse\u{201D} by -12"
        );
        assert_eq!(
            did("transform_notes", json!({ "clip": clip, "quantize": 0.25 }), p),
            "Quantized the notes of \u{201C}Verse\u{201D}"
        );
        assert_eq!(
            did("write_drums", json!({ "target": { "track": track, "bars": 2 }, "groove": "house" }), p),
            "Wrote drums on \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            did("write_chords", json!({ "clip": clip, "progression": "Am | F" }), p),
            "Wrote chords in \u{201C}Verse\u{201D}"
        );
        assert_eq!(
            did(
                "set_automation",
                json!({ "target": { "type": "device_param", "device": device, "param": "cutoff" }, "points": [] }),
                p
            ),
            "Automated cutoff of \u{201C}Poly Synth\u{201D}"
        );
        assert_eq!(
            did("set_automation", json!({ "target": { "type": "track_volume", "track": track } }), p),
            "Automated the volume of \u{201C}Bass\u{201D}"
        );
        assert_eq!(did("history", json!({ "action": "undo", "steps": 3 }), p), "Undid 3 steps");
        assert_eq!(
            did("save_project", json!({ "path": "C:\\Songs\\Night Drive.trill" }), p),
            "Saved the project as \u{201C}Night Drive.trill\u{201D}"
        );
        assert_eq!(did("save_project", json!({}), p), "Saved the project");
        assert_eq!(did("load_project", json!({ "path": "songs/day.trill" }), p), "Opened \u{201C}day.trill\u{201D}");
        assert_eq!(did("render", json!({ "path": "mix.wav" }), p), "Rendered the song to \u{201C}mix.wav\u{201D}");
        assert_eq!(did("render", json!({}), p), "Rendered the song");
        assert_eq!(did("import_midi", json!({ "path": "D:/midi/riff.mid" }), p), "Imported \u{201C}riff.mid\u{201D}");
        assert_eq!(did("export_midi", json!({ "path": "out.mid" }), p), "Exported the song as \u{201C}out.mid\u{201D}");
        assert_eq!(
            did("recover_project", json!({ "action": "open", "index": 0 }), p),
            "Brought back a project that was not saved"
        );
        assert_eq!(did("new_project", json!({ "name": "Sketch" }), p), "Started a new project, \u{201C}Sketch\u{201D}");
        assert_eq!(did("transport", json!({ "action": "play" }), p), "Started playback");
        assert_eq!(
            did("search_sounds", json!({ "text": "deep kick" }), p),
            "Searched the sounds for \u{201C}deep kick\u{201D}"
        );
        assert_eq!(
            did("edit_time", json!({ "action": "duplicate", "start": 0, "end": 16 }), p),
            "Repeated a stretch of time"
        );
    }

    #[test]
    fn a_look_at_the_web_reads_as_the_query_or_the_address() {
        let search = describe_web(&WebLookup::Search { query: "dorian mode chord progressions".into() });
        assert_eq!(search.doing, "Searching the web for \u{201C}dorian mode chord progressions\u{201D}");
        assert_eq!(search.did, "Searched the web for \u{201C}dorian mode chord progressions\u{201D}");

        let page = |url: &str| describe_web(&WebLookup::Page { url: url.into(), question: "What is it?".into() });
        assert_eq!(page("https://example.com/theory/dorian").doing, "Reading example.com/theory/dorian");
        assert_eq!(page("https://example.com/").did, "Read example.com");
        // Anything out of the ordinary about an address stays in sight.
        assert_eq!(page("http://127.0.0.1:7777/mcp").did, "Read 127.0.0.1:7777/mcp");
        assert_eq!(page("ftp://example.com/x").did, "Read ftp://example.com/x");
        assert_eq!(page("https://example.com/a?b=c#d").did, "Read example.com/a?b=c#d");
    }

    #[test]
    fn what_is_not_in_the_project_goes_by_its_id_and_unknown_tools_by_their_name() {
        let (project, ..) = song();
        assert_eq!(did("delete_clip", json!({ "clip": 999 }), &project), "Deleted the clip clip 999");
        assert_eq!(did("update_track", json!({ "track": 42, "solo": true }), &project), "Changed the solo of track 42");
        assert_eq!(did("get_device", json!({}), &project), "Read a device");

        let wording = describe("freeze_track", &json!({ "track": 1 }), &project);
        assert_eq!((wording.doing.as_str(), wording.did.as_str()), ("Freeze track", "Freeze track"));
        assert_eq!(describe("", &Value::Null, &project).did, "A tool");
        // Arguments of the wrong shape never break a line.
        assert_eq!(did("create_track", json!("nonsense"), &project), "Added the track");
        assert_eq!(did("add_notes", json!({ "clip": "seven", "notes": 3 }), &project), "Added 0 notes to a clip");
    }

    #[test]
    fn every_tool_the_assistant_is_offered_has_words_of_its_own() {
        let (project, track, clip, device) = song();
        let input = json!({ "track": track, "clip": clip, "device": device });
        // The tools in the README's table.
        for tool in [
            "get_project",
            "set_project",
            "new_project",
            "save_project",
            "load_project",
            "recover_project",
            "import_midi",
            "export_midi",
            "render",
            "history",
            "create_track",
            "update_track",
            "delete_track",
            "list_devices",
            "describe_device",
            "add_device",
            "get_device",
            "update_device",
            "delete_device",
            "search_presets",
            "save_preset",
            "show_device_editor",
            "create_clip",
            "create_audio_clip",
            "update_clip",
            "split_clip",
            "duplicate_clip",
            "delete_clip",
            "search_sounds",
            "library",
            "preview_sound",
            "set_sample",
            "get_notes",
            "add_notes",
            "set_notes",
            "remove_notes",
            "transform_notes",
            "write_chords",
            "write_drums",
            "write_arpeggio",
            "write_bass",
            "write_melody",
            "analyze_music",
            "music_reference",
            "set_automation",
            "get_automation",
            "set_markers",
            "edit_time",
            "transport",
        ] {
            let wording = describe(tool, &input, &project);
            assert_ne!(wording.did, spelled_out(tool), "{tool} has no words of its own");
            assert_ne!(wording.doing, wording.did, "{tool} reads the same before and after");
            assert!(!wording.did.contains("  ") && !wording.did.ends_with(' '), "{tool}: {:?}", wording.did);
        }
    }
}
