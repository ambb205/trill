//! Tool calls in plain words, and what each says it made.
//!
//! Whoever works through this server does so by calling its tools, and
//! each call can be said in a line: what it is doing while it runs, what it
//! did once it has. `create_track {"name": "Bass"}` reads *Added the track
//! "Bass"*. Tracks, clips and devices are called by their names, looked up
//! in the project as it was when the call began; a call to a tool this
//! table does not know reads as its name with the underscores taken out.
//!
//! Three things read these lines, which is why they are here, beside the
//! tools themselves: the journal of a session, in which every call is an
//! entry under its words; the server, which enters them; and the window,
//! whose assistant panel shows a conversation's calls as they are made.
//!
//! [`of_call`] reads where in the song a call landed, from its arguments
//! (`update_clip {"clip": 7}`) or its answer (`create_track` answers
//! `{"track_id": 9}`): a track, some clips, a device, an envelope, a
//! stretch of the timeline. [`heard`] reads, from the answer of a call that
//! listened, what it heard in a few words.

use serde_json::Value;
use trill_core::journal::Place;
use trill_core::journal::words::{list, quoted};
use trill_core::{AutomationTarget, ClipId, DeviceId, Project, TrackId};

/// One tool call in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wording {
    /// While it runs: "Adding the track \u{201C}Bass\u{201D}".
    pub doing: String,
    /// Once it has: "Added the track \u{201C}Bass\u{201D}".
    pub did: String,
}

impl Wording {
    pub fn new(doing: impl Into<String>, did: impl Into<String>) -> Self {
        Self { doing: doing.into(), did: did.into() }
    }

    /// `doing` and `did` followed by the same words: ("Adding", "Added",
    /// "the track \u{201C}Bass\u{201D}").
    pub fn about(doing: &str, did: &str, what: &str) -> Self {
        Self::new(format!("{doing} {what}"), format!("{did} {what}"))
    }
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

/// `tool`'s name as words: `set_sample` reads "Set sample".
fn spelled_out(tool: &str) -> String {
    let words = tool.replace(['_', '-'], " ");
    let mut letters = words.chars();
    letters.next().map_or_else(|| "A tool".to_owned(), |first| first.to_uppercase().chain(letters).collect())
}

/// Says in words what calling `tool` with `input` does, naming things as
/// `project` names them.
/// What a preset is called in a sentence: one that was asked for by its
/// name, by that; one that is a file, by the file's name and not the
/// whole way to it.
fn preset_words(preset: &str) -> String {
    match trill_core::presets::PresetFile::asked(preset) {
        Some(file) => {
            let name = file.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            match file.within {
                Some(within) => format!("the preset {within} of the file {name}"),
                None => format!("the preset file {name}"),
            }
        }
        None => format!("the preset {preset}"),
    }
}

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
        "set_tempo" => {
            let bar = call.input.get("bar").and_then(Value::as_u64).unwrap_or(1);
            let place = match call.input.get("beat").and_then(Value::as_f64).filter(|beat| *beat > 1.0) {
                Some(beat) => format!("bar {bar} beat {beat}"),
                None => format!("bar {bar}"),
            };
            let flag = |key: &str| call.input.get(key).and_then(Value::as_bool) == Some(true);
            match (call.input.get("bpm").and_then(Value::as_f64), flag("remove"), flag("ramp")) {
                (_, true, _) => about("Removing", "Removed", &format!("the change of tempo at {place}")),
                (Some(bpm), _, true) => Wording {
                    doing: format!("Making the tempo glide to {bpm} BPM by {place}"),
                    did: format!("Made the tempo glide to {bpm} BPM by {place}"),
                },
                (Some(bpm), ..) => about("Setting", "Set", &format!("the tempo to {bpm} BPM at {place}")),
                (None, ..) => about("Setting", "Set", &format!("the tempo at {place}")),
            }
        }
        "set_time_signature" => {
            let bar = call.input.get("bar").and_then(Value::as_u64).unwrap_or(1);
            let lasting = match call.input.get("bars").and_then(Value::as_u64) {
                Some(1) => " for a bar".to_owned(),
                Some(bars) => format!(" for {bars} bars"),
                None => String::new(),
            };
            match (call.text("time_signature"), call.input.get("remove").and_then(Value::as_bool)) {
                (_, Some(true)) => about("Removing", "Removed", &format!("the change of time signature at bar {bar}")),
                (Some(signature), _) => {
                    about("Setting", "Set", &format!("the time signature to {signature} at bar {bar}{lasting}"))
                }
                (None, _) => about("Setting", "Set", &format!("the time signature at bar {bar}")),
            }
        }
        "get_tempo_map" => about("Reading", "Read", "the tempo and the time signature along the song"),
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
            // One that listens to another track says which.
            match call.track("sidechain") {
                Some(key) => about("Adding", "Added", &format!("{device} to {owner}, listening to {key}")),
                None => about("Adding", "Added", &format!("{device} to {owner}")),
            }
        }
        "get_device" => about("Reading", "Read", &call.device("device").unwrap_or_else(|| "a device".to_owned())),
        "update_device" => {
            let device = call.device("device").unwrap_or_else(|| "a device".to_owned());
            // The track it is to listen to; 0 is its own input again.
            let key = call.id("sidechain").map(|id| match id {
                0 => "its own input".to_owned(),
                _ => call.track("sidechain").unwrap_or_else(|| "another track".to_owned()),
            });
            match (call.text("preset"), call.input.get("enabled").and_then(Value::as_bool), call.has("params")) {
                (Some(preset), _, _) => about("Setting", "Set", &format!("{device} to {}", preset_words(preset))),
                (None, Some(true), false) => about("Switching on", "Switched on", &device),
                (None, Some(false), false) => about("Bypassing", "Bypassed", &device),
                (None, None, false) if key.is_some() => {
                    about("Having", "Had", &format!("{device} listen to {}", key.unwrap_or_default()))
                }
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
        "save_sound" => {
            let track = call.track("track").unwrap_or_else(|| "a track".to_owned());
            let name = call.text("name").map(quoted).unwrap_or_default();
            about("Saving", "Saved", format!("the sound of {track} as {name}").trim_end_matches(" as ").trim_end())
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
            about("Adding", "Added", &format!("{name}{track}{}", played_as(call.input.get("notes"))))
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
        "get_controllers" => {
            about("Reading", "Read", &format!("the controllers of {}", call.clip("clip").unwrap_or_default()))
        }
        "set_controllers" => {
            about("Shaping", "Shaped", &format!("the controllers of {}", call.clip("clip").unwrap_or_default()))
        }
        "shape_dynamics" => {
            about("Shaping", "Shaped", &format!("the dynamics of {}", call.clip("clip").unwrap_or_default()))
        }
        "add_notes" | "set_notes" => {
            let count = call.input.get("notes").and_then(Value::as_array).map_or(0, Vec::len);
            let notes = if count == 1 { "1 note".to_owned() } else { format!("{count} notes") };
            let clip = call.clip("clip").unwrap_or_else(|| "a clip".to_owned());
            let how = played_as(call.input.get("notes"));
            if tool == "add_notes" {
                about("Adding", "Added", &format!("{notes} to {clip}{how}"))
            } else {
                about("Rewriting", "Rewrote", &format!("{clip} with {notes}{how}"))
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
                Some("send") => {
                    let from = target_call.track("track").unwrap_or_default();
                    format!("the send of {from} to {}", target_call.track("to").unwrap_or_default())
                }
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
            Some("copy") => about("Copying", "Copied", "a stretch of time"),
            _ => about("Editing", "Edited", "the timeline"),
        },
        "get_instrument_profile" => match (call.track("track"), call.text("uri")) {
            (Some(track), _) => about("Reading", "Read", &format!("what is known about the instrument of {track}")),
            (None, Some(uri)) => about("Reading", "Read", &format!("what is known about {}", device_kind(uri))),
            (None, None) => about("Reading", "Read", "which instruments are known"),
        },
        "set_instrument_profile" => match call.track("track") {
            Some(track) => about("Noting", "Noted", &format!("what the instrument of {track} does")),
            None => about("Noting", "Noted", "what an instrument does"),
        },

        "transport" => match call.text("action") {
            Some("play") => about("Starting", "Started", "playback"),
            Some("stop") => about("Stopping", "Stopped", "playback"),
            Some("seek") => about("Moving", "Moved", "the playhead"),
            Some("panic") => about("Silencing", "Silenced", "everything"),
            _ => about("Checking", "Checked", "the transport"),
        },
        "render" => {
            // With stems, every track becomes a file of its own as well.
            let what = if call.has("stems") { "the song and its tracks" } else { "the song" };
            match call.text("path") {
                Some(path) => about("Rendering", "Rendered", &format!("{what} to {}", quoted(file_name(path)))),
                None => about("Rendering", "Rendered", what),
            }
        }
        "film" | "film_plan" => {
            let what = match (call.text("of"), call.text("section")) {
                (Some("making_of"), _) => "how the song was made".to_owned(),
                (_, Some(section)) => format!("{} of the song", quoted(section)),
                _ => "the song".to_owned(),
            };
            match (tool, call.text("path")) {
                ("film_plan", _) => about("Planning", "Planned", &format!("a film of {what}")),
                (_, Some(path)) => about("Filming", "Filmed", &format!("{what} to {}", quoted(file_name(path)))),
                (_, None) => about("Filming", "Filmed", &what),
            }
        }
        "measure" => match call.has("tracks") {
            true => about("Measuring", "Measured", "some of the tracks"),
            false => about("Measuring", "Measured", "the song"),
        },
        "probe" => match (call.track("track"), call.text("device")) {
            (Some(track), _) => about("Trying out", "Tried out", &format!("the instrument of {track}")),
            (None, Some(uri)) => about("Trying out", "Tried out", device_kind(uri)),
            (None, None) => about("Trying out", "Tried out", "an instrument"),
        },

        "audio" => match call.has("output") || call.has("sample_rate") || call.has("buffer_size") {
            true => about("Changing", "Changed", "the audio device"),
            false => about("Checking", "Checked", "the audio device"),
        },
        "midi_inputs" => match call.has("enable") || call.has("disable") {
            true => about("Switching", "Switched", "MIDI inputs"),
            false => about("Looking at", "Looked at", "the MIDI inputs"),
        },
        "play_notes" => match call.track("track") {
            Some(track) => about("Playing", "Played", &format!("notes on {track}")),
            None => about("Playing", "Played", "notes"),
        },
        "get_journal" => about("Reading", "Read", "how the song was made"),
        "journal" => match call.text("action") {
            Some("delete") => about("Deleting", "Deleted", "the journal of this song"),
            Some("keep") => about("Switching on", "Switched on", "the journal"),
            Some("stop") => about("Switching off", "Switched off", "the journal"),
            Some("export") => about("Writing out", "Wrote out", "how the song was made"),
            _ => about("Checking", "Checked", "the journal"),
        },
        "get_song_at" => match call.input.get("step").and_then(Value::as_u64) {
            Some(step) => about("Reading", "Read", &format!("the song as it was after step {step}")),
            None => about("Reading", "Read", "the song as it was"),
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
            Some("move_folder") => about("Telling", "Told", "the sound library where a folder went"),
            _ => about("Checking", "Checked", "the sound library"),
        },
        "missing_files" => about("Looking for", "Looked for", "the song's missing files"),
        "relink_files" => match crate::files::one_file(input) {
            Some(file) => about("Relinking", "Relinked", &quoted(file)),
            None => about("Relinking", "Relinked", "the song's missing files"),
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

        "describe_wavetable" => match (call.text("table"), input.get("recipe").is_some(), call.device("device")) {
            (_, true, _) => about("Looking at", "Looked at", "the table a recipe would make"),
            (_, _, Some(device)) => about("Looking at", "Looked at", &format!("a table of {device}")),
            (Some(table), ..) => about("Looking at", "Looked at", &format!("the wavetable {}", quoted(table))),
            _ => about("Listing", "Listed", "the wavetables"),
        },
        "set_wavetable" => {
            let device = call.device("device").unwrap_or_else(|| "a device".to_owned());
            let recipe = input.get("recipe").and_then(|recipe| recipe.get("name")).and_then(Value::as_str);
            match (call.text("table"), input.get("recipe").is_some(), call.text("file")) {
                (Some(table), ..) => about("Giving", "Gave", &format!("{device} the wavetable {}", quoted(table))),
                (_, true, _) => match recipe {
                    Some(name) => about("Making", "Made", &format!("the wavetable {} for {device}", quoted(name))),
                    None => about("Making", "Made", &format!("a wavetable for {device}")),
                },
                (_, _, Some(file)) => {
                    about("Reading", "Read", &format!("a wavetable for {device} from {}", quoted(file_name(file))))
                }
                _ => about("Naming", "Named", &format!("the macros of {device}")),
            }
        }

        other => {
            let words = spelled_out(other);
            Wording::new(words.clone(), words)
        }
    }
}

/// The automation target a call's `target` argument names.
fn target_of(target: &Value) -> Option<AutomationTarget> {
    let id = |key: &str| target.get(key).and_then(Value::as_u64);
    match target.get("type")?.as_str()? {
        "track_volume" => Some(AutomationTarget::TrackVolume { track: TrackId(id("track")?) }),
        "track_pan" => Some(AutomationTarget::TrackPan { track: TrackId(id("track")?) }),
        "master_volume" => Some(AutomationTarget::MasterVolume),
        "send" => Some(AutomationTarget::Send { track: TrackId(id("track")?), to: TrackId(id("to")?) }),
        "device_param" => Some(AutomationTarget::DeviceParam {
            device: DeviceId(id("device")?),
            param: target.get("param")?.as_str()?.to_owned(),
        }),
        _ => None,
    }
}

/// What a call to `tool` with `input` that answered `answer` made or
/// changed, if it says. A call that only read, that removed something, or
/// that this table does not know, names nothing.
pub fn of_call(tool: &str, input: &Value, answer: &str) -> Option<Place> {
    let answer: Value = serde_json::from_str(answer).unwrap_or(Value::Null);
    let given = |key: &str| input.get(key).and_then(Value::as_u64);
    let answered = |key: &str| answer.get(key).and_then(Value::as_u64);
    match tool {
        "create_track" => answered("track_id").map(Place::track),
        "update_track" | "set_instrument_profile" | "save_sound" => given("track").map(Place::track),

        "add_device" => answered("device_id").map(Place::device),
        "update_device" | "set_sample" | "set_wavetable" | "show_device_editor" | "save_preset" => {
            given("device").map(Place::device)
        }

        "create_clip" | "create_audio_clip" => Place::clips(answered("clip_id")),
        "relink_files" => {
            let clips = answer.get("clips").and_then(Value::as_array);
            Place::clips(clips.into_iter().flatten().filter_map(Value::as_u64))
        }
        "update_clip" | "add_notes" | "set_notes" | "remove_notes" | "transform_notes" => Place::clips(given("clip")),
        "write_chords" | "write_drums" | "write_arpeggio" | "write_bass" | "write_melody" => {
            Place::clips(answered("clip_id"))
        }
        "split_clip" => {
            let half = |key: &str| answer.get(key)?.get("clip_id")?.as_u64();
            Place::clips(half("first").into_iter().chain(half("second")))
        }
        "duplicate_clip" => {
            let copies = answer.get("copies").and_then(Value::as_array);
            Place::clips(copies.into_iter().flatten().filter_map(|copy| copy.get("clip_id")?.as_u64()))
        }
        "import_midi" => {
            let parts = answer.get("tracks").and_then(Value::as_array);
            Place::clips(parts.into_iter().flatten().filter_map(|part| part.get("clip_id")?.as_u64()))
        }

        "set_automation" => {
            let cleared = answer.get("cleared").and_then(Value::as_bool) == Some(true);
            input.get("target").and_then(target_of).filter(|_| !cleared).map(|target| Place::Lane { target })
        }
        "set_markers" => {
            let beats = input
                .get("markers")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|marker| marker.get("beat")?.as_f64());
            let (start, end) =
                beats.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), beat| (low.min(beat), high.max(beat)));
            (start <= end).then_some(Place::Span { start, end })
        }
        "edit_time" => {
            let number = |key: &str| input.get(key).and_then(Value::as_f64);
            let start = number("start")?;
            let length = match (input.get("action")?.as_str()?, number("end"), number("length")) {
                // What was there is gone: the cut is all there is to see.
                ("delete", ..) => 0.0,
                // The stretch and its repeat.
                ("duplicate", Some(end), _) => 2.0 * (end - start),
                ("insert", _, Some(length)) => length,
                // The copy, where it was put.
                ("copy", Some(end), _) => {
                    let to = number("to")?;
                    return Some(Place::Span { start: to, end: to + (end - start).max(0.0) });
                }
                _ => return None,
            };
            Some(Place::Span { start, end: start + length.max(0.0) })
        }
        "set_project" => {
            let region = input.get("loop")?;
            let (start, end) = (region.get("start")?.as_f64()?, region.get("end")?.as_f64()?);
            (end > start).then_some(Place::Span { start, end })
        }
        // Where the change lies, and how far it reaches if it was told to.
        "set_tempo" | "set_time_signature" => {
            let start = answer.get("at_beat")?.as_f64()?;
            let end = answer.get("until_beat").and_then(Value::as_f64).unwrap_or(start);
            (end >= start).then_some(Place::Span { start, end })
        }
        _ => None,
    }
}

/// A number as it is said: one decimal, and no more than is there.
fn number(value: &Value) -> Option<String> {
    let value = value.as_f64()?;
    Some(format!("{:.1}", (value * 10.0).round() / 10.0))
}

/// What a call that listened heard, in a few words, read from its answer:
/// a measurement's loudness, peak and length; which of a probe's notes
/// sounded. `None` for a call that does not listen, and for an answer that
/// says nothing of the kind.
pub fn heard(tool: &str, answer: &str) -> Option<String> {
    let answer: Value = serde_json::from_str(answer).ok()?;
    match tool {
        "film" => {
            // How long it is and how it sounds: what a film is known by.
            let seconds = answer.get("seconds").and_then(Value::as_f64)?;
            let mut said = vec![format!("{}:{:02}", seconds as u64 / 60, seconds as u64 % 60)];
            if let Some(size) = answer.get("size").and_then(Value::as_str) {
                said.push(size.to_owned());
            }
            if let Some(loudness) = answer.get("loudness_lufs").and_then(number) {
                said.push(format!("{loudness} LUFS"));
            }
            Some(said.join(", "))
        }
        "film_plan" => {
            let takes = answer.get("takes").and_then(Value::as_array).map(Vec::len);
            let shots = answer.get("shots").and_then(Value::as_u64);
            match (takes, shots) {
                (Some(takes), _) => Some(format!("{takes} takes")),
                (None, Some(shots)) => Some(format!("{shots} shots")),
                (None, None) => None,
            }
        }
        "measure" | "render" => {
            let mut said = Vec::new();
            if let Some(loudness) = answer.get("loudness_lufs").and_then(number) {
                said.push(format!("{loudness} LUFS"));
            }
            if let Some(peak) = answer.get("true_peak_db").or_else(|| answer.get("peak_db")).and_then(number) {
                said.push(format!("peak {peak} dB"));
            }
            if let Some(seconds) = answer.get("duration_seconds").and_then(number) {
                said.push(format!("{seconds} s"));
            }
            (!said.is_empty()).then(|| said.join(", "))
        }
        "get_instrument_profile" => {
            // A drum track: how many drums it can be written by, and whose word that is.
            let drums = answer.get("drum_map").and_then(Value::as_object).map(|map| map.len())?;
            let lacking: Vec<&str> = answer
                .get("kit")
                .and_then(|kit| kit.get("has_no"))
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            Some(match (answer.get("kit").is_some(), lacking.is_empty()) {
                (true, true) => format!("{drums} drums by name, read from the kit that is loaded"),
                (true, false) => {
                    format!("{drums} drums by name, read from the kit that is loaded; it has no {}", lacking.join(", "))
                }
                (false, _) => format!("{drums} drums by name"),
            })
        }
        "probe" => {
            let played = answer.get("played").and_then(Value::as_u64)?;
            let notes = if played == 1 { "1 note played".to_owned() } else { format!("{played} notes played") };
            Some(match answer.get("sounding").and_then(Value::as_str) {
                Some("none") => format!("{notes}, none sounding"),
                Some(sounding) => format!("{notes}, sounding: {sounding}"),
                None => notes,
            })
        }
        _ => None,
    }
}

/// The ways of playing that the notes of a call ask for by name, as the
/// end of a sentence about them: `, played as palm_mute and hammer`. Empty
/// where no note says how it is played.
fn played_as(notes: Option<&Value>) -> String {
    let mut names: Vec<&str> = Vec::new();
    for note in notes.and_then(Value::as_array).into_iter().flatten() {
        let how = match note {
            Value::Object(fields) => fields.get("how").or_else(|| fields.get("articulation")).and_then(Value::as_str),
            // The fifth item, or the fourth where that is no velocity.
            Value::Array(items) => items.get(4).or_else(|| items.get(3)).and_then(Value::as_str),
            _ => None,
        };
        if let Some(how) = how.filter(|how| !names.contains(how)) {
            names.push(how);
        }
    }
    match names.as_slice() {
        [] => String::new(),
        [one] => format!(", played as {one}"),
        [first @ .., last] => format!(", played as {} and {last}", first.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trill_core::ChainRef;

    fn clips(ids: &[ClipId]) -> Option<Place> {
        Place::clips(ids.iter().map(|id| id.0))
    }

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
        assert_eq!(did("set_tempo", json!({ "bar": 9, "bpm": 140 }), p), "Set the tempo to 140 BPM at bar 9");
        assert_eq!(
            did("set_tempo", json!({ "bar": 12, "beat": 3, "bpm": 90.5, "ramp": true }), p),
            "Made the tempo glide to 90.5 BPM by bar 12 beat 3"
        );
        assert_eq!(did("set_tempo", json!({ "bar": 9, "remove": true }), p), "Removed the change of tempo at bar 9");
        assert_eq!(
            did("set_time_signature", json!({ "bar": 9, "time_signature": "7/8", "bars": 4 }), p),
            "Set the time signature to 7/8 at bar 9 for 4 bars"
        );
        assert_eq!(
            did("set_time_signature", json!({ "bar": 9, "remove": true }), p),
            "Removed the change of time signature at bar 9"
        );
        assert_eq!(did("get_tempo_map", json!({}), p), "Read the tempo and the time signature along the song");
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
            did("add_device", json!({ "uri": "builtin:compressor", "sidechain": track }), p),
            "Added compressor to the master, listening to \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "sidechain": track }), p),
            "Had \u{201C}Poly Synth\u{201D} listen to \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "sidechain": 0 }), p),
            "Had \u{201C}Poly Synth\u{201D} listen to its own input"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "params": { "cutoff": 900 } }), p),
            "Adjusted \u{201C}Poly Synth\u{201D}"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "preset": "warm_pad" }), p),
            "Set \u{201C}Poly Synth\u{201D} to the preset warm_pad"
        );
        // A preset that is a file is called by the file's name.
        let file = std::env::temp_dir().join("packs").join("OMEGA SAW.SerumPreset").to_string_lossy().into_owned();
        assert_eq!(
            did("update_device", json!({ "device": device, "preset": file }), p),
            "Set \u{201C}Poly Synth\u{201D} to the preset file OMEGA SAW.SerumPreset"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "preset": "Pads/Warm Pad" }), p),
            "Set \u{201C}Poly Synth\u{201D} to the preset Pads/Warm Pad"
        );
        assert_eq!(
            did("update_device", json!({ "device": device, "enabled": false }), p),
            "Bypassed \u{201C}Poly Synth\u{201D}"
        );
        // A track's sound, kept as one thing.
        assert_eq!(
            did("save_sound", json!({ "track": track, "name": "Dub Bass" }), p),
            "Saved the sound of \u{201C}Bass\u{201D} as \u{201C}Dub Bass\u{201D}"
        );
        assert_eq!(did("save_sound", json!({ "track": track }), p), "Saved the sound of \u{201C}Bass\u{201D}");
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
        assert_eq!(
            did("render", json!({ "path": "mix.wav", "stems": ["all"] }), p),
            "Rendered the song and its tracks to \u{201C}mix.wav\u{201D}"
        );
        assert_eq!(did("measure", json!({}), p), "Measured the song");
        assert_eq!(did("measure", json!({ "tracks": [1] }), p), "Measured some of the tracks");
        assert!(did("probe", json!({ "track": track }), p).starts_with("Tried out the instrument of "));
        assert_eq!(did("probe", json!({ "device": "builtin:polysynth" }), p), "Tried out polysynth");
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
            did("edit_time", json!({ "action": "copy", "start": 0, "end": 16, "to": 32 }), p),
            "Copied a stretch of time"
        );
        assert_eq!(
            did("edit_time", json!({ "action": "duplicate", "start": 0, "end": 16 }), p),
            "Repeated a stretch of time"
        );
    }

    #[test]
    fn files_that_are_missing_are_looked_for_and_relinked_in_words() {
        let (project, ..) = song();
        let p = &project;
        let wording = describe("missing_files", &json!({}), p);
        assert_eq!(wording.doing, "Looking for the song's missing files");
        assert_eq!(wording.did, "Looked for the song's missing files");
        assert_eq!(did("relink_files", json!({}), p), "Relinked the song's missing files");
        assert_eq!(
            did("relink_files", json!({ "found": "likely", "look_in": ["D:/x"] }), p),
            "Relinked the song's missing files"
        );
        // One file by hand goes by its name, not by where it is.
        let one = json!({ "files": [{ "clip": 7, "to": "D:\\Samples\\Kicks\\Kick 01.wav" }] });
        assert_eq!(did("relink_files", one, p), "Relinked \u{201C}Kick 01.wav\u{201D}");
        let two = json!({ "files": [{ "clip": 7, "to": "a.wav" }, { "clip": 8, "to": "b.wav" }] });
        assert_eq!(did("relink_files", two, p), "Relinked the song's missing files");
        assert_eq!(
            did("library", json!({ "action": "move_folder", "path": "D:/a", "to": "D:/b" }), p),
            "Told the sound library where a folder went"
        );
        // Where it landed: the clips, as the answer names them.
        let answer = json!({ "relinked": { "files": 1, "places": 2 }, "clips": [7, 9] }).to_string();
        assert_eq!(of_call("relink_files", &json!({}), &answer), Place::clips([7, 9]));
        assert_eq!(of_call("relink_files", &json!({}), "{}"), None);
        assert_eq!(of_call("missing_files", &json!({}), "{}"), None);
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
        // Notes that say how they are played say so in the journal too.
        let riff = json!([["E2", 0, 0.5, 100, "palm_mute"], ["E2", 1, 0.5, "palm_mute"], { "pitch": 40, "start": 2, "length": 1, "how": "hammer" }, [40, 3, 1, 90]]);
        assert!(
            did("add_notes", json!({ "clip": "seven", "notes": riff }), &project)
                .ends_with("Added 4 notes to a clip, played as palm_mute and hammer")
        );
        assert_eq!(played_as(Some(&json!([[40, 0, 1, 100, "muted"]]))), ", played as muted");
        assert_eq!(
            played_as(Some(&json!([[40, 0, 1, 100], ["kick", 0, 1]]))),
            "",
            "a drum's name is its pitch, not how it is played"
        );
        // What a drum track's instrument was found to know.
        let kit = json!({ "drum_map": { "kick": 36, "china": 52 }, "kit": { "has_no": ["clap", "splash"] } });
        assert_eq!(
            heard("get_instrument_profile", &kit.to_string()).as_deref(),
            Some("2 drums by name, read from the kit that is loaded; it has no clap, splash")
        );
        assert_eq!(
            heard("get_instrument_profile", &json!({ "drum_map": { "kick": 36 } }).to_string()).as_deref(),
            Some("1 drums by name")
        );
        assert_eq!(heard("get_instrument_profile", &json!({ "plays": "pitched" }).to_string()), None);
    }

    #[test]
    fn every_tool_the_server_offers_has_words_of_its_own() {
        let (project, track, clip, device) = song();
        let input = json!({ "track": track, "clip": clip, "device": device });
        let tools = crate::TrillServer::tool_names();
        assert!(tools.len() > 50, "the server's own list: {tools:?}");
        for tool in &tools {
            let wording = describe(tool, &input, &project);
            assert_ne!(wording.did, spelled_out(tool), "{tool} has no words of its own");
            assert_ne!(wording.doing, wording.did, "{tool} reads the same before and after");
            assert!(!wording.did.contains("  ") && !wording.did.ends_with(' '), "{tool}: {:?}", wording.did);
        }
    }

    #[test]
    fn a_call_says_what_it_made_or_changed() {
        let of = |tool: &str, input: Value, answer: Value| of_call(tool, &input, &answer.to_string());
        // What was made is named in the answer.
        assert_eq!(
            of("create_track", json!({ "name": "Drums" }), json!({ "track_id": 9, "instrument_device_id": 10 })),
            Some(Place::track(9))
        );
        assert_eq!(
            of("add_device", json!({ "track": 2, "uri": "builtin:reverb" }), json!({ "device_id": 12 })),
            Some(Place::device(12))
        );
        assert_eq!(
            of("create_clip", json!({ "track": 2 }), json!({ "clip_id": 14, "note_count": 0 })),
            clips(&[ClipId(14)])
        );
        assert_eq!(of("create_audio_clip", json!({ "track": 2 }), json!({ "clip_id": 15 })), clips(&[ClipId(15)]));
        let written = json!({ "clip_id": 58, "track": 26, "created": true, "bars": 8 });
        for tool in ["write_chords", "write_drums", "write_arpeggio", "write_bass", "write_melody"] {
            assert_eq!(of(tool, json!({ "target": { "track": 26 } }), written.clone()), clips(&[ClipId(58)]), "{tool}");
        }
        assert_eq!(
            of(
                "split_clip",
                json!({ "clip": 3, "at": 8 }),
                json!({ "first": { "clip_id": 3 }, "second": { "clip_id": 20 } })
            ),
            clips(&[ClipId(3), ClipId(20)])
        );
        assert_eq!(
            of("duplicate_clip", json!({ "clip": 3 }), json!({ "copies": [{ "clip_id": 21 }, { "clip_id": 22 }] })),
            clips(&[ClipId(21), ClipId(22)])
        );
        assert_eq!(
            of("import_midi", json!({ "path": "a.mid" }), json!({ "tracks": [{ "track_id": 30, "clip_id": 31 }] })),
            clips(&[ClipId(31)])
        );

        // What was changed is named in the arguments.
        assert_eq!(of("update_track", json!({ "track": 4, "volume_db": -6 }), json!({})), Some(Place::track(4)));
        assert_eq!(of("update_device", json!({ "device": 5, "params": {} }), json!({})), Some(Place::device(5)));
        assert_eq!(of("save_sound", json!({ "track": 4, "name": "Tone" }), json!({})), Some(Place::track(4)));
        assert_eq!(of("set_sample", json!({ "device": 5, "path": "k.wav" }), json!({})), Some(Place::device(5)));
        assert_eq!(of("set_wavetable", json!({ "device": 5, "table": "Acid" }), json!({})), Some(Place::device(5)));
        assert_eq!(of("describe_wavetable", json!({ "device": 5 }), json!({})), None);
        for tool in ["update_clip", "add_notes", "set_notes", "remove_notes", "transform_notes"] {
            assert_eq!(of(tool, json!({ "clip": 7 }), json!({})), clips(&[ClipId(7)]), "{tool}");
        }
        let target = json!({ "type": "device_param", "device": 5, "param": "cutoff" });
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": target, "points": [[0, 1]] }),
                json!({ "point_count": 1, "cleared": false })
            ),
            Some(Place::Lane { target: AutomationTarget::DeviceParam { device: DeviceId(5), param: "cutoff".into() } })
        );
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": { "type": "track_volume", "track": 4 } }),
                json!({ "point_count": 2 })
            ),
            Some(Place::Lane { target: AutomationTarget::TrackVolume { track: TrackId(4) } })
        );
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": { "type": "send", "track": 4, "to": 9 } }),
                json!({ "point_count": 5 })
            ),
            Some(Place::Lane { target: AutomationTarget::Send { track: TrackId(4), to: TrackId(9) } })
        );
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": { "type": "master_volume" }, "points": [] }),
                json!({ "cleared": true })
            ),
            None
        );

        // Time: the markers set, the stretch repeated or opened up, the loop.
        let markers = json!({ "markers": [{ "beat": 32, "name": "Verse" }, { "beat": 0, "name": "Intro" }] });
        assert_eq!(of("set_markers", markers, json!({})), Some(Place::Span { start: 0.0, end: 32.0 }));
        assert_eq!(of("set_markers", json!({ "markers": [] }), json!({})), None);
        let repeat = json!({ "action": "duplicate", "start": 16, "end": 32 });
        assert_eq!(of("edit_time", repeat, json!({})), Some(Place::Span { start: 16.0, end: 48.0 }));
        let open = json!({ "action": "insert", "start": 8, "length": 4 });
        assert_eq!(of("edit_time", open, json!({})), Some(Place::Span { start: 8.0, end: 12.0 }));
        let cut = json!({ "action": "delete", "start": 8, "end": 16 });
        assert_eq!(of("edit_time", cut, json!({})), Some(Place::Span { start: 8.0, end: 8.0 }));
        let copy = json!({ "action": "copy", "start": 16, "end": 32, "to": 64 });
        assert_eq!(of("edit_time", copy, json!({})), Some(Place::Span { start: 64.0, end: 80.0 }));
        let looped = json!({ "tempo": 96, "loop": { "enabled": true, "start": 16, "end": 32 } });
        assert_eq!(of("set_project", looped, json!({})), Some(Place::Span { start: 16.0, end: 32.0 }));
        assert_eq!(of("set_project", json!({ "tempo": 96 }), json!({})), None);
        let changed = json!({ "at_beat": 32.0, "until_beat": 46.0 });
        assert_eq!(
            of("set_time_signature", json!({ "bar": 9, "time_signature": "7/8", "bars": 4 }), changed),
            Some(Place::Span { start: 32.0, end: 46.0 })
        );
        assert_eq!(
            of("set_tempo", json!({ "bar": 9, "bpm": 140 }), json!({ "at_beat": 32.0 })),
            Some(Place::Span { start: 32.0, end: 32.0 })
        );
        assert_eq!(of("get_tempo_map", json!({}), json!({ "tempo": [] })), None);

        // Reading, removing and anything unknown name nothing; nor does an
        // answer that is not what was expected.
        for tool in [
            "get_project",
            "get_notes",
            "delete_track",
            "delete_clip",
            "delete_device",
            "history",
            "render",
            "freeze_track",
        ] {
            assert_eq!(
                of(tool, json!({ "track": 1, "clip": 2, "device": 3 }), json!({ "track_id": 1, "clip_id": 2 })),
                None,
                "{tool}"
            );
        }
        assert_eq!(of_call("create_track", &json!({ "name": "x" }), "it went wrong"), None);
        assert_eq!(of_call("create_clip", &json!("nonsense"), "{\"clip_id\":\"seven\"}"), None);
    }

    #[test]
    fn what_a_call_heard_is_said_in_a_few_words() {
        let measured =
            json!({ "loudness_lufs": -14.23, "peak_db": -2.0, "true_peak_db": -1.04, "duration_seconds": 96.0 });
        assert_eq!(heard("measure", &measured.to_string()).as_deref(), Some("-14.2 LUFS, peak -1.0 dB, 96.0 s"));
        let rendered = json!({ "loudness_lufs": -20.0, "peak_db": -3.26, "path": "mix.wav" });
        assert_eq!(heard("render", &rendered.to_string()).as_deref(), Some("-20.0 LUFS, peak -3.3 dB"));
        let probed = json!({ "played": 88, "sounding": "E1 to G4", "silent": "A0 to D#1" });
        assert_eq!(heard("probe", &probed.to_string()).as_deref(), Some("88 notes played, sounding: E1 to G4"));
        let silent = json!({ "played": 1, "sounding": "none" });
        assert_eq!(heard("probe", &silent.to_string()).as_deref(), Some("1 note played, none sounding"));
        // A call that does not listen, and answers that say nothing.
        assert_eq!(heard("create_track", "{\"track_id\":3}"), None);
        assert_eq!(heard("measure", "it went wrong"), None);
        assert_eq!(heard("measure", "{}"), None);
    }
}
