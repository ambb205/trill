//! `playback_load`: how the machine keeps up with the song as it plays.
//!
//! A song that plays live has a speaker to keep up with: every buffer the
//! audio device asks for has to be made in the time it lasts, and one
//! that is not is a gap in what is heard. The tool says how much of that
//! time is in use, how many buffers came late, on how many threads the
//! song is made and what each track costs, over the last ten seconds
//! ([`Session::keeping_up`](trill_engine::Session::keeping_up)); and,
//! when the song is more than the machine plays in time, the sentence
//! the window shows ([`trill_engine::Shortfall::sentence`]). `transport`
//! carries that sentence too, so that whoever presses play hears of it.

use crate::server::{TrillServer, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use trill_engine::{KeepingUp, Session};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Whether the machine keeps up with the song
- A song that plays live has to be made in real time, and a large one may be more than the machine manages: each \
buffer that comes late is a gap in what is heard. transport says so when it happens (`falling_short`), and \
playback_load says how much of the audio thread's time is in use and what each track costs. It is a matter of \
live playback only: measure, render and probe take the time they need and are never affected, so do not judge a \
mix by how it played, and do not take a song apart because it stutters live. What helps is switching off or \
removing devices on the tracks that cost most; muting a track does not, because its devices go on running.";

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlaybackLoadRequest {
    /// The most tracks to list, the ones that cost most first (default 12). The rest are counted in
    /// `other_tracks`.
    pub limit: Option<usize>,
}

/// How many tracks a reply lists when the caller does not say, and the
/// most it can ask for.
const DEFAULT_LISTED: usize = 12;
const MAX_LISTED: usize = 200;

/// A share as a percentage with one decimal.
fn percent(share: f64) -> f64 {
    (share * 1_000.0).round() / 10.0
}

/// What `transport` adds to its reply while the song is more than the
/// machine plays in time: the sentence, and where to ask for more.
pub(crate) fn falling_short_note(session: &Session) -> Option<String> {
    let short = session.falling_short()?;
    Some(format!("{} playback_load has every track's cost.", short.sentence()))
}

/// The reply of `playback_load`: what `keeping` says, compactly.
pub(crate) fn load_json(keeping: &KeepingUp, buffer_frames: u32, limit: usize) -> Value {
    if keeping.load.callbacks == 0 {
        return json!({
            "note": "No buffer has been asked for in the last ten seconds: there is no audio device (a server \
                     started without one makes no sound), or the stream has just been opened. measure and render \
                     do not need one."
        });
    }
    let load = &keeping.load;
    let rate = keeping.sample_rate;
    let mut out = json!({
        "over_seconds": round(keeping.seconds),
        "load_percent": percent(f64::from(load.mean)),
        "worst_buffer_percent": percent(f64::from(load.worst)),
        "late_buffers": load.late,
        "buffers": load.callbacks,
        "threads": load.threads,
        "threads_it_may_have": load.allowed,
        "buffer_frames": buffer_frames,
        "sample_rate": rate,
    });
    let mut tracks: Vec<_> = keeping.tracks.iter().map(|track| (track, track.cost.share(rate))).collect();
    tracks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let all: f64 = tracks.iter().map(|(_, share)| share).sum();
    let listed: Vec<Value> = (tracks.iter().take(limit))
        .map(|(track, share)| {
            json!({
                "track_id": track.track,
                "name": track.name,
                "percent_of_a_processor": percent(*share),
                "longest_block_ms": round(track.cost.longest as f64 / 1e6),
            })
        })
        .collect();
    out["tracks"] = json!(listed);
    if tracks.len() > limit {
        let rest: f64 = tracks.iter().skip(limit).map(|(_, share)| share).sum();
        out["other_tracks"] = json!({ "count": tracks.len() - limit, "percent_of_a_processor": percent(rest) });
    }
    out["all_tracks_percent_of_a_processor"] = json!(percent(all));
    out["master_percent_of_a_processor"] = json!(percent(keeping.master.share(rate)));
    if let Some(short) = &keeping.short {
        out["falling_short"] = json!(short.sentence());
    }
    out
}

#[tool_router(router = playing_router, vis = "pub(crate)")]
impl TrillServer {
    /// How the machine keeps up with the song as it plays live, over the last ten seconds: `load_percent` is
    /// the share of each buffer's time that making it took on the audio device's thread (100 means none to
    /// spare) and `worst_buffer_percent` the same for the worst one; `late_buffers` took longer than they last,
    /// and each is a gap in what is heard; `threads` is how many processors the song is made on (a large song
    /// is spread over several by itself). `tracks` has what each track costs, the costliest first, as a
    /// percentage of one processor's time, with its devices: a track's cost is there whether or not it is
    /// muted or anything is playing on it, and goes when its devices are switched off or removed.
    /// `falling_short` is set while the song is more than the machine plays in time, and says what helps.
    /// This is about live playback only: measure, render and probe are never late. Reads only.
    #[tool(annotations(title = "Playback load", read_only_hint = true))]
    fn playback_load(&self, Parameters(request): Parameters<PlaybackLoadRequest>) -> CallToolResult {
        let mut session = self.session.lock();
        let keeping = session.keeping_up();
        let limit = request.limit.unwrap_or(DEFAULT_LISTED).clamp(1, MAX_LISTED);
        finish(Ok(load_json(&keeping, session.audio_info().buffer_frames, limit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::TrackId;
    use trill_engine::{Cost, Load, Shortfall, TrackCost};

    fn track(id: u64, name: &str, nanos: u64) -> TrackCost {
        // A second of sound at 48 kHz in a hundred blocks.
        let cost = Cost { blocks: 100, frames: 48_000, nanos, longest: nanos / 50, longest_ran: 0 };
        TrackCost { track: TrackId(id), name: name.to_string(), cost }
    }

    #[test]
    fn the_reply_lists_the_tracks_that_cost_most_first_and_counts_the_rest() {
        let load =
            Load { callbacks: 100, late: 3, mean: 0.366, worst: 1.43, threads: 8, allowed: 8, ..Load::default() };
        let short = Shortfall { episode: 1, late: 3, buffers: 100, costliest: Vec::new(), threads: 8, allowed: 8 };
        let keeping = KeepingUp {
            seconds: 1.0,
            load,
            tracks: vec![track(1, "Drums", 77_000_000), track(2, "Pad", 259_000_000), track(3, "Bass", 24_000_000)],
            master: Cost { blocks: 100, frames: 48_000, nanos: 2_000_000, longest: 130_000, longest_ran: 0 },
            sample_rate: 48_000.0,
            short: Some(short.clone()),
        };
        let reply = load_json(&keeping, 480, 2);
        assert_eq!(reply["load_percent"], 36.6);
        assert_eq!(reply["worst_buffer_percent"], 143.0);
        assert_eq!((reply["late_buffers"].as_u64(), reply["buffers"].as_u64()), (Some(3), Some(100)));
        assert_eq!((reply["threads"].as_u64(), reply["buffer_frames"].as_u64()), (Some(8), Some(480)));
        let names: Vec<&str> =
            reply["tracks"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Pad", "Drums"]);
        assert_eq!(reply["tracks"][0]["percent_of_a_processor"], 25.9);
        assert_eq!(reply["tracks"][0]["longest_block_ms"], 5.18);
        assert_eq!(reply["other_tracks"], json!({ "count": 1, "percent_of_a_processor": 2.4 }));
        assert_eq!(reply["all_tracks_percent_of_a_processor"], 36.0);
        assert_eq!(reply["master_percent_of_a_processor"], 0.2);
        assert_eq!(reply["falling_short"], short.sentence());

        // With no device asking for anything there is nothing to say but that.
        let silent = load_json(&KeepingUp::default(), 0, 12);
        assert!(silent["note"].as_str().unwrap().contains("No buffer has been asked for"), "{silent}");
        assert!(silent.get("tracks").is_none());
    }
}
