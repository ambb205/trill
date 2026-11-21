//! Recording sound: `track_input` (what a track records and how it is
//! heard meanwhile), `audio_inputs` (the input and what is arriving on
//! it), `takes` (the recordings kept beside a track, and the comp made of
//! them), `recording_latency` (how long sound takes to go round, and the
//! measurement of it) and `unused_recordings`. Recording itself is
//! `transport`, as for notes: `record` and `stop`.
//!
//! # The assistant's door
//!
//! An input is a microphone, and a microphone hears a room. The assistant
//! in the window acts on what it reads, some of which strangers wrote, and
//! its panel cannot ask before it does something. So at its door a track
//! that records an input is armed by the user's own hand or not at all:
//! the assistant may not arm one, may not give an armed track an input,
//! may not open the input to look at its level, and may not run the
//! measurement, which opens it too. It may record once the user has armed
//! the track. It cannot delete a recording either. A client the user set
//! up themselves is the user's own and is held to none of this.

use crate::server::{Reply, TrillServer, explain, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use trill_core::{ClipId, Monitoring, TrackId, TrackInput};
use trill_engine::{Correction, Measurement, Recorded, Session};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Recording sound
- A track records what a keyboard plays unless it is given an input of the audio device (track_input: a channel, \
or two side by side). Armed tracks of both kinds are recorded together by transport's record, as one take and \
one undo step; stop ends it and says what was recorded, where its file is, how loud it was and whether any of it \
is silence for sound that did not arrive.
- Record only when asked to. A microphone hears the room the user is in, and you cannot hear it: before a take, \
ask audio_inputs what is arriving on the channel (a level near -inf means nothing is plugged in or the wrong \
channel; `clipped` means it is too loud at the source, which nothing after the fact repairs), and say what you \
found rather than recording blind.
- Monitoring (hearing the input through the track's devices) is off unless asked for: through speakers a \
microphone feeds back. The round trip in milliseconds is in track_input's reply.
- Every take is kept beside its track in a lane and never played from there; the track plays ordinary audio \
clips. With the loop on each pass is a take of its own and the track plays the last whole one. takes lists them \
with their files, so that listen can be pointed at each to hold them side by side by pitch and timing; `use` puts \
a stretch of one on the track, with the seams faded, and whatever it covers is kept as a take first. Nothing \
recorded is ever lost by recording over it or by comping, and each is one undo step.
- A take lands where it was sung by taking off how long sound takes to go round, which the devices only partly \
say. recording_latency says what is taken off and whether it was measured; if takes sound late or early against \
the song, tell the user to run its measurement with a cable from the output to the input.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackInputRequest {
    /// Track id.
    pub track: u64,
    /// What the track records: a channel of the audio device's input counted from 1 ("1"), two side by side as
    /// left and right ("1+2"), or "none" for what a keyboard plays, as a track records that says nothing.
    /// Omit to leave it as it is.
    pub input: Option<String>,
    /// How the input is heard through the track's devices: "off" (default: it is recorded and not heard
    /// through Trill), "armed" (while the track is armed) or "always". Through speakers a microphone that is
    /// monitored feeds back.
    pub monitor: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AudioInputsRequest {
    /// true opens the input for a moment, if nothing has it open, to say what is arriving on each channel
    /// now. Without it the levels are only given while the input is open anyway (a track that records one is
    /// armed or monitored).
    pub listen: Option<bool>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TakesAction {
    /// The takes of `track`, or of every track: each with its lane, where it lies, its file and how far into
    /// the file it begins, and whether the track plays it.
    #[default]
    List,
    /// Put `take` on its track: all of it, or with `from_beat` and `to_beat` that stretch of it, in place of
    /// what the track had there. What is covered is kept as a take first. One undo step.
    Use,
    /// Hear `take` in place of what its track plays, without changing the song; omit `take` to go back to
    /// the song. Not saved and no undo step: it is for choosing by ear.
    Hear,
    /// Give `take` the name `name`.
    Rename,
    /// Take `take` out of its lane. Its file stays on disk, and what the track plays of it stays on the
    /// track. One undo step.
    Remove,
    /// How loud `take` is, read from its file: peak and level in dB, and how long after its start the sound
    /// begins.
    Measure,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TakesRequest {
    #[serde(default)]
    pub action: TakesAction,
    /// Track id, for `list`. Omit for every track that has takes.
    pub track: Option<u64>,
    /// Take id, as `list` gives it.
    pub take: Option<u64>,
    /// For `use`: where the stretch begins, in beats of the song. Omit both for the whole take.
    pub from_beat: Option<f64>,
    /// For `use`: where the stretch ends, in beats of the song.
    pub to_beat: Option<f64>,
    /// For `rename`.
    pub name: Option<String>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatencyAction {
    /// What is taken off to put a take where it was sung, and where each figure is from.
    #[default]
    Status,
    /// Play a second of quiet noise and listen for it on `channel` of the input: the output has to reach that
    /// input, through a cable or a microphone held to the speaker. The reply is the result, a few seconds
    /// later; it changes nothing by itself.
    Measure,
    /// Take the last measurement's result in place of what the devices say, and remember it for these
    /// devices as they are open.
    Use,
    /// Add `adjust_ms` by hand, on top of what is measured or said: more puts takes earlier.
    Adjust,
    /// Go back to what the devices say: forget the measurement and the adjustment.
    Forget,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecordingLatencyRequest {
    #[serde(default)]
    pub action: LatencyAction,
    /// For `measure`: the channel of the input to listen on, counted from 1 (default 1).
    pub channel: Option<u16>,
    /// For `adjust`: milliseconds, -500 to 500.
    pub adjust_ms: Option<f64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UnusedRecordingsRequest {
    /// Paths to delete, as the list gives them. Deleting cannot be undone. Omit to list only.
    pub delete: Option<Vec<String>>,
}

/// What `input` asks a track to record, or why that is nothing a track records.
fn parse_input(text: &str) -> Result<Option<(u16, bool)>, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") || text.eq_ignore_ascii_case("keyboard") {
        return Ok(None);
    }
    let wrong = || format!("`input` is a channel (\"1\"), two side by side (\"1+2\") or \"none\", not \"{text}\"");
    let parts: Vec<&str> = text.split(['+', '-', ',', '/']).map(str::trim).collect();
    let channel = |part: &str| part.parse::<u16>().ok().filter(|channel| *channel >= 1).ok_or_else(wrong);
    match parts.as_slice() {
        [one] => Ok(Some((channel(one)?, false))),
        [left, right] => {
            let (left, right) = (channel(left)?, channel(right)?);
            match right == left + 1 {
                true => Ok(Some((left, true))),
                false => Err(format!("a pair is two channels side by side, such as \"{left}+{}\"", left + 1)),
            }
        }
        _ => Err(wrong()),
    }
}

fn parse_monitor(text: &str) -> Result<Monitoring, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Ok(Monitoring::Off),
        "armed" | "auto" => Ok(Monitoring::Armed),
        "always" | "on" => Ok(Monitoring::Always),
        other => Err(format!("`monitor` is \"off\", \"armed\" or \"always\", not \"{other}\"")),
    }
}

fn monitor_word(monitor: Monitoring) -> &'static str {
    match monitor {
        Monitoring::Off => "off",
        Monitoring::Armed => "armed",
        Monitoring::Always => "always",
    }
}

fn db(level: f32) -> Value {
    match level > 0.0 {
        true => json!(round(20.0 * f64::from(level).log10())),
        false => json!("-inf"),
    }
}

/// What a track records, in a reply.
pub(crate) fn input_json(input: TrackInput) -> Value {
    json!({ "records": input.said(), "monitor": monitor_word(input.monitor) })
}

/// What a reply says of a track whose input is held: the song says it is
/// heard, and nobody's hand has switched that on.
pub(crate) const HELD: &str = "The song says this input is heard through the track, but it is held and not heard: only \
                               the user opens a microphone into the speakers, with a click on the track's monitor \
                               switch. Nothing is wrong with the song.";

/// Says in `out`, the reply about track `id`, that its input is held, where it is.
pub(crate) fn say_held(session: &Session, id: TrackId, out: &mut Value) {
    if session.input_held(id) {
        out["input_held"] = json!(HELD);
    }
}

/// The input as it stands: open or not, and what is wrong.
fn status_json(session: &Session) -> Value {
    let status = session.input_status();
    let mut out = match &status.open {
        Some(open) => json!({
            "open": true,
            "device": open.device,
            "sample_rate": open.sample_rate,
            "channels": open.channels,
            "exclusive": open.exclusive,
            "clock": if open.shares_clock { "the output's own" } else { "its own: read by its time stamps" },
        }),
        None => json!({ "open": false, "needed": status.needed }),
    };
    if let Some(problem) = status.problem {
        out["problem"] = json!(problem);
    }
    out
}

/// How long sound takes to go round, in a reply.
fn latency_json(session: &Session) -> Value {
    let Some(latency) = session.recording_latency() else {
        return json!({
            "note": "The input is closed, so nothing is known yet: it is open while a track that records one is armed or monitored."
        });
    };
    let mut out = json!({
        "round_trip_ms": round(latency.round_trip_ms),
        "input_says_ms": round(latency.input_ms),
        "handing_over_ms": round(latency.handover_ms),
        "clocks_apart_ppm": round(latency.drift_ppm),
    });
    match latency.measured_ms {
        Some(measured) => {
            out["measured_ms"] = json!(round(measured));
            out["from"] = json!("a measurement, which stands in place of what the output says");
        }
        None => {
            out["output_says_ms"] = json!(round(latency.output_ms));
            out["from"] = json!(
                "what the devices say, which is seldom all there is: `measure` finds the rest with a cable from the output to the input"
            );
        }
    }
    if latency.adjust_ms != 0.0 {
        out["adjusted_by_hand_ms"] = json!(round(latency.adjust_ms));
    }
    if latency.correction == Correction::ByHand {
        out["from"] = json!("what the devices say, adjusted by hand");
    }
    out
}

/// What a recording came to, for `transport`'s reply to `stop`.
pub(crate) fn recorded_json(recorded: &Recorded, rate: f64) -> Value {
    let takes: Vec<Value> = recorded
        .takes
        .iter()
        .map(|take| {
            json!({
                "track_id": take.track,
                "take_id": take.take,
                "from_beat": round(take.start_beat),
                "beats": round(take.length_beats),
                "seconds": round(take.seconds),
                "file": take.file,
                "on_the_track": take.chosen,
            })
        })
        .collect();
    let peaks: Vec<Value> = recorded
        .peaks_db
        .iter()
        .map(|(file, peak)| json!({ "file": file, "peak_db": if *peak <= -119.0 { json!("-inf") } else { json!(round(*peak)) } }))
        .collect();
    let mut out = json!({
        "takes": takes,
        "files": peaks,
        "placed_by_ms": round(recorded.taken_off_ms),
        "placed_by": match recorded.correction {
            Correction::Measured => "a measured round trip",
            Correction::ByHand => "what the devices say, adjusted by hand",
            Correction::None => "what the devices say (not measured: recording_latency)",
        },
    });
    if recorded.silent_frames > 0 {
        out["silent"] = json!(format!(
            "{} frames ({:.0} ms) did not arrive in time and are silence where they would have been",
            recorded.silent_frames,
            recorded.silent_frames as f64 * 1000.0 / rate
        ));
    }
    if let Some(why) = &recorded.ended_early {
        out["ended_early"] = json!(why);
    }
    if recorded.takes.is_empty() {
        out["note"] = json!("The song never rolled, so there is nothing of the recording on any track.");
    }
    out
}

fn takes_of(session: &Session, track: Option<TrackId>) -> Value {
    let project = session.project();
    let heard = session.heard_take();
    let mut tracks = Vec::new();
    for found in project.tracks.iter().filter(|found| track.is_none_or(|id| id == found.id)) {
        if found.takes.is_empty() && track.is_none() {
            continue;
        }
        let takes: Vec<Value> = found
            .takes
            .iter()
            .map(|take| {
                let Some(audio) = take.clip.audio() else { return json!({ "take_id": take.clip.id }) };
                // Whether the track plays any of this take's recording.
                let played = found.clips.iter().any(|clip| clip.audio().is_some_and(|on| on.path == audio.path));
                let mut out = json!({
                    "take_id": take.clip.id,
                    "name": take.clip.name,
                    "lane": take.lane + 1,
                    "from_beat": round(take.clip.start),
                    "to_beat": round(take.clip.end()),
                    "file": audio.path,
                    "begins_in_file_s": round(audio.offset_seconds),
                    "on_the_track": played,
                });
                if heard == Some(take.clip.id) {
                    out["heard_in_place"] = json!(true);
                }
                out
            })
            .collect();
        let mut out = json!({ "track_id": found.id, "name": found.name, "takes": takes });
        if let Some(input) = found.input {
            out["input"] = input_json(input);
        }
        tracks.push(out);
    }
    json!({ "tracks": tracks })
}

/// The level of a take, read from its file: its peak and its level over
/// the whole of it in dB, and how long after its start the sound begins.
fn measure_take(session: &Session, take: ClipId) -> Reply {
    let project = session.project();
    let (_, found) = project.take(take).map_err(|error| explain(project, error))?;
    let audio = found.clip.audio().ok_or("that take holds no audio")?;
    let buffer = session.media().audio(std::path::Path::new(&audio.path)).map_err(|error| error.to_string())?;
    let rate = buffer.sample_rate;
    let seconds = project.tempo_map().seconds_between(found.clip.start, found.clip.end());
    let from = ((audio.offset_seconds * rate) as usize).min(buffer.frames());
    let to = (from + (seconds * rate) as usize).min(buffer.frames());
    let frames = || (from..to).map(|frame| buffer.left[frame].abs().max(buffer.right[frame].abs()));
    let peak = frames().fold(0.0f32, f32::max);
    let power = frames().map(|sample| f64::from(sample).powi(2)).sum::<f64>() / (to - from).max(1) as f64;
    // Where it first comes within 30 dB of its peak.
    let begins = frames().position(|sample| sample > peak * 0.0316 && peak > 0.0);
    Ok(json!({
        "take_id": take,
        "name": found.clip.name,
        "seconds": round((to - from) as f64 / rate),
        "peak_db": db(peak),
        "level_db": db(power.sqrt() as f32),
        "sound_begins_after_s": begins.map(|frame| round(frame as f64 / rate)),
        "clipped": peak >= 1.0,
    }))
}

#[tool_router(router = recording_router, vis = "pub(crate)")]
impl TrillServer {
    /// What a track records and how that is heard meanwhile: an input of the audio device (one channel, or two
    /// side by side), or what a keyboard plays. One undo step; with nothing to change it reads the track's
    /// input, whether the device's input is open, and how long sound takes to go round.
    #[tool(annotations(title = "Track input"))]
    fn track_input(&self, Parameters(request): Parameters<TrackInputRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| -> Reply {
            let id = TrackId(request.track);
            let was = session.project().track(id).map_err(|error| explain(session.project(), error))?.input;
            let mut wanted = was;
            if let Some(text) = &request.input {
                let monitor = was.map_or(Monitoring::Off, |input| input.monitor);
                wanted = parse_input(text)?.map(|(channel, stereo)| TrackInput { channel, stereo, monitor });
            }
            if let Some(text) = &request.monitor {
                let monitor = parse_monitor(text)?;
                match &mut wanted {
                    Some(input) => input.monitor = monitor,
                    None => {
                        return Err(
                            "a track that records a keyboard has no input to monitor: give it `input` first".into()
                        );
                    }
                }
            }
            // A client the user set up, asking for an input to be heard
            // that the song already says is, and that is held: that is
            // the hand that lets it go. (At the assistant's door it stays
            // held, and the reply says so.)
            if request.monitor.is_some() && wanted == was && !self.is_guarded() && session.input_held(id) {
                session.let_input_be_heard(id).map_err(|error| explain(session.project(), error))?;
            }
            if wanted != was {
                // A microphone goes live by the user's own hand.
                if self.is_guarded() && wanted.is_some() && session.is_armed(id) {
                    return Err(
                        "This track is armed, and giving it an input would open the microphone. The user gives an \
                                armed track its input themselves, in the track's header; or disarm it first."
                            .into(),
                    );
                }
                session
                    .edit(|project| project.set_track_input(id, wanted))
                    .map_err(|error| explain(session.project(), error))?;
            }
            let mut out = json!({ "track_id": id });
            match wanted {
                Some(input) => out["input"] = input_json(input),
                None => out["records"] = json!("what a keyboard plays"),
            }
            out["armed"] = json!(session.is_armed(id));
            say_held(&session, id, &mut out);
            out["device_input"] = status_json(&session);
            if let Some(latency) = session.recording_latency() {
                out["round_trip_ms"] = json!(round(latency.round_trip_ms));
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// The input of the audio device: which device it is, whether it is open, and what is arriving on each of
    /// its channels now (the highest level since the last look, in dB of full scale, and whether it clipped).
    /// The device is chosen with `audio`. Look here before a take: you cannot hear the room.
    #[tool(annotations(title = "Audio inputs", read_only_hint = true))]
    async fn audio_inputs(&self, Parameters(request): Parameters<AudioInputsRequest>) -> CallToolResult {
        let (session, guarded) = (self.session.clone(), self.is_guarded());
        let outcome = tokio::task::spawn_blocking(move || -> Reply {
            // A client the user set up is the user's hand, on this thread too.
            let _hand = (!guarded).then(trill_engine::hand::by_hand);
            let listen = request.listen.unwrap_or(false);
            let was_open = session.lock().input_status().open.is_some();
            if listen && !was_open {
                if guarded {
                    return Err("Opening the input to look at its level opens the microphone, which the user does \
                                themselves: when they arm a track that records it, the levels are here."
                        .into());
                }
                let mut session = session.lock();
                session.watch_input(true).map_err(|error| error.to_string())?;
                if let Err(error) = session.ready_input() {
                    let _ = session.watch_input(false);
                    return Err(error.to_string());
                }
                session.input_levels();
            }
            if listen {
                // Half a second of whatever is arriving.
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            let mut session = session.lock();
            let mut out = status_json(&session);
            let levels = session.input_levels();
            if !levels.is_empty() {
                let channels: Vec<Value> = levels
                    .iter()
                    .enumerate()
                    .map(|(index, (peak, clipped))| json!({ "channel": index + 1, "peak_db": db(*peak), "clipped": clipped }))
                    .collect();
                out["arriving"] = json!(channels);
            }
            if listen && !was_open {
                let _ = session.watch_input(false);
            }
            out["asked_for"] = json!(session.audio_config().input.clone().unwrap_or_else(|| "the system's default input".into()));
            Ok(out)
        })
        .await;
        finish(outcome.unwrap_or_else(|error| Err(format!("the input did not answer: {error}"))))
    }

    /// The takes kept beside a track, and the comp made of them: list them, put one or a stretch of one on the
    /// track, hear one in place, rename, remove or measure one. Every recording is a take in a lane, never
    /// played from there; the track plays ordinary audio clips, and nothing a take covers is lost.
    #[tool(annotations(title = "Takes"))]
    fn takes(&self, Parameters(request): Parameters<TakesRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let reply = (|| -> Reply {
            let take = || {
                request.take.map(ClipId).ok_or_else(|| "this needs `take`: a take id, as `list` gives them".to_string())
            };
            match request.action {
                TakesAction::List => {
                    if let Some(track) = request.track {
                        session.project().track(TrackId(track)).map_err(|error| explain(session.project(), error))?;
                    }
                    Ok(takes_of(&session, request.track.map(TrackId)))
                }
                TakesAction::Use => {
                    let id = take()?;
                    let clip = match (request.from_beat, request.to_beat) {
                        (None, None) => session.edit(|project| project.choose_take(id)),
                        (Some(from), Some(to)) => session.edit(|project| project.comp_take(id, from, to)),
                        _ => {
                            return Err(
                                "a stretch needs both `from_beat` and `to_beat`; omit both for the whole take".into()
                            );
                        }
                    };
                    let clip = clip.map_err(|error| explain(session.project(), error))?;
                    let project = session.project();
                    let track = project.clip_track(clip).map_err(|error| explain(project, error))?;
                    let plays: Vec<Value> = project
                        .track(track)
                        .map(|track| {
                            let audio = track.clips.iter().filter(|clip| clip.audio().is_some());
                            audio.map(|clip| json!({ "clip_id": clip.id, "name": clip.name, "from_beat": round(clip.start), "to_beat": round(clip.end()) })).collect()
                        })
                        .unwrap_or_default();
                    Ok(json!({ "clip_id": clip, "track_id": track, "the_track_plays": plays }))
                }
                TakesAction::Hear => {
                    let wanted = request.take.map(ClipId);
                    session.hear_take(wanted).map_err(|error| explain(session.project(), error))?;
                    Ok(match wanted {
                        Some(id) => {
                            json!({ "heard_in_place": id, "note": "The song is unchanged: `use` puts the take on the track, and `hear` without a take goes back to the song." })
                        }
                        None => json!({ "heard_in_place": Value::Null }),
                    })
                }
                TakesAction::Rename => {
                    let (id, name) = (take()?, request.name.clone().ok_or("`rename` needs `name`")?);
                    session
                        .edit(|project| project.rename_take(id, name.clone()))
                        .map_err(|error| explain(session.project(), error))?;
                    Ok(json!({ "take_id": id, "name": name }))
                }
                TakesAction::Remove => {
                    let id = take()?;
                    let removed = session
                        .edit(|project| project.remove_take(id))
                        .map_err(|error| explain(session.project(), error))?;
                    let file = removed.clip.audio().map(|audio| audio.path.clone());
                    Ok(json!({ "removed": id, "name": removed.clip.name, "file_kept": file }))
                }
                TakesAction::Measure => measure_take(&session, take()?),
            }
        })();
        finish(reply)
    }

    /// How long sound takes to go from the microphone to the speaker, which is what a take is moved back by to
    /// lie where it was sung, and what a monitored input is late by: where each figure is from, the
    /// measurement that finds what the devices do not say, and a correction by hand.
    #[tool(annotations(title = "Recording latency"))]
    async fn recording_latency(&self, Parameters(request): Parameters<RecordingLatencyRequest>) -> CallToolResult {
        let (session, runtime, guarded) = (self.session.clone(), self.runtime.clone(), self.is_guarded());
        let outcome = tokio::task::spawn_blocking(move || -> Reply {
            // A client the user set up is the user's hand, on this thread too.
            let _hand = (!guarded).then(trill_engine::hand::by_hand);
            let saved = |kept: Result<(), trill_library::SaveError>, out: &mut Value| {
                if let Err(error) = kept {
                    out["not_saved"] = json!(format!("{error}. It holds for this run only."));
                }
            };
            match request.action {
                LatencyAction::Status => Ok(latency_json(&session.lock())),
                LatencyAction::Measure => {
                    if guarded {
                        return Err("The measurement opens the input and plays noise through the output: the user runs it, \
                                    in the audio settings."
                            .into());
                    }
                    session.lock().measure_latency(request.channel.unwrap_or(1)).map_err(|error| error.to_string())?;
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let mut session = session.lock();
                        session.housekeeping();
                        match session.measurement() {
                            Measurement::Running if std::time::Instant::now() < deadline => continue,
                            Measurement::Running => return Err("the measurement did not end".into()),
                            Measurement::Failed(why) => return Err(why),
                            Measurement::Idle => return Err("the measurement was lost".into()),
                            Measurement::Done(measured) => {
                                return Ok(json!({
                                    "round_trip_ms": round(measured.round_trip_ms),
                                    "more_than_the_devices_say_ms": round(measured.more_than_said_ms),
                                    "likeness": round(measured.likeness),
                                    "came_back_at_db": round(measured.heard_db),
                                    "note": "Nothing has changed yet: `use` takes it. A likeness under 0.6 is a poor reading: measure again, through a cable.",
                                }));
                            }
                        }
                    }
                }
                LatencyAction::Use => {
                    let Measurement::Done(measured) = session.lock().measurement() else {
                        return Err("there is no measurement to take: `measure` first".into());
                    };
                    let kept = runtime.keep_measured_latency(Some(measured.beyond_ms));
                    let mut out = latency_json(&session.lock());
                    saved(kept, &mut out);
                    Ok(out)
                }
                LatencyAction::Adjust => {
                    let adjust = request.adjust_ms.ok_or("`adjust` needs `adjust_ms`")?;
                    if !adjust.is_finite() || adjust.abs() > 500.0 {
                        return Err("`adjust_ms` is between -500 and 500".into());
                    }
                    let kept = runtime.keep_latency_adjustment(adjust);
                    let mut out = latency_json(&session.lock());
                    saved(kept, &mut out);
                    Ok(out)
                }
                LatencyAction::Forget => {
                    let kept = runtime.keep_measured_latency(None).and_then(|()| runtime.keep_latency_adjustment(0.0));
                    let mut out = latency_json(&session.lock());
                    saved(kept, &mut out);
                    Ok(out)
                }
            }
        })
        .await;
        finish(outcome.unwrap_or_else(|error| Err(format!("the measurement did not answer: {error}"))))
    }

    /// The recordings in the song's own folder that nothing plays any more, with their sizes: not the song or
    /// any state undo can reach, not another song Trill knows of (one saved under another name plays the same
    /// files), not unsaved work that can still be recovered. With `delete`, their removal: to the system's
    /// Recycle Bin, or a `removed` folder beside the takes; never out of existence. One that any song plays is
    /// refused with that song's name. Trill never removes a recording by itself: a take that is removed or
    /// undone leaves its file.
    #[tool(annotations(title = "Unused recordings", destructive_hint = true))]
    async fn unused_recordings(&self, Parameters(request): Parameters<UnusedRecordingsRequest>) -> CallToolResult {
        let (runtime, guarded) = (self.runtime.clone(), self.is_guarded());
        // The other songs' files are read to answer: not with the server's own thread.
        let outcome = tokio::task::spawn_blocking(move || -> Reply {
            let mut out = json!({});
            if let Some(paths) = &request.delete {
                if guarded {
                    return Err("Removing a recording is the user's to do, who is asked first: File, Unused \
                                recordings lists them and says where they would go."
                        .into());
                }
                let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
                let removed = runtime.delete_unused_recordings(&paths)?;
                out["removed"] = json!({
                    "count": paths.len(),
                    "bytes": removed.bytes,
                    "went": removed.went(),
                    "in_the_recycle_bin": removed.recycled,
                    "set_aside": removed.set_aside,
                });
            }
            let listed = runtime.unused_recordings();
            out["folder"] = json!(listed.folder);
            out["bytes"] = json!(listed.unused.iter().map(|(_, bytes)| bytes).sum::<u64>());
            let unused: Vec<Value> =
                listed.unused.iter().map(|(path, bytes)| json!({ "path": path, "bytes": bytes })).collect();
            out["unused"] = json!(unused);
            if !listed.played_elsewhere.is_empty() {
                // Not this song's any more, and somebody else's still: they stay.
                let kept: Vec<Value> = listed
                    .played_elsewhere
                    .iter()
                    .map(|kept| json!({ "path": kept.path, "bytes": kept.bytes, "played_by": kept.by }))
                    .collect();
                out["played_by_other_songs"] = json!(kept);
            }
            out["removed_go"] = json!(listed.goes);
            Ok(out)
        })
        .await;
        finish(outcome.unwrap_or_else(|error| Err(format!("the recordings could not be listed: {error}"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_is_a_channel_a_pair_side_by_side_or_none() {
        assert_eq!(parse_input("1"), Ok(Some((1, false))));
        assert_eq!(parse_input(" 3+4 "), Ok(Some((3, true))));
        assert_eq!(parse_input("5-6"), Ok(Some((5, true))));
        assert_eq!(parse_input("none"), Ok(None));
        assert!(parse_input("1+3").unwrap_err().contains("\"1+2\""));
        assert!(parse_input("0").is_err() && parse_input("left").is_err() && parse_input("1+2+3").is_err());
        assert_eq!(parse_monitor("Armed"), Ok(Monitoring::Armed));
        assert!(parse_monitor("loud").unwrap_err().contains("\"always\""));
        assert_eq!(db(1.0), json!(0.0));
        assert_eq!(db(0.0), json!("-inf"));
    }
}
